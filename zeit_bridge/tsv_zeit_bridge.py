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
  (rounded, >= 1), "start": first decimal year, "ordinal": Python ordinal day of
  each date (None when the series has no real dates)}.
- Multiband tools (manifest "requires": {"bands": [role, ...]}, e.g. CCDC) also
  get ctx["bands"] = {role: float64 array, NaN = missing} for every role tsv
  mapped (the required ones plus any of "optional_bands"; [T] for pixel runs,
  [T, rows, cols] for chunks) and ctx["fmask"] = int32 Fmask codes with the same
  shape (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation),
  converted from the series' quality band, or derived from missing values when
  there is none. ctx["shown"] = {"band": role, "nd": [role A, role B]} says
  what the shown series is in band roles (None when unknown), e.g. to draw a
  multiband model in the shown units.
- `pixel` receives ctx["values"] (floats, NaN = missing) and returns
  {"overlays": [...], "rows": [[label, text], ...]}. Overlay types, x in
  decimal years: {"type": "line"|"markers", "label", "x", "y"} and
  {"type": "vlines", "label", "x"} (e.g. break dates).
- `chunk` receives stack: float64 [T, rows, cols] (NaN = missing) and returns
  {output_id: 2D float array (rows, cols), NaN = no value} for every output id
  declared in the manifest.

Manifest: id, name, category, description, requires {"time": "any"|"annual"|
"regular", "min_dates": n, "min_per_year": n, "bands": [role, ...],
"optional_bands": [role, ...]}, modes, params (id, label, type
int|float|bool|enum, default, min, max, options, labels, help), outputs (id,
name, colormap, unit). Band roles: blue, green, red, nir, swir1, swir2, thermal.

Series shown by tsv
-------------------
With one file per date and several bands, tsv may show a normalized difference
of two bands and hide unusable dates with a quality band. Pixel runs receive the
shown values; raster jobs get the pieces ("input", "nd_input", "qa_input",
"qa_rule") and the bridge rebuilds the same values (shown_stack).
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


UNIX_EPOCH_ORDINAL = 719163  # date(1970, 1, 1).toordinal()


def make_ctx(years, days=None):
    """days: days since 1970-01-01 of each date (None without real dates)."""
    years = [float(y) for y in years]
    ordinal = [int(d) + UNIX_EPOCH_ORDINAL for d in days] if days else None
    return {"years": years, "per_year": per_year(years), "start": years[0] if years else 0.0,
            "ordinal": ordinal}


# ---------------------------------------------------------------------------
# Quality band and normalized difference (same rules as tsv's reader)
# ---------------------------------------------------------------------------

def qa_usable(rule, qa):
    """Bool array: observations the quality rule keeps (NaN codes = unusable)."""
    import numpy as np
    qa = np.asarray(qa, dtype=np.float64)
    ok = np.isfinite(qa) & (qa >= 0)
    code = np.where(ok, qa, 0).astype(np.int64)
    if rule == "fmask":
        keep = (code == 0) | (code == 1)
    elif rule == "landsat_c2":  # bits 0 fill, 1 dilated cloud, 3 cloud, 4 shadow, 5 snow
        keep = (code & 0x3B) == 0
    elif rule == "nonzero":
        keep = code != 0
    else:
        keep = np.ones(code.shape, bool)
    return ok & keep


def to_fmask(rule, qa, values_ok):
    """Fmask codes from the quality band; without one, 0 where every band has a
    value and 255 elsewhere. values_ok: bool array, all bands finite."""
    import numpy as np
    out = np.where(values_ok, 0, 255).astype(np.int32)
    if qa is None or rule in (None, "none"):
        return out
    qa = np.asarray(qa, dtype=np.float64)
    fin = np.isfinite(qa) & (qa >= 0)
    code = np.where(fin, qa, 255).astype(np.int64)
    if rule == "fmask":
        f = code.astype(np.int32)
    elif rule == "landsat_c2":
        f = np.zeros(code.shape, np.int32)
        f[(code & (1 << 7)) != 0] = 1                     # water
        f[(code & (1 << 5)) != 0] = 3                     # snow
        f[(code & (1 << 4)) != 0] = 2                     # cloud shadow
        f[(code & ((1 << 1) | (1 << 3))) != 0] = 4        # dilated cloud, cloud
        f[(code & 1) != 0] = 255                          # fill
    else:  # nonzero mask
        f = np.where(code != 0, 0, 4).astype(np.int32)
    f[~fin] = 255
    return np.where(values_ok, f, 255).astype(np.int32)


def norm_diff(a, b):
    import numpy as np
    with np.errstate(invalid="ignore", divide="ignore"):
        s = a + b
        return np.where(s != 0, (a - b) / s, np.nan)


def band_ctx(ctx, bands, qa, rule):
    """Adds ctx["bands"] and ctx["fmask"] from {role: array} and the QA codes."""
    import numpy as np
    bands = {r: np.asarray(v, dtype=np.float64) for r, v in (bands or {}).items()}
    ctx["bands"] = bands
    if bands:
        ok = np.logical_and.reduce([np.isfinite(v) for v in bands.values()])
        ctx["fmask"] = to_fmask(rule, qa, ok)
    return ctx


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
    ctx = make_ctx(spec["years"], spec.get("days"))
    ctx["shown"] = spec.get("shown")
    nodata = spec.get("nodata")
    x0, y0, x1, y1 = spec["window"]
    out_dir = spec["output_dir"]
    os.makedirs(out_dir, exist_ok=True)
    W, H = x1 - x0, y1 - y0

    # Extra inputs, one VRT per source band (one band per date each).
    extra = {}
    for key in ("nd_input", "qa_input"):
        if spec.get(key):
            extra[key] = spec[key]
    for role, path in (spec.get("bands") or {}).items():
        extra["band:" + role] = path
    srcs = {k: rasterio.open(v) for k, v in extra.items()}

    def read(src, win, raw=False):
        a = src.read(window=win).astype(np.float64)
        if not raw:
            if src.nodata is not None and np.isfinite(src.nodata):
                a[a == src.nodata] = np.nan
            a[~np.isfinite(a)] = np.nan
        return a

    with rasterio.open(spec["input"]) as src:
        if src.count != len(ctx["years"]):
            raise ValueError(f"input has {src.count} bands but {len(ctx['years'])} dates were given")
        for k, s in srcs.items():
            if s.count != src.count or s.width != src.width or s.height != src.height:
                raise ValueError(f"{k} does not match the input ({s.count} bands, {s.width} x {s.height})")
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
                win = Window(x0, y0 + r, W, h)
                stack = src.read(window=win).astype(np.float64)  # [T, h, W]
                if nodata is not None:
                    stack[stack == float(nodata)] = np.nan
                stack[~np.isfinite(stack)] = np.nan
                qa = read(srcs["qa_input"], win, raw=True) if "qa_input" in srcs else None
                # The series tsv shows: normalized difference, then the QA mask.
                if "nd_input" in srcs:
                    stack = norm_diff(stack, read(srcs["nd_input"], win))
                if qa is not None:
                    stack[~qa_usable(spec.get("qa_rule"), qa)] = np.nan
                if any(k.startswith("band:") for k in srcs):
                    bands = {k[5:]: read(s, win) for k, s in srcs.items() if k.startswith("band:")}
                    band_ctx(ctx, bands, qa, spec.get("qa_rule"))
                res = entry["chunk"](p, stack, ctx)
                for oid, d in dst.items():
                    d.write(np.asarray(res[oid], dtype=np.float32), 1, window=Window(0, r, W, h))
                done += h
                el = time.time() - t0
                progress(done / H, f"rows {done}/{H}, {el:.0f} s elapsed, ~{el / done * (H - done):.0f} s left")
        finally:
            for d in dst.values():
                d.close()
            for s in srcs.values():
                s.close()

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
                ctx = make_ctx(params["years"], params.get("days"))
                ctx["values"] = [float("nan") if v is None else float(v) for v in params["values"]]
                ctx["shown"] = params.get("shown")
                if params.get("bands"):
                    nan = float("nan")
                    bands = {r: [nan if v is None else float(v) for v in vals]
                             for r, vals in params["bands"].items()}
                    qa = params.get("qa")
                    qa = [nan if v is None else float(v) for v in qa] if qa else None
                    band_ctx(ctx, bands, qa, params.get("qa_rule"))
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
