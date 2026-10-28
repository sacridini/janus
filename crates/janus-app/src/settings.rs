//! Settings (src/app_settings.cpp, app_fullres.cpp): the interface's theme, font size and transparency, the
//! processing threads, the overview memory and the caches, whose disk budget is regulated here too
//! (pump_disk_cache). Kept in janus-rs.ini in the C++'s format ([Janus][Settings], [JanusBasemap][Settings]); the
//! first time, read from the C++'s layout file, so both versions start alike. --budget overrides the memory for
//! one run.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Slider, Ui};
use janus_cache::disk::{self, Budget};
use janus_session::FullResMode;

use crate::app::State;
use crate::theme;

/// The values kept between sessions.
#[derive(Clone)]
pub struct Settings {
    /// the modern interface (Inter, the image editors' greys, rounded cards), else the classic one (ImGui's look)
    pub modern: bool,
    /// the classic interface's theme (theme::NAMES) and the modern one's (theme::MODERN_NAMES)
    pub theme: usize,
    pub modern_theme: usize,
    /// ImGui's pixels (13: ProggyClean as ImGui draws it)
    pub font_size: i32,
    /// the panels' backgrounds (the desktop shows through) and the whole window
    pub panel_opacity: f32,
    pub window_opacity: f32,
    /// the Files panel goes to what is opened
    pub reveal_opened: bool,
    /// GitHub asked for a newer version at the start (update.rs)
    pub check_updates: bool,
    /// processing threads; 0: every logical core but 2
    pub threads: usize,
    pub overview_budget_mb: i32,
    /// the disk cache budget; 0: automatic
    pub cache_budget_gb: u64,
    pub full_res: FullResMode,
    pub export_dir: String,
    pub ffmpeg: String,
    pub interpreter: String,
    pub favorites: Vec<String>,
    /// the command palette's commands used last, newest first
    pub palette_recent: Vec<String>,
    /// saved layer styles: name, style (style.rs's text)
    pub styles: Vec<(String, String)>,
    /// saved views (navigation.rs's text); the minimap shown
    pub saved_views: Vec<String>,
    pub minimap: bool,
    /// the projects opened or saved last, newest first; the last session opened again at the start
    pub recent_projects: Vec<String>,
    pub reopen_last: bool,
}

impl Settings {
    /// The theme to draw with (theme::apply's id): the modern interface's or the classic one's.
    pub fn look(&self) -> usize {
        if self.modern { theme::MODERN + self.modern_theme } else { self.theme }
    }

    /// As --interface gives it: modern or classic, and a theme of that interface by name.
    pub fn set_interface(&mut self, spec: &str) {
        let mut parts = spec.split(',').map(str::trim);
        self.modern = !parts.next().is_some_and(|i| i.eq_ignore_ascii_case("classic"));
        if let Some(name) = parts.next() {
            let names: &[&str] = if self.modern { &theme::MODERN_NAMES } else { &theme::NAMES };
            if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(name)) {
                *(if self.modern { &mut self.modern_theme } else { &mut self.theme }) = i;
            }
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            modern: true,
            theme: theme::DARK,
            // a first start: the modern interface, Darkest
            modern_theme: theme::MODERN_DARKEST - theme::MODERN,
            font_size: theme::DEFAULT_FONT_SIZE,
            panel_opacity: 1.0,
            window_opacity: 1.0,
            reveal_opened: true,
            check_updates: true,
            threads: 0,
            overview_budget_mb: 1024,
            cache_budget_gb: 0,
            full_res: FullResMode::Hdd,
            export_dir: String::new(),
            ffmpeg: String::new(),
            interpreter: String::new(),
            favorites: Vec::new(),
            palette_recent: Vec::new(),
            styles: Vec::new(),
            saved_views: Vec::new(),
            minimap: false,
            recent_projects: Vec::new(),
            reopen_last: true,
        }
    }
}

/// Where the settings and the layout are kept: %LOCALAPPDATA%\Janus, ~/Library/Application Support/Janus,
/// $XDG_DATA_HOME/janus.
pub fn app_data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Janus"));
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|d| PathBuf::from(d).join("Library/Application Support/Janus"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".local/share")))
        .map(|d| d.join("janus"));
    base.unwrap_or_else(|| std::env::temp_dir().join("Janus"))
}

/// The basemap's setting as kept (not whether it is shown: each session starts without it).
#[derive(Clone, Default)]
pub struct BasemapSetting {
    pub source: Option<String>,
    pub opacity: Option<f32>,
    pub url: Option<String>,
    pub max_zoom: Option<i32>,
    pub tile_size: Option<i32>,
    pub attribution: Option<String>,
}

/// The [Janus][Settings] and [JanusBasemap][Settings] sections of an ini file (the C++'s keys).
pub fn parse(text: &str) -> (Settings, BasemapSetting) {
    let mut s = Settings::default();
    let mut b = BasemapSetting::default();
    let mut section = "";
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('[') {
            section = match line {
                "[Janus][Settings]" => "janus",
                "[JanusBasemap][Settings]" => "basemap",
                _ => "",
            };
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let int = || v.trim().parse::<i64>().ok();
        match (section, k) {
            ("janus", "FullResCache") => {
                s.full_res = match int().unwrap_or(1).clamp(0, 2) {
                    0 => FullResMode::Off,
                    2 => FullResMode::All,
                    _ => FullResMode::Hdd,
                }
            }
            ("janus", "CacheBudgetGB") => s.cache_budget_gb = int().unwrap_or(0).clamp(0, 1 << 20) as u64,
            // up to 0.38 the full-resolution caches alone had a budget: one changed by hand is kept, the old
            // default (64 GB) becomes automatic
            ("janus", "FullResBudgetGB") if int() != Some(64) => {
                s.cache_budget_gb = int().unwrap_or(0).clamp(8, 1 << 20) as u64
            }
            ("janus", "OverviewBudgetMB") => s.overview_budget_mb = int().unwrap_or(1024).clamp(64, 1 << 20) as i32,
            ("janus", "Threads") => s.threads = int().unwrap_or(0).max(0) as usize,
            ("janus", "FontSize") => {
                s.font_size =
                    int().unwrap_or(13).clamp(i64::from(theme::MIN_FONT_SIZE), i64::from(theme::MAX_FONT_SIZE)) as i32
            }
            ("janus", "RevealOpened") => s.reveal_opened = int() != Some(0),
            ("janus", "CheckUpdates") => s.check_updates = int() != Some(0),
            ("janus", "ExportDir") => s.export_dir = v.to_string(),
            ("janus", "Ffmpeg") => s.ffmpeg = v.to_string(),
            ("janus", "Interpreter") => s.interpreter = v.to_string(),
            ("janus", "PanelOpacity") => s.panel_opacity = int().unwrap_or(100).clamp(10, 100) as f32 / 100.0,
            ("janus", "WindowOpacity") => s.window_opacity = int().unwrap_or(100).clamp(20, 100) as f32 / 100.0,
            ("janus", "Favorite") if !v.is_empty() => s.favorites.push(v.to_string()),
            ("janus", "PaletteRecent") if !v.is_empty() => s.palette_recent.push(v.to_string()),
            ("janus", "SavedView") if !v.is_empty() => s.saved_views.push(v.to_string()),
            ("janus", "Minimap") => s.minimap = int() == Some(1),
            ("janus", "RecentProject") if !v.is_empty() => s.recent_projects.push(v.to_string()),
            ("janus", "ReopenLastSession") => s.reopen_last = int() != Some(0),
            ("janus", "Style") => {
                if let Some((name, style)) = v.split_once('\t') {
                    s.styles.push((name.to_string(), style.to_string()));
                }
            }
            ("janus", "Theme") => {
                if let Some(i) = theme::NAMES.iter().position(|n| *n == v) {
                    s.theme = i;
                }
            }
            ("janus", "Interface") => s.modern = v != "Classic",
            ("janus", "ModernTheme") => {
                if let Some(i) = theme::MODERN_NAMES.iter().position(|n| *n == v) {
                    s.modern_theme = i;
                }
            }
            ("basemap", "Source") => b.source = Some(v.to_string()),
            ("basemap", "Opacity") => b.opacity = v.trim().parse::<f32>().ok().map(|o| o.clamp(0.0, 1.0)),
            ("basemap", "Url") => b.url = Some(v.to_string()),
            ("basemap", "MaxZoom") => b.max_zoom = int().map(|z| z.clamp(0, 22) as i32),
            ("basemap", "TileSize") => b.tile_size = Some(if int() == Some(512) { 512 } else { 256 }),
            ("basemap", "Attribution") => b.attribution = Some(v.to_string()),
            _ => {}
        }
    }
    (s, b)
}

/// The two sections as the C++ writes them.
pub fn to_ini(s: &Settings, b: &crate::basemap::BasemapUi) -> String {
    let full_res = match s.full_res {
        FullResMode::Off => 0,
        FullResMode::Hdd => 1,
        FullResMode::All => 2,
    };
    let mut out = format!(
        "[Janus][Settings]\nFullResCache={full_res}\nCacheBudgetGB={}\nOverviewBudgetMB={}\nThreads={}\nTheme={}\n\
         FontSize={}\nRevealOpened={}\nExportDir={}\nFfmpeg={}\nPanelOpacity={}\nWindowOpacity={}\n",
        s.cache_budget_gb,
        s.overview_budget_mb,
        s.threads,
        theme::NAMES[s.theme],
        s.font_size,
        i32::from(s.reveal_opened),
        s.export_dir,
        s.ffmpeg,
        (s.panel_opacity * 100.0).round() as i32,
        (s.window_opacity * 100.0).round() as i32,
    );
    // the modern interface's keys after the C++'s
    out += &format!(
        "Interface={}\nModernTheme={}\n",
        if s.modern { "Modern" } else { "Classic" },
        theme::MODERN_NAMES[s.modern_theme]
    );
    out += &format!("CheckUpdates={}\n", i32::from(s.check_updates));
    if !s.interpreter.is_empty() {
        out += &format!("Interpreter={}\n", s.interpreter);
    }
    for f in &s.favorites {
        out += &format!("Favorite={f}\n");
    }
    for c in &s.palette_recent {
        out += &format!("PaletteRecent={c}\n");
    }
    for v in &s.saved_views {
        out += &format!("SavedView={v}\n");
    }
    out += &format!("Minimap={}\n", i32::from(s.minimap));
    for p in &s.recent_projects {
        out += &format!("RecentProject={p}\n");
    }
    out += &format!("ReopenLastSession={}\n", i32::from(s.reopen_last));
    for (name, style) in &s.styles {
        out += &format!("Style={name}\t{style}\n");
    }
    out += &format!(
        "\n[JanusBasemap][Settings]\nSource={}\nOpacity={:.2}\nUrl={}\nMaxZoom={}\nTileSize={}\nAttribution={}\n\n",
        b.source, b.opacity, b.url, b.max_zoom, b.tile_size, b.attribution
    );
    out
}

/// The settings kept: this version's file, else the C++'s layout file's sections, else the defaults.
pub fn load() -> (Settings, BasemapSetting) {
    let dir = app_data_dir();
    for name in ["janus-rs.ini", "layout-0.7.ini"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            return parse(&text);
        }
    }
    (Settings::default(), BasemapSetting::default())
}

fn save(text: &str) {
    let dir = app_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let (path, tmp) = (dir.join("janus-rs.ini"), dir.join("janus-rs.ini.tmp"));
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn logical_cores() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

/// The settings' state in the interface.
pub struct SettingsState {
    pub s: Settings,
    pub show: bool,
    /// the overview memory being set (applied with its button)
    budget_ui: i32,
    /// what the ini holds, and when a change was made (saved a second later)
    saved: String,
    changed_at: Option<Instant>,
    /// theme, opacity and font size applied
    applied: Option<(usize, u32, i32)>,
    pub disk: Budget,
    next_disk_check: Option<Instant>,
    /// the last comparison of the settings with the file
    checked_at: Option<Instant>,
    /// --settings-dir: kept elsewhere (tests); None: the user's
    pub no_save: bool,
}

impl SettingsState {
    pub fn new(s: Settings, no_save: bool) -> Self {
        let budget_ui = s.overview_budget_mb;
        // what the file holds: nothing is written until a value changes
        let saved = std::fs::read_to_string(app_data_dir().join("janus-rs.ini")).unwrap_or_default();
        SettingsState {
            s,
            show: false,
            budget_ui,
            saved,
            changed_at: None,
            applied: None,
            disk: Budget {
                bytes: 0,
                cap: 0,
                used: 0,
                overview: 0,
                full_res: 0,
                disk: 0,
                available: 0,
                reserve: 0,
                automatic: true,
            },
            next_disk_check: None,
            checked_at: None,
            no_save,
        }
    }

    pub fn changed(&mut self) {
        self.changed_at = Some(Instant::now());
    }
}

impl State {
    /// Once per frame: the theme and the font if they changed; the settings saved a second after a change; the
    /// disk caches regulated (every minute, every 2 s while Settings or Performance show them).
    pub(crate) fn pump_settings(&mut self, ctx: &egui::Context) {
        let st = &mut self.settings;
        let key = (st.s.look(), st.s.panel_opacity.to_bits(), st.s.font_size);
        if st.applied != Some(key) {
            theme::apply(ctx, st.s.look(), st.s.panel_opacity, st.s.font_size);
            st.applied = Some(key);
            self.map.dirty = true;
            // the cursor's series in the theme's colour
            if let Some(h) = &mut self.series.hover {
                h.color = theme::cursor_series();
            }
        }
        let due = st.changed_at.is_some_and(|t| t.elapsed() > Duration::from_secs(1))
            || st.checked_at.is_none_or(|t| t.elapsed() > Duration::from_secs(2));
        if due {
            st.changed_at = None;
            st.checked_at = Some(Instant::now());
            let text = to_ini(&st.s, &self.basemap.ui);
            if text != st.saved && !st.no_save {
                save(&text);
                st.saved = text;
            }
        }
        self.pump_disk_cache();
    }

    /// The overview's and full-resolution caches against their budget: over it (the disk filling up, or a smaller
    /// budget), the least recently opened series' caches go; if those in use still take too much, their
    /// full-resolution builds stop (Resume once there is room).
    fn pump_disk_cache(&mut self) {
        let st = &mut self.settings;
        if st.next_disk_check.is_some_and(|t| Instant::now() < t) {
            return;
        }
        st.next_disk_check = Some(Instant::now() + Duration::from_secs(if st.show { 2 } else { 60 }));
        let dir = janus_cache::overview::default_cache_dir();
        let setting = st.s.cache_budget_gb << 30;
        st.disk = disk::budget(&dir, setting);
        if st.disk.used <= st.disk.bytes {
            return;
        }
        disk::prune(&dir, st.disk.bytes);
        st.disk = disk::budget(&dir, setting);
        if st.disk.used <= st.disk.bytes {
            return;
        }
        let gb = (1u64 << 30) as f64;
        let why = if st.disk.by_disk() {
            format!(
                "stopped: the disk has {:.1} GB free and Janus keeps {:.0} GB of it free",
                st.disk.available as f64 / gb,
                st.disk.reserve as f64 / gb
            )
        } else {
            format!(
                "stopped: the open series' caches take {:.1} GB, more than the budget ({:.0} GB)",
                st.disk.used as f64 / gb,
                st.disk.bytes as f64 / gb
            )
        };
        for s in self.session.iter().chain(self.layers.iter().filter_map(|l| l.session.as_ref())) {
            if s.s.full_res.building() {
                s.s.full_res.stop_because(&why);
            }
        }
    }

    /// The processing threads, the cache budget and the full-resolution mode to every open series.
    fn apply_session_settings(&mut self) {
        let settings = self.session_settings();
        for s in self.session.iter().chain(self.layers.iter().filter_map(|l| l.session.as_ref())) {
            s.s.apply_settings(&settings);
        }
        self.settings.next_disk_check = None;
    }

    /// The Settings window (File > Settings..., Ctrl+,).
    pub(crate) fn settings_ui(&mut self, ctx: &egui::Context) {
        if !self.settings.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Settings", ctx, 480.0)
            .open(&mut open)
            .collapsible(false)
            .max_width(720.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.settings_contents(ui)));
        self.settings.show = open;
    }

    fn settings_contents(&mut self, ui: &mut Ui) {
        // what is laid out never wider than the window: a widget wider than the room it measured made the window
        // grow, and the next frame's room with it, up to the screen's width
        let w = ui.available_width();
        ui.set_max_width(w);
        section(ui, "Interface");
        let st = &mut self.settings;
        let mut changed = false;
        // the interface: two halves of one row, as a segmented control
        ui.horizontal(|ui| {
            let half = egui::vec2((w - ui.spacing().item_spacing.x) / 2.0, ui.spacing().interact_size.y);
            for (modern, name, tip) in [
                (
                    true,
                    "Modern",
                    "The image editors' look (Photoshop, Lightroom): panels as cards on a darker ground, rounded \
                     controls, a proportional font (Inter), four greys from darkest to light",
                ),
                (false, "Classic", "Dear ImGui's look, as the C++ version draws it: its pixel font and seven themes"),
            ] {
                let on = st.s.modern == modern;
                if ui.add_sized(half, egui::Button::new(name).selected(on)).on_hover_text(tip).clicked() && !on {
                    st.s.modern = modern;
                    changed = true;
                }
            }
        });
        if st.s.modern {
            changed |= modern_themes(ui, w, &mut st.s.modern_theme);
        } else {
            egui::ComboBox::from_id_salt("theme")
                .width(w)
                .selected_text(theme::NAMES[st.s.theme])
                .show_ui(ui, |ui| {
                    for (i, n) in theme::NAMES.iter().enumerate() {
                        changed |= ui.selectable_value(&mut st.s.theme, i, *n).changed();
                    }
                })
                .response
                .on_hover_text(
                    "Dark: ImGui's dark colours (the default). Light: dark text on light panels.\nClassic: ImGui's \
                     original colours. Janus: the program's own colours\n(blue on navy panels, orange accents). \
                     Studio: mid grey with dark text\nand dark chart displays. Graphite: dark grey with a yellow \
                     accent.\nMatrix: phosphor green on black.",
                );
        }
        ui.weak("The map, its colour bars and exported figures keep their colours.");
        let mut panel = (st.s.panel_opacity * 100.0).round() as i32;
        fit_slider(ui, w, SLIDER_TEXT);
        if ui
            .add(Slider::new(&mut panel, 10..=100).custom_formatter(|v, _| format!("panels' background: {v}% opaque")))
            .on_hover_text(
                "Lower it to see the desktop behind the panels: their backgrounds and the map's\nempty area become \
                 translucent; text, chart lines, controls and the map's data stay\nopaque. Takes effect at the next \
                 start when it was opaque at this one.",
            )
            .changed()
        {
            st.s.panel_opacity = panel as f32 / 100.0;
            changed = true;
        }
        let mut font = st.s.font_size;
        fit_slider(ui, w, SLIDER_TEXT);
        if ui
            .add(
                Slider::new(&mut font, theme::MIN_FONT_SIZE..=theme::MAX_FONT_SIZE)
                    .custom_formatter(|v, _| format!("Font size: {v} px")),
            )
            .on_hover_text(
                "Size of the interface text (and of the labels on the map). 13 px: ImGui's\npixel font as the C++ \
                 version draws it (the classic interface), Photoshop's panels' text (the modern one).",
            )
            .changed()
        {
            st.s.font_size = font;
            changed = true;
        }
        if st.s.font_size != theme::DEFAULT_FONT_SIZE
            && ui
                .add_sized([w, 20.0], egui::Button::new(format!("Default font size ({} px)", theme::DEFAULT_FONT_SIZE)))
                .clicked()
        {
            st.s.font_size = theme::DEFAULT_FONT_SIZE;
            changed = true;
        }
        changed |= ui
            .checkbox(&mut st.s.reveal_opened, "Files panel follows what is opened")
            .on_hover_text(
                "Opening a series, a file or a folder expands the Files panel down to it\nand scrolls there.",
            )
            .changed();
        changed |= ui
            .checkbox(&mut st.s.reopen_last, "Reopen the last session at start")
            .on_hover_text(
                "The layers, styles, pins, ROI and view kept when Janus closes (and every minute),\nopened again \
                 when it starts without files (its project, if it was one, named in the title).",
            )
            .changed();
        changed |= ui
            .checkbox(&mut st.s.check_updates, "Check for updates when Janus starts")
            .on_hover_text(
                "One request to GitHub at the start: a button on the menu bar when there is a newer version\n\
                 (Help > Check for updates installs it).",
            )
            .changed();

        section(ui, "Processing");
        let cores = logical_cores();
        let mut n = if st.s.threads > 0 { st.s.threads } else { janus_session::default_processing_threads() };
        let mut threads_changed = false;
        fit_slider(ui, w, SLIDER_TEXT);
        if ui
            .add(
                Slider::new(&mut n, 1..=cores)
                    .custom_formatter(move |v, _| format!("Processing threads: {v} of {cores}")),
            )
            .on_hover_text(
                "CPU threads the processing may use: Janus' readers (overview, exact series,\nROI, detail tiles), \
                 the full-resolution cache and exports. Fewer leaves\ncores free for other programs. Default: every \
                 logical core but 2.",
            )
            .changed()
        {
            st.s.threads = n;
            threads_changed = true;
        }
        let def = janus_session::default_processing_threads();
        if st.s.threads > 0
            && st.s.threads != def
            && ui.add_sized([w, 20.0], egui::Button::new(format!("Default ({def}: every core but 2)"))).clicked()
        {
            st.s.threads = 0;
            threads_changed = true;
        }
        if let Some(s) = &self.session {
            let r = ui.weak(format!(
                "Active layer: {} background readers + {} interactive{}",
                s.s.bg().threads(),
                s.s.fg().threads(),
                if s.s.rotational { " (HDD: one reader)" } else { "" }
            ));
            if s.s.rotational {
                r.on_hover_text(
                    "A spinning disk is read by one thread whatever this setting:\ntwo readers were measured 4x \
                     slower (the head jumps between files).",
                );
            }
        }

        section(ui, "Memory and caches");
        let st = &mut self.settings;
        fit_slider(ui, w, SLIDER_TEXT);
        ui.add(
            Slider::new(&mut st.budget_ui, 128..=8192)
                .logarithmic(true)
                .custom_formatter(|v, _| format!("Overview memory: {v} MB")),
        )
        .on_hover_text(
            "Memory for the cube overview (on the GPU). Larger = more resolution without tiles,\nbut slower to build.",
        );
        let mut reopen = false;
        if st.budget_ui != st.s.overview_budget_mb {
            let label = if self.session.is_some() { "Apply (reopens the active layer)" } else { "Apply" };
            if ui.add_sized([w, 20.0], egui::Button::new(label)).clicked() {
                st.s.overview_budget_mb = st.budget_ui;
                self.opts.budget_mb = 0; // the --budget of this run gives way to the setting
                changed = true;
                reopen = true;
            }
        } else if self.opts.budget_mb > 0 {
            ui.weak(format!("--budget {} MB for this run", self.opts.budget_mb));
        }
        // the disk cache budget: automatic or by hand, what it is now and why, what the caches take
        let st = &mut self.settings;
        let gb = (1u64 << 30) as f64;
        let b = st.disk;
        let mut automatic = st.s.cache_budget_gb == 0;
        let mut budget_changed = false;
        if ui
            .checkbox(&mut automatic, "Disk cache budget: automatic")
            .on_hover_text(
                "The overview and full-resolution caches share it. Automatic: a tenth of the\ndisk holding the cache \
                 folder, 8 to 256 GB. Either way a tenth of the disk\n(10 to 100 GB) stays free: as the disk fills \
                 up the budget shrinks, and the\nleast recently opened series leave the cache first.",
            )
            .changed()
        {
            st.s.cache_budget_gb = if automatic { 0 } else { ((b.cap as f64 / gb).round() as u64).max(8) };
            budget_changed = true;
        }
        if automatic {
            if b.disk > 0 {
                ui.weak(format!(
                    "{:.0} GB: a tenth of the {:.0} GB disk (8 to 256 GB)",
                    b.cap as f64 / gb,
                    b.disk as f64 / gb
                ));
            } else {
                ui.weak(format!("{:.0} GB (the disk's size is unknown)", b.cap as f64 / gb));
            }
        } else {
            let mut v = st.s.cache_budget_gb;
            fit_slider(ui, w, SLIDER_TEXT);
            if ui
                .add(
                    Slider::new(&mut v, 8..=2048)
                        .logarithmic(true)
                        .custom_formatter(|v, _| format!("Disk cache budget: {v} GB")),
                )
                .changed()
            {
                st.s.cache_budget_gb = v.max(8);
                budget_changed = true;
            }
        }
        if b.by_disk() {
            ui.colored_label(
                theme::warning(),
                format!(
                    "Now {:.1} GB: the disk has {:.1} GB free, {:.0} GB of it kept free",
                    b.bytes as f64 / gb,
                    b.available as f64 / gb,
                    b.reserve as f64 / gb
                ),
            );
        }
        let dir = janus_cache::overview::default_cache_dir();
        ui.weak(format!(
            "In use: {:.1} GB (overviews {:.1}, full resolution {:.1})",
            b.used as f64 / gb,
            b.overview as f64 / gb,
            b.full_res as f64 / gb
        ))
        .on_hover_text(dir.to_string_lossy());
        if ui
            .add_sized([w, 20.0], egui::Button::new("Clear overview cache"))
            .on_hover_text(format!("{:.1} GB in {} (the open series' are kept)", b.overview as f64 / gb, dir.display()))
            .clicked()
        {
            clear_overview_cache(&dir);
            st.next_disk_check = None;
        }
        // the full-resolution cache
        let mut mode = st.s.full_res;
        const MODES: [(FullResMode, &str); 3] = [
            (FullResMode::Off, "Full-resolution cache: on demand"),
            (FullResMode::Hdd, "Full-resolution cache: series on an HDD"),
            (FullResMode::All, "Full-resolution cache: every series"),
        ];
        let mut mode_changed = false;
        egui::ComboBox::from_id_salt("full res")
            .width(w)
            .selected_text(MODES.iter().find(|m| m.0 == mode).map_or("", |m| m.1))
            .show_ui(ui, |ui| {
                for (m, label) in MODES {
                    mode_changed |= ui.selectable_value(&mut mode, m, label).changed();
                }
            })
            .response
            .on_hover_text(
                "A copy of each series at full resolution in the cache folder (~1.4x smaller\nthan raw floats, \
                 lossless), built in the background: exact series, ROI\nand detail tiles read from it in about a \
                 millisecond. On an HDD the\nfirst build shares the overview's read of the files.",
            );
        if ui
            .add_sized([w, 20.0], egui::Button::new("Clear full-resolution cache"))
            .on_hover_text(format!("{:.1} GB in {} (series open now are kept)", b.full_res as f64 / gb, dir.display()))
            .clicked()
        {
            janus_cache::FullResCache::clear(&dir);
            st.next_disk_check = None;
        }
        if mode_changed {
            st.s.full_res = mode;
            changed = true;
            // the open series too: their builds stopped or started
            for s in self.session.iter().chain(self.layers.iter().filter_map(|l| l.session.as_ref())) {
                let want = mode == FullResMode::All || (mode == FullResMode::Hdd && s.s.rotational);
                if want && !s.s.full_res.complete() {
                    s.s.build_full_res();
                } else if !want {
                    s.s.full_res.stop();
                }
            }
        }

        let ffmpeg = self.settings.s.ffmpeg.clone();
        self.ffmpeg_settings_ui(ui);
        changed |= ffmpeg != self.settings.s.ffmpeg;

        section(ui, "Validation");
        let st = &mut self.settings;
        let who = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
        changed |= ui
            .add(
                egui::TextEdit::singleline(&mut st.s.interpreter)
                    .hint_text(format!("Interpreter: {who}"))
                    .desired_width(w),
            )
            .on_hover_text(
                "The name your labels of a validation project's points are kept under (by default\nyour login): with \
                 several interpreters, each one's labels, their agreement and\nthe disagreements to settle",
            )
            .changed();

        if changed || threads_changed || budget_changed {
            self.settings.changed();
        }
        if threads_changed || budget_changed {
            self.apply_session_settings();
        }
        if reopen && self.opening.is_none() && self.active < self.layers.len() {
            let ctx = ui.ctx().clone();
            let inputs = self.layers[self.active].inputs.clone();
            self.remove_layer(self.active, &ctx);
            self.open_inputs(inputs, true, &ctx);
        }
    }

    /// The Performance panel's sections: the full-resolution cache of the active layer, the disk caches.
    pub(crate) fn cache_perf_ui(&mut self, ui: &mut Ui) {
        let w = ui.available_width();
        if let Some(s) = &self.session {
            let c = &s.s.full_res;
            let n = s.info.t();
            section(ui, "Full-resolution cache");
            let err = c.error();
            if !c.started() {
                if !err.is_empty() {
                    ui.colored_label(theme::error(), format!("Not built: {err}"));
                } else {
                    ui.weak(if s.s.rotational {
                        "Off: exact series are read from the HDD"
                    } else {
                        "Off (the series is on an SSD)"
                    });
                }
                if ui
                    .add_sized([w, 20.0], egui::Button::new("Build it now"))
                    .on_hover_text(format!(
                        "Copies the series at full resolution to the cache folder (about {:.1} GB),\nin the background: \
                         exact series, ROI and detail tiles are then read\nfrom there in about a millisecond, even \
                         with the files on an HDD.",
                        c.estimated_bytes() as f64 / 1e9
                    ))
                    .clicked()
                {
                    s.s.build_full_res();
                }
            } else {
                if c.complete() {
                    ui.label(format!("Complete: {n} dates, {:.2} GB ({})", c.bytes() as f64 / 1e9, c.codec()));
                    if c.build_seconds() > 0.0 {
                        ui.label(format!(
                            "Built in {:.0} s ({} dates read with the overview)",
                            c.build_seconds(),
                            c.dates_with_overview()
                        ));
                    }
                } else {
                    let f = c.dates_done() as f32 / n.max(1) as f32;
                    ui.add(egui::ProgressBar::new(f).text(format!(
                        "{}/{n} dates, {:.2} of ~{:.1} GB",
                        c.dates_done(),
                        c.bytes() as f64 / 1e9,
                        c.estimated_bytes() as f64 / 1e9
                    )));
                    if !err.is_empty() {
                        ui.colored_label(theme::error(), &err);
                    } else if c.building() {
                        ui.weak("Building in the background; cached dates are used already");
                    }
                    // also after a stop for room: resumes if the dates fit now
                    if !c.building() && !c.read_only() && ui.add_sized([w, 20.0], egui::Button::new("Resume")).clicked()
                    {
                        s.s.build_full_res();
                    }
                }
                if c.series_hits() > 0 {
                    ui.label(format!(
                        "Series from the cache: {:.2} ms ({} reads)",
                        c.last_series_ms(),
                        c.series_hits()
                    ));
                }
                if c.avg_window_ms() > 0.0 {
                    ui.label(format!("ROI / tile blocks: {:.2} ms per date", c.avg_window_ms()));
                }
            }
        }
        let b = self.settings.disk;
        let gb = (1u64 << 30) as f64;
        section(ui, "Disk cache");
        ui.label(format!(
            "{:.1} of {:.1} GB ({})",
            b.used as f64 / gb,
            b.bytes as f64 / gb,
            if b.automatic { "automatic" } else { "set in Settings" }
        ))
        .on_hover_text(janus_cache::overview::default_cache_dir().to_string_lossy());
        ui.label(format!(
            "Overviews {:.1} GB, full resolution {:.1} GB",
            b.overview as f64 / gb,
            b.full_res as f64 / gb
        ));
        if b.disk > 0 {
            ui.label(format!(
                "Disk: {:.0} of {:.0} GB free ({:.0} GB kept free)",
                b.available as f64 / gb,
                b.disk as f64 / gb,
                b.reserve as f64 / gb
            ));
        }
        if b.by_disk() {
            ui.colored_label(
                theme::warning(),
                format!("Budget shrunk from {:.0} GB: the disk is filling up", b.cap as f64 / gb),
            );
        }
    }
}

/// The overviews in the cache folder, but for the open series'.
fn clear_overview_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == disk::OVERVIEW_EXT) && !disk::in_use(&p) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// A section's title with a line after it (ImGui's SeparatorText).
/// The modern themes as swatches, as Photoshop's Color Theme: each a panel of its grey with its text; true when
/// another was picked.
fn modern_themes(ui: &mut Ui, w: f32, chosen: &mut usize) -> bool {
    let n = theme::MODERN_NAMES.len();
    let gap = ui.spacing().item_spacing.x;
    let size = egui::vec2((w - gap * (n - 1) as f32) / n as f32, 34.0);
    let mut changed = false;
    ui.horizontal(|ui| {
        for (i, name) in theme::MODERN_NAMES.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            let (panel, text, bar) = theme::swatch(theme::MODERN + i);
            let p = ui.painter();
            p.rect_filled(rect, 6, panel);
            p.rect_filled(
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 7.0)),
                egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 },
                bar,
            );
            p.text(
                rect.center() + egui::vec2(0.0, 3.0),
                egui::Align2::CENTER_CENTER,
                *name,
                egui::FontId::proportional(11.5),
                text,
            );
            let stroke = if *chosen == i {
                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill)
            } else if resp.hovered() {
                egui::Stroke::new(1.0, ui.visuals().widgets.hovered.fg_stroke.color)
            } else {
                egui::Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color)
            };
            p.rect_stroke(rect, 6, stroke, egui::StrokeKind::Outside);
            if resp.on_hover_text(format!("{name} theme")).clicked() && *chosen != i {
                *chosen = i;
                changed = true;
            }
        }
    });
    changed
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.strong(title);
        ui.add(egui::Separator::default().horizontal());
    });
}

/// The longest of the sliders' values: every slider of the window as long, their value boxes beside them.
const SLIDER_TEXT: &str = "panels' background: 100% opaque";

/// The slider's width for a row of `w`: what its value box (`widest`, its longest text) leaves.
pub(crate) fn fit_slider(ui: &mut Ui, w: f32, widest: &str) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text = ui.painter().layout_no_wrap(widest.to_string(), font, egui::Color32::WHITE).size().x;
    let pad = 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x + 2.0;
    ui.spacing_mut().slider_width = (w - text - pad).max(60.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++'s settings are read as written, and written back the same.
    #[test]
    fn ini_round_trip() {
        let text = "[Window][Map]\nPos=0,0\n\n[Janus][Settings]\nFullResCache=2\nCacheBudgetGB=0\nOverviewBudgetMB=2048\n\
                    Threads=6\nTheme=Janus\nFontSize=16\nRevealOpened=0\nExportDir=C:\\out\nFfmpeg=\nPanelOpacity=80\n\
                    WindowOpacity=100\nFavorite=D:\\data\n\n[JanusBasemap][Settings]\nSource=osm\nOpacity=0.50\nUrl=\n\
                    MaxZoom=19\nTileSize=512\nAttribution=x\n";
        let (s, b) = parse(text);
        assert_eq!((s.theme, s.font_size, s.threads, s.overview_budget_mb), (theme::JANUS, 16, 6, 2048));
        assert_eq!(s.full_res, FullResMode::All);
        assert!(!s.reveal_opened);
        assert_eq!((s.panel_opacity, s.export_dir.as_str(), s.favorites.len()), (0.8, "C:\\out", 1));
        assert_eq!((b.source.as_deref(), b.opacity, b.tile_size), (Some("osm"), Some(0.5), Some(512)));
        let mut bm = crate::basemap::BasemapUi::default();
        (bm.source, bm.opacity, bm.tile_size, bm.attribution) = ("osm".into(), 0.5, 512, "x".into());
        let (s2, b2) = parse(&to_ini(&s, &bm));
        assert_eq!((s2.theme, s2.font_size, s2.threads, s2.full_res), (s.theme, s.font_size, s.threads, s.full_res));
        assert_eq!((b2.source, b2.tile_size), (Some("osm".into()), Some(512)));
        // the C++'s settings have no interface: the modern one, its Darkest theme; the classic one kept when chosen
        assert_eq!(s.look(), theme::MODERN_DARKEST);
        let classic = Settings { modern: false, modern_theme: 3, ..s };
        let (s3, _) = parse(&to_ini(&classic, &bm));
        assert_eq!((s3.modern, s3.modern_theme, s3.look()), (false, 3, theme::JANUS));
        // the check for updates on unless turned off (the C++'s settings have no key for it)
        assert!(s.check_updates);
        let (s4, _) = parse(&to_ini(&Settings { check_updates: false, ..s3 }, &bm));
        assert!(!s4.check_updates);
    }
}
