#include "overview.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>

#include "job_pool.hpp"

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
        double meta[3] = {L.hasNoData ? L.noData : 0.0, L.scale, L.offset};
        k = fnv1a(k, meta, sizeof(meta));
    }
    return k;
}

} // namespace

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
    std::snprintf(name, sizeof(name), "%016llx.tsvcube", (unsigned long long)key);
    cachePath_ = (fs::u8path(cacheDir) / name).u8string();

    std::error_code ec;
    if (fs::file_size(fs::u8path(cachePath_), ec) == sizeof(Header) + data.size() * sizeof(float)) {
        fromCache_ = true;
        // Read layer by layer so the GPU receives data while the rest loads.
        pool.submit(0, [this, key] {
            FILE* f = nullptr;
#ifdef _WIN32
            _wfopen_s(&f, fs::u8path(cachePath_).wstring().c_str(), L"rb");
#else
            f = std::fopen(cachePath_.c_str(), "rb");
#endif
            Header hd{};
            bool ok = f && std::fread(&hd, sizeof(hd), 1, f) == 1 && hd.key == key && hd.w == w &&
                      hd.h == h && hd.T == T && std::memcmp(hd.magic, kMagic, 8) == 0;
            const size_t n = size_t(w) * h;
            for (int t = 0; t < T; ++t) {
                if (ok) ok = std::fread(data.data() + n * t, sizeof(float), n, f) == n;
                if (!ok) failed_++;
                markDone(t);
            }
            if (f) std::fclose(f);
        });
        return;
    }

    // Build from scratch. The order follows the focused date: if the user
    // jumps to 2010 while loading, 2010 and the following dates come first.
    claimed_.assign(T, 0);
    for (int i = 0; i < T; ++i) {
        pool.submit(10, [this] {
            const int t = claimNext();
            if (t >= 0) decode(t);
        });
    }
}

void Overview::decode(int t) {
    CubeReader& r = threadReader(info_);
    if (!r.readWindow(t, 0, 0, info_->width, info_->height, data.data() + size_t(t) * w * h, w, h)) failed_++;
    markDone(t);
}

void Overview::markDone(int t) {
    {
        std::lock_guard<std::mutex> lk(m_);
        ready_.push_back(t);
    }
    if (done_.fetch_add(1) + 1 == T) {
        buildSeconds_ = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0_).count();
        if (!fromCache_ && failed_ == 0) writeCache();
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

void Overview::writeCache() {
    const fs::path final = fs::u8path(cachePath_);
    const fs::path tmp = final.string() + ".tmp";
    FILE* f = nullptr;
#ifdef _WIN32
    _wfopen_s(&f, tmp.wstring().c_str(), L"wb");
#else
    f = std::fopen(tmp.c_str(), "wb");
#endif
    if (!f) return;
    Header hd{};
    std::memcpy(hd.magic, kMagic, 8);
    hd.key = cacheKey(*info_, w, h);
    hd.w = w;
    hd.h = h;
    hd.T = T;
    bool ok = std::fwrite(&hd, sizeof(hd), 1, f) == 1 &&
              std::fwrite(data.data(), sizeof(float), data.size(), f) == data.size();
    ok = (std::fclose(f) == 0) && ok;
    std::error_code ec;
    if (ok) fs::rename(tmp, final, ec);
    else fs::remove(tmp, ec);
}

void Overview::pruneCache(const std::string& cacheDir, uint64_t maxBytes) {
    struct Entry { fs::path p; fs::file_time_type t; uint64_t size; };
    std::vector<Entry> entries;
    uint64_t total = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec)) {
        if (e.path().extension() != ".tsvcube") continue;
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
