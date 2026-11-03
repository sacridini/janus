"""Drawing a very large vector layer in Janus (a state's CAR: a million polygons and more): times and completeness.

    python tools/bench_vector_draw.py make DIR                 a synthetic layer and a series under it, in DIR
    python tools/bench_vector_draw.py run DIR JANUS_EXE NAME   Janus's --measure-vectors on them, kept as NAME
    python tools/bench_vector_draw.py report DIR NAME...       bench/vector-draw.json from the runs NAME...

`make` writes `car.gpkg`: 1 200 000 polygons in SIRGAS 2000 (EPSG:4674, as the CAR's files) over 5 x 5 degrees, a
jittered grid of irregular polygons of 8 to 48 vertices (about 33 million vertices; a CAR of a large state has 0.5-1.5
million properties), sizes from a fifth to the whole of their cell, 400 large polygons of 2000 vertices across them
(conservation units, overlapping), each with a `name` and a `mun` column; and `series/`, three dates of 2000 x 2000
Float32 on the same extent (the map's grid). `run` runs Janus (pinned to 3 cores by the caller, `--threads 3`) with
`--measure-vectors`, which prints the reading, placing, the GPU's build, the frame times over many frames (p50, p95)
whole, 8x closer and zoomed in, the time to the first whole drawing and the memory; JANUS_MEASURE_PNG saves the
whole layer's frame. `report` compares each run's frame with the layer rasterized at the frame's resolution
(rasterio.features.rasterize, the polygons' outlines, every pixel touched): the share of the rasterized pixels that
the frame drew (within a pixel), i.e. the features drawn.
"""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

for k in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "GDAL_NUM_THREADS", "RAYON_NUM_THREADS"):
    os.environ.setdefault(k, "3")

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
EXTENT = (-50.0, -20.0, -45.0, -15.0)  # lon0, lat0, lon1, lat1
N_SIDE = 1096  # 1096^2 = 1 201 216 cells
SERIES = 2000


def make(d: Path):
    import geopandas as gpd
    import rasterio
    import shapely
    from rasterio.transform import from_bounds

    d.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(42)
    lon0, lat0, lon1, lat1 = EXTENT
    cw, ch = (lon1 - lon0) / N_SIDE, (lat1 - lat0) / N_SIDE
    t0 = time.time()
    n = N_SIDE * N_SIDE
    ix, iy = np.meshgrid(np.arange(N_SIDE), np.arange(N_SIDE))
    cx = lon0 + (ix.ravel() + 0.5 + rng.uniform(-0.1, 0.1, n)) * cw
    cy = lat0 + (iy.ravel() + 0.5 + rng.uniform(-0.1, 0.1, n)) * ch
    rad = rng.uniform(0.1, 0.4, n)
    k = rng.integers(8, 49, n)
    geoms = np.empty(n, dtype=object)
    for m in range(8, 49):
        sel = np.nonzero(k == m)[0]
        a = np.linspace(0, 2 * np.pi, m, endpoint=False)[None, :] + rng.uniform(0, 0.3, (len(sel), m))
        r = rad[sel, None] * rng.uniform(0.7, 1.0, (len(sel), m))
        x = cx[sel, None] + r * cw * np.cos(a)
        y = cy[sel, None] + r * ch * np.sin(a)
        ring = np.stack([x, y], axis=-1)
        ring = np.concatenate([ring, ring[:, :1]], axis=1)
        geoms[sel] = shapely.polygons(ring)
    # large ones across them
    big = []
    for _ in range(400):
        bx, by = rng.uniform(lon0 + 0.2, lon1 - 0.2), rng.uniform(lat0 + 0.2, lat1 - 0.2)
        a = np.linspace(0, 2 * np.pi, 2000, endpoint=False)
        r = rng.uniform(0.03, 0.15) * (1 + 0.25 * np.sin(5 * a + rng.uniform(0, 6)))
        ring = np.stack([bx + r * np.cos(a), by + r * np.sin(a)], axis=-1)
        big.append(shapely.Polygon(np.concatenate([ring, ring[:1]])))
    geoms = np.concatenate([geoms, np.array(big, dtype=object)])
    total = len(geoms)
    names = np.char.add("BA-", np.char.zfill(np.arange(total).astype(str), 7))
    mun = np.char.add("Municipality ", (np.arange(total) % 417).astype(str))
    df = gpd.GeoDataFrame({"name": names, "mun": mun}, geometry=geoms, crs="EPSG:4674")
    out = d / "car.gpkg"
    if out.exists():
        out.unlink()
    df.to_file(out, engine="pyogrio", layer="car")
    vertices = int(shapely.get_num_coordinates(geoms).sum())
    print(f"car.gpkg: {total} polygons, {vertices} vertices, {time.time() - t0:.0f} s")
    s = d / "series"
    s.mkdir(exist_ok=True)
    tr = from_bounds(lon0, lat0, lon1, lat1, SERIES, SERIES)
    for i, date in enumerate(["2020-01-01", "2021-01-01", "2022-01-01"]):
        v = (rng.random((SERIES, SERIES), dtype=np.float32) * 0.2 + 0.3 + 0.05 * i).astype("float32")
        with rasterio.open(s / f"ndvi_{date}.tif", "w", driver="GTiff", width=SERIES, height=SERIES, count=1,
                           dtype="float32", crs="EPSG:4674", transform=tr, tiled=True, compress="deflate") as o:
            o.write(v, 1)
    (d / "meta.json").write_text(json.dumps({"polygons": total, "vertices": vertices}))


def run(d: Path, exe: str, name: str):
    out = d / f"run-{name}"
    out.mkdir(exist_ok=True)
    env = dict(os.environ, JANUS_MEASURE_PNG=str(out / "whole.png"), JANUS_MEASURE_JSON=str(out / "measure.json"))
    t = time.time()
    p = subprocess.run([exe, "--threads", "3", "--measure-vectors", str(d / "series"), str(d / "car.gpkg")],
                       env=env, capture_output=True, text=True)
    (out / "stdout.txt").write_text(p.stdout + "\n--- stderr ---\n" + p.stderr)
    print(p.stdout)
    print(f"exit {p.returncode}, {time.time() - t:.0f} s")


def completeness(d: Path, name: str, png: str, key: str):
    """The share of the layer's outlines (rasterized at the frame's resolution) the frame drew, within a pixel."""
    import geopandas as gpd
    import shapely
    from PIL import Image
    from rasterio.features import rasterize
    from rasterio.transform import from_bounds

    out = d / f"run-{name}"
    m = json.loads((out / "measure.json").read_text())
    img = np.asarray(Image.open(out / png).convert("RGB")).astype(np.int16)
    h, w = img.shape[:2]
    drawn = img.sum(axis=2) > 24  # the frame's background is black
    # the series' extent in the frame's pixels: the frame's own geotransform
    x0, y0, x1, y1 = m[key]
    lon0, lat0, lon1, lat1 = EXTENT
    sx, sy = (lon1 - lon0) / (x1 - x0), (lat1 - lat0) / (y1 - y0)
    west, north = lon0 - x0 * sx, lat1 + y0 * sy
    tr = from_bounds(west, north - h * sy, west + w * sx, north, w, h)
    cache = d / f"outlines_{key}_{w}x{h}_{x0:.1f}_{y0:.1f}.npy"
    if cache.exists():
        want = np.load(cache)
    else:
        df = gpd.read_file(d / "car.gpkg", engine="pyogrio", columns=[], bbox=(west, north - h * sy, west + w * sx, north))
        lines = shapely.boundary(df.geometry.values)
        want = rasterize(((g, 1) for g in lines), out_shape=(h, w), transform=tr, all_touched=True, dtype="uint8") > 0
        np.save(cache, want)
    # within a pixel: the frame's drawn pixels grown by one
    grown = drawn.copy()
    grown[1:, :] |= drawn[:-1, :]
    grown[:-1, :] |= drawn[1:, :]
    grown[:, 1:] |= drawn[:, :-1]
    grown[:, :-1] |= drawn[:, 1:]
    hit = int((want & grown).sum())
    return {"outline_pixels": int(want.sum()), "drawn_of_them": hit, "share": hit / max(1, int(want.sum()))}


def report(d: Path, names):
    meta = json.loads((d / "meta.json").read_text())
    res = {"layer": {"polygons": meta["polygons"], "vertices": meta["vertices"], "crs": "EPSG:4674",
                     "series": f"{SERIES} x {SERIES} x 3 Float32 on the same extent"},
           "machine": "RTX 3060 (Direct3D 12), Janus pinned to 3 cores, --threads 3, release build",
           "runs": {}}
    for name in names:
        m = json.loads((d / f"run-{name}" / "measure.json").read_text())
        m["completeness"] = {
            "whole layer": completeness(d, name, "whole.png", "series_in_frame"),
            "8x closer": completeness(d, name, "whole_8x.png", "series_in_frame_8x"),
        }
        res["runs"][name] = m
        print(name, json.dumps(m["completeness"]))
    path = ROOT / "bench" / "vector-draw.json"
    path.write_text(json.dumps(res, indent=2) + "\n")
    print(f"written {path}")


if __name__ == "__main__":
    cmd, d = sys.argv[1], Path(sys.argv[2])
    if cmd == "make":
        make(d)
    elif cmd == "run":
        run(d, sys.argv[3], sys.argv[4])
    elif cmd == "report":
        report(d, sys.argv[3:])
    else:
        sys.exit(__doc__)
