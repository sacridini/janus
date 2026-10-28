//! `--selftest-ui A B [C [D [E]]]` (src/app_selftest.cpp, stages 0-19): the layers workflow driven frame by frame
//! on the test series of tools/make_selftest_data.py. A opens (continuous), B is added as a layer (100 px east of
//! A, same grid lines); the cursor and a pin at B's (50, 100) read both layers' series; the map is drawn with both,
//! with B hidden (a map panel of A draws the same), with both hidden; A made active carries the pin to its grid; B
//! closes. C (bands per date, Fmask) opens with its quality band, reopens in place as NDVI keeping the pin and the
//! view; the difference, largest-drop and temporal-composite maps and the season are checked against CPU
//! references. D and E (categorical, with and without a colour table) open with their classes drawn in their
//! colours. The C++'s exports, transect, full-resolution cache, basemap, settings and Analysis stages follow their
//! features (RUST_MIGRATION.md).

use janus_gpu::Mode;

#[path = "ui_test_animation.rs"]
mod animation;
#[path = "ui_test_basemap.rs"]
mod basemap;
#[path = "ui_test_fullres.rs"]
mod fullres;
#[path = "ui_test_interface.rs"]
mod interface;
#[path = "ui_test_misc.rs"]
mod misc;
#[path = "ui_test_validation.rs"]
mod validation;
#[path = "ui_test_vectors.rs"]
mod vectors;

use crate::app::State;
use crate::classes::ClassState;
use crate::series::approx_series;
use crate::session::Session;

/// The offscreen targets the test draws into (the map panels use 2, 3...).
const MAP_SLOT: i32 = 98;
const VIEW_SLOT: u32 = 99;
const SIZE: [u32; 2] = [400, 300];

/// Test points, in a 200 × 120 frame scaled to the layer.
const POINTS: [[usize; 2]; 5] = [[10, 10], [60, 70], [150, 30], [180, 100], [120, 60]];

#[derive(Default)]
pub struct UiTest {
    stage: usize,
    t0: Option<std::time::Instant>,
    since: Option<std::time::Instant>,
    /// the pin's x before A was made active
    pin_x: usize,
    /// the season checked (0: none of the dates, 1: some) and the statistics computed before it
    season: usize,
    stats_gen: u64,
    /// the NDVI's bands (1-based) on the spectrum, for its layer
    index_bands: (i32, i32),
    /// stages 160-162: C's NDVI at the pin (as a normalized difference)
    ndvi: Vec<f32>,
    /// stages 120-127: the vector files' folder, the points written, the exports, frames of a stage, the
    /// interpretation's pin, its labels' file
    dir: std::path::PathBuf,
    pts: Vec<[f64; 2]>,
    jobs: Vec<std::sync::Arc<crate::exports::ExportJob>>,
    frames: usize,
    pin: Option<u32>,
    reveal: Option<std::path::PathBuf>,
    /// stages 130-137: the animations' files and options
    anim: animation::AnimTest,
    /// stage 70: the font size before the test's; 81: the run's folder, the layers and the active one before it
    font: i32,
    /// stage 58: the full-resolution cache's series read before the pin
    hits: usize,

    run: Option<(std::path::PathBuf, usize, usize)>,
    /// stage 82: the vector layers before the SNIC run
    run_vectors: usize,
    /// stages 192-196: the project's state as saved, the series whose file goes
    texts: Vec<String>,
    /// the exit code, once decided (the window takes another frame to close)
    result: Option<i32>,
    /// each window's width at the last frame and how many frames in a row it grew (a window must not widen by
    /// itself: a row a fraction of a point wider than it grows it frame after frame)
    widths: std::collections::HashMap<egui::Id, (f32, usize)>,
}

/// Frames in a row a window may widen (its content changed) before it is a window growing by itself.
const GROWING: usize = 30;

/// A window (Tasks, Exports, a tool's...) widening at every frame for GROWING frames: its id, else None.
fn growing_window(
    ctx: &egui::Context,
    widths: &mut std::collections::HashMap<egui::Id, (f32, usize)>,
) -> Option<String> {
    let rects: Vec<(egui::Id, f32)> = ctx.memory(|m| {
        m.layer_ids()
            .filter(|l| l.order == egui::Order::Middle)
            .filter_map(|l| m.area_rect(l.id).map(|r| (l.id, r.width())))
            .collect()
    });
    for (id, w) in rects {
        let e = widths.entry(id).or_insert((w, 0));
        e.1 = if w > e.0 + 1e-4 { e.1 + 1 } else { 0 };
        e.0 = w;
        if e.1 >= GROWING {
            // its title, when it is one of these
            let titles = ["Tasks", "Exports", "Log", "Settings", "Chips", "Self-test files"];
            let tools =
                janus_zeit::manifest().tools.iter().map(|t| (egui::Id::new(("zeit tool", &t.id)), t.name.as_str()));
            let name = titles
                .iter()
                .map(|t| (egui::Id::new(*t), *t))
                .chain([(egui::Id::new("ml window"), "Deep learning")])
                .chain([(egui::Id::new("calculator window"), "Raster calculator")])
                .chain(tools)
                .find(|(i, _)| *i == id)
                .map_or_else(|| format!("{id:?}"), |(_, t)| t.to_string());
            return Some(format!("{name} ({w:.1} points wide)"));
        }
    }
    None
}

pub(crate) fn loaded(s: Option<&Session>) -> bool {
    s.is_some_and(|s| s.s.overview.complete() && (s.info.t() < 2 || s.stats.is_some()))
}

impl State {
    pub fn ui_selftest_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        if self.ui_test.result.is_some() {
            return self.ui_test.result;
        }
        let mut st = std::mem::take(&mut self.ui_test);
        if let Some(name) = growing_window(ctx, &mut st.widths) {
            println!("FAIL: the window {name} widens by itself, frame after frame");
            self.ui_test = st;
            self.ui_test.result = Some(1);
            return Some(1);
        }
        let r = self.ui_selftest_stage(&mut st, ctx);
        st.result = r;
        self.ui_test = st;
        r
    }

    /// The main map's view fitted to a 400 × 300 canvas, drawn offscreen (C++ fitView + renderMap); the target's
    /// pixels.
    fn test_render(&mut self) -> Option<Vec<u8>> {
        self.render_map(MAP_SLOT, SIZE, [SIZE[0] as f32, SIZE[1] as f32], 1.0);
        let (_, _, px) = self.gpu.as_ref()?.read_target(MAP_SLOT)?;
        Some(px)
    }

    fn test_fit(&mut self) {
        let Some(g) = self.grid() else { return };
        let scale = 0.98 * (f64::from(SIZE[0]) / g.ow).min(f64::from(SIZE[1]) / g.oh);
        self.map.set_view([g.ow / 2.0, g.oh / 2.0], scale);
    }

    /// The test target's pixel showing source pixel (ix, iy) of the active layer.
    fn test_pixel_of(&self, ix: usize, iy: usize) -> Option<(usize, usize)> {
        let g = self.grid()?;
        let m = &self.map;
        let x = f64::from(SIZE[0]) / 2.0 + ((ix as f64 + 0.5) / g.fx - m.center[0]) * m.scale;
        let y = f64::from(SIZE[1]) / 2.0 + ((iy as f64 + 0.5) / g.fy - m.center[1]) * m.scale;
        Some((x as usize, y as usize))
    }

    /// The map in `mode` with the range lo..hi shows `value` at source pixel (ix, iy) in its colour (between the
    /// colormap's two nearest entries, as its texture is sampled), or the background where it is NaN.
    fn expect_colour(&mut self, mode: Mode, lo: f32, hi: f32, ix: usize, iy: usize, value: f32) -> bool {
        self.display.mode = mode;
        let r = &mut self.display.ranges[mode as usize];
        (r.lo, r.hi, r.manual) = (lo, hi, true);
        let Some(img) = self.test_render() else { return false };
        let Some((cx, cy)) = self.test_pixel_of(ix, iy) else { return false };
        let p = &img[(cy * SIZE[0] as usize + cx) * 4..][..3];
        let (a, c) = if value.is_nan() {
            let bg = crate::theme::map_background().map(|v| (v * 255.0).round() as u8);
            ([bg[0], bg[1], bg[2], 255], [bg[0], bg[1], bg[2], 255])
        } else {
            let lut = janus_gpu::colormaps::all()[self.display.cmap[mode as usize]].lut();
            let k = ((value - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0;
            (lut[k.floor() as usize], lut[k.ceil() as usize])
        };
        let ok = (0..3).all(|i| {
            let (l, u) = (i32::from(a[i].min(c[i])) - 3, i32::from(a[i].max(c[i])) + 3);
            (l..=u).contains(&i32::from(p[i]))
        });
        if !ok {
            println!(
                "    {mode:?} at ({ix},{iy}): value {value:.4}, map rgb{:?}, expected rgb{:?}..{:?}",
                (p[0], p[1], p[2]),
                &a[..3],
                &c[..3]
            );
        }
        ok
    }

    /// Difference and largest-drop maps of the active layer (statistics computed): the GPU statistics against the
    /// CPU rule for every overview pixel, then map pixels against the colour of the CPU value.
    fn check_change_maps(&mut self) -> Result<(), &'static str> {
        let s = self.session.as_ref().ok_or("no series")?;
        let (ov, info) = (&s.s.overview, s.info.clone());
        let st = s.stats.as_ref().ok_or("no statistics")?;
        let n = info.t();
        let (mut drops, mut gappy, mut ties, mut max_err) = (0, 0, 0, 0f64);
        let mut v = vec![0f32; n];
        for p in 0..ov.w * ov.h {
            let mut gap = false;
            for (t, x) in v.iter_mut().enumerate() {
                *x = ov.layer(t).ok_or("a date not read")?[p];
                gap |= x.is_nan();
            }
            let (mag, at) = janus_cube::stats::largest_drop(&v);
            let (g_mag, g_at) = (st.s1[p][3], st.s2[p]);
            if mag.is_nan() != g_mag.is_nan() || at.is_none() != g_at.is_nan() {
                return Err("drop: NaN where the CPU has none");
            }
            if mag.is_nan() {
                continue;
            }
            max_err = max_err.max((mag - f64::from(g_mag)).abs());
            if (mag - f64::from(g_mag)).abs() > 1e-5 {
                println!("    pixel {p}: CPU {mag:.7} at {at:?}, GPU {g_mag:.7} at {g_at}; series {v:?}");
                return Err("drop magnitude differs from the CPU");
            }
            if let Some(at) = at {
                if g_at as usize != at {
                    ties += 1;
                    if ties > 3 {
                        return Err("drop date differs from the CPU"); // only a near tie may differ
                    }
                    continue;
                }
                drops += 1;
                gappy += usize::from(gap);
            }
        }
        println!(
            "    largest drop: {drops} pixels with a drop ({gappy} with masked dates), max |GPU - CPU| {max_err:.2e}, \
             {ties} near ties"
        );
        if drops == 0 || gappy == 0 {
            return Err("the test data should have drops, some across masked dates");
        }

        self.test_fit();
        let (saved_t, saved_ref) = (self.display.t, self.display.diff_ref);
        let mut checked = 0;
        for q in POINTS {
            let (ix, iy) = (q[0] * info.width / 200, q[1] * info.height / 120);
            let sv = approx_series(self.session.as_ref().ok_or("no series")?, ix, iy);
            let (mag, at) = janus_cube::stats::largest_drop(&sv);
            if !self.expect_colour(Mode::DropMag, 0.0, 0.05, ix, iy, mag as f32) {
                return Err("largest drop magnitude: wrong colour on the map");
            }
            let at = at.map_or(f32::NAN, |a| a as f32);
            if !self.expect_colour(Mode::DropDate, 0.0, (n - 1) as f32, ix, iy, at) {
                return Err("largest drop date: wrong colour on the map");
            }
            // the difference: to a fixed reference (the first date), then to the previous date
            for reference in [0, -1] {
                for t in [1, 2, 3, n - 1] {
                    self.display.diff_ref = reference;
                    self.display.t = t;
                    let r = if reference < 0 { t - 1 } else { reference as usize };
                    let d = sv[t] - sv[r];
                    if !self.expect_colour(Mode::Diff, -0.05, 0.05, ix, iy, d) {
                        return Err("difference: wrong colour on the map");
                    }
                    checked += usize::from(!d.is_nan());
                }
            }
        }
        println!("    map pixels of the drop and difference modes match the CPU ({checked} finite differences)");
        self.display.t = saved_t;
        self.display.diff_ref = saved_ref;
        self.display.mode = Mode::Value;
        self.map.dirty = true;
        Ok(())
    }

    /// A temporal composite of `sv` over dates t0..=t1 in `in_season` (sorted values: series of < 64 dates).
    fn reference(sv: &[f32], t0: usize, t1: usize, reducer: i32, in_season: &[bool]) -> f32 {
        let mut v: Vec<f32> = (t0..=t1).filter(|&t| in_season[t] && !sv[t].is_nan()).map(|t| sv[t]).collect();
        if reducer == 4 {
            return v.len() as f32;
        }
        if v.is_empty() {
            return f32::NAN;
        }
        v.sort_by(f32::total_cmp);
        let n = v.len();
        match reducer {
            0 if n % 2 == 1 => v[n / 2],
            0 => 0.5 * (v[n / 2 - 1] + v[n / 2]),
            1 => (v.iter().map(|&x| f64::from(x)).sum::<f64>() / n as f64) as f32,
            2 => v[n - 1],
            _ => v[0],
        }
    }

    /// Temporal composites of the active layer: 5 reducers × 3 windows, map pixels against a CPU reference.
    fn check_composites(&mut self) -> Result<(), &'static str> {
        let info = self.session.as_ref().ok_or("no series")?.info.clone();
        let n = info.t();
        if !info.time_is_date || n < 4 {
            return Err("the test layer should have at least 4 dates");
        }
        self.test_fit();
        let saved = (self.display.reducer, self.display.window, self.display.days, self.display.t);
        let all = vec![true; n];
        for q in POINTS {
            let (ix, iy) = (q[0] * info.width / 200, q[1] * info.height / 120);
            let sv = approx_series(self.session.as_ref().ok_or("no series")?, ix, iy);
            for reducer in 0..5 {
                self.display.reducer = reducer;
                let (lo, hi) = if reducer == 4 { (0.0, n as f32) } else { (-1.0, 1.0) };
                self.display.window = 2; // the whole series
                if !self.expect_colour(Mode::Composite, lo, hi, ix, iy, Self::reference(&sv, 0, n - 1, reducer, &all)) {
                    return Err("composite of the whole series: wrong colour on the map");
                }
                self.display.window = 0; // the date's year: one date per year here
                let t = n / 2;
                self.display.t = t;
                if !self.expect_colour(Mode::Composite, lo, hi, ix, iy, Self::reference(&sv, t, t, reducer, &all)) {
                    return Err("composite of the date's year: wrong colour on the map");
                }
                self.display.window = 1; // ±400 days: the date and its neighbours (yearly dates)
                self.display.days = 400;
                if !self.expect_colour(
                    Mode::Composite,
                    lo,
                    hi,
                    ix,
                    iy,
                    Self::reference(&sv, t - 1, t + 1, reducer, &all),
                ) {
                    return Err("composite of +-400 days: wrong colour on the map");
                }
            }
        }
        (self.display.reducer, self.display.window, self.display.days, self.display.t) = saved;
        self.display.mode = Mode::Value;
        self.map.dirty = true;
        println!("    temporal composites (5 reducers x 3 windows, {} pixels) match the CPU", POINTS.len());
        Ok(())
    }

    /// The season set (stage `k` of its check): a day no date falls on (none in), then the days from the 2nd date
    /// to the middle one (some in). Its statistics are computed again in the next frames.
    fn set_test_season(&mut self, k: usize) -> Option<()> {
        let info = self.session.as_ref()?.info.clone();
        let doy = |t: usize| crate::season::day_of_year(info.layers[t].time).0;
        let empty = (1..366).find(|&d| (0..info.t()).all(|t| doy(t) != d))?;
        let (a, b) = (doy(1), doy(info.t() / 2));
        self.display.season = crate::season::Season {
            on: true,
            start: if k == 0 { empty } else { a.min(b) },
            end: if k == 0 { empty } else { a.max(b) },
        };
        Some(())
    }

    /// The mean and the observations in the season against the CPU.
    fn check_season(&mut self, k: usize) -> Result<(), &'static str> {
        let info = self.session.as_ref().ok_or("no series")?.info.clone();
        let n = info.t();
        let season = self.display.season;
        let in_season: Vec<bool> = (0..n)
            .map(|t| {
                let d = crate::season::day_of_year(info.layers[t].time).0;
                d >= season.start && d <= season.end
            })
            .collect();
        if (0..n).any(|t| in_season[t] != season.contains(&info, t)) {
            return Err("season: a date's day of the year misjudged");
        }
        let inside = in_season.iter().filter(|&&b| b).count();
        if (k == 0) != (inside == 0) {
            return Err("season: the test season should hold no date, then some");
        }
        self.test_fit();
        let saved = (self.display.reducer, self.display.window);
        for q in POINTS {
            let (ix, iy) = (q[0] * info.width / 200, q[1] * info.height / 120);
            let sv = approx_series(self.session.as_ref().ok_or("no series")?, ix, iy);
            if !self.expect_colour(Mode::Mean, -1.0, 1.0, ix, iy, Self::reference(&sv, 0, n - 1, 1, &in_season)) {
                return Err("temporal mean in the season: wrong colour on the map");
            }
            (self.display.window, self.display.reducer) = (2, 4);
            let count = Self::reference(&sv, 0, n - 1, 4, &in_season);
            if !self.expect_colour(Mode::Composite, 0.0, n as f32, ix, iy, count) {
                return Err("observations in the season: wrong colour on the map");
            }
        }
        (self.display.reducer, self.display.window) = saved;
        self.display.mode = Mode::Value;
        self.map.dirty = true;
        println!("    season {}: {inside} of {n} dates in; the mean and the observations match the CPU", season.text());
        Ok(())
    }

    /// After C: D opens (stage 14), or the test ends without it.
    fn test_open_d(
        &mut self,
        st: &mut UiTest,
        input: &dyn Fn(usize) -> Option<String>,
        t0: std::time::Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
        let Some(d) = input(2) else {
            println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
            return Some(0);
        };
        self.open_inputs(vec![d], false, ctx);
        println!("[{:5.1} s] open D (categorical, colour table)", t0.elapsed().as_secs_f64());
        st.stage = 14;
        st.since = Some(std::time::Instant::now());
        None
    }

    fn ui_selftest_stage(&mut self, st: &mut UiTest, ctx: &egui::Context) -> Option<i32> {
        let t0 = *st.t0.get_or_insert_with(std::time::Instant::now);
        let fail = |what: &str| {
            println!("FAIL: {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
        };
        let ok = || {
            println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
            Some(0)
        };
        if st.since.get_or_insert_with(std::time::Instant::now).elapsed().as_secs_f64() > 120.0 {
            return fail("timeout");
        }
        let extra = self.opts.extra_layers.clone();
        let input = |k: usize| extra.get(k).cloned();
        match st.stage {
            // A opens with the app (the basemap off, whatever the user's setting)
            0 => {
                self.basemap.ui.on = false;
                if self.opening.is_some() {
                    return None;
                }
                if self.session.is_none() {
                    return fail(&format!("could not open A: {}", self.error.clone().unwrap_or_default()));
                }
                if self.layers.len() != 1
                    || !loaded(self.session.as_ref())
                    || self.classes.state == ClassState::Undecided
                {
                    return None;
                }
                if self.classes.state != ClassState::Off {
                    return fail("A (continuous) must not be categorical");
                }
                next(st, "A loaded (continuous)");
                self.open_inputs(vec![input(0)?], true, ctx);
                next(st, "add B as a layer");
            }
            2 => {
                if self.opening.is_some() || self.layers.len() != 2 || !loaded(self.session.as_ref()) {
                    return None;
                }
                if self.active != 1 {
                    return fail("the added layer should be active");
                }
                let p = &self.layers[0].place;
                println!(
                    "    layer A in B's grid: x' = {:.3} + {:.3} x, y' = {:.3} + {:.3} y (aligned {})",
                    p.ax, p.bx, p.ay, p.by, p.aligned
                );
                if !p.aligned {
                    return fail("A should be aligned with B (same CRS)");
                }
                next(st, "B loaded, alignment checked");
                // the cursor and a pin at the same place, in the overlap of both layers
                self.set_chart_layers(1);
                self.set_hover(Some((50, 100))); // B's pixel 50 = A's pixel 150 (B starts 100 px east)
                self.rest_hover();
                self.add_pin(50, 100);
                next(st, "cursor and pin placed");
            }
            4 => {
                let (sr, a) = (&self.series, &self.layers[0].series);
                let exact = |h: Option<&crate::series::SeriesView>| h.is_some_and(|h| h.exact);
                if !exact(sr.hover.as_ref())
                    || !exact(a.hover.as_ref())
                    || !exact(sr.pins.first())
                    || !exact(a.pins.first())
                {
                    return None;
                }
                let (bh, ah) = (sr.hover.as_ref()?, a.hover.as_ref()?);
                let (bi, ai) = (&self.session.as_ref()?.info, &self.layers[0].session.as_ref()?.info);
                println!(
                    "    cursor: B({},{}) = {:.3} at {} | A({},{}) = {:.3} at {}",
                    bh.x, bh.y, bh.values[0], bi.layers[0].label, ah.x, ah.y, ah.values[0], ai.layers[0].label
                );
                if ah.x != bh.x + 100 {
                    return fail("A's cursor pixel should be B's + 100");
                }
                if (bh.values[0] - 2.0).abs() > 1e-4 {
                    return fail("B's first value should be 2.0");
                }
                next(st, "exact series of both layers under the cursor and the pin");
            }
            5 => {
                self.test_fit();
                let centre = |img: &[u8]| {
                    let p = (150 * SIZE[0] as usize + 200) * 4;
                    [img[p], img[p + 1], img[p + 2]]
                };
                let both = centre(&self.test_render()?);
                println!("    map center with both layers: rgb{both:?}");
                self.layers[1].visible = false; // the active layer hidden: A must show through
                let a_alone = centre(&self.test_render()?);
                println!("    map center with B hidden:    rgb{a_alone:?}");
                if a_alone == both {
                    return fail("hiding the top layer did not change the map");
                }
                // a map panel showing A on the same view draws what the main map shows of A alone
                let cube = self.layers[0].session.as_ref()?.info.id;
                let mut v = crate::views::ViewSetup::new(VIEW_SLOT, cube);
                self.render_view(&mut v, SIZE, 1.0, VIEW_SLOT as i32);
                let (_, _, img) = self.gpu.as_ref()?.read_target(VIEW_SLOT as i32)?;
                self.gpu.as_mut()?.release_target(VIEW_SLOT as i32);
                let panel = centre(&img);
                println!("    map panel showing A:         rgb{panel:?}");
                if panel != a_alone {
                    return fail("a map panel of A should draw what the main map shows of A");
                }
                // a Zeit result over A (1 everywhere, at the top of 0..1): drawn on both
                let info = self.layers[0].session.as_ref()?.info.clone();
                let mut r = crate::zeit::ResultLayer::for_test(vec![1.0; 16], 4, 4, String::new());
                (r.cube, r.w, r.h, r.visible) = (cube, info.width, info.height, true);
                r.grid = Some(info.clone());
                r.tex = Some(self.gpu.as_ref()?.create_tile(4, 4, &r.data));
                self.push_result(r);
                let on_map = centre(&self.test_render()?);
                let mut v = crate::views::ViewSetup::new(VIEW_SLOT, cube);
                self.render_view(&mut v, SIZE, 1.0, VIEW_SLOT as i32);
                let (_, _, img) = self.gpu.as_ref()?.read_target(VIEW_SLOT as i32)?;
                self.gpu.as_mut()?.release_target(VIEW_SLOT as i32);
                let on_panel = centre(&img);
                self.zeit.results.pop();
                println!("    a result over A: rgb{on_map:?} on the map, rgb{on_panel:?} on the panel");
                let top = janus_gpu::colormaps::all()[janus_gpu::colormaps::VIRIDIS].sample(1.0);
                if on_map != on_panel || on_map.iter().zip(&top).any(|(&a, &b)| a.abs_diff(b) > 2) {
                    return fail("a result should be drawn over its layer on the map and on a map panel");
                }
                self.layers[0].visible = false;
                let none = centre(&self.test_render()?);
                println!("    map center with both hidden: rgb{none:?}");
                if none.iter().any(|&c| c > 40) {
                    return fail("with every layer hidden the map should be empty");
                }
                self.layers[0].visible = true;
                self.layers[1].visible = true;
                self.map.dirty = true;
                if let Err(e) = self.test_transect(ctx) {
                    return fail(&e);
                }
                // Analysis: the scatter of A and B (B = A - 0.5)
                let t = self.display.t;
                let sc = self.compute_scatter(0, t, 1, t);
                println!(
                    "    scatter A x B at {}: {} pixels, r {:.6}, B = {:.4} + {:.4} A, mean(B - A) {:.4}",
                    self.session.as_ref()?.info.layers[t].label,
                    sc.n,
                    sc.r,
                    sc.a,
                    sc.b,
                    sc.mean_diff
                );
                if sc.n < 100
                    || sc.r.is_nan()
                    || sc.r <= 0.999
                    || (sc.b - 1.0).abs() > 0.01
                    || (sc.a + 0.5).abs() > 0.02
                    || (sc.mean_diff + 0.5).abs() > 0.02
                {
                    return fail("the scatter of A and B should be the line B = A - 0.5");
                }
                next(st, "layer drawing and visibility, a map panel, transect");
                st.stage = 50; // the basemap (50-56), then 6
            }
            6 => {
                st.pin_x = self.series.pins.first()?.x;
                self.set_active(0, ctx);
                if self.active != 0 || self.session.as_ref()?.info.id != self.layer_session(0)?.info.id {
                    return fail("A should be active");
                }
                let pin = self.series.pins.first().map(|p| p.x);
                let b = &self.layers[1].place;
                println!(
                    "    pin x: {} in B's grid -> {pin:?} in A's grid; B in A's grid: x' = {:.1} + {:.1} x",
                    st.pin_x, b.ax, b.bx
                );
                if pin != Some(st.pin_x + 100) {
                    return fail("the pin should move to A's grid (+100)");
                }
                next(st, "active layer switched, pin remapped");
                st.stage = 70; // font, Files, Log, Analysis, layout, exports, a Zeit run (70-81), then 7
            }
            // B closed from the Layers list: its row clicked, then Delete (taken in this frame's shortcuts)
            7 => {
                let id = self.layer_session(1)?.info.id;
                self.contents.click(crate::contents::Sel::Series(id), egui::Modifiers::NONE);
                vectors::key(ctx, egui::Key::Delete, egui::Modifiers::NONE);
                next(st, "B selected on the list, Delete");
            }
            8 => {
                if self.layers.len() != 1 || self.active != 0 {
                    return fail("Delete on the list should close B and leave A active");
                }
                if !self.contents.selected.is_empty() {
                    return fail("closing the selected layers should empty the selection");
                }
                next(st, "layer closed");
                let Some(c) = input(1) else { return ok() };
                self.basemap.ui.on = true; // ticked: opening another series hides it again
                self.open_inputs(vec![c], false, ctx);
                println!("[{:5.1} s] open C (bands per date)", t0.elapsed().as_secs_f64());
            }
            9 => {
                if self.opening.is_some()
                    || self.layers.len() != 1
                    || !loaded(self.session.as_ref())
                    || self.session.as_ref()?.info.bands_per_date < 2
                {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                println!(
                    "    {}; {} bands per date, shown: {}",
                    info.description,
                    info.bands_per_date,
                    info.selection_text()
                );
                if info.sel.qa_band == 0 {
                    return fail("the Fmask band should be used automatically");
                }
                if self.basemap.ui.on {
                    return fail("opening another series should hide the basemap");
                }
                self.add_pin(info.width - 10, info.height / 2);
                // a view of our own, to check it is kept
                self.test_fit();
                let (c, sc) = (self.map.center, self.map.scale);
                self.map.set_view(c, sc * 2.0);
                self.map.touched = true;
                next(st, "C loaded, quality band detected");
            }
            10 => {
                if !self.series.pins.first().is_some_and(|p| p.exact) {
                    return None;
                }
                let mut sel = self.session.as_ref()?.info.sel;
                sel.band = 4; // NIR
                sel.nd_band = 3; // Red -> NDVI
                self.reopen_layer(self.active, sel, ctx);
                next(st, "reopen C as ND(NIR, Red)");
            }
            11 => {
                let s = self.session.as_ref()?;
                if self.opening.is_some()
                    || s.info.sel.nd_band != 3
                    || !loaded(Some(s))
                    || !self.series.pins.first().is_some_and(|p| p.exact)
                {
                    return None;
                }
                let pin = &self.series.pins[0];
                let finite: Vec<f32> = pin.values.iter().copied().filter(|v| v.is_finite()).collect();
                let (lo, hi) = finite.iter().fold((f32::MAX, f32::MIN), |(l, h), &v| (l.min(v), h.max(v)));
                let masked = pin.values.len() - finite.len();
                println!(
                    "    {} ({}); pin at ({},{}): NDVI {lo:.3} .. {hi:.3}, {masked} of {} dates masked or missing",
                    self.layers[0].name,
                    s.info.description,
                    pin.x,
                    pin.y,
                    s.info.t()
                );
                if self.layers.len() != 1 || self.active != 0 {
                    return fail("the reopened layer should stay in place and active");
                }
                if lo < -1.0 || hi > 1.0 {
                    return fail("a normalized difference is within [-1, 1]");
                }
                if masked == 0 {
                    return fail("cloudy dates should be masked by the QA band");
                }
                if self.map.fit_pending || !self.map.touched {
                    return fail("the view should be kept");
                }
                if let Err(e) = self.check_change_maps() {
                    return fail(e);
                }
                if let Err(e) = self.check_composites() {
                    return fail(e);
                }
                st.ndvi = self.series.pins[0].values.clone();
                next(st, "reopened in place: name, pin and view kept, values are NDVI, clouds masked; composites");
                st.stage = 160; // the NDVI as an expression, the raster calculator (160-162), then 12
            }
            160 => {
                // the Display panel's expression in place of the normalized difference
                let mut sel = self.session.as_ref()?.info.sel;
                (sel.band, sel.nd_band) = (1, 0);
                sel.expr = janus_cube::calc::ExprId::intern("NDVI", "(N - R) / (N + R)");
                self.reopen_layer(self.active, sel, ctx);
                next(st, "reopen C with the expression (N - R) / (N + R)");
            }
            161 => {
                let s = self.session.as_ref()?;
                if self.opening.is_some()
                    || s.info.expr.is_none()
                    || !loaded(Some(s))
                    || !self.series.pins.first().is_some_and(|p| p.exact)
                {
                    return None;
                }
                let pin = &self.series.pins[0];
                let same = pin.values.len() == st.ndvi.len()
                    && pin.values.iter().zip(&st.ndvi).all(|(a, b)| (a - b).abs() < 1e-6 || (a.is_nan() && b.is_nan()));
                println!(
                    "    {}: the pin's {} dates as the normalized difference",
                    s.info.selection_text(),
                    st.ndvi.len()
                );
                if !same {
                    return fail("the expression's series should be the normalized difference's");
                }
                if !s.info.selection_text().contains("NDVI") {
                    return fail("the layer should be described by its expression's name");
                }
                // the raster calculator over it: the maximum over time, a layer of its own
                self.calc.show = true;
                self.calc.text = "ndvi = (N - R) / (N + R)\nmax(ndvi)".into();
                self.calc.primary = None;
                self.calc.cog = false;
                self.calc_run(ctx);
                next(st, "the expression's series is the NDVI; the calculator runs max(ndvi)");
            }
            162 => {
                if self.opening.is_some() || self.layers.len() != 2 || !loaded(self.layer_session(1)) {
                    if self
                        .emb_downloads
                        .iter()
                        .any(|d| d.status.lock().unwrap().state == crate::zeit::JobState::Failed)
                    {
                        return fail("the calculator's run failed (Tools > Tasks > Log)");
                    }
                    return None;
                }
                if self.active != 0 {
                    return fail("the series should stay active when the calculator's result is added");
                }
                let pin = self.series.pins.first()?.clone();
                let info = self.layer_session(1)?.info.clone();
                let v = janus_cube::with_thread_reader(&info, |r| r.read_pixel(0, pin.x, pin.y))?;
                let want = st.ndvi.iter().copied().filter(|v| v.is_finite()).fold(f32::NAN, f32::max);
                println!(
                    "    \"{}\": {} image, max(ndvi) at the pin {v:.4} (its series' maximum {want:.4})",
                    self.layers[1].name,
                    info.t()
                );
                if info.t() != 1 || (v - want).abs() > 1e-5 {
                    return fail("the calculator's max(ndvi) should be the pin's maximum NDVI");
                }
                if !self.calc.planned() {
                    return fail("the calculator should plan its text");
                }
                self.calc.show = false;
                self.remove_layer(1, ctx);
                st.season = 0;
                st.stats_gen = self.session.as_ref()?.stats_gen;
                self.set_test_season(0)?;
                next(st, "the calculator's result: an image, the maximum NDVI at the pin; closed");
                st.stage = 12;
            }
            // the season's statistics computed again (pump_season), then checked; the other season, then none
            12 => {
                let s = self.session.as_ref()?;
                if s.stats.is_none() || s.stats_gen == st.stats_gen {
                    return None;
                }
                st.stats_gen = s.stats_gen;
                if st.season < 2 {
                    if let Err(e) = self.check_season(st.season) {
                        return fail(e);
                    }
                    st.season += 1;
                    if st.season == 2 {
                        self.display.season = Default::default();
                    } else {
                        self.set_test_season(1)?;
                    }
                    return None;
                }
                next(st, "the season: the mean and the observations in it match the CPU");
                if std::env::var_os("JANUS_PYTHON").is_none() && run_python("values = [0.0]").is_err() {
                    println!("    Copy Python: skipped (no Python with zeit and rasterio; JANUS_PYTHON names one)");
                    return self.test_open_d(st, &input, t0, ctx);
                }
                // Copy Python (the C++'s stages 150-152): a pin on the cloudy half, Mann-Kendall on the chart
                self.clear_pins();
                self.add_pin(150, 30);
                let mk = janus_zeit::manifest().tool("mann_kendall")?;
                *self.zeit.params(mk) = mk.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                self.zeit.fit_only(Some(&mk.id));
                st.stage = 30;
            }
            30 => {
                let pin = self.series.pins.last()?.clone();
                let id = self.session.as_ref()?.info.id;
                if !pin.exact
                    || !self
                        .zeit
                        .fit_of("mann_kendall", id, crate::zeit::Which::Pin(pin.id), pin.x, pin.y)
                        .is_some_and(|r| r.is_ok())
                {
                    return None;
                }
                let code = self.python_snippet(crate::python::Which::Pin(pin.id))?;
                let v = match run_python(&code) {
                    Ok(v) => v,
                    Err(e) => return fail(&format!("the pin's code should run: {e}")),
                };
                let Some(nan) = same(&pin.values, &v, 1e-6) else {
                    return fail("the pin's code should read the pin's series");
                };
                if !code.contains("zeit.mann_kendall(series") {
                    return fail("the code should call Mann-Kendall");
                }
                println!(
                    "    pin: {} dates as in Janus ({nan} masked by Fmask), zeit.mann_kendall(series, ...) ran",
                    v.len()
                );
                if nan == 0 {
                    return fail("the pin should have dates masked by the quality band");
                }
                self.zeit.fit_only(None);
                self.set_roi_rect([20, 10, 120, 100]);
                next(st, "Copy Python of the pin: the same series, and the tool's call");
            }
            31 => {
                if !self.series.roi.complete() {
                    return None;
                }
                let code = self.python_snippet(crate::python::Which::Roi)?;
                let v = match run_python(&code) {
                    Ok(v) => v,
                    Err(e) => return fail(&format!("the ROI's code should run: {e}")),
                };
                if same(&self.series.roi.mean, &v, 1e-5).is_none() {
                    return fail("the ROI's code should give the ROI's mean at every date");
                }
                println!("    ROI (rectangle): the mean of {} dates as in Janus", v.len());
                self.clear_roi();
                self.clear_pins();
                next(st, "Copy Python of the ROI: its mean");
                // a multiband tool (the C++'s stages 153-154): CCDC on Zeit's Landsat series, when it is there
                let dir = std::path::Path::new(self.opts.inputs.first()?).parent().map(|d| d.join("landsat"));
                let Some(dir) = dir.filter(|d| d.is_dir()) else {
                    return self.test_open_d(st, &input, t0, ctx);
                };
                self.open_inputs(vec![dir.to_string_lossy().into_owned()], false, ctx);
            }
            32 => {
                let s = self.session.as_ref()?;
                if self.opening.is_some() || s.info.bands_per_date < 6 || !s.s.overview.complete() {
                    return None;
                }
                let (w, h) = (s.info.width, s.info.height);
                self.clear_pins();
                self.add_pin(w / 2, h / 2);
                let Some(ccdc) = janus_zeit::manifest().tool("ccdc") else {
                    return fail("CCDC should be among the tools");
                };
                *self.zeit.params(ccdc) = ccdc.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                self.zeit.fit_only(Some(&ccdc.id));
                next(st, "the Landsat series (six bands and Fmask per date), CCDC on a pin");
            }
            33 => {
                let pin = self.series.pins.last()?.clone();
                let id = self.session.as_ref()?.info.id;
                if !pin.exact
                    || !self
                        .zeit
                        .fit_of("ccdc", id, crate::zeit::Which::Pin(pin.id), pin.x, pin.y)
                        .is_some_and(|r| r.is_ok())
                {
                    return None;
                }
                let code = self.python_snippet(crate::python::Which::Pin(pin.id))?;
                let check = format!("{code}\nassert int(result1[\"n_segments\"].values.ravel()[0]) >= 1\n");
                let v = match run_python(&check) {
                    Ok(v) => v,
                    Err(e) => return fail(&format!("the CCDC code should run and fit segments: {e}")),
                };
                if same(&pin.values, &v, 1e-6).is_none() {
                    return fail("the CCDC code should read the pin's series");
                }
                if !code.contains("zeit.ccdc(bands_cube, qa=fmask") {
                    return fail("the code should call zeit.ccdc on the bands and their Fmask codes");
                }
                println!("    CCDC: the bands by role in reflectance x 10000 and the Fmask codes; zeit.ccdc ran");
                self.zeit.fit_only(None);
                self.clear_pins();
                next(st, "the CCDC code: its bands, Fmask codes and call");
                return self.test_open_d(st, &input, t0, ctx);
            }
            14 | 17 => {
                let path = input(if st.stage == 14 { 2 } else { 3 })?;
                if self.opening.is_some()
                    || self.layers.len() != 1
                    || self.layers[0].inputs.first() != Some(&path)
                    || !loaded(self.session.as_ref())
                    || self.classes.state == ClassState::Undecided
                {
                    return None;
                }
                let s = self.session.as_ref()?;
                let c = &self.classes;
                if c.state != ClassState::On {
                    return fail("the series should be detected as categorical");
                }
                let list: Vec<String> = c
                    .list
                    .iter()
                    .map(|e| format!("{}={} rgb({},{},{})", e.value, e.name, e.color[0], e.color[1], e.color[2]))
                    .collect();
                println!("    {}: {} classes: {}", s.info.description, c.list.len(), list.join(" "));
                if c.list.iter().map(|e| e.value).collect::<Vec<_>>() != [3, 15, 24, 33] {
                    return fail("classes should be 3, 15, 24, 33");
                }
                for (i, a) in c.list.iter().enumerate() {
                    if c.list[i + 1..].iter().any(|b| b.color == a.color) {
                        return fail("every class needs its own colour");
                    }
                }
                let forest = c.find(3)?;
                if st.stage == 14 && (forest.name != "Forest" || forest.color[0] != 31) {
                    return fail("the file's class names and colours should be used");
                }
                if !self.mode_available(Mode::Value) || self.mode_available(Mode::Mean) {
                    return fail("only the value mode makes sense");
                }
                next(st, "classes detected");
            }
            15 | 18 => {
                // the map's centre at the first date: forest, drawn in its class colour
                self.display.t = 0;
                self.display.mode = Mode::Value;
                self.test_fit();
                let img = self.test_render()?;
                let p = &img[(150 * SIZE[0] as usize + 200) * 4..][..3];
                let forest = self.classes.find(3)?.color;
                println!("    map centre: rgb{:?}, forest colour rgb{forest:?}", (p[0], p[1], p[2]));
                if (0..3).any(|i| (i32::from(p[i]) - i32::from(forest[i])).abs() > 2) {
                    return fail("the map should show the class colour");
                }
                // a pixel that changed class: its summary in the statistics
                let s = self.session.as_ref()?;
                let v = approx_series(s, 20, 40);
                for (k, v) in self.classes.summary(&s.info, &v, 0) {
                    println!("    {k:<18} {v}");
                }
                self.map.dirty = true;
                if let Err(e) = self.test_transect_classes() {
                    return fail(&e);
                }
                if let Err(e) = self.test_class_analysis() {
                    return fail(&e);
                }
                self.clear_transect();
                next(st, "map in class colours, class summary, transect in class colours");
                if st.stage == 16 {
                    let Some(e) = input(3) else { return ok() };
                    self.open_inputs(vec![e], false, ctx);
                    next(st, "open E (categorical, plain values)");
                } else {
                    // G: spectra (90-97); F (another CRS): the polygonal ROI (110-114)
                    st.stage = if input(5).is_some() {
                        90
                    } else if input(4).is_some() {
                        110
                    } else {
                        return ok();
                    };
                }
            }
            90..=100 => return self.test_spectra(st, &input, t0, ctx),
            110..=115 => return self.test_roi_polygon(st, &input, t0, ctx),
            120..=127 => return self.test_vectors(st, t0, ctx),
            50..=56 => return self.test_basemap(st, t0, ctx),
            57..=59 => return self.test_full_res(st, t0),
            70..=78 | 81..=89 => return self.test_misc(st, t0, ctx),
            130..=137 | 79 | 80 => return self.test_animation(st, &input, t0, ctx),
            170..=179 => return self.test_validation(st, &input, t0, ctx),
            180..=219 => return self.test_interface(st, &input, t0, ctx),
            _ => {}
        }
        None
    }
}

impl State {
    /// Waits (at most 30 s) for every date of the transect at full resolution, then takes them.
    fn transect_read(&mut self) -> Result<f64, String> {
        let w0 = std::time::Instant::now();
        while self.transect_rows_read() < self.transect.t_n {
            if w0.elapsed().as_secs_f64() > 30.0 {
                return Err("timeout reading the transect at full resolution".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        self.update_transect_rows();
        self.publish_transect();
        Ok(w0.elapsed().as_secs_f64() * 1000.0)
    }

    /// The transect's image pixel (x, y), drawn w x h.
    fn transect_pixel(&mut self, size: [u32; 2], x: u32, y: u32) -> Option<[u8; 3]> {
        self.render_transect(size);
        let (w, _, px) = self.gpu.as_ref()?.read_target(crate::transect::TRANSECT_SLOT)?;
        let p = ((y * w + x) * 4) as usize;
        Some([px[p], px[p + 1], px[p + 2]])
    }

    /// With A and B open, B active (src/app_selftest.cpp, selfTestCompare): a transect along B's row 100, columns
    /// 10 to 290 (exact values 2 + 0.01 (x - 50) + 0.1 t), filled from the overview at once, then at full
    /// resolution, drawn in the layer's colours; a polyline leaving B; the same line on A when A is made active.
    fn test_transect(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let t_n = self.session.as_ref().ok_or("no series")?.info.t();
        let exact = |x: i64, t: usize| 2.0 + 0.01 * (x - 50) as f64 + 0.1 * t as f64;
        self.set_transect(vec![[10.5, 100.5], [290.5, 100.5]]);
        let tr = &self.transect;
        if !tr.on || tr.n != 281 || tr.t_n != t_n {
            return Err("the transect should have 281 samples x every date".into());
        }
        println!(
            "    transect: {} samples x {} dates, {:.0} {}, {} dates from the overview at once",
            tr.n,
            tr.t_n,
            tr.length,
            tr.unit,
            tr.state.iter().filter(|&&s| s == 1).count()
        );
        if tr.unit != "m" || (tr.length - 280.0 * 30.0).abs() > 1e-6 {
            return Err("the transect should be 8400 m long".into());
        }
        {
            let s = self.session.as_ref().ok_or("no series")?;
            let ov = &s.s.overview;
            for t in 0..t_n {
                let l = ov.layer(t).ok_or("no overview")?;
                for i in 0..tr.n {
                    let ox = ((tr.px[i] as f64 * ov.w as f64 / s.info.width as f64) as usize).min(ov.w - 1);
                    let oy = ((tr.py[i] as f64 * ov.h as f64 / s.info.height as f64) as usize).min(ov.h - 1);
                    if tr.state[t] == 0 || tr.values[t * tr.n + i].to_bits() != l[oy * ov.w + ox].to_bits() {
                        return Err("the transect should be filled from the overview".into());
                    }
                }
            }
        }
        let ms = self.transect_read()?;
        let tr = &self.transect;
        let mut max_err = 0f64;
        for t in 0..t_n {
            if tr.state[t] != 2 {
                return Err("every date should be read at full resolution".into());
            }
            for i in 0..tr.n {
                max_err = max_err.max((f64::from(tr.values[t * tr.n + i]) - exact(tr.px[i], t)).abs());
            }
        }
        println!("    transect at full resolution in {ms:.0} ms, max error {max_err:.2e}");
        if max_err > 1e-4 {
            return Err("wrong full-resolution transect values".into());
        }
        // the image: cell (i, t) in the map's colours
        let (i, t) = (140, 5);
        let (n, v, x) = (tr.n, tr.values[t * tr.n + 140], tr.px[i]);
        let px = self
            .transect_pixel([(n * 2) as u32, (t_n * 2) as u32], 2 * i as u32 + 1, 2 * t as u32 + 1)
            .ok_or("no image")?;
        let (cmap, lo, hi) = self.transect_colors();
        let e = janus_gpu::colormaps::all()[cmap].sample(((v - lo) / (hi - lo)).clamp(0.0, 1.0));
        let label = &self.session.as_ref().ok_or("no series")?.info.layers[t].label;
        println!(
            "    transect image at ({x}, {label}): value {v:.3} -> rgb{px:?}, colormap rgb{:?}",
            [e[0], e[1], e[2]]
        );
        if (0..3).any(|k| px[k].abs_diff(e[k]) > 4) {
            return Err("the transect image should use the layer's colormap and range".into());
        }
        // a polyline on B: east along row 100, south along column 150, east again past B's edge (x = 300):
        // distances add up segment by segment, the samples off the layer stay without data, the others are B's
        self.set_transect(vec![[10.5, 100.5], [150.5, 100.5], [150.5, 180.5], [400.5, 180.5]]);
        let tr = &self.transect;
        if !tr.on || tr.pts.len() != 4 || tr.vdist.len() != 4 {
            return Err("the polyline should keep its 4 vertices".into());
        }
        let want = [0.0, 140.0 * 30.0, 220.0 * 30.0, 470.0 * 30.0];
        if tr.vdist.iter().zip(want).any(|(d, w)| (d - w).abs() > 1e-6) {
            return Err("the polyline's distances should add up along it".into());
        }
        self.transect_read()?;
        let tr = &self.transect;
        let (mut off, mut bad, mut max_err) = (0, 0, 0f64);
        for i in 0..tr.n {
            let outside = tr.at[i][0] >= 300.0;
            off += usize::from(tr.px[i] < 0);
            bad += usize::from(outside != (tr.px[i] < 0));
            for t in 0..t_n {
                let v = tr.values[t * tr.n + i];
                if tr.px[i] < 0 {
                    bad += usize::from(!v.is_nan());
                } else {
                    max_err = max_err.max((f64::from(v) - exact(tr.px[i], t)).abs());
                }
            }
        }
        println!(
            "    polyline transect: {} samples, {:.0} m, bends at {:.0} and {:.0} m, {off} samples off B, max error {max_err:.1e}",
            tr.n, tr.length, tr.vdist[1], tr.vdist[2]
        );
        if tr.n != 471 || !(90..=110).contains(&off) || bad > 0 || max_err > 1e-5 {
            return Err("the polyline should have 471 samples, ~100 off B (no data), B's values elsewhere".into());
        }
        // another active layer: the same line on its grid (A's pixels = B's + 100)
        self.set_transect(vec![[10.5, 100.5], [290.5, 100.5]]);
        self.transect_read()?;
        let b = self.active;
        self.set_active(1 - b, ctx);
        self.pump_transect(ctx);
        let (ax, an) = (self.transect.pts.first().map_or(f64::NAN, |p| p[0]), self.transect.n);
        self.set_active(b, ctx);
        self.pump_transect(ctx);
        let (bx, bn) = (self.transect.pts.first().map_or(f64::NAN, |p| p[0]), self.transect.n);
        println!(
            "    transect: starts at x {ax:.1} on A ({an} samples, clipped to A), {bx:.1} on B again ({bn} samples)"
        );
        if !self.transect.on || (ax - 110.5).abs() > 1e-3 || an != 191 || (bx - 10.5).abs() > 1e-3 || bn != 281 {
            return Err("the transect should follow the active layer".into());
        }
        Ok(())
    }

    /// Classes: 200 x 120 pixels; at the 1st date forest 11300, pasture 3900, urban 4800, water 4000; from the 5th
    /// date the block is pasture (src/app_selftest.cpp, selfTestAnalysis(2)).
    fn test_class_analysis(&mut self) -> Result<(), String> {
        if !self.count_class_timeline(1e9) {
            return Err("the class timeline should be complete".into());
        }
        let s = self.session.as_ref().ok_or("no series")?;
        let (ow, oh) = (s.s.overview.w, s.s.overview.h);
        let labels = (s.info.layers[0].label.clone(), s.info.layers[s.info.t() - 1].label.clone());
        let c = &self.analysis.classes;
        let t_n = c.counts.len();
        let count = |t: usize, v: i32| c.values.iter().position(|&x| x == v).map_or(0.0, |k| f64::from(c.counts[t][k]));
        let full = ow == 200 && oh == 120;
        let px = (ow * oh) as f64;
        let near = |got: f64, want: f64| if full { got == want } else { (got / px - want / 24000.0).abs() < 0.01 };
        println!(
            "    classes at {}: forest {}, pasture {}, urban {}, water {} (overview {ow} x {oh})",
            labels.0,
            count(0, 3),
            count(0, 15),
            count(0, 24),
            count(0, 33)
        );
        if !near(count(0, 3), 11300.0)
            || !near(count(0, 15), 3900.0)
            || !near(count(0, 24), 4800.0)
            || !near(count(0, 33), 4000.0)
            || !near(count(t_n - 1, 3), 11200.0)
            || !near(count(t_n - 1, 15), 4000.0)
        {
            return Err("the share of each class should follow the file".into());
        }
        let tr = self.count_transitions(0, t_n - 1);
        let k_n = tr.values.len();
        let (mut changed, mut forest_to_pasture) = (0u64, 0u64);
        for i in 0..k_n {
            for j in 0..k_n {
                if i != j {
                    changed += u64::from(tr.m[i * k_n + j]);
                }
                if tr.values[i] == 3 && tr.values[j] == 15 {
                    forest_to_pasture = u64::from(tr.m[i * k_n + j]);
                }
            }
        }
        println!(
            "    transitions {} -> {}: {changed} of {} pixels changed, forest -> pasture {forest_to_pasture}",
            labels.0, labels.1, tr.both
        );
        if !tr.done
            || changed != forest_to_pasture
            || !near(forest_to_pasture as f64, 100.0)
            || !near(tr.both as f64, 24000.0)
        {
            return Err("the only transition should be the forest block turning to pasture".into());
        }
        Ok(())
    }

    /// D / E: pasture (15) left of column 40, forest (3) right of it, at the first date: the transect in the class
    /// colours.
    fn test_transect_classes(&mut self) -> Result<(), String> {
        self.set_date(0);
        self.set_transect(vec![[10.5, 60.5], [190.5, 60.5]]);
        self.transect_read()?;
        let (n, t_n) = (self.transect.n as u32, self.transect.t_n as u32);
        for (i, cls) in [(5usize, 15), (100, 3)] {
            let px = self.transect_pixel([n, t_n], i as u32, 0).ok_or("no image")?;
            let e = self.classes.find(cls).ok_or("no such class")?.color;
            let (x, v) = (self.transect.px[i], self.transect.values[i]);
            println!("    transect at column {x}: class {v}, rgb{px:?}, class colour rgb{e:?}");
            if v != cls as f32 || (0..3).any(|k| px[k].abs_diff(e[k]) > 2) {
                return Err("the transect should be drawn in class colours".into());
            }
        }
        Ok(())
    }
}

/// G's wavelengths (tools/make_selftest_data.py): 420-1000 nm every 20, 1030-2440 every 30 but 1780-1930.
fn g_wavelengths() -> Vec<f64> {
    let mut nm: Vec<f64> = (420..=1000).step_by(20).map(f64::from).collect();
    nm.extend((1030..=2440).step_by(30).filter(|&v| !(1780..=1930).contains(&v)).map(f64::from));
    nm
}

/// G's reflectance at band t (nm[t]) of pixel (x, y): NaN in the water absorption.
fn g_expected(nm: &[f64], t: usize, x: usize, y: usize) -> f64 {
    if (1340.0..=1390.0).contains(&nm[t]) {
        return f64::NAN;
    }
    let r = 0.03 + 0.4 * (x as f64 / 119.0) / (1.0 + (-(nm[t] - 720.0) / 25.0).exp()) + 0.001 * (y % 10) as f64;
    (r * 10000.0).round() * 0.0001
}

impl State {
    /// The spectrum open: its axis (sorted wavelengths in nm, the chart broken at the gap without bands), the
    /// true-colour start at the near infrared, the values of every band, and that the Zeit tools do not apply.
    fn check_spectrum(&self) -> Result<(), &'static str> {
        let nm = g_wavelengths();
        let s = self.session.as_ref().ok_or("no series")?;
        let info = &s.info;
        let runs = janus_cube::spectral::plot_runs(info);
        let file = std::path::Path::new(&info.first_path).file_name().map(|f| f.to_string_lossy().to_string());
        println!(
            "    {}: {}, {} to {}, {} piece(s) on the chart",
            file.unwrap_or_default(),
            info.description,
            info.layers[0].label,
            info.layers[info.t() - 1].label,
            runs.len()
        );
        if !info.spectral || info.time_is_date {
            return Err("the file should open as a spectrum");
        }
        if info.t() != nm.len() {
            return Err("one layer per band expected");
        }
        if info.layers.iter().zip(&nm).any(|(l, &w)| (l.time - w).abs() > 1e-6) {
            return Err("wavelengths should be read in nm and sorted");
        }
        if info.layers[0].label != "420.0 nm" {
            return Err("layers should be labelled by wavelength");
        }
        if runs.len() != 2 || info.layers[runs[1].0].time != 1960.0 {
            return Err("the chart should break at 1750-1960 nm");
        }
        if self.classes.state != ClassState::Off {
            return Err("reflectances are not classes");
        }
        // the tools on a series in time are not offered, those on a spectrum (unmixing) are
        let on_series = janus_zeit::manifest().tools.iter().filter(|t| t.input.as_deref() != Some("layers"));
        for t in on_series {
            if crate::zeit::applicability(t, info).is_none() != (t.requires.time == "spectrum") {
                return Err("the Zeit tools work along time, not on a spectrum (but the spectrum's own)");
            }
        }
        let rgb = janus_cube::spectral::composite_layers(info, &janus_cube::spectral::COMPOSITES[0]);
        if rgb.is_none() || self.display.mode != Mode::Rgb || Some(self.display.rgb) != rgb {
            return Err("a spectrum should open in true colour");
        }
        if info.layers[self.display.t].time != 860.0 {
            return Err("a spectrum should open at the near infrared (860 nm)");
        }
        let (mut finite, mut missing) = (0, 0);
        for [x, y] in [[5, 7], [60, 33], [119, 79], [90, 2]] {
            let v = approx_series(s, x, y);
            for (t, &v) in v.iter().enumerate() {
                let e = g_expected(&nm, t, x, y);
                if e.is_nan() != v.is_nan() || (!e.is_nan() && (e - f64::from(v)).abs() > 1e-5) {
                    println!("    pixel ({x}, {y}) at {}: {v}, expected {e}", info.layers[t].label);
                    return Err("the spectrum's values differ from the file");
                }
                finite += usize::from(!e.is_nan());
                missing += usize::from(e.is_nan());
            }
        }
        println!("    {finite} values as written, {missing} no-data (water absorption)");
        Ok(())
    }

    /// Stages 90-97 (src/app_selftest.cpp, selfTestSpectra): G = synthetic spectra. 90 opens the ENVI file
    /// (wavelengths in its header, longest first); 91 checks it; 92 opens G, which has no rasters but an EnMAP
    /// product folder (GeoTIFF without wavelengths, METADATA.XML beside it, a quality layer that must not be
    /// opened), as its zip extracts; 93 checks that the same spectrum comes out; 94-95 add its NDVI as a layer (one
    /// dated map); 96-97 open G/series, three products as a series of scenes in NDVI. (The C++'s 98-100, the
    /// spectrum's principal components and similarity, come with the embeddings.)
    fn test_spectra(
        &mut self,
        st: &mut UiTest,
        input: &dyn Fn(usize) -> Option<String>,
        t0: std::time::Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
        let fail = |what: &str| {
            println!("FAIL: {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
        };
        let g = std::path::PathBuf::from(input(5)?);
        let nm = g_wavelengths();
        let ready = |s: &State| {
            s.opening.is_none()
                && s.layers.len() == 1
                && loaded(s.session.as_ref())
                && s.classes.state != ClassState::Undecided
        };
        // the NDVI's bands (1-based) and the shown series of a pixel
        let ndvi = |s: &State| -> Option<(i32, i32)> {
            let info = &s.session.as_ref()?.info;
            janus_cube::spectral::index_bands(&info.band_nm, &janus_cube::spectral::INDICES[0])
        };
        match st.stage {
            90 => {
                self.open_inputs(vec![g.join("envi").join("spectrum.bsq").to_string_lossy().into()], false, ctx);
                next(st, "open G (ENVI spectrum)");
            }
            91 => {
                if !ready(self) {
                    return None;
                }
                if let Err(e) = self.check_spectrum() {
                    return fail(e);
                }
                next(st, "ENVI spectrum: wavelengths, chart gaps, true colour, values");
            }
            92 => {
                // G itself: no rasters, the product in a subfolder (as its zip extracts)
                self.open_inputs(vec![g.to_string_lossy().into()], false, ctx);
                next(st, "open G, whose subfolder is an EnMAP product (GeoTIFF + METADATA.XML)");
            }
            93 => {
                if !ready(self) {
                    return None;
                }
                let first = std::path::PathBuf::from(&self.session.as_ref()?.info.first_path);
                if !janus_cube::remote::is_enmap_spectral_image(&first) {
                    return fail("the folder should open its spectral image alone");
                }
                if let Err(e) = self.check_spectrum() {
                    return fail(e);
                }
                next(st, "EnMAP product: wavelengths from METADATA.XML, quality layers left out");
            }
            94 => {
                let spectrum_bands = ndvi(self);
                if !self.add_spectral_index_layer(&janus_cube::spectral::INDICES[0], ctx) {
                    return fail("NDVI should be offered on the spectrum");
                }
                st.index_bands = spectrum_bands.unwrap_or_default();
                next(st, "add NDVI of the EnMAP scene as a layer");
            }
            95 => {
                if self.opening.is_some() || self.layers.len() != 2 || !self.session.as_ref()?.s.overview.complete() {
                    return None;
                }
                let s = self.session.as_ref()?;
                let info = &s.info;
                let (a, b) = st.index_bands;
                let name = &self.layers.get(self.active)?.name;
                println!("    {name}: {}, {}, {} date(s)", info.description, info.layers[0].label, info.t());
                if info.spectral || info.t() != 1 || info.sel.band != a || info.sel.nd_band != b {
                    return fail("the index should be one map of two bands");
                }
                if nm[a as usize - 1] != 860.0 || (nm[b as usize - 1] - 650.0).abs() != 10.0 {
                    return fail("NDVI should use 860 and 640 or 660 nm");
                }
                if !info.time_is_date || info.layers[0].label != "2022-08-10" {
                    return fail("the index should take the scene's date");
                }
                if !name.starts_with("NDVI - ") {
                    return fail("the index layer should be named after it");
                }
                for [x, y] in [[5, 7], [60, 33], [119, 79]] {
                    let (ea, eb) = (g_expected(&nm, a as usize - 1, x, y), g_expected(&nm, b as usize - 1, x, y));
                    let e = (ea - eb) / (ea + eb);
                    let v = approx_series(s, x, y);
                    if v.len() != 1 || (f64::from(v[0]) - e).abs() > 1e-5 {
                        println!("    ({x}, {y}): {:?}, expected {e}", v.first());
                        return fail("the index's values differ");
                    }
                }
                next(st, "NDVI of a scene: one map, dated, values of (NIR - red) / (NIR + red)");
            }
            96 => {
                self.open_inputs(vec![g.join("series").to_string_lossy().into()], false, ctx);
                next(st, "open G/series (three EnMAP products, a series of scenes)");
            }
            97 => {
                if self.opening.is_some() || self.layers.len() != 1 || !self.session.as_ref()?.s.overview.complete() {
                    return None;
                }
                let (a, b) = ndvi(self)?;
                let s = self.session.as_ref()?;
                let info = &s.info;
                println!(
                    "    {}; band names \"{}\"...; {} to {}",
                    info.description,
                    info.band_names.first()?,
                    info.layers[0].label,
                    info.layers[info.t() - 1].label
                );
                if info.spectral
                    || info.t() != 3
                    || !info.time_is_date
                    || info.bands_per_date != nm.len()
                    || info.band_nm.len() != nm.len()
                {
                    return fail("three dates of every band expected");
                }
                if info.layers[1].label != "2023-07-05" {
                    return fail("the dates should come from the product names");
                }
                if !info.band_names[0].contains("420.0 nm") {
                    return fail("band names should give the wavelength");
                }
                if info.sel.band != a || info.sel.nd_band != b {
                    return fail("a series with wavelengths should open in NDVI");
                }
                for [x, y] in [[5, 7], [60, 33]] {
                    let v = approx_series(s, x, y);
                    for k in 0..3 {
                        let ea = g_expected(&nm, a as usize - 1, x, y) + 0.01 * k as f64;
                        let eb = g_expected(&nm, b as usize - 1, x, y) + 0.01 * k as f64;
                        let e = (ea - eb) / (ea + eb);
                        if v.len() != 3 || (f64::from(v[k]) - e).abs() > 1e-5 {
                            println!("    ({x}, {y}) at {k}: {:?}, expected {e}", v.get(k));
                            return fail("the series' NDVI differs");
                        }
                    }
                }
                next(st, "series of scenes: dates from the names, wavelengths in the band names, NDVI");
            }
            98 => {
                self.open_inputs(vec![g.join("envi").join("spectrum.bsq").to_string_lossy().into()], false, ctx);
                next(st, "open G's ENVI spectrum again");
            }
            99 => {
                if !ready(self) {
                    return None;
                }
                self.show_spectral_analysis();
                st.frames = 0;
                next(st, "its principal components and similarity (Embeddings panel)");
            }
            100 => {
                let r = self.test_spectral_views(st, &nm);
                if r < 0 {
                    return None;
                }
                if r > 0 {
                    return fail("spectral principal components or similarity");
                }
                next(st, "spectrum: principal components (bands without data left out), similarity as in the file");
                if input(4).is_none() {
                    println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                    return Some(0);
                }
                st.stage = 110; // the polygonal ROI
            }
            _ => {}
        }
        None
    }
}

impl State {
    /// The active layer is G's ENVI spectrum, its analysis asked for (stages 98-100 of the C++): its principal
    /// components (the bands without data read as 0), then the similarity to the spectrum under the cursor against
    /// the file's. -1 waiting, 0 passed, 1 failed (printed).
    fn test_spectral_views(&mut self, st: &mut UiTest, nm: &[f64]) -> i32 {
        let t_n = nm.len();
        let Some(e) = self.layers.get(self.active).and_then(|l| l.emb.as_ref()).filter(|e| e.spectral) else {
            println!("    the spectrum should have its analysis layer");
            return 1;
        };
        let Some(s) = e.store.clone() else { return -1 };
        if !s.complete() || e.basis.is_none() || e.img.first().is_none_or(|i| !i.has_texture()) {
            return -1;
        }
        if st.frames == 0 {
            // the principal components of the scene
            let b = e.basis.clone().unwrap();
            let skipped = s.ranges().map_or(0, |r| r.skip.iter().filter(|&&k| k != 0).count());
            let valid = (0..s.pixels()).filter(|&p| s.valid(0, p)).count();
            let pct = |c: usize| 100.0 * b.eig[c] / b.total;
            println!(
                "    store {} x {}, {} dimensions ({skipped} without data), {valid} of {} pixels valid; PC1-3 {:.1}% {:.1}% {:.1}%",
                s.w,
                s.h,
                s.d,
                s.pixels(),
                pct(0),
                pct(1),
                pct(2)
            );
            if s.d != t_n || skipped != 2 {
                println!("    the store should read every band, the 2 without data as 0");
                return 1;
            }
            if valid != s.pixels() {
                println!("    bands without data must not make the spectra invalid");
                return 1;
            }
            if !e.img[0].rgba || b.eig[0] < 0.5 * b.total {
                println!("    the principal components should be an RGB image, PC1 the reflectance gradient");
                return 1;
            }
            let e = self.layers[self.active].emb.as_mut().unwrap();
            (e.view, e.ref_kind) = (crate::embeddings::View::Similarity, 0);
            e.reset_reference();
            self.series.hover = None;
            self.set_hover(Some((60, 33)));
            self.map.dirty = true;
            st.frames = 1;
            return -1;
        }
        let img = &e.img[0];
        if e.reference.is_empty() || img.rgba || img.values.len() != s.pixels() {
            return -1;
        }
        let cosine = |x0: usize, y0: usize, x1: usize, y1: usize| {
            let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
            for t in 0..t_n {
                let (a, b) = (g_expected(nm, t, x0, y0), g_expected(nm, t, x1, y1));
                if a.is_nan() || b.is_nan() {
                    continue;
                }
                ab += a * b;
                aa += a * a;
                bb += b * b;
            }
            ab / (aa * bb).sqrt()
        };
        let mut worst = 0f64;
        for [x, y] in [[60, 33], [5, 7], [119, 79], [90, 2]] {
            let (want, got) = (cosine(60, 33, x, y), f64::from(img.values[y * s.w + x]));
            worst = worst.max((got - want).abs());
            if (got - want).abs() > 0.005 {
                println!("    similarity at ({x}, {y}): {got:.5}, expected {want:.5} from the file");
                return 1;
            }
        }
        println!("    spectral similarity to the cursor: max |map - file| {worst:.2e} (Int8 store); {}", e.ref_text);
        self.layers[self.active].emb.as_mut().unwrap().view = crate::embeddings::View::Band;
        0
    }
}

/// B's pixels (A's are B's + 100 in x). Concave (an L), so its bounding box is not the ROI.
const L_POLY: [f64; 12] = [20.0, 30.0, 150.0, 30.0, 150.0, 80.0, 70.4, 80.0, 70.4, 170.0, 20.0, 170.0];

impl State {
    /// Stages 110-114 (src/app_roi.cpp, selfTestRoiPolygon): a concave polygon drawn on B (vertices clicked, the
    /// last one twice as a double click does), its statistics on B, on A (same CRS, shifted) and on F (another
    /// CRS) against the pixels whose centres are inside. (The C++'s 115, the outline in a PNG export, comes with
    /// the exports.)
    fn test_roi_polygon(
        &mut self,
        st: &mut UiTest,
        input: &dyn Fn(usize) -> Option<String>,
        t0: std::time::Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
        let fail = |what: &str| {
            println!("FAIL: {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
        };
        let ready = |s: &State, n: usize| {
            s.opening.is_none() && s.layers.len() == n && (0..n).all(|i| loaded(s.layer_session(i)))
        };
        match st.stage {
            110 => {
                self.open_inputs(vec![input(0)?], false, ctx);
                next(st, "polygonal ROI: open B");
            }
            111 => {
                if !ready(self, 1) {
                    return None;
                }
                self.open_inputs(vec![self.opts.inputs.first()?.clone()], true, ctx);
                next(st, "add A");
            }
            112 => {
                if !ready(self, 2) {
                    return None;
                }
                self.open_inputs(vec![input(4)?], true, ctx);
                next(st, "add F (another CRS)");
            }
            113 => {
                if !ready(self, 3) {
                    return None;
                }
                self.set_active(0, ctx); // B
                self.set_chart_layers(1);
                self.test_fit();
                // the vertices as clicked; the last one twice (a double click's two clicks)
                self.series.poly.on = true;
                self.series.poly.vertices = L_POLY.as_chunks::<2>().0.iter().map(|v| [v[0], v[1]]).collect();
                let last = *self.series.poly.vertices.last()?;
                self.series.poly.vertices.push(last);
                self.finish_polygon();
                let p = &self.series.roi_poly;
                if self.series.poly.on || p.len() != 1 || p[0].len() != L_POLY.len() {
                    return fail("closing the polygon should leave polygon mode with its 6 vertices as the ROI");
                }
                if self.series.roi_rect != Some([20, 30, 150, 170]) {
                    return fail("the ROI's bounding box should be the polygon's");
                }
                if (janus_geo::plane::area(p) - (130.0 * 50.0 + 50.4 * 90.0)).abs() > 1e-3 {
                    return fail("the area of the L");
                }
                if self.roi_contains(100.0, 120.0) || !self.roi_contains(30.0, 120.0) || !self.roi_contains(100.0, 50.0)
                {
                    return fail("roi_contains: inside the L, not in its bounding box's notch");
                }
                next(st, "polygon closed as the ROI of every layer");
            }
            114 => {
                if !self.series.roi.complete()
                    || !self.layers[1].series.roi.complete()
                    || !self.layers[2].series.roi.complete()
                {
                    return None;
                }
                // the active layer: every date's statistics from the pixels whose centres are inside
                let info = self.session.as_ref()?.info.clone();
                let r = self.series.roi.data.clone()?;
                let (x0, y0, w, h) = (r.x0, r.y0, r.x1 - r.x0, r.y1 - r.y0);
                if r.sampled || r.bw != w {
                    return fail("the L should be read at full resolution");
                }
                let poly = self.series.roi_poly.clone();
                let mut reader = janus_cube::CubeReader::new(info.clone());
                let mut buf = vec![0f32; w * h];
                let (mut inside, mut worst) = (0, 0f32);
                for t in 0..info.t() {
                    if !reader.read_window(t, (x0, y0, w, h), &mut buf, (w, h)) {
                        return fail("B cannot be read");
                    }
                    let mut v: Vec<f32> = (0..w * h)
                        .filter(|k| {
                            janus_geo::plane::contains(&poly, (x0 + k % w) as f64 + 0.5, (y0 + k / w) as f64 + 0.5)
                        })
                        .map(|k| buf[k])
                        .collect();
                    inside = v.len();
                    let e = janus_cube::stats::sample_stats(&mut v);
                    let g = r.per_t[t].get()?;
                    if e.n != g.n {
                        return fail("the ROI's pixel count on B differs from the pixels inside");
                    }
                    if e.n > 0 {
                        worst = worst.max((e.mean - g.mean).abs()).max((e.p90 - g.p90).abs());
                    }
                }
                println!(
                    "    B: {inside} of {w} x {h} pixels of the bounding box inside the L, max |mean, p90 - CPU| {worst:.2e}"
                );
                if worst > 1e-5 {
                    return fail("the ROI's statistics on B differ from the CPU's");
                }
                // A (B + 100 px) and F (UTM 22S): the cells of their masks against the centres inside
                let check = |i: usize, name: &str, inside: &dyn Fn(f64, f64) -> bool, tolerance: f64| -> bool {
                    let Some(r) = &self.layers[i].series.roi.data else { return false };
                    let (mut ones, mut wrong) = (0usize, 0usize);
                    for j in 0..r.bh {
                        for k in 0..r.bw {
                            let m = r.mask.is_empty() || r.mask[j * r.bw + k];
                            ones += usize::from(m);
                            wrong += usize::from(m != inside((r.x0 + k) as f64 + 0.5, (r.y0 + j) as f64 + 0.5));
                        }
                    }
                    println!(
                        "    {name}: {} x {} px read at ({}, {}), {ones} inside, {wrong} differ from the exact test",
                        r.bw, r.bh, r.x0, r.y0
                    );
                    ones > 0 && wrong as f64 <= tolerance * ones as f64
                };
                if !check(1, "A", &|x, y| janus_geo::plane::contains(&poly, x - 100.0, y), 0.0) {
                    return fail("A's ROI should be the L moved 100 px");
                }
                // F: the L's straight edges are curves there (16 points per side of its box)
                let f = &self.layers[2].place;
                let on_f = |x: f64, y: f64| {
                    f.to_active(x, y).is_some_and(|(bx, by)| janus_geo::plane::contains(&poly, bx, by))
                };
                if f.reproj.is_none() || !check(2, "F", &on_f, 0.01) {
                    return fail("F's ROI should be the L reprojected");
                }
                // Copy Python of the L: its rings rasterized give the same mean
                if std::env::var_os("JANUS_PYTHON").is_some() || run_python("values = [0.0]").is_ok() {
                    let code = self.python_snippet(crate::python::Which::Roi)?;
                    let v = match run_python(&code) {
                        Ok(v) => v,
                        Err(e) => return fail(&format!("the polygon's code should run: {e}")),
                    };
                    if !code.contains("rasterize(") || same(&self.series.roi.mean, &v, 1e-5).is_none() {
                        return fail("the polygon's code should give the ROI's mean at every date");
                    }
                    println!("    Copy Python of the L: its mean at {} dates as in Janus", v.len());
                }
                // the outline in a PNG with the marks
                let o =
                    crate::export_map::PngOptions { marks: true, label: false, legend: false, ..Default::default() };
                let dir = std::env::temp_dir().join("janus-selftest-roi");
                let _ = std::fs::create_dir_all(&dir);
                let Some(job) = self.export_png(dir.join("roi.png").to_string_lossy().into_owned(), o, ctx) else {
                    return fail("the PNG did not start");
                };
                st.jobs = vec![job];
                next(st, "the L's statistics on B, A and F");
            }
            115 => {
                let j = st.jobs[0].clone();
                j.wait();
                if j.state() != crate::exports::ExportState::Done {
                    return fail(&format!("the PNG with the ROI should be written: {}", j.error()));
                }
                let Ok(ds) = gdal::Dataset::open(&j.path) else { return fail("the PNG cannot be opened") };
                let (w, h) = ds.raster_size();
                let band = |b: usize| -> Vec<u8> {
                    ds.rasterband(b)
                        .and_then(|r| r.read_as::<u8>((0, 0), (w, h), (w, h), None))
                        .map(|b| b.into_shape_and_vec().1)
                        .unwrap_or_default()
                };
                let (r, g, b) = (band(1), band(2), band(3));
                // the middle of the notch's two edges (yellow), and the notch itself (the map's colours)
                let ppp = ctx.pixels_per_point();
                let grid = self.grid()?;
                let rect = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(self.map.canvas[0].floor(), self.map.canvas[1].floor()),
                );
                let yellow = |x: f64, y: f64| {
                    let p = self.map.screen_of(rect, x / grid.fx, y / grid.fy);
                    let (px, py) = (((p.x * ppp) as usize).min(w - 1), ((p.y * ppp) as usize).min(h - 1));
                    let i = py * w + px;
                    r[i] > 200 && g[i] > 160 && b[i] < 120
                };
                if !yellow(110.0, 80.0) || !yellow(70.4, 125.0) {
                    return fail("the PNG should show the L's edges in yellow");
                }
                if yellow(110.0, 125.0) {
                    return fail("the notch of the L should not be outlined");
                }
                drop(ds);
                if let Some(d) = std::path::Path::new(&j.path).parent() {
                    let _ = std::fs::remove_dir_all(d);
                }
                st.jobs.clear();
                self.clear_roi();
                next(st, "the L's outline in the exported PNG");
                st.stage = 120; // vector layers
            }
            _ => {}
        }
        None
    }
}

/// Runs Copy Python's code (the Python of JANUS_PYTHON, else `python`) and reads the series it computed.
fn run_python(code: &str) -> Result<Vec<f64>, String> {
    let dir = std::env::temp_dir().join("janus-selftest-python");
    let _ = std::fs::create_dir_all(&dir);
    let script = dir.join("snippet.py");
    let text = format!(
        "import zeit, rasterio\n{code}\nimport json, math\n\
         print(\"JANUS_CHECK \" + json.dumps([float(v) if math.isfinite(v) else None for v in values]))\n"
    );
    std::fs::write(&script, text).map_err(|e| e.to_string())?;
    let python = std::env::var("JANUS_PYTHON").unwrap_or_else(|_| "python".into());
    let out = std::process::Command::new(&python).arg(&script).output().map_err(|e| format!("{python}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("JANUS_CHECK "))
        .ok_or_else(|| format!("exit {:?}:\n{stdout}\n{}", out.status.code(), String::from_utf8_lossy(&out.stderr)))?;
    let v: Vec<Option<f64>> = serde_json::from_str(line).map_err(|e| e.to_string())?;
    Ok(v.into_iter().map(|x| x.unwrap_or(f64::NAN)).collect())
}

/// The same series (within `tol` relative), and how many dates have no value; None if not.
fn same(a: &[f32], b: &[f64], tol: f64) -> Option<usize> {
    if a.len() != b.len() {
        return None;
    }
    let mut nan = 0;
    for (&x, &y) in a.iter().zip(b) {
        if x.is_nan() != y.is_nan() {
            return None;
        }
        if x.is_nan() {
            nan += 1;
        } else if (f64::from(x) - y).abs() > tol * (1.0 + y.abs()) {
            return None;
        }
    }
    Some(nan)
}
