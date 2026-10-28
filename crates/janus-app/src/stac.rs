//! File > Search STAC catalog (janus_cube::stac): a catalog's collection searched over the visible area, the ROI or
//! a box of longitudes and latitudes, between two dates, below a cloud cover; its dates and bands picked; the series
//! opened in place (a VRT per date, read as the map needs it) or downloaded as GeoTIFFs. Both are jobs of
//! Tools > Tasks, like the embeddings' download, the layer added when it is ready.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_cube::stac::{self, BandChoice, CATALOGS, DateGroup, Item, Orbit, Search};
use janus_cube::stac_vrt::{self, BuildSpec};

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::theme;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

/// Cell sizes offered (0: the finest of the bands).
const RESOLUTIONS: [f64; 7] = [0.0, 10.0, 20.0, 30.0, 60.0, 100.0, 250.0];
/// at most this many items in a search
const MAX_ITEMS: usize = 2000;

struct Searching {
    rx: Receiver<Result<Vec<Item>, String>>,
    cancel: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    started: Instant,
    search: Search,
}

type Listing = Result<Vec<(String, String)>, String>;

/// The window's choices, the search under way and its results.
pub struct StacUi {
    pub show: bool,
    /// one of CATALOGS, or CATALOGS.len(): a custom URL
    pub catalog: usize,
    pub custom_url: String,
    pub collection: String,
    /// each catalog's collections, listed when asked
    listed: HashMap<String, Listing>,
    listing: Option<(String, Receiver<Listing>)>,
    /// 0 the visible area, 1 the ROI, 2 a box of longitudes and latitudes
    pub area: usize,
    /// west, south, east, north
    pub bbox: [f64; 4],
    pub start: String,
    pub end: String,
    pub max_cloud: f64,
    pub res: usize,
    searching: Option<Searching>,
    pub items: Vec<Item>,
    /// the search the items answer
    pub searched: Option<Search>,
    pub error: String,
    pub bands: Vec<(BandChoice, bool)>,
    pub dates: Vec<(DateGroup, bool)>,
    /// a radar's passes over the area (sat:orbit_state, sat:relative_orbit), the most scenes first
    pub orbits: Vec<Orbit>,
    /// the pass kept (orbits.len(): all of them)
    pub orbit: usize,
    /// a search to start (--stac)
    start_pending: bool,
    /// its series opened once found (--stac-open)
    pub open_after: bool,
}

/// "YYYY-MM-DD" of a day `back` days before today.
fn day_text(back: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    janus_cube::dates::format_date(now - back as f64 * 86400.0, janus_cube::TimeGranularity::Day)
}

impl Default for StacUi {
    fn default() -> Self {
        StacUi {
            show: false,
            catalog: 0,
            custom_url: String::new(),
            collection: CATALOGS[0].collections[0].id.into(),
            listed: HashMap::new(),
            listing: None,
            area: 0,
            bbox: [-63.0, -10.0, -62.9, -9.9],
            start: day_text(365),
            end: day_text(0),
            max_cloud: 30.0,
            res: 0,
            searching: None,
            items: Vec::new(),
            searched: None,
            error: String::new(),
            bands: Vec::new(),
            dates: Vec::new(),
            orbits: Vec::new(),
            orbit: 0,
            start_pending: false,
            open_after: false,
        }
    }
}

impl StacUi {
    /// --stac CATALOG;COLLECTION;W,S,E,N;START;END[;CLOUD]: the window open with these choices, its search started.
    pub fn apply_option(&mut self, spec: &str) -> Result<(), String> {
        let p: Vec<&str> = spec.split(';').map(str::trim).collect();
        if p.len() < 5 {
            return Err("CATALOG;COLLECTION;W,S,E,N;START;END[;CLOUD]".into());
        }
        match CATALOGS.iter().position(|c| c.id == p[0]) {
            Some(i) => self.catalog = i,
            None => (self.catalog, self.custom_url) = (CATALOGS.len(), p[0].to_string()),
        }
        self.collection = p[1].to_string();
        let b: Vec<f64> = p[2].split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if b.len() != 4 {
            return Err("the box as W,S,E,N".into());
        }
        self.bbox = [b[0], b[1], b[2], b[3]];
        self.area = 2;
        (self.start, self.end) = (p[3].to_string(), p[4].to_string());
        if let Some(c) = p.get(5).and_then(|c| c.parse().ok()) {
            self.max_cloud = c;
        }
        self.show = true;
        self.start_pending = true;
        Ok(())
    }

    /// Nothing under way: the search answered (or none was asked for).
    pub fn settled(&self) -> bool {
        !self.start_pending && self.searching.is_none() && self.listing.is_none()
    }

    /// The dates of the items of the pass kept (all the items without passes), all ticked.
    pub(crate) fn regroup(&mut self) {
        let groups = match self.orbits.get(self.orbit) {
            Some(o) => stac::group_by_day_of(&self.items, &o.items),
            None => stac::group_by_day(&self.items),
        };
        self.dates = groups.into_iter().map(|g| (g, true)).collect();
    }

    fn url(&self) -> String {
        CATALOGS.get(self.catalog).map_or_else(|| self.custom_url.trim().to_string(), |c| c.url.to_string())
    }

    fn preset(&self) -> Option<&'static stac::Collection> {
        CATALOGS.get(self.catalog)?.collections.iter().find(|c| c.id == self.collection)
    }

    /// The layer's name: the collection and the catalog.
    fn series_name(&self, s: &Search) -> String {
        let collection = CATALOGS
            .iter()
            .flat_map(|c| c.collections.iter())
            .find(|c| c.id == s.collection)
            .map_or(s.collection.clone(), |c| c.name.to_string());
        let catalog = CATALOGS.iter().find(|c| c.url == s.url).map_or_else(
            || s.url.split('/').nth(2).unwrap_or(&s.url).to_string(),
            |c| c.name.split(" (").next().unwrap_or(c.name).to_string(),
        );
        format!("{collection} ({catalog})")
    }
}

impl State {
    pub(crate) fn stac_window(&mut self, ctx: &egui::Context) {
        if !self.stac.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Search STAC catalog", ctx, 480.0)
            .id(egui::Id::new("stac window"))
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.stac_ui(ui)));
        self.stac.show = open;
    }

    /// Searches and listings that answered.
    pub(crate) fn pump_stac(&mut self, ctx: &egui::Context) {
        if self.stac.start_pending {
            self.stac.start_pending = false;
            self.start_stac_search(ctx);
        }
        if self.stac.open_after && self.stac.searching.is_none() && !self.stac.start_pending {
            self.stac.open_after = false;
            if !self.stac.items.is_empty() {
                self.start_stac_job(false, ctx);
            }
        }
        let s = &mut self.stac;
        if let Some((url, rx)) = &s.listing
            && let Ok(r) = rx.try_recv()
        {
            s.listed.insert(url.clone(), r);
            s.listing = None;
        }
        let Some(sr) = &s.searching else { return };
        match sr.rx.try_recv() {
            Ok(r) => {
                let search = sr.search.clone();
                s.searching = None;
                match r {
                    Ok(items) => {
                        let choices = stac::band_choices(&items);
                        let on = stac::default_bands(&choices, s.preset().map(|p| p.bands));
                        s.bands = choices.into_iter().map(|c| (c.clone(), on.contains(&c.key))).collect();
                        s.error = if items.is_empty() {
                            "nothing found: another area, more dates or more clouds".into()
                        } else {
                            String::new()
                        };
                        s.items = items;
                        s.searched = Some(search);
                        // a radar's most frequent pass: one geometry through the series
                        s.orbits = stac::orbits(&s.items);
                        s.orbit = 0;
                        s.regroup();
                    }
                    Err(e) if e == "cancelled" => {}
                    Err(e) => s.error = e,
                }
            }
            Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(200)),
        }
    }

    /// The area searched (longitude, latitude), and its size in metres.
    fn stac_bounds(&self) -> Result<([f64; 4], f64, f64), String> {
        let s = &self.stac;
        if s.area < 2 {
            return self.embedding_bounds(s.area);
        }
        let b = s.bbox;
        if !(b[0] < b[2] && b[1] < b[3] && b[0] >= -180.0 && b[2] <= 180.0 && b[1] >= -90.0 && b[3] <= 90.0) {
            return Err("the box needs west < east and south < north (degrees)".into());
        }
        let lat = 0.5 * (b[1] + b[3]) * std::f64::consts::PI / 180.0;
        Ok((b, (b[2] - b[0]) * 111_320.0 * lat.cos(), (b[3] - b[1]) * 110_574.0))
    }

    fn start_stac_search(&mut self, ctx: &egui::Context) {
        let (bbox, _, _) = match self.stac_bounds() {
            Ok(b) => b,
            Err(e) => {
                self.stac.error = e;
                return;
            }
        };
        let s = &mut self.stac;
        let search = Search {
            url: s.url(),
            collection: s.collection.trim().to_string(),
            bbox,
            start: s.start.trim().to_string(),
            end: s.end.trim().to_string(),
            max_cloud: (s.max_cloud < 100.0 && stac::has_clouds(&s.collection)).then_some(s.max_cloud),
            max_items: MAX_ITEMS,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let count = Arc::new(AtomicUsize::new(0));
        let (q, cn, ct, c) = (search.clone(), cancel.clone(), count.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("stac search".into())
            .spawn(move || {
                let r = stac::search(
                    &q,
                    &|n| {
                        ct.store(n, Ordering::Relaxed);
                        c.request_repaint();
                    },
                    &cn,
                );
                let _ = tx.send(r);
                c.request_repaint();
            })
            .ok();
        s.error.clear();
        s.searching = Some(Searching { rx, cancel, count, started: Instant::now(), search });
    }

    /// The series of the dates and bands ticked: built (and downloaded) in a job, the layer added when it is ready.
    fn start_stac_job(&mut self, download: bool, ctx: &egui::Context) {
        let s = &self.stac;
        let Some(search) = s.searched.clone() else { return };
        let bands: Vec<BandChoice> = s.bands.iter().filter(|b| b.1).map(|b| b.0.clone()).collect();
        let items: Vec<Item> =
            s.dates.iter().filter(|d| d.1).flat_map(|d| d.0.items.iter().map(|&i| s.items[i].clone())).collect();
        let n_dates = s.dates.iter().filter(|d| d.1).count();
        let orbit = s.orbits.get(s.orbit).map(Orbit::label);
        // a radar's series shown in dB (VV) as it opens
        let sel = stac::display_index(&search.collection).and_then(janus_cube::calc::indices::find).map(|ix| {
            janus_cube::BandSelection {
                expr: janus_cube::calc::ExprId::intern(&ix.name, &ix.formula),
                chosen: true,
                ..Default::default()
            }
        });
        let res = RESOLUTIONS[s.res.min(RESOLUTIONS.len() - 1)];
        let name = s.series_name(&search);
        let first = s.dates.iter().find(|d| d.1).map(|d| d.0.day).unwrap_or(0);
        let last = s.dates.iter().rev().find(|d| d.1).map(|d| d.0.day).unwrap_or(0);
        let day = |d: i64| janus_cube::dates::format_date(d as f64 * 86400.0, janus_cube::TimeGranularity::Day);
        let folder = self.results_dir().join("stac").join(format!(
            "{}_{}_{}_{}",
            stac_vrt::sanitize(&search.collection),
            day(first),
            day(last),
            crate::zeit::timestamp()
        ));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.error_popup = Some(format!("could not create {}: {e}", folder.display()));
            return;
        }
        let title =
            format!("{} {name}: {n_dates} dates, {} bands", if download { "Download" } else { "STAC" }, bands.len());
        let spec = BuildSpec {
            items,
            bands,
            bbox: search.bbox,
            res: (res > 0.0).then_some(res),
            dir: if download { folder.join("vrt") } else { folder.clone() },
            planetary: stac::is_planetary(&search.url),
        };
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== {} job: {title}\ncatalog: {}\ncollection: {}\nbbox: {:?}\ndates: {} to {} ({n_dates})\nbands: {}\n\
             cells: {}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            search.url,
            search.collection,
            search.bbox,
            day(first),
            day(last),
            spec.bands.iter().map(|b| b.key.as_str()).collect::<Vec<_>>().join(", "),
            if res > 0.0 { format!("{res} m") } else { "the finest of the bands".into() },
            folder.display()
        ));
        if let Some(o) = &orbit {
            log.line(&format!("pass: {o}"));
        }
        let log_path = log.path.to_string_lossy().into_owned();
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, c, out) = (status.clone(), cancel.clone(), ctx.clone(), folder.clone());
        std::thread::Builder::new()
            .name("stac series".into())
            .spawn(move || {
                let t0 = Instant::now();
                let logged = Mutex::new((-1.0, -1.0));
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    let now = t0.elapsed().as_secs_f64();
                    let mut l = logged.lock().unwrap();
                    if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
                        log.line(&format!("{:3.0}%  {m}", f * 100.0));
                        *l = (f, now);
                    }
                    c.request_repaint();
                };
                // a download builds the VRTs first (a tenth of the job), then reads them whole
                let split = if download { 0.1 } else { 1.0 };
                let r = stac_vrt::build(&spec, &|f, m| progress(f * split, m), &cn).and_then(|built| {
                    log.line(&format!(
                        "grid: {} x {} cells of {} in {}{}",
                        built.grid.w,
                        built.grid.h,
                        built.grid.gt[1],
                        if built.grid.crs.len() > 40 { "its WKT" } else { &built.grid.crs },
                        if built.warped > 0 {
                            format!(", {} items warped onto it", built.warped)
                        } else {
                            String::new()
                        }
                    ));
                    if download {
                        stac_vrt::download(&built.files, &out, &|f, m| progress(split + (1.0 - split) * f, m), &cn)
                    } else {
                        Ok(built.files)
                    }
                });
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(files) => {
                        for f in &files {
                            log.line(&format!("output: {f}"));
                        }
                        log.line("done");
                        (s.state, s.progress) = (JobState::Done, 1.0);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        log.line("failed");
                        (s.state, s.error) = (JobState::Failed, e);
                    }
                }
                c.request_repaint();
            })
            .ok();
        let folder = folder.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "stac".into(),
            cube: 0,
            output_dir: folder.clone(),
            log_path,
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true, // nothing for the Zeit results to load
        });
        self.emb_downloads.push(EmbDownload {
            title: if download { format!("Download {name}") } else { name.clone() },
            status,
            started: Instant::now(),
            folder,
            name: Some(name),
            keep_active: false,
            sel,
            opened: false,
            ended: None,
        });
    }

    /// The items' footprints as a vector layer (a GeoJSON in the results folder).
    fn stac_footprints(&mut self, ctx: &egui::Context) {
        let dir = self.results_dir().join("stac");
        let f = dir.join(format!("footprints_{}.geojson", crate::zeit::timestamp()));
        let r =
            std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&f, stac::footprints_geojson(&self.stac.items)));
        match r {
            Ok(()) => self.add_vector_files(vec![f.to_string_lossy().into_owned()], ctx),
            Err(e) => self.error_popup = Some(format!("{}: {e}", f.display())),
        }
    }

    pub(crate) fn stac_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let w = ui.available_width();
        let has = self.session.is_some();
        if !has && self.stac.area < 2 {
            self.stac.area = 2;
        }
        ui.add(
            egui::Label::new(
                "Satellite images of a catalog (STAC) as a series, read in place: each date the mosaic of its \
                 scenes over the area, with the bands you pick, on one grid. Only what the map shows is \
                 downloaded, or the whole series once as GeoTIFFs.",
            )
            .wrap(),
        );
        crate::panels::section(ui, "Catalog");
        let s = &mut self.stac;
        let before = (s.catalog, s.custom_url.clone());
        let cat_name = CATALOGS.get(s.catalog).map_or("Another STAC API (URL)", |c| c.name);
        egui::ComboBox::from_id_salt("stac catalog").width(w).selected_text(cat_name).show_ui(ui, |ui| {
            for (i, c) in CATALOGS.iter().enumerate() {
                ui.selectable_value(&mut s.catalog, i, c.name).on_hover_text(c.help);
            }
            ui.selectable_value(&mut s.catalog, CATALOGS.len(), "Another STAC API (URL)");
        });
        if let Some(c) = CATALOGS.get(s.catalog) {
            ui.weak(c.help);
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut s.custom_url)
                    .hint_text("https://.../stac/v1 (the API's root, with /search)")
                    .desired_width(w),
            );
        }
        if (s.catalog, s.custom_url.clone()) != before
            && let Some(c) = CATALOGS.get(s.catalog)
        {
            s.collection = c.collections[0].id.into();
        }
        // the collection: the catalog's offered ones, those listed from the API, or typed
        let url = s.url();
        ui.horizontal(|ui| {
            let shown = s.preset().map_or(s.collection.clone(), |p| p.name.to_string());
            egui::ComboBox::from_id_salt("stac collection").width((w - 90.0).max(120.0)).selected_text(shown).show_ui(
                ui,
                |ui| {
                    if let Some(c) = CATALOGS.get(s.catalog) {
                        for p in c.collections {
                            ui.selectable_value(&mut s.collection, p.id.to_string(), p.name).on_hover_text(p.help);
                        }
                        ui.separator();
                    }
                    match s.listed.get(&url) {
                        Some(Ok(list)) => {
                            for (id, title) in list {
                                let label = if title.is_empty() { id.clone() } else { format!("{id}  {title}") };
                                ui.selectable_value(&mut s.collection, id.clone(), label);
                            }
                        }
                        Some(Err(e)) => {
                            ui.colored_label(theme::warning(), e);
                        }
                        None => {
                            ui.weak("List all to see the catalog's other collections");
                        }
                    }
                },
            );
            let listing = s.listing.is_some();
            if ui
                .add_enabled(
                    !listing && !url.is_empty(),
                    egui::Button::new(if listing { "Listing..." } else { "List all" }),
                )
                .on_hover_text("The catalog's collections (its /collections)")
                .clicked()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                let (u, c) = (url.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(stac::collections(&u));
                    c.request_repaint();
                });
                s.listing = Some((url.clone(), rx));
            }
        });
        if let Some(p) = s.preset() {
            ui.weak(p.help);
        } else {
            ui.add(egui::TextEdit::singleline(&mut s.collection).hint_text("collection id").desired_width(w));
        }
        crate::panels::section(ui, "Area and dates");
        ui.horizontal(|ui| {
            ui.add_enabled_ui(has, |ui| {
                ui.radio_value(&mut s.area, 0, "Visible area");
                ui.radio_value(&mut s.area, 1, "ROI");
            });
            ui.radio_value(&mut s.area, 2, "Box (degrees)");
        });
        if s.area == 2 {
            ui.horizontal(|ui| {
                for (k, label) in ["W", "S", "E", "N"].iter().enumerate() {
                    ui.label(*label);
                    let range = if k % 2 == 0 { -180.0..=180.0 } else { -90.0..=90.0 };
                    ui.add(egui::DragValue::new(&mut s.bbox[k]).range(range).speed(0.01).fixed_decimals(4));
                }
            });
        }
        let bounds = self.stac_bounds();
        let s = &mut self.stac;
        match &bounds {
            Ok((_, wm, hm)) => {
                ui.weak(format!("{:.1} x {:.1} km", wm / 1000.0, hm / 1000.0));
            }
            Err(e) => {
                ui.colored_label(theme::warning(), e);
            }
        }
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(egui::TextEdit::singleline(&mut s.start).desired_width(90.0));
            ui.label("to");
            ui.add(egui::TextEdit::singleline(&mut s.end).desired_width(90.0)).on_hover_text("YYYY-MM-DD, inclusive");
        });
        if stac::has_clouds(&s.collection) {
            ui.add(
                egui::Slider::new(&mut s.max_cloud, 0.0..=100.0).fixed_decimals(0).suffix(" %").text("clouds at most"),
            )
            .on_hover_text("The scenes' cloud cover (eo:cloud_cover), as the catalog gives it; 100: all");
        }
        let dates_ok = stac::parse_time(&s.start).zip(stac::parse_time(&s.end)).is_some_and(|(a, b)| a <= b);
        if !dates_ok {
            ui.colored_label(theme::warning(), "dates as YYYY-MM-DD, the first not after the last");
        }
        let can = bounds.is_ok() && dates_ok && !s.collection.trim().is_empty() && !url.is_empty();
        if let Some(sr) = &s.searching {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "searching: {} scenes ({:.0} s)",
                    sr.count.load(Ordering::Relaxed),
                    sr.started.elapsed().as_secs_f64()
                ));
                if ui.button("Cancel").clicked() {
                    sr.cancel.store(true, Ordering::Relaxed);
                }
            });
        } else if ui
            .add_enabled(can, egui::Button::new("Search").min_size(vec2(w, 0.0)))
            .on_hover_text("The catalog's scenes over the area between the dates (STAC API /search)")
            .clicked()
        {
            self.start_stac_search(&ctx);
        }
        let s = &mut self.stac;
        if !s.error.is_empty() {
            ui.colored_label(theme::warning(), &s.error);
        }
        if s.items.is_empty() {
            return;
        }
        crate::panels::section(ui, "Dates");
        if !s.orbits.is_empty() {
            let label = |o: &Orbit| format!("{} ({} scenes)", o.label(), o.items.len());
            let shown = s.orbits.get(s.orbit).map_or("all passes (mixed)".to_string(), label);
            let mut pick = s.orbit;
            ui.horizontal(|ui| {
                ui.label("Pass");
                egui::ComboBox::from_id_salt("stac orbit")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for (i, o) in s.orbits.iter().enumerate() {
                            ui.selectable_value(&mut pick, i, label(o));
                        }
                        ui.selectable_value(&mut pick, s.orbits.len(), "all passes (mixed)");
                    })
                    .response
                    .on_hover_text(
                        "The satellite's direction (ascending or descending) and relative orbit: scenes of one\n\
                     pass see the ground from the same side and angle. Mixing passes makes false steps\n\
                     in the series; the pass with the most scenes is kept at first.",
                    );
            });
            if pick != s.orbit {
                s.orbit = pick;
                s.regroup();
            }
        }
        let ticked = s.dates.iter().filter(|d| d.1).count();
        ui.label(format!("{} scenes on {} dates; {ticked} ticked", s.items.len(), s.dates.len()));
        ui.horizontal(|ui| {
            if ui.small_button("All").clicked() {
                s.dates.iter_mut().for_each(|d| d.1 = true);
            }
            if ui.small_button("None").clicked() {
                s.dates.iter_mut().for_each(|d| d.1 = false);
            }
            if s.dates.iter().any(|d| d.0.cloud.is_some())
                && ui
                    .small_button("Clearest (< 10%)")
                    .on_hover_text("Only the dates whose scenes have less than 10% of clouds on average")
                    .clicked()
            {
                s.dates.iter_mut().for_each(|d| d.1 = d.0.cloud.is_none_or(|c| c < 10.0));
            }
        });
        egui::ScrollArea::vertical().id_salt("stac dates").max_height(180.0).show(ui, |ui| {
            for (g, on) in &mut s.dates {
                let date = janus_cube::dates::format_date(g.day as f64 * 86400.0, janus_cube::TimeGranularity::Day);
                let n = g.items.len();
                let clouds = g.cloud.map_or(String::new(), |c| format!(", {c:.0}% clouds"));
                ui.checkbox(on, format!("{date}  {n} scene{}{clouds}", if n > 1 { "s" } else { "" }));
            }
        });
        crate::panels::section(ui, "Bands");
        ui.horizontal_wrapped(|ui| {
            for (b, on) in &mut s.bands {
                let r = ui.checkbox(on, &b.label);
                let mut tip = format!("asset \"{}\"", b.key);
                if let Some(nm) = b.wavelength_nm {
                    tip += &format!(", {nm:.0} nm");
                }
                if b.quality != janus_cube::QaRule::None {
                    tip += &format!("\nmasks clouds ({})", b.quality.label());
                }
                r.on_hover_text(tip);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Cells");
            let label =
                |i: usize| if i == 0 { "finest of the bands".to_string() } else { format!("{} m", RESOLUTIONS[i]) };
            egui::ComboBox::from_id_salt("stac res")
                .selected_text(label(s.res))
                .show_ui(ui, |ui| {
                    for i in 0..RESOLUTIONS.len() {
                        ui.selectable_value(&mut s.res, i, label(i));
                    }
                })
                .response
                .on_hover_text(
                    "The series' cell size. Coarser cells average the reflectances (the quality\n\
                 band takes the nearest code): lighter series for large areas.",
                );
        });
        let n_bands = s.bands.iter().filter(|b| b.1).count();
        let mut block = None;
        if ticked < 2 {
            block = Some("tick 2 dates or more (a series)");
        } else if n_bands == 0 {
            block = Some("tick a band");
        }
        if let (Some(q), Ok((_, wm, hm))) = (&s.searched, &bounds) {
            let res = if RESOLUTIONS[s.res] > 0.0 {
                RESOLUTIONS[s.res]
            } else {
                // the finest band ticked, from the first item that has its grid
                s.bands
                    .iter()
                    .filter(|b| b.1)
                    .filter_map(|b| {
                        s.items.iter().find_map(|it| {
                            let a = it.assets.get(&b.0.key)?;
                            a.gt.map(|g| g[1].abs()).or(a.resolution).or(it.gt.map(|g| g[1].abs()))
                        })
                    })
                    .fold(f64::INFINITY, f64::min)
            };
            if res.is_finite() {
                let (cw, ch) = ((wm / res).ceil(), (hm / res).ceil());
                let gb = cw * ch * 2.0 * (n_bands * ticked) as f64 / (1u64 << 30) as f64;
                ui.weak(format!(
                    "{cw:.0} x {ch:.0} cells of {res} m; {ticked} dates x {n_bands} bands (~{gb:.1} GB uncompressed)"
                ))
                .on_hover_text(format!(
                    "{:.4}, {:.4} to {:.4}, {:.4} (longitude, latitude)",
                    q.bbox[0], q.bbox[1], q.bbox[2], q.bbox[3]
                ));
                if cw > stac_vrt::MAX_SIDE as f64 || ch > stac_vrt::MAX_SIDE as f64 {
                    block = Some("too many cells: a smaller area or coarser cells");
                }
            }
            if q.bbox != bounds.as_ref().map(|b| b.0).unwrap_or(q.bbox) {
                ui.weak("(the series covers the area searched; search again for the new one)");
            }
        }
        let busy = self.opening.is_some();
        let s = &self.stac;
        let name = s.searched.as_ref().map(|q| s.series_name(q)).unwrap_or_default();
        ui.horizontal(|ui| {
            let bw = (w - 8.0) / 2.0;
            if ui
                .add_enabled(block.is_none() && !busy, egui::Button::new("Open as a layer").min_size(vec2(bw, 0.0)))
                .on_hover_text(format!(
                    "\"{name}\": a VRT per date pointing at the catalog's files, read in place\n\
                     (the overviews when zoomed out). Needs the connection while you work.\n\
                     Saved under {}",
                    self.results_dir().join("stac").display()
                ))
                .clicked()
            {
                self.start_stac_job(false, &ctx);
            }
            if ui
                .add_enabled(block.is_none(), egui::Button::new("Download as GeoTIFFs").min_size(vec2(bw, 0.0)))
                .on_hover_text(
                    "The same series read once and written as cloud-optimized GeoTIFFs\n\
                     (one per date): works offline after. Progress in Tools > Tasks.",
                )
                .clicked()
            {
                self.start_stac_job(true, &ctx);
            }
        });
        if let Some(b) = block {
            ui.colored_label(theme::warning(), b);
        }
        if ui
            .add(egui::Button::new("Footprints as a vector layer").min_size(vec2(w, 0.0)))
            .on_hover_text("The scenes' outlines, with their id, date and cloud cover")
            .clicked()
        {
            self.stac_footprints(&ctx);
        }
        // the jobs started from here
        for d in &self.emb_downloads {
            let st = d.status.lock().unwrap();
            if st.state == JobState::Running && d.name.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("{}: {:.0}% {}", d.title, st.progress * 100.0, st.message));
                });
            }
        }
    }
}
