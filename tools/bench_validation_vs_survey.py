"""The areas of transitions between two dates by the direct, difference and ratio estimators (phase 10): Janus and
zeit.transitions against R's survey package (Lumley), the black box of tests/data/make_survey_fixtures.R; time and
agreement.

    python tools/bench_validation_vs_survey.py [DIR]    (rasterio, scipy, geopandas, Zeit; R with survey)

A made-up landscape of 4096 x 4096 pixels of 30 m in UTM zone 23S (0.09 ha): Forest, Pasture, Crop and Water at two
dates (patches cleared, cropped, regrown between them), two maps of the dates with errors (6% of the classes, 80% of
the changes found, 1.5% of false change), strata that are not the transitions (an older alert map: its alerts, a
buffer of 2 pixels, the rest), and two stratified random samples: `typical`, 250 + 200 + 150 points, and `large`,
2500 + 2000 + 1500. The reference of a point is the truth at its pixel.

Timed (the best of three for the counts, the median for the estimates):
- Janus (crates/janus-validation/examples/vs_survey.rs, release, THREADS threads): `mapped_transitions` (the maps
  read on the strata's grid, the area of every transition inside the strata) and `assess` of the transitions, by
  each estimator.
- Zeit: `zeit.transitions(map_from, map_to, points, strata=..., estimator=...)` on the maps in memory (the count of
  the transitions and the estimates), by each estimator.
- R survey: svydesign(ids = ~1, strata = ~h, weights = ~A_h / n_h) and, for every transition, svytotal (direct),
  the mapped area plus svytotal of y - x (difference), svyratio(~y, ~x) times the mapped area (ratio), svyratio for
  the user's and producer's accuracies, svymean for the overall; the mapped areas given (numpy's count).
- numpy: the count of the transitions inside the strata (np.unique of the pairs), as a script does it.

Agreement: the largest relative difference of the estimates and standard errors of every transition (and of the
accuracies) of Janus and of Zeit against R. Writes bench/validation-vs-survey.json.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

THREADS = int(os.environ.get("BENCH_THREADS", "3"))
for v in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "GDAL_NUM_THREADS", "RAYON_NUM_THREADS",
          "ZEIT_NUM_THREADS"):
    os.environ.setdefault(v, str(THREADS))
import numpy as np
import rasterio
from rasterio.transform import from_origin
from scipy.ndimage import binary_dilation, gaussian_filter

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_survey.exe"
RSCRIPT = Path(r"C:\Program Files\R\R-4.4.2\bin\Rscript.exe")
SIZE = 4096
PIXEL = 0.09  # ha
ESTIMATORS = ("direct", "difference", "ratio")
CASES = {"typical": (250, 200, 150), "large": (2500, 2000, 1500)}

R_CODE = r"""
suppressMessages(library(survey))
a <- commandArgs(trailingOnly = TRUE)
pts <- read.csv(a[1])
areas <- read.csv(a[2])
mapped <- read.csv(a[3], stringsAsFactors = FALSE)
reps <- as.integer(a[5])
area_h <- setNames(areas$area, areas$stratum)
n_h <- table(pts$h)
pts$w <- as.numeric(area_h[as.character(pts$h)] / n_h[as.character(pts$h)])
pts$m <- paste(pts$map_from, "->", pts$map_to)
pts$r <- paste(pts$ref_from, "->", pts$ref_to)
classes <- sort(unique(c(mapped$transition, pts$r)))
X <- setNames(rep(0, length(classes)), classes)
X[mapped$transition] <- mapped$area
run <- function(estimator) {
    d <- pts
    des <- svydesign(ids = ~1, strata = ~h, weights = ~w, data = d)
    out <- list()
    for (k in classes) {
        # (update() looks the names up in the design's variables first: columns of d, not local variables)
        d$y <- as.numeric(d$r == k)
        d$x <- as.numeric(d$m == k)
        d$yx <- d$y * d$x
        d$dd <- d$y - d$x
        y <- d$y
        x <- d$x
        des <- update(des, y = d$y, x = d$x, yx = d$yx, dd = d$dd)
        res <- list()
        if (estimator == "direct") {
            e <- svytotal(~y, des)
            res$area <- c(coef(e)[[1]], SE(e)[[1]])
        } else if (estimator == "difference") {
            e <- svytotal(~dd, des)
            res$area <- c(X[[k]] + coef(e)[[1]], SE(e)[[1]])
        } else if (sum(x) > 0) {
            e <- svyratio(~y, ~x, des)
            res$area <- c(coef(e)[[1]] * X[[k]], SE(e)[[1]] * X[[k]])
        } else {
            res$area <- c(NA, NA)
        }
        if (sum(x) > 0) {
            u <- svyratio(~yx, ~x, des)
            res$users <- c(coef(u)[[1]], SE(u)[[1]])
        } else {
            res$users <- c(NA, NA)
        }
        if (sum(y) > 0) {
            p <- svyratio(~yx, ~y, des)
            res$producers <- c(coef(p)[[1]], SE(p)[[1]])
        } else {
            res$producers <- c(NA, NA)
        }
        out[[k]] <- res
    }
    des <- update(des, agree = as.numeric(d$r == d$m))
    o <- svymean(~agree, des)
    list(classes = out, overall = c(coef(o)[[1]], SE(o)[[1]]))
}
res <- list(survey = as.character(packageVersion("survey")))
for (estimator in c("direct", "difference", "ratio")) {
    ts <- numeric(reps)
    for (i in 1:reps) {
        t0 <- Sys.time()
        r <- run(estimator)
        ts[i] <- as.numeric(difftime(Sys.time(), t0, units = "secs"))
    }
    r$seconds <- median(ts)
    res[[estimator]] <- r
}
jsonlite::write_json(res, a[4], digits = NA, auto_unbox = TRUE, na = "null")
"""


def write_tif(path, a, transform):
    prof = dict(driver="GTiff", width=a.shape[1], height=a.shape[0], count=1, dtype=a.dtype.name, crs="EPSG:32723",
                transform=transform, nodata=0, tiled=True, compress="deflate")
    with rasterio.open(path, "w", **prof) as d:
        d.write(a, 1)


def landscape(d):
    """The truth, the two maps and the strata, written to d; the truth kept for the reference."""
    rng = np.random.default_rng(20141114)
    n = SIZE

    def field(sigma, share):
        """Smooth patches over `share` of the pixels."""
        f = gaussian_filter(rng.standard_normal((n, n)), sigma)
        return f > np.quantile(f, 1 - share)

    f = gaussian_filter(rng.standard_normal((n, n)), 40)
    cover = np.digitize(f, np.quantile(f, [0.45, 0.75, 0.95]))  # Forest 45%, Pasture 30%, Crop 20%, Water 5%
    ref_from = (cover + 1).astype(np.uint8)
    ref_to = ref_from.copy()
    ref_to[field(12, 0.06) & (ref_from == 1)] = 2  # cleared
    ref_to[field(10, 0.05) & (ref_from == 2)] = 3  # cropped
    ref_to[field(8, 0.03) & (ref_from == 3)] = 1  # regrown

    def flip(a, p):
        k = rng.random(a.shape) < p
        out = a.copy()
        out[k] = ((a[k] - 1 + rng.integers(1, 4, k.sum())) % 4 + 1).astype(np.uint8)
        return out

    map_from = flip(ref_from, 0.06)
    changed = ref_from != ref_to
    map_to = map_from.copy()
    seen = changed & (rng.random(changed.shape) < 0.8)
    map_to[seen] = ref_to[seen]
    still = ~changed
    map_to[still] = flip(map_from, 0.015)[still]
    alert = (changed & (rng.random(changed.shape) < 0.7)) | (rng.random(changed.shape) < 0.01)
    near = binary_dilation(alert, np.ones((5, 5), bool))
    strata = np.where(alert, 2, np.where(near, 3, 1)).astype(np.uint8)
    tr = from_origin(270000, 7500000, 30, 30)
    for name, a in [("strata", strata), ("map_from", map_from), ("map_to", map_to)]:
        write_tif(d / f"{name}.tif", a, tr)
    return strata, map_from, map_to, ref_from, ref_to


def draw(strata, sizes, seed):
    rng = np.random.default_rng(seed)
    flat = strata.ravel()
    cells = []
    for h, k in zip((1, 2, 3), sizes):
        idx = np.flatnonzero(flat == h)
        cells.append(rng.choice(idx, k, replace=False))
    return np.concatenate(cells)


def numpy_count(strata, a, b):
    t = time.perf_counter()
    inside = strata > 0
    code = a[inside].astype(np.int32) * 256 + b[inside]
    vals, counts = np.unique(code, return_counts=True)
    s = time.perf_counter() - t
    return s, {f"{v // 256} -> {v % 256}": c * PIXEL for v, c in zip(vals.tolist(), counts.tolist())}


def rel(a, b):
    """The largest relative difference of two lists of numbers (None as NaN: both or neither)."""
    worst = 0.0
    for x, y in zip(a, b):
        x = np.nan if x is None else float(x)
        y = np.nan if y is None else float(y)
        if np.isnan(x) or np.isnan(y):
            if np.isnan(x) != np.isnan(y):
                return float("inf")
            continue
        worst = max(worst, abs(x - y) / max(abs(y), 1e-12))
    return worst


def zeit_run(d, cells, ref_from, ref_to, estimator, reps):
    import geopandas as gpd
    import rioxarray
    import zeit

    maps = [rioxarray.open_rasterio(d / f"{n}.tif").sel(band=1, drop=True).load() for n in ("map_from", "map_to")]
    strata = rioxarray.open_rasterio(d / "strata.tif").sel(band=1, drop=True).load()
    rows, cols = np.divmod(cells, SIZE)
    x, y = strata.rio.transform() * (cols + 0.5, rows + 0.5)
    pts = gpd.GeoDataFrame({"ref_from": ref_from.ravel()[cells].astype(int), "ref_to": ref_to.ravel()[cells].astype(int)},
                           geometry=gpd.points_from_xy(x, y), crs=strata.rio.crs)
    ts = []
    for _ in range(reps):
        t = time.perf_counter()
        tr = zeit.transitions(maps[0], maps[1], pts, strata=strata, estimator=estimator)
        ts.append(time.perf_counter() - t)
    return float(np.median(ts)), tr


def main():
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(tempfile.mkdtemp(prefix="janus-bench-survey-"))
    out_dir.mkdir(parents=True, exist_ok=True)
    if not EXE.exists():
        sys.exit(f"build it first: cargo build --release -j {THREADS} -p janus-validation --example vs_survey")
    t = time.perf_counter()
    strata, map_from, map_to, ref_from, ref_to = landscape(out_dir)
    print(f"landscape {SIZE} x {SIZE} written in {time.perf_counter() - t:.1f} s")
    numpy_s, mapped = numpy_count(strata, map_from, map_to)
    areas = {h: float((strata == h).sum() * PIXEL) for h in (1, 2, 3)}
    (out_dir / "areas.csv").write_text("stratum,area\n" + "".join(f"{h},{a!r}\n" for h, a in areas.items()))
    (out_dir / "mapped.csv").write_text("transition,area\n" + "".join(f"{k},{v!r}\n" for k, v in mapped.items()))
    (out_dir / "survey.R").write_text(R_CODE)
    report = {"size": [SIZE, SIZE], "pixel_ha": PIXEL, "threads": THREADS, "numpy_count_s": numpy_s}
    for case, sizes in CASES.items():
        cells = draw(strata, sizes, 7 if case == "typical" else 8)
        rows, cols = np.divmod(cells, SIZE)
        rf, rt = ref_from.ravel()[cells], ref_to.ravel()[cells]
        lines = ["row,col,ref_from,ref_to"] + [f"{r},{c},{a},{b}" for r, c, a, b in zip(rows, cols, rf, rt)]
        (out_dir / f"points_{case}.csv").write_text("\n".join(lines) + "\n")
        lines = ["h,map_from,map_to,ref_from,ref_to"] + [
            f"{h},{a},{b},{c},{e}"
            for h, a, b, c, e in zip(strata.ravel()[cells], map_from.ravel()[cells], map_to.ravel()[cells], rf, rt)
        ]
        (out_dir / f"survey_{case}.csv").write_text("\n".join(lines) + "\n")
        # Janus
        jpath = out_dir / f"janus_{case}.json"
        # Janus's own GDAL (beside janus.exe), not one on the PATH
        env = dict(os.environ, PATH=str(EXE.parents[1]) + os.pathsep + os.environ.get("PATH", ""))
        subprocess.run([str(EXE), str(out_dir / "strata.tif"), str(out_dir / "map_from.tif"),
                        str(out_dir / "map_to.tif"), str(out_dir / f"points_{case}.csv"), str(THREADS), str(jpath)],
                       check=True, env=env)
        jan = json.loads(jpath.read_text())
        # R survey
        rpath = out_dir / f"survey_{case}.json"
        reps_r = 3 if case == "typical" else 1
        t = time.perf_counter()
        subprocess.run([str(RSCRIPT), str(out_dir / "survey.R"), str(out_dir / f"survey_{case}.csv"),
                        str(out_dir / "areas.csv"), str(out_dir / "mapped.csv"), str(rpath), str(reps_r)], check=True)
        r_wall = time.perf_counter() - t
        rr = json.loads(rpath.read_text())
        res = {"points": int(len(cells)), "transitions": None, "survey_version": rr["survey"],
               "janus_count_s": jan["count_s"],
               "max_rel_diff_mapped_janus_numpy": rel([jan["mapped"].get(k, 0.0) for k in mapped],
                                                      list(mapped.values())),
               "rscript_wall_s": r_wall}
        for e in ESTIMATORS:
            je, re_ = jan["estimators"][e], rr[e]
            classes = list(re_["classes"])
            res["transitions"] = len(classes)
            zs, zt = zeit_run(out_dir, cells, ref_from, ref_to, e, 3)
            area = zt.area
            r_area = [x for k in classes for x in re_["classes"][k]["area"]]
            j_area = [x for k in classes for x in je["classes"][k]["area"]]
            z_area = [x for k in classes for x in ([area.estimate[k], area.se[k]] if k in area.index else [None, None])]
            r_acc = [x for k in classes for q in ("users", "producers") for x in re_["classes"][k][q]] + re_["overall"]
            j_acc = [x for k in classes for q in ("users", "producers") for x in je["classes"][k][q]] + je["overall"]
            z_acc = [x for k in classes for x in (
                [zt.users.estimate[k], zt.users.se[k], zt.producers.estimate[k], zt.producers.se[k]]
                if k in zt.users.index else [None] * 4)] + [zt.overall["estimate"], zt.overall["se"]]
            change = [k for k in classes if k.split(" -> ")[0] != k.split(" -> ")[1]]
            ci = {k: None if je["classes"][k]["area"][1] is None else je["classes"][k]["area"][1] * 1.959964
                  for k in change}
            res[e] = {
                "janus_assess_s": je["assess_s"],
                "janus_s": jan["count_s"] + je["assess_s"],
                "zeit_s": zs,
                "survey_s": re_["seconds"],
                "max_rel_diff_janus_survey": rel(j_area, r_area),
                "max_rel_diff_zeit_survey": rel(z_area, r_area),
                "max_rel_diff_accuracies_janus_survey": rel(j_acc, r_acc),
                "max_rel_diff_accuracies_zeit_survey": rel(z_acc, r_acc),
                "change_ci_ha": {k: None if v is None else round(v, 1) for k, v in ci.items()},
            }
            print(f"{case} {e}: Janus {res[e]['janus_s']:.3f} s (estimates {je['assess_s'] * 1e3:.3f} ms), "
                  f"Zeit {zs:.3f} s, survey {re_['seconds']:.3f} s; vs survey: Janus "
                  f"{res[e]['max_rel_diff_janus_survey']:.1e}, Zeit {res[e]['max_rel_diff_zeit_survey']:.1e}")
        report[case] = res
    path = ROOT / "bench" / "validation-vs-survey.json"
    path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"written {path}")


if __name__ == "__main__":
    main()
