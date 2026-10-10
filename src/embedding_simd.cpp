// SIMD kernels of the embedding views (embedding.hpp), chosen once at run time:
// AVX2 + FMA when the CPU (and the OS) has them, else SSE2 on x86-64; NEON on
// ARM64; plain C++ elsewhere. The build needs no -mavx2 / /arch flag: the AVX2
// functions are compiled for it on their own (GCC/Clang target attribute; MSVC
// takes the intrinsics in any function) and only called after the CPUID check.
#include "embedding.hpp"

#include <cstring>

#if defined(__x86_64__) || defined(_M_X64)
#define EMB_X86 1
#include <immintrin.h>
#ifdef _MSC_VER
#include <intrin.h>
#define EMB_AVX2 /* MSVC: AVX2 intrinsics need no attribute */
#else
#include <cpuid.h>
#define EMB_AVX2 __attribute__((target("avx2,fma")))
#endif
#elif defined(__aarch64__) || defined(_M_ARM64)
#define EMB_NEON 1
#include <arm_neon.h>
#endif

namespace embsimd {
namespace {

// ---------------------------------------------------------------------------
// Scalar references (also the tails of the vector loops)
// ---------------------------------------------------------------------------

void syrkScalar(const float* X, int D, int n, int ld, float* C) {
    for (int i = 0; i < D; ++i)
        for (int j = i; j < D; ++j) {
            const float* a = X + size_t(i) * ld;
            const float* b = X + size_t(j) * ld;
            float s = 0;
            for (int k = 0; k < n; ++k) s += a[k] * b[k];
            C[size_t(i) * D + j] += s;
        }
}

void projectScalar(const int8_t* q, size_t n, int D, const float* W, int K, const float* bias, float* out) {
    for (size_t i = 0; i < n; ++i) {
        const int8_t* v = q + i * D;
        for (int k = 0; k < K; ++k) {
            const float* w = W + size_t(k) * D;
            float s = bias[k];
            for (int b = 0; b < D; ++b) s += float(v[b]) * w[b];
            out[i * K + k] = s;
        }
    }
}

void dotDequantScalar(const int8_t* A, const int8_t* B, size_t n, int D, const float* s, const float* o,
                      float* out) {
    for (size_t i = 0; i < n; ++i) {
        const int8_t* a = A + i * D;
        const int8_t* c = B + i * D;
        float acc = 0;
        for (int b = 0; b < D; ++b) acc += (float(a[b]) * s[b] + o[b]) * (float(c[b]) * s[b] + o[b]);
        out[i] = acc;
    }
}

#if EMB_X86
// ---------------------------------------------------------------------------
// x86-64: SSE2 (every x86-64 CPU) and AVX2 + FMA
// ---------------------------------------------------------------------------

inline float hsum128(__m128 v) {
    __m128 s = _mm_add_ps(v, _mm_movehl_ps(v, v));
    s = _mm_add_ss(s, _mm_shuffle_ps(s, s, 1));
    return _mm_cvtss_f32(s);
}

// 8 int8 -> 2 x 4 floats without SSE4.1 (sign extension by unpacking and shifting).
inline void i8x8ToF32Sse2(const int8_t* p, __m128& lo, __m128& hi) {
    const __m128i b = _mm_loadl_epi64(reinterpret_cast<const __m128i*>(p));
    const __m128i w = _mm_srai_epi16(_mm_unpacklo_epi8(b, b), 8);           // 8 x int16
    lo = _mm_cvtepi32_ps(_mm_srai_epi32(_mm_unpacklo_epi16(w, w), 16));
    hi = _mm_cvtepi32_ps(_mm_srai_epi32(_mm_unpackhi_epi16(w, w), 16));
}

// C += X X^T over tiles of 2 rows x 4 columns (8 accumulators of 4 floats).
void syrkSse2(const float* X, int D, int n, int ld, float* C) {
    for (int i = 0; i < D; i += 2)
        for (int j = i & ~3; j < D; j += 4) { // tiles that reach the upper triangle
            __m128 a[2][4];
            for (auto& r : a)
                for (auto& c : r) c = _mm_setzero_ps();
            const float* p0 = X + size_t(i) * ld;
            const float* p1 = p0 + ld;
            const float* q0 = X + size_t(j) * ld;
            const float* q1 = q0 + ld;
            const float* q2 = q1 + ld;
            const float* q3 = q2 + ld;
            for (int k = 0; k < n; k += 4) {
                const __m128 x0 = _mm_loadu_ps(p0 + k), x1 = _mm_loadu_ps(p1 + k);
                const __m128 y[4] = {_mm_loadu_ps(q0 + k), _mm_loadu_ps(q1 + k), _mm_loadu_ps(q2 + k),
                                     _mm_loadu_ps(q3 + k)};
                for (int c = 0; c < 4; ++c) {
                    a[0][c] = _mm_add_ps(a[0][c], _mm_mul_ps(x0, y[c]));
                    a[1][c] = _mm_add_ps(a[1][c], _mm_mul_ps(x1, y[c]));
                }
            }
            for (int r = 0; r < 2; ++r)
                for (int c = 0; c < 4; ++c) C[size_t(i + r) * D + j + c] += hsum128(a[r][c]);
        }
}

void projectSse2(const int8_t* q, size_t n, int D, const float* W, int K, const float* bias, float* out) {
    const int D8 = D & ~7;
    for (size_t i = 0; i < n; ++i) {
        const int8_t* v = q + i * D;
        __m128 acc[4] = {_mm_setzero_ps(), _mm_setzero_ps(), _mm_setzero_ps(), _mm_setzero_ps()};
        for (int b = 0; b < D8; b += 8) {
            __m128 lo, hi;
            i8x8ToF32Sse2(v + b, lo, hi);
            for (int k = 0; k < K; ++k) {
                const float* w = W + size_t(k) * D + b;
                acc[k] = _mm_add_ps(acc[k], _mm_add_ps(_mm_mul_ps(lo, _mm_loadu_ps(w)), _mm_mul_ps(hi, _mm_loadu_ps(w + 4))));
            }
        }
        for (int k = 0; k < K; ++k) {
            float s = bias[k] + hsum128(acc[k]);
            const float* w = W + size_t(k) * D;
            for (int b = D8; b < D; ++b) s += float(v[b]) * w[b];
            out[i * K + k] = s;
        }
    }
}

void dotDequantSse2(const int8_t* A, const int8_t* B, size_t n, int D, const float* s, const float* o, float* out) {
    const int D8 = D & ~7;
    for (size_t i = 0; i < n; ++i) {
        const int8_t* a = A + i * D;
        const int8_t* c = B + i * D;
        __m128 acc = _mm_setzero_ps();
        for (int b = 0; b < D8; b += 8) {
            __m128 alo, ahi, clo, chi;
            i8x8ToF32Sse2(a + b, alo, ahi);
            i8x8ToF32Sse2(c + b, clo, chi);
            const __m128 s0 = _mm_loadu_ps(s + b), s1 = _mm_loadu_ps(s + b + 4);
            const __m128 o0 = _mm_loadu_ps(o + b), o1 = _mm_loadu_ps(o + b + 4);
            const __m128 fa0 = _mm_add_ps(_mm_mul_ps(alo, s0), o0), fa1 = _mm_add_ps(_mm_mul_ps(ahi, s1), o1);
            const __m128 fc0 = _mm_add_ps(_mm_mul_ps(clo, s0), o0), fc1 = _mm_add_ps(_mm_mul_ps(chi, s1), o1);
            acc = _mm_add_ps(acc, _mm_add_ps(_mm_mul_ps(fa0, fc0), _mm_mul_ps(fa1, fc1)));
        }
        float r = hsum128(acc);
        for (int b = D8; b < D; ++b) r += (float(a[b]) * s[b] + o[b]) * (float(c[b]) * s[b] + o[b]);
        out[i] = r;
    }
}

EMB_AVX2 inline float hsum256(__m256 v) {
    __m128 s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
    s = _mm_add_ps(s, _mm_movehl_ps(s, s));
    s = _mm_add_ss(s, _mm_shuffle_ps(s, s, 1));
    return _mm_cvtss_f32(s);
}

EMB_AVX2 inline __m256 i8x8ToF32Avx2(const int8_t* p) {
    return _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(_mm_loadl_epi64(reinterpret_cast<const __m128i*>(p))));
}

// 2 x 4 tile: 8 accumulators + 6 loads in the 16 ymm registers; 6 loads feed 8 FMAs.
EMB_AVX2 void syrkAvx2(const float* X, int D, int n, int ld, float* C) {
    for (int i = 0; i < D; i += 2)
        for (int j = i & ~3; j < D; j += 4) {
            __m256 a00 = _mm256_setzero_ps(), a01 = a00, a02 = a00, a03 = a00;
            __m256 a10 = a00, a11 = a00, a12 = a00, a13 = a00;
            const float* p0 = X + size_t(i) * ld;
            const float* p1 = p0 + ld;
            const float* q0 = X + size_t(j) * ld;
            const float* q1 = q0 + ld;
            const float* q2 = q1 + ld;
            const float* q3 = q2 + ld;
            for (int k = 0; k < n; k += 8) {
                const __m256 x0 = _mm256_loadu_ps(p0 + k), x1 = _mm256_loadu_ps(p1 + k);
                const __m256 y0 = _mm256_loadu_ps(q0 + k), y1 = _mm256_loadu_ps(q1 + k);
                const __m256 y2 = _mm256_loadu_ps(q2 + k), y3 = _mm256_loadu_ps(q3 + k);
                a00 = _mm256_fmadd_ps(x0, y0, a00);
                a01 = _mm256_fmadd_ps(x0, y1, a01);
                a02 = _mm256_fmadd_ps(x0, y2, a02);
                a03 = _mm256_fmadd_ps(x0, y3, a03);
                a10 = _mm256_fmadd_ps(x1, y0, a10);
                a11 = _mm256_fmadd_ps(x1, y1, a11);
                a12 = _mm256_fmadd_ps(x1, y2, a12);
                a13 = _mm256_fmadd_ps(x1, y3, a13);
            }
            float* c0 = C + size_t(i) * D + j;
            float* c1 = c0 + D;
            c0[0] += hsum256(a00);
            c0[1] += hsum256(a01);
            c0[2] += hsum256(a02);
            c0[3] += hsum256(a03);
            c1[0] += hsum256(a10);
            c1[1] += hsum256(a11);
            c1[2] += hsum256(a12);
            c1[3] += hsum256(a13);
        }
}

EMB_AVX2 void projectAvx2(const int8_t* q, size_t n, int D, const float* W, int K, const float* bias, float* out) {
    const int D8 = D & ~7;
    for (size_t i = 0; i < n; ++i) {
        const int8_t* v = q + i * D;
        __m256 a0 = _mm256_setzero_ps(), a1 = a0, a2 = a0, a3 = a0;
        const float* w0 = W;
        const float* w1 = W + D;
        const float* w2 = W + 2 * size_t(D);
        const float* w3 = W + 3 * size_t(D);
        switch (K) { // the K weight rows stay in L1 across pixels
        case 1:
            for (int b = 0; b < D8; b += 8) a0 = _mm256_fmadd_ps(i8x8ToF32Avx2(v + b), _mm256_loadu_ps(w0 + b), a0);
            break;
        case 2:
            for (int b = 0; b < D8; b += 8) {
                const __m256 x = i8x8ToF32Avx2(v + b);
                a0 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w0 + b), a0);
                a1 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w1 + b), a1);
            }
            break;
        case 3:
            for (int b = 0; b < D8; b += 8) {
                const __m256 x = i8x8ToF32Avx2(v + b);
                a0 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w0 + b), a0);
                a1 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w1 + b), a1);
                a2 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w2 + b), a2);
            }
            break;
        default:
            for (int b = 0; b < D8; b += 8) {
                const __m256 x = i8x8ToF32Avx2(v + b);
                a0 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w0 + b), a0);
                a1 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w1 + b), a1);
                a2 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w2 + b), a2);
                a3 = _mm256_fmadd_ps(x, _mm256_loadu_ps(w3 + b), a3);
            }
        }
        const __m256 acc[4] = {a0, a1, a2, a3};
        for (int k = 0; k < K; ++k) {
            float s = bias[k] + hsum256(acc[k]);
            const float* w = W + size_t(k) * D;
            for (int b = D8; b < D; ++b) s += float(v[b]) * w[b];
            out[i * K + k] = s;
        }
    }
}

EMB_AVX2 void dotDequantAvx2(const int8_t* A, const int8_t* B, size_t n, int D, const float* s, const float* o,
                             float* out) {
    const int D8 = D & ~7;
    for (size_t i = 0; i < n; ++i) {
        const int8_t* a = A + i * D;
        const int8_t* c = B + i * D;
        __m256 acc = _mm256_setzero_ps();
        for (int b = 0; b < D8; b += 8) {
            const __m256 sb = _mm256_loadu_ps(s + b), ob = _mm256_loadu_ps(o + b);
            const __m256 fa = _mm256_fmadd_ps(i8x8ToF32Avx2(a + b), sb, ob);
            const __m256 fc = _mm256_fmadd_ps(i8x8ToF32Avx2(c + b), sb, ob);
            acc = _mm256_fmadd_ps(fa, fc, acc);
        }
        float r = hsum256(acc);
        for (int b = D8; b < D; ++b) r += (float(a[b]) * s[b] + o[b]) * (float(c[b]) * s[b] + o[b]);
        out[i] = r;
    }
}

bool cpuHasAvx2Fma() {
#ifdef _MSC_VER
    int r[4];
    __cpuid(r, 0);
    if (r[0] < 7) return false;
    __cpuid(r, 1);
    const bool fma = (r[2] >> 12) & 1, osxsave = (r[2] >> 27) & 1, avx = (r[2] >> 28) & 1;
    if (!(fma && osxsave && avx)) return false;
    if ((_xgetbv(0) & 6) != 6) return false; // the OS saves the ymm registers
    __cpuidex(r, 7, 0);
    return (r[1] >> 5) & 1;
#else
    __builtin_cpu_init();
    return __builtin_cpu_supports("avx2") && __builtin_cpu_supports("fma");
#endif
}
#endif // EMB_X86

#if EMB_NEON
// ---------------------------------------------------------------------------
// ARM64: NEON (every ARM64 CPU)
// ---------------------------------------------------------------------------

void syrkNeon(const float* X, int D, int n, int ld, float* C) {
    for (int i = 0; i < D; i += 2)
        for (int j = i & ~3; j < D; j += 4) {
            float32x4_t a[2][4];
            for (auto& r : a)
                for (auto& c : r) c = vdupq_n_f32(0);
            const float* p0 = X + size_t(i) * ld;
            const float* p1 = p0 + ld;
            const float* q[4] = {X + size_t(j) * ld, X + size_t(j + 1) * ld, X + size_t(j + 2) * ld,
                                 X + size_t(j + 3) * ld};
            for (int k = 0; k < n; k += 4) {
                const float32x4_t x0 = vld1q_f32(p0 + k), x1 = vld1q_f32(p1 + k);
                for (int c = 0; c < 4; ++c) {
                    const float32x4_t y = vld1q_f32(q[c] + k);
                    a[0][c] = vfmaq_f32(a[0][c], x0, y);
                    a[1][c] = vfmaq_f32(a[1][c], x1, y);
                }
            }
            for (int r = 0; r < 2; ++r)
                for (int c = 0; c < 4; ++c) C[size_t(i + r) * D + j + c] += vaddvq_f32(a[r][c]);
        }
}

inline void i8x8ToF32Neon(const int8_t* p, float32x4_t& lo, float32x4_t& hi) {
    const int16x8_t w = vmovl_s8(vld1_s8(p));
    lo = vcvtq_f32_s32(vmovl_s16(vget_low_s16(w)));
    hi = vcvtq_f32_s32(vmovl_s16(vget_high_s16(w)));
}

void projectNeon(const int8_t* q, size_t n, int D, const float* W, int K, const float* bias, float* out) {
    const int D8 = D & ~7;
    for (size_t i = 0; i < n; ++i) {
        const int8_t* v = q + i * D;
        float32x4_t acc[4] = {vdupq_n_f32(0), vdupq_n_f32(0), vdupq_n_f32(0), vdupq_n_f32(0)};
        for (int b = 0; b < D8; b += 8) {
            float32x4_t lo, hi;
            i8x8ToF32Neon(v + b, lo, hi);
            for (int k = 0; k < K; ++k) {
                const float* w = W + size_t(k) * D + b;
                acc[k] = vfmaq_f32(vfmaq_f32(acc[k], lo, vld1q_f32(w)), hi, vld1q_f32(w + 4));
            }
        }
        for (int k = 0; k < K; ++k) {
            float s = bias[k] + vaddvq_f32(acc[k]);
            const float* w = W + size_t(k) * D;
            for (int b = D8; b < D; ++b) s += float(v[b]) * w[b];
            out[i * K + k] = s;
        }
    }
}

void dotDequantNeon(const int8_t* A, const int8_t* B, size_t n, int D, const float* s, const float* o, float* out) {
    const int D8 = D & ~7;
    for (size_t i = 0; i < n; ++i) {
        const int8_t* a = A + i * D;
        const int8_t* c = B + i * D;
        float32x4_t acc = vdupq_n_f32(0);
        for (int b = 0; b < D8; b += 8) {
            float32x4_t alo, ahi, clo, chi;
            i8x8ToF32Neon(a + b, alo, ahi);
            i8x8ToF32Neon(c + b, clo, chi);
            const float32x4_t s0 = vld1q_f32(s + b), s1 = vld1q_f32(s + b + 4);
            const float32x4_t o0 = vld1q_f32(o + b), o1 = vld1q_f32(o + b + 4);
            acc = vfmaq_f32(acc, vfmaq_f32(o0, alo, s0), vfmaq_f32(o0, clo, s0));
            acc = vfmaq_f32(acc, vfmaq_f32(o1, ahi, s1), vfmaq_f32(o1, chi, s1));
        }
        float r = vaddvq_f32(acc);
        for (int b = D8; b < D; ++b) r += (float(a[b]) * s[b] + o[b]) * (float(c[b]) * s[b] + o[b]);
        out[i] = r;
    }
}
#endif // EMB_NEON

struct Kernels {
    const char* name;
    void (*syrk)(const float*, int, int, int, float*);
    void (*project)(const int8_t*, size_t, int, const float*, int, const float*, float*);
    void (*dotDequant)(const int8_t*, const int8_t*, size_t, int, const float*, const float*, float*);
};

Kernels pick() {
#if EMB_X86
    if (cpuHasAvx2Fma()) return {"AVX2+FMA", syrkAvx2, projectAvx2, dotDequantAvx2};
    return {"SSE2", syrkSse2, projectSse2, dotDequantSse2};
#elif EMB_NEON
    return {"NEON", syrkNeon, projectNeon, dotDequantNeon};
#else
    return {"scalar", syrkScalar, projectScalar, dotDequantScalar};
#endif
}

const Kernels& kernels() {
    static const Kernels k = pick();
    return k;
}

bool forcedScalar = false;

} // namespace

const char* name() { return forcedScalar ? "scalar" : kernels().name; }
void forceScalar(bool on) { forcedScalar = on; }

void syrk(const float* X, int D, int n, int ld, float* C) {
    (forcedScalar ? syrkScalar : kernels().syrk)(X, D, n, ld, C);
}

void project(const int8_t* q, size_t n, int D, const float* W, int K, const float* bias, float* out) {
    (forcedScalar ? projectScalar : kernels().project)(q, n, D, W, K, bias, out);
}

void dotDequant(const int8_t* A, const int8_t* B, size_t n, int D, const float* s, const float* o, float* out) {
    (forcedScalar ? dotDequantScalar : kernels().dotDequant)(A, B, n, D, s, o, out);
}

} // namespace embsimd
