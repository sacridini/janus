//! An interpreter's label of a point, and the reference that follows from everyone's (src/validation.cpp: Label,
//! resolve, nowIso, yearOf).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::rules::str_of;

/// The "interpreter" whose labels settle disagreements.
pub const ADJUDICATED: &str = "(adjudicated)";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub label: String,
    pub date: String,
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
}

impl Label {
    pub fn is_empty(&self) -> bool {
        self.label.is_empty()
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
        }
    }
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
}
