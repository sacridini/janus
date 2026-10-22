//! `janus --selftest-zeit IN...` (src/selftest.cpp, runZeitSelfTest): the whole Zeit path without a window, every
//! tool that applies to the series: a fit of the centre pixel (its overlays, rows and Python calls), the run's
//! estimate, a raster run on a 256 x 256 window and its outputs read back; the tools on maps on the maps of dates
//! the others made. Prints the timings.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_cube::{BandSelection, CubeReader};
use janus_zeit::job::{active_outputs, estimate, run_job};
use janus_zeit::{Params, manifest, run_pixel};
use serde_json::{Value, json};

use crate::zeit::{ResultLayer, applicability, base_spec, guess_roles, read_pixel_input, series_inputs};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

pub fn selftest_zeit(inputs: &[String], threads: usize) -> i32 {
    let fail = |what: String| {
        println!("FAIL: {what}");
        1
    };
    let t0 = Instant::now();
    let info = match janus_cube::open_cube(inputs, &BandSelection::default()) {
        Ok(i) => Arc::new(i),
        Err(e) => return fail(format!("open: {e}")),
    };
    let n = info.t();
    println!(
        "series: {}, {} x {}, {n} dates, {} .. {} ({:.0} ms)",
        info.description,
        info.width,
        info.height,
        info.layers[0].label,
        info.layers[n - 1].label,
        ms(t0)
    );
    println!(
        "zeit: {} (native, zeit-core), {} tool(s), {threads} threads",
        janus_zeit::ZEIT_VERSION,
        manifest().tools.len()
    );
    let years: Vec<f64> = (0..n).map(|t| info.decimal_year(t)).collect();
    let mut reader = CubeReader::new(info.clone());
    let (px, py) = (info.width / 2, info.height / 2);
    let series: Vec<f32> = (0..n).map(|t| reader.read_pixel(t, px, py).unwrap_or(f32::NAN)).collect();
    let roles = guess_roles(&info);
    if info.bands_per_date > 1 {
        let r: Vec<String> = roles.iter().map(|(k, b)| format!("{k}={b}")).collect();
        println!("bands per date: {} (shown: {}), roles: {}", info.bands_per_date, info.selection_text(), r.join(" "));
    }
    let work = crate::settings::app_data_dir().join("selftest");
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::create_dir_all(&work);
    // 256 x 256 at the centre: enough for several chunks, quick for slow models
    let x0 = (info.width / 2).saturating_sub(128);
    let y0 = (info.height / 2).saturating_sub(128);
    let window = [x0, y0, info.width.min(x0 + 256), info.height.min(y0 + 256)];
    let (ww, wh) = (window[2] - window[0], window[3] - window[1]);

    // the tools on the series first, then those on result maps
    let mut order: Vec<&janus_zeit::Tool> =
        manifest().tools.iter().filter(|t| t.input.as_deref() != Some("layers")).collect();
    order.extend(manifest().tools.iter().filter(|t| t.input.as_deref() == Some("layers")));
    let mut date_maps: Vec<Value> = vec![];
    let mut ran = 0;
    for tool in order {
        let layers_tool = tool.input.as_deref() == Some("layers");
        let mut why = applicability(tool, &info);
        if layers_tool && date_maps.len() < 2 {
            why = Some("fewer than two maps of dates were made".into());
        }
        let missing: Vec<&String> =
            tool.requires.bands.iter().flatten().filter(|r| !roles.contains_key(r.as_str())).collect();
        if why.is_none() && !missing.is_empty() {
            why =
                Some(format!("unmapped bands: {}", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if let Some(w) = why {
            println!("\n[{}] skipped: {w}", tool.id);
            continue;
        }
        println!("\n[{}]", tool.id);
        let mut params: BTreeMap<String, Value> =
            tool.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect();
        for p in &tool.params {
            if p.kind == "patterns" {
                // three pixels of the series, as a user would add pins
                let pats: Vec<Value> = (0..3)
                    .map(|k| {
                        let (x, y) = (info.width * (2 * k + 1) / 6, info.height / 2);
                        let vals: Vec<Value> =
                            (0..n).map(|t| reader.read_pixel(t, x, y).filter(|v| v.is_finite()).map_or(Value::Null, |v| json!(f64::from(v)))).collect();
                        json!({
                            "name": format!("Class {} ({x}, {y})", k + 1),
                            "from": "selftest",
                            "years": years,
                            "days": if info.time_is_date { json!((0..n).map(|t| info.unix_day(t)).collect::<Vec<_>>()) } else { Value::Null },
                            "values": vals,
                        })
                    })
                    .collect();
                params.insert(p.id.clone(), json!(pats));
            }
            if p.kind == "layers" {
                params.insert(p.id.clone(), json!(date_maps));
            }
        }
        let p = Params::with_defaults(&tool.id, &params).unwrap_or_default();

        if tool.modes.iter().any(|m| m == "pixel") {
            let tp = Instant::now();
            let input = read_pixel_input(&info, tool, &roles, &series, px, py, n - 1);
            let read_ms = ms(tp);
            let tf = Instant::now();
            let r = match run_pixel(&tool.id, &p, &input) {
                Ok(r) => r,
                Err(e) => return fail(format!("{} pixel run: {e}", tool.id)),
            };
            let overlays: Vec<String> =
                r.overlays.iter().map(|o| format!("{}({}, {})", o.kind, o.label, o.x.len())).collect();
            println!(
                "  pixel: {:.3} ms (+ {read_ms:.1} ms reading its bands), overlays: {}",
                ms(tf),
                overlays.join(" ")
            );
            for [a, b] in &r.rows {
                println!("    {a}: {b}");
            }
            // "Copy Python": the Zeit calls, as code
            let Some((code, _)) = janus_zeit::python::calls(&tool.id, &p, &input) else {
                return fail(format!("{}: no Python calls", tool.id));
            };
            let call = code.lines().find(|l| l.contains("zeit.")).unwrap_or("");
            println!("  python: {}", &call[..call.find('(').unwrap_or(call.len())]);
            // the BFAST family's model drawn over the series explains more than the mean does
            if tool.id.starts_with("bfast") {
                let valid: Vec<(f64, f64)> = years
                    .iter()
                    .zip(&series)
                    .filter(|(_, v)| v.is_finite())
                    .map(|(&y, &v)| (y, f64::from(v)))
                    .collect();
                let mean = valid.iter().map(|v| v.1).sum::<f64>() / valid.len().max(1) as f64;
                let (mut se, mut ss, mut k) = (0.0, 0.0, 0);
                for o in r
                    .overlays
                    .iter()
                    .filter(|o| o.kind == "line" && !o.label.contains("trend") && !o.label.contains("projected"))
                {
                    for &(y, v) in &valid {
                        for i in 0..o.x.len().saturating_sub(1) {
                            if let (true, Some(a), Some(b)) = (o.x[i] <= y && y <= o.x[i + 1], o.y[i], o.y[i + 1]) {
                                let f = if o.x[i + 1] > o.x[i] { (y - o.x[i]) / (o.x[i + 1] - o.x[i]) } else { 0.0 };
                                let m = a * (1.0 - f) + b * f;
                                se += (m - v).powi(2);
                                ss += (v - mean).powi(2);
                                k += 1;
                                break;
                            }
                        }
                    }
                }
                if k < 10 {
                    return fail(format!("{}: the fitted model should be drawn over the series", tool.id));
                }
                let r2 = 1.0 - se / ss.max(1e-12);
                println!("  model on the chart: {k} observations under it, R2 {r2:.3}");
                if r2 < 0.05 {
                    return fail(format!("{}: the fitted model does not follow the series", tool.id));
                }
            }
        }

        if tool.modes.iter().any(|m| m == "raster") {
            let mut spec = base_spec(&info, tool, &params, n - 1, threads);
            if !layers_tool && let Err(e) = series_inputs(&info, tool, &roles, &work.join("cube"), &mut spec) {
                return fail(format!("vrt: {e}"));
            }
            spec.window = Some(window);
            let te = Instant::now();
            match estimate(&spec) {
                Ok(e) => println!("  estimate: {:.2} s of computing (measured in {:.0} ms)", e.seconds(ww, wh), ms(te)),
                Err(e) => return fail(format!("{} estimate: {e}", tool.id)),
            }
            spec.output_dir = work.join(&tool.id).to_string_lossy().to_string();
            let tj = Instant::now();
            let result = match run_job(&spec, &|_, _| {}, &AtomicBool::new(false)) {
                Ok(r) => r,
                Err(e) => return fail(format!("{} job: {e}", tool.id)),
            };
            println!("  raster job ({ww} x {wh} px x {n} dates): {:.0} ms", ms(tj));
            // RUST_MIGRATION.md's gate: LandTrendr on 512 x 512 in less than 1.5 s
            if tool.id == "landtrendr" && info.width >= 512 && info.height >= 512 {
                let mut big = spec.clone();
                let (x0, y0) = ((info.width - 512) / 2, (info.height - 512) / 2);
                big.window = Some([x0, y0, x0 + 512, y0 + 512]);
                big.output_dir = work.join("landtrendr-512").to_string_lossy().to_string();
                let tb = Instant::now();
                if let Err(e) = run_job(&big, &|_, _| {}, &AtomicBool::new(false)) {
                    return fail(format!("landtrendr 512 job: {e}"));
                }
                println!("  raster job (512 x 512 px x {n} dates): {:.0} ms (gate: < 1500 ms)", ms(tb));
            }
            let tool_params = Params::with_defaults(&tool.id, &params).unwrap_or_default();
            if result.outputs.len() != active_outputs(tool, &tool_params).len() {
                return fail(format!("{}: the job returned another set of outputs", tool.id));
            }
            for o in &result.outputs {
                let path = o["path"].as_str().unwrap_or("").to_string();
                let name = o["name"].as_str().unwrap_or("");
                if o["series"].as_bool() == Some(true) {
                    // a series output opens as a series of the same dates (what Janus adds as a layer)
                    let s = match janus_cube::open_cube(std::slice::from_ref(&path), &BandSelection::default()) {
                        Ok(s) => s,
                        Err(e) => return fail(format!("{}: series output: {e}", tool.id)),
                    };
                    if s.t() != n
                        || s.time_is_date != info.time_is_date
                        || (info.time_is_date && s.layers[0].label != info.layers[0].label)
                    {
                        return fail(format!("{}: series output {} has other dates", tool.id, o["id"]));
                    }
                    println!(
                        "    {name:<28} series: {}, {} .. {}",
                        s.description,
                        s.layers[0].label,
                        s.layers[s.t() - 1].label
                    );
                    continue;
                }
                let (data, tw, th) = match crate::zeit::load_result(&path, 4096) {
                    Ok(d) => d,
                    Err(e) => return fail(format!("load result: {e}")),
                };
                let unit = o["unit"].as_str().unwrap_or("").to_string();
                let mut layer = ResultLayer::for_test(data, tw, th, unit.clone());
                layer.auto_range();
                let valid = layer.data.iter().filter(|v| v.is_finite()).count();
                println!(
                    "    {name:<28} {:5.1}% valid, range {} .. {}",
                    100.0 * valid as f64 / layer.data.len() as f64,
                    janus_cube::multidim::format_g(f64::from(layer.lo), 6),
                    janus_cube::multidim::format_g(f64::from(layer.hi), 6)
                );
                if unit == "year" && valid > 0 {
                    date_maps.push(json!({"name": format!("{}: {name}", tool.name), "path": path}));
                }
                if let Some(cs) = o["class_series"].as_array().filter(|a| !a.is_empty()) {
                    println!("      typical series of {} classes", cs.len());
                }
                if let Some(classes) = o["classes"].as_array().filter(|a| !a.is_empty()) {
                    let mut count = vec![0usize; classes.len() + 1];
                    for &v in &layer.data {
                        if v.is_finite() && v >= 1.0 && v as usize <= classes.len() {
                            count[v.round() as usize] += 1;
                        }
                    }
                    for (k, c) in classes.iter().enumerate() {
                        println!(
                            "      {:<26} {:5.1}%",
                            c.as_str().unwrap_or(""),
                            100.0 * count[k + 1] as f64 / layer.data.len() as f64
                        );
                    }
                }
            }
        }
        ran += 1;
    }
    if ran == 0 {
        return fail("no tool applies to this series".into());
    }
    println!("OK ({:.1} s total)", t0.elapsed().as_secs_f64());
    0
}
