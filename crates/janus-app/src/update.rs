//! Help > Check for updates: the latest release on GitHub against the version running. On Windows, when Janus was
//! installed by its installer, the new installer is downloaded and run in place (Janus closes, the installer puts the
//! new version in its folder and opens it again, installer/janus.iss's /RELAUNCH); elsewhere, or from a build, the
//! release's package opens in the browser. Checked once at the start too (Settings: "Check for updates when Janus
//! starts"): when there is a newer version, a notice and, on the right of the menu bar, the version and an Update
//! button (the installed program downloads and installs it from there).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use egui::{RichText, Ui};
use serde_json::Value;

use crate::app::State;
use crate::theme;

/// The version running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const LATEST: &str = "https://api.github.com/repos/sacridini/janus/releases/latest";
pub const RELEASES: &str = "https://github.com/sacridini/janus/releases";

/// A release on GitHub: its version, its date, its notes, its page, this platform's package (URL, bytes).
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub date: String,
    pub notes: String,
    pub page: String,
    pub package: Option<(String, u64)>,
}

/// "v1.4.1", "1.4.1-rc1": (1, 4, 1).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split(['-', '+']).next()?;
    let mut parts = s.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    Some((major, minor, patch))
}

/// `a` is a version after `b`.
pub fn newer(a: &str, b: &str) -> bool {
    matches!((parse_version(a), parse_version(b)), (Some(a), Some(b)) if a > b)
}

/// The end of this platform's package's name (tools/package.py).
fn package_suffix() -> &'static str {
    if cfg!(windows) {
        "-setup.exe"
    } else if cfg!(target_os = "macos") {
        ".dmg"
    } else {
        "-linux-x86_64.tar.xz"
    }
}

/// GitHub's answer for a release (GET /repos/{owner}/{repo}/releases/latest).
pub fn parse_release(json: &[u8], suffix: &str) -> Result<Release, String> {
    let v: Value = serde_json::from_slice(json).map_err(|e| format!("GitHub's answer could not be read: {e}"))?;
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let version = text("tag_name").trim_start_matches(['v', 'V']).to_string();
    if parse_version(&version).is_none() {
        return Err(match v.get("message").and_then(Value::as_str) {
            Some(m) => format!("GitHub: {m}"),
            None => "GitHub's answer names no version".into(),
        });
    }
    let package = v.get("assets").and_then(Value::as_array).and_then(|assets| {
        assets.iter().find_map(|a| {
            let name = a.get("name")?.as_str()?;
            let url = a.get("browser_download_url")?.as_str()?;
            name.ends_with(suffix).then(|| (url.to_string(), a.get("size").and_then(Value::as_u64).unwrap_or(0)))
        })
    });
    let page = text("html_url");
    Ok(Release {
        version,
        date: text("published_at").chars().take(10).collect(),
        notes: text("body").trim().replace("\r\n", "\n"),
        page: if page.is_empty() { RELEASES.into() } else { page },
        package,
    })
}

/// The folder Janus's installer put it in (its uninstaller is there), None for a build or a copy.
pub(crate) fn installed_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    (cfg!(windows) && dir.join("unins000.exe").is_file()).then_some(dir)
}

/// Where a check or a download is.
#[derive(Clone, Default)]
enum Status {
    #[default]
    Unchecked,
    Checking,
    Failed(String),
    Known(Release),
    Downloading(Release),
    Downloaded(Release, PathBuf),
    DownloadFailed(Release, String),
}

/// The updates' state: the dialog, the check or download running in the background.
#[derive(Default)]
pub struct UpdateUi {
    pub open: bool,
    status: Arc<Mutex<Status>>,
    got: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    /// the check at the start was made
    started: bool,
    /// the newer version found was announced (a notice)
    pub announced: bool,
}

impl UpdateUi {
    fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set(&self, s: Status) {
        if let Ok(mut st) = self.status.lock() {
            *st = s;
        }
    }

    /// The latest release asked of GitHub, in the background.
    pub fn check(&mut self, ctx: &egui::Context) {
        if matches!(self.status(), Status::Checking | Status::Downloading(..)) {
            return;
        }
        self.set(Status::Checking);
        let (status, ctx) = (self.status.clone(), ctx.clone());
        std::thread::spawn(move || {
            let agent = format!("USERAGENT=Janus/{VERSION} (https://github.com/sacridini/janus)");
            let options = [agent.as_str(), "TIMEOUT=20", "HEADERS=Accept: application/vnd.github+json"];
            let s = match janus_gdal::http_fetch(LATEST, None, &options)
                .and_then(|body| parse_release(&body, package_suffix()))
            {
                Ok(r) => Status::Known(r),
                Err(e) => Status::Failed(e),
            };
            if let Ok(mut st) = status.lock() {
                *st = s;
            }
            ctx.request_repaint();
        });
    }

    /// Once, at the start of an interactive run.
    pub fn check_at_start(&mut self, ctx: &egui::Context) {
        if !std::mem::replace(&mut self.started, true) {
            self.check(ctx);
        }
    }

    /// A version newer than this one, once known.
    pub fn available(&self) -> Option<Release> {
        match self.status() {
            Status::Known(r) | Status::Downloading(r) | Status::Downloaded(r, _) | Status::DownloadFailed(r, _)
                if newer(&r.version, VERSION) =>
            {
                Some(r)
            }
            _ => None,
        }
    }

    /// The menu bar's Update: the dialog opened and, for the installed program, the new installer downloaded at once
    /// (the dialog then installs it); else the dialog's Download (a build or a copy, another platform).
    pub(crate) fn start_update(&mut self, ctx: &egui::Context) {
        self.open = true;
        if let Status::Known(r) = self.status()
            && newer(&r.version, VERSION)
            && r.package.is_some()
            && installed_dir().is_some()
        {
            self.download(r, ctx);
        }
    }

    /// A release as if GitHub had named it (the UI self-test).
    pub(crate) fn pretend(&mut self, r: Release) {
        self.started = true;
        self.set(Status::Known(r));
    }

    /// The release's installer downloaded to the temporary folder, in the background.
    fn download(&mut self, r: Release, ctx: &egui::Context) {
        let Some((url, _)) = r.package.clone() else { return };
        let name = url.rsplit('/').next().unwrap_or("janus-setup.exe").to_string();
        let path = std::env::temp_dir().join("janus-update").join(name);
        self.got.store(0, Ordering::Relaxed);
        self.cancel.store(false, Ordering::Relaxed);
        self.set(Status::Downloading(r.clone()));
        let (status, got, cancel, ctx) = (self.status.clone(), self.got.clone(), self.cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            let progress = |n: f64| {
                got.store(n as u64, Ordering::Relaxed);
                ctx.request_repaint();
            };
            let s = match janus_ml::fetch::fetch(&url, &path, &progress, &cancel) {
                Ok(()) => Status::Downloaded(r, path),
                Err(e) if e == "cancelled" => Status::Known(r),
                // the weights' advice (JANUS_WEIGHTS) left out
                Err(e) => Status::DownloadFailed(r, e.split("; without network").next().unwrap_or(&e).to_string()),
            };
            if let Ok(mut st) = status.lock() {
                *st = s;
            }
            ctx.request_repaint();
        });
    }
}

/// The installer run over the installed Janus: silently, into its folder, for this user or all of them as it was
/// installed, Janus closed by it if still open and opened again after (/RELAUNCH).
fn run_installer(setup: &Path, dir: &Path) -> Result<(), String> {
    let all_users =
        ["ProgramFiles", "ProgramW6432"].iter().filter_map(std::env::var_os).any(|p| dir.starts_with(PathBuf::from(p)));
    std::process::Command::new(setup)
        .args(["/SILENT", "/SP-", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/RELAUNCH"])
        .arg(if all_users { "/ALLUSERS" } else { "/CURRENTUSER" })
        .arg(format!("/DIR={}", dir.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", setup.display()))
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

impl State {
    /// Help > Check for updates: the version running, the latest one, its notes, its download and installation.
    pub(crate) fn update_window(&mut self, ctx: &egui::Context) {
        if !self.update.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Janus updates")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| self.update_ui(ui));
        self.update.open &= open;
    }

    fn update_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.set_width(420.0);
        ui.horizontal(|ui| {
            ui.label("You are using");
            ui.label(RichText::new(format!("Janus {VERSION}")).strong());
            ui.weak(format!("(GDAL {})", janus_cube::gdal_version()));
        });
        ui.add_space(6.0);
        let installed = installed_dir();
        match self.update.status() {
            Status::Unchecked => {
                self.update.check(&ctx);
            }
            Status::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Checking for a newer version...");
                });
            }
            Status::Failed(e) => {
                ui.colored_label(theme::warning(), "Could not check for updates.");
                ui.add(egui::Label::new(RichText::new(e).weak().small()).wrap());
                if ui.button("Try again").clicked() {
                    self.update.check(&ctx);
                }
            }
            Status::Known(r) if !newer(&r.version, VERSION) => {
                ui.label(format!("Janus is up to date: {} is the latest version.", r.version));
            }
            Status::Known(r) => {
                self.newer_release(ui, &r);
                ui.horizontal(|ui| {
                    let Some((url, size)) = &r.package else { return };
                    if installed.is_some() {
                        let label = format!("Download and install ({})", mb(*size));
                        if ui.add(egui::Button::new(RichText::new(label).strong())).clicked() {
                            self.update.download(r.clone(), &ctx);
                        }
                    } else {
                        let tip = if cfg!(windows) {
                            "Not installed by its installer (a build or a copy): the new installer is downloaded by \
                             the browser"
                        } else {
                            "The new package, downloaded by the browser"
                        };
                        if ui.button(format!("Download ({})", mb(*size))).on_hover_text(tip).clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(url));
                        }
                    }
                });
            }
            Status::Downloading(r) => {
                self.newer_release(ui, &r);
                let total = r.package.as_ref().map_or(0, |p| p.1);
                let got = self.update.got.load(Ordering::Relaxed);
                ui.horizontal(|ui| {
                    let f = if total > 0 { (got as f32 / total as f32).min(1.0) } else { 0.0 };
                    let text = if total > 0 { format!("{} of {}", mb(got), mb(total)) } else { mb(got) };
                    ui.add(egui::ProgressBar::new(f).text(text).desired_width(300.0));
                    if ui.button("Cancel").clicked() {
                        self.update.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
            Status::Downloaded(r, path) => {
                self.newer_release(ui, &r);
                ui.label("Downloaded. Janus closes, the new version is installed and Janus opens again.");
                if ui.add(egui::Button::new(RichText::new(format!("Install {} now", r.version)).strong())).clicked()
                    && let Some(dir) = &installed
                {
                    match run_installer(&path, dir) {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.update.set(Status::DownloadFailed(r.clone(), e)),
                    }
                }
            }
            Status::DownloadFailed(r, e) => {
                self.newer_release(ui, &r);
                ui.colored_label(theme::warning(), "The download failed.");
                ui.add(egui::Label::new(RichText::new(e).weak().small()).wrap());
                if ui.button("Try again").clicked() {
                    self.update.download(r, &ctx);
                }
            }
        }
        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            let busy = matches!(self.update.status(), Status::Checking | Status::Downloading(..));
            if ui.add_enabled(!busy, egui::Button::new("Check again")).clicked() {
                self.update.check(&ctx);
            }
            if ui.button("Releases page").clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(RELEASES));
            }
        });
        if ui
            .checkbox(&mut self.settings.s.check_updates, "Check for updates when Janus starts")
            .on_hover_text("One request to GitHub at the start; a button on the menu bar when there is a newer version")
            .changed()
        {
            self.settings.changed();
        }
    }

    /// A newer release: its version and date, its notes folded.
    fn newer_release(&self, ui: &mut Ui, r: &Release) {
        ui.label(RichText::new(format!("Janus {} is out ({}).", r.version, r.date)).strong().color(theme::accent()));
        if !r.notes.is_empty() {
            egui::CollapsingHeader::new("What's new").id_salt("update notes").show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    ui.add(egui::Label::new(&r.notes).wrap());
                });
            });
        }
        if ui.link("Its page on GitHub").clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&r.page));
        }
        ui.add_space(4.0);
    }

    /// Help > About Janus: its version, GDAL's, its license and its page.
    pub(crate) fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let m = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(format!("Janus {VERSION}"));
            ui.label("Raster time series analysis: change detection and validation");
            ui.add_space(6.0);
            ui.weak(format!("GDAL {}", janus_cube::gdal_version()));
            ui.weak("GNU General Public License, version 2");
            ui.hyperlink_to("github.com/sacridini/janus", "https://github.com/sacridini/janus");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let check = ui.button("Check for updates...").clicked();
                if check {
                    self.update.open = true;
                    self.update.check(ui.ctx());
                }
                check || ui.button("Close").clicked()
            })
            .inner
        });
        if m.inner || m.should_close() {
            self.about_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.4.1"), Some((1, 4, 1)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("2.0.0-rc1"), Some((2, 0, 0)));
        assert_eq!(parse_version("latest"), None);
        assert!(newer("1.10.0", "1.9.3"));
        assert!(newer("v2.0.0", "1.99.99"));
        assert!(!newer("1.4.1", "1.4.1"));
        assert!(!newer("1.4.0", "1.4.1"));
        assert!(!newer("nightly", "1.4.1"));
    }

    #[test]
    fn release_from_github() {
        let json = br#"{
            "tag_name": "v1.5.0", "html_url": "https://github.com/sacridini/janus/releases/tag/v1.5.0",
            "published_at": "2026-11-02T10:00:00Z", "body": "Layers: Delete closes the selected.\r\n",
            "assets": [
                {"name": "janus-1.5.0-linux-x86_64.tar.xz", "size": 10, "browser_download_url": "https://x/l"},
                {"name": "janus-1.5.0-setup.exe", "size": 123456789, "browser_download_url": "https://x/w"}
            ]
        }"#;
        let r = parse_release(json, "-setup.exe").unwrap();
        assert_eq!(r.version, "1.5.0");
        assert_eq!(r.date, "2026-11-02");
        assert_eq!(r.notes, "Layers: Delete closes the selected.");
        assert_eq!(r.package, Some(("https://x/w".into(), 123456789)));
        assert_eq!(parse_release(json, ".dmg").unwrap().package, None);
        let limited = br#"{"message": "API rate limit exceeded"}"#;
        assert_eq!(parse_release(limited, ".dmg").unwrap_err(), "GitHub: API rate limit exceeded");
    }

    /// GitHub asked as Help > Check for updates asks it (the network: cargo test -- --ignored).
    #[test]
    #[ignore]
    fn latest_on_github() {
        let agent = format!("USERAGENT=Janus/{VERSION} (https://github.com/sacridini/janus)");
        let body = janus_gdal::http_fetch(LATEST, None, &[agent.as_str(), "TIMEOUT=20"]).unwrap();
        let r = parse_release(&body, "-setup.exe").unwrap();
        assert!(!newer(VERSION, &r.version) || VERSION.ends_with("-dev"), "{} after the latest {}", VERSION, r.version);
        assert!(r.package.is_some_and(|(url, size)| url.ends_with("-setup.exe") && size > 1_000_000));
    }
}
