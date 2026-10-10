#pragma once

// Foundation-model embeddings (TESSERA, AlphaEarth...): one file per year with D
// bands, each pixel a D-dimensional vector with no physical unit. Janus shows
// them through their principal components (RGB), the cosine similarity to a
// reference vector, or the change from one year to the next; everything is
// computed on the CPU from a reduced copy of every year (EmbeddingStore) and
// drawn as an image.

#include <array>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "cube.hpp"

class JobPool;

// What the layer holds, from Zeit's ZEIT_EMBEDDING tag (written by Janus'
// download and by zeit.save_raster) or, without it, from its bands (16+ bands
// per date named A00, A01...).
struct EmbeddingMeta {
    bool is = false;
    std::string source;      // "alphaearth", "tessera" ("" = recognised by its bands only)
    std::string model, version, license, attribution;
    int dims = 0;
    std::string label() const; // e.g. "AlphaEarth Foundations, 64 dimensions"
};
EmbeddingMeta embeddingMeta(const CubeInfo& info);

// SIMD kernels (embedding_simd.cpp), chosen once at run time: AVX2+FMA, SSE2,
// NEON or scalar.
namespace embsimd {
const char* name();
void forceScalar(bool on); // the plain C++ kernels (self-test, comparisons)
// C (D x D row-major; the upper triangle with the diagonal is complete) += X X^T,
// X: D rows of n floats, row stride ld. D % 4 == 0, n % 8 == 0.
void syrk(const float* X, int D, int n, int ld, float* C);
// out[i*K + k] = bias[k] + sum_b q[i*D + b] * W[k*D + b] for i < n, K <= 4.
void project(const int8_t* q, size_t n, int D, const float* W, int K, const float* bias, float* out);
// out[i] = sum_b (A[i*D+b] s[b] + o[b]) (B[i*D+b] s[b] + o[b]).
void dotDequant(const int8_t* A, const int8_t* B, size_t n, int D, const float* s, const float* o, float* out);
} // namespace embsimd

// Runs fn(begin, end) over [0, n) split among the pool's threads; returns when all are done.
void parallelFor(JobPool& pool, size_t n, size_t grain, const std::function<void(size_t, size_t)>& fn);

// Top k eigenpairs of a symmetric D x D matrix (row-major) by subspace iteration
// (block power iteration on k + 4 vectors with a Rayleigh-Ritz step, Jacobi on
// the small matrix), eigenvalues descending. Returns the iterations used.
int topEigen(const std::vector<double>& C, int D, int k, std::vector<double>& vecs, std::vector<double>& vals);

// Every year at reduced resolution: [year][pixel][dimension] Int8 with a scale
// and offset per dimension (value = q * scale + offset), a validity flag and the
// norm of each vector. Read in the background, a year per job; the dimensions'
// ranges come first from a small read of the middle year.
class EmbeddingStore {
public:
    ~EmbeddingStore();
    // budgetBytes: for q + valid + norm of every year (the resolution follows).
    void start(std::shared_ptr<const CubeInfo> info, int64_t budgetBytes, int threads, std::function<void()> wake);
    void cancel();

    std::shared_ptr<const CubeInfo> info;
    int w = 0, h = 0, T = 0, D = 0;
    double fx = 1, fy = 1;                 // source pixels per store pixel (x, y)
    std::vector<float> scale, offset;      // per dimension
    std::atomic<bool> rangesReady{false};

    bool yearReady(int t) const { return t >= 0 && t < T && ready_[t].load(); }
    int yearsDone() const { return done_.load(); }
    bool complete() const { return T > 0 && done_.load() == T; }
    bool failed() const;
    std::string error() const;
    double seconds() const { return seconds_.load(); }
    size_t bytes() const { return q_.size() + valid_.size() + norm_.size() * sizeof(float); }

    size_t pixels() const { return size_t(w) * h; }
    const int8_t* vec(int t, size_t p) const { return q_.data() + (size_t(t) * pixels() + p) * D; }
    const int8_t* year(int t) const { return vec(t, 0); }
    bool valid(int t, size_t p) const { return valid_[size_t(t) * pixels() + p] != 0; }
    const uint8_t* validYear(int t) const { return valid_.data() + size_t(t) * pixels(); }
    float norm(int t, size_t p) const { return norm_[size_t(t) * pixels() + p]; }
    const float* normYear(int t) const { return norm_.data() + size_t(t) * pixels(); }
    void dequant(int t, size_t p, float* out) const;
    // Store pixel of a layer pixel (continuous coordinates); false outside.
    bool pixelOf(double lx, double ly, size_t& p) const;

protected: // the self-test fills a store in memory
    void readYear(int t);
    bool computeRanges();
    std::vector<int8_t> q_;
    std::vector<uint8_t> valid_;
    std::vector<float> norm_;
    std::unique_ptr<std::atomic<bool>[]> ready_;
    std::atomic<int> done_{0};
    std::atomic<bool> cancel_{false};
    std::atomic<double> seconds_{0};
    std::chrono::steady_clock::time_point t0_;
    mutable std::mutex m_;
    std::string error_;
    std::function<void()> wake_;
    std::unique_ptr<JobPool> pool_;
    std::thread starter_;
};

// Principal components of a set of vectors.
struct PcaBasis {
    int D = 0, k = 0;
    std::vector<float> mean;    // D
    std::vector<float> comps;   // k x D, unit length; sign fixed: the largest loading positive
    std::vector<double> eig;    // variance along each component
    double total = 0;           // total variance (sum over the dimensions)
    int samples = 0, iterations = 0;
    int years = 0;              // years the sample was drawn from
    double ms = 0, covMs = 0, eigMs = 0;
    std::array<int, 3> shown{0, 1, 2}; // components drawn as R, G, B
    float lo[3] = {0, 0, 0}, hi[3] = {1, 1, 1}; // their stretch (percentiles of the sample)
};

// Up to maxN valid (year, pixel) pairs: every year (allYears) or year t, inside
// rect (store pixels x0, y0, x1, y1; nullptr = everywhere), evenly spread, with
// a fixed seed (the same request gives the same sample).
std::vector<std::pair<int, uint32_t>> sampleVectors(const EmbeddingStore& s, const int* rect, size_t maxN,
                                                    bool allYears, int t);
// PCA of the sample: covariance in parallel with the SIMD kernel, then topEigen.
// `shown` and `stretchPct` set the drawn components and their percentile stretch.
PcaBasis fitPca(const EmbeddingStore& s, const std::vector<std::pair<int, uint32_t>>& sample, int k,
                std::array<int, 3> shown, float stretchPct, JobPool& pool);

// Year t as RGBA8 (store size): the three shown components, stretched; invalid
// pixels transparent.
void renderPcaRgb(const EmbeddingStore& s, int t, const PcaBasis& b, JobPool& pool, std::vector<uint8_t>& rgba);
// Cosine similarity of every vector of year t to `ref` (D values); NaN where invalid.
void renderSimilarity(const EmbeddingStore& s, int t, const std::vector<float>& ref, JobPool& pool,
                      std::vector<float>& out);
// Cosine distance (1 - cosine similarity) between years t and t0; NaN where either is invalid.
void renderChange(const EmbeddingStore& s, int t, int t0, JobPool& pool, std::vector<float>& out);
// pLo / pHi percentiles of the finite values (a sample of them).
void percentileRange(const std::vector<float>& v, float pLo, float pHi, float& lo, float& hi);

// Background computations of one embedding layer: one worker thread that runs
// the newest request of each slot ("basis", "image") and posts the results; the
// heavy loops inside run on the pool's threads.
class EmbeddingEngine {
public:
    EmbeddingEngine(int threads, std::function<void()> wake);
    ~EmbeddingEngine();
    // Replaces what is waiting in `slot` (not what runs).
    void submit(const std::string& slot, std::function<void()> fn);
    bool busy() const;
    JobPool& pool() { return *pool_; }
    void setThreads(int n);

private:
    void loop();
    std::function<void()> wake_;
    std::unique_ptr<JobPool> pool_;
    mutable std::mutex m_;
    std::condition_variable cv_;
    std::map<std::string, std::function<void()>> waiting_;
    bool running_ = false, stop_ = false;
    std::thread thread_;
};

// --selftest-embeddings: kernels against the scalar ones, topEigen and fitPca
// against a known spectrum, timings. Prints a report; nullptr = passed.
const char* embeddingSelfTest(std::string& report, int threads);
