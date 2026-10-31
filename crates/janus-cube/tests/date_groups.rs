//! A single file of several bands a date, date after date: dated by a `<name>_dates.csv` next to it (cdts' and
//! Zeit's) or by its bands' names ("2020-01-15_red"); a band a date dated by the file next to it.

use std::path::Path;

use gdal::raster::Buffer;
use gdal::{DriverManager, Metadata};
use janus_cube::{BandSelection, CubeInfo, CubeReader, open_cube};

const W: usize = 20;
const H: usize = 10;

/// `n` bands, band b's value b * 10 everywhere; descriptions if given.
fn stack(f: &Path, n: usize, descs: &[&str]) {
    let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = drv.create_with_band_type::<f32, _>(f, W, H, n).unwrap();
    ds.set_geo_transform(&[500000.0, 30.0, 0.0, 8900000.0, 0.0, -30.0]).unwrap();
    for b in 1..=n {
        let mut rb = ds.rasterband(b).unwrap();
        rb.write((0, 0), (W, H), &mut Buffer::new((W, H), vec![b as f32 * 10.0; W * H])).unwrap();
        if let Some(d) = descs.get(b - 1) {
            rb.set_description(d).unwrap();
        }
    }
}

fn open(f: &Path, sel: &BandSelection) -> CubeInfo {
    open_cube(&[f.to_string_lossy().to_string()], sel).unwrap()
}

fn value(info: CubeInfo, t: usize) -> f32 {
    let mut r = CubeReader::new(std::sync::Arc::new(info));
    let mut v = vec![0f32; 1];
    assert!(r.read_window(t, (3, 4, 1, 1), &mut v, (1, 1)));
    v[0]
}

#[test]
fn groups_of_bands() {
    let d = std::env::temp_dir().join("janus_cube_date_groups");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();

    // 3 dates x 4 bands, the dates out of order in the file
    let f = d.join("ccdc.tif");
    stack(&f, 12, &[]);
    std::fs::write(
        d.join("ccdc_Dates.csv"),
        "Date,Fractional_Year,Ordinal_Day\n2021-03-01,2021.16,737850\n2020-07-06,2020.51,737612\n2022-01-10,2022.02,738165\n",
    )
    .unwrap();
    let info = open(&f, &BandSelection::default());
    assert!(info.time_is_date && !info.stack);
    assert_eq!((info.t(), info.bands_per_date), (3, 4));
    assert_eq!(info.band_names, ["Band 1", "Band 2", "Band 3", "Band 4"]);
    let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(labels, ["2020-07-06", "2021-03-01", "2022-01-10"]);
    // sorted by date: the file's second date (bands 5-8) first
    assert_eq!(info.layers.iter().map(|l| l.band_base).collect::<Vec<_>>(), [4, 0, 8]);
    assert!(info.description.contains("3 dates x 4 bands, dates from ccdc_Dates.csv"), "{}", info.description);
    assert_eq!(value(info, 0), 50.0);
    // band 3 of each date, its normalized difference with band 1: (70 - 50) / (70 + 50) on the first
    let sel = BandSelection { band: 3, nd_band: 1, chosen: true, ..Default::default() };
    let info = open(&f, &sel);
    assert_eq!(info.layers[2].band, 11);
    assert!((value(info, 0) - 20.0 / 120.0).abs() < 1e-6);

    // the bands named date_band
    let f = d.join("named.tif");
    let names = ["2020-01-15_red", "2020-01-15_nir", "2020-02-16_red", "2020-02-16_nir"];
    stack(&f, 4, &names);
    let info = open(&f, &BandSelection::default());
    assert_eq!((info.t(), info.bands_per_date), (2, 2));
    assert_eq!(info.band_names, ["red", "nir"]);
    assert_eq!(info.layers[1].label, "2020-02-16");
    assert_eq!(value(info, 1), 30.0);

    // a band a date, the dates next to it (no header)
    let f = d.join("ndvi.tif");
    stack(&f, 3, &[]);
    std::fs::write(d.join("ndvi_dates.txt"), "2001-05-01\n2000-05-01\n2002-05-01\n").unwrap();
    let info = open(&f, &BandSelection::default());
    assert_eq!((info.t(), info.bands_per_date), (3, 1));
    assert!(info.time_is_date);
    assert_eq!(info.layers.iter().map(|l| l.band).collect::<Vec<_>>(), [2, 1, 3]);

    // dates that do not divide the bands: as before, a band a date without dates
    let f = d.join("odd.tif");
    stack(&f, 5, &[]);
    std::fs::write(d.join("odd_dates.csv"), "Date\n2001-05-01\n2000-05-01\n").unwrap();
    let info = open(&f, &BandSelection::default());
    assert_eq!((info.t(), info.bands_per_date, info.time_is_date), (5, 1, false));
    let _ = std::fs::remove_dir_all(&d);
}
