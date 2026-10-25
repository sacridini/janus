"""The references of janus-zeit's BEAST tool (tests/beast.rs): zeit.beast on the NDVI series of the CCDC raster case
(tests/data/raster/ccdc-0: 16 x 10 pixels, 150 irregular dates, 2014-2020), with the tool's parameters, as its maps.

    python tools/beast_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import rasterio
import xarray as xr

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data"
# few samples: the test runs every pixel
CASES = [
    {"samples": 300, "chains": 1},
    {"season": "none", "deltat": 1 / 12, "tcp_max": 3, "samples": 300, "chains": 2, "seed": 7},
]
DEFAULTS = dict(season="auto", deltat=0.0, tcp_max=10, scp_max=10, torder_max=1, samples=2000, chains=2, burnin=200,
                thin=5, seed=42)


def main():
    case_dir = HERE / "raster" / "ccdc-0"
    spec = json.loads((case_dir / "spec.json").read_text())
    dates = np.datetime64("1970-01-01") + np.array(spec["days"], dtype="timedelta64[D]")
    with rasterio.open(case_dir / spec["input"]) as src:
        stack = src.read().astype(np.float64)
        if src.nodata is not None and not np.isnan(src.nodata):
            stack[stack == src.nodata] = np.nan
    cube = xr.DataArray(stack, dims=("time", "y", "x"), coords={"time": dates})
    out = {"input": "raster/ccdc-0/" + spec["input"], "cases": []}
    for params in CASES:
        p = dict(DEFAULTS, **params)
        season = "harmonic" if p["season"] == "auto" else p["season"]   # 23 dates a year
        res = zeit.beast(cube, season=season, deltat=p["deltat"] or None, tcp_minmax=(0, p["tcp_max"]),
                         scp_minmax=(0, p["scp_max"]), torder_minmax=(0, p["torder_max"]), samples=p["samples"],
                         chains=p["chains"], burnin=p["burnin"], thin=p["thin"], seed=p["seed"], nodata=None,
                         n_jobs=1)
        nan = np.full(res.r2.shape, np.nan)
        maps = {
            "trend_ncp": res.trend_ncp.values,
            "cp_date": res.trend_cp.isel(tcp=0).values,
            "cp_prob": res.trend_cp_prob.isel(tcp=0).values,
            "cp_abrupt": res.trend_cp_abrupt.isel(tcp=0).values,
            "season_ncp": res.season_ncp.values if "season_ncp" in res else nan,
            "r2": res.r2.values,
        }
        out["cases"].append({"params": params, "maps": {
            k: [None if not np.isfinite(x) else float(x) for x in np.asarray(v, dtype=np.float64).ravel()]
            for k, v in maps.items()}})
        print(params, "grid", res.sizes["time"], "mean trend changepoints", float(np.nanmean(res.trend_ncp)))
    (HERE / "beast.json").write_text(json.dumps(out))


if __name__ == "__main__":
    main()
