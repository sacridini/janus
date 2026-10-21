//! Band roles (src/app_zeit.cpp, guessBandRoles): which band of a date is blue, green, red, NIR, SWIR1, SWIR2 or
//! thermal, from the band names (RGB composites, the multiband tools).

use std::collections::BTreeMap;

use crate::CubeInfo;

/// The roles in the order of a surface reflectance stack.
pub const ROLES: [&str; 7] = ["blue", "green", "red", "nir", "swir1", "swir2", "thermal"];

/// Role -> band (1..=bands_per_date).
pub type BandRoles = BTreeMap<&'static str, i32>;

/// The roles found in the band names: keywords, most specific first; a band takes the first role it matches (red
/// edge, QA and Fmask bands none). Unnamed stacks of 6 or more bands are taken as Blue..SWIR2 in order.
pub fn guess_band_roles(info: &CubeInfo) -> BandRoles {
    const KEYS: [(&str, &[&str]); 7] = [
        ("swir1", &["swir1", "swir16", "swir161"]),
        ("swir2", &["swir2", "swir22", "swir220"]),
        ("thermal", &["thermal", "lwir", "tir", "stb10", "brightness"]),
        ("nir", &["nir"]),
        ("blue", &["blue"]),
        ("green", &["green"]),
        ("red", &["red"]),
    ];
    let mut r = BandRoles::new();
    let mut generic = true;
    for (i, name) in info.band_names.iter().enumerate() {
        let n: String = name.to_lowercase().chars().filter(|c| !matches!(c, ' ' | '_' | '-')).collect();
        if !n.starts_with("band") {
            generic = false;
        }
        if n.contains("rededge") || n.contains("qa") || n.contains("fmask") {
            continue;
        }
        for (role, words) in KEYS {
            if r.contains_key(role) {
                continue;
            }
            if words.iter().any(|w| n.contains(w)) {
                r.insert(role, i as i32 + 1);
                break;
            }
        }
    }
    if r.is_empty() && generic && info.bands_per_date >= 6 {
        for (k, role) in ROLES.iter().take(6).enumerate() {
            r.insert(role, k as i32 + 1);
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(names: &[&str]) -> CubeInfo {
        CubeInfo {
            band_names: names.iter().map(|s| s.to_string()).collect(),
            bands_per_date: names.len(),
            ..Default::default()
        }
    }

    #[test]
    fn landsat_and_sentinel() {
        let r = guess_band_roles(&named(&["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR_2", "QA_PIXEL"]));
        assert_eq!(r["blue"], 1);
        assert_eq!(r["swir1"], 5);
        assert_eq!(r["swir2"], 6);
        assert!(!r.contains_key("thermal"));
        // red edge bands are not red
        let r = guess_band_roles(&named(&["B2 blue", "B3 green", "B4 red", "B5 red-edge", "B8 nir"]));
        assert_eq!((r["red"], r["nir"]), (3, 5));
        // unnamed stacks
        let r = guess_band_roles(&named(&["Band 1", "Band 2", "Band 3", "Band 4", "Band 5", "Band 6"]));
        assert_eq!((r["blue"], r["swir2"]), (1, 6));
        assert!(guess_band_roles(&named(&["Band 1", "Band 2", "Band 3"])).is_empty());
    }
}
