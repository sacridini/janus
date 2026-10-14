"""Real data for the validation tutorial (site/validation.html) and its screenshots.

Usage: python tools/make_tutorial_data.py <out>   (GDAL's Python bindings, numpy, pystac-client;
       a few minutes and ~60 MB, read in place from public cloud buckets)
Then:  janus --shots-validation <out> <site/assets/img/validation>

An area of 16 x 13 km south-west of Novo Progresso (Pará, Brazil), on the BR-163
deforestation front:

  <out>/s2/s2_dry_<year>0801.tif   Sentinel-2 L2A dry-season composites, 2017-2024:
         the per-pixel median of the clearest scenes of June to September
         (Earth Search, tiles 21MXN and its neighbour), clouds and shadows
         masked with the SCL;
         Blue, Green, Red, NIR, SWIR1, SWIR2 at 20 m, reflectance x 10000 (Int16,
         0 = no clear observation), UTM 21S
  <out>/gfc_loss_2018_2023.tif     the map to validate: Global Forest Change
         v1.11 (Hansen et al. 2013) year of forest loss, 2018 to 2023 (UInt16:
         the year, 0 = no loss in those years, no-data), in its own grid (WGS 84,
         0.00025 degrees, ~28 m)
  <out>/mapbiomas_loss_2018_2023.tif  a second map assessed with the same sample:
         the first year of 2018-2023 in which a pixel of forest formation (class
         3) in MapBiomas Collection 9 is no longer forest the year after it was
         (same coding), in MapBiomas' grid (WGS 84, 1 arc-second, ~30 m)
"""
import os
import sys

import numpy as np
from osgeo import gdal, osr
from pystac_client import Client

gdal.UseExceptions()
gdal.SetConfigOption("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR")
gdal.SetConfigOption("GDAL_HTTP_MAX_RETRY", "5")
gdal.SetConfigOption("GDAL_HTTP_RETRY_DELAY", "2")

WEST, SOUTH, EAST, NORTH = -55.76, -7.36, -55.61, -7.24
YEARS = range(2017, 2025)
SCENES = 4  # clearest scenes per year
RES = 20
UTM = 32721  # UTM 21S
BANDS = [("blue", "Blue"), ("green", "Green"), ("red", "Red"), ("nir", "NIR"), ("swir16", "SWIR1"), ("swir22", "SWIR2")]
GFC = "/vsicurl/https://storage.googleapis.com/earthenginepartners-hansen/GFC-2023-v1.11/Hansen_GFC-2023-v1.11_lossyear_00N_060W.tif"


def read(ds, band=1):
    b = ds.GetRasterBand(band)
    dt = {gdal.GDT_Byte: np.uint8, gdal.GDT_UInt16: np.uint16, gdal.GDT_Int16: np.int16, gdal.GDT_Float32: np.float32}[b.DataType]
    return np.frombuffer(b.ReadRaster(), dt).reshape(ds.RasterYSize, ds.RasterXSize)


def write(ds, a, band=1):
    ds.GetRasterBand(band).WriteRaster(0, 0, a.shape[1], a.shape[0], np.ascontiguousarray(a).tobytes())


def utm_bounds():
    src, dst = osr.SpatialReference(), osr.SpatialReference()
    src.ImportFromEPSG(4326)
    dst.ImportFromEPSG(UTM)
    src.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
    dst.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
    ct = osr.CoordinateTransformation(src, dst)
    xs, ys = zip(*[ct.TransformPoint(x, y)[:2] for x in (WEST, EAST) for y in (SOUTH, NORTH)])
    snap = lambda v, f: f(v / RES) * RES
    return snap(min(xs), np.floor), snap(min(ys), np.floor), snap(max(xs), np.ceil), snap(max(ys), np.ceil)


def loss_map(out):
    path = f"{out}/gfc_loss_2018_2023.tif"
    if os.path.exists(path):
        return
    src = gdal.Open(GFC)
    gt = src.GetGeoTransform()
    x0, y0 = int(round((WEST - gt[0]) / gt[1])), int(round((NORTH - gt[3]) / gt[5]))
    w, h = int(round((EAST - WEST) / gt[1])), int(round((SOUTH - NORTH) / gt[5]))
    b = src.GetRasterBand(1)
    a = np.frombuffer(b.ReadRaster(x0, y0, w, h), np.uint8).reshape(h, w)
    year = np.where((a >= 18) & (a <= 23), 2000 + a.astype(np.uint16), 0).astype(np.uint16)
    d = gdal.GetDriverByName("GTiff").Create(path, w, h, 1, gdal.GDT_UInt16, ["COMPRESS=DEFLATE", "TILED=YES"])
    d.SetGeoTransform((gt[0] + x0 * gt[1], gt[1], 0, gt[3] + y0 * gt[5], 0, gt[5]))
    d.SetProjection(src.GetProjection())
    write(d, year)
    d.GetRasterBand(1).SetDescription("Year of forest loss (GFC v1.11)")
    d.GetRasterBand(1).SetNoDataValue(0)  # no loss: transparent over the images (and no change, for a map of events)
    d = None
    print(f"{path}: {w} x {h}, {int((year > 0).sum())} pixels of loss in 2018-2023")


def mapbiomas_loss(out):
    path = f"{out}/mapbiomas_loss_2018_2023.tif"
    if os.path.exists(path):
        return
    url = "/vsicurl/https://storage.googleapis.com/mapbiomas-public/initiatives/brasil/collection_9/lclu/coverage/brasil_coverage_{}.tif"
    year, prev, geo = None, None, None
    for y in range(2017, 2024):
        src = gdal.Open(url.format(y))
        gt = src.GetGeoTransform()
        x0, y0 = int(round((WEST - gt[0]) / gt[1])), int(round((NORTH - gt[3]) / gt[5]))
        w, h = int(round((EAST - WEST) / gt[1])), int(round((SOUTH - NORTH) / gt[5]))
        a = np.frombuffer(src.GetRasterBand(1).ReadRaster(x0, y0, w, h), np.uint8).reshape(h, w)
        if prev is None:
            year = np.zeros((h, w), np.uint16)
            geo = ((gt[0] + x0 * gt[1], gt[1], 0, gt[3] + y0 * gt[5], 0, gt[5]), src.GetProjection())
        else:
            year[(year == 0) & (prev == 3) & (a != 3)] = y
        prev = a
    d = gdal.GetDriverByName("GTiff").Create(path, year.shape[1], year.shape[0], 1, gdal.GDT_UInt16, ["COMPRESS=DEFLATE", "TILED=YES"])
    d.SetGeoTransform(geo[0])
    d.SetProjection(geo[1])
    write(d, year)
    d.GetRasterBand(1).SetDescription("Year of forest loss (MapBiomas C9)")
    d.GetRasterBand(1).SetNoDataValue(0)
    d = None
    print(f"{path}: {year.shape[1]} x {year.shape[0]}, {int((year > 0).sum())} pixels of loss in 2018-2023")


def scale(asset):
    # Only the scale: Earth Search's COGs are harmonized already, the -0.1 offset its
    # newer items declare (processing baseline 04.00+) would be taken off twice
    # (the same place and day reads the same DN in both processings).
    rb = (asset.extra_fields.get("raster:bands") or [{}])[0]
    return rb.get("scale", 0.0001)


def composite(out, catalog, year, bounds):
    path = f"{out}/s2/s2_dry_{year}0801.tif"
    if os.path.exists(path):
        return
    items = catalog.search(collections=["sentinel-2-l2a"], bbox=[WEST, SOUTH, EAST, NORTH],
                           datetime=f"{year}-06-01/{year}-09-30", query={"eo:cloud_cover": {"lt": 20}}).item_collection()
    # The clearest scenes of each tile over the area (it straddles two), one
    # processing of each acquisition (Earth Search keeps reprocessings as _1, _2...).
    seen, picked, per_tile = set(), [], {}
    for it in sorted(items, key=lambda i: i.properties["eo:cloud_cover"]):
        tile, day = it.id.split("_")[1], it.properties["datetime"][:10]
        if (tile, day) not in seen and per_tile.get(tile, 0) < SCENES:
            seen.add((tile, day))
            per_tile[tile] = per_tile.get(tile, 0) + 1
            picked.append(it)
    xmin, ymin, xmax, ymax = bounds
    W, H = int((xmax - xmin) / RES), int((ymax - ymin) / RES)
    stack = np.zeros((len(picked), len(BANDS), H, W), np.float32)
    valid = np.zeros((len(picked), H, W), bool)
    for k, it in enumerate(picked):
        opts = dict(format="MEM", dstSRS=f"EPSG:{UTM}", outputBounds=bounds, width=W, height=H)
        scl = read(gdal.Warp("", "/vsicurl/" + it.assets["scl"].href, resampleAlg="near", **opts))
        valid[k] = np.isin(scl, [4, 5, 6, 7, 11])  # vegetation, bare, water, unclassified, snow: not cloud nor shadow
        for b, (key, _) in enumerate(BANDS):
            a = read(gdal.Warp("", "/vsicurl/" + it.assets[key].href, resampleAlg="average", **opts)).astype(np.float32)
            valid[k] &= a > 0
            stack[k, b] = a * scale(it.assets[key]) * 10000
        print(f"  {it.id}: {it.properties['eo:cloud_cover']:.1f}% cloud, {valid[k].mean():.0%} clear here")
    stack[~np.broadcast_to(valid[:, None], stack.shape)] = np.nan
    with np.errstate(all="ignore"):
        med = np.nanmedian(stack, axis=0)
    med = np.where(np.isnan(med), 0, np.clip(np.round(med), 1, 32767)).astype(np.int16)
    os.makedirs(f"{out}/s2", exist_ok=True)
    d = gdal.GetDriverByName("GTiff").Create(path, W, H, len(BANDS), gdal.GDT_Int16, ["COMPRESS=DEFLATE", "PREDICTOR=2", "TILED=YES"])
    d.SetGeoTransform((xmin, RES, 0, ymax, 0, -RES))
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(UTM)
    d.SetProjection(srs.ExportToWkt())
    for b, (_, name) in enumerate(BANDS):
        write(d, med[b], b + 1)
        d.GetRasterBand(b + 1).SetDescription(name)
        d.GetRasterBand(b + 1).SetNoDataValue(0)
    d = None
    print(f"{path}: {len(picked)} scenes, {(med[0] > 0).mean():.1%} of the pixels with a clear observation")


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    loss_map(out)
    mapbiomas_loss(out)
    catalog = Client.open("https://earth-search.aws.element84.com/v1")
    bounds = utm_bounds()
    for y in YEARS:
        composite(out, catalog, y, bounds)


if __name__ == "__main__":
    main()
