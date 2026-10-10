"""Synthetic inputs for the self-tests (small, a few MB in all).

Usage: python tools/make_selftest_data.py <out>   (requires GDAL's Python bindings and numpy)
Then:  jn --selftest-ui <out>/A <out>/B <out>/C <out>/D <out>/E <out>/F
       jn --selftest-zeit <out>/serie
       jn --selftest-zeit <out>/landsat

  A, B   continuous layers; B starts 100 px east of A and its first value at
         (50, 100) is 2.0 (what --selftest-ui checks)
  C      one file per date with 5 bands (Blue, Green, Red, NIR, Fmask), clouds
         on the right half of every third date
  D, E   categorical (classes 3, 15, 24, 33), with and without a colour table
         and class names
  F      B reprojected from UTM 23S to UTM 22S (EPSG:32722; its grid turns ~2.3
         degrees), nearest neighbour at 15 m: every value is one of B's
  serie  21 annual dates with a break in 2010 (every Zeit tool that applies)
  landsat  monthly Blue..SWIR2 + Fmask, 2016-2021: forest degraded in 2019 and
         cleared in 2020, clouds Fmask misses (CCDC, NDFI, CODED, Tmask...)
"""
import os
import sys

import numpy as np
from osgeo import gdal, osr

SRS = osr.SpatialReference()
SRS.ImportFromEPSG(32723)


def create(path, w, h, bands, dtype, x0=500000, y0=7500000):
    d = gdal.GetDriverByName("GTiff").Create(path, w, h, bands, dtype)
    d.SetGeoTransform((x0, 30, 0, y0, 0, -30))
    d.SetProjection(SRS.ExportToWkt())
    return d


def layers(out):
    xx = np.tile(np.arange(300, dtype="float32"), (200, 1))
    for name, x0, base, ref in (("A", 500000, 1.0, 0), ("B", 503000, 2.0, 50)):
        os.makedirs(f"{out}/{name}", exist_ok=True)
        for i, y in enumerate(range(2000, 2016)):
            d = create(f"{out}/{name}/v_{y}.tif", 300, 200, 1, gdal.GDT_Float32, x0)
            d.GetRasterBand(1).WriteArray((base + 0.01 * (xx - ref) + 0.1 * i).astype("float32"))
            d = None


def reprojected(out):
    os.makedirs(f"{out}/F", exist_ok=True)
    for y in range(2000, 2016):
        gdal.Warp(f"{out}/F/v_{y}.tif", f"{out}/B/v_{y}.tif", dstSRS="EPSG:32722", xRes=15, yRes=15,
                  resampleAlg="near", dstNodata=float("nan"), creationOptions=["COMPRESS=DEFLATE", "PREDICTOR=3"])


def bands(out):
    W, H = 200, 120
    os.makedirs(f"{out}/C", exist_ok=True)
    names = ["Blue", "Green", "Red", "NIR", "Fmask"]
    for i, y in enumerate(range(2010, 2020)):
        d = create(f"{out}/C/LC08_{y}0701_SR.tif", W, H, 5, gdal.GDT_Float32, 600000, 7400000)
        rng = np.random.default_rng(i)
        for b in range(4):
            d.GetRasterBand(b + 1).WriteArray((0.05 + 0.1 * b + 0.01 * rng.random((H, W))).astype("float32"))
            d.GetRasterBand(b + 1).SetDescription(names[b])
        q = np.zeros((H, W), "float32")
        if i % 3 == 0:
            q[:, W // 2:] = 4  # cloud
        d.GetRasterBand(5).WriteArray(q)
        d.GetRasterBand(5).SetDescription("Fmask")
        d = None


def classes(out):
    # Forest (3) in the centre at the first date; (20, 40) turns from forest to pasture.
    W, H = 200, 120
    for name, table in (("D", True), ("E", False)):
        os.makedirs(f"{out}/{name}", exist_ok=True)
        for i, y in enumerate(range(2000, 2008)):
            a = np.full((H, W), 3, "uint8")
            a[:, :40] = 15
            a[:30, 40:] = 24
            a[100:, :] = 33
            a[35:45, 15:25] = 3 if i < 4 else 15
            d = create(f"{out}/{name}/lc_{y}.tif", W, H, 1, gdal.GDT_Byte, 600000, 7400000)
            b = d.GetRasterBand(1)
            b.WriteArray(a)
            if table:
                ct = gdal.ColorTable()
                ct.SetColorEntry(3, (31, 120, 40, 255))
                ct.SetColorEntry(15, (230, 200, 90, 255))
                ct.SetColorEntry(24, (200, 30, 30, 255))
                ct.SetColorEntry(33, (40, 80, 220, 255))
                b.SetRasterColorTable(ct)
                cats = [""] * 34
                cats[3], cats[15], cats[24], cats[33] = "Forest", "Pasture", "Urban", "Water"
                b.SetRasterCategoryNames(cats)
            d = None


def serie(out):
    os.makedirs(f"{out}/serie", exist_ok=True)
    rng = np.random.default_rng(0)
    for i, y in enumerate(range(2000, 2021)):
        a = (0.7 + 0.002 * i + rng.normal(0, 0.02, (120, 160))).astype("float32")
        a[40:80, 50:110] -= 0.4 * (y >= 2010)
        d = create(f"{out}/serie/ndvi_{y}.tif", 160, 120, 1, gdal.GDT_Float32)
        d.GetRasterBand(1).WriteArray(a)
        d = None


def landsat(out):
    """Monthly surface reflectance x 10000 (Blue..SWIR2) with Fmask, 2016-2021: forest, a
    degraded patch from mid 2019 (canopy opened, recovering), a cleared one from 2020 at
    the centre, clouds Fmask flags and a few it misses (bright, coded clear)."""
    W, H = 48, 36
    os.makedirs(f"{out}/landsat", exist_ok=True)
    names = ["Blue", "Green", "Red", "NIR", "SWIR1", "SWIR2", "Fmask"]
    forest = np.array([250, 550, 300, 3300, 1450, 550], "float32")
    opened = np.array([350, 650, 450, 2900, 2000, 1000], "float32")  # logged: NPV, soil and shade
    pasture = np.array([550, 900, 1150, 2600, 3000, 2000], "float32")
    rng = np.random.default_rng(1)
    k = 0
    for y in range(2016, 2022):
        for m in range(1, 13):
            t = y + (m - 0.5) / 12
            refl = np.broadcast_to(forest[:, None, None], (6, H, W)).copy()
            refl[3] += 250 * np.sin(2 * np.pi * (t - 2016))  # seasonal NIR
            if t >= 2019.45:  # degradation: a strong opening that recovers over two years
                f = max(0.0, 1 - (t - 2019.45) / 2.5)
                refl[:, 2:10, 2:16] = (forest * (1 - f) + opened * f)[:, None, None]
            if t >= 2020.2:  # clearing at the centre
                refl[:, 12:28, 18:40] = pasture[:, None, None]
            refl += rng.normal(0, 40, refl.shape)
            q = np.zeros((H, W), "float32")
            if k % 5 == 2:  # a cloud Fmask finds
                q[20:, :20] = 4
                refl[:, 20:, :20] += 4000
            if k % 11 == 6:  # haze Fmask misses: brighter in green, coded clear
                refl[:3, :, 24:] += 900
            d = create(f"{out}/landsat/LC08_{y}{m:02d}15_SR.tif", W, H, 7, gdal.GDT_Float32, 610000, 7410000)
            for b in range(7):
                d.GetRasterBand(b + 1).WriteArray(q if b == 6 else refl[b].astype("float32"))
                d.GetRasterBand(b + 1).SetDescription(names[b])
            d = None
            k += 1


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    for make in (layers, reprojected, bands, classes, serie, landsat):
        make(out)
    print(f"ok: {out}")


if __name__ == "__main__":
    main()
