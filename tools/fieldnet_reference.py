"""The field network's reference for crates/janus-ml/tests/fieldnet.rs: segmentation_models_pytorch's
Unet(efficientnet-b3) (what Fields of The World and WRI's Trazo train) with a checkpoint's weights, its scores on
fixed inputs (little-endian float32, [b, 8, h, w] -> [b, 3, h, w]).

    python tools/fieldnet_reference.py Trazo3.ckpt crates/janus-ml/tests/data/fieldnet

Needs torch and segmentation_models_pytorch (0.5); the checkpoint is read with weights_only=True."""
import sys
from pathlib import Path

import numpy as np
import segmentation_models_pytorch as smp
import torch

ckpt, out = sys.argv[1], Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
ck = torch.load(ckpt, map_location="cpu", weights_only=True)
sd = {k[6:]: v for k, v in ck["state_dict"].items() if k.startswith("model.")}
net = smp.Unet("efficientnet-b3", encoder_weights=None, in_channels=8, classes=3)
net.load_state_dict(sd, strict=True)
net.eval()
rng = np.random.default_rng(11)
# reflectances / 3000 of two windows: a field-like pattern plus noise, and pure noise
b, h, w = 2, 64, 96
x = (rng.random((b, 8, h, w), dtype=np.float32) * 0.3).astype(np.float32)
yy, xx = np.mgrid[0:h, 0:w]
fields = ((yy // 20 + xx // 24) % 2).astype(np.float32)
for c, (lo, hi) in enumerate([(0.05, 0.12), (0.06, 0.10), (0.04, 0.08), (0.25, 0.45)] * 2):
    x[0, c] += lo + (hi - lo) * fields
x.tofile(out / "input.f32")
with torch.no_grad():
    y = net(torch.from_numpy(x)).numpy().astype(np.float32)
y.tofile(out / "scores.f32")
print(f"{ckpt}: scores {y.shape}, |mean| {np.abs(y).mean():.3f}")
