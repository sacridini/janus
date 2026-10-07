"""Land surface phenology (Zeit) for tsv: season start/peak/end per pixel.

Zeit's phenology (a port of phenofit) smooths the series (Whittaker or HANTS),
splits it into growing seasons, fits a double-logistic-type curve to each
season and derives the dates from the fitted curve. Time is in days: tsv's
decimal years are converted to "days since 1 January of the first year,
1-indexed" (Zeit's convention) and back.

Maps are per season, so a multi-year series is summarized: the median over all
seasons (a per-pixel "typical year"), the most recent season or the season that
peaks in a chosen year. Dates are mapped as day of year counted from 1 January
of the year of the season's peak, so seasons crossing a new year stay
continuous (a start in the previous November is negative, an end in the next
January is > 365).
"""
import datetime as _dt
import math
import warnings

import numpy as np

CURVES = ["beck", "elmore", "gu", "zhang", "ag", "klos", "dl"]
CURVE_LABELS = ["Beck (double logistic)", "Elmore (double logistic + greendown)", "Gu", "Zhang (logistic)",
                "AG (asymmetric Gaussian)", "Klosterman", "DL (double logistic, simple)"]

# Zeit's 21 output rows (src/phenology.cpp, zeit/xarray_api.py run_phenology).
M = {name: i for i, name in enumerate([
    "trs2_sos", "trs2_eos", "trs5_sos", "trs5_eos", "trs6_sos", "trs6_eos",
    "der_sos", "der_pos", "der_eos",
    "gu_ud", "gu_sd", "gu_dd", "gu_rd",
    "zhang_greenup", "zhang_maturity", "zhang_senescence", "zhang_dormancy",
    "los", "pop", "r2", "rmse"])}

# Start / end of season of each extraction method (rows of M). Zeit computes
# every method at once (its extraction_method argument is unused); the choice
# here only selects which dates are reported.
METHODS = {
    "trs5": ("trs5_sos", "trs5_eos"),
    "trs2": ("trs2_sos", "trs2_eos"),
    "trs6": ("trs6_sos", "trs6_eos"),
    "der": ("der_sos", "der_eos"),
    "zhang": ("zhang_greenup", "zhang_dormancy"),
    "gu": ("gu_ud", "gu_rd"),
}

MANIFEST = {
    "id": "phenology",
    "name": "Phenology",
    "category": "Phenology",
    "description": (
        "Land surface phenology as implemented in Zeit (a port of phenofit): smooths the series, "
        "splits it into growing seasons, fits a curve to each season and derives its start (SOS), "
        "peak (POS) and end (EOS). Needs a sub-annual vegetation index series (e.g. NDVI/EVI, "
        "8-16 day or monthly). Maps summarize the seasons per pixel (median, latest or a given "
        "year); dates are days of the year of the season's peak."),
    "requires": {"time": "any", "min_per_year": 6, "min_dates": 12},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "curve", "label": "Curve", "type": "enum", "default": "elmore", "options": CURVES,
         "labels": CURVE_LABELS, "help": "Function fitted to each season."},
        {"id": "method", "label": "Season start/end", "type": "enum", "default": "trs5",
         "options": list(METHODS),
         "labels": ["Threshold 50% of amplitude", "Threshold 20%", "Threshold 60%",
                    "Derivative (fastest rise / fall)", "Zhang (curvature: greenup / dormancy)",
                    "Gu (upturn / recession dates)"],
         "help": "How SOS and EOS are read from the fitted curve. POS is always the curve's maximum."},
        {"id": "smoothing", "label": "Smoothing", "type": "enum", "default": "whittaker",
         "options": ["whittaker", "hants", "none"], "labels": ["Whittaker", "HANTS (harmonics)", "None"],
         "help": "Smoothing used to find the seasons (the curves are fitted to the observations)."},
        {"id": "lambda", "label": "Whittaker lambda", "type": "float", "default": 10.0, "min": 0.1,
         "max": 10000.0, "help": "Whittaker only: larger = smoother."},
        {"id": "hants_frequencies", "label": "HANTS frequencies", "type": "int", "default": 3, "min": 1,
         "max": 8, "help": "HANTS only: number of harmonics per year."},
        {"id": "min_pixel_amplitude", "label": "Min pixel amplitude", "type": "float", "default": 0.1,
         "min": 0.0, "max": 1e9,
         "help": "Pixels whose series varies less than this (water, urban, bare soil) are skipped. "
                 "Use the index scale (e.g. 0.1 for NDVI, 1000 for NDVI x 10000)."},
        {"id": "min_amplitude", "label": "Min season amplitude", "type": "float", "default": 0.0,
         "min": 0.0, "max": 1e9, "help": "Seasons with a smaller rise above their troughs are dropped."},
        {"id": "min_season_length", "label": "Min season length (days)", "type": "int", "default": 0,
         "min": 0, "max": 1000, "help": "Seasons shorter than this (trough to trough) are dropped."},
        {"id": "season", "label": "Season mapped", "type": "enum", "default": "median",
         "options": ["median", "latest", "year"],
         "labels": ["Median of all seasons", "Most recent season", "Season peaking in the year below"],
         "help": "Raster runs: how the seasons of each pixel become one map."},
        {"id": "year", "label": "Year", "type": "int", "default": 0, "min": 0, "max": 3000,
         "help": "With 'Season peaking in the year below': calendar year of the peak (0 = latest)."},
    ],
    "outputs": [
        {"id": "sos", "name": "Start of season (SOS)", "colormap": "Viridis", "unit": "day of year"},
        {"id": "pos", "name": "Peak of season (POS)", "colormap": "Viridis", "unit": "day of year"},
        {"id": "eos", "name": "End of season (EOS)", "colormap": "Viridis", "unit": "day of year"},
        {"id": "los", "name": "Length of season (EOS - SOS)", "colormap": "Plasma", "unit": "days"},
        {"id": "peak", "name": "Peak value", "colormap": "Viridis", "unit": "value"},
        {"id": "amplitude", "name": "Amplitude", "colormap": "Plasma", "unit": "value"},
        {"id": "r2", "name": "Fit R2", "colormap": "Viridis", "unit": "R2"},
        {"id": "n_seasons", "name": "Seasons detected", "colormap": "Viridis", "unit": "count"},
    ],
    "chunk_cells": 20_000,  # 0.1-0.6 ms/pixel on all cores (Klosterman ~3.5): frequent progress
}


# ---------------------------------------------------------------------------
# Time conversions (tsv decimal year = Y + (day of year - 1) / days in Y)
# ---------------------------------------------------------------------------

def _diy(y):
    return 366.0 if (y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)) else 365.0


def _base_year(ctx):
    return int(math.floor(ctx["years"][0]))


def _to_days(years, base):
    """Decimal years -> days since base-01-01, 1-indexed (Zeit's day numbers)."""
    b = _dt.date(base, 1, 1).toordinal()
    out = []
    for yr in years:
        y = int(math.floor(yr))
        out.append(_dt.date(y, 1, 1).toordinal() - b + (yr - y) * _diy(y) + 1.0)
    return np.array(out, dtype=np.float64)


def _jan1_days(year, base):
    """Day number of 1 January of `year` (array) relative to base, vectorized."""
    y = np.asarray(year, dtype=np.int64)
    d = (y - 1970).astype("datetime64[Y]").astype("datetime64[D]") - np.datetime64(f"{base}-01-01", "D")
    return d.astype(np.int64).astype(np.float64) + 1.0


def _year_of_days(days, base):
    """Calendar year of Zeit day numbers (array, NaN -> -1)."""
    d = np.asarray(days, dtype=np.float64)
    ok = np.isfinite(d)
    out = np.full(d.shape, -1, dtype=np.int64)
    dates = np.datetime64(f"{base}-01-01", "D") + (np.floor(d[ok]) - 1).astype(np.int64).astype("timedelta64[D]")
    out[ok] = dates.astype("datetime64[Y]").astype(np.int64) + 1970
    return out


def _days_to_decimal_year(d, base):
    date = _dt.date(base, 1, 1) + _dt.timedelta(days=float(d) - 1.0)
    frac = float(d) - math.floor(float(d))
    return date.year + (date.timetuple().tm_yday - 1 + frac) / _diy(date.year)


def _fmt_day(d, base):
    return (_dt.date(base, 1, 1) + _dt.timedelta(days=float(d) - 1.0)).isoformat()


# ---------------------------------------------------------------------------
# Zeit call
# ---------------------------------------------------------------------------

def _max_seasons(ctx):
    years = ctx["years"]
    span = max(1.0, years[-1] - years[0])
    return int(min(200, 2 * math.ceil(span) + 2))  # room for two seasons per year


def _prepare(values):
    """[P, T] with NaN -> gap-filled values and weights (0 at gaps).

    Zeit's smoothers do not skip NaN, so gaps are filled by linear
    interpolation in time and given weight 0 (they do not pull the smoothing
    or the curve fit)."""
    v = np.array(values, dtype=np.float64, copy=True)
    ok = np.isfinite(v)
    w = ok.astype(np.float64)
    P, T = v.shape
    idx = np.arange(T)
    for p in np.nonzero((~ok).any(axis=1) & ok.any(axis=1))[0]:
        good = ok[p]
        v[p, ~good] = np.interp(idx[~good], idx[good], v[p, good])
    return v, w, ok.any(axis=1)


def _fit(p, values, days, S, n_jobs):
    from zeit._core.phenology import CurveType, fit_phenology_batch

    curve = {"beck": CurveType.BECK, "elmore": CurveType.ELMORE, "gu": CurveType.GU, "zhang": CurveType.ZHANG,
             "ag": CurveType.AG, "klos": CurveType.KLOS, "dl": CurveType.DL}[p["curve"]]
    v, w, has_data = _prepare(values)
    out = fit_phenology_batch(
        np.ascontiguousarray(v), np.ascontiguousarray(days), int(curve), extraction_method=0, max_seasons=S,
        whittaker_lambda=float(p["lambda"]), apply_whittaker=p["smoothing"] == "whittaker",
        apply_hants=p["smoothing"] == "hants", hants_frequencies=int(p["hants_frequencies"]),
        min_season_length=int(p["min_season_length"]), min_amplitude=float(p["min_amplitude"]),
        min_pixel_amplitude=float(p["min_pixel_amplitude"]), n_jobs=n_jobs,
        weights_array=np.ascontiguousarray(w))
    out = np.asarray(out)  # [21, P, S]
    out[:, ~has_data, :] = np.nan
    return out, v, w


def _season_table(p, out, v, w, days, base):
    """Per-season arrays [P, S] in Zeit day numbers + peak value / amplitude."""
    k_sos, k_eos = METHODS[p["method"]]
    sos, eos, pos = out[M[k_sos]], out[M[k_eos]], out[M["pop"]]
    fitted = np.isfinite(pos)
    # Peak value and amplitude from the observations (Zeit returns only
    # dates): peak = highest observation in [SOS, EOS]; amplitude = peak minus
    # the lowest observation within half a year of the peak (the troughs).
    P, S = pos.shape
    obs = np.where(w > 0, v, np.nan)  # [P, T]
    peak = np.full((P, S), np.nan)
    amp = np.full((P, S), np.nan)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", RuntimeWarning)  # windows without observations -> NaN
        for s in range(S):
            f = fitted[:, s]
            if not f.any():
                continue
            a, b, c = sos[f, s], eos[f, s], pos[f, s]
            inside = (days[None, :] >= a[:, None]) & (days[None, :] <= b[:, None])
            pk = np.nanmax(np.where(inside, obs[f], np.nan), axis=1)
            year = np.abs(days[None, :] - c[:, None]) <= 182.6
            low = np.nanmin(np.where(year, obs[f], np.nan), axis=1)
            peak[f, s] = pk
            amp[f, s] = pk - low
    return {"sos": sos, "pos": pos, "eos": eos, "los": eos - sos, "peak": peak, "amplitude": amp,
            "r2": out[M["r2"]], "fitted": fitted}


# ---------------------------------------------------------------------------
# Pixel
# ---------------------------------------------------------------------------

def pixel(p, ctx):
    years = ctx["years"]
    base = _base_year(ctx)
    days = _to_days(years, base)
    vals = np.array(ctx["values"], dtype=np.float64)
    n_valid = int(np.isfinite(vals).sum())
    if n_valid < 6:
        return {"overlays": [], "rows": [["Phenology", f"not enough data ({n_valid} valid)"]]}
    if years[-1] - years[0] < 1.0:
        return {"overlays": [], "rows": [["Phenology", "needs at least one year of data"]]}
    S = _max_seasons(ctx)
    out, v, w = _fit(p, vals[None, :], days, S, 1)
    t = _season_table(p, out, v, w, days, base)
    idx = [s for s in range(S) if t["fitted"][0, s]]
    if not idx:
        rng = np.nanmax(vals) - np.nanmin(vals)
        why = (f"pixel amplitude {rng:.4g} < {float(p['min_pixel_amplitude']):g}"
               if rng < float(p["min_pixel_amplitude"]) else "no growing season found")
        return {"overlays": [], "rows": [["Phenology", why]]}

    def dec(d):
        return _days_to_decimal_year(d, base)

    lines, px, py = [], [], []
    rows = [["Seasons", str(len(idx))]]
    for s in idx:
        sos, pos, eos = (float(t[k][0, s]) for k in ("sos", "pos", "eos"))
        for d in (sos, eos):
            if math.isfinite(d):
                lines.append(dec(d))
        if math.isfinite(pos):
            px.append(dec(pos))
            py.append(float(np.interp(pos, days, v[0])))
    shown = idx[-12:]  # the table stays readable on long series
    if len(shown) < len(idx):
        rows.append([f"  (latest {len(shown)} shown)", ""])
    for s in shown:
        sos, pos, eos, los, pk, r2 = (float(t[k][0, s]) for k in ("sos", "pos", "eos", "los", "peak", "r2"))
        label = f"Peak {_fmt_day(pos, base)}" if math.isfinite(pos) else "Season"
        parts = []
        if math.isfinite(sos) and math.isfinite(eos):
            parts.append(f"{_fmt_day(sos, base)} to {_fmt_day(eos, base)}")
        if math.isfinite(los):
            parts.append(f"{los:.0f} d")
        if math.isfinite(pk):
            parts.append(f"peak {pk:.4g}")
        if math.isfinite(r2):
            parts.append(f"R2 {r2:.2f}")
        rows.append([label, ", ".join(parts) or "-"])
    # Typical season: medians of the per-season values.
    pos_y = _year_of_days(t["pos"][0, idx], base)
    rel = {k: t[k][0, idx] - _jan1_days(pos_y, base) + 1.0 for k in ("sos", "pos", "eos")}
    med = {k: float(np.nanmedian(a)) for k, a in rel.items()}
    rows.append(["Median SOS / POS / EOS (DOY)", f"{med['sos']:.0f} / {med['pos']:.0f} / {med['eos']:.0f}"])
    rows.append(["Median length", f"{float(np.nanmedian(t['los'][0, idx])):.0f} days"])

    overlays = []
    if lines:
        overlays.append({"type": "vlines", "label": "Season start/end", "x": lines})
    if px:
        overlays.append({"type": "markers", "label": "Season peak", "x": px, "y": py})
    return {"overlays": overlays, "rows": rows}


# ---------------------------------------------------------------------------
# Raster
# ---------------------------------------------------------------------------

def chunk(p, stack, ctx):
    T, h, wd = stack.shape
    base = _base_year(ctx)
    days = _to_days(ctx["years"], base)
    S = _max_seasons(ctx)
    values = np.ascontiguousarray(stack.reshape(T, -1).T)
    out, v, w = _fit(p, values, days, S, -1)
    t = _season_table(p, out, v, w, days, base)
    fitted = t["fitted"]
    P = fitted.shape[0]

    pos_year = _year_of_days(t["pos"], base)  # [P, S], -1 where unfitted
    jan1 = np.where(fitted, _jan1_days(np.where(fitted, pos_year, 1970), base), np.nan)
    per = {k: t[k] - jan1 + 1.0 for k in ("sos", "pos", "eos")}
    per.update({k: t[k] for k in ("los", "peak", "amplitude", "r2")})

    mode = p["season"]
    res = {}
    if mode == "median":
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", RuntimeWarning)  # pixels without seasons -> NaN
            for k, a in per.items():
                res[k] = np.nanmedian(np.where(fitted, a, np.nan), axis=1)
    else:
        if mode == "year" and int(p["year"]) > 0:
            sel = fitted & (pos_year == int(p["year"]))
        else:
            sel = fitted
        # Latest selected season of each pixel.
        cols = np.where(sel, np.arange(fitted.shape[1])[None, :], -1).max(axis=1)
        has = cols >= 0
        r = np.arange(P)
        c = np.maximum(cols, 0)
        for k, a in per.items():
            res[k] = np.where(has, a[r, c], np.nan)
    res["n_seasons"] = np.where(np.isfinite(values).any(axis=1), fitted.sum(axis=1).astype(np.float64), np.nan)
    return {k: np.asarray(a, dtype=np.float32).reshape(h, wd) for k, a in res.items()}


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
