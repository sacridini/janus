//! The calculator's results as VRTs: a derived band per date whose pixel function (`janus_calc`, registered with
//! GDAL in this process) compiles the band's canonical text once and evaluates it over the sources GDAL reads,
//! resampled, scaled and with their no data as NaN. Anything that reads rasters through this GDAL reads them:
//! the overview and the tiles (through the files' overviews), a pixel's series, Zeit's runs, a COG export.
//!
//! A band's arguments: `expr` (the canonical text) and `n` (its inputs), `absent` (inputs without a source at this
//! date: NaN), the date's variables (`year`, `month`, `day`, `doy`, `decyear`, `t`). A reduction over time
//! (`reduce`) evaluates the text at each of `dates` dates over `captures` inputs read once and `stride` inputs of
//! that date, its variables in `vars` (`y,m,d,doy,decyear,t;...`).

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt::Write as _;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::compile::RedFn;
use super::program::{BLOCK, DateVars, Opnd};
use crate::BandMeta;

pub const PIXEL_FUNCTION: &str = "janus_calc";

static REGISTER: Once = Once::new();

/// Makes the pixel function known to GDAL (once per process; before a calculator's VRT is opened). GDAL's pool of
/// the files VRTs read (100 by default) is raised unless set: a band over a series of more dates than that closed
/// and reopened its files block after block (a reduction over 107 dates read in 6 s instead of 3). The program
/// sets it first, from the files it may open (janus-app's main).
pub fn register() {
    REGISTER.call_once(|| {
        let key = "GDAL_MAX_DATASET_POOL_SIZE";
        if std::env::var_os(key).is_none() && gdal::config::get_config_option(key, "").unwrap_or_default().is_empty() {
            let _ = gdal::config::set_config_option(key, "450");
        }
        let name = CString::new(PIXEL_FUNCTION).unwrap();
        // SAFETY: GDAL copies the name; the function is a 'static extern "C" fn.
        unsafe {
            gdal_sys::GDALAddDerivedBandPixelFuncWithArgs(name.as_ptr(), Some(pixel_function), std::ptr::null());
        }
    });
}

/// A band's arguments by name.
struct Args(Vec<(String, String)>);

impl Args {
    /// # Safety
    /// `list` is null or a null-terminated list of C strings "KEY=VALUE".
    unsafe fn of(list: *mut *mut c_char) -> Args {
        let mut v = Vec::new();
        let mut p = list as *const *const c_char;
        // SAFETY: as the caller promises.
        unsafe {
            while !p.is_null() && !(*p).is_null() {
                let s = CStr::from_ptr(*p).to_string_lossy();
                if let Some((k, x)) = s.split_once('=') {
                    v.push((k.to_string(), x.to_string()));
                }
                p = p.add(1);
            }
        }
        Args(v)
    }

    fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|a| a.0.eq_ignore_ascii_case(k)).map(|a| a.1.as_str())
    }

    fn num(&self, k: &str) -> Option<f64> {
        self.get(k).and_then(|v| v.trim().parse().ok())
    }

    fn list(&self, k: &str) -> Vec<usize> {
        self.get(k).map_or(Vec::new(), |v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
    }

    fn vars(&self) -> DateVars {
        let mut d = DateVars::default();
        for (i, v) in super::program::DateVar::ALL.iter().enumerate() {
            if let Some(x) = self.num(v.name()) {
                d.0[i] = x as f32;
            }
        }
        d
    }
}

fn parse_vars(s: &str) -> DateVars {
    let mut d = DateVars::default();
    for (i, x) in s.split(',').enumerate().take(6) {
        d.0[i] = x.trim().parse().unwrap_or(f32::NAN);
    }
    d
}

/// The values of one date's variables as a `vars` entry.
pub fn vars_text(v: &DateVars) -> String {
    v.0.iter().map(|x| if x.is_nan() { "nan".to_string() } else { format!("{x}") }).collect::<Vec<_>>().join(",")
}

fn report(m: &str) -> gdal_sys::CPLErr::Type {
    let fmt = CString::new("%s").unwrap();
    let msg = CString::new(format!("janus_calc: {m}")).unwrap_or_default();
    // SAFETY: a format and its argument, both C strings alive for the call.
    unsafe { gdal_sys::CPLError(gdal_sys::CPLErr::CE_Failure, 1 as c_int, fmt.as_ptr(), msg.as_ptr()) };
    gdal_sys::CPLErr::CE_Failure
}

static LARGEST: AtomicUsize = AtomicUsize::new(0);

/// The most source values one call of the pixel function has been given (what a block of its sources holds).
pub fn largest_request() -> usize {
    LARGEST.load(Ordering::Relaxed)
}

/// GDAL's call for a window of a derived band.
unsafe extern "C" fn pixel_function(
    sources: *mut *mut c_void,
    n_sources: c_int,
    data: *mut c_void,
    w: c_int,
    h: c_int,
    src_type: gdal_sys::GDALDataType::Type,
    buf_type: gdal_sys::GDALDataType::Type,
    pixel_space: c_int,
    line_space: c_int,
    args: *mut *mut c_char,
) -> gdal_sys::CPLErr::Type {
    let n = (w.max(0) as usize) * (h.max(0) as usize);
    LARGEST.fetch_max(n * n_sources.max(0) as usize, Ordering::Relaxed);
    // SAFETY: GDAL's argument list.
    let args = unsafe { Args::of(args) };
    // the sources as floats (they are asked for as Float32; converted otherwise)
    let ns = n_sources.max(0) as usize;
    let f32_type = src_type == gdal_sys::GDALDataType::GDT_Float32;
    let owned: Vec<Vec<f32>> = if f32_type {
        Vec::new()
    } else {
        (0..ns)
            .map(|i| {
                let mut v = vec![0f32; n];
                // SAFETY: GDAL gives ns buffers of n values of src_type; converted into v.
                unsafe {
                    gdal_sys::GDALCopyWords64(
                        *sources.add(i),
                        src_type,
                        gdal_sys::GDALGetDataTypeSizeBytes(src_type),
                        v.as_mut_ptr() as *mut c_void,
                        gdal_sys::GDALDataType::GDT_Float32,
                        4,
                        n as i64,
                    )
                };
                v
            })
            .collect()
    };
    let src: Vec<&[f32]> = if f32_type {
        // SAFETY: GDAL gives ns buffers of n floats.
        (0..ns).map(|i| unsafe { std::slice::from_raw_parts(*sources.add(i) as *const f32, n) }).collect()
    } else {
        owned.iter().map(|v| v.as_slice()).collect()
    };
    let mut out = vec![0f32; n];
    if let Err(e) = evaluate(&args, &src, &mut out) {
        return report(&e);
    }
    // the result into GDAL's buffer, of its type and spacing
    for y in 0..h.max(0) as usize {
        // SAFETY: GDAL's buffer holds h lines of line_space bytes, w pixels of pixel_space bytes each.
        unsafe {
            gdal_sys::GDALCopyWords64(
                out[y * w as usize..].as_ptr() as *const c_void,
                gdal_sys::GDALDataType::GDT_Float32,
                4,
                (data as *mut u8).add(y * line_space as usize) as *mut c_void,
                buf_type,
                pixel_space,
                w as i64,
            );
        }
    }
    gdal_sys::CPLErr::CE_None
}

/// The inputs of the program from the sources present (`absent` ones, 1-based, NaN).
fn place<'a>(total: usize, absent: &[usize], src: &[&'a [f32]], nan: &'a [f32]) -> Result<Vec<&'a [f32]>, String> {
    let mut v = Vec::with_capacity(total);
    let mut k = 0;
    for i in 1..=total {
        if absent.contains(&i) {
            v.push(nan);
        } else {
            v.push(*src.get(k).ok_or_else(|| format!("{} sources for {total} inputs", src.len()))?);
            k += 1;
        }
    }
    Ok(v)
}

fn evaluate(args: &Args, src: &[&[f32]], out: &mut [f32]) -> Result<(), String> {
    let n = out.len();
    let text = args.get("expr").ok_or("no expr")?;
    let absent = args.list("absent");
    let nan = vec![f32::NAN; n];
    let Some(reduce) = args.get("reduce") else {
        let inputs = args.num("n").ok_or("no n")? as usize;
        let p = super::cached_canonical(text, inputs).map_err(|e| e.message)?;
        let ins = place(inputs, &absent, src, &nan)?;
        p.eval(&ins, &args.vars(), out);
        return Ok(());
    };
    let f = RedFn::parse(reduce).ok_or_else(|| format!("reduce={reduce}"))?;
    let (dates, stride, captures) = (
        args.num("dates").ok_or("no dates")? as usize,
        args.num("stride").ok_or("no stride")? as usize,
        args.num("captures").unwrap_or(0.0) as usize,
    );
    let p = super::cached_canonical(text, captures + stride).map_err(|e| e.message)?;
    let all = place(captures + dates * stride, &absent, src, &nan)?;
    let vars: Vec<DateVars> = args.get("vars").map_or(Vec::new(), |v| v.split(';').map(parse_vars).collect());
    reduce_dates(f, &p, &all, captures, stride, dates, &vars, out);
    Ok(())
}

/// A reduction over `dates` dates: the program at each date, its no data skipped. Block by block (a block's
/// accumulators stay in the cache over the dates), a loop of its own for each function.
#[allow(clippy::too_many_arguments)]
pub fn reduce_dates(
    f: RedFn,
    p: &super::Program,
    all: &[&[f32]],
    captures: usize,
    stride: usize,
    dates: usize,
    vars: &[DateVars],
    out: &mut [f32],
) {
    let n = out.len();
    // a program that is one of its inputs is read in place
    let input = match (p.code.is_empty(), p.out) {
        (true, Opnd::Input(k)) => Some(k as usize),
        _ => None,
    };
    let keep_all = matches!(f, RedFn::Median | RedFn::Percentile(_));
    let mut tmp = vec![0f32; BLOCK];
    let mut values: Vec<f32> = if keep_all { vec![0f32; BLOCK * dates] } else { Vec::new() };
    let (mut acc, mut m2, mut cnt) = (vec![0f64; BLOCK], vec![0f64; BLOCK], vec![0u32; BLOCK]);
    let mut ext = vec![f32::NAN; BLOCK];
    let mut px: Vec<f32> = Vec::with_capacity(dates);
    let mut ins: Vec<&[f32]> = Vec::with_capacity(captures + stride);
    let no_vars = DateVars::default();
    for start in (0..n).step_by(BLOCK) {
        let len = BLOCK.min(n - start);
        let r = start..start + len;
        acc[..len].fill(0.0);
        m2[..len].fill(0.0);
        cnt[..len].fill(0);
        ext[..len].fill(f32::NAN);
        for d in 0..dates {
            let date = &all[captures + d * stride..captures + (d + 1) * stride];
            let v: &[f32] = match input {
                Some(k) => {
                    let s = if k < captures { all[k] } else { date[k - captures] };
                    &s[r.clone()]
                }
                None => {
                    ins.clear();
                    ins.extend(all[..captures].iter().chain(date).map(|s| &s[r.clone()]));
                    p.eval(&ins, vars.get(d).unwrap_or(&no_vars), &mut tmp[..len]);
                    &tmp[..len]
                }
            };
            match f {
                RedFn::Median | RedFn::Percentile(_) => values[d * BLOCK..d * BLOCK + len].copy_from_slice(v),
                RedFn::Max => {
                    for (e, &x) in ext[..len].iter_mut().zip(v) {
                        // NaN so far, or below x (a select, not a branch)
                        let take = x.is_finite() & (x > *e || e.is_nan());
                        *e = if take { x } else { *e };
                    }
                }
                RedFn::Min => {
                    for (e, &x) in ext[..len].iter_mut().zip(v) {
                        let take = x.is_finite() & (x < *e || e.is_nan());
                        *e = if take { x } else { *e };
                    }
                }
                RedFn::Sum | RedFn::Mean | RedFn::At { .. } => {
                    for ((a, c), &x) in acc[..len].iter_mut().zip(&mut cnt[..len]).zip(v) {
                        let ok = x.is_finite();
                        *a += if ok { f64::from(x) } else { 0.0 };
                        *c += u32::from(ok);
                    }
                }
                RedFn::Count => {
                    for (c, &x) in cnt[..len].iter_mut().zip(v) {
                        *c += u32::from(x.is_finite() && x != 0.0);
                    }
                }
                RedFn::Std => {
                    // Welford
                    for (((a, q), c), &x) in acc[..len].iter_mut().zip(&mut m2[..len]).zip(&mut cnt[..len]).zip(v) {
                        let ok = x.is_finite();
                        *c += u32::from(ok);
                        let x = if ok { f64::from(x) } else { *a };
                        let delta = x - *a;
                        *a += delta / f64::from((*c).max(1));
                        *q += delta * (x - *a);
                    }
                }
            }
        }
        let dst = &mut out[start..start + len];
        match f {
            RedFn::Median | RedFn::Percentile(_) => {
                let q = match f {
                    RedFn::Percentile(p) => f64::from(p) / 100.0,
                    _ => 0.5,
                };
                for (i, o) in dst.iter_mut().enumerate() {
                    px.clear();
                    px.extend((0..dates).map(|d| values[d * BLOCK + i]).filter(|v| v.is_finite()));
                    *o = percentile(&mut px, q);
                }
            }
            RedFn::Max | RedFn::Min => dst.copy_from_slice(&ext[..len]),
            RedFn::Count => dst.iter_mut().zip(&cnt[..len]).for_each(|(o, &c)| *o = c as f32),
            RedFn::Sum => {
                for ((o, &a), &c) in dst.iter_mut().zip(&acc[..len]).zip(&cnt[..len]) {
                    *o = if c == 0 { f32::NAN } else { a as f32 };
                }
            }
            RedFn::Mean | RedFn::At { .. } => {
                for ((o, &a), &c) in dst.iter_mut().zip(&acc[..len]).zip(&cnt[..len]) {
                    *o = if c == 0 { f32::NAN } else { (a / f64::from(c)) as f32 };
                }
            }
            RedFn::Std => {
                for ((o, &q), &c) in dst.iter_mut().zip(&m2[..len]).zip(&cnt[..len]) {
                    *o = if c == 0 { f32::NAN } else { (q / f64::from(c)).sqrt() as f32 };
                }
            }
        }
    }
}

/// numpy's percentile (linear between the nearest ranks) of the values (reordered); NaN without any.
pub fn percentile(v: &mut [f32], q: f64) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    let pos = q * (v.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    let (_, &mut a, above) = v.select_nth_unstable_by(lo, f32::total_cmp);
    // the next rank: the least of those above
    let b = if hi == lo { a } else { above.iter().copied().fold(f32::INFINITY, f32::min) };
    let f = pos - lo as f64;
    (f64::from(a) + f * (f64::from(b) - f64::from(a))) as f32
}

/// Whether a file is a VRT of the calculator (its first bytes name the pixel function).
pub fn is_calc_vrt(path: &str) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut head = vec![0u8; 4096];
    let n = f.read(&mut head).unwrap_or(0);
    String::from_utf8_lossy(&head[..n]).contains(PIXEL_FUNCTION)
}

/// Runs `f` with GDAL reading through its block cache on this thread (GDAL_FORCE_CACHING) when `on`: a derived
/// band then computes block by block, each block's sources only in memory, instead of every source of a whole
/// window at once (a reduction over a hundred dates of the overview).
pub fn with_block_reads<R>(on: bool, f: impl FnOnce() -> R) -> R {
    if !on {
        return f();
    }
    let key = "GDAL_FORCE_CACHING";
    let old = gdal::config::get_thread_local_config_option(key, "").unwrap_or_default();
    let _ = gdal::config::set_thread_local_config_option(key, "YES");
    let r = f();
    let _ = if old.is_empty() {
        gdal::config::clear_thread_local_config_option(key)
    } else {
        gdal::config::set_thread_local_config_option(key, &old)
    };
    r
}

// ---------------------------------------------------------------------------------------------------------------
// Writing VRTs
// ---------------------------------------------------------------------------------------------------------------

/// One source of a derived band: a band of a file, where it goes on the grid and how it is resampled.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub path: String,
    pub band: i32,
    pub meta: BandMeta,
    /// quality codes as they are (no scale, no no data)
    pub raw: bool,
    /// the file's window (x, y, w, h) and where it goes on the grid (None: the whole file on the whole grid)
    pub rects: Option<([f64; 4], [f64; 4])>,
    /// nearest, bilinear, cubic, average, mode (None: nearest)
    pub resampling: Option<String>,
}

/// A derived band: its description (the date), its sources (None: absent at this date) and arguments.
#[derive(Clone, Debug, Default)]
pub struct Band {
    pub description: String,
    pub sources: Vec<Option<Source>>,
    pub args: Vec<(String, String)>,
}

/// The grid of a VRT.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub w: usize,
    pub h: usize,
    pub gt: Option<[f64; 6]>,
    /// WKT ("" = none)
    pub srs: String,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn g17(v: f64) -> String {
    crate::multidim::format_g(v, 17)
}

/// The XML of a VRT of derived bands; `block` its blocks' width and height (small for bands of many sources: GDAL
/// reads every source of a block at once; see block_shape).
pub fn vrt_xml(grid: &Grid, bands: &[Band], (bx, by): (usize, usize)) -> String {
    let mut x = format!("<VRTDataset rasterXSize=\"{}\" rasterYSize=\"{}\">\n", grid.w, grid.h);
    if !grid.srs.is_empty() {
        let _ = writeln!(x, "  <SRS>{}</SRS>", xml_escape(&grid.srs));
    }
    if let Some(g) = grid.gt {
        let _ = writeln!(x, "  <GeoTransform>{}</GeoTransform>", g.map(g17).join(", "));
    }
    for (i, b) in bands.iter().enumerate() {
        let _ = writeln!(
            x,
            "  <VRTRasterBand dataType=\"Float32\" band=\"{}\" subClass=\"VRTDerivedRasterBand\" blockXSize=\"{bx}\" \
             blockYSize=\"{by}\">",
            i + 1
        );
        let _ = writeln!(x, "    <Description>{}</Description>", xml_escape(&b.description));
        x += "    <NoDataValue>nan</NoDataValue>\n";
        let _ = writeln!(x, "    <PixelFunctionType>{PIXEL_FUNCTION}</PixelFunctionType>");
        x += "    <PixelFunctionArguments";
        let absent: Vec<String> =
            b.sources.iter().enumerate().filter(|s| s.1.is_none()).map(|s| (s.0 + 1).to_string()).collect();
        let mut args = b.args.clone();
        if !absent.is_empty() {
            args.push(("absent".into(), absent.join(",")));
        }
        for (k, v) in &args {
            let _ = write!(x, " {k}=\"{}\"", xml_escape(v));
        }
        x += "/>\n    <SourceTransferType>Float32</SourceTransferType>\n";
        for s in b.sources.iter().flatten() {
            let m = if s.raw { BandMeta::default() } else { s.meta };
            let scaled = m.scale != 1.0 || m.offset != 0.0;
            // a NaN no data is already the band's (a float band): a SimpleSource, GDAL's faster path (a
            // ComplexSource masks it pixel by pixel, a reduction over 48 dates of 1024² read 5× slower)
            let no_data = m.no_data.filter(|nd| !nd.is_nan());
            let kind = if scaled || no_data.is_some() { "ComplexSource" } else { "SimpleSource" };
            let rs = s
                .resampling
                .as_deref()
                .filter(|r| *r != "nearest")
                .map_or(String::new(), |r| format!(" resampling=\"{r}\""));
            let _ = writeln!(x, "    <{kind}{rs}>");
            let _ = writeln!(x, "      <SourceFilename relativeToVRT=\"0\">{}</SourceFilename>", xml_escape(&s.path));
            let _ = writeln!(x, "      <SourceBand>{}</SourceBand>", s.band);
            if let Some((src, dst)) = s.rects {
                let r = |a: [f64; 4]| {
                    format!(
                        "xOff=\"{}\" yOff=\"{}\" xSize=\"{}\" ySize=\"{}\"",
                        g17(a[0]),
                        g17(a[1]),
                        g17(a[2]),
                        g17(a[3])
                    )
                };
                let _ = writeln!(x, "      <SrcRect {} />\n      <DstRect {} />", r(src), r(dst));
            }
            if scaled {
                let _ = writeln!(
                    x,
                    "      <ScaleOffset>{}</ScaleOffset>\n      <ScaleRatio>{}</ScaleRatio>",
                    g17(m.offset),
                    g17(m.scale)
                );
            }
            if let Some(nd) = no_data {
                let _ = writeln!(x, "      <NODATA>{}</NODATA>", g17(nd));
            }
            let _ = writeln!(x, "    </{kind}>");
        }
        x += "  </VRTRasterBand>\n";
    }
    x + "</VRTDataset>\n"
}

/// The arguments of a band computed at a date.
pub fn date_args(expr: &str, inputs: usize, vars: &DateVars) -> Vec<(String, String)> {
    let mut a = vec![("expr".to_string(), expr.to_string()), ("n".to_string(), inputs.to_string())];
    for (i, v) in super::program::DateVar::ALL.iter().enumerate() {
        if !vars.0[i].is_nan() {
            a.push((v.name().to_string(), format!("{}", vars.0[i])));
        }
    }
    a
}

/// The side of the blocks of a band of `sources` sources: their buffers together within ~32 MB (a large window is
/// read in stripes of block rows on up to 8 threads, each with a block's buffers). Smaller blocks cost little more
/// (a reduction over 107 dates of 877 × 886: 2.8 s on one thread with blocks of 112 to 512) once GDAL keeps the
/// sources open (`register`).
pub fn block_side(sources: usize) -> usize {
    let px = (32usize << 20) / (4 * sources.max(1));
    let side = (px as f64).sqrt() as usize;
    side.clamp(64, 512) / 16 * 16
}

/// The blocks of a band of these sources on the grid: squares of block_side, or, when its first source is the whole
/// of a file in strips as wide as the grid (a GeoTIFF of rows, a VRT of such strips), strips as wide, of as many rows
/// as fit in the same memory. A square block of a file in strips decompresses its rows whole: each row of a grid
/// 7318 wide was decompressed 17 times over (blocks of 448), a reduction over 41 such dates read for minutes.
pub fn block_shape(grid: &Grid, sources: &[Option<Source>]) -> (usize, usize) {
    let n = sources.len();
    let side = block_side(n);
    let strips = sources.iter().flatten().find(|s| s.rects.is_none()).and_then(|s| {
        let ds = with_block_reads(is_calc_vrt(&s.path), || crate::open_raster(&s.path)).ok()?;
        let (bx, by) = ds.rasterband(s.band as usize).ok()?.block_size();
        (bx == ds.raster_size().0 && bx == grid.w && by < bx).then_some(by.max(1))
    });
    match strips {
        Some(by) if grid.w > side => {
            let rows = ((32usize << 20) / (4 * n.max(1)) / grid.w).clamp(1, 512);
            (grid.w, (rows / by).max(1) * by)
        }
        _ => (side, side),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_as_numpy() {
        assert_eq!(percentile(&mut [3.0, 1.0, 2.0, 4.0], 0.5), 2.5);
        assert_eq!(percentile(&mut [5.0], 0.9), 5.0);
        assert!((percentile(&mut [1.0, 2.0, 3.0, 4.0, 5.0], 0.9) - 4.6).abs() < 1e-6);
        assert!(percentile(&mut [], 0.5).is_nan());
        assert_eq!(block_side(1), 512);
        assert_eq!(block_side(428), 128);
    }
}
