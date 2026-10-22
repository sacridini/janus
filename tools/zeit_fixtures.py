"""The reference of Janus's native Zeit (crates/janus-zeit): what the Python bridge
(zeit_bridge/) answers, with Zeit's own Rust core, for pixel runs of every tool on
made-up series, written as JSON fixtures that `cargo test -p janus-zeit` compares
with the native runs.

    python tools/zeit_fixtures.py [OUT_DIR] [TOOL...]

OUT_DIR defaults to crates/janus-zeit/tests/data. Needs a Python with Zeit >= 2.0
(numpy, xarray, pandas); the manifest of every tool is written too (tools.json),
with the Zeit version it came from.
"""
import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "zeit_bridge"))

import numpy as np  # noqa: E402

import janus_zeit_bridge as bridge  # noqa: E402
import zeit_common as zc  # noqa: E402

sys.stdout = sys.__stdout__  # the bridge sends its own stdout to stderr


def jsonable(x):
    """NaN and infinities as null, numpy scalars and arrays as Python ones."""
    if isinstance(x, dict):
        return {k: jsonable(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [jsonable(v) for v in x]
    if isinstance(x, np.ndarray):
        return jsonable(x.tolist())
    if isinstance(x, (np.floating, float)):
        return float(x) if math.isfinite(x) else None
    if isinstance(x, np.integer):
        return int(x)
    return x


def annual(first=1985, last=2024, month_day="07-01"):
    """Decimal years and days since 1970 of one date a year."""
    days = np.array([f"{y}-{month_day}" for y in range(first, last + 1)], dtype="datetime64[D]")
    return zc.decimal_years(days).tolist(), days.astype(np.int64).tolist()


def annual_series(rng, n):
    """Annual NDVI-like series: stable, disturbed and recovering, gradual decline, noisy,
    with missing years; a few degenerate ones (short, nearly empty, all missing)."""
    out = []
    for k in range(n):
        t = np.arange(40, dtype=np.float64)
        base = 0.8 + rng.normal(0, 0.02, 40)
        kind = k % 5
        if kind == 1:  # a loss, then recovery
            at = int(rng.integers(5, 30))
            depth = rng.uniform(0.2, 0.6)
            base[at:] -= depth * np.exp(-(t[at:] - at) / rng.uniform(2, 10))
        elif kind == 2:  # gradual decline
            base -= np.clip(t - rng.integers(5, 20), 0, None) * rng.uniform(0.005, 0.02)
        elif kind == 3:  # two losses
            for at in sorted(rng.choice(np.arange(5, 35), 2, replace=False)):
                base[at:at + 4] -= rng.uniform(0.2, 0.4)
        elif kind == 4:  # a gain (planting)
            at = int(rng.integers(5, 30))
            base[:at] -= 0.4
        base += rng.normal(0, rng.uniform(0.005, 0.05), 40)
        gaps = rng.random(40) < rng.uniform(0.0, 0.25)
        base[gaps] = np.nan
        out.append(base)
    short = np.full(40, np.nan)
    short[10:14] = [0.8, 0.5, 0.6, 0.7]
    out += [short, np.full(40, np.nan), np.r_[np.full(20, 0.8), np.full(20, 0.3)]]
    return out


def landtrendr_cases(rng):
    years, days = annual()
    variants = [
        {},
        {"event_type": "gain"},
        {"sort_by": "newest", "max_segments": 3},
        {"sort_by": "fastest", "min_magnitude": 0.1},
        {"sort_by": "longest", "min_duration": 2, "spike_threshold": 1.0},
        {"sort_by": "dsnr", "pre_val_threshold": 0.7, "recovery_threshold": 1.0},
        {"pval_threshold": 0.2, "best_model_proportion": 1.0, "vertex_count_overshoot": 1},
        {"min_observations_needed": 20},
    ]
    cases = []
    for i, values in enumerate(annual_series(rng, 40)):
        params = variants[i % len(variants)]
        cases.append({"params": params, "years": years, "days": days, "values": values})
    return cases


def monthly(first=2010, years=10):
    days = np.array([f"{y}-{m:02d}-15" for y in range(first, first + years) for m in range(1, 13)],
                    dtype="datetime64[D]")
    return zc.decimal_years(days).tolist(), days.astype(np.int64).tolist()


def monthly_series(rng, n, years=10):
    """Seasonal series with a trend (up, down or none) and gaps."""
    out = []
    t = np.arange(12 * years)
    for k in range(n):
        v = 0.5 + 0.2 * np.sin(2 * np.pi * t / 12 + rng.uniform(0, 6)) + (k % 3 - 1) * rng.uniform(0, 0.003) * t
        v += rng.normal(0, rng.uniform(0.01, 0.08), t.size)
        v[rng.random(t.size) < rng.uniform(0, 0.3)] = np.nan
        out.append(v)
    return out


def mann_kendall_cases(rng):
    cases = []
    years, days = annual()
    variants = [{}, {"method": "original"}, {"method": "yue_wang", "lag": 3}, {"alpha": 0.2, "lag": 0},
                {"min_valid": 30}]
    for i, values in enumerate(annual_series(rng, 20)):
        cases.append({"params": variants[i % len(variants)], "years": years, "days": days, "values": values})
    m_years, m_days = monthly()
    variants = [{"method": "seasonal"}, {"method": "seasonal", "period": 6}, {}, {"method": "original"}]
    for i, values in enumerate(monthly_series(rng, 12)):
        cases.append({"params": variants[i % len(variants)], "years": m_years, "days": m_days, "values": values})
    return cases


def sixteen_day(rng, first=2018, n=69):
    """16-day dates with a few missing (uneven spacing)."""
    days = np.datetime64(f"{first}-01-01") + np.arange(n) * 16
    days = days[rng.random(n) > 0.1]
    return zc.decimal_years(days).tolist(), days.astype(np.int64).tolist()


def smooth_cases(rng):
    cases = []
    years, days = annual()
    variants = [{}, {"lmbda": 1000.0}, {"method": "savgol"}, {"method": "savgol", "window": 8, "polyorder": 3},
                {"lmbda": 0.5}, {"method": "savgol", "window": 999, "polyorder": 6}]
    for i, values in enumerate(annual_series(rng, 12)):
        cases.append({"params": variants[i % len(variants)], "years": years, "days": days, "values": values})
    for i in range(12):
        s_years, s_days = sixteen_day(rng)
        t = np.array(s_days, dtype=np.float64)
        v = 0.5 + 0.25 * np.sin(2 * np.pi * t / 365.25 + rng.uniform(0, 6)) + rng.normal(0, 0.04, t.size)
        v[rng.random(t.size) < 0.2] = np.nan
        cases.append({"params": variants[i % len(variants)], "years": s_years, "days": s_days, "values": v})
    # no real dates: evenly spaced; and short series
    m_years, _ = monthly(2015, 2)
    for i, values in enumerate(monthly_series(rng, 4, 2)):
        cases.append({"params": variants[i % 3], "years": m_years, "days": None, "values": values})
    for t in (3, 4, 5):
        y, d = annual(2000, 2000 + t - 1)
        cases.append({"params": {"method": "savgol"}, "years": y, "days": d, "values": rng.uniform(0, 1, t)})
        cases.append({"params": {}, "years": y, "days": d, "values": rng.uniform(0, 1, t)})
    return cases


def regular(first="2015-01-01", step=16, n=184):
    days = np.datetime64(first) + np.arange(n) * step
    return zc.decimal_years(days).tolist(), days.astype(np.int64).tolist()


def seasonal_breaks(rng, days, k):
    """A seasonal index with an abrupt drop (or two, or none), noise and cloudy gaps."""
    t = np.asarray(days, dtype=np.float64)
    v = 0.6 + 0.2 * np.sin(2 * np.pi * t / 365.25 + rng.uniform(0, 6)) + rng.normal(0, 0.03, t.size)
    for at in rng.choice(np.arange(20, t.size - 20), k % 3, replace=False):
        v[at:] -= rng.uniform(0.15, 0.4)
    if k % 4 == 3:
        v += np.linspace(0, rng.uniform(-0.2, 0.2), t.size)
    v[rng.random(t.size) < rng.uniform(0, 0.25)] = np.nan
    return v


def bfast_family_cases(variants, monthly_too=True):
    def cases(rng):
        out = []
        for k in range(16):
            years, days = regular(n=int(rng.integers(60, 190)))
            out.append({"params": variants[k % len(variants)], "years": years, "days": days,
                        "values": seasonal_breaks(rng, days, k)})
        if monthly_too:
            for k in range(6):
                years, days = monthly(2012, 8)
                out.append({"params": variants[k % len(variants)], "years": years, "days": days,
                            "values": seasonal_breaks(rng, days, k)})
        years, days = regular(n=30)  # short
        out.append({"params": {}, "years": years, "days": days, "values": seasonal_breaks(rng, days, 0)})
        return out
    return cases


CASES = {
    "landtrendr": landtrendr_cases,
    "mann_kendall": mann_kendall_cases,
    "smooth": smooth_cases,
    "bfast": bfast_family_cases([{}, {"h": 0.25, "max_breaks_trend": 2}, {"order": 1, "level": 0.2},
                                 {"max_iter": 2, "max_breaks_season": 1}, {"min_valid": 150}]),
    "bfast_lite": bfast_family_cases([{}, {"max_breaks": 2, "order": 2}, {"h": 0.3}, {"min_valid": 150}]),
    "bfast_monitor": bfast_family_cases([{}, {"monitor_start": 2019.5}, {"h": "0.5", "period": "4", "alpha": "0.01"},
                                         {"monitor_start": 2016.0, "min_valid": 30}, {"h": "1.0", "order": 1}]),
}


def main():
    args = sys.argv[1:]
    out_dir = args.pop(0) if args and not args[0] in CASES else os.path.join(HERE, "..", "crates", "janus-zeit",
                                                                           "tests", "data")
    wanted = args or list(CASES)
    os.makedirs(out_dir, exist_ok=True)
    manifest = bridge.manifest()
    with open(os.path.join(out_dir, "tools.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(jsonable(manifest), f, indent=1)
        f.write("\n")
    tools = bridge.tools()
    for tool in wanted:
        entry = tools[tool]
        rng = np.random.default_rng(20261022)
        fixtures = []
        for case in CASES[tool](rng):
            p = bridge.defaults(entry["manifest"], case["params"])
            ctx = bridge.make_ctx(case["years"], case["days"], n_jobs=1)
            ctx["values"] = [float(v) for v in case["values"]]
            try:
                result = entry["pixel"](p, ctx)
            except Exception as e:  # the native run must fail too
                result = {"error": f"{type(e).__name__}: {e}"}
            fixtures.append({"params": case["params"], "years": case["years"], "days": case["days"],
                             "values": case["values"], "result": result})
        path = os.path.join(out_dir, f"{tool}.json")
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            json.dump(jsonable({"zeit": manifest.get("zeit_version"), "cases": fixtures}), f)
            f.write("\n")
        print(f"{tool}: {len(fixtures)} cases -> {path}")


if __name__ == "__main__":
    main()
