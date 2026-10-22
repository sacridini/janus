//! `--selftest-ui` stages 57-59 (src/app_selftest.cpp, selfTestFullRes: the C++'s 30-32), with B active: the
//! full-resolution cache. 57: a cache built in a temporary folder by the overview pass (full dates read once for
//! both), compared bit by bit with GDAL: the overview it fills, pixel series (block corners and edges), windows and
//! subsampled windows; then B's own cache, built through the session. 58 waits for it and asks for a pin and an ROI,
//! 59 checks that both came from it and equal the source.

use std::sync::atomic::Ordering;

use janus_cache::FullResCache;
use janus_cube::CubeReader;
use janus_cube::stats::sample_stats;

use super::UiTest;
use crate::app::State;

/// Bit for bit, NaN equal to NaN.
fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
}

impl State {
    pub(super) fn test_full_res(&mut self, st: &mut UiTest, t0: std::time::Instant) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
            st.frames = 0;
        };
        let info = self.session.as_ref()?.info.clone();
        let (w, h, t) = (info.width, info.height, info.t());
        match st.stage {
            57 => {
                let nanos =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros());
                let dir = std::env::temp_dir().join(format!("janus-selftest-{nanos}"));
                let _ = std::fs::create_dir_all(&dir);
                let (mut series, mut windows, bytes);
                {
                    let c = FullResCache::new(info.clone(), dir.clone());
                    if !c.start(2, 1 << 40) {
                        return fail(&format!("could not start a full-resolution cache: {}", c.error()));
                    }
                    let mut r = CubeReader::new(info.clone());
                    let (ow, oh) = (97, 61); // subsampled like an overview, odd factors
                    let (mut ov, mut reference) = (vec![0f32; ow * oh], vec![0f32; ow * oh]);
                    for i in 0..t {
                        if c.build_with_overview(i, &mut ov, ow, oh) != Some(true) {
                            return fail("the overview pass should build every date");
                        }
                        r.read_window(i, (0, 0, w, h), &mut reference, (ow, oh));
                        if !same(&ov, &reference) {
                            return fail("the overview taken from full dates differs from GDAL's");
                        }
                    }
                    if !c.complete() || c.dates_with_overview() != t {
                        return fail("the cache should be complete");
                    }
                    bytes = c.bytes();
                    let px = [
                        (0, 0),
                        (w - 1, h - 1),
                        (63, 63),
                        (64, 64),
                        (63, 64),
                        (w - 1, 0),
                        (0, h - 1),
                        (128, 127),
                        (w / 2, h / 2),
                        (w - 2, 129),
                    ];
                    series = 0;
                    for (x, y) in px {
                        let (mut a, mut got) = (vec![0f32; t], vec![false; t]);
                        if c.read_series(x, y, &mut a, &mut got) != t {
                            return fail("every date should be cached");
                        }
                        let b: Vec<f32> = (0..t).map(|i| r.read_pixel(i, x, y).unwrap_or(f32::NAN)).collect();
                        if !same(&a, &b) {
                            return fail("a series from the cache differs from the source");
                        }
                        series += 1;
                    }
                    let win = [
                        (0, 0, w, h, w, h),
                        (10, 7, 200, 150, 200, 150),
                        (5, 3, w - 9, h - 5, 97, 61),
                        (63, 63, 2, 2, 2, 2),
                        (100, 50, 150, 120, 40, 33),
                        (0, 0, w, h, 256 / 4, 33),
                        (w - 70, h - 70, 70, 70, 70, 70),
                        (1, 1, w - 1, h - 1, w / 3, h / 7),
                    ];
                    windows = 0;
                    for (x0, y0, ww, wh, bw, bh) in win {
                        let (mut x, mut y) = (vec![0f32; bw * bh], vec![0f32; bw * bh]);
                        for i in 0..t {
                            if !c.read_window(i, (x0, y0, ww, wh), &mut x, (bw, bh)) {
                                return fail("window not cached");
                            }
                            r.read_window(i, (x0, y0, ww, wh), &mut y, (bw, bh));
                            if !same(&x, &y) {
                                return fail(&format!(
                                    "window ({x0}, {y0}, {ww} x {wh} -> {bw} x {bh}) differs from the source"
                                ));
                            }
                        }
                        windows += 1;
                    }
                }
                let _ = std::fs::remove_dir_all(&dir);
                println!(
                    "    {t} dates read once for the overview and the cache (lossless, {:.0} KB, raw {:.0} KB); {series} \
                     series and {windows} windows x {t} dates equal to GDAL's",
                    bytes as f64 / 1024.0,
                    (w * h * t * 4) as f64 / 1024.0
                );
                let s = &self.session.as_ref()?.s;
                if !s.build_full_res() {
                    return fail(&format!("B's cache could not start: {}", s.full_res.error()));
                }
                next(st, "full-resolution cache (temporary) equal to the source");
            }
            58 => {
                let c = &self.session.as_ref()?.s.full_res;
                if !c.complete() {
                    return None;
                }
                st.hits = c.series_hits();
                self.add_pin(77, 131); // a pin of its own: the earlier one is remapped later
                self.set_roi_rect([13, 21, 163, 191]);
                next(st, "B's own cache complete; a pin and an ROI requested");
            }
            59 => {
                let pin = self.series.pins.last()?.clone();
                let roi = self.series.roi.data.clone()?;
                if !pin.exact || !roi.complete() {
                    return None;
                }
                let c = self.session.as_ref()?.s.full_res.clone();
                let mut r = CubeReader::new(info.clone());
                let v: Vec<f32> = (0..t).map(|i| r.read_pixel(i, pin.x, pin.y).unwrap_or(f32::NAN)).collect();
                if c.series_hits() == st.hits {
                    return fail("the series should come from the cache");
                }
                if !same(&pin.values, &v) {
                    return fail("the series differs from the source");
                }
                if roi.cached_dates.load(Ordering::Relaxed) != t {
                    return fail("the ROI should come from the cache");
                }
                for i in 0..t {
                    let mut buf = vec![0f32; roi.bw * roi.bh];
                    r.read_window(i, (roi.x0, roi.y0, roi.x1 - roi.x0, roi.y1 - roi.y0), &mut buf, (roi.bw, roi.bh));
                    let a = sample_stats(&mut buf);
                    let Some(b) = roi.per_t[i].get() else { return fail("every date of the ROI should be read") };
                    let bits = |x: f32, y: f32| x.to_bits() == y.to_bits();
                    if a.n != b.n
                        || !bits(a.mean, b.mean)
                        || !bits(a.p10, b.p10)
                        || !bits(a.p50, b.p50)
                        || !bits(a.p90, b.p90)
                    {
                        return fail("the ROI statistics differ from the source's");
                    }
                }
                println!(
                    "    series from the cache in {:.2} ms, ROI {} x {} x {t} dates in {:.1} ms",
                    c.last_series_ms(),
                    roi.x1 - roi.x0,
                    roi.y1 - roi.y0,
                    roi.ms().unwrap_or(0.0)
                );
                self.clear_roi();
                self.remove_pin(pin.id);
                next(st, "series and ROI through B's cache equal the source");
                st.stage = 6;
            }
            _ => {}
        }
        None
    }
}
