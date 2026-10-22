"""LandTrendr (zeit.landtrendr) for Janus: per-pixel segmentation and change-event maps
(zeit.extract_events). The years are those of the series' dates."""
import numpy as np

import zeit_common as zc

MANIFEST = {
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
         "help": "Direction of change of interest. Orients the segmentation (Zeit's direction) "
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


def _kwargs(p):
    return dict(
        direction=p["event_type"],
        max_segments=int(p["max_segments"]),
        pval_threshold=float(p["pval_threshold"]),
        recovery_threshold=float(p["recovery_threshold"]),
        spike_threshold=float(p["spike_threshold"]),
        best_model_proportion=float(p["best_model_proportion"]),
        vertex_count_overshoot=int(p["vertex_count_overshoot"]),
        min_observations_needed=int(p["min_observations_needed"]),
    )


def _run(p, stack, ctx):
    """zeit.landtrendr and zeit.extract_events on a [T, rows, cols] chunk: (vertices, events)."""
    import zeit
    lt = zeit.landtrendr(zc.cube(stack, ctx), nodata=None, n_jobs=ctx.get("n_jobs", -1), **_kwargs(p))
    events = zeit.extract_events(lt, event_type=p["event_type"], sort_by=p["sort_by"],
                                 min_magnitude=float(p["min_magnitude"]), min_duration=int(p["min_duration"]),
                                 pre_val_threshold=float(p["pre_val_threshold"]))
    return lt, events


def pixel(p, ctx):
    vals = np.array(ctx["values"], dtype=np.float64)
    if np.isfinite(vals).sum() < 2:
        return {"overlays": [], "rows": [["Vertices", "-"]]}
    lt, ev = _run(p, zc.pixel_stack(ctx), ctx)
    n = int(lt["n_vertices"].values[0, 0])
    vyears = lt["vertex_year"].values[:n, 0, 0].astype(int)
    vvals = lt["vertex_value"].values[:n, 0, 0].astype(np.float64)
    vx = [float(y) for y in vyears]
    vy = [float(v) if np.isfinite(v) else None for v in vvals]

    # Segments in the direction of interest (the event the maps would show is below).
    loss = p["event_type"] == "loss"
    n_events = sum(1 for a, b in zip(vvals[:-1], vvals[1:]) if ((a - b) if loss else (b - a)) > 0)
    rows = [["Vertices", str(n)], ["Segments", str(max(0, n - 1))],
            [f"{p['event_type'].capitalize()} segments", str(n_events)]]
    e = {k: zc.grid(ev, k)[0, 0] for k in ("yod", "magnitude", "duration", "pre_val", "post_val", "rate", "dsnr")}
    if e["yod"] > 0:
        y0, dur = int(e["yod"]), int(e["duration"])
        rows += [[f"Mapped event ({p['sort_by']})", f"{e['magnitude']:.5g} ({y0}-{y0 + dur})"],
                 ["  pre / post", f"{e['pre_val']:.5g} / {e['post_val']:.5g}"],
                 ["  rate", f"{e['rate']:.5g}/yr"],
                 ["  DSNR", f"{e['dsnr']:.4g}" if np.isfinite(e["dsnr"]) else "-"]]
    else:
        rows += [["Mapped event", "none"]]
    return {
        "overlays": [
            {"type": "line", "label": "LandTrendr fit", "x": vx, "y": vy},
            {"type": "markers", "label": "LandTrendr vertices", "x": vx, "y": vy},
        ],
        "rows": rows,
    }


def chunk(p, stack, ctx):
    _, events = _run(p, stack, ctx)
    none = events["yod"].values == 0  # no event in this pixel
    out = {}
    for o in MANIFEST["outputs"]:
        a = zc.grid(events, o["id"])
        a[none] = np.nan
        out[o["id"]] = a
    return zc.chunk_outputs(out, stack.shape[1:])


def warmup():
    """A pixel run on a made-up series: compiles zeit.extract_events' numba kernel
    (a few seconds, once per process) before the first real one."""
    p = {q["id"]: q["default"] for q in MANIFEST["params"]}
    years = list(range(2000, 2012))
    days = (np.array([f"{y}-07-01" for y in years], dtype="datetime64[D]").astype(np.int64)
            + zc.UNIX_EPOCH_ORDINAL)
    pixel(p, {"years": years, "ordinal": [int(d) for d in days], "n_jobs": 1,
              "values": [0.8] * 6 + [0.4] * 6})


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk, "warmup": warmup}]
