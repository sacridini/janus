#include "results.hpp"

#include <algorithm>
#include <cmath>

#include <cpl_error.h>
#include <gdal_priv.h>

#include "stats.hpp"

float ResultLayer::valueAt(int cubeX, int cubeY) const {
    if (cubeX < x0 || cubeY < y0 || cubeX >= x0 + w || cubeY >= y0 + h || data.empty()) return NAN;
    const int px = std::min(tw - 1, int(double(cubeX - x0) * tw / w));
    const int py = std::min(th - 1, int(double(cubeY - y0) * th / h));
    return data[size_t(py) * tw + px];
}

bool loadResultRaster(const std::string& path, int maxDim, std::vector<float>& data, int& tw, int& th,
                      std::string& error) {
    CPLErrorReset();
    GDALDataset* ds = GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
    if (!ds) {
        error = path + ": " + CPLGetLastErrorMsg();
        return false;
    }
    const int W = ds->GetRasterXSize(), H = ds->GetRasterYSize();
    const double s = std::min(1.0, double(maxDim) / std::max(W, H));
    tw = std::max(1, int(std::lround(W * s)));
    th = std::max(1, int(std::lround(H * s)));
    data.assign(size_t(tw) * th, NAN);
    GDALRasterBand* b = ds->GetRasterBand(1);
    int hasNd = 0;
    const double nd = b->GetNoDataValue(&hasNd);
    GDALRasterIOExtraArg extra;
    INIT_RASTERIO_EXTRA_ARG(extra);
    extra.eResampleAlg = GRIORA_NearestNeighbour;
    const bool ok = b->RasterIO(GF_Read, 0, 0, W, H, data.data(), tw, th, GDT_Float32, 0, 0, &extra) == CE_None;
    GDALClose(ds);
    if (!ok) {
        error = path + ": read failed";
        return false;
    }
    if (hasNd && !std::isnan(nd))
        for (float& v : data)
            if (v == float(nd)) v = NAN;
    return true;
}

void autoResultRange(ResultLayer& r) {
    std::vector<float> v;
    v.reserve(r.data.size());
    for (float f : r.data)
        if (std::isfinite(f)) v.push_back(f);
    if (v.empty()) {
        r.lo = 0;
        r.hi = 1;
        return;
    }
    if (r.unit == "year" || r.unit == "years") {
        auto [mn, mx] = std::minmax_element(v.begin(), v.end());
        r.lo = *mn;
        r.hi = std::max(*mx, *mn + 1.0f);
    } else {
        samplePercentiles(v.data(), v.size(), 2000000, 0.02, 0.98, r.lo, r.hi);
    }
}
