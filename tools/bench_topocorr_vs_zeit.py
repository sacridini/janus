"""Janus's topographic correction against Zeit's and RStoolbox's: results and time.

    python tools/bench_topocorr_vs_zeit.py [DIR] [SIDE]      (Zeit's environment: zeit, xarray, rasterio)

A synthetic scene of SIDE x SIDE pixels (default 2000) of 30 m, six bands and one date: hills of 1 to 6 km with finer relief
for a DEM (as float32), a land cover of vegetation and soil mixed by a smooth field, and the light of the relief
with a diffuse part (so that the C-correction has a c), the sun at zenith 38 and azimuth 140 degrees. Janus runs it through
the job runner (crates/janus-zeit/examples/vs_zeit_topocorr.rs: the six bands read from their files, the DEM warped
onto the grid, the corrected file written; 4 threads, best of 3) and the corrected file is compared with Zeit's own call
(zeit.topographic_correction on the same cube and DEM; 4 threads, and the same read from the files and written, best of 3)
for the C-correction and SCS+C. RStoolbox's topCor(method = "C") (Rscript, one thread; RStoolbox is read through its
installed package, never its source) runs through Zeit's harness script (benchmarks/vs_originals/topocorr.R, given
Zeit's slope and aspect so that the correction is compared) on the same bands: the values and the time. RStoolbox fits on a
sample of at most 10000 pixels (what it keeps of the cells), Janus and Zeit on every pixel with a slope of at least
`min_slope` (0 here), so on a large image the two differ by the sampling. Writes bench/topocorr-vs-zeit.json.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

os.environ.setdefault("OMP_NUM_THREADS", "4")
import rasterio
import xarray as xr
import zeit
from rasterio.transform import from_origin
from zeit._topocorr import slope_aspect

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_zeit_topocorr.exe"
ZEIT_REPO = Path(os.environ.get("ZEIT_REPO", r"C:\Users\eduar\Documents\dev\zeit"))
RSCRIPT = os.environ.get("RSCRIPT", r"C:\Program Files\R\R-4.4.2\bin\Rscript.exe")
SZ, SAZ, RES, NB = 38.0, 140.0, 30.0, 6


def best(fn, repeat=3):
    fn()
    out = []
    for _ in range(repeat):
        t = time.perf_counter()
        fn()
        out.append(time.perf_counter() - t)
    return min(out)


def terrain(side, seed=9):
    rng = np.random.default_rng(seed)
    yy, xx = np.mgrid[0:side, 0:side] * RES
    z = np.zeros((side, side))
    for _ in range(14):
        k = rng.uniform(2 * np.pi / 6000, 2 * np.pi / 1000)
        th = rng.uniform(0, 2 * np.pi)
        z += rng.uniform(15, 60) * np.sin(k * (np.cos(th) * xx + np.sin(th) * yy) + rng.uniform(0, 6.3))
    fine = rng.normal(0, 1, (side, side))
    for _ in range(3):
        fine = (fine + np.roll(fine, 1, 0) + np.roll(fine, 1, 1) + np.roll(fine, (1, 1), (0, 1))) / 4
    z += 12 * fine / fine.std() + 800
    return z.astype(np.float32)


def smooth_field(side, rng, passes=12):
    f = rng.normal(0, 1, (side, side))
    for _ in range(passes):
        f = (f + np.roll(f, 1, 0) + np.roll(f, -1, 0) + np.roll(f, 1, 1) + np.roll(f, -1, 1)) / 5
    return (f - f.min()) / (f.max() - f.min())


def write(path, a):
    a = a if a.ndim == 3 else a[None]
    side = a.shape[-1]
    prof = dict(driver="GTiff", width=side, height=side, count=a.shape[0], dtype="float32", crs="EPSG:32722",
                transform=from_origin(500000, 8000000, RES, RES), nodata=np.nan, tiled=True)
    with rasterio.open(path, "w", **prof) as d:
        d.write(a.astype(np.float32))


def make(d, side):
    d.mkdir(parents=True, exist_ok=True)
    z = terrain(side)
    write(d / "dem.tif", z)
    dem = zeit.load_raster(d / "dem.tif").squeeze(drop=True)
    sa = slope_aspect(dem)
    rng = np.random.default_rng(5)
    veg = smooth_field(side, rng)
    soil = np.array([0.08, 0.12, 0.16, 0.22, 0.30, 0.26])
    green = np.array([0.03, 0.06, 0.04, 0.45, 0.22, 0.10])
    s, a = np.radians(sa.slope.values), np.radians(sa.aspect.values)
    sz, saz = np.radians(SZ), np.radians(SAZ)
    cosi = np.cos(sz) * np.cos(s) + np.sin(sz) * np.sin(s) * np.cos(saz - a)
    light = 0.7 * cosi / np.cos(sz) + 0.3
    bands = np.stack([(veg * green[b] + (1 - veg) * soil[b]) * light + rng.normal(0, 0.004, (side, side)) for b in range(NB)])
    for b in range(NB):
        write(d / f"b{b + 1}.tif", bands[b])
    bands.reshape(NB, -1).T.astype(np.float64).tofile(d / "img.bin")
    np.stack([s.ravel(), a.ravel()], 1).astype(np.float64).tofile(d / "sa.bin")
    return dem, bands.astype(np.float32)


def janus(d, method, threads, repeat=3):
    out = subprocess.run([str(EXE), str(d), str(NB), str(SZ), str(SAZ), method, str(threads), str(repeat)],
                         check=True, capture_output=True, text=True,
                         env={**os.environ, "GDAL_NUM_THREADS": "1", "PATH": str(ROOT / "target" / "release") + os.pathsep + os.environ["PATH"]})
    return json.loads(out.stdout.strip().splitlines()[-1])


def main():
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ.get("TEMP", ".")) / "janus-bench-topocorr"
    side = int(sys.argv[2]) if len(sys.argv) > 2 else 2000
    dem, bands = make(d, side)
    cube = xr.DataArray(bands, dims=("band", "y", "x"), coords={"band": [f"b{i + 1}" for i in range(NB)],
                        "y": dem.y.values, "x": dem.x.values}).rio.write_crs(32722)
    result = {"side": side, "bands": NB, "threads": 4}
    for method, key in (("c", "c"), ("scs+c", "scs+c")):
        run = janus(d, method, 4)
        folder = Path(run["folder"])
        files = sorted((folder / "series").glob("*.tif"))
        with rasterio.open(files[0]) as f:
            ours = f.read().astype(np.float64)
        theirs = zeit.topographic_correction(cube, dem, sun_zenith=SZ, sun_azimuth=SAZ, method=method, min_slope=0.0, n_jobs=4)
        diff = np.abs(ours - theirs.values.astype(np.float64))
        t_zeit = best(lambda: zeit.topographic_correction(cube, dem, sun_zenith=SZ, sun_azimuth=SAZ, method=method,
                                                          min_slope=0.0, n_jobs=4))

        def zeit_files():
            z = zeit.topographic_correction(cube, dem, sun_zenith=SZ, sun_azimuth=SAZ, method=method, min_slope=0.0, n_jobs=4)
            zeit.save_raster(z, str(d / f"zeit_{key.replace('+', '')}"))

        t_zeit_files = best(zeit_files)
        c_zeit = theirs.topo_c.values.ravel()
        rows = list(open(folder / "topo_c.csv").read().splitlines())[1:]
        c_ours = np.array([float(r.split(",")[3]) for r in rows])
        result[key] = {
            "janus_seconds": run["seconds"], "zeit_call_seconds": t_zeit, "zeit_call_and_save_seconds": t_zeit_files,
            "max_abs_difference": float(diff.max()), "max_c_difference": float(np.abs(c_ours - c_zeit).max()),
            "c": c_zeit.tolist(),
        }
        print(f"{method}: Janus {run['seconds']:.2f} s (read, warp the DEM, correct, write), Zeit {t_zeit:.2f} s "
              f"(the call alone), {t_zeit_files:.2f} s (and written); max |difference| {diff.max():.2e}, c {np.abs(c_ours - c_zeit).max():.1e}")
    # RStoolbox topCor(method = "C"), one thread, Zeit's harness
    script = ZEIT_REPO / "benchmarks" / "vs_originals" / "topocorr.R"
    if script.exists() and Path(RSCRIPT).exists():
        env = {**os.environ}
        env["PATH"] = os.pathsep.join(p for p in env["PATH"].split(os.pathsep) if not any(k in p for k in ("GDAL", "Strawberry")))
        out = subprocess.run([RSCRIPT, str(script), str(d), "topcor", str(np.radians(SAZ)), str(np.radians(SZ))],
                             capture_output=True, text=True, env=env)
        sec = [l for l in out.stdout.splitlines() if l.startswith("SECONDS")]
        if sec:
            r_seconds = float(sec[0].split()[1])
            theirs_r = np.fromfile(d / "topcor_given.bin").reshape(side, side, NB).transpose(2, 0, 1)
            c = zeit.topographic_correction(cube, dem, sun_zenith=SZ, sun_azimuth=SAZ, method="c", min_slope=0.0, n_jobs=4)
            result["rstoolbox"] = {"seconds_one_thread": r_seconds, "max_relative_difference_to_zeit":
                                   float((np.abs(c.values - theirs_r) / np.abs(theirs_r)).max())}
            print(f"RStoolbox topCor(C), one thread: {r_seconds:.2f} s; max relative difference to Zeit's {result['rstoolbox']['max_relative_difference_to_zeit']:.1e} "
                  "(it fits on a sample of at most 10000 pixels)")
        else:
            print("RStoolbox did not run:", out.stderr[-400:])
    (ROOT / "bench").mkdir(exist_ok=True)
    (ROOT / "bench" / "topocorr-vs-zeit.json").write_text(json.dumps(result, indent=1))


if __name__ == "__main__":
    main()
