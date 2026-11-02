//! k-means (a tool after the bridge, Janus' own): a raster run finds the kinds of series a made-up image holds,
//! names its clusters by the samples in them (each place counting once), and the SOM's are named the same way.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use gdal::DriverManager;
use gdal::raster::Buffer;
use janus_zeit::job::{JobSpec, SamplePixel, run_job};
use serde_json::{Value, json};

const W: usize = 60;
const H: usize = 40;
const T: usize = 12;

/// The kind of series of a pixel: forest (flat high) on the left third, regrowth (rising) in the middle, pasture
/// (flat low) on the right.
fn kind(x: usize) -> usize {
    x * 3 / W
}

/// A 12-date image of the three kinds, a little noise and some dates without a value, as a GeoTIFF.
fn image(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join("input.tif");
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<f32, _>(&path, W, H, T).unwrap();
    ds.set_geo_transform(&[500000.0, 30.0, 0.0, 9000000.0, 0.0, -30.0]).unwrap();
    for t in 0..T {
        let mut v = vec![0f32; W * H];
        for y in 0..H {
            for x in 0..W {
                let noise = (((x * 31 + y * 17 + t * 7) % 13) as f32 - 6.0) * 0.004;
                let base = match kind(x) {
                    0 => 0.85,
                    1 => 0.25 + 0.05 * t as f32,
                    _ => 0.3,
                };
                v[y * W + x] = if (x + y + t) % 23 == 0 { f32::NAN } else { base + noise };
            }
        }
        let mut b = ds.rasterband(t + 1).unwrap();
        b.set_no_data_value(Some(f64::NAN)).unwrap();
        b.write((0, 0), (W, H), &mut Buffer::new((W, H), v)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

/// The cluster map's values and the run's class names.
fn run(tool: &str, params: Value, samples: Vec<SamplePixel>, name: &str) -> (Vec<f64>, Vec<String>, Value) {
    let dir = std::env::temp_dir().join(format!("janus-zeit-kmeans-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let spec = JobSpec {
        tool: tool.into(),
        params: serde_json::from_value(params).unwrap(),
        input: image(&dir),
        years: (0..T).map(|t| 2010.0 + t as f64).collect(),
        output_dir: dir.join("out").to_string_lossy().into_owned(),
        threads: Some(2),
        samples,
        ..Default::default()
    };
    let res = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let o = res.outputs.iter().find(|o| o["id"] == "cluster").unwrap().clone();
    let ds = gdal::Dataset::open(o["path"].as_str().unwrap()).unwrap();
    let band = ds.rasterband(1).unwrap();
    let nd = band.no_data_value();
    let map: Vec<f64> = band
        .read_as::<f64>((0, 0), (W, H), (W, H), None)
        .unwrap()
        .data()
        .iter()
        .map(|&v| if nd == Some(v) { f64::NAN } else { v })
        .collect();
    let names = o["classes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    (map, names, o)
}

/// Samples: a polygon of forest (25 pixels, one place), two pins of regrowth, a polygon of pasture with a pin of
/// forest wrongly in it (the pasture's place outvotes it), all as the Classification window gives them.
fn samples() -> Vec<SamplePixel> {
    let mut s = vec![];
    for y in 10..15 {
        for x in 5..10 {
            s.push(SamplePixel { x, y, class: "Forest".into(), place: 0 });
        }
    }
    s.push(SamplePixel { x: 30, y: 20, class: "Regrowth".into(), place: 1 });
    s.push(SamplePixel { x: 28, y: 33, class: "Regrowth".into(), place: 2 });
    for y in 20..23 {
        for x in 50..53 {
            s.push(SamplePixel { x, y, class: "Pasture".into(), place: 3 });
        }
    }
    s.push(SamplePixel { x: 55, y: 5, class: "Pasture".into(), place: 4 });
    s.push(SamplePixel { x: 57, y: 30, class: "Forest".into(), place: 5 });
    s
}

#[test]
fn kmeans_finds_the_kinds_and_samples_name_them() {
    let (map, names, o) = run("kmeans", json!({"k": 3, "starts": 4}), samples(), "named");
    // each kind of series one cluster, every pixel with values given one
    let mut of_kind = [None; 3];
    for y in 0..H {
        for x in 0..W {
            let c = map[y * W + x];
            assert!(c.is_finite(), "pixel {x},{y} without a cluster");
            let k = &mut of_kind[kind(x)];
            assert_eq!(*k.get_or_insert(c), c, "pixel {x},{y}");
        }
    }
    let of_kind = of_kind.map(|c| c.unwrap() as usize);
    // ordered by their centres' mean: pasture (0.3), regrowth (0.525), forest (0.85)
    assert_eq!(of_kind, [3, 2, 1], "forest, regrowth, pasture: {of_kind:?}");
    assert_eq!(names[of_kind[0] - 1], "Forest (cluster 3, 33%)", "{names:?}");
    assert_eq!(names[of_kind[1] - 1], "Regrowth (cluster 2, 33%)", "{names:?}");
    // pasture: two places of pasture against one of forest
    assert_eq!(names[of_kind[2] - 1], "Pasture (cluster 1, 33%)", "{names:?}");
    assert_eq!(o["class_samples"][of_kind[2] - 1], json!({"Pasture": 10, "Forest": 1}));
    // the typical series are the centres
    let s = o["class_series"].as_array().unwrap();
    assert_eq!(s.len(), 3);
    let first = s[of_kind[0] - 1]["y"][0].as_f64().unwrap();
    assert!((first - 0.85).abs() < 0.01, "{first}");
}

#[test]
fn without_samples_the_clusters_are_numbered() {
    let (_, names, o) = run("kmeans", json!({"k": 3}), vec![], "numbered");
    assert_eq!(names, ["Cluster 1 (33%)", "Cluster 2 (33%)", "Cluster 3 (33%)"]);
    assert!(o.get("class_samples").is_none());
}

#[test]
fn the_som_is_named_by_the_samples_too() {
    let (map, names, _) = run("som", json!({"x": 3, "y": 1, "passes": 5}), samples(), "som");
    let forest = map[12 * W + 7] as usize;
    assert!(names[forest - 1].starts_with("Forest (cluster "), "{names:?}");
    let pasture = map[21 * W + 51] as usize;
    assert!(names[pasture - 1].starts_with("Pasture (cluster "), "{names:?}");
}
