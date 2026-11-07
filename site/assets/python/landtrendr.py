"""Janus 1.16.0 (2026-11-07): pin 1 of LT_Stack_NDVI_Rondonia.tif,
reproduced outside Janus. Needs: pip install numpy rasterio xarray zeit-cdts
"""
import numpy as np
import rasterio
from rasterio.windows import Window
from rasterio.enums import Resampling

# 1 file, 40 bands (1 band = 1 date), every band in each block (pixel-interleaved): read block by block
# Each date: (file, band, band B of a normalized difference (A - B) / (A + B), quality band).
layers = [
    ("LT_Stack_NDVI_Rondonia.tif", 1, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 2, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 3, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 4, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 5, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 6, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 7, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 8, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 9, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 10, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 11, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 12, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 13, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 14, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 15, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 16, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 17, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 18, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 19, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 20, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 21, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 22, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 23, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 24, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 25, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 26, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 27, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 28, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 29, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 30, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 31, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 32, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 33, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 34, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 35, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 36, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 37, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 38, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 39, None, None),
    ("LT_Stack_NDVI_Rondonia.tif", 40, None, None),
]
dates = np.array(["1985", "1986", "1987", "1988", "1989", "1990", "1991", "1992", "1993", "1994", "1995", "1996", "1997", "1998", "1999", "2000", "2001", "2002", "2003", "2004", "2005", "2006", "2007", "2008", "2009", "2010", "2011", "2012", "2013", "2014", "2015", "2016", "2017", "2018", "2019", "2020", "2021", "2022", "2023", "2024"], dtype="datetime64[D]")
# Time in years since the first date, as Janus fits trends (365.2425-day years).
years = np.array([0, 0.9993360576, 1.998672115, 2.998008173, 4.000082137, 4.999418195, 5.998754252, 6.99809031, 8.000164274, 8.999500332, 9.99883639, 10.99817245, 12.00024641, 12.99958247, 13.99891853, 14.99825458, 16.00032855, 16.99966461, 17.99900066, 18.99833672, 20.00041069, 20.99974674, 21.9990828, 22.99841886, 24.00049282, 24.99982888, 25.99916494, 26.998501, 28.00057496, 28.99991102, 29.99924708, 30.99858313, 32.0006571, 32.99999316, 33.99932921, 34.99866527, 36.00073923, 37.00007529, 37.99941135, 38.99874741])
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

# The pixel (column, row of the files), its centre at x -63.08396625, y -10.35600317 (EPSG:4326).
col, row = 1272, 1414
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

# LandTrendr, fitted on the chart with these parameters: the same Zeit calls Janus makes
# (its model is drawn from their results).
import xarray as xr
import zeit

series = xr.DataArray(values[:, None, None], dims=("time", "y", "x"), coords={"time": dates.astype("datetime64[ns]")})
result1 = zeit.landtrendr(series, nodata=None, n_jobs=1, direction='loss', max_segments=6, pval_threshold=0.05, recovery_threshold=0.25, spike_threshold=0.9, best_model_proportion=0.75, vertex_count_overshoot=3, min_observations_needed=6)
result2 = zeit.extract_events(result1, event_type='loss', sort_by='greatest', min_magnitude=0.0, min_duration=1, pre_val_threshold=0.0)
print(result1)
