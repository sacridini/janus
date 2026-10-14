#pragma once

#ifndef JANUS_VERSION
#define JANUS_VERSION "dev"
#endif

// Shared by janus (the GUI) and jn.com (the Windows console launcher).
inline const char* kUsage =
    "Janus " JANUS_VERSION " - raster time series viewer\n"
    "\n"
    "Usage:\n"
    "  jn [options] [input ...]\n"
    "\n"
    "Inputs:\n"
    "  folder\\             every raster in the folder, 1 file = 1 date\n"
    "  series.tif          1 multiband file, 1 band = 1 date\n"
    "  a.tif b.tif ...     several files, 1 file = 1 date\n"
    "  \"ndvi_*.tif\"        wildcard pattern (* and ?)\n"
    "  https://.../cog.tif cloud-optimized GeoTIFFs on the web, read in place (only\n"
    "  s3://, gs://, az:// the blocks shown are downloaded); several URLs = a series\n"
    "  clim.nc, data.zarr  NetCDF / Zarr / HDF: a variable's time steps (CF dates);\n"
    "                      with several variables the window asks which one, or\n"
    "                      name it: NETCDF:\"clim.nc\":t2m, ZARR:\"data.zarr\":/t2m\n"
    "  plots.gpkg ...      vector files (GeoJSON, Shapefile, GeoPackage, KML, CSV\n"
    "                      of points) open as vector layers over the series\n"
    "  Dates are read from the file name or the band description\n"
    "  (YYYY-MM-DD, YYYYMMDD, YYYY_MM, YYYY, A2001001...).\n"
    "\n"
    "Options:\n"
    "  --band N            band used when each file is one date (default: 1)\n"
    "  --budget MB         memory for the cube overview, this run only\n"
    "                      (default: Settings, 1024)\n"
    "  --threads N         background reader threads, this run only (default: the\n"
    "                      processing threads in Settings, at most 12; HDD = 1);\n"
    "                      with --selftest-zeit, Zeit's threads\n"
    "  -h, --help          show this help\n"
    "  --version           show the version\n"
    "\n"
    "Developer options:\n"
    "  --zeit-python EXE   Python with Zeit to use instead of the bundled runtime\n"
    "                      (or set JANUS_ZEIT_PYTHON)\n"
    "  --zeit-bridge PY    bridge script to use (or set JANUS_ZEIT_BRIDGE)\n"
    "  --measure-startup   print startup timings and exit after the first frame\n"
    "  --measure-cache on|off IN  time exact series and ROI from the files, build the\n"
    "                      overview (and the full-resolution cache if on), time the\n"
    "                      reads again from the cache, in a hidden window\n"
    "  --measure-vectors IN VEC...  time vector files over the series IN (reading,\n"
    "                      placing, drawing, picking, extracting), in a hidden window\n"
    "  --describe IN       print how IN opens (dates, size, CRS, variables of a\n"
    "                      NetCDF / Zarr, the series of the centre pixel) and exit\n"
    "  --selftest-zeit IN  run the Zeit tools end to end on IN without a window\n"
    "  --selftest-embeddings  check the embedding kernels and PCA, with timings\n"
    "  --measure-validation IN  time the strata of the map IN (a buffer of 1 px), its\n"
    "                      areas, a sample of 1000 points and the estimators\n"
    "  --selftest-validation  check the sampling design, strata, sample and accuracy\n"
    "                      estimators (Olofsson et al. 2014) and, with the runtime,\n"
    "                      the estimates against zeit.accuracy\n"
    "  --shots-validation DATA OUT  the validation tutorial's screenshots (PNGs in\n"
    "                      OUT) from tools/make_tutorial_data.py's DATA, in a hidden\n"
    "                      window\n"
    "  --selftest-embeddings-ui DIR [OUT]  a series of embeddings in every view, in a\n"
    "                      hidden window; PNGs of each in OUT\n"
    "  --selftest-ui A B [C [D [E [F [G]]]]]  drive the layers workflow in a hidden window\n"
    "                      (A, then B as a layer; C: several bands per date;\n"
    "                      D, E: categorical, with and without a colour table;\n"
    "                      F: B in another CRS, reprojected over it; G: spectra,\n"
    "                      ENVI and an EnMAP product; then a polygonal ROI,\n"
    "                      vector layers, the interpretation, an animation and a\n"
    "                      validation project on D; the basemap is checked with\n"
    "                      local tiles, no network)\n";

// Options that take a value (the launcher validates them before opening the GUI).
inline bool isValueOption(const char* a) {
    const char* opts[] = {"--band", "--budget", "--threads", "--zeit-python", "--zeit-bridge"};
    for (const char* o : opts) {
        const char *p = a, *q = o;
        while (*p && *p == *q) ++p, ++q;
        if (!*p && !*q) return true;
    }
    return false;
}
