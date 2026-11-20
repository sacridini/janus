//! The MCP server's `tune_tool`: Tune on samples (the window's search, IDEIAS phase 62) without the window. A
//! change tool's parameters are searched on reference points (a validation project's calibration split, or a vector
//! file of points with a date field): the Latin hypercube's sets run on the points' series, scored by omission,
//! commission, F1, the dates' agreement and error; the answer is the table of the sets, the Pareto front, which
//! parameters matter, the k-fold optimism of the best set, and the Python loop (zeit-cdts) that redoes the search.
//! A child of mcp.rs: its Server.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use janus_geo::vectors::{VectorData, load_vector_file};
use janus_validation as val;
use janus_zeit::python::{TunePointAt, TuneScriptSet, TuneScriptSpec};
use janus_zeit::tune::{self, ScoreOpts, SetKind, TuneConfig, TuneParam};
use janus_zeit::{Tool, manifest};
use serde_json::{Value, json};

use super::{Call, R, Server, out_dir};
use crate::tune_ui::{self, Done, Prepared, Refs, Wanted};

/// Folds of the optimism check when the call does not say.
const FOLDS: usize = 5;
/// Sets a search runs at most (the window's own limit is far lower).
const MAX_SETS: usize = 5000;

fn round(v: f64, digits: i32) -> Value {
    let m = 10f64.powi(digits);
    if v.is_finite() { json!((v * m).round() / m) } else { Value::Null }
}

/// A set's scores as the answer gives them.
fn scores_of(s: &tune::Scores) -> Value {
    json!({
        "omission": round(s.omission, 4), "commission": round(s.commission, 4), "f1": round(s.f1, 4),
        "f1_dated": round(s.f1_dated, 4), "date_agreement": round(s.date_agreement, 4),
        "mean_abs_error_years": round(s.mean_abs_error, 3),
        "true_positives": s.tp, "omitted": s.fn_, "false_changes": s.fp,
    })
}

impl Server {
    /// The reference of a call: the calibration points of a validation project, or the features of a vector file
    /// with a date field. The answer's `Refs`, what the reference is as text and as data.
    fn tune_reference(&self, a: &Value, weighted: bool) -> R<(Refs, String, Value)> {
        let file = a["reference"].as_str().filter(|s| !s.is_empty()).ok_or(
            "reference: a vector file of points with a date field (and `field`), or a validation project (.gpkg)",
        )?;
        if val::is_project(file) {
            let p = val::load_project(file)?;
            if a["use_accuracy"].as_bool().unwrap_or(false) {
                return Err(tune_ui::accuracy_refusal(&p));
            }
            // a project without a split refuses: the accuracy points cannot calibrate
            let idx = val::calibration_points(&p)?;
            let mapping = match a["labels"].as_object() {
                Some(o) => o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect(),
                None => tune_ui::default_mapping(&p),
            };
            let (points, stats) = tune_ui::project_reference(&p, &idx, &mapping);
            let c = p.calibration.clone().unwrap_or_default();
            let name =
                std::path::Path::new(&p.path).file_name().map_or(p.path.clone(), |n| n.to_string_lossy().into_owned());
            let areas = if weighted { p.strata.strata.iter().map(|s| s.area).collect() } else { Vec::new() };
            let desc = json!({
                "kind": "project", "file": p.path, "calibration_points": idx.len(),
                "split": { "fraction": c.fraction, "seed": c.seed.to_string(), "points": c.ids.len() },
                "labels": mapping,
            });
            let crs = Box::new(VectorData { crs_wkt: p.strata.grid.wkt.clone(), ..Default::default() });
            let text = format!("the calibration points of {name} ({} of {})", idx.len(), p.points.len());
            return Ok((Refs::Project { crs, points, stats, areas }, text, desc));
        }
        if a["use_accuracy"].as_bool().unwrap_or(false) {
            return Err("use_accuracy is for a validation project's points".into());
        }
        let layers = load_vector_file(file, 1_000_000)?;
        let data = match a["layer"].as_str() {
            Some(l) => layers.into_iter().find(|d| d.layer == l).ok_or(format!("{file} has no layer {l:?}"))?,
            None => layers.into_iter().next().ok_or(format!("{file} has no features"))?,
        };
        let field = a["field"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
            format!(
                "field: the date field of the points (a year or a date; empty = no change); the layer has {:?}",
                data.fields
            )
        })?;
        let at = data
            .fields
            .iter()
            .position(|f| f == field)
            .ok_or_else(|| format!("{file} has no field {field:?}; its fields: {:?}", data.fields))?;
        let desc = json!({ "kind": "layer", "layer": data.layer, "file": file, "field": field });
        let text = format!(
            "{}, field {field}",
            std::path::Path::new(file).file_name().map_or(file.into(), |n| n.to_string_lossy())
        );
        Ok((Refs::Layer { data: Arc::new(data), field: at }, text, desc))
    }

    /// `tune_tool`: the search of a tool's parameters on reference points.
    pub(super) fn tune_tool(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let id = a["tool"].as_str().unwrap_or("");
        let tool: &'static Tool = manifest().tool(id).ok_or_else(|| format!("no tool {id:?} (list_tools)"))?;
        let spec = tune::spec(id).ok_or_else(|| {
            format!(
                "{id} has no change date to tune; the tools that can be tuned: {}",
                tune::tunable_tools().join(", ")
            )
        })?;
        let info = self.series(&a["series"])?;
        if let Some(why) = crate::zeit::applicability(tool, &info) {
            return Err(format!("{} cannot run on this series: {why}", tool.name));
        }
        let base: std::collections::BTreeMap<String, Value> = a["params"]
            .as_object()
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        if let Some(bad) = base.keys().find(|k| !tool.params.iter().any(|p| &p.id == *k)) {
            return Err(format!("{} has no parameter {bad:?} (describe_tool)", tool.name));
        }
        let mut roles = crate::zeit::guess_roles(&info);
        for (k, v) in a["roles"].as_object().into_iter().flatten() {
            roles.insert(k.clone(), v.as_i64().ok_or(format!("roles: {k} is a band number"))? as i32);
        }
        // what varies: the tool's default parameters unless the call names them
        let vary: Vec<String> = match a["vary"].as_array() {
            Some(v) => v.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            None => spec.default_params().iter().map(|p| p.param.clone()).collect(),
        };
        if vary.is_empty() {
            return Err("vary: at least one parameter".into());
        }
        let tunable: Vec<&str> = spec.params.iter().map(|p| p.param.as_str()).collect();
        if let Some(bad) = vary.iter().find(|v| spec.param(v).is_none()) {
            return Err(format!(
                "{bad:?} is not a tunable parameter of {}; they are: {}",
                tool.name,
                tunable.join(", ")
            ));
        }
        // ranges the call edits: {param: {min, max} or {choices: [...]}}
        let mut ranges = std::collections::BTreeMap::new();
        for (k, r) in a["ranges"].as_object().into_iter().flatten() {
            let mut tp: TuneParam =
                spec.param(k).cloned().ok_or(format!("ranges: {k:?} is not a tunable parameter"))?;
            if let Some(ch) = r["choices"].as_array() {
                let options: Vec<String> = ch.iter().filter_map(|x| x.as_str().map(String::from)).collect();
                if options.is_empty() || tp.choices.is_none() {
                    return Err(format!("ranges: {k}: choices are for a parameter of options, and need one at least"));
                }
                tp.choices = Some(options);
            } else if tp.choices.is_none() {
                let (lo, hi) = (r["min"].as_f64().or(tp.min), r["max"].as_f64().or(tp.max));
                if lo.zip(hi).is_none_or(|(l, h)| l >= h)
                    || (tp.scale == tune::Scale::Log && lo.is_some_and(|l| l <= 0.0))
                {
                    return Err(format!(
                        "ranges: {k}: min below max{}",
                        if tp.scale == tune::Scale::Log { ", above 0 (log scale)" } else { "" }
                    ));
                }
                (tp.min, tp.max) = (lo, hi);
            }
            ranges.insert(k.clone(), tp);
        }
        let n = info.t();
        let year = |i: usize| info.decimal_year(i).floor() as i32;
        let window = match a["years"].as_array().map(|v| v.iter().filter_map(Value::as_i64).collect::<Vec<_>>()) {
            Some(v) if v.len() == 2 && v[0] <= v[1] => (v[0] as i32, v[1] as i32),
            Some(_) => return Err("years: [first, last] year a change counts in".into()),
            None => (year(0).min(year(n.saturating_sub(1))), year(0).max(year(n.saturating_sub(1)))),
        };
        let weighted = a["weighted"].as_bool().unwrap_or(false);
        let threads = a["threads"].as_u64().map_or(self.threads, |t| (t as usize).clamp(1, self.threads));
        let folds = a["k"].as_u64().map_or(FOLDS, |k| (k as usize).clamp(2, 20));
        let seed = a["seed"].as_u64().unwrap_or(1);
        let n_sets = a["n_sets"].as_u64().map_or(200, |k| (k as usize).clamp(1, MAX_SETS));
        let tolerance = a["tolerance"].as_i64().map_or(1, |k| k.clamp(0, 20) as i32);

        c.progress(0.0, "reading the reference");
        let (refs, text, desc) = self.tune_reference(a, weighted)?;
        let weighted = weighted && matches!(refs, Refs::Project { .. });
        let wanted = Wanted {
            key: String::new(),
            tool,
            info: info.clone(),
            roles: roles.clone(),
            refs,
            text,
            desc,
            base: base.clone(),
            window,
            threads,
            map_t: n.saturating_sub(1),
        };
        c.progress(0.02, "reading the points' series");
        let prep: Prepared = tune_ui::read_points(wanted)?;
        if c.stop.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let mut cfg = TuneConfig::new(id, window);
        cfg.base = base;
        cfg.vary = vary;
        cfg.ranges = ranges;
        cfg.n_sets = n_sets;
        cfg.seed = seed;
        cfg.threads = threads;
        cfg.opts = ScoreOpts { window, tolerance, weighted };
        let total = cfg.n_sets + 2;
        c.progress(0.1, &format!("{total} sets on {} points", prep.points.len()));
        let done_sets = AtomicUsize::new(0);
        let stop = c.stop.clone();
        let t_run = Instant::now();
        let res = std::thread::scope(|s| {
            let worker = s.spawn(|| tune::run(&cfg, &prep.ctx, &prep.points, &stop, &done_sets));
            while !worker.is_finished() {
                let d = done_sets.load(Ordering::Relaxed);
                c.progress(0.1 + 0.85 * d as f64 / total as f64, &format!("{d} of {total} sets"));
                std::thread::sleep(Duration::from_millis(100));
            }
            worker.join().unwrap_or_else(|_| Err("the search stopped on an internal error".into()))
        })?;
        let run_seconds = t_run.elapsed().as_secs_f64();
        let done = Done::new(cfg, res, prep.points.clone(), &prep, run_seconds);
        c.progress(0.97, "writing the answer");

        let r = &done.result;
        let sets: Vec<Value> = done
            .ranked
            .iter()
            .enumerate()
            .map(|(rank, &k)| {
                json!({
                    "set": k,
                    "kind": match r.sets[k].kind { SetKind::Default => "default", SetKind::Current => "current", SetKind::Sampled => "sampled" },
                    "rank": rank + 1,
                    "params": done.values(k),
                    "scores": scores_of(&r.sets[k].scores),
                })
            })
            .collect();
        let failed: Vec<Value> = r
            .sets
            .iter()
            .enumerate()
            .filter_map(|(k, s)| s.error.as_ref().map(|e| json!({ "set": k, "params": done.values(k), "error": e })))
            .collect();
        let prcc: serde_json::Map<String, Value> =
            r.vary.iter().zip(&done.prcc).map(|(p, &v)| (p.clone(), round(v, 3))).collect();
        let optimism = r.optimism(&prep.points, folds, seed).map(|m| {
            json!({
                "folds": m.k,
                "best_set": m.best,
                "in_sample": scores_of(&m.in_sample),
                "held_out": scores_of(&m.held_out),
                "f1_gap": round(m.f1_gap(), 4),
                "chosen_per_fold": m.chosen,
                "fold_f1": m.fold_f1.iter().map(|&v| round(v, 4)).collect::<Vec<_>>(),
            })
        });
        let dir = out_dir(a, &format!("tune_{id}"))?;
        // the Python that redoes the search (when the series and the tool allow it)
        let points_at: Vec<TunePointAt> = prep
            .points
            .iter()
            .zip(prep.places.iter())
            .map(|(p, &(col, row))| TunePointAt { col, row, reference: p.reference, weight: p.weight })
            .collect();
        let script_sets: Vec<TuneScriptSet> = (0..r.sets.len())
            .map(|k| TuneScriptSet {
                name: done.name(k),
                values: r.sets[k].values.clone(),
                f1_dated: Some(r.sets[k].scores.f1_dated).filter(|v| v.is_finite()),
            })
            .collect();
        let path = |p: &str| crate::provenance::absolute(p);
        let script = janus_zeit::python::tune_script(&TuneScriptSpec {
            info: &info,
            version: env!("CARGO_PKG_VERSION"),
            name: &info.description,
            tool: id,
            base: &done.cfg.base,
            input: &prep.ctx,
            vary: &r.vary,
            sets: &script_sets,
            points: &points_at,
            opts: r.opts,
            reference: &done.reference,
            roles: &roles,
            path: &path,
        });
        let py_path = dir.join(crate::provenance::TUNE_SCRIPT);
        let python = match &script {
            Ok(code) => std::fs::write(&py_path, code)
                .map(|()| json!(py_path))
                .unwrap_or_else(|e| json!(format!("{} could not be written: {e}", py_path.display()))),
            Err(why) => json!(format!("no Python of this search: {why}")),
        };
        let mut answer = json!({
            "tool": tool.name,
            "reference": done.desc,
            "reference_text": done.reference,
            "reference_points": { "candidates": prep.stats.candidates, "with_change": prep.stats.with_change, "no_change": prep.stats.no_change, "left_out": prep.stats.left_out, "no_date": prep.stats.no_date, "bad_date": prep.stats.bad_date, "outside_the_series": prep.stats.outside },
            "points": done.points.len(),
            "changes_in_window": r.n_changes,
            "few_changes": r.few_changes,
            "few_changes_below": tune::MIN_CHANGES,
            "year_window": [window.0, window.1],
            "tolerance_years": tolerance,
            "weighted": weighted,
            "varied": r.vary,
            "sets_run": r.sets.len(),
            "seed": seed.to_string(),
            "threads": threads,
            "cancelled": r.cancelled,
            "best_set": done.ranked.first(),
            "sets": sets,
            "pareto_front": done.pareto,
            "pareto_note": "the `set` numbers of the sets no other set beats on both omission and commission, by increasing omission",
            "which_parameters_matter": { "score": "f1_dated", "prcc": prcc, "note": "partial rank correlation of each parameter with the dated F1 over the sets (-1 to 1)" },
            "optimism": optimism,
            "failed_sets": failed,
            "python": python,
            "read_seconds": round(prep.read_seconds, 2),
            "search_seconds": round(run_seconds, 2),
            "seconds": round(t0.elapsed().as_secs_f64(), 1),
            "folder": dir,
        });
        if r.few_changes {
            answer["warning"] = json!(format!(
                "only {} points with a change in the window: the scores are unreliable; collect more points",
                r.n_changes
            ));
        }
        let file = dir.join("tune.json");
        std::fs::write(&file, serde_json::to_string_pretty(&answer).unwrap_or_default())
            .map_err(|e| format!("{}: {e}", file.display()))?;
        answer["file"] = json!(file);
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use gdal::spatial_ref::{AxisMappingStrategy, SpatialRef};

    const W: usize = 20;
    const H: usize = 10;
    const YEARS: std::ops::RangeInclusive<i32> = 1990..=2019;

    /// A series of annual files (1 July) of W x H pixels: 0.75 and a little noise; the pixels of an odd column lose
    /// 0.4 from the year `drop_year(col, row)` on.
    fn annual_series(dir: &std::path::Path) -> String {
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut srs = SpatialRef::from_epsg(32720).unwrap();
        srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        for y in YEARS {
            let p = dir.join(format!("{y}-07-01.tif"));
            let mut d = drv.create_with_band_type::<f32, _>(&p, W, H, 1).unwrap();
            d.set_geo_transform(&[500000.0, 10.0, 0.0, 9000000.0, 0.0, -10.0]).unwrap();
            d.set_spatial_ref(&srs).unwrap();
            let v: Vec<f32> = (0..W * H)
                .map(|i| {
                    let (c, r) = (i % W, i / W);
                    let noise = 0.09 * ((0..4).map(|k| unit(i, y, k)).sum::<f32>() - 2.0);
                    0.75 + noise - if c % 2 == 1 && y >= drop_year(c, r) { 0.4 } else { 0.0 }
                })
                .collect();
            let mut b = d.rasterband(1).unwrap();
            b.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), v)).unwrap();
        }
        dir.to_string_lossy().into_owned()
    }

    /// A pseudo-random number in [0, 1) of a pixel, a year and a draw (SplitMix64's mixing).
    fn unit(i: usize, y: i32, k: u64) -> f32 {
        let mut z = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (((y as u64) << 32) | k);
        z ^= z >> 30;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 27;
        z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    fn drop_year(col: usize, row: usize) -> i32 {
        2000 + ((row * 3 + col) % 12) as i32
    }

    /// The first four rows' pixels as points (80), the date field the year of the drop ("" = no change).
    fn points_file(dir: &std::path::Path) -> String {
        let mut feats = vec![];
        for r in 0..4 {
            for c in 0..W {
                let date = if c % 2 == 1 { drop_year(c, r).to_string() } else { String::new() };
                feats.push(format!(
                    r#"{{"type": "Feature", "properties": {{"date": "{date}"}}, "geometry": {{"type": "Point", "coordinates": [{}, {}]}}}}"#,
                    500005.0 + 10.0 * c as f64,
                    8999995.0 - 10.0 * r as f64
                ));
            }
        }
        let gj = dir.join("points.geojson");
        std::fs::write(
            &gj,
            format!(
                r#"{{"type": "FeatureCollection", "crs": {{"type": "name", "properties": {{"name": "urn:ogc:def:crs:EPSG::32720"}}}}, "features": [{}]}}"#,
                feats.join(",")
            ),
        )
        .unwrap();
        gj.to_string_lossy().into_owned()
    }

    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            crate::configure_data();
            gdal::DriverManager::register_all();
        });
    }

    fn quiet() -> Call {
        Call {
            out: Arc::new(Mutex::new(std::io::stdout())),
            token: Value::Null,
            sent: Mutex::new((0, Instant::now())),
            stop: Arc::default(),
        }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("janus-mcp-{name}-{stamp}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn tunes_landtrendr_on_points_and_writes_its_python() {
        setup();
        let dir = temp("tune");
        let series = annual_series(&dir);
        let points = points_file(&dir);
        let s = Server { threads: 2, ..Default::default() };
        let args = json!({
            "tool": "landtrendr", "series": series, "reference": points, "field": "date", "n_sets": 30,
            // the current values accept any fit: many false changes, which the search must beat
            "params": { "pval_threshold": 0.5 },
            "threads": 2, "output_dir": dir.join("out"),
        });
        let r = s.tune_tool(&args, &quiet()).unwrap();
        assert_eq!(r["points"], 80);
        assert_eq!(r["changes_in_window"], 40);
        assert_eq!(r["few_changes"], false);
        assert_eq!(r["varied"], json!(["max_segments", "spike_threshold", "recovery_threshold", "pval_threshold"]));
        let sets = r["sets"].as_array().unwrap();
        assert_eq!(sets.len(), r["sets_run"].as_u64().unwrap() as usize);
        // sorted by the dated F1, best first, and the best beats the permissive current values
        let f1d = |k: usize| sets[k]["scores"]["f1_dated"].as_f64().unwrap_or(-1.0);
        assert!((1..sets.len()).all(|k| f1d(k - 1) >= f1d(k)));
        let cur = sets.iter().find(|x| x["kind"] == "current").unwrap();
        assert!(f1d(0) > cur["scores"]["f1_dated"].as_f64().unwrap() + 0.1, "{} vs {}", f1d(0), cur["scores"]);
        assert_eq!(sets[0]["rank"], 1);
        // the front's indices are sets of the answer; the sensitivity names every varied parameter
        let ids: Vec<u64> = sets.iter().map(|x| x["set"].as_u64().unwrap()).collect();
        let front = r["pareto_front"].as_array().unwrap();
        assert!(!front.is_empty() && front.iter().all(|k| ids.contains(&k.as_u64().unwrap())));
        assert_eq!(r["which_parameters_matter"]["prcc"].as_object().unwrap().len(), 4);
        assert_eq!(r["optimism"]["folds"], 5);
        // the answer and the Python are in the folder
        assert!(std::fs::read_to_string(r["file"].as_str().unwrap()).unwrap().contains("\"pareto_front\""));
        let py = std::fs::read_to_string(r["python"].as_str().unwrap()).unwrap();
        assert!(py.contains("zeit.landtrendr") && py.contains("POINTS = [") && py.contains("SETS = ["), "{py}");
        // with the Python of JANUS_PYTHON (zeit-cdts): the script's table agrees with the engine's
        match std::env::var("JANUS_PYTHON") {
            Ok(python) => {
                let out = std::process::Command::new(&python)
                    .arg(r["python"].as_str().unwrap())
                    .output()
                    .unwrap_or_else(|e| panic!("{python}: {e}"));
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                assert!(out.status.success(), "{}\n{text}", String::from_utf8_lossy(&out.stderr));
                println!("{text}");
                let gap: f64 = text
                    .lines()
                    .find_map(|l| l.strip_prefix("the dated F1 differs from Janus's table by "))
                    .and_then(|l| l.split(' ').next())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("no comparison line in:\n{text}"));
                assert!(gap < 1e-9, "the script's dated F1 differs from the engine's by {gap}:\n{text}");
            }
            Err(_) => println!("skipped the script's run: JANUS_PYTHON names no Python with zeit-cdts"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Monthly files (the 15th) of 2005-2016, W x H pixels: a season, noise and a drop of 0.3 in the odd columns from
    /// `base` + 0 or 1 on.
    fn monthly_series(dir: &std::path::Path, base: i32) -> String {
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut srs = SpatialRef::from_epsg(32720).unwrap();
        srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        for y in 2005..=2016 {
            for m in 1..=12 {
                let p = dir.join(format!("{y}-{m:02}-15.tif"));
                let mut d = drv.create_with_band_type::<f32, _>(&p, W, H, 1).unwrap();
                d.set_geo_transform(&[500000.0, 10.0, 0.0, 9000000.0, 0.0, -10.0]).unwrap();
                d.set_spatial_ref(&srs).unwrap();
                let t = f64::from(y) + f64::from(m - 1) / 12.0;
                let v: Vec<f32> = (0..W * H)
                    .map(|i| {
                        let (c, r) = (i % W, i / W);
                        let noise = 0.07 * ((0..4).map(|k| unit(i, y * 12 + m, k)).sum::<f32>() - 2.0);
                        let season = 0.1 * (2.0 * std::f64::consts::PI * t).sin() as f32;
                        let drop = c % 2 == 1 && y >= base + ((r * 3 + c) % 2) as i32;
                        0.75 + season + noise - if drop { 0.3 } else { 0.0 }
                    })
                    .collect();
                let mut b = d.rasterband(1).unwrap();
                b.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), v)).unwrap();
            }
        }
        dir.to_string_lossy().into_owned()
    }

    /// The points of the first four rows with the year the monthly series drops in (odd columns).
    fn monthly_points(dir: &std::path::Path, base: i32) -> String {
        let mut feats = vec![];
        for r in 0..4 {
            for c in 0..W {
                let date = if c % 2 == 1 { (base + ((r * 3 + c) % 2) as i32).to_string() } else { String::new() };
                feats.push(format!(
                    r#"{{"type": "Feature", "properties": {{"date": "{date}"}}, "geometry": {{"type": "Point", "coordinates": [{}, {}]}}}}"#,
                    500005.0 + 10.0 * c as f64,
                    8999995.0 - 10.0 * r as f64
                ));
            }
        }
        let gj = dir.join("points.geojson");
        std::fs::write(
            &gj,
            format!(
                r#"{{"type": "FeatureCollection", "crs": {{"type": "name", "properties": {{"name": "urn:ogc:def:crs:EPSG::32720"}}}}, "features": [{}]}}"#,
                feats.join(",")
            ),
        )
        .unwrap();
        gj.to_string_lossy().into_owned()
    }

    /// The Python loops of BFAST, BFAST Lite, BFAST Monitor and EWMACD (zeit-cdts, JANUS_PYTHON) give the engine's
    /// dated F1 on every set: the years they take from Zeit's results are the maps' dates.
    #[test]
    fn the_python_loops_of_the_other_tools_agree_with_the_engine() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit-cdts");
            return;
        };
        setup();
        let s = Server { threads: 2, ..Default::default() };
        for (tool, base) in [("bfast", 2008), ("bfast_lite", 2008), ("bfast_monitor", 2014), ("ewmacd", 2012)] {
            let dir = temp(&format!("tune-{tool}"));
            let series = monthly_series(&dir, base);
            let points = monthly_points(&dir, base);
            let args = json!({
                "tool": tool, "series": series, "reference": points, "field": "date", "n_sets": 3, "threads": 2,
                "output_dir": dir.join("out"),
            });
            let r = s.tune_tool(&args, &quiet()).unwrap_or_else(|e| panic!("{tool}: {e}"));
            let py = r["python"].as_str().unwrap_or_else(|| panic!("{tool}: {}", r["python"]));
            let out = std::process::Command::new(&python).arg(py).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(out.status.success(), "{tool}: {}\n{text}", String::from_utf8_lossy(&out.stderr));
            let gap: f64 = text
                .lines()
                .find_map(|l| l.strip_prefix("the dated F1 differs from Janus's table by "))
                .and_then(|l| l.split(' ').next())
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{tool}: no comparison line in:\n{text}"));
            println!("{tool}: {gap}");
            assert!(gap < 1e-9, "{tool}: the script's dated F1 differs from the engine's by {gap}:\n{text}");
            // not every set may be empty: something is found
            assert!(r["sets"][0]["scores"]["f1"].as_f64().unwrap_or(0.0) > 0.0, "{tool}: {}", r["sets"][0]);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn refuses_what_cannot_calibrate() {
        setup();
        let dir = temp("tune-refuse");
        let series = annual_series(&dir);
        let points = points_file(&dir);
        let s = Server { threads: 2, ..Default::default() };
        let c = quiet();
        let run = |extra: Value| {
            let mut a =
                json!({ "tool": "landtrendr", "series": series, "reference": points, "field": "date", "n_sets": 2 });
            for (k, v) in extra.as_object().unwrap() {
                a[k] = v.clone();
            }
            s.tune_tool(&a, &c).unwrap_err()
        };
        assert!(run(json!({ "tool": "smooth" })).contains("no change date to tune"));
        assert!(run(json!({ "tool": "nope" })).contains("no tool"));
        assert!(run(json!({ "vary": ["event_type"] })).contains("not a tunable parameter"));
        assert!(run(json!({ "field": "nothing" })).contains("no field"));
        assert!(run(json!({ "field": "" })).contains("date field"));
        assert!(run(json!({ "use_accuracy": true })).contains("validation project"));
        assert!(run(json!({ "years": [2010, 2000] })).contains("years"));
        assert!(run(json!({ "reference": "" })).contains("reference"));
        assert!(run(json!({ "params": { "bogus": 1 } })).contains("no parameter"));
        assert!(run(json!({ "ranges": { "max_segments": { "min": 8, "max": 3 } } })).contains("min below max"));
        assert!(run(json!({ "ranges": { "max_segments": { "choices": ["a"] } } })).contains("choices"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
