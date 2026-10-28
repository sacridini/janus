"""Janus's reproject / resample / clip (IDEIAS phase 43) against the gdalwarp command and rioxarray: results and time.

    python tools/bench_warp_vs_gdalwarp.py [DIR]     (Zeit's environment: rasterio, rioxarray; gdalwarp on the PATH)

Two maps in UTM zone 23S at 30 m: `reflectance`, 8192 x 8192 Float32 (a smooth field and noise), and `classes`,
8192 x 8192 of 6 classes (Byte). Cases: lonlat (reflectance to EPSG:4326, bilinear, COG), mode90 (classes to 90 m
by mode, COG), clip3857 (a 60 km window of reflectance to EPSG:3857, cubic, GeoTIFF), vrt (lonlat as a warped VRT).
Janus (crates/janus-zeit/examples/vs_gdalwarp.rs, 8 threads) prints gdalwarp's arguments; the command runs with the
same ones on the same file and the two results are compared pixel for pixel (and their grids); rioxarray's
reproject (num_threads=8; to_raster as a COG with the same compression) for lonlat and mode90, compared where its
grid is gdalwarp's. Writes bench/warp-vs-gdalwarp.json.
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

os.environ.setdefault("OMP_NUM_THREADS", "8")
os.environ.setdefault("GDAL_NUM_THREADS", "8")
import numpy as np
import rasterio
import rioxarray
from rasterio.enums import Resampling
from rasterio.transform import from_origin

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_gdalwarp.exe"
GDALWARP = shutil.which("gdalwarp") or str(Path(sys.executable).parent / "Library" / "bin" / "gdalwarp.exe")


def make(d):
    rng = np.random.default_rng(43)
    n = 8192
    y, x = np.mgrid[0:n, 0:n].astype(np.float32) / n
    refl = (0.2 + 0.1 * np.sin(6 * x) * np.cos(4 * y) + 0.02 * rng.standard_normal((n, n))).astype(np.float32)
    small = rng.standard_normal((n // 64, n // 64))
    cls = np.digitize(np.kron(small, np.ones((64, 64))) + 0.3 * rng.standard_normal((n, n)), [-1, -0.4, 0, 0.4, 1])
    for name, a, nd in [("reflectance", refl, None), ("classes", (cls + 1).astype(np.uint8), 0)]:
        prof = dict(driver="GTiff", width=n, height=n, count=1, dtype=a.dtype.name, crs="EPSG:32723",
                    transform=from_origin(270000, 7500000, 30, 30), nodata=nd, tiled=True, compress="deflate")
        with rasterio.open(d / f"{name}.tif", "w", **prof) as f:
            f.write(a, 1)


def read(path):
    with rasterio.open(path) as d:
        return d.read(1), d.transform, d.crs


def same(a, b):
    (x, tx, cx), (y, ty, cy) = read(a), read(b)
    if x.shape != y.shape:
        return {"same_grid": False, "shapes": [x.shape, y.shape]}
    eq = np.array_equal(x, y, equal_nan=x.dtype.kind == "f")
    return {"same_grid": tx == ty and cx == cy, "shape": list(x.shape), "bit_equal": bool(eq)}


def main():
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "bench-warp"
    d.mkdir(parents=True, exist_ok=True)
    if not (d / "classes.tif").exists():
        make(d)
    res = {}
    for case, src in [("lonlat", "reflectance"), ("mode90", "classes"), ("clip3857", "reflectance"),
                      ("vrt", "reflectance")]:
        jd, gd = d / f"{case}_janus", d / f"{case}_gdal"
        jd.mkdir(exist_ok=True)
        gd.mkdir(exist_ok=True)
        runs = []
        for _ in range(2):
            r = subprocess.run([str(EXE), case, str(d / f"{src}.tif"), str(jd), "8"], capture_output=True, text=True,
                               check=True)
            runs.append(json.loads(r.stdout))
        j = min(runs, key=lambda x: x["seconds"])
        out = gd / Path(j["output"]).name
        gs = []
        for _ in range(2):
            t = time.perf_counter()
            subprocess.run([GDALWARP, "-q", *j["args"], str(d / f"{src}.tif"), str(out)], check=True)
            gs.append(time.perf_counter() - t)
        row = {"janus_s": j["seconds"], "gdalwarp_s": min(gs), **same(j["output"], out)}
        if case in ("lonlat", "mode90"):
            ts = []
            for _ in range(2):
                t = time.perf_counter()
                da = rioxarray.open_rasterio(d / f"{src}.tif", masked=False).squeeze()
                if case == "lonlat":
                    rp = da.rio.reproject("EPSG:4326", resampling=Resampling.bilinear, num_threads=8)
                else:
                    rp = da.rio.reproject(da.rio.crs, resolution=90, resampling=Resampling.mode, num_threads=8)
                rp.rio.to_raster(d / f"{case}_rioxarray.tif", driver="COG", compress="DEFLATE", predictor="YES",
                                 num_threads=8)
                ts.append(time.perf_counter() - t)
            row["rioxarray_s"] = min(ts)
            r = same(j["output"], d / f"{case}_rioxarray.tif")
            row["rioxarray_same_grid"] = r.get("same_grid", False)
            if r.get("same_grid"):
                x, y = read(j["output"])[0], read(d / f"{case}_rioxarray.tif")[0]
                ok = np.isfinite(x) & np.isfinite(y) if x.dtype.kind == "f" else np.ones(x.shape, bool)
                row["rioxarray_max_abs_diff"] = float(np.abs(x[ok].astype(float) - y[ok].astype(float)).max())
                row["rioxarray_equal_share"] = float((x[ok] == y[ok]).mean())
            else:
                row["rioxarray_shapes"] = [list(map(int, s)) for s in r.get("shapes", [])]
        res[case] = row
        print(case, json.dumps(row))
    out = ROOT / "bench" / "warp-vs-gdalwarp.json"
    out.write_text(json.dumps(res, indent=2))
    print("written", out)


if __name__ == "__main__":
    main()
