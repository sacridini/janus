//! `--selftest-ui` stages 180-199: the interface of phases 48-55, on what the validation stages left open (D). 180:
//! the status bar (the cursor's coordinates in longitude and latitude, the scale) and the notices of tasks that end
//! (a short task none, a long one and a failed one each one).

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
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                Some(0)
            }
            _ => None,
        }
    }
}
