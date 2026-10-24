"""The references of janus-zeit's recovery tool (tests/recovery.rs): zeit.recovery_metrics on zeit.landtrendr of
the LandTrendr raster case (tests/data/raster/landtrendr-0), with the tool's parameters, as float32 maps.

    python tools/recovery_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import rasterio

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data"
CASES = [
    {},
    {"basis": "magnitude", "target": 0.6, "horizon": 3, "pre_years": 1, "sort_by": "newest"},
    {"event_type": "gain", "basis": "magnitude"},
]
DEFAULTS = dict(event_type="loss", max_segments=6, pval_threshold=0.05, recovery_threshold=0.25, spike_threshold=0.9,
                best_model_proportion=0.75, vertex_count_overshoot=3, min_observations_needed=6, sort_by="greatest",
                min_magnitude=0.0, min_duration=1, pre_val_threshold=0.0, target=0.8, horizon=5, pre_years=2,
                basis="pre_value")
OUTPUTS = ["y2r", "r80p", "rri", "ari", "yryr", "magnitude", "pre_val", "yod"]


def main():
    case_dir = HERE / "raster" / "landtrendr-0"
    spec = json.loads((case_dir / "spec.json").read_text())
    years = [int(y) for y in spec["years"]]
    with rasterio.open(case_dir / "input.tif") as src:
        stack = src.read().astype(np.float64)
        if src.nodata is not None:
            stack[stack == src.nodata] = np.nan
    out = {"input": "raster/landtrendr-0/input.tif", "years": years, "cases": []}
    for params in CASES:
        p = dict(DEFAULTS, **params)
        lt = zeit.landtrendr(stack, years=years, nodata=None, n_jobs=1, direction=p["event_type"],
                             max_segments=p["max_segments"], pval_threshold=p["pval_threshold"],
                             recovery_threshold=p["recovery_threshold"], spike_threshold=p["spike_threshold"],
                             best_model_proportion=p["best_model_proportion"],
                             vertex_count_overshoot=p["vertex_count_overshoot"],
                             min_observations_needed=p["min_observations_needed"])
        rec = zeit.recovery_metrics(lt, event_type=p["event_type"], sort_by=p["sort_by"],
                                    min_magnitude=p["min_magnitude"], min_duration=p["min_duration"],
                                    pre_val_threshold=p["pre_val_threshold"], target=p["target"], horizon=p["horizon"],
                                    pre_years=p["pre_years"], basis=p["basis"], n_jobs=1)
        maps = {}
        for name in OUTPUTS:
            v = np.asarray(rec[name].values, dtype=np.float32)
            if name == "yod":
                v = np.where(v > 0, v, np.nan).astype(np.float32)
            maps[name] = [None if not np.isfinite(x) else float(x) for x in v.ravel()]
        out["cases"].append({"params": params, "maps": maps})
    (HERE / "recovery.json").write_text(json.dumps(out))
    print(f"{len(CASES)} cases, {stack.shape[1] * stack.shape[2]} pixels")


if __name__ == "__main__":
    main()
