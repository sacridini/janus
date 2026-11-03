//! The calculator's VRTs read by Janus itself: their sources read straight from their files (a GDAL dataset of each
//! file per thread, kept), the band's values from them by the same evaluation as the pixel function's, without
//! GDAL's VRT in between. GDAL's VRT reads every source through a pool of datasets under a lock of the process
//! (reading gained little beyond four threads) and parses its XML on the way; here each thread reads its own.
//!
//! The values are GDAL's own, bit for bit: what GDAL does between the VRT and its sources is done here the same
//! way (the sources' windows, `VRTSimpleSource::GetSrcDstWindow`; their no data and scale, `VRTComplexSource`; a
//! window read through the VRT's blocks and resampled by the nearest neighbour, `GDALRasterBand::IRasterIO`, when the
//! VRT was opened with GDAL_FORCE_CACHING), the files themselves read by GDAL with the same arguments as the VRT
//! would read them. The files written stay what they were (VRTs GDAL and QGIS read); only Janus reads them so.
//!
//! A read that takes pixels of the grid (through the blocks, or a VRT whose every value is its pixel's own) is
//! computed in parts of whole blocks of the files, each block read by one thread (`threads`): the values do not
//! depend on the parts. Each thread keeps its datasets open (`close_files_under` before files are written again).
//!
//! What it does not cover is read by GDAL as before (`open` gives None, a read `Miss::NotCovered`): a VRT of another
//! form than the calculator's, a source on the network, a source of complex numbers, a resampling other than the
//! calculator's.
//!
//! One difference, on purpose: a band of one source whose file has overviews, read at a smaller size, GDAL reads
//! through the VRT's virtual overviews, which copy the source and leave the pixel function out (`x * 2` showed `x`
//! zoomed out). Here it is computed as any band, its source read through the file's overviews.

#![allow(non_upper_case_globals)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use gdal::Dataset;
use gdal_sys::GDALDataType::{self, *};
use gdal_sys::GDALRIOResampleAlg::{self, *};

use super::vrt::{Args, PIXEL_FUNCTION, PIXEL_FUNCTION_ELEMENT, evaluate};

/// GDAL's window not set (VRTSimpleSource's UNINIT_WINDOW).
const UNSET: f64 = -1.0;

/// Why a read was not made here.
#[derive(Debug)]
pub enum Miss {
    /// GDAL reads it (see the module's notes)
    NotCovered,
    /// a file could not be read
    Failed(String),
}

impl From<String> for Miss {
    fn from(e: String) -> Self {
        Miss::Failed(e)
    }
}

/// How GDAL would read the VRT: opened as is (`Direct`, the derived band computes the window asked), or with
/// GDAL_FORCE_CACHING (`Cached`, the window resampled from the band's blocks, each computed whole at full
/// resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    Direct,
    Cached,
}

// ---------------------------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------------------------

static OWN: AtomicBool = AtomicBool::new(true);
static THREADS: AtomicUsize = AtomicUsize::new(0);

/// Whether the calculator's VRTs are read here (true by default; false: through GDAL, as before).
pub fn set_enabled(on: bool) {
    OWN.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    OWN.load(Ordering::Relaxed)
}

/// The threads reading a calculator's band at once (all reads together): the processors, eight at most, unless set
/// (the program's threads setting).
pub fn set_threads(n: usize) {
    THREADS.store(n.max(1), Ordering::Relaxed);
}

pub fn threads() -> usize {
    match THREADS.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(1, |n| n.get()).min(8),
        n => n,
    }
}

/// The threads of the reads (as many as `threads`, made again when it changes); each keeps its files open.
pub(crate) fn pool() -> Arc<rayon::ThreadPool> {
    static POOL: Mutex<Option<(usize, Arc<rayon::ThreadPool>)>> = Mutex::new(None);
    let n = threads();
    let mut p = POOL.lock().unwrap();
    if let Some((k, pool)) = p.as_ref()
        && *k == n
    {
        return pool.clone();
    }
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("calculator reader {i}"))
            .build()
            .expect("the calculator's reader threads"),
    );
    *p = Some((n, pool.clone()));
    pool
}

// ---------------------------------------------------------------------------------------------------------------
// The VRT
// ---------------------------------------------------------------------------------------------------------------

/// How a source's values become the band's: as read (SimpleSource), or with its no data and scale
/// (ComplexSource).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Simple,
    Complex { scale: Option<(f64, f64)>, nodata: Option<f64> },
}

#[derive(Debug)]
struct Src {
    path: String,
    band: usize,
    kind: Kind,
    /// SrcRect and DstRect (x, y, w, h; UNSET without one)
    src_win: [f64; 4],
    dst_win: [f64; 4],
    resampling: Option<GDALRIOResampleAlg::Type>,
    /// a calculator's VRT read here too
    nested: Option<Arc<CalcVrt>>,
    /// a file's size and time when the VRT was read: a file written again under its name is another file
    stamp: u64,
    /// its band's size and type, once read
    info: OnceLock<(usize, usize, GDALDataType::Type)>,
}

#[derive(Debug)]
struct DBand {
    args: Vec<(String, String)>,
    sources: Vec<Src>,
}

/// A calculator's VRT as read here.
#[derive(Debug)]
pub struct CalcVrt {
    pub path: String,
    pub width: usize,
    pub height: usize,
    /// its bands' blocks
    pub block: (usize, usize),
    bands: Vec<DBand>,
    /// every value a pixel's own whatever window is read (no source placed by a window, none another kind of VRT,
    /// each of the grid's size): a window is computed at its pixels alone
    pointwise: OnceLock<bool>,
    /// a file it reads (or a VRT among them) has overviews: read smaller, GDAL reads them
    overviews: OnceLock<bool>,
    /// the blocks of the files it reads (the first's): what a thread reads whole
    grain: OnceLock<(usize, usize)>,
}

/// A file's size and modification time.
fn stamp(path: &str) -> u64 {
    std::fs::metadata(path).ok().map_or(0, |m| {
        let t =
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
        (t as u64).wrapping_mul(31).wrapping_add(m.len())
    })
}

/// The calculator's VRT at `path`, read once (again when written again); None: not one read here.
pub fn open(path: &str) -> Option<Arc<CalcVrt>> {
    if !enabled() {
        return None;
    }
    open_at(path, 0)
}

fn open_at(path: &str, depth: usize) -> Option<Arc<CalcVrt>> {
    type Cache = HashMap<String, (u64, Option<Arc<CalcVrt>>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    if depth > 4 || !path.to_ascii_lowercase().ends_with(".vrt") || crate::is_remote_path(path) {
        return None;
    }
    let st = stamp(path);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((s, v)) = cache.lock().unwrap().get(path)
        && *s == st
    {
        return v.clone();
    }
    let v = std::fs::read_to_string(path).ok().and_then(|t| parse(path, &t, depth)).map(Arc::new);
    let mut c = cache.lock().unwrap();
    if c.len() > 4096 {
        c.clear();
    }
    c.insert(path.to_string(), (st, v.clone()));
    v
}

// --- a small reader of the calculator's XML (vrt::vrt_xml writes it) -------------------------------------------

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.0 == k).map(|a| a.1.as_str())
    }
}

fn unescape(s: &str) -> Option<String> {
    if !s.contains('&') {
        return Some(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find(';')?;
        out.push(match &tail[..=end] {
            "&amp;" => '&',
            "&lt;" => '<',
            "&gt;" => '>',
            "&quot;" => '"',
            "&apos;" => '\'',
            _ => return None,
        });
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// The elements of an XML text (no comments, declarations nor CDATA: None).
fn parse_xml(text: &str) -> Option<Node> {
    let mut stack: Vec<Node> = vec![Node::default()];
    let mut rest = text;
    loop {
        let Some(lt) = rest.find('<') else {
            if !rest.trim().is_empty() {
                return None;
            }
            break;
        };
        let t = unescape(&rest[..lt])?;
        stack.last_mut()?.text.push_str(&t);
        rest = &rest[lt + 1..];
        let gt = rest.find('>')?;
        let tag = &rest[..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let node = stack.pop()?;
            if node.name != name.trim() || stack.is_empty() {
                return None;
            }
            stack.last_mut()?.children.push(node);
            continue;
        }
        if tag.starts_with('!') || tag.starts_with('?') {
            return None;
        }
        let (tag, closed) = match tag.strip_suffix('/') {
            Some(t) => (t, true),
            None => (tag, false),
        };
        let tag = tag.trim();
        let (name, mut a) = match tag.find(char::is_whitespace) {
            Some(i) => (&tag[..i], tag[i..].trim()),
            None => (tag, ""),
        };
        let mut node = Node { name: name.to_string(), ..Default::default() };
        while !a.is_empty() {
            let eq = a.find('=')?;
            let k = a[..eq].trim().to_string();
            let v = a[eq + 1..].trim_start();
            let q = v.chars().next()?;
            if q != '"' && q != '\'' {
                return None;
            }
            let end = v[1..].find(q)? + 1;
            node.attrs.push((k, unescape(&v[1..end])?));
            a = v[end + 1..].trim_start();
        }
        if closed {
            stack.last_mut()?.children.push(node);
        } else {
            stack.push(node);
        }
    }
    let mut root = stack.pop()?;
    if !stack.is_empty() || root.children.len() != 1 {
        return None;
    }
    root.children.pop()
}

fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
    s.trim().parse().ok()
}

/// A number as GDAL reads it in a VRT (CPLAtof: also "nan", "inf").
fn atof(s: &str) -> Option<f64> {
    let t = s.trim();
    match t.to_ascii_lowercase().as_str() {
        "nan" => Some(f64::NAN),
        "inf" | "+inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        _ => t.parse().ok(),
    }
}

fn rect(n: &Node) -> Option<[f64; 4]> {
    Some([atof(n.attr("xOff")?)?, atof(n.attr("yOff")?)?, atof(n.attr("xSize")?)?, atof(n.attr("ySize")?)?])
}

fn parse(path: &str, text: &str, depth: usize) -> Option<CalcVrt> {
    if !text.contains(PIXEL_FUNCTION_ELEMENT) {
        return None;
    }
    let root = parse_xml(text)?;
    if root.name != "VRTDataset" || root.attrs.len() != 2 {
        return None;
    }
    let (w, h): (usize, usize) = (num(root.attr("rasterXSize")?)?, num(root.attr("rasterYSize")?)?);
    let mut bands = Vec::new();
    let mut block = None;
    for c in &root.children {
        match c.name.as_str() {
            "SRS" | "GeoTransform" => {}
            "VRTRasterBand" => {
                if c.attr("dataType")? != "Float32" || c.attr("subClass")? != "VRTDerivedRasterBand" {
                    return None;
                }
                let b: usize = num(c.attr("band")?)?;
                let bs: (usize, usize) = (num(c.attr("blockXSize")?)?, num(c.attr("blockYSize")?)?);
                if b != bands.len() + 1
                    || bs.0 == 0
                    || bs.1 == 0
                    || block.is_some_and(|k| k != bs)
                    || c.attrs.len() != 5
                {
                    return None;
                }
                block = Some(bs);
                bands.push(parse_band(c, depth)?);
            }
            _ => return None,
        }
    }
    Some(CalcVrt {
        path: path.to_string(),
        width: w,
        height: h,
        block: block?,
        bands,
        pointwise: OnceLock::new(),
        overviews: OnceLock::new(),
        grain: OnceLock::new(),
    })
}

fn parse_band(c: &Node, depth: usize) -> Option<DBand> {
    let mut band = DBand { args: Vec::new(), sources: Vec::new() };
    let (mut func, mut transfer, mut nodata) = (false, false, false);
    for e in &c.children {
        match e.name.as_str() {
            "Description" => {}
            "NoDataValue" => nodata = atof(&e.text)?.is_nan(),
            "PixelFunctionType" => func = e.text.trim() == PIXEL_FUNCTION,
            "PixelFunctionArguments" => band.args = e.attrs.clone(),
            "SourceTransferType" => transfer = e.text.trim() == "Float32",
            "SimpleSource" | "ComplexSource" => band.sources.push(parse_source(e, depth)?),
            _ => return None,
        }
    }
    (func && transfer && nodata).then_some(band)
}

fn parse_source(e: &Node, depth: usize) -> Option<Src> {
    let complex = e.name == "ComplexSource";
    let resampling = match e.attrs.as_slice() {
        [] => None,
        [(k, v)] if k == "resampling" => Some(match v.to_ascii_lowercase().as_str() {
            r if r.starts_with("near") => GRIORA_NearestNeighbour,
            "bilinear" => GRIORA_Bilinear,
            "cubic" => GRIORA_Cubic,
            r if r.starts_with("aver") => GRIORA_Average,
            r if r.starts_with("mode") => GRIORA_Mode,
            _ => return None,
        }),
        _ => return None,
    };
    let (mut path, mut band) = (None, None);
    let (mut src_win, mut dst_win) = ([UNSET; 4], [UNSET; 4]);
    let (mut off, mut ratio, mut nodata) = (None, None, None);
    for c in &e.children {
        match c.name.as_str() {
            "SourceFilename" => {
                if c.attrs.as_slice() != [("relativeToVRT".to_string(), "0".to_string())] {
                    return None;
                }
                path = Some(c.text.trim().to_string());
            }
            "SourceBand" => band = num::<usize>(&c.text),
            "SrcRect" => src_win = rect(c)?,
            "DstRect" => dst_win = rect(c)?,
            "ScaleOffset" if complex => off = Some(atof(&c.text)?),
            "ScaleRatio" if complex => ratio = Some(atof(&c.text)?),
            "NODATA" if complex => nodata = Some(atof(&c.text)?),
            _ => return None,
        }
    }
    let path = path?;
    if crate::is_remote_path(&path) || path.is_empty() {
        return None;
    }
    // a calculator's VRT read here too; any other VRT (a warp's) read by GDAL as a file
    let nested = if path.to_ascii_lowercase().ends_with(".vrt") { open_at(&path, depth + 1) } else { None };
    let scale = (off.is_some() || ratio.is_some()).then(|| (off.unwrap_or(0.0), ratio.unwrap_or(1.0)));
    let kind = if complex { Kind::Complex { scale, nodata } } else { Kind::Simple };
    // windows set in part: GDAL checks each value against UNSET, as here
    let stamp = stamp(&path);
    Some(Src {
        path,
        band: band.filter(|&b| b >= 1)?,
        kind,
        src_win,
        dst_win,
        resampling,
        nested,
        stamp,
        info: OnceLock::new(),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// The files, a dataset of each per thread
// ---------------------------------------------------------------------------------------------------------------

struct Handles {
    open: HashMap<(String, u64), (Dataset, u64)>,
    clock: u64,
}

thread_local! {
    static HANDLES: RefCell<Handles> = RefCell::new(Handles { open: HashMap::new(), clock: 0 });
}

/// The files a thread keeps open: GDAL's pool of the files VRTs read, shared by the threads.
fn handles_kept() -> usize {
    let pool = gdal::config::get_config_option("GDAL_MAX_DATASET_POOL_SIZE", "450")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(450);
    (pool / threads()).max(64)
}

/// Closes this thread's files.
pub fn drop_thread_handles() {
    HANDLES.with_borrow_mut(|h| h.open.clear());
}

/// Closes the files of every thread of the reads, and this thread's.
pub fn close_files() {
    drop_thread_handles();
    pool().broadcast(|_| drop_thread_handles());
}

/// Closes the files under `dir` the threads of the reads and this thread keep open: before they are written again
/// or removed (Windows keeps an open file).
pub fn close_files_under(dir: &std::path::Path) {
    let d = dir.to_string_lossy().into_owned();
    let close = || HANDLES.with_borrow_mut(|h| h.open.retain(|(p, _), _| !p.starts_with(&d)));
    close();
    pool().broadcast(|_| close());
}

/// `f` over this thread's dataset of a source's file (opened as GDAL's VRT opens it: no GDAL_FORCE_CACHING).
fn with_source<R>(s: &Src, f: impl FnOnce(&Dataset) -> Result<R, Miss>) -> Result<R, Miss> {
    HANDLES.with_borrow_mut(|h| {
        h.clock += 1;
        let now = h.clock;
        let key = (s.path.clone(), s.stamp);
        if !h.open.contains_key(&key) {
            let cap = handles_kept();
            while h.open.len() >= cap {
                let Some(old) = h.open.iter().min_by_key(|e| e.1.1).map(|e| e.0.clone()) else { break };
                h.open.remove(&old);
            }
            let key_c = "GDAL_FORCE_CACHING";
            let was = gdal::config::get_thread_local_config_option(key_c, "").unwrap_or_default();
            let _ = gdal::config::set_thread_local_config_option(key_c, "NO");
            let ds = crate::open_raster(&s.path);
            let _ = if was.is_empty() {
                gdal::config::clear_thread_local_config_option(key_c)
            } else {
                gdal::config::set_thread_local_config_option(key_c, &was)
            };
            let ds = ds.map_err(|e| Miss::Failed(format!("{}: {e}", s.path)))?;
            h.open.insert(key.clone(), (ds, now));
        }
        let e = h.open.get_mut(&key).expect("opened");
        e.1 = now;
        f(&e.0)
    })
}

/// A source band's size and type (kept: the files' bands do not change under a stamp).
fn band_info(s: &Src) -> Result<(usize, usize, GDALDataType::Type), Miss> {
    if let Some(i) = s.info.get() {
        return Ok(*i);
    }
    if let Some(n) = &s.nested {
        if s.band > n.bands.len() {
            return Err(Miss::Failed(format!("{}: no band {}", s.path, s.band)));
        }
        return Ok(*s.info.get_or_init(|| (n.width, n.height, GDT_Float32)));
    }
    let i = with_source(s, |ds| {
        let b = ds.rasterband(s.band).map_err(|e| Miss::Failed(format!("{}: {e}", s.path)))?;
        let (w, h) = b.size();
        // SAFETY: a live band.
        Ok((w, h, unsafe { gdal_sys::GDALGetRasterDataType(b.c_rasterband()) }))
    })?;
    Ok(*s.info.get_or_init(|| i))
}

/// Whether a source's band has overviews (GDAL looks for them beside the file: once a dataset, not on every
/// thread's).
fn has_overviews(s: &Src) -> bool {
    with_source(s, |ds| {
        let b = ds.rasterband(s.band).map_err(|e| Miss::Failed(format!("{}: {e}", s.path)))?;
        // SAFETY: a live band.
        Ok(unsafe { gdal_sys::GDALGetOverviewCount(b.c_rasterband()) } > 0)
    })
    .unwrap_or(true)
}

// ---------------------------------------------------------------------------------------------------------------
// Values: GDAL's conversions
// ---------------------------------------------------------------------------------------------------------------

/// Values read from a file in the type GDAL's VRT asks for.
enum Typed {
    U8(Vec<u8>),
    I16(Vec<i16>),
    U16(Vec<u16>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

impl Typed {
    fn new(t: GDALDataType::Type, n: usize) -> Typed {
        match t {
            GDT_Byte => Typed::U8(vec![0; n]),
            GDT_Int16 => Typed::I16(vec![0; n]),
            GDT_UInt16 => Typed::U16(vec![0; n]),
            GDT_Float64 => Typed::F64(vec![0.0; n]),
            _ => Typed::F32(vec![0.0; n]),
        }
    }

    fn gdal_type(&self) -> GDALDataType::Type {
        match self {
            Typed::U8(_) => GDT_Byte,
            Typed::I16(_) => GDT_Int16,
            Typed::U16(_) => GDT_UInt16,
            Typed::F32(_) => GDT_Float32,
            Typed::F64(_) => GDT_Float64,
        }
    }

    fn ptr(&mut self) -> *mut std::ffi::c_void {
        match self {
            Typed::U8(v) => v.as_mut_ptr().cast(),
            Typed::I16(v) => v.as_mut_ptr().cast(),
            Typed::U16(v) => v.as_mut_ptr().cast(),
            Typed::F32(v) => v.as_mut_ptr().cast(),
            Typed::F64(v) => v.as_mut_ptr().cast(),
        }
    }

    /// The values at `idx`, in that order.
    fn pick(&self, idx: &[usize]) -> Typed {
        match self {
            Typed::U8(v) => Typed::U8(idx.iter().map(|&i| v[i]).collect()),
            Typed::I16(v) => Typed::I16(idx.iter().map(|&i| v[i]).collect()),
            Typed::U16(v) => Typed::U16(idx.iter().map(|&i| v[i]).collect()),
            Typed::F32(v) => Typed::F32(idx.iter().map(|&i| v[i]).collect()),
            Typed::F64(v) => Typed::F64(idx.iter().map(|&i| v[i]).collect()),
        }
    }
}

fn is_complex(t: GDALDataType::Type) -> bool {
    // SAFETY: a pure function of the value.
    unsafe { gdal_sys::GDALDataTypeIsComplex(t) != 0 }
}

/// GDALIsValueInRange for the integer types of VRTComplexSource's fast path.
fn in_range(v: f64, t: GDALDataType::Type) -> bool {
    let (lo, hi) = match t {
        GDT_Byte => (0.0, 255.0),
        GDT_Int16 => (-32768.0, 32767.0),
        _ => (0.0, 65535.0),
    };
    v >= lo && v <= hi
}

/// GDALAdjustNoDataCloseToFloatMax.
fn adjust_float_max(v: f64) -> f64 {
    let m = f64::from(f32::MAX);
    if (v - -m).abs() < 1e-10 * m {
        -m
    } else if (v - m).abs() < 1e-10 * m {
        m
    } else {
        v
    }
}

/// How a source's values are read and turned into the band's (VRTSimpleSource::RasterIO,
/// VRTComplexSource::RasterIO, RasterIOProcessNoData, RasterIOInternal).
#[derive(Clone, Copy, Debug)]
enum Conv {
    /// read as Float32, as they are
    AsIs,
    /// read in the file's type (Byte, Int16, UInt16), those equal to the no data skipped
    NoData(GDALDataType::Type, f64),
    /// read as Float32 or Float64, no data skipped, scaled: (working type Float64, no data, scale)
    Work { f64: bool, nodata: Option<f64>, scale: Option<(f64, f64)>, adjust: bool },
}

fn conversion(kind: Kind, t: GDALDataType::Type) -> Conv {
    match kind {
        Kind::Simple => Conv::AsIs,
        Kind::Complex { scale: None, nodata: Some(nd) } if matches!(t, GDT_Byte | GDT_Int16 | GDT_UInt16) => {
            if in_range(nd, t) { Conv::NoData(t, nd) } else { Conv::AsIs }
        }
        Kind::Complex { scale, nodata } => Conv::Work {
            f64: matches!(t, GDT_Int32 | GDT_UInt32 | GDT_Int64 | GDT_UInt64),
            nodata,
            scale,
            adjust: t == GDT_Float32,
        },
    }
}

impl Conv {
    /// The type to read the file in.
    fn read_type(self) -> GDALDataType::Type {
        match self {
            Conv::AsIs => GDT_Float32,
            Conv::NoData(t, _) => t,
            Conv::Work { f64: true, .. } => GDT_Float64,
            Conv::Work { .. } => GDT_Float32,
        }
    }

    /// The band's values from those read, `dst[i]` for `src[i]` (left as they are where skipped: NaN).
    fn apply(self, src: &Typed, dst: &mut [f32]) {
        match (self, src) {
            (Conv::AsIs, Typed::F32(v)) => dst.copy_from_slice(&v[..dst.len()]),
            (Conv::NoData(_, nd), Typed::U8(v)) => {
                let k = nd as u8;
                v.iter().zip(dst.iter_mut()).filter(|p| *p.0 != k).for_each(|(&x, o)| *o = f32::from(x));
            }
            (Conv::NoData(_, nd), Typed::I16(v)) => {
                let k = nd as i16;
                v.iter().zip(dst.iter_mut()).filter(|p| *p.0 != k).for_each(|(&x, o)| *o = f32::from(x));
            }
            (Conv::NoData(_, nd), Typed::U16(v)) => {
                let k = nd as u16;
                v.iter().zip(dst.iter_mut()).filter(|p| *p.0 != k).for_each(|(&x, o)| *o = f32::from(x));
            }
            (Conv::Work { nodata, scale, adjust, .. }, Typed::F32(v)) => {
                let nd = nodata.map(|n| if adjust { adjust_float_max(n) } else { n });
                let nan_nd = nd.is_some_and(f64::is_nan);
                // GDALIsValueInRange<float>
                let real_nd = nd.filter(|n| !n.is_nan() && (n.is_infinite() || n.abs() <= f64::from(f32::MAX)));
                let k = real_nd.map(|n| n as f32);
                for (&x, o) in v.iter().zip(dst.iter_mut()) {
                    if nan_nd && x.is_nan() {
                        continue;
                    }
                    // ARE_REAL_EQUAL(float, float)
                    if let Some(k) = k
                        && (x == k || (x - k).abs() < f32::EPSILON * (x + k).abs() * 2.0)
                    {
                        continue;
                    }
                    *o = match scale {
                        Some((off, ratio)) => (f64::from(x) * ratio + off) as f32,
                        None => x,
                    };
                }
            }
            (Conv::Work { nodata, scale, .. }, Typed::F64(v)) => {
                let nan_nd = nodata.is_some_and(f64::is_nan);
                let k = nodata.filter(|n| !n.is_nan());
                for (&x, o) in v.iter().zip(dst.iter_mut()) {
                    if nan_nd && x.is_nan() {
                        continue;
                    }
                    // ARE_REAL_EQUAL(double, double), float's epsilon
                    if let Some(k) = k
                        && (x == k || (x - k).abs() < f64::from(f32::EPSILON) * (x + k).abs() * 2.0)
                    {
                        continue;
                    }
                    let r = match scale {
                        Some((off, ratio)) => x * ratio + off,
                        None => x,
                    };
                    // GDALCopyWord(double, float)
                    *o = if r > f64::from(f32::MAX) {
                        f32::INFINITY
                    } else if r < -f64::from(f32::MAX) {
                        f32::NEG_INFINITY
                    } else {
                        r as f32
                    };
                }
            }
            _ => unreachable!("read in the type asked"),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The sources' windows (VRTSimpleSource::GetSrcDstWindow)
// ---------------------------------------------------------------------------------------------------------------

/// What a source reads for a request: the window in the file (floating and whole pixels) and where it goes in the
/// buffer.
#[derive(Clone, Copy, Debug)]
struct SrcWin {
    df: [f64; 4],
    req: [i32; 4],
    out: [i32; 4],
}

fn round_if_close_to_int(v: f64) -> f64 {
    let c = (v + 0.5).floor();
    if (v - c).abs() < 1e-3 { c } else { v }
}

/// VRTSimpleSource::GetSrcDstWindow for a request (dfXOff, dfYOff, dfXSize, dfYSize) into (nBufXSize, nBufYSize),
/// the source band of `size`; None: nothing to read.
fn src_dst_window(s: &Src, win: [f64; 4], (bw, bh): (i32, i32), size: (usize, usize)) -> Option<SrcWin> {
    let [sxo, syo, sxs, sys] = s.src_win;
    let [dxo, dyo, dxs, dys] = s.dst_win;
    let [x, y, xs, ys] = win;
    if sxs == 0.0 || sys == 0.0 || dxs == 0.0 || dys == 0.0 {
        return None;
    }
    let dst_set = s.dst_win != [UNSET; 4];
    if dst_set && (x >= dxo + dxs || y >= dyo + dys || x + xs <= dxo || y + ys <= dyo) {
        return None;
    }
    let mut out = [0, 0, bw, bh];
    let (mut mod_x, mut mod_y) = (false, false);
    let (mut rx, mut ry, mut rxs, mut rys) = (x, y, xs, ys);
    if dst_set {
        if rx < dxo {
            rxs = rxs + rx - dxo;
            rx = dxo;
            mod_x = true;
        }
        if ry < dyo {
            rys = rys + ry - dyo;
            ry = dyo;
            mod_y = true;
        }
        if rx + rxs > dxo + dxs {
            rxs = dxo + dxs - rx;
            mod_x = true;
        }
        if ry + rys > dyo + dys {
            rys = dyo + dys - ry;
            mod_y = true;
        }
    }
    let scale_x = sxs / dxs;
    let scale_y = sys / dys;
    let mut df = [(rx - dxo) * scale_x + sxo, (ry - dyo) * scale_y + syo, rxs * scale_x, rys * scale_y];
    let max = f64::from(i32::MAX);
    if !df.iter().all(|v| v.is_finite()) || df[0] > max || df[1] > max || df[2] < 0.0 || df[3] < 0.0 {
        return None;
    }
    if df[0] < 0.0 {
        df[2] += df[0];
        df[0] = 0.0;
        mod_x = true;
    }
    if df[1] < 0.0 {
        df[3] += df[1];
        df[1] = 0.0;
        mod_y = true;
    }
    let mut req = [df[0].floor() as i32, df[1].floor() as i32, 0, 0];
    const EPS: f64 = 1e-3;
    if df[0] - f64::from(req[0]) > 1.0 - EPS {
        req[0] += 1;
        df[0] = f64::from(req[0]);
    }
    if df[1] - f64::from(req[1]) > 1.0 - EPS {
        req[1] += 1;
        df[1] = f64::from(req[1]);
    }
    req[2] = if df[2] > max { i32::MAX } else { (df[2] + 0.5).floor() as i32 };
    req[3] = if df[3] > max { i32::MAX } else { (df[3] + 0.5).floor() as i32 };
    if req[2] == 0 {
        req[2] = 1;
    }
    if req[3] == 0 {
        req[3] = 1;
    }
    let (bxs, bys) = (size.0 as i32, size.1 as i32);
    if req[2] > i32::MAX - req[0] || req[0] + req[2] > bxs {
        req[2] = bxs - req[0];
        mod_x = true;
    }
    if df[0] + df[2] > f64::from(bxs) {
        df[2] = f64::from(bxs) - df[0];
        mod_x = true;
    }
    if req[3] > i32::MAX - req[1] || req[1] + req[3] > bys {
        req[3] = bys - req[1];
        mod_y = true;
    }
    if df[1] + df[3] > f64::from(bys) {
        df[3] = f64::from(bys) - df[1];
        mod_y = true;
    }
    if req[0] >= bxs || req[1] >= bys || req[2] <= 0 || req[3] <= 0 {
        return None;
    }
    if mod_x || mod_y {
        // SrcToDst
        let to_dst = |sx: f64, sy: f64| (((sx - sxo) / sxs) * dxs + dxo, ((sy - syo) / sys) * dys + dyo);
        let (ulx, uly) = to_dst(df[0], df[1]);
        let (lrx, lry) = to_dst(df[0] + df[2], df[1] + df[3]);
        if mod_x {
            let k = f64::from(bw) / xs;
            let ox = (ulx - x) * k;
            out[0] = if ox <= 0.0 {
                0
            } else if ox > max {
                i32::MAX
            } else {
                (ox + EPS) as i32
            };
            let d = (ox - f64::from(out[0])) / k / dxs * sxs;
            df[0] -= d;
            df[2] = (df[2] + d).min(max);
            let mut right = (lrx - x) * k;
            if right < ox {
                return None;
            }
            if right > max {
                right = max;
            }
            let n_right = (right - EPS).ceil() as i32;
            out[2] = n_right - out[0];
            if out[2] > i32::MAX - out[0] || out[0] + out[2] > bw {
                out[2] = bw - out[0];
            }
            let d = (f64::from(n_right) - right) / k / dxs * sxs;
            df[2] = (df[2] + d).min(max);
        }
        if mod_y {
            let k = f64::from(bh) / ys;
            let oy = (uly - y) * k;
            out[1] = if oy <= 0.0 {
                0
            } else if oy > max {
                i32::MAX
            } else {
                (oy + EPS) as i32
            };
            let d = (oy - f64::from(out[1])) / k / dys * sys;
            df[1] -= d;
            df[3] = (df[3] + d).min(max);
            let mut top = (lry - y) * k;
            if top < oy {
                return None;
            }
            if top > max {
                top = max;
            }
            let n_top = (top - EPS).ceil() as i32;
            out[3] = n_top - out[1];
            if out[3] > i32::MAX - out[1] || out[1] + out[3] > bh {
                out[3] = bh - out[1];
            }
            let d = (f64::from(n_top) - top) / k / dys * sys;
            df[3] = (df[3] + d).min(max);
        }
        if out[2] < 1 || out[3] < 1 {
            return None;
        }
    }
    Some(SrcWin { df: df.map(round_if_close_to_int), req, out })
}

/// GDAL's last error message.
fn last_error() -> String {
    // SAFETY: GDAL's thread-local message, copied at once.
    unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()).to_string_lossy().into_owned() }
}

/// GDALRasterIOEx on a file's band.
fn raster_io(
    b: &gdal::raster::RasterBand,
    req: [i32; 4],
    buf: &mut Typed,
    size: (i32, i32),
    df: Option<[f64; 4]>,
    alg: GDALRIOResampleAlg::Type,
) -> bool {
    let f = df.unwrap_or([0.0; 4]);
    let mut extra = gdal_sys::GDALRasterIOExtraArg {
        nVersion: 1,
        eResampleAlg: alg,
        pfnProgress: None,
        pProgressData: std::ptr::null_mut(),
        bFloatingPointWindowValidity: i32::from(df.is_some()),
        dfXOff: f[0],
        dfYOff: f[1],
        dfXSize: f[2],
        dfYSize: f[3],
    };
    let t = buf.gdal_type();
    // SAFETY: a live band, a buffer of size.0 × size.1 values of its type, the extra arguments alive for the call.
    let err = unsafe {
        gdal_sys::GDALRasterIOEx(
            b.c_rasterband(),
            gdal_sys::GDALRWFlag::GF_Read,
            req[0],
            req[1],
            req[2],
            req[3],
            buf.ptr(),
            size.0,
            size.1,
            t,
            0,
            0,
            &mut extra,
        )
    };
    err == gdal_sys::CPLErr::CE_None
}

// ---------------------------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------------------------

/// The most source values a request computes at once (its sources' buffers together).
const CHUNK_VALUES: usize = 8 << 20;

impl CalcVrt {
    pub fn bands(&self) -> usize {
        self.bands.len()
    }

    /// Whether every pixel's value is its own whatever window is read (then computed at the pixels read alone).
    pub fn pointwise(&self) -> bool {
        *self.pointwise.get_or_init(|| {
            self.bands.iter().flat_map(|b| &b.sources).all(|s| {
                if s.src_win != [UNSET; 4] || s.dst_win != [UNSET; 4] {
                    return false;
                }
                match &s.nested {
                    Some(n) => n.width == self.width && n.height == self.height && n.pointwise(),
                    None => {
                        !s.path.to_ascii_lowercase().ends_with(".vrt")
                            && band_info(s).is_ok_and(|(w, h, t)| w == self.width && h == self.height && !is_complex(t))
                    }
                }
            })
        })
    }

    fn check(&self, band: usize, (x, y, w, h): (usize, usize, usize, usize)) -> Result<&DBand, Miss> {
        if x + w > self.width || y + h > self.height {
            return Err(Miss::Failed(format!("{}: window out of the raster", self.path)));
        }
        self.bands.get(band.wrapping_sub(1)).ok_or_else(|| Miss::Failed(format!("{}: no band {band}", self.path)))
    }

    /// Band `band` (1-based) over the window `win` read into `size` as GDAL reads it (`how`), `fwin` the
    /// floating-point window of GDALRasterIOEx if any; `out` holds size.0 × size.1 values.
    pub fn read(
        &self,
        band: usize,
        win: (usize, usize, usize, usize),
        fwin: Option<[f64; 4]>,
        size: (usize, usize),
        how: How,
        out: &mut [f32],
    ) -> Result<(), Miss> {
        if win.2 == 0 || win.3 == 0 || size.0 == 0 || size.1 == 0 {
            return Ok(());
        }
        self.check(band, win)?;
        match how {
            How::Direct => self.direct(band, win, fwin, size, out, false),
            How::Cached => self.cached(band, win, fwin, size, out),
        }
    }

    /// Full resolution rows y0..y0 + rows of band `band` (what a GeoTIFF of it holds), on the threads of the reads.
    pub fn read_rows(&self, band: usize, y0: usize, rows: usize, out: &mut [f32]) -> Result<(), Miss> {
        let w = self.width;
        self.read(band, (0, y0, w, rows), None, (w, rows), How::Cached, out)
    }

    /// The VRT computed here into a GeoTIFF of Float32 at `tif` (`options` its creation options) with what GDAL copies
    /// of it (grid, CRS, metadata of the default domain, the bands' descriptions and metadata, no data NaN): a pointwise VRT's values are
    /// the same whatever windows read it. Chunks of rows one after another, each on the threads of the reads;
    /// `progress` the share written, false to stop (`Miss::Failed("stopped")`).
    pub fn write_tif(
        &self,
        tif: &std::path::Path,
        options: &[&str],
        progress: &dyn Fn(f64) -> bool,
    ) -> Result<(), Miss> {
        use gdal::Metadata as _;
        let failed = |e: gdal::errors::GdalError| Miss::Failed(format!("{}: {e}", tif.display()));
        let src = Dataset::open(&self.path).map_err(|e| Miss::Failed(format!("{}: {e}", self.path)))?;
        let (w, h, n) = (self.width, self.height, self.bands.len());
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(failed)?;
        let co = gdal::raster::RasterCreationOptions::from_iter(options.iter().copied());
        let mut ds = drv.create_with_band_type_with_options::<f32, _>(tif, w, h, n, &co).map_err(failed)?;
        if let Ok(gt) = src.geo_transform() {
            ds.set_geo_transform(&gt).map_err(failed)?;
        }
        let wkt = src.projection();
        if !wkt.is_empty() {
            ds.set_projection(&wkt).map_err(failed)?;
        }
        for m in src.metadata().filter(|m| m.domain.is_empty()) {
            ds.set_metadata_item(&m.key, &m.value, &m.domain).map_err(failed)?;
        }
        for b in 1..=n {
            let (sb, mut db) = (src.rasterband(b).map_err(failed)?, ds.rasterband(b).map_err(failed)?);
            db.set_no_data_value(Some(f64::NAN)).map_err(failed)?;
            db.set_description(&sb.description().unwrap_or_default()).map_err(failed)?;
            for m in sb.metadata().filter(|m| m.domain.is_empty()) {
                db.set_metadata_item(&m.key, &m.value, &m.domain).map_err(failed)?;
            }
        }
        drop(src);
        // rows by whole tiles of 256, about CHUNK_VALUES values
        let per = ((CHUNK_VALUES / w.max(1)) / 256 * 256).max(256).min(h);
        let chunks = h.div_ceil(per) * n;
        let mut done = 0;
        for b in 1..=n {
            for y in (0..h).step_by(per) {
                let rows = per.min(h - y);
                let mut buf = vec![0f32; w * rows];
                self.read_rows(b, y, rows, &mut buf)?;
                let mut buf = gdal::raster::Buffer::new((w, rows), buf);
                ds.rasterband(b).and_then(|mut d| d.write((0, y as isize), (w, rows), &mut buf)).map_err(failed)?;
                done += 1;
                if !progress(done as f64 / chunks as f64) {
                    return Err(Miss::Failed("stopped".into()));
                }
            }
        }
        ds.flush_cache().map_err(failed)
    }

    /// The derived band's IRasterIO (VRTDerivedRasterBand::IRasterIO): each source read for the window into a
    /// buffer of NaN, the band computed from them. `cached`: the VRT was opened with GDAL_FORCE_CACHING (its
    /// sources' datasets too: a calculator's VRT among them is read through its blocks).
    fn direct(
        &self,
        band: usize,
        win: (usize, usize, usize, usize),
        fwin: Option<[f64; 4]>,
        (bw, bh): (usize, usize),
        out: &mut [f32],
        cached: bool,
    ) -> Result<(), Miss> {
        let (x, y, w, h) = win;
        // a sampling of the grid: its pixels alone
        if let Some((rows, cols)) = self.direct_indices(band, win, fwin, (bw, bh)) {
            return self.sample(band, &rows, &cols, out);
        }
        let b = self.check(band, win)?;
        let f = fwin.unwrap_or([x as f64, y as f64, w as f64, h as f64]);
        let n = bw * bh;
        let mut bufs: Vec<Vec<f32>> = Vec::with_capacity(b.sources.len());
        for s in &b.sources {
            let mut v = vec![f32::NAN; n];
            self.source_into(s, f, (bw, bh), &mut v, cached)?;
            bufs.push(v);
        }
        let refs: Vec<&[f32]> = bufs.iter().map(Vec::as_slice).collect();
        evaluate(&Args(b.args.clone()), &refs, &mut out[..n]).map_err(|e| Miss::Failed(format!("{}: {e}", self.path)))
    }

    /// A source read for the request (fwin into `size`) into `buf` (NaN where it gives nothing), as
    /// VRTSimpleSource / VRTComplexSource::RasterIO read it.
    fn source_into(
        &self,
        s: &Src,
        fwin: [f64; 4],
        (bw, bh): (usize, usize),
        buf: &mut [f32],
        cached: bool,
    ) -> Result<(), Miss> {
        let (sw, shh, t) = band_info(s)?;
        if is_complex(t) {
            return Err(Miss::NotCovered);
        }
        let Some(win) = src_dst_window(s, fwin, (bw as i32, bh as i32), (sw, shh)) else { return Ok(()) };
        let [ox, oy, ow, oh] = win.out.map(|v| v as usize);
        let conv = conversion(s.kind, t);
        let mut typed = Typed::new(conv.read_type(), ow * oh);
        match &s.nested {
            Some(nv) => {
                let Typed::F32(v) = &mut typed else { unreachable!("a calculator's band is Float32") };
                let req = (win.req[0] as usize, win.req[1] as usize, win.req[2] as usize, win.req[3] as usize);
                nv.check(s.band, req)?;
                if cached {
                    nv.cached(s.band, req, Some(win.df), (ow, oh), v)?;
                } else {
                    nv.direct(s.band, req, Some(win.df), (ow, oh), v, false)?;
                }
            }
            None => {
                let alg = s.resampling.unwrap_or(GRIORA_NearestNeighbour);
                with_source(s, |ds| {
                    let b = ds.rasterband(s.band).map_err(|e| Miss::Failed(format!("{}: {e}", s.path)))?;
                    if raster_io(&b, win.req, &mut typed, (ow as i32, oh as i32), Some(win.df), alg) {
                        Ok(())
                    } else {
                        Err(Miss::Failed(format!("{}: {}", s.path, last_error())))
                    }
                })?;
            }
        }
        // the buffer is this source's alone, NaN before: the values it skips stay NaN
        if ox == 0 && ow == bw {
            conv.apply(&typed, &mut buf[oy * bw..(oy + oh) * bw]);
        } else {
            let mut part = vec![f32::NAN; ow * oh];
            conv.apply(&typed, &mut part);
            for (r, row) in part.chunks_exact(ow).enumerate() {
                buf[(oy + r) * bw + ox..(oy + r) * bw + ox + ow].copy_from_slice(row);
            }
        }
        Ok(())
    }

    /// GDALRasterBand::IRasterIO, the band read through its blocks (GDAL_FORCE_CACHING): at full resolution the
    /// window's pixels, else each pixel of the buffer the nearest's (GDAL's arithmetic).
    fn cached(
        &self,
        band: usize,
        win: (usize, usize, usize, usize),
        fwin: Option<[f64; 4]>,
        size: (usize, usize),
        out: &mut [f32],
    ) -> Result<(), Miss> {
        let (rows, cols) = self.indices(win, fwin, size);
        self.at(band, &rows, &cols, out)
    }

    /// The pixels of the grid GDALRasterBand::IRasterIO's nearest neighbour takes for a request (the window, its
    /// floating-point one if any, into bw × bh): the rows and the columns, in the buffer's order.
    fn indices(
        &self,
        (x, y, w, h): (usize, usize, usize, usize),
        fwin: Option<[f64; 4]>,
        (bw, bh): (usize, usize),
    ) -> (Vec<usize>, Vec<usize>) {
        let int_coords = fwin.is_none_or(|f| f == [x as f64, y as f64, w as f64, h as f64]);
        if bw == w && bh == h && int_coords {
            return ((y..y + h).collect(), (x..x + w).collect());
        }
        let [dx, dy, dw, dh] = fwin.unwrap_or([x as f64, y as f64, w as f64, h as f64]);
        let (inc_x, inc_y) = (dw / bw as f64, dh / bh as f64);
        const EPS: f64 = 1e-10;
        let (rw, rh) = (self.width as f64, self.height as f64);
        let rows = (0..bh).map(|j| ((j as f64 + 0.5) * inc_y + dy + EPS).max(0.0).min(rh - 1.0) as usize).collect();
        let start = 0.5 * inc_x + dx + EPS;
        let cols = if int_coords && (inc_x as i32) as f64 == inc_x && (inc_x as i32) < i32::MAX / 4 {
            // DownsamplingIntegerXFactor: whole steps, the last within the raster
            let (x0, step) = (start as i64, inc_x as i64);
            let mut c: Vec<usize> = (0..bw as i64).map(|k| (x0 + k * step) as usize).collect();
            if bw >= 2 {
                c[bw - 1] = (x0 + (bw as i64 - 1) * step).min(self.width as i64 - 1) as usize;
            }
            c
        } else {
            let mut sx = start;
            (0..bw)
                .map(|_| {
                    let c = sx.max(0.0).min(rw - 1.0) as usize;
                    sx += inc_x;
                    c
                })
                .collect()
        };
        (rows, cols)
    }

    /// The pixels a read of the derived band as is takes (`Direct`) when it is a sampling of the grid: a pointwise
    /// VRT at full resolution, or at another size when none of its files has overviews (each file then read by
    /// GDALRasterBand::IRasterIO's nearest neighbour over the source window GetSrcDstWindow gives, the VRT's own).
    /// None: read as the derived band reads it.
    fn direct_indices(
        &self,
        band: usize,
        win: (usize, usize, usize, usize),
        fwin: Option<[f64; 4]>,
        (bw, bh): (usize, usize),
    ) -> Option<(Vec<usize>, Vec<usize>)> {
        if !self.pointwise() {
            return None;
        }
        let (x, y, w, h) = win;
        let f = [x as f64, y as f64, w as f64, h as f64];
        if bw == w && bh == h && fwin.is_none_or(|g| g == f) {
            return Some(((y..y + h).collect(), (x..x + w).collect()));
        }
        if self.has_overviews() {
            return None;
        }
        let s = self.bands.get(band.wrapping_sub(1))?.sources.first()?;
        let sw = src_dst_window(s, fwin.unwrap_or(f), (bw as i32, bh as i32), (self.width, self.height))?;
        if sw.out != [0, 0, bw as i32, bh as i32] {
            return None;
        }
        let [rx, ry, rw, rh] = sw.req.map(|v| v as usize);
        Some(self.indices((rx, ry, rw, rh), Some(sw.df), (bw, bh)))
    }

    /// Whether a file it reads (or one a VRT it reads reads) has overviews.
    fn has_overviews(&self) -> bool {
        *self.overviews.get_or_init(|| {
            self.bands.iter().flat_map(|b| &b.sources).any(|s| match &s.nested {
                Some(n) => n.has_overviews(),
                None => has_overviews(s),
            })
        })
    }

    /// The blocks of the first file it reads (through the VRTs it reads), else its own.
    fn grain(&self) -> (usize, usize) {
        *self.grain.get_or_init(|| {
            let Some(s) = self.bands.first().and_then(|b| b.sources.first()) else { return self.block };
            match &s.nested {
                Some(n) => n.grain(),
                None => with_source(s, |ds| {
                    let b = ds.rasterband(s.band).map_err(|e| Miss::Failed(e.to_string()))?;
                    Ok(b.block_size())
                })
                .unwrap_or(self.block),
            }
        })
    }

    /// The values at rows × cols of the grid, `out[j * cols.len() + i]`, as the band's blocks give them.
    fn at(&self, band: usize, rows: &[usize], cols: &[usize], out: &mut [f32]) -> Result<(), Miss> {
        if self.pointwise() { self.sample(band, rows, cols, out) } else { self.through_blocks(band, rows, cols, out) }
    }

    /// Parts of one read stacked in `out` (each a window, its floating-point window if any, and its size; all as
    /// wide), each read as GDAL reads it (`how`): when every part is a sampling of the grid, by the same columns,
    /// all of them at once on the threads of the reads (`Ok(true)`, a block of a file read by one thread);
    /// Ok(false): to be read part by part.
    pub fn read_parts(&self, band: usize, parts: &[Part], how: How, out: &mut [f32]) -> Result<bool, Miss> {
        let mut rows = Vec::new();
        let mut cols: Option<Vec<usize>> = None;
        for &(win, fwin, size) in parts {
            self.check(band, win)?;
            if win.2 == 0 || win.3 == 0 || size.0 == 0 || size.1 == 0 {
                return Ok(false);
            }
            let (r, c) = match how {
                How::Cached => self.indices(win, fwin, size),
                How::Direct => match self.direct_indices(band, win, fwin, size) {
                    Some(rc) => rc,
                    None => return Ok(false),
                },
            };
            match &cols {
                Some(c0) if *c0 != c => return Ok(false),
                Some(_) => {}
                None => cols = Some(c),
            }
            rows.extend(r);
        }
        let Some(cols) = cols else { return Ok(true) };
        let n = rows.len() * cols.len();
        match how {
            How::Direct => self.sample(band, &rows, &cols, &mut out[..n])?,
            How::Cached => self.at(band, &rows, &cols, &mut out[..n])?,
        }
        Ok(true)
    }

    /// The values at rows × cols (full resolution; a pointwise VRT), `out[j * cols.len() + i]`, computed at those
    /// pixels alone: by parts of whole blocks of the files (a block read by one thread), on the threads of the reads.
    fn sample(&self, band: usize, rows: &[usize], cols: &[usize], out: &mut [f32]) -> Result<(), Miss> {
        let b = self.check(band, (0, 0, 0, 0))?;
        if rows.is_empty() || cols.is_empty() {
            return Ok(());
        }
        let unique = |v: &[usize]| {
            let mut u = v.to_vec();
            u.sort_unstable();
            u.dedup();
            u
        };
        let (urows, ucols) = (unique(rows), unique(cols));
        let nc = cols.len();
        let sorted = |v: &[usize]| v.windows(2).all(|p| p[0] <= p[1]);
        if !sorted(rows) || !sorted(cols) {
            // in another order: one part, each value placed by its row and column
            let vals = self.sample_part(b, &urows, &ucols)?;
            let ci: Vec<usize> = cols.iter().map(|c| ucols.binary_search(c).unwrap()).collect();
            for (j, r) in rows.iter().enumerate() {
                let src = &vals[urows.binary_search(r).unwrap() * ucols.len()..];
                for (o, &c) in out[j * nc..(j + 1) * nc].iter_mut().zip(&ci) {
                    *o = src[c];
                }
            }
            return Ok(());
        }
        // the parts: the files' rows of blocks, each across runs of their columns of blocks (two parts a thread at
        // least, when there are as many columns of blocks)
        let (gw, gh) = self.grain();
        let groups = |u: &[usize], g: usize| {
            let mut v: Vec<(usize, usize)> = Vec::new();
            for (k, &x) in u.iter().enumerate() {
                match v.last_mut() {
                    Some(last) if u[last.0] / g == x / g => last.1 = k + 1,
                    _ => v.push((k, k + 1)),
                }
            }
            v
        };
        let (rg, cg) = (groups(&urows, gh.max(1)), groups(&ucols, gw.max(1)));
        let runs = (threads() * 2).div_ceil(rg.len()).clamp(1, cg.len());
        let cr: Vec<(usize, usize)> = cg.chunks(cg.len().div_ceil(runs)).map(|c| (c[0].0, c[c.len() - 1].1)).collect();
        let units: Vec<((usize, usize), (usize, usize))> =
            rg.iter().flat_map(|&r| cr.iter().map(move |&c| (r, c))).collect();
        let out = Mutex::new(out);
        let run = |(r, c): ((usize, usize), (usize, usize))| -> Result<(), Miss> {
            let (ur, uc) = (&urows[r.0..r.1], &ucols[c.0..c.1]);
            let vals = self.sample_part(b, ur, uc)?;
            // the buffer's rows and columns of these (a row or a column asked twice, twice)
            let (j0, j1) = (rows.partition_point(|&v| v < ur[0]), rows.partition_point(|&v| v <= ur[ur.len() - 1]));
            let (i0, i1) = (cols.partition_point(|&v| v < uc[0]), cols.partition_point(|&v| v <= uc[uc.len() - 1]));
            let ci: Vec<usize> = cols[i0..i1].iter().map(|v| uc.binary_search(v).unwrap()).collect();
            let mut o = out.lock().unwrap();
            for (j, r) in rows.iter().enumerate().take(j1).skip(j0) {
                let src = &vals[ur.binary_search(r).unwrap() * uc.len()..];
                for (d, &k) in o[j * nc + i0..j * nc + i1].iter_mut().zip(&ci) {
                    *d = src[k];
                }
            }
            Ok(())
        };
        if units.len() == 1 {
            return run(units[0]);
        }
        let error = Mutex::new(None);
        pool().scope(|sc| {
            for &u in &units {
                let (run, error) = (&run, &error);
                sc.spawn(move |_| {
                    if let Err(e) = run(u) {
                        error.lock().unwrap().get_or_insert(e);
                    }
                });
            }
        });
        error.into_inner().unwrap().map_or(Ok(()), Err)
    }

    /// The band at urows × ucols (sorted, each once) of a pointwise VRT, in chunks of rows within memory.
    fn sample_part(&self, b: &DBand, urows: &[usize], ucols: &[usize]) -> Result<Vec<f32>, Miss> {
        let nc = ucols.len();
        let span_w = ucols[nc - 1] - ucols[0] + 1;
        let per_px = b.sources.len().max(1);
        let most = (CHUNK_VALUES / threads()).max(1 << 20);
        let mut vals = vec![f32::NAN; urows.len() * nc];
        let mut at = 0;
        while at < urows.len() {
            // rows whose sources' values (and one source's span read) fit
            let mut end = at + 1;
            while end < urows.len()
                && (end + 1 - at) * nc * per_px <= most
                && (urows[end] - urows[at] + 1) * span_w <= most
            {
                end += 1;
            }
            self.gather(b, &urows[at..end], ucols, &mut vals[at * nc..end * nc])?;
            at = end;
        }
        Ok(vals)
    }

    /// The band at rows × cols (sorted, each once) of a pointwise VRT.
    fn gather(&self, b: &DBand, rows: &[usize], cols: &[usize], out: &mut [f32]) -> Result<(), Miss> {
        let n = rows.len() * cols.len();
        let (r0, c0) = (rows[0], cols[0]);
        let (sh, sw) = (rows[rows.len() - 1] - r0 + 1, cols[cols.len() - 1] - c0 + 1);
        // the values' places in the span read
        let idx: Vec<usize> = rows.iter().flat_map(|&r| cols.iter().map(move |&c| (r - r0) * sw + (c - c0))).collect();
        let dense = sh == rows.len() && sw == cols.len();
        let mut bufs: Vec<Vec<f32>> = Vec::with_capacity(b.sources.len());
        for s in &b.sources {
            let mut v = vec![f32::NAN; n];
            let (_, _, t) = band_info(s)?;
            let conv = conversion(s.kind, t);
            match &s.nested {
                Some(nv) => {
                    let mut f = vec![f32::NAN; n];
                    nv.gather(&nv.bands[s.band - 1], rows, cols, &mut f)?;
                    conv.apply(&Typed::F32(f), &mut v);
                }
                None => {
                    let mut typed = Typed::new(conv.read_type(), sw * sh);
                    let req = [c0 as i32, r0 as i32, sw as i32, sh as i32];
                    with_source(s, |ds| {
                        let band = ds.rasterband(s.band).map_err(|e| Miss::Failed(format!("{}: {e}", s.path)))?;
                        if raster_io(&band, req, &mut typed, (sw as i32, sh as i32), None, GRIORA_NearestNeighbour) {
                            Ok(())
                        } else {
                            Err(Miss::Failed(format!("{}: {}", s.path, last_error())))
                        }
                    })?;
                    if dense {
                        conv.apply(&typed, &mut v);
                    } else {
                        conv.apply(&typed.pick(&idx), &mut v);
                    }
                }
            }
            bufs.push(v);
        }
        let refs: Vec<&[f32]> = bufs.iter().map(Vec::as_slice).collect();
        evaluate(&Args(b.args.clone()), &refs, &mut out[..n]).map_err(|e| Miss::Failed(format!("{}: {e}", self.path)))
    }

    /// The values at rows × cols from the band's blocks, each computed whole as GDAL computes a block
    /// (VRTSourcedRasterBand::IReadBlock): a VRT whose values depend on the window read. The blocks of a few of
    /// their rows at a time, on the threads of the reads.
    fn through_blocks(&self, band: usize, rows: &[usize], cols: &[usize], out: &mut [f32]) -> Result<(), Miss> {
        let (bx, by) = self.block;
        let nc = cols.len();
        let unique = |mut v: Vec<usize>| {
            v.sort_unstable();
            v.dedup();
            v
        };
        let bcols = unique(cols.iter().map(|c| c / bx).collect());
        let brows = unique(rows.iter().map(|r| r / by).collect());
        // rows of blocks together: two blocks a thread at least
        let per = (threads() * 2).div_ceil(bcols.len().max(1)).max(1);
        for batch in brows.chunks(per) {
            let keys: Vec<(usize, usize)> = batch.iter().flat_map(|&r| bcols.iter().map(move |&c| (c, r))).collect();
            let block = |&(c, r): &(usize, usize)| -> Result<Vec<f32>, Miss> {
                let (x0, y0) = (c * bx, r * by);
                let (w, h) = (bx.min(self.width - x0), by.min(self.height - y0));
                let mut v = vec![0f32; w * h];
                self.direct(band, (x0, y0, w, h), None, (w, h), &mut v, true)?;
                Ok(v)
            };
            let done: Vec<Mutex<Option<Block>>> = keys.iter().map(|_| Mutex::new(None)).collect();
            if keys.len() == 1 {
                *done[0].lock().unwrap() = Some(block(&keys[0]));
            } else {
                pool().scope(|sc| {
                    for (k, key) in keys.iter().enumerate() {
                        let (block, done) = (&block, &done);
                        sc.spawn(move |_| *done[k].lock().unwrap() = Some(block(key)));
                    }
                });
            }
            let mut got: HashMap<(usize, usize), Vec<f32>> = HashMap::with_capacity(keys.len());
            for (key, d) in keys.iter().zip(done) {
                got.insert(*key, d.into_inner().unwrap().expect("computed")?);
            }
            for (j, &r) in rows.iter().enumerate() {
                if !batch.contains(&(r / by)) {
                    continue;
                }
                for (i, &c) in cols.iter().enumerate() {
                    let key = (c / bx, r / by);
                    let w = bx.min(self.width - key.0 * bx);
                    out[j * nc + i] = got[&key][(r - key.1 * by) * w + (c - key.0 * bx)];
                }
            }
        }
        Ok(())
    }
}

/// A block computed, or why not.
type Block = Result<Vec<f32>, Miss>;

/// A part of a read: the window, its floating-point window if any, and the size it is read into.
pub type Part = ((usize, usize, usize, usize), Option<[f64; 4]>, (usize, usize));
