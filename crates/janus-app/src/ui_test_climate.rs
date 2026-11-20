//! `--selftest-ui` stages 365-369: File > Climate data... (climate.rs), offline. 365 writes a fake CHIRPS folder of three
//! tiny months (a value says its month and pixel) and opens the window on a box; 366 checks it is drawn (the credit
//! included) and presses Download; 367 waits for the job and the layer (three dates, named for the rain, the series
//! studied still active) and reads the files' values; 368 downloads into a folder with two of the months (kept; only the
//! third is read) and checks the rain is drawn as bars in the chart; 369 puts everything back. Then 370 (ui_test_topocorr.rs), 385 (ui_test_calc_detach.rs), then 299.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::climate::{self, Source, Spec};
use crate::zeit::JobState;

/// What the stages share.
#[derive(Default)]
struct Kept {
    dir: PathBuf,
    layers: usize,
    active: usize,
}

static KEPT: Mutex<Option<Kept>> = Mutex::new(None);

fn with_kept<R>(f: impl FnOnce(&mut Kept) -> R) -> R {
    let mut g = KEPT.lock().unwrap();
    f(g.get_or_insert_with(Kept::default))
}

const BOX: [f64; 4] = [-63.0, -10.0, -62.0, -9.0];
const NAME: &str = "CHIRPS rainfall (mm/month)";

/// A fake month: 24 x 24 cells of 0.05 degrees, north-west corner (-63.1, -8.9); a value is 1000 x month + column +
/// 100 x row.
fn write_month(path: &std::path::Path, month: u32) -> Result<(), String> {
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let (w, h) = (24usize, 24usize);
    let mut ds = drv.create_with_band_type::<f32, _>(path, w, h, 1).map_err(|e| e.to_string())?;
    ds.set_geo_transform(&[-63.1, 0.05, 0.0, -8.9, 0.0, -0.05]).map_err(|e| e.to_string())?;
    let srs = gdal::spatial_ref::SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    ds.set_spatial_ref(&srs).map_err(|e| e.to_string())?;
    let data: Vec<f32> = (0..w * h).map(|p| (1000 * month as usize + p % w + 100 * (p / w)) as f32).collect();
    ds.rasterband(1)
        .and_then(|mut b| b.write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), data)))
        .map_err(|e| e.to_string())
}

impl State {
    pub(super) fn test_climate(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
            st.frames = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        match stage {
            365 => {
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-climate-{stamp}"));
                let _ = std::fs::remove_dir_all(&dir);
                let cogs = dir.join("served").join("monthly").join("global").join("cogs");
                if let Err(e) = std::fs::create_dir_all(&cogs) {
                    return fail(&format!("{}: {e}", cogs.display()));
                }
                for m in 1..=3u32 {
                    if let Err(e) = write_month(&cogs.join(format!("chirps-v3.0.2020.{m:02}.cog")), m) {
                        return fail(&format!("the fake month could not be written: {e}"));
                    }
                }
                climate::set_root_override(Some(dir.join("served").to_string_lossy().into_owned()));
                with_kept(|k| *k = Kept { dir: dir.clone(), layers: self.layers.len(), active: self.active });
                self.open_climate();
                if !self.climate.show {
                    return fail("File > Climate data... should open the window");
                }
                let c = &mut self.climate;
                (c.source, c.area, c.bbox) = (Source::Monthly, 2, BOX);
                (c.start, c.end) = ((2020, 1), (2020, 3));
                c.folder = dir.join("out").to_string_lossy().into_owned();
                next(
                    st,
                    "File > Climate data...: the window opened on a box, 2020-01 to 2020-03, a fake source of three months",
                );
            }
            366 => {
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                if ctx.memory(|m| m.area_rect(egui::Id::new("climate window"))).is_none() {
                    return fail("the Climate data window should be drawn");
                }
                let dir = with_kept(|k| k.dir.clone());
                let spec = Spec {
                    source: Source::Monthly,
                    bbox: BOX,
                    start: (2020, 1),
                    end: (2020, 3),
                    out_dir: dir.join("out"),
                };
                match climate::estimate(&spec) {
                    Ok(e) if e.files == 3 && (e.cols, e.rows) == (20, 20) => {
                        println!("    estimate: {} files of {} x {} px", e.files, e.cols, e.rows);
                    }
                    other => return fail(&format!("the estimate should be 3 files of 20 x 20: {other:?}")),
                }
                self.start_climate_download(ctx);
                if self.climate.last.is_none() {
                    return fail("Download should start a job");
                }
                next(st, "Download pressed: the job started");
            }
            367 => {
                let n = self.layers.len();
                let before = with_kept(|k| k.layers);
                if let Some(s) = &self.climate.last {
                    let s = s.lock().unwrap();
                    if s.state == JobState::Failed {
                        return fail(&format!("the download failed: {}", s.error));
                    }
                }
                if n <= before || self.opening.is_some() {
                    return if waited > 60.0 { fail("the rain's layer should open") } else { None };
                }
                let Some(k) = self.layers.iter().position(|l| l.name == NAME) else {
                    return fail(&format!("a layer named {NAME:?} should be open"));
                };
                if !super::loaded(self.layer_session(k)) {
                    return if waited > 60.0 { fail("the rain's layer should load") } else { None };
                }
                let info = self.layer_session(k)?.info.clone();
                if info.t() != 3 || !info.time_is_date {
                    return fail(&format!("the layer should have 3 dates, it has {}", info.t()));
                }
                let days: Vec<i64> = info.layers.iter().map(|l| (l.time / 86400.0).round() as i64).collect();
                let want: Vec<i64> = (1..=3).map(|m| janus_cube::dates::days_from_civil(2020, m, 1)).collect();
                if days != want {
                    return fail(&format!("the dates should be 2020-01-01, -02-01, -03-01: {days:?}"));
                }
                if before > 0 && self.active != with_kept(|k| k.active) {
                    return fail("the series being studied should stay active");
                }
                // the files: the box's cells (fake columns 2.., rows 2..), the credit in their metadata
                let dir = with_kept(|k| k.dir.clone()).join("out");
                for m in 1..=3u32 {
                    let path = dir.join(format!("chirps_2020-{m:02}.tif"));
                    let ds = gdal::Dataset::open(&path).map_err(|e| e.to_string()).ok()?;
                    if ds.raster_size() != (20, 20) {
                        return fail(&format!("{}: {:?} should be 20 x 20", path.display(), ds.raster_size()));
                    }
                    let b = ds.rasterband(1).ok()?;
                    let v = b.read_as::<f32>((0, 0), (20, 20), (20, 20), None).ok()?;
                    let (v, w) = (v.data(), 20);
                    let want = |c: usize, r: usize| (1000 * m as usize + (c + 2) + 100 * (r + 2)) as f32;
                    if v[0] != want(0, 0) || v[5 + 7 * w] != want(5, 7) || v[19 + 19 * w] != want(19, 19) {
                        return fail(&format!("{}: the values should be the fake month's cells", path.display()));
                    }
                    let gt = ds.geo_transform().ok()?;
                    if (gt[0] + 63.0).abs() > 1e-9 || (gt[3] + 9.0).abs() > 1e-9 {
                        return fail(&format!("{}: the origin should be -63, -9: {gt:?}", path.display()));
                    }
                    use gdal::Metadata;
                    if ds.metadata_item("DATA_DOI", "").as_deref() != Some(climate::DATA_DOI) {
                        return fail("the files should carry the repository's DOI");
                    }
                }
                if !dir.join("ATTRIBUTION.txt").exists() {
                    return fail("ATTRIBUTION.txt should be beside the files");
                }
                println!("    \"{NAME}\": 3 dates, the box's 20 x 20 cells of every month read as the source has them");
                next(
                    st,
                    "the download wrote three months, the layer opened with 3 dates, the series studied still active",
                );
            }
            368 => {
                let dir = with_kept(|k| k.dir.clone());
                let spec = Spec {
                    source: Source::Monthly,
                    bbox: BOX,
                    start: (2020, 1),
                    end: (2020, 3),
                    out_dir: dir.join("out"),
                };
                // a folder with two of the months (the layer holds the first folder's files open): only the third is read
                let again = dir.join("again");
                if let Err(e) = std::fs::create_dir_all(&again) {
                    return fail(&format!("{}: {e}", again.display()));
                }
                for m in [1, 3] {
                    let name = format!("chirps_2020-{m:02}.tif");
                    if let Err(e) = std::fs::copy(dir.join("out").join(&name), again.join(&name)) {
                        return fail(&format!("{name}: {e}"));
                    }
                }
                let spec = Spec { out_dir: again, ..spec };
                match climate::download(&spec, &|_, _| {}, &AtomicBool::new(false)) {
                    Ok(o) if (o.fetched, o.kept, o.failed.len()) == (1, 2, 0) => {
                        println!("    a folder with two months: 1 read, 2 kept");
                    }
                    other => {
                        return fail(&format!(
                            "downloading into a folder with two of the months should read the third only: {other:?}"
                        ));
                    }
                }
                if !self.chart.looks.get(NAME).is_some_and(|l| l.bars) {
                    return fail("the rain should be drawn as bars in the chart");
                }
                next(st, "the months written are kept; the rain is drawn as bars");
            }
            369 => {
                let (dir, before) = with_kept(|k| (k.dir.clone(), k.layers));
                for i in (before..self.layers.len()).rev() {
                    self.remove_layer(i, ctx);
                }
                self.chart.looks.remove(NAME);
                self.climate.show = false;
                self.climate.last = None;
                self.climate.folder.clear();
                climate::set_root_override(None);
                let _ = std::fs::remove_dir_all(&dir);
                *KEPT.lock().unwrap() = None;
                next(st, "Climate data: everything put back");
                st.stage = 370; // the topographic correction (370-374, ui_test_topocorr.rs), then 385 and 299
            }
            _ => {}
        }
        None
    }
}
