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
