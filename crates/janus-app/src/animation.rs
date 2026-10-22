//! Animation export (src/app_animation.cpp): the map over a range of dates (or bands of a spectrum) as a GIF, MP4
//! or WebM. Frames are rendered on the main thread like the PNG export (offscreen, with the date stamp, legend and
//! scale bar drawn on the CPU), one per frame of the interface once the date, its detail tiles and the basemap are
//! on the GPU; the view stays frozen behind a modal progress dialog. A background thread encodes them from a short
//! queue (a few frames in memory, whatever the length): our own GIF writer (gif.rs), or ffmpeg fed raw RGBA on its
//! standard input (no library linked; found in Settings, next to Janus or on PATH).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use egui::{Ui, vec2};
use janus_gpu::Mode;

use crate::app::State;
use crate::export_map::FigureOverlay;
use crate::exports::{ExportJob, start_export_job};
use crate::theme;

/// The longest wait for a frame's detail tiles and basemap tiles (s).
const TILE_WAIT_S: f64 = 20.0;

#[derive(Clone, Copy)]
pub struct AnimOptions {
    /// dates (to < 0: the last)
    pub from: i32,
    pub to: i32,
    /// every n-th date
    pub step: i32,
    pub fps: f32,
    /// x the on-screen resolution
    pub scale: i32,
    /// 0 GIF, 1 MP4 (H.264), 2 WebM (VP9)
    pub format: usize,
    /// 0 the map's (dark), 1 white
    pub background: usize,
    pub stamp: bool,
    pub legend: bool,
    pub scale_bar: bool,
    pub marks: bool,
    /// GIF: error diffusion (smoother gradients, larger files)
    pub dither: bool,
}

impl Default for AnimOptions {
    fn default() -> Self {
        AnimOptions {
            from: 0,
            to: -1,
            step: 1,
            fps: 4.0,
            scale: 1,
            format: 0,
            background: 0,
            stamp: true,
            legend: true,
            scale_bar: true,
            marks: false,
            dither: false,
        }
    }
}

pub fn anim_ext(format: usize) -> &'static str {
    match format {
        1 => "mp4",
        2 => "webm",
        _ => "gif",
    }
}

fn time_dependent(m: Mode) -> bool {
    matches!(m, Mode::Value | Mode::Anomaly | Mode::Rgb | Mode::Diff | Mode::Composite)
}

#[derive(Default)]
struct Queue {
    frames: VecDeque<Vec<u8>>,
    closed: bool,
    /// set by the main thread: the export fails with it
    abort: String,
}

/// Frames from the main thread to the encoder: at most MAX wait in memory.
#[derive(Default)]
struct FrameQueue {
    m: Mutex<Queue>,
    cv: Condvar,
}

impl FrameQueue {
    const MAX: usize = 3;

    fn full(&self) -> bool {
        self.m.lock().unwrap().frames.len() >= Self::MAX
    }

    fn push(&self, f: Vec<u8>) {
        self.m.lock().unwrap().frames.push_back(f);
        self.cv.notify_all();
    }

    fn close(&self, why: &str) {
        let mut q = self.m.lock().unwrap();
        q.closed = true;
        if !why.is_empty() {
            q.abort = why.to_string();
        }
        drop(q);
        self.cv.notify_all();
    }

    /// The next frame; None at the end (closed and empty), on cancel; Err on abort.
    fn pop(&self, j: &ExportJob) -> Result<Option<Vec<u8>>, String> {
        let mut q = self.m.lock().unwrap();
        while q.frames.is_empty() && !q.closed && !j.cancelled() {
            q = self.cv.wait_timeout(q, Duration::from_millis(50)).unwrap().0;
        }
        if !q.abort.is_empty() {
            return Err(q.abort.clone());
        }
        if j.cancelled() {
            return Ok(None);
        }
        Ok(q.frames.pop_front())
    }
}

/// Measured by the encoder, for the self-test's report.
#[derive(Default)]
pub(crate) struct AnimStats {
    pub frames: AtomicUsize,
    quantize: AtomicU64,
    encode: AtomicU64,
}

impl AnimStats {
    fn add(a: &AtomicU64, ms: f64) {
        let _ = a.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some((f64::from_bits(v) + ms).to_bits()));
    }

    /// The mean quantize and compress times of a GIF frame (ms).
    pub fn per_frame(&self) -> (f64, f64) {
        let n = self.frames.load(Ordering::Relaxed).max(1) as f64;
        (
            f64::from_bits(self.quantize.load(Ordering::Relaxed)) / n,
            f64::from_bits(self.encode.load(Ordering::Relaxed)) / n,
        )
    }
}

/// The export being rendered.
pub(crate) struct AnimRun {
    o: AnimOptions,
    pub dates: Vec<usize>,
    /// the frame being prepared
    pub next: usize,
    orig_t: usize,
    cube: u64,
    /// every frame's size
    w: usize,
    h: usize,
    queue: Arc<FrameQueue>,
    pub job: Arc<ExportJob>,
    wait_since: Instant,
    /// wait_since is set for `next`
    waiting: bool,
    /// what the frame waits for
    status: String,
    /// frames drawn without all their detail tiles (timeout)
    pub late_tiles: usize,
}

#[derive(Default)]
pub struct AnimationUi {
    pub o: AnimOptions,
    pub(crate) run: Option<AnimRun>,
    pub(crate) stats: Arc<AnimStats>,
    /// the ffmpeg found for a setting, and when (asked of the file system at most every 2 s)
    ffmpeg: Mutex<Option<(String, String, Instant)>>,
}

/// The last lines of a log (ffmpeg's errors).
fn log_tail(path: &std::path::Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default().replace('\r', "");
    let all: Vec<&str> = text.trim_end_matches('\n').lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// `name` in a folder of PATH, if any.
fn find_on_path(name: &str) -> Option<String> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

impl State {
    /// The ffmpeg that would be used ("" if none): Settings', else next to Janus, else on PATH.
    pub fn find_ffmpeg(&self) -> String {
        let set = self.settings.s.ffmpeg.clone();
        let mut cache = self.anim.ffmpeg.lock().unwrap();
        if let Some((s, found, at)) = cache.as_ref()
            && *s == set
            && at.elapsed().as_secs_f64() < 2.0
        {
            return found.clone();
        }
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        let found = if !set.is_empty() {
            if std::path::Path::new(&set).is_file() { set.clone() } else { String::new() }
        } else if let Some(p) =
            std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(exe))).filter(|p| p.is_file())
        {
            p.to_string_lossy().into_owned()
        } else {
            find_on_path("ffmpeg").unwrap_or_default()
        };
        *cache = Some((set, found.clone(), Instant::now()));
        found
    }

    /// The dates of an animation: from..to every step (backwards when to < from).
    pub fn anim_dates(&self, o: &AnimOptions) -> Vec<usize> {
        let Some(s) = &self.session else { return Vec::new() };
        let n = s.info.t() as i32;
        let from = o.from.clamp(0, n - 1);
        let to = (if o.to < 0 { n - 1 } else { o.to }).clamp(0, n - 1);
        let step = o.step.max(1);
        let mut d = Vec::new();
        let mut t = from;
        if from <= to {
            while t <= to {
                d.push(t as usize);
                t += step;
            }
        } else {
            while t >= to {
                d.push(t as usize);
                t -= step;
            }
        }
        d
    }

    /// The export dialog's options of an animation; false if it cannot be saved.
    pub(crate) fn animation_options_ui(&mut self, ui: &mut Ui) -> bool {
        let Some(s) = &self.session else { return false };
        let info = s.info.clone();
        let n = info.t() as i32;
        let unit = if info.spectral { "band" } else { "date" };
        let name = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let mut can = true;
        let ffmpeg = self.find_ffmpeg();
        let o = &mut self.anim.o;
        o.from = o.from.clamp(0, n - 1);
        if o.to < 0 || o.to >= n {
            o.to = n - 1;
        }
        ui.colored_label(theme::accent(), name);
        let w = ui.text_style_height(&egui::TextStyle::Body) * 20.0;
        ui.spacing_mut().slider_width = w;
        let label = |t: i32| info.layers[t.clamp(0, n - 1) as usize].label.clone();
        ui.add(egui::Slider::new(&mut o.from, 0..=n - 1).custom_formatter(|v, _| label(v as i32)).text("From"));
        ui.add(egui::Slider::new(&mut o.to, 0..=n - 1).custom_formatter(|v, _| label(v as i32)).text("To"))
            .on_hover_text("A range that ends before it starts plays backwards.");
        ui.add(
            egui::Slider::new(&mut o.step, 1..=(n - 1).max(1))
                .custom_formatter(|v, _| format!("every {v} {unit}(s)"))
                .text("Step"),
        );
        ui.add(
            egui::Slider::new(&mut o.fps, 0.5..=30.0)
                .logarithmic(true)
                .custom_formatter(|v, _| format!("{v:.1} frames/s"))
                .text("Frame rate"),
        );
        let o = self.anim.o;
        let frames = self.anim_dates(&o).len();
        ui.weak(format!(
            "{frames} frame{}, {:.1} s",
            if frames == 1 { "" } else { "s" },
            frames as f32 / o.fps.max(0.1)
        ));
        if o.format == 0 {
            let cs = (100.0 / f64::from(o.fps)).round().max(2.0);
            if (cs / 100.0 - 1.0 / f64::from(o.fps)).abs() > 0.004 {
                ui.weak(format!("GIF: each frame lasts {:.2} s (in hundredths of a second)", cs / 100.0));
            }
        }
        ui.add_space(4.0);
        const FORMATS: [&str; 3] =
            ["GIF (any browser, slides; 256 colours a frame)", "MP4 (H.264, ffmpeg)", "WebM (VP9, ffmpeg)"];
        let old = self.anim.o.format;
        let o = &mut self.anim.o;
        egui::ComboBox::from_label("Format").width(w).selected_text(FORMATS[o.format.min(2)]).show_ui(ui, |ui| {
            for (f, text) in FORMATS.iter().enumerate() {
                let enabled = f == 0 || !ffmpeg.is_empty();
                ui.add_enabled_ui(enabled, |ui| ui.selectable_value(&mut o.format, f, *text)).response.on_disabled_hover_text(
                    "Needs ffmpeg: install it (e.g. winget install ffmpeg, apt install ffmpeg,\nbrew install ffmpeg) or set its path in File > Settings.",
                );
            }
        });
        if o.format > 0 && ffmpeg.is_empty() {
            o.format = 0;
        }
        if o.format != old {
            // the file name follows the format
            let p = &mut self.export.path;
            if let Some(dot) = p.rfind('.').filter(|&d| p.rfind(['/', '\\']).is_none_or(|s| d > s)) {
                let e = p[dot + 1..].to_ascii_lowercase();
                if e == "gif" || e == "mp4" || e == "webm" {
                    p.truncate(dot);
                }
            }
            *p += &format!(".{}", anim_ext(o.format));
        }
        if o.format > 0 {
            ui.weak(format!("ffmpeg: {ffmpeg}"));
        } else if ffmpeg.is_empty() {
            ui.weak("MP4 and WebM need ffmpeg (not found; see File > Settings).");
        }
        let bgs = ["Map's (dark)", "White"];
        egui::ComboBox::from_label("Background").width(w).selected_text(bgs[o.background.min(1)]).show_ui(ui, |ui| {
            for (k, b) in bgs.iter().enumerate() {
                ui.selectable_value(&mut o.background, k, *b);
            }
        });
        ui.checkbox(&mut o.stamp, "Date stamp")
            .on_hover_text(format!("The {unit} large, top left, with the layer and the display mode under it"));
        ui.checkbox(&mut o.legend, "Legend (colour bar or classes)");
        ui.add_enabled(
            info.geo_transform.is_some() && !info.crs_wkt.is_empty(),
            egui::Checkbox::new(&mut o.scale_bar, "Scale bar"),
        )
        .on_hover_text("Ground distance at the centre of the view (the layer needs a CRS)");
        ui.checkbox(&mut o.marks, "Pins, ROI and vector layers");
        if o.format == 0 {
            ui.checkbox(&mut o.dither, "Dither (GIF)")
                .on_hover_text("Spreads the error of the 256-colour palette: smoother gradients\n(e.g. the basemap's imagery), larger files.");
        }
        let mode = self.display.mode;
        let emb = self.layers.get(self.active).is_some_and(|l| l.emb.as_ref().is_some_and(|e| e.drawn()));
        if !time_dependent(mode) || emb {
            ui.colored_label(
                theme::warning(),
                format!(
                    "The map shows {}, which does not change with the {unit}:\nevery frame will be alike.",
                    crate::display::mode_name_of(mode, &info)
                ),
            );
        } else if mode == Mode::Rgb {
            ui.colored_label(
                theme::warning(),
                format!("RGB composite: its three {unit}s are fixed, the frames change\nonly where other layers follow the {unit}."),
            );
        }
        ui.weak(format!("Drawn as the map shows it, waiting for each {unit}'s detail tiles."));
        if frames < 1 || (self.anim.o.format > 0 && ffmpeg.is_empty()) {
            can = false;
        }
        can
    }

    /// The export started: the encoder's thread waits for the frames the pump renders.
    pub fn start_animation(&mut self, path: String, o: AnimOptions, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        if self.anim.run.is_some() {
            return None;
        }
        let s = self.session.as_ref()?;
        let dates = self.anim_dates(&o);
        if dates.is_empty() {
            return None;
        }
        let ppp = ctx.pixels_per_point();
        let ps = ppp * o.scale as f32;
        let (w, h) = (
            (self.map.canvas[0].floor() * ps).round() as usize, // as render_export sizes its target
            (self.map.canvas[1].floor() * ps).round() as usize,
        );
        let cube = s.info.id;
        self.display.playing = false;
        let queue = Arc::new(FrameQueue::default());
        let names = ["GIF", "MP4", "WebM"];
        let title = format!("Animation as {}, {} frames of {w} x {h} px", names[o.format.min(2)], dates.len());
        let total = dates.len();
        let ffmpeg = if o.format > 0 { self.find_ffmpeg() } else { String::new() };
        let log = crate::settings::app_data_dir().join("ffmpeg.log");
        let threads = self.session_settings().processing_threads().to_string();
        let stats = Arc::new(AnimStats::default());
        self.anim.stats = stats.clone();
        let q = queue.clone();
        let p = path.clone();
        let job = start_export_job(title, path, ctx, move |j| {
            let tmp = format!("{p}.part");
            let mut done = 0;
            let r = (|| -> Result<(), String> {
                if o.format == 0 {
                    let delay = (100.0 / f64::from(o.fps)).round().max(2.0) as u16;
                    let mut gif = crate::gif::GifWriter::open(&tmp, w, h, 0)?;
                    while let Some(frame) = q.pop(j)? {
                        gif.add_frame(&frame, delay, o.dither)?;
                        AnimStats::add(&stats.quantize, gif.last_quantize_ms);
                        AnimStats::add(&stats.encode, gif.last_encode_ms);
                        done += 1;
                        stats.frames.store(done, Ordering::Relaxed);
                        j.set_progress(done as f64 / total as f64);
                    }
                    gif.close()?;
                } else {
                    // even sizes (yuv420p): the last column / row is left out when odd
                    let (vw, vh) = (w & !1, h & !1);
                    let mut args: Vec<String> =
                        ["-hide_banner", "-loglevel", "warning", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s"]
                            .map(String::from)
                            .to_vec();
                    args.push(format!("{vw}x{vh}"));
                    args.extend([
                        "-framerate".to_string(),
                        janus_validation::fmt_g(f64::from(o.fps), 6),
                        "-i".into(),
                        "-".into(),
                    ]);
                    let tail: &[&str] = if o.format == 1 {
                        &[
                            "-c:v",
                            "libx264",
                            "-preset",
                            "medium",
                            "-crf",
                            "18",
                            "-pix_fmt",
                            "yuv420p",
                            "-movflags",
                            "+faststart",
                            "-threads",
                        ]
                    } else {
                        &[
                            "-c:v",
                            "libvpx-vp9",
                            "-crf",
                            "32",
                            "-b:v",
                            "0",
                            "-deadline",
                            "good",
                            "-cpu-used",
                            "4",
                            "-row-mt",
                            "1",
                            "-pix_fmt",
                            "yuv420p",
                            "-threads",
                        ]
                    };
                    args.extend(tail.iter().map(|s| s.to_string()));
                    args.extend([
                        threads.clone(),
                        "-f".into(),
                        if o.format == 1 { "mp4" } else { "webm" }.into(),
                        tmp.clone(),
                    ]);
                    let logf =
                        std::fs::File::create(&log).map_err(|e| format!("cannot write {}: {e}", log.display()))?;
                    let mut cmd = std::process::Command::new(&ffmpeg);
                    cmd.args(&args)
                        .stdin(std::process::Stdio::piped())
                        .stdout(logf.try_clone().map_err(|e| e.to_string())?)
                        .stderr(logf);
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                    }
                    let mut child = cmd.spawn().map_err(|e| format!("cannot start {ffmpeg}: {e}"))?;
                    let mut stdin = child.stdin.take().unwrap();
                    let mut error = String::new();
                    loop {
                        let frame = match q.pop(j) {
                            Ok(Some(f)) => f,
                            Ok(None) => break,
                            Err(e) => {
                                error = e;
                                break;
                            }
                        };
                        let mut ok = true;
                        for y in 0..vh {
                            if stdin.write_all(&frame[y * w * 4..y * w * 4 + vw * 4]).is_err() {
                                ok = false;
                                break;
                            }
                        }
                        if !ok {
                            error = "ffmpeg stopped reading the frames".into();
                            break;
                        }
                        done += 1;
                        stats.frames.store(done, Ordering::Relaxed);
                        j.set_progress(done as f64 / total as f64);
                    }
                    drop(stdin);
                    if j.cancelled() || !error.is_empty() {
                        let _ = child.kill();
                    }
                    let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                    if error.is_empty() && !j.cancelled() && code != 0 {
                        error = format!("ffmpeg failed (exit code {code})");
                    }
                    if !error.is_empty() && !j.cancelled() {
                        let t = log_tail(&log, 6);
                        if !t.is_empty() {
                            error += &format!(":\n{t}");
                        }
                    }
                    if !error.is_empty() {
                        return Err(error);
                    }
                }
                if done < total && !j.cancelled() {
                    return Err(format!("the export ended after {done} of {total} frames"));
                }
                Ok(())
            })();
            let ok = r.is_ok() && !j.cancelled();
            if ok {
                let _ = std::fs::remove_file(&p);
                if let Err(e) = std::fs::rename(&tmp, &p) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("could not rename {tmp}: {e}"));
                }
            } else {
                let _ = std::fs::remove_file(&tmp); // never a half-written file
            }
            r
        });
        self.anim.run = Some(AnimRun {
            o,
            dates,
            next: 0,
            orig_t: self.display.t,
            cube,
            w,
            h,
            queue,
            job: job.clone(),
            wait_since: Instant::now(),
            waiting: false,
            status: String::new(),
            late_tiles: 0,
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }

    /// Once a frame: the next date set, waited for (its data, detail tiles and basemap on the GPU), rendered and
    /// queued; the date restored at the end.
    pub(crate) fn pump_animation(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.anim.run else { return };
        let end = |s: &mut State, why: &str| {
            let r = s.anim.run.take().unwrap();
            r.queue.close(why);
            if s.session.as_ref().is_some_and(|x| x.info.id == r.cube) {
                s.set_date(r.orig_t);
            }
            s.map.dirty = true;
        };
        if r.job.cancelled() || r.job.state() != crate::exports::ExportState::Running {
            return end(self, "");
        }
        if self.session.as_ref().is_none_or(|s| s.info.id != r.cube) {
            return end(self, "the active layer changed during the export");
        }
        if r.next >= r.dates.len() {
            return end(self, "");
        }
        ctx.request_repaint();
        let t = r.dates[r.next];
        if self.display.t != t || !r.waiting {
            // the map asks for this date's tiles in this frame; the frame is drawn in a later one
            self.set_date(t);
            self.map.dirty = true;
            let r = self.anim.run.as_mut().unwrap();
            r.waiting = true;
            r.wait_since = Instant::now();
            r.status = "Loading...".into();
            return;
        }
        if r.queue.full() {
            self.anim.run.as_mut().unwrap().status = "Encoding...".into();
            return;
        }
        // what the frame needs on the GPU: each visible layer's date(s) and statistics...
        let mut waiting_for = None;
        for (i, l) in self.layers.iter().enumerate() {
            if !l.visible || !l.place.aligned || l.emb.as_ref().is_some_and(|e| e.drawn()) {
                continue;
            }
            let act = i == self.active;
            let (s, d) = if act { (self.session.as_ref(), &self.display) } else { (l.session.as_ref(), &l.disp) };
            let Some(s) = s else { continue };
            let mut ready = d.ready(s, &d.layer_params(s));
            let comp = if act { self.composite.as_ref() } else { l.composite.as_ref() };
            if let Some(c) = comp.filter(|c| c.on) {
                ready = c.ready(d.t, s.info.t()); // three bands as RGB: their image of the date
            }
            if !ready {
                waiting_for =
                    Some(format!("Loading {} at {}...", l.name, s.info.layers[d.t.min(s.info.t() - 1)].label));
                break;
            }
        }
        if let Some(w) = waiting_for {
            self.anim.run.as_mut().unwrap().status = w;
            return;
        }
        // ...the active layer's detail tiles at this zoom, and the basemap's (up to TILE_WAIT_S)
        let timed_out = r.wait_since.elapsed().as_secs_f64() > TILE_WAIT_S;
        let next = r.dates.get(r.next + 1).copied();
        let mode = self.display.mode;
        if self.map.detail_level.is_some()
            && matches!(mode, Mode::Value | Mode::Diff)
            && let (Some(s), Some(view)) = (self.session.as_mut(), self.map.last_view)
            && !s.s.defer_random_reads()
        {
            let mut missing = s.tiles.missing(t, &view);
            if mode == Mode::Diff
                && let Some(r0) = self.display.diff_ref_date(t, s.info.t())
            {
                missing += s.tiles.missing(r0, &view);
            }
            if let Some(nx) = next {
                s.tiles.update(s.s.fg(), t, &view, Some(nx)); // read the next date meanwhile
            }
            if missing > 0 && !timed_out {
                self.anim.run.as_mut().unwrap().status = format!("Reading the detail tiles ({missing} left)...");
                return;
            }
            if missing > 0 {
                self.anim.run.as_mut().unwrap().late_tiles += 1;
            }
        }
        if self.basemap.shown()
            && self.basemap.b.as_ref().is_some_and(|b| b.inflight() > 0 && !b.offline())
            && !timed_out
        {
            self.anim.run.as_mut().unwrap().status = "Loading the basemap...".into();
            return;
        }
        let r = self.anim.run.as_ref().unwrap();
        let (o, rw, rh) = (r.o, r.w, r.h);
        let ppp = ctx.pixels_per_point();
        let bg = (o.background == 1).then_some([1.0, 1.0, 1.0, 1.0]);
        let Some((mut rgba, w, h)) = self.render_export(o.scale, false, bg, ppp) else {
            return end(self, "the map could not be rendered");
        };
        if w != rw || h != rh {
            return end(self, "the map was resized during the export");
        }
        let fo = FigureOverlay {
            label: if o.stamp { 2 } else { 0 },
            legend: o.legend,
            marks: o.marks,
            scale_bar: o.scale_bar,
        };
        self.draw_figure_overlay(&mut rgba, w, h, o.scale, fo, ppp);
        let r = self.anim.run.as_mut().unwrap();
        r.queue.push(rgba);
        r.next += 1;
        r.waiting = false;
        if r.next >= r.dates.len() {
            end(self, "");
        }
    }

    /// The modal progress of an animation being rendered.
    pub(crate) fn animation_dialog(&mut self, ctx: &egui::Context) {
        let Some(r) = &self.anim.run else { return };
        let Some(s) = &self.session else { return };
        let k = r.next.min(r.dates.len() - 1);
        let text = format!("Frame {} of {}: {}", k + 1, r.dates.len(), s.info.layers[r.dates[k]].label);
        let status = r.status.clone();
        let encoded = self.anim.stats.frames.load(Ordering::Relaxed);
        let (n, next) = (r.dates.len(), r.next);
        let job = r.job.clone();
        egui::Modal::new(egui::Id::new("animation run")).show(ctx, |ui| {
            ui.heading("Exporting animation");
            ui.label(text);
            ui.weak(status);
            let w = ui.text_style_height(&egui::TextStyle::Body) * 24.0;
            ui.add(
                egui::ProgressBar::new(next as f32 / n as f32)
                    .desired_width(w)
                    .text(format!("{encoded} / {n} encoded")),
            );
            ui.weak("The map stays as it is until the last frame is drawn.");
            if ui.add(egui::Button::new("Cancel").min_size(vec2(120.0, 0.0))).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                job.cancel.store(true, Ordering::Relaxed);
            }
        });
    }

    /// Settings: the ffmpeg used for MP4 and WebM.
    pub(crate) fn ffmpeg_settings_ui(&mut self, ui: &mut Ui) {
        crate::panels::section(ui, "Video export (ffmpeg)");
        ui.horizontal(|ui| {
            // the button's own width left beside the field
            let font = egui::TextStyle::Button.resolve(ui.style());
            let button = ui.painter().layout_no_wrap("Browse...".into(), font, egui::Color32::WHITE).size().x
                + 2.0 * ui.spacing().button_padding.x
                + ui.spacing().item_spacing.x;
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.settings.s.ffmpeg)
                    .hint_text("next to Janus, else on PATH")
                    .desired_width((ui.available_width() - button).max(60.0)),
            );
            r.on_hover_text(
                "The ffmpeg executable that writes MP4 and WebM animations (File > Export\nanimation). Empty: ffmpeg next to Janus, else the one on PATH.",
            );
            if ui.button("Browse...").clicked()
                && let Some(f) = rfd::FileDialog::new().pick_file()
            {
                self.settings.s.ffmpeg = f.to_string_lossy().into_owned();
            }
        });
        let t = self.settings.s.ffmpeg.trim_matches([' ', '\t', '"']).to_string();
        if t != self.settings.s.ffmpeg {
            self.settings.s.ffmpeg = t;
        }
        let found = self.find_ffmpeg();
        if !found.is_empty() {
            ui.weak(format!("Using {found}"));
        } else if !self.settings.s.ffmpeg.is_empty() {
            ui.colored_label(theme::warning(), "No such file: MP4 and WebM are off.");
        } else {
            ui.weak("Not found: MP4 and WebM are off (GIF needs nothing).");
        }
    }
}
