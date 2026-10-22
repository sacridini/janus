//! RGB composites of three bands per date (src/app_composite.cpp), for series with several bands in each date's
//! file (Landsat or Sentinel-2 surface reflectance): true colour, colour infrared, SWIR... drawn instead of the
//! series' quantity, while the cube, the chart, the cursor and the statistics keep showing that quantity.
//!
//! Each date's three bands are read in the background at the overview's size (one date at a time, the date shown
//! first; a few dates kept on the GPU), and, zoomed past the overview, the visible window again at the screen's
//! resolution. The stretch (2-98 % of each band) is set from the first image and kept for every date, so dates
//! compare and play as a film.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use egui::{ComboBox, Slider, Ui};
use janus_cube::roles::guess_band_roles;
use janus_cube::stats::sample_percentiles;
use janus_cube::{CubeInfo, CubeReader};
use janus_gpu::{Gpu, GpuTexture, MapFrame};

use crate::app::State;
use crate::theme;

/// RGBA images of dates kept on the GPU.
const CACHE_BYTES: usize = 256 << 20;
const MAX_DETAIL_SIDE: usize = 4096;

fn now() -> f64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn mix_key(h: u64, v: u64) -> u64 {
    (h ^ v.wrapping_add(0x9e3779b97f4a7c15).wrapping_add(h << 6).wrapping_add(h >> 2)).wrapping_mul(1099511628211)
}

/// Band values (3 × w × h) -> RGBA8 through the stretch; transparent where a band has no data.
fn to_rgba(v: &[f32], n: usize, lo: &[f32; 3], hi: &[f32; 3]) -> Vec<u8> {
    let mut out = vec![0u8; n * 4];
    for i in 0..n {
        let mut ok = true;
        for c in 0..3 {
            let x = v[c * n + i];
            if !x.is_finite() {
                ok = false;
                break;
            }
            let f = ((x - lo[c]) / (hi[c] - lo[c])).clamp(0.0, 1.0);
            out[i * 4 + c] = (f * 255.0).round() as u8;
        }
        if ok {
            out[i * 4 + 3] = 255;
        } else {
            out[i * 4..i * 4 + 4].fill(0);
        }
    }
    out
}

/// A preset by band roles (names guessed), else by wavelength when the bands have one; 0 = not found.
pub struct Preset {
    pub name: &'static str,
    pub bands: [i32; 3],
}

pub fn presets(info: &CubeInfo) -> Vec<Preset> {
    const WANTS: [(&str, [&str; 3], [f64; 3]); 5] = [
        ("True colour (red, green, blue)", ["red", "green", "blue"], [650.0, 560.0, 480.0]),
        ("Colour infrared (NIR, red, green)", ["nir", "red", "green"], [860.0, 650.0, 560.0]),
        ("SWIR: burns, soil (SWIR2, NIR, red)", ["swir2", "nir", "red"], [2200.0, 860.0, 650.0]),
        ("Agriculture (SWIR1, NIR, blue)", ["swir1", "nir", "blue"], [1610.0, 860.0, 480.0]),
        ("Land / water (NIR, SWIR1, red)", ["nir", "swir1", "red"], [860.0, 1610.0, 650.0]),
    ];
    let roles = guess_band_roles(info);
    WANTS
        .iter()
        .map(|(name, want, nm)| {
            let mut bands = [0; 3];
            for c in 0..3 {
                if let Some(&b) = roles.get(want[c]) {
                    bands[c] = b;
                } else if !info.band_nm.is_empty() {
                    // nm: no band that far is that colour
                    let mut d = 60.0;
                    for (b, &w) in info.band_nm.iter().enumerate() {
                        if (w - nm[c]).abs() < d {
                            d = (w - nm[c]).abs();
                            bands[c] = b as i32 + 1;
                        }
                    }
                }
            }
            Preset { name, bands }
        })
        .collect()
}

/// An image on the GPU: a date's (the overview's size) or the detail (the visible window).
struct Img {
    tex: GpuTexture,
    key: u64,
    w: usize,
    h: usize,
    /// the source window it covers
    win: [f64; 4],
    used: f64,
}

/// The values of the image last read (the cursor's band values, a stretch to this image).
pub struct Values {
    t: usize,
    w: usize,
    h: usize,
    detail: bool,
    win: [f64; 4],
    v: Vec<f32>,
}

struct Read {
    t: usize,
    detail: bool,
    key: u64,
    win: [f64; 4],
    w: usize,
    h: usize,
    rx: Receiver<Option<Vec<f32>>>,
}

#[derive(Default)]
pub struct BandComposite {
    pub on: bool,
    pub bands: [i32; 3],
    /// a date of its own while the time bar moves the series (chart, cursor)
    pub fixed_date: bool,
    pub date: usize,
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    stretched: bool,
    /// the bands and the stretch: images made with another key are read again
    key: u64,
    imgs: HashMap<usize, Img>,
    detail: Option<Img>,
    detail_t: Option<usize>,
    want_detail: u64,
    want_since: f64,
    last: Option<Values>,
    reading: Option<Read>,
    pub error: String,
    /// the cube it was made for
    cube_id: u64,
}

impl BandComposite {
    /// Bands of the series `cube_id` (1-based) as red, green and blue, on.
    pub fn of(cube_id: u64, bands: [i32; 3]) -> Self {
        BandComposite { on: true, bands, cube_id, ..Default::default() }
    }

    fn clear_images(&mut self) {
        self.imgs.clear();
        self.detail = None;
        self.detail_t = None;
        self.want_detail = 0;
        self.last = None;
    }

    /// Other bands: a new stretch, every image read again.
    fn restart(&mut self) {
        self.stretched = false;
        self.key = mix_key(self.key, 11);
        self.clear_images();
    }

    /// The date drawn when the map shows t.
    pub fn date_of(&self, t: usize, n: usize) -> usize {
        (if self.fixed_date { self.date } else { t }).min(n - 1)
    }

    pub fn ready(&self, t: usize, n: usize) -> bool {
        !self.on || self.imgs.get(&self.date_of(t, n)).is_some_and(|i| i.key == self.key)
    }

    pub fn title(&self, info: &CubeInfo, t: usize) -> String {
        let name = |b: i32| {
            info.band_names.get((b - 1).max(0) as usize).filter(|_| b >= 1).cloned().unwrap_or(format!("band {b}"))
        };
        format!(
            "{}  |  RGB: {}, {}, {}",
            info.layers[self.date_of(t, info.t())].label,
            name(self.bands[0]),
            name(self.bands[1]),
            name(self.bands[2])
        )
    }

    /// The band values under the cursor (a source pixel's centre), from the image last read.
    pub fn status_at(&self, ax: f64, ay: f64, t: usize, n: usize) -> String {
        let Some(v) = &self.last else { return String::new() };
        let [x0, y0, x1, y1] = v.win;
        if v.t != self.date_of(t, n) || v.v.is_empty() || ax < x0 || ay < y0 || ax >= x1 || ay >= y1 {
            return String::new();
        }
        let i = (((ax - x0) / (x1 - x0) * v.w as f64) as usize).min(v.w - 1);
        let j = (((ay - y0) / (y1 - y0) * v.h as f64) as usize).min(v.h - 1);
        let (np, p) = (v.w * v.h, j * v.w + i);
        let g = |x: f32| janus_cube::multidim::format_g(f64::from(x), 5);
        format!(
            "  |  RGB {}, {}, {}{}",
            g(v.v[p]),
            g(v.v[np + p]),
            g(v.v[2 * np + p]),
            if v.detail { "" } else { " (overview)" }
        )
    }

    /// The source window of the detail image drawn at t, if one is.
    pub fn detail_window(&self, t: usize, n: usize) -> Option<[f64; 4]> {
        let d = self.date_of(t, n);
        (self.detail_t == Some(d)).then_some(())?;
        self.detail.as_ref().filter(|i| i.key == self.key).map(|i| i.win)
    }

    /// The images of date `d` (the map shows t), the detail over the overview's: f(texture, source window).
    pub fn draw(&mut self, t: usize, n: usize, mut f: impl FnMut(&GpuTexture, [f64; 4])) {
        let d = self.date_of(t, n);
        let now = now();
        if let Some(i) = self.imgs.get_mut(&d) {
            f(&i.tex, i.win); // pixels as they are, like the cube
            i.used = now;
        }
        if self.detail_t == Some(d)
            && let Some(i) = self.detail.as_mut().filter(|i| i.key == self.key)
        {
            f(&i.tex, i.win);
            i.used = now;
        }
    }
}

impl State {
    /// Once per frame: an image read comes to the GPU, the next is asked for.
    pub(crate) fn pump_composite(&mut self, ctx: &egui::Context) {
        let (Some(s), Some(gpu), Some(c)) = (&self.session, &self.gpu, self.composite.as_mut()) else { return };
        let info = &s.info;
        if c.cube_id != info.id {
            // another series, or reopened with another band selection
            *c = BandComposite { cube_id: info.id, ..Default::default() };
            self.map.dirty = true;
            return;
        }
        if !c.on {
            return;
        }
        let now = now();
        // a read that ended: its image (made with the stretch of now)
        if let Some(r) = &c.reading {
            match r.rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                res => {
                    let r = c.reading.take().unwrap();
                    match res.ok().flatten() {
                        None => c.error = "the bands could not be read".into(),
                        Some(v) if r.key == c.key || !c.stretched => {
                            c.error.clear();
                            let n = r.w * r.h;
                            if !c.stretched {
                                // 2-98 % of each band, kept for every date
                                for ch in 0..3 {
                                    (c.lo[ch], c.hi[ch]) =
                                        sample_percentiles(&v[ch * n..(ch + 1) * n], 400_000, 0.02, 0.98);
                                }
                                c.stretched = true;
                                c.key = mix_key(c.key, 7);
                            }
                            let rgba = to_rgba(&v, n, &c.lo, &c.hi);
                            let img = Img {
                                tex: gpu.create_image(r.w as u32, r.h as u32, &rgba),
                                key: c.key,
                                w: r.w,
                                h: r.h,
                                win: r.win,
                                used: now,
                            };
                            if r.detail {
                                c.detail = Some(img);
                                c.detail_t = Some(r.t);
                            } else {
                                c.imgs.insert(r.t, img);
                            }
                            c.last = Some(Values { t: r.t, w: r.w, h: r.h, detail: r.detail, win: r.win, v });
                            self.map.dirty = true;
                        }
                        Some(_) => {} // read with a stretch no longer wanted
                    }
                }
            }
        }
        if c.reading.is_some() || s.s.defer_random_reads() {
            return; // one read at a time; an HDD builds its overview
        }
        let nb = info.bands_per_date as i32;
        if c.bands.iter().any(|&b| b < 1 || b > nb) {
            c.error = "pick three bands".into();
            return;
        }
        let n = info.t();
        let want = c.date_of(self.display.t, n);
        let start = |c: &mut BandComposite, t: usize, detail: bool, win: [f64; 4], w: usize, h: usize| {
            let (tx, rx) = std::sync::mpsc::channel();
            let (info, bands, ctx) = (info.clone(), c.bands, ctx.clone());
            let (ix, iy) = (win[0] as usize, win[1] as usize);
            let (iw, ih) = ((win[2] - win[0]) as usize, (win[3] - win[1]) as usize);
            std::thread::spawn(move || {
                let mut reader = CubeReader::new(info);
                let n = w * h;
                let mut v = vec![0.0f32; 3 * n];
                let ok = (0..3).all(|ch| {
                    reader.read_band_window(t, bands[ch], (ix, iy, iw, ih), &mut v[ch * n..(ch + 1) * n], (w, h))
                });
                let _ = tx.send(ok.then_some(v));
                ctx.request_repaint();
            });
            c.reading = Some(Read { t, detail, key: c.key, win, w, h, rx });
        };
        let mut started = false;
        match c.imgs.get_mut(&want) {
            Some(i) if i.key == c.key => i.used = now,
            _ => {
                let ov = &s.s.overview;
                start(c, want, false, [0.0, 0.0, info.width as f64, info.height as f64], ov.w, ov.h);
                started = true;
            }
        }
        // zoomed in past the overview: the visible window at the screen's resolution, once the view has been
        // still for a moment
        if !started && let Some(v) = &self.map.last_view {
            let factor = info.width as f64 / s.s.overview.w.max(1) as f64;
            let ps = v.scale;
            let win = [
                (v.x0.floor().max(0.0) as usize).min(info.width),
                (v.y0.floor().max(0.0) as usize).min(info.height),
                (v.x1.ceil().max(0.0) as usize).min(info.width),
                (v.y1.ceil().max(0.0) as usize).min(info.height),
            ];
            if ps * factor > 1.5 && win[2] > win[0] && win[3] > win[1] {
                let (ww, wh) = (win[2] - win[0], win[3] - win[1]);
                let k = 1f64.min(ps).min(MAX_DETAIL_SIDE as f64 / ww.max(wh) as f64);
                let (bw, bh) = (1.max((ww as f64 * k).ceil() as usize), 1.max((wh as f64 * k).ceil() as usize));
                let key = mix_key(
                    mix_key(mix_key(mix_key(c.key, want as u64), win[0] as u64), win[1] as u64),
                    (win[2] as u64 * 65536 + win[3] as u64).wrapping_add(bw as u64 * 7919),
                );
                let winf = win.map(|v| v as f64);
                let have = c.detail_t == Some(want)
                    && c.detail.as_ref().is_some_and(|d| d.key == c.key && d.win == winf && d.w == bw);
                if key != c.want_detail {
                    c.want_detail = key;
                    c.want_since = now;
                } else if !have && now - c.want_since > 0.15 {
                    start(c, want, true, winf, bw, bh);
                }
            }
        }
        // dates not drawn lately go first when the cache is full
        let mut bytes: usize = c.imgs.values().map(|i| i.w * i.h * 4).sum();
        while bytes > CACHE_BYTES && c.imgs.len() > 1 {
            let Some((&t, i)) = c.imgs.iter().filter(|(t, _)| **t != want).min_by(|a, b| a.1.used.total_cmp(&b.1.used))
            else {
                break;
            };
            bytes -= i.w * i.h * 4;
            c.imgs.remove(&t);
        }
    }

    /// The composite's images into the map's frame (`to_px`: a source window to the target's pixels).
    pub(crate) fn draw_composite(
        c: &mut BandComposite,
        gpu: &Gpu,
        f: &mut MapFrame,
        t: usize,
        n: usize,
        alpha: f32,
        to_px: impl Fn([f64; 4]) -> [f32; 4],
    ) {
        // translucent: the overview's image only outside the detail's, blended once
        let detail = c.detail_window(t, n).filter(|_| alpha < 1.0).map(&to_px);
        c.draw(t, n, |tex, win| {
            let r = to_px(win);
            match detail.filter(|d| *d != r) {
                Some(d) => {
                    for clip in janus_gpu::uncovered(r, &[d]) {
                        f.clip = Some(clip);
                        gpu.draw_image(f, tex, r, alpha, None, true);
                    }
                    f.clip = None;
                }
                None => gpu.draw_image(f, tex, r, alpha, None, true),
            }
        });
    }

    /// The Display panel's section, for series with three bands or more per date.
    pub(crate) fn composite_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        if info.bands_per_date < 3 || info.spectral {
            return;
        }
        let t_now = self.display.t;
        let c = self.composite.get_or_insert_with(|| BandComposite { cube_id: info.id, ..Default::default() });
        let mut dirty = false;
        ui.separator();
        ui.strong("RGB composite");
        let presets = presets(&info);
        if ui
            .checkbox(&mut c.on, "Three bands of each date as RGB")
            .on_hover_text(format!(
                "Draws the map as a colour composite of three bands of each date (true colour,\n\
                 colour infrared...). The chart, the cursor, the statistics and the tools keep\n\
                 the series' quantity ({}).",
                info.selection_text()
            ))
            .changed()
        {
            if c.on && c.bands == [0, 0, 0] {
                c.bands = [3.min(info.bands_per_date as i32), 2, 1];
                if let Some(p) = presets.iter().find(|p| p.bands.iter().all(|&b| b > 0)) {
                    c.bands = p.bands;
                }
                c.restart();
            }
            dirty = true;
        }
        if c.on {
            ComboBox::from_id_salt("composite preset")
                .selected_text("Preset...")
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for p in &presets {
                        let ok = p.bands.iter().all(|&b| b > 0);
                        let label = if ok {
                            format!("{}  {}-{}-{}", p.name, p.bands[0], p.bands[1], p.bands[2])
                        } else {
                            p.name.to_string()
                        };
                        if ui.add_enabled(ok, egui::Button::selectable(false, label)).clicked() && p.bands != c.bands {
                            c.bands = p.bands;
                            c.restart();
                            dirty = true;
                        }
                    }
                })
                .response
                .on_hover_text("Bands found by their names (or wavelengths)");
            let name = |b: i32| {
                format!("{b}: {}", info.band_names.get((b - 1).max(0) as usize).filter(|_| b >= 1).map_or("?", |s| s))
            };
            for (ch, label) in ["R", "G", "B"].iter().enumerate() {
                let current = if c.bands[ch] > 0 { name(c.bands[ch]) } else { "?".into() };
                ComboBox::from_label(*label).selected_text(current).width(ui.available_width() - 24.0).show_ui(
                    ui,
                    |ui| {
                        for b in 1..=info.bands_per_date as i32 {
                            if ui.selectable_label(b == c.bands[ch], name(b)).clicked() && b != c.bands[ch] {
                                c.bands[ch] = b;
                                c.restart();
                                dirty = true;
                            }
                        }
                    },
                );
            }
            // the date: the map's (it follows the time bar and plays), or one of its own
            ui.horizontal(|ui| {
                if ui.radio(!c.fixed_date, "Map's date").clicked() {
                    c.fixed_date = false;
                    dirty = true;
                }
                if ui.radio(c.fixed_date, "Fixed date").clicked() {
                    c.fixed_date = true;
                    c.date = t_now;
                    dirty = true;
                }
            });
            if c.fixed_date {
                let n = info.t();
                c.date = c.date.min(n - 1);
                dirty |= ui
                    .add(
                        Slider::new(&mut c.date, 0..=n - 1)
                            .custom_formatter(|v, _| info.layers[v as usize].label.clone()),
                    )
                    .on_hover_text(
                        "The composite stays at this date while the time bar moves the series\n(chart, cursor) through \
                         the others",
                    )
                    .changed();
            }
            // the stretch of each channel; kept for every date, so they compare
            let mut changed = false;
            for (ch, label) in ["R", "G", "B"].iter().enumerate() {
                ui.horizontal(|ui| {
                    let speed = f64::from(((c.hi[ch] - c.lo[ch]) * 0.005).max(1e-6));
                    changed |= ui.add(egui::DragValue::new(&mut c.lo[ch]).speed(speed)).changed();
                    changed |= ui.add(egui::DragValue::new(&mut c.hi[ch]).speed(speed)).changed();
                    ui.label(*label);
                });
                if c.hi[ch].is_nan() || c.hi[ch] <= c.lo[ch] {
                    c.hi[ch] = c.lo[ch] + 1e-6;
                }
            }
            let have = c.last.as_ref().is_some_and(|v| !v.v.is_empty());
            if ui
                .add_enabled(have, egui::Button::new("Stretch to this image (2-98%)").small())
                .on_hover_text("The stretch of the image last read (the visible area when zoomed in)")
                .clicked()
                && let Some(v) = &c.last
            {
                let n = v.w * v.h;
                for ch in 0..3 {
                    (c.lo[ch], c.hi[ch]) = sample_percentiles(&v.v[ch * n..(ch + 1) * n], 400_000, 0.02, 0.98);
                }
                changed = true;
            }
            if changed {
                // the image last read again at once, the others as they are needed
                let bits = |x: f32| u64::from(x.to_bits());
                c.key = mix_key(
                    mix_key(mix_key(c.key, bits(c.lo[0]) ^ bits(c.hi[2])), bits(c.lo[1])),
                    bits(c.hi[0]) ^ bits(c.lo[2]) ^ bits(c.hi[1]),
                );
                if let (Some(v), Some(gpu)) = (&c.last, &self.gpu) {
                    let rgba = to_rgba(&v.v, v.w * v.h, &c.lo, &c.hi);
                    let tex = gpu.create_image(v.w as u32, v.h as u32, &rgba);
                    let img = Img { tex, key: c.key, w: v.w, h: v.h, win: v.win, used: now() };
                    if v.detail {
                        c.detail = Some(img);
                    } else {
                        c.imgs.insert(v.t, img);
                    }
                }
                dirty = true;
            }
            if !c.error.is_empty() {
                ui.colored_label(theme::warning(), &c.error);
            }
            ui.weak(format!("Chart and cursor: {}", info.selection_text()));
        }
        if dirty {
            self.map.dirty = true;
        }
    }
}

/// `--selftest-composite C` (app_composite.cpp, --selftest-ui stages 140-145): C (five bands per date, Fmask)
/// opened with an overview of half its resolution; true colour from the band names; the map's pixels against the
/// bands in the files (the overview's image, then the detail read when zoomed in); another date, a fixed date; the
/// series untouched.
#[derive(Default)]
pub struct SelfTest {
    stage: usize,
    t0: Option<Instant>,
    band: i32,
    mode: Option<janus_gpu::Mode>,
}

impl State {
    /// The map's colour at a source pixel (drawn at the centre of a 400 × 300 target at the map's scale) against
    /// the stretch of the file's bands there.
    fn composite_check(&mut self, x: usize, y: usize, t: usize, tolerance: i32, what: &str) -> bool {
        let (Some(s), Some(gpu), Some(c)) = (&self.session, self.gpu.as_mut(), self.composite.as_mut()) else {
            return false;
        };
        let sc = self.map.last_view.map_or(1.0, |v| v.scale) as f32;
        let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
        let mut f = gpu.begin_map(1, [400, 300], crate::map::MAP_BACKGROUND);
        State::draw_composite(c, gpu, &mut f, t, s.info.t(), 1.0, |w| {
            [
                200.0 + (w[0] as f32 - cx) * sc,
                150.0 + (w[1] as f32 - cy) * sc,
                200.0 + (w[2] as f32 - cx) * sc,
                150.0 + (w[3] as f32 - cy) * sc,
            ]
        });
        gpu.end_map(f);
        let Some((_, _, rgba)) = gpu.read_target(1) else { return false };
        let px = &rgba[(150 * 400 + 200) * 4..][..3];
        let mut reader = CubeReader::new(s.info.clone());
        let e: Vec<i32> = (0..3)
            .map(|ch| {
                let v = reader.read_band_pixel(t, c.bands[ch], x, y, false).unwrap_or(f32::NAN);
                (((v - c.lo[ch]) / (c.hi[ch] - c.lo[ch])).clamp(0.0, 1.0) * 255.0).round() as i32
            })
            .collect();
        println!(
            "    {what}: ({x}, {y}) at {}: map rgb({}, {}, {}), bands stretched rgb({}, {}, {})",
            s.info.layers[t].label, px[0], px[1], px[2], e[0], e[1], e[2]
        );
        (0..3).all(|ch| (i32::from(px[ch]) - e[ch]).abs() <= tolerance)
    }

    /// One frame of the test; Some(exit code) when it is over.
    pub fn composite_selftest_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        let mut st = std::mem::take(&mut self.composite_test);
        let r = self.composite_selftest_stage(&mut st);
        self.composite_test = st;
        r
    }

    fn composite_selftest_stage(&mut self, st: &mut SelfTest) -> Option<i32> {
        let t0 = *st.t0.get_or_insert_with(Instant::now);
        let fail = |stage: usize, what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut SelfTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
        };
        match st.stage {
            0 => {
                let s = self.session.as_ref()?;
                if !s.s.overview.complete() || s.stats.is_none() {
                    return None;
                }
                let info = s.info.clone();
                if s.s.overview.w >= info.width {
                    return fail(0, "C's overview should be coarser than C");
                }
                let p = presets(&info);
                if p.len() < 2 || p[0].bands != [3, 2, 1] || p[1].bands != [4, 3, 2] {
                    return fail(
                        0,
                        "true colour should be bands 3-2-1 and colour infrared 4-3-2 (from the band names)",
                    );
                }
                self.composite =
                    Some(BandComposite { on: true, bands: p[0].bands, cube_id: info.id, ..Default::default() });
                st.band = info.sel.band;
                st.mode = Some(self.display.mode);
                self.map.fit();
                self.set_date(1);
                next(st, "true colour (3-2-1) and colour infrared (4-3-2) found by the band names; composite on");
            }
            1 => {
                let s = self.session.as_ref()?;
                let info = s.info.clone();
                let c = self.composite.as_ref()?;
                if !c.ready(1, info.t()) || self.map.last_view.is_none() {
                    return None;
                }
                let img = &c.imgs[&1];
                println!(
                    "    overview image {} x {} of {} x {}; stretch R {}..{} G {}..{} B {}..{}",
                    img.w, img.h, info.width, info.height, c.lo[0], c.hi[0], c.lo[1], c.hi[1], c.lo[2], c.hi[2]
                );
                // each overview pixel is a pixel of the file: the map there shows its three bands
                let last = c.last.as_ref()?;
                let (ow, oh) = (last.w, last.h);
                let mut reader = CubeReader::new(info.clone());
                let mut same = 0;
                for j in (5..oh).step_by(17) {
                    for i in (3..ow).step_by(23) {
                        let x = ((i as f64 + 0.5) * info.width as f64 / ow as f64) as usize;
                        let y = ((j as f64 + 0.5) * info.height as f64 / oh as f64) as usize;
                        let v = reader.read_band_pixel(1, c.bands[0], x, y, false);
                        same += usize::from(v == Some(last.v[j * ow + i]));
                    }
                }
                if same < 10 {
                    return fail(1, "the composite's overview should hold the file's band values");
                }
                let x = ((10.0 + 0.5) * info.width as f64 / ow as f64) as usize;
                let y = ((7.0 + 0.5) * info.height as f64 / oh as f64) as usize;
                if !self.composite_check(x, y, 1, 2, "overview") {
                    return fail(1, "the map should show the three bands, stretched");
                }
                if info.sel.band != st.band || Some(self.display.mode) != st.mode {
                    return fail(1, "the series (chart, cursor) should keep its band and mode");
                }
                // zoomed in past the overview: the visible window is read again at full resolution (8 screen
                // points per source pixel around (60.5, 40.5))
                let ov = &self.session.as_ref()?.s.overview;
                let (fx, fy) = (info.width as f64 / ov.w as f64, info.height as f64 / ov.h as f64);
                self.map.set_view([60.5 / fx, 40.5 / fy], 8.0 * fx);
                next(st, "the overview's image: the file's bands, stretched; the series untouched");
            }
            2 => {
                let s = self.session.as_ref()?;
                let n = s.info.t();
                let (w, h) = (s.info.width as f64, s.info.height as f64);
                let v = self.map.last_view?;
                if v.scale < 4.0 {
                    return None; // the zoom not drawn yet (8 points per source pixel)
                }
                let win = [
                    v.x0.floor().clamp(0.0, w),
                    v.y0.floor().clamp(0.0, h),
                    v.x1.ceil().clamp(0.0, w),
                    v.y1.ceil().clamp(0.0, h),
                ];
                let c = self.composite.as_ref()?;
                let have = c.detail_t == Some(1)
                    && c.detail
                        .as_ref()
                        .is_some_and(|d| d.key == c.key && d.win == win && d.w == (win[2] - win[0]) as usize);
                if !have {
                    return None;
                }
                println!("    detail: window {}..{} x {}..{} read at full resolution", win[0], win[2], win[1], win[3]);
                if !self.composite_check(61, 41, 1, 1, "zoomed in") || !self.composite_check(59, 38, 1, 1, "zoomed in")
                {
                    return fail(2, "zoomed in, every pixel should show its own bands");
                }
                if self.composite.as_ref()?.status_at(61.5, 41.5, 1, n).contains("overview") {
                    return fail(2, "the cursor's band values should come from the detail");
                }
                self.map.fit();
                self.set_date(4);
                next(st, "zoomed in: the visible window at full resolution, pixel by pixel");
            }
            3 => {
                let n = self.session.as_ref()?.info.t();
                if !self.composite.as_ref()?.ready(4, n) || self.map.last_view.is_none_or(|v| v.scale > 7.0) {
                    return None;
                }
                if !self.composite_check(30, 20, 4, 2, "another date") {
                    return fail(3, "another date should show its own bands");
                }
                // a fixed date: the time bar moves, the composite stays
                let c = self.composite.as_mut()?;
                c.fixed_date = true;
                c.date = 1;
                self.set_date(7);
                let info = self.session.as_ref()?.info.clone();
                let c = self.composite.as_ref()?;
                if !c.ready(7, n) || !c.title(&info, 7).starts_with(&info.layers[1].label) {
                    return fail(3, "a fixed date should be drawn (from the cache) whatever the map's date");
                }
                if !self.composite_check(30, 20, 1, 2, "fixed date") {
                    return fail(3, "the fixed date should be drawn");
                }
                next(st, "another date; a fixed date while the time bar moves");
            }
            4 => {
                self.composite.as_mut()?.on = false;
                self.map.dirty = true;
                next(st, "composite off");
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                st.stage = 5;
                return Some(0);
            }
            _ => {}
        }
        None
    }
}
