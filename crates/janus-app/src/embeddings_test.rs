//! `--selftest-embeddings-ui DIR [OUT]` (src/app_embeddings.cpp, selfTestEmbeddingStep): a series of embeddings in
//! the window: the principal components of every year (against a plain PCA of the same sample), the local PCA of an
//! ROI, the similarity to a pin, the change from the previous year, the direction of change, classes from three pins,
//! every year in turn; each view also written as a GeoTIFF of the whole layer at full resolution and checked against
//! the image shown (the direction and the classes also as PNGs of the map). (The C++'s PNG figures come
//! with the exports, its download stage with the native downloads.)

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;

use janus_embed::{ExportKind, sample_vectors};

use crate::app::State;
use crate::embeddings::View;
use crate::exports::{ExportJob, ExportState};

const SLOT: i32 = 97;

#[derive(Default)]
pub struct EmbeddingsTest {
    stage: usize,
    frames: usize,
    t0: Option<Instant>,
    since: Option<Instant>,
    out: std::path::PathBuf,
    job: Option<(Arc<ExportJob>, janus_embed::EmbeddingExport)>,
    /// the download's last progress message shown
    shown: String,
    result: Option<i32>,
}

impl State {
    pub(crate) fn embeddings_test_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        if self.embeddings_test.result.is_some() {
            return self.embeddings_test.result;
        }
        let mut st = std::mem::take(&mut self.embeddings_test);
        let r = self.embeddings_test_stage(&mut st, ctx);
        st.result = r;
        self.embeddings_test = st;
        r
    }

    /// Distinct colours over a 23 x 23 grid of the map drawn at 400 x 300.
    fn map_colours(&mut self) -> usize {
        let Some(g) = self.grid() else { return 0 };
        let s = 0.98 * (400.0 / g.ow).min(300.0 / g.oh);
        self.map.set_view([g.ow / 2.0, g.oh / 2.0], s);
        self.render_map(SLOT, [400, 300], [400.0, 300.0], 1.0);
        let Some((w, h, px)) = self.gpu.as_ref().and_then(|gpu| gpu.read_target(SLOT)) else { return 0 };
        let mut seen = BTreeSet::new();
        for j in 1..24 {
            for i in 1..24 {
                let p = ((h * j / 24) * w + w * i / 24) as usize * 4;
                seen.insert((px[p], px[p + 1], px[p + 2]));
            }
        }
        seen.len()
    }

    /// The map drawn at 800 x 600 into a PNG.
    fn save_map_png(&mut self, path: &std::path::Path) -> Result<(), String> {
        let g = self.grid().ok_or("no grid")?;
        let sc = 0.98 * (800.0 / g.ow).min(600.0 / g.oh);
        self.map.set_view([g.ow / 2.0, g.oh / 2.0], sc);
        self.render_map(SLOT, [800, 600], [800.0, 600.0], 1.0);
        let (w, h, px) = self.gpu.as_ref().and_then(|gpu| gpu.read_target(SLOT)).ok_or("no image of the map")?;
        image::save_buffer(path, &px, w, h, image::ColorType::Rgba8).map_err(|e| e.to_string())?;
        println!("    {}", path.display());
        Ok(())
    }

    /// The GeoTIFF's grid, then its values at the source pixel of every store pixel of a grid: the image's
    /// (similarity, change) or the scores of the store's vector (principal components), within the store's Int8
    /// steps.
    fn check_tif(&self, path: &str, spec: &janus_embed::EmbeddingExport) -> Result<(), String> {
        let li = self.embedding_layer().ok_or("no layer of embeddings")?;
        let e = self.layers[li].emb.as_ref().ok_or("no layer of embeddings")?;
        let s = e.store.as_ref().ok_or("no store")?;
        let r = s.ranges().ok_or("no ranges")?;
        let ds = gdal::Dataset::open(path).map_err(|er| format!("could not open {path}: {er}"))?;
        let info = &spec.info;
        let k = spec.bands();
        if ds.raster_size() != (info.width, info.height) || ds.raster_count() != k {
            return Err("the GeoTIFF should have the layer's size and one band per value".into());
        }
        if let Some(g) = info.geo_transform {
            let gt = ds.geo_transform().map_err(|_| "the GeoTIFF should be on the layer's grid")?;
            if (gt[0] - g[0]).abs() > 1e-9 || (gt[3] - g[3]).abs() > 1e-9 || (gt[1] - g[1]).abs() > 1e-12 {
                return Err("the GeoTIFF should be on the layer's grid".into());
            }
        }
        // a vector beyond the store's range is clipped there (Int8): the image is off, not the file
        let saturated = |t: usize, p: usize| s.valid(t, p) && s.vec(t, p).iter().any(|&c| c == 127 || c == -127);
        let (mut worst, mut bound, mut n, mut clipped) = (0f64, 0f64, 0usize, 0usize);
        let mut v = vec![0f32; s.d];
        // the direction and the classes: a pixel near the threshold (or a tie of two classes) may fall either side of
        // it in the store's Int8 steps; a few such pixels are allowed
        let lenient = matches!(spec.kind, ExportKind::Direction | ExportKind::Classes);
        // a pixel missing in both (unchanged, below the minimum score) agrees too
        let (mut misses, mut blank) = (0usize, 0usize);
        let pair = matches!(spec.kind, ExportKind::Change | ExportKind::Direction | ExportKind::ChangeLike);
        for band in 0..k {
            let b = ds.rasterband(band + 1).map_err(|er| er.to_string())?;
            let data = b
                .read_as::<f32>((0, 0), (info.width, info.height), (info.width, info.height), None)
                .map_err(|er| er.to_string())?;
            let data = data.data();
            for j in 0..16 {
                for i in 0..16 {
                    let (sx, sy) = ((s.w - 1) * i / 15, (s.h - 1) * j / 15);
                    let p = sy * s.w + sx;
                    let sat =
                        usize::from(saturated(spec.t, p)) + usize::from(pair && saturated(spec.t0.unwrap_or(0), p));
                    if sat > 0 {
                        // a change reads two vectors per pixel: each counts
                        clipped += if band == 0 { sat } else { 0 };
                        continue;
                    }
                    let x = (((sx as f64 + 0.5) * s.fx) as usize).min(info.width - 1);
                    let y = (((sy as f64 + 0.5) * s.fy) as usize).min(info.height - 1);
                    let got = data[y * info.width + x];
                    let (want, tol) = if spec.kind == ExportKind::Pca {
                        if !s.valid(spec.t, p) {
                            continue;
                        }
                        let bs = spec.basis.as_ref().ok_or("no basis")?;
                        s.dequant(spec.t, p, &mut v);
                        let (mut sc, mut step) = (0f64, 0f64);
                        for (d, &x) in v.iter().enumerate() {
                            let c = f64::from(bs.comps[band * s.d + d]);
                            sc += f64::from(x - bs.mean[d]) * c;
                            step += c.abs() * f64::from(r.scale[d]) * 0.5;
                        }
                        (sc as f32, step as f32 + 1e-4)
                    } else if spec.kind == ExportKind::Direction {
                        let t0 = spec.t0.ok_or("no year compared with")?;
                        let bs = spec.direction.as_ref().ok_or("no directions")?;
                        if !s.valid(spec.t, p) || !s.valid(t0, p) {
                            continue;
                        }
                        let mut c = vec![0f32; s.d];
                        s.dequant(spec.t, p, &mut v);
                        s.dequant(t0, p, &mut c);
                        let dot: f64 = v.iter().zip(&c).map(|(&a, &b)| f64::from(a) * f64::from(b)).sum();
                        let cos = dot / (f64::from(s.norm(spec.t, p)) * f64::from(s.norm(t0, p)));
                        if 1.0 - cos <= f64::from(bs.threshold) {
                            (f32::NAN, 0.0)
                        } else {
                            let (mut sc, mut step) = (0f64, 0f64);
                            for d in 0..s.d {
                                let w = f64::from(bs.comps[band * s.d + d]);
                                sc += (f64::from(v[d] - c[d]) - f64::from(bs.mean[d])) * w;
                                step += w.abs() * f64::from(r.scale[d]);
                            }
                            (sc as f32, step as f32 + 1e-4)
                        }
                    } else if spec.kind == ExportKind::Classes {
                        let img = &e.img[spec.t];
                        if band == 0 {
                            let k = img.classes[p];
                            (if k == janus_embed::classify::NONE { f32::NAN } else { f32::from(k) + 1.0 }, 0.0)
                        } else {
                            (img.values[p], 0.03)
                        }
                    } else {
                        (e.img[spec.t].values[p], 0.02)
                    };
                    if want.is_finite() != got.is_finite() {
                        if lenient {
                            misses += 1;
                            continue;
                        }
                        return Err("the GeoTIFF's missing values should be the image's".into());
                    }
                    if !want.is_finite() {
                        blank += 1;
                        continue;
                    }
                    if lenient && (got - want).abs() > tol {
                        misses += 1;
                        continue;
                    }
                    worst = worst.max(f64::from((got - want).abs()));
                    bound = bound.max(f64::from(tol));
                    if (got - want).abs() > tol {
                        return Err(format!(
                            "band {} at ({x}, {y}): {got:.5} in the GeoTIFF, {want:.5} shown",
                            band + 1
                        ));
                    }
                    n += 1;
                }
            }
        }
        // a layer that hardly changed: its direction is missing almost everywhere, as it should be
        if n == 0 && !(lenient && blank > 0) {
            return Err("no value of the GeoTIFF could be compared".into());
        }
        let looked = n + misses + blank;
        if misses * 20 > looked {
            return Err(format!("{misses} of {looked} values of the GeoTIFF differ from the image's"));
        }
        if misses > 0 {
            println!("    {misses} of {looked} values on the other side of the threshold (or of a tie)");
        }
        let vectors = 256 * if pair { 2 } else { 1 }; // looked at
        if clipped > vectors / 10 {
            return Err("the store's ranges should hold the vectors of every year".into());
        }
        let name = std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        println!(
            "    {name}: {} x {} px, {k} band(s), {n} values within {worst:.4} of the image (at most {bound:.4}), {blank} missing in both; {clipped} clipped vectors left out",
            info.width, info.height
        );
        Ok(())
    }

    fn embeddings_test_stage(&mut self, st: &mut EmbeddingsTest, ctx: &egui::Context) -> Option<i32> {
        let t0 = *st.t0.get_or_insert_with(Instant::now);
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        if st.since.get_or_insert_with(Instant::now).elapsed().as_secs_f64() > 180.0 {
            return fail("timeout");
        }
        if let Some(e) = self.error_popup.clone() {
            return fail(&format!("open: {e}"));
        }
        let next = |st: &mut EmbeddingsTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
            st.frames = 0;
        };
        st.frames += 1;
        let li = self.embedding_layer();
        let lt = li.map_or(0, |i| self.layer_date(i));
        // the image of the date shown is up to date
        let current = |s: &State| {
            li.is_some_and(|i| {
                let e = s.layers[i].emb.as_ref().unwrap();
                e.img.get(lt).is_some_and(|img| img.has_texture() && img.key == s.embedding_key(i, lt))
            })
        };
        // a view written as a GeoTIFF of the whole layer
        let export = |s: &mut State, st: &mut EmbeddingsTest, name: &str| -> Result<(), String> {
            let spec = s.embedding_export_spec(true)?;
            let path = st.out.join(name).to_string_lossy().into_owned();
            let job = s.export_embeddings(path, s.embedding_export_spec(true)?, ctx);
            st.job = Some((job, spec));
            Ok(())
        };
        // -1 waiting, 0 done, 1 failed
        let exported = |s: &State, st: &mut EmbeddingsTest| -> i32 {
            let Some((job, _)) = &st.job else { return 0 };
            if job.state() == ExportState::Running {
                return -1;
            }
            let (job, spec) = st.job.take().unwrap();
            if job.state() != ExportState::Done {
                println!("    {}: {}", job.title, job.error());
                return 1;
            }
            match s.check_tif(&job.path, &spec) {
                Ok(()) => 0,
                Err(e) => {
                    println!("    {e}");
                    1
                }
            }
        };
        match st.stage {
            0 => {
                st.out = self
                    .opts
                    .selftest_embeddings_out
                    .clone()
                    .unwrap_or_else(|| crate::settings::app_data_dir().join("selftest_embeddings"));
                let _ = std::fs::create_dir_all(&st.out);
                println!("SIMD kernels: {}", janus_embed::simd::name());
                st.stage = 1;
            }
            1 => {
                // another kind of series: the embeddings of its visible area are downloaded and added on top, as the
                // Layers panel does
                if li.is_none() && self.layers.len() == 1 && self.emb_downloads.is_empty() {
                    if self.opening.is_some() || !self.session.as_ref()?.s.overview.complete() || st.frames < 3 {
                        return None;
                    }
                    // the ROI given (--roi), else the visible area
                    let area = if self.opts.roi.is_some() {
                        self.series.roi_rect?;
                        1
                    } else {
                        0
                    };
                    self.emb_download = crate::emb_download::DownloadUi { source: 0, y0: 2023, y1: 2024, res: 0, area };
                    let (ll, wm, hm) = match self.embedding_bounds(area) {
                        Ok(b) => b,
                        Err(e) => return fail(&format!("bounds: {e}")),
                    };
                    println!(
                        "    {}: downloading AlphaEarth 2023-2024 for {:.4} {:.4} {:.4} {:.4} ({:.2} x {:.2} km)",
                        self.layers[0].name,
                        ll[0],
                        ll[1],
                        ll[2],
                        ll[3],
                        wm / 1000.0,
                        hm / 1000.0
                    );
                    self.start_embedding_download(ctx);
                    if self.emb_downloads.is_empty() {
                        return fail("the download did not start");
                    }
                    st.stage = 100;
                    st.since = Some(Instant::now());
                    return None;
                }
                let i = li?;
                let e = self.layers[i].emb.as_ref()?;
                let s = e.store.clone()?;
                if s.failed() {
                    return fail(&format!("store: {}", s.error()));
                }
                // the basis of every year (the first one may come from the years read first)
                if !s.complete()
                    || e.basis.as_ref().is_none_or(|b| b.years != s.t_n)
                    || e.basis_have != e.basis_sent
                    || e.busy()
                    || !current(self)
                    || st.frames < 3
                {
                    return None;
                }
                let b = e.basis.clone()?;
                println!(
                    "    {}: {} years, store {} x {} px (1:{:.2}), {:.0} MB, read in {:.2} s",
                    e.meta.label(),
                    s.t_n,
                    s.w,
                    s.h,
                    s.fx,
                    s.bytes() as f64 / 1048576.0,
                    s.seconds()
                );
                if b.samples < 2 {
                    return fail("no valid vectors in the layer");
                }
                let pct = |c: usize| 100.0 * b.eig[c] / b.total;
                println!(
                    "    global PCA: {} vectors in {:.1} ms (covariance {:.2}, eigen {:.2} ms, {} it.); PC1-3 {:.1}% {:.1}% {:.1}%; image {:.1} ms",
                    b.samples,
                    b.ms,
                    b.cov_ms,
                    b.eig_ms,
                    b.iterations,
                    pct(0),
                    pct(1),
                    pct(2),
                    e.img[lt].ms
                );
                // the same sample, the plain way: mean and covariance in double, the eigenvalues
                let sample = sample_vectors(&s, None, 65536, true, 0, None);
                let d = s.d;
                let (mut mean, mut c, mut v) = (vec![0f64; d], vec![0f64; d * d], vec![0f32; d]);
                for &(t, p) in &sample {
                    s.dequant(t, p as usize, &mut v);
                    for k in 0..d {
                        mean[k] += f64::from(v[k]);
                    }
                }
                for m in mean.iter_mut() {
                    *m /= sample.len() as f64;
                }
                for &(t, p) in &sample {
                    s.dequant(t, p as usize, &mut v);
                    for r in 0..d {
                        for q in r..d {
                            c[r * d + q] += (f64::from(v[r]) - mean[r]) * (f64::from(v[q]) - mean[q]);
                        }
                    }
                }
                let mut tr = 0.0;
                for r in 0..d {
                    for q in r..d {
                        c[r * d + q] /= (sample.len() - 1) as f64;
                        c[q * d + r] = c[r * d + q];
                    }
                    tr += c[r * d + r];
                }
                let (_, vals, _) = janus_embed::eigen::top_eigen(&c, d, 3);
                let dm = (0..d).map(|k| (mean[k] - f64::from(b.mean[k])).abs()).fold(0.0, f64::max);
                println!(
                    "    plain PCA of the same sample: PC1-3 {:.1}% {:.1}% {:.1}% (total {:.4} vs {:.4}, max mean diff {dm:.2e})",
                    100.0 * vals[0] / tr,
                    100.0 * vals[1] / tr,
                    100.0 * vals[2] / tr,
                    tr,
                    b.total
                );
                if (0..3).any(|k| (vals[k] - b.eig[k]).abs() > 1e-4 * vals[0]) {
                    return fail("the PCA differs from the plain one");
                }
                if (tr - b.total).abs() > 1e-4 * tr {
                    return fail("the total variance differs from the plain one");
                }
                let colours = self.map_colours();
                println!("    {colours} distinct colours on a 23 x 23 grid of the map");
                if colours < 40 {
                    return fail("the principal components should colour the map");
                }
                if let Err(e) = export(self, st, "pca.tif") {
                    return fail(&e);
                }
                next(st, "principal components of every year");
            }
            2 => {
                match exported(self, st) {
                    -1 => return None,
                    1 => return fail("export"),
                    _ => {}
                }
                // ROI: the central ninth of the image; local PCA
                let info = self.session.as_ref()?.info.clone();
                let (w, h) = (info.width as i64, info.height as i64);
                self.set_roi_rect([w / 3, h / 3, 2 * w / 3, 2 * h / 3]);
                self.layers[li?].emb.as_mut()?.scope = 1;
                next(st, "pca.tif");
            }
            3 => {
                let e = self.layers[li?].emb.as_ref()?;
                if !e.basis_local || !current(self) || st.frames < 3 {
                    return None;
                }
                let b = e.basis.clone()?;
                let pct = |c: usize| 100.0 * b.eig[c] / b.total;
                println!(
                    "    local PCA: {} vectors in {:.1} ms; PC1-3 {:.1}% {:.1}% {:.1}%",
                    b.samples,
                    b.ms,
                    pct(0),
                    pct(1),
                    pct(2)
                );
                if b.samples == 0 {
                    return fail("no vectors in the ROI");
                }
                if let Err(e) = export(self, st, "pca_roi.tif") {
                    return fail(&e);
                }
                next(st, "local PCA of the ROI");
            }
            4 => {
                match exported(self, st) {
                    -1 => return None,
                    1 => return fail("export"),
                    _ => {}
                }
                let info = self.session.as_ref()?.info.clone();
                self.add_pin(info.width / 2, info.height / 2);
                let id = self.series.pins.last()?.id;
                let e = self.layers[li?].emb.as_mut()?;
                (e.view, e.ref_kind, e.ref_pin) = (View::Similarity, 1, id);
                e.reset_reference();
                self.map.dirty = true;
                next(st, "pca_roi.tif");
            }
            5 => {
                if !current(self) || st.frames < 3 {
                    return None;
                }
                let pin = self.series.pins.last()?.clone();
                let i = li?;
                let Some(p) = self.embedding_pixel(i, pin.x as f64 + 0.5, pin.y as f64 + 0.5) else {
                    return fail("pin off the store");
                };
                let img = &self.layers[i].emb.as_ref()?.img[lt];
                let me = img.values[p];
                println!(
                    "    similarity: pin {} -> {me:.6} at itself, range {:.3} .. {:.3}, image {:.1} ms",
                    pin.id, img.lo, img.hi, img.ms
                );
                if me.is_nan() || me <= 0.999 {
                    return fail("the similarity of the reference to itself should be 1");
                }
                if let Err(e) = export(self, st, "similarity.tif") {
                    return fail(&e);
                }
                next(st, "similarity to a pin");
            }
            6 => {
                match exported(self, st) {
                    -1 => return None,
                    1 => return fail("export"),
                    _ => {}
                }
                let i = li?;
                if self.layers[i].emb.as_ref()?.info.t() < 2 {
                    // one year: no change to show
                    next(st, "similarity.tif");
                    st.stage = 8;
                    return None;
                }
                self.layers[i].emb.as_mut()?.view = View::Change;
                let n = self.session.as_ref()?.info.t();
                self.set_date(n - 1);
                self.map.dirty = true;
                next(st, "similarity.tif");
            }
            7 => {
                if !current(self) || st.frames < 3 {
                    return None;
                }
                let e = self.layers[li?].emb.as_ref()?;
                let img = &e.img[lt];
                let mut n = 0;
                for &x in img.values.iter().filter(|x| x.is_finite()) {
                    if !(-1e-4..=2.0001).contains(&x) {
                        return fail("a cosine distance outside [0, 2]");
                    }
                    n += 1;
                }
                println!(
                    "    change {}: {n} pixels, range {:.3} .. {:.3}, image {:.1} ms",
                    e.info.layers[lt].label, img.lo, img.hi, img.ms
                );
                if n == 0 {
                    return fail("no change values");
                }
                if let Err(e) = export(self, st, "change.tif") {
                    return fail(&e);
                }
                next(st, "change from the previous year");
            }
            8 => {
                match exported(self, st) {
                    -1 => return None,
                    1 => return fail("export"),
                    _ => {}
                }
                // the direction of change, of the year shown from the previous one (one year: on to the classes)
                if self.layers[li?].emb.as_ref()?.info.t() < 2 {
                    next(st, "change.tif");
                    st.stage = 10;
                    return None;
                }
                self.layers[li?].emb.as_mut()?.view = View::Direction;
                self.map.dirty = true;
                next(st, "change.tif");
            }
            9 => {
                let i = li?;
                let e = self.layers[i].emb.as_ref()?;
                if e.dir_basis.is_none() || e.busy() || !current(self) || st.frames < 3 {
                    return None;
                }
                let b = e.dir_basis.clone()?;
                let pct = |c: usize| 100.0 * b.eig.get(c).copied().unwrap_or(0.0) / b.total.max(1e-30);
                println!(
                    "    direction: {} changed vectors of {} pairs ({:.1}% above {:.3}) in {:.1} ms; PC1-3 {:.1}% {:.1}% {:.1}%; image {:.1} ms",
                    b.samples,
                    b.pairs,
                    100.0 * b.changed,
                    b.threshold,
                    b.ms,
                    pct(0),
                    pct(1),
                    pct(2),
                    e.img[lt].ms
                );
                if b.samples < 3 {
                    // a layer that does not change: nothing to draw or write
                    println!("    no pixel changed above the threshold: no direction to draw");
                    next(st, "direction of change");
                    st.stage = 10;
                    return None;
                }
                if let Err(e) = self.save_map_png(&st.out.join("direction.png")) {
                    return fail(&e);
                }
                // the changes are the exception: a few colours over the grey (where 1% of the pixels changed)
                let colours = self.map_colours();
                println!("    {colours} distinct colours on a 23 x 23 grid of the map");
                if b.changed >= 0.01 && colours < 3 {
                    return fail("the directions of change should colour the map");
                }
                if let Err(e) = export(self, st, "direction.tif") {
                    return fail(&e);
                }
                next(st, "direction of change");
            }
            10 => {
                match exported(self, st) {
                    -1 => return None,
                    1 => return fail("export"),
                    _ => {}
                }
                // three classes from three pins of the year shown (where the principal components differ most)
                let info = self.session.as_ref()?.info.clone();
                let (w, h) = (info.width, info.height);
                self.clear_pins();
                for (x, y) in [(w / 5, h / 5), (4 * w / 5, h / 2), (w / 2, 4 * h / 5)] {
                    self.add_pin(x, y);
                }
                let i = li?;
                let ids: Vec<u32> = self.series.pins.iter().map(|p| p.id).collect();
                self.layers[i].emb.as_mut()?.view = View::Classes;
                for id in ids {
                    let Some((v, src)) = self.class_samples(i, crate::embeddings::ClassSource::Pin(id)) else {
                        return fail("a pin without a vector");
                    };
                    let e = self.layers[i].emb.as_mut()?;
                    let k = e.classes.len();
                    e.classes.push(crate::embeddings::EmbClass {
                        name: format!("pin {id}"),
                        color: [egui::Color32::RED, egui::Color32::GREEN, egui::Color32::BLUE][k % 3],
                        samples: v,
                        sources: vec![src],
                    });
                    e.cls_gen += 1;
                }
                self.map.dirty = true;
                next(st, "direction.tif");
            }
            11 => {
                let i = li?;
                let e = self.layers[i].emb.as_ref()?;
                if e.classifier.is_none() || e.busy() || !current(self) || st.frames < 3 {
                    if !e.cls_error.is_empty() && !e.busy() {
                        return fail(&format!("classifier: {}", e.cls_error));
                    }
                    return None;
                }
                let c = e.classifier.clone()?;
                let img = &e.img[lt];
                let total: usize = img.counts.iter().sum();
                let shares: Vec<String> =
                    img.counts.iter().map(|&n| format!("{:.1}%", 100.0 * n as f64 / total.max(1) as f64)).collect();
                println!(
                    "    classes: fitted in {:.2} ms ({} iterations), {:.0}% of the samples right; shares {}; image {:.1} ms",
                    c.ms,
                    c.iterations,
                    100.0 * c.train_acc,
                    shares.join(" "),
                    img.ms
                );
                // each pin's pixel in its own class
                for (k, pin) in self.series.pins.clone().iter().enumerate() {
                    let p = self.embedding_pixel(i, pin.x as f64 + 0.5, pin.y as f64 + 0.5)?;
                    let got = self.layers[i].emb.as_ref()?.img[lt].classes[p];
                    if usize::from(got) != k {
                        return fail(&format!("pin {} classified as {got}, not as its own class {k}", pin.id));
                    }
                }
                if img.counts.iter().take(3).filter(|&&n| n > 0).count() < 3 {
                    return fail("every class should have pixels");
                }
                if let Err(e) = self.save_map_png(&st.out.join("classes.png")) {
                    return fail(&e);
                }
                if let Err(e) = export(self, st, "classes.tif") {
                    return fail(&e);
                }
                next(st, "classes from three pins");
            }
            12 => {
                match exported(self, st) {
                    -1 => return None,
                    1 => return fail("export"),
                    _ => {}
                }
                // every year in the principal components, as when playing: time per image
                let e = self.layers[li?].emb.as_mut()?;
                (e.view, e.scope) = (View::Pca, 0);
                self.set_date(0);
                self.map.dirty = true;
                next(st, "classes.tif");
            }
            100 => {
                // the download: its progress, then the layer on top, the series still active
                if let Some(d) = self.emb_downloads.last() {
                    let s = d.status.lock().unwrap();
                    if s.state == crate::zeit::JobState::Failed {
                        return fail(&format!("download: {}", s.error));
                    }
                    if s.state == crate::zeit::JobState::Running && !s.message.is_empty() && s.message != st.shown {
                        st.shown = s.message.clone();
                        println!("    {:3.0}%  {}", s.progress * 100.0, s.message);
                    }
                }
                let i = li?;
                if self.opening.is_some() || self.layers[i].emb.as_ref()?.store.is_none() {
                    return None;
                }
                if self.layers.len() != 2 || self.active != 0 || self.layers[0].emb.is_some() {
                    return fail("the series should stay active, the embeddings on top");
                }
                let l = &self.layers[i];
                let how = l
                    .place
                    .reproj
                    .as_ref()
                    .map_or("same grid".to_string(), |(r, _)| format!("reprojected from {}", r.crs_text));
                println!("    added: {}, {}; {how}", l.name, l.emb.as_ref()?.meta.label());
                if !l.place.aligned {
                    return fail("the embeddings could not be placed on the series");
                }
                // the views' checks below are made on the embeddings as the active layer
                self.set_active(i, ctx);
                st.stage = 1;
                st.since = Some(Instant::now());
            }
            _ => {
                if !current(self) {
                    return None;
                }
                let n = self.session.as_ref()?.info.t();
                if self.display.t + 1 < n {
                    self.set_date(self.display.t + 1);
                    return None;
                }
                let e = self.layers[li?].emb.as_ref()?;
                let ms: f64 = e.img.iter().map(|i| i.ms).sum::<f64>() / e.img.len() as f64;
                println!("    every year in principal components: {ms:.1} ms per image");
                println!("OK ({:.1} s); files in {}", t0.elapsed().as_secs_f64(), st.out.display());
                return Some(0);
            }
        }
        None
    }
}
