//! Sentinel-2's angles from a tile's MTD_TL.xml (`janus_zeit::s2angles`), a port of Zeit's `zeit.sentinel2_angles`:
//! the angles of real tile metadata (23LKC, a full tile and a partial swath: tests/data/s2angles) on grids in the
//! tile's CRS, in the neighbouring UTM zone, in geographic coordinates, and with two tiles for a date, equal to what
//! Zeit gave (tools/s2angles_fixtures.py): zeniths to 1e-4 degree, azimuths to 1e-3 degree (circular difference),
//! missing where Zeit has none; and Zeit's own synthetic cases (a tile whose fields are linear, so the interpolation
//! is known at any point).

use std::path::{Path, PathBuf};

use gdal::spatial_ref::SpatialRef;
use janus_zeit::s2angles::{self, Like, Prepared};
use serde_json::Value;

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/s2angles")
}

fn wkt(epsg: u32) -> String {
    SpatialRef::from_epsg(epsg).unwrap().to_wkt().unwrap()
}

/// A tile's view band B06 fused (Zeit's default).
fn tile(name: &str) -> Prepared {
    let text = std::fs::read_to_string(data().join(name)).unwrap();
    s2angles::read(&text, name, s2angles::band_id("B06").unwrap()).unwrap()
}

/// Whether two rows are the same values, missing ones too (bit for bit).
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

/// The angular distance in degrees.
fn circular(a: f32, b: f32) -> f64 {
    let d = (f64::from(a) - f64::from(b) + 180.0).rem_euclid(360.0) - 180.0;
    d.abs()
}

/// The comparison of two angle sets [4][pixels]: the largest zenith and azimuth differences, and how many pixels one
/// has missing and the other not.
fn compare(got: &[Vec<f32>; 4], want: &[Vec<f32>], what: &str) -> (f64, f64, usize, usize) {
    let (mut zen, mut az, mut present, mut mismatched) = (0.0f64, 0.0f64, 0, 0);
    for k in 0..4 {
        assert_eq!(got[k].len(), want[k].len(), "{what}");
        for (i, (g, w)) in got[k].iter().zip(&want[k]).enumerate() {
            if g.is_nan() != w.is_nan() {
                mismatched += 1;
                continue;
            }
            if g.is_nan() {
                continue;
            }
            present += 1;
            if k % 2 == 0 {
                zen = zen.max((f64::from(*g) - f64::from(*w)).abs());
                assert!((f64::from(*g) - f64::from(*w)).abs() < 1e-4, "{what}: zenith {k} pixel {i}: {g} vs {w}");
            } else {
                az = az.max(circular(*g, *w));
                assert!(circular(*g, *w) < 1e-3, "{what}: azimuth {k} pixel {i}: {g} vs {w}");
            }
        }
    }
    (zen, az, present, mismatched)
}

#[test]
fn real_tiles_are_zeits() {
    let cases: Value = serde_json::from_str(&std::fs::read_to_string(data().join("angles.json")).unwrap()).unwrap();
    let (mut values, mut worst_zen, mut worst_az) = (0, 0.0f64, 0.0f64);
    for c in cases["cases"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let epsg = c["epsg"].as_u64().unwrap() as u32;
        let f = |k: &str, i: usize| c[k][i].as_f64().unwrap();
        let (w, h) = (f("size", 0) as usize, f("size", 1) as usize);
        let gt = [f("origin", 0), f("pixel", 0), 0.0, f("origin", 1), 0.0, -f("pixel", 1)];
        let like = Like::new(&wkt(epsg), gt).unwrap();
        // the sources: a name, or a list of names (a date of several tiles); the one "(sensing time moved...)" is the
        // partial swath
        let entry = &c["sources"][0];
        let names: Vec<&str> = match entry {
            Value::Array(a) => a.iter().map(|v| v.as_str().unwrap()).collect(),
            v => vec![v.as_str().unwrap()],
        };
        let tiles: Vec<Prepared> = names.iter().map(|n| tile(n.split(" (").next().unwrap())).collect();
        let refs: Vec<&Prepared> = tiles.iter().collect();
        let got = s2angles::angles(&refs, &like, [0, 0, w, h]).unwrap();
        // Zeit's angles: [4][rows][columns] float32
        let bytes = std::fs::read(data().join(c["file"].as_str().unwrap())).unwrap();
        let all: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        assert_eq!(all.len(), 4 * w * h, "{name}");
        let want: Vec<Vec<f32>> = all.chunks(w * h).map(|p| p.to_vec()).collect();
        let (zen, az, present, mismatched) = compare(&got, &want, name);
        println!(
            "{name}: {w} x {h} pixels, {present} values equal to Zeit's, zeniths within {zen:.2e}, azimuths within {az:.2e} \
             degrees, {mismatched} missing in one only"
        );
        assert_eq!(mismatched, 0, "{name}: the pixels with no angle are Zeit's");
        values += present;
        worst_zen = worst_zen.max(zen);
        worst_az = worst_az.max(az);
        // the same window in strips of rows and in a corner of it (the offsets of a raster run's chunks)
        let strips = [(0, h / 3), (h / 3, h / 2 + 7), (h / 2 + 7, h)];
        for (a, b) in strips {
            let part = s2angles::angles(&refs, &like, [0, a, w, b - a]).unwrap();
            for k in 0..4 {
                assert!(same(&part[k], &got[k][a * w..b * w]), "{name}: strip {a}..{b} channel {k}");
            }
        }
        let (x0, y0) = (w / 4, h / 5);
        let corner = s2angles::angles(&refs, &like, [x0, y0, w - x0 - 3, h - y0 - 2]).unwrap();
        for k in 0..4 {
            for row in 0..h - y0 - 2 {
                let lo = (y0 + row) * w + x0;
                assert!(same(&corner[k][row * (w - x0 - 3)..(row + 1) * (w - x0 - 3)], &got[k][lo..lo + w - x0 - 3]));
            }
        }
    }
    println!(
        "{values} values compared with zeit.sentinel2_angles, zeniths within {worst_zen:.2e}, azimuths {worst_az:.2e}"
    );
    assert!(values > 300_000);
}

#[test]
fn the_partial_swath_has_no_view_angle_where_no_detector_covers() {
    let want = tile("s2_23LKC_20230726_partial_MTD_TL.xml");
    let like = Like::new(&wkt(32723), [199980.0 + 1010.0, 250.0, 0.0, 8300020.0 - 2020.0, 0.0, -250.0]).unwrap();
    let a = s2angles::angles(&[&want], &like, [0, 0, 120, 100]).unwrap();
    // the sun's grid is complete; the view's is a strip
    assert!(a[0].iter().all(|v| v.is_finite()) && a[1].iter().all(|v| v.is_finite()));
    let valid = a[2].iter().filter(|v| v.is_finite()).count();
    assert!(valid > 5000 && valid < 12000, "{valid}");
    assert_eq!(a[2].iter().filter(|v| v.is_finite()).count(), a[3].iter().filter(|v| v.is_finite()).count());
}

#[test]
fn a_tile_sensed_more_than_a_day_from_its_date_is_refused() {
    let full = tile("s2_23LKC_20230723_MTD_TL.xml");
    // 2023-07-23 (sensed at 13:26 UTC) is day 19561 of 1970-01-01: its midnight and the next one are within a day
    for day in [19561, 19562] {
        full.check_date(day).unwrap_or_else(|e| panic!("{day}: {e}"));
    }
    assert!(full.check_date(19560).is_err());
    let e = full.check_date(19566).unwrap_err();
    assert!(e.contains("sensed on 2023-07-23") && e.contains("2023-07-28"), "{e}");
    assert!(full.check_date(19558).is_err());
}

#[test]
fn bad_input_is_named() {
    let xml = std::fs::read_to_string(data().join("s2_23LKC_20230723_MTD_TL.xml")).unwrap();
    let g = s2angles::parse(&xml, "t").unwrap();
    assert!(g.prepare(5).is_ok());
    assert!(g.prepare(13).unwrap_err().contains("no view angles for band id 13"));
    assert!(s2angles::parse("<a><b/></a>", "x").unwrap_err().contains("no Tile_Geocoding"));
    assert!(s2angles::parse("<a><b></a>", "x").is_err());
    assert_eq!(g.crs, "EPSG:32723");
    assert_eq!((g.ulx, g.uly, g.step), (199980.0, 8300020.0, (5000.0, 5000.0)));
    assert!(Like::new("", [0.0, 1.0, 0.0, 0.0, 0.0, -1.0]).unwrap_err().contains("no CRS"));
    assert!(Like::new(&wkt(32723), [0.0, 1.0, 0.1, 0.0, 0.0, -1.0]).unwrap_err().contains("rotated"));
}

// --- Zeit's synthetic tile (tests/test_s2angles.py): every field linear in the node's row i and column j ---------

const ULX: f64 = 300000.0;
const ULY: f64 = 8000000.0;
const STEP: f64 = 5000.0;
const N: usize = 23;

fn sun_zenith(i: f64, j: f64) -> f64 {
    30.0 + 0.1 * j + 0.05 * i
}

fn sun_azimuth(i: f64, j: f64) -> f64 {
    100.0 + 0.2 * i - 0.3 * j
}

fn view_zenith(i: f64, j: f64) -> f64 {
    5.0 + 0.1 * j + 0.02 * i
}

fn view_azimuth(i: f64, j: f64) -> f64 {
    (358.0 + 0.5 * j + 0.1 * i).rem_euclid(360.0)
}

fn values(f: impl Fn(usize, usize) -> f64) -> String {
    let rows: String = (0..N)
        .map(|i| {
            let row: Vec<String> = (0..N)
                .map(|j| {
                    let v = f(i, j);
                    if v.is_finite() { format!("{v:?}") } else { "NaN".to_string() }
                })
                .collect();
            format!("<VALUES>{}</VALUES>", row.join(" "))
        })
        .collect();
    format!("<COL_STEP unit=\"m\">5000</COL_STEP><ROW_STEP unit=\"m\">5000</ROW_STEP><Values_List>{rows}</Values_List>")
}

/// `make_xml`: band 5's detector 1 covers the columns `cover[0]`, detector 2 (zenith +0.5, azimuth +2 degrees) `cover[1]`.
fn make_xml(cover: [(usize, usize); 2], sensing: &str) -> String {
    let sun = format!(
        "<Sun_Angles_Grid><Zenith>{}</Zenith><Azimuth>{}</Azimuth></Sun_Angles_Grid>",
        values(|i, j| sun_zenith(i as f64, j as f64)),
        values(|i, j| sun_azimuth(i as f64, j as f64))
    );
    let mut views = String::new();
    for (d, (lo, hi)) in cover.iter().enumerate() {
        let on = |j: usize| (*lo..=*hi).contains(&j);
        let (dz, da) = if d == 1 { (0.5, 2.0) } else { (0.0, 0.0) };
        let z = values(|i, j| if on(j) { view_zenith(i as f64, j as f64) + dz } else { f64::NAN });
        let a = values(|i, j| if on(j) { (view_azimuth(i as f64, j as f64) + da).rem_euclid(360.0) } else { f64::NAN });
        views += &format!(
            "<Viewing_Incidence_Angles_Grids bandId=\"5\" detectorId=\"{}\"><Zenith>{z}</Zenith><Azimuth>{a}</Azimuth>\
             </Viewing_Incidence_Angles_Grids>",
            d + 1
        );
    }
    let geo: String = [10, 20, 60]
        .iter()
        .map(|r| format!("<Geoposition resolution=\"{r}\"><ULX>300000</ULX><ULY>8000000</ULY><XDIM>{r}</XDIM><YDIM>-{r}</YDIM></Geoposition>"))
        .collect();
    format!(
        "<?xml version=\"1.0\"?><n1:Level-2A_Tile_ID xmlns:n1=\"https://psd-14.sentinel2.eo.esa.int/PSD/x.xsd\">\
         <n1:General_Info><SENSING_TIME>{sensing}</SENSING_TIME></n1:General_Info>\
         <n1:Geometric_Info><Tile_Geocoding><HORIZONTAL_CS_CODE>EPSG:32723</HORIZONTAL_CS_CODE>{geo}</Tile_Geocoding>\
         <Tile_Angles>{sun}<Mean_Sun_Angle><ZENITH_ANGLE>1</ZENITH_ANGLE></Mean_Sun_Angle>{views}</Tile_Angles>\
         </n1:Geometric_Info></n1:Level-2A_Tile_ID>"
    )
}

fn synthetic(cover: [(usize, usize); 2]) -> Prepared {
    s2angles::read(&make_xml(cover, "2023-07-23T13:36:41.935298Z"), "synthetic", 5).unwrap()
}

/// The angles at points given in nodes (u columns east, v rows south of the tile's corner), one pixel each, on a grid
/// whose pixel centre is the point (north up, or south up).
fn at(tiles: &[&Prepared], points: &[(f64, f64)], south_up: bool) -> [Vec<f32>; 4] {
    let mut out: [Vec<f32>; 4] = Default::default();
    for &(u, v) in points {
        let (x, y) = (ULX + u * STEP, ULY - v * STEP);
        let gt =
            if south_up { [x - 0.5, 1.0, 0.0, y - 0.5, 0.0, 1.0] } else { [x - 0.5, 1.0, 0.0, y + 0.5, 0.0, -1.0] };
        let like = Like::new(&wkt(32723), gt).unwrap();
        for (o, a) in out.iter_mut().zip(s2angles::angles(tiles, &like, [0, 0, 1, 1]).unwrap()) {
            o.extend(a);
        }
    }
    out
}

#[test]
fn interpolation_at_known_points() {
    // single detector (columns 0-9); nodes and the middle of cells; azimuths wrap (358 + 0.5 j crosses 360)
    let t = synthetic([(0, 12), (10, 22)]);
    let (us, vs) = ([0.0, 0.3, 4.5, 8.25, 9.0], [0.0, 1.5, 7.2, 21.9]);
    let points: Vec<(f64, f64)> = vs.iter().flat_map(|&v| us.iter().map(move |&u| (u, v))).collect();
    for south_up in [false, true] {
        let a = at(&[&t], &points, south_up);
        for (k, &(u, v)) in points.iter().enumerate() {
            // south up: the same y, the grid's rows run the other way: the point is the same
            assert!((f64::from(a[0][k]) - sun_zenith(v, u)).abs() < 2e-5, "{u} {v}: {}", a[0][k]);
            assert!(circular(a[1][k], sun_azimuth(v, u) as f32) < 2e-3);
            assert!((f64::from(a[2][k]) - view_zenith(v, u)).abs() < 2e-5, "{u} {v}: {}", a[2][k]);
            assert!(
                circular(a[3][k], view_azimuth(v, u) as f32) < 2e-3,
                "{u} {v}: {} vs {}",
                a[3][k],
                view_azimuth(v, u)
            );
            assert!((0.0..360.0).contains(&a[3][k]));
        }
    }
}

#[test]
fn detectors_are_fused_by_the_mean_per_cell() {
    // at the nodes of row 4: detector 1 alone (j < 10), both (10-12), detector 2 alone (j > 12)
    let t = synthetic([(0, 12), (10, 22)]);
    let points: Vec<(f64, f64)> = (0..23).map(|j| (j as f64, 4.0)).collect();
    let a = at(&[&t], &points, false);
    for (j, _) in points.iter().enumerate() {
        let off = if (10..=12).contains(&j) {
            0.25
        } else if j > 12 {
            0.5
        } else {
            0.0
        };
        assert!((f64::from(a[2][j]) - (view_zenith(4.0, j as f64) + off)).abs() < 2e-5, "column {j}: {}", a[2][j]);
        // the circular mean of a and a + 2 is a + 1 on the overlap, a + 2 where only detector 2 sees
        let off = if (10..=12).contains(&j) {
            1.0
        } else if j > 12 {
            2.0
        } else {
            0.0
        };
        let want = (view_azimuth(4.0, j as f64) + off).rem_euclid(360.0) as f32;
        assert!(circular(a[3][j], want) < 2e-3, "column {j}: {} vs {want}", a[3][j]);
    }
    // between nodes of the overlap the field is the bilinear of the fused nodes
    let b = at(&[&t], &[(10.5, 4.0), (11.9, 4.0), (9.5, 4.0)], false);
    for (g, w) in
        b[2].iter().zip([view_zenith(4.0, 10.5) + 0.25, view_zenith(4.0, 11.9) + 0.25, view_zenith(4.0, 9.5) + 0.125])
    {
        assert!((f64::from(*g) - w).abs() < 2e-5, "{g} vs {w}");
    }
}

#[test]
fn nan_where_no_detector_covers_and_flat_to_the_valid_corner() {
    // only detector 1, columns 0-12
    let t = synthetic([(0, 12), (30, 31)]);
    let us = [11.0, 12.0, 12.5, 12.999, 13.2, 20.0];
    let a = at(&[&t], &us.map(|u| (u, 5.0)), false);
    for (k, u) in [11.0, 12.0].iter().enumerate() {
        assert!((f64::from(a[2][k]) - view_zenith(5.0, *u)).abs() < 2e-5);
    }
    // between the last valid node (12) and the invalid one (13) the valid corner's value is kept
    for k in [2, 3] {
        assert!((f64::from(a[2][k]) - view_zenith(5.0, 12.0)).abs() < 2e-5, "{}", a[2][k]);
    }
    // the cell 13-14 has no valid corner
    assert!(a[2][4..].iter().all(|v| v.is_nan()) && a[3][4..].iter().all(|v| v.is_nan()));
    // the sun's grid is complete
    assert!(a[0].iter().all(|v| v.is_finite()) && a[1].iter().all(|v| v.is_finite()));
}

#[test]
fn outside_the_grid_is_nan() {
    let t = synthetic([(0, 22), (30, 31)]);
    let pts = [
        (-100.0 / STEP, 10.0),
        (0.0, -100.0 / STEP),
        (110000.0 / STEP, 3.0),
        (110100.0 / STEP, 3.0),
        (3.0, 22.0 + 50.0 / STEP),
        (1.0, 3.0),
    ];
    let a = at(&[&t], &pts, false);
    let finite: Vec<bool> = a[0].iter().map(|v| v.is_finite()).collect();
    assert_eq!(finite, [false, false, true, false, false, true]);
}

#[test]
fn several_tiles_of_a_date_take_the_first_with_a_view_angle() {
    // the first reaches column 12 only; the second all of them (no offset)
    let a = synthetic([(0, 12), (30, 31)]);
    let b = synthetic([(0, 22), (30, 31)]);
    let points = [(6.0, 5.0), (16.0, 5.0)];
    let one = at(&[&a], &points, false);
    assert!(one[2][0].is_finite() && one[2][1].is_nan());
    let both = at(&[&a, &b], &points, false);
    assert!(both[2].iter().all(|v| v.is_finite()));
    assert_eq!(both[2][0], one[2][0]);
    assert!((f64::from(both[2][1]) - view_zenith(5.0, 16.0)).abs() < 2e-5);
    // where the first has no view zenith all four angles come from the next tile (the sun's too, as Zeit's)
    let alone = at(&[&b], &points, false);
    assert_eq!(both[0][1], alone[0][1]);
}
