//! The colormaps of the maps and charts: ImPlot's, plus Janus' NDVI and Strata (src/app.cpp), sampled exactly as
//! ImPlot samples them, so that a map has the same colours as in the C++ (and its self-tests the same values).

/// RGBA in one word, red in the low byte (ImGui's IM_COL32).
const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    0xFF00_0000 | (b << 16) | (g << 8) | r
}

/// A float colour as ImGui stores it (ColorConvertFloat4ToU32).
fn rgbf(r: f32, g: f32, b: f32) -> u32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    rgb(c(r), c(g), c(b))
}

pub struct Colormap {
    pub name: &'static str,
    /// qualitative: a colour per class, no blending
    pub qualitative: bool,
    keys: Vec<u32>,
}

/// ImPlot's colormaps in their order (ImPlotColormap_Deep = 0 ... Greys = 15), then NDVI and Strata.
pub fn all() -> &'static [Colormap] {
    static ALL: std::sync::OnceLock<Vec<Colormap>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        let c = |name, qualitative, keys: &[u32]| Colormap { name, qualitative, keys: keys.to_vec() };
        vec![
            c(
                "Deep",
                true,
                &[
                    rgb(76, 114, 176),
                    rgb(221, 132, 82),
                    rgb(85, 168, 104),
                    rgb(196, 78, 82),
                    rgb(129, 114, 179),
                    rgb(147, 120, 96),
                    rgb(218, 139, 195),
                    rgb(140, 140, 140),
                    rgb(204, 185, 116),
                    rgb(100, 181, 205),
                ],
            ),
            c(
                "Dark",
                true,
                &[
                    rgb(228, 26, 28),
                    rgb(55, 126, 184),
                    rgb(77, 175, 74),
                    rgb(152, 78, 163),
                    rgb(255, 127, 0),
                    rgb(255, 255, 51),
                    rgb(166, 86, 40),
                    rgb(247, 129, 191),
                    rgb(153, 153, 153),
                ],
            ),
            c(
                "Pastel",
                true,
                &[
                    rgb(251, 180, 174),
                    rgb(179, 205, 227),
                    rgb(204, 235, 197),
                    rgb(222, 203, 228),
                    rgb(254, 217, 166),
                    rgb(255, 255, 204),
                    rgb(229, 216, 189),
                    rgb(253, 218, 236),
                    rgb(242, 242, 242),
                ],
            ),
            c(
                "Paired",
                true,
                &[
                    rgb(66, 206, 227),
                    rgb(31, 120, 180),
                    rgb(178, 223, 138),
                    rgb(51, 160, 44),
                    rgb(251, 154, 153),
                    rgb(227, 26, 28),
                    rgb(253, 191, 111),
                    rgb(255, 127, 0),
                    rgb(202, 178, 214),
                    rgb(106, 61, 154),
                    rgb(255, 255, 153),
                    rgb(177, 89, 40),
                ],
            ),
            c(
                "Viridis",
                false,
                &[
                    rgb(68, 1, 84),
                    rgb(72, 36, 117),
                    rgb(65, 68, 135),
                    rgb(53, 95, 141),
                    rgb(42, 120, 142),
                    rgb(33, 145, 140),
                    rgb(34, 168, 132),
                    rgb(68, 191, 112),
                    rgb(122, 209, 81),
                    rgb(189, 223, 38),
                    rgb(253, 231, 37),
                ],
            ),
            c(
                "Plasma",
                false,
                &[
                    rgb(13, 8, 135),
                    rgb(65, 4, 157),
                    rgb(106, 0, 168),
                    rgb(143, 13, 164),
                    rgb(177, 42, 144),
                    rgb(204, 71, 120),
                    rgb(225, 100, 98),
                    rgb(242, 132, 75),
                    rgb(252, 166, 54),
                    rgb(252, 206, 37),
                    rgb(240, 249, 33),
                ],
            ),
            c(
                "Hot",
                false,
                &[
                    rgb(64, 0, 0),
                    rgb(128, 0, 0),
                    rgb(191, 0, 0),
                    rgb(255, 0, 0),
                    rgb(255, 64, 0),
                    rgb(255, 128, 0),
                    rgb(255, 191, 0),
                    rgb(255, 255, 0),
                    rgb(255, 255, 85),
                    rgb(255, 255, 170),
                    rgb(255, 255, 255),
                ],
            ),
            c(
                "Cool",
                false,
                &[
                    rgb(0, 255, 255),
                    rgb(26, 230, 255),
                    rgb(51, 204, 255),
                    rgb(77, 179, 255),
                    rgb(102, 153, 255),
                    rgb(128, 128, 255),
                    rgb(153, 102, 255),
                    rgb(179, 77, 255),
                    rgb(204, 51, 255),
                    rgb(230, 26, 255),
                    rgb(255, 0, 255),
                ],
            ),
            c(
                "Pink",
                false,
                &[
                    rgb(74, 0, 0),
                    rgb(123, 66, 66),
                    rgb(158, 93, 93),
                    rgb(186, 114, 114),
                    rgb(198, 151, 132),
                    rgb(208, 180, 147),
                    rgb(218, 206, 161),
                    rgb(228, 228, 174),
                    rgb(237, 237, 205),
                    rgb(246, 246, 231),
                    rgb(255, 255, 255),
                ],
            ),
            c(
                "Jet",
                false,
                &[
                    rgb(0, 0, 170),
                    rgb(0, 0, 255),
                    rgb(0, 85, 255),
                    rgb(0, 170, 255),
                    rgb(0, 255, 255),
                    rgb(85, 255, 170),
                    rgb(170, 255, 85),
                    rgb(255, 255, 0),
                    rgb(255, 170, 0),
                    rgb(255, 85, 0),
                    rgb(255, 0, 0),
                ],
            ),
            c(
                "Twilight",
                false,
                &[
                    rgb(226, 217, 226),
                    rgb(166, 191, 202),
                    rgb(109, 144, 192),
                    rgb(95, 88, 176),
                    rgb(83, 30, 124),
                    rgb(47, 20, 54),
                    rgb(100, 25, 75),
                    rgb(159, 60, 80),
                    rgb(192, 117, 94),
                    rgb(208, 179, 158),
                    rgb(226, 217, 226),
                ],
            ),
            c(
                "RdBu",
                false,
                &[
                    rgb(103, 0, 31),
                    rgb(178, 24, 43),
                    rgb(214, 96, 77),
                    rgb(244, 165, 130),
                    rgb(253, 219, 199),
                    rgb(247, 247, 247),
                    rgb(209, 229, 240),
                    rgb(146, 197, 222),
                    rgb(67, 147, 195),
                    rgb(33, 102, 172),
                    rgb(5, 48, 97),
                ],
            ),
            c(
                "BrBG",
                false,
                &[
                    rgb(84, 48, 5),
                    rgb(140, 81, 10),
                    rgb(191, 129, 45),
                    rgb(223, 194, 125),
                    rgb(246, 232, 195),
                    rgb(245, 245, 245),
                    rgb(199, 234, 229),
                    rgb(128, 205, 193),
                    rgb(53, 151, 143),
                    rgb(1, 102, 94),
                    rgb(0, 60, 48),
                ],
            ),
            c(
                "PiYG",
                false,
                &[
                    rgb(142, 1, 82),
                    rgb(197, 27, 125),
                    rgb(222, 119, 174),
                    rgb(241, 182, 218),
                    rgb(253, 224, 239),
                    rgb(247, 247, 247),
                    rgb(230, 245, 208),
                    rgb(184, 225, 134),
                    rgb(127, 188, 65),
                    rgb(77, 146, 33),
                    rgb(39, 100, 25),
                ],
            ),
            c(
                "Spectral",
                false,
                &[
                    rgb(158, 1, 66),
                    rgb(213, 62, 79),
                    rgb(244, 109, 67),
                    rgb(253, 174, 97),
                    rgb(254, 224, 139),
                    rgb(255, 255, 191),
                    rgb(230, 245, 152),
                    rgb(171, 221, 164),
                    rgb(102, 194, 165),
                    rgb(50, 136, 189),
                    rgb(94, 79, 162),
                ],
            ),
            c("Greys", false, &[rgb(255, 255, 255), rgb(0, 0, 0)]),
            // designed for vegetation indices
            Colormap {
                name: "NDVI",
                qualitative: false,
                keys: vec![
                    rgbf(0.55, 0.33, 0.14),
                    rgbf(0.80, 0.66, 0.42),
                    rgbf(0.96, 0.91, 0.60),
                    rgbf(0.62, 0.80, 0.36),
                    rgbf(0.22, 0.56, 0.22),
                    rgbf(0.04, 0.30, 0.10),
                ],
            },
            // CODED's strata: forest, non-forest, degradation, deforestation, disturbance
            Colormap {
                name: "Strata",
                qualitative: true,
                keys: vec![
                    rgbf(0.10, 0.45, 0.18),
                    rgbf(0.85, 0.76, 0.50),
                    rgbf(0.96, 0.55, 0.10),
                    rgbf(0.84, 0.12, 0.12),
                    rgbf(0.55, 0.35, 0.75),
                ],
            },
            // a monitored pixel's state (EWMACD): stable, disturbed, recovered, gain
            Colormap {
                name: "Monitoring",
                qualitative: true,
                keys: vec![
                    rgbf(0.62, 0.64, 0.60),
                    rgbf(0.84, 0.12, 0.12),
                    rgbf(0.96, 0.66, 0.15),
                    rgbf(0.12, 0.55, 0.30),
                ],
            },
            // a monitored pixel's alert (S-CCD): stable, change in progress, waiting for a model after a change, no model
            Colormap {
                name: "Alerts",
                qualitative: true,
                keys: vec![
                    rgbf(0.62, 0.64, 0.60),
                    rgbf(0.84, 0.12, 0.12),
                    rgbf(0.96, 0.66, 0.15),
                    rgbf(0.90, 0.90, 0.88),
                ],
            },
            // emerging hot spots' patterns: none, then new ... historical hot spots (reds), the same cold (blues)
            Colormap {
                name: "Hotspots",
                qualitative: true,
                keys: vec![
                    rgbf(0.78, 0.78, 0.76),
                    rgbf(0.55, 0.00, 0.05),
                    rgbf(0.80, 0.10, 0.10),
                    rgbf(0.95, 0.25, 0.15),
                    rgbf(0.98, 0.45, 0.30),
                    rgbf(0.99, 0.65, 0.50),
                    rgbf(0.95, 0.60, 0.15),
                    rgbf(0.85, 0.40, 0.65),
                    rgbf(0.98, 0.82, 0.78),
                    rgbf(0.03, 0.19, 0.42),
                    rgbf(0.13, 0.40, 0.67),
                    rgbf(0.25, 0.55, 0.85),
                    rgbf(0.42, 0.68, 0.90),
                    rgbf(0.62, 0.80, 0.95),
                    rgbf(0.20, 0.70, 0.70),
                    rgbf(0.50, 0.45, 0.80),
                    rgbf(0.82, 0.90, 0.97),
                ],
            },
        ]
    })
}

pub const VIRIDIS: usize = 4;
pub const PLASMA: usize = 5;
pub const BRBG: usize = 12;

pub fn by_name(name: &str) -> Option<usize> {
    all().iter().position(|c| c.name == name)
}

/// ImPlot's ImMixU32: the two colours blended by s/256, per channel.
fn mix(a: u32, b: u32, s: u32) -> u32 {
    let (af, bf) = (256 - s, s);
    let (al, ah) = (a & 0x00ff00ff, (a & 0xff00ff00) >> 8);
    let (bl, bh) = (b & 0x00ff00ff, (b & 0xff00ff00) >> 8);
    let ml = al.wrapping_mul(af).wrapping_add(bl.wrapping_mul(bf));
    let mh = ah.wrapping_mul(af).wrapping_add(bh.wrapping_mul(bf));
    (mh & 0xff00ff00) | ((ml & 0xff00ff00) >> 8)
}

impl Colormap {
    /// ImPlot's table: the keys, or 255 steps between each two of them.
    fn table(&self) -> Vec<u32> {
        if self.qualitative {
            return self.keys.clone();
        }
        let mut t = Vec::with_capacity(255 * (self.keys.len() - 1) + 1);
        for w in self.keys.windows(2) {
            t.extend((0..255).map(|s| mix(w[0], w[1], s)));
        }
        t.push(*self.keys.last().unwrap());
        t
    }

    /// The colour at t in [0, 1], as ImPlot's SampleColormap (RGBA bytes).
    pub fn sample(&self, t: f32) -> [u8; 4] {
        let table = self.table();
        self.sample_table(&table, t)
    }

    fn sample_table(&self, table: &[u32], t: f32) -> [u8; 4] {
        let n = table.len();
        let i = if self.qualitative {
            ((n as f32 * t) as i32).clamp(0, n as i32 - 1) as usize
        } else {
            ((n - 1) as f32 * t + 0.5) as i32 as usize
        };
        table[i.min(n - 1)].to_le_bytes()
    }

    /// The 256 colours of the map's lookup texture (gpu_gl.cpp, sampleColormap).
    pub fn lut(&self) -> [[u8; 4]; 256] {
        let table = self.table();
        std::array::from_fn(|i| {
            let mut c = self.sample_table(&table, i as f32 / 255.0);
            c[3] = 255;
            c
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = [u8; 4]> + '_ {
        self.keys.iter().map(|k| k.to_le_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ends_and_middle() {
        let v = &all()[VIRIDIS];
        assert_eq!(v.sample(0.0), [68, 1, 84, 255]);
        assert_eq!(v.sample(1.0), [253, 231, 37, 255]);
        let lut = v.lut();
        assert_eq!(lut[0], [68, 1, 84, 255]);
        assert_eq!(lut[255], [253, 231, 37, 255]);
        // half way between keys 4 and 5 of 11 (table of 2551 colours, index 1275 = key 5)
        assert_eq!(v.sample(0.5), [33, 145, 140, 255]);
        // qualitative: the fifth of five classes, no blending
        assert_eq!(all()[by_name("Strata").unwrap()].sample(0.99), [140, 89, 191, 255]);
    }
}
