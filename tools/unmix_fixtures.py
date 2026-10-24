"""The references of janus-zeit's unmixing tools (tests/unmix.rs): zeit.unmix with a table of endmembers, called as
Janus's Copy Python calls it, on synthetic mixtures: a series of 6-band dates (reflectance x 10000, a few
observations missing) and a scene of 30 wavelengths (reflectance 0-1). Writes tests/data/unmix/.

    python tools/unmix_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import pandas as pd
import rasterio
import xarray as xr
from rasterio.transform import from_origin

import zeit

OUT = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data" / "unmix"
H, W = 6, 8


def write(path, stack):
    with rasterio.open(path, "w", driver="GTiff", width=W, height=H, count=stack.shape[0], dtype="float32",
                       crs="EPSG:32722", transform=from_origin(600000, 9000000, 30, 30), nodata=None) as d:
        d.write(stack.astype(np.float32))


def mixtures(rng, endmembers, n, noise):
    k = endmembers.shape[0]
    f = rng.dirichlet(np.ones(k), size=n)
    return f @ endmembers + noise * rng.standard_normal((n, endmembers.shape[1])), f


def unmix_case(data, names, spectra, constraint):
    sum_to_one, nonneg = {"fcls": (True, True), "nnls": (False, True), "scls": (True, False),
                          "ols": (False, False)}[constraint]
    scale = 10000.0 if (np.asarray(spectra) > 2).any() else 1.0
    table = pd.DataFrame(dict(zip(names, [list(map(float, s)) for s in spectra])),
                         index=list(data.band.values)).T
    return zeit.unmix(data, table / scale, bands=list(data.band.values), sum_to_one=sum_to_one, nonneg=nonneg,
                      scale=scale, ndfi=False, nodata=None, n_jobs=1)


def as_list(a):
    return [None if not np.isfinite(x) else float(x) for x in np.asarray(a, dtype=np.float32).ravel()]


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(7)
    expected = {}

    # a series: T dates of 6 bands, x 10000
    t = 5
    em = np.array([[400, 700, 450, 4200, 2000, 900],     # vegetation
                   [1800, 2400, 2900, 3400, 4100, 3600],  # soil
                   [600, 500, 300, 150, 80, 40]], float)  # water
    names = ["vegetation", "soil", "water"]
    obs, _ = mixtures(rng, em, t * H * W, 60.0)
    cube = obs.reshape(t, H, W, 6).astype(np.float32).astype(np.float64)   # as the GeoTIFFs keep it
    shown = cube[..., 3].copy()                     # the NIR as the shown series
    shown[1, 2, 3] = np.nan                         # an observation masked by the quality band
    cube[3, 4, 5, 2] = np.nan                       # a band without a value
    for b in range(6):
        write(OUT / f"series_b{b + 1:03d}.tif", cube[..., b])
    write(OUT / "series_shown.tif", shown)
    masked = np.where(np.isfinite(shown)[..., None], cube, np.nan)
    data = xr.DataArray(masked.transpose(0, 3, 1, 2), dims=("time", "band", "y", "x"),
                        coords={"band": [f"b{b + 1:03d}" for b in range(6)]})
    picks = [(0, 1, 1), (2, 3, 6), (4, 5, 0)]       # (date, row, col) of the "pins"
    spectra = [cube[d, r, c].tolist() for d, r, c in picks]
    for constraint in ("fcls", "nnls", "ols"):
        res = unmix_case(data, names, spectra, constraint)
        expected[f"series_{constraint}"] = {
            "names": names, "spectra": spectra, "constraint": constraint,
            "fraction": [as_list(res[n].values) for n in names],
            "rmse": as_list(res["rmse"].values),
        }

    # a scene: 30 wavelengths, reflectance 0-1
    nm = np.linspace(450, 2400, 30)
    em = np.stack([0.05 + 0.45 / (1 + np.exp(-(nm - 720) / 15)) - 0.2 * (nm > 1400),      # vegetation
                   0.1 + 0.25 * (nm - 450) / 1950,                                        # soil
                   0.08 * np.exp(-(nm - 450) / 300)])                                     # water
    em = np.clip(em, 0.005, None)
    obs, _ = mixtures(rng, em, H * W, 0.004)
    scene = obs.reshape(H, W, 30).transpose(2, 0, 1).astype(np.float32).astype(np.float64)
    write(OUT / "spectrum.tif", scene)
    data = xr.DataArray(scene, dims=("band", "y", "x"), coords={"band": [f"b{b + 1:03d}" for b in range(30)]})
    spectra = [scene[:, r, c].tolist() for r, c in ((0, 0), (3, 4), (5, 7))]
    for constraint in ("fcls", "scls"):
        res = unmix_case(data, names, spectra, constraint)
        expected[f"spectrum_{constraint}"] = {
            "names": names, "spectra": spectra, "constraint": constraint,
            "fraction": [as_list(res[n].values) for n in names],
            "rmse": as_list(res["rmse"].values),
        }
    (OUT / "expected.json").write_text(json.dumps(expected))
    print(f"{len(expected)} cases in {OUT}")


if __name__ == "__main__":
    main()
