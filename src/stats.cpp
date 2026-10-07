#include "stats.hpp"

#include <algorithm>
#include <cmath>

SeriesStats computeSeriesStats(const std::vector<double>& xAll, const std::vector<float>& vAll) {
    SeriesStats s;
    std::vector<double> x, v;
    std::vector<int> idx;
    for (size_t i = 0; i < vAll.size() && i < xAll.size(); ++i) {
        if (std::isnan(vAll[i])) continue;
        x.push_back(xAll[i]);
        v.push_back(vAll[i]);
        idx.push_back(int(i));
    }
    const int n = int(v.size());
    s.n = n;
    if (n == 0) return s;

    double sum = 0, sx = 0;
    s.min = s.max = v[0];
    s.argMin = s.argMax = idx[0];
    for (int i = 0; i < n; ++i) {
        sum += v[i];
        sx += x[i];
        if (v[i] < s.min) { s.min = v[i]; s.argMin = idx[i]; }
        if (v[i] > s.max) { s.max = v[i]; s.argMax = idx[i]; }
    }
    s.mean = sum / n;
    const double mx = sx / n;

    std::vector<double> sorted = v;
    std::nth_element(sorted.begin(), sorted.begin() + n / 2, sorted.end());
    s.median = sorted[n / 2];
    if (n % 2 == 0) s.median = 0.5 * (s.median + *std::max_element(sorted.begin(), sorted.begin() + n / 2));

    double sxx = 0, sxy = 0, syy = 0;
    for (int i = 0; i < n; ++i) {
        const double dx = x[i] - mx, dy = v[i] - s.mean;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    s.std = n > 1 ? std::sqrt(syy / (n - 1)) : 0.0;
    s.cv = s.mean != 0 ? s.std / std::fabs(s.mean) : 0.0;
    if (sxx > 0) {
        s.olsSlope = sxy / sxx;
        s.olsIntercept = s.mean - s.olsSlope * mx;
        s.r2 = syy > 0 ? (sxy * sxy) / (sxx * syy) : 0.0;
    }

    // Sen's slope: O(n^2), trivial for series up to a few thousand. (The
    // Mann-Kendall test, with autocorrelation corrections, is a Zeit tool.)
    if (n >= 3) {
        std::vector<double> slopes;
        slopes.reserve(size_t(n) * (n - 1) / 2);
        for (int i = 0; i < n - 1; ++i)
            for (int j = i + 1; j < n; ++j)
                if (x[j] != x[i]) slopes.push_back((v[j] - v[i]) / (x[j] - x[i]));
        if (!slopes.empty()) {
            const size_t m = slopes.size() / 2;
            std::nth_element(slopes.begin(), slopes.begin() + m, slopes.end());
            s.senSlope = slopes[m];
            if (slopes.size() % 2 == 0)
                s.senSlope = 0.5 * (s.senSlope + *std::max_element(slopes.begin(), slopes.begin() + m));
            // Intercept = median of (v - slope * x), keeping Sen's robustness.
            std::vector<double> r(n);
            for (int i = 0; i < n; ++i) r[i] = v[i] - s.senSlope * x[i];
            std::nth_element(r.begin(), r.begin() + n / 2, r.end());
            s.senIntercept = r[n / 2];
        }
    }
    return s;
}

SampleStats computeSampleStats(std::vector<float>& v) {
    SampleStats s;
    v.erase(std::remove_if(v.begin(), v.end(), [](float f) { return std::isnan(f); }), v.end());
    s.n = int(v.size());
    if (v.empty()) {
        s.mean = s.std = s.p10 = s.p50 = s.p90 = NAN;
        return s;
    }
    double sum = 0, sq = 0;
    for (float f : v) sum += f;
    s.mean = float(sum / v.size());
    for (float f : v) sq += (f - s.mean) * double(f - s.mean);
    s.std = v.size() > 1 ? float(std::sqrt(sq / (v.size() - 1))) : 0.f;
    auto q = [&](double p) {
        size_t k = size_t(p * (v.size() - 1));
        std::nth_element(v.begin(), v.begin() + k, v.end());
        return v[k];
    };
    s.p10 = q(0.10);
    s.p50 = q(0.50);
    s.p90 = q(0.90);
    return s;
}

void samplePercentiles(const float* data, size_t n, size_t maxSamples, double qLo, double qHi,
                       float& lo, float& hi) {
    std::vector<float> s;
    const size_t step = std::max<size_t>(1, n / std::max<size_t>(1, maxSamples));
    s.reserve(n / step + 1);
    for (size_t i = 0; i < n; i += step)
        if (!std::isnan(data[i])) s.push_back(data[i]);
    if (s.empty()) {
        lo = 0;
        hi = 1;
        return;
    }
    auto q = [&](double p) {
        size_t k = size_t(p * (s.size() - 1));
        std::nth_element(s.begin(), s.begin() + k, s.end());
        return s[k];
    };
    lo = q(qLo);
    hi = q(qHi);
    if (!(hi > lo)) hi = lo + 1e-6f;
}
