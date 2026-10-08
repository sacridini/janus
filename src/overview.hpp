#pragma once

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <mutex>
#include <new>
#include <string>
#include <vector>

#include "cube.hpp"

class JobPool;

// Page-aligned storage whose length is rounded up to whole pages: the Metal
// renderer shares the overview with the GPU without copying it
// (newBufferWithBytesNoCopy needs both).
template <class T>
struct PageAllocator {
    using value_type = T;
    static constexpr size_t kPage = 16384; // Apple Silicon page (a multiple of 4 KiB pages)
    static size_t roundUp(size_t bytes) { return (bytes + kPage - 1) / kPage * kPage; }

    PageAllocator() = default;
    template <class U>
    PageAllocator(const PageAllocator<U>&) {}
    T* allocate(size_t n) { return static_cast<T*>(::operator new(roundUp(n * sizeof(T)), std::align_val_t(kPage))); }
    void deallocate(T* p, size_t) { ::operator delete(p, std::align_val_t(kPage)); }
    template <class U>
    bool operator==(const PageAllocator<U>&) const { return true; }
    template <class U>
    bool operator!=(const PageAllocator<U>&) const { return false; }
};

// The whole cube at reduced resolution, [t][y][x] float32 (NaN = nodata).
// Kept in RAM (for instant series/statistics) and sent to the GPU as a texture
// array. Built in parallel (1 job per date) and cached on disk: opening the
// same cube again only reads one contiguous file.
class Overview {
public:
    int w = 0, h = 0, T = 0;
    double factor = 1;      // source pixels per overview pixel
    std::vector<float, PageAllocator<float>> data;
    // Bytes of the page-aligned block behind `data` (whole pages).
    size_t pageBytes() const { return PageAllocator<float>::roundUp(data.capacity() * sizeof(float)); }

    // `budgetBytes` accounts for T floats per pixel (cube) + 8 (statistics). Metal
    // shares `data` with the GPU (see PageAllocator); GL keeps a second copy there.
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
