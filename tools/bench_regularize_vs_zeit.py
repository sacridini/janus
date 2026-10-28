"""Janus's regularization (IDEIAS phase 40) against Zeit's: results and time.

    python tools/bench_regularize_vs_zeit.py [DIR]     (Zeit's environment: zeit, xarray, rasterio)

An irregular series of 64 dates 16 to 20 days apart from 2020-01-03, with a fifth of every date clouded (NaN) and
four dates clouded whole: one band of 1024 x 1024 (`one`) and three bands of 512 x 512 (`multi`). Janus regularizes
it to 16 days (crates/janus-zeit/examples/vs_zeit_regularize.rs, 8 threads, the COGs written); Zeit:
regularize_time_series(cube, "16D", "median" / "medoid") (read from the files, OMP_NUM_THREADS=8), the gaps of the
median filled by xarray's interpolate_na (linear in time) and by zeit.smooth(method="whittaker", lmbda=10) (compared
in the gaps only: Janus keeps the values). The clearest date has no counterpart in Zeit: its time alone. And R's
sits_regularize (sits 1.5 with gdalcubes, 8 cores) on band 1 of `multi` as Int16 reflectances: its time against
Janus's median of the same files, its values against the first value of each period (what it takes without a cloud
band). Writes bench/regularize-vs-zeit.json.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

os.environ.setdefault("OMP_NUM_THREADS", "8")
import numpy as np
import pandas as pd
import rasterio
import xarray as xr
import zeit
from rasterio.transform import from_origin

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_zeit_regularize.exe"


def best(fn, repeat=2):
    fn()
    out = []
    for _ in range(repeat):
        t = time.perf_counter()
        fn()
        out.append(time.perf_counter() - t)
    return min(out) * 1000


def write_tif(path, a):
    k, h, w = a.shape
    prof = dict(driver="GTiff", width=w, height=h, count=k, dtype="float32", crs="EPSG:32720",
                transform=from_origin(500000, 8900000, 10, 10), nodata=np.nan, tiled=True)
    with rasterio.open(path, "w", **prof) as d:
        d.write(a.astype(np.float32))


def make(d, name, side, bands, seed):
    s = d / name
    s.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(seed)
    day = np.datetime64("2020-01-03")
    times = []
    for t in range(64):
        doy = (day - day.astype("datetime64[Y]")).astype(int)
        base = 0.5 + 0.3 * np.sin(2 * np.pi * doy / 365.25)
        a = np.stack([base + 0.1 * b + rng.normal(0, 0.1, (side, side)) for b in range(bands)])
        a[:, rng.random((side, side)) < 0.2] = np.nan
        if t in (9, 10, 30, 47):
            a[:] = np.nan
        f = s / f"s_{str(day).replace('-', '')}.tif"
        if not f.exists():
            write_tif(f, a)
        times.append(day)
        day = day + np.timedelta64(int(rng.integers(16, 21)), "D")
    return sorted(s.glob("*.tif")), np.array(times, dtype="datetime64[ns]")


RSCRIPT = os.environ.get("RSCRIPT", r"C:\Program Files\R\R-4.4.2\bin\Rscript.exe")
SITS_R = """suppressMessages(library(sits))
a <- commandArgs(TRUE)
cube <- sits_cube(source = "MPC", collection = "SENTINEL-2-L2A", data_dir = a[1],
                  parse_info = c("satellite", "sensor", "tile", "band", "date"), progress = FALSE)
t <- system.time(sits_regularize(cube, period = "P16D", res = 10, output_dir = a[2], multicores = 8,
                                 progress = FALSE))
cat("ELAPSED", t[["elapsed"]], "\\n")
"""


def sits_input(d, files, times):
    """Band 1 of `multi` as sits reads a local cube: Int16 reflectances (x 10000, -9999 no data), named
    SENTINEL-2_MSI_20LKP_B04_<date>.tif."""
    s = d / "sits"
    s.mkdir(exist_ok=True)
    for f, t in zip(files, times):
        out = s / f"SENTINEL-2_MSI_20LKP_B04_{str(t)[:10]}.tif"
        if out.exists():
            continue
        with rasterio.open(f) as src:
            a = src.read(1)
            prof = src.profile | dict(count=1, dtype="int16", nodata=-9999)
        v = np.where(np.isfinite(a), np.round(a * 10000), -9999).astype(np.int16)
        with rasterio.open(out, "w", **prof) as o:
            o.write(v, 1)
    return sorted(s.glob("*.tif"))


def sits_run(d, files, times):
    """sits_regularize on the Int16 series (gdalcubes, 8 cores; no cloud band: the periods' first values): its
    time, and its values against the first value of each period."""
    if not Path(RSCRIPT).exists():
        return None
    out = d / "sits_out"
    if out.exists():
        for f in out.iterdir():
            f.unlink()
    out.mkdir(exist_ok=True)
    script = d / "sits_reg.R"
    script.write_text(SITS_R)
    r = subprocess.run([RSCRIPT, str(script), str(d / "sits"), str(out)], capture_output=True, text=True,
                       env=os.environ | {"OMP_NUM_THREADS": "8"})
    line = [x for x in (r.stdout + r.stderr).splitlines() if x.startswith("ELAPSED")]
    if not line:
        print(r.stdout[-2000:], r.stderr[-2000:])
        return None
    ms = float(line[0].split()[1]) * 1000
    got = sorted(out.glob("*.tif"))
    arr = np.stack([rasterio.open(f).read(1) for f in files]).astype(np.float64)
    arr[arr == -9999] = np.nan
    days = times.astype("datetime64[D]")
    start = days[0]
    first = []
    for k in range(len(got)):
        a, b = start + np.timedelta64(16 * k, "D"), start + np.timedelta64(16 * (k + 1), "D")
        sel = arr[(days >= a) & (days < b)]
        f = np.full(arr.shape[1:], np.nan)
        for x in sel:
            f = np.where(np.isnan(f), x, f)
        first.append(f)
    sv = np.stack([rasterio.open(f).read(1) for f in got]).astype(np.float64)
    sv[sv == -9999] = np.nan
    both = np.isfinite(sv) & np.isfinite(np.stack(first))
    same = float(np.mean(sv[both] == np.stack(first)[both])) if both.any() else 0.0
    return {"sits_ms": ms, "dates": len(got), "same_as_first_value": same,
            "sits": r.stdout.split("ELAPSED")[0].strip()[-200:]}


def load(files, times, bands):
    arr = np.stack([rasterio.open(f).read() for f in files])  # time, band, y, x
    if bands == 1:
        return xr.DataArray(arr[:, 0], dims=("time", "y", "x"), coords={"time": times})
    return xr.DataArray(arr, dims=("time", "band", "y", "x"), coords={"time": times})


def write_cogs(da, d):
    """A result as one COG per date, as Janus writes it (DEFLATE, the float predictor)."""
    d.mkdir(parents=True, exist_ok=True)
    for k, t in enumerate(da.time.values):
        a = da.isel(time=k).values
        a = a[None] if a.ndim == 2 else a
        prof = dict(driver="COG", width=a.shape[-1], height=a.shape[-2], count=a.shape[0], dtype="float32",
                    crs="EPSG:32720", transform=from_origin(500000, 8900000, 10, 10), nodata=np.nan,
                    compress="DEFLATE", predictor=3, num_threads=8)
        with rasterio.open(d / f"z_{str(t)[:10].replace('-', '')}.tif", "w", **prof) as f:
            f.write(a.astype(np.float32))


def compare(name, ref, got, mask=None):
    ref = np.asarray(ref, dtype=np.float64).ravel()
    got = got.astype(np.float64)
    assert got.size == ref.size, f"{name}: {got.size} values vs {ref.size}"
    if mask is not None:
        mask = np.asarray(mask).ravel()
        ref, got = ref[mask], got[mask]
    both = np.isfinite(ref) & np.isfinite(got)
    err = float(np.max(np.abs(got[both] - ref[both]))) if both.any() else 0.0
    agree = float(np.mean(np.isfinite(ref) == np.isfinite(got)))
    return err, agree


def main():
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target" / "bench-regularize"
    d.mkdir(parents=True, exist_ok=True)
    one, t1 = make(d, "one", 1024, 1, 4)
    multi, tm = make(d, "multi", 512, 3, 5)
    sits_files = sits_input(d, multi, tm)
    subprocess.run(["cargo", "build", "-j", "8", "--release", "-p", "janus-zeit", "--example", "vs_zeit_regularize"],
                   cwd=ROOT, check=True)
    subprocess.run([str(EXE), str(d)], check=True)
    jt = json.loads((d / "regularize.json").read_text())
    res = {}
    rows = []

    def zeit_reg(files, times, bands, method):
        return zeit.regularize_time_series(load(files, times, bands), freq="16D", method=method)

    def got(name):
        return np.fromfile(d / f"{name}.f32", dtype=np.float32)

    out = {}
    for name, files, times, bands, method in [("median", one, t1, 1, "median"), ("medoid", multi, tm, 3, "medoid"),
                                              ("medoid_one", one, t1, 1, "medoid")]:
        ms = best(lambda: out.__setitem__(name, zeit_reg(files, times, bands, method)))
        err, agree = compare(name, out[name].values, got(name))
        w = best(lambda: write_cogs(out[name], d / "zeit" / name), 1)
        res[name] = {"zeit_ms": ms, "zeit_write_ms": w, "janus_ms": jt[name]["ms"], "max_abs_err": err,
                     "nan_agree": agree, "dates": jt[name]["dates"], "zeit_dates": int(out[name].sizes["time"])}
    med = out["median"]
    lin = {}
    ms = best(lambda: lin.__setitem__("v", zeit_reg(one, t1, 1, "median").interpolate_na(dim="time", method="linear")))
    err, agree = compare("linear", lin["v"].values, got("linear"))
    res["linear"] = {"zeit_ms": ms, "janus_ms": jt["linear"]["ms"], "max_abs_err": err, "nan_agree": agree,
                     "dates": jt["linear"]["dates"], "against": "xarray interpolate_na(method='linear')"}
    wh = {}
    ms = best(lambda: wh.__setitem__("v", zeit.smooth(zeit_reg(one, t1, 1, "median"), method="whittaker", lmbda=10.0,
                                                      n_jobs=8)))
    # the gaps between two values (Janus keeps the values and does not extend the ends)
    v = med.values
    ok = np.isfinite(v)
    first, last = ok.argmax(0), len(v) - 1 - ok[::-1].argmax(0)
    k = np.arange(len(v))[:, None, None]
    gaps = ~ok & (k > first) & (k < last)
    err, agree = compare("whittaker", np.asarray(wh["v"]), got("whittaker"), gaps)
    res["whittaker"] = {"zeit_ms": ms, "janus_ms": jt["whittaker"]["ms"], "max_abs_err_in_gaps": err,
                        "nan_agree_in_gaps": agree, "gaps": int(gaps.sum()), "against": "zeit.smooth(whittaker)"}
    res["clearest"] = {"janus_ms": jt["clearest"]["ms"], "dates": jt["clearest"]["dates"]}
    sr = sits_run(d, sits_files, tm)
    if sr:
        sr["janus_median_ms"] = jt["sits_median"]["ms"]
        sr["janus_dates"] = jt["sits_median"]["dates"]
        res["sits"] = sr
        print(f"sits_regularize {sr['sits_ms']:7.0f} ms ({sr['dates']} dates, the first value of each period on "
              f"{sr['same_as_first_value']:.4f} of the pixels)   Janus's median {sr['janus_median_ms']:.0f} ms "
              f"({sr['janus_dates']} dates): x{sr['sits_ms'] / sr['janus_median_ms']:.1f}")
    for k_, r in res.items():
        if k_ == "sits":
            continue
        z = r.get("zeit_ms")
        e = r.get("max_abs_err", r.get("max_abs_err_in_gaps"))
        zw = r.get("zeit_write_ms")
        rows.append(f"{k_:<11} " + (f"Zeit {z:7.0f} ms   " if z else " " * 18) + f"Janus {r['janus_ms']:7.0f} ms" +
                    (f"   x{z / r['janus_ms']:5.1f}   |diff| {e:.1e}" if z else "") +
                    (f"   (Zeit + its COGs written {z + zw:.0f} ms: x{(z + zw) / r['janus_ms']:.1f})" if zw else ""))
    print("\n".join(rows))
    res["zeit"] = getattr(zeit, "__version__", "?")
    res["xarray"] = xr.__version__
    res["pandas"] = pd.__version__
    o = ROOT / "bench" / "regularize-vs-zeit.json"
    o.write_text(json.dumps(res, indent=1))
    print(f"-> {o}")


if __name__ == "__main__":
    main()
