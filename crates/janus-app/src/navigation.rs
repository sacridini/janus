//! Getting around the map (phase 51). Go to a coordinate (Ctrl+G): longitude and latitude in decimal degrees or in
//! degrees, minutes and seconds, UTM, x y in an EPSG CRS or in the layer's, a source pixel; the map centred there, a
//! mark that fades, a pin if asked. Saved views: the area (its centre in longitude and latitude and its width in
//! metres, so any layer in any CRS can go back to it), the date and the layers shown, by name, kept in the
//! settings. The minimap (M): the active layer whole in a corner of the map, the view's rectangle on it; a click or
//! a drag on it moves the map. (The lens, the swipe as a circle that follows the cursor, is the swipe's: views.rs.)

use std::time::{Duration, Instant};

use egui::{Color32, Pos2, Rect, Stroke, Ui, vec2};

use crate::app::State;
use crate::style::{esc, unesc};

/// A place typed in Go to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// longitude, latitude (WGS 84)
    LonLat(f64, f64),
    /// x, y in a CRS: an EPSG code, or the layer's (None)
    Xy(Option<u32>, f64, f64),
    /// a source pixel of the active layer: column, row
    Pixel(f64, f64),
}

/// The GPU target the minimap is drawn into.
pub const MINIMAP_SLOT: i32 = 1 << 20;
/// The minimap's longer side (points).
const MINIMAP_SIZE: f32 = 170.0;
/// How long the mark of a place gone to stays.
const MARK_SECONDS: f32 = 8.0;

enum Token {
    Num(f64),
    /// a hemisphere: N, S, E, W (O: oeste)
    Hemi(char),
}

/// The numbers and hemisphere letters of a coordinate, the other characters (degree, minute and second signs,
/// commas) between them; a comma between digits is a decimal comma when no point is used.
fn tokens(s: &str) -> Result<Vec<Token>, String> {
    let decimal_comma = !s.contains('.');
    let ch: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < ch.len() {
        let c = ch[i];
        let digit = |k: usize| ch.get(k).is_some_and(|c| c.is_ascii_digit());
        if c.is_ascii_digit()
            || ((c == '-' || c == '+' || c == '.') && (digit(i + 1) || (c != '.' && ch.get(i + 1) == Some(&'.'))))
        {
            let mut t = String::new();
            t.push(c);
            i += 1;
            while i < ch.len() {
                let c = ch[i];
                if c.is_ascii_digit() || c == '.' {
                    t.push(c);
                } else if c == ',' && decimal_comma && digit(i + 1) && ch[i - 1].is_ascii_digit() {
                    t.push('.');
                } else if (c == 'e' || c == 'E')
                    && (digit(i + 1) || (matches!(ch.get(i + 1), Some('-' | '+')) && digit(i + 2)))
                {
                    t.push(c);
                    t.push(ch[i + 1]);
                    i += 1;
                } else {
                    break;
                }
                i += 1;
            }
            out.push(Token::Num(t.parse().map_err(|_| format!("not a number: {t}"))?));
            continue;
        }
        let up = c.to_ascii_uppercase();
        if matches!(up, 'N' | 'S' | 'E' | 'W' | 'O') {
            if ch.get(i + 1).is_some_and(|c| c.is_alphabetic()) {
                return Err(format!("not understood: {}", ch[i..].iter().collect::<String>()));
            }
            out.push(Token::Hemi(if up == 'O' { 'W' } else { up }));
        } else if c.is_alphabetic() {
            return Err(format!("not understood: {c}"));
        }
        i += 1;
    }
    Ok(out)
}

/// Degrees, minutes and seconds (one to three numbers) as degrees, the sign of the degrees for all.
fn degrees(v: &[f64]) -> Result<f64, String> {
    if v.is_empty() || v.len() > 3 {
        return Err("a coordinate is degrees, or degrees minutes (seconds)".into());
    }
    if v[1..].iter().any(|&x| !(0.0..60.0).contains(&x)) {
        return Err("minutes and seconds go from 0 to 59".into());
    }
    let m = v.get(1).copied().unwrap_or(0.0) / 60.0 + v.get(2).copied().unwrap_or(0.0) / 3600.0;
    Ok(if v[0] < 0.0 || (v[0] == 0.0 && v[0].is_sign_negative()) { v[0] - m } else { v[0] + m })
}

/// A place as typed: `-23.55, -46.63` (latitude, longitude; longitude first if the first is past 90),
/// `23°33'S 46°38'W`, `23 33 S 46 38 W`, `23S 333000 7394000` (UTM: zone and hemisphere, or a latitude band C-X),
/// `EPSG:4326 -46.63 -23.55`, `512000 7395000` (beyond degrees: the layer's CRS), `px 120 80` (a pixel).
pub fn parse_place(text: &str) -> Result<Place, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("type a place".into());
    }
    let lower = t.to_ascii_lowercase();
    let nums = |s: &str| -> Result<Vec<f64>, String> {
        tokens(s)?
            .into_iter()
            .map(|t| if let Token::Num(v) = t { Ok(v) } else { Err("x y: two numbers".into()) })
            .collect()
    };
    for p in ["pixel", "px"] {
        if let Some(rest) = lower.strip_prefix(p) {
            return match nums(rest)?[..] {
                [x, y] => Ok(Place::Pixel(x, y)),
                _ => Err("px column row".into()),
            };
        }
    }
    if let Some(rest) = lower.strip_prefix("epsg") {
        let rest = rest.trim_start_matches([':', ' ']);
        let (code, rest) = rest.split_at(rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len()));
        let code: u32 = code.parse().map_err(|_| "EPSG:code x y".to_string())?;
        return match nums(rest)?[..] {
            [x, y] => Ok(Place::Xy(Some(code), x, y)),
            _ => Err("EPSG:code x y".into()),
        };
    }
    // UTM: zone and a letter, easting, northing
    let rest = lower.strip_prefix("utm").unwrap_or(&lower).trim();
    let words: Vec<&str> =
        rest.split(|c: char| c.is_whitespace() || c == ',' || c == ';').filter(|w| !w.is_empty()).collect();
    let utm = match words[..] {
        [z, e, n] if z.len() >= 2 => Some((&z[..z.len() - 1], &z[z.len() - 1..], e, n)),
        [z, l, e, n] => Some((z, l, e, n)),
        _ => None,
    };
    if let Some((z, l, e, n)) = utm
        && let Ok(zone) = z.parse::<u32>()
        && let Some(letter) = l.chars().next().filter(|c| c.is_ascii_alphabetic() && l.len() == 1)
        && let (Ok(e), Ok(n)) = (e.parse::<f64>(), n.parse::<f64>())
    {
        if !(1..=60).contains(&zone) {
            return Err("a UTM zone goes from 1 to 60".into());
        }
        // N and S the hemisphere (23S: zone 23 south); another letter a latitude band (C to M south)
        let south = match letter {
            'n' => false,
            's' => true,
            'c'..='m' => true,
            'p'..='x' => false,
            _ => return Err(format!("not a UTM letter: {}", letter.to_ascii_uppercase())),
        };
        return Ok(Place::Xy(Some(if south { 32700 } else { 32600 } + zone), e, n));
    }
    // degrees: two groups of numbers, each with its hemisphere or not
    let toks = tokens(t)?;
    let hemis = toks.iter().filter(|t| matches!(t, Token::Hemi(_))).count();
    let mut groups: Vec<(Vec<f64>, Option<char>)> = Vec::new();
    if hemis == 0 {
        let v: Vec<f64> = toks.iter().map(|t| if let Token::Num(v) = t { *v } else { 0.0 }).collect();
        if !v.len().is_multiple_of(2) || v.is_empty() || v.len() > 6 {
            return Err("two coordinates: latitude and longitude, or x and y".into());
        }
        let (a, b) = v.split_at(v.len() / 2);
        groups.push((a.to_vec(), None));
        groups.push((b.to_vec(), None));
    } else {
        let before = matches!(toks.first(), Some(Token::Hemi(_)));
        let mut cur: (Vec<f64>, Option<char>) = (vec![], None);
        for tk in toks {
            match tk {
                Token::Num(v) => cur.0.push(v),
                Token::Hemi(h) if before => {
                    if cur.1.is_some() {
                        groups.push(std::mem::take(&mut cur));
                    }
                    cur.1 = Some(h);
                }
                Token::Hemi(h) => {
                    cur.1 = Some(h);
                    groups.push(std::mem::take(&mut cur));
                }
            }
        }
        if !cur.0.is_empty() || cur.1.is_some() {
            groups.push(cur);
        }
    }
    let [a, b] = &groups[..] else { return Err("two coordinates: latitude and longitude".into()) };
    let (va, vb) = (degrees(&a.0)?, degrees(&b.0)?);
    let signed = |v: f64, h: Option<char>| if matches!(h, Some('S' | 'W')) { -v.abs() } else { v };
    let (va, vb) = (signed(va, a.1), signed(vb, b.1));
    let is_lat = |h: Option<char>| h.map(|h| h == 'N' || h == 'S');
    let (lon, lat) = match (is_lat(a.1), is_lat(b.1)) {
        (Some(true), Some(false)) | (Some(true), None) | (None, Some(false)) => (vb, va),
        (Some(false), Some(true)) | (Some(false), None) | (None, Some(true)) => (va, vb),
        (Some(x), Some(_)) => return Err(format!("two {}", if x { "latitudes" } else { "longitudes" })),
        (None, None) if va.abs() <= 90.0 && vb.abs() <= 180.0 => (vb, va),
        (None, None) if va.abs() <= 180.0 && vb.abs() <= 90.0 => (va, vb),
        (None, None) if a.0.len() == 1 => return Ok(Place::Xy(None, va, vb)),
        (None, None) => return Err("out of the degrees' range".into()),
    };
    if lat.abs() > 90.0 || lon.abs() > 180.0 {
        return Err("a latitude goes to 90, a longitude to 180".into());
    }
    Ok(Place::LonLat(lon, lat))
}

/// A view saved by name: its centre and width (longitude, latitude and metres; a layer without a CRS: source
/// pixels), the date shown and the layers shown.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedView {
    pub name: String,
    pub geo: bool,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub date: String,
    pub layers: Vec<String>,
}

impl SavedView {
    /// As the settings keep it: tab-separated, the names escaped.
    pub fn to_text(&self) -> String {
        let layers: Vec<String> = self.layers.iter().map(|l| esc(l)).collect();
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            esc(&self.name),
            if self.geo { "geo" } else { "px" },
            self.x,
            self.y,
            self.width,
            esc(&self.date),
            layers.join("|")
        )
    }

    pub fn from_text(s: &str) -> Option<SavedView> {
        let f: Vec<&str> = s.split('\t').collect();
        let [name, geo, x, y, width, date, layers] = f[..] else { return None };
        Some(SavedView {
            name: unesc(name),
            geo: geo == "geo",
            x: x.parse().ok()?,
            y: y.parse().ok()?,
            width: width.parse().ok().filter(|w: &f64| *w > 0.0)?,
            date: unesc(date),
            layers: layers.split('|').filter(|l| !l.is_empty()).map(unesc).collect(),
        })
    }
}

/// Go to's window, the mark of the place gone to, the minimap's state.
#[derive(Default)]
pub struct NavUi {
    pub goto_open: bool,
    pub goto_text: String,
    goto_focus: bool,
    pub goto_pin: bool,
    /// the place gone to: the cube, its source pixel, what was typed, when
    pub mark: Option<(u64, [f64; 2], String, Instant)>,
    /// the name a view is saved under (its menu's field)
    view_name: String,
    /// what the minimap's target holds, and a drag on it
    minimap_key: Option<(u64, u64, [u32; 2])>,
    minimap_drag: bool,
}

impl State {
    pub(crate) fn toggle_goto(&mut self) {
        self.nav.goto_open = !self.nav.goto_open && self.session.is_some();
        self.nav.goto_focus = self.nav.goto_open;
    }

    /// The active layer's CRS, axes as x (east), y (north).
    fn layer_srs(&self) -> Option<gdal::spatial_ref::SpatialRef> {
        let s = self.session.as_ref()?;
        let mut srs = gdal::spatial_ref::SpatialRef::from_wkt(&s.info.crs_wkt).ok()?;
        srs.set_axis_mapping_strategy(gdal::spatial_ref::AxisMappingStrategy::TraditionalGisOrder);
        Some(srs)
    }

    /// A place's source pixel in the active layer (fractional: its centre is +0.5).
    pub(crate) fn place_pixel(&self, p: Place) -> Result<[f64; 2], String> {
        use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
        let s = self.session.as_ref().ok_or("no layer is open")?;
        let info = &s.info;
        let (x, y) = match p {
            Place::Pixel(x, y) => return Ok([x, y]),
            Place::Xy(None, x, y) => (x, y),
            Place::LonLat(..) | Place::Xy(Some(_), ..) => {
                let to = self.layer_srs().ok_or("the layer has no CRS")?;
                let (code, x, y) = match p {
                    Place::LonLat(lon, lat) => (4326, lon, lat),
                    Place::Xy(Some(c), x, y) => (c, x, y),
                    _ => unreachable!(),
                };
                let mut from = SpatialRef::from_epsg(code).map_err(|_| format!("EPSG:{code} is not known"))?;
                from.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
                let tr = CoordTransform::new(&from, &to).map_err(|e| format!("no transformation: {e}"))?;
                let (mut xs, mut ys, mut zs) = ([x], [y], [0.0]);
                tr.transform_coords(&mut xs, &mut ys, &mut zs).map_err(|_| "outside the layer's CRS".to_string())?;
                (xs[0], ys[0])
            }
        };
        let (px, py) = info.geo_to_pixel(x, y).ok_or("the layer is not georeferenced: px column row")?;
        if !px.is_finite() || !py.is_finite() {
            return Err("outside the layer's CRS".into());
        }
        Ok([px, py])
    }

    /// What Go to will do with the text: where it lands, or why not.
    fn goto_preview(&self) -> Result<(Place, [f64; 2]), String> {
        let p = parse_place(&self.nav.goto_text)?;
        Ok((p, self.place_pixel(p)?))
    }

    /// The map centred on a source pixel (zoomed in to a screen point a pixel at least), marked; a pin there if
    /// asked and it is on the layer.
    pub(crate) fn go_to_pixel(&mut self, at: [f64; 2], label: &str, pin: bool) {
        let Some(s) = &self.session else { return };
        let (id, w, h) = (s.info.id, s.info.width as f64, s.info.height as f64);
        let Some(g) = self.grid() else { return };
        let now = self.map.scale / g.fx;
        self.view_at(at[0], at[1], now.max(1.0));
        self.map.touched = true;
        self.nav.mark = Some((id, at, label.to_string(), Instant::now()));
        if pin && (0.0..w).contains(&at[0]) && (0.0..h).contains(&at[1]) {
            self.add_pin(at[0] as usize, at[1] as usize);
        }
    }

    /// Go to's window (Ctrl+G).
    pub(crate) fn goto_ui(&mut self, ctx: &egui::Context) {
        if !self.nav.goto_open {
            return;
        }
        if self.session.is_none() {
            self.nav.goto_open = false;
            return;
        }
        let mut open = true;
        let mut go = false;
        let preview = self.goto_preview();
        egui::Window::new("Go to")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_TOP, vec2(0.0, 60.0))
            .show(ctx, |ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.nav.goto_text)
                        .hint_text("-23.55, -46.63   23°33'S 46°38'W   23S 333000 7394000")
                        .desired_width(360.0),
                );
                if std::mem::take(&mut self.nav.goto_focus) {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
                match &preview {
                    Ok((p, px)) => {
                        let what = match p {
                            Place::LonLat(lon, lat) => format!("lat {lat:.6}, lon {lon:.6}"),
                            Place::Xy(Some(c), x, y) => format!("EPSG:{c} x {x:.2}, y {y:.2}"),
                            Place::Xy(None, x, y) => format!("x {x:.2}, y {y:.2} in the layer's CRS"),
                            Place::Pixel(x, y) => format!("pixel {x}, {y}"),
                        };
                        let on = self.session.as_ref().is_some_and(|s| {
                            (0.0..s.info.width as f64).contains(&px[0]) && (0.0..s.info.height as f64).contains(&px[1])
                        });
                        ui.weak(format!(
                            "{what} -> pixel ({:.0}, {:.0}){}",
                            px[0].floor(),
                            px[1].floor(),
                            if on { "" } else { ", off the layer" }
                        ));
                    }
                    Err(_) if self.nav.goto_text.trim().is_empty() => {
                        ui.weak(
                            "Latitude, longitude (decimal or ° ' \"), UTM zone + N/S, EPSG:code x y, x y, px col row",
                        );
                    }
                    Err(e) => {
                        ui.colored_label(crate::theme::warning(), e);
                    }
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.nav.goto_pin, "Drop a pin there");
                    if ui.add_enabled(preview.is_ok(), egui::Button::new("Go")).clicked() {
                        go = true;
                    }
                });
            });
        if go && let Ok((_, px)) = preview {
            let label = self.nav.goto_text.trim().to_string();
            let pin = self.nav.goto_pin;
            self.go_to_pixel(px, &label, pin);
            open = false;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        self.nav.goto_open = open;
    }

    /// The place gone to, on a map canvas: a ring and a cross that fade, what was typed beside them.
    pub(crate) fn goto_mark(&mut self, painter: &egui::Painter, rect: Rect) {
        let Some((id, at, label, when)) = self.nav.mark.clone() else { return };
        let left = 1.0 - when.elapsed().as_secs_f32() / MARK_SECONDS;
        let Some(g) = self.grid().filter(|_| left > 0.0 && self.session.as_ref().is_some_and(|s| s.info.id == id))
        else {
            self.nav.mark = None;
            return;
        };
        let c = self.map.screen_of(rect, at[0] / g.fx, at[1] / g.fy);
        let a = (left * 2.0).min(1.0);
        let col = crate::theme::accent().gamma_multiply(a);
        let dark = Color32::from_black_alpha((180.0 * a) as u8);
        for (w, color) in [(4.0, dark), (2.0, col)] {
            painter.circle_stroke(c, 14.0, Stroke::new(w, color));
            for d in [vec2(1.0, 0.0), vec2(0.0, 1.0)] {
                painter.line_segment([c - d * 24.0, c - d * 8.0], Stroke::new(w, color));
                painter.line_segment([c + d * 8.0, c + d * 24.0], Stroke::new(w, color));
            }
        }
        let font = crate::map::body_font(painter);
        let galley = painter.layout_no_wrap(label, font, Color32::WHITE.gamma_multiply(a));
        let tp = c + vec2(20.0, -galley.size().y - 14.0);
        painter.rect_filled(Rect::from_min_size(tp, galley.size()).expand(3.0), 3.0, dark);
        painter.galley(tp, galley, Color32::WHITE);
        painter.ctx().request_repaint_after(Duration::from_millis(50));
    }

    /// Metres of ground per source pixel of the active layer at its pixel `at` (None: no CRS, or not linear).
    fn metres_per_pixel(&self, at: [f64; 2]) -> Option<f64> {
        let s = self.session.as_ref()?;
        let gt = s.info.geo_transform?;
        let size = gt[1].hypot(gt[4]);
        let srs = self.layer_srs()?;
        if srs.is_geographic() {
            let (_, lat) = s.info.pixel_to_geo(at[0], at[1])?;
            Some(size * 111_320.0 * lat.to_radians().cos())
        } else {
            Some(size * srs.linear_units())
        }
    }

    /// The view now (the main map's): its centre, width, date and layers shown, under `name`.
    pub(crate) fn current_view(&mut self, name: &str) -> Option<SavedView> {
        let g = self.grid()?;
        let centre = [self.map.center[0] * g.fx, self.map.center[1] * g.fy];
        let width_px = f64::from(self.map.canvas[0].max(1.0)) / self.map.scale * g.fx;
        let s = self.session.as_ref()?;
        let date = s.info.layers.get(self.display.t).map_or(String::new(), |l| l.label.clone());
        let geo = s.info.pixel_to_geo(centre[0], centre[1]);
        let mut layers: Vec<String> =
            self.layers.iter().filter(|l| l.visible && !l.canvas).map(|l| l.name.clone()).collect();
        layers.extend(self.vectors.iter().filter(|v| v.visible).map(|v| v.name.clone()));
        let lon_lat = geo.and_then(|g| self.status_lon_lat(g));
        let metres = self.metres_per_pixel(centre);
        let (geo, x, y, width) = match (lon_lat, metres) {
            (Some((lon, lat)), Some(m)) => (true, lon, lat, width_px * m),
            _ => (false, centre[0], centre[1], width_px),
        };
        Some(SavedView { name: name.to_string(), geo, x, y, width, date, layers })
    }

    /// Back to a saved view: the place and width on the active layer, its date (by its label) and its layers shown
    /// (when any of them is open). Err: why not.
    pub(crate) fn restore_view(&mut self, v: &SavedView) -> Result<(), String> {
        let centre = if v.geo { self.place_pixel(Place::LonLat(v.x, v.y))? } else { [v.x, v.y] };
        let width_px =
            if v.geo { v.width / self.metres_per_pixel(centre).ok_or("the active layer has no CRS")? } else { v.width };
        let canvas = f64::from(self.map.canvas[0].max(1.0));
        self.view_at(centre[0], centre[1], canvas / width_px.max(1e-9));
        self.map.touched = true;
        if let Some(t) = self.session.as_ref().and_then(|s| s.info.layers.iter().position(|l| l.label == v.date)) {
            self.set_date(t);
        }
        let open = |n: &String| v.layers.contains(n);
        if self.layers.iter().any(|l| open(&l.name)) || self.vectors.iter().any(|x| open(&x.name)) {
            for l in self.layers.iter_mut().filter(|l| !l.canvas) {
                l.visible = open(&l.name);
            }
            for x in &mut self.vectors {
                x.visible = open(&x.name);
            }
        }
        self.map.dirty = true;
        Ok(())
    }

    /// The saved views (the settings').
    pub(crate) fn saved_views(&self) -> Vec<SavedView> {
        self.settings.s.saved_views.iter().filter_map(|t| SavedView::from_text(t)).collect()
    }

    /// The view now saved as `name` (one of that name replaced).
    pub(crate) fn save_view(&mut self, name: &str) -> bool {
        let name = name.trim();
        let Some(v) = self.current_view(name).filter(|_| !name.is_empty()) else { return false };
        let views = &mut self.settings.s.saved_views;
        views.retain(|t| SavedView::from_text(t).is_none_or(|x| x.name != name));
        views.push(v.to_text());
        self.settings.changed();
        true
    }

    pub(crate) fn delete_view(&mut self, name: &str) {
        self.settings.s.saved_views.retain(|t| SavedView::from_text(t).is_none_or(|x| x.name != name));
        self.settings.changed();
    }

    /// View > Saved views: save the view now under a name; each saved one (a click goes back to it, x deletes it).
    pub(crate) fn saved_views_menu(&mut self, ui: &mut Ui) {
        let has = self.session.is_some();
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.nav.view_name).hint_text("name").desired_width(120.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let ok = has && !self.nav.view_name.trim().is_empty();
            if (ui.add_enabled(ok, egui::Button::new("Save view")).clicked() || (enter && ok))
                && self.save_view(&self.nav.view_name.clone())
            {
                self.nav.view_name.clear();
                ui.close();
            }
        })
        .response
        .on_hover_text("The area (centre and width, for any layer in any CRS), the date and the layers shown");
        let views = self.saved_views();
        if !views.is_empty() {
            ui.separator();
        }
        for v in views {
            ui.horizontal(|ui| {
                let tip = format!(
                    "{}{}{}",
                    if v.geo {
                        format!("lat {:.5}, lon {:.5}, {:.0} m wide", v.y, v.x, v.width)
                    } else {
                        format!("pixel ({:.0}, {:.0}), {:.0} px wide", v.x, v.y, v.width)
                    },
                    if v.date.is_empty() { String::new() } else { format!("\n{}", v.date) },
                    if v.layers.is_empty() { String::new() } else { format!("\n{}", v.layers.join(", ")) }
                );
                if ui.add_enabled(has, egui::Button::new(&v.name)).on_hover_text(tip).clicked() {
                    if let Err(e) = self.restore_view(&v) {
                        self.notify(&format!("Saved view {}", v.name), &e, true);
                    }
                    ui.close();
                }
                if ui.small_button("x").on_hover_text("Delete this saved view").clicked() {
                    self.delete_view(&v.name);
                }
            });
        }
    }

    /// The minimap's place on a map canvas: its bottom right corner, the active layer's shape.
    pub(crate) fn minimap_rect(&self, rect: Rect) -> Option<Rect> {
        let g = self.grid().filter(|_| self.settings.s.minimap && self.session.is_some())?;
        let k = MINIMAP_SIZE / (g.ow.max(g.oh) as f32);
        let size = vec2(g.ow as f32 * k, g.oh as f32 * k).max(vec2(24.0, 24.0));
        if size.x * 2.5 > rect.width() || size.y * 2.5 > rect.height() {
            return None;
        }
        Some(Rect::from_min_size(rect.max - size - vec2(12.0, 12.0), size))
    }

    /// A press or a drag on the minimap: the map centred on that point (true: the minimap had the pointer).
    pub(crate) fn minimap_input(&mut self, ui: &Ui, rect: Rect, resp: &egui::Response) -> bool {
        let Some(mr) = self.minimap_rect(rect) else { return false };
        let Some(g) = self.grid() else { return false };
        let p = resp.interact_pointer_pos().or(resp.hover_pos());
        let over = p.is_some_and(|p| mr.contains(p));
        if resp.drag_started() && over {
            self.nav.minimap_drag = true;
        }
        let pressed = over && (resp.clicked() || ui.input(|i| i.pointer.primary_pressed()));
        if self.nav.minimap_drag && !resp.dragged() && !resp.drag_started() {
            self.nav.minimap_drag = false;
            return true;
        }
        if !self.nav.minimap_drag && !pressed {
            return false;
        }
        if let Some(p) = p {
            let f = ((p - mr.min) / mr.size()).clamp(vec2(0.0, 0.0), vec2(1.0, 1.0));
            self.minimap_go([f64::from(f.x) * g.ow, f64::from(f.y) * g.oh]);
        }
        true
    }

    /// The map centred on an overview point (the minimap's).
    pub(crate) fn minimap_go(&mut self, at: [f64; 2]) {
        self.map.center = at;
        self.map.touched = true;
        self.map.dirty = true;
    }

    /// The minimap over a map canvas: the active layer whole (drawn again only when what it shows changes), the
    /// view's rectangle on it.
    pub(crate) fn draw_minimap(&mut self, painter: &egui::Painter, rect: Rect, ppp: f32) {
        let Some(mr) = self.minimap_rect(rect) else { return };
        let Some(g) = self.grid() else { return };
        let size = [(mr.width() * ppp).round() as u32, (mr.height() * ppp).round() as u32];
        let Some((texture, remade)) = self.target_texture(MINIMAP_SLOT, size) else { return };
        if let Some(s) = self.session.as_ref()
            && let Some(cube) = &s.cube
        {
            let mut params = self.display.layer_params(s);
            if self.classes.on() && params.mode == janus_gpu::Mode::Value {
                params.class_lut = self.classes.lut.clone();
            }
            let cmap = self.display.cmap[params.mode as usize];
            let key = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (params.mode, params.layers, params.range.map(f32::to_bits), cmap).hash(&mut h);
                params.class_lut.as_ref().map(|l| std::sync::Arc::as_ptr(l) as usize).hash(&mut h);
                s.s.overview.layers_done().hash(&mut h);
                (s.info.id, h.finish(), size)
            };
            if (remade || self.nav.minimap_key != Some(key))
                && self.display.ready(s, &params)
                && let Some(gpu) = self.gpu.as_mut()
            {
                let mut f = gpu.begin_map(MINIMAP_SLOT, size, crate::map::MAP_BACKGROUND);
                gpu.draw_cube(&mut f, cube, [0.0, 0.0, size[0] as f32, size[1] as f32], &params, cmap, 1.0);
                gpu.end_map(f);
                self.nav.minimap_key = Some(key);
            }
        }
        painter.rect_filled(mr.expand(2.0), 3.0, Color32::from_black_alpha(170));
        painter.image(texture, mr, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        // the view: the canvas' corners in overview pixels
        let m = &self.map;
        let to_mini = |x: f64, y: f64| mr.min + vec2((x / g.ow) as f32 * mr.width(), (y / g.oh) as f32 * mr.height());
        let a = m.image_at(rect, rect.min);
        let b = m.image_at(rect, rect.max);
        let view = Rect::from_two_pos(to_mini(a[0], a[1]), to_mini(b[0], b[1])).intersect(mr);
        if view.is_positive() {
            let v = if view.width() < 4.0 && view.height() < 4.0 {
                Rect::from_center_size(view.center(), vec2(6.0, 6.0))
            } else {
                view
            };
            painter.rect_stroke(v, 0.0, Stroke::new(3.0, Color32::from_black_alpha(160)), egui::StrokeKind::Middle);
            painter.rect_stroke(v, 0.0, Stroke::new(1.5, crate::theme::accent()), egui::StrokeKind::Middle);
        }
        painter.rect_stroke(mr, 0.0, Stroke::new(1.0, Color32::from_white_alpha(90)), egui::StrokeKind::Outside);
    }

    pub(crate) fn toggle_minimap(&mut self) {
        self.settings.s.minimap = !self.settings.s.minimap;
        self.settings.changed();
        if !self.settings.s.minimap {
            if let Some(g) = self.gpu.as_mut() {
                g.release_target(MINIMAP_SLOT);
            }
            self.map_textures.remove(&MINIMAP_SLOT);
            self.nav.minimap_key = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lon_lat(s: &str) -> (f64, f64) {
        match parse_place(s) {
            Ok(Place::LonLat(lon, lat)) => (lon, lat),
            other => panic!("{s}: {other:?}"),
        }
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6
    }

    #[test]
    fn places_as_typed() {
        let want = (-46.63, -23.55);
        for s in [
            "-23.55, -46.63",
            "-23.55 -46.63",
            "-23,55 -46,63",
            "23.55S 46.63W",
            "S 23.55 W 46.63",
            "46.63 W, 23.55 S",
            "-46.63 -23.55",
        ] {
            // the last: a first number past 90 is not a latitude... unless both fit: lat, lon
            if s == "-46.63 -23.55" {
                assert!(near(lon_lat(s), (-23.55, -46.63)), "{s}");
                continue;
            }
            assert!(near(lon_lat(s), want), "{s}: {:?}", lon_lat(s));
        }
        assert!(near(lon_lat("-120.5 45.25"), (-120.5, 45.25)));
        let dms = lon_lat("23°33'00\"S 46°37'48\"W");
        assert!(near(dms, (-(46.0 + 37.0 / 60.0 + 48.0 / 3600.0), -(23.0 + 33.0 / 60.0))), "{dms:?}");
        assert!(near(lon_lat("23 33 S 46 37.8 O"), (-(46.0 + 37.8 / 60.0), -(23.0 + 33.0 / 60.0))));
        assert!(near(lon_lat("-23 33, -46 37.8"), (-(46.0 + 37.8 / 60.0), -(23.0 + 33.0 / 60.0))));
        assert_eq!(parse_place("23S 333000 7394000"), Ok(Place::Xy(Some(32723), 333000.0, 7394000.0)));
        assert_eq!(parse_place("utm 23 K 333000 7394000"), Ok(Place::Xy(Some(32723), 333000.0, 7394000.0)));
        assert_eq!(parse_place("18N 500000, 4649776"), Ok(Place::Xy(Some(32618), 500000.0, 4649776.0)));
        assert_eq!(parse_place("EPSG:3857 -5190000 -2700000"), Ok(Place::Xy(Some(3857), -5190000.0, -2700000.0)));
        assert_eq!(parse_place("512000.5 7395000"), Ok(Place::Xy(None, 512000.5, 7395000.0)));
        assert_eq!(parse_place("px 120 80"), Ok(Place::Pixel(120.0, 80.0)));
        for bad in ["", "hello", "23", "91N 10E", "10N 20N", "23 70 S 46 W", "61S 1 2"] {
            assert!(parse_place(bad).is_err(), "{bad}: {:?}", parse_place(bad));
        }
    }

    #[test]
    fn saved_view_text() {
        let v = SavedView {
            name: "Rio\tdelta | 2".into(),
            geo: true,
            x: -43.2,
            y: -22.9,
            width: 12500.0,
            date: "2010-07-01".into(),
            layers: vec!["NDVI|x".into(), "roads.gpkg".into()],
        };
        assert_eq!(SavedView::from_text(&v.to_text()), Some(v));
    }
}
