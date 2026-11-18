//! `--selftest-ui` stages 345-349: the Welcome window (welcome.rs), offline. 345 writes a sample (a small GeoTIFF) and
//! a manifest of two entries, one with a wrong checksum, in a folder of its own (the program's manifest and samples'
//! folder are replaced for the test) and opens Help > Welcome; 346 checks the start page's buttons and the manifest
//! read, and presses "Open sample data..."; 347 presses Download and open on both samples; 348 checks that the good one
//! was downloaded, checked, opened as a layer and the window closed, and that the broken one failed with its checksum
//! and left no file; 349 the window again says "Downloaded" for the first, then the "?" button of a Zeit tool's window
//! opens its guide (the address kept, no browser), and everything is put back.

use std::path::PathBuf;
use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::welcome::{self, test_hooks};
use crate::zeit::JobState;

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct WelcomeTest {
    dir: PathBuf,
    layers: usize,
    active: usize,
    /// a stage's phase
    phase: u8,
}

const GOOD: &str = "test-sample";
const BROKEN: &str = "broken-sample";

/// A 32 x 32 GeoTIFF of a gradient, the sample.
fn write_sample(path: &std::path::Path) -> Result<(), String> {
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let (w, h) = (32usize, 32usize);
    let mut ds = drv.create_with_band_type::<f32, _>(path, w, h, 1).map_err(|e| e.to_string())?;
    ds.set_geo_transform(&[-60.0, 0.001, 0.0, -10.0, 0.0, -0.001]).map_err(|e| e.to_string())?;
    let srs = gdal::spatial_ref::SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    ds.set_spatial_ref(&srs).map_err(|e| e.to_string())?;
    let data: Vec<f32> = (0..w * h).map(|p| ((p % w) + (p / w)) as f32 / 64.0).collect();
    ds.rasterband(1)
        .and_then(|mut b| b.write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), data)))
        .map_err(|e| e.to_string())
}

impl State {
    pub(super) fn test_welcome(
        &mut self,
        st: &mut UiTest,
        _input: &dyn Fn(usize) -> Option<String>,
        t0: Instant,
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
            st.since = Some(Instant::now());
            st.frames = 0;
            st.welcome.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let drawn =
            |keys: &[&str]| keys.iter().filter(|k| !test_hooks::drawn(k)).map(|k| k.to_string()).collect::<Vec<_>>();
        match stage {
            345 => {
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-welcome-{stamp}"));
                let _ = std::fs::remove_dir_all(&dir);
                if let Err(e) = std::fs::create_dir_all(dir.join("served")) {
                    return fail(&format!("{}: {e}", dir.display()));
                }
                let tif = dir.join("served").join("sample.tif");
                if let Err(e) = write_sample(&tif) {
                    return fail(&format!("the sample could not be written: {e}"));
                }
                let sha = match std::fs::read(&tif) {
                    Ok(b) => janus_ml::fetch::sha256_hex(&b),
                    Err(e) => return fail(&e.to_string()),
                };
                let entry = |id: &str, sha: &str| {
                    serde_json::json!({
                        "id": id, "title": format!("Test sample {id}"), "description": "Made by the self-test.",
                        "size_mb": 0.01, "guide": "landtrendr",
                        // the file beside the manifest, as a release's assets are
                        "files": [{"url": "sample.tif", "name": "sample.tif", "sha256": sha}],
                        "open": "sample.tif"
                    })
                };
                let manifest = dir.join("served").join("samples.json");
                let text = serde_json::json!([entry(GOOD, &sha), entry(BROKEN, &"0".repeat(64))]);
                if let Err(e) = std::fs::write(&manifest, text.to_string()) {
                    return fail(&e.to_string());
                }
                welcome::set_override(Some(manifest.to_string_lossy().into_owned()), Some(dir.join("samples")));
                test_hooks::start();
                st.welcome = WelcomeTest { dir, layers: self.layers.len(), active: self.active, phase: 0 };
                self.open_welcome();
                next(
                    st,
                    "Help > Welcome: the window opened, with a sample and a manifest of two in a folder of its own",
                );
                None
            }
            346 => {
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                if ctx.memory(|m| m.area_rect(egui::Id::new("welcome window"))).is_none() {
                    return fail("the Welcome window should be drawn");
                }
                let missing = drawn(&[
                    "Open sample data...",
                    "Open folder...",
                    "Search STAC...",
                    "Which method?",
                    "link:guides.html",
                ]);
                if !missing.is_empty() {
                    return fail(&format!("the start page should have these buttons: {missing:?}"));
                }
                // the manifest is fetched when the window opens
                match self.welcome_samples() {
                    None => {
                        return if waited > 20.0 { fail("the list of samples should be read") } else { None };
                    }
                    Some(Err(e)) => return fail(&format!("the list of samples should be read: {e}")),
                    Some(Ok(l)) => {
                        let ids: Vec<&str> = l.iter().map(|s| s.id.as_str()).collect();
                        if ids != [GOOD, BROKEN] || !l[0].files[0].url.ends_with("sample.tif") {
                            return fail(&format!("the list should have the two samples: {ids:?}"));
                        }
                    }
                }
                test_hooks::press("Open sample data...");
                next(
                    st,
                    "the start page: Open sample data..., Open folder..., Search STAC..., Which method?, the guides; the list of samples read",
                );
                None
            }
            347 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                if !self.welcome.samples_view {
                    return fail("Open sample data... should show the list of samples");
                }
                let missing = drawn(&["Back", "guide:test-sample", "download:test-sample", "download:broken-sample"]);
                if !missing.is_empty() {
                    return fail(&format!(
                        "the list should have, for each sample, its guide and Download and open: {missing:?}"
                    ));
                }
                // the broken one first (it fails and the window stays), then the good one (its download closes the
                // window as soon as it opens)
                let tasks = self.zeit.jobs.iter().filter(|j| j.title.starts_with("Sample data: ")).count();
                match st.welcome.phase {
                    0 => {
                        test_hooks::press(&format!("download:{BROKEN}"));
                        st.welcome.phase = 1;
                    }
                    1 => {
                        if tasks < 1 {
                            return if waited > 10.0 { fail("Download and open should start a task") } else { None };
                        }
                        test_hooks::press(&format!("download:{GOOD}"));
                        st.welcome.phase = 2;
                    }
                    _ => {
                        if tasks < 2 {
                            return if waited > 10.0 {
                                fail(&format!(
                                    "Download and open should start a task for each sample, there are {tasks}"
                                ))
                            } else {
                                None
                            };
                        }
                        if tasks != 2 {
                            return fail(&format!(
                                "each download should be one task of Tools > Tasks, there are {tasks}"
                            ));
                        }
                        next(st, "Download and open on both: two tasks in Tools > Tasks");
                    }
                }
                None
            }
            348 => {
                let dir = st.welcome.dir.join("samples");
                let broken = self.welcome_run(BROKEN);
                if let Some((JobState::Failed, e)) = &broken {
                    if !e.contains("checksum") {
                        return fail(&format!("the broken sample should fail on its checksum: {e}"));
                    }
                } else {
                    return if waited > 60.0 {
                        fail(&format!("the broken sample should fail: {broken:?}"))
                    } else {
                        None
                    };
                }
                // the good one: downloaded, opened (its run gone), the window closed
                if self.welcome_run(GOOD).is_some() || self.opening.is_some() || self.layers.len() == st.welcome.layers
                {
                    return if waited > 60.0 {
                        fail(&format!(
                            "the sample should be downloaded and opened as a layer: {:?}",
                            self.welcome_run(GOOD)
                        ))
                    } else {
                        None
                    };
                }
                if self.layers.len() != st.welcome.layers + 1 {
                    return fail("the sample should open as one layer");
                }
                if self.welcome.show {
                    return fail("the window should close once the sample opens");
                }
                let (good_dir, bad_dir) = (dir.join(GOOD), dir.join(BROKEN));
                if !good_dir.join("sample.tif").is_file() || !good_dir.join(".complete").is_file() {
                    return fail("the sample's folder should have the file and the mark of a complete download");
                }
                if bad_dir.join("sample.tif").exists() || bad_dir.join(".complete").exists() {
                    return fail("the file that failed its checksum should have been deleted");
                }
                let name = self.layers.last().map(|l| l.inputs.clone()).unwrap_or_default();
                if name.len() != 1 || !std::path::Path::new(&name[0]).starts_with(&good_dir) {
                    return fail(&format!("the layer should be the downloaded file: {name:?}"));
                }
                println!("    opened {} ({} layers)", name[0], self.layers.len());
                st.welcome.phase = 0;
                next(
                    st,
                    "the good sample downloaded, checked and opened as a layer; the broken one failed on its checksum",
                );
                None
            }
            349 => {
                st.frames += 1;
                match st.welcome.phase {
                    0 => {
                        // the list again: the sample says Downloaded (Open), the broken one can be downloaded again
                        self.open_welcome();
                        self.welcome.samples_view = true;
                        test_hooks::clear_drawn();
                        st.welcome.phase = 1;
                        st.frames = 0;
                    }
                    1 => {
                        if st.frames < 4 {
                            return None;
                        }
                        let missing = drawn(&["open:test-sample", "download:broken-sample"]);
                        if !missing.is_empty() {
                            return fail(&format!("a sample downloaded should say so and offer Open: {missing:?}"));
                        }
                        self.welcome.show = false;
                        // the "?" of a Zeit tool's window
                        let Some(tool) = janus_zeit::manifest().tool("landtrendr") else {
                            return fail("the manifest should have LandTrendr");
                        };
                        self.set_tool_open(tool, true);
                        test_hooks::clear_drawn();
                        st.welcome.phase = 2;
                        st.frames = 0;
                    }
                    2 => {
                        if st.frames < 4 {
                            return None;
                        }
                        if !test_hooks::drawn("guide:landtrendr") {
                            return fail("the tool's window should have a ? button");
                        }
                        test_hooks::press("guide:landtrendr");
                        st.welcome.phase = 3;
                        st.frames = 0;
                    }
                    _ => {
                        if st.frames < 3 {
                            return None;
                        }
                        let urls = test_hooks::urls();
                        if urls != ["https://sacridini.github.io/janus/landtrendr.html"] {
                            return fail(&format!("? should open the tool's guide: {urls:?}"));
                        }
                        // everything put back
                        if let Some(tool) = janus_zeit::manifest().tool("landtrendr") {
                            self.set_tool_open(tool, false);
                        }
                        for k in (st.welcome.layers..self.layers.len()).rev() {
                            self.remove_layer(k, ctx);
                        }
                        if st.welcome.active < self.layers.len() && st.welcome.active != self.active {
                            self.set_active(st.welcome.active, ctx);
                        }
                        let dir = st.welcome.dir.clone();
                        self.zeit.jobs.retain(|j| !std::path::Path::new(&j.output_dir).starts_with(&dir));
                        self.welcome.show = false;
                        self.welcome.samples_view = false;
                        test_hooks::stop();
                        welcome::set_override(None, None);
                        let _ = std::fs::remove_dir_all(&dir);
                        st.welcome = Default::default();
                        next(
                            st,
                            "Downloaded shown again; the ? of the LandTrendr window opens its guide (no browser); all put back",
                        );
                        st.stage = 299;
                    }
                }
                None
            }
            _ => None,
        }
    }
}
