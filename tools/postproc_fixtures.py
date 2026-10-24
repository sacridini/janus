"""The references of janus-zeit's post-processing tool (tests/postproc.rs): a class series made here (blocks of four
classes over seven years, salt-and-pepper, one-year excursions, a corner without data), and Zeit's post-processing of
it with the tool's parameters: zeit.desawtooth_classes, then each year's map through zeit.apply_majority_filter and
zeit.mmu_sieve.

    python tools/postproc_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import rasterio
from rasterio.transform import from_origin

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data"
CASES = [
    {},
    {"max_length": 2, "majority": 5, "iterations": 2, "min_size": 12, "connectivity": "4"},
    {"max_length": 0, "majority": 0, "min_size": 30},
    {"majority": 0, "min_size": 0},
]
DEFAULTS = dict(max_length=1, majority=3, iterations=1, min_size=6, connectivity="8")
NODATA = 0


def series():
    rng = np.random.default_rng(11)
    t, h, w = 7, 40, 48
    base = np.kron(rng.integers(1, 5, (5, 6)), np.ones((8, 8), dtype=int))
    stack = np.repeat(base[None], t, axis=0)
    # a clearing from the fourth year on
    stack[3:, 10:22, 14:30] = 4
    # one-year excursions
    for _ in range(60):
        i, j = rng.integers(0, h), rng.integers(0, w)
        k = rng.integers(1, t - 1)
        stack[k, i, j] = rng.integers(1, 5)
    noise = rng.random(stack.shape) < 0.06
    stack[noise] = rng.integers(1, 5, noise.sum())
    stack[:, :5, :6] = NODATA
    return stack.astype(np.int16)


def main():
    stack = series()
    t, h, w = stack.shape
    out_dir = HERE / "postproc"
    out_dir.mkdir(exist_ok=True)
    path = out_dir / "classes.tif"
    with rasterio.open(path, "w", driver="GTiff", width=w, height=h, count=t, dtype="int16", nodata=NODATA,
                       crs="EPSG:32722", transform=from_origin(500000, 9000000, 30, 30)) as dst:
        dst.write(stack)
    days = [int((np.datetime64(f"{2015 + k}-07-01") - np.datetime64("1970-01-01")).astype(int)) for k in range(t)]
    years = [2015 + k + 181 / 365 for k in range(t)]
    cases = []
    values = np.where(stack == NODATA, np.nan, stack.astype(np.float64))
    for params in CASES:
        p = dict(DEFAULTS, **params)
        res = zeit.desawtooth_classes(values, max_length=p["max_length"]) if p["max_length"] > 0 else values.copy()
        maps = []
        for m in res:
            if p["majority"] >= 2:
                m = zeit.apply_majority_filter(m, p["majority"] | 1, iterations=p["iterations"])
            if p["min_size"] >= 2:
                m = zeit.mmu_sieve(m, p["min_size"], connectivity=int(p["connectivity"]))
            maps.append(m)
        res = np.stack(maps)
        changed = np.where(np.isnan(values).all(axis=0), np.nan,
                           ((res != values) & ~np.isnan(values)).sum(axis=0).astype(float))
        cases.append({"params": params,
                      "classes": [None if np.isnan(v) else int(v) for v in res.ravel()],
                      "changed": [None if np.isnan(v) else int(v) for v in changed.ravel()]})
        print(params, "values changed:", int(np.nansum(changed)))
    (out_dir / "postproc.json").write_text(json.dumps(
        {"input": "postproc/classes.tif", "nodata": NODATA, "years": years, "days": days, "cases": cases}))


if __name__ == "__main__":
    main()
