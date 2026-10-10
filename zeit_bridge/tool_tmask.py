"""Tmask (zeit.tmask) for Janus: clouds and cloud shadows that the quality band missed,
found in each pixel's own time series (Zhu & Woodcock 2014).

Zeit fits a robust harmonic model (MATLAB's robustfit, bisquare, as CCDC's autoTmask)
to the green and SWIR1 series of every pixel; an observation whose green rises more
than 0.04 above it (cloud) or whose SWIR drops more than 0.04 below it (shadow), in
reflectance, is flagged. Pixels with 5 valid observations or fewer are not screened.
Observations the quality band already rejects stay out.

A raster run writes the series Janus shows without the flagged observations, which
opens as a new layer: BFAST, phenology or LandTrendr can then run on the cleaner series.
"""
import numpy as np

import zeit_common as zc

ROLES = ["green", "swir1"]

MANIFEST = {
    "id": "tmask",
    "name": "Tmask (cloud screening)",
    "category": "Preprocessing",
    "description": (
        "Tmask (Zhu & Woodcock 2014), as implemented in Zeit: a robust harmonic model of each pixel's green "
        "and SWIR1 series flags the observations that are too bright in green (clouds) or too dark in "
        "SWIR (shadows) for their date, which a quality band often misses (haze, thin clouds, shadow "
        "edges). Needs the green and SWIR1 bands and real dates. A raster run writes the series shown "
        "without the flagged observations and opens it as a new layer. The model spans the whole series: "
        "after a sudden lasting change (a clearing) the observations can look like clouds to it, so check "
        "such pixels on the chart."),
    "requires": {"time": "any", "min_dates": 6, "min_per_year": 2, "bands": list(ROLES)},
    "modes": ["pixel", "raster"],
    "params": [
        zc.units_param("Tmask's thresholds are in reflectance (0.04)."),
        {"id": "write_series", "label": "Write the screened series", "type": "bool", "default": True,
         "help": "Raster runs: also write the series shown without the flagged observations (opened as a "
                 "new layer)."},
    ],
    "outputs": [
        {"id": "n_flagged", "name": "Observations flagged", "colormap": "Plasma", "unit": "count"},
        {"id": "flagged_share", "name": "Share flagged", "colormap": "Plasma", "unit": "%"},
        {"id": "series", "name": "Screened series", "unit": "value", "series": True, "when": "write_series"},
    ],
    "chunk_cells": 100_000,
}


def _flags(p, ctx):
    """(flagged, valid) bool [T, ...]: valid = both bands have a value the quality band keeps."""
    import zeit
    cube, _ = zc.landsat_cube(p, ctx, ROLES)
    vals = cube.values
    valid = np.all(np.isfinite(vals) & (vals > 0), axis=1)
    clear = np.asarray(zeit.tmask(cube, green="green", swir="swir1", scale=10000.0, nodata=None).values, bool)
    return valid & ~clear, valid


def _check(ctx):
    missing = zc.missing_roles(ctx, ROLES)
    if missing:
        return "map the bands first: " + ", ".join(missing)
    if not ctx.get("ordinal"):
        return "needs real dates"
    return None


def pixel(p, ctx):
    why = _check(ctx)
    if why:
        return {"overlays": [], "rows": [["Tmask", why]]}
    flagged, valid = (a.ravel() for a in _flags(p, ctx))
    n_valid, n_flag = int(valid.sum()), int(flagged.sum())
    rows = [["Valid observations", str(n_valid)], ["Flagged (cloud or shadow)", str(n_flag)]]
    if n_valid <= 5:
        rows.append(["Tmask", "not screened (5 valid observations or fewer)"])
    values = np.asarray(ctx["values"], dtype=np.float64)
    shown = np.where(flagged, values, np.nan)
    overlays = [zc.series_overlay("Tmask: cloud or shadow", ctx, shown, kind="markers")] if np.isfinite(shown).any() \
        else []
    return {"overlays": overlays, "rows": rows}


def chunk(p, stack, ctx):
    why = _check(ctx)
    if why:
        raise ValueError("Tmask: " + why)
    T, h, w = stack.shape
    flagged, valid = _flags(p, ctx)
    nv = valid.sum(axis=0)
    nf = flagged.sum(axis=0)
    with np.errstate(invalid="ignore", divide="ignore"):
        share = np.where(nv > 0, 100.0 * nf / nv, np.nan)
    res = zc.chunk_outputs({"n_flagged": np.where(nv > 0, nf, np.nan), "flagged_share": share}, (h, w))
    if p.get("write_series"):
        res["series"] = np.where(flagged, np.nan, stack)
    return res


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
