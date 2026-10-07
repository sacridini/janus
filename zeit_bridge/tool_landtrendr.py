"""LandTrendr (Zeit) for tsv: per-pixel segmentation and change-event maps."""
import numpy as np

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


def _kwargs(p):
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


def pixel(p, ctx):
    from zeit.landtrendr import run_landtrendr

    years = [int(round(y)) for y in ctx["years"]]
    vals = np.array(ctx["values"], dtype=np.float64)
    if np.isfinite(vals).sum() < 2:
        return {"overlays": [], "rows": [["Vertices", "-"]]}
    vertices = run_landtrendr(years, vals, **_kwargs(p))
    vx = [float(v["year"]) for v in vertices]
    vy = [float(v["value"]) if np.isfinite(v["value"]) else None for v in vertices]

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

    rows = [["Vertices", str(len(vertices))], ["Segments", str(max(0, len(vertices) - 1))]]
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


def chunk(p, stack, ctx):
    from zeit.metrics import extract_events
    from zeit.raster import run_landtrendr_array

    years = np.array([int(round(y)) for y in ctx["years"]], dtype=np.int32)
    vertices, rmse = run_landtrendr_array(years, stack, no_data_value=-1e30, return_rmse=True, **_kwargs(p))
    events = extract_events(vertices, event_type=p["event_type"], sort_by=p["sort_by"],
                            min_magnitude=float(p["min_magnitude"]), min_duration=int(p["min_duration"]),
                            pre_val_threshold=float(p["pre_val_threshold"]), rmse_map=rmse)
    none = events["yod"] == 0  # no event in this pixel
    out = {}
    for o in MANIFEST["outputs"]:
        a = events[o["id"]].astype(np.float32)
        a[none] = np.nan
        out[o["id"]] = a
    return out


TOOLS = [{"manifest": MANIFEST, "pixel": pixel, "chunk": chunk}]
