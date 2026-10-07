#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include "cube.hpp"

class JobPool;

// The whole cube at reduced resolution, [t][y][x] float32 (NaN = nodata).
// Kept in RAM (for instant series/statistics) and sent to the GPU as a texture
// array. Built in parallel (1 job per date) and cached on disk: opening the
// same cube again only reads one contiguous file.
class Overview {
public:
    int w = 0, h = 0, T = 0;
    double factor = 1;      // source pixels per overview pixel
    std::vector<float> data;

    // `budgetBytes` accounts for T floats per pixel (cube) + 8 (statistics on the GPU).
    void start(std::shared_ptr<const CubeInfo> info, int64_t budgetBytes, int maxTexSize,
               JobPool& pool, const std::string& cacheDir);

    std::vector<int> takeReadyLayers();   // main thread: dates ready for upload
    // The date the user is looking at: the next ones built are t, t+1, t+2...
    void setFocus(int t) { focus_ = t; }
    int layersDone() const { return done_.load(); }
    bool complete() const { return T > 0 && done_.load() == T; }
    bool fromCache() const { return fromCache_; }
    double buildSeconds() const { return buildSeconds_.load(); }
    std::string cachePath() const { return cachePath_; }
    int failedLayers() const { return failed_.load(); }


    const float* layer(int t) const { return data.data() + size_t(t) * w * h; }
    float at(int t, int x, int y) const { return data[(size_t(t) * h + y) * w + x]; }

    static void pruneCache(const std::string& cacheDir, uint64_t maxBytes);

private:
    void markDone(int t);
    int claimNext();
    int claimNextLocked();
    void decode(int t);
    void writeCache();

    std::shared_ptr<const CubeInfo> info_;
    std::string cachePath_;
    bool fromCache_ = false;
    std::mutex m_;
    std::vector<int> ready_;
    std::vector<char> claimed_;

    std::atomic<int> focus_{0};
    std::atomic<int> done_{0};
    std::atomic<int> failed_{0};
    std::atomic<double> buildSeconds_{0};
    std::chrono::steady_clock::time_point t0_;
};
