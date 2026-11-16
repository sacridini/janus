//! Wayback against the real service (ignored: needs the network). Run with
//! `cargo test -p janus-geo wayback -- --ignored --nocapture`.

use janus_geo::wayback;

#[test]
#[ignore]
fn berlin_lists_and_renders() {
    let cache = std::env::temp_dir().join("janus-geo-wayback-cache");
    let (lon, lat) = (13.64525, 52.45580);
    let z = wayback::zoom_for(150.0, lat, wayback::MAX_Z);
    let images = wayback::list(lon, lat, z, &cache).expect("list");
    println!("zoom {z}: {} images", images.len());
    for i in &images {
        println!("{} release {} ({}) res {:?} {}", i.date, i.release, i.release_date, i.res_m, i.desc);
    }
    assert!(images.len() >= 3, "few images: {}", images.len());
    assert!(images.windows(2).all(|w| w[0].date < w[1].date));

    // the newest image, in UTM 33N (EPSG:32633), 150 m
    let last = images.last().unwrap();
    let rel = wayback::releases(&cache).unwrap().into_iter().find(|r| r.num == last.release).unwrap();
    let mut xs = [lon];
    let mut ys = [lat];
    let mut zs = [0.0];
    let mut wgs = gdal::spatial_ref::SpatialRef::from_epsg(4326).unwrap();
    wgs.set_axis_mapping_strategy(gdal::spatial_ref::AxisMappingStrategy::TraditionalGisOrder);
    let mut utm = gdal::spatial_ref::SpatialRef::from_epsg(32633).unwrap();
    utm.set_axis_mapping_strategy(gdal::spatial_ref::AxisMappingStrategy::TraditionalGisOrder);
    gdal::spatial_ref::CoordTransform::new(&wgs, &utm).unwrap().transform_coords(&mut xs, &mut ys, &mut zs).unwrap();
    let px = wayback::out_pixels(150.0, lat, z);
    let im = wayback::render(&rel, z, "EPSG:32633", (xs[0], ys[0]), 150.0, px, &cache).expect("render");
    assert_eq!((im.w, im.h), (px, px));
    let out = std::env::temp_dir().join("janus-wayback-berlin.png");
    im.write_png(&out).unwrap();
    println!("rendered {} ({} px) -> {}", last.date, px, out.display());
}
