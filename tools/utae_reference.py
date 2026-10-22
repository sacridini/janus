"""The reference of Janus's U-TAE (crates/janus-ml, utae.rs): zeit.ai.utae.UTAE (a port of the authors' code,
utae-paps, layer for layer) with the authors' PASTIS weights (fold 1), on made-up patches (one of them with
dates without observation), written as float32 files that `cargo test -p janus-ml --release --test utae`
compares with the native network's scores (float32 tolerance).

    python tools/utae_reference.py WEIGHTS [OUT_DIR]

WEIGHTS is the authors' Fold_1/model.pth.tar (Zenodo 5172293); OUT_DIR defaults to
crates/janus-ml/tests/data/utae. Needs Zeit >= 2.0 with torch.
"""
import json
import os
import sys

import numpy as np
import torch

from zeit.ai.utae import UTAE

HERE = os.path.dirname(os.path.abspath(__file__))
weights = sys.argv[1]
out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, "..", "crates", "janus-ml", "tests", "data", "utae")
os.makedirs(out, exist_ok=True)

B, T, C, H, W = 2, 6, 10, 24, 24
rng = np.random.default_rng(20261022)
x = rng.normal(0, 1, (B, T, C, H, W)).astype(np.float32)
x[1, 2] = 0.0   # no observation on these dates
x[1, 4] = 0.0
positions = np.stack([np.sort(rng.uniform(0, 400, T)) for _ in range(B)]).astype(np.float32)

model = UTAE(input_dim=C)
state = torch.load(weights, map_location="cpu", weights_only=False)["state_dict"]
missing, unexpected = model.load_state_dict(state, strict=False)
assert not unexpected and all(k.endswith("denom") for k in missing), (missing, unexpected)
model.eval()
with torch.inference_mode():
    y = model(torch.from_numpy(x), batch_positions=torch.from_numpy(positions)).numpy().astype(np.float32)

x.tofile(os.path.join(out, "input.f32"))
positions.tofile(os.path.join(out, "positions.f32"))
y.tofile(os.path.join(out, "scores.f32"))
with open(os.path.join(out, "shape.json"), "w") as f:
    json.dump({"input": [B, T, C, H, W], "scores": list(y.shape)}, f)
print(f"{out}: scores {y.shape}, range {y.min():.3f} .. {y.max():.3f}")
