"""CODED (zeit.coded) for Janus: Continuous Degradation Detection (Bullock, Woodcock &
Olofsson 2020) on the NDFI of every observation.

Per pixel, Zeit unmixes each observation (see tool_ndfi.py), fits a model of the NDFI
(constant and an annual harmonic) over a training period and monitors after it:
`consec` observations in a row more than `thresh` RMSEs below the model are a change,
followed by a new model of the next `min_years`. A change after which the land is
still forest (model mean NDFI >= forest_ndfi) is degradation (selective logging,
understory fire); one after which it is not, deforestation. Janus has no training
points here, so forest is told by that NDFI threshold (Zeit's rule without them).
"""
import numpy as np

import zeit_common as zc

ROLES = zc.LANDSAT_ROLES
STRATA = ["Forest", "Non-forest", "Degradation", "Deforestation", "Disturbance"]
TYPES = {1: "degradation", 2: "deforestation", 3: "disturbance (too few observations after it to tell)"}

MANIFEST = {
    "id": "coded",
    "name": "CODED (degradation)",
    "category": "Change detection",
    "description": (
        "Continuous Degradation Detection (Bullock et al. 2020), as implemented in Zeit: the NDFI of every "
        "observation (spectral unmixing, Souza et al. 2005) is modelled over a training period, and "
        "observations falling well below the model afterwards are a change. A change after which the "
        "land is still forest is degradation (selective logging, understory fire); otherwise it is "
        "deforestation. Needs Landsat-style surface reflectance (blue, green, red, NIR, SWIR1, SWIR2) "
        "with real dates; a quality band is strongly recommended."),
    "requires": {"time": "any", "min_dates": 12, "min_per_year": 2, "bands": list(ROLES)},
    "modes": ["pixel", "raster"],
    "params": [
        zc.units_param("The unmixing endmembers are reflectances."),
        {"id": "monitor_start", "label": "Monitoring start (year)", "type": "float", "default": 0.0, "min": 0.0,
         "max": 2100.0, "help": "Year the monitoring starts; the training period is the years before it. "
                                "0 = the training period starts with the series."},
        {"id": "train_years", "label": "Training period (years)", "type": "float", "default": 3.0, "min": 1.0,
         "max": 20.0, "help": "Years of the training period, whose NDFI model the monitoring compares with."},
        {"id": "consec", "label": "Consecutive observations", "type": "int", "default": 3, "min": 1, "max": 20,
         "help": "Observations in a row beyond the threshold that make a change."},
        {"id": "thresh", "label": "Threshold (RMSEs)", "type": "float", "default": 3.0, "min": 0.5, "max": 20.0,
         "help": "How far below the model, in RMSEs of the training fit, an observation must fall."},
        {"id": "min_years", "label": "Years after a change", "type": "float", "default": 3.0, "min": 0.5,
         "max": 20.0, "help": "Years modelled as the land cover after a change, and the least time between "
                              "two changes."},
        {"id": "min_obs", "label": "Min observations", "type": "int", "default": 6, "min": 3, "max": 100,
         "help": "Least observations to fit a model (training, or after a change)."},
        {"id": "direction", "label": "Changes", "type": "enum", "default": "loss", "options": ["loss", "both"],
         "labels": ["NDFI drops (CODED)", "Drops and rises"],
         "help": "Only drops of the NDFI (disturbances, as CODED), or rises too."},
        {"id": "max_events", "label": "Max changes", "type": "int", "default": 3, "min": 1, "max": 10,
         "help": "Changes kept per pixel."},
        {"id": "forest_ndfi", "label": "Forest NDFI", "type": "float", "default": 0.5, "min": -1.0, "max": 1.0,
         "help": "Forest where a model's mean NDFI is at least this (before: forest or not; after a change: "
                 "degradation or deforestation). Closed forest is near 1; open woodlands are naturally lower."},
        {"id": "cloud_threshold", "label": "Max cloud fraction", "type": "float", "default": 0.05, "min": 0.0,
         "max": 1.0, "help": "Observations whose unmixed cloud fraction is above this are left out (CODED: "
                             "0.05); 1 keeps them all."},
    ],
    "outputs": [
        {"id": "strata", "name": "Strata", "colormap": "Strata", "unit": "class", "classes": STRATA},
        {"id": "first_change", "name": "First change date", "colormap": "Viridis", "unit": "year"},
        {"id": "first_ndfi_change", "name": "NDFI change at the first change", "colormap": "BrBG", "unit": "NDFI"},
        {"id": "last_change", "name": "Last change date", "colormap": "Viridis", "unit": "year"},
        {"id": "n_events", "name": "Changes", "colormap": "Viridis", "unit": "count"},
        {"id": "ndfi_mean", "name": "Mean NDFI of the training period", "colormap": "NDVI", "unit": "NDFI"},
    ],
    "chunk_cells": 20_000,  # 6 bands and 7 fractions of every date in memory
}


def _run(p, ctx):
    import zeit
    cube, _ = zc.landsat_cube(p, ctx, ROLES)
    start = float(p["monitor_start"])
    ct = float(p["cloud_threshold"])
    return zeit.coded(cube, start=start if start > 0 else None, train_years=float(p["train_years"]),
                      consec=int(p["consec"]), thresh=float(p["thresh"]), min_years=float(p["min_years"]),
                      min_obs=int(p["min_obs"]), direction=p["direction"], max_events=int(p["max_events"]),
                      forest_ndfi=float(p["forest_ndfi"]), scale=10000.0, cloud_threshold=ct if ct < 1.0 else None,
                      nodata=None, n_jobs=ctx.get("n_jobs", -1))


def _check(ctx):
    missing = zc.missing_roles(ctx, ROLES)
    if missing:
        return "map the bands first: " + ", ".join(missing)
    if not ctx.get("ordinal"):
        return "needs real dates"
    return None


def _events(ds):
    """(dates [E, ...] as decimal years, NDFI change [E, ...], type [E, ...]) of the changes."""
    return (zc.grid(ds, "t_change"), zc.grid(ds, "ndfi_change"), zc.grid(ds, "type"))


def pixel(p, ctx):
    import zeit
    why = _check(ctx)
    if why:
        return {"overlays": [], "rows": [["CODED", why]]}
    ds = _run(p, ctx)
    dates, dndfi, kind = (a[:, 0, 0] for a in _events(ds))
    post = zc.grid(ds, "post_ndfi")[:, 0, 0]
    mean, rmse = float(zc.grid(ds, "ndfi_mean")[0, 0]), float(zc.grid(ds, "rmse")[0, 0])
    rows = []
    if not np.isfinite(mean):
        rows.append(["Training", "no model (too few clear observations in the training period)"])
    else:
        forest = int(zc.grid(ds, "forest")[0, 0]) == 1
        rows += [["Training", f"{'forest' if forest else 'not forest'}, mean NDFI {mean:.3f}, RMSE {rmse:.3f}"],
                 ["Training observations", str(int(zc.grid(ds, "n_train")[0, 0]))]]
    stratum = int(zc.grid(ds, "strata")[0, 0])
    rows.append(["Stratum", STRATA[stratum - 1] if 1 <= stratum <= len(STRATA) else "-"])
    breaks = []
    for k in range(len(dates)):
        if not np.isfinite(dates[k]):
            continue
        breaks.append(float(dates[k]))
        text = f"{TYPES.get(int(kind[k]), 'change')}, NDFI change {dndfi[k]:+.3f}"
        if np.isfinite(post[k]):
            text += f", NDFI after {post[k]:.3f}"
        rows.append([f"Change {zc.fmt_year(dates[k])}", text])
    if not breaks:
        rows.append(["Changes", "none"])
    overlays = []
    if zc.shown_is_index(ctx):
        ct = float(p["cloud_threshold"])
        cube, _ = zc.landsat_cube(p, ctx, ROLES)
        ndfi = zeit.unmix(cube, "souza2005", scale=10000.0, cloud_threshold=ct if ct < 1.0 else None,
                          nodata=None)["ndfi"].values
        overlays.append(zc.series_overlay("NDFI", ctx, ndfi, kind="markers"))
        if np.isfinite(mean):
            t0 = float(ds.attrs.get("train_start", ctx["years"][0]))
            t1 = t0 + float(p["train_years"])
            overlays.append({"type": "line", "label": "CODED training mean NDFI", "x": [t0, t1], "y": [mean, mean]})
    if breaks:
        overlays.append({"type": "vlines", "label": "CODED changes", "x": breaks})
    return {"overlays": overlays, "rows": rows}


def chunk(p, stack, ctx):
    why = _check(ctx)
    if why:
        raise ValueError("CODED: " + why)
    h, w = stack.shape[1:]
    ds = _run(p, ctx)
    dates, dndfi, _ = _events(ds)                          # [E, h, w]
    has = np.isfinite(dates)
    anyc = has.any(axis=0)
    E = dates.shape[0]
    first_k = np.argmax(has, axis=0)
    last_k = E - 1 - np.argmax(has[::-1], axis=0)
    take = lambda a, k: np.take_along_axis(a, k[None], axis=0)[0]
    strata = zc.grid(ds, "strata")
    mean = zc.grid(ds, "ndfi_mean")
    res = {
        "strata": np.where(strata > 0, strata, np.nan),
        "first_change": np.where(anyc, take(dates, first_k), np.nan),
        "first_ndfi_change": np.where(anyc, take(dndfi, first_k), np.nan),
        "last_change": np.where(anyc, take(dates, last_k), np.nan),
        "n_events": np.where(np.isfinite(mean) | anyc, has.sum(axis=0), np.nan),
        "ndfi_mean": mean,
    }
    return zc.chunk_outputs(res, (h, w))


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
