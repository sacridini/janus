//! The font sizes' tour (`--shots layout IN OUT`): the series IN opened with a pin and an ROI, then at each font
//! size of the Settings (JANUS_AUDIT_FONTS, else 10 to 28 px) every panel, window, tab and menu shown one at a time,
//! and what each frame painted read for text that does not fit its place (layout_audit.rs). A screenshot of each
//! place with such texts (OUT/<font>-<place>.png, OUT/<font>-<place>-all.png with every place's when
//! JANUS_AUDIT_ALL is set) and a report (OUT/report.txt): the place, the font, the text, what is wrong and by how
//! many points. Run with --interface classic and modern: their fonts differ.

use std::fmt::Write as _;

use egui::{Context, Event, PointerButton};
use janus_cube::BandSelection;

use crate::app::{State, Tab};
use crate::layout_audit::{self, Finding};
use crate::shots::Shots;

/// A place of the interface.
#[derive(Clone, Debug)]
pub(crate) enum Place {
    /// the window as it opens: Layers, Display, the map, the time series, the statistics
    Main,
    /// a panel of the dock brought to the front
    Dock(Tab),
    /// a command of the palette (by its key: category/label), run again to close what it opened
    Command(String, bool),
    /// a Zeit tool's window
    Tool(String),
    /// a tab of Analysis
    Analysis(usize, &'static str),
    /// a tab of Classification
    Classification(usize, &'static str),
    /// the Settings window
    Settings,
    /// the command palette, a word typed
    Palette,
    /// a menu of the menu bar, open
    Menu(&'static str),
    /// a window opened by a field of the state
    Window(&'static str),
}

impl Place {
    fn name(&self) -> String {
        let clean = |s: &str| -> String {
            s.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
        };
        match self {
            Place::Main => "main".into(),
            Place::Dock(t) => format!("dock_{}", clean(&format!("{t:?}"))),
            Place::Command(k, _) => format!("cmd_{}", clean(k)),
            Place::Tool(id) => format!("tool_{id}"),
            Place::Analysis(_, n) => format!("analysis_{}", clean(n)),
            Place::Classification(_, n) => format!("classification_{}", clean(n)),
            Place::Settings => "settings".into(),
            Place::Palette => "palette".into(),
            Place::Menu(m) => format!("menu_{}", clean(m)),
            Place::Window(w) => format!("window_{w}"),
        }
    }
}

/// Where the tour is.
#[derive(Default)]
pub(crate) struct LayoutTour {
    fonts: Vec<i32>,
    places: Vec<Place>,
    font: usize,
    place: usize,
    /// 0: open the place, 1: wait for it to settle, 2: read its frames
    phase: u8,
    frames: usize,
    since: Option<std::time::Instant>,
    report: String,
    /// texts that do not fit: (font, place, finding)
    found: Vec<(i32, String, Finding)>,
    /// those of the main window at the font size of this round: left out of the other places'
    main: std::collections::HashSet<(crate::layout_audit::Kind, String, String)>,
}

impl State {
    pub(crate) fn shots_layout(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        match sh.stage {
            0 => {
                let input = sh.input.clone();
                self.shots_open(&input, BandSelection::default(), ctx);
                sh.next("the series opening");
                None
            }
            1 => {
                if self.opening.is_some() || self.session.is_none() {
                    return None;
                }
                sh.layout = Some((vec![], vec![]));
                let (w, h) = self.session.as_ref().map_or((1, 1), |s| (s.info.width, s.info.height));
                self.add_pin(w / 3, h / 3);
                self.add_pin(w / 2, h / 2);
                self.set_roi_rect([w as i64 / 4, h as i64 / 4, w as i64 / 2, h as i64 / 2]);
                sh.next("a pin and an ROI");
                None
            }
            2 => {
                if !self.shots_quiet() || sh.time() < 3.0 {
                    return None;
                }
                if sh.frames < 40 {
                    return None;
                }
                self.map.fit();
                let fonts = std::env::var("JANUS_AUDIT_FONTS").ok().map_or_else(
                    || vec![10, 13, 16, 20, 24, 28],
                    |s| s.split(',').filter_map(|f| f.trim().parse().ok()).collect(),
                );
                let places = self.layout_places(std::env::var("JANUS_AUDIT_PLACES").ok().as_deref());
                println!("    {} places at {} font sizes: {fonts:?}", places.len(), fonts.len());
                // a model in the Classification window's Apply tab (its area of applicability, its error profile)
                if let Ok(p) = std::env::var("JANUS_AUDIT_MODEL") {
                    match janus_ml::Model::read_header(&p) {
                        Ok(h) => self.ml.model = Some((p, h)),
                        Err(e) => println!("    JANUS_AUDIT_MODEL: {e}"),
                    }
                }
                sh.audit = LayoutTour { fonts, places, ..Default::default() };
                sh.next("the places listed");
                None
            }
            3 => self.layout_step(sh, ctx),
            _ => Some(0),
        }
    }

    /// Every place of the interface the tour shows (those whose name contains one of `only`'s words, if given).
    fn layout_places(&self, only: Option<&str>) -> Vec<Place> {
        let mut v = vec![
            Place::Main,
            Place::Dock(Tab::Files),
            Place::Command("View/Performance panel".into(), true),
            Place::Settings,
            Place::Palette,
        ];
        for m in ["File", "Edit", "View", "Tools", "Validation", "Help"] {
            v.push(Place::Menu(m));
        }
        for (k, n) in crate::analysis::ANALYSIS_TABS {
            v.push(Place::Analysis(k, n));
        }
        for (k, n) in [(0, "Samples"), (1, "Method"), (2, "Model"), (3, "Apply")] {
            v.push(Place::Classification(k, n));
        }
        for w in [
            "calculator",
            "stac",
            "regularize",
            "speckle",
            "warp",
            "build",
            "save",
            "polygonize",
            "areas",
            "fields",
            "export",
            "goto",
            "tasks",
            "log",
            "exports",
            "validation",
            "chips",
            "updates",
            "about",
            "help",
        ] {
            v.push(Place::Window(w));
        }
        for t in &janus_zeit::manifest().tools {
            v.push(Place::Tool(t.id.clone()));
        }
        match only {
            Some(words) => v
                .into_iter()
                .filter(|p| {
                    let n = p.name();
                    words.split(',').any(|w| n.contains(w.trim()))
                })
                .collect(),
            None => v,
        }
    }

    /// Everything a place may have opened, closed.
    fn layout_close_all(&mut self, ctx: &Context) {
        egui::Popup::close_all(ctx);
        for t in &janus_zeit::manifest().tools {
            if self.zeit.tools.get(&t.id).is_some_and(|u| u.open) {
                self.set_tool_open(t, false);
            }
        }
        self.settings.show = false;
        self.palette.open = false;
        self.analysis.show = false;
        self.ml.open = false;
        self.calc.show = false;
        self.stac.show = false;
        self.reg.show = false;
        self.speckle.show = false;
        self.warp.show = false;
        self.build.show = false;
        self.save.show = false;
        self.poly.show = false;
        self.fields.show = false;
        self.export.open = false;
        self.nav.goto_open = false;
        self.zeit.show_tasks = false;
        self.zeit.log.show = false;
        self.show_exports = false;
        self.val.show = false;
        self.val.show_chips = false;
        self.update.open = false;
        self.about_open = false;
        self.help_popup = false;
    }

    /// Shows a place.
    fn layout_open(&mut self, sh: &mut Shots, place: &Place) {
        match place {
            Place::Main => {}
            Place::Dock(t) => sh.focus.push(*t),
            Place::Command(key, _) => self.palette.run_next = Some(key.clone()),
            Place::Tool(id) => {
                if let Some(t) = janus_zeit::manifest().tool(id) {
                    self.set_tool_open(t, true);
                }
            }
            Place::Analysis(k, _) => {
                (self.analysis.show, self.analysis.tab, self.analysis.focus) = (true, *k, true);
            }
            Place::Classification(k, _) => {
                (self.ml.open, self.ml.source, self.ml.arch, self.ml.tab) = (true, 3, crate::ml::FOREST, *k);
                // the spatial folds' settings shown (blocks: the most rows)
                self.ml.cfg.folds.method = janus_ml::folds::Method::Blocks;
            }
            Place::Settings => self.settings.show = true,
            Place::Palette => {
                self.palette.open = true;
                self.palette.set_query("a");
            }
            Place::Menu(_) => {}
            Place::Window(w) => match *w {
                "calculator" => self.calc.show = true,
                "stac" => self.stac.show = true,
                "regularize" => self.reg.show = true,
                "speckle" => self.speckle.show = true,
                "warp" => self.warp.show = true,
                "build" => self.build.show = true,
                "save" => self.save.show = true,
                "polygonize" => {
                    if let Some(id) = self.session.as_ref().map(|s| s.info.id) {
                        self.open_polygonize(crate::contents::Sel::Series(id));
                    }
                }
                "areas" => self.open_areas(None),
                "fields" => self.fields.show = true,
                "export" => self.open_export(0),
                "goto" => self.toggle_goto(),
                "tasks" => self.zeit.show_tasks = true,
                "log" => self.zeit.log.show = true,
                "exports" => self.show_exports = true,
                "validation" => {
                    self.val.show = true;
                    self.val.select_tab = Some(0);
                }
                "chips" => self.val.show_chips = true,
                "updates" => self.update.open = true,
                "about" => self.about_open = true,
                "help" => self.help_popup = true,
                _ => {}
            },
        }
    }

    fn layout_step(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let tour = &mut sh.audit;
        if tour.font >= tour.fonts.len() {
            return Some(self.layout_report(sh));
        }
        let font = tour.fonts[tour.font];
        if tour.place >= tour.places.len() {
            tour.font += 1;
            tour.place = 0;
            return None;
        }
        let place = tour.places[tour.place].clone();
        tour.frames += 1;
        match tour.phase {
            0 => {
                // the font size of this round (applied before the next frame), the place shown
                if tour.place == 0 {
                    println!("    font size {font} px");
                    self.settings.s.font_size = font;
                    let st = &self.settings.s;
                    crate::theme::apply(ctx, st.look(), st.panel_opacity, font);
                }
                self.layout_close_all(ctx);
                self.layout_open(sh, &place);
                let tour = &mut sh.audit;
                (tour.phase, tour.frames, tour.since) = (1, 0, Some(std::time::Instant::now()));
                None
            }
            1 => {
                // a menu opened by a click on its title
                if let Place::Menu(m) = place
                    && tour.frames <= 2
                {
                    if let Some(&r) = sh.menus.get(m) {
                        let c = r.center();
                        sh.events.push(Event::PointerMoved(c));
                        sh.events.push(Event::PointerButton {
                            pos: c,
                            button: PointerButton::Primary,
                            pressed: tour.frames == 1,
                            modifiers: Default::default(),
                        });
                    }
                    return None;
                }
                let waited = tour.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
                // settled: a few frames (the windows take their size in two), the series' reads done (or 4 s)
                if tour.frames < 8 || !self.shots_quiet() && waited < 4.0 {
                    return None;
                }
                layout_audit::start();
                (tour.phase, tour.frames) = (2, 0);
                None
            }
            _ => {
                if tour.frames < 3 {
                    return None;
                }
                let mut found = layout_audit::stop();
                let name = place.name();
                let key = |f: &Finding| (f.kind, f.layer.clone(), f.text.clone());
                if matches!(place, Place::Main) {
                    sh.audit.main = found.iter().map(key).collect();
                } else {
                    found.retain(|f| !sh.audit.main.contains(&key(f)));
                }
                let all = std::env::var_os("JANUS_AUDIT_ALL").is_some();
                if !found.is_empty() || all {
                    sh.shoot(&format!("{font:02}-{name}{}", if found.is_empty() { "-all" } else { "" }), &[]);
                }
                if !found.is_empty() {
                    println!("    {font} px, {name}: {} texts do not fit", found.len());
                }
                let tour = &mut sh.audit;
                tour.found.extend(found.into_iter().map(|f| (font, name.clone(), f)));
                // a palette command that toggles: run again to take it away
                if let Place::Command(key, true) = &place {
                    self.palette.run_next = Some(key.clone());
                }
                (tour.phase, tour.place) = (0, tour.place + 1);
                None
            }
        }
    }

    /// The report: OUT/report.txt, each place's texts that do not fit at each font size. Exit 0 (the tour ran).
    fn layout_report(&mut self, sh: &mut Shots) -> i32 {
        let tour = &mut sh.audit;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Font sizes' layout audit: {} places at {:?} px, {} texts that do not fit\n",
            tour.places.len(),
            tour.fonts,
            tour.found.len()
        );
        // texts cut short at the smallest font size too are cut on purpose (a long name in a list): left out
        let smallest = tour.fonts.iter().copied().min().unwrap_or(0);
        let short: std::collections::HashSet<(String, String)> = tour
            .found
            .iter()
            .filter(|(f, _, x)| *f == smallest && x.kind == crate::layout_audit::Kind::Elided)
            .map(|(_, p, x)| (p.clone(), x.text.clone()))
            .collect();
        tour.found.retain(|(f, p, x)| {
            x.kind != crate::layout_audit::Kind::Elided
                || *f != smallest && !short.contains(&(p.clone(), x.text.clone()))
        });
        tour.found.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)).then(a.2.kind.cmp(&b.2.kind)));
        let mut last = (String::new(), 0);
        for (font, place, f) in &tour.found {
            if (place.clone(), *font) != last {
                let _ = writeln!(out, "{place} at {font} px:");
                last = (place.clone(), *font);
            }
            let other = if f.other.is_empty() { String::new() } else { format!(" (over \"{}\")", f.other) };
            let _ = writeln!(
                out,
                "  {:<17} {:>6.1} pt  \"{}\"{other}  at ({:.0}, {:.0}) {}",
                f.kind.name(),
                f.by,
                f.text,
                f.rect.min.x,
                f.rect.min.y,
                f.layer
            );
        }
        tour.report = out;
        let path = sh.out.join("report.txt");
        let _ = std::fs::create_dir_all(&sh.out);
        match std::fs::write(&path, &sh.audit.report) {
            Ok(()) => println!("    report: {}", path.display()),
            Err(e) => println!("    the report could not be written: {e}"),
        }
        0
    }
}
