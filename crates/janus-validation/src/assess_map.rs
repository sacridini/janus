//! A map of the project assessed: its classes, the reference labels translated to them, the estimators' input,
//! the area of change by year, and the Python (zeit.accuracy) that gives the same numbers (src/validation.cpp:
//! defaultMapping, mapClasses, prepare, changeByYear, pythonSnippet).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::NONE;
use crate::estimate::{AreaEstimator, AssessInput, ClassArea, Estimator};
use crate::labels::{Label, resolve, year_of};
use crate::project::{AssessedMap, Project};
use crate::rules::{MapKind, MapRule, fmt_g};

/// What a map of events calls the reference labels: "change", "no change" or "" (left out).
pub const CHANGE: &str = "change";
pub const NO_CHANGE: &str = "no change";

fn has_any(s: &str, words: &[&str]) -> bool {
    words.iter().any(|w| s.contains(w))
}

fn cannot_tell(l: &str) -> bool {
    has_any(l, &["can't", "cant", "cannot", "can not", "unsure", "unknown", "cloud", "no data", "nodata", "n/a"])
}

/// Years of the changes of a point's labels that the map calls change (sorted); `any`: a change label at all.
fn change_years(l: &Label, mapping: &BTreeMap<String, String>) -> (Vec<i32>, bool) {
    let mut ys = Vec::new();
    let mut any = false;
    let mut add = |label: &str, date: &str| {
        if mapping.get(label).map(String::as_str) != Some(CHANGE) {
            return;
        }
        any = true;
        let y = year_of(date);
        if y != 0 {
            ys.push(y);
        }
    };
    add(&l.label, &l.date);
    for e in &l.events {
        add(&e.label, &e.date);
    }
    ys.sort_unstable();
    (ys, any)
}

/// Translations guessed from the names: a label equal to a class (case aside); for events, labels of loss and
/// disturbance are change, the rest no change; "can't tell", clouds and no data left out. Labels not guessed are
/// absent.
pub fn default_mapping(r: &MapRule, label_set: &[String], class_names: &[String]) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for label in label_set {
        let l = label.to_ascii_lowercase();
        if cannot_tell(&l) {
            m.insert(label.clone(), String::new());
            continue;
        }
        if r.kind == MapKind::Events {
            let stable = has_any(&l, &["no change", "stable", "unchanged", "persist"]);
            let change = has_any(
                &l,
                &[
                    "deforest", "degrad", "loss", "clear", "fire", "burn", "harvest", "logging", "mining", "change",
                    "disturb", "cut", "conver", "drought", "flood",
                ],
            );
            m.insert(label.clone(), if change && !stable { CHANGE } else { NO_CHANGE }.to_string());
            continue;
        }
        for c in class_names {
            if c.to_ascii_lowercase() == l {
                m.insert(label.clone(), c.clone());
            }
        }
    }
    m
}

/// The classes of the map: those it has (areas, values at the points) in order.
pub fn map_classes(r: &MapRule, values: &[f64], mapped: &BTreeMap<i32, f64>) -> Vec<i32> {
    match r.kind {
        MapKind::Classes => {
            let mut s: BTreeSet<i32> =
                mapped.iter().filter(|&(&c, &a)| a > 0.0 && c != NONE).map(|(&c, _)| c).collect();
            for &v in values {
                let c = r.class_of(v);
                if c != NONE {
                    s.insert(c);
                }
            }
            s.into_iter().collect()
        }
        MapKind::Intervals => (1..r.bins.len() as i32).collect(),
        MapKind::Events => std::iter::once(0).chain(1..(r.bins.len() as i32).max(2)).collect(),
    }
}

/// The estimator's input for one map: its class and the reference's at each point, the strata's areas; what was
/// left out and why.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    pub input: AssessInput,
    /// the map's class of each of input.classes (NONE: a reference class only)
    pub class_keys: Vec<i32>,
    /// maps of events: years at each point (0: none)
    pub map_year: Vec<i32>,
    pub ref_year: Vec<i32>,
    pub labelled: i32,
    pub unlabelled: i32,
    pub conflicts: i32,
    pub unmapped: i32,
    pub excluded: i32,
    /// points that calibrate a tool (the project's calibration split): left out of the assessment
    pub calibration: i32,
    pub no_date: i32,
    pub off_map: i32,
    pub unmapped_labels: BTreeSet<String>,
}

/// `use_points`: the points to count (empty: all), e.g. the first labelled ones.
pub fn prepare(
    p: &Project,
    m: &AssessedMap,
    values: &[f64],
    mapped: &BTreeMap<i32, f64>,
    confidence: f64,
    use_points: &[bool],
) -> Prepared {
    let mut o = Prepared::default();
    let r = &m.rule;
    let n = p.points.len();
    // the points answered year by year read with this map's translation
    let labels = crate::labels::for_mapping(&p.labels, &p.years, &m.mapping);
    o.class_keys = map_classes(r, values, mapped);
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();
    let mut idx_of_key: BTreeMap<i32, usize> = BTreeMap::new();
    for &k in &o.class_keys {
        idx_of_key.insert(k, o.input.classes.len());
        idx.insert(r.class_name(k), o.input.classes.len());
        o.input.classes.push(r.class_name(k));
        o.input.mapped.push(mapped.get(&k).copied().unwrap_or(0.0));
    }
    let class_index = |o: &mut Prepared, idx: &mut BTreeMap<String, usize>, name: &str| -> usize {
        if let Some(&i) = idx.get(name) {
            return i;
        }
        idx.insert(name.to_string(), o.input.classes.len());
        o.input.classes.push(name.to_string());
        o.input.mapped.push(0.0);
        o.class_keys.push(NONE);
        o.input.classes.len() - 1
    };
    o.input.confidence = confidence;
    o.input.estimator = p.estimator;
    o.input.stratum_area = p.strata.strata.iter().map(|s| s.area).collect();
    o.input.stratum = vec![None; n];
    o.input.map = vec![None; n];
    o.input.reference = vec![None; n];
    o.map_year = vec![0; n];
    o.ref_year = vec![0; n];
    for i in 0..n {
        if !use_points.is_empty() && !use_points.get(i).copied().unwrap_or(false) {
            continue;
        }
        if p.calibration.as_ref().is_some_and(|c| c.has(p.points[i].id)) {
            o.calibration += 1;
            continue;
        }
        o.input.stratum[i] = p.strata.index_of_code(p.points[i].stratum);
        let v = values.get(i).copied().unwrap_or(f64::NAN);
        let c = r.class_of(v);
        if c != NONE {
            o.input.map[i] = idx_of_key.get(&c).copied();
        }
        if r.kind == MapKind::Events && v.is_finite() && v > 0.0 {
            o.map_year[i] = v.floor() as i32;
        }
        let (l, conflict) = resolve(&labels, i);
        let Some(l) = l else {
            if conflict {
                o.conflicts += 1;
            } else {
                o.unlabelled += 1;
            }
            continue;
        };
        o.labelled += 1;
        if o.input.map[i].is_none() {
            o.off_map += 1;
        }
        let tr = m.mapping.get(&l.label);
        if r.kind == MapKind::Events {
            let (ys, any) = change_years(l, &m.mapping);
            if !any {
                match tr {
                    None => {
                        o.unmapped += 1;
                        o.unmapped_labels.insert(l.label.clone());
                    }
                    Some(t) if t.is_empty() => o.excluded += 1,
                    Some(_) => o.input.reference[i] = Some(class_index(&mut o, &mut idx, NO_CHANGE)),
                }
                continue;
            }
            if r.bins.is_empty() {
                o.input.reference[i] = Some(class_index(&mut o, &mut idx, CHANGE));
                o.ref_year[i] = ys.first().copied().unwrap_or(0);
                continue;
            }
            if ys.is_empty() {
                o.no_date += 1;
                continue;
            }
            let found = ys.iter().find_map(|&y| {
                let k = r.class_of(f64::from(y));
                (k != NONE && k > 0).then_some((k, y))
            });
            match found {
                // changes, but none in the map's periods
                None => o.input.reference[i] = Some(class_index(&mut o, &mut idx, NO_CHANGE)),
                Some((period, year)) => {
                    o.input.reference[i] = Some(class_index(&mut o, &mut idx, &r.class_name(period)));
                    o.ref_year[i] = year;
                }
            }
            continue;
        }
        let target = match tr {
            Some(t) if t.is_empty() => {
                o.excluded += 1;
                continue;
            }
            Some(t) => t.clone(),
            None if idx.contains_key(&l.label) => l.label.clone(),
            None => {
                o.unmapped += 1;
                o.unmapped_labels.insert(l.label.clone());
                continue;
            }
        };
        o.input.reference[i] = Some(class_index(&mut o, &mut idx, &target));
    }
    // a point without a reference or off the map does not count
    for i in 0..n {
        if o.input.reference[i].is_none() || o.input.map[i].is_none() {
            o.input.stratum[i] = None;
        }
    }
    o
}

/// The area of change of each year from the reference alone (the dates of the labels that `m` calls change): the
/// area whose first change in the period is in that year, with its interval.
pub fn change_by_year(p: &Project, m: &AssessedMap, confidence: f64) -> Vec<(i32, ClassArea)> {
    let n = p.points.len();
    let labels = crate::labels::for_mapping(&p.labels, &p.years, &m.mapping);
    let mut st: Vec<Option<usize>> = vec![None; n];
    let mut year = vec![0; n];
    let (mut y0, mut y1) = (i32::MAX, i32::MIN);
    let period = m.rule.kind == MapKind::Events && m.rule.bins.len() >= 2;
    for i in 0..n {
        // the points that calibrate do not estimate
        if p.calibration.as_ref().is_some_and(|c| c.has(p.points[i].id)) {
            continue;
        }
        let (Some(l), _) = resolve(&labels, i) else { continue };
        let (ys, any) = change_years(l, &m.mapping);
        if any && ys.is_empty() {
            continue; // a change without a date: unknown year
        }
        if !any && m.mapping.get(&l.label).is_none_or(|t| t.is_empty()) {
            continue;
        }
        st[i] = p.strata.index_of_code(p.points[i].stratum);
        for &y in &ys {
            if !period || (f64::from(y) >= m.rule.bins[0] && f64::from(y) < *m.rule.bins.last().unwrap()) {
                year[i] = y;
                y0 = y0.min(y);
                y1 = y1.max(y);
                break;
            }
        }
    }
    if y0 > y1 {
        return Vec::new();
    }
    let area: Vec<f64> = p.strata.strata.iter().map(|s| s.area).collect();
    let e = Estimator::new(&area, &st, confidence);
    let total = p.strata.total_area();
    let mut out = Vec::new();
    let mut ind = vec![0.0; n];
    for y in y0..=y1 {
        for i in 0..n {
            ind[i] = if year[i] == y { 1.0 } else { 0.0 };
        }
        let v = e.mean(&ind);
        let mut a = ClassArea { est: v.est * total, se: v.se * total, ..Default::default() };
        a.ci = e.z * a.se;
        a.lo = a.est - a.ci;
        a.hi = a.est + a.ci;
        out.push((y, a));
    }
    out
}

pub(crate) fn py_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                o.push('\\');
                o.push(c);
            }
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

pub(crate) fn py_num(v: f64) -> String {
    if v.is_infinite() {
        return if v < 0.0 { "float(\"-inf\")".into() } else { "float(\"inf\")".into() };
    }
    fmt_g(v, 17)
}

/// A path for Python: forward slashes.
pub(crate) fn py_path(p: &str) -> String {
    py_str(&p.replace('\\', "/"))
}

/// Python (zeit.accuracy) that gives the same estimates from the project's files: the GeoPackage's sample points
/// read with `gpd.read_file`, the maps with `zeit.load_raster`, the estimates by `zeit.accuracy`.
pub fn python_snippet(p: &Project, m: &AssessedMap, confidence: f64) -> String {
    let r = &m.rule;
    let events = r.kind == MapKind::Events;
    let periods = events && r.bins.len() >= 2;
    let file = Path::new(&p.path).file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned());
    let mut s = format!(
        "# Accuracy and error-adjusted areas of {} (Olofsson et al. 2014; Stehman 2014),\n# from the Janus validation project {file}\n",
        if r.name.is_empty() { &r.path } else { &r.name }
    );
    s += "import geopandas as gpd\n";
    if periods {
        s += "import pandas as pd\n";
    }
    if events {
        s += "import xarray as xr\n";
    }
    s += "import zeit\n\n";
    s += &format!(
        "samples = gpd.read_file({}, layer=\"samples\")  # the points, at their pixels' centres\n",
        py_path(&p.path)
    );
    s += "samples = samples[samples[\"status\"] == \"labelled\"]  # an agreed (or adjudicated) reference label\n";
    s += "samples[\"ref\"] = samples[\"label\"].map({  # reference label -> the map's class (a label not listed is left out)\n";
    for (label, target) in &m.mapping {
        if target.is_empty() {
            continue;
        }
        let code = || {
            let named = r.names.iter().find(|(_, name)| *name == target).map(|(code, _)| code.to_string());
            named.or_else(|| if r.names.is_empty() { target.parse::<i64>().ok().map(|c| c.to_string()) } else { None })
        };
        let v = if events {
            py_str(if target == CHANGE { CHANGE } else { NO_CHANGE })
        } else if r.kind == MapKind::Classes {
            code().unwrap_or_else(|| py_str(target))
        } else {
            py_str(target)
        };
        s += &format!("    {}: {v},\n", py_str(label));
    }
    s += "})\n";
    if periods {
        // a change falls in the map's period of its year (the first four-digit number of the reference date); a
        // change without a year is left out, and one outside the periods is no change
        let names: Vec<String> = (1..r.bins.len() as i32).map(|k| py_str(&r.class_name(k))).collect();
        s += "year = samples[\"ref_date\"].str.extract(r\"(?<!\\d)(\\d{4})(?!\\d)\", expand=False).astype(float)\n";
        s += "change = samples[\"ref\"] == \"change\"\n";
        s += &format!(
            "period = pd.cut(year[change], [{}], right=False, labels=[{}]).astype(object)\n",
            r.bins.iter().map(|&b| py_num(b)).collect::<Vec<_>>().join(", "),
            names.join(", ")
        );
        s += "samples.loc[change, \"ref\"] = period.where(period.notna() | year[change].isna(), \"no change\")\n";
    }
    if events {
        // Janus reads a point's reference from all of its dated events: the points where that differs from the
        // rules above are given as they are
        let pr = prepare(p, m, &vec![f64::NAN; p.points.len()], &BTreeMap::new(), confidence, &[]);
        let mut fix = Vec::new();
        let labels = crate::labels::for_mapping(&p.labels, &p.years, &m.mapping);
        for (i, pt) in p.points.iter().enumerate() {
            let Some(l) = resolve(&labels, i).0 else { continue };
            let simple = match m.mapping.get(&l.label).map(String::as_str) {
                None | Some("") => None,
                Some(CHANGE) if r.bins.is_empty() => Some(CHANGE.to_string()),
                Some(CHANGE) => match year_of(&l.date) {
                    0 => None,
                    y => Some(match r.class_of(f64::from(y)) {
                        k if k != NONE && k > 0 => r.class_name(k),
                        _ => NO_CHANGE.to_string(),
                    }),
                },
                Some(_) => Some(NO_CHANGE.to_string()),
            };
            let actual = pr.input.reference[i].map(|c| pr.input.classes[c].clone());
            if simple != actual {
                fix.push(format!("{}: {}", pt.id, actual.map_or("None".to_string(), |a| py_str(&a))));
            }
        }
        if !fix.is_empty() {
            s += &format!("fix = {{{}}}  # points with several dated labels\n", fix.join(", "));
            s += "samples.loc[samples[\"sample_id\"].isin(fix), \"ref\"] = samples[\"sample_id\"].map(fix)\n";
        }
    }
    s += "\n";
    let band = if r.band == 1 { String::new() } else { format!(", band={}", r.band) };
    let the_map = if events {
        s += &format!(
            "the_map = xr.Dataset({{\"yod\": zeit.load_raster({}, masked=True{band})}})  # a map of events: > 0 change\n",
            py_path(&r.path)
        );
        "the_map".to_string()
    } else if r.band == 1 {
        py_path(&r.path)
    } else {
        s += &format!("the_map = zeit.load_raster({}{band})\n", py_path(&r.path));
        "the_map".to_string()
    };
    let bins = if r.bins.len() >= 2 {
        format!(", bins=[{}]", r.bins.iter().map(|&b| py_num(b)).collect::<Vec<_>>().join(", "))
    } else {
        String::new()
    };
    // the model-assisted estimators use the map's areas inside the strata, which Zeit counts as Janus does when
    // the map is on the strata's grid
    let estimator = if p.estimator == AreaEstimator::Direct {
        String::new()
    } else {
        format!(", estimator={}", py_str(p.estimator.name()))
    };
    s += &format!(
        "acc = zeit.accuracy({the_map}, samples, strata={}{bins}, confidence={}{estimator})\n",
        py_path(&p.strata_path),
        fmt_g(confidence, 6)
    );
    s += "print(acc)\n";
    s
}
