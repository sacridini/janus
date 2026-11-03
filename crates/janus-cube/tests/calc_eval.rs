//! The calculator's VRTs read by Janus itself (calc::eval) against GDAL reading them, bit for bit: every kind of
//! source the calculator writes (a scaled UInt16 with no data, Float32 with NaN, Int16 and Byte with no data,
//! Int32 scaled, Float64), reductions and the VRTs of reductions they read, lags, a layer on another grid
//! (resampled) and one in another CRS (warped), at full resolution, at smaller and larger sizes, in windows,
//! through GDAL's derived band and through its blocks; and the cube reader with and without it.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gdal::raster::{Buffer, GdalType, RasterCreationOptions};
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager, Metadata};
use janus_cube::calc::eval::{self, How, Miss};
use janus_cube::calc::plan::{CalcLayer, Options, alias_of, plan};
use janus_cube::{BandSelection, open_cube, with_thread_reader};

const W: usize = 700;
const H: usize = 530;
const GT: [f64; 6] = [500000.0, 10.0, 0.0, 8900000.0, 0.0, -10.0];

/// A pseudo-random value in 0..1 of (k, x, y).
fn noise(k: usize, x: usize, y: usize) -> f64 {
    let mut h = (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (x as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= (y as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h % 1_000_000) as f64 / 1_000_000.0
}

/// A tiled, compressed GeoTIFF of `names.len()` bands of type T: band k at (x, y) from `f`, its no data, scale and
/// offset, overviews at 2 and 4 if `ovr`.
#[allow(clippy::too_many_arguments)]
fn tif<T: GdalType + Copy>(
    path: &Path,
    epsg: u32,
    gt: [f64; 6],
    (w, h): (usize, usize),
    names: &[&str],
    nodata: Option<f64>,
    scale: Option<(f64, f64)>,
    ovr: bool,
    f: impl Fn(usize, usize, usize) -> T,
) {
    let d = DriverManager::get_driver_by_name("GTiff").unwrap();
    let co = RasterCreationOptions::from_iter(["TILED=YES", "BLOCKXSIZE=256", "BLOCKYSIZE=256", "COMPRESS=DEFLATE"]);
    let mut ds = d.create_with_band_type_with_options::<T, _>(path, w, h, names.len(), &co).unwrap();
    ds.set_geo_transform(&gt).unwrap();
    ds.set_spatial_ref(&SpatialRef::from_epsg(epsg).unwrap()).unwrap();
    for (k, name) in names.iter().enumerate() {
        let mut b = ds.rasterband(k + 1).unwrap();
        b.set_no_data_value(nodata).unwrap();
        b.set_description(name).unwrap();
        if let Some((o, s)) = scale {
            b.set_offset(o).unwrap();
            b.set_scale(s).unwrap();
        }
        let v: Vec<T> = (0..w * h).map(|i| f(k, i % w, i / w)).collect();
        let mut buf = Buffer::new((w, h), v);
        b.write((0, 0), (w, h), &mut buf).unwrap();
    }
    if ovr {
        ds.build_overviews("NEAREST", &[2, 4], &[]).unwrap();
    }
}

fn layer(name: &str, files: &[String], taken: &[CalcLayer]) -> CalcLayer {
    let info = open_cube(files, &BandSelection { chosen: true, ..Default::default() })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let t: Vec<String> = taken.iter().map(|l| l.alias.clone()).collect();
    CalcLayer { name: name.into(), alias: alias_of(name, &t), info: Arc::new(info) }
}

fn layers(dir: &Path) -> Vec<CalcLayer> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let p = |n: &str| dir.join(n).to_string_lossy().to_string();
    let mut out = Vec::new();
    // a series of reflectances: UInt16 scaled by 0.0001, no data 0 (a pixel in 20), overviews on some dates
    let mut s = Vec::new();
    for (d, day) in ["20230105", "20230301", "20230620", "20231011", "20240115", "20240402"].iter().enumerate() {
        let f = p(&format!("s_{day}.tif"));
        tif::<u16>(Path::new(&f), 32720, GT, (W, H), &["Red", "NIR"], Some(0.0), Some((0.0, 0.0001)), d % 2 == 0, {
            move |k, x, y| {
                let v = noise(k * 10 + d, x, y);
                if v < 0.05 { 0 } else { (500.0 + 6000.0 * v) as u16 }
            }
        });
        s.push(f);
    }
    out.push(layer("s", &s, &out));
    // Float32 with NaN as no data (simple sources)
    let mut fl = Vec::new();
    for (d, day) in ["20230106", "20230302", "20230621", "20231012", "20240116"].iter().enumerate() {
        let f = p(&format!("f_{day}.tif"));
        tif::<f32>(Path::new(&f), 32720, GT, (W, H), &["Value"], Some(f64::NAN), None, false, move |_, x, y| {
            let v = noise(100 + d, x, y);
            if v < 0.1 { f32::NAN } else { (v * 3.0 - 1.0) as f32 }
        });
        fl.push(f);
    }
    out.push(layer("f", &fl, &out));
    // images: Int16 and Byte with no data (GDAL's fast path), Int32 scaled (Float64 working type), Float64
    let f = p("i16.tif");
    tif::<i16>(Path::new(&f), 32720, GT, (W, H), &["A"], Some(-9999.0), None, false, |_, x, y| {
        if noise(200, x, y) < 0.1 { -9999 } else { (noise(201, x, y) * 2000.0 - 1000.0) as i16 }
    });
    out.push(layer("i16", &[f], &out));
    let f = p("byte.tif");
    tif::<u8>(Path::new(&f), 32720, GT, (W, H), &["B"], Some(255.0), None, false, |_, x, y| {
        if noise(300, x, y) < 0.1 { 255 } else { (noise(301, x, y) * 200.0) as u8 }
    });
    out.push(layer("byte", &[f], &out));
    let f = p("i32.tif");
    tif::<i32>(Path::new(&f), 32720, GT, (W, H), &["C"], Some(-1.0), Some((10.0, 0.5)), false, |_, x, y| {
        if noise(400, x, y) < 0.1 { -1 } else { (noise(401, x, y) * 1e7) as i32 }
    });
    out.push(layer("i32", &[f], &out));
    let f = p("f64.tif");
    tif::<f64>(Path::new(&f), 32720, GT, (W, H), &["D"], Some(-9999.0), None, false, |_, x, y| {
        if noise(500, x, y) < 0.1 { -9999.0 } else { noise(501, x, y) * 1e3 }
    });
    out.push(layer("f64", &[f], &out));
    // an image with overviews alone in its band (GDAL reads it smaller through virtual overviews)
    let f = p("ovr.tif");
    tif::<f32>(Path::new(&f), 32720, GT, (W, H), &["E"], Some(-9999.0), None, true, |_, x, y| {
        (noise(600, x, y) * 10.0) as f32
    });
    out.push(layer("ovr", &[f], &out));
    // a coarser grid, shifted by 15 m (a window of it, resampled), and an image in lon/lat (warped)
    let f = p("coarse.tif");
    let gt = [GT[0] - 15.0, 20.0, 0.0, GT[3] + 15.0, 0.0, -20.0];
    tif::<f32>(Path::new(&f), 32720, gt, (W / 2 + 2, H / 2 + 2), &["G"], Some(-9999.0), None, false, |_, x, y| {
        (x + 3 * y) as f32 * 0.25
    });
    out.push(layer("coarse", &[f], &out));
    let f = p("wgs.tif");
    let gt = [-63.001, 0.0002, 0.0, -9.799, 0.0, -0.0002];
    tif::<f32>(Path::new(&f), 4326, gt, (420, 300), &["H"], Some(-9999.0), None, false, |_, x, y| (x * 7 + y) as f32);
    out.push(layer("wgs", &[f], &out));
    out
}

/// GDAL's read of a VRT's band, opened as the reader opens it (with GDAL_FORCE_CACHING for `Cached`), as
/// GDALRasterIOEx with the floating window if any.
fn gdal_read(
    path: &str,
    how: How,
    win: (usize, usize, usize, usize),
    fwin: Option<[f64; 4]>,
    (bw, bh): (usize, usize),
) -> Vec<f32> {
    let cached = how == How::Cached;
    janus_cube::calc::vrt::with_block_reads(cached, || {
        let ds = Dataset::open(path).unwrap();
        let b = ds.rasterband(1).unwrap();
        let mut out = vec![0f32; bw * bh];
        let f = fwin.unwrap_or([0.0; 4]);
        let mut extra = gdal_sys::GDALRasterIOExtraArg {
            nVersion: 1,
            eResampleAlg: gdal_sys::GDALRIOResampleAlg::GRIORA_NearestNeighbour,
            pfnProgress: None,
            pProgressData: std::ptr::null_mut(),
            bFloatingPointWindowValidity: i32::from(fwin.is_some()),
            dfXOff: f[0],
            dfYOff: f[1],
            dfXSize: f[2],
            dfYSize: f[3],
        };
        // SAFETY: a live band, a buffer of bw × bh floats.
        let err = unsafe {
            gdal_sys::GDALRasterIOEx(
                b.c_rasterband(),
                gdal_sys::GDALRWFlag::GF_Read,
                win.0 as i32,
                win.1 as i32,
                win.2 as i32,
                win.3 as i32,
                out.as_mut_ptr().cast(),
                bw as i32,
                bh as i32,
                gdal_sys::GDALDataType::GDT_Float32,
                0,
                0,
                &mut extra,
            )
        };
        assert_eq!(err, gdal_sys::CPLErr::CE_None, "{path}");
        out
    })
}

/// The same bits (any NaN as any other).
fn same(a: &[f32], b: &[f32]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| !(x.to_bits() == y.to_bits() || x.is_nan() && y.is_nan()))
}

type Request = ((usize, usize, usize, usize), Option<[f64; 4]>, (usize, usize));

fn requests() -> Vec<Request> {
    vec![
        ((0, 0, W, H), None, (W, H)),
        ((13, 7, 333, 211), None, (333, 211)),
        ((5, 9, 1, 1), None, (1, 1)),
        ((W - 1, H - 1, 1, 1), None, (1, 1)),
        ((0, 0, W, H), None, (W / 2, H / 2)),
        ((0, 0, W, H), None, (W / 3, H / 3)),
        ((0, 0, W, H), None, (W / 7, 101)),
        ((17, 3, 401, 299), None, (100, 77)),
        ((10, 10, 20, 15), None, (80, 60)),
        ((0, 12, W, 101), Some([0.0, 12.37, W as f64, 100.5]), (W / 2, 40)),
        ((0, 100, W, 64), Some([0.0, 100.0, W as f64, 64.0]), (W, 64)),
        ((0, 0, W, H), None, (W, H / 2)),
    ]
}

/// Each request of each VRT through the reader here and through GDAL, both ways of opening; the requests it
/// leaves to GDAL (counted). With `virtual_overviews` (a band of one source with overviews), a request at a smaller
/// size GDAL reads through the VRT's virtual overviews, without the pixel function: there it is checked by
/// `virtual_overviews_left_out`, not against GDAL.
fn compare(files: &[String], what: &str, virtual_overviews: bool) -> usize {
    let mut not_covered = 0;
    for f in files {
        let v = eval::open(f).unwrap_or_else(|| panic!("{what}: {f} not read here"));
        for how in [How::Direct, How::Cached] {
            for &(win, fwin, size) in &requests() {
                if virtual_overviews && (size.0 < win.2 || size.1 < win.3) {
                    continue;
                }
                let want = gdal_read(f, how, win, fwin, size);
                let mut got = vec![0f32; size.0 * size.1];
                match v.read(1, win, fwin, size, how, &mut got) {
                    Ok(()) => {
                        if let Some(i) = same(&got, &want) {
                            panic!(
                                "{what}: {f} {how:?} {win:?} {fwin:?} -> {size:?}: at {i} ({}, {}) {} here, {} by GDAL",
                                i % size.0,
                                i / size.0,
                                got[i],
                                want[i]
                            );
                        }
                    }
                    Err(Miss::NotCovered) => not_covered += 1,
                    Err(Miss::Failed(e)) => panic!("{what}: {f}: {e}"),
                }
            }
        }
    }
    not_covered
}

fn written(layers: &[CalcLayer], text: &str, dir: &Path, opts: &Options) -> Vec<String> {
    let p = plan(text, layers, opts).unwrap_or_else(|e| panic!("{text}: {}", e.message));
    let _ = std::fs::remove_dir_all(dir);
    p.write(text, layers, opts, dir, &|_, _| {}, &AtomicBool::new(false)).unwrap().files
}

/// One test: the reader's part turns the reader here off a moment, for the whole process.
#[test]
fn the_same_values_as_gdal() {
    values_as_gdal();
    the_reader_the_same_with_and_without_it();
    written_and_pixels_the_same_with_and_without_it();
    eval::close_files();
}

/// What a GeoTIFF holds that GDAL reports: grid, CRS, metadata, and each band's (and overview's) type, no data,
/// description, metadata, blocks and values.
fn tif_summary(path: &str) -> (String, Vec<Vec<u32>>) {
    let ds = Dataset::open(path).unwrap();
    let mut s = format!("{:?} {:?} {}\n", ds.raster_size(), ds.geo_transform().ok(), ds.projection());
    let mut values = Vec::new();
    fn md(m: &impl Metadata) -> String {
        // (not the derived datasets GDAL offers: they name the file)
        let mut v: Vec<String> = m
            .metadata()
            .filter(|e| e.domain != "DERIVED_SUBDATASETS")
            .map(|e| format!("{}:{}={}", e.domain, e.key, e.value))
            .collect();
        v.sort();
        v.join(";")
    }
    s += &md(&ds);
    for i in 1..=ds.raster_count() {
        let b = ds.rasterband(i).unwrap();
        let mut bands = vec![b];
        for o in 0..bands[0].overview_count().unwrap() {
            bands.push(bands[0].overview(o as usize).unwrap());
        }
        for b in &bands {
            let (w, h) = b.size();
            s += &format!(
                "\n{i}: {w}x{h} {:?} {:?} {:?} {:?} {}",
                b.band_type(),
                b.no_data_value().map(f64::to_bits),
                b.description().ok(),
                b.block_size(),
                md(b)
            );
            let v = b.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
            values.push(v.data().iter().map(|x| x.to_bits()).collect());
        }
    }
    (s, values)
}

/// A pointwise result written as a COG (Janus computes it into a GeoTIFF first) and one that is not (GDAL), the
/// same as GDAL writes them; a series' values at a pixel (Live's chart) the same.
fn written_and_pixels_the_same_with_and_without_it() {
    let dir = std::env::temp_dir().join("janus_calc_eval_cog");
    let ls = layers(&dir.join("data"));
    eval::set_threads(3);
    for (k, text) in ["median(nd(s.N, s.R))", "s.R + coarse", "s.N * 2 + wgs"].iter().enumerate() {
        let files = written(&ls, text, &dir.join(format!("calc{k}")), &Options::default());
        let some = vec![files[0].clone()];
        let cog = |own: bool| {
            eval::set_enabled(own);
            let out = dir.join(format!("cog{k}_{own}"));
            let _ = std::fs::remove_dir_all(&out);
            let t = janus_cube::stac_vrt::download(&some, &out, &|_, _| {}, &AtomicBool::new(false)).unwrap();
            eval::set_enabled(true);
            assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1, "{text}: the COG alone left");
            tif_summary(&t[0])
        };
        let (own, by_gdal) = (cog(true), cog(false));
        assert_eq!(own.0, by_gdal.0, "{text}: the COG's description");
        assert_eq!(own.1, by_gdal.1, "{text}: the COG's values");
        for (x, y) in [(0, 0), (5, 9), (W - 1, H - 1), (W, 3)] {
            eval::set_enabled(false);
            let want = janus_cube::calc::live::read_pixel(&files, x, y);
            eval::set_enabled(true);
            let got = janus_cube::calc::live::read_pixel(&files, x, y);
            assert_eq!(same(&got, &want), None, "{text}: pixel ({x}, {y})");
        }
    }
}

fn values_as_gdal() {
    eval::set_threads(3);
    let dir = std::env::temp_dir().join("janus_calc_eval");
    let ls = layers(&dir.join("data"));
    let bilinear = Options { resampling: "bilinear".into(), ..Default::default() };
    for (k, (text, opts, virtual_overviews)) in [
        ("nd(s.N, s.R)", Options::default(), false),
        ("count(nd(s.N, s.R) > 0.2)", Options::default(), false),
        ("median(s.N)", Options::default(), false),
        ("s.N - mean(s.N)", Options::default(), false),
        // the first date: its one source, with overviews (GDAL reads it smaller through virtual overviews)
        ("s.N - s.N[-1]", Options::default(), true),
        ("max(f) - min(f)", Options::default(), false),
        ("std(f)", Options::default(), false),
        ("i16 + byte * 2 + i32 / 1000 + f64", Options::default(), false),
        ("s.R + coarse", bilinear.clone(), false),
        ("mean(s.R + coarse)", bilinear, false),
        ("s.N * 2 + wgs", Options::default(), false),
        ("ovr * 2", Options::default(), true),
        ("mean(s.N) by year", Options::default(), false),
    ]
    .into_iter()
    .enumerate()
    {
        let files = written(&ls, text, &dir.join(format!("calc{k}")), &opts);
        // the first and last dates (each reads every source of its date)
        let some: Vec<String> = [files.first(), files.last()].into_iter().flatten().cloned().collect();
        assert_eq!(compare(&some, text, virtual_overviews), 0, "{text}: every request read here");
        if text == "ovr * 2" {
            virtual_overviews_left_out(&some[0], &dir.join("data").join("ovr.tif").to_string_lossy());
        }
    }
}

/// `ovr * 2` read at a smaller size: GDAL reads the VRT's virtual overviews, `ovr` itself (no pixel function);
/// here it is computed. Through the derived band each source is read at the smaller size (`ovr`'s overviews), so
/// it is twice `ovr` read so; through the blocks, the blocks at full resolution sampled, as GDAL does when the
/// source has no overviews (opened with OVERVIEW_LEVEL=NONE).
fn virtual_overviews_left_out(vrt: &str, ovr: &str) {
    let (win, size) = ((0, 0, W, H), (W / 2, H / 2));
    let v = eval::open(vrt).unwrap();
    let read = |how| {
        let mut got = vec![0f32; size.0 * size.1];
        v.read(1, win, None, size, how, &mut got).unwrap_or_else(|_| panic!("ovr * 2 {how:?}"));
        got
    };
    let src: Vec<f32> = Dataset::open(ovr)
        .unwrap()
        .rasterband(1)
        .unwrap()
        .read_as::<f32>((0, 0), (W, H), size, None)
        .unwrap()
        .data()
        .iter()
        .map(|&x| if x == -9999.0 { f32::NAN } else { x * 2.0 })
        .collect();
    let by_gdal = gdal_read(vrt, How::Direct, win, None, size);
    assert!(same(&by_gdal, &src).is_some(), "GDAL computes `ovr * 2` smaller: the quirk is gone, see eval's doc");
    assert_eq!(same(&read(How::Direct), &src), None, "ovr * 2 Direct, smaller");
    let plain = vrt.replace(".vrt", "_no_overviews.vrt");
    let xml = std::fs::read_to_string(vrt).unwrap().replace(
        "</SourceFilename>",
        "</SourceFilename><OpenOptions><OOI key=\"OVERVIEW_LEVEL\">NONE</OOI></OpenOptions>",
    );
    std::fs::write(&plain, xml).unwrap();
    assert_eq!(same(&read(How::Cached), &gdal_read(&plain, How::Cached, win, None, size)), None, "ovr * 2 Cached");
}

fn the_reader_the_same_with_and_without_it() {
    let dir = std::env::temp_dir().join("janus_calc_eval_reader");
    let ls = layers(&dir.join("data"));
    for (k, text) in
        ["count(nd(s.N, s.R) > 0.2)", "s.N - mean(s.N)", "s.R + wgs", "i16 + byte + i32 + f64"].iter().enumerate()
    {
        let files = written(&ls, text, &dir.join(format!("calc{k}")), &Options::default());
        let info = Arc::new(open_cube(&files, &BandSelection { chosen: true, ..Default::default() }).unwrap());
        for threads in [1, 3] {
            eval::set_threads(threads);
            for &(win, _, size) in requests().iter().filter(|r| r.1.is_none()) {
                let read = |own: bool| {
                    eval::set_enabled(own);
                    janus_cube::drop_thread_reader();
                    let mut v = vec![0f32; size.0 * size.1];
                    let t = info.t() - 1;
                    assert!(with_thread_reader(&info, |r| r.read_window(t, win, &mut v, size)), "{text}");
                    eval::set_enabled(true);
                    v
                };
                let (own, by_gdal) = (read(true), read(false));
                if let Some(i) = same(&own, &by_gdal) {
                    panic!(
                        "{text}, {threads} threads, {win:?} -> {size:?}: at {i} {} here, {} by GDAL",
                        own[i], by_gdal[i]
                    );
                }
            }
        }
    }
}
