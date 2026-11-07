"""Janus 1.17.0 (2026-11-07): pin 1 of landsat, reproduced outside Janus with Zeit.
Needs: pip install "zeit-cdts>=3.2"
"""
import xarray as xr
import zeit

# Pin 1: column 896, row 186 of the files, its centre at x -63.1191015, y -10.1301365 (EPSG:4326)
src = "landsat"
qa = zeit.load_raster(src, band=7, masked=True, pixel=(896, 186))  # QA_PIXEL
bands_cube = zeit.load_raster(
    src, bands={"blue": 1, "green": 2, "red": 3, "nir": 4, "swir1": 5, "swir2": 6}, scale=True,
    masked=True, pixel=(896, 186)
)
# Zeit takes reflectance x 10000: Janus tells the units by the median (<= 2: 0-1; >= 6500: Landsat Collection 2 digital numbers)
median = float(bands_cube.median())
bands_cube = bands_cube * 10000 if median <= 2 else (bands_cube * 2.75e-05 - 0.2) * 10000 if median >= 6500 else bands_cube
complete = bands_cube.notnull().all("band")  # a date missing a band is no observation
c = qa.fillna(0).astype("int64")
fmask = xr.where((c & 1) > 0, 255, xr.where((c & 10) > 0, 4, xr.where((c & 16) > 0, 2, xr.where((c & 32) > 0, 3, xr.where((c & 128) > 0, 1, 0)))))  # Landsat C2 QA_PIXEL bits as Fmask codes: 0 clear, 1 water, 2 shadow, 3 snow, 4 cloud, 255 none
fmask = fmask.where((qa >= 0) & complete, 255)
bands_cube = bands_cube.where(fmask <= 1)  # only the clear observations (Fmask 0 and 1)
bands_cube = bands_cube.expand_dims(y=1, x=1).transpose("time", "band", "y", "x")  # CODED wants y and x
result = zeit.coded(bands_cube, start=2022.0, train_years=2.0, scale=10000.0)

print(result)
