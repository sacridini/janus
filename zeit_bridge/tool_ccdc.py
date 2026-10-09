"""CCDC (zeit.ccdc) for Janus: continuous change detection on multiband surface reflectance.

Zeit's CCDC is a port of the original MATLAB code (Zhu & Woodcock 2014). Like
the original it works on Landsat-style surface reflectance x 10000 (Blue, Green,
Red, NIR, SWIR1, SWIR2 [, brightness temperature in deg C x 100]) and Fmask
codes. Janus maps the bands of each date to these roles and passes them in
ctx["bands"] / ctx["fmask"]; this module converts the values to the x 10000
convention ("Reflectance units" parameter) and back for display.

Zeit returns the segments' dates and harmonic models. The magnitude of a break
is the jump between the two segment models at the break date (per band, x
10000) and the maps show its Euclidean norm over the detection bands
(Green..SWIR2), as zeit.extract_events does, plus the signed NDVI change of
that jump. Pixel runs and maps use the same numbers.
"""
import datetime as _dt
import math

import numpy as np

import zeit_common as zc

ROLES = ["blue", "green", "red", "nir", "swir1", "swir2"]
DETECT = [1, 2, 3, 4, 5]  # Green..SWIR2 (Zeit's default detection bands with >= 6 bands)
UNIX_EPOCH_ORDINAL = 719163

UNITS = ["auto", "x10000", "unit", "landsat_c2"]
UNIT_LABELS = ["Automatic (from the values)", "Reflectance x 10000", "Reflectance 0-1",
               "Landsat C2 L2 digital numbers"]

MANIFEST = {
    "id": "ccdc",
    "name": "CCDC",
    "category": "Change detection",
    "description": (
        "Continuous Change Detection and Classification (Zhu & Woodcock 2014), Zeit's port of the "
        "original MATLAB code. Fits harmonic models to every band of a surface-reflectance series "
        "(Landsat-style: blue, green, red, NIR, SWIR1, SWIR2, optional thermal) with a quality "
        "band, and starts a new model when consecutive clear observations depart from the "
        "current one. Needs several bands per date (map them to the roles below) and real dates; "
        "a quality band (Fmask or Landsat QA_PIXEL) is strongly recommended. Break magnitude = "
        "jump between the segment models at the break over the detection bands (reflectance x 10000)."),
    "requires": {"time": "any", "min_dates": 24, "min_per_year": 4,
                 "bands": list(ROLES), "optional_bands": ["thermal"]},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "units", "label": "Reflectance units", "type": "enum", "default": "auto", "options": UNITS,
         "labels": UNIT_LABELS,
         "help": "CCDC's thresholds are defined on reflectance x 10000. Automatic: median <= 2 -> "
                 "reflectance 0-1; median >= 6500 -> Landsat Collection 2 Level-2 digital numbers "
                 "(x 0.0000275 - 0.2); otherwise already x 10000. Janus applies the files' scale/offset "
                 "metadata first, when present."},
        {"id": "conseq_anom", "label": "Consecutive anomalies", "type": "int", "default": 6, "min": 3, "max": 20,
         "help": "Consecutive anomalous clear observations needed to flag a change (the original's 'conse')."},
        {"id": "chi2_prob_threshold", "label": "Change probability", "type": "float", "default": 0.99,
         "min": 0.5, "max": 0.999999,
         "help": "Chi-square probability of the change threshold over the detection bands."},
        {"id": "tmax_cg_prob_threshold", "label": "Outlier probability", "type": "float",
         "default": 0.999999, "min": 0.9, "max": 0.99999999,
         "help": "Chi-square probability above which a single observation is an outlier, not change."},
        {"id": "num_c", "label": "Max coefficients", "type": "enum", "default": "8", "options": ["4", "6", "8"],
         "labels": ["4 (trend + annual)", "6 (+ semi-annual)", "8 (+ 4-month)"],
         "help": "Maximum number of harmonic model coefficients per band."},
        {"id": "max_segments", "label": "Max segments", "type": "int", "default": 8, "min": 2, "max": 20,
         "help": "Segments kept per pixel."},
    ],
    "outputs": [
        {"id": "n_breaks", "name": "Breaks", "colormap": "Viridis", "unit": "count"},
        {"id": "largest_break", "name": "Largest break date", "colormap": "Viridis", "unit": "year"},
        {"id": "magnitude", "name": "Largest break magnitude", "colormap": "Plasma", "unit": "refl x 10000"},
        {"id": "ndvi_change", "name": "NDVI change at largest break", "colormap": "BrBG", "unit": "NDVI"},
        {"id": "first_break", "name": "First break date", "colormap": "Viridis", "unit": "year"},
        {"id": "last_break", "name": "Last break date", "colormap": "Viridis", "unit": "year"},
        {"id": "n_segments", "name": "Segments", "colormap": "Viridis", "unit": "count"},
    ],
    "chunk_cells": 20_000,  # ~0.1 ms/pixel on all cores (1 ms on one); segment buffers ~85 MB per chunk
}


# ---------------------------------------------------------------------------
# Units
# ---------------------------------------------------------------------------

def _units(p, ctx, optical):
    """Unit mode of the optical bands; 'auto' is decided once per job (ctx persists across chunks)."""
    if p["units"] != "auto":
        return p["units"]
    if "_ccdc_units" in ctx:
        return ctx["_ccdc_units"]
    v = np.concatenate([np.ravel(a) for a in optical])
    v = v[np.isfinite(v)]
    if v.size > 200_000:
        v = v[:: v.size // 200_000]
    mode = "x10000"
    if v.size:
        med = float(np.median(v))
        mode = "unit" if med <= 2.0 else "landsat_c2" if med >= 6500.0 else "x10000"
    ctx["_ccdc_units"] = mode
    return mode


def _to_x10000(v, mode):
    if mode == "unit":
        return v * 10000.0
    if mode == "landsat_c2":
        return (v * 2.75e-5 - 0.2) * 10000.0
    return v


def _from_x10000(v, mode):
    if mode == "unit":
        return v / 10000.0
    if mode == "landsat_c2":
        return (v / 10000.0 + 0.2) / 2.75e-5
    return v


def _thermal_to_c100(v, mode):
    """Brightness temperature -> deg C x 100 (Kelvin, deg C, C2 ST digital numbers or already x 100)."""
    f = v[np.isfinite(v)]
    med = float(np.median(f)) if f.size else 0.0
    if mode == "landsat_c2" and med > 1000.0:  # C2 ST_B10 digital numbers -> Kelvin
        v = v * 0.00341802 + 149.0
        med = med * 0.00341802 + 149.0
    if 150.0 < med < 400.0:  # Kelvin
        return (v - 273.15) * 100.0
    if -100.0 < med <= 150.0:  # deg C
        return v * 100.0
    return v


def _thermal_from_c100(v, ref, mode):
    """Inverse of _thermal_to_c100, using the shown thermal values `ref` to recover the units."""
    f = ref[np.isfinite(ref)]
    med = float(np.median(f)) if f.size else 0.0
    if mode == "landsat_c2" and med > 1000.0:
        return (v / 100.0 + 273.15 - 149.0) / 0.00341802
    if 150.0 < med < 400.0:
        return v / 100.0 + 273.15
    if -100.0 < med <= 150.0:
        return v / 100.0
    return v


# ---------------------------------------------------------------------------
# Dates and models
# ---------------------------------------------------------------------------

def _fmt(ordinal):
    return _dt.date.fromordinal(int(ordinal)).isoformat() if ordinal and ordinal > 0 else "-"


def _ordinals(t):
    """datetime64 -> Python ordinal days (NaT -> 0)."""
    t = np.asarray(t)
    days = t.astype("datetime64[D]").astype(np.int64) + UNIX_EPOCH_ORDINAL
    return np.where(np.isnat(t), 0, days)


def _terms(t):
    """Harmonic design terms on the datenum axis of Zeit's coefs, t: ordinal days [...] -> [..., 8]."""
    t = np.asarray(t, dtype=np.float64) + 366.0
    w = 2.0 * np.pi / 365.25
    return np.stack([np.ones_like(t), t, np.cos(w * t), np.sin(w * t), np.cos(2 * w * t), np.sin(2 * w * t),
                     np.cos(3 * w * t), np.sin(3 * w * t)], axis=-1)


def _roles(ctx):
    bands = ctx.get("bands") or {}
    missing = [r for r in ROLES if r not in bands]
    roles = list(ROLES) + (["thermal"] if "thermal" in bands else [])
    return roles, missing


def _inputs(p, ctx):
    """Bands in Zeit's order and units: (array [B, ...], roles, unit mode)."""
    bands = ctx["bands"]
    roles, _ = _roles(ctx)
    mode = _units(p, ctx, [bands[r] for r in ROLES])
    arrs = [_to_x10000(bands[r], mode) for r in ROLES]
    if "thermal" in roles:
        arrs.append(_thermal_to_c100(bands["thermal"], mode))
    return np.stack(arrs), roles, mode


def _segments(p, ctx, vals, qa):
    """zeit.ccdc on bands [B, T, rows, cols] and Fmask codes [T, rows, cols]. Returns
    {counts [P], start/end/brk ordinal days [P, S] (0 = none), coefs [P, S, B, 8],
    used [P, S]}."""
    import xarray as xr
    import zeit

    B, T, h, w = vals.shape
    roles, _ = _roles(ctx)
    S = int(p["max_segments"])
    data = xr.DataArray(vals.transpose(1, 0, 2, 3), dims=("time", "band", "y", "x"),
                        coords={"time": zc.times(ctx), "band": roles})
    ds = zeit.ccdc(data, qa=qa, max_segments=S, conseq_anom=int(p["conseq_anom"]),
                   chi2_prob_threshold=float(p["chi2_prob_threshold"]),
                   tmax_cg_prob_threshold=float(p["tmax_cg_prob_threshold"]), num_c=int(p["num_c"]),
                   thermal_band="thermal" if "thermal" in roles else None, nodata=None,
                   n_jobs=ctx.get("n_jobs", -1))
    P = h * w
    seg = lambda a: np.asarray(a).reshape(S, P).T                  # (segment, y, x) -> [P, S]
    counts = np.nan_to_num(zc.grid(ds, "n_segments").reshape(P)).astype(np.int64)
    used = np.arange(S)[None, :] < counts[:, None]
    return {
        "counts": counts,
        "used": used,
        "start": seg(_ordinals(ds["t_start"].values)),
        "end": seg(_ordinals(ds["t_end"].values)),
        "brk": np.where(used, seg(_ordinals(ds["t_break"].values)), 0),
        "coefs": np.asarray(ds["coefs"].values, dtype=np.float64).transpose(3, 4, 0, 1, 2).reshape(P, S, B, -1),
    }


def _jumps(sg):
    """Per break: (magnitude over the detection bands, NDVI change) [P, S], NaN where a
    segment ends without a break or no segment follows it."""
    P, S = sg["brk"].shape
    has = sg["brk"] > 0
    mag = np.full((P, S), np.nan)
    dndvi = np.full((P, S), np.nan)
    for s in range(S - 1):
        ok = has[:, s] & sg["used"][:, s + 1]
        if not ok.any():
            continue
        terms = _terms(sg["brk"][ok, s])                                          # [n, 8]
        before = np.einsum("nbk,nk->nb", sg["coefs"][ok, s], terms)             # [n, B]
        after = np.einsum("nbk,nk->nb", sg["coefs"][ok, s + 1], terms)
        mag[ok, s] = np.sqrt(np.sum((after - before)[:, DETECT] ** 2, axis=1))
        with np.errstate(invalid="ignore", divide="ignore"):
            nd0 = (before[:, 3] - before[:, 2]) / (before[:, 3] + before[:, 2])
            nd1 = (after[:, 3] - after[:, 2]) / (after[:, 3] + after[:, 2])
        dndvi[ok, s] = nd1 - nd0
    return mag, dndvi


def _prepare(p, ctx):
    """(bands x 10000 [B, T, ...] with NaN, Fmask [T, ...] with 255 where a band is missing, roles, units)."""
    vals, roles, mode = _inputs(p, ctx)
    qa = np.array(ctx["fmask"], dtype=np.int32)
    qa[~np.all(np.isfinite(vals), axis=0)] = 255
    return vals, qa, roles, mode


# ---------------------------------------------------------------------------
# Pixel
# ---------------------------------------------------------------------------

def _shown_curve(seg_coefs, roles, mode, shown, ctx, t):
    """The model in the units of the series Janus shows (one band or a normalized difference)."""
    def band(role):
        if role not in roles:
            return None
        b = roles.index(role)
        v = _terms(t) @ np.asarray(seg_coefs[b], dtype=np.float64)
        if role == "thermal":
            return _thermal_from_c100(v, np.asarray(ctx["bands"]["thermal"]), mode)
        return _from_x10000(v, mode)

    if not shown:
        return None
    nd = shown.get("nd")
    if nd and len(nd) == 2:
        a, b = band(nd[0]), band(nd[1])
        if a is None or b is None:
            return None
        with np.errstate(invalid="ignore", divide="ignore"):
            return np.where(a + b != 0, (a - b) / (a + b), np.nan)
    return band(shown.get("band")) if shown.get("band") else None


def pixel(p, ctx):
    roles, missing = _roles(ctx)
    if missing:
        return {"overlays": [], "rows": [["CCDC", "map the bands first: " + ", ".join(missing)]]}
    if not ctx.get("ordinal"):
        return {"overlays": [], "rows": [["CCDC", "needs real dates"]]}
    vals, qa, roles, mode = _prepare(p, ctx)  # [B, T], [T]
    n_obs = int((qa < 255).sum())
    n_clear = int((qa < 2).sum())
    if n_clear < 12:
        return {"overlays": [], "rows": [["CCDC", f"not enough clear observations ({n_clear}, need 12)"]]}

    sg = _segments(p, ctx, vals[:, :, None, None], qa[:, None, None])
    mag, dndvi = _jumps(sg)
    n = int(sg["counts"][0])
    days = np.asarray(ctx["ordinal"], dtype=np.int64)
    rows = [["Segments", str(n)], ["Observations", f"{n_obs} ({n_clear} clear)"],
            ["Units", UNIT_LABELS[UNITS.index(mode)]]]
    overlays = []
    breaks = []
    for k in range(n):
        t0, t1, tb = (int(sg[key][0, k]) for key in ("start", "end", "brk"))
        clear = int(((days >= t0) & (days <= t1) & (qa < 2)).sum())
        rows.append([f"Segment {k + 1}", f"{_fmt(t0)} to {_fmt(t1)}, {clear} clear obs"])
        if tb > 0:
            m, d = mag[0, k], dndvi[0, k]
            rows.append([f"  break {_fmt(tb)}",
                         f"magnitude {m:.4g} (refl x 10000), NDVI change {d:+.3f}" if math.isfinite(m)
                         else "end of the series' model"])
            breaks.append(float(zc.ordinal_decimal_years([tb])[0]))
        if t1 > t0:
            t = np.linspace(t0, t1, max(2, min(400, (t1 - t0) // 8 + 1)))
            y = _shown_curve(sg["coefs"][0, k], roles, mode, ctx.get("shown"), ctx, t)
            if y is not None:
                overlays.append({"type": "line", "label": f"CCDC segment {k + 1}",
                                 "x": [float(v) for v in zc.ordinal_decimal_years(t)],
                                 "y": [float(v) if math.isfinite(v) else None for v in y]})
    if breaks:
        overlays.append({"type": "vlines", "label": "CCDC breaks", "x": breaks})
    return {"overlays": overlays, "rows": rows}


# ---------------------------------------------------------------------------
# Raster
# ---------------------------------------------------------------------------

def chunk(p, stack, ctx):
    T, h, w = stack.shape
    roles, missing = _roles(ctx)
    if missing or not ctx.get("ordinal"):
        raise ValueError("CCDC needs the bands " + ", ".join(ROLES) + " mapped and real dates")
    vals, qa, roles, mode = _prepare(p, ctx)  # [B, T, h, w], [T, h, w]
    sg = _segments(p, ctx, vals, qa)
    mag, dndvi = _jumps(sg)
    P, S = sg["brk"].shape
    has = sg["brk"] > 0
    fitted = sg["counts"] > 0
    years = zc.ordinal_decimal_years(np.where(has, sg["brk"], np.nan))   # [P, S]
    first = np.full(P, np.nan)
    last = np.full(P, np.nan)
    anyb = has.any(axis=1)
    first[anyb] = years[anyb, np.argmax(has[anyb], axis=1)]
    last[anyb] = years[anyb, S - 1 - np.argmax(has[anyb, ::-1], axis=1)]
    score = np.where(np.isfinite(mag), mag, -1.0)
    k = np.argmax(score, axis=1)
    r = np.arange(P)
    big = score[r, k] >= 0
    res = {
        "n_breaks": np.where(fitted, has.sum(axis=1), np.nan),
        "largest_break": np.where(big, years[r, k], np.nan),
        "magnitude": np.where(big, mag[r, k], np.nan),
        "ndvi_change": np.where(big, dndvi[r, k], np.nan),
        "first_break": first,
        "last_break": last,
        "n_segments": np.where(fitted, sg["counts"], np.nan),
    }
    return zc.chunk_outputs(res, (h, w))


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
