"""Janus's rasterize (IDEIAS phase 44) against gdal_rasterize and rasterio.features.rasterize: results and time.

    python tools/bench_rasterize_vs_gdal.py [DIR]     (GDAL's Python and rasterio, shapely; run
                                                      tools/bench_polygonize_vs_gdal.py first: its polygons)

The polygons of phase 41's benchmark (gdal_polygonize's GeoPackages): `classes`, 11 732 polygons of 6 classes on a
2048 x 2048 grid at 30 m, and `loss`, 45 711 polygons of years on 4096 x 4096. Each burned on its map's grid by its
`value` (the pixels' centres, and every pixel touched for the classes) and as a mask (1). Janus
(crates/janus-zeit/examples/vs_gdal_rasterize.rs) against the gdal_rasterize command (-a value / -burn 1, -te, -ts,
the same type and init) compared pixel for pixel, and rasterio.features.rasterize (the features read with OGR, the
reading timed apart). A burn of the values by the centres gives back the map polygonized. Writes
bench/rasterize-vs-gdal.json.
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

os.environ.setdefault("GDAL_NUM_THREADS", "8")
import numpy as np
import rasterio
from osgeo import ogr
from rasterio.features import rasterize
from shapely import wkb

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_gdal_rasterize.exe"
GDAL_RASTERIZE = shutil.which("gdal_rasterize") or str(Path(sys.executable).parent / "Library" / "bin" / "gdal_rasterize.exe")


def read_masked(path):
    """The band as float64, its no data NaN."""
    with rasterio.open(path) as d:
        a, nd = d.read(1).astype("float64"), d.nodata
    if nd is not None and not np.isnan(nd):
        a[a == nd] = np.nan
    return a


def equal(a, b):
    return bool(np.array_equal(a, b, equal_nan=True))


def features(gpkg, field):
    ds = ogr.Open(str(gpkg))
    lyr = ds.GetLayer(0)
    out = []
    for f in lyr:
        g = wkb.loads(bytes(f.GetGeometryRef().ExportToWkb()))
        out.append((g, f.GetField(field) if field != "-" else 1))
    return out


def main():
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "bench-polygonize"
    d = ROOT / "target" / "bench-rasterize"
    d.mkdir(parents=True, exist_ok=True)
    res = {}
    for name, gpkg, grid, field, touched in [
        ("classes_value", "classes_4_gdal.gpkg", "classes.tif", "value", False),
        ("classes_value_touched", "classes_4_gdal.gpkg", "classes.tif", "value", True),
        ("classes_mask", "classes_4_gdal.gpkg", "classes.tif", "-", False),
        ("loss_value", "loss_8_gdal.gpkg", "loss.tif", "value", False),
    ]:
        vec, grid = src / gpkg, src / grid
        with rasterio.open(grid) as g:
            w, h, tr = g.width, g.height, g.transform
            b = g.bounds
        jo, go = d / f"{name}_janus.tif", d / f"{name}_gdal.tif"
        runs = []
        for _ in range(3):
            r = subprocess.run([str(EXE), str(vec), field, str(grid), str(jo), "1" if touched else "0"],
                               capture_output=True, text=True, check=True)
            runs.append(json.loads(r.stdout))
        j = min(runs, key=lambda x: x["seconds"])
        # the command with Janus's type, initial value and no data
        with rasterio.open(jo) as jd:
            ot, nd = jd.dtypes[0], jd.nodata
        gtype = {"uint8": "Byte", "int32": "Int32", "float64": "Float64"}[ot]
        init = "0" if nd is None else ("nan" if np.isnan(nd) else str(int(nd)))
        burn = (["-burn", "1"] if field == "-" else ["-a", field]) + ["-ot", gtype, "-init", init]
        burn += [] if nd is None else ["-a_nodata", init]
        cmd = [GDAL_RASTERIZE, "-q", *burn, *(["-at"] if touched else []), "-te", str(b.left), str(b.bottom),
               str(b.right), str(b.top), "-ts", str(w), str(h), "-co", "TILED=YES", "-co", "COMPRESS=DEFLATE",
               str(vec), str(go)]
        gs = []
        for _ in range(3):
            if go.exists():
                go.unlink()
            t = time.perf_counter()
            subprocess.run(cmd, check=True)
            gs.append(time.perf_counter() - t)
        t = time.perf_counter()
        feats = features(vec, field)
        read_s = time.perf_counter() - t
        rs = []
        for _ in range(3):
            t = time.perf_counter()
            arr = rasterize(feats, out_shape=(h, w), transform=tr, all_touched=touched,
                            fill=0 if field == "-" else np.nan, dtype="uint8" if field == "-" else "float64")
            rs.append(time.perf_counter() - t)
        a, g = read_masked(jo), read_masked(go)
        row = {
            "type": gtype,
            "janus_s": j["seconds"],
            "janus_read_features_s": j["read_s"],
            "gdal_rasterize_s": min(gs),
            "rasterio_rasterize_s": min(rs),
            "rasterio_read_features_s": read_s,
            "polygons": j["burned"],
            "equal_gdal_rasterize": equal(a, g),
            "equal_rasterio": equal(a, arr.astype("float64")),
        }
        if field != "-" and not touched:
            row["equal_map_polygonized"] = equal(a, read_masked(grid))
        res[name] = row
        print(name, json.dumps(row))
    out = ROOT / "bench" / "rasterize-vs-gdal.json"
    out.write_text(json.dumps(res, indent=2))
    print("written", out)


if __name__ == "__main__":
    main()
