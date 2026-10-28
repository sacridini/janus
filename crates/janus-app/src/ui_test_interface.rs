//! `--selftest-ui` stages 180-199: the interface of phases 48-55, on what the validation stages left open (D). 180:
//! the status bar (the cursor's coordinates in longitude and latitude, the scale) and the notices of tasks that end
//! (a short task none, a long one and a failed one each one; a newer version of Janus one, once). 181: the command
//! palette (what a query finds first, a command run from it, the recent ones first). 182: the Layers panel (a blend
//! mode from the palette, a style copied and pasted on another series, a style saved by name and read back, layers
//! grouped, hidden and moved together). 183: navigation (Go to a longitude and latitude, an EPSG x y, a UTM zone; a saved view; the minimap;
//! the lens).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::contents::Sel;
use crate::zeit::{Job, JobState, JobStatus};

/// A task of the Tasks window for the notices' test: running, `seconds` long once ended.
fn fake_job(title: &str) -> Job {
    Job {
        title: title.to_string(),
        tool: "test".into(),
        cube: 0,
        output_dir: String::new(),
        log_path: String::new(),
        started: Instant::now(),
        status: Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.5,
            message: String::new(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        })),
        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        loaded: true,
    }
}

impl State {
    pub(super) fn test_interface(
        &mut self,
        st: &mut UiTest,
        _input: &dyn Fn(usize) -> Option<String>,
        t0: Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
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
        match stage {
            180 => {
                // the status bar: a pixel of the active layer, its coordinates in longitude and latitude
                let info = self.session.as_ref()?.info.clone();
                let (x, y) = (info.width / 2, info.height / 2);
                let geo = info.pixel_to_geo(x as f64 + 0.5, y as f64 + 0.5);
                let Some(lon_lat) = self.status_lon_lat(geo?) else {
                    return fail("the status bar should give the cursor's longitude and latitude");
                };
                if !(-180.0..=180.0).contains(&lon_lat.0) || !(-90.0..=90.0).contains(&lon_lat.1) {
                    return fail(&format!("longitude and latitude out of range: {lon_lat:?}"));
                }
                let Some((scale, ground)) = self.status_scale() else {
                    return fail("the status bar should give the map's scale");
                };
                println!(
                    "    pixel ({x}, {y}) of {}: lon {:.5} lat {:.5}; scale {scale}, {ground}",
                    info.crs_authority, lon_lat.0, lon_lat.1
                );
                // the notices: a task of a second ends without one, a long one and a failed one with one each
                let names = ["short task", "long task", "failing task"];
                let jobs = names.map(fake_job);
                let first = self.zeit.jobs.len();
                self.zeit.jobs.extend(jobs);
                self.pump_notices(ctx);
                for (k, (state, seconds, error)) in
                    [(JobState::Done, 1.0, ""), (JobState::Done, 75.0, ""), (JobState::Failed, 2.0, "no disk")]
                        .into_iter()
                        .enumerate()
                {
                    let mut s = self.zeit.jobs[first + k].status.lock().unwrap();
                    (s.state, s.seconds, s.error) = (state, seconds, error.to_string());
                }
                self.pump_notices(ctx);
                // only these tasks' notices: an earlier stage's task may end now (a slow runner's training)
                let got: Vec<_> =
                    self.notice_titles().into_iter().filter(|(t, _)| names.contains(&t.as_str())).collect();
                self.zeit.jobs.truncate(first);
                let want = [("long task".to_string(), false), ("failing task".to_string(), true)];
                if got != want {
                    return fail(&format!("notices {got:?}, expected {want:?}"));
                }
                println!("    notices: {got:?}");
                self.clear_notices();
                // a newer version on GitHub: a notice once, the menu bar's Update; the version running: neither
                let release = |v: &str| crate::update::Release {
                    version: v.into(),
                    date: "2026-11-02".into(),
                    notes: String::new(),
                    page: crate::update::RELEASES.into(),
                    package: None,
                };
                self.update.pretend(release("99.0.0"));
                self.pump_notices(ctx);
                self.pump_notices(ctx);
                let titles = self.notice_titles();
                if self.update.available().is_none()
                    || titles.iter().filter(|t| t.0 == "A new version of Janus").count() != 1
                {
                    return fail(&format!("a newer version should be announced once: {titles:?}"));
                }
                self.clear_notices();
                self.update.announced = false;
                self.update.pretend(release(crate::update::VERSION));
                self.pump_notices(ctx);
                if self.update.available().is_some() || !self.notice_titles().is_empty() {
                    return fail("the version running is not a newer one");
                }
                println!("    a newer version announced once, Update on the menu bar; none for the version running");
                next(st, "status bar: coordinates in longitude and latitude, the scale; notices of tasks that end");
                None
            }
            181 => {
                // what a query finds first
                for (q, want) in [
                    ("export png", "Export map as PNG..."),
                    ("swipe", "Swipe on / off"),
                    ("landtrendr", "LandTrendr..."),
                    ("reset lay", "Reset layout"),
                    ("ex", "Exports"),
                ] {
                    let got = self.palette_matches(q).into_iter().next().map(|c| c.label);
                    if got.as_deref() != Some(want) {
                        return fail(&format!("the palette's first match for \"{q}\": {got:?}, expected {want:?}"));
                    }
                }
                // a command run from it: the swipe on, then off; a tool's window open
                let swipe = self.palette_matches("swipe").into_iter().next()?;
                let was = self.swipe.is_some();
                self.run_command(&swipe, ctx);
                if self.swipe.is_some() == was {
                    return fail("the palette's Swipe should toggle the swipe");
                }
                self.run_command(&swipe, ctx);
                let tool = self.palette_matches("landtrendr").into_iter().next()?;
                self.run_command(&tool, ctx);
                if !self.zeit.tools.get("landtrendr").is_some_and(|u| u.open) {
                    return fail("the palette's LandTrendr should open its window");
                }
                let lt = janus_zeit::manifest().tool("landtrendr")?;
                self.set_tool_open(lt, false);
                // the recent ones first, the last one run on top; an empty query lists every command
                let all = self.palette_matches("");
                let first: Vec<&str> = all.iter().take(2).map(|c| c.label.as_str()).collect();
                if first != ["LandTrendr...", "Swipe on / off"] {
                    return fail(&format!("the recent commands first: {first:?}"));
                }
                // the layers by name; Ctrl+K opens it, Escape closes it
                let name = self.layers.iter().find(|l| !l.canvas)?.name.clone();
                if !all.iter().any(|c| c.label == format!("Zoom to: {name}")) {
                    return fail("the palette should list the open layers");
                }
                self.toggle_palette();
                if !self.palette.open {
                    return fail("Ctrl+K should open the palette");
                }
                self.toggle_palette();
                println!("    {} commands; recent first: {first:?}", all.len());
                next(st, "command palette: queries, commands run from it, the recent ones first");
                None
            }
            182 => {
                // the blend mode, from the palette
                let blend = self.palette_matches("blend multiply").into_iter().next()?;
                self.run_command(&blend, ctx);
                if self.layers[self.active].blend != janus_gpu::Blend::Multiply {
                    return fail(&format!("the palette's {} should set the active layer's blend mode", blend.label));
                }
                self.layers[self.active].blend = janus_gpu::Blend::Normal;
                // a style copied from the active layer, pasted on another series (or itself)
                let me = Sel::Series(self.session.as_ref()?.info.id);
                let series: Vec<Sel> = (0..self.layers.len())
                    .filter(|&i| !self.layers[i].canvas)
                    .filter_map(|i| self.layer_session(i).map(|s| Sel::Series(s.info.id)))
                    .collect();
                let other = series.iter().copied().find(|&s| s != me).unwrap_or(me);
                let before = (self.style_of(me)?, self.style_of(other)?);
                let v = janus_gpu::Mode::Value as usize;
                self.display.cmap[v] = janus_gpu::colormaps::PLASMA;
                self.display.ranges[v].lo = -3.5;
                self.display.ranges[v].hi = 7.25;
                self.display.ranges[v].manual = true;
                self.layers[self.active].opacity = 0.6;
                self.layers[self.active].blend = janus_gpu::Blend::Screen;
                let copied = self.style_of(me)?;
                self.contents.selected = [other].into_iter().collect();
                if self.paste_style(other, &copied) != 1 {
                    return fail("the style should be pasted on the layer selected");
                }
                let Some(crate::style::LayerStyle::Series(got)) = self.style_of(other) else { return fail("no style") };
                if got.cmap[v] != janus_gpu::colormaps::PLASMA
                    || got.ranges[v] != (-3.5, 7.25, true)
                    || got.opacity != 0.6
                    || got.blend != janus_gpu::Blend::Screen
                {
                    return fail(&format!("the style pasted: {got:?}"));
                }
                // a style saved by name, read back as it was, deleted
                self.save_style("selftest style", &copied);
                let saved = self.saved_styles_for(other).into_iter().find(|s| s.0 == "selftest style");
                if saved.as_ref().map(|s| &s.1) != Some(&copied) {
                    return fail("the saved style should read back as it was saved");
                }
                self.delete_saved_style("selftest style");
                if self.settings.s.styles.iter().any(|s| s.0 == "selftest style") {
                    return fail("the saved style should be deleted");
                }
                self.apply_style(me, &before.0);
                self.apply_style(other, &before.1);
                // the layers on the list grouped: next to each other, shown and hidden together, moved together
                let all: Vec<Sel> = self.draw_order().into_iter().filter_map(|it| self.sel_of(it)).collect();
                let picked: Vec<Sel> = all.iter().copied().step_by(2).collect();
                self.contents.selected = picked.iter().copied().collect();
                let g = self.group_selected()?;
                let order: Vec<Option<Sel>> = self.draw_order().into_iter().map(|it| self.sel_of(it)).collect();
                let at: Vec<usize> =
                    (0..order.len()).filter(|&k| order[k].is_some_and(|s| picked.contains(&s))).collect();
                if at.len() != picked.len() || at.windows(2).any(|w| w[1] != w[0] + 1) {
                    return fail(&format!("the group's layers should be next to each other: at {at:?}"));
                }
                self.set_group_visible(g, false);
                if self.group_visible(g) {
                    return fail("the group's eye should hide its layers");
                }
                self.set_group_visible(g, true);
                self.move_group(g, false);
                let n = self.draw_order().len();
                let bottom: Vec<Option<Sel>> = self
                    .draw_order()
                    .into_iter()
                    .map(|it| self.sel_of(it))
                    .filter(|s| s.is_some())
                    .take(picked.len())
                    .collect();
                if bottom.iter().any(|s| s.is_none_or(|s| !picked.contains(&s))) {
                    return fail("the group should move to the bottom, its layers together");
                }
                self.move_group(g, true);
                self.ungroup(g);
                self.contents.selected.clear();
                println!(
                    "    blend from the palette; a style pasted ({} -> {}); a saved style; {} of {n} layers grouped",
                    copied.to_text().len(),
                    if other == me { "itself" } else { "another series" },
                    picked.len()
                );
                next(st, "Layers: blend modes, styles copied, pasted and saved, groups");
                None
            }
            183 => {
                // Go to: the centre pixel's longitude and latitude typed, back to that pixel; its x y in its EPSG CRS
                let info = self.session.as_ref()?.info.clone();
                let centre = [info.width as f64 / 2.0 + 0.5, info.height as f64 / 2.0 + 0.5];
                let (lon, lat) = self.status_lon_lat(info.pixel_to_geo(centre[0], centre[1])?)?;
                let (gx, gy) = info.pixel_to_geo(centre[0], centre[1])?;
                let mut typed = vec![format!("{lat:.8}, {lon:.8}")];
                if let Some(code) = info.crs_authority.strip_prefix("EPSG:") {
                    typed.push(format!("EPSG:{code} {gx} {gy}"));
                    if let Some(zone) = code.parse::<u32>().ok().filter(|c| (32701..=32760).contains(c)) {
                        typed.push(format!("{}S {gx} {gy}", zone - 32700));
                    }
                }
                for t in &typed {
                    let got = crate::navigation::parse_place(t).and_then(|p| self.place_pixel(p));
                    match got {
                        Ok(at) if (at[0] - centre[0]).abs() < 0.05 && (at[1] - centre[1]).abs() < 0.05 => {}
                        other => return fail(&format!("Go to {t}: {other:?}, expected pixel {centre:?}")),
                    }
                }
                let g = self.grid()?;
                self.go_to_pixel([10.5, 10.5], "corner", false);
                let c = [self.map.center[0] * g.fx, self.map.center[1] * g.fy];
                if (c[0] - 10.5).abs() > 0.01 || (c[1] - 10.5).abs() > 0.01 || self.nav.mark.is_none() {
                    return fail(&format!("Go to should centre the map on the place and mark it: {c:?}"));
                }
                // a saved view: the map moved, then back to it
                self.view_at(centre[0], centre[1], 2.0);
                let before = (self.map.center, self.map.scale);
                if !self.save_view("selftest view") {
                    return fail("the view should be saved");
                }
                self.view_at(5.0, 5.0, 8.0);
                let v = self.saved_views().into_iter().find(|v| v.name == "selftest view")?;
                if let Err(e) = self.restore_view(&v) {
                    return fail(&format!("the saved view should come back: {e}"));
                }
                let (c, s) = (self.map.center, self.map.scale);
                if (c[0] - before.0[0]).abs() > 0.05
                    || (c[1] - before.0[1]).abs() > 0.05
                    || (s / before.1 - 1.0).abs() > 0.01
                {
                    return fail(&format!("the saved view: centre {c:?} scale {s}, expected {before:?}"));
                }
                self.delete_view("selftest view");
                // the minimap: in the map's corner, a click on it moves the map there
                let was = self.settings.s.minimap;
                self.settings.s.minimap = true;
                let canvas =
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.map.canvas[0], self.map.canvas[1]));
                if self.minimap_rect(canvas).is_none() {
                    return fail(&format!("the minimap should fit on a map of {:?}", self.map.canvas));
                }
                self.minimap_go([g.ow * 0.25, g.oh * 0.75]);
                if (self.map.center[0] - g.ow * 0.25).abs() > 1e-9 || (self.map.center[1] - g.oh * 0.75).abs() > 1e-9 {
                    return fail("a click on the minimap should move the map there");
                }
                self.settings.s.minimap = was;
                // the lens: the swipe as a circle, on and off
                let swipe = self.swipe.is_some();
                self.toggle_lens();
                if !self.swipe.as_ref().is_some_and(|s| s.lens) {
                    return fail("L should turn the lens on");
                }
                self.toggle_lens();
                if self.swipe.as_ref().is_some_and(|s| s.lens) {
                    return fail("L again should turn it off");
                }
                if self.swipe.is_some() != swipe {
                    self.toggle_swipe();
                }
                self.map.fit();
                println!(
                    "    went to {} (lon {lon:.5}, lat {lat:.5}); a saved view; the minimap; the lens",
                    typed.join(" | ")
                );
                next(st, "navigation: Go to, saved views, the minimap, the lens");
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                Some(0)
            }
            _ => None,
        }
    }
}
