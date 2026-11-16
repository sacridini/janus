"""Janus 1.17.0 (2026-11-07): pin 1 of EVI_Series_ID_EU_150-X026-Y027_2001_2025.tif, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2"
"""
import numpy as np
import zeit

# Pin 1: column 71, row 319 of the files, its centre at x 3481625, y 2607125 (EPSG:3035)
series = zeit.load_raster(
    "EVI_Series_ID_EU_150-X026-Y027_2001_2025.tif", masked=True, pixel=(71, 319)
)
series = series.squeeze("band", drop=True)  # the bands are dates of one named band
values = series.values.ravel()
# the gaps filled linearly (by date index), with weight 0: they pull neither the smoothing nor the curves
ok = np.isfinite(values)
idx = np.arange(len(values))
filled = np.interp(idx, idx[ok], values[ok])
series = series.copy(data=filled.reshape(series.shape))
weights = ok.astype(float).reshape(series.shape)
result = zeit.phenology(series, curve='elmore', weights=weights, annual=False, max_seasons=52)

print(result)
