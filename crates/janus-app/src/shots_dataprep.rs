//! The Data preparation guide's screenshots (`--shots dataprep DATA OUT`; site/data-preparation.html), on real data.
//! DATA is the folder tools/make_dataprep_data.py makes: `serra/series` (36 clear dates of 10 x 10 km of the Serra dos
//! Orgaos, Sentinel-2 L2A, 2022 and 2023, with each tile's MTD_TL.xml), `topo/series` (eight Landsat 8 and 9 dates of
//! 21 x 21 km of the same mountains, the sun's zenith spread from 31 to 55 degrees), `mixed/landsat` and `mixed/s2`
//! (Landsat 8 and 9 and Sentinel-2 dates of 36 x 36 km of irrigated fields in western Bahia on one 30 m grid) and
//! `mixed/coreg` (eight of them, four of each sensor, one Landsat date with its georeference moved by a pixel on
//! purpose). The tour takes the pictures of the figures of the first half of the page (the menu, NBAR, the spectral
//! adjustment, harmonizing the sensors, the topographic correction and the co-registration), runs the tools the
//! pictures need and prints the numbers the text quotes. The windows and charts are written as cropped PNG; the maps
//! that go side by side are parts (`parts/`) that tools/make_dataprep_figures.py puts together.
//!
//! JANUS_DATAPREP_FROM=<first stage of a block> runs the tour from that block on (the blocks open their own series).
//!
//! The stage numbers leave room: each block ends by jumping to the next one's first stage, the last (the
//! co-registration) to `OTHERS`, where the tour of the page's other figures goes (stages 180 to 299); the last arm closes the tour.

use std::path::{Path, PathBuf};

use egui::{Context, Event, PointerButton};
use janus_cube::BandSelection;
use janus_zeit::manifest;
use serde_json::{Value, json};

use crate::app::{State, Tab};
use crate::session::Opening;
use crate::shots::{Shots, Target, tool_tab};
use crate::zeit::JobState;

const TOOLS: [&str; 11] = [
    "nbar",
    "spectral_adjust",
    "topocorr",
    "coregister",
    "level2",
    "tmask",
    "clear_observations",
    "interpolate",
    "smooth",
    "standardize",
    "improphe",
];

/// The blocks' first stages; a block ends by jumping to the next one's.
const MENU: usize = 5;
const NBAR: usize = 10;
const SPECTRAL: usize = 40;
const HARMONIZE: usize = 60;
const TOPO: usize = 100;
const COREG: usize = 140;
const OTHERS: usize = 180;
const LEVEL2: usize = OTHERS;
const CLOUDY: usize = 200;
const IMPROPHE: usize = 260;
/// The last stage: the tour's end.
const END: usize = 299;
/// ImproPhe's kernel radius (map units; 50 pixels of 10 m): lowered from the default 2500 m, which is minutes for a
/// window of 600 x 600 pixels.
const IMPROPHE_RADIUS: f64 = 500.0;

/// The pixels of the Serra (x to the right, y down from the window's north-west corner), chosen by
/// `make_dataprep_data.py pins` on the Copernicus DEM and the series' own NIR. For NBAR (Sentinel-2, 10 m): the flat
/// (under 2 degrees) forest of the highest median NDVI (0.91) in uniform 7 x 7 pixels. For the topographic correction
/// (Landsat, 30 m): two pixels of forest (median NDVI over 0.85 on the dates of the high sun, 10th percentile over 0.7,
/// uniform 3 x 3) on the two sides of a ridge, 120 m apart at the same height: one on a slope of 32 degrees facing the
/// sun of the date of the lowest sun (cos i 0.92) and one on a slope of 25 degrees facing away (0.22), each with the NIR
/// of the median of its class that day (0.38 and 0.17).
const FLAT_FOREST: (usize, usize) = (988, 809);
const SUNLIT: (usize, usize) = (165, 366);
const SHADED: (usize, usize) = (165, 370);
/// The date the ridge's maps are shown at (the lowest sun), and the place and the screen points a pixel takes.
const WINTER: &str = "2022-06-16";
const RIDGE: (f64, f64, f64) = (165.0, 368.0, 9.0);

/// The stable pixel of the mixed series' chart (30 m, x to the right, y down), by `make_dataprep_data.py mixed-pins`:
/// evergreen forest (median NDVI 0.85 on the 58 Sentinel-2 and the 27 Landsat dates, uniform 5 x 5 pixels), the one
/// whose NDVI varies least (standard deviation 0.017). The band the harmonization is read in is the blue (1): the one
/// HLS's lines change most (the Sentinel-2 mean there is 0.0217 against Landsat's 0.0133).
const STABLE: (usize, usize) = (782, 492);
const HARM_BAND: i32 = 1;

/// The classes of the quality flags as the tour's expression gives them: value, name and colour.
const QAI_CLASSES: [(i32, &str, [u8; 3]); 6] = [
    (0, "clear", [70, 70, 70]),
    (1, "cloud buffer", [140, 190, 255]),
    (2, "cloud", [255, 255, 255]),
    (3, "cloud shadow", [150, 60, 190]),
    (4, "water", [30, 100, 230]),
    (5, "no data", [0, 0, 0]),
];

/// The view of the Level 1C figure: the scene's pixel (x to the right, y down) at the middle of the map and the screen
/// points a pixel takes.
const VIEW_L2: (f64, f64, f64) = (6500.0, 6000.0, 0.45);

/// The cloudy series (`cloudy`: the Serra's 21 x 21 km, Landsat 8 and 9 of 2022 to 2024 with the cloudy dates too, 30 m,
/// 700 x 700 px) and its pixels (x to the right, y down), chosen by `check_dataprep_numbers.py cloudy`.
const FOREST_PIN: (usize, usize) = (119, 464);
const PASTURE_PIN: (usize, usize) = (502, 484);
const TMASK_PIN: (usize, usize) = (580, 318);
const GAPS_PIN: (usize, usize) = (681, 41);
/// The cloudy date the screened series is shown at (a prefix of the label), and the wet and the dry month of the
/// clear-sky observations' figure.
const CLOUDY_DATE: &str = "2024-08-24";
const WET_MONTH: &str = "2023-02";
const DRY_MONTH: &str = "2023-06";
/// The range the two months' counts are drawn in (the most clear dates a month has).
const CSO_MAX: f32 = 4.0;

/// What the tour keeps between its stages.
#[derive(Default)]
pub struct DataprepShots {
    /// the layers before a run that opens a series
    layers: usize,
    /// the runs' folders by name
    runs: Vec<(String, String)>,
    /// the range of values the two maps of the ridge are drawn in
    range: (f32, f32),
    /// the layers of the NBAR of the NIR and of its c-factors (found by their names, which the charts' legends change)
    nbar: usize,
    cfactor: usize,
    /// the Level 1C result's surface reflectance and quality layers
    boa: usize,
    qai: usize,
    /// the clear-sky observations' count layer
    cso: usize,
}

fn go(sh: &mut Shots, done: &str, to: usize) {
    sh.next(done);
    sh.stage = to;
}

fn tabs(sh: &mut Shots, under: f32) {
    sh.under = under;
    sh.layout = Some((TOOLS.iter().map(|t| tool_tab(t)).collect(), vec![Tab::Tasks]));
}

fn cat(run: &str, file: &str) -> String {
    std::fs::read_to_string(Path::new(run).join(file)).unwrap_or_default()
}

fn folder(sh: &Shots, parts: &[&str]) -> String {
    let mut p = PathBuf::from(&sh.input);
    for part in parts {
        p.push(part);
    }
    p.to_string_lossy().into_owned()
}

impl State {
    /// A series opened (replacing the layers, or as one more), shown as `band`.
    fn dp_open(&mut self, path: String, add: bool, band: i32, ctx: &Context) {
        self.error = None;
        let sel = BandSelection { band, ..Default::default() };
        self.opening = Some(Opening::start_with(vec![path], add, sel, ctx));
    }

    /// Nothing opening and the last layer loaded.
    fn dp_loaded(&self) -> bool {
        self.opening.is_none()
            && self.session.is_some()
            && crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
    }

    /// Whether the layers have settled after a run opened a series: nothing opening, `n` layers, the last loaded.
    fn dp_series_open(&self, n: usize) -> bool {
        self.zeit.series_to_open.is_empty()
            && self.opening.is_none()
            && self.layer_jobs.queue.is_empty()
            && self.layers.len() == n
            && crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
    }

    /// A tool's window: opened with `params` (frame 1), brought to the front (3), its picture once drawn.
    fn dp_window(&mut self, sh: &mut Shots, id: &str, params: Value, file: &str, scroll: f32) -> bool {
        if sh.frames == 1 {
            self.shots_open_tool(id, params);
            self.zeit.show_tasks = false;
            return false;
        }
        if sh.frames == 3 {
            sh.focus.push(tool_tab(id));
        }
        if sh.frames == 5 && scroll > 0.0 {
            // the description is long: the picture starts under it, at the bands
            sh.scroll = Some((tool_tab(id), scroll));
        }
        if !self.shots_quiet() || sh.time() < 6.0 {
            return false;
        }
        sh.shoot("-", &[(file, Shots::fit(tool_tab(id)))]);
        true
    }

    /// The run's state: -1 running, 0 done, 1 failed (a run that writes series and no map is done with its task).
    fn dp_done(&self, sh: &Shots) -> i32 {
        match sh.job.and_then(|j| self.zeit.jobs.get(j)).map(|j| j.status.lock().unwrap().state) {
            Some(JobState::Done) => 0,
            Some(JobState::Running) => -1,
            _ => 1,
        }
    }

    /// The run `key` done: its folder noted and printed, the window closed, no map shown.
    fn dp_finish(&mut self, sh: &mut Shots, key: &str, id: Option<&str>, files: &[&str]) -> Option<String> {
        let job = sh.job.and_then(|j| self.zeit.jobs.get(j))?;
        let run = job.output_dir.clone();
        println!("    {key}: {:.1} s, folder {run}", job.status.lock().unwrap().seconds);
        for r in self.zeit.results.iter().filter(|r| r.run == run) {
            println!("      map {:?} ({} x {}, {:?})", r.name, r.w, r.h, r.classes);
        }
        sh.dataprep.runs.push((key.to_string(), run.clone()));
        let data = sh.out.join("data");
        let _ = std::fs::create_dir_all(&data);
        for f in files {
            let _ = std::fs::copy(Path::new(&run).join(f), data.join(format!("{key}-{f}")));
        }
        self.zeit.show_tasks = false;
        if let Some(t) = id.and_then(|id| manifest().tool(id)) {
            self.set_tool_open(t, false);
        }
        self.shots_hide_results();
        Some(run)
    }

    /// The layer whose name has every word of `words` (lower case).
    fn dp_layer(&self, words: &[&str]) -> Option<usize> {
        self.layers.iter().position(|l| {
            let n = l.name.to_lowercase();
            words.iter().all(|w| n.contains(w))
        })
    }

    /// Only layer `k` shown, active, with a pin of each of `pins`; the chart of all visible layers when `chart`.
    fn dp_show(&mut self, shown: &[usize], active: usize, pins: &[(usize, usize)], ctx: &Context) {
        for k in 0..self.layers.len() {
            self.layers[k].visible = shown.contains(&k);
        }
        self.set_active(active, ctx);
        self.shots_hide_results();
        self.clear_pins();
        for &(x, y) in pins {
            self.add_pin(x, y);
        }
    }

    /// The chart of the layers shown (frame 1 sets it up), its picture once drawn.
    #[allow(clippy::too_many_arguments)]
    fn dp_chart(
        &mut self,
        sh: &mut Shots,
        shown: &[usize],
        pins: &[(usize, usize)],
        files: &[(&str, Target)],
        ctx: &Context,
    ) -> bool {
        if sh.frames == 1 {
            self.dp_show(shown, shown[0], pins, ctx);
            self.set_chart_layers(1);
            tabs(sh, 0.45);
            return false;
        }
        if sh.frames == 30 {
            self.map.fit();
        }
        if sh.frames < 40 || !self.shots_quiet() || sh.time() < 10.0 {
            return false;
        }
        sh.shoot("-", files);
        true
    }

    /// The tool's window closed.
    fn dp_close(&mut self, id: &str) {
        if let Some(t) = manifest().tool(id) {
            self.set_tool_open(t, false);
        }
    }

    /// A pixel tool's window (opened with `params` in frame 1, in front from frame 3) and the chart of the pins with the
    /// tool's fit drawn over their series; the pictures once the chart is drawn.
    #[allow(clippy::too_many_arguments)]
    fn dp_tool_chart(
        &mut self,
        sh: &mut Shots,
        id: &str,
        params: Option<Value>,
        scroll: f32,
        pins: &[(usize, usize)],
        files: &[(&str, Target)],
        ctx: &Context,
    ) -> bool {
        if sh.frames == 1
            && let Some(params) = params
        {
            self.shots_open_tool(id, params);
            self.zeit.show_tasks = false;
        }
        if sh.frames == 3 {
            sh.focus.push(tool_tab(id));
        }
        if sh.frames == 5 && scroll > 0.0 {
            sh.scroll = Some((tool_tab(id), scroll));
        }
        if sh.frames == 39 {
            let tab = tool_tab(id);
            println!("    {id}: shown {}, docked {}, tabs {:?}", self.shown(tab), sh.docked.contains(&tab), sh.docked);
        }
        self.dp_chart(sh, &[0], pins, files, ctx)
    }

    pub(crate) fn shots_dataprep(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL (dataprep, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        // the runs take as long as they take
        let slow = matches!(sh.stage, 14..=16 | 64..=66 | 104..=106 | 144..=146 | 154..=156 | 184..=187 | 224..=229 | 234..=236 | 264..=267);
        if t > if slow { 7200.0 } else { 300.0 } {
            return fail(sh, "timeout");
        }
        // (no notice of a task that ended over a window or the chart)
        self.clear_notices();
        match sh.stage {
            // ---- the series and the menu -----------------------------------------------------------------------
            0 => {
                let cache = std::env::var_os("JANUS_SHOTS_CACHE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::temp_dir().join("janus-dataprep-shots"));
                let _ = std::fs::remove_dir_all(cache.join("results"));
                let _ = std::fs::create_dir_all(cache.join("results"));
                let _ = std::fs::create_dir_all(sh.out.join("parts"));
                self.zeit.results_dir = Some(cache.join("results"));
                let path = folder(sh, &["serra", "series"]);
                self.dp_open(path, false, 4, ctx);
                sh.next("the Serra's Sentinel-2 series opening");
            }
            1 => {
                if !self.dp_loaded() {
                    return None;
                }
                tabs(sh, 0.64);
                let info = self.session.as_ref()?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    series: {} x {} px, {} dates {} to {}, {} bands a date {:?}, geotransform {:?}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    labels.first().unwrap_or(&""),
                    labels.last().unwrap_or(&""),
                    info.bands_per_date,
                    info.band_names,
                    info.geo_transform,
                    info.crs_name
                );
                println!("    roles: {:?}", crate::zeit::guess_roles(&info));
                self.set_date(self.shots_date_of(WINTER));
                sh.next("the series open");
            }
            2 => {
                if sh.frames == 30 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.next("the series shown");
                // JANUS_DATAPREP_FROM=40 (or 60, 100, 140...): the tour from that block on (each opens its own series)
                sh.stage = std::env::var("JANUS_DATAPREP_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(MENU);
            }
            // ---- the menu (the Tools menu, its Data preparation submenu) ------------------------------------------
            5 => {
                let Some(&m) = sh.menus.get("Tools") else {
                    return if t > 10.0 { fail(sh, "the Tools menu is not drawn") } else { None };
                };
                if sh.frames <= 2 {
                    let c = m.center();
                    sh.events.push(Event::PointerMoved(c));
                    sh.events.push(Event::PointerButton {
                        pos: c,
                        button: PointerButton::Primary,
                        pressed: sh.frames == 1,
                        modifiers: Default::default(),
                    });
                    return None;
                }
                // the pointer on "Data preparation" opens its submenu
                let Some(&d) = sh.menus.get("Data preparation") else {
                    return if t > 10.0 { fail(sh, "the Data preparation entry is not drawn") } else { None };
                };
                sh.events.push(Event::PointerMoved(d.center()));
                if t < 3.0 {
                    return None;
                }
                sh.shoot("-", &[("01-menu", Target::Menus(m))]);
                sh.next("Tools > Data preparation");
            }
            6 => {
                if sh.frames == 1 {
                    egui::Popup::close_all(ctx);
                    return None;
                }
                if sh.frames < 6 {
                    return None;
                }
                go(sh, "the menu closed", NBAR);
            }
            // ---- NBAR ----------------------------------------------------------------------------------------------
            10 => {
                if sh.frames == 1 {
                    // the Sentinel-2 granule metadata, every band mapped (the narrow NIR as nir08), the c-factor written
                    let _ = self.active_roles();
                    self.zeit.roles.insert("nir08".into(), 5);
                    println!("    roles for NBAR: {:?}", self.zeit.roles);
                }
                if self.dp_window(
                    sh,
                    "nbar",
                    json!({ "angles": "sentinel2", "sza": "fixed", "sza_fixed": 30.0, "cfactor": true }),
                    "08-nbar-window",
                    300.0,
                ) {
                    sh.next("the NBAR window");
                    sh.stage = NBAR + 4;
                }
            }
            14 => {
                // the run on the red and the NIR only (the chart is of the NIR): fewer layers to open
                for r in ["blue", "green", "swir1", "swir2", "nir08"] {
                    self.zeit.roles.remove(r);
                }
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "nbar", 0, None, ctx) {
                    1 => return fail(sh, &format!("NBAR did not start: {:?}", self.error_popup)),
                    0 => sh.next("NBAR running"),
                    _ => {}
                }
            }
            15 => match self.dp_done(sh) {
                0 => sh.next("NBAR done"),
                -1 => {}
                _ => return fail(sh, &format!("NBAR failed: {}", self.shots_job_error(sh))),
            },
            16 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "nbar", Some("nbar"), &[])?;
                    println!("    job.log:\n{}", cat(&run, "job.log"));
                    return None;
                }
                // the NBAR series and the c-factors of the red and the NIR open as layers
                if !self.dp_series_open(sh.dataprep.layers + 4) {
                    return if t > 600.0 { fail(sh, "the four series should open") } else { None };
                }
                for l in &self.layers {
                    println!("    layer {:?}", l.name);
                }
                sh.next("the NBAR series open");
            }
            17 => {
                // the NIR as it is and as NBAR made it, on the flat forest's pixel
                if sh.frames == 1 {
                    let (Some(nbar), Some(c)) =
                        (self.dp_layer(&["nbar_nbar", "nir"]), self.dp_layer(&["cfactor", "nir"]))
                    else {
                        return fail(sh, "no layer 'NBAR nir' or c-factor of the NIR");
                    };
                    (sh.dataprep.nbar, sh.dataprep.cfactor) = (nbar, c);
                    self.layers[0].name = "NIR as observed".into();
                    self.layers[nbar].name = "NIR after NBAR (30 deg)".into();
                }
                let nbar = sh.dataprep.nbar;
                if self.dp_chart(
                    sh,
                    &[0, nbar],
                    &[FLAT_FOREST],
                    &[("09-nbar-chart", Shots::tab(Tab::TimeSeries)), ("09-nbar-table", Shots::tab(Tab::Statistics))],
                    ctx,
                ) {
                    sh.next("the NIR before and after NBAR");
                }
            }
            18 => {
                let c = sh.dataprep.cfactor;
                if sh.frames == 1 {
                    self.layers[c].name = "c-factor of the NIR".into();
                }
                if self.dp_chart(sh, &[c], &[FLAT_FOREST], &[("09-nbar-cfactor", Shots::tab(Tab::TimeSeries))], ctx) {
                    go(sh, "the c-factor of the NIR", SPECTRAL);
                }
            }
            // ---- the spectral adjustment ---------------------------------------------------------------------------
            40 => {
                // a Landsat 8-9 series, NIR and red as the NDVI as in the guides
                if sh.frames == 1 {
                    self.set_chart_layers(0);
                    self.clear_pins();
                    self.error = None;
                    let sel = BandSelection { band: 4, nd_band: 3, ..Default::default() };
                    let path = folder(sh, &["mixed", "landsat"]);
                    self.opening = Some(Opening::start_with(vec![path], false, sel, ctx));
                    return None;
                }
                if !self.dp_loaded() {
                    return None;
                }
                tabs(sh, 0.64);
                let info = self.session.as_ref()?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    Landsat series: {} x {} px, {} dates {} to {}, bands {:?}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    labels.first().unwrap_or(&""),
                    labels.last().unwrap_or(&""),
                    info.band_names,
                    info.crs_name
                );
                println!("    roles: {:?}", crate::zeit::guess_roles(&info));
                self.shots_colormap("NDVI");
                self.set_date(info.t() - 1);
                sh.next("the Landsat series open");
            }
            41 => {
                if sh.frames == 20 {
                    self.map.fit();
                }
                if self.dp_window(sh, "spectral_adjust", json!({}), "10-spectral-window", 0.0) {
                    go(sh, "the spectral adjustment's window", HARMONIZE);
                }
            }
            // ---- harmonizing the sensors ---------------------------------------------------------------------------
            60 => {
                if sh.frames == 1 {
                    if let Some(t) = manifest().tool("spectral_adjust") {
                        self.set_tool_open(t, false);
                    }
                    let path = folder(sh, &["mixed", "s2"]);
                    self.dp_open(path, false, HARM_BAND, ctx);
                    return None;
                }
                if !self.dp_loaded() {
                    return None;
                }
                sh.next("the Sentinel-2 series open");
            }
            61 => {
                if sh.frames == 1 {
                    let path = folder(sh, &["mixed", "landsat"]);
                    self.dp_open(path, true, HARM_BAND, ctx);
                    return None;
                }
                if !self.dp_loaded() || self.layers.len() != 2 {
                    return None;
                }
                for (i, l) in self.layers.iter().enumerate() {
                    let info = &self.layer_session(i)?.info;
                    println!(
                        "    layer {i}: {:?}, {} dates, {}, values times {} by Harmonize's guess, {}",
                        l.name,
                        info.t(),
                        info.description,
                        janus_zeit::harmonize::guess_factor(info),
                        info.data_type
                    );
                }
                sh.next("the Landsat series open too");
            }
            62 => {
                // the window: the Sentinel-2 series active, the Landsat one to join
                if sh.frames == 1 {
                    for l in &mut self.layers {
                        l.visible = false;
                    }
                    self.layers[0].visible = true;
                    self.set_active(0, ctx);
                    let other = self.layer_session(1)?.info.id;
                    self.harmonize.show = true;
                    self.harmonize.join = Some(other);
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 12 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("-", &[("11-harmonize-window", Target::Area("Harmonize sensors"))]);
                sh.next("the Harmonize window");
            }
            63 => {
                if sh.frames == 1 {
                    sh.dataprep.layers = self.layers.len();
                    let before = self.zeit.jobs.len();
                    self.harmonize_run(ctx);
                    if self.zeit.jobs.len() == before {
                        return fail(sh, &format!("Harmonize did not start: {:?}", self.error_popup));
                    }
                    sh.job = Some(self.zeit.jobs.len() - 1);
                    self.zeit.show_tasks = true;
                    return None;
                }
                match self.dp_done(sh) {
                    0 => sh.next("Harmonize done"),
                    -1 => {}
                    _ => return fail(sh, &format!("Harmonize failed: {}", self.shots_job_error(sh))),
                }
            }
            64 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "harmonize", None, &[])?;
                    println!("    job.log:\n{}", cat(&run, "job.log"));
                    self.harmonize.show = false;
                    return None;
                }
                if !self.dp_series_open(sh.dataprep.layers + 1) {
                    return if t > 600.0 { fail(sh, "the harmonized series should open") } else { None };
                }
                for l in &self.layers {
                    println!("    layer {:?}", l.name);
                }
                // the same band as the two series it comes from
                let k = self.layers.len() - 1;
                let mut sel = self.layer_session(k)?.info.sel;
                (sel.band, sel.nd_band, sel.expr) = (HARM_BAND, 0, janus_cube::calc::ExprId::NONE);
                self.reopen_layer(k, sel, ctx);
                sh.next("the harmonized series open");
            }
            65 => {
                if self.opening.is_some() || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1)) {
                    return None;
                }
                sh.next("the harmonized series shown in the same band");
            }
            66 => {
                // the stable pixel on the three series: Sentinel-2, Landsat, both harmonized
                if sh.frames == 1 {
                    for (k, n) in
                        ["Sentinel-2 as it is", "Landsat 8-9 as it is", "Harmonized (both)"].iter().enumerate()
                    {
                        self.layers[k].name = (*n).into();
                    }
                }
                if self.dp_chart(sh, &[0, 1, 2], &[STABLE], &[("12-harmonize-chart", Shots::tab(Tab::TimeSeries))], ctx)
                {
                    go(sh, "the chart of the stable pixel on the three series", TOPO);
                }
            }
            // ---- the topographic correction ------------------------------------------------------------------------
            100 => {
                if sh.frames == 1 {
                    self.set_chart_layers(0);
                    self.clear_pins();
                    self.zeit.results.clear();
                    let path = folder(sh, &["topo", "series"]);
                    self.dp_open(path, false, 4, ctx);
                    return None;
                }
                if !self.dp_loaded() || self.layers.len() != 1 {
                    return None;
                }
                tabs(sh, 0.64);
                let info = self.session.as_ref()?.info.clone();
                println!("    topo series: {} dates, {}", info.t(), info.description);
                self.shots_colormap("Viridis");
                self.set_date(self.shots_date_of(WINTER));
                sh.next("the eight dates open");
            }
            101 => {
                if sh.frames == 20 {
                    self.map.fit();
                }
                if self.dp_window(sh, "topocorr", json!({}), "13-topocorr-window", f32::MAX) {
                    sh.next("the topographic correction's window");
                    sh.stage = TOPO + 4;
                }
            }
            104 => {
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "topocorr", 0, None, ctx) {
                    1 => return fail(sh, &format!("the correction did not start: {:?}", self.error_popup)),
                    0 => sh.next("the correction running"),
                    _ => {}
                }
            }
            105 => match self.dp_done(sh) {
                0 => sh.next("the correction done"),
                -1 => {}
                _ => return fail(sh, &format!("the correction failed: {}", self.shots_job_error(sh))),
            },
            106 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "topocorr", Some("topocorr"), &["topo_c.csv", "validation.csv"])?;
                    println!(
                        "    topo_c.csv:\n{}\n    validation.csv:\n{}\n    report.txt:\n{}",
                        cat(&run, "topo_c.csv"),
                        cat(&run, "validation.csv"),
                        cat(&run, "report.txt")
                    );
                    return None;
                }
                if !self.dp_series_open(sh.dataprep.layers + 1) {
                    return if t > 600.0 { fail(sh, "the corrected series should open") } else { None };
                }
                // the same band as the original: the NIR
                let k = self.layers.len() - 1;
                println!("    corrected layer {:?}", self.layers[k].name);
                let mut sel = self.layer_session(k)?.info.sel;
                (sel.band, sel.nd_band, sel.expr) = (4, 0, janus_cube::calc::ExprId::NONE);
                self.reopen_layer(k, sel, ctx);
                sh.next("the corrected series opens");
            }
            107 => {
                if self.opening.is_some() || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1)) {
                    return None;
                }
                sh.next("the corrected series shown in the NIR");
            }
            108 | 109 => {
                // the ridge before and after, the same date and the same range of values: parts of a figure
                let (k, part) =
                    if sh.stage == 108 { (0, "parts/14a-ridge-before") } else { (1, "parts/14b-ridge-after") };
                if sh.frames == 1 {
                    self.dp_show(&[k], k, &[], ctx);
                    self.shots_colormap("Viridis");
                    let date = self.shots_date_of(WINTER);
                    self.set_date(date);
                    tabs(sh, 0.64);
                    return None;
                }
                if sh.frames == 6 && sh.stage == 109 {
                    let m = janus_gpu::Mode::Value as usize;
                    let (lo, hi) = sh.dataprep.range;
                    self.display.ranges[m] = crate::display::Range::fixed(lo, hi);
                    if let Some(b) = self.display.band_ranges.get_mut(3) {
                        *b = crate::display::Range::fixed(lo, hi);
                    }
                    self.map.dirty = true;
                }
                if sh.frames == 20 {
                    self.shots_zoom_to(RIDGE.0, RIDGE.1, RIDGE.2);
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                if sh.stage == 108 {
                    let r = self.display.ranges[janus_gpu::Mode::Value as usize];
                    sh.dataprep.range = (r.lo, r.hi);
                    println!("    NIR range {} to {}", r.lo, r.hi);
                }
                sh.shoot("-", &[(part, Shots::tab(Tab::Map))]);
                sh.next(part);
            }
            110 => {
                // the sunlit and the shaded pixel, the series before and after, all the dates
                if sh.frames == 1 {
                    self.map.fit();
                    self.layers[0].name = "as observed".into();
                    self.layers[1].name = "corrected (SCS+C)".into();
                }
                if self.dp_chart(
                    sh,
                    &[0, 1],
                    &[SUNLIT, SHADED],
                    &[("15-topocorr-chart", Shots::tab(Tab::TimeSeries))],
                    ctx,
                ) {
                    go(sh, "the chart of the two slopes before and after", COREG);
                }
            }
            // ---- the co-registration -------------------------------------------------------------------------------
            140 => {
                if sh.frames == 1 {
                    self.set_chart_layers(0);
                    self.clear_pins();
                    self.zeit.results.clear();
                    self.shots_colormap("NDVI");
                    let sel = BandSelection { band: 4, nd_band: 3, ..Default::default() };
                    let path = folder(sh, &["mixed", "coreg"]);
                    self.error = None;
                    self.opening = Some(Opening::start_with(vec![path], false, sel, ctx));
                    return None;
                }
                if !self.dp_loaded() || self.layers.len() != 1 {
                    return None;
                }
                tabs(sh, 0.64);
                let info = self.session.as_ref()?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    coreg series: {} dates {labels:?}, bands {:?}, {}",
                    info.t(),
                    info.band_names,
                    info.crs_name
                );
                println!("    roles: {:?}", crate::zeit::guess_roles(&info));
                self.set_date(0);
                sh.next("the eight dates open");
            }
            141 => {
                if sh.frames == 20 {
                    self.map.fit();
                }
                // the window as it opens, then the run in global mode
                if sh.frames == 1 {
                    self.shots_open_tool("coregister", json!({}));
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 8 {
                    return None;
                }
                sh.next("the co-registration's window");
                sh.stage = COREG + 14;
            }
            154 => {
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "coregister", 0, None, ctx) {
                    1 => return fail(sh, &format!("the co-registration did not start: {:?}", self.error_popup)),
                    0 => sh.next("the co-registration (global) running"),
                    _ => {}
                }
            }
            155 => match self.dp_done(sh) {
                0 => sh.next("the co-registration (global) done"),
                -1 => {}
                _ => return fail(sh, &format!("the co-registration failed: {}", self.shots_job_error(sh))),
            },
            156 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "coreg_global", None, &["shifts.csv", "report.txt"])?;
                    println!(
                        "    shifts.csv:\n{}\n    report.txt:\n{}",
                        cat(&run, "shifts.csv"),
                        cat(&run, "report.txt")
                    );
                    // the window stays open, with the shifts listed
                    if let Some(t) = manifest().tool("coregister") {
                        self.set_tool_open(t, true);
                    }
                    return None;
                }
                if !self.dp_series_open(sh.dataprep.layers + 1) {
                    return if t > 600.0 { fail(sh, "the moved series should open") } else { None };
                }
                sh.next("the moved series open");
            }
            157 => {
                if sh.frames == 1 {
                    self.dp_show(&[0], 0, &[], ctx);
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(tool_tab("coregister"));
                }
                if sh.frames == 5 {
                    // the description is long and the shifts are at the foot: the window scrolled to its end
                    sh.scroll = Some((tool_tab("coregister"), f32::MAX));
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 30 || !self.shots_quiet() || t < 8.0 {
                    return None;
                }
                sh.shoot("-", &[("16-coreg-window", Shots::fit(tool_tab("coregister")))]);
                sh.next("the co-registration's window after the global run");
            }
            158 => {
                // the local mode: a tie point every 200 pixels; the arrows (shifts.gpkg) over the first date
                if sh.frames == 1 {
                    // the global run's layers and arrows out
                    for k in (1..self.layers.len()).rev() {
                        self.remove_layer(k, ctx);
                    }
                    for v in (0..self.vectors.len()).rev() {
                        self.remove_vector(v);
                    }
                    self.set_active(0, ctx);
                    self.shots_open_tool(
                        "coregister",
                        json!({ "mode": "local", "grid_res": 200, "arrow_scale": 30.0 }),
                    );
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 8 {
                    return None;
                }
                sh.next("the co-registration (local)");
                sh.stage = COREG + 24;
            }
            164 => {
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "coregister", 0, None, ctx) {
                    1 => return fail(sh, &format!("the local co-registration did not start: {:?}", self.error_popup)),
                    0 => sh.next("the co-registration (local) running"),
                    _ => {}
                }
            }
            165 => match self.dp_done(sh) {
                0 => sh.next("the co-registration (local) done"),
                -1 => {}
                _ => return fail(sh, &format!("the local co-registration failed: {}", self.shots_job_error(sh))),
            },
            166 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(
                        sh,
                        "coreg_local",
                        Some("coregister"),
                        &["shifts.csv", "tie_points.csv", "report.txt"],
                    )?;
                    println!(
                        "    shifts.csv:\n{}\n    report.txt:\n{}",
                        cat(&run, "shifts.csv"),
                        cat(&run, "report.txt")
                    );
                    return None;
                }
                if !self.dp_series_open(sh.dataprep.layers + 1) {
                    return if t > 600.0 { fail(sh, "the moved series should open") } else { None };
                }
                for v in &self.vectors {
                    println!("    vector layer {:?}: {}", v.name, v.data.kind_text());
                }
                sh.next("the moved series and the arrows open");
            }
            167 => {
                // the arrows over the first date of the series as it was
                if sh.frames == 1 {
                    self.dp_show(&[0], 0, &[], ctx);
                    for v in &mut self.vectors {
                        v.visible = v.data.path.ends_with("shifts.gpkg");
                    }
                    self.zeit.show_tasks = false;
                    self.set_date(0);
                    // the map as tall as the window allows, for the arrows
                    tabs(sh, 0.92);
                    return None;
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 8.0 {
                    return None;
                }
                sh.shoot("17-coreg-arrows", &[("map", Shots::tab(Tab::Map))]);
                go(sh, "the arrows of the local run", LEVEL2);
            }
            // ---- Level 1C to surface reflectance -----------------------------------------------------------------
            180 => {
                // the window with the scene's .SAFE folder and the DEM chosen, the defaults, the memory line
                let l1c = folder(sh, &["l1c"]);
                let safe = std::fs::read_dir(&l1c)
                    .ok()
                    .and_then(|d| {
                        d.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.to_string_lossy().ends_with(".SAFE"))
                    })
                    .map(|p| p.to_string_lossy().into_owned());
                let Some(safe) = safe else { return fail(sh, &format!("no .SAFE folder in {l1c}")) };
                let dem = folder(sh, &["l1c", "dem_23KQR.tif"]);
                if self.dp_window(sh, "level2", json!({ "input": safe, "dem": dem }), "02-level2-window", 0.0) {
                    sh.next("the Level 1C window");
                    sh.stage = LEVEL2 + 4;
                }
            }
            184 => {
                // JANUS_DATAPREP_L2_RUN=<a run's folder>: its products opened instead of a new run (to redo the pictures)
                if let Some(run) = std::env::var_os("JANUS_DATAPREP_L2_RUN") {
                    let scene = std::fs::read_dir(&run)
                        .ok()
                        .and_then(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.is_dir()));
                    let Some(scene) = scene else { return fail(sh, "no product folder in JANUS_DATAPREP_L2_RUN") };
                    sh.dataprep.layers = self.layers.len();
                    self.dp_open(scene.join("boa.tif").to_string_lossy().into_owned(), true, 1, ctx);
                    sh.dataprep.runs.push(("level2 (reused)".into(), scene.to_string_lossy().into_owned()));
                    sh.stage = LEVEL2 + 10;
                    return None;
                }
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "level2", 0, None, ctx) {
                    1 => return fail(sh, &format!("Level 1C did not start: {:?}", self.error_popup)),
                    0 => sh.next("Level 1C running"),
                    _ => {}
                }
            }
            185 => match self.dp_done(sh) {
                0 => sh.next("Level 1C done"),
                -1 => {}
                _ => return fail(sh, &format!("Level 1C failed: {}", self.shots_job_error(sh))),
            },
            186 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "level2", Some("level2"), &[])?;
                    println!(
                        "    job.log:
{}",
                        cat(&run, "job.log")
                    );
                    // the scene's folder holds attrs.json (how the correction went)
                    if let Ok(d) = std::fs::read_dir(&run) {
                        for e in d.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
                            println!(
                                "    attrs.json:
{}",
                                cat(&e.path().to_string_lossy(), "attrs.json")
                            );
                        }
                    }
                    return None;
                }
                // the surface reflectance and the quality layers open
                if !self.dp_series_open(sh.dataprep.layers + 2) {
                    return if t > 900.0 { fail(sh, "the two series should open") } else { None };
                }
                for l in &self.layers {
                    println!("    layer {:?}", l.name);
                }
                let (Some(boa), Some(qai)) = (self.dp_layer(&["boa"]), self.dp_layer(&["qai"])) else {
                    return fail(sh, "no layer named after boa.tif and qai.tif");
                };
                (sh.dataprep.boa, sh.dataprep.qai) = (boa, qai);
                if let Some(s) = self.layer_session(qai) {
                    println!("    QAI bands {:?}, {}", s.info.band_names, s.info.data_type);
                }
                if let Some(s) = self.layer_session(boa) {
                    println!(
                        "    BOA {} x {} px, bands {:?}, geotransform {:?}",
                        s.info.width, s.info.height, s.info.band_names, s.info.geo_transform
                    );
                }
                sh.next("the Level 1C result open");
            }
            190 => {
                if !self.dp_loaded() || self.layers.len() != sh.dataprep.layers + 1 {
                    return None;
                }
                let scene = sh.dataprep.runs.last().map(|r| r.1.clone()).unwrap_or_default();
                self.dp_open(Path::new(&scene).join("qai.tif").to_string_lossy().into_owned(), true, 1, ctx);
                sh.next("the reused surface reflectance open");
            }
            191 => {
                if sh.frames < 3 || !self.dp_series_open(sh.dataprep.layers + 2) {
                    return None;
                }
                let n = self.layers.len();
                (sh.dataprep.boa, sh.dataprep.qai) = (n - 2, n - 1);
                go(sh, "the reused quality open", LEVEL2 + 7);
            }
            187 => {
                // the whole scene in true colour (to choose the view of the figure)
                let k = sh.dataprep.boa;
                if sh.frames == 1 {
                    self.dp_show(&[k], k, &[], ctx);
                    tabs(sh, 0.92);
                    return None;
                }
                if sh.frames == 5 {
                    let id = self.session.as_ref()?.info.id;
                    self.composite = Some(crate::composite::BandComposite::of(id, [3, 2, 1]));
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 60 || !self.shots_quiet() || t < 15.0 {
                    return None;
                }
                sh.shoot("-", &[("parts/03-level2-whole", Shots::tab(Tab::Map))]);
                sh.next("the whole scene in true colour");
            }
            188 | 189 => {
                // the view of the figure, in true colour (188) and the quality classes (189): parts of a figure
                let (k, part) = if sh.stage == 188 {
                    (sh.dataprep.boa, "parts/03a-level2-boa")
                } else {
                    (sh.dataprep.qai, "parts/03b-level2-qai")
                };
                if sh.frames == 1 {
                    self.dp_show(&[k], k, &[], ctx);
                    tabs(sh, 0.92);
                    if sh.stage == 189 {
                        // the quality flags as classes by the calculator: 0 clear, 1 cloud buffer, 2 cloud, 3 shadow,
                        // 4 water, 5 no data
                        let mut sel = self.layer_session(k)?.info.sel;
                        sel.expr = janus_cube::calc::ExprId::intern(
                            "QAI classes",
                            "where(bit(band1, 0) > 0, 5, where(bits(band1, 1, 2) >= 2, 2, where(bit(band1, 3) > 0, 3, where(bits(band1, 1, 2) == 1, 1, where(bit(band1, 5) > 0, 4, 0)))))",
                        );
                        self.reopen_layer(k, sel, ctx);
                        self.composite = None;
                    }
                    return None;
                }
                if sh.stage == 188 && sh.frames == 5 {
                    let id = self.session.as_ref()?.info.id;
                    self.composite = Some(crate::composite::BandComposite::of(id, [3, 2, 1]));
                }
                if sh.frames == 20 {
                    self.shots_zoom_to(VIEW_L2.0, VIEW_L2.1, VIEW_L2.2);
                }
                if sh.stage == 189 && sh.frames == 30 {
                    // the classes' names and colours (tools/make_dataprep_figures.py draws the same legend)
                    let style: Vec<crate::classes::ClassEntry> = QAI_CLASSES
                        .iter()
                        .map(|&(value, name, color)| crate::classes::ClassEntry {
                            value,
                            name: name.into(),
                            color,
                            visible: true,
                            count: 0,
                        })
                        .collect();
                    self.classes.apply_style(Some(&style));
                    self.map.dirty = true;
                }
                if sh.frames < 60 || !self.shots_quiet() || t < 15.0 {
                    return None;
                }
                sh.shoot("-", &[(part, Shots::tab(Tab::Map))]);
                if sh.stage == 189 {
                    go(sh, part, CLOUDY);
                } else {
                    sh.next(part);
                }
            }
            // ---- the cloudy series: Tmask, the clear-sky observations, Interpolate, Smoothing, Standardize ----------
            200 => {
                if sh.frames == 1 {
                    self.set_chart_layers(0);
                    self.clear_pins();
                    self.zeit.results.clear();
                    self.map.dirty = true;
                    for v in (0..self.vectors.len()).rev() {
                        self.remove_vector(v);
                    }
                    self.composite = None;
                    self.shots_colormap("NDVI");
                    let sel = BandSelection { band: 4, nd_band: 3, ..Default::default() };
                    let path = folder(sh, &["cloudy"]);
                    self.error = None;
                    self.opening = Some(Opening::start_with(vec![path], false, sel, ctx));
                    return None;
                }
                if !self.dp_loaded() || self.layers.len() != 1 {
                    return None;
                }
                tabs(sh, 0.64);
                let info = self.session.as_ref()?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    cloudy series: {} x {} px, {} dates {} to {}, bands {:?}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    labels.first().unwrap_or(&""),
                    labels.last().unwrap_or(&""),
                    info.band_names,
                    info.crs_name
                );
                println!("    roles: {:?}", crate::zeit::guess_roles(&info));
                self.layers[0].name = "Landsat NDVI".into();
                self.set_date(self.shots_date_of("2023-07"));
                sh.next("the cloudy series open");
            }
            201 => {
                // Tmask: the window with the bands mapped, the run on the ROI
                if sh.frames == 20 {
                    self.map.fit();
                }
                if self.dp_window(sh, "tmask", json!({}), "04-tmask-window", 0.0) {
                    sh.next("the Tmask window");
                }
            }
            202 => {
                // the pin's series with the markers of the flagged dates, and the statistics
                let files =
                    [("05-tmask-chart", Shots::tab(Tab::TimeSeries)), ("05-tmask-table", Shots::tab(Tab::Statistics))];
                if self.dp_chart(sh, &[0], &[TMASK_PIN], &files, ctx) {
                    go(sh, "the Tmask chart", CLOUDY + 24);
                }
            }
            224 => {
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "tmask", 0, None, ctx) {
                    1 => return fail(sh, &format!("Tmask did not start: {:?}", self.error_popup)),
                    0 => sh.next("Tmask running"),
                    _ => {}
                }
            }
            225 => match self.dp_done(sh) {
                0 => sh.next("Tmask done"),
                -1 => {}
                _ => return fail(sh, &format!("Tmask failed: {}", self.shots_job_error(sh))),
            },
            226 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "tmask", Some("tmask"), &[])?;
                    println!("    job.log:\n{}", cat(&run, "job.log"));
                    return None;
                }
                // the screened series opens as a layer, the maps load
                if !self.dp_series_open(sh.dataprep.layers + 1) || self.zeit.loads_pending() {
                    return if t > 900.0 { fail(sh, "the screened series should open") } else { None };
                }
                for l in &self.layers {
                    println!("    layer {:?}", l.name);
                }
                self.layers[1].name = "Screened series".into();
                sh.next("the screened series open");
            }
            227 => {
                // the share of the observations flagged, over the ROI
                if sh.frames == 1 {
                    self.dp_show(&[], 0, &[], ctx);
                    tabs(sh, 0.64);
                    if !self.shots_show_result(sh, "Share flagged") {
                        return fail(sh, "no 'Share flagged' map");
                    }
                    return None;
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("-", &[("parts/06a-tmask-share", Shots::tab(Tab::Map))]);
                sh.next("the share flagged");
            }
            228 | 229 => {
                // a cloudy date of the screened series (its flagged observations empty) and of the original
                let (k, part) =
                    if sh.stage == 228 { (1, "parts/06b-tmask-screened") } else { (0, "parts/06c-tmask-original") };
                if sh.frames == 1 {
                    self.shots_hide_results();
                    self.dp_show(&[k], k, &[], ctx);
                    self.shots_colormap("NDVI");
                    let date = self.shots_date_of(CLOUDY_DATE);
                    self.set_date(date);
                    return None;
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("-", &[(part, Shots::tab(Tab::Map))]);
                if sh.stage == 228 {
                    sh.next(part);
                } else {
                    // the screened series out again: the next tools read the original
                    self.remove_layer(1, ctx);
                    self.zeit.results.clear();
                    go(sh, part, CLOUDY + 30);
                }
            }
            // ---- the clear-sky observations, a month of the wet season and one of the dry --------------------------
            230 => {
                if sh.frames == 1 {
                    self.clear_pins();
                    self.dp_show(&[0], 0, &[], ctx);
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 5 {
                    return None;
                }
                sh.next("the original series shown");
                sh.stage = CLOUDY + 34;
            }
            234 => {
                sh.dataprep.layers = self.layers.len();
                // the whole image, one month a window, the count of the clear observations
                self.shots_set_params("clear_observations", json!({ "months": 1, "statistics": ["count"] }));
                match self.shots_run_stage(sh, "clear_observations", 0, None, ctx) {
                    1 => return fail(sh, &format!("the clear-sky observations did not start: {:?}", self.error_popup)),
                    0 => sh.next("the clear-sky observations running"),
                    _ => {}
                }
            }
            235 => match self.dp_done(sh) {
                0 => sh.next("the clear-sky observations done"),
                -1 => {}
                _ => return fail(sh, &format!("the clear-sky observations failed: {}", self.shots_job_error(sh))),
            },
            236 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "cso", Some("clear_observations"), &[])?;
                    println!("    job.log:\n{}", cat(&run, "job.log"));
                    return None;
                }
                if !self.dp_series_open(sh.dataprep.layers + 1) {
                    return if t > 900.0 { fail(sh, "the count should open") } else { None };
                }
                for l in &self.layers {
                    println!("    layer {:?}", l.name);
                }
                let k = self.layers.len() - 1;
                sh.dataprep.cso = k;
                let info = self.layer_session(k)?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    count layer: {} windows {} to {}, {}",
                    info.t(),
                    labels[0],
                    labels[labels.len() - 1],
                    info.data_type
                );
                sh.next("the count opens");
            }
            237 | 238 => {
                // the count of a wet month and of a dry month, the same range of values: parts of a figure
                let (month, part) =
                    if sh.stage == 237 { (WET_MONTH, "parts/07a-cso-wet") } else { (DRY_MONTH, "parts/07b-cso-dry") };
                let k = sh.dataprep.cso;
                if sh.frames == 1 {
                    self.dp_show(&[k], k, &[], ctx);
                    tabs(sh, 0.64);
                    self.shots_colormap("Viridis");
                    let date = self.shots_date_of(month);
                    self.set_date(date);
                    return None;
                }
                if sh.frames == 4 {
                    // whole numbers are classes to the app; a count is read as a quantity here
                    self.classes.state = crate::classes::ClassState::Off;
                    self.map.dirty = true;
                }
                if sh.frames == 6 {
                    let m = janus_gpu::Mode::Value as usize;
                    self.display.ranges[m] = crate::display::Range::fixed(0.0, CSO_MAX);
                    if let Some(b) = self.display.band_ranges.get_mut(0) {
                        *b = crate::display::Range::fixed(0.0, CSO_MAX);
                    }
                    self.map.dirty = true;
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("-", &[(part, Shots::tab(Tab::Map))]);
                if sh.stage == 237 {
                    sh.next(part);
                } else {
                    go(sh, part, CLOUDY + 40);
                }
            }
            // ---- Interpolate, Smoothing, Standardize: the windows and the charts of the pins ---------------------------
            240 => {
                let id = "interpolate";
                let files = [
                    ("18-interpolate-window", Shots::fit(tool_tab(id))),
                    ("18-interpolate-chart", Shots::tab(Tab::TimeSeries)),
                    ("18-interpolate-table", Shots::tab(Tab::Statistics)),
                ];
                let params = json!({ "method": "rbf", "step": 16 });
                if self.dp_tool_chart(sh, id, Some(params), 300.0, &[GAPS_PIN], &files, ctx) {
                    go(sh, "Interpolate's window and chart", CLOUDY + 45);
                }
            }
            245 | 246 => {
                // Whittaker at lambda 10, then Savitzky-Golay over the same pixel
                let id = "smooth";
                let whittaker = sh.stage == 245;
                if sh.frames == 1 && whittaker {
                    // (a window is closed in the frame after its picture is taken, not in it)
                    self.dp_close("interpolate");
                }
                let files = [
                    (if whittaker { "19-smooth-window" } else { "19-smooth-window-sg" }, Shots::fit(tool_tab(id))),
                    (if whittaker { "19-smooth-whittaker" } else { "19-smooth-savgol" }, Shots::tab(Tab::TimeSeries)),
                    (if whittaker { "19-smooth-table" } else { "19-smooth-table-sg" }, Shots::tab(Tab::Statistics)),
                ];
                let params = if whittaker {
                    Some(json!({ "method": "whittaker", "lmbda": 10.0 }))
                } else {
                    if sh.frames == 1 {
                        self.shots_set_params(id, json!({ "method": "savgol", "window": 7, "polyorder": 2 }));
                    }
                    None
                };
                if self.dp_tool_chart(sh, id, params, 300.0, &[FOREST_PIN], &files, ctx) {
                    if whittaker {
                        sh.next("Whittaker's window and chart");
                    } else {
                        go(sh, "Savitzky-Golay's chart", CLOUDY + 50);
                    }
                }
            }
            250 => {
                let id = "standardize";
                if sh.frames == 1 {
                    self.dp_close("smooth");
                }
                let files = [
                    ("20-standardize-window", Shots::fit(tool_tab(id))),
                    ("20-standardize-chart", Shots::tab(Tab::TimeSeries)),
                    ("20-standardize-table", Shots::tab(Tab::Statistics)),
                ];
                if self.dp_tool_chart(sh, id, Some(json!({})), 0.0, &[FOREST_PIN, PASTURE_PIN], &files, ctx) {
                    go(sh, "Standardize's window and chart", IMPROPHE);
                }
            }
            // ---- ImproPhe -------------------------------------------------------------------------------------------
            260 => {
                if sh.frames == 1 {
                    self.dp_close("standardize");
                    self.set_chart_layers(0);
                    self.clear_pins();
                    self.zeit.results.clear();
                    self.map.dirty = true;
                    self.shots_colormap("Viridis");
                    let sel = BandSelection { band: 4, nd_band: 3, ..Default::default() };
                    let path = folder(sh, &["improphe", "fine"]);
                    self.error = None;
                    self.opening = Some(Opening::start_with(vec![path], false, sel, ctx));
                    return None;
                }
                if !self.dp_loaded() || self.layers.len() != 1 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    fine series: {} x {} px, {} dates {} to {}, bands {:?}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    labels.first().unwrap_or(&""),
                    labels.last().unwrap_or(&""),
                    info.band_names,
                    info.crs_name
                );
                self.layers[0].name = "Sentinel-2 10 m".into();
                sh.next("the Sentinel-2 series open");
            }
            261 => {
                if sh.frames == 1 {
                    let path = folder(sh, &["improphe", "coarse"]);
                    self.dp_open(path, true, 1, ctx);
                    return None;
                }
                if !self.dp_loaded() || self.layers.len() != 2 {
                    return None;
                }
                let info = self.layer_session(1)?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    coarse series: {} x {} px, {} dates {labels:?}, bands {:?}, geotransform {:?}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    info.band_names,
                    info.geo_transform,
                    info.crs_name
                );
                self.layers[1].name = "MODIS 250 m NDVI (mean of the year)".into();
                sh.next("the MODIS series open");
            }
            262 => {
                // the window: the Sentinel-2 series active, the MODIS one picked as the coarse series
                if sh.frames == 1 {
                    self.dp_show(&[0], 0, &[], ctx);
                    tabs(sh, 0.64);
                    let coarse = json!({ "name": self.layers[1].name, "inputs": self.layers[1].inputs });
                    self.shots_open_tool(
                        "improphe",
                        json!({ "coarse": coarse, "by": "year", "kernel_size": IMPROPHE_RADIUS }),
                    );
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(tool_tab("improphe"));
                }
                if sh.frames == 5 {
                    sh.scroll = Some((tool_tab("improphe"), f32::MAX));
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 30 || !self.shots_quiet() || t < 8.0 {
                    return None;
                }
                sh.shoot("-", &[("21-improphe-window", Shots::fit(tool_tab("improphe")))]);
                sh.next("the ImproPhe window");
                sh.stage = IMPROPHE + 4;
            }
            264 => {
                sh.dataprep.layers = self.layers.len();
                match self.shots_run_stage(sh, "improphe", 0, None, ctx) {
                    1 => return fail(sh, &format!("ImproPhe did not start: {:?}", self.error_popup)),
                    0 => sh.next("ImproPhe running"),
                    _ => {}
                }
            }
            265 => match self.dp_done(sh) {
                0 => sh.next("ImproPhe done"),
                -1 => {}
                _ => return fail(sh, &format!("ImproPhe failed: {}", self.shots_job_error(sh))),
            },
            266 => {
                if sh.frames == 1 {
                    let run = self.dp_finish(sh, "improphe", Some("improphe"), &[])?;
                    println!("    job.log:\n{}", cat(&run, "job.log"));
                    return None;
                }
                if !self.dp_series_open(sh.dataprep.layers + 1) {
                    return if t > 900.0 { fail(sh, "the ImproPhe series should open") } else { None };
                }
                let k = self.layers.len() - 1;
                let info = self.layer_session(k)?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    ImproPhe layer {:?}: {} x {} px, {} dates {labels:?}, bands {:?}",
                    self.layers[k].name,
                    info.width,
                    info.height,
                    info.t(),
                    info.band_names
                );
                self.layers[k].name = "ImproPhe".into();
                sh.next("the ImproPhe series open");
            }
            267 | 268 => {
                // the coarse layer and the result of the same year, the same range of values: parts of a figure
                let (k, part) =
                    if sh.stage == 267 { (1, "parts/22a-improphe-coarse") } else { (2, "parts/22b-improphe-result") };
                if sh.frames == 1 {
                    self.dp_show(&[k], k, &[], ctx);
                    tabs(sh, 0.64);
                    self.shots_colormap("Viridis");
                    let date = self.shots_date_of("2023");
                    self.set_date(date);
                    return None;
                }
                if sh.frames == 6 && sh.stage == 268 {
                    let m = janus_gpu::Mode::Value as usize;
                    let (lo, hi) = sh.dataprep.range;
                    self.display.ranges[m] = crate::display::Range::fixed(lo, hi);
                    if let Some(b) = self.display.band_ranges.get_mut(0) {
                        *b = crate::display::Range::fixed(lo, hi);
                    }
                    self.map.dirty = true;
                }
                if sh.frames == 20 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                if sh.stage == 267 {
                    let r = self.display.ranges[janus_gpu::Mode::Value as usize];
                    sh.dataprep.range = (r.lo, r.hi);
                    println!("    coarse range {} to {}", r.lo, r.hi);
                }
                sh.shoot("-", &[(part, Shots::tab(Tab::Map))]);
                if sh.stage == 267 {
                    sh.next(part);
                } else {
                    go(sh, part, END);
                }
            }
            _ => {
                // (the stages of the page's other figures go before this arm, from OTHERS to END)
                println!("    runs: {:?}", sh.dataprep.runs);
                println!("OK");
                return Some(0);
            }
        }
        None
    }
}
