#pragma once

#include <atomic>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "cube.hpp"
#include "gpu.hpp"

class JobPool;

// Visible map region in source pixels + scale (screen px per source px).
struct ViewRect {
    double x0, y0, x1, y1;
    double scale;
};

// Detail tiles (full resolution or intermediate levels) for when the zoom goes
// past the overview resolution. Only the current date (and the next one while
// animating) is requested; requests that left the screen are dropped before
// being read.
class TileManager {
public:
    static constexpr int kTileSize = 256;

    TileManager(std::shared_ptr<const CubeInfo> info, double overviewFactor, JobPool& pool,
                size_t maxGpuBytes);
    ~TileManager();

    // Main thread, once per frame. Returns the wanted level (-1 = overview is enough).
    int update(int t, const ViewRect& v, int prefetchT);
    // Uploads the tiles that were read (up to `maxUploads`). True if anything changed.
    bool uploadReady(int maxUploads);
    // Calls f(tex, x, y, w, h) for the cached tiles of date t covering the
    // view, from coarsest to finest.
    void forEachVisible(int t, const ViewRect& v,
                        const std::function<void(GpuTex, double, double, double, double)>& f);

    size_t gpuBytes() const { return gpuBytes_; }
    int gpuTiles() const { return int(gpu_.size()); }
    int inflight() const;
    double avgLoadMs() const { return loads_ ? loadMsSum_ / loads_ : 0.0; }

private:
    struct Tile {
        GpuTex tex = 0;
        int w = 0, h = 0;
        double x, y, sw, sh;  // rectangle in the source
        uint64_t lastUsed = 0;
    };
    struct Result {
        uint64_t key;
        int w, h;
        double x, y, sw, sh;
        std::vector<float> data;
        double ms;
    };
    static uint64_t makeKey(int t, int level, int tx, int ty);
    void request(int t, int level, const ViewRect& v, int priority);
    void evict();

    std::shared_ptr<const CubeInfo> info_;
    double overviewFactor_;
    int maxLevel_;          // coarsest level that is still finer than the overview
    JobPool& pool_;
    size_t maxGpuBytes_;
    size_t gpuBytes_ = 0;
    uint64_t frame_ = 0;
    std::unordered_map<uint64_t, Tile> gpu_;

    // Shared with the reader threads.
    struct Shared {
        std::mutex m;
        std::unordered_map<uint64_t, uint64_t> wanted;  // key -> last frame it was visible
        std::unordered_set<uint64_t> inflight;
        std::vector<Result> results;
        std::atomic<uint64_t> frame{0};
    };
    std::shared_ptr<Shared> sh_;
    double loadMsSum_ = 0;
    int loads_ = 0;
};
