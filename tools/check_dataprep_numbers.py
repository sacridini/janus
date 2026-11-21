#!/usr/bin/env python3
"""The numbers of site/data-preparation.html's figures that need the runs' files: what the harmonization changed and
what the topographic correction did at the two pixels of its chart. The tour (`janus --shots dataprep`) prints each run's
folder ("runs: ...").

    python tools/check_dataprep_numbers.py harmonize DATA RUN_FOLDER
    python tools/check_dataprep_numbers.py topo DATA RUN_FOLDER
    python tools/check_dataprep_numbers.py cloudy DATA [x,y ...]

harmonize: for every Sentinel-2 date with a Landsat date at most 3 days from it, the difference of the mean over the
           pixels clear in both (by each one's quality band), Sentinel-2 minus Landsat, as it was and as Harmonize wrote
           it, for each band; and the same at the stable pixel (5 x 5 pixels).
topo:      NIR of the sunlit and the shaded pixel of the chart, as observed and corrected, each date, and the mean absolute
           difference between the two pixels before and after.
cloudy:    on DATA/cloudy (Landsat 8 and 9 of 2022 to 2024, the cloudy dates too; the quality band masks as Janus does: bits
           0, 1, 3, 4 and 5 of QA_PIXEL): the valid observations of the dates and of the months, Tmask's flags (Zeit's own,
           as the Tmask tool runs it), the candidate pixels of the figures (a forest, a seasonal pixel, one Tmask flags
           several dates of, one with long wet-season gaps) and, for the pixels given, their series.
Needs the Python of the geo environment (GDAL, numpy); `cloudy` the zeit environment (rasterio, Zeit).
"""

import datetime as dt
import re
import sys
from pathlib import Path

import numpy as np

try:
    from osgeo import gdal

    gdal.UseExceptions()
except ImportError:  # the zeit environment (the `cloudy` command) has rasterio and Zeit instead
    gdal = None
STABLE = (782, 492)
SUNLIT, SHADED = (165, 366), (165, 370)
BANDS = ["blue", "green", "red", "nir", "swir1", "swir2"]


def refl(ds, b):
    rb = ds.GetRasterBand(b)
    a = rb.ReadAsArray().astype(np.float32)
    sc, off = rb.GetScale(), rb.GetOffset()
    if sc is not None:
        a = np.where(a == 0, np.nan, a * sc + (off or 0.0))
    return a


def read(path, qa=True):
    ds = gdal.Open(str(path))
    bands = np.array([refl(ds, b) for b in range(1, 7)])
    if qa and ds.RasterCount >= 7:
        q = ds.GetRasterBand(7).ReadAsArray().astype(np.uint32)
        bands[:, (q & 0b111111) != 0] = np.nan
    return bands


def day(p):
    m = re.search(r"(\d{4})-?(\d{2})-?(\d{2})", p.stem)
    return dt.date(int(m[1]), int(m[2]), int(m[3]))


def harmonize(data, run):
    s2 = {day(p): p for p in sorted((data / "mixed" / "s2").glob("*.tif"))}
    ls = {day(p): p for p in sorted((data / "mixed" / "landsat").glob("*.tif"))}
    out = {day(p): p for p in sorted(Path(run).glob("*.tif"))}
    rows = []
    for d, p in s2.items():
        near = [(abs((d - e).days), e) for e in ls if abs((d - e).days) <= 3]
        if not near or d not in out or d in ls:
            continue
        gap, e = min(near)
        a, c = read(p), read(ls[e])
        h = read(out[d], qa=False)
        ok = np.isfinite(a).all(axis=0) & np.isfinite(c).all(axis=0) & np.isfinite(h).all(axis=0)
        x, y = STABLE
        pin = (slice(None), slice(y - 2, y + 3), slice(x - 2, x + 3))
        rows.append((
            [np.mean((a - c)[:, ok], axis=1), np.mean((h - c)[:, ok], axis=1)],
            [np.nanmean((a - c)[pin], axis=(1, 2)), np.nanmean((h - c)[pin], axis=(1, 2))],
            [np.mean(c[:, ok], axis=1), np.nanmean(c[pin], axis=(1, 2))],
        ))
    print(len(rows), "pairs of a Sentinel-2 and a Landsat date at most 3 days apart")
    for name, k in (("all clear pixels", 0), ("the stable pixel", 1)):
        before = np.mean([r[k][0] for r in rows], axis=0)
        after = np.mean([r[k][1] for r in rows], axis=0)
        mean_l = np.mean([r[2][k] for r in rows], axis=0)
        print(name)
        for b, n in enumerate(BANDS):
            print(f"  {n:6s} Landsat mean {mean_l[b]:.4f}  S2 - Landsat before {before[b]:+.4f} ({100 * before[b] / mean_l[b]:+.1f} %)  after {after[b]:+.4f} ({100 * after[b] / mean_l[b]:+.1f} %)")


def topo(data, run):
    before = {day(p): p for p in sorted((data / "topo" / "series").glob("*.tif"))}
    after = {day(p): p for p in sorted((Path(run) / "series").glob("*.tif"))}
    gaps_b, gaps_a = [], []
    print("date        zenith  sunlit NIR before/after   shaded NIR before/after   ratio before -> after")
    for d, p in before.items():
        ds = gdal.Open(str(p))
        zen = float(ds.GetMetadataItem("STAC_SUN_ZENITH"))
        b = read(p, qa=False)[3]
        a = read(after[d], qa=False)[3] if d in after else None
        if a is None:
            continue

        def at(m, xy):
            return float(np.nanmean(m[xy[1] - 1 : xy[1] + 2, xy[0] - 1 : xy[0] + 2]))

        sb, hb, sa, ha = at(b, SUNLIT), at(b, SHADED), at(a, SUNLIT), at(a, SHADED)
        gaps_b.append(abs(sb - hb))
        gaps_a.append(abs(sa - ha))
        print(f"{d}  {zen:5.1f}   {sb:.3f} / {sa:.3f}            {hb:.3f} / {ha:.3f}            {sb / hb:.2f} -> {sa / ha:.2f}")
    print(f"mean absolute difference between the two pixels: {np.mean(gaps_b):.3f} before, {np.mean(gaps_a):.3f} after")


def cloudy_cube(data):
    """The dates, the reflectance of the six bands (NaN where the quality band or the no-data masks) and NDVI of
    DATA/cloudy as arrays (time, band, y, x) and (time, y, x)."""
    import rasterio

    files = sorted((Path(data) / "cloudy").glob("*.tif"))
    days = [day(f) for f in files]
    order = np.argsort([d.toordinal() for d in days])
    refl, qas = [], []
    for k in order:
        with rasterio.open(files[k]) as ds:
            dn = ds.read().astype(np.float32)
        qa = dn[6].astype(np.uint32)
        ok = (qa & 0x3B) == 0
        r = dn[:6] * 2.75e-5 - 0.2
        r[:, ~ok] = np.nan
        r[dn[:6] == 0] = np.nan
        refl.append(r)
        qas.append(qa)
    days = [days[k] for k in order]
    refl = np.array(refl)
    ndvi = (refl[:, 3] - refl[:, 2]) / (refl[:, 3] + refl[:, 2])
    return days, refl, ndvi, np.array(qas)


def gaps(days, valid, lo, hi):
    """The longest stretch (days) between valid dates inside [lo, hi] (dates), counting from lo and to hi."""
    ts = [d.toordinal() for d, v in zip(days, valid) if v and lo <= d <= hi]
    edges = [lo.toordinal()] + ts + [hi.toordinal()]
    return max(b - a for a, b in zip(edges, edges[1:]))


def cloudy(data, pins):
    import xarray as xr
    import zeit

    days, refl, ndvi, qa = cloudy_cube(data)
    n = len(days)
    h, w = ndvi.shape[1:]
    print(n, "dates", days[0], "to", days[-1], f"{w} x {h} px")
    valid = np.isfinite(ndvi)
    share = valid.mean(axis=(1, 2))
    print("share of the window valid by date: median %.2f, min %.2f, max %.2f; dates under 20 %%: %d" % (np.median(share), share.min(), share.max(), (share < 0.2).sum()))
    wet = np.array([d.month in (12, 1, 2, 3) for d in days])
    dry = np.array([d.month in (6, 7, 8, 9) for d in days])
    print("valid share by date: wet season (Dec-Mar) %.2f, dry season (Jun-Sep) %.2f" % (share[wet].mean(), share[dry].mean()))
    months = sorted({(d.year, d.month) for d in days})
    print("clear observations of the window's median pixel, by month:")
    for ym in months:
        sel = [i for i, d in enumerate(days) if (d.year, d.month) == ym]
        c = valid[sel].sum(axis=0)
        print(f"  {ym[0]}-{ym[1]:02d}: dates {len(sel)}, count median {np.median(c):.0f} (min {c.min()}, max {c.max()})")
    # Tmask
    cube = xr.DataArray(
        refl[:, [1, 4]], dims=("time", "band", "y", "x"),
        coords={"time": np.array([np.datetime64(d.isoformat()) for d in days]), "band": ["green", "swir1"]},
    )
    clear = zeit.tmask(cube, green="green", swir="swir1", scale=1.0).values
    both = np.isfinite(refl[:, 1]) & np.isfinite(refl[:, 4])
    flagged = both & ~clear
    nf = flagged.sum(axis=0)
    nv = both.sum(axis=0)
    print(f"Tmask: flagged observations per pixel median {np.median(nf):.0f}, mean {nf.mean():.2f}, max {nf.max()}; share {100 * flagged.sum() / both.sum():.2f} % of all valid observations")
    sh = 100.0 * nf / np.maximum(nv, 1)
    print("share flagged: median %.1f %%, 5th %.1f, 95th %.1f; left half %.2f, right half %.2f, top %.2f, bottom %.2f" % (
        np.median(sh), np.percentile(sh, 5), np.percentile(sh, 95), sh[:, : w // 2].mean(), sh[:, w // 2 :].mean(), sh[: h // 2].mean(), sh[h // 2 :].mean()))
    per_date = flagged.sum(axis=(1, 2)) / np.maximum(both.sum(axis=(1, 2)), 1)
    print("Tmask: the dates with the largest share of their valid pixels flagged (and over a third of the window valid):")
    for i in np.argsort(-per_date):
        if both[i].mean() > 0.33:
            print(f"  {days[i]}: {100 * per_date[i]:.1f} % of {100 * both[i].mean():.0f} % valid flagged")
            if per_date[i] < 0.5 * per_date.max():
                break
    # candidates
    med = np.nanmedian(ndvi, axis=0)
    for lo_, hi_, name in ((-1, 0.5, "median NDVI under 0.5"), (0.5, 0.7, "0.5 to 0.7"), (0.7, 0.8, "0.7 to 0.8"), (0.8, 2, "over 0.8")):
        m = (med >= lo_) & (med < hi_)
        print(f"share flagged by cover ({name}): mean {sh[m].mean():.1f} % over {m.sum()} pixels ({100 * m.mean():.0f} % of the window)")
    hazy = days.index(dt.date(2024, 8, 24)) if dt.date(2024, 8, 24) in days else None
    if hazy is not None:
        dry_other = [i for i, d in enumerate(days) if d.month in (7, 8, 9) and i != hazy and np.isfinite(refl[i, 1]).mean() > 0.5]
        g = lambda i: np.nanmedian(refl[i, 1])
        print(f"2024-08-24: valid {100 * np.isfinite(ndvi[hazy]).mean():.0f} %, flagged {100 * per_date[hazy]:.0f} % of them; median green {g(hazy):.3f} against {np.median([g(i) for i in dry_other]):.3f} on the {len(dry_other)} other clear dates of July to September; median NDVI {np.nanmedian(ndvi[hazy]):.2f} against {np.median([np.nanmedian(ndvi[i]) for i in dry_other]):.2f}")
    p10, p90 = np.nanpercentile(ndvi, 10, axis=0), np.nanpercentile(ndvi, 90, axis=0)
    nvalid = valid.sum(axis=0)
    from scipy import ndimage

    def uni(a, size=3):
        a0 = np.nan_to_num(a)
        m = ndimage.uniform_filter(a0, size)
        return np.sqrt(np.maximum(ndimage.uniform_filter(a0**2, size) - m**2, 0))

    ok_n = nvalid >= 0.45 * n
    forest = (med > 0.8) & ok_n & (uni(med, 3) < 0.03)
    ys, xs = np.where(forest)
    print("forest candidates", len(xs))
    for j in np.argsort(np.nanstd(ndvi[:, ys, xs], axis=0))[:5]:
        x, y = xs[j], ys[j]
        print(f"  forest ({x}, {y}): median {med[y, x]:.2f}, p10 {p10[y, x]:.2f}, valid {nvalid[y, x]}, std {np.nanstd(ndvi[:, y, x]):.3f}, Tmask flagged {nf[y, x]}")
    seas = (med > 0.45) & (med < 0.72) & (p90 - p10 > 0.2) & ok_n & (uni(med, 3) < 0.05)
    ys, xs = np.where(seas)
    print("seasonal candidates", len(xs))
    for lbl, ordr in (("flags between 3 and 8", np.argsort(np.abs(nf[ys, xs] - 5))), ):
        for j in ordr[:8]:
            x, y = xs[j], ys[j]
            print(f"  seasonal ({x}, {y}): median {med[y, x]:.2f}, p10 {p10[y, x]:.2f}, p90 {p90[y, x]:.2f}, valid {nvalid[y, x]}, flagged {nf[y, x]}")
    past = (med > 0.5) & (med < 0.62) & (p90 - p10 > 0.2) & (p90 - p10 < 0.45) & (nf <= 8) & ok_n & (uni(med, 3) < 0.04)
    ys2, xs2 = np.where(past)
    print("pasture near 0.5 candidates", len(xs2))
    for j in np.argsort(-(p90 - p10)[ys2, xs2])[:10]:
        x, y = xs2[j], ys2[j]
        print(f"  pasture ({x}, {y}): median {med[y, x]:.2f}, p10 {p10[y, x]:.2f}, p90 {p90[y, x]:.2f}, valid {nvalid[y, x]}, flagged {nf[y, x]}")
    # wet-season gaps of the valid series
    cand = []
    for x, y in zip(xs[::7], ys[::7]):
        g = []
        for yr in (2023, 2024):
            g.append(gaps(days, valid[:, y, x], dt.date(yr - 1, 12, 1), dt.date(yr, 3, 31)))
        cand.append((min(g), x, y))
    cand.sort(reverse=True)
    print("long wet-season gaps (the shorter of the two wet seasons Dec-Mar, days):", cand[:6])
    for xy in pins:
        x, y = (int(v) for v in xy.split(","))
        print(f"pixel ({x}, {y}): median NDVI {med[y, x]:.2f}, valid {nvalid[y, x]} of {n}, flagged {nf[y, x]}")
        for i, d in enumerate(days):
            mark = "FLAG" if flagged[i, y, x] else ""
            print(f"   {d} NDVI {ndvi[i, y, x]:.3f} green {refl[i, 1, y, x]:.3f} swir1 {refl[i, 4, y, x]:.3f} cirrus {(qa[i, y, x] >> 2) & 1} {mark}")


def main():
    a = sys.argv[1:]
    if a and a[0] == "cloudy" and len(a) >= 2:
        cloudy(Path(a[1]), a[2:])
        return
    if len(a) != 3 or a[0] not in ("harmonize", "topo"):
        raise SystemExit(__doc__)
    {"harmonize": harmonize, "topo": topo}[a[0]](Path(a[1]), a[2])


if __name__ == "__main__":
    main()
