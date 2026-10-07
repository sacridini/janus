"""Smoothing (Zeit) for tsv: Whittaker or Savitzky-Golay curve over the chart.

Both smoothers in Zeit work on the observation index (equal steps), not on the
real dates. Gaps (NaN, e.g. masked clouds):
- Whittaker: weight 0 at the gap, so the curve is interpolated through it;
- Savitzky-Golay: needs a complete series, so gaps are filled by linear
  interpolation in time first. Zeit's apply_savgol_filter resets cells whose
  input is exactly 0 to 0 (it treats 0 as nodata); the series is shifted so it
  has no zero before filtering and shifted back after (the filter preserves
  constants, so the result is unchanged otherwise).
"""
import math

import numpy as np

MANIFEST = {
    "id": "smooth",
    "name": "Smoothing",
    "category": "Preprocessing",
    "description": (
        "Smoothed curve of the series on the chart, with Zeit's Whittaker smoother (penalises "
        "roughness; gaps get zero weight) or Savitzky-Golay filter (local polynomial; gaps are "
        "interpolated first). Both use equal steps between observations."),
    "requires": {"time": "any", "min_dates": 5},
    "modes": ["pixel"],
    "params": [
        {"id": "method", "label": "Method", "type": "enum", "default": "whittaker",
         "options": ["whittaker", "savgol"], "labels": ["Whittaker", "Savitzky-Golay"],
         "help": "Whittaker handles gaps naturally and is a common choice for vegetation indices."},
        {"id": "lmbd", "label": "Whittaker lambda", "type": "float", "default": 10.0, "min": 0.001,
         "max": 1e7, "help": "Larger = smoother (try 1-10 for 16-day series, 100+ for strong noise)."},
        {"id": "window_length", "label": "S-G window (dates)", "type": "int", "default": 7, "min": 3,
         "max": 999, "help": "Odd number of observations in each local fit (made odd and capped at the "
                             "series length)."},
        {"id": "polyorder", "label": "S-G polynomial order", "type": "int", "default": 2, "min": 0,
         "max": 6, "help": "Order of the local polynomial (below the window length)."},
    ],
    "outputs": [],
}


def _interp(vals, x):
    ok = np.isfinite(vals)
    return np.interp(x, x[ok], vals[ok])


def pixel(p, ctx):
    from zeit.smooth import apply_savgol_filter, apply_whittaker_filter

    vals = np.array(ctx["values"], dtype=np.float64)
    years = np.array(ctx["years"], dtype=np.float64)
    T = len(vals)
    ok = np.isfinite(vals)
    n = int(ok.sum())
    if n < 3:
        return {"overlays": [], "rows": [["Smoothing", f"not enough data ({n} valid)"]]}
    gaps = T - n
    if p["method"] == "savgol":
        wl = int(p["window_length"])
        wl = min(wl if wl % 2 else wl + 1, T if T % 2 else T - 1)
        po = max(0, min(int(p["polyorder"]), wl - 1))
        if wl < 3:
            return {"overlays": [], "rows": [["Smoothing", "series too short for Savitzky-Golay"]]}
        filled = _interp(vals, years)
        shift = 1.0 - float(np.min(filled))  # no exact zeros (see the module notes)
        sm = apply_savgol_filter((filled + shift)[:, None, None], window_length=wl, polyorder=po)[:, 0, 0] - shift
        label = f"Smoothed (Savitzky-Golay {wl}/{po})"
    else:
        lmbd = float(p["lmbd"])
        w = ok.astype(np.float64)
        y = np.where(ok, vals, 0.0)
        sm = apply_whittaker_filter(y[:, None, None], lmbd=lmbd, weights=w[:, None, None])[:, 0, 0]
        label = f"Smoothed (Whittaker λ={lmbd:g})"
    res = vals[ok] - sm[ok]
    rmse = float(np.sqrt(np.mean(res ** 2))) if n else float("nan")
    fin = np.isfinite(sm)
    rows = [["Residual RMSE", f"{rmse:.4g}" if math.isfinite(rmse) else "-"],
            ["Gaps " + ("weighted 0" if p["method"] == "whittaker" else "interpolated"), str(gaps)]]
    if fin.any():
        k = int(np.nanargmax(np.where(fin, sm, -np.inf)))
        rows.append(["Smoothed max", f"{sm[k]:.4g} ({years[k]:.2f})"])
    return {"overlays": [{"type": "line", "label": label, "x": [float(x) for x in years[fin]],
                          "y": [float(v) for v in sm[fin]]}],
            "rows": rows}


TOOLS = [{"manifest": MANIFEST, "pixel": pixel}]
