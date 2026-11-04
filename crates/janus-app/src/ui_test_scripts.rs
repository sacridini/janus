//! The self-test of the Python history's scripts (scripts.rs): a run of Janus' own tool is in the history with its
//! script, and that script (rerun.py), run by a Python with what it imports, writes the same files as Janus in the
//! run's `python` folder: rasters value for value, GeoPackages feature for feature, CSVs cell for cell.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use gdal::vector::LayerAccess;

use crate::app::State;

/// The modules a script imports at its top level (not those imported only on a path it may not take).
fn imports(code: &str) -> Vec<String> {
    let mut m: Vec<String> = code
        .lines()
        .filter_map(|l| {
            l.strip_prefix("import ").or_else(|| l.strip_prefix("from ").filter(|r| r.contains(" import ")))
        })
        .flat_map(|rest| {
            let head = rest.split(" import ").next().unwrap_or(rest);
            head.split(',')
                .map(|s| s.split_whitespace().next().unwrap_or("").split('.').next().unwrap_or("").to_string())
        })
        .filter(|s| !s.is_empty())
        .collect();
    m.sort();
    m.dedup();
    m
}

/// The first Python that imports `modules`: JANUS_GDAL_PYTHON, JANUS_PYTHON, then `python` (asked once each).
fn python_for(modules: &[String]) -> Option<String> {
    static SEEN: Mutex<Option<HashMap<(String, String), bool>>> = Mutex::new(None);
    let mut candidates: Vec<String> =
        ["JANUS_GDAL_PYTHON", "JANUS_PYTHON"].iter().filter_map(|v| std::env::var(v).ok()).collect();
    candidates.push("python".into());
    candidates.dedup();
    let list = modules.join(", ");
    for p in candidates {
        let key = (p.clone(), list.clone());
        let known = SEEN.lock().unwrap().get_or_insert_with(HashMap::new).get(&key).copied();
        let ok = known.unwrap_or_else(|| {
            let ok = std::process::Command::new(&p)
                .args(["-c", &format!("import {list}")])
                .output()
                .is_ok_and(|o| o.status.success());
            SEEN.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, ok);
            ok
        });
        if ok {
            return Some(p);
        }
    }
    None
}

/// Two numbers alike: equal, both NaN, or within `tol` of the larger (relative; `abs` absolute).
fn alike(a: f64, b: f64, tol: f64, abs: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan()) || (a - b).abs() <= tol * a.abs().max(b.abs()) + abs
}

type Raster = (usize, usize, Vec<f64>, [f64; 6]);

fn read_raster(path: &Path) -> Result<Raster, String> {
    let d = gdal::Dataset::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = d.raster_size();
    let mut v = vec![];
    for b in 1..=d.raster_count() {
        let band = d.rasterband(b).map_err(|e| e.to_string())?;
        v.extend(band.read_as::<f64>((0, 0), (w, h), (w, h), None).map_err(|e| e.to_string())?.data().iter().copied());
    }
    Ok((w, h, v, d.geo_transform().unwrap_or_default()))
}

/// Rasters alike: the same grid, each value within `tol`. The largest difference.
fn same_raster(ours: &Path, theirs: &Path, tol: f64) -> Result<(usize, f64), String> {
    let (a, b) = (read_raster(ours)?, read_raster(theirs)?);
    let grid = (a.0, a.1) == (b.0, b.1) && a.3.iter().zip(&b.3).all(|(x, y)| alike(*x, *y, 1e-12, 0.0));
    if !grid || a.2.len() != b.2.len() {
        return Err(format!("{}: another grid from rerun.py", theirs.display()));
    }
    let mut worst = 0f64;
    let mut differ = 0;
    for (x, y) in a.2.iter().zip(&b.2) {
        if !alike(*x, *y, tol, if tol > 0.0 { tol } else { 0.0 }) {
            differ += 1;
        } else if x.is_finite() && y.is_finite() {
            worst = worst.max((x - y).abs());
        }
    }
    if differ > 0 {
        return Err(format!("{}: {differ} of {} values differ from Janus'", theirs.display(), a.2.len()));
    }
    Ok((a.2.len(), worst))
}

/// A feature: its fields by name (as text; None: null), its area and envelope.
type Feature = (HashMap<String, Option<String>>, f64, [f64; 4]);

fn read_features(path: &Path) -> Result<Vec<Feature>, String> {
    let d = gdal::Dataset::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut l = d.layer(0).map_err(|e| e.to_string())?;
    let names: Vec<String> = l.defn().fields().map(|f| f.name()).collect();
    let mut out = vec![];
    for f in l.features() {
        let mut fields = HashMap::new();
        for (i, n) in names.iter().enumerate() {
            fields.insert(n.clone(), f.field_as_string(i).ok().flatten());
        }
        let (area, env) = f.geometry().map_or((0.0, [0.0; 4]), |g| {
            let e = g.envelope();
            (g.area(), [e.MinX, e.MinY, e.MaxX, e.MaxY])
        });
        out.push((fields, area, env));
    }
    Ok(out)
}

/// GeoPackages alike: as many features, each (by its place: envelope, area) with the same fields, numbers within
/// `tol` (1e-9 at least: areas measured by GDAL and by pyproj).
fn same_features(ours: &Path, theirs: &Path, tol: f64) -> Result<usize, String> {
    let (mut a, mut b) = (read_features(ours)?, read_features(theirs)?);
    if a.len() != b.len() {
        return Err(format!("{}: {} features, Janus {}", theirs.display(), b.len(), a.len()));
    }
    let order = |x: &Feature, y: &Feature| {
        x.2.iter()
            .chain([&x.1])
            .zip(y.2.iter().chain([&y.1]))
            .map(|(p, q)| p.total_cmp(q))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    a.sort_by(order);
    b.sort_by(order);
    let tol = tol.max(1e-9);
    for (k, (x, y)) in a.iter().zip(&b).enumerate() {
        if !alike(x.1, y.1, tol, 0.0) || x.2.iter().zip(&y.2).any(|(p, q)| !alike(*p, *q, 1e-12, 0.0)) {
            return Err(format!("{}: feature {k} has another geometry", theirs.display()));
        }
        for (name, v) in &x.0 {
            let w = y.0.get(name).ok_or_else(|| format!("{}: no column {name}", theirs.display()))?;
            let same = match (v, w) {
                (Some(p), Some(q)) => match (p.parse::<f64>(), q.parse::<f64>()) {
                    (Ok(p), Ok(q)) => alike(p, q, tol, 0.0),
                    _ => p == q,
                },
                (None, None) => true,
                _ => false,
            };
            if !same {
                return Err(format!("{}: feature {k}'s {name} is {w:?}, Janus' {v:?}", theirs.display()));
            }
        }
    }
    Ok(a.len())
}

/// The decimals a number is written with.
fn decimals(s: &str) -> i32 {
    s.split_once('.').map_or(0, |(_, d)| d.len() as i32)
}

/// CSVs alike: the same lines, each cell the same text or a number within `tol` (and the last decimal written).
fn same_csv(ours: &Path, theirs: &Path, tol: f64) -> Result<usize, String> {
    let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let (a, b) = (read(ours)?, read(theirs)?);
    let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    if la.len() != lb.len() {
        return Err(format!("{}: {} lines, Janus {}", theirs.display(), lb.len(), la.len()));
    }
    let mut cells = 0;
    for (k, (x, y)) in la.iter().zip(&lb).enumerate() {
        let (cx, cy): (Vec<&str>, Vec<&str>) = (x.split(',').collect(), y.split(',').collect());
        if cx.len() != cy.len() {
            return Err(format!("{}: line {} has {} cells, Janus' {}", theirs.display(), k + 1, cy.len(), cx.len()));
        }
        for (p, q) in cx.iter().zip(&cy) {
            let same = p == q
                || match (p.parse::<f64>(), q.parse::<f64>()) {
                    (Ok(u), Ok(v)) => {
                        let last = 10f64.powi(-decimals(p).max(decimals(q)));
                        alike(u, v, tol.max(1e-9), if decimals(p).max(decimals(q)) > 0 { last } else { 0.0 })
                    }
                    _ => false,
                };
            if !same {
                return Err(format!("{}: line {}: {q}, Janus {p}", theirs.display(), k + 1));
            }
            cells += 1;
        }
    }
    Ok(cells)
}

/// A run of Janus' own tool (`dir`, its files `outputs`) in the Python history with its script, and that script
/// writing the same files in the run's `python` folder (a raster of Janus' as a VRT may come back as a GeoTIFF), its
/// numbers within `tol` (relative; 0: equal). Ok: what was compared, or why the script was not run.
pub(crate) fn check_run_script(state: &State, dir: &Path, outputs: &[String], tol: f64) -> Result<String, String> {
    let e = state
        .history
        .entries
        .iter()
        .rev()
        .find(|e| Path::new(&e.dir) == dir)
        .ok_or("the run should be in the Python history")?;
    let script =
        std::fs::read_to_string(dir.join(crate::provenance::SCRIPT)).map_err(|_| "no rerun.py beside the run")?;
    if !e.script || !e.code.contains(script.lines().next().unwrap_or("")) {
        return Err("the run's history entry should hold its script".into());
    }
    let modules = imports(&script);
    let Some(python) = python_for(&modules) else {
        return Ok(format!(
            "rerun.py not run (no Python with {}; JANUS_GDAL_PYTHON or JANUS_PYTHON names one)",
            modules.join(", ")
        ));
    };
    let mut cmd = std::process::Command::new(&python);
    cmd.arg(dir.join(crate::provenance::SCRIPT));
    for v in ["GDAL_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "RAYON_NUM_THREADS"] {
        cmd.env(v, "2");
    }
    let out = cmd.output().map_err(|e| format!("{python}: {e}"))?;
    if !out.status.success() {
        return Err(format!("rerun.py failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    let (mut what, mut worst) = (vec![], 0f64);
    for o in outputs {
        let ours = Path::new(o);
        let name = ours.file_name().ok_or("an output without a name")?;
        let mut theirs = dir.join("python").join(name);
        if !theirs.exists() && ours.extension().is_some_and(|x| x == "vrt") {
            theirs.set_extension("tif");
        }
        let ext = ours.extension().map_or(String::new(), |x| x.to_string_lossy().to_lowercase());
        match ext.as_str() {
            "gpkg" => what.push(format!("{} features", same_features(ours, &theirs, tol)?)),
            "csv" => what.push(format!("{} cells", same_csv(ours, &theirs, tol)?)),
            _ => {
                let (n, d) = same_raster(ours, &theirs, tol)?;
                worst = worst.max(d);
                what.push(format!("{n} values"));
            }
        }
    }
    let largest = if tol > 0.0 { format!(", largest difference {worst:.1e}") } else { String::new() };
    Ok(format!(
        "rerun.py run ({}): its {} file(s) as Janus' ({}){largest}",
        modules.join(" "),
        outputs.len(),
        what.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn top_level_imports() {
        let code = "import os\nimport numpy as np\nfrom osgeo import gdal, ogr\nimport rasterio.features\nif x:\n    import zeit\n";
        assert_eq!(super::imports(code), ["numpy", "os", "osgeo", "rasterio"]);
    }
}
