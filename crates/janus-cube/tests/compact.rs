//! Zeit's compact (int8) embedding files read as the float32 files Zeit writes for the same cubes. The fixtures
//! (tests/data/compact, made by make_fixtures.py with zeit.save_raster(compact=True)) come in pairs: the compact
//! file and the float32 file Zeit writes from its decode, so the expected values are Zeit's own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gdal::{GdalOpenFlags, Metadata};
use janus_cube::{BandSelection, CubeInfo, CubeReader, Encoding, open_cube};

const W: usize = 10;
const H: usize = 8;

fn data(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/compact").join(name).to_string_lossy().into_owned()
}

fn open(path: &str) -> CubeInfo {
    open_cube(&[path.to_string()], &BandSelection::default()).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Equal bit for bit, the NaNs equal.
fn same(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!((x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits(), "{what}: pixel {i}: {x} vs {y}");
    }
}

/// Every band of every date, whole and at half size, the shown band, and single pixels: the same from both files.
fn compare(compact: &str, float: &str, encoding: Encoding, dates: usize, dims: usize) {
    let (c, f) = (open(&data(compact)), open(&data(float)));
    assert_eq!(c.encoding, Some(encoding), "{compact}");
    assert_eq!(f.encoding, None);
    assert_eq!((c.t(), c.bands_per_date), (dates, dims), "{compact}");
    assert_eq!((c.t(), c.bands_per_date), (f.t(), f.bands_per_date));
    assert_eq!((c.width, c.height), (W, H));
    assert_eq!(c.band_names, f.band_names);
    assert_eq!(c.band_names[0], "A00");
    assert_eq!(c.band_names.len(), dims, "the scale bands are not bands of the cube");
    assert_eq!(c.embedding_tag, f.embedding_tag);
    assert!(c.embedding_tag.contains("embedding_source"), "{}", c.embedding_tag);
    assert_eq!(c.time_is_date, f.time_is_date);
    assert_eq!(c.description, f.description);
    for (a, b) in c.layers.iter().zip(&f.layers) {
        assert_eq!((&a.label, a.time), (&b.label, b.time));
    }
    let (mut rc, mut rf) = (CubeReader::new(Arc::new(c.clone())), CubeReader::new(Arc::new(f.clone())));
    let mut nan_seen = false;
    for t in 0..dates {
        for (size, win) in [((W, H), (0, 0, W, H)), ((W / 2, H / 2), (0, 0, W, H)), ((3, 2), (2, 1, 6, 5))] {
            let n = size.0 * size.1;
            let (mut a, mut b) = (vec![0f32; n], vec![0f32; n]);
            // the shown band
            assert!(rc.read_window(t, win, &mut a, size) && rf.read_window(t, win, &mut b, size));
            same(&a, &b, &format!("{compact} date {t} shown, {size:?}"));
            for band in 1..=dims as i32 {
                assert!(
                    rc.read_band_window(t, band, win, &mut a, size) && rf.read_band_window(t, band, win, &mut b, size)
                );
                same(&a, &b, &format!("{compact} date {t} band {band}, {size:?}"));
                nan_seen |= a.iter().any(|v| v.is_nan());
            }
        }
        for (x, y) in [(0, 0), (1, 1), (2, 3), (4, 4), (9, 7)] {
            for band in [1, dims as i32 / 2, dims as i32] {
                let (a, b) = (rc.read_band_pixel(t, band, x, y, false), rf.read_band_pixel(t, band, x, y, false));
                same(&[a.unwrap()], &[b.unwrap()], &format!("{compact} pixel {x},{y}"));
            }
            same(&[rc.read_pixel(t, x, y).unwrap()], &[rf.read_pixel(t, x, y).unwrap()], "shown pixel");
        }
    }
    assert!(nan_seen, "{compact}: the fixture has pixels without an embedding");
}

#[test]
fn aef_single_and_multi_date() {
    compare("aef2_compact.tif", "aef2_float.tif", Encoding::Aef, 2, 64);
    compare("aef1_compact.tif", "aef1_float.tif", Encoding::Aef, 1, 64);
}

#[test]
fn scaled_single_and_multi_date() {
    compare("tess2_compact.tif", "tess2_float.tif", Encoding::Scaled, 2, 32);
    compare("tess1_compact.tif", "tess1_float.tif", Encoding::Scaled, 1, 32);
}

#[test]
fn scaled_cog() {
    compare("tess2_compact_cog.tif", "tess2_float.tif", Encoding::Scaled, 2, 32);
    assert_eq!(open(&data("tess2_compact_cog.tif")).layers[1].scale_band, 66);
}

#[test]
fn scale_bands_are_in_the_files_layout() {
    let c = open(&data("tess2_compact.tif"));
    // 33 bands a date: the dimensions, then the scale
    assert_eq!(
        c.layers.iter().map(|l| (l.band_base, l.band, l.scale_band)).collect::<Vec<_>>(),
        [(0, 1, 33), (33, 34, 66)]
    );
    let a = open(&data("aef2_compact.tif"));
    assert_eq!(a.layers.iter().map(|l| (l.band_base, l.scale_band)).collect::<Vec<_>>(), [(0, 0), (64, 0)]);
}

fn unknown_copy(from: &str, name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("janus_cube_compact");
    std::fs::create_dir_all(&d).unwrap();
    let to = d.join(name);
    std::fs::copy(data(from), &to).unwrap();
    to
}

#[test]
fn unknown_encoding_is_an_error() {
    let to = unknown_copy("aef1_compact.tif", "unknown.tif");
    {
        let opts = gdal::DatasetOptions {
            open_flags: GdalOpenFlags::GDAL_OF_RASTER | GdalOpenFlags::GDAL_OF_UPDATE,
            ..Default::default()
        };
        let mut ds = gdal::Dataset::open_ex(&to, opts).unwrap();
        ds.set_metadata_item("ZEIT_EMBEDDING_ENCODING", "wavelet-int4", "").unwrap();
    }
    let e = open_cube(&[to.to_string_lossy().into_owned()], &BandSelection::default()).unwrap_err();
    assert!(
        e.contains("unknown embedding encoding") && e.contains("wavelet-int4") && e.contains("update Janus"),
        "{e}"
    );
}

#[test]
fn a_file_that_does_not_fit_its_tag_is_an_error() {
    // a float32 file tagged scaled: no scale bands
    let to = unknown_copy("aef1_float.tif", "wrong_tag.tif");
    {
        let opts = gdal::DatasetOptions {
            open_flags: GdalOpenFlags::GDAL_OF_RASTER | GdalOpenFlags::GDAL_OF_UPDATE,
            ..Default::default()
        };
        let mut ds = gdal::Dataset::open_ex(&to, opts).unwrap();
        ds.set_metadata_item("ZEIT_EMBEDDING_ENCODING", "scaled-int8", "").unwrap();
    }
    let e = open_cube(&[to.to_string_lossy().into_owned()], &BandSelection::default()).unwrap_err();
    assert!(e.contains("not a compact embeddings file"), "{e}");
}

#[test]
fn what_reads_the_files_itself_refuses_compact_embeddings() {
    use janus_cube::calc::plan::{CalcLayer, Options, plan};
    for name in ["aef1_compact.tif", "tess2_compact.tif"] {
        let info = open(&data(name));
        let msg = info.require_float_bands("Tool").unwrap_err();
        assert!(msg.contains("Tool does not read Zeit's compact embeddings") && msg.contains("compact=True"), "{msg}");
        // the calculator
        let layers = vec![CalcLayer { name: "e".into(), alias: "e".into(), info: Arc::new(info.clone()) }];
        let e = plan("e + 1", &layers, &Options::default()).err().expect("the calculator refuses").message;
        assert!(e.contains("The calculator does not read Zeit's compact embeddings"), "{e}");
        // the series' VRT (what a Zeit run reads)
        let vrt = std::env::temp_dir().join("janus_cube_compact").join("refused.vrt");
        std::fs::create_dir_all(vrt.parent().unwrap()).unwrap();
        let e = janus_cube::vrt::write_cube_vrt(&info, &vrt.to_string_lossy(), 0, false).unwrap_err();
        assert!(e.contains("does not read Zeit's compact embeddings"), "{e}");
    }
    // a float32 file is read as ever
    let f = open(&data("tess2_float.tif"));
    assert!(f.require_float_bands("Tool").is_ok());
    let layers = vec![CalcLayer { name: "e".into(), alias: "e".into(), info: Arc::new(f) }];
    assert!(plan("e + 1", &layers, &Options::default()).is_ok());
}
