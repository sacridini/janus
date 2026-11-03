"""Janus's profiles of every line of a layer and its ROI saved as a vector against shapely + rasterio: results and
time.

    python tools/bench_profiles_vs_rasterio.py      (rasterio, geopandas, shapely, pandas; Janus's example
                                                     crates/janus-geo/examples/vs_profiles.rs built in release)

A grid of 4096 x 4096 pixels of 30 m in UTM 23S, 12 dates of smooth values (Float32 tiled GeoTIFFs, DEFLATE, -9999
without a value in a block) and 200 lines over it (polylines of 2 to 6 vertices, a fifth of them multi-lines, some
running off the grid), in a GeoPackage.

Profiles of every line: Janus (vs_profiles profiles, BENCH_THREADS threads: each line sampled about a pixel apart as
the transect samples it, the dates read by windows around the samples) against the same samples taken by shapely
(the line in the grid's pixels; n = its length in pixels, rounded up, plus one; line.interpolate at n evenly spaced
distances; the pixel under each) and read by rasterio, either as a script usually does it (`src.sample` at the
pixels' centres, date by date) or by reading each date whole and indexing the array (numpy). Compared sample by
sample: the pixel, the distance along the line (metres), the place and the value. The times are the sampling and
reading (the best of three; `sample` once); the long CSV's writing apart (Janus, pandas' to_csv).

The ROI saved as a vector: an outline of 600 vertices with a hole, in the grid's pixels, written by Janus
(vs_profiles roi: GeoPackage and GeoJSON, as Save ROI as vector writes them) with its pixels (centres inside, even-odd)
as a mask; each file read back by geopandas and burned by rasterio.features.rasterize (centres) on the grid: the same
pixels? The writing timed against geopandas' to_file of the same polygon.

Writes bench/profiles-vs-rasterio.json.
"""
import json
import math
import os
import subprocess
import time
from pathlib import Path

THREADS = int(os.environ.get("BENCH_THREADS", "3"))
for v in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "GDAL_NUM_THREADS", "RAYON_NUM_THREADS"):
    os.environ.setdefault(v, str(THREADS))
import geopandas as gpd
import numpy as np
import pandas as pd
import rasterio
import shapely
from rasterio.features import rasterize
from rasterio.transform import from_origin
from scipy.ndimage import zoom
from shapely.geometry import LineString, MultiLineString

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_profiles.exe"
D = ROOT / "target" / "bench-profiles"
N, PX, X0, Y0, DATES, LINES = 4096, 30.0, 500000.0, 7600000.0, 12, 200
TR = from_origin(X0, Y0, PX, PX)
# Janus's example with Janus's own GDAL (staged beside janus.exe in target/release), never another one on the PATH
ENV = dict(os.environ, PATH=str(ROOT / "target" / "release") + os.pathsep + os.environ.get("PATH", ""),
           PROJ_DATA=str(ROOT / "target" / "release" / "share" / "proj"),
           GDAL_DRIVER_PATH=str(ROOT / "target" / "release" / "gdalplugins"))


def smooth(rng, n):
    return zoom(rng.standard_normal((n // 64, n // 64)), 64, order=3)[:n, :n]


def make():
    D.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(28)
    prof = dict(driver="GTiff", width=N, height=N, count=1, crs="EPSG:32723", transform=TR, tiled=True,
                compress="deflate", dtype="float32", nodata=-9999)
    base = smooth(rng, N)
    maps = []
    for t in range(DATES):
        p = D / f"v_{2001 + t}.tif"
        maps.append(p)
        if p.exists():
            continue
        v = (base + 0.3 * smooth(rng, N) + 0.05 * t).astype("float32")
        v[1500:1800, 2200:2700] = -9999
        with rasterio.open(p, "w", **prof) as d:
            d.write(v, 1)
    gp = D / "lines.gpkg"
    if not gp.exists():
        geoms = []
        for k in range(LINES):
            def part():
                m = int(rng.integers(2, 7))
                start = rng.uniform(-200, N + 200, 2)
                steps = rng.normal(0, 350, (m - 1, 2))
                pts = np.vstack([start, start + np.cumsum(steps, axis=0)])
                return [(X0 + c * PX, Y0 - r * PX) for c, r in pts]
            if k % 5 == 0:
                geoms.append(MultiLineString([part(), part()]))
            else:
                geoms.append(LineString(part()))
        gpd.GeoDataFrame({"name": [f"line {k}" for k in range(LINES)]}, geometry=geoms, crs="EPSG:32723").to_file(
            gp, driver="GPKG")
    return maps, gp


def samples_py(gp):
    """The samples of every line as shapely takes them: per (fid, part) the pixels, places and distances."""
    lines = gpd.read_file(gp, fid_as_index=True)
    inv = ~TR
    rows = []
    for fid, g in lines.geometry.items():
        parts = list(g.geoms) if g.geom_type == "MultiLineString" else [g]
        for part, ln in enumerate(parts):
            xy = np.asarray(ln.coords)
            cr = np.array([inv * (x, y) for x, y in xy])
            px = LineString(cr)
            n = math.ceil(px.length) + 1
            d = px.length * np.arange(n) / (n - 1)
            pts = shapely.get_coordinates(shapely.line_interpolate_point(px, d))
            col, row = np.floor(pts[:, 0]).astype(np.int64), np.floor(pts[:, 1]).astype(np.int64)
            ok = (col >= 0) & (row >= 0) & (col < N) & (row < N)
            if not ok.any():
                continue
            dm = ln.length * np.arange(n) / (n - 1)
            i = np.nonzero(ok)[0]
            rows.append(pd.DataFrame({"fid": fid, "part": part, "sample": i, "distance_m": dm[i],
                                      "x": X0 + pts[i, 0] * PX, "y": Y0 - pts[i, 1] * PX,
                                      "col": col[i], "row": row[i]}))
    return pd.concat(rows, ignore_index=True)


def values_numpy(maps, s):
    out = []
    for p in maps:
        with rasterio.open(p) as src:
            a = src.read(1, masked=True).filled(np.nan)
        out.append(a[s["row"].to_numpy(), s["col"].to_numpy()])
    return out


def values_sample(maps, s):
    out = []
    xy = list(zip(X0 + (s["col"].to_numpy() + 0.5) * PX, Y0 - (s["row"].to_numpy() + 0.5) * PX))
    for p in maps:
        with rasterio.open(p) as src:
            v = np.fromiter((x[0] for x in src.sample(xy, masked=False)), np.float32, len(xy))
        v[v == -9999] = np.nan
        out.append(v)
    return out


def best_of(k, f):
    best, r = math.inf, None
    for _ in range(k):
        t = time.perf_counter()
        r = f()
        best = min(best, time.perf_counter() - t)
    return best, r


def profiles(maps, gp):
    out = D / "janus_profiles.csv"
    janus = []
    for _ in range(3):
        r = subprocess.run([str(EXE), "profiles", str(gp), str(THREADS), str(out), *map(str, maps)],
                           capture_output=True, text=True, check=True, env=ENV)
        janus.append(json.loads(r.stdout.strip().splitlines()[-1]))
    j_best = min(janus, key=lambda x: x["seconds"])
    t_samples, s = best_of(3, lambda: samples_py(gp))
    t_numpy, vals = best_of(3, lambda: values_numpy(maps, s))
    t_sample = time.perf_counter()
    vals_sample = values_sample(maps, s)
    t_sample = time.perf_counter() - t_sample
    # the long table as pandas writes it
    long = pd.concat([s.assign(date=str(2001 + k), value=v) for k, v in enumerate(vals)], ignore_index=True)
    t_csv, _ = best_of(3, lambda: long.to_csv(D / "py_profiles.csv", index=False))
    # sample by sample
    jd = pd.read_csv(out, dtype={"date": str})
    key = ["fid", "part", "sample", "date"]
    m = jd.merge(long, on=key, how="outer", suffixes=("_j", "_p"), indicator=True)
    both = m[m["_merge"] == "both"]
    same_px = int(((both["col_j"] == both["col_p"]) & (both["row_j"] == both["row_p"])).sum())
    vj, vp = both["value_j"].to_numpy(), both["value_p"].to_numpy()
    # the same float32, bit for bit (Janus writes the shortest text that reads back as it)
    same_v = int(((vj.astype(np.float32) == vp.astype(np.float32)) | (np.isnan(vj) & np.isnan(vp))).sum())
    agree_sample = all(np.array_equal(a, b, equal_nan=True) for a, b in zip(vals, vals_sample))
    return {
        "lines": LINES, "dates": DATES, "threads": THREADS,
        "janus": {"seconds": j_best["seconds"], "write_seconds": j_best["write_seconds"],
                  "runs": [x["seconds"] for x in janus], "samples": j_best["samples"],
                  "pixels_read": j_best["pixels"], "windows_a_date": j_best["windows"]},
        "python": {"samples_seconds": t_samples, "read_numpy_seconds": t_numpy,
                   "read_rasterio_sample_seconds": t_sample, "write_seconds": t_csv,
                   "samples": int(len(s)), "rasterio_sample_equals_numpy": bool(agree_sample)},
        "speedup": {"vs_numpy": (t_samples + t_numpy) / j_best["seconds"],
                    "vs_rasterio_sample": (t_samples + t_sample) / j_best["seconds"],
                    "csv": t_csv / j_best["write_seconds"]},
        "agreement": {
            "rows_janus": int(len(jd)), "rows_python": int(len(long)),
            "only_janus": int((m["_merge"] == "left_only").sum()),
            "only_python": int((m["_merge"] == "right_only").sum()),
            "same_pixel": same_px, "same_value": same_v, "compared": int(len(both)),
            "max_distance_diff_m": float(np.abs(both["distance_m_j"] - both["distance_m_p"]).max()),
            "max_xy_diff_m": float(max(np.abs(both["x_j"] - both["x_p"]).max(),
                                       np.abs(both["y_j"] - both["y_p"]).max())),
        },
    }


def roi(maps):
    out = D / "roi"
    r = subprocess.run([str(EXE), "roi", str(maps[0]), str(out)], capture_output=True, text=True, check=True, env=ENV)
    j = json.loads(r.stdout.strip().splitlines()[-1])
    with rasterio.open(out / "roi_mask.tif") as m:
        mask = m.read(1).astype(bool)
    res = {"janus": j, "pixels": int(mask.sum())}
    for name in ("roi.gpkg", "roi.geojson"):
        g = gpd.read_file(out / name)
        burnt = rasterize([(g.geometry.iloc[0], 1)], out_shape=(N, N), transform=TR, all_touched=False,
                          dtype="uint8").astype(bool)
        geom = g.geometry.iloc[0]
        poly = geom.geoms[0] if geom.geom_type == "MultiPolygon" else geom
        t, _ = best_of(5, lambda: g.to_file(D / f"py_{name}", driver="GPKG" if name.endswith("gpkg") else "GeoJSON"))
        res[name] = {"crs": g.crs.to_string(), "type": geom.geom_type, "vertices": len(poly.exterior.coords) - 1,
                     "holes": len(poly.interiors), "fields": {k: str(v) for k, v in g.drop(columns="geometry").iloc[0].items()},
                     "pixels_rasterio": int(burnt.sum()), "differ": int((burnt != mask).sum()),
                     "geopandas_write_seconds": t}
    return res


def main():
    maps, gp = make()
    res = {"grid": f"{N} x {N} x {DATES}, {PX:g} m, UTM 23S, Float32 tiled DEFLATE", "profiles": profiles(maps, gp),
           "roi": roi(maps)}
    (ROOT / "bench").mkdir(exist_ok=True)
    (ROOT / "bench" / "profiles-vs-rasterio.json").write_text(json.dumps(res, indent=2))
    print(json.dumps(res, indent=2))


if __name__ == "__main__":
    main()
