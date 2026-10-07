"""Bridge between tsv (C++ viewer) and Zeit (Python + C++ time-series library).

tsv runs this script with its private Python runtime, in a separate process.
All algorithms come from Zeit's public API. This file is the protocol and the
generic machinery; each tool lives in a `tool_*.py` module next to it.

Modes
-----
serve
    Persistent JSON-lines RPC on stdin/stdout: the tool manifest and per-pixel
    runs (the series is sent by tsv, the result comes back in ~ms).
        -> {"id": 1, "method": "hello"}
        <- {"id": 1, "result": {"protocol": 1, "zeit_version": "...", "tools": [...]}}
        -> {"id": 2, "method": "run_pixel", "params": {"tool": "...", "params": {...},
                                                         "years": [...], "values": [...]}}
        <- {"id": 2, "result": {"overlays": [...], "rows": [...]}}
job SPEC.json
    One-shot raster run (whole image or a window). Prints JSON lines:
        {"progress": 0.42, "message": "..."} ... then {"result": {...}} or {"error": "..."}
    tsv cancels a job by terminating the process.

stdout carries only protocol messages; anything else (warnings, prints from
libraries) goes to stderr, which tsv saves to a log file.

Tool modules
------------
A `tool_*.py` module defines `TOOLS`, a list of dicts:

    {"manifest": {...},                       # sent to tsv as is (see below)
     "pixel": pixel(p, ctx) -> dict,          # optional (manifest "modes": ["pixel", ...])
     "chunk": chunk(p, stack, ctx) -> dict}   # optional (manifest "modes": [..., "raster"])

- `p`: parameter values (defaults filled in), keyed by parameter id.
- `ctx`: {"years": decimal year of each date, "per_year": observations per year
  (rounded, >= 1), "start": first decimal year}.
- `pixel` receives ctx["values"] (floats, NaN = missing) and returns
  {"overlays": [...], "rows": [[label, text], ...]}. Overlay types, x in
  decimal years: {"type": "line"|"markers", "label", "x", "y"} and
  {"type": "vlines", "label", "x"} (e.g. break dates).
- `chunk` receives stack: float64 [T, rows, cols] (NaN = missing) and returns
  {output_id: 2D float array (rows, cols), NaN = no value} for every output id
  declared in the manifest.

Manifest: id, name, category, description, requires {"time": "any"|"annual"|
"regular", "min_dates": n, "min_per_year": n}, modes, params (id, label, type
int|float|bool|enum, default, min, max, options, labels, help), outputs (id,
name, colormap, unit).
"""
import glob
import importlib
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

# The embeddable Python does not put the script folder on sys.path.
HERE = os.path.dirname(os.path.abspath(__file__))
if HERE not in sys.path:
    sys.path.insert(0, HERE)


def send(msg):
    _proto.write(json.dumps(msg, separators=(",", ":"), allow_nan=False) + "\n")
    _proto.flush()


def finite(x):
    """JSON has no NaN: missing values travel as null."""
    if x is None:
        return None
    x = float(x)
    return x if math.isfinite(x) else None


def per_year(years):
    """Observations per year, from the median spacing of the dates (>= 1)."""
    if len(years) < 2:
        return 1
    d = sorted(b - a for a, b in zip(years[:-1], years[1:]) if b > a)
    if not d:
        return 1
    med = d[len(d) // 2]
    return max(1, int(round(1.0 / med))) if med > 0 else 1


def make_ctx(years):
    years = [float(y) for y in years]
    return {"years": years, "per_year": per_year(years), "start": years[0] if years else 0.0}


# ---------------------------------------------------------------------------
# Tool registry
# ---------------------------------------------------------------------------

_TOOLS = None
_LOAD_ERRORS = []


def tools():
    """{tool id: entry} from every tool_*.py next to this file (loaded once)."""
    global _TOOLS
    if _TOOLS is None:
        _TOOLS = {}
        for path in sorted(glob.glob(os.path.join(HERE, "tool_*.py"))):
            name = os.path.splitext(os.path.basename(path))[0]
            try:
                mod = importlib.import_module(name)
                for t in mod.TOOLS:
                    _TOOLS[t["manifest"]["id"]] = t
            except Exception as e:  # one broken tool must not hide the others
                traceback.print_exc()
                _LOAD_ERRORS.append(f"{name}: {type(e).__name__}: {e}")
    return _TOOLS


def defaults(manifest, given):
    out = {p["id"]: p["default"] for p in manifest["params"]}
    out.update({k: v for k, v in (given or {}).items() if k in out})
    return out


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
        "tools": [t["manifest"] for t in tools().values()],
        "errors": _LOAD_ERRORS,
    }


# ---------------------------------------------------------------------------
# Generic raster runner (used by every tool's chunk function)
# ---------------------------------------------------------------------------

def run_raster(entry, p, spec, progress):
    import numpy as np
    import rasterio
    from rasterio.windows import Window

    m = entry["manifest"]
    ctx = make_ctx(spec["years"])
    nodata = spec.get("nodata")
    x0, y0, x1, y1 = spec["window"]
    out_dir = spec["output_dir"]
    os.makedirs(out_dir, exist_ok=True)
    W, H = x1 - x0, y1 - y0

    with rasterio.open(spec["input"]) as src:
        if src.count != len(ctx["years"]):
            raise ValueError(f"input has {src.count} bands but {len(ctx['years'])} dates were given")
        transform = src.window_transform(Window(x0, y0, W, H))
        profile = dict(driver="GTiff", width=W, height=H, count=1, crs=src.crs, transform=transform,
                       dtype="float32", nodata=float("nan"), compress="deflate", tiled=True,
                       blockxsize=256, blockysize=256, BIGTIFF="IF_SAFER")
        paths = {o["id"]: os.path.join(out_dir, f"{m['id']}_{o['id']}.tif") for o in m["outputs"]}
        dst = {oid: rasterio.open(path, "w", **profile) for oid, path in paths.items()}

        # Full-width row bands: each strip of the source is read once (fast on HDDs).
        cells = int(m.get("chunk_cells", 4_000_000))
        rows_per_chunk = max(8, min(512, cells // max(1, W)))
        done = 0
        t0 = time.time()
        try:
            for r in range(0, H, rows_per_chunk):
                h = min(rows_per_chunk, H - r)
                stack = src.read(window=Window(x0, y0 + r, W, h)).astype(np.float64)  # [T, h, W]
                if nodata is not None:
                    stack[stack == float(nodata)] = np.nan
                stack[~np.isfinite(stack)] = np.nan
                res = entry["chunk"](p, stack, ctx)
                for oid, d in dst.items():
                    d.write(np.asarray(res[oid], dtype=np.float32), 1, window=Window(0, r, W, h))
                done += h
                el = time.time() - t0
                progress(done / H, f"rows {done}/{H}, {el:.0f} s elapsed, ~{el / done * (H - done):.0f} s left")
        finally:
            for d in dst.values():
                d.close()

    return {"outputs": [dict(o, path=paths[o["id"]]) for o in m["outputs"]], "window": [x0, y0, x1, y1]}


# ---------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------

def serve():
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
                entry = tools()[params["tool"]]
                p = defaults(entry["manifest"], params.get("params"))
                ctx = make_ctx(params["years"])
                ctx["values"] = [float("nan") if v is None else float(v) for v in params["values"]]
                send({"id": req_id, "result": entry["pixel"](p, ctx)})
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
    last = [0.0]

    def progress(frac, message=""):
        now = time.time()
        if now - last[0] >= 0.25 or frac >= 1.0:  # at most 4 updates per second
            last[0] = now
            send({"progress": round(float(frac), 4), "message": message})

    try:
        entry = tools()[spec["tool"]]
        p = defaults(entry["manifest"], spec.get("params"))
        progress(0.0, "starting")
        send({"result": run_raster(entry, p, spec, progress)})
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
