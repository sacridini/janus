"""Download of foundation-model embeddings (zeit.load_embeddings) for Janus.

A job with {"tool": "embeddings", "params": {"source", "years", "bounds"
(west, south, east, north in longitude/latitude), "res" (metres, None =
native 10 m)}, "output_dir"} writes one GeoTIFF per year into output_dir, which
Janus then opens as a layer (one file per date, D bands):

- Int16 with a scale and offset per band (the GDAL metadata any GIS reads),
  nodata -32768; pixel-interleaved, so a pixel's vector is contiguous; tiled
  256 x 256, DEFLATE with the integer predictor;
- the band descriptions are the dimensions ("A00", "A01"...) and the dataset
  carries Zeit's ZEIT_EMBEDDING tag (source, version, model, licence), which
  tells Janus what the layer is.

Progress is reported per strip of rows of each year: the strips pull the
product's blocks over the network (Zeit keeps the last ones in a cache).
"""
import os
import time

import numpy as np

# Range of the values kept exactly enough in Int16 (|v| <= RANGE): AlphaEarth's
# vectors have norm 1; TESSERA's dequantised values reach about +-18 (1e-7 of them above 16).
_RANGE = {"alphaearth": 1.0, "tessera": 32.0}
_STRIP = 512  # rows per strip (TESSERA's block side, half of AlphaEarth's)
NODATA = -32768


def _quantise(block, scale):
    q = np.rint(block / scale)
    np.clip(q, -32767, 32767, out=q)
    q[~np.isfinite(block)] = NODATA
    return q.astype(np.int16)


def run(spec, progress):
    import rasterio
    import zeit
    from rasterio.windows import Window
    from zeit._embeddings import tags_of

    p = spec.get("params") or {}
    source = p["source"]
    years = [int(y) for y in p["years"]]
    west, south, east, north = (float(v) for v in p["bounds"])
    res = p.get("res") or None
    out_dir = spec["output_dir"]
    os.makedirs(out_dir, exist_ok=True)

    progress(0.0, "finding the data" + (" (the AlphaEarth file index, 78 MB, is downloaded once a month)"
                                         if source == "alphaearth" else ""))
    cube = zeit.load_embeddings((west, south, east, north), source=source, years=years,
                                res=float(res) if res else None, chunks="auto")
    T, D, H, W = (cube.sizes[d] for d in ("time", "band", "y", "x"))
    scale = _RANGE.get(source, 64.0) / 32767.0
    profile = dict(driver="GTiff", width=W, height=H, count=D, dtype="int16", nodata=NODATA,
                   crs=cube.rio.crs, transform=cube.rio.transform(), tiled=True, blockxsize=256, blockysize=256,
                   compress="deflate", predictor=2, interleave="pixel", BIGTIFF="IF_SAFER")
    tags = tags_of(cube)
    names = [str(b) for b in np.atleast_1d(cube.band.values)]
    strips = [(r, min(_STRIP, H - r)) for r in range(0, H, _STRIP)]
    total = T * len(strips)
    done = 0
    t0 = time.time()
    outputs = []
    for i in range(T):
        year = int(np.datetime64(cube.time.values[i], "Y").astype(int) + 1970)
        path = os.path.join(out_dir, f"{source}_{year}.tif")
        tmp = path + ".part"
        with rasterio.open(tmp, "w", **profile) as dst:
            dst.update_tags(**tags)
            dst.scales = [scale] * D
            dst.offsets = [0.0] * D
            for b, name in enumerate(names, 1):
                dst.set_band_description(b, name)
            for r, h in strips:
                block = np.asarray(cube.isel(time=i, y=slice(r, r + h)).values, dtype=np.float32)  # (D, h, W)
                dst.write(_quantise(block, scale), window=Window(0, r, W, h))
                done += 1
                el = time.time() - t0
                left = el / done * (total - done)
                progress(done / total, f"{year} ({i + 1}/{T}): rows {r + h}/{H}, ~{left:.0f} s left")
        os.replace(tmp, path)
        outputs.append({"id": str(year), "name": f"{source} {year}", "path": path})
    return {"outputs": outputs, "folder": out_dir, "size": [W, H], "dims": D, "years": years,
            "crs": str(cube.rio.crs)}
