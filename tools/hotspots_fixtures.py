"""The references of janus-zeit's emerging hot spots tool (tests/hotspots.rs): a series made here (noise, a patch
always high, one rising, one falling, a cold spot at the last years, a corner without data) and Zeit's
emerging_hotspots of it with the tool's parameters: the patterns (as the tool's classes, from 1: 1 no pattern, 2-9
the hot spots, 10-17 the cold ones), the Gi* series, its trend and the dates of hot and cold spots.

    python tools/hotspots_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import rasterio
import xarray as xr
from rasterio.transform import from_origin

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data"
CASES = [
    {},
    {"radius": 3, "shape": "circle", "lag": 0, "trend": "original"},
    {"radius": 2, "lag": 2, "reference": "cube", "alpha": 0.01, "trend": "yue_wang"},
]
DEFAULTS = dict(radius=1, shape="square", lag=1, reference="date", alpha=0.05, trend="hamed_rao")
NODATA = -9999.0


def series():
    rng = np.random.default_rng(5)
    t, h, w = 11, 36, 44
    v = rng.normal(0.5, 0.1, (t, h, w))
    ramp = np.linspace(0, 1, t)[:, None, None]
    v[:, 4:12, 4:12] += 0.25                       # always high
    v[:, 20:30, 6:16] += 0.35 * ramp                # rising
    v[:, 6:14, 26:36] += 0.35 * (1 - ramp)          # falling
    v[-3:, 22:30, 28:38] -= 0.3                     # low at the last years
    v[:, :4, -5:] = NODATA
    v[4, 15, 20] = NODATA
    return v.astype(np.float32)


def main():
    stack = series()
    t, h, w = stack.shape
    out_dir = HERE / "hotspots"
    out_dir.mkdir(exist_ok=True)
    path = out_dir / "values.tif"
    with rasterio.open(path, "w", driver="GTiff", width=w, height=h, count=t, dtype="float32", nodata=NODATA,
                       crs="EPSG:32722", transform=from_origin(500000, 9000000, 30, 30)) as dst:
        dst.write(stack)
    days = [int((np.datetime64(f"{2010 + k}-07-01") - np.datetime64("1970-01-01")).astype(int)) for k in range(t)]
    years = [2010 + k + 181 / 365 for k in range(t)]
    values = np.where(stack == NODATA, np.nan, stack.astype(np.float64))
    da = xr.DataArray(values, dims=("time", "y", "x"),
                      coords={"time": np.array([f"{2010 + k}-07-01" for k in range(t)], dtype="datetime64[ns]")})
    cases = []
    for params in CASES:
        p = dict(DEFAULTS, **params)
        r = zeit.emerging_hotspots(da, p["radius"], shape=p["shape"], lag=p["lag"], reference=p["reference"],
                                   alpha=p["alpha"], trend=p["trend"])
        cat = r.category.values.astype(int)
        pattern = np.where(cat == -128, np.nan, np.where(cat >= 0, cat + 1, 9 - cat)).astype(float)
        none = cat == -128
        opt = lambda a: [None if np.isnan(x) else float(x) for x in np.asarray(a, dtype=float).ravel()]
        cases.append({"params": params, "pattern": opt(pattern), "gi": opt(r.gi.values),
                      "trend_z": opt(r.trend_z.values),
                      "n_hot": opt(np.where(none, np.nan, r.n_hot.values)),
                      "n_cold": opt(np.where(none, np.nan, r.n_cold.values))})
        print(params, {int(k): int(v) for k, v in zip(*np.unique(cat, return_counts=True))})
    (out_dir / "hotspots.json").write_text(json.dumps(
        {"input": "hotspots/values.tif", "nodata": NODATA, "years": years, "days": days, "cases": cases}))


if __name__ == "__main__":
    main()
