//! The calibration split: the points that tune a tool's parameters (Tune on samples) must not estimate the
//! accuracy too, or the accuracy would come out optimistic (the parameters were chosen to fit those very points).
//! With the same project for both, a seeded random draw inside each stratum marks the points that calibrate; the
//! accuracy uses the rest with the same stratum weights (the areas), which is still a stratified random sample, so
//! the estimators stay unbiased, with larger errors.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::design::Point;
use crate::project::Project;

/// A split kept in the project: the points (by id) that calibrate, and how they were drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrationSplit {
    /// the share of each stratum that calibrates
    pub fraction: f64,
    pub seed: u64,
    /// the ids of the points that calibrate, sorted
    pub ids: Vec<u32>,
}

impl CalibrationSplit {
    /// The split of `points`: `calibration_split`'s draw, kept by id.
    pub fn draw(points: &[Point], fraction: f64, seed: u64) -> CalibrationSplit {
        let mask = calibration_split(points, fraction, seed);
        let mut ids: Vec<u32> =
            points.iter().zip(&mask).filter(|(_, c)| **c).map(|(p, _)| p.id.max(0) as u32).collect();
        ids.sort_unstable();
        CalibrationSplit { fraction, seed, ids }
    }

    /// Whether the point with this id calibrates.
    pub fn has(&self, id: i32) -> bool {
        id >= 0 && self.ids.binary_search(&(id as u32)).is_ok()
    }

    /// One flag per point: true where it calibrates.
    pub fn mask(&self, points: &[Point]) -> Vec<bool> {
        points.iter().map(|p| self.has(p.id)).collect()
    }

    pub fn to_json(&self) -> Value {
        json!({"fraction": self.fraction, "seed": self.seed.to_string(), "ids": self.ids})
    }

    /// None when `j` is not a split (a project from before the split has none).
    pub fn from_json(j: &Value) -> Option<CalibrationSplit> {
        let fraction = j.get("fraction")?.as_f64()?;
        // the seed is kept as text: a u64 does not survive a JSON number
        let seed = match j.get("seed")? {
            Value::String(s) => s.trim().parse().ok()?,
            v => v.as_u64()?,
        };
        let mut ids: Vec<u32> = j.get("ids")?.as_array()?.iter().filter_map(|v| v.as_u64().map(|x| x as u32)).collect();
        ids.sort_unstable();
        ids.dedup();
        Some(CalibrationSplit { fraction, seed, ids })
    }
}

/// SplitMix64 (Steele, Lea and Flood 2014): a small seeded generator, the same on every platform.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..n (n > 0), by rejection so that no value is likelier.
    fn below(&mut self, n: u64) -> u64 {
        let zone = u64::MAX - u64::MAX % n;
        loop {
            let x = self.next_u64();
            if x < zone {
                return x % n;
            }
        }
    }
}

/// Which points calibrate: a seeded random draw inside each stratum of round(fraction x n_h) of its points, at
/// least one point of every stratum with two or more left for the accuracy (a stratum of one point keeps it for
/// the accuracy). The draw of a stratum depends on the seed, the stratum's code and the ids of its points, not on
/// the order of the points or on the other strata, so it is reproducible. `fraction` is held to 0..1.
pub fn calibration_split(points: &[Point], fraction: f64, seed: u64) -> Vec<bool> {
    let fraction = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
    let mut by_stratum: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, p) in points.iter().enumerate() {
        by_stratum.entry(p.stratum).or_default().push(i);
    }
    let mut out = vec![false; points.len()];
    for (code, mut members) in by_stratum {
        let n = members.len();
        if n < 2 {
            continue;
        }
        members.sort_by_key(|&i| (points[i].id, i));
        let k = ((fraction * n as f64).round_ties_even() as usize).min(n - 1);
        let mut rng = SplitMix64(seed ^ SplitMix64(u64::from(code as u32) ^ 0xA5A5_5A5A_1234_5678).next_u64());
        // a partial Fisher-Yates: the first k of the shuffled members
        for j in 0..k {
            let r = j + rng.below((n - j) as u64) as usize;
            members.swap(j, r);
            out[members[j]] = true;
        }
    }
    out
}

/// The points that calibrate, as indices into the project's points. A project with no split refuses: the points
/// of the accuracy assessment cannot tune the parameters too.
pub fn calibration_points(p: &Project) -> Result<Vec<usize>, String> {
    let Some(c) = &p.calibration else {
        return Err("the accuracy points cannot calibrate: make a calibration split first, or use another set of \
                    points"
            .into());
    };
    Ok(p.points.iter().enumerate().filter(|(_, q)| c.has(q.id)).map(|(i, _)| i).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `counts[h]` points in stratum code h + 1, ids from 1.
    fn points(counts: &[usize]) -> Vec<Point> {
        let mut v = Vec::new();
        for (h, &n) in counts.iter().enumerate() {
            for _ in 0..n {
                v.push(Point { id: v.len() as i32 + 1, stratum: h as i32 + 1, ..Default::default() });
            }
        }
        v
    }

    fn per_stratum(pts: &[Point], mask: &[bool]) -> BTreeMap<i32, (usize, usize)> {
        let mut m: BTreeMap<i32, (usize, usize)> = BTreeMap::new();
        for (p, &c) in pts.iter().zip(mask) {
            let e = m.entry(p.stratum).or_default();
            e.0 += 1;
            e.1 += usize::from(c);
        }
        m
    }

    #[test]
    fn split_is_per_stratum_and_reproducible() {
        let pts = points(&[100, 40, 10, 2, 1]);
        let a = calibration_split(&pts, 0.3, 7);
        assert_eq!(a, calibration_split(&pts, 0.3, 7));
        assert_ne!(a, calibration_split(&pts, 0.3, 8));
        let m = per_stratum(&pts, &a);
        assert_eq!(m[&1], (100, 30));
        assert_eq!(m[&2], (40, 12));
        assert_eq!(m[&3], (10, 3));
        // two points: round(0.6) = 1 calibrates, 1 stays; one point: stays for the accuracy
        assert_eq!(m[&4], (2, 1));
        assert_eq!(m[&5], (1, 0));
        // even at fraction 1 one point of each stratum with two or more is left for the accuracy
        let all = calibration_split(&pts, 1.0, 7);
        let m = per_stratum(&pts, &all);
        assert!(m.values().all(|&(n, c)| if n >= 2 { c == n - 1 } else { c == 0 }));
        // the order of the points does not change who is drawn (by id)
        let mut rev = pts.clone();
        rev.reverse();
        let b = calibration_split(&rev, 0.3, 7);
        for (p, &c) in rev.iter().zip(&b) {
            assert_eq!(c, a[(p.id - 1) as usize]);
        }
    }

    #[test]
    fn split_json_round_trip_and_old_projects() {
        let pts = points(&[30, 30]);
        let c = CalibrationSplit::draw(&pts, 0.3, u64::MAX - 5);
        assert_eq!(c.ids.len(), 18);
        assert_eq!(CalibrationSplit::from_json(&c.to_json()), Some(c.clone()));
        assert_eq!(c.mask(&pts), calibration_split(&pts, 0.3, u64::MAX - 5));
        // a project's analysis from before the split has no key
        let old = json!({"estimator": "direct"});
        assert_eq!(old.get("calibration").and_then(CalibrationSplit::from_json), None);
        assert_eq!(CalibrationSplit::from_json(&Value::Null), None);
    }

    #[test]
    fn calibration_points_stay_out_of_the_assessment() {
        use crate::assess_map::prepare;
        use crate::labels::Label;
        use crate::project::AssessedMap;
        use crate::rules::MapRule;
        use crate::strata::Stratum;

        // two strata of 40 points each, areas 3:1; the map says the stratum's class, the reference agrees
        let pts = points(&[40, 40]);
        let mut p = Project { points: pts.clone(), ..Default::default() };
        p.strata.strata = vec![
            Stratum { code: 1, name: "Stable".into(), cls: 1, area: 300.0, weight: 0.75, ..Default::default() },
            Stratum { code: 2, name: "Change".into(), cls: 2, area: 100.0, weight: 0.25, ..Default::default() },
        ];
        let rule = MapRule {
            names: [(1, "Stable".to_string()), (2, "Change".to_string())].into_iter().collect(),
            ..Default::default()
        };
        let label_set = vec!["Stable".to_string(), "Change".to_string()];
        let m = AssessedMap {
            rule: rule.clone(),
            mapping: crate::assess_map::default_mapping(&rule, &label_set, &["Stable".into(), "Change".into()]),
        };
        let values: Vec<f64> = pts.iter().map(|q| f64::from(q.stratum)).collect();
        p.labels.insert(
            "a".into(),
            pts.iter()
                .map(|q| Label { label: label_set[(q.stratum - 1) as usize].clone(), ..Default::default() })
                .collect(),
        );
        let mapped = [(1, 300.0), (2, 100.0)].into_iter().collect();
        let before = prepare(&p, &m, &values, &mapped, 0.95, &[]);
        assert_eq!((before.calibration, before.labelled), (0, 80));
        p.calibration = Some(CalibrationSplit::draw(&p.points, 0.3, 11));
        let after = prepare(&p, &m, &values, &mapped, 0.95, &[]);
        // 30% of each stratum: 12 + 12
        assert_eq!((after.calibration, after.labelled), (24, 56));
        assert_eq!(after.input.stratum_area, before.input.stratum_area);
        let c = p.calibration.as_ref().unwrap();
        for (q, s) in p.points.iter().zip(&after.input.stratum) {
            assert_eq!(s.is_none(), c.has(q.id), "point {}", q.id);
        }
        // the estimators run on the rest with the same weights
        let a = crate::estimate::assess(&after.input);
        assert_eq!(a.n, 56);
    }

    #[test]
    fn accuracy_points_cannot_calibrate() {
        let mut p = Project { points: points(&[20, 20]), ..Default::default() };
        let e = calibration_points(&p).unwrap_err();
        assert!(e.contains("cannot calibrate"), "{e}");
        let c = CalibrationSplit::draw(&p.points, 0.25, 3);
        p.calibration = Some(c);
        let idx = calibration_points(&p).unwrap();
        assert_eq!(idx.len(), 10);
        assert!(idx.iter().all(|&i| p.calibration.as_ref().unwrap().has(p.points[i].id)));
    }
}
