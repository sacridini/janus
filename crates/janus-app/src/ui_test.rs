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
    /// the exit code, once decided (the window takes another frame to close)
    result: Option<i32>,
}

fn loaded(s: Option<&Session>) -> bool {
    s.is_some_and(|s| s.s.overview.complete() && (s.info.t() < 2 || s.stats.is_some()))
}

impl State {
    pub fn ui_selftest_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        if self.ui_test.result.is_some() {
            return self.ui_test.result;
        }
        let mut st = std::mem::take(&mut self.ui_test);
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
                self.layers[0].visible = false;
                let none = centre(&self.test_render()?);
                println!("    map center with both hidden: rgb{none:?}");
                if none.iter().any(|&c| c > 40) {
                    return fail("with every layer hidden the map should be empty");
                }
                self.layers[0].visible = true;
                self.layers[1].visible = true;
                self.map.dirty = true;
                next(st, "layer drawing and visibility, a map panel");
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
            }
            7 => {
                self.remove_layer(1, ctx);
                if self.layers.len() != 1 || self.active != 0 {
                    return fail("closing B should leave A active");
                }
                next(st, "layer closed");
                let Some(c) = input(1) else { return ok() };
                self.basemap.ui.on = true; // ticked: opening another series hides it again
                self.open_inputs(vec![c], false, ctx);
                next(st, "open C (bands per date)");
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
                st.season = 0;
                st.stats_gen = self.session.as_ref()?.stats_gen;
                self.set_test_season(0)?;
                next(st, "reopened in place: name, pin and view kept, values are NDVI, clouds masked; composites");
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
                self.zeit.pixel_tool = Some(mk.id.clone());
                self.zeit.version += 1;
                st.stage = 30;
            }
            30 => {
                let pin = self.series.pins.last()?.clone();
                let id = self.session.as_ref()?.info.id;
                if !pin.exact
                    || !self.zeit.fit_of(id, crate::zeit::Which::Pin(pin.id), pin.x, pin.y).is_some_and(|r| r.is_ok())
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
                self.zeit.pixel_tool = None;
                self.zeit.version += 1;
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
                next(st, "map in class colours, class summary");
                if st.stage == 16 {
                    let Some(e) = input(3) else { return ok() };
                    self.open_inputs(vec![e], false, ctx);
                    next(st, "open E (categorical, plain values)");
                } else {
                    return ok();
                }
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
