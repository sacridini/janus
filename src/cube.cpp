#include "cube.hpp"

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <regex>

#include <cpl_conv.h>
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
    if (GDALDataset* ds = GDALDataset::Open(info.firstPath.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY)) {
        if (const OGRSpatialReference* srs = ds->GetSpatialRef()) {
            char* wkt = nullptr;
            srs->exportToWkt(&wkt);
            if (wkt) srsWkt = wkt;
            CPLFree(wkt);
        }
        GDALClose(ds);
    }
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
        if (sourceBand > 0 && sourceBand != L.band) {
            srcBand = sourceBand;
            if (sourceBand == info.sel.ndBand) {
                m = L.ndMeta;
            } else if (GDALDataset* ds = GDALDataset::Open(L.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY)) {
                m = sourceBand <= ds->GetRasterCount() ? readBandMeta(ds->GetRasterBand(sourceBand)) : BandMeta{};
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

bool isRasterPath(const std::string& path) { return isRasterExt(fs::u8path(path)); }

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
            L.meta = readBandMeta(rb);
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
        info->bandsPerDate = first->GetRasterCount();
        for (int b = 1; b <= info->bandsPerDate; ++b) {
            const char* d = first->GetRasterBand(b)->GetDescription();
            info->bandNames.push_back(d && d[0] ? std::string(d) : "Band " + std::to_string(b));
        }
        info->sel = sel;
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
            const BandSelection& s = info->sel;
            const int need = std::max({s.band, s.ndBand, s.qaBand});
            if (need > ds->GetRasterCount()) {
                error = f + ": has no band " + std::to_string(need) + ".";
                GDALClose(ds);
                return nullptr;
            }
            Layer L;
            L.path = f;
            L.band = s.band;
            L.meta = readBandMeta(ds->GetRasterBand(s.band));
            if (s.ndBand > 0) L.ndMeta = readBandMeta(ds->GetRasterBand(s.ndBand));
            L.label = fs::u8path(f).stem().u8string();
            timeTexts.push_back(L.label);
            info->layers.push_back(L);
            GDALClose(ds);
        }
        std::snprintf(desc, sizeof(desc), "%d files (%s of each)", int(files.size()),
                      info->bandsPerDate > 1 ? info->selectionText().c_str() : "band 1");
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

GDALRasterBand* CubeReader::band(int layer, int b) {
    const Layer& L = info_->layers[layer];
    auto it = ds_.find(L.path);
    if (it == ds_.end())
        it = ds_.emplace(L.path, GDALDataset::Open(L.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY)).first;
    if (!it->second) return nullptr;
    if (b <= 0) b = L.band;
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
    if (!readRaw(layer, L.band, x, y, w, h, buf, bw, bh)) return false;
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

CubeReader& threadReader(const std::shared_ptr<const CubeInfo>& info) {
    thread_local std::unique_ptr<CubeReader> reader;
    if (!reader || reader->cubeId() != info->id) reader = std::make_unique<CubeReader>(info);
    return *reader;
}
