#pragma once

#include <vector>

// Statistics of a time series (NaN values are ignored).
// `x` = time in years (or index), so slopes come out "per year".
struct SeriesStats {
    int n = 0;
    double mean = 0, median = 0, std = 0, cv = 0;
    double min = 0, max = 0;
    int argMin = -1, argMax = -1;
    double olsSlope = 0, olsIntercept = 0, r2 = 0;
    double senSlope = 0, senIntercept = 0;
};

SeriesStats computeSeriesStats(const std::vector<double>& x, const std::vector<float>& v);

// Statistics of a set of values (an ROI at one date).
struct SampleStats {
    int n = 0;
    float mean = 0, std = 0, p10 = 0, p50 = 0, p90 = 0;
};
SampleStats computeSampleStats(std::vector<float>& values); // reorders `values`

// Percentiles of a subsampled sample (for the automatic stretch range).
void samplePercentiles(const float* data, size_t n, size_t maxSamples, double qLo, double qHi,
                       float& lo, float& hi);
