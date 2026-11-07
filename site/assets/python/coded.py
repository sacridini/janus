"""Janus 1.17.0 (2026-11-07): pin 1 of landsat, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2.1"
"""
import zeit

# Pin 1: column 896, row 186 of the files, its centre at x -63.1191015, y -10.1301365 (EPSG:4326)
src = "landsat"
qa = zeit.load_raster(src, band=7, masked=True, pixel=(896, 186))  # QA_PIXEL
bands_cube = zeit.load_raster(
    src, bands={"blue": 1, "green": 2, "red": 3, "nir": 4, "swir1": 5, "swir2": 6}, scale=True,
    masked=True, pixel=(896, 186)
)
bands_cube = bands_cube.where(zeit.qa_to_fmask(qa, "landsat_c2") <= 1)  # only the clear observations (Fmask 0 and 1)
result = zeit.coded(bands_cube, start=2022.0, train_years=2.0)

print(result)
