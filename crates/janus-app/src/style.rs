//! A layer's style (phase 50): what the Layers panel copies from a layer and pastes on others, and what a saved
//! style keeps by name in the settings. A series': its display mode, each mode's colormap and range (the ranges set
//! by hand; the others computed again from the layer's own values), the stretch per date, the RGB bands, its
//! opacity and blend mode, and whether it is shown as classes, with their names, colours and visibility (given to
//! the same values of the other layer). A vector layer's: its colour.

use egui::Color32;
use janus_gpu::{Blend, Mode, colormaps};

use crate::app::State;
use crate::classes::ClassEntry;
use crate::contents::Sel;

#[derive(Clone, Debug, PartialEq)]
pub struct SeriesStyle {
    pub mode: Mode,
    pub cmap: [usize; 14],
    /// each mode's range: low, high, set by hand
    pub ranges: [(f32, f32, bool); 14],
    pub per_date_range: bool,
    pub rgb: [usize; 3],
    pub opacity: f32,
    pub blend: Blend,
    /// shown as classes: their names, colours and visibility
    pub classes: Option<Vec<ClassEntry>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerStyle {
    Series(Box<SeriesStyle>),
    Vector(Color32),
}

/// A name or a number in the style's text: the characters that separate its fields escaped.
pub(crate) fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' | ';' | '|' | ':' | ',' | '=' | '\t' | '\n' | '\r' => out += &format!("%{:02X}", ch as u32),
            _ => out.push(ch),
        }
    }
    out
}

pub(crate) fn unesc(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: &[u8]) -> String {
    c.iter().map(|v| format!("{v:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    for (k, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(2 * k..2 * k + 2)?, 16).ok()?;
    }
    (s.len() == 2 * N).then_some(out)
}

impl LayerStyle {
    /// As one line of text (a saved style in the settings): "series;mode=0;cmap=viridis,...;..." or
    /// "vector;color=rrggbbaa".
    pub fn to_text(&self) -> String {
        match self {
            LayerStyle::Vector(c) => format!("vector;color={}", hex(&c.to_srgba_unmultiplied())),
            LayerStyle::Series(s) => {
                let cmaps = colormaps::all();
                let cmap: Vec<String> =
                    s.cmap.iter().map(|&c| cmaps.get(c).map_or_else(|| c.to_string(), |m| esc(m.name))).collect();
                let ranges: Vec<String> =
                    s.ranges.iter().map(|(lo, hi, m)| format!("{lo}:{hi}:{}", u8::from(*m))).collect();
                let mut out = format!(
                    "series;mode={};cmap={};ranges={};perdate={};rgb={},{},{};opacity={};blend={}",
                    s.mode as i32,
                    cmap.join(","),
                    ranges.join(","),
                    u8::from(s.per_date_range),
                    s.rgb[0],
                    s.rgb[1],
                    s.rgb[2],
                    s.opacity,
                    s.blend.name().to_lowercase(),
                );
                if let Some(classes) = &s.classes {
                    let list: Vec<String> = classes
                        .iter()
                        .map(|c| format!("{}:{}:{}:{}", c.value, hex(&c.color), u8::from(c.visible), esc(&c.name)))
                        .collect();
                    out += &format!(";classes={}", list.join("|"));
                }
                out
            }
        }
    }

    /// Read back from `to_text`; None if it is not a style.
    pub fn from_text(text: &str) -> Option<LayerStyle> {
        let mut parts = text.trim().split(';');
        let kind = parts.next()?;
        let fields: Vec<(&str, &str)> = parts.filter_map(|p| p.split_once('=')).collect();
        let get = |k: &str| fields.iter().find(|f| f.0 == k).map(|f| f.1);
        if kind == "vector" {
            let [r, g, b, a] = unhex::<4>(get("color")?)?;
            return Some(LayerStyle::Vector(Color32::from_rgba_unmultiplied(r, g, b, a)));
        }
        if kind != "series" {
            return None;
        }
        let mode = *Mode::ALL.get(get("mode")?.parse::<usize>().ok()?)?;
        let mut cmap = crate::display::Display::default().cmap;
        for (k, c) in get("cmap").unwrap_or("").split(',').enumerate().take(14) {
            let name = unesc(c);
            if let Some(i) =
                colormaps::by_name(&name).or_else(|| name.parse().ok().filter(|&i| i < colormaps::all().len()))
            {
                cmap[k] = i;
            }
        }
        let mut ranges = [(0.0, 1.0, false); 14];
        for (k, r) in get("ranges").unwrap_or("").split(',').enumerate().take(14) {
            let v: Vec<&str> = r.split(':').collect();
            if let [lo, hi, m] = v[..]
                && let (Ok(lo), Ok(hi)) = (lo.parse(), hi.parse())
            {
                ranges[k] = (lo, hi, m == "1");
            }
        }
        let mut rgb = [0, 0, 0];
        for (k, b) in get("rgb").unwrap_or("").split(',').enumerate().take(3) {
            rgb[k] = b.parse().unwrap_or(0);
        }
        let classes = get("classes").map(|list| {
            list.split('|')
                .filter_map(|c| {
                    let mut f = c.splitn(4, ':');
                    let value = f.next()?.parse().ok()?;
                    let color = unhex::<3>(f.next()?)?;
                    let visible = f.next()? == "1";
                    Some(ClassEntry { value, name: unesc(f.next()?), color, visible, count: 0 })
                })
                .collect()
        });
        Some(LayerStyle::Series(Box::new(SeriesStyle {
            mode,
            cmap,
            ranges,
            per_date_range: get("perdate") == Some("1"),
            rgb,
            opacity: get("opacity").and_then(|o| o.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.0, 1.0),
            blend: get("blend").and_then(Blend::parse).unwrap_or_default(),
            classes,
        })))
    }
}

impl State {
    /// The index of the series of cube `id` (not the canvas).
    pub(crate) fn series_index(&self, id: u64) -> Option<usize> {
        (0..self.layers.len())
            .find(|&i| !self.layers[i].canvas && self.layer_session(i).is_some_and(|s| s.info.id == id))
    }

    /// Layer `sel`'s style.
    pub(crate) fn style_of(&self, sel: Sel) -> Option<LayerStyle> {
        match sel {
            Sel::Vector(id) => self.vectors.iter().find(|v| v.id == id).map(|v| LayerStyle::Vector(v.color)),
            Sel::Series(id) => {
                let i = self.series_index(id)?;
                let l = &self.layers[i];
                let (d, c) = if i == self.active { (&self.display, &self.classes) } else { (&l.disp, &l.classes) };
                Some(LayerStyle::Series(Box::new(SeriesStyle {
                    mode: d.mode,
                    cmap: d.cmap,
                    ranges: d.ranges.map(|r| (r.lo, r.hi, r.manual)),
                    per_date_range: d.per_date_range,
                    rgb: d.rgb,
                    opacity: l.opacity,
                    blend: l.blend,
                    // a class's count is not of the style
                    classes: c.on().then(|| c.list.iter().map(|e| ClassEntry { count: 0, ..e.clone() }).collect()),
                })))
            }
        }
    }

    /// `style` given to layer `sel`; false if it is a style of the other kind of layer.
    pub(crate) fn apply_style(&mut self, sel: Sel, style: &LayerStyle) -> bool {
        match (sel, style) {
            (Sel::Vector(id), LayerStyle::Vector(c)) => {
                let Some(v) = self.vectors.iter_mut().find(|v| v.id == id) else { return false };
                v.color = *c;
            }
            (Sel::Series(id), LayerStyle::Series(st)) => {
                let Some(i) = self.series_index(id) else { return false };
                let n = self.layer_session(i).map_or(1, |s| s.info.t()).max(1);
                let active = i == self.active;
                let (d, c) = if active {
                    (&mut self.display, &mut self.classes)
                } else {
                    let l = &mut self.layers[i];
                    (&mut l.disp, &mut l.classes)
                };
                // a single date or band: the modes through time left out
                d.mode = if n >= 2 || matches!(st.mode, Mode::Value | Mode::Rgb) { st.mode } else { Mode::Value };
                d.cmap = st.cmap;
                for (r, &(lo, hi, manual)) in d.ranges.iter_mut().zip(&st.ranges) {
                    if manual {
                        (r.lo, r.hi, r.manual) = (lo, hi, true);
                    } else {
                        r.manual = false;
                        r.invalidate();
                    }
                }
                d.per_date_range = st.per_date_range;
                d.rgb = st.rgb.map(|b| b.min(n - 1));
                c.apply_style(st.classes.as_deref());
                let l = &mut self.layers[i];
                (l.opacity, l.blend) = (st.opacity, st.blend);
            }
            _ => return false,
        }
        self.map.dirty = true;
        true
    }

    /// The style copied (a row's Copy style) pasted on the layers selected, or on `sel` alone when it is not in
    /// the selection; how many took it.
    pub(crate) fn paste_style(&mut self, sel: Sel, style: &LayerStyle) -> usize {
        let targets: Vec<Sel> = if self.contents.selected.contains(&sel) {
            self.contents.order.iter().copied().filter(|s| self.contents.selected.contains(s)).collect()
        } else {
            vec![sel]
        };
        targets.into_iter().filter(|&t| self.apply_style(t, style)).count()
    }

    /// The saved style named `name` (the settings' styles), replaced if there is one.
    pub(crate) fn save_style(&mut self, name: &str, style: &LayerStyle) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let styles = &mut self.settings.s.styles;
        let text = style.to_text();
        match styles.iter_mut().find(|s| s.0 == name) {
            Some(s) => s.1 = text,
            None => styles.push((name, text)),
        }
        self.settings.changed();
    }

    /// The saved styles a layer of `sel`'s kind takes: name, style.
    pub(crate) fn saved_styles_for(&self, sel: Sel) -> Vec<(String, LayerStyle)> {
        self.settings
            .s
            .styles
            .iter()
            .filter_map(|(n, t)| Some((n.clone(), LayerStyle::from_text(t)?)))
            .filter(|(_, st)| {
                matches!((sel, st), (Sel::Series(_), LayerStyle::Series(_)) | (Sel::Vector(_), LayerStyle::Vector(_)))
            })
            .collect()
    }

    pub(crate) fn delete_saved_style(&mut self, name: &str) {
        self.settings.s.styles.retain(|s| s.0 != name);
        self.settings.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_text_round_trip() {
        let mut ranges = [(0.0, 1.0, false); 14];
        ranges[0] = (-0.25, 0.875, true);
        let s = LayerStyle::Series(Box::new(SeriesStyle {
            mode: Mode::Rgb,
            cmap: [colormaps::PLASMA; 14],
            ranges,
            per_date_range: true,
            rgb: [3, 2, 1],
            opacity: 0.5,
            blend: Blend::Multiply,
            classes: Some(vec![ClassEntry {
                value: 3,
                name: "Forest; dense: 50%|x".into(),
                color: [10, 200, 30],
                visible: false,
                count: 0,
            }]),
        }));
        assert_eq!(LayerStyle::from_text(&s.to_text()), Some(s));
        let v = LayerStyle::Vector(Color32::from_rgba_unmultiplied(255, 128, 0, 200));
        assert_eq!(LayerStyle::from_text(&v.to_text()), Some(v));
        assert_eq!(LayerStyle::from_text("nonsense"), None);
    }
}
