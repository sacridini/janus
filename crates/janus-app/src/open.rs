//! Opening beyond local files (src/app_open.cpp): File > Open URL (cloud-optimized GeoTIFFs and other rasters over
//! http, S3, GCS, Azure, read by GDAL's network file systems: only the header, the overview level the map needs and
//! the blocks looked at are downloaded), the variable picker of NetCDF / Zarr / HDF files of several variables, and
//! the Display panel's variable and level of such a layer (each change reopens the layer in place, like a band
//! selection).

use std::sync::mpsc::Receiver;

use egui::Ui;
use janus_cube::multidim::MdVariable;
use janus_cube::{BandSelection, CubeInfo};

use crate::app::State;

/// What an open in the background gives.
pub enum OpenResult {
    /// the series, and what was opened (a container's only variable replaces the file)
    Opened(Box<CubeInfo>, Vec<String>),
    /// a container of several variables: one is picked first
    Variables(Vec<MdVariable>),
    Failed(String),
}

/// The open itself (off the interface's thread).
pub fn open(inputs: Vec<String>, sel: &BandSelection) -> OpenResult {
    let mut inputs = inputs;
    if inputs.len() == 1 {
        let vars = janus_cube::multidim::list_variables(&inputs[0]);
        if vars.len() > 1 {
            return OpenResult::Variables(vars);
        }
        if let Some(v) = vars.into_iter().next() {
            inputs = vec![v.dataset];
        }
    }
    match janus_cube::open_cube(&inputs, sel) {
        Ok(info) => OpenResult::Opened(Box::new(info), inputs),
        Err(e) => OpenResult::Failed(e),
    }
}

#[derive(Default)]
pub struct OpenUi {
    /// File > Open URL
    pub url_open: bool,
    url_text: String,
    url_add: bool,
    /// the variable picker: the container's variables, which one, added as a layer
    pub var_choices: Vec<MdVariable>,
    var_file: String,
    var_add: bool,
    var_pick: usize,
    /// the Display panel: the active container's variables, listed in the background
    md_vars_for: String,
    md_vars: Vec<MdVariable>,
    md_vars_rx: Option<Receiver<Vec<MdVariable>>>,
}

impl State {
    /// File > Open URL (Ctrl+U); `add`: as a layer.
    pub fn open_url_dialog(&mut self, add: bool) {
        self.open_ui.url_add = add && self.session.is_some();
        self.open_ui.url_open = true;
    }

    /// A container of several variables was opened: which one, asked.
    pub(crate) fn ask_variable(&mut self, vars: Vec<MdVariable>, file: String, add: bool) {
        let o = &mut self.open_ui;
        o.var_choices = vars;
        o.var_file = file;
        o.var_add = add;
        o.var_pick = 0;
    }

    /// The Open URL and variable dialogs, when asked.
    pub(crate) fn open_dialogs(&mut self, ctx: &egui::Context) {
        let font = ctx.style_of(ctx.theme()).text_styles[&egui::TextStyle::Body].size;
        if self.open_ui.url_open {
            let mut open_now: Option<(Vec<String>, bool)> = None;
            let mut close = false;
            let has_series = self.session.is_some();
            let busy = self.opening.is_some();
            let o = &mut self.open_ui;
            let modal = egui::Modal::new(egui::Id::new("open url")).show(ctx, |ui| {
                ui.set_width(font * 46.0 * 0.62);
                ui.heading("Open URL");
                ui.add(
                    egui::Label::new(
                        "Rasters on the web, opened in place: of a cloud-optimized GeoTIFF only the header, the \
                         overview level the map needs and the blocks you look at are downloaded.",
                    )
                    .wrap(),
                );
                ui.weak("One URL per line; several make a series (dates from the file names).");
                let r = ui.add(egui::TextEdit::multiline(&mut o.url_text).desired_rows(7).desired_width(f32::INFINITY));
                if ui.memory(|m| m.focused().is_none()) {
                    r.request_focus();
                }
                ui.weak("https://...   s3://bucket/key.tif   gs://bucket/key.tif   az://container/key.tif").on_hover_text(
                    "Public S3 and GCS buckets are read without an account. Private ones use the\nusual credentials \
                     (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or ~/.aws,\nGOOGLE_APPLICATION_CREDENTIALS, \
                     AZURE_STORAGE_CONNECTION_STRING...),\nor a signed URL (e.g. from Planetary Computer). NetCDF and \
                     Zarr open too.",
                );
                if ui
                    .small_button("Example: a Sentinel-2 band on AWS")
                    .on_hover_text(
                        "Band 4 (red) of a Sentinel-2 scene of Element 84's Earth Search COGs:\n10980 x 10980 pixels, \
                         ~200 MB, opened in a second or two",
                    )
                    .clicked()
                {
                    o.url_text = "https://sentinel-cogs.s3.us-west-2.amazonaws.com/sentinel-s2-l2a-cogs/36/Q/WD/2020/7/\
                                  S2A_36QWD_20200701_0_L2A/B04.tif"
                        .into();
                }
                let urls: Vec<String> = o
                    .url_text
                    .lines()
                    .map(|l| l.trim_matches(|c: char| matches!(c, ' ' | '\t' | '"' | '\'')).to_string())
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .collect();
                ui.add_enabled(has_series, egui::Checkbox::new(&mut o.url_add, "Add as a layer"));
                ui.horizontal(|ui| {
                    let enter = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                    let can = !urls.is_empty() && !busy;
                    let b = ui.add_enabled(can, egui::Button::new("Open").min_size(egui::vec2(font * 6.0 * 0.62, 0.0)));
                    if (b.on_hover_text("Ctrl+Enter").clicked() || enter) && can {
                        open_now = Some((urls.clone(), o.url_add && has_series));
                        close = true;
                    }
                    if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(font * 6.0 * 0.62, 0.0))).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        close = true;
                    }
                });
            });
            if close || modal.should_close() {
                self.open_ui.url_open = false;
            }
            if let Some((urls, add)) = open_now {
                self.open_inputs(urls, add, ctx);
            }
        }
        if !self.open_ui.var_choices.is_empty() {
            let mut pick: Option<(String, bool)> = None;
            let mut close = false;
            let busy = self.opening.is_some();
            let o = &mut self.open_ui;
            let modal = egui::Modal::new(egui::Id::new("choose a variable")).show(ctx, |ui| {
                ui.set_width(font * 40.0 * 0.62);
                ui.heading("Choose a variable");
                ui.label(format!("{} holds {} variables:", janus_cube::display_name(&o.var_file), o.var_choices.len()));
                let mut open = false;
                egui::ScrollArea::vertical().max_height(font * 12.0 * 1.2).show(ui, |ui| {
                    for (i, v) in o.var_choices.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let r = ui.selectable_label(o.var_pick == i, &v.name);
                            if r.clicked() {
                                o.var_pick = i;
                            }
                            if r.double_clicked() {
                                o.var_pick = i;
                                open = true;
                            }
                            ui.weak(&v.desc);
                        });
                    }
                });
                ui.weak("Another variable can be picked later in the Display panel.");
                ui.horizontal(|ui| {
                    open |= ui.add_enabled(!busy, egui::Button::new("Open")).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
                if open && !busy && o.var_pick < o.var_choices.len() {
                    pick = Some((o.var_choices[o.var_pick].dataset.clone(), o.var_add));
                    close = true;
                }
            });
            if close || modal.should_close() {
                self.open_ui.var_choices.clear();
            }
            if let Some((dataset, add)) = pick {
                self.open_inputs(vec![dataset], add, ctx);
            }
        }
    }

    /// The variables of the active layer's container, listed in the background (a remote file needs requests).
    pub(crate) fn list_container_variables(&mut self, ctx: &egui::Context, info: &CubeInfo) {
        let container = janus_cube::multidim::container_of(&info.first_path);
        let o = &mut self.open_ui;
        if o.md_vars_for != container && o.md_vars_rx.is_none() {
            o.md_vars_for = container.clone();
            o.md_vars.clear();
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(janus_cube::multidim::list_variables(&container));
                ctx.request_repaint();
            });
            o.md_vars_rx = Some(rx);
        }
        if let Some(rx) = &o.md_vars_rx
            && let Ok(v) = rx.try_recv()
        {
            o.md_vars = v;
            o.md_vars_rx = None;
        }
    }

    /// The Display panel's variable and level of a NetCDF / Zarr layer: another one reopens it in place.
    pub(crate) fn multidim_ui(&mut self, ui: &mut Ui, info: &CubeInfo) {
        self.list_container_variables(ui.ctx(), info);
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong("Variable");
            ui.add(egui::Separator::default().horizontal());
        });
        let busy = self.opening.is_some();
        let mut reopen: Option<(Vec<String>, BandSelection)> = None;
        ui.add_enabled_ui(!busy, |ui| {
            if self.open_ui.md_vars.len() > 1 {
                egui::ComboBox::from_id_salt("variable")
                    .width(ui.available_width())
                    .selected_text(&info.md_variable)
                    .show_ui(ui, |ui| {
                        for v in &self.open_ui.md_vars {
                            if ui.selectable_label(v.name == info.md_variable, &v.name).on_hover_text(&v.desc).clicked()
                                && v.name != info.md_variable
                            {
                                reopen = Some((vec![v.dataset.clone()], BandSelection::default()));
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "The file's variables: another one reopens the layer in place\n(same name in the Layers \
                         panel, date, pins and view kept)",
                    );
            } else {
                ui.label(if info.md_variable.is_empty() {
                    janus_cube::display_name(&info.first_path)
                } else {
                    info.md_variable.clone()
                });
            }
            if !info.md_levels.is_empty() {
                let cur = (info.sel.band.max(1) as usize).min(info.md_levels.len()) - 1;
                egui::ComboBox::from_id_salt("level")
                    .width(ui.available_width())
                    .selected_text(&info.md_levels[cur])
                    .show_ui(ui, |ui| {
                        for (k, l) in info.md_levels.iter().enumerate() {
                            if ui.selectable_label(k == cur, l).clicked() && k != cur {
                                let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
                                reopen = Some((inputs, BandSelection { band: k as i32 + 1, ..Default::default() }));
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "The variable has more dimensions than time (e.g. pressure level, depth,\nensemble member): \
                         the series shows one value of each",
                    );
            }
        });
        ui.weak(&info.description);
        if let Some((inputs, sel)) = reopen {
            let ctx = ui.ctx().clone();
            self.reopen_layer_inputs(self.active, inputs, BandSelection { chosen: true, ..sel }, &ctx);
        }
    }
}

/// `--selftest-multidim H/clim.nc` (app_open.cpp, --selftest-ui stages 160-165): a NetCDF of two variables (written
/// by tools/make_selftest_data.py). Its variables are asked; "ta" (time, level, lat, lon) opens as 12 noleap months
/// at 2 levels, drawn as a composite with a season; the other level, then the other variable, replace it in place;
/// the Open URL dialog opens.
#[derive(Default)]
pub struct MultidimTest {
    stage: usize,
    t0: Option<std::time::Instant>,
}

impl State {
    pub fn multidim_selftest_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        let mut st = std::mem::take(&mut self.multidim_test);
        let r = self.multidim_selftest_stage(&mut st, ctx);
        self.multidim_test = st;
        r
    }

    fn multidim_selftest_stage(&mut self, st: &mut MultidimTest, ctx: &egui::Context) -> Option<i32> {
        let t0 = *st.t0.get_or_insert_with(std::time::Instant::now);
        let fail = |what: &str| {
            println!("FAIL: {what}");
            Some(1)
        };
        let next = |st: &mut MultidimTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
        };
        if t0.elapsed().as_secs_f64() > 60.0 {
            return fail("timeout");
        }
        // ta at (x, y), date t, level k: 100 k + t + 0.01 y; t2m: 10 + t + 0.1 x
        let value = |s: &Option<crate::session::Session>, t: usize, x: usize, y: usize| {
            s.as_ref().and_then(|s| janus_cube::CubeReader::new(s.info.clone()).read_pixel(t, x, y)).unwrap_or(f32::NAN)
        };
        let loaded = |s: &State| {
            s.opening.is_none() && s.session.as_ref().is_some_and(|s| s.s.overview.complete() && s.stats.is_some())
        };
        match st.stage {
            0 => {
                if self.opening.is_some() || self.open_ui.var_choices.is_empty() {
                    if self.opening.is_none() && !self.layers.is_empty() {
                        return fail("a file of several variables should ask which one");
                    }
                    if self.opening.is_none() && self.error.is_some() {
                        return fail(&format!("could not open: {}", self.error.clone().unwrap_or_default()));
                    }
                    return None;
                }
                let vars = &self.open_ui.var_choices;
                if vars.len() != 2 {
                    return fail("H/clim.nc has two variables");
                }
                for v in vars {
                    println!("    variable {}: {}", v.name, v.desc);
                }
                let Some(ta) = vars.iter().find(|v| v.name == "ta").filter(|v| v.desc.contains("air temperature"))
                else {
                    return fail("ta and its long name should be listed");
                };
                let ta = ta.dataset.clone();
                self.open_ui.var_choices.clear(); // the picker, as its Open button does
                self.open_inputs(vec![ta], false, ctx);
                next(st, "variables listed with their long names; ta picked");
            }
            1 => {
                if !loaded(self) {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                println!(
                    "    {}; {} .. {}; levels: {:?}; CRS {}",
                    info.description,
                    info.layers[0].label,
                    info.layers[info.t() - 1].label,
                    info.md_levels,
                    info.crs_authority
                );
                if !info.multidim
                    || info.md_variable != "ta"
                    || info.t() != 12
                    || info.md_levels.len() != 2
                    || !info.time_is_date
                {
                    return fail("ta should open as 12 dates at 2 levels");
                }
                // noleap: day 59 is March 1st (in the real 2020, a leap year, it would be February 29th)
                if info.layers[2].label != "2020-03"
                    || info.layers[11].label != "2020-12"
                    || info.granularity != janus_cube::TimeGranularity::Month
                {
                    return fail("the CF dates (days since 2020-01-01, noleap) should be the months of 2020");
                }
                if info.crs_authority != "EPSG:4326" {
                    return fail("a lon/lat grid without a grid mapping should be WGS 84");
                }
                if (value(&self.session, 3, 4, 7) - (3.0 + 0.07)).abs() > 1e-4 {
                    return fail("level 1 of ta: wrong values");
                }
                if self.layers[0].name != "clim ta" {
                    return fail(&format!(
                        "the layer should be named after the file and the variable ({})",
                        self.layers[0].name
                    ));
                }
                self.display.mode = janus_gpu::Mode::Composite; // drawn by the next frames, with the season
                self.display.season = crate::season::Season { on: true, start: 60, end: 151 };
                self.reopen_layer(self.active, BandSelection { band: 2, chosen: true, ..Default::default() }, ctx);
                next(st, "ta: noleap months, level 1000 values; composite and season drawn; level 850 asked");
            }
            2 => {
                if !loaded(self) || self.session.as_ref()?.info.sel.band != 2 {
                    return None;
                }
                if (value(&self.session, 3, 4, 7) - (100.0 + 3.0 + 0.07)).abs() > 1e-4 {
                    return fail("level 2 of ta: wrong values");
                }
                if self.layers.len() != 1 || self.layers[0].name != "clim ta" {
                    return fail("the level should change in place");
                }
                // the Display panel lists the file's variables in the background
                let info = self.session.as_ref()?.info.clone();
                if self.open_ui.md_vars.is_empty() {
                    self.list_container_variables(ctx, &info);
                    return None;
                }
                let Some(t2m) = self.open_ui.md_vars.iter().find(|v| v.name == "t2m").map(|v| v.dataset.clone()) else {
                    return fail("the Display panel should list the file's variables");
                };
                self.reopen_layer_inputs(self.active, vec![t2m], BandSelection::default(), ctx);
                next(st, "level 850 in place; the other variable asked from the Display panel's list");
            }
            3 => {
                if !loaded(self) || self.session.as_ref()?.info.md_variable != "t2m" {
                    return None;
                }
                if (value(&self.session, 5, 9, 3) - (10.0 + 5.0 + 0.9)).abs() > 1e-4 {
                    return fail("t2m: wrong values");
                }
                if self.layers.len() != 1
                    || self.layers[0].name != "clim t2m"
                    || !self.session.as_ref()?.info.md_levels.is_empty()
                {
                    return fail("t2m should replace ta in place, named after it, without levels");
                }
                self.display.mode = janus_gpu::Mode::Value;
                self.display.season = Default::default();
                self.open_url_dialog(false);
                next(st, "t2m in place of ta; the Open URL dialog asked");
            }
            4 => {
                if !self.open_ui.url_open {
                    return fail("the Open URL dialog should open");
                }
                self.open_ui.url_open = false;
                next(st, "the Open URL dialog opened");
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                st.stage = 5;
                return Some(0);
            }
            _ => {}
        }
        None
    }
}
