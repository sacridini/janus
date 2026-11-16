"""Janus 1.17.0 (2026-11-07): pin 1 of LT_Stack_NDVI_Rondonia.tif, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2"
"""
import zeit

# Pin 1: column 1272, row 1414 of the files, its centre at x -63.0839662485, y -10.3560031742 (EPSG:4326)
series = zeit.load_raster("LT_Stack_NDVI_Rondonia.tif", masked=True, pixel=(1272, 1414))
fit = zeit.landtrendr(series)
events = zeit.extract_events(fit, event_type='loss')

print(fit)
print(events)
