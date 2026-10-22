//! `--selftest-ui` stages 130-137 (src/app_animation.cpp, selfTestAnimation): the GIF writer and reader alone;
//! A animated over 5 dates as a GIF, each frame against the map rendered again at its date (the stamp and the scale
//! bar only in the GIF), its delays and loop; the MP4 through ffmpeg (if found); an export cancelled after 3 frames
//! leaves no file and the date restored.

use std::sync::atomic::Ordering;

use janus_gpu::Mode;

use super::UiTest;
use crate::animation::AnimOptions;
use crate::app::State;
use crate::exports::ExportState;

/// Mean and largest |a - b| over a rectangle of two RGBA images of width w.
fn compare(a: &[u8], b: &[u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> (f64, i32) {
    let (mut s, mut n, mut m) = (0f64, 0usize, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            for c in 0..3 {
                let i = (y * w + x) * 4 + c;
                let d = (i32::from(a[i]) - i32::from(b[i])).abs();
                s += f64::from(d);
                m = m.max(d);
                n += 1;
            }
        }
    }
    (if n > 0 { s / n as f64 } else { 0.0 }, m)
}

/// Pixels of a rectangle brighter than 230 in every channel (white text, the scale bar).
fn whites(a: &[u8], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> i64 {
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = &a[(y * w + x) * 4..];
            n += i64::from(p[0] > 230 && p[1] > 230 && p[2] > 230);
        }
    }
    n
}

/// The GIF writer alone: an image of <= 256 colours comes back exact, one of many colours within the palette's
/// error, dithered or not.
fn gif_codec(dir: &std::path::Path) -> Result<(), String> {
    let (w, h) = (173usize, 91usize); // odd sizes, LZW resets (> 4096 codes)
    let mut img = vec![0u8; w * h * 4];
    let mut smooth = img.clone();
    for y in 0..h {
        for x in 0..w {
            let k = (x * 7 + y * 13 + (x * y) % 11) % 251; // 251 colours
            img[(y * w + x) * 4..][..4].copy_from_slice(&[(k * 37) as u8, (k * 11 + 3) as u8, (255 - k) as u8, 255]);
            smooth[(y * w + x) * 4..][..4].copy_from_slice(&[
                (x * 255 / (w - 1)) as u8,
                (y * 255 / (h - 1)) as u8,
                ((x + y) * 255 / (w + h - 2)) as u8,
                255,
            ]);
        }
    }
    let path = dir.join("codec.gif").to_string_lossy().into_owned();
    let mut g = crate::gif::GifWriter::open(&path, w, h, 0)?;
    g.add_frame(&img, 7, false)?;
    g.add_frame(&smooth, 8, false)?;
    g.add_frame(&smooth, 9, true)?;
    g.close()?;
    let d = crate::gif::decode(&path)?;
    if d.w != w || d.h != h || d.frames.len() != 3 || d.loops != 0 {
        return Err("GIF: wrong size, frame count or loop".into());
    }
    if d.frames.iter().map(|f| f.0).collect::<Vec<_>>() != [7, 8, 9] {
        return Err("GIF: wrong delays".into());
    }
    if compare(&d.frames[0].1, &img, w, 0, 0, w, h).1 != 0 {
        return Err("GIF: an image of 251 colours should come back exact".into());
    }
    let (mean, maxd) = compare(&d.frames[1].1, &smooth, w, 0, 0, w, h);
    print!("    GIF codec: 251 colours exact; {w} x {h} gradient within {mean:.2} mean, {maxd} max");
    if mean > 4.0 || maxd > 24 {
        return Err("GIF: the median cut palette is too far from the gradient".into());
    }
    let (mean, _) = compare(&d.frames[2].1, &smooth, w, 0, 0, w, h);
    println!(" (dithered {mean:.2} mean)");
    if mean > 8.0 {
        return Err("GIF: dithered frame too far from the image".into());
    }
    Ok(())
}

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct AnimTest {
    dir: std::path::PathBuf,
    gif: String,
    mp4: String,
    o: AnimOptions,
    dates: Vec<usize>,
    orig_t: usize,
}

impl State {
    pub(super) fn test_animation(
        &mut self,
        st: &mut UiTest,
        input: &dyn Fn(usize) -> Option<String>,
        t0: std::time::Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
        };
        let a = self.opts.inputs.first().cloned().unwrap_or_default();
        let _ = input;
        let ppp = ctx.pixels_per_point();
        let mut at = std::mem::take(&mut st.anim);
        let r = (|| -> Option<i32> {
            match st.stage {
                130 => {
                    at.dir = std::env::temp_dir().join("janus_selftest_anim");
                    let _ = std::fs::remove_dir_all(&at.dir);
                    let _ = std::fs::create_dir_all(&at.dir);
                    if let Err(e) = gif_codec(&at.dir) {
                        return fail(&e);
                    }
                    self.open_inputs(vec![a.clone()], false, ctx);
                    st.frames = 0;
                    next(st, "GIF writer and reader; open A again");
                }
                131 => {
                    // A alone, loaded, on the map as it is laid out
                    let ready = self.opening.is_none()
                        && self.layers.len() == 1
                        && self.layers[0].inputs == [a.clone()]
                        && self.session.as_ref().is_some_and(|s| s.s.overview.complete() && s.stats.is_some())
                        && self.map.canvas[0] >= 100.0;
                    if !ready {
                        return None;
                    }
                    if st.frames == 0 {
                        self.display.mode = Mode::Value;
                        let r = &mut self.display.ranges[Mode::Value as usize];
                        (r.lo, r.hi, r.manual) = (1.0, 5.0, true);
                        self.map.fit();
                        self.set_date(3);
                        self.map.dirty = true;
                        st.frames = 1;
                        return None;
                    }
                    // the view fitted and drawn first: the frames and their comparison share it
                    if self.map.fit_pending || st.frames < 4 {
                        st.frames += 1;
                        return None;
                    }
                    at.orig_t = self.display.t;
                    at.o = AnimOptions { from: 2, to: 14, step: 3, fps: 3.0, ..Default::default() }; // 2, 5, 8, 11, 14
                    at.dates = self.anim_dates(&at.o);
                    at.gif = at.dir.join("a.gif").to_string_lossy().into_owned();
                    let m = self.ground_metres_per_pixel(150.0, 100.0);
                    println!("    ground: {m:.3} m per pixel of A (30 m in UTM 23S, near its central meridian)");
                    if (m - 30.0).abs() > 0.3 {
                        return fail("the scale bar's ground distance is wrong");
                    }
                    if self.start_animation(at.gif.clone(), at.o, ctx).is_none() || at.dates.len() != 5 {
                        return fail("the animation did not start");
                    }
                    next(st, "A animated: 5 dates as a GIF");
                }
                132 => {
                    if let Some(r) = &self.anim.run {
                        if self.display.t == at.orig_t && r.next > 0 {
                            return fail("the date should follow the frames");
                        }
                        return None;
                    }
                    let job = self.exports.last()?.clone();
                    match job.state() {
                        ExportState::Running => return None,
                        ExportState::Done => {}
                        _ => {
                            println!("    {}", job.error());
                            return fail("the GIF export failed");
                        }
                    }
                    if self.display.t != at.orig_t {
                        return fail("the date should be restored after the export");
                    }
                    let d = match crate::gif::decode(&at.gif) {
                        Ok(d) => d,
                        Err(e) => {
                            println!("    {e}");
                            return fail("the exported GIF cannot be decoded");
                        }
                    };
                    let (q, e) = self.anim.stats.per_frame();
                    let kb = std::fs::metadata(&at.gif).map_or(0.0, |m| m.len() as f64 / 1024.0);
                    println!(
                        "    {}: {} x {}, {} frames, {kb:.0} KB; {q:.1} ms to quantize and {e:.1} ms to compress a frame",
                        at.gif,
                        d.w,
                        d.h,
                        d.frames.len()
                    );
                    if d.frames.len() != at.dates.len() {
                        return fail("the GIF should have one frame per date");
                    }
                    if d.loops != 0 {
                        return fail("the GIF should loop forever");
                    }
                    if d.frames.iter().any(|f| f.0 != 33) {
                        return fail("a frame at 3 frames/s lasts 33 hundredths");
                    }
                    // each frame against the map rendered again at its date: the map in the middle (overlays are at
                    // the edges), the stamp and the scale bar at the corners only in the GIF
                    let (w, h) = (d.w, d.h);
                    let mut plain = Vec::new();
                    for &t in &at.dates {
                        self.display.t = t;
                        let Some((img, pw, ph)) = self.render_export(1, false, None, ppp) else {
                            return fail("the map cannot be rendered");
                        };
                        if pw != w || ph != h {
                            return fail("the GIF's size differs from the map's");
                        }
                        plain.push(img);
                    }
                    self.display.t = at.orig_t;
                    self.map.dirty = true;
                    let mid = (w * 3 / 10, h * 3 / 10, w * 7 / 10, h * 7 / 10);
                    for (k, f) in d.frames.iter().enumerate() {
                        let (mean, maxd) = compare(&f.1, &plain[k], w, mid.0, mid.1, mid.2, mid.3);
                        let other = (0..plain.len())
                            .filter(|&q| q != k)
                            .map(|q| compare(&f.1, &plain[q], w, mid.0, mid.1, mid.2, mid.3).0)
                            .fold(f64::INFINITY, f64::min);
                        let stamp = whites(&f.1, w, 0, 0, w / 3, h / 6) - whites(&plain[k], w, 0, 0, w / 3, h / 6);
                        let bar = whites(&f.1, w, w * 3 / 4, h * 5 / 6, w, h)
                            - whites(&plain[k], w, w * 3 / 4, h * 5 / 6, w, h);
                        let label = &self.session.as_ref()?.info.layers[at.dates[k]].label;
                        println!(
                            "    frame {} ({label}): |GIF - map| {mean:.2} mean, {maxd} max (other dates >= {other:.2}); stamp {stamp}, scale bar {bar} white px",
                            k + 1
                        );
                        if mean > 1.5 || maxd > 24 {
                            return fail("a GIF frame differs from the map at its date");
                        }
                        if other <= mean {
                            return fail("a GIF frame matches another date better than its own");
                        }
                        if stamp < 50 {
                            return fail("the date stamp is missing");
                        }
                        if bar < 30 {
                            return fail("the scale bar is missing");
                        }
                    }
                    next(st, "the GIF: one frame per date, delays, loop, pixels, stamp and scale bar");
                    if self.find_ffmpeg().is_empty() {
                        println!("    ffmpeg not found: MP4 not tested");
                        st.stage = 135;
                        return None;
                    }
                    at.o.format = 1;
                    at.o.fps = 2.0;
                    at.o.scale = 1;
                    at.mp4 = at.dir.join("a.mp4").to_string_lossy().into_owned();
                    if self.start_animation(at.mp4.clone(), at.o, ctx).is_none() {
                        return fail("the MP4 export did not start");
                    }
                }
                133 => {
                    let job = self.exports.last()?.clone();
                    if self.anim.run.is_some() || job.state() == ExportState::Running {
                        return None;
                    }
                    if job.state() != ExportState::Done {
                        println!("    {}", job.error());
                        return fail("the MP4 export failed");
                    }
                    let bytes = std::fs::metadata(&at.mp4).map_or(0, |m| m.len());
                    let ffmpeg = self.find_ffmpeg();
                    println!("    {}: {:.0} KB (ffmpeg {ffmpeg})", at.mp4, bytes as f64 / 1024.0);
                    if bytes < 1000 {
                        return fail("the MP4 is empty");
                    }
                    // ffprobe next to ffmpeg, if any: the number of frames
                    let probe = std::path::Path::new(&ffmpeg).with_file_name(if cfg!(windows) {
                        "ffprobe.exe"
                    } else {
                        "ffprobe"
                    });
                    if probe.exists() {
                        let out = std::process::Command::new(&probe)
                            .args([
                                "-v",
                                "error",
                                "-count_frames",
                                "-select_streams",
                                "v:0",
                                "-show_entries",
                                "stream=nb_read_frames,width,height",
                                "-of",
                                "csv=p=0",
                            ])
                            .arg(&at.mp4)
                            .output();
                        if let Ok(o) = out {
                            let line =
                                String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string();
                            println!("    ffprobe: width,height,frames = {line}");
                            let v: Vec<i64> = line.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                            if v.len() != 3 || v[2] != at.dates.len() as i64 {
                                return fail("the MP4 should have one frame per date");
                            }
                            let cw = (self.map.canvas[0].floor() * ppp).round() as i64;
                            if v[0] % 2 != 0 || v[1] % 2 != 0 || (v[0] - cw).abs() > 1 {
                                return fail("the MP4's size should be the map's, even");
                            }
                        }
                    }
                    next(st, "the MP4 through ffmpeg");
                    st.stage = 135;
                }
                135 => {
                    // a cancelled export leaves nothing behind and restores the date
                    at.o.format = 0;
                    at.o.from = 0;
                    at.o.to = -1;
                    at.o.step = 1;
                    at.gif = at.dir.join("cancelled.gif").to_string_lossy().into_owned();
                    if self.start_animation(at.gif.clone(), at.o, ctx).is_none() {
                        return fail("the export did not start");
                    }
                    next(st, "an export started, to be cancelled");
                }
                136 => {
                    if self.anim.run.as_ref().is_some_and(|r| r.next < 3) {
                        return None;
                    }
                    if let Some(j) = self.exports.last() {
                        j.cancel.store(true, Ordering::Relaxed);
                    }
                    next(st, "cancelled after 3 frames");
                }
                137 => {
                    let job = self.exports.last()?.clone();
                    if self.anim.run.is_some() || job.state() == ExportState::Running {
                        return None;
                    }
                    if job.state() != ExportState::Cancelled {
                        return fail("the export should be cancelled");
                    }
                    if std::path::Path::new(&at.gif).exists()
                        || std::path::Path::new(&format!("{}.part", at.gif)).exists()
                    {
                        return fail("a cancelled export must leave no file");
                    }
                    if self.display.t != at.orig_t {
                        return fail("the date should be restored after a cancel");
                    }
                    self.exports.clear();
                    if std::env::var_os("JANUS_KEEP_ANIM").is_none() {
                        let _ = std::fs::remove_dir_all(&at.dir);
                    }
                    next(st, "cancel: no file left, the date restored");
                    if self.opts.extra_layers.len() < 3 {
                        println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                        return Some(0);
                    }
                    st.stage = 170; // validation, on D
                }
                _ => {}
            }
            None
        })();
        st.anim = at;
        r
    }
}
