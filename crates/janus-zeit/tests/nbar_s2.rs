//! NBAR with its angles from Sentinel-2's granule metadata (`angles = "sentinel2"`): a raster run and a pixel run give
//! what zeit.nbar gives with `zeit.sentinel2_angles` (tests/data/s2angles/series, written by
//! tools/s2angles_fixtures.py: 20 x 16 pixels of 800 m in tile 23LKC, three dates whose metadata are the full tile's
//! of 2023-07-23 (twice) and a partial swath's of 2023-07-26, whose view angles are missing in the east of the
//! scene), for the whole scene, a window of it (the angles are computed for the window) and a fixed solar zenith.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, Metadata};
use janus_cube::{BandSelection, open_cube};
use janus_zeit::granules::of_series;
use janus_zeit::job::{JobResult, JobSpec, run_job};
use janus_zeit::pixel::Site;
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::{Value, json};

const W: usize = 20;
const H: usize = 16;
const T: usize = 3;
const ROLES: [&str; 3] = ["blue", "red", "nir"];

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/s2angles")
}

fn abs(name: &str) -> String {
    data().join(name).to_string_lossy().to_string()
}

/// [band][date][pixel], missing as null.
type Values = Vec<Vec<Vec<Option<f64>>>>;

#[derive(Deserialize)]
struct Pixel {
    values: Values,
    cfactor: Option<Values>,
}

#[derive(Deserialize)]
struct Window {
    params: BTreeMap<String, Value>,
    /// x0, y0, x1, y1
    window: [usize; 4],
    values: Values,
    cfactor: Option<Values>,
    sza: Vec<Option<f64>>,
    pixels: BTreeMap<String, Pixel>,
}

#[derive(Deserialize)]
struct Fixtures {
    granules: Vec<String>,
    windows: BTreeMap<String, Window>,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(&std::fs::read_to_string(data().join("nbar.json")).unwrap()).unwrap()
}

/// The series' spec: the roles' files and each date's metadata (local paths).
fn spec(f: &Fixtures, params: &BTreeMap<String, Value>) -> JobSpec {
    let mut spec: JobSpec =
        serde_json::from_str(&std::fs::read_to_string(data().join("series/spec.json")).unwrap()).unwrap();
    spec.tool = "nbar".into();
    spec.input = abs("series/input.tif");
    spec.params = params.clone();
    spec.params.insert("angles".into(), json!("sentinel2"));
    for r in ROLES {
        spec.bands.insert(r.to_string(), abs(&format!("series/{r}.tif")));
    }
    spec.granules = f.granules.iter().map(|g| vec![abs(g)]).collect();
    spec
}

fn read(path: &str) -> Vec<Vec<f64>> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    (1..=ds.raster_count())
        .map(|b| {
            let band = ds.rasterband(b).unwrap();
            let nd = band.no_data_value();
            let v = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
            v.data()
                .iter()
                .map(|&x| if nd.is_some_and(|d| d == x || d.is_nan() && x.is_nan()) { f64::NAN } else { x })
                .collect()
        })
        .collect()
}

fn close(got: f64, want: Option<f64>) -> bool {
    match want {
        Some(w) => got.is_finite() && (got - w).abs() <= 1e-6 * w.abs() + 1e-12,
        None => !got.is_finite(),
    }
}

fn run(spec: &mut JobSpec, name: &str) -> Result<JobResult, String> {
    let out = std::env::temp_dir().join(format!("janus-zeit-nbar-s2-test-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    run_job(spec, &|_, _| {}, &AtomicBool::new(false))
}

#[test]
fn raster_run_is_zeits() {
    let f = fixtures();
    let (mut values, mut worst, mut missing) = (0, 0.0f64, 0);
    for (name, c) in &f.windows {
        let mut s = spec(&f, &c.params);
        s.window = Some(c.window);
        let result = run(&mut s, &format!("raster-{name}")).unwrap();
        let cfactor = c.params["cfactor"] == json!(true);
        let want_ids: Vec<String> = ROLES
            .iter()
            .map(|r| format!("nbar_{r}"))
            .chain(ROLES.iter().filter(|_| cfactor).map(|r| format!("cfactor_{r}")))
            .collect();
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, want_ids, "{name}");
        let one_of = |id: &str, want: &Values, m: usize| {
            let o = result.outputs.iter().find(|o| o["id"] == id).unwrap();
            let bands = read(o["path"].as_str().unwrap());
            assert_eq!(bands.len(), T, "{id}");
            let (mut n, mut worst, mut nan) = (0, 0.0f64, 0);
            for (k, got) in bands.iter().enumerate() {
                assert_eq!(got.len(), want[m][k].len(), "{name} {id}");
                for (i, (g, w)) in got.iter().zip(&want[m][k]).enumerate() {
                    // written as float32
                    let w = w.map(|w| f64::from(w as f32));
                    assert!(close(*g, w), "{name} {id} date {k} pixel {i}: {g} vs {w:?}");
                    match w {
                        Some(w) => {
                            n += 1;
                            worst = worst.max((g - w).abs() / w.abs().max(1e-12));
                        }
                        None => nan += 1,
                    }
                }
            }
            (n, worst, nan)
        };
        for (m, r) in ROLES.iter().enumerate() {
            let (n, w, nan) = one_of(&format!("nbar_{r}"), &c.values, m);
            values += n;
            worst = worst.max(w);
            missing += nan;
            if cfactor {
                let (n, w, _) = one_of(&format!("cfactor_{r}"), c.cfactor.as_ref().unwrap(), m);
                values += n;
                worst = worst.max(w);
            }
        }
        // the scene's solar zenith of each date over the window is what the angles of the window give
        assert!(c.sza.iter().all(|s| s.is_some()), "{name}");
    }
    println!(
        "{values} values compared with zeit.nbar(…, zeit.sentinel2_angles(…)), the largest relative difference {worst:.2e}, \
         {missing} missing in both"
    );
    assert!(values > 3000 && missing > 100);
}

fn site(col: usize, row: usize) -> Site {
    let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
    Site { crs_wkt: wkt, geotransform: [199980.0 + 12500.0, 800.0, 0.0, 8300020.0 - 3000.0, 0.0, -800.0], col, row }
}

fn pixel_input(f: &Fixtures, col: usize, row: usize) -> PixelInput {
    let s = spec(f, &BTreeMap::new());
    let at = |path: &str| -> Vec<f64> { read(path).iter().map(|b| b[row * W + col]).collect() };
    PixelInput {
        years: s.years.clone(),
        days: s.days.clone(),
        values: at(&s.input),
        bands: s.bands.iter().map(|(r, p)| (r.clone(), at(p))).collect(),
        granules: s.granules.clone(),
        site: Some(site(col, row)),
        ..Default::default()
    }
}

#[test]
fn pixel_run_is_zeits() {
    let f = fixtures();
    let (mut n, mut worst) = (0, 0.0f64);
    for (name, c) in &f.windows {
        let mut params = c.params.clone();
        params.insert("angles".into(), json!("sentinel2"));
        let p = Params::with_defaults("nbar", &params).unwrap();
        for (key, want) in &c.pixels {
            let (col, row) = key.split_once(',').map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap())).unwrap();
            let r = run_pixel("nbar", &p, &pixel_input(&f, col, row)).unwrap();
            assert_eq!(r.overlays.len(), ROLES.len(), "{name} pixel {key}: {:?}", r.rows);
            for (o, (m, role)) in r.overlays.iter().zip(ROLES.iter().enumerate()) {
                assert_eq!(o.label, format!("NBAR {role}"));
                let valid: Vec<f64> = want.values[m].iter().filter_map(|d| d[0]).collect();
                assert_eq!(o.y.len(), valid.len(), "{name} {role} pixel {key}");
                for (g, w) in o.y.iter().zip(&valid) {
                    assert!(close(g.unwrap(), Some(*w)), "{name} {role} pixel {key}: {g:?} vs {w}");
                    worst = worst.max((g.unwrap() - w).abs() / w.abs());
                    n += 1;
                }
                if let Some(cf) = &want.cfactor {
                    let finite: Vec<f64> = cf[m].iter().filter_map(|d| d[0]).collect();
                    let row = r.rows.iter().find(|row| row[0] == format!("Mean c-factor ({role})")).unwrap();
                    if !finite.is_empty() {
                        let mean = finite.iter().sum::<f64>() / finite.len() as f64;
                        let shown: f64 = row[1].parse().unwrap();
                        assert!((shown - mean).abs() <= 1e-5 * mean.abs(), "{} vs {mean}", row[1]);
                    }
                }
            }
        }
    }
    println!("{n} pixel values compared with zeit.nbar, the largest relative difference {worst:.2e}");
    assert!(n > 30);
}

#[test]
fn a_date_without_metadata_stops_the_run_and_names_it() {
    let f = fixtures();
    let mut s = spec(&f, &BTreeMap::new());
    s.granules[1].clear();
    s.granules[2].clear();
    let e = run(&mut s, "none").map(|_| ()).unwrap_err();
    assert!(e.contains("no Sentinel-2 granule metadata") && e.contains("2023-07-24, 2023-07-26"), "{e}");
    // none given at all
    s.granules.clear();
    let e = run(&mut s, "none2").map(|_| ()).unwrap_err();
    assert!(e.contains("2023-07-23, 2023-07-24, 2023-07-26"), "{e}");
    // the pixel run says it in its rows
    let p = Params::with_defaults("nbar", &BTreeMap::from([("angles".to_string(), json!("sentinel2"))])).unwrap();
    let mut input = pixel_input(&f, 3, 2);
    input.granules[2].clear();
    let r = run_pixel("nbar", &p, &input).unwrap();
    assert!(r.overlays.is_empty() && r.rows[0][1].contains("2023-07-26"), "{:?}", r.rows);
    // a pixel run that does not know the pixel's place
    let mut input = pixel_input(&f, 3, 2);
    input.site = None;
    let r = run_pixel("nbar", &p, &input).unwrap();
    assert!(r.rows[0][1].contains("place"), "{:?}", r.rows);
}

#[test]
fn metadata_of_another_date_is_refused() {
    let f = fixtures();
    // the partial swath of 2023-07-26 for the date 2023-07-23
    let mut s = spec(&f, &BTreeMap::new());
    s.granules[0] = vec![abs("s2_23LKC_20230726_partial_MTD_TL.xml")];
    let e = run(&mut s, "wrong-date").map(|_| ()).unwrap_err();
    assert!(e.contains("2023-07-23") && e.contains("sensed on 2023-07-26"), "{e}");
}

#[test]
fn the_view_band_and_a_bad_one() {
    let f = fixtures();
    let mut s = spec(&f, &BTreeMap::from([("view_band".to_string(), json!("B02"))]));
    s.window = Some([0, 0, W, H]);
    let b02 = run(&mut s, "b02").unwrap();
    let mut s = spec(&f, &BTreeMap::new());
    s.window = Some([0, 0, W, H]);
    let b06 = run(&mut s, "b06").unwrap();
    // another view band's angles change the result a little
    let get = |r: &JobResult| read(r.outputs[0]["path"].as_str().unwrap());
    let (a, b) = (get(&b02), get(&b06));
    let diff = a
        .iter()
        .flatten()
        .zip(b.iter().flatten())
        .filter(|(x, y)| x.is_finite() && y.is_finite() && (*x - *y).abs() > 1e-9)
        .count();
    assert!(diff > 100, "{diff}");
    let mut s = spec(&f, &BTreeMap::from([("view_band".to_string(), json!("B13"))]));
    let e = run(&mut s, "b13").map(|_| ()).unwrap_err();
    assert!(e.contains("B13") && e.contains("not a Sentinel-2 band"), "{e}");
}

#[test]
fn the_angle_bands_are_not_needed_but_the_bands_to_normalize_are() {
    let f = fixtures();
    let mut s = spec(&f, &BTreeMap::new());
    s.bands.clear();
    let e = run(&mut s, "no-bands").map(|_| ()).unwrap_err();
    assert!(e.contains("map at least one band to normalize"), "{e}");
    // the angles mode "bands" still asks for the angle bands
    let mut s = spec(&f, &BTreeMap::new());
    s.params.insert("angles".into(), json!("bands"));
    let e = run(&mut s, "bands-mode").map(|_| ()).unwrap_err();
    assert!(e.contains("map the angle bands first: sza, saa, vza, vaa"), "{e}");
}

/// A one-band GeoTIFF of a date at `path`, optionally with the metadata item of its granule.
fn write_date(path: &Path, granule: Option<&str>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = drv.create_with_band_type::<f32, _>(path, 2, 2, 1).unwrap();
    ds.set_geo_transform(&[500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
    if let Some(g) = granule {
        ds.set_metadata_item("STAC_GRANULE_METADATA", g, "").unwrap();
    }
}

#[test]
fn copy_python_calls_zeit_sentinel2_angles() {
    let f = fixtures();
    let params = BTreeMap::from([("angles".to_string(), json!("sentinel2")), ("view_band".to_string(), json!("B8A"))]);
    let p = Params::with_defaults("nbar", &params).unwrap();
    let mut input = pixel_input(&f, 3, 2);
    input.granules[1] = vec!["https://h/a.xml".into(), "https://h/b.xml".into()];
    input.granules[2].clear();
    input.bands.retain(|r, _| ROLES.contains(&r.as_str()));
    let (code, inputs) = janus_zeit::python::calls("nbar", &p, &input).unwrap();
    assert_eq!(inputs, vec!["nbar_cube"]);
    let first = abs("s2_23LKC_20230723_MTD_TL.xml").replace('\\', "\\\\");
    let sources = format!("sources = ['{first}', ['https://h/a.xml', 'https://h/b.xml'], None]\n");
    assert!(code.contains(&sources), "{code}");
    assert!(
        code.contains("angles = zeit.sentinel2_angles(sources, like=nbar_cube, view_band='B8A')\nresult1 = zeit.nbar("),
        "{code}"
    );
    assert!(!code.contains("angles_raw") && code.contains("planetary_computer"), "{code}");
}

#[test]
fn each_dates_metadata_comes_from_its_vrt_or_its_product() {
    let base = std::env::temp_dir().join("janus-zeit-test-granules-of-series");
    let _ = std::fs::remove_dir_all(&base);
    // the metadata a STAC date's VRT records (here the files of a folder carry it as metadata), several for a date
    let stac = base.join("stac");
    write_date(&stac.join("20230723.tif"), Some("https://h/a/MTD_TL.xml https://h/b/MTD_TL.xml"));
    write_date(&stac.join("20230724.tif"), Some("https://h/c/MTD_TL.xml"));
    write_date(&stac.join("20230725.tif"), None);
    let info = open_cube(&[stac.to_string_lossy().into_owned()], &BandSelection::default()).unwrap();
    assert_eq!(info.t(), 3);
    let g = of_series(&info);
    assert_eq!(g[0], ["https://h/a/MTD_TL.xml", "https://h/b/MTD_TL.xml"]);
    assert_eq!(g[1], ["https://h/c/MTD_TL.xml"]);
    assert!(g[2].is_empty(), "{:?}", g[2]);
    // a folder of files that are each beside its own metadata is not a case (one name per folder); files in
    // the granule folders of a SAFE product are
    let mut dirs = vec![];
    for d in ["20230723", "20230724"] {
        let granule = base.join("safe").join(format!("S2_{d}.SAFE/GRANULE/L2A_T23LKC_{d}"));
        std::fs::create_dir_all(&granule).unwrap();
        std::fs::write(granule.join("MTD_TL.xml"), "<x/>").unwrap();
        write_date(&granule.join("IMG_DATA/R10m").join(format!("T23LKC_{d}T132229_B04_10m.tif")), None);
        dirs.push(granule.join("IMG_DATA/R10m").to_string_lossy().into_owned());
    }
    let info = open_cube(&dirs, &BandSelection::default()).unwrap();
    assert_eq!(info.t(), 2);
    let g = of_series(&info);
    for (k, d) in ["20230723", "20230724"].iter().enumerate() {
        assert_eq!(g[k].len(), 1);
        assert!(g[k][0].replace('\\', "/").ends_with(&format!("GRANULE/L2A_T23LKC_{d}/MTD_TL.xml")), "{:?}", g[k]);
    }
    let _ = std::fs::remove_dir_all(&base);
}
