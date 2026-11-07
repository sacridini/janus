"""Janus 1.16.0 (2026-11-07): pin 1 of landsat,
reproduced outside Janus. Needs: pip install numpy rasterio xarray zeit-cdts
"""
import numpy as np
import rasterio
from rasterio.windows import Window
from rasterio.enums import Resampling

# 125 files (ND(NIR, Red), QA QA_PIXEL of each): ND(NIR, Red), QA QA_PIXEL
# Each date: (file, band, band B of a normalized difference (A - B) / (A + B), quality band).
layers = [
    ("landsat/ls_2020-02-04.tif", 4, 3, 7),
    ("landsat/ls_2020-05-26.tif", 4, 3, 7),
    ("landsat/ls_2020-06-20.tif", 4, 3, 7),
    ("landsat/ls_2020-07-06.tif", 4, 3, 7),
    ("landsat/ls_2020-07-13.tif", 4, 3, 7),
    ("landsat/ls_2020-07-22.tif", 4, 3, 7),
    ("landsat/ls_2020-07-29.tif", 4, 3, 7),
    ("landsat/ls_2020-08-07.tif", 4, 3, 7),
    ("landsat/ls_2020-08-23.tif", 4, 3, 7),
    ("landsat/ls_2020-08-30.tif", 4, 3, 7),
    ("landsat/ls_2020-09-08.tif", 4, 3, 7),
    ("landsat/ls_2020-09-24.tif", 4, 3, 7),
    ("landsat/ls_2020-10-10.tif", 4, 3, 7),
    ("landsat/ls_2020-10-26.tif", 4, 3, 7),
    ("landsat/ls_2021-06-14.tif", 4, 3, 7),
    ("landsat/ls_2021-06-30.tif", 4, 3, 7),
    ("landsat/ls_2021-07-09.tif", 4, 3, 7),
    ("landsat/ls_2021-07-16.tif", 4, 3, 7),
    ("landsat/ls_2021-07-25.tif", 4, 3, 7),
    ("landsat/ls_2021-08-01.tif", 4, 3, 7),
    ("landsat/ls_2021-08-10.tif", 4, 3, 7),
    ("landsat/ls_2021-08-26.tif", 4, 3, 7),
    ("landsat/ls_2021-09-11.tif", 4, 3, 7),
    ("landsat/ls_2021-10-04.tif", 4, 3, 7),
    ("landsat/ls_2021-10-29.tif", 4, 3, 7),
    ("landsat/ls_2022-05-24.tif", 4, 3, 7),
    ("landsat/ls_2022-05-25.tif", 4, 3, 7),
    ("landsat/ls_2022-06-25.tif", 4, 3, 7),
    ("landsat/ls_2022-06-26.tif", 4, 3, 7),
    ("landsat/ls_2022-07-03.tif", 4, 3, 7),
    ("landsat/ls_2022-07-11.tif", 4, 3, 7),
    ("landsat/ls_2022-07-12.tif", 4, 3, 7),
    ("landsat/ls_2022-07-19.tif", 4, 3, 7),
    ("landsat/ls_2022-07-20.tif", 4, 3, 7),
    ("landsat/ls_2022-07-27.tif", 4, 3, 7),
    ("landsat/ls_2022-07-28.tif", 4, 3, 7),
    ("landsat/ls_2022-08-12.tif", 4, 3, 7),
    ("landsat/ls_2022-08-13.tif", 4, 3, 7),
    ("landsat/ls_2022-08-20.tif", 4, 3, 7),
    ("landsat/ls_2022-08-21.tif", 4, 3, 7),
    ("landsat/ls_2022-09-05.tif", 4, 3, 7),
    ("landsat/ls_2022-09-21.tif", 4, 3, 7),
    ("landsat/ls_2022-10-08.tif", 4, 3, 7),
    ("landsat/ls_2022-11-09.tif", 4, 3, 7),
    ("landsat/ls_2023-02-12.tif", 4, 3, 7),
    ("landsat/ls_2023-05-19.tif", 4, 3, 7),
    ("landsat/ls_2023-05-20.tif", 4, 3, 7),
    ("landsat/ls_2023-05-28.tif", 4, 3, 7),
    ("landsat/ls_2023-06-04.tif", 4, 3, 7),
    ("landsat/ls_2023-06-21.tif", 4, 3, 7),
    ("landsat/ls_2023-06-28.tif", 4, 3, 7),
    ("landsat/ls_2023-06-29.tif", 4, 3, 7),
    ("landsat/ls_2023-07-06.tif", 4, 3, 7),
    ("landsat/ls_2023-07-07.tif", 4, 3, 7),
    ("landsat/ls_2023-07-15.tif", 4, 3, 7),
    ("landsat/ls_2023-07-22.tif", 4, 3, 7),
    ("landsat/ls_2023-07-23.tif", 4, 3, 7),
    ("landsat/ls_2023-07-30.tif", 4, 3, 7),
    ("landsat/ls_2023-07-31.tif", 4, 3, 7),
    ("landsat/ls_2023-08-07.tif", 4, 3, 7),
    ("landsat/ls_2023-08-08.tif", 4, 3, 7),
    ("landsat/ls_2023-08-16.tif", 4, 3, 7),
    ("landsat/ls_2023-08-23.tif", 4, 3, 7),
    ("landsat/ls_2023-08-24.tif", 4, 3, 7),
    ("landsat/ls_2023-09-08.tif", 4, 3, 7),
    ("landsat/ls_2023-09-16.tif", 4, 3, 7),
    ("landsat/ls_2023-09-24.tif", 4, 3, 7),
    ("landsat/ls_2023-09-25.tif", 4, 3, 7),
    ("landsat/ls_2023-10-02.tif", 4, 3, 7),
    ("landsat/ls_2023-10-18.tif", 4, 3, 7),
    ("landsat/ls_2024-04-03.tif", 4, 3, 7),
    ("landsat/ls_2024-05-29.tif", 4, 3, 7),
    ("landsat/ls_2024-05-30.tif", 4, 3, 7),
    ("landsat/ls_2024-06-06.tif", 4, 3, 7),
    ("landsat/ls_2024-06-07.tif", 4, 3, 7),
    ("landsat/ls_2024-06-14.tif", 4, 3, 7),
    ("landsat/ls_2024-06-15.tif", 4, 3, 7),
    ("landsat/ls_2024-06-22.tif", 4, 3, 7),
    ("landsat/ls_2024-07-09.tif", 4, 3, 7),
    ("landsat/ls_2024-07-16.tif", 4, 3, 7),
    ("landsat/ls_2024-07-17.tif", 4, 3, 7),
    ("landsat/ls_2024-07-24.tif", 4, 3, 7),
    ("landsat/ls_2024-07-25.tif", 4, 3, 7),
    ("landsat/ls_2024-08-01.tif", 4, 3, 7),
    ("landsat/ls_2024-08-02.tif", 4, 3, 7),
    ("landsat/ls_2024-08-18.tif", 4, 3, 7),
    ("landsat/ls_2024-08-26.tif", 4, 3, 7),
    ("landsat/ls_2024-09-11.tif", 4, 3, 7),
    ("landsat/ls_2025-01-16.tif", 4, 3, 7),
    ("landsat/ls_2025-05-09.tif", 4, 3, 7),
    ("landsat/ls_2025-05-17.tif", 4, 3, 7),
    ("landsat/ls_2025-05-24.tif", 4, 3, 7),
    ("landsat/ls_2025-06-01.tif", 4, 3, 7),
    ("landsat/ls_2025-06-02.tif", 4, 3, 7),
    ("landsat/ls_2025-06-09.tif", 4, 3, 7),
    ("landsat/ls_2025-06-18.tif", 4, 3, 7),
    ("landsat/ls_2025-07-03.tif", 4, 3, 7),
    ("landsat/ls_2025-07-04.tif", 4, 3, 7),
    ("landsat/ls_2025-07-11.tif", 4, 3, 7),
    ("landsat/ls_2025-07-12.tif", 4, 3, 7),
    ("landsat/ls_2025-07-19.tif", 4, 3, 7),
    ("landsat/ls_2025-07-20.tif", 4, 3, 7),
    ("landsat/ls_2025-07-27.tif", 4, 3, 7),
    ("landsat/ls_2025-08-12.tif", 4, 3, 7),
    ("landsat/ls_2025-08-13.tif", 4, 3, 7),
    ("landsat/ls_2025-08-21.tif", 4, 3, 7),
    ("landsat/ls_2025-08-28.tif", 4, 3, 7),
    ("landsat/ls_2025-08-29.tif", 4, 3, 7),
    ("landsat/ls_2025-09-06.tif", 4, 3, 7),
    ("landsat/ls_2025-09-13.tif", 4, 3, 7),
    ("landsat/ls_2026-05-27.tif", 4, 3, 7),
    ("landsat/ls_2026-06-04.tif", 4, 3, 7),
    ("landsat/ls_2026-06-13.tif", 4, 3, 7),
    ("landsat/ls_2026-06-21.tif", 4, 3, 7),
    ("landsat/ls_2026-06-28.tif", 4, 3, 7),
    ("landsat/ls_2026-06-29.tif", 4, 3, 7),
    ("landsat/ls_2026-07-06.tif", 4, 3, 7),
    ("landsat/ls_2026-07-14.tif", 4, 3, 7),
    ("landsat/ls_2026-07-23.tif", 4, 3, 7),
    ("landsat/ls_2026-07-31.tif", 4, 3, 7),
    ("landsat/ls_2026-08-07.tif", 4, 3, 7),
    ("landsat/ls_2026-08-08.tif", 4, 3, 7),
    ("landsat/ls_2026-08-15.tif", 4, 3, 7),
    ("landsat/ls_2026-08-16.tif", 4, 3, 7),
    ("landsat/ls_2026-08-23.tif", 4, 3, 7),
]
dates = np.array(["2020-02-04", "2020-05-26", "2020-06-20", "2020-07-06", "2020-07-13", "2020-07-22", "2020-07-29", "2020-08-07", "2020-08-23", "2020-08-30", "2020-09-08", "2020-09-24", "2020-10-10", "2020-10-26", "2021-06-14", "2021-06-30", "2021-07-09", "2021-07-16", "2021-07-25", "2021-08-01", "2021-08-10", "2021-08-26", "2021-09-11", "2021-10-04", "2021-10-29", "2022-05-24", "2022-05-25", "2022-06-25", "2022-06-26", "2022-07-03", "2022-07-11", "2022-07-12", "2022-07-19", "2022-07-20", "2022-07-27", "2022-07-28", "2022-08-12", "2022-08-13", "2022-08-20", "2022-08-21", "2022-09-05", "2022-09-21", "2022-10-08", "2022-11-09", "2023-02-12", "2023-05-19", "2023-05-20", "2023-05-28", "2023-06-04", "2023-06-21", "2023-06-28", "2023-06-29", "2023-07-06", "2023-07-07", "2023-07-15", "2023-07-22", "2023-07-23", "2023-07-30", "2023-07-31", "2023-08-07", "2023-08-08", "2023-08-16", "2023-08-23", "2023-08-24", "2023-09-08", "2023-09-16", "2023-09-24", "2023-09-25", "2023-10-02", "2023-10-18", "2024-04-03", "2024-05-29", "2024-05-30", "2024-06-06", "2024-06-07", "2024-06-14", "2024-06-15", "2024-06-22", "2024-07-09", "2024-07-16", "2024-07-17", "2024-07-24", "2024-07-25", "2024-08-01", "2024-08-02", "2024-08-18", "2024-08-26", "2024-09-11", "2025-01-16", "2025-05-09", "2025-05-17", "2025-05-24", "2025-06-01", "2025-06-02", "2025-06-09", "2025-06-18", "2025-07-03", "2025-07-04", "2025-07-11", "2025-07-12", "2025-07-19", "2025-07-20", "2025-07-27", "2025-08-12", "2025-08-13", "2025-08-21", "2025-08-28", "2025-08-29", "2025-09-06", "2025-09-13", "2026-05-27", "2026-06-04", "2026-06-13", "2026-06-21", "2026-06-28", "2026-06-29", "2026-07-06", "2026-07-14", "2026-07-23", "2026-07-31", "2026-08-07", "2026-08-08", "2026-08-15", "2026-08-16", "2026-08-23"], dtype="datetime64[D]")
# Time in years since the first date, as Janus fits trends (365.2425-day years).
years = np.array([0, 0.3066455848, 0.37509326, 0.4188997721, 0.4380651211, 0.4627062842, 0.4818716332, 0.5065127963, 0.5503193084, 0.5694846575, 0.5941258205, 0.6379323326, 0.6817388447, 0.7255453569, 1.358001875, 1.401808388, 1.426449551, 1.4456149, 1.470256063, 1.489421412, 1.514062575, 1.557869087, 1.601675599, 1.66464746, 1.733095135, 2.299841886, 2.302579793, 2.38745491, 2.390192817, 2.409358166, 2.431261422, 2.433999329, 2.453164678, 2.455902585, 2.475067934, 2.477805841, 2.518874446, 2.521612353, 2.540777702, 2.543515609, 2.584584215, 2.628390727, 2.674935146, 2.76254817, 3.022649336, 3.285488408, 3.288226315, 3.310129571, 3.32929492, 3.37583934, 3.395004689, 3.397742596, 3.416907945, 3.419645852, 3.441549108, 3.460714457, 3.463452364, 3.482617713, 3.48535562, 3.504520969, 3.507258876, 3.529162132, 3.548327481, 3.551065388, 3.592133993, 3.614037249, 3.635940505, 3.638678412, 3.657843761, 3.701650273, 4.161618651, 4.314941443, 4.31767935, 4.336844699, 4.339582606, 4.358747955, 4.361485862, 4.380651211, 4.42719563, 4.446360979, 4.449098886, 4.468264235, 4.471002142, 4.490167491, 4.492905398, 4.536711911, 4.558615167, 4.602421679, 4.950135869, 5.25951936, 5.281422616, 5.300587966, 5.322491222, 5.325229129, 5.344394478, 5.369035641, 5.410104246, 5.412842153, 5.432007502, 5.434745409, 5.453910758, 5.456648665, 5.475814014, 5.519620526, 5.522358433, 5.544261689, 5.563427038, 5.566164945, 5.588068201, 5.60723355, 6.308137744, 6.330041, 6.354682163, 6.376585419, 6.395750768, 6.398488675, 6.417654024, 6.43955728, 6.464198443, 6.4861017, 6.505267049, 6.508004956, 6.527170305, 6.529908212, 6.549073561])
qa_rule = "landsat_c2"

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

# The pixel (column, row of the files), its centre at x -63.1191015, y -10.1301365 (EPSG:4326).
col, row = 896, 186
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

# CODED (degradation), fitted on the chart with these parameters: the same Zeit calls Janus makes
# (its model is drawn from their results).
import xarray as xr
import zeit

# The bands by role (bands of each date's file), as reflectance x 10000 (Zeit's convention:
# 0-1 reflectance x 10000; Landsat C2 L2 numbers x 0.0000275 - 0.2, x 10000; set `units`).
roles = {"blue": 1, "green": 2, "nir": 4, "red": 3, "swir1": 5, "swir2": 6}
order = [r for r in ("blue", "green", "red", "nir", "swir1", "swir2", "thermal") if r in roles]
raw = {r: np.array([read(layer[0], roles[r], Window(col, row, 1, 1))[0, 0] for layer in layers])
       for r in order}
optical = np.concatenate([raw[r] for r in order if r != "thermal"])
med = np.nanmedian(optical)
units = "unit" if med <= 2 else "landsat_c2" if med >= 6500 else "x10000"
to_x10000 = {"unit": lambda v: v * 10000, "landsat_c2": lambda v: (v * 2.75e-5 - 0.2) * 10000,
             "x10000": lambda v: v}[units]
cube = np.stack([to_x10000(raw[r]) for r in order])  # (band, time); the thermal band: deg C x 100 in Zeit
bands_cube = xr.DataArray(cube.T[:, :, None, None], dims=("time", "band", "y", "x"),
                          coords={"time": dates.astype("datetime64[ns]"), "band": order})
# Fmask codes (0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 no observation) from the quality band.
complete = np.all(np.isfinite(cube), axis=0)
q = np.array([read(path, qa, Window(col, row, 1, 1), raw=True)[0, 0] for path, _, _, qa in layers])
c = np.where(np.isfinite(q) & (q >= 0), q, 255).astype(np.int64)
if qa_rule == "fmask":
    f = c.astype(np.int32)
elif qa_rule == "landsat_c2":
    f = np.zeros(c.shape, np.int32)
    f[(c & 128) != 0] = 1; f[(c & 32) != 0] = 3; f[(c & 16) != 0] = 2; f[(c & 10) != 0] = 4; f[(c & 1) != 0] = 255
elif qa_rule == "hls_fmask":
    f = np.zeros(c.shape, np.int32)
    f[(c & 32) != 0] = 1; f[(c & 16) != 0] = 3; f[(c & 8) != 0] = 2; f[(c & 6) != 0] = 4; f[c == 255] = 255
else:
    f = np.where(c != 0, 0, 4).astype(np.int32)
fmask_codes = np.where(complete, f, 255)
fmask = fmask_codes.astype(np.int32)[:, None, None]
result1 = zeit.coded(bands_cube, start=2022.0, train_years=2.0, consec=3, thresh=3.0, min_years=3.0, min_obs=6, direction='loss', max_events=3, forest_ndfi=0.5, scale=10000.0, cloud_threshold=0.05, nodata=None, n_jobs=1)
print(result1)
