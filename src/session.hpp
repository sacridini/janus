#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include "cube.hpp"
#include "gpu.hpp"
#include "job_pool.hpp"
#include "overview.hpp"
#include "stats.hpp"
#include "tiles.hpp"

struct SessionSettings {
    int64_t overviewBudgetBytes = 1024ll << 20;
    size_t tileBudgetBytes = 384ull << 20;
    std::string cacheDir;
    int maxTexSize = 16384;
    int ioThreads = 0;          // 0 = automatic (HDD: 1, SSD: cores)
};

struct SeriesResult {
    uint64_t id;
    int x, y;
    std::vector<float> values;
    double ms;
};

// Statistics of a rectangular ROI, date by date (computed in parallel).
struct RoiData {
    int x0, y0, x1, y1;         // source pixels, [x0, x1)
    int bw, bh;                 // size read (may be subsampled)
    bool sampled = false;
    std::vector<SampleStats> perT;
    std::atomic<int> done{0};
    std::atomic<bool> cancel{false};
    std::chrono::steady_clock::time_point t0;
    std::atomic<double> ms{0};
};

// An open time series: data, GPU state and background work.
class Session {
public:
    Session(std::shared_ptr<CubeInfo> info, const SessionSettings& s, std::function<void()> wake);
    ~Session();

    // Main thread, once per frame: uploads whatever is ready to the GPU.
    // Returns true if the map needs to be redrawn.
    bool pump(Gpu& gpu);
    bool busy() const;
    // HDD with the overview still building: random reads (pins, tiles, ROI)
    // wait, so the read head is not pulled away from the sequential pass.
    bool deferRandomReads() const { return rotational && !overview.complete(); }

    // Exact series (full resolution). `cancellable`: dropped if another
    // cancellable call arrives before the read starts (mouse cursor).
    uint64_t requestSeries(int x, int y, bool cancellable);
    std::vector<SeriesResult> takeSeries();
    std::shared_ptr<RoiData> startRoi(int x0, int y0, int x1, int y1);

    std::shared_ptr<const CubeInfo> info;
    Overview overview;
    GpuCube gpu; // after overview: destroyed first (Metal may read the overview's memory in place)
    std::unique_ptr<TileManager> tiles;
    std::vector<float> timesYears;
    bool rotational = false;
    double statsMs = 0;
    double openSeconds = 0;

    JobPool& bgPool() { return *bg_; }
    JobPool& fgPool() { return *fg_; }

private:
    std::function<void()> wake_;
    std::mutex seriesM_;
    std::vector<SeriesResult> seriesResults_;
    std::atomic<uint64_t> nextSeriesId_{1};
    std::atomic<uint64_t> latestHover_{0};
    // Declared last = destroyed first (jobs use the members above).
    std::unique_ptr<JobPool> fg_, bg_;
};
