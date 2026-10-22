//! A map of the project assessed: its classes, the reference labels translated to them, the estimators' input,
//! the area of change by year, and the Python (zeit.accuracy) that gives the same numbers (src/validation.cpp:
//! defaultMapping, mapClasses, prepare, changeByYear, pythonSnippet).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::NONE;
use crate::estimate::{AssessInput, ClassArea, Estimator};
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
        o.input.stratum[i] = p.strata.index_of_code(p.points[i].stratum);
        let v = values.get(i).copied().unwrap_or(f64::NAN);
        let c = r.class_of(v);
        if c != NONE {
            o.input.map[i] = idx_of_key.get(&c).copied();
        }
        if r.kind == MapKind::Events && v.is_finite() && v > 0.0 {
            o.map_year[i] = v.floor() as i32;
        }
        let (l, conflict) = resolve(&p.labels, i);
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
    let mut st: Vec<Option<usize>> = vec![None; n];
    let mut year = vec![0; n];
    let (mut y0, mut y1) = (i32::MAX, i32::MIN);
    let period = m.rule.kind == MapKind::Events && m.rule.bins.len() >= 2;
    for i in 0..n {
        let (Some(l), _) = resolve(&p.labels, i) else { continue };
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

fn py_str(s: &str) -> String {
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

fn py_num(v: f64) -> String {
    if v.is_infinite() {
        return if v < 0.0 { "float(\"-inf\")".into() } else { "float(\"inf\")".into() };
    }
    fmt_g(v, 17)
}

/// A path for Python: forward slashes.
fn py_path(p: &str) -> String {
    py_str(&p.replace('\\', "/"))
}

/// Python (zeit.accuracy) that gives the same estimates from the project's files.
pub fn python_snippet(p: &Project, m: &AssessedMap, confidence: f64) -> String {
    let r = &m.rule;
    let mut s = String::new();
    let file = Path::new(&p.path).file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned());
    s += &format!(
        "# Accuracy and error-adjusted areas of {} (Olofsson et al. 2014; Stehman 2014),\n# from the Janus validation project {file}\n",
        if r.name.is_empty() { &r.path } else { &r.name }
    );
    s += "import sqlite3\n\nimport geopandas as gpd\nimport pandas as pd\nimport rioxarray\n";
    if r.kind == MapKind::Events {
        s += "import xarray as xr\n";
    }
    s += "import zeit\n\n";
    s += "# The sample points (a GeoPackage is SQLite), at their pixels' centres in the strata raster\n";
    s += &format!("with sqlite3.connect({}) as db:\n", py_path(&p.path));
    s += "    samples = pd.read_sql(\"SELECT sample_id, stratum, row, col, label, ref_date, status FROM samples\", db)\n";
    s += &format!("strata = rioxarray.open_rasterio({}).squeeze(drop=True)\n", py_path(&p.strata_path));
    s += "x, y = strata.rio.transform() * (samples[\"col\"] + 0.5, samples[\"row\"] + 0.5)\n";
    s += "samples = gpd.GeoDataFrame(samples, geometry=gpd.points_from_xy(x, y), crs=strata.rio.crs)\n";
    s += "samples = samples[samples[\"status\"] == \"labelled\"]  # an agreed (or adjudicated) reference label\n";
    if r.kind == MapKind::Events {
        // change comes from any of a point's events, and its period from their dates: the reference as Janus reads it
        let pr = prepare(p, m, &vec![f64::NAN; p.points.len()], &BTreeMap::new(), confidence, &[]);
        s += "# The reference class of each point: its labels (and dates) translated to the map's periods\n";
        s += "reference = {";
        let mut k = 0;
        for (i, pt) in p.points.iter().enumerate() {
            let Some(c) = pr.input.reference[i] else { continue };
            if k > 0 {
                s += ", ";
            }
            k += 1;
            s += &format!("{}: {}", pt.id, py_str(&pr.input.classes[c]));
            if k % 8 == 0 {
                s += "\n             ";
            }
        }
        s += "}\nsamples[\"ref\"] = samples[\"sample_id\"].map(reference)\n";
    } else {
        s += "to_map = {  # reference label -> the map's class\n";
        for (label, target) in &m.mapping {
            if target.is_empty() {
                s += &format!("    {}: None,\n", py_str(label));
                continue;
            }
            let mut v = py_str(target);
            if r.kind == MapKind::Classes {
                for (code, name) in &r.names {
                    if name == target {
                        v = code.to_string();
                    }
                }
                if r.names.is_empty()
                    && let Ok(code) = target.parse::<i64>()
                {
                    v = code.to_string();
                }
            }
            s += &format!("    {}: {v},\n", py_str(label));
        }
        s += "}\nsamples[\"ref\"] = samples[\"label\"].map(to_map)\n";
    }
    s += "samples = samples[samples[\"ref\"].notna()]\n\n";
    s += &format!(
        "the_map = rioxarray.open_rasterio({}, masked=True).sel(band={}, drop=True)\n",
        py_path(&r.path),
        r.band
    );
    let mut bins = String::new();
    if r.bins.len() >= 2 {
        bins = format!(", bins=[{}]", r.bins.iter().map(|&b| py_num(b)).collect::<Vec<_>>().join(", "));
    }
    if r.kind == MapKind::Events {
        s += "the_map = xr.Dataset({\"yod\": the_map.fillna(0)})  # a map of events: > 0 change\n";
    }
    s += &format!(
        "acc = zeit.accuracy(the_map, samples, strata={}{bins}, confidence={})\n",
        py_path(&p.strata_path),
        fmt_g(confidence, 6)
    );
    s += "print(acc)\n";
    s
}
