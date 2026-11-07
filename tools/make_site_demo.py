#!/usr/bin/env python
"""Build the data of the interactive LandTrendr demo of the Janus website.

Usage (in an environment with zeit, rasterio, numpy and Pillow)::

    python -I tools/make_site_demo.py LT_Stack_NDVI_Rondonia.tif site/assets/demo [--window XOFF YOFF]

The input is the 40-band annual NDVI stack of a forest frontier in Rondonia, Brazil
(bands yr1985..yr2024, Int16 NDVI x 10000, 0 = no data, EPSG:4326). The script crops a
256 x 256 window at full resolution (chosen automatically around the clearing pin of the
guide unless --window is given), runs ``zeit.landtrendr`` on it at Zeit's defaults and writes,
in the output folder, everything the page needs, so that the page shows Zeit's real results
and reimplements nothing:

``ndvi.png``
    Grayscale atlas of 8 x 5 tiles of 256 x 256 px (2048 x 1280); year 1985 + i sits at
    column i % 8, row i // 8. Value 0 = no data, 1..255 = NDVI from -0.2 to 1.0 (``meta.json``).
``lt.bin``
    Little-endian, pixel by pixel in row-major order, 14 bytes per pixel: up to 7 vertices
    of the LandTrendr fit as (year - 1985, quantized value) uint8 pairs, the same value
    mapping as the atlas; both bytes 255 for the vertices that do not exist.
``yod.png``, ``mag.png``
    Greatest-loss event per pixel (``zeit.extract_events``, min magnitude 0.25 and pre-event
    value 0.75 in NDVI, i.e. 2500 and 7500 in the raw units the data are segmented in):
    yod = year of the vertex where the loss starts - 1985, 255 = no event; magnitude =
    round(magnitude / 10000 * 255), 0 = no event (mapping in ``meta.json``).
``grid.json``
    Six chosen pixels, each with its 40-year series and the LandTrendr fit for 54 parameter
    combinations (max_segments x spike_threshold x recovery_threshold), run by Zeit on the
    pixel's series, as ``fits["<max_segments>_<spike>_<recovery>"] = {vertices, rmse}``.
``meta.json``
    Years, bounds, pixel size, mappings, encodings, Zeit version and commit, parameters.

NDVI drops with vegetation loss, so LandTrendr runs with ``direction="loss"`` (Zeit's
default and documented use for NDVI); the data are segmented in their raw x 10000 scale and
the outputs are divided by 10000 for the page. 0 is the no-data value of the stack.
"""

import argparse
import itertools
import json
import os
import subprocess
import time
from pathlib import Path

for _v in ("OMP_NUM_THREADS", "RAYON_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS",
           "NUMBA_NUM_THREADS", "GDAL_NUM_THREADS"):
    os.environ.setdefault(_v, "4")

import warnings

import numpy as np
import rasterio
from PIL import Image
from rasterio.windows import Window

warnings.filterwarnings("ignore", message="Mean of empty slice")
warnings.filterwarnings("ignore", message="Degrees of freedom")
N_JOBS = 4
SIZE = 256
YEAR0 = 1985
NYEARS = 40
VMIN, VMAX = -0.2, 1.0
MAX_VERT = 7
MAG_SCALE = 10000.0  # raw magnitude that maps to 255 in mag.png
PINS = {
    "clearing": (-63.083966, -10.356003),
    "regrowth": (-63.344298, -10.220986),
    "forest": (-63.202544, -10.326628),
}
MIN_MAG, PRE_VAL = 2500, 7500
GRID_MS = [1, 2, 3, 4, 5, 6]
GRID_SPIKE = [0.75, 0.9, 1.0]
GRID_REC = [0.25, 0.5, 1.0]


def _zeit_version():
    from importlib.metadata import PackageNotFoundError, version

    # An editable install keeps the metadata of when it was installed: the
    # checkout's pyproject.toml says which version the code really is.
    import re
    import zeit

    pyproject = Path(zeit.__file__).resolve().parent.parent / "pyproject.toml"
    if pyproject.is_file():
        m = re.search(r'^version\s*=\s*"([^"]+)"', pyproject.read_text(encoding="utf-8"), re.M)
        if m:
            return m.group(1)
    for name in ("zeit-cdts", "zeit"):
        try:
            return version(name)
        except PackageNotFoundError:
            pass
    return None


def _zeit_commit(repo):
    """Short commit of the Zeit checkout (git if it is on PATH, else read .git/HEAD)."""
    try:
        return subprocess.run(["git", "-C", str(repo), "rev-parse", "--short", "HEAD"],
                              capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        pass
    try:
        head = (repo / ".git" / "HEAD").read_text().strip()
        if head.startswith("ref:"):
            ref = repo / ".git" / head.split(" ", 1)[1]
            if ref.exists():
                return ref.read_text().strip()[:7]
            for line in (repo / ".git" / "packed-refs").read_text().splitlines():
                if line.endswith(head.split(" ", 1)[1]):
                    return line[:7]
        return head[:7]
    except OSError:
        return None


def quantize(ndvi):
    """NDVI (float, NaN = missing) to uint8: 0 = no data, 1..255 = VMIN..VMAX."""
    q = np.rint((np.clip(ndvi, VMIN, VMAX) - VMIN) / (VMAX - VMIN) * 254.0) + 1
    return np.where(np.isfinite(ndvi), q, 0).astype(np.uint8)


def dequantize(q):
    return (q.astype(np.float64) - 1.0) / 254.0 * (VMAX - VMIN) + VMIN


def save_png(path, arr):
    Image.fromarray(arr, mode="L").save(path, optimize=True, compress_level=9)


def choose_window(src, pin, size=SIZE, step=32, margin=24):
    """Window containing the pin that maximises the variety of the year of the largest drop.

    Score = entropy of the year of the largest year-to-year NDVI drop (pixels with a drop of
    at least 0.25) x a balance term between pixels with and without such a drop x the share
    of valid data.
    """
    col, row = ~src.transform * pin
    col, row = int(col), int(row)
    xs = [x for x in range(col - size + margin, col - margin + 1, step) if 0 <= x <= src.width - size]
    ys = [y for y in range(row - size + margin, row - margin + 1, step) if 0 <= y <= src.height - size]
    best = None
    scores = []
    for xo in xs:
        for yo in ys:
            a = src.read(window=Window(xo, yo, size, size)).astype(np.float32) / 10000.0
            valid = a != 0
            d = a[1:] - a[:-1]
            d[~(valid[1:] & valid[:-1])] = 0
            idx = d.argmin(0)
            big = d.min(0) <= -0.25
            f = big.mean()
            hist = np.bincount(idx[big], minlength=NYEARS - 1).astype(float)
            p = hist / max(hist.sum(), 1.0)
            h = -(p[p > 0] * np.log2(p[p > 0])).sum()
            score = h * np.sqrt(4 * f * (1 - f)) * valid.mean()
            scores.append((xo, yo, round(float(score), 3), round(float(f), 3), round(float(h), 2)))
            if best is None or score > best[0]:
                best = (score, xo, yo)
    return best[1], best[2], scores


def events(lt, zeit, min_magnitude, pre_val):
    return zeit.extract_events(lt, event_type="loss", sort_by="greatest", min_magnitude=min_magnitude,
                               pre_val_threshold=pre_val)


def choose_pixels(raw, ev, lowev, pin_xy):
    """Six pixels with a story each: the clearing pin plus rule-picked ones. Returns [(role, x, y)]."""
    yod = ev.yod.values.astype(int)
    mag = ev.magnitude.values
    post = ev.post_val.values
    nobs = (raw != 0).sum(0)
    ndvi = raw.astype(float) / 10000.0
    last3 = np.where(raw[-3:] != 0, ndvi[-3:], np.nan)
    end = np.nanmean(last3, 0)
    complete = nobs >= NYEARS - 1
    chosen = [("clearing_pin", *pin_xy)]
    taken = np.zeros(yod.shape, bool)

    def far(x, y, d=12):
        return all(abs(x - cx) + abs(y - cy) > d for _, cx, cy in chosen)

    def pick(role, score):
        s = np.where(taken | ~np.isfinite(score), -np.inf, score)
        for _ in range(2000):
            y, x = np.unravel_index(np.argmax(s), s.shape)
            if not np.isfinite(s[y, x]):
                return
            if far(x, y):
                chosen.append((role, int(x), int(y)))
                return
            s[y, x] = -np.inf

    old_regrew = (yod >= 1986) & (yod <= 1998) & complete & (end >= 0.7)
    pick("old_clearing_regrown", np.where(old_regrew, mag - 0 * post, np.nan))
    recent = (yod >= 2018) & complete
    pick("recent_clearing", np.where(recent, mag, np.nan))
    mid = (yod >= 2006) & (yod <= 2014) & complete & (end <= 0.55)
    pick("mid_period_clearing", np.where(mid, mag, np.nan))
    lm = lowev.magnitude.values
    ly = lowev.yod.values.astype(int)
    degr = (ly > 0) & (yod == 0) & complete & (lm >= 1200) & (end >= 0.6)
    pick("degraded_patch", np.where(degr, lm, np.nan))
    # Stable forest: no event, high and flat NDVI, complete series (lowest standard deviation).
    sd = np.nanstd(np.where(raw != 0, ndvi, np.nan), 0)
    stable = (ly == 0) & complete & (np.nanmean(np.where(raw != 0, ndvi, np.nan), 0) >= 0.85)
    pick("stable_forest", np.where(stable, -sd, np.nan))
    return chosen


def label_of(role, series, yod, pre, post, end):
    s = np.asarray(series, float)
    if role == "stable_forest":
        return f"Forest all along: NDVI stays near {np.nanmean(s):.2f}"
    yr = int(yod) if yod else None
    if role == "clearing_pin":
        return f"Guide's clearing pin: NDVI falls from {pre:.2f} to {post:.2f} (loss starting after {yr}), {end:.2f} by 2024"
    if role == "old_clearing_regrown":
        return f"Early clearing after {yr} (NDVI {pre:.2f} to {post:.2f}) that regrows to {end:.2f} by 2024"
    if role == "recent_clearing":
        return f"Recent clearing after {yr}: NDVI falls from {pre:.2f} to {post:.2f}"
    if role == "mid_period_clearing":
        return f"Clearing after {yr}: NDVI falls from {pre:.2f} to {post:.2f} and stays low until 2024 ({end:.2f})"
    if role == "degraded_patch":
        return f"Partial loss after {yr}: NDVI falls from {pre:.2f} to {post:.2f}, not a full clearing"
    return role


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", help="LT_Stack_NDVI_Rondonia.tif")
    ap.add_argument("output", help="output folder (site/assets/demo)")
    ap.add_argument("--window", nargs=2, type=int, metavar=("XOFF", "YOFF"),
                    help="top-left pixel of the 256 x 256 crop (default: chosen around the clearing pin)")
    args = ap.parse_args()
    t0 = time.time()
    out = Path(args.output)
    out.mkdir(parents=True, exist_ok=True)

    import zeit

    with rasterio.open(args.input) as src:
        years = [int(d[2:]) for d in src.descriptions]
        assert years == list(range(YEAR0, YEAR0 + NYEARS)), src.descriptions
        pin_px = {k: tuple(int(v) for v in ~src.transform * ll) for k, ll in PINS.items()}
        if args.window:
            xo, yo = args.window
        else:
            xo, yo, scores = choose_window(src, PINS["clearing"])
            print("window scores (xoff, yoff, score, share with big drop, entropy):")
            for s in sorted(scores, key=lambda r: -r[2])[:8]:
                print("  ", s)
        raw = src.read(window=Window(xo, yo, SIZE, SIZE))
        tr = src.window_transform(Window(xo, yo, SIZE, SIZE))
        px, py = abs(tr.a), abs(tr.e)
        west, north = tr.c, tr.f
        east, south = west + SIZE * px, north - SIZE * py
    print(f"window xoff={xo} yoff={yo}; lon {west:.5f}..{east:.5f} lat {south:.5f}..{north:.5f}")
    inside = {}
    for k, (c, r) in pin_px.items():
        inside[k] = bool(xo <= c < xo + SIZE and yo <= r < yo + SIZE)
    print("pins inside:", inside)

    # 1. Atlas.
    ndvi = np.where(raw != 0, raw.astype(np.float64) / 10000.0, np.nan)
    q = quantize(ndvi)
    atlas = np.zeros((5 * SIZE, 8 * SIZE), np.uint8)
    for i in range(NYEARS):
        r, c = divmod(i, 8)
        atlas[r * SIZE:(r + 1) * SIZE, c * SIZE:(c + 1) * SIZE] = q[i]
    save_png(out / "ndvi.png", atlas)

    # 2. LandTrendr on the crop (raw x 10000 scale, 0 = no data, direction="loss").
    lt = zeit.landtrendr(raw, years=years, direction="loss", n_jobs=N_JOBS)
    vy = lt.vertex_year.values.astype(int)  # (vertex, y, x), 0 past the last
    vv = lt.vertex_value.values.astype(np.float64) / 10000.0
    assert vy.shape[0] == MAX_VERT
    pairs = np.full((SIZE, SIZE, MAX_VERT, 2), 255, np.uint8)
    exists = vy > 0
    qv = quantize(np.where(exists, vv, np.nan))
    for k in range(MAX_VERT):
        e = exists[k]
        pairs[..., k, 0] = np.where(e, vy[k] - YEAR0, 255)
        pairs[..., k, 1] = np.where(e, qv[k], 255)
    (out / "lt.bin").write_bytes(pairs.reshape(-1).tobytes())

    # 3. Greatest-loss maps.
    ev = events(lt, zeit, MIN_MAG, PRE_VAL)
    yod = ev.yod.values.astype(int)
    mag = np.nan_to_num(ev.magnitude.values)
    save_png(out / "yod.png", np.where(yod > 0, yod - YEAR0, 255).astype(np.uint8))
    save_png(out / "mag.png", np.where(yod > 0, np.rint(np.clip(mag / MAG_SCALE, 0, 1) * 255), 0).astype(np.uint8))
    print(f"pixels with a loss event: {(yod > 0).mean():.3f}")

    # 4. Parameter grid on six pixels.
    lowev = events(lt, zeit, 1000, PRE_VAL)
    pin_xy = (pin_px["clearing"][0] - xo, pin_px["clearing"][1] - yo) if inside["clearing"] else None
    chosen = choose_pixels(raw, ev, lowev, pin_xy) if pin_xy else choose_pixels(raw, ev, lowev, (128, 128))
    if not pin_xy:
        chosen = chosen[1:]
    for k in ("regrowth", "forest"):
        if inside[k]:
            chosen.insert(0, (f"{k}_pin", pin_px[k][0] - xo, pin_px[k][1] - yo))
    xs = np.array([c[1] for c in chosen])
    ys = np.array([c[2] for c in chosen])
    sub = raw[:, ys, xs][:, None, :]  # (time, 1, n) cube
    ev1 = zeit.extract_events(lt, event_type="loss", sort_by="greatest")
    fits = [dict() for _ in chosen]
    default_key = "6_0.9_0.25"
    for ms, sp, rc in itertools.product(GRID_MS, GRID_SPIKE, GRID_REC):
        r = zeit.landtrendr(sub, years=years, direction="loss", max_segments=ms, spike_threshold=sp,
                            recovery_threshold=rc, n_jobs=N_JOBS)
        for i in range(len(chosen)):
            y = r.vertex_year.values[:, 0, i].astype(int)
            v = r.vertex_value.values[:, 0, i]
            keep = y > 0
            fits[i][f"{ms}_{sp}_{rc}"] = {
                "vertices": [[int(a), round(float(b) / 10000.0, 3)] for a, b in zip(y[keep], v[keep])],
                "rmse": round(float(r.rmse.values[0, i]) / 10000.0, 4)}
    pixels = []
    for i, (role, x, y) in enumerate(chosen):
        s = ndvi[:, y, x]
        e1 = int(ev1.yod.values[y, x])
        pre = float(ev1.pre_val.values[y, x]) / 10000.0
        post = float(ev1.post_val.values[y, x]) / 10000.0
        end = float(np.nanmean(s[-3:]))
        e = int(ev.yod.values[y, x])
        lab = label_of(role.replace("_pin", "_pin"), s, e if e else e1, pre, post, end)
        if role == "regrowth_pin":
            lab = f"Guide's regrowth pin: {np.nanmin(s):.2f} at the lowest, {end:.2f} by 2024"
        if role == "forest_pin":
            lab = f"Guide's continuous-forest pin: NDVI stays near {np.nanmean(s):.2f}"
        pixels.append({
            "role": role, "x": int(x), "y": int(y),
            "lon": round(west + (x + 0.5) * px, 6), "lat": round(north - (y + 0.5) * py, 6),
            "label": lab,
            "series": [None if not np.isfinite(a) else round(float(a), 3) for a in s],
            "fits": fits[i]})
    (out / "grid.json").write_text(json.dumps({"params": {
        "max_segments": GRID_MS, "spike_threshold": GRID_SPIKE, "recovery_threshold": GRID_REC,
        "key": "<max_segments>_<spike_threshold>_<recovery_threshold>"}, "pixels": pixels},
        separators=(",", ":")), encoding="utf-8")

    # 5. Verification.
    rng = np.random.default_rng(0)
    worst = 0.0
    checked = 0
    step = (VMAX - VMIN) / 254.0
    flat = pairs.reshape(SIZE * SIZE, MAX_VERT, 2)
    for _ in range(20):
        x, y = int(rng.integers(SIZE)), int(rng.integers(SIZE))
        one = zeit.landtrendr(raw[:, y, x].astype(float), years=years, direction="loss", nodata=0)
        zy = one.vertex_year.values.astype(int)
        zv = one.vertex_value.values / 10000.0
        keep = zy > 0
        b = flat[y * SIZE + x]
        by = b[b[:, 0] != 255].astype(int)
        assert list(by[:, 0] + YEAR0) == list(zy[keep]), (x, y)
        dq = dequantize(b[: len(by), 1])
        err = np.abs(dq - np.clip(zv[keep], VMIN, VMAX))
        worst = max(worst, float(err.max()))
        checked += len(by)
    print(f"lt.bin vs zeit on 20 random pixels: {checked} vertices, max error {worst:.5f} (step {step:.5f}, half step {step / 2:.5f})")
    assert worst <= step / 2 + 1e-9
    for i, (role, x, y) in enumerate(chosen):
        b = flat[y * SIZE + x]
        by = b[b[:, 0] != 255].astype(int)
        dq = dequantize(by[:, 1])
        g = fits[i][default_key]["vertices"]
        assert [v[0] for v in g] == list(by[:, 0] + YEAR0), (role, g, by)
        e = np.abs(np.array([v[1] for v in g]) - dq).max()
        assert e <= step / 2 + 0.0006, (role, e)
        print(f"grid pixel {role} ({x},{y}): default entry matches lt.bin, max diff {e:.5f}")

    meta = {
        "title": "LandTrendr demo, annual NDVI, Rondonia (Brazil)",
        "years": years, "width": SIZE, "height": SIZE,
        "window": {"xoff": xo, "yoff": yo, "source": Path(args.input).name},
        "crs": "EPSG:4326",
        "bounds": {"west": west, "south": south, "east": east, "north": north},
        "corners": {"nw": [west, north], "ne": [east, north], "sw": [west, south], "se": [east, south]},
        "pixel_size": {"x": px, "y": py},
        "atlas": {"file": "ndvi.png", "columns": 8, "rows": 5, "tile": SIZE,
                  "tile_of_year": "column = i % 8, row = i // 8, i = year - 1985"},
        "ndvi_mapping": {"min": VMIN, "max": VMAX, "no_data": 0,
                         "decode": "ndvi = (q - 1) / 254 * (max - min) + min, for q in 1..255"},
        "lt_bin": {"file": "lt.bin", "endianness": "little", "bytes_per_pixel": 2 * MAX_VERT,
                   "order": "row-major pixels; per pixel 7 vertices of (year - 1985, q) uint8; 255,255 = no vertex",
                   "value_mapping": "same as ndvi_mapping"},
        "yod": {"file": "yod.png", "decode": "year = 1985 + v, 255 = no event",
                "year_meaning": "year of the vertex where the greatest loss starts (last observation before the loss)"},
        "mag": {"file": "mag.png", "decode": "magnitude in NDVI = v / 255 * 1.0 (raw 10000 scale: v / 255 * 10000), 0 = no event",
                "scale_ndvi": MAG_SCALE / 10000.0},
        "event_filter": {"sort_by": "greatest", "event_type": "loss",
                         "min_magnitude_ndvi": MIN_MAG / 10000.0, "pre_val_threshold_ndvi": PRE_VAL / 10000.0,
                         "note": "extract_events takes the raw x 10000 units of the segmented data: 2500 and 7500"},
        "grid": {"file": "grid.json", "max_segments": GRID_MS, "spike_threshold": GRID_SPIKE,
                 "recovery_threshold": GRID_REC},
        "zeit": {"version": getattr(zeit, "__version__", None) or _zeit_version(),
                 "commit": _zeit_commit(Path(zeit.__file__).resolve().parents[1])},
        "landtrendr_parameters": {"direction": "loss", "max_segments": 6, "pval_threshold": 0.05,
                                  "recovery_threshold": 0.25, "prevent_fast_recovery": True,
                                  "spike_threshold": 0.9, "best_model_proportion": 0.75,
                                  "vertex_count_overshoot": 3, "min_observations_needed": 6,
                                  "nodata": 0, "scale": "raw Int16 NDVI x 10000, outputs divided by 10000"},
        "pins_inside": inside,
    }
    (out / "meta.json").write_text(json.dumps(meta, indent=1), encoding="utf-8")
    for f in sorted(out.iterdir()):
        print(f"{f.name}: {f.stat().st_size / 1024:.1f} KB")
    print(f"runtime {time.time() - t0:.1f} s")
    for p in pixels:
        print(p["role"], p["x"], p["y"], p["lon"], p["lat"], "|", p["label"])


if __name__ == "__main__":
    main()
