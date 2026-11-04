//! The official alerts' screenshots in the validation guide (`--shots alerts DATA OUT`; site/validation.html,
//! "Against official alerts"): the guide's place near Novo Progresso (tools/make_tutorial_data.py: the Sentinel-2
//! dry-season composites of 2017-2024), LandTrendr's year of detection on their NBR, compared with INPE's DETER
//! (TerraBrasilis' WFS) and with UMD's GLAD-S2 alerts of 2019-2023 (read from their public files). It needs the
//! network: the alerts are fetched as a user's would be.

use std::path::PathBuf;

use egui::Context;
use janus_cube::BandSelection;
use serde_json::json;

use crate::app::{State, Tab};
use crate::composite::BandComposite;
use crate::shots::Shots;

/// The window of the alerts compared, and the map's name in the list.
const WINDOW: &str = "from=2019-01-01,to=2023-12-31,map=Year of detection";

impl State {
    /// Only the comparison's agreement map drawn among the results (the vector layers stay).
    fn shots_agreement_only(&mut self) -> bool {
        let mut found = false;
        for r in &mut self.zeit.results {
            r.visible = r.name.starts_with("Agreement");
            found |= r.visible;
        }
        self.map.dirty = true;
        found
    }

    /// The comparison asked by `arg` (as --alerts) finished: Some(Ok) with its maps loaded, Some(Err) failed.
    fn shots_alerts_done(&self) -> Option<Result<(), String>> {
        let a = &self.alerts;
        if !a.error.is_empty() {
            return Some(Err(a.error.clone()));
        }
        let loaded = !a.busy() && a.done.is_some() && self.zeit.pending_loads == 0 && self.vector_loads.is_empty();
        (loaded && self.shots_quiet()).then_some(Ok(()))
    }

    pub(crate) fn shots_alerts(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL (alerts, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        if t > 600.0 {
            return fail(sh, "timeout");
        }
        let data = PathBuf::from(&sh.input);
        let panel = || Shots::fit(Tab::Validation);
        match sh.stage {
            0 => {
                // the composites, the NBR (NIR and SWIR2: bands 4 and 6)
                let sel = BandSelection { band: 4, nd_band: 6, chosen: true, ..Default::default() };
                self.shots_open(&data.join("s2").to_string_lossy(), sel, ctx);
                sh.next("the Sentinel-2 composites opening (NBR)");
            }
            1 => {
                // in SWIR1 / NIR / red, the last date; LandTrendr run on the whole image
                if self.opening.is_some() || self.session.is_none() {
                    return None;
                }
                let id = self.session.as_ref()?.info.id;
                self.composite = Some(BandComposite::of(id, [5, 4, 3]));
                let n = self.session.as_ref()?.info.t();
                self.set_date(n - 1);
                sh.layout = Some((vec![Tab::Validation], vec![Tab::Tasks]));
                if !self.shots_open_tool("landtrendr", json!({})) || !self.shots_run(sh, "landtrendr", 0, ctx) {
                    return fail(sh, "LandTrendr did not start");
                }
                sh.job_stage = Some(sh.stage);
                sh.next(&format!("{n} dates in SWIR1 / NIR / red; LandTrendr running on the whole image"));
            }
            2 => match self.shots_ran(sh) {
                -1 => return None,
                0 => sh.next("LandTrendr's maps loaded"),
                _ => return fail(sh, &format!("LandTrendr failed: {}", self.shots_job_error(sh))),
            },
            4 => {
                // the tab's end: DETER's classes
                if sh.frames == 1 {
                    sh.scroll = Some((Tab::Validation, f32::MAX));
                    return None;
                }
                if sh.frames < 10 {
                    return None;
                }
                sh.shoot("-", &[("17-alerts-deter-classes", panel())]);
                sh.next("DETER's classes, the tab scrolled to its end");
            }
            3 | 5 => {
                // DETER (fetched from TerraBrasilis), then GLAD-S2 (its files of the map's area)
                if sh.stage == 5 && sh.frames == 1 {
                    sh.scroll = Some((Tab::Validation, 0.0));
                }
                let (arg, name) = if sh.stage == 3 {
                    (format!("deter-amz,{WINDOW}"), "17-alerts-deter")
                } else {
                    (format!("glad-s2,download=1,{WINDOW}"), "18-alerts-glad")
                };
                if sh.frames == 1 {
                    if let Err(e) = self.alerts_option(&arg, ctx) {
                        return fail(sh, &e);
                    }
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(Tab::Validation);
                    self.map.fit();
                }
                match self.shots_alerts_done() {
                    None => return None,
                    Some(Err(e)) => return fail(sh, &e),
                    Some(Ok(())) => {}
                }
                if !self.shots_agreement_only() {
                    return fail(sh, "no agreement map");
                }
                self.clear_notices(); // the comparison's "Done" over the tab
                if t < 3.0 {
                    return None;
                }
                let d = self.alerts.done.clone()?;
                let c = &d.cmp;
                sh.shoot(name, &[("panel", panel())]);
                sh.next(&format!(
                    "{}: {} of {} alerts detected; both {}, only the map {}, only the alerts {} pixels",
                    d.alerts_name, c.alerts_detected, c.alerts, c.both, c.only_map, c.only_alerts
                ));
            }
            _ => {
                println!("OK");
                return Some(0);
            }
        }
        None
    }
}
