"""Janus 1.17.0 (2026-11-07): pin 1 of LT_Stack_NDVI_Rondonia.tif, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2"
"""
import zeit

# Pin 1: column 306, row 913 of the files, its centre at x -63.3442980178, y -10.2209863869 (EPSG:4326)
series = zeit.load_raster("LT_Stack_NDVI_Rondonia.tif", masked=True, pixel=(306, 913))
result = zeit.beast(
    series, season='none', tcp_minmax=(0, 10), scp_minmax=(0, 10), torder_minmax=(0, 1),
    samples=2000, chains=2
)

print(result)
