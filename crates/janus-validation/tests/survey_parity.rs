//! The direct, difference and ratio estimators of the areas of classes and of transitions between two dates against
//! R's survey package (tests/data/make_survey_fixtures.R, the same fixture as Zeit's tests/test_accuracy_survey.py):
//! a sample stratified by another map than the one assessed, to 1e-10; through `assess` on the points' classes, and
//! through `prepare_transitions` on a project whose labels give the class at each date (an event dated between the
//! two dates where it changed).

use std::collections::BTreeMap;

use janus_validation::{
    AreaEstimator, AssessInput, AssessedMap, Event, Label, MapKind, MapRule, Point, Project, Strata, Stratum,
    TransitionSpec, assess, prepare_transitions,
};
use serde_json::Value;

fn fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/accuracy_survey.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).expect("JSON")
}

fn ints(v: &Value) -> Vec<i32> {
    v.as_array().unwrap().iter().map(|x| x.as_i64().unwrap() as i32).collect()
}

/// R's (estimate, se), None for NA.
fn pair(v: &Value) -> Option<(f64, f64)> {
    Some((v[0].as_f64()?, v[1].as_f64()?))
}

fn close(ours: f64, theirs: Option<f64>, what: &str) {
    match theirs {
        None => assert!(ours.is_nan(), "{what}: {ours}, R NA"),
        Some(t) => assert!((ours - t).abs() <= 1e-9 + 1e-10 * t.abs(), "{what}: {ours} vs R {t}"),
    }
}

struct Population {
    names: Vec<String>,
    pixel: f64,
    strata: Vec<i32>,
    map_from: Vec<i32>,
    map_to: Vec<i32>,
}

impl Population {
    fn new(j: &Value) -> Population {
        Population {
            names: j["names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect(),
            pixel: j["pixel_area"].as_f64().unwrap(),
            strata: ints(&j["strata"]),
            map_from: ints(&j["map_from"]),
            map_to: ints(&j["map_to"]),
        }
    }

    fn name(&self, c: i32) -> String {
        self.names[c as usize - 1].clone()
    }

    /// The area of each value of `codes` (in order of the values).
    fn areas(&self, codes: &[i32]) -> BTreeMap<i32, f64> {
        let mut a = BTreeMap::new();
        for &c in codes {
            *a.entry(c).or_insert(0.0) += self.pixel;
        }
        a
    }
}

/// The estimates of `classes` (names) from each point's stratum, map and reference class names.
fn check(
    estimator: AreaEstimator,
    strata_codes: &[i32],
    stratum_area: &BTreeMap<i32, f64>,
    map: &[String],
    reference: &[String],
    mapped: &BTreeMap<String, f64>,
    want: &Value,
) {
    let classes: Vec<String> = want["classes"].as_object().unwrap().keys().cloned().collect();
    let idx = |n: &String| classes.iter().position(|c| c == n);
    let order: Vec<i32> = stratum_area.keys().copied().collect();
    let inp = AssessInput {
        stratum_area: stratum_area.values().copied().collect(),
        stratum: strata_codes.iter().map(|h| order.iter().position(|o| o == h)).collect(),
        map: map.iter().map(idx).collect(),
        reference: reference.iter().map(idx).collect(),
        classes: classes.clone(),
        mapped: classes.iter().map(|c| mapped.get(c).copied().unwrap_or(0.0)).collect(),
        confidence: 0.95,
        estimator,
    };
    let a = assess(&inp);
    assert_eq!(a.n as usize, map.len());
    for (c, name) in classes.iter().enumerate() {
        let r = &want["classes"][name];
        close(a.area[c].mapped, r["mapped"].as_f64(), &format!("{name}: mapped"));
        let e = pair(&r[estimator.name()]);
        close(a.area[c].est, e.map(|p| p.0), &format!("{name}: {} area", estimator.name()));
        close(a.area[c].se, e.map(|p| p.1), &format!("{name}: {} se", estimator.name()));
        close(a.users[c].est, pair(&r["users"]).map(|p| p.0), &format!("{name}: users"));
        close(a.users[c].se, pair(&r["users"]).map(|p| p.1), &format!("{name}: users se"));
        close(a.producers[c].est, pair(&r["producers"]).map(|p| p.0), &format!("{name}: producers"));
        close(a.producers[c].se, pair(&r["producers"]).map(|p| p.1), &format!("{name}: producers se"));
    }
    close(a.overall.est, want["overall"][0].as_f64(), "overall");
    close(a.overall.se, want["overall"][1].as_f64(), "overall se");
}

fn points(j: &Value, key: &str) -> (Vec<i32>, [Vec<i32>; 4]) {
    let t = &j[key];
    (ints(&t["stratum"]), [ints(&t["map_from"]), ints(&t["map_to"]), ints(&t["ref_from"]), ints(&t["ref_to"])])
}

#[test]
fn transitions_classes_and_strata_as_r_survey() {
    let j = fixture();
    let pop = Population::new(&j);
    let tr = |a: i32, b: i32| format!("{} -> {}", pop.name(a), pop.name(b));
    let strata_area = pop.areas(&pop.strata);
    let (h, [mf, mt, rf, rt]) = points(&j, "points");
    let mut mapped_tr = BTreeMap::new();
    for (&a, &b) in pop.map_from.iter().zip(&pop.map_to) {
        *mapped_tr.entry(tr(a, b)).or_insert(0.0) += pop.pixel;
    }
    let mapped_to: BTreeMap<String, f64> = pop.areas(&pop.map_to).into_iter().map(|(c, a)| (pop.name(c), a)).collect();
    let map_tr: Vec<String> = mf.iter().zip(&mt).map(|(&a, &b)| tr(a, b)).collect();
    let ref_tr: Vec<String> = rf.iter().zip(&rt).map(|(&a, &b)| tr(a, b)).collect();
    let map_c: Vec<String> = mt.iter().map(|&c| pop.name(c)).collect();
    let ref_c: Vec<String> = rt.iter().map(|&c| pop.name(c)).collect();
    // a sample stratified by the second map's classes: the three estimators agree
    let (h2, [_, mt2, _, rt2]) = points(&j, "points_by_map");
    let by_map_area = pop.areas(&pop.map_to);
    let map2: Vec<String> = mt2.iter().map(|&c| pop.name(c)).collect();
    let ref2: Vec<String> = rt2.iter().map(|&c| pop.name(c)).collect();
    for e in AreaEstimator::ALL {
        check(e, &h, &strata_area, &map_tr, &ref_tr, &mapped_tr, &j["transitions"]);
        check(e, &h, &strata_area, &map_c, &ref_c, &mapped_to, &j["classes"]);
        check(e, &h2, &by_map_area, &map2, &ref2, &mapped_to, &j["by_map"]);
    }
}

#[test]
fn transitions_from_a_projects_labels_as_r_survey() {
    let j = fixture();
    let pop = Population::new(&j);
    let (h, [mf, mt, rf, rt]) = points(&j, "points");
    let mut p = Project {
        strata: Strata {
            strata: pop
                .areas(&pop.strata)
                .into_iter()
                .map(|(code, area)| Stratum { code, name: code.to_string(), area, ..Default::default() })
                .collect(),
            ..Default::default()
        },
        points: h
            .iter()
            .enumerate()
            .map(|(i, &s)| Point { id: i as i32 + 1, stratum: s, ..Default::default() })
            .collect(),
        ..Default::default()
    };
    // the class at the first date; where it changed, an event at a date between the two (another, before the
    // first date, that changes nothing); a class name in another case translated
    let labels: Vec<Label> = (0..h.len())
        .map(|i| {
            let mut l = Label { label: pop.name(rf[i]), ..Default::default() };
            if i % 7 == 0 {
                l.events.push(Event { label: pop.name(rf[i]), date: "2001-03-01".into() });
            }
            if rt[i] != rf[i] {
                l.events.push(Event { label: pop.name(rt[i]), date: "2009-08-15".into() });
            }
            if i % 11 == 0 {
                l.events.push(Event { label: "Fire".into(), date: "2012".into() });
            }
            l
        })
        .collect();
    p.labels.insert("ana".into(), labels);
    let rule = |band| MapRule {
        path: "lc.tif".into(),
        band,
        kind: MapKind::Classes,
        names: (1..=3).map(|c| (c, pop.name(c))).collect(),
        ..Default::default()
    };
    p.maps = vec![
        AssessedMap { rule: rule(1), mapping: BTreeMap::new() },
        AssessedMap { rule: rule(2), mapping: [("Fire".to_string(), String::new())].into() },
    ];
    let spec = TransitionSpec { from: 0, to: 1, from_date: "2005-01-01".into(), to_date: "2015-01-01".into() };
    let mut mapped = BTreeMap::new();
    for (&a, &b) in pop.map_from.iter().zip(&pop.map_to) {
        *mapped.entry((a, b)).or_insert(0.0) += pop.pixel;
    }
    let (fv, tv): (Vec<f64>, Vec<f64>) =
        (mf.iter().map(|&v| f64::from(v)).collect(), mt.iter().map(|&v| f64::from(v)).collect());
    for e in AreaEstimator::ALL {
        p.estimator = e;
        let t = prepare_transitions(&p, &spec, &fv, &tv, &mapped, 0.95, &[]);
        assert_eq!((t.prep.labelled as usize, t.prep.no_date, t.prep.unmapped, t.prep.excluded), (h.len(), 0, 0, 0));
        let a = assess(&t.prep.input);
        assert_eq!(a.n as usize, h.len());
        assert_eq!(a.estimator, e);
        let want = &j["transitions"]["classes"];
        assert_eq!(a.classes.len(), want.as_object().unwrap().len());
        for (c, name) in a.classes.iter().enumerate() {
            let r = &want[name];
            assert!(r.is_object(), "{name} is not one of R's transitions");
            let (a1, b1) = t.pairs[c];
            assert_eq!(name, &format!("{} -> {}", pop.name(a1), pop.name(b1)));
            close(a.area[c].mapped, r["mapped"].as_f64(), &format!("{name}: mapped"));
            let v = pair(&r[e.name()]);
            close(a.area[c].est, v.map(|p| p.0), &format!("{name}: {} area", e.name()));
            close(a.area[c].se, v.map(|p| p.1), &format!("{name}: {} se", e.name()));
        }
        // the difference estimates add up to the total area, as the direct ones
        if e != AreaEstimator::Ratio {
            let sum: f64 = a.area.iter().map(|x| x.est).sum();
            assert!((sum - a.total_area).abs() < 1e-9 * a.total_area);
        }
        // the reference of each point at each date, for the Python
        assert!(t.ref_from.iter().zip(&rf).all(|(x, &c)| x.as_deref() == Some(pop.name(c).as_str())));
        assert!(t.ref_to.iter().zip(&rt).all(|(x, &c)| x.as_deref() == Some(pop.name(c).as_str())));
    }
    // dates out of order of the events: before the clearing, both dates give the first class
    let early = TransitionSpec { from_date: "2003".into(), to_date: "2008-12-31".into(), ..spec.clone() };
    let t = prepare_transitions(&p, &early, &fv, &tv, &mapped, 0.95, &[]);
    assert!(t.ref_from.iter().zip(&t.ref_to).all(|(a, b)| a == b));
    // a label left out ("Can't tell") and one the maps have no class for
    let ana = p.labels.get_mut("ana").unwrap();
    ana[0] = Label { label: "Fire".into(), ..Default::default() };
    ana[1] = Label { label: "Savanna".into(), ..Default::default() };
    let t = prepare_transitions(&p, &spec, &fv, &tv, &mapped, 0.95, &[]);
    assert_eq!((t.prep.excluded, t.prep.unmapped), (1, 1));
}
