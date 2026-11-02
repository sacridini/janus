"""The k-means tool's fit (crates/janus-zeit/src/cluster.rs, by examples/vs_sklearn_kmeans.rs) against
scikit-learn's KMeans (k-means++ starts, Lloyd's algorithm, the best of 10 starts, its defaults otherwise), on the
same made-up NDVI-like series: 8 kinds of pixel (forests, pastures, two crops a year, a crop, a clearing, a regrowth,
water), each with its own seasonal amplitude and phase, noise, the gaps filled as the tool fills them. The inertia
(the sum of squared distances to the centres: the lower the better) of the mean of 3 seeds, how alike the two
partitions are (adjusted Rand index), and the time of the fit, the best of 3 runs (1 on the large set), on 1
thread and on 4.

    python tools/bench_kmeans_vs_sklearn.py      (writes bench/kmeans-vs-sklearn.json)
"""
import json
import subprocess
import tempfile
import time
from pathlib import Path

import numpy as np
import sklearn
from sklearn.cluster import KMeans
from sklearn.metrics import adjusted_rand_score
from threadpoolctl import threadpool_limits

ROOT = Path(__file__).resolve().parent.parent
K, STARTS, SEEDS, DATES = 8, 10, [0, 1, 2], 60


def series(n, seed):
    """n pixels' series of DATES dates (12 a year over 5 years)."""
    rng = np.random.default_rng(seed)
    t = np.arange(DATES) / 12.0
    season = lambda amp, phase: amp * np.sin(2 * np.pi * (t - phase))
    kinds = [
        0.82 + season(0.04, 0.2),  # forest
        0.55 + season(0.15, 0.1),  # pasture
        0.45 + season(0.3, 0.0) * np.sign(np.sin(4 * np.pi * t)),  # two crops a year
        0.5 + season(0.3, 0.3),  # a crop
        np.where(t < 2.5, 0.82, 0.35) + season(0.05, 0.2),  # a clearing
        np.clip(0.3 + 0.12 * t, 0, 0.85) + season(0.05, 0.2),  # a regrowth
        np.full(DATES, -0.1),  # water
        0.65 + season(0.08, 0.4),  # savanna
    ]
    kind = rng.integers(0, len(kinds), n)
    x = np.stack(kinds)[kind] + rng.normal(0, 0.04, (n, DATES)) + rng.normal(0, 0.05, (n, 1))
    return np.ascontiguousarray(x, dtype=np.float64)


def ours(path, n, threads, seed, runs):
    exe = ROOT / "target" / "release" / "examples" / "vs_sklearn_kmeans.exe"
    out = subprocess.run(
        [str(exe), str(path), str(n), str(DATES), str(K), str(STARTS), str(seed), str(threads), str(runs)],
        capture_output=True, text=True, check=True,
    )
    r = json.loads(out.stdout)
    labels = np.fromfile(str(path).removesuffix(".f64") + ".labels", dtype=np.uint8)
    return r["seconds"], r["inertia"], labels


def theirs(x, threads, seed, runs):
    best, km = np.inf, None
    with threadpool_limits(threads):
        for _ in range(runs):
            t = time.perf_counter()
            km = KMeans(K, init="k-means++", n_init=STARTS, algorithm="lloyd", random_state=seed).fit(x)
            best = min(best, time.perf_counter() - t)
    return best, km.inertia_, km.labels_


def main():
    subprocess.run(
        ["cargo", "build", "--release", "-j", "8", "-p", "janus-zeit", "--example", "vs_sklearn_kmeans"],
        cwd=ROOT, check=True,
    )
    result = {"sklearn": sklearn.__version__, "k": K, "starts": STARTS, "dates": DATES, "sets": []}
    with tempfile.TemporaryDirectory() as d:
        for n, runs in [(20_000, 3), (200_000, 1)]:
            entry = {"pixels": n, "runs": runs}
            inertia_o, inertia_s, ari = [], [], []
            for seed in SEEDS:
                x = series(n, seed)
                path = Path(d) / f"x{n}_{seed}.f64"
                x.tofile(path)
                _, io, lo = ours(path, n, 4, seed, 1)
                _, its, ls = theirs(x, 4, seed, 1)
                inertia_o.append(io)
                inertia_s.append(its)
                ari.append(adjusted_rand_score(ls, lo))
            entry["inertia_janus"] = float(np.mean(inertia_o))
            entry["inertia_sklearn"] = float(np.mean(inertia_s))
            entry["inertia_ratio"] = entry["inertia_janus"] / entry["inertia_sklearn"]
            entry["adjusted_rand"] = float(np.mean(ari))
            x = series(n, 0)
            path = Path(d) / f"x{n}_t.f64"
            x.tofile(path)
            for threads in [1, 4]:
                to, _, _ = ours(path, n, threads, 0, runs)
                ts, _, _ = theirs(x, threads, 0, runs)
                entry[f"seconds_{threads}t"] = {"janus": round(to, 4), "sklearn": round(ts, 4), "speedup": round(ts / to, 2)}
            print(json.dumps(entry))
            result["sets"].append(entry)
    out = ROOT / "bench" / "kmeans-vs-sklearn.json"
    out.write_text(json.dumps(result, indent=1) + "\n")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
