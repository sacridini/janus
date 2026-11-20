#!/usr/bin/env python3
"""Put the screenshots of `janus --shots eof` where site/eof.html names them: site/assets/img/eof/.

    janus --threads 4 --shots eof EVI_series.tif <shots folder>
    python tools/make_eof_figures.py <shots folder>
    python tools/make_webp.py && python tools/build_site.py

The full-window pictures (01, 04, 05, 06, 11) become JPEG (the guides keep those as JPEG, with a WebP twin made by
tools/make_webp.py), the windows and the panels stay PNG, and the two figures of the modes' maps (14: EOF 2 and 3 not
rotated; 15: the first three rotated) are made from the parts the tour wrote in parts/: the map of each mode cropped
to the data, with its colour bar and name under it, side by side. Needs Pillow.
"""

import shutil
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "assets" / "img" / "eof"
JPEG = ["01-series-rain", "04-best-r", "05-best-lag", "06-significant", "11-eof-maps"]
PNG = [
    "02-climate-window", "03-lagcorr-window", "07-modes-lag", "08-chart-unshifted", "09-chart-shifted",
    "10-eof-window", "12-modes-eof", "13-chart-overlay",
]
# pictures cut above a row the panel only half shows (name: the height kept, in pixels)
HEIGHT = {"12-modes-eof": 777}
BACKGROUND = (10, 10, 10)
# the part of a map panel's picture (1407 x 628 px at the tour's window size) with the data, and the one with its name
# and colour bar; the toolbar above the map and the series' own bar are left out
DATA = (440, 78, 960, 490)
LEGEND = (14, 428, 440, 474)


def panel(path: Path) -> Image.Image:
    img = Image.open(path).convert("RGB")
    data = img.crop(DATA)
    legend = img.crop(LEGEND)
    out = Image.new("RGB", (data.width, data.height + legend.height + 6), BACKGROUND)
    out.paste(data, (0, 0))
    out.paste(legend, (6, data.height + 6))
    return out


def side_by_side(parts: list[Path], target: Path) -> None:
    panels = [panel(p) for p in parts]
    gap = 14
    w = sum(p.width for p in panels) + gap * (len(panels) - 1)
    out = Image.new("RGB", (w, max(p.height for p in panels)), BACKGROUND)
    x = 0
    for p in panels:
        out.paste(p, (x, 0))
        x += p.width + gap
    out.save(target, optimize=True)
    print("wrote", target, out.size)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    src = Path(sys.argv[1])
    OUT.mkdir(parents=True, exist_ok=True)
    for name in JPEG:
        img = Image.open(src / f"{name}.png").convert("RGB")
        img.save(OUT / f"{name}.jpg", quality=88, optimize=True)
        print("wrote", OUT / f"{name}.jpg", img.size)
    for name in PNG:
        if name in HEIGHT:
            img = Image.open(src / f"{name}.png").convert("RGB")
            img.crop((0, 0, img.width, HEIGHT[name])).save(OUT / f"{name}.png", optimize=True)
        else:
            shutil.copyfile(src / f"{name}.png", OUT / f"{name}.png")
        print("wrote", OUT / f"{name}.png")
    parts = src / "parts"
    side_by_side([parts / "14a-eof2.png", parts / "14b-eof3.png"], OUT / "14-unrotated.png")
    side_by_side([parts / "15a-rot1.png", parts / "15b-rot2.png", parts / "15c-rot3.png"], OUT / "15-rotated.png")


if __name__ == "__main__":
    main()
