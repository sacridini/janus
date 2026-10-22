//! numpy's generators (janus_zeit::rng) against numpy itself (tests/data/numpy_rng.json).

use janus_zeit::rng::{Mt19937, Pcg64};
use serde_json::Value;

fn data() -> Value {
    let path = format!("{}/tests/data/numpy_rng.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn u64s(v: &Value) -> Vec<u64> {
    v.as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect()
}

#[test]
fn random_state_as_numpy() {
    for c in data()["mt"].as_array().unwrap() {
        let mut r = Mt19937::new(c["seed"].as_u64().unwrap() as u32);
        let u32s: Vec<u64> = (0..5).map(|_| r.randint(1 << 32)).collect();
        assert_eq!(u32s, u64s(&c["u32"]));
        let d: Vec<f64> = (0..7).map(|_| r.next_double()).collect();
        let want: Vec<f64> = c["rand"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert_eq!(d, want);
        let ri: Vec<u64> = (0..20).map(|_| r.randint(37)).collect();
        assert_eq!(ri, u64s(&c["randint37"]));
        let ri: Vec<u64> = (0..5).map(|_| r.randint(70001)).collect();
        assert_eq!(ri, u64s(&c["randint70001"]));
    }
}

#[test]
fn generator_choice_as_numpy() {
    for c in data()["choice"].as_array().unwrap() {
        let mut r = Pcg64::new(c["seed"].as_u64().unwrap());
        let got: Vec<u64> = r
            .choice(c["pop"].as_u64().unwrap() as usize, c["size"].as_u64().unwrap() as usize)
            .into_iter()
            .map(|v| v as u64)
            .collect();
        assert_eq!(got, u64s(&c["idx"]), "seed {} pop {} size {}", c["seed"], c["pop"], c["size"]);
    }
}
