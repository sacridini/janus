"""Janus 1.16.0 (2026-11-07): pin 1 of series,
reproduced outside Janus. Needs: pip install numpy rasterio xarray zeit-cdts
"""
import numpy as np
import rasterio
from rasterio.windows import Window
from rasterio.enums import Resampling

# 41 files (band 1 of each)
# Each date: (file, band, band B of a normalized difference (A - B) / (A + B), quality band).
layers = [
    ("series/landsat_medoid_1985.vrt", 1, None, None),
    ("series/landsat_medoid_1986.vrt", 1, None, None),
    ("series/landsat_medoid_1987.vrt", 1, None, None),
    ("series/landsat_medoid_1988.vrt", 1, None, None),
    ("series/landsat_medoid_1989.vrt", 1, None, None),
    ("series/landsat_medoid_1990.vrt", 1, None, None),
    ("series/landsat_medoid_1991.vrt", 1, None, None),
    ("series/landsat_medoid_1992.vrt", 1, None, None),
    ("series/landsat_medoid_1993.vrt", 1, None, None),
    ("series/landsat_medoid_1994.vrt", 1, None, None),
    ("series/landsat_medoid_1995.vrt", 1, None, None),
    ("series/landsat_medoid_1996.vrt", 1, None, None),
    ("series/landsat_medoid_1997.vrt", 1, None, None),
    ("series/landsat_medoid_1998.vrt", 1, None, None),
    ("series/landsat_medoid_1999.vrt", 1, None, None),
    ("series/landsat_medoid_2000.vrt", 1, None, None),
    ("series/landsat_medoid_2001.vrt", 1, None, None),
    ("series/landsat_medoid_2002.vrt", 1, None, None),
    ("series/landsat_medoid_2003.vrt", 1, None, None),
    ("series/landsat_medoid_2004.vrt", 1, None, None),
    ("series/landsat_medoid_2005.vrt", 1, None, None),
    ("series/landsat_medoid_2006.vrt", 1, None, None),
    ("series/landsat_medoid_2007.vrt", 1, None, None),
    ("series/landsat_medoid_2008.vrt", 1, None, None),
    ("series/landsat_medoid_2009.vrt", 1, None, None),
    ("series/landsat_medoid_2010.vrt", 1, None, None),
    ("series/landsat_medoid_2011.vrt", 1, None, None),
    ("series/landsat_medoid_2012.vrt", 1, None, None),
    ("series/landsat_medoid_2013.vrt", 1, None, None),
    ("series/landsat_medoid_2014.vrt", 1, None, None),
    ("series/landsat_medoid_2015.vrt", 1, None, None),
    ("series/landsat_medoid_2016.vrt", 1, None, None),
    ("series/landsat_medoid_2017.vrt", 1, None, None),
    ("series/landsat_medoid_2018.vrt", 1, None, None),
    ("series/landsat_medoid_2019.vrt", 1, None, None),
    ("series/landsat_medoid_2020.vrt", 1, None, None),
    ("series/landsat_medoid_2021.vrt", 1, None, None),
    ("series/landsat_medoid_2022.vrt", 1, None, None),
    ("series/landsat_medoid_2023.vrt", 1, None, None),
    ("series/landsat_medoid_2024.vrt", 1, None, None),
    ("series/landsat_medoid_2025.vrt", 1, None, None),
]
dates = np.array(["1985", "1986", "1987", "1988", "1989", "1990", "1991", "1992", "1993", "1994", "1995", "1996", "1997", "1998", "1999", "2000", "2001", "2002", "2003", "2004", "2005", "2006", "2007", "2008", "2009", "2010", "2011", "2012", "2013", "2014", "2015", "2016", "2017", "2018", "2019", "2020", "2021", "2022", "2023", "2024", "2025"], dtype="datetime64[D]")
# Time in years since the first date, as Janus fits trends (365.2425-day years).
years = np.array([0, 0.9993360576, 1.998672115, 2.998008173, 4.000082137, 4.999418195, 5.998754252, 6.99809031, 8.000164274, 8.999500332, 9.99883639, 10.99817245, 12.00024641, 12.99958247, 13.99891853, 14.99825458, 16.00032855, 16.99966461, 17.99900066, 18.99833672, 20.00041069, 20.99974674, 21.9990828, 22.99841886, 24.00049282, 24.99982888, 25.99916494, 26.998501, 28.00057496, 28.99991102, 29.99924708, 30.99858313, 32.0006571, 32.99999316, 33.99932921, 34.99866527, 36.00073923, 37.00007529, 37.99941135, 38.99874741, 40.00082137])
qa_rule = "none"

def usable(q, rule):
    """Observations Janus keeps, from the quality band's codes (nodata: none)."""
    ok = np.isfinite(q) & (q >= 0)
    c = np.where(ok, q, 0).astype(np.int64)
    if rule == "fmask":          # 0 clear land, 1 water
        return ok & ((c == 0) | (c == 1))
    if rule == "landsat_c2":     # QA_PIXEL bits 0 fill, 1 dilated cloud, 3 cloud, 4 shadow, 5 snow
        return ok & ((c & 0x3B) == 0)
    if rule == "scl":            # Sentinel-2 SCL: 4 vegetation, 5 bare soil, 6 water
        return ok & (c >= 4) & (c <= 6)
    if rule == "hls_fmask":      # HLS Fmask bits 1 cloud, 2 adjacent, 3 shadow, 4 snow; 255 fill
        return ok & (c != 255) & ((c & 0x1E) == 0)
    return ok & (c != 0)         # a 0/1 mask


def read(path, band, window, out_shape=None, raw=False):
    """A band over a window: nodata -> NaN, the band's scale and offset applied."""
    with rasterio.open(path) as src:
        kw = {"out_shape": out_shape, "resampling": Resampling.nearest} if out_shape else {}
        a = src.read(band, window=window, masked=True, **kw).astype("float64")
        if not raw:
            a = a * src.scales[band - 1] + src.offsets[band - 1]
        return np.ma.filled(a, np.nan)


def shown(layer, window, out_shape=None):
    """What the series shows at a date: the band, or the normalized difference, masked by the quality band."""
    path, band, nd_band, qa_band = layer
    v = read(path, band, window, out_shape)
    if nd_band:
        b = read(path, nd_band, window, out_shape)
        with np.errstate(invalid="ignore", divide="ignore"):
            v = np.where(v + b != 0, (v - b) / (v + b), np.nan)
    if qa_band:
        v = np.where(usable(read(path, qa_band, window, out_shape, raw=True), qa_rule), v, np.nan)
    return v

# The pixel (column, row of the files), its centre at x -38.03255809, y -9.460742162 (EPSG:4326).
col, row = 1426, 1199
values = np.array([shown(layer, Window(col, row, 1, 1))[0, 0] for layer in layers])

ok = np.isfinite(values)
y, t = values[ok], years[ok]
print("n", ok.sum(), "mean", y.mean(), "std", y.std(ddof=1) if y.size > 1 else np.nan)
if y.size >= 2:
    slope, intercept = np.polyfit(t, y, 1)                  # OLS, per year
    i, j = np.triu_indices(y.size, 1)
    keep = t[j] != t[i]
    sen = np.median((y[j] - y[i])[keep] / (t[j] - t[i])[keep])  # Sen's slope, per year
    print("OLS slope", slope, "Sen's slope", sen)

# AR(1) trend (remotePARTS), fitted on the chart with these parameters: the same Zeit calls Janus makes
# (its model is drawn from their results).
import xarray as xr
import zeit

series = xr.DataArray(values[:, None, None], dims=("time", "y", "x"), coords={"time": dates.astype("datetime64[ns]")})
result1 = zeit.ar_trend(series, time='year', harmonics=0, gaps='exact', min_valid=4, nodata=None, n_jobs=1)
print(result1)
