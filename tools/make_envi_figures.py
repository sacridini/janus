#!/usr/bin/env python3
"""Put the screenshots of `janus --shots envi` where site/envi.html names them: site/assets/img/envi/.

    JANUS_ENVI_S1=<radar folder> janus --threads 4 --shots envi <Landsat folder> <shots folder>
    python tools/make_envi_figures.py <shots folder>
    python tools/make_webp.py && python tools/build_site.py

(tools/make_envi_data.py says how the two folders are made.) The full-window pictures become JPEG (the guides keep
those as JPEG, with a WebP twin made by tools/make_webp.py), the windows, the charts and the maps of classes stay PNG. The figures made of
several maps are put together from the parts the tour wrote in parts/: the map of each cropped to the data, with its
legend under it, side by side; the comparison of the denoising is two crops of the same zoomed area. The chart of the
MNF's eigenvalues is drawn from the run's table (data/mnf_eigenvalues.csv, which the tour copies). Needs Pillow and
matplotlib.
"""

import csv
import shutil
import sys
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "site" / "assets" / "img" / "envi"
BACKGROUND = (11, 11, 11)
# the figures, in the order of the page: the tour's file -> the guide's
JPEG = {
    "01-landsat-series": "01-landsat-series",
    "05-cva-magnitude": "07-cva-magnitude",
    "06-cva-sectors": "08-cva-sectors",
    "19-omnibus-pin": "14-omnibus-pin",
    "16-omnibus-first-change": "16-omnibus-first-change",
    "18-omnibus-direction": "17-omnibus-direction",
}
PNG = {
    "02-mnf-window": "02-mnf-window",
    "04-cva-window": "06-cva-window",
    "07-imad-window": "09-imad-window",
    "10-radcal-window": "12-radcal-window",
    "11-radcal-chart": "13-radcal-chart",
}
GAP = 14


def nonbackground(img: Image.Image, box: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    """The bounding box, in the picture's pixels, of what differs from the background inside `box`."""
    a = np.asarray(img.convert("RGB").crop(box)).astype(int)
    m = (np.abs(a - np.array(BACKGROUND)).max(axis=2) > 12)
    ys, xs = np.where(m)
    if not len(xs):
        raise SystemExit(f"nothing in {box}")
    return box[0] + int(xs.min()), box[1] + int(ys.min()), box[0] + int(xs.max()) + 1, box[1] + int(ys.max()) + 1


def parts_panel(img: Image.Image, classes: bool) -> tuple[Image.Image, Image.Image]:
    """A map panel's picture (1407 x 667 px at the tour's size) cut into the data and its legend (the toolbar, the date
    and the series' own bar left out). A class map's legend is in rows under the data, a map of values' is the bar at
    the foot of the left."""
    if classes:
        data = img.crop(nonbackground(img, (330, 78, 1100, 574)))
        leg = img.crop(nonbackground(img, (8, 576, 1398, 656)))
    else:
        data = img.crop(nonbackground(img, (360, 78, 1100, 656)))
        leg = img.crop(nonbackground(img, (8, 590, 400, 656)))
    return data, leg


def together(panels: list[tuple[Image.Image, Image.Image]], classes: bool, cols: int | None = None) -> Image.Image:
    """The panels side by side (rows of `cols`): a class map's legend once at the foot, a value map's under each."""
    cols = cols or len(panels)
    rows = [panels[i : i + cols] for i in range(0, len(panels), cols)]
    row_imgs = []
    for row in rows:
        if classes:
            w = sum(d.width for d, _ in row) + GAP * (len(row) - 1)
            h = max(d.height for d, _ in row)
            img = Image.new("RGB", (w, h), BACKGROUND)
            x = 0
            for d, _ in row:
                img.paste(d, (x, 0))
                x += d.width + GAP
        else:
            cells = []
            for d, leg in row:
                cell = Image.new("RGB", (max(d.width, leg.width), d.height + leg.height + 6), BACKGROUND)
                cell.paste(d, (0, 0))
                cell.paste(leg, (0, d.height + 6))
                cells.append(cell)
            w = sum(c.width for c in cells) + GAP * (len(cells) - 1)
            img = Image.new("RGB", (w, max(c.height for c in cells)), BACKGROUND)
            x = 0
            for c in cells:
                img.paste(c, (x, 0))
                x += c.width + GAP
        row_imgs.append(img)
    w = max(i.width for i in row_imgs)
    out = Image.new("RGB", (w, sum(i.height for i in row_imgs) + GAP * (len(row_imgs) - 1)), BACKGROUND)
    y = 0
    for i in row_imgs:
        out.paste(i, (0, y))
        y += i.height + GAP
    if classes:
        leg = panels[0][1]
        full = Image.new("RGB", (max(out.width, leg.width), out.height + leg.height + 8), BACKGROUND)
        full.paste(out, (0, 0))
        full.paste(leg, (0, out.height + 8))
        out = full
    return out


def save(img: Image.Image, name: str, width: int = 1600, jpeg: bool = False) -> None:
    """A figure, at most `width` pixels wide (the maps of the parts are taken at the screen's size): JPEG for the maps
    of values (noisy, and 4 times smaller), PNG for the flat colours of the classes."""
    if img.width > width:
        img = img.resize((width, round(img.height * width / img.width)), Image.LANCZOS)
    if jpeg:
        img.save(OUT / f"{name}.jpg", quality=88, optimize=True)
    else:
        img.save(OUT / f"{name}.png", optimize=True)
    print("wrote", OUT / f"{name}.{'jpg' if jpeg else 'png'}", img.size)


def eigenvalues(src: Path) -> None:
    """The MNF's components: the SNR of each (eigenvalue minus 1), the share of the signal they hold."""
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    with open(src / "data" / "mnf-mnf_eigenvalues.csv", newline="") as f:
        rows = list(csv.DictReader(f))
    k = [int(r["component"]) for r in rows]
    snr = [float(r["snr"]) for r in rows]
    share = [float(r["cumulative_signal_share"]) * 100 for r in rows]
    fg, muted, accent, line = "#e8e8e8", "#9a9a9a", "#e8a33d", "#5ab0e0"
    fig, ax = plt.subplots(figsize=(7.2, 3.6), dpi=150)
    fig.patch.set_facecolor("#0b0b0b")
    ax.set_facecolor("#141414")
    bars = ax.bar(k, snr, color=accent, width=0.62)
    ax.axhline(1.0, color=muted, lw=1, ls="--")
    ax.text(k[-1] + 0.45, 1.0, "SNR 1", color=muted, va="bottom", ha="right", fontsize=8)
    for b, v in zip(bars, snr):
        ax.text(b.get_x() + b.get_width() / 2, v + 0.25, f"{v:.1f}", ha="center", color=fg, fontsize=8)
    ax.set_xlabel("MNF component", color=fg)
    ax.set_ylabel("Signal-to-noise ratio (eigenvalue - 1)", color=fg)
    ax.set_xticks(k)
    ax.tick_params(colors=fg)
    for s in ax.spines.values():
        s.set_color("#333333")
    ax2 = ax.twinx()
    ax2.plot(k, share, color=line, marker="o", ms=4, lw=1.4)
    ax2.set_ylim(0, 105)
    ax2.set_ylabel("Cumulative share of the signal (%)", color=line)
    ax2.tick_params(colors=line)
    for s in ax2.spines.values():
        s.set_color("#333333")
    fig.tight_layout()
    fig.savefig(OUT / "04-mnf-eigenvalues.png", facecolor=fig.get_facecolor())
    plt.close(fig)
    print("wrote", OUT / "04-mnf-eigenvalues.png")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    src = Path(sys.argv[1])
    parts = src / "parts"
    OUT.mkdir(parents=True, exist_ok=True)
    for name, final in JPEG.items():
        img = Image.open(src / f"{name}.png").convert("RGB")
        img.save(OUT / f"{final}.jpg", quality=88, optimize=True)
        print("wrote", OUT / f"{final}.jpg", img.size)
    for name, final in PNG.items():
        shutil.copyfile(src / f"{name}.png", OUT / f"{final}.png")
        print("wrote", OUT / f"{final}.png")
    # the two charts of the radar's pixels side by side: the one that changed, the one that did not
    a, b = Image.open(src / "19-omnibus-pin-chart.png").convert("RGB"), Image.open(src / "22-omnibus-stable.png").convert("RGB")
    both = Image.new("RGB", (a.width + b.width + GAP, max(a.height, b.height)), BACKGROUND)
    both.paste(a, (0, 0))
    both.paste(b, (a.width + GAP, 0))
    both.save(OUT / "15-omnibus-charts.png", optimize=True)
    print("wrote", OUT / "15-omnibus-charts.png", both.size)
    # the MNF's maps, six components in two rows
    load = lambda n: Image.open(parts / f"{n}.png").convert("RGB")
    save(
        together([parts_panel(load(f"03{c}-mnf{k}"), False) for k, c in enumerate("abcdef", 1)], False, cols=3),
        "03-mnf-maps",
        width=1500,
        jpeg=True,
    )
    eigenvalues(src)
    # the SWIR2 as it is and rebuilt from the components kept: the same area, a crop of each
    box = (200, 150, 900, 560)
    o, d = load("04a-blue-original").crop(box), load("04b-blue-denoised").crop(box)
    both = Image.new("RGB", (o.width * 2 + GAP, o.height), BACKGROUND)
    both.paste(o, (0, 0))
    both.paste(d, (o.width + GAP, 0))
    both.save(OUT / "05-mnf-denoised.png", optimize=True)
    print("wrote", OUT / "05-mnf-denoised.png", both.size)
    save(
        together([parts_panel(load(f"08{c}-mad{k}"), False) for k, c in enumerate("abc", 1)], False),
        "10-imad-variates",
        jpeg=True,
    )
    save(
        together([parts_panel(load("09a-pnochange"), False), parts_panel(load("09b-change"), False)], False),
        "11-imad-probability",
        jpeg=True,
    )
    save(
        together(
            [parts_panel(load("12a-magnitude-original"), False), parts_panel(load("12b-magnitude-normalized"), False)],
            False,
        ),
        "18-chain-magnitude",
        jpeg=True,
    )
    save(
        together([parts_panel(load("13a-sectors-original"), True), parts_panel(load("13b-sectors-normalized"), True)], True),
        "19-chain-sectors",
    )


if __name__ == "__main__":
    main()
