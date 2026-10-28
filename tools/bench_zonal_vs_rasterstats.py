"""Janus's zonal statistics over time (IDEIAS phase 45) against rasterstats and exactextractr: results and time.

    python tools/bench_zonal_vs_rasterstats.py     (the geo env: GDAL, rasterio, rasterstats, shapely; R with
                                                   exactextractr and terra for its part)

A grid of 4096 x 4096 at 30 m in UTM 23S, 8 dates of smooth values (Float32, -9999 without a value in a few areas)
and 8 dates of 5 classes (Byte, 255 without a value); 2000 Voronoi cells over it (non-overlapping "properties") and
100 circles over them (pixels in several zones). Janus (crates/janus-zeit/examples/vs_zonal.rs, 8 threads) against
rasterstats.zonal_stats (the pixels' centres, numpy's statistics; categorical=True for the classes) on every date,
compared zone by zone, and R's exactextractr::exact_extract (8 dates at once; each pixel weighted by the part of it
covered, so its statistics differ from the centres'). Writes bench/zonal-vs-rasterstats.json.
"""
import json
import os
import subprocess
import time
from pathlib import Path

os.environ.setdefault("GDAL_NUM_THREADS", "8")
os.environ.setdefault("OMP_NUM_THREADS", "8")
import numpy as np
import rasterio
from rasterio.transform import from_origin
from rasterstats import zonal_stats
from scipy.ndimage import zoom
from shapely import voronoi_polygons, MultiPoint, box, Point
import geopandas as gpd

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_zonal.exe"
RSCRIPT = r"C:\Program Files\R\R-4.4.2\bin\Rscript.exe"
D = ROOT / "target" / "bench-zonal"
N, PX, X0, Y0, DATES = 4096, 30.0, 500000.0, 7600000.0, 8
STATS = ["count", "mean", "std", "min", "percentile_10", "percentile_25", "median", "percentile_75", "percentile_90",
         "max"]


def smooth(rng, n):
    return zoom(rng.standard_normal((n // 64, n // 64)), 64, order=3)[:n, :n]


def make():
    D.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(45)
    tr = from_origin(X0, Y0, PX, PX)
    prof = dict(driver="GTiff", width=N, height=N, count=1, crs="EPSG:32723", transform=tr, tiled=True,
                compress="deflate")
    base = smooth(rng, N)
    hole = np.zeros((N, N), bool)
    hole[1000:1300, 2000:2600] = True
    vals, cls = [], []
    for t in range(DATES):
        v = (base + 0.3 * smooth(rng, N) + 0.05 * t).astype("float32")
        v[hole] = -9999
        p = D / f"v{t}.tif"
        if not p.exists():
            with rasterio.open(p, "w", dtype="float32", nodata=-9999, **prof) as d:
                d.write(v, 1)
        c = np.digitize(base + 0.4 * smooth(rng, N), [-0.8, -0.3, 0.3, 0.8]).astype("uint8") + 1
        c[hole] = 255
        q = D / f"c{t}.tif"
        if not q.exists():
            with rasterio.open(q, "w", dtype="uint8", nodata=255, **prof) as d:
                d.write(c, 1)
        vals.append(str(p))
        cls.append(str(q))
    gp = D / "zones.gpkg"
    if not gp.exists():
        ext = box(X0, Y0 - N * PX, X0 + N * PX, Y0)
        pts = MultiPoint(list(zip(rng.uniform(ext.bounds[0], ext.bounds[2], 2000),
                                  rng.uniform(ext.bounds[1], ext.bounds[3], 2000))))
        cells = [g.intersection(ext) for g in voronoi_polygons(pts, extend_to=ext).geoms]
        circles = [Point(rng.uniform(X0, X0 + N * PX), rng.uniform(Y0 - N * PX, Y0)).buffer(rng.uniform(300, 1500))
                   for _ in range(100)]
        geoms = cells + circles
        gpd.GeoDataFrame({"name": [f"z{i}" for i in range(len(geoms))]}, geometry=geoms, crs="EPSG:32723") \
            .to_file(gp, driver="GPKG")
    return vals, cls, gp


def janus(gp, kind, maps):
    out = D / f"janus_{kind}.csv"
    runs = []
    for _ in range(3):
        r = subprocess.run([str(EXE), str(gp), kind, "8", str(out), *maps], capture_output=True, text=True, check=True)
        runs.append(json.loads(r.stdout))
    j = min(runs, key=lambda x: x["seconds"])
    return j, out


def main():
    vals, cls, gp = make()
    res = {}
    # values
    j, out = janus(gp, "values", vals)
    import pandas as pd
    jt = pd.read_csv(out)
    t = time.perf_counter()
    rs = [zonal_stats(str(gp), p, stats=STATS, nodata=-9999) for p in vals]
    rs_s = time.perf_counter() - t
    worst = {s: 0.0 for s in STATS}
    zones = len(rs[0])
    for d in range(DATES):
        jd = jt[jt.date == f"v{d}"].sort_values("fid").reset_index(drop=True)
        for z in range(zones):
            r = rs[d][z]
            for s, col in zip(STATS, ["n", "mean", "std", "min", "p10", "p25", "median", "p75", "p90", "max"]):
                a, b = jd.at[z, col], r[s]
                if b is None or (isinstance(b, float) and np.isnan(b)):
                    if not (np.isnan(a) or (s == "count" and a == 0)):
                        worst[s] = float("inf")
                    continue
                worst[s] = max(worst[s], abs(a - b) / max(1.0, abs(b)))
    res["values"] = {"zones": j["zones"], "dates": DATES, "shared_pixels": j["shared"], "janus_s": j["seconds"],
                     "rasterstats_s": rs_s, "max_relative_difference_vs_rasterstats": worst}
    print("values", json.dumps(res["values"]))
    # classes
    j, out = janus(gp, "classes", cls)
    jt = pd.read_csv(out)
    t = time.perf_counter()
    rc = [zonal_stats(str(gp), p, categorical=True, nodata=255) for p in cls]
    rc_s = time.perf_counter() - t
    differ = 0
    for d in range(DATES):
        jd = jt[jt.date == f"c{d}"]
        got = {(int(r.fid) - 1, int(r.value)): int(r.pixels) for r in jd.itertuples()}
        want = {(z, int(k)): int(v) for z, m in enumerate(rc[d]) for k, v in m.items()}
        differ += sum(1 for k in set(got) | set(want) if got.get(k, 0) != want.get(k, 0))
    res["classes"] = {"zones": j["zones"], "dates": DATES, "janus_s": j["seconds"], "rasterstats_s": rc_s,
                      "counts_differing_from_rasterstats": differ}
    print("classes", json.dumps(res["classes"]))
    # exactextractr: the 8 dates at once
    r_script = D / "ee.R"
    r_script.write_text(f"""
suppressMessages({{library(terra); library(sf); library(exactextractr)}})
r <- rast(c({", ".join(repr(p.replace(chr(92), "/")) for p in vals)}))
z <- st_read("{str(gp).replace(chr(92), "/")}", quiet = TRUE)
for (k in 1:2) {{
  t0 <- Sys.time()
  m <- exact_extract(r, z, c("count", "mean", "stdev", "min", "quantile", "median", "max"),
                     quantiles = c(0.1, 0.25, 0.75, 0.9), progress = FALSE)
  s <- as.numeric(Sys.time() - t0, units = "secs")
}}
t0 <- Sys.time()
c <- exact_extract(rast(c({", ".join(repr(p.replace(chr(92), "/")) for p in cls)})), z, "frac", progress = FALSE)
sc <- as.numeric(Sys.time() - t0, units = "secs")
write.csv(m[, grep("^mean", names(m))], "{(D / "ee_mean.csv").as_posix()}", row.names = FALSE)
cat(sprintf('{{"values_s": %f, "classes_frac_s": %f}}', s, sc))
""")
    env = dict(os.environ, OMP_NUM_THREADS="8")
    r = subprocess.run([RSCRIPT, str(r_script)], capture_output=True, text=True, env=env)
    if r.returncode == 0:
        ee = json.loads(r.stdout.strip().splitlines()[-1])
        em = pd.read_csv(D / "ee_mean.csv")
        jt = pd.read_csv(D / "janus_values.csv")
        rel = []
        for d in range(DATES):
            a = jt[jt.date == f"v{d}"].sort_values("fid")["mean"].to_numpy()
            b = em.iloc[:, d].to_numpy()
            ok = ~np.isnan(a) & ~np.isnan(b)
            rel.append(np.abs(a[ok] - b[ok]) / np.maximum(1.0, np.abs(b[ok])))
        rel = np.concatenate(rel)
        res["exactextractr"] = {"values_s": ee["values_s"], "classes_frac_s": ee["classes_frac_s"],
                                "mean_relative_difference_median": float(np.median(rel)),
                                "mean_relative_difference_p99": float(np.percentile(rel, 99))}
    else:
        res["exactextractr"] = {"error": r.stderr[-2000:]}
    print("exactextractr", json.dumps(res["exactextractr"]))
    p = ROOT / "bench" / "zonal-vs-rasterstats.json"
    p.write_text(json.dumps(res, indent=2))
    print("written", p)


if __name__ == "__main__":
    main()
