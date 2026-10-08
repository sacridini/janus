#include "basemap.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <mutex>
#include <unordered_set>

#include <cpl_conv.h>
#include <cpl_error.h>
#include <gdal_priv.h>
#include <ogr_spatialref.h>

#include "job_pool.hpp"
#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

constexpr double kE = 20037508.342789244; // half the width of the Web Mercator world (m)
constexpr double kMargin = 1.5;           // grid around the active layer, x its longer side
constexpr int kMaxTiles = 300;            // per view and level (a 4K screen needs ~160)
constexpr double kRetryMs = 15000;        // a tile that failed (no connection) is asked for again after
constexpr double kRetryHttpMs = 300000;   // ... or after a server error (a wrong URL, key)
std::atomic<int> g_opened{0};

double nowMs() {
    return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

uint64_t tileKey(int z, int x, int y) { return (uint64_t(z) << 44) | (uint64_t(x) << 22) | uint64_t(y); }

void replaceAll(std::string& s, const std::string& from, const std::string& to) {
    for (size_t p = s.find(from); p != std::string::npos; p = s.find(from, p + to.size())) s.replace(p, from.size(), to);
}

std::string xmlEscape(const std::string& s) {
    std::string out;
    for (char c : s) {
        if (c == '&') out += "&amp;";
        else if (c == '<') out += "&lt;";
        else if (c == '>') out += "&gt;";
        else if (c == '"') out += "&quot;";
        else out += c;
    }
    return out;
}

bool isFileUrl(const std::string& url) { return url.rfind("file://", 0) == 0; }

// GDAL WMS description of an XYZ source: the TMS service over the whole Web
// Mercator world, one overview per zoom level, RGBA (JPEG tiles get an opaque
// alpha; tiles the server does not have, 204/404, are transparent).
std::string wmsXml(const BasemapSource& s, const std::string& cacheDir) {
    std::string url = s.url;
    const bool fromBottom = url.find("{-y}") != std::string::npos; // TMS rows
    replaceAll(url, "{-y}", "${y}");
    replaceAll(url, "{z}", "${z}");
    replaceAll(url, "{x}", "${x}");
    replaceAll(url, "{y}", "${y}");
    replaceAll(url, "{s}", "a");
    const std::string ts = std::to_string(s.tileSize);
    std::string x = "<GDAL_WMS><Service name=\"TMS\"><ServerUrl>" + xmlEscape(url) +
                    "</ServerUrl></Service><DataWindow>"
                    "<UpperLeftX>-20037508.342789244</UpperLeftX><UpperLeftY>20037508.342789244</UpperLeftY>"
                    "<LowerRightX>20037508.342789244</LowerRightX><LowerRightY>-20037508.342789244</LowerRightY>"
                    "<TileLevel>" + std::to_string(s.maxZoom) + "</TileLevel><TileCountX>1</TileCountX>"
                    "<TileCountY>1</TileCountY><YOrigin>" + (fromBottom ? "bottom" : "top") + "</YOrigin></DataWindow>"
                    "<Projection>EPSG:3857</Projection><BlockSizeX>" + ts + "</BlockSizeX><BlockSizeY>" + ts +
                    "</BlockSizeY><BandsCount>4</BandsCount>"
                    "<UserAgent>Janus/" JANUS_VERSION " (+https://github.com/sacridini/janus)</UserAgent>"
                    "<Timeout>20</Timeout><ZeroBlockHttpCodes>204,404</ZeroBlockHttpCodes>";
    if (!isFileUrl(s.url)) // local tiles need no copy
        x += "<Cache><Path>" + xmlEscape(cacheDir) + "</Path><Expires>" + std::to_string(s.cacheDays * 86400) +
             "</Expires><MaxSize>536870912</MaxSize></Cache>";
    return x + "</GDAL_WMS>";
}

// TLS certificates for libcurl: the user's setting if any, else the system's
// bundle (Linux, macOS: the conda-built curl looks for one at its build
// prefix), else the one shipped in share/ssl. Empty: curl's default (Windows:
// Schannel and the system store).
std::string caBundle() {
    for (const char* k : {"CURL_CA_BUNDLE", "GDAL_CURL_CA_BUNDLE", "SSL_CERT_FILE"})
        if (CPLGetConfigOption(k, nullptr)) return "";
    std::string path = platform::caBundlePath();
    std::error_code ec;
    const fs::path shipped = fs::u8path(platform::resourceDir()) / "share" / "ssl" / "cacert.pem";
    if (path.empty() && fs::exists(shipped, ec)) path = shipped.u8string();
    return path;
}

} // namespace

const std::vector<BasemapSource>& basemapPresets() {
    static const std::vector<BasemapSource> p = {
        {"esri", "Esri World Imagery",
         "https://server.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}", 18, 256,
         "Powered by Esri | Source: Esri, Vantor, Earthstar Geographics, and the GIS User Community",
         "Esri's terms of use apply (no key; the attribution must be shown)", 4, 30},
        {"eox", "Sentinel-2 cloudless 2016 (EOX)",
         "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless_3857/default/g/{z}/{y}/{x}.jpg", 15, 256,
         "Sentinel-2 cloudless 2016 - https://cloudless.eox.at by EOX IT Services GmbH "
         "(Contains modified Copernicus Sentinel data 2016)",
         "CC BY 4.0 (the 2016 mosaic; EOX's later years are non-commercial only)", 4, 90},
        {"osm", "OpenStreetMap", "https://tile.openstreetmap.org/{z}/{x}/{y}.png", 19, 256,
         "\xC2\xA9 OpenStreetMap contributors",
         "ODbL; OSM's tile usage policy: light use only (2 connections, tiles cached)", 2, 7},
    };
    return p;
}

std::string basemapUrlProblem(const std::string& url) {
    if (url.empty()) return "type the tile URL";
    if (url.rfind("https://", 0) != 0 && url.rfind("http://", 0) != 0 && !isFileUrl(url))
        return "the URL must start with https://, http:// or file://";
    if (url.find("{z}") == std::string::npos || url.find("{x}") == std::string::npos ||
        (url.find("{y}") == std::string::npos && url.find("{-y}") == std::string::npos))
        return "the URL needs {z}, {x} and {y}";
    return "";
}

// ---------------------------------------------------------------------------
// Reader threads
// ---------------------------------------------------------------------------

struct Basemap::Shared {
    std::string xml;
    std::string caBundle;
    int maxZoom = 19, tileSize = 256;
    std::function<void()> wake;

    std::mutex m;
    std::unordered_map<uint64_t, uint64_t> wanted; // key -> last frame it was in view
    std::unordered_set<uint64_t> inflight;
    std::unordered_map<uint64_t, double> failed;   // key -> when it may be asked for again (ms)
    struct Result {
        uint64_t key;
        std::vector<unsigned char> rgba;
        double ms;
    };
    std::vector<Result> results;
    std::string error;                             // the last server error
    std::atomic<uint64_t> frame{0};
    std::atomic<bool> offline{false};

    // A dataset per concurrent request: GDAL datasets are not thread-safe.
    std::mutex dsM;
    std::vector<GDALDataset*> idle;

    ~Shared() {
        for (GDALDataset* d : idle) GDALClose(d);
    }
    GDALDataset* acquire() {
        {
            std::lock_guard<std::mutex> lk(dsM);
            if (!idle.empty()) {
                GDALDataset* d = idle.back();
                idle.pop_back();
                return d;
            }
        }
        const char* const drivers[] = {"WMS", nullptr};
        GDALDataset* d = GDALDataset::Open(xml.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY, drivers);
        if (d) ++g_opened;
        return d;
    }
    void release(GDALDataset* d) {
        if (!d) return;
        std::lock_guard<std::mutex> lk(dsM);
        idle.push_back(d);
    }
};

namespace {

// Tile (x, y) of zoom z as RGBA rows: one block of the overview of that level
// (GDAL fetches all four bands of it in one request). `http`: the status of a
// failed request (0: the server was not reached).
bool readTile(GDALDataset* ds, int maxZoom, int ts, int z, int x, int y, unsigned char* rgba, int& http,
              std::string& error) {
    GDALRasterBand* bands[4];
    for (int b = 0; b < 4; ++b) {
        GDALRasterBand* band = ds->GetRasterBand(b + 1);
        if (band && z < maxZoom) band = band->GetOverview(maxZoom - z - 1);
        if (!band || band->GetXSize() != (ts << z)) {
            error = "unexpected tile pyramid";
            return false;
        }
        bands[b] = band;
    }
    CPLErrorReset();
    bool ok = true;
    for (int b = 0; b < 4 && ok; ++b)
        ok = bands[b]->RasterIO(GF_Read, x * ts, y * ts, ts, ts, rgba + b, ts, ts, GDT_Byte, 4, GSpacing(ts) * 4,
                                nullptr) == CE_None;
    for (GDALRasterBand* band : bands) band->FlushCache(false); // on the GPU now, not in GDAL's block cache
    if (ok) return true;
    const std::string msg = CPLGetLastErrorMsg();
    const size_t p = msg.find("HTTP status code: ");
    http = p == std::string::npos ? -1 : std::atoi(msg.c_str() + p + 18);
    if (http == 0 && (msg.find("certificate") != std::string::npos || msg.find("SSL") != std::string::npos))
        http = -1; // reached, but TLS failed: not "offline"
    error = http > 0    ? "HTTP " + std::to_string(http)
            : http == 0 ? "no connection"
            : msg.find("certificate") != std::string::npos ? "TLS certificates (see CURL_CA_BUNDLE)"
                                                           : "the tile could not be read";
    return false;
}

} // namespace

// ---------------------------------------------------------------------------
// Basemap
// ---------------------------------------------------------------------------

Basemap::Basemap(const BasemapSource& src, const std::string& cacheDir, std::function<void()> wake)
    : src_(src), sh_(std::make_shared<Shared>()) {
    src_.maxZoom = std::clamp(src_.maxZoom, 0, src_.tileSize > 256 ? 21 : 22); // pixel sizes stay in an int
    std::error_code ec;
    fs::create_directories(fs::u8path(cacheDir), ec);
    sh_->xml = wmsXml(src_, cacheDir);
    sh_->caBundle = caBundle();
    sh_->maxZoom = src_.maxZoom;
    sh_->tileSize = src_.tileSize;
    sh_->wake = std::move(wake);
    pool_ = std::make_unique<JobPool>(std::max(1, src_.connections));
}

Basemap::~Basemap() {
    {
        std::lock_guard<std::mutex> lk(sh_->m);
        sh_->wanted.clear(); // queued requests drop themselves
    }
    releaseGpu();
}

void Basemap::releaseGpu() {
    for (auto& [_, t] : gpu_) Gpu::deleteTexture(t.tex);
    gpu_.clear();
    gpuBytes_ = 0;
    map_.reset();
    mapFor_ = 0;
}

int Basemap::datasetsOpened() { return g_opened.load(); }

bool Basemap::offline() const { return sh_->offline.load(); }

std::string Basemap::error() const {
    std::lock_guard<std::mutex> lk(sh_->m);
    return sh_->error;
}

int Basemap::inflight() const {
    std::lock_guard<std::mutex> lk(sh_->m);
    return int(sh_->inflight.size());
}

// The tiles' frame: a Web Mercator rectangle around everything the map can
// show (the layer and kMargin x its longer side around it), as a raster about
// as fine as the layer; Reprojection makes the grid from the layer's pixels to
// it, as for a layer in another CRS. Its uv stay small numbers (float in the
// shader) even for the finest tiles.
bool Basemap::setMap(const CubeInfo& a, std::string& why) {
    if (map_ && mapFor_ == a.id) return true;
    map_.reset();
    mapFor_ = 0;
    ++mapVersion_;
    if (!a.hasGeoTransform) {
        why = "the active layer has no georeferencing";
        return false;
    }
    if (a.crsWkt.empty()) {
        why = "the active layer has no CRS";
        return false;
    }
    OGRSpatialReference sa, sm;
    if (sa.importFromWkt(a.crsWkt.c_str()) != OGRERR_NONE || sm.importFromEPSG(3857) != OGRERR_NONE) {
        why = "the active layer's CRS is not understood";
        return false;
    }
    sa.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    sm.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    CPLPushErrorHandler(CPLQuietErrorHandler);
    std::unique_ptr<OGRCoordinateTransformation, void (*)(OGRCoordinateTransformation*)> ct(
        OGRCreateCoordinateTransformation(&sa, &sm), OGRCoordinateTransformation::DestroyCT);
    CPLPopErrorHandler();
    if (!ct) {
        why = "no transformation to Web Mercator";
        return false;
    }
    const double m = kMargin * std::max(a.width, a.height);
    const double around[4] = {-m, -m, a.width + m, a.height + m};
    const int n = 33;
    std::vector<double> xs, ys;
    for (int j = 0; j < n; ++j)
        for (int i = 0; i < n; ++i) {
            double gx, gy;
            a.pixelToGeo(around[0] + (around[2] - around[0]) * i / (n - 1),
                         around[1] + (around[3] - around[1]) * j / (n - 1), gx, gy);
            xs.push_back(gx);
            ys.push_back(gy);
        }
    std::vector<int> ok(xs.size(), 0);
    CPLPushErrorHandler(CPLQuietErrorHandler);
    ct->Transform(xs.size(), xs.data(), ys.data(), nullptr, ok.data());
    CPLPopErrorHandler();
    double box[4] = {HUGE_VAL, HUGE_VAL, -HUGE_VAL, -HUGE_VAL};
    for (size_t k = 0; k < xs.size(); ++k) {
        if (!ok[k] || !std::isfinite(xs[k]) || !std::isfinite(ys[k])) continue;
        box[0] = std::min(box[0], std::clamp(xs[k], -kE, kE));
        box[1] = std::min(box[1], std::clamp(ys[k], -kE, kE));
        box[2] = std::max(box[2], std::clamp(xs[k], -kE, kE));
        box[3] = std::max(box[3], std::clamp(ys[k], -kE, kE));
    }
    if (!(box[2] > box[0] && box[3] > box[1])) {
        why = "the layer is outside Web Mercator";
        return false;
    }
    // About one frame pixel per map pixel: the grid's error (< 0.05 of the
    // frame's pixels) stays far below the screen's pixels.
    const double res = std::sqrt((box[2] - box[0]) * (box[3] - box[1]) / ((around[2] - around[0]) * (around[3] - around[1])));
    CubeInfo f;
    f.width = frameW_ = std::max(1, int(std::ceil((box[2] - box[0]) / res)));
    f.height = frameH_ = std::max(1, int(std::ceil((box[3] - box[1]) / res)));
    f.hasGeoTransform = true;
    f.geoTransform = {box[0], res, 0, box[3], 0, -res};
    char* wkt = nullptr;
    const char* opts[] = {"FORMAT=WKT2", nullptr};
    sm.exportToWkt(&wkt, opts);
    f.crsWkt = wkt ? wkt : "";
    CPLFree(wkt);
    f.crsAuthority = "EPSG:3857";
    frame_[0] = box[0];
    frame_[1] = box[3];
    frame_[2] = res;
    std::unique_ptr<Reprojection> r = Reprojection::create(a, f, why, kMargin);
    if (!r || !r->hasDomain()) {
        if (r) why = "the layer is outside Web Mercator";
        return false;
    }
    r->tex = Gpu::createWarpGrid(r->gridW, r->gridH, r->grid.data());
    map_ = std::move(r);
    mapFor_ = a.id;
    placedAt_ = nowMs();
    firstTileMs_ = -1;
    return true;
}

void Basemap::tick() {
    ++frameNo_;
    sh_->frame = frameNo_;
    if (frameNo_ % 120 == 0) { // prune old requests and failures
        const double now = nowMs();
        std::lock_guard<std::mutex> lk(sh_->m);
        for (auto it = sh_->wanted.begin(); it != sh_->wanted.end();)
            it = it->second + 120 < frameNo_ ? sh_->wanted.erase(it) : std::next(it);
        for (auto it = sh_->failed.begin(); it != sh_->failed.end();)
            it = now > it->second ? sh_->failed.erase(it) : std::next(it);
    }
}

bool Basemap::viewTiles(const double v[4], double pxPerMapPx, int& z, double box[4]) const {
    if (!map_ || !(pxPerMapPx > 0)) return false;
    const Reprojection& R = *map_;
    const double x0 = std::max(v[0], R.domain[0]), y0 = std::max(v[1], R.domain[1]);
    const double x1 = std::min(v[2], R.domain[2]), y1 = std::min(v[3], R.domain[3]);
    if (!(x1 > x0 && y1 > y0)) return false;
    const std::vector<double> p = R.polygon(x0, y0, x1, y1, 8);
    if (p.size() < 6) return false;
    box[0] = box[1] = HUGE_VAL;
    box[2] = box[3] = -HUGE_VAL;
    for (size_t i = 0; i + 1 < p.size(); i += 2) {
        const double X = frame_[0] + p[i] * frame_[2], Y = frame_[1] - p[i + 1] * frame_[2];
        box[0] = std::min(box[0], X);
        box[1] = std::min(box[1], Y);
        box[2] = std::max(box[2], X);
        box[3] = std::max(box[3], Y);
    }
    for (int k = 0; k < 4; ++k) box[k] = std::clamp(box[k], -kE, kE);
    // Metres of Web Mercator per target pixel at the view's centre; the level
    // whose pixels are about as fine (up to 1.23x enlarged).
    const double k = R.layerPxPerMapPx((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    if (!(k > 0)) return false;
    const double metres = k * frame_[2] / pxPerMapPx;
    z = std::clamp(int(std::ceil(std::log2(2 * kE / (src_.tileSize * metres)) - 0.3)), 0, src_.maxZoom);
    for (; z > 0; --z) {
        int r[4];
        tileRange(z, box, r);
        if (double(r[2] - r[0] + 1) * (r[3] - r[1] + 1) <= kMaxTiles) break;
    }
    return true;
}

void Basemap::tileRange(int z, const double box[4], int r[4]) const {
    const int n = 1 << z;
    const double s = std::ldexp(2 * kE, -z);
    r[0] = std::clamp(int(std::floor((box[0] + kE) / s)), 0, n - 1);
    r[2] = std::clamp(int(std::floor((box[2] + kE) / s - 1e-9)), 0, n - 1);
    r[1] = std::clamp(int(std::floor((kE - box[3]) / s)), 0, n - 1);
    r[3] = std::clamp(int(std::floor((kE - box[1]) / s - 1e-9)), 0, n - 1);
}

int Basemap::update(const double view[4], double pxPerMapPx) {
    int z;
    double box[4];
    if (!viewTiles(view, pxPerMapPx, z, box)) return -1;
    if (z >= 3) request(z - 3, box, 0); // a coarse preview first: 1/64 of the tiles
    request(z, box, 1);
    return z;
}

void Basemap::request(int z, const double box[4], int priority) {
    int r[4];
    tileRange(z, box, r);
    const double cx = (r[0] + r[2]) * 0.5, cy = (r[1] + r[3]) * 0.5, now = nowMs();
    std::vector<std::pair<double, uint64_t>> todo;
    std::lock_guard<std::mutex> lk(sh_->m);
    for (int y = r[1]; y <= r[3]; ++y)
        for (int x = r[0]; x <= r[2]; ++x) {
            const uint64_t key = tileKey(z, x, y);
            if (auto g = gpu_.find(key); g != gpu_.end()) {
                g->second.lastUsed = frameNo_;
                continue;
            }
            sh_->wanted[key] = frameNo_;
            if (auto f = sh_->failed.find(key); f != sh_->failed.end() && now < f->second) continue;
            if (sh_->inflight.count(key)) continue;
            todo.push_back({std::hypot(x - cx, y - cy), key});
        }
    // The pool runs the newest first: the tiles nearest the centre go last.
    std::sort(todo.begin(), todo.end(), [](const auto& a, const auto& b) { return a.first > b.first; });
    for (const auto& [_, key] : todo) {
        sh_->inflight.insert(key);
        const int x = int((key >> 22) & 0x3FFFFF), y = int(key & 0x3FFFFF);
        auto sh = sh_;
        pool_->submit(priority, [sh, key, z, x, y] {
            {
                std::lock_guard<std::mutex> lk(sh->m);
                auto it = sh->wanted.find(key);
                if (it == sh->wanted.end() || it->second + 2 < sh->frame.load()) {
                    sh->inflight.erase(key); // left the screen before being sent
                    return;
                }
            }
            // Per thread: a dead connection fails fast; certificates for curl.
            CPLSetThreadLocalConfigOption("GDAL_HTTP_CONNECTTIMEOUT", "5");
            if (!sh->caBundle.empty()) CPLSetThreadLocalConfigOption("CURL_CA_BUNDLE", sh->caBundle.c_str());
            const double t0 = nowMs();
            const int ts = sh->tileSize;
            Shared::Result res{key, std::vector<unsigned char>(size_t(ts) * ts * 4), 0};
            int http = -1;
            std::string error = "the tile source could not be opened";
            GDALDataset* ds = sh->acquire();
            const bool ok = ds && readTile(ds, sh->maxZoom, ts, z, x, y, res.rgba.data(), http, error);
            sh->release(ds);
            res.ms = nowMs() - t0;
            {
                std::lock_guard<std::mutex> lk(sh->m);
                if (ok) {
                    sh->results.push_back(std::move(res));
                    sh->offline = false;
                    sh->error.clear();
                } else {
                    sh->inflight.erase(key);
                    sh->failed[key] = nowMs() + (http == 0 ? kRetryMs : kRetryHttpMs);
                    if (http == 0) sh->offline = true;
                    else sh->error = error;
                }
            }
            if (sh->wake) sh->wake();
        });
    }
}

bool Basemap::uploadReady(int maxUploads) {
    std::vector<Shared::Result> batch;
    {
        std::lock_guard<std::mutex> lk(sh_->m);
        const int n = std::min<int>(maxUploads, int(sh_->results.size()));
        for (int i = 0; i < n; ++i) {
            batch.push_back(std::move(sh_->results.back()));
            sh_->results.pop_back();
            sh_->inflight.erase(batch.back().key);
        }
    }
    const int ts = src_.tileSize;
    for (Shared::Result& r : batch) {
        if (gpu_.count(r.key)) continue;
        Tile t;
        t.tex = Gpu::createImageTexture(ts, ts, r.rgba.data());
        t.lastUsed = frameNo_;
        gpu_.emplace(r.key, t);
        gpuBytes_ += size_t(ts) * ts * 4;
        loadMsSum_ += r.ms;
        ++loads_;
        if (firstTileMs_ < 0) firstTileMs_ = nowMs() - placedAt_;
    }
    if (gpuBytes_ > maxGpuBytes_) evict();
    return !batch.empty();
}

void Basemap::evict() {
    std::vector<std::pair<uint64_t, uint64_t>> order; // (lastUsed, key)
    order.reserve(gpu_.size());
    for (auto& [k, t] : gpu_) order.push_back({t.lastUsed, k});
    std::sort(order.begin(), order.end());
    const size_t one = size_t(src_.tileSize) * src_.tileSize * 4;
    for (auto& [used, key] : order) {
        if (gpuBytes_ <= maxGpuBytes_ * 9 / 10 || used >= frameNo_) break;
        Gpu::deleteTexture(gpu_[key].tex);
        gpuBytes_ -= one;
        gpu_.erase(key);
    }
}

// Where tile (z, x, y) is drawn: the map-space box of its part inside the
// frame (outside it the transformation may fail), with a margin for the
// curvature of its edges; the shader discards what falls outside the tile.
void Basemap::place(int z, int x, int y, Tile& t) {
    t.quadFor = mapVersion_;
    t.drawable = false;
    const Reprojection& R = *map_;
    const double s = std::ldexp(2 * kE, -z), res = frame_[2];
    const double X0 = -kE + x * s, Y0 = kE - y * s; // top left
    const double lx0 = (X0 - frame_[0]) / res, ly0 = (frame_[1] - Y0) / res, side = s / res;
    const double cx0 = std::max(lx0, 0.0), cy0 = std::max(ly0, 0.0);
    const double cx1 = std::min(lx0 + side, double(frameW_)), cy1 = std::min(ly0 + side, double(frameH_));
    double b[4];
    if (!(cx1 > cx0 && cy1 > cy0) || !R.footprint(cx0, cy0, cx1, cy1, b)) return;
    const double mx = (b[2] - b[0]) * 0.02 + 1, my = (b[3] - b[1]) * 0.02 + 1;
    double* q = t.quad;
    q[0] = std::max(b[0] - mx, R.domain[0]);
    q[1] = std::max(b[1] - my, R.domain[1]);
    q[2] = std::min(b[2] + mx, R.domain[2]);
    q[3] = std::min(b[3] + my, R.domain[3]);
    if (q[2] <= q[0] || q[3] <= q[1]) return;
    const double dw = R.domain[2] - R.domain[0], dh = R.domain[3] - R.domain[1];
    t.warp.grid = R.tex;
    t.warp.quad[0] = float((q[0] - R.domain[0]) / dw);
    t.warp.quad[1] = float((q[1] - R.domain[1]) / dh);
    t.warp.quad[2] = float((q[2] - q[0]) / dw);
    t.warp.quad[3] = float((q[3] - q[1]) / dh);
    t.warp.src[0] = float(lx0 / frameW_);
    t.warp.src[1] = float(ly0 / frameH_);
    t.warp.src[2] = float(side / frameW_);
    t.warp.src[3] = float(side / frameH_);
    t.drawable = true;
}

void Basemap::forEachVisible(const double view[4], double pxPerMapPx,
                             const std::function<void(GpuTex, const double*, const WarpParams&)>& f) {
    int z;
    double box[4];
    if (!viewTiles(view, pxPerMapPx, z, box)) return;
    // Coarse to fine; the next finer level (zooming out) before the wanted one.
    std::vector<int> levels;
    for (int l = std::max(0, z - 8); l < z; ++l) levels.push_back(l);
    if (z < src_.maxZoom) levels.push_back(z + 1);
    levels.push_back(z);
    for (int l : levels) {
        int r[4];
        tileRange(l, box, r);
        if (double(r[2] - r[0] + 1) * (r[3] - r[1] + 1) > 4 * kMaxTiles) continue;
        for (int y = r[1]; y <= r[3]; ++y)
            for (int x = r[0]; x <= r[2]; ++x) {
                auto it = gpu_.find(tileKey(l, x, y));
                if (it == gpu_.end()) continue;
                Tile& t = it->second;
                t.lastUsed = frameNo_;
                if (t.quadFor != mapVersion_) place(l, x, y, t);
                if (t.drawable) f(t.tex, t.quad, t.warp);
            }
    }
}
