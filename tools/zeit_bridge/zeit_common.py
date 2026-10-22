"""Shared by the tool modules: Janus's series as inputs of Zeit's public API and
Zeit's results back as Janus's numbers.

Every tool runs the same code for a pixel and for a chunk: a pixel is a 1 x 1
chunk. Zeit takes the chunk as an xarray cube (time, y, x) whose `time`
coordinate holds the real dates (when the series has them), and returns an
xarray Dataset with the same (y, x) grid.
"""
import numpy as np

UNIX_EPOCH_ORDINAL = 719163  # date(1970, 1, 1).toordinal()


def times(ctx):
    """datetime64[ns] of each date, or None when the series has no real dates."""
    if not ctx.get("ordinal"):
        return None
    days = np.asarray(ctx["ordinal"], dtype=np.int64) - UNIX_EPOCH_ORDINAL
    return days.astype("datetime64[D]").astype("datetime64[ns]")


def cube(stack, ctx, synthetic_dates=False):
    """[T, rows, cols] -> DataArray (time, y, x), dated when the series is.

    synthetic_dates: without real dates, number the dates as consecutive days
    (for Zeit functions that need dates but only use their spacing)."""
    import xarray as xr
    t = times(ctx)
    if t is None and synthetic_dates:
        t = np.arange(stack.shape[0]).astype("datetime64[D]").astype("datetime64[ns]")
    return xr.DataArray(stack, dims=("time", "y", "x"), coords={"time": t} if t is not None else {})


def pixel_stack(ctx):
    """The pixel's series (ctx["values"]) as a [T, 1, 1] chunk."""
    return np.asarray(ctx["values"], dtype=np.float64)[:, None, None]


def grid(ds, name):
    """A variable of Zeit's result as float64 numpy (dates as Janus decimal years)."""
    a = ds[name].values
    if np.issubdtype(a.dtype, np.datetime64):
        return decimal_years(a)
    return np.asarray(a, dtype=np.float64)


def decimal_years(t):
    """datetime64 -> decimal year, Janus's convention (year + (day of year - 1) / days
    in the year); NaT -> NaN."""
    t = np.asarray(t)
    out = np.full(t.shape, np.nan)
    ok = ~np.isnat(t)
    if ok.any():
        days = t[ok].astype("datetime64[D]")
        year = days.astype("datetime64[Y]")
        y = year.astype(np.int64) + 1970
        doy = (days - year.astype("datetime64[D]")).astype(np.int64)
        leap = ((y % 4 == 0) & (y % 100 != 0)) | (y % 400 == 0)
        out[ok] = y + doy / np.where(leap, 366.0, 365.0)
    return out


def ordinal_decimal_years(ordinals):
    """Python ordinal days -> decimal years (NaN / <= 0 -> NaN)."""
    o = np.asarray(ordinals, dtype=np.float64)
    t = np.full(o.shape, np.datetime64("NaT"), dtype="datetime64[D]")
    ok = np.isfinite(o) & (o > 0)
    t[ok] = (o[ok].astype(np.int64) - UNIX_EPOCH_ORDINAL).astype("datetime64[D]")
    return decimal_years(t)


def fmt_year(y):
    """Decimal year -> 'YYYY-MM' (approximate, for the info rows)."""
    import math
    yr = int(math.floor(y + 1e-9))
    month = min(12, int((y - yr) * 12 + 1e-6) + 1)
    return f"{yr}-{month:02d}"


def chunk_outputs(res, shape):
    """{id: array} -> {id: float32 (rows, cols)}."""
    return {k: np.asarray(v, dtype=np.float32).reshape(shape) for k, v in res.items()}


# ---------------------------------------------------------------------------
# Surface reflectance (multiband tools: CCDC, NDFI, CODED, Tmask)
# ---------------------------------------------------------------------------

LANDSAT_ROLES = ["blue", "green", "red", "nir", "swir1", "swir2"]

UNITS = ["auto", "x10000", "unit", "landsat_c2"]
UNIT_LABELS = ["Automatic (from the values)", "Reflectance x 10000", "Reflectance 0-1",
               "Landsat C2 L2 digital numbers"]


def units_param(what):
    """The "Reflectance units" parameter of a multiband tool; `what` says why the units matter."""
    return {"id": "units", "label": "Reflectance units", "type": "enum", "default": "auto", "options": UNITS,
            "labels": UNIT_LABELS,
            "help": what + " Automatic: median <= 2 -> reflectance 0-1; median >= 6500 -> Landsat Collection 2 "
                    "Level-2 digital numbers (x 0.0000275 - 0.2); otherwise already x 10000. Janus applies the "
                    "files' scale/offset metadata first, when present."}


def reflectance_units(p, ctx, optical):
    """Unit mode of the optical bands; 'auto' is decided once per job (ctx persists across chunks)."""
    if p["units"] != "auto":
        return p["units"]
    cache = ctx.setdefault("cache", {})  # a job's chunks share it (see the bridge's Inputs)
    if "units" in cache:
        return cache["units"]
    v = np.concatenate([np.ravel(a) for a in optical])
    v = v[np.isfinite(v)]
    if v.size > 200_000:
        v = v[:: v.size // 200_000]
    mode = "x10000"
    if v.size:
        med = float(np.median(v))
        mode = "unit" if med <= 2.0 else "landsat_c2" if med >= 6500.0 else "x10000"
    cache["units"] = mode
    return mode


def to_x10000(v, mode):
    if mode == "unit":
        return v * 10000.0
    if mode == "landsat_c2":
        return (v * 2.75e-5 - 0.2) * 10000.0
    return v


def from_x10000(v, mode):
    if mode == "unit":
        return v / 10000.0
    if mode == "landsat_c2":
        return (v / 10000.0 + 0.2) / 2.75e-5
    return v


def missing_roles(ctx, roles):
    bands = ctx.get("bands") or {}
    return [r for r in roles if r not in bands]


def landsat_cube(p, ctx, roles=LANDSAT_ROLES, clear_only=True):
    """The mapped bands as reflectance x 10000 in a DataArray (time, band, y, x) whose band
    names are the roles, dated; observations the quality band rejects (Fmask cloud, shadow,
    snow or no observation) are NaN when clear_only. Returns (cube, unit mode)."""
    import xarray as xr
    if times(ctx) is None:
        raise ValueError("needs real dates (none were found in the file names or band descriptions)")
    bands = ctx["bands"]
    mode = reflectance_units(p, ctx, [bands[r] for r in roles])
    vals = np.stack([to_x10000(np.asarray(bands[r], dtype=np.float64), mode) for r in roles], axis=1)
    if vals.ndim == 2:  # a pixel: [T, B] -> [T, B, 1, 1]
        vals = vals[:, :, None, None]
    if clear_only and "fmask" in ctx:
        fm = np.asarray(ctx["fmask"]).reshape(vals.shape[0], *vals.shape[2:])
        vals = np.where((fm <= 1)[:, None], vals, np.nan)
    cube = xr.DataArray(vals, dims=("time", "band", "y", "x"), coords={"time": times(ctx), "band": list(roles)})
    return cube, mode


def shown_is_index(ctx):
    """True when Janus shows a normalized difference (values in -1..1): an index such as the
    NDFI can then be drawn on the same axis."""
    shown = ctx.get("shown") or {}
    nd = shown.get("nd")
    if nd and len(nd) == 2:
        return True
    v = np.asarray(ctx.get("values") or [], dtype=np.float64)
    v = v[np.isfinite(v)]
    return bool(v.size) and float(v.min()) >= -1.0 and float(v.max()) <= 1.0


def series_overlay(label, ctx, values, kind="line"):
    """An overlay of one value per date of the series (NaN dates left out)."""
    v = np.asarray(values, dtype=np.float64).ravel()
    x = np.asarray(ctx["years"], dtype=np.float64)
    ok = np.isfinite(v)
    return {"type": kind, "label": label, "x": [float(a) for a in x[ok]], "y": [float(b) for b in v[ok]]}
