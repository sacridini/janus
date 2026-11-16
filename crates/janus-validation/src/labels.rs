//! An interpreter's label of a point, and the reference that follows from everyone's (src/validation.cpp: Label,
//! resolve, nowIso, yearOf).

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::assess_map::CHANGE;
use crate::rules::str_of;

/// The "interpreter" whose labels settle disagreements.
pub const ADJUDICATED: &str = "(adjudicated)";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub label: String,
    pub date: String,
}

/// The "label" of a year that cannot be judged (cloud, no data): a value of its own, never part of the label set.
pub const YEAR_NA: &str = "n/a";

/// The label of one year, in the response design "one label per year" (Validation Viewer's second mode): the
/// class (or `YEAR_NA`) and the confidence of that year.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct YearLabel {
    pub label: String,
    /// 0 not given, 1 low, 2 medium, 3 high
    pub confidence: i32,
    /// UTC, ISO 8601
    pub updated: String,
}

/// How a point is answered: one label (and the date of the change), or one label for each year of a list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Response {
    #[default]
    PerPoint,
    PerYear,
}

impl Response {
    pub fn key(self) -> &'static str {
        match self {
            Response::PerPoint => "point",
            Response::PerYear => "year",
        }
    }

    /// Anything but "year" is one label per point (old projects have no key).
    pub fn from_key(s: &str) -> Response {
        if s.trim() == "year" { Response::PerYear } else { Response::PerPoint }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    /// class of the label set ("" = not labelled)
    pub label: String,
    /// date of the change, as the series shows it ("" = none)
    pub date: String,
    pub notes: String,
    /// 0 not given, 1 low, 2 medium, 3 high
    pub confidence: i32,
    /// more events at the point
    pub events: Vec<Event>,
    /// UTC, ISO 8601
    pub updated: String,
    /// one label per year (the design "one label per year"); then `label` and `date` hold the reference derived
    /// from them (`derive`), so what reads the plain label (older versions, the samples layer) still works
    pub years: BTreeMap<i32, YearLabel>,
}

impl Label {
    pub fn is_empty(&self) -> bool {
        self.years.is_empty()
            && self.label.is_empty()
            && self.date.is_empty()
            && self.notes.is_empty()
            && self.confidence == 0
            && self.events.is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "label": self.label,
            "date": self.date,
            "notes": self.notes,
            "confidence": self.confidence,
            "events": events_json(&self.events),
            "updated": self.updated,
            "years": self
                .years
                .iter()
                .map(|(y, l)| {
                    (y.to_string(), json!({"label": l.label, "confidence": l.confidence, "updated": l.updated}))
                })
                .collect::<serde_json::Map<String, Value>>(),
        })
    }

    pub fn from_json(j: &Value) -> Label {
        Label {
            label: str_of(j, "label"),
            date: str_of(j, "date"),
            notes: str_of(j, "notes"),
            confidence: j.get("confidence").and_then(Value::as_i64).unwrap_or(0) as i32,
            events: events_from_json(j.get("events").unwrap_or(&Value::Null)),
            updated: str_of(j, "updated"),
            years: j
                .get("years")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(y, l)| {
                    let y = y.trim().parse::<i32>().ok()?;
                    let l = YearLabel {
                        label: str_of(l, "label"),
                        confidence: l.get("confidence").and_then(Value::as_i64).unwrap_or(0) as i32,
                        updated: str_of(l, "updated"),
                    };
                    Some((y, l))
                })
                .collect(),
        }
    }

    /// Every year of `years` has a label (or "n/a").
    pub fn years_done(&self, years: &[i32]) -> bool {
        !years.is_empty() && years.iter().all(|y| self.years.get(y).is_some_and(|l| !l.label.is_empty()))
    }

    /// How many of `years` have a label.
    pub fn years_given(&self, years: &[i32]) -> usize {
        years.iter().filter(|y| self.years.get(y).is_some_and(|l| !l.label.is_empty())).count()
    }

    /// Sets `label` and `date` to the reference that follows from the years (`derive`).
    pub fn rederive(&mut self, years: &[i32], mapping: &BTreeMap<String, String>) {
        (self.label, self.date) = derive(self, years, mapping);
    }
}

/// The (label, date) a point answered year by year stands for, as the estimators read them: complete (every year
/// of `years` labelled, "n/a" included), else none. If some year has a label that `mapping` calls change, the
/// reference is that label at the first such year (the date is the year); else the label of the last year with a
/// class (no date). A point of "n/a" only has none.
pub fn derive(l: &Label, years: &[i32], mapping: &BTreeMap<String, String>) -> (String, String) {
    if !l.years_done(years) {
        return (String::new(), String::new());
    }
    let mut ys: Vec<i32> = years.to_vec();
    ys.sort_unstable();
    ys.dedup();
    let given = |y: &i32| l.years.get(y).filter(|x| x.label != YEAR_NA && !x.label.is_empty());
    let is_change = |y: &i32| given(y).is_some_and(|x| mapping.get(&x.label).map(String::as_str) == Some(CHANGE));
    if let Some(y) = ys.iter().find(|y| is_change(y)) {
        return (l.years[y].label.clone(), y.to_string());
    }
    ys.iter().rev().find_map(given).map_or_else(Default::default, |x| (x.label.clone(), String::new()))
}

/// The labels as the map whose translation is `mapping` reads them: those answered by year with their reference
/// derived with that translation, the rest as they are (borrowed when none is answered by year).
pub fn for_mapping<'a>(
    labels: &'a BTreeMap<String, Vec<Label>>,
    years: &[i32],
    mapping: &BTreeMap<String, String>,
) -> Cow<'a, BTreeMap<String, Vec<Label>>> {
    if !labels.values().any(|v| v.iter().any(|l| !l.years.is_empty())) {
        return Cow::Borrowed(labels);
    }
    let mut out = labels.clone();
    for l in out.values_mut().flatten().filter(|l| !l.years.is_empty()) {
        l.rederive(years, mapping);
    }
    Cow::Owned(out)
}

/// The translation used while labelling by year, before a map is assessed: the first map of events's, over the
/// guess from the names of the label set.
pub fn reference_mapping(maps: &[crate::project::AssessedMap], label_set: &[String]) -> BTreeMap<String, String> {
    use crate::rules::{MapKind, MapRule};
    let rule = MapRule { kind: MapKind::Events, ..Default::default() };
    let mut m = crate::assess_map::default_mapping(&rule, label_set, &[]);
    if let Some(a) = maps.iter().find(|a| a.rule.kind == MapKind::Events) {
        m.extend(a.mapping.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    m
}

/// The years of a text: "2018-2023", "2017, 2019, 2021-2024", "2000-2020/5" (every 5th from the first); sorted,
/// without repeats. Err with the reason when a piece is not a year or a range.
pub fn parse_years(text: &str) -> Result<Vec<i32>, String> {
    let year = |s: &str| -> Result<i32, String> {
        match s.trim().parse::<i32>() {
            Ok(y) if (1..=9999).contains(&y) => Ok(y),
            _ => Err(format!("\"{}\" is not a year", s.trim())),
        }
    };
    let mut out = Vec::new();
    for piece in text.split([',', ';', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        let (range, step) = match piece.split_once('/') {
            Some((r, s)) => match s.trim().parse::<i32>() {
                Ok(k) if k >= 1 => (r, k),
                _ => return Err(format!("\"{}\" is not a step", s.trim())),
            },
            None => (piece, 1),
        };
        match range.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (year(a)?, year(b)?);
                if b < a {
                    return Err(format!("{a}-{b} runs backwards"));
                }
                out.extend((a..=b).step_by(step as usize));
            }
            None => {
                if step != 1 {
                    return Err(format!("\"{piece}\": a step needs a range"));
                }
                out.push(year(range)?);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Years as text that `parse_years` reads back: runs of three or more consecutive years as "a-b".
pub fn years_text(years: &[i32]) -> String {
    let mut ys = years.to_vec();
    ys.sort_unstable();
    ys.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < ys.len() {
        let mut j = i;
        while j + 1 < ys.len() && ys[j + 1] == ys[j] + 1 {
            j += 1;
        }
        if j >= i + 2 {
            parts.push(format!("{}-{}", ys[i], ys[j]));
            i = j + 1;
        } else {
            parts.push(ys[i].to_string());
            i += 1;
        }
    }
    parts.join(", ")
}

pub(crate) fn events_json(ev: &[Event]) -> Value {
    Value::Array(ev.iter().map(|e| json!({"label": e.label, "date": e.date})).collect())
}

pub(crate) fn events_from_json(j: &Value) -> Vec<Event> {
    j.as_array().into_iter().flatten().map(|e| Event { label: str_of(e, "label"), date: str_of(e, "date") }).collect()
}

/// Events as stored in a text field ("" for none).
pub(crate) fn events_text(ev: &[Event]) -> String {
    if ev.is_empty() { String::new() } else { events_json(ev).to_string() }
}

pub(crate) fn events_from(s: &str) -> Vec<Event> {
    if s.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Value>(s).map(|j| events_from_json(&j)).unwrap_or_default()
}

/// Now, UTC, ISO 8601 ("2026-10-22T09:15:00Z").
pub fn now_iso() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // days to a civil date (Howard Hinnant's algorithm)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// The first run of exactly four digits, 0 if none.
pub fn year_of(date: &str) -> i32 {
    let b = date.as_bytes();
    let digit = |i: usize| b[i].is_ascii_digit();
    for i in 0..b.len().saturating_sub(3) {
        if (0..4).all(|k| digit(i + k)) && (i + 4 == b.len() || !digit(i + 4)) && (i == 0 || !digit(i - 1)) {
            return date[i..i + 4].parse().unwrap_or(0);
        }
    }
    0
}

/// The reference of point i: the adjudicated label, else the label of the only interpreter or of all when they
/// agree (the first by name); `conflict`: they disagree and nobody adjudicated. None: no label (or a conflict).
pub fn resolve(labels: &BTreeMap<String, Vec<Label>>, i: usize) -> (Option<&Label>, bool) {
    if let Some(l) = labels.get(ADJUDICATED).and_then(|v| v.get(i))
        && !l.label.is_empty()
    {
        return (Some(l), false);
    }
    let mut first: Option<&Label> = None;
    let mut conflict = false;
    for (who, v) in labels {
        if who == ADJUDICATED {
            continue;
        }
        let Some(l) = v.get(i).filter(|l| !l.label.is_empty()) else { continue };
        match first {
            None => first = Some(l),
            Some(f) if f.label != l.label => conflict = true,
            _ => {}
        }
    }
    (if conflict { None } else { first }, conflict)
}

/// What `mine` becomes when another copy's label `theirs` of the same point is merged: the newer label of the
/// point, and of each year the newer one (a year given in one copy only is kept). None: nothing to take.
pub fn merge_label(mine: &Label, theirs: &Label) -> Option<Label> {
    if theirs.is_empty() {
        return None;
    }
    let merged = if mine.is_empty() {
        theirs.clone()
    } else {
        let mut m = if theirs.updated > mine.updated { theirs.clone() } else { mine.clone() };
        let (a, b) = (&mine.years, &theirs.years);
        m.years = a.clone();
        for (y, x) in b {
            if a.get(y).is_none_or(|v| x.updated > v.updated) {
                m.years.insert(*y, x.clone());
            }
        }
        m
    };
    (merged != *mine).then_some(merged)
}

/// The reference of point i for one year, as `resolve` gives the point's: the adjudicated label of the year, else the
/// one of the only interpreter or of all when they agree. None: none, or a disagreement.
pub fn resolve_year(labels: &BTreeMap<String, Vec<Label>>, i: usize, year: i32) -> Option<&YearLabel> {
    let of = |who: &String| labels.get(who)?.get(i)?.years.get(&year).filter(|x| !x.label.is_empty());
    if let Some(x) = of(&ADJUDICATED.to_string()) {
        return Some(x);
    }
    let mut first: Option<&YearLabel> = None;
    for who in labels.keys().filter(|w| *w != ADJUDICATED) {
        let Some(x) = of(who) else { continue };
        match first {
            None => first = Some(x),
            Some(f) if f.label != x.label => return None,
            _ => {}
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn years_and_time() {
        assert_eq!(year_of("2019-07-14"), 2019);
        assert_eq!(year_of("2005"), 2005);
        assert_eq!(year_of("12345"), 0);
        assert_eq!(year_of(""), 0);
        assert_eq!(year_of("14/07/2019"), 2019);
        let t = now_iso();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }

    fn by_year(v: &[(i32, &str)]) -> Label {
        let year = |l: &str| YearLabel { label: l.to_string(), confidence: 2, updated: "2026-01-01T00:00:00Z".into() };
        Label { years: v.iter().map(|&(y, l)| (y, year(l))).collect(), ..Default::default() }
    }

    #[test]
    fn year_lists() {
        assert_eq!(parse_years("2018-2023").unwrap(), vec![2018, 2019, 2020, 2021, 2022, 2023]);
        assert_eq!(parse_years("2017, 2019, 2021-2024").unwrap(), vec![2017, 2019, 2021, 2022, 2023, 2024]);
        assert_eq!(parse_years("2000-2020/5").unwrap(), vec![2000, 2005, 2010, 2015, 2020]);
        assert_eq!(parse_years("2020 2018 2020").unwrap(), vec![2018, 2020]);
        assert_eq!(parse_years("").unwrap(), Vec::<i32>::new());
        for bad in ["abc", "2020-2018", "2000-2020/0", "2020/5", "2018-", "0", "-5"] {
            assert!(parse_years(bad).is_err(), "{bad}");
        }
        assert_eq!(years_text(&[2018, 2019, 2020, 2021, 2023]), "2018-2021, 2023");
        assert_eq!(years_text(&[2005, 2015, 2019]), "2005, 2015, 2019");
        assert_eq!(parse_years(&years_text(&[2018, 2019, 2020, 2022])).unwrap(), vec![2018, 2019, 2020, 2022]);
    }

    #[test]
    fn reference_of_years() {
        let map: BTreeMap<String, String> =
            [("Forest", "no change"), ("Clear", "change"), ("Fire", "change"), ("Cloud", "")]
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .into();
        let ys = [2018, 2019, 2020, 2021];
        // a change in the middle: its label at its first year, whatever follows
        let l = by_year(&[(2018, "Forest"), (2019, "Forest"), (2020, "Clear"), (2021, "Fire")]);
        assert_eq!(derive(&l, &ys, &map), ("Clear".to_string(), "2020".to_string()));
        // no change: the last class, no date; n/a years are skipped
        let l2 = by_year(&[(2018, "Forest"), (2019, YEAR_NA), (2020, "Forest"), (2021, YEAR_NA)]);
        assert_eq!(derive(&l2, &ys, &map), ("Forest".to_string(), String::new()));
        // incomplete: no reference, even with a change
        let l3 = by_year(&[(2018, "Forest"), (2019, "Clear")]);
        assert_eq!(derive(&l3, &ys, &map), (String::new(), String::new()));
        assert!(!l3.years_done(&ys) && l3.years_given(&ys) == 2);
        // only n/a: done, but nothing to refer to
        let l4 = by_year(&[(2018, YEAR_NA), (2019, YEAR_NA), (2020, YEAR_NA), (2021, YEAR_NA)]);
        assert!(l4.years_done(&ys));
        assert_eq!(derive(&l4, &ys, &map), (String::new(), String::new()));
        // the same years, another translation: Forest as change
        let other: BTreeMap<String, String> = [("Forest".to_string(), CHANGE.to_string())].into();
        let mut all = BTreeMap::new();
        all.insert("ana".to_string(), vec![l.clone()]);
        let v = for_mapping(&all, &ys, &other);
        assert_eq!(v["ana"][0].label, "Forest");
        assert_eq!(v["ana"][0].date, "2018");
        // nothing by year: borrowed
        let mut plain = BTreeMap::new();
        plain.insert("ana".to_string(), vec![Label { label: "X".into(), ..Default::default() }]);
        assert!(matches!(for_mapping(&plain, &ys, &other), Cow::Borrowed(_)));
        // one year across interpreters: agreement, disagreement, adjudication
        let mut two = BTreeMap::new();
        two.insert("ana".to_string(), vec![by_year(&[(2018, "Forest"), (2019, "Clear")])]);
        two.insert("bruno".to_string(), vec![by_year(&[(2018, "Forest"), (2019, "Fire")])]);
        assert_eq!(resolve_year(&two, 0, 2018).map(|x| x.label.as_str()), Some("Forest"));
        assert!(resolve_year(&two, 0, 2019).is_none());
        assert!(resolve_year(&two, 0, 2020).is_none());
        two.insert(ADJUDICATED.to_string(), vec![by_year(&[(2019, "Clear")])]);
        assert_eq!(resolve_year(&two, 0, 2019).map(|x| x.label.as_str()), Some("Clear"));
        // merging copies: the newer year of each, the newer point's own fields
        let year = |l: &str, u: &str| YearLabel { label: l.into(), confidence: 0, updated: u.into() };
        let mut a = by_year(&[]);
        a.updated = "2026-01-01".into();
        a.years = [(2018, year("Forest", "2026-01-01")), (2019, year("Clear", "2026-01-05"))].into();
        let mut b = by_year(&[]);
        b.updated = "2026-02-01".into();
        b.notes = "later".into();
        b.years = [(2019, year("Fire", "2026-01-02")), (2020, year("Fire", "2026-02-01"))].into();
        let m = merge_label(&a, &b).unwrap();
        assert_eq!(m.notes, "later");
        assert_eq!(m.years.len(), 3);
        assert_eq!(m.years[&2019].label, "Clear");
        assert_eq!(m.years[&2020].label, "Fire");
        assert!(merge_label(&m, &b).is_none());
        assert!(merge_label(&a, &Label::default()).is_none());
        assert_eq!(merge_label(&Label::default(), &b).unwrap(), b);
        // json round trip
        assert_eq!(Label::from_json(&l.to_json()), l);
        assert!(!l.is_empty());
    }
}
