#include "reproject.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>

#include <cpl_error.h>
#include <ogr_spatialref.h>

namespace {

// Inverse of a geotransform (CRS -> pixel); false if it is singular.
bool invert(const double g[6], double i[6]) {
    const double det = g[1] * g[5] - g[2] * g[4];
    if (det == 0 || !std::isfinite(det)) return false;
    i[1] = g[5] / det;
    i[2] = -g[2] / det;
    i[4] = -g[4] / det;
    i[5] = g[1] / det;
    i[0] = -(i[1] * g[0] + i[2] * g[3]);
    i[3] = -(i[4] * g[0] + i[5] * g[3]);
    return true;
}

void apply(const double g[6], double& x, double& y) {
    const double gx = g[0] + x * g[1] + y * g[2], gy = g[3] + x * g[4] + y * g[5];
    x = gx;
    y = gy;
}

// Points outside a projection's area of use fail one by one: expected, not logged.
struct Quiet {
    Quiet() { CPLPushErrorHandler(CPLQuietErrorHandler); }
    ~Quiet() { CPLPopErrorHandler(); }
};

// `perEdge` points per edge around the rectangle, clockwise from (x0, y0).
void edgePoints(double x0, double y0, double x1, double y1, int perEdge, std::vector<double>& xs,
                std::vector<double>& ys) {
    const double cx[5] = {x0, x1, x1, x0, x0}, cy[5] = {y0, y0, y1, y1, y0};
    xs.clear();
    ys.clear();
    for (int e = 0; e < 4; ++e)
        for (int i = 0; i < perEdge; ++i) {
            const double f = double(i) / perEdge;
            xs.push_back(cx[e] + f * (cx[e + 1] - cx[e]));
            ys.push_back(cy[e] + f * (cy[e + 1] - cy[e]));
        }
}

} // namespace

std::unique_ptr<Reprojection> Reprojection::create(const CubeInfo& a, const CubeInfo& b, std::string& why,
                                                 double margin) {
    if (!a.hasGeoTransform || !b.hasGeoTransform) {
        why = "no georeferencing";
        return nullptr;
    }
    std::unique_ptr<Reprojection> r(new Reprojection);
    r->activeId = a.id;
    r->layerId = b.id;
    r->crsText = b.crsAuthority.empty() ? b.crsName : b.crsAuthority;
    std::copy(a.geoTransform.begin(), a.geoTransform.end(), r->ga_);
    std::copy(b.geoTransform.begin(), b.geoTransform.end(), r->gb_);
    if (!invert(r->ga_, r->ia_) || !invert(r->gb_, r->ib_)) {
        why = "invalid geotransform";
        return nullptr;
    }
    r->lw_ = b.width;
    r->lh_ = b.height;
    if (a.crsWkt.empty() && b.crsWkt.empty()) {
        r->sameCrs = true; // neither has a CRS: their coordinates are taken as the same
    } else if (a.crsWkt.empty() || b.crsWkt.empty()) {
        why = "unknown CRS";
        return nullptr;
    } else {
        OGRSpatialReference sa, sb;
        if (sa.importFromWkt(a.crsWkt.c_str()) != OGRERR_NONE || sb.importFromWkt(b.crsWkt.c_str()) != OGRERR_NONE) {
            why = "CRS not understood";
            return nullptr;
        }
        sa.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER); // x = easting / longitude
        sb.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
        if (sa.IsSame(&sb)) {
            r->sameCrs = true;
        } else {
            Quiet q;
            r->fwd_ = OGRCreateCoordinateTransformation(&sa, &sb);
            r->inv_ = OGRCreateCoordinateTransformation(&sb, &sa);
            if (!r->fwd_ || !r->inv_) {
                why = "no transformation between the CRSs";
                return nullptr;
            }
        }
    }
    r->buildGrid(a, margin);
    return r;
}

Reprojection::~Reprojection() {
    if (fwd_) OGRCoordinateTransformation::DestroyCT(fwd_);
    if (inv_) OGRCoordinateTransformation::DestroyCT(inv_);
    Gpu::deleteTexture(tex);
}

void Reprojection::toLayer(size_t n, double* x, double* y, int* ok) const {
    for (size_t i = 0; i < n; ++i) {
        apply(ga_, x[i], y[i]);
        ok[i] = 1;
    }
    if (fwd_ && n) {
        Quiet q;
        fwd_->Transform(n, x, y, nullptr, ok);
    }
    for (size_t i = 0; i < n; ++i) {
        if (ok[i] && std::isfinite(x[i]) && std::isfinite(y[i])) apply(ib_, x[i], y[i]);
        else ok[i] = 0;
    }
}

void Reprojection::toActive(size_t n, double* x, double* y, int* ok) const {
    for (size_t i = 0; i < n; ++i) {
        apply(gb_, x[i], y[i]);
        ok[i] = 1;
    }
    if (inv_ && n) {
        Quiet q;
        inv_->Transform(n, x, y, nullptr, ok);
    }
    for (size_t i = 0; i < n; ++i) {
        if (ok[i] && std::isfinite(x[i]) && std::isfinite(y[i])) apply(ia_, x[i], y[i]);
        else ok[i] = 0;
    }
}

bool Reprojection::toLayer(double x, double y, double& lx, double& ly) const {
    int ok = 0;
    toLayer(1, &x, &y, &ok);
    lx = x;
    ly = y;
    return ok != 0;
}

bool Reprojection::toActive(double lx, double ly, double& x, double& y) const {
    int ok = 0;
    toActive(1, &lx, &ly, &ok);
    x = lx;
    y = ly;
    return ok != 0;
}

bool Reprojection::footprint(double x0, double y0, double x1, double y1, double box[4]) const {
    std::vector<double> xs, ys;
    edgePoints(x0, y0, x1, y1, 16, xs, ys);
    std::vector<int> ok(xs.size());
    toActive(xs.size(), xs.data(), ys.data(), ok.data());
    box[0] = box[1] = HUGE_VAL;
    box[2] = box[3] = -HUGE_VAL;
    for (size_t i = 0; i < xs.size(); ++i) {
        if (!ok[i]) continue;
        box[0] = std::min(box[0], xs[i]);
        box[1] = std::min(box[1], ys[i]);
        box[2] = std::max(box[2], xs[i]);
        box[3] = std::max(box[3], ys[i]);
    }
    return box[2] >= box[0];
}

std::vector<double> Reprojection::polygon(double x0, double y0, double x1, double y1, int perEdge) const {
    std::vector<double> xs, ys;
    edgePoints(x0, y0, x1, y1, perEdge, xs, ys);
    std::vector<int> ok(xs.size());
    toLayer(xs.size(), xs.data(), ys.data(), ok.data());
    std::vector<double> out;
    for (size_t i = 0; i < xs.size(); ++i)
        if (ok[i]) {
            out.push_back(xs[i]);
            out.push_back(ys[i]);
        }
    return out;
}

double Reprojection::layerPxPerMapPx(double x, double y) const {
    double px[3] = {x, x + 1, x}, py[3] = {y, y, y + 1};
    int ok[3];
    toLayer(3, px, py, ok);
    if (!ok[0] || !ok[1] || !ok[2]) return 0;
    return std::sqrt(std::fabs((px[1] - px[0]) * (py[2] - py[0]) - (px[2] - px[0]) * (py[1] - py[0])));
}

bool Reprojection::gridAt(double x, double y, double& lx, double& ly) const {
    if (!hasDomain()) return false;
    const double gx = (x - domain[0]) / (domain[2] - domain[0]) * (gridW - 1);
    const double gy = (y - domain[1]) / (domain[3] - domain[1]) * (gridH - 1);
    const int i = std::clamp(int(std::floor(gx)), 0, gridW - 2), j = std::clamp(int(std::floor(gy)), 0, gridH - 2);
    const double fx = gx - i, fy = gy - j;
    auto at = [&](int ii, int jj, int c) { return double(grid[(size_t(jj) * gridW + ii) * 2 + c]); };
    auto lerp = [&](int c) {
        return (1 - fy) * ((1 - fx) * at(i, j, c) + fx * at(i + 1, j, c)) +
               fy * ((1 - fx) * at(i, j + 1, c) + fx * at(i + 1, j + 1, c));
    };
    lx = lerp(0) * lw_;
    ly = lerp(1) * lh_;
    return std::isfinite(lx) && std::isfinite(ly);
}

// The grid covers the part of the layer the map can show: the active layer
// and, around it, what a fitted view of an elongated image leaves free
// (`margin` x its longer side: 0.6 for layers). 64 cells along the longer side; twice as many while the
// bilinear interpolation, checked at every cell's centre against the exact
// transformation, is off by more than 0.05 of the layer's pixels (up to 512).
void Reprojection::buildGrid(const CubeInfo& a, double margin) {
    const auto t0 = std::chrono::steady_clock::now();
    const double m = margin * std::max(a.width, a.height);
    const double around[4] = {-m, -m, a.width + m, a.height + m};
    // The layer's edges on the map, plus the points of a 17 x 17 lattice over
    // that area which fall inside the layer (its edges may not transform at all,
    // e.g. a continent in geographic coordinates over a UTM scene).
    double box[4];
    if (!footprint(0, 0, lw_, lh_, box)) {
        box[0] = box[1] = HUGE_VAL;
        box[2] = box[3] = -HUGE_VAL;
    }
    const int lattice = 17;
    std::vector<double> xs, ys, mx, my;
    for (int j = 0; j < lattice; ++j)
        for (int i = 0; i < lattice; ++i) {
            mx.push_back(around[0] + (around[2] - around[0]) * i / (lattice - 1));
            my.push_back(around[1] + (around[3] - around[1]) * j / (lattice - 1));
        }
    xs = mx;
    ys = my;
    std::vector<int> ok(xs.size());
    toLayer(xs.size(), xs.data(), ys.data(), ok.data());
    const double cw = (around[2] - around[0]) / (lattice - 1), ch = (around[3] - around[1]) / (lattice - 1);
    for (size_t k = 0; k < xs.size(); ++k)
        if (ok[k] && xs[k] >= 0 && ys[k] >= 0 && xs[k] <= lw_ && ys[k] <= lh_ &&
            !(mx[k] >= box[0] && mx[k] <= box[2] && my[k] >= box[1] && my[k] <= box[3])) {
            // On the layer, but missed by its edges: with the lattice's cell around it.
            box[0] = std::min(box[0], mx[k] - cw);
            box[1] = std::min(box[1], my[k] - ch);
            box[2] = std::max(box[2], mx[k] + cw);
            box[3] = std::max(box[3], my[k] + ch);
        }
    domain[0] = std::max(box[0], around[0]);
    domain[1] = std::max(box[1], around[1]);
    domain[2] = std::min(box[2], around[2]);
    domain[3] = std::min(box[3], around[3]);
    gridW = gridH = 0;
    grid.clear();
    if (!(domain[2] > domain[0] && domain[3] > domain[1])) return;
    const double dw = domain[2] - domain[0], dh = domain[3] - domain[1];
    for (int cells = 64;; cells *= 2) {
        const double cell = std::max(dw, dh) / cells;
        gridW = std::max(2, int(std::ceil(dw / cell - 1e-9)) + 1);
        gridH = std::max(2, int(std::ceil(dh / cell - 1e-9)) + 1);
        const size_t n = size_t(gridW) * gridH;
        xs.resize(n);
        ys.resize(n);
        ok.resize(n);
        for (int j = 0; j < gridH; ++j)
            for (int i = 0; i < gridW; ++i) {
                xs[size_t(j) * gridW + i] = domain[0] + dw * i / (gridW - 1);
                ys[size_t(j) * gridW + i] = domain[1] + dh * j / (gridH - 1);
            }
        toLayer(n, xs.data(), ys.data(), ok.data());
        grid.resize(n * 2);
        for (size_t k = 0; k < n; ++k) {
            grid[k * 2] = ok[k] ? float(xs[k] / lw_) : NAN;
            grid[k * 2 + 1] = ok[k] ? float(ys[k] / lh_) : NAN;
        }
        // Error at the cells' centres.
        const size_t nc = size_t(gridW - 1) * (gridH - 1);
        xs.resize(nc);
        ys.resize(nc);
        ok.resize(nc);
        for (int j = 0; j + 1 < gridH; ++j)
            for (int i = 0; i + 1 < gridW; ++i) {
                xs[size_t(j) * (gridW - 1) + i] = domain[0] + dw * (i + 0.5) / (gridW - 1);
                ys[size_t(j) * (gridW - 1) + i] = domain[1] + dh * (j + 0.5) / (gridH - 1);
            }
        std::vector<double> cx = xs, cy = ys;
        toLayer(nc, xs.data(), ys.data(), ok.data());
        gridError = 0;
        for (size_t k = 0; k < nc; ++k) {
            double gx, gy;
            if (ok[k] && gridAt(cx[k], cy[k], gx, gy))
                gridError = std::max(gridError, std::hypot(gx - xs[k], gy - ys[k]));
        }
        if (gridError <= 0.05 || cells >= 512) break;
    }
    buildMs = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
}
