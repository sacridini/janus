//! The topographic correction's window (Tools > Data preparation > Topographic correction) beyond the generic form: the
//! sun of each date as the run will find it (its zenith, azimuth and where it comes from), computed from the same
//! lookups as the run (`janus_zeit::sun`), red where a date has none, so a date without a sun is seen before the run.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use eframe::egui::{self, Ui};
use janus_cube::CubeInfo;
use janus_zeit::Params;
use janus_zeit::manifest::Tool;
use janus_zeit::sun::{self, Known, Source, SunHint};

use crate::app::State;
use crate::theme;

/// A date's line of the preview.
pub(crate) struct SunRow {
    pub label: String,
    pub zenith: String,
    pub azimuth: String,
    pub from: String,
    pub missing: bool,
}

/// What each date's files say of its sun and the series' centre as latitude and longitude, read once per series.
type Facts = (Vec<SunHint>, Option<(f64, f64)>);

fn facts_of(info: &CubeInfo) -> Arc<Facts> {
    static CACHE: LazyLock<Mutex<HashMap<u64, Arc<Facts>>>> = LazyLock::new(Default::default);
    if let Some(f) = CACHE.lock().ok().and_then(|c| c.get(&info.id).filter(|f| f.0.len() == info.t()).cloned()) {
        return f;
    }
    let centre =
        info.geo_transform.and_then(|g| sun::centre_lat_lon(&info.crs_wkt, &g, [0, 0, info.width, info.height]));
    let found = Arc::new((sun::of_series(info), centre));
    if let Ok(mut c) = CACHE.lock() {
        c.insert(info.id, found.clone());
    }
    found
}

/// The lines of the preview for these parameters and roles.
pub(crate) fn rows(
    info: &CubeInfo,
    tool: &Tool,
    params: &Params,
    roles: &std::collections::BTreeMap<String, i32>,
) -> Vec<SunRow> {
    let facts = facts_of(info);
    let granules = if janus_zeit::granules::needs(&tool.id, params) { crate::zeit::granules_for(info) } else { vec![] };
    let known = Known {
        hints: &facts.0,
        bands: roles.contains_key("sza") && roles.contains_key("saa"),
        granules: &granules,
        centre: facts.1,
    };
    sun::plan(params, &known, info.t())
        .iter()
        .enumerate()
        .map(|(t, s)| {
            let label = info.layers.get(t).map_or(format!("date {}", t + 1), |l| l.label.clone());
            let (zenith, azimuth, from, missing) = match s {
                Source::Scene { zenith, azimuth, from } => {
                    (format!("{zenith:.2}"), format!("{azimuth:.2}"), from.to_string(), false)
                }
                Source::Bands | Source::Granules => {
                    ("per pixel".into(), "per pixel".into(), s.name().to_string(), false)
                }
                Source::Missing(why) => (String::new(), String::new(), why.clone(), true),
            };
            SunRow { label, zenith, azimuth, from, missing }
        })
        .collect()
}

impl State {
    /// The sun of each date for the tool's form now.
    pub(crate) fn sun_rows(&mut self, tool: &Tool) -> Vec<SunRow> {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return vec![] };
        let roles = self.tool_active_roles(tool);
        let given = self.zeit.params(tool).clone();
        let params = Params::with_defaults(&tool.id, &given).unwrap_or_default();
        rows(&info, tool, &params, &roles)
    }

    /// "Sun of each date" under the parameters: date, zenith, azimuth, source.
    pub(crate) fn sun_preview_ui(&mut self, ui: &mut Ui, tool: &Tool) {
        let rows = self.sun_rows(tool);
        if rows.is_empty() {
            return;
        }
        let lost = rows.iter().filter(|r| r.missing).count();
        let title =
            if lost > 0 { format!("Sun of each date ({lost} without)") } else { "Sun of each date".to_string() };
        egui::CollapsingHeader::new(title).id_salt("topocorr_sun").default_open(true).show(ui, |ui| {
            egui::Grid::new("topocorr_sun_grid").num_columns(4).spacing([10.0, 2.0]).striped(true).show(ui, |ui| {
                for h in ["Date", "Zenith", "Azimuth", "From"] {
                    ui.strong(h);
                }
                ui.end_row();
                for r in rows.iter().take(60) {
                    let text = |s: &str| {
                        if r.missing { egui::RichText::new(s).color(theme::error()) } else { egui::RichText::new(s) }
                    };
                    ui.label(text(&r.label));
                    ui.label(text(&r.zenith));
                    ui.label(text(&r.azimuth));
                    ui.label(text(&r.from));
                    ui.end_row();
                }
            });
            if rows.len() > 60 {
                ui.weak(format!("... and {} more dates", rows.len() - 60));
            }
        });
    }
}
