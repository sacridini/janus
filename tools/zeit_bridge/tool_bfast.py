"""BFAST family for Janus: classic BFAST (zeit.bfast), BFAST Lite (zeit.bfast_lite)
and BFAST Monitor (zeit.bfast_monitor).

All three fit trend + harmonic models on a regular series (Zeit puts it on the
regular time axis of R's `ts`, from the dates). A break's date is the date of
the first observation after it and its magnitude the model after the break
minus the model before it on that date (Zeit's convention, as in
zeit.extract_events).
"""
import math

import numpy as np

import zeit_common as zc

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
        "Faster than classic BFAST. Magnitude = the model after the break minus the model before "
        "it, on the first observation after the break."),
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

def _not_enough(name, n_valid, need):
    return {"overlays": [], "rows": [[name, f"not enough data ({n_valid} valid, need {need})"]]}


def _stack(ds, prefix, n):
    """Variables prefix1..prefixn of a result -> [n, rows, cols] (dates as decimal years)."""
    return np.stack([zc.grid(ds, f"{prefix}{k}") for k in range(1, n + 1)])


def _largest(mags, dates):
    """Date and (signed) magnitude of the largest jump per pixel: mags, dates [K, rows, cols]."""
    score = np.where(np.isfinite(mags) & np.isfinite(dates), np.abs(mags), -1.0)
    k = np.argmax(score, axis=0)[None]
    has = np.take_along_axis(score, k, axis=0)[0] >= 0
    return (np.where(has, np.take_along_axis(dates, k, axis=0)[0], np.nan),
            np.where(has, np.take_along_axis(mags, k, axis=0)[0], np.nan))


def _first_after(vals, years, idx):
    """Date of the first valid observation after index idx (the last one before a break)."""
    for t in range(int(idx) + 1, len(vals)):
        if np.isfinite(vals[t]):
            return float(years[t])
    return float("nan")


# ---------------------------------------------------------------------------
# Fitted models on the chart. Zeit returns the breaks, not the models: they are
# fitted again here by least squares, as the final model of each method is
# (piecewise linear trend + harmonic season), on Zeit's regular time axis
# (start + index / frequency, as R's ts) and drawn at the series' dates.
# ---------------------------------------------------------------------------

def _axis(ctx):
    """Regular time of each observation (Zeit's ts axis) and the harmonic order cap."""
    T = len(ctx["years"])
    return float(ctx["start"]) + np.arange(T) / float(ctx["per_year"])


def _harmonics(u, order):
    cols = []
    for k in range(1, order + 1):
        cols += [np.sin(2 * np.pi * k * u), np.cos(2 * np.pi * k * u)]
    return np.stack(cols, axis=-1) if cols else np.zeros(u.shape + (0,))


def _order(p, ctx):
    return max(1, min(int(p["order"]), int(ctx["per_year"]) // 2 or 1))


def _boundaries(n, starts):
    """Segments [a, b) of indices 0..n from the indices where segments start."""
    cuts = sorted({int(s) for s in starts if 0 < int(s) < n})
    edges = [0] + cuts + [n]
    return [(edges[k], edges[k + 1]) for k in range(len(edges) - 1)]


def _index_of_date(years, d):
    """Index of the first observation on or after decimal year d."""
    return next((t for t, y in enumerate(years) if y >= d - 1e-9), len(years))


def _dense(a, b, ctx, per_obs=6):
    """Fractional indices across [a, b - 1] and their dates (interpolated between observations)."""
    years = np.asarray(ctx["years"], dtype=np.float64)
    if b - a < 2:
        s = np.array([float(a)])
    else:
        s = np.linspace(a, b - 1, min(600, (b - 1 - a) * per_obs + 1))
    x = np.interp(s, np.arange(len(years)), years)
    u = float(ctx["start"]) + s / float(ctx["per_year"])
    return s, u, x


def _line(label, x, y, **style):
    o = {"type": "line", "label": label, "x": [float(v) for v in x],
         "y": [float(v) if np.isfinite(v) else None for v in y]}
    o.update(style)
    return o


def _segment_model(vals, u, a, b, order):
    """Trend + harmonic fitted on the valid observations in [a, b); None if too few."""
    idx = np.arange(a, b)
    ok = np.isfinite(vals[a:b])
    X = np.column_stack([np.ones(b - a), u[a:b], _harmonics(u[a:b], order)])
    if ok.sum() < X.shape[1] + 1:
        X = X[:, :2]  # a short segment: its trend alone
        if ok.sum() < 3:
            return None
    beta, *_ = np.linalg.lstsq(X[ok], vals[idx[ok]], rcond=None)
    return beta


def _eval(beta, u, order):
    X = np.column_stack([np.ones_like(u), u, _harmonics(u, order)])[:, :len(beta)]
    return X @ beta, X[:, :2] @ beta[:2]


def bfast_model_overlays(vals, ctx, order, trend_starts, season_starts):
    """BFAST: a trend per trend segment and a season per season segment, fitted
    together; the model (trend + season) and the trend, broken at trend breaks."""
    T = len(vals)
    u = _axis(ctx)
    tseg = _boundaries(T, trend_starts)
    sseg = _boundaries(T, season_starts)

    def design(uu, ss):
        cols = []
        for a, b in tseg:
            m = ((ss >= a) & (ss < b)).astype(np.float64)
            cols += [m, m * uu]
        H = _harmonics(uu, order)
        for a, b in sseg:
            m = (ss >= a) & (ss < b)
            cols += [H[:, k] * m for k in range(H.shape[1])]
        return np.column_stack(cols), 2 * len(tseg)

    s_obs = np.arange(T, dtype=np.float64)
    X, nt = design(u, s_obs)
    ok = np.isfinite(vals)
    if ok.sum() < X.shape[1] + 1:
        return []
    beta, *_ = np.linalg.lstsq(X[ok], vals[ok], rcond=None)
    out = []
    for k, (a, b) in enumerate(tseg):
        s, uu, x = _dense(a, b, ctx)
        D, _ = design(uu, np.minimum(s, b - 1e-6))
        out.append(_line("BFAST model", x, D @ beta))
        out.append(_line("BFAST trend", x, D[:, :nt] @ beta[:nt], weight=1.5, alpha=0.7))
    return out


# ---------------------------------------------------------------------------
# BFAST (classic)
# ---------------------------------------------------------------------------

def _bfast(p, stack, ctx):
    import zeit
    return zeit.bfast(zc.cube(stack, ctx), order=int(p["order"]), h=float(p["h"]),
                      max_breaks_trend=int(p["max_breaks_trend"]), max_breaks_season=int(p["max_breaks_season"]),
                      max_iter=int(p["max_iter"]), level=float(p["level"]), min_valid=int(p["min_valid"]),
                      nodata=None, n_jobs=ctx.get("n_jobs", -1))


def _bfast_maps(p, ds):
    K = int(p["max_breaks_trend"])
    date, mag = _largest(_stack(ds, "trend_magnitude_", K), _stack(ds, "trend_break_date_", K))
    valid = zc.grid(ds, "valid") == 1.0
    return {
        "n_trend_breaks": np.where(valid, zc.grid(ds, "n_trend_breaks"), np.nan),
        "n_season_breaks": np.where(valid, zc.grid(ds, "n_season_breaks"), np.nan),
        "break_date": np.where(valid, date, np.nan),
        "magnitude": np.where(valid, mag, np.nan),
    }


def bfast_pixel(p, ctx):
    vals = np.array(ctx["values"], dtype=np.float64)
    n_valid = int(np.isfinite(vals).sum())
    need = max(int(p["min_valid"]), 2 * ctx["per_year"] + 1)
    if n_valid < need or len(vals) <= 2 * ctx["per_year"]:
        return _not_enough("BFAST", n_valid, need)
    ds = _bfast(p, zc.pixel_stack(ctx), ctx)
    if zc.grid(ds, "valid")[0, 0] != 1.0:
        return _not_enough("BFAST", n_valid, need)
    K, Ks = int(p["max_breaks_trend"]), int(p["max_breaks_season"])
    trend = sorted((float(x), float(j)) for x, j in zip(_stack(ds, "trend_break_date_", K)[:, 0, 0],
                                                        _stack(ds, "trend_magnitude_", K)[:, 0, 0])
                   if np.isfinite(x))
    tx = [x for x, _ in trend]
    sidx = _stack(ds, "season_breakpoint_idx_", Ks)[:, 0, 0]
    sx = sorted(x for x in (_first_after(vals, ctx["years"], i) for i in sidx if np.isfinite(i)) if np.isfinite(x))
    rows = [["Trend breaks", str(len(tx))]]
    for x, j in trend:
        rows.append([f"  {zc.fmt_year(x)}", f"jump {j:.5g}" if np.isfinite(j) else "-"])
    rows += [["Season breaks", str(len(sx))],
             ["  dates", ", ".join(zc.fmt_year(x) for x in sx) or "-"]]
    m = _bfast_maps(p, ds)
    if np.isfinite(m["break_date"][0, 0]):
        rows.append(["Largest trend jump", f"{m['magnitude'][0, 0]:.5g} at {zc.fmt_year(m['break_date'][0, 0])}"])
    rows.append(["Iterations", str(int(zc.grid(ds, "n_iter")[0, 0]))])
    years = list(ctx["years"])
    overlays = bfast_model_overlays(vals, ctx, _order(p, ctx), [_index_of_date(years, x) for x in tx],
                                    [int(i) + 1 for i in sidx if np.isfinite(i)])
    if tx:
        overlays.append({"type": "vlines", "label": "BFAST trend breaks", "x": tx})
    if sx:
        overlays.append({"type": "vlines", "label": "BFAST season breaks", "x": sx})
    return {"overlays": overlays, "rows": rows}


def bfast_chunk(p, stack, ctx):
    return zc.chunk_outputs(_bfast_maps(p, _bfast(p, stack, ctx)), stack.shape[1:])


# ---------------------------------------------------------------------------
# BFAST Lite
# ---------------------------------------------------------------------------

def _lite(p, stack, ctx):
    import zeit
    return zeit.bfast_lite(zc.cube(stack, ctx), order=int(p["order"]), h=float(p["h"]),
                           max_breaks=int(p["max_breaks"]), min_valid=int(p["min_valid"]), nodata=None,
                           n_jobs=ctx.get("n_jobs", -1))


def _lite_maps(p, ds):
    K = int(p["max_breaks"])
    dates = _stack(ds, "break_date_", K)
    date, mag = _largest(_stack(ds, "magnitude_", K), dates)
    has = np.isfinite(dates).any(axis=0)
    first = np.where(has, np.min(np.where(np.isfinite(dates), dates, np.inf), axis=0), np.nan)
    valid = zc.grid(ds, "valid") == 1.0
    return {
        "n_breaks": np.where(valid, zc.grid(ds, "n_breaks"), np.nan),
        "break_date": np.where(valid, date, np.nan),
        "magnitude": np.where(valid, mag, np.nan),
        "first_break": np.where(valid, first, np.nan),
    }


def lite_pixel(p, ctx):
    vals = np.array(ctx["values"], dtype=np.float64)
    n_valid = int(np.isfinite(vals).sum())
    need = int(p["min_valid"])
    if n_valid < need:
        return _not_enough("BFAST Lite", n_valid, need)
    ds = _lite(p, zc.pixel_stack(ctx), ctx)
    if zc.grid(ds, "valid")[0, 0] != 1.0:
        return _not_enough("BFAST Lite", n_valid, need)
    K = int(p["max_breaks"])
    breaks = sorted((float(x), float(j)) for x, j in zip(_stack(ds, "break_date_", K)[:, 0, 0],
                                                         _stack(ds, "magnitude_", K)[:, 0, 0])
                    if np.isfinite(x))
    rows = [["Breaks", str(len(breaks))]]
    for x, j in breaks:
        rows.append([f"  {zc.fmt_year(x)}", f"jump {j:.5g}" if np.isfinite(j) else "-"])
    rows += [["LWZ", f"{zc.grid(ds, 'lwz')[0, 0]:.5g}"], ["Valid observations", str(n_valid)]]
    bx = [x for x, _ in breaks]
    # The model: trend + harmonic, every coefficient breaking at once (bfastlite's).
    u, order, overlays = _axis(ctx), _order(p, ctx), []
    for a, b in _boundaries(len(vals), [_index_of_date(list(ctx["years"]), x) for x in bx]):
        beta = _segment_model(vals, u, a, b, order)
        if beta is not None:
            _, uu, x = _dense(a, b, ctx)
            model, trend = _eval(beta, uu, order)
            overlays += [_line("BFAST Lite model", x, model), _line("BFAST Lite trend", x, trend, weight=1.5, alpha=0.7)]
    if bx:
        overlays.append({"type": "vlines", "label": "BFAST Lite breaks", "x": bx})
    return {"overlays": overlays, "rows": rows}


def lite_chunk(p, stack, ctx):
    return zc.chunk_outputs(_lite_maps(p, _lite(p, stack, ctx)), stack.shape[1:])


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


def _monitor(p, stack, ctx):
    import zeit
    i = _monitor_index(p, ctx)
    # History = observations before i: on a regular time axis that starts at the
    # first date with one step per observation, the monitoring starts halfway
    # between observations i-1 and i, so rounding cannot move it.
    start, freq = float(ctx["start"]), int(ctx["per_year"])
    ds = zeit.bfast_monitor(zc.cube(stack, ctx), start + (i - 0.5) / freq, start_time=start, frequency=freq,
                            order=int(p["order"]), h=float(p["h"]), period=int(float(p["period"])),
                            alpha=float(p["alpha"]), min_valid=int(p["min_valid"]), nodata=None,
                            n_jobs=ctx.get("n_jobs", -1))
    return ds, i


def _monitor_maps(ds):
    valid = zc.grid(ds, "valid") == 1.0
    has = zc.grid(ds, "has_break")
    return {
        "break_date": np.where(valid & (has == 1.0), zc.grid(ds, "break_date"), np.nan),
        "magnitude": np.where(valid, zc.grid(ds, "magnitude"), np.nan),
        "has_break": np.where(valid, has, np.nan),
    }


def monitor_pixel(p, ctx):
    ds, i = _monitor(p, zc.pixel_stack(ctx), ctx)
    start_x = float(ctx["years"][i])
    n_hist = int(np.isfinite(np.asarray(ctx["values"], dtype=np.float64)[:i]).sum())
    overlays = [{"type": "vlines", "label": "Monitoring start", "x": [start_x]}]
    rows = [["Monitoring from", zc.fmt_year(start_x)], ["History obs.", str(n_hist)]]
    if zc.grid(ds, "valid")[0, 0] != 1.0:
        rows.append(["BFAST Monitor", f"not enough history data (need {int(p['min_valid'])} valid "
                                      f"and more than the model terms)"])
        return {"overlays": overlays, "rows": rows}
    # The history's model, and where it goes over the monitoring period (what the
    # observations are compared with).
    vals = np.asarray(ctx["values"], dtype=np.float64)
    u, order = _axis(ctx), _order(p, ctx)
    beta = _segment_model(vals, u, 0, i, order)
    if beta is not None:
        _, uu, x = _dense(0, i, ctx)
        overlays.append(_line("History model", x, _eval(beta, uu, order)[0]))
        _, uu, x = _dense(i - 1, len(vals), ctx)
        overlays.append(_line("Model projected", x, _eval(beta, uu, order)[0], weight=1.5, alpha=0.7))
    m = {k: v[0, 0] for k, v in _monitor_maps(ds).items()}
    if np.isfinite(m["break_date"]):
        overlays.append({"type": "vlines", "label": "BFAST Monitor break", "x": [float(m["break_date"])]})
        rows.append(["Break", zc.fmt_year(float(m["break_date"]))])
    else:
        rows.append(["Break", "none"])
    sigma = zc.grid(ds, "sigma")[0, 0]
    rows += [["Magnitude (median residual)", f"{m['magnitude']:.5g}" if np.isfinite(m["magnitude"]) else "-"],
             ["History residual sigma", f"{sigma:.5g}" if np.isfinite(sigma) else "-"]]
    return {"overlays": overlays, "rows": rows}


def monitor_chunk(p, stack, ctx):
    ds, _ = _monitor(p, stack, ctx)
    return zc.chunk_outputs(_monitor_maps(ds), stack.shape[1:])


TOOLS = [
    {"manifest": BFAST, "pixel": bfast_pixel, "chunk": bfast_chunk},
    {"manifest": BFAST_LITE, "pixel": lite_pixel, "chunk": lite_chunk},
    {"manifest": BFAST_MONITOR, "pixel": monitor_pixel, "chunk": monitor_chunk},
]
