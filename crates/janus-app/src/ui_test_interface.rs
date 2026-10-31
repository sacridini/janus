//! `--selftest-ui` stages 180-199: the interface of phases 48-55, on what the validation stages left open (D). 180:
//! the status bar (the cursor's coordinates in longitude and latitude, the scale) and the notices of tasks that end
//! (a short task none, a long one and a failed one each one; a newer version of Janus one, once). 181: the command
//! palette (what a query finds first, a command run from it, the recent ones first). 182: the Layers panel (a blend
//! mode from the palette, a style copied and pasted on another series, a style saved by name and read back, layers
//! grouped, hidden and moved together). 183: navigation (Go to a longitude and latitude, an EPSG x y, a UTM zone; a saved view; the minimap;
//! the lens). 184: a run's map as a layer of its own (its row over the rasters; with the series hidden, Pixel values
//! in the chart's place: its value at the cursor, in the status bar too, its pixels by value; removed by Delete).
//! 185-186: provenance (a Mann-Kendall run on an ROI with alpha changed: its provenance.json, alpha named as changed,
//! the window, the maps; Copy Python of the run and, with a Python that has Zeit, its p-values against Janus'; Run
//! again brings the parameters and the run's window back). 187-190: a preview (at most 256 px a side at the centre
//! of the view; its 7 maps marked preview over its window; the next preview replaces them and deletes their folder;
//! Auto-preview runs again when a parameter changes; the tool's window closed removes it).

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
                // the lens: the swipe as a circle, on and off; the wheel steps its date
                let swipe = self.swipe.is_some();
                self.toggle_lens();
                if !self.swipe.as_ref().is_some_and(|s| s.lens) {
                    return fail("L should turn the lens on");
                }
                let lens_layer = self.swipe.as_ref().and_then(|s| self.layer_of(s.view.cube))?;
                let n = self.layer_session(lens_layer)?.info.t();
                let dates = (self.lens_step_date(-1000), self.lens_step_date(1));
                if dates != (Some(0), Some(1.min(n - 1))) || !self.swipe.as_ref().is_some_and(|s| s.view.own_date) {
                    return fail(&format!("the wheel should step the lens' date: {dates:?} of {n}"));
                }
                self.toggle_lens();
                if self.swipe.as_ref().is_some_and(|s| s.lens) || self.swipe.is_some() != swipe {
                    return fail("L again should turn the lens off, and leave the swipe as it was before it");
                }
                // a swipe on before it (C): L makes it the lens, L again the divider
                if !swipe {
                    self.toggle_swipe();
                }
                self.toggle_lens();
                self.toggle_lens();
                if !self.swipe.as_ref().is_some_and(|s| !s.lens) {
                    return fail("L twice over a swipe should leave its divider");
                }
                if !swipe {
                    self.toggle_swipe();
                }
                self.map.fit();
                println!(
                    "    went to {} (lon {lon:.5}, lat {lat:.5}); a saved view; the minimap; the lens",
                    typed.join(" | ")
                );
                next(st, "navigation: Go to, saved views, the minimap, the lens");
                None
            }
            184 => {
                // a map of years over the left half of the active layer's grid (no value on the right): 1990 + row % 10
                let info = self.session.as_ref()?.info.clone();
                let (w, h) = (info.width, info.height);
                let data: Vec<f32> =
                    (0..w * h).map(|i| if i % w < w / 2 { 1990.0 + (i / w % 10) as f32 } else { f32::NAN }).collect();
                let mut r = crate::zeit::ResultLayer::for_test(data, w, h, "year".into());
                (r.cube, r.grid, r.visible) = (info.id, Some(info.clone()), true);
                r.name = "Self-test: year of detection".into();
                r.auto_range();
                r.tex = Some(self.gpu.as_ref()?.create_tile(w as u32, h as u32, &r.data));
                self.push_result(r);
                let id = self.zeit.results.iter().find(|r| r.name.starts_with("Self-test"))?.id;
                // a row of its own, over every raster
                let order = self.draw_order();
                let k = order.iter().position(|&it| self.sel_of(it) == Some(Sel::Result(id)))?;
                if order[k + 1..].iter().any(|it| matches!(it, crate::order::Item::Series(_))) {
                    return fail("a run's map should go over every raster");
                }
                // the series hidden: Pixel values in the chart's place, the map's value at the cursor
                let was = self.layers[self.active].visible;
                self.layers[self.active].visible = false;
                if self.chart_shown() {
                    return fail("with the active series hidden, Pixel values should replace the chart");
                }
                self.set_hover(Some((2, 3)));
                let name = "Self-test: year of detection";
                let at = |s: &State| s.cursor_values().into_iter().find(|(n, _)| n == name).map(|x| x.1);
                let (left, status) = (at(self), self.result_status_at(2, 3));
                self.set_hover(Some((w - 1, 3)));
                let right = at(self);
                println!("    the map at (2, 3): {left:?} (status bar: {status:?}); at ({}, 3): {right:?}", w - 1);
                if left.as_deref() != Some("1993") || !status.contains("1993") {
                    return fail("Pixel values and the status bar should give the map's year at the cursor");
                }
                if right.as_deref() != Some("-") {
                    return fail("where the map has no value, Pixel values should say so");
                }
                // its pixels by value: 10 years, the left half of the grid in all
                self.values.focus = Some(Sel::Result(id));
                let counts = self.focused_counts()?;
                let total: usize = counts.iter().map(|c| c.1).sum();
                if counts.len() != 10 || total != h * (w / 2) || counts[0].0 != 1990.0 {
                    return fail(&format!("its pixels by value: {counts:?}"));
                }
                // removed with its row selected (Delete)
                self.contents.selected = [Sel::Result(id)].into_iter().collect();
                self.remove_selected(ctx);
                if self.zeit.results.iter().any(|r| r.id == id) {
                    return fail("Delete should remove the map selected");
                }
                self.layers[self.active].visible = was;
                self.map.dirty = true;
                next(st, "a run's map as a layer of its own; Pixel values and its statistics with the series hidden");
                None
            }
            185 => {
                // Mann-Kendall with alpha changed, on an ROI, into a folder of the test's
                let info = self.session.as_ref()?.info.clone();
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-provenance-{stamp}"));
                let tool = janus_zeit::manifest().tool("mann_kendall")?;
                let mut params: std::collections::BTreeMap<String, serde_json::Value> =
                    tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                params.insert("alpha".into(), 0.1.into());
                *self.zeit.params(tool) = params;
                let (w, h) = (info.width as i64, info.height as i64);
                self.set_roi_rect([w / 4, h / 4, w / 4 + 24.min(w / 2), h / 4 + 16.min(h / 2)]);
                self.zeit.tools.entry(tool.id.clone()).or_default().scope = 2;
                self.zeit.results_dir = Some(dir.clone());
                st.run = Some((dir, self.layers.len(), self.active));
                self.run_tool(tool, ctx);
                next(st, "Mann-Kendall run on an ROI with alpha 0.1");
                None
            }
            186 => {
                let (dir, _, _) = st.run.clone()?;
                let job = self.zeit.jobs.last()?;
                let (state, error) = {
                    let s = job.status.lock().ok()?;
                    (s.state, s.error.clone())
                };
                let (loaded, out_dir) = (job.loaded, job.output_dir.clone());
                match state {
                    JobState::Failed | JobState::Cancelled => return fail(&format!("the run should finish: {error}")),
                    JobState::Running => return None,
                    JobState::Done => {}
                }
                if !loaded || self.zeit.loads_pending() {
                    return None;
                }
                let run = std::path::PathBuf::from(&out_dir);
                let p = match crate::provenance::Provenance::read(&run) {
                    Ok(p) => p,
                    Err(e) => return fail(&format!("the run should write its provenance: {e}")),
                };
                let roi = self.series.roi_rect?;
                let series = p.series.clone().unwrap_or_default();
                println!(
                    "    provenance: {} by Janus {} (zeit-core {}), {} in {:.2} s, {} dates of {} files, {} {:?}, \
                     changed {:?}, {} outputs, {}",
                    p.tool_name,
                    p.janus,
                    &p.zeit_core[..7],
                    p.state,
                    p.seconds,
                    series.dates,
                    series.files.len(),
                    p.scope,
                    p.window,
                    p.changed,
                    p.outputs.len(),
                    p.python
                );
                if p.tool != "mann_kendall" || p.state != "done" || p.janus != env!("CARGO_PKG_VERSION") {
                    return fail("the provenance should name the tool, its end and Janus' version");
                }
                if p.changed != ["alpha"] || p.params.get("alpha") != Some(&0.1.into()) {
                    return fail("the provenance should give the parameters and name alpha as changed");
                }
                if p.window != Some(roi) || p.scope_index != 2 || series.files.is_empty() || series.dates < 4 {
                    return fail("the provenance should give the ROI's window and the series' files");
                }
                if p.outputs.len() != 7 || !p.outputs.iter().all(|o| std::path::Path::new(&o.path).is_file()) {
                    return fail("the provenance should list the 7 maps written");
                }
                // from a map's row: found from its run's folder
                let r = self.zeit.results.iter().find(|r| r.run == out_dir)?;
                if crate::provenance::find(&r.path).as_deref() != Some(run.as_path()) {
                    return fail("a run's map should find its provenance");
                }
                // Copy Python of the run: the same call with alpha 0.1
                let code = match self.copy_run_python(ctx, &out_dir) {
                    Ok(c) => c,
                    Err(e) => return fail(&format!("the run should have its Python: {e}")),
                };
                if !code.contains("zeit.mann_kendall(series") || !code.contains("alpha=0.1") {
                    return fail("the run's Python should call Mann-Kendall with alpha 0.1");
                }
                // Run again: the parameters and the very window back, the ROI gone meanwhile
                let tool = janus_zeit::manifest().tool("mann_kendall")?;
                self.zeit.params(tool).insert("alpha".into(), 0.05.into());
                self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
                self.clear_roi();
                self.run_again(&p);
                let alpha = self.zeit.params(tool).get("alpha").cloned();
                let u = self.zeit.tools.get(&tool.id)?;
                if alpha != Some(0.1.into()) || u.scope != 3 || !u.open || self.tool_run_window(tool, 3).0 != roi {
                    return fail("Run again should bring back the parameters and the run's window");
                }
                println!("    run again: alpha 0.1 and the run's window {roi:?} back in the tool's window");
                // the Python run (when there is a Python with zeit and rasterio): its p-value map against Janus'
                let python = std::env::var("JANUS_PYTHON").unwrap_or_else(|_| "python".into());
                let mut cmd = std::process::Command::new(&python);
                cmd.arg(run.join(crate::provenance::SCRIPT));
                for v in ["OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "NUMBA_NUM_THREADS"] {
                    cmd.env(v, "8");
                }
                let has_zeit = std::process::Command::new(&python)
                    .args(["-c", "import zeit, rasterio, xarray"])
                    .output()
                    .is_ok_and(|o| o.status.success());
                if has_zeit {
                    let out = match cmd.output() {
                        Ok(o) if o.status.success() => o,
                        Ok(o) => return fail(&format!("rerun.py failed:\n{}", String::from_utf8_lossy(&o.stderr))),
                        Err(e) => return fail(&format!("{python}: {e}")),
                    };
                    let _ = out;
                    let ours = p.outputs.iter().find(|o| o.path.ends_with("_p.tif"))?.path.clone();
                    let read = |path: &str| -> Option<Vec<f32>> {
                        let ds = gdal::Dataset::open(path).ok()?;
                        let (w, h) = ds.raster_size();
                        Some(ds.rasterband(1).ok()?.read_as::<f32>((0, 0), (w, h), (w, h), None).ok()?.data().to_vec())
                    };
                    let (a, b) = (read(&ours)?, read(&run.join("python").join("p.tif").to_string_lossy())?);
                    let mut worst = 0f32;
                    let mut n = 0;
                    for (x, y) in a.iter().zip(&b) {
                        if x.is_finite() != y.is_finite() {
                            return fail(&format!("rerun.py's p-values have values where Janus' have none: {x} {y}"));
                        }
                        if x.is_finite() {
                            worst = worst.max((x - y).abs());
                            n += 1;
                        }
                    }
                    println!("    rerun.py: its p-value map against Janus', {n} pixels, largest difference {worst:e}");
                    if a.len() != b.len() || n == 0 || worst > 1e-4 {
                        return fail("rerun.py should give Janus' p-values");
                    }
                } else {
                    println!(
                        "    rerun.py: not run (no Python with zeit, rasterio and xarray; JANUS_PYTHON names one)"
                    );
                }
                // gone again
                self.zeit.results.retain(|r| r.run != out_dir);
                self.zeit.jobs.pop();
                self.zeit.results_dir = None;
                self.set_tool_open(tool, false);
                let _ = std::fs::remove_dir_all(&dir);
                self.map.dirty = true;
                next(st, "provenance: the run's record, Run again, Copy Python of the run (and its maps as Janus')");
                None
            }
            187 => {
                // a preview without an ROI: at most 256 px a side at the centre of the view
                let info = self.session.as_ref()?.info.clone();
                let tool = janus_zeit::manifest().tool("mann_kendall")?;
                self.set_tool_open(tool, true);
                self.zeit.params(tool).insert("alpha".into(), 0.1.into());
                self.run_preview(tool, ctx);
                let p = self.zeit.preview.as_ref()?;
                let [x0, y0, x1, y1] = p.window;
                println!("    preview on {:?} of {} x {} px", p.window, info.width, info.height);
                if x1 - x0 > crate::zeit::PREVIEW_SIDE
                    || y1 - y0 > crate::zeit::PREVIEW_SIDE
                    || x1 > info.width
                    || y1 > info.height
                    || x1 - x0 < info.width.min(crate::zeit::PREVIEW_SIDE)
                {
                    return fail("a preview without an ROI should cover at most 256 px a side of the view");
                }
                st.run = Some((std::path::PathBuf::from(&p.dir), 0, 0));
                next(st, "preview started at the centre of the view");
                None
            }
            188 | 189 => {
                let (dir, _, _) = st.run.clone()?;
                let dir = dir.to_string_lossy().into_owned();
                let job = self.zeit.jobs.iter().find(|j| j.output_dir == dir)?;
                let s = job.status.lock().ok()?.state;
                if s == JobState::Failed || s == JobState::Cancelled {
                    return fail("the preview should finish");
                }
                if s == JobState::Running || !job.loaded || self.zeit.loads_pending() {
                    return None;
                }
                let maps: Vec<&crate::zeit::ResultLayer> = self.zeit.results.iter().filter(|r| r.run == dir).collect();
                let w = self.zeit.preview.as_ref()?.window;
                if maps.len() != 7
                    || !maps
                        .iter()
                        .all(|r| r.preview && r.name.starts_with("Preview: ") && [r.x0, r.y0] == [w[0], w[1]])
                {
                    return fail("a preview's maps should be layers marked preview over its window");
                }
                let tool = janus_zeit::manifest().tool("mann_kendall")?;
                if stage == 188 {
                    // the next preview replaces it: its maps, its task and (soon) its folder gone
                    self.zeit.params(tool).insert("alpha".into(), 0.05.into());
                    self.run_preview(tool, ctx);
                    if self.zeit.results.iter().any(|r| r.run == dir)
                        || self.zeit.jobs.iter().any(|j| j.output_dir == dir)
                    {
                        return fail("a new preview should replace the last one's maps");
                    }
                    let new = self.zeit.preview.as_ref()?.dir.clone();
                    st.run = Some((std::path::PathBuf::from(new), 0, 0));
                    st.reveal = Some(std::path::PathBuf::from(dir));
                    next(st, "a preview's maps marked preview over its window; a second preview replaces them");
                    return None;
                }
                if st.reveal.as_ref().is_some_and(|d| d.exists()) {
                    return None; // the first preview's folder, deleted on a thread
                }
                // Auto-preview: a parameter changed, a preview again once they rest
                let u = self.zeit.tools.entry(tool.id.clone()).or_default();
                u.auto_preview = true;
                u.version += 1;
                u.preview_asked = Some(Instant::now());
                self.zeit.params(tool).insert("alpha".into(), 0.2.into());
                st.reveal = None;
                next(st, "the first preview's folder deleted; Auto-preview asked by a parameter changed");
                None
            }
            190 => {
                let (first, _, _) = st.run.clone()?;
                let first = first.to_string_lossy().into_owned();
                let tool = janus_zeit::manifest().tool("mann_kendall")?;
                let version = self.zeit.tools.get(&tool.id)?.version;
                let p = self.zeit.preview.as_ref()?;
                if p.dir == first || p.version != version {
                    return None; // the auto-preview not made yet
                }
                let dir = p.dir.clone();
                let job = self.zeit.jobs.iter().find(|j| j.output_dir == dir)?;
                if job.status.lock().ok()?.state == JobState::Running || !job.loaded || self.zeit.loads_pending() {
                    return None;
                }
                if self.zeit.results.iter().filter(|r| r.run == dir && r.preview).count() != 7 {
                    return fail("the auto-preview should give its maps");
                }
                // the tool's window closed: its preview goes with it
                self.set_tool_open(tool, false);
                self.zeit.tools.entry(tool.id.clone()).or_default().auto_preview = false;
                if self.zeit.preview.is_some() || self.zeit.results.iter().any(|r| r.preview) {
                    return fail("closing the tool's window should remove its preview");
                }
                next(st, "Auto-preview made again on a parameter changed; closing the tool's window removes it");
                None
            }
            191 => {
                // a run's map to keep in the project: Mann-Kendall on an ROI, into a folder of the test's
                let info = self.session.as_ref()?.info.clone();
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-project-{stamp}"));
                let tool = janus_zeit::manifest().tool("mann_kendall")?;
                *self.zeit.params(tool) = tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                let (w, h) = (info.width as i64, info.height as i64);
                self.set_roi_rect([w / 5, h / 5, w / 5 + 30.min(w / 2), h / 5 + 20.min(h / 2)]);
                self.zeit.tools.entry(tool.id.clone()).or_default().scope = 2;
                self.zeit.results_dir = Some(dir.join("results"));
                // and a vector layer
                let _ = std::fs::create_dir_all(&dir);
                let gj = dir.join("plot.geojson");
                let feature = r#"{"type": "Feature", "properties": {"plot": 1}, "geometry": {"type": "Point", "coordinates": [0.0, 0.0]}}"#;
                std::fs::write(&gj, format!(r#"{{"type": "FeatureCollection", "features": [{feature}]}}"#)).ok()?;
                self.add_vector_files(vec![gj.to_string_lossy().into_owned()], ctx);
                st.run = Some((dir, 0, 0));
                self.run_tool(tool, ctx);
                next(st, "project: a Mann-Kendall run for a map to keep, a vector layer");
                None
            }
            192 => {
                let (dir, _, _) = st.run.clone()?;
                let job = self.zeit.jobs.last()?;
                if job.status.lock().ok()?.state == JobState::Running
                    || !job.loaded
                    || self.zeit.loads_pending()
                    || !self.vector_loads.is_empty()
                {
                    return None;
                }
                let out_dir = job.output_dir.clone();
                // the vector layer in a colour of its own
                let v = self.vectors.iter().position(|v| v.data.path.ends_with("plot.geojson"))?;
                self.vectors[v].color = egui::Color32::from_rgb(200, 30, 160);
                // a state to keep: the active layer's opacity and blend, another series hidden, its date, a map's
                // colours, two layers grouped, a pin, the ROI
                let a = self.active;
                (self.layers[a].opacity, self.layers[a].blend) = (0.6, janus_gpu::Blend::Multiply);
                if let Some(o) = (0..self.layers.len()).find(|&i| i != a && !self.layers[i].canvas) {
                    self.layers[o].visible = false;
                }
                if self.session.as_ref()?.info.t() > 2 {
                    self.set_date(2);
                }
                let r = self.zeit.results.iter_mut().find(|r| r.run == out_dir)?;
                (r.lo, r.hi, r.visible) = (-0.5, 0.25, true);
                let rid = r.id;
                let sid = self.session.as_ref()?.info.id;
                self.contents.selected = [Sel::Series(sid), Sel::Result(rid)].into_iter().collect();
                self.group_selected()?;
                self.contents.selected.clear();
                self.clear_pins();
                self.add_pin(12, 9);
                self.zeit.results_dir = None;
                let file = dir.join("project").join("test.janus");
                let _ = std::fs::create_dir_all(file.parent()?);
                self.save_project_to(&file);
                let text = std::fs::read_to_string(&file).ok()?;
                let p: crate::project::Project = serde_json::from_str(&text).ok()?;
                let rel =
                    p.series.iter().flat_map(|s| &s.inputs).filter(|f| !std::path::Path::new(&f.path).is_absolute());
                println!(
                    "    saved {} ({} bytes): {} series ({} input paths relative), {} vector layers, {} maps, {} groups",
                    file.display(),
                    text.len(),
                    p.series.len(),
                    rel.count(),
                    p.vectors.len(),
                    p.results.len(),
                    p.groups.len()
                );
                if p.format != crate::project::FORMAT
                    || p.results.is_empty()
                    || p.vectors.len() != 1
                    || p.groups.len() != 1
                    || p.pins.len() != 1
                {
                    return fail("the project should keep the series, the map, the group and the pin");
                }
                if self.project.dirty || self.project.path.as_deref() != Some(file.as_path()) {
                    return fail("a project saved should be its file, with no changes");
                }
                st.texts = vec![self.project_snapshot()];
                st.reveal = Some(file);
                next(st, "project saved: series, vector layers, a map, a group, a pin, the ROI");
                None
            }
            193 => {
                // a change: the project is marked changed (the title's *), and Open asks first
                let file = st.reveal.clone()?;
                if st.since.is_some_and(|t| t.elapsed().as_secs_f64() < 0.1) {
                    let a = self.active;
                    self.layers[a].opacity = 0.3;
                    return None;
                }
                if !self.project.dirty {
                    return None; // looked at once a second
                }
                if !self.window_title().contains("test.janus*") {
                    return fail(&format!("the title should mark the changes: {}", self.window_title()));
                }
                self.open_project(&file, ctx);
                if self.project.confirm.is_none() || self.project.restoring() {
                    return fail("opening a project over changes not saved should ask first");
                }
                // Discard
                (self.project.confirm, self.project.dirty) = (None, false);
                self.open_project(&file, ctx);
                if !self.project.restoring() || !self.layers.is_empty() {
                    return fail("opening a project should close what was open and open its layers");
                }
                next(st, "changes marked in the title (*); Open asks first; the project opening");
                None
            }
            194 | 196 => {
                if self.project.restoring() || self.opening.is_some() || self.zeit.loads_pending() {
                    return None;
                }
                let now = self.project_snapshot();
                let was = st.texts.first()?.clone();
                if stage == 194 {
                    if now != was {
                        println!("    kept:\n{was}\n    opened:\n{now}");
                        return fail("the project opened should be as it was saved");
                    }
                    println!("    opened as saved: {} lines of state ({})", now.lines().count(), self.window_title());
                    // a file gone: the rest opens, a notice says what is missing
                    let file = st.reveal.clone()?;
                    let mut p = crate::project::Project::read(&file).ok()?;
                    let gone = p.series.first()?.name.clone();
                    for f in &mut p.series[0].inputs {
                        f.path = "gone/nowhere.tif".into();
                        f.absolute = file.parent()?.join("gone").join("nowhere.tif").to_string_lossy().into_owned();
                    }
                    let broken = file.with_file_name("broken.janus");
                    p.write(&broken).ok()?;
                    self.clear_notices();
                    self.open_project(&broken, ctx);
                    st.texts.push(gone);
                    next(st, "the project opened as it was saved (order, styles, dates, map, group, pin, ROI, view)");
                    st.stage = 196;
                    return None;
                }
                let gone = st.texts.get(1)?.clone();
                let notices = self.notice_titles();
                let names: Vec<String> = self.layers.iter().map(|l| l.name.clone()).collect();
                println!("    without {gone}: {} series open, notices {notices:?}", names.len());
                if !notices.iter().any(|n| n.0.contains("without some of its layers")) || names.contains(&gone) {
                    return fail("a file gone should leave a notice and the rest open");
                }
                let saved: crate::project::Project = serde_json::from_str(&was).ok()?;
                if names.len() + 1 != saved.series.len() {
                    return fail("every other series should open");
                }
                // the whole project back for what follows
                let file = st.reveal.take()?;
                self.open_project(&file, ctx);
                next(st, "a file gone: a notice, the rest of the project opened");
                st.stage = 197;
                None
            }
            197 => {
                if self.project.restoring() || self.opening.is_some() || self.zeit.loads_pending() {
                    return None;
                }
                // undo: four changes, each a step (as a frame's end records it)
                self.record_undo();
                (self.undo.past, self.undo.future) = (vec![], vec![]);
                let before = self.undo_text();
                let a = self.active;
                let o = (0..self.layers.len()).find(|&i| i != a && !self.layers[i].canvas)?;
                let name = self.layers[o].name.clone();
                self.layers[a].opacity = 0.25;
                self.record_undo();
                self.layers[o].visible = !self.layers[o].visible;
                self.record_undo();
                self.add_pin(20, 15);
                self.record_undo();
                self.remove_layer(o, ctx);
                self.record_undo();
                let steps: Vec<String> = self.undo.past.iter().map(|s| s.what.clone()).collect();
                println!("    steps: {steps:?}; Edit menu: Undo {}", self.can_undo().unwrap_or("-"));
                let want = ["Opacity of", "Show", "Pins", "Close"];
                if steps.len() != 4 || steps.iter().zip(want).any(|(s, w)| !s.starts_with(w)) {
                    return fail("four changes should be four steps: opacity, visibility, a pin, a layer closed");
                }
                if steps[3] != format!("Close {name}") || self.layers.iter().position(|l| l.name == name).is_some() {
                    return fail("the layer should be closed");
                }
                st.texts = vec![before, self.undo_text(), name];
                st.run = Some((std::path::PathBuf::new(), 0, 0));
                next(st, "undo: four changes recorded as four steps");
                None
            }
            198 | 199 => {
                if self.undo.busy()
                    || self.opening.is_some()
                    || self.zeit.loads_pending()
                    || !self.vector_loads.is_empty()
                {
                    return None;
                }
                let (dir, undone, redone) = st.run.clone()?;
                if stage == 198 && undone < 4 {
                    self.undo(ctx);
                    st.run = Some((dir, undone + 1, redone));
                    return None;
                }
                if stage == 199 && redone < 4 {
                    self.redo(ctx);
                    st.run = Some((dir, undone, redone + 1));
                    return None;
                }
                let want = st.texts.get(if stage == 198 { 0 } else { 1 })?.clone();
                let now = self.undo_text();
                if now != want {
                    println!("    want:\n{want}\n    now:\n{now}");
                    return fail(if stage == 198 {
                        "undoing every step should give the state before them back"
                    } else {
                        "redoing every step should give the state after them back"
                    });
                }
                let name = st.texts.get(2)?.clone();
                let open = self.layers.iter().position(|l| l.name == name).is_some();
                if stage == 198 {
                    println!(
                        "    undone 4 steps: {name} open again ({} layers), Redo {}",
                        self.layers.len(),
                        self.can_redo().unwrap_or("-")
                    );
                    if !open || !self.undo.past.is_empty() || self.undo.future.len() != 4 {
                        return fail("the layer closed should be open again, every step redoable");
                    }
                    next(st, "undo: four steps undone, the layer closed opened again as it was");
                    return None;
                }
                println!("    redone 4 steps ({} layers)", self.layers.len());
                if open || self.undo.past.len() != 4 {
                    return fail("redoing should close the layer again");
                }
                next(st, "redo: the four steps done again");
                None
            }
            // the calculator live: a reduction on the map as its dates are read, the chart at a pin
            200 => {
                let (ls, at) = self.calc_layers();
                let a = ls[at.iter().position(|&k| k == self.active)?].alias.clone();
                let s = self.session.as_ref()?;
                if self.series.pins.is_empty() {
                    let (x, y) = (s.info.width / 3, s.info.height / 2);
                    self.add_pin(x, y);
                }
                self.calc.show = true;
                self.calc.text = format!("sum({a}) + max({a})");
                self.calc.live.on = true;
                self.calc.live.chart = true;
                next(st, "the calculator live and its chart on sum + max");
                None
            }
            201 => {
                let pin = self.series.pins.first()?.clone();
                let live = self.zeit.results.iter().find(|r| r.preview && r.name.starts_with("Live: "));
                let live = live?;
                if !self.calc.live.label.is_empty()
                    || !pin.exact
                    || pin.values.is_empty()
                    || self.calc.live.lines.len() < 3
                {
                    return None;
                }
                let v: Vec<f32> = pin.values.iter().copied().filter(|v| v.is_finite()).collect();
                let want = v.iter().sum::<f32>() + v.iter().fold(f32::NAN, |m, &x| m.max(x));
                let result = self.calc.live.lines.iter().find(|l| l.name == "the result")?;
                let got = result.pts.first()?[1] as f32;
                let reduced = self.calc.live.lines.iter().filter(|l| l.name.starts_with("what ")).map(|l| l.pts.len());
                println!(
                    "    {}: {} x {} live, the chart at the pin: {} (sum + max of its {} values: {want})",
                    live.name,
                    live.tw,
                    live.th,
                    self.calc_chart_text(),
                    v.len()
                );
                if (got - want).abs() > 1e-4 * want.abs().max(1.0) || reduced.clone().any(|n| n != v.len()) {
                    return fail("the chart should give the pin's sum + max and the values it reduces");
                }
                if live.data.iter().all(|x| !x.is_finite()) {
                    return fail("the live result should have values");
                }
                self.calc.live.on = false;
                self.calc.show = false;
                next(st, "the calculator live: sum + max on the map, the chart at the pin");
                None
            }
            202 => {
                // gone with Live unticked
                if self.zeit.results.iter().any(|r| r.name.starts_with("Live: ")) {
                    return None;
                }
                // the series regularized to periods of two years, by the median
                let r = &mut self.reg;
                (r.step, r.days, r.composite, r.fill, r.all_bands, r.roi) = (4, 730, 0, 0, false, false);
                r.name = "Two years".into();
                st.texts = vec![self.layers.get(self.active)?.name.clone()];
                self.regularize_run(ctx);
                next(st, "the series regularized to periods of 730 days by the median");
                None
            }
            203 => {
                let k = self.layers.iter().position(|l| l.name == "Two years");
                let Some(k) = k else {
                    if self
                        .emb_downloads
                        .iter()
                        .any(|d| d.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail("the regularization failed (Tools > Tasks > Log)");
                    }
                    return None;
                };
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) {
                    return None;
                }
                let pin = self.series.pins.first()?.clone();
                let src = self.session.as_ref()?.info.clone();
                let reg = self.layer_session(k)?.info.clone();
                let times: Vec<f64> = src.layers.iter().map(|l| l.time).collect();
                let ps = janus_zeit::regularize::periods(&times, janus_zeit::regularize::Step::Days(730));
                let mut worst = 0f32;
                for (p, period) in ps.iter().enumerate() {
                    let mut v: Vec<f32> =
                        period.dates.iter().map(|&t| pin.values[t]).filter(|v| v.is_finite()).collect();
                    v.sort_by(f32::total_cmp);
                    let want = match v.len() {
                        0 => f32::NAN,
                        n if !n.is_multiple_of(2) => v[n / 2],
                        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
                    };
                    let got = janus_cube::with_thread_reader(&reg, |r| r.read_pixel(p, pin.x, pin.y))?;
                    if want.is_nan() != got.is_nan() {
                        return fail("a period without values should have none");
                    }
                    if want.is_finite() {
                        worst = worst.max((got - want).abs());
                    }
                }
                println!(
                    "    {} -> \"Two years\": {} dates ({}), the pin's medians off by {worst} at most",
                    st.texts.first()?,
                    reg.t(),
                    reg.layers.iter().map(|l| l.label.as_str()).collect::<Vec<_>>().join(", ")
                );
                if reg.t() != ps.len() || worst > 1e-5 {
                    return fail("each period should be the median of the pin's dates in it");
                }
                if self.layers[self.active].name == "Two years" {
                    return fail("the series studied should stay active");
                }
                self.remove_layer(k, ctx);
                next(st, "regularized: a date per period, each the median of its dates");
                None
            }
            204 => {
                // the date shown of the land cover polygonized, 4-wise, every patch
                let info = self.session.as_ref()?.info.clone();
                let l = info.layers.get(self.display.t.min(info.t() - 1))?.clone();
                let ds = gdal::Dataset::open(&l.path).ok()?;
                let (w, h) = ds.raster_size();
                let v =
                    ds.rasterband(l.band.max(1) as usize).ok()?.read_as::<f64>((0, 0), (w, h), (w, h), None).ok()?;
                let v = v.data();
                // the patches counted by a flood fill
                let mut seen = vec![false; w * h];
                let mut sizes = vec![];
                for i in 0..w * h {
                    if seen[i] {
                        continue;
                    }
                    sizes.push(0usize);
                    let mut stack = vec![i];
                    seen[i] = true;
                    while let Some(j) = stack.pop() {
                        *sizes.last_mut()? += 1;
                        let (x, y) = (j % w, j / w);
                        let near = [
                            (x > 0).then(|| j - 1),
                            (x + 1 < w).then(|| j + 1),
                            (y > 0).then(|| j - w),
                            (y + 1 < h).then(|| j + w),
                        ];
                        for k in near.into_iter().flatten() {
                            if !seen[k] && v[k] == v[j] {
                                seen[k] = true;
                                stack.push(k);
                            }
                        }
                    }
                }
                self.open_polygonize(crate::contents::Sel::Series(info.id));
                let p = &mut self.poly;
                (p.eight, p.min_area_ha, p.simplify_m, p.mean_of) = (false, 0.0, 0.0, None);
                p.name = "Patches".into();
                let gpkg = self.polygonize_start(ctx)?;
                sizes.sort_unstable();
                // the second run's minimum: the second smallest patch (the smallest merged)
                let second = sizes.get(1).copied().unwrap_or(1);
                st.texts = vec![gpkg, sizes.len().to_string(), (w * h).to_string(), second.to_string()];
                next(st, "the land cover polygonized, every patch");
                None
            }
            205 | 206 => {
                let gpkg = st.texts.first()?.clone();
                let Some(k) = self.vectors.iter().position(|v| v.data.path == gpkg) else {
                    if self.zeit.jobs.iter().any(|j| {
                        j.tool == "polygonize" && j.status.lock().unwrap().state == crate::zeit::JobState::Failed
                    }) {
                        return fail("polygonize failed (Tools > Tasks > Log)");
                    }
                    return None;
                };
                let n = self.vectors[k].data.areas;
                let ds = gdal::Dataset::open(&gpkg).ok()?;
                use gdal::vector::LayerAccess;
                let mut layer = ds.layer(0).ok()?;
                let (mut pixels, mut area, mut least, mut named) = (0i64, 0.0, i64::MAX, 0);
                for f in layer.features() {
                    let px = f.field_index("pixels").ok().and_then(|i| f.field_as_integer64(i).ok().flatten())?;
                    pixels += px;
                    least = least.min(px);
                    area += f.field_index("area_ha").ok().and_then(|i| f.field_as_double(i).ok().flatten())?;
                    named += f.field_index("class").ok().and_then(|i| f.field_as_string(i).ok().flatten()).is_some()
                        as usize;
                }
                let (patches, all): (usize, i64) = (st.texts[1].parse().ok()?, st.texts[2].parse().ok()?);
                println!(
                    "    {}: {n} polygons ({named} with a class name), {pixels} px, {area:.2} ha, the smallest {least} px",
                    self.vectors[k].name
                );
                if st.stage == 205 {
                    if n != patches || pixels != all {
                        return fail(&format!("{patches} patches of {all} px expected"));
                    }
                    // 30 m pixels in UTM: 0.09 ha each, geodesic within the scale factor
                    if (area / (all as f64 * 0.09) - 1.0).abs() > 0.01 {
                        return fail("the polygons' area should be their pixels'");
                    }
                    self.remove_vector(k);
                    // again 8-wise, the patches under the second smallest merged (30 m pixels, 0.09 ha)
                    let second: f64 = st.texts[3].parse().ok()?;
                    let p = &mut self.poly;
                    (p.eight, p.min_area_ha) = (true, (second - 0.5) * 0.09);
                    p.name = "Patches merged".into();
                    st.texts[0] = self.polygonize_start(ctx)?;
                    next(st, "polygonized: a polygon per patch, their pixels and area the map's");
                } else {
                    let second: i64 = st.texts[3].parse().ok()?;
                    if least < second || n >= patches || pixels != all {
                        return fail(&format!("with a minimum of {second} px the smallest patch should be merged"));
                    }
                    self.remove_vector(k);
                    self.poly.show = false;
                    next(st, "polygonized 8-wise with a minimum area: the smaller patches merged");
                }
                None
            }
            207 => {
                // Analysis > Areas: the land cover's classes at every date, at full resolution
                self.open_areas(None);
                (self.areas.roi, self.areas.unit) = (false, 0);
                self.areas.table = None;
                self.areas_run(ctx);
                next(st, "the areas of the land cover's classes at every date");
                None
            }
            208 => {
                self.pump_areas();
                let Some((t, meta)) = self.areas.table.clone() else {
                    if self.areas.status.starts_with("Failed") {
                        return fail(&self.areas.status.clone());
                    }
                    return None;
                };
                let info = self.session.as_ref()?.info.clone();
                let n = (info.width * info.height) as u64;
                let csv = std::path::Path::new(&meta.folder).join("areas.csv");
                println!(
                    "    {} dates x {} classes in {:.2} s; {}; first date {:.2} ha",
                    t.labels.len(),
                    t.values.len(),
                    meta.seconds,
                    t.note,
                    t.total(0) / 1e4
                );
                if t.labels.len() != info.t() || (0..t.labels.len()).any(|s| t.pixels[s].iter().sum::<u64>() != n) {
                    return fail("every date should count all its pixels");
                }
                // UTM 30 m: each pixel about 900 m²
                for s in 0..t.labels.len() {
                    for k in 0..t.values.len() {
                        let px = t.pixels[s][k] as f64;
                        if px > 0.0 && !(899.0..901.0).contains(&(t.m2[s][k] / px)) {
                            return fail("a class's area should be its pixels' (30 m)");
                        }
                    }
                }
                // the classes' timeline counts the overview, the series' whole grid here
                let c = &self.analysis.classes;
                if self.session.as_ref()?.s.overview.factor == 1.0 && c.next == info.t() {
                    for (k, &v) in c.values.iter().enumerate() {
                        let j = t.values.iter().position(|&x| x == i64::from(v));
                        for s in 0..t.labels.len() {
                            let px = j.map_or(0, |j| t.pixels[s][j]);
                            if px != c.counts[s][k] as u64 {
                                return fail("the full-resolution counts should be the class timeline's");
                            }
                        }
                    }
                    println!("    the counts equal the class timeline's at every date");
                }
                if !csv.exists() {
                    return fail("the task should write areas.csv");
                }
                self.analysis.show = false;
                next(st, "areas: every date's classes counted, each pixel its area, the CSV written");
                None
            }
            209 => {
                // the land cover warped with its own CRS and pixel: the same grid, the same values
                let w = &mut self.warp;
                (w.source, w.target, w.pixel_on, w.area, w.format, w.resampling) = (None, 0, false, 0, 0, None);
                (w.nodata_on, w.data_type) = (false, 0);
                w.name = "Warped same".into();
                self.warp_run(ctx);
                next(st, "the land cover warped onto its own grid (COGs)");
                None
            }
            210 | 211 => {
                let name = if st.stage == 210 { "Warped same" } else { "Warped lonlat" };
                let Some(k) = self.layers.iter().position(|l| l.name == name) else {
                    if self
                        .zeit
                        .jobs
                        .iter()
                        .any(|j| j.tool == "warp" && j.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail("the warp failed (Tools > Tasks > Log)");
                    }
                    return None;
                };
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) {
                    return None;
                }
                let src = self.session.as_ref()?.info.clone();
                let out = self.layer_session(k)?.info.clone();
                let srs = gdal::spatial_ref::SpatialRef::from_wkt(&out.crs_wkt).ok()?;
                println!(
                    "    {name}: {} dates of {} x {} px, {} ({})",
                    out.t(),
                    out.width,
                    out.height,
                    out.crs_name,
                    std::path::Path::new(&out.layers[0].path).extension()?.to_string_lossy()
                );
                if out.t() != src.t() {
                    return fail("a warped date per date");
                }
                if st.stage == 210 {
                    if (out.width, out.height) != (src.width, src.height) || out.geo_transform != src.geo_transform {
                        return fail("the same CRS and pixel should give the same grid");
                    }
                    let pin = self.series.pins.first()?.clone();
                    for t in 0..out.t() {
                        let v = janus_cube::with_thread_reader(&out, |r| r.read_pixel(t, pin.x, pin.y))?;
                        if v != pin.values[t] && !(v.is_nan() && pin.values[t].is_nan()) {
                            return fail("the same grid should keep every value");
                        }
                    }
                    self.remove_layer(k, ctx);
                    let w = &mut self.warp;
                    (w.target, w.format) = (1, 2);
                    w.srs = "EPSG:4326".into();
                    w.name = "Warped lonlat".into();
                    self.warp_run(ctx);
                    next(st, "warped onto its own grid: every value kept; then to EPSG:4326, virtual");
                } else {
                    if !srs.is_geographic() || !out.layers[0].path.ends_with(".vrt") {
                        return fail("warped VRTs in longitudes and latitudes expected");
                    }
                    self.remove_layer(k, ctx);
                    self.warp.show = false;
                    next(st, "warped to longitudes and latitudes as VRTs, opened as a series");
                }
                None
            }
            212 => {
                // the land cover's date polygonized again, then rasterized back on its grid
                self.open_polygonize(crate::contents::Sel::Series(self.session.as_ref()?.info.id));
                let p = &mut self.poly;
                (p.eight, p.min_area_ha, p.simplify_m, p.mean_of) = (false, 0.0, 0.0, None);
                p.name = "Patches again".into();
                st.texts = vec![self.polygonize_start(ctx)?];
                self.poly.show = false;
                next(st, "the land cover polygonized again, to rasterize");
                None
            }
            213 => {
                let gpkg = st.texts.first()?.clone();
                let v = self.vectors.iter().find(|v| v.data.path == gpkg)?;
                let id = v.id;
                self.open_rasterize(id);
                let r = &mut self.raster;
                (r.burn, r.field, r.all_touched) = (2, "value".into(), false);
                r.name = "Patches raster".into();
                self.rasterize_run(ctx);
                next(st, "the polygons' values rasterized on the series' grid");
                None
            }
            214 | 215 => {
                let name = if st.stage == 214 { "Patches raster" } else { "Patches classes" };
                let Some(k) = self.layers.iter().position(|l| l.name == name) else {
                    if self.zeit.jobs.iter().any(|j| {
                        j.tool == "rasterize" && j.status.lock().unwrap().state == crate::zeit::JobState::Failed
                    }) {
                        return fail("rasterize failed (Tools > Tasks > Log)");
                    }
                    return None;
                };
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) {
                    return None;
                }
                let out = self.layer_session(k)?.info.clone();
                let src = self.session.as_ref()?.info.clone();
                let read = |path: &str, band: usize| -> Option<Vec<f64>> {
                    let d = gdal::Dataset::open(path).ok()?;
                    let (w, h) = d.raster_size();
                    Some(d.rasterband(band).ok()?.read_as::<f64>((0, 0), (w, h), (w, h), None).ok()?.data().to_vec())
                };
                let l = src.layers.get(self.display.t.min(src.t() - 1))?;
                let want = read(&l.path, l.band.max(1) as usize)?;
                let got = read(&out.layers[0].path, 1)?;
                if st.stage == 214 {
                    let differ = want.iter().zip(&got).filter(|(a, b)| a != b).count();
                    println!(
                        "    {name}: {} x {} px, {differ} differ from the date polygonized",
                        out.width, out.height
                    );
                    if (out.width, out.height) != (src.width, src.height) || differ > 0 {
                        return fail("the polygons rasterized back should be the map they came from");
                    }
                    self.remove_layer(k, ctx);
                    let r = &mut self.raster;
                    (r.field, r.name) = ("class".into(), "Patches classes".into());
                    self.rasterize_run(ctx);
                    next(st, "rasterized back on the grid: the map polygonized, pixel for pixel");
                } else {
                    let mut named: Vec<i64> = want.iter().map(|&v| v as i64).collect();
                    named.sort_unstable();
                    named.dedup();
                    let masks = self.mask_layers();
                    println!(
                        "    {name}: classes {:?}; a tool's mask: {}",
                        out.class_names.values().collect::<Vec<_>>(),
                        masks.iter().any(|m| m.0 == out.id)
                    );
                    if out.class_names.len() != named.len() || !masks.iter().any(|m| m.0 == out.id) {
                        return fail("a class per name, and the layer offered as a tool's mask");
                    }
                    self.remove_layer(k, ctx);
                    self.raster.show = false;
                    // the polygons' zonal statistics: the land cover's classes at every date
                    let gpkg = st.texts.first()?.clone();
                    let id = self.vectors.iter().find(|v| v.data.path == gpkg)?.id;
                    self.open_zonal(id);
                    let guess = self.zonal_guess();
                    let n = self.session.as_ref()?.info.t();
                    let z = &mut self.zonal;
                    (z.source, z.kind, z.transitions, z.from, z.to) = (None, guess, true, 0, n - 1);
                    (z.id_field, z.name) = ("class".into(), "Patches zonal".into());
                    self.zonal_run(ctx);
                    next(st, "a text attribute rasterized as named classes, offered as a tool's mask");
                }
                None
            }
            216 => {
                // each polygon is a patch of the date polygonized: all its pixels in one class at that date
                if self.zonal.table.is_none() {
                    if self
                        .zeit
                        .jobs
                        .iter()
                        .any(|j| j.tool == "zonal" && j.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail("zonal statistics failed (Tools > Tasks > Log)");
                    }
                    return None;
                }
                let (t, meta) = self.zonal.table.clone()?;
                let gpkg = st.texts.first()?.clone();
                // each polygon's pixels, as polygonize wrote them
                let pixels: std::collections::HashMap<i64, u64> = {
                    use gdal::vector::LayerAccess;
                    let d = gdal::Dataset::open(&gpkg).ok()?;
                    let mut l = d.layer(0).ok()?;
                    let i = l.defn().field_index("pixels").ok()?;
                    l.features()
                        .map(|f| {
                            (f.fid().unwrap_or(0) as i64, f.field_as_integer64(i).ok().flatten().unwrap_or(0) as u64)
                        })
                        .collect()
                };
                let date = self.display.t.min(t.labels.len() - 1);
                let mut whole = 0;
                for (z, fid) in t.fids.iter().enumerate() {
                    let want = *pixels.get(fid)?;
                    let mine: Vec<_> = t.counts[date].iter().filter(|c| c.0 as usize == z).collect();
                    if mine.len() == 1 && mine[0].2 == want && t.pixels[z] == want {
                        whole += 1;
                    }
                }
                let moved: u64 = t.transitions.iter().map(|e| e.3).sum();
                let out = std::path::Path::new(&meta.folder);
                // the GeoPackage written, opened as a layer when loaded
                let opened = self.vectors.iter().find(|v| std::path::Path::new(&v.data.path).starts_with(out));
                opened?;
                let columns = opened.map_or(0, |v| v.data.fields.iter().filter(|f| f.starts_with("ha_")).count());
                println!(
                    "    Patches zonal ({:?}): {whole} of {} polygons wholly one class at {}, {moved} pixels in the transitions, {columns} ha_ columns on the GeoPackage",
                    t.kind,
                    t.fids.len(),
                    t.labels[date]
                );
                println!("    named by their class: {:?}", t.ids);
                if t.kind != janus_zeit::zonal::ZonalKind::Classes
                    || t.ids.iter().any(|n| !meta.names.values().any(|c| c == n))
                    || whole != t.fids.len()
                    || moved != t.pixels.iter().sum::<u64>()
                    || columns != t.values.len() * t.labels.len()
                {
                    return fail(
                        "each patch one class at its date, every pixel in the transitions, a column a class and date",
                    );
                }
                let out_layer = opened.map(|v| v.id);
                for path in [
                    Some(gpkg),
                    out_layer.and_then(|id| self.vectors.iter().find(|v| v.id == id)).map(|v| v.data.path.clone()),
                ]
                .into_iter()
                .flatten()
                {
                    if let Some(i) = self.vectors.iter().position(|v| v.data.path == path) {
                        self.remove_vector(i);
                    }
                }
                self.zonal.show = false;
                next(st, "the polygons' classes over time, every patch whole at its date");
                None
            }
            217 => {
                // the land cover saved as a NetCDF with its time axis, opened back
                let dir = std::env::temp_dir().join(format!("janus_selftest_series_{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).ok()?;
                let name = self.layers.get(self.active)?.name.clone();
                let sv = &mut self.save;
                (sv.format, sv.scope, sv.all_bands, sv.dont_open) =
                    (janus_zeit::series_io::SaveFormat::NetCdf, 0, false, false);
                sv.path = dir.join("land.nc").to_string_lossy().into_owned();
                self.save_run(ctx);
                st.texts = vec![name, dir.to_string_lossy().into_owned()];
                next(st, "the land cover saved as a NetCDF with a time axis");
                None
            }
            218 | 220 => {
                // the series saved (218) or built (220) against the land cover: its dates and every value
                let src = st.texts.first()?.clone();
                let name = if st.stage == 218 { format!("{src} saved") } else { "Land rebuilt".to_string() };
                let Some(k) = self.layers.iter().position(|l| l.name == name) else {
                    let tool = if st.stage == 218 { "save_series" } else { "build_series" };
                    if self
                        .zeit
                        .jobs
                        .iter()
                        .any(|j| j.tool == tool && j.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail(&format!("{tool} failed (Tools > Tasks > Log)"));
                    }
                    return None;
                };
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) {
                    return None;
                }
                let d = self.layers.iter().position(|l| l.name == src)?;
                let (a, b) = (self.layer_session(d)?.info.clone(), self.layer_session(k)?.info.clone());
                let days = |i: &janus_cube::CubeInfo| -> Vec<String> {
                    i.layers.iter().map(|l| janus_zeit::series_io::day_text(l.time)).collect()
                };
                let (w, h) = (a.width, a.height);
                let mut differ = 0;
                for t in 0..a.t().min(b.t()) {
                    let read = |i: &std::sync::Arc<janus_cube::CubeInfo>| {
                        let mut v = vec![0f32; w * h];
                        janus_cube::with_thread_reader(i, |r| r.read_window(t, (0, 0, w, h), &mut v, (w, h)))
                            .then_some(v)
                    };
                    let (va, vb) = (read(&a)?, read(&b)?);
                    differ += va.iter().zip(&vb).filter(|(x, y)| x != y).count();
                }
                println!(
                    "    {name}: {} dates ({} to {}), {} x {} px, {differ} values differ ({})",
                    b.t(),
                    days(&b).first()?,
                    days(&b).last()?,
                    b.width,
                    b.height,
                    b.description
                );
                if days(&a) != days(&b) || (b.width, b.height) != (w, h) || differ > 0 {
                    return fail("the same dates and values as the land cover");
                }
                self.remove_layer(k, ctx);
                if st.stage == 218 {
                    // the land cover's files built into a series again
                    let mut files: Vec<String> = a.layers.iter().map(|l| l.path.clone()).collect();
                    files.dedup();
                    self.build.name = "Land rebuilt".into();
                    self.build.cog = false;
                    self.build_plan_start(files);
                    next(st, "the NetCDF opened back: the land cover's dates and values");
                } else {
                    self.build.show = false;
                    self.save.show = false;
                    if let Some(dir) = st.texts.get(1) {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    next(st, "the land cover's files built into VRTs: its dates and values");
                }
                None
            }
            219 => {
                let p = self.build.plan.as_ref()?;
                println!(
                    "    planned: {} dates of {} band(s) ({}), {} left out",
                    p.dates.len(),
                    p.bands.len(),
                    p.bands.join(", "),
                    p.skipped.len()
                );
                if p.dates.len() != self.session.as_ref()?.info.t() || !p.skipped.is_empty() {
                    return fail("a date per file of the land cover");
                }
                self.build_run(ctx);
                next(st, "the land cover's files planned as a series, building");
                None
            }
            221 => {
                // a radar's passes in the STAC window: the one with the most scenes kept, its days alone
                let pass = |st: &str, r: i64, day: f64| janus_cube::stac::Item {
                    id: format!("{st}{r}_{day}"),
                    time: day * 86400.0,
                    orbit_state: Some(st.into()),
                    relative_orbit: Some(r),
                    ..Default::default()
                };
                let sx = &mut self.stac;
                sx.items = vec![
                    pass("ascending", 10, 19300.0),
                    pass("descending", 83, 19301.0),
                    pass("descending", 83, 19313.0),
                    pass("descending", 83, 19313.0),
                ];
                sx.orbits = janus_cube::stac::orbits(&sx.items);
                sx.orbit = 0;
                sx.regroup();
                let days: Vec<usize> = sx.dates.iter().map(|d| d.0.items.len()).collect();
                println!(
                    "    passes: {:?}, the first kept: {days:?} scenes a day",
                    sx.orbits.iter().map(|o| o.label()).collect::<Vec<_>>()
                );
                if sx.orbits.first().map(|o| o.label()).as_deref() != Some("descending 83") || days != [1, 2] {
                    return fail("the pass with the most scenes kept, two days of it");
                }
                (sx.items, sx.orbits, sx.dates) = (Vec::new(), Vec::new(), Vec::new());
                // a radar series of 6 dates (VV, VH, one look of speckle), a clearing where VH falls after the 3rd,
                // opened as a STAC radar search opens: in dB
                let dir = std::env::temp_dir().join(format!("janus_selftest_s1_{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).ok()?;
                let (w, h) = (96usize, 64usize);
                let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32720).ok()?.to_wkt().ok()?;
                let mut seed = 47u64;
                let mut speck = |base: f64| -> f32 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let u = ((seed >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
                    (base * -u.ln()) as f32
                };
                let drv = gdal::DriverManager::get_driver_by_name("GTiff").ok()?;
                for t in 0..6 {
                    let p = dir.join(format!("s1_2023{:02}01.tif", 2 * t + 1));
                    let mut d = drv.create_with_band_type::<f32, _>(&p, w, h, 2).ok()?;
                    d.set_geo_transform(&[494500.0, 10.0, 0.0, 9005150.0, 0.0, -10.0]).ok()?;
                    d.set_projection(&wkt).ok()?;
                    for b in 0..2 {
                        let v: Vec<f32> = (0..w * h)
                            .map(|i| {
                                let cleared = t >= 3 && (i % w) >= 48 && (i / w) >= 32;
                                speck(match (b, cleared) {
                                    (0, _) => 0.1,
                                    (_, false) => 0.03,
                                    (_, true) => 0.006,
                                })
                            })
                            .collect();
                        let mut rb = d.rasterband(b + 1).ok()?;
                        gdal::Metadata::set_description(&mut rb, ["VV", "VH"][b]).ok()?;
                        let mut buf = gdal::raster::Buffer::new((w, h), v);
                        rb.write((0, 0), (w, h), &mut buf).ok()?;
                    }
                }
                let ix = janus_cube::calc::indices::find(janus_cube::stac::display_index("sentinel-1-rtc")?)?;
                let sel = janus_cube::BandSelection {
                    expr: janus_cube::calc::ExprId::intern(&ix.name, &ix.formula),
                    chosen: true,
                    ..Default::default()
                };
                let status = Arc::new(Mutex::new(crate::zeit::JobStatus {
                    state: crate::zeit::JobState::Done,
                    progress: 1.0,
                    message: String::new(),
                    error: String::new(),
                    result: None,
                    seconds: 0.0,
                }));
                self.emb_downloads.push(crate::emb_download::EmbDownload {
                    title: "Radar".into(),
                    status,
                    started: std::time::Instant::now(),
                    folder: dir.to_string_lossy().into_owned(),
                    name: Some("Radar".into()),
                    keep_active: false,
                    sel: Some(sel),
                    opened: false,
                    ended: None,
                });
                st.texts = vec![dir.to_string_lossy().into_owned()];
                next(st, "a radar series opened in dB (VV)");
                None
            }
            222 | 223 => {
                // 222: the radar series shown as VV in dB; 223: it filtered by Quegan's, shown so too
                let name = if st.stage == 222 { "Radar" } else { "Radar filtered" };
                let Some(k) = self.layers.iter().position(|l| l.name == name) else {
                    if self
                        .zeit
                        .jobs
                        .iter()
                        .any(|j| j.tool == "speckle" && j.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail("the speckle filter failed (Tools > Tasks > Log)");
                    }
                    return None;
                };
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) {
                    return None;
                }
                let info = self.layer_session(k)?.info.clone();
                let (w, h) = (info.width, info.height);
                let read = |t: usize, band: i32| -> Option<Vec<f32>> {
                    let mut v = vec![0f32; w * h];
                    let ok = janus_cube::with_thread_reader(&info, |r| {
                        if band == 0 {
                            r.read_window(t, (0, 0, w, h), &mut v, (w, h))
                        } else {
                            r.read_band_window(t, band, (0, 0, w, h), &mut v, (w, h))
                        }
                    });
                    ok.then_some(v)
                };
                let (db, vv) = (read(1, 0)?, read(1, 1)?);
                let off = db.iter().zip(&vv).map(|(d, v)| (d - 10.0 * v.log10()).abs()).fold(0f32, f32::max);
                let cv = |v: &[f32]| {
                    let n = v.len() as f64;
                    let m = v.iter().map(|&x| f64::from(x)).sum::<f64>() / n;
                    (v.iter().map(|&x| (f64::from(x) - m).powi(2)).sum::<f64>() / n).sqrt() / m
                };
                // VH in dB in the clearing (the lower right quarter) before and after
                let quarter = |v: &[f32]| {
                    let s: Vec<f64> = (0..w * h)
                        .filter(|i| i % w >= 48 && i / w >= 32)
                        .map(|i| 10.0 * f64::from(v[i]).log10())
                        .collect();
                    s.iter().sum::<f64>() / s.len() as f64
                };
                let fall = quarter(&read(4, 2)?) - quarter(&read(1, 2)?);
                println!(
                    "    {name}: {} dates of {:?}, shown as {}: {off:.1e} dB from 10 log10 VV; VV's variation {:.2}; the clearing's VH {fall:+.1} dB",
                    info.t(),
                    info.band_names,
                    info.selection_text(),
                    cv(&vv)
                );
                if info.selection_text() != "VVdB" || off > 1e-4 || info.t() != 6 {
                    return fail("6 dates shown as VV in dB");
                }
                if st.stage == 222 {
                    if (cv(&vv) - 1.0).abs() > 0.15 {
                        return fail("one look of speckle: a variation of about 1");
                    }
                    self.speckle.filter = 0;
                    self.speckle.size = 7;
                    self.speckle.name = "Radar filtered".into();
                    self.speckle_run(ctx);
                    next(st, "the radar series in dB; Quegan's filter under way");
                } else {
                    // the speckle averaged over 6 dates and a 7 x 7 window, the clearing kept
                    if cv(&vv) > 0.55 || fall > -5.0 {
                        return fail("less speckle, the clearing's fall in VH kept");
                    }
                    for n in ["Radar filtered", "Radar"] {
                        if let Some(i) = self.layers.iter().position(|l| l.name == n) {
                            self.remove_layer(i, ctx);
                        }
                    }
                    self.speckle.show = false;
                    if let Some(dir) = st.texts.first() {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    next(st, "Quegan's filter: the speckle averaged, the clearing kept, shown in dB");
                }
                None
            }
            224 => {
                // a series of 24 months (2022-2023), its left half greening in March, its right half in September,
                // a cloud over its top rows in the 6th month
                let dir = std::env::temp_dir().join(format!("janus_selftest_seasons_{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).ok()?;
                let (w, h) = (40usize, 30usize);
                let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32720).ok()?.to_wkt().ok()?;
                let drv = gdal::DriverManager::get_driver_by_name("GTiff").ok()?;
                for k in 0..24 {
                    let (year, month) = (2022 + k / 12, k % 12 + 1);
                    let day = janus_cube::dates::days_from_civil(year, month as u32, 15);
                    let frac = janus_cube::dates::decimal_year(day as f64 * 86400.0).rem_euclid(1.0);
                    let p = dir.join(format!("ndvi_{year}-{month:02}-15.tif"));
                    let mut d = drv.create_with_band_type::<f32, _>(&p, w, h, 1).ok()?;
                    d.set_geo_transform(&[494500.0, 10.0, 0.0, 9005150.0, 0.0, -10.0]).ok()?;
                    d.set_projection(&wkt).ok()?;
                    let v: Vec<f32> = (0..w * h)
                        .map(|i| {
                            if k == 5 && i / w < 6 {
                                return f32::NAN;
                            }
                            let peak = if i % w < w / 2 { 0.2 } else { 0.7 };
                            (0.5 + 0.3 * (std::f64::consts::TAU * (frac - peak)).cos()) as f32
                        })
                        .collect();
                    let mut rb = d.rasterband(1).ok()?;
                    rb.set_no_data_value(Some(f64::NAN)).ok()?;
                    rb.write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), v)).ok()?;
                }
                let status = Arc::new(Mutex::new(crate::zeit::JobStatus {
                    state: crate::zeit::JobState::Done,
                    progress: 1.0,
                    message: String::new(),
                    error: String::new(),
                    result: None,
                    seconds: 0.0,
                }));
                self.emb_downloads.push(crate::emb_download::EmbDownload {
                    title: "Seasons".into(),
                    status,
                    started: std::time::Instant::now(),
                    folder: dir.to_string_lossy().into_owned(),
                    name: Some("Seasons".into()),
                    keep_active: false,
                    sel: None,
                    opened: false,
                    ended: None,
                });
                st.texts = vec![dir.to_string_lossy().into_owned()];
                next(st, "a series of two years, greening in March on the left, in September on the right");
                None
            }
            225 => {
                // the seasonal peak, the share of valid pixels, the dates of a month and the histogram's highlight
                let k = self.layers.iter().position(|l| l.name == "Seasons")?;
                if self.opening.is_some() || !super::loaded(self.layer_session(k)) || self.active != k {
                    return None;
                }
                if !self.mode_available(janus_gpu::Mode::Phase) {
                    return fail("the seasonal peak available for 24 dates round the year");
                }
                let s = self.session.as_ref()?;
                let ov = &s.s.overview;
                let times = self.display.gpu_times(s);
                let (left, right) = (15 * ov.w + 5, 15 * ov.w + ov.w - 6);
                let (pl, pr) = (self.display.phase_at(s, &times, left)?, self.display.phase_at(s, &times, right)?);
                println!(
                    "    peaks {} and {}, amplitudes {:.3} and {:.3}",
                    crate::phase::date_of(pl[0]),
                    crate::phase::date_of(pr[0]),
                    pl[1],
                    pr[1]
                );
                if (pl[0] - 0.2).abs() > 0.01 || (pr[0] - 0.7).abs() > 0.01 || (pl[1] - 0.6).abs() > 0.01 {
                    return fail("the peaks in March and September, a peak-to-peak amplitude of 0.6");
                }
                let full = [0.0, 0.0, ov.w as f64, ov.h as f64];
                let (clean, cloudy) =
                    (crate::mosaic::valid_share(s, full, 0, 100_000), crate::mosaic::valid_share(s, full, 5, 100_000));
                println!(
                    "    valid: {:.0} % on the first date, {:.0} % under the cloud",
                    clean * 100.0,
                    cloudy * 100.0
                );
                if (clean - 1.0).abs() > 1e-6 || (cloudy - 0.8).abs() > 0.01 {
                    return fail("every pixel valid on the first date, 80 % on the cloudy one");
                }
                // the March dates of both years
                self.display.t = 2;
                self.analysis.mosaic.which = 1;
                let march = self.mosaic_dates(self.session.as_ref()?);
                self.analysis.mosaic.which = 0;
                if march != [2, 14] {
                    return fail(&format!("the March dates of every year: {march:?}"));
                }
                // the map: the left half's hue that of March, as the CPU makes it
                self.test_fit();
                self.display.mode = janus_gpu::Mode::Phase;
                self.display.phase.by_mean = false;
                let r = &mut self.display.ranges[janus_gpu::Mode::Phase as usize];
                (r.lo, r.hi, r.manual) = (0.0, 0.6, true);
                let img = self.test_render()?;
                let (cx, cy) = self.test_pixel_of(10, 15)?;
                let got = &img[(cy * super::SIZE[0] as usize + cx) * 4..][..3];
                let want = janus_gpu::phase::hsv(pl[0], (pl[1] / 0.6).min(1.0), 1.0).map(|c| c * 255.0);
                if (0..3).any(|c| (f32::from(got[c]) - want[c]).abs() > 3.0) {
                    return fail(&format!("the seasonal peak's colour {got:?}, not {want:?}"));
                }
                // the first date (January) with the values 0.6-0.7 highlighted: the left half (0.66) in its
                // colour, the right half (0.34) grey
                self.display.mode = janus_gpu::Mode::Value;
                self.display.t = 0;
                self.display.highlight = Some((janus_gpu::Mode::Value, 0.6, 0.7));
                let r = &mut self.display.ranges[janus_gpu::Mode::Value as usize];
                (r.lo, r.hi, r.manual) = (0.2, 0.8, true);
                let img = self.test_render()?;
                let at = |ix: usize| -> Option<[u8; 3]> {
                    let (cx, cy) = self.test_pixel_of(ix, 15)?;
                    let p = &img[(cy * super::SIZE[0] as usize + cx) * 4..][..3];
                    Some([p[0], p[1], p[2]])
                };
                let (lc, rc) = (at(10)?, at(30)?);
                let grey = |c: [u8; 3]| c.iter().max().unwrap() - c.iter().min().unwrap() <= 3 && c[0] < 110;
                println!("    highlighted 0.6-0.7: the left half {lc:?}, the right half {rc:?}");
                if grey(lc) || !grey(rc) {
                    return fail("the highlighted values in colour, the others grey");
                }
                self.display.highlight = None;
                self.display.phase.by_mean = true;
                next(st, "the seasonal peak, the valid share, the dates of a month and the highlight");
                None
            }
            226 => {
                // pins dropped by class (the class new pins get): 8 on the left half (greening in March), 8 on the
                // right (September); coloured by class, kept through the project; a Random Forest of 60 trees on them
                let k = self.layers.iter().position(|l| l.name == "Seasons")?;
                if self.active != k {
                    return None;
                }
                self.clear_pins();
                self.ml.pin_class_list.clear();
                self.ml.pins_by_class = true;
                for (class, x0) in [("Early", 2), ("Late", 22)] {
                    self.ml.drop_class = class.into();
                    for i in 0..8 {
                        self.drop_pin(x0 + 2 * i, 3 + 3 * i);
                    }
                }
                self.ml.drop_class.clear();
                let colour = |k: usize| {
                    let [r, g, b] = crate::ml::class_color(k);
                    egui::Color32::from_rgb(r, g, b)
                };
                let classed = self.series.pins.iter().all(|p| {
                    let c = self.ml.pin_classes.get(&p.id).map(String::as_str);
                    let want = if p.x < 20 { "Early" } else { "Late" };
                    c == Some(want) && p.color == colour(usize::from(p.x >= 20))
                });
                if self.series.pins.len() != 16 || !classed || self.ml.pin_class_list != ["Early", "Late"] {
                    return fail("16 pins dropped, each of its class and in its colour");
                }
                // the project keeps each pin's class
                let saved = self.project_state(None);
                self.clear_pins();
                self.restore_pins(&saved.pins, &saved.pin_classes, &saved.pin_class_list);
                let again = self.project_state(None);
                if again.pin_classes != saved.pin_classes
                    || saved.pin_classes.iter().filter(|c| *c == "Late").count() != 8
                {
                    return fail(&format!("the pins' classes kept by the project: {:?}", again.pin_classes));
                }
                self.ml.source = 3;
                self.ml.arch = crate::ml::FOREST;
                self.ml.forest = janus_ml::forest::ForestHyper { trees: 60, ..Default::default() };
                self.ml.cfg = janus_ml::TrainConfig { validation_split: 0.25, ..janus_ml::TrainConfig::tempcnn() };
                self.ml.shown = true;
                self.ml.bands.clear();
                self.ml.scope = 0;
                self.ml.aoa = true;
                self.ml.post = Default::default();
                let (sites, classes) = match self.ml_sites() {
                    Ok(s) => s,
                    Err(e) => return fail(&format!("the pins should give samples: {e}")),
                };
                if classes != ["Early", "Late"] || sites.len() != 16 {
                    return fail(&format!("16 pins in the classes Early, Late, got {} in {classes:?}", sites.len()));
                }
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-forest-{stamp}"));
                self.zeit.results_dir = Some(dir.clone());
                st.texts.push(dir.to_string_lossy().into_owned());
                self.ml.run = None;
                self.ml_train_now(ctx);
                next(st, "pins dropped by class (Early, Late), coloured by it and kept by the project; a forest grown");
                None
            }
            227 => {
                let run = self.ml.run.clone()?;
                let r = run.lock().ok()?;
                if !r.done {
                    return None;
                }
                if !r.error.is_empty() {
                    return fail(&format!("the forest should grow: {}", r.error));
                }
                let h = r.header.clone()?;
                let (path, trees) = (r.model_path.clone(), r.epochs.last().map_or(0, |e| e.epoch));
                drop(r);
                self.pump_ml();
                let imp: Vec<f64> = h.training["importance"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
                    .unwrap_or_default();
                let (oa, oob) = (
                    h.training["validation"]["overall_accuracy"].as_f64().unwrap_or(f64::NAN),
                    h.training["oob"]["overall_accuracy"].as_f64().unwrap_or(f64::NAN),
                );
                println!(
                    "    {}: {trees} trees, validation accuracy {oa:.2}, out of bag {oob:.2}, {} importances",
                    h.arch.name(),
                    imp.len()
                );
                let forest = matches!(h.arch, janus_ml::Arch::RandomForest(_));
                if !forest || trees != 60 || oa < 0.99 || oob < 0.99 || imp.len() != 24 {
                    return fail("a forest of 60 trees telling the halves apart, an importance for each date");
                }
                if (imp.iter().sum::<f64>() - 1.0).abs() > 1e-6 || h.applicability.is_none() {
                    return fail("the importances summing to 1, the area of applicability kept");
                }
                let model = match janus_ml::Model::load(&path, &janus_ml::model::shared_device(false).0) {
                    Ok(m) => m,
                    Err(e) => return fail(&format!("the forest should read back from its file: {e}")),
                };
                if model.forest().map(|f| f.trees()) != Some(60) || self.ml.model.as_ref().map(|m| &m.0) != Some(&path)
                {
                    return fail("the forest kept in its file and the model to apply");
                }
                self.ml_apply_now(ctx);
                next(st, "the forest: validation and out-of-bag accuracy, each date's importance; applied");
                None
            }
            228 => {
                let tag = st.texts.get(1)?.clone();
                let job = self.zeit.jobs.last()?;
                let (state, error) = {
                    let s = job.status.lock().ok()?;
                    (s.state, s.error.clone())
                };
                match state {
                    crate::zeit::JobState::Failed | crate::zeit::JobState::Cancelled => {
                        return fail(&format!("the forest's classification should finish: {error}"));
                    }
                    crate::zeit::JobState::Running => return None,
                    crate::zeit::JobState::Done => {}
                }
                let mine: Vec<usize> =
                    (0..self.zeit.results.len()).filter(|&k| self.zeit.results[k].path.starts_with(&tag)).collect();
                if mine.len() < 3
                    || mine.iter().any(|&k| self.zeit.results[k].data.is_empty())
                    || self.zeit.loads_pending()
                {
                    return None;
                }
                let class = mine
                    .iter()
                    .map(|&k| &self.zeit.results[k])
                    .find(|r| !r.classes.is_empty() && r.name.contains(": Class"))?;
                // every pixel of the left half Early (1), of the right half Late (2)
                let wrong = (0..class.th)
                    .flat_map(|y| (0..class.tw).map(move |x| (x, y)))
                    .filter(|&(x, y)| {
                        let want = if (x * class.w / class.tw) < 20 { 1.0 } else { 2.0 };
                        class.data[y * class.tw + x] != want
                    })
                    .count();
                println!(
                    "    the forest's class map: {:?}, {wrong} pixels of {} wrong",
                    class.classes,
                    class.data.len()
                );
                if class.classes != ["Early", "Late"] || wrong > 0 {
                    return fail("the class map: Early on the left half, Late on the right");
                }
                next(st, "Random Forest: its class map Early on the left half, Late on the right");
                // the samples checked as they are gathered: the window open on them
                self.ml.open = true;
                self.ml.tab = 0;
                None
            }
            229 => {
                let r = self.ml.check.current()?;
                if r.samples != 16 {
                    return None;
                }
                let w = crate::sample_check::warnings(r);
                println!(
                    "    checked: {} samples, estimated accuracy {:.2} ({} folds), {} suspect, {} warnings",
                    r.samples,
                    r.accuracy,
                    r.folds,
                    r.suspects(),
                    w.len()
                );
                let sig = r.signatures.len() == 2 && r.signatures.iter().all(|s| s.len() == 24 && s[0][1].is_finite());
                if r.accuracy < 0.99 || r.suspects() != 0 || w.iter().any(|x| x.level > 1) || !sig {
                    return fail("16 pins of two halves: all fitting their class, each class's typical series");
                }
                // a pin of the left half given the right half's class: taken for what it is
                let id = self.series.pins.iter().find(|p| p.x == 2)?.id;
                self.set_pin_class(id, "Late");
                st.texts.push(id.to_string());
                next(st, "the samples checked: the estimated accuracy, each class's series, no warning to fix");
                None
            }
            230 => {
                let r = self.ml.check.current()?;
                let id: u32 = st.texts.last()?.parse().ok()?;
                let p = r.places.iter().find(|p| p.pin == Some(id))?;
                let w = crate::sample_check::warnings(r);
                println!(
                    "    pin {id} (Late) fits Late {:.2}, looks like {} {:.2}; {}",
                    p.own,
                    r.classes[p.likely].name,
                    p.likely_p,
                    w.first().map_or("", |x| x.text.as_str())
                );
                if !p.suspect() || !r.suspect_pins().contains(&id) || w.first().is_none_or(|x| x.level != 2) {
                    return fail("the mislabelled pin should look like its half's class, a warning to fix");
                }
                self.ml.open = false;
                let k = self.layers.iter().position(|l| l.name == "Seasons")?;
                self.remove_layer(k, ctx);
                self.clear_pins();
                st.texts.pop();
                for dir in &st.texts {
                    let _ = std::fs::remove_dir_all(dir);
                }
                next(st, "a pin given the wrong class: flagged as looking like another, ringed on the map");
                None
            }
            231 => {
                // the validation's folder (stages 170-179), its strata layer open until now
                let _ = std::fs::remove_dir_all(&st.dir);
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                Some(0)
            }
            _ => None,
        }
    }
}
