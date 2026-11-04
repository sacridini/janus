//! `--selftest-ui` stages 290-298: official alerts against a map of change (alerts_ui.rs), offline. 290 writes a
//! map of change dates over the left half of the active layer (2020-03-01) and starts the OGC test server
//! (janus_geo::ogc_mock: squares of 20 pixels, one a day from 2020-01-01, every third a degradation); 291 compares
//! them as DETER is compared (the WFS fetched in a task, a vector layer); 292 checks every number against a count
//! made here, pixel by pixel (centres in squares); 293 the CSVs and the agreement map exported, the tab shown;
//! 294-295 GLAD-S2 rasters written here (a tile of a third of the pixel over the top-left quarter, confidence 4 on
//! 2020-02-04 in its left half) compared the same way, their dates a layer; 296-297 the same map as a file from
//! disk in YYYYMMDD (Int32: past float's whole numbers), its encoding detected, the same numbers, its agreement a
//! layer of its own; 298 everything removed.

use std::time::Instant;

use janus_geo::ogc_mock::{MockData, MockServer};
use janus_validation::Grid;
use janus_validation::alerts::{self, Product, days_from_civil, decimal_year};

use super::UiTest;
use crate::alerts_ui::{AlertFrom, WfsAlerts};
use crate::app::State;

/// The self-test's map of change.
const MAP_NAME: &str = "Self-test: change dates";

impl State {
    pub(super) fn test_alerts(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let dir = st.dir.join("alerts");
        match stage {
            290 => {
                // a layer with an EPSG CRS active (stage 280's)
                let info = self.session.as_ref()?.info.clone();
                let g = info.geo_transform?;
                let epsg: u32 = info.crs_authority.strip_prefix("EPSG:")?.parse().ok()?;
                let (w, h) = (info.width, info.height);
                if w < 60 || h < 30 || g[2] != 0.0 || g[4] != 0.0 {
                    return fail(&format!("a layer of 60 x 30 pixels at least, north up: {w} x {h}"));
                }
                let _ = std::fs::create_dir_all(&dir);
                let grid = Grid { w, h, gt: g, has_gt: true, wkt: info.crs_wkt.clone() };
                let d = decimal_year(days_from_civil(2020, 3, 1)) as f32;
                let map: Vec<f32> = (0..w * h).map(|i| if i % w < w / 2 { d } else { f32::NAN }).collect();
                let path = dir.join("change.tif").to_string_lossy().into_owned();
                if let Err(e) = alerts::write_f32(&path, &grid, &map) {
                    return fail(&format!("the map of change: {e}"));
                }
                let mut r = crate::zeit::ResultLayer::for_test(vec![], 0, 0, "year".into());
                (r.cube, r.grid, r.w, r.h, r.visible) = (info.id, Some(info.clone()), w, h, true);
                (r.name, r.path, r.run) = (MAP_NAME.into(), path, dir.to_string_lossy().into_owned());
                self.load_result_layer(r, true, ctx);
                // squares of 20 pixels in cells of 30: their edges on the pixels' edges
                let px = g[1];
                let data = MockData {
                    epsg,
                    x0: g[0],
                    y0: g[3],
                    cell: 30.0 * px,
                    side: 20.0 * px,
                    nx: (w / 30).clamp(1, 10),
                    ny: (h / 30).clamp(1, 6),
                    max_page: 400,
                };
                let server = match MockServer::start(data) {
                    Ok(s) => s,
                    Err(e) => return fail(&format!("the test server: {e}")),
                };
                println!(
                    "    a map of change over {} x {h} pixels (2020-03-01); {} x {} squares at {}",
                    w / 2,
                    server.data.nx,
                    server.data.ny,
                    server.url("")
                );
                st.web = Some(server);
                self.clear_notices();
                next(st, "a map of change dates and a server of dated polygons");
                None
            }
            291 => {
                let loaded = self.zeit.results.iter().any(|r| r.name == MAP_NAME && !r.data.is_empty());
                if !loaded || self.zeit.pending_loads > 0 {
                    return if waited > 30.0 { fail("the map of change should load") } else { None };
                }
                let maps = self.alert_maps();
                let Some((path, _)) = maps.iter().find(|m| m.1 == MAP_NAME) else {
                    return fail(&format!("the map should be offered as a map of change: {maps:?}"));
                };
                let server = st.web.as_ref()?;
                let a = &mut self.alerts;
                (a.from_kind, a.map, a.from, a.to) =
                    (AlertFrom::Server, path.clone(), "2020-01-01".into(), "2020-12-31".into());
                a.server = Some(WfsAlerts {
                    name: "Test alerts".into(),
                    service: janus_geo::ogc::Service::Wfs,
                    url: server.url("/ows"),
                    layer: "test:plots".into(),
                    date_field: "day".into(),
                    class_field: "classname".into(),
                });
                st.run_vectors = self.vectors.len();
                if let Err(e) = self.alerts_compare(ctx) {
                    return fail(&e);
                }
                if !self.web.fetching() {
                    return fail("the alerts should be fetched first, in a task");
                }
                next(st, "Compare: the server's alerts of the map's area fetched in a task (Tools > Tasks)");
                None
            }
            292 => {
                if self.alerts.busy() || self.alerts.done.is_none() {
                    if !self.alerts.error.is_empty() {
                        return fail(&self.alerts.error.clone());
                    }
                    return if waited > 90.0 { fail("the comparison should end") } else { None };
                }
                let d = self.alerts.done.clone()?;
                let c = &d.cmp;
                // the alerts a vector layer, the agreement a map of three classes
                if self.vectors.len() <= st.run_vectors || !self.vector_loads.is_empty() {
                    return if waited > 30.0 { fail("the fetched alerts should open as a vector layer") } else { None };
                }
                let agreement = self.zeit.results.iter().filter(|r| r.name.starts_with("Agreement:")).count();
                let ok = self.zeit.results.iter().any(|r| {
                    r.name.starts_with("Agreement:") && r.classes.len() == 3 && r.path == d.agreement && r.visible
                });
                if agreement != 1 || !ok || self.zeit.pending_loads > 0 {
                    return if waited > 30.0 { fail("the agreement map should be a layer of 3 classes") } else { None };
                }
                // the count made here: each pixel's centre in a square, the map on the left half
                let info = self.zeit.results.iter().find(|r| r.name == MAP_NAME)?.grid.clone()?;
                let g = info.geo_transform?;
                let (w, h) = (info.width, info.height);
                let feats = st.web.as_ref()?.data.features();
                let day0 = days_from_civil(2020, 1, 1);
                let map_day = days_from_civil(2020, 3, 1);
                let (mut both, mut only_map, mut only_alerts) = (0, 0, 0);
                let mut lags = Vec::new();
                let mut hit = vec![(0usize, 0usize); feats.len()];
                for y in 0..h {
                    for x in 0..w {
                        let (cx, cy) = (g[0] + (x as f64 + 0.5) * g[1], g[3] + (y as f64 + 0.5) * g[5]);
                        let f = feats
                            .iter()
                            .position(|f| cx > f.square[0] && cx < f.square[2] && cy > f.square[1] && cy < f.square[3]);
                        let m = x < w / 2;
                        match (m, f) {
                            (true, Some(k)) => {
                                both += 1;
                                lags.push(f64::from(map_day - (day0 + feats[k].id as i32 - 1)));
                            }
                            (true, None) => only_map += 1,
                            (false, Some(_)) => only_alerts += 1,
                            _ => {}
                        }
                        if let Some(k) = f {
                            hit[k].0 += 1;
                            hit[k].1 += usize::from(m);
                        }
                    }
                }
                lags.sort_by(f64::total_cmp);
                let detected = hit.iter().filter(|h| h.1 > 0).count();
                let degradation = feats.iter().zip(&hit).filter(|(f, h)| f.class == "DEGRADACAO" && h.1 > 0).count();
                let median = alerts::quantile(&lags, 0.5);
                println!(
                    "    both {} (here {both}), only the map {} ({only_map}), only the alerts {} ({only_alerts}); \
                     {} of {} alerts detected ({detected}); median lag {} days ({median}); {:.3} s",
                    c.both, c.only_map, c.only_alerts, c.alerts_detected, c.alerts, c.lag.median, c.seconds
                );
                if (c.both, c.only_map, c.only_alerts) != (both, only_map, only_alerts)
                    || c.alerts != feats.len()
                    || c.alerts_detected != detected
                    || c.lag.n != lags.len()
                    || c.lag.median != median
                    || c.lag.min != lags[0]
                    || c.classes.iter().find(|k| k.name == "DEGRADACAO").map(|k| k.detected) != Some(degradation)
                {
                    return fail("the comparison's numbers should be the count made here");
                }
                let ha = c.both as f64 * g[1] * g[5].abs() / 1e4;
                if (c.both_ha - ha).abs() > 1e-6 * ha.max(1.0) || c.map_patches != 1 || c.map_patches_confirmed != 1 {
                    return fail(&format!("areas and patches: {} ha, {} patches", c.both_ha, c.map_patches));
                }
                next(st, "the numbers are the count made pixel by pixel; the alerts and the agreement are layers");
                None
            }
            293 => {
                let d = self.alerts.done.clone()?;
                let out = dir.join("export");
                self.alerts_export(&d, &out);
                let files: Vec<_> = ["summary", "classes", "alerts", "lags"]
                    .iter()
                    .map(|s| out.join(format!("test_alerts_{s}.csv")))
                    .collect();
                let summary = std::fs::read_to_string(&files[0]).unwrap_or_default();
                let rows = std::fs::read_to_string(&files[2]).map_or(0, |t| t.lines().count());
                if !files.iter().all(|f| f.exists())
                    || !out.join("test_alerts_agreement.tif").exists()
                    || !summary.contains(&format!("both_pixels,{}", d.cmp.both))
                    || rows != d.cmp.alerts + 1
                {
                    return fail(&format!("the CSVs: {} ({rows} rows)", self.alerts.error));
                }
                self.open_alerts();
                next(st, "the summary, classes, alerts and lags as CSVs, the agreement map; Validation > Alerts");
                None
            }
            294 => {
                if self.val.tab != 5 {
                    return if waited > 5.0 { fail("Validation > Alerts should show") } else { None };
                }
                // GLAD-S2 over the top-left quarter, a third of the pixel; alerts in its left half
                let info = self.zeit.results.iter().find(|r| r.name == MAP_NAME)?.grid.clone()?;
                let g = info.geo_transform?;
                let (cw, ch) = (info.width / 2, info.height / 2);
                let (fw, fh) = (3 * cw, 3 * ch);
                let tile = Grid {
                    w: fw,
                    h: fh,
                    gt: [g[0], g[1] / 3.0, 0.0, g[3], 0.0, g[5] / 3.0],
                    has_gt: true,
                    wkt: info.crs_wkt.clone(),
                };
                let inside = |i: usize| i % fw < 3 * (cw / 2);
                let days = (days_from_civil(2020, 2, 4) - days_from_civil(2018, 12, 31)) as f32;
                let conf: Vec<f32> = (0..fw * fh).map(|i| if inside(i) { 4.0 } else { 0.0 }).collect();
                let date: Vec<f32> = (0..fw * fh).map(|i| if inside(i) { days } else { 0.0 }).collect();
                let (pc, pd) = (dir.join("alert.tif"), dir.join("alertDate.tif"));
                for (p, v) in [(&pc, &conf), (&pd, &date)] {
                    if let Err(e) = alerts::write_f32(&p.to_string_lossy(), &tile, v) {
                        return fail(&e);
                    }
                }
                let a = &mut self.alerts;
                (a.from_kind, a.product) = (AlertFrom::Rasters, Product::GladS2);
                a.files = crate::alerts_ui::pair_files(
                    Product::GladS2,
                    &[pc.to_string_lossy().into_owned(), pd.to_string_lossy().into_owned()],
                );
                if a.files.len() != 1 || a.files[0].date.is_empty() {
                    return fail(&format!("GLAD's files paired: {:?}", a.files));
                }
                if let Err(e) = self.alerts_compare(ctx) {
                    return fail(&e);
                }
                next(st, "GLAD-S2 rasters (alert and alertDate) compared with the map");
                None
            }
            295 => {
                let done = self.alerts.done.clone();
                if self.alerts.busy() || done.as_ref().is_none_or(|d| d.alert_dates.is_none()) {
                    if !self.alerts.error.is_empty() {
                        return fail(&self.alerts.error.clone());
                    }
                    return if waited > 60.0 { fail("the comparison should end") } else { None };
                }
                if self.zeit.pending_loads > 0 {
                    return None;
                }
                let c = &done.as_ref()?.cmp;
                let info = self.zeit.results.iter().find(|r| r.name == MAP_NAME)?.grid.clone()?;
                let (cw, ch) = (info.width / 2, info.height / 2);
                let alerted = (cw / 2) * ch;
                println!(
                    "    covered {}, both {}, only the map {}, only the alerts {}; lag {} days; {} alert(s)",
                    c.covered_pixels, c.both, c.only_map, c.only_alerts, c.lag.median, c.alerts
                );
                if (c.covered_pixels, c.both, c.only_map, c.only_alerts) != (cw * ch, alerted, cw * ch - alerted, 0)
                    || c.lag.median != 26.0
                    || (c.alerts, c.alerts_detected) != (1, 1)
                    || c.units[0].unit.class != 3
                {
                    return fail("GLAD-S2's numbers");
                }
                let dates = self.zeit.results.iter().filter(|r| r.name.ends_with(": date") && r.unit == "year").count();
                let agreement = self.zeit.results.iter().filter(|r| r.name.starts_with("Agreement:")).count();
                if dates != 1 || agreement != 1 {
                    return fail(&format!("the alerts' dates and one agreement map: {dates}, {agreement}"));
                }
                next(
                    st,
                    "GLAD-S2: its tile's cover, confidence and dates decoded; its dates a layer, the agreement replaced",
                );
                None
            }
            296 => {
                // the run's map as a file from disk, its dates in YYYYMMDD
                let r = self.zeit.results.iter().find(|r| r.name == MAP_NAME)?;
                let info = r.grid.clone()?;
                let (w, h) = (info.width, info.height);
                let path = dir.join("change_ymd.tif").to_string_lossy().into_owned();
                let write = || -> Result<(), String> {
                    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
                    let mut ds =
                        drv.create_with_band_type::<i32, _>(&path, w, h, 1).map_err(|e| format!("{path}: {e}"))?;
                    ds.set_geo_transform(&info.geo_transform.unwrap_or_default()).map_err(|e| e.to_string())?;
                    ds.set_projection(&info.crs_wkt).map_err(|e| e.to_string())?;
                    let mut b = ds.rasterband(1).map_err(|e| e.to_string())?;
                    b.set_no_data_value(Some(0.0)).map_err(|e| e.to_string())?;
                    let v: Vec<i32> = (0..w * h).map(|i| if i % w < w / 2 { 20200301 } else { 0 }).collect();
                    let mut buf = gdal::raster::Buffer::new((w, h), v);
                    b.write((0, 0), (w, h), &mut buf).map_err(|e| e.to_string())
                };
                if let Err(e) = write() {
                    return fail(&format!("the map in YYYYMMDD: {e}"));
                }
                st.run_vectors = self.layers.len();
                let arg = format!(
                    "glad-s2,mapfile={path},tab=0,files={};{}",
                    self.alerts.files[0].conf, self.alerts.files[0].date
                );
                if let Err(e) = self.alerts_option(&arg, ctx) {
                    return fail(&e);
                }
                if self.alerts.map != path || self.alerts.map_code != alerts::DateCode::Auto {
                    return fail("the file should be the map compared, its dates detected");
                }
                next(st, "a map of change from disk in YYYYMMDD (File..., Its dates as: detected) against GLAD-S2");
                None
            }
            297 => {
                let done = self.alerts.done.clone();
                let path = dir.join("change_ymd.tif").to_string_lossy().into_owned();
                if self.alerts.busy() || done.as_ref().is_none_or(|d| d.map_path != path) {
                    if !self.alerts.error.is_empty() {
                        return fail(&self.alerts.error.clone());
                    }
                    return if waited > 60.0 { fail("the comparison should end") } else { None };
                }
                let d = done.as_ref()?;
                let agreement = (0..self.layers.len())
                    .find(|&k| self.layer_session(k).is_some_and(|s| s.info.layers[0].path == d.agreement));
                if self.opening.is_some() || agreement.is_none() {
                    return if waited > 60.0 { fail("the agreement should open as a layer") } else { None };
                }
                let c = &d.cmp;
                let info = self.zeit.results.iter().find(|r| r.name == MAP_NAME)?.grid.clone()?;
                let (cw, ch) = (info.width / 2, info.height / 2);
                let alerted = (cw / 2) * ch;
                let log = std::fs::read_to_string(d.folder.join("job.log")).unwrap_or_default();
                if (c.covered_pixels, c.both, c.only_map, c.only_alerts) != (cw * ch, alerted, cw * ch - alerted, 0)
                    || c.lag.median != 26.0
                    || !log.contains("YYYYMMDD")
                {
                    return fail(&format!(
                        "the file's numbers: covered {}, both {}, only the map {}, lag {}",
                        c.covered_pixels, c.both, c.only_map, c.lag.median
                    ));
                }
                if let Some(k) = agreement {
                    self.remove_layer(k, ctx);
                }
                next(
                    st,
                    "the file's dates read as YYYYMMDD: GLAD-S2's numbers as with the run's map; the agreement a layer",
                );
                None
            }
            298 => {
                let ours = dir.to_string_lossy().into_owned();
                let runs: Vec<String> =
                    self.alerts.done.iter().map(|d| d.folder.to_string_lossy().into_owned()).collect();
                self.zeit.results.retain(|r| r.run != ours && !runs.contains(&r.run));
                let fetched: Vec<String> = self.web.sources.iter().map(|s| s.path.clone()).collect();
                for path in &fetched {
                    while let Some(j) = self.vectors.iter().position(|v| &v.data.path == path) {
                        self.remove_vector(j);
                    }
                    let _ = std::fs::remove_file(path);
                }
                for r in runs {
                    let _ = std::fs::remove_dir_all(r);
                }
                self.web = Default::default();
                self.alerts = Default::default();
                self.clear_notices();
                st.web = None;
                next(st, "the comparisons' layers removed");
                st.stage = 299;
                None
            }
            _ => None,
        }
    }
}
