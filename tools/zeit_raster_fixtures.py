"""The reference of Janus's native Zeit raster runs (crates/janus-zeit, job.rs): small GeoTIFF stacks
and job specs, run by the Python bridge (zeit_bridge/, run_raster) with Zeit's own Rust core; its
GeoTIFFs are kept next to them, and `cargo test -p janus-zeit --test raster` runs the native job on
the same spec and compares every pixel.

    python tools/zeit_raster_fixtures.py [--out DIR] [TOOL...]

DIR defaults to crates/janus-zeit/tests/data/raster; one folder per case (TOOL-N) with the inputs,
spec.json (paths relative to the folder) and expected/ (the bridge's outputs and result.json).
"""
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "zeit_bridge"))

import numpy as np  # noqa: E402
import rasterio  # noqa: E402
from rasterio.transform import from_origin  # noqa: E402

import janus_zeit_bridge as bridge  # noqa: E402
import zeit_fixtures as zf  # noqa: E402

sys.stdout = sys.__stdout__

H, W = 10, 16  # rows, columns of every case
TRANSFORM = from_origin(500000.0, 7500000.0, 30.0, 30.0)
CRS = "EPSG:32722"


def write_stack(path, stack, nodata=np.nan, dtype="float32"):
    """[T, H, W] as a GeoTIFF of T bands."""
    t = stack.shape[0]
    with rasterio.open(path, "w", driver="GTiff", width=W, height=H, count=t, dtype=dtype, crs=CRS,
                       transform=TRANSFORM, nodata=nodata, compress="deflate") as d:
        d.write(np.asarray(stack, dtype=dtype))


def grid(series):
    """H x W pixel series -> [T, H, W]."""
    return np.stack(series[:H * W], axis=1).reshape(-1, H, W)


def landtrendr_cases(rng):
    years, days = zf.annual()
    out = []
    for params in ({}, {"event_type": "gain", "sort_by": "newest"}, {"sort_by": "dsnr", "min_magnitude": 0.1}):
        series = zf.annual_series(rng, H * W)
        out.append({"params": params, "years": years, "days": days, "input": grid(series)})
    return out


def mann_kendall_cases(rng):
    years, days = zf.annual()
    out = [{"params": {}, "years": years, "days": days, "input": grid(zf.annual_series(rng, H * W))}]
    m_years, m_days = zf.monthly()
    out.append({"params": {"method": "seasonal"}, "years": m_years, "days": m_days,
                "input": grid(zf.monthly_series(rng, H * W))})
    return out


def bfast_family_cases(variants):
    def cases(rng):
        out = []
        for k, params in enumerate(variants):
            years, days = zf.regular(n=120) if k % 2 == 0 else zf.monthly(2012, 8)
            series = [zf.seasonal_breaks(rng, days, j) for j in range(H * W)]
            out.append({"params": params, "years": years, "days": days, "input": grid(series)})
        return out
    return cases


def landsat_grid(rng, n_dates=90):
    """H x W Landsat pixels on the same dates: (years, days, {role: [T, H, W]}, Fmask [T, H, W], NDVI shown)."""
    days = np.datetime64("2014-01-05") + np.arange(n_dates) * 16
    pixels = [zf.landsat_series(rng, days=days) for _ in range(H * W)]
    years, day_numbers = pixels[0][0], pixels[0][1]
    bands = {r: grid([px[2][r] for px in pixels]) for r in pixels[0][2]}
    return years, day_numbers, bands, grid([px[3] for px in pixels]), grid([px[4] for px in pixels])


def multiband_cases(roles, variants):
    def cases(rng):
        out = []
        for k, params in enumerate(variants):
            years, days, bands, qa, shown = landsat_grid(rng)
            case = {"params": params, "years": years, "days": days, "input": shown,
                    "bands": {r: bands[r] for r in roles}, "qa_input": qa, "qa_rule": "fmask",
                    "shown": {"band": None, "nd": ["nir", "red"]}}
            if k % 3 == 1:  # reflectance 0-1
                case["bands"] = {r: v / 10000.0 for r, v in case["bands"].items()}
            if k % 3 == 2:  # Landsat C2 digital numbers with QA_PIXEL bits
                case["bands"] = {r: (v / 10000.0 + 0.2) / 2.75e-5 for r, v in case["bands"].items()}
                case["qa_input"], case["qa_rule"] = zf.landsat_c2_qa(qa), "landsat_c2"
            out.append(case)
        return out
    return cases


CASES = {
    "coded": multiband_cases(zf.zc.LANDSAT_ROLES, [{}, {"direction": "both", "thresh": 2.0}, {"monitor_start": 2016.0}]),
    "ndfi": multiband_cases(zf.zc.LANDSAT_ROLES, [{}, {"fractions": True, "cloud_threshold": 1.0}, {"cloud_threshold": 0.2}]),
    "tmask": multiband_cases(["green", "swir1"], [{}, {"write_series": False}, {}]),
    "landtrendr": landtrendr_cases,
    "mann_kendall": mann_kendall_cases,
    "bfast": bfast_family_cases([{}, {"h": 0.25, "max_breaks_trend": 2}]),
    "bfast_lite": bfast_family_cases([{}, {"max_breaks": 2, "order": 2}]),
    "bfast_monitor": bfast_family_cases([{}, {"monitor_start": 2016.5, "h": "0.5"}]),
}


def main():
    args = sys.argv[1:]
    out_dir = os.path.join(HERE, "..", "crates", "janus-zeit", "tests", "data", "raster")
    if args[:1] == ["--out"]:
        out_dir, args = args[1], args[2:]
    unknown = [a for a in args if a not in CASES]
    if unknown:
        sys.exit(f"no raster cases for {unknown}; tools: {sorted(CASES)}")
    tools = bridge.tools()
    for tool in args or list(CASES):
        rng = np.random.default_rng(20261022)
        for k, case in enumerate(CASES[tool](rng)):
            d = os.path.join(out_dir, f"{tool}-{k}")
            shutil.rmtree(d, ignore_errors=True)
            os.makedirs(os.path.join(d, "expected"))
            write_stack(os.path.join(d, "input.tif"), case["input"])
            spec = {"tool": tool, "params": case["params"], "input": "input.tif", "nodata": None,
                    "years": case["years"], "days": case["days"], "window": [0, 0, W, H], "threads": 1}
            for key in ("shown", "map_year", "labels"):
                if key in case:
                    spec[key] = case[key]
            for key, name in (("nd_input", "nd.tif"), ("qa_input", "qa.tif")):
                if key in case:
                    write_stack(os.path.join(d, name), case[key])
                    spec[key] = name
            if "qa_rule" in case:
                spec["qa_rule"] = case["qa_rule"]
            if "bands" in case:
                spec["bands"] = {}
                for role, stack in case["bands"].items():
                    write_stack(os.path.join(d, f"band_{role}.tif"), stack)
                    spec["bands"][role] = f"band_{role}.tif"
            with open(os.path.join(d, "spec.json"), "w", encoding="utf-8", newline="\n") as f:
                json.dump(spec, f, indent=1)
            # the bridge, on absolute paths
            run = dict(spec, output_dir=os.path.join(d, "expected"))
            for key in ("input", "nd_input", "qa_input"):
                if key in run:
                    run[key] = os.path.join(d, run[key])
            if "bands" in run:
                run["bands"] = {r: os.path.join(d, p) for r, p in run["bands"].items()}
            entry = tools[tool]
            p = bridge.defaults(entry["manifest"], spec["params"])
            try:
                result = bridge.run_raster(entry, p, run, lambda f, m="": None)
                for o in result["outputs"]:
                    o["path"] = os.path.basename(o["path"])
            except Exception as e:  # the native run must fail too
                result = {"error": f"{type(e).__name__}: {e}"}
            with open(os.path.join(d, "expected", "result.json"), "w", encoding="utf-8", newline="\n") as f:
                json.dump(result, f, indent=1)
            print(f"{tool}-{k}: {len(result.get('outputs', []))} outputs" + (f" ({result['error']})" if "error" in result else ""))


if __name__ == "__main__":
    main()
