"""Smoothing (zeit.smooth) for Janus: Whittaker or Savitzky-Golay curve over the chart.

Whittaker follows the real spacing of the dates (uneven dates are fine) and
fills gaps (NaN, e.g. masked clouds) with the curve; Savitzky-Golay assumes
evenly spaced dates and interpolates gaps linearly in time first. A series
without dates is taken as evenly spaced.
"""
import math

import numpy as np

import zeit_common as zc

MANIFEST = {
    "id": "smooth",
    "name": "Smoothing",
    "category": "Preprocessing",
    "description": (
        "Smoothed curve of the series on the chart, with Zeit's Whittaker smoother (penalises "
        "roughness, follows the real spacing of the dates and fills gaps with the curve) or "
        "Savitzky-Golay filter (local polynomial over evenly spaced dates; gaps are interpolated first)."),
    "requires": {"time": "any", "min_dates": 5},
    "modes": ["pixel"],
    "params": [
        {"id": "method", "label": "Method", "type": "enum", "default": "whittaker",
         "options": ["whittaker", "savgol"], "labels": ["Whittaker", "Savitzky-Golay"],
         "help": "Whittaker handles gaps and uneven dates and is a common choice for vegetation indices."},
        {"id": "lmbda", "label": "Whittaker lambda", "type": "float", "default": 10.0, "min": 0.001,
         "max": 1e7, "help": "Larger = smoother (10 is light for a 16-day series, 1000 strong). Its meaning "
                             "does not depend on the spacing of the dates."},
        {"id": "window", "label": "S-G window (dates)", "type": "int", "default": 7, "min": 3,
         "max": 999, "help": "Odd number of observations in each local fit (made odd and capped at the "
                             "series length)."},
        {"id": "polyorder", "label": "S-G polynomial order", "type": "int", "default": 2, "min": 0,
         "max": 6, "help": "Order of the local polynomial (below the window length)."},
    ],
    "outputs": [],
}


def pixel(p, ctx):
    import zeit

    vals = np.array(ctx["values"], dtype=np.float64)
    years = np.array(ctx["years"], dtype=np.float64)
    T = len(vals)
    ok = np.isfinite(vals)
    n = int(ok.sum())
    if n < 3:
        return {"overlays": [], "rows": [["Smoothing", f"not enough data ({n} valid)"]]}
    gaps = T - n
    data = zc.cube(zc.pixel_stack(ctx), ctx, synthetic_dates=True)
    if p["method"] == "savgol":
        wl = int(p["window"])
        wl = min(wl if wl % 2 else wl + 1, T if T % 2 else T - 1)
        po = max(0, min(int(p["polyorder"]), wl - 1))
        if wl < 3:
            return {"overlays": [], "rows": [["Smoothing", "series too short for Savitzky-Golay"]]}
        sm = zeit.smooth(data, method="savgol", window=wl, polyorder=po, nodata=None)
        label = f"Smoothed (Savitzky-Golay {wl}/{po})"
    else:
        lmbda = float(p["lmbda"])
        sm = zeit.smooth(data, method="whittaker", lmbda=lmbda, nodata=None, n_jobs=1)
        label = f"Smoothed (Whittaker λ={lmbda:g})"
    sm = np.asarray(sm.values, dtype=np.float64)[:, 0, 0]
    res = vals[ok] - sm[ok]
    rmse = float(np.sqrt(np.nanmean(res ** 2))) if n else float("nan")
    fin = np.isfinite(sm)
    rows = [["Residual RMSE", f"{rmse:.4g}" if math.isfinite(rmse) else "-"],
            ["Gaps " + ("filled by the curve" if p["method"] == "whittaker" else "interpolated"), str(gaps)]]
    if fin.any():
        k = int(np.nanargmax(np.where(fin, sm, -np.inf)))
        rows.append(["Smoothed max", f"{sm[k]:.4g} ({years[k]:.2f})"])
    return {"overlays": [{"type": "line", "label": label, "x": [float(x) for x in years[fin]],
                          "y": [float(v) for v in sm[fin]]}],
            "rows": rows}


TOOLS = [{"manifest": MANIFEST, "pixel": pixel}]
