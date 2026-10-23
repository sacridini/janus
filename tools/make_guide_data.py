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

       python tools/make_guide_data.py labels MAPBIOMAS SERIES OUT
  MAPBIOMAS  a MapBiomas land cover map (collection 8, a year), any extent
  SERIES     a file of the series (one date of the landsat folder): its grid
  OUT        a folder: mapbiomas.tif (the map on the series' grid, in the guides'
             five classes), samples.gpkg (points, `class`: up to 250 a class, at
             pixels whose 5 x 5 neighbourhood is all of their class) and
             areas.gpkg (the map's patches of 20 pixels or more as polygons,
             `class`, in the western half only: the labels of U-TAE)
Then:  janus --shots ml LANDSAT_FOLDER site/assets/img/ml   (and snic, presto)
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


# MapBiomas (collection 8) -> the guides' classes, in this order in the files: the class maps' colours (Paired,
# a colour a class in their order) then are blue for water and urban, light green for pasture, dark green for forest
CLASSES = {"Water": (33,), "Urban": (24,), "Pasture": (15,), "Forest": (3, 4, 6, 11), "Agriculture": (39, 41)}


def labels(mapbiomas, series, out):
    os.makedirs(out, exist_ok=True)
    ref = gdal.Open(series)
    w, h, gt = ref.RasterXSize, ref.RasterYSize, ref.GetGeoTransform()
    warped = gdal.Warp("", mapbiomas, format="MEM", outputBounds=(gt[0], gt[3] + h * gt[5], gt[0] + w * gt[1], gt[3]),
                       width=w, height=h, dstSRS=ref.GetProjection(), resampleAlg="near")
    raw = warped.ReadAsArray()
    cls = np.zeros_like(raw, dtype=np.uint8)
    names = list(CLASSES)
    for k, codes in enumerate(CLASSES.values()):
        cls[np.isin(raw, codes)] = k + 1
    drv = gdal.GetDriverByName("GTiff")
    m = drv.Create(os.path.join(out, "mapbiomas.tif"), w, h, 1, gdal.GDT_Byte, ["COMPRESS=DEFLATE"])
    m.SetGeoTransform(gt)
    m.SetProjection(ref.GetProjection())
    b = m.GetRasterBand(1)
    b.WriteArray(cls)
    b.SetNoDataValue(0)
    b.SetCategoryNames(["none"] + names)
    # cores: the 5 x 5 window all of one class
    core = cls > 0
    for dy in range(-2, 3):
        for dx in range(-2, 3):
            core &= np.roll(np.roll(cls, dy, 0), dx, 1) == cls
    core[:2, :] = core[-2:, :] = core[:, :2] = core[:, -2:] = False
    rng = np.random.default_rng(42)
    srs = ref.GetSpatialRef()
    vdrv = gdal.GetDriverByName("GPKG")
    path = os.path.join(out, "samples.gpkg")
    if os.path.exists(path):
        vdrv.Delete(path)
    vds = vdrv.Create(path, 0, 0, 0, gdal.GDT_Unknown)
    from osgeo import ogr
    lyr = vds.CreateLayer("samples", srs, ogr.wkbPoint)
    lyr.CreateField(ogr.FieldDefn("class", ogr.OFTString))
    for k, name in enumerate(names):
        ys, xs = np.nonzero(core & (cls == k + 1))
        pick = rng.choice(len(xs), min(250, len(xs)), replace=False) if len(xs) else []
        for i in pick:
            f = ogr.Feature(lyr.GetLayerDefn())
            f.SetField("class", name)
            x, y = gt[0] + (xs[i] + 0.5) * gt[1], gt[3] + (ys[i] + 0.5) * gt[5]
            f.SetGeometry(ogr.CreateGeometryFromWkt(f"POINT ({x} {y})"))
            lyr.CreateFeature(f)
        print(f"{name}: {len(pick)} points of {len(xs)} core pixels")
    vds = None
    # the western half's patches as polygons
    west = cls.copy()
    west[:, w // 2:] = 0
    mem = gdal.GetDriverByName("MEM").Create("", w, h, 1, gdal.GDT_Byte)
    mem.SetGeoTransform(gt)
    mem.SetProjection(ref.GetProjection())
    mb = mem.GetRasterBand(1)
    mb.WriteArray(west)
    mb.SetNoDataValue(0)
    gdal.SieveFilter(mb, mb, mb, 20, 4)
    path = os.path.join(out, "areas.gpkg")
    if os.path.exists(path):
        vdrv.Delete(path)
    tmp = gdal.GetDriverByName("Memory").Create("", 0, 0, 0, gdal.GDT_Unknown)
    poly = tmp.CreateLayer("p", srs, ogr.wkbPolygon)
    poly.CreateField(ogr.FieldDefn("code", ogr.OFTInteger))
    gdal.Polygonize(mb, mb, poly, 0)
    vds = vdrv.Create(path, 0, 0, 0, gdal.GDT_Unknown)
    lyr = vds.CreateLayer("areas", srs, ogr.wkbPolygon)
    lyr.CreateField(ogr.FieldDefn("class", ogr.OFTString))
    # by class, in the classes' order (the order the classes are met in)
    feats = sorted(poly, key=lambda f: f.GetField("code"))
    for f in feats:
        g = ogr.Feature(lyr.GetLayerDefn())
        g.SetField("class", names[f.GetField("code") - 1])
        g.SetGeometry(f.GetGeometryRef().Clone())
        lyr.CreateFeature(g)
    print(f"{lyr.GetFeatureCount()} polygons in the western half: {out}")


def main():
    gdal.UseExceptions()
    gdal.SetConfigOption("GDAL_PAM_ENABLED", "NO")
    gdal.SetCacheMax(1 << 30)
    a = sys.argv[1:]
    if len(a) in (4, 8) and a[0] == "landsat":
        landsat(a[1], a[2], a[3], [float(v) for v in a[4:]] if len(a) == 8 else None)
    elif len(a) == 4 and a[0] == "labels":
        labels(a[1], a[2], a[3])
    elif len(a) == 3 and a[0] == "annual":
        annual(a[1], a[2])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
