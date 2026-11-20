//! `--selftest-ui` stages 385-388: Tools > Raster calculator in a window of its own (an egui viewport). 385 opens the
//! calculator and detaches it; 386 checks it is drawn in a viewport other than the main window's (its expression
//! checked there, by the window's own frames) and runs it; 387 waits for the result's layer and brings the calculator
//! back into the main window; 388 checks it is drawn there again and puts everything back. Then 389-392: every other
//! window of Janus (the list in `entries`), one after the other: 389 opens it, 390 checks it is drawn in the main
//! window and detaches it, 391 checks it is drawn in a viewport of its own (not the root's) with its content and
//! brings it back, 392 checks it is drawn in the main window again and closes it. Then 299, the end.

use std::sync::Mutex;

use egui::Id;
use std::time::Instant;

use super::UiTest;
use crate::app::State;

/// What the stages share: the layers and the active one before the run.
static KEPT: Mutex<Option<(usize, usize)>> = Mutex::new(None);

/// The window 389-392 are at (an index in `entries`).
static AT: Mutex<usize> = Mutex::new(0);

/// A window of Janus that can be detached: how it is found, opened and closed.
struct Entry {
    name: &'static str,
    /// the window's egui id
    id: fn() -> Id,
    open: fn(&mut State),
    close: fn(&mut State),
}

fn entries() -> Vec<Entry> {
    macro_rules! flag {
        ($name:expr, $id:expr, $($f:ident).+) => {
            Entry { name: $name, id: || Id::new($id), open: |s| s.$($f).+ = true, close: |s| s.$($f).+ = false }
        };
    }
    vec![
        flag!("Settings", "Settings", settings.show),
        flag!("Welcome", "welcome window", welcome.show),
        flag!("Climate data", "climate window", climate.show),
        flag!("Search STAC catalog", "stac window", stac.show),
        flag!("Add web layer", "Add web layer", web.ui.show),
        flag!("Analysis-ready data", "ard window", ard.open),
        flag!("Classification", "ml window", ml.open),
        flag!("Time series analysis", "tsa window", tsa.open),
        flag!("Regularize series", "Regularize series", reg.show),
        flag!("Speckle filter", "Speckle filter", speckle.show),
        flag!("Harmonize sensors", "Harmonize sensors", harmonize.show),
        flag!("Polygonize", "Polygonize", poly.show),
        flag!("Field delineation", "Field delineation", fields.show),
        flag!("Reproject, resample, clip", "Reproject, resample, clip", warp.show),
        flag!("Rasterize", "Rasterize on the series' grid", raster.show),
        flag!("Zonal statistics", "Zonal statistics over time", zonal.show),
        flag!("Profiles of every line", "Profiles of every line", profiles.show),
        flag!("Tasks", "Tasks", zeit.show_tasks),
        flag!("Log", "Log", zeit.log.show),
        flag!("Python history", "python history window", history.show),
        flag!("Build series", "Build series", build.show),
        flag!("Save series as", "Save series as", save.show),
        flag!("Exports", "Exports", show_exports),
        flag!("Janus updates", "Janus updates", update.open),
        flag!("Chips", "Chips", val.show_chips),
        flag!("About", "about", about_open),
        // a tool's window (the generic one of the Tools menu) and Tune on samples
        Entry {
            name: "A Zeit tool",
            id: || Id::new(("zeit tool", &"bfast_lite".to_string())),
            open: |s| {
                if let Some(t) = janus_zeit::manifest().tool("bfast_lite") {
                    s.set_tool_open(t, true);
                }
            },
            close: |s| {
                if let Some(t) = janus_zeit::manifest().tool("bfast_lite") {
                    s.set_tool_open(t, false);
                }
            },
        },
        Entry {
            name: "Tune on samples",
            id: || Id::new("tune window"),
            open: |s| {
                if let Some(t) = janus_zeit::manifest().tool("bfast_lite") {
                    s.open_tune(t);
                }
            },
            close: |s| s.tune.open = false,
        },
        Entry {
            name: "Provenance",
            id: || Id::new("Provenance"),
            open: |s| s.prov.shown = Some((std::path::PathBuf::new(), crate::provenance::Provenance::default())),
            close: |s| s.prov.shown = None,
        },
        Entry {
            name: "Save figure",
            id: || Id::new("figure dialog"),
            open: |s| s.open_figure(crate::figure_ui::Source::Chart),
            close: |s| s.export.figure.source = None,
        },
        Entry {
            name: "Crash notice",
            id: || Id::new("crash notice"),
            open: |s| {
                s.crash_notice.closed = Some(crate::crash::Closed {
                    when: 0,
                    text: "self-test".into(),
                    file: std::path::PathBuf::from("report.txt"),
                    markers: Vec::new(),
                    earlier: 0,
                })
            },
            close: |s| s.crash_notice.closed = None,
        },
    ]
}

impl State {
    pub(super) fn test_calc_detach(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
            st.frames = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        match stage {
            385 => {
                let (ls, at) = self.calc_layers();
                let a = ls[at.iter().position(|&k| k == self.active)?].alias.clone();
                *KEPT.lock().unwrap() = Some((self.layers.len(), self.active));
                self.calc.show = true;
                crate::panels::set_detached(ctx, State::calc_window_id(), false);
                self.calc.text = format!("max({a}) + 1");
                self.calc.out_name = "Detached calculator".into();
                self.calc.name_typed = true;
                self.calc.primary = None;
                self.calc.cog = false;
                self.calc.live.on = false;
                self.calc.live.chart = false;
                crate::panels::set_detached(ctx, State::calc_window_id(), true);
                next(st, "Tools > Raster calculator: opened and detached into a window of its own");
            }
            386 => {
                st.frames += 1;
                if st.frames < 6 {
                    return None;
                }
                match crate::panels::detached_viewport(ctx, State::calc_window_id()) {
                    Some((v, _)) if v != egui::ViewportId::ROOT => {}
                    other => return fail(&format!("the calculator should be drawn in its own viewport: {other:?}")),
                }
                if !self.calc.check.as_ref().is_some_and(|c| c.result.is_ok()) {
                    return fail("the calculator's expression should be checked (and fine) in its window");
                }
                self.calc_run(ctx);
                next(st, "the calculator is in its own viewport and runs max + 1 from there");
            }
            387 => {
                let (before, _) = (*KEPT.lock().unwrap())?;
                if self.opening.is_some() || self.layers.len() != before + 1 {
                    if waited > 90.0 {
                        return fail("the detached calculator's result should open as a layer");
                    }
                    return None;
                }
                if crate::panels::detached_viewport(ctx, State::calc_window_id()).is_none() {
                    return fail("the calculator should still be in its viewport while the result opens");
                }
                crate::panels::set_detached(ctx, State::calc_window_id(), false);
                next(st, "the detached calculator made a layer; back into the main window");
            }
            388 => {
                st.frames += 1;
                if st.frames < 6 {
                    return None;
                }
                if crate::panels::detached_viewport(ctx, State::calc_window_id()).is_some()
                    || crate::panels::is_detached(ctx, State::calc_window_id())
                {
                    return fail("the calculator should be in the main window again");
                }
                if !ctx.memory(|m| m.area_rect(egui::Id::new("calculator window"))).is_some_and(|r| r.width() > 1.0) {
                    return fail("the calculator's window should be drawn in the main window again");
                }
                let (before, _) = (*KEPT.lock().unwrap())?;
                self.calc.show = false;
                for i in (before..self.layers.len()).rev() {
                    self.remove_layer(i, ctx);
                }
                *KEPT.lock().unwrap() = None;
                next(st, "the calculator is back in the main window; everything put back");
                *AT.lock().unwrap() = 0;
                st.stage = 389;
            }
            389 => {
                let i = *AT.lock().unwrap();
                let list = entries();
                let Some(e) = list.get(i) else {
                    st.stage = 400; // MNF, CVA, iMAD and the normalization (400-406, ui_test_envi.rs), then 299
                    return None;
                };
                (e.open)(self);
                next(st, &format!("{}: opened", e.name));
            }
            390 => {
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                let list = entries();
                let e = &list[*AT.lock().unwrap()];
                let id = (e.id)();
                if !crate::panels::drawn_last_frame(ctx, id) {
                    return fail(&format!("{} should be drawn in the main window (its id {id:?})", e.name));
                }
                if crate::panels::detached_viewport(ctx, id).is_some() {
                    return fail(&format!("{} should not be in a viewport before it is detached", e.name));
                }
                crate::panels::set_detached(ctx, id, true);
                next(st, &format!("{}: drawn in the main window, detached", e.name));
            }
            391 => {
                st.frames += 1;
                if st.frames < 5 {
                    return None;
                }
                let list = entries();
                let e = &list[*AT.lock().unwrap()];
                let id = (e.id)();
                match crate::panels::detached_viewport(ctx, id) {
                    Some((v, size)) if v != egui::ViewportId::ROOT && size.x > 1.0 && size.y > 1.0 => {}
                    other => {
                        return fail(&format!(
                            "{} should be drawn with its content in a viewport of its own: {other:?}",
                            e.name
                        ));
                    }
                }
                crate::panels::set_detached(ctx, id, false);
                next(st, &format!("{}: drawn in a viewport of its own, brought back", e.name));
            }
            392 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let list = entries();
                let mut at = AT.lock().unwrap();
                let e = &list[*at];
                let id = (e.id)();
                if crate::panels::is_detached(ctx, id)
                    || crate::panels::detached_viewport(ctx, id).is_some()
                    || !crate::panels::drawn_last_frame(ctx, id)
                {
                    return fail(&format!("{} should be drawn in the main window again", e.name));
                }
                (e.close)(self);
                *at += 1;
                drop(at);
                next(st, &format!("{}: back in the main window, closed", e.name));
                st.stage = 389;
            }
            _ => {}
        }
        None
    }
}
