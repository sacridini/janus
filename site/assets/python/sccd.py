"""Janus 1.17.0 (2026-11-07): pin 1 of landsat, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2.1"
"""
import zeit

# Pin 1: column 378, row 778 of the files, its centre at x -63.3251555, y -10.1842055 (EPSG:4326)
src = "landsat"
qa = zeit.load_raster(src, band=7, masked=True, pixel=(378, 778))  # QA_PIXEL
bands_cube = zeit.load_raster(
    src, bands={"blue": 1, "green": 2, "red": 3, "nir": 4, "swir1": 5, "swir2": 6}, scale=True,
    masked=True, pixel=(378, 778)
)
result = zeit.sccd(bands_cube, qa=zeit.qa_to_fmask(qa, "landsat_c2"), scale='auto')

print(result)
