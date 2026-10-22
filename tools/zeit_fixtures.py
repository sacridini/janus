"""The reference of Janus's native Zeit (crates/janus-zeit): what the Python bridge
(zeit_bridge/) answers, with Zeit's own Rust core, for pixel runs of every tool on
made-up series, written as JSON fixtures that `cargo test -p janus-zeit` compares
with the native runs.

    python tools/zeit_fixtures.py [--out DIR] [TOOL...]

DIR defaults to crates/janus-zeit/tests/data; without TOOLs, every tool. Needs a Python with Zeit >= 2.0
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


def landsat_series(rng, n=None, first="2014-01-05", step=16, disturb=True, days=None):
    """Six Landsat bands (reflectance x 10000) of a forest pixel every `step` days (or on `days`), a clearing in
    some, with Fmask codes (cloud, shadow, snow, missing) and clouds and shadows Fmask missed; and the NDVI Janus
    shows."""
    if days is None:
        n = n or int(rng.integers(60, 200))
        days = np.datetime64(first) + np.arange(n) * step
        days = days[rng.random(n) > 0.05]
    n = days.size
    t = days.astype(np.int64).astype(np.float64)
    season = np.sin(2 * np.pi * t / 365.25 + rng.uniform(0, 6))
    base = {"blue": 300, "green": 550, "red": 350, "nir": 3200, "swir1": 1500, "swir2": 700}
    amp = {"blue": 30, "green": 60, "red": 60, "nir": 400, "swir1": 150, "swir2": 80}
    bands = {r: base[r] + amp[r] * season + rng.normal(0, 0.03 * base[r], n) for r in base}
    if disturb and rng.random() < 0.7:  # a clearing: soil
        at = int(rng.integers(n // 4, 3 * n // 4))
        strength = rng.uniform(0.15, 1.0)  # light: degradation (still forest after it); strong: deforestation
        for r, d in {"blue": 400, "green": 500, "red": 900, "nir": -1400, "swir1": 1300, "swir2": 1100}.items():
            bands[r][at:] += strength * d * np.exp(-(t[at:] - t[at]) / rng.uniform(300, 3000))
    qa = np.zeros(n)
    kind = rng.random(n)
    cloud, shadow = kind < 0.12, (kind >= 0.12) & (kind < 0.18)
    missed_cloud, missed_shadow = (kind >= 0.18) & (kind < 0.22), (kind >= 0.22) & (kind < 0.25)
    bright = {"blue": 2500, "green": 2500, "red": 2500, "nir": 2000, "swir1": 1500, "swir2": 1200}
    for r in bands:
        c = cloud | missed_cloud
        bands[r][c] += bright[r] * rng.uniform(0.3, 1.0, c.sum())
        bands[r][shadow | missed_shadow] *= 0.4
    qa[cloud] = 4
    qa[shadow] = 2
    qa[(kind >= 0.25) & (kind < 0.26)] = 3
    gone = (kind >= 0.26) & (kind < 0.29)
    for r in bands:
        bands[r][gone] = np.nan
    qa[gone] = 255
    nd = (bands["nir"] - bands["red"]) / (bands["nir"] + bands["red"])
    values = np.where(qa <= 1, nd, np.nan)
    return zc.decimal_years(days).tolist(), days.astype(np.int64).tolist(), bands, qa, values


def landsat_c2_qa(fmask):
    """Fmask codes as Landsat Collection 2 QA_PIXEL bits."""
    out = np.full(fmask.shape, 1 << 6, dtype=np.float64)  # clear
    out[fmask == 4] = (1 << 3) | (1 << 1)
    out[fmask == 2] = 1 << 4
    out[fmask == 3] = 1 << 5
    out[fmask == 255] = 1
    return out


def multiband_cases(roles, variants, n_cases=20, step=16):
    def cases(rng):
        out = []
        for k in range(n_cases):
            years, days, bands, qa, values = landsat_series(rng, step=step)
            case = {"params": dict(variants[k % len(variants)]), "years": years, "days": days, "values": values,
                    "bands": {r: bands[r] for r in roles}, "qa": qa, "qa_rule": "fmask",
                    "shown": {"band": None, "nd": ["nir", "red"]}}
            units = k % 4
            if units == 1:  # reflectance 0-1
                case["bands"] = {r: v / 10000.0 for r, v in case["bands"].items()}
            elif units == 2:  # Landsat C2 L2 digital numbers, with its QA bits
                case["bands"] = {r: (v / 10000.0 + 0.2) / 2.75e-5 for r, v in case["bands"].items()}
                case["qa"], case["qa_rule"] = landsat_c2_qa(qa), "landsat_c2"
            elif k % 8 == 3:  # no quality band
                case["qa"], case["qa_rule"] = None, None
            out.append(case)
        years, days, bands, qa, values = landsat_series(rng, step=step)
        out.append({"params": {}, "years": years, "days": days, "values": values,
                    "bands": {r: bands[r] for r in roles[:-1]}, "qa": qa, "qa_rule": "fmask"})  # a role missing
        out.append({"params": {}, "years": years, "days": None, "values": values,
                    "bands": {r: bands[r] for r in roles}, "qa": qa, "qa_rule": "fmask"})  # no real dates
        years, days, bands, qa, values = landsat_series(rng, n=8, step=step)
        out.append({"params": {}, "years": years, "days": days, "values": values,
                    "bands": {r: bands[r] for r in roles}, "qa": qa, "qa_rule": "fmask"})  # few dates
        return out
    return cases


def phenology_cases(rng):
    curves = ["beck", "elmore", "gu", "zhang", "ag", "klos", "dl"]
    methods = ["trs5", "trs2", "trs6", "der", "zhang", "gu"]
    out = []
    for k in range(28):
        if k % 4 == 3:
            years, days = monthly(2014, int(rng.integers(2, 7)))
        else:
            first = f"{int(rng.integers(2005, 2016))}-0{int(rng.integers(1, 9))}-0{int(rng.integers(1, 9))}"
            years, days = regular(first=first, n=int(rng.integers(30, 180)))
        t = np.asarray(days, dtype=np.float64)
        phase = rng.uniform(0, 6)
        v = 0.35 + 0.3 * np.clip(np.sin(2 * np.pi * t / 365.25 + phase), -0.6, 1) + rng.normal(0, 0.02, t.size)
        if k % 7 == 5:  # two seasons a year
            v = 0.4 + 0.2 * np.sin(4 * np.pi * t / 365.25 + phase) + rng.normal(0, 0.02, t.size)
        if k % 9 == 8:  # barely varies
            v = 0.2 + rng.normal(0, 0.01, t.size)
        v[rng.random(t.size) < rng.uniform(0, 0.25)] = np.nan
        params = {"curve": curves[k % len(curves)], "method": methods[k % len(methods)],
                  "smoothing": ["whittaker", "hants", "none"][k % 3]}
        if k % 5 == 4:
            params.update({"lambda": 100.0, "min_amplitude": 0.05, "min_season_length": 60})
        out.append({"params": params, "years": years, "days": days, "values": v})
    years, days = regular(n=20)  # under a year
    out.append({"params": {}, "years": years, "days": days, "values": rng.uniform(0, 1, 20)})
    years, days = regular(n=40)  # no real dates
    out.append({"params": {}, "years": years, "days": None, "values": rng.uniform(0, 1, 40)})
    return out


def class_series(rng, kind, days):
    """A pixel of a land cover: forest (high, flat), crop (one season a year), pasture (low, weak season)."""
    t = np.asarray(days, dtype=np.float64)
    phase = rng.normal(0, 0.2)
    if kind == "forest":
        v = 0.82 + 0.03 * np.sin(2 * np.pi * t / 365.25 + phase)
    elif kind == "crop":
        v = 0.3 + 0.5 * np.clip(np.sin(2 * np.pi * t / 365.25 + 1.5 + phase), 0, 1)
    else:
        v = 0.45 + 0.1 * np.sin(2 * np.pi * t / 365.25 + 3 + phase)
    v = v + rng.normal(0, 0.03, t.size)
    v[rng.random(t.size) < 0.1] = np.nan
    return v


def twdtw_cases(rng):
    kinds = ["forest", "crop", "pasture"]
    out = []
    variants = [{}, {"cycle": "year"}, {"max_elapsed": 0}, {"max_distance": 2.0}, {"steepness": 1.0, "midpoint": 20.0},
                {"min_valid": 500}, {"max_elapsed": 30, "cycle": "year"}]
    for k in range(24):
        years, days = regular(first="2018-01-01", n=int(rng.integers(40, 90)))
        pats = []
        for j, kind in enumerate(kinds[: 1 + k % 3]):
            pv = class_series(rng, kind, days)
            pat = {"name": kind if k % 5 else "same", "from": f"pin {j + 1}", "years": years, "days": days,
                   "values": [None if not np.isfinite(x) else float(x) for x in pv]}
            if k % 4 == 1 and j == 0:  # a one-season pattern
                pat = {key: (val[:23] if isinstance(val, list) else val) for key, val in pat.items()}
            pats.append(pat)
        params = dict(variants[k % len(variants)], patterns=pats)
        out.append({"params": params, "years": years, "days": days,
                    "values": class_series(rng, kinds[k % 3], days)})
    years, days = regular(n=40)
    v = class_series(rng, "crop", days)
    bad = {"name": "x", "years": years, "days": None, "values": [0.5] * 40}
    out.append({"params": {"patterns": [bad]}, "years": years, "days": days, "values": v})  # a pattern without dates
    out.append({"params": {}, "years": years, "days": days, "values": v})  # no pattern
    good = {"name": "crop", "years": years, "days": days, "values": [float(x) for x in class_series(rng, "crop", days)]}
    out.append({"params": {"patterns": [good]}, "years": years, "days": None, "values": v})  # no real dates
    return out


def ccdc_cases(synthetic):
    def cases(rng):
        out = []
        variants = [{}, {"conseq_anom": 4, "num_c": "6"}, {"chi2_prob_threshold": 0.9, "max_segments": 3},
                    {"num_c": "4"}, {"tmax_cg_prob_threshold": 0.99}]
        shown = [{"band": None, "nd": ["nir", "red"]}, {"band": "nir", "nd": None}, {"band": "thermal", "nd": None},
                 None, {"band": None, "nd": ["nir", "swir1"]}]
        for k in range(22):
            years, days, bands, qa, values = landsat_series(rng, n=int(rng.integers(60, 230)))
            case = {"params": dict(variants[k % len(variants)]), "years": years, "days": days, "values": values,
                    "bands": dict(bands), "qa": qa, "qa_rule": "fmask", "shown": shown[k % len(shown)]}
            if k % 3 == 0:  # thermal: Kelvin, deg C or C2 ST digital numbers
                kelvin = 295.0 + 8.0 * np.sin(2 * np.pi * np.asarray(days) / 365.25) + rng.normal(0, 1, len(days))
                case["bands"]["thermal"] = [kelvin, kelvin - 273.15, (kelvin - 149.0) / 0.00341802][k % 9 // 3]
            if k % 4 == 1:
                case["bands"] = {r: v / 10000.0 if r != "thermal" else v for r, v in case["bands"].items()}
            if synthetic:
                case["params"]["date"] = [0.0, 2016.5, 2030.0, 2000.0][k % 4]
                case["map_year"] = [None, 2017.25][k % 2]
            out.append(case)
        years, days, bands, qa, values = landsat_series(rng, n=20)
        out.append({"params": {}, "years": years, "days": days, "values": values, "bands": bands, "qa": qa,
                    "qa_rule": "fmask"})  # too few clear observations
        return out
    return cases


CASES = {
    "ccdc": ccdc_cases(False),
    "ccdc_synthetic": ccdc_cases(True),
    "twdtw": twdtw_cases,
    "phenology": phenology_cases,
    "coded": multiband_cases(zc.LANDSAT_ROLES, [{}, {"monitor_start": 2017.0}, {"direction": "both", "max_events": 5},
                                                {"thresh": 2.0, "consec": 2, "forest_ndfi": 0.3}, {"cloud_threshold": 1.0},
                                                {"train_years": 1.5, "min_obs": 4, "min_years": 1.0}], n_cases=30),
    "ndfi": multiband_cases(zc.LANDSAT_ROLES, [{}, {"cloud_threshold": 1.0}, {"cloud_threshold": 0.2}]),
    "tmask": multiband_cases(["green", "swir1"], [{}, {"units": "x10000"}, {"write_series": False}]),
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
    out_dir = os.path.join(HERE, "..", "crates", "janus-zeit", "tests", "data")
    if args[:1] == ["--out"]:
        out_dir = args[1]
        args = args[2:]
    unknown = [a for a in args if a not in CASES]
    if unknown:
        sys.exit(f"no cases for {unknown}; tools: {sorted(CASES)}")
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
            ctx["shown"] = case.get("shown")
            ctx["map_year"] = case.get("map_year")
            if case.get("bands"):
                bridge.band_ctx(ctx, {r: [float(x) for x in v] for r, v in case["bands"].items()},
                                None if case.get("qa") is None else [float(x) for x in case["qa"]],
                                case.get("qa_rule"))
            try:
                result = entry["pixel"](p, ctx)
            except Exception as e:  # the native run must fail too
                result = {"error": f"{type(e).__name__}: {e}"}
            extra = {k: case[k] for k in ("bands", "qa", "qa_rule", "shown", "map_year") if k in case}
            fixtures.append(extra | {"params": case["params"], "years": case["years"], "days": case["days"],
                                     "values": case["values"], "result": result})
        path = os.path.join(out_dir, f"{tool}.json")
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            json.dump(jsonable({"zeit": manifest.get("zeit_version"), "cases": fixtures}), f)
            f.write("\n")
        print(f"{tool}: {len(fixtures)} cases -> {path}")


if __name__ == "__main__":
    main()
