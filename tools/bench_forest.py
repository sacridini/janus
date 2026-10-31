"""Janus's Random Forest (crates/janus-ml/src/forest.rs) against scikit-learn's RandomForestClassifier and R's
randomForest (Breiman & Cutler's code, the engine of sits_rfor), on the same samples: sits' own (MODIS NDVI and
Landsat-8 of Rondonia, its train/test split of tools/ml_sits_reference.R) and a larger made-up set
(make_classification). 500 trees, sqrt(p) values tried at each split, leaves down to 1 sample, bootstrap samples,
for all three. The test accuracy (the mean of 3 seeds) and the out-of-bag accuracy; the time to grow the forest and
to classify the test samples, the best of 3 (1 run on the large set), on 1 thread and on 4 (randomForest: 1 only).

    python tools/bench_forest.py      (writes bench/forest-vs-originals.json)
"""
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

import numpy as np
from sklearn.datasets import make_classification
from sklearn.ensemble import RandomForestClassifier

ROOT = Path(__file__).resolve().parent.parent
TREES, SEEDS = 500, 3
RSCRIPT = os.environ.get("RSCRIPT", r"C:\Program Files\R\R-4.4.2\bin\Rscript.exe")

R_CODE = r"""
suppressMessages(library(randomForest))
a <- commandArgs(trailingOnly = TRUE)
d <- a[[1]]; p <- as.integer(a[[2]]); trees <- as.integer(a[[3]]); seeds <- as.integer(strsplit(a[[4]], ",")[[1]]); runs <- as.integer(a[[5]])
rd <- function(f, what, size) readBin(file.path(d, f), what, n = file.size(file.path(d, f)) / size, size = size)
xtr <- matrix(rd("xtr.f32", "numeric", 4), ncol = p, byrow = TRUE)
xte <- matrix(rd("xte.f32", "numeric", 4), ncol = p, byrow = TRUE)
ytr <- factor(rd("ytr.u8", "integer", 1)); yte <- rd("yte.u8", "integer", 1)
for (seed in seeds) {
    fit <- Inf; pred <- Inf
    for (r in seq_len(runs)) {
        set.seed(seed)
        t <- system.time(m <- randomForest(xtr, ytr, ntree = trees))[["elapsed"]]
        fit <- min(fit, t)
        t <- system.time(got <- predict(m, xte))[["elapsed"]]
        pred <- min(pred, t)
    }
    acc <- mean(as.integer(as.character(got)) == yte)
    oob <- 1 - m$err.rate[trees, "OOB"]
    cat(sprintf('{"threads": 1, "seed": %d, "fit_ms": %.1f, "predict_ms": %.1f, "accuracy": %.6f, "oob": %.6f}\n',
                seed, fit * 1000, pred * 1000, acc, oob))
}
"""


def best(fn, runs):
    out = []
    for _ in range(runs):
        t = time.perf_counter()
        fn()
        out.append(time.perf_counter() - t)
    return min(out) * 1000


def sits_set(name):
    d = json.loads((ROOT / "crates" / "janus-ml" / "tests" / "data" / f"sits_{name}.json").read_text())
    labels = d["labels"]
    bands = [d["bands"]] if isinstance(d["bands"], str) else d["bands"]   # one band: unboxed by jsonlite
    x = {"train": [], "test": []}
    y = {"train": [], "test": []}
    for s in d["samples"]:
        x[s["split"]].append(np.asarray(s["values"], dtype=np.float32).ravel())   # dates x bands, as Janus reads
        y[s["split"]].append(labels.index(s["label"]))
    return (np.stack(x["train"]), np.array(y["train"]), np.stack(x["test"]), np.array(y["test"]),
            f"sits {name} ({len(d['timeline'])} dates x {len(bands)} bands, {len(labels)} classes)")


def made_up():
    x, y = make_classification(n_samples=25_000, n_features=64, n_informative=24, n_redundant=8, n_classes=8,
                               n_clusters_per_class=2, class_sep=1.2, random_state=7)
    x = x.astype(np.float32)
    return x[:20_000], y[:20_000], x[20_000:], y[20_000:], "make_classification (64 values, 8 classes)"


def sklearn_runs(xtr, ytr, xte, yte, runs):
    out = []
    for threads in (1, 4):
        for seed in range(SEEDS):
            m = RandomForestClassifier(n_estimators=TREES, max_features="sqrt", oob_score=True, n_jobs=threads,
                                       random_state=seed)
            t_fit = best(lambda: m.fit(xtr, ytr), runs)
            got = None

            def pred():
                nonlocal got
                got = m.predict(xte)
            t_pred = best(pred, runs)
            out.append({"threads": threads, "seed": seed, "fit_ms": t_fit, "predict_ms": t_pred,
                        "accuracy": float((got == yte).mean()), "oob": float(m.oob_score_)})
    return out


def summary(runs):
    by = {}
    for t in sorted({r["threads"] for r in runs}):
        rs = [r for r in runs if r["threads"] == t]
        by[t] = {"fit_ms": min(r["fit_ms"] for r in rs), "predict_ms": min(r["predict_ms"] for r in rs),
                 "accuracy": float(np.mean([r["accuracy"] for r in rs])), "oob": float(np.mean([r["oob"] for r in rs]))}
    return by


def main():
    d = Path(tempfile.gettempdir()) / "janus-forest-bench"
    d.mkdir(exist_ok=True)
    (d / "rf.R").write_text(R_CODE)
    env = dict(os.environ, CARGO_BUILD_JOBS="8", RAYON_NUM_THREADS="4", OMP_NUM_THREADS="4")
    subprocess.run(["cargo", "build", "--release", "-q", "-j", "8", "-p", "janus-ml", "--example", "forest_bench"],
                   check=True, cwd=ROOT, env=env)
    exe = ROOT / "target" / "release" / "examples" / ("forest_bench.exe" if os.name == "nt" else "forest_bench")
    results = []
    for xtr, ytr, xte, yte, name in [sits_set("samples_modis_ndvi"), sits_set("samples_l8_rondonia_2bands"),
                                     made_up()]:
        xtr.astype("<f4").tofile(d / "xtr.f32")
        xte.astype("<f4").tofile(d / "xte.f32")
        ytr.astype(np.uint8).tofile(d / "ytr.u8")
        yte.astype(np.uint8).tofile(d / "yte.u8")
        p = xtr.shape[1]
        runs = 1 if len(ytr) * TREES > 5_000_000 else 3
        print(f"\n{name}: {len(ytr)} training samples, {len(yte)} test, {p} values", flush=True)
        janus = [json.loads(l) for l in subprocess.run([str(exe), str(d), str(p), str(TREES), str(SEEDS)], check=True,
                                                        capture_output=True, text=True, env=env).stdout.splitlines()]
        sk = sklearn_runs(xtr, ytr, xte, yte, runs)
        r = subprocess.run([RSCRIPT, str(d / "rf.R"), str(d), str(p), str(TREES), ",".join(map(str, range(1, SEEDS + 1))),
                            str(runs)], capture_output=True, text=True, env=env)
        if r.returncode:
            raise SystemExit(r.stderr)
        rf = [json.loads(l) for l in r.stdout.splitlines() if l.startswith("{")]
        row = {"set": name, "train": len(ytr), "test": len(yte), "values": p,
               "janus": summary(janus), "sklearn": summary(sk), "randomForest": summary(rf)}
        results.append(row)
        print(f"{'':14}{'threads':>8}{'grow (ms)':>12}{'classify (ms)':>15}{'test acc.':>11}{'OOB acc.':>10}")
        for who in ("janus", "sklearn", "randomForest"):
            for t, s in row[who].items():
                print(f"{who:14}{t:>8}{s['fit_ms']:>12.1f}{s['predict_ms']:>15.1f}{s['accuracy']:>11.4f}{s['oob']:>10.4f}")
    out = ROOT / "bench" / "forest-vs-originals.json"
    out.write_text(json.dumps({"trees": TREES, "seeds": SEEDS, "results": results}, indent=1))
    print(f"\n{out}")


if __name__ == "__main__":
    main()
