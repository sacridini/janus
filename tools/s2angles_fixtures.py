"""The references of janus-zeit's Sentinel-2 angles (src/s2angles.rs, tests/s2angles.rs) and of the NBAR tool's
`angles = "sentinel2"`: zeit.sentinel2_angles on real tile metadata (23LKC, Zeit's two fixtures, copied to
tests/data/s2angles: the full tile of 2023-07-23 and a partial swath of 2023-07-26), and zeit.nbar on a small
series whose dates take them (tests/data/s2angles/series).

    python tools/s2angles_fixtures.py <the Zeit checkout's tests/data folder>

Writes into crates/janus-zeit/tests/data/s2angles: `angles.json` (the cases: grid, sources, the file of the
float32 angles [4][rows][columns] Zeit gave, NaN where it has none) and `nbar.json` (the series' NBAR as
tools/nbar_fixtures.py writes it, with the angles Zeit read from the metadata). Needs Zeit, rioxarray and pyproj.
"""

import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd
import rasterio
import xarray as xr
from pyproj import Transformer
from rasterio.transform import from_origin

import zeit

HERE = Path(__file__).resolve().parents[1] / "crates" / "janus-zeit" / "tests" / "data" / "s2angles"
FULL, PARTIAL = "s2_23LKC_20230723_MTD_TL.xml", "s2_23LKC_20230726_partial_MTD_TL.xml"
ULX, ULY = 199980.0, 8300020.0       # the tile's upper left corner (23LKC)


def cube(xs, ys, crs, date="2023-07-23"):
    data = np.full((1, 1, ys.size, xs.size), 0.2, dtype=np.float32)
    return xr.DataArray(data, dims=("time", "band", "y", "x"),
                        coords={"time": pd.to_datetime([date]), "band": ["B04"], "y": ys, "x": xs}).rio.write_crs(crs)


def centres(x0, y0, dx, dy, w, h):
    """The pixel centres of a north-up grid whose upper left corner is (x0, y0)."""
    return x0 + (np.arange(w) + 0.5) * dx, y0 - (np.arange(h) + 0.5) * dy


def main(zeit_data):
    HERE.mkdir(parents=True, exist_ok=True)
    texts = {n: (Path(zeit_data) / n).read_text(encoding="utf-8") for n in (FULL, PARTIAL)}
    for n, t in texts.items():
        (HERE / n).write_text(t, encoding="utf-8")
    # a pair of tiles is one date in the second case: the partial one's sensing time moved to the full one's day
    # (Zeit refuses sources more than a day from the date)
    moved = texts[PARTIAL].replace("2023-07-26T13:36:41.935298Z", "2023-07-23T13:36:41.935298Z")
    assert moved != texts[PARTIAL]

    # tile centre in the other CRSs
    cx, cy = ULX + 54900, ULY - 54900
    lon, lat = Transformer.from_crs(32723, 4326, always_xy=True).transform(cx, cy)
    x24, y24 = Transformer.from_crs(32723, 32724, always_xy=True).transform(cx, cy)

    def grid(name, crs, x0, y0, dx, dy, w, h, sources, date="2023-07-23"):
        xs, ys = centres(x0, y0, dx, dy, w, h)
        like = cube(xs, ys, crs, date)
        got = zeit.sentinel2_angles(sources, like, view_band="B06", n_jobs=1)
        a = np.stack([got[k].values[0] for k in ("SZA", "SAA", "VZA", "VAA")]).astype(np.float32)
        a.tofile(HERE / f"{name}.f32")
        nan = np.isnan(a[2])
        print(f"{name}: {w} x {h}, VZA {np.nanmin(a[2]):.3f}..{np.nanmax(a[2]):.3f}, no view angle at {nan.sum()} of {nan.size}")
        return {"name": name, "epsg": crs, "origin": [x0, y0], "pixel": [dx, dy], "size": [w, h], "sources": sources_names(sources),
                "date": date, "file": f"{name}.f32"}

    full, partial = texts[FULL], texts[PARTIAL]
    names = {id(full): FULL, id(partial): PARTIAL, id(moved): PARTIAL + " (sensing time moved to 2023-07-23)"}

    def sources_names(s):
        return [[names[id(t)] for t in e] if isinstance(e, list) else names[id(e)] for e in (s if isinstance(s, list) else [s])]

    cases = [
        # 10 m pixels, a window of 300 x 200 in the swath overlap's neighbourhood
        grid("utm10m", 32723, ULX + 46012.0, ULY - 31005.0, 10.0, 10.0, 300, 200, [full]),
        # 250 m pixels over 40 x 30 km: cells and detector edges
        grid("utm250m", 32723, ULX + 5130.0, ULY - 4070.0, 250.0, 250.0, 160, 120, [full]),
        # the tile and its neighbour zone
        grid("utm24s", 32724, round(x24) - 14000.0, round(y24) + 12000.0, 250.0, 250.0, 112, 96, [full]),
        grid("wgs84", 4326, lon - 0.13, lat + 0.11, 0.002, 0.002, 130, 100, [full]),
        # the partial swath alone: no view angle outside its strip
        grid("partial", 32723, ULX + 1010.0, ULY - 2020.0, 250.0, 250.0, 120, 100, [partial], "2023-07-26"),
        # two tiles of a date, in both orders (the first fills the grid, the next the pixels it has no view zenith for)
        grid("two_partial_first", 32723, ULX + 3010.0, ULY - 5020.0, 250.0, 250.0, 160, 120, [[moved, full]]),
        grid("two_full_first", 32723, ULX + 3010.0, ULY - 5020.0, 250.0, 250.0, 160, 120, [[full, moved]]),
    ]
    (HERE / "angles.json").write_text(json.dumps({"cases": cases}, indent=1))
    series(texts)


def series(texts):
    """Three dates on a 20 x 16 grid of 800 m pixels (UTM 23S, across the east edge of the partial swath):
    the full tile's metadata on 2023-07-23 and 2023-07-24, the partial one's on 2023-07-26; bands blue, red, NIR as
    a file each (a band per date); the metadata of each date is in spec.json's `granules`."""
    W, H, T = 20, 16, 3
    x0, y0, res = ULX + 12500.0, ULY - 3000.0, 800.0
    days = ["2023-07-23", "2023-07-24", "2023-07-26"]
    meta = [FULL, FULL, PARTIAL]
    rng = np.random.default_rng(20261107)
    roles = ["blue", "red", "nir"]
    base = {"blue": 0.04, "red": 0.06, "nir": 0.35}
    refl = {r: (base[r] * (1 + 0.3 * rng.random((T, H, W)))).astype(np.float32) for r in roles}
    for r in roles:
        refl[r][rng.random((T, H, W)) < 0.05] = np.nan
    transform = from_origin(x0, y0, res, res)

    def write(name, array):
        with rasterio.open(HERE / "series" / name, "w", driver="GTiff", width=W, height=H, count=array.shape[0],
                           dtype="float32", nodata=np.nan, crs="EPSG:32723", transform=transform,
                           compress="deflate") as dst:
            dst.write(array)

    (HERE / "series").mkdir(exist_ok=True)
    for r in roles:
        write(f"{r}.tif", refl[r])
    write("input.tif", refl["red"])
    decimal = [2023 + (np.datetime64(d) - np.datetime64("2023-01-01")) / np.timedelta64(1, "D") / 365 for d in days]
    spec = {"tool": "nbar", "params": {}, "input": "input.tif", "nodata": None, "years": decimal,
            "days": [int((np.datetime64(d) - np.datetime64("1970-01-01")) / np.timedelta64(1, "D")) for d in days],
            "window": [0, 0, W, H], "threads": 1, "granules": [[m] for m in meta]}
    (HERE / "series" / "spec.json").write_text(json.dumps(spec))

    xs, ys = centres(x0, y0, res, res, W, H)
    time = pd.to_datetime(days)
    out = {"granules": meta, "days": days, "windows": {}}
    cases = [
        ("whole", (0, 0, W, H), {"sza": "scene", "cfactor": True}, {"sza_fixed": 30.0}),
        ("window", (2, 1, 19, 15), {"sza": "scene", "cfactor": False}, {}),
        ("fixed", (0, 0, W, H), {"sza": "fixed", "cfactor": True}, {"sza_fixed": 33.0}),
    ]
    for name, (a, b, c, d), params, extra in cases:
        sel = (slice(b, d), slice(a, c))
        like = xr.DataArray(
            np.stack([refl[r][:, sel[0], sel[1]] for r in roles], axis=1).astype(np.float64),
            dims=("time", "band", "y", "x"),
            coords={"time": time, "band": roles, "y": ys[sel[0]], "x": xs[sel[1]]}).rio.write_crs(32723)
        angles = zeit.sentinel2_angles([str(HERE / m) for m in meta], like, view_band="B06", n_jobs=1)
        sza = "scene" if params["sza"] == "scene" else extra["sza_fixed"]
        res_ = zeit.nbar(like, angles, sza=sza, bands={r: r for r in roles}, nodata=None, n_jobs=1,
                         return_cfactor=params["cfactor"])
        nbar, cf = res_ if params["cfactor"] else (res_, None)

        def clean(v):
            return [None if not np.isfinite(x) else float(f"{x:.10g}") for x in np.asarray(v, dtype=np.float64).reshape(-1)]

        pick = lambda arr: [[clean(arr.sel(band=r).isel(time=k)) for k in range(T)] for r in roles]
        entry = {"params": {**params, **extra}, "window": [a, b, c, d], "roles": roles, "values": pick(nbar),
                 "cfactor": pick(cf) if cf is not None else None, "sza": clean(nbar.nbar_sza.values),
                 "angles": {k: [clean(angles[k].isel(time=i)) for i in range(T)] for k in ("SZA", "SAA", "VZA", "VAA")}}
        # the pixels of the pixel runs (x, y in the whole grid): one pixel cube each, as the tool's pixel run gets
        pixels = {}
        for (px, py) in [(3, 2), (10, 8), (0, 0), (17, 12), (6, 14)]:
            if not (a <= px < c and b <= py < d):
                continue
            one = xr.DataArray(
                np.stack([refl[r][:, py:py + 1, px:px + 1] for r in roles], axis=1).astype(np.float64),
                dims=("time", "band", "y", "x"),
                coords={"time": time, "band": roles, "y": ys[py:py + 1], "x": xs[px:px + 1]}).rio.write_crs(32723)
            ang = zeit.sentinel2_angles([str(HERE / m) for m in meta], one, view_band="B06", n_jobs=1)
            r1 = zeit.nbar(one, ang, sza=sza, bands={r: r for r in roles}, nodata=None, n_jobs=1,
                           return_cfactor=params["cfactor"])
            n1, c1 = r1 if params["cfactor"] else (r1, None)
            pixels[f"{px},{py}"] = {
                "values": [[clean(n1.sel(band=r).isel(time=k)) for k in range(T)] for r in roles],
                "cfactor": [[clean(c1.sel(band=r).isel(time=k)) for k in range(T)] for r in roles] if c1 is not None else None,
                "angles": {k: clean(ang[k].values.reshape(T, -1)[:, 0]) for k in ("SZA", "SAA", "VZA", "VAA")}}
        entry["pixels"] = pixels
        out["windows"][name] = entry
        valid = np.isfinite(np.array(entry["values"][0], dtype=float)).sum(axis=1)
        print("series", name, "scene sza", entry["sza"], "values per date", valid.tolist())
    (HERE / "nbar.json").write_text(json.dumps(out))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else r"C:\Users\eduar\Documents\dev\zeit\tests\data")
