"""The references of janus-zeit's NBAR (c-factor) tool (tests/nbar.rs): zeit.nbar on a small series made here
(tests/data/nbar: 12 x 16 pixels, 8 dates, the eleven bands with a BRDF role as a file each, and the four angle bands
as degrees (float32) and as hundredths of a degree (int16, as Landsat Collection 2 and HLS store them)), with the
tool's parameters (nbar.json): the scene's mean solar zenith and a fixed one, with and without the c-factors, a
subset of the bands, both angle units. The angles have pixels outside the model's domain (a solar zenith of 95
degrees), angles without a value, a date without any valid angle, and the reflectance has gaps.

    python tools/nbar_fixtures.py

Needs Zeit (the local checkout is imported when it is the python's zeit), numpy, xarray and rasterio.
"""

import json
from pathlib import Path

import numpy as np
import rasterio
import xarray as xr
from rasterio.transform import from_origin

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data" / "nbar"
W, H, T = 12, 16, 8
ROLES = ["coastal", "blue", "green", "red", "rededge1", "rededge2", "rededge3", "nir", "nir08", "swir1", "swir2"]
ANGLES = ["sza", "saa", "vza", "vaa"]
# the pixels the pixel runs are tried on: (x, y)
PIXELS = [(1, 1), (3, 2), (5, 5), (11, 7), (0, 0), (8, 4), (2, 12)]

# the tool's parameters, the bands mapped, the unit of the angle files
CASES = [
    {"params": {"sza": "scene", "angle_scale": "degrees", "cfactor": False}, "roles": ROLES},
    {"params": {"sza": "fixed", "sza_fixed": 32.5, "angle_scale": "hundredths", "cfactor": True},
     "roles": ["blue", "green", "red", "nir", "swir1", "swir2"]},
    {"params": {"sza": "scene", "angle_scale": "hundredths", "cfactor": True},
     "roles": ["red", "nir08", "rededge2"]},
    {"params": {"sza": "fixed", "sza_fixed": 30.0, "angle_scale": "degrees", "cfactor": False}, "roles": ["nir"]},
]


def decimal_year(day):
    d = np.datetime64("1970-01-01") + np.timedelta64(int(day), "D")
    y = int(str(d)[:4])
    start = np.datetime64(f"{y}-01-01")
    n = 366 if (y % 4 == 0 and y % 100 != 0) or y % 400 == 0 else 365
    return y + int((d - start) / np.timedelta64(1, "D")) / n


def clean(a):
    return [None if not np.isfinite(x) else float(f"{x:.10g}") for x in np.asarray(a, dtype=np.float64).reshape(-1)]


def write(name, array, nodata, dtype):
    with rasterio.open(HERE / name, "w", driver="GTiff", width=W, height=H, count=array.shape[0], dtype=dtype,
                       nodata=nodata, crs="EPSG:32722", transform=from_origin(500000.0, 8900000.0, 30.0, 30.0),
                       compress="deflate") as dst:
        dst.write(array.astype(dtype))


def main():
    HERE.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(20261108)
    days = 18271 + 16 * np.arange(T) + rng.integers(0, 4, T)
    yy, xx = np.mgrid[0:H, 0:W]

    # the angles in degrees: a solar zenith of the date and a slow change over the scene, a view zenith that grows
    # from the scene's centre line (up to 7.5 degrees), azimuths of the sun and of the view
    sza = (28 + 4 * np.arange(T))[:, None, None] + 0.08 * yy[None] + 0.03 * xx[None] + 0.2 * rng.standard_normal((T, H, W))
    saa = 140 + 2 * np.arange(T)[:, None, None] + 0.1 * yy[None] + 0.05 * rng.standard_normal((T, H, W))
    vza = np.abs(xx[None] - 5.5) * 0.7 + 0.05 * rng.random((T, H, W)) + 0 * sza
    vaa = np.where(xx[None] < 5.5, 280.0, 100.0) + 2 * rng.standard_normal((T, H, W)) + 0 * sza
    angles = {"sza": sza, "saa": saa, "vza": vza, "vaa": vaa}
    # the model's domain: a pixel with a solar zenith of 95 degrees at every date, a few angles that are missing, a
    # view zenith above 90 degrees, one date without a valid angle (every angle missing)
    sza[:, 0, 0] = 95.0
    vaa[2, 4, 3] = np.nan
    vza[5, 5, 5] = np.nan
    saa[1, 9, 9] = np.nan
    vza[3, 10, 2] = 91.0
    sza[6, :, :] = np.nan
    vza[6, :, :] = np.nan
    saa[6, :, :] = np.nan
    vaa[6, :, :] = np.nan
    # the same angles as Landsat Collection 2 stores them: integers in hundredths of a degree, -32768 for none
    hundredths = {k: np.where(np.isfinite(v), np.round(v * 100.0), -32768).astype(np.int16) for k, v in angles.items()}
    # and what zeit is given of them: the files' values, scaled, missing as NaN
    for k in ANGLES:
        write(f"{k}_deg.tif", np.where(np.isfinite(angles[k]), angles[k], np.nan).astype(np.float32), np.nan, "float32")
        write(f"{k}_cd.tif", hundredths[k], -32768, "int16")
    degrees = {k: np.where(np.isfinite(v), v, np.nan).astype(np.float32).astype(np.float64) for k, v in angles.items()}
    centi = {k: np.where(v == -32768, np.nan, v.astype(np.float64)) for k, v in hundredths.items()}

    # reflectance (0-1 scale x 10000), the bands of a vegetated surface that changes with the season, with gaps
    base = np.array([300, 450, 700, 500, 1500, 2600, 3200, 3500, 3600, 1800, 900], dtype=np.float64)
    season = 1 + 0.2 * np.sin(2 * np.pi * np.arange(T) / 8)[:, None, None, None]
    refl = base[None, :, None, None] * season * (1 + 0.1 * rng.standard_normal((T, 1, H, W)))
    refl = refl * (1 + 0.02 * rng.standard_normal((T, len(ROLES), H, W)))
    refl[rng.random(refl.shape) < 0.04] = np.nan
    refl[:, :, 7, 11] = np.where(np.arange(T)[:, None] % 2 == 0, np.nan, refl[:, :, 7, 11])
    refl = refl.astype(np.float32)
    for k, role in enumerate(ROLES):
        write(f"{role}.tif", refl[:, k], np.nan, "float32")
    write("input.tif", refl[:, 3], np.nan, "float32")
    spec = {"tool": "nbar", "params": {}, "input": "input.tif", "nodata": None,
            "years": [decimal_year(d) for d in days], "days": [int(d) for d in days], "window": [0, 0, W, H],
            "threads": 1}
    (HERE / "spec.json").write_text(json.dumps(spec))

    index = np.datetime64("1970-01-01") + days.astype("timedelta64[D]")
    time = index.astype("datetime64[ns]")
    out = {"pixels": PIXELS, "cases": []}
    for c in CASES:
        unit = "cd" if c["params"]["angle_scale"] == "hundredths" else "deg"
        scale = 0.01 if unit == "cd" else 1.0
        given = centi if unit == "cd" else degrees
        roles = c["roles"]
        sza_arg = "scene" if c["params"]["sza"] == "scene" else c["params"]["sza_fixed"]
        cf = c["params"]["cfactor"]

        def run(sel):
            cube = xr.DataArray(refl[(slice(None), [ROLES.index(r) for r in roles]) + sel].astype(np.float64),
                                dims=("time", "band", "y", "x"), coords={"time": time, "band": roles})
            ang = xr.Dataset({a.upper(): (("time", "y", "x"), given[a][(slice(None),) + sel] * scale)
                              for a in ANGLES}, coords={"time": time})
            return zeit.nbar(cube, ang, sza=sza_arg, bands=dict(zip(roles, roles)), nodata=None, n_jobs=1,
                             return_cfactor=cf)

        def parts(result):
            nbar, cfac = result if cf else (result, None)
            pick = lambda a: [[clean(a.sel(band=r).isel(time=k)) for k in range(T)] for r in roles]
            return {"values": pick(nbar), "cfactor": pick(cfac) if cf else None,
                    "sza": clean(nbar.nbar_sza.values)}

        whole = parts(run((slice(None), slice(None))))
        pixels = {}
        for x, y in PIXELS:
            pixels[f"{x},{y}"] = parts(run((slice(y, y + 1), slice(x, x + 1))))
        out["cases"].append({"params": c["params"], "unit": unit, "roles": roles, **whole, "pixels": pixels})
        n = int(np.isnan(np.array(whole["values"][0], dtype=float)).sum())
        print(c["params"], roles, "scene sza", whole["sza"], "|", n, "values missing in the first band")
    (HERE / "nbar.json").write_text(json.dumps(out))


if __name__ == "__main__":
    main()
