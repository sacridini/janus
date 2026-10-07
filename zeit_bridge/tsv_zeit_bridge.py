"""Bridge between tsv (C++ viewer) and Zeit (Python + C++ time-series library).

tsv runs this script with its private Python runtime, in a separate process.
All algorithms come from Zeit's public API; this file only describes the tools
(the manifest tsv turns into menus and forms), converts parameters, reads/writes
rasters in chunks and reports progress.

Modes
-----
serve
    Persistent JSON-lines RPC on stdin/stdout, used for the tool manifest and
    for per-pixel runs (the series is sent by tsv, the result comes back in ~ms).
        -> {"id": 1, "method": "hello"}
        <- {"id": 1, "result": {"protocol": 1, "zeit_version": "...", "tools": [...]}}
        -> {"id": 2, "method": "run_pixel", "params": {"tool": "landtrendr", ...}}
        <- {"id": 2, "result": {"overlays": [...], "rows": [...]}}
job SPEC.json
    One-shot raster run (whole image or a window). Prints JSON lines:
        {"progress": 0.42, "message": "..."} ... then {"result": {...}} or {"error": "..."}
    tsv cancels a job by terminating the process.

stdout carries only protocol messages; anything else (warnings, prints from
libraries) goes to stderr, which tsv saves to a log file.
"""
import json
import math
import os
import sys
import time
import traceback

PROTOCOL = 1

# Keep the protocol channel clean: libraries that print go to stderr.
_proto = sys.stdout
sys.stdout = sys.stderr
_proto.reconfigure(encoding="utf-8", newline="\n")


def send(msg):
    _proto.write(json.dumps(msg, separators=(",", ":"), allow_nan=False) + "\n")
    _proto.flush()


def finite(x):
    """JSON has no NaN: missing values travel as null."""
    if x is None:
        return None
    x = float(x)
    return x if math.isfinite(x) else None


# ---------------------------------------------------------------------------
# Tool manifest
# ---------------------------------------------------------------------------
# Parameter types: int, float, bool, enum (options + labels). `zeit` names the
# keyword argument of Zeit's API when it differs from `id`.

LANDTRENDR = {
    "id": "landtrendr",
    "name": "LandTrendr",
    "category": "Change detection",
    "description": (
        "Temporal segmentation of annual series (Kennedy et al. 2010), as implemented in Zeit. "
        "Fits a sequence of straight segments and maps the greatest (or newest, fastest...) "
        "loss or gain event per pixel."),
    "requires": {"time": "annual", "min_dates": 6},
    "modes": ["pixel", "raster"],
    "params": [
        {"id": "event_type", "label": "Event", "type": "enum", "default": "loss",
         "options": ["loss", "gain"], "labels": ["Loss (index drop)", "Gain (index rise)"],
         "help": "Direction of change of interest. Orients the segmentation (Zeit's modifier) "
                 "and selects which segments become events."},
        {"id": "max_segments", "label": "Max segments", "type": "int", "default": 6, "min": 1, "max": 10,
         "help": "Maximum number of segments in the fitted trajectory."},
        {"id": "pval_threshold", "label": "p-value threshold", "type": "float", "default": 0.05,
         "min": 0.001, "max": 1.0, "help": "Models with a worse p-value are rejected."},
        {"id": "recovery_threshold", "label": "Recovery threshold", "type": "float", "default": 0.25,
         "min": 0.0, "max": 10.0, "help": "Maximum recovery rate per year (1/years)."},
        {"id": "spike_threshold", "label": "Spike threshold", "type": "float", "default": 0.9,
         "min": 0.0, "max": 1.0, "help": "Desawtooth dampening of single-year spikes (1 = none)."},
        {"id": "best_model_proportion", "label": "Best model proportion", "type": "float", "default": 0.75,
         "min": 0.0, "max": 2.0, "help": "Prefer the model with most vertices whose p-value is within "
                                         "(2 - this) times the best one."},
        {"id": "vertex_count_overshoot", "label": "Vertex count overshoot", "type": "int", "default": 3,
         "min": 0, "max": 10, "help": "Extra candidate vertices before pruning."},
        {"id": "min_observations_needed", "label": "Min observations", "type": "int", "default": 6,
         "min": 3, "max": 100, "help": "Pixels with fewer valid years are not fitted."},
        {"id": "sort_by", "label": "Pick event by", "type": "enum", "default": "greatest",
         "options": ["greatest", "newest", "fastest", "longest", "dsnr"],
         "labels": ["Greatest magnitude", "Newest", "Fastest", "Longest", "DSNR (magnitude / RMSE)"],
         "help": "Which event is mapped when a pixel has several (raster runs)."},
        {"id": "min_magnitude", "label": "Min magnitude", "type": "float", "default": 0.0, "min": 0.0,
         "max": 1e9, "help": "Events smaller than this are ignored (raster runs)."},
        {"id": "min_duration", "label": "Min duration (years)", "type": "int", "default": 1, "min": 1,
         "max": 100, "help": "Events shorter than this are ignored (raster runs)."},
        {"id": "pre_val_threshold", "label": "Pre-event value threshold", "type": "float", "default": 0.0,
         "min": -1e9, "max": 1e9, "help": "Loss: ignore events starting below this value. "
                                          "Gain: ignore events starting above it. 0 = off."},
    ],
    "outputs": [
        {"id": "yod", "name": "Year of detection", "colormap": "Viridis", "unit": "year"},
        {"id": "magnitude", "name": "Magnitude", "colormap": "Plasma", "unit": "value"},
        {"id": "duration", "name": "Duration", "colormap": "Viridis", "unit": "years"},
        {"id": "pre_val", "name": "Pre-event value", "colormap": "Viridis", "unit": "value"},
        {"id": "post_val", "name": "Post-event value", "colormap": "Viridis", "unit": "value"},
        {"id": "rate", "name": "Rate", "colormap": "Plasma", "unit": "value/year"},
        {"id": "dsnr", "name": "DSNR", "colormap": "Plasma", "unit": "ratio"},
    ],
}

TOOLS = [LANDTRENDR]


def manifest():
    import zeit
    try:
        from importlib.metadata import version
        zv = version("zeit-cdts")
    except Exception:
        zv = getattr(zeit, "__version__", "?")
    return {
        "protocol": PROTOCOL,
        "zeit_version": zv,
        "python": sys.version.split()[0],
        "tools": TOOLS,
    }


def defaults(tool, given):
    out = {p["id"]: p["default"] for p in tool["params"]}
    out.update({k: v for k, v in (given or {}).items() if k in out})
    return out


# ---------------------------------------------------------------------------
# LandTrendr
# ---------------------------------------------------------------------------

def lt_kwargs(p):
    return dict(
        max_segments=int(p["max_segments"]),
        pval_threshold=float(p["pval_threshold"]),
        recovery_threshold=float(p["recovery_threshold"]),
        spike_threshold=float(p["spike_threshold"]),
        best_model_proportion=float(p["best_model_proportion"]),
        vertex_count_overshoot=int(p["vertex_count_overshoot"]),
        min_observations_needed=int(p["min_observations_needed"]),
        modifier=-1.0 if p["event_type"] == "loss" else 1.0,
    )


def landtrendr_pixel(p, years, values):
    """years: decimal years (annual series); values: floats with None = missing."""
    import numpy as np
    from zeit.landtrendr import run_landtrendr

    yrs = [int(round(y)) for y in years]
    vals = np.array([np.nan if v is None else float(v) for v in values], dtype=np.float64)
    if np.isfinite(vals).sum() < 2:
        return {"overlays": [], "rows": [["Vertices", "-"]], "note": "not enough valid years"}
    vertices = run_landtrendr(yrs, vals, **lt_kwargs(p))
    vx = [float(v["year"]) for v in vertices]
    vy = [finite(v["value"]) for v in vertices]

    # Segments as events, in the direction of interest.
    loss = p["event_type"] == "loss"
    events = []
    for a, b in zip(vertices[:-1], vertices[1:]):
        dur = b["year"] - a["year"]
        if dur <= 0:
            continue
        mag = (a["value"] - b["value"]) if loss else (b["value"] - a["value"])
        if mag > 0:
            events.append((mag, a["year"], b["year"], a["value"], b["value"]))
    events.sort(reverse=True)

    rows = [["Vertices", str(len(vertices))],
            ["Segments", str(max(0, len(vertices) - 1))]]
    if events:
        mag, y0, y1, v0, v1 = events[0]
        rows += [[f"Greatest {p['event_type']}", f"{mag:.5g} ({y0}-{y1})"],
                 ["  pre / post", f"{v0:.5g} / {v1:.5g}"],
                 ["  rate", f"{mag / (y1 - y0):.5g}/yr"],
                 [f"{p['event_type'].capitalize()} events", str(len(events))]]
    else:
        rows += [[f"{p['event_type'].capitalize()} events", "0"]]
    return {
        "overlays": [
            {"type": "line", "label": "LandTrendr fit", "x": vx, "y": vy},
            {"type": "markers", "label": "LandTrendr vertices", "x": vx, "y": vy},
        ],
        "rows": rows,
    }


def landtrendr_raster(p, spec, progress):
    import numpy as np
    import rasterio
    from rasterio.windows import Window
    from zeit.metrics import extract_events
    from zeit.raster import run_landtrendr_array

    years = np.array([int(round(y)) for y in spec["years"]], dtype=np.int32)
    nodata = spec.get("nodata")
    x0, y0, x1, y1 = spec["window"]
    out_dir = spec["output_dir"]
    os.makedirs(out_dir, exist_ok=True)

    with rasterio.open(spec["input"]) as src:
        if src.count != len(years):
            raise ValueError(f"input has {src.count} bands but {len(years)} dates were given")
        W, H = x1 - x0, y1 - y0
        transform = src.window_transform(Window(x0, y0, W, H))
        profile = dict(driver="GTiff", width=W, height=H, count=1, crs=src.crs, transform=transform,
                       compress="deflate", tiled=True, blockxsize=256, blockysize=256, BIGTIFF="IF_SAFER")

        outputs = {o["id"]: o for o in LANDTRENDR["outputs"]}
        dst = {}
        for oid in outputs:
            prof = dict(profile)
            prof.update(dtype="float32", nodata=float("nan"))
            dst[oid] = rasterio.open(os.path.join(out_dir, f"landtrendr_{oid}.tif"), "w", **prof)

        # Full-width row bands: each strip of the source is read once (fast on HDDs).
        rows_per_chunk = max(16, min(512, int(4_000_000 // max(1, W))))
        done_rows = 0
        t0 = time.time()
        try:
            for r in range(0, H, rows_per_chunk):
                h = min(rows_per_chunk, H - r)
                win = Window(x0, y0 + r, W, h)
                stack = src.read(window=win).astype(np.float64)  # [T, h, W]
                nd = float(nodata) if nodata is not None else -1e30
                if nodata is None:
                    stack[~np.isfinite(stack)] = nd
                vertices, rmse = run_landtrendr_array(years, stack, no_data_value=nd, return_rmse=True,
                                                      **lt_kwargs(p))
                events = extract_events(vertices, event_type=p["event_type"], sort_by=p["sort_by"],
                                        min_magnitude=float(p["min_magnitude"]),
                                        min_duration=int(p["min_duration"]),
                                        pre_val_threshold=float(p["pre_val_threshold"]),
                                        rmse_map=rmse)
                none = events["yod"] == 0  # no event in this pixel
                ow = Window(0, r, W, h)
                for oid, d in dst.items():
                    a = events[oid].astype(np.float32)
                    a[none] = np.nan
                    d.write(a, 1, window=ow)
                done_rows += h
                el = time.time() - t0
                eta = el / done_rows * (H - done_rows)
                progress(done_rows / H, f"rows {done_rows}/{H}, {el:.0f} s elapsed, ~{eta:.0f} s left")
        finally:
            for d in dst.values():
                d.close()

    return {"outputs": [dict(outputs[oid], path=os.path.join(out_dir, f"landtrendr_{oid}.tif"))
                        for oid in outputs],
            "window": [x0, y0, x1, y1]}


PIXEL = {"landtrendr": landtrendr_pixel}
RASTER = {"landtrendr": landtrendr_raster}


# ---------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------

def serve():
    tools = {t["id"]: t for t in TOOLS}
    send({"event": "starting", "protocol": PROTOCOL})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req_id = None
        try:
            req = json.loads(line)
            req_id = req.get("id")
            method = req.get("method")
            params = req.get("params") or {}
            if method == "hello":
                send({"id": req_id, "result": manifest()})
            elif method == "run_pixel":
                tool = tools[params["tool"]]
                p = defaults(tool, params.get("params"))
                res = PIXEL[tool["id"]](p, params["years"], params["values"])
                send({"id": req_id, "result": res})
            elif method == "shutdown":
                send({"id": req_id, "result": {}})
                return
            else:
                send({"id": req_id, "error": f"unknown method: {method}"})
        except Exception as e:  # never die on a bad request
            traceback.print_exc()
            send({"id": req_id, "error": f"{type(e).__name__}: {e}"})


def job(spec_path):
    with open(spec_path, encoding="utf-8") as f:
        spec = json.load(f)
    tool = next(t for t in TOOLS if t["id"] == spec["tool"])
    p = defaults(tool, spec.get("params"))
    last = [0.0]

    def progress(frac, message=""):
        now = time.time()
        if now - last[0] >= 0.25 or frac >= 1.0:  # at most 4 updates per second
            last[0] = now
            send({"progress": round(float(frac), 4), "message": message})

    try:
        progress(0.0, "starting")
        res = RASTER[tool["id"]](p, spec, progress)
        send({"result": res})
    except Exception as e:
        traceback.print_exc()
        send({"error": f"{type(e).__name__}: {e}"})
        sys.exit(1)


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "serve"
    if mode == "serve":
        serve()
    elif mode == "job":
        job(sys.argv[2])
    else:
        sys.exit(f"usage: {sys.argv[0]} serve | job SPEC.json")
