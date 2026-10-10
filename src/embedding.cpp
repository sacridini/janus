#include "embedding.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <numeric>
#include <random>

#include <gdal_priv.h>
#include <nlohmann/json.hpp>

#include "job_pool.hpp"

using json = nlohmann::json;
using Clock = std::chrono::steady_clock;

namespace {

double msSince(Clock::time_point t0) { return std::chrono::duration<double, std::milli>(Clock::now() - t0).count(); }

std::string text(const json& j, const char* key) {
    auto it = j.find(key);
    if (it == j.end() || it->is_null()) return "";
    return it->is_string() ? it->get<std::string>() : it->dump();
}

} // namespace

// ---------------------------------------------------------------------------
// What the layer is
// ---------------------------------------------------------------------------

std::string EmbeddingMeta::label() const {
    std::string s = !model.empty() && model.rfind("http", 0) != 0 ? model
                    : source == "tessera"                        ? "TESSERA"
                    : source == "alphaearth"                     ? "AlphaEarth Foundations"
                    : !source.empty()                            ? source
                                                                 : "Embeddings";
    if (source == "tessera" && !version.empty()) s += " v" + version;
    return s + ", " + std::to_string(dims) + " dimensions";
}

EmbeddingMeta embeddingMeta(const CubeInfo& info) {
    EmbeddingMeta m;
    if (info.bandsPerDate < 2) return m;
    if (!info.embeddingTag.empty()) {
        const json j = json::parse(info.embeddingTag, nullptr, false);
        if (j.is_object()) {
            m.is = true;
            m.source = text(j, "embedding_source");
            m.model = text(j, "embedding_model");
            m.version = text(j, "embedding_version");
            m.license = text(j, "embedding_license");
            m.attribution = text(j, "embedding_attribution");
            const std::string d = text(j, "embedding_dimensions");
            m.dims = d.empty() ? 0 : std::atoi(d.c_str());
        }
    }
    if (!m.is && info.bandsPerDate >= 16) { // Zeit's dimension names: A00, A01...
        int named = 0;
        for (const std::string& n : info.bandNames)
            named += n.size() >= 2 && n[0] == 'A' &&
                     std::all_of(n.begin() + 1, n.end(), [](char c) { return c >= '0' && c <= '9'; });
        m.is = named == info.bandsPerDate;
    }
    if (m.is && m.dims <= 0) m.dims = info.bandsPerDate;
    return m;
}

// ---------------------------------------------------------------------------
// Parallel loops
// ---------------------------------------------------------------------------

void parallelFor(JobPool& pool, size_t n, size_t grain, const std::function<void(size_t, size_t)>& fn) {
    if (n == 0) return;
    grain = std::max<size_t>(1, grain);
    const size_t maxChunks = size_t(pool.threads()) * 4;
    const size_t chunks = std::max<size_t>(1, std::min((n + grain - 1) / grain, maxChunks));
    if (chunks == 1) {
        fn(0, n);
        return;
    }
    struct Latch {
        std::mutex m;
        std::condition_variable cv;
        size_t left;
    };
    auto latch = std::make_shared<Latch>();
    latch->left = chunks;
    for (size_t c = 0; c < chunks; ++c) {
        const size_t b = n * c / chunks, e = n * (c + 1) / chunks;
        pool.submit(0, [latch, b, e, &fn] {
            fn(b, e);
            std::lock_guard<std::mutex> lk(latch->m);
            if (--latch->left == 0) latch->cv.notify_all();
        });
    }
    std::unique_lock<std::mutex> lk(latch->m);
    latch->cv.wait(lk, [&] { return latch->left == 0; });
}

// ---------------------------------------------------------------------------
// Eigenvectors: subspace iteration
// ---------------------------------------------------------------------------

namespace {

// Cyclic Jacobi on a small symmetric n x n matrix A (row-major, destroyed):
// eigenvalues in `vals`, eigenvectors as the columns of V, sorted descending.
void jacobi(std::vector<double>& A, int n, std::vector<double>& vals, std::vector<double>& V) {
    V.assign(size_t(n) * n, 0.0);
    for (int i = 0; i < n; ++i) V[size_t(i) * n + i] = 1.0;
    for (int sweep = 0; sweep < 100; ++sweep) {
        double off = 0, diag = 0;
        for (int i = 0; i < n; ++i)
            for (int j = 0; j < n; ++j) (i == j ? diag : off) += A[size_t(i) * n + j] * A[size_t(i) * n + j];
        if (off <= 1e-30 * std::max(diag, 1e-300)) break;
        for (int p = 0; p < n - 1; ++p)
            for (int q = p + 1; q < n; ++q) {
                const double apq = A[size_t(p) * n + q];
                if (std::fabs(apq) < 1e-300) continue;
                const double app = A[size_t(p) * n + p], aqq = A[size_t(q) * n + q];
                const double theta = (aqq - app) / (2 * apq);
                const double t = (theta >= 0 ? 1.0 : -1.0) / (std::fabs(theta) + std::sqrt(theta * theta + 1));
                const double c = 1 / std::sqrt(t * t + 1), s = t * c;
                for (int k = 0; k < n; ++k) { // A = J^T A J
                    const double akp = A[size_t(k) * n + p], akq = A[size_t(k) * n + q];
                    A[size_t(k) * n + p] = c * akp - s * akq;
                    A[size_t(k) * n + q] = s * akp + c * akq;
                }
                for (int k = 0; k < n; ++k) {
                    const double apk = A[size_t(p) * n + k], aqk = A[size_t(q) * n + k];
                    A[size_t(p) * n + k] = c * apk - s * aqk;
                    A[size_t(q) * n + k] = s * apk + c * aqk;
                }
                for (int k = 0; k < n; ++k) {
                    const double vkp = V[size_t(k) * n + p], vkq = V[size_t(k) * n + q];
                    V[size_t(k) * n + p] = c * vkp - s * vkq;
                    V[size_t(k) * n + q] = s * vkp + c * vkq;
                }
            }
    }
    std::vector<int> order(n);
    std::iota(order.begin(), order.end(), 0);
    std::sort(order.begin(), order.end(), [&](int a, int b) { return A[size_t(a) * n + a] > A[size_t(b) * n + b]; });
    vals.resize(n);
    std::vector<double> Vs(size_t(n) * n);
    for (int c = 0; c < n; ++c) {
        vals[c] = A[size_t(order[c]) * n + order[c]];
        for (int r = 0; r < n; ++r) Vs[size_t(r) * n + c] = V[size_t(r) * n + order[c]];
    }
    V.swap(Vs);
}

// Columns of Q (D x b, column j at Q[j*D]) made orthonormal (modified Gram-Schmidt, twice).
void orthonormalize(std::vector<double>& Q, int D, int b) {
    for (int pass = 0; pass < 2; ++pass)
        for (int j = 0; j < b; ++j) {
            double* qj = Q.data() + size_t(j) * D;
            for (int i = 0; i < j; ++i) {
                const double* qi = Q.data() + size_t(i) * D;
                double d = 0;
                for (int r = 0; r < D; ++r) d += qi[r] * qj[r];
                for (int r = 0; r < D; ++r) qj[r] -= d * qi[r];
            }
            double nn = 0;
            for (int r = 0; r < D; ++r) nn += qj[r] * qj[r];
            nn = std::sqrt(nn);
            if (nn < 1e-300) { // a degenerate direction: any unit vector orthogonal enough
                std::fill(qj, qj + D, 0.0);
                qj[j % D] = 1.0;
                continue;
            }
            for (int r = 0; r < D; ++r) qj[r] /= nn;
        }
}

} // namespace

int topEigen(const std::vector<double>& C, int D, int k, std::vector<double>& vecs, std::vector<double>& vals) {
    k = std::clamp(k, 1, D);
    const int b = std::min(D, k + 6); // guard vectors: convergence goes as (lambda_{b+1} / lambda_k)^iterations
    std::vector<double> Q(size_t(D) * b), Z(size_t(D) * b), H(size_t(b) * b), V, lam, prev(k, 0.0);
    std::mt19937_64 rng(7);
    std::normal_distribution<double> nd;
    for (double& v : Q) v = nd(rng);
    orthonormalize(Q, D, b);
    auto rayleighRitz = [&] {
        for (int j = 0; j < b; ++j) { // Z = C Q
            const double* q = Q.data() + size_t(j) * D;
            double* z = Z.data() + size_t(j) * D;
            for (int r = 0; r < D; ++r) {
                const double* c = C.data() + size_t(r) * D;
                double s = 0;
                for (int i = 0; i < D; ++i) s += c[i] * q[i];
                z[r] = s;
            }
        }
        for (int a = 0; a < b; ++a) // H = Q^T Z (symmetric)
            for (int c = a; c < b; ++c) {
                double s = 0;
                const double* qa = Q.data() + size_t(a) * D;
                const double* zc = Z.data() + size_t(c) * D;
                const double* qc = Q.data() + size_t(c) * D;
                const double* za = Z.data() + size_t(a) * D;
                for (int r = 0; r < D; ++r) s += qa[r] * zc[r] + qc[r] * za[r];
                H[size_t(a) * b + c] = H[size_t(c) * b + a] = 0.5 * s;
            }
        jacobi(H, b, lam, V);
    };
    int it = 0;
    for (it = 1; it <= 300; ++it) {
        rayleighRitz();
        // Power step on the Ritz vectors: Q = orth(Z V) = orth(C Q V).
        std::vector<double> next(size_t(D) * b, 0.0);
        for (int c = 0; c < b; ++c)
            for (int a = 0; a < b; ++a) {
                const double v = V[size_t(a) * b + c];
                const double* z = Z.data() + size_t(a) * D;
                double* o = next.data() + size_t(c) * D;
                for (int r = 0; r < D; ++r) o[r] += v * z[r];
            }
        // Converged when the k leading directions stop turning.
        double worst = 0;
        if (it > 1) {
            for (int c = 0; c < k; ++c) {
                double dot = 0, nn = 0;
                const double* o = next.data() + size_t(c) * D;
                const double* q = Q.data() + size_t(c) * D;
                for (int r = 0; r < D; ++r) {
                    dot += o[r] * q[r];
                    nn += o[r] * o[r];
                }
                worst = std::max(worst, 1.0 - std::fabs(dot) / std::sqrt(std::max(nn, 1e-300)));
            }
        }
        Q.swap(next);
        orthonormalize(Q, D, b);
        if (it > 1 && worst < 1e-12) break;
    }
    rayleighRitz(); // final Ritz pairs
    vecs.assign(size_t(k) * D, 0.0);
    vals.assign(lam.begin(), lam.begin() + k);
    for (int c = 0; c < k; ++c)
        for (int a = 0; a < b; ++a) {
            const double v = V[size_t(a) * b + c];
            const double* q = Q.data() + size_t(a) * D;
            for (int r = 0; r < D; ++r) vecs[size_t(c) * D + r] += v * q[r];
        }
    return it;
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

namespace {

struct BandInfo {
    bool hasNoData = false;
    double noData = 0, scale = 1, offset = 0;
};

std::vector<BandInfo> bandInfo(GDALDataset* ds, int D, int base) {
    std::vector<BandInfo> out(D);
    for (int b = 0; b < D; ++b) {
        GDALRasterBand* rb = ds->GetRasterBand(base + b + 1);
        int ok = 0;
        out[b].noData = rb->GetNoDataValue(&ok);
        out[b].hasNoData = ok != 0;
        out[b].scale = rb->GetScale(&ok);
        if (!ok) out[b].scale = 1;
        out[b].offset = rb->GetOffset(&ok);
        if (!ok) out[b].offset = 0;
    }
    return out;
}

// The file is read one block at a time at full resolution, in its own type
// (Int16, Byte...), every band in one call: each block is decompressed once.
// (Resampled or type-converting reads make GDAL go band by band on
// pixel-interleaved files, decompressing a block once per band when the cache
// cannot hold every band's share: 30 s instead of 0.5 s for TESSERA's 128 bands.)
template <class T>
void convertVector(const T* v, const std::vector<BandInfo>& bi, float* out) {
    for (size_t b = 0; b < bi.size(); ++b) {
        const BandInfo& m = bi[b];
        const double x = double(v[b]);
        out[b] = !std::isfinite(x) || (m.hasNoData && x == m.noData) ? NAN : float(x * m.scale + m.offset);
    }
}

class BlockReader {
public:
    BlockReader(GDALDataset* ds, const std::vector<BandInfo>& bi, int base) : ds_(ds), bi_(bi) {
        W = ds->GetRasterXSize();
        H = ds->GetRasterYSize();
        GDALRasterBand* b1 = ds->GetRasterBand(base + 1);
        b1->GetBlockSize(&bw, &bh);
        // Strips or odd layouts: windows of a reasonable size.
        if (bw < 16 || bw > 1024) bw = std::min(W, bw < 16 ? 256 : 1024);
        if (bh < 16) bh = std::min(H, 64);
        dt_ = b1->GetRasterDataType();
        if (dt_ != GDT_Byte && dt_ != GDT_Int8 && dt_ != GDT_Int16 && dt_ != GDT_UInt16 && dt_ != GDT_Float32)
            dt_ = GDT_Float32;
        bytes_ = GDALGetDataTypeSizeBytes(dt_);
        bands_.resize(bi.size());
        std::iota(bands_.begin(), bands_.end(), base + 1); // the date's bands
    }
    int W = 0, H = 0, bw = 0, bh = 0;
    // Reads the window (x0, y0, w, h) at full resolution; false on error.
    bool read(int x0, int y0, int w, int h) {
        const int D = int(bi_.size());
        x0_ = x0;
        y0_ = y0;
        w_ = w;
        raw_.resize(size_t(w) * h * D * bytes_);
        return ds_->RasterIO(GF_Read, x0, y0, w, h, raw_.data(), w, h, dt_, D, bands_.data(), GSpacing(D) * bytes_,
                             GSpacing(w) * D * bytes_, bytes_, nullptr) == CE_None;
    }
    // Source pixel (x, y) of the window last read: its D values, nodata -> NaN, scaled.
    void vector(int x, int y, float* out) const {
        const size_t i = (size_t(y - y0_) * w_ + (x - x0_)) * bi_.size() * bytes_;
        const unsigned char* p = raw_.data() + i;
        switch (dt_) {
        case GDT_Byte: convertVector(reinterpret_cast<const uint8_t*>(p), bi_, out); break;
        case GDT_Int8: convertVector(reinterpret_cast<const int8_t*>(p), bi_, out); break;
        case GDT_Int16: convertVector(reinterpret_cast<const int16_t*>(p), bi_, out); break;
        case GDT_UInt16: convertVector(reinterpret_cast<const uint16_t*>(p), bi_, out); break;
        default: convertVector(reinterpret_cast<const float*>(p), bi_, out);
        }
    }

private:
    GDALDataset* ds_;
    const std::vector<BandInfo>& bi_;
    GDALDataType dt_ = GDT_Float32;
    int bytes_ = 4;
    std::vector<int> bands_;
    std::vector<unsigned char> raw_;
    int x0_ = 0, y0_ = 0, w_ = 0;
};

} // namespace

EmbeddingStore::~EmbeddingStore() {
    cancel();
    if (starter_.joinable()) starter_.join();
    pool_.reset(); // waits for the running reads
}

void EmbeddingStore::cancel() { cancel_ = true; }

bool EmbeddingStore::failed() const {
    std::lock_guard<std::mutex> lk(m_);
    return !error_.empty();
}

std::string EmbeddingStore::error() const {
    std::lock_guard<std::mutex> lk(m_);
    return error_;
}

void EmbeddingStore::start(std::shared_ptr<const CubeInfo> inf, int64_t budgetBytes, int threads,
                           std::function<void()> wake) {
    info = std::move(inf);
    wake_ = std::move(wake);
    T = info->T();
    D = info->bandsPerDate;
    const double W = info->width, H = info->height;
    const double perPixel = double(T) * (D + 1 + sizeof(float));
    const double f = std::max(1.0, std::sqrt(W * H * perPixel / double(std::max<int64_t>(budgetBytes, 1 << 20))));
    w = std::max(1, int(std::ceil(W / f)));
    h = std::max(1, int(std::ceil(H / f)));
    fx = W / w;
    fy = H / h;
    ready_ = std::make_unique<std::atomic<bool>[]>(size_t(T));
    for (int t = 0; t < T; ++t) ready_[t] = false;
    try {
        q_.assign(size_t(w) * h * T * D, 0);
        valid_.assign(size_t(w) * h * T, 0);
        norm_.assign(size_t(w) * h * T, 0.0f);
    } catch (const std::bad_alloc&) {
        std::lock_guard<std::mutex> lk(m_);
        error_ = "not enough memory for the embeddings";
        return;
    }
    pool_ = std::make_unique<JobPool>(std::max(1, std::min(threads, T)));
    starter_ = std::thread([this] {
        const auto t0 = Clock::now();
        if (!computeRanges()) {
            if (wake_) wake_();
            return;
        }
        rangesReady = true;
        t0_ = t0;
        for (int t = T - 1; t >= 0; --t) // the pool runs the newest first: the first year starts first
            pool_->submit(0, [this, t] {
                if (!cancel_) readYear(t);
            });
    });
}

// Each dimension's range from the 0.1 and 99.9 percentiles of a small read of
// the middle year, widened by 5%: the Int8 steps cover what matters.
bool EmbeddingStore::computeRanges() {
    const Layer& L = info->layers[T / 2];
    GDALDataset* ds = GDALDataset::Open(L.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
    auto fail = [&](const std::string& why) {
        std::lock_guard<std::mutex> lk(m_);
        error_ = why;
        if (ds) GDALClose(ds);
        return false;
    };
    if (!ds) return fail(L.path + ": " + CPLGetLastErrorMsg());
    if (ds->GetRasterCount() < L.bandBase + D) return fail(L.path + ": fewer bands than the first year");
    const std::vector<BandInfo> bi = bandInfo(ds, D, L.bandBase);
    BlockReader rd(ds, bi, L.bandBase);
    std::vector<float> buf; // sampled vectors, pixel after pixel
    std::vector<float> v(D);
    const int nbx = (rd.W + rd.bw - 1) / rd.bw, nby = (rd.H + rd.bh - 1) / rd.bh;
    const int gx = std::min(nbx, 4), gy = std::min(nby, 4);
    for (int j = 0; j < gy; ++j)
        for (int i = 0; i < gx; ++i) {
            const int bx = int((i + 0.5) * nbx / gx), by = int((j + 0.5) * nby / gy);
            const int x0 = bx * rd.bw, y0 = by * rd.bh;
            const int w = std::min(rd.bw, rd.W - x0), h = std::min(rd.bh, rd.H - y0);
            if (!rd.read(x0, y0, w, h)) return fail(L.path + ": " + CPLGetLastErrorMsg());
            const int step = std::max(1, int(std::sqrt(double(w) * h / 4096.0)));
            for (int y = y0; y < y0 + h; y += step)
                for (int x = x0; x < x0 + w; x += step) {
                    rd.vector(x, y, v.data());
                    buf.insert(buf.end(), v.begin(), v.end());
                }
        }
    GDALClose(ds);
    ds = nullptr;
    const size_t sw = buf.size() / std::max(D, 1), sh = 1;
    scale.assign(D, 1.0f);
    offset.assign(D, 0.0f);
    std::vector<float> col;
    for (int b = 0; b < D; ++b) {
        col.clear();
        for (size_t p = 0; p < size_t(sw) * sh; ++p)
            if (std::isfinite(buf[p * D + b])) col.push_back(buf[p * D + b]);
        float lo = -1, hi = 1;
        if (!col.empty()) {
            const size_t i0 = size_t(0.001 * (col.size() - 1)), i1 = size_t(0.999 * (col.size() - 1));
            std::nth_element(col.begin(), col.begin() + i0, col.end());
            lo = col[i0];
            std::nth_element(col.begin(), col.begin() + i1, col.end());
            hi = col[i1];
        }
        const float pad = std::max(0.05f * (hi - lo), 1e-6f);
        lo -= pad;
        hi += pad;
        offset[b] = 0.5f * (lo + hi);
        scale[b] = (hi - lo) / 254.0f;
    }
    return true;
}

void EmbeddingStore::readYear(int t) {
    const Layer& L = info->layers[t];
    GDALDataset* ds = GDALDataset::Open(L.path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
    if (!ds || ds->GetRasterCount() < L.bandBase + D) {
        std::lock_guard<std::mutex> lk(m_);
        if (error_.empty()) error_ = L.path + ": " + (ds ? std::string("fewer bands than the first year") : CPLGetLastErrorMsg());
        if (ds) GDALClose(ds);
        return;
    }
    const std::vector<BandInfo> bi = bandInfo(ds, D, L.bandBase);
    BlockReader rd(ds, bi, L.bandBase);
    // The source pixel nearest to each store pixel's centre.
    std::vector<int> srcX(w), srcY(h);
    for (int x = 0; x < w; ++x) srcX[x] = std::min(rd.W - 1, int((x + 0.5) * fx));
    for (int y = 0; y < h; ++y) srcY[y] = std::min(rd.H - 1, int((y + 0.5) * fy));
    std::vector<float> inv(D), v(D);
    for (int b = 0; b < D; ++b) inv[b] = 1.0f / scale[b];
    for (int y0 = 0; y0 < rd.H && !cancel_; y0 += rd.bh) {
        const int y1 = std::min(rd.H, y0 + rd.bh);
        const int r0 = int(std::lower_bound(srcY.begin(), srcY.end(), y0) - srcY.begin());
        const int r1 = int(std::lower_bound(srcY.begin(), srcY.end(), y1) - srcY.begin());
        if (r0 >= r1) continue; // no store row samples this block row
        for (int x0 = 0; x0 < rd.W && !cancel_; x0 += rd.bw) {
            const int x1 = std::min(rd.W, x0 + rd.bw);
            const int c0 = int(std::lower_bound(srcX.begin(), srcX.end(), x0) - srcX.begin());
            const int c1 = int(std::lower_bound(srcX.begin(), srcX.end(), x1) - srcX.begin());
            if (c0 >= c1) continue;
            if (!rd.read(x0, y0, x1 - x0, y1 - y0)) {
                std::lock_guard<std::mutex> lk(m_);
                if (error_.empty()) error_ = L.path + ": " + CPLGetLastErrorMsg();
                GDALClose(ds);
                return;
            }
            for (int r = r0; r < r1; ++r)
                for (int c = c0; c < c1; ++c) {
                    rd.vector(srcX[c], srcY[r], v.data());
                    const size_t p = size_t(r) * w + c;
                    int8_t* q = q_.data() + (size_t(t) * pixels() + p) * D;
                    bool ok = true;
                    double nn = 0;
                    for (int b = 0; b < D; ++b) {
                        if (!std::isfinite(v[b])) {
                            ok = false;
                            break;
                        }
                        const float k = std::clamp(std::nearbyint((v[b] - offset[b]) * inv[b]), -127.0f, 127.0f);
                        q[b] = int8_t(k);
                        const double d = double(k) * scale[b] + offset[b];
                        nn += d * d;
                    }
                    if (!ok) std::fill(q, q + D, int8_t(0));
                    valid_[size_t(t) * pixels() + p] = ok ? 1 : 0;
                    norm_[size_t(t) * pixels() + p] = ok ? float(std::sqrt(nn)) : 0.0f;
                }
        }
    }
    GDALClose(ds);
    if (cancel_) return;
    ready_[t] = true;
    {
        std::lock_guard<std::mutex> lk(m_);
        if (done_ + 1 == T) seconds_ = msSince(t0_) / 1000.0;
        ++done_;
    }
    if (wake_) wake_();
}

void EmbeddingStore::dequant(int t, size_t p, float* out) const {
    const int8_t* q = vec(t, p);
    for (int b = 0; b < D; ++b) out[b] = q[b] * scale[b] + offset[b];
}

bool EmbeddingStore::pixelOf(double lx, double ly, size_t& p) const {
    const int sx = int(std::floor(lx / fx)), sy = int(std::floor(ly / fy));
    if (sx < 0 || sy < 0 || sx >= w || sy >= h) return false;
    p = size_t(sy) * w + sx;
    return true;
}

// ---------------------------------------------------------------------------
// PCA
// ---------------------------------------------------------------------------

std::vector<std::pair<int, uint32_t>> sampleVectors(const EmbeddingStore& s, const int* rect, size_t maxN,
                                                    bool allYears, int t) {
    std::vector<int> years;
    for (int y = 0; y < s.T; ++y)
        if ((allYears || y == t) && s.yearReady(y)) years.push_back(y);
    int x0 = 0, y0 = 0, x1 = s.w, y1 = s.h;
    if (rect) {
        x0 = std::clamp(rect[0], 0, s.w);
        y0 = std::clamp(rect[1], 0, s.h);
        x1 = std::clamp(rect[2], 0, s.w);
        y1 = std::clamp(rect[3], 0, s.h);
    }
    std::vector<std::pair<int, uint32_t>> out;
    const size_t area = size_t(std::max(0, x1 - x0)) * std::max(0, y1 - y0);
    const size_t total = area * years.size();
    if (total == 0 || maxN == 0) return out;
    auto at = [&](size_t i) {
        const int y = years[i / area];
        const size_t a = i % area;
        return std::pair<int, uint32_t>(y, uint32_t((y0 + a / (x1 - x0)) * s.w + x0 + a % (x1 - x0)));
    };
    if (total <= maxN) { // every valid vector
        for (size_t i = 0; i < total; ++i) {
            const auto yp = at(i);
            if (s.valid(yp.first, yp.second)) out.push_back(yp);
        }
        return out;
    }
    // Evenly spread with a fixed jitter (the same request, the same sample).
    std::mt19937_64 rng(12345);
    const double step = double(total) / double(maxN);
    out.reserve(maxN);
    for (size_t k = 0; k < maxN; ++k) {
        size_t i = size_t((k + std::uniform_real_distribution<double>(0, 1)(rng)) * step);
        for (int tries = 0; tries < 8 && i < total; ++tries, i += std::max<size_t>(1, size_t(step / 8))) {
            const auto yp = at(i);
            if (s.valid(yp.first, yp.second)) {
                out.push_back(yp);
                break;
            }
        }
    }
    return out;
}

PcaBasis fitPca(const EmbeddingStore& s, const std::vector<std::pair<int, uint32_t>>& sample, int k,
                std::array<int, 3> shown, float stretchPct, JobPool& pool) {
    const auto t0 = Clock::now();
    PcaBasis B;
    const int D = s.D, Dp = (D + 3) & ~3; // syrk works on rows in fours
    B.D = D;
    B.k = std::clamp(k, 1, D);
    B.shown = shown;
    B.samples = int(sample.size());
    const size_t N = sample.size();
    if (N < 2) return B;

    // Mean: integer sums of the Int8 codes, dequantised once (s * sum q + n * o).
    std::vector<int64_t> qsum(D, 0);
    std::mutex mm;
    parallelFor(pool, N, 8192, [&](size_t b, size_t e) {
        std::vector<int32_t> part(D, 0); // 8192 x 127 fits in 32 bits
        for (size_t i = b; i < e; ++i) {
            const int8_t* q = s.vec(sample[i].first, sample[i].second);
            for (int d = 0; d < D; ++d) part[d] += q[d];
        }
        std::lock_guard<std::mutex> lk(mm);
        for (int d = 0; d < D; ++d) qsum[d] += part[d];
    });
    std::vector<double> mean(D);
    B.mean.resize(D);
    for (int d = 0; d < D; ++d) {
        mean[d] = double(s.scale[d]) * double(qsum[d]) / double(N) + s.offset[d];
        B.mean[d] = float(mean[d]);
    }

    // Covariance: blocks of 256 centred vectors transposed to D rows, X X^T by
    // the SIMD kernel in float, each block's result added in double.
    const auto tc = Clock::now();
    constexpr int kBlock = 256;
    std::vector<double> C(size_t(Dp) * Dp, 0.0);
    parallelFor(pool, (N + kBlock - 1) / kBlock, 4, [&](size_t b, size_t e) {
        std::vector<float> X(size_t(Dp) * kBlock), Cb(size_t(Dp) * Dp);
        std::vector<double> local(size_t(Dp) * Dp, 0.0);
        std::vector<float> sc(s.scale), sh(D); // centred value = q * scale + (offset - mean)
        for (int d = 0; d < D; ++d) sh[d] = float(s.offset[d] - mean[d]);
        for (size_t blk = b; blk < e; ++blk) {
            std::fill(X.begin(), X.end(), 0.0f);
            const size_t i0 = blk * kBlock, i1 = std::min(N, i0 + kBlock);
            for (size_t i = i0; i < i1; ++i) {
                const int8_t* q = s.vec(sample[i].first, sample[i].second);
                float* col = X.data() + (i - i0);
                for (int d = 0; d < D; ++d) col[size_t(d) * kBlock] = q[d] * sc[d] + sh[d];
            }
            std::fill(Cb.begin(), Cb.end(), 0.0f);
            embsimd::syrk(X.data(), Dp, kBlock, kBlock, Cb.data());
            for (int r = 0; r < D; ++r)
                for (int c = r; c < D; ++c) local[size_t(r) * Dp + c] += Cb[size_t(r) * Dp + c];
        }
        std::lock_guard<std::mutex> lk(mm);
        for (size_t i = 0; i < local.size(); ++i) C[i] += local[i];
    });
    std::vector<double> Cd(size_t(D) * D);
    for (int r = 0; r < D; ++r)
        for (int c = r; c < D; ++c) Cd[size_t(r) * D + c] = Cd[size_t(c) * D + r] = C[size_t(r) * Dp + c] / double(N - 1);
    B.covMs = msSince(tc);
    for (int d = 0; d < D; ++d) B.total += Cd[size_t(d) * D + d];

    // Leading eigenvectors; each one's sign fixed (its largest loading positive),
    // so the colours do not flip when the sample changes a little.
    const auto te = Clock::now();
    std::vector<double> vecs;
    B.iterations = topEigen(Cd, D, B.k, vecs, B.eig);
    B.eigMs = msSince(te);
    B.comps.assign(size_t(B.k) * D, 0.0f);
    for (int c = 0; c < B.k; ++c) {
        const double* v = vecs.data() + size_t(c) * D;
        int big = 0;
        for (int d = 1; d < D; ++d)
            if (std::fabs(v[d]) > std::fabs(v[big])) big = d;
        const double sign = v[big] < 0 ? -1.0 : 1.0;
        for (int d = 0; d < D; ++d) B.comps[size_t(c) * D + d] = float(sign * v[d]);
    }

    // Stretch of the shown components: percentiles of the sample's projections.
    for (int& c : B.shown) c = std::clamp(c, 0, B.k - 1);
    std::vector<float> W(size_t(3) * D), bias(3, 0.0f);
    for (int c = 0; c < 3; ++c)
        for (int d = 0; d < D; ++d) {
            const float cd = B.comps[size_t(B.shown[c]) * D + d];
            W[size_t(c) * D + d] = cd * s.scale[d];
            bias[c] += (s.offset[d] - B.mean[d]) * cd;
        }
    std::vector<float> proj(N * 3);
    parallelFor(pool, N, 4096, [&](size_t b, size_t e) { // gathered, then one kernel call per 512
        constexpr size_t kChunk = 512;
        std::vector<int8_t> g(kChunk * D);
        for (size_t i0 = b; i0 < e; i0 += kChunk) {
            const size_t m = std::min(kChunk, e - i0);
            for (size_t i = 0; i < m; ++i)
                std::memcpy(&g[i * D], s.vec(sample[i0 + i].first, sample[i0 + i].second), size_t(D));
            embsimd::project(g.data(), m, D, W.data(), 3, bias.data(), &proj[i0 * 3]);
        }
    });
    const float p = std::clamp(stretchPct, 0.0f, 49.0f) / 100.0f;
    parallelFor(pool, 3, 1, [&](size_t b, size_t e) {
        std::vector<float> col(N);
        for (size_t c = b; c < e; ++c) {
            for (size_t i = 0; i < N; ++i) col[i] = proj[i * 3 + c];
            const size_t i0 = size_t(p * (N - 1)), i1 = size_t((1 - p) * (N - 1));
            std::nth_element(col.begin(), col.begin() + i0, col.end());
            B.lo[c] = col[i0];
            std::nth_element(col.begin() + i0, col.begin() + i1, col.end()); // above i0: a smaller range
            B.hi[c] = std::max(col[i1], B.lo[c] + 1e-12f);
        }
    });
    B.ms = msSince(t0);
    return B;
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

void renderPcaRgb(const EmbeddingStore& s, int t, const PcaBasis& B, JobPool& pool, std::vector<uint8_t>& rgba) {
    const int D = s.D;
    const size_t n = s.pixels();
    rgba.assign(n * 4, 0);
    if (B.comps.empty()) return;
    // Scale, offset, mean and stretch folded into the weights: 3 dot products per pixel.
    std::vector<float> W(size_t(3) * D), bias(3, 0.0f);
    for (int c = 0; c < 3; ++c) {
        const float k = 1.0f / (B.hi[c] - B.lo[c]);
        for (int d = 0; d < D; ++d) {
            const float cd = B.comps[size_t(B.shown[c]) * D + d];
            W[size_t(c) * D + d] = cd * s.scale[d] * k;
            bias[c] += (s.offset[d] - B.mean[d]) * cd * k;
        }
        bias[c] -= B.lo[c] * k;
    }
    const int8_t* q = s.year(t);
    const uint8_t* valid = s.validYear(t);
    parallelFor(pool, n, 8192, [&](size_t b, size_t e) {
        constexpr size_t kChunk = 1024;
        float out[kChunk * 3];
        for (size_t i0 = b; i0 < e; i0 += kChunk) {
            const size_t m = std::min(kChunk, e - i0);
            embsimd::project(q + i0 * D, m, D, W.data(), 3, bias.data(), out);
            for (size_t i = 0; i < m; ++i) {
                uint8_t* px = &rgba[(i0 + i) * 4];
                if (!valid[i0 + i]) continue; // transparent
                for (int c = 0; c < 3; ++c) px[c] = uint8_t(std::clamp(out[i * 3 + c], 0.0f, 1.0f) * 255.0f + 0.5f);
                px[3] = 255;
            }
        }
    });
}

void renderSimilarity(const EmbeddingStore& s, int t, const std::vector<float>& ref, JobPool& pool,
                      std::vector<float>& out) {
    const int D = s.D;
    const size_t n = s.pixels();
    out.assign(n, NAN);
    double rn = 0;
    for (float v : ref) rn += double(v) * v;
    rn = std::sqrt(rn);
    if (int(ref.size()) != D || rn <= 0) return;
    std::vector<float> W(D);
    float bias = 0;
    for (int d = 0; d < D; ++d) {
        W[d] = float(ref[d] / rn) * s.scale[d];
        bias += float(ref[d] / rn) * s.offset[d];
    }
    const int8_t* q = s.year(t);
    const uint8_t* valid = s.validYear(t);
    const float* nrm = s.normYear(t);
    parallelFor(pool, n, 8192, [&](size_t b, size_t e) {
        constexpr size_t kChunk = 1024;
        float dot[kChunk];
        for (size_t i0 = b; i0 < e; i0 += kChunk) {
            const size_t m = std::min(kChunk, e - i0);
            embsimd::project(q + i0 * D, m, D, W.data(), 1, &bias, dot);
            for (size_t i = 0; i < m; ++i)
                if (valid[i0 + i] && nrm[i0 + i] > 0) out[i0 + i] = dot[i] / nrm[i0 + i];
        }
    });
}

void renderChange(const EmbeddingStore& s, int t, int t0, JobPool& pool, std::vector<float>& out) {
    const int D = s.D;
    const size_t n = s.pixels();
    out.assign(n, NAN);
    if (t0 < 0 || t0 >= s.T) return;
    const int8_t* a = s.year(t);
    const int8_t* c = s.year(t0);
    const uint8_t* va = s.validYear(t);
    const uint8_t* vc = s.validYear(t0);
    const float* na = s.normYear(t);
    const float* nc = s.normYear(t0);
    parallelFor(pool, n, 8192, [&](size_t b, size_t e) {
        constexpr size_t kChunk = 1024;
        float dot[kChunk];
        for (size_t i0 = b; i0 < e; i0 += kChunk) {
            const size_t m = std::min(kChunk, e - i0);
            embsimd::dotDequant(a + i0 * D, c + i0 * D, m, D, s.scale.data(), s.offset.data(), dot);
            for (size_t i = 0; i < m; ++i) {
                const size_t p = i0 + i;
                if (va[p] && vc[p] && na[p] > 0 && nc[p] > 0) out[p] = 1.0f - dot[i] / (na[p] * nc[p]);
            }
        }
    });
}

void percentileRange(const std::vector<float>& v, float pLo, float pHi, float& lo, float& hi) {
    std::vector<float> f;
    const size_t step = std::max<size_t>(1, v.size() / 200000);
    for (size_t i = 0; i < v.size(); i += step)
        if (std::isfinite(v[i])) f.push_back(v[i]);
    if (f.empty()) {
        lo = 0;
        hi = 1;
        return;
    }
    const size_t i0 = size_t(pLo / 100.0f * (f.size() - 1)), i1 = size_t(pHi / 100.0f * (f.size() - 1));
    std::nth_element(f.begin(), f.begin() + i0, f.end());
    lo = f[i0];
    std::nth_element(f.begin(), f.begin() + i1, f.end());
    hi = std::max(f[i1], lo + 1e-6f);
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

EmbeddingEngine::EmbeddingEngine(int threads, std::function<void()> wake)
    : wake_(std::move(wake)), pool_(std::make_unique<JobPool>(std::max(1, threads))) {
    thread_ = std::thread([this] { loop(); });
}

EmbeddingEngine::~EmbeddingEngine() {
    {
        std::lock_guard<std::mutex> lk(m_);
        stop_ = true;
        waiting_.clear();
    }
    cv_.notify_all();
    if (thread_.joinable()) thread_.join();
    pool_.reset();
}

void EmbeddingEngine::submit(const std::string& slot, std::function<void()> fn) {
    {
        std::lock_guard<std::mutex> lk(m_);
        waiting_[slot] = std::move(fn);
    }
    cv_.notify_all();
}

bool EmbeddingEngine::busy() const {
    std::lock_guard<std::mutex> lk(m_);
    return running_ || !waiting_.empty();
}

void EmbeddingEngine::setThreads(int n) { pool_->setThreads(std::max(1, n)); }

void EmbeddingEngine::loop() {
    for (;;) {
        std::function<void()> fn;
        {
            std::unique_lock<std::mutex> lk(m_);
            cv_.wait(lk, [this] { return stop_ || !waiting_.empty(); });
            if (stop_) return;
            auto it = waiting_.begin(); // "basis" sorts before "image": a new basis first
            fn = std::move(it->second);
            waiting_.erase(it);
            running_ = true;
        }
        fn();
        {
            std::lock_guard<std::mutex> lk(m_);
            running_ = false;
        }
        if (wake_) wake_();
    }
}

// ---------------------------------------------------------------------------
// Self-test
// ---------------------------------------------------------------------------

namespace {

// A store filled in memory (no files): T years of w x h vectors.
struct SyntheticStore : EmbeddingStore {
    void fill(int w_, int h_, int T_, int D_, const std::function<void(int, size_t, float*)>& gen, float range) {
        w = w_;
        h = h_;
        T = T_;
        D = D_;
        scale.assign(D, range / 127.0f);
        offset.assign(D, 0.0f);
        ready_ = std::make_unique<std::atomic<bool>[]>(size_t(T));
        q_.assign(size_t(w) * h * T * D, 0);
        valid_.assign(size_t(w) * h * T, 1);
        norm_.assign(size_t(w) * h * T, 0.0f);
        std::vector<float> v(D);
        for (int t = 0; t < T; ++t) {
            for (size_t p = 0; p < pixels(); ++p) {
                gen(t, p, v.data());
                int8_t* q = q_.data() + (size_t(t) * pixels() + p) * D;
                double nn = 0;
                for (int b = 0; b < D; ++b) {
                    q[b] = int8_t(std::clamp(std::nearbyint(v[b] / scale[b]), -127.0f, 127.0f));
                    nn += double(q[b] * scale[b]) * (q[b] * scale[b]);
                }
                norm_[size_t(t) * pixels() + p] = float(std::sqrt(nn));
            }
            ready_[t] = true;
            ++done_;
        }
        rangesReady = true;
    }
};

} // namespace

const char* embeddingSelfTest(std::string& report, int threads) {
    char line[256];
    auto add = [&](const char* fmt, auto... args) {
        std::snprintf(line, sizeof(line), fmt, args...);
        report += line;
        report += '\n';
    };
    JobPool pool(std::max(1, threads));
    std::mt19937 rng(1);
    std::normal_distribution<float> nd;
    add("SIMD kernels: %s, %d threads", embsimd::name(), pool.threads());

    // 1. Kernels against the scalar ones.
    for (int D : {64, 128, 67}) {
        const int Dp = (D + 3) & ~3, n = 256;
        std::vector<float> X(size_t(Dp) * n);
        for (float& v : X) v = nd(rng);
        std::vector<float> C1(size_t(Dp) * Dp, 0.0f), C2 = C1;
        embsimd::syrk(X.data(), Dp, n, n, C1.data());
        embsimd::forceScalar(true);
        embsimd::syrk(X.data(), Dp, n, n, C2.data());
        embsimd::forceScalar(false);
        double worst = 0;
        for (int r = 0; r < Dp; ++r)
            for (int c = r; c < Dp; ++c)
                worst = std::max(worst, double(std::fabs(C1[size_t(r) * Dp + c] - C2[size_t(r) * Dp + c])) /
                                            std::max(1.0f, std::fabs(C2[size_t(r) * Dp + c])));
        if (worst > 1e-4) return "syrk kernel differs from the scalar one";
        std::vector<int8_t> q(size_t(1000) * D);
        for (int8_t& v : q) v = int8_t(std::clamp(int(nd(rng) * 40), -127, 127));
        std::vector<float> W(size_t(3) * D), bias = {0.5f, -1.0f, 2.0f}, s(D), o(D);
        for (float& v : W) v = nd(rng);
        for (int d = 0; d < D; ++d) {
            s[d] = 0.01f + 0.001f * d;
            o[d] = 0.1f * nd(rng);
        }
        for (int K = 1; K <= 3; ++K) {
            std::vector<float> a(1000 * K), b(1000 * K);
            embsimd::project(q.data(), 1000, D, W.data(), K, bias.data(), a.data());
            embsimd::forceScalar(true);
            embsimd::project(q.data(), 1000, D, W.data(), K, bias.data(), b.data());
            embsimd::forceScalar(false);
            for (size_t i = 0; i < a.size(); ++i)
                if (std::fabs(a[i] - b[i]) > 1e-3f * std::max(1.0f, std::fabs(b[i]))) return "project kernel differs";
        }
        std::vector<float> a(999), b(999);
        embsimd::dotDequant(q.data(), q.data() + D, 999, D, s.data(), o.data(), a.data());
        embsimd::forceScalar(true);
        embsimd::dotDequant(q.data(), q.data() + D, 999, D, s.data(), o.data(), b.data());
        embsimd::forceScalar(false);
        for (size_t i = 0; i < a.size(); ++i)
            if (std::fabs(a[i] - b[i]) > 1e-3f * std::max(1.0f, std::fabs(b[i]))) return "dotDequant kernel differs";
    }
    add("kernels: syrk, project, dotDequant equal the scalar ones (D = 64, 128, 67)");

    // 2. topEigen on a known spectrum: C = Q diag(l) Q^T.
    {
        const int D = 128;
        std::vector<double> Q(size_t(D) * D);
        std::normal_distribution<double> ndd;
        std::mt19937_64 r64(3);
        for (double& v : Q) v = ndd(r64);
        orthonormalize(Q, D, D); // columns: an orthonormal basis
        std::vector<double> lam(D);
        for (int i = 0; i < D; ++i) lam[i] = i < 3 ? 10.0 / (i + 1) : 2.0 * std::pow(0.95, i);
        std::vector<double> C(size_t(D) * D, 0.0);
        for (int r = 0; r < D; ++r)
            for (int c = 0; c < D; ++c) {
                double s = 0;
                for (int i = 0; i < D; ++i) s += Q[size_t(i) * D + r] * lam[i] * Q[size_t(i) * D + c];
                C[size_t(r) * D + c] = s;
            }
        const auto t0 = Clock::now();
        std::vector<double> vecs, vals;
        const int it = topEigen(C, D, 3, vecs, vals);
        const double ms = msSince(t0);
        double errV = 0, errL = 0;
        for (int c = 0; c < 3; ++c) {
            double dot = 0;
            for (int r = 0; r < D; ++r) dot += vecs[size_t(c) * D + r] * Q[size_t(c) * D + r];
            errV = std::max(errV, 1.0 - std::fabs(dot));
            errL = std::max(errL, std::fabs(vals[c] - lam[c]) / lam[c]);
        }
        add("topEigen (D = 128, top 3): %d iterations, %.2f ms; eigenvalue error %.1e, vector error %.1e", it, ms,
            errL, errV);
        if (errL > 1e-9 || errV > 1e-9) return "topEigen did not find the known eigenpairs";
    }

    // 3. fitPca on vectors with known principal directions, and the timings of
    // every step on a store of 1000 x 1000 pixels.
    for (int D : {64, 128}) {
        const int w = 1000, h = 1000, T = 1;
        std::vector<float> dirs(size_t(3) * D);
        for (float& v : dirs) v = nd(rng);
        for (int c = 0; c < 3; ++c) { // orthonormal directions
            float* d = &dirs[size_t(c) * D];
            for (int i = 0; i < c; ++i) {
                const float* e = &dirs[size_t(i) * D];
                float dot = 0;
                for (int k = 0; k < D; ++k) dot += d[k] * e[k];
                for (int k = 0; k < D; ++k) d[k] -= dot * e[k];
            }
            float nn = 0;
            for (int k = 0; k < D; ++k) nn += d[k] * d[k];
            for (int k = 0; k < D; ++k) d[k] /= std::sqrt(nn);
        }
        const float sd[3] = {3.0f, 2.0f, 1.2f};
        SyntheticStore st;
        std::mt19937 g(5);
        std::normal_distribution<float> n01;
        st.fill(w, h, T, D, [&](int, size_t, float* v) {
            const float a = n01(g) * sd[0], b = n01(g) * sd[1], c = n01(g) * sd[2];
            for (int k = 0; k < D; ++k) v[k] = a * dirs[k] + b * dirs[D + k] + c * dirs[2 * D + k] + 0.15f * n01(g);
        }, 6.0f);
        const auto ts = Clock::now();
        const auto sample = sampleVectors(st, nullptr, 65536, true, 0);
        const double sampleMs = msSince(ts);
        PcaBasis B = fitPca(st, sample, 3, {0, 1, 2}, 2.0f, pool);
        double worst = 0;
        for (int c = 0; c < 3; ++c) {
            double dot = 0;
            for (int k = 0; k < D; ++k) dot += double(B.comps[size_t(c) * D + k]) * dirs[size_t(c) * D + k];
            worst = std::max(worst, 1.0 - std::fabs(dot));
        }
        std::vector<uint8_t> rgba;
        auto t1 = Clock::now();
        renderPcaRgb(st, 0, B, pool, rgba);
        const double rgbMs = msSince(t1);
        std::vector<float> ref(D), out;
        st.dequant(0, 12345, ref.data());
        t1 = Clock::now();
        renderSimilarity(st, 0, ref, pool, out);
        const double simMs = msSince(t1);
        if (std::fabs(out[12345] - 1.0f) > 1e-4f) return "similarity of a vector to itself is not 1";
        // Scalar timings for comparison.
        embsimd::forceScalar(true);
        PcaBasis Bs = fitPca(st, sample, 3, {0, 1, 2}, 2.0f, pool);
        t1 = Clock::now();
        renderPcaRgb(st, 0, Bs, pool, rgba);
        const double rgbScalarMs = msSince(t1);
        embsimd::forceScalar(false);
        add("D = %3d: PCA of %d vectors in %.1f ms (sample %.1f, covariance %.1f [scalar %.1f], eigen %.2f, %d it.); "
            "direction error %.1e", D, B.samples, B.ms, sampleMs, B.covMs, Bs.covMs, B.eigMs, B.iterations, worst);
        add("        1 Mpx year: RGB %.1f ms [scalar %.1f], similarity %.1f ms", rgbMs, rgbScalarMs, simMs);
        if (worst > 1e-3) return "fitPca did not find the known directions";
    }
    return nullptr;
}
