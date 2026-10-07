"""CCDC (Zeit) for tsv: continuous change detection on multiband surface reflectance.

Zeit's CCDC is a port of the original MATLAB code (Zhu & Woodcock 2014). Like
the original it works on Landsat-style surface reflectance x 10000 (Blue, Green,
Red, NIR, SWIR1, SWIR2 [, brightness temperature in deg C x 100]), Fmask codes
and Python ordinal days. tsv maps the bands of each date to these roles and
passes them in ctx["bands"] / ctx["fmask"]; this module converts the values to
the x 10000 convention ("Reflectance units" parameter) and back for display.

Raster maps come from Zeit's batch function, which returns the segments' dates
and harmonic coefficients but not the original's change magnitude/probability:
the magnitude of a break is therefore the jump between the two segment models
at the break date (per band, x 10000), and the map shows its Euclidean norm over
the detection bands (Green..SWIR2), plus the signed NDVI change of that jump.
Pixel runs use the single-pixel fit, which also gives the original's change
probability and per-band magnitude.
"""
import datetime as _dt
import math

import numpy as np

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
        "a quality band (Fmask or Landsat QA_PIXEL) is strongly recommended. Break magnitude in "
        "the maps = jump between the segment models at the break (reflectance x 10000)."),
    "requires": {"time": "any", "min_dates": 24, "min_per_year": 4,
                 "bands": list(ROLES), "optional_bands": ["thermal"]},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "units", "label": "Reflectance units", "type": "enum", "default": "auto", "options": UNITS,
         "labels": UNIT_LABELS,
         "help": "CCDC's thresholds are defined on reflectance x 10000. Automatic: median <= 2 -> "
                 "reflectance 0-1; median >= 6500 -> Landsat Collection 2 Level-2 digital numbers "
                 "(x 0.0000275 - 0.2); otherwise already x 10000. tsv applies the files' scale/offset "
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
         "help": "Segments kept per pixel in raster runs."},
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

def _decimal_year(ordinal):
    """Python ordinal day -> decimal year (same convention as tsv: year + (day of year - 1) / days)."""
    d = _dt.date.fromordinal(int(ordinal))
    n = 366 if (d.year % 4 == 0 and d.year % 100 != 0) or d.year % 400 == 0 else 365
    return d.year + (d.toordinal() - _dt.date(d.year, 1, 1).toordinal()) / n


def _decimal_years(ordinals):
    """Vectorized _decimal_year; NaN / <= 0 -> NaN."""
    o = np.asarray(ordinals, dtype=np.float64)
    out = np.full(o.shape, np.nan)
    ok = np.isfinite(o) & (o > 0)
    if ok.any():
        days = (o[ok].astype(np.int64) - UNIX_EPOCH_ORDINAL).astype("datetime64[D]")
        year = days.astype("datetime64[Y]")
        y = year.astype(np.int64) + 1970
        doy = (days - year.astype("datetime64[D]")).astype(np.int64)
        leap = ((y % 4 == 0) & (y % 100 != 0)) | (y % 400 == 0)
        out[ok] = y + doy / np.where(leap, 366.0, 365.0)
    return out


def _fmt(ordinal):
    return _dt.date.fromordinal(int(ordinal)).isoformat() if ordinal and ordinal > 0 else "-"


def _terms(t):
    """Harmonic design terms on the datenum axis (zeit.ccdc.predict), t: ordinal days [...] -> [..., 8]."""
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


# ---------------------------------------------------------------------------
# Pixel
# ---------------------------------------------------------------------------

def _shown_curve(seg_coefs, roles, mode, shown, ctx, t):
    """The model in the units of the series tsv shows (one band or a normalized difference)."""
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
    from zeit.ccdc import run_ccdc

    roles, missing = _roles(ctx)
    if missing:
        return {"overlays": [], "rows": [["CCDC", "map the bands first: " + ", ".join(missing)]]}
    if not ctx.get("ordinal"):
        return {"overlays": [], "rows": [["CCDC", "needs real dates"]]}
    vals, roles, mode = _inputs(p, ctx)  # [B, T]
    qa = np.array(ctx["fmask"], dtype=np.int64)
    missing_obs = ~np.all(np.isfinite(vals), axis=0)
    qa[missing_obs] = 255
    vals = np.where(np.isfinite(vals), vals, 0.0)
    n_obs = int((qa < 255).sum())
    n_clear = int((qa < 2).sum())
    if n_clear < 12:
        return {"overlays": [], "rows": [["CCDC", f"not enough clear observations ({n_clear}, need 12)"]]}

    segs = run_ccdc(ctx["ordinal"], vals, qa, conseq_anom=int(p["conseq_anom"]),
                    chi2_prob_threshold=float(p["chi2_prob_threshold"]),
                    tmax_cg_prob_threshold=float(p["tmax_cg_prob_threshold"]), num_c=int(p["num_c"]),
                    thermal_band=6 if "thermal" in roles else None)
    rows = [["Segments", str(len(segs))], ["Observations", f"{n_obs} ({n_clear} clear)"],
            ["Units", UNIT_LABELS[UNITS.index(mode)]]]
    overlays = []
    breaks = []
    for k, s in enumerate(segs, 1):
        rows.append([f"Segment {k}", f"{_fmt(s['t_start'])} to {_fmt(s['t_end'])}, {s['num_obs']} obs"])
        if s["t_break"] > 0:
            mag = np.asarray(s["magnitude"], dtype=np.float64)
            norm = float(np.sqrt(np.sum(mag[DETECT] ** 2))) if mag.size > max(DETECT) else float("nan")
            rows.append([f"  break {_fmt(s['t_break'])}",
                         f"prob {s['change_prob']:.3g}, magnitude {norm:.4g} (refl x 10000)"
                         if math.isfinite(norm) else f"prob {s['change_prob']:.3g}"])
            breaks.append(_decimal_year(s["t_break"]))
        t0, t1 = int(s["t_start"]), int(s["t_end"])
        if t1 > t0:
            t = np.linspace(t0, t1, max(2, min(400, (t1 - t0) // 8 + 1)))
            y = _shown_curve(s["coefs"], roles, mode, ctx.get("shown"), ctx, t)
            if y is not None:
                overlays.append({"type": "line", "label": f"CCDC segment {k}",
                                 "x": [_decimal_year(v) for v in t],
                                 "y": [float(v) if math.isfinite(v) else None for v in y]})
    if breaks:
        overlays.append({"type": "vlines", "label": "CCDC breaks", "x": breaks})
    return {"overlays": overlays, "rows": rows}


# ---------------------------------------------------------------------------
# Raster
# ---------------------------------------------------------------------------

def chunk(p, stack, ctx):
    from zeit.ccdc import run_ccdc_batch

    T, h, w = stack.shape
    roles, missing = _roles(ctx)
    if missing or not ctx.get("ordinal"):
        raise ValueError("CCDC needs the bands " + ", ".join(ROLES) + " mapped and real dates")
    vals, roles, mode = _inputs(p, ctx)  # [B, T, h, w]
    B = vals.shape[0]
    qa = np.array(ctx["fmask"], dtype=np.int32)  # [T, h, w]
    qa[~np.all(np.isfinite(vals), axis=0)] = 255
    values = np.ascontiguousarray(vals.transpose(2, 3, 0, 1))  # [h, w, B, T], NaN = no observation
    qa = np.ascontiguousarray(qa.transpose(1, 2, 0))
    S = int(p["max_segments"])
    segs, counts = run_ccdc_batch(np.asarray(ctx["ordinal"], dtype=np.int32), values, qa, max_segments=S,
                                  return_coefs=True, conseq_anom=int(p["conseq_anom"]),
                                  chi2_prob_threshold=float(p["chi2_prob_threshold"]),
                                  tmax_cg_prob_threshold=float(p["tmax_cg_prob_threshold"]),
                                  num_c=int(p["num_c"]), thermal_band=6 if "thermal" in roles else None,
                                  n_jobs=-1)
    P = h * w
    counts = np.asarray(counts).reshape(P)
    segs = np.asarray(segs).reshape(P, S, 3 + B * 9)
    idx = np.arange(S)[None, :]
    used = idx < counts[:, None]                              # [P, S]
    tb = np.where(used, segs[:, :, 2], 0.0)                   # break ordinal, 0 = none
    has = tb > 0
    coefs = np.stack([segs[:, :, 3 + b * 9 + 1: 3 + b * 9 + 9] for b in range(B)], axis=2)  # [P, S, B, 8]

    # Jump between consecutive models at each break (needs a following segment).
    mag = np.full((P, S), np.nan)
    dndvi = np.full((P, S), np.nan)
    for s in range(S - 1):
        ok = has[:, s] & used[:, s + 1]
        if not ok.any():
            continue
        terms = _terms(tb[ok, s])                                          # [n, 8]
        before = np.einsum("nbk,nk->nb", coefs[ok, s], terms)            # [n, B]
        after = np.einsum("nbk,nk->nb", coefs[ok, s + 1], terms)
        mag[ok, s] = np.sqrt(np.sum((after - before)[:, DETECT] ** 2, axis=1))
        with np.errstate(invalid="ignore", divide="ignore"):
            nd0 = (before[:, 3] - before[:, 2]) / (before[:, 3] + before[:, 2])
            nd1 = (after[:, 3] - after[:, 2]) / (after[:, 3] + after[:, 2])
        dndvi[ok, s] = nd1 - nd0

    n_breaks = has.sum(axis=1).astype(np.float64)
    fitted = counts > 0
    years = _decimal_years(np.where(has, tb, np.nan))                     # [P, S]
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
        "n_breaks": np.where(fitted, n_breaks, np.nan),
        "largest_break": np.where(big, years[r, k], np.nan),
        "magnitude": np.where(big, mag[r, k], np.nan),
        "ndvi_change": np.where(big, dndvi[r, k], np.nan),
        "first_break": first,
        "last_break": last,
        "n_segments": np.where(fitted, counts.astype(np.float64), np.nan),
    }
    return {kk: v.reshape(h, w).astype(np.float32) for kk, v in res.items()}


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
