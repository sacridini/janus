"""The data and the figures of the trend test guide (site/trend-test.html).

Usage: python tools/make_parts_guide.py data SERIES OUT
  SERIES  a folder of annual rasters of an index, one per year (landsat_medoid_YYYY.tif:
          the guide's 41 Landsat medoid composites of 1985-2025 over Sergipe and
          Alagoas, EPSG:4326, 30 m), with 0 outside the data and over the sea
  OUT     a folder: OUT/series/*.vrt (each year with 0 as no-data, so the sea is not
          taken as values) and OUT/zones.tif (the zones by distance to the sea, on the
          series' grid: 1 coastal, under 40 km; 2 transition, 40 to 100 km; 3 interior,
          beyond 100 km; 0, no class, over water), with a colour table and the names
          Janus shows. Needs GDAL's Python bindings, numpy and scipy.

       python tools/make_parts_guide.py figures OUT
  OUT     site/assets/img/trend-test: the figures that explain the method, made by
          running Zeit's ar_trend and parts (the methods of Janus's trend test) on
          simulated maps with a known spatial correlation. Needs the zeit package
          (https://github.com/sacridini/zeit-cdts), matplotlib and Pillow.

The screenshots: see site/README.md.
"""
import os
import sys
import time

import numpy as np

# the guide's classes
ZONES = [(1, "Coastal (< 40 km)", (201, 228, 222)), (2, "Transition (40-100 km)", (127, 179, 168)),
         (3, "Interior (> 100 km)", (61, 122, 111))]


# ---------------------------------------------------------------------------
# data
# ---------------------------------------------------------------------------
def data(series, out):
    from osgeo import gdal
    from scipy import ndimage

    gdal.UseExceptions()
    files = sorted(f for f in os.listdir(series) if f.lower().endswith(".tif"))
    os.makedirs(os.path.join(out, "series"), exist_ok=True)
    for f in files:
        gdal.Translate(os.path.join(out, "series", f[:-4] + ".vrt"), os.path.join(series, f), format="VRT",
                       noData=0)
    print(f"{len(files)} years as VRTs with 0 as no-data: {os.path.join(out, 'series')}")

    # the water and the sea, on a grid 10 times coarser (nearest pixel of each cell)
    first = gdal.Open(os.path.join(series, files[0]))
    w, h = first.RasterXSize, first.RasterYSize
    gt = first.GetGeoTransform()
    k = 10
    bw, bh = -(-w // k), -(-h // k)
    total = np.zeros((bh, bw))
    count = np.zeros((bh, bw))
    for f in files:
        src = gdal.Open(os.path.join(series, f))
        a = src.GetRasterBand(1).ReadAsArray(buf_xsize=bw, buf_ysize=bh,
                                             resample_alg=gdal.GRIORA_NearestNeighbour).astype(float)
        ok = np.isfinite(a) & (a != 0)
        total += np.where(ok, a, 0)
        count += ok
    mean = np.where(count > 0, total / np.maximum(count, 1), np.nan)
    water = ~(mean >= 0.05) | (count < 30)
    labels, _ = ndimage.label(water)
    sea = labels == labels[-1, -1]  # the water touching the south-east corner
    # distances in km, the degrees projected at the mean latitude (as the trend test does)
    lat0 = gt[3] + gt[5] * h / 2
    km = 6371.0088 * np.pi / 180
    dy = abs(gt[5]) * h / bh * km
    dx = gt[1] * w / bw * km * np.cos(np.radians(lat0))
    dist = ndimage.distance_transform_edt(~sea, sampling=(dy, dx))
    small = np.where(water, 0, np.where(dist < 40, 1, np.where(dist < 100, 2, 3))).astype(np.uint8)
    zones = np.repeat(np.repeat(small, k, axis=0), k, axis=1)[:h, :w]

    path = os.path.join(out, "zones.tif")
    ds = gdal.GetDriverByName("GTiff").Create(path, w, h, 1, gdal.GDT_Byte,
                                               options=["COMPRESS=DEFLATE", "TILED=YES"])
    ds.SetGeoTransform(gt)
    ds.SetProjection(first.GetProjection())
    band = ds.GetRasterBand(1)
    table = gdal.ColorTable()
    table.SetColorEntry(0, (0, 0, 0, 0))
    for value, _, rgb in ZONES:
        table.SetColorEntry(value, (*rgb, 255))
    band.SetRasterColorTable(table)
    band.SetCategoryNames([""] + [name for _, name, _ in ZONES])
    band.SetNoDataValue(0)
    band.WriteArray(zones)
    ds = None
    shares = {name: f"{100 * np.mean(small == v) / np.mean(small > 0):.0f}%" for v, name, _ in ZONES}
    print(f"zones {w} x {h}: {path} {shares}")


# ---------------------------------------------------------------------------
# figures (the site's colours: window #15161A, text #E6E6E6, blue #4296FA, orange #F29A38)
# ---------------------------------------------------------------------------
BG, PANEL, TEXT, MUTE, RULE = "#15161A", "#1C2A3C", "#E6E6E6", "#A0A7B2", "#33405A"
BLUE, ORANGE, PALE, GREEN = "#4296FA", "#F29A38", "#CEE3FF", "#3FB68B"
UTM = "EPSG:31984"
RANGE_PX = 15  # the simulated range, in 30 m pixels: 450 m


def fields(rng, n, range_px, k):
    """k independent n x n Gaussian fields of unit variance whose correlation is
    exp(-d / range_px), d in pixels (circulant embedding on a 2n x 2n torus)."""
    m = 2 * n
    i = np.minimum(np.arange(m), m - np.arange(m))
    d = np.hypot(i[:, None], i[None, :])
    lam = np.clip(np.fft.fft2(np.exp(-d / range_px)).real, 0, None)
    out = []
    while len(out) < k:
        e = rng.standard_normal((m, m)) + 1j * rng.standard_normal((m, m))
        f = np.fft.fft2(np.sqrt(lam / (m * m)) * e)
        out += [f.real[:n, :n], f.imag[:n, :n]]
    return np.array(out[:k])


def synthetic(seed, n=60, nt=30, slope=0.0, rho=0.3, nugget=0.2, sd=0.03):
    """A projected (time, y, x) cube of 30 m pixels: 0.5 + slope * t + sd * e, e AR(1) in
    time whose innovations are spatially correlated (RANGE_PX pixels, a nugget)."""
    import pandas as pd
    import xarray as xr
    rng = np.random.default_rng(seed)
    f = fields(rng, n, RANGE_PX, nt)
    innov = np.sqrt(1 - nugget) * f + np.sqrt(nugget) * rng.standard_normal(f.shape)
    e = np.empty_like(innov)
    e[0] = innov[0] / np.sqrt(1 - rho ** 2)
    for t in range(1, nt):
        e[t] = rho * e[t - 1] + innov[t]
    values = 0.5 + slope * np.arange(nt)[:, None, None] + sd * e
    cube = xr.DataArray(values, dims=("time", "y", "x"), coords={
        "time": pd.date_range("1995-01-01", periods=nt, freq="YS"),
        "y": 8_900_000.0 - 30.0 * np.arange(n), "x": 600_000.0 + 30.0 * np.arange(n)})
    return cube.rio.write_crs(UTM)


def naive_p(slopes):
    from scipy import stats
    s = np.asarray(slopes, dtype=float).ravel()
    return float(stats.ttest_1samp(s[np.isfinite(s)], 0.0).pvalue)


def style():
    import matplotlib.pyplot as plt
    plt.rcParams.update({
        "font.family": ["IBM Plex Sans", "Segoe UI", "DejaVu Sans"], "font.size": 10.5,
        "text.color": TEXT, "axes.labelcolor": MUTE, "axes.edgecolor": RULE, "axes.facecolor": BG,
        "figure.facecolor": BG, "savefig.facecolor": BG, "axes.titlesize": 11.5, "axes.titlelocation": "left",
        "axes.titlecolor": PALE, "axes.titlepad": 10, "axes.spines.top": False, "axes.spines.right": False,
        "axes.grid": True, "axes.axisbelow": True, "grid.color": "#262A31", "xtick.color": MUTE,
        "ytick.color": MUTE, "xtick.major.size": 0, "ytick.major.size": 0, "legend.frameon": False,
        "legend.fontsize": 9.5, "lines.linewidth": 2, "savefig.dpi": 150,
    })


def save(fig, out, name):
    import matplotlib.pyplot as plt
    from PIL import Image
    path = os.path.join(out, name + ".png")
    fig.savefig(path, bbox_inches="tight", pad_inches=0.15)
    plt.close(fig)
    Image.open(path).convert("RGB").quantize(256, method=Image.Quantize.MEDIANCUT,
                                             dither=Image.Dither.NONE).save(path, optimize=True)
    print(f"  {path} ({os.path.getsize(path) / 1024:.0f} KB)")


def map_axes(ax, title):
    ax.set_xticks([])
    ax.set_yticks([])
    ax.grid(False)
    for s in ax.spines.values():
        s.set_visible(False)
    ax.set_title(title)


def figures(out):
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    from matplotlib.colors import LinearSegmentedColormap, ListedColormap
    import zeit
    from zeit import _core

    style()
    os.makedirs(out, exist_ok=True)
    trend_cm = LinearSegmentedColormap.from_list("t", ["#B5651D", "#3A2A1E", "#15161A", "#163B3A", "#2FA39A"])
    sig_cm = ListedColormap(["#C9742A", "#22252B", "#2FA39A"])

    # 1. a map without a trend: its pixels' trends, those significant, and the two tests
    cube = synthetic(7, n=100)
    ar = zeit.ar_trend(cube)
    s, p = ar.slope.values, ar.slope_p.values
    res = zeit.parts(cube, trend=ar, partsize=1000)
    up, down = np.mean((p < .05) & (s > 0)), np.mean((p < .05) & (s < 0))
    pn, pp = naive_p(s), float(res.table.loc["mean", "p"])
    print(f"problem: {100 * up:.1f}% up, {100 * down:.1f}% down, naive p {pn:.2g}, PARTS p {pp:.3f}")
    fig = plt.figure(figsize=(13.5, 4.9))
    gs = fig.add_gridspec(1, 3, width_ratios=[1, 1, 1.25], wspace=0.12)
    axes = [fig.add_subplot(gs[0, k]) for k in range(3)]
    lim = np.nanpercentile(np.abs(s), 98)
    im = axes[0].imshow(s, cmap=trend_cm, vmin=-lim, vmax=lim)
    map_axes(axes[0], "Each pixel's AR(1) trend (the true trend is 0)")
    tick = float(f"{0.6 * lim:.1g}")
    cb = fig.colorbar(im, ax=axes[0], orientation="horizontal", fraction=0.046, pad=0.03, ticks=[-tick, 0, tick])
    cb.outline.set_visible(False)
    cb.ax.tick_params(colors=MUTE, length=0)
    axes[1].imshow(np.where(p < .05, np.sign(s), 0) + 1, cmap=sig_cm, vmin=0, vmax=2, interpolation="nearest")
    map_axes(axes[1], f"p < 0.05: {100 * (up + down):.1f}% of the pixels")
    from matplotlib.patches import Patch
    axes[1].legend(handles=[Patch(color=sig_cm.colors[2], label=f"increasing, {100 * up:.1f}%"),
                            Patch(color=sig_cm.colors[0], label=f"decreasing, {100 * down:.1f}%")],
                   loc="upper left", bbox_to_anchor=(0, -0.02))
    ax = axes[2]
    ax.set_xlim(0, 1)
    ax.set_ylim(0, 1)
    ax.axis("off")
    ax.set_title("Does the map's mean trend differ from 0?")
    mant, expo = f"{pn:.1e}".split("e")
    for yv, colr, name, ptxt, verdict in [
            (0.66, ORANGE, "t-test of the 10,000 slopes,\nas if the pixels were independent",
             f"p = {mant} × 10$^{{{int(expo)}}}$", "\"yes\": wrong"),
            (0.2, BLUE, "PARTS (the trend test)", f"p = {pp:.2f}", "\"no evidence\": right")]:
        ax.add_patch(plt.Rectangle((0, yv - 0.15), 1, 0.36, color=PANEL, lw=0))
        ax.add_patch(plt.Rectangle((0, yv - 0.15), 0.012, 0.36, color=colr, lw=0))
        ax.text(0.05, yv + 0.1, name, fontsize=10, color=TEXT, va="center", linespacing=1.3)
        ax.text(0.05, yv - 0.07, ptxt, fontsize=15, color=colr, va="center", weight="bold")
        ax.text(0.97, yv - 0.07, verdict, fontsize=10, color=MUTE, va="center", ha="right")
    fig.subplots_adjust(left=0.02, right=0.98, top=0.9, bottom=0.2)
    save(fig, out, "01-problem")

    # 2. the correlation of the residual series against distance, and the fitted function
    xs, ys = np.meshgrid(cube.x.values, cube.y.values)
    idx = np.random.default_rng(1).choice(cube.sizes["y"] * cube.sizes["x"], 800, replace=False)
    nt = cube.sizes["time"]
    design = np.column_stack([np.ones(nt), np.arange(1, nt + 1, dtype=float)])
    r = _core.parts.ar_residuals(np.ascontiguousarray(cube.values.reshape(nt, -1)[:, idx].T), design)
    c = np.corrcoef(r)
    x, y = xs.ravel()[idx], ys.ravel()[idx]
    iu = np.triu_indices(len(idx), 1)
    dd, cc = np.hypot(x[:, None] - x[None, :], y[:, None] - y[None, :])[iu], c[iu]
    fitted, true = res.correlation["range"], 30.0 * RANGE_PX
    print(f"correlation: fitted range {fitted:.0f} m (true {true:.0f}), nugget {np.nanmean(res.nuggets):.3f}")
    fig, ax = plt.subplots(figsize=(8, 4.4))
    ax.hexbin(dd, cc, gridsize=60, cmap=LinearSegmentedColormap.from_list("h", [BG, "#2B3443", "#5E6878"]),
              mincnt=1, bins="log", linewidths=0)
    edges = np.linspace(0, dd.max(), 30)
    mid = [(dd[(dd >= a) & (dd < b)].mean(), cc[(dd >= a) & (dd < b)].mean()) for a, b in zip(edges[:-1], edges[1:])
           if ((dd >= a) & (dd < b)).sum() > 20]
    ax.plot(*np.array(mid).T, "o", color=PALE, ms=4.5, label="mean of the pairs at that distance")
    xl = np.linspace(0, dd.max(), 300)
    ax.plot(xl, np.exp(-xl / fitted), color=BLUE, lw=2.4, label=f"fitted exp(−d / {fitted:.0f} m)")
    ax.plot(xl, 0.8 * np.exp(-xl / true), color=ORANGE, lw=1.6, ls=(0, (4, 3)),
            label=f"true correlation, 0.8 exp(−d / {true:.0f} m)")
    ax.axhline(0, color=RULE, lw=1)
    ax.set_ylim(-0.6, 1.05)
    ax.set_xlabel("distance between two pixels (m)")
    ax.set_ylabel("correlation of their residual series")
    ax.set_title("Neighbours share their ups and downs")
    ax.legend(loc="upper right")
    save(fig, out, "05-correlation")

    # 3. random partitions
    n = 60
    pm = np.asarray(_core.parts.sample_partitions(n * n, 600, 6, 3))
    yy, xx = np.divmod(np.arange(n * n), n)
    fig, ax = plt.subplots(figsize=(5.2, 5.4))
    ax.scatter(xx, yy, s=3, color="#2B3443", linewidths=0)
    for k, colr in zip(range(3), [BLUE, ORANGE, GREEN]):
        ax.scatter(xx[pm[k]], yy[pm[k]], s=7, color=colr, linewidths=0, label=f"partition {k + 1}")
    ax.set_aspect("equal")
    ax.invert_yaxis()
    map_axes(ax, "3 of 6 random partitions of 600 pixels")
    ax.legend(loc="lower left", bbox_to_anchor=(0, -0.1), ncol=3, handletextpad=0.2, markerscale=2)
    save(fig, out, "06-partitions")

    # 4. calibration: maps without a trend, and maps with one
    t0 = time.time()
    rows = []
    for s_ in range(300):
        cube = synthetic(10_000 + s_)
        ar = zeit.ar_trend(cube)
        res = zeit.parts(cube, trend=ar, partsize=600)
        rows.append((np.mean(ar.slope_p.values < .05), naive_p(ar.slope.values), float(res.table.loc["mean", "p"])))
    a = np.array(rows)
    slopes = [0.0001, 0.0002, 0.0003, 0.0005, 0.0008]
    power = {"naive": [np.mean(a[:, 1] < .05)], "parts": [np.mean(a[:, 2] < .05)]}
    for b in slopes:
        hn, hp = [], []
        for s_ in range(100):
            cube = synthetic(20_000 + s_, slope=b)
            ar = zeit.ar_trend(cube)
            res = zeit.parts(cube, trend=ar, partsize=600)
            hn.append(naive_p(ar.slope.values) < .05)
            hp.append(float(res.table.loc["mean", "p"]) < .05)
        power["naive"].append(np.mean(hn))
        power["parts"].append(np.mean(hp))
    print(f"calibration ({time.time() - t0:.0f} s): false alarms naive {np.mean(a[:, 1] < .05):.3f}, "
          f"PARTS {np.mean(a[:, 2] < .05):.3f}; significant pixels {100 * a[:, 0].mean():.1f}% on average, "
          f"95% of maps below {100 * np.quantile(a[:, 0], .95):.1f}%, at most {100 * a[:, 0].max():.1f}%; power {power}")
    fig, axes = plt.subplots(1, 3, figsize=(14.5, 4.3))
    ax = axes[0]
    bins = np.linspace(0, 1, 21)
    ax.hist(a[:, 1], bins=bins, color=ORANGE, alpha=0.85, label="t-test of the pixels' slopes")
    ax.hist(a[:, 2], bins=bins, color=BLUE, alpha=0.85, label="PARTS")
    ax.axhline(len(a) / 20, color=MUTE, lw=1, ls=(0, (4, 3)))
    ax.set_xlabel("p-value of \"the mean trend is 0\"")
    ax.set_ylabel(f"maps (of {len(a)})")
    ax.set_title(f"No trend: p < 0.05 in {100 * np.mean(a[:, 1] < .05):.0f}% vs {100 * np.mean(a[:, 2] < .05):.0f}% of maps")
    ax.legend(loc="upper center")
    ax = axes[1]
    ax.hist(100 * a[:, 0], bins=30, color="#5E6878")
    ax.axvline(5, color=PALE, lw=1.2, ls=(0, (4, 3)))
    ax.text(5.3, ax.get_ylim()[1] * 0.92, "5% expected", color=MUTE, fontsize=9)
    ax.set_xlabel("pixels with a significant trend (%)")
    ax.set_ylabel("maps")
    ax.set_title("No trend: the share of significant pixels")
    ax = axes[2]
    xv = [0.0] + slopes
    xv = [1e4 * v for v in xv]
    ax.plot(xv, power["naive"], "o-", color=ORANGE, label="t-test of the pixels' slopes")
    ax.plot(xv, power["parts"], "o-", color=BLUE, label="PARTS")
    ax.axhline(0.05, color=MUTE, lw=1, ls=(0, (4, 3)))
    ax.set_ylim(-0.03, 1.05)
    ax.set_xlabel("true trend (10⁻⁴ per year; noise SD 0.03)")
    ax.set_ylabel("share of maps where p < 0.05")
    ax.set_title("A real trend: how often it is found")
    ax.legend(loc="lower right")
    fig.tight_layout()
    save(fig, out, "07-calibration")


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "data":
        data(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 3 and sys.argv[1] == "figures":
        figures(sys.argv[2])
    else:
        sys.exit(__doc__)
