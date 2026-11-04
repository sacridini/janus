//! `--selftest-ui` stages 280-289: web layers (web_layers.rs) from a small OGC server started on 127.0.0.1
//! (janus_geo::ogc_mock: no network), its squares on a grid over the active layer. 280 starts it and connects
//! (service found: WFS); 281 fetches the layer of squares over the active layer; 282 checks the vector layer it
//! opens (every square, its name, a square as the ROI where it lies); 283-285 OGC API - Features with a date
//! filter and a cap (5 of 11, the layer named and a notice); 285-286 a server's error said plainly; 286-288 a WMS
//! map with a fixed time added on top (its tiles GetMaps of their boxes with TIME, drawn, its credit; a click asks
//! it GetFeatureInfo: the square there, at its time); 289 the
//! project's entries, saved and opened again (the WFS layer read from its GeoPackage, the OGC API's, deleted,
//! fetched again, the map's tiles at its time), then everything removed.

use std::time::Instant;

use janus_geo::ogc::Service;
use janus_geo::ogc_mock::{MockData, MockServer};

use super::UiTest;
use crate::app::State;

impl State {
    pub(super) fn test_web(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
        let caps_read = |s: &State| s.web.ui.loading_done();
        match stage {
            280 => {
                // a layer with an EPSG CRS active
                let epsg = |s: &State, i: usize| {
                    let info = &s.layer_session(i)?.info;
                    info.geo_transform?;
                    info.crs_authority.strip_prefix("EPSG:")?.parse::<u32>().ok()
                };
                if epsg(self, self.active).is_none() {
                    let Some(i) = (0..self.layers.len()).find(|&i| epsg(self, i).is_some()) else {
                        return fail("a layer with an EPSG CRS should be open");
                    };
                    self.set_active(i, ctx);
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                let g = info.geo_transform?;
                let (w, h) = (info.width as f64 * g[1], info.height as f64 * g[5].abs());
                let cell = w / 10.0;
                let data = MockData {
                    epsg: epsg(self, self.active)?,
                    x0: g[0],
                    y0: g[3],
                    cell,
                    side: cell / 2.0,
                    nx: 10,
                    ny: ((h / cell).floor() as usize).clamp(1, 6),
                    max_page: 400,
                };
                let server = match MockServer::start(data) {
                    Ok(s) => s,
                    Err(e) => return fail(&format!("the test server: {e}")),
                };
                self.test_fit();
                self.clear_notices();
                self.open_web_dialog();
                (self.web.ui.url, self.web.ui.service) = (server.url("/ows"), None);
                self.web_connect(ctx);
                println!(
                    "    a test server at {}: {} x {} squares of {:.0} m in EPSG:{}",
                    server.url(""),
                    server.data.nx,
                    server.data.ny,
                    server.data.side,
                    server.data.epsg
                );
                st.web = Some(server);
                next(st, "File > Add web layer...: a server's URL, its service to be found");
                None
            }
            281 => {
                if !caps_read(self) {
                    return if waited > 30.0 { fail("the server's capabilities should be read") } else { None };
                }
                let Some(caps) = self.web.ui.caps.clone() else {
                    return fail(&format!("capabilities: {}", self.web.ui.error));
                };
                if caps.service != Service::Wfs {
                    return fail(&format!("the service should be found: WFS, not {}", caps.service.name()));
                }
                let Some(k) = caps.layers.iter().position(|l| l.name == "test:plots") else {
                    return fail("the layer test:plots should be listed");
                };
                self.web.ui.picked = Some(k);
                self.web.ui.area = 2; // the active layer
                let (req, name) = match self.web_request() {
                    Ok(r) => r,
                    Err(e) => return fail(&e),
                };
                st.server().clear_log();
                st.run_vectors = self.vectors.len();
                self.web_fetch(req, name, None, ctx);
                if self.zeit.jobs.last().is_none_or(|j| !j.title.starts_with("Fetch")) {
                    return fail("the fetch should be a task of Tools > Tasks");
                }
                next(
                    st,
                    &format!(
                        "WFS found ({} layers); the squares over the active layer fetched in a task",
                        caps.layers.len()
                    ),
                );
                None
            }
            282 => {
                let Some(li) = self.test_web_layer(st) else {
                    return if waited > 60.0 {
                        fail(&format!("the features should open: {}", self.web.ui.error))
                    } else {
                        None
                    };
                };
                let d = &st.server().data;
                let n = d.nx * d.ny;
                let v = &self.vectors[li];
                if v.data.features.len() != n || v.data.areas != n {
                    return fail(&format!("{} squares should open, not {}", n, v.data.features.len()));
                }
                if v.name != "Test plots" {
                    return fail(&format!("the layer should be named after the server's layer, not {}", v.name));
                }
                let log = st.server().requests();
                if !log.iter().any(|r| r.contains("GetFeature") && r.contains("FILTER")) {
                    return fail(&format!("GetFeature should ask for the box: {log:?}"));
                }
                if !v.data.fields.iter().any(|f| f == "name") || !v.data.fields.iter().any(|f| f == "day") {
                    return fail(&format!("the fields should come: {:?}", v.data.fields));
                }
                let fields = v.data.fields.clone();
                // a square as the ROI: where its corners are on the layer
                let (cell, side, g) = (d.cell, d.side, self.session.as_ref()?.info.geo_transform?);
                if !self.feature_as_roi(li, 0) {
                    return fail("the first square should become the ROI");
                }
                let r = self.series.roi_rect?;
                let m = (cell - side) / 2.0;
                let want = [m / g[1], m / g[5].abs(), (m + side) / g[1], (m + side) / g[5].abs()];
                let off = r.iter().zip(want).map(|(&a, b)| (a as f64 - b).abs()).fold(0.0, f64::max);
                if off > 1.5 {
                    return fail(&format!("the ROI {r:?} should be the square's pixels {want:?}"));
                }
                println!("    {n} squares, fields {fields:?}; the first as the ROI: {r:?}");
                self.clear_roi();
                (self.web.ui.url, self.web.ui.service) = (st.server().url("/oapi"), None);
                self.web_connect(ctx);
                next(st, "a WFS layer as a vector layer: its features, fields and places (a square as the ROI)");
                None
            }
            283 => {
                if !caps_read(self) {
                    return if waited > 30.0 { fail("the OGC API's collections should be read") } else { None };
                }
                let Some(caps) = self.web.ui.caps.clone() else {
                    return fail(&format!("collections: {}", self.web.ui.error));
                };
                if caps.service != Service::Oapif {
                    return fail(&format!("an OGC API should be found, not {}", caps.service.name()));
                }
                let Some(k) = caps.layers.iter().position(|l| l.name == "parcels") else {
                    return fail("the collection parcels should be listed");
                };
                // its fields, as the dialog reads them
                match janus_geo::ogc::layer_fields(caps.service, &caps.url, "parcels") {
                    Ok(f) if f.iter().any(|f| f.0 == "day") => {}
                    r => return fail(&format!("the collection's fields should have its date: {r:?}")),
                }
                let u = &mut self.web.ui;
                (u.picked, u.area, u.max_features) = (Some(k), 3, 5);
                (u.date_field, u.from, u.to) = ("day".into(), "2020-01-10".into(), "2020-01-20".into());
                let (req, name) = match self.web_request() {
                    Ok(r) => r,
                    Err(e) => return fail(&e),
                };
                st.server().clear_log();
                st.run_vectors = self.vectors.len();
                self.web_fetch(req, name, None, ctx);
                next(st, "OGC API - Features found: a collection's days 2020-01-10 to 2020-01-20, at most 5");
                None
            }
            284 => {
                let Some(li) = self.test_web_layer(st) else {
                    return if waited > 60.0 {
                        fail(&format!("the features should open: {}", self.web.ui.error))
                    } else {
                        None
                    };
                };
                let v = &self.vectors[li];
                let src = self.web.source_of(&v.data.path)?;
                if v.data.features.len() != 5 || !src.capped || !v.name.contains("first 5") {
                    return fail(&format!("5 features, capped and said so: {} ({})", v.data.features.len(), v.name));
                }
                if !self.notice_titles().iter().any(|(t, failed)| t.contains("first 5") && !failed) {
                    return fail(&format!("a notice should say the cap was reached: {:?}", self.notice_titles()));
                }
                let log = st.server().requests();
                if !log.iter().any(|r| r.contains("/items") && r.contains("datetime=") && r.contains("2020-01-10")) {
                    return fail(&format!("the dates should go to the server: {log:?}"));
                }
                println!("    {}: {} features (capped); {} requests", v.name, v.data.features.len(), log.len());
                (self.web.ui.url, self.web.ui.service) = (st.server().url("/error"), Some(Service::Wms));
                self.web_connect(ctx);
                next(st, "the dates filtered by the server, the cap reached and said (the layer's name, a notice)");
                None
            }
            285 => {
                if !caps_read(self) {
                    return if waited > 30.0 { fail("a server's error should come back") } else { None };
                }
                let e = self.web.ui.error.clone();
                if self.web.ui.caps.is_some() || e.is_empty() || e.len() > 400 {
                    return fail(&format!("a plain error, no layers: {e:?}"));
                }
                println!("    {e}");
                (self.web.ui.url, self.web.ui.service) = (st.server().url("/ows"), Some(Service::Wms));
                self.web_connect(ctx);
                next(st, "a server's error said plainly");
                None
            }
            286 => {
                if !caps_read(self) {
                    return if waited > 30.0 { fail("the WMS capabilities should be read") } else { None };
                }
                let Some(caps) = self.web.ui.caps.clone() else {
                    return fail(&format!("WMS: {}", self.web.ui.error));
                };
                let Some(k) = caps.layers.iter().position(|l| l.name == "test:alerts") else {
                    return fail("the map test:alerts should be listed");
                };
                if caps.layers[k].times.len() != 3 {
                    return fail("the map's times should be read");
                }
                let u = &mut self.web.ui;
                (u.picked, u.opacity) = (Some(k), 1.0);
                (u.time_mode, u.fixed_time) = (crate::web_layers::TimeMode::Fixed, "2020-01-01".into());
                if let Err(e) = self.web_add_picked_map() {
                    return fail(&e);
                }
                st.server().clear_log();
                self.test_fit();
                next(st, "a WMS map added with a time, on top of the layers");
                None
            }
            287 => {
                self.test_render();
                let w = self.web.maps.last()?;
                if !w.why.is_empty() {
                    return fail(&format!("the map should be drawn: {}", w.why));
                }
                let done = w.b.as_ref().is_some_and(|b| b.has_map() && b.loads() > 0 && b.inflight() == 0);
                if !done {
                    return if waited > 60.0 { fail("the map's tiles should be read") } else { None };
                }
                if let Some(e) = w.b.as_ref().map(|b| b.error()).filter(|e| !e.is_empty()) {
                    return fail(&format!("tiles: {e}"));
                }
                next(st, "the map's tiles read");
                None
            }
            288 if self.web.info.is_some() => {
                // GetFeatureInfo's answer: the square under the click (or nothing between the squares)
                let inf = self.web.info.as_ref()?;
                if !inf.done() {
                    return if waited > 30.0 { fail("the WMS map should answer GetFeatureInfo") } else { None };
                }
                let [x, y] = *st.pts.first()?;
                let (gx, gy) = self.session.as_ref()?.info.pixel_to_geo(x, y)?;
                let want = st.server().data.features().into_iter().find(|f| {
                    let q = f.square;
                    gx >= q[0] && gx <= q[2] && gy >= q[1] && gy <= q[3]
                });
                let (name, answer) = (&inf.answers[0].0, &inf.answers[0].1);
                let got = match answer {
                    Some(Ok(v)) => v.clone(),
                    a => return fail(&format!("{name} should answer: {a:?}")),
                };
                let field = |k: &str| got.first().and_then(|f| f.fields.iter().find(|x| x.0 == k)).map(|x| x.1.clone());
                let ok = match &want {
                    Some(f) => got.len() == 1 && field("name") == Some(f.name.clone()),
                    None => false,
                };
                if !ok || (want.is_some() && field("time").as_deref() != Some("2020-01-01")) {
                    return fail(&format!(
                        "the square under the click ({:?}) at the map's time: {got:?}",
                        want.map(|f| f.name)
                    ));
                }
                let log = st.server().requests();
                let asked = log.iter().filter(|r| r.contains("GetFeatureInfo")).count();
                if asked != 1
                    || !log.iter().any(|r| {
                        r.contains("INFO_FORMAT=application%2Fjson") || r.contains("INFO_FORMAT=application/json")
                    })
                {
                    return fail(&format!("one GetFeatureInfo in GeoJSON: {log:?}"));
                }
                println!("    a click on a square: {name} says {}", want.map_or("nothing".to_string(), |f| f.name));
                self.web.info = None;
                st.pts.clear();
                next(
                    st,
                    "the WMS map drawn at its time, its credit on the map and the exports; a click asks it what is there",
                );
                None
            }
            288 => {
                let px = self.test_render()?;
                let (cx, cy) = (super::SIZE[0] as usize / 2, super::SIZE[1] as usize / 2);
                let p = &px[(cy * super::SIZE[0] as usize + cx) * 4..][..4];
                // the server's colour of a box at TIME 2020-01-*: blue 220, red and green 30 to 229
                if p[2] != 220 || p[0] < 30 || p[1] < 30 {
                    return fail(&format!("the map should be drawn on top in the server's colour: {p:?}"));
                }
                let log = st.server().requests();
                let maps: Vec<&String> = log.iter().filter(|r| r.to_ascii_lowercase().contains("getmap")).collect();
                let asked = |r: &String| {
                    let up = r.to_ascii_uppercase();
                    up.contains("TIME=2020-01-01")
                        && (up.contains("LAYERS=TEST:ALERTS") || up.contains("LAYERS=TEST%3AALERTS"))
                };
                if maps.is_empty() || !maps.iter().all(|r| asked(r)) {
                    return fail(&format!("GetMap with the layer and its time: {maps:?}"));
                }
                if !self.basemap_attribution().contains("Janus mock data") {
                    return fail(&format!("the map's credit should show: {}", self.basemap_attribution()));
                }
                println!("    {} GetMaps; the centre {p:?}; credit {}", maps.len(), self.basemap_attribution());
                // a click in the middle of the first square: GetFeatureInfo asked of the map
                let q = st.server().data.features().first()?.square;
                let (x, y) = self.session.as_ref()?.info.geo_to_pixel((q[0] + q[2]) / 2.0, (q[1] + q[3]) / 2.0)?;
                st.server().clear_log();
                if !self.web_info_at(x, y, ctx) {
                    return fail("a click should ask the queryable WMS map what is there");
                }
                st.pts = vec![[x, y]];
                println!("[{:5.1} s] the WMS map drawn at its time, its credit shown", t0.elapsed().as_secs_f64());
                st.since = Some(Instant::now());
                None
            }
            289 if st.reveal.is_some() => {
                // the project opening again: its series, the WFS layer from its GeoPackage, the OGC API's fetched
                // again, the map
                if self.project.restoring()
                    || self.opening.is_some()
                    || self.zeit.loads_pending()
                    || self.web.fetching()
                    || !self.vector_loads.is_empty()
                {
                    return if waited > 120.0 { fail("the project with web layers should open") } else { None };
                }
                let gone = st.texts.first()?.clone();
                let series = st.texts.get(1)?.clone();
                if self.layers.len().to_string() != series {
                    return fail(&format!("its {series} series should open again, not {}", self.layers.len()));
                }
                let web: Vec<usize> = (0..self.vectors.len())
                    .filter(|&j| self.web.source_of(&self.vectors[j].data.path).is_some())
                    .collect();
                let d = &st.server().data;
                let n = d.nx * d.ny;
                let wfs = web.iter().find(|&&j| self.vectors[j].data.features.len() == n);
                let oapi = web.iter().find(|&&j| self.vectors[j].data.path == gone);
                let (Some(&wfs), Some(&oapi)) = (wfs, oapi) else {
                    let got: Vec<(String, usize)> = web
                        .iter()
                        .map(|&j| (self.vectors[j].name.clone(), self.vectors[j].data.features.len()))
                        .collect();
                    return fail(&format!("both fetched layers should open again: {got:?}"));
                };
                let (wv, ov) = (&self.vectors[wfs], &self.vectors[oapi]);
                if wv.name != "Test plots" || ov.data.features.len() != 5 || !ov.name.contains("first 5") {
                    return fail(&format!(
                        "the layers as saved: {} / {} ({})",
                        wv.name,
                        ov.name,
                        ov.data.features.len()
                    ));
                }
                let (wname, oname) = (wv.name.clone(), ov.name.clone());
                let log = st.server().requests();
                if !log.iter().any(|r| r.contains("/items") && r.contains("datetime=")) {
                    return fail(&format!("the GeoPackage gone should be fetched again: {log:?}"));
                }
                if log.iter().any(|r| r.contains("GetFeature")) {
                    return fail(&format!("the WFS layer should be read from its GeoPackage: {log:?}"));
                }
                let Some(m) = self.web.maps.first() else { return fail("the map should be added again") };
                if self.web.maps.len() != 1
                    || m.spec.layer != "test:alerts"
                    || m.fixed_time != "2020-01-01"
                    || m.time_mode != crate::web_layers::TimeMode::Fixed
                {
                    return fail(&format!("the map as saved: {} {:?} {}", m.spec.layer, m.time_mode, m.fixed_time));
                }
                // its tiles read again, at its time
                self.test_render();
                let m = self.web.maps.first()?;
                if !m.b.as_ref().is_some_and(|b| b.has_map() && b.loads() > 0 && b.inflight() == 0) {
                    return if waited > 120.0 {
                        fail(&format!("the map's tiles should be read: {}", m.why))
                    } else {
                        None
                    };
                }
                let maps: Vec<String> = st.server().requests().into_iter().filter(|r| r.contains("GetMap")).collect();
                if maps.is_empty() || !maps.iter().all(|r| r.to_ascii_uppercase().contains("TIME=2020-01-01")) {
                    return fail(&format!("GetMap at the map's time: {maps:?}"));
                }
                println!(
                    "    opened again: {} series, {} ({} squares, read), {} (fetched again), {} GetMaps",
                    self.layers.len(),
                    wname,
                    n,
                    oname,
                    maps.len()
                );
                st.reveal = None;
                st.texts.clear();
                self.web_cleanup(st);
                next(st, "the project opened again: the map, the WFS layer read, the GeoPackage gone fetched again");
                st.stage = 290;
                None
            }
            289 => {
                let p = self.project_state(None);
                let fetched: Vec<String> = self.web.sources.iter().map(|s| s.path.clone()).collect();
                let kept = p.vectors.iter().filter(|v| v.web.is_some()).count();
                if p.web_maps.len() != 1 || kept != 2 || !p.order.iter().any(|k| k == "w0") {
                    return fail(&format!(
                        "the project should keep them: {} maps, {kept} fetched, {:?}",
                        p.web_maps.len(),
                        p.order
                    ));
                }
                if p.web_maps[0].layer != "test:alerts" || p.web_maps[0].fixed_time != "2020-01-01" {
                    return fail(&format!("the map's entry: {:?}", p.web_maps[0]));
                }
                // saved and opened again: the OGC API's GeoPackage gone (fetched again), the WFS one kept
                let file = st.dir.join("web-project").join("web.janus");
                let _ = std::fs::create_dir_all(file.parent()?);
                self.save_project_to(&file);
                let Some(gone) =
                    self.web.sources.iter().find(|s| s.req.service == Service::Oapif).map(|s| s.path.clone())
                else {
                    return fail("the OGC API's layer should be a web source");
                };
                if !fetched.contains(&gone) || std::fs::remove_file(&gone).is_err() {
                    return fail(&format!("its GeoPackage should be there: {gone}"));
                }
                st.server().clear_log();
                self.clear_notices();
                let series = self.layers.len();
                self.open_project(&file, ctx);
                if !self.project.restoring() {
                    return fail("the project should open again");
                }
                st.texts = vec![gone, series.to_string()];
                st.reveal = Some(file);
                next(st, "the project keeps the map and the fetched layers (their requests); saved, opened again");
                st.stage = 289;
                None
            }
            _ => None,
        }
    }

    /// Everything of the web stages removed (the maps, the fetched layers and their files), the server stopped.
    fn web_cleanup(&mut self, st: &mut UiTest) {
        let fetched: Vec<String> = self.web.sources.iter().map(|s| s.path.clone()).collect();
        while !self.web.maps.is_empty() {
            self.remove_web_map(0);
        }
        for path in &fetched {
            while let Some(j) = self.vectors.iter().position(|v| &v.data.path == path) {
                self.remove_vector(j);
            }
            let _ = std::fs::remove_file(path);
        }
        self.web = Default::default();
        self.clear_notices();
        st.web = None;
    }

    /// The vector layer the last fetch opened, placed on the active layer (None while it comes).
    fn test_web_layer(&self, st: &UiTest) -> Option<usize> {
        if self.web.fetching() || !self.vector_loads.is_empty() || self.vectors.len() <= st.run_vectors {
            return None;
        }
        let li = self.vectors.len() - 1;
        let v = &self.vectors[li];
        (self.web.source_of(&v.data.path).is_some_and(|s| s.named)
            && v.placed_for == self.vector_place_key()
            && !v.px.is_empty())
        .then_some(li)
    }
}

impl UiTest {
    fn server(&self) -> &MockServer {
        self.web.as_ref().expect("the test server")
    }
}
