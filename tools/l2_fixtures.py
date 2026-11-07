"""The references of janus-zeit's Tasseled Cap and Spectral adjustment tools (tests/l2.rs):
zeit.tasseled_cap and zeit.spectral_adjust on a small multiband series made here (tests/data/l2: 12 x 8 pixels, 24
dates over two years, six bands as reflectance x 10000 in a file per role, an Fmask band, dates of Landsat 7, 8 and
9 and one of Sentinel-2, a pixel without a valid observation and one with a single one), with the tools' parameters
(l2.json). The observations the Fmask rejects are left out of the cube, as Janus leaves them out; the sensor of
each date is given to Zeit as the tool gets it.

    python tools/l2_fixtures.py

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

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data" / "l2"
W, H, T = 12, 8, 24
ROLES = ["blue", "green", "red", "nir", "swir1", "swir2"]
# reflectance x 10000 of green vegetation, bare soil and water
VEG = np.array([400, 700, 500, 3500, 1800, 900], dtype=np.float64)
SOIL = np.array([1200, 1500, 1800, 2300, 3200, 2800], dtype=np.float64)
WATER = np.array([300, 400, 300, 150, 50, 30], dtype=np.float64)


def decimal_year(day):
    d = np.datetime64("1970-01-01") + np.timedelta64(int(day), "D")
    y = int(str(d)[:4])
    start = np.datetime64(f"{y}-01-01")
    n = 366 if (y % 4 == 0 and y % 100 != 0) or y % 400 == 0 else 365
    return y + int((d - start) / np.timedelta64(1, "D")) / n


def clean(a):
    return [None if not np.isfinite(x) else float(f"{x:.9g}") for x in np.asarray(a, dtype=np.float64).reshape(-1)]


def write(name, array, nodata=np.nan, dtype="float32"):
    with rasterio.open(HERE / name, "w", driver="GTiff", width=W, height=H, count=array.shape[0], dtype=dtype,
                       nodata=nodata, crs="EPSG:32722", transform=from_origin(500000.0, 8900000.0, 30.0, 30.0),
                       compress="deflate") as dst:
        dst.write(array.astype(dtype))


# the tool's parameters and what zeit takes for them
TASSELED = [
    {"components": ["brightness", "greenness", "wetness"], "coefficients": "crist1985"},
    {"components": ["wetness", "disturbance"], "coefficients": "sensor"},
    {"components": ["disturbance", "brightness"], "coefficients": "huang2002"},
    {"components": ["greenness"], "coefficients": "baig2014"},
    {"components": ["brightness", "greenness", "wetness", "disturbance"], "coefficients": "sensor"},
]
ADJUST = [
    {"sensor": "auto", "force_search": False},
    {"sensor": "LND08", "force_search": False},
    {"sensor": "LND07", "force_search": True},
    {"sensor": "auto", "force_search": True},
]


def main():
    HERE.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(20261107)
    # 24 dates, every 30 days from 2020-01-10 (some a little off)
    days = 18271 + 30 * np.arange(T) + rng.integers(0, 6, T)
    t = (days - days[0]) / 365.25
    # each date's sensor: Landsat 7 up to the 8th, Landsat 8 after, Landsat 9 from the 18th, one Sentinel-2 date
    sensors = ["LND07"] * 8 + ["LND08"] * 10 + ["LND09"] * 6
    sensors[5] = "LND05"
    sensors[11] = "SEN2"
    # what a Landsat-only series is for the sensor of each date: the Sentinel-2 date is a Landsat 8 one, and two
    # dates are MODIS
    landsat_sensors = list(sensors)
    landsat_sensors[11] = "LND08"
    landsat_sensors[2] = "MOD01"
    landsat_sensors[20] = "MOD02"

    # land cover mixtures per pixel: vegetation (with a season), soil and water
    veg = np.clip(0.55 + 0.3 * np.sin(2 * np.pi * (t[:, None, None] - 0.2)) + 0.1 * rng.standard_normal((1, H, W)),
                  0, 1)
    water = np.zeros((1, H, W))
    water[0, :2, :] = 0.7
    soil = np.clip(1 - veg - water, 0, 1)
    mix = veg[..., None] * VEG + soil[..., None] * SOIL + water[..., None] * WATER   # (T, H, W, 6)
    mix = mix * (1 + 0.03 * rng.standard_normal((T, H, W, 1))) + 40 * rng.standard_normal((T, H, W, 6))
    mix = np.clip(mix, 5, 9000)
    # sensors differ a little
    mix[np.array(sensors) == "LND07"] *= 1.02
    cube = np.moveaxis(mix, -1, 1)                                  # (T, band, H, W)
    # a missing band here and there, a pixel without a valid observation, one with a single one
    holes = rng.random((T, H, W)) < 0.06
    cube[np.broadcast_to(holes[:, None], cube.shape) & (rng.random(cube.shape) < 0.5)] = np.nan
    cube[:, :, 0, 0] = np.nan
    cube[:, :, 7, 11] = np.nan
    cube[10, :, 7, 11] = mix[10, 7, 11, :]
    cube = cube.astype(np.float32)
    # Fmask: clear, water, a few shadows, snow, clouds, no observation
    qa = np.zeros((T, H, W), dtype=np.float32)
    r = rng.random((T, H, W))
    qa[r < 0.12] = 4
    qa[(r >= 0.12) & (r < 0.17)] = 2
    qa[(r >= 0.17) & (r < 0.19)] = 3
    qa[(r >= 0.19) & (r < 0.22)] = 255
    qa[(r >= 0.22) & (r < 0.27)] = 1
    qa[10, 7, 11] = 0                                               # the single observation is a clear one

    for k, role in enumerate(ROLES):
        write(f"{role}.tif", cube[:, k])
    write("qa.tif", qa)
    write("input.tif", cube[:, 3])
    spec = {"tool": "tasseled_cap", "params": {}, "input": "input.tif", "nodata": None,
            "bands": {role: f"{role}.tif" for role in ROLES}, "qa_input": "qa.tif", "qa_rule": "fmask",
            "years": [decimal_year(d) for d in days], "days": [int(d) for d in days],
            "window": [0, 0, W, H], "threads": 1}
    (HERE / "spec.json").write_text(json.dumps(spec))

    index = pd.DatetimeIndex(np.datetime64("1970-01-01") + days.astype("timedelta64[D]"))
    clear = np.where((qa <= 1)[:, None], cube.astype(np.float64), np.nan)
    data = xr.DataArray(clear, dims=("time", "band", "y", "x"), coords={"time": index, "band": ROLES})

    def zeit_sensors(names):
        return ["sentinel-2" if s == "SEN2" else s for s in names]

    out = {"sensors": sensors, "landsat_sensors": landsat_sensors, "tasseled_cap": [], "spectral_adjust": []}
    for c in TASSELED:
        kwargs = {}
        if c["coefficients"] == "sensor":
            kwargs["sensor"] = zeit_sensors(sensors)
        r = zeit.tasseled_cap(data, c["components"], coefficients=c["coefficients"], bands=dict(zip(ROLES, ROLES)), nodata=None, n_jobs=1, **kwargs)
        out["tasseled_cap"].append({"params": c, "values": [[clean(r.sel(band=n).isel(time=k)) for k in range(T)]
                                                             for n in c["components"]]})
        print("tasseled_cap", c, int(np.isnan(r.values).sum()), "values missing")
    for c in ADJUST:
        auto = c["sensor"] == "auto"
        r = zeit.spectral_adjust(data, sensor=zeit_sensors(landsat_sensors) if auto else c["sensor"], scale=10000.0,
                                 bands=dict(zip(ROLES, ROLES)), nodata=None, n_jobs=1, force_search=c["force_search"])
        names = list(r.band.values)
        out["spectral_adjust"].append({"params": c, "bands": names,
                                       "values": [[clean(r.sel(band=n).isel(time=k)) for k in range(T)]
                                                  for n in names]})
        print("spectral_adjust", c, int(np.isnan(r.values).sum()), "values missing")
    (HERE / "l2.json").write_text(json.dumps(out))


if __name__ == "__main__":
    main()
