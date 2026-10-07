#pragma once

#ifndef TSV_VERSION
#define TSV_VERSION "dev"
#endif

// Shared by tsv.exe (GUI) and tsv.com (console launcher).
inline const char* kUsage =
    "tsv " TSV_VERSION " - raster time series viewer\n"
    "\n"
    "Usage:\n"
    "  tsv [options] [input ...]\n"
    "\n"
    "Inputs:\n"
    "  folder\\             every raster in the folder, 1 file = 1 date\n"
    "  series.tif          1 multiband file, 1 band = 1 date\n"
    "  a.tif b.tif ...     several files, 1 file = 1 date\n"
    "  \"ndvi_*.tif\"        wildcard pattern (* and ?)\n"
    "  Dates are read from the file name or the band description\n"
    "  (YYYY-MM-DD, YYYYMMDD, YYYY_MM, YYYY, A2001001...).\n"
    "\n"
    "Options:\n"
    "  --band N            band used when each file is one date (default: 1)\n"
    "  --budget MB         GPU memory for the cube overview (default: 1024)\n"
    "  --threads N         background reader threads (default: auto; HDD = 1)\n"
    "  -h, --help          show this help\n"
    "  --version           show the version\n"
    "\n"
    "Developer options:\n"
    "  --zeit-python EXE   Python with Zeit to use instead of the bundled runtime\n"
    "                      (or set TSV_ZEIT_PYTHON)\n"
    "  --zeit-bridge PY    bridge script to use (or set TSV_ZEIT_BRIDGE)\n"
    "  --measure-startup   print startup timings and exit after the first frame\n"
    "  --selftest-zeit IN  run the Zeit tools end to end on IN without a window\n"
    "  --selftest-ui A B   drive the layers workflow (A, then B as a layer) in a hidden window\n";

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
