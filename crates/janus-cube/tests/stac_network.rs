//! STAC series from the real catalogs (network; `cargo test -p janus-cube --test stac_network -- --ignored`): a
//! search, the series built as VRTs, opened as a cube and read.

use std::sync::atomic::AtomicBool;

use janus_cube::stac::{self, Search};
use janus_cube::stac_vrt::{BuildSpec, build, download};
use janus_cube::{BandSelection, QaRule, open_cube, with_thread_reader};

fn series(
    url: &str,
    collection: &str,
    bbox: [f64; 4],
    start: &str,
    end: &str,
    bands: &[&str],
    name: &str,
) -> Vec<String> {
    janus_cube::remote::configure_remote_access();
    let cancel = AtomicBool::new(false);
    let s = Search {
        url: url.into(),
        collection: collection.into(),
        bbox,
        start: start.into(),
        end: end.into(),
        max_cloud: Some(50.0),
        max_items: 40,
    };
    let t0 = std::time::Instant::now();
    let items = stac::search(&s, &|_| {}, &cancel).expect("search");
    assert!(!items.is_empty(), "{collection}: no items");
    let choices = stac::band_choices(&items);
    let picked: Vec<_> = bands.iter().map(|k| choices.iter().find(|c| c.key == *k).expect(k).clone()).collect();
    let dir = std::env::temp_dir().join(format!("janus_stac_test_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let spec = BuildSpec { items, bands: picked, bbox, res: None, dir, planetary: stac::is_planetary(url) };
    let built = build(&spec, &|_, _| {}, &cancel).expect("build");
    eprintln!(
        "{name}: {} items, {} dates, grid {}x{} {} (warped {}), {:.1} s",
        spec.items.len(),
        built.files.len(),
        built.grid.w,
        built.grid.h,
        &built.grid.crs[..built.grid.crs.len().min(20)],
        built.warped,
        t0.elapsed().as_secs_f64()
    );
    let info = open_cube(&built.files, &BandSelection::default()).expect("open");
    assert_eq!(info.t(), built.files.len());
    assert!(info.time_is_date, "dates from the file names");
    assert_eq!(info.bands_per_date, bands.len());
    eprintln!("  bands {:?} nm {:?} sel {:?}", info.band_names, info.band_nm, info.sel);
    if bands.len() > 2 {
        // red and NIR with wavelengths: NDVI at first; a quality band recognised
        assert!(info.sel.nd_band > 0, "NDVI at first");
    }
    let info = std::sync::Arc::new(info);
    let t1 = std::time::Instant::now();
    let (w, h) = (info.width, info.height);
    let mut v = vec![0f32; 64 * 64];
    let read = with_thread_reader(&info, |r| r.read_window(info.t() - 1, (0, 0, w, h), &mut v, (64, 64)));
    let finite = v.iter().filter(|x| x.is_finite()).count();
    eprintln!("  last date read at 64x64: {read} {finite} values, {:.1} s", t1.elapsed().as_secs_f64());
    assert!(read);
    built.files
}

#[test]
#[ignore]
fn earth_search() {
    series(
        "https://earth-search.aws.element84.com/v1",
        "sentinel-2-c1-l2a",
        [-63.0, -10.0, -62.9, -9.9],
        "2024-07-01",
        "2024-07-31",
        &["red", "nir", "scl"],
        "es",
    );
}

#[test]
#[ignore]
fn earth_search_two_utm_zones() {
    // 60 W is the edge of UTM zones 20 and 21: tiles of both, the minority warped
    series(
        "https://earth-search.aws.element84.com/v1",
        "sentinel-2-c1-l2a",
        [-60.3, -10.05, -59.6, -9.95],
        "2024-08-01",
        "2024-08-20",
        &["red", "nir", "scl"],
        "es_zones",
    );
}

#[test]
#[ignore]
fn planetary_computer() {
    series(
        "https://planetarycomputer.microsoft.com/api/stac/v1",
        "sentinel-2-l2a",
        [-63.0, -10.0, -62.9, -9.9],
        "2024-07-01",
        "2024-07-31",
        &["B04", "B08", "SCL"],
        "pc",
    );
}

#[test]
#[ignore]
fn planetary_computer_landsat() {
    series(
        "https://planetarycomputer.microsoft.com/api/stac/v1",
        "landsat-c2-l2",
        [-63.0, -10.0, -62.9, -9.9],
        "2024-06-01",
        "2024-08-31",
        &["red", "nir08", "qa_pixel"],
        "pc_landsat",
    );
}

#[test]
#[ignore]
fn brazil_data_cube() {
    // no proj: metadata: the grids are read from one file per tile
    let vrts = series(
        "https://data.inpe.br/bdc/stac/v1",
        "S2-16D-2",
        [-63.0, -10.0, -62.9, -9.9],
        "2024-06-01",
        "2024-08-31",
        &["B04", "B08", "SCL"],
        "bdc",
    );
    // two dates downloaded: the same bands, names and values as read in place
    let out = std::env::temp_dir().join("janus_stac_test_bdc_download");
    let _ = std::fs::remove_dir_all(&out);
    let t0 = std::time::Instant::now();
    let tifs = download(&vrts[..2], &out, &|_, _| {}, &AtomicBool::new(false)).expect("download");
    eprintln!("  2 dates downloaded in {:.1} s", t0.elapsed().as_secs_f64());
    let a = std::sync::Arc::new(open_cube(&vrts[..2], &BandSelection::default()).unwrap());
    let b = std::sync::Arc::new(open_cube(&tifs, &BandSelection::default()).unwrap());
    assert_eq!(
        (a.width, a.height, a.bands_per_date, &a.band_names),
        (b.width, b.height, b.bands_per_date, &b.band_names)
    );
    assert_eq!((a.sel, a.band_nm.clone()), (b.sel, b.band_nm.clone()));
    for t in 0..2 {
        let (mut x, mut y) = (vec![0f32; 100], vec![0f32; 100]);
        with_thread_reader(&a, |r| r.read_window(t, (300, 300, 10, 10), &mut x, (10, 10)));
        with_thread_reader(&b, |r| r.read_window(t, (300, 300, 10, 10), &mut y, (10, 10)));
        assert!(x.iter().zip(&y).all(|(p, q)| p == q || p.is_nan() && q.is_nan()), "date {t}: {x:?} vs {y:?}");
    }
    assert_eq!(QaRule::Scl.id(), "scl");
}
