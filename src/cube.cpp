#include "cube.hpp"

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <regex>

#include <cpl_error.h>
#include <gdal_priv.h>
#include <ogr_spatialref.h>

namespace fs = std::filesystem;

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

// Days since 1970-01-01 (Howard Hinnant's algorithm), without relying on timegm.
static int64_t daysFromCivil(int y, unsigned m, unsigned d) {
    y -= m <= 2;
    const int64_t era = (y >= 0 ? y : y - 399) / 400;
    const unsigned yoe = unsigned(y - era * 400);
    const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + int64_t(doe) - 719468;
}

static void civilFromDays(int64_t z, int& y, unsigned& m, unsigned& d) {
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

std::string formatTime(const CubeInfo& info, double t) {
    char buf[64];
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
    default: std::snprintf(buf, sizeof(buf), "%04d-%02u-%02u", y, m, d); break;
    }
    return buf;
}

double CubeInfo::yearsFromStart(int t) const {
    if (!timeIsDate) return double(t);
    return (layers[t].time - layers[0].time) / (365.2425 * 86400.0);
}

bool CubeInfo::pixelToGeo(double px, double py, double& gx, double& gy) const {
    if (!hasGeoTransform) return false;
    const auto& g = geoTransform;
    gx = g[0] + px * g[1] + py * g[2];
    gy = g[3] + px * g[4] + py * g[5];
    return true;
}

// ---------------------------------------------------------------------------
// Layer discovery
// ---------------------------------------------------------------------------

static bool isRasterExt(const fs::path& p) {
    static const char* exts[] = {".tif", ".tiff", ".vrt", ".img", ".jp2", ".nc", ".hdf",
                                 ".h5", ".dat", ".bil", ".asc", ".envi", ".grd"};
    std::string e = p.extension().u8string();
    std::transform(e.begin(), e.end(), e.begin(), [](unsigned char c) { return char(std::tolower(c)); });
    for (const char* x : exts)
        if (e == x) return true;
    return false;
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
        fs::path p = fs::u8path(in);
        std::error_code ec;
        if (fs::is_directory(p, ec)) {
            std::vector<std::string> dir;
            for (auto& e : fs::directory_iterator(p, ec))
                if (e.is_regular_file() && isRasterExt(e.path())) dir.push_back(e.path().u8string());
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

static void readBandMeta(GDALRasterBand* b, Layer& L) {
    int ok = 0;
    L.noData = b->GetNoDataValue(&ok);
    L.hasNoData = ok != 0;
    L.scale = b->GetScale(&ok);
    if (!ok) L.scale = 1;
    L.offset = b->GetOffset(&ok);
    if (!ok) L.offset = 0;
}

std::shared_ptr<CubeInfo> openCube(const std::vector<std::string>& inputs, int band, std::string& error) {
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
        GDALDataset* ds = GDALDataset::Open(f.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds) {
            error = f + ": " + (CPLGetLastErrorMsg()[0] ? CPLGetLastErrorMsg() : "GDAL could not open the file");
        }
        return ds;
    };

    GDALDataset* first = openDs(files[0]);
    if (!first) return nullptr;
    info->firstPath = files[0];
    info->width = first->GetRasterXSize();
    info->height = first->GetRasterYSize();
    if (first->GetRasterCount() == 0) {
        error = files[0] + ": no raster bands.";
        GDALClose(first);
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
    }

    std::vector<std::string> timeTexts; // where to extract each layer's date from
    char desc[256];
    if (files.size() == 1) {
        // One file: each band is one date.
        const int n = first->GetRasterCount();
        for (int b = 1; b <= n; ++b) {
            GDALRasterBand* rb = first->GetRasterBand(b);
            Layer L;
            L.path = files[0];
            L.band = b;
            readBandMeta(rb, L);
            std::string t = rb->GetDescription();
            if (char** md = rb->GetMetadata())
                for (int i = 0; md[i]; ++i) t += std::string(" ") + md[i];
            L.label = rb->GetDescription()[0] ? rb->GetDescription() : "band " + std::to_string(b);
            timeTexts.push_back(t);
            info->layers.push_back(L);
        }
        std::snprintf(desc, sizeof(desc), "1 file, %d bands (1 band = 1 date)", n);
        GDALClose(first);
    } else {
        GDALClose(first);
        for (const std::string& f : files) {
            GDALDataset* ds = openDs(f);
            if (!ds) return nullptr;
            if (ds->GetRasterXSize() != info->width || ds->GetRasterYSize() != info->height) {
                error = fs::u8path(f).filename().u8string() + ": size " + std::to_string(ds->GetRasterXSize()) +
                        "x" + std::to_string(ds->GetRasterYSize()) + " differs from the 1st file (" +
                        std::to_string(info->width) + "x" + std::to_string(info->height) + ").";
                GDALClose(ds);
                return nullptr;
            }
            if (band > ds->GetRasterCount()) {
                error = f + ": has no band " + std::to_string(band) + ".";
                GDALClose(ds);
                return nullptr;
            }
            Layer L;
            L.path = f;
            L.band = band;
            readBandMeta(ds->GetRasterBand(band), L);
            L.label = fs::u8path(f).stem().u8string();
            timeTexts.push_back(L.label);
            info->layers.push_back(L);
            GDALClose(ds);
        }
        std::snprintf(desc, sizeof(desc), "%d files (band %d of each)", int(files.size()), band);
    }
    info->description = desc;

    // Dates: only used if every layer has a recognizable date.
    bool allDates = true;
    TimeGranularity gran = TimeGranularity::Year;
    std::vector<double> times(info->layers.size());
    for (size_t i = 0; i < info->layers.size() && allDates; ++i) {
        TimeGranularity g = parseDate(timeTexts[i], times[i]);
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

GDALRasterBand* CubeReader::band(int layer) {
    const Layer& L = info_->layers[layer];
    auto it = ds_.find(L.path);
    if (it == ds_.end())
        it = ds_.emplace(L.path, GDALDataset::Open(L.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY)).first;
    return it->second ? it->second->GetRasterBand(L.band) : nullptr;
}

static void postProcess(const Layer& L, float* v, size_t n) {
    const float nd = float(L.noData);
    const bool scaled = L.scale != 1.0 || L.offset != 0.0;
    const float s = float(L.scale), o = float(L.offset);
    for (size_t i = 0; i < n; ++i) {
        if (L.hasNoData && (v[i] == nd || (std::isnan(nd) && std::isnan(v[i])))) v[i] = NAN;
        else if (std::isinf(v[i])) v[i] = NAN;
        else if (scaled) v[i] = v[i] * s + o;
    }
}

bool CubeReader::readWindow(int layer, int x, int y, int w, int h, float* buf, int bw, int bh) {
    GDALRasterBand* b = band(layer);
    if (!b) return false;
    GDALRasterIOExtraArg extra;
    INIT_RASTERIO_EXTRA_ARG(extra);
    extra.eResampleAlg = GRIORA_NearestNeighbour;
    if (b->RasterIO(GF_Read, x, y, w, h, buf, bw, bh, GDT_Float32, 0, 0, &extra) != CE_None) return false;
    postProcess(info_->layers[layer], buf, size_t(bw) * bh);
    return true;
}

bool CubeReader::readPixel(int layer, int x, int y, float& value) {
    return readWindow(layer, x, y, 1, 1, &value, 1, 1);
}

CubeReader& threadReader(const std::shared_ptr<const CubeInfo>& info) {
    thread_local std::unique_ptr<CubeReader> reader;
    if (!reader || reader->cubeId() != info->id) reader = std::make_unique<CubeReader>(info);
    return *reader;
}
