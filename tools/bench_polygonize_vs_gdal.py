"""Janus's polygonize (IDEIAS phase 41) against gdal_polygonize and rasterio.features.shapes: polygons and time.

    python tools/bench_polygonize_vs_gdal.py [DIR]     (an environment with GDAL's Python, rasterio, pyproj, scipy)

Two maps in UTM (30 m): `classes`, 2048 x 2048 of 6 classes (smoothed noise cut at its sextiles: large patches and
many small ones), and `loss`, 4096 x 4096 of a year of loss (2001-2020) in sparse patches, the rest without a value,
with a magnitude map of the same grid. Janus (crates/janus-zeit/examples/vs_gdal_polygonize.rs: labels, GDALPolygonize,
geodesic area and perimeter, pixels, a GeoPackage) against the gdal_polygonize command (a GeoPackage of the polygons and
their value alone) and rasterio.features.shapes (the polygons in memory; then their geodesic area and perimeter with
pyproj's Geod, what Janus's columns hold). Same polygons: each value's count and area, and the polygons one by one
(value, area, centroid). Writes bench/polygonize-vs-gdal.json.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

os.environ.setdefault("GDAL_NUM_THREADS", "8")
os.environ.setdefault("OMP_NUM_THREADS", "8")
import numpy as np
import rasterio
from osgeo import ogr
from pyproj import Geod
from rasterio.features import shapes
from rasterio.transform import from_origin
from scipy.ndimage import gaussian_filter
from shapely.geometry import shape

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_gdal_polygonize.exe"
BIN = Path(sys.executable).parent / "Scripts"


def write_tif(path, a, nodata):
    h, w = a.shape
    prof = dict(driver="GTiff", width=w, height=h, count=1, dtype=a.dtype.name, crs="EPSG:32723",
                transform=from_origin(600000, 7500000, 30, 30), nodata=nodata, tiled=True, compress="deflate")
    with rasterio.open(path, "w", **prof) as d:
        d.write(a, 1)


def make(d):
    rng = np.random.default_rng(41)
    f = gaussian_filter(rng.standard_normal((2048, 2048)), 6)
    cuts = np.quantile(f, [1 / 6, 2 / 6, 3 / 6, 4 / 6, 5 / 6])
    classes = (np.digitize(f, cuts) + 1).astype(np.uint8)
    write_tif(d / "classes.tif", classes, 0)
    g = gaussian_filter(rng.standard_normal((4096, 4096)), 3)
    years = np.floor(2001 + 20 * (gaussian_filter(rng.standard_normal((4096, 4096)), 40) * 8 + 0.5).clip(0, 0.999))
    loss = np.where(g > 0.08, years, np.nan).astype(np.float32)
    write_tif(d / "loss.tif", loss, float("nan"))
    mag = np.where(np.isnan(loss), np.nan, rng.uniform(-0.6, -0.1, loss.shape)).astype(np.float32)
    write_tif(d / "magnitude.tif", mag, float("nan"))


def janus(src, out, conn, extra=()):
    r = subprocess.run([str(EXE), str(src), str(out), str(conn), "0", *map(str, extra)],
                       capture_output=True, text=True, check=True)
    return json.loads(r.stdout)


def gdal_cli(src, out, conn):
    if out.exists():
        out.unlink()
    cmd = [str(BIN / "gdal_polygonize.exe"), "-q", "-f", "GPKG", str(src), str(out), "polygons", "value"]
    if conn == 8:
        cmd.insert(1, "-8")
    t = time.perf_counter()
    subprocess.run(cmd, check=True)
    return time.perf_counter() - t


def rio(src, conn, geodesic):
    geod = Geod(ellps="WGS84")
    from pyproj import Transformer
    t = time.perf_counter()
    with rasterio.open(src) as d:
        a = d.read(1)
        mask = d.read_masks(1) > 0
        tr = d.transform
        to_ll = Transformer.from_crs(d.crs, "EPSG:4326", always_xy=True)
    n = 0
    for geom, v in shapes(a, mask=mask, connectivity=conn, transform=tr):
        n += 1
        if geodesic:
            ring = geom["coordinates"][0]
            xs, ys = to_ll.transform(*zip(*ring))
            geod.polygon_area_perimeter(xs, ys)
    return time.perf_counter() - t, n


def read(path, field):
    ds = ogr.Open(str(path))
    lyr = ds.GetLayer(0)
    out = []
    for f in lyr:
        g = f.GetGeometryRef()
        c = g.Centroid()
        out.append((float(f.GetField(field)), round(g.GetArea(), 1), round(c.GetX(), 2), round(c.GetY(), 2)))
    return out


def compare(a, b):
    per = lambda p: {v: (sum(1 for q in p if q[0] == v), sum(q[1] for q in p if q[0] == v)) for v in {q[0] for q in p}}
    pa, pb = per(a), per(b)
    same_values = all(pa[v][0] == pb.get(v, (0, 0))[0] and abs(pa[v][1] - pb[v][1]) < 1 for v in pa) and pa.keys() == pb.keys()
    sa, sb = sorted(a), sorted(b)
    same = sum(x == y for x, y in zip(sa, sb))
    return {"polygons": [len(a), len(b)], "same_per_value": same_values, "same_polygons": same}


def best(fn, repeat=3):
    out = [fn() for _ in range(repeat)]
    return min(out, key=lambda x: x if isinstance(x, float) else x[0])


def main():
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "bench-polygonize"
    d.mkdir(parents=True, exist_ok=True)
    if not (d / "loss.tif").exists():
        make(d)
    res = {}
    for name, conn in [("classes", 4), ("classes", 8), ("loss", 8)]:
        src = d / f"{name}.tif"
        jo, go = d / f"{name}_{conn}_janus.gpkg", d / f"{name}_{conn}_gdal.gpkg"
        js = [janus(src, jo, conn) for _ in range(3)]
        j = min(js, key=lambda r: r["seconds"])
        g = min(gdal_cli(src, go, conn) for _ in range(3))
        r_shapes, n_shapes = best(lambda: rio(src, conn, False))
        r_geod, _ = best(lambda: rio(src, conn, True), 1)
        row = {
            "janus_s": j["seconds"],
            "gdal_polygonize_s": g,
            "rasterio_shapes_s": r_shapes,
            "rasterio_shapes_geod_s": r_geod,
            "rasterio_polygons": n_shapes,
            **compare(read(jo, "value"), read(go, "value")),
        }
        if name == "loss":
            m = min((janus(src, jo, conn, [d / "magnitude.tif"]) for _ in range(2)), key=lambda r: r["seconds"])
            row["janus_with_mean_s"] = m["seconds"]
        res[f"{name}_{conn}"] = row
        print(name, conn, json.dumps(row))
    out = ROOT / "bench" / "polygonize-vs-gdal.json"
    out.write_text(json.dumps(res, indent=2))
    print("written", out)


if __name__ == "__main__":
    main()
