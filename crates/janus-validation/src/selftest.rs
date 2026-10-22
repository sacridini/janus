//! --selftest-validation: the estimators against the example of Olofsson et al. (2014) (numbers of zeit.accuracy,
//! which match the paper's), the design against zeit.sampling_design, the strata of synthetic maps (buffer, limit,
//! a second map on another grid, bands of a few rows) against a pixel-by-pixel count, the sample, the project's
//! file, and, with a Python with Zeit, the estimates of a labelled project against zeit.accuracy run on its files
//! (the code of "Copy Python"). --measure-validation: the parts timed on a real map (src/validation_selftest.cpp).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager};
use janus_geo::plane::Polygon;
use serde_json::Value;

use crate::reader::TEST_BAND_ROWS;
use crate::*;

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol * b.abs().max(1.0)
}

fn utm23s() -> String {
    SpatialRef::from_epsg(32723).and_then(|s| s.to_wkt()).unwrap_or_default()
}

fn write_raster(path: &str, w: usize, h: usize, gt: &[f64; 6], v: &[f32], nodata: Option<f64>) -> Result<(), String> {
    let d = DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let mut ds = d.create_with_band_type::<f32, _>(path, w, h, 1).map_err(|e| e.to_string())?;
    ds.set_geo_transform(gt).map_err(|e| e.to_string())?;
    ds.set_projection(&utm23s()).map_err(|e| e.to_string())?;
    let mut b = ds.rasterband(1).map_err(|e| e.to_string())?;
    if nodata.is_some() {
        b.set_no_data_value(nodata).map_err(|e| e.to_string())?;
    }
    let mut buf = Buffer::new((w, h), v.to_vec());
    b.write((0, 0), (w, h), &mut buf).map_err(|e| e.to_string())
}

fn read_raster(path: &str) -> (Vec<f32>, usize, usize) {
    let Ok(ds) = Dataset::open(path) else { return (Vec::new(), 0, 0) };
    let (w, h) = ds.raster_size();
    let v = ds
        .rasterband(1)
        .and_then(|b| b.read_as::<f32>((0, 0), (w, h), (w, h), None))
        .map(|b| b.into_shape_and_vec().1)
        .unwrap_or_default();
    (v, w, h)
}

/// Olofsson et al. (2014), tables 8-10 (zeit.accuracy's numbers); the design (zeit.sampling_design on the same
/// map); interpreters and dates.
fn test_olofsson(rep: &mut String) -> Result<(), &'static str> {
    let px = [200000.0, 150000.0, 3200000.0, 6450000.0];
    let table = [[66, 0, 5, 4], [0, 55, 8, 12], [1, 0, 153, 11], [2, 1, 9, 313]];
    let mut inp = AssessInput {
        classes: ["1", "2", "3", "4"].map(String::from).to_vec(),
        confidence: 0.95,
        ..Default::default()
    };
    for p in px {
        inp.stratum_area.push(p * 0.09);
        inp.mapped.push(p * 0.09);
    }
    for i in 0..4 {
        for j in 0..4 {
            for _ in 0..table[i][j] {
                inp.stratum.push(Some(i));
                inp.map.push(Some(i));
                inp.reference.push(Some(j));
            }
        }
    }
    let a = assess(&inp);
    rep.push_str(&format!(
        "  Olofsson et al. (2014): overall {:.4} +- {:.4}, deforestation {:.0} +- {:.0} ha\n",
        a.overall.est,
        a.overall.hi - a.overall.est,
        a.area[0].est,
        a.area[0].ci
    ));
    let t = 1e-8;
    if a.n != 640 {
        return Err("Olofsson: 640 points expected");
    }
    if !near(a.overall.est, 0.9465118881, t) || !near(a.overall.se, 0.0094304172, t) {
        return Err("Olofsson: overall accuracy");
    }
    if !near(a.users[1].est, 0.7333333333, t) || !near(a.users[1].se, 0.0514066401, t) {
        return Err("Olofsson: user's accuracy");
    }
    if !near(a.producers[0].est, 0.7486614048, t) || !near(a.producers[0].se, 0.1088315576, t) {
        return Err("Olofsson: producer's accuracy");
    }
    if !near(a.producers[1].hi, 1.1015600840, t) {
        return Err("Olofsson: producer's interval");
    }
    if !near(a.area[0].est, 21157.7622377622, t) || !near(a.area[0].hi, 27315.2834758526, t) {
        return Err("Olofsson: area");
    }
    if !near(a.area[3].lo, 565104.7966731654, t) {
        return Err("Olofsson: area of stable non-forest");
    }
    if !near(a.confusion[3][0], 0.0039692308, 1e-7) || !near(a.confusion[0][0], 0.0176, 1e-9) {
        return Err("Olofsson: matrix");
    }
    if !near(z_of(0.95), 1.959963984540054, 1e-12) || !near(z_of(0.9), 1.6448536269514722, 1e-12) {
        return Err("normal quantile");
    }

    let ua = [0.7, 0.6, 0.9, 0.95];
    let mut s: Vec<Stratum> = (0..4)
        .map(|i| Stratum {
            pixels: px[i] as i64,
            area: px[i] * 0.09,
            weight: px[i] / 1e7,
            expected_ua: ua[i],
            change: i < 2,
            ..Default::default()
        })
        .collect();
    if sample_size(&s, 0.01) != 641 {
        return Err("sample size (eq. 13)");
    }
    let cases: [(Alloc, i32, i32, [i32; 4]); 6] = [
        (Alloc::Proportional, 641, 50, [50, 50, 179, 362]),
        (Alloc::Equal, 641, 0, [161, 160, 160, 160]),
        (Alloc::Neyman, 641, 0, [23, 19, 243, 356]),
        (Alloc::Proportional, 641, 100, [100, 100, 146, 295]),
        (Alloc::Neyman, 641, 75, [75, 75, 199, 292]),
        (Alloc::Proportional, 300, 100, [100, 100, 100, 193]),
    ];
    for (a, n, min, want) in cases {
        if allocate(&s, n, a, min) != want {
            rep.push_str(&format!("  allocation {}, n {n}, at least {min}\n", a.name()));
            return Err("allocation differs from zeit.sampling_design");
        }
    }
    let n = allocate(&s, 641, Alloc::Proportional, 50);
    for i in 0..4 {
        s[i].n = n[i];
    }
    let e = expected_errors(&s, 900000.0, 0.95);
    if !(e.overall_se > 0.005 && e.overall_se < 0.02) || !e.change {
        return Err("expected errors");
    }

    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let g = cohen(&v(&["y", "y", "n", "n", "y", ""]), &v(&["y", "n", "n", "n", "y", "n"]));
    if g.n != 5 || !near(g.observed, 0.8, 1e-12) || !near(g.kappa, 0.32 / 0.52, 1e-12) {
        return Err("Cohen's kappa");
    }
    let est = Estimator::new(&[1.0, 1.0], &[Some(0), Some(0), Some(1), Some(1)], 0.95);
    let d = date_agreement(&est, &[2010, 2012, 2015, 0], &[2010, 2010, 2016, 2016], 1.0);
    if d.n != 3 || !near(d.within.est, 2.0 / 3.0, 1e-12) || d.diffs.get(&2) != Some(&1) {
        return Err("date agreement");
    }
    let named = MapRule {
        kind: MapKind::Intervals,
        bins: vec![-1.0, -0.1, 1.0],
        names: [(3, "Forest".to_string()), (15, "Pasture".to_string())].into(),
        change: [1].into(),
        ..Default::default()
    };
    let back = MapRule::from_json(&serde_json::from_str::<Value>(&named.to_json().to_string()).unwrap());
    if back.names != named.names || back.bins != named.bins || back.change != named.change || back.kind != named.kind {
        return Err("a map's rule written and read back");
    }
    if year_of("2019-07-14") != 2019 || year_of("2005") != 2005 || year_of("12345") != 0 || year_of("") != 0 {
        return Err("years of dates");
    }
    Ok(())
}

/// The self-test; `python`: an interpreter with Zeit to compare with zeit.accuracy (None: not compared).
pub fn self_test(rep: &mut String, python: Option<&str>) -> Result<(), &'static str> {
    test_olofsson(rep)?;

    // synthetic maps: A, years of change (0 none, NaN no data in a corner), 30 m in UTM 23S; B, classes at 60 m
    // whose grid starts 10 m off A's
    let dir = std::env::temp_dir().join("janus-selftest-validation");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let (w, h) = (173usize, 131usize);
    let gt_a = [500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];
    let mut a = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (xi, yi) = (x as i64, y as i64);
            a[y * w + x] = if x < 6 && y < 5 {
                f32::NAN
            } else if (xi - 40) * (xi - 40) + (yi - 30) * (yi - 30) < 120 {
                2004.0
            } else if x > 100 && x < 130 && y > 70 && y < 80 {
                2015.0
            } else if (x * 7 + y * 13) % 97 == 0 {
                (2001 + (x + y) % 20) as f32
            } else {
                0.0
            };
        }
    }
    let (wb, hb) = (90usize, 70usize);
    let gt_b = [500010.0, 60.0, 0.0, 7500010.0, 0.0, -60.0];
    let mut bm = vec![0f32; wb * hb];
    for y in 0..hb {
        for x in 0..wb {
            bm[y * wb + x] = (1 + (x / 9 + y / 7) % 3) as f32;
        }
    }
    let pa = dir.join("a.tif").to_string_lossy().into_owned();
    let pb = dir.join("b.tif").to_string_lossy().into_owned();
    if write_raster(&pa, w, h, &gt_a, &a, None).is_err() || write_raster(&pb, wb, hb, &gt_b, &bm, Some(0.0)).is_err() {
        return Err("cannot write the test maps");
    }
    let ra =
        MapRule { path: pa.clone(), kind: MapKind::Events, bins: vec![2000.0, 2010.0, 2021.0], ..Default::default() };
    let rb = MapRule { path: pb.clone(), kind: MapKind::Classes, change: [2].into(), ..Default::default() };

    // B under A's pixel centre
    let b_at = |x: usize, y: usize| -> f64 {
        let gx = gt_a[0] + (x as f64 + 0.5) * gt_a[1];
        let gy = gt_a[3] + (y as f64 + 0.5) * gt_a[5];
        let bx = ((gx - gt_b[0]) / gt_b[1]).floor() as i64;
        let by = ((gy - gt_b[3]) / gt_b[5]).floor() as i64;
        if bx < 0 || by < 0 || bx >= wb as i64 || by >= hb as i64 {
            return f64::NAN;
        }
        f64::from(bm[by as usize * wb + bx as usize])
    };
    let limit: Polygon = vec![vec![3.2, 2.7, 168.4, 9.1, 150.3, 128.6, 10.7, 101.2]];
    let cases: [(&str, Vec<MapRule>, i32, bool); 2] = [
        ("events in periods, buffer of 2 px, inside a polygon", vec![ra.clone()], 2, true),
        ("agreement of two maps on different grids, buffer of 1 px", vec![ra.clone(), rb.clone()], 1, false),
    ];
    let mut kept = Strata::default();
    let mut kept_path = String::new();
    for (ci, (name, maps, buffer, has_limit)) in cases.iter().enumerate() {
        let spec = StrataSpec {
            maps: maps.clone(),
            buffer: *buffer,
            limit: if *has_limit { limit.clone() } else { Vec::new() },
        };
        let out = dir.join(format!("strata_{ci}.tif")).to_string_lossy().into_owned();
        TEST_BAND_ROWS.store(7, Ordering::Relaxed);
        let r = compute_strata(&spec, &out, None);
        TEST_BAND_ROWS.store(0, Ordering::Relaxed);
        let s = match r {
            Ok(s) => s,
            Err(e) => {
                rep.push_str(&format!("  {e}\n"));
                return Err("computeStrata failed");
            }
        };
        // expected key and change per pixel
        let m = maps.len();
        let mut key = vec![NONE; w * h];
        let mut chg = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if m == 1 {
                    key[i] = ra.class_of(f64::from(a[i]));
                    chg[i] = ra.is_change(key[i]);
                } else {
                    let (ka, kb) = (ra.class_of(f64::from(a[i])), rb.class_of(b_at(x, y)));
                    if ka == NONE || kb == NONE {
                        continue;
                    }
                    key[i] = i32::from(ra.is_change(ka)) + i32::from(rb.is_change(kb));
                    chg[i] = key[i] > 0;
                }
            }
        }
        let (got, w2, h2) = read_raster(&out);
        if w2 != w || h2 != h {
            return Err("the strata raster has another size");
        }
        let mut want: BTreeMap<String, i64> = BTreeMap::new();
        let have: BTreeMap<String, i64> = s.strata.iter().map(|t| (t.name.clone(), t.pixels)).collect();
        let mut bad = 0;
        let bu = *buffer as i64;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let mut nm = String::new();
                let inside = !has_limit || janus_geo::plane::contains(&limit, x as f64 + 0.5, y as f64 + 0.5);
                if key[i] != NONE && inside {
                    let mut dil = false;
                    for dy in -bu..=bu {
                        for dx in -bu..=bu {
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            dil |= xx >= 0
                                && yy >= 0
                                && xx < w as i64
                                && yy < h as i64
                                && chg[yy as usize * w + xx as usize];
                        }
                    }
                    nm = if bu > 0 && !chg[i] && dil {
                        format!("buffer ({bu} px)")
                    } else if m == 1 {
                        ra.class_name(key[i])
                    } else if key[i] == 0 {
                        "no change in any map".into()
                    } else if key[i] == 2 {
                        "change in all 2 maps".into()
                    } else {
                        "change in 1 of 2 maps".into()
                    };
                    *want.entry(nm.clone()).or_insert(0) += 1;
                }
                let code = got[i] as i32;
                let got_name =
                    if code != 0 { s.index_of_code(code).map_or("", |k| s.strata[k].name.as_str()) } else { "" };
                bad += i64::from(got_name != nm);
            }
        }
        rep.push_str(&format!("  strata, {name}: {} strata, {bad} pixel(s) differ\n", s.strata.len()));
        if bad != 0 {
            return Err("the strata raster differs from the pixel-by-pixel count");
        }
        if want != have {
            return Err("the strata's pixels differ from the pixel-by-pixel count");
        }
        if s.strata.iter().any(|t| !near(t.area, t.pixels as f64 * 0.09, 1e-9)) {
            return Err("the strata's areas (30 m pixels: 0.09 ha)");
        }
        let mapped_total: f64 = s.mapped[0].values().sum();
        if !near(mapped_total, s.total_area(), 1e-9) {
            return Err("the mapped areas do not add up to the strata's");
        }
        if *has_limit {
            kept = s;
            kept_path = out;
        }
    }

    // the sample: the counts asked, each point in its stratum, no pixel twice, the same again with the same seed;
    // then more points, none of the first
    let n: Vec<i32> = kept.strata.iter().map(|t| t.pixels.min(25) as i32).collect();
    let (p1, p2) = match (draw_sample(&kept, &kept_path, &n, 7, &[]), draw_sample(&kept, &kept_path, &n, 7, &[])) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            rep.push_str(&format!("  {e}\n"));
            return Err("drawSample failed");
        }
    };
    let (st, _, _) = read_raster(&kept_path);
    let check_points = |pts: &[Point], seen: &mut BTreeSet<(i32, i32)>| -> Result<(), &'static str> {
        for q in pts {
            if st[q.row as usize * w + q.col as usize] as i32 != q.stratum {
                return Err("a point is not in its stratum");
            }
            if !seen.insert((q.row, q.col)) {
                return Err("a pixel drawn twice");
            }
            if kept.grid.centre(i64::from(q.col), i64::from(q.row)) != (q.x, q.y) {
                return Err("a point is not at its pixel's centre");
            }
        }
        Ok(())
    };
    let mut seen = BTreeSet::new();
    check_points(&p1, &mut seen)?;
    if p1.len() != p2.len()
        || p1.iter().zip(&p2).enumerate().any(|(i, (a, b))| a.row != b.row || a.col != b.col || a.id != i as i32 + 1)
    {
        return Err("the same seed gave another sample");
    }
    let mut per: BTreeMap<i32, i32> = BTreeMap::new();
    for q in &p1 {
        *per.entry(q.stratum).or_insert(0) += 1;
    }
    if kept.strata.iter().enumerate().any(|(i, t)| per.get(&t.code).copied().unwrap_or(0) != n[i]) {
        return Err("a stratum got another number of points");
    }
    let mut more = vec![0; kept.strata.len()];
    more[0] = 10;
    let p3 = draw_sample(&kept, &kept_path, &more, 7, &p1).map_err(|_| "drawSample (more points) failed")?;
    if p3.len() != 10 || p3[0].id != p1.len() as i32 + 1 {
        return Err("more points: 10, with the ids after the first");
    }
    check_points(&p3, &mut seen)?;
    {
        let mut counted = kept.clone();
        counted.row_counts.clear();
        if count_strata(&kept_path, &mut counted, None).is_err() || counted.row_counts != kept.row_counts {
            return Err("countStrata differs from computeStrata");
        }
    }
    rep.push_str(&format!("  sample: {} points, the same with the same seed, 10 more drawn apart\n", p1.len()));

    // the project: written, read back, labelled by two interpreters, adjudicated, merged
    let mut pr = Project {
        path: dir.join("project.gpkg").to_string_lossy().into_owned(),
        strata_path: kept_path.clone(),
        id: new_project_id(),
        spec: StrataSpec { maps: vec![ra.clone()], buffer: 2, limit: limit.clone() },
        strata: kept.clone(),
        seed: 7,
        design: serde_json::json!({"alloc": "proportional"}),
        label_set: ["Stable", "Deforestation", "Can't tell"].map(String::from).to_vec(),
        ..Default::default()
    };
    pr.strata.row_counts.clear();
    pr.points = p1.clone();
    pr.points.extend_from_slice(&p3);
    let am = AssessedMap { rule: ra.clone(), mapping: default_mapping(&ra, &pr.label_set, &[]) };
    pr.maps = vec![am.clone()];
    let mp = |k: &str| am.mapping.get(k).cloned();
    if mp("Deforestation").as_deref() != Some(CHANGE)
        || mp("Stable").as_deref() != Some(NO_CHANGE)
        || mp("Can't tell").as_deref() != Some("")
    {
        return Err("the default translation of labels for a map of events");
    }
    if let Err(e) = create_project(&pr) {
        rep.push_str(&format!("  {e}\n"));
        return Err("createProject failed");
    }
    // labels: the map's own class, every 6th point the other way; "ana" all, "bruno" a few, one disagreement
    let xy: Vec<[f64; 2]> = pr.points.iter().map(|q| [q.x, q.y]).collect();
    let va = values_at(&ra, &kept.grid.wkt, &xy).map_err(|_| "valuesAt failed")?;
    for (i, q) in pr.points.iter().enumerate() {
        let v = a[q.row as usize * w + q.col as usize];
        if !(if v.is_nan() { va[i].is_nan() } else { va[i] == f64::from(v) }) {
            return Err("valuesAt: not the map's value");
        }
    }
    let mut ana: Vec<(usize, Label)> = Vec::new();
    let mut bruno: Vec<(usize, Label)> = Vec::new();
    for i in 0..pr.points.len() {
        let change = ra.is_change(ra.class_of(va[i]));
        let mut l = Label {
            label: if change != (i % 6 == 0) { "Deforestation" } else { "Stable" }.into(),
            confidence: 1 + (i % 3) as i32,
            updated: now_iso(),
            ..Default::default()
        };
        if l.label == "Deforestation" {
            l.date = format!("{}-08-01", if change { va[i] as i32 } else { 2012 });
        }
        if i == 3 {
            l.label = "Can't tell".into();
        }
        if i == 5 {
            l.events.push(Event { label: "Deforestation".into(), date: "2018".into() });
        }
        ana.push((i, l.clone()));
        if i < 12 {
            let mut m = l;
            if i == 1 {
                m.label = if m.label == "Stable" { "Deforestation" } else { "Stable" }.into();
            }
            bruno.push((i, m));
        }
    }
    let finals = |q: &Project| -> Vec<FinalRef> {
        (0..q.points.len())
            .map(|i| {
                let (r, conflict) = resolve(&q.labels, i);
                FinalRef { point: i, reference: r.cloned().unwrap_or_default(), conflict, interpreters: 0 }
            })
            .collect()
    };
    let mut q = pr.clone();
    for (who, rows) in [("ana", &ana), ("bruno", &bruno)] {
        let v = q.labels.entry(who.into()).or_default();
        v.resize(q.points.len(), Label::default());
        for (i, l) in rows {
            v[*i] = l.clone();
        }
    }
    if let Err(e) =
        save_labels(&pr.path, "ana", &ana, &[]).and_then(|_| save_labels(&pr.path, "bruno", &bruno, &finals(&q)))
    {
        rep.push_str(&format!("  {e}\n"));
        return Err("saveLabels failed");
    }
    if !is_project(&pr.path) || is_project(&pa) {
        return Err("isProject");
    }
    let mut back = match load_project(&pr.path) {
        Ok(b) => b,
        Err(e) => {
            rep.push_str(&format!("  {e}\n"));
            return Err("loadProject failed");
        }
    };
    if back.points.len() != pr.points.len()
        || back.strata.strata.len() != pr.strata.strata.len()
        || back.id != pr.id
        || back.seed != 7
        || back.label_set != pr.label_set
        || back.maps.len() != 1
        || back.maps[0].mapping != am.mapping
        || back.spec.limit != limit
        || back.spec.buffer != 2
        || Path::new(&back.strata_path) != Path::new(&kept_path)
        || back.strata.mapped.len() != 1
    {
        return Err("the project read back differs");
    }
    if back.points.iter().zip(&pr.points).any(|(a, b)| a.row != b.row || a.stratum != b.stratum || a.x != b.x) {
        return Err("a point read back differs");
    }
    if back.labels.len() != 2
        || back.labels["ana"][5].events.len() != 1
        || back.labels["ana"][7].confidence != 2
        || back.labels["bruno"][0].label != ana[0].1.label
    {
        return Err("the labels read back differ");
    }
    let (r1, conflict) = resolve(&back.labels, 1);
    if r1.is_some() || !conflict {
        return Err("a disagreement should be a conflict");
    }
    let v = back.labels.entry(ADJUDICATED.into()).or_default();
    v.resize(back.points.len(), Label::default());
    v[1] = ana[1].1.clone();
    if resolve(&back.labels, 1).0 != Some(&back.labels[ADJUDICATED][1]) {
        return Err("the adjudicated label should settle it");
    }
    match read_labels(&pr.path, back.points.len()) {
        Ok((id, other)) if id == pr.id && other.get("bruno").is_some_and(|v| !v[11].label.is_empty()) => {}
        _ => return Err("readLabels (merge) failed"),
    }
    rep.push_str(&format!("  project: {} points, 2 interpreters, written and read back\n", back.points.len()));

    // the estimates of the labelled project, then (with Python) zeit.accuracy on its files
    back.labels.remove(ADJUDICATED);
    let mapped_a = mapped_areas(&ra, &kept_path, &back.strata, None).map_err(|_| "mappedAreas failed")?;
    for (k, v) in &mapped_a {
        if !near(*v, back.strata.mapped[0].get(k).copied().unwrap_or(0.0), 1e-9) {
            return Err("mappedAreas differs from the strata's pass");
        }
    }
    let periods = am.clone();
    let mut plain = am.clone();
    plain.rule.bins.clear();
    let classes = AssessedMap {
        rule: rb.clone(),
        mapping: [("Stable", "1"), ("Deforestation", "2"), ("Can't tell", "")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into(),
    };
    for (name, m) in [("events in periods", &periods), ("events", &plain), ("classes (another grid)", &classes)] {
        let xy: Vec<[f64; 2]> = back.points.iter().map(|q| [q.x, q.y]).collect();
        let v = values_at(&m.rule, &back.strata.grid.wkt, &xy).map_err(|_| "valuesAt failed")?;
        let mp = mapped_areas(&m.rule, &kept_path, &back.strata, None).map_err(|_| "mappedAreas failed")?;
        let pp = prepare(&back, m, &v, &mp, 0.95, &[]);
        let r = assess(&pp.input);
        rep.push_str(&format!(
            "  {name}: {} points, overall {:.4} +- {:.4} ({} conflict, {} left out, {} without date)\n",
            r.n,
            r.overall.est,
            r.overall.hi - r.overall.est,
            pp.conflicts,
            pp.excluded,
            pp.no_date
        ));
        if r.n < 50 || pp.conflicts != 1 || pp.excluded != 1 {
            return Err("the labelled project's points");
        }
        let Some(python) = python else { continue };
        let code = python_snippet(&back, m, 0.95)
            + "import json\n\
               print(\"JANUS_CHECK \" + json.dumps({\"n\": int(acc.n), \"overall\": float(acc.overall.estimate), \
               \"se\": float(acc.overall.se), \"area\": {str(k): [float(v), float(s)] for k, v, s in \
               zip(acc.area.index, acc.area.estimate, acc.area.se)}}))\n";
        let script = dir.join("check.py");
        if std::fs::write(&script, &code).is_err() {
            return Err("cannot write the Python check");
        }
        let out = std::process::Command::new(python).arg(&script).output().map_err(|_| "cannot run Python")?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        let Some(k) = text.find("JANUS_CHECK ").filter(|_| out.status.success()) else {
            rep.push_str(&format!("  Python said:\n{text}\n  code:\n{code}\n"));
            return Err("zeit.accuracy did not run");
        };
        let line = text[k + 12..].lines().next().unwrap_or("");
        let j: Value = serde_json::from_str(line).map_err(|_| "zeit.accuracy's numbers not read")?;
        let f = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
        if j["n"].as_i64() != Some(i64::from(r.n))
            || !near(f(&j["overall"]), r.overall.est, 1e-9)
            || !near(f(&j["se"]), r.overall.se, 1e-9)
        {
            rep.push_str(&format!("  zeit: {j}\n"));
            return Err("zeit.accuracy gives another overall accuracy");
        }
        for (ci, c) in r.classes.iter().enumerate() {
            let Some(z) = j["area"].get(c) else {
                if r.area[ci].est == 0.0 {
                    continue;
                }
                rep.push_str(&format!("  zeit: {j}\n"));
                return Err("zeit.accuracy has not every class");
            };
            if !near(f(&z[0]), r.area[ci].est, 1e-9) || !near(f(&z[1]), r.area[ci].se, 1e-9) {
                rep.push_str(&format!("  class {c}, zeit: {z}\n"));
                return Err("zeit.accuracy gives another area");
            }
        }
        rep.push_str("    the same as zeit.accuracy on the project's files\n");
    }
    let years = change_by_year(&back, &plain, 0.95);
    let sum: f64 = years.iter().map(|(_, a)| a.est).sum();
    if years.is_empty() || !(sum > 0.0) {
        return Err("the area of change by year");
    }
    if std::env::var_os("JANUS_KEEP_VALIDATION").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}

/// --measure-validation IN: the strata of IN (a buffer of 1 px), its areas, a sample of 1000 points, its values at
/// them and the estimators, timed.
pub fn measure(path: &str) -> i32 {
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    let Ok(ds) = Dataset::open(path) else {
        println!("cannot open {path}");
        return 1;
    };
    let floating = ds.rasterband(1).is_ok_and(|b| {
        matches!(b.band_type(), gdal::raster::GdalDataType::Float32 | gdal::raster::GdalDataType::Float64)
    });
    let (w, h) = ds.raster_size();
    drop(ds);
    let mut r = MapRule {
        path: path.to_string(),
        kind: if floating { MapKind::Events } else { MapKind::Classes },
        ..Default::default()
    };
    if !floating {
        r.change = [1].into();
    }
    let spec = StrataSpec { maps: vec![r.clone()], buffer: 1, limit: Vec::new() };
    let out = std::env::temp_dir().join("janus-measure-strata.tif").to_string_lossy().into_owned();
    let prog = Progress::with_threads(std::thread::available_parallelism().map_or(1, |n| n.get()));
    let t0 = Instant::now();
    let s = match compute_strata(&spec, &out, Some(&prog)) {
        Ok(s) => s,
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    println!(
        "{w} x {h} ({:.0} Mpx, {}, {} threads): strata in {:.0} ms ({} strata)",
        (w * h) as f64 / 1e6,
        r.kind.name(),
        prog.threads,
        ms(t0),
        s.strata.len()
    );
    let t0 = Instant::now();
    if mapped_areas(&r, &out, &s, Some(&prog)).is_err() {
        return 1;
    }
    println!("  mapped areas (the strata raster and the map read again): {:.0} ms", ms(t0));
    let t0 = Instant::now();
    let mut counted = s.clone();
    let _ = count_strata(&out, &mut counted, Some(&prog));
    println!("  strata counted again (a later draw): {:.0} ms", ms(t0));
    let n = allocate(&s.strata, 1000, Alloc::Proportional, 50);
    let t0 = Instant::now();
    let Ok(pts) = draw_sample(&s, &out, &n, 42, &[]) else { return 1 };
    println!("  {} points drawn in {:.0} ms", pts.len(), ms(t0));
    let xy: Vec<[f64; 2]> = pts.iter().map(|p| [p.x, p.y]).collect();
    let t0 = Instant::now();
    let _ = values_at(&r, &s.grid.wkt, &xy);
    println!("  the map at the points: {:.0} ms", ms(t0));
    // the estimators on 5000 points, 10 classes, every frame of a busy panel
    let mut inp = AssessInput { confidence: 0.95, ..Default::default() };
    for c in 0..10 {
        inp.classes.push(c.to_string());
        inp.stratum_area.push(1000.0 * (c + 1) as f64);
        inp.mapped.push(1000.0 * (c + 1) as f64);
    }
    for i in 0..5000 {
        inp.stratum.push(Some(i % 10));
        inp.map.push(Some(i % 10));
        inp.reference.push(Some(if i % 7 == 0 { (i + 1) % 10 } else { i % 10 }));
    }
    let t0 = Instant::now();
    let mut a = Assessment::default();
    for _ in 0..10 {
        a = assess(&inp);
    }
    println!("  assess (5000 points, 10 classes): {:.2} ms; overall {:.4}", ms(t0) / 10.0, a.overall.est);
    if std::env::var_os("JANUS_KEEP_VALIDATION").is_none() {
        let _ = std::fs::remove_file(&out);
    } else {
        println!("  strata raster kept: {out}");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++'s self-test project (JANUS_KEEP_VALIDATION=1 janus.exe --selftest-validation, its folder copied and
    /// named by JANUS_CPP_VALIDATION): read by the Rust, the same sample drawn again, the same strata computed,
    /// the same estimates; then written by the Rust and read back. `cargo test -p janus-validation --release --
    /// --ignored against_the_cpp --nocapture`
    #[test]
    #[ignore]
    fn against_the_cpp() {
        let Some(dir) = std::env::var_os("JANUS_CPP_VALIDATION") else { return };
        let dir = std::path::PathBuf::from(dir);
        let p = load_project(&dir.join("project.gpkg").to_string_lossy()).unwrap();
        println!(
            "C++ project: {} points, {} strata, {} interpreters, strata raster {}",
            p.points.len(),
            p.strata.strata.len(),
            p.labels.len(),
            p.strata_path
        );
        // the same sample from the same strata and seed
        let mut s = p.strata.clone();
        count_strata(&p.strata_path, &mut s, None).unwrap();
        let n: Vec<i32> = s.strata.iter().map(|t| t.pixels.min(25) as i32).collect();
        let first = draw_sample(&s, &p.strata_path, &n, p.seed, &[]).unwrap();
        let mut more = vec![0; s.strata.len()];
        more[0] = 10;
        let second = draw_sample(&s, &p.strata_path, &more, p.seed, &first).unwrap();
        let mine: Vec<Point> = first.into_iter().chain(second).collect();
        assert_eq!(mine.len(), p.points.len());
        for (a, b) in mine.iter().zip(&p.points) {
            assert_eq!((a.id, a.stratum, a.row, a.col, a.x, a.y), (b.id, b.stratum, b.row, b.col, b.x, b.y));
        }
        println!("  the same {} points drawn again", mine.len());
        // the same strata from the same maps
        let mut spec = p.spec.clone();
        spec.maps[0].path = dir.join("a.tif").to_string_lossy().into_owned();
        let out = std::env::temp_dir().join("janus-validation-against-cpp.tif").to_string_lossy().into_owned();
        let r = compute_strata(&spec, &out, None).unwrap();
        {
            let ds = Dataset::open(&out).unwrap();
            let b = ds.rasterband(1).unwrap();
            let ct = b.color_table().expect("a colour table");
            println!("  colour table of {} entries, stratum 1 {:?}", ct.entry_count(), ct.entry_as_rgb(1));
            assert!(ct.entry_as_rgb(1).is_some_and(|e| e.a == 255));
        }
        let (a, _, _) = read_raster(&out);
        let (b, _, _) = read_raster(&p.strata_path);
        assert_eq!(a, b, "the strata rasters");
        for (x, y) in r.strata.iter().zip(&p.strata.strata) {
            assert_eq!(
                (x.code, &x.name, x.cls, x.change, x.buffer, x.pixels),
                (y.code, &y.name, y.cls, y.change, y.buffer, y.pixels)
            );
            assert_eq!((x.area.to_bits(), x.weight.to_bits()), (y.area.to_bits(), y.weight.to_bits()));
        }
        assert_eq!(r.grid.wkt, p.strata.grid.wkt, "the grid's WKT");
        assert_eq!(r.mapped[0], p.strata.mapped[0]);
        println!("  the same strata raster, table and mapped areas");
        // the same estimates as the C++'s report (events in periods: 108 points, overall 0.7821 +- 0.1106)
        let mut m = p.maps[0].clone();
        m.rule.path = spec.maps[0].path.clone();
        let xy: Vec<[f64; 2]> = p.points.iter().map(|q| [q.x, q.y]).collect();
        let v = values_at(&m.rule, &p.strata.grid.wkt, &xy).unwrap();
        let pp = prepare(&p, &m, &v, &p.strata.mapped[0], 0.95, &[]);
        let e = assess(&pp.input);
        println!(
            "  events in periods: {} points, overall {:.4} +- {:.4}",
            e.n,
            e.overall.est,
            e.overall.hi - e.overall.est
        );
        assert_eq!(e.n, 108);
        assert!((e.overall.est - 0.7821).abs() < 5e-5 && (e.overall.hi - e.overall.est - 0.1106).abs() < 5e-5);
        // written by the Rust, read back the same
        let mut q = p.clone();
        q.path = dir.join("rust.gpkg").to_string_lossy().into_owned();
        create_project(&q).unwrap();
        let back = load_project(&q.path).unwrap();
        assert_eq!(back.points, p.points);
        assert_eq!(back.labels, p.labels);
        assert_eq!(back.strata.strata, p.strata.strata);
        assert_eq!((back.spec, back.maps, back.design, back.label_set), (p.spec, p.maps, p.design, p.label_set));
        println!("  written by the Rust as {} and read back the same", q.path);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn selftest_validation() {
        let mut rep = String::new();
        let r = super::self_test(&mut rep, None);
        print!("{rep}");
        r.unwrap();
    }
}
