#!/usr/bin/env python3
"""Check the omnibus test of site/envi.html against Sentinel-2: the numbers of that guide's "How to read them".

    python tools/check_envi_omnibus.py RUN [BOX]

RUN is the folder of the omnibus run that `janus --shots envi` made (omnibus_first_change.tif, omnibus_n_changes.tif and
omnibus_first_direction.tif in it; the tour prints it as "omnibus: ... folder ..."). BOX is the area of the series in
degrees, W,S,E,N (default: tools/make_envi_data.py's). Planetary Computer's Sentinel-2 L2A is read on the grid of the
run's maps, three dates of the tile with the fewest clouds, as NDVI with the scene classification's clouds out:

    before  the winter of 2022 (June to August)
    after1  June 2024
    after2  October 15 to December 2024

A pixel is forest lost when its NDVI is 0.75 or more before and below 0.5 on both later dates (a smoke haze lowers the
NDVI of one date, not of two months apart), and forest kept when it is 0.75 or more on all three. Prints the share of
each in the area, how many of the lost have a change in the run and its direction and date, and how many of the kept have
one. Needs rasterio, pystac-client and planetary-computer (the geo environment).
"""

import sys
from pathlib import Path

import numpy as np
import planetary_computer
import pystac_client
import rasterio
from rasterio.enums import Resampling
from rasterio.vrt import WarpedVRT

DEFAULT_BOX = (-62.839, -10.757, -62.759, -10.677)
WINDOWS = {"before": ("2022-06-01", "2022-08-31"), "after1": ("2024-05-01", "2024-06-30"), "after2": ("2024-10-15", "2024-12-31")}
BAD_SCL = [0, 1, 3, 8, 9, 10, 11]  # no data, saturated, shadow, cloud (medium, high), cirrus, snow


def ndvi(catalog, box, grid, window):
    items = list(
        catalog.search(
            collections=["sentinel-2-l2a"],
            bbox=box,
            datetime="/".join(window),
            query={"eo:cloud_cover": {"lt": 20}},
        ).items()
    )
    items.sort(key=lambda i: i.properties["eo:cloud_cover"])
    for item in items[:5]:
        bands = {}
        for k in ("B04", "B08", "SCL"):
            with rasterio.open(item.assets[k].href) as src:
                with WarpedVRT(src, crs=grid["crs"], transform=grid["transform"], width=grid["w"], height=grid["h"],
                               resampling=Resampling.nearest) as vrt:
                    bands[k] = vrt.read(1).astype("f4")
        red, nir = (bands["B04"] - 1000) / 1e4, (bands["B08"] - 1000) / 1e4  # the offset of the 2022 processing baseline
        nd = (nir - red) / (nir + red + 1e-6)
        nd[np.isin(bands["SCL"], BAD_SCL)] = np.nan
        if np.isnan(nd).mean() < 0.05:
            print(f"  {window[0]}..{window[1]}: {item.id}, {100 * (1 - np.isnan(nd).mean()):.1f} % clear")
            return nd
    raise SystemExit(f"no clear scene in {window}")


def main() -> None:
    if len(sys.argv) not in (2, 3):
        raise SystemExit(__doc__)
    run = Path(sys.argv[1])
    box = tuple(float(v) for v in sys.argv[2].split(",")) if len(sys.argv) == 3 else DEFAULT_BOX
    with rasterio.open(run / "omnibus_first_change.tif") as src:
        first, grid = src.read(1), {"crs": src.crs, "transform": src.transform, "w": src.width, "h": src.height}
    n_changes = rasterio.open(run / "omnibus_n_changes.tif").read(1)
    direction = rasterio.open(run / "omnibus_first_direction.tif").read(1)  # 1 none, 2 brightening, 3 darkening, 4 indefinite
    catalog = pystac_client.Client.open(
        "https://planetarycomputer.microsoft.com/api/stac/v1", modifier=planetary_computer.sign_inplace
    )
    before, after1, after2 = (ndvi(catalog, box, grid, w) for w in WINDOWS.values())
    lost = (before > 0.75) & (after1 < 0.5) & (after2 < 0.5)
    kept = (before > 0.75) & (after1 > 0.75) & (after2 > 0.75)
    changed = n_changes >= 1
    print(f"forest lost: {100 * lost.mean():.1f} % of the area; forest kept: {100 * kept.mean():.1f} %")
    print(f"pixels with a change in the run: {100 * changed.mean():.1f} %")
    print(f"forest lost with a change: {100 * (lost & changed).sum() / lost.sum():.1f} %")
    print(f"changed pixels that are forest lost: {100 * (lost & changed).sum() / changed.sum():.1f} %")
    print(f"forest kept with a change: {100 * (kept & changed).sum() / kept.sum():.2f} %")
    both = lost & changed
    for k, name in ((2, "brightening"), (3, "darkening"), (4, "indefinite")):
        print(f"  first change of the forest lost, {name}: {100 * ((direction == k) & both).sum() / both.sum():.1f} %")
    q = np.nanpercentile(first[both], [10, 25, 50, 75, 90])
    print("  its date (decimal year), 10 / 25 / 50 / 75 / 90 %:", " ".join(f"{v:.2f}" for v in q))


if __name__ == "__main__":
    main()
