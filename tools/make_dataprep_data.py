#!/usr/bin/env python3
"""The inputs of `janus --shots dataprep` (site/data-preparation.html): real Sentinel-2 and Landsat scenes from
Planetary Computer, cut to a window and written as one GeoTIFF a date.

    python tools/make_dataprep_data.py serra OUT
    python tools/make_dataprep_data.py topo OUT
    python tools/make_dataprep_data.py mixed OUT
    python tools/make_dataprep_data.py pins OUT
    python tools/make_dataprep_data.py mixed-pins OUT
    python tools/make_dataprep_data.py l1c OUT
    python tools/make_dataprep_data.py cloudy OUT
    python tools/make_dataprep_data.py improphe OUT

serra: 10 x 10 km of the Serra dos Orgaos (Teresopolis, Rio de Janeiro, about 22.5 S and 43.0 W), Sentinel-2 L2A tile
       23KQR, relative orbit 95, 2022 and 2023: the dates whose window is clear (3 % or less cloud, shadow, cirrus or no
       data by the SCL), on the 10 m grid of EPSG:32723 (x 704000 to 714000, y 7505000 to 7515000), for NBAR. A file a
       date in OUT/serra/series, bands named as Janus names a Sentinel-2 search's (Blue (B02) ... NIR (B08), NIR
       (narrow) (B8A), SWIR 1 (B11), SWIR 2 (B12), SCL (scene classification)), B8A, B11, B12 and the SCL from 20 m by
       bilinear (nearest for the SCL), in digital numbers with the scale and offset of the product, and what Janus reads
       of a date from its file: the STAC platform, item, time, the item's mean sun zenith and azimuth, and the tile's
       MTD_TL.xml (downloaded to OUT/serra/granules: the sun and view angles of NBAR with angles = Sentinel-2 granule
       metadata).
topo:  21 x 21 km of the same mountains (x 699015 to 720015, y 7499005 to 7520005) in Landsat 8 and 9 Collection 2
       Level-2 (path 217, row 76), 2022 and 2023, on the 30 m grid of Landsat (its pixel edges at 15 m; a nearest-neighbour
       copy, Landsat's own pixels), for the topographic correction: the dates whose window is clear (12 % or less cloud,
       shadow, cirrus or fill by QA_PIXEL; the quality band leaves those out) in OUT/topo/landsat, and in OUT/topo/series eight of them whose sun's zenith is
       spread from its lowest to its highest (copies). Landsat and not Sentinel-2: Sentinel-2 L2A of Sen2Cor is already
       corrected for the terrain with a DEM (its reflectance on a slope shows hardly any illumination), Landsat Collection
       2's LaSRC is not. Blue, Green, Red, NIR, SWIR1, SWIR2 and QA_PIXEL as the Landsat guides' files, with the sun of the
       item (view:sun_elevation and view:sun_azimuth) as the STAC_SUN_ZENITH, STAC_SUN_AZIMUTH of each file, which is how a
       series built from a catalogue search records them.
mixed: 36 x 36 km of the Luis Eduardo Magalhaes irrigation area (Bahia, about 12.15 S and 45.7 W), center pivots and fields
       with sharp edges, 2022 and 2023, on the 30 m grid of Landsat Collection 2 (x 402015 to 438015, y 8638765 to 8674765
       in EPSG:32723: the Landsat grid, which has its pixel edges at 15 m, moved by the 10 000 km of the false northing of
       the northern zone Landsat's items are in; a nearest-neighbour copy, no resampling of Landsat).
         OUT/mixed/landsat   Landsat 8 and 9 Collection 2 Level-2 dates with 0.5 % or less cloud, shadow, cirrus or fill
                             (path 220, row 68, the frame that holds the window): Blue, Green, Red, NIR, SWIR1, SWIR2
                             and QA_PIXEL, the digital numbers with scale 0.0000275 and offset -0.2 as the files of the
                             Landsat guides have them
         OUT/mixed/s2        Sentinel-2 L2A (tile 23LMG) dates with the same clearness, on the same grid: Blue (B02), Green
                             (B03), Red (B04), NIR (narrow) (B8A), SWIR 1 (B11), SWIR 2 (B12), averaged from 10 and 20 m
                             to 30 m, scale 0.0001 and the offset of the product, and QA_PIXEL made of the SCL (bit 0 no
                             data, bit 3 cloud for SCL 8, 9 and 10, bit 4 shadow for SCL 3, bit 5 snow for SCL 11)
         OUT/mixed/coreg     eight dates of the dry season of 2023 (four of each sensor, evenly through June 15 to
                             September 25), copies of the two folders' files with the Sentinel-2 bands named as
                             Landsat's so that they are one series; the Landsat date of 2023-07-18 has its pixels moved
                             by 1 pixel to the right and 1 down ON PURPOSE (copied one over, no resampling; the edge
                             that has nothing is no data and fill in the quality band), because the real shifts between
                             Landsat 8-9 and Sentinel-2 here are all under 0.15 pixel, nothing for the tool to show;
                             noted in the file's NOTE metadata item

cloudy: the Serra's 21 x 21 km of the topographic correction's window (x 699015 to 720015, y 7499005 to 7520005, the 30 m grid
        of Landsat, 700 x 700 px), Landsat 8 and 9 Collection 2 Level-2 of 2022 to 2024 (path 217, row 76) with ALL the dates
        whose frame holds the window (no fill), the cloudy ones too, in OUT/cloudy (files as topo/landsat's): the series
        Tmask, the clear-sky observations, Interpolate, Smoothing and Standardize are shown on (a humid mountain area: its
        wet season, November to March, has the clouds).
improphe: the ImproPhe figures' fine and coarse series, in OUT/improphe: fine/ is 6 x 6 km of the same area (x 421515 to 427515,
          y 8657765 to 8663765, 600 x 600 px of 10 m, EPSG:32723) in Sentinel-2 L2A (tile 23LMG) of 2022 and 2023, the
          dates with 10 % or less cloud, shadow, cirrus or no data by the SCL (Blue, Green, Red, NIR (B08), SWIR 1, SWIR 2 and
          the SCL; B11 and B12 from 20 m by bilinear); coarse/ is the same area in MODIS Terra 250 m NDVI (MOD13Q1 v061,
          Planetary Computer): ONE LAYER A YEAR, 2022 and 2023, each the mean over the year of the composites that start in it (23 a year) and the
          product's own pixel reliability (0 or 1, good or marginal) keeps, a metric made here and not a MODIS product, put
          on EPSG:32723 at 250 m by nearest neighbour (the MODIS pixels are 231.7 m in the sinusoidal grid: this is a
          resampling of the coarse data, said in the figure).

Then:  janus --threads 4 --shots dataprep OUT <shots folder>
       python tools/make_dataprep_figures.py <shots folder>
       python tools/make_webp.py

Needs the Python of the geo environment (GDAL, numpy, scipy, pystac-client, planetary-computer, pyproj) and the network;
l1c needs only Zeit's (the zeit environment).
"""

import datetime as dt
import math
import os
import shutil
import sys
import time
import urllib.request
from pathlib import Path

L1C_PRODUCT = "S2A_MSIL1C_20230717T130251_N0509_R095_T23KQR_20230717T175959.SAFE"


def l1c(out):
    """The Level 1C scene of the Level 1C tool's figures and the Copernicus GLO-30 DEM over it (Zeit's own downloaders: the
    scene from Google Cloud's public bucket, the DEM from Earth Search), in OUT/l1c, at most 4 connections."""
    import zeit

    dest = out / "l1c"
    dest.mkdir(parents=True, exist_ok=True)
    safe = zeit.download_l1c(L1C_PRODUCT, dest=dest, n_jobs=4)
    print("scene", safe)
    dem = zeit.download_dem(safe, dest=dest / "dem_23KQR.tif", n_jobs=4)
    print("dem", dem)


if __name__ == "__main__" and sys.argv[1:2] == ["l1c"]:
    # (only Zeit is needed: the Python of the zeit environment, before the imports of the others)
    l1c(Path(sys.argv[2]))
    raise SystemExit(0)


import numpy as np
import planetary_computer
import pyproj
import pystac_client
from osgeo import gdal, osr

gdal.UseExceptions()
gdal.SetConfigOption("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR")
gdal.SetConfigOption("CPL_VSIL_CURL_ALLOWED_EXTENSIONS", ".tif,.TIF")
gdal.SetConfigOption("GDAL_HTTP_MAX_RETRY", "4")
gdal.SetConfigOption("GDAL_HTTP_RETRY_DELAY", "2")
gdal.SetConfigOption("GDAL_NUM_THREADS", "2")

PC = "https://planetarycomputer.microsoft.com/api/stac/v1"
# the Serra's window: 10 m, 1000 x 1000 px
SERRA = {"epsg": 32723, "x0": 704000.0, "y_top": 7515000.0, "w": 1000, "h": 1000, "res": 10.0}
# the irrigation area's window: Landsat's grid, 30 m, 1200 x 1200 px
MIXED = {"epsg": 32723, "x0": 402015.0, "y_top": 8674765.0, "w": 1200, "h": 1200, "res": 30.0}
YEARS3 = ("2022-01-01", "2024-12-31")
# ImproPhe's fine window: Sentinel-2's own grid, 10 m, 600 x 600 px (6 x 6 km), inside the irrigation area's; its coarse
# series covers it with a margin of 2 km at 250 m
IMPRO = {"epsg": 32723, "x0": 415670.0, "y_top": 8660000.0, "w": 600, "h": 600, "res": 10.0}
# the topographic correction's window: Landsat's grid, 30 m, 700 x 700 px
TOPO = {"epsg": 32723, "x0": 699015.0, "y_top": 7520005.0, "w": 700, "h": 700, "res": 30.0}
SERRA_POINT = (-42.99, -22.50)
MIXED_POINT = (-45.735, -12.15)
S2_NAMES = ["Blue (B02)", "Green (B03)", "Red (B04)", "NIR (narrow) (B8A)", "SWIR 1 (B11)", "SWIR 2 (B12)", "QA_PIXEL"]
LANDSAT_NAMES = ["Blue", "Green", "Red", "NIR", "SWIR1", "SWIR2", "QA_PIXEL"]
# the Landsat date of the co-registration's series whose pixels are moved on purpose, and by how much (whole pixels
# to the right and down)
SHIFT_DATE, SHIFT_PX = "2023-07-18", (1, 1)
YEARS = ("2022-01-01", "2023-12-31")
COREG_SPAN = ("2023-06-15", "2023-09-25")


def client():
    # unsigned: each address is signed when it is read (a signature lasts under an hour, a run longer)
    return pystac_client.Client.open(PC)


def bounds(g):
    return (g["x0"], g["y_top"] - g["h"] * g["res"], g["x0"] + g["w"] * g["res"], g["y_top"])


def warp(href, g, alg, src_srs=None, otype=gdal.GDT_UInt16, nodata=0):
    """The band at `href` on the grid `g` (UInt16, 0 where there is nothing; `otype` and `nodata` for another)."""
    x0, y0, x1, y1 = bounds(g)
    opts = gdal.WarpOptions(
        format="MEM", outputBounds=(x0, y0, x1, y1), width=g["w"], height=g["h"], dstSRS=f"EPSG:{g['epsg']}",
        srcSRS=src_srs, resampleAlg=alg, srcNodata=nodata, dstNodata=nodata, outputType=otype,
    )
    for attempt in range(5):
        try:
            ds = gdal.Warp("", "/vsicurl/" + planetary_computer.sign(href), options=opts)
            a = ds.ReadAsArray()
            ds = None
            return a
        except RuntimeError as e:
            if attempt == 4:
                raise
            print("   retry", attempt + 1, " ".join(str(e)[:80].split()))
            time.sleep(3 * (attempt + 1))


def offset_of(item, key):
    """The reflectance offset of a Sentinel-2 asset (-0.1 from processing baseline 04.00 on), and its scale."""
    rb = item.assets[key].extra_fields.get("raster:bands", [{}])[0]
    base = float(item.properties.get("s2:processing_baseline", "0"))
    return float(rb.get("scale", 1e-4)), float(rb.get("offset", -0.1 if base >= 4.0 else 0.0))


def write(path, arrays, names, scales, g, tags, nodata=0):
    drv = gdal.GetDriverByName("GTiff")
    ds = drv.Create(str(path), g["w"], g["h"], len(arrays), gdal.GDT_UInt16,
                    ["COMPRESS=DEFLATE", "PREDICTOR=2", "TILED=YES", "INTERLEAVE=BAND"])
    ds.SetGeoTransform((g["x0"], g["res"], 0, g["y_top"], 0, -g["res"]))
    srs = osr.SpatialReference()
    srs.ImportFromEPSG(g["epsg"])
    ds.SetProjection(srs.ExportToWkt())
    for k, (a, name, sc) in enumerate(zip(arrays, names, scales)):
        b = ds.GetRasterBand(k + 1)
        b.WriteArray(a)
        b.SetDescription(name)
        b.SetNoDataValue(nodata)
        if sc:
            b.SetScale(sc[0])
            b.SetOffset(sc[1])
    for key, v in tags.items():
        ds.SetMetadataItem(key, str(v))
    ds.FlushCache()
    ds = None


def s2_items(cat, point, g, bbox_pad=0.0):
    """The Sentinel-2 L2A items of the point over the years, one for each acquisition (the newest processing)."""
    s = cat.search(collections=["sentinel-2-l2a"], intersects={"type": "Point", "coordinates": list(point)},
                   datetime=f"{YEARS[0]}/{YEARS[1]}", max_items=2000)
    best = {}
    for i in s.items():
        key = i.properties["datetime"][:19]
        base = float(i.properties.get("s2:processing_baseline", "0"))
        if key not in best or base > float(best[key].properties.get("s2:processing_baseline", "0")):
            best[key] = i
    return sorted(best.values(), key=lambda i: i.properties["datetime"])


def bad_share(scl):
    """The share of the pixels cloud, shadow, cirrus, saturated or without data (SCL 0, 1, 3, 8, 9, 10)."""
    return float(np.isin(scl, [0, 1, 3, 8, 9, 10]).mean())


def s2_tags(item, extra=None):
    p = item.properties
    t = {
        "STAC_PLATFORM": p["platform"],
        "STAC_COLLECTION": "sentinel-2-l2a",
        "STAC_ITEMS": item.id,
        "STAC_DATETIME": p["datetime"].replace("+00:00", "Z"),
        "STAC_SUN_ZENITH": p["s2:mean_solar_zenith"],
        "STAC_SUN_AZIMUTH": p["s2:mean_solar_azimuth"],
    }
    t.update(extra or {})
    return t


def serra(out):
    g = SERRA
    cat = client()
    series = out / "serra" / "series"
    gran = out / "serra" / "granules"
    for d in (series, gran):
        d.mkdir(parents=True, exist_ok=True)
    items = [i for i in s2_items(cat, SERRA_POINT, g) if i.properties["s2:mgrs_tile"] == "23KQR"]
    print(len(items), "acquisitions of tile 23KQR")
    keys = ["B02", "B03", "B04", "B08", "B8A", "B11", "B12"]
    names = ["Blue (B02)", "Green (B03)", "Red (B04)", "NIR (B08)", "NIR (narrow) (B8A)", "SWIR 1 (B11)", "SWIR 2 (B12)",
             "SCL (scene classification)"]
    kept = []
    for it in items:
        day = it.properties["datetime"][:10]
        scl = warp(it.assets["SCL"].href, g, "near")
        share = bad_share(scl)
        if share > 0.03:
            print(day, f"{share:.1%} bad: skipped")
            continue
        sensor = it.properties["platform"][-2:]
        path = series / f"S2{sensor[-1]}_{day}.tif"
        mtd = gran / f"S2{sensor[-1]}_{day}_MTD_TL.xml"
        if not mtd.exists():
            url = planetary_computer.sign(it.assets["granule-metadata"].href)
            with urllib.request.urlopen(url) as r, open(mtd, "wb") as f:
                f.write(r.read())
        if not path.exists():
            arrays, scales = [], []
            for k in keys:
                alg = "near" if k in ("B02", "B03", "B04", "B08") else "bilinear"
                arrays.append(warp(it.assets[k].href, g, alg))
                scales.append(offset_of(it, k))
            arrays.append(scl)
            scales.append(None)
            write(path, arrays, names, scales, g, s2_tags(it, {"STAC_GRANULE_METADATA": str(mtd.resolve())}))
        kept.append((day, path, float(it.properties["s2:mean_solar_zenith"]), float(it.properties["s2:mean_solar_azimuth"])))
        print(day, f"{share:.1%} bad, sun zenith {kept[-1][2]:.1f} azimuth {kept[-1][3]:.1f}: {path.name}")
    print(len(kept), "dates in", series)


def landsat_item_files(item, g, qa):
    """The Landsat item's six bands and QA_PIXEL on the grid `g`, as arrays."""
    arrays = [warp(item.assets[k].href, g, "near", src_srs=item.properties.get("proj:code"))
              for k in ["blue", "green", "red", "nir08", "swir16", "swir22"]]
    arrays.append(qa)
    return arrays


def topo(out):
    """Landsat 8 and 9 of the Serra's mountains for the topographic correction, and eight dates of them."""
    g = TOPO
    cat = client()
    folder = out / "topo" / "landsat"
    folder.mkdir(parents=True, exist_ok=True)
    s = cat.search(collections=["landsat-c2-l2"], intersects={"type": "Point", "coordinates": list(SERRA_POINT)},
                   datetime=f"{YEARS[0]}/{YEARS[1]}", query={"platform": {"in": ["landsat-8", "landsat-9"]}}, max_items=1000)
    items = sorted(s.items(), key=lambda i: i.properties["datetime"])
    print(len(items), "Landsat scenes")
    taken = {}
    for it in items:
        day = it.properties["datetime"][:10]
        if day in taken:
            continue
        try:
            qa = warp(it.assets["qa_pixel"].href, g, "near", src_srs=it.properties.get("proj:code"))
        except RuntimeError as e:
            print(day, "cannot be read:", " ".join(str(e)[:60].split()))
            continue
        share = landsat_bad(qa)
        if share > 0.12:
            print(day, it.id[-26:], f"{share:.1%} bad: skipped")
            continue
        path = folder / f"LC0{it.properties['platform'][-1]}_{day}.tif"
        if not path.exists():
            write(path, landsat_item_files(it, g, qa), LANDSAT_NAMES, [(2.75e-05, -0.2)] * 6 + [None], g, landsat_tags(it))
        taken[day] = path
        print(day, it.id, f"{share:.2%} bad -> {path.name}")
    print(len(taken), "dates in", folder)
    choose_topo(out)


def choose_topo(out):
    """The eight dates of the topographic correction: the sun's zenith spread evenly from the lowest to the highest
    sun of the dates of OUT/topo/landsat (the nearest date to each of eight evenly spaced zeniths); the files are copies."""
    series, chosen_dir = out / "topo" / "landsat", out / "topo" / "series"
    chosen_dir.mkdir(parents=True, exist_ok=True)
    for p in chosen_dir.glob("*.tif"):
        p.unlink()
    kept = []
    for f in sorted(series.glob("*.tif")):
        ds = gdal.Open(str(f))
        kept.append((f.stem.split("_")[1], f, float(ds.GetMetadataItem("STAC_SUN_ZENITH")), float(ds.GetMetadataItem("STAC_SUN_AZIMUTH"))))
        ds = None
    lo, hi = min(k[2] for k in kept), max(k[2] for k in kept)
    chosen = []
    for q in range(8):
        w = lo + q * (hi - lo) / 7
        chosen.append(min((k for k in kept if k not in chosen), key=lambda k: abs(k[2] - w)))
    for day, path, z, a in sorted(chosen):
        shutil.copyfile(path, chosen_dir / path.name)
        print("topo", day, f"zenith {z:.1f} azimuth {a:.1f}")


def read_dem(g, cat):
    """The Copernicus DEM GLO-30 of the grid `g` (bilinear, metres): what Janus' own download gives, for choosing pixels."""
    x0, y0, x1, y1 = bounds(g)
    t = pyproj.Transformer.from_crs(g["epsg"], 4326, always_xy=True)
    lon0, lat0 = t.transform(x0, y0)
    lon1, lat1 = t.transform(x1, y1)
    items = list(cat.search(collections=["cop-dem-glo-30"], bbox=[lon0 - 0.01, lat0 - 0.01, lon1 + 0.01, lat1 + 0.01]).items())
    hrefs = ["/vsicurl/" + planetary_computer.sign(i.assets["data"].href) for i in items]
    opts = gdal.WarpOptions(format="MEM", outputBounds=(x0, y0, x1, y1), width=g["w"], height=g["h"],
                            dstSRS=f"EPSG:{g['epsg']}", resampleAlg="bilinear", outputType=gdal.GDT_Float32)
    ds = gdal.Warp("", hrefs, options=opts)
    return ds.ReadAsArray()


def slope_aspect(dem, res):
    """Slope and aspect (radians; the aspect clockwise from north, the direction the slope faces) by Horn's method."""
    p = np.pad(dem, 1, mode="edge")
    a, b, c = p[:-2, :-2], p[:-2, 1:-1], p[:-2, 2:]
    d, f = p[1:-1, :-2], p[1:-1, 2:]
    gg, h, i = p[2:, :-2], p[2:, 1:-1], p[2:, 2:]
    dzdx = ((c + 2 * f + i) - (a + 2 * d + gg)) / (8 * res)
    dzdy = ((gg + 2 * h + i) - (a + 2 * b + c)) / (8 * res)  # rows go south
    slope = np.arctan(np.hypot(dzdx, dzdy))
    aspect = np.arctan2(-dzdx, dzdy) % (2 * np.pi)  # facing east: dzdx < 0
    return slope, aspect


def pins(out):
    """Candidate pixels for the figures of the Serra: print them, with the numbers that make them. The tour keeps the
    choice in crates/janus-app/src/shots_dataprep.rs. First the flat forest of the NBAR chart (Sentinel-2 series, 10 m),
    then the sunlit and the shaded forest pixels of the topographic correction's chart (Landsat, 30 m)."""
    from scipy import ndimage
    from scipy.spatial import cKDTree

    cat = client()

    def read(files, nir_b, red_b, qa_b, bad):
        nir, red, days, zen, azi = [], [], [], [], []
        for f in files:
            ds = gdal.Open(str(f))
            ok = ~bad(ds.GetRasterBand(qa_b).ReadAsArray())
            refl = lambda b: ds.GetRasterBand(b).ReadAsArray().astype(np.float32) * ds.GetRasterBand(b).GetScale() + ds.GetRasterBand(b).GetOffset()
            nir.append(np.where(ok, refl(nir_b), np.nan))
            red.append(np.where(ok, refl(red_b), np.nan))
            days.append(f.stem.split("_")[1])
            zen.append(float(ds.GetMetadataItem("STAC_SUN_ZENITH")))
            azi.append(float(ds.GetMetadataItem("STAC_SUN_AZIMUTH")))
        return np.array(nir), np.array(red), days, zen, azi

    def uniform(a, size):
        mean = ndimage.uniform_filter(np.nan_to_num(a), size)
        sq = ndimage.uniform_filter(np.nan_to_num(a) ** 2, size)
        return np.sqrt(np.maximum(sq - mean**2, 0)) / np.maximum(mean, 1e-3)

    with np.errstate(all="ignore"):
        # the flat forest (S2)
        g = SERRA
        dem = read_dem(g, cat)
        slope, aspect = slope_aspect(dem, g["res"])
        sl = np.degrees(slope)
        files = sorted((out / "serra" / "series").glob("*.tif"))
        nir, red, days, zen, azi = read(files, 4, 3, 8, lambda scl: np.isin(scl, [0, 1, 3, 8, 9, 10]))
        ndvi = (nir - red) / (nir + red)
        med = np.nanmedian(ndvi, axis=0)
        w = days.index("2023-06-27")
        cv = uniform(nir[w], 7)
        flat = (sl < 2.0) & (cv < 0.08) & np.isfinite(med)
        ys, xs = np.where(flat)
        print("S2:", len(days), "dates; flat candidates", len(xs))
        for j in np.argsort(-np.nan_to_num(med[ys, xs]))[:6]:
            x, y = xs[j], ys[j]
            print(f"flat forest ({x}, {y}): NDVI median {med[y, x]:.2f}, NIR median {np.nanmedian(nir[:, y, x]):.3f} std {np.nanstd(nir[:, y, x]):.3f}, valid dates {int(np.isfinite(nir[:, y, x]).sum())}, height {dem[y, x]:.0f} m")
        # the Landsat dates of the topographic correction
        g = TOPO
        dem = read_dem(g, cat)
        slope, aspect = slope_aspect(dem, g["res"])
        sl = np.degrees(slope)
        files = sorted((out / "topo" / "series").glob("*.tif"))
        nir, red, days, zen, azi = read(files, 4, 3, 7, lambda qa: (qa & 0b111111) != 0)
        ndvi = (nir - red) / (nir + red)
        print("Landsat:", days, [round(z, 1) for z in zen])
        high = [i for i, z in enumerate(zen) if z < 32]
        forest = np.nanmedian(ndvi[high], axis=0) > 0.85
        print("forest pixels", int(forest.sum()), "of", forest.size, "; DEM", float(np.nanmin(dem)), "to", float(np.nanmax(dem)), "m")
        for i in np.argsort(zen):
            z, a = np.radians(zen[i]), np.radians(azi[i])
            cosi = np.cos(z) * np.cos(slope) + np.sin(z) * np.sin(slope) * np.cos(a - aspect)
            sun = forest & (sl > 15) & (cosi > 0.85) & np.isfinite(nir[i])
            shade = forest & (sl > 15) & (cosi < 0.35) & np.isfinite(nir[i])
            print(f"  {days[i]} zenith {zen[i]:.1f} azimuth {azi[i]:.0f}: forest NIR on slopes over 15 deg, cos i over 0.85 median {np.nanmedian(nir[i][sun]):.3f} (n {int(sun.sum())}), under 0.35 {np.nanmedian(nir[i][shade]):.3f} (n {int(shade.sum())})")
        i = int(np.argmax(zen))
        z, a = np.radians(zen[i]), np.radians(azi[i])
        cosi = np.cos(z) * np.cos(slope) + np.sin(z) * np.sin(slope) * np.cos(a - aspect)
        cvn = uniform(nir[i], 3)
        stable = forest & (np.nanpercentile(ndvi, 10, axis=0) > 0.70) & (cvn < 0.12) & np.isfinite(nir[i])
        sun = stable & (sl > 15) & (sl < 35) & (cosi > 0.85)
        shade = stable & (sl > 15) & (sl < 35) & (cosi < 0.35)
        ms, mh = np.nanmedian(nir[i][sun]), np.nanmedian(nir[i][shade])
        print(f"{days[i]} (lowest sun): candidate sunlit {int(sun.sum())} (NIR median {ms:.3f}), shaded {int(shade.sum())} ({mh:.3f})")
        ys, xs = np.where(sun & (np.abs(nir[i] / ms - 1) < 0.05))
        ys2, xs2 = np.where(shade & (np.abs(nir[i] / mh - 1) < 0.05))
        if len(xs) and len(xs2):
            d, k = cKDTree(np.c_[xs2, ys2]).query(np.c_[xs, ys])
            for j in np.argsort(d)[:6]:
                p, q = (int(xs[j]), int(ys[j])), (int(xs2[k[j]]), int(ys2[k[j]]))
                print(f"pair: sunlit {p} slope {sl[p[1], p[0]]:.0f} cos i {cosi[p[1], p[0]]:.2f} NIR {nir[i][p[1], p[0]]:.3f} | shaded {q} slope {sl[q[1], q[0]]:.0f} cos i {cosi[q[1], q[0]]:.2f} NIR {nir[i][q[1], q[0]]:.3f} | {d[j] * g['res']:.0f} m apart, dz {abs(dem[p[1], p[0]] - dem[q[1], q[0]]):.0f} m")


def mixed_pins(out):
    """The stable pixel of the harmonization's chart: evergreen forest (median NDVI over 0.8 on both sensors' dates) in
    uniform 5 x 5 pixels, the one whose NDVI varies least over the dates of both sensors; and what the two sensors say
    there (mean of each band over the dates, the mean difference of the nearest pairs of dates, at most 3 days apart)."""
    from scipy import ndimage

    base = out / "mixed"

    def load(folder):
        arrs, days = [], []
        for f in sorted((base / folder).glob("*.tif")):
            ds = gdal.Open(str(f))
            qa = ds.GetRasterBand(7).ReadAsArray()
            ok = (qa & 0b111111) == 0
            bands = []
            for b in range(1, 7):
                rb = ds.GetRasterBand(b)
                a = rb.ReadAsArray().astype(np.float32) * rb.GetScale() + rb.GetOffset()
                bands.append(np.where(ok & (rb.ReadAsArray() > 0), a, np.nan))
            arrs.append(np.array(bands))
            days.append(f.stem.split("_")[1])
        return np.array(arrs), days

    s2, d2 = load("s2")
    ls, dl = load("landsat")
    print("Sentinel-2", s2.shape, "Landsat", ls.shape)
    nd = lambda a: (a[:, 3] - a[:, 2]) / (a[:, 3] + a[:, 2])
    n2, nl = nd(s2), nd(ls)
    both = np.concatenate([n2, nl])
    med = np.nanmedian(both, axis=0)
    sd = np.nanstd(both, axis=0)
    # uniform: the coefficient of variation of the median NIR in 5 x 5
    nir = np.nanmedian(np.concatenate([s2[:, 3], ls[:, 3]]), axis=0)
    mean = ndimage.uniform_filter(np.nan_to_num(nir), 5)
    sq = ndimage.uniform_filter(np.nan_to_num(nir) ** 2, 5)
    cv = np.sqrt(np.maximum(sq - mean**2, 0)) / np.maximum(mean, 1e-3)
    valid = np.isfinite(n2).sum(0) >= 0.8 * len(d2)
    valid &= np.isfinite(nl).sum(0) >= 0.8 * len(dl)
    cand = (med > 0.8) & (cv < 0.05) & valid
    ys, xs = np.where(cand)
    print("candidates", len(xs))
    order = np.argsort(sd[ys, xs])[:6]
    day = lambda d: dt.date.fromisoformat(d).toordinal()
    for j in order:
        x, y = int(xs[j]), int(ys[j])
        print(f"pixel ({x}, {y}): median NDVI {med[y, x]:.3f}, std {sd[y, x]:.3f}, valid S2 {int(np.isfinite(n2[:, y, x]).sum())} Landsat {int(np.isfinite(nl[:, y, x]).sum())}")
    x, y = int(xs[order[0]]), int(ys[order[0]])
    names = ["blue", "green", "red", "nir", "swir1", "swir2"]
    for b, name in enumerate(names):
        a, c = s2[:, b, y, x], ls[:, b, y, x]
        diffs = []
        for i, d in enumerate(d2):
            if not np.isfinite(a[i]):
                continue
            k = [(abs(day(d) - day(e)), m) for m, e in enumerate(dl) if np.isfinite(c[m])]
            if not k:
                continue
            gap, m = min(k)
            if gap <= 3:
                diffs.append(a[i] - c[m])
        print(f"  {name}: S2 mean {np.nanmean(a):.4f} Landsat mean {np.nanmean(c):.4f}, S2 - Landsat of {len(diffs)} pairs: mean {np.mean(diffs) if diffs else float('nan'):+.4f} ({100 * np.mean(diffs) / np.nanmean(c) if diffs else float('nan'):+.1f} % of Landsat's)")


def landsat_tags(item):
    p = item.properties
    return {
        "STAC_PLATFORM": p["platform"],
        "STAC_COLLECTION": "landsat-c2-l2",
        "STAC_ITEMS": item.id,
        "STAC_DATETIME": p["datetime"].replace("+00:00", "Z"),
        "STAC_SUN_ZENITH": 90.0 - p["view:sun_elevation"],
        "STAC_SUN_AZIMUTH": p["view:sun_azimuth"],
    }


def qa_from_scl(scl):
    """A Landsat Collection 2 QA_PIXEL made of the SCL: fill (bit 0) for no data, cloud (bit 3) for SCL 8, 9 and 10
    (and the cirrus bit 2 of 10), shadow (bit 4) for 3, snow (bit 5) for 11."""
    qa = np.zeros(scl.shape, np.uint16)
    qa |= np.where(scl == 0, 1, 0).astype(np.uint16)
    qa |= np.where(np.isin(scl, [8, 9, 10]), 1 << 3, 0).astype(np.uint16)
    qa |= np.where(scl == 10, 1 << 2, 0).astype(np.uint16)
    qa |= np.where(scl == 3, 1 << 4, 0).astype(np.uint16)
    qa |= np.where(scl == 11, 1 << 5, 0).astype(np.uint16)
    return qa


def landsat_bad(qa):
    """Fill, dilated cloud, cirrus, cloud, shadow, snow (bits 0 to 5): the share of such pixels."""
    return float(((qa & 0b111111) != 0).mean())


def mixed(out):
    g = MIXED
    cat = client()
    base = out / "mixed"
    for d in ("landsat", "s2", "coreg"):
        (base / d).mkdir(parents=True, exist_ok=True)
    # Landsat 8 and 9
    s = cat.search(collections=["landsat-c2-l2"], intersects={"type": "Point", "coordinates": list(MIXED_POINT)},
                   datetime=f"{YEARS[0]}/{YEARS[1]}", query={"platform": {"in": ["landsat-8", "landsat-9"]}}, max_items=1000)
    ls = sorted(s.items(), key=lambda i: i.properties["datetime"])
    print(len(ls), "Landsat scenes")
    lkeys = ["blue", "green", "red", "nir08", "swir16", "swir22", "qa_pixel"]
    lnames = LANDSAT_NAMES
    taken = {}
    for it in ls:
        day = it.properties["datetime"][:10]
        if day in taken:
            continue
        have = sorted((base / "landsat").glob(f"*_{day}.tif"))
        if have:
            taken[day] = have[0]
            continue
        try:
            qa = warp(it.assets["qa_pixel"].href, g, "near", src_srs=it.properties.get("proj:code"))
        except RuntimeError as e:
            print(day, it.id[-26:], "cannot be read:", " ".join(str(e)[:60].split()))
            continue
        # a frame that does not hold the window has fill in it
        share = landsat_bad(qa)
        if share > 0.005:
            print(day, it.id[-26:], f"{share:.1%} bad: skipped")
            continue
        sensor = it.properties["platform"][-1]
        path = base / "landsat" / f"LC0{sensor}_{day}.tif"
        if not path.exists():
            arrays = []
            for k in lkeys[:-1]:
                arrays.append(warp(it.assets[k].href, g, "near", src_srs=it.properties.get("proj:code")))
            arrays.append(qa)
            scales = [(2.75e-05, -0.2)] * 6 + [None]
            write(path, arrays, lnames, scales, g, landsat_tags(it))
        taken[day] = path
        print(day, it.id, f"{share:.2%} bad -> {path.name}")
    print(len(taken), "Landsat dates in", base / "landsat")
    # Sentinel-2
    items = [i for i in s2_items(cat, MIXED_POINT, g) if i.properties["s2:mgrs_tile"] == "23LMG"]
    print(len(items), "Sentinel-2 acquisitions of tile 23LMG")
    skeys = ["B02", "B03", "B04", "B8A", "B11", "B12"]
    s2taken = {}
    for it in items:
        day = it.properties["datetime"][:10]
        have = sorted((base / "s2").glob(f"*_{day}.tif"))
        if have:
            s2taken[day] = have[0]
            describe(have[0], S2_NAMES)
            continue
        try:
            scl = warp(it.assets["SCL"].href, g, "near")
        except RuntimeError as e:
            print(day, it.id[-26:], "cannot be read:", " ".join(str(e)[:60].split()))
            continue
        qa = qa_from_scl(scl)
        share = bad_share(scl)
        if share > 0.005:
            print(day, f"{share:.1%} bad: skipped")
            continue
        sensor = it.properties["platform"][-1]
        path = base / "s2" / f"S2{sensor}_{day}.tif"
        if not path.exists():
            arrays, scales = [], []
            for k in skeys:
                arrays.append(warp(it.assets[k].href, g, "average"))
                scales.append(offset_of(it, k))
            arrays.append(qa)
            scales.append(None)
            write(path, arrays, S2_NAMES, scales, g, s2_tags(it))
        s2taken[day] = path
        print(day, it.id, f"{share:.2%} bad -> {path.name}")
    print(len(s2taken), "Sentinel-2 dates in", base / "s2")
    for path in s2taken.values():
        to_reflectance(path)
    # the co-registration's eight dates: four of each sensor, evenly through the dry season of 2023
    for p in list((base / "coreg").glob("*.tif")):
        p.unlink()
    n = 0
    for folder, taken_days in (("landsat", taken), ("s2", s2taken)):
        days = sorted(d for d in taken_days if COREG_SPAN[0] <= d <= COREG_SPAN[1])
        pick = [days[round(k * (len(days) - 1) / 3)] for k in range(4)] if len(days) >= 4 else days
        for day in pick:
            dest = base / "coreg" / taken_days[day].name
            shutil.copyfile(taken_days[day], dest)
            n += 1
            if folder == "s2":
                describe(dest, LANDSAT_NAMES)
            if folder == "landsat" and day == SHIFT_DATE:
                move_pixels(dest, SHIFT_PX)
            print("coreg", folder, day)
    print(n, "files in", base / "coreg")


def to_reflectance(path):
    """The Sentinel-2 file's bands as float32 reflectance (the scale and offset applied, no scale tags, NaN where there is
    no value; the quality band as it is, as float32): Janus' Harmonize guesses the units of a series from its values
    and takes a UInt16 Sentinel-2 file with a scale and an offset for reflectance times 10 000 while it reads it as
    reflectance, so the file is written in the units it is read in."""
    src = gdal.Open(str(path))
    if src.GetRasterBand(1).DataType == gdal.GDT_Float32:
        return
    n = src.RasterCount
    arrays, names, tags = [], [], src.GetMetadata()
    for b in range(1, n + 1):
        rb = src.GetRasterBand(b)
        dn = rb.ReadAsArray()
        names.append(rb.GetDescription())
        if rb.GetScale() is None:
            arrays.append(dn.astype(np.float32))
        else:
            arrays.append(np.where(dn == 0, np.nan, dn * rb.GetScale() + rb.GetOffset()).astype(np.float32))
    gt, wkt = src.GetGeoTransform(), src.GetProjection()
    w, h = src.RasterXSize, src.RasterYSize
    src = None
    tmp = Path(str(path) + ".tmp")
    ds = gdal.GetDriverByName("GTiff").Create(str(tmp), w, h, n, gdal.GDT_Float32,
                                              ["COMPRESS=DEFLATE", "PREDICTOR=3", "TILED=YES", "INTERLEAVE=BAND"])
    ds.SetGeoTransform(gt)
    ds.SetProjection(wkt)
    for b, (a, name) in enumerate(zip(arrays, names), 1):
        rb = ds.GetRasterBand(b)
        rb.WriteArray(a)
        rb.SetDescription(name)
        if name != "QA_PIXEL":
            rb.SetNoDataValue(float("nan"))
    for k, v in tags.items():
        ds.SetMetadataItem(k, v)
    ds.FlushCache()
    ds = None
    tmp.replace(path)


def describe(path, names):
    """The band descriptions of a file (only when they differ)."""
    ds = gdal.Open(str(path), gdal.GA_Update)
    if [ds.GetRasterBand(b + 1).GetDescription() for b in range(len(names))] != names:
        for b, name in enumerate(names):
            ds.GetRasterBand(b + 1).SetDescription(name)
    ds.FlushCache()
    ds = None


def move_pixels(path, px):
    """The file's pixels moved `px` = (x, y) whole pixels to the right and down (a scene registered a pixel off), each
    band copied one over; what the move leaves without a value is 0 (the no data) and, in the quality band, the fill bit.
    Said in the file."""
    dx, dy = px
    ds = gdal.Open(str(path), gdal.GA_Update)
    for b in range(1, ds.RasterCount + 1):
        rb = ds.GetRasterBand(b)
        a = rb.ReadAsArray()
        h, w = a.shape
        out = np.zeros_like(a)
        out[dy:, dx:] = a[: h - dy, : w - dx]
        if rb.GetDescription() == "QA_PIXEL":
            out[:dy, :] = 1
            out[:, :dx] = 1
        rb.WriteArray(out)
    ds.SetMetadataItem("NOTE", f"pixels moved by {dx} pixel(s) to the right and {dy} down on purpose (tools/make_dataprep_data.py)")
    ds.FlushCache()
    ds = None


def cloudy(out):
    """Landsat 8 and 9 of the topographic correction's window, every date whose frame holds the window (no fill), the
    cloudy ones too, 2022 to 2024."""
    g = TOPO
    cat = client()
    base = out / "cloudy"
    base.mkdir(parents=True, exist_ok=True)
    s = cat.search(collections=["landsat-c2-l2"], intersects={"type": "Point", "coordinates": list(SERRA_POINT)},
                   datetime=f"{YEARS3[0]}/{YEARS3[1]}", query={"platform": {"in": ["landsat-8", "landsat-9"]}}, max_items=2000)
    items = sorted(s.items(), key=lambda i: i.properties["datetime"])
    print(len(items), "Landsat scenes")
    lkeys = ["blue", "green", "red", "nir08", "swir16", "swir22"]
    taken = {}
    for it in items:
        day = it.properties["datetime"][:10]
        if day in taken:
            continue
        have = sorted(base.glob(f"*_{day}.tif"))
        if have:
            taken[day] = have[0]
            continue
        try:
            qa = warp(it.assets["qa_pixel"].href, g, "near", src_srs=it.properties.get("proj:code"))
        except RuntimeError as e:
            print(day, it.id[-26:], "cannot be read:", " ".join(str(e)[:60].split()))
            continue
        fill = float(((qa & 1) != 0).mean())
        if fill > 0.005:
            print(day, it.id[-26:], f"{fill:.1%} fill: not this frame")
            continue
        path = base / f"LC0{it.properties['platform'][-1]}_{day}.tif"
        try:
            arrays = [warp(it.assets[k].href, g, "near", src_srs=it.properties.get("proj:code")) for k in lkeys] + [qa]
        except RuntimeError as e:
            print(day, it.id[-26:], "a band cannot be read:", " ".join(str(e)[:60].split()))
            continue
        write(path, arrays, LANDSAT_NAMES, [(2.75e-05, -0.2)] * 6 + [None], g, landsat_tags(it))
        taken[day] = path
        print(day, it.id, f"{landsat_bad(qa):.1%} cloud, shadow, cirrus or fill -> {path.name}")
    print(len(taken), "dates in", base)


def improphe(out):
    """ImproPhe's two series: Sentinel-2 at 10 m of the IMPRO window (the clear dates of 2022 and 2023) and the coarse
    series, a layer a year of MODIS Terra's 250 m NDVI (the mean over the year of the composites whose pixel reliability
    is good or marginal) on the same CRS at 250 m by nearest neighbour."""
    g = IMPRO
    cat = client()
    fine = out / "improphe" / "fine"
    coarse = out / "improphe" / "coarse"
    for d in (fine, coarse):
        d.mkdir(parents=True, exist_ok=True)
    items = [i for i in s2_items(cat, MIXED_POINT, g) if i.properties["s2:mgrs_tile"] == "23LMG"]
    print(len(items), "Sentinel-2 acquisitions of tile 23LMG")
    keys = ["B02", "B03", "B04", "B08", "B11", "B12"]
    names = ["Blue (B02)", "Green (B03)", "Red (B04)", "NIR (B08)", "SWIR 1 (B11)", "SWIR 2 (B12)", "SCL (scene classification)"]
    kept = 0
    for it in items:
        day = it.properties["datetime"][:10]
        path = fine / f"S2{it.properties['platform'][-1]}_{day}.tif"
        if path.exists():
            kept += 1
            continue
        try:
            scl = warp(it.assets["SCL"].href, g, "near")
        except RuntimeError as e:
            print(day, "cannot be read:", " ".join(str(e)[:60].split()))
            continue
        share = bad_share(scl)
        if share > 0.10:
            print(day, f"{share:.1%} bad: skipped")
            continue
        arrays, scales = [], []
        for k in keys:
            arrays.append(warp(it.assets[k].href, g, "near" if k in ("B02", "B03", "B04", "B08") else "bilinear"))
            scales.append(offset_of(it, k))
        arrays.append(scl)
        scales.append(None)
        write(path, arrays, names, scales, g, s2_tags(it))
        kept += 1
        print(day, f"{share:.1%} bad -> {path.name}")
    print(kept, "Sentinel-2 dates in", fine)
    # MODIS: the window with 2 km around it, on the 250 m grid of UTM 23S
    x0, y0, x1, y1 = bounds(g)
    cg = {"epsg": g["epsg"], "x0": math.floor((x0 - 2000) / 250) * 250.0, "res": 250.0}
    cg["y_top"] = math.ceil((y1 + 2000) / 250) * 250.0
    cg["w"] = int(math.ceil((x1 + 2000) / 250) * 250 - cg["x0"]) // 250
    cg["h"] = int(cg["y_top"] - math.floor((y0 - 2000) / 250) * 250) // 250
    print("coarse grid", cg)
    for year in (2022, 2023):
        path = coarse / f"MOD13Q1_{year}-07-02.tif"
        if path.exists():
            continue
        s = cat.search(collections=["modis-13Q1-061"], intersects={"type": "Point", "coordinates": list(MIXED_POINT)},
                       datetime=f"{year}-01-01/{year}-12-31", max_items=200)
        # the composites that start in the year (the first of the year's items starts in December), once each (the newest
        # processing of a composite is the last of its id)
        by_start = {}
        for i in sorted((i for i in s.items() if i.id.startswith(f"MOD13Q1.A{year}")), key=lambda i: i.id):
            by_start[i.id.split(".")[1]] = i
        mods = [by_start[k] for k in sorted(by_start)]
        total = np.zeros((cg["h"], cg["w"]), np.float64)
        count = np.zeros((cg["h"], cg["w"]), np.int32)
        for it in mods:
            # (Int16: the NDVI's fill is -3000 and the reliability's -1; the files' own no-data values are not set here)
            nd = warp(it.assets["250m_16_days_NDVI"].href, cg, "near", otype=gdal.GDT_Int16, nodata=-3000)
            rel = warp(it.assets["250m_16_days_pixel_reliability"].href, cg, "near", otype=gdal.GDT_Int16, nodata=-1)
            ok = (rel >= 0) & (rel <= 1) & (nd != -3000)
            print("  ", it.id, "pixels kept", round(float(ok.mean()), 3))
            total += np.where(ok, nd * 1e-4, 0.0)
            count += ok
        print(year, len(mods), "composites")
        mean = np.where(count > 0, total / np.maximum(count, 1), np.nan).astype(np.float32)
        drv = gdal.GetDriverByName("GTiff")
        ds = drv.Create(str(path), cg["w"], cg["h"], 1, gdal.GDT_Float32, ["COMPRESS=DEFLATE", "TILED=NO"])
        ds.SetGeoTransform((cg["x0"], cg["res"], 0, cg["y_top"], 0, -cg["res"]))
        srs = osr.SpatialReference()
        srs.ImportFromEPSG(cg["epsg"])
        ds.SetProjection(srs.ExportToWkt())
        b = ds.GetRasterBand(1)
        b.WriteArray(mean)
        b.SetDescription(f"NDVI, mean of the year {year}")
        b.SetNoDataValue(float("nan"))
        ds.SetMetadataItem("STAC_DATETIME", f"{year}-07-02T00:00:00Z")
        ds.SetMetadataItem("NOTE", f"mean over {year} of the MOD13Q1 v061 250 m NDVI composites (pixel reliability 0 or 1), "
                           f"{len(mods)} composites, by nearest neighbour on EPSG:32723 at 250 m; made by tools/make_dataprep_data.py")
        ds.FlushCache()
        ds = None
        print("wrote", path)


def main():
    a = sys.argv[1:]
    cmds = {"serra": serra, "mixed": mixed, "pins": pins, "topo": topo, "mixed-pins": mixed_pins, "cloudy": cloudy, "improphe": improphe}
    if len(a) != 2 or a[0] not in cmds:
        raise SystemExit(__doc__)
    cmds[a[0]](Path(a[1]))


if __name__ == "__main__":
    main()
