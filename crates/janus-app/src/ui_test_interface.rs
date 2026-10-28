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
            200 => {
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                Some(0)
            }
            _ => None,
        }
    }
}
