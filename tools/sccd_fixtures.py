"""The references of janus-zeit's S-CCD tool (tests/sccd.rs): zeit.sccd on the bands and Fmask codes of the CCDC
raster case (tests/data/raster/ccdc-0: 16 x 10 pixels, 150 irregular dates, 2014-2020, reflectance x 10000), with the
tool's parameters, as its maps.

    python tools/sccd_fixtures.py
"""

import json
from pathlib import Path

import numpy as np
import pandas as pd
import rasterio
import xarray as xr

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data"
ROLES = ["blue", "green", "red", "nir", "swir1", "swir2"]
CASES = [
    {},
    {"conse": 4, "prob_threshold": 0.95},
    {"conse": 8, "outlier_prob": 0.999, "max_angle": 60.0, "max_segments": 3},
]
DEFAULTS = dict(conse=6, prob_threshold=0.99, outlier_prob=0.99999, max_angle=30.0, max_segments=6)


def decimal_year(t):
    """Janus's decimal year of a date (numeric::decimal_year), NaN for NaT."""
    t = pd.Timestamp(t)
    if pd.isna(t):
        return np.nan
    start = pd.Timestamp(t.year, 1, 1)
    return t.year + (t - start).days / (366.0 if t.is_leap_year else 365.0)


def read(path):
    with rasterio.open(path) as src:
        a = src.read().astype(np.float64)
        if src.nodata is not None and not np.isnan(src.nodata):
            a[a == src.nodata] = np.nan
    return a


def main():
    case_dir = HERE / "raster" / "ccdc-0"
    spec = json.loads((case_dir / "spec.json").read_text())
    dates = np.datetime64("1970-01-01") + np.array(spec["days"], dtype="timedelta64[D]")
    bands = np.stack([read(case_dir / spec["bands"][r]) for r in ROLES], axis=1)  # (time, band, y, x)
    q = read(case_dir / spec["qa_input"])
    fmask = np.where(np.isfinite(q) & (q >= 0), q, 255).astype(np.int32)
    fmask[~np.all(np.isfinite(bands), axis=1)] = 255
    cube = xr.DataArray(bands, dims=("time", "band", "y", "x"), coords={"time": dates, "band": ROLES})
    out = {"cases": []}
    for params in CASES:
        p = dict(DEFAULTS, **params)
        res = zeit.sccd(cube, qa=fmask, max_segments=p["max_segments"], conse=p["conse"],
                        prob_threshold=p["prob_threshold"], outlier_prob=p["outlier_prob"],
                        max_angle=p["max_angle"], nodata=None, n_jobs=1)
        h, w = res.n_breaks.shape
        maps = {k: np.full((h, w), np.nan) for k in
                ("n_breaks", "first_break", "last_break", "largest_break", "magnitude", "state", "n_anomalies",
                 "anomaly_start")}
        mag = res.magnitude.values.astype(np.float64)  # segment, band, y, x
        for y in range(h):
            for x in range(w):
                count = int(res.n_segments.values[y, x])
                mode = int(res.nrt_mode.values[y, x])
                anomalies = int(res.n_anomalies.values[y, x])
                maps["state"][y, x] = (2 if anomalies > 0 else 1) if mode == 1 else 3 if mode == 2 and count > 0 else 4
                if mode == 1:
                    maps["n_anomalies"][y, x] = res.n_anomalies.values[y, x]
                    if res.n_anomalies.values[y, x] > 0:
                        maps["anomaly_start"][y, x] = decimal_year(res.anomaly_start.values[y, x])
                if count == 0:
                    continue
                brk = [(decimal_year(res.t_break.values[k, y, x]), float(np.sqrt(np.sum(mag[k, 1:6, y, x] ** 2))))
                       for k in range(count) if not pd.isna(res.t_break.values[k, y, x])]
                maps["n_breaks"][y, x] = len(brk)
                if brk:
                    maps["first_break"][y, x] = brk[0][0]
                    maps["last_break"][y, x] = brk[-1][0]
                    big = brk[0]
                    for b in brk[1:]:
                        if b[1] > big[1]:
                            big = b
                    maps["largest_break"][y, x], maps["magnitude"][y, x] = big
        out["cases"].append({"params": params, "maps": {
            k: [None if not np.isfinite(v) else float(v) for v in m.ravel()] for k, m in maps.items()}})
        print(params, "breaks", int(np.nansum(maps["n_breaks"])), "monitoring",
              int((maps["state"] <= 2).sum()), "with anomalies", int((maps["state"] == 2).sum()))
    (HERE / "sccd.json").write_text(json.dumps(out))


if __name__ == "__main__":
    main()
