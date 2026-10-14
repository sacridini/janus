"""Inputs of the guides of the Zeit tools (site/*.html) from local stacks.

Usage: python tools/make_guide_data.py landsat STACK DATES OUT [WEST SOUTH EAST NORTH]
  STACK  a single GeoTIFF of Landsat Collection 2 Level-2 dates, 7 bands each in
         order (blue, green, red, NIR, SWIR1, SWIR2, QA_PIXEL), date after date
         (e.g. a Google Earth Engine export)
  DATES  its CSV of dates, one row per date in band order (column Date: YYYY-MM-DD)
  OUT    a folder: a GeoTIFF per date, OUT/ls_<date>.tif (UInt16 digital numbers,
         0 = no data, with the reflectance scale and offset), its bands named (what
         Janus reads to map the bands of CCDC, CODED and the NDFI, and to find the
         quality band). A stack interleaved by
         pixel is read once, by rows: reading one band of it would read them all.
  WEST.. the part to keep, in the stack's CRS (default: all of it)
Then:  janus --shots ccdc OUT site/assets/img/ccdc   (and coded)

       python tools/make_guide_data.py annual STACK OUT.vrt
  STACK  a single GeoTIFF of an annual index, one band per year (e.g. "yr1985"),
         with 0 outside the area it covers
  OUT    a VRT of it with 0 as no-data, so that the outside is not taken as values
Then:  janus --shots landtrendr OUT.vrt site/assets/img/landtrendr
"""
import csv
import math
import os
import sys

import numpy as np
from osgeo import gdal

NAMES = ["Blue", "Green", "Red", "NIR", "SWIR1", "SWIR2", "QA_PIXEL"]


def annual(stack, out):
    gdal.Translate(out, stack, format="VRT", noData=0)
    ds = gdal.Open(out)
    print(f"{ds.RasterCount} years, {ds.RasterXSize} x {ds.RasterYSize}, 0 as no-data: {out}")


def landsat(stack, dates_csv, out, box=None):
    ds = gdal.Open(stack)
    with open(dates_csv, newline="") as f:
        dates = [row["Date"] for row in csv.DictReader(f)]
    per = len(NAMES)
    if ds.RasterCount != len(dates) * per:
        sys.exit(f"{ds.RasterCount} bands, {len(dates)} dates: expected {per} bands per date")
    gt = ds.GetGeoTransform()
    x0, y0, w, h = 0, 0, ds.RasterXSize, ds.RasterYSize
    if box:
        west, south, east, north = box
        x0 = max(0, int(math.floor((west - gt[0]) / gt[1])))
        y0 = max(0, int(math.floor((north - gt[3]) / gt[5])))
        w = min(ds.RasterXSize, int(math.ceil((east - gt[0]) / gt[1]))) - x0
        h = min(ds.RasterYSize, int(math.ceil((south - gt[3]) / gt[5]))) - y0
    os.makedirs(out, exist_ok=True)
    drv = gdal.GetDriverByName("GTiff")
    outs = []
    for date in dates:
        d = drv.Create(os.path.join(out, f"ls_{date}.tif"), w, h, per, gdal.GDT_UInt16,
                       ["COMPRESS=DEFLATE", "PREDICTOR=2", "TILED=YES", "INTERLEAVE=BAND"])
        d.SetGeoTransform((gt[0] + x0 * gt[1], gt[1], 0, gt[3] + y0 * gt[5], 0, gt[5]))
        d.SetProjection(ds.GetProjection())
        for k, name in enumerate(NAMES):
            b = d.GetRasterBand(k + 1)
            b.SetDescription(name)
            b.SetNoDataValue(0)
            if name != "QA_PIXEL":  # reflectance = DN x 0.0000275 - 0.2: indices of DNs would be wrong
                b.SetScale(0.0000275)
                b.SetOffset(-0.2)
        outs.append(d)
    rows = 16
    for j in range(0, h, rows):
        n = min(rows, h - j)
        buf = np.frombuffer(ds.ReadRaster(x0, y0 + j, w, n, buf_type=gdal.GDT_Float64), np.float64)
        buf = buf.reshape(ds.RasterCount, n, w)
        vals = np.clip(np.nan_to_num(buf, nan=0), 0, 65535).astype(np.uint16)
        for i, d in enumerate(outs):
            for k in range(per):
                d.GetRasterBand(k + 1).WriteRaster(0, j, w, n, vals[i * per + k].tobytes())
        print(f"\r  rows {j + n}/{h}", end="", file=sys.stderr)
    print(file=sys.stderr)
    outs = None
    print(f"{len(dates)} dates ({dates[0]} .. {dates[-1]}), {w} x {h}: {out}")


def main():
    gdal.UseExceptions()
    gdal.SetConfigOption("GDAL_PAM_ENABLED", "NO")
    gdal.SetCacheMax(1 << 30)
    a = sys.argv[1:]
    if len(a) in (4, 8) and a[0] == "landsat":
        landsat(a[1], a[2], a[3], [float(v) for v in a[4:]] if len(a) == 8 else None)
    elif len(a) == 3 and a[0] == "annual":
        annual(a[1], a[2])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
