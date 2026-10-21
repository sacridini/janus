//! Reprojection between a UTM grid and a geographic one, and a rotated grid in the same CRS: the exact transforms
//! against OGR's directly, their round trip, and the GPU grid against the exact transform.

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::CubeInfo;
use janus_geo::Reprojection;

fn wkt(epsg: u32) -> String {
    SpatialRef::from_epsg(epsg).unwrap().to_wkt().unwrap()
}

/// A 30 m UTM 22S scene (Landsat-like) and a geographic grid over it, 0.001 degree pixels.
fn layers() -> (CubeInfo, CubeInfo) {
    let utm = CubeInfo {
        id: 1,
        width: 2000,
        height: 1500,
        geo_transform: Some([500000.0, 30.0, 0.0, 7400000.0, 0.0, -30.0]),
        crs_wkt: wkt(32722),
        crs_authority: "EPSG:32722".into(),
        ..Default::default()
    };
    let geo = CubeInfo {
        id: 2,
        width: 900,
        height: 700,
        geo_transform: Some([-51.2, 0.001, 0.0, -23.3, 0.0, -0.001]),
        crs_wkt: wkt(4326),
        crs_authority: "EPSG:4326".into(),
        ..Default::default()
    };
    (utm, geo)
}

#[test]
fn utm_to_geographic() {
    let (utm, geo) = layers();
    let r = Reprojection::new(&utm, &geo, 0.6).unwrap();
    assert!(!r.same_crs);
    assert_eq!(r.crs_text, "EPSG:4326");

    // a map pixel through OGR directly
    let mut a = SpatialRef::from_epsg(32722).unwrap();
    let mut b = SpatialRef::from_epsg(4326).unwrap();
    a.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    b.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&a, &b).unwrap();
    for &(px, py) in &[(0.0, 0.0), (1000.5, 750.5), (1999.0, 1499.0)] {
        let (mut x, mut y, mut z) = ([500000.0 + px * 30.0], [7400000.0 - py * 30.0], [0.0]);
        ct.transform_coords(&mut x, &mut y, &mut z).unwrap();
        let (lx, ly) = r.to_layer(px, py).unwrap();
        assert!((lx - (x[0] + 51.2) / 0.001).abs() < 1e-6, "{lx}");
        assert!((ly - (-23.3 - y[0]) / 0.001).abs() < 1e-6, "{ly}");
        // and back
        let (bx, by) = r.to_active(lx, ly).unwrap();
        assert!((bx - px).abs() < 1e-6 && (by - py).abs() < 1e-6, "{bx} {by}");
    }

    // the grid: within 0.05 of the layer's pixels at the cells' centres, and so at other points
    assert!(r.has_domain());
    assert!(r.grid_error <= 0.05, "{}", r.grid_error);
    assert_eq!(r.grid.len(), r.grid_w * r.grid_h * 2);
    for &(px, py) in &[(10.3, 20.7), (777.7, 333.3), (1500.0, 1200.0)] {
        let (ex, ey) = r.to_layer(px, py).unwrap();
        let (gx, gy) = r.grid_at(px, py).unwrap();
        assert!((gx - ex).hypot(gy - ey) < 0.1, "{gx} {gy} vs {ex} {ey}");
    }

    // the layer on the map: its footprint holds its centre, and a map pixel is ~0.03 of a degree pixel... in
    // degree pixels: 30 m / (0.001 degree ≈ 102 m east, 111 m north)
    let fp = r.footprint(0.0, 0.0, 900.0, 700.0).unwrap();
    let (cx, cy) = r.to_active(450.0, 350.0).unwrap();
    assert!(cx > fp[0] && cx < fp[2] && cy > fp[1] && cy < fp[3]);
    let k = r.layer_px_per_map_px(1000.0, 750.0);
    assert!(k > 0.25 && k < 0.35, "{k}");
    let poly = r.polygon(0.0, 0.0, 100.0, 100.0, 4);
    assert_eq!(poly.len(), 32);
}

#[test]
fn rotated_same_crs() {
    let (utm, _) = layers();
    // the same CRS, a grid rotated by 10 degrees with 60 m pixels
    let (s, c) = (10f64.to_radians().sin(), 10f64.to_radians().cos());
    let rot = CubeInfo {
        id: 3,
        width: 800,
        height: 600,
        geo_transform: Some([510000.0, 60.0 * c, 60.0 * s, 7390000.0, 60.0 * s, -60.0 * c]),
        crs_wkt: wkt(32722),
        ..Default::default()
    };
    let r = Reprojection::new(&utm, &rot, 0.6).unwrap();
    assert!(r.same_crs);
    let (lx, ly) = r.to_layer(500.0, 400.0).unwrap();
    let (bx, by) = r.to_active(lx, ly).unwrap();
    assert!((bx - 500.0).abs() < 1e-9 && (by - 400.0).abs() < 1e-9);
    // affine: the grid is exact
    assert!(r.grid_error < 1e-3, "{}", r.grid_error);
}

#[test]
fn not_related() {
    let (mut utm, geo) = layers();
    utm.geo_transform = None;
    assert_eq!(Reprojection::new(&utm, &geo, 0.6).err().unwrap(), "no georeferencing");
    let (mut utm, geo) = layers();
    utm.crs_wkt.clear();
    assert_eq!(Reprojection::new(&utm, &geo, 0.6).err().unwrap(), "unknown CRS");
}
