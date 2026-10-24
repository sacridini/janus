"""The embeddings' few-shot classes and direction of change (janus-embed) against scikit-learn, on the same data: a
layer of 1000 x 1000 pixels and 64 dimensions (AlphaEarth's), two years, four classes of regions, a patch that
changed; 15 or 50 samples a class (pins). The logistic regression is the same problem as scikit-learn's
(LogisticRegression on standardised samples, class_weight="balanced", C = 1 / (lambda n): the intercept free), so
their probabilities agree (scikit-learn's to a tolerance of 1e-12; timed with its default); the direction basis is the PCA of the same differences.
Timed: the fit (with the held-out accuracy: leave-one-out up to 60 samples, else 5 folds, as Janus), the classes of
the whole year, the direction basis; scikit-learn on 1 thread and on all (threadpoolctl).

    python tools/bench_embeddings_sklearn.py
"""
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

import numpy as np
from sklearn.decomposition import PCA
from sklearn.linear_model import LogisticRegression
from sklearn.model_selection import PredefinedSplit, cross_val_predict
from sklearn.pipeline import make_pipeline
from sklearn.preprocessing import StandardScaler
from threadpoolctl import threadpool_limits

ROOT = Path(__file__).resolve().parent.parent
W = H = 1000
D, K, RANGE, LAMBDA, THRESHOLD = 64, 4, 0.5, 0.1, 0.2


def best(fn, repeat=3):
    fn()
    out = []
    for _ in range(repeat):
        t = time.perf_counter()
        fn()
        out.append(time.perf_counter() - t)
    return min(out) * 1000


def layer(rng):
    """Two years of vectors on the store's grid (multiples of RANGE / 127), each pixel of one of K regions."""
    scale = np.float32(RANGE) / np.float32(127)
    centres = rng.normal(0, 0.12, (K, D))
    yy, xx = np.mgrid[:H, :W]
    region = ((yy * 4 // H) + (xx * 4 // W) + (yy // 37 + xx // 53) % 2) % K
    years = []
    for t in range(2):
        v = centres[region] + rng.normal(0, 0.05, (H, W, D))
        if t == 1:   # a patch changed to another region's vectors, shifted
            patch = (slice(300, 600), slice(200, 700))
            v[patch] = centres[(region[patch] + 1) % K] + 0.08 + rng.normal(0, 0.05, v[patch].shape)
        q = np.clip(np.round(v / scale), -127, 127)
        years.append((q.astype(np.float32) * scale).reshape(-1, D))
    return years, region.ravel()


def candidates(t1, t0, threshold):
    """janus-embed's direction sample: every step-th pixel, the differences of those whose cosine distance is above
    the threshold, up to 65536 of them."""
    n = t1.shape[0]
    step = max(-(-n // 262_144), 1)
    p = np.arange(1 % step, n, step)
    a, b = t1[p].astype(np.float64), t0[p].astype(np.float64)
    dist = (1 - (a * b).sum(1) / (np.linalg.norm(a, axis=1) * np.linalg.norm(b, axis=1))).astype(np.float32)
    above = p[dist > threshold]
    keep = min(len(above), 65536)
    pick = above[(np.arange(keep) * len(above)) // keep]
    return t1[pick] - t0[pick]


def main():
    d = Path(tempfile.gettempdir()) / "janus-vs-sklearn"
    d.mkdir(exist_ok=True)
    rng = np.random.default_rng(7)
    years, region = layer(rng)
    for t, v in enumerate(years):
        v.tofile(d / f"t{t}.f32")
    exe = ROOT / "target" / "release" / "examples" / ("vs_sklearn.exe" if os.name == "nt" else "vs_sklearn")
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "janus-embed", "--example", "vs_sklearn"], check=True,
                   cwd=ROOT)
    for per_class in (15, 50):
        idx = np.concatenate([rng.choice(np.flatnonzero(region == c), per_class, replace=False) for c in range(K)])
        x, y = years[1][idx], np.repeat(np.arange(K), per_class)
        x64 = x.astype(np.float64)   # the comparison: scikit-learn fits float32 samples in float32
        x.tofile(d / "samples.f32")
        y.astype(np.uint8).tofile(d / "labels.u8")
        janus = json.loads(subprocess.run([str(exe), str(d), str(W), str(H), str(D), str(RANGE), str(LAMBDA),
                                           str(THRESHOLD)], capture_output=True, text=True, check=True).stdout)
        n = len(y)
        # timed with scikit-learn's defaults; compared with its solution to 1e-12
        model = make_pipeline(StandardScaler(), LogisticRegression(C=1 / (LAMBDA * n), class_weight="balanced"))
        exact = make_pipeline(StandardScaler(), LogisticRegression(C=1 / (LAMBDA * n), class_weight="balanced",
                                                                   tol=1e-12, max_iter=100000))
        folds = np.arange(n) if n <= 60 else np.concatenate([np.arange(per_class) % 5 for _ in range(K)])

        def fit():
            model.fit(x, y)
            cross_val_predict(model, x, y, cv=PredefinedSplit(folds))

        # the same model: probabilities of the samples and of the year, the classes of the year
        model.fit(x, y)
        exact.fit(x64, y)
        w = np.fromfile(d / "janus_w.f32", np.float32).reshape(K, D)
        b = np.fromfile(d / "janus_b.f32", np.float32)
        z = x.astype(np.float64) @ w.T + b
        pj = np.exp(z - z.max(1, keepdims=True))
        pj /= pj.sum(1, keepdims=True)
        p_diff = float(np.abs(pj - exact.predict_proba(x64)).max())
        proba = exact.predict_proba(years[1].astype(np.float64))
        cls_j = np.fromfile(d / "janus_class.u8", np.uint8)
        score_j = np.fromfile(d / "janus_score.f32", np.float32)
        agree = float((proba.argmax(1) == cls_j).mean())
        s_diff = float(np.abs(proba.max(1) - score_j).max())
        diffs = candidates(years[1], years[0], THRESHOLD)
        comps = np.fromfile(d / "janus_comps.f32", np.float32).reshape(3, D)
        pca = PCA(3, svd_solver="covariance_eigh").fit(diffs)
        cos = np.abs((pca.components_ * comps).sum(1))
        for threads in (1, None):
            with threadpool_limits(threads):
                t_fit = best(fit, 5)
                t_apply = best(lambda: model.predict_proba(years[1]).argmax(1))
                t_dir = best(lambda: PCA(3, svd_solver="covariance_eigh").fit(candidates(years[1], years[0],
                                                                                           THRESHOLD)))
            jt = janus[0] if threads == 1 else janus[1]
            label = "1 thread" if threads == 1 else f"all ({jt['threads']} Janus)"
            print(f"{K}x{per_class} samples, {label}:  fit+CV sklearn {t_fit:8.2f} ms  Janus {jt['fit_ms']:7.2f} ms "
                  f"(x{t_fit / jt['fit_ms']:.0f}) | year sklearn {t_apply:8.1f} ms  Janus {jt['apply_ms']:6.1f} ms "
                  f"(x{t_apply / jt['apply_ms']:.0f}) | direction sklearn {t_dir:7.1f} ms  Janus "
                  f"{jt['direction_ms']:6.1f} ms (x{t_dir / jt['direction_ms']:.1f})")
        print(f"  same model: probabilities of the samples to {p_diff:.1e}, the year's classes {agree:.5f} the same, "
              f"scores to {s_diff:.1e}; direction: {len(diffs)} differences (Janus {janus[0]['direction_samples']}), "
              f"|cos| of the components {np.round(cos, 6).tolist()}, eigenvalues {np.round(pca.explained_variance_, 6).tolist()}"
              f" (Janus {np.round(janus[0]['eig'], 6).tolist()})")


if __name__ == "__main__":
    main()
