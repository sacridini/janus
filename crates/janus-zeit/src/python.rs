//! "Copy Python": the Zeit calls a pixel run makes, as code (what the bridge's run_recording wrote), from the
//! tool's parameters. `tool_call` holds each tool's call as data (the function, its arguments, the lines before it);
//! it is shown two ways: `calls`, in full (what a raster run's script and the bridge's recordings have: `nodata=None,
//! n_jobs=1` and all), and `ToolCall::compact`, the Zeit call as a person would write it (what a pin's or an ROI's
//! script has, from `pixel_script`). The code expects what the script defines first: `series` (the series as a
//! DataArray (time, y, x)); for multiband tools `bands_cube` (time, band, y, x) and `fmask`.

use std::fmt::Write as _;

use crate::pixel::{Params, PixelInput};
use crate::temporal_metrics::unique_options;

mod level2;
mod script;
mod tune_script;
mod zeit_defaults;

pub use zeit_defaults::ZEIT_DEFAULTS;

pub use level2::level2_script;
pub use script::{Roi, Spec, Step, Target, pixel_script};
pub use tune_script::{TunePointAt, TuneScriptSet, TuneScriptSpec, tune_functions, tune_script};

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

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// A list of strings as a Python list.
fn py_list(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|x| py_str(x)).collect::<Vec<_>>().join(", "))
}

/// One argument of a call: `name=value`, or the value alone.
#[derive(Clone, Debug)]
struct Arg {
    name: Option<&'static str>,
    value: String,
    /// `nodata=None` and `n_jobs=1`: what a pixel run needs to be exactly what Janus ran, not what Zeit's reader
    /// leaves to do (the script reads with `masked=True`, so NaN is the only no-data, and a pixel is one job)
    plumbing: bool,
}

fn pos(value: impl Into<String>) -> Arg {
    Arg { name: None, value: value.into(), plumbing: false }
}

fn kw(name: &'static str, value: impl Into<String>) -> Arg {
    Arg { name: Some(name), value: value.into(), plumbing: false }
}

fn plumbing(name: &'static str, value: &str) -> Arg {
    Arg { name: Some(name), value: value.into(), plumbing: true }
}

/// A line of the tool's code.
#[derive(Clone, Debug)]
enum Piece {
    /// code as it is (one or more lines, without the last line break)
    Raw(String),
    /// `var = func(args)`
    Call { var: &'static str, func: &'static str, args: Vec<Arg> },
}

/// The Zeit call(s) of a tool: its code, and the inputs the code needs ("series", "bands_cube", "fmask",
/// "bands_all", "spectrum").
#[derive(Clone, Debug)]
pub struct ToolCall {
    pieces: Vec<Piece>,
    pub inputs: Vec<&'static str>,
    /// the code reads `values` (the series as a flat array)
    pub needs_values: bool,
    tool: String,
}

/// One Zeit call as data: the function (`zeit.bfast`), its positional arguments and its keywords (plumbing included),
/// each value as the Python text that writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct CallData {
    pub func: String,
    pub pos: Vec<String>,
    pub kw: Vec<(String, String)>,
}

fn call(var: &'static str, func: &'static str, args: Vec<Arg>) -> Piece {
    Piece::Call { var, func, args }
}

impl ToolCall {
    fn new(tool: &str, pieces: Vec<Piece>, inputs: Vec<&'static str>) -> ToolCall {
        ToolCall { pieces, inputs, needs_values: false, tool: tool.to_string() }
    }

    /// The code in full: every argument, one statement a line (what `calls` gives).
    pub fn full(&self) -> String {
        let lines: Vec<String> = self
            .pieces
            .iter()
            .map(|p| match p {
                Piece::Raw(t) => t.clone(),
                Piece::Call { var, func, args } => {
                    let a: Vec<String> = args.iter().map(arg_text).collect();
                    format!("{var} = {func}({})", a.join(", "))
                }
            })
            .collect();
        lines.join("\n")
    }

    /// The same code over the variable `var` where it reads `series` (a Time series analysis chain's `analysis`).
    pub fn reading(&self, var: &str) -> ToolCall {
        let swap = |t: &str| replace_word(t, "series", var);
        let pieces = self
            .pieces
            .iter()
            .map(|p| match p {
                Piece::Raw(t) => Piece::Raw(swap(t)),
                Piece::Call { var: v, func, args } => Piece::Call {
                    var: v,
                    func,
                    args: args.iter().map(|a| Arg { value: swap(&a.value), ..a.clone() }).collect(),
                },
            })
            .collect();
        ToolCall { pieces, ..self.clone() }
    }

    /// The same calls with the keyword `name` set to `value` (added after `qa` or at the end if the call has none), or
    /// left out if `value` is None.
    pub(crate) fn with_arg(&self, name: &'static str, value: Option<&str>) -> ToolCall {
        let pieces = self
            .pieces
            .iter()
            .map(|p| match p {
                Piece::Call { var, func, args } => {
                    let mut args = args.clone();
                    let at = args.iter().position(|a| a.name == Some(name));
                    match (at, value) {
                        (Some(i), Some(v)) => args[i].value = v.to_string(),
                        (Some(i), None) => {
                            args.remove(i);
                        }
                        (None, Some(v)) => {
                            let i = args.iter().position(|a| a.name == Some("qa")).map_or(args.len(), |q| q + 1);
                            args.insert(i, kw(name, v));
                        }
                        (None, None) => {}
                    }
                    Piece::Call { var, func, args }
                }
                raw => raw.clone(),
            })
            .collect();
        ToolCall { pieces, ..self.clone() }
    }

    /// The keyword arguments of the calls, (function, parameter, value as Python writes it), plumbing included.
    pub fn arguments(&self) -> Vec<(String, String, String)> {
        let mut out = vec![];
        for p in &self.pieces {
            if let Piece::Call { func, args, .. } = p {
                out.extend(
                    args.iter().filter_map(|a| a.name.map(|n| (func.to_string(), n.to_string(), a.value.clone()))),
                );
            }
        }
        out
    }

    /// The Zeit calls as data (the lines of plain code are not calls and are left out), in the order they run.
    pub fn data(&self) -> Vec<CallData> {
        self.pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Call { func, args, .. } => Some(CallData {
                    func: func.to_string(),
                    pos: args.iter().filter(|a| a.name.is_none()).map(|a| a.value.clone()).collect(),
                    kw: args.iter().filter_map(|a| a.name.map(|n| (n.to_string(), a.value.clone()))).collect(),
                }),
                Piece::Raw(_) => None,
            })
            .collect()
    }

    /// The names of the results the code makes (`result1`, `result2`...), in the order they are made.
    fn results(&self) -> Vec<&'static str> {
        self.pieces
            .iter()
            .flat_map(|p| match p {
                // a call may make several (`result1, result2 = ...`) or an intermediate (`angles = ...`)
                Piece::Call { var, .. } => var.split(", ").filter(|v| v.starts_with("result")).collect::<Vec<_>>(),
                Piece::Raw(t) => {
                    ["result1", "result2"].into_iter().filter(|v| t.starts_with(&format!("{v} = "))).collect()
                }
            })
            .collect()
    }

    /// The names a result is given in a script for a person: what it is, not its number.
    fn result_names(&self) -> &'static [&'static str] {
        match self.tool.as_str() {
            "landtrendr" => &["fit", "events"],
            "recovery" => &["fit", "recovery"],
            "linear_trend" => &["folded", "trend"],
            "cat" => &["folded", "cat"],
            "postproc" => &["classes", "filtered"],
            "nbar" => &["nbar", "cfactor"],
            _ => &["result"],
        }
    }

    /// The code as one writes it: the plumbing arguments left out, long calls broken over lines, each result under
    /// the name `prefix` + what it is (`fit`, `events`...). Returns the code and the names of the results.
    pub fn compact(&self, prefix: &str) -> (String, Vec<String>) {
        let names = self.result_names();
        let results = self.results();
        let rename = |t: &str| -> String {
            let mut out = t.to_string();
            for (k, r) in results.iter().enumerate() {
                let to = format!("{prefix}{}", names.get(k).or(names.first()).copied().unwrap_or("result"));
                out = replace_word(&out, r, &to);
            }
            out
        };
        let lines: Vec<String> = self
            .pieces
            .iter()
            .map(|p| match p {
                Piece::Raw(t) => rename(t),
                Piece::Call { var, func, args } => {
                    let a: Vec<String> = args
                        .iter()
                        .filter(|a| !a.plumbing && !is_default(func, a))
                        .map(|a| rename(&arg_text(a)))
                        .collect();
                    wrap_call(&rename(var), func, &a)
                }
            })
            .collect();
        let shown: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(k, _)| format!("{prefix}{}", names.get(k).or(names.first()).copied().unwrap_or("result")))
            .collect();
        (lines.join("\n"), shown)
    }
}

/// Whether the argument is a keyword with the value Zeit's function has for it by default.
fn is_default(func: &str, a: &Arg) -> bool {
    a.name.is_some_and(|n| ZEIT_DEFAULTS.iter().any(|&(f, p, v)| f == func && p == n && v == a.value))
}

fn arg_text(a: &Arg) -> String {
    match a.name {
        Some(n) => format!("{n}={}", a.value),
        None => a.value.clone(),
    }
}

/// `text` with the identifier `word` replaced by `to` (whole identifiers only).
fn replace_word(text: &str, word: &str, to: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(word) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + word.len()..].chars().next();
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out += &rest[..i];
        out += if ident(before) || ident(after) { word } else { to };
        rest = &rest[i + word.len()..];
    }
    out + rest
}

/// `var = func(args)` on one line if it fits, else the arguments filled in lines of at most 100 characters.
fn wrap_call(var: &str, func: &str, args: &[String]) -> String {
    let one = format!("{var} = {func}({})", args.join(", "));
    if one.chars().count() <= 100 {
        return one;
    }
    let mut out = format!("{var} = {func}(\n");
    let mut line = String::new();
    for (k, a) in args.iter().enumerate() {
        let item = if k + 1 < args.len() { format!("{a},") } else { a.clone() };
        if !line.is_empty() && 4 + line.chars().count() + 1 + item.chars().count() > 100 {
            let _ = writeln!(out, "    {line}");
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line += &item;
    }
    let _ = write!(out, "    {line}\n)");
    out
}

/// The calls (code) and the inputs the code needs ("series", "bands_cube", "fmask").
pub fn calls(tool: &str, p: &Params, input: &PixelInput) -> Option<(String, Vec<&'static str>)> {
    let c = tool_call(tool, p, input)?;
    Some((c.full(), c.inputs))
}

/// LandTrendr's call, which the event extraction and the recovery metrics follow.
fn landtrendr_fit(p: &Params) -> Piece {
    let f = |id: &str| py_float(p.f64(id));
    let i = |id: &str| p.int(id).to_string();
    let s = |id: &str| py_str(p.str(id));
    call(
        "result1",
        "zeit.landtrendr",
        vec![
            pos("series"),
            plumbing("nodata", "None"),
            plumbing("n_jobs", "1"),
            kw("direction", s("event_type")),
            kw("max_segments", i("max_segments")),
            kw("pval_threshold", f("pval_threshold")),
            kw("recovery_threshold", f("recovery_threshold")),
            kw("spike_threshold", f("spike_threshold")),
            kw("best_model_proportion", f("best_model_proportion")),
            kw("vertex_count_overshoot", i("vertex_count_overshoot")),
            kw("min_observations_needed", i("min_observations_needed")),
        ],
    )
}

/// A tool's Zeit call(s), as data.
pub fn tool_call(tool: &str, p: &Params, input: &PixelInput) -> Option<ToolCall> {
    let f = |id: &str| py_float(p.f64(id));
    let i = |id: &str| p.int(id).to_string();
    let s = |id: &str| py_str(p.str(id));
    let one = |piece: Piece| ToolCall::new(tool, vec![piece], vec!["series"]);
    Some(match tool {
        "landtrendr" => ToolCall::new(
            tool,
            vec![
                landtrendr_fit(p),
                call(
                    "result2",
                    "zeit.extract_events",
                    vec![
                        pos("result1"),
                        kw("event_type", s("event_type")),
                        kw("sort_by", s("sort_by")),
                        kw("min_magnitude", f("min_magnitude")),
                        kw("min_duration", i("min_duration")),
                        kw("pre_val_threshold", f("pre_val_threshold")),
                    ],
                ),
            ],
            vec!["series"],
        ),
        "recovery" => ToolCall::new(
            tool,
            vec![
                landtrendr_fit(p),
                call(
                    "result2",
                    "zeit.recovery_metrics",
                    vec![
                        pos("result1"),
                        kw("event_type", s("event_type")),
                        kw("sort_by", s("sort_by")),
                        kw("min_magnitude", f("min_magnitude")),
                        kw("min_duration", i("min_duration")),
                        kw("pre_val_threshold", f("pre_val_threshold")),
                        kw("target", f("target")),
                        kw("horizon", i("horizon")),
                        kw("pre_years", i("pre_years")),
                        kw("basis", s("basis")),
                        plumbing("n_jobs", "1"),
                    ],
                ),
            ],
            vec!["series"],
        ),
        "unmix" | "unmix_spectrum" => {
            let e = crate::unmix::endmembers(p).ok()?;
            let (sum_to_one, nonneg) = match p.str("constraint") {
                "nnls" => (false, true),
                "scls" => (true, false),
                "ols" => (false, false),
                _ => (true, true),
            };
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
                 (the bands without a value in one left out)"
            );
            ToolCall::new(
                tool,
                vec![
                    Piece::Raw(code),
                    call(
                        "result1",
                        "zeit.unmix",
                        vec![
                            pos(data),
                            pos(format!("endmembers / {scale}")),
                            kw("bands", "list(endmembers.columns)"),
                            kw("sum_to_one", py_bool(sum_to_one)),
                            kw("nonneg", py_bool(nonneg)),
                            kw("scale", scale),
                            kw("ndfi", "False"),
                            plumbing("nodata", "None"),
                            plumbing("n_jobs", "1"),
                        ],
                    ),
                ],
                vec![if tool == "unmix" { "bands_all" } else { "spectrum" }],
            )
        }
        "mann_kendall" => {
            let lag = p.int("lag");
            let period = (if p.int("period") != 0 { p.int("period") } else { input.per_year() }).max(1);
            one(call(
                "result1",
                "zeit.mann_kendall",
                vec![
                    pos("series"),
                    kw("method", s("method")),
                    kw("alpha", f("alpha")),
                    kw("lag", if lag < 0 { "None".to_string() } else { lag.to_string() }),
                    kw("period", period.to_string()),
                    kw("min_valid", i("min_valid")),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            ))
        }
        "ar_trend" => one(call(
            "result1",
            "zeit.ar_trend",
            vec![
                pos("series"),
                kw("time", s("time")),
                kw("harmonics", i("harmonics")),
                kw("gaps", s("gaps")),
                kw("min_valid", i("min_valid")),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
        "smooth" => {
            if p.str("method") == "savgol" {
                let t = input.values.len() as i64;
                let wl = p.int("window");
                let wl = (if wl % 2 != 0 { wl } else { wl + 1 }).min(if t % 2 != 0 { t } else { t - 1 });
                let po = p.int("polyorder").min(wl - 1).max(0);
                one(call(
                    "result1",
                    "zeit.smooth",
                    vec![
                        pos("series"),
                        kw("method", "'savgol'"),
                        kw("window", wl.to_string()),
                        kw("polyorder", po.to_string()),
                        plumbing("nodata", "None"),
                    ],
                ))
            } else {
                one(call(
                    "result1",
                    "zeit.smooth",
                    vec![
                        pos("series"),
                        kw("method", "'whittaker'"),
                        kw("lmbda", f("lmbda")),
                        plumbing("nodata", "None"),
                        plumbing("n_jobs", "1"),
                    ],
                ))
            }
        }
        "bfast" => one(call(
            "result1",
            "zeit.bfast",
            vec![
                pos("series"),
                kw("order", i("order")),
                kw("h", f("h")),
                kw("max_breaks_trend", i("max_breaks_trend")),
                kw("max_breaks_season", i("max_breaks_season")),
                kw("max_iter", i("max_iter")),
                kw("level", f("level")),
                kw("min_valid", i("min_valid")),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
        "bfast_lite" => one(call(
            "result1",
            "zeit.bfast_lite",
            vec![
                pos("series"),
                kw("order", i("order")),
                kw("h", f("h")),
                kw("max_breaks", i("max_breaks")),
                kw("min_valid", i("min_valid")),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
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
            one(call(
                "result1",
                "zeit.bfast_monitor",
                vec![
                    pos("series"),
                    pos(py_float(start + (k as f64 - 0.5) / freq as f64)),
                    kw("start_time", py_float(start)),
                    kw("frequency", freq.to_string()),
                    kw("order", i("order")),
                    kw("h", py_float(p.str("h").parse().unwrap_or(0.25))),
                    kw("period", (p.str("period").parse::<f64>().unwrap_or(10.0) as i64).to_string()),
                    kw("alpha", py_float(p.str("alpha").parse().unwrap_or(0.05))),
                    kw("min_valid", i("min_valid")),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            ))
        }
        "phenology" => {
            let years = &input.years;
            let span = (years[years.len() - 1] - years[0]).max(1.0);
            let s_max = (2.0 * span.ceil() + 2.0).min(200.0) as usize;
            let smoothing = p.str("smoothing");
            let mut c = ToolCall::new(
                tool,
                vec![
                    Piece::Raw(
                        "# the gaps filled linearly (by date index), with weight 0: they pull neither the smoothing \
                         nor the curves\nok = np.isfinite(values)\nidx = np.arange(len(values))\nfilled = \
                         np.interp(idx, idx[ok], values[ok])\nseries = series.copy(data=filled.reshape(series.shape))\n\
                         weights = ok.astype(float).reshape(series.shape)"
                            .into(),
                    ),
                    call(
                        "result1",
                        "zeit.phenology",
                        vec![
                            pos("series"),
                            kw("curve", s("curve")),
                            kw("weights", "weights"),
                            kw("annual", "False"),
                            kw("max_seasons", s_max.to_string()),
                            kw("whittaker_lambda", f("lambda")),
                            kw("apply_whittaker", py_bool(smoothing == "whittaker")),
                            kw("apply_hants", py_bool(smoothing == "hants")),
                            kw("hants_frequencies", i("hants_frequencies")),
                            kw("min_season_length", i("min_season_length")),
                            kw("min_amplitude", f("min_amplitude")),
                            kw("min_pixel_amplitude", f("min_pixel_amplitude")),
                            plumbing("nodata", "None"),
                            plumbing("n_jobs", "1"),
                        ],
                    ),
                ],
                vec!["series"],
            );
            c.needs_values = true;
            c
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
            code += "}";
            let max_elapsed = p.f64("max_elapsed");
            ToolCall::new(
                tool,
                vec![
                    Piece::Raw(code),
                    call(
                        "result1",
                        "zeit.twdtw",
                        vec![
                            pos("series"),
                            pos("patterns"),
                            kw("steepness", f("steepness")),
                            kw("midpoint", f("midpoint")),
                            kw("cycle", if p.str("cycle") == "none" { "None" } else { "'year'" }),
                            kw("max_elapsed", if max_elapsed > 0.0 { py_float(max_elapsed) } else { "None".into() }),
                            plumbing("nodata", "None"),
                            plumbing("n_jobs", "1"),
                        ],
                    ),
                ],
                vec!["series"],
            )
        }
        "tmask" => ToolCall::new(
            tool,
            vec![call(
                "result1",
                "zeit.tmask",
                vec![
                    pos("bands_cube"),
                    kw("green", "'green'"),
                    kw("swir", "'swir1'"),
                    kw("scale", "10000.0"),
                    plumbing("nodata", "None"),
                ],
            )],
            vec!["bands_cube"],
        ),
        "ndfi" | "coded" => {
            let ct = p.f64("cloud_threshold");
            let ct = if ct < 1.0 { py_float(ct) } else { "None".into() };
            if tool == "ndfi" {
                ToolCall::new(
                    tool,
                    vec![call(
                        "result1",
                        "zeit.unmix",
                        vec![
                            pos("bands_cube"),
                            pos("'souza2005'"),
                            kw("scale", "10000.0"),
                            kw("cloud_threshold", ct),
                            plumbing("nodata", "None"),
                            plumbing("n_jobs", "1"),
                        ],
                    )],
                    vec!["bands_cube"],
                )
            } else {
                let start = p.f64("monitor_start");
                ToolCall::new(
                    tool,
                    vec![call(
                        "result1",
                        "zeit.coded",
                        vec![
                            pos("bands_cube"),
                            kw("start", if start > 0.0 { py_float(start) } else { "None".into() }),
                            kw("train_years", f("train_years")),
                            kw("consec", i("consec")),
                            kw("thresh", f("thresh")),
                            kw("min_years", f("min_years")),
                            kw("min_obs", i("min_obs")),
                            kw("direction", s("direction")),
                            kw("max_events", i("max_events")),
                            kw("forest_ndfi", f("forest_ndfi")),
                            kw("scale", "10000.0"),
                            kw("cloud_threshold", ct),
                            plumbing("nodata", "None"),
                            plumbing("n_jobs", "1"),
                        ],
                    )],
                    vec!["bands_cube"],
                )
            }
        }
        "postproc" => {
            let max_length = p.int("max_length").max(0);
            let window = p.int("majority").max(0);
            let min_size = p.int("min_size").max(0);
            let mut pieces = vec![if max_length > 0 {
                call(
                    "result1",
                    "zeit.desawtooth_classes",
                    vec![pos("series"), kw("max_length", max_length.to_string())],
                )
            } else {
                Piece::Raw("result1 = series".into())
            }];
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
                pieces.push(Piece::Raw(format!(
                    "result2 = result1.copy(data=np.stack([{map} for m in result1.values]))"
                )));
            }
            ToolCall::new(tool, pieces, vec!["series"])
        }
        "ewmacd" => {
            let d = crate::ewmacd::dates(input).ok()?;
            let ep = crate::ewmacd::ewmacd_params(p, &d).ok()?;
            one(call(
                "result1",
                "zeit.ewmacd",
                vec![
                    pos("series"),
                    kw("train_end", ep.train_end.to_string()),
                    kw("order", ep.harmonics.to_string()),
                    kw("lmbda", py_float(ep.lambda)),
                    kw("sigmas", py_float(ep.sigmas)),
                    kw("persistence", ep.persistence.to_string()),
                    kw("xbar_train", py_float(ep.xbar_train)),
                    kw("xbar_monitor", py_float(ep.xbar_monitor)),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            ))
        }
        "ccdc" | "ccdc_synthetic" => ToolCall::new(
            tool,
            vec![call(
                "result1",
                "zeit.ccdc",
                vec![
                    pos("bands_cube"),
                    kw("qa", "fmask"),
                    kw("max_segments", i("max_segments")),
                    kw("conseq_anom", i("conseq_anom")),
                    kw("chi2_prob_threshold", f("chi2_prob_threshold")),
                    kw("tmax_cg_prob_threshold", f("tmax_cg_prob_threshold")),
                    kw("num_c", p.str("num_c")),
                    kw("thermal_band", if input.bands.contains_key("thermal") { "'thermal'" } else { "None" }),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            )],
            vec!["bands_cube", "fmask"],
        ),
        "beast" => {
            let o = crate::beast::options(p, input).ok()?;
            let dt = p.f64("deltat");
            one(call(
                "result1",
                "zeit.beast",
                vec![
                    pos("series"),
                    kw("season", if o.season { "'harmonic'" } else { "'none'" }),
                    kw("deltat", if dt > 0.0 { py_float(dt) } else { "None".into() }),
                    kw("tcp_minmax", format!("(0, {})", o.tcp.1)),
                    kw("scp_minmax", format!("(0, {})", o.scp.1)),
                    kw("torder_minmax", format!("(0, {})", o.torder.1)),
                    kw("samples", o.samples.to_string()),
                    kw("chains", o.chains.to_string()),
                    kw("burnin", o.burnin.to_string()),
                    kw("thin", o.thin.to_string()),
                    kw("seed", o.seed.to_string()),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            ))
        }
        "sccd" => ToolCall::new(
            tool,
            vec![call(
                "result1",
                "zeit.sccd",
                vec![
                    pos(if input.bands.contains_key("thermal") {
                        "bands_cube.sel(band=[b for b in order if b != 'thermal'])"
                    } else {
                        "bands_cube"
                    }),
                    kw("qa", "fmask"),
                    kw("max_segments", i("max_segments")),
                    kw("conse", i("conse")),
                    kw("prob_threshold", f("prob_threshold")),
                    kw("outlier_prob", f("outlier_prob")),
                    kw("max_angle", f("max_angle")),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            )],
            vec!["bands_cube", "fmask"],
        ),
        "interpolate" => {
            let mut args = vec![pos("series"), pos(s("method")), kw("step", i("step"))];
            match p.str("method") {
                "rbf" => {
                    let sigma: Vec<String> =
                        p.str("sigma").split(',').filter_map(|x| x.trim().parse::<f64>().ok()).map(py_float).collect();
                    args.push(kw("sigma", format!("[{}]", sigma.join(", "))));
                    args.push(kw("cutoff", f("cutoff")));
                }
                "moving" => args.push(kw("window", f("window"))),
                "harmonic" => {
                    args.push(kw("modes", i("modes")));
                    args.push(kw("trend", py_bool(p.bool("trend"))));
                }
                _ => {}
            }
            args.push(plumbing("nodata", "None"));
            args.push(plumbing("n_jobs", "1"));
            one(call("result1", "zeit.interpolate", args))
        }
        "temporal_metrics" => one(call(
            "result1",
            "zeit.temporal_metrics",
            vec![
                pos("series"),
                pos(py_list(&unique_options(p, "metrics"))),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
        "fold" => one(call(
            "result1",
            "zeit.temporal_metrics",
            vec![
                pos("series"),
                pos(py_list(&unique_options(p, "metrics"))),
                kw("by", s("by")),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
        "standardize" => one(call(
            "result1",
            "zeit.standardize",
            vec![pos("series"), pos(s("method")), plumbing("nodata", "None"), plumbing("n_jobs", "1")],
        )),
        "linear_trend" | "cat" => {
            let by = s("by");
            let fold = call(
                "result1",
                "zeit.temporal_metrics",
                vec![
                    pos("series"),
                    pos(format!("[{}]", s("metric"))),
                    kw("by", by.clone()),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                ],
            );
            let stats = py_list(&unique_options(p, "statistics"));
            let second = if tool == "cat" {
                call(
                    "result2",
                    "zeit.cat",
                    vec![
                        pos("result1"),
                        kw("penalty", py_bool(p.bool("penalty"))),
                        kw("dim", by),
                        kw("conf", f("conf")),
                        kw("tail", s("tail")),
                        plumbing("nodata", "None"),
                        plumbing("n_jobs", "1"),
                    ],
                )
            } else {
                call(
                    "result2",
                    "zeit.linear_trend",
                    vec![
                        pos("result1"),
                        kw("dim", by),
                        kw("conf", f("conf")),
                        kw("tail", s("tail")),
                        plumbing("nodata", "None"),
                        plumbing("n_jobs", "1"),
                    ],
                )
            };
            ToolCall::new(
                tool,
                vec![fold, second, Piece::Raw(format!("# the statistics the tool writes: {stats}"))],
                vec!["series"],
            )
        }
        "tasseled_cap" => {
            let mut args = vec![
                pos("clear_cube"),
                pos(py_list(&unique_options(p, "components"))),
                kw("coefficients", s("coefficients")),
            ];
            if p.str("coefficients") == "sensor" && !input.sensors.is_empty() {
                args.push(kw("sensor", py_list(&zeit_sensors(&input.sensors))));
            }
            args.extend([kw("bands", ROLE_BANDS), plumbing("nodata", "None"), plumbing("n_jobs", "1")]);
            ToolCall::new(
                tool,
                vec![Piece::Raw(CLEAR_ONLY.into()), call("result1", "zeit.tasseled_cap", args)],
                vec!["bands_cube", "fmask"],
            )
        }
        "spectral_adjust" => {
            let mut args = vec![pos("clear_cube")];
            match p.str("sensor") {
                "auto" if input.sensors.is_empty() => {}
                "auto" => args.push(kw("sensor", py_list(&zeit_sensors(&input.sensors)))),
                name => args.push(kw("sensor", py_str(name))),
            }
            args.extend([
                kw("bands", ROLE_BANDS),
                kw("scale", "10000.0"),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
                kw("force_search", py_bool(p.bool("force_search"))),
            ]);
            ToolCall::new(
                tool,
                vec![Piece::Raw(CLEAR_ONLY.into()), call("result1", "zeit.spectral_adjust", args)],
                vec!["bands_cube", "fmask"],
            )
        }
        "nbar" => {
            let roles: Vec<String> = zeit_core::brdf::ROLES
                .iter()
                .filter(|r| input.bands.contains_key(**r))
                .map(|r| format!("{}: {}", py_str(r), py_str(r)))
                .collect();
            let mut pieces = vec![];
            if crate::nbar::sentinel2(p) {
                // one source per date (a list of them for a date of several tiles), as the dates of the cube
                let sources: Vec<String> = input
                    .granules
                    .iter()
                    .map(|g| match g.as_slice() {
                        [] => "None".to_string(),
                        [one] => py_str(one),
                        many => py_list(many),
                    })
                    .collect();
                pieces.push(Piece::Raw(format!(
                    "# Sentinel-2's angles from each date's granule metadata (MTD_TL.xml, as the STAC items give their\n\
                     # hrefs; Zeit signs Planetary Computer's if the planetary_computer package is installed).\n\
                     # One source per date of the cube, in its order.\n\
                     sources = [{}]",
                    sources.join(", ")
                )));
                pieces.push(call(
                    "angles",
                    "zeit.sentinel2_angles",
                    vec![pos("sources"), kw("like", "nbar_cube"), kw("view_band", py_str(p.str("view_band")))],
                ));
            } else {
                let scale = if p.str("angle_scale") == "hundredths" { " * 0.01" } else { "" };
                pieces.push(Piece::Raw(format!(
                    "angles = xr.Dataset({{a.upper(): nbar_angles.sel(band=a, drop=True){scale} for a in nbar_angles.band.values}})  \
                     # the angle bands, in degrees"
                )));
            }
            let (var, cfactor) = if p.bool("cfactor") { ("result1, result2", "True") } else { ("result1", "False") };
            pieces.push(call(
                var,
                "zeit.nbar",
                vec![
                    pos("nbar_cube"),
                    pos("angles"),
                    kw("sza", if p.str("sza") == "fixed" { f("sza_fixed") } else { "'scene'".into() }),
                    kw("bands", format!("{{{}}}", roles.join(", "))),
                    plumbing("nodata", "None"),
                    plumbing("n_jobs", "1"),
                    kw("return_cfactor", cfactor),
                ],
            ));
            ToolCall::new(tool, pieces, vec!["nbar_cube"])
        }
        "improphe" => {
            let by = match p.str("by") {
                "year" => "'year'",
                "scenes" => "None",
                _ => "'auto'",
            };
            let seasons: Vec<String> = p
                .str("seasons")
                .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
                .filter_map(|x| x.parse::<i32>().ok())
                .map(|x| x.to_string())
                .collect();
            let pca = match p.str("pca") {
                "principal" => "'principal'",
                "force" => "'force'",
                _ => "None",
            };
            ToolCall::new(
                tool,
                vec![call(
                    "result1",
                    "zeit.improphe",
                    vec![
                        pos("coarse_cube"),
                        pos("fine_cube"),
                        kw("by", by),
                        kw("seasons", format!("({})", seasons.join(", "))),
                        kw("kernel_size", f("kernel_size")),
                        kw("texture_size", f("texture_size")),
                        kw("pc_variance", f("pc_variance")),
                        kw("force", py_bool(p.bool("force"))),
                        kw("pca", pca),
                        plumbing("nodata", "None"),
                        plumbing("n_jobs", "1"),
                    ],
                )],
                vec!["improphe_cubes"],
            )
        }
        "clear_observations" => one(call(
            "result1",
            "zeit.clear_observations",
            vec![
                pos("series"),
                kw("months", i("months")),
                kw("metrics", py_list(&unique_options(p, "statistics"))),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
        "polar_metrics" => one(call(
            "result1",
            "zeit.polar_metrics",
            vec![
                pos("series"),
                pos(py_list(&unique_options(p, "metrics"))),
                kw("start", f("start")),
                kw("mid", f("mid")),
                kw("end", f("end")),
                kw("adaptive", py_bool(p.bool("adaptive"))),
                plumbing("nodata", "None"),
                plumbing("n_jobs", "1"),
            ],
        )),
        _ => return None,
    })
}

/// The cube of the observations the quality band keeps (the others missing, as Janus leaves them out); `fmask` is a
/// DataArray (time, y, x).
const CLEAR_ONLY: &str = "clear_cube = bands_cube.where(fmask <= 1)  # the observations the quality band keeps (Janus leaves the others out)";

/// The cube's band of each role: Zeit looks for `swir16` and `swir22` where Janus names them `swir1` and `swir2`.
const ROLE_BANDS: &str =
    "{'blue': 'blue', 'green': 'green', 'red': 'red', 'nir': 'nir', 'swir1': 'swir1', 'swir2': 'swir2'}";

/// Each date's sensor as Zeit's `sensor=` takes it: FORCE's names, and "sentinel-2" for any Sentinel-2 unit.
fn zeit_sensors(sensors: &[String]) -> Vec<String> {
    sensors.iter().map(|s| if s == "SEN2" { "sentinel-2".into() } else { s.clone() }).collect()
}
