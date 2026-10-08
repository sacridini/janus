#include "session.hpp"

#include <algorithm>
#include <cmath>
#include <thread>

#include "platform.hpp"

int logicalCores() { return int(std::max(1u, std::thread::hardware_concurrency())); }

int defaultProcessingThreads() { return std::max(1, logicalCores() - 2); }

int SessionSettings::processingThreads() const {
    return threads > 0 ? std::min(threads, logicalCores()) : defaultProcessingThreads();
}

// Spinning HDD: 1 sequential reader, whatever the processing threads (a disk
// decision). Measured cold (LZW, 1-row strips): 1 thread = 1.2-1.4 s/date,
// already close to the physical floor (sweeping the file at ~140 MB/s); 2
// threads = 5.0 s/date, because the head keeps jumping between files.
static int bgThreadsFor(const SessionSettings& s, bool rotational) {
    return s.ioThreads > 0 ? s.ioThreads : rotational ? 1 : std::min(s.processingThreads(), 12);
}
static int fgThreadsFor(const SessionSettings& s, bool rotational) {
    return rotational ? 1 : std::min(s.processingThreads(), 4);
}
// Full-resolution cache on an HDD: one thread streams the file, these decode and compress.
static int decodeThreadsFor(const SessionSettings& s, bool rotational) {
    return rotational ? std::clamp(s.processingThreads() / 2, 1, 4) : 1;
}

Session::Session(std::shared_ptr<CubeInfo> infoIn, const SessionSettings& s, std::function<void()> wake)
    : info(std::move(infoIn)), wake_(std::move(wake)) {
    rotational = platform::isOnRotationalDisk(info->firstPath);
    bg_ = std::make_unique<JobPool>(bgThreadsFor(s, rotational), wake_);
    fg_ = std::make_unique<JobPool>(fgThreadsFor(s, rotational), wake_);

    const int T = info->T();
    timesYears.resize(T);
    for (int t = 0; t < T; ++t) timesYears[t] = float(info->yearsFromStart(t));

    // Full-resolution cache: opened before the overview starts, so that on an
    // HDD the overview pass builds both in the same read of each date (one
    // thread streams the file, several decode); then the dates the overview
    // pass does not read (its own cache had them).
    fullRes = std::make_shared<FullResCache>(info, s.cacheDir);
    fullResDecodeThreads_ = decodeThreadsFor(s, rotational);
    fullResBudget_ = s.fullResBudgetBytes;
    const bool wantFull = s.fullResMode == SessionSettings::FullResAll ||
                          (s.fullResMode == SessionSettings::FullResHdd && rotational);
    if (wantFull) fullRes->start(fullResDecodeThreads_, fullResBudget_);
    overview.fullResBuild = [c = fullRes.get()](int t, float* layer, int w, int h) {
        return c->buildWithOverview(t, layer, w, h);
    };
    overview.start(info, s.overviewBudgetBytes, s.maxTexSize, *bg_, s.cacheDir);
    if (wantFull) fullRes->submitBuild(*bg_);
    gpu.create(overview.w, overview.h, T, timesYears, overview.data.data(), overview.pageBytes());
    tiles = std::make_unique<TileManager>(info, overview.factor, *fg_, s.tileBudgetBytes, fullRes);
}

Session::~Session() {
    fullRes->stop(); // the date being built is dropped instead of waited for
    fg_.reset();
    bg_.reset();
}

bool Session::pump(Gpu& g) {
    bool changed = false;
    tiles->tick();
    for (int t : overview.takeReadyLayers()) {
        gpu.uploadLayer(t, overview.layer(t));
        changed = true;
    }
    if (overview.complete() && !gpu.statsValid && info->T() >= 2) {
        const auto t0 = std::chrono::steady_clock::now();
        g.computeStats(gpu);
        statsMs = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
        changed = true;
    }
    changed |= tiles->uploadReady(16);
    return changed;
}

bool Session::busy() const {
    return !overview.complete() || bg_->pending() > 0 || fg_->pending() > 0;
}

uint64_t Session::requestSeries(int x, int y, bool cancellable) {
    const uint64_t id = nextSeriesId_++;
    if (cancellable) latestHover_ = id;
    fg_->submit(0, [this, id, x, y, cancellable] {
        if (cancellable && latestHover_.load() != id) return; // the mouse has already moved on
        const auto t0 = std::chrono::steady_clock::now();
        const int T = info->T();
        SeriesResult r{id, x, y, std::vector<float>(size_t(T), NAN), 0};
        // Dates in the full-resolution cache first (one small block each), the
        // rest from the source files.
        std::vector<char> got(T, 0);
        if (fullRes->readSeries(x, y, r.values.data(), got.data()) < T) {
            CubeReader& reader = threadReader(info);
            for (int t = 0; t < T; ++t) {
                if (cancellable && latestHover_.load() != id) return;
                if (!got[t]) reader.readPixel(t, x, y, r.values[t]);
            }
        }
        r.ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
        std::lock_guard<std::mutex> lk(seriesM_);
        seriesResults_.push_back(std::move(r));
    });
    return id;
}

std::vector<SeriesResult> Session::takeSeries() {
    std::lock_guard<std::mutex> lk(seriesM_);
    std::vector<SeriesResult> out;
    out.swap(seriesResults_);
    return out;
}

// Cells of the bw x bh buffer read for the window whose centres (in source
// pixels) are inside the polygon (even-odd rule, one scanline per row).
static std::vector<char> polygonMask(const std::vector<double>& poly, int x0, int y0, int w, int h, int bw, int bh) {
    std::vector<char> mask(size_t(bw) * bh, 0);
    const size_t n = poly.size() / 2;
    std::vector<double> xs;
    for (int j = 0; j < bh; ++j) {
        const double sy = y0 + (j + 0.5) * h / bh;
        xs.clear();
        for (size_t k = 0; k < n; ++k) {
            const double ax = poly[k * 2], ay = poly[k * 2 + 1];
            const double bx = poly[(k + 1) % n * 2], by = poly[(k + 1) % n * 2 + 1];
            if ((ay <= sy) != (by <= sy)) xs.push_back(ax + (sy - ay) * (bx - ax) / (by - ay));
        }
        std::sort(xs.begin(), xs.end());
        for (size_t k = 0; k + 1 < xs.size(); k += 2) {
            // Centres x0 + (i + 0.5) w / bw in [xs[k], xs[k + 1]).
            const int i0 = std::max(0, int(std::ceil((xs[k] - x0) * bw / w - 0.5)));
            const int i1 = std::min(bw, int(std::ceil((xs[k + 1] - x0) * bw / w - 0.5)));
            for (int i = i0; i < i1; ++i) mask[size_t(j) * bw + i] = 1;
        }
    }
    return mask;
}

std::shared_ptr<RoiData> Session::startRoi(int x0, int y0, int x1, int y1, const std::vector<double>& polygon) {
    auto roi = std::make_shared<RoiData>();
    roi->x0 = std::clamp(std::min(x0, x1), 0, info->width - 1);
    roi->y0 = std::clamp(std::min(y0, y1), 0, info->height - 1);
    roi->x1 = std::clamp(std::max(x0, x1), roi->x0 + 1, info->width);
    roi->y1 = std::clamp(std::max(y0, y1), roi->y0 + 1, info->height);
    const int w = roi->x1 - roi->x0, h = roi->y1 - roi->y0;
    // Above 4 Mpx per date, read subsampled (GDAL uses overviews if present).
    const double maxPx = 4.0e6;
    const double s = std::min(1.0, std::sqrt(maxPx / (double(w) * h)));
    roi->bw = std::max(1, int(w * s));
    roi->bh = std::max(1, int(h * s));
    roi->sampled = s < 1.0;
    if (polygon.size() >= 6) roi->mask = polygonMask(polygon, roi->x0, roi->y0, w, h, roi->bw, roi->bh);
    roi->perT.resize(info->T());
    roi->t0 = std::chrono::steady_clock::now();
    for (int t = 0; t < info->T(); ++t) {
        fg_->submit(5 + t, [this, roi, t] { // chronological: oldest date first
            if (!roi->cancel) {
                std::vector<float> buf(size_t(roi->bw) * roi->bh);
                const int x = roi->x0, y = roi->y0, w = roi->x1 - roi->x0, h = roi->y1 - roi->y0;
                const bool cached = fullRes->readWindow(t, x, y, w, h, buf.data(), roi->bw, roi->bh);
                if (cached) roi->cachedDates++;
                if (cached || threadReader(info).readWindow(t, x, y, w, h, buf.data(), roi->bw, roi->bh)) {
                    for (size_t i = 0; i < roi->mask.size(); ++i)
                        if (!roi->mask[i]) buf[i] = NAN;
                    roi->perT[t] = computeSampleStats(buf);
                }
            }
            if (roi->done.fetch_add(1) + 1 == info->T())
                roi->ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - roi->t0).count();
        });
    }
    return roi;
}

void Session::setThreads(const SessionSettings& s) {
    bg_->setThreads(bgThreadsFor(s, rotational));
    fg_->setThreads(fgThreadsFor(s, rotational));
    fullResDecodeThreads_ = decodeThreadsFor(s, rotational);
}

bool Session::buildFullRes() {
    if (!fullRes->start(fullResDecodeThreads_, fullResBudget_)) return false;
    fullRes->submitBuild(*bg_);
    return true;
}
