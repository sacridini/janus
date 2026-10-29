"""Janus's Sentinel-1 path (IDEIAS phase 47) against stackstac + numpy, rasterio + numpy, and scipy's speckle filters.

    python tools/bench_s1_vs_stackstac.py     (the geo env: pystac-client, planetary-computer, stackstac, dask,
                                               rasterio, scipy; a connection to Planetary Computer)

Planetary Computer's sentinel-1-rtc over the STAC guide's square of Rondonia (BOX; its pastures drying out in the
2024 dry season), 2024, the pass with the most
scenes. Janus (crates/janus-zeit/examples/vs_s1.rs, 8 threads): the search, a VRT a day (VV, VH at 10 m on the
scenes' own grid) and every day read whole as VV in dB (the VVdB index, 10 log10 VV). stackstac: the same items
searched and signed, stacked on Janus's grid (its bounds, 10 m, nearest, not snapped), each day's scenes mosaicked
in Janus's order (the last on top), 10 log10 by numpy, computed by dask on 8 threads. rasterio: one day's scene
read on the same window. The values compared pixel by pixel.

Speckle: Janus's linear VV and VH written locally (OUT/linear) and filtered by Janus (Quegan, Lee with 4.4 looks,
7 x 7, 8 threads) against the same formulas through scipy.ndimage.uniform_filter (the windows' sums over the pixels
with a value: mode "constant" on the values and on their mask), numpy on one thread.
Writes bench/s1-vs-stackstac.json.
"""
import json
import os
import subprocess
import time
from pathlib import Path

os.environ.setdefault("GDAL_NUM_THREADS", "8")
os.environ.setdefault("OMP_NUM_THREADS", "8")
import dask
import numpy as np
import planetary_computer
import pystac_client
import rasterio
import stackstac
from rasterio.windows import from_bounds
from scipy.ndimage import uniform_filter

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_s1.exe"
OUT = ROOT / "target" / "bench-s1"
BOX = os.environ.get("S1_BOX", "-63.00,-10.00,-62.92,-9.92")
START, END = os.environ.get("S1_START", "2024-01-01"), os.environ.get("S1_END", "2024-12-31")
SIZE, LOOKS = 7, 4.4


def janus():
    r = subprocess.run([str(EXE), str(OUT), BOX, START, END, "8"], capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(r.stderr)
    return json.loads(r.stdout)


def compare(a, b):
    both = np.isfinite(a) & np.isfinite(b)
    d = np.abs(a[both] - b[both])
    return {"pixels": int(a.size), "both_valued": int(both.sum()),
            "valued_in_one_only": int((np.isfinite(a) ^ np.isfinite(b)).sum()),
            "max_abs_diff": float(d.max()) if d.size else None,
            "differing_over_1e-4": int((d > 1e-4).sum())}


def box_sums(x, size):
    """Each pixel's window sums over the pixels with a value (the window cut by the edges), and their count."""
    valid = np.isfinite(x)
    x0 = np.where(valid, x, 0).astype("float64")
    k = size * size
    s = uniform_filter(x0, size, mode="constant") * k
    q = uniform_filter(x0 * x0, size, mode="constant") * k
    c = np.rint(uniform_filter(valid.astype("float64"), size, mode="constant") * k)
    return s, q, c


def lee(x, size, looks):
    s, q, c = box_sums(x, size)
    with np.errstate(invalid="ignore", divide="ignore"):
        m = s / c
        v = np.maximum(q / c - m * m, 0)
        w = np.where(v > 0, np.maximum(1 - m * m / (looks * v), 0), 0)
        out = m + w * (x - m)
    return np.where(np.isfinite(x) & (c > 0), out, np.nan).astype("float32")


def quegan(stack, size):
    """stack: dates x rows x columns of one band."""
    means = []
    for x in stack:
        s, _, c = box_sums(x, size)
        with np.errstate(invalid="ignore", divide="ignore"):
            means.append(s / c)
    means = np.array(means)
    ok = np.isfinite(stack) & (means > 0)
    with np.errstate(invalid="ignore", divide="ignore"):
        ratio = np.where(ok, stack / means, 0).sum(axis=0)
        n = ok.sum(axis=0)
        out = means * ratio / n
    return np.where(np.isfinite(stack) & (n > 0), out, np.nan).astype("float32")


def main():
    res = {"box": BOX, "dates": [START, END]}
    # Janus: twice, the second timed (the scenes' headers in GDAL's cache as stackstac's are in its)
    janus()
    r = janus()
    w, h, gt = r["w"], r["h"], r["gt"]
    days = r["dates"]
    jdb = np.fromfile(OUT / "janus_vvdb.f32", dtype="<f4").reshape(len(days), h, w)
    res["janus"] = {k: r[k] for k in ["search_s", "build_s", "read_db_s", "quegan_s", "lee_s", "items_found", "pass",
                                      "passes", "crs", "w", "h", "band_names"]}
    res["janus"]["dates"] = len(days)
    print("janus", json.dumps(res["janus"]))
    bounds = (gt[0], gt[3] + h * gt[5], gt[0] + w * gt[1], gt[3])
    epsg = int(r["crs"].split(":")[1])
    # stackstac + numpy
    t = time.perf_counter()
    cat = pystac_client.Client.open("https://planetarycomputer.microsoft.com/api/stac/v1",
                                    modifier=planetary_computer.sign_inplace)
    b = [float(v) for v in BOX.split(",")]
    items = {i.id: i for i in cat.search(collections=["sentinel-1-rtc"], bbox=b, datetime=f"{START}/{END}").items()}
    order = [i for d in days for i in d["items"]]
    st = stackstac.stack([items[i] for i in order], assets=["vv"], epsg=epsg, resolution=10, bounds=bounds,
                         snap_bounds=False, resampling=rasterio.enums.Resampling.nearest, dtype="float32",
                         fill_value=np.float32(np.nan), rescale=False, chunksize=2048)
    with dask.config.set(scheduler="threads", num_workers=8):
        arr = st.sel(band="vv").values
    k = 0
    sdb = np.full((len(days), h, w), np.nan, dtype="float32")
    for di, d in enumerate(days):
        for _ in d["items"]:
            sdb[di] = np.where(np.isfinite(arr[k]), arr[k], sdb[di])
            k += 1
    with np.errstate(invalid="ignore", divide="ignore"):
        sdb = (10 * np.log10(sdb)).astype("float32")
    res["stackstac"] = {"seconds": time.perf_counter() - t, "grid": [int(st.sizes["y"]), int(st.sizes["x"])],
                        **compare(jdb, sdb)}
    print("stackstac", json.dumps(res["stackstac"]))
    # rasterio + numpy: the first day with one scene
    di = next(i for i, d in enumerate(days) if len(d["items"]) == 1)
    it = items[days[di]["items"][0]]
    t = time.perf_counter()
    with rasterio.open(it.assets["vv"].href) as src:
        win = from_bounds(*bounds, transform=src.transform)
        v = src.read(1, window=win, out_shape=(h, w), masked=True).filled(np.nan).astype("float32")
    with np.errstate(invalid="ignore", divide="ignore"):
        rdb = (10 * np.log10(v)).astype("float32")
    res["rasterio"] = {"day": days[di]["day"], "seconds": time.perf_counter() - t, **compare(jdb[di], rdb)}
    print("rasterio", json.dumps(res["rasterio"]))
    # speckle: Janus's local linear values through scipy
    files = sorted((OUT / "linear").glob("s1_*.tif"))
    lin = np.array([rasterio.open(f).read() for f in files], dtype="float32")  # dates x 2 x rows x columns
    spk = {}
    for name in ["quegan", "lee"]:
        t = time.perf_counter()
        if name == "lee":
            ref = np.array([[lee(lin[ti, bi], SIZE, LOOKS) for bi in range(2)] for ti in range(len(files))])
        else:
            ref = np.stack([quegan(lin[:, bi], SIZE) for bi in range(2)], axis=1)
        sec = time.perf_counter() - t
        jan = np.array([rasterio.open(f).read() for f in sorted((OUT / name).glob("f_*.tif"))], dtype="float32")
        both = np.isfinite(ref) & np.isfinite(jan)
        rel = np.abs(jan[both] - ref[both]) / np.maximum(np.abs(ref[both]), 1e-12)
        spk[name] = {"janus_s": r[f"{name}_s"], "scipy_s": sec, "dates": len(files), "grid": [h, w],
                     "valued_in_one_only": int((np.isfinite(ref) ^ np.isfinite(jan)).sum()),
                     "max_rel_diff": float(rel.max()), "median_rel_diff": float(np.median(rel)),
                     "cv_before": float(np.nanstd(lin[:, 0]) / np.nanmean(lin[:, 0])),
                     "cv_after": float(np.nanstd(jan[:, 0]) / np.nanmean(jan[:, 0]))}
        print(name, json.dumps(spk[name]))
    res["speckle"] = spk
    # the dry season: VH in dB after Quegan's filter, the first six dates' median against the last six's; the
    # largest patches that fell more than 3 dB, where they are and their series
    from scipy.ndimage import label
    from pyproj import Transformer
    qf = sorted((OUT / "quegan").glob("f_*.tif"))
    with rasterio.open(qf[0]) as src0:
        tf, crs = src0.transform, src0.crs
    vh = np.array([rasterio.open(f).read(2) for f in qf], dtype="float32")
    with np.errstate(invalid="ignore", divide="ignore"):
        vh = 10 * np.log10(np.where(vh > 0, vh, np.nan))
    drop = np.nanmedian(vh[:6], axis=0) - np.nanmedian(vh[-6:], axis=0)
    lab, _ = label(drop > 3)
    sizes = np.bincount(lab.ravel())[1:]
    to_ll = Transformer.from_crs(crs, "EPSG:4326", always_xy=True)
    patches = []
    for k in np.argsort(sizes)[::-1][:5]:
        ys, xs = np.nonzero(lab == k + 1)
        lon, lat = to_ll.transform(*(tf * (xs.mean(), ys.mean())))
        patches.append({"ha": float(sizes[k]) / 100, "lon": round(lon, 4), "lat": round(lat, 4),
                        "vh_db": [round(float(np.nanmedian(v[lab == k + 1])), 1) for v in vh]})
    res["change"] = {"vh_db_drop_over_3db_share": float(np.nanmean(drop > 3)),
                     "dates": [f.stem[2:] for f in qf], "largest_patches": patches}
    print("change", json.dumps(res["change"]))
    p = ROOT / "bench" / "s1-vs-stackstac.json"
    p.write_text(json.dumps(res, indent=2))
    print("written", p)


if __name__ == "__main__":
    main()
