"""The reference of janus-zeit's Level 2 run (crates/janus-zeit/src/level2_run.rs, tests/level2_run.rs): Zeit's `zeit.level2` on a
Sentinel-2 L1C .SAFE with the defaults (brdf, topography, Fmask) and the DEM, its products written by `ds.zeit.save(folder)`
and its attributes, for one of the merge options (one process per option: the run takes 21 to 29 GiB).

    python tools/level2_run_fixtures.py <the .SAFE> <the DEM> <out folder> --merge none|regression|improphe [--zeit <Zeit checkout>] [--jobs 8]

Writes <out folder>/level2_<merge>/ with `boa.tif`, `qai.tif`, `dst.tif`, `vzn.tif`, `hot.tif`, `aod.tif`, `wvp.tif` (what
`.zeit.save` writes) and `attrs.json` (the Dataset's attrs; a NaN as the string "NaN"). Every thread count is capped at --jobs
(8 at most).
"""

import argparse
import json
import math
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
    ap.add_argument("--merge", default="none", choices=["none", "regression", "improphe"])
    ap.add_argument("--zeit", default=r"C:\Users\eduar\Documents\dev\zeit")
    ap.add_argument("--jobs", type=int, default=JOBS_MAX)
    a = ap.parse_args()
    jobs = max(1, min(a.jobs, JOBS_MAX))
    for k in ("RAYON_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS", "GDAL_NUM_THREADS"):
        os.environ[k] = str(jobs)
    sys.path.insert(0, a.zeit)

    from zeit import _level2 as l2
    import zeit  # noqa: F401  (the .zeit accessor)

    merge = None if a.merge == "none" else a.merge
    folder = Path(a.out) / f"level2_{a.merge}"
    t0 = time.perf_counter()
    ds = l2.level2(a.safe, a.dem, n_jobs=jobs, merge=merge)
    print(f"level2 ({a.merge}): {time.perf_counter() - t0:.1f} s", flush=True)
    ds.zeit.save(folder)
    attrs = {k: ("NaN" if isinstance(v, float) and math.isnan(v) else v) for k, v in ds.attrs.items()}
    (folder / "attrs.json").write_text(json.dumps(attrs, indent=1))
    try:
        import psutil

        print("peak working set (GiB):", psutil.Process().memory_info().peak_wset / 2**30)
    except Exception:
        pass
    print("written to", folder)


if __name__ == "__main__":
    main()
