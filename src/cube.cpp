#include "cube.hpp"

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <future>
#include <regex>

#include <cpl_conv.h>
#include <cpl_error.h>
#include <gdal_priv.h>
#include <gdal_rat.h>
#include <ogr_spatialref.h>

#include "basemap.hpp"
#include "multidim.hpp"

namespace fs = std::filesystem;

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

// Days since 1970-01-01 (Howard Hinnant's algorithm), without relying on timegm.
int64_t daysFromCivil(int y, unsigned m, unsigned d) {
    y -= m <= 2;
    const int64_t era = (y >= 0 ? y : y - 399) / 400;
    const unsigned yoe = unsigned(y - era * 400);
    const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + int64_t(doe) - 719468;
}

void civilFromDays(int64_t z, int& y, unsigned& m, unsigned& d) {
    z += 719468;
    const int64_t era = (z >= 0 ? z : z - 146096) / 146097;
    const unsigned doe = unsigned(z - era * 146097);
    const unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const unsigned mp = (5 * doy + 2) / 153;
    d = doy - (153 * mp + 2) / 5 + 1;
    m = mp < 10 ? mp + 3 : mp - 9;
    y = int(int64_t(yoe) + era * 400 + (m <= 2));
}

// Looks for a date in the text. Returns the granularity found (Index = none).
static TimeGranularity parseDate(const std::string& s, double& unixSec) {
    static const std::regex ymd(R"((?:^|[^0-9])((?:19|20)\d{2})[-_.]?(0[1-9]|1[0-2])[-_.]?(0[1-9]|[12]\d|3[01])(?:[^0-9]|$))");
    static const std::regex ydoy(R"(A((?:19|20)\d{2})(\d{3})(?:[^0-9]|$))"); // MODIS: A2001001
    static const std::regex ym(R"((?:^|[^0-9])((?:19|20)\d{2})[-_.](0[1-9]|1[0-2])(?:[^0-9]|$))");
    static const std::regex y(R"((?:^|[^0-9])((?:19|20)\d{2})(?:[^0-9]|$))");
    std::smatch m;
    auto day = [](int64_t days) { return double(days) * 86400.0; };
    if (std::regex_search(s, m, ymd)) {
        unixSec = day(daysFromCivil(std::stoi(m[1]), std::stoi(m[2]), std::stoi(m[3])));
        return TimeGranularity::Day;
    }
    if (std::regex_search(s, m, ydoy)) {
        int doy = std::stoi(m[2]);
        if (doy >= 1 && doy <= 366) {
            unixSec = day(daysFromCivil(std::stoi(m[1]), 1, 1) + doy - 1);
            return TimeGranularity::Day;
        }
    }
    if (std::regex_search(s, m, ym)) {
        unixSec = day(daysFromCivil(std::stoi(m[1]), std::stoi(m[2]), 1));
        return TimeGranularity::Month;
    }
    if (std::regex_search(s, m, y)) {
        unixSec = day(daysFromCivil(std::stoi(m[1]), 1, 1));
        return TimeGranularity::Year;
    }
    return TimeGranularity::Index;
}

std::string describeSeriesFiles(const std::vector<std::string>& names) {
    if (names.empty()) return "";
    for (const std::string& n : names)
        if (isEnmapSpectralImage(n)) {
            double t = 0;
            CubeInfo fmt;
            fmt.timeIsDate = true;
            fmt.granularity = TimeGranularity::Day;
            const bool dated = parseDate(fs::u8path(n).stem().u8string(), t) != TimeGranularity::Index;
            return "EnMAP scene" + (dated ? ", " + formatTime(fmt, t) : std::string()) + " (opens its spectrum)";
        }
    std::vector<double> times;
    TimeGranularity gran = TimeGranularity::Year;
    for (const std::string& n : names) {
        double t = 0;
        const TimeGranularity g = parseDate(fs::u8path(n).stem().u8string(), t);
        if (g == TimeGranularity::Index) continue;
        times.push_back(t);
        gran = std::max(gran, g);
    }
    char buf[160];
    const int n = int(names.size());
    if (int(times.size()) < n) {
        if (times.empty()) std::snprintf(buf, sizeof(buf), "%d rasters, no dates in the names (ordered by name)", n);
        else std::snprintf(buf, sizeof(buf), "%d rasters, dates in %d of the names (ordered by name)", n, int(times.size()));
        return buf;
    }
    std::sort(times.begin(), times.end());
    CubeInfo fmt; // formatTime only needs these two
    fmt.timeIsDate = true;
    fmt.granularity = gran;
    if (n == 1) {
        std::snprintf(buf, sizeof(buf), "1 raster, %s", formatTime(fmt, times[0]).c_str());
        return buf;
    }
    std::vector<double> gaps;
    for (size_t i = 1; i < times.size(); ++i) gaps.push_back((times[i] - times[i - 1]) / 86400.0);
    std::nth_element(gaps.begin(), gaps.begin() + gaps.size() / 2, gaps.end());
    const double gap = gaps[gaps.size() / 2];
    std::string every;
    if (gap >= 360 && gap <= 370) every = "yearly";
    else if (gap >= 28 && gap <= 31) every = "monthly";
    else if (gap >= 0.5 && gap <= 1.5) every = "daily";
    else if (gap > 0) every = "every ~" + std::to_string(int(std::lround(gap))) + " days";
    else every = "several files share a date";
    std::snprintf(buf, sizeof(buf), "%d rasters, %s to %s, %s", n, formatTime(fmt, times.front()).c_str(),
                  formatTime(fmt, times.back()).c_str(), every.c_str());
    return buf;
}

std::string formatTime(const CubeInfo& info, double t) {
    char buf[64];
    if (info.spectral) {
        std::snprintf(buf, sizeof(buf), "%.1f nm", t);
        return buf;
    }
    if (!info.timeIsDate) {
        std::snprintf(buf, sizeof(buf), "%d", int(std::lround(t)) + 1);
        return buf;
    }
    int y;
    unsigned m, d;
    civilFromDays(int64_t(std::floor(t / 86400.0)), y, m, d);
    switch (info.granularity) {
    case TimeGranularity::Year: std::snprintf(buf, sizeof(buf), "%04d", y); break;
    case TimeGranularity::Month: std::snprintf(buf, sizeof(buf), "%04d-%02u", y, m); break;
    case TimeGranularity::Hour: {
        const int64_t s = int64_t(std::floor(t)) - int64_t(std::floor(t / 86400.0)) * 86400;
        std::snprintf(buf, sizeof(buf), "%04d-%02u-%02u %02d:%02d", y, m, d, int(s / 3600), int(s / 60 % 60));
        break;
    }
    default: std::snprintf(buf, sizeof(buf), "%04d-%02u-%02u", y, m, d); break;
    }
    return buf;
}

double CubeInfo::yearsFromStart(int t) const {
    if (!timeIsDate) return double(t);
    return (layers[t].time - layers[0].time) / (365.2425 * 86400.0);
}

static bool isLeap(int y) { return (y % 4 == 0 && y % 100 != 0) || y % 400 == 0; }

double CubeInfo::decimalYear(int t) const {
    if (!timeIsDate) return double(t);
    const int64_t days = int64_t(std::floor(layers[t].time / 86400.0));
    int y;
    unsigned m, d;
    civilFromDays(days, y, m, d);
    return y + double(days - daysFromCivil(y, 1, 1)) / (isLeap(y) ? 366.0 : 365.0);
}

double CubeInfo::xFromDecimalYear(double yr) const {
    if (!timeIsDate) return yr;
    const int y = int(std::floor(yr));
    const double days = double(daysFromCivil(y, 1, 1)) + (yr - y) * (isLeap(y) ? 366.0 : 365.0);
    return days * 86400.0;
}

int64_t CubeInfo::unixDay(int t) const { return int64_t(std::floor(layers[t].time / 86400.0)); }

int CubeInfo::nearestLayer(double time) const {
    int best = 0;
    for (int i = 1; i < T(); ++i)
        if (std::fabs(layers[i].time - time) < std::fabs(layers[best].time - time)) best = i;
    return best;
}

std::vector<std::pair<int, int>> CubeInfo::plotRuns() const {
    if (!spectral || T() < 3) return {{0, T()}};
    // A step over 3 times the median spacing is a gap without bands.
    std::vector<double> steps;
    for (int t = 1; t < T(); ++t) steps.push_back(layers[t].time - layers[t - 1].time);
    std::vector<double> sorted = steps;
    std::nth_element(sorted.begin(), sorted.begin() + sorted.size() / 2, sorted.end());
    const double limit = 3 * sorted[sorted.size() / 2];
    std::vector<std::pair<int, int>> runs;
    int begin = 0;
    for (int t = 1; t < T(); ++t)
        if (steps[t - 1] > limit) {
            runs.push_back({begin, t});
            begin = t;
        }
    runs.push_back({begin, T()});
    return runs;
}

bool sameAxis(const CubeInfo& a, const CubeInfo& b) { return a.timeIsDate == b.timeIsDate && a.spectral == b.spectral; }

bool matchableAxis(const CubeInfo& a, const CubeInfo& b) {
    return (a.timeIsDate && b.timeIsDate) || (a.spectral && b.spectral);
}

const std::vector<SpectralComposite>& spectralComposites() {
    static const std::vector<SpectralComposite> c = {
        {"True colour (640, 550, 470 nm)", {640, 550, 470}},
        {"Colour infrared (860, 650, 550 nm)", {860, 650, 550}},
        {"SWIR (2200, 860, 650 nm)", {2200, 860, 650}},
        {"Agriculture (1650, 860, 470 nm)", {1650, 860, 470}},
    };
    return c;
}

bool spectralCompositeLayers(const CubeInfo& info, const SpectralComposite& c, std::array<int, 3>& rgb) {
    if (!info.spectral) return false;
    for (int k = 0; k < 3; ++k) {
        rgb[k] = info.nearestLayer(c.nm[k]);
        if (std::fabs(info.layers[rgb[k]].time - c.nm[k]) > 40) return false;
    }
    return true;
}

const std::vector<SpectralIndex>& spectralIndices() {
    // Broad indices accept a multispectral sensor's band (e.g. Landsat's SWIR1 at
    // 1609 nm for 1650); PRI needs the narrow bands it is defined on.
    static const std::vector<SpectralIndex> s = {
        {"NDVI", "vegetation (NIR 860, red 650 nm)", 860, 650, 50},
        {"NDRE", "red edge, chlorophyll (790, 720 nm)", 790, 720, 15},
        {"NDWI", "open water (green 560, NIR 860 nm)", 560, 860, 50},
        {"NDMI", "vegetation moisture (NIR 860, SWIR 1650 nm)", 860, 1650, 60},
        {"NBR", "burn severity (NIR 860, SWIR 2200 nm)", 860, 2200, 60},
        {"NDSI", "snow (green 560, SWIR 1650 nm)", 560, 1650, 60},
        {"PRI", "photosynthesis, light use (531, 570 nm)", 531, 570, 6},
    };
    return s;
}

bool spectralIndexBands(const std::vector<double>& bandNm, const SpectralIndex& s, int& a, int& b) {
    auto nearest = [&](double nm, int& band) {
        band = 0;
        for (int i = 0; i < int(bandNm.size()); ++i)
            if (band == 0 || std::fabs(bandNm[i] - nm) < std::fabs(bandNm[band - 1] - nm)) band = i + 1;
        return band > 0 && std::fabs(bandNm[band - 1] - nm) <= s.tolerance;
    };
    return nearest(s.a, a) && nearest(s.b, b) && a != b;
}

// ---------------------------------------------------------------------------
// Wavelengths
// ---------------------------------------------------------------------------

// Factor to nanometres of a unit name ("nm", "Micrometers"...); 0 if unknown.
static double toNanometres(std::string u) {
    std::transform(u.begin(), u.end(), u.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    if (u == "nm" || u.rfind("nanomet", 0) == 0) return 1;
    if (u == "um" || u == "\xC2\xB5m" || u.rfind("micromet", 0) == 0 || u.rfind("micron", 0) == 0) return 1000;
    return 0;
}

// EnMAP's product metadata next to its spectral image (the GeoTIFF delivery
// has no wavelengths of its own): <bandCharacterisation> <bandID number="n">
// <wavelengthCenterOfBand>.
static std::vector<double> enmapWavelengths(const std::string& path, int bands) {
    const fs::path p = fs::u8path(path);
    std::string name = p.filename().u8string(), upper = name;
    std::transform(upper.begin(), upper.end(), upper.begin(), [](unsigned char c) { return char(std::toupper(c)); });
    const size_t at = upper.rfind("SPECTRAL_IMAGE");
    if (at == std::string::npos) return {};
    // Through GDAL: product paths easily pass Windows' 260 characters.
    const std::string meta = (p.parent_path() / fs::u8path(name.substr(0, at) + "METADATA.XML")).u8string();
    GByte* buf = nullptr;
    vsi_l_offset size = 0;
    if (!VSIIngestFile(nullptr, meta.c_str(), &buf, &size, 64 << 20)) return {};
    const std::string xml(reinterpret_cast<const char*>(buf), size_t(size));
    VSIFree(buf);
    const size_t begin = xml.find("<bandCharacterisation>"), end = xml.find("</bandCharacterisation>");
    if (begin == std::string::npos || end == std::string::npos) return {};
    std::vector<double> nm(size_t(bands), 0.0);
    const std::string idTag = "<bandID number=\"", wlTag = "<wavelengthCenterOfBand>";
    for (size_t i = xml.find(idTag, begin); i < end; i = xml.find(idTag, i + 1)) {
        const int b = std::atoi(xml.c_str() + i + idTag.size());
        const size_t w = xml.find(wlTag, i);
        if (b >= 1 && b <= bands && w < end) nm[size_t(b - 1)] = std::atof(xml.c_str() + w + wlTag.size());
    }
    return nm;
}

// The wavelength (nm) of every band of a file, from the band metadata (ENVI's
// "wavelength" and "wavelength_units"), GDAL's IMAGERY domain
// (CENTRAL_WAVELENGTH_UM) or EnMAP's METADATA.XML; empty unless each band has
// one, at least 3 and all different.
static std::vector<double> bandWavelengths(GDALDataset* ds, const std::string& path) {
    const int n = ds->GetRasterCount();
    if (n < 3) return {};
    const char* dsUnits = ds->GetMetadataItem("wavelength_units");
    std::vector<double> nm(size_t(n), 0.0);
    bool all = true;
    for (int b = 1; b <= n && all; ++b) {
        GDALRasterBand* rb = ds->GetRasterBand(b);
        double v = 0;
        if (const char* w = rb->GetMetadataItem("wavelength")) {
            const char* u = rb->GetMetadataItem("wavelength_units");
            v = std::atof(w);
            double f = toNanometres(u ? u : dsUnits ? dsUnits : "");
            if (f == 0) f = v < 30 ? 1000 : 1; // no unit: micrometres are small numbers
            v *= f;
        } else if (const char* um = rb->GetMetadataItem("CENTRAL_WAVELENGTH_UM", "IMAGERY")) {
            v = std::atof(um) * 1000;
        }
        nm[size_t(b - 1)] = v;
        all = v > 0;
    }
    if (!all) nm = enmapWavelengths(path, n);
    if (nm.empty() || std::any_of(nm.begin(), nm.end(), [](double v) { return !(v > 0); })) return {};
    std::vector<double> sorted = nm;
    std::sort(sorted.begin(), sorted.end());
    if (std::adjacent_find(sorted.begin(), sorted.end()) != sorted.end()) return {}; // e.g. one band at several dates
    return nm;
}

// The bands' descriptions ("Band N" without one), with the wavelength when
// they have one and the description does not say it.
static std::vector<std::string> bandNamesWithNm(GDALDataset* ds, const std::vector<double>& nm) {
    std::vector<std::string> names;
    for (int b = 1; b <= ds->GetRasterCount(); ++b) {
        const char* d = ds->GetRasterBand(b)->GetDescription();
        std::string n = d && d[0] ? std::string(d) : "Band " + std::to_string(b);
        if (size_t(b) <= nm.size() && n.find("nm") == std::string::npos) {
            char w[32];
            std::snprintf(w, sizeof(w), " (%.1f nm)", nm[size_t(b - 1)]);
            n += w;
        }
        names.push_back(n);
    }
    return names;
}

// ---------------------------------------------------------------------------
// Bands per date
// ---------------------------------------------------------------------------

const char* qaRuleId(QaRule r) {
    switch (r) {
    case QaRule::Fmask: return "fmask";
    case QaRule::LandsatC2: return "landsat_c2";
    case QaRule::NonZero: return "nonzero";
    default: return "none";
    }
}

const char* qaRuleLabel(QaRule r) {
    switch (r) {
    case QaRule::Fmask: return "Fmask codes (0 clear, 1 water)";
    case QaRule::LandsatC2: return "Landsat C2 QA_PIXEL bits";
    case QaRule::NonZero: return "Mask (0 = invalid)";
    default: return "None";
    }
}

bool qaUsable(QaRule r, uint32_t code) {
    switch (r) {
    case QaRule::Fmask: return code == 0 || code == 1;
    // Bits 0 fill, 1 dilated cloud, 3 cloud, 4 cloud shadow, 5 snow.
    case QaRule::LandsatC2: return (code & 0x3Bu) == 0;
    case QaRule::NonZero: return code != 0;
    default: return true;
    }
}

std::string CubeInfo::selectionText() const {
    if (bandsPerDate <= 1) return "";
    auto name = [&](int b) {
        return b >= 1 && b <= int(bandNames.size()) ? bandNames[b - 1] : "band " + std::to_string(b);
    };
    std::string s = sel.ndBand > 0 ? "ND(" + name(sel.band) + ", " + name(sel.ndBand) + ")" : name(sel.band);
    if (sel.qaBand > 0 && sel.qaRule != QaRule::None) s += ", QA " + name(sel.qaBand);
    return s;
}

static std::string xmlEscape(const std::string& s) {
    std::string out;
    for (char c : s) {
        switch (c) {
        case '&': out += "&amp;"; break;
        case '<': out += "&lt;"; break;
        case '>': out += "&gt;"; break;
        case '"': out += "&quot;"; break;
        default: out += c;
        }
    }
    return out;
}

static BandMeta readBandMeta(GDALRasterBand* b);

bool writeCubeVrt(const CubeInfo& info, const std::string& path, std::string& error, int sourceBand, bool raw) {
    std::string srsWkt;
    if (GDALDataset* ds = openRaster(info.firstPath)) {
        if (const OGRSpatialReference* srs = ds->GetSpatialRef()) {
            char* wkt = nullptr;
            srs->exportToWkt(&wkt);
            if (wkt) srsWkt = wkt;
            CPLFree(wkt);
        }
        GDALClose(ds);
    }
    if (srsWkt.empty()) srsWkt = info.crsWkt; // assumed (lon/lat NetCDF without a grid mapping)
    char num[64];
    std::string x = "<VRTDataset rasterXSize=\"" + std::to_string(info.width) + "\" rasterYSize=\"" +
                    std::to_string(info.height) + "\">\n";
    if (!srsWkt.empty()) x += "  <SRS>" + xmlEscape(srsWkt) + "</SRS>\n";
    if (info.hasGeoTransform) {
        char gt[256];
        std::snprintf(gt, sizeof(gt), "%.17g, %.17g, %.17g, %.17g, %.17g, %.17g", info.geoTransform[0],
                      info.geoTransform[1], info.geoTransform[2], info.geoTransform[3], info.geoTransform[4],
                      info.geoTransform[5]);
        x += std::string("  <GeoTransform>") + gt + "</GeoTransform>\n";
    }
    for (int t = 0; t < info.T(); ++t) {
        const Layer& L = info.layers[t];
        int srcBand = L.band;
        BandMeta m = L.meta;
        if (sourceBand > 0 && sourceBand + L.bandBase != L.band) { // sourceBand: a band of the date
            srcBand = sourceBand + L.bandBase;
            if (sourceBand == info.sel.ndBand) {
                m = L.ndMeta;
            } else if (GDALDataset* ds = openRaster(L.path)) {
                m = srcBand <= ds->GetRasterCount() ? readBandMeta(ds->GetRasterBand(srcBand)) : BandMeta{};
                GDALClose(ds);
            }
        }
        if (raw) m = BandMeta{};
        const bool scaled = m.scale != 1.0 || m.offset != 0.0;
        const char* source = scaled || m.hasNoData ? "ComplexSource" : "SimpleSource";
        x += "  <VRTRasterBand dataType=\"Float32\" band=\"" + std::to_string(t + 1) + "\">\n";
        x += "    <Description>" + xmlEscape(L.label) + "</Description>\n";
        if (!raw) x += "    <NoDataValue>nan</NoDataValue>\n";
        x += std::string("    <") + source + ">\n";
        x += "      <SourceFilename relativeToVRT=\"0\">" + xmlEscape(L.path) + "</SourceFilename>\n";
        x += "      <SourceBand>" + std::to_string(srcBand) + "</SourceBand>\n";
        if (scaled) {
            std::snprintf(num, sizeof(num), "%.17g", m.offset);
            x += std::string("      <ScaleOffset>") + num + "</ScaleOffset>\n";
            std::snprintf(num, sizeof(num), "%.17g", m.scale);
            x += std::string("      <ScaleRatio>") + num + "</ScaleRatio>\n";
        }
        if (m.hasNoData) {
            std::snprintf(num, sizeof(num), "%.17g", m.noData);
            x += std::string("      <NODATA>") + num + "</NODATA>\n";
        }
        x += std::string("    </") + source + ">\n";
        x += "  </VRTRasterBand>\n";
    }
    x += "</VRTDataset>\n";

    FILE* f = nullptr;
#ifdef _WIN32
    _wfopen_s(&f, fs::u8path(path).wstring().c_str(), L"wb");
#else
    f = std::fopen(path.c_str(), "wb");
#endif
    if (!f) {
        error = "could not write " + path;
        return false;
    }
    const bool ok = std::fwrite(x.data(), 1, x.size(), f) == x.size();
    std::fclose(f);
    if (!ok) error = "could not write " + path;
    return ok;
}

bool CubeInfo::pixelToGeo(double px, double py, double& gx, double& gy) const {
    if (!hasGeoTransform) return false;
    const auto& g = geoTransform;
    gx = g[0] + px * g[1] + py * g[2];
    gy = g[3] + px * g[4] + py * g[5];
    return true;
}

bool CubeInfo::geoToPixel(double gx, double gy, double& px, double& py) const {
    const auto& g = geoTransform;
    const double det = g[1] * g[5] - g[2] * g[4];
    if (!hasGeoTransform || det == 0) return false;
    const double dx = gx - g[0], dy = gy - g[3];
    px = (g[5] * dx - g[2] * dy) / det;
    py = (g[1] * dy - g[4] * dx) / det;
    return true;
}

// ---------------------------------------------------------------------------
// Layer discovery
// ---------------------------------------------------------------------------

static bool isRasterExt(const fs::path& p) {
    static const char* exts[] = {".tif", ".tiff", ".vrt", ".img", ".jp2", ".nc", ".hdf",
                                 ".h5", ".dat", ".bil", ".bsq", ".bip", ".asc", ".envi", ".grd"};
    std::string e = p.extension().u8string();
    std::transform(e.begin(), e.end(), e.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    for (const char* x : exts)
        if (e == x) return true;
    return false;
}

bool isRasterPath(const std::string& path) { return isRasterExt(fs::u8path(path)); }

bool isEnmapSpectralImage(const std::string& path) {
    std::string n = fs::u8path(path).filename().u8string();
    std::transform(n.begin(), n.end(), n.begin(), [](unsigned char c) { return char(std::toupper(c)); });
    return n.rfind("ENMAP", 0) == 0 && n.find("SPECTRAL_IMAGE") != std::string::npos && isRasterExt(fs::u8path(path));
}

static std::string lowerStr(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    return s;
}

static bool startsWith(const std::string& s, const char* prefix) { return s.rfind(prefix, 0) == 0; }

bool isRemotePath(const std::string& path) {
    const std::string p = lowerStr(datasetFile(path));
    for (const char* u : {"http://", "https://", "ftp://", "s3://", "gs://", "az://"})
        if (startsWith(p, u)) return true;
    for (const char* v : {"/vsicurl", "/vsis3", "/vsigs", "/vsiaz", "/vsiadls", "/vsioss", "/vsiswift", "/vsiwebhdfs"})
        if (p.find(v) != std::string::npos) return true;
    return false;
}

// A Zarr store: a folder named *.zarr or holding Zarr's metadata files.
static bool isZarrStore(const fs::path& p) {
    std::error_code ec;
    if (!fs::is_directory(p, ec)) return false;
    if (lowerStr(p.extension().u8string()) == ".zarr") return true;
    for (const char* f : {".zgroup", ".zarray", "zarr.json"})
        if (fs::exists(p / f, ec)) return true;
    return false;
}

std::string gdalPath(const std::string& input) {
    const std::string l = lowerStr(input);
    std::string p = input;
    if (startsWith(l, "http://") || startsWith(l, "https://") || startsWith(l, "ftp://")) p = "/vsicurl/" + input;
    else if (startsWith(l, "s3://")) p = "/vsis3/" + input.substr(5);
    else if (startsWith(l, "gs://")) p = "/vsigs/" + input.substr(5);
    else if (startsWith(l, "az://")) p = "/vsiaz/" + input.substr(5);
    else return input;
    // A remote Zarr store is a folder: GDAL needs to be told the driver.
    std::string bare = l.substr(0, l.find('?'));
    while (!bare.empty() && bare.back() == '/') bare.pop_back();
    if (bare.size() > 5 && bare.compare(bare.size() - 5, 5, ".zarr") == 0) p = "ZARR:\"" + p + "\"";
    return p;
}

std::string datasetFile(const std::string& path) {
    // DRIVER:"file":rest (NETCDF, ZARR, HDF5, HDF4_SDS...); a Windows drive
    // letter ("C:\...") has a single character before the colon.
    const size_t colon = path.find(':');
    if (colon != std::string::npos && colon > 1 && colon + 1 < path.size() && path[colon + 1] == '"') {
        const size_t end = path.find('"', colon + 2);
        if (end != std::string::npos) return path.substr(colon + 2, end - colon - 2);
    }
    return path;
}

std::string displayName(const std::string& path) {
    std::string file = datasetFile(path);
    std::string var; // what follows the file in a subdataset name
    if (file != path) {
        const size_t end = path.find('"', path.find(':') + 2);
        var = end + 1 < path.size() ? path.substr(end + 1) : "";
        while (!var.empty() && (var[0] == ':' || var[0] == '/')) var.erase(0, 1);
    }
    if (isRemotePath(file)) {
        file = file.substr(0, file.find('?')); // a signed URL's token
        while (!file.empty() && file.back() == '/') file.pop_back();
    }
    const size_t slash = file.find_last_of("/\\");
    std::string name = slash == std::string::npos ? file : file.substr(slash + 1);
    const size_t dot = name.find_last_of('.');
    if (dot != std::string::npos && dot > 0) name.erase(dot);
    if (name.empty()) name = file;
    return var.empty() ? name : name + " " + var;
}

GDALDataset* openRaster(const std::string& path) {
    if (!isRemotePath(path)) return GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
    // Thread-local: local files keep finding their side-cars (.ovr, .aux.xml, .hdr).
    const std::string old = CPLGetThreadLocalConfigOption("GDAL_DISABLE_READDIR_ON_OPEN", "");
    CPLSetThreadLocalConfigOption("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR");
    GDALDataset* ds = GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
    CPLSetThreadLocalConfigOption("GDAL_DISABLE_READDIR_ON_OPEN", old.empty() ? nullptr : old.c_str());
    return ds;
}

void configureRemoteAccess() {
    auto setDefault = [](const char* key, const char* value) {
        if (!CPLGetConfigOption(key, nullptr)) CPLSetConfigOption(key, value);
    };
    setDefault("GDAL_HTTP_MAX_RETRY", "3");
    setDefault("GDAL_HTTP_RETRY_DELAY", "1");
    setDefault("GDAL_HTTP_MERGE_CONSECUTIVE_RANGES", "YES");
    setDefault("CPL_VSIL_CURL_CACHE_SIZE", "268435456"); // 256 MB of downloaded blocks (default 16 MB)
    setDefault("CPL_VSIL_CURL_CHUNK_SIZE", "65536");
    // Public buckets (Landsat, Sentinel-2 COGs...) without an account: unsigned
    // requests unless credentials are configured.
    std::error_code ec;
    const std::string home = CPLGetConfigOption("USERPROFILE", CPLGetConfigOption("HOME", ""));
    const bool awsCreds = CPLGetConfigOption("AWS_ACCESS_KEY_ID", nullptr) || CPLGetConfigOption("AWS_PROFILE", nullptr) ||
                          CPLGetConfigOption("AWS_DEFAULT_PROFILE", nullptr) ||
                          (!home.empty() && (fs::exists(fs::u8path(home) / ".aws" / "credentials", ec) ||
                                             fs::exists(fs::u8path(home) / ".aws" / "config", ec)));
    if (!awsCreds) setDefault("AWS_NO_SIGN_REQUEST", "YES");
    const bool gsCreds = CPLGetConfigOption("GOOGLE_APPLICATION_CREDENTIALS", nullptr) ||
                         CPLGetConfigOption("GS_SECRET_ACCESS_KEY", nullptr) || CPLGetConfigOption("GS_OAUTH2_REFRESH_TOKEN", nullptr);
    if (!gsCreds) setDefault("GS_NO_SIGN_REQUEST", "YES");
    if (const std::string ca = curlCaBundle(); !ca.empty()) setDefault("CURL_CA_BUNDLE", ca.c_str());
}

static bool wildcardMatch(const char* pat, const char* s) {
    if (*pat == '\0') return *s == '\0';
    if (*pat == '*') return wildcardMatch(pat + 1, s) || (*s && wildcardMatch(pat, s + 1));
    if (*s && (*pat == '?' || std::tolower((unsigned char)*pat) == std::tolower((unsigned char)*s)))
        return wildcardMatch(pat + 1, s + 1);
    return false;
}

// "Natural" ordering: img2 < img10.
static bool naturalLess(const std::string& a, const std::string& b) {
    size_t i = 0, j = 0;
    while (i < a.size() && j < b.size()) {
        if (std::isdigit((unsigned char)a[i]) && std::isdigit((unsigned char)b[j])) {
            size_t i2 = i, j2 = j;
            while (i2 < a.size() && std::isdigit((unsigned char)a[i2])) ++i2;
            while (j2 < b.size() && std::isdigit((unsigned char)b[j2])) ++j2;
            std::string na = a.substr(i, i2 - i), nb = b.substr(j, j2 - j);
            na.erase(0, std::min(na.find_first_not_of('0'), na.size()));
            nb.erase(0, std::min(nb.find_first_not_of('0'), nb.size()));
            if (na.size() != nb.size()) return na.size() < nb.size();
            if (na != nb) return na < nb;
            i = i2;
            j = j2;
        } else {
            char ca = char(std::tolower((unsigned char)a[i])), cb = char(std::tolower((unsigned char)b[j]));
            if (ca != cb) return ca < cb;
            ++i;
            ++j;
        }
    }
    return a.size() - i < b.size() - j;
}

static std::vector<std::string> expandInputs(const std::vector<std::string>& inputs) {
    std::vector<std::string> files;
    for (const std::string& in : inputs) {
        // URLs, subdataset names and Zarr stores (folders) are one input each.
        if (isRemotePath(in) || datasetFile(in) != in) {
            files.push_back(gdalPath(in));
            continue;
        }
        fs::path p = fs::u8path(in);
        std::error_code ec;
        if (isZarrStore(p)) {
            files.push_back(in);
        } else if (fs::is_directory(p, ec)) {
            std::vector<std::string> dir;
            for (auto& e : fs::directory_iterator(p, ec))
                if (e.is_regular_file() && isRasterExt(e.path())) dir.push_back(e.path().u8string());
            // An EnMAP product folder: its spectral image, not the quality layers beside it.
            const auto spectral = std::find_if(dir.begin(), dir.end(), [](const std::string& f) { return isEnmapSpectralImage(f); });
            if (spectral != dir.end()) dir = {*spectral};
            // No rasters, but EnMAP products in subfolders: where a zip extracts to
            // (one product: its spectrum; several: a series of scenes).
            if (dir.empty()) {
                std::vector<std::string> products;
                for (auto& sub : fs::directory_iterator(p, ec))
                    if (sub.is_directory())
                        for (auto& e : fs::directory_iterator(sub.path(), ec))
                            if (e.is_regular_file() && isEnmapSpectralImage(e.path().u8string()))
                                products.push_back(e.path().u8string());
                dir = products;
            }
            std::sort(dir.begin(), dir.end(), naturalLess);
            files.insert(files.end(), dir.begin(), dir.end());
        } else if (in.find_first_of("*?") != std::string::npos) {
            fs::path parent = p.parent_path().empty() ? fs::current_path() : p.parent_path();
            std::string pat = p.filename().u8string();
            std::vector<std::string> dir;
            for (auto& e : fs::directory_iterator(parent, ec))
                if (e.is_regular_file() && wildcardMatch(pat.c_str(), e.path().filename().u8string().c_str()))
                    dir.push_back(e.path().u8string());
            std::sort(dir.begin(), dir.end(), naturalLess);
            files.insert(files.end(), dir.begin(), dir.end());
        } else {
            files.push_back(in);
        }
    }
    return files;
}

static BandMeta readBandMeta(GDALRasterBand* b) {
    BandMeta m;
    int ok = 0;
    m.noData = b->GetNoDataValue(&ok);
    m.hasNoData = ok != 0;
    m.scale = b->GetScale(&ok);
    if (!ok) m.scale = 1;
    m.offset = b->GetOffset(&ok);
    if (!ok) m.offset = 0;
    return m;
}

// Classes declared on a band (colour table, category names, attribute table).
static void readClassInfo(GDALRasterBand* b, CubeInfo& info) {
    if (const GDALColorTable* ct = b->GetColorTable()) {
        info.fileCategorical = true;
        for (int i = 0; i < ct->GetColorEntryCount() && i < 65536; ++i) {
            const GDALColorEntry* e = ct->GetColorEntry(i);
            info.classColors[i] = {(unsigned char)e->c1, (unsigned char)e->c2, (unsigned char)e->c3,
                                   (unsigned char)(e->c4 ? e->c4 : 255)};
        }
    }
    if (CSLConstList names = b->GetCategoryNames()) {
        for (int i = 0; names[i]; ++i)
            if (names[i][0]) info.classNames[i] = names[i];
        if (!info.classNames.empty()) info.fileCategorical = true;
    }
    if (GDALRasterAttributeTable* rat = b->GetDefaultRAT()) {
        const int value = rat->GetColOfUsage(GFU_MinMax) >= 0 ? rat->GetColOfUsage(GFU_MinMax) : rat->GetColOfUsage(GFU_Min);
        const int name = rat->GetColOfUsage(GFU_Name);
        const int r = rat->GetColOfUsage(GFU_Red), g = rat->GetColOfUsage(GFU_Green), bl = rat->GetColOfUsage(GFU_Blue);
        for (int row = 0; row < rat->GetRowCount() && row < 65536; ++row) {
            const int v = value >= 0 ? rat->GetValueAsInt(row, value) : row;
            if (name >= 0) {
                const char* n = rat->GetValueAsString(row, name);
                if (n && n[0]) info.classNames[v] = n;
            }
            if (r >= 0 && g >= 0 && bl >= 0)
                info.classColors[v] = {(unsigned char)rat->GetValueAsInt(row, r), (unsigned char)rat->GetValueAsInt(row, g),
                                       (unsigned char)rat->GetValueAsInt(row, bl), 255};
        }
        if (name >= 0 || r >= 0) info.fileCategorical = true;
    }
}

// Labels as precise as the dates need: years, months, days or hours.
static TimeGranularity granularityOf(const std::vector<Layer>& layers) {
    bool jan1 = true, first = true, midnight = true;
    for (const Layer& L : layers) {
        int y;
        unsigned m, d;
        const int64_t day = int64_t(std::floor(L.time / 86400.0));
        civilFromDays(day, y, m, d);
        midnight = midnight && std::fabs(L.time - double(day) * 86400.0) < 1;
        first = first && d == 1;
        jan1 = jan1 && d == 1 && m == 1;
    }
    return !midnight ? TimeGranularity::Hour
           : jan1    ? TimeGranularity::Year
           : first   ? TimeGranularity::Month
                     : TimeGranularity::Day;
}

std::shared_ptr<CubeInfo> openCube(const std::vector<std::string>& inputs, const BandSelection& sel,
                                   std::string& error) {
    static std::atomic<uint64_t> nextId{1};
    auto info = std::make_shared<CubeInfo>();
    info->id = nextId++;

    std::vector<std::string> files = expandInputs(inputs);
    if (files.empty()) {
        error = "No raster files found.";
        return nullptr;
    }

    auto openDs = [&](const std::string& f) -> GDALDataset* {
        CPLErrorReset();
        GDALDataset* ds = openRaster(f);
        if (!ds) {
            error = f + ": " + (CPLGetLastErrorMsg()[0] ? CPLGetLastErrorMsg() : "GDAL could not open the file");
            if (isRemotePath(f))
                error += "\n(remote: check the address and the connection; private buckets need credentials, "
                         "e.g. AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or a signed URL)";
        }
        return ds;
    };

    GDALDataset* first = openDs(files[0]);
    if (!first) return nullptr;
    info->firstPath = files[0];
    info->remote = isRemotePath(files[0]);
    info->width = first->GetRasterXSize();
    info->height = first->GetRasterYSize();
    if (first->GetRasterCount() == 0) {
        // A container of variables (NetCDF, Zarr, HDF): the interface asks
        // which one (listVariables); here only a lone one opens by itself.
        const std::vector<MdVariable> vars = files.size() == 1 ? listVariables(files[0]) : std::vector<MdVariable>{};
        GDALClose(first);
        if (vars.size() == 1) return openCube({vars[0].dataset}, sel, error);
        error = files[0] + ": no raster bands.";
        if (!vars.empty()) {
            error = files[0] + " holds " + std::to_string(vars.size()) + " variables; open one of them:";
            for (const MdVariable& v : vars) error += "\n  " + v.dataset;
        }
        return nullptr;
    }
    info->dataType = GDALGetDataTypeName(first->GetRasterBand(1)->GetRasterDataType());
    info->driver = first->GetDriverName();
    info->hasGeoTransform = first->GetGeoTransform(info->geoTransform.data()) == CE_None;
    if (const OGRSpatialReference* srs = first->GetSpatialRef()) {
        if (const char* n = srs->GetName()) info->crsName = n;
        const char* a = srs->GetAuthorityName(nullptr);
        const char* c = srs->GetAuthorityCode(nullptr);
        if (a && c) info->crsAuthority = std::string(a) + ":" + c;
        char* wkt = nullptr;
        const char* opts[] = {"FORMAT=WKT2_2019", nullptr};
        if (srs->exportToWkt(&wkt, opts) == OGRERR_NONE && wkt) info->crsWkt = wkt;
        CPLFree(wkt);
    }

    std::vector<std::string> timeTexts; // where to extract each layer's date from
    char desc[256];
    // One file of embeddings: a year with D bands, or zeit.save_raster's every
    // year in one file (bands "<date>_<dimension>", date after date).
    int packedDates = 0, packedDims = 0;
    std::vector<std::string> packedPrefix, packedNames;
    if (files.size() == 1 && first->GetRasterCount() >= 16) {
        const int n = first->GetRasterCount();
        const char* tag = first->GetMetadataItem("ZEIT_EMBEDDING");
        auto isDim = [](const std::string& s) {
            return s.size() >= 2 && s[0] == 'A' && std::all_of(s.begin() + 1, s.end(), [](char c) { return c >= '0' && c <= '9'; });
        };
        std::vector<std::string> prefix(n), name(n);
        bool dims = true, prefixed = true;
        for (int b = 1; b <= n; ++b) {
            const std::string d = first->GetRasterBand(b)->GetDescription();
            const size_t u = d.rfind('_');
            name[b - 1] = u == std::string::npos ? d : d.substr(u + 1);
            prefix[b - 1] = u == std::string::npos ? "" : d.substr(0, u);
            dims = dims && isDim(d);
            prefixed = prefixed && u != std::string::npos && isDim(name[b - 1]);
        }
        if (prefixed) { // dates in blocks of D bands with the same dimension names
            int D = 1;
            while (D < n && prefix[D] == prefix[0]) ++D;
            bool ok = n % D == 0;
            for (int b = 0; ok && b < n; ++b) ok = prefix[b] == prefix[b / D * D] && name[b] == name[b % D];
            if (ok && D >= 16) {
                packedDates = n / D;
                packedDims = D;
                for (int g = 0; g < packedDates; ++g) packedPrefix.push_back(prefix[size_t(g) * D]);
                packedNames.assign(name.begin(), name.begin() + D);
            }
        } else if (dims || tag) { // one year
            packedDates = 1;
            packedDims = n;
            for (int b = 1; b <= n; ++b) {
                const char* d = first->GetRasterBand(b)->GetDescription();
                packedNames.push_back(d && d[0] ? std::string(d) : "Band " + std::to_string(b));
            }
            packedPrefix.push_back(displayName(files[0]));
        }
        if (packedDates > 0 && tag) info->embeddingTag = tag;
    }
    if (packedDates > 0) {
        info->bandsPerDate = packedDims;
        info->bandNames = packedNames;
        info->sel = sel;
        info->sel.ndBand = std::min(info->sel.ndBand, packedDims);
        info->sel.qaBand = 0;
        info->sel.qaRule = QaRule::None;
        info->sel.band = std::clamp(info->sel.band, 1, packedDims);
        for (int g = 0; g < packedDates; ++g) {
            Layer L;
            L.path = files[0];
            L.bandBase = g * packedDims;
            L.band = L.bandBase + info->sel.band;
            L.meta = readBandMeta(first->GetRasterBand(L.band));
            if (info->sel.ndBand > 0) L.ndMeta = readBandMeta(first->GetRasterBand(L.bandBase + info->sel.ndBand));
            L.label = packedPrefix[g];
            timeTexts.push_back(packedPrefix[g]);
            info->layers.push_back(L);
        }
        std::snprintf(desc, sizeof(desc), "1 file, %d date%s x %d bands (embeddings)", packedDates,
                      packedDates > 1 ? "s" : "", packedDims);
        GDALClose(first);
    } else if (files.size() == 1) {
        // One file: each band is one date, or one wavelength of a spectrum.
        const int n = first->GetRasterCount();
        const std::vector<double> nm = bandWavelengths(first, files[0]);
        info->bandNm = nm;
        info->spectral = !nm.empty();
        // An index of a spectrum (picked in the Display panel): one map, the
        // normalized difference of two of its bands, dated by the file's name.
        if (info->spectral && sel.chosen && sel.ndBand > 0) {
            info->spectral = false;
            info->bandsPerDate = n;
            info->bandNames = bandNamesWithNm(first, nm);
            info->sel = sel;
            info->sel.band = std::clamp(sel.band, 1, n);
            info->sel.ndBand = std::clamp(sel.ndBand, 1, n);
            info->sel.qaBand = 0;
            info->sel.qaRule = QaRule::None;
            Layer L;
            L.path = files[0];
            L.band = info->sel.band;
            L.meta = readBandMeta(first->GetRasterBand(L.band));
            L.ndMeta = readBandMeta(first->GetRasterBand(info->sel.ndBand));
            L.label = displayName(files[0]);
            timeTexts.push_back(L.label);
            info->layers.push_back(L);
            GDALClose(first);
            std::snprintf(desc, sizeof(desc), "1 file, %s of a spectrum of %d bands", info->selectionText().c_str(), n);
            info->description = desc;
            double t = 0; // the scene's date, if its name has one
            if (const TimeGranularity g = parseDate(timeTexts[0], t); g != TimeGranularity::Index) {
                info->layers[0].time = t;
                info->timeIsDate = true;
                info->granularity = g;
                info->layers[0].label = formatTime(*info, t);
            }
            return info;
        }
        // A NetCDF / Zarr variable: bands placed by their coordinates (CF time),
        // one level of the other dimensions at a time.
        if (MdBands md; !info->spectral && readMdBands(first, files[0], md)) {
            info->multidim = true;
            const std::string var = displayName(files[0]), file = displayName(datasetFile(files[0]));
            info->mdVariable = var.size() > file.size() + 1 ? var.substr(file.size() + 1) : "";
            if (const char* v = first->GetRasterBand(1)->GetMetadataItem("NETCDF_VARNAME"); v && info->mdVariable.empty())
                info->mdVariable = v; // a file of one variable, opened by its name
            if (md.levels.size() > 1) info->mdLevels = md.levels;
            info->sel = BandSelection{};
            info->sel.band = std::clamp(sel.band, 1, int(md.levels.size()));
            info->sel.chosen = sel.chosen;
            for (int b = 1; b <= n; ++b) {
                if (md.level[size_t(b - 1)] != info->sel.band - 1) continue;
                GDALRasterBand* rb = first->GetRasterBand(b);
                Layer L;
                L.path = files[0];
                L.band = b;
                L.meta = readBandMeta(rb);
                if (info->layers.empty()) readClassInfo(rb, *info);
                L.time = md.time[size_t(b - 1)];
                if (!md.timeIsDate) L.label = md.timeDim + " " + std::to_string(int(info->layers.size()) + 1);
                info->layers.push_back(L);
            }
            std::stable_sort(info->layers.begin(), info->layers.end(),
                             [](const Layer& a, const Layer& b) { return a.time < b.time; });
            if (md.timeIsDate) {
                info->timeIsDate = true;
                info->granularity = granularityOf(info->layers);
                for (Layer& L : info->layers) L.label = formatTime(*info, L.time);
            } else {
                char v[48];
                for (size_t i = 0; i < info->layers.size(); ++i) { // the axis' coordinate, as an index
                    std::snprintf(v, sizeof(v), "%.6g", info->layers[i].time);
                    info->layers[i].label = md.timeDim + " " + v;
                    info->layers[i].time = double(i);
                }
            }
            // Lon/lat grids without a grid mapping (most climate files): WGS 84,
            // so basemaps, vectors and other layers line up.
            const auto& g = info->geoTransform;
            const double x1 = g[0] + g[1] * info->width, y1 = g[3] + g[5] * info->height;
            if (info->crsWkt.empty() && info->hasGeoTransform && std::min(g[0], x1) >= -180.5 &&
                std::max(g[0], x1) <= 360.5 && std::min(g[3], y1) >= -90.5 && std::max(g[3], y1) <= 90.5) {
                OGRSpatialReference wgs;
                wgs.importFromEPSG(4326);
                char* wkt = nullptr;
                const char* opts[] = {"FORMAT=WKT2_2019", nullptr};
                if (wgs.exportToWkt(&wkt, opts) == OGRERR_NONE && wkt) {
                    info->crsWkt = wkt;
                    info->crsName = "WGS 84 (assumed: lon/lat grid without a grid mapping)";
                    info->crsAuthority = "EPSG:4326";
                }
                CPLFree(wkt);
            }
            GDALClose(first);
            std::string d = std::string(info->driver) + " variable" + (info->mdVariable.empty() ? "" : " " + info->mdVariable) +
                            ", " + std::to_string(info->T()) + (md.timeIsDate ? " dates" : " steps of " + md.timeDim);
            if (!info->mdLevels.empty()) d += ", " + info->mdLevels[size_t(info->sel.band - 1)];
            if (!md.calendar.empty() && lowerStr(md.calendar) != "standard" && lowerStr(md.calendar) != "gregorian" &&
                lowerStr(md.calendar) != "proleptic_gregorian")
                d += " (" + md.calendar + " calendar)";
            info->description = d;
            if (info->layers.empty()) {
                error = files[0] + ": no bands for the chosen level.";
                return nullptr;
            }
            return info;
        }
        for (int b = 1; b <= n; ++b) {
            GDALRasterBand* rb = first->GetRasterBand(b);
            Layer L;
            L.path = files[0];
            L.band = b;
            L.meta = readBandMeta(rb);
            if (b == 1 && !info->spectral) readClassInfo(rb, *info);
            std::string t = rb->GetDescription();
            if (CSLConstList md = rb->GetMetadata())
                for (int i = 0; md[i]; ++i) t += std::string(" ") + md[i];
            L.label = rb->GetDescription()[0] ? rb->GetDescription() : "band " + std::to_string(b);
            if (info->spectral) L.time = nm[size_t(b - 1)];
            timeTexts.push_back(t);
            info->layers.push_back(L);
        }
        // Compressed with every band in each block: each band read decompresses
        // all of them (~70x slower on a 224-band EnMAP scene).
        const char* il = first->GetMetadataItem("INTERLEAVE", "IMAGE_STRUCTURE");
        const char* comp = first->GetMetadataItem("COMPRESSION", "IMAGE_STRUCTURE");
        const bool slowLayout = il && EQUAL(il, "PIXEL") && comp && n >= 16;
        GDALClose(first);
        if (info->spectral) {
            std::stable_sort(info->layers.begin(), info->layers.end(),
                             [](const Layer& a, const Layer& b) { return a.time < b.time; });
            for (Layer& L : info->layers) L.label = formatTime(*info, L.time);
            std::snprintf(desc, sizeof(desc), "1 file, %d bands, %.0f to %.0f nm (spectrum)%s", n,
                          info->layers.front().time, info->layers.back().time,
                          slowLayout ? ". Slow to read: compressed pixel-interleaved (gdal_translate "
                                       "-co INTERLEAVE=BAND makes it fast)"
                                     : "");
            info->description = desc;
            return info;
        }
        std::snprintf(desc, sizeof(desc), "1 file, %d bands (1 band = 1 date)", n);
    } else {
        info->bandsPerDate = first->GetRasterCount();
        info->bandNm = bandWavelengths(first, files[0]);
        info->bandNames = bandNamesWithNm(first, info->bandNm);
        info->sel = sel;
        // Bands with wavelengths (e.g. a series of EnMAP scenes): NDVI rather than
        // the first band (418 nm on EnMAP), unless a band was asked for.
        int ndviA = 0, ndviB = 0;
        if (!sel.chosen && sel.band == 1 && sel.ndBand == 0 &&
            spectralIndexBands(info->bandNm, spectralIndices()[0], ndviA, ndviB)) {
            info->sel.band = ndviA;
            info->sel.ndBand = ndviB;
        }
        // A quality band recognised by name masks clouds from the start (the
        // Display panel can turn it off).
        if (!sel.chosen && sel.qaBand == 0)
            for (int b = 1; b <= info->bandsPerDate; ++b) {
                std::string n = info->bandNames[b - 1];
                std::transform(n.begin(), n.end(), n.begin(), [](unsigned char c) { return char(std::tolower(c)); });
                const QaRule r = n.find("fmask") != std::string::npos      ? QaRule::Fmask
                                 : n.find("qa_pixel") != std::string::npos ? QaRule::LandsatC2
                                                                           : QaRule::None;
                if (r != QaRule::None) {
                    info->sel.qaBand = b;
                    info->sel.qaRule = r;
                    break;
                }
            }
        if (info->sel.qaBand <= 0) info->sel.qaRule = QaRule::None;
        if (info->sel.qaRule == QaRule::None) info->sel.qaBand = 0;
        if (const char* tag = first->GetMetadataItem("ZEIT_EMBEDDING")) info->embeddingTag = tag;
        GDALClose(first);
        // Remote files: each open is a few round trips (~1 s), so they are
        // opened 16 at a time; local ones as they are needed.
        std::vector<GDALDataset*> pre(files.size(), nullptr);
        if (info->remote && files.size() > 1) {
            std::vector<std::string> errs(files.size());
            for (size_t b = 0; b < files.size(); b += 16) {
                std::vector<std::future<void>> batch;
                for (size_t i = b; i < std::min(files.size(), b + 16); ++i)
                    batch.push_back(std::async(std::launch::async, [&, i] {
                        CPLErrorReset();
                        pre[i] = openRaster(files[i]);
                        if (!pre[i]) errs[i] = CPLGetLastErrorMsg();
                    }));
                for (auto& f : batch) f.get();
            }
            for (size_t i = 0; i < files.size(); ++i)
                if (!pre[i]) {
                    error = files[i] + ": " + (errs[i].empty() ? "GDAL could not open the file" : errs[i]);
                    for (GDALDataset* d : pre)
                        if (d) GDALClose(d);
                    return nullptr;
                }
        }
        for (size_t fi = 0; fi < files.size(); ++fi) {
            const std::string& f = files[fi];
            GDALDataset* ds = pre[fi] ? std::exchange(pre[fi], nullptr) : openDs(f);
            auto closeRest = [&] {
                for (GDALDataset* d : pre)
                    if (d) GDALClose(d);
            };
            if (!ds) {
                closeRest();
                return nullptr;
            }
            if (ds->GetRasterXSize() != info->width || ds->GetRasterYSize() != info->height) {
                error = fs::u8path(f).filename().u8string() + ": size " + std::to_string(ds->GetRasterXSize()) +
                        "x" + std::to_string(ds->GetRasterYSize()) + " differs from the 1st file (" +
                        std::to_string(info->width) + "x" + std::to_string(info->height) + ").";
                GDALClose(ds);
                closeRest();
                return nullptr;
            }
            const BandSelection& s = info->sel;
            const int need = std::max({s.band, s.ndBand, s.qaBand});
            if (need > ds->GetRasterCount()) {
                error = f + ": has no band " + std::to_string(need) + ".";
                GDALClose(ds);
                closeRest();
                return nullptr;
            }
            Layer L;
            L.path = f;
            L.band = s.band;
            L.meta = readBandMeta(ds->GetRasterBand(s.band));
            if (s.ndBand > 0) L.ndMeta = readBandMeta(ds->GetRasterBand(s.ndBand));
            else if (info->layers.empty()) readClassInfo(ds->GetRasterBand(s.band), *info);
            L.label = displayName(f);
            timeTexts.push_back(L.label);
            info->layers.push_back(L);
            GDALClose(ds);
        }
        std::snprintf(desc, sizeof(desc), "%d files (%s of each)", int(files.size()),
                      info->bandsPerDate > 1 ? info->selectionText().c_str() : "band 1");
    }
    info->description = desc;

    // Dates: only used if every layer has a recognizable date. Files in a
    // folder each, with the date on the folder (Sentinel-2 COGs on AWS:
    // .../S2B_23KPQ_20230811_0_L2A/B04.tif), take their folder's.
    std::vector<std::string> folders;
    for (const Layer& L : info->layers) {
        std::string f = datasetFile(L.path);
        f = f.substr(0, f.find('?'));
        const size_t end = f.find_last_of("/\\");
        const size_t start = end == std::string::npos ? std::string::npos : f.find_last_of("/\\", end - 1);
        folders.push_back(end == std::string::npos ? "" : f.substr(start == std::string::npos ? 0 : start + 1, end - start - 1));
    }
    std::vector<std::string> distinct = folders;
    std::sort(distinct.begin(), distinct.end());
    const bool folderPerFile = files.size() > 1 && std::unique(distinct.begin(), distinct.end()) == distinct.end();
    bool allDates = true;
    TimeGranularity gran = TimeGranularity::Year;
    std::vector<double> times(info->layers.size());
    for (size_t i = 0; i < info->layers.size() && allDates; ++i) {
        TimeGranularity g = parseDate(timeTexts[i], times[i]);
        if (g == TimeGranularity::Index && folderPerFile && i < folders.size()) g = parseDate(folders[i], times[i]);
        if (g == TimeGranularity::Index) allDates = false;
        gran = std::max(gran, g);
    }
    if (allDates) {
        for (size_t i = 0; i < times.size(); ++i) info->layers[i].time = times[i];
        std::stable_sort(info->layers.begin(), info->layers.end(),
                         [](const Layer& a, const Layer& b) { return a.time < b.time; });
        info->timeIsDate = true;
        info->granularity = gran;
        for (auto& L : info->layers) L.label = formatTime(*info, L.time);
    } else {
        for (size_t i = 0; i < info->layers.size(); ++i) info->layers[i].time = double(i);
    }
    return info;
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

CubeReader::CubeReader(std::shared_ptr<const CubeInfo> info) : info_(std::move(info)) {}

CubeReader::~CubeReader() {
    for (auto& [_, ds] : ds_)
        if (ds) GDALClose(ds);
}

GDALRasterBand* CubeReader::band(int layer, int b) {
    const Layer& L = info_->layers[layer];
    auto it = ds_.find(L.path);
    if (it == ds_.end())
        it = ds_.emplace(L.path, openRaster(L.path)).first;
    if (!it->second) return nullptr;
    b = b <= 0 ? L.band : b + L.bandBase; // b: a band of the date (1..bandsPerDate)
    return b <= it->second->GetRasterCount() ? it->second->GetRasterBand(b) : nullptr;
}

static void postProcess(const BandMeta& m, float* v, size_t n) {
    const float nd = float(m.noData);
    const bool scaled = m.scale != 1.0 || m.offset != 0.0;
    const float s = float(m.scale), o = float(m.offset);
    for (size_t i = 0; i < n; ++i) {
        if (m.hasNoData && (v[i] == nd || (std::isnan(nd) && std::isnan(v[i])))) v[i] = NAN;
        else if (std::isinf(v[i])) v[i] = NAN;
        else if (scaled) v[i] = v[i] * s + o;
    }
}

bool CubeReader::readRaw(int layer, int b, int x, int y, int w, int h, float* buf, int bw, int bh) {
    GDALRasterBand* rb = band(layer, b);
    if (!rb) return false;
    GDALRasterIOExtraArg extra;
    INIT_RASTERIO_EXTRA_ARG(extra);
    extra.eResampleAlg = GRIORA_NearestNeighbour;
    return rb->RasterIO(GF_Read, x, y, w, h, buf, bw, bh, GDT_Float32, 0, 0, &extra) == CE_None;
}

// The shown quantity: the band, or the normalized difference of two bands,
// with unusable observations (QA band) as NaN.
bool CubeReader::readWindow(int layer, int x, int y, int w, int h, float* buf, int bw, int bh) {
    const Layer& L = info_->layers[layer];
    const BandSelection& sel = info_->sel;
    const size_t n = size_t(bw) * bh;
    if (!readRaw(layer, 0, x, y, w, h, buf, bw, bh)) return false; // 0: the shown band
    postProcess(L.meta, buf, n);
    if (sel.ndBand > 0) {
        tmp_.resize(n);
        if (!readRaw(layer, sel.ndBand, x, y, w, h, tmp_.data(), bw, bh)) return false;
        postProcess(L.ndMeta, tmp_.data(), n);
        for (size_t i = 0; i < n; ++i) {
            const float a = buf[i], b = tmp_[i], s = a + b;
            buf[i] = s != 0 ? (a - b) / s : NAN; // NaN in either input stays NaN
        }
    }
    if (sel.qaBand > 0 && sel.qaRule != QaRule::None) {
        tmp_.resize(n);
        if (!readRaw(layer, sel.qaBand, x, y, w, h, tmp_.data(), bw, bh)) return false;
        for (size_t i = 0; i < n; ++i) {
            const float q = tmp_[i];
            if (!(q >= 0) || !qaUsable(sel.qaRule, uint32_t(q))) buf[i] = NAN;
        }
    }
    return true;
}

bool CubeReader::readPixel(int layer, int x, int y, float& value) {
    return readWindow(layer, x, y, 1, 1, &value, 1, 1);
}

bool CubeReader::readBandPixel(int layer, int b, int x, int y, float& value, bool raw) {
    GDALRasterBand* rb = band(layer, b);
    if (!rb || !readRaw(layer, b, x, y, 1, 1, &value, 1, 1)) return false;
    if (!raw) postProcess(readBandMeta(rb), &value, 1);
    return true;
}

bool CubeReader::readBandWindow(int layer, int b, int x, int y, int w, int h, float* buf, int bw, int bh) {
    GDALRasterBand* rb = band(layer, b);
    if (!rb || !readRaw(layer, b, x, y, w, h, buf, bw, bh)) return false;
    postProcess(readBandMeta(rb), buf, size_t(bw) * bh);
    return true;
}

CubeReader& threadReader(const std::shared_ptr<const CubeInfo>& info) {
    thread_local std::unique_ptr<CubeReader> reader;
    if (!reader || reader->cubeId() != info->id) reader = std::make_unique<CubeReader>(info);
    return *reader;
}
