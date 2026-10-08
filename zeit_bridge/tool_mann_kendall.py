"""Mann-Kendall trend test + Theil-Sen slope (Zeit) for Janus."""
import math

import numpy as np

METHODS = ["hamed_rao", "original", "yue_wang", "seasonal"]

MANIFEST = {
    "id": "mann_kendall",
    "name": "Mann-Kendall trend",
    "category": "Trend",
    "description": (
        "Mann-Kendall monotonic trend test with Sen's slope, ported from pymannkendall to C++ in "
        "Zeit. Hamed-Rao and Yue-Wang correct the variance for serial correlation (usual in EO "
        "composites); the seasonal variant (Hirsch & Slack) tests sub-annual series per season. "
        "The first map shows Sen's slope only where the trend is significant."),
    "requires": {"time": "any", "min_dates": 4},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "method", "label": "Method", "type": "enum", "default": "hamed_rao", "options": METHODS,
         "labels": ["Hamed-Rao (autocorrelation-corrected)", "Original", "Yue-Wang (autocorrelation-corrected)",
                    "Seasonal (Hirsch & Slack)"],
         "help": "Hamed-Rao is the usual choice for annual EO composites, which are serially correlated. "
                 "Seasonal pools per-season tests for sub-annual series."},
        {"id": "alpha", "label": "Significance level", "type": "float", "default": 0.05, "min": 0.0001,
         "max": 0.5, "help": "A trend is significant when its p-value is below this."},
        {"id": "lag", "label": "Autocorrelation lags", "type": "int", "default": -1, "min": -1, "max": 10000,
         "help": "Hamed-Rao / Yue-Wang only: number of first lags used by the correction (-1 = all)."},
        {"id": "period", "label": "Seasons per year", "type": "int", "default": 0, "min": 0, "max": 366,
         "help": "Seasonal method only: observations per cycle (12 monthly, 23 MODIS 16-day). "
                 "0 = automatic, from the dates."},
        {"id": "min_valid", "label": "Min valid observations", "type": "int", "default": 4, "min": 3,
         "max": 10000, "help": "Pixels with fewer valid observations are left empty (raster runs)."},
    ],
    "outputs": [
        {"id": "sig_slope", "name": "Significant Sen's slope", "colormap": "BrBG", "unit": "value/step"},
        {"id": "slope", "name": "Sen's slope", "colormap": "BrBG", "unit": "value/step"},
        {"id": "p", "name": "p-value", "colormap": "Plasma", "unit": "p"},
        {"id": "z", "name": "Z statistic", "colormap": "BrBG", "unit": "z"},
        {"id": "tau", "name": "Kendall's tau", "colormap": "BrBG", "unit": "tau"},
        {"id": "trend", "name": "Trend (-1, 0, +1)", "colormap": "RdBu", "unit": "class"},
        {"id": "intercept", "name": "Intercept", "colormap": "Viridis", "unit": "value"},
    ],
}

# Row order of Zeit's batch output (zeit/trend.py MK_METRIC_NAMES).
ROWS = ["trend", "h", "p", "z", "tau", "s", "var_s", "slope", "intercept"]


def _args(p, ctx):
    from zeit._core.mannkendall import MKMethod

    code = {"original": MKMethod.ORIGINAL, "hamed_rao": MKMethod.HAMED_RAO,
            "yue_wang": MKMethod.YUE_WANG, "seasonal": MKMethod.SEASONAL}[p["method"]]
    period = int(p["period"]) or int(ctx["per_year"])
    return int(code), float(p["alpha"]), int(p["lag"]), max(1, period)


def _unit(p, ctx):
    if p["method"] == "seasonal":
        return "/cycle"
    return "/yr" if ctx["per_year"] == 1 else "/step"


def pixel(p, ctx):
    from zeit._core.mannkendall import mk_test_single

    values = [float(v) for v in ctx["values"]]
    n_valid = sum(1 for v in values if math.isfinite(v))
    if n_valid < max(3, int(p["min_valid"])):
        return {"overlays": [], "rows": [["Mann-Kendall", f"not enough data ({n_valid} valid)"]]}
    method, alpha, lag, period = _args(p, ctx)
    trend, h, pv, z, tau, s, var_s, slope, intercept = mk_test_single(values, method, alpha, lag, period)

    def fmt(x):
        return f"{x:.5g}" if x is not None and math.isfinite(x) else "-"

    unit = _unit(p, ctx)
    rows = [
        ["Trend", "increasing" if trend > 0 else "decreasing" if trend < 0 else "no trend"],
        [f"Significant (alpha {alpha:g})", "yes" if h else "no"],
        ["p-value", fmt(pv)],
        ["Z", fmt(z)],
        ["Kendall's tau", fmt(tau)],
        ["Sen's slope", fmt(slope) + unit],
        ["S / Var(S)", f"{fmt(s)} / {fmt(var_s)}"],
    ]
    overlays = []
    if math.isfinite(slope) and math.isfinite(intercept):
        # Sen's line over the series index (per cycle for the seasonal method).
        years = ctx["years"]
        step = 1.0 / period if p["method"] == "seasonal" else 1.0
        xs = [years[0], years[-1]]
        ys = [intercept, intercept + slope * (len(years) - 1) * step]
        overlays.append({"type": "line", "label": "Sen's slope", "x": xs, "y": ys})
    return {"overlays": overlays, "rows": rows}


def chunk(p, stack, ctx):
    from zeit._core.mannkendall import fit_mann_kendall_batch

    T, rows, cols = stack.shape
    method, alpha, lag, period = _args(p, ctx)
    values = np.ascontiguousarray(stack.reshape(T, rows * cols).T)
    out = fit_mann_kendall_batch(values, method, alpha, lag, period, int(p["min_valid"]), ctx.get("n_jobs", -1))
    out = np.asarray(out, dtype=np.float32).reshape(len(ROWS), rows, cols)
    r = {name: out[i] for i, name in enumerate(ROWS)}
    sig = r["slope"].copy()
    sig[~(r["h"] > 0.5)] = np.nan  # only where the trend is significant
    return {"sig_slope": sig, "slope": r["slope"], "p": r["p"], "z": r["z"], "tau": r["tau"],
            "trend": r["trend"], "intercept": r["intercept"]}


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
