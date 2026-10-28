//! The status bar at the foot of the window and the notices of tasks that end. The bar: what the map has under the
//! cursor (its pixel, its coordinates in the layer's CRS and in longitude and latitude, the value), the map's zoom,
//! CRS and scale, and the tasks under way (a click opens them). A task that ends (a Zeit or deep-learning run, a
//! download, an export) leaves a notice in the corner for a while; with the window in the background, the system is
//! asked for the user's attention (the taskbar's icon flashes, the Dock's bounces).

use std::collections::HashSet;
use std::time::Instant;

use egui::{Align, Align2, Layout, RichText, Ui, vec2};
use janus_cube::multidim::format_g;

use crate::app::State;
use crate::exports::ExportState;
use crate::theme;
use crate::zeit::JobState;

/// A task shorter than this ends without a notice (an export of a PNG), unless it failed.
const NOTICE_MIN_SECONDS: f64 = 3.0;
/// How long a notice stays: a task done, one that failed.
const NOTICE_SECONDS: f64 = 8.0;
const NOTICE_FAILED_SECONDS: f64 = 30.0;

/// What the map shows under the cursor, set by the map panel each frame it is drawn.
#[derive(Default, Clone)]
pub struct MapStatus {
    /// "zoom 100.0% (overview 1:2.0)"
    pub zoom: String,
    /// the active layer's source pixel under the cursor, and its coordinates in the layer's CRS
    pub pixel: Option<(usize, usize)>,
    pub geo: Option<(f64, f64)>,
    /// what the map shows there ("value 0.52", "class Forest"...), the embeddings' and vector layers' notes
    pub value: String,
    /// the frame it was set on (an older one: the map was not drawn, nothing under the cursor)
    pub frame: u64,
}

/// What a notice offers.
#[derive(Clone)]
enum NoticeAction {
    Tasks,
    Log(String),
    Folder(String),
    Exports,
}

struct Notice {
    title: String,
    text: String,
    failed: bool,
    actions: Vec<NoticeAction>,
    at: Instant,
}

/// A CRS's linear units and its transformation to longitudes and latitudes (made once per layer).
struct CrsInfo {
    cube: u64,
    geographic: bool,
    /// metres per unit of the CRS (projected)
    metres: f64,
    to_lon_lat: Option<gdal::spatial_ref::CoordTransform>,
}

#[derive(Default)]
pub struct StatusUi {
    pub map: MapStatus,
    notices: Vec<Notice>,
    /// tasks seen running (by their status' address): a notice when they end
    running: HashSet<usize>,
    crs: Option<CrsInfo>,
}

fn crs_info(info: &janus_cube::CubeInfo) -> CrsInfo {
    use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
    let mut c = CrsInfo { cube: info.id, geographic: false, metres: 1.0, to_lon_lat: None };
    let Ok(mut srs) = SpatialRef::from_wkt(&info.crs_wkt) else { return c };
    c.geographic = srs.is_geographic();
    c.metres = srs.linear_units();
    if !c.geographic
        && let Ok(mut wgs) = SpatialRef::from_epsg(4326)
    {
        srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        wgs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        c.to_lon_lat = CoordTransform::new(&srs, &wgs).ok();
    }
    c
}

/// A map scale's denominator rounded to two significant figures, its thousands apart: 1:12 000.
pub(crate) fn scale_text(denominator: f64) -> String {
    if !denominator.is_finite() || denominator <= 0.0 {
        return String::new();
    }
    let p = 10f64.powi((denominator.log10().floor() as i32 - 1).max(0));
    let n = ((denominator / p).round() * p) as u64;
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    format!("1:{out}")
}

/// A length on the ground: metres below a kilometre, else km.
fn length_text(m: f64) -> String {
    if m >= 1000.0 { format!("{} km", format_g(m / 1000.0, 3)) } else { format!("{} m", format_g(m, 3)) }
}

/// A duration as the Tasks window gives it: seconds, then minutes and seconds.
fn duration_text(s: f64) -> String {
    if s < 60.0 { format!("{s:.0} s") } else { format!("{} min {:02.0} s", (s / 60.0).floor(), s % 60.0) }
}

impl State {
    /// Tasks that ended since the last frame: a notice each (those of a few seconds left out, unless they failed),
    /// and the system's attention asked for when the window is in the background.
    pub(crate) fn pump_notices(&mut self, ctx: &egui::Context) {
        let mut ended = Vec::new();
        for j in &self.zeit.jobs {
            let key = std::sync::Arc::as_ptr(&j.status) as usize;
            let s = j.status.lock().unwrap();
            if s.state == JobState::Running {
                self.status.running.insert(key);
            } else if self.status.running.remove(&key) {
                let failed = s.state == JobState::Failed;
                if failed || s.seconds >= NOTICE_MIN_SECONDS {
                    let (text, mut actions) = match s.state {
                        JobState::Failed => (s.error.clone(), vec![NoticeAction::Log(j.log_path.clone())]),
                        JobState::Cancelled => (format!("Cancelled after {}", duration_text(s.seconds)), vec![]),
                        _ => (format!("Done in {}", duration_text(s.seconds)), vec![NoticeAction::Tasks]),
                    };
                    if !failed && !j.output_dir.is_empty() {
                        actions.push(NoticeAction::Folder(j.output_dir.clone()));
                    }
                    ended.push(Notice { title: j.title.clone(), text, failed, actions, at: Instant::now() });
                }
            }
        }
        for e in &self.exports {
            let key = std::sync::Arc::as_ptr(e) as usize;
            let state = e.state();
            if state == ExportState::Running {
                self.status.running.insert(key);
            } else if self.status.running.remove(&key) {
                let failed = state == ExportState::Failed;
                if failed || e.seconds() >= NOTICE_MIN_SECONDS {
                    let folder =
                        std::path::Path::new(&e.path).parent().map_or(String::new(), |p| p.to_string_lossy().into());
                    let (text, actions) = match state {
                        ExportState::Failed => (e.error(), vec![NoticeAction::Exports]),
                        ExportState::Cancelled => ("Cancelled".to_string(), vec![]),
                        _ => (
                            format!("Written in {}", duration_text(e.seconds())),
                            vec![NoticeAction::Folder(folder), NoticeAction::Exports],
                        ),
                    };
                    ended.push(Notice { title: e.title.clone(), text, failed, actions, at: Instant::now() });
                }
            }
        }
        if ended.is_empty() {
            return;
        }
        if !self.opts.scripted() && ctx.input(|i| i.viewport().focused) == Some(false) {
            let kind = if ended.iter().any(|n| n.failed) {
                egui::UserAttentionType::Critical
            } else {
                egui::UserAttentionType::Informational
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(kind));
        }
        self.status.notices.extend(ended);
        // a few at a time: the newest
        let n = self.status.notices.len();
        if n > 4 {
            self.status.notices.drain(..n - 4);
        }
    }

    /// The notices in the bottom right corner, above the status bar, newest at the bottom; they go after a while
    /// (a failure stays longer), or with their ×.
    pub(crate) fn notices_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.status.notices.retain(|n| {
            now.duration_since(n.at).as_secs_f64() < if n.failed { NOTICE_FAILED_SECONDS } else { NOTICE_SECONDS }
        });
        if self.status.notices.is_empty() {
            return;
        }
        // the next one to go
        if let Some(left) = self
            .status
            .notices
            .iter()
            .map(|n| {
                (if n.failed { NOTICE_FAILED_SECONDS } else { NOTICE_SECONDS }) - now.duration_since(n.at).as_secs_f64()
            })
            .reduce(f64::min)
        {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(left.max(0.05)));
        }
        let mut close = None;
        let mut act = None;
        egui::Area::new(egui::Id::new("notices"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-12.0, -40.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                for (k, n) in self.status.notices.iter().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_width(360.0);
                        ui.horizontal(|ui| {
                            let (mark, c) =
                                if n.failed { ("Failed", theme::error()) } else { ("Done", theme::accent()) };
                            ui.label(RichText::new(mark).strong().color(c));
                            ui.add(egui::Label::new(RichText::new(&n.title).strong()).truncate());
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                                    close = Some(k);
                                }
                            });
                        });
                        if !n.text.is_empty() {
                            ui.add(egui::Label::new(RichText::new(&n.text).weak()).wrap());
                        }
                        if !n.actions.is_empty() {
                            ui.horizontal(|ui| {
                                for a in &n.actions {
                                    let label = match a {
                                        NoticeAction::Tasks => "Show in Tasks",
                                        NoticeAction::Log(_) => "Log",
                                        NoticeAction::Folder(_) => "Open folder",
                                        NoticeAction::Exports => "Exports",
                                    };
                                    if ui.small_button(label).clicked() {
                                        act = Some((k, a.clone()));
                                    }
                                }
                            });
                        }
                    });
                    ui.add_space(4.0);
                }
            });
        if let Some((k, a)) = act {
            match a {
                NoticeAction::Tasks => self.zeit.show_tasks = true,
                NoticeAction::Log(path) => (self.zeit.log.path, self.zeit.log.show) = (path, true),
                NoticeAction::Folder(dir) => crate::platform::open_in_explorer(&dir),
                NoticeAction::Exports => self.show_exports = true,
            }
            close = Some(k);
        }
        if let Some(k) = close {
            self.status.notices.remove(k);
        }
    }

    /// The status bar: the cursor's pixel, coordinates and value at the left; the tasks, the scale and the CRS at the
    /// right.
    pub(crate) fn status_bar_ui(&mut self, ui: &mut Ui) {
        self.status_crs();
        let fresh = self.status.map.frame + 1 >= self.frame;
        let lon_lat = self.status.map.geo.and_then(|g| self.status_lon_lat(g));
        let sep = |ui: &mut Ui| {
            ui.label(RichText::new("|").color(ui.visuals().weak_text_color().gamma_multiply(0.6)));
        };
        ui.spacing_mut().item_spacing.x = 6.0;
        // from the right: the tasks, the CRS, the scale, the zoom; then, in what is left, what is under the cursor
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            self.tasks_status(ui);
            if let Some(s) = &self.session
                && !self.vectors_only()
            {
                let info = &s.info;
                let crs = if info.crs_authority.is_empty() { &info.crs_name } else { &info.crs_authority };
                if !crs.is_empty() {
                    ui.weak(crs).on_hover_text(format!("The active layer's CRS: {}", info.crs_name));
                    sep(ui);
                }
            }
            if let Some((scale, ground, tip)) = self.scale_status() {
                ui.weak(ground).on_hover_text(&tip);
                sep(ui);
                ui.weak(scale).on_hover_text(&tip);
                sep(ui);
            }
            if fresh && !self.status.map.zoom.is_empty() {
                ui.weak(&self.status.map.zoom);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                let m = &self.status.map;
                if !fresh || (m.pixel.is_none() && m.geo.is_none()) {
                    if self.session.is_some() {
                        ui.weak("Point at the map: its pixel, coordinates and value here");
                    }
                    return;
                }
                if let Some((x, y)) = m.pixel {
                    ui.weak(format!("col {x}  row {y}"));
                    sep(ui);
                }
                if let Some((gx, gy)) = m.geo {
                    let geographic = self.status.crs.as_ref().is_some_and(|c| c.geographic);
                    if geographic {
                        ui.weak(format!("lon {gx:.6}  lat {gy:.6}"));
                    } else {
                        ui.weak(format!("x {gx:.2}  y {gy:.2}"));
                        if let Some((lon, lat)) = lon_lat {
                            sep(ui);
                            ui.weak(format!("lon {lon:.6}  lat {lat:.6}"))
                                .on_hover_text("Longitude and latitude (WGS 84)");
                        }
                    }
                }
                let value = m.value.trim_start_matches([' ', '|']).trim();
                if !value.is_empty() {
                    sep(ui);
                    ui.label(value);
                }
            });
        });
    }

    /// The active layer's CRS's units and transformation to longitudes and latitudes, made when it changes.
    fn status_crs(&mut self) {
        let id = self.session.as_ref().map(|s| s.info.id);
        if let Some(s) = &self.session
            && self.status.crs.as_ref().is_none_or(|c| Some(c.cube) != id)
        {
            self.status.crs = Some(crs_info(&s.info));
        }
    }

    /// Coordinates of the active layer's CRS in longitude and latitude, as the status bar gives them.
    pub(crate) fn status_lon_lat(&mut self, (x, y): (f64, f64)) -> Option<(f64, f64)> {
        self.status_crs();
        let c = self.status.crs.as_ref()?;
        if c.geographic {
            return Some((x, y));
        }
        let (mut xs, mut ys, mut zs) = ([x], [y], [0.0]);
        c.to_lon_lat.as_ref()?.transform_coords(&mut xs, &mut ys, &mut zs).ok()?;
        xs[0].is_finite().then_some((xs[0], ys[0]))
    }

    /// The status bar's scale and the ground a screen pixel covers.
    pub(crate) fn status_scale(&mut self) -> Option<(String, String)> {
        self.status_crs();
        self.scale_status().map(|(a, b, _)| (a, b))
    }

    /// The notices shown: their titles, and whether each is of a failure.
    pub(crate) fn notice_titles(&self) -> Vec<(String, bool)> {
        self.status.notices.iter().map(|n| (n.title.clone(), n.failed)).collect()
    }

    pub(crate) fn clear_notices(&mut self) {
        self.status.notices.clear();
    }

    /// The map's scale on the screen (1:N at 96 points per inch) and the ground a screen pixel covers.
    fn scale_status(&self) -> Option<(String, String, String)> {
        let s = self.session.as_ref()?;
        let g = s.info.geo_transform?;
        let c = self.status.crs.as_ref().filter(|c| c.cube == s.info.id)?;
        if self.map.scale <= 0.0 || self.vectors_only() && c.geographic && s.info.width < 4 {
            return None;
        }
        let grid = self.grid()?;
        // CRS units per source pixel, source pixels per screen point
        let unit = g[1].hypot(g[4]);
        let mut metres = unit * c.metres;
        if c.geographic {
            // a degree of longitude where the map is centred
            let lat = g[3] + g[5] * self.map.center[1] * grid.fy;
            metres = unit * 111_320.0 * lat.to_radians().cos().abs().max(1e-6);
        }
        let per_point = metres * grid.fx / self.map.scale;
        let point = 0.0254 / 96.0;
        Some((
            scale_text(per_point / point),
            format!("{}/px", length_text(per_point)),
            "The map's scale on the screen (96 points per inch), and the ground a screen point covers".into(),
        ))
    }

    /// The tasks under way: how many, the oldest one's progress; a click opens Tasks (or Exports).
    fn tasks_status(&mut self, ui: &mut Ui) {
        let jobs: Vec<(String, f64)> = self
            .zeit
            .jobs
            .iter()
            .filter_map(|j| {
                let s = j.status.lock().unwrap();
                (s.state == JobState::Running).then(|| (j.title.clone(), s.progress))
            })
            .collect();
        let exports: Vec<(String, f64)> = self
            .exports
            .iter()
            .filter(|e| e.state() == ExportState::Running)
            .map(|e| (e.title.clone(), e.progress()))
            .collect();
        let n = jobs.len() + exports.len();
        if n == 0 {
            return;
        }
        let Some((title, p)) = jobs.first().or(exports.first()).cloned() else { return };
        let text = if n == 1 { title.clone() } else { format!("{n} tasks: {title}") };
        let bar = egui::ProgressBar::new(p as f32).desired_width(110.0).text(format!("{:.0}%", p * 100.0));
        let r = ui.add(bar);
        let label =
            ui.add(egui::Label::new(RichText::new(text).color(theme::accent())).truncate().sense(egui::Sense::click()));
        let all: Vec<String> = jobs.iter().chain(&exports).map(|(t, p)| format!("{:3.0}%  {t}", p * 100.0)).collect();
        let tip = format!("{}\n\nClick: the Tasks window", all.join("\n"));
        if label.on_hover_text(&tip).clicked() || r.on_hover_text(&tip).clicked() {
            if jobs.is_empty() {
                self.show_exports = true;
            } else {
                self.zeit.show_tasks = true;
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_rounded() {
        assert_eq!(scale_text(12_345.0), "1:12 000");
        assert_eq!(scale_text(987.0), "1:990");
        assert_eq!(scale_text(1_250_000.0), "1:1 300 000");
        assert_eq!(scale_text(42.0), "1:42");
        assert_eq!(scale_text(0.0), "");
    }
}
