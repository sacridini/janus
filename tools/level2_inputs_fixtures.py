"""The reference of janus-zeit's Level 2 inputs (crates/janus-zeit/src/level2.rs, tests/level2_inputs.rs): what Zeit's
`zeit/_level2.py` computes from a Sentinel-2 L1C .SAFE before it runs the correction, as .npy files that the (ignored,
run by hand) Rust test compares value by value.

    python tools/level2_inputs_fixtures.py <the .SAFE> <the DEM> <out folder> [--zeit <Zeit checkout>] [--jobs 8]

Writes into <out folder> (one folder per scene, e.g. .../janus_ref/<scene>.SAFE): `desc.json` (the metadata the core
reads), `dn.npy` (13, nrows, ncols) uint16, `geo_lon.npy` / `geo_lat.npy` float64, `qai0.npy` int16 and `toa.npy`
(13, nrows, ncols) int16 of `prepare`, `dem10.npy` and `dem20.npy` float32 (`dem_on_grid`, 10 m and 20 m),
`vza20.npy` / `vaa20.npy` float32 (the view angles of B8A in hundredths of a degree, missing filled by the mean) and
`flags.npy` int16 (`fmask_flags`). The .SAFE can be the one with the JPEG 2000 files (Python's GDAL decodes them): the Rust
side may read a copy of the same bands as GeoTIFFs. Every thread count is capped at --jobs (8 at most).
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

JOBS_MAX = 8


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("safe")
    ap.add_argument("dem")
    ap.add_argument("out")
    ap.add_argument("--zeit", default=r"C:\Users\eduar\Documents\dev\zeit")
    ap.add_argument("--jobs", type=int, default=JOBS_MAX)
    a = ap.parse_args()
    jobs = max(1, min(a.jobs, JOBS_MAX))
    for k in ("RAYON_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "GDAL_NUM_THREADS"):
        os.environ[k] = str(jobs)
    sys.path.insert(0, a.zeit)

    import numpy as np

    from zeit import _level2 as l2

    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    times = {}

    def step(name, fn):
        t0 = time.perf_counter()
        r = fn()
        times[name] = round(time.perf_counter() - t0, 2)
        print(f"{name}: {times[name]} s", flush=True)
        return r

    safe = l2.Safe(a.safe)
    desc = safe.describe()
    nr, nc = desc["nrows"], desc["ncols"]
    dn = step("read_bands", lambda: l2.read_bands(safe, desc, jobs))
    np.save(out / "dn.npy", dn)
    lon, lat = step("geo_tables", lambda: l2.geo_tables(safe, desc))
    np.save(out / "geo_lon.npy", lon)
    np.save(out / "geo_lat.npy", lat)
    qai0, toa = step("prepare", lambda: l2.prepare(safe, desc, dn, jobs))
    np.save(out / "qai0.npy", np.ascontiguousarray(qai0))
    np.save(out / "toa.npy", np.ascontiguousarray(toa))
    del toa, qai0
    dem10 = step("dem10", lambda: l2.dem_on_grid(a.dem, safe))
    np.save(out / "dem10.npy", dem10)
    del dem10
    dem20 = step("dem20", lambda: l2.dem_on_grid(a.dem, safe, resolution=20.0, desc=desc))
    np.save(out / "dem20.npy", dem20)
    vza, vaa, missing = step("view_angles", lambda: l2._view_angles_20m(safe, desc, nr // 2, nc // 2, jobs))
    np.save(out / "vza20.npy", vza)
    np.save(out / "vaa20.npy", vaa)
    flags, info = step("fmask_flags", lambda: l2.fmask_flags(safe, desc, dn, dem=dem20, n_jobs=jobs))
    np.save(out / "flags.npy", flags)
    meta = {k: v for k, v in desc.items() if k not in ("vzen", "vazi")}
    meta["vzen_len"] = len(desc["vzen"])
    meta["view_missing"] = missing
    meta["fmask_info"] = {k: (v if isinstance(v, (int, float, str, bool, type(None))) else list(v)) for k, v in info.items()}
    meta["python_seconds"] = times
    (out / "desc.json").write_text(json.dumps(meta, indent=1, default=lambda o: o.tolist() if hasattr(o, "tolist") else str(o)))
    print("written to", out)


if __name__ == "__main__":
    main()
