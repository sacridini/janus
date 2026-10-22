//! Chips (src/app_validation.cpp: pumpChips, uiChips): the feature being interpreted at every date of the active
//! layer, around it, read ahead for the next ones (the disk seeks happen before they are needed); a click shows a
//! date on the map, a double click makes it the date of the change.

use std::sync::Arc;

use egui::{Color32, Rect, Stroke, StrokeKind, Ui, pos2, vec2};
use janus_cube::CubeInfo;

use crate::app::State;
use crate::validation::Job;

/// The window read around a feature: [t][band][y][x], 1 or 3 bands.
pub(crate) struct ChipSet {
    cube: u64,
    cx: usize,
    cy: usize,
    w: usize,
    t_n: usize,
    rgb: bool,
    v: Vec<f32>,
}

struct ChipJob {
    key: u64,
    job: Job<Arc<ChipSet>>,
    set: Option<Arc<ChipSet>>,
    used: f64,
}

pub(crate) struct Chips {
    jobs: Vec<ChipJob>,
    pub radius: i32,
    pub per_year: bool,
    pub size: f32,
    tex: Option<egui::TextureHandle>,
    tex_key: u64,
    cols: usize,
    cell: usize,
    tex_w: usize,
    tex_h: usize,
    /// the dates drawn, in the texture's order
    pub dates: Vec<usize>,
}

impl Default for Chips {
    fn default() -> Self {
        Chips {
            jobs: Vec::new(),
            radius: 12,
            per_year: false,
            size: 84.0,
            tex: None,
            tex_key: 0,
            cols: 0,
            cell: 0,
            tex_w: 0,
            tex_h: 0,
            dates: Vec::new(),
        }
    }
}

impl Chips {
    pub fn has_texture(&self) -> bool {
        self.tex.is_some()
    }
}

/// The window of 2r + 1 pixels around (cx, cy) at every date: the shown quantity, or the composite's 3 bands.
fn read_chips(info: Arc<CubeInfo>, cx: usize, cy: usize, r: i64, bands: Option<[i32; 3]>) -> Arc<ChipSet> {
    let w = (2 * r + 1) as usize;
    let nb = if bands.is_some() { 3 } else { 1 };
    let mut c = ChipSet {
        cube: info.id,
        cx,
        cy,
        w,
        t_n: info.t(),
        rgb: bands.is_some(),
        v: vec![f32::NAN; info.t() * nb * w * w],
    };
    let (wx0, wy0) = (cx as i64 - r, cy as i64 - r);
    // the part of the window on the layer
    let (x0, y0) = (wx0.max(0) as usize, wy0.max(0) as usize);
    let x1 = (wx0 + w as i64).min(info.width as i64).max(0) as usize;
    let y1 = (wy0 + w as i64).min(info.height as i64).max(0) as usize;
    if x1 <= x0 || y1 <= y0 {
        return Arc::new(c);
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let mut buf = vec![0f32; bw * bh];
    janus_cube::with_thread_reader(&info, |rd| {
        for t in 0..c.t_n {
            for b in 0..nb {
                let ok = match bands {
                    Some(bs) => rd.read_band_window(t, bs[b], (x0, y0, bw, bh), &mut buf, (bw, bh)),
                    None => rd.read_window(t, (x0, y0, bw, bh), &mut buf, (bw, bh)),
                };
                if !ok {
                    continue;
                }
                let base = (t * nb + b) * w * w;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (dy, dx) = ((y as i64 - wy0) as usize, (x as i64 - wx0) as usize);
                        c.v[base + dy * w + dx] = buf[(y - y0) * bw + (x - x0)];
                    }
                }
            }
        }
    });
    Arc::new(c)
}

impl State {
    /// The bands of the active layer's composite, if it is drawn as one.
    fn chip_bands(&self) -> Option<[i32; 3]> {
        self.composite.as_ref().filter(|c| c.on).map(|c| c.bands)
    }

    /// The current feature and the next two read (kept: the six used last).
    pub(crate) fn pump_chips(&mut self, ctx: &egui::Context) {
        if !self.val.show_chips || self.session.is_none() {
            return;
        }
        let Some(li) = self.labels.vector.and_then(|id| self.vector_index(id)) else { return };
        if self.vectors[li].anchor.is_empty() {
            return;
        }
        let info = self.session.as_ref().unwrap().info.clone();
        let bands = self.chip_bands();
        let r = self.val.chips.radius;
        let key_of = |cx: usize, cy: usize| {
            let mut k = info.id.wrapping_mul(1000003) ^ (((cx as u64) << 32) | cy as u64);
            k = k.wrapping_mul(31).wrapping_add(r as u64);
            for b in bands.unwrap_or([0; 3]) {
                k = k.wrapping_mul(131).wrapping_add(b as u64);
            }
            k
        };
        // the current feature and the next two
        let mut want = Vec::new();
        if let Some(c) = self.labels.current {
            want.push(c);
            let mut cur = c;
            for _ in 0..2 {
                let f = &self.labels.filter;
                let nx = if !f.is_empty() {
                    f.iter().position(|&x| x == cur).and_then(|p| f.get(p + 1).copied())
                } else {
                    (cur + 1 < self.vectors[li].anchor.len()).then_some(cur + 1)
                };
                let Some(nx) = nx else { break };
                want.push(nx);
                cur = nx;
            }
        }
        let now = ctx.input(|i| i.time);
        for fi in want {
            let Some(&[ax, ay]) = self.vectors[li].anchor.get(fi) else { continue };
            if ax.is_nan() || ax < 0.0 || ay < 0.0 || ax >= info.width as f64 || ay >= info.height as f64 {
                continue;
            }
            let (cx, cy) = (ax as usize, ay as usize);
            let key = key_of(cx, cy);
            let chips = &mut self.val.chips;
            if let Some(j) = chips.jobs.iter_mut().find(|j| j.key == key) {
                j.used = now;
                continue;
            }
            let inf = info.clone();
            chips.jobs.push(ChipJob {
                key,
                job: Job::start(move || read_chips(inf, cx, cy, i64::from(r), bands), ctx),
                set: None,
                used: now - if Some(fi) == self.labels.current { 0.0 } else { 0.5 },
            });
        }
        let chips = &mut self.val.chips;
        for j in &mut chips.jobs {
            if let Some(s) = j.job.poll() {
                j.set = Some(s);
            }
        }
        // keep a few
        while chips.jobs.len() > 6 {
            let old = (0..chips.jobs.len()).min_by(|&a, &b| chips.jobs[a].used.total_cmp(&chips.jobs[b].used)).unwrap();
            chips.jobs.remove(old);
        }
    }

    /// The Chips window.
    pub(crate) fn chips_window(&mut self, ctx: &egui::Context) {
        if !self.val.show_chips || self.docked(crate::app::Tab::Chips) {
            return;
        }
        let mut open = true;
        egui::Window::new("Chips").open(&mut open).default_size([560.0, 460.0]).show(ctx, |ui| self.chips_ui(ui));
        if !open {
            self.val.show_chips = false;
        }
    }

    pub(crate) fn chips_ui(&mut self, ui: &mut Ui) {
        let c = &mut self.val.chips;
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut c.radius, 4..=40).text("radius (px)"));
            ui.add(egui::Slider::new(&mut c.size, 40.0..=220.0).fixed_decimals(0).text("size"));
            ui.checkbox(&mut c.per_year, "one per year")
                .on_hover_text("For each year, the date nearest the map's day of the year");
        });
        let li = self.labels.vector.and_then(|id| self.vector_index(id));
        let cur = self.labels.current;
        let (Some(li), Some(s), Some(cur)) = (li, &self.session, cur) else {
            ui.label(
                "Start an interpretation (a vector layer's Interpret..., or Validation > Interpret the sample): the \
                 feature being interpreted is shown at every date of the active layer, around it.",
            );
            return;
        };
        let Some(&[ax, ay]) = self.vectors[li].anchor.get(cur) else { return };
        if ax.is_nan() {
            return;
        }
        let info = s.info.clone();
        let (cx, cy) = (ax as usize, ay as usize);
        let rgb = self.chip_bands();
        let c = &self.val.chips;
        let w = (2 * c.radius + 1) as usize;
        let set = c
            .jobs
            .iter()
            .filter_map(|j| j.set.clone())
            .find(|s| s.cube == info.id && s.cx == cx && s.cy == cy && s.w == w && s.rgb == rgb.is_some());
        let Some(set) = set else {
            ui.weak("Reading the dates around the point...");
            return;
        };
        // the dates shown: all, or one per year (nearest the map's day of the year)
        let t_now = self.display.t;
        let mut dates: Vec<usize> = (0..info.t()).collect();
        if c.per_year && info.time_is_date {
            // (year, day of the year)
            let doy = |day: i64| {
                let (y, m, d) = janus_zeit::numeric::civil(day);
                let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
                const BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
                (y, BEFORE[m as usize - 1] + i64::from(d) - 1 + i64::from(leap && m > 2))
            };
            let (_, d0) = doy(info.unix_day(t_now));
            let mut best: std::collections::BTreeMap<i64, (i64, usize)> = Default::default();
            for t in 0..info.t() {
                let (y, d) = doy(info.unix_day(t));
                let dist = (d - d0).abs();
                if best.get(&y).is_none_or(|b| dist < b.0) {
                    best.insert(y, (dist, t));
                }
            }
            dates = best.values().map(|b| b.1).collect();
        }
        // the colours: the map's (its colormap and range, a class's colour, or the composite's stretch)
        let scale = (c.size / w as f32).ceil().max(1.0) as usize;
        let cell = w * scale;
        let cols = ((dates.len() as f64).sqrt().ceil() as usize).max(1);
        let rows = dates.len().div_ceil(cols);
        let m = janus_gpu::Mode::Value as usize;
        let (lo, hi) = (self.display.ranges[m].lo, self.display.ranges[m].hi);
        let cmap = self.display.cmap[m];
        let classes = (self.classes.state == crate::classes::ClassState::On).then(|| self.classes.list.clone());
        let comp = self.composite.as_ref().map(|c| (c.lo, c.hi));
        let mut key = (Arc::as_ptr(&set) as u64).wrapping_mul(1000003);
        for &t in &dates {
            key = key.wrapping_mul(31).wrapping_add(t as u64);
        }
        key = key.wrapping_mul(31).wrapping_add(cmap as u64 * 7 + scale as u64);
        key = key.wrapping_mul(131).wrapping_add(u64::from(lo.to_bits()));
        key = key.wrapping_mul(131).wrapping_add(u64::from(hi.to_bits()));
        if let (true, Some((clo, chi))) = (set.rgb, comp) {
            for b in 0..3 {
                key = key.wrapping_mul(131).wrapping_add(u64::from(clo[b].to_bits()) * 7 + u64::from(chi[b].to_bits()));
            }
        }
        let c = &mut self.val.chips;
        if key != c.tex_key {
            c.tex_key = key;
            let (tw, th) = (cols * cell, rows * cell);
            let mut rgba = vec![0u8; tw * th * 4];
            let nb = if set.rgb { 3 } else { 1 };
            let lut = janus_gpu::colormaps::all()[cmap].lut();
            for (k, &t) in dates.iter().enumerate() {
                let (ox, oy) = ((k % cols) * cell, (k / cols) * cell);
                for y in 0..w {
                    for x in 0..w {
                        let mut px = [40u8, 40, 40, 255];
                        if set.rgb {
                            let (clo, chi) = comp.unwrap_or(([0.0; 3], [1.0; 3]));
                            let mut any = false;
                            for b in 0..3 {
                                let val = set.v[(t * nb + b) * w * w + y * w + x];
                                if val.is_nan() {
                                    continue;
                                }
                                any = true;
                                let s = (val - clo[b]) / (chi[b] - clo[b]).max(1e-12);
                                px[b] = (255.0 * s.clamp(0.0, 1.0)).round() as u8;
                            }
                            if !any {
                                px = [40, 40, 40, 255];
                            }
                        } else {
                            let val = set.v[t * w * w + y * w + x];
                            if !val.is_nan() {
                                let rgb = match &classes {
                                    Some(list) => {
                                        let v = val.round() as i32;
                                        list.iter().find(|e| e.value == v).map_or([77, 77, 77], |e| e.color)
                                    }
                                    None => {
                                        let s = ((val - lo) / (hi - lo).max(1e-12)).clamp(0.0, 1.0);
                                        let c = janus_gpu::colormaps::all()[cmap].sample(s);
                                        let _ = &lut;
                                        [c[0], c[1], c[2]]
                                    }
                                };
                                px = [rgb[0], rgb[1], rgb[2], 255];
                            }
                        }
                        for sy in 0..scale {
                            for sx in 0..scale {
                                let d = ((oy + y * scale + sy) * tw + ox + x * scale + sx) * 4;
                                rgba[d..d + 4].copy_from_slice(&px);
                            }
                        }
                    }
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([tw, th], &rgba);
            c.tex = Some(ui.ctx().load_texture("chips", img, egui::TextureOptions::NEAREST));
            (c.cols, c.cell, c.tex_w, c.tex_h) = (cols, cell, tw, th);
            c.dates = dates;
        }
        // the grid: click a date to show it on the map, double click to make it the date of the change
        let change_date = self.labels.entries.get(cur).map(|e| e.date.clone()).unwrap_or_default();
        let c = &self.val.chips;
        let (Some(tex), sz) = (c.tex.clone(), c.size) else { return };
        let dates = c.dates.clone();
        let (cols, cell, tw, th, r) = (c.cols, c.cell as f32, c.tex_w as f32, c.tex_h as f32, c.radius);
        let mut go: Option<(usize, bool)> = None;
        egui::ScrollArea::both().id_salt("chips").auto_shrink(false).show(ui, |ui| {
            let per_row = ((ui.available_width() / (sz + ui.spacing().item_spacing.x)) as usize).max(1);
            for row in dates.chunks(per_row).enumerate() {
                ui.horizontal(|ui| {
                    for (j, &t) in row.1.iter().enumerate() {
                        let k = row.0 * per_row + j;
                        ui.vertical(|ui| {
                            let uv0 = pos2((k % cols) as f32 * cell / tw, (k / cols) as f32 * cell / th);
                            let uv = Rect::from_min_size(uv0, vec2(cell / tw, cell / th));
                            let resp =
                                ui.add(egui::Image::new((tex.id(), vec2(sz, sz))).uv(uv).sense(egui::Sense::click()));
                            let label = &info.layers[t].label;
                            let resp = resp.on_hover_text(format!(
                                "{label}: click to show it, double click: the date of the change"
                            ));
                            if resp.double_clicked() {
                                go = Some((t, true));
                            } else if resp.clicked() {
                                go = Some((t, false));
                            }
                            // the point's pixel, the date on the map and the date of the change
                            let p0 = resp.rect.min;
                            let c0 = r as f32 / (2 * r + 1) as f32 * sz;
                            let c1 = c0 + sz / (2 * r + 1) as f32;
                            let painter = ui.painter();
                            painter.rect_stroke(
                                Rect::from_min_max(p0 + vec2(c0, c0), p0 + vec2(c1, c1)),
                                0.0,
                                Stroke::new(1.5, Color32::YELLOW),
                                StrokeKind::Middle,
                            );
                            if t == t_now {
                                painter.rect_stroke(
                                    resp.rect.expand(2.0),
                                    0.0,
                                    Stroke::new(2.0, Color32::WHITE),
                                    StrokeKind::Middle,
                                );
                            }
                            if !change_date.is_empty() && *label == change_date {
                                painter.rect_stroke(
                                    resp.rect.expand(4.0),
                                    0.0,
                                    Stroke::new(2.0, Color32::from_rgb(240, 70, 60)),
                                    StrokeKind::Middle,
                                );
                            }
                            ui.weak(label);
                        });
                    }
                });
            }
        });
        if let Some((t, date)) = go {
            self.set_date(t);
            if date {
                self.interp_set_date(false);
            }
            self.map.dirty = true;
        }
    }
}
