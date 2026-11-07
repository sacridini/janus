"""The references of janus-zeit's ImproPhe tool (tests/improphe.rs): zeit.improphe on a small series made here
(tests/data/improphe: a fine series of 60 x 60 pixels of 10 m, 20 dates over two years in three bands of reflectance
x 10000 with clouds left as NaN, land cover in blocks; two coarse series of 30 m on grids that are not the fine one's:
a field with a layer for each of three years (2020-2022; no fine scene in 2022) in two fields, one with a NoData
value and one with NaN, and four scenes of a sensor in two bands), with the tool's parameters (improphe.json): the
two variants, FORCE's arithmetic and the principal components apart, a window crossing the new year, and windows
(the fine series cut to the window, as the tool's run is). Each case's result is a float32 GeoTIFF (expected_*.tif),
a band for each layer and field as the tool writes them.

    python tools/improphe_fixtures.py

Needs Zeit (the local checkout is imported when it is the python's zeit), numpy, pandas, xarray, rasterio and
rioxarray.
"""

import json
from pathlib import Path

import numpy as np
import pandas as pd
import rasterio
import xarray as xr
from rasterio.transform import from_origin

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data" / "improphe"
W = H = 60
RES = 10.0
X0, Y0 = 500000.0, 8900000.0
CRS = "EPSG:32722"
NODATA = -9999.0

# reflectance x 10000 of four covers (blue, green... three bands: red, nir, swir1) and the coarse metrics of each
COVER = np.array([[400, 3500, 1800], [1500, 2300, 3200], [300, 150, 50], [700, 2800, 1200]], dtype=np.float64)
PHASE = np.array([0.0, 1.5, 0.0, 3.0])
AMPLITUDE = np.array([0.35, 0.1, 0.02, 0.25])


def decimal_year(day):
    d = np.datetime64("1970-01-01") + np.timedelta64(int(day), "D")
    y = int(str(d)[:4])
    start = np.datetime64(f"{y}-01-01")
    n = 366 if (y % 4 == 0 and y % 100 != 0) or y % 400 == 0 else 365
    return y + int((d - start) / np.timedelta64(1, "D")) / n


def cover_of(x, y):
    """The cover at map coordinates: blocks of about 150 m with a diagonal boundary."""
    u, v = (x - X0) / 150.0, (Y0 - y) / 150.0
    return (np.floor(u + 0.35 * v).astype(int) + 2 * np.floor(v).astype(int)) % 4


def write(name, array, nodata, transform, count=None):
    array = np.asarray(array, dtype=np.float32)
    with rasterio.open(HERE / name, "w", driver="GTiff", width=array.shape[-1], height=array.shape[-2],
                       count=array.shape[0], dtype="float32", nodata=nodata, crs=CRS, transform=transform,
                       compress="deflate") as dst:
        dst.write(array)


def clean(a):
    return np.where(np.isfinite(a), a, np.nan).astype(np.float32)


def main():
    HERE.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(20261109)
    yy, xx = np.mgrid[0:H, 0:W]
    fx, fy = X0 + (xx + 0.5) * RES, Y0 - (yy + 0.5) * RES
    cover = cover_of(fx, fy)

    # the fine dates: 10 a year, not evenly; the day of the year drives the phenology
    dates = pd.DatetimeIndex(sorted(
        [f"2020-{m:02d}-{d:02d}" for m, d in [(1, 8), (2, 19), (3, 30), (5, 2), (6, 11), (7, 20), (8, 29), (10, 3),
                                              (11, 14), (12, 26)]]
        + [f"2021-{m:02d}-{d:02d}" for m, d in [(1, 11), (2, 22), (4, 5), (5, 16), (6, 27), (8, 7), (9, 18),
                                                (10, 30), (11, 28), (12, 31)]]))
    T = len(dates)
    days = ((dates - pd.Timestamp("1970-01-01")) // pd.Timedelta(days=1)).to_numpy()
    doy = dates.dayofyear.to_numpy()
    season = 0.5 + 0.5 * np.sin(2 * np.pi * doy[:, None, None] / 365.0 - PHASE[cover][None])
    fine = np.empty((T, 3, H, W))
    for b in range(3):
        base = COVER[cover, b][None]
        # the vegetated covers change with the season, the others hardly
        fine[:, b] = base * (1 + AMPLITUDE[cover][None] * (season - 0.5) * (1 if b == 1 else 0.5))
    fine *= 1 + 0.03 * rng.standard_normal(fine.shape)
    # clouds (a date's whole bands) over blobs, and scattered missing values
    cloud = rng.random((T, H, W)) < 0.10
    blob = np.zeros((T, H, W), bool)
    for t in rng.choice(T, 6, replace=False):
        cy, cx, r = rng.integers(5, 55), rng.integers(5, 55), rng.integers(6, 14)
        blob[t] = (yy - cy) ** 2 + (xx - cx) ** 2 < r * r
    fine[np.broadcast_to((cloud | blob)[:, None], fine.shape)] = np.nan
    fine = fine.astype(np.float32)
    tf = from_origin(X0, Y0, RES, RES)
    for b in range(3):
        write(f"fine_b{b + 1}.tif", fine[:, b], np.nan, tf)
    write("input.tif", fine[:, 1], np.nan, tf)

    # the coarse grids and what they hold: a cover's metric of the year (and a second field), the mean over the
    # pixel's 3 x 3 sub-points
    def coarse_cells(x0, y0, nx, ny, res):
        sub = (np.arange(3) + 0.5) / 3.0
        cx = x0 + (np.arange(nx)[None, :, None, None] + sub[None, None, None, :]) * res
        cy = y0 - (np.arange(ny)[:, None, None, None] + sub[None, None, :, None]) * res
        cx, cy = np.broadcast_arrays(cx, cy)
        return cover_of(cx, cy)

    # CF: a field with a layer a year, shifted by 60 m so that the fine series' first six columns are outside it
    cf_x0, cf_y0, cf_nx, cf_ny = X0 + 60.0, Y0 + 30.0, 20, 22
    c = coarse_cells(cf_x0, cf_y0, cf_nx, cf_ny, 30.0)
    level = np.array([0.8, 0.5, 0.05, 0.6])
    years = [2020, 2021, 2022]
    f1 = np.stack([(level[c] * (1 + 0.1 * k)).mean(axis=(2, 3)) for k in range(3)])
    f2 = np.stack([(np.array([120.0, 60.0, 5.0, 90.0])[c] * (1 - 0.05 * k)).mean(axis=(2, 3)) for k in range(3)])
    f1 = f1 + 0.01 * rng.standard_normal(f1.shape)
    f2 = f2 + 1.0 * rng.standard_normal(f2.shape)
    f1[1, 3:5, 4:8] = NODATA          # a NoData value
    f2[0, 10:12, 6:9] = np.nan        # NaN
    f1[2, 15, 15] = NODATA
    tcf = from_origin(cf_x0, cf_y0, 30.0, 30.0)
    write("coarse_cf_f1.tif", f1, NODATA, tcf)
    write("coarse_cf_f2.tif", f2, np.nan, tcf)

    # L2: scenes of a sensor on a grid 20 m off the fine one's, 30 m pixels
    l2_x0, l2_y0, l2_nx, l2_ny = X0 - 20.0, Y0 + 10.0, 21, 21
    c2 = coarse_cells(l2_x0, l2_y0, l2_nx, l2_ny, 30.0)
    scenes = pd.DatetimeIndex(["2020-03-12", "2020-09-05", "2021-01-23", "2021-07-18"])
    sdoy = scenes.dayofyear.to_numpy()
    sc = []
    for b in range(2):
        layers = []
        for k in range(len(scenes)):
            ph = 0.5 + 0.5 * np.sin(2 * np.pi * sdoy[k] / 365.0 - PHASE)
            v = (COVER[:, b] * (1 + AMPLITUDE * (ph - 0.5)))[c2].mean(axis=(2, 3))
            layers.append(v * (1 + 0.02 * rng.standard_normal(v.shape)))
        sc.append(np.stack(layers))
    sc = np.stack(sc)                  # (band, scene, y, x)
    sc[0, 1, 8:10, 8:12] = NODATA
    sc[1, 2, 3, 3] = np.nan
    tl2 = from_origin(l2_x0, l2_y0, 30.0, 30.0)
    for b in range(2):
        write(f"coarse_l2_b{b + 1}.tif", sc[b], NODATA if b == 0 else np.nan, tl2)

    def spec_time(index):
        d = ((index - pd.Timestamp("1970-01-01")) // pd.Timedelta(days=1)).to_numpy()
        return [decimal_year(x) for x in d], [int(x) for x in d]

    cf_dates = pd.DatetimeIndex([f"{y}-07-01" for y in years])
    fy, fd = spec_time(dates)
    cy_, cd_ = spec_time(cf_dates)
    ly, ld = spec_time(scenes)
    spec = {"tool": "improphe", "params": {}, "input": "input.tif", "nodata": None, "years": fy, "days": fd,
            "window": [0, 0, W, H], "threads": 2,
            "other_cf": {"name": "cf", "inputs": ["coarse_cf_f1.tif", "coarse_cf_f2.tif"],
                         "band_names": ["metric", "amplitude"], "years": cy_, "days": cd_,
                         "labels": [str(d.date()) for d in cf_dates]},
            "other_l2": {"name": "l2", "inputs": ["coarse_l2_b1.tif", "coarse_l2_b2.tif"],
                         "band_names": ["red", "nir"], "years": ly, "days": ld,
                         "labels": [str(d.date()) for d in scenes]}}
    (HERE / "spec.json").write_text(json.dumps(spec))

    # what zeit is given: the fine cube (time, band, y, x), the coarse ones with their missing values as NaN
    def fine_cube(x0, y0, x1, y1):
        return xr.DataArray(fine[:, :, y0:y1, x0:x1].astype(np.float32), dims=("time", "band", "y", "x"),
                            coords={"time": dates.to_numpy(), "band": ["b1", "b2", "b3"],
                                    "y": Y0 - (np.arange(y0, y1) + 0.5) * RES,
                                    "x": X0 + (np.arange(x0, x1) + 0.5) * RES}).rio.write_crs(CRS)

    def coarse_cube(values, x0, y0, times, bands):
        v = np.where(values == NODATA, np.nan, values).astype(np.float32)
        ny, nx = v.shape[-2:]
        return xr.DataArray(v, dims=("time", "band", "y", "x"),
                            coords={"time": times.to_numpy(), "band": bands,
                                    "y": y0 - (np.arange(ny) + 0.5) * 30.0, "x": x0 + (np.arange(nx) + 0.5) * 30.0}
                            ).rio.write_crs(CRS)

    cf = coarse_cube(np.stack([f1, f2], axis=1), cf_x0, cf_y0, cf_dates, ["metric", "amplitude"])
    l2 = coarse_cube(np.transpose(sc, (1, 0, 2, 3)), l2_x0, l2_y0, scenes, ["red", "nir"])

    base = dict(kernel_size=100.0, texture_size=30.0, pc_variance=0.975)
    cases = [
        {"name": "cf_default", "coarse": "cf", "by": "year", "force": False},
        {"name": "cf_force", "coarse": "cf", "by": "year", "force": True},
        {"name": "cf_force_pca_principal", "coarse": "cf", "by": "year", "force": True, "pca": "principal"},
        {"name": "cf_principal_force_pca", "coarse": "cf", "by": "year", "force": False, "pca": "force"},
        {"name": "cf_crossing", "coarse": "cf", "by": "year", "force": False, "seasons": "330,60,330",
         "kernel_size": 60.0, "texture_size": 20.0, "pc_variance": 0.9},
        {"name": "cf_roi", "coarse": "cf", "by": "year", "force": False, "window": [8, 5, 47, 52]},
        {"name": "l2_default", "coarse": "l2", "by": "scenes", "force": False},
        {"name": "l2_auto", "coarse": "l2", "by": "auto", "force": True},
        {"name": "l2_pca_principal_force", "coarse": "l2", "by": "scenes", "force": True, "pca": "principal"},
        {"name": "l2_roi", "coarse": "l2", "by": "scenes", "force": False, "window": [14, 9, 55, 44]},
    ]
    out = []
    for c in cases:
        k = {**base, **{key: c[key] for key in ("kernel_size", "texture_size", "pc_variance") if key in c}}
        seasons = [int(s) for s in c.get("seasons", "1,91,181,271,365").split(",")]
        x0, y0, x1, y1 = c.get("window", [0, 0, W, H])
        by = None if c["by"] == "scenes" else c["by"]
        coarse = cf if c["coarse"] == "cf" else l2
        result = zeit.improphe(coarse, fine_cube(x0, y0, x1, y1), by=by, seasons=tuple(seasons), force=c["force"],
                               pca=c.get("pca"), nodata=None, n_jobs=2, **k)
        values = np.asarray(result.values, dtype=np.float32)
        bands = values.reshape(-1, values.shape[-2], values.shape[-1])
        write(f"expected_{c['name']}.tif", bands, np.nan, from_origin(X0 + x0 * RES, Y0 - y0 * RES, RES, RES))
        params = {"by": c["by"], "seasons": c.get("seasons", "1,91,181,271,365"), "force": c["force"],
                  "pca": c.get("pca") or "default", **k}
        out.append({"name": c["name"], "coarse": c["coarse"], "params": params, "window": [x0, y0, x1, y1],
                    "expected": f"expected_{c['name']}.tif", "attrs": result.attrs["improphe"]})
        n = int(np.isnan(bands).sum())
        print(c["name"], bands.shape, n, "missing |", result.attrs["improphe"])
    (HERE / "improphe.json").write_text(json.dumps({"cases": out}, indent=1))


if __name__ == "__main__":
    main()
