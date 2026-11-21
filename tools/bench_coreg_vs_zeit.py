"""Janus's co-registration of dates against Zeit's and AROSICS' (the shifts, the series and the time).

    python tools/bench_coreg_vs_zeit.py [DIR] [SIDE] [DATES]      (Zeit's environment: zeit, xarray, rasterio, scipy)

A synthetic series of DATES dates (default 8) of SIDE x SIDE pixels (default 2048) of 30 m, four bands a date: a scene of
value noise (5 octaves, cubic interpolation of random lattices at fractional coordinates, so a shift of a fraction of a
pixel is exact), a different scene for each band, the dates the first moved by known shifts (whole and fractional pixels).
Janus runs it through the job runner (crates/janus-zeit/examples/vs_zeit_coregister.rs: the bands read from their files,
the shifts found against the first date, the series written as warped VRTs, or as COGs; 4 threads, best of 3) and its
shifts and its COGs are compared with Zeit's own calls (zeit.coregister and zeit.coregister_local on the same cube; 4
threads, best of 3, with and without the series written). AROSICS 1.14.0 (Apache-2.0) runs in its own conda environment
(`arosics-ref`, Zeit's benchmark's) through Zeit's driver (benchmarks/vs_originals/arosics_driver.py, which calls its
public API: COREG and COREG_LOCAL, estimating the shifts and correcting the target): the shifts and the time, one CPU and
four. Only outputs are compared with AROSICS. Writes bench/coregister-vs-zeit.json.
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
from scipy import ndimage as ndi

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_zeit_coregister.exe"
ZEIT_REPO = Path(os.environ.get("ZEIT_REPO", r"C:\Users\eduar\Documents\dev\zeit"))
AROSICS_ENV = Path(os.environ.get("AROSICS_ENV", r"C:\Users\eduar\miniforge3\envs\arosics-ref"))
RES, NB = 30.0, 4
# the amount each date must be moved to fit the first (x right, y down)
SHIFTS = [(0.0, 0.0), (3.0, -2.0), (0.4, 1.7), (-2.25, 1.1), (1.0, 0.0), (-0.5, -3.0), (2.0, 2.0), (0.0, -1.0)]


def best(fn, repeat=3):
    fn()
    out = []
    for _ in range(repeat):
        t = time.perf_counter()
        fn()
        out.append(time.perf_counter() - t)
    return min(out)


def scene(seed, side, dx, dy):
    """Value noise at the pixel centres, its content at (x + dx, y + dy): to be moved by (dx, dy) to fit the unmoved."""
    yy, xx = np.mgrid[0:side, 0:side].astype(np.float32)
    out = np.zeros((side, side), np.float32)
    for o in range(5):
        period = 2 << o
        lat = np.random.default_rng(seed * 100 + o).normal(size=(side // period + 8, side // period + 8))
        coords = np.stack([(yy + 0.5 + dy) / period + 3, (xx + 0.5 + dx) / period + 3])
        out += (ndi.map_coordinates(lat, coords, order=3, mode="nearest") * np.sqrt(period)).astype(np.float32)
    return 0.3 + 0.04 * out


def write(path, a):
    a = a if a.ndim == 3 else a[None]
    prof = dict(driver="GTiff", width=a.shape[-1], height=a.shape[-2], count=a.shape[0], dtype="float32", crs="EPSG:32722",
                transform=from_origin(500000, 8000000, RES, RES), nodata=np.nan, tiled=True)
    with rasterio.open(path, "w", **prof) as d:
        d.write(a.astype(np.float32))


def make(d, side, dates):
    d.mkdir(parents=True, exist_ok=True)
    cube = np.zeros((dates, NB, side, side), np.float32)
    for t in range(dates):
        dx, dy = SHIFTS[t % len(SHIFTS)]
        for b in range(NB):
            cube[t, b] = scene(11 + b, side, dx, dy)
    for b in range(NB):
        write(d / f"b{b + 1}.tif", cube[:, b])
    for t in range(dates):
        write(d / f"nir_{t}.tif", cube[t, NB - 1])
    return cube


def janus(d, mode, fmt, threads, window, grid, dates):
    out = subprocess.run([str(EXE), str(d), str(NB), str(dates), mode, fmt, str(threads), str(window), str(grid), "3"],
                         check=True, capture_output=True, text=True,
                         env={**os.environ, "GDAL_NUM_THREADS": "1", "PATH": str(ROOT / "target" / "release") + os.pathsep + os.environ["PATH"]})
    return json.loads(out.stdout.strip().splitlines()[-1])


def csv(path):
    rows = [l.split(",") for l in Path(path).read_text().splitlines()]
    return {c: [r[i] for r in rows[1:]] for i, c in enumerate(rows[0])}


def arosics(d, dates, mode, cpus, window, grid):
    """AROSICS in its own environment, through Zeit's driver: a pair for each date against the first."""
    driver = ZEIT_REPO / "benchmarks" / "vs_originals" / "arosics_driver.py"
    if not driver.exists():
        return None
    spec = {"repeat": 1, "pairs": [{"name": f"date{t}", "ref": str(d / "nir_0.tif"), "tgt": str(d / f"nir_{t}.tif"),
                                    "nodata": None, "ws": window, "cpus": cpus, "grid_res": grid,
                                    "table": str(d / f"arosics_table_{t}.json")} for t in range(1, dates)]}
    (d / "arosics_spec.json").write_text(json.dumps(spec))
    # its own environment's Python, with that environment's DLLs first and the user's site-packages off (as Zeit's benchmark)
    env = dict(os.environ, PYTHONNOUSERSITE="1", OMP_NUM_THREADS=str(cpus), OPENBLAS_NUM_THREADS=str(cpus),
               MKL_NUM_THREADS=str(cpus), GDAL_NUM_THREADS="1", PYTHONIOENCODING="utf-8")
    # (Janus's PROJ and GDAL data are for its own libraries, not AROSICS' environment's)
    for k in ("PROJ_DATA", "PROJ_LIB", "GDAL_DATA", "GDAL_DRIVER_PATH"):
        env.pop(k, None)
    e = str(AROSICS_ENV)
    env["PATH"] = os.pathsep.join([e, e + r"\Library\mingw-w64\bin", e + r"\Library\usr\bin", e + r"\Library\bin",
                                   e + r"\Scripts", env["PATH"]])
    t0 = time.perf_counter()
    r = subprocess.run([str(AROSICS_ENV / "python.exe"), str(driver), "run" if mode == "global" else "local",
                        str(d / "arosics_spec.json"), str(d / "arosics_out.json")],
                       capture_output=True, text=True, env=env, stdin=subprocess.DEVNULL)
    if r.returncode != 0:
        print("AROSICS did not run:", r.stderr[-500:])
        return None
    out = json.loads((d / "arosics_out.json").read_text())
    return {"wall_seconds": time.perf_counter() - t0, "pairs": out}


def arosics_global(d, dates, dx, dy, result):
    for cpus in (1, 4):
        a = arosics(d, dates, "global", cpus, 256, 200)
        if a:
            result.setdefault("arosics_global", {})[f"cpus{cpus}"] = a
            ok = [p for p in a["pairs"] if p.get("success")]
            diff = max([abs(p["x_shift_px"] - dx[int(p["name"][4:])]) for p in ok] + [abs(p["y_shift_px"] - dy[int(p["name"][4:])]) for p in ok])
            est = sum(p["t_estimate"] for p in ok)
            cor = sum(p.get("t_correct", 0) for p in ok)
            print(f"AROSICS COREG, {cpus} CPU: {est:.2f} s the shifts of {len(ok)} dates, {cor:.2f} s their correction; "
                  f"its shifts differ from Janus's by at most {diff:.1e}")


def arosics_local(d, dates, result):
    a = arosics(d, min(dates, 3), "local", 4, 256, 256)
    if a:
        result["arosics_local"] = a
        print("AROSICS COREG_LOCAL, 4 CPUs:", round(a["wall_seconds"], 1), "s wall for", len(a["pairs"]), "dates")


def main():
    d = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ.get("TEMP", ".")) / "janus-bench-coreg"
    side = int(sys.argv[2]) if len(sys.argv) > 2 else 2048
    dates = int(sys.argv[3]) if len(sys.argv) > 3 else 8
    if os.environ.get("BENCH_ONLY_AROSICS") == "1":
        # the data and the other results of an earlier run: only AROSICS is run and added to them
        result = json.loads((ROOT / "bench" / "coregister-vs-zeit.json").read_text())
        dx, dy = (np.array([np.nan if v is None else v for v in row]) for row in result["global"]["shifts_janus"])
        arosics_global(d, dates, dx, dy, result)
        arosics_local(d, dates, result)
        (ROOT / "bench" / "coregister-vs-zeit.json").write_text(json.dumps(result, indent=1))
        return
    cube = make(d, side, dates)
    x = RES * np.arange(side) + 500000 + RES / 2
    y = 8000000 - RES * np.arange(side) - RES / 2
    da = xr.DataArray(cube, dims=("time", "band", "y", "x"),
                      coords={"time": np.arange(dates), "band": [f"b{i + 1}" for i in range(NB)], "y": y, "x": x}).rio.write_crs(32722)
    ref = da.sel(band=f"b{NB}").isel(time=0)
    band = f"b{NB}"
    result = {"side": side, "dates": dates, "bands": NB, "threads": 4, "truth": SHIFTS[:dates]}

    # ---- global
    jv = janus(d, "global", "vrt", 4, 256, 200, dates)
    jc = janus(d, "global", "cog", 4, 256, 200, dates)
    jn = janus(d, "global", "none", 4, 256, 200, dates)
    mine = csv(Path(jc["folder"]) / "shifts.csv")
    kw = dict(window_size=256, band=band, n_jobs=4)
    aligned, shifts = zeit.coregister(da, ref, **kw)
    t_call = best(lambda: zeit.coregister(da, ref, **kw))

    def call_and_save():
        a, _ = zeit.coregister(da, ref, **kw)
        zeit.save_raster(a, str(d / "zeit_aligned"))

    t_save = best(call_and_save)
    dx = np.array([float(v) if v else np.nan for v in mine["x_shift_px"]])
    dy = np.array([float(v) if v else np.nan for v in mine["y_shift_px"]])
    shift_diff = max(np.nanmax(np.abs(dx - shifts["x_shift_px"].values)), np.nanmax(np.abs(dy - shifts["y_shift_px"].values)))
    files = sorted((Path(jc["folder"]) / "series").glob("*.tif"))
    worst = 0.0
    for t, f in enumerate(files):
        with rasterio.open(f) as s:
            ours = s.read()
        z = aligned.isel(time=t).values
        sl = (slice(None), slice(8, -8), slice(8, -8))
        both = np.isfinite(ours[: NB][sl]) & np.isfinite(z[sl])
        worst = max(worst, float(np.abs(ours[: NB][sl] - z[sl])[both].max()))
    result["global"] = {
        "janus_shifts_only_seconds": jn["seconds"], "janus_vrt_seconds": jv["seconds"], "janus_cog_seconds": jc["seconds"],
        "zeit_call_seconds": t_call,
        "zeit_call_and_save_seconds": t_save, "max_shift_difference_to_zeit": float(shift_diff),
        "max_aligned_difference_to_zeit": worst,
        "shifts_janus": [dx.tolist(), dy.tolist()],
    }
    print(f"global: Janus {jn['seconds']:.2f} s (the shifts only), {jv['seconds']:.2f} s (warped VRTs), {jc['seconds']:.2f} s (COGs); Zeit {t_call:.2f} s (the call), "
          f"{t_save:.2f} s (and saved); shifts differ by {shift_diff:.1e}, the series by at most {worst:.1e}")
    arosics_global(d, dates, dx, dy, result)

    # ---- local
    jl = janus(d, "local", "cog", 4, 256, 256, dates)
    jln = janus(d, "local", "none", 4, 256, 256, dates)
    kwl = dict(grid_res=256, window_size=256, band=band, n_jobs=4)
    la, points = zeit.coregister_local(da, ref, **kwl)
    t_local = best(lambda: zeit.coregister_local(da, ref, **kwl), repeat=2)

    def local_and_save():
        a, _ = zeit.coregister_local(da, ref, **kwl)
        zeit.save_raster(a, str(d / "zeit_local"))

    t_local_save = best(local_and_save, repeat=2)
    mine_pts = csv(Path(jl["folder"]) / "tie_points.csv")
    used_mine = sum(v == "true" for v in mine_pts["used"])
    # (Zeit also matches the reference to itself)
    used_zeit = int(points[points["time"] != points["time"].iloc[0]]["used"].sum())
    result["local"] = {"janus_shifts_only_seconds": jln["seconds"], "janus_cog_seconds": jl["seconds"],
                       "zeit_call_seconds": t_local, "zeit_call_and_save_seconds": t_local_save,
                       "tie_points_used_janus": used_mine, "tie_points_used_zeit": used_zeit}
    print(f"local: Janus {jln['seconds']:.2f} s (the tie points only), {jl['seconds']:.2f} s (COGs); Zeit {t_local:.2f} s (the call), "
          f"{t_local_save:.2f} s (and saved); tie points used {used_mine} against {used_zeit}")
    arosics_local(d, dates, result)
    (ROOT / "bench").mkdir(exist_ok=True)
    (ROOT / "bench" / "coregister-vs-zeit.json").write_text(json.dumps(result, indent=1))


if __name__ == "__main__":
    main()
