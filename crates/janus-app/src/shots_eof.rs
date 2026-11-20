//! The Lagged correlation and EOF guide's screenshots (`--shots eof SERIES OUT`; site/eof.html), on real data: SERIES is
//! a multi-date EVI GeoTIFF (the guide's: 25 years of 16-day EVI over 170 km of western France, EPSG:3035). The tour
//! fetches CHIRPS v3 monthly rainfall for its box through File > Climate data (into `JANUS_SHOTS_CACHE`, else a folder of
//! the temporary files: a month already there is kept, so a second run needs no network), runs the lagged correlation of
//! the EVI with the rain, the EOF without rotation and with it, and takes the pictures the page names. The full-window
//! ones are written as PNG (the guide keeps them as JPEG), the panels and windows as cropped PNG; the maps of the modes
//! come as parts (`parts/`) that tools/make_eof_figures.py puts side by side. What the guide quotes is printed. A pixel
//! for the chart is picked from the correlation's maps (a significant one with the most common lag), or given with
//! `JANUS_EOF_PIN=x,y` and `JANUS_EOF_LAG=k`.

use std::path::PathBuf;

use egui::Context;
use janus_zeit::manifest;
use serde_json::json;

use crate::app::{State, Tab};
use crate::chart_axis::{Lag, Look};
use crate::shots::{Shots, Target, tool_tab};

/// What the tour keeps between its stages.
#[derive(Default)]
pub struct EofShots {
    cache: PathBuf,
    /// the series studied, the rain's layer (name and files)
    series: usize,
    rain: Option<(String, Vec<String>)>,
    /// the pixel of the chart and its best lag
    pin: (usize, usize),
    lag: u32,
    /// the layers before the calculator's corr()
    layers: usize,
    /// the runs' folders
    corr_run: String,
    eof_run: String,
    rot_run: String,
}

/// The layer of a run whose name ends with `suffix`.
fn result_path(s: &State, run: &str, suffix: &str) -> Option<String> {
    s.zeit.results.iter().find(|r| r.run == run && r.name.ends_with(suffix)).map(|r| r.path.clone())
}

fn read_map(path: &str) -> Option<(Vec<f32>, usize)> {
    let ds = gdal::Dataset::open(path).ok()?;
    let (w, h) = ds.raster_size();
    let v = ds.rasterband(1).ok()?.read_as::<f32>((0, 0), (w, h), (w, h), None).ok()?;
    Some((v.data().to_vec(), w))
}

fn quantile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() { f64::NAN } else { v[((v.len() - 1) as f64 * q).round() as usize] }
}

/// The pixel of the chart: significant, at the most common lag of the significant pixels (at least 1 when any is), its
/// correlation the upper quartile of those. Also prints what the guide quotes of the maps.
fn pick_pin(s: &State, run: &str) -> Option<((usize, usize), u32)> {
    let (r, w) = read_map(&result_path(s, run, "Correlation at the best lag")?)?;
    let (lag, _) = read_map(&result_path(s, run, "Best lag")?)?;
    let (sig, _) = read_map(&result_path(s, run, "Significant")?)?;
    let (q, _) = read_map(&result_path(s, run, "False discovery rate q")?)?;
    let (neff, _) = read_map(&result_path(s, run, "Effective pairs at the best lag")?)?;
    let valid: Vec<usize> = (0..r.len()).filter(|&i| r[i].is_finite()).collect();
    let signif: Vec<usize> = valid.iter().copied().filter(|&i| sig[i] > 1.5).collect();
    let mut hist = [0usize; 16];
    for &i in &signif {
        hist[(lag[i].max(0.0) as usize).min(15)] += 1;
    }
    let mut lags: Vec<f64> = signif.iter().map(|&i| f64::from(lag[i])).collect();
    let mut rs: Vec<f64> = signif.iter().map(|&i| f64::from(r[i])).collect();
    let mut all_r: Vec<f64> = valid.iter().map(|&i| f64::from(r[i])).collect();
    let mut ne: Vec<f64> = valid.iter().map(|&i| f64::from(neff[i])).collect();
    println!(
        "    maps: {} pixels with a value of {}, {} significant ({:.1} %); q < 0.1 on {}",
        valid.len(),
        r.len(),
        signif.len(),
        100.0 * signif.len() as f64 / valid.len().max(1) as f64,
        valid.iter().filter(|&&i| q[i] < 0.1).count()
    );
    println!(
        "    significant: median best lag {}, r median {:.3} (5-95 %: {:.3} to {:.3}); all pixels: r median {:.3}; median n_eff {:.1}",
        quantile(&mut lags, 0.5),
        quantile(&mut rs, 0.5),
        quantile(&mut rs, 0.05),
        quantile(&mut rs, 0.95),
        quantile(&mut all_r, 0.5),
        quantile(&mut ne, 0.5)
    );
    println!("    best lag histogram of the significant pixels: {hist:?}");
    let mode = (1..16).max_by_key(|&k| hist[k]).filter(|&k| hist[k] > 0).unwrap_or(0);
    let want: usize = std::env::var("JANUS_EOF_LAG").ok().and_then(|v| v.parse().ok()).unwrap_or(mode);
    let mut cands: Vec<usize> = signif.iter().copied().filter(|&i| lag[i] as usize == want && r[i] > 0.0).collect();
    cands.sort_by(|&a, &b| r[a].total_cmp(&r[b]));
    if let Some(p) = std::env::var("JANUS_EOF_PIN").ok().and_then(|v| {
        let mut it = v.split(',').filter_map(|x| x.trim().parse::<usize>().ok());
        Some((it.next()?, it.next()?))
    }) {
        return Some((p, want as u32));
    }
    // among the upper quartile of those, the one nearest the middle of the image
    let from = cands.len() * 3 / 4;
    let (cx, cy) = (w as f64 / 2.0, (r.len() / w) as f64 / 2.0);
    let &i = cands[from..].iter().min_by(|&&a, &&b| {
        let d = |i: usize| ((i % w) as f64 - cx).powi(2) + ((i / w) as f64 - cy).powi(2);
        d(a).total_cmp(&d(b))
    })?;
    Some(((i % w, i / w), want as u32))
}

impl State {
    pub(crate) fn shots_eof(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL (eof, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        // the download (3) and the runs (5-6, 16-17, 20-21) take as long as they take
        let slow = matches!(sh.stage, 3 | 5 | 6 | 16 | 17 | 20 | 21);
        if t > if slow { 7200.0 } else { 240.0 } {
            return fail(sh, "timeout");
        }
        // (no notice of a task that ended over a window or the chart)
        self.clear_notices();
        let lag_tool = tool_tab("lagged_correlation");
        let eof_tool = tool_tab("eof");
        let Some(lc) = manifest().tool("lagged_correlation") else { return fail(sh, "no lagged correlation tool") };
        let Some(eof) = manifest().tool("eof") else { return fail(sh, "no EOF tool") };
        let tabs = |s: &mut Shots, under: f32| {
            s.under = under;
            s.layout = Some((vec![lag_tool, eof_tool], vec![Tab::Tasks]));
        };
        match sh.stage {
            0 => {
                let cache = std::env::var_os("JANUS_SHOTS_CACHE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::temp_dir().join("janus-eof-shots"));
                let _ = std::fs::remove_dir_all(cache.join("results"));
                let _ = std::fs::create_dir_all(cache.join("results"));
                let _ = std::fs::create_dir_all(sh.out.join("parts"));
                self.zeit.results_dir = Some(cache.join("results"));
                sh.eof.cache = cache;
                let input = sh.input.clone();
                self.shots_open(&input, janus_cube::BandSelection::default(), ctx);
                sh.next("the EVI series opening");
            }
            1 => {
                if self.opening.is_some() || self.session.is_none() {
                    return None;
                }
                tabs(sh, 0.5);
                self.shots_colormap("NDVI");
                let i = self.shots_date_of("2020-07");
                self.set_date(i);
                sh.eof.series = self.active;
                let info = self.session.as_ref()?.info.clone();
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    series: {} x {} px, {} dates, {} to {}, geotransform {:?}",
                    info.width,
                    info.height,
                    info.t(),
                    labels.first().unwrap_or(&""),
                    labels.last().unwrap_or(&""),
                    info.geo_transform
                );
                sh.next("the series open");
            }
            2 => {
                // File > Climate data: the visible area is the whole image
                if sh.frames == 1 {
                    self.map.fit();
                    self.open_climate();
                    self.climate.folder = sh.eof.cache.join("chirps").to_string_lossy().into_owned();
                    return None;
                }
                if sh.frames < 8 || t < 6.0 {
                    return None;
                }
                println!(
                    "    climate: {:?}, area {}, {:?} to {:?}",
                    self.climate.source, self.climate.area, self.climate.start, self.climate.end
                );
                sh.shoot("-", &[("02-climate-window", Target::Area("climate window"))]);
                sh.next("File > Climate data, before the download");
            }
            3 => {
                if sh.frames == 1 {
                    sh.eof.layers = self.layers.len();
                    self.start_climate_download(ctx);
                    if self.climate.last.is_none() {
                        return fail(sh, "Download should start a job");
                    }
                    return None;
                }
                if let Some(s) = &self.climate.last {
                    let s = s.lock().unwrap();
                    if s.state == crate::zeit::JobState::Failed {
                        return fail(sh, &format!("the download failed: {}", s.error));
                    }
                }
                let k = self.layers.iter().position(|l| l.name == "CHIRPS rainfall (mm/month)")?;
                if self.opening.is_some() || !crate::ui_test::loaded(self.layer_session(k)) || !self.shots_quiet() {
                    return None;
                }
                let info = self.layer_session(k)?.info.clone();
                println!("    rain: {} dates, {} x {} cells, {:.1}s", info.t(), info.width, info.height, t);
                // on top of the series on the map would hide it: invisible there, still in the chart
                self.layers[k].opacity = 0.0;
                self.chart
                    .looks
                    .insert(self.layers[k].name.clone(), Look { right: true, bars: true, ..Default::default() });
                sh.eof.rain = Some((self.layers[k].name.clone(), self.layers[k].inputs.clone()));
                self.climate.show = false;
                self.set_active(sh.eof.series, ctx);
                sh.next("the rain downloaded and open as a series, on the right axis as bars");
            }
            4 => {
                // the lagged correlation's window, with the rain as the driver
                if sh.frames == 1 {
                    let Some((name, inputs)) = sh.eof.rain.clone() else { return fail(sh, "no rain layer") };
                    self.shots_open_tool(
                        "lagged_correlation",
                        json!({ "y": { "name": name, "inputs": inputs }, "lags": 4 }),
                    );
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(lag_tool);
                }
                if !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("-", &[("03-lagcorr-window", Shots::fit(lag_tool))]);
                sh.next("Lagged correlation's window");
            }
            5 => match self.shots_run_stage(sh, "lagged_correlation", 0, None, ctx) {
                1 => return fail(sh, &format!("the run did not start: {:?}", self.error_popup)),
                0 => sh.next("the lagged correlation running"),
                _ => {}
            },
            6 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                }
            }
            7 => {
                // the maps read, a pixel picked, the series with the rain
                if sh.frames == 1 {
                    let job = sh.job.and_then(|j| self.zeit.jobs.get(j));
                    let Some(job) = job else { return fail(sh, "no job") };
                    sh.eof.corr_run = job.output_dir.clone();
                    println!(
                        "    lagged correlation: {:.1} s, folder {}",
                        job.status.lock().unwrap().seconds,
                        job.output_dir
                    );
                    let Some((pin, lag)) = pick_pin(self, &sh.eof.corr_run.clone()) else {
                        return fail(sh, "no significant pixel to pin");
                    };
                    println!("    pin ({}, {}), lag {lag}", pin.0, pin.1);
                    (sh.eof.pin, sh.eof.lag) = (pin, lag);
                    self.shots_hide_results();
                    self.zeit.show_tasks = false;
                    self.set_tool_open(lc, false);
                    sh.chart_years = Some((2018.0, 2022.0));
                    let i = self.shots_date_of("2020-07");
                    self.set_date(i);
                    self.clear_pins();
                    self.add_pin(pin.0, pin.1);
                    self.set_chart_layers(1);
                    self.map.fit();
                    return None;
                }
                if !self.shots_quiet() || t < 10.0 {
                    return None;
                }
                sh.shoot("01-series-rain", &[]);
                sh.next("the series with the rain at a pin");
            }
            8..=10 => {
                let (suffix, file) = match sh.stage {
                    8 => ("Correlation at the best lag", "04-best-r"),
                    9 => ("Best lag", "05-best-lag"),
                    _ => ("Significant", "06-significant"),
                };
                if sh.frames == 1 {
                    if !self.shots_show_result(sh, suffix) {
                        return fail(sh, &format!("no result {suffix}"));
                    }
                    return None;
                }
                if !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot(file, &[]);
                sh.next(file);
            }
            11 => {
                // Analysis > Modes of the correlation, with the room it needs
                if sh.frames == 1 {
                    tabs(sh, 0.3);
                    (self.analysis.show, self.analysis.tab, self.analysis.focus) = (true, crate::modes::TAB, true);
                    self.analysis.modes.run = sh.eof.corr_run.clone();
                    return None;
                }
                if sh.frames < 8 || t < 6.0 || self.analysis.modes.drawn < 5 {
                    return None;
                }
                sh.shoot("-", &[("07-modes-lag", Shots::tab(Tab::Analysis))]);
                sh.next("Analysis > Modes for the lagged correlation");
            }
            12 => {
                // the calculator's corr() of the series and the rain at the pin's lag
                if sh.frames == 1 {
                    self.analysis.show = false;
                    tabs(sh, 0.5);
                    self.set_active(sh.eof.series, ctx);
                    let Some((rain, _)) = sh.eof.rain.clone() else { return fail(sh, "no rain layer") };
                    let rain_at = self.layers.iter().position(|l| l.name == rain)?;
                    let (ls, at) = self.calc_layers();
                    let alias = |k: usize| at.iter().position(|&i| i == k).map(|p| ls[p].alias.clone());
                    let (a, b) = (alias(sh.eof.series)?, alias(rain_at)?);
                    sh.eof.layers = self.layers.len();
                    self.calc.show = true;
                    self.calc.text = format!("corr({a}, {b}, lag={}m)", sh.eof.lag);
                    self.calc.primary = None;
                    self.calc.cog = false;
                    self.calc.out_name = "EVI x rain correlation".into();
                    self.calc_run(ctx);
                    return None;
                }
                if self.opening.is_some()
                    || self.layers.len() == sh.eof.layers
                    || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
                {
                    return if t > 300.0 { fail(sh, "the calculator's corr() should arrive") } else { None };
                }
                let k = self.layers.len() - 1;
                println!("    calculator: {:?} -> layer {:?}", self.calc.text, self.layers[k].name);
                self.layers[k].visible = false;
                self.calc.show = false;
                self.set_active(sh.eof.series, ctx);
                let i = self.shots_date_of("2020-07");
                self.set_date(i);
                self.map.fit();
                sh.next("the calculator's corr() is a layer");
            }
            13 => {
                // the chart as it happened
                if sh.frames == 1 {
                    self.shots_hide_results();
                    self.set_chart_layers(1);
                    return None;
                }
                if !self.shots_quiet() || t < 8.0 {
                    return None;
                }
                sh.shoot("-", &[("08-chart-unshifted", Shots::tab(Tab::TimeSeries))]);
                sh.next("the chart: the rain as it happened");
            }
            14 => {
                if sh.frames == 1 {
                    let Some((rain, _)) = sh.eof.rain.clone() else { return fail(sh, "no rain layer") };
                    let l = Lag::Months(sh.eof.lag as i32);
                    self.chart.looks.entry(rain).or_default().shift = Some(l);
                    return None;
                }
                if t < 3.0 {
                    return None;
                }
                sh.shoot("-", &[("09-chart-shifted", Shots::tab(Tab::TimeSeries))]);
                sh.next("the chart: the rain shifted by the lag");
            }
            15 => {
                // back to the rain as it happened; the EOF's window
                if sh.frames == 1 {
                    if let Some((rain, _)) = sh.eof.rain.clone() {
                        self.chart.looks.entry(rain).or_default().shift = None;
                    }
                    self.shots_open_tool("eof", json!({ "n_modes": 10, "maps": 4 }));
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(eof_tool);
                }
                if !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("-", &[("10-eof-window", Shots::fit(eof_tool))]);
                sh.next("the EOF's window");
            }
            16 => match self.shots_run_stage(sh, "eof", 0, None, ctx) {
                1 => return fail(sh, &format!("the EOF did not start: {:?}", self.error_popup)),
                0 => sh.next("the EOF running"),
                _ => {}
            },
            17 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the EOF failed: {}", self.shots_job_error(sh)));
                }
            }
            18 => {
                // the first mode's map over the window
                if sh.frames == 1 {
                    let job = sh.job.and_then(|j| self.zeit.jobs.get(j));
                    let Some(job) = job else { return fail(sh, "no job") };
                    sh.eof.eof_run = job.output_dir.clone();
                    println!("    EOF: {:.1} s, folder {}", job.status.lock().unwrap().seconds, job.output_dir);
                    self.zeit.show_tasks = false;
                    self.set_tool_open(eof, false);
                    self.shots_hide_results();
                    let run = sh.eof.eof_run.clone();
                    if !self.modes_show_map(&run, 1, false) {
                        return fail(sh, "EOF 1's map should be there");
                    }
                    self.set_chart_layers(0);
                    return None;
                }
                if !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("11-eof-maps", &[]);
                sh.next("EOF 1 over the window");
            }
            19 => {
                // Analysis > Modes of the EOF
                if sh.frames == 1 {
                    tabs(sh, 0.01);
                    (self.analysis.show, self.analysis.tab, self.analysis.focus) = (true, crate::modes::TAB, true);
                    self.analysis.modes.run = sh.eof.eof_run.clone();
                    self.analysis.modes.drawn = 0;
                    return None;
                }
                if sh.frames < 8 || t < 5.0 || self.analysis.modes.drawn < 5 {
                    return None;
                }
                sh.shoot("-", &[("12-modes-eof", Shots::tab(Tab::Analysis))]);
                sh.next("Analysis > Modes for the EOF");
            }
            20 => {
                // the chart's overlay of mode 1 at the pin
                if sh.frames == 1 {
                    self.analysis.show = false;
                    tabs(sh, 0.5);
                    self.set_chart_layers(0);
                    return None;
                }
                if !self.shots_quiet() || t < 8.0 {
                    return None;
                }
                sh.shoot("-", &[("13-chart-overlay", Shots::tab(Tab::TimeSeries))]);
                sh.next("the chart with EOF 1 over the pin's anomalies");
            }
            21 => {
                // the run with the first three modes rotated
                if sh.frames == 1 {
                    self.shots_open_tool("eof", json!({ "n_modes": 10, "maps": 4, "rotate": 3 }));
                    return None;
                }
                match self.shots_run_stage(sh, "eof", 0, None, ctx) {
                    1 => return fail(sh, "the rotated EOF did not start"),
                    0 => sh.next("the rotated EOF running"),
                    _ => {}
                }
            }
            22 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the rotated EOF failed: {}", self.shots_job_error(sh)));
                }
            }
            23..=27 => {
                // the maps of the parts (EOF 2 and 3 as they are; the rotated ones)
                if sh.frames == 1 && sh.stage == 23 {
                    let job = sh.job.and_then(|j| self.zeit.jobs.get(j));
                    let Some(job) = job else { return fail(sh, "no job") };
                    sh.eof.rot_run = job.output_dir.clone();
                    println!("    rotated EOF: {:.1} s, folder {}", job.status.lock().unwrap().seconds, job.output_dir);
                    self.zeit.show_tasks = false;
                    self.set_tool_open(eof, false);
                    self.clear_pins();
                    // the series' own colour bar out of the maps' pictures
                    self.layers[sh.eof.series].visible = false;
                    self.map.fit();
                }
                let (id, file) = match sh.stage {
                    23 => ("eof_2", "parts/14a-eof2"),
                    24 => ("eof_3", "parts/14b-eof3"),
                    25 => ("eof_rotated_1", "parts/15a-rot1"),
                    26 => ("eof_rotated_2", "parts/15b-rot2"),
                    _ => ("eof_rotated_3", "parts/15c-rot3"),
                };
                if sh.frames == 2 {
                    let run = sh.eof.rot_run.clone();
                    let Some(path) = self
                        .zeit
                        .modes_runs
                        .iter()
                        .find(|m| m.run == run)
                        .and_then(|m| m.maps.iter().find(|(i, _)| i == id).map(|(_, p)| p.clone()))
                    else {
                        return fail(sh, &format!("the run has no map {id}"));
                    };
                    let mut found = false;
                    for r in &mut self.zeit.results {
                        r.visible = r.path == path;
                        found |= r.visible;
                    }
                    self.map.dirty = true;
                    if !found {
                        return fail(sh, &format!("no layer for {id}"));
                    }
                }
                if sh.frames < 3 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("-", &[(file, Shots::tab(Tab::Map))]);
                sh.next(file);
            }
            _ => {
                println!("    runs: {} | {} | {}", sh.eof.corr_run, sh.eof.eof_run, sh.eof.rot_run);
                println!("OK");
                return Some(0);
            }
        }
        None
    }
}
