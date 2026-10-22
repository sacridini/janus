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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::manifest::{Output, Tool, manifest};
use crate::pixel::{Params, PixelInput, Shown};

/// What Janus sends for a run (the bridge's job spec; job.json in the run's folder).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
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
    input: Source,
    nd: Option<Source>,
    qa: Option<Source>,
    bands: Vec<(String, Source)>,
}

fn open(path: &str) -> Result<Dataset, String> {
    Dataset::open(path).map_err(|e| format!("{path}: {e}"))
}

/// A dataset opened once per reading thread: its bands (a file per date, most often compressed) read on several
/// cores at once.
struct Source {
    handles: Vec<Dataset>,
}

impl Source {
    fn open(path: &str, threads: usize) -> Result<Source, String> {
        let first = open(path)?;
        let k = threads.clamp(1, first.raster_count().max(1));
        let mut handles = vec![first];
        for _ in 1..k {
            handles.push(open(path)?);
        }
        Ok(Source { handles })
    }

    fn ds(&self) -> &Dataset {
        &self.handles[0]
    }

    /// Every band of a window as float64, [band][pixels]; unless `raw`, the dataset's NoData and non-finite values
    /// NaN (the bridge's `_read`).
    fn read(&mut self, x0: usize, y0: usize, w: usize, h: usize, raw: bool) -> Result<Vec<f64>, String> {
        let n = self.ds().raster_count();
        if self.handles.len() == 1 {
            return read(self.ds(), 1..=n, x0, y0, w, h, raw);
        }
        let per = n.div_ceil(self.handles.len());
        let parts: Vec<Result<Vec<f64>, String>> = std::thread::scope(|s| {
            let running: Vec<_> = self
                .handles
                .iter_mut()
                .enumerate()
                .filter(|(i, _)| i * per < n)
                .map(|(i, ds)| {
                    let ds: &mut Dataset = ds;
                    s.spawn(move || read(ds, i * per + 1..=((i + 1) * per).min(n), x0, y0, w, h, raw))
                })
                .collect();
            running.into_iter().map(|r| r.join().unwrap_or_else(|_| Err("reading failed".into()))).collect()
        });
        let mut out = Vec::with_capacity(n * w * h);
        for part in parts {
            out.extend(part?);
        }
        Ok(out)
    }
}

/// The bands `bands` (1-based) of a window as float64, [band][pixels] (see `Source::read`).
fn read(
    ds: &Dataset,
    bands: std::ops::RangeInclusive<usize>,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    raw: bool,
) -> Result<Vec<f64>, String> {
    let mut out = Vec::with_capacity(bands.clone().count() * w * h);
    for b in bands {
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
        let threads = match spec.n_jobs() {
            n if n > 0 => n as usize,
            _ => std::thread::available_parallelism().map_or(4, |n| n.get()),
        };
        let open = |path: &str| Source::open(path, threads);
        let input = open(&spec.input)?;
        let input_ds = input.ds();
        let t = spec.years.len();
        if input_ds.raster_count() != t {
            return Err(format!("ValueError: input has {} bands but {t} dates were given", input_ds.raster_count()));
        }
        let (w, h) = input_ds.raster_size();
        let matching = |name: &str, src: Source| -> Result<Source, String> {
            let ds = src.ds();
            let (sw, sh) = ds.raster_size();
            if ds.raster_count() != t || sw != w || sh != h {
                return Err(format!(
                    "ValueError: {name} does not match the input ({} bands, {sw} x {sh})",
                    ds.raster_count()
                ));
            }
            Ok(src)
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
    fn read(&mut self, x0: usize, y0: usize, w: usize, h: usize) -> Result<Chunk, String> {
        let mut stack = self.input.read(x0, y0, w, h, true)?;
        let nodata = self.spec.nodata;
        for v in stack.iter_mut() {
            if nodata == Some(*v) || !v.is_finite() {
                *v = f64::NAN;
            }
        }
        let qa = self.qa.as_mut().map(|d| d.read(x0, y0, w, h, true)).transpose()?;
        if let Some(nd) = &mut self.nd {
            let b = nd.read(x0, y0, w, h, false)?;
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
            .iter_mut()
            .map(|(r, d)| d.read(x0, y0, w, h, false).map(|a| (r.clone(), a)))
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
        "phenology" => crate::phenology::chunk(p, chunk, ctx),
        "twdtw" => crate::twdtw::chunk(p, chunk, ctx),
        "som" => crate::som::chunk(p, chunk, ctx),
        "ccdc_synthetic" => crate::ccdc::synthetic_chunk(p, chunk, ctx),
        "bfast_lite" => crate::bfast::lite_chunk(p, chunk, ctx),
        "bfast_monitor" => crate::bfast::monitor_chunk(p, chunk, ctx),
        _ => Err(format!("{tool}: raster runs not available in this version")),
    }
}

/// Rows of the window a tool's fit samples (spread over it).
const FIT_ROWS: usize = 64;

/// Every pixel of `n_rows` full-width rows spread over the window, as one chunk of 1 x N (the bridge's
/// `sample_rows`): what a tool fits its model on before the chunks.
fn sample_rows(
    inputs: &mut Inputs,
    window: [usize; 4],
    n_rows: usize,
    progress: &dyn Fn(f64),
) -> Result<Chunk, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let k = n_rows.min(h).max(1);
    let mut rows: Vec<usize> = crate::numeric::linspace(y0 as f64, (y1 - 1) as f64, k)
        .into_iter()
        .map(|r| r.round_ties_even() as usize)
        .collect();
    rows.sort_unstable();
    rows.dedup();
    let chunks: Vec<Chunk> = rows
        .iter()
        .enumerate()
        .map(|(i, &r)| {
            let c = inputs.read(x0, r, w, 1);
            progress((i + 1) as f64 / rows.len() as f64);
            c
        })
        .collect::<Result<_, _>>()?;
    // [T][pixels] of each row, side by side
    let t = inputs.spec.years.len();
    let n = w * chunks.len();
    let join = |get: &dyn Fn(&Chunk) -> &[f64]| -> Vec<f64> {
        let mut out = vec![0.0; t * n];
        for (ci, c) in chunks.iter().enumerate() {
            let a = get(c);
            for i in 0..t {
                out[i * n + ci * w..i * n + (ci + 1) * w].copy_from_slice(&a[i * w..(i + 1) * w]);
            }
        }
        out
    };
    let stack = join(&|c: &Chunk| &c.stack);
    let bands = chunks[0].bands.keys().map(|r| (r.clone(), join(&|c: &Chunk| &c.bands[r]))).collect();
    let qa = chunks[0].qa.as_ref().map(|_| join(&|c: &Chunk| c.qa.as_deref().unwrap()));
    Ok(Chunk { t, h: 1, w: n, stack, bands, qa })
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
    if tool.input.as_deref() == Some("layers") {
        return run_layers(spec, tool, &p, progress, cancel);
    }
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut ctx = JobCtx { input: spec.context(), n_jobs: spec.n_jobs(), ..Default::default() };
    // a tool with a model of the whole window (a SOM) fits it first, on rows spread over it
    let base = if spec.tool == "som" {
        let sample = sample_rows(&mut inputs, [x0, y0, x1, y1], FIT_ROWS, &|f| {
            progress(0.05 * f, "reading a sample of the image")
        })?;
        progress(0.05, &format!("fitting on a sample of {} pixels", sample.pixels()));
        ctx.model = Some(crate::som::fit(&p, &sample, &ctx)?);
        0.1
    } else {
        0.0
    };
    let mut outs = Outputs::new(tool, &active_outputs(tool, &p), &dir, inputs.input.ds(), window, spec)?;
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
                base + (1.0 - base) * done as f64 / h as f64,
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

/// A run's computing time as the bridge's `estimate` measured it: a fixed cost per chunk plus a cost per pixel,
/// fitted on growing samples from the middle of the window (after a warm-up); the reading not included.
#[derive(Clone, Copy, Debug, Default)]
pub struct Estimate {
    pub sec_per_px: f64,
    pub sec_per_chunk: f64,
    pub chunk_cells: f64,
    /// a tool's fit (sampling the window and fitting its model: a SOM)
    pub sec_fixed: f64,
}

impl Estimate {
    /// Seconds for a window of `w` x `h` pixels (the C++'s estimateJobSeconds).
    pub fn seconds(&self, w: usize, h: usize) -> f64 {
        let rows = ((self.chunk_cells / w.max(1) as f64) as usize).clamp(8, 512);
        let chunks = h.div_ceil(rows);
        self.sec_fixed + chunks as f64 * self.sec_per_chunk + w as f64 * h as f64 * self.sec_per_px
    }
}

pub fn estimate(spec: &JobSpec) -> Result<Estimate, String> {
    let tool = manifest().tool(&spec.tool).ok_or_else(|| format!("KeyError: '{}'", spec.tool))?;
    let cells = tool.chunk_cells.unwrap_or(4_000_000) as f64;
    if tool.input.as_deref() == Some("layers") {
        return Ok(Estimate { sec_per_px: 5e-8, sec_per_chunk: 0.0, chunk_cells: cells, sec_fixed: 0.0 });
    }
    let p = Params::with_defaults(&spec.tool, &spec.params).unwrap_or_default();
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let [x0, y0, x1, y1] = spec.window.unwrap_or([0, 0, iw, ih]);
    let (w, h) = (x1 - x0, y1 - y0);
    let mut ctx = JobCtx { input: spec.context(), n_jobs: spec.n_jobs(), ..Default::default() };
    let mut fixed = 0.0;
    if spec.tool == "som" {
        // fewer rows than a run reads
        let t = std::time::Instant::now();
        let sample = sample_rows(&mut inputs, [x0, y0, x1, y1], 16, &|_| {})?;
        ctx.model = Some(crate::som::fit(&p, &sample, &ctx)?);
        fixed = t.elapsed().as_secs_f64();
    }
    let ctx = ctx;
    let mut sample = |n: usize| -> Result<(f64, f64), String> {
        let (sw, sh) = (n.min(w), n.min(h));
        let chunk = inputs.read(x0 + (w - sw) / 2, y0 + (h - sh) / 2, sw, sh)?;
        let t = std::time::Instant::now();
        run_chunk(&spec.tool, &p, &chunk, &ctx)?;
        Ok((t.elapsed().as_secs_f64(), (sw * sh) as f64))
    };
    sample(2)?; // warm-up
    let mut points = vec![sample(8)?];
    if points[0].0 < 1.5 {
        points.push(sample(16)?);
    }
    for side in [64, 128] {
        if points.len() < 2 || side > w.max(h) * 2 {
            break;
        }
        let ((t0, _), (t1, n1)) = (points[points.len() - 2], points[points.len() - 1]);
        let spent: f64 = points.iter().map(|p| p.0).sum();
        let flat = t1 < 1.5 * t0 && t1 < 1.5;
        let cheap = t1 * (side * side) as f64 / n1 < 1.0;
        if spent > 4.0 || !(flat || cheap) {
            break;
        }
        points.push(sample(side)?);
    }
    // least squares t = a + b n (b >= 0)
    let (per_chunk, per_px) = if points.len() == 1 {
        (0.0, points[0].0 / points[0].1.max(1.0))
    } else {
        let k = points.len() as f64;
        let (mn, mt) = (points.iter().map(|p| p.1).sum::<f64>() / k, points.iter().map(|p| p.0).sum::<f64>() / k);
        let sxx: f64 = points.iter().map(|p| (p.1 - mn).powi(2)).sum();
        let b =
            if sxx > 0.0 { (points.iter().map(|p| (p.1 - mn) * (p.0 - mt)).sum::<f64>() / sxx).max(0.0) } else { 0.0 };
        ((mt - b * mn).max(0.0), b)
    };
    Ok(Estimate { sec_per_px: per_px, sec_per_chunk: per_chunk, chunk_cells: cells, sec_fixed: fixed })
}

/// A map on the grid of `reference` (nearest neighbour, NaN where it has no value): a WarpedVRT's work.
fn warped_to(src: &Dataset, reference: &Dataset) -> Result<Dataset, String> {
    let (w, h) = reference.raster_size();
    let driver = DriverManager::get_driver_by_name("MEM").map_err(|e| e.to_string())?;
    let mut dst = driver.create_with_band_type::<f32, _>("", w, h, 1).map_err(|e| e.to_string())?;
    if let Ok(g) = reference.geo_transform() {
        dst.set_geo_transform(&g).map_err(|e| e.to_string())?;
    }
    dst.set_projection(&reference.projection()).map_err(|e| e.to_string())?;
    {
        let mut b = dst.rasterband(1).map_err(|e| e.to_string())?;
        b.set_no_data_value(Some(f64::NAN)).map_err(|e| e.to_string())?;
        b.fill(f64::NAN, None).map_err(|e| e.to_string())?;
    }
    let rv = unsafe {
        gdal_sys::GDALReprojectImage(
            src.c_dataset(),
            std::ptr::null(),
            dst.c_dataset(),
            std::ptr::null(),
            gdal_sys::GDALResampleAlg::GRA_NearestNeighbour,
            0.0,
            0.0,
            None,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rv != gdal_sys::CPLErr::CE_None {
        return Err("could not put a map on the grid of the first one".into());
    }
    Ok(dst)
}

/// A tool over maps Janus shows (its "layers" parameter) instead of the series (the bridge's `run_layers`): the
/// outputs on the first map's grid, the others put on it.
fn run_layers(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let lp = tool.params.iter().find(|q| q.kind == "layers").ok_or("no layers parameter")?;
    let paths: Vec<String> = p
        .0
        .get(&lp.id)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|m| m["path"].as_str().filter(|s| !s.is_empty()).map(str::to_string)).collect())
        .unwrap_or_default();
    if paths.len() < 2 {
        return Err(format!("ValueError: choose at least two maps in '{}'", lp.label));
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let reference = open(&paths[0])?;
    let others: Vec<Dataset> =
        paths[1..].iter().map(|q| open(q).and_then(|d| warped_to(&d, &reference))).collect::<Result<_, _>>()?;
    let (w, h) = reference.raster_size();
    let mut outs = Outputs::new(tool, &active_outputs(tool, p), &dir, &reference, [0, 0, w, h], spec)?;
    let rows = rows_per_chunk(tool, w);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let first = |d: &Dataset| -> Result<Vec<f64>, String> {
            let band = d.rasterband(1).map_err(|e| e.to_string())?;
            let buf: Buffer<f64> = band.read_as((0, r as isize), (w, hh), (w, hh), None).map_err(|e| e.to_string())?;
            let nd = band.no_data_value();
            Ok(buf.data().iter().map(|&v| if !v.is_finite() || nd == Some(v) { f64::NAN } else { v }).collect())
        };
        let mut maps = vec![first(&reference)?];
        for o in &others {
            maps.push(first(o)?);
        }
        outs.write(&crate::agreement::chunk(p, &maps), r, hh)?;
        r += hh;
        progress(r as f64 / h as f64, &format!("rows {r}/{h}"));
    }
    let outputs = outs.describe(p, None);
    drop(outs);
    Ok(JobResult { outputs, window: spec.window.unwrap_or([0, 0, w, h]) })
}
