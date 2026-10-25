//! A layer's bands by name: the roles of the Awesome Spectral Indices (N, R, RE1, S1...) placed by wavelength or
//! recognised in the band names, a band's own name or a word of it (`B04`, `B4`, `nir`, `SCL`), or its place
//! (`band4`, `b4` when no band is called so).

use crate::CubeInfo;

/// A role of the Awesome Spectral Indices: its letter, what it is, its range of wavelengths (nm).
pub struct Role {
    pub code: &'static str,
    pub name: &'static str,
    pub nm: (f64, f64),
    /// words of band names that mean it (lowercase, without spaces)
    pub words: &'static [&'static str],
}

pub const ROLES: &[Role] = &[
    Role {
        code: "A",
        name: "Aerosols (coastal)",
        nm: (400.0, 455.0),
        words: &["coastal", "aerosol", "aerosols", "ultrablue"],
    },
    Role { code: "B", name: "Blue", nm: (450.0, 530.0), words: &["blue"] },
    Role { code: "G", name: "Green", nm: (510.0, 600.0), words: &["green"] },
    Role { code: "G1", name: "Green 1", nm: (510.0, 550.0), words: &["green1", "green05"] },
    Role { code: "Y", name: "Yellow", nm: (585.0, 625.0), words: &["yellow"] },
    Role { code: "R", name: "Red", nm: (620.0, 690.0), words: &["red"] },
    Role {
        code: "RE1",
        name: "Red edge 1",
        nm: (695.0, 715.0),
        words: &["rededge1", "re1", "rededge071", "rededge70"],
    },
    Role {
        code: "RE2",
        name: "Red edge 2",
        nm: (730.0, 750.0),
        words: &["rededge2", "re2", "rededge075", "rededge74"],
    },
    Role {
        code: "RE3",
        name: "Red edge 3",
        nm: (765.0, 795.0),
        words: &["rededge3", "re3", "rededge078", "rededge78"],
    },
    Role { code: "N", name: "Near infrared (NIR)", nm: (760.0, 900.0), words: &["nir", "nir1", "nearinfrared"] },
    Role {
        code: "N2",
        name: "Near infrared 2 (narrow)",
        nm: (850.0, 880.0),
        words: &["nir2", "nir08", "narrownir", "nirnarrow"],
    },
    Role { code: "WV", name: "Water vapour", nm: (930.0, 960.0), words: &["wv", "watervapour", "watervapor", "nir09"] },
    Role {
        code: "S1",
        name: "Shortwave infrared 1 (SWIR 1)",
        nm: (1550.0, 1750.0),
        words: &["swir1", "swir16", "swir161"],
    },
    Role {
        code: "S2",
        name: "Shortwave infrared 2 (SWIR 2)",
        nm: (2080.0, 2350.0),
        words: &["swir2", "swir22", "swir220"],
    },
    Role {
        code: "T",
        name: "Thermal infrared",
        nm: (10400.0, 12500.0),
        words: &["thermal", "lwir", "tir", "stb10", "brightness"],
    },
    Role {
        code: "T1",
        name: "Thermal infrared 1",
        nm: (10600.0, 11190.0),
        words: &["thermal1", "lwir11", "tir1", "st_b10"],
    },
    Role { code: "T2", name: "Thermal infrared 2", nm: (11500.0, 12510.0), words: &["thermal2", "lwir12", "tir2"] },
    Role { code: "VV", name: "Radar VV", nm: (0.0, 0.0), words: &["vv"] },
    Role { code: "VH", name: "Radar VH", nm: (0.0, 0.0), words: &["vh"] },
    Role { code: "HH", name: "Radar HH", nm: (0.0, 0.0), words: &["hh"] },
    Role { code: "HV", name: "Radar HV", nm: (0.0, 0.0), words: &["hv"] },
];

pub fn role(code: &str) -> Option<&'static Role> {
    ROLES.iter().find(|r| r.code == code)
}

/// Lowercase, without spaces, underscores, dashes or a leading zero after letters: "Red (B04)" -> "red(b4)",
/// "SWIR_1" -> "swir1", "B08" -> "b8".
pub fn norm(s: &str) -> String {
    let mut out = String::new();
    let mut prev_alpha = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, ' ' | '_' | '-') {
            continue;
        }
        let l = c.to_ascii_lowercase();
        if l == '0' && prev_alpha && chars.peek().is_some_and(|n| n.is_ascii_digit()) {
            continue;
        }
        prev_alpha = l.is_alphabetic();
        out.push(l);
    }
    out
}

/// The words a band answers to: its whole name (without a wavelength in brackets) and each word of it.
fn band_words(name: &str) -> Vec<String> {
    let base = match name.find(" (") {
        Some(k) if name.ends_with("nm)") => &name[..k],
        _ => name,
    };
    let mut w = vec![norm(base)];
    // "Red (B04)": red, b4; "SR_B4": sr, b4
    for part in base.split(|c: char| !c.is_alphanumeric()).filter(|p| p.chars().any(char::is_alphabetic)) {
        let n = norm(part);
        if !w.contains(&n) {
            w.push(n);
        }
    }
    // a name's words joined without the separators ("SWIR 1" is swir1 already; "B8A" stays)
    w
}

/// What the bands of a layer are called and which roles they fill.
#[derive(Clone, Debug, Default)]
pub struct BandNames {
    /// each band's name (1-based by position)
    pub names: Vec<String>,
    words: Vec<Vec<String>>,
    /// (role code, band) found
    pub roles: Vec<(&'static str, i32)>,
}

/// How a name resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Band(i32),
    /// a role this layer has no band for
    MissingRole(&'static str),
    Ambiguous(Vec<i32>),
}

impl BandNames {
    pub fn of(info: &CubeInfo) -> Self {
        let n = info.bands_per_date.max(1);
        let names: Vec<String> =
            (0..n).map(|b| info.band_names.get(b).cloned().unwrap_or_else(|| format!("Band {}", b + 1))).collect();
        Self::new(names, &info.band_nm)
    }

    /// From the bands' names and wavelengths (nm, 0 or none for a band without one).
    pub fn new(names: Vec<String>, nm: &[f64]) -> Self {
        let words: Vec<Vec<String>> = names.iter().map(|n| band_words(n)).collect();
        let mut roles: Vec<(&'static str, i32)> = Vec::new();
        for r in ROLES {
            // by wavelength: the band nearest the range's centre, within it (10 nm of margin)
            let mut best: Option<(f64, i32)> = None;
            if r.nm.1 > 0.0 {
                let centre = 0.5 * (r.nm.0 + r.nm.1);
                for (b, &w) in nm.iter().enumerate() {
                    if w > 0.0 && w >= r.nm.0 - 10.0 && w <= r.nm.1 + 10.0 {
                        let d = (w - centre).abs();
                        if best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, b as i32 + 1));
                        }
                    }
                }
            }
            // by name: a word of the band's name (red edge bands are not red, nor QA bands anything)
            if best.is_none() {
                for (b, w) in words.iter().enumerate() {
                    let full = &w[0];
                    if r.code != "RE1"
                        && r.code != "RE2"
                        && r.code != "RE3"
                        && (full.contains("rededge")
                            || (full.len() == 3
                                && full.starts_with("re")
                                && full.ends_with(|c: char| c.is_ascii_digit())))
                    {
                        continue;
                    }
                    if full.contains("qa") || full.contains("fmask") || full == "scl" {
                        continue;
                    }
                    let hit = w.iter().any(|x| r.words.contains(&x.as_str()))
                        || (r.code != "R" && r.words.iter().any(|k| k.len() >= 4 && full.contains(k)));
                    if hit {
                        best = Some((0.0, b as i32 + 1));
                        break;
                    }
                }
            }
            if let Some((_, b)) = best {
                roles.push((r.code, b));
            }
        }
        // unnamed stacks of 6 or more bands: Blue..SWIR2 in order (roles.rs)
        if roles.is_empty() && names.len() >= 6 && names.iter().all(|n| norm(n).starts_with("band")) {
            for (k, code) in ["B", "G", "R", "N", "S1", "S2"].iter().enumerate() {
                roles.push((code, k as i32 + 1));
            }
        }
        BandNames { names, words, roles }
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    pub fn role_band(&self, code: &str) -> Option<i32> {
        self.roles.iter().find(|r| r.0 == code).map(|r| r.1)
    }

    /// The roles a band fills.
    pub fn roles_of(&self, band: i32) -> Vec<&'static str> {
        self.roles.iter().filter(|r| r.1 == band).map(|r| r.0).collect()
    }

    /// A name as a band of this layer: a role's code (N, RE1, S1: case matters), a band's name or a word of it
    /// (case does not), a role's long name (nir, swir1), its place (band4, b4); None if it names none.
    pub fn resolve(&self, id: &str) -> Option<Resolved> {
        if let Some(r) = role(id) {
            return Some(self.role_band(r.code).map_or(Resolved::MissingRole(r.code), Resolved::Band));
        }
        let n = norm(id);
        let hits: Vec<i32> =
            (0..self.names.len()).filter(|&b| self.words[b].contains(&n)).map(|b| b as i32 + 1).collect();
        // a band's whole name before a word of several
        let whole: Vec<i32> = hits.iter().copied().filter(|&b| self.words[b as usize - 1][0] == n).collect();
        let by_role = ROLES.iter().find(|r| r.words.contains(&n.as_str())).map(|r| r.code);
        match (whole.len(), hits.len()) {
            (1, _) => return Some(Resolved::Band(whole[0])),
            (_, 1) => return Some(Resolved::Band(hits[0])),
            // a role's word in several names ("NIR (B08)", "NIR 2 (B8A)"): the role's band
            (_, k) if k > 1 => {
                return Some(match by_role.and_then(|c| self.role_band(c)) {
                    Some(b) if hits.contains(&b) => Resolved::Band(b),
                    _ => Resolved::Ambiguous(hits),
                });
            }
            _ => {}
        }
        if let Some(code) = by_role {
            return Some(self.role_band(code).map_or(Resolved::MissingRole(code), Resolved::Band));
        }
        let digits = n.strip_prefix("band").or_else(|| n.strip_prefix('b'))?;
        let k: usize = digits.parse().ok().filter(|_| digits.bytes().all(|c| c.is_ascii_digit()))?;
        (k >= 1 && k <= self.names.len()).then_some(Resolved::Band(k as i32))
    }

    /// The bands a name could be, as they can be written: "S1 (SWIR 1 (B11)) or S2 (SWIR 2 (B12))", a band
    /// without a role by its place ("band5 (...)").
    pub fn candidates(&self, bands: &[i32]) -> String {
        bands
            .iter()
            .map(|&b| {
                let name = self.names.get(b as usize - 1).map_or("", |s| s.as_str());
                // without the wavelength the name may end with ("Red (B04) (665.0 nm)")
                let name = match name.rfind(" (") {
                    Some(i) if name.ends_with("nm)") => &name[..i],
                    _ => name,
                };
                let word = self.roles_of(b).first().map_or(format!("band{b}"), |r| r.to_string());
                format!("{word} ({name})")
            })
            .collect::<Vec<_>>()
            .join(" or ")
    }

    /// A short description of band b: "B08 · NIR (N)".
    pub fn describe(&self, band: i32) -> String {
        let name = self.names.get(band as usize - 1).cloned().unwrap_or_else(|| format!("Band {band}"));
        let roles = self.roles_of(band);
        if roles.is_empty() {
            format!("band {band}: {name}")
        } else {
            format!("band {band}: {name} ({})", roles.join(", "))
        }
    }

    /// The names an identifier could have meant (for "unknown name" errors): roles and band words.
    pub fn suggestions(&self) -> Vec<String> {
        let mut v: Vec<String> = self.roles.iter().map(|r| r.0.to_string()).collect();
        for w in &self.words {
            if let Some(first) = w.first()
                && first.chars().next().is_some_and(char::is_alphabetic)
                && first.chars().all(char::is_alphanumeric)
            {
                v.push(first.clone());
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s2() -> BandNames {
        // a STAC series of Sentinel-2: by wavelength
        let names = [
            ("Coastal (B01)", 443.0),
            ("Blue (B02)", 490.0),
            ("Green (B03)", 560.0),
            ("Red (B04)", 665.0),
            ("Red edge 1 (B05)", 705.0),
            ("Red edge 2 (B06)", 740.0),
            ("Red edge 3 (B07)", 783.0),
            ("NIR (B08)", 842.0),
            ("NIR 2 (B8A)", 865.0),
            ("Water vapour (B09)", 945.0),
            ("SWIR 1 (B11)", 1610.0),
            ("SWIR 2 (B12)", 2190.0),
            ("SCL", 0.0),
        ];
        BandNames::new(names.iter().map(|n| n.0.to_string()).collect(), &names.map(|n| n.1))
    }

    #[test]
    fn sentinel2_by_wavelength() {
        let b = s2();
        let r = |id| b.resolve(id);
        assert_eq!(r("N"), Some(Resolved::Band(8)));
        assert_eq!(r("N2"), Some(Resolved::Band(9)));
        assert_eq!(r("RE1"), Some(Resolved::Band(5)));
        assert_eq!(r("RE3"), Some(Resolved::Band(7)));
        assert_eq!(r("WV"), Some(Resolved::Band(10)));
        assert_eq!(r("A"), Some(Resolved::Band(1)));
        assert_eq!(r("S2"), Some(Resolved::Band(12)));
        // band names and their words, with or without the zero
        assert_eq!(r("B04"), Some(Resolved::Band(4)));
        assert_eq!(r("B4"), Some(Resolved::Band(4)));
        assert_eq!(r("b8a"), Some(Resolved::Band(9)));
        assert_eq!(r("B9"), Some(Resolved::Band(10)));
        assert_eq!(r("scl"), Some(Resolved::Band(13)));
        assert_eq!(r("nir"), Some(Resolved::Band(8)));
        assert_eq!(r("swir1"), Some(Resolved::Band(11)));
        // the place
        assert_eq!(r("band9"), Some(Resolved::Band(9)));
        assert_eq!(r("T"), Some(Resolved::MissingRole("T")));
        assert_eq!(r("foo"), None);
        assert_eq!(r("b14"), None);
    }

    #[test]
    fn landsat_by_name() {
        let names = ["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2", "QA_PIXEL"];
        let b = BandNames::new(names.iter().map(|s| s.to_string()).collect(), &[]);
        assert_eq!(b.resolve("R"), Some(Resolved::Band(3)));
        assert_eq!(b.resolve("S1"), Some(Resolved::Band(5)));
        assert_eq!(b.resolve("qa_pixel"), Some(Resolved::Band(7)));
        // bN without a band called so is the place
        assert_eq!(b.resolve("b4"), Some(Resolved::Band(4)));
        assert_eq!(b.resolve("swir"), Some(Resolved::Ambiguous(vec![5, 6])));
        assert_eq!(b.resolve("RE1"), Some(Resolved::MissingRole("RE1")));
        // unnamed stacks
        let b = BandNames::new((1..=6).map(|k| format!("Band {k}")).collect(), &[]);
        assert_eq!((b.role_band("B"), b.role_band("S2")), (Some(1), Some(6)));
        assert_eq!(b.resolve("band1"), Some(Resolved::Band(1)));
    }
}
