#pragma once

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include "cube.hpp"

class JobPool;

// The whole cube at full resolution on the local disk (the cache folder, an
// SSD): exact series, ROI and detail tiles without touching the source files,
// which may sit on an HDD. Stored date by date in 64x64 blocks, each compressed
// on its own and losslessly (float bits as integers, difference with the
// previous pixel, zigzag, bytes split into 4 planes, zstd through GDAL's
// compressor registry: no new dependency). A series reads one small block per
// date (41 reads of ~11 KB: 0.8-1 ms); an ROI or a tile reads the blocks it
// covers, date by date as they are asked for.
//
// Dates, not time-contiguous chunks: a chunk holding every date can only be
// written once every date was read, i.e. the whole cube held in memory or a
// second pass rewriting it; date by date, the cache is built in the same
// sequential read as the overview, each date is usable as soon as it is
// written, and a stopped build resumes date by date like the overview cache
// (a date is trusted once its block table and flag are written).
//
// Built in the background: on an HDD, for a source without internal overviews,
// the overview pass reads each date at full resolution and builds both caches
// in that read (Overview::fullResBuild); dates whose overview came from its
// cache are read afterwards, in a pass of their own.
class FullResCache {
public:
    static constexpr int kBlock = 64;

    FullResCache(std::shared_ptr<const CubeInfo> info, std::string cacheDir);
    ~FullResCache();
    FullResCache(const FullResCache&) = delete;
    FullResCache& operator=(const FullResCache&) = delete;

    // Opens (or creates) the cache file; submitBuild then builds the missing
    // dates in `pool` (after the overview's jobs). `decodeThreads` > 1 (HDD):
    // each date is streamed by one reader and decoded by that many threads.
    // Also resumes a stopped build. False if it cannot be used: see error().
    bool start(int decodeThreads, uint64_t budgetBytes);
    void submitBuild(JobPool& pool);
    // Stops building (the date being read is dropped); what is done stays readable.
    void stop() { stop_ = true; }
    bool started() const { return started_.load(); }
    bool building() const { return started_.load() && !stop_.load() && !readOnly_ && !complete(); }

    // Overview pass: reads date t at full resolution once for both caches and
    // fills `ov` (ow x oh, GDAL's nearest neighbour). -1: not handled here (date
    // already cached or being built, or the cache is off); 0: read failed; 1: done.
    int buildWithOverview(int t, float* ov, int ow, int oh);

    bool hasDate(int t) const { return t >= 0 && t < T_ && done_[t].load(std::memory_order_acquire); }
    int datesDone() const { return nDone_.load(); }
    bool complete() const { return T_ > 0 && nDone_.load() == T_; }

    // Pixel (x, y) of every cached date into out[t]; got[t] = 1 where read.
    // Returns the number of dates read.
    int readSeries(int x, int y, float* out, char* got);
    // As CubeReader::readWindow (same pixels, also when subsampled); false if
    // date t is not cached.
    bool readWindow(int t, int x, int y, int w, int h, float* buf, int bw, int bh);

    // Shown in the Performance panel.
    std::string path() const { return path_; }
    std::string error() const;
    const char* codec() const { return codec_; }
    bool readOnly() const { return readOnly_; }
    uint64_t bytes() const { return end_.load(); }        // file size (data written)
    uint64_t estimatedBytes() const;                       // when complete
    double buildSeconds() const { return buildSeconds_.load(); }
    int datesWithOverview() const { return withOverview_.load(); }
    int seriesHits() const { return seriesHits_.load(); }
    double lastSeriesMs() const { return lastSeriesMs_.load(); }
    int windowHits() const { return windowHits_.load(); }
    double avgWindowMs() const { int n = windowHits_.load(); return n ? windowMsSum_.load() / n : 0.0; }

    // Removes the oldest caches (least recently opened) until the folder's
    // full-resolution caches take at most maxBytes; files in use are kept.
    static void prune(const std::string& cacheDir, uint64_t maxBytes);
    // Removes every full-resolution cache not in use; returns the bytes freed.
    static uint64_t clear(const std::string& cacheDir);
    // Size of the cache folder's full-resolution caches.
    static uint64_t folderBytes(const std::string& cacheDir);

private:
    bool buildDate(int t, float* ov, int ow, int oh);
    bool storeBand(int t, int by, const float* band);
    bool claim(int t);
    int claimNext();
    void release(int t, bool ok);
    bool loadBlock(int t, int b, float* out);
    void finish();
    FILE* takeReader();
    void giveReader(FILE* f);
    int blockW(int bx) const { return std::min(kBlock, W_ - bx * kBlock); }
    int blockH(int by) const { return std::min(kBlock, H_ - by * kBlock); }

    std::shared_ptr<const CubeInfo> info_;
    std::string cacheDir_, path_;
    int W_ = 0, H_ = 0, T_ = 0, nbx_ = 0, nby_ = 0, nb_ = 0;
    int decodeThreads_ = 1;
    const char* codec_ = "";
    uint64_t key_ = 0, tableOffset_ = 0, dataOffset_ = 0;
    bool readOnly_ = false;
    bool integrate_ = false;     // the overview pass reads full dates (source without overviews)

    std::unique_ptr<std::atomic<bool>[]> done_;
    std::vector<uint64_t> off_;  // [t * nb + b]: block offset in the file
    std::vector<uint32_t> size_; // compressed size (raw size = stored as is)

    mutable std::mutex m_;       // state_, error_
    std::vector<char> state_;    // 0 missing, 1 building, 2 done, 3 failed
    std::string error_;
    std::mutex fileM_;
    FILE* w_ = nullptr;          // open while dates are missing
    std::atomic<uint64_t> end_{0};
    std::mutex readersM_;
    std::vector<FILE*> readers_;

    std::atomic<bool> started_{false}, stop_{false};
    std::atomic<int> nDone_{0}, withOverview_{0};
    std::atomic<double> buildSeconds_{0};
    double buildStart_ = 0;
    std::atomic<int> seriesHits_{0}, windowHits_{0};
    std::atomic<double> lastSeriesMs_{0}, windowMsSum_{0};
};
