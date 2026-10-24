"""The references of janus-zeit's EWMACD tool (tests/ewmacd.rs): zeit.ewmacd on the series of the CCDC raster case
(tests/data/raster/ccdc-0: 150 irregular dates, 2014-2020), with the tool's parameters, as its maps.

    python tools/ewmacd_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import rasterio

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data"
CASES = [
    {},
    {"train_end": 2016, "lambda": 0.5, "persistence": 1},
    {"order": 1, "sigmas": 2.0, "xbar_monitor": 5.0, "persistence": 5},
]
DEFAULTS = dict(train_end=0, order=2, sigmas=3.0, persistence=3, xbar_train=1.5, xbar_monitor=20.0)
DEFAULTS["lambda"] = 0.3


def main():
    case_dir = HERE / "raster" / "ccdc-0"
    spec = json.loads((case_dir / "spec.json").read_text())
    dates = np.datetime64("1970-01-01") + np.array(spec["days"], dtype="timedelta64[D]")
    with rasterio.open(case_dir / spec["input"]) as src:
        stack = src.read().astype(np.float64)
        if src.nodata is not None:
            stack[stack == src.nodata] = np.nan
    first, last = int(str(dates[0])[:4]), int(str(dates[-1])[:4])
    out = {"input": "raster/ccdc-0/" + spec["input"], "cases": []}
    for params in CASES:
        p = dict(DEFAULTS, **params)
        end = p["train_end"] if p["train_end"] > 0 else min(first + 3, last)
        res = zeit.ewmacd(stack, dates=dates, train_end=end, order=p["order"], lmbda=p["lambda"], sigmas=p["sigmas"],
                          persistence=p["persistence"], xbar_train=p["xbar_train"], xbar_monitor=p["xbar_monitor"],
                          nodata=None, n_jobs=1)
        maps = {
            "break_date": res.break_time.values,
            "severity": res.severity.values,
            "recovery_date": res.recovery_time.values,
            "gain_date": res.gain_time.values,
            "state": res.state.values + 1,
            "last_flag": res.flags.values[-1],
            "sigma": res.sigma.values,
        }
        out["cases"].append({"params": params, "maps": {
            k: [None if not np.isfinite(x) else float(x) for x in np.asarray(v, dtype=np.float64).ravel()]
            for k, v in maps.items()}})
        print(params, "disturbed", int(np.isfinite(res.break_time.values).sum()), "of", res.break_time.size)
    (HERE / "ewmacd.json").write_text(json.dumps(out))


if __name__ == "__main__":
    main()
