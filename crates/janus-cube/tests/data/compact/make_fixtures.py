"""Regenerates the fixtures of tests/compact.rs: Zeit's compact (int8) embedding GeoTIFFs and, next to each, the
float32 file Zeit writes for the same cube (the compact file read back by zeit.load_raster, so the expected values
are Zeit's own decode).

Run with an environment that has zeit >= 3.7 (save_raster(..., compact=True)), rasterio and xarray:

    python make_fixtures.py [output folder]      (default: the folder of this script)

Files written, each 10 x 8 pixels (EPSG:32720, 10 m):
    aef2_compact.tif / aef2_float.tif    AlphaEarth, 2 dates x 64 dimensions, exact AEF codes, some NaN pixels
    aef1_compact.tif / aef1_float.tif    the same, a single date
    tess2_compact.tif / tess2_float.tif  TESSERA-like, 2 dates x 32 dimensions of very different magnitudes, NaN
    tess1_compact.tif / tess1_float.tif  the same, a single date
    tess2_compact_cog.tif                tess2 as a COG (same values as tess2_float.tif)
"""

import sys
from pathlib import Path

import numpy as np
import pandas as pd
import xarray as xr

import zeit
from zeit import _alphaearth

W, H = 10, 8


def cube(data, source, years):
    da = xr.DataArray(
        data.astype(np.float32),
        dims=("time", "band", "y", "x"),
        coords={
            "time": pd.DatetimeIndex([pd.Timestamp(y, 1, 1) for y in years]),
            "band": [f"A{i:02d}" for i in range(data.shape[1])],
            "y": 8900000 - 5 - 10 * np.arange(H),
            "x": 500000 + 5 + 10 * np.arange(W),
        },
        name="embeddings",
    )
    da = da.rio.write_crs(32720)
    da.attrs.update({"embedding_source": source, "embedding_version": "1", "embedding_variant": "annual"})
    return da


def aef_data(rng, n):
    codes = rng.integers(-127, 128, size=(n, 64, H, W)).astype(np.int8)
    codes[:, :, 0, 0] = np.arange(-63, 1)   # a run of codes, from the negative ones to zero
    x = _alphaearth.dequantise(codes)
    x[0, 5, 2, 3] = np.nan          # one dimension NaN: the whole pixel-date is NoData
    x[-1, :, 4, 4] = np.nan
    return x


def tess_data(rng, n):
    mag = 10.0 ** rng.uniform(-3, 3, size=(n, 1, H, W))
    x = rng.normal(size=(n, 32, H, W)) * mag
    x[0, :, 0, 0] = 0.0             # an all-zero pixel
    x[-1, :, 0, 1] = 1e-6           # a tiny one
    x[0, :, 1, 1] = np.nan
    x[-1, 7, 3, 2] = np.nan
    return x.astype(np.float32)


def write(out, name, data, source, years, **kw):
    path = zeit.save_raster(cube(data, source, years), out / f"{name}_compact.tif", compact=True, **kw)
    # the float32 file of the same cube: Zeit's decode of the compact one
    zeit.save_raster(zeit.load_raster(path), out / f"{name}_float.tif")


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent
    out.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(20261114)
    write(out, "aef2", aef_data(rng, 2), "alphaearth", [2023, 2024])
    write(out, "aef1", aef_data(rng, 1), "alphaearth", [2024])
    t2 = tess_data(rng, 2)
    write(out, "tess2", t2, "tessera", [2023, 2024])
    write(out, "tess1", tess_data(rng, 1), "tessera", [2024])
    zeit.save_raster(cube(t2, "tessera", [2023, 2024]), out / "tess2_compact_cog.tif", compact=True, driver="COG")


if __name__ == "__main__":
    main()
