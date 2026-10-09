"""TWDTW classification (zeit.twdtw) for Janus: each pixel gets the class of the
most similar reference series (pattern), with Time-Weighted Dynamic Time Warping.

Patterns are made in Janus from pins or the ROI mean (parameter of type
"patterns") and passed to Zeit as series indexed by their dates. The distance is
the R package twdtw's: a pattern may match any stretch of the series, each pair
of observations costs their difference plus a logistic time weight
1 / (1 + exp(-steepness * (elapsed days - midpoint))), measured between
the dates by default (Janus's patterns are whole series from pins; "days of
the year" lets a one-season pattern match that season in any year), and dates a
pixel has no value for are left out of its series.
"""
import math

import numpy as np

import zeit_common as zc

MANIFEST = {
    "id": "twdtw",
    "name": "TWDTW classification",
    "category": "Classification",
    "description": (
        "Time-Weighted Dynamic Time Warping (Maus et al. 2016), as implemented in Zeit (the distance "
        "of the R package twdtw). Each pixel gets the class of the most similar pattern: drop pins on "
        "places you know (forest, crop, pasture...), add them below as classes and name them. A "
        "pattern may match any stretch of the series; matching dates far apart in time is "
        "penalised by the time weight."),
    "requires": {"time": "any", "min_dates": 6, "min_per_year": 1},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "patterns", "label": "Classes", "type": "patterns", "default": [],
         "help": "One reference series per class, taken from a pin or the ROI mean. Drop a pin on a "
                 "place whose class you know, choose it in the list and give the class a name. "
                 "Several patterns may share a name (they are separate entries in the legend)."},
        {"id": "steepness", "label": "Time weight: steepness", "type": "float", "default": 0.1,
         "min": 0.0001, "max": 10.0, "help": "Slope of the logistic time weight, per day (larger = sharper "
                                              "transition at the midpoint)."},
        {"id": "midpoint", "label": "Time weight: midpoint (days)", "type": "float", "default": 50.0,
         "min": 0.0, "max": 3650.0, "help": "Time apart, in days, at which matching two dates costs half a "
                                            "unit more than matching the same date."},
        {"id": "cycle", "label": "Time measured", "type": "enum", "default": "none", "options": ["none", "year"],
         "labels": ["Between the dates", "Between days of the year"],
         "help": "Between the dates: for patterns that cover the period of the series (a pin's whole "
                 "series). Between days of the year (31 December and 1 January are a day apart): for "
                 "patterns of one season, which then match that season in any year."},
        {"id": "max_elapsed", "label": "Max time apart (days)", "type": "int", "default": 365, "min": 0,
         "max": 3650, "help": "Dates more than this apart are never matched. 0 = no limit. The time weight "
                              "adds at most 1 per pair of dates: on series with large values (e.g. "
                              "reflectance x 10000) only this limit keeps a pattern near its own dates."},
        {"id": "max_distance", "label": "Max distance", "type": "float", "default": 0.0, "min": 0.0,
         "max": 1e9, "help": "Pixels whose best distance is above this stay unclassified. "
                             "0 = classify every pixel."},
        {"id": "min_valid", "label": "Min valid observations", "type": "int", "default": 6, "min": 2,
         "max": 10000, "help": "Pixels with fewer valid dates stay unclassified."},
    ],
    "outputs": [
        {"id": "class", "name": "Class", "colormap": "Dark", "unit": "class", "classes_param": "patterns"},
        {"id": "distance", "name": "Distance (best class)", "colormap": "Plasma", "unit": "distance"},
        {"id": "margin", "name": "Margin to the 2nd class", "colormap": "Viridis", "unit": "distance"},
    ],
    "chunk_cells": 200_000,
}


def _patterns(p):
    """[(name, pandas.Series indexed by date, decimal years)], gaps dropped."""
    import pandas as pd

    out = []
    for k, pat in enumerate(p.get("patterns") or []):
        name = str(pat.get("name") or f"Class {k + 1}")
        vals = np.array([np.nan if v is None else float(v) for v in pat.get("values") or []], dtype=np.float64)
        years = np.array(pat.get("years") or [], dtype=np.float64)
        days = pat.get("days")
        if days is None or len(days) != len(vals):
            raise ValueError(f"pattern '{name}' has no dates")
        days = np.array([-1 if d is None else int(d) for d in days], dtype=np.int64)
        ok = np.isfinite(vals) & (days >= 0)
        if len(years) != len(vals):
            years = np.full(len(vals), np.nan)
        if ok.sum() < 2:
            raise ValueError(f"pattern '{name}' has fewer than 2 valid dates")
        index = pd.DatetimeIndex(days[ok].astype("datetime64[D]").astype("datetime64[ns]"))
        out.append((name, pd.Series(vals[ok], index=index), years[ok]))
    return out


def _run(p, stack, ctx, pats):
    """(class 1..K or NaN, best distance, margin, distances [K, ...]) for a [T, rows, cols] chunk."""
    import zeit

    if zc.times(ctx) is None:
        raise ValueError("TWDTW needs dates (none were found in the file names or band descriptions)")
    max_elapsed = float(p["max_elapsed"])
    # Keys by position: several patterns may share a name; label k is the k-th pattern.
    ds = zeit.twdtw(zc.cube(stack, ctx), {str(k): s for k, (_, s, _) in enumerate(pats)},
                    steepness=float(p["steepness"]), midpoint=float(p["midpoint"]),
                    cycle=None if p["cycle"] == "none" else "year",
                    max_elapsed=max_elapsed if max_elapsed > 0 else None, nodata=None,
                    n_jobs=ctx.get("n_jobs", -1))
    dist = zc.grid(ds, "distances")                                       # [K, rows, cols]
    label = zc.grid(ds, "label")
    best = zc.grid(ds, "distance")
    enough = np.isfinite(stack).sum(axis=0) >= max(2, int(p["min_valid"]))
    cls = np.where((label > 0) & enough, label, np.nan)
    max_distance = float(p["max_distance"])
    if max_distance > 0:
        cls[best > max_distance] = np.nan
    second = np.sort(np.where(np.isfinite(dist), dist, np.inf), axis=0)[1] if len(pats) > 1 else np.inf
    with np.errstate(invalid="ignore"):
        margin = np.where(np.isfinite(cls) & np.isfinite(second), second - best, np.nan)
    best = np.where(enough, best, np.nan)
    return cls, best, margin, dist


def pixel(p, ctx):
    try:
        pats = _patterns(p)
    except ValueError as e:
        return {"overlays": [], "rows": [["TWDTW", str(e)]]}
    if not pats:
        return {"overlays": [], "rows": [["TWDTW", "add at least one class pattern (from a pin or the ROI)"]]}
    stack = zc.pixel_stack(ctx)
    n = int(np.isfinite(stack).sum())
    if n < max(2, int(p["min_valid"])):
        return {"overlays": [], "rows": [["TWDTW", f"not enough data ({n} valid, need {int(p['min_valid'])})"]]}
    try:
        cls, best, margin, dist = _run(p, stack, ctx, pats)
    except ValueError as e:
        return {"overlays": [], "rows": [["TWDTW", str(e)]]}
    c, d = cls[0, 0], dist[:, 0, 0]
    rows = []
    if np.isfinite(c):
        rows.append(["Class", pats[int(c) - 1][0]])
        if np.isfinite(margin[0, 0]):
            rows.append(["Margin to 2nd", f"{margin[0, 0]:.4g}"])
    else:
        rows.append(["Class", "unclassified (above the max distance)" if np.isfinite(d).any() else "no match"])
    for i in np.argsort(np.where(np.isfinite(d), d, np.inf)):
        rows.append([f"  {pats[i][0]}", f"{d[i]:.4g}" if math.isfinite(d[i]) else "no match (max time apart)"])
    gaps = stack.shape[0] - n
    if gaps:
        rows.append(["Dates without a value (left out)", str(gaps)])
    overlays = []
    if np.isfinite(c):
        name, s, py = pats[int(c) - 1]
        ok = np.isfinite(py)
        if ok.sum() >= 2:
            overlays.append({"type": "line", "label": f"Pattern: {name}", "x": [float(x) for x in py[ok]],
                             "y": [float(y) for y in s.values[ok]]})
    return {"overlays": overlays, "rows": rows}


def chunk(p, stack, ctx):
    pats = _patterns(p)
    if not pats:
        raise ValueError("add at least one class pattern (from a pin or the ROI) in the tool window")
    cls, best, margin, _ = _run(p, stack, ctx, pats)
    return zc.chunk_outputs({"class": cls, "distance": best, "margin": margin}, stack.shape[1:])


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
