"""Agreement of change maps (zeit.agreement) for Janus: where and when several maps of
change dates agree, e.g. LandTrendr, CCDC, BFAST and CODED results.

The tool runs on maps Janus already shows (manifest "input": "layers"): any result
whose unit is a date ("year"), from any series. The bridge puts them on the grid of
the first one. A date counts by its calendar year (as LandTrendr's year of detection);
two maps agree when their years are at most `tolerance` apart. Zeit picks, per pixel,
the year the most maps agree with (ties: the year more maps give exactly, then the
earliest).
"""
import numpy as np

import zeit_common as zc

MANIFEST = {
    "id": "agreement",
    "name": "Agreement of change maps",
    "category": "Change detection",
    "input": "layers",
    "description": (
        "Where and when several maps of change dates agree (Zeit's zeit.agreement): choose two or more "
        "results with dates (LandTrendr's year of detection, CCDC, BFAST or CODED break dates...), from "
        "this series or others. A change counts by its calendar year; maps agree when their years are at "
        "most the tolerance apart. Gives the consensus year and how many maps back it: changes found by "
        "several algorithms are the ones to trust. The maps are put on the grid of the first one."),
    "requires": {"time": "any", "min_dates": 0},
    "modes": ["raster"],
    "params": [
        {"id": "maps", "label": "Maps of change dates", "type": "layers", "unit": "year", "default": [],
         "help": "Tool results whose values are dates. The outputs are on the grid of the first one checked."},
        {"id": "tolerance", "label": "Tolerance (years)", "type": "int", "default": 1, "min": 0, "max": 10,
         "help": "Years two changes may be apart and still agree (0: the same year)."},
    ],
    "outputs": [
        {"id": "yod", "name": "Consensus year", "colormap": "Viridis", "unit": "year"},
        {"id": "n_agree", "name": "Maps that agree", "colormap": "Viridis", "unit": "count"},
        {"id": "share", "name": "Share of the maps that agree", "colormap": "Viridis", "unit": "%"},
        {"id": "n_detected", "name": "Maps with a change", "colormap": "Viridis", "unit": "count"},
        {"id": "spread", "name": "Spread of the years", "colormap": "Plasma", "unit": "years"},
    ],
}


def chunk(p, stack, ctx):
    import xarray as xr
    import zeit

    names, seen = [], {}
    for n in ctx["names"]:  # zeit.agreement takes a dict: the names must differ
        seen[n] = seen.get(n, 0) + 1
        names.append(n if seen[n] == 1 else f"{n} ({seen[n]})")
    years = np.floor(stack)  # a date counts by its calendar year
    maps = {n: xr.DataArray(years[k], dims=("y", "x")) for k, n in enumerate(names)}
    ds = zeit.agreement(maps, tolerance=int(p["tolerance"]))
    yod = zc.grid(ds, "yod")
    found = yod > 0
    n_agree = zc.grid(ds, "n_agree")
    res = {
        "yod": np.where(found, yod, np.nan),
        "n_agree": np.where(found, n_agree, np.nan),
        "share": np.where(found, 100.0 * n_agree / len(names), np.nan),
        "n_detected": np.where(np.isfinite(stack).any(axis=0), zc.grid(ds, "n_detected"), np.nan),
        "spread": np.where(found, zc.grid(ds, "spread"), np.nan),
    }
    return zc.chunk_outputs(res, stack.shape[1:])


TOOLS = [{"manifest": MANIFEST, "chunk": chunk}]
