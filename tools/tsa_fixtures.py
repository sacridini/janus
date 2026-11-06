"""The references of janus-zeit's Interpolate, Temporal metrics, Fold and Standardize tools (tests/tsa.rs):
zeit.interpolate on a small series made here (tests/data/tsa: 12 x 8 pixels, 120 irregular dates over about 2.5
years, gaps of every size, a pixel without a valid observation and one with a single one), for each method, with
the tool's parameters (tsa.json); and zeit.temporal_metrics (whole series and by period) and zeit.standardize on
the same series (tsa_stats.json).

    python tools/tsa_fixtures.py

Needs Zeit (the local checkout is imported when it is the python's zeit), numpy, pandas, xarray and rasterio.
"""

import json
from pathlib import Path

import numpy as np
import pandas as pd
import rasterio
import xarray as xr
from rasterio.transform import from_origin

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data" / "tsa"
W, H, T = 12, 8, 120
# the tool's parameters (the others at their defaults: rbf, step 16, sigma 8,16,32, cutoff 0.95, window 16,
# modes 3, trend on) and what zeit.interpolate takes for them
CASES = [
    {},
    {"method": "rbf", "step": 10, "sigma": "4,8,16", "cutoff": 0.9},
    {"method": "rbf", "step": 12, "sigma": "16"},
    {"method": "linear", "step": 8},
    {"method": "moving", "step": 16, "window": 20},
    {"method": "moving", "step": 9, "window": 7},
    {"method": "harmonic"},
    {"method": "harmonic", "step": 30, "modes": 2, "trend": False},
    {"method": "harmonic", "step": 12, "modes": 1},
]
DEFAULTS = dict(method="rbf", step=16, sigma="8,16,32", cutoff=0.95, window=16, modes=3, trend=True)


def decimal_year(day):
    d = np.datetime64("1970-01-01") + np.timedelta64(int(day), "D")
    y = int(str(d)[:4])
    start = np.datetime64(f"{y}-01-01")
    n = 366 if (y % 4 == 0 and y % 100 != 0) or y % 400 == 0 else 365
    return y + int((d - start) / np.timedelta64(1, "D")) / n


ALL_METRICS = ["min", "p05", "p10", "p25", "median", "p75", "p90", "p95", "max", "range", "iqr", "mean", "std",
               "skew", "kurtosis", "count"]


def clean(a):
    return [None if not np.isfinite(x) else float(f"{x:.9g}") for x in np.asarray(a, dtype=np.float64).reshape(-1)]


def stats(cube):
    """tsa_stats.json: for each case, the tool's parameters and zeit's values: temporal_metrics [metric][pixel],
    fold [metric][period][pixel] with the period labels, standardize [date][pixel]."""
    out = {"temporal_metrics": [], "fold": [], "standardize": []}
    default = ["min", "p25", "median", "p75", "max", "mean", "std"]
    for metrics in (default, ALL_METRICS, ["std", "p05", "count"]):
        r = zeit.temporal_metrics(cube, metrics, nodata=None, n_jobs=1)
        out["temporal_metrics"].append({"params": {"metrics": metrics}, "values": [clean(r.sel(metric=m)) for m in
                                        zeit.indices.parse_metrics(metrics)]})
    for by, metrics in (("year", ["mean"]), ("year", ALL_METRICS), ("quarter", ["mean", "count", "p05"]),
                        ("month", ["mean", "median", "count", "std"]), ("week", ["mean", "count"]),
                        ("doy", ["max", "count"]), ("month", ["iqr", "range", "skew", "kurtosis"])):
        r = zeit.temporal_metrics(cube, metrics, by=by, nodata=None, n_jobs=1)
        out["fold"].append({"params": {"metrics": metrics, "by": by}, "periods": [str(x) for x in r[by].values],
                            "values": [[clean(r.sel(metric=m).isel({by: k})) for k in range(r.sizes[by])]
                                       for m in zeit.indices.parse_metrics(metrics)]})
    for method in ("normalize", "center"):
        r = zeit.standardize(cube, method, nodata=None, n_jobs=1)
        out["standardize"].append({"params": {"method": method},
                                   "values": [clean(r.isel(time=k)) for k in range(r.sizes["time"])]})
        print("standardize", method, int(np.isnan(r.values).sum()), "values missing")
    (HERE / "tsa_stats.json").write_text(json.dumps(out))


def main():
    rng = np.random.default_rng(20261106)
    # irregular dates: 1 to 15 days apart, from 2020-03-30
    steps = rng.integers(1, 16, T)
    days = 18351 + np.cumsum(steps) - steps[0]
    t = (days - days[0]) / 365.25
    base = 0.45 + 0.3 * np.sin(2 * np.pi * (t - 0.2))[:, None, None]
    stack = base + 0.1 * rng.standard_normal((T, H, W)) + 0.02 * rng.standard_normal((1, H, W)) * t[:, None, None]
    gaps = rng.random((T, H, W)) < 0.35
    stack[gaps] = np.nan
    stack[30:48, 2, 3] = np.nan                       # a long gap (more than the widest kernel's reach)
    stack[:, 0, 0] = np.nan                           # no valid observation
    stack[:, 7, 11] = np.nan
    stack[57, 7, 11] = 0.5                            # a single one
    stack[:20, 5, 5] = np.nan                         # no data at the start
    stack = stack.astype(np.float32)

    HERE.mkdir(parents=True, exist_ok=True)
    with rasterio.open(HERE / "input.tif", "w", driver="GTiff", width=W, height=H, count=T, dtype="float32",
                       nodata=np.nan, crs="EPSG:32722", transform=from_origin(500000.0, 8900000.0, 30.0, 30.0),
                       compress="deflate") as dst:
        dst.write(stack)
    spec = {"tool": "interpolate", "params": {}, "input": "input.tif", "nodata": None,
            "years": [decimal_year(d) for d in days], "days": [int(d) for d in days],
            "window": [0, 0, W, H], "threads": 1}
    (HERE / "spec.json").write_text(json.dumps(spec))

    index = pd.DatetimeIndex(np.datetime64("1970-01-01") + days.astype("timedelta64[D]"))
    cube = xr.DataArray(stack.astype(np.float64), dims=("time", "y", "x"), coords={"time": index})
    out = {"cases": []}
    for params in CASES:
        p = dict(DEFAULTS, **params)
        kwargs = dict(step=p["step"], nodata=None, n_jobs=1)
        if p["method"] == "rbf":
            kwargs.update(sigma=[float(x) for x in p["sigma"].split(",")], cutoff=p["cutoff"])
        elif p["method"] == "moving":
            kwargs.update(window=p["window"])
        elif p["method"] == "harmonic":
            kwargs.update(modes=p["modes"], trend=p["trend"])
        res = zeit.interpolate(cube, p["method"], **kwargs)
        grid = [str(x.date()) for x in pd.DatetimeIndex(res.time.values)]
        vals = np.asarray(res.values, dtype=np.float64).reshape(len(grid), -1)
        out["cases"].append({
            "params": params, "dates": grid,
            "values": [[None if not np.isfinite(x) else float(f'{x:.9g}') for x in row] for row in vals]})
        print(params, len(grid), "dates;", int(np.isnan(vals).sum()), "values missing")
    (HERE / "tsa.json").write_text(json.dumps(out))
    stats(cube)


if __name__ == "__main__":
    main()
