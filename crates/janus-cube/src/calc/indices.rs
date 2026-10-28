//! The Awesome Spectral Indices (Montero et al. 2023, MIT; spectral-indices.json, made by
//! tools/spectral_indices.py): each index's formula over band roles (N, R, S1...), ready for a layer once its
//! constants take their defaults and its wavelengths (lambdaN...) the layer's.

use std::sync::LazyLock;

use super::lex::{Tok, tokens};
use super::names::{BandNames, role};

pub struct SpectralIndex {
    pub name: String,
    pub long_name: String,
    /// vegetation, water, burn, urban, soil, radar, snow, clouds
    pub domain: String,
    /// as the catalogue writes it (Python)
    pub formula: String,
    /// the roles it needs (its constants left out)
    pub roles: Vec<String>,
    pub reference: String,
}

struct Catalogue {
    indices: Vec<SpectralIndex>,
    constants: Vec<(String, Option<f64>)>,
}

static CATALOGUE: LazyLock<Catalogue> = LazyLock::new(|| {
    let v: serde_json::Value = serde_json::from_str(include_str!("spectral-indices.json")).expect("the indices");
    let constants: Vec<(String, Option<f64>)> = v["constants"]
        .as_object()
        .map(|m| m.iter().map(|(k, x)| (k.clone(), x.as_f64())).collect())
        .unwrap_or_default();
    let s = |x: &serde_json::Value| x.as_str().unwrap_or_default().to_string();
    let indices = v["indices"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| SpectralIndex {
                    name: s(&i["n"]),
                    long_name: s(&i["l"]),
                    domain: s(&i["d"]),
                    formula: s(&i["f"]),
                    roles: i["b"]
                        .as_array()
                        .map(|b| b.iter().map(s).filter(|r| role(r).is_some()).collect())
                        .unwrap_or_default(),
                    reference: s(&i["r"]),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut indices: Vec<SpectralIndex> = indices;
    // a radar's backscatter in decibels, how it is looked at (not in the catalogue)
    for (name, long_name, formula, roles) in [
        ("VVdB", "VV backscatter in decibels", "10.0 * log10(VV)", &["VV"][..]),
        ("VHdB", "VH backscatter in decibels", "10.0 * log10(VH)", &["VH"][..]),
        ("VHVVdB", "VH-VV Ratio in decibels", "10.0 * log10(VH/VV)", &["VH", "VV"][..]),
    ] {
        indices.push(SpectralIndex {
            name: name.into(),
            long_name: long_name.into(),
            domain: "radar".into(),
            formula: formula.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            reference: "10 log10 of the linear backscatter (sigma0 or gamma0)".into(),
        });
    }
    Catalogue { indices, constants }
});

/// Every index, by name.
pub fn all() -> &'static [SpectralIndex] {
    &CATALOGUE.indices
}

/// An index by its name: as written (BAI is not BaI), else whatever its case.
pub fn find(name: &str) -> Option<&'static SpectralIndex> {
    all().iter().find(|i| i.name == name).or_else(|| all().iter().find(|i| i.name.eq_ignore_ascii_case(name)))
}

/// A constant's default (None: no default, e.g. PAR).
pub fn constant(name: &str) -> Option<Option<f64>> {
    CATALOGUE.constants.iter().find(|c| c.0 == name).map(|c| c.1)
}

fn number(v: f64) -> String {
    let s = crate::multidim::format_g(v, 10);
    if v < 0.0 { format!("({s})") } else { s }
}

impl SpectralIndex {
    /// The roles the layer has no band for (empty: it can be computed).
    pub fn missing(&self, names: &BandNames) -> Vec<String> {
        self.roles.iter().filter(|r| names.role_band(r).is_none()).cloned().collect()
    }

    /// Whether it needs a value no band gives (PAR).
    pub fn needs_external(&self) -> bool {
        tokens(&self.formula).iter().any(|t| {
            t.tok == Tok::Ident
                && constant(&self.formula[t.span.clone()]).is_some_and(|d| d.is_none())
                && !self.formula[t.span.clone()].starts_with("lambda")
        })
    }

    pub fn usable(&self, names: &BandNames) -> bool {
        self.missing(names).is_empty() && !self.needs_external()
    }

    /// The formula for a layer: constants by their defaults, `lambdaX` by the wavelength of the layer's band for X
    /// (else the middle of X's range), `**` as `^`. None if a value is missing.
    pub fn formula_for(&self, names: &BandNames, band_nm: &[f64]) -> Option<String> {
        let f = &self.formula;
        let mut out = String::new();
        let mut at = 0;
        for t in tokens(f) {
            out += &f[at..t.span.start];
            at = t.span.end;
            let text = &f[t.span.clone()];
            match t.tok {
                Tok::Ident if role(text).is_none() => {
                    if let Some(r) = text.strip_prefix("lambda").and_then(role) {
                        let nm = names
                            .role_band(r.code)
                            .and_then(|b| band_nm.get(b as usize - 1).copied())
                            .filter(|w| *w > 0.0)
                            .unwrap_or(0.5 * (r.nm.0 + r.nm.1));
                        out += &number(nm);
                    } else {
                        match constant(text) {
                            Some(Some(v)) => out += &number(v),
                            Some(None) => return None,
                            None => out += text,
                        }
                    }
                }
                Tok::Punct(super::lex::Punct::Pow) => out += "^",
                _ => out += text,
            }
        }
        out += &f[at..];
        Some(out)
    }

    /// "EVI: Enhanced Vegetation Index".
    pub fn title(&self) -> String {
        format!("{}: {}", self.name, self.long_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::{DateVars, LayerExpr};

    #[test]
    fn catalogue() {
        assert!(all().len() > 250);
        // names that differ by case only
        assert_eq!(find("BAI").unwrap().long_name, "Burned Area Index");
        assert_eq!(find("BaI").unwrap().long_name, "Bareness Index");
        let evi = find("EVI").unwrap();
        assert_eq!(evi.roles, vec!["N", "R", "B"]);
        let names = BandNames::new(
            ["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2"].iter().map(|s| s.to_string()).collect(),
            &[482.0, 561.0, 655.0, 865.0, 1609.0, 2201.0],
        );
        let f = evi.formula_for(&names, &[482.0, 561.0, 655.0, 865.0, 1609.0, 2201.0]).unwrap();
        assert_eq!(f, "2.5 * (N - R) / (N + 6 * R - 7.5 * B + 1)");
        assert!(find("NDREI").unwrap().missing(&names) == vec!["RE1"]);
        assert!(!find("NIRvP").unwrap().usable(&names));
        // every index this layer covers compiles
        let mut n = 0;
        for i in all().iter().filter(|i| i.usable(&names)) {
            let f = i.formula_for(&names, &[]).unwrap();
            let e = LayerExpr::compile(&f, &names).unwrap_or_else(|e| panic!("{}: {f}: {e:?}", i.name));
            let px = [0.04f32, 0.07, 0.05, 0.35, 0.18, 0.09];
            let inputs: Vec<f32> = e.bands.iter().map(|&b| px[b as usize - 1]).collect();
            let _ = e.program.eval_one(&inputs, &DateVars::default());
            n += 1;
        }
        assert!(n > 100, "{n}");
        // the wavelengths of a layer's bands
        let fai = find("FAI").unwrap().formula_for(&names, &[482.0, 561.0, 655.0, 865.0, 1609.0, 2201.0]).unwrap();
        assert!(fai.contains("865") && fai.contains("1609"), "{fai}");
    }
}
