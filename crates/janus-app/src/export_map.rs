//! Export (src/app_export.cpp): the map as a PNG figure, the active layer's values and the rendered view as
//! georeferenced GeoTIFFs (for QGIS), the embeddings' view, Zeit results and a run's every output. The GPU renders
//! on the main thread (offscreen, at the chosen resolution); reading the data and writing the files run in the
//! background, with progress and cancel in the Exports window. Files are written with GDAL under a temporary name
//! (.part) and renamed when complete: a cancelled or failed export leaves nothing behind.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::Arc;

use egui::{Pos2, Rect, Ui, vec2};
use gdal::Metadata;
use janus_cube::CubeReader;
use janus_gpu::Mode;

use crate::app::State;
use crate::canvas::{Canvas, FIGURE_FONT_SIZE, Face, Legend, Rgba, draw_credit, draw_legends, draw_scale_bar, rgba};
use crate::exports::{ExportJob, sanitize, start_export_job};
use crate::theme;

/// The GPU target of the offscreen renders.
pub(crate) const EXPORT_SLOT: i32 = -1;
/// Pixels per side of a rendered export, and in all (512 MB of RGBA).
const MAX_EXPORT_SIDE: f64 = 16384.0;
const MAX_EXPORT_PIXELS: f64 = 128e6;

/// The PNG's choices.
#[derive(Clone, Copy)]
pub struct PngOptions {
    /// x the on-screen resolution (rendered at it, not upscaled)
    pub scale: i32,
    /// 0 the map's (dark), 1 white, 2 transparent
    pub background: usize,
    /// date and mode, top left
    pub label: bool,
    /// colour bar or classes, bottom left
    pub legend: bool,
    /// pins, ROI, vector layers
    pub marks: bool,
    /// ground distance, bottom right
    pub scale_bar: bool,
}

impl Default for PngOptions {
    fn default() -> Self {
        PngOptions { scale: 1, background: 0, label: true, legend: true, marks: false, scale_bar: false }
    }
}

/// What is drawn over a rendered map (PNG, animation frames).
#[derive(Clone, Copy, Default)]
pub struct FigureOverlay {
    /// 0 none, 1 date and mode (one line), 2 the date stamp of an animation (large)
    pub label: u8,
    pub legend: bool,
    pub marks: bool,
    pub scale_bar: bool,
}

/// The export dialog and the "Save all outputs" one.
pub struct ExportUi {
    /// 0 none, 1 PNG, 2 values, 3 rendered view, 4 embeddings, 5 animation
    pub kind: usize,
    /// the dialog asked for this frame (its file name set then)
    pub open: bool,
    pub shown: bool,
    /// the file, as typed
    pub path: String,
    pub png: PngOptions,
    pub values_whole: bool,
    pub view_scale: i32,
    /// "Save all outputs": the result whose run, the folder as typed
    pub save_run: Option<(String, String)>,
}

impl Default for ExportUi {
    fn default() -> Self {
        ExportUi {
            kind: 0,
            open: false,
            shown: false,
            path: String::new(),
            png: PngOptions::default(),
            values_whole: false,
            view_scale: 1,
            save_run: None,
        }
    }
}

/// GDAL's progress callback: the job's progress, false once it is cancelled.
extern "C" fn gdal_progress(done: f64, _: *const c_char, arg: *mut c_void) -> c_int {
    // SAFETY: `arg` is the job passed with the callback, alive for the call.
    let j = unsafe { &*(arg as *const ExportJob) };
    j.set_progress(done);
    c_int::from(!j.cancelled())
}

fn last_gdal_error(what: &str) -> String {
    // SAFETY: GDAL's own message buffer.
    let m = unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }.to_string_lossy().into_owned();
    if m.is_empty() { what.to_string() } else { format!("{what}: {m}") }
}

/// The file written as <path>.part renamed once complete (removed on failure).
fn finish_file(tmp: &str, path: &str, ok: bool) -> Result<(), String> {
    if !ok {
        let _ = std::fs::remove_file(tmp);
        return Err(String::new());
    }
    let _ = std::fs::remove_file(path);
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(format!("could not rename {tmp}: {e}"));
    }
    Ok(())
}

/// The CRS (WKT) of a raster file, empty without one.
fn file_wkt(path: &str) -> String {
    gdal::Dataset::open(path).ok().map(|d| d.projection()).unwrap_or_default()
}

/// A NULL-terminated KEY=VALUE list for GDAL.
struct Options {
    _s: Vec<CString>,
    p: Vec<*mut c_char>,
}

impl Options {
    fn new(kv: &[(&str, &str)]) -> Options {
        let s: Vec<CString> = kv.iter().map(|(k, v)| CString::new(format!("{k}={v}")).unwrap()).collect();
        let mut p: Vec<*mut c_char> = s.iter().map(|c| c.as_ptr() as *mut c_char).collect();
        p.push(std::ptr::null_mut());
        Options { _s: s, p }
    }
}

/// An RGB(A) image (RGBA8 rows top-down) written through a MEM dataset and CreateCopy: PNG, or GTiff with a
/// geotransform and a CRS.
#[allow(clippy::too_many_arguments)]
fn write_image(
    path: &str,
    driver: &str,
    rgba: &[u8],
    w: usize,
    h: usize,
    alpha: bool,
    gt: Option<[f64; 6]>,
    wkt: &str,
    options: &[(&str, &str)],
    job: &ExportJob,
    copyright: &str,
) -> Result<(), String> {
    let bands = if alpha { 4 } else { 3 };
    let mem = gdal::DriverManager::get_driver_by_name("MEM").map_err(|_| "GDAL has no MEM driver".to_string())?;
    let drv = gdal::DriverManager::get_driver_by_name(driver).map_err(|_| format!("GDAL has no {driver} driver"))?;
    let mut src = mem
        .create_with_band_type::<u8, _>("", w, h, bands)
        .map_err(|_| last_gdal_error("could not allocate the image"))?;
    let mut map = [1, 2, 3, 4];
    // SAFETY: the buffer holds w x h pixels of 4 bytes; GDAL reads `bands` of them, pixel-interleaved.
    let err = unsafe {
        gdal_sys::GDALDatasetRasterIO(
            src.c_dataset(),
            gdal_sys::GDALRWFlag::GF_Write,
            0,
            0,
            w as c_int,
            h as c_int,
            rgba.as_ptr() as *mut c_void,
            w as c_int,
            h as c_int,
            gdal_sys::GDALDataType::GDT_Byte,
            bands as c_int,
            map.as_mut_ptr(),
            4,
            (w * 4) as c_int,
            1,
        )
    };
    let mut ok = err == gdal_sys::CPLErr::CE_None;
    use gdal::raster::ColorInterpretation::*;
    for (b, ci) in [RedBand, GreenBand, BlueBand, AlphaBand].into_iter().take(bands).enumerate() {
        if let Ok(mut rb) = src.rasterband(b + 1) {
            let _ = rb.set_color_interpretation(ci);
        }
    }
    if let Some(gt) = gt {
        let _ = src.set_geo_transform(&gt);
    }
    if !wkt.is_empty() {
        let _ = src.set_projection(wkt);
    }
    if !copyright.is_empty() {
        let _ = src.set_metadata_item("TIFFTAG_COPYRIGHT", copyright, "");
    }
    let tmp = format!("{path}.part");
    let mut error = String::new();
    if ok {
        let opts = Options::new(options);
        let ctmp = CString::new(tmp.as_str()).unwrap();
        // SAFETY: live driver and dataset handles; the callback's argument is the job, alive for the call.
        unsafe {
            gdal_sys::CPLErrorReset();
            let out = gdal_sys::GDALCreateCopy(
                drv.c_driver(),
                ctmp.as_ptr(),
                src.c_dataset(),
                0,
                opts.p.as_ptr() as *mut *mut c_char,
                Some(gdal_progress),
                job as *const ExportJob as *mut c_void,
            );
            ok = !out.is_null();
            if !out.is_null() {
                gdal_sys::GDALClose(out);
            }
            ok = ok && gdal_sys::CPLGetLastErrorType() < gdal_sys::CPLErr::CE_Failure && !job.cancelled();
        }
        if !ok {
            error = last_gdal_error("could not write the file");
        }
    }
    drop(src);
    finish_file(&tmp, path, ok).map_err(|e| if e.is_empty() { error } else { e })
}

/// What `label` 1 or 2 says: the title, or the date and the line under it.
struct Titles {
    title: String,
    date: String,
    sub: String,
    dated: bool,
}

impl State {
    /// The map's canvas in whole points, and the export's size at `scale`.
    fn export_size(&self, scale: i32, ppp: f32) -> ([f32; 2], [u32; 2]) {
        let cw = self.map.canvas[0].floor().max(1.0);
        let ch = self.map.canvas[1].floor().max(1.0);
        let ps = ppp * scale as f32;
        ([cw, ch], [(cw * ps).round() as u32, (ch * ps).round() as u32])
    }

    pub fn export_scale_fits(&self, scale: i32, ppp: f32) -> bool {
        let (_, [w, h]) = self.export_size(scale, ppp);
        let side = self
            .gpu
            .as_ref()
            .map_or(0.0, |g| f64::from(g.device().limits().max_texture_dimension_2d))
            .min(MAX_EXPORT_SIDE);
        f64::from(w) <= side && f64::from(h) <= side && f64::from(w) * f64::from(h) <= MAX_EXPORT_PIXELS
    }

    /// The map rendered offscreen at `scale` x the screen's pixels: over `bg` (None: the map's), or transparent
    /// (drawn over black and over white: a pixel covered with opacity a reads c a over black and c a + 1 - a over
    /// white, which gives a and c back with the renderer as it is).
    pub(crate) fn render_export(
        &mut self,
        scale: i32,
        transparent: bool,
        bg: Option<[f32; 4]>,
        ppp: f32,
    ) -> Option<(Vec<u8>, usize, usize)> {
        let (canvas, size) = self.export_size(scale, ppp);
        let ps = ppp * scale as f32;
        let draw = |s: &mut State, bg: Option<[f32; 4]>| -> Option<Vec<u8>> {
            s.map.export_bg = bg;
            s.render_map(EXPORT_SLOT, size, canvas, ps);
            s.map.export_bg = None;
            s.gpu.as_ref()?.read_target(EXPORT_SLOT).map(|t| t.2)
        };
        let out = if !transparent {
            draw(self, bg)
        } else {
            let black = draw(self, Some([0.0, 0.0, 0.0, 1.0]));
            let white = draw(self, Some([1.0, 1.0, 1.0, 1.0]));
            black.zip(white).map(|(mut p, q)| {
                use rayon::prelude::*;
                p.par_chunks_mut(4).zip(q.par_chunks(4)).for_each(|(p, q)| {
                    let d = (i32::from(q[0]) - i32::from(p[0]))
                        .max(i32::from(q[1]) - i32::from(p[1]))
                        .max(i32::from(q[2]) - i32::from(p[2]))
                        .max(0);
                    if d == 0 {
                        p[3] = 255; // opaque (most pixels): as drawn
                        return;
                    }
                    let a = 255 - d;
                    for c in &mut p[..3] {
                        *c = if a > 0 { ((i32::from(*c) * 255 + a / 2) / a).min(255) as u8 } else { 0 };
                    }
                    p[3] = a as u8;
                });
                p
            })
        };
        if let Some(g) = self.gpu.as_mut() {
            g.release_target(EXPORT_SLOT);
        }
        out.map(|p| (p, size[0] as usize, size[1] as usize))
    }

    /// Metres on the ground per pixel of the active layer at its pixel (x, y), along a row (haversine on the mean
    /// Earth radius, whatever the CRS); 0 when it cannot be known.
    pub fn ground_metres_per_pixel(&self, x: f64, y: f64) -> f64 {
        let Some(s) = &self.session else { return 0.0 };
        let info = &s.info;
        if info.geo_transform.is_none() || info.crs_wkt.is_empty() {
            return 0.0;
        }
        use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
        let (Ok(mut src), Ok(mut ll)) = (SpatialRef::from_wkt(&info.crs_wkt), SpatialRef::from_epsg(4326)) else {
            return 0.0;
        };
        src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        ll.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let Ok(ct) = CoordTransform::new(&src, &ll) else { return 0.0 };
        // two points 10 pixels apart along the row, as longitude and latitude
        let (Some(a), Some(b)) = (info.pixel_to_geo(x - 5.0, y), info.pixel_to_geo(x + 5.0, y)) else { return 0.0 };
        let (mut gx, mut gy) = ([a.0, b.0], [a.1, b.1]);
        if ct.transform_coords(&mut gx, &mut gy, &mut []).is_err() {
            return 0.0;
        }
        const R: f64 = 6371008.8;
        let rad = std::f64::consts::PI / 180.0;
        let (dlat, dlon) = ((gy[1] - gy[0]) * rad, (gx[1] - gx[0]) * rad);
        let h = (dlat / 2.0).sin().powi(2) + (gy[0] * rad).cos() * (gy[1] * rad).cos() * (dlon / 2.0).sin().powi(2);
        2.0 * R * h.sqrt().min(1.0).asin() / 10.0
    }

    /// "<layer>_<date><suffix>".
    pub fn export_name(&self, suffix: &str) -> String {
        let mut name = self.layers.get(self.active).map_or("janus".to_string(), |l| sanitize(&l.name));
        if let Some(s) = &self.session {
            name += &format!("_{}", sanitize(&s.info.layers[self.display.t].label));
        }
        name + suffix
    }

    /// The basemap's credit, whenever it is drawn.
    pub(crate) fn basemap_attribution(&self) -> String {
        if !self.basemap.shown() {
            return String::new();
        }
        self.basemap.b.as_ref().map(|b| b.source().attribution.clone()).unwrap_or_default()
    }

    /// The top result drawn, as on the map: the last visible result of a placed layer (shown or not: its maps are
    /// layers of their own), else the active layer's last, else the last.
    pub(crate) fn top_result(&self) -> Option<usize> {
        let r = &self.zeit.results;
        let mut top = None;
        for (i, l) in self.layers.iter().enumerate() {
            if !l.place.aligned {
                continue;
            }
            let id = if i == self.active {
                self.session.as_ref().map(|s| s.info.id)
            } else {
                l.session.as_ref().map(|s| s.info.id)
            };
            for (k, x) in r.iter().enumerate() {
                if x.visible && Some(x.cube) == id {
                    top = Some(k);
                }
            }
        }
        if top.is_none()
            && let Some(s) = &self.session
        {
            top = r.iter().rposition(|x| x.cube == s.info.id);
        }
        top.or(if r.is_empty() { None } else { Some(r.len() - 1) })
    }

    fn titles(&self) -> Titles {
        let s = self.session.as_ref().unwrap();
        let info = &s.info;
        let d = &self.display;
        let emb = self.embedding_map_info();
        let active = self.layers.get(self.active);
        let comp = self.composite.as_ref().filter(|c| c.on && emb.is_none());
        let mode = d.mode;
        let dated = emb.is_none()
            && (comp.is_some() || matches!(mode, Mode::Value | Mode::Anomaly | Mode::Diff | Mode::Composite));
        let title = match (&emb, comp) {
            (Some(e), _) => e.0.clone(),
            (None, Some(c)) => c.title(info, d.t),
            _ => self.map_title(),
        };
        let name = active.map_or(String::new(), |l| l.name.clone());
        let mut sub = if name.is_empty() {
            crate::display::mode_name_of(mode, info).to_string()
        } else {
            format!("{name}  |  {}", crate::display::mode_name_of(mode, info))
        };
        let mut date_t = d.t;
        if let Some(c) = comp {
            let t = c.title(info, d.t);
            sub = format!("{name}  |  {}", t.find("RGB: ").map_or(t.as_str(), |k| &t[k..]));
            date_t = c.date_of(d.t, info.t());
        } else if mode == Mode::Composite {
            sub = format!(
                "{}{}",
                if name.is_empty() { String::new() } else { format!("{name}  |  ") },
                d.temporal_text(info, d.t)
            );
        } else if mode == Mode::Diff {
            let r = d.diff_ref_date(d.t, info.t()).map_or("(none)".to_string(), |r| info.layers[r].label.clone());
            sub += &format!(" from {r}");
        }
        Titles { title, date: info.layers[date_t].label.clone(), sub, dated }
    }

    /// Over `img` (w x h RGBA, rendered at `scale`): the label, legend, marks, scale bar and the basemap's credit,
    /// as the map draws them, in points x ps. Figures keep their text whatever the interface's font size.
    pub(crate) fn draw_figure_overlay(
        &self,
        img: &mut [u8],
        w: usize,
        h: usize,
        scale: i32,
        o: FigureOverlay,
        ppp: f32,
    ) {
        let Some(s) = &self.session else { return };
        let Some(g) = self.grid() else { return };
        let ps = ppp * scale as f32;
        let (canvas, _) = self.export_size(scale, ppp);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(canvas[0], canvas[1]));
        let to_px = |x: f64, y: f64| -> (f32, f32) {
            let p = self.map.screen_of(rect, x / g.fx, y / g.fy);
            (p.x * ps, p.y * ps)
        };
        let mut cv = Canvas { w: w as i32, h: h as i32, px: img };
        let fs = FIGURE_FONT_SIZE * ps;
        if o.marks {
            // vector layers under the ROI and the pins, as on the map
            for v in self.vectors.iter().filter(|v| v.visible && !v.px.is_empty()) {
                let col: Rgba = v.color.to_array();
                let dark = rgba(0, 0, 0, 170);
                for (i, parts) in v.px.iter().enumerate() {
                    let b = v.boxes[i];
                    if b[0] > b[2] {
                        continue;
                    }
                    let (b0, b1) = (to_px(f64::from(b[0]), f64::from(b[1])), to_px(f64::from(b[2]), f64::from(b[3])));
                    if b1.0 < -10.0 || b1.1 < -10.0 || b0.0 > w as f32 + 10.0 || b0.1 > h as f32 + 10.0 {
                        continue;
                    }
                    let kind = v.data.features[i].kind;
                    for r in parts {
                        if kind == janus_geo::vectors::Kind::Point {
                            let c = to_px(r[0], r[1]);
                            cv.circle(c.0, c.1, 5.5 * ps, dark, 0.0);
                            cv.circle(c.0, c.1, 4.0 * ps, col, 0.0);
                            continue;
                        }
                        let mut pts: Vec<f32> = Vec::new();
                        let mut k = 0;
                        while k + 1 < r.len() {
                            let q = to_px(r[k], r[k + 1]);
                            let m = pts.len();
                            let skip = m > 0 && k + 2 < r.len() && (q.0 - pts[m - 2]).hypot(q.1 - pts[m - 1]) < ps;
                            if !skip {
                                pts.extend([q.0, q.1]);
                            }
                            k += 2;
                        }
                        if pts.len() < 4 {
                            continue;
                        }
                        let closed = kind == janus_geo::vectors::Kind::Area;
                        cv.polyline(&pts, dark, 3.5 * ps, closed);
                        cv.polyline(&pts, col, 1.5 * ps, closed);
                    }
                }
            }
            let yellow = rgba(255, 210, 60, 255);
            if let Some(r) = self.series.roi_rect {
                if self.series.roi_poly.is_empty() {
                    let (a, b) = (to_px(r[0] as f64, r[1] as f64), to_px(r[2] as f64, r[3] as f64));
                    cv.rect(a.0, a.1, b.0, b.1, yellow, 2.0 * ps);
                } else {
                    // a quarter of a screen point
                    let tol = 0.25 / (self.map.scale / g.fx);
                    for ring in janus_geo::plane::simplify(&self.series.roi_poly, tol) {
                        let pts: Vec<f32> = ring
                            .chunks(2)
                            .flat_map(|v| {
                                let q = to_px(v[0], v[1]);
                                [q.0, q.1]
                            })
                            .collect();
                        cv.polyline(&pts, yellow, 2.0 * ps, true);
                    }
                }
            }
            for p in &self.series.pins {
                let c = to_px(p.x as f64 + 0.5, p.y as f64 + 0.5);
                let col: Rgba = p.color.to_array();
                cv.circle(c.0, c.1, 8.5 * ps, rgba(0, 0, 0, 220), 3.0 * ps);
                cv.circle(c.0, c.1, 6.5 * ps, col, 0.0);
                cv.circle(c.0, c.1, 6.5 * ps, rgba(255, 255, 255, 255), 1.5 * ps);
                let num = p.id.to_string();
                let (tx, ty) = (c.0 + 11.0 * ps, c.1 - 18.0 * ps);
                let tw = Canvas::text_width(&num, fs, Face::Bitmap);
                cv.fill_rect(tx - 3.0 * ps, ty - ps, tx + tw + 3.0 * ps, ty + fs + ps, rgba(0, 0, 0, 190));
                cv.text(tx, ty, &num, fs, col, Face::Bitmap);
            }
        }
        let t = self.titles();
        if o.label == 2 && t.dated {
            // an animation's date stamp: the date large (the scalable font), the layer and the mode under it
            let big = 30.0 * ps;
            let w_big = Canvas::text_width(&t.date, big, Face::Vector);
            let (x, y) = (12.0 * ps, 10.0 * ps);
            let w_sub = Canvas::text_width(&t.sub, fs, Face::Bitmap);
            cv.fill_rect(
                x - 7.0 * ps,
                y - 5.0 * ps,
                x + w_big.max(w_sub) + 7.0 * ps,
                y + big + 4.0 * ps + fs + 5.0 * ps,
                rgba(0, 0, 0, 165),
            );
            cv.text(x, y, &t.date, big, rgba(255, 255, 255, 255), Face::Vector);
            cv.text(x, y + big + 4.0 * ps, &t.sub, fs, rgba(225, 225, 225, 255), Face::Bitmap);
        } else if o.label > 0 {
            let (x, y) = (10.0 * ps, 8.0 * ps);
            let tw = Canvas::text_width(&t.title, fs, Face::Bitmap);
            cv.fill_rect(x - 5.0 * ps, y - 4.0 * ps, x + tw + 5.0 * ps, y + fs + 4.0 * ps, rgba(0, 0, 0, 165));
            cv.text(x, y, &t.title, fs, rgba(255, 255, 255, 255), Face::Bitmap);
        }
        if o.legend {
            let mut legends = Vec::new(); // bottom up: the active layer, then the result drawn on top
            let emb = self.embedding_map_info();
            let comp = self.composite.as_ref().is_some_and(|c| c.on);
            if let Some(a) = self.layers.get(self.active).filter(|l| l.visible) {
                if let Some((_, bar, _)) = &emb {
                    if let Some((cmap, lo, hi)) = *bar {
                        // similarity, change: a colour bar; principal components: none
                        let what = if t.title.contains("Change") {
                            " - change (cosine distance)"
                        } else {
                            " - cosine similarity"
                        };
                        legends.push(Legend {
                            caption: format!("{}{what}", a.name),
                            cmap: Some(cmap),
                            lo,
                            hi,
                            classes: Vec::new(),
                        });
                    }
                } else if self.display.mode != Mode::Rgb && !comp {
                    let mode = self.display.mode;
                    if self.classes.on() && mode == Mode::Value {
                        let classes = self
                            .classes
                            .list
                            .iter()
                            .filter(|e| e.visible)
                            .map(|e| ([e.color[0], e.color[1], e.color[2], 255], e.name.clone()));
                        legends.push(Legend {
                            caption: a.name.clone(),
                            cmap: None,
                            lo: 0.0,
                            hi: 1.0,
                            classes: classes.collect(),
                        });
                    } else {
                        let unit = if mode == Mode::Slope {
                            format!(" ({})", if s.info.time_is_date { "/yr" } else { "/step" })
                        } else {
                            String::new()
                        };
                        let r = &self.display.ranges[mode as usize];
                        legends.push(Legend {
                            caption: format!("{} - {}{unit}", a.name, crate::display::mode_name(mode)),
                            cmap: Some(self.display.cmap[mode as usize]),
                            lo: r.lo,
                            hi: r.hi,
                            classes: Vec::new(),
                        });
                    }
                }
            }
            if let Some(top) = self.top_result().map(|k| &self.zeit.results[k]).filter(|r| r.visible) {
                let mut l =
                    Legend { caption: top.name.clone(), cmap: None, lo: top.lo, hi: top.hi, classes: Vec::new() };
                if !top.classes.is_empty() {
                    let keys: Vec<[u8; 4]> = janus_gpu::colormaps::all()[top.cmap].keys().collect();
                    l.classes = top
                        .classes
                        .iter()
                        .enumerate()
                        .map(|(k, c)| {
                            (keys.get(k % keys.len().max(1)).copied().unwrap_or([128, 128, 128, 255]), c.clone())
                        })
                        .collect();
                } else {
                    l.cmap = Some(top.cmap);
                }
                legends.push(l);
            }
            draw_legends(&mut cv, &legends, ps);
        }
        let credit_top = draw_credit(&mut cv, &self.basemap_attribution(), ps); // whenever the basemap is drawn
        if o.scale_bar {
            // measured at the centre of the view; one target pixel is 1 / (scale ps) layer pixels
            let (cx, cy) = (self.map.center[0] * g.fx, self.map.center[1] * g.fy);
            let px_per_source = self.map.scale / g.fx * f64::from(ps);
            draw_scale_bar(&mut cv, self.ground_metres_per_pixel(cx, cy) / px_per_source, ps, credit_top);
        }
    }

    pub fn export_png(&mut self, path: String, o: PngOptions, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        let ppp = ctx.pixels_per_point();
        let transparent = o.background == 2;
        let bg = (o.background == 1).then_some([1.0, 1.0, 1.0, 1.0]);
        let (mut img, w, h) = self.render_export(o.scale, transparent, bg, ppp)?;
        let fo = FigureOverlay { label: u8::from(o.label), legend: o.legend, marks: o.marks, scale_bar: o.scale_bar };
        self.draw_figure_overlay(&mut img, w, h, o.scale, fo, ppp);
        let title = format!("Map as PNG, {w} x {h} px");
        let p = path.clone();
        let job = start_export_job(title, path, ctx, move |j| {
            write_image(&p, "PNG", &img, w, h, transparent, None, "", &[("ZLEVEL", "6")], j, "")
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }

    /// The active layer's values at the date: the whole image or the visible area, Float32, georeferenced.
    pub fn export_values(&mut self, path: String, whole: bool, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        let s = self.session.as_ref()?;
        let info = s.info.clone();
        let win = if whole { [0, 0, info.width, info.height] } else { self.embedding_visible_window_active()? };
        let t = self.display.t;
        let name = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let title =
            format!("{name} at {}, {} x {} px (Float32)", info.layers[t].label, win[2] - win[0], win[3] - win[1]);
        let threads = self.session_settings().processing_threads().to_string();
        let p = path.clone();
        let job = start_export_job(title, path, ctx, move |j| {
            let (x0, y0, w, h) = (win[0], win[1], win[2] - win[0], win[3] - win[1]);
            let drv =
                gdal::DriverManager::get_driver_by_name("GTiff").map_err(|_| "GDAL has no GTiff driver".to_string())?;
            let mut co = gdal::raster::RasterCreationOptions::new();
            for (k, v) in [
                ("TILED", "YES"),
                ("COMPRESS", "DEFLATE"),
                ("PREDICTOR", "3"),
                ("BIGTIFF", "IF_SAFER"),
                ("NUM_THREADS", threads.as_str()),
            ] {
                let _ = co.set_name_value(k, v);
            }
            let tmp = format!("{p}.part");
            let mut out = drv
                .create_with_band_type_with_options::<f32, _>(&tmp, w, h, 1, &co)
                .map_err(|_| last_gdal_error("could not create the file"))?;
            if let Some(g) = info.geo_transform {
                let (fx, fy) = (x0 as f64, y0 as f64);
                let _ = out.set_geo_transform(&[
                    g[0] + fx * g[1] + fy * g[2],
                    g[1],
                    g[2],
                    g[3] + fx * g[4] + fy * g[5],
                    g[4],
                    g[5],
                ]);
            }
            let wkt = file_wkt(&info.layers[t].path);
            if !wkt.is_empty() {
                let _ = out.set_projection(&wkt);
            }
            let what = info.selection_text();
            let _ = out.set_metadata_item("JANUS_DATE", &info.layers[t].label, "");
            if !what.is_empty() {
                let _ = out.set_metadata_item("JANUS_QUANTITY", &what, "");
            }
            let src = std::path::absolute(&info.layers[t].path)
                .map_or(info.layers[t].path.clone(), |p| p.to_string_lossy().into_owned());
            let _ = out.set_metadata_item("JANUS_SOURCE", &src, "");
            let mut ok = true;
            let mut error = String::new();
            {
                let mut band = out.rasterband(1).map_err(|e| e.to_string())?;
                let _ = band.set_no_data_value(Some(f64::NAN));
                let desc = if what.is_empty() {
                    info.layers[t].label.clone()
                } else {
                    format!("{} {what}", info.layers[t].label)
                };
                let _ = band.set_description(&desc);
                // strips of whole tiles, read with a reader of our own
                let mut reader = CubeReader::new(info.clone());
                const STRIP: usize = 256;
                let mut buf = vec![0f32; w * STRIP];
                let mut r = 0;
                while r < h && ok && !j.cancelled() {
                    let n = STRIP.min(h - r);
                    if !reader.read_window(t, (x0, y0 + r, w, n), &mut buf, (w, n)) {
                        error = last_gdal_error(&format!("could not read {}", info.layers[t].path));
                        ok = false;
                    } else {
                        let mut b = gdal::raster::Buffer::new((w, n), buf[..w * n].to_vec());
                        if band.write((0, r as isize), (w, n), &mut b).is_err() {
                            error = last_gdal_error("could not write the file");
                            ok = false;
                        }
                    }
                    r += n;
                    j.set_progress(r as f64 / h as f64);
                }
            }
            // SAFETY: GDAL's error state.
            unsafe { gdal_sys::CPLErrorReset() };
            drop(out);
            if ok && unsafe { gdal_sys::CPLGetLastErrorType() } >= gdal_sys::CPLErr::CE_Failure {
                error = last_gdal_error("could not write the file");
                ok = false;
            }
            finish_file(&tmp, &p, ok && !j.cancelled()).map_err(|e| if e.is_empty() { error } else { e })
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }

    /// The rendered view as an RGBA GeoTIFF in the active layer's CRS (transparent where nothing is drawn).
    pub fn export_view(&mut self, path: String, scale: i32, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        let ppp = ctx.pixels_per_point();
        let (mut img, w, h) = self.render_export(scale, true, None, ppp)?;
        let credit = self.basemap_attribution();
        {
            let mut cv = Canvas { w: w as i32, h: h as i32, px: &mut img };
            draw_credit(&mut cv, &credit, ppp * scale as f32);
        }
        // target pixel (px, py) -> active layer pixel: the view's top left plus px / (screen points per source
        // pixel x ps)
        let s = self.session.as_ref()?;
        let info = &s.info;
        let g = self.grid()?;
        let ps = f64::from(ppp) * f64::from(scale);
        let src_per_target = g.fx / (self.map.scale * ps);
        let (canvas, _) = self.export_size(scale, ppp);
        let top_left = self.map.image_at(Rect::from_min_size(Pos2::ZERO, vec2(canvas[0], canvas[1])), Pos2::ZERO);
        let (ax, ay) = (top_left[0] * g.fx, top_left[1] * g.fy);
        let gt = info.geo_transform.map(|g| {
            [
                g[0] + ax * g[1] + ay * g[2],
                g[1] * src_per_target,
                g[2] * src_per_target,
                g[3] + ax * g[4] + ay * g[5],
                g[4] * src_per_target,
                g[5] * src_per_target,
            ]
        });
        let src = info.layers[self.display.t].path.clone();
        let title = format!("Rendered view as GeoTIFF, {w} x {h} px (RGBA)");
        let p = path.clone();
        let job = start_export_job(title, path, ctx, move |j| {
            let opts = [
                ("TILED", "YES"),
                ("COMPRESS", "DEFLATE"),
                ("PHOTOMETRIC", "RGB"),
                ("ALPHA", "NON-PREMULTIPLIED"),
                ("BIGTIFF", "IF_SAFER"),
            ];
            let wkt = if gt.is_some() { file_wkt(&src) } else { String::new() };
            let copyright = if credit.is_empty() { String::new() } else { format!("Basemap: {credit}") };
            write_image(&p, "GTiff", &img, w, h, true, gt, &wkt, &opts, j, &copyright)
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }

    /// A result's GeoTIFF copied (in chunks, for the progress), with its side file.
    pub fn export_result(&mut self, k: usize, path: String, ctx: &egui::Context) -> Option<Arc<ExportJob>> {
        let r = self.zeit.results.get(k)?;
        let src = r.path.clone();
        let p = path.clone();
        let job = start_export_job(format!("{} as GeoTIFF", r.name), path, ctx, move |j| {
            if std::fs::canonicalize(&src).ok().is_some_and(|a| std::fs::canonicalize(&p).ok() == Some(a)) {
                return Ok(());
            }
            let total = std::fs::metadata(&src).map_or(1, |m| m.len().max(1));
            let tmp = format!("{p}.part");
            let copy = || -> Result<(), String> {
                use std::io::{Read, Write};
                let mut i = std::fs::File::open(&src).map_err(|_| format!("could not open {src}"))?;
                let mut o = std::fs::File::create(&tmp).map_err(|_| format!("could not open {tmp}"))?;
                let mut buf = vec![0u8; 4 << 20];
                let mut done = 0u64;
                while !j.cancelled() {
                    let n = i.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    o.write_all(&buf[..n]).map_err(|_| format!("could not write {tmp}"))?;
                    done += n as u64;
                    j.set_progress(done as f64 / total as f64);
                }
                Ok(())
            };
            let r = copy();
            let ok = r.is_ok() && !j.cancelled();
            finish_file(&tmp, &p, ok).map_err(|e| if e.is_empty() { r.err().unwrap_or_default() } else { e })?;
            // a side file with statistics or metadata, if any, goes along
            let aux = format!("{src}.aux.xml");
            if std::path::Path::new(&aux).exists() {
                let _ = std::fs::copy(&aux, format!("{p}.aux.xml"));
            }
            Ok(())
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }

    /// The outputs of result k's run (the results from its folder, of its series).
    pub(crate) fn run_outputs(&self, k: usize) -> Vec<usize> {
        let r = &self.zeit.results[k];
        let run = std::path::Path::new(&r.path).parent().map(|p| p.to_path_buf());
        (0..self.zeit.results.len())
            .filter(|&i| {
                let x = &self.zeit.results[i];
                x.cube == r.cube && std::path::Path::new(&x.path).parent().map(|p| p.to_path_buf()) == run
            })
            .collect()
    }

    /// "<layer>_<output>.tif", e.g. ndvi_landtrendr_yod.tif.
    pub(crate) fn result_file_name(&self, k: usize) -> String {
        let r = &self.zeit.results[k];
        let mut layer = String::new();
        for (i, l) in self.layers.iter().enumerate() {
            let id = if i == self.active {
                self.session.as_ref().map(|s| s.info.id)
            } else {
                l.session.as_ref().map(|s| s.info.id)
            };
            if id == Some(r.cube) {
                layer = format!("{}_", sanitize(&l.name));
            }
        }
        let stem =
            std::path::Path::new(&r.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{layer}{}.tif", sanitize(&stem))
    }

    pub fn save_result(&mut self, k: usize, ctx: &egui::Context) {
        let name = self.result_file_name(k);
        if let Some(path) = self.ask_save_path("Save the result as GeoTIFF", &name, "GeoTIFF", "tif", "") {
            self.export_result(k, path, ctx);
        }
    }

    pub fn open_save_run(&mut self, k: usize) {
        self.export.save_run = Some((self.zeit.results[k].path.clone(), self.export_folder()));
    }

    pub fn export_run(&mut self, k: usize, folder: &str, ctx: &egui::Context) -> Vec<Arc<ExportJob>> {
        let mut jobs = Vec::new();
        for i in self.run_outputs(k) {
            let path = std::path::Path::new(folder).join(self.result_file_name(i)).to_string_lossy().into_owned();
            jobs.extend(self.export_result(i, path, ctx));
        }
        self.settings.s.export_dir = folder.to_string();
        jobs
    }

    /// The system's save dialog in the export folder (or `folder`), the folder remembered.
    pub fn ask_save_path(&mut self, title: &str, name: &str, filter: &str, ext: &str, folder: &str) -> Option<String> {
        let mut dlg = rfd::FileDialog::new().set_title(title).set_file_name(name).add_filter(filter, &[ext]);
        let dir = if folder.is_empty() { self.export_folder() } else { folder.to_string() };
        if !dir.is_empty() {
            dlg = dlg.set_directory(dir);
        }
        let p = dlg.save_file()?.to_string_lossy().into_owned();
        self.remember_export_folder(&p);
        Some(p)
    }

    /// The typed file: trimmed (as pasted from Explorer), a name alone in the export folder, `ext` added.
    pub(crate) fn export_target(&self, ext: &str) -> String {
        let typed = self.export.path.trim_matches([' ', '\t', '"']);
        if typed.is_empty() {
            return String::new();
        }
        let mut p = std::path::PathBuf::from(typed);
        if p.is_relative() {
            p = std::path::Path::new(&self.export_folder()).join(p);
        }
        if p.file_name().is_none() {
            return String::new();
        }
        let have = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let mut s = p.to_string_lossy().into_owned();
        if have != ext && !(ext == "tif" && have == "tiff") {
            s += &format!(".{ext}");
        }
        s
    }

    /// The File menu's exports.
    pub(crate) fn export_menu(&mut self, ui: &mut Ui) {
        let has = self.session.is_some();
        let geo = self.session.as_ref().is_some_and(|s| s.info.geo_transform.is_some());
        let item = |ui: &mut Ui, label: &str, keys: &str, enabled: bool| {
            ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys))
        };
        if item(ui, "Export map as PNG...", "Ctrl+E", has).clicked() {
            self.open_export(1);
            ui.close();
        }
        if item(ui, "Export values as GeoTIFF...", "Ctrl+Shift+E", has).clicked() {
            self.open_export(2);
            ui.close();
        }
        if item(ui, "Export rendered view as GeoTIFF...", "Ctrl+Shift+V", geo).clicked() {
            self.open_export(3);
            ui.close();
        }
        let emb = self.embedding_export_spec(true).is_ok();
        if item(ui, "Export embeddings as GeoTIFF...", "Ctrl+Shift+M", emb)
            .on_hover_text("The embedding view shown (principal components, similarity or change)\nat full resolution, read again from the files")
            .clicked()
        {
            self.open_export(4);
            ui.close();
        }
        if item(ui, "Export animation (GIF, MP4, WebM)...", "Ctrl+Shift+A", has)
            .on_hover_text(
                "The map over a range of dates as a timelapse, with the date stamp,\nlegend, scale bar and basemap",
            )
            .clicked()
        {
            self.open_export(5);
            ui.close();
        }
        ui.add_enabled_ui(!self.zeit.results.is_empty(), |ui| {
            ui.menu_button("Export Zeit result as GeoTIFF", |ui| {
                if let Some(top) = self.top_result() {
                    let name = self.zeit.results[top].name.clone();
                    if ui.add(egui::Button::new(format!("Save {name}...")).shortcut_text("Ctrl+S")).on_hover_text("The result drawn on top of the map").clicked() {
                        ui.close();
                        self.save_result(top, ui.ctx());
                    }
                    if ui
                        .add(egui::Button::new("Save all outputs of its run...").shortcut_text("Ctrl+Shift+S"))
                        .on_hover_text("Every output of the run of the result on top (e.g. all of LandTrendr's)\ninto a folder at once")
                        .clicked()
                    {
                        self.open_save_run(top);
                        ui.close();
                    }
                }
                ui.separator();
                for k in 0..self.zeit.results.len() {
                    let name = self.zeit.results[k].name.clone();
                    if ui.button(name).clicked() {
                        ui.close();
                        self.save_result(k, ui.ctx());
                    }
                }
            });
        });
        if ui.add(egui::Button::selectable(self.show_exports, "Exports").shortcut_text("Ctrl+J")).clicked() {
            self.show_exports = !self.show_exports;
            ui.close();
        }
    }

    pub(crate) fn open_export(&mut self, kind: usize) {
        self.export.kind = kind;
        self.export.open = true;
    }

    /// Ctrl+E, Ctrl+Shift+E / V / M / A, Ctrl+S, Ctrl+Shift+S, Ctrl+J.
    pub(crate) fn export_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        if self.export.shown || self.export.save_run.is_some() {
            return; // a dialog is open: one at a time
        }
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let chord = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let has = self.session.is_some();
        if has && chord(cs, Key::E) {
            self.open_export(2);
        }
        if has && chord(Modifiers::COMMAND, Key::E) {
            self.open_export(1);
        }
        if has && self.session.as_ref().is_some_and(|s| s.info.geo_transform.is_some()) && chord(cs, Key::V) {
            self.open_export(3);
        }
        if self.embedding_export_spec(true).is_ok() && chord(cs, Key::M) {
            self.open_export(4);
        }
        if has && chord(cs, Key::A) {
            self.open_export(5);
        }
        if let Some(top) = self.top_result() {
            if chord(cs, Key::S) {
                self.open_save_run(top);
            } else if chord(Modifiers::COMMAND, Key::S) {
                self.save_result(top, ctx);
            }
        }
        if chord(Modifiers::COMMAND, Key::J) {
            self.show_exports = !self.show_exports;
        }
    }

    /// The default file name of the export of this kind.
    fn export_default_name(&self, kind: usize) -> String {
        if kind == 5 {
            let d = self.anim_dates(&self.anim.o);
            let mut name = self.layers.get(self.active).map_or("janus".to_string(), |l| sanitize(&l.name));
            if let (Some(s), Some(a), Some(b)) = (&self.session, d.first(), d.last()) {
                name += &format!("_{}-{}", sanitize(&s.info.layers[*a].label), sanitize(&s.info.layers[*b].label));
            }
            return format!("{name}.{}", crate::animation::anim_ext(self.anim.o.format));
        }
        if kind == 4
            && let (Some(i), Ok(e)) = (self.embedding_layer(), self.embedding_export_spec(true))
        {
            let l = &self.layers[i];
            let date = e.info.layers.get(e.t).map_or(String::new(), |l| sanitize(&l.label));
            let view = match e.kind {
                janus_embed::export::ExportKind::Pca => "_pca.tif",
                janus_embed::export::ExportKind::Similarity => "_similarity.tif",
                _ => "_change.tif",
            };
            return format!("{}_{date}{view}", sanitize(&l.name));
        }
        self.export_name(["", ".png", ".tif", "_view.tif", ".tif", ".gif"][kind.min(5)])
    }

    /// The export dialog: its options, where the file goes, Export.
    pub(crate) fn export_dialog(&mut self, ctx: &egui::Context) {
        const TITLES: [&str; 6] = [
            "",
            "Export map as PNG",
            "Export values as GeoTIFF",
            "Export rendered view as GeoTIFF",
            "Export embeddings as GeoTIFF",
            "Export animation",
        ];
        const FILTERS: [&str; 6] = ["", "PNG image", "GeoTIFF", "GeoTIFF", "GeoTIFF", "Animation"];
        let kind = self.export.kind.min(5);
        if self.export.open {
            self.export.open = false;
            self.export.shown = kind > 0 && self.session.is_some();
            if kind == 5
                && let Some(s) = &self.session
            {
                // every date at the map's speed, unless changed in the dialog
                self.anim.o.from = 0;
                self.anim.o.to = s.info.t() as i32 - 1;
                self.anim.o.fps = self.display.fps;
            }
            // where it goes: the last export's folder (else the layer's)
            let name = self.export_default_name(kind);
            self.export.path = std::path::Path::new(&self.export_folder()).join(name).to_string_lossy().into_owned();
        }
        if !self.export.shown {
            return;
        }
        if self.session.is_none() || kind == 0 {
            self.export.shown = false;
            return;
        }
        let ppp = ctx.pixels_per_point();
        let mut ext = if kind == 5 {
            crate::animation::anim_ext(self.anim.o.format)
        } else {
            ["", "png", "tif", "tif", "tif", "gif"][kind]
        };
        let (mut save, mut cancel, mut can) = (false, false, true);
        let modal = egui::Modal::new(egui::Id::new("export dialog")).show(ctx, |ui| {
            ui.heading(TITLES[kind]);
            let info = self.session.as_ref().unwrap().info.clone();
            // the scale of a rendered export: 1x, 2x, 4x the map's resolution on screen
            let scale_choice = |s: &State, ui: &mut Ui, scale: &mut i32, choices: &[i32]| {
                ui.label("Resolution (rendered at it, not enlarged):");
                for &k in choices {
                    let (_, [w, h]) = s.export_size(k, ppp);
                    let fits = s.export_scale_fits(k, ppp);
                    if !fits && *scale == k {
                        *scale = 1;
                    }
                    ui.add_enabled_ui(fits, |ui| ui.radio_value(scale, k, format!("{k}x  ({w} x {h} px)")))
                        .response
                        .on_disabled_hover_text("Too large for the GPU");
                }
                if s.map.detail_level.is_some() || s.display.mode != Mode::Value {
                    ui.weak("Data drawn as on screen: overview or the detail tiles already read.");
                }
            };
            match kind {
                1 => {
                    let mut scale = self.export.png.scale;
                    scale_choice(self, ui, &mut scale, &[1, 2, 4]);
                    let o = &mut self.export.png;
                    o.scale = scale;
                    ui.add_space(4.0);
                    let bgs = ["Map's (dark)", "White", "Transparent"];
                    egui::ComboBox::from_label("Background").width(200.0).selected_text(bgs[o.background.min(2)]).show_ui(ui, |ui| {
                        for (k, b) in bgs.iter().enumerate() {
                            ui.selectable_value(&mut o.background, k, *b);
                        }
                    });
                    ui.checkbox(&mut o.label, "Date and mode label");
                    ui.checkbox(&mut o.legend, "Legend (colour bar or classes)");
                    ui.checkbox(&mut o.marks, "Pins, ROI and vector layers");
                    ui.add_enabled(info.geo_transform.is_some() && !info.crs_wkt.is_empty(), egui::Checkbox::new(&mut o.scale_bar, "Scale bar"))
                        .on_hover_text("Ground distance at the centre of the view (the layer needs a CRS)");
                }
                2 => {
                    let name = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
                    ui.colored_label(theme::accent(), name);
                    let what = info.selection_text();
                    ui.label(format!("Date {}{}{what}", info.layers[self.display.t].label, if what.is_empty() { "" } else { ", " }));
                    let win = self.embedding_visible_window_active();
                    let (vw, vh) = win.map_or((0, 0), |w| (w[2] - w[0], w[3] - w[1]));
                    let e = &mut self.export;
                    if win.is_none() {
                        e.values_whole = true;
                    }
                    ui.add_enabled_ui(win.is_some(), |ui| {
                        if ui.radio(!e.values_whole, format!("Visible area  ({vw} x {vh} px)")).clicked() {
                            e.values_whole = false;
                        }
                    });
                    if ui.radio(e.values_whole, format!("Whole image  ({} x {} px)", info.width, info.height)).clicked() {
                        e.values_whole = true;
                    }
                    let px = if e.values_whole { (info.width * info.height) as f64 } else { (vw * vh) as f64 };
                    ui.weak(format!("Full resolution, Float32, no data = NaN, {:.0} MB before compression.", px * 4.0 / 1e6));
                    ui.weak(if info.geo_transform.is_some() {
                        "Georeferenced like the layer (same grid and CRS)."
                    } else {
                        "The layer has no georeferencing: pixel coordinates only."
                    });
                    if self.display.mode != Mode::Value {
                        ui.colored_label(
                            theme::warning(),
                            format!("The map shows {}; the file holds the values at the date.", crate::display::mode_name(self.display.mode)),
                        );
                    }
                    if self.session.as_ref().is_some_and(|s| s.s.defer_random_reads()) {
                        // as pins and tiles: the HDD's head stays on the overview
                        ui.colored_label(theme::warning(), "Building the overview from an HDD: available once it ends.");
                        can = false;
                    }
                }
                3 => {
                    let mut scale = self.export.view_scale;
                    scale_choice(self, ui, &mut scale, &[1, 2, 4]);
                    self.export.view_scale = scale;
                    ui.weak("RGB + alpha (transparent where nothing is drawn), as shown,\ngeoreferenced in the active layer's CRS.");
                }
                5 => {
                    let mut scale = self.anim.o.scale;
                    scale_choice(self, ui, &mut scale, &[1, 2]);
                    self.anim.o.scale = scale;
                    can &= self.animation_options_ui(ui);
                    ext = crate::animation::anim_ext(self.anim.o.format); // may have changed above
                }
                _ => {
                    let l = self.embedding_layer();
                    ui.colored_label(theme::accent(), l.map_or(String::new(), |i| self.layers[i].name.clone()));
                    // the embeddings' own visible window and size
                    let vis = self.embedding_export_spec(false);
                    let whole = self.embedding_export_spec(true);
                    let e = &mut self.export;
                    if vis.is_err() {
                        e.values_whole = true;
                    }
                    let (vw, vh) = vis.as_ref().map_or((0, 0), |s| (s.win[2] - s.win[0], s.win[3] - s.win[1]));
                    ui.add_enabled_ui(vis.is_ok(), |ui| {
                        if ui.radio(!e.values_whole, format!("Visible area  ({vw} x {vh} px)")).clicked() {
                            e.values_whole = false;
                        }
                    });
                    let (ww, wh) = whole.as_ref().map_or((0, 0), |s| (s.win[2] - s.win[0], s.win[3] - s.win[1]));
                    if ui.radio(e.values_whole, format!("Whole layer  ({ww} x {wh} px)")).clicked() {
                        e.values_whole = true;
                    }
                    match if e.values_whole { &whole } else { &vis } {
                        Ok(spec) => {
                            for b in &spec.band_names {
                                ui.label(format!("  - {b}"));
                            }
                            let px = ((spec.win[2] - spec.win[0]) * (spec.win[3] - spec.win[1])) as f64;
                            ui.weak(format!(
                                "Full resolution, Float32, no data = NaN, {:.0} MB before compression.",
                                px * spec.bands() as f64 * 4.0 / 1e6
                            ));
                            ui.weak(if spec.kind == janus_embed::export::ExportKind::Pca {
                                "Scores along each component (not stretched), from the vectors in the files."
                            } else {
                                "Computed again from the vectors in the files, not from the reduced copy shown."
                            });
                            ui.weak(if spec.info.geo_transform.is_some() {
                                "Georeferenced like the layer of embeddings (its grid and CRS)."
                            } else {
                                "The layer has no georeferencing: pixel coordinates only."
                            });
                        }
                        Err(why) => {
                            ui.colored_label(theme::warning(), why);
                            can = false;
                        }
                    }
                }
            }
            // where the file goes: typed (a name alone goes to the export folder) or chosen in the system's dialog
            crate::panels::section(ui, "Save to");
            let mut enter = false;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.export.path).desired_width(ui.text_style_height(&egui::TextStyle::Body) * 34.0));
                enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let tip = format!("Full path of the file, or a name alone (saved in {}).", self.export_folder());
                r.on_hover_text(tip);
                if ui.button("Browse...").clicked() {
                    let target = self.export_target(ext);
                    let t = std::path::Path::new(&target);
                    let known = !target.is_empty() && t.parent().is_some_and(|p| p.is_dir());
                    let name = if known { t.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default() } else { self.export_default_name(kind) };
                    let folder = if known { t.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() } else { String::new() };
                    if let Some(p) = self.ask_save_path(TITLES[kind], &name, FILTERS[kind], ext, &folder) {
                        self.export.path = p;
                    }
                }
            });
            let target = self.export_target(ext);
            let t = std::path::Path::new(&target);
            if target.is_empty() {
                ui.colored_label(theme::warning(), "Type a file name, or choose one with Browse...");
                can = false;
            } else if !t.parent().is_some_and(|p| p.is_dir()) {
                ui.colored_label(theme::warning(), format!("The folder {} does not exist.", t.parent().map_or(String::new(), |p| p.display().to_string())));
                can = false;
            } else if t.is_dir() {
                ui.colored_label(theme::warning(), format!("{} is a folder: add a file name.", t.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned())));
                can = false;
            } else if t.exists() {
                ui.colored_label(theme::warning(), format!("{} exists and will be replaced.", t.file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned())));
            } else {
                ui.weak(format!("Saved as {target}"));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                save = ui.add_enabled(can, egui::Button::new("Export").min_size(vec2(120.0, 0.0))).clicked() || (enter && can);
                cancel = ui.add(egui::Button::new("Cancel").min_size(vec2(120.0, 0.0))).clicked();
            });
            target
        });
        if cancel || modal.should_close() {
            self.export.shown = false;
            return;
        }
        if save {
            let target = modal.inner;
            self.export.shown = false;
            self.remember_export_folder(&target);
            match kind {
                1 => {
                    self.export_png(target, self.export.png, ctx);
                }
                2 => {
                    self.export_values(target, self.export.values_whole, ctx);
                }
                3 => {
                    self.export_view(target, self.export.view_scale, ctx);
                }
                5 => {
                    let o = self.anim.o;
                    self.start_animation(target, o, ctx);
                }
                _ => {
                    if let Ok(spec) = self.embedding_export_spec(self.export.values_whole) {
                        self.export_embeddings(target, spec, ctx);
                    }
                }
            }
        }
    }

    /// "Save all outputs": the run's outputs, the folder, Save all.
    pub(crate) fn save_run_dialog(&mut self, ctx: &egui::Context) {
        let Some((of, _)) = self.export.save_run.clone() else { return };
        let Some(k) = self.zeit.results.iter().position(|r| r.path == of) else {
            self.export.save_run = None; // the result went
            return;
        };
        let outs = self.run_outputs(k);
        let (mut save, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::new("save run")).show(ctx, |ui| {
            ui.heading("Save all outputs");
            let r = &self.zeit.results[k];
            let tool = r.name.split(':').next().unwrap_or("").to_string();
            ui.colored_label(
                theme::accent(),
                format!("{tool}: {} output{}", outs.len(), if outs.len() == 1 { "" } else { "s" }),
            );
            let run = std::path::Path::new(&r.path)
                .parent()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.weak(format!("Run {run}"));
            crate::panels::section(ui, "Save to the folder");
            let mut folder = self.export.save_run.as_ref().map(|s| s.1.clone()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut folder)
                        .desired_width(ui.text_style_height(&egui::TextStyle::Body) * 34.0),
                );
                if ui.button("Browse...").clicked()
                    && let Some(d) = rfd::FileDialog::new().pick_folder()
                {
                    folder = d.to_string_lossy().into_owned();
                }
            });
            let f = folder.trim_matches([' ', '\t', '"']).to_string();
            let ok = !f.is_empty() && std::path::Path::new(&f).is_dir();
            // each file as written: Float32 GeoTIFF copies, named after the layer and the output
            for &i in &outs {
                let name = self.result_file_name(i);
                ui.horizontal(|ui| {
                    ui.label(format!("  - {name}")).on_hover_text(&self.zeit.results[i].name);
                    if ok && std::path::Path::new(&f).join(&name).exists() {
                        ui.colored_label(theme::warning(), "(replaces the file there)");
                    }
                });
            }
            if !ok {
                ui.colored_label(
                    theme::warning(),
                    if f.is_empty() {
                        "Type a folder, or choose one with Browse..."
                    } else {
                        "This folder does not exist."
                    },
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                save = ui.add_enabled(ok, egui::Button::new("Save all").min_size(vec2(120.0, 0.0))).clicked();
                cancel = ui.add(egui::Button::new("Cancel").min_size(vec2(120.0, 0.0))).clicked();
            });
            if let Some(s) = self.export.save_run.as_mut() {
                s.1 = folder;
            }
            f
        });
        if cancel || modal.should_close() {
            self.export.save_run = None;
        } else if save {
            self.export.save_run = None;
            self.export_run(k, &modal.inner, ctx);
        }
    }
}
