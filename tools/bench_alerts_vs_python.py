"""Janus's official alerts against a map of change (Validation > Alerts) against Python: the same numbers, timed.

    python tools/bench_alerts_vs_python.py [--map CHANGE.tif] [--out bench/alerts-vs-python.json]

(an environment with numpy, scipy, rasterio, geopandas, pyogrio, requests and Pillow; the conda env `geo`.)
Janus's side is crates/janus-validation/examples/vs_python_alerts.rs (build it first: cargo build --release -p
janus-validation --example vs_python_alerts). Python does what janus_validation::alerts does, with the tools one
would use: rasterio.features.rasterize for the polygons (pixel centres, the latest alert burned first so the
earliest holds a pixel), rasterio.vrt.WarpedVRT (nearest, the exact transformer: tolerance=1e-9) for the rasters,
numpy for the decoding and the counts, scipy.ndimage.label for the patches (8-connected), numpy.percentile (linear)
for the lags.

Cases:
- synthetic (made here, seeded): a map of change dates of 3000 x 3000 pixels of 30 m, 20000 dated polygons of
  two classes (a GeoPackage) and a GLAD-S2 pair (alert, alertDate) at 10 m over a third of it;
- with --map (a Janus run's map of change dates, e.g. CCDC's first break over the s2ro series, Rondonia): INPE's
  DETER (Amazon) of its area fetched by Janus (WFS), UMD's GLAD-S2 and GLAD-L clipped from their public GeoTIFFs
  (keyless, read over the network once, then local), GFW's RADD tiles of its area decoded by Janus into a GeoTIFF
  and, independently, here from the same PNG tiles (checked equal).
Each case timed RUNS times (the median): the map read, the alerts placed on its grid, the comparison (no network).
Writes bench/alerts-vs-python.json.
"""
import argparse
import io
import json
import math
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

for k in ("GDAL_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "NUMEXPR_NUM_THREADS"):
    os.environ.setdefault(k, "2")

import geopandas as gpd
import numpy as np
import rasterio
import rasterio.features
import rasterio.warp
import rasterio.windows
from rasterio.vrt import WarpedVRT
import requests
from PIL import Image
from rasterio.crs import CRS
from rasterio.transform import Affine
from scipy import ndimage
from shapely.geometry import box

ROOT = Path(__file__).resolve().parents[1]
NAME = "vs_python_alerts.exe" if os.name == "nt" else "vs_python_alerts"
EXE = next((p for p in (ROOT / "target" / d / "examples" / NAME for d in ("release", "debug")) if p.exists()),
           ROOT / "target" / "release" / "examples" / NAME)
# Janus's own GDAL (staged next to janus.exe), never the environment's
ENV = dict(os.environ, PATH=str(EXE.parents[1]) + os.pathsep + os.environ.get("PATH", ""),
           PROJ_DATA=str(EXE.parents[1] / "share" / "proj"), GDAL_DRIVER_PATH=str(EXE.parents[1] / "gdalplugins"))
RUNS = 3
NO_DAY = np.iinfo(np.int32).min
DETER = "https://terrabrasilis.dpi.inpe.br/geoserver/deter-amz/ows"
GLAD = "https://storage.googleapis.com/earthenginepartners-hansen"
EPOCH = np.datetime64("1970-01-01", "D")


def day(s):
    return int((np.datetime64(s, "D") - EPOCH).astype(int))


def ymd(d):
    return str(EPOCH + np.timedelta64(int(d), "D"))


def janus(*args):
    r = subprocess.run([str(EXE), *map(str, args)], capture_output=True, text=True, env=ENV)
    out = r.stdout.strip().splitlines()
    if r.returncode != 0 or not out:
        raise RuntimeError(f"{EXE.name} {args[0]}: {r.stdout} {r.stderr}")
    return json.loads(out[-1])


# --- what janus_validation::alerts does -------------------------------------------------------------------------

def year_start(y):
    return (np.asarray(y, dtype=np.int64) - 1970).astype("datetime64[Y]").astype("datetime64[D]").astype(np.int64)


def days_of_decimal_years(v):
    """Janus's decimal years to days, to the nearest day (none: not finite or <= 0)."""
    v = v.astype(np.float64)
    ok = np.isfinite(v) & (v > 0)
    y = np.floor(np.where(ok, v, 2000.0)).astype(np.int64)
    n = year_start(y + 1) - year_start(y)
    d = year_start(y) + np.floor((np.where(ok, v, 2000.0) - y) * n + 0.5).astype(np.int64)
    return np.where(ok, d, NO_DAY).astype(np.int64)


def row_areas(src):
    """Each row's pixel area (ha): the sphere's (authalic radius) in a geographic CRS."""
    t = src.transform
    if src.crs.is_geographic:
        r, rad = 6371007.181, math.pi / 180
        tops = (t.f + t.e * np.arange(src.height)) * rad
        bottoms = (t.f + t.e * (np.arange(src.height) + 1)) * rad
        return r * r * abs(t.a) * rad * np.abs(np.sin(tops) - np.sin(bottoms)) / 1e4
    f = src.crs.linear_units_factor[1]
    return np.full(src.height, abs(t.a * t.e - t.b * t.d) * f * f / 1e4)


def read_map(path):
    with rasterio.open(path) as src:
        v = src.read(1).astype(np.float32)
        if src.nodata is not None and not math.isnan(src.nodata):
            v[v == np.float32(src.nodata)] = np.nan
        return v, src.transform, src.crs, row_areas(src), (src.width, src.height)


def alerts_from_vector(path, layer, date_field, class_field, transform, crs, shape):
    df = gpd.read_file(path, layer=layer, fid_as_index=True)
    dates = np.array([str(x)[:10].replace("/", "-") if x is not None and str(x) not in ("NaT", "None", "nan") else ""
                      for x in df[date_field]])
    ok = (dates != "") & df.geometry.notna().values
    df, dates = df[ok], dates[ok]
    df = df.to_crs(crs)
    days = np.array([day(d) for d in dates], dtype=np.int64)
    names = df[class_field].fillna("").astype(str).values if class_field else np.array([""] * len(df))
    classes = sorted(set(names)) or [""]
    cls = np.array([classes.index(n) for n in names], dtype=np.int64)
    fids = df.index.values.astype(np.int64)
    order = sorted(range(len(df)), key=lambda k: (-days[k], -fids[k]))
    geoms = df.geometry.values
    burned = rasterio.features.rasterize(((geoms[k], k + 1) for k in order), out_shape=shape, transform=transform,
                                         fill=0, dtype="int32", all_touched=False)
    unit = burned.astype(np.int64)
    has = unit > 0
    pday = np.where(has, days[np.maximum(unit - 1, 0)], NO_DAY)
    pcls = np.where(has, cls[np.maximum(unit - 1, 0)], 0)
    names = [c if c else "(no class)" for c in classes]
    return dict(day=pday, cls=pcls, unit=unit, covered=None, classes=names, unit_day=days, unit_cls=cls, fids=fids)


PRODUCTS = {
    "glad-l": ["probable", "confirmed"],
    "glad-s2": ["most recent observation only", "low", "medium", "high"],
    "radd": ["low", "high"],
    "integrated": ["low", "high", "highest"],
}


def decode(product, conf, date, year):
    """GLAD-L: alert 2|3, date the day of the year; GLAD-S2: alert 1-4, days since 2018-12-31; GFW's date_conf:
    confidence * 10000 + days since 2014-12-31."""
    ok = np.isfinite(conf) & (conf > 0)
    c = np.where(ok, conf, 0).astype(np.int64)
    if product == "glad-l":
        d = np.where(np.isfinite(date), date, 0).astype(np.int64)
        n = 366 if (year % 4 == 0 and year % 100 != 0) or year % 400 == 0 else 365
        ok &= (c >= 2) & (c <= 3) & (d >= 1) & (d <= n)
        return np.where(ok, year_start(year) + d - 1, NO_DAY), np.where(ok, c - 2, 0)
    if product == "glad-s2":
        d = np.where(np.isfinite(date), date, 0).astype(np.int64)
        ok &= (c >= 1) & (c <= 4) & (d >= 1)
        return np.where(ok, day("2018-12-31") + d, NO_DAY), np.where(ok, c - 1, 0)
    hi = 3 if product == "radd" else 4
    k, d = c // 10000, c % 10000
    ok &= (k >= 2) & (k <= hi) & (d >= 1)
    return np.where(ok, day("2014-12-31") + d, NO_DAY), np.where(ok, k - 2, 0)


def warp(path, transform, crs, shape):
    # the exact transformer (gdalwarp -et 0, as Janus; rasterio refuses tolerance=0, 1e-9 is every pixel): the
    # default approximate one (0.125 pixel) is interpolated over the warp's chunks, so its nearest neighbour at a source pixel's edge depends on how it is read
    # (outside the source or on its nodata: the alpha band's 0, NaN here)
    with rasterio.open(path) as src, WarpedVRT(src, crs=crs, transform=transform, width=shape[1], height=shape[0],
                                                resampling=rasterio.warp.Resampling.nearest, tolerance=1e-9,
                                                add_alpha=True) as vrt:
        v = vrt.read(1).astype(np.float32)
        return np.where(vrt.read(vrt.count) > 0, v, np.float32(np.nan))


def alerts_from_rasters(product, files, transform, crs, shape):
    pday = np.full(shape, NO_DAY, dtype=np.int64)
    pcls = np.zeros(shape, dtype=np.int64)
    covered = np.zeros(shape, dtype=bool)
    for f in files:
        conf = warp(f[0], transform, crs, shape)
        date = warp(f[1], transform, crs, shape) if len(f) > 1 and f[1] else np.full(shape, np.nan, np.float32)
        covered |= np.isfinite(conf)
        d, c = decode(product, conf, date, f[2] if len(f) > 2 else 0)
        take = (d != NO_DAY) & ((pday == NO_DAY) | (d < pday))
        pday = np.where(take, d, pday)
        pcls = np.where(take, c, pcls)
    return dict(day=pday, cls=pcls, unit=None, covered=covered, classes=PRODUCTS[product])


def quantile(v, p):
    return float(np.percentile(v, 100 * p)) if len(v) else None


def compare(change, rows_ha, a, d0, d1, bin_days):
    h, w = change.shape
    area = np.broadcast_to(rows_ha[:, None], (h, w))
    covered = a["covered"] if a["covered"] is not None else np.ones((h, w), dtype=bool)
    mday = days_of_decimal_years(change)
    m = covered & (mday != NO_DAY) & (mday >= d0) & (mday <= d1)
    al = covered & (a["day"] != NO_DAY) & (a["day"] >= d0) & (a["day"] <= d1)
    both, om, oa = m & al, m & ~al, ~m & al
    lags = (mday[both] - a["day"][both]).astype(np.float64)
    s = {}
    s["covered_pixels"] = int(covered.sum())
    s["covered_ha"] = float(area[covered].sum())
    s["both_pixels"], s["only_map_pixels"], s["only_alerts_pixels"] = int(both.sum()), int(om.sum()), int(oa.sum())
    s["both_ha"], s["only_map_ha"], s["only_alerts_ha"] = (float(area[x].sum()) for x in (both, om, oa))
    s["map_ha"] = s["both_ha"] + s["only_map_ha"]
    s["alerts_ha"] = s["both_ha"] + s["only_alerts_ha"]
    div = lambda a_, b_: a_ / b_ if b_ else None  # noqa: E731
    s["alerts_detected_share_by_pixels"] = div(s["both_ha"], s["alerts_ha"])
    s["map_confirmed_share_by_pixels"] = div(s["both_ha"], s["map_ha"])
    s["dice"] = div(2 * s["both_ha"], s["map_ha"] + s["alerts_ha"])
    eight = np.ones((3, 3), dtype=int)
    # the alerts: polygons, or the raster's patches (their earliest date, highest confidence)
    if a["unit"] is not None:
        unit = np.where(al, a["unit"], 0)
        n_units = len(a["unit_day"])
        ucls = a["unit_cls"]
        in_win = (a["unit_day"] >= d0) & (a["unit_day"] <= d1)
    else:
        unit, n_units = ndimage.label(al, structure=eight)
        flat = unit.ravel()
        idx = flat > 0
        ucls = np.zeros(n_units, dtype=np.int64)
        np.maximum.at(ucls, flat[idx] - 1, a["cls"].ravel()[idx])
        in_win = np.ones(n_units, dtype=bool)
    flat = unit.ravel()
    idx = flat > 0
    k = flat[idx] - 1
    pixels = np.bincount(k, minlength=n_units)
    det = np.bincount(k, weights=m.ravel()[idx], minlength=n_units)
    ha = np.bincount(k, weights=area.ravel()[idx], minlength=n_units)
    det_ha = np.bincount(k, weights=(area * m).ravel()[idx], minlength=n_units)
    have = pixels > 0
    s["alerts"] = int(have.sum())
    s["alerts_detected"] = int((det[have] > 0).sum())
    s["alerts_detected_share_by_count"] = div(s["alerts_detected"], s["alerts"])
    s["alerts_detected_share_by_area"] = div(float(ha[have & (det > 0)].sum()), float(ha[have].sum()))
    s["alerts_too_small"] = int((in_win & ~have).sum()) if a["unit"] is not None else 0
    lab, n_patches = ndimage.label(m, structure=eight)
    s["map_patches"] = int(n_patches)
    s["map_patches_confirmed"] = int(len(np.unique(lab[both])))
    s["map_confirmed_share_by_count"] = div(s["map_patches_confirmed"], s["map_patches"])
    s["lag_n"] = int(len(lags))
    s["lag_mean_days"] = float(lags.mean()) if len(lags) else None
    s["lag_min_days"] = float(lags.min()) if len(lags) else None
    for name, p in (("q10", 0.1), ("q25", 0.25), ("median", 0.5), ("q75", 0.75), ("q90", 0.9)):
        s[f"lag_{name}_days"] = quantile(lags, p)
    s["lag_max_days"] = float(lags.max()) if len(lags) else None
    for dd in (30, 90, 365):
        s[f"lag_within_{dd if dd != 365 else 365}_days"] = float((np.abs(lags) <= dd).mean()) if len(lags) else None
    bins, counts = np.unique(np.floor_divide(lags.astype(np.int64), bin_days), return_counts=True)
    hist = [[int(b * bin_days), int((b + 1) * bin_days), int(n)] for b, n in zip(bins, counts)]
    # by class (the alerts' class), the lags of their detected pixels
    unit_of_px = np.where(unit > 0, unit - 1, 0)
    px_cls = ucls[unit_of_px] if n_units else np.zeros_like(unit)
    classes = []
    for c, name in enumerate(a["classes"]):
        sel = have & (ucls == c)
        lag_c = (mday - a["day"])[both & (unit > 0) & (px_cls == c)].astype(np.float64)
        classes.append(dict(class_=name, alerts=int(sel.sum()), detected=int((det[sel] > 0).sum()),
                            ha=float(ha[sel].sum()), detected_ha=float(det_ha[sel].sum()),
                            lag_median=quantile(lag_c, 0.5)))
    return s, classes, hist


def python_case(map_path, alerts_fn, d0, d1, bin_days):
    times, out = [], None
    for _ in range(RUNS):
        t = time.perf_counter()
        change, transform, crs, rows, (w, h) = read_map(map_path)
        a = alerts_fn(transform, crs, (h, w))
        out = compare(change, rows, a, d0, d1, bin_days)
        times.append(time.perf_counter() - t)
    return out, statistics.median(times), times


def same(a, b):
    if a is None or b is None:
        return a is None and b is None
    return abs(a - b) <= 1e-9 * max(1.0, abs(a), abs(b))


def check(janus_out, py):
    s, classes, hist = py
    diff = {k: (janus_out["summary"].get(k), v) for k, v in s.items() if not same(janus_out["summary"].get(k), v)}
    cd = []
    for jc, pc in zip(janus_out["classes"], classes):
        for k in ("alerts", "detected", "ha", "detected_ha", "lag_median"):
            if not same(jc[k], pc[k]):
                cd.append((pc["class_"], k, jc[k], pc[k]))
    if len(janus_out["classes"]) != len(classes):
        cd.append(("count", len(janus_out["classes"]), len(classes)))
    return dict(summary_differs=diff, classes_differ=cd, histogram_same=janus_out["histogram"] == hist,
                same=not diff and not cd and janus_out["histogram"] == hist)


# --- inputs ---------------------------------------------------------------------------------------------------

def synthetic(d):
    """A map of 3000 x 3000 pixels of 30 m (UTM 20S), 20000 polygons, a GLAD-S2 pair at 10 m over a third."""
    rng = np.random.default_rng(42)
    w = h = 3000
    t = Affine(30.0, 0, 500000.0, 0, -30.0, 9000000.0)
    crs = CRS.from_epsg(32720)
    # patches of change: 4000 squares of 3-40 pixels, dates 2020-2023 as decimal years
    change = np.full((h, w), np.nan, dtype=np.float32)
    for _ in range(4000):
        x, y, s = rng.integers(0, w - 40), rng.integers(0, h - 40), rng.integers(3, 40)
        change[y:y + s, x:x + s] = np.float32(2020 + rng.random() * 4)
    mp = d / "change.tif"
    with rasterio.open(mp, "w", driver="GTiff", width=w, height=h, count=1, dtype="float32", crs=crs, transform=t,
                       nodata=np.nan, compress="deflate", tiled=True) as dst:
        dst.write(change, 1)
    # alerts: polygons near half the patches (shifted), the others alone
    geoms, dates, classes = [], [], []
    for k in range(20000):
        x0 = 500000 + rng.random() * (w - 50) * 30
        y0 = 9000000 - rng.random() * (h - 50) * 30
        s = 60 + rng.random() * 900
        geoms.append(box(x0, y0 - s, x0 + s * (0.5 + rng.random()), y0))
        dates.append(ymd(day("2019-06-01") + int(rng.integers(0, 1700))))
        classes.append("DESMATAMENTO_CR" if rng.random() < 0.7 else "DEGRADACAO")
    gdf = gpd.GeoDataFrame({"view_date": dates, "classname": classes}, geometry=geoms, crs=crs)
    vp = d / "polygons.gpkg"
    if vp.exists():
        vp.unlink()
    gdf.to_file(vp, layer="alerts", driver="GPKG")
    # GLAD-S2 at 10 m over the top third: alerts where the map changed (and a few elsewhere), dates near its own
    fw, fh = 3 * w, h
    ft = Affine(10.0, 0, 500000.0, 0, -10.0, 9000000.0)
    up = np.repeat(np.repeat(change[: h // 3], 3, axis=0), 3, axis=1)
    noise = rng.random(up.shape) < 0.002
    alert = np.where(np.isfinite(up) | noise, rng.integers(1, 5, up.shape), 0).astype(np.uint8)
    base = np.where(np.isfinite(up), up, 2021.5)
    days = ((base - 2019) * 365.25 + rng.integers(-60, 60, up.shape)).clip(1, 3000)
    alert_date = np.where(alert > 0, days, 0).astype(np.uint16)
    paths = []
    for name, arr in (("alert", alert), ("alertDate", alert_date)):
        p = d / f"{name}.tif"
        with rasterio.open(p, "w", driver="GTiff", width=fw, height=fh // 3, count=1, dtype=arr.dtype, crs=crs,
                           transform=ft, compress="lzw") as dst:
            dst.write(arr, 1)
        paths.append(p)
    return mp, vp, paths


def glad_tiles(b):
    """GLAD's 10-degree tiles of a box: xmin_ymin_xmax_ymax, e.g. 070W_20S_060W_10S."""
    ew = lambda v: f"{abs(v):03d}{'W' if v < 0 else 'E'}"  # noqa: E731
    ns = lambda v: f"{abs(v):02d}{'S' if v < 0 else 'N'}"  # noqa: E731
    out = []
    for y in range(math.floor(b[1] / 10) * 10, math.ceil(b[3] / 10) * 10, 10):
        for x in range(math.floor(b[0] / 10) * 10, math.ceil(b[2] / 10) * 10, 10):
            out.append(f"{ew(x)}_{ns(y)}_{ew(x + 10)}_{ns(y + 10)}")
    return out


def clip_remote(url, bounds, out, margin=0.002):
    """A window of a public GeoTIFF (its own grid) saved locally; False when the box is off it."""
    with rasterio.Env(GDAL_DISABLE_READDIR_ON_OPEN="EMPTY_DIR", GDAL_HTTP_TIMEOUT="120"):
        with rasterio.open(url) as src:
            win = rasterio.windows.from_bounds(bounds[0] - margin, bounds[1] - margin, bounds[2] + margin,
                                               bounds[3] + margin, src.transform).round_offsets().round_lengths()
            win = win.intersection(rasterio.windows.Window(0, 0, src.width, src.height))
            if win.width <= 0 or win.height <= 0:
                return False
            arr = src.read(1, window=win)
            prof = src.profile.copy()
            prof.update(width=arr.shape[1], height=arr.shape[0], transform=src.window_transform(win),
                        compress="lzw", tiled=True, blockxsize=256, blockysize=256)
            prof.pop("nodata", None)
            with rasterio.open(out, "w", **prof) as dst:
                dst.write(arr, 1)
    return True


def gfw_python(bounds, z=14, dataset="wur_radd_alerts"):
    """GFW's tiles of a box decoded here (gfw-data-api's symbology: day = R * 255 + G since 2014-12-31,
    confidence B // 100 - 1): the date_conf values of the mosaic."""
    n = 2 ** z
    tx = lambda lon: int(min(max(math.floor((lon + 180) / 360 * n), 0), n - 1))  # noqa: E731
    ty = lambda lat: int(min(max(math.floor((1 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2 * n), 0),  # noqa: E731
                             n - 1))
    x0, x1, y0, y1 = tx(bounds[0]), tx(bounds[2]), ty(bounds[3]), ty(bounds[1])
    out = np.zeros(((y1 - y0 + 1) * 256, (x1 - x0 + 1) * 256), dtype=np.float32)
    for y in range(y0, y1 + 1):
        for x in range(x0, x1 + 1):
            r = requests.get(f"https://tiles.globalforestwatch.org/{dataset}/latest/default/{z}/{x}/{y}.png",
                             timeout=60, headers={"User-Agent": "Janus benchmark (github.com/sacridini/janus)"})
            if r.status_code == 404:
                continue
            r.raise_for_status()
            a = np.array(Image.open(io.BytesIO(r.content)).convert("RGBA")).astype(np.int64)
            d = a[..., 0] * 255 + a[..., 1]
            c = 2 + np.clip(a[..., 2] // 100 - 1, 0, 1)
            v = np.where((d > 0) & (d < 10000), c * 10000 + d, 0)
            out[(y - y0) * 256:(y - y0 + 1) * 256, (x - x0) * 256:(x - x0 + 1) * 256] = v
    return out


def map_bounds(path):
    with rasterio.open(path) as src:
        return rasterio.warp.transform_bounds(src.crs, "EPSG:4326", *src.bounds, densify_pts=21)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--map", help="a Janus run's map of change dates (decimal years)")
    ap.add_argument("--from", dest="d0", default="2019-06-01")
    ap.add_argument("--to", dest="d1", default="2024-09-30")
    ap.add_argument("--out", default=str(ROOT / "bench" / "alerts-vs-python.json"))
    args = ap.parse_args()
    if not EXE.exists():
        sys.exit(f"build {EXE} first")
    res = {"runs": RUNS, "janus": str(EXE.name), "python": sys.version.split()[0],
           "numpy": np.__version__, "rasterio": rasterio.__version__, "geopandas": gpd.__version__,
           "gdal_python": rasterio.__gdal_version__, "cases": {}}
    work = Path(tempfile.mkdtemp(prefix="janus_alerts_bench_"))
    bin_days = 30

    def run_case(name, map_path, janus_args, alerts_fn, d0, d1):
        print(f"{name}...", flush=True)
        j = janus(*janus_args)
        py, secs, ts = python_case(map_path, alerts_fn, day(d0), day(d1), bin_days)
        c = check(j, py)
        res["cases"][name] = dict(window=[d0, d1], grid=j["grid"], janus_seconds=j["seconds"], janus_runs=j["runs"],
                                  python_seconds=secs, python_runs=ts, summary=j["summary"], classes=j["classes"],
                                  **c)
        s = j["summary"]
        print(f"  same: {c['same']}; both {s['both_pixels']}, only map {s['only_map_pixels']}, only alerts "
              f"{s['only_alerts_pixels']}; {s['alerts_detected']} of {s['alerts']} alerts; janus {j['seconds']:.3f} s,"
              f" python {secs:.3f} s", flush=True)
        if not c["same"]:
            print("  differs:", c["summary_differs"], c["classes_differ"][:5], flush=True)

    # synthetic
    mp, vp, (ga, gd) = synthetic(work)
    d0, d1 = "2019-01-01", "2024-12-31"
    run_case("synthetic_polygons", mp,
             ["vector", mp, vp, "alerts", "view_date", "classname", d0, d1, bin_days, RUNS],
             lambda t, c, s: alerts_from_vector(vp, "alerts", "view_date", "classname", t, c, s), d0, d1)
    run_case("synthetic_glad_s2", mp, ["raster", mp, "glad-s2", d0, d1, bin_days, RUNS, f"{ga},{gd}"],
             lambda t, c, s: alerts_from_rasters("glad-s2", [(str(ga), str(gd))], t, c, s), d0, d1)
    if args.map:
        m = args.map
        b = map_bounds(m)
        bb = ",".join(f"{v:.6f}" for v in b)
        d0, d1 = args.d0, args.d1
        # DETER fetched once by Janus (the same GeoPackage for both)
        deter = work / "deter.gpkg"
        f = janus("deter", DETER, "deter-amz:deter_amz", bb, d0, d1, deter)
        res["deter_fetch"] = f
        run_case("deter_amazon", m, ["vector", m, deter, f["layer"], "view_date", "classname", d0, d1, bin_days, RUNS],
                 lambda t, c, s: alerts_from_vector(deter, f["layer"], "view_date", "classname", t, c, s), d0, d1)
        # GLAD-S2 and GLAD-L clipped from UMD's public files (keyless)
        s2, gl = [], []
        for tile in glad_tiles(b):
            pa, pd = work / f"s2_alert_{tile}.tif", work / f"s2_alertDate_{tile}.tif"
            if clip_remote(f"/vsicurl/{GLAD}/S2alert/alert/{tile}.tif", b, pa) and                     clip_remote(f"/vsicurl/{GLAD}/S2alert/alertDate/{tile}.tif", b, pd):
                s2.append((str(pa), str(pd)))
            for y in range(max(2021, int(d0[:4])), int(d1[:4]) + 1):
                pa, pd = work / f"gl_alert{y}_{tile}.tif", work / f"gl_alertDate{y}_{tile}.tif"
                if clip_remote(f"/vsicurl/{GLAD}/GLADalert/C2/current/alert{y % 100:02d}_{tile}.tif", b, pa) and                         clip_remote(f"/vsicurl/{GLAD}/GLADalert/C2/current/alertDate{y % 100:02d}_{tile}.tif", b, pd):
                    gl.append((str(pa), str(pd), y))
        run_case("glad_s2", m, ["raster", m, "glad-s2", d0, d1, bin_days, RUNS, *[f"{x},{y}" for x, y in s2]],
                 lambda t, c, s: alerts_from_rasters("glad-s2", s2, t, c, s), d0, d1)
        run_case("glad_l", m, ["raster", m, "glad-l", d0, d1, bin_days, RUNS, *[f"{a},{d},{y}" for a, d, y in gl]],
                 lambda t, c, s: alerts_from_rasters("glad-l", gl, t, c, s), d0, d1)
        # RADD: GFW's tiles decoded by Janus, and here from the same PNGs
        radd = work / "radd.tif"
        g = janus("gfw-mosaic", "radd", bb, radd)
        with rasterio.open(radd) as src:
            mine = src.read(1)
        theirs = gfw_python(b)
        res["radd_tiles"] = dict(janus=g, same_values=bool(np.array_equal(np.nan_to_num(mine), theirs)),
                                 alert_pixels=int((theirs > 0).sum()))
        print(f"radd tiles decoded the same: {res['radd_tiles']['same_values']}", flush=True)
        run_case("radd", m, ["raster", m, "radd", d0, d1, bin_days, RUNS, f"{radd}"],
                 lambda t, c, s: alerts_from_rasters("radd", [(str(radd),)], t, c, s), d0, d1)
    Path(args.out).write_text(json.dumps(res, indent=2, default=float), encoding="utf-8")
    print(f"written {args.out}")


if __name__ == "__main__":
    main()
