//! A tool over a window of the series (the bridge's `job` mode: run_raster, Inputs, Outputs): the shown series
//! (with its normalized difference and quality band applied) and the mapped bands read in full-width row bands,
//! the next band read while the tool computes this one, and the maps written as GeoTIFFs as the bridge wrote
//! them (float32 with NaN, or int16 with a scale; series outputs one band per date). The spec is the bridge's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::raster::{Buffer, RasterCreationOptions};
use gdal::{Dataset, DriverManager, Metadata};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::manifest::{Output, Tool, manifest};
use crate::pixel::{Params, PixelInput, Shown};

/// What Janus sends for a run (the bridge's job spec).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct JobSpec {
    pub tool: String,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// the shown band of each date (a VRT of T bands)
    #[serde(default)]
    pub input: String,
    /// the normalized difference's other band, and the quality band (raw codes)
    #[serde(default)]
    pub nd_input: Option<String>,
    #[serde(default)]
    pub qa_input: Option<String>,
    #[serde(default)]
    pub qa_rule: Option<String>,
    /// multiband tools: each mapped role's band
    #[serde(default)]
    pub bands: BTreeMap<String, String>,
    #[serde(default)]
    pub nodata: Option<f64>,
    #[serde(default)]
    pub years: Vec<f64>,
    #[serde(default)]
    pub days: Option<Vec<i64>>,
    /// Janus's date labels: the band descriptions of series outputs
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub shown: Option<Shown>,
    #[serde(default)]
    pub map_year: Option<f64>,
    /// x0, y0, x1, y1 in the input's pixels
    #[serde(default)]
    pub window: Option<[usize; 4]>,
    #[serde(default)]
    pub output_dir: String,
    /// CPU threads (Settings); none or <= 0: not limited
    #[serde(default)]
    pub threads: Option<i64>,
}

impl JobSpec {
    /// Zeit's n_jobs.
    pub fn n_jobs(&self) -> i32 {
        match self.threads {
            Some(n) if n > 0 => n as i32,
            _ => -1,
        }
    }

    /// The series' context (dates, what is shown) as a pixel input without values.
    pub fn context(&self) -> PixelInput {
        PixelInput {
            years: self.years.clone(),
            days: self.days.clone(),
            shown: self.shown.clone(),
            map_year: self.map_year,
            qa_rule: self.qa_rule.clone(),
            ..Default::default()
        }
    }
}

/// A window of rows: the shown series and, for multiband tools, the mapped bands and the quality codes; each
/// [T][pixels] (pixels row by row), NaN = missing.
#[derive(Clone, Debug, Default)]
pub struct Chunk {
    pub t: usize,
    pub h: usize,
    pub w: usize,
    pub stack: Vec<f64>,
    pub bands: BTreeMap<String, Vec<f64>>,
    pub qa: Option<Vec<f64>>,
}

impl Chunk {
    pub fn pixels(&self) -> usize {
        self.h * self.w
    }

    /// [T][P] -> [P][T] (Zeit's batches take the pixels' series one after the other).
    pub fn by_pixel(&self, a: &[f64]) -> Vec<f64> {
        let (t, n) = (self.t, self.pixels());
        let mut out = vec![0.0; a.len()];
        for i in 0..t {
            for p in 0..n {
                out[p * t + i] = a[i * n + p];
            }
        }
        out
    }

    /// One pixel's series and bands, as a pixel run gets them.
    pub fn pixel_input(&self, ctx: &PixelInput, p: usize) -> PixelInput {
        let n = self.pixels();
        let col = |a: &[f64]| (0..self.t).map(|i| a[i * n + p]).collect::<Vec<f64>>();
        PixelInput {
            values: col(&self.stack),
            bands: self.bands.iter().map(|(r, a)| (r.clone(), col(a))).collect(),
            qa: self.qa.as_deref().map(col),
            ..ctx.clone()
        }
    }
}

/// What a run shares between its chunks: the series' context and what a tool decides once per job (the
/// reflectance units), and the model a tool fits first (a SOM's).
#[derive(Default)]
pub struct JobCtx {
    pub input: PixelInput,
    pub n_jobs: i32,
    pub cache: Mutex<BTreeMap<String, String>>,
    pub model: Option<Value>,
}

impl JobCtx {
    /// A value decided once per job (zeit_common's ctx["cache"]).
    pub fn once(&self, key: &str, f: impl FnOnce() -> String) -> String {
        let mut c = self.cache.lock().unwrap();
        c.entry(key.to_string()).or_insert_with(f).clone()
    }
}

/// A tool's maps of a chunk by output id: [pixels] for a map, [T][pixels] for a series.
pub type ChunkOut = BTreeMap<String, Vec<f64>>;

/// The observations the quality rule keeps (NaN codes unusable; the bridge's qa_usable).
pub fn qa_usable(rule: Option<&str>, qa: f64) -> bool {
    let ok = qa.is_finite() && qa >= 0.0;
    let code = if ok { qa as i64 } else { 0 };
    let keep = match rule {
        Some("fmask") => code == 0 || code == 1,
        Some("landsat_c2") => code & 0x3B == 0,
        Some("nonzero") => code != 0,
        _ => true,
    };
    ok && keep
}

// ---------------------------------------------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------------------------------------------

struct Inputs {
    spec: JobSpec,
    input: Dataset,
    nd: Option<Dataset>,
    qa: Option<Dataset>,
    bands: Vec<(String, Dataset)>,
}

fn open(path: &str) -> Result<Dataset, String> {
    Dataset::open(path).map_err(|e| format!("{path}: {e}"))
}

/// Every band of a window as float64, [band][pixels]; unless `raw`, the dataset's NoData and non-finite values
/// NaN (the bridge's `_read`).
fn read(ds: &Dataset, x0: usize, y0: usize, w: usize, h: usize, raw: bool) -> Result<Vec<f64>, String> {
    let mut out = Vec::with_capacity(ds.raster_count() * w * h);
    for b in 1..=ds.raster_count() {
        let band = ds.rasterband(b).map_err(|e| e.to_string())?;
        let buf: Buffer<f64> =
            band.read_as((x0 as isize, y0 as isize), (w, h), (w, h), None).map_err(|e| e.to_string())?;
        let nodata = if raw { None } else { band.no_data_value().filter(|v| v.is_finite()) };
        out.extend(buf.data().iter().map(|&v| {
            if raw {
                v
            } else if nodata == Some(v) || !v.is_finite() {
                f64::NAN
            } else {
                v
            }
        }));
    }
    Ok(out)
}

impl Inputs {
    fn new(spec: &JobSpec) -> Result<Inputs, String> {
        let input = open(&spec.input)?;
        let t = spec.years.len();
        if input.raster_count() != t {
            return Err(format!("ValueError: input has {} bands but {t} dates were given", input.raster_count()));
        }
        let (w, h) = input.raster_size();
        let matching = |name: &str, ds: Dataset| -> Result<Dataset, String> {
            let (sw, sh) = ds.raster_size();
            if ds.raster_count() != t || sw != w || sh != h {
                return Err(format!(
                    "ValueError: {name} does not match the input ({} bands, {sw} x {sh})",
                    ds.raster_count()
                ));
            }
            Ok(ds)
        };
        let nd = spec.nd_input.as_deref().map(|p| open(p).and_then(|d| matching("nd_input", d))).transpose()?;
        let qa = spec.qa_input.as_deref().map(|p| open(p).and_then(|d| matching("qa_input", d))).transpose()?;
        let bands = spec
            .bands
            .iter()
            .map(|(r, p)| open(p).and_then(|d| matching(&format!("band:{r}"), d)).map(|d| (r.clone(), d)))
            .collect::<Result<_, _>>()?;
        Ok(Inputs { spec: spec.clone(), input, nd, qa, bands })
    }

    /// The shown series of a window (normalized difference, quality mask) and its bands and codes.
    fn read(&self, x0: usize, y0: usize, w: usize, h: usize) -> Result<Chunk, String> {
        let mut stack = read(&self.input, x0, y0, w, h, true)?;
        let nodata = self.spec.nodata;
        for v in stack.iter_mut() {
            if nodata == Some(*v) || !v.is_finite() {
                *v = f64::NAN;
            }
        }
        let qa = self.qa.as_ref().map(|d| read(d, x0, y0, w, h, true)).transpose()?;
        if let Some(nd) = &self.nd {
            let b = read(nd, x0, y0, w, h, false)?;
            for (a, b) in stack.iter_mut().zip(&b) {
                let s = *a + b;
                *a = if s != 0.0 { (*a - b) / s } else { f64::NAN };
            }
        }
        if let Some(q) = &qa {
            let rule = self.spec.qa_rule.as_deref();
            for (v, &code) in stack.iter_mut().zip(q) {
                if !qa_usable(rule, code) {
                    *v = f64::NAN;
                }
            }
        }
        let bands = self
            .bands
            .iter()
            .map(|(r, d)| read(d, x0, y0, w, h, false).map(|a| (r.clone(), a)))
            .collect::<Result<_, _>>()?;
        Ok(Chunk { t: self.spec.years.len(), h, w, stack, bands, qa })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------------------------------------------

/// The outputs a run writes (one with "when" only if that bool parameter is on).
pub fn active_outputs<'a>(tool: &'a Tool, p: &Params) -> Vec<&'a Output> {
    tool.outputs.iter().filter(|o| o.when.as_deref().is_none_or(|w| p.bool(w))).collect()
}

/// One label per date for a series output's band descriptions: Janus's, else ISO dates, else decimal years.
fn date_labels(spec: &JobSpec) -> Vec<String> {
    if let Some(l) = spec.labels.as_ref().filter(|l| l.len() == spec.years.len()) {
        return l.clone();
    }
    if let Some(days) = &spec.days {
        return days
            .iter()
            .map(|&d| {
                let (y, m, dd) = crate::numeric::civil(d);
                format!("{y:04}-{m:02}-{dd:02}")
            })
            .collect();
    }
    spec.years.iter().map(|y| format!("{y:.4}")).collect()
}

struct Outputs {
    w: usize,
    outs: Vec<(Output, PathBuf, Dataset)>,
}

impl Outputs {
    fn new(
        tool: &Tool,
        outputs: &[&Output],
        dir: &Path,
        src: &Dataset,
        window: [usize; 4],
        spec: &JobSpec,
    ) -> Result<Outputs, String> {
        let (w, h) = (window[2] - window[0], window[3] - window[1]);
        let gt = src.geo_transform().ok();
        let wkt = src.projection();
        let threads = match spec.threads {
            Some(n) if n > 0 => n.to_string(),
            _ => "ALL_CPUS".into(),
        };
        let labels = date_labels(spec);
        let driver = DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
        let mut outs = vec![];
        for o in outputs {
            let path = dir.join(format!("{}_{}.tif", tool.id, o.id));
            let count = if o.series == Some(true) { labels.len() } else { 1 };
            let int16 = o.dtype.as_deref() == Some("int16");
            let mut opts = vec![
                "COMPRESS=DEFLATE".to_string(),
                "TILED=YES".into(),
                "BLOCKXSIZE=256".into(),
                "BLOCKYSIZE=256".into(),
                "BIGTIFF=IF_SAFER".into(),
                format!("NUM_THREADS={threads}"),
            ];
            if count > 1 {
                opts.push("INTERLEAVE=BAND".into());
            }
            if int16 {
                opts.push("PREDICTOR=2".into());
            }
            let options = RasterCreationOptions::from_iter(opts);
            let mut ds = if int16 {
                driver.create_with_band_type_with_options::<i16, _>(&path, w, h, count, &options)
            } else {
                driver.create_with_band_type_with_options::<f32, _>(&path, w, h, count, &options)
            }
            .map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some(g) = gt {
                let x = g[0] + window[0] as f64 * g[1] + window[1] as f64 * g[2];
                let y = g[3] + window[0] as f64 * g[4] + window[1] as f64 * g[5];
                ds.set_geo_transform(&[x, g[1], g[2], y, g[4], g[5]]).map_err(|e| e.to_string())?;
            }
            if !wkt.is_empty() {
                ds.set_projection(&wkt).map_err(|e| e.to_string())?;
            }
            for b in 1..=count {
                let mut band = ds.rasterband(b).map_err(|e| e.to_string())?;
                band.set_no_data_value(Some(if int16 { -32768.0 } else { f64::NAN })).map_err(|e| e.to_string())?;
                if o.series == Some(true) {
                    band.set_description(&labels[b - 1]).map_err(|e| e.to_string())?;
                }
                if int16 {
                    band.set_scale(o.scale.unwrap_or(1.0)).map_err(|e| e.to_string())?;
                    band.set_offset(0.0).map_err(|e| e.to_string())?;
                }
            }
            outs.push(((*o).clone(), path, ds));
        }
        Ok(Outputs { w, outs })
    }

    fn write(&mut self, res: &ChunkOut, r: usize, h: usize) -> Result<(), String> {
        let w = self.w;
        for (o, path, ds) in &mut self.outs {
            let a = res.get(&o.id).ok_or_else(|| format!("KeyError: '{}'", o.id))?;
            let count = ds.raster_count();
            for b in 0..count {
                let part = &a[b * w * h..(b + 1) * w * h];
                let mut band = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
                let result = if o.dtype.as_deref() == Some("int16") {
                    let scale = o.scale.unwrap_or(1.0);
                    let data: Vec<i16> = part
                        .iter()
                        .map(|&v| {
                            let q = (v / scale).round_ties_even();
                            if q.is_finite() { q.clamp(-32767.0, 32767.0) as i16 } else { -32768 }
                        })
                        .collect();
                    band.write((0, r as isize), (w, h), &mut Buffer::new((w, h), data))
                } else {
                    let data: Vec<f32> = part.iter().map(|&v| v as f32).collect();
                    band.write((0, r as isize), (w, h), &mut Buffer::new((w, h), data))
                };
                result.map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        Ok(())
    }

    /// The result's outputs: the manifest's, with their path, a class map's class names and what the tool's fit
    /// adds (the bridge's `describe`).
    fn describe(&self, p: &Params, model: Option<&Value>) -> Vec<Value> {
        self.outs
            .iter()
            .map(|(o, path, _)| {
                let mut d = serde_json::to_value(o).unwrap_or_default();
                d["path"] = json!(path.to_string_lossy());
                if let Some(param) = &o.classes_param {
                    let names: Vec<String> =
                        p.0.get(param)
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .enumerate()
                                    .map(|(k, c)| {
                                        c.get("name")
                                            .and_then(Value::as_str)
                                            .map_or_else(|| format!("Class {}", k + 1), str::to_string)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                    d["classes"] = json!(names);
                }
                if let Some(extra) =
                    model.and_then(|m| m.get("outputs")).and_then(|e| e.get(&o.id)).and_then(Value::as_object)
                {
                    for (k, v) in extra {
                        d[k] = v.clone();
                    }
                }
                d
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------------------------------

/// Rows of each full-width band a run computes at once.
pub fn rows_per_chunk(tool: &Tool, w: usize) -> usize {
    let cells = tool.chunk_cells.unwrap_or(4_000_000).max(0) as usize;
    (cells / w.max(1)).clamp(8, 512)
}

/// A tool's computation of a chunk.
pub fn run_chunk(tool: &str, p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    match tool {
        "landtrendr" => crate::landtrendr::chunk(p, chunk, ctx),
        "mann_kendall" => crate::mann_kendall::chunk(p, chunk, ctx),
        "bfast" => crate::bfast::bfast_chunk(p, chunk, ctx),
        "tmask" => crate::tmask::chunk(p, chunk, ctx),
        "ndfi" => crate::ndfi::chunk(p, chunk, ctx),
        "coded" => crate::coded::chunk(p, chunk, ctx),
        "ccdc" => crate::ccdc::chunk(p, chunk, ctx),
        "ccdc_synthetic" => crate::ccdc::synthetic_chunk(p, chunk, ctx),
        "bfast_lite" => crate::bfast::lite_chunk(p, chunk, ctx),
        "bfast_monitor" => crate::bfast::monitor_chunk(p, chunk, ctx),
        _ => Err(format!("{tool}: raster runs not available in this version")),
    }
}

/// A run's result: the outputs written (paths and descriptions) and the window.
pub struct JobResult {
    pub outputs: Vec<Value>,
    pub window: [usize; 4],
}

/// Runs a job: `progress(fraction, message)`; `cancel` stops it between chunks (an error "cancelled").
pub fn run_job(
    spec: &JobSpec,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let tool = manifest().tool(&spec.tool).ok_or_else(|| format!("KeyError: '{}'", spec.tool))?;
    let p = Params::with_defaults(&spec.tool, &spec.params).unwrap_or_default();
    let inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let ctx = JobCtx { input: spec.context(), n_jobs: spec.n_jobs(), ..Default::default() };
    let mut outs = Outputs::new(tool, &active_outputs(tool, &p), &dir, &inputs.input, window, spec)?;
    let rows = rows_per_chunk(tool, w);
    let bands: Vec<(usize, usize)> = (0..h).step_by(rows).map(|r| (r, rows.min(h - r))).collect();
    let t0 = std::time::Instant::now();
    // the next band read on a thread while this one is computed (the disk and the CPU at once)
    std::thread::scope(|s| -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Chunk, String>>(1);
        let bands_r = &bands;
        s.spawn(move || {
            for &(r, hh) in bands_r {
                if tx.send(inputs.read(x0, y0 + r, w, hh)).is_err() {
                    break;
                }
            }
        });
        let mut done = 0;
        for &(r, hh) in &bands {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let chunk = rx.recv().map_err(|e| e.to_string())??;
            let res = run_chunk(&spec.tool, &p, &chunk, &ctx)?;
            drop(chunk);
            outs.write(&res, r, hh)?;
            done += hh;
            let el = t0.elapsed().as_secs_f64();
            progress(
                done as f64 / h as f64,
                &format!("rows {done}/{h}, {el:.0} s elapsed, ~{:.0} s left", el / done as f64 * (h - done) as f64),
            );
        }
        Ok(())
    })?;
    let outputs = outs.describe(&p, ctx.model.as_ref());
    drop(outs);
    Ok(JobResult { outputs, window })
}

impl PixelInput {
    /// The pixel as a 1 x 1 chunk (the bridge's pixel_stack): a pixel run computes as a raster run does.
    pub fn as_chunk(&self) -> Chunk {
        Chunk {
            t: self.values.len(),
            h: 1,
            w: 1,
            stack: self.values.clone(),
            bands: self.bands.clone(),
            qa: self.qa.clone(),
        }
    }
}

impl JobCtx {
    /// A pixel run's context: one thread, nothing shared.
    pub fn for_pixel(input: &PixelInput) -> JobCtx {
        let input = PixelInput { values: vec![], bands: BTreeMap::new(), qa: None, ..input.clone() };
        JobCtx { input, n_jobs: 1, ..Default::default() }
    }
}
