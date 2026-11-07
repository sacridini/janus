"""Janus 1.17.0 (2026-11-07): pin 1 of EVI_Series_ID_EU_150-X026-Y027_2001_2025.tif, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2"
"""
import zeit

# Pin 1: column 435, row 153 of the files, its centre at x 3572625, y 2648625 (EPSG:3035)
series = zeit.load_raster(
    "EVI_Series_ID_EU_150-X026-Y027_2001_2025.tif", masked=True, pixel=(435, 153)
)
series = series.squeeze("band", drop=True)  # the bands are dates of one named band
result = zeit.bfast_monitor(series, 2009.9782608695652, start_time=2001.0, frequency=23)

print(result)
