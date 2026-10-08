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
    largestDrop(vAll, s.dropMag, s.argDrop);
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

void largestDrop(const std::vector<float>& v, double& mag, int& at) {
    mag = 0;
    at = -1;
    int n = 0;
    float last = 0;
    for (size_t i = 0; i < v.size(); ++i) {
        if (std::isnan(v[i])) continue;
        if (n++ > 0 && double(last) - v[i] > mag) {
            mag = double(last) - v[i];
            at = int(i);
        }
        last = v[i];
    }
    if (n < 2) mag = NAN;
}

SampleStats computeSampleStats(std::vector<float>& v) {
    SampleStats s;
    v.erase(std::remove_if(v.begin(), v.end(), [](float f) { return std::isnan(f); }), v.end());
    s.n = int(v.size());
    if (v.empty()) {
        s.mean = s.std = s.p10 = s.p25 = s.p50 = s.p75 = s.p90 = NAN;
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
    s.p25 = q(0.25);
    s.p50 = q(0.50);
    s.p75 = q(0.75);
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

namespace {
// Days since 1970-01-01 of a civil date (proleptic Gregorian; H. Hinnant's algorithm).
long long daysFromCivil(long long y, unsigned m, unsigned d) {
    y -= m <= 2;
    const long long era = (y >= 0 ? y : y - 399) / 400;
    const unsigned yoe = unsigned(y - era * 400);
    const unsigned doy = (153 * (m > 2 ? m - 3 : m + 9) + 2) / 5 + d - 1;
    const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + (long long)doe - 719468;
}
} // namespace

void yearAndDay(double unixSeconds, int& year, int& dayOfYear) {
    const long long days = (long long)std::floor(unixSeconds / 86400.0);
    // Civil year of `days` (H. Hinnant's civil_from_days), then the day since its 1 January.
    const long long z = days + 719468;
    const long long era = (z >= 0 ? z : z - 146096) / 146097;
    const unsigned doe = unsigned(z - era * 146097);
    const unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const unsigned doyMarch = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const unsigned mp = (5 * doyMarch + 2) / 153;
    const unsigned m = mp < 10 ? mp + 3 : mp - 9;
    const long long y = (long long)yoe + era * 400 + (m <= 2);
    year = int(y);
    dayOfYear = int(days - daysFromCivil(y, 1, 1)) + 1;
}

int observationsPerYear(const std::vector<double>& t) {
    std::vector<double> gaps;
    for (size_t i = 1; i < t.size(); ++i)
        if (t[i] > t[i - 1]) gaps.push_back(t[i] - t[i - 1]);
    if (gaps.empty()) return 1;
    std::nth_element(gaps.begin(), gaps.begin() + gaps.size() / 2, gaps.end());
    const double days = gaps[gaps.size() / 2] / 86400.0;
    return days > 0 ? std::max(1, int(std::lround(365.25 / days))) : 1;
}

SeasonalGrid seasonalGrid(const std::vector<double>& t, const std::vector<float>& v, int bins) {
    SeasonalGrid g;
    g.bins = std::max(1, bins);
    int y0 = 0, y1 = -1;
    for (size_t i = 0; i < t.size() && i < v.size(); ++i) {
        int y, d;
        yearAndDay(t[i], y, d);
        if (y1 < y0) y0 = y1 = y;
        y0 = std::min(y0, y);
        y1 = std::max(y1, y);
    }
    if (y1 < y0) return g;
    g.year0 = y0;
    g.years = y1 - y0 + 1;
    std::vector<double> sum(size_t(g.years) * g.bins, 0.0);
    g.n.assign(sum.size(), 0);
    for (size_t i = 0; i < t.size() && i < v.size(); ++i) {
        if (!std::isfinite(v[i])) continue;
        int y, d;
        yearAndDay(t[i], y, d);
        const size_t k = size_t(y - y0) * g.bins + std::min(g.bins - 1, (d - 1) * g.bins / 366);
        sum[k] += v[i];
        ++g.n[k];
    }
    g.mean.resize(sum.size());
    for (size_t k = 0; k < sum.size(); ++k) g.mean[k] = g.n[k] ? float(sum[k] / g.n[k]) : NAN;
    return g;
}
