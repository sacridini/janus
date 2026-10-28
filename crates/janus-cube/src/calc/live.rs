//! The calculator live: its result over the whole grid at a reduced size (as the map's overview shows it), a
//! reduction over time updated as its dates are read ("max so far, 23 of 107 dates") instead of after all of them;
//! and a pixel's values at every date (the chart beside the expression).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::compile::{Over, is_periods};
use super::plan::{CalcLayer, Key, Options, Plan, date_vars};
use super::program::{BLOCK, Builder, DateVars, Program};
use super::vrt;

/// What the live result shows now.
pub struct LiveUpdate {
    /// w x h values over the whole grid, NaN where none
    pub data: Vec<f32>,
    pub w: usize,
    pub h: usize,
    /// the dates read of those a reduction needs (both 1 without one)
    pub done: usize,
    pub total: usize,
    /// what it is: "max so far: 23 of 107 dates", the date of a series shown
    pub label: String,
}

/// The values of a live result kept at most (the dates of a reduction, each at the reduced size).
const LIVE_BYTES: usize = 384 << 20;

/// How often a reduction being read is shown again.
const LIVE_EVERY: Duration = Duration::from_millis(250);

/// The reduced size of the grid: its long side `side`, or less when `dates` images of it would not fit.
fn live_size(w: usize, h: usize, side: usize, dates: usize) -> (usize, usize) {
    let mut side = side.min(w.max(h)).max(1);
    while side > 64 && side * side * dates.max(1) * 4 > LIVE_BYTES {
        side = side * 3 / 4;
    }
    if w >= h { (side, (side * h / w).max(1)) } else { ((side * w / h).max(1), side) }
}

/// A VRT's band 1 read whole at `size` (nearest).
fn read_small(path: &str, size: (usize, usize)) -> Result<Vec<f32>, String> {
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
    let (w, h) = ds.raster_size();
    let b = ds.rasterband(1).map_err(|e| e.to_string())?;
    let buf = b.read_as::<f32>((0, 0), (w, h), size, None).map_err(|e| format!("{path}: {e}"))?;
    Ok(buf.data().to_vec())
}

/// The program that is its input 0.
fn identity() -> Program {
    let mut b = Builder::<u8>::default();
    let n = b.input(0);
    b.finish(n)
}

/// The plan of what reduction `j` reduces: its body at each date of its series (None: its inputs read other
/// reductions, or periods).
pub fn body_plan(p: &Plan, j: usize) -> Option<Plan> {
    let st = p.stages.get(j)?;
    if is_periods(st.series) || st.inputs.iter().any(|k| matches!(k, Key::Stage(_))) {
        return None;
    }
    let mut used: Vec<usize> = st
        .inputs
        .iter()
        .filter_map(|k| match k {
            Key::Band { layer, .. } => Some(*layer),
            Key::Stage(_) => None,
        })
        .collect();
    used.sort_unstable();
    used.dedup();
    Some(Plan {
        program: st.body.clone(),
        inputs: st.inputs.clone(),
        stages: Vec::new(),
        series: Some(st.series),
        grid: p.grid,
        name: None,
        used,
        periods: p.periods.clone(),
    })
}

/// The dates of reduction `j` read (its series', within its slice; the one of at()).
pub fn stage_dates(p: &Plan, j: usize, layers: &[CalcLayer]) -> Vec<usize> {
    let st = &p.stages[j];
    let info = p.series_info(layers, st.series);
    match st.only {
        Some(t) => vec![t],
        None => (0..info.t())
            .filter(|&t| st.slice.is_none_or(|s| !info.time_is_date || s.contains(info.layers[t].time)))
            .collect(),
    }
}

/// The result live into `dir` (emptied first), at most `side` px on the grid's long side; a series' date nearest
/// `time` (its first without one). A result of one image whose every input is a reduction over all the dates of a
/// layer is shown as its dates are read; any other once computed. `on` is called with each update.
#[allow(clippy::too_many_arguments)]
pub fn live(
    p: &Plan,
    text: &str,
    layers: &[CalcLayer],
    opts: &Options,
    dir: &Path,
    side: usize,
    time: Option<f64>,
    cancel: &AtomicBool,
    on: &mut dyn FnMut(LiveUpdate),
) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(dir);
    let g = &layers[p.grid].info;
    let progressive = p.series.is_none()
        && !p.stages.is_empty()
        && p.inputs.iter().all(|k| matches!(k, Key::Stage(_)))
        && p.stages.iter().all(|s| matches!(s.over, Over::All) && s.only.is_none())
        && (0..p.stages.len()).all(|j| body_plan(p, j).is_some());
    if !progressive {
        let (w, h) = live_size(g.width, g.height, side, 1);
        let written = p.write(text, layers, opts, dir, &|_, _| {}, cancel)?;
        let dates = p.dates(layers);
        let t = match (time, p.series) {
            (Some(time), Some(_)) => (0..dates.len())
                .min_by(|&a, &b| (dates[a].0 - time).abs().total_cmp(&(dates[b].0 - time).abs()))
                .unwrap_or(0),
            _ => 0,
        };
        let Some(file) = written.files.get(t) else { return Err("no date to show".into()) };
        let data = read_small(file, (w, h))?;
        let label = if p.series.is_some() { format!("the date {}", dates[t].1) } else { String::new() };
        on(LiveUpdate { data, w, h, done: 1, total: 1, label });
        return Ok(());
    }
    // each reduction's body written as a series, its dates read in turn and reduced so far
    let n = p.stages.len();
    let dates: Vec<Vec<usize>> = (0..n).map(|j| stage_dates(p, j, layers)).collect();
    let total: usize = dates.iter().map(Vec::len).sum();
    let (w, h) = live_size(g.width, g.height, side, total);
    let mut files = Vec::with_capacity(n);
    for j in 0..n {
        let bp = body_plan(p, j).ok_or("not a reduction of a layer")?;
        files.push(bp.write(text, layers, opts, &dir.join(format!("body{}", j + 1)), &|_, _| {}, cancel)?.files);
    }
    let id = identity();
    let mut values: Vec<Vec<Vec<f32>>> = vec![Vec::new(); n];
    let mut vars: Vec<Vec<DateVars>> = vec![Vec::new(); n];
    let infos: Vec<_> = p.stages.iter().map(|s| p.series_info(layers, s.series)).collect();
    let mut reduced = vec![vec![f32::NAN; w * h]; n];
    let mut out = vec![f32::NAN; w * h];
    let mut shown = Instant::now();
    let longest = dates.iter().map(Vec::len).max().unwrap_or(0);
    let mut done = 0;
    for d in 0..longest {
        for j in 0..n {
            let Some(&t) = dates[j].get(d) else { continue };
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            values[j].push(read_small(&files[j][t], (w, h))?);
            vars[j].push(date_vars(&infos[j], t));
            done += 1;
        }
        if shown.elapsed() < LIVE_EVERY && d + 1 < longest {
            continue;
        }
        for j in 0..n {
            let all: Vec<&[f32]> = values[j].iter().map(Vec::as_slice).collect();
            vrt::reduce_dates(p.stages[j].f, &id, &all, 0, 1, all.len(), &vars[j], &mut reduced[j]);
        }
        let ins: Vec<&[f32]> = p
            .inputs
            .iter()
            .map(|k| match k {
                Key::Stage(j) => reduced[*j].as_slice(),
                Key::Band { .. } => unreachable!("only reductions"),
            })
            .collect();
        for (k, o) in out.chunks_mut(BLOCK).enumerate() {
            let part: Vec<&[f32]> = ins.iter().map(|s| &s[k * BLOCK..k * BLOCK + o.len()]).collect();
            p.program.eval(&part, &DateVars::default(), o);
        }
        let what = p.stages.iter().map(|s| s.f.name()).collect::<Vec<_>>().join(", ");
        let label = if done < total { format!("{what} so far: {done} of {total} dates") } else { String::new() };
        on(LiveUpdate { data: out.clone(), w, h, done, total, label });
        shown = Instant::now();
    }
    Ok(())
}

/// The value at pixel (x, y) of the grid of each file (a VRT a date): a series at the cursor.
pub fn read_pixel(files: &[String], x: usize, y: usize) -> Vec<f32> {
    files
        .iter()
        .map(|f| {
            gdal::Dataset::open(f)
                .ok()
                .and_then(|ds| {
                    let (w, h) = ds.raster_size();
                    if x >= w || y >= h {
                        return None;
                    }
                    let b = ds.rasterband(1).ok()?;
                    b.read_as::<f32>((x as isize, y as isize), (1, 1), (1, 1), None).ok().map(|v| v.data()[0])
                })
                .unwrap_or(f32::NAN)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(live_size(2000, 1000, 512, 1), (512, 256));
        assert_eq!(live_size(100, 300, 50, 1), (16, 50));
        assert_eq!(live_size(100, 300, 512, 1), (100, 300), "no larger than the grid");
        // many dates: smaller
        let (w, h) = live_size(4000, 4000, 1024, 400);
        assert!(w * h * 400 * 4 <= LIVE_BYTES && w < 1024 && w == h);
    }
}
