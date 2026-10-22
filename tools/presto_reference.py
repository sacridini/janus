"""The reference of Janus's Presto (crates/janus-ml, presto.rs): zeit.ai.presto (the authors' encoder in PyTorch,
their weights) on made-up series of Sentinel-2, Sentinel-1 and SRTM bands with gaps, written as a fixture that
`cargo test -p janus-ml --release --test presto` compares with the native encodings (float32 tolerance).

    python tools/presto_reference.py [OUT]

OUT defaults to crates/janus-ml/tests/data/presto.json. Needs Zeit >= 2.0 with torch; the weights are
downloaded once (zeit.ai.presto.pretrained_weights).
"""
import json
import os
import sys

import numpy as np
import torch

from zeit.ai.presto import Presto

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..", "crates", "janus-ml", "tests", "data", "presto.json")

BANDS = ["blue", "green", "red", "rededge1", "rededge2", "rededge3", "nir", "nir08", "swir1", "swir2", "VV", "VH",
         "elevation", "slope"]
N, T = 64, 12

rng = np.random.default_rng(20261022)
months = [((m + 3) % 12) + 1 for m in range(T)]   # April to March
x = np.empty((N, T, len(BANDS)), dtype=np.float32)
season = np.sin(2 * np.pi * np.arange(T) / 12)[None, :]
for k, b in enumerate(BANDS):
    if b in ("VV", "VH"):
        x[:, :, k] = (-12 if b == "VV" else -18) + 3 * season + rng.normal(0, 1, (N, T))
    elif b == "elevation":
        x[:, :, k] = rng.uniform(0, 1500, (N, 1))
    elif b == "slope":
        x[:, :, k] = rng.uniform(0, 30, (N, 1))
    else:
        base = rng.uniform(0.02, 0.4, (N, 1))
        x[:, :, k] = np.clip(base + 0.1 * season * (1 if "nir" in b or "edge" in b else -0.3) + rng.normal(0, 0.02, (N, T)), 0.001, 1)
gaps = rng.random((N, T)) < 0.2
x[gaps, :10] = np.nan            # cloudy dates: the optical bands only
x[:10, :, :10] = np.nan          # no optical data at all
x[10:15, :, 10:12] = np.nan      # no radar
x[15:20] = np.nan                # nothing: no encoding
latlon = np.stack([rng.uniform(-30, 10, N), rng.uniform(-70, -40, N)], 1).astype(np.float32)

model = Presto(BANDS, months, num_classes=1, reflectance_scale=1e4, s1_linear=False).eval()
with torch.inference_mode():
    keep = np.isfinite(x).any(axis=(1, 2))
    emb = np.full((N, 128), np.nan, dtype=np.float32)
    emb[keep] = model.embed(torch.from_numpy(x[keep]), torch.from_numpy(latlon[keep])).numpy()

out = {"bands": BANDS, "months": months, "reflectance_scale": 1e4, "s1_linear": False,
       "latlon": latlon.tolist(), "values": [[[None if not np.isfinite(v) else float(v) for v in row] for row in px] for px in x],
       "embeddings": [[None if not np.isfinite(v) else float(v) for v in e] for e in emb]}
with open(OUT, "w") as f:
    json.dump(out, f)
print(f"{OUT}: {int(keep.sum())} encodings of {N} pixels")
