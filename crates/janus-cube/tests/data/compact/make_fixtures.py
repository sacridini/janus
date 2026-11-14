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
    series_<source>_compact/ and series_<source>_float/   a file a year (2023, 2024; 16 dimensions of 8 x 6 pixels):
                                         what Janus's compact download writes, and the float32 series of it
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
            "y": 8900000 - 5 - 10 * np.arange(data.shape[2]),
            "x": 500000 + 5 + 10 * np.arange(data.shape[3]),
        },
        name="embeddings",
    )
    da = da.rio.write_crs(32720)
    da.attrs.update({"embedding_source": source, "embedding_version": "1", "embedding_variant": "annual"})
    return da


def aef_data(rng, n, d=64, h=H, w=W):
    codes = rng.integers(-127, 128, size=(n, d, h, w)).astype(np.int8)
    codes[:, :, 0, 0] = np.arange(-d + 1, 1)   # a run of codes, from the negative ones to zero
    x = _alphaearth.dequantise(codes)
    x[0, 5, 2, 3] = np.nan          # one dimension NaN: the whole pixel-date is NoData
    x[-1, :, 4, 4] = np.nan
    return x


def tess_data(rng, n, d=32, h=H, w=W):
    mag = 10.0 ** rng.uniform(-3, 3, size=(n, 1, h, w))
    x = rng.normal(size=(n, d, h, w)) * mag
    x[0, :, 0, 0] = 0.0             # an all-zero pixel
    x[-1, :, 0, 1] = 1e-6           # a tiny one
    x[0, :, 1, 1] = np.nan
    x[-1, 7, 3, 2] = np.nan
    return x.astype(np.float32)


def write(out, name, data, source, years, **kw):
    path = zeit.save_raster(cube(data, source, years), out / f"{name}_compact.tif", compact=True, **kw)
    # the float32 file of the same cube: Zeit's decode of the compact one
    zeit.save_raster(zeit.load_raster(path), out / f"{name}_float.tif")


def series(out, source, make, years):
    """What Janus downloads with the compact option: a file a year (single-date compact files, 16 dimensions of
    8 x 6 pixels), in a folder, and the float32 series of the same cubes in another."""
    rng = np.random.default_rng(7)
    for year in years:
        data = make(rng, 1, 16, 6, 8)
        name = f"{source}_{year}"
        path = zeit.save_raster(cube(data, source, [year]), out / f"series_{source}_compact" / f"{name}.tif",
                                compact=True)
        zeit.save_raster(zeit.load_raster(path), out / f"series_{source}_float" / f"{name}.tif")


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
    series(out, "alphaearth", aef_data, [2023, 2024])
    series(out, "tessera", tess_data, [2023, 2024])


if __name__ == "__main__":
    main()
