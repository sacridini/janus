//! The transitions between two dates of a map of classes: each pair of a class at the first date and a class at
//! the second ("Forest -> Pasture", "Forest -> Forest") is a class of a map of transitions, assessed as a map is
//! (the error matrix, accuracies and the error-adjusted area of each transition, by the direct, difference or ratio
//! estimator), as zeit.transitions does. The reference class of a point at a date comes from its labels, which
//! describe the ground: its label and its other events, each a class from its date on (an undated one: before every
//! dated one).

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::NONE;
use crate::assess_map::{Prepared, py_path, py_str};
use crate::labels::{Label, resolve, year_of};
use crate::project::{AssessedMap, Project};
use crate::rules::{MapKind, MapRule, fmt_g, str_of};

/// Between the names of the two classes of a transition.
pub const ARROW: &str = " -> ";

/// The transitions assessed: from map `from` of the project at `from_date` to map `to` at `to_date` (dates as the
/// labels give them, e.g. a series' date labels).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransitionSpec {
    pub from: usize,
    pub to: usize,
    pub from_date: String,
    pub to_date: String,
}

impl TransitionSpec {
    pub fn to_json(&self) -> Value {
        json!({"from": self.from, "to": self.to, "from_date": self.from_date, "to_date": self.to_date})
    }

    pub fn from_json(j: &Value) -> Option<TransitionSpec> {
        Some(TransitionSpec {
            from: j.get("from")?.as_u64()? as usize,
            to: j.get("to")?.as_u64()? as usize,
            from_date: str_of(j, "from_date"),
            to_date: str_of(j, "to_date"),
        })
    }
}

/// A date as (year, month, day), 0 where not given: "2019-07-14", "2019-07", "2019", "14/07/2019"; None without a
/// year.
pub fn date_parts(s: &str) -> Option<[i32; 3]> {
    let y = year_of(s);
    if y == 0 {
        return None;
    }
    let groups: Vec<&str> = s.split(|c: char| !c.is_ascii_digit()).filter(|g| !g.is_empty()).collect();
    let k = groups.iter().position(|g| g.len() == 4 && g.parse() == Ok(y))?;
    let num = |i: usize| groups.get(i).and_then(|g| g.parse::<i32>().ok()).unwrap_or(0);
    let (m, d) = if k == 0 {
        (num(1), num(2)) // year first
    } else if k == 2 {
        (num(1), num(0)) // day/month/year
    } else {
        (0, 0)
    };
    let m = if (1..=12).contains(&m) { m } else { 0 };
    let d = if m > 0 && (1..=31).contains(&d) { d } else { 0 };
    Some([y, m, d])
}

/// `event` on or before `at`, compared to the precision of the less precise (an event in "2019" is on or before
/// "2019-01-01"); None if either has no year.
pub fn on_or_before(event: &str, at: &str) -> Option<bool> {
    let (e, a) = (date_parts(event)?, date_parts(at)?);
    for k in 0..3 {
        if e[k] == 0 || a[k] == 0 {
            break;
        }
        if e[k] != a[k] {
            return Some(e[k] < a[k]);
        }
    }
    Some(true)
}

/// The label that holds at `at` among a point's label and events that `counts` (classes of the map): the latest
/// dated on or before it, else the first undated one; None if neither.
pub fn label_at<'a>(l: &'a Label, at: &str, counts: &dyn Fn(&str) -> bool) -> Option<&'a str> {
    let mut undated: Option<&str> = None;
    let mut dated: Option<(&str, [i32; 3])> = None;
    let entries = std::iter::once((l.label.as_str(), l.date.as_str()))
        .chain(l.events.iter().map(|e| (e.label.as_str(), e.date.as_str())));
    for (label, date) in entries {
        if label.is_empty() || !counts(label) {
            continue;
        }
        match date_parts(date) {
            None => {
                if undated.is_none() {
                    undated = Some(label);
                }
            }
            Some(parts) => {
                if on_or_before(date, at) != Some(true) {
                    continue;
                }
                // the latest; of equal dates, the later given
                if dated.is_none_or(|(_, p)| parts >= p) {
                    dated = Some((label, parts));
                }
            }
        }
    }
    dated.map(|(l, _)| l).or(undated)
}

/// The date of a map, guessed from its name ("lc @ 2015-01-01": its series' date; else a year in it), or "".
pub fn guess_date(name: &str) -> String {
    if let Some((_, d)) = name.rsplit_once(" @ ")
        && date_parts(d).is_some()
    {
        return d.trim().to_string();
    }
    match year_of(name) {
        0 => String::new(),
        y => y.to_string(),
    }
}

/// The estimators' input for the transitions, what was left out and why, and each point's reference class at
/// each date (for the Python).
#[derive(Clone, Debug, Default)]
pub struct Transitions {
    /// input.classes are the transitions "<from> -> <to>", input.mapped their mapped areas; map_year and ref_year
    /// unused; no_date counts the points whose labels give no class at one of the dates
    pub prep: Prepared,
    /// per transition: the maps' classes (NONE: not a class of the map)
    pub pairs: Vec<(i32, i32)>,
    /// per point: the reference's class at each date (names of the maps' classes), when it has both
    pub ref_from: Vec<Option<String>>,
    pub ref_to: Vec<Option<String>>,
}

/// A map's class of a reference label: its translation, else the class of that name; "" left out; None unknown.
fn translate<'a>(m: &'a AssessedMap, names: &'a BTreeMap<String, i32>, label: &'a str) -> Option<&'a str> {
    match m.mapping.get(label) {
        Some(t) => Some(t.as_str()),
        None => names.get_key_value(label).map(|(k, _)| k.as_str()),
    }
}

/// The transitions from map `t.from` to `t.to` of the project: their values at the points, the mapped area of each
/// pair of classes (`mapped_transitions`). Both maps should be maps of classes. `use_points`: the points to count
/// (empty: all).
pub fn prepare_transitions(
    p: &Project,
    t: &TransitionSpec,
    from_values: &[f64],
    to_values: &[f64],
    mapped: &BTreeMap<(i32, i32), f64>,
    confidence: f64,
    use_points: &[bool],
) -> Transitions {
    let mut o = Transitions::default();
    let (Some(mf), Some(mt)) = (p.maps.get(t.from), p.maps.get(t.to)) else { return o };
    let (rf, rt) = (&mf.rule, &mt.rule);
    let n = p.points.len();
    // the maps' classes by name
    let names_of = |r: &MapRule, values: &[f64], keys: &mut Vec<i32>| {
        let mut s: std::collections::BTreeSet<i32> = keys.iter().copied().collect();
        for &v in values {
            let c = r.class_of(v);
            if c != NONE {
                s.insert(c);
            }
        }
        *keys = s.into_iter().collect();
        keys.iter().map(|&c| (r.class_name(c), c)).collect::<BTreeMap<String, i32>>()
    };
    let (mut kf, mut kt): (Vec<i32>, Vec<i32>) = (Vec::new(), Vec::new());
    for (&(a, b), &area) in mapped {
        if area > 0.0 && a != NONE && b != NONE {
            kf.push(a);
            kt.push(b);
        }
    }
    let names_f = names_of(rf, from_values, &mut kf);
    let names_t = names_of(rt, to_values, &mut kt);
    // the map's transitions (with an area or at a point), in the order of the classes
    let mut pairs: std::collections::BTreeSet<(i32, i32)> =
        mapped.iter().filter(|&(&(a, b), &area)| area > 0.0 && a != NONE && b != NONE).map(|(&k, _)| k).collect();
    for i in 0..n.min(from_values.len()).min(to_values.len()) {
        let (a, b) = (rf.class_of(from_values[i]), rt.class_of(to_values[i]));
        if a != NONE && b != NONE {
            pairs.insert((a, b));
        }
    }
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();
    let mut idx_of_pair: BTreeMap<(i32, i32), usize> = BTreeMap::new();
    for &(a, b) in &pairs {
        let name = format!("{}{ARROW}{}", rf.class_name(a), rt.class_name(b));
        idx_of_pair.insert((a, b), o.prep.input.classes.len());
        idx.insert(name.clone(), o.prep.input.classes.len());
        o.prep.input.classes.push(name);
        o.prep.input.mapped.push(mapped.get(&(a, b)).copied().unwrap_or(0.0));
        o.pairs.push((a, b));
        o.prep.class_keys.push(NONE);
    }
    o.prep.input.confidence = confidence;
    o.prep.input.estimator = p.estimator;
    o.prep.input.stratum_area = p.strata.strata.iter().map(|s| s.area).collect();
    o.prep.input.stratum = vec![None; n];
    o.prep.input.map = vec![None; n];
    o.prep.input.reference = vec![None; n];
    o.ref_from = vec![None; n];
    o.ref_to = vec![None; n];
    for i in 0..n {
        if !use_points.is_empty() && !use_points.get(i).copied().unwrap_or(false) {
            continue;
        }
        o.prep.input.stratum[i] = p.strata.index_of_code(p.points[i].stratum);
        let a = rf.class_of(from_values.get(i).copied().unwrap_or(f64::NAN));
        let b = rt.class_of(to_values.get(i).copied().unwrap_or(f64::NAN));
        if a != NONE && b != NONE {
            o.prep.input.map[i] = idx_of_pair.get(&(a, b)).copied();
        }
        let (l, conflict) = resolve(&p.labels, i);
        let Some(l) = l else {
            if conflict {
                o.prep.conflicts += 1;
            } else {
                o.prep.unlabelled += 1;
            }
            continue;
        };
        o.prep.labelled += 1;
        if o.prep.input.map[i].is_none() {
            o.prep.off_map += 1;
        }
        let (tf, tt) = (translate(mf, &names_f, &l.label), translate(mt, &names_t, &l.label));
        if tf == Some("") || tt == Some("") {
            o.prep.excluded += 1;
            continue;
        }
        if tf.is_none() && tt.is_none() && l.events.is_empty() {
            o.prep.unmapped += 1;
            o.prep.unmapped_labels.insert(l.label.clone());
            continue;
        }
        let class_at = |m: &AssessedMap, names: &BTreeMap<String, i32>, at: &str| -> Option<String> {
            let counts = |label: &str| translate(m, names, label).is_some_and(|c| !c.is_empty());
            label_at(l, at, &counts).and_then(|label| translate(m, names, label)).map(String::from)
        };
        let (Some(cf), Some(ct)) = (class_at(mf, &names_f, &t.from_date), class_at(mt, &names_t, &t.to_date)) else {
            if tf.is_none() || tt.is_none() {
                o.prep.unmapped += 1;
                o.prep.unmapped_labels.insert(l.label.clone());
            } else {
                o.prep.no_date += 1;
            }
            continue;
        };
        let name = format!("{cf}{ARROW}{ct}");
        let k = match idx.get(&name) {
            Some(&k) => k,
            None => {
                let k = o.prep.input.classes.len();
                idx.insert(name.clone(), k);
                o.prep.input.classes.push(name);
                o.prep.input.mapped.push(0.0);
                o.pairs.push((names_f.get(&cf).copied().unwrap_or(NONE), names_t.get(&ct).copied().unwrap_or(NONE)));
                o.prep.class_keys.push(NONE);
                k
            }
        };
        o.prep.input.reference[i] = Some(k);
        o.ref_from[i] = Some(cf);
        o.ref_to[i] = Some(ct);
    }
    // the reference's transitions the map lacks after the map's, by name (as zeit.transitions orders them)
    let first_ref = pairs.len();
    if o.prep.input.classes.len() > first_ref + 1 {
        let mut order: Vec<usize> = (first_ref..o.prep.input.classes.len()).collect();
        order.sort_by(|&x, &y| o.prep.input.classes[x].cmp(&o.prep.input.classes[y]));
        let mut new_of = vec![0; o.prep.input.classes.len()];
        for (k, &old) in order.iter().enumerate() {
            new_of[old] = first_ref + k;
        }
        for k in 0..first_ref {
            new_of[k] = k;
        }
        let permute = |v: &mut Vec<String>| {
            let old = v.clone();
            for (k, &src) in order.iter().enumerate() {
                v[first_ref + k] = old[src].clone();
            }
        };
        permute(&mut o.prep.input.classes);
        let old_pairs = o.pairs.clone();
        for (k, &src) in order.iter().enumerate() {
            o.pairs[first_ref + k] = old_pairs[src];
        }
        for r in o.prep.input.reference.iter_mut().flatten() {
            *r = new_of[*r];
        }
    }
    // a point without a reference or off the maps does not count
    for i in 0..n {
        if o.prep.input.reference[i].is_none() || o.prep.input.map[i].is_none() {
            o.prep.input.stratum[i] = None;
        }
    }
    o
}

/// Both maps of the transitions are maps of classes.
pub fn can_transition(a: &MapRule, b: &MapRule) -> bool {
    a.kind == MapKind::Classes && b.kind == MapKind::Classes
}

/// The code of a class of a map (for Python: the reference as the map's values), else its name.
fn py_class(r: &MapRule, name: &str) -> String {
    for (code, n) in &r.names {
        if n == name {
            return code.to_string();
        }
    }
    match name.parse::<i64>() {
        Ok(code) if r.names.is_empty() || !r.names.values().any(|n| n == name) => code.to_string(),
        _ => py_str(name),
    }
}

/// Python (zeit.transitions) that gives the same estimates from the project's files.
pub fn transitions_snippet(p: &Project, t: &TransitionSpec, tr: &Transitions, confidence: f64) -> String {
    let (Some(mf), Some(mt)) = (p.maps.get(t.from), p.maps.get(t.to)) else { return String::new() };
    let file = Path::new(&p.path).file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned());
    let mut s = format!(
        "# The area of each transition from {} ({}) to {} ({}) (Olofsson et al. 2014; Stehman 2014),\n\
         # from the Janus validation project {file}\n",
        mf.rule.name, t.from_date, mt.rule.name, t.to_date
    );
    s += "import sqlite3\n\nimport geopandas as gpd\nimport pandas as pd\nimport rioxarray\nimport zeit\n\n";
    s += "# The sample points (a GeoPackage is SQLite), at their pixels' centres in the strata raster\n";
    s += &format!("with sqlite3.connect({}) as db:\n", py_path(&p.path));
    s += "    samples = pd.read_sql(\"SELECT sample_id, stratum, row, col FROM samples\", db)\n";
    s += &format!("strata = rioxarray.open_rasterio({}).squeeze(drop=True)\n", py_path(&p.strata_path));
    s += "x, y = strata.rio.transform() * (samples[\"col\"] + 0.5, samples[\"row\"] + 0.5)\n";
    s += "samples = gpd.GeoDataFrame(samples, geometry=gpd.points_from_xy(x, y), crs=strata.rio.crs)\n";
    s += "# The reference class of each point at each date: its labels and their dated events, as Janus reads them\n";
    for (var, refs, rule) in [("ref_from", &tr.ref_from, &mf.rule), ("ref_to", &tr.ref_to, &mt.rule)] {
        s += &format!("{var} = {{");
        let mut k = 0;
        for (i, pt) in p.points.iter().enumerate() {
            let Some(c) = refs.get(i).and_then(Option::as_ref) else { continue };
            if k > 0 {
                s += ", ";
            }
            k += 1;
            s += &format!("{}: {}", pt.id, py_class(rule, c));
            if k % 10 == 0 {
                s += "\n            ";
            }
        }
        s += "}\n";
    }
    s += "samples[\"ref_from\"] = samples[\"sample_id\"].map(ref_from)\n";
    s += "samples[\"ref_to\"] = samples[\"sample_id\"].map(ref_to)\n";
    s += "samples = samples[samples[\"ref_from\"].notna() & samples[\"ref_to\"].notna()]\n\n";
    for (var, r) in [("map_from", &mf.rule), ("map_to", &mt.rule)] {
        s += &format!(
            "{var} = rioxarray.open_rasterio({}, masked=True).sel(band={}, drop=True)\n",
            py_path(&r.path),
            r.band
        );
    }
    // (the maps' areas inside the strata: Zeit counts them as Janus does when the maps are on the strata's grid)
    s += &format!(
        "tr = zeit.transitions(map_from, map_to, samples, strata={}, estimator={}, confidence={})\n",
        py_path(&p.strata_path),
        py_str(p.estimator.name()),
        fmt_g(confidence, 6)
    );
    s += "print(tr)\n";
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::Event;

    #[test]
    fn dates() {
        assert_eq!(date_parts("2019-07-14"), Some([2019, 7, 14]));
        assert_eq!(date_parts("2019"), Some([2019, 0, 0]));
        assert_eq!(date_parts("14/07/2019"), Some([2019, 7, 14]));
        assert_eq!(date_parts("lc_2019.tif"), Some([2019, 0, 0]));
        assert_eq!(date_parts("no date"), None);
        assert_eq!(on_or_before("2019", "2019-01-01"), Some(true));
        assert_eq!(on_or_before("2019-06-01", "2019-05-31"), Some(false));
        assert_eq!(on_or_before("2018-12-31", "2019"), Some(true));
        assert_eq!(on_or_before("2020", "2019-12-31"), Some(false));
        assert_eq!(guess_date("D @ 2003-01-01"), "2003-01-01");
        assert_eq!(guess_date("lc_2007.tif"), "2007");
        assert_eq!(guess_date("map"), "");
    }

    #[test]
    fn the_class_at_a_date() {
        let classes = ["Forest", "Pasture", "Crop"];
        let counts = |l: &str| classes.contains(&l);
        let ev = |label: &str, date: &str| Event { label: label.into(), date: date.into() };
        // a forest cleared in 2018, cropped in 2021; a fire (no class) in between
        let l = Label {
            label: "Forest".into(),
            events: vec![ev("Pasture", "2018-07-01"), ev("Fire", "2019"), ev("Crop", "2021")],
            ..Default::default()
        };
        assert_eq!(label_at(&l, "2015-01-01", &counts), Some("Forest"));
        assert_eq!(label_at(&l, "2018-07-01", &counts), Some("Pasture"));
        assert_eq!(label_at(&l, "2020", &counts), Some("Pasture"));
        assert_eq!(label_at(&l, "2022-01-01", &counts), Some("Crop"));
        // a dated label alone: unknown before its date
        let l = Label { label: "Pasture".into(), date: "2018".into(), ..Default::default() };
        assert_eq!(label_at(&l, "2015", &counts), None);
        assert_eq!(label_at(&l, "2019", &counts), Some("Pasture"));
        // an undated event is the class before the dated ones
        let l = Label {
            label: "Pasture".into(),
            date: "2018".into(),
            events: vec![ev("Forest", "")],
            ..Default::default()
        };
        assert_eq!(label_at(&l, "2015", &counts), Some("Forest"));
    }
}
