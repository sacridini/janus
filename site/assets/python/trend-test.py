"""Janus 1.17.0 (2026-11-07): pin 1 of series, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2"
"""
import zeit

# Pin 1: column 1426, row 1199 of the files, its centre at x -38.0325580887, y -9.460742162 (EPSG:4326)
series = zeit.load_raster("series", masked=True, pixel=(1426, 1199))
result = zeit.ar_trend(series, time='year', min_valid=4)

print(result)
