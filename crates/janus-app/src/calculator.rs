//! Tools > Raster calculator (janus_cube::calc): an expression over the open layers, each recognised by a short
//! name (`s2.nir`, `dem`), a series and an image together (the image at every date), two series matched by date,
//! layers of other grids resampled or reprojected onto the result's, reductions over time (`max(ndvi)`), rules
//! (`ndvi < 0.2 = 0`). The editor colours the text, numbers its lines, completes names (Tab, Enter), explains
//! what is under the pointer and points at errors as they are typed; Ctrl+Enter or Run makes the result: a layer
//! of VRTs computed as the map shows them, or cloud-optimized GeoTIFFs written once (Tools > Tasks). The texts run
//! are kept (History), the favourites starred.

use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{Color32, FontId, Key, Modifiers, RichText, Stroke, TextFormat, Ui, vec2};
use janus_cube::calc::editor::{self, Class, Completion};
use janus_cube::calc::funcs::{CONSTANTS, DATE_VARS, FUNCS, Group};
use janus_cube::calc::plan::{self, CalcLayer, DateMatch, LayerKind, Options, Placement, Plan};
use janus_cube::calc::{CalcError, indices};
use serde::{Deserialize, Serialize};

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::theme;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const RESAMPLINGS: [(&str, &str); 5] = [
    ("nearest", "Nearest neighbour (classes, quality codes)"),
    ("bilinear", "Bilinear (continuous values)"),
    ("cubic", "Cubic (continuous values, smoother)"),
    ("average", "Average (to coarser cells)"),
    ("mode", "Mode (classes to coarser cells)"),
];
const MATCHINGS: [&str; 4] = ["The same day", "The nearest date within", "The same month", "The same year"];
const MAX_HISTORY: usize = 80;
const EDITOR: &str = "calc editor";

/// Examples (bare names are bands of the layer chosen for them).
const EXAMPLES: &[(&str, &str)] = &[
    ("NDVI", "(N - R) / (N + R)"),
    ("EVI", "2.5 * (N - R) / (N + 6 * R - 7.5 * B + 1)"),
    ("Maximum NDVI over time", "ndvi = (N - R) / (N + R)\nmax(ndvi)"),
    ("Each date's departure from the mean", "ndvi = (N - R) / (N + R)\nndvi - mean(ndvi)"),
    (
        "Classes by rules",
        "# the result starts as the NDVI; each rule changes the pixels it holds for\nresult = nd(N, R)\n\
         result < 0.2 = 1\nresult >= 0.2 && result < 0.5 = 2\nresult >= 0.5 = 3",
    ),
    ("Dates below a threshold", "count(nd(N, R) < 0.3)"),
    ("Dates with a value", "count(isvalid(N))"),
    ("90th percentile", "percentile(nd(N, R), 90)"),
    ("Gaps filled with a value", "coalesce(nd(N, R), -1)"),
    ("Seasonal: the date's month", "where(month >= 6 && month <= 9, nd(N, R), nan)"),
];

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    text: String,
    name: String,
    #[serde(default)]
    favorite: bool,
    #[serde(default)]
    when: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    text: String,
    #[serde(default)]
    history: Vec<Entry>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Layers,
    Functions,
    Indices,
    History,
}

/// The completions shown under the cursor.
struct Popup {
    range: Range<usize>,
    items: Vec<Completion>,
    sel: usize,
}

/// The text planned as it stands: a summary, or the error and where it is.
struct Check {
    key: u64,
    result: Result<Vec<String>, (CalcError, String)>,
}

/// The Display panel's expression for a layer: its text, and the index it was picked from while unchanged.
pub struct Draft {
    pub layer: u64,
    pub text: String,
    pub name: String,
    pub picked: String,
}

pub struct CalcUi {
    pub show: bool,
    pub text: String,
    pub out_name: String,
    /// the name typed (else from the text)
    name_typed: bool,
    /// the layer whose bands bare names are (None: the active one)
    pub primary: Option<String>,
    pub dates_from: Option<String>,
    pub grid_from: Option<String>,
    pub matching: usize,
    pub days: f64,
    pub resampling: usize,
    pub apply_qa: bool,
    /// written as GeoTIFFs (else a layer of VRTs)
    pub cog: bool,
    side: Side,
    /// the height of what is under the editor, the frame before (the editor takes the rest: a window whose
    /// content asks for more than it has grows)
    below: f32,
    /// the last run's job, its name and folder: the status line follows it
    last_run: Option<(Arc<Mutex<JobStatus>>, String, String)>,
    search: String,
    all_indices: bool,
    saved: Saved,
    loaded: bool,
    popup: Option<Popup>,
    /// the cursor's byte offset (where the lists insert)
    cursor: usize,
    check: Option<Check>,
    /// run once the layers are open (--calc-run)
    pub run_pending: bool,
    /// what the last run did
    pub status: String,
    pub draft: Option<Draft>,
    pub draft_search: String,
}

impl Default for CalcUi {
    fn default() -> Self {
        CalcUi {
            show: false,
            text: String::new(),
            out_name: String::new(),
            name_typed: false,
            primary: None,
            dates_from: None,
            grid_from: None,
            matching: 1,
            days: 8.0,
            resampling: 0,
            apply_qa: true,
            cog: false,
            side: Side::Layers,
            below: 300.0,
            last_run: None,
            search: String::new(),
            all_indices: false,
            saved: Saved::default(),
            loaded: false,
            popup: None,
            cursor: 0,
            check: None,
            run_pending: false,
            status: String::new(),
            draft: None,
            draft_search: String::new(),
        }
    }
}

fn saved_path() -> std::path::PathBuf {
    crate::settings::app_data_dir().join("calculator.json")
}

/// at() of date t of a series: by its year when the year has no other date, by its month when the month has none,
/// else by its day.
fn at_text(info: &janus_cube::CubeInfo, alias: &str, t: usize) -> String {
    let ymd = |t: usize| janus_cube::dates::civil_from_days((info.layers[t].time / 86400.0).floor() as i64);
    let (y, m, d) = ymd(t);
    let same = |f: &dyn Fn((i32, u32, u32)) -> bool| (0..info.t()).filter(|&k| f(ymd(k))).count() == 1;
    let when = if same(&|(a, _, _)| a == y) {
        y.to_string()
    } else if same(&|(a, b, _)| a == y && b == m) {
        format!("{y}{m:02}")
    } else {
        format!("{y}{m:02}{d:02}")
    };
    format!("at({alias}, {when})")
}

fn char_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

fn byte_of(text: &str, ch: usize) -> usize {
    text.char_indices().nth(ch).map_or(text.len(), |(i, _)| i)
}

/// The editor's colours, light or dark.
fn color(c: Class, dark: bool, text: Color32) -> Color32 {
    let rgb = |d: [u8; 3], l: [u8; 3]| {
        let v = if dark { d } else { l };
        Color32::from_rgb(v[0], v[1], v[2])
    };
    match c {
        Class::Number => rgb([209, 154, 102], [152, 104, 1]),
        Class::Str => rgb([152, 195, 121], [80, 161, 79]),
        Class::Comment => rgb([127, 132, 142], [160, 161, 167]),
        Class::Keyword | Class::Result => rgb([198, 120, 221], [166, 38, 164]),
        Class::Function => rgb([97, 175, 239], [64, 120, 242]),
        Class::Constant | Class::DateVar | Class::Input => rgb([86, 182, 194], [1, 132, 188]),
        Class::Layer => rgb([229, 192, 123], [193, 132, 1]),
        Class::Band => rgb([224, 108, 117], [228, 86, 73]),
        Class::Local => rgb([171, 178, 191], [56, 58, 66]),
        Class::Operator | Class::Bracket | Class::Unknown => text,
    }
}

/// The text coloured as the editor shows it, the error's span underlined.
fn highlight(
    text: &str,
    layers: &[CalcLayer],
    primary: Option<usize>,
    font: &FontId,
    dark: bool,
    plain: Color32,
    error: Option<Range<usize>>,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let classes = editor::classify(text, layers, primary);
    let fmt = |c: Option<Class>, err: bool| TextFormat {
        font_id: font.clone(),
        color: c.map_or(plain, |c| color(c, dark, plain)),
        italics: matches!(c, Some(Class::Comment | Class::Result)),
        underline: if err { Stroke::new(1.5, theme::error()) } else { Stroke::NONE },
        ..Default::default()
    };
    let err = error.filter(|e| e.end <= text.len());
    // pieces split where a token or the error starts or ends
    let mut cuts: Vec<usize> = vec![0, text.len()];
    for (r, _) in &classes {
        cuts.extend([r.start, r.end]);
    }
    if let Some(e) = &err {
        cuts.extend([e.start, e.end.max(e.start + 1).min(text.len())]);
    }
    cuts.retain(|&c| c <= text.len() && text.is_char_boundary(c));
    cuts.sort_unstable();
    cuts.dedup();
    let mut k = 0;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        while k < classes.len() && classes[k].0.end <= a {
            k += 1;
        }
        let c = classes.get(k).filter(|(r, _)| r.start <= a && b <= r.end).map(|x| x.1);
        let in_err = err.as_ref().is_some_and(|e| a < e.end.max(e.start + 1) && b > e.start);
        job.append(&text[a..b], 0.0, fmt(c, in_err));
    }
    job
}

impl CalcUi {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(s) = std::fs::read_to_string(saved_path())
            && let Ok(v) = serde_json::from_str::<Saved>(&s)
        {
            if self.text.is_empty() {
                self.text = v.text.clone();
            }
            self.saved = v;
        }
    }

    fn save(&mut self) {
        self.saved.text = self.text.clone();
        if let Ok(s) = serde_json::to_string_pretty(&self.saved) {
            let _ = std::fs::create_dir_all(crate::settings::app_data_dir());
            let _ = std::fs::write(saved_path(), s);
        }
    }

    fn remember(&mut self, name: &str) {
        let h = &mut self.saved.history;
        let favorite = h.iter().any(|e| e.text == self.text && e.favorite);
        h.retain(|e| e.text != self.text);
        h.insert(
            0,
            Entry { text: self.text.clone(), name: name.into(), favorite, when: crate::zeit::timestamp_text() },
        );
        // the favourites stay
        let mut n = 0;
        h.retain(|e| {
            n += 1;
            e.favorite || n <= MAX_HISTORY
        });
        self.save();
    }

    /// The text planned without an error.
    pub fn planned(&self) -> bool {
        self.check.as_ref().is_some_and(|c| c.result.as_ref().is_ok_and(|l| !l.is_empty()))
    }

    fn is_favorite(&self) -> bool {
        self.saved.history.iter().any(|e| e.text == self.text && e.favorite)
    }

    fn toggle_favorite(&mut self) {
        let on = !self.is_favorite();
        if let Some(e) = self.saved.history.iter_mut().find(|e| e.text == self.text) {
            e.favorite = on;
        } else if on {
            let name = self.default_name(None);
            self.saved.history.insert(
                0,
                Entry { text: self.text.clone(), name, favorite: true, when: crate::zeit::timestamp_text() },
            );
        }
        self.save();
    }

    /// The result's name: the name the text gives it, else its last statement's function and what it takes
    /// (`count(ndvi < 0.4)`: count_ndvi), else the first name it defines, else "calc".
    fn default_name(&self, plan_name: Option<&str>) -> String {
        default_name(&self.text, plan_name)
    }

    /// The status line once the last run ends: done (its layer added), failed or cancelled.
    fn follow_run(&mut self) {
        let Some((st, name, folder)) = &self.last_run else { return };
        let s = st.lock().unwrap();
        self.status = match s.state {
            JobState::Running => return,
            JobState::Done if s.seconds < 1.0 => {
                format!("\"{name}\" added as a layer, in {:.0} ms ({folder})", s.seconds * 1000.0)
            }
            JobState::Done => format!("\"{name}\" added as a layer, in {:.1} s ({folder})", s.seconds),
            JobState::Cancelled => format!("\"{name}\" cancelled"),
            _ => format!("\"{name}\" failed: {}", s.error),
        };
        drop(s);
        self.last_run = None;
    }

    /// Text inserted at the cursor (the editor gets the focus back, its cursor after it).
    fn insert(&mut self, ctx: &egui::Context, s: &str) {
        let at = self.cursor.min(self.text.len());
        let at = if self.text.is_char_boundary(at) { at } else { self.text.len() };
        self.text.insert_str(at, s);
        self.set_cursor(ctx, at + s.len());
    }

    fn set_cursor(&mut self, ctx: &egui::Context, byte: usize) {
        self.cursor = byte;
        let id = egui::Id::new(EDITOR);
        let mut st = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
        let c = CCursor::new(char_of(&self.text, byte));
        st.cursor.set_char_range(Some(CCursorRange::one(c)));
        st.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    fn accept(&mut self, ctx: &egui::Context) {
        let Some(p) = self.popup.take() else { return };
        let Some(c) = p.items.get(p.sel) else { return };
        let r = p.range.start.min(self.text.len())..p.range.end.min(self.text.len());
        self.text.replace_range(r.clone(), &c.insert);
        self.set_cursor(ctx, r.start + c.insert.len());
    }
}

fn default_name(text: &str, plan_name: Option<&str>) -> String {
    if let Some(n) = plan_name.filter(|n| *n != "result") {
        return n.to_string();
    }
    let statements: Vec<&str> = text
        .lines()
        .map(|l| l.split('#').next().unwrap_or(""))
        .flat_map(|l| l.split(';'))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let words = |l: &str| -> Vec<String> {
        l.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| w.chars().next().is_some_and(char::is_alphabetic))
            .map(String::from)
            .collect()
    };
    if let Some(last) = statements.last()
        && let [f, x, ..] = words(last).as_slice()
        && janus_cube::calc::funcs::func(f).is_some()
        && last.starts_with(&format!("{f}("))
    {
        return format!("{f}_{x}");
    }
    statements
        .iter()
        .find_map(|l| {
            let (target, rest) = l.split_once('=')?;
            let w = target.trim();
            let plain = !rest.starts_with('=') && w.len() >= 2 && w.chars().all(|c| c.is_alphanumeric() || c == '_');
            (plain && w != "result").then(|| w.to_string())
        })
        .unwrap_or_else(|| "calc".into())
}

/// What the result is and reads, line by line.
fn summary(p: &Plan, layers: &[CalcLayer], opts: &Options) -> Vec<String> {
    let mut out = Vec::new();
    let g = &layers[p.grid];
    let cell = g.info.geo_transform.map_or(String::new(), |t| {
        let geographic = gdal::spatial_ref::SpatialRef::from_wkt(&g.info.crs_wkt).is_ok_and(|s| s.is_geographic());
        let unit = if geographic { "°" } else { " m" };
        format!(" of {}{unit}", janus_cube::multidim::format_g(t[1].abs(), 4))
    });
    let shape = match p.series {
        Some(s) => format!("A series of {} dates (those of {})", layers[s].info.t(), layers[s].alias),
        None => "One image".to_string(),
    };
    out.push(format!("{shape}, {} × {} cells{cell} (the grid of {})", g.info.width, g.info.height, g.alias));
    let mut reads = Vec::new();
    for &l in &p.used {
        let cl = &layers[l];
        let mut s = cl.alias.clone();
        let mut notes = Vec::new();
        match plan::kind(&cl.info) {
            LayerKind::Stack => notes.push("a band of a stack".to_string()),
            _ if cl.info.t() == 1 && p.series.is_some() => notes.push("an image: at every date".into()),
            _ => {}
        }
        if p.series.is_some_and(|s| s != l) && cl.info.t() > 1 && plan::kind(&cl.info) != LayerKind::Stack {
            let (n, t) = plan::match_stats(p.series, layers, l, opts.matching);
            notes.push(if n == t { format!("all {t} dates matched") } else { format!("{n} of {t} dates matched") });
        }
        if l != p.grid {
            match plan::placement(&cl.info, &g.info) {
                Ok(Placement::Warp) => notes.push(format!("reprojected ({})", opts.resampling)),
                Ok(Placement::Window(..)) => {
                    let cell = |i: &janus_cube::CubeInfo| i.geo_transform.map_or(0.0, |t| t[1].abs());
                    if (cell(&cl.info) - cell(&g.info)).abs() > 1e-9 * cell(&g.info) {
                        notes.push(format!("resampled ({})", opts.resampling));
                    }
                }
                Ok(Placement::Outside) => notes.push("outside the grid!".into()),
                _ => {}
            }
        }
        if !notes.is_empty() {
            s += &format!(" ({})", notes.join(", "));
        }
        reads.push(s);
    }
    if !reads.is_empty() {
        out.push(format!("Reads {}", reads.join("; ")));
    }
    if !p.stages.is_empty() {
        let names: Vec<String> = p
            .stages
            .iter()
            .map(|s| match s.only {
                Some(t) => format!("the date {} of {}", layers[s.series].info.layers[t].label, layers[s.series].alias),
                None => s.f.name(),
            })
            .collect();
        out.push(format!("Over time: {}", names.join(", ")));
    }
    out
}

impl State {
    /// The open layers as the calculator names them (the canvas of vector layers left out), and each one's place
    /// among the map's layers.
    pub(crate) fn calc_layers(&self) -> (Vec<CalcLayer>, Vec<usize>) {
        let mut out: Vec<CalcLayer> = Vec::new();
        let mut at = Vec::new();
        for (i, l) in self.layers.iter().enumerate() {
            if l.canvas {
                continue;
            }
            let Some(s) = self.layer_session(i) else { continue };
            let taken: Vec<String> = out.iter().map(|c| c.alias.clone()).collect();
            out.push(CalcLayer { name: l.name.clone(), alias: plan::alias_of(&l.name, &taken), info: s.info.clone() });
            at.push(i);
        }
        (out, at)
    }

    fn calc_options(&self, layers: &[CalcLayer], at: &[usize]) -> Options {
        let c = &self.calc;
        let by_name = |n: &Option<String>| n.as_ref().and_then(|n| layers.iter().position(|l| &l.name == n));
        let primary = match &c.primary {
            Some(n) if n.is_empty() => None,
            Some(_) => by_name(&c.primary),
            None => at.iter().position(|&i| i == self.active),
        };
        Options {
            primary,
            dates_from: by_name(&c.dates_from),
            grid_from: by_name(&c.grid_from),
            matching: match c.matching {
                0 => DateMatch::Exact,
                2 => DateMatch::SameMonth,
                3 => DateMatch::SameYear,
                _ => DateMatch::Nearest(c.days),
            },
            resampling: RESAMPLINGS[c.resampling.min(RESAMPLINGS.len() - 1)].0.into(),
            apply_qa: c.apply_qa,
        }
    }

    pub(crate) fn calculator_window(&mut self, ctx: &egui::Context) {
        if !self.calc.show {
            return;
        }
        self.calc.load();
        let mut open = true;
        let max_h = (ctx.content_rect().height() - 60.0).max(300.0);
        egui::Window::new("Raster calculator")
            .id(egui::Id::new("calculator window"))
            .open(&mut open)
            .resizable(true)
            .default_size(vec2(980.0, 600.0_f32.min(max_h)))
            .min_size(vec2(560.0, 320.0))
            .max_height(max_h)
            .show(ctx, |ui| self.calculator_ui(ui));
        if !open {
            self.calc.show = false;
            self.calc.popup = None;
            self.calc.save();
        }
    }

    /// --calc-run: the text run once its layers are open.
    pub(crate) fn pump_calculator(&mut self, ctx: &egui::Context) {
        if self.calc.run_pending && self.opening.is_none() && self.pending_opens.is_empty() && !self.layers.is_empty() {
            self.calc.run_pending = false;
            self.calc_run(ctx);
        }
    }

    fn calculator_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let (layers, at) = self.calc_layers();
        let opts = self.calc_options(&layers, &at);
        self.calc_check(&layers, &opts);
        let mut run = false;

        // the toolbar
        ui.horizontal(|ui| {
            let ok = self.calc.check.as_ref().is_some_and(|c| c.result.is_ok()) && self.opening.is_none();
            let b = egui::Button::new(RichText::new("▶ Run").strong()).shortcut_text("Ctrl+Enter");
            if ui
                .add_enabled(ok, b.min_size(vec2(110.0, 0.0)))
                .on_hover_text("Makes the result as a layer (Ctrl+Enter in the editor)")
                .on_disabled_hover_text("Fix the expression first (the line under the editor says what is wrong)")
                .clicked()
            {
                run = true;
            }
            ui.separator();
            ui.label("Output");
            let c = &mut self.calc;
            if ui
                .add(egui::TextEdit::singleline(&mut c.out_name).desired_width(140.0).hint_text("name"))
                .on_hover_text("The layer's name (and its folder's)")
                .changed()
            {
                c.name_typed = !c.out_name.trim().is_empty();
            }
            egui::ComboBox::from_id_salt("calc output")
                .selected_text(if c.cog { "GeoTIFFs" } else { "Virtual layer" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut c.cog, false, "Virtual layer").on_hover_text(
                        "A VRT per date, computed as the map, the charts and the tools read it:\n\
                         nothing is written but the formula (instant, small)",
                    );
                    ui.selectable_value(&mut c.cog, true, "GeoTIFFs (cloud-optimized)").on_hover_text(
                        "Every pixel computed once and written as compressed COGs, one per date\n\
                         (a task in Tools > Tasks): faster to read after, works without the sources",
                    );
                });
            ui.separator();
            let fav = c.is_favorite();
            if ui
                .add_enabled(!c.text.trim().is_empty(), egui::Button::new(if fav { "★" } else { "☆" }).frame(false))
                .on_hover_text(if fav { "A favourite (click to remove it)" } else { "Keep as a favourite" })
                .clicked()
            {
                c.toggle_favorite();
            }
            ui.menu_button("Examples", |ui| {
                for (name, text) in EXAMPLES {
                    if ui.button(*name).on_hover_text(RichText::new(*text).monospace()).clicked() {
                        c.text = text.to_string();
                        c.name_typed = false;
                        c.popup = None;
                        ui.close();
                    }
                }
            });
            if ui.button("Clear").clicked() {
                c.text.clear();
                c.name_typed = false;
                c.popup = None;
            }
        });
        ui.add_space(2.0);

        egui::Panel::right("calc side")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| self.calc_side(ui, &layers, &opts));
        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
            if layers.is_empty() {
                ui.colored_label(
                    theme::warning(),
                    "Open a layer first: its short name appears in Layers, on the right.",
                );
            }
            run |= self.calc_editor(ui, &layers, opts.primary);
            let top = ui.cursor().top();
            self.calc_status(ui);
            self.calc_options_ui(ui, &layers, &opts);
            self.calc.follow_run();
            if !self.calc.status.is_empty() {
                ui.add_space(4.0);
                ui.weak(&self.calc.status);
            }
            // the runs under way
            for d in &self.emb_downloads {
                let st = d.status.lock().unwrap();
                if st.state == JobState::Running && d.title.starts_with("Calculator") {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("{}: {:.0}% {}", d.title, st.progress * 100.0, st.message));
                    });
                }
            }
            self.calc.below = ui.cursor().top() - top;
        });
        if run {
            self.calc_run(&ctx);
        }
    }

    /// Plans the text when it or the layers changed.
    fn calc_check(&mut self, layers: &[CalcLayer], opts: &Options) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.calc.text.hash(&mut h);
        for l in layers {
            (l.info.id, &l.alias).hash(&mut h);
        }
        format!("{opts:?}").hash(&mut h);
        let key = h.finish();
        if self.calc.check.as_ref().is_some_and(|c| c.key == key) {
            return;
        }
        let text = &self.calc.text;
        let result = if text.trim().is_empty() {
            Ok(vec![])
        } else {
            match plan::plan(text, layers, opts) {
                Ok(p) => {
                    let s = summary(&p, layers, opts);
                    if !self.calc.name_typed {
                        self.calc.out_name = self.calc.default_name(p.name.as_deref());
                    }
                    Ok(s)
                }
                Err(e) => {
                    let (l, c) = e.line_col(text);
                    Err((e, format!("line {l}, column {c}")))
                }
            }
        };
        self.calc.check = Some(Check { key, result });
    }

    /// The editor: the text coloured, its lines numbered, completions under the cursor, the error underlined.
    /// True when Ctrl+Enter asked for a run.
    fn calc_editor(&mut self, ui: &mut Ui, layers: &[CalcLayer], primary: Option<usize>) -> bool {
        let ctx = ui.ctx().clone();
        let id = egui::Id::new(EDITOR);
        let focused = ui.memory(|m| m.has_focus(id));
        let mut run = false;
        // the keys the editor would take otherwise
        if focused {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
                run = true;
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Space)) {
                self.calc.popup = editor::complete(&self.calc.text, self.calc.cursor, layers, primary)
                    .map(|(range, items)| Popup { range, items, sel: 0 });
            }
            if self.calc.popup.is_some() {
                let n = self.calc.popup.as_ref().map_or(0, |p| p.items.len());
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown))
                    && let Some(p) = &mut self.calc.popup
                {
                    p.sel = (p.sel + 1) % n.max(1);
                }
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp))
                    && let Some(p) = &mut self.calc.popup
                {
                    p.sel = (p.sel + n.max(1) - 1) % n.max(1);
                }
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                    self.calc.popup = None;
                }
                if ctx.input_mut(|i| {
                    i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Enter)
                }) {
                    self.calc.accept(&ctx);
                }
            }
        }
        // code at the size of the interface's text (egui's monospace is smaller)
        let font = egui::FontId::monospace(egui::TextStyle::Body.resolve(ui.style()).size);
        let dark = ui.visuals().dark_mode;
        let plain = ui.visuals().text_color();
        let error = match self.calc.check.as_ref().map(|c| &c.result) {
            Some(Err((e, _))) => Some(e.span.clone()),
            _ => None,
        };
        let lines = self.calc.text.split('\n').count().max(1);
        let digits = lines.to_string().len().max(2);
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let gutter = char_w * digits as f32 + 14.0;
        let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
            let job = highlight(buf.as_str(), layers, primary, &font, dark, plain, error.clone());
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let height = (ui.available_height() - self.calc.below - 6.0).max(120.0);
        let out = egui::ScrollArea::both()
            .id_salt("calc editor scroll")
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::TextEdit::multiline(&mut self.calc.text)
                    .id(id)
                    .code_editor()
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .min_size(vec2(0.0, height - 4.0))
                    .margin(egui::Margin { left: gutter as i8, right: 6, top: 6, bottom: 6 })
                    .hint_text(
                        "An expression over the layers, e.g.\n  (s2.nir - s2.red) / (s2.nir + s2.red)\n  \
                         ndvi = nd(N, R)\n  max(ndvi)\nCtrl+Space completes, Ctrl+Enter runs.",
                    )
                    .layouter(&mut layouter)
                    .show(ui)
            })
            .inner;
        let resp = out.response.response.clone();
        // the line numbers, in the margin
        {
            let painter = ui.painter_at(resp.rect);
            let weak = theme::weak_text();
            let mut line = 1;
            let mut new_line = true;
            let cur_line = out.cursor_range.map(|r| char_of_line(&self.calc.text, r.primary.index.0));
            for row in &out.galley.rows {
                if new_line {
                    let y = out.galley_pos.y + row.pos.y;
                    let col = if Some(line) == cur_line { plain } else { weak };
                    painter.text(
                        egui::pos2(resp.rect.left() + gutter - 8.0, y),
                        egui::Align2::RIGHT_TOP,
                        line.to_string(),
                        font.clone(),
                        col,
                    );
                    line += 1;
                }
                new_line = row.ends_with_newline;
            }
            let x = resp.rect.left() + gutter - 3.0;
            painter.line_segment(
                [egui::pos2(x, resp.rect.top()), egui::pos2(x, resp.rect.bottom())],
                Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            );
        }
        // the cursor, and the completions as one types
        if let Some(r) = out.cursor_range {
            let byte = byte_of(&self.calc.text, r.primary.index.0);
            let moved = byte != self.calc.cursor;
            self.calc.cursor = byte;
            if resp.changed() {
                let typed = self.calc.text[..byte].chars().next_back();
                self.calc.popup = match typed {
                    Some(c) if c.is_alphanumeric() || c == '_' || c == '.' => {
                        editor::complete(&self.calc.text, byte, layers, primary).map(|(range, items)| Popup {
                            range,
                            items,
                            sel: 0,
                        })
                    }
                    _ => None,
                };
                self.calc.status.clear();
            } else if moved {
                self.calc.popup = None;
            }
        }
        if !focused && !ui.memory(|m| m.has_focus(id)) && !ctx.egui_is_using_pointer() {
            // the popup stays only while the editor is focused (a click on it refocuses)
            if self.calc.popup.is_some() && ctx.input(|i| i.pointer.any_pressed()) {
                self.calc.popup = None;
            }
        }
        // what is under the pointer
        if let Some(pos) = resp.hover_pos()
            && self.calc.popup.is_none()
        {
            let rel = pos - out.galley_pos;
            let on_text = out.galley.rows.iter().any(|row| row.rect().contains(egui::pos2(rel.x, rel.y)));
            if on_text {
                let c = out.galley.cursor_from_pos(rel);
                let byte = byte_of(&self.calc.text, c.index.0);
                if let Some((_, d)) = editor::describe_at(&self.calc.text, byte, layers, primary) {
                    resp.clone().on_hover_ui_at_pointer(|ui| {
                        ui.set_max_width(420.0);
                        ui.label(RichText::new(d).monospace());
                    });
                }
            }
        }
        // the completions' list, under the cursor
        if let Some(p) = &self.calc.popup
            && let Some(r) = out.cursor_range
        {
            let at = out.galley.pos_from_cursor(r.primary);
            let pos = out.galley_pos + vec2(at.left(), at.bottom() + 2.0);
            let mut clicked = None;
            egui::Area::new(egui::Id::new("calc completions")).order(egui::Order::Foreground).fixed_pos(pos).show(
                &ctx,
                |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_width(260.0);
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            for (k, c) in p.items.iter().enumerate() {
                                let label = RichText::new(&c.label).monospace().color(color(c.class, dark, plain));
                                let r = ui
                                    .horizontal(|ui| {
                                        let r = ui.selectable_label(k == p.sel, label);
                                        ui.weak(&c.detail);
                                        r
                                    })
                                    .inner;
                                if k == p.sel {
                                    r.scroll_to_me(None);
                                }
                                if r.clicked() {
                                    clicked = Some(k);
                                }
                            }
                        });
                        ui.weak("Tab or Enter: insert · Esc: close");
                    });
                },
            );
            if let Some(k) = clicked {
                if let Some(p) = &mut self.calc.popup {
                    p.sel = k;
                }
                self.calc.accept(&ctx);
            }
        }
        run
    }

    /// The line under the editor: what the result will be, or what is wrong and where (a click goes there).
    fn calc_status(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let Some(c) = &self.calc.check else { return };
        match &c.result {
            Ok(lines) if lines.is_empty() => {
                ui.weak("Type an expression; the layers, functions and indices on the right insert at the cursor.");
            }
            Ok(lines) => {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::from_rgb(80, 180, 110), "✔");
                    ui.label(&lines[0]);
                });
                for l in &lines[1..] {
                    ui.weak(l);
                }
            }
            Err((e, at)) => {
                let span = e.span.clone();
                let r = ui
                    .horizontal_wrapped(|ui| {
                        ui.colored_label(theme::error(), "✖");
                        ui.add(
                            egui::Label::new(RichText::new(format!("{at}: {}", e.message)).color(theme::error()))
                                .sense(egui::Sense::click()),
                        )
                    })
                    .inner;
                if r.on_hover_text("Click: the cursor to it").clicked() {
                    let b = span.start.min(self.calc.text.len());
                    self.calc.set_cursor(&ctx, b);
                }
            }
        }
    }

    fn calc_options_ui(&mut self, ui: &mut Ui, layers: &[CalcLayer], opts: &Options) {
        egui::CollapsingHeader::new("Options").id_salt("calc options").default_open(true).show(ui, |ui| {
            let c = &mut self.calc;
            let name_of = |i: Option<usize>, none: &str| i.map_or(none.to_string(), |i| layers[i].alias.clone());
            egui::Grid::new("calc options grid").num_columns(2).spacing(vec2(10.0, 4.0)).show(ui, |ui| {
                ui.label("Bare names").on_hover_text(
                    "Whose bands N, R, nir, B04... are without a layer's name before them\n(the active layer unless chosen)",
                );
                egui::ComboBox::from_id_salt("calc primary")
                    .selected_text(name_of(opts.primary, "none (always name the layer)"))
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(c.primary.is_none(), "The active layer").clicked() {
                            c.primary = None;
                        }
                        if ui.selectable_label(c.primary.as_deref() == Some(""), "None").clicked() {
                            c.primary = Some(String::new());
                        }
                        for l in layers {
                            if ui.selectable_label(c.primary.as_ref() == Some(&l.name), &l.alias).clicked() {
                                c.primary = Some(l.name.clone());
                            }
                        }
                    });
                ui.end_row();
                let series: Vec<&CalcLayer> = layers.iter().filter(|l| l.info.t() > 1).collect();
                ui.label("Dates of").on_hover_text("The series whose dates the result has (else the first it reads)");
                egui::ComboBox::from_id_salt("calc dates")
                    .selected_text(c.dates_from.clone().map_or("the first series read".into(), |n| {
                        layers.iter().find(|l| l.name == n).map_or(n, |l| l.alias.clone())
                    }))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut c.dates_from, None, "The first series read");
                        for l in &series {
                            ui.selectable_value(&mut c.dates_from, Some(l.name.clone()), &l.alias);
                        }
                    });
                ui.end_row();
                ui.label("Grid of").on_hover_text(
                    "The layer whose cells the result has (else that of its dates, else the first read);\n\
                     the others are resampled or reprojected onto it",
                );
                egui::ComboBox::from_id_salt("calc grid")
                    .selected_text(c.grid_from.clone().map_or("the layer of the dates".into(), |n| {
                        layers.iter().find(|l| l.name == n).map_or(n, |l| l.alias.clone())
                    }))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut c.grid_from, None, "The layer of the dates");
                        for l in layers {
                            ui.selectable_value(&mut c.grid_from, Some(l.name.clone()), &l.alias);
                        }
                    });
                ui.end_row();
                ui.label("Match dates").on_hover_text(
                    "How another series' dates join the result's: a date with none\nmatched is no data there",
                );
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("calc matching").selected_text(MATCHINGS[c.matching]).show_ui(
                        ui,
                        |ui| {
                            for (i, m) in MATCHINGS.iter().enumerate() {
                                ui.selectable_value(&mut c.matching, i, *m);
                            }
                        },
                    );
                    if c.matching == 1 {
                        ui.add(egui::DragValue::new(&mut c.days).range(0.0..=400.0).speed(0.5).suffix(" days"));
                    }
                });
                ui.end_row();
                ui.label("Resampling");
                egui::ComboBox::from_id_salt("calc resampling")
                    .selected_text(RESAMPLINGS[c.resampling].0)
                    .show_ui(ui, |ui| {
                        for (i, (r, what)) in RESAMPLINGS.iter().enumerate() {
                            ui.selectable_value(&mut c.resampling, i, *r).on_hover_text(*what);
                        }
                    });
                ui.end_row();
                ui.label("Quality");
                ui.checkbox(&mut c.apply_qa, "Mask the bands named by their layer's quality band")
                    .on_hover_text("Clouds, shadows and fill as no data (the quality band shown in Display)");
                ui.end_row();
            });
        });
    }

    /// The lists on the right: the layers, the functions, the indices, the texts run.
    fn calc_side(&mut self, ui: &mut Ui, layers: &[CalcLayer], opts: &Options) {
        let ctx = ui.ctx().clone();
        let dark = ui.visuals().dark_mode;
        let plain = ui.visuals().text_color();
        ui.horizontal(|ui| {
            let s = &mut self.calc.side;
            ui.selectable_value(s, Side::Layers, "Layers");
            ui.selectable_value(s, Side::Functions, "Functions");
            ui.selectable_value(s, Side::Indices, "Indices");
            ui.selectable_value(s, Side::History, "History");
        });
        if matches!(self.calc.side, Side::Functions | Side::Indices | Side::History) {
            ui.add(egui::TextEdit::singleline(&mut self.calc.search).hint_text("Search").desired_width(f32::INFINITY));
        }
        ui.separator();
        let mut insert: Option<String> = None;
        let mut load: Option<Entry> = None;
        let search = self.calc.search.to_lowercase();
        let hit = |s: &str| search.is_empty() || s.to_lowercase().contains(&search);
        egui::ScrollArea::vertical().id_salt("calc side scroll").auto_shrink([false, false]).show(ui, |ui| match self
            .calc
            .side
        {
            Side::Layers => {
                if layers.is_empty() {
                    ui.weak("No layer is open.");
                }
                for (i, l) in layers.iter().enumerate() {
                    let bare = opts.primary == Some(i);
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::Button::new(RichText::new(&l.alias).monospace().strong().color(color(
                                Class::Layer,
                                dark,
                                plain,
                            )))
                            .frame(false),
                        );
                        if r.on_hover_text(editor::layer_summary(l)).clicked() {
                            insert = Some(l.alias.clone());
                        }
                        if bare {
                            ui.weak("(bare names)");
                        }
                    });
                    ui.weak(format!("{}: {}", l.name, editor::layer_kind_text(l)));
                    let names = plan::member_names(&l.info);
                    egui::CollapsingHeader::new(format!(
                        "{} band{}",
                        names.len(),
                        if names.len() == 1 { "" } else { "s" }
                    ))
                    .id_salt(("calc bands", &l.alias))
                    .default_open(layers.len() <= 2 && names.len() <= 16)
                    .show(ui, |ui| {
                        for b in 1..=names.len() as i32 {
                            let roles = names.roles_of(b);
                            let word = roles.first().map_or(format!("band{b}"), |r| r.to_string());
                            let text = if bare { word.clone() } else { format!("{}.{word}", l.alias) };
                            ui.horizontal(|ui| {
                                let r = ui.add(
                                    egui::Button::new(RichText::new(&text).monospace().color(color(
                                        Class::Band,
                                        dark,
                                        plain,
                                    )))
                                    .frame(false),
                                );
                                let nm = l.info.band_nm.get(b as usize - 1).copied().filter(|w| *w > 0.0);
                                // the wavelength, unless the name gives it ("Red (B04) (665.0 nm)")
                                let name = &names.names[b as usize - 1];
                                let what = name.clone()
                                    + &nm
                                        .filter(|_| !name.ends_with("nm)"))
                                        .map_or(String::new(), |w| format!(" · {w:.0} nm"));
                                if r.on_hover_text(names.describe(b)).clicked() {
                                    insert = Some(text.clone());
                                }
                                ui.weak(what);
                            });
                        }
                    });
                    // a series' dates, each as at(): one date as an image
                    if l.info.t() > 1 && l.info.time_is_date {
                        egui::CollapsingHeader::new(format!("{} dates", l.info.t()))
                            .id_salt(("calc dates", &l.alias))
                            .show(ui, |ui| {
                                for t in 0..l.info.t() {
                                    let text = at_text(&l.info, &l.alias, t);
                                    ui.horizontal(|ui| {
                                        let r = ui.add(
                                            egui::Button::new(RichText::new(&text).monospace().color(color(
                                                Class::Function,
                                                dark,
                                                plain,
                                            )))
                                            .frame(false),
                                        );
                                        if r.on_hover_text("This date alone, as an image").clicked() {
                                            insert = Some(text.clone());
                                        }
                                        ui.weak(&l.info.layers[t].label);
                                    });
                                }
                            });
                    }
                    ui.add_space(4.0);
                }
                ui.separator();
                ui.weak(
                    "A layer by its short name is what it shows (its band, index or expression);\n\
                         layer.band is one of its bands: by role (N, R, S1, RE1), by name (nir, B04)\n\
                         or by place (band4, layer[4]). \"Any name\".band works too.\n\
                         A series is computed date by date; at(series, 2003) is one of its dates\n\
                         (a year, 200307 a month, 20030715 a day), mean(series) and the like reduce it.",
                );
            }
            Side::Functions => {
                for g in [Group::Math, Group::Logic, Group::Index, Group::Bits, Group::Time] {
                    let fs: Vec<_> = FUNCS.iter().filter(|f| f.group == g && (hit(f.name) || hit(f.doc))).collect();
                    if fs.is_empty() {
                        continue;
                    }
                    crate::panels::section(ui, g.name());
                    for f in fs {
                        let r = ui.add(
                            egui::Button::new(
                                RichText::new(format!("{}({})", f.name, f.args)).monospace().color(color(
                                    Class::Function,
                                    dark,
                                    plain,
                                )),
                            )
                            .frame(false),
                        );
                        if r.on_hover_text(f.doc).clicked() {
                            insert = Some(format!("{}(", f.name));
                        }
                    }
                }
                let vars: Vec<_> = DATE_VARS.iter().filter(|v| hit(v.0) || hit(v.1)).collect();
                if !vars.is_empty() {
                    crate::panels::section(ui, "The date's variables");
                    for (v, doc) in vars {
                        let r = ui.add(
                            egui::Button::new(RichText::new(*v).monospace().color(color(Class::DateVar, dark, plain)))
                                .frame(false),
                        );
                        if r.on_hover_text(*doc).clicked() {
                            insert = Some(v.to_string());
                        }
                    }
                }
                crate::panels::section(ui, "Constants and operators");
                for (c, _, doc) in CONSTANTS.iter().filter(|c| hit(c.0) || hit(c.2)) {
                    let r = ui.add(
                        egui::Button::new(RichText::new(*c).monospace().color(color(Class::Constant, dark, plain)))
                            .frame(false),
                    );
                    if r.on_hover_text(*doc).clicked() {
                        insert = Some(c.to_string());
                    }
                }
                ui.weak(
                    "+ - * / % ^ (or **)   < <= > >= == !=   and or not (&& || !)\n\
                         name = expression    gives a name to a value\n\
                         condition = value    a rule: sets the result where the condition holds\n\
                         x == nan             tests for no data   # a comment",
                );
            }
            Side::Indices => {
                let primary = opts.primary.and_then(|p| layers.get(p));
                if let Some(p) = primary {
                    ui.checkbox(&mut self.calc.all_indices, format!("Also those {} cannot compute", p.alias));
                } else {
                    ui.weak("Choose the layer of bare names (Options) to see which it can compute.");
                }
                ui.weak(format!("{} indices of the Awesome Spectral Indices (MIT)", indices::all().len()));
                let names = primary.map(|p| plan::member_names(&p.info));
                for ix in indices::all() {
                    if !(hit(&ix.name) || hit(&ix.long_name) || hit(&ix.domain)) {
                        continue;
                    }
                    let missing = names.as_ref().map(|n| ix.missing(n)).unwrap_or_default();
                    let usable = names.as_ref().is_some_and(|n| ix.usable(n));
                    if names.is_some() && !usable && !self.calc.all_indices {
                        continue;
                    }
                    let text = match primary {
                        Some(p) if usable => editor::index_text(ix, p, true),
                        _ => None,
                    };
                    let r = ui
                        .add_enabled_ui(names.is_none() || usable, |ui| {
                            ui.horizontal(|ui| {
                                let r = ui
                                    .add(egui::Button::new(RichText::new(&ix.name).monospace().strong()).frame(false));
                                ui.weak(&ix.long_name);
                                r
                            })
                            .inner
                        })
                        .inner;
                    let mut tip = format!("{}\n{}\n{}", ix.title(), ix.formula, ix.domain);
                    if !missing.is_empty() {
                        tip += &format!("\nneeds {}", missing.join(", "));
                    } else if ix.needs_external() {
                        tip += "\nneeds a value no band gives";
                    }
                    if !ix.reference.is_empty() {
                        tip += &format!("\n{}", ix.reference);
                    }
                    if r.on_hover_text(tip).on_disabled_hover_text("this layer lacks a band it needs").clicked() {
                        insert = Some(text.unwrap_or_else(|| ix.formula.replace("**", "^")));
                    }
                }
            }
            Side::History => {
                let h = &self.calc.saved.history;
                if h.is_empty() {
                    ui.weak("The texts you run are kept here; ☆ keeps one as a favourite.");
                }
                let mut remove = None;
                let mut star = None;
                for fav in [true, false] {
                    let list: Vec<(usize, &Entry)> = h
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.favorite == fav && (hit(&e.name) || hit(&e.text)))
                        .collect();
                    if list.is_empty() {
                        continue;
                    }
                    crate::panels::section(ui, if fav { "Favourites" } else { "Recent" });
                    for (i, e) in list {
                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new(if e.favorite { "★" } else { "☆" }).frame(false)).clicked()
                            {
                                star = Some(i);
                            }
                            let first = e.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                            let r = ui.add(egui::Button::new(RichText::new(&e.name).strong()).frame(false));
                            ui.weak(RichText::new(first).monospace());
                            if r.on_hover_text(RichText::new(format!("{}\n\n{}", e.text, e.when)).monospace()).clicked()
                            {
                                load = Some(e.clone());
                            }
                            if ui.small_button("×").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    }
                }
                if let Some(i) = star {
                    self.calc.saved.history[i].favorite ^= true;
                    self.calc.save();
                }
                if let Some(i) = remove {
                    self.calc.saved.history.remove(i);
                    self.calc.save();
                }
            }
        });
        if let Some(s) = insert {
            self.calc.popup = None;
            self.calc.insert(&ctx, &s);
        }
        if let Some(e) = load {
            self.calc.text = e.text;
            self.calc.out_name = e.name;
            self.calc.name_typed = true;
            self.calc.popup = None;
            let end = self.calc.text.len();
            self.calc.set_cursor(&ctx, end);
        }
    }

    /// The result made: its VRTs written (and translated to GeoTIFFs) in a task, the layer added when ready.
    pub(crate) fn calc_run(&mut self, ctx: &egui::Context) {
        let (layers, at) = self.calc_layers();
        let opts = self.calc_options(&layers, &at);
        let text = self.calc.text.clone();
        let p = match plan::plan(&text, &layers, &opts) {
            Ok(p) => p,
            Err(e) => {
                let (l, c) = e.line_col(&text);
                self.calc.status = format!("not run: line {l}, column {c}: {}", e.message);
                return;
            }
        };
        let name = if self.calc.out_name.trim().is_empty() {
            self.calc.default_name(p.name.as_deref())
        } else {
            self.calc.out_name.trim().to_string()
        };
        let folder = self.results_dir().join("calc").join(format!(
            "{}_{}",
            janus_cube::stac_vrt::sanitize(&name),
            crate::zeit::timestamp()
        ));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.error_popup = Some(format!("could not create {}: {e}", folder.display()));
            return;
        }
        let cog = self.calc.cog;
        let n_dates = p.series.map_or(1, |s| layers[s].info.t());
        let title = format!(
            "Calculator {name}: {}{}",
            if n_dates > 1 { format!("{n_dates} dates") } else { "an image".into() },
            if cog { ", GeoTIFFs" } else { "" }
        );
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Raster calculator: {name}\n{}\nlayers: {}\noptions: {opts:?}\noutput: {}\n--- text\n{text}\n---\n",
            crate::zeit::timestamp_text(),
            layers.iter().map(|l| format!("{} = \"{}\"", l.alias, l.name)).collect::<Vec<_>>().join(", "),
            folder.display()
        ));
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
        let (t2, o2) = (text.clone(), opts.clone());
        std::thread::Builder::new()
            .name("calculator".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let split = if cog { 0.05 } else { 1.0 };
                let vrt_dir = if cog { out.join("vrt") } else { out.clone() };
                let r = p.write(&t2, &layers, &o2, &vrt_dir, &|f, m| progress(f * split, m), &cn).and_then(|w| {
                    if w.warped > 0 {
                        log.line(&format!("{} layers reprojected onto the grid", w.warped));
                    }
                    if cog {
                        janus_cube::stac_vrt::download(
                            &w.files,
                            &out,
                            &|f, m| progress(split + (1.0 - split) * f, m),
                            &cn,
                        )
                    } else {
                        Ok(w.files)
                    }
                });
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(files) => {
                        log.line(&format!("{} files, {:.1} s", files.len(), s.seconds));
                        log.line("done");
                        (s.state, s.progress) = (JobState::Done, 1.0);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        (s.state, s.error) = (JobState::Failed, e);
                    }
                }
                c.request_repaint();
            })
            .ok();
        let folder_s = folder.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "calculator".into(),
            cube: 0,
            output_dir: folder_s.clone(),
            log_path,
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.emb_downloads.push(EmbDownload {
            title,
            status: status.clone(),
            started: Instant::now(),
            folder: folder_s,
            name: Some(name.clone()),
            // the layer the expression was written on stays active
            keep_active: true,
            opened: false,
            ended: None,
        });
        self.calc.remember(&name);
        self.calc.status = format!("\"{name}\" under way: it is added as a layer when ready ({})", folder.display());
        self.calc.last_run = Some((status, name, folder.display().to_string()));
    }

    /// The Display panel's index or expression of a layer of several bands: an index of the library picked by
    /// name, or a text over the bands; applied, the layer shows it (map, charts, tools).
    pub(crate) fn expression_ui(
        &mut self,
        ui: &mut Ui,
        info: &janus_cube::CubeInfo,
        sel: &mut janus_cube::BandSelection,
    ) {
        let names = janus_cube::calc::names::BandNames::of(info);
        let d = self.calc.draft.get_or_insert_with(|| Draft {
            layer: u64::MAX,
            text: String::new(),
            name: String::new(),
            picked: String::new(),
        });
        if d.layer != info.id {
            *d = Draft {
                layer: info.id,
                text: info.sel.expr.text(),
                name: info.sel.expr.name(),
                picked: info.sel.expr.text(),
            };
        }
        let search = &mut self.calc.draft_search;
        let current = if d.name.is_empty() { "Spectral index...".to_string() } else { d.name.clone() };
        egui::ComboBox::from_id_salt("display index")
            .width(ui.available_width())
            .height(320.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(search).hint_text("Search (EVI, water, burn...)"));
                let s = search.to_lowercase();
                let mut n = 0;
                for ix in indices::all() {
                    if !ix.usable(&names) {
                        continue;
                    }
                    let t = ix.title();
                    if !s.is_empty() && !t.to_lowercase().contains(&s) && !ix.domain.contains(&s) {
                        continue;
                    }
                    n += 1;
                    let r = ui.selectable_label(d.name == ix.name, &t).on_hover_text(format!(
                        "{}\n{}{}",
                        ix.formula,
                        ix.domain,
                        if ix.reference.is_empty() { String::new() } else { format!("\n{}", ix.reference) }
                    ));
                    if r.clicked()
                        && let Some(f) = ix.formula_for(&names, &info.band_nm)
                    {
                        d.text = f.clone();
                        d.picked = f;
                        d.name = ix.name.clone();
                    }
                }
                if n == 0 {
                    ui.weak("none of the bands' indices matches");
                }
            })
            .response
            .on_hover_text(
                "The indices of the Awesome Spectral Indices this series has the bands for,\n\
                 its bands placed by wavelength or name (N, R, RE1, S1...)",
            );
        let r = ui.add(
            egui::TextEdit::multiline(&mut d.text)
                .code_editor()
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("or an expression: (N - R) / (N + R)"),
        );
        if r.changed() && d.text != d.picked {
            d.name.clear();
        }
        let text = d.text.trim().to_string();
        let mut ok = true;
        if text.is_empty() {
            sel.expr = janus_cube::calc::ExprId::NONE;
        } else {
            match janus_cube::calc::LayerExpr::compile(&text, &names) {
                Ok(e) => {
                    sel.expr = janus_cube::calc::ExprId::intern(&d.name, &text);
                    let bands: Vec<String> = e.bands.iter().map(|&b| names.describe(b)).collect();
                    ui.weak(format!("reads {}", bands.join("; ")));
                }
                Err(e) => {
                    let (l, c) = e.line_col(&text);
                    ui.colored_label(
                        theme::error(),
                        if text.contains('\n') {
                            format!("line {l}, column {c}: {e}")
                        } else {
                            format!("column {c}: {e}")
                        },
                    );
                    ok = false;
                    sel.expr = info.sel.expr;
                }
            }
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ok && !text.is_empty(), egui::Button::new("Open in calculator"))
                .on_hover_text("The expression in Tools > Raster calculator, over this layer's bands")
                .clicked()
            {
                self.calc.text = text.clone();
                self.calc.primary = None;
                self.calc.name_typed = false;
                self.calc.show = true;
            }
            if !text.is_empty() && ui.small_button("Clear").clicked() {
                if let Some(d) = &mut self.calc.draft {
                    d.text.clear();
                    d.name.clear();
                }
                sel.expr = janus_cube::calc::ExprId::NONE;
            }
        });
    }
}

/// The line (from 1) of a character index.
fn char_of_line(text: &str, ch: usize) -> usize {
    text.chars().take(ch).filter(|&c| c == '\n').count() + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_names() {
        let n = |t: &str| super::default_name(t, None);
        assert_eq!(n("# dates\nndvi = (N - R) / (N + R)\ncount(ndvi < 0.4)"), "count_ndvi");
        assert_eq!(n("ndvi = nd(N, R); ndvi - mean(ndvi)"), "ndvi");
        assert_eq!(n("result = nd(N, R)\nresult < 0.2 = 1"), "calc");
        assert_eq!(n("(N - R) / (N + R)"), "calc");
        assert_eq!(super::default_name("x", Some("evi")), "evi");
    }
}
