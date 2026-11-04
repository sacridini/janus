//! A polygon of even-odd rings (the ROI) written as a vector file through OGR: a GeoPackage or GeoJSON (by
//! extension) in a given CRS, one feature with a few fields. The rings become OGC polygons: a ring inside an even
//! number of others is a shell, the rest holes of the ring right around them (a multipolygon when there are several
//! shells), so the file reads back as the same rings and fills the same pixels by the even-odd rule.

use gdal::vector::{FieldDefn, Geometry, LayerAccess, LayerOptions, OGRFieldType, OGRwkbGeometryType};

use crate::plane::{self, Ring};

/// A field's value.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Text(String),
    Integer(i64),
    Real(f64),
}

/// Each shell with its holes (indices into `rings`): the even-odd rings as OGC polygons. A ring's depth is the
/// number of other rings around one of its vertices; a hole belongs to the deepest ring around it.
pub fn nest(rings: &[Ring]) -> Vec<(usize, Vec<usize>)> {
    let ok: Vec<usize> = (0..rings.len()).filter(|&i| rings[i].len() >= 6).collect();
    let around = |i: usize| -> Vec<usize> {
        let (x, y) = (rings[i][0], rings[i][1]);
        ok.iter().copied().filter(|&j| j != i && plane::contains(std::slice::from_ref(&rings[j]), x, y)).collect()
    };
    let arounds: Vec<Vec<usize>> = rings.iter().enumerate().map(|(i, _)| around(i)).collect();
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    for &i in &ok {
        if arounds[i].len().is_multiple_of(2) {
            out.push((i, Vec::new()));
        }
    }
    for &i in &ok {
        let a = &arounds[i];
        if a.len() % 2 == 1 {
            // the ring right around it: the deepest of those around it
            let parent = a.iter().copied().max_by_key(|&j| arounds[j].len());
            if let Some(s) = parent.and_then(|p| out.iter_mut().find(|s| s.0 == p)) {
                s.1.push(i);
            }
        }
    }
    out
}

fn ring_wkt(r: &[f64]) -> String {
    // closed: the first vertex again; `{}` writes the shortest text that reads back as the same number
    let mut v: Vec<String> = r.as_chunks::<2>().0.iter().map(|p| format!("{} {}", p[0], p[1])).collect();
    v.push(format!("{} {}", r[0], r[1]));
    format!("({})", v.join(", "))
}

/// The rings as WKT: a POLYGON (one shell) or a MULTIPOLYGON.
pub fn to_wkt(rings: &[Ring]) -> Option<String> {
    let polys: Vec<String> = nest(rings)
        .iter()
        .map(|(s, holes)| {
            let mut parts = vec![ring_wkt(&rings[*s])];
            parts.extend(holes.iter().map(|&h| ring_wkt(&rings[h])));
            format!("({})", parts.join(", "))
        })
        .collect();
    match polys.len() {
        0 => None,
        1 => Some(format!("POLYGON {}", polys[0])),
        _ => Some(format!("MULTIPOLYGON ({})", polys.join(", "))),
    }
}

/// `rings` (even-odd, in the coordinates of `crs_wkt`; empty: none) written to `path` as one feature of layer
/// `layer` with `fields`: a GeoPackage (.gpkg) or GeoJSON (.geojson, .json), replaced if it exists. A GeoJSON whose
/// CRS has no EPSG code is written in WGS 84 longitudes and latitudes (RFC 7946: the format cannot name another).
/// The OGR driver of a vector file written by its extension (.gpkg, .shp, .geojson), its layer's creation options,
/// and the file (and a shapefile's sidecars, a GeoPackage's journal) removed before it is written.
pub fn vector_driver(path: &str) -> Result<(&'static str, &'static [&'static str]), String> {
    let p = std::path::Path::new(path);
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let (driver, options): (&str, &[&str]) = match ext.as_str() {
        "gpkg" => ("GPKG", &[]),
        "shp" => ("ESRI Shapefile", &["ENCODING=UTF-8"]),
        "geojson" | "json" => ("GeoJSON", &[]),
        _ => return Err(format!("{path}: a GeoPackage (.gpkg), shapefile (.shp) or GeoJSON (.geojson)")),
    };
    if driver == "ESRI Shapefile" {
        for side in ["shp", "shx", "dbf", "prj", "cpg", "qix", "sbn", "sbx"] {
            let _ = std::fs::remove_file(p.with_extension(side));
        }
    } else {
        for side in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{side}"));
        }
    }
    Ok((driver, options))
}

pub fn write_polygon(
    path: &str,
    layer: &str,
    crs_wkt: &str,
    rings: &[Ring],
    fields: &[(&str, FieldValue)],
) -> Result<(), String> {
    let wkt = to_wkt(rings).ok_or("no ring of three vertices")?;
    let geom = Geometry::from_wkt(&wkt).map_err(|e| e.to_string())?;
    let (driver, layer_options) = vector_driver(path)?;
    crate::vectors::register();
    let drv = gdal::DriverManager::get_driver_by_name(driver).map_err(|e| e.to_string())?;
    let srs = (!crs_wkt.is_empty()).then(|| crate::reproject::srs(crs_wkt)).flatten();
    let epsg = srs.as_ref().is_some_and(|s| {
        let mut s = s.clone();
        s.auth_name().is_some_and(|a| a.eq_ignore_ascii_case("EPSG"))
            || (s.auto_identify_epsg().is_ok() && s.auth_name().is_some_and(|a| a.eq_ignore_ascii_case("EPSG")))
    });
    let rfc7946 = driver == "GeoJSON" && srs.is_some() && !epsg;
    let mut ds = drv.create_vector_only(path).map_err(|e| format!("cannot create {path}: {e}"))?;
    let options: &[&str] = if rfc7946 { &["RFC7946=YES"] } else { layer_options };
    let ty =
        if wkt.starts_with("MULTI") { OGRwkbGeometryType::wkbMultiPolygon } else { OGRwkbGeometryType::wkbPolygon };
    {
        let l = ds
            .create_layer(LayerOptions { name: layer, srs: srs.as_ref(), ty, options: Some(options) })
            .map_err(|e| format!("cannot write {path}: {e}"))?;
        for (name, v) in fields {
            let ty = match v {
                FieldValue::Text(_) => OGRFieldType::OFTString,
                FieldValue::Integer(_) => OGRFieldType::OFTInteger64,
                FieldValue::Real(_) => OGRFieldType::OFTReal,
            };
            FieldDefn::new(name, ty).and_then(|d| d.add_to_layer(&l)).map_err(|e| e.to_string())?;
        }
        let mut f = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
        f.set_geometry(geom).map_err(|e| e.to_string())?;
        for (k, (_, v)) in fields.iter().enumerate() {
            match v {
                FieldValue::Text(s) => f.set_field_string(k, s),
                FieldValue::Integer(i) => f.set_field_integer64(k, *i),
                FieldValue::Real(x) => f.set_field_double(k, *x),
            }
            .map_err(|e| e.to_string())?;
        }
        f.create(&l).map_err(|e| format!("cannot write {path}: {e}"))?;
    }
    ds.close().map_err(|e| format!("cannot write {path}: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::{Kind, Placer, load_vector_file};

    fn square(x0: f64, y0: f64, s: f64) -> Ring {
        vec![x0, y0, x0 + s, y0, x0 + s, y0 + s, x0, y0 + s]
    }

    #[test]
    fn rings_nested() {
        // a shell, its hole, an island in the hole, and a second shell apart
        let rings = vec![square(0.0, 0.0, 10.0), square(2.0, 2.0, 6.0), square(4.0, 4.0, 2.0), square(20.0, 0.0, 3.0)];
        let mut n = nest(&rings);
        n.sort();
        assert_eq!(n, vec![(0, vec![1]), (2, vec![]), (3, vec![])]);
        let w = to_wkt(&rings).unwrap();
        assert!(w.starts_with("MULTIPOLYGON (((0 0, 10 0, 10 10, 0 10, 0 0), (2 2, 8 2"), "{w}");
        assert_eq!(to_wkt(&rings[..1]).unwrap(), "POLYGON ((0 0, 10 0, 10 10, 0 10, 0 0))");
        assert!(to_wkt(&[vec![0.0, 0.0, 1.0, 1.0]]).is_none());
    }

    #[test]
    fn written_and_read_back() {
        let d = std::env::temp_dir().join(format!("janus-geo-polygon-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        // UTM 23S: a shell with a hole, odd coordinates
        let utm = gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
        let rings = vec![
            vec![500000.123456789, 7600000.5, 503001.25, 7600010.0, 502500.0, 7597000.333, 500100.0, 7597500.0],
            vec![501000.0, 7599000.0, 502000.0, 7599000.0, 501500.0, 7598000.0],
        ];
        let gt = [499000.0, 30.0, 0.0, 7601000.0, 0.0, -30.0];
        for name in ["roi.gpkg", "roi.geojson", "roi.shp"] {
            let p = d.join(name).to_string_lossy().into_owned();
            let fields = [("name", FieldValue::Text("ROI".into())), ("pixels", FieldValue::Integer(1234))];
            write_polygon(&p, "roi", &utm, &rings, &fields).unwrap();
            // twice: replaced
            write_polygon(&p, "roi", &utm, &rings, &fields).unwrap();
            let v = &load_vector_file(&p, 1_000_000).unwrap()[0];
            assert_eq!(v.features.len(), 1, "{name}");
            let f = &v.features[0];
            assert_eq!(f.kind, Kind::Area);
            assert_eq!(f.values, ["ROI", "1234"]);
            assert_eq!(f.parts.len(), 2);
            // a shapefile's rings turned (its shells clockwise): the same pixels below
            let shp = name.ends_with(".shp");
            for (a, b) in f.parts.iter().zip(&rings) {
                assert_eq!(a.len(), b.len(), "{name}: the closing vertex dropped");
                for (x, y) in a.iter().zip(b).filter(|_| !shp) {
                    assert!((x - y).abs() < 1e-6, "{name}: {x} vs {y}");
                }
            }
            assert_eq!(v.crs_name, "EPSG:32723", "{name}");
            // the same pixels on the grid
            let p = Placer::new(v, &utm, Some(gt)).unwrap();
            let placed = p.placed(&f.parts);
            let mut ours = rings.clone();
            plane::transform(&mut ours, |x, y| Some(((x - gt[0]) / gt[1], (y - gt[3]) / gt[5])));
            let a = plane::mask(&placed, 0.0, 0.0, 200.0, 200.0, 200, 200);
            let b = plane::mask(&ours, 0.0, 0.0, 200.0, 200.0, 200, 200);
            assert!(a.iter().filter(|&&m| m).count() > 1000);
            assert_eq!(a, b, "{name}");
        }
        assert!(write_polygon(&d.join("x.kml").to_string_lossy(), "roi", &utm, &rings, &[]).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
