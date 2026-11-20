//! The co-registration's window (Tools > Data preparation > Co-register dates) beyond the generic form: the parameters
//! of the other shift and reference left out of the form, and the shifts of the last run as a table (a row a date: how
//! far it was moved, how reliable the shift is, the similarity to the reference before and after, or why it was left as
//! it is), a click on a row showing that date on the map.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use eframe::egui::{self, Ui};
use serde_json::Value;

use crate::app::State;
use crate::theme;

/// Whether the tool's parameter has no use with the others' values (its form leaves it out).
pub(crate) fn hidden(tool: &str, param: &str, values: &BTreeMap<String, Value>) -> bool {
    if tool != "coregister" {
        return false;
    }
    let text = |k: &str| values.get(k).and_then(Value::as_str).unwrap_or("");
    let local = text("mode") == "local";
    match param {
        "ref_date" => text("reference") != "date",
        "ref_dates" => text("reference") != "composite",
        "ref_layer" | "ref_band" | "ref_layer_date" => text("reference") != "layer",
        "grid_res" | "filter_level" | "min_points" | "arosics_compat" | "warp" => !local,
        "rs_max_outlier" | "rs_tolerance" => !local || text("filter_level") != "3",
        _ => false,
    }
}

/// Whether a run needs another open series besides the active one (a driver, a reference layer).
pub(crate) fn needs_other_series(tool: &str, params: &BTreeMap<String, Value>) -> bool {
    let text = |k: &str| params.get(k).and_then(Value::as_str);
    match tool {
        "lagged_correlation" => text("y_mode") != Some("file"),
        "coregister" => text("reference") == Some("layer"),
        _ => true,
    }
}

/// A date's row of a run's shifts.csv.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ShiftRow {
    pub date: String,
    pub dx: f64,
    pub dy: f64,
    pub reliability: f64,
    pub ssim_before: f64,
    pub ssim_after: f64,
    pub applied: bool,
    pub reason: String,
    pub reference: bool,
    /// the tie points: placed, matched, used (local mode)
    pub points: Option<[usize; 3]>,
}

/// The fields of a CSV line, the quoted ones read.
fn fields(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (vec![], String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// The rows of a shifts.csv (a row a date), as the run wrote it.
pub(crate) fn parse(text: &str) -> Vec<ShiftRow> {
    let mut lines = text.lines();
    let Some(header) = lines.next().map(fields) else { return vec![] };
    let at = |name: &str| header.iter().position(|h| h == name);
    let (date, dx, dy) = (at("date"), at("x_shift_px"), at("y_shift_px"));
    let (rel, sb, sa) = (at("reliability"), at("ssim_before"), at("ssim_after"));
    let (applied, reason, reference) = (at("applied"), at("reason"), at("reference"));
    let (points, matched, used) = (at("points"), at("matched"), at("used"));
    lines
        .map(fields)
        .map(|row| {
            let get = |i: Option<usize>| i.and_then(|i| row.get(i)).map_or("", String::as_str);
            let num = |i: Option<usize>| get(i).parse::<f64>().unwrap_or(f64::NAN);
            let count = |i: Option<usize>| get(i).parse::<usize>().ok();
            ShiftRow {
                date: get(date).to_string(),
                dx: num(dx),
                dy: num(dy),
                reliability: num(rel),
                ssim_before: num(sb),
                ssim_after: num(sa),
                applied: get(applied) == "true",
                reason: get(reason).to_string(),
                reference: get(reference) == "true",
                points: count(points).zip(count(matched)).zip(count(used)).map(|((p, m), u)| [p, m, u]),
            }
        })
        .collect()
}

/// The rows of the shifts.csv of a run folder, read again when the file changes.
pub(crate) fn rows_of(dir: &str) -> Arc<Vec<ShiftRow>> {
    static CACHE: LazyLock<Mutex<HashMap<String, (SystemTime, Arc<Vec<ShiftRow>>)>>> = LazyLock::new(Default::default);
    let path = std::path::Path::new(dir).join("shifts.csv");
    let Some(modified) = std::fs::metadata(&path).and_then(|m| m.modified()).ok() else { return Arc::default() };
    if let Some((t, rows)) = CACHE.lock().ok().and_then(|c| c.get(dir).cloned())
        && t == modified
    {
        return rows;
    }
    let rows = Arc::new(std::fs::read_to_string(&path).map(|t| parse(&t)).unwrap_or_default());
    if let Ok(mut c) = CACHE.lock() {
        c.insert(dir.to_string(), (modified, rows.clone()));
    }
    rows
}

/// What a shift's row says needs a look: a shift near the largest allowed or a low reliability.
pub(crate) fn warnings(rows: &[ShiftRow], max_shift: f64) -> Vec<String> {
    let mut out = vec![];
    for r in rows.iter().filter(|r| r.applied) {
        if r.dx.hypot(r.dy) > 0.9 * max_shift {
            out.push(format!(
                "{}: a shift of {:.2} pixels is near the largest allowed ({max_shift})",
                r.date,
                r.dx.hypot(r.dy)
            ));
        } else if r.reliability < 60.0 {
            out.push(format!("{}: reliability {:.0}, under 60", r.date, r.reliability));
        }
    }
    out
}

impl State {
    /// "Shifts of the last run": the table of the last co-registration of the active series.
    pub(crate) fn coregister_table_ui(&mut self, ui: &mut Ui) {
        let Some(id) = self.session.as_ref().map(|s| s.info.id) else { return };
        let done = |j: &&crate::zeit::Job| {
            j.tool == "coregister"
                && j.cube == id
                && j.status.lock().is_ok_and(|s| s.state == crate::zeit::JobState::Done)
        };
        let Some(dir) = self.zeit.jobs.iter().rev().find(done).map(|j| j.output_dir.clone()) else { return };
        let rows = rows_of(&dir);
        if rows.is_empty() {
            return;
        }
        let max_shift = self
            .zeit
            .params(janus_zeit::manifest().tool("coregister").expect("the co-registration is a tool"))
            .get("max_shift")
            .and_then(Value::as_f64)
            .unwrap_or(5.0);
        let moved = rows.iter().filter(|r| r.applied).count();
        let title = format!("Shifts of the last run ({moved} of {} dates moved)", rows.len());
        let mut show = None;
        egui::CollapsingHeader::new(title).id_salt("coregister_shifts").default_open(true).show(ui, |ui| {
            let local = rows.iter().any(|r| r.points.is_some());
            egui::Grid::new("coregister_shifts_grid").num_columns(6).spacing([10.0, 2.0]).striped(true).show(
                ui,
                |ui| {
                    let shift = if local { "Mean dx, dy (px)" } else { "dx, dy (px)" };
                    for h in [
                        "Date",
                        shift,
                        "Reliability",
                        "SSIM before > after",
                        if local { "Tie points" } else { "" },
                        "Moved",
                    ] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for (i, r) in rows.iter().enumerate().take(80) {
                        let text = |s: String| {
                            if r.applied || r.reference {
                                egui::RichText::new(s)
                            } else {
                                egui::RichText::new(s).color(theme::error())
                            }
                        };
                        if ui.link(&r.date).on_hover_text("Show this date on the map").clicked() {
                            show = Some(i);
                        }
                        ui.label(if r.dx.is_finite() { format!("{:+.3}, {:+.3}", r.dx, r.dy) } else { String::new() });
                        ui.label(if r.reliability.is_finite() {
                            format!("{:.0}", r.reliability)
                        } else {
                            String::new()
                        });
                        ui.label(if r.ssim_before.is_finite() {
                            format!("{:.3} > {:.3}", r.ssim_before, r.ssim_after)
                        } else {
                            String::new()
                        });
                        ui.label(
                            r.points.map_or(String::new(), |[p, m, u]| format!("{u} used of {m} matched, {p} placed")),
                        );
                        ui.label(text(if r.reference {
                            "the reference".into()
                        } else if r.applied {
                            "yes".into()
                        } else {
                            format!("no: {}", r.reason)
                        }));
                        ui.end_row();
                    }
                },
            );
            if rows.len() > 80 {
                ui.weak(format!("... and {} more dates (shifts.csv)", rows.len() - 80));
            }
            for w in warnings(&rows, max_shift).iter().take(6) {
                ui.colored_label(theme::error(), w);
            }
        });
        if let Some(i) = show {
            self.set_date(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_leaves_out_what_the_shift_and_reference_do_not_use() {
        let mut v: BTreeMap<String, Value> = BTreeMap::new();
        let set = |v: &mut BTreeMap<String, Value>, k: &str, x: &str| {
            v.insert(k.to_string(), Value::String(x.to_string()));
        };
        set(&mut v, "mode", "global");
        set(&mut v, "reference", "date");
        assert!(hidden("coregister", "grid_res", &v) && hidden("coregister", "warp", &v));
        assert!(hidden("coregister", "ref_layer", &v) && hidden("coregister", "ref_dates", &v));
        assert!(!hidden("coregister", "ref_date", &v) && !hidden("coregister", "window_size", &v));
        set(&mut v, "mode", "local");
        set(&mut v, "reference", "layer");
        set(&mut v, "filter_level", "2");
        assert!(!hidden("coregister", "grid_res", &v) && hidden("coregister", "rs_tolerance", &v));
        set(&mut v, "filter_level", "3");
        assert!(!hidden("coregister", "rs_tolerance", &v) && !hidden("coregister", "ref_layer", &v));
        assert!(hidden("coregister", "ref_date", &v));
        assert!(!hidden("topocorr", "grid_res", &v));
    }

    #[test]
    fn another_series_is_needed_by_a_layer_reference_only() {
        let given = |k: &str, x: &str| -> BTreeMap<String, Value> { [(k.to_string(), Value::String(x.into()))].into() };
        assert!(needs_other_series("coregister", &given("reference", "layer")));
        assert!(!needs_other_series("coregister", &given("reference", "date")));
        assert!(!needs_other_series("lagged_correlation", &given("y_mode", "file")));
        assert!(needs_other_series("lagged_correlation", &given("y_mode", "window_mean")));
        assert!(needs_other_series("improphe", &BTreeMap::new()));
    }

    #[test]
    fn a_shifts_file_is_read_by_its_columns() {
        let text = "date,x_shift_px,y_shift_px,x_shift_map,y_shift_map,reliability,ssim_before,ssim_after,success,reason,window_x0,window_y0,window_x1,window_y1,fft_size,applied,points,matched,used,valid_share,reference\n\
                    2022-01-08,0,0,,,,,,true,the reference,,,,,,false,,,,1,true\n\
                    2022-01-24,3.0,-1.5,90,45,88.5,0.4,0.9,true,,64,64,320,320,128,true,,,,0.98,false\n\
                    2022-02-09,,,,,,,,false,\"only 3 valid tie point(s), fewer than min_points=5\",,,,,,false,49,12,3,1,false\n";
        let rows = parse(text);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].reference && !rows[0].applied);
        assert_eq!((rows[1].dx, rows[1].dy, rows[1].reliability), (3.0, -1.5, 88.5));
        assert!(rows[1].applied && rows[1].points.is_none());
        assert_eq!(rows[2].reason, "only 3 valid tie point(s), fewer than min_points=5");
        assert_eq!(rows[2].points, Some([49, 12, 3]));
        assert!(rows[2].dx.is_nan());
        // a shift near the largest allowed, a low reliability
        let w = warnings(&rows, 3.2);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("2022-01-24: a shift of 3.35"), "{w:?}");
        assert!(warnings(&rows, 5.0).is_empty());
    }
}
