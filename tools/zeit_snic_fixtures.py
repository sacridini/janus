"""The reference of Janus's native SNIC (crates/janus-zeit, snic.rs): Zeit's seed grids
(zeit.segmentation.snic_grid) and its segmentation (run_snic, zeit-core's snic::segment) of small
made-up series, written as fixtures that `cargo test -p janus-zeit --test snic` compares with the
native runs.

    python tools/zeit_snic_fixtures.py [--out DIR]

DIR defaults to crates/janus-zeit/tests/data/snic: grids.json (the seeds of every grid), and one folder
per run (snic-N) with the inputs, spec.json (paths relative to the folder) and expected.json (the
labels, sizes, centroids and means of zeit's run on the same features). The features are what Janus
compares: the shown series or every band of every date, the gaps filled as the bridge's SOM fills them
(tool_som.fill_gaps), as float32. Needs a Python with Zeit >= 2.0 (numpy, rasterio).
"""
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "zeit_bridge"))

import numpy as np  # noqa: E402
import rasterio  # noqa: E402
from rasterio.transform import from_origin  # noqa: E402

from zeit.segmentation import run_snic, snic_grid  # noqa: E402

import tool_som  # noqa: E402
import zeit_fixtures as zf  # noqa: E402

sys.stdout = sys.__stdout__

H, W, T = 40, 56, 12
TRANSFORM = from_origin(500000.0, 7500000.0, 30.0, 30.0)
CRS = "EPSG:32722"
DEFAULTS = {"features": "shown", "spacing": 10, "grid": "rectangular", "compactness": 0.5, "padding": 0.5,
            "tile": 1024, "gaps": "interpolate", "min_valid": 50.0, "seed": 42, "polygons": True}


def grid_cases():
    out = []
    for h, w, spacing, padding, kind, seed in [
        (40, 56, (10, 10), None, "rectangular", 0),
        (40, 56, (10, 10), (0, 0), "rectangular", 0),
        (37, 23, (7.5, 4), (2.25, 1), "rectangular", 0),
        (40, 56, (10, 10), None, "diamond", 0),
        (101, 77, (6, 9), (3, 0), "diamond", 0),
        (40, 56, (10, 10), None, "hexagonal", 0),
        (64, 64, (5, 5), (2.5, 2.5), "hexagonal", 0),
        (40, 56, (10, 10), None, "random", 42),
        (40, 56, (3, 3), (0, 0), "random", 7),       # Floyd's algorithm
        (150, 200, (2.5, 2.5), (1, 1), "random", 3),  # a shuffle of every position
        (12, 12, (20, 20), (0, 0), "rectangular", 0),  # one seed, in the middle
        (10, 10, (10, 10), (5, 5), "rectangular", 0),  # no room
        (10, 10, (1, 1), None, "rectangular", 0),      # spacing <= 1
    ]:
        case = {"h": h, "w": w, "spacing": list(map(float, spacing)),
                "padding": None if padding is None else list(map(float, padding)), "type": kind, "seed": seed}
        try:
            case["seeds"] = snic_grid((h, w), spacing, padding=padding, type=kind, random_state=seed).tolist()
        except ValueError as e:
            case["error"] = str(e)
        out.append(case)
    return out


def fields(rng, n_bands):
    """[n_bands, T, H, W] series of a few fields (Voronoi cells, each a profile), noisy, with gaps and a
    block without any value."""
    centres = rng.uniform([0, 0], [H, W], (9, 2))
    yy, xx = np.mgrid[0:H, 0:W]
    cell = np.argmin((yy[None] - centres[:, 0, None, None]) ** 2 + (xx[None] - centres[:, 1, None, None]) ** 2, axis=0)
    t = np.arange(T)
    out = np.empty((n_bands, T, H, W))
    for b in range(n_bands):
        prof = 0.2 + 0.6 * rng.random((9, 1)) * (0.5 + 0.5 * np.sin(2 * np.pi * t / T + rng.uniform(0, 6, (9, 1))))
        out[b] = prof[cell].transpose(2, 0, 1) + rng.normal(0, 0.03, (T, H, W)) + 0.05 * b
    gaps = rng.random((T, H, W)) < 0.12
    out[:, gaps] = np.nan
    out[:, :, 30:36, 4:12] = np.nan  # no value: left out
    return out


def write_stack(path, stack):
    with rasterio.open(path, "w", driver="GTiff", width=W, height=H, count=stack.shape[0], dtype="float32", crs=CRS,
                       transform=TRANSFORM, nodata=np.nan, compress="deflate") as d:
        d.write(stack.astype(np.float32))


def features(p, shown, bands, years, window):
    """What Janus segments: [F, h, w] float32 of the window."""
    x0, y0, x1, y1 = window
    x = np.asarray(years, dtype=np.float64)

    def filled(s):  # [T, H, W] (float32 values) -> [T, h, w], gaps filled
        s = s.astype(np.float32).astype(np.float64)[:, y0:y1, x0:x1]
        return tool_som.fill_gaps(s.reshape(T, -1), x, p).reshape(T, y1 - y0, x1 - x0)

    if p["features"] == "bands":
        f = np.stack([filled(b) for b in bands], axis=1)  # [T, B, h, w]: date by date, the bands of a date together
        return f.reshape(-1, y1 - y0, x1 - x0).astype(np.float32), filled(shown).astype(np.float32)
    f = filled(shown).astype(np.float32)
    return f, f


def run_cases(rng):
    years, days = zf.regular(first="2020-01-01", n=T)
    out = []
    for params, window in [
        ({}, [0, 0, W, H]),
        ({"grid": "hexagonal", "compactness": 0.1, "tile": 16, "gaps": "skip", "spacing": 6}, [0, 0, W, H]),
        ({"features": "bands", "grid": "random", "seed": 7, "tile": 20, "compactness": 1.0}, [5, 3, 50, 37]),
        ({"grid": "diamond", "padding": 0.0, "spacing": 8, "tile": 0, "min_valid": 80.0}, [0, 0, W, H]),
    ]:
        p = dict(DEFAULTS, **params)
        bands = fields(rng, 3)
        shown = bands[0] if p["features"] == "shown" else bands[2] - bands[1]
        f, shown_f = features(p, shown, bands, years, window)
        x0, y0, x1, y1 = window
        h, w = y1 - y0, x1 - x0
        s = float(p["spacing"])
        seeds = snic_grid((h, w), s, padding=p["padding"] * s, type=p["grid"], random_state=p["seed"])
        th, tw = (h, w) if p["tile"] == 0 else (min(p["tile"], h), min(p["tile"], w))
        res = run_snic(f, seeds=seeds, compactness=p["compactness"], tile_size=(th, tw))
        # the shown series' mean of each segment, over the pixels with a value on each date
        lab = res.labels.ravel()
        sm = np.full((len(seeds), T), np.nan)
        flat = shown_f.reshape(T, -1).astype(np.float64)
        for k in np.unique(lab[lab >= 0]):
            sm[k] = np.nanmean(flat[:, lab == k], axis=1)
        out.append({"params": params, "window": window, "years": years, "days": days, "shown": shown,
                    "bands": bands if p["features"] == "bands" else None,
                    "expected": {"labels": res.labels.ravel().tolist(), "sizes": res.sizes.tolist(),
                                 "centroids": res.centroids.ravel().tolist(),
                                 "means": res.means.reshape(len(seeds), -1).ravel().tolist(),
                                 "shown_means": sm.ravel().tolist()}})
    return out


def main():
    out = os.path.join(HERE, "..", "crates", "janus-zeit", "tests", "data", "snic")
    if "--out" in sys.argv:
        out = sys.argv[sys.argv.index("--out") + 1]
    out = os.path.abspath(out)
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    with open(os.path.join(out, "grids.json"), "w") as f:
        json.dump(zf.jsonable(grid_cases()), f)
    rng = np.random.default_rng(20261022)
    for k, case in enumerate(run_cases(rng)):
        d = os.path.join(out, f"snic-{k}")
        os.makedirs(d)
        write_stack(os.path.join(d, "input.tif"), case["shown"])
        spec = {"tool": "snic", "params": case["params"], "input": "input.tif", "nodata": None,
                "years": case["years"], "days": case["days"],
                "labels": [str(np.datetime64(int(v), "D")) for v in case["days"]], "window": case["window"]}
        if case["bands"] is not None:
            spec["bands"], spec["band_names"] = {}, {}
            for b, stack in enumerate(case["bands"]):
                write_stack(os.path.join(d, f"b{b + 1}.tif"), stack)
                spec["bands"][f"b{b + 1}"] = f"b{b + 1}.tif"
                spec["band_names"][f"b{b + 1}"] = ["blue", "red", "nir"][b]
        with open(os.path.join(d, "spec.json"), "w") as f:
            json.dump(spec, f, indent=1)
        with open(os.path.join(d, "expected.json"), "w") as f:
            json.dump(zf.jsonable(case["expected"]), f)
        print(f"snic-{k}: {sum(1 for s in case['expected']['sizes'] if s > 0)} segments")


if __name__ == "__main__":
    main()
