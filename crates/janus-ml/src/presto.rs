//! Presto (Tseng et al. 2023, nasaharvest/presto, MIT): a small transformer pretrained on the monthly series of
//! single pixels (Sentinel-1, Sentinel-2, ERA5, SRTM, Dynamic World), any of which may be missing. Its encoder as
//! zeit.ai.presto ports it (the authors' weights give the authors' encodings): a token per group of bands and date
//! (its month, its group and its date embedded with it), one for SRTM and one for the pixel's latitude and
//! longitude, two transformer blocks over the tokens observed, and the mean of those tokens.
//!
//! On a series of Janus: its bands matched to Presto's inputs by name and role, its units brought to Presto's
//! (reflectance 0-10000, dB), monthly composites of a year (each month the median of its observations), the
//! latitude and longitude of each pixel. Two uses: the encodings (128 values a pixel and year) as an embedding
//! layer, like TESSERA's and AlphaEarth's; and a classifier trained on them (the encoder kept as pretrained).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use burn::module::Param;
use burn::nn::{Embedding, EmbeddingConfig, LayerNorm, LayerNormConfig, Linear, LinearConfig};
use burn::prelude::*;
use burn::store::{ModuleSnapshot, PytorchStore};
use burn::tensor::activation::{gelu, softmax};
use janus_cube::CubeInfo;
use serde::{Deserialize, Serialize};

use crate::data::{InputBand, Series};

/// Presto's inputs in its order, and the shift and divisor of its normalization.
pub const INPUTS: [&str; 17] = [
    "VV",
    "VH",
    "B2",
    "B3",
    "B4",
    "B5",
    "B6",
    "B7",
    "B8",
    "B8A",
    "B11",
    "B12",
    "temperature_2m",
    "total_precipitation",
    "elevation",
    "slope",
    "NDVI",
];
const SHIFT: [f32; 17] = [25.0, 25.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -272.15, 0.0, 0.0, 0.0, 0.0];
const DIV: [f32; 17] = [25.0, 25.0, 1e4, 1e4, 1e4, 1e4, 1e4, 1e4, 1e4, 1e4, 1e4, 1e4, 35.0, 0.03, 2000.0, 50.0, 1.0];
/// The groups of bands (a token each a date): their inputs' range.
const GROUPS: [(&str, std::ops::Range<usize>); 9] = [
    ("s1", 0..2),
    ("s2_rgb", 2..5),
    ("s2_red_edge", 5..8),
    ("s2_nir_10m", 8..9),
    ("s2_nir_20m", 9..10),
    ("s2_swir", 10..12),
    ("era5", 12..14),
    ("srtm", 14..16),
    ("ndvi", 16..17),
];
/// The weights' names of the groups (nn.ModuleDict keys)
const GROUP_KEYS: [&str; 9] =
    ["S1", "S2_RGB", "S2_Red_Edge", "S2_NIR_10m", "S2_NIR_20m", "S2_SWIR", "ERA5", "SRTM", "NDVI"];
pub const MAX_DATES: usize = 24;
pub const DIMS: usize = 128;

/// The authors' weights, at a fixed revision (zeit.ai.presto's).
pub const WEIGHTS_URL: &str = "https://raw.githubusercontent.com/nasaharvest/presto/11e207a668a34336ced1d8e492a1bd5849b96c4a/data/default_model.pt";
pub const WEIGHTS_SHA256: &str = "559c9d76f525bdf2dd3774da8b1e7f1510769b73baacac12e9e4d8d5c67fbe65";
pub const VERSION: &str = "nasaharvest/presto@11e207a";

/// Band names (any case) that hold each input (zeit's ALIASES), besides Janus's band roles.
fn aliases(input: &str) -> &'static [&'static str] {
    match input {
        "VV" => &["vv"],
        "VH" => &["vh"],
        "B2" => &["blue", "b02", "b2"],
        "B3" => &["green", "b03", "b3"],
        "B4" => &["red", "b04", "b4"],
        "B5" => &["rededge1", "b05", "b5"],
        "B6" => &["rededge2", "b06", "b6"],
        "B7" => &["rededge3", "b07", "b7"],
        "B8" => &["nir", "b08", "b8"],
        "B8A" => &["nir08", "b8a"],
        "B11" => &["swir1", "swir16", "b11"],
        "B12" => &["swir2", "swir22", "b12"],
        "temperature_2m" => &["temperature_2m", "t2m"],
        "total_precipitation" => &["total_precipitation", "tp"],
        "elevation" => &["elevation", "dem"],
        "slope" => &["slope"],
        _ => &[],
    }
}

#[derive(Module, Debug)]
pub struct Attention {
    qkv: Linear,
    proj: Linear,
}

#[derive(Module, Debug)]
pub struct Mlp {
    fc1: Linear,
    fc2: Linear,
}

#[derive(Module, Debug)]
pub struct Block {
    norm1: LayerNorm,
    attn: Attention,
    norm2: LayerNorm,
    mlp: Mlp,
}

#[derive(Module, Debug)]
pub struct EoPatchEmbed {
    s1: Linear,
    s2_rgb: Linear,
    s2_red_edge: Linear,
    s2_nir_10m: Linear,
    s2_nir_20m: Linear,
    s2_swir: Linear,
    era5: Linear,
    srtm: Linear,
    ndvi: Linear,
}

impl EoPatchEmbed {
    fn group(&self, k: usize) -> &Linear {
        [
            &self.s1,
            &self.s2_rgb,
            &self.s2_red_edge,
            &self.s2_nir_10m,
            &self.s2_nir_20m,
            &self.s2_swir,
            &self.era5,
            &self.srtm,
            &self.ndvi,
        ][k]
    }
}

#[derive(Module, Debug)]
pub struct Encoder {
    eo_patch_embed: EoPatchEmbed,
    dw_embed: Embedding,
    latlon_embed: Linear,
    blocks: Vec<Block>,
    norm: LayerNorm,
    pos_embed: Param<Tensor<3>>,
    month_embed: Embedding,
    channel_embed: Embedding,
}

const HEADS: usize = 8;

impl Encoder {
    pub fn new(device: &Device) -> Encoder {
        let d = DIMS;
        let lin = |i: usize| LinearConfig::new(i, d).init(device);
        let block = || Block {
            norm1: LayerNormConfig::new(d).init(device),
            attn: Attention {
                qkv: LinearConfig::new(d, 3 * d).init(device),
                proj: LinearConfig::new(d, d).init(device),
            },
            norm2: LayerNormConfig::new(d).init(device),
            mlp: Mlp { fc1: LinearConfig::new(d, 4 * d).init(device), fc2: LinearConfig::new(4 * d, d).init(device) },
        };
        Encoder {
            eo_patch_embed: EoPatchEmbed {
                s1: lin(2),
                s2_rgb: lin(3),
                s2_red_edge: lin(3),
                s2_nir_10m: lin(1),
                s2_nir_20m: lin(1),
                s2_swir: lin(2),
                era5: lin(2),
                srtm: lin(2),
                ndvi: lin(1),
            },
            dw_embed: EmbeddingConfig::new(10, d).init(device),
            latlon_embed: lin(3),
            blocks: vec![block(), block()],
            norm: LayerNormConfig::new(d).init(device),
            pos_embed: Param::from_tensor(Tensor::zeros([1, MAX_DATES, d / 2], device)).set_require_grad(false),
            month_embed: EmbeddingConfig::new(12, d / 4).init(device),
            channel_embed: EmbeddingConfig::new(GROUPS.len() + 1, d / 4).init(device),
        }
    }

    /// The authors' weights (the `encoder.` part of their state dict).
    pub fn pretrained(path: &Path, device: &Device) -> Result<Encoder, String> {
        let mut enc = Encoder::new(device);
        let mut store = PytorchStore::from_file(path).with_key_remapping(r"^encoder\.", "");
        for (k, key) in GROUP_KEYS.iter().enumerate() {
            store = store
                .with_key_remapping(format!(r"^eo_patch_embed\.{key}\."), format!("eo_patch_embed.{}.", GROUPS[k].0));
        }
        let store = store.allow_partial(false);
        let mut store = store;
        let r = enc.load_from(&mut store).map_err(|e| format!("{}: Presto's weights: {e}", path.display()))?;
        // the pretraining's decoder is not used
        let unused: Vec<&String> = r.unused.iter().filter(|k| !k.starts_with("decoder.")).collect();
        if !r.missing.is_empty() || !unused.is_empty() || !r.errors.is_empty() {
            return Err(format!(
                "{}: Presto's weights do not fit its encoder (missing {:?}, unused {:?}, errors {:?})",
                path.display(),
                r.missing,
                unused,
                r.errors
            ));
        }
        Ok(enc)
    }

    /// The encodings [b, 128] of b series: `x` and `missing` [b, t, 17] (Presto's normalized inputs; 1 where
    /// missing), `months` [b, t] (0-11), `latlon` [b, 2] (degrees). Dynamic World is never given (its tokens are
    /// all masked, so they are left out).
    pub fn forward(&self, x: Tensor<3>, missing: Tensor<3>, months: Tensor<2, Int>, latlon: Tensor<2>) -> Tensor<2> {
        let [b, t, _] = x.dims();
        let device = x.device();
        let q = DIMS / 4;
        let month = self.month_embed.forward(months); // [b, t, 32]
        let pos = self.pos_embed.val().slice([0..1, 0..t, 0..DIMS / 2]).expand([b, t, DIMS / 2]);
        let channels = self.channel_embed.weight.val();
        let (mut tokens, mut keep) = (vec![], vec![]);
        for (k, (name, range)) in GROUPS.iter().enumerate() {
            let tok = self.eo_patch_embed.group(k).forward(x.clone().slice([0..b, 0..t, range.clone()]));
            let channel = channels.clone().slice([k..k + 1, 0..q]).reshape([1, 1, q]);
            let miss = missing.clone().slice([0..b, 0..t, range.clone()]).max_dim(2).reshape([b, t]);
            if *name == "srtm" {
                // static: one token, with neither month nor date
                let tok = tok.slice([0..b, 0..1, 0..DIMS]);
                let add = Tensor::cat(
                    vec![
                        Tensor::zeros([b, 1, q], &device),
                        channel.expand([b, 1, q]),
                        Tensor::zeros([b, 1, DIMS / 2], &device),
                    ],
                    2,
                );
                tokens.push(tok + add);
                keep.push(miss.slice([0..b, 0..1]).neg().add_scalar(1.0));
            } else {
                tokens.push(tok + Tensor::cat(vec![month.clone(), channel.expand([b, t, q]), pos.clone()], 2));
                keep.push(miss.neg().add_scalar(1.0));
            }
        }
        // the pixel's place first, always observed
        let ll = latlon * (std::f64::consts::PI / 180.0);
        let lat = ll.clone().slice([0..b, 0..1]);
        let lon = ll.slice([0..b, 1..2]);
        let cart =
            Tensor::cat(vec![lat.clone().cos() * lon.clone().cos(), lat.clone().cos() * lon.sin(), lat.sin()], 1);
        tokens.insert(0, self.latlon_embed.forward(cart).reshape([b, 1, DIMS]));
        keep.insert(0, Tensor::ones([b, 1], &device));
        let keep = Tensor::cat(keep, 1); // [b, n]
        let n = keep.dims()[1];
        let mut x = Tensor::cat(tokens, 1) * keep.clone().reshape([b, n, 1]);
        // attention over the tokens observed only
        let bias = (keep.clone() - 1.0).mul_scalar(1e9).reshape([b, 1, 1, n]);
        let dh = DIMS / HEADS;
        for blk in &self.blocks {
            let h = blk.norm1.forward(x.clone());
            let qkv = blk.attn.qkv.forward(h).reshape([b, n, 3, HEADS, dh]).permute([2, 0, 3, 1, 4]);
            let part = |i: usize| qkv.clone().slice([i..i + 1, 0..b, 0..HEADS, 0..n, 0..dh]).reshape([b, HEADS, n, dh]);
            let (qq, kk, vv) = (part(0), part(1), part(2));
            let att = softmax(qq.matmul(kk.swap_dims(2, 3)) / (dh as f64).sqrt() + bias.clone(), 3);
            let o = att.matmul(vv).swap_dims(1, 2).reshape([b, n, DIMS]);
            x = x + blk.attn.proj.forward(o);
            let h = blk.norm2.forward(x.clone());
            x = x.clone() + blk.mlp.fc2.forward(gelu(blk.mlp.fc1.forward(h)));
        }
        let w = keep.reshape([b, n, 1]);
        let mean = (x * w.clone()).sum_dim(1) / w.sum_dim(1);
        self.norm.forward(mean.reshape([b, DIMS]))
    }
}

/// The band (0-based) holding each of Presto's first 16 inputs: by name (zeit's aliases, any case), else as
/// `fallback` says (Janus's band roles); a band holds one input at most.
pub fn match_names(names: &[String], fallback: &dyn Fn(&str) -> Option<usize>) -> Vec<Option<usize>> {
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let mut used = vec![];
    INPUTS[..16]
        .iter()
        .map(|input| {
            let by_name = aliases(input).iter().find_map(|a| lower.iter().position(|n| n == a));
            let found = by_name.or_else(|| fallback(input)).filter(|b| !used.contains(b));
            if let Some(b) = found {
                used.push(b);
            }
            found
        })
        .collect()
}

/// How a series feeds Presto: each input's band (1-based, 0 = not there), the units, the dates' months.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PrestoInputs {
    /// for each of Presto's 16 inputs (not the NDVI, which Presto computes): the band of the series' dates and its
    /// name, or none
    pub bands: Vec<Option<InputBand>>,
    /// factor bringing Sentinel-2 to 0-10000 (1e4 for reflectance 0-1)
    pub reflectance_scale: f32,
    /// the Sentinel-1 bands hold linear backscatter (converted to dB)
    pub s1_linear: bool,
    /// monthly composites of this year (else the series' own dates, at most 24)
    pub year: Option<i32>,
}

impl PrestoInputs {
    /// Presto's inputs in a series: the bands by name (zeit's aliases), else by Janus's band roles; the units from
    /// the values' 2-98 % quantiles of a sample (`ranges`, by band: reflectance 0-1 when below 2; dB backscatter is
    /// mostly negative).
    pub fn of(info: &CubeInfo, ranges: &[(f32, f32)], year: Option<i32>) -> Result<PrestoInputs, String> {
        let roles = janus_cube::roles::guess_band_roles(info);
        let by_role = |input: &str| {
            let role = match input {
                "B2" => "blue",
                "B3" => "green",
                "B4" => "red",
                "B8" => "nir",
                "B11" => "swir1",
                "B12" => "swir2",
                _ => "",
            };
            roles.get(role).map(|&b| b as usize - 1)
        };
        let bands: Vec<Option<InputBand>> = match_names(&info.band_names, &by_role)
            .into_iter()
            .map(|k| k.map(|k| InputBand::source(info, k as i32 + 1)))
            .collect();
        if bands.iter().all(Option::is_none) {
            return Err(format!(
                "none of the bands ({}) is an input of Presto: name them as Sentinel-2 (blue, red, nir, B8A, swir1...), \
                 Sentinel-1 (VV, VH), ERA5 or SRTM bands",
                info.band_names.join(", ")
            ));
        }
        let high = |k: usize| bands[k].as_ref().and_then(|b| ranges.get(b.band as usize - 1)).map(|r| r.1);
        let low = |k: usize| bands[k].as_ref().and_then(|b| ranges.get(b.band as usize - 1)).map(|r| r.0);
        let s2_high = (2..12).filter_map(high).fold(f32::NAN, f32::max);
        let s1_low = (0..2).filter_map(low).fold(f32::NAN, f32::min);
        Ok(PrestoInputs {
            bands,
            reflectance_scale: if s2_high.is_finite() && s2_high <= 2.0 { 1e4 } else { 1.0 },
            s1_linear: s1_low.is_finite() && s1_low >= 0.0,
            year,
        })
    }

    /// The bands read from the series, in their order.
    pub fn read_bands(&self) -> Vec<InputBand> {
        self.bands.iter().flatten().cloned().collect()
    }

    /// The months (0-11) of what Presto is given, and for each the series' dates it is made of.
    pub fn steps(&self, info: &CubeInfo) -> Result<(Vec<i64>, Vec<Vec<usize>>), String> {
        let n = info.t();
        let month_of = |t: usize| {
            let y = info.decimal_year(t);
            let (yr, frac) = (y.floor(), y - y.floor());
            (yr as i32, ((frac * 12.0).floor() as i64).clamp(0, 11))
        };
        if !info.time_is_date {
            return Err("Presto needs the series' dates (it encodes their months)".into());
        }
        match self.year {
            Some(year) => {
                let dates: Vec<Vec<usize>> =
                    (0..12).map(|m| (0..n).filter(|&t| month_of(t) == (year, m)).collect()).collect();
                if dates.iter().all(Vec::is_empty) {
                    return Err(format!("the series has no date in {year}"));
                }
                Ok(((0..12).collect(), dates))
            }
            None => {
                if n > MAX_DATES {
                    return Err(format!("Presto takes at most {MAX_DATES} dates (this series has {n}): choose a year"));
                }
                Ok(((0..n).map(|t| month_of(t).1).collect(), (0..n).map(|t| vec![t]).collect()))
            }
        }
    }
}

/// Presto's inputs of n pixels: [n][steps][17 normalized values, 17 missing flags (1 = missing)], their months and
/// their latitude and longitude, from the bands read [n][dates][bands] (NaN missing).
pub fn prepare(raw: &Series, inputs: &PrestoInputs, steps: &[Vec<usize>]) -> Series {
    let (n, t) = (raw.n, steps.len());
    let mut out = vec![0f32; n * t * 34];
    let read: Vec<(usize, usize)> =
        inputs.bands.iter().enumerate().filter_map(|(k, b)| b.as_ref().map(|_| k)).enumerate().collect();
    for i in 0..n {
        let px = raw.pixel(i);
        // each input of each step: the median of its dates' values
        let mut v = vec![f32::NAN; t * 17];
        for (s, dates) in steps.iter().enumerate() {
            for &(j, k) in &read {
                let mut vals: Vec<f32> = dates.iter().map(|&d| px[d * raw.b + j]).filter(|x| x.is_finite()).collect();
                if vals.is_empty() {
                    continue;
                }
                vals.sort_by(f32::total_cmp);
                let m = vals.len();
                let mut x = if m % 2 == 1 { vals[m / 2] } else { 0.5 * (vals[m / 2 - 1] + vals[m / 2]) };
                if (2..12).contains(&k) {
                    x *= inputs.reflectance_scale;
                }
                if k < 2 && inputs.s1_linear {
                    x = 10.0 * x.max(1e-10).log10();
                }
                v[s * 17 + k] = x;
            }
        }
        // SRTM is static: the mean of the steps
        for k in 14..16 {
            let vals: Vec<f32> = (0..t).map(|s| v[s * 17 + k]).filter(|x| x.is_finite()).collect();
            let mean = if vals.is_empty() { f32::NAN } else { vals.iter().sum::<f32>() / vals.len() as f32 };
            for s in 0..t {
                v[s * 17 + k] = mean;
            }
        }
        let o = &mut out[i * t * 34..(i + 1) * t * 34];
        for s in 0..t {
            for k in 0..16 {
                let x = v[s * 17 + k];
                let miss = !x.is_finite();
                o[s * 34 + k] = if miss { 0.0 } else { (x + SHIFT[k]) / DIV[k] };
                o[s * 34 + 17 + k] = miss as u8 as f32;
            }
            // the NDVI of the normalized NIR and red
            let (nir, red) = (o[s * 34 + 8], o[s * 34 + 4]);
            let total = nir + red;
            o[s * 34 + 16] = if total > 0.0 { (nir - red) / total } else { 0.0 };
            o[s * 34 + 17 + 16] = (o[s * 34 + 17 + 8] > 0.0 || o[s * 34 + 17 + 4] > 0.0) as u8 as f32;
        }
    }
    Series { n, t, b: 34, values: out }
}

/// Whether a pixel has any observation (else it gets no encoding).
pub fn observed(x: &Series, i: usize) -> bool {
    let px = x.pixel(i);
    (0..x.t).any(|s| (0..16).any(|k| px[s * 34 + 17 + k] == 0.0))
}

/// Pixels a batch at most so that the attention's scores (batch x heads x tokens x tokens) stay within 64 MB: a
/// year of 12 months is up to 12 x 9 + 2 tokens a pixel, and 8192 pixels would ask the GPU for a buffer of 2.5 GB
/// (more than wgpu gives).
pub fn batch_for(steps: usize) -> usize {
    let tokens = steps * GROUPS.len() + 2;
    ((16 << 20) / (HEADS * tokens * tokens)).clamp(16, 8192)
}

/// The encodings [n][128] of prepared inputs (n x steps x 34), their months and places, in batches (at most
/// `batch`, and at most batch_for's).
pub fn encode(enc: &Encoder, x: &Series, months: &[i64], latlon: &[f32], batch: usize, device: &Device) -> Vec<f32> {
    let t = x.t;
    let batch = batch.min(batch_for(t));
    let mut out = Vec::with_capacity(x.n * DIMS);
    for s in (0..x.n).step_by(batch.max(1)) {
        let m = batch.min(x.n - s);
        let all = Tensor::<3>::from_data(
            TensorData::new(x.values[s * t * 34..(s + m) * t * 34].to_vec(), [m, t, 34]),
            device,
        );
        let v = all.clone().slice([0..m, 0..t, 0..17]);
        let miss = all.slice([0..m, 0..t, 17..34]);
        let mo: Vec<i64> = (0..m).flat_map(|_| months.iter().copied()).collect();
        let mo = Tensor::<2, Int>::from_data(TensorData::new(mo, [m, t]), device);
        let ll = Tensor::<2>::from_data(TensorData::new(latlon[s * 2..(s + m) * 2].to_vec(), [m, 2]), device);
        let e = enc.forward(v, miss, mo, ll);
        out.extend(e.into_data().try_to_vec::<f32>().unwrap_or_else(|_| vec![f32::NAN; m * DIMS]));
    }
    out
}

/// Latitude and longitude (degrees) of pixels of a series (their centres), `[lat, lon]` each.
pub fn latlon_of(info: &CubeInfo, pixels: &[(f64, f64)]) -> Result<Vec<f32>, String> {
    use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
    let g = info.geo_transform.ok_or("Presto needs the series' georeferencing (the pixels' latitude and longitude)")?;
    let mut src = SpatialRef::from_wkt(&info.crs_wkt).map_err(|e| format!("the series' CRS: {e}"))?;
    let mut dst = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&src, &dst).map_err(|e| e.to_string())?;
    let mut xs: Vec<f64> = pixels.iter().map(|&(c, r)| g[0] + (c + 0.5) * g[1] + (r + 0.5) * g[2]).collect();
    let mut ys: Vec<f64> = pixels.iter().map(|&(c, r)| g[3] + (c + 0.5) * g[4] + (r + 0.5) * g[5]).collect();
    let mut zs = vec![0.0; xs.len()];
    ct.transform_coords(&mut xs, &mut ys, &mut zs).map_err(|e| e.to_string())?;
    Ok(xs.iter().zip(&ys).flat_map(|(&lon, &lat)| [lat as f32, lon as f32]).collect())
}

/// Where the weights are: a folder JANUS_WEIGHTS (or ZEIT_WEIGHTS) names, Zeit's cache, else Janus's (downloaded
/// there on first use, checked against their SHA-256).
pub fn weights(cache: &Path, progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<PathBuf, String> {
    let name = "default_model.pt";
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let candidates: Vec<PathBuf> = ["JANUS_WEIGHTS", "ZEIT_WEIGHTS"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(|d| PathBuf::from(d).join(name)))
        .chain(home.map(|h| h.join(".cache").join("zeit").join("weights").join(name)))
        .chain(std::iter::once(cache.join(name)))
        .collect();
    for c in &candidates {
        if c.exists() && sha256_ok(c)? {
            return Ok(c.clone());
        }
    }
    let path = cache.join(name);
    crate::fetch::fetch(WEIGHTS_URL, &path, progress, cancel)?;
    if !sha256_ok(&path)? {
        let _ = std::fs::remove_file(&path);
        return Err(format!("{WEIGHTS_URL} is not Presto's weights (its SHA-256 differs)"));
    }
    Ok(path)
}

/// The file's SHA-256 is the weights' (a `.sha256` beside it records a check).
fn sha256_ok(path: &Path) -> Result<bool, String> {
    let stamp = PathBuf::from(format!("{}.sha256", path.display()));
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == WEIGHTS_SHA256) {
        return Ok(true);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ok = crate::fetch::sha256_hex(&bytes) == WEIGHTS_SHA256;
    if ok {
        let _ = std::fs::write(&stamp, WEIGHTS_SHA256);
    }
    Ok(ok)
}

/// The encoder, loaded once per device.
pub fn shared_encoder(path: &Path, device: &Device) -> Result<Arc<Encoder>, String> {
    static CACHE: std::sync::Mutex<Vec<(PathBuf, String, Arc<Encoder>)>> = std::sync::Mutex::new(Vec::new());
    let key = format!("{device:?}");
    let mut c = CACHE.lock().unwrap();
    if let Some(e) = c.iter().find(|e| e.0 == path && e.1 == key) {
        return Ok(e.2.clone());
    }
    let enc = Arc::new(Encoder::pretrained(path, device)?);
    c.push((path.to_path_buf(), key, enc.clone()));
    Ok(enc)
}

/// The tag of Presto's embedding files (as zeit.ai.presto_embeddings tags its cubes).
pub fn embedding_tag(variant: &str) -> String {
    serde_json::json!({
        "embedding_source": "presto",
        "embedding_version": VERSION,
        "embedding_variant": variant,
        "embedding_model": "Presto",
        "embedding_dimensions": DIMS,
        "embedding_license": "MIT",
        "embedding_citation": "Tseng et al. (2023), Lightweight, Pre-trained Transformers for Remote Sensing Timeseries, arXiv:2304.14065",
    })
    .to_string()
}

/// Presto's encodings of a window of a series for a year (or its own dates): a GeoTIFF of 128 bands (A00-A127,
/// Int16 x 0.0001), tagged as embeddings (the Embeddings panel opens it as TESSERA's).
#[allow(clippy::too_many_arguments)]
pub fn embed_window(
    info: &Arc<CubeInfo>,
    inputs: &PrestoInputs,
    window: [usize; 4],
    weights: &Path,
    path: &Path,
    device: &Device,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<usize, String> {
    use gdal::Metadata;
    use gdal::raster::{Buffer, RasterCreationOptions};
    let enc = shared_encoder(weights, device)?;
    let (months, steps) = inputs.steps(info)?;
    let read = inputs.read_bands();
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let opts = RasterCreationOptions::from_iter([
        "TILED=YES",
        "BLOCKXSIZE=256",
        "BLOCKYSIZE=256",
        "COMPRESS=DEFLATE",
        "PREDICTOR=2",
        "INTERLEAVE=PIXEL",
        "BIGTIFF=IF_SAFER",
    ]);
    let tmp = PathBuf::from(format!("{}.part", path.display()));
    let mut ds = drv
        .create_with_band_type_with_options::<i16, _>(&tmp, w, h, DIMS, &opts)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Some(g) = info.geo_transform {
        let gx = g[0] + x0 as f64 * g[1] + y0 as f64 * g[2];
        let gy = g[3] + x0 as f64 * g[4] + y0 as f64 * g[5];
        ds.set_geo_transform(&[gx, g[1], g[2], gy, g[4], g[5]]).map_err(|e| e.to_string())?;
    }
    if !info.crs_wkt.is_empty() {
        ds.set_projection(&info.crs_wkt).map_err(|e| e.to_string())?;
    }
    let _ = ds.set_metadata_item("ZEIT_EMBEDDING", &embedding_tag("pretrained"), "");
    const SCALE: f64 = 1e-4;
    for b in 0..DIMS {
        let mut band = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
        let _ = band.set_no_data_value(Some(-32768.0));
        let _ = band.set_description(&format!("A{b:02}"));
        let _ = band.set_scale(SCALE);
        let _ = band.set_offset(0.0);
    }
    let rows = (16384 / w.max(1)).clamp(1, 64);
    let mut done = 0;
    let mut encoded = 0;
    let t0 = std::time::Instant::now();
    for r in (0..h).step_by(rows) {
        if cancel.load(Ordering::Relaxed) {
            drop(ds);
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let raw = crate::data::read_window(info, &read, (x0, y0 + r, w, hh));
        let x = prepare(&raw, inputs, &steps);
        let pixels: Vec<(f64, f64)> = (0..w * hh).map(|i| ((x0 + i % w) as f64, (y0 + r + i / w) as f64)).collect();
        let ll = latlon_of(info, &pixels)?;
        let idx: Vec<usize> = (0..x.n).filter(|&i| observed(&x, i)).collect();
        let sel = x.select(&idx);
        let ll_sel: Vec<f32> = idx.iter().flat_map(|&i| [ll[2 * i], ll[2 * i + 1]]).collect();
        let e = encode(&enc, &sel, &months, &ll_sel, 8192, device);
        encoded += idx.len();
        // [band][row][col], quantised
        let mut q = vec![-32768i16; DIMS * w * hh];
        for (j, &i) in idx.iter().enumerate() {
            for d in 0..DIMS {
                q[d * w * hh + i] = (e[j * DIMS + d] as f64 / SCALE).round_ties_even().clamp(-32767.0, 32767.0) as i16;
            }
        }
        for d in 0..DIMS {
            let part = q[d * w * hh..(d + 1) * w * hh].to_vec();
            ds.rasterband(d + 1)
                .map_err(|e| e.to_string())?
                .write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), part))
                .map_err(|e| e.to_string())?;
        }
        done += hh;
        let el = t0.elapsed().as_secs_f64();
        progress(
            done as f64 / h as f64,
            &format!("rows {done}/{h}, {el:.0} s elapsed, ~{:.0} s left", el / done as f64 * (h - done) as f64),
        );
    }
    drop(ds);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(encoded)
}

/// Presto as a classifier: the encoder as pretrained (frozen while training) and a head on its encodings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PrestoHyper {
    pub inputs: PrestoInputs,
    /// the head's hidden layers (none: a linear classifier, as zeit.ai.presto's)
    pub hidden: Vec<usize>,
    /// the authors' weights, for a training (a model file has its own)
    #[serde(skip)]
    pub weights: Option<PathBuf>,
}

/// Values a step of a classifier's input holds: 17 normalized inputs, 17 missing flags, the latitude and
/// longitude (the same on every step) and the month (0-11).
pub const CHANNELS: usize = 37;

#[derive(Module, Debug)]
pub struct PrestoNet {
    encoder: Encoder,
    hidden: Vec<Linear>,
    out: Linear,
}

impl PrestoNet {
    pub fn new(h: &PrestoHyper, classes: usize, device: &Device) -> Result<PrestoNet, String> {
        let encoder = match &h.weights {
            Some(w) => Encoder::pretrained(w, device)?,
            None => Encoder::new(device),
        };
        let mut c = DIMS;
        let mut hidden = vec![];
        for &k in &h.hidden {
            hidden.push(LinearConfig::new(c, k).init(device));
            c = k;
        }
        Ok(PrestoNet { encoder, hidden, out: LinearConfig::new(c, classes).init(device) })
    }

    /// The encoder kept as it is while the head learns.
    pub fn freeze_encoder(mut self) -> PrestoNet {
        self.encoder = self.encoder.no_grad();
        self
    }

    /// [n, steps, CHANNELS] -> the classes' scores.
    pub fn forward(&self, x: Tensor<3>) -> Tensor<2> {
        let [n, t, _] = x.dims();
        let v = x.clone().slice([0..n, 0..t, 0..17]);
        let miss = x.clone().slice([0..n, 0..t, 17..34]);
        let latlon = x.clone().slice([0..n, 0..1, 34..36]).reshape([n, 2]);
        let months = x.slice([0..n, 0..t, 36..37]).reshape([n, t]).int();
        let mut e = self.encoder.forward(v, miss, months, latlon).detach();
        for l in &self.hidden {
            e = burn::tensor::activation::relu(l.forward(e));
        }
        self.out.forward(e)
    }
}

/// A classifier's input: prepared inputs (n x steps x 34) with each pixel's place and each step's month.
pub fn series(x: &Series, months: &[i64], latlon: &[f32]) -> Series {
    let (n, t) = (x.n, x.t);
    let mut values = vec![0f32; n * t * CHANNELS];
    for i in 0..n {
        for s in 0..t {
            let src = &x.values[(i * t + s) * 34..(i * t + s + 1) * 34];
            let dst = &mut values[(i * t + s) * CHANNELS..(i * t + s + 1) * CHANNELS];
            dst[..34].copy_from_slice(src);
            dst[34] = latlon[2 * i];
            dst[35] = latlon[2 * i + 1];
            dst[36] = months[s] as f32;
        }
    }
    Series { n, t, b: CHANNELS, values }
}

/// What a classifier on Presto reads of n pixels (their raw bands, [n][dates][bands], at `pixels`): its input, and
/// whether each pixel has an observation.
pub fn classifier_input(
    info: &CubeInfo,
    inputs: &PrestoInputs,
    raw: &Series,
    pixels: &[(f64, f64)],
) -> Result<(Series, Vec<bool>), String> {
    let (months, steps) = inputs.steps(info)?;
    let x = prepare(raw, inputs, &steps);
    let keep: Vec<bool> = (0..x.n).map(|i| observed(&x, i)).collect();
    let ll = latlon_of(info, pixels)?;
    Ok((series(&x, &months, &ll), keep))
}
