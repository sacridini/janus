//! The notice after a failure (IDEIAS.md, phase 58): at the start, when the last run did not end normally (a panic,
//! a native crash, a kill: crash.rs), a window with the report, to restore the session, open the folder, copy the
//! report or open an issue with it filled in. Nothing is sent by itself. Also `--crash-test panic|native`.

use egui::{Id, RichText, ScrollArea, TextEdit, TextStyle};

use crate::app::State;
use crate::crash::{self, Closed};

/// A frame of the run after which `--crash-test` fails (the window is up by then).
const TEST_FRAME: u64 = 30;

/// The window's state.
#[derive(Default)]
pub struct CrashNotice {
    /// what the last run left, until the notice is dismissed
    pub closed: Option<Closed>,
    /// the buttons drawn at the last frame (the self-test reads them)
    pub buttons: Vec<&'static str>,
}

impl CrashNotice {
    /// At the start of a window: the old reports cleaned, what the last run left, this run's marker written.
    pub fn at_start() -> Self {
        CrashNotice { closed: crash::start_window(), buttons: Vec::new() }
    }
}

enum Act {
    Restore,
    Folder,
    Copy,
    Issue,
    Dismiss,
}

impl State {
    /// `--crash-test`: the failure asked for, on the main thread once the window has been drawn for a while.
    pub(crate) fn pump_crash_test(&mut self) {
        if self.frame >= TEST_FRAME
            && let Some(mode) = self.opts.crash_test.clone()
        {
            self.opts.crash_test = None;
            crash::provoke(&mode);
        }
    }

    /// The notice dismissed: the dead markers go, so the next start does not say it again.
    pub(crate) fn dismiss_crash_notice(&mut self) {
        if let Some(c) = self.crash_notice.closed.take() {
            crash::dismiss(&c);
        }
        self.crash_notice.buttons.clear();
    }

    /// "Janus closed unexpectedly at ...", with the report.
    pub(crate) fn crash_window(&mut self, ctx: &egui::Context) {
        let Some(closed) = self.crash_notice.closed.take() else { return };
        let can_restore = crate::project::session_path().is_file();
        let mut buttons: Vec<&'static str> = Vec::new();
        let screen = ctx.content_rect();
        let m = egui::Modal::new(Id::new("crash notice")).show(ctx, |ui| {
            let width = (crate::fit::scaled(ui, 640.0)).min(screen.width() - 80.0).max(240.0);
            ui.set_width(width);
            ui.heading("Janus closed unexpectedly");
            ui.label(format!("It stopped at {}.", crash::pretty(closed.when)));
            if let Some(name) = closed.file.file_name() {
                ui.weak(format!("Report: {}", name.to_string_lossy()));
            }
            if closed.earlier > 0 {
                ui.weak(format!("{} earlier runs also ended badly; their reports are in the folder.", closed.earlier));
            }
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Nothing is sent by Janus itself. Read the report below before you send it: file paths in \
                         it may hold the names of people.",
                    )
                    .weak(),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            ScrollArea::both().max_height((screen.height() - 260.0).clamp(120.0, 320.0)).show(ui, |ui| {
                let mut text: &str = &closed.text;
                ui.add(TextEdit::multiline(&mut text).font(TextStyle::Monospace).desired_width(f32::INFINITY));
            });
            ui.add_space(6.0);
            let mut act = None;
            ui.horizontal_wrapped(|ui| {
                let mut button = |ui: &mut egui::Ui, label: &'static str, tip: &str, a: Act| {
                    buttons.push(label);
                    if ui.button(label).on_hover_text(tip).clicked() {
                        act = Some(a);
                    }
                };
                if can_restore {
                    button(ui, "Restore the session", "Open the layers that were open at the last save", Act::Restore);
                }
                button(ui, "Open folder", "The folder with the reports", Act::Folder);
                button(ui, "Copy report", "The report's text to the clipboard", Act::Copy);
                button(ui, "Open an issue", "The browser, with the report in a new issue to read and send", Act::Issue);
                button(ui, "Dismiss", "Close this notice; the reports stay in the folder", Act::Dismiss);
            });
            act
        });
        self.crash_notice.buttons = buttons;
        let close = m.should_close();
        let act = m.inner.or(close.then_some(Act::Dismiss));
        self.crash_notice.closed = Some(closed);
        let Some(act) = act else { return };
        match act {
            Act::Restore => {
                self.reopen_session(ctx);
                self.dismiss_crash_notice();
            }
            Act::Folder => crate::platform::open_in_explorer(&crash::dir().to_string_lossy()),
            Act::Copy => {
                if let Some(c) = &self.crash_notice.closed {
                    ctx.copy_text(c.text.clone());
                }
            }
            Act::Issue => {
                if let Some(c) = &self.crash_notice.closed {
                    let title = format!("Janus {} closed unexpectedly", env!("CARGO_PKG_VERSION"));
                    ctx.open_url(egui::OpenUrl::new_tab(crash::issue_url(&title, &c.text)));
                }
            }
            Act::Dismiss => self.dismiss_crash_notice(),
        }
    }
}
