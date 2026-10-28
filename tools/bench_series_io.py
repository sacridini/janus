"""Janus's Build series and Save series as (IDEIAS phase 46) against gdalbuildvrt, gdal_translate and xarray.

    python tools/bench_series_io.py     (the geo env: GDAL's command line tools, rasterio, rioxarray, xarray with
                                         netCDF4 and zarr, dask)

Loose files: 24 dates x 2 bands (B04, B08) x 4 tiles of 1024 x 1024 (Int16, UTM 23S, 10 m): 192 files. Building
(crates/janus-zeit/examples/vs_series_io.rs, 8 threads): Janus's plan and VRTs against the gdalbuildvrt commands
that make the same thing (a mosaic per date and band, then -separate, on one -te / -tr), compared pixel for pixel;
as COGs against gdal_translate -of COG one date after another (NUM_THREADS=8). Saving the 24 dates' B04 (2048 x 2048):
Janus to a multiband COG, a NetCDF and a Zarr against rioxarray + xarray (each date opened, concatenated on time,
to_netcdf with zlib and to_zarr); xarray reads Janus's NetCDF and Zarr back: the same values and dates.
Writes bench/series-io.json.
"""
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

os.environ.setdefault("GDAL_NUM_THREADS", "8")
os.environ.setdefault("OMP_NUM_THREADS", "8")
import numpy as np
import pandas as pd
import rasterio
import rioxarray  # noqa: F401
import xarray as xr
from rasterio.transform import from_origin

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "target" / "release" / "examples" / "vs_series_io.exe"
D = ROOT / "target" / "bench-series-io"
N, PX, X0, Y0, DATES = 1024, 10.0, 600000.0, 7500000.0, 24
BIN = Path(shutil.which("gdalbuildvrt")).parent


def make():
    src = D / "files"
    src.mkdir(parents=True, exist_ok=True)
    rng = np.random.default_rng(46)
    dates = pd.date_range("2021-01-05", periods=DATES, freq="15D")
    files = []
    for d in dates:
        for ti, (tx, ty) in enumerate([(0, 0), (1, 0), (0, 1), (1, 1)]):
            tr = from_origin(X0 + tx * N * PX, Y0 - ty * N * PX, PX, PX)
            for b in ["B04", "B08"]:
                p = src / f"S2_{d:%Y%m%d}_T{ti + 1}_{b}.tif"
                files.append(str(p))
                if p.exists():
                    continue
                v = rng.integers(0, 10000, (N, N), dtype="int16")
                with rasterio.open(p, "w", driver="GTiff", width=N, height=N, count=1, dtype="int16", crs="EPSG:32723",
                                   transform=tr, tiled=True, compress="deflate", nodata=-1) as o:
                    o.write(v, 1)
    return files, dates


def run(args):
    r = subprocess.run([str(EXE), *args], capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(r.stderr)
    return json.loads(r.stdout)


def best(f, n=3):
    out = [f() for _ in range(n)]
    return min(out, key=lambda x: x["seconds"])


def gdal_build(files, dates, out, cog):
    """The same series with the command line tools: per date a mosaic per band, then the bands separate."""
    t = time.perf_counter()
    if out.exists():
        shutil.rmtree(out)
    (out / "bands").mkdir(parents=True)
    te = [str(X0), str(Y0 - 2 * N * PX), str(X0 + 2 * N * PX), str(Y0)]
    made = []
    for d in dates:
        bands = []
        for b in ["B04", "B08"]:
            fs = [f for f in files if f"_{d:%Y%m%d}_" in f and f.endswith(f"_{b}.tif")]
            m = out / "bands" / f"{d:%Y%m%d}_{b}.vrt"
            subprocess.run([str(BIN / "gdalbuildvrt"), "-q", "-te", *te, "-tr", "10", "10", str(m), *fs], check=True)
            bands.append(str(m))
        v = out / f"s_{d:%Y%m%d}.vrt"
        subprocess.run([str(BIN / "gdalbuildvrt"), "-q", "-separate", "-te", *te, "-tr", "10", "10", str(v), *bands],
                       check=True)
        made.append(v)
        if cog:
            subprocess.run([str(BIN / "gdal_translate"), "-q", "-of", "COG", "-co", "COMPRESS=DEFLATE", "-co",
                            "PREDICTOR=YES", "-co", "NUM_THREADS=8", str(v), str(out / f"s_{d:%Y%m%d}.tif")],
                           check=True)
    return time.perf_counter() - t, made


def main():
    files, dates = make()
    res = {"files": len(files), "dates": DATES, "grid": [2 * N, 2 * N]}
    # building: VRTs, then COGs
    jv = best(lambda: run(["build", str(D / "janus_vrt"), "s", "vrt", "8", *files]))
    gv_s, gmade = min((gdal_build(files, dates, D / "gdal_vrt", False) for _ in range(3)), key=lambda x: x[0])
    differ = 0
    for k, d in enumerate(dates):
        with rasterio.open(D / "janus_vrt" / f"s_{d:%Y%m%d}.vrt") as a, rasterio.open(gmade[k]) as b:
            differ += int(np.count_nonzero(a.read() != b.read()))
    res["build_vrt"] = {"janus_s": jv["seconds"], "janus_plan_s": jv["plan_s"], "gdalbuildvrt_s": gv_s,
                        "bands": jv["bands"], "values_differing": differ}
    print("build_vrt", json.dumps(res["build_vrt"]))
    jc = run(["build", str(D / "janus_cog"), "s", "cog", "8", *files])
    gc_s, _ = gdal_build(files, dates, D / "gdal_cog", True)
    differ = 0
    for d in dates:
        with rasterio.open(D / "janus_cog" / f"s_{d:%Y%m%d}.tif") as a, rasterio.open(D / "gdal_cog" / f"s_{d:%Y%m%d}.tif") as b:
            differ += int(np.count_nonzero(a.read() != b.read()))
    res["build_cog"] = {"janus_s": jc["seconds"], "gdal_translate_s": gc_s, "values_differing": differ}
    print("build_cog", json.dumps(res["build_cog"]))
    # saving the 24 dates' first band
    vrts = [str(D / "janus_vrt" / f"s_{d:%Y%m%d}.vrt") for d in dates]
    out = D / "saved"
    out.mkdir(exist_ok=True)
    save = {}
    for fmt, ext in [("cog", "tif"), ("nc", "nc"), ("zarr", "zarr")]:
        p = out / f"janus.{ext}"
        save[fmt] = {"janus_s": run(["save", str(p), fmt, "shown", "8", *vrts])["seconds"]}
    # xarray: each date's first band opened, concatenated on time, written
    def stack():
        arrs = [rioxarray.open_rasterio(v).sel(band=1, drop=True).astype("float32") for v in vrts]
        da = xr.concat(arrs, dim=pd.Index(dates, name="time")).rename("value").load()
        da.attrs.pop("long_name", None)  # the two bands' names, from the VRTs: one band kept
        return da
    t = time.perf_counter()
    da = stack()
    read_s = time.perf_counter() - t
    t = time.perf_counter()
    da.to_netcdf(out / "xarray.nc", encoding={"value": {"zlib": True, "chunksizes": (1, 512, 512)}})
    save["nc"]["xarray_s"] = time.perf_counter() - t + read_s
    t = time.perf_counter()
    if (out / "xarray.zarr").exists():
        shutil.rmtree(out / "xarray.zarr")
    da.chunk({"time": 1, "y": 512, "x": 512}).to_zarr(out / "xarray.zarr", zarr_format=2)
    save["zarr"]["xarray_s"] = time.perf_counter() - t + read_s
    t = time.perf_counter()
    da.rio.write_crs("EPSG:32723").transpose("time", "y", "x").rio.to_raster(out / "xarray.tif", driver="COG",
                                                                          compress="DEFLATE")
    save["cog"]["rioxarray_s"] = time.perf_counter() - t + read_s
    save["xarray_read_s"] = read_s
    # Janus's files read by xarray and rasterio: the values and dates
    want = da.values
    nc = xr.open_dataset(out / "janus.nc")
    save["nc"]["xarray_reads_same"] = bool(np.array_equal(nc["value"].values, want)) and \
        bool((pd.to_datetime(nc["time"].values) == dates).all())
    zr = xr.open_zarr(out / "janus.zarr")
    save["zarr"]["xarray_reads_same"] = bool(np.array_equal(zr["value"].values, want)) and \
        bool((pd.to_datetime(zr["time"].values) == dates).all())
    with rasterio.open(out / "janus.tif") as c:
        save["cog"]["same"] = bool(np.array_equal(c.read(), want)) and \
            list(c.descriptions) == [f"{d:%Y-%m-%d}" for d in dates]
    res["save"] = save
    print("save", json.dumps(save))
    p = ROOT / "bench" / "series-io.json"
    p.write_text(json.dumps(res, indent=2))
    print("written", p)


if __name__ == "__main__":
    main()
