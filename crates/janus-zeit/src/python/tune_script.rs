//! "Copy Python" of a tuning on samples (Tune on samples): the loop that redoes the search outside Janus with Zeit.
//! It reads each reference point's series as the script of a pin does (`pixel_script`'s reads, one `load_raster` of
//! the pixel per point), runs the tool's Zeit call once per set of parameters on all the points together (a
//! `(time, 1, points)` cube, as the engine's one-row chunk), takes the calendar year of the change the map would
//! show from the result, scores it with the engine's definitions (`tune::Scores`) and prints the table in the
//! engine's order. The sets are written out in full, the values the search sampled, so the script needs no random
//! generator to match the run.
//!
//! The tools whose map date is one output of a single-series Zeit call are written: LandTrendr, BFAST, BFAST Lite,
//! BFAST Monitor and EWMACD. The tools of bands (CCDC, S-CCD, CODED) and BEAST are not (their reads and results
//! need more than this loop).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use janus_cube::CubeInfo;
use serde_json::Value;

use super::script::{Spec, Target, pixel_script};
use super::{CallData, py_float, py_str, tool_call};
use crate::pixel::{Params, PixelInput};
use crate::tune::ScoreOpts;

/// A reference point on the series' pixels.
#[derive(Clone, Debug)]
pub struct TunePointAt {
    pub col: usize,
    pub row: usize,
    /// the year of the change (None: no change)
    pub reference: Option<f64>,
    /// the weight it has when the scores are weighted by the strata
    pub weight: f64,
}

/// A set of parameters of the search, by the tuning's `vary` order.
#[derive(Clone, Debug)]
pub struct TuneScriptSet {
    pub name: String,
    pub values: Vec<Value>,
    /// the dated F1 the engine found for it (the script prints how far its own is)
    pub f1_dated: Option<f64>,
}

/// What the script is made from.
pub struct TuneScriptSpec<'a> {
    pub info: &'a CubeInfo,
    pub version: &'a str,
    /// the layer's name
    pub name: &'a str,
    pub tool: &'a str,
    /// the tool's parameters the sets that do not vary them keep
    pub base: &'a BTreeMap<String, Value>,
    /// the series' dates, as the engine ran them
    pub input: &'a PixelInput,
    pub vary: &'a [String],
    /// the defaults, the current values, then the sampled ones
    pub sets: &'a [TuneScriptSet],
    pub points: &'a [TunePointAt],
    pub opts: ScoreOpts,
    /// what the reference is, for a comment
    pub reference: &'a str,
    pub roles: &'a BTreeMap<String, i32>,
    /// a file's path as the script writes it
    pub path: &'a dyn Fn(&str) -> String,
}

/// A parameter value as Python writes it.
fn py_value(v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        Value::Number(n) => py_float(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => py_str(s),
        other => py_str(&other.to_string()),
    }
}

/// `dict(a=1, b=2)`.
fn py_dict(kw: &[(String, String)]) -> String {
    format!("dict({})", kw.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", "))
}

/// The function that turns the Zeit result into the calendar year of the change the map shows at each point (NaN:
/// none), by tool; None for a tool this loop does not write.
fn map_years(tool: &str, calls: &[CallData]) -> Option<String> {
    let f = |i: usize| calls.get(i).map(|c| c.func.as_str());
    Some(match tool {
        "landtrendr" => format!(
            "def map_years(cube, kw):\n    \"\"\"LandTrendr's event year (the map's date), as zeit.extract_events gives it.\"\"\"\n    \
             fit = {a}(cube, **kw[\"{a}\"])\n    events = {b}(fit, **kw[\"{b}\"])\n    \
             year = events[\"yod\"].values.astype(float).reshape(-1)\n    return np.where(year > 0, year, np.nan)\n",
            a = f(0)?,
            b = f(1)?
        ),
        "bfast" => format!(
            "def map_years(cube, kw):\n    \"\"\"The year of the trend break of the largest magnitude, where the series was valid.\"\"\"\n    \
             k = kw[\"{a}\"]\n    res = {a}(cube, **k)\n    n = k[\"max_breaks_trend\"]\n    \
             mags = np.stack([res[f\"trend_magnitude_{{j}}\"].values.reshape(-1) for j in range(1, n + 1)])\n    \
             dates = np.stack([res[f\"trend_break_date_{{j}}\"].values.reshape(-1) for j in range(1, n + 1)])\n    \
             return largest_year(mags, dates, res[\"valid\"].values.reshape(-1) == 1)\n",
            a = f(0)?
        ),
        "bfast_lite" => format!(
            "def map_years(cube, kw):\n    \"\"\"The year of the break of the largest magnitude, where the series was valid.\"\"\"\n    \
             k = kw[\"{a}\"]\n    res = {a}(cube, **k)\n    n = k[\"max_breaks\"]\n    \
             mags = np.stack([res[f\"magnitude_{{j}}\"].values.reshape(-1) for j in range(1, n + 1)])\n    \
             dates = np.stack([res[f\"break_date_{{j}}\"].values.reshape(-1) for j in range(1, n + 1)])\n    \
             return largest_year(mags, dates, res[\"valid\"].values.reshape(-1) == 1)\n",
            a = f(0)?
        ),
        "bfast_monitor" => format!(
            "def map_years(cube, kw):\n    \"\"\"The year of the break the monitoring found, where the history was valid.\"\"\"\n    \
             res = {a}(cube, MONITOR_START, **kw[\"{a}\"])\n    \
             ok = (res[\"valid\"].values.reshape(-1) == 1) & (res[\"has_break\"].values.reshape(-1) == 1)\n    \
             return np.where(ok, year_of(res[\"break_date\"].values.reshape(-1)), np.nan)\n",
            a = f(0)?
        ),
        "ewmacd" => format!(
            "def map_years(cube, kw):\n    \"\"\"The year of the first disturbance after the training years.\"\"\"\n    \
             res = {a}(cube, **kw[\"{a}\"])\n    when = res[\"break_time\"].values.astype(float).reshape(-1)\n    \
             return np.where(np.isfinite(when) & (when > 0), np.floor(when), np.nan)\n",
            a = f(0)?
        ),
        _ => return None,
    })
}

/// The statements of the Python file's reading of one point: what `pixel_script` writes for a pin of the first
/// pixel, from its reads to the tool's call, with the pixel as the function's arguments. Also what the script
/// needs installed.
fn read_point_code(spec: &TuneScriptSpec, params: &Params, tool_code: &str) -> Result<(String, String), String> {
    let tools = [(spec.tool.to_string(), params.clone())];
    let pin = Spec {
        info: spec.info,
        version: spec.version,
        name: spec.name,
        target: Target::Pin { id: 1, col: 0, row: 0 },
        roles: spec.roles,
        tools: &tools,
        path: spec.path,
        other: None,
        chain: &[],
    };
    let text = pixel_script(&pin)?;
    let needs = text
        .lines()
        .find_map(|l| l.strip_prefix("Needs: pip install "))
        .unwrap_or("zeit-cdts")
        .trim_matches('"')
        .to_string();
    let (_, body) = text.split_once("import zeit\n").ok_or("the reads of the series could not be written")?;
    let end = body.find(tool_code).ok_or("the reads of the series could not be told from the tool's call")?;
    let reads = body[..end].trim();
    if !reads.contains("pixel=(0, 0)") {
        return Err("this series is not read pixel by pixel (a NetCDF or Zarr variable): no Python loop of it".into());
    }
    let reads: Vec<String> = reads
        .replace("pixel=(0, 0)", "pixel=(col, row)")
        .lines()
        .filter(|l| !l.starts_with("# Pin 1:"))
        .map(str::to_string)
        .collect();
    Ok((reads.join("\n"), needs))
}

/// The Python of the tuning (see the module's documentation), or why there is none.
pub fn tune_script(spec: &TuneScriptSpec) -> Result<String, String> {
    if spec.points.is_empty() {
        return Err("no points to write the loop for".into());
    }
    if spec.sets.is_empty() {
        return Err("no sets to write the loop for".into());
    }
    if !spec.info.time_is_date {
        return Err("a series without dates is not in Copy Python: it needs the dates Zeit reads".into());
    }
    let tool = crate::manifest().tool(spec.tool).ok_or_else(|| format!("no tool {}", spec.tool))?;
    // the Zeit calls of every set; the first one's are the base the others differ from
    let calls_of = |values: &[Value]| -> Result<Vec<CallData>, String> {
        let mut given = spec.base.clone();
        for (id, v) in spec.vary.iter().zip(values) {
            given.insert(id.clone(), v.clone());
        }
        let p = Params::with_defaults(spec.tool, &given).ok_or("its parameters are not the tool's")?;
        let c = tool_call(spec.tool, &p, spec.input)
            .ok_or_else(|| format!("{}: Zeit's call cannot be written for this series", tool.name))?;
        if c.inputs != ["series"] {
            return Err(format!("{}: the Python loop of the tools of bands is not written yet", tool.name));
        }
        Ok(c.data())
    };
    let base = calls_of(&spec.sets[0].values)?;
    let Some(functions) = tune_functions(spec.tool, &base) else {
        return Err(format!("{}: the Python loop of this tool is not written yet", tool.name));
    };
    let mut given = spec.base.clone();
    for (id, v) in spec.vary.iter().zip(&spec.sets[0].values) {
        given.insert(id.clone(), v.clone());
    }
    let base_params = Params::with_defaults(spec.tool, &given).ok_or("its parameters are not the tool's")?;
    let tool_code = tool_call(spec.tool, &base_params, spec.input).map(|c| c.compact("").0).unwrap_or_default();
    let (reads, needs) = read_point_code(spec, &base_params, &tool_code)?;

    let mut o = String::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = crate::numeric::civil(now.div_euclid(86400));
    let _ = writeln!(
        o,
        "\"\"\"Janus {} ({y:04}-{m:02}-{d:02}): the search of {}'s parameters on samples (Tune on samples) over {}, \
         redone outside Janus with Zeit.\nReference: {}\nNeeds: pip install \"{}\"\n\"\"\"",
        spec.version,
        tool.name,
        spec.name,
        spec.reference.replace('\n', " "),
        needs
    );
    o += "import numpy as np\nimport xarray as xr\n";
    if reads.contains("pd.") {
        o += "import pandas as pd\n";
    }
    o += "import zeit\n\n";
    let _ = writeln!(
        o,
        "SEARCH = True  # False to skip the search (it runs every set on every point)\n\
         N_JOBS = 4  # the threads Zeit uses for each set (the sets run one after the other)\n"
    );
    // the reference
    let _ = writeln!(
        o,
        "# The reference points: the column and row of the series' pixels, the year of the change (None: no change)\n\
         # and the weight the point has when the scores are weighted by the strata.\nPOINTS = ["
    );
    for p in spec.points {
        let year = p.reference.map_or("None".to_string(), |y| format!("{}", y.floor() as i64));
        let _ = writeln!(o, "    ({}, {}, {year}, {}),", p.col, p.row, py_float(p.weight));
    }
    let _ = writeln!(
        o,
        "]\nYEARS = ({}, {})  # a change counts only in these years, in the reference and in the map\n\
         TOLERANCE = {}  # a found change's dates agree when the calendar years differ by at most this\n\
         WEIGHTED = {}\n",
        spec.opts.window.0,
        spec.opts.window.1,
        spec.opts.tolerance,
        if spec.opts.weighted { "True" } else { "False" }
    );
    // the series
    let _ = writeln!(o, "\ndef read_point(col, row):\n    \"\"\"The series at a pixel, as Janus reads it.\"\"\"");
    for l in reads.lines() {
        if l.is_empty() {
            o.push('\n');
        } else {
            let _ = writeln!(o, "    {l}");
        }
    }
    o += "    return series\n\n\n";
    o += "def read_points():\n    \"\"\"Every point's series as one (time, 1, points) cube: the engine's row of pixels.\"\"\"\n    \
          each = [read_point(col, row) for col, row, _, _ in POINTS]\n    \
          values = np.stack([np.asarray(s.values, dtype=float).reshape(-1) for s in each], axis=1)\n    \
          return xr.DataArray(values[:, None, :], dims=(\"time\", \"y\", \"x\"), coords={\"time\": each[0].time.values})\n\n\n";
    // the sets
    let _ =
        writeln!(o, "# What the search varies, and its sets: the values of the varied parameters, and the keywords of");
    let _ =
        writeln!(o, "# the Zeit calls that differ from the first set's (the Janus table's dated F1, to compare with).");
    let _ = writeln!(o, "VARIED = [{}]", spec.vary.iter().map(|v| py_str(v)).collect::<Vec<_>>().join(", "));
    let base_kw: Vec<String> = base.iter().map(|c| format!("    {}: {},", py_str(&c.func), py_dict(&c.kw))).collect();
    let _ = writeln!(o, "BASE = {{\n{}\n}}", base_kw.join("\n"));
    if let Some(p) = base.first().and_then(|c| (spec.tool == "bfast_monitor").then(|| c.pos.get(1).cloned())).flatten()
    {
        let _ = writeln!(o, "MONITOR_START = {p}  # the decimal year the monitoring starts at");
    }
    let _ = writeln!(o, "SETS = [");
    for s in spec.sets {
        let calls = calls_of(&s.values)?;
        if calls.len() != base.len() {
            return Err("the sets' Zeit calls differ in number".into());
        }
        let mut over: Vec<String> = vec![];
        for (c, b) in calls.iter().zip(&base) {
            let diff: Vec<(String, String)> = c.kw.iter().filter(|kv| !b.kw.contains(kv)).cloned().collect();
            if !diff.is_empty() {
                over.push(format!("{}: {}", py_str(&c.func), py_dict(&diff)));
            }
        }
        let values: Vec<String> = s.values.iter().map(py_value).collect();
        let _ = writeln!(
            o,
            "    ({}, [{}], {{{}}}, {}),",
            py_str(&s.name),
            values.join(", "),
            over.join(", "),
            s.f1_dated.filter(|v| v.is_finite()).map_or("None".to_string(), py_float)
        );
    }
    o += "]\n\n\n";
    o += &functions;
    o += &LOOP.replace("@TOOL@", &tool.name);
    Ok(o)
}

/// The Python functions of the loop: the calendar year of the map's change from the Zeit result (`map_years`, with its
/// helpers) and the engine's scores (`scores`, `rank_key`), over the globals POINTS, YEARS, TOLERANCE and WEIGHTED. None
/// for a tool this loop does not write. The benchmark (tools/bench_tune) runs these very functions.
pub fn tune_functions(tool: &str, base: &[CallData]) -> Option<String> {
    let map_years = map_years(tool, base)?;
    let mut o = String::from(YEAR_OF);
    if map_years.contains("largest_year") {
        o += LARGEST_YEAR;
    }
    o += &map_years;
    o += "\n\n";
    o += SCORES;
    Some(o)
}

/// The calendar year of a break's date.
const YEAR_OF: &str = r#"def year_of(dates):
    """The calendar year of datetime64 dates (NaN for a missing one)."""
    years = dates.astype("datetime64[Y]").astype(np.int64) + 1970
    return np.where(np.isnat(dates), np.nan, years)


"#;

/// The break the map shows: the one of the largest magnitude.
const LARGEST_YEAR: &str = r#"def largest_year(mags, dates, valid):
    """The year of the break of the largest |magnitude| (the first one on a tie) that has a date, NaN where the series
    was not valid or has none."""
    years = year_of(dates)
    score = np.where(np.isfinite(mags) & np.isfinite(years), np.abs(mags), -1.0)
    k = score.argmax(axis=0)
    pick = np.take_along_axis(years, k[None, :], axis=0)[0]
    found = (np.take_along_axis(score, k[None, :], axis=0)[0] >= 0) & valid
    return np.where(found, pick, np.nan)


"#;

/// The scores of the engine (`tune::Scores`), written the same way.
const SCORES: &str = r#"REF = np.array([np.nan if p[2] is None else float(np.floor(p[2])) for p in POINTS])
WEIGHT = np.array([p[3] for p in POINTS]) if WEIGHTED else np.ones(len(POINTS))


def ratio(a, b, empty):
    return a / b if b > 0 else empty


def inside(year):
    return np.isfinite(year) & (year >= YEARS[0]) & (year <= YEARS[1])


def scores(year):
    """The scores of a set from the calendar year of its map's change at each point (NaN: none): the points
    count (weighted when WEIGHTED). A change is a reference change (its year is inside YEARS) or a map change;
    omission = the reference changes the map has none at; commission = the map changes the reference has none
    at; f1 = 2 TP / (2 TP + FP + FN), the dates apart; agreement = the share of the TP whose years differ by at
    most TOLERANCE; error = their mean |difference|; f1_dated counts a TP with a date beyond the tolerance as both
    an omission and a false change."""
    r_in, m_in = inside(REF), inside(year)
    both = r_in & m_in
    tp = WEIGHT[both].sum()
    fn = WEIGHT[r_in & ~m_in].sum()
    fp = WEIGHT[~r_in & m_in].sum()
    gap = np.abs(REF - year)
    agree = WEIGHT[both & (gap <= TOLERANCE)].sum()
    error = (WEIGHT * np.where(both, gap, 0.0)).sum()
    nan = float("nan")
    return dict(
        omission=ratio(fn, tp + fn, nan),
        commission=ratio(fp, tp + fp, 0.0),
        f1=ratio(2 * tp, 2 * tp + fp + fn, nan),
        f1_dated=ratio(2 * agree, 2 * agree + fp + fn + 2 * (tp - agree), nan),
        agreement=ratio(agree, tp, nan),
        error=ratio(error, tp, nan),
    )


def rank_key(s, i):
    """The table's order: the higher dated F1, then F1, then the agreement, then the smaller error; NaN last."""
    def k(v, sign):
        return (1, 0.0) if np.isnan(v) else (0, -sign * v)
    return (k(s["f1_dated"], 1), k(s["f1"], 1), k(s["agreement"], 1), k(s["error"], -1), i)


"#;

/// The loop and the table.
const LOOP: &str = r#"if SEARCH:
    cube = read_points()
    rows = []
    for i, (name, values, over, janus) in enumerate(SETS):
        kw = {f: {**BASE[f], **over.get(f, {})} for f in BASE}
        for k in kw.values():
            if "n_jobs" in k:
                k["n_jobs"] = N_JOBS
        rows.append((name, values, scores(map_years(cube, kw)), janus))
    print(f"@TOOL@ on {len(POINTS)} points, {len(SETS)} sets (the table is ordered by the dated F1)")
    head = " ".join(f"{v:>14}" for v in VARIED)
    print(f"{'rank':>4} {'set':<10} {head}  omission commission     F1 F1 dated  dates   error")
    pct = lambda v: "      -" if np.isnan(v) else f"{100 * v:6.1f}%"
    num = lambda v: "     -" if np.isnan(v) else f"{v:6.3f}"
    for rank, i in enumerate(sorted(range(len(rows)), key=lambda i: rank_key(rows[i][2], i)), 1):
        name, values, s, _ = rows[i]
        vals = " ".join(f"{v:>14.5g}" if isinstance(v, float) else f"{str(v):>14}" for v in values)
        print(f"{rank:>4} {name:<10} {vals}  {pct(s['omission'])}   {pct(s['commission'])} {num(s['f1'])}   {num(s['f1_dated'])} "
              f"{pct(s['agreement'])} {num(s['error'])}")
    gaps = [abs(s["f1_dated"] - janus) for _, _, s, janus in rows if janus is not None and np.isfinite(s["f1_dated"])]
    if gaps:
        print(f"the dated F1 differs from Janus's table by {max(gaps):.2g} at most")
"#;
