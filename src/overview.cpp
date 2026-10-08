#include "overview.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>

#include "job_pool.hpp"

#ifdef _WIN32
#include <share.h>
#endif

namespace fs = std::filesystem;

namespace {

constexpr char kMagic[8] = {'T', 'S', 'V', 'C', 'U', 'B', 'E', '1'};

struct Header {
    char magic[8];
    uint64_t key;
    int32_t w, h, T, reserved;
};

uint64_t fnv1a(uint64_t h, const void* p, size_t n) {
    const auto* b = static_cast<const unsigned char*>(p);
    for (size_t i = 0; i < n; ++i) h = (h ^ b[i]) * 1099511628211ull;
    return h;
}

// Cache key: changes if any file changes (size/mtime) or if the overview
// resolution changes.
uint64_t cacheKey(const CubeInfo& info, int w, int h) {
    uint64_t k = 1469598103934665603ull;
    k = fnv1a(k, kMagic, sizeof(kMagic));
    k = fnv1a(k, &w, sizeof(w));
    k = fnv1a(k, &h, sizeof(h));
    for (const Layer& L : info.layers) {
        k = fnv1a(k, L.path.data(), L.path.size());
        k = fnv1a(k, &L.band, sizeof(L.band));
        std::error_code ec;
        const fs::path p = fs::u8path(L.path);
        const uint64_t size = fs::file_size(p, ec);
        const auto mtime = fs::last_write_time(p, ec).time_since_epoch().count();
        k = fnv1a(k, &size, sizeof(size));
        k = fnv1a(k, &mtime, sizeof(mtime));
        double meta[3] = {L.meta.hasNoData ? L.meta.noData : 0.0, L.meta.scale, L.meta.offset};
        k = fnv1a(k, meta, sizeof(meta));
    }
    // Normalized difference and QA mask change the values (only hashed when in
    // use, so caches of plain series stay valid).
    if (info.sel.ndBand > 0 || info.sel.qaBand > 0) {
        const int sel[4] = {info.sel.band, info.sel.ndBand, info.sel.qaBand, int(info.sel.qaRule)};
        k = fnv1a(k, sel, sizeof(sel));
        for (const Layer& L : info.layers) {
            double meta[3] = {L.ndMeta.hasNoData ? L.ndMeta.noData : 0.0, L.ndMeta.scale, L.ndMeta.offset};
            k = fnv1a(k, meta, sizeof(meta));
        }
    }
    return k;
}

bool seekTo(FILE* f, uint64_t offset) {
#ifdef _WIN32
    return _fseeki64(f, int64_t(offset), SEEK_SET) == 0;
#else
    return fseeko(f, off_t(offset), SEEK_SET) == 0;
#endif
}

// `exclusive`: no other session may write the file meanwhile (Windows).
FILE* openFile(const std::string& path, const char* mode, bool exclusive) {
#ifdef _WIN32
    const std::wstring m(mode, mode + std::strlen(mode));
    return _wfsopen(fs::u8path(path).wstring().c_str(), m.c_str(), exclusive ? _SH_DENYWR : _SH_DENYNO);
#else
    (void)exclusive;
    return std::fopen(path.c_str(), mode);
#endif
}

} // namespace

Overview::~Overview() {
    if (cache_) std::fclose(cache_);
}

void Overview::start(std::shared_ptr<const CubeInfo> info, int64_t budgetBytes, int maxTexSize,
                     JobPool& pool, const std::string& cacheDir) {
    info_ = std::move(info);
    T = info_->T();
    const double W = info_->width, H = info_->height;
    const double maxPx = double(budgetBytes) / (4.0 * (T + 8));
    double s = std::min(1.0, std::sqrt(maxPx / (W * H)));
    s = std::min(s, double(maxTexSize) / std::max(W, H));
    w = std::max(1, int(std::lround(W * s)));
    h = std::max(1, int(std::lround(H * s)));
    factor = W / w;
    data.assign(size_t(w) * h * T, NAN);
    t0_ = std::chrono::steady_clock::now();

    const uint64_t key = cacheKey(*info_, w, h);
    char name[32];
    std::snprintf(name, sizeof(name), "%016llx.januscube", (unsigned long long)key);
    cachePath_ = (fs::u8path(cacheDir) / name).u8string();

    // Dates already in the cache: all of them in a complete file without an
    // index (the first format), the indexed ones in a file written date by
    // date (a build that was interrupted resumes where it stopped).
    const size_t n = size_t(w) * h;
    std::vector<char> have(T, 0);
    bool indexed = false;
    if (FILE* f = openFile(cachePath_, "rb", false)) {
        std::error_code ec;
        const uint64_t size = fs::file_size(fs::u8path(cachePath_), ec);
        Header hd{};
        if (std::fread(&hd, sizeof(hd), 1, f) == 1 && std::memcmp(hd.magic, kMagic, 8) == 0 && hd.key == key &&
            hd.w == w && hd.h == h && hd.T == T) {
            if (hd.reserved == 0 && size == sizeof(Header) + data.size() * sizeof(float)) {
                std::fill(have.begin(), have.end(), 1);
                dataOffset_ = sizeof(Header);
            } else if (hd.reserved == 1 && std::fread(have.data(), 1, T, f) == size_t(T)) {
                indexed = true;
                dataOffset_ = sizeof(Header) + T;
                for (int t = 0; t < T; ++t) // an entry past the end of the file is not trusted
                    if (have[t] && dataOffset_ + n * sizeof(float) * (t + 1) > size) have[t] = 0;
            }
        }
        std::fclose(f);
    }
    cachedLayers_ = int(std::count(have.begin(), have.end(), 1));
    fromCache_ = cachedLayers_ == T;

    if (fromCache_) {
        // Read layer by layer so the GPU receives data while the rest loads.
        pool.submit(0, [this, n] {
            FILE* f = openFile(cachePath_, "rb", false);
            bool ok = f && seekTo(f, dataOffset_);
            for (int t = 0; t < T; ++t) {
                if (ok) ok = std::fread(data.data() + n * t, sizeof(float), n, f) == n;
                if (!ok) failed_++;
                markDone(t);
            }
            if (f) std::fclose(f);
        });
        return;
    }

    // Kept open, and closed to other writers, while the missing dates are built.
    if (indexed) {
        cache_ = openFile(cachePath_, "r+b", true);
    } else if ((cache_ = openFile(cachePath_, "w+b", true))) {
        std::fill(have.begin(), have.end(), 0);
        dataOffset_ = sizeof(Header) + T;
        Header hd{};
        std::memcpy(hd.magic, kMagic, 8);
        hd.key = key;
        hd.w = w;
        hd.h = h;
        hd.T = T;
        hd.reserved = 1; // the index of the dates written follows the header
        if (std::fwrite(&hd, sizeof(hd), 1, cache_) != 1 || std::fwrite(have.data(), 1, T, cache_) != size_t(T) ||
            std::fflush(cache_) != 0) {
            std::fclose(cache_);
            cache_ = nullptr;
        }
    }
    if (!cache_) std::fill(have.begin(), have.end(), 0); // another session writes it: no cache
    cachedLayers_ = int(std::count(have.begin(), have.end(), 1));

    // Build what is missing. The order follows the focused date: if the user
    // jumps to 2010 while loading, 2010 and the following dates come first.
    claimed_.assign(have.begin(), have.end());
    if (cachedLayers_ > 0) {
        pool.submit(0, [this, have] {
            for (int t = 0; t < T; ++t) {
                if (!have[t]) continue;
                if (loadLayer(t)) markDone(t);
                else decode(t); // unreadable in the cache: from the source
            }
        });
    }
    for (int i = cachedLayers_; i < T; ++i) {
        pool.submit(10, [this] {
            const int t = claimNext();
            if (t >= 0) decode(t);
        });
    }
}

void Overview::decode(int t) {
    CubeReader& r = threadReader(info_);
    if (r.readWindow(t, 0, 0, info_->width, info_->height, data.data() + size_t(t) * w * h, w, h)) saveLayer(t);
    else failed_++; // left out of the cache: retried on the next open
    markDone(t);
}

bool Overview::loadLayer(int t) {
    const size_t n = size_t(w) * h;
    std::lock_guard<std::mutex> lk(fileM_);
    return cache_ && seekTo(cache_, dataOffset_ + n * sizeof(float) * t) &&
           std::fread(data.data() + n * t, sizeof(float), n, cache_) == n;
}

void Overview::saveLayer(int t) {
    const size_t n = size_t(w) * h;
    const char one = 1;
    std::lock_guard<std::mutex> lk(fileM_);
    if (!cache_) return;
    // The data first, then its index entry: a write cut short is never trusted.
    const bool ok = seekTo(cache_, dataOffset_ + n * sizeof(float) * t) &&
                    std::fwrite(layer(t), sizeof(float), n, cache_) == n && std::fflush(cache_) == 0 &&
                    seekTo(cache_, sizeof(Header) + t) && std::fwrite(&one, 1, 1, cache_) == 1 &&
                    std::fflush(cache_) == 0;
    if (!ok) {
        std::fclose(cache_);
        cache_ = nullptr;
    }
}

void Overview::markDone(int t) {
    {
        std::lock_guard<std::mutex> lk(m_);
        ready_.push_back(t);
    }
    if (done_.fetch_add(1) + 1 == T) {
        buildSeconds_ = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0_).count();
        std::lock_guard<std::mutex> lk(fileM_);
        if (cache_) std::fclose(cache_);
        cache_ = nullptr;
    }
}

int Overview::claimNext() {
    std::lock_guard<std::mutex> lk(m_);
    return claimNextLocked();
}

int Overview::claimNextLocked() {
    const int f = focus_.load();
    for (int d = 0; d < T; ++d) {
        const int t = (f + d) % T;
        if (!claimed_[t]) {
            claimed_[t] = 1;
            return t;
        }
    }
    return -1;
}

std::vector<int> Overview::takeReadyLayers() {
    std::lock_guard<std::mutex> lk(m_);
    std::vector<int> out;
    out.swap(ready_);
    return out;
}

void Overview::pruneCache(const std::string& cacheDir, uint64_t maxBytes) {
    struct Entry { fs::path p; fs::file_time_type t; uint64_t size; };
    std::vector<Entry> entries;
    uint64_t total = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec)) {
        if (e.path().extension() != ".januscube") continue;
        entries.push_back({e.path(), e.last_write_time(ec), e.file_size(ec)});
        total += entries.back().size;
    }
    std::sort(entries.begin(), entries.end(), [](const Entry& a, const Entry& b) { return a.t < b.t; });
    for (const Entry& e : entries) {
        if (total <= maxBytes) break;
        fs::remove(e.p, ec);
        total -= e.size;
    }
}
