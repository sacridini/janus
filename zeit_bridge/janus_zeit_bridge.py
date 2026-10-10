"""Bridge between Janus (C++ viewer) and Zeit (Python + C++ time-series library).

Janus runs this script with its private Python runtime, in a separate process.
All algorithms come from Zeit's public API. This file is the protocol and the
generic machinery; each tool lives in a `tool_*.py` module next to it.

Modes
-----
serve
    Persistent JSON-lines RPC on stdin/stdout: the tool manifest and per-pixel
    runs (the series is sent by Janus, the result comes back in ~ms).
        -> {"id": 1, "method": "hello"}
        <- {"id": 1, "result": {"protocol": 1, "zeit_version": "...", "tools": [...]}}
        -> {"id": 2, "method": "run_pixel", "params": {"tool": "...", "params": {...},
                                                         "years": [...], "values": [...]}}
        <- {"id": 2, "result": {"overlays": [...], "rows": [...]}}
        -> {"id": 3, "method": "estimate", "params": {job spec without output_dir}}
        <- {"id": 3, "result": {"sec_per_px": ..., "sec_per_chunk": ..., "chunk_cells": ...}}
job SPEC.json
    One-shot raster run (whole image or a window). Prints JSON lines:
        {"progress": 0.42, "message": "..."} ... then {"result": {...}} or {"error": "..."}
    Janus cancels a job by terminating the process.

stdout carries only protocol messages; anything else (warnings, prints from
libraries) goes to stderr, which Janus saves to a log file.

Tool modules
------------
A `tool_*.py` module defines `TOOLS`, a list of dicts:

    {"manifest": {...},                       # sent to Janus as is (see below)
     "pixel": pixel(p, ctx) -> dict,          # optional (manifest "modes": ["pixel", ...])
     "chunk": chunk(p, stack, ctx) -> dict,   # optional (manifest "modes": [..., "raster"])
     "fit": fit(p, stack, ctx) -> model,      # optional: a model of the whole window (e.g. a
                                              # SOM), fitted before the chunks on the pixels of
                                              # rows spread over it (stack [T, 1, N]); the
                                              # chunks get it in ctx["model"]
     "warmup": warmup()}                      # optional: compiles what the first run would
                                              # (e.g. numba kernels), in the background after hello

zeit_common.py, shared by the tools, turns Janus's series and chunks into the
inputs of Zeit's API (dated xarray cubes) and its results back into arrays.

- `p`: parameter values (defaults filled in), keyed by parameter id.
- `ctx`: {"years": decimal year of each date, "per_year": observations per year
  (rounded, >= 1), "start": first decimal year, "ordinal": Python ordinal day of
  each date (None when the series has no real dates), "n_jobs": CPU threads a
  chunk may use, to pass as Zeit's n_jobs (-1 = not limited)}. Janus sets the
  limit in Settings: it reaches the bridge as JANUS_THREADS (with
  OMP_NUM_THREADS, NUMBA_NUM_THREADS and the BLAS ones) and as "threads" in a
  job spec.
- Multiband tools (manifest "requires": {"bands": [role, ...]}, e.g. CCDC) also
  get ctx["bands"] = {role: float64 array, NaN = missing} for every role Janus
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
  the run writes ([T, rows, cols] for a series output).

Parameters of type "patterns" are edited in Janus (reference series from pins or
the ROI): a list of {"name", "from", "years": decimal years, "days": days since
1970 or null, "values": floats or null} on the series' own dates. An output with
"classes_param": "<param id>" is a class map: value k (1-based) is the k-th
pattern; the job result lists the names as "classes". A class map may also
declare its "classes" in the manifest, or get them from the fit
(model["outputs"][output id] is merged into the result's output, e.g. its
"classes" and "class_series": [{"x": decimal years, "y": values}, ...], the
typical series of each class, which Janus draws for the class under the cursor).

Parameters of type "layers" are maps Janus shows, chosen in the tool window
("unit": only maps of that unit, e.g. "year"): a list of {"name", "path"}. A tool
with "input": "layers" runs on them instead of the series (run_layers).

Manifest: id, name, category, description, input ("series", the default, or
"layers"), requires {"time": "any"|"annual"|"regular", "min_dates": n,
"min_per_year": n, "bands": [role, ...], "optional_bands": [role, ...]}, modes,
params (id, label, type int|float|bool|enum|patterns|layers, default, min, max,
options, labels, help, unit), outputs (id, name, colormap, unit, classes;
"series": true for a series of the run's dates, opened in Janus as a new layer;
"dtype": "int16" with "scale" to store it in 2 bytes; "when": a bool parameter
that must be on for it to be written). Band roles: blue, green, red, nir, swir1,
swir2, thermal.

Series shown by Janus
-------------------
With one file per date and several bands, Janus may show a normalized difference
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
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor

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


def threads(spec=None):
    """CPU threads Janus allows (Settings): the job spec's "threads", else
    JANUS_THREADS from the environment; -1 = not limited (Zeit's default)."""
    n = (spec or {}).get("threads") or os.environ.get("JANUS_THREADS") or -1
    try:
        n = int(n)
    except ValueError:
        n = -1
    return n if n > 0 else -1


def make_ctx(years, days=None, n_jobs=-1):
    """days: days since 1970-01-01 of each date (None without real dates)."""
    years = [float(y) for y in years]
    ordinal = [int(d) + UNIX_EPOCH_ORDINAL for d in days] if days else None
    return {"years": years, "per_year": per_year(years), "start": years[0] if years else 0.0,
            "ordinal": ordinal, "n_jobs": n_jobs}


# ---------------------------------------------------------------------------
# Quality band and normalized difference (same rules as Janus's reader)
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

class Inputs:
    """The rasters of a job spec, read one window at a time: the shown series
    (normalized difference and QA mask applied, as Janus shows it) plus, for
    multiband tools, every band and the Fmask codes in ctx."""

    def __init__(self, spec):
        import rasterio
        self.spec = spec
        self.ctx = make_ctx(spec["years"], spec.get("days"), threads(spec))
        self.ctx["shown"] = spec.get("shown")
        # Each window gets a copy of ctx; this dict is shared by all of them, for what a
        # tool decides once per job (e.g. the reflectance units).
        self.ctx["cache"] = {}
        self.nodata = spec.get("nodata")
        paths = {"input": spec["input"]}
        for key in ("nd_input", "qa_input"):
            if spec.get(key):
                paths[key] = spec[key]
        for role, path in (spec.get("bands") or {}).items():
            paths["band:" + role] = path
        self.srcs = {}
        try:
            for k, v in paths.items():
                self.srcs[k] = rasterio.open(v)
            src = self.src = self.srcs["input"]
            if src.count != len(self.ctx["years"]):
                raise ValueError(f"input has {src.count} bands but {len(self.ctx['years'])} dates were given")
            for k, s in self.srcs.items():
                if s.count != src.count or s.width != src.width or s.height != src.height:
                    raise ValueError(f"{k} does not match the input ({s.count} bands, {s.width} x {s.height})")
        except Exception:
            self.close()
            raise

    def close(self):
        for s in self.srcs.values():
            s.close()
        self.srcs = {}

    @staticmethod
    def _read(src, win, raw=False):
        import numpy as np
        a = src.read(window=win, out_dtype=np.float64)  # converted by GDAL: no float32 copy on the way
        if not raw:
            if src.nodata is not None and np.isfinite(src.nodata):
                a[a == src.nodata] = np.nan
            a[~np.isfinite(a)] = np.nan
        return a

    def read(self, win):
        """Shown stack [T, h, w] for the window, and the ctx of that window
        (its own bands/fmask for multiband tools): run_raster reads the next
        window while the tool computes the current one."""
        import numpy as np
        stack = self.src.read(window=win, out_dtype=np.float64)
        if self.nodata is not None:
            stack[stack == float(self.nodata)] = np.nan
        stack[~np.isfinite(stack)] = np.nan
        qa = self._read(self.srcs["qa_input"], win, raw=True) if "qa_input" in self.srcs else None
        if "nd_input" in self.srcs:
            stack = norm_diff(stack, self._read(self.srcs["nd_input"], win))
        if qa is not None:
            stack[~qa_usable(self.spec.get("qa_rule"), qa)] = np.nan
        ctx = dict(self.ctx)
        bands = {k[5:]: self._read(s, win) for k, s in self.srcs.items() if k.startswith("band:")}
        if bands:
            band_ctx(ctx, bands, qa, self.spec.get("qa_rule"))
        return stack, ctx


def active_outputs(m, p):
    """The manifest's outputs this run writes (an output with "when": param id only if that
    bool parameter is on)."""
    return [o for o in m["outputs"] if not o.get("when") or p.get(o["when"])]


def date_labels(spec):
    """One label per date for the band descriptions of a series output, which Janus reads
    back as the dates: Janus's own labels (same granularity: "2005", "2005-03" or
    "2005-03-14"), else ISO dates, else the decimal years."""
    import datetime as dt
    if spec.get("labels") and len(spec["labels"]) == len(spec["years"]):
        return [str(s) for s in spec["labels"]]
    if spec.get("days"):
        return [(dt.date(1970, 1, 1) + dt.timedelta(days=int(d))).isoformat() for d in spec["days"]]
    return [f"{y:.4f}" for y in spec["years"]]


class Outputs:
    """The GeoTIFFs of a run, written one full-width row band at a time. A map is one band
    (rows, cols); a series output ("series": true) has one band per date [T, rows, cols],
    the dates in the band descriptions, and opens in Janus as a new layer. "dtype": "int16"
    with "scale" stores value / scale (nodata -32768, the scale in the band metadata that
    Janus and any GIS apply); the default is float32 with NaN."""

    def __init__(self, m, outputs, out_dir, crs, transform, W, H, spec, labels=None):
        import rasterio
        # The outputs' blocks are compressed on the job's threads (one thread
        # took a third of a Mann-Kendall run).
        n = threads(spec)
        base = dict(driver="GTiff", width=W, height=H, count=1, crs=crs, transform=transform,
                    dtype="float32", nodata=float("nan"), compress="deflate", tiled=True,
                    blockxsize=256, blockysize=256, BIGTIFF="IF_SAFER",
                    num_threads=str(n) if n > 0 else "ALL_CPUS")
        self.W = W
        self.outs = {o["id"]: o for o in outputs}
        self.paths = {o["id"]: os.path.join(out_dir, f"{m['id']}_{o['id']}.tif") for o in outputs}
        self.dst = {}
        try:
            for o in outputs:
                prof = dict(base)
                count = 1
                if o.get("series"):
                    count = len(labels)
                    prof.update(count=count, interleave="band")  # Janus reads a date at a time
                if o.get("dtype") == "int16":
                    prof.update(dtype="int16", nodata=-32768, predictor=2)
                d = self.dst[o["id"]] = rasterio.open(self.paths[o["id"]], "w", **prof)
                if o.get("series"):
                    for b, label in enumerate(labels, start=1):
                        d.set_band_description(b, label)
                if o.get("dtype") == "int16":
                    d.scales = [float(o.get("scale", 1.0))] * count
                    d.offsets = [0.0] * count
        except Exception:
            self.close()
            raise

    def write(self, res, r, h):
        import numpy as np
        from rasterio.windows import Window
        win = Window(0, r, self.W, h)
        for oid, d in self.dst.items():
            o = self.outs[oid]
            a = np.asarray(res[oid], dtype=np.float64)
            if o.get("dtype") == "int16":
                q = np.round(a / float(o.get("scale", 1.0)))
                a = np.where(np.isfinite(q), np.clip(q, -32767, 32767), -32768).astype(np.int16)
            else:
                a = a.astype(np.float32)
            if o.get("series"):
                d.write(a.reshape(d.count, h, self.W), window=win)
            else:
                d.write(a.reshape(h, self.W), 1, window=win)

    def close(self):
        for d in self.dst.values():
            d.close()
        self.dst = {}

    def describe(self, p, model=None):
        """The result's outputs: the manifest's, with their path, the class names of a
        class map and what the tool's fit adds (model["outputs"][id], e.g. its classes)."""
        extra = (model or {}).get("outputs", {}) if isinstance(model, dict) else {}
        out = []
        for oid, o in self.outs.items():
            d = dict(o, path=self.paths[oid])
            if o.get("classes_param"):  # class map: value k = name of the k-th pattern
                d["classes"] = [str(c.get("name", f"Class {k + 1}")) for k, c in enumerate(p[o["classes_param"]])]
            d.update(extra.get(oid, {}))
            out.append(d)
        return out


FIT_ROWS = 64  # rows of the window a tool's fit samples (spread over it)


def sample_rows(inp, window, n_rows=FIT_ROWS, progress=None):
    """Every pixel of n_rows full-width rows spread over the window, as one [T, 1, N]
    stack and its ctx (with the bands and Fmask codes of multiband tools): what a tool
    fits its model on before the chunks (e.g. a SOM)."""
    import numpy as np
    from rasterio.windows import Window
    x0, y0, x1, y1 = window
    W, H = x1 - x0, y1 - y0
    rows = sorted({int(round(r)) for r in np.linspace(y0, y1 - 1, max(1, min(H, n_rows)))})
    stacks, ctxs = [], []
    for k, r in enumerate(rows):
        s, c = inp.read(Window(x0, r, W, 1))
        stacks.append(s)
        ctxs.append(c)
        if progress:
            progress((k + 1) / len(rows))
    ctx = ctxs[0]
    if ctx.get("bands"):
        ctx["bands"] = {role: np.concatenate([c["bands"][role] for c in ctxs], axis=2) for role in ctx["bands"]}
        ctx["fmask"] = np.concatenate([c["fmask"] for c in ctxs], axis=2)
    return np.concatenate(stacks, axis=2), ctx


def run_raster(entry, p, spec, progress):
    from rasterio.windows import Window

    m = entry["manifest"]
    x0, y0, x1, y1 = spec["window"]
    out_dir = spec["output_dir"]
    os.makedirs(out_dir, exist_ok=True)
    W, H = x1 - x0, y1 - y0

    inp = Inputs(spec)
    outs = None
    try:
        # A tool with a model of the whole window (e.g. a SOM) fits it first, on a
        # sample of the window's rows; every chunk then gets it in ctx["model"].
        base = 0.0
        if entry.get("fit"):
            base = 0.1
            stack, ctx = sample_rows(inp, (x0, y0, x1, y1),
                                     progress=lambda f: progress(base * 0.5 * f, "reading a sample of the image"))
            progress(base * 0.5, f"fitting on a sample of {stack.shape[2]} pixels")
            inp.ctx["model"] = entry["fit"](p, stack, ctx)
            del stack, ctx
        src = inp.src
        outs = Outputs(m, active_outputs(m, p), out_dir, src.crs, src.window_transform(Window(x0, y0, W, H)),
                       W, H, spec, labels=date_labels(spec))

        # Full-width row bands: each strip of the source is read once (fast on
        # HDDs). The next band is read in a thread while the tool computes this
        # one (GDAL reads without the GIL; the disk and the CPU work at once).
        rows = rows_per_chunk(m, W)
        bands = [(r, min(rows, H - r)) for r in range(0, H, rows)]
        done = 0
        t0 = time.time()
        with ThreadPoolExecutor(1) as reader:  # waits for a read still running before the finally
            def read(k):
                r, h = bands[k]
                return reader.submit(inp.read, Window(x0, y0 + r, W, h))  # ([T, h, W], ctx)

            nxt = read(0)
            for k, (r, h) in enumerate(bands):
                stack, ctx = nxt.result()
                if k + 1 < len(bands):
                    nxt = read(k + 1)
                res = entry["chunk"](p, stack, ctx)
                del stack, ctx  # before the next band arrives: two stacks at most
                outs.write(res, r, h)
                done += h
                el = time.time() - t0
                progress(base + (1 - base) * done / H,
                         f"rows {done}/{H}, {el:.0f} s elapsed, ~{el / done * (H - done):.0f} s left")
    finally:
        if outs:
            outs.close()
        inp.close()

    return {"outputs": outs.describe(p, inp.ctx.get("model")), "window": [x0, y0, x1, y1]}


def run_layers(entry, p, spec, progress):
    """A tool over maps Janus already shows (manifest "input": "layers"), e.g. the agreement
    of several change maps, instead of the series. Its parameter of type "layers" lists
    them ({"name", "path"}); the outputs are on the first one's grid, and the others are
    put on it (nearest neighbour) whatever their CRS or extent. chunk(p, stack, ctx) gets
    stack: float64 [n maps, rows, cols] (NaN = no value) and ctx["names"]."""
    import numpy as np
    import rasterio
    from rasterio.enums import Resampling
    from rasterio.vrt import WarpedVRT
    from rasterio.windows import Window

    m = entry["manifest"]
    lp = next(q for q in m["params"] if q["type"] == "layers")
    maps = [x for x in (p.get(lp["id"]) or []) if x.get("path")]
    if len(maps) < 2:
        raise ValueError(f"choose at least two maps in '{lp['label']}'")
    out_dir = spec["output_dir"]
    os.makedirs(out_dir, exist_ok=True)
    srcs, readers, outs = [], [], None
    try:
        for x in maps:
            srcs.append(rasterio.open(x["path"]))
        ref = srcs[0]
        readers = [ref] + [WarpedVRT(s, crs=ref.crs, transform=ref.transform, width=ref.width, height=ref.height,
                                     resampling=Resampling.nearest, nodata=np.nan, dtype="float32")
                           for s in srcs[1:]]
        W, H = ref.width, ref.height
        outs = Outputs(m, active_outputs(m, p), out_dir, ref.crs, ref.transform, W, H, spec)
        ctx = {"names": [str(x.get("name", f"map {k + 1}")) for k, x in enumerate(maps)], "n_jobs": threads(spec)}
        rows = rows_per_chunk(m, W)
        for r in range(0, H, rows):
            h = min(rows, H - r)
            stack = np.stack([rd.read(1, window=Window(0, r, W, h), out_dtype=np.float64, masked=True).filled(np.nan)
                              for rd in readers])
            outs.write(entry["chunk"](p, stack, ctx), r, h)
            progress((r + h) / H, f"rows {r + h}/{H}")
    finally:
        if outs:
            outs.close()
        for rd in readers[1:]:
            rd.close()
        for s in srcs:
            s.close()
    window = spec.get("window") or [0, 0, W, H]
    return {"outputs": outs.describe(p), "window": window}


def rows_per_chunk(m, W):
    """Rows of each full-width band a job processes at once."""
    cells = int(m.get("chunk_cells", 4_000_000))
    return max(8, min(512, cells // max(1, W)))


def estimate(entry, p, spec):
    """Computing time of a job: a fixed cost per chunk (some tools spend most
    of a small call starting threads or compiling) plus a cost per pixel,
    fitted on growing samples from the middle of the window (after a warm-up
    call). Reading the data is not included. Returns the model so the caller
    can apply it to any window: seconds = sec_fixed + chunks * sec_per_chunk +
    pixels * sec_per_px, with chunks = ceil(rows / rows_per_chunk(W)); sec_fixed
    is a tool's fit (sampling the window and fitting its model, e.g. a SOM)."""
    from rasterio.windows import Window

    if entry["manifest"].get("input") == "layers":  # a few array operations per pixel
        return {"sec_per_px": 5e-8, "sec_per_chunk": 0.0, "sec_fixed": 0.0, "chunk_cells": 4_000_000}
    x0, y0, x1, y1 = spec["window"]
    W, H = x1 - x0, y1 - y0
    inp = Inputs(spec)
    try:
        fixed = 0.0
        if entry.get("fit"):  # fewer rows than a run reads: the serve process waits meanwhile
            t = time.perf_counter()
            stack, ctx = sample_rows(inp, spec["window"], n_rows=16)
            inp.ctx["model"] = entry["fit"](p, stack, ctx)
            fixed = time.perf_counter() - t
            del stack, ctx

        def sample(n):
            w, h = min(n, W), min(n, H)
            win = Window(x0 + (W - w) // 2, y0 + (H - h) // 2, w, h)
            stack, ctx = inp.read(win)
            t = time.perf_counter()
            entry["chunk"](p, stack, ctx)
            return time.perf_counter() - t, w * h

        sample(2)  # warm-up

        def fit(points):
            """Least squares t = a + b * n (b >= 0)."""
            if len(points) == 1:
                t, n = points[0]
                return 0.0, t / max(1, n)  # pessimistic: everything per pixel
            ns = [n for _, n in points]
            ts = [t for t, _ in points]
            mn, mt = sum(ns) / len(ns), sum(ts) / len(ts)
            sxx = sum((n - mn) ** 2 for n in ns)
            b = max(0.0, sum((n - mn) * (t - mt) for t, n in points) / sxx) if sxx > 0 else 0.0
            return max(0.0, mt - b * mn), b

        # Samples of 8 and 16 px a side, then 64 and 128 while the time barely
        # grows with the size (a large fixed cost per call, e.g. LandTrendr's
        # ~1 s: only large samples show its per-pixel cost) or the next sample
        # should be cheap anyway; about 4 s at most.
        points = [sample(8)]
        if points[0][0] < 1.5:
            points.append(sample(16))
        for side in (64, 128):
            if len(points) < 2 or side > max(W, H) * 2:
                break
            (t0, n0), (t1, n1) = points[-2], points[-1]
            spent = sum(t for t, _ in points)
            flat = t1 < 1.5 * t0 and t1 < 1.5
            cheap = t1 * side * side / n1 < 1.0  # even if all of it were per pixel
            if spent > 4.0 or not (flat or cheap):
                break
            points.append(sample(side))
        per_chunk, per_px = fit(points)
        cells = int(entry["manifest"].get("chunk_cells", 4_000_000))
        return {"sec_per_px": per_px, "sec_per_chunk": per_chunk, "sec_fixed": fixed, "chunk_cells": cells,
                "samples": [[n, round(t, 4)] for t, n in points]}
    finally:
        inp.close()


# ---------------------------------------------------------------------------
# Entry points
# ---------------------------------------------------------------------------

def warm_up():
    """Runs the tools' warmup hooks (one at a time, errors only logged)."""
    for entry in tools().values():
        if entry.get("warmup"):
            try:
                entry["warmup"]()
            except Exception:
                traceback.print_exc()


def serve():
    send({"event": "starting", "protocol": PROTOCOL})
    warming = None
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
                if warming is None:  # while Janus is idle: the first pixel run does not wait
                    warming = threading.Thread(target=warm_up, name="warmup", daemon=True)
                    warming.start()
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
            elif method == "estimate":
                entry = tools()[params["tool"]]
                p = defaults(entry["manifest"], params.get("params"))
                send({"id": req_id, "result": estimate(entry, p, params)})
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
        if spec["tool"] == "embeddings":  # a download, not a tool over the cube (task_embeddings.py)
            import task_embeddings
            send({"result": task_embeddings.run(spec, progress)})
            return
        entry = tools()[spec["tool"]]
        p = defaults(entry["manifest"], spec.get("params"))
        progress(0.0, "starting")
        run = run_layers if entry["manifest"].get("input") == "layers" else run_raster
        send({"result": run(entry, p, spec, progress)})
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
