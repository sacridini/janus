"""Mann-Kendall trend test + Theil-Sen slope (zeit.mann_kendall) for Janus."""
import math

import numpy as np

import zeit_common as zc

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

def _period(p, ctx):
    return max(1, int(p["period"]) or int(ctx["per_year"]))


def _run(p, stack, ctx):
    """zeit.mann_kendall on a [T, rows, cols] chunk -> {variable: (rows, cols)}."""
    import zeit
    lag = int(p["lag"])
    ds = zeit.mann_kendall(zc.cube(stack, ctx), method=p["method"], alpha=float(p["alpha"]),
                           lag=None if lag < 0 else lag, period=_period(p, ctx), min_valid=int(p["min_valid"]),
                           nodata=None, n_jobs=ctx.get("n_jobs", -1))
    return {name: zc.grid(ds, name) for name in ds.data_vars}


def _unit(p, ctx):
    if p["method"] == "seasonal":
        return "/cycle"
    return "/yr" if ctx["per_year"] == 1 else "/step"


def pixel(p, ctx):
    values = [float(v) for v in ctx["values"]]
    n_valid = sum(1 for v in values if math.isfinite(v))
    if n_valid < max(3, int(p["min_valid"])):
        return {"overlays": [], "rows": [["Mann-Kendall", f"not enough data ({n_valid} valid)"]]}
    r = {k: float(v[0, 0]) for k, v in _run(p, zc.pixel_stack(ctx), ctx).items()}
    trend, h, pv, z, tau = r["trend"], r["h"], r["p"], r["z"], r["tau"]
    s, var_s, slope, intercept = r["s"], r["var_s"], r["slope"], r["intercept"]
    alpha, period = float(p["alpha"]), _period(p, ctx)

    def fmt(x):
        return f"{x:.5g}" if x is not None and math.isfinite(x) else "-"

    unit = _unit(p, ctx)
    rows = [
        ["Trend", "increasing" if trend > 0 else "decreasing" if trend < 0 else "no trend"],
        [f"Significant (alpha {alpha:g})", "yes" if h > 0.5 else "no"],
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
    r = _run(p, stack, ctx)
    sig = np.where(r["h"] > 0.5, r["slope"], np.nan)  # only where the trend is significant
    res = {"sig_slope": sig, "slope": r["slope"], "p": r["p"], "z": r["z"], "tau": r["tau"],
           "trend": r["trend"], "intercept": r["intercept"]}
    return zc.chunk_outputs(res, stack.shape[1:])


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
