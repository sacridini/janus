//! `--selftest-ui` stages 330-333: the notice after a failure (crash_ui.rs, crash.rs). 330 puts a report and the
//! marker of a run that is gone in a folder of its own (the test's, not the user's), as a crash leaves them, and looks
//! for them as the start does; 331 the window is drawn with its buttons; 332 dismissed, it goes and the next start
//! finds nothing; 333 the folder removed. The last stage of the whole test follows (299).

use std::path::PathBuf;
use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::crash::{self, Facts};

/// A pid no process has (the marker is of a run that is gone).
const GONE: u32 = 0xFFFF_FFF0;

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct CrashTest {
    dir: PathBuf,
}

impl State {
    pub(super) fn test_crash_notice(&mut self, st: &mut UiTest, t0: Instant, _ctx: &egui::Context) -> Option<i32> {
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
        let own = std::process::id();
        match stage {
            330 => {
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-crash-{stamp}"));
                let _ = std::fs::create_dir_all(&dir);
                crash::set_dir(Some(dir.clone()));
                st.crash = CrashTest { dir: dir.clone() };
                let now = crash::now_secs();
                let text = crash::format_report(&Facts {
                    kind: "panic",
                    secs: now,
                    thread: "main",
                    message: "fake failure of the self-test",
                    location: "src/ui_test_crash.rs:1:1",
                    backtrace: "   0: a frame\n",
                    log: &["[00:00:00] a line of the log".to_string()],
                });
                if crash::write_file(&dir, now, "txt", text.as_bytes()).is_none() {
                    return fail("the report could not be written");
                }
                crash::write_marker(&dir, GONE, now - 120);
                let found = crash::check_previous(&dir, own, &crash::process_alive, now);
                let Some(found) = found else { return fail("a report and a dead marker, and the start finds nothing") };
                if !found.text.contains("fake failure of the self-test")
                    || found.earlier != 0
                    || found.markers.len() != 1
                {
                    return fail(&format!("the wrong report was found: {:?}", found.text));
                }
                self.crash_notice.closed = Some(found);
                next(st, "a report and the marker of a run that is gone: found as the start finds them");
                None
            }
            331 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let shown = self.crash_notice.buttons.clone();
                let mut want = vec!["Open folder", "Copy report", "Open an issue", "Dismiss"];
                if crate::project::session_path().is_file() {
                    want.insert(0, "Restore the session");
                }
                if shown != want {
                    return fail(&format!("the notice's buttons are {shown:?}, expected {want:?}"));
                }
                let Some(c) = &self.crash_notice.closed else { return fail("the notice is gone") };
                let url = crash::issue_url("Janus closed unexpectedly", &c.text);
                if !url.starts_with("https://github.com/sacridini/janus/issues/new?title=") || url.len() > 8000 {
                    return fail(&format!("the issue's URL is {} bytes: {url:.80}", url.len()));
                }
                next(st, "the notice is drawn with its buttons; the issue's URL is within the limit");
                None
            }
            332 => {
                if st.frames == 0 {
                    self.dismiss_crash_notice();
                }
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                if self.crash_notice.closed.is_some() || !self.crash_notice.buttons.is_empty() {
                    return fail("the notice is still there after Dismiss");
                }
                let again = crash::check_previous(&st.crash.dir, own, &crash::process_alive, crash::now_secs());
                if again.is_some() || !crash::dead_markers(&st.crash.dir, own, &crash::process_alive).is_empty() {
                    return fail("the next start finds the failure again after Dismiss");
                }
                next(st, "dismissed: the window is gone and the next start finds nothing");
                None
            }
            333 => {
                crash::set_dir(None);
                let _ = std::fs::remove_dir_all(&st.crash.dir);
                st.crash = Default::default();
                next(st, "the test's crashes folder removed");
                st.stage = 299;
                None
            }
            _ => None,
        }
    }
}
