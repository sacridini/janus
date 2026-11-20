//! The series' VRT of one file of many dates with one band each: the band asked for (the shown band, or band 1) of
//! each date is that date's band of the file, not the first band of the file for every date.

use std::path::Path;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{DriverManager, Metadata};
use janus_cube::{BandSelection, open_cube};

const W: usize = 8;
const H: usize = 6;

#[test]
fn a_file_of_many_dates_one_band_each() {
    let dir = std::env::temp_dir().join("janus_vrt_many_dates");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("series.tif").to_string_lossy().to_string();
    let days = ["20240101", "20240111", "20240121", "20240131"];
    {
        let d = DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut ds = d.create_with_band_type::<f32, _>(Path::new(&file), W, H, days.len()).unwrap();
        ds.set_geo_transform(&[500000.0, 10.0, 0.0, 8900000.0, 0.0, -10.0]).unwrap();
        ds.set_spatial_ref(&SpatialRef::from_epsg(32720).unwrap()).unwrap();
        for (k, day) in days.iter().enumerate() {
            let mut b = ds.rasterband(k + 1).unwrap();
            b.set_description(day).unwrap();
            let mut buf = Buffer::new((W, H), vec![(k + 1) as f32; W * H]);
            b.write((0, 0), (W, H), &mut buf).unwrap();
        }
    }
    let sel = BandSelection { stack: Some(false), ..Default::default() };
    let info = open_cube(&[file], &sel).expect("open");
    assert_eq!(info.t(), days.len());
    // the shown band (0) and band 1 (the shown band) must both give each date its own band
    for source_band in [0, 1] {
        let vrt = dir.join(format!("zeit{source_band}.vrt")).to_string_lossy().to_string();
        janus_cube::vrt::write_cube_vrt(&info, &vrt, source_band, false).unwrap();
        let ds = gdal::Dataset::open(&vrt).unwrap();
        assert_eq!(ds.raster_count(), days.len());
        for t in 0..days.len() {
            let b = ds.rasterband(t + 1).unwrap();
            let v = b.read_as::<f32>((0, 0), (W, H), (W, H), None).unwrap();
            assert!(
                v.data().iter().all(|&x| x == (t + 1) as f32),
                "source_band {source_band}, date {t}: {:?}",
                &v.data()[..3]
            );
        }
    }
}
