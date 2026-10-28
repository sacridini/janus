//! The raster calculator over local files made here: a layer's own expression (read by the reader and by Zeit's
//! VRT), and the calculator's VRTs over several layers: a series by an image, two series matched by date, a layer
//! on a coarser grid and one in another CRS, a stack's band, the rules, the reductions over time.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{DriverManager, Metadata};
use janus_cube::calc::ExprId;
use janus_cube::calc::plan::{CalcLayer, DateMatch, Options, alias_of, plan};
use janus_cube::{BandSelection, CubeInfo, open_cube, with_thread_reader};

const W: usize = 40;
const H: usize = 30;
const GT: [f64; 6] = [500000.0, 10.0, 0.0, 8900000.0, 0.0, -10.0];
const ND: f64 = -9999.0;

/// A Float32 GeoTIFF: band k's value at (x, y) from `f(k, x, y)`, its names, no data -9999.
fn tif(
    path: &Path,
    epsg: u32,
    gt: [f64; 6],
    (w, h): (usize, usize),
    names: &[&str],
    f: impl Fn(usize, usize, usize) -> f32,
) {
    let d = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = d.create_with_band_type::<f32, _>(path, w, h, names.len()).unwrap();
    ds.set_geo_transform(&gt).unwrap();
    ds.set_spatial_ref(&SpatialRef::from_epsg(epsg).unwrap()).unwrap();
    for (k, name) in names.iter().enumerate() {
        let mut b = ds.rasterband(k + 1).unwrap();
        b.set_no_data_value(Some(ND)).unwrap();
        b.set_description(name).unwrap();
        let v: Vec<f32> = (0..w * h).map(|i| f(k, i % w, i / w)).collect();
        let mut buf = Buffer::new((w, h), v);
        b.write((0, 0), (w, h), &mut buf).unwrap();
    }
}

/// The values of a one-band raster.
fn read(path: &str) -> (Vec<f32>, usize, usize) {
    let ds = gdal::Dataset::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (w, h) = ds.raster_size();
    let b = ds.rasterband(1).unwrap();
    (b.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec(), w, h)
}

fn red(d: usize) -> f32 {
    0.1 * (d + 1) as f32
}

fn nir(d: usize, x: usize) -> f32 {
    0.3 + 0.01 * x as f32 + 0.05 * d as f32
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5 || (a.is_nan() && b.is_nan())
}

struct Data {
    s2: Vec<String>,
    dem: String,
    l8: Vec<String>,
    wgs: String,
    stk: String,
}

fn data(dir: &Path) -> Data {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let p = |n: &str| dir.join(n).to_string_lossy().to_string();
    // a series of 3 dates of red and NIR (10 m); date 2's red has no data at (0, 0)
    let mut s2 = Vec::new();
    for (d, day) in ["20240701", "20240711", "20240721"].iter().enumerate() {
        let f = p(&format!("s2_{day}.tif"));
        tif(Path::new(&f), 32720, GT, (W, H), &["Red", "NIR"], |k, x, y| {
            if k == 0 { if d == 1 && x == 0 && y == 0 { ND as f32 } else { red(d) } } else { nir(d, x) }
        });
        s2.push(f);
    }
    // an image on the same grid: its row
    let dem = p("dem.tif");
    tif(Path::new(&dem), 32720, GT, (W, H), &["Elevation"], |_, _, y| y as f32);
    // a series of 2 dates on a 20 m grid: 100, 200
    let mut l8 = Vec::new();
    for (d, day) in ["20240702", "20240720"].iter().enumerate() {
        let f = p(&format!("l8_{day}.tif"));
        let gt = [GT[0], 20.0, 0.0, GT[3], 0.0, -20.0];
        tif(Path::new(&f), 32720, gt, (W / 2, H / 2), &["Value"], |_, _, _| 100.0 * (d + 1) as f32);
        l8.push(f);
    }
    // an image in lon/lat over the area: 5
    let wgs = p("wgs.tif");
    tif(Path::new(&wgs), 4326, [-63.0, 0.001, 0.0, -9.8, 0.0, -0.001], (200, 200), &["Five"], |_, _, _| 5.0);
    // a stack: one file of three bands, 1, 2, 3
    let stk = p("stack.tif");
    tif(Path::new(&stk), 32720, GT, (W, H), &["alpha", "beta", "gamma"], |k, _, _| (k + 1) as f32);
    Data { s2, dem, l8, wgs, stk }
}

fn layers(d: &Data) -> Vec<CalcLayer> {
    let mut out: Vec<CalcLayer> = Vec::new();
    for (name, files) in [
        ("s2", d.s2.clone()),
        ("dem", vec![d.dem.clone()]),
        ("l8", d.l8.clone()),
        ("wgs", vec![d.wgs.clone()]),
        ("stk", vec![d.stk.clone()]),
    ] {
        let info = open_cube(&files, &BandSelection::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let taken: Vec<String> = out.iter().map(|l| l.alias.clone()).collect();
        out.push(CalcLayer { name: name.into(), alias: alias_of(name, &taken), info: Arc::new(info) });
    }
    out
}

/// The result of `text` over the layers: each date's values.
fn run(text: &str, layers: &[CalcLayer], opts: &Options, dir: &Path) -> Vec<Vec<f32>> {
    let p = plan(text, layers, opts).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    let w =
        p.write(text, layers, opts, dir, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{text}: {e}"));
    w.files
        .iter()
        .map(|f| {
            let (v, w, h) = read(f);
            assert_eq!((w, h), (W, H), "{text}: the grid of the series");
            v
        })
        .collect()
}

#[test]
fn a_layer_expression() {
    let dir = std::env::temp_dir().join("janus_calc_layer");
    let d = data(&dir);
    let sel = BandSelection { expr: ExprId::intern("NDVI", "(N - R) / (N + R)"), chosen: true, ..Default::default() };
    let info: Arc<CubeInfo> = Arc::new(open_cube(&d.s2, &sel).expect("open"));
    assert_eq!(info.t(), 3);
    assert!(info.expr.is_some());
    let mut v = vec![0f32; W * H];
    let ndvi = |d: usize, x: usize| (nir(d, x) - red(d)) / (nir(d, x) + red(d));
    for t in 0..3 {
        assert!(with_thread_reader(&info, |r| r.read_window(t, (0, 0, W, H), &mut v, (W, H))));
        for (i, &x) in v.iter().enumerate() {
            let want = if t == 1 && i == 0 { f32::NAN } else { ndvi(t, i % W) };
            assert!(close(x, want), "date {t}, pixel {i}: {x} vs {want}");
        }
    }
    // a half-size read (an overview)
    let mut s = vec![0f32; (W / 2) * (H / 2)];
    assert!(with_thread_reader(&info, |r| r.read_window(2, (0, 0, W, H), &mut s, (W / 2, H / 2))));
    assert!(s.iter().all(|x| x.is_finite()));
    // what Zeit reads: a derived band per date
    let vrt = dir.join("zeit.vrt").to_string_lossy().to_string();
    janus_cube::vrt::write_cube_vrt(&info, &vrt, 0, false).unwrap();
    let ds = gdal::Dataset::open(&vrt).unwrap();
    assert_eq!(ds.raster_count(), 3);
    for t in 0..3 {
        let b = ds.rasterband(t + 1).unwrap();
        let x = b.read_as::<f32>((0, 0), (W, H), (W, H), None).unwrap();
        for (i, &x) in x.data().iter().enumerate() {
            let want = if t == 1 && i == 0 { f32::NAN } else { ndvi(t, i % W) };
            assert!(close(x, want), "VRT date {t}, pixel {i}: {x} vs {want}");
        }
    }
}

#[test]
fn across_layers() {
    let dir = std::env::temp_dir().join("janus_calc_layers");
    let d = data(&dir);
    let ls = layers(&d);
    assert_eq!(ls.iter().map(|l| l.alias.as_str()).collect::<Vec<_>>(), ["s2", "dem", "l8", "wgs", "stk"]);
    let opts = Options::default();
    let out = |n: &str| dir.join("out").join(n);

    // a series by an image: the image at every date
    let r = run("(s2.nir - s2.red) / (s2.nir + s2.red) * dem", &ls, &opts, &out("a"));
    assert_eq!(r.len(), 3);
    for (t, v) in r.iter().enumerate() {
        for (i, &x) in v.iter().enumerate() {
            let (px, py) = (i % W, i / W);
            let want =
                if t == 1 && i == 0 { f32::NAN } else { (nir(t, px) - red(t)) / (nir(t, px) + red(t)) * py as f32 };
            assert!(close(x, want), "a: date {t}, ({px}, {py}): {x} vs {want}");
        }
    }

    // two series: l8's dates matched to s2's within 8 days (07-11 is 9 days from both: no data); l8 on 20 m cells
    let r = run("s2.red + l8", &ls, &opts, &out("b"));
    assert_eq!(r.len(), 3);
    assert!(r[0].iter().all(|&x| close(x, red(0) + 100.0)), "b: date 1 {:?}", &r[0][..4]);
    assert!(r[1].iter().all(|x| x.is_nan()), "b: date 2 has no l8 date");
    assert!(r[2].iter().all(|&x| close(x, red(2) + 200.0)), "b: date 3");
    // within 10 days: date 2 takes the nearest
    let o = Options { matching: DateMatch::Nearest(10.0), ..Options::default() };
    let r = run("s2.red + l8", &ls, &o, &out("b10"));
    assert!(r[1][1..].iter().all(|&x| close(x, red(1) + 100.0) || close(x, red(1) + 200.0)), "b10: {:?}", &r[1][..4]);

    // a layer in another CRS, warped onto the grid
    let r = run("dem + wgs", &ls, &opts, &out("c"));
    assert_eq!(r.len(), 1);
    assert!(r[0].iter().enumerate().all(|(i, &x)| close(x, 5.0 + (i / W) as f32)), "c: {:?}", &r[0][..4]);
    // the grid of the layer chosen, not the first named
    let o = Options { grid_from: Some(1), ..Options::default() };
    let r = run("wgs + dem", &ls, &o, &out("c2"));
    assert!(r[0].iter().enumerate().all(|(i, &x)| close(x, 5.0 + (i / W) as f32)), "c2: {:?}", &r[0][..4]);

    // a stack's band is one image, by name or by place
    for text in ["stk.beta * s2.red", "stk.band2 * s2.red", "stk[2] * s2.red"] {
        let r = run(text, &ls, &opts, &out("d"));
        assert_eq!(r.len(), 3, "{text}");
        assert!(r[2].iter().all(|&x| close(x, 2.0 * red(2))), "{text}: {:?}", &r[2][..4]);
    }

    // rules: the result starts from the first operand of the condition
    let r = run("s2.red > 0.15 = 1\nresult == nan = -1", &ls, &opts, &out("e"));
    assert!(r[0].iter().all(|&x| close(x, red(0))));
    assert_eq!(r[1][0], -1.0);
    assert!(r[1][1..].iter().chain(&r[2]).all(|&x| x == 1.0));

    // reductions over time: an image each
    let r = run("max(s2.nir)", &ls, &opts, &out("f"));
    assert_eq!(r.len(), 1);
    assert!(r[0].iter().enumerate().all(|(i, &x)| close(x, nir(2, i % W))), "max: {:?}", &r[0][..4]);
    let r = run("mean(s2.red)", &ls, &opts, &out("g"));
    assert!(close(r[0][0], (red(0) + red(2)) / 2.0), "mean skips no data: {}", r[0][0]);
    assert!(close(r[0][1], (red(0) + red(1) + red(2)) / 3.0));
    let r = run("count(s2.red > 0.15)", &ls, &opts, &out("h"));
    assert_eq!((r[0][0], r[0][1]), (1.0, 2.0));
    let r = run("median(s2.red)", &ls, &opts, &out("i"));
    assert!(close(r[0][1], red(1)));
    // an anomaly: each date less the mean of the series
    let r = run("s2.red - mean(s2.red)", &ls, &opts, &out("j"));
    assert_eq!(r.len(), 3);
    assert!(close(r[2][1], red(2) - (red(0) + red(1) + red(2)) / 3.0));
    // several arguments: pixel by pixel
    let r = run("max(s2.red, dem / 100)", &ls, &opts, &out("k"));
    assert_eq!(r.len(), 3);
    assert!(close(r[0][W * 25], 0.25));

    // a date's variables
    let r = run("s2.red * 0 + month * 100 + day", &ls, &opts, &out("l"));
    assert_eq!((r[0][5], r[1][5], r[2][5]), (701.0, 711.0, 721.0));
}

#[test]
fn errors() {
    let dir = std::env::temp_dir().join("janus_calc_errors");
    let d = data(&dir);
    let ls = layers(&d);
    let e = |t: &str| plan(t, &ls, &Options::default()).err().unwrap_or_else(|| panic!("{t} planned")).message;
    assert!(e("s3.red").contains("no layer"), "{}", e("s3.red"));
    assert!(e("s2.swir").contains("no band"), "{}", e("s2.swir"));
    assert!(e("dem.band2").contains("no band"), "{}", e("dem.band2"));
    assert!(e("stk[4]").contains("bands 1 to 3"), "{}", e("stk[4]"));
}

#[test]
fn a_reduction_by_blocks() {
    // 12 dates of 600 × 600: a reduction's band reads its sources a block at a time, not the whole window
    let dir = std::env::temp_dir().join("janus_calc_blocks");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let n = 600;
    let mut files = Vec::new();
    for m in 1..=12 {
        let f = dir.join(format!("m_2024{m:02}15.tif")).to_string_lossy().to_string();
        tif(Path::new(&f), 32720, GT, (n, n), &["Value"], |_, x, _| (m * 1000 + x) as f32);
        files.push(f);
    }
    let info = open_cube(&files, &BandSelection::default()).unwrap();
    let ls = vec![CalcLayer { name: "m".into(), alias: "m".into(), info: Arc::new(info) }];
    let opts = Options::default();
    let p = plan("max(m) - min(m)", &ls, &opts).unwrap();
    let w = p.write("max(m) - min(m)", &ls, &opts, &dir.join("out"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(w.files.len(), 1);
    let v = janus_cube::calc::vrt::with_block_reads(true, || read(&w.files[0]).0);
    assert!(v.iter().all(|&x| x == 11000.0), "{:?}", &v[..4]);
    let largest = janus_cube::calc::vrt::largest_request();
    assert!(largest < n * n * 12, "a call of {largest} values: the window at once");
}

#[test]
fn files_in_strips() {
    // dates in strips of one row, wider than a square block: the reduction's VRT and the result's in strips as wide
    // (a square block decompressed every row once per block across), the values as with squares
    let dir = std::env::temp_dir().join("janus_calc_strips");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (1500, 40);
    let opts = gdal::raster::RasterCreationOptions::from_iter(["BLOCKYSIZE=1", "COMPRESS=LZW"]);
    let mut files = Vec::new();
    for d in 0..6 {
        let f = dir.join(format!("s_{}0101.tif", 2000 + d));
        let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut ds = drv.create_with_band_type_with_options::<f32, _>(&f, w, h, 1, &opts).unwrap();
        ds.set_geo_transform(&GT).unwrap();
        ds.set_spatial_ref(&SpatialRef::from_epsg(32723).unwrap()).unwrap();
        let v: Vec<f32> = (0..w * h).map(|i| (d * d) as f32 + (i % w) as f32 * 0.001).collect();
        ds.rasterband(1).unwrap().write((0, 0), (w, h), &mut Buffer::new((w, h), v)).unwrap();
        files.push(f.to_string_lossy().to_string());
    }
    let info = open_cube(&files, &BandSelection::default()).unwrap();
    let ls = vec![CalcLayer { name: "s".into(), alias: "s".into(), info: Arc::new(info) }];
    let o = Options::default();
    let text = "s - mean(s)";
    let p = plan(text, &ls, &o).unwrap();
    let wr = p.write(text, &ls, &o, &dir.join("out"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let block = |f: &str| {
        let ds = janus_cube::calc::vrt::with_block_reads(true, || gdal::Dataset::open(f).unwrap());
        ds.rasterband(1).unwrap().block_size()
    };
    let stage = dir.join("out").join("stages").join("stage1_mean.vrt");
    let (sx, sy) = block(stage.to_str().unwrap());
    assert_eq!(sx, w, "the reduction's blocks: {sx} x {sy}");
    assert_eq!(block(&wr.files[0]), (w, sy), "the result's blocks as the reduction's");
    // the mean of d² over 0..6 is 55 / 6
    for (d, f) in wr.files.iter().enumerate() {
        let v = janus_cube::calc::vrt::with_block_reads(true, || read(f).0);
        let want = (d * d) as f32 - 55.0 / 6.0;
        assert!(v.iter().all(|&x| close(x, want)), "date {d}: {} for {want}", v[0]);
    }
    // at(): one date as an image, by its year or its day, and in an expression over the series
    type Want = fn(usize) -> f32;
    let cases: [(&str, Want); 2] =
        [("at(s, 2003)", |x| 9.0 + x as f32 * 0.001), ("at(s, 20030101) - at(s, 2001)", |_| 8.0)];
    for (text, want) in cases {
        let p = plan(text, &ls, &o).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        let wr = p.write(text, &ls, &o, &dir.join("at"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(wr.files.len(), 1, "{text}: one image");
        let v = janus_cube::calc::vrt::with_block_reads(true, || read(&wr.files[0]).0);
        assert!(v.iter().enumerate().all(|(i, &x)| close(x, want(i % w))), "{text}: {} {}", v[0], v[w - 1]);
    }
    let p = plan("s - at(s, 2000)", &ls, &o).unwrap();
    assert!(p.series.is_some(), "a series less one of its dates is a series");
    for (text, msg) in [
        ("at(s, 1999)", "has no date in 1999"),
        ("at(s, 200013)", "the date is a year"),
        ("at(s, 0.5)", "the date is a year"),
    ] {
        let e = plan(text, &ls, &o).err().unwrap_or_else(|| panic!("{text} should fail"));
        assert!(e.message.contains(msg), "{text}: {}", e.message);
    }
}

#[test]
fn a_series_as_geotiffs() {
    // the calculator's VRTs of a series written as cloud-optimized GeoTIFFs, several dates at once: in order, the
    // values of the VRTs
    let dir = std::env::temp_dir().join("janus_calc_cogs");
    let d = data(&dir);
    let ls = layers(&d);
    let opts = Options::default();
    let text = "nd(s2.nir, s2.red) * 10";
    let p = plan(text, &ls, &opts).unwrap();
    let w = p.write(text, &ls, &opts, &dir.join("vrt"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let fractions = std::sync::Mutex::new(Vec::new());
    let cogs = janus_cube::stac_vrt::download(
        &w.files,
        &dir.join("cog"),
        &|f, _| fractions.lock().unwrap().push(f),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(cogs.len(), 3);
    for (vrt, cog) in w.files.iter().zip(&cogs) {
        let stem = |p: &str| Path::new(p).file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(stem(vrt), stem(cog));
        let (a, b) = (read(vrt).0, read(cog).0);
        assert!(a.iter().zip(&b).all(|(x, y)| close(*x, *y)), "{cog}");
    }
    assert!(fractions.into_inner().unwrap().last().is_some_and(|f| (f - 1.0).abs() < 1e-9));
    // a date that cannot be read fails the whole
    let mut bad = w.files.clone();
    bad.insert(1, dir.join("none.vrt").to_string_lossy().into_owned());
    assert!(janus_cube::stac_vrt::download(&bad, &dir.join("cog2"), &|_, _| {}, &AtomicBool::new(false)).is_err());
}

/// The time series of `time_ops`: 30 dates 33 and 41 days apart from 2019-01-05 (three years), value
/// ((7d + 3x) mod 11) / 10 at date d and column x, no data at date 5 of column 0.
struct Irregular {
    days: Vec<i64>,
    layers: Vec<CalcLayer>,
}

fn ts_value(d: usize, x: usize) -> f32 {
    if d == 5 && x == 0 { f32::NAN } else { ((d * 7 + x * 3) % 11) as f32 / 10.0 }
}

fn irregular(dir: &Path) -> Irregular {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let mut day = janus_cube::dates::days_from_civil(2019, 1, 5);
    let (mut days, mut files) = (Vec::new(), Vec::new());
    for d in 0..30 {
        let (y, m, dd) = janus_cube::dates::civil_from_days(day);
        let f = dir.join(format!("ts_{y:04}{m:02}{dd:02}.tif")).to_string_lossy().to_string();
        tif(Path::new(&f), 32720, GT, (W, H), &["Value"], |_, x, _| {
            let v = ts_value(d, x);
            if v.is_nan() { ND as f32 } else { v }
        });
        files.push(f);
        days.push(day);
        day += if d % 2 == 0 { 33 } else { 41 };
    }
    let info = open_cube(&files, &BandSelection::default()).unwrap();
    Irregular { days, layers: vec![CalcLayer { name: "ts".into(), alias: "ts".into(), info: Arc::new(info) }] }
}

/// Equal, both no data, or both the same infinity.
fn same(a: f32, b: f32) -> bool {
    close(a, b) || (a.is_infinite() && a == b) || (a - b).abs() < 1e-4 * b.abs().max(1.0)
}

#[test]
fn time_ops() {
    let dir = std::env::temp_dir().join("janus_calc_time");
    let ts = irregular(&dir);
    let (ls, days) = (&ts.layers, &ts.days);
    let opts = Options::default();
    let n = days.len();
    let decyear = |d: usize| janus_cube::dates::decimal_year(days[d] as f64 * 86400.0) as f32;
    let ymd = |d: usize| janus_cube::dates::civil_from_days(days[d]);
    let valid = |x: usize| (0..n).filter(move |&d| !ts_value(d, x).is_nan());
    let cols = [0usize, 1, 4, 7];
    let out = |s: &str| dir.join("out").join(s);
    let check = |text: &str, name: &str, want: &dyn Fn(usize, usize) -> f32| {
        let r = run(text, ls, &opts, &out(name));
        for (t, v) in r.iter().enumerate() {
            for &x in &cols {
                let (got, w) = (v[x], want(t, x));
                assert!(same(got, w), "{text}: date {t}, column {x}: {got} vs {w}");
            }
        }
        r.len()
    };

    // the date of the largest value (the first of equal ones)
    assert_eq!(
        check("argmax(ts)", "argmax", &|_, x| {
            let best = valid(x).fold(None, |b: Option<usize>, d| match b {
                Some(b) if ts_value(b, x) >= ts_value(d, x) => Some(b),
                _ => Some(d),
            });
            decyear(best.unwrap())
        }),
        1
    );
    // a calendar slice: June to September 2019
    let in_slice = |d: usize| {
        let (y, m, _) = ymd(d);
        y == 2019 && (6..=9).contains(&m)
    };
    let median = |mut v: Vec<f32>| janus_cube::calc::vrt::percentile(&mut v, 0.5);
    check("median(ts[2019-06 .. 2019-09])", "slice", &|_, x| {
        median(valid(x).filter(|&d| in_slice(d)).map(|d| ts_value(d, x)).collect())
    });
    // months of every year: November to February
    check("count(isvalid(ts[11 .. 2]))", "months", &|_, x| {
        valid(x).filter(|&d| matches!(ymd(d).1, 11 | 12 | 1 | 2)).count() as f32
    });
    // by year: a series of the years
    let years: Vec<i32> = {
        let mut v: Vec<i32> = (0..n).map(|d| ymd(d).0).collect();
        v.dedup();
        v
    };
    let k = check("max(ts) by year", "by_year", &|t, x| {
        valid(x).filter(|&d| ymd(d).0 == years[t]).map(|d| ts_value(d, x)).fold(f32::NAN, f32::max)
    });
    assert_eq!(k, years.len());
    let text = "max(ts) by year - min(ts) by year";
    let p = plan(text, ls, &opts).unwrap();
    let labels: Vec<String> = p.dates(ls).iter().map(|d| d.1.clone()).collect();
    assert_eq!(labels, years.iter().map(|y| y.to_string()).collect::<Vec<_>>());
    // the series written opens with its years as dates
    let w = p.write(text, ls, &opts, &out("range"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let info = open_cube(&w.files, &BandSelection::default()).unwrap();
    let opened: Vec<&String> = info.layers.iter().map(|l| &l.label).collect();
    assert!(info.time_is_date && info.t() == years.len(), "{opened:?}");
    // by month: one date per month that has one
    let p = plan("mean(ts) by month", ls, &opts).unwrap();
    assert_eq!(p.dates(ls).len(), n, "a date a month here");
    // a moving window of 90 days: the dates in (t - 90, t]
    check("mean(ts, 90d)", "window", &|t, x| {
        let v: Vec<f32> =
            valid(x).filter(|&d| days[d] <= days[t] && days[d] > days[t] - 90).map(|d| ts_value(d, x)).collect();
        if v.is_empty() { f32::NAN } else { v.iter().sum::<f32>() / v.len() as f32 }
    });
    // the previous observation of the pixel (date 5 of column 0 has none: date 4's)
    for (text, name) in [("ts[-1]", "lag1"), ("lag(ts, 1)", "lag1b")] {
        check(text, name, &|t, x| valid(x).rfind(|&d| d < t).map_or(f32::NAN, |d| ts_value(d, x)));
    }
    check("ts - ts[-2]", "lag2", &|t, x| {
        let prev: Vec<usize> = valid(x).filter(|&d| d < t).collect();
        let p = if prev.len() >= 2 { ts_value(prev[prev.len() - 2], x) } else { f32::NAN };
        ts_value(t, x) - p
    });
    // a year before: the nearest date within half the usual step
    let mut steps: Vec<i64> = (1..n).map(|d| days[d] - days[d - 1]).collect();
    steps.sort();
    let tol = (steps[steps.len() / 2] as f64 / 2.0).max(8.0);
    check("lag(ts, 1y)", "lag1y", &|t, x| {
        let (y, m, d) = ymd(t);
        let target = janus_cube::dates::days_from_civil(y - 1, m, d);
        let near = (0..n)
            .filter(|&e| e != t && ((days[e] - target).abs() as f64) <= tol)
            .min_by_key(|&e| (days[e] - target).abs());
        near.map_or(f32::NAN, |e| ts_value(e, x))
    });
    // the anomaly against the months of 2019 and 2020
    check("anomaly(ts, base=2019..2020, by=month)", "anomaly", &|t, x| {
        let m = ymd(t).1;
        let base: Vec<f64> = valid(x)
            .filter(|&d| (2019..=2020).contains(&ymd(d).0) && ymd(d).1 == m)
            .map(|d| ts_value(d, x) as f64)
            .collect();
        if base.is_empty() {
            return f32::NAN;
        }
        let mean = base.iter().sum::<f64>() / base.len() as f64;
        let std = (base.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / base.len() as f64).sqrt();
        // a division by 0 is no data (the calculator's x / 0)
        if std == 0.0 { f32::NAN } else { ((ts_value(t, x) as f64 - mean) / std) as f32 }
    });
    // the reductions read by several dates computed once into GeoTIFFs: the same values
    let mat = Options { materialize: true, ..Default::default() };
    for (k, text) in ["anomaly(ts, base=2019..2020, by=month)", "ts - mean(ts)"].iter().enumerate() {
        let (lazy, done) = (out(&format!("lazy{k}")), out(&format!("mat{k}")));
        let (a, b) = (run(text, ls, &opts, &lazy), run(text, ls, &mat, &done));
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
            assert!(same(*x, *y), "{text}: {x} vs {y}");
        }
        let tifs = std::fs::read_dir(done.join("stages"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tif"))
            .count();
        assert!(tifs > 0, "{text}: no stage computed");
    }
    // live at full size: the reduction read date by date gives the result written; a series' date; a pixel's series
    for text in ["max(ts) - min(ts)", "argmax(ts)", "median(ts[2019-06 .. 2019-12])", "ts - mean(ts)"] {
        let p = plan(text, ls, &opts).unwrap();
        let mut last = None;
        let mut on = |u: janus_cube::calc::live::LiveUpdate| last = Some(u);
        let t = p.series.map(|_| days[3] as f64 * 86400.0);
        let cancel = AtomicBool::new(false);
        janus_cube::calc::live::live(&p, text, ls, &opts, &out("live"), W, t, &cancel, &mut on).unwrap();
        let u = last.unwrap();
        assert_eq!((u.w, u.h, u.done), (W, H, u.total), "{text}");
        let want = run(text, ls, &opts, &out("live_ref"));
        let want = &want[if p.series.is_some() { 3 } else { 0 }];
        for (a, b) in u.data.iter().zip(want) {
            assert!(same(*a, *b), "{text}: {a} vs {b}");
        }
    }
    let w =
        plan("ts * 2", ls, &opts).unwrap().write("ts * 2", ls, &opts, &out("px"), &|_, _| {}, &AtomicBool::new(false));
    let px = janus_cube::calc::live::read_pixel(&w.unwrap().files, 4, 2);
    assert_eq!(px.len(), n);
    for (d, v) in px.iter().enumerate() {
        assert!(same(*v, ts_value(d, 4) * 2.0), "date {d}: {v}");
    }
    // events
    let hi = |d: usize, x: usize| ts_value(d, x) > 0.5;
    check("first(ts > 0.5)", "first", &|_, x| valid(x).find(|&d| hi(d, x)).map_or(f32::NAN, decyear));
    check("last(ts > 0.5)", "last", &|_, x| valid(x).rfind(|&d| hi(d, x)).map_or(f32::NAN, decyear));
    check("first(ts > 0.5 and year >= 2020)", "first2020", &|_, x| {
        valid(x).find(|&d| hi(d, x) && ymd(d).0 >= 2020).map_or(f32::NAN, decyear)
    });
    check("longest(ts > 0.5)", "longest", &|_, x| {
        let (mut best, mut start, mut last) = (0i64, None, 0i64);
        for d in valid(x) {
            last = days[d];
            match (hi(d, x), start) {
                (true, None) => start = Some(days[d]),
                (false, Some(s)) => {
                    best = best.max(days[d] - s);
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            best = best.max(last - s);
        }
        best as f32
    });
    check("persist(ts > 0.5, 2)", "persist", &|t, x| {
        let v: Vec<bool> = valid(x).filter(|&d| d <= t).map(|d| hi(d, x)).collect();
        let k = v.len();
        if k >= 1 && !v[k - 1] || k >= 2 && !v[k - 2] {
            0.0
        } else if k >= 2 {
            1.0
        } else {
            f32::NAN
        }
    });

    // errors
    for (text, msg) in [
        ("ts[2019-06 .. 2019-09]", "goes in a reduction"),
        ("max(ts) by year - ts", "different dates"),
        ("max(ts, 90d) by year", "one or the other"),
        ("median(ts[2019-13 .. 2020-01])", "a date is"),
        ("anomaly(ts, base=2019..2018)", "the base"),
        ("lag(ts, 0.5)", "a lag is"),
        ("mean(ts, 90d, 3)", "the last argument"),
    ] {
        let e = plan(text, ls, &opts).err().unwrap_or_else(|| panic!("{text} should fail"));
        assert!(e.message.contains(msg), "{text}: {}", e.message);
    }
}
