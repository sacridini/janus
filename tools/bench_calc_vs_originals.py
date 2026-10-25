"""The raster calculator (janus_cube::calc) against its originals, in result and in time:

- the Awesome Spectral Indices a six-band layer (Blue..SWIR2 with Landsat's wavelengths) can compute, Janus's
  compiled formulas against spyndex's (the same catalogue, numpy) over 2048 x 2048 random reflectances;
- the reductions over time of a series of 48 dates of 1024 x 1024 with gaps (max, mean, median, std, a count,
  the 90th percentile, a date's departure from the mean) through the calculator's VRTs against xarray (files read
  with rasterio, reduced with skipna), each timed from the files to the image;
- the overview of a series of 24 dates of four bands read as NDVI by a normalized difference and by an expression
  (and EVI), Janus only: what an expression costs on the map.

    python tools/bench_calc_vs_originals.py [DIR]      (numpy, xarray, rasterio, spyndex, GDAL's Python bindings)

Writes bench/calc-vs-originals.json.
"""
import json
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np
import rasterio
import spyndex
import xarray as xr
from osgeo import gdal

ROOT = Path(__file__).resolve().parent.parent
EXE = ROOT / "target" / "release" / "examples" / "vs_originals_calc.exe"
N_SIDE = 2048
NM = {"B": 482.0, "G": 561.0, "R": 655.0, "N": 865.0, "S1": 1609.0, "S2": 2201.0}


def best(fn, repeat=3):
    fn()
    out = []
    for _ in range(repeat):
        t = time.perf_counter()
        fn()
        out.append(time.perf_counter() - t)
    return min(out) * 1000


def janus(*args):
    subprocess.run([str(EXE), *map(str, args)], check=True)


def write_tif(path, a, nodata=np.nan, names=None):
    h, w = a.shape[-2:]
    k = 1 if a.ndim == 2 else a.shape[0]
    d = gdal.GetDriverByName("GTiff").Create(str(path), w, h, k, gdal.GDT_Float32, ["TILED=YES", "COMPRESS=NONE"])
    d.SetGeoTransform([500000, 10, 0, 8900000, 0, -10])
    d.SetProjection("EPSG:32720")
    for b in range(k):
        band = d.GetRasterBand(b + 1)
        band.WriteArray(a if a.ndim == 2 else a[b])
        band.SetNoDataValue(nodata)
        if names:
            band.SetDescription(names[b])
    d = None


def indices(d):
    n = N_SIDE * N_SIDE
    rng = np.random.default_rng(1)
    bands = rng.uniform(0.01, 0.6, (6, n)).astype(np.float32)
    bands.tofile(d / "bands.f32")
    janus("indices", d, n)
    listed = json.loads((d / "indices.json").read_text())
    roles = ["B", "G", "R", "N", "S1", "S2"]
    rows, worst = [], []
    for ix in listed:
        name = ix["name"]
        si = spyndex.indices[name]
        params = {}
        for b in si.bands:
            if b in NM:
                params[b] = bands[roles.index(b)]
            elif b.startswith("lambda"):
                params[b] = NM.get(b[6:], np.nan)
            elif b in spyndex.constants:
                params[b] = spyndex.constants[b].default
        if any(isinstance(v, float) and np.isnan(v) for v in params.values()):
            continue  # a wavelength of a band the layer lacks: Janus takes the role's centre, spyndex has none
        try:
            with np.errstate(all="ignore"):
                ref = np.asarray(spyndex.computeIndex(name, params=params), dtype=np.float64)
        except Exception as e:  # noqa: BLE001
            print(f"  {name}: spyndex failed ({e})")
            continue
        with np.errstate(all="ignore"):
            t_ref = best(lambda: spyndex.computeIndex(name, params=params), 2)
        got = np.fromfile(d / ix["file"], dtype=np.float32).astype(np.float64)
        ref = np.where(np.isfinite(ref), ref, np.nan)
        both = np.isfinite(ref) & np.isfinite(got)
        nan_same = float(np.mean(np.isfinite(ref) == np.isfinite(got)))
        scale = np.maximum(1.0, np.abs(ref[both]))
        err = float(np.max(np.abs(got[both] - ref[both]) / scale)) if both.any() else 0.0
        rows.append({"name": name, "rel_err": err, "nan_agree": nan_same, "spyndex_ms": t_ref,
                     "janus_ms": ix["janus_ms"]})
        worst.append((err, name))
    worst.sort(reverse=True)
    t_s = sum(r["spyndex_ms"] for r in rows)
    t_j = sum(r["janus_ms"] for r in rows)
    agree = min(r["nan_agree"] for r in rows)
    print(f"indices: {len(rows)} compared over {N_SIDE} x {N_SIDE} px; largest relative difference "
          f"{worst[0][0]:.2e} ({worst[0][1]}), the same no data at {agree:.6f} of the pixels at least")
    print(f"  all of them: spyndex {t_s / 1000:.2f} s, Janus {t_j / 1000:.2f} s (1 thread), x{t_s / t_j:.1f}")
    for r in sorted(rows, key=lambda r: -r["rel_err"])[:5]:
        print(f"  {r['name']:<12} rel {r['rel_err']:.2e}  no data agree {r['nan_agree']:.6f}")
    return {"compared": len(rows), "max_rel_err": worst[0][0], "worst": worst[0][1], "nan_agree_min": agree,
            "spyndex_s": t_s / 1000, "janus_s": t_j / 1000, "rows": rows}


def reductions(d):
    s = d / "series"
    s.mkdir(exist_ok=True)
    rng = np.random.default_rng(2)
    t_n, side = 48, 1024
    for t in range(t_n):
        a = (0.5 + 0.3 * np.sin(2 * np.pi * t / 12) + rng.normal(0, 0.1, (side, side))).astype(np.float32)
        a[rng.random((side, side)) < 0.2] = np.nan  # clouds: 20% of the pixels a date
        write_tif(s / f"s_{2020 + t // 12}{t % 12 + 1:02d}15.tif", a)
    janus("reduce", d)
    jt = json.loads((d / "reduce.json").read_text())
    files = sorted(s.glob("*.tif"))

    def load():
        arr = np.stack([rasterio.open(f).read(1) for f in files])
        return xr.DataArray(arr, dims=("time", "y", "x"))

    ops = {
        "max": lambda da: da.max("time", skipna=True),
        "mean": lambda da: da.mean("time", skipna=True),
        "median": lambda da: da.median("time", skipna=True),
        "std": lambda da: da.std("time", skipna=True),
        "count": lambda da: (da > 0.5).sum("time"),
        "p90": lambda da: da.quantile(0.9, "time", skipna=True),
        "anomaly_last": lambda da: (da - da.mean("time", skipna=True)).isel(time=-1),
    }
    out = {}
    for name, op in ops.items():
        res = {}
        t_x = best(lambda: res.__setitem__("v", op(load()).values), 2)
        ref = np.asarray(res["v"], dtype=np.float64).ravel()
        got = np.fromfile(d / f"r_{name}.f32", dtype=np.float32).astype(np.float64)
        both = np.isfinite(ref) & np.isfinite(got)
        err = float(np.max(np.abs(got[both] - ref[both]))) if both.any() else 0.0
        agree = float(np.mean(np.isfinite(ref) == np.isfinite(got)))
        out[name] = {"max_abs_err": err, "nan_agree": agree, "xarray_ms": t_x, "janus_ms": jt[name]}
        print(f"{name:<13} xarray {t_x:7.0f} ms   Janus {jt[name]:7.0f} ms   x{t_x / jt[name]:5.1f}   "
              f"|diff| {err:.1e}  no data agree {agree:.6f}")
    return out


def shown(d):
    b = d / "bands"
    b.mkdir(exist_ok=True)
    rng = np.random.default_rng(3)
    for t in range(24):
        a = rng.uniform(0.01, 0.6, (4, 2048, 2048)).astype(np.float32)
        write_tif(b / f"b_{2020 + t // 12}{t % 12 + 1:02d}15.tif", a, names=["Blue", "Green", "Red", "NIR"])
    janus("shown", d)
    jt = json.loads((d / "shown.json").read_text())
    print(f"overview of 24 dates: a band {jt['band']:.0f} ms, NDVI as a normalized difference {jt['nd']:.0f} ms, "
          f"as an expression {jt['ndvi_expr']:.0f} ms, EVI {jt['evi_expr']:.0f} ms")
    return jt


def main():
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "janus-cube", "--example", "vs_originals_calc"],
                   check=True, cwd=ROOT)
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(tempfile.mkdtemp(prefix="janus_calc_bench_"))
    d.mkdir(parents=True, exist_ok=True)
    res = {"spyndex": spyndex.__version__, "xarray": xr.__version__, "indices": indices(d),
           "reductions": reductions(d), "shown": shown(d)}
    out = ROOT / "bench" / "calc-vs-originals.json"
    out.write_text(json.dumps(res, indent=1))
    print(f"-> {out}")


if __name__ == "__main__":
    main()
