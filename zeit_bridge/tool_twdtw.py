"""TWDTW classification (Zeit) for tsv: each pixel gets the class of the most
similar reference series (pattern), with Time-Weighted Dynamic Time Warping.

Patterns are made in tsv from pins or the ROI mean (parameter of type
"patterns"). Zeit's TWDTW works on absolute dates in days: the time penalty is
alpha / (1 + exp(-beta * (|day difference| - gamma))) and no point is matched
to one more than max_time_warp days away. Series and patterns are therefore
put on the same day axis (Python ordinal days). Zeit propagates NaN into the
distance, so gaps in the pixel series are filled by linear interpolation in
time (the same in pixel and raster runs); gaps in a pattern are dropped.
Distances are reported per date of the series (the DTW sum divided by the
number of dates), so thresholds do not depend on the series length.
"""
import math

import numpy as np

UNIX_EPOCH_ORDINAL = 719163  # date(1970, 1, 1).toordinal(); pattern "days" are since 1970

MANIFEST = {
    "id": "twdtw",
    "name": "TWDTW classification",
    "category": "Classification",
    "description": (
        "Time-Weighted Dynamic Time Warping (Maus et al. 2016), as implemented in Zeit. Each pixel "
        "gets the class of the most similar pattern: drop pins on places you know (forest, crop, "
        "pasture...), add them below as classes and name them. Matching allows small shifts in "
        "time, penalised by the time weight. Distances are per date (DTW cost / number of dates)."),
    "requires": {"time": "any", "min_dates": 6, "min_per_year": 1},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "patterns", "label": "Classes", "type": "patterns", "default": [],
         "help": "One reference series per class, taken from a pin or the ROI mean. Drop a pin on a "
                 "place whose class you know, choose it in the list and give the class a name. "
                 "Several patterns may share a name (they are separate entries in the legend)."},
        {"id": "alpha", "label": "Time weight: max penalty", "type": "float", "default": 0.1, "min": 0.0,
         "max": 1000.0, "help": "Largest penalty added per matched pair of dates far apart in time, "
                                "in the units of the series (e.g. 0.1 NDVI)."},
        {"id": "beta", "label": "Time weight: steepness", "type": "float", "default": 0.05, "min": 0.0001,
         "max": 10.0, "help": "Slope of the logistic time weight, per day (larger = sharper transition "
                              "at the midpoint)."},
        {"id": "gamma", "label": "Time weight: midpoint (days)", "type": "float", "default": 50.0,
         "min": 0.0, "max": 3650.0, "help": "Time difference, in days, where the penalty reaches half "
                                            "of its maximum."},
        {"id": "max_time_warp", "label": "Max time shift (days)", "type": "int", "default": 365, "min": 1,
         "max": 3650, "help": "Dates more than this apart are never matched (Sakoe-Chiba band)."},
        {"id": "max_distance", "label": "Max distance per date", "type": "float", "default": 0.0, "min": 0.0,
         "max": 1e9, "help": "Pixels whose best distance per date is above this stay unclassified. "
                             "0 = classify every pixel."},
        {"id": "min_valid", "label": "Min valid observations", "type": "int", "default": 6, "min": 2,
         "max": 10000, "help": "Pixels with fewer valid dates stay unclassified (gaps are interpolated)."},
    ],
    "outputs": [
        {"id": "class", "name": "Class", "colormap": "Dark", "unit": "class", "classes_param": "patterns"},
        {"id": "distance", "name": "Distance per date (best class)", "colormap": "Plasma", "unit": "distance"},
        {"id": "margin", "name": "Margin to the 2nd class", "colormap": "Viridis", "unit": "distance"},
    ],
    "chunk_cells": 200_000,
}


def _params(p):
    from zeit._core.twdtw import TWDTWParams
    tp = TWDTWParams()
    tp.alpha = float(p["alpha"])
    tp.beta = float(p["beta"])
    tp.gamma = float(p["gamma"])
    tp.max_time_warp = int(p["max_time_warp"])
    tp.subsequence_matching = False
    return tp


def _series_days(ctx):
    if not ctx.get("ordinal"):
        raise ValueError("TWDTW needs dates (none were found in the file names or band descriptions)")
    return np.asarray(ctx["ordinal"], dtype=np.int64)


def _patterns(p, ctx):
    """[(name, days int32 [m], values float64 [m], years [m])], gaps dropped."""
    out = []
    for k, pat in enumerate(p.get("patterns") or []):
        name = str(pat.get("name") or f"Class {k + 1}")
        vals = np.array([np.nan if v is None else float(v) for v in pat.get("values") or []], dtype=np.float64)
        years = np.array(pat.get("years") or [], dtype=np.float64)
        days = pat.get("days")
        if days is None or len(days) != len(vals):
            raise ValueError(f"pattern '{name}' has no dates")
        days = np.array([d + UNIX_EPOCH_ORDINAL if d is not None else -1 for d in days], dtype=np.int64)
        ok = np.isfinite(vals) & (days >= 0)
        if len(years) != len(vals):
            years = np.full(len(vals), np.nan)
        if ok.sum() < 2:
            raise ValueError(f"pattern '{name}' has fewer than 2 valid dates")
        out.append((name, days[ok].astype(np.int32), vals[ok], years[ok]))
    return out


def _fill(values, days, min_valid):
    """Linear interpolation of NaN over time, per row of values [P, T]; rows with
    fewer than min_valid observations come back all-NaN. Returns (filled, n_valid)."""
    P, T = values.shape
    ok = np.isfinite(values)
    n = ok.sum(axis=1)
    out = np.full_like(values, np.nan)
    x = days.astype(np.float64)
    full = n == T
    out[full] = values[full]
    for i in np.nonzero(~full & (n >= max(2, min_valid)))[0]:
        m = ok[i]
        out[i] = np.interp(x, x[m], values[i, m])  # edges: nearest valid value
    out[n < max(2, min_valid)] = np.nan
    return out, n


def _classify(dist, max_distance):
    """dist [K, P] per-date distances -> (class 1..K or NaN, best, margin)."""
    d = np.where(np.isfinite(dist), dist, np.inf)
    order = np.argsort(d, axis=0)
    best_i = order[0]
    cols = np.arange(d.shape[1])
    best = d[best_i, cols]
    second = d[order[1], cols] if d.shape[0] > 1 else np.full(d.shape[1], np.inf)
    cls = (best_i + 1).astype(np.float64)
    bad = ~np.isfinite(best)
    if max_distance > 0:
        bad |= best > max_distance
    cls[bad] = np.nan
    with np.errstate(invalid="ignore"):
        margin = np.where(np.isfinite(second) & ~bad, second - best, np.nan)
    best = np.where(bad & ~np.isfinite(best), np.nan, best)
    return cls, best, margin


def pixel(p, ctx):
    from zeit._core.twdtw import fit_twdtw

    try:
        pats = _patterns(p, ctx)
        days = _series_days(ctx)
    except ValueError as e:
        return {"overlays": [], "rows": [["TWDTW", str(e)]]}
    if not pats:
        return {"overlays": [], "rows": [["TWDTW", "add at least one class pattern (from a pin or the ROI)"]]}
    vals = np.array(ctx["values"], dtype=np.float64)[None, :]
    filled, n = _fill(vals, days, int(p["min_valid"]))
    if not np.isfinite(filled).all():
        return {"overlays": [], "rows": [["TWDTW", f"not enough data ({int(n[0])} valid, need {int(p['min_valid'])})"]]}
    tp = _params(p)
    T = vals.shape[1]
    ts = [float(v) for v in filled[0]]
    ts_days = [int(d) for d in days]
    dist = np.array([[fit_twdtw(ts, ts_days, [float(v) for v in pv], [int(d) for d in pd], 1, tp).distance / T]
                     for _, pd, pv, _ in pats])
    cls, best, margin = _classify(dist, float(p["max_distance"]))
    rows = []
    if np.isfinite(cls[0]):
        k = int(cls[0]) - 1
        rows.append(["Class", pats[k][0]])
        if np.isfinite(margin[0]):
            rows.append(["Margin to 2nd", f"{margin[0]:.4g}"])
    else:
        rows.append(["Class", "unclassified (above the max distance)" if np.isfinite(dist).any() else "no match"])
    for i in np.argsort(np.where(np.isfinite(dist[:, 0]), dist[:, 0], np.inf)):
        d = dist[i, 0]
        rows.append([f"  {pats[i][0]}", f"{d:.4g}" if math.isfinite(d) else "no match (max time shift)"])
    gaps = T - int(n[0])
    if gaps:
        rows.append(["Gaps interpolated", str(gaps)])
    overlays = []
    if np.isfinite(cls[0]):
        name, _, pv, py = pats[int(cls[0]) - 1]
        ok = np.isfinite(py)
        if ok.sum() >= 2:
            overlays.append({"type": "line", "label": f"Pattern: {name}", "x": [float(x) for x in py[ok]],
                             "y": [float(y) for y in pv[ok]]})
    return {"overlays": overlays, "rows": rows}


def chunk(p, stack, ctx):
    from zeit._core.twdtw import fit_twdtw_batch

    pats = _patterns(p, ctx)
    if not pats:
        raise ValueError("add at least one class pattern (from a pin or the ROI) in the tool window")
    days = _series_days(ctx)
    T, h, w = stack.shape
    values = np.ascontiguousarray(stack.reshape(T, h * w).T)
    filled, _ = _fill(values, days, int(p["min_valid"]))
    ok = np.isfinite(filled).all(axis=1)
    tp = _params(p)
    dates = np.ascontiguousarray(days, dtype=np.int32)
    dist = np.full((len(pats), h * w), np.nan)
    if ok.any():
        batch = np.ascontiguousarray(filled[ok][None, :, :])  # [1, P_ok, T]
        for k, (_, pd, pv, _) in enumerate(pats):
            d = np.asarray(fit_twdtw_batch(batch, dates, np.ascontiguousarray(pv), np.ascontiguousarray(pd),
                                           tp, math.inf, -1))[0]
            dist[k, ok] = d / T
    cls, best, margin = _classify(dist, float(p["max_distance"]))
    cls[~ok] = np.nan
    best[~ok] = np.nan
    margin[~ok] = np.nan
    f = lambda a: a.reshape(h, w).astype(np.float32)
    return {"class": f(cls), "distance": f(best), "margin": f(margin)}


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
