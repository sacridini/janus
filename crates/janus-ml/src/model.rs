//! A trained model: its network (or forest) and what applying it needs (the architecture and its hyperparameters, the values
//! read on each date, the dates, the scaling, the classes), kept in a `.janusmodel` file: a header of JSON and the
//! weights as SafeTensors (a forest's trees in its own format, forest.rs).

use std::sync::Arc;

use burn::module::Module;
use burn::prelude::*;
use burn::store::{ModuleSnapshot, SafetensorsStore};
use burn::tensor::activation::softmax;
use janus_cube::CubeInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::data::{Calendar, InputBand, Norm};
use crate::forest::{Forest, ForestHyper};
use crate::ltae::{LTae, LTaeHyper};
use crate::presto::{PrestoHyper, PrestoNet};
use crate::tempcnn::{TempCnn, TempCnnHyper};
use crate::utae::{Utae, UtaeHyper};

const MAGIC: &[u8] = b"JANUSMODEL\x01";

/// The architecture and its hyperparameters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "arch", rename_all = "lowercase")]
pub enum Arch {
    TempCnn(TempCnnHyper),
    LTae(LTaeHyper),
    Presto(PrestoHyper),
    Utae(UtaeHyper),
    RandomForest(ForestHyper),
}

impl Arch {
    pub fn name(&self) -> &'static str {
        match self {
            Arch::TempCnn(_) => "TempCNN",
            Arch::LTae(_) => "L-TAE",
            Arch::Presto(_) => "Presto",
            Arch::Utae(_) => "U-TAE",
            Arch::RandomForest(_) => "Random Forest",
        }
    }
}

/// A class a model gives: its name and colour.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Class {
    pub name: String,
    pub color: [u8; 3],
}

/// What a model file says besides its weights.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Header {
    pub format: u32,
    /// the Janus that trained it
    pub janus: String,
    pub arch: Arch,
    pub inputs: Vec<InputBand>,
    pub calendar: Calendar,
    pub norm: Norm,
    /// U-TAE: each value's mean and deviation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standard: Option<crate::dense::Standard>,
    pub classes: Vec<Class>,
    /// a pixel needs values on this share of the dates (%), its gaps filled in time
    pub min_valid: f64,
    /// the series it was trained on
    pub series: String,
    /// the samples, the training's settings, history and validation
    #[serde(default)]
    pub training: Value,
    /// the samples as the network read them, for its area of applicability (none in older models, nor Presto's
    /// and U-TAE's)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicability: Option<crate::aoa::Applicability>,
    pub created: String,
}

/// One network a model (never many side by side): its size is no matter. A forest is no network (no weights to
/// learn by gradients), held here as models hold their network.
#[allow(clippy::large_enum_variant, deprecated)]
#[derive(Module, Debug)]
pub enum Net {
    TempCnn(TempCnn),
    LTae(LTae),
    Presto(PrestoNet),
    Utae(Utae),
    Forest(burn::module::Ignored<Arc<Forest>>),
}

impl Net {
    pub fn new(arch: &Arch, bands: usize, calendar: &Calendar, classes: usize, device: &Device) -> Result<Net, String> {
        Ok(match arch {
            Arch::TempCnn(h) => Net::TempCnn(TempCnn::new(h, bands, calendar.len(), classes, device)),
            Arch::LTae(h) => Net::LTae(LTae::new(h, bands, &calendar.day_offsets(), classes, device)),
            Arch::Presto(h) => Net::Presto(PrestoNet::new(h, classes, device)?),
            Arch::Utae(h) => Net::Utae(Utae::new(h, bands, classes, device)),
            Arch::RandomForest(_) => return Err("a forest is grown (forest::train), not made".into()),
        })
    }

    #[allow(deprecated)]
    pub fn forest(forest: Forest) -> Net {
        Net::Forest(burn::module::Ignored(Arc::new(forest)))
    }

    /// [n, dates, values] -> the classes' scores [n, classes] (a pixel's networks: U-TAE classifies patches,
    /// dense::classify_windows).
    pub fn forward(&self, x: Tensor<3>) -> Tensor<2> {
        match self {
            Net::TempCnn(m) => m.forward(x),
            Net::LTae(m) => m.forward(x),
            Net::Presto(m) => m.forward(x),
            Net::Utae(_) => panic!("U-TAE classifies patches (dense::classify_windows), not pixels"),
            Net::Forest(_) => panic!("a forest is no network: Model::predict"),
        }
    }
}

pub struct Model {
    pub header: Header,
    pub net: Net,
    pub device: Device,
}

impl Model {
    /// The model's forest, when it is one.
    pub fn forest(&self) -> Option<&Forest> {
        match &self.net {
            Net::Forest(f) => Some(&***f),
            _ => None,
        }
    }

    pub fn save(&self, path: &str) -> Result<(), String> {
        let weights = if let Some(f) = self.forest() {
            f.to_bytes()
        } else {
            let mut store = SafetensorsStore::from_bytes(None);
            self.net.clone().valid().save_into(&mut store).map_err(|e| format!("cannot keep the weights: {e}"))?;
            store.get_bytes().map_err(|e| format!("cannot keep the weights: {e}"))?
        };
        let header = serde_json::to_vec(&self.header).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(MAGIC.len() + 8 + header.len() + weights.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(header.len() as u64).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&weights);
        let tmp = format!("{path}.part");
        std::fs::write(&tmp, out).map_err(|e| format!("{tmp}: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{path}: {e}"))
    }

    /// A model file's header only.
    pub fn read_header(path: &str) -> Result<Header, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        Ok(split(&bytes, path)?.0)
    }

    pub fn load(path: &str, device: &Device) -> Result<Model, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let (header, weights) = split(&bytes, path)?;
        if let Arch::RandomForest(_) = &header.arch {
            let f = Forest::from_bytes(weights).map_err(|e| format!("{path}: {e}"))?;
            if f.classes != header.classes.len() || f.features != header.calendar.len() * header.inputs.len() {
                return Err(format!("{path}: its forest does not fit its classes and values"));
            }
            return Ok(Model { header, net: Net::forest(f), device: device.clone() });
        }
        let mut net = Net::new(&header.arch, header.inputs.len(), &header.calendar, header.classes.len(), device)?;
        let mut store = SafetensorsStore::from_bytes(Some(weights.to_vec()));
        net.load_from(&mut store).map_err(|e| format!("{path}: its weights do not fit its network: {e}"))?;
        Ok(Model { header, net: net.valid(), device: device.clone() })
    }

    /// The steps and values a pixel's input has (the dates and values; Presto's months and channels).
    pub fn input_shape(&self) -> (usize, usize) {
        match &self.header.arch {
            Arch::Presto(h) => {
                (if h.inputs.year.is_some() { 12 } else { self.header.calendar.len() }, crate::presto::CHANNELS)
            }
            _ => (self.header.calendar.len(), self.header.inputs.len()),
        }
    }

    /// The classes' probabilities [n][classes] of n pixels' inputs (n x steps x values), in batches.
    pub fn predict(&self, x: &[f32], n: usize, batch: usize) -> Vec<f32> {
        let ((t, b), k) = (self.input_shape(), self.header.classes.len());
        if let Some(f) = self.forest() {
            return f.predict(&x[..n * t * b], n);
        }
        let mut out = Vec::with_capacity(n * k);
        let net = self.net.valid();
        for s in (0..n).step_by(batch.max(1)) {
            let m = batch.min(n - s);
            let input = Tensor::<3>::from_data(
                TensorData::new(x[s * t * b..(s + m) * t * b].to_vec(), [m, t, b]),
                &self.device,
            );
            let p = softmax(net.forward(input), 1);
            out.extend(p.into_data().try_to_vec::<f32>().unwrap_or_else(|_| vec![f32::NAN; m * k]));
        }
        out
    }

    /// The model's inputs in another series (by name), or why it cannot be applied there.
    pub fn inputs_in(&self, info: &CubeInfo) -> Result<Vec<InputBand>, String> {
        if let Arch::Presto(h) = &self.header.arch {
            // any series with the bands, and dates in the year of the composites
            h.inputs.steps(info)?;
            return self.header.inputs.iter().map(|b| b.find_in(info)).collect();
        }
        if matches!(self.header.arch, Arch::Utae(_)) {
            // any dates: their positions are the series' own
            return self.header.inputs.iter().map(|b| b.find_in(info)).collect();
        }
        if info.t() != self.header.calendar.len() {
            return Err(format!(
                "the model was trained on a series of {} dates; this one has {}",
                self.header.calendar.len(),
                info.t()
            ));
        }
        self.header.inputs.iter().map(|b| b.find_in(info)).collect()
    }
}

fn split<'a>(bytes: &'a [u8], path: &str) -> Result<(Header, &'a [u8]), String> {
    let bad = || format!("{path} is not a Janus model");
    if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(bad());
    }
    let len = u64::from_le_bytes(bytes[MAGIC.len()..MAGIC.len() + 8].try_into().map_err(|_| bad())?) as usize;
    let start = MAGIC.len() + 8;
    let json = bytes.get(start..start + len).ok_or_else(bad)?;
    let header: Header = serde_json::from_slice(json).map_err(|e| format!("{path}: {e}"))?;
    Ok((header, &bytes[start + len..]))
}

/// The device models run on: the GPU through wgpu when there is one (and `gpu`), else the CPU; and its name.
pub fn device(gpu: bool) -> (Device, String) {
    if gpu {
        let probe = std::panic::catch_unwind(|| {
            let d = Device::wgpu(Default::default());
            // the first use makes the device: a machine without a usable GPU fails here
            let _ = Tensor::<1>::zeros([1], &d).into_data();
            d
        });
        if let Ok(d) = probe {
            return (d, "GPU (wgpu)".into());
        }
    }
    (Device::flex(), "CPU".into())
}

/// A shared handle on the device, made once.
pub fn shared_device(gpu: bool) -> Arc<(Device, String)> {
    static GPU: std::sync::OnceLock<Arc<(Device, String)>> = std::sync::OnceLock::new();
    static CPU: std::sync::OnceLock<Arc<(Device, String)>> = std::sync::OnceLock::new();
    if gpu {
        GPU.get_or_init(|| Arc::new(device(true))).clone()
    } else {
        CPU.get_or_init(|| Arc::new(device(false))).clone()
    }
}
