#include "tiles.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>

#include "fullres_cache.hpp"
#include "gpu.hpp"
#include "job_pool.hpp"

TileManager::TileManager(std::shared_ptr<const CubeInfo> info, double overviewFactor, JobPool& pool,
                         size_t maxGpuBytes, std::shared_ptr<FullResCache> fullRes)
    : info_(std::move(info)), fullRes_(std::move(fullRes)), overviewFactor_(overviewFactor), pool_(pool),
      maxGpuBytes_(maxGpuBytes), sh_(std::make_shared<Shared>()) {
    // Largest level L (2^L source px per texel) still finer than the overview.
    maxLevel_ = overviewFactor_ > 1.0001 ? int(std::ceil(std::log2(overviewFactor_))) - 1 : -1;
}

TileManager::~TileManager() {
    for (auto& [_, tile] : gpu_) Gpu::deleteTexture(tile.tex);
    std::lock_guard<std::mutex> lk(sh_->m);
    sh_->wanted.clear(); // pending jobs will drop themselves
}

uint64_t TileManager::makeKey(int t, int level, int tx, int ty) {
    return (uint64_t(t) << 43) | (uint64_t(level) << 38) | (uint64_t(tx) << 19) | uint64_t(ty);
}

int TileManager::inflight() const {
    std::lock_guard<std::mutex> lk(sh_->m);
    return int(sh_->inflight.size());
}

template <typename F>
static void forTilesInView(const ViewRect& v, int level, int W, int H, F f) {
    const double span = double(TileManager::kTileSize << level);
    const int ntx = int(std::ceil(W / span)), nty = int(std::ceil(H / span));
    const int tx0 = std::max(0, int(std::floor(v.x0 / span)));
    const int ty0 = std::max(0, int(std::floor(v.y0 / span)));
    const int tx1 = std::min(ntx - 1, int(std::floor((v.x1 - 1e-9) / span)));
    const int ty1 = std::min(nty - 1, int(std::floor((v.y1 - 1e-9) / span)));
    for (int ty = ty0; ty <= ty1; ++ty)
        for (int tx = tx0; tx <= tx1; ++tx) f(tx, ty);
}

void TileManager::tick() {
    ++frame_;
    sh_->frame = frame_;
    if (frame_ % 120 == 0) { // prune old requests
        std::lock_guard<std::mutex> lk(sh_->m);
        for (auto it = sh_->wanted.begin(); it != sh_->wanted.end();)
            it = it->second + 120 < frame_ ? sh_->wanted.erase(it) : std::next(it);
    }
}

int TileManager::update(int t, const ViewRect& v, int prefetchT) {
    if (maxLevel_ < 0 || v.scale * overviewFactor_ <= 1.0) return -1;
    const int level = std::clamp(int(std::floor(std::log2(1.0 / v.scale))), 0, maxLevel_);
    request(t, level, v, 1);
    if (prefetchT >= 0) request(prefetchT, level, v, 3);
    return level;
}

void TileManager::request(int t, int level, const ViewRect& v, int priority) {
    const int W = info_->width, H = info_->height;
    forTilesInView(v, level, W, H, [&](int tx, int ty) {
        const uint64_t key = makeKey(t, level, tx, ty);
        auto g = gpu_.find(key);
        if (g != gpu_.end()) {
            g->second.lastUsed = frame_;
            return;
        }
        std::lock_guard<std::mutex> lk(sh_->m);
        sh_->wanted[key] = frame_;
        if (!sh_->inflight.insert(key).second) return;

        auto sh = sh_;
        auto info = info_;
        auto fullRes = fullRes_;
        pool_.submit(priority, [sh, info, fullRes, key, t, level, tx, ty] {
            {
                std::lock_guard<std::mutex> lk(sh->m);
                auto it = sh->wanted.find(key);
                if (it == sh->wanted.end() || it->second + 2 < sh->frame.load()) {
                    sh->inflight.erase(key); // left the screen before being read
                    return;
                }
            }
            const auto t0 = std::chrono::steady_clock::now();
            const int span = TileManager::kTileSize << level;
            Result r;
            r.key = key;
            const int x = tx * span, y = ty * span;
            const int sw = std::min(span, info->width - x), shh = std::min(span, info->height - y);
            r.w = std::max(1, (sw + (1 << level) - 1) >> level);
            r.h = std::max(1, (shh + (1 << level) - 1) >> level);
            r.x = x;
            r.y = y;
            r.sw = sw;
            r.sh = shh;
            r.data.resize(size_t(r.w) * r.h);
            if (!(fullRes && fullRes->readWindow(t, x, y, sw, shh, r.data.data(), r.w, r.h)) &&
                !threadReader(info).readWindow(t, x, y, sw, shh, r.data.data(), r.w, r.h))
                std::fill(r.data.begin(), r.data.end(), NAN);
            r.ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
            std::lock_guard<std::mutex> lk(sh->m);
            sh->results.push_back(std::move(r));
        });
    });
}

bool TileManager::uploadReady(int maxUploads) {
    std::vector<Result> batch;
    {
        std::lock_guard<std::mutex> lk(sh_->m);
        const int n = std::min<int>(maxUploads, int(sh_->results.size()));
        for (int i = 0; i < n; ++i) {
            batch.push_back(std::move(sh_->results.back()));
            sh_->results.pop_back();
            sh_->inflight.erase(batch.back().key);
        }
    }
    for (Result& r : batch) {
        if (gpu_.count(r.key)) continue;
        Tile tile;
        tile.tex = Gpu::createTileTexture(r.w, r.h, r.data.data());
        tile.w = r.w;
        tile.h = r.h;
        tile.x = r.x;
        tile.y = r.y;
        tile.sw = r.sw;
        tile.sh = r.sh;
        tile.lastUsed = frame_;
        gpu_.emplace(r.key, tile);
        gpuBytes_ += size_t(r.w) * r.h * 4;
        loadMsSum_ += r.ms;
        ++loads_;
    }
    if (gpuBytes_ > maxGpuBytes_) evict();
    return !batch.empty();
}

void TileManager::evict() {
    std::vector<std::pair<uint64_t, uint64_t>> order; // (lastUsed, key)
    order.reserve(gpu_.size());
    for (auto& [k, tile] : gpu_) order.push_back({tile.lastUsed, k});
    std::sort(order.begin(), order.end());
    for (auto& [used, key] : order) {
        if (gpuBytes_ <= maxGpuBytes_ * 9 / 10 || used >= frame_) break;
        Tile& tile = gpu_[key];
        Gpu::deleteTexture(tile.tex);
        gpuBytes_ -= size_t(tile.w) * tile.h * 4;
        gpu_.erase(key);
    }
}

void TileManager::forEachVisible(int t, const ViewRect& v,
                                 const std::function<void(GpuTex, double, double, double, double)>& f) {
    for (int level = maxLevel_; level >= 0; --level) {
        forTilesInView(v, level, info_->width, info_->height, [&](int tx, int ty) {
            auto it = gpu_.find(makeKey(t, level, tx, ty));
            if (it == gpu_.end()) return;
            it->second.lastUsed = frame_;
            f(it->second.tex, it->second.x, it->second.y, it->second.sw, it->second.sh);
        });
    }
}
