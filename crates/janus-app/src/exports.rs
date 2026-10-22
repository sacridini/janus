//! Files written in the background (src/app_export.cpp: startExportJob, uiExports): a job per file on a thread of
//! its own, its progress and its end in the Exports window. Series extracted at vector features and labels use
//! it; the map's exports join it with phase 9.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::app::State;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportState {
    Running,
    Done,
    Failed,
    Cancelled,
}

struct Outcome {
    state: ExportState,
    error: String,
    seconds: f64,
}

pub struct ExportJob {
    pub title: String,
    pub path: String,
    outcome: Mutex<Outcome>,
    /// [0, 1], as f64 bits
    progress: AtomicU64,
    pub cancel: AtomicBool,
    started: Instant,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    ctx: egui::Context,
}

impl ExportJob {
    pub fn state(&self) -> ExportState {
        self.outcome.lock().unwrap().state
    }

    pub fn error(&self) -> String {
        self.outcome.lock().unwrap().error.clone()
    }

    pub fn seconds(&self) -> f64 {
        let o = self.outcome.lock().unwrap();
        if o.state == ExportState::Running { self.started.elapsed().as_secs_f64() } else { o.seconds }
    }

    pub fn progress(&self) -> f64 {
        f64::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// Progress in [0, 1]; it wakes the interface.
    pub fn set_progress(&self, p: f64) {
        self.progress.store(p.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        self.ctx.request_repaint();
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Waits for the job to end.
    pub fn wait(&self) {
        if let Some(h) = self.thread.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

/// Runs `work` on a thread of its own as an export of `path`: Err(why) on failure; cancelled when it returns after
/// `cancel` was set.
pub fn start_export_job(
    title: String,
    path: String,
    ctx: &egui::Context,
    work: impl FnOnce(&ExportJob) -> Result<(), String> + Send + 'static,
) -> Arc<ExportJob> {
    let job = Arc::new(ExportJob {
        title,
        path,
        outcome: Mutex::new(Outcome { state: ExportState::Running, error: String::new(), seconds: 0.0 }),
        progress: AtomicU64::new(0f64.to_bits()),
        cancel: AtomicBool::new(false),
        started: Instant::now(),
        thread: Mutex::new(None),
        ctx: ctx.clone(),
    });
    let j = job.clone();
    let handle = std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let r = work(&j);
            let mut o = j.outcome.lock().unwrap();
            o.seconds = j.started.elapsed().as_secs_f64();
            (o.state, o.error) = match r {
                _ if j.cancelled() => (ExportState::Cancelled, String::new()),
                Ok(()) => (ExportState::Done, String::new()),
                Err(e) => (ExportState::Failed, e),
            };
            if o.state == ExportState::Done {
                j.progress.store(1f64.to_bits(), Ordering::Relaxed);
            }
            drop(o);
            j.ctx.request_repaint();
        })
        .expect("a thread for the export");
    *job.thread.lock().unwrap() = Some(handle);
    job
}

/// Writes `text` as `path` (through a temporary file renamed when complete).
pub fn write_text_file(path: &str, text: &str) -> Result<(), String> {
    let tmp = format!("{path}.part");
    let r = std::fs::write(&tmp, text).and_then(|_| {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
    });
    r.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {path}: {e}")
    })
}

/// A name for a file: what a file system takes.
pub fn sanitize(name: &str) -> String {
    name.chars().map(|c| if "\\/:*?\"<>| ".contains(c) { '_' } else { c }).collect()
}

impl State {
    /// Where exports go: the folder chosen last, else the active series' folder.
    pub fn export_folder(&self) -> String {
        let d = &self.settings.s.export_dir;
        if !d.is_empty() && std::path::Path::new(d).is_dir() {
            return d.clone();
        }
        self.session
            .as_ref()
            .and_then(|s| std::path::absolute(&s.info.first_path).ok())
            .and_then(|p| p.parent().map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }

    /// The folder of a file written: the next exports go there.
    pub fn remember_export_folder(&mut self, path: &str) {
        if let Some(d) = std::path::Path::new(path).parent() {
            self.settings.s.export_dir = d.to_string_lossy().into_owned();
            self.settings.changed();
        }
    }

    /// The Exports window: each file being written or written, its progress, cancel, its folder.
    pub(crate) fn exports_window(&mut self, ctx: &egui::Context) {
        if !self.show_exports {
            return;
        }
        let mut open = true;
        let mut remove = None;
        egui::Window::new("Exports").open(&mut open).default_size([600.0, 260.0]).show(ctx, |ui| {
            if self.exports.is_empty() {
                ui.weak("Nothing exported yet.");
            }
            for (i, j) in self.exports.iter().enumerate() {
                ui.push_id(i, |ui| {
                    let st = j.state();
                    ui.label(&j.title);
                    ui.weak(&j.path);
                    let name = match st {
                        ExportState::Running => "writing",
                        ExportState::Done => "done",
                        ExportState::Cancelled => "cancelled",
                        ExportState::Failed => "failed",
                    };
                    ui.horizontal(|ui| {
                        let p = if st == ExportState::Done { 1.0 } else { j.progress() };
                        let bw = ui.text_style_height(&egui::TextStyle::Body) * 4.5;
                        ui.add(
                            egui::ProgressBar::new(p as f32)
                                .desired_width((ui.available_width() - 2.0 * bw - 8.0).max(60.0))
                                .text(format!("{name}  {:.0}%  {:.1} s", p * 100.0, j.seconds())),
                        );
                        if st == ExportState::Running {
                            if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(bw, 0.0))).clicked() {
                                j.cancel.store(true, Ordering::Relaxed);
                            }
                        } else {
                            if ui.add(egui::Button::new("Folder").min_size(egui::vec2(bw, 0.0))).clicked()
                                && let Some(d) = std::path::Path::new(&j.path).parent()
                            {
                                crate::platform::open_in_explorer(&d.to_string_lossy());
                            }
                            if ui.add(egui::Button::new("Remove").min_size(egui::vec2(bw, 0.0))).clicked() {
                                remove = Some(i);
                            }
                        }
                    });
                    if st == ExportState::Failed {
                        ui.colored_label(theme::error(), j.error());
                    }
                    ui.separator();
                });
            }
        });
        if let Some(i) = remove {
            self.exports.remove(i);
        }
        self.show_exports = open;
    }
}
