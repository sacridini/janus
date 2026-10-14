#include "multidim.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>

#include <cpl_conv.h>
#include <cpl_string.h>
#include <gdal_priv.h>

#include "cube.hpp"

namespace {

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    return s;
}

std::string trim(const std::string& s) {
    const size_t a = s.find_first_not_of(" \t\r\n"), b = s.find_last_not_of(" \t\r\n");
    return a == std::string::npos ? "" : s.substr(a, b - a + 1);
}

bool startsWith(const std::string& s, const char* p) { return s.rfind(p, 0) == 0; }

bool endsWith(const std::string& s, const char* p) {
    const size_t n = std::strlen(p);
    return s.size() >= n && s.compare(s.size() - n, n, p) == 0;
}

// The name of a subdataset (or of its container) has a variable after the file?
bool namesVariable(const std::string& path) {
    const std::string file = datasetFile(path);
    if (file == path) return false;
    const size_t end = path.find('"', path.find(':') + 2);
    return end != std::string::npos && end + 1 < path.size();
}

// --- Calendars -----------------------------------------------------------------

enum class Calendar { Standard, NoLeap, AllLeap, Day360 };

Calendar calendarOf(const std::string& c) {
    const std::string l = lower(trim(c));
    if (l == "noleap" || l == "365_day") return Calendar::NoLeap;
    if (l == "all_leap" || l == "366_day") return Calendar::AllLeap;
    if (l == "360_day") return Calendar::Day360;
    return Calendar::Standard; // standard, gregorian, proleptic_gregorian, julian, none, ""
}

const int kCum[13] = {0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365};
const int kCumLeap[13] = {0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 366};

int64_t floorDiv(int64_t a, int64_t b) { return a / b - ((a % b != 0) && ((a < 0) != (b < 0))); }

// Day number of a date in a model calendar (any fixed origin), and back.
int64_t calendarDays(Calendar c, int y, int m, int d) {
    switch (c) {
    case Calendar::NoLeap: return int64_t(y) * 365 + kCum[m - 1] + d - 1;
    case Calendar::AllLeap: return int64_t(y) * 366 + kCumLeap[m - 1] + d - 1;
    case Calendar::Day360: return int64_t(y) * 360 + (m - 1) * 30 + d - 1;
    default: return daysFromCivil(y, unsigned(m), unsigned(d));
    }
}

void calendarDate(Calendar c, int64_t days, int& y, int& m, int& d) {
    if (c == Calendar::Standard) {
        unsigned mm, dd;
        civilFromDays(days, y, mm, dd);
        m = int(mm);
        d = int(dd);
        return;
    }
    const int len = c == Calendar::NoLeap ? 365 : c == Calendar::AllLeap ? 366 : 360;
    y = int(floorDiv(days, len));
    const int r = int(days - int64_t(y) * len);
    if (c == Calendar::Day360) {
        m = r / 30 + 1;
        d = r % 30 + 1;
        return;
    }
    const int* cum = c == Calendar::NoLeap ? kCum : kCumLeap;
    m = 1;
    while (m < 12 && r >= cum[m]) ++m;
    d = r - cum[m - 1] + 1;
}

bool isLeap(int y) { return (y % 4 == 0 && y % 100 != 0) || y % 400 == 0; }

int monthLength(int y, int m) {
    static const int len[12] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
    return m == 2 && isLeap(y) ? 29 : len[m - 1];
}

// A real date for a model calendar's date: the day clamped to the month.
double unixOf(int y, int m, int d, double secondsOfDay) {
    d = std::clamp(d, 1, monthLength(y, m));
    return double(daysFromCivil(y, unsigned(m), unsigned(d))) * 86400.0 + secondsOfDay;
}

// "1970-01-01", "1900-1-1 00:00:00.0", "2000-01-01T12:00:00Z", "1950-01-01 00:00 -3:00".
bool parseReference(const std::string& s, int& y, int& m, int& d, double& secondsOfDay) {
    y = 0;
    m = d = 1;
    secondsOfDay = 0;
    const char* p = s.c_str();
    char* end = nullptr;
    const long yy = std::strtol(p, &end, 10);
    if (end == p) return false;
    y = int(yy);
    p = end;
    if (*p == '-') {
        m = int(std::strtol(p + 1, &end, 10));
        p = end;
        if (*p == '-') {
            d = int(std::strtol(p + 1, &end, 10));
            p = end;
        }
    }
    if (m < 1 || m > 12 || d < 1 || d > 31) return false;
    while (*p == ' ' || *p == 'T' || *p == 't') ++p;
    if (std::isdigit((unsigned char)*p)) {
        const double h = std::strtod(p, &end);
        p = end;
        double mi = 0, sec = 0;
        if (*p == ':') {
            mi = std::strtod(p + 1, &end);
            p = end;
            if (*p == ':') {
                sec = std::strtod(p + 1, &end);
                p = end;
            }
        }
        secondsOfDay = h * 3600 + mi * 60 + sec;
        while (*p == ' ') ++p;
        if (*p == '+' || *p == '-') { // time zone: the reference is local time
            const int sign = *p == '-' ? -1 : 1;
            const double th = std::strtod(p + 1, &end);
            double tm = 0;
            if (*end == ':') tm = std::strtod(end + 1, nullptr);
            if (std::fabs(th) <= 14) secondsOfDay -= sign * (std::fabs(th) * 3600 + tm * 60);
        }
    }
    return true;
}

// Attribute of an array of a multidimensional dataset (e.g. the time
// coordinate's calendar in a Zarr store), "" if absent.
std::string arrayAttribute(const std::string& path, const std::string& array, const char* attr) {
    const std::string container = containerOf(path);
    GDALDataset* ds = GDALDataset::FromHandle(
        GDALOpenEx(container.c_str(), GDAL_OF_MULTIDIM_RASTER | GDAL_OF_READONLY, nullptr, nullptr, nullptr));
    if (!ds) return "";
    std::string v;
    if (auto root = ds->GetRootGroup())
        if (auto arr = root->OpenMDArray(array))
            if (auto a = arr->GetAttribute(attr))
                if (const char* s = a->ReadAsString()) v = s;
    GDALClose(ds);
    return v;
}

std::string formatCoordinate(const std::string& raw) {
    char* end = nullptr;
    const double v = std::strtod(raw.c_str(), &end);
    if (end == raw.c_str()) return raw;
    char b[48];
    std::snprintf(b, sizeof(b), "%.6g", v);
    return b;
}

} // namespace

std::string containerOf(const std::string& dataset) {
    if (startsWith(dataset, "ZARR:\"")) return dataset.substr(0, dataset.find('"', 6) + 1);
    return datasetFile(dataset);
}

bool cfTimeToUnix(double value, const std::string& units, const std::string& calendar, double& unixSec) {
    if (!std::isfinite(value)) return false;
    const std::string u = lower(units);
    const size_t since = u.find(" since ");
    if (since == std::string::npos) return false;
    const std::string unit = trim(u.substr(0, since));
    int y, m, d;
    double sod;
    if (!parseReference(trim(units.substr(since + 7)), y, m, d, sod)) return false;
    const Calendar cal = calendarOf(calendar);
    // Whole months / years: calendar steps (monthly and yearly products).
    const bool months = unit == "month" || unit == "months" || unit == "mon";
    const bool years = unit == "year" || unit == "years" || unit == "yr" || unit == "yrs";
    if ((months || years) && std::fabs(value - std::round(value)) < 1e-6) {
        const int64_t total = int64_t(y) * 12 + (m - 1) + int64_t(std::llround(value)) * (years ? 12 : 1);
        const int y2 = int(floorDiv(total, 12)), m2 = int(total - int64_t(y2) * 12) + 1;
        unixSec = unixOf(y2, m2, d, sod);
        return true;
    }
    double step;
    if (unit == "s" || unit == "sec" || unit == "secs" || unit == "second" || unit == "seconds") step = 1;
    else if (unit == "ms" || unit == "msec" || unit == "msecs" || unit == "millisecond" || unit == "milliseconds") step = 0.001;
    else if (unit == "min" || unit == "mins" || unit == "minute" || unit == "minutes") step = 60;
    else if (unit == "h" || unit == "hr" || unit == "hrs" || unit == "hour" || unit == "hours") step = 3600;
    else if (unit == "d" || unit == "day" || unit == "days") step = 86400;
    else if (unit == "week" || unit == "weeks") step = 7 * 86400;
    else if (months) step = 365.242198781 / 12 * 86400; // udunits' month and year
    else if (years) step = 365.242198781 * 86400;
    else return false;
    const double total = double(calendarDays(cal, y, m, d)) * 86400.0 + sod + value * step;
    const int64_t days = int64_t(std::floor(total / 86400.0));
    const double rest = total - double(days) * 86400.0;
    if (cal == Calendar::Standard) {
        unixSec = double(days) * 86400.0 + rest;
        return true;
    }
    int y2, m2, d2;
    calendarDate(cal, days, y2, m2, d2);
    unixSec = unixOf(y2, m2, d2, rest);
    return true;
}

std::vector<MdVariable> listVariables(const std::string& input) {
    std::vector<MdVariable> out;
    const std::string path = gdalPath(input);
    if (namesVariable(path)) return out;
    GDALDataset* ds = openRaster(path);
    if (!ds) return out;
    if (ds->GetRasterCount() > 0) { // a single variable (or an ordinary raster): opens directly
        GDALClose(ds);
        return out;
    }
    CSLConstList sds = ds->GetMetadata("SUBDATASETS");
    for (int i = 1; sds; ++i) {
        const char* name = CSLFetchNameValue(sds, ("SUBDATASET_" + std::to_string(i) + "_NAME").c_str());
        if (!name) break;
        const char* d = CSLFetchNameValue(sds, ("SUBDATASET_" + std::to_string(i) + "_DESC").c_str());
        MdVariable v;
        v.dataset = name;
        const size_t colon = v.dataset.find_last_of(':');
        v.name = colon == std::string::npos ? v.dataset : v.dataset.substr(colon + 1);
        while (!v.name.empty() && v.name[0] == '/') v.name.erase(0, 1);
        // Shape: "[24x50x60] t2m (32-bit floating-point)" -> 24 x 50 x 60.
        const std::string desc = d ? d : "";
        std::vector<long> shape;
        if (desc.size() > 1 && desc[0] == '[') {
            const char* p = desc.c_str() + 1;
            char* end = nullptr;
            for (long n = std::strtol(p, &end, 10); end != p; n = std::strtol(p, &end, 10)) {
                shape.push_back(n);
                p = end;
                if (*p != 'x') break;
                ++p;
            }
        }
        // Not a raster: bounds of cells (n x 2), coordinate reference variables.
        const std::string l = lower(v.name);
        if ((shape.size() >= 2 && shape.back() <= 2) || endsWith(l, "_bnds") || endsWith(l, "_bounds") ||
            l == "crs" || l == "spatial_ref")
            continue;
        for (size_t k = 0; k < shape.size(); ++k) v.desc += (k ? " x " : "") + std::to_string(shape[k]);
        if (v.desc.empty()) v.desc = desc;
        out.push_back(v);
    }
    GDALClose(ds);
    // Long names and units, from the multidimensional view of the container.
    if (out.empty()) return out;
    GDALDataset* md = GDALDataset::FromHandle(
        GDALOpenEx(containerOf(out[0].dataset).c_str(), GDAL_OF_MULTIDIM_RASTER | GDAL_OF_READONLY, nullptr, nullptr, nullptr));
    if (!md) return out;
    if (auto root = md->GetRootGroup())
        for (MdVariable& v : out) {
            auto arr = root->OpenMDArrayFromFullname("/" + v.name);
            if (!arr) arr = root->OpenMDArray(v.name);
            if (!arr) continue;
            auto text = [&](const char* name) -> std::string {
                const auto a = arr->GetAttribute(name);
                const char* s = a ? a->ReadAsString() : nullptr;
                return s ? s : "";
            };
            std::string longName = text("long_name");
            if (longName.empty()) longName = text("standard_name");
            const std::string units = !text("units").empty() ? text("units") : arr->GetUnit(); // Zarr: the array's unit
            if (!longName.empty()) v.desc += ", " + longName;
            if (!units.empty()) v.desc += " (" + units + ")";
        }
    GDALClose(md);
    return out;
}

bool readMdBands(GDALDataset* ds, const std::string& path, MdBands& out) {
    out = MdBands{};
    const int n = ds->GetRasterCount();
    if (n < 1) return false;
    std::vector<std::vector<std::pair<std::string, std::string>>> coords(static_cast<size_t>(n)); // per band: (dim, value)
    std::map<std::string, std::string> units;
    for (int b = 1; b <= n; ++b) {
        CSLConstList md = ds->GetRasterBand(b)->GetMetadata();
        for (int i = 0; md && md[i]; ++i) {
            const std::string kv = md[i];
            const size_t eq = kv.find('=');
            if (eq == std::string::npos) continue;
            const std::string k = kv.substr(0, eq), v = kv.substr(eq + 1);
            if (startsWith(k, "NETCDF_DIM_")) coords[size_t(b - 1)].emplace_back(k.substr(11), v);
            else if (startsWith(k, "DIM_") && endsWith(k, "_VALUE")) coords[size_t(b - 1)].emplace_back(k.substr(4, k.size() - 10), v);
            else if (startsWith(k, "DIM_") && endsWith(k, "_UNIT")) units[k.substr(4, k.size() - 9)] = v;
        }
    }
    if (coords[0].empty()) return false;
    std::vector<std::string> dims;
    for (const auto& [dim, _] : coords[0]) dims.push_back(dim);
    for (const auto& c : coords)
        if (c.size() != dims.size()) return false; // not one value per dimension on every band
    auto unitOf = [&](const std::string& dim) {
        if (const char* u = ds->GetMetadataItem((dim + "#units").c_str())) return std::string(u); // netCDF
        const auto it = units.find(dim);
        return it == units.end() ? std::string() : it->second;
    };
    // The time axis: CF units ("... since ..."), else a usual name, else the first dimension.
    int ti = -1;
    for (size_t k = 0; k < dims.size() && ti < 0; ++k)
        if (lower(unitOf(dims[k])).find(" since ") != std::string::npos) ti = int(k);
    for (size_t k = 0; k < dims.size() && ti < 0; ++k) {
        const std::string l = lower(dims[k]);
        if (l == "time" || l == "t" || l == "date" || l == "valid_time" || l == "times") ti = int(k);
    }
    if (ti < 0) ti = 0;
    out.timeDim = dims[size_t(ti)];
    const std::string tunits = unitOf(out.timeDim);
    if (const char* c = ds->GetMetadataItem((out.timeDim + "#calendar").c_str())) out.calendar = c;
    else if (lower(tunits).find(" since ") != std::string::npos) out.calendar = arrayAttribute(path, out.timeDim, "calendar");

    out.time.resize(size_t(n));
    out.level.resize(size_t(n));
    out.timeIsDate = lower(tunits).find(" since ") != std::string::npos;
    std::map<std::string, int> levelIndex;
    for (int b = 0; b < n; ++b) {
        const auto& c = coords[size_t(b)];
        const double v = std::strtod(c[size_t(ti)].second.c_str(), nullptr);
        double t = v;
        if (out.timeIsDate && !cfTimeToUnix(v, tunits, out.calendar, t)) out.timeIsDate = false;
        out.time[size_t(b)] = t;
        std::string lv;
        for (size_t k = 0; k < c.size(); ++k)
            if (int(k) != ti) lv += (lv.empty() ? "" : ", ") + c[k].first + " " + formatCoordinate(c[k].second);
        const auto it = levelIndex.emplace(lv, int(out.levels.size()));
        if (it.second) out.levels.push_back(lv);
        out.level[size_t(b)] = it.first->second;
    }
    if (!out.timeIsDate) // undecodable: the raw coordinate
        for (int b = 0; b < n; ++b) out.time[size_t(b)] = std::strtod(coords[size_t(b)][size_t(ti)].second.c_str(), nullptr);
    return true;
}
