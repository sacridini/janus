"""SOM clustering (zeit.som) for Janus: groups the pixels whose whole trajectories are
alike, without labels, and gives each group its typical series.

A Self-Organizing Map (Kohonen; Zeit's engine is a bit-for-bit port of MiniSom in C++)
is trained on a sample of the pixels of the run's window (the bridge's fit step, on
rows spread over it); every pixel then goes to its nearest neuron, whose prototype is
the cluster's typical series. Janus draws that series on the chart for the cluster
under the cursor.

The features of a pixel are its values on every date, so a pixel needs one on each:
gaps (clouds, no data) are filled by linear interpolation in time when enough dates
have a value, or the pixel is left out.
"""
import math

import numpy as np

import zeit_common as zc

MAX_CLUSTERS = 12  # the class colours Janus has (qualitative colormaps)

MANIFEST = {
    "id": "som",
    "name": "SOM clustering",
    "category": "Classification",
    "description": (
        "Self-Organizing Map (Kohonen), as implemented in Zeit: unsupervised clustering of the pixels by "
        "their whole series. The map is trained on a sample of the area, then every pixel goes to the "
        "nearest neuron; neighbouring neurons hold similar series. The result is a cluster map; hover it "
        "to see the typical series of the cluster under the cursor on the chart. Good for a first look "
        "at what kinds of trajectories an area has."),
    "requires": {"time": "any", "min_dates": 3},
    "modes": ["raster"],
    "params": [
        {"id": "x", "label": "Grid columns", "type": "int", "default": 3, "min": 1, "max": 12,
         "help": "Neurons per row of the map: columns x rows clusters (at most 12)."},
        {"id": "y", "label": "Grid rows", "type": "int", "default": 3, "min": 1, "max": 12,
         "help": "Rows of neurons of the map: columns x rows clusters (at most 12)."},
        {"id": "sample", "label": "Training sample (pixels)", "type": "int", "default": 20000, "min": 500,
         "max": 500000, "help": "Pixels the map is trained on, drawn at random from rows spread over the area."},
        {"id": "passes", "label": "Training passes", "type": "int", "default": 20, "min": 1, "max": 200,
         "help": "Passes over the sample (online training: one update per pixel and pass)."},
        {"id": "gaps", "label": "Dates without a value", "type": "enum", "default": "interpolate",
         "options": ["interpolate", "skip"],
         "labels": ["Interpolate in time", "Leave the pixel out"],
         "help": "A pixel needs a value on every date: fill the gaps linearly between its neighbours in time "
                 "(the first and last values repeat at the ends), or leave such pixels without a cluster."},
        {"id": "min_valid", "label": "Min dates with a value (%)", "type": "float", "default": 50.0, "min": 1.0,
         "max": 100.0, "help": "When interpolating: pixels with values on fewer dates get no cluster."},
        {"id": "seed", "label": "Random seed", "type": "int", "default": 42, "min": 0, "max": 1000000,
         "help": "Seed of the sample and of the training (same seed, same clusters)."},
    ],
    "outputs": [
        {"id": "cluster", "name": "Cluster", "colormap": "Paired", "unit": "class"},
        {"id": "distance", "name": "Distance to the typical series", "colormap": "Plasma", "unit": "value"},
    ],
    "chunk_cells": 100_000,
}


def fill_gaps(v, x, p):
    """[T, P] -> [T, P] with every value finite, or NaN columns for the pixels left out."""
    T = v.shape[0]
    ok = np.isfinite(v)
    if p["gaps"] == "skip":
        return np.where(ok.all(axis=0)[None], v, np.nan)
    keep = ok.sum(axis=0) >= max(2, math.ceil(float(p["min_valid"]) / 100.0 * T))
    t = np.arange(T)[:, None]
    prev = np.maximum.accumulate(np.where(ok, t, -1), axis=0)              # last date with a value <= t
    nxt = np.minimum.accumulate(np.where(ok, t, T)[::-1], axis=0)[::-1]    # first date with a value >= t
    a_i = np.clip(np.where(prev >= 0, prev, nxt), 0, T - 1)
    b_i = np.clip(np.where(nxt < T, nxt, prev), 0, T - 1)
    cols = np.arange(v.shape[1])[None]
    a, b = v[a_i, cols], v[b_i, cols]
    xa, xb = x[a_i], x[b_i]
    with np.errstate(invalid="ignore", divide="ignore"):
        f = np.where(xb > xa, (x[:, None] - xa) / (xb - xa), 0.0)
    out = a + (b - a) * f
    out[:, ~keep] = np.nan
    return out


def _features(p, stack, ctx):
    T = stack.shape[0]
    x = np.asarray(ctx["years"], dtype=np.float64)
    return fill_gaps(stack.reshape(T, -1), x, p)


def fit(p, stack, ctx):
    import zeit
    n = int(p["x"]) * int(p["y"])
    if n > MAX_CLUSTERS:
        raise ValueError(f"at most {MAX_CLUSTERS} clusters (columns x rows), got {int(p['x'])} x {int(p['y'])}")
    T = stack.shape[0]
    flat = stack.reshape(T, -1)
    cand = np.flatnonzero(np.isfinite(flat).any(axis=0))
    rng = np.random.default_rng(int(p["seed"]))
    if len(cand) > 3 * int(p["sample"]):  # enough to keep the sample after the gaps; bounded memory
        cand = np.sort(rng.choice(cand, 3 * int(p["sample"]), replace=False))
    X = fill_gaps(flat[:, cand], np.asarray(ctx["years"], dtype=np.float64), p)
    X = X[:, np.isfinite(X).all(axis=0)]                                    # [T, N]
    if X.shape[1] < n:
        raise ValueError(f"only {X.shape[1]} sampled pixels have enough values for {n} clusters")
    sample = min(int(p["sample"]), X.shape[1])
    ds = zeit.som(X[:, None, :], x=int(p["x"]), y=int(p["y"]), sample=sample,
                  num_iters=int(p["passes"]) * sample, algorithm="online", seed=int(p["seed"]), nodata=None,
                  n_jobs=ctx.get("n_jobs", -1))
    proto = np.asarray(ds["prototypes"].values, dtype=np.float64).reshape(n, -1)   # [neuron, T]
    counts = np.asarray(ds["n_pixels"].values, dtype=np.float64)
    # Neurons no sampled pixel chose sit between clusters: left out, so that the clusters
    # are numbered 1..m without holes (in the map's order: neighbours hold similar series).
    used = counts > 0
    proto, counts = proto[used], counts[used]
    share = 100.0 * counts / max(1.0, counts.sum())
    years = [float(v) for v in ctx["years"]]
    names = [f"Cluster {k + 1} ({share[k]:.0f}%)" for k in range(len(proto))]
    series = [{"x": years, "y": [float(v) for v in proto[k]]} for k in range(len(proto))]
    return {"prototypes": proto,
            "outputs": {"cluster": {"classes": names, "class_series": series}}}


def chunk(p, stack, ctx):
    T, h, w = stack.shape
    W = ctx["model"]["prototypes"]                                          # [n, T]
    X = _features(p, stack, ctx).T                                          # [P, T]
    ok = np.isfinite(X).all(axis=1)
    cluster = np.full(h * w, np.nan)
    dist = np.full(h * w, np.nan)
    if ok.any():
        Xo = X[ok]
        d2 = (Xo * Xo).sum(axis=1)[:, None] - 2.0 * Xo @ W.T + (W * W).sum(axis=1)[None]
        k = np.argmin(d2, axis=1)
        cluster[ok] = k + 1
        dist[ok] = np.sqrt(np.maximum(d2[np.arange(len(k)), k], 0.0))
    return zc.chunk_outputs({"cluster": cluster, "distance": dist}, (h, w))


TOOLS = [{"manifest": MANIFEST, "fit": fit, "chunk": chunk}]
