#!/usr/bin/env python3
"""Put the screenshots of `janus --shots dataprep` where site/data-preparation.html names them:
site/assets/img/data-preparation/dp-NN-slug.(png|jpg).

    janus --threads 4 --shots dataprep <data folder> <shots folder>
    python tools/make_dataprep_figures.py <shots folder>
    python tools/make_webp.py && python tools/build_site.py

(tools/make_dataprep_data.py says how the data folder is made.) The windows and charts stay PNG; the figures made of
several pictures are put together here from what the tour wrote: the chart of NBAR and the c-factor side by side, the
two maps of the ridge (the part of each around the ridge, its legend under it, with a title) side by side as JPEG, the
arrows of the co-registration cropped to the map; the surface reflectance of the Level 1C scene beside its quality
flags (the flags' legend drawn here, from the classes the tour's expression gives them), Tmask's share flagged beside
the series of a date before and after it, the count of clear observations of a wet and of a dry month, the windows and
charts of Interpolate, Smoothing and Standardize with their tables, and ImproPhe's coarse layer beside its result.
Needs Pillow and numpy.
"""

import shutil
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "assets" / "img" / "data-preparation"
BACKGROUND = (11, 11, 11)
GAP = 14
# the range of the NIR the two maps of the ridge are drawn in (the tour prints it: "NIR range 0.06276 to 0.4239")
RANGE = (0.06276, 0.4239)
# the tour's file -> the guide's, for the pictures used as they are
PNG = {
    "01-menu": "dp-01-menu",
    "08-nbar-window": "dp-08-nbar-window",
    "10-spectral-window": "dp-10-spectral-window",
    "11-harmonize-window": "dp-11-harmonize-window",
    "12-harmonize-chart": "dp-12-harmonize-chart",
    "13-topocorr-window": "dp-13-topocorr-window",
    "15-topocorr-chart": "dp-15-topocorr-chart",
    "16-coreg-window": "dp-16-coreg-window",
    "02-level2-window": "dp-02-level2-window",
    "04-tmask-window": "dp-04-tmask-window",
    "21-improphe-window": "dp-21-improphe-window",
}


def nonbackground(img: Image.Image, box, threshold=12):
    """The bounding box, in the picture's pixels, of what differs from the background inside `box`."""
    a = np.asarray(img.convert("RGB").crop(box)).astype(int)
    m = np.abs(a - np.array(BACKGROUND)).max(axis=2) > threshold
    ys, xs = np.where(m)
    if not len(xs):
        raise SystemExit(f"nothing in {box}")
    return box[0] + int(xs.min()), box[1] + int(ys.min()), box[0] + int(xs.max()) + 1, box[1] + int(ys.max()) + 1


def font(size=18):
    try:
        return ImageFont.truetype("arial.ttf", size)
    except OSError:
        return ImageFont.load_default(size=size)


def titled(img: Image.Image, text: str) -> Image.Image:
    """The picture with a line of text above it."""
    f = font(18)
    h = 30
    out = Image.new("RGB", (img.width, img.height + h), BACKGROUND)
    ImageDraw.Draw(out).text((8, 4), text, fill=(232, 232, 232), font=f)
    out.paste(img, (0, h))
    return out


def side_by_side(images, gap=GAP) -> Image.Image:
    w = sum(i.width for i in images) + gap * (len(images) - 1)
    out = Image.new("RGB", (w, max(i.height for i in images)), BACKGROUND)
    x = 0
    for i in images:
        out.paste(i, (x, 0))
        x += i.width + gap
    return out


def save_png(img: Image.Image, name: str) -> None:
    img.convert("RGB").save(OUT / f"{name}.png", optimize=True)
    print("wrote", OUT / f"{name}.png", img.size)


def save_jpeg(img: Image.Image, name: str, width: int = 1600) -> None:
    img = img.convert("RGB")
    if img.width > width:
        img = img.resize((width, round(img.height * width / img.width)), Image.LANCZOS)
    img.save(OUT / f"{name}.jpg", quality=88, optimize=True)
    print("wrote", OUT / f"{name}.jpg", img.size)


def ridge(src: Path) -> None:
    """The map of the date of the lowest sun before and after the correction: the same area and range of values, the
    part of the map panel around the ridge, its legend under it."""
    panels = []
    for part, title in (("14a-ridge-before", "As observed"), ("14b-ridge-after", "After the correction (SCS+C)")):
        img = Image.open(src / "parts" / f"{part}.png").convert("RGB")
        w, h = img.size
        data = img.crop((w // 2 - 350, 78, w // 2 + 350, h - 52))
        # the colour bar (over the foot of the map's left, the same place on every map panel), its two numbers written
        # under it on a plain ground
        bar = img.crop((20, h - 49, 241, h - 36))
        legend = Image.new("RGB", (bar.width + 40, 36), BACKGROUND)
        legend.paste(bar, (20, 2))
        draw = ImageDraw.Draw(legend)
        draw.text((14, 17), f"{RANGE[0]:.3f}", fill=(232, 232, 232), font=font(15))
        draw.text((bar.width - 16, 17), f"{RANGE[1]:.3f}", fill=(232, 232, 232), font=font(15))
        cell = Image.new("RGB", (data.width, data.height + legend.height + 6), BACKGROUND)
        cell.paste(data, (0, 0))
        cell.paste(legend, (0, data.height + 6))
        panels.append(titled(cell, title))
    save_jpeg(side_by_side(panels), "dp-14-topocorr-ridge")


def arrows(src: Path) -> None:
    """The map of the first date with the arrows of the shifts, cropped to the data and the arrows that leave it."""
    img = Image.open(src / "17-coreg-arrows-map.png").convert("RGB")
    w, h = img.size
    # the toolbar and the date's slider (the first 76 px) and the colour bar (the foot of the left) left out
    box = nonbackground(img, (300, 78, w - 300, h - 4), threshold=30)
    pad = 10
    crop = img.crop((max(0, box[0] - pad), 78, min(w, box[2] + pad), min(h, box[3] + pad)))
    save_jpeg(crop, "dp-17-coreg-arrows")


def map_panel(src: Path, part: str, title: str, legend: Image.Image | bool | None = None) -> Image.Image:
    """A map part: the data of the map panel (the toolbar, the date's label and the legend of the panel left out), the
    panel's own colour bar with its numbers under it (or `legend`, drawn here; False: none), a title above."""
    img = Image.open(src / "parts" / f"{part}.png").convert("RGB")
    w, h = img.size
    box = nonbackground(img, (250, 80, w - 20, h - 8), threshold=20)
    data = img.crop(box)
    if legend is False:
        return titled(data, title)
    if legend is None:
        # the bar and its numbers (and a name above it) at the foot of the panel's left
        region = (12, h - 70, 280, h - 8)
        lb = nonbackground(img, region, threshold=20)
        legend = img.crop((lb[0] - 2, lb[1] - 2, lb[2] + 2, lb[3] + 2))
    cell = Image.new("RGB", (max(data.width, legend.width + 20), data.height + legend.height + 10), BACKGROUND)
    cell.paste(data, (0, 0))
    cell.paste(legend, (10, data.height + 8))
    return titled(cell, title)


def maps_figure(src: Path, name: str, panels) -> None:
    save_jpeg(side_by_side([map_panel(src, *p) for p in panels]), name)


# the quality flags' classes as the tour's expression gives them and its legend gives them colours (shots_dataprep.rs,
# QAI_CLASSES)
QAI_CLASSES = [
    ("clear", (70, 70, 70)),
    ("cloud buffer", (140, 190, 255)),
    ("cloud", (255, 255, 255)),
    ("cloud shadow", (150, 60, 190)),
    ("water", (30, 100, 230)),
]


def qai_legend() -> Image.Image:
    """A row of swatches with the classes' names."""
    f = font(15)
    out = Image.new("RGB", (560, 24), BACKGROUND)
    d = ImageDraw.Draw(out)
    x = 2
    for name, color in QAI_CLASSES:
        d.rectangle((x, 5, x + 13, 18), fill=color, outline=(120, 120, 120))
        d.text((x + 19, 3), name, fill=(232, 232, 232), font=f)
        x += 19 + int(d.textlength(name, font=f)) + 16
    return out.crop((0, 0, x, 24))


def stacked(images, gap=GAP) -> Image.Image:
    w = max(i.width for i in images)
    out = Image.new("RGB", (w, sum(i.height for i in images) + gap * (len(images) - 1)), BACKGROUND)
    y = 0
    for i in images:
        out.paste(i, (0, y))
        y += i.height + gap
    return out


def shot(src: Path, name: str) -> Image.Image:
    return Image.open(src / f"{name}.png").convert("RGB")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    src = Path(sys.argv[1])
    OUT.mkdir(parents=True, exist_ok=True)
    for name, final in PNG.items():
        shutil.copyfile(src / f"{name}.png", OUT / f"{final}.png")
        print("wrote", OUT / f"{final}.png")
    # NBAR: the NIR before and after, and the c-factors that made the difference
    a = Image.open(src / "09-nbar-chart.png").convert("RGB")
    b = Image.open(src / "09-nbar-cfactor.png").convert("RGB")
    save_png(side_by_side([a, b]), "dp-09-nbar-chart")
    ridge(src)
    arrows(src)


if __name__ == "__main__":
    main()
