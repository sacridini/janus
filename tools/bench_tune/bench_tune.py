"""The tuning loop of Tune on samples in Python with zeit-cdts, against Janus's engine (see README.md).

Reads what the Rust benchmark wrote (`<tool>_input.json`, `<tool>_janus.json`, `<tool>_functions.py`), runs every
set's Zeit call on all the points, scores each set with the functions of the generated script (`<tool>_functions.py`:
the calendar year of the map's change from the Zeit result, and the engine's scores written in Python) and compares the
scores and the time with Janus's.

  python bench_tune.py DIR [--tools landtrendr bfast] [--mode threads|procs] [--workers 4]

`threads` (the loop of the generated script): the sets one after the other, Zeit's own `n_jobs=WORKERS` threads on the
points of each. `procs`: WORKERS processes, a share of the sets each, `n_jobs=1`. Either way WORKERS threads in all.
"""

import argparse
import json
import math
import multiprocessing as mp
import time

import numpy as np
import xarray as xr
import zeit

KEYS = ("omission", "commission", "f1", "f1_dated", "agreement", "error")

# what a worker process holds (set by `_init`)
_state = {}


def _namespace(tool, inp, folder):
    """The generated script's functions over this benchmark's points (its globals POINTS, YEARS...)."""
    ns = {
        "np": np,
        "zeit": zeit,
        "POINTS": [(0, 0, p["reference"], 1.0) for p in inp["points"]],
        "YEARS": tuple(inp["window"]),
        "TOLERANCE": inp["tolerance"],
        "WEIGHTED": False,
    }
    exec(open(f"{folder}/{tool}_functions.py", encoding="utf-8").read(), ns)
    return ns


def _cube(inp):
    """The points' series as a (time, 1, points) cube, as the generated script makes it."""
    values = np.array([[np.nan if v is None else v for v in p["values"]] for p in inp["points"]], dtype=float).T
    when = np.array(inp["days"], dtype="int64").astype("datetime64[D]").astype("datetime64[ns]")
    return xr.DataArray(values[:, None, :], dims=("time", "y", "x"), coords={"time": when})


def _kwargs(calls, n_jobs):
    """{function: keywords} of a set's calls, the values written as Python (what the script's BASE holds)."""
    kw = {c["func"]: {k: eval(v, {"float": float}) for k, v in c["kw"]} for c in calls}
    for k in kw.values():
        if "n_jobs" in k:
            k["n_jobs"] = n_jobs
    return kw


def _init(tool, folder, n_jobs):
    inp = json.load(open(f"{folder}/{tool}_input.json"))
    _state.update(ns=_namespace(tool, inp, folder), cube=_cube(inp), n_jobs=n_jobs)


def _one(calls):
    ns = _state["ns"]
    return ns["scores"](ns["map_years"](_state["cube"], _kwargs(calls, _state["n_jobs"])))


def _share(args):
    return [_one(c) for c in args]


def run(tool, folder, mode, workers):
    inp = json.load(open(f"{folder}/{tool}_input.json"))
    jan = json.load(open(f"{folder}/{tool}_janus.json"))
    calls = [s["calls"] for s in inp["sets"]]
    _init(tool, folder, workers if mode == "threads" else 1)
    _one(calls[0])  # a first call (imports, thread pool) outside the time
    t0 = time.perf_counter()
    if mode == "threads":
        rows = [_one(c) for c in calls]
    else:
        # the sets dealt out in turn, so the cheap and the dear ones spread over the processes
        shares = [calls[i::workers] for i in range(workers)]
        with mp.get_context("spawn").Pool(workers, initializer=_init, initargs=(tool, folder, 1)) as pool:
            done = pool.map(_share, shares)
        rows = [None] * len(calls)
        for i, share in enumerate(done):
            rows[i::workers] = share
    seconds = time.perf_counter() - t0
    worst, off, nan_mismatch = 0.0, 0, 0
    for mine, theirs in zip(rows, jan["scores"]):
        gap = 0.0
        for k in KEYS:
            a, b = mine[k], theirs[k]
            a_nan, b_nan = a is None or math.isnan(a), b is None or math.isnan(b)
            if a_nan != b_nan:
                nan_mismatch += 1
                gap = max(gap, 1.0)
            elif not a_nan:
                gap = max(gap, abs(a - b))
        worst = max(worst, gap)
        off += gap > 1e-12
    return dict(
        tool=tool,
        points=len(inp["points"]),
        sets=len(calls),
        mode=mode,
        workers=workers,
        python_seconds=seconds,
        janus_seconds=jan["seconds"],
        janus_threads=jan["threads"],
        max_abs_difference=worst,
        sets_that_differ=int(off),
        nan_mismatches=nan_mismatch,
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folder")
    ap.add_argument("--tools", nargs="+", default=["landtrendr", "bfast"])
    ap.add_argument("--mode", choices=["threads", "procs"], default="threads")
    ap.add_argument("--workers", type=int, default=4)
    a = ap.parse_args()
    for tool in a.tools:
        r = run(tool, a.folder, a.mode, a.workers)
        print(json.dumps(r))
        print(
            f"{r['tool']:>10}: {r['sets']} sets x {r['points']} points | Janus {r['janus_threads']} threads "
            f"{r['janus_seconds']:.2f} s | Python ({r['mode']}, {r['workers']}) {r['python_seconds']:.2f} s "
            f"= {r['python_seconds'] / r['janus_seconds']:.1f}x | max |score difference| {r['max_abs_difference']:.3g} "
            f"({r['sets_that_differ']} sets differ)"
        )


if __name__ == "__main__":
    main()
