"""Janus's areas by class and date (IDEIAS phase 42) against R's terra::expanse and a numpy count: areas and time.

    python tools/bench_areas_vs_terra.py [DIR]     (an environment with rasterio, scipy; R with terra)

Three cases: `utm`, a series of 8 class maps (6 classes) of 4096 x 4096 at 30 m in UTM zone 23S, 110 to 230 km
west of its central meridian (the nominal 900 m² off by up to 0.05%); `lonlat`, 8 maps of 4096 x 4096 at 0.00025
degrees from 10 to 11 degrees south; `years`, one map of 8192 x 4096 of decimal years of loss (2001-2020.99, most
pixels without a value) counted by year. Janus (crates/janus-zeit/examples/vs_terra_areas.rs, 8 threads, each pixel's
geodesic area) against terra::expanse(rast(files), unit = "m", byValue = TRUE) (its cells' areas: transform = TRUE
for UTM) and numpy's bincount times the nominal pixel area (rasterio, what a script does: the scale and latitude
ignored). Writes bench/areas-vs-terra.json.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

os.environ.setdefault("OMP_NUM_THREADS", "8")
import numpy as np
import rasterio
from rasterio.transform import from_origin
from scipy.ndimage import gaussian_filter

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_terra_areas.exe"
RSCRIPT = Path(r"C:\Program Files\R\R-4.4.2\bin\Rscript.exe")

R_CODE = r"""
suppressMessages(library(terra))
terraOptions(progress = 0)
a <- commandArgs(trailingOnly = TRUE)
out <- a[1]; byyear <- a[2] == "1"; files <- a[3:length(a)]
t0 <- Sys.time()
r <- rast(files)
if (byyear) r <- floor(r)
e <- expanse(r, unit = "m", byValue = TRUE, transform = TRUE)
s <- as.numeric(difftime(Sys.time(), t0, units = "secs"))
write.csv(e, out, row.names = FALSE)
cat(s, "\n")
"""


def write_tif(path, a, crs, transform, nodata):
    h, w = a.shape
    prof = dict(driver="GTiff", width=w, height=h, count=1, dtype=a.dtype.name, crs=crs, transform=transform,
                nodata=nodata, tiled=True, compress="deflate")
    with rasterio.open(path, "w", **prof) as d:
        d.write(a, 1)


def make(d):
    rng = np.random.default_rng(42)
    for case, crs, tr in [
        ("utm", "EPSG:32723", from_origin(270000, 7500000, 30, 30)),
        ("lonlat", "EPSG:4326", from_origin(-45, -10, 0.00025, 0.00025)),
    ]:
        base = gaussian_filter(rng.standard_normal((4096, 4096)), 8)
        for t in range(8):
            f = base + 0.02 * t * gaussian_filter(rng.standard_normal((4096, 4096)), 8) / base.std()
            cuts = np.quantile(f, [1 / 6, 2 / 6, 3 / 6, 4 / 6, 5 / 6])
            write_tif(d / f"{case}_{t}.tif", (np.digitize(f, cuts) + 1).astype(np.uint8), crs, tr, 0)
    g = gaussian_filter(rng.standard_normal((4096, 8192)), 3)
    years = 2001 + 20 * (gaussian_filter(rng.standard_normal((4096, 8192)), 60) * 15 + 0.5).clip(0, 0.9999)
    loss = np.where(g > 0.08, years, np.nan).astype(np.float32)
    write_tif(d / "years_0.tif", loss, "EPSG:32723", from_origin(270000, 7500000, 30, 30), float("nan"))


def numpy_count(files, floor):
    t = time.perf_counter()
    out, counts = [], []
    for f in files:
        with rasterio.open(f) as d:
            a = d.read(1)
            px = abs(d.transform.a * d.transform.e)
            nd = d.nodata
        a = a[~np.isnan(a)] if a.dtype.kind == "f" else a[a != nd]
        v = np.floor(a).astype(np.int64) if floor else a.astype(np.int64)
        vals, n = np.unique(v, return_counts=True)
        out.append(dict(zip(vals.tolist(), (n * px).tolist())))
        counts.append(dict(zip(vals.tolist(), n.tolist())))
    return time.perf_counter() - t, out, counts


def compare(janus, other):
    worst = 0.0
    for s, row in enumerate(janus["m2"]):
        for v, a in zip(janus["values"], row):
            b = other[s].get(v, 0.0)
            if a > 0:
                worst = max(worst, abs(b / a - 1))
    return worst


def main():
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "bench-areas"
    d.mkdir(parents=True, exist_ok=True)
    if not (d / "years_0.tif").exists():
        make(d)
    rfile = d / "expanse.R"
    rfile.write_text(R_CODE)
    res = {}
    for case, n, floor in [("utm", 8, False), ("lonlat", 8, False), ("years", 1, True)]:
        files = [str(d / f"{case}_{t}.tif") for t in range(n)]
        js = []
        for _ in range(3):
            out = d / f"{case}_janus.json"
            subprocess.run([str(EXE), str(out), "1" if floor else "0", "8", *files], check=True, capture_output=True)
            js.append(json.loads(out.read_text()))
        j = min(js, key=lambda x: x["seconds"])
        rcsv = d / f"{case}_terra.csv"
        rs = subprocess.run([str(RSCRIPT), str(rfile), str(rcsv), "1" if floor else "0", *files],
                            check=True, capture_output=True, text=True)
        r_s = float(rs.stdout.split()[-1])
        terra = [dict() for _ in files]
        for line in rcsv.read_text().splitlines()[1:]:
            layer, value, area = line.split(",")
            terra[int(layer) - 1][int(float(value))] = float(area)
        np_s, np_out, np_n = min((numpy_count(files, floor) for _ in range(2)), key=lambda x: x[0])
        row = {
            "janus_s": j["seconds"],
            "terra_expanse_s": r_s,
            "numpy_nominal_s": np_s,
            "note": j["note"],
            "max_rel_diff_terra": compare(j, terra),
            "max_rel_diff_numpy_nominal": compare(j, np_out),
            "pixels_equal_numpy": all(
                dict(zip(j["values"], j["pixels"][s])) == {v: np_n[s].get(v, 0) for v in j["values"]}
                and set(np_n[s]) <= set(j["values"])
                for s in range(len(files))
            ),
        }
        res[case] = row
        print(case, json.dumps(row))
    out = ROOT / "bench" / "areas-vs-terra.json"
    out.write_text(json.dumps(res, indent=2))
    print("written", out)


if __name__ == "__main__":
    main()
