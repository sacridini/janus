"""NDFI (zeit.unmix) for Janus: spectral mixture analysis of every observation and the
Normalized Difference Fraction Index of Souza et al. (2005), the index of forest
degradation (selective logging, understory fire) in the tropics.

Each observation of a Landsat-style surface-reflectance series (blue, green, red, NIR,
SWIR1, SWIR2) is unmixed into fractions of green vegetation (GV), non-photosynthetic
vegetation (NPV), soil, shade and cloud with the endmembers of Souza et al. (2005)
and CODED's cloud, fully constrained (Zeit's C++ engine). NDFI = (GVs - (NPV + soil)) /
(GVs + NPV + soil), GVs = GV / (1 - shade): near 1 in closed forest, lower where the
canopy was opened, below 0 on soil and pasture.

A raster run writes the NDFI (and, if asked, the fractions) as a series of the same
dates, which Janus opens as a new layer: every tool (LandTrendr, BFAST...) can then run
on it, as CODED does internally.
"""
import warnings

import numpy as np

import zeit_common as zc

ROLES = zc.LANDSAT_ROLES
FRACTIONS = ["gv", "npv", "soil", "shade"]
FRACTION_NAMES = {"gv": "GV fraction", "npv": "NPV fraction", "soil": "Soil fraction", "shade": "Shade fraction"}

MANIFEST = {
    "id": "ndfi",
    "name": "NDFI (spectral unmixing)",
    "category": "Indices",
    "description": (
        "Spectral mixture analysis (Zeit's zeit.unmix) of every observation into green vegetation, "
        "non-photosynthetic vegetation, soil, shade and cloud fractions, with the endmembers of Souza et "
        "al. (2005), and their NDFI: near 1 in closed forest, lower where logging or fire opened the "
        "canopy, below 0 on soil and pasture. Needs Landsat-style surface reflectance (blue, green, red, "
        "NIR, SWIR1, SWIR2) with real dates. A raster run writes the NDFI as a series and opens it as a "
        "new layer, ready for LandTrendr, BFAST and the other tools."),
    "requires": {"time": "any", "min_dates": 1, "bands": list(ROLES)},
    "modes": ["pixel", "raster"],
    "params": [
        zc.units_param("The endmembers are reflectances."),
        {"id": "cloud_threshold", "label": "Max cloud fraction", "type": "float", "default": 0.05, "min": 0.0,
         "max": 1.0, "help": "Observations whose cloud fraction is above this are left out (CODED: 0.05); "
                             "1 keeps every observation. The quality band, when set, is applied first."},
        {"id": "fractions", "label": "Also write the fractions", "type": "bool", "default": False,
         "help": "Raster runs: also write the GV, NPV, soil and shade fractions, each as a series "
                 "(opened as layers)."},
    ],
    "outputs": [
        {"id": "ndfi", "name": "NDFI", "unit": "NDFI", "series": True, "dtype": "int16", "scale": 1e-4},
        {"id": "ndfi_mean", "name": "Mean NDFI", "colormap": "NDVI", "unit": "NDFI"},
        {"id": "ndfi_min", "name": "Lowest NDFI", "colormap": "NDVI", "unit": "NDFI"},
        {"id": "n_obs", "name": "Observations used", "colormap": "Viridis", "unit": "count"},
    ] + [{"id": f, "name": FRACTION_NAMES[f], "unit": "fraction", "series": True, "dtype": "int16", "scale": 1e-4,
          "when": "fractions"} for f in FRACTIONS],
    "chunk_cells": 50_000,  # 6 bands x every date in memory, and 7 fractions out
}


def unmix(p, ctx):
    """zeit.unmix of the mapped bands: Dataset (time, y, x) of gv, npv, soil, shade, cloud,
    rmse and ndfi; NaN where the quality band or the cloud fraction leaves an observation out."""
    import zeit
    cube, _ = zc.landsat_cube(p, ctx, ROLES)
    ct = float(p["cloud_threshold"])
    return zeit.unmix(cube, "souza2005", scale=10000.0, cloud_threshold=ct if ct < 1.0 else None, nodata=None,
                      n_jobs=ctx.get("n_jobs", -1))


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
        return {"overlays": [], "rows": [["NDFI", why]]}
    ds = unmix(p, ctx)
    ndfi = np.asarray(ds["ndfi"].values, dtype=np.float64).ravel()
    ok = np.isfinite(ndfi)
    rows = [["Observations used", f"{int(ok.sum())} of {ndfi.size}"]]
    if not ok.any():
        return {"overlays": [], "rows": rows + [["NDFI", "no clear observation"]]}
    rows += [["Mean NDFI", f"{np.mean(ndfi[ok]):.3f}"], ["Lowest NDFI", f"{np.min(ndfi[ok]):.3f}"],
             ["Highest NDFI", f"{np.max(ndfi[ok]):.3f}"]]
    for f in FRACTIONS:
        v = np.asarray(ds[f].values, dtype=np.float64).ravel()
        rows.append([f"Mean {FRACTION_NAMES[f]}", f"{np.nanmean(v):.3f}"])
    overlays = []
    if zc.shown_is_index(ctx):
        overlays.append(zc.series_overlay("NDFI", ctx, ndfi))
    else:
        rows.append(["NDFI on the chart", "drawn when the chart shows an index (values in -1..1)"])
    return {"overlays": overlays, "rows": rows}


def chunk(p, stack, ctx):
    why = _check(ctx)
    if why:
        raise ValueError("NDFI: " + why)
    T, h, w = stack.shape
    ds = unmix(p, ctx)
    ndfi = np.asarray(ds["ndfi"].values, dtype=np.float64)        # [T, h, w]
    n = np.isfinite(ndfi).sum(axis=0)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)  # pixels without any observation
        res = {"ndfi_mean": np.nanmean(ndfi, axis=0), "ndfi_min": np.nanmin(ndfi, axis=0)}
    res = zc.chunk_outputs(res, (h, w))
    res["n_obs"] = np.where(n > 0, n, np.nan).astype(np.float32)
    res["ndfi"] = ndfi
    if p.get("fractions"):
        for f in FRACTIONS:
            res[f] = np.asarray(ds[f].values, dtype=np.float64)
    return res


def warmup():
    """The first unmixing imports and loads what CODED and NDFI use (~2 s): before a pixel waits."""
    import xarray as xr
    import zeit
    t = np.array(["2020-01-01", "2020-02-01"], dtype="datetime64[ns]")
    cube = xr.DataArray(np.full((2, 6, 1, 1), 1000.0), dims=("time", "band", "y", "x"),
                        coords={"time": t, "band": ROLES})
    zeit.unmix(cube, "souza2005", scale=10000.0, nodata=None)


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk, "warmup": warmup}]
