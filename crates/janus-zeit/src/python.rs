//! "Copy Python": the Zeit calls a pixel run makes, as code (what the bridge's run_recording wrote), from the
//! tool's parameters. The code expects what Janus' snippet defines first: `values` (the series), `series` (it as
//! a DataArray (time, y, x)), `dates`; for multiband tools `bands_cube` (time, band, y, x) and `fmask`.

use std::fmt::Write as _;

use crate::pixel::{Params, PixelInput};

/// A float as Python's repr.
pub fn py_float(v: f64) -> String {
    if v.is_nan() {
        return "float(\"nan\")".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "float(\"inf\")".into() } else { "-float(\"inf\")".into() };
    }
    let s = format!("{v:?}");
    match s.split_once('e') {
        Some((m, e)) => {
            let (sign, digits) = e.strip_prefix('-').map_or(("+", e), |d| ("-", d));
            format!("{m}e{sign}{digits:0>2}")
        }
        None => s,
    }
}

fn py_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The calls (code) and the inputs the code needs ("series", "bands_cube", "fmask").
pub fn calls(tool: &str, p: &Params, input: &PixelInput) -> Option<(String, Vec<&'static str>)> {
    let f = |id: &str| py_float(p.f64(id));
    let i = |id: &str| p.int(id).to_string();
    let s = |id: &str| py_str(p.str(id));
    let series = vec!["series"];
    let multiband = vec!["bands_cube"];
    Some(match tool {
        "landtrendr" => (
            format!(
                "result1 = zeit.landtrendr(series, nodata=None, n_jobs=1, direction={}, max_segments={}, pval_threshold={}, \
                 recovery_threshold={}, spike_threshold={}, best_model_proportion={}, vertex_count_overshoot={}, \
                 min_observations_needed={})\nresult2 = zeit.extract_events(result1, event_type={}, sort_by={}, \
                 min_magnitude={}, min_duration={}, pre_val_threshold={})",
                s("event_type"),
                i("max_segments"),
                f("pval_threshold"),
                f("recovery_threshold"),
                f("spike_threshold"),
                f("best_model_proportion"),
                i("vertex_count_overshoot"),
                i("min_observations_needed"),
                s("event_type"),
                s("sort_by"),
                f("min_magnitude"),
                i("min_duration"),
                f("pre_val_threshold")
            ),
            series,
        ),
        "recovery" => (
            format!(
                "result1 = zeit.landtrendr(series, nodata=None, n_jobs=1, direction={}, max_segments={}, pval_threshold={}, \
                 recovery_threshold={}, spike_threshold={}, best_model_proportion={}, vertex_count_overshoot={}, \
                 min_observations_needed={})\nresult2 = zeit.recovery_metrics(result1, event_type={}, sort_by={}, \
                 min_magnitude={}, min_duration={}, pre_val_threshold={}, target={}, horizon={}, pre_years={}, \
                 basis={}, n_jobs=1)",
                s("event_type"),
                i("max_segments"),
                f("pval_threshold"),
                f("recovery_threshold"),
                f("spike_threshold"),
                f("best_model_proportion"),
                i("vertex_count_overshoot"),
                i("min_observations_needed"),
                s("event_type"),
                s("sort_by"),
                f("min_magnitude"),
                i("min_duration"),
                f("pre_val_threshold"),
                f("target"),
                i("horizon"),
                i("pre_years"),
                s("basis")
            ),
            series,
        ),
        "unmix" | "unmix_spectrum" => {
            let e = crate::unmix::endmembers(p).ok()?;
            let (sum_to_one, nonneg) = match p.str("constraint") {
                "nnls" => (false, true),
                "scls" => (true, false),
                "ols" => (false, false),
                _ => (true, true),
            };
            let py_bool = |b: bool| if b { "True" } else { "False" };
            let b = e.bands;
            let mut code = String::from("endmembers = pd.DataFrame({\n");
            for (j, name) in e.names.iter().enumerate() {
                let v: Vec<String> = e.spectra[j * b..(j + 1) * b].iter().map(|&x| py_float(x)).collect();
                let _ = writeln!(code, "    {}: [{}],", py_str(name), v.join(", "));
            }
            let (data, what) = if tool == "unmix" { ("bands_all", "bands") } else { ("spectrum", "spectra") };
            let scale = py_float(e.scale());
            let _ = write!(
                code,
                "}}, index=list({data}.band.values)).T.dropna(axis=1)  # the pins' {what}, in the data's units \
                 (the bands without a value in one left out)\n\
                 result1 = zeit.unmix({data}, endmembers / {scale}, bands=list(endmembers.columns), sum_to_one={}, \
                 nonneg={}, scale={scale}, ndfi=False, nodata=None, n_jobs=1)",
                py_bool(sum_to_one),
                py_bool(nonneg)
            );
            (code, vec![if tool == "unmix" { "bands_all" } else { "spectrum" }])
        }
        "mann_kendall" => {
            let lag = p.int("lag");
            let period = (if p.int("period") != 0 { p.int("period") } else { input.per_year() }).max(1);
            (
                format!(
                    "result1 = zeit.mann_kendall(series, method={}, alpha={}, lag={}, period={period}, min_valid={}, \
                     nodata=None, n_jobs=1)",
                    s("method"),
                    f("alpha"),
                    if lag < 0 { "None".into() } else { lag.to_string() },
                    i("min_valid")
                ),
                series,
            )
        }
        "ar_trend" => (
            format!(
                "result1 = zeit.ar_trend(series, time={}, harmonics={}, gaps={}, min_valid={}, nodata=None, n_jobs=1)",
                s("time"),
                i("harmonics"),
                s("gaps"),
                i("min_valid")
            ),
            series,
        ),
        "smooth" => {
            if p.str("method") == "savgol" {
                let t = input.values.len() as i64;
                let wl = p.int("window");
                let wl = (if wl % 2 != 0 { wl } else { wl + 1 }).min(if t % 2 != 0 { t } else { t - 1 });
                let po = p.int("polyorder").min(wl - 1).max(0);
                (
                    format!("result1 = zeit.smooth(series, method='savgol', window={wl}, polyorder={po}, nodata=None)"),
                    series,
                )
            } else {
                (
                    format!(
                        "result1 = zeit.smooth(series, method='whittaker', lmbda={}, nodata=None, n_jobs=1)",
                        f("lmbda")
                    ),
                    series,
                )
            }
        }
        "bfast" => (
            format!(
                "result1 = zeit.bfast(series, order={}, h={}, max_breaks_trend={}, max_breaks_season={}, max_iter={}, \
                 level={}, min_valid={}, nodata=None, n_jobs=1)",
                i("order"),
                f("h"),
                i("max_breaks_trend"),
                i("max_breaks_season"),
                i("max_iter"),
                f("level"),
                i("min_valid")
            ),
            series,
        ),
        "bfast_lite" => (
            format!(
                "result1 = zeit.bfast_lite(series, order={}, h={}, max_breaks={}, min_valid={}, nodata=None, n_jobs=1)",
                i("order"),
                f("h"),
                i("max_breaks"),
                i("min_valid")
            ),
            series,
        ),
        "bfast_monitor" => {
            // the monitoring starts halfway between the last date of the history and the first after it
            let years = &input.years;
            let t = years.len();
            let ms = p.f64("monitor_start");
            let idx = if ms <= 0.0 {
                (0.75 * t as f64).floor() as usize
            } else {
                years.iter().position(|&y| y >= ms - 1e-9).unwrap_or(t - 1)
            };
            let freq = input.per_year();
            let k = idx.max(1).min((t as i64 - freq).max(1) as usize);
            let start = years[0];
            (
                format!(
                    "result1 = zeit.bfast_monitor(series, {}, start_time={}, frequency={freq}, order={}, h={}, \
                     period={}, alpha={}, min_valid={}, nodata=None, n_jobs=1)",
                    py_float(start + (k as f64 - 0.5) / freq as f64),
                    py_float(start),
                    i("order"),
                    py_float(p.str("h").parse().unwrap_or(0.25)),
                    p.str("period").parse::<f64>().unwrap_or(10.0) as i64,
                    py_float(p.str("alpha").parse().unwrap_or(0.05)),
                    i("min_valid")
                ),
                series,
            )
        }
        "phenology" => {
            let years = &input.years;
            let span = (years[years.len() - 1] - years[0]).max(1.0);
            let s_max = (2.0 * span.ceil() + 2.0).min(200.0) as usize;
            let smoothing = p.str("smoothing");
            (
                format!(
                    "# the gaps filled linearly (by date index), with weight 0: they pull neither the smoothing nor \
                     the curves\nok = np.isfinite(values)\nidx = np.arange(len(values))\nfilled = np.interp(idx, \
                     idx[ok], values[ok])\nseries = series.copy(data=filled[:, None, None])\nweights = \
                     ok.astype(float)[:, None, None]\nresult1 = zeit.phenology(series, curve={}, weights=weights, \
                     annual=False, max_seasons={s_max}, whittaker_lambda={}, apply_whittaker={}, apply_hants={}, \
                     hants_frequencies={}, min_season_length={}, min_amplitude={}, min_pixel_amplitude={}, \
                     nodata=None, n_jobs=1)",
                    s("curve"),
                    f("lambda"),
                    if smoothing == "whittaker" { "True" } else { "False" },
                    if smoothing == "hants" { "True" } else { "False" },
                    i("hants_frequencies"),
                    i("min_season_length"),
                    f("min_amplitude"),
                    f("min_pixel_amplitude")
                ),
                series,
            )
        }
        "twdtw" => {
            let pats = crate::twdtw::patterns(p).ok()?;
            let mut code = String::from("import pandas as pd\npatterns = {\n");
            for (k, pat) in pats.iter().enumerate() {
                let dates: Vec<String> = pat
                    .days
                    .iter()
                    .map(|&d| {
                        let (y, m, dd) = crate::numeric::civil(d);
                        format!("\"{y:04}-{m:02}-{dd:02}\"")
                    })
                    .collect();
                let values: Vec<String> = pat.values.iter().map(|&v| py_float(v)).collect();
                let _ = writeln!(
                    code,
                    "    \"{k}\": pd.Series([{}], index=pd.to_datetime([{}])),  # {}",
                    values.join(", "),
                    dates.join(", "),
                    pat.name
                );
            }
            code += "}\n";
            let max_elapsed = p.f64("max_elapsed");
            let _ = write!(
                code,
                "result1 = zeit.twdtw(series, patterns, steepness={}, midpoint={}, cycle={}, max_elapsed={}, \
                 nodata=None, n_jobs=1)",
                f("steepness"),
                f("midpoint"),
                if p.str("cycle") == "none" { "None" } else { "'year'" },
                if max_elapsed > 0.0 { py_float(max_elapsed) } else { "None".into() }
            );
            (code, series)
        }
        "tmask" => (
            "result1 = zeit.tmask(bands_cube, green='green', swir='swir1', scale=10000.0, nodata=None)".into(),
            multiband,
        ),
        "ndfi" | "coded" => {
            let ct = p.f64("cloud_threshold");
            let ct = if ct < 1.0 { py_float(ct) } else { "None".into() };
            if tool == "ndfi" {
                (
                    format!(
                        "result1 = zeit.unmix(bands_cube, 'souza2005', scale=10000.0, cloud_threshold={ct}, nodata=None, n_jobs=1)"
                    ),
                    multiband,
                )
            } else {
                let start = p.f64("monitor_start");
                (
                    format!(
                        "result1 = zeit.coded(bands_cube, start={}, train_years={}, consec={}, thresh={}, min_years={}, \
                         min_obs={}, direction={}, max_events={}, forest_ndfi={}, scale=10000.0, cloud_threshold={ct}, \
                         nodata=None, n_jobs=1)",
                        if start > 0.0 { py_float(start) } else { "None".into() },
                        f("train_years"),
                        i("consec"),
                        f("thresh"),
                        f("min_years"),
                        i("min_obs"),
                        s("direction"),
                        i("max_events"),
                        f("forest_ndfi")
                    ),
                    multiband,
                )
            }
        }
        "postproc" => {
            let max_length = p.int("max_length").max(0);
            let window = p.int("majority").max(0);
            let min_size = p.int("min_size").max(0);
            let mut code = if max_length > 0 {
                format!("result1 = zeit.desawtooth_classes(series, max_length={max_length})")
            } else {
                "result1 = series".into()
            };
            // each date's map: the spatial filters
            let mut map = "m".to_string();
            if window >= 2 {
                map = format!(
                    "zeit.apply_majority_filter({map}, {}, iterations={})",
                    window | 1,
                    p.int("iterations").max(1)
                );
            }
            if min_size >= 2 {
                map = format!(
                    "zeit.mmu_sieve({map}, min_size={min_size}, connectivity={})",
                    s("connectivity").trim_matches('\'')
                );
            }
            if map != "m" {
                code += &format!("\nresult2 = result1.copy(data=np.stack([{map} for m in result1.values]))");
            }
            (code, series)
        }
        "ewmacd" => {
            let d = crate::ewmacd::dates(input).ok()?;
            let ep = crate::ewmacd::ewmacd_params(p, &d).ok()?;
            (
                format!(
                    "result1 = zeit.ewmacd(series, train_end={}, order={}, lmbda={}, sigmas={}, persistence={}, \
                     xbar_train={}, xbar_monitor={}, nodata=None, n_jobs=1)",
                    ep.train_end,
                    ep.harmonics,
                    py_float(ep.lambda),
                    py_float(ep.sigmas),
                    ep.persistence,
                    py_float(ep.xbar_train),
                    py_float(ep.xbar_monitor)
                ),
                series,
            )
        }
        "ccdc" | "ccdc_synthetic" => (
            format!(
                "result1 = zeit.ccdc(bands_cube, qa=fmask, max_segments={}, conseq_anom={}, chi2_prob_threshold={}, \
                 tmax_cg_prob_threshold={}, num_c={}, thermal_band={}, nodata=None, n_jobs=1)",
                i("max_segments"),
                i("conseq_anom"),
                f("chi2_prob_threshold"),
                f("tmax_cg_prob_threshold"),
                p.str("num_c"),
                if input.bands.contains_key("thermal") { "'thermal'" } else { "None" }
            ),
            vec!["bands_cube", "fmask"],
        ),
        "beast" => {
            let o = crate::beast::options(p, input).ok()?;
            let dt = p.f64("deltat");
            (
                format!(
                    "result1 = zeit.beast(series, season={}, deltat={}, tcp_minmax=(0, {}), scp_minmax=(0, {}), \
                     torder_minmax=(0, {}), samples={}, chains={}, burnin={}, thin={}, seed={}, nodata=None, n_jobs=1)",
                    if o.season { "'harmonic'" } else { "'none'" },
                    if dt > 0.0 { py_float(dt) } else { "None".into() },
                    o.tcp.1,
                    o.scp.1,
                    o.torder.1,
                    o.samples,
                    o.chains,
                    o.burnin,
                    o.thin,
                    o.seed
                ),
                vec!["series"],
            )
        }
        "sccd" => (
            format!(
                "result1 = zeit.sccd(bands_cube.sel(band=[b for b in order if b != 'thermal']), qa=fmask, \
                 max_segments={}, conse={}, prob_threshold={}, outlier_prob={}, max_angle={}, nodata=None, n_jobs=1)",
                i("max_segments"),
                i("conse"),
                f("prob_threshold"),
                f("outlier_prob"),
                f("max_angle")
            ),
            vec!["bands_cube", "fmask"],
        ),
        _ => return None,
    })
}
