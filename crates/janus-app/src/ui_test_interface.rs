//! `--selftest-ui` stages 180-199: the interface of phases 48-55, on what the validation stages left open (D). 180:
//! the status bar (the cursor's coordinates in longitude and latitude, the scale) and the notices of tasks that end
//! (a short task none, a long one and a failed one each one). 181: the command palette (what a query finds first,
//! a command run from it, the recent ones first).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::UiTest;
use crate::app::State;
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
                let jobs = ["short task", "long task", "failing task"].map(fake_job);
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
                let got = self.notice_titles();
                self.zeit.jobs.truncate(first);
                let want = [("long task".to_string(), false), ("failing task".to_string(), true)];
                if got != want {
                    return fail(&format!("notices {got:?}, expected {want:?}"));
                }
                println!("    notices: {got:?}");
                self.clear_notices();
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
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                Some(0)
            }
            _ => None,
        }
    }
}
