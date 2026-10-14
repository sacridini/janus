// Validation of maps (Olofsson et al. 2014; Stehman 2014): see validation.hpp.
#include "validation.hpp"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <functional>
#include <memory>
#include <numeric>
#include <random>
#include <unordered_map>
#include <unordered_set>

#include <cpl_conv.h>
#include <cpl_error.h>
#include <cpl_string.h>
#include <gdal_priv.h>
#include <gdal_utils.h>
#include <ogr_spatialref.h>
#include <ogrsf_frmts.h>

namespace fs = std::filesystem;

namespace val {

extern int g_testBandRows; // validation_selftest.cpp

namespace {
constexpr double kPi = 3.14159265358979323846;
}

const char* const kAdjudicated = "(adjudicated)";

const char* kindName(MapKind k) {
    switch (k) {
    case MapKind::Classes: return "classes";
    case MapKind::Intervals: return "intervals";
    case MapKind::Events: return "events";
    }
    return "classes";
}

const char* allocName(Alloc a) {
    switch (a) {
    case Alloc::Proportional: return "proportional";
    case Alloc::Equal: return "equal";
    case Alloc::Neyman: return "neyman";
    case Alloc::Manual: return "manual";
    }
    return "proportional";
}

// ---------------------------------------------------------------------------
// Map rules
// ---------------------------------------------------------------------------

int MapRule::classOf(double v) const {
    switch (kind) {
    case MapKind::Classes:
        if (!std::isfinite(v) || v != std::floor(v) || std::fabs(v) > 2e9) return kNone;
        return int(v);
    case MapKind::Intervals: {
        if (!std::isfinite(v)) return kNone;
        const int i = int(std::upper_bound(bins.begin(), bins.end(), v) - bins.begin());
        return i >= 1 && i < int(bins.size()) ? i : kNone;
    }
    case MapKind::Events: {
        if (!std::isfinite(v) || v <= 0) return 0; // no data is no change, as zeit's maps of events
        if (bins.empty()) return 1;
        const int i = int(std::upper_bound(bins.begin(), bins.end(), v) - bins.begin());
        return i >= 1 && i < int(bins.size()) ? i : kNone;
    }
    }
    return kNone;
}

bool MapRule::isChange(int cls) const {
    if (cls == kNone) return false;
    if (kind == MapKind::Events) return cls > 0;
    return change.count(cls) > 0;
}

std::string intervalName(double lo, double hi, bool years) {
    char b[96];
    if (years) {
        if (hi - lo > 1) std::snprintf(b, sizeof(b), "%d-%d", int(lo), int(hi) - 1);
        else std::snprintf(b, sizeof(b), "%d", int(lo));
        return b;
    }
    auto fmt = [](double v) {
        char s[40];
        if (std::isinf(v)) return std::string(v < 0 ? "-inf" : "inf");
        if (v == std::floor(v) && std::fabs(v) < 1e15) std::snprintf(s, sizeof(s), "%lld", (long long)v);
        else std::snprintf(s, sizeof(s), "%g", v);
        return std::string(s);
    };
    return fmt(lo) + "-" + fmt(hi);
}

std::string MapRule::className(int cls) const {
    if (cls == kNone) return "";
    switch (kind) {
    case MapKind::Classes: {
        const auto it = names.find(cls);
        return it != names.end() && !it->second.empty() ? it->second : std::to_string(cls);
    }
    case MapKind::Intervals:
        return cls >= 1 && cls < int(bins.size()) ? intervalName(bins[cls - 1], bins[cls], false) : "";
    case MapKind::Events:
        if (cls == 0) return "no change";
        if (bins.empty()) return "change";
        return cls >= 1 && cls < int(bins.size()) ? intervalName(bins[cls - 1], bins[cls], true) : "";
    }
    return "";
}

json MapRule::toJson() const {
    json n = json::object();
    for (const auto& [v, s] : names) n[std::to_string(v)] = s;
    return {{"path", path}, {"band", band}, {"name", name}, {"kind", kindName(kind)}, {"bins", bins},
            {"names", n}, {"change", std::vector<int>(change.begin(), change.end())}};
}

MapRule MapRule::fromJson(const json& j) {
    MapRule r;
    r.path = j.value("path", "");
    r.band = j.value("band", 1);
    r.name = j.value("name", "");
    const std::string k = j.value("kind", "classes");
    r.kind = k == "intervals" ? MapKind::Intervals : k == "events" ? MapKind::Events : MapKind::Classes;
    for (const json& b : j.value("bins", json::array()))
        if (b.is_number()) r.bins.push_back(b.get<double>());
    const json names = j.value("names", json::object()); // items() of a temporary would dangle
    for (const auto& [v, s] : names.items())
        if (s.is_string()) r.names[std::atoi(v.c_str())] = s.get<std::string>();
    for (const json& c : j.value("change", json::array()))
        if (c.is_number_integer()) r.change.insert(c.get<int>());
    return r;
}

std::vector<double> parseBins(const std::string& text, std::string& error) {
    std::vector<double> out;
    std::string t = text;
    for (char& c : t)
        if (c == ';' || c == '\t') c = ',';
    size_t i = 0;
    while (i < t.size()) {
        size_t j = t.find(',', i);
        if (j == std::string::npos) j = t.size();
        std::string tok = t.substr(i, j - i);
        tok.erase(0, tok.find_first_not_of(" "));
        tok.erase(tok.find_last_not_of(" ") + 1);
        i = j + 1;
        if (tok.empty()) continue;
        char* end = nullptr;
        const double v = std::strtod(tok.c_str(), &end);
        if (!end || *end) {
            error = "not a number: " + tok;
            return {};
        }
        out.push_back(v);
    }
    for (size_t k = 1; k < out.size(); ++k)
        if (!(out[k] > out[k - 1])) {
            error = "the edges must increase";
            return {};
        }
    if (out.size() == 1) {
        error = "give at least two edges";
        return {};
    }
    return out;
}

std::string binsText(const std::vector<double>& bins) {
    std::string s;
    for (double b : bins) {
        char t[40];
        if (std::isinf(b)) std::snprintf(t, sizeof(t), "%s", b < 0 ? "-inf" : "inf");
        else std::snprintf(t, sizeof(t), "%g", b);
        s += (s.empty() ? "" : ", ") + std::string(t);
    }
    return s;
}

// ---------------------------------------------------------------------------
// Grids and reading
// ---------------------------------------------------------------------------

void Grid::centre(int col, int row, double& x, double& y) const {
    x = gt[0] + (col + 0.5) * gt[1] + (row + 0.5) * gt[2];
    y = gt[3] + (col + 0.5) * gt[4] + (row + 0.5) * gt[5];
}

json Grid::toJson() const {
    return {{"w", w}, {"h", h}, {"gt", gt}, {"has_gt", hasGt}, {"wkt", wkt}};
}

Grid Grid::fromJson(const json& j) {
    Grid g;
    g.w = j.value("w", 0);
    g.h = j.value("h", 0);
    const json t = j.value("gt", json::array());
    for (int i = 0; i < 6 && i < int(t.size()); ++i) g.gt[i] = t[i].get<double>();
    g.hasGt = j.value("has_gt", false);
    g.wkt = j.value("wkt", "");
    return g;
}

namespace {

std::string wktOf(const OGRSpatialReference* s) {
    if (!s) return "";
    char* w = nullptr;
    const char* opts[] = {"FORMAT=WKT2_2018", nullptr};
    if (s->exportToWkt(&w, opts) != OGRERR_NONE) {
        CPLFree(w);
        w = nullptr;
        if (s->exportToWkt(&w) != OGRERR_NONE) {
            CPLFree(w);
            return "";
        }
    }
    std::string r = w ? w : "";
    CPLFree(w);
    return r;
}

void gridOf(GDALDataset* ds, Grid& g) {
    g.w = ds->GetRasterXSize();
    g.h = ds->GetRasterYSize();
    g.hasGt = ds->GetGeoTransform(g.gt.data()) == CE_None;
    if (!g.hasGt) g.gt = {0, 1, 0, 0, 0, 1};
    g.wkt = wktOf(ds->GetSpatialRef());
}

bool sameCrs(const std::string& a, const std::string& b) {
    if (a.empty() || b.empty()) return true; // one without a CRS: taken as the other's
    if (a == b) return true;
    OGRSpatialReference x, y;
    if (x.importFromWkt(a.c_str()) != OGRERR_NONE || y.importFromWkt(b.c_str()) != OGRERR_NONE) return false;
    return x.IsSame(&y);
}

bool sameGrid(const Grid& a, const Grid& b) {
    if (a.w != b.w || a.h != b.h || a.hasGt != b.hasGt) return false;
    const double tol = 1e-6 * std::max(std::fabs(a.gt[1]), std::fabs(a.gt[5]));
    for (int i = 0; i < 6; ++i)
        if (std::fabs(a.gt[i] - b.gt[i]) > tol) return false;
    return sameCrs(a.wkt, b.wkt);
}

std::string lastError(const std::string& what) {
    const char* m = CPLGetLastErrorMsg();
    return what + (m && *m ? std::string(": ") + m : std::string());
}

// One band of a raster, read in rows as float (no data -> NaN), on its own grid
// or warped (nearest neighbour) onto another one.
class MapReader {
public:
    MapReader() = default;
    MapReader(const MapReader&) = delete;
    MapReader& operator=(const MapReader&) = delete;
    ~MapReader() {
        if (warped_) GDALClose(warped_);
        if (one_) GDALClose(one_);
        if (ds_) GDALClose(ds_);
    }
    bool open(const std::string& path, int band, const Grid* target, std::string& error, int threads = 1) {
        CPLErrorReset();
        // Several threads decode a band's blocks (GeoTIFF).
        const std::string nt = "NUM_THREADS=" + std::to_string(std::max(1, threads));
        const char* oo[] = {nt.c_str(), nullptr};
        ds_ = GDALDataset::FromHandle(GDALOpenEx(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY, nullptr,
                                                 threads > 1 ? oo : nullptr, nullptr));
        if (!ds_) {
            error = lastError("cannot open " + path);
            return false;
        }
        if (band < 1 || band > ds_->GetRasterCount()) {
            error = path + ": no band " + std::to_string(band);
            return false;
        }
        gridOf(ds_, own_);
        GDALDataset* use = ds_;
        int b = band;
        if (target && !sameGrid(own_, *target)) {
            if (!target->hasGt || target->gt[2] != 0 || target->gt[4] != 0 || target->gt[5] >= 0 || !own_.hasGt) {
                error = path + ": not on the grid of the strata, and it cannot be warped onto it (rotated or no transform)";
                return false;
            }
            // The band alone (a VRT), then warped onto the target grid: float, NaN where it has no data.
            const std::string bs = std::to_string(band);
            const char* targs[] = {"-of", "VRT", "-b", bs.c_str(), nullptr};
            GDALTranslateOptions* to = GDALTranslateOptionsNew(const_cast<char**>(targs), nullptr);
            one_ = GDALDataset::FromHandle(GDALTranslate("", GDALDataset::ToHandle(ds_), to, nullptr));
            GDALTranslateOptionsFree(to);
            if (!one_) {
                error = lastError(path + ": cannot read band " + bs);
                return false;
            }
            const Grid& g = *target;
            CPLStringList a;
            a.AddString("-of");
            a.AddString("VRT");
            a.AddString("-r");
            a.AddString("near");
            a.AddString("-ot");
            a.AddString("Float32");
            a.AddString("-dstnodata");
            a.AddString("nan");
            if (!g.wkt.empty() && !own_.wkt.empty()) {
                a.AddString("-t_srs");
                a.AddString(g.wkt.c_str());
            }
            a.AddString("-te");
            a.AddString(CPLSPrintf("%.17g", g.gt[0]));
            a.AddString(CPLSPrintf("%.17g", g.gt[3] + g.h * g.gt[5]));
            a.AddString(CPLSPrintf("%.17g", g.gt[0] + g.w * g.gt[1]));
            a.AddString(CPLSPrintf("%.17g", g.gt[3]));
            a.AddString("-ts");
            a.AddString(CPLSPrintf("%d", g.w));
            a.AddString(CPLSPrintf("%d", g.h));
            GDALWarpAppOptions* wo = GDALWarpAppOptionsNew(a.List(), nullptr);
            GDALDatasetH src = GDALDataset::ToHandle(one_);
            int usage = 0;
            warped_ = GDALDataset::FromHandle(GDALWarp("", nullptr, 1, &src, wo, &usage));
            GDALWarpAppOptionsFree(wo);
            if (!warped_) {
                error = lastError(path + ": cannot warp it onto the grid of the strata");
                return false;
            }
            use = warped_;
            b = 1;
        }
        band_ = use->GetRasterBand(b);
        w = use->GetRasterXSize();
        h = use->GetRasterYSize();
        int has = 0;
        nodata_ = band_->GetNoDataValue(&has);
        hasNodata_ = has && !std::isnan(nodata_);
        return true;
    }
    bool rows(int y0, int n, float* out) {
        if (band_->RasterIO(GF_Read, 0, y0, w, n, out, w, n, GDT_Float32, 0, 0) != CE_None) return false;
        if (hasNodata_) {
            const float nd = float(nodata_);
            for (size_t i = 0, N = size_t(w) * n; i < N; ++i)
                if (out[i] == nd) out[i] = NAN;
        }
        return true;
    }
    const Grid& own() const { return own_; }
    int w = 0, h = 0;

private:
    GDALDataset* ds_ = nullptr;
    GDALDataset* one_ = nullptr;
    GDALDataset* warped_ = nullptr;
    GDALRasterBand* band_ = nullptr;
    Grid own_;
    double nodata_ = 0;
    bool hasNodata_ = false;
};

// The crossings of a polygon's rings with the centre line of each row, rows
// asked in increasing order (an active edge list).
class Spans {
public:
    explicit Spans(const Polygon& p) {
        for (const Ring& r : p) {
            const size_t n = r.size() / 2;
            for (size_t i = 0; i < n; ++i) {
                double x0 = r[2 * i], y0 = r[2 * i + 1];
                double x1 = r[2 * ((i + 1) % n)], y1 = r[2 * ((i + 1) % n) + 1];
                if (y0 == y1) continue;
                if (y0 > y1) {
                    std::swap(x0, x1);
                    std::swap(y0, y1);
                }
                edges_.push_back({x0, y0, x1, y1});
            }
        }
        std::sort(edges_.begin(), edges_.end(), [](const auto& a, const auto& b) { return a[1] < b[1]; });
    }
    bool empty() const { return edges_.empty(); }
    // Pixels whose centres are inside on row y: inside[x] = 1.
    void row(int y, int w, std::vector<char>& inside) {
        const double yc = y + 0.5;
        while (next_ < edges_.size() && edges_[next_][1] <= yc) active_.push_back(next_++);
        xs_.clear();
        for (size_t k = 0; k < active_.size();) {
            const auto& e = edges_[active_[k]];
            if (e[3] <= yc) {
                active_[k] = active_.back();
                active_.pop_back();
                continue;
            }
            xs_.push_back(e[0] + (yc - e[1]) * (e[2] - e[0]) / (e[3] - e[1]));
            ++k;
        }
        std::sort(xs_.begin(), xs_.end());
        inside.assign(w, 0);
        for (size_t k = 0; k + 1 < xs_.size(); k += 2) {
            // centres x + 0.5 in [a, b)
            const int a = std::max(0, int(std::ceil(xs_[k] - 0.5)));
            const int b = std::min(w, int(std::ceil(xs_[k + 1] - 0.5)));
            for (int x = a; x < b; ++x) inside[x] = 1;
        }
    }

private:
    std::vector<std::array<double, 4>> edges_; // x0, y0 (lower), x1, y1
    std::vector<size_t> active_;
    std::vector<double> xs_;
    size_t next_ = 0;
};

// Areas of classes on one row: runs of a class added at once.
struct RowAcc {
    std::unordered_map<int, int64_t> count;
    int last = kNone;
    int64_t run = 0;
    void add(int c) {
        if (c == last) {
            ++run;
            return;
        }
        flush();
        last = c;
        run = 1;
    }
    void flush() {
        if (run) count[last] += run;
        run = 0;
        last = kNone;
    }
    void into(std::map<int, double>& area, double rowArea) {
        flush();
        for (const auto& [c, n] : count) area[c] += double(n) * rowArea;
        count.clear();
    }
};

// Rows per band: about 256 MB of floats, a multiple of 256 (the strata raster's tiles) when large.
int bandRows(int w, int maps, int halo) {
    if (g_testBandRows > 0) return g_testBandRows;
    const int64_t budget = int64_t(256) << 20;
    int64_t b = budget / (int64_t(std::max(1, w)) * (4 * int64_t(maps) + 8)) - 2 * int64_t(halo);
    b = std::clamp<int64_t>(b, 1, 1024);
    if (b >= 256) b -= b % 256;
    return int(b);
}

const std::array<std::array<unsigned char, 3>, 8> kChangeColors = {{{215, 48, 39}, {244, 109, 67}, {253, 174, 97},
                                                                     {165, 0, 38}, {252, 78, 42}, {227, 26, 28},
                                                                     {189, 0, 38}, {240, 59, 32}}};
const std::array<std::array<unsigned char, 3>, 8> kOtherColors = {{{26, 152, 80}, {145, 207, 96}, {69, 117, 180},
                                                                    {116, 173, 209}, {102, 189, 99}, {153, 112, 171},
                                                                    {128, 128, 128}, {77, 146, 33}}};

} // namespace

bool readGrid(const std::string& path, Grid& g, std::string& error) {
    CPLErrorReset();
    GDALDataset* ds = GDALDataset::FromHandle(GDALOpenEx(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY, nullptr, nullptr, nullptr));
    if (!ds) {
        error = lastError("cannot open " + path);
        return false;
    }
    gridOf(ds, g);
    GDALClose(ds);
    return true;
}

std::vector<double> rowAreas(const Grid& g, std::string& unit) {
    OGRSpatialReference s;
    if (g.wkt.empty() || !g.hasGt || s.importFromWkt(g.wkt.c_str()) != OGRERR_NONE) {
        unit = "pixels";
        return std::vector<double>(g.h, 1.0);
    }
    unit = "ha";
    std::vector<double> a(g.h);
    if (s.IsGeographic()) {
        const double R = 6371007.181, rad = kPi / 180;
        for (int y = 0; y < g.h; ++y) {
            const double top = (g.gt[3] + g.gt[5] * y) * rad, bottom = (g.gt[3] + g.gt[5] * (y + 1)) * rad;
            a[y] = R * R * std::fabs(g.gt[1]) * rad * std::fabs(std::sin(top) - std::sin(bottom)) / 1e4;
        }
    } else {
        const double f = s.GetLinearUnits();
        std::fill(a.begin(), a.end(), std::fabs(g.gt[1] * g.gt[5] - g.gt[2] * g.gt[4]) * f * f / 1e4);
    }
    return a;
}

// ---------------------------------------------------------------------------
// Strata
// ---------------------------------------------------------------------------

json StrataSpec::toJson() const {
    json m = json::array();
    for (const MapRule& r : maps) m.push_back(r.toJson());
    return {{"maps", m}, {"buffer", buffer}, {"limit", limit}};
}

StrataSpec StrataSpec::fromJson(const json& j) {
    StrataSpec s;
    for (const json& m : j.value("maps", json::array())) s.maps.push_back(MapRule::fromJson(m));
    s.buffer = j.value("buffer", 0);
    for (const json& r : j.value("limit", json::array())) s.limit.push_back(r.get<Ring>());
    return s;
}

double Strata::totalArea() const {
    double a = 0;
    for (const Stratum& s : strata) a += s.area;
    return a;
}

int Strata::indexOfCode(int code) const {
    for (size_t i = 0; i < strata.size(); ++i)
        if (strata[i].code == code) return int(i);
    return -1;
}

bool computeStrata(const StrataSpec& spec, const std::string& out, Strata& result, Progress* p, std::string& error) {
    if (spec.maps.empty()) {
        error = "no map to stratify";
        return false;
    }
    Strata r;
    const int M = int(spec.maps.size());
    std::vector<std::unique_ptr<MapReader>> in(M);
    for (int k = 0; k < M; ++k) {
        in[k] = std::make_unique<MapReader>();
        if (!in[k]->open(spec.maps[k].path, spec.maps[k].band, k ? &r.grid : nullptr, error, p ? p->threads : 1)) return false;
        if (k == 0) r.grid = in[0]->own();
    }
    const int W = r.grid.w, H = r.grid.h;
    r.rowArea = rowAreas(r.grid, r.unit);

    GDALDriver* drv = GetGDALDriverManager()->GetDriverByName("GTiff");
    if (!drv) {
        error = "GDAL has no GTiff driver";
        return false;
    }
    char** co = nullptr;
    co = CSLSetNameValue(co, "TILED", "YES");
    co = CSLSetNameValue(co, "COMPRESS", "DEFLATE");
    co = CSLSetNameValue(co, "BIGTIFF", "IF_SAFER");
    co = CSLSetNameValue(co, "NUM_THREADS", std::to_string(std::max(1, p ? p->threads : 1)).c_str());
    CPLErrorReset();
    GDALDataset* o = drv->Create(out.c_str(), W, H, 1, GDT_Byte, co);
    CSLDestroy(co);
    if (!o) {
        error = lastError("cannot write " + out);
        return false;
    }
    bool ok = false;
    auto finish = [&]() {
        GDALClose(o);
        if (!ok) VSIUnlink(out.c_str());
    };
    if (r.grid.hasGt) o->SetGeoTransform(r.grid.gt.data());
    if (!r.grid.wkt.empty()) o->SetProjection(r.grid.wkt.c_str());
    GDALRasterBand* ob = o->GetRasterBand(1);
    ob->SetNoDataValue(0);

    const int buf = std::max(0, spec.buffer);
    const int B = bandRows(W, M, buf);
    std::vector<std::vector<float>> vals(M);
    std::vector<int> key;
    std::vector<uint8_t> chg, hd;
    std::vector<int32_t> col;  // vertical window counts of hd
    std::vector<int64_t> pre(size_t(W) + 1);
    std::vector<uint8_t> outRows;
    std::vector<char> inside(W, 1);
    std::map<int, int> codeOfKey;
    int bufferCode = 0, nextCode = 1;
    int lastKey = kNone, lastCode = 0; // runs of a class along a row: no map lookup
    std::vector<std::vector<int32_t>> counts(256);
    std::vector<std::map<int, double>> mapped(M);
    std::vector<RowAcc> acc(M);
    Spans spans(spec.limit);

    for (int y0 = 0; y0 < H; y0 += B) {
        if (p && p->cancel) {
            error = "cancelled";
            finish();
            return false;
        }
        const int y1 = std::min(H, y0 + B), r0 = std::max(0, y0 - buf), r1 = std::min(H, y1 + buf), R = r1 - r0;
        for (int k = 0; k < M; ++k) {
            vals[k].resize(size_t(W) * R);
            if (!in[k]->rows(r0, R, vals[k].data())) {
                error = lastError(spec.maps[k].path + ": read failed");
                finish();
                return false;
            }
        }
        key.assign(size_t(W) * R, kNone);
        chg.assign(size_t(W) * R, 0);
        for (size_t i = 0, N = size_t(W) * R; i < N; ++i) {
            if (M == 1) {
                const int c = spec.maps[0].classOf(vals[0][i]);
                key[i] = c;
                chg[i] = c != kNone && spec.maps[0].isChange(c);
                continue;
            }
            int n = 0;
            bool any = true;
            for (int k = 0; k < M && any; ++k) {
                const int c = spec.maps[k].classOf(vals[k][i]);
                if (c == kNone) any = false;
                else n += spec.maps[k].isChange(c);
            }
            if (any) {
                key[i] = n;
                chg[i] = n > 0;
            }
        }
        // The buffer: change dilated by `buf` pixels, along the rows then down the columns.
        if (buf > 0) {
            hd.assign(size_t(W) * R, 0);
            for (int rr = 0; rr < R; ++rr) {
                const uint8_t* c = &chg[size_t(rr) * W];
                pre[0] = 0;
                for (int x = 0; x < W; ++x) pre[x + 1] = pre[x] + c[x];
                uint8_t* h = &hd[size_t(rr) * W];
                for (int x = 0; x < W; ++x) h[x] = pre[std::min(W, x + buf + 1)] - pre[std::max(0, x - buf)] > 0;
            }
            col.assign(W, 0);
            for (int rr = std::max(r0, y0 - buf); rr <= std::min(r1 - 1, y0 + buf - 1); ++rr)
                for (int x = 0; x < W; ++x) col[x] += hd[size_t(rr - r0) * W + x];
        }
        outRows.assign(size_t(W) * (y1 - y0), 0);
        for (int y = y0; y < y1; ++y) {
            if (buf > 0) { // the window [y - buf, y + buf]: add its last row, drop the one before it
                const int add = y + buf, drop = y - buf - 1;
                if (add < r1)
                    for (int x = 0; x < W; ++x) col[x] += hd[size_t(add - r0) * W + x];
                if (drop >= r0)
                    for (int x = 0; x < W; ++x) col[x] -= hd[size_t(drop - r0) * W + x];
            }
            if (!spans.empty()) spans.row(y, W, inside);
            const size_t base = size_t(y - r0) * W;
            uint8_t* orow = &outRows[size_t(y - y0) * W];
            for (int x = 0; x < W; ++x) {
                const int kv = key[base + x];
                if (kv == kNone || !inside[x]) continue;
                int code;
                if (buf > 0 && !chg[base + x] && col[x] > 0) {
                    if (!bufferCode) bufferCode = nextCode++;
                    code = bufferCode;
                } else if (kv == lastKey) {
                    code = lastCode;
                } else {
                    auto it = codeOfKey.find(kv);
                    if (it == codeOfKey.end()) it = codeOfKey.emplace(kv, nextCode++).first;
                    code = it->second;
                    lastKey = kv;
                    lastCode = code;
                }
                if (code > 255) {
                    error = "more than 255 strata: give the map's values as intervals";
                    finish();
                    return false;
                }
                orow[x] = uint8_t(code);
                if (counts[code].empty()) counts[code].assign(H, 0);
                ++counts[code][y];
                for (int k = 0; k < M; ++k) acc[k].add(M == 1 ? kv : spec.maps[k].classOf(vals[k][base + x]));
            }
            for (int k = 0; k < M; ++k) acc[k].into(mapped[k], r.rowArea[y]);
        }
        if (ob->RasterIO(GF_Write, 0, y0, W, y1 - y0, outRows.data(), W, y1 - y0, GDT_Byte, 0, 0) != CE_None) {
            error = lastError("cannot write " + out);
            finish();
            return false;
        }
        if (p) p->done = double(y1) / H;
    }
    // The strata in order: by class (or by how many maps call change), the buffer last.
    std::vector<std::pair<int, int>> keys(codeOfKey.begin(), codeOfKey.end()); // (key, code), sorted by key
    for (const auto& [k, code] : keys) {
        Stratum s;
        s.code = code;
        if (M == 1) {
            s.cls = k;
            s.name = spec.maps[0].className(k);
            s.change = spec.maps[0].isChange(k);
        } else {
            s.change = k > 0;
            s.name = k == 0 ? "no change in any map"
                   : k == M ? "change in all " + std::to_string(M) + " maps"
                            : "change in " + std::to_string(k) + " of " + std::to_string(M) + " maps";
        }
        r.strata.push_back(s);
    }
    if (bufferCode) {
        Stratum s;
        s.code = bufferCode;
        s.buffer = true;
        s.name = "buffer (" + std::to_string(buf) + " px)";
        r.strata.push_back(s);
    }
    double total = 0;
    for (Stratum& s : r.strata) {
        const std::vector<int32_t>& c = counts[s.code];
        for (int y = 0; y < H; ++y) {
            s.pixels += c[y];
            s.area += c[y] * r.rowArea[y];
        }
        total += s.area;
        r.rowCounts.push_back(c);
    }
    for (Stratum& s : r.strata) s.weight = total > 0 ? s.area / total : 0;
    r.mapped = std::move(mapped);
    if (r.strata.empty()) {
        error = "no pixel of the map falls in a stratum (no data everywhere, or outside the limit)";
        finish();
        return false;
    }
    // Names and colours for other programs (QGIS) and for the map.
    std::vector<std::string> names(nextCode);
    GDALColorTable ct;
    GDALColorEntry none{0, 0, 0, 0};
    ct.SetColorEntry(0, &none);
    int nc = 0, no = 0;
    for (const Stratum& s : r.strata) {
        names[s.code] = s.name;
        const auto& c = s.buffer ? std::array<unsigned char, 3>{254, 224, 139}
                      : s.change ? kChangeColors[nc++ % kChangeColors.size()]
                                 : kOtherColors[no++ % kOtherColors.size()];
        GDALColorEntry e{c[0], c[1], c[2], 255};
        ct.SetColorEntry(s.code, &e);
    }
    CPLStringList cats;
    for (const std::string& n : names) cats.AddString(n.c_str());
    ob->SetCategoryNames(cats.List());
    ob->SetColorTable(&ct);
    ok = true;
    finish();
    result = std::move(r);
    return true;
}

bool countStrata(const std::string& strataPath, Strata& s, Progress* p, std::string& error) {
    MapReader in;
    if (!in.open(strataPath, 1, nullptr, error, p ? p->threads : 1)) return false;
    if (in.w != s.grid.w || in.h != s.grid.h) {
        error = strataPath + ": not the size of the project's strata";
        return false;
    }
    const int W = in.w, H = in.h, B = bandRows(W, 1, 0);
    std::vector<int> idx(256, -1);
    for (size_t i = 0; i < s.strata.size(); ++i) idx[s.strata[i].code & 255] = int(i);
    s.rowCounts.assign(s.strata.size(), std::vector<int32_t>(H, 0));
    std::vector<float> v;
    for (int y0 = 0; y0 < H; y0 += B) {
        if (p && p->cancel) {
            error = "cancelled";
            return false;
        }
        const int n = std::min(B, H - y0);
        v.resize(size_t(W) * n);
        if (!in.rows(y0, n, v.data())) {
            error = lastError(strataPath + ": read failed");
            return false;
        }
        for (int r = 0; r < n; ++r)
            for (int x = 0; x < W; ++x) {
                const float c = v[size_t(r) * W + x];
                if (!(c >= 1 && c <= 255)) continue;
                const int i = idx[int(c)];
                if (i >= 0) ++s.rowCounts[i][y0 + r];
            }
        if (p) p->done = double(y0 + n) / H;
    }
    return true;
}

bool mappedAreas(const MapRule& map, const std::string& strataPath, const Strata& s, std::map<int, double>& area,
                 Progress* p, std::string& error) {
    MapReader st, m;
    const int threads = p ? p->threads : 1;
    if (!st.open(strataPath, 1, nullptr, error, threads) || !m.open(map.path, map.band, &s.grid, error, threads)) return false;
    const int W = st.w, H = st.h, B = bandRows(W, 2, 0);
    std::vector<float> a, b;
    RowAcc acc;
    area.clear();
    for (int y0 = 0; y0 < H; y0 += B) {
        if (p && p->cancel) {
            error = "cancelled";
            return false;
        }
        const int n = std::min(B, H - y0);
        a.resize(size_t(W) * n);
        b.resize(size_t(W) * n);
        if (!st.rows(y0, n, a.data()) || !m.rows(y0, n, b.data())) {
            error = lastError(map.path + ": read failed");
            return false;
        }
        for (int r = 0; r < n; ++r) {
            for (int x = 0; x < W; ++x) {
                const size_t i = size_t(r) * W + x;
                if (!(a[i] >= 1)) continue;
                const int c = map.classOf(b[i]);
                if (c != kNone) acc.add(c);
            }
            acc.into(area, y0 + r < int(s.rowArea.size()) ? s.rowArea[y0 + r] : 1.0);
        }
        if (p) p->done = double(y0 + n) / H;
    }
    return true;
}

bool valuesAt(const MapRule& map, const std::string& wkt, const std::vector<std::array<double, 2>>& xy,
              std::vector<double>& out, std::string& error) {
    CPLErrorReset();
    GDALDataset* ds = GDALDataset::FromHandle(GDALOpenEx(map.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY, nullptr, nullptr, nullptr));
    if (!ds) {
        error = lastError("cannot open " + map.path);
        return false;
    }
    std::unique_ptr<GDALDataset, void (*)(GDALDataset*)> hold(ds, [](GDALDataset* d) { GDALClose(d); });
    if (map.band < 1 || map.band > ds->GetRasterCount()) {
        error = map.path + ": no band " + std::to_string(map.band);
        return false;
    }
    Grid g;
    gridOf(ds, g);
    double inv[6];
    if (!GDALInvGeoTransform(g.gt.data(), inv)) {
        error = map.path + ": its transform cannot be inverted";
        return false;
    }
    std::vector<double> x(xy.size()), y(xy.size());
    std::vector<int> okv(xy.size(), 1);
    for (size_t i = 0; i < xy.size(); ++i) {
        x[i] = xy[i][0];
        y[i] = xy[i][1];
    }
    if (!sameCrs(wkt, g.wkt)) {
        OGRSpatialReference a, b;
        a.importFromWkt(wkt.c_str());
        b.importFromWkt(g.wkt.c_str());
        a.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
        b.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
        std::unique_ptr<OGRCoordinateTransformation> ct(OGRCreateCoordinateTransformation(&a, &b));
        if (!ct) {
            error = map.path + ": no transformation from the points' CRS";
            return false;
        }
        ct->Transform(int(xy.size()), x.data(), y.data(), nullptr, okv.data());
    }
    GDALRasterBand* band = ds->GetRasterBand(map.band);
    int has = 0;
    const double nd = band->GetNoDataValue(&has);
    // In row order: the blocks stay in GDAL's cache.
    std::vector<size_t> order(xy.size());
    std::vector<std::pair<int, int>> px(xy.size(), {-1, -1});
    for (size_t i = 0; i < xy.size(); ++i) {
        order[i] = i;
        if (!okv[i]) continue;
        const double c = inv[0] + x[i] * inv[1] + y[i] * inv[2], r = inv[3] + x[i] * inv[4] + y[i] * inv[5];
        if (c >= 0 && r >= 0 && c < g.w && r < g.h) px[i] = {int(std::floor(c)), int(std::floor(r))};
    }
    std::sort(order.begin(), order.end(), [&](size_t a, size_t b) { return px[a].second < px[b].second; });
    out.assign(xy.size(), NAN);
    for (size_t i : order) {
        if (px[i].first < 0) continue;
        double v = NAN;
        if (band->RasterIO(GF_Read, px[i].first, px[i].second, 1, 1, &v, 1, 1, GDT_Float64, 0, 0) != CE_None) continue;
        if (has && !std::isnan(nd) && v == nd) v = NAN;
        out[i] = v;
    }
    return true;
}

// ---------------------------------------------------------------------------
// Design
// ---------------------------------------------------------------------------

int sampleSize(const std::vector<Stratum>& s, double targetSe) {
    double ws = 0, ws2 = 0, N = 0;
    for (const Stratum& t : s) {
        if (t.pixels <= 0) continue;
        const double sd = std::sqrt(t.expectedUa * (1 - t.expectedUa));
        ws += t.weight * sd;
        ws2 += t.weight * sd * sd;
        N += double(t.pixels);
    }
    if (targetSe <= 0 || N <= 0) return 0;
    return int(std::ceil(ws * ws / (targetSe * targetSe + ws2 / N)));
}

namespace {

// Largest remainders (as zeit's _round: ties to the first stratum).
std::vector<int64_t> roundTo(const std::vector<double>& v, int64_t total) {
    std::vector<int64_t> f(v.size());
    int64_t sum = 0;
    for (size_t i = 0; i < v.size(); ++i) sum += f[i] = int64_t(std::floor(v[i]));
    int64_t extra = total - sum;
    if (extra > 0) {
        std::vector<size_t> order(v.size());
        std::iota(order.begin(), order.end(), 0);
        std::stable_sort(order.begin(), order.end(), [&](size_t a, size_t b) { return v[a] - f[a] > v[b] - f[b]; });
        for (size_t k = 0; k < order.size() && extra > 0; ++k, --extra) ++f[order[k]];
    }
    return f;
}

} // namespace

std::vector<int> allocate(const std::vector<Stratum>& s, int n, Alloc a, int minPer) {
    const size_t S = s.size();
    std::vector<int> out(S, 0);
    if (a == Alloc::Manual) {
        for (size_t i = 0; i < S; ++i) out[i] = int(std::clamp<int64_t>(s[i].n, 0, s[i].pixels));
        return out;
    }
    std::vector<double> share(S);
    double sum = 0;
    for (size_t i = 0; i < S; ++i) {
        const double sd = std::sqrt(s[i].expectedUa * (1 - s[i].expectedUa));
        share[i] = s[i].pixels <= 0 ? 0 : a == Alloc::Equal ? 1.0 : a == Alloc::Neyman ? s[i].weight * sd : s[i].weight;
        sum += share[i];
    }
    if (sum <= 0 || n <= 0) return out;
    for (double& v : share) v /= sum;
    auto scaled = [&](int64_t total, const std::vector<double>& sh) {
        std::vector<double> v(S);
        for (size_t i = 0; i < S; ++i) v[i] = double(total) * sh[i];
        return roundTo(v, total);
    };
    std::vector<int64_t> counts = scaled(n, share);
    if (minPer > 0) {
        std::vector<char> low(S);
        for (size_t i = 0; i < S; ++i) low[i] = counts[i] < minPer;
        for (size_t it = 0; it < S; ++it) {
            if (std::none_of(low.begin(), low.end(), [](char c) { return c != 0; })) break;
            const int64_t nLow = std::count(low.begin(), low.end(), 1);
            const int64_t rest = n - int64_t(minPer) * nLow;
            if (rest <= 0) {
                const bool all = nLow == int64_t(S);
                for (size_t i = 0; i < S; ++i)
                    if (low[i] || all) counts[i] = minPer;
                break;
            }
            std::vector<double> free(S);
            double fs = 0;
            for (size_t i = 0; i < S; ++i) fs += free[i] = low[i] ? 0 : share[i];
            if (fs > 0) {
                for (double& v : free) v /= fs;
                counts = scaled(rest, free);
            } else {
                std::fill(counts.begin(), counts.end(), 0);
            }
            for (size_t i = 0; i < S; ++i) {
                if (low[i]) counts[i] = minPer;
                low[i] = low[i] || counts[i] < minPer;
            }
        }
    }
    for (size_t i = 0; i < S; ++i) out[i] = int(std::min<int64_t>(counts[i], s[i].pixels));
    return out;
}

Expected expectedErrors(const std::vector<Stratum>& s, double totalArea, double confidence) {
    Expected e;
    const double z = zOf(confidence);
    double v = 0, vc = 0, pc = 0;
    bool ok = true;
    for (const Stratum& t : s) {
        const double u = t.expectedUa;
        e.userSe.push_back(t.n > 1 ? std::sqrt(u * (1 - u) / (t.n - 1)) : NAN);
        e.change |= t.change;
        if (t.weight <= 0) continue;
        if (t.n <= 1) {
            ok = false;
            continue;
        }
        v += t.weight * t.weight * u * (1 - u) / (t.n - 1);
        const double pch = t.change ? u : 1 - u; // a stratum's errors taken as change (or as no change)
        pc += t.weight * pch;
        vc += t.weight * t.weight * pch * (1 - pch) / (t.n - 1);
    }
    if (ok) {
        e.overallSe = std::sqrt(v);
        if (e.change) {
            e.changeArea = pc * totalArea;
            e.changeCi = z * std::sqrt(vc) * totalArea;
        }
    }
    return e;
}

// ---------------------------------------------------------------------------
// Sample
// ---------------------------------------------------------------------------

namespace {

// SplitMix64 and an unbiased bounded draw: the same numbers on every platform
// (std::uniform_int_distribution is not specified).
struct Rng {
    uint64_t s;
    uint64_t next() {
        uint64_t z = (s += 0x9E3779B97F4A7C15ull);
        z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ull;
        z = (z ^ (z >> 27)) * 0x94D049BB133111EBull;
        return z ^ (z >> 31);
    }
    uint64_t below(uint64_t bound) { // [0, bound)
        const uint64_t threshold = (0 - bound) % bound;
        for (;;) {
            const uint64_t r = next();
            if (r >= threshold) return r % bound;
        }
    }
};

} // namespace

bool drawSample(const Strata& s, const std::string& strataPath, const std::vector<int>& n, uint64_t seed,
                const std::vector<Point>& existing, std::vector<Point>& out, std::string& error) {
    out.clear();
    if (s.rowCounts.size() != s.strata.size()) {
        error = "the strata were not counted";
        return false;
    }
    const int H = s.grid.h, W = s.grid.w;
    MapReader in;
    if (!in.open(strataPath, 1, nullptr, error)) return false;
    std::unordered_map<int, std::vector<float>> rows;
    auto row = [&](int y) -> const std::vector<float>* {
        auto it = rows.find(y);
        if (it != rows.end()) return &it->second;
        std::vector<float> v(W);
        if (!in.rows(y, 1, v.data())) return nullptr;
        return &rows.emplace(y, std::move(v)).first->second;
    };
    struct Pick {
        int stratum, row;
        int64_t within;
    };
    std::vector<Pick> picks;
    for (size_t i = 0; i < s.strata.size() && i < n.size(); ++i) {
        if (n[i] <= 0) continue;
        const int code = s.strata[i].code;
        const std::vector<int32_t>& per = s.rowCounts[i];
        std::vector<int64_t> ends(H);
        int64_t acc = 0;
        for (int y = 0; y < H; ++y) ends[y] = acc += per[y];
        // The ranks (among the stratum's pixels, in row order) of the points it already has.
        std::vector<int64_t> taken;
        for (const Point& p : existing) {
            if (p.stratum != code || p.row < 0 || p.row >= H) continue;
            const std::vector<float>* r = row(p.row);
            if (!r) {
                error = lastError(strataPath + ": read failed");
                return false;
            }
            int64_t k = 0;
            for (int x = 0; x < p.col && x < W; ++x) k += (*r)[x] == float(code);
            taken.push_back((p.row ? ends[p.row - 1] : 0) + k);
        }
        std::sort(taken.begin(), taken.end());
        taken.erase(std::unique(taken.begin(), taken.end()), taken.end());
        const int64_t free = acc - int64_t(taken.size());
        const int64_t k = std::min<int64_t>(n[i], std::max<int64_t>(0, free));
        if (k <= 0) continue;
        Rng rng{seed * 0x9E3779B97F4A7C15ull ^ (uint64_t(code) << 32) ^ uint64_t(existing.size())};
        // Floyd's algorithm: k distinct of [0, free).
        std::unordered_set<int64_t> chosen;
        chosen.reserve(size_t(k) * 2);
        for (int64_t j = free - k; j < free; ++j) {
            const int64_t t = int64_t(rng.below(uint64_t(j + 1)));
            if (!chosen.insert(t).second) chosen.insert(j);
        }
        std::vector<int64_t> idx(chosen.begin(), chosen.end());
        std::sort(idx.begin(), idx.end());
        size_t e = 0;
        for (int64_t c : idx) {
            int64_t rank = c + int64_t(e); // the c-th free rank: skip the taken ones below it
            while (e < taken.size() && taken[e] <= rank) {
                ++e;
                ++rank;
            }
            const int y = int(std::upper_bound(ends.begin(), ends.end(), rank) - ends.begin());
            picks.push_back({int(i), y, rank - (ends[y] - per[y])});
        }
    }
    int id = 0;
    for (const Point& p : existing) id = std::max(id, p.id);
    std::sort(picks.begin(), picks.end(), [](const Pick& a, const Pick& b) {
        return a.stratum != b.stratum ? a.stratum < b.stratum : a.row != b.row ? a.row < b.row : a.within < b.within;
    });
    for (const Pick& k : picks) {
        const std::vector<float>* r = row(k.row);
        if (!r) {
            error = lastError(strataPath + ": read failed");
            return false;
        }
        const float code = float(s.strata[k.stratum].code);
        int64_t seen = 0;
        int colFound = -1;
        for (int x = 0; x < W; ++x)
            if ((*r)[x] == code && seen++ == k.within) {
                colFound = x;
                break;
            }
        if (colFound < 0) {
            error = strataPath + ": the strata raster changed since it was counted";
            return false;
        }
        Point p;
        p.id = ++id;
        p.stratum = s.strata[k.stratum].code;
        p.row = k.row;
        p.col = colFound;
        s.grid.centre(p.col, p.row, p.x, p.y);
        out.push_back(p);
    }
    return true;
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

std::string nowIso() {
    const std::time_t t = std::time(nullptr);
    std::tm u{};
#ifdef _WIN32
    gmtime_s(&u, &t);
#else
    gmtime_r(&t, &u);
#endif
    char b[32];
    std::strftime(b, sizeof(b), "%Y-%m-%dT%H:%M:%SZ", &u);
    return b;
}

int yearOf(const std::string& date) {
    for (size_t i = 0; i + 4 <= date.size(); ++i) {
        bool four = true;
        for (size_t k = 0; k < 4; ++k) four = four && date[i + k] >= '0' && date[i + k] <= '9';
        if (four && (i + 4 == date.size() || date[i + 4] < '0' || date[i + 4] > '9') &&
            (i == 0 || date[i - 1] < '0' || date[i - 1] > '9'))
            return std::atoi(date.substr(i, 4).c_str());
    }
    return 0;
}

const Label* resolve(const std::map<std::string, std::vector<Label>>& labels, size_t i, bool& conflict) {
    conflict = false;
    const auto adj = labels.find(kAdjudicated);
    if (adj != labels.end() && i < adj->second.size() && !adj->second[i].label.empty()) return &adj->second[i];
    const Label* first = nullptr;
    for (const auto& [who, v] : labels) {
        if (who == kAdjudicated || i >= v.size() || v[i].label.empty()) continue;
        if (!first) first = &v[i];
        else if (v[i].label != first->label) conflict = true;
    }
    return conflict ? nullptr : first;
}

json labelToJson(const Label& l) {
    json e = json::array();
    for (const Event& v : l.events) e.push_back({{"label", v.label}, {"date", v.date}});
    return {{"label", l.label}, {"date", l.date}, {"notes", l.notes}, {"confidence", l.confidence},
            {"events", e}, {"updated", l.updated}};
}

Label labelFromJson(const json& j) {
    Label l;
    l.label = j.value("label", "");
    l.date = j.value("date", "");
    l.notes = j.value("notes", "");
    l.confidence = j.value("confidence", 0);
    l.updated = j.value("updated", "");
    for (const json& e : j.value("events", json::array()))
        l.events.push_back({e.value("label", ""), e.value("date", "")});
    return l;
}

// ---------------------------------------------------------------------------
// Estimation
// ---------------------------------------------------------------------------

double zOf(double confidence) {
    // Acklam's inverse normal, then a Newton step on erfc.
    const double p = 0.5 + confidence / 2;
    static const double a[] = {-3.969683028665376e+01, 2.209460984245205e+02, -2.759285104469687e+02,
                               1.383577518672690e+02,  -3.066479806614716e+01, 2.506628277459239e+00};
    static const double b[] = {-5.447609879822406e+01, 1.615858368580409e+02, -1.556989798598866e+02,
                               6.680131188771972e+01,  -1.328068155288572e+01};
    static const double c[] = {-7.784894002430293e-03, -3.223964580411365e-01, -2.400758277161838e+00,
                               -2.549732539343734e+00, 4.374664141464968e+00,  2.938163982698783e+00};
    static const double d[] = {7.784695709041462e-03, 3.224671290700398e-01, 2.445134137142996e+00,
                               3.754408661907416e+00};
    if (!(p > 0 && p < 1)) return NAN;
    double x;
    if (p < 0.02425) {
        const double q = std::sqrt(-2 * std::log(p));
        x = (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5]) /
            ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1);
    } else if (p <= 1 - 0.02425) {
        const double q = p - 0.5, r = q * q;
        x = (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q /
            (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1);
    } else {
        const double q = std::sqrt(-2 * std::log(1 - p));
        x = -(((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5]) /
            ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1);
    }
    const double e = 0.5 * std::erfc(-x / std::sqrt(2.0)) - p;
    const double u = e * std::sqrt(2 * kPi) * std::exp(x * x / 2);
    return x - u / (1 + x * u / 2);
}

Estimator::Estimator(const std::vector<double>& stratumArea, const std::vector<int>& stratumOfPoint, double confidence) {
    z = zOf(confidence);
    double total = 0;
    for (double a : stratumArea) total += std::max(0.0, a);
    std::vector<std::vector<int>> m(stratumArea.size());
    for (size_t i = 0; i < stratumOfPoint.size(); ++i)
        if (stratumOfPoint[i] >= 0 && stratumOfPoint[i] < int(m.size())) m[stratumOfPoint[i]].push_back(int(i));
    covered = 0;
    for (size_t h = 0; h < m.size(); ++h) {
        const double w = total > 0 ? stratumArea[h] / total : 0;
        if (w <= 0) continue;
        if (m[h].empty()) {
            missing.push_back(int(h));
            continue;
        }
        members_.push_back(std::move(m[h]));
        weight_.push_back(w);
        covered += w;
    }
    if (covered > 0 && covered < 0.999999)
        for (double& w : weight_) w /= covered;
}

Est Estimator::interval(double est, double se) const {
    return {est, se, est - z * se, est + z * se};
}

Est Estimator::mean(const std::vector<double>& y) const {
    double est = 0, var = 0;
    if (members_.empty()) return {};
    for (size_t g = 0; g < members_.size(); ++g) {
        const std::vector<int>& m = members_[g];
        double s = 0;
        for (int i : m) s += y[i];
        const double mu = s / m.size();
        double ss = 0;
        for (int i : m) ss += (y[i] - mu) * (y[i] - mu);
        est += weight_[g] * mu;
        if (m.size() > 1) var += weight_[g] * weight_[g] * (ss / (m.size() - 1)) / m.size();
    }
    return interval(est, std::sqrt(var));
}

Est Estimator::ratio(const std::vector<double>& y, const std::vector<double>& x) const {
    if (members_.empty()) return {};
    std::vector<double> my(members_.size()), mx(members_.size());
    double yb = 0, xb = 0;
    for (size_t g = 0; g < members_.size(); ++g) {
        double sy = 0, sx = 0;
        for (int i : members_[g]) {
            sy += y[i];
            sx += x[i];
        }
        my[g] = sy / members_[g].size();
        mx[g] = sx / members_[g].size();
        yb += weight_[g] * my[g];
        xb += weight_[g] * mx[g];
    }
    if (xb == 0) return {};
    const double r = yb / xb;
    double var = 0;
    for (size_t g = 0; g < members_.size(); ++g) {
        const std::vector<int>& m = members_[g];
        if (m.size() < 2) continue;
        double vy = 0, vx = 0, cxy = 0;
        for (int i : m) {
            vy += (y[i] - my[g]) * (y[i] - my[g]);
            vx += (x[i] - mx[g]) * (x[i] - mx[g]);
            cxy += (y[i] - my[g]) * (x[i] - mx[g]);
        }
        const double d = double(m.size() - 1);
        var += weight_[g] * weight_[g] * (vy / d + r * r * vx / d - 2 * r * cxy / d) / m.size();
    }
    return interval(r, std::sqrt(std::max(var / (xb * xb), 0.0)));
}

Assessment assess(const AssessInput& in) {
    Assessment a;
    a.classes = in.classes;
    const size_t C = in.classes.size(), N = in.stratum.size();
    std::vector<int> st(N, -1);
    for (size_t i = 0; i < N; ++i)
        if (in.stratum[i] >= 0 && in.map[i] >= 0 && in.ref[i] >= 0 && in.map[i] < int(C) && in.ref[i] < int(C)) {
            st[i] = in.stratum[i];
            ++a.n;
        }
    const Estimator e(in.stratumArea, st, in.confidence);
    a.covered = e.covered;
    a.missing = e.missing;
    for (double v : in.stratumArea) a.totalArea += std::max(0.0, v);
    a.confusion.assign(C, std::vector<double>(C, 0.0));
    a.counts.assign(C, std::vector<int>(C, 0));
    a.cells.assign(C, std::vector<std::vector<int>>(C));
    std::vector<double> y(N, 0.0), x(N, 0.0);
    auto fill = [&](std::vector<double>& v, const std::function<bool(size_t)>& f) {
        for (size_t i = 0; i < N; ++i) v[i] = st[i] >= 0 && f(i) ? 1.0 : 0.0;
    };
    for (size_t i = 0; i < N; ++i)
        if (st[i] >= 0) {
            ++a.counts[in.map[i]][in.ref[i]];
            a.cells[in.map[i]][in.ref[i]].push_back(int(i));
        }
    if (a.n == 0) {
        a.users.assign(C, {});
        a.producers.assign(C, {});
        a.area.assign(C, {});
        for (size_t c = 0; c < C && c < in.mapped.size(); ++c) a.area[c].mapped = in.mapped[c];
        return a;
    }
    for (size_t i = 0; i < C; ++i)
        for (size_t j = 0; j < C; ++j) {
            if (!a.counts[i][j]) continue;
            fill(y, [&](size_t k) { return in.map[k] == int(i) && in.ref[k] == int(j); });
            a.confusion[i][j] = e.mean(y).est;
        }
    fill(y, [&](size_t k) { return in.map[k] == in.ref[k]; });
    a.overall = e.mean(y);
    for (size_t c = 0; c < C; ++c) {
        fill(y, [&](size_t k) { return in.map[k] == int(c) && in.ref[k] == int(c); });
        fill(x, [&](size_t k) { return in.map[k] == int(c); });
        a.users.push_back(e.ratio(y, x));
        fill(x, [&](size_t k) { return in.ref[k] == int(c); });
        a.producers.push_back(e.ratio(y, x));
        const Est p = e.mean(x);
        ClassArea ar;
        ar.mapped = c < in.mapped.size() ? in.mapped[c] : 0;
        ar.est = p.est * a.totalArea;
        ar.se = p.se * a.totalArea;
        ar.ci = e.z * ar.se;
        ar.lo = ar.est - ar.ci;
        ar.hi = ar.est + ar.ci;
        a.area.push_back(ar);
    }
    return a;
}

DateAgreement dateAgreement(const Estimator& e, const std::vector<int>& mapYear, const std::vector<int>& refYear,
                            double tolerance) {
    DateAgreement d;
    d.tolerance = tolerance;
    const size_t N = mapYear.size();
    std::vector<double> both(N, 0.0), within(N, 0.0), diff(N, 0.0);
    for (size_t i = 0; i < N; ++i) {
        if (mapYear[i] <= 0 || refYear[i] <= 0) continue;
        both[i] = 1;
        const int dd = mapYear[i] - refYear[i];
        diff[i] = std::abs(dd);
        within[i] = std::abs(dd) <= tolerance;
        ++d.n;
        ++d.diffs[dd];
    }
    if (d.n) {
        d.within = e.ratio(within, both);
        d.meanDiff = e.ratio(diff, both);
    }
    return d;
}

Agreement cohen(const std::vector<std::string>& a, const std::vector<std::string>& b) {
    Agreement g;
    std::map<std::string, int> ca, cb;
    int same = 0;
    for (size_t i = 0; i < a.size() && i < b.size(); ++i) {
        if (a[i].empty() || b[i].empty()) continue;
        ++g.n;
        ++ca[a[i]];
        ++cb[b[i]];
        same += a[i] == b[i];
    }
    if (!g.n) return g;
    g.observed = double(same) / g.n;
    double pe = 0;
    for (const auto& [k, n] : ca) {
        const auto it = cb.find(k);
        if (it != cb.end()) pe += double(n) / g.n * double(it->second) / g.n;
    }
    g.kappa = pe < 1 ? (g.observed - pe) / (1 - pe) : NAN;
    return g;
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

std::string newProjectId() {
    std::random_device rd;
    const uint64_t t = uint64_t(std::chrono::high_resolution_clock::now().time_since_epoch().count());
    Rng r{(uint64_t(rd()) << 32) ^ rd() ^ t};
    char b[40];
    std::snprintf(b, sizeof(b), "%016llx%016llx", (unsigned long long)r.next(), (unsigned long long)r.next());
    return b;
}

namespace {

std::string sqlText(const std::string& s) {
    std::string o = "'";
    for (char c : s) {
        o += c;
        if (c == '\'') o += '\'';
    }
    return o + "'";
}

std::string eventsText(const std::vector<Event>& ev) {
    if (ev.empty()) return "";
    json e = json::array();
    for (const Event& v : ev) e.push_back({{"label", v.label}, {"date", v.date}});
    return e.dump();
}

std::vector<Event> eventsFrom(const std::string& s) {
    std::vector<Event> out;
    if (s.empty()) return out;
    try {
        for (const json& e : json::parse(s)) out.push_back({e.value("label", ""), e.value("date", "")});
    } catch (const std::exception&) {
    }
    return out;
}

struct Ds {
    GDALDataset* d = nullptr;
    ~Ds() {
        if (d) GDALClose(d);
    }
};

GDALDataset* openGpkg(const std::string& path, bool update, std::string& error) {
    CPLErrorReset();
    GDALDataset* d = GDALDataset::FromHandle(
        GDALOpenEx(path.c_str(), GDAL_OF_VECTOR | (update ? GDAL_OF_UPDATE : GDAL_OF_READONLY), nullptr, nullptr, nullptr));
    if (!d) error = lastError("cannot open " + path);
    return d;
}

void addField(OGRLayer* L, const char* name, OGRFieldType t) {
    OGRFieldDefn f(name, t);
    L->CreateField(&f);
}

bool writeStrataTable(GDALDataset* ds, const Project& p, std::string& error) {
    OGRLayer* L = ds->GetLayerByName("strata");
    if (!L) {
        error = "the project has no strata table";
        return false;
    }
    ds->ExecuteSQL("DELETE FROM strata", nullptr, nullptr);
    for (const Stratum& s : p.strata.strata) {
        OGRFeature f(L->GetLayerDefn());
        f.SetField("code", s.code);
        f.SetField("name", s.name.c_str());
        if (s.cls != kNone) f.SetField("class", s.cls);
        f.SetField("change", int(s.change));
        f.SetField("buffer", int(s.buffer));
        f.SetField("pixels", GIntBig(s.pixels));
        f.SetField("area", s.area);
        f.SetField("weight", s.weight);
        f.SetField("expected_ua", s.expectedUa);
        f.SetField("n", s.n);
        if (L->CreateFeature(&f) != OGRERR_NONE) {
            error = lastError("cannot write the strata table");
            return false;
        }
    }
    return true;
}

bool writeProjectTable(GDALDataset* ds, const Project& p, std::string& error) {
    OGRLayer* L = ds->GetLayerByName("project");
    if (!L) {
        error = "the project has no project table";
        return false;
    }
    ds->ExecuteSQL("DELETE FROM project", nullptr, nullptr);
    json maps = json::array();
    for (const AssessedMap& m : p.maps) maps.push_back({{"rule", m.rule.toJson()}, {"mapping", m.mapping}});
    json mapped = json::array();
    for (const auto& m : p.strata.mapped) {
        json o = json::object();
        for (const auto& [c, a] : m) o[std::to_string(c)] = a;
        mapped.push_back(o);
    }
    const std::string rel = fs::u8path(p.strataPath).lexically_relative(fs::u8path(p.path).parent_path()).u8string();
    const std::vector<std::pair<std::string, std::string>> kv = {
        {"format", "janus-validation 1"},
        {"id", p.id},
        {"spec", p.spec.toJson().dump()},
        {"grid", p.strata.grid.toJson().dump()},
        {"unit", p.strata.unit},
        {"seed", std::to_string(p.seed)},
        {"design", p.design.dump()},
        {"label_set", json(p.labelSet).dump()},
        {"maps", maps.dump()},
        {"mapped", mapped.dump()},
        {"strata_raster", rel.empty() ? p.strataPath : rel},
    };
    for (const auto& [k, v] : kv) {
        OGRFeature f(L->GetLayerDefn());
        f.SetField("key", k.c_str());
        f.SetField("value", v.c_str());
        if (L->CreateFeature(&f) != OGRERR_NONE) {
            error = lastError("cannot write the project table");
            return false;
        }
    }
    return true;
}

bool writePoints(GDALDataset* ds, const Project& p, const std::vector<Point>& pts, std::string& error) {
    OGRLayer* L = ds->GetLayerByName("samples");
    if (!L) {
        error = "the project has no samples layer";
        return false;
    }
    std::map<int, std::string> names;
    for (const Stratum& s : p.strata.strata) names[s.code] = s.name;
    for (const Point& q : pts) {
        OGRFeature f(L->GetLayerDefn());
        f.SetFID(q.id);
        f.SetField("sample_id", q.id);
        f.SetField("stratum", q.stratum);
        f.SetField("stratum_name", names[q.stratum].c_str());
        f.SetField("row", q.row);
        f.SetField("col", q.col);
        OGRPoint g(q.x, q.y);
        f.SetGeometry(&g);
        if (L->CreateFeature(&f) != OGRERR_NONE) {
            error = lastError("cannot write the sample points");
            return false;
        }
    }
    return true;
}

} // namespace

bool createProject(const Project& p, std::string& error) {
    GDALDriver* drv = GetGDALDriverManager()->GetDriverByName("GPKG");
    if (!drv) {
        error = "GDAL has no GeoPackage driver";
        return false;
    }
    std::error_code ec;
    fs::remove(fs::u8path(p.path), ec);
    CPLErrorReset();
    Ds ds;
    ds.d = drv->Create(p.path.c_str(), 0, 0, 0, GDT_Unknown, nullptr);
    if (!ds.d) {
        error = lastError("cannot create " + p.path);
        return false;
    }
    OGRSpatialReference srs;
    const bool hasSrs = !p.strata.grid.wkt.empty() && srs.importFromWkt(p.strata.grid.wkt.c_str()) == OGRERR_NONE;
    if (hasSrs) srs.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    OGRLayer* s = ds.d->CreateLayer("samples", hasSrs ? &srs : nullptr, wkbPoint, nullptr);
    OGRLayer* l = ds.d->CreateLayer("labels", nullptr, wkbNone, nullptr);
    OGRLayer* t = ds.d->CreateLayer("strata", nullptr, wkbNone, nullptr);
    OGRLayer* k = ds.d->CreateLayer("project", nullptr, wkbNone, nullptr);
    if (!s || !l || !t || !k) {
        error = lastError("cannot create the project's tables");
        return false;
    }
    addField(s, "sample_id", OFTInteger);
    addField(s, "stratum", OFTInteger);
    addField(s, "stratum_name", OFTString);
    addField(s, "row", OFTInteger);
    addField(s, "col", OFTInteger);
    addField(s, "label", OFTString);
    addField(s, "ref_date", OFTString);
    addField(s, "confidence", OFTInteger);
    addField(s, "notes", OFTString);
    addField(s, "events", OFTString);
    addField(s, "interpreters", OFTInteger);
    addField(s, "status", OFTString);
    addField(l, "sample_id", OFTInteger);
    addField(l, "interpreter", OFTString);
    addField(l, "label", OFTString);
    addField(l, "date", OFTString);
    addField(l, "confidence", OFTInteger);
    addField(l, "notes", OFTString);
    addField(l, "events", OFTString);
    addField(l, "updated", OFTString);
    addField(t, "code", OFTInteger);
    addField(t, "name", OFTString);
    addField(t, "class", OFTInteger);
    addField(t, "change", OFTInteger);
    addField(t, "buffer", OFTInteger);
    addField(t, "pixels", OFTInteger64);
    addField(t, "area", OFTReal);
    addField(t, "weight", OFTReal);
    addField(t, "expected_ua", OFTReal);
    addField(t, "n", OFTInteger);
    addField(k, "key", OFTString);
    addField(k, "value", OFTString);
    ds.d->StartTransaction();
    if (!writePoints(ds.d, p, p.points, error) || !writeStrataTable(ds.d, p, error) || !writeProjectTable(ds.d, p, error)) {
        ds.d->RollbackTransaction();
        return false;
    }
    if (ds.d->CommitTransaction() != OGRERR_NONE) {
        error = lastError("cannot write " + p.path);
        return false;
    }
    ds.d->ExecuteSQL("CREATE INDEX labels_point ON labels(sample_id, interpreter)", nullptr, nullptr);
    ds.d->ExecuteSQL("CREATE INDEX samples_id ON samples(sample_id)", nullptr, nullptr);
    GDALClose(ds.d);
    ds.d = nullptr;
    // Labels already given (a project written again, e.g. merged).
    std::vector<FinalRef> finals;
    for (size_t i = 0; i < p.points.size(); ++i) {
        bool conflict = false;
        const Label* r = resolve(p.labels, i, conflict);
        FinalRef f;
        f.point = int(i);
        if (r) f.ref = *r;
        f.conflict = conflict;
        for (const auto& [w, v] : p.labels) f.interpreters += i < v.size() && !v[i].label.empty();
        if (r || conflict) finals.push_back(f);
    }
    bool first = true;
    for (const auto& [who, v] : p.labels) {
        std::vector<std::pair<int, Label>> rows;
        for (size_t i = 0; i < v.size(); ++i)
            if (!v[i].empty()) rows.push_back({int(i), v[i]});
        if (rows.empty()) continue;
        if (!saveLabels(p.path, who, rows, first ? finals : std::vector<FinalRef>{}, error)) return false;
        first = false;
    }
    return true;
}

bool saveProjectMeta(const Project& p, std::string& error) {
    Ds ds;
    if (!(ds.d = openGpkg(p.path, true, error))) return false;
    ds.d->StartTransaction();
    if (!writeStrataTable(ds.d, p, error) || !writeProjectTable(ds.d, p, error)) {
        ds.d->RollbackTransaction();
        return false;
    }
    if (ds.d->CommitTransaction() != OGRERR_NONE) {
        error = lastError("cannot write " + p.path);
        return false;
    }
    return true;
}

bool appendPoints(const Project& p, const std::vector<Point>& pts, std::string& error) {
    Ds ds;
    if (!(ds.d = openGpkg(p.path, true, error))) return false;
    ds.d->StartTransaction();
    if (!writePoints(ds.d, p, pts, error) || !writeStrataTable(ds.d, p, error)) {
        ds.d->RollbackTransaction();
        return false;
    }
    if (ds.d->CommitTransaction() != OGRERR_NONE) {
        error = lastError("cannot write " + p.path);
        return false;
    }
    return true;
}

bool saveLabels(const std::string& path, const std::string& interpreter, const std::vector<std::pair<int, Label>>& rows,
                const std::vector<FinalRef>& finals, std::string& error) {
    Ds ds;
    if (!(ds.d = openGpkg(path, true, error))) return false;
    OGRLayer* L = ds.d->GetLayerByName("labels");
    if (!L) {
        error = path + ": no labels table";
        return false;
    }
    ds.d->StartTransaction();
    if (!rows.empty()) {
        std::string ids;
        for (const auto& [i, l] : rows) ids += (ids.empty() ? "" : ",") + std::to_string(i + 1);
        ds.d->ExecuteSQL(("DELETE FROM labels WHERE interpreter = " + sqlText(interpreter) + " AND sample_id IN (" + ids + ")").c_str(),
                         nullptr, nullptr);
    }
    for (const auto& [i, l] : rows) {
        if (l.empty()) continue;
        OGRFeature f(L->GetLayerDefn());
        f.SetField("sample_id", i + 1);
        f.SetField("interpreter", interpreter.c_str());
        f.SetField("label", l.label.c_str());
        f.SetField("date", l.date.c_str());
        f.SetField("confidence", l.confidence);
        f.SetField("notes", l.notes.c_str());
        f.SetField("events", eventsText(l.events).c_str());
        f.SetField("updated", l.updated.c_str());
        if (L->CreateFeature(&f) != OGRERR_NONE) {
            ds.d->RollbackTransaction();
            error = lastError("cannot write the labels");
            return false;
        }
    }
    for (const FinalRef& f : finals) {
        const Label& r = f.ref;
        const std::string status = f.conflict ? "conflict" : r.label.empty() ? "" : "labelled";
        const std::string sql = "UPDATE samples SET label = " + (r.label.empty() ? std::string("NULL") : sqlText(r.label)) +
                                ", ref_date = " + (r.date.empty() ? std::string("NULL") : sqlText(r.date)) +
                                ", confidence = " + (r.confidence ? std::to_string(r.confidence) : std::string("NULL")) +
                                ", notes = " + (r.notes.empty() ? std::string("NULL") : sqlText(r.notes)) +
                                ", events = " + (r.events.empty() ? std::string("NULL") : sqlText(eventsText(r.events))) +
                                ", interpreters = " + std::to_string(f.interpreters) + ", status = " +
                                (status.empty() ? std::string("NULL") : sqlText(status)) +
                                " WHERE sample_id = " + std::to_string(f.point + 1);
        ds.d->ExecuteSQL(sql.c_str(), nullptr, nullptr);
    }
    if (ds.d->CommitTransaction() != OGRERR_NONE) {
        error = lastError("cannot write the labels to " + path);
        return false;
    }
    return true;
}

namespace {

std::map<std::string, std::string> readKeys(GDALDataset* ds) {
    std::map<std::string, std::string> kv;
    if (OGRLayer* L = ds->GetLayerByName("project")) {
        L->ResetReading();
        while (OGRFeature* f = L->GetNextFeature()) {
            kv[f->GetFieldAsString("key")] = f->GetFieldAsString("value");
            OGRFeature::DestroyFeature(f);
        }
    }
    return kv;
}

bool readLabelTable(GDALDataset* ds, size_t points, std::map<std::string, std::vector<Label>>& out) {
    OGRLayer* L = ds->GetLayerByName("labels");
    if (!L) return false;
    L->ResetReading();
    while (OGRFeature* f = L->GetNextFeature()) {
        const int id = f->GetFieldAsInteger("sample_id");
        const std::string who = f->GetFieldAsString("interpreter");
        if (id >= 1 && size_t(id) <= points && !who.empty()) {
            std::vector<Label>& v = out[who];
            v.resize(points);
            Label& l = v[id - 1];
            l.label = f->GetFieldAsString("label");
            l.date = f->GetFieldAsString("date");
            l.confidence = f->GetFieldAsInteger("confidence");
            l.notes = f->GetFieldAsString("notes");
            l.events = eventsFrom(f->GetFieldAsString("events"));
            l.updated = f->GetFieldAsString("updated");
        }
        OGRFeature::DestroyFeature(f);
    }
    return true;
}

} // namespace

bool loadProject(const std::string& path, Project& out, std::string& error) {
    Ds ds;
    if (!(ds.d = openGpkg(path, false, error))) return false;
    const auto kv = readKeys(ds.d);
    const auto get = [&](const char* k) {
        const auto it = kv.find(k);
        return it == kv.end() ? std::string() : it->second;
    };
    if (get("format").rfind("janus-validation", 0) != 0) {
        error = path + " is not a validation project of Janus";
        return false;
    }
    Project p;
    p.path = fs::absolute(fs::u8path(path)).lexically_normal().u8string();
    try {
        p.id = get("id");
        p.spec = StrataSpec::fromJson(json::parse(get("spec")));
        p.strata.grid = Grid::fromJson(json::parse(get("grid")));
        p.strata.unit = get("unit");
        p.seed = std::strtoull(get("seed").c_str(), nullptr, 10);
        const std::string d = get("design");
        p.design = d.empty() ? json::object() : json::parse(d);
        const std::string ls = get("label_set");
        if (!ls.empty()) p.labelSet = json::parse(ls).get<std::vector<std::string>>();
        const std::string mp = get("maps");
        for (const json& m : mp.empty() ? json::array() : json::parse(mp)) {
            AssessedMap a;
            a.rule = MapRule::fromJson(m.value("rule", json::object()));
            a.mapping = m.value("mapping", std::map<std::string, std::string>());
            p.maps.push_back(std::move(a));
        }
        const std::string ma = get("mapped");
        for (const json& m : ma.empty() ? json::array() : json::parse(ma)) {
            std::map<int, double> o;
            for (const auto& [c, a] : m.items()) o[std::atoi(c.c_str())] = a.get<double>();
            p.strata.mapped.push_back(o);
        }
    } catch (const std::exception& e) {
        error = path + ": damaged project table (" + e.what() + ")";
        return false;
    }
    const fs::path raster = fs::u8path(get("strata_raster"));
    p.strataPath = (raster.is_absolute() ? raster : fs::u8path(p.path).parent_path() / raster).lexically_normal().u8string();
    std::string unit;
    p.strata.rowArea = rowAreas(p.strata.grid, unit);
    if (OGRLayer* L = ds.d->GetLayerByName("strata")) {
        L->ResetReading();
        while (OGRFeature* f = L->GetNextFeature()) {
            Stratum s;
            s.code = f->GetFieldAsInteger("code");
            s.name = f->GetFieldAsString("name");
            s.cls = f->IsFieldSetAndNotNull(f->GetFieldIndex("class")) ? f->GetFieldAsInteger("class") : kNone;
            s.change = f->GetFieldAsInteger("change") != 0;
            s.buffer = f->GetFieldAsInteger("buffer") != 0;
            s.pixels = f->GetFieldAsInteger64("pixels");
            s.area = f->GetFieldAsDouble("area");
            s.weight = f->GetFieldAsDouble("weight");
            s.expectedUa = f->GetFieldAsDouble("expected_ua");
            s.n = f->GetFieldAsInteger("n");
            p.strata.strata.push_back(s);
            OGRFeature::DestroyFeature(f);
        }
    }
    OGRLayer* S = ds.d->GetLayerByName("samples");
    if (!S || p.strata.strata.empty()) {
        error = path + ": no samples layer or no strata";
        return false;
    }
    std::vector<Point> pts;
    S->ResetReading();
    while (OGRFeature* f = S->GetNextFeature()) {
        Point q;
        q.id = f->GetFieldAsInteger("sample_id");
        q.stratum = f->GetFieldAsInteger("stratum");
        q.row = f->GetFieldAsInteger("row");
        q.col = f->GetFieldAsInteger("col");
        if (const OGRGeometry* g = f->GetGeometryRef(); g && wkbFlatten(g->getGeometryType()) == wkbPoint) {
            q.x = g->toPoint()->getX();
            q.y = g->toPoint()->getY();
        } else {
            p.strata.grid.centre(q.col, q.row, q.x, q.y);
        }
        pts.push_back(q);
        OGRFeature::DestroyFeature(f);
    }
    std::sort(pts.begin(), pts.end(), [](const Point& a, const Point& b) { return a.id < b.id; });
    for (size_t i = 0; i < pts.size(); ++i)
        if (pts[i].id != int(i) + 1) {
            error = path + ": the sample ids must run from 1 to the number of points (was the samples layer edited?)";
            return false;
        }
    p.points = std::move(pts);
    readLabelTable(ds.d, p.points.size(), p.labels);
    for (auto& [who, v] : p.labels) v.resize(p.points.size());
    out = std::move(p);
    return true;
}

bool isProject(const std::string& path) {
    std::string ext = fs::u8path(path).extension().u8string();
    for (char& c : ext) c = char(std::tolower(static_cast<unsigned char>(c)));
    if (ext != ".gpkg") return false;
    std::string e;
    Ds ds;
    if (!(ds.d = openGpkg(path, false, e)) || !ds.d->GetLayerByName("project")) return false;
    const auto kv = readKeys(ds.d);
    const auto it = kv.find("format");
    return it != kv.end() && it->second.rfind("janus-validation", 0) == 0;
}

bool readLabels(const std::string& path, size_t points, std::string& projectId,
                std::map<std::string, std::vector<Label>>& out, std::string& error) {
    Ds ds;
    if (!(ds.d = openGpkg(path, false, error))) return false;
    const auto kv = readKeys(ds.d);
    const auto it = kv.find("id");
    projectId = it == kv.end() ? "" : it->second;
    if (!readLabelTable(ds.d, points, out)) {
        error = path + ": no labels table";
        return false;
    }
    return true;
}

// ---------------------------------------------------------------------------
// Assessing a map of the project
// ---------------------------------------------------------------------------

const char* const kChange = "change";
const char* const kNoChange = "no change";

namespace {

std::string lower(std::string s) {
    for (char& c : s) c = char(std::tolower(static_cast<unsigned char>(c)));
    return s;
}

bool hasAny(const std::string& s, std::initializer_list<const char*> words) {
    for (const char* w : words)
        if (s.find(w) != std::string::npos) return true;
    return false;
}

bool cannotTell(const std::string& l) {
    return hasAny(l, {"can't", "cant", "cannot", "can not", "unsure", "unknown", "cloud", "no data", "nodata", "n/a"});
}

// Years of the changes of a point's labels that the map calls change (sorted); `any`: a change label at all.
std::vector<int> changeYears(const Label& L, const std::map<std::string, std::string>& mapping, bool& any) {
    std::vector<int> ys;
    any = false;
    auto add = [&](const std::string& label, const std::string& date) {
        const auto it = mapping.find(label);
        if (it == mapping.end() || it->second != kChange) return;
        any = true;
        if (const int y = yearOf(date)) ys.push_back(y);
    };
    add(L.label, L.date);
    for (const Event& e : L.events) add(e.label, e.date);
    std::sort(ys.begin(), ys.end());
    return ys;
}

} // namespace

std::map<std::string, std::string> defaultMapping(const MapRule& r, const std::vector<std::string>& labelSet,
                                                  const std::vector<std::string>& classNames) {
    std::map<std::string, std::string> m;
    for (const std::string& label : labelSet) {
        const std::string l = lower(label);
        if (cannotTell(l)) {
            m[label] = "";
            continue;
        }
        if (r.kind == MapKind::Events) {
            const bool stable = hasAny(l, {"no change", "stable", "unchanged", "persist"});
            const bool change = hasAny(l, {"deforest", "degrad", "loss", "clear", "fire", "burn", "harvest", "logging",
                                           "mining", "change", "disturb", "cut", "conver", "drought", "flood"});
            m[label] = change && !stable ? kChange : kNoChange;
            continue;
        }
        for (const std::string& c : classNames)
            if (lower(c) == l) m[label] = c;
    }
    return m;
}

std::vector<int> mapClasses(const MapRule& r, const std::vector<double>& values, const std::map<int, double>& mapped) {
    std::vector<int> k;
    switch (r.kind) {
    case MapKind::Classes: {
        std::set<int> s;
        for (const auto& [c, a] : mapped)
            if (a > 0 && c != kNone) s.insert(c);
        for (double v : values)
            if (const int c = r.classOf(v); c != kNone) s.insert(c);
        k.assign(s.begin(), s.end());
        break;
    }
    case MapKind::Intervals:
        for (int i = 1; i < int(r.bins.size()); ++i) k.push_back(i);
        break;
    case MapKind::Events:
        k.push_back(0);
        for (int i = 1; i < std::max(2, int(r.bins.size())); ++i) k.push_back(i);
        break;
    }
    return k;
}

Prepared prepare(const Project& p, const AssessedMap& m, const std::vector<double>& values,
                 const std::map<int, double>& mapped, double confidence, const std::vector<char>& use) {
    Prepared o;
    const MapRule& r = m.rule;
    const size_t N = p.points.size();
    o.classKeys = mapClasses(r, values, mapped);
    std::map<std::string, int> idx;
    std::map<int, int> idxOfKey;
    for (int k : o.classKeys) {
        idxOfKey[k] = int(o.in.classes.size());
        idx[r.className(k)] = int(o.in.classes.size());
        o.in.classes.push_back(r.className(k));
        const auto it = mapped.find(k);
        o.in.mapped.push_back(it == mapped.end() ? 0.0 : it->second);
    }
    auto classIndex = [&](const std::string& name) {
        const auto it = idx.find(name);
        if (it != idx.end()) return it->second;
        idx[name] = int(o.in.classes.size());
        o.in.classes.push_back(name);
        o.in.mapped.push_back(0);
        o.classKeys.push_back(kNone);
        return int(o.in.classes.size()) - 1;
    };
    o.in.confidence = confidence;
    for (const Stratum& s : p.strata.strata) o.in.stratumArea.push_back(s.area);
    o.in.stratum.assign(N, -1);
    o.in.map.assign(N, -1);
    o.in.ref.assign(N, -1);
    o.mapYear.assign(N, 0);
    o.refYear.assign(N, 0);
    for (size_t i = 0; i < N; ++i) {
        if (!use.empty() && (i >= use.size() || !use[i])) continue;
        o.in.stratum[i] = p.strata.indexOfCode(p.points[i].stratum);
        const double v = i < values.size() ? values[i] : NAN;
        const int c = r.classOf(v);
        if (c != kNone) {
            const auto it = idxOfKey.find(c);
            o.in.map[i] = it == idxOfKey.end() ? -1 : it->second;
        }
        if (r.kind == MapKind::Events && std::isfinite(v) && v > 0) o.mapYear[i] = int(std::floor(v));
        bool conflict = false;
        const Label* L = resolve(p.labels, i, conflict);
        if (!L) {
            ++(conflict ? o.conflicts : o.unlabelled);
            continue;
        }
        ++o.labelled;
        if (o.in.map[i] < 0) ++o.offMap;
        const auto tr = m.mapping.find(L->label);
        if (r.kind == MapKind::Events) {
            bool any = false;
            const std::vector<int> ys = changeYears(*L, m.mapping, any);
            if (!any) {
                if (tr == m.mapping.end()) {
                    ++o.unmapped;
                    o.unmappedLabels.insert(L->label);
                    continue;
                }
                if (tr->second.empty()) {
                    ++o.excluded;
                    continue;
                }
                o.in.ref[i] = classIndex(kNoChange);
                continue;
            }
            if (r.bins.empty()) {
                o.in.ref[i] = classIndex(kChange);
                o.refYear[i] = ys.empty() ? 0 : ys.front();
                continue;
            }
            if (ys.empty()) {
                ++o.noDate;
                continue;
            }
            int period = kNone, year = 0;
            for (int y : ys)
                if (const int k = r.classOf(double(y)); k != kNone && k > 0) {
                    period = k;
                    year = y;
                    break;
                }
            if (period == kNone) { // changes, but none in the map's periods
                o.in.ref[i] = classIndex(kNoChange);
                continue;
            }
            o.in.ref[i] = classIndex(r.className(period));
            o.refYear[i] = year;
            continue;
        }
        std::string target;
        if (tr != m.mapping.end()) {
            target = tr->second;
            if (target.empty()) {
                ++o.excluded;
                continue;
            }
        } else if (idx.count(L->label)) {
            target = L->label;
        } else {
            ++o.unmapped;
            o.unmappedLabels.insert(L->label);
            continue;
        }
        o.in.ref[i] = classIndex(target);
    }
    // A point without a reference or off the map does not count.
    for (size_t i = 0; i < N; ++i)
        if (o.in.ref[i] < 0 || o.in.map[i] < 0) o.in.stratum[i] = -1;
    return o;
}

std::vector<std::pair<int, ClassArea>> changeByYear(const Project& p, const AssessedMap& m, double confidence) {
    std::vector<std::pair<int, ClassArea>> out;
    const size_t N = p.points.size();
    std::vector<int> st(N, -1), year(N, 0);
    int y0 = INT_MAX, y1 = INT_MIN;
    const bool period = m.rule.kind == MapKind::Events && m.rule.bins.size() >= 2;
    for (size_t i = 0; i < N; ++i) {
        bool conflict = false;
        const Label* L = resolve(p.labels, i, conflict);
        if (!L) continue;
        bool any = false;
        const std::vector<int> ys = changeYears(*L, m.mapping, any);
        if (any && ys.empty()) continue; // a change without a date: unknown year
        if (!any) {
            const auto tr = m.mapping.find(L->label);
            if (tr == m.mapping.end() || tr->second.empty()) continue;
        }
        st[i] = p.strata.indexOfCode(p.points[i].stratum);
        for (int y : ys)
            if (!period || (y >= m.rule.bins.front() && y < m.rule.bins.back())) {
                year[i] = y;
                y0 = std::min(y0, y);
                y1 = std::max(y1, y);
                break;
            }
    }
    if (y0 > y1) return out;
    std::vector<double> area;
    for (const Stratum& s : p.strata.strata) area.push_back(s.area);
    const Estimator e(area, st, confidence);
    const double total = p.strata.totalArea();
    std::vector<double> ind(N);
    for (int y = y0; y <= y1; ++y) {
        for (size_t i = 0; i < N; ++i) ind[i] = year[i] == y;
        const Est v = e.mean(ind);
        ClassArea a;
        a.est = v.est * total;
        a.se = v.se * total;
        a.ci = e.z * a.se;
        a.lo = a.est - a.ci;
        a.hi = a.est + a.ci;
        out.push_back({y, a});
    }
    return out;
}

namespace {

std::string pyStr(const std::string& s) {
    std::string o = "\"";
    for (char c : s) {
        if (c == '\\' || c == '"') o += '\\';
        if (c == '\n') {
            o += "\\n";
            continue;
        }
        o += c;
    }
    return o + "\"";
}

std::string pyNum(double v) {
    if (std::isinf(v)) return v < 0 ? "float(\"-inf\")" : "float(\"inf\")";
    char b[40];
    std::snprintf(b, sizeof(b), "%.17g", v);
    return b;
}

std::string pyPath(const std::string& p) {
    return pyStr(fs::u8path(p).generic_u8string());
}

} // namespace

std::string pythonSnippet(const Project& p, const AssessedMap& m, double confidence) {
    const MapRule& r = m.rule;
    std::string s;
    s += "# Accuracy and error-adjusted areas of " + (r.name.empty() ? r.path : r.name) +
         " (Olofsson et al. 2014; Stehman 2014),\n# from the Janus validation project " +
         fs::u8path(p.path).filename().u8string() + "\n";
    s += "import sqlite3\n\nimport geopandas as gpd\nimport pandas as pd\nimport rioxarray\n";
    if (r.kind == MapKind::Events) s += "import xarray as xr\n";
    s += "import zeit\n\n";
    s += "# The sample points (a GeoPackage is SQLite), at their pixels' centres in the strata raster\n";
    s += "with sqlite3.connect(" + pyPath(p.path) + ") as db:\n";
    s += "    samples = pd.read_sql(\"SELECT sample_id, stratum, row, col, label, ref_date, status FROM samples\", db)\n";
    s += "strata = rioxarray.open_rasterio(" + pyPath(p.strataPath) + ").squeeze(drop=True)\n";
    s += "x, y = strata.rio.transform() * (samples[\"col\"] + 0.5, samples[\"row\"] + 0.5)\n";
    s += "samples = gpd.GeoDataFrame(samples, geometry=gpd.points_from_xy(x, y), crs=strata.rio.crs)\n";
    s += "samples = samples[samples[\"status\"] == \"labelled\"]  # an agreed (or adjudicated) reference label\n";
    if (r.kind == MapKind::Events) {
        // Change comes from any of a point's events, and its period from their dates: the reference as Janus reads it.
        const Prepared pr = prepare(p, m, std::vector<double>(p.points.size(), NAN), {}, confidence);
        s += "# The reference class of each point: its labels (and dates) translated to the map's periods\n";
        s += "reference = {";
        int k = 0;
        for (size_t i = 0; i < p.points.size(); ++i) {
            if (pr.in.ref[i] < 0) continue;
            s += (k++ ? ", " : "") + std::to_string(p.points[i].id) + ": " + pyStr(pr.in.classes[pr.in.ref[i]]);
            if (k % 8 == 0) s += "\n             ";
        }
        s += "}\nsamples[\"ref\"] = samples[\"sample_id\"].map(reference)\n";
    } else {
        s += "to_map = {  # reference label -> the map's class\n";
        for (const auto& [label, target] : m.mapping) {
            if (target.empty()) {
                s += "    " + pyStr(label) + ": None,\n";
                continue;
            }
            std::string v = pyStr(target);
            if (r.kind == MapKind::Classes)
                for (const auto& [code, name] : r.names)
                    if (name == target) v = std::to_string(code);
            if (r.kind == MapKind::Classes && r.names.empty()) {
                char* end = nullptr;
                const long code = std::strtol(target.c_str(), &end, 10);
                if (end && !*end) v = std::to_string(code);
            }
            s += "    " + pyStr(label) + ": " + v + ",\n";
        }
        s += "}\nsamples[\"ref\"] = samples[\"label\"].map(to_map)\n";
    }
    s += "samples = samples[samples[\"ref\"].notna()]\n\n";
    s += "the_map = rioxarray.open_rasterio(" + pyPath(r.path) + ", masked=True).sel(band=" + std::to_string(r.band) +
         ", drop=True)\n";
    std::string bins;
    if (r.bins.size() >= 2) {
        bins = ", bins=[";
        for (size_t i = 0; i < r.bins.size(); ++i) bins += (i ? ", " : "") + pyNum(r.bins[i]);
        bins += "]";
    }
    char conf[32];
    std::snprintf(conf, sizeof(conf), "%g", confidence);
    if (r.kind == MapKind::Events)
        s += "the_map = xr.Dataset({\"yod\": the_map.fillna(0)})  # a map of events: > 0 change\n";
    s += "acc = zeit.accuracy(the_map, samples, strata=" + pyPath(p.strataPath) + bins + ", confidence=" + conf + ")\n";
    s += "print(acc)\n";
    return s;
}

} // namespace val
