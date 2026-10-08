#include "fullres_cache.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <map>
#include <thread>

#include <cpl_compressor.h>
#include <gdal_priv.h>

#include "job_pool.hpp"
#include "overview.hpp"

#ifdef _WIN32
#include <share.h>
#endif

namespace fs = std::filesystem;

namespace {

constexpr char kMagic[8] = {'J', 'A', 'N', 'U', 'S', 'F', 'R', '1'};
constexpr const char* kExt = ".janusfull";

// File: header, one flag per date, one block table per date (nb offsets, then
// nb sizes), then the blocks. A date's blocks are written first, then its
// table, then its flag: a write cut short is never trusted.
struct Header {
    char magic[8];
    uint64_t key;
    int32_t W, H, T, block, codec, reserved;
    uint64_t tableOffset, dataOffset;
};

enum Codec { kZstd = 1, kZlib = 2 };
const char* codecName(int c) { return c == kZstd ? "zstd" : "zlib"; }
// zstd when this GDAL has it (conda-forge builds do), else zlib (always there).
int bestCodec() {
    return CPLGetCompressor("zstd") && CPLGetDecompressor("zstd") ? kZstd : kZlib;
}
bool codecAvailable(int c) {
    return (c == kZstd || c == kZlib) && CPLGetCompressor(codecName(c)) && CPLGetDecompressor(codecName(c));
}

double nowSeconds() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

void addTo(std::atomic<double>& a, double v) {
    double cur = a.load();
    while (!a.compare_exchange_weak(cur, cur + v)) {
    }
}

bool seekTo(FILE* f, uint64_t offset) {
#ifdef _WIN32
    return _fseeki64(f, int64_t(offset), SEEK_SET) == 0;
#else
    return fseeko(f, off_t(offset), SEEK_SET) == 0;
#endif
}

// `exclusive`: no other process may write the file meanwhile (Windows).
FILE* openFile(const std::string& path, const char* mode, bool exclusive) {
#ifdef _WIN32
    const std::wstring m(mode, mode + std::strlen(mode));
    return _wfsopen(fs::u8path(path).wstring().c_str(), m.c_str(), exclusive ? _SH_DENYWR : _SH_DENYNO);
#else
    (void)exclusive;
    return std::fopen(path.c_str(), mode);
#endif
}

// Caches open in this process: never pruned or cleared.
std::mutex gInUseM;
std::map<std::string, int> gInUse;

bool inUse(const fs::path& p) {
    std::lock_guard<std::mutex> lk(gInUseM);
    auto it = gInUse.find(p.filename().u8string());
    return it != gInUse.end() && it->second > 0;
}

// Lossless block coding. Neighbouring floats share sign, exponent and high
// mantissa bits: as integers their differences are small, zigzag makes them
// positive, and splitting the 4 bytes into planes puts the (nearly constant)
// high bytes together. Measured on Landsat NDVI (Float32, full mantissa):
// 1.42x with zstd level 1, against 1.31x for zstd on shuffled bytes alone and
// 1.17x for zstd alone; deflate gains 1% more at a tenth of the speed.
void toPlanes(const float* v, size_t n, uint8_t* p) {
    uint32_t prev = 0;
    for (size_t i = 0; i < n; ++i) {
        uint32_t u;
        std::memcpy(&u, v + i, 4);
        const uint32_t d = u - prev;
        prev = u;
        const uint32_t z = (d << 1) ^ (0u - (d >> 31));
        p[i] = uint8_t(z);
        p[n + i] = uint8_t(z >> 8);
        p[2 * n + i] = uint8_t(z >> 16);
        p[3 * n + i] = uint8_t(z >> 24);
    }
}

void fromPlanes(const uint8_t* p, size_t n, float* v) {
    uint32_t prev = 0;
    for (size_t i = 0; i < n; ++i) {
        const uint32_t z = uint32_t(p[i]) | uint32_t(p[n + i]) << 8 | uint32_t(p[2 * n + i]) << 16 |
                           uint32_t(p[3 * n + i]) << 24;
        prev += (z >> 1) ^ (0u - (z & 1));
        std::memcpy(v + i, &prev, 4);
    }
}

struct Scratch {
    std::vector<uint8_t> a, b, out;
    std::vector<float> f;
};
Scratch& scratch() {
    thread_local Scratch s;
    return s;
}

// Source pixel of each buffer pixel when reading (off, size) into n pixels:
// GDAL's nearest neighbour exactly (GDALRasterBand::IRasterIO: columns by
// accumulated increments, rows by multiplication, both with a 1e-10 epsilon),
// so a subsampled read from the cache gives what the source gives.
void sourceColumns(int off, int size, int n, int rasterSize, std::vector<int>& out) {
    out.resize(n);
    if (n == size) {
        for (int i = 0; i < n; ++i) out[i] = off + i;
        return;
    }
    const double inc = size / double(n);
    double d = 0.5 * inc + off + 1e-10;
    for (int i = 0; i < n; ++i, d += inc) out[i] = int(std::min(std::max(0.0, d), double(rasterSize - 1)));
}

void sourceRows(int off, int size, int n, int rasterSize, std::vector<int>& out) {
    out.resize(n);
    if (n == size) {
        for (int i = 0; i < n; ++i) out[i] = off + i;
        return;
    }
    const double inc = size / double(n);
    for (int i = 0; i < n; ++i)
        out[i] = int(std::min(std::max(0.0, (i + 0.5) * inc + off + 1e-10), double(rasterSize - 1)));
}

// Byte range of date t's blocks in its file (every band read for it), from
// the GeoTIFF block offsets; false if unknown (another driver, not a local file).
bool dateByteRange(const CubeInfo& info, int t, uint64_t& begin, uint64_t& end) {
    const Layer& L = info.layers[t];
    std::error_code ec;
    if (!fs::is_regular_file(fs::u8path(L.path), ec)) return false;
    GDALDataset* ds = GDALDataset::Open(L.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
    if (!ds) return false;
    bool ok = std::strcmp(ds->GetDriverName(), "GTiff") == 0;
    begin = UINT64_MAX;
    end = 0;
    for (int b : {L.band, info.sel.ndBand, info.sel.qaBand}) {
        if (!ok || b <= 0 || b > ds->GetRasterCount()) continue;
        GDALRasterBand* rb = ds->GetRasterBand(b);
        int bw = 0, bh = 0;
        rb->GetBlockSize(&bw, &bh);
        const int nx = (info.width + bw - 1) / bw, ny = (info.height + bh - 1) / bh;
        char k1[64], k2[64];
        std::snprintf(k1, sizeof(k1), "BLOCK_OFFSET_%d_%d", nx - 1, ny - 1);
        std::snprintf(k2, sizeof(k2), "BLOCK_SIZE_%d_%d", nx - 1, ny - 1);
        const char* o0 = rb->GetMetadataItem("BLOCK_OFFSET_0_0", "TIFF");
        const char* o1 = rb->GetMetadataItem(k1, "TIFF");
        const char* s1 = rb->GetMetadataItem(k2, "TIFF");
        if (!o0 || !o1 || !s1) {
            ok = false;
            break;
        }
        begin = std::min<uint64_t>(begin, std::strtoull(o0, nullptr, 10));
        end = std::max<uint64_t>(end, std::strtoull(o1, nullptr, 10) + std::strtoull(s1, nullptr, 10));
    }
    GDALClose(ds);
    return ok && end > begin;
}

// HDD: reads a date's bytes ahead of GDAL in large sequential chunks (at most
// kLead ahead of what was decoded), so the disk streams while GDAL decodes
// from the OS cache in parallel; decoders wait for it (a decoder reading on
// its own would pull the head away). Without it GDAL alternates between
// reading a band's strips and decoding them, and the disk waits.
class Prefetcher {
public:
    Prefetcher(const std::string& path, uint64_t begin, uint64_t end) : begin_(begin), end_(end), used_(begin) {
        thread_ = std::thread([this, path] { run(path); });
    }
    ~Prefetcher() {
        {
            std::lock_guard<std::mutex> lk(m_);
            stop_ = true;
        }
        cv_.notify_all();
        thread_.join();
    }
    // Blocks until the first fraction f of the range was read (or reading stopped).
    void waitFor(double f) {
        const uint64_t want = begin_ + uint64_t(f * double(end_ - begin_));
        std::unique_lock<std::mutex> lk(m_);
        if (want > used_) { // the lead never keeps it from what a decoder waits for
            used_ = want;
            cv_.notify_all();
        }
        cv_.wait(lk, [&] { return done_ || pos_ >= want; });
    }
    // The decoders have gone through this fraction of the range.
    void progress(double f) {
        {
            std::lock_guard<std::mutex> lk(m_);
            used_ = std::max(used_, begin_ + uint64_t(f * double(end_ - begin_)));
        }
        cv_.notify_all();
    }

private:
    static constexpr size_t kChunk = 4u << 20;
    static constexpr uint64_t kLead = 128ull << 20;
    void run(const std::string& path) {
        FILE* f = openFile(path, "rb", false);
        std::vector<char> buf(kChunk);
        uint64_t pos = begin_;
        bool ok = f && seekTo(f, pos);
        if (f) std::setvbuf(f, nullptr, _IONBF, 0);
        while (ok && pos < end_) {
            {
                std::unique_lock<std::mutex> lk(m_);
                cv_.wait(lk, [&] { return stop_ || pos < used_ + kLead; });
                if (stop_) break;
            }
            const size_t n = size_t(std::min<uint64_t>(kChunk, end_ - pos));
            ok = std::fread(buf.data(), 1, n, f) == n;
            pos += n;
            {
                std::lock_guard<std::mutex> lk(m_);
                pos_ = pos;
            }
            cv_.notify_all();
        }
        if (f) std::fclose(f);
        {
            std::lock_guard<std::mutex> lk(m_);
            done_ = true;
        }
        cv_.notify_all();
    }
    const uint64_t begin_, end_;
    std::mutex m_;
    std::condition_variable cv_;
    uint64_t used_, pos_ = 0;
    bool stop_ = false, done_ = false;
    std::thread thread_;
};

} // namespace

FullResCache::FullResCache(std::shared_ptr<const CubeInfo> info, std::string cacheDir)
    : info_(std::move(info)), cacheDir_(std::move(cacheDir)) {
    W_ = info_->width;
    H_ = info_->height;
    T_ = info_->T();
    nbx_ = (W_ + kBlock - 1) / kBlock;
    nby_ = (H_ + kBlock - 1) / kBlock;
    nb_ = nbx_ * nby_;
    done_ = std::make_unique<std::atomic<bool>[]>(size_t(std::max(T_, 1)));
    for (int t = 0; t < T_; ++t) done_[t] = false;
    state_.assign(T_, 0);
}

FullResCache::~FullResCache() {
    stop_ = true;
    if (w_) std::fclose(w_);
    for (FILE* f : readers_) std::fclose(f);
    if (!path_.empty()) {
        std::lock_guard<std::mutex> lk(gInUseM);
        --gInUse[fs::u8path(path_).filename().u8string()];
    }
}

std::string FullResCache::error() const {
    std::lock_guard<std::mutex> lk(m_);
    return error_;
}

uint64_t FullResCache::estimatedBytes() const {
    const int done = nDone_.load();
    if (done > 0 && end_.load() > dataOffset_)
        return dataOffset_ + uint64_t(double(end_.load() - dataOffset_) / done * T_);
    // Measured lossless ratio on Landsat NDVI: 1.42.
    return dataOffset_ + uint64_t(double(W_) * H_ * T_ * 4 / 1.4);
}

bool FullResCache::start(int decodeThreads, uint64_t budgetBytes) {
    if (started_) { // resume after stop()
        std::lock_guard<std::mutex> lk(m_);
        if (readOnly_ || !w_) return complete();
        for (char& s : state_)
            if (s == 3) s = 0;
        stop_ = false;
        return true;
    }
    if (T_ <= 0) return false;
    {
        std::lock_guard<std::mutex> lk(m_);
        error_.clear();
        readOnly_ = false;
    }
    decodeThreads_ = std::max(1, decodeThreads);
    const int codec = bestCodec();
    codec_ = codecName(codec);
    const int32_t salt[4] = {kBlock, codec, 0, 0};
    char saltBytes[8 + sizeof(salt)];
    std::memcpy(saltBytes, kMagic, 8);
    std::memcpy(saltBytes + 8, salt, sizeof(salt));
    key_ = cubeCacheKey(*info_, saltBytes, sizeof(saltBytes));
    char name[40];
    std::snprintf(name, sizeof(name), "%016llx%s", (unsigned long long)key_, kExt);
    if (path_.empty()) { // a start() that failed may be retried
        std::lock_guard<std::mutex> lk(gInUseM);
        ++gInUse[name];
    }
    path_ = (fs::u8path(cacheDir_) / name).u8string();
    tableOffset_ = sizeof(Header) + uint64_t(T_);
    dataOffset_ = tableOffset_ + uint64_t(T_) * nb_ * 12;
    off_.assign(size_t(T_) * nb_, 0);
    size_.assign(size_t(T_) * nb_, 0);

    // HDD, source without internal overviews: the overview pass reads full
    // dates and builds both caches at once. Measured cold (Landsat NDVI, LZW,
    // 1-row strips): 1.28 s per date for both against 1.27 s for the overview
    // alone, then 1.31 s per date for a pass of its own. On an SSD the overview
    // comes first (it is CPU-bound there, and full dates cost 3x more to decode).
    integrate_ = false;
    if (GDALDataset* ds = decodeThreads_ > 1 ? GDALDataset::Open(info_->layers[0].path.c_str(),
                                                                 GDAL_OF_RASTER | GDAL_OF_READONLY)
                                             : nullptr) {
        const int b = std::min(std::max(1, info_->layers[0].band), ds->GetRasterCount());
        integrate_ = b >= 1 && ds->GetRasterBand(b)->GetOverviewCount() == 0;
        GDALClose(ds);
    }

    // Dates already in the file.
    const fs::path p = fs::u8path(path_);
    std::error_code ec;
    bool valid = false;
    uint64_t validEnd = dataOffset_;
    if (FILE* f = openFile(path_, "rb", false)) {
        const uint64_t fileSize = fs::file_size(p, ec);
        Header hd{};
        if (std::fread(&hd, sizeof(hd), 1, f) == 1 && std::memcmp(hd.magic, kMagic, 8) == 0 && hd.key == key_ &&
            hd.W == W_ && hd.H == H_ && hd.T == T_ && hd.block == kBlock && hd.codec == codec &&
            hd.tableOffset == tableOffset_ && hd.dataOffset == dataOffset_) {
            std::vector<char> flags(T_);
            valid = std::fread(flags.data(), 1, T_, f) == size_t(T_);
            for (int t = 0; valid && t < T_; ++t) {
                if (!flags[t]) continue;
                uint64_t* o = &off_[size_t(t) * nb_];
                uint32_t* s = &size_[size_t(t) * nb_];
                bool ok = seekTo(f, tableOffset_ + uint64_t(t) * nb_ * 12) &&
                          std::fread(o, 8, nb_, f) == size_t(nb_) && std::fread(s, 4, nb_, f) == size_t(nb_);
                uint64_t e = 0;
                for (int b = 0; ok && b < nb_; ++b) {
                    ok = o[b] >= dataOffset_ && o[b] + s[b] <= fileSize && s[b] > 0;
                    e = std::max(e, o[b] + s[b]);
                }
                if (!ok) continue; // an entry past the end of the file is not trusted
                validEnd = std::max(validEnd, e);
                state_[t] = 2;
                done_[t] = true;
                ++nDone_;
            }
        }
        std::fclose(f);
    }
    end_ = validEnd;
    if (complete()) {
        fs::last_write_time(p, fs::file_time_type::clock::now(), ec); // least recently used goes first
        started_ = true;
        return true;
    }

    // Room for the missing dates: within the budget (older caches pruned) and
    // on the disk.
    auto fail = [&](const std::string& why) {
        std::lock_guard<std::mutex> lk(m_);
        error_ = why;
        readOnly_ = true;
        started_ = nDone_ > 0; // what is there stays readable
        return started_.load();
    };
    const uint64_t raw = uint64_t(double(W_) * H_ * T_ * 4 / 1.4);
    const uint64_t need = dataOffset_ + raw / T_ * (T_ - nDone_.load());
    const uint64_t have = valid ? fs::file_size(p, ec) : 0;
    char msg[160];
    if (need > budgetBytes) {
        std::snprintf(msg, sizeof(msg), "needs %.1f GB, more than the cache budget (%.0f GB)", need / 1e9,
                      budgetBytes / 1e9);
        return fail(msg);
    }
    prune(cacheDir_, budgetBytes - need);
    if (folderBytes(cacheDir_) - have + need > budgetBytes) {
        std::snprintf(msg, sizeof(msg), "needs %.1f GB; the budget (%.0f GB) is taken by caches in use", need / 1e9,
                      budgetBytes / 1e9);
        return fail(msg);
    }
    const fs::space_info sp = fs::space(fs::u8path(cacheDir_), ec);
    if (!ec && sp.available < need + (2ull << 30)) {
        std::snprintf(msg, sizeof(msg), "needs %.1f GB, the disk has %.1f GB free", need / 1e9, sp.available / 1e9);
        return fail(msg);
    }

    // Kept open, and closed to other writers, while dates are missing.
    if (valid) {
        w_ = openFile(path_, "r+b", true);
    } else if ((w_ = openFile(path_, "w+b", true))) {
        Header hd{};
        std::memcpy(hd.magic, kMagic, 8);
        hd.key = key_;
        hd.W = W_;
        hd.H = H_;
        hd.T = T_;
        hd.block = kBlock;
        hd.codec = codec;
        hd.tableOffset = tableOffset_;
        hd.dataOffset = dataOffset_;
        const std::vector<char> zeros(size_t(dataOffset_ - sizeof(Header)), 0);
        if (std::fwrite(&hd, sizeof(hd), 1, w_) != 1 || std::fwrite(zeros.data(), 1, zeros.size(), w_) != zeros.size() ||
            std::fflush(w_) != 0) {
            std::fclose(w_);
            w_ = nullptr;
        }
        end_ = dataOffset_;
    }
    if (!w_) return fail(valid ? "in use by another Janus window (read only)" : "could not write " + path_);
    fs::last_write_time(p, fs::file_time_type::clock::now(), ec);
    buildStart_ = nowSeconds();
    started_ = true;
    return true;
}

void FullResCache::submitBuild(JobPool& pool) {
    if (!started_ || readOnly_ || !w_) return;
    int missing = 0;
    {
        std::lock_guard<std::mutex> lk(m_);
        missing = int(std::count(state_.begin(), state_.end(), 0));
    }
    // After the overview's jobs (priorities 0 and 10): on an HDD the single
    // reader finishes the overview first, then sweeps the files again for the
    // dates whose overview came from its cache.
    for (int i = 0; i < missing; ++i)
        pool.submit(20, [this] {
            if (stop_) return;
            const int t = claimNext();
            if (t >= 0) buildDate(t, nullptr, 0, 0);
        });
}

bool FullResCache::claim(int t) {
    std::lock_guard<std::mutex> lk(m_);
    if (state_[t] != 0) return false;
    state_[t] = 1;
    return true;
}

int FullResCache::claimNext() {
    std::lock_guard<std::mutex> lk(m_);
    for (int t = 0; t < T_; ++t)
        if (state_[t] == 0) {
            state_[t] = 1;
            return t;
        }
    return -1;
}

void FullResCache::release(int t, bool ok) {
    {
        std::lock_guard<std::mutex> lk(m_);
        state_[t] = ok ? 2 : stop_ ? 0 : 3;
    }
    if (!ok) return;
    done_[t].store(true, std::memory_order_release);
    if (nDone_.fetch_add(1) + 1 == T_) finish();
}

void FullResCache::finish() {
    buildSeconds_ = nowSeconds() - buildStart_;
    std::lock_guard<std::mutex> lk(fileM_);
    if (!w_) return;
    std::fclose(w_);
    w_ = nullptr;
    std::error_code ec; // drop what an interrupted build left past the data
    if (fs::file_size(fs::u8path(path_), ec) > end_.load()) fs::resize_file(fs::u8path(path_), end_.load(), ec);
}

int FullResCache::buildWithOverview(int t, float* ov, int ow, int oh) {
    if (!started_ || stop_ || readOnly_ || !integrate_ || !claim(t)) return -1;
    const bool ok = buildDate(t, ov, ow, oh);
    if (ok) ++withOverview_;
    return ok ? 1 : 0;
}

// Reads date t in bands of kBlock rows and stores each band's blocks. On an
// HDD (decodeThreads_ > 1) a Prefetcher streams the file in order while
// several threads, each with its own GDAL handles, decode and compress the
// bands from the OS cache (taken in order, so they stay close behind it):
// GDAL's own multi-threaded decoding (one job per 1-row strip) was measured
// at 0.6-1.0 s per date against 0.25 s this way.
bool FullResCache::buildDate(int t, float* ov, int ow, int oh) {
    std::unique_ptr<Prefetcher> prefetch;
    uint64_t begin = 0, end = 0;
    if (decodeThreads_ > 1 && dateByteRange(*info_, t, begin, end))
        prefetch = std::make_unique<Prefetcher>(info_->layers[t].path, begin, end);
    std::vector<int> xs, ys;
    if (ov) {
        sourceColumns(0, W_, ow, W_, xs);
        sourceRows(0, H_, oh, H_, ys);
    }
    std::atomic<int> next{0}, bandsDone{0};
    std::atomic<bool> readFailed{false}, writeFailed{false};
    auto work = [&] {
        CubeReader reader(info_);
        std::vector<float> band;
        for (;;) {
            const int by = next++;
            if (by >= nby_ || readFailed || (stop_ && !ov)) break;
            const int y0 = by * kBlock, bh = blockH(by);
            band.resize(size_t(W_) * bh);
            if (prefetch) prefetch->waitFor(double(by + 1) / nby_);
            if (!reader.readWindow(t, 0, y0, W_, bh, band.data(), W_, bh)) {
                readFailed = true;
                break;
            }
            // Overview rows taken from this band (GDAL's nearest neighbour).
            for (auto j = std::lower_bound(ys.begin(), ys.end(), y0); ov && j != ys.end() && *j < y0 + bh; ++j) {
                const float* src = band.data() + size_t(*j - y0) * W_;
                float* dst = ov + size_t(j - ys.begin()) * ow;
                for (int i = 0; i < ow; ++i) dst[i] = src[xs[i]];
            }
            // When stopped the overview still needs the rest of the date.
            if (!stop_ && !writeFailed && !storeBand(t, by, band.data())) writeFailed = true;
            if (prefetch) prefetch->progress(double(++bandsDone) / nby_);
        }
    };
    std::vector<std::thread> helpers;
    for (int k = 1; k < decodeThreads_; ++k) helpers.emplace_back(work);
    work();
    for (std::thread& h : helpers) h.join();

    const bool readOk = !readFailed;
    bool ok = readOk && !stop_ && !writeFailed;
    if (ok) {
        const char one = 1;
        std::lock_guard<std::mutex> lk(fileM_);
        ok = w_ && std::fflush(w_) == 0 && seekTo(w_, tableOffset_ + uint64_t(t) * nb_ * 12) &&
             std::fwrite(&off_[size_t(t) * nb_], 8, nb_, w_) == size_t(nb_) &&
             std::fwrite(&size_[size_t(t) * nb_], 4, nb_, w_) == size_t(nb_) && std::fflush(w_) == 0 &&
             seekTo(w_, sizeof(Header) + uint64_t(t)) && std::fwrite(&one, 1, 1, w_) == 1 && std::fflush(w_) == 0;
        if (!ok && w_) {
            std::fclose(w_);
            w_ = nullptr;
        }
    }
    if (readOk && !ok && !stop_) {
        std::lock_guard<std::mutex> lk(m_);
        if (error_.empty()) error_ = "could not write the cache (disk full?)";
    }
    release(t, ok);
    return readOk;
}

// Compresses the blocks of band `by` (W x blockH floats) and appends them.
bool FullResCache::storeBand(int t, int by, const float* band) {
    const int bh = blockH(by);
    const CPLCompressor* comp = CPLGetCompressor(codec_);
    const char* const opts[] = {"LEVEL=1", nullptr};
    Scratch& s = scratch();
    std::vector<uint8_t>& out = s.out;
    out.clear();
    std::vector<uint32_t> sizes(nbx_);
    for (int bx = 0; bx < nbx_; ++bx) {
        const int bw = blockW(bx);
        const size_t n = size_t(bw) * bh, rawBytes = n * 4;
        s.f.resize(n);
        for (int j = 0; j < bh; ++j)
            std::memcpy(s.f.data() + size_t(j) * bw, band + size_t(j) * W_ + size_t(bx) * kBlock, bw * 4);
        s.a.resize(rawBytes);
        toPlanes(s.f.data(), n, s.a.data());
        const size_t cap = rawBytes + rawBytes / 8 + 1024;
        const size_t at = out.size();
        out.resize(at + cap);
        void* dst = out.data() + at;
        size_t size = cap;
        // Stored as is when compression does not pay (the size tells: raw size).
        if (!comp || !comp->pfnFunc(s.a.data(), rawBytes, &dst, &size, opts, comp->user_data) || size == 0 ||
            size >= rawBytes) {
            std::memcpy(out.data() + at, s.a.data(), rawBytes);
            size = rawBytes;
        }
        out.resize(at + size);
        sizes[bx] = uint32_t(size);
    }
    std::lock_guard<std::mutex> lk(fileM_);
    const uint64_t base = end_.load();
    if (!w_ || !seekTo(w_, base) || std::fwrite(out.data(), 1, out.size(), w_) != out.size()) return false;
    uint64_t o = base;
    for (int bx = 0; bx < nbx_; ++bx) {
        const size_t i = size_t(t) * nb_ + size_t(by) * nbx_ + bx;
        off_[i] = o;
        size_[i] = sizes[bx];
        o += sizes[bx];
    }
    end_ = o;
    return true;
}

FILE* FullResCache::takeReader() {
    {
        std::lock_guard<std::mutex> lk(readersM_);
        if (!readers_.empty()) {
            FILE* f = readers_.back();
            readers_.pop_back();
            return f;
        }
    }
    FILE* f = openFile(path_, "rb", false);
    if (f) std::setvbuf(f, nullptr, _IONBF, 0); // whole blocks: no stdio buffer in between
    return f;
}

void FullResCache::giveReader(FILE* f) {
    std::lock_guard<std::mutex> lk(readersM_);
    readers_.push_back(f);
}

bool FullResCache::loadBlock(int t, int b, float* out) {
    const size_t i = size_t(t) * nb_ + b;
    const uint64_t offset = off_[i];
    const uint32_t size = size_[i];
    const int bx = b % nbx_, by = b / nbx_;
    const size_t n = size_t(blockW(bx)) * blockH(by), rawBytes = n * 4;
    Scratch& s = scratch();
    s.b.resize(size);
    FILE* f = takeReader();
    if (!f) return false;
    const bool read = seekTo(f, offset) && std::fread(s.b.data(), 1, size, f) == size;
    giveReader(f);
    if (!read) return false;
    const uint8_t* planes = s.b.data();
    if (size != rawBytes) {
        const CPLCompressor* dec = CPLGetDecompressor(codec_);
        s.a.resize(rawBytes);
        void* dst = s.a.data();
        size_t outSize = rawBytes;
        if (!dec || !dec->pfnFunc(s.b.data(), size, &dst, &outSize, nullptr, dec->user_data) || outSize != rawBytes)
            return false;
        planes = s.a.data();
    }
    fromPlanes(planes, n, out);
    return true;
}

int FullResCache::readSeries(int x, int y, float* out, char* got) {
    if (!started_ || nDone_.load() == 0 || x < 0 || y < 0 || x >= W_ || y >= H_) return 0;
    const double t0 = nowSeconds();
    const int bx = x / kBlock, by = y / kBlock;
    const size_t idx = size_t(y - by * kBlock) * blockW(bx) + (x - bx * kBlock);
    std::vector<float> block(size_t(kBlock) * kBlock);
    int n = 0;
    for (int t = 0; t < T_; ++t) {
        if (!hasDate(t) || !loadBlock(t, by * nbx_ + bx, block.data())) continue;
        out[t] = block[idx];
        got[t] = 1;
        ++n;
    }
    if (n) {
        ++seriesHits_;
        lastSeriesMs_ = (nowSeconds() - t0) * 1000;
    }
    return n;
}

bool FullResCache::readWindow(int t, int x, int y, int w, int h, float* buf, int bw, int bh) {
    if (!hasDate(t) || w <= 0 || h <= 0 || bw <= 0 || bh <= 0) return false;
    const double t0 = nowSeconds();
    std::vector<int> xs, ys;
    sourceColumns(x, w, bw, W_, xs);
    sourceRows(y, h, bh, H_, ys);
    std::vector<float> block(size_t(kBlock) * kBlock);
    // Source rows and columns grow with the buffer's: walk them block by block.
    for (int j0 = 0; j0 < bh;) {
        const int by = ys[j0] / kBlock;
        int j1 = j0;
        while (j1 < bh && ys[j1] / kBlock == by) ++j1;
        for (int i0 = 0; i0 < bw;) {
            const int bx = xs[i0] / kBlock;
            int i1 = i0;
            while (i1 < bw && xs[i1] / kBlock == bx) ++i1;
            if (!loadBlock(t, by * nbx_ + bx, block.data())) return false;
            const int stride = blockW(bx), x0 = bx * kBlock;
            for (int j = j0; j < j1; ++j) {
                const float* src = block.data() + size_t(ys[j] - by * kBlock) * stride;
                float* dst = buf + size_t(j) * bw;
                for (int i = i0; i < i1; ++i) dst[i] = src[xs[i] - x0];
            }
            i0 = i1;
        }
        j0 = j1;
    }
    ++windowHits_;
    addTo(windowMsSum_, (nowSeconds() - t0) * 1000);
    return true;
}

void FullResCache::prune(const std::string& cacheDir, uint64_t maxBytes) {
    struct Entry { fs::path p; fs::file_time_type t; uint64_t size; };
    std::vector<Entry> entries;
    uint64_t total = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec)) {
        if (e.path().extension() != kExt) continue;
        entries.push_back({e.path(), e.last_write_time(ec), e.file_size(ec)});
        total += entries.back().size;
    }
    std::sort(entries.begin(), entries.end(), [](const Entry& a, const Entry& b) { return a.t < b.t; });
    for (const Entry& e : entries) {
        if (total <= maxBytes) break;
        if (inUse(e.p) || !fs::remove(e.p, ec)) continue;
        total -= e.size;
    }
}

uint64_t FullResCache::clear(const std::string& cacheDir) {
    uint64_t freed = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec)) {
        if (e.path().extension() != kExt || inUse(e.path())) continue;
        const uint64_t size = e.file_size(ec);
        if (fs::remove(e.path(), ec)) freed += size;
    }
    return freed;
}

uint64_t FullResCache::folderBytes(const std::string& cacheDir) {
    uint64_t total = 0;
    std::error_code ec;
    for (auto& e : fs::directory_iterator(fs::u8path(cacheDir), ec))
        if (e.path().extension() == kExt) total += e.file_size(ec);
    return total;
}
