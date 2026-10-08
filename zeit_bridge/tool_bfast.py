"""BFAST family (Zeit) for Janus: classic BFAST, BFAST Lite and BFAST Monitor.

All three fit trend + harmonic models on a regular series, so time is the
synthetic `start + i / per_year` grid of R's `ts` (Zeit's convention). Break
indices returned by Zeit are positions in the full series; they are mapped
back to the real dates in ctx["years"] for display and for the output maps.
"""
import math

import numpy as np

_ORDER = {"id": "order", "label": "Harmonic order", "type": "int", "default": 3, "min": 1, "max": 6,
          "help": "Number of sine/cosine pairs in the seasonal model (capped at observations per year)."}

_REQUIRES = {"time": "regular", "min_per_year": 2}

BFAST = {
    "id": "bfast",
    "name": "BFAST",
    "category": "Change detection",
    "description": (
        "Breaks For Additive Season and Trend (Verbesselt et al. 2010), as implemented in Zeit. "
        "Iteratively splits the series into trend and seasonal components and detects breaks in "
        "each. Needs a regular series with several observations per year and more than two years."),
    "requires": dict(_REQUIRES, min_dates=20),
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "h", "label": "Min segment size", "type": "float", "default": 0.15, "min": 0.05, "max": 0.5,
         "help": "Minimum segment length, as a fraction of the valid observations."},
        {"id": "max_breaks_trend", "label": "Max trend breaks", "type": "int", "default": 5, "min": 1, "max": 10,
         "help": "Maximum number of breaks searched for in the trend component."},
        {"id": "max_breaks_season", "label": "Max season breaks", "type": "int", "default": 5, "min": 1,
         "max": 10, "help": "Maximum number of breaks searched for in the seasonal component."},
        _ORDER,
        {"id": "level", "label": "Significance level", "type": "float", "default": 0.05, "min": 0.001,
         "max": 0.5, "help": "A component is only searched for breaks when the OLS-MOSUM stability "
                             "test rejects at this level."},
        {"id": "max_iter", "label": "Max iterations", "type": "int", "default": 10, "min": 1, "max": 50,
         "help": "Maximum trend/season re-estimation rounds (usually converges much earlier)."},
        {"id": "min_valid", "label": "Min observations", "type": "int", "default": 20, "min": 5, "max": 1000,
         "help": "Pixels with fewer valid observations are not fitted."},
    ],
    "outputs": [
        {"id": "n_trend_breaks", "name": "Trend breaks", "colormap": "Viridis", "unit": "count"},
        {"id": "n_season_breaks", "name": "Season breaks", "colormap": "Viridis", "unit": "count"},
        {"id": "break_date", "name": "Largest trend break date", "colormap": "Viridis", "unit": "year"},
        {"id": "magnitude", "name": "Largest trend break magnitude", "colormap": "RdBu", "unit": "value"},
    ],
    "chunk_cells": 20_000,  # ~1.6 ms/pixel: keep progress updates frequent
}

BFAST_LITE = {
    "id": "bfast_lite",
    "name": "BFAST Lite",
    "category": "Change detection",
    "description": (
        "Single-pass multiple-break detection (Masiliunas et al. 2021), as implemented in Zeit. "
        "Fits one segmented trend + harmonic model and picks the number of breaks by LWZ. "
        "Faster than classic BFAST. Magnitude = difference of the mean value after and before "
        "the break (segment means)."),
    "requires": dict(_REQUIRES, min_dates=20),
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "h", "label": "Min segment size", "type": "float", "default": 0.15, "min": 0.05, "max": 0.5,
         "help": "Minimum segment length, as a fraction of the valid observations."},
        {"id": "max_breaks", "label": "Max breaks", "type": "int", "default": 5, "min": 1, "max": 10,
         "help": "Maximum number of breaks searched for and reported."},
        _ORDER,
        {"id": "min_valid", "label": "Min observations", "type": "int", "default": 20, "min": 5, "max": 1000,
         "help": "Pixels with fewer valid observations are not fitted."},
    ],
    "outputs": [
        {"id": "n_breaks", "name": "Breaks", "colormap": "Viridis", "unit": "count"},
        {"id": "break_date", "name": "Largest break date", "colormap": "Viridis", "unit": "year"},
        {"id": "magnitude", "name": "Largest break magnitude", "colormap": "RdBu", "unit": "value"},
        {"id": "first_break", "name": "First break date", "colormap": "Viridis", "unit": "year"},
    ],
    "chunk_cells": 20_000,  # ~3 ms/pixel: keep progress updates frequent
}

BFAST_MONITOR = {
    "id": "bfast_monitor",
    "name": "BFAST Monitor",
    "category": "Change detection",
    "description": (
        "Near-real-time disturbance monitoring (Verbesselt et al. 2012), as implemented in Zeit. "
        "Fits a stable trend + harmonic model on the history period and flags the first date in "
        "the monitoring period where the OLS-MOSUM process leaves its boundary. "
        "History = everything before the monitoring start."),
    "requires": dict(_REQUIRES, min_dates=12),
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "monitor_start", "label": "Monitoring start (year)", "type": "float", "default": 0.0,
         "min": 0.0, "max": 3000.0,
         "help": "Decimal year where monitoring begins (e.g. 2018.5). 0 = automatic: the last 25% "
                 "of the series. Clamped so the history and at least one year of monitoring fit."},
        {"id": "h", "label": "MOSUM window", "type": "enum", "default": "0.25",
         "options": ["0.25", "0.5", "1.0"], "labels": ["0.25 of history", "0.5 of history", "1.0 of history"],
         "help": "Moving-sum window as a fraction of the history length."},
        {"id": "period", "label": "Monitoring period", "type": "enum", "default": "10",
         "options": ["2", "4", "6", "8", "10"],
         "labels": ["2x history", "4x history", "6x history", "8x history", "10x history"],
         "help": "Maximum monitoring length relative to the history (sets the critical boundary)."},
        _ORDER,
        {"id": "alpha", "label": "Significance level", "type": "enum", "default": "0.05",
         "options": ["0.01", "0.05", "0.1"], "labels": ["0.01", "0.05", "0.10"],
         "help": "Significance level of the monitoring boundary."},
        {"id": "min_valid", "label": "Min history obs.", "type": "int", "default": 10, "min": 5, "max": 1000,
         "help": "Pixels with fewer valid observations in the history period are not fitted."},
    ],
    "outputs": [
        {"id": "break_date", "name": "Break date", "colormap": "Viridis", "unit": "year"},
        {"id": "magnitude", "name": "Magnitude (median residual)", "colormap": "RdBu", "unit": "value"},
        {"id": "has_break", "name": "Break detected", "colormap": "Greys", "unit": "0/1"},
    ],
}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _fmt_year(y):
    """Decimal year -> 'YYYY-MM' (approximate, for the info rows)."""
    yr = int(math.floor(y + 1e-9))
    month = min(12, int((y - yr) * 12 + 1e-6) + 1)
    return f"{yr}-{month:02d}"


def _years_at(years, idx):
    """Dates for break indices (into the full series); NaN/out of range -> NaN."""
    years = np.asarray(years, dtype=np.float64)
    idx = np.asarray(idx, dtype=np.float64)
    out = np.full(idx.shape, np.nan)
    ok = np.isfinite(idx) & (idx >= 0) & (idx <= len(years) - 1)
    out[ok] = years[idx[ok].astype(np.int64)]
    return out


def _break_indices(vals):
    """Finite break indices of one pixel, as sorted ints."""
    return sorted(int(v) for v in vals if np.isfinite(v))


def _values_2d(stack):
    """[T, rows, cols] -> contiguous [pixels, T] float64."""
    T = stack.shape[0]
    return np.ascontiguousarray(stack.reshape(T, -1).T, dtype=np.float64)


def _segment_jumps(values, breaks):
    """Jump at each break = mean(next segment) - mean(previous segment).

    values: [P, T] (NaN = missing); breaks: [P, K] break indices (last index
    of the segment before the break), NaN-padded and ascending. Returns [P, K]
    (NaN where there is no break or a segment has no valid value).
    """
    P, T = values.shape
    K = breaks.shape[1]
    ok = np.isfinite(values)
    S = np.zeros((P, T + 1))
    C = np.zeros((P, T + 1))
    np.cumsum(np.where(ok, values, 0.0), axis=1, out=S[:, 1:])
    np.cumsum(ok, axis=1, out=C[:, 1:])
    rows = np.arange(P)

    def seg_mean(a, b):  # inclusive [a, b] per pixel
        s = S[rows, b + 1] - S[rows, a]
        c = C[rows, b + 1] - C[rows, a]
        with np.errstate(invalid="ignore", divide="ignore"):
            return np.where(c > 0, s / np.maximum(c, 1), np.nan)

    has = np.isfinite(breaks)
    bi = np.where(has, breaks, 0).astype(np.int64)
    bi = np.clip(bi, 0, T - 2)
    jumps = np.full((P, K), np.nan)
    for k in range(K):
        prev_end = bi[:, k - 1] if k > 0 else np.full(P, -1)
        nxt = bi[:, k + 1] if k + 1 < K else np.full(P, T - 1)
        nxt = np.where(has[:, k + 1], nxt, T - 1) if k + 1 < K else nxt
        left = seg_mean(prev_end + 1, bi[:, k])
        right = seg_mean(bi[:, k] + 1, nxt)
        jumps[:, k] = np.where(has[:, k], right - left, np.nan)
    return jumps


def _not_enough(name, n_valid, need):
    return {"overlays": [], "rows": [[name, f"not enough data ({n_valid} valid, need {need})"]]}


# ---------------------------------------------------------------------------
# BFAST (classic)
# ---------------------------------------------------------------------------

def _bfast_batch(p, values, ctx, n_jobs):
    from zeit._core.bfast import fit_bfast_batch
    return fit_bfast_batch(values, float(ctx["start"]), int(ctx["per_year"]), order=int(p["order"]),
                           h=float(p["h"]), max_breaks_trend=int(p["max_breaks_trend"]),
                           max_breaks_season=int(p["max_breaks_season"]), max_iter=int(p["max_iter"]),
                           level=float(p["level"]), min_valid=int(p["min_valid"]), n_jobs=n_jobs)


def _bfast_largest(out, ctx):
    """(date, magnitude) of the largest trend jump; date from Zeit's synthetic time."""
    t = out[3]
    row = np.where(np.isfinite(t), np.round((t - ctx["start"]) * ctx["per_year"]), np.nan)
    return _years_at(ctx["years"], row), out[2]


def bfast_pixel(p, ctx):
    vals = np.array(ctx["values"], dtype=np.float64)
    n_valid = int(np.isfinite(vals).sum())
    need = max(int(p["min_valid"]), 2 * ctx["per_year"] + 1)
    if n_valid < need or len(vals) <= 2 * ctx["per_year"]:
        return _not_enough("BFAST", n_valid, need)
    out = _bfast_batch(p, vals[None, :], ctx, 1)[:, 0]
    if out[6] != 1.0:
        return _not_enough("BFAST", n_valid, need)
    mbt = int(p["max_breaks_trend"])
    trend = _break_indices(out[7:7 + mbt])
    season = _break_indices(out[7 + mbt:])
    years = ctx["years"]
    tx = [float(years[i]) for i in trend]
    sx = [float(years[i]) for i in season]
    date, mag = _bfast_largest(out[:, None], ctx)
    rows = [["Trend breaks", str(len(trend))],
            ["  dates", ", ".join(_fmt_year(x) for x in tx) or "-"],
            ["Season breaks", str(len(season))],
            ["  dates", ", ".join(_fmt_year(x) for x in sx) or "-"]]
    if trend and np.isfinite(date[0]):
        rows.append(["Largest trend jump", f"{float(mag[0]):.5g} at {_fmt_year(float(date[0]))}"])
    rows.append(["Iterations", str(int(out[4]))])
    overlays = []
    if tx:
        overlays.append({"type": "vlines", "label": "BFAST trend breaks", "x": tx})
    if sx:
        overlays.append({"type": "vlines", "label": "BFAST season breaks", "x": sx})
    return {"overlays": overlays, "rows": rows}


def bfast_chunk(p, stack, ctx):
    _, h, w = stack.shape
    out = _bfast_batch(p, _values_2d(stack), ctx, ctx.get("n_jobs", -1))
    valid = out[6] == 1.0
    date, mag = _bfast_largest(out, ctx)
    has = np.isfinite(date)
    res = {
        "n_trend_breaks": np.where(valid, out[0], np.nan),
        "n_season_breaks": np.where(valid, out[1], np.nan),
        "break_date": np.where(valid & has, date, np.nan),
        "magnitude": np.where(valid & has, mag, np.nan),
    }
    return {k: v.reshape(h, w).astype(np.float32) for k, v in res.items()}


# ---------------------------------------------------------------------------
# BFAST Lite
# ---------------------------------------------------------------------------

def _lite_batch(p, values, ctx, n_jobs):
    from zeit._core.bfastlite import fit_bfast_lite_batch
    return fit_bfast_lite_batch(values, float(ctx["start"]), int(ctx["per_year"]), order=int(p["order"]),
                                h=float(p["h"]), max_breaks_output=int(p["max_breaks"]),
                                min_valid=int(p["min_valid"]), n_jobs=n_jobs)


def lite_pixel(p, ctx):
    vals = np.array(ctx["values"], dtype=np.float64)
    n_valid = int(np.isfinite(vals).sum())
    need = int(p["min_valid"])
    if n_valid < need:
        return _not_enough("BFAST Lite", n_valid, need)
    out = _lite_batch(p, vals[None, :], ctx, 1)
    if out[4, 0] != 1.0:
        return _not_enough("BFAST Lite", n_valid, need)
    idx = _break_indices(out[5:, 0])
    years = ctx["years"]
    bx = [float(years[i]) for i in idx]
    rows = [["Breaks", str(len(idx))]]
    if idx:
        jumps = _segment_jumps(vals[None, :], np.array([idx], dtype=np.float64))[0]
        for x, j in zip(bx, jumps):
            rows.append([f"  {_fmt_year(x)}", f"jump {j:.5g}" if np.isfinite(j) else "-"])
    rows += [["LWZ", f"{float(out[2, 0]):.5g}"], ["Valid observations", str(n_valid)]]
    overlays = [{"type": "vlines", "label": "BFAST Lite breaks", "x": bx}] if bx else []
    return {"overlays": overlays, "rows": rows}


def lite_chunk(p, stack, ctx):
    _, h, w = stack.shape
    values = _values_2d(stack)
    out = _lite_batch(p, values, ctx, ctx.get("n_jobs", -1))
    valid = out[4] == 1.0
    breaks = np.sort(out[5:].T, axis=1)  # [P, K], NaN last
    P = breaks.shape[0]
    jumps = _segment_jumps(values, breaks)
    absj = np.where(np.isfinite(jumps), np.abs(jumps), -1.0)
    k = np.argmax(absj, axis=1)
    rows = np.arange(P)
    has = absj[rows, k] >= 0
    res = {
        "n_breaks": np.where(valid, out[0], np.nan),
        "break_date": np.where(valid & has, _years_at(ctx["years"], breaks[rows, k]), np.nan),
        "magnitude": np.where(valid & has, jumps[rows, k], np.nan),
        "first_break": np.where(valid, _years_at(ctx["years"], breaks[:, 0]), np.nan),
    }
    return {k: v.reshape(h, w).astype(np.float32) for k, v in res.items()}


# ---------------------------------------------------------------------------
# BFAST Monitor
# ---------------------------------------------------------------------------

def _monitor_index(p, ctx):
    """First observation of the monitoring period (index into the series)."""
    years = ctx["years"]
    T = len(years)
    ms = float(p["monitor_start"])
    if ms <= 0:
        i = int(math.floor(0.75 * T))
    else:
        i = next((k for k, y in enumerate(years) if y >= ms - 1e-9), T - 1)
    # Keep at least one year (one cycle) to monitor when the series allows it.
    return min(max(i, 1), max(1, T - int(ctx["per_year"])))


def _monitor_batch(p, values, ctx, n_jobs):
    from zeit._core.bfastmonitor import fit_bfast_monitor_batch
    i = _monitor_index(p, ctx)
    # Zeit splits history/monitoring on its synthetic time grid; put the split
    # halfway between observations i-1 and i so rounding cannot move it.
    synth = float(ctx["start"]) + (i - 0.5) / int(ctx["per_year"])
    out = fit_bfast_monitor_batch(values, float(ctx["start"]), synth, int(ctx["per_year"]),
                                  order=int(p["order"]), h=float(p["h"]), period=int(float(p["period"])),
                                  alpha=float(p["alpha"]), min_valid=int(p["min_valid"]), n_jobs=n_jobs)
    return out, i


def monitor_pixel(p, ctx):
    vals = np.array(ctx["values"], dtype=np.float64)
    out, i = _monitor_batch(p, vals[None, :], ctx, 1)
    out = out[:, 0]
    years = ctx["years"]
    start_x = float(years[i])
    n_hist = int(np.isfinite(vals[:i]).sum())
    overlays = [{"type": "vlines", "label": "Monitoring start", "x": [start_x]}]
    rows = [["Monitoring from", _fmt_year(start_x)], ["History obs.", str(n_hist)]]
    if out[6] != 1.0:
        rows.append(["BFAST Monitor", f"not enough history data (need {int(p['min_valid'])} valid "
                                      f"and more than the model terms)"])
        return {"overlays": overlays, "rows": rows}
    date = _years_at(years, out[1:2])[0]
    if out[5] == 1.0 and np.isfinite(date):
        overlays.append({"type": "vlines", "label": "BFAST Monitor break", "x": [float(date)]})
        rows.append(["Break", _fmt_year(float(date))])
    else:
        rows.append(["Break", "none"])
    rows += [["Magnitude (median residual)", f"{float(out[2]):.5g}" if np.isfinite(out[2]) else "-"],
             ["History residual sigma", f"{float(out[3]):.5g}" if np.isfinite(out[3]) else "-"]]
    return {"overlays": overlays, "rows": rows}


def monitor_chunk(p, stack, ctx):
    _, h, w = stack.shape
    out, _ = _monitor_batch(p, _values_2d(stack), ctx, ctx.get("n_jobs", -1))
    valid = out[6] == 1.0
    brk = valid & (out[5] == 1.0)
    res = {
        "break_date": np.where(brk, _years_at(ctx["years"], out[1]), np.nan),
        "magnitude": np.where(valid, out[2], np.nan),
        "has_break": np.where(valid, out[5], np.nan),
    }
    return {k: v.reshape(h, w).astype(np.float32) for k, v in res.items()}


TOOLS = [
    {"manifest": BFAST, "pixel": bfast_pixel, "chunk": bfast_chunk},
    {"manifest": BFAST_LITE, "pixel": lite_pixel, "chunk": lite_chunk},
    {"manifest": BFAST_MONITOR, "pixel": monitor_pixel, "chunk": monitor_chunk},
]
