#pragma once

// NetCDF, Zarr, HDF and other containers of several variables, each a cube
// of (time, [level,] y, x): the variables a file offers, and where each band
// of a variable sits on its extra dimensions, with the dates decoded by the
// CF conventions ("days since 1970-01-01", calendars of climate models).
// GDAL reads them all: a variable is a subdataset (NETCDF:"f.nc":t2m,
// ZARR:"f.zarr":/t2m) whose bands are every combination of the extra
// dimensions; netCDF tags each band with NETCDF_DIM_<dim>=<value> and Zarr
// with DIM_<dim>_VALUE / DIM_<dim>_UNIT.

#include <string>
#include <vector>

class GDALDataset;

// One variable (a GDAL subdataset) of a container.
struct MdVariable {
    std::string dataset; // the GDAL name that opens it, e.g. NETCDF:"clim.nc":t2m
    std::string name;    // e.g. "t2m"
    std::string desc;    // e.g. "24 x 50 x 60, 2 m temperature (degC)"
};

// The container a variable comes from, as GDAL opens it: the file of
// NETCDF:"f.nc":t2m; ZARR:"store" for a Zarr array (a remote store needs the prefix).
std::string containerOf(const std::string& dataset);

// The variables of a container with several of them (a single-variable
// NetCDF opens directly and lists nothing); empty if `path` is not such a file.
std::vector<MdVariable> listVariables(const std::string& path);

// Where each band of a variable sits on its extra dimensions.
struct MdBands {
    std::string timeDim;            // "" = no time dimension recognised
    bool timeIsDate = false;        // time decoded to dates (CF units)
    std::vector<double> time;       // per band: Unix seconds if timeIsDate, else the coordinate
    std::vector<int> level;         // per band: index into `levels`
    std::vector<std::string> levels; // the other dimensions' values, e.g. "level 850" ("" with time alone)
    std::string calendar;           // CF calendar of the time axis ("" = standard)
};
// false if the bands carry no dimension tags (an ordinary multiband file).
bool readMdBands(GDALDataset* ds, const std::string& path, MdBands& out);

// A CF time value -> Unix seconds. units: "<unit> since <date>[ <time>]";
// calendar: standard / gregorian / proleptic_gregorian (also "" and julian,
// as Gregorian), noleap / 365_day, all_leap / 366_day, 360_day. Dates of the
// model calendars become the nearest real date (30 February -> 28 or 29).
// "months since" / "years since" with whole values count calendar months and
// years (as in monthly and yearly products). false if `units` is not CF time.
bool cfTimeToUnix(double value, const std::string& units, const std::string& calendar, double& unixSec);
