// Layers: several series open at once, the Layers and Files panels, and the
// series of the non-active layers under the cursor and the pins.
//
// Map space is the active layer's pixel grid. Other layers are placed through
// their geotransforms: straight (an affine map) with the same CRS and no
// rotation, else reprojected (reproject.hpp: OGR/PROJ on the CPU, a warp grid
// in the shader). The active layer's display state lives in the App members
// (mode_, cmap_, range_...) and is swapped in/out of LayerDisplay when the
// active layer changes.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <filesystem>

#include <implot.h>

#include "glfw.hpp"
#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

bool sameCrs(const CubeInfo& a, const CubeInfo& b) {
    if (!a.crsAuthority.empty() || !b.crsAuthority.empty()) return a.crsAuthority == b.crsAuthority;
    return a.crsName == b.crsName;
}

int nearestTime(const std::vector<Layer>& layers, double time) {
    int best = 0;
    for (int i = 1; i < int(layers.size()); ++i)
        if (std::fabs(layers[i].time - time) < std::fabs(layers[best].time - time)) best = i;
    return best;
}

} // namespace

const App::SeriesLayer* App::activeLayer() const {
    return active_ >= 0 && active_ < int(layers_.size()) ? &layers_[active_] : nullptr;
}

std::string App::layerName(const CubeInfo& info, const std::vector<std::string>& inputs) const {
    fs::path p = fs::u8path(inputs.size() == 1 ? inputs[0] : info.firstPath);
    if (inputs.size() != 1) p = p.parent_path();
    std::string n = p.has_stem() ? p.stem().u8string() : p.filename().u8string();
    if (n.empty()) n = p.u8string();
    // Disambiguate equal names (e.g. two folders called "ndvi").
    int dup = 0;
    for (const SeriesLayer& L : layers_)
        if (L.name == n || L.name.rfind(n + " (", 0) == 0) ++dup;
    return dup ? n + " (" + std::to_string(dup + 1) + ")" : n;
}

void App::saveDisplay(LayerDisplay& d) const {
    d.mode = mode_;
    d.rgb = rgb_;
    d.diffRef = diffRef_;
    d.cmap = cmap_;
    d.range = range_;
    d.perDateRange = perDateRange_;
    d.t = t_;
}

void App::loadDisplay(const LayerDisplay& d) {
    mode_ = d.mode;
    rgb_ = d.rgb;
    diffRef_ = d.diffRef;
    cmap_ = d.cmap;
    range_ = d.range;
    perDateRange_ = d.perDateRange;
    t_ = d.t;
    histKey_ = ~0ull;
    appliedCmap_ = -1;
}

// Pixel of layer L -> pixel of the active layer.
bool App::toActive(const SeriesLayer& L, double lx, double ly, double& x, double& y) const {
    if (!L.aligned) return false;
    if (L.reproj) return L.reproj->toActive(lx, ly, x, y);
    x = L.ax + L.bx * lx;
    y = L.ay + L.by * ly;
    return true;
}

bool App::activeToLayer(const SeriesLayer& L, double x, double y, double& lx, double& ly) const {
    if (!L.aligned) return false;
    if (L.reproj) return L.reproj->toLayer(x, y, lx, ly);
    if (L.bx == 0 || L.by == 0) return false;
    lx = (x - L.ax) / L.bx;
    ly = (y - L.ay) / L.by;
    return true;
}

// Pixel of the active layer -> pixel of layer L (false outside it).
bool App::fromActive(const SeriesLayer& L, double x, double y, int& lx, int& ly) const {
    double fx, fy;
    const CubeInfo& info = *L.session->info;
    if (!activeToLayer(L, x, y, fx, fy) || !(fx >= 0 && fy >= 0 && fx < info.width && fy < info.height)) return false;
    lx = int(fx);
    ly = int(fy);
    return true;
}

bool App::layerQuad(const SeriesLayer& L, double x0, double y0, double x1, double y1, double q[4],
                    WarpParams& w) const {
    w = WarpParams{};
    if (!L.aligned) return false;
    if (!L.reproj) {
        toActive(L, x0, y0, q[0], q[1]);
        toActive(L, x1, y1, q[2], q[3]);
        return true;
    }
    const Reprojection& R = *L.reproj;
    if (!R.hasDomain() || !R.tex) return false;
    const CubeInfo& info = *L.session->info;
    std::copy(R.domain, R.domain + 4, q);
    if (x0 > 0 || y0 > 0 || x1 < info.width || y1 < info.height) {
        // Part of the layer (a tile, a result): the bounding box of its edges,
        // plus a margin for their curvature between the points; the shader
        // discards what falls outside it.
        double b[4];
        if (!R.footprint(x0, y0, x1, y1, b)) return false;
        const double mx = (b[2] - b[0]) * 0.02 + 1, my = (b[3] - b[1]) * 0.02 + 1;
        q[0] = std::max(b[0] - mx, R.domain[0]);
        q[1] = std::max(b[1] - my, R.domain[1]);
        q[2] = std::min(b[2] + mx, R.domain[2]);
        q[3] = std::min(b[3] + my, R.domain[3]);
        if (q[2] <= q[0] || q[3] <= q[1]) return false;
    }
    const double dw = R.domain[2] - R.domain[0], dh = R.domain[3] - R.domain[1];
    w.grid = R.tex;
    w.quad[0] = float((q[0] - R.domain[0]) / dw);
    w.quad[1] = float((q[1] - R.domain[1]) / dh);
    w.quad[2] = float((q[2] - q[0]) / dw);
    w.quad[3] = float((q[3] - q[1]) / dh);
    w.src[0] = float(x0 / info.width);
    w.src[1] = float(y0 / info.height);
    w.src[2] = float((x1 - x0) / info.width);
    w.src[3] = float((y1 - y0) / info.height);
    return true;
}

ViewRect App::layerView(const SeriesLayer& L, double x0, double y0, double x1, double y1, double pxPerMapPx) const {
    if (!L.reproj)
        return {(x0 - L.ax) / L.bx, (y0 - L.ay) / L.by, (x1 - L.ax) / L.bx, (y1 - L.ay) / L.by, pxPerMapPx * L.bx};
    // Reprojected: the bounding box of the view's edges in the layer's pixels.
    const std::vector<double> p = L.reproj->polygon(x0, y0, x1, y1, 8);
    const double k = L.reproj->layerPxPerMapPx((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    if (p.empty() || !(k > 0)) return {0, 0, 0, 0, 0}; // nothing to read
    ViewRect v{HUGE_VAL, HUGE_VAL, -HUGE_VAL, -HUGE_VAL, pxPerMapPx / k};
    for (size_t i = 0; i + 1 < p.size(); i += 2) {
        v.x0 = std::min(v.x0, p[i]);
        v.y0 = std::min(v.y0, p[i + 1]);
        v.x1 = std::max(v.x1, p[i]);
        v.y1 = std::max(v.y1, p[i + 1]);
    }
    return v;
}

void App::updateAlignment() {
    const SeriesLayer* A = activeLayer();
    if (!A) return;
    const CubeInfo& a = *A->session->info;
    for (SeriesLayer& L : layers_) {
        const CubeInfo& b = *L.session->info;
        L.alignNote.clear();
        L.ax = L.ay = 0;
        L.bx = L.by = 1;
        if (&L == A) {
            L.reproj.reset();
            L.aligned = true;
            continue;
        }
        if (!a.hasGeoTransform || !b.hasGeoTransform) {
            // Without georeferencing, only identical grids can be overlaid.
            L.reproj.reset();
            L.aligned = a.width == b.width && a.height == b.height;
            if (!L.aligned) L.alignNote = "no georeferencing and a different size: shown only when active";
            continue;
        }
        const auto& ga = a.geoTransform;
        const auto& gb = b.geoTransform;
        const bool rotated = ga[2] != 0 || ga[4] != 0 || gb[2] != 0 || gb[4] != 0;
        if (!rotated && sameCrs(a, b)) { // the fast path: an affine map
            L.reproj.reset();
            L.aligned = true;
            L.bx = gb[1] / ga[1];
            L.by = gb[5] / ga[5];
            L.ax = (gb[0] - ga[0]) / ga[1];
            L.ay = (gb[3] - ga[3]) / ga[5];
            continue;
        }
        // Another CRS or a rotated grid: reprojected (kept while both layers are the same).
        if (!L.reproj || L.reproj->activeId != a.id || L.reproj->layerId != b.id) {
            std::string why;
            L.reproj = Reprojection::create(a, b, why);
            if (L.reproj && L.reproj->hasDomain())
                L.reproj->tex = Gpu::createWarpGrid(L.reproj->gridW, L.reproj->gridH, L.reproj->grid.data());
            if (!L.reproj) {
                L.aligned = false;
                const std::string crs = b.crsAuthority.empty() ? b.crsName : b.crsAuthority;
                L.alignNote = (rotated ? "rotated grid" : "different CRS (" + crs + ")") + (", " + why) +
                              ": shown only when active";
                continue;
            }
        }
        L.aligned = true;
    }
}

// Every non-active layer shows the date nearest to the active layer's date.
void App::syncLayerTimes() {
    const SeriesLayer* A = activeLayer();
    if (!A) return;
    const CubeInfo& a = *A->session->info;
    for (SeriesLayer& L : layers_) {
        if (&L == A) continue;
        const CubeInfo& b = *L.session->info;
        const int t = (a.timeIsDate && b.timeIsDate) ? nearestTime(b.layers, a.layers[t_].time)
                                                     : std::min(t_, b.T() - 1);
        if (t != L.disp.t) {
            L.disp.t = t;
            mapDirty_ = true;
        }
        L.session->overview.setFocus(L.disp.t);
    }
}

void App::setActive(int i) {
    if (i < 0 || i >= int(layers_.size())) return;
    SeriesLayer* prev = active_ >= 0 && active_ < int(layers_.size()) ? &layers_[active_] : nullptr;
    SeriesLayer& N = layers_[i];
    // The view (its centre and zoom), the pins and the transect live in map
    // space: carried to the new grid through the transformation that places
    // the new layer on the current map (straight or reprojected).
    const bool carry = prev && prev != &N;
    if (carry) updateAlignment(); // the new layer may have just been opened
    bool keepView = false;
    double viewX = 0, viewY = 0, viewScale = 0;
    if (carry && viewTouched_ && N.aligned) {
        const double mx = (canvasSize_.x * 0.5 - offset_.x) / scale_, my = (canvasSize_.y * 0.5 - offset_.y) / scale_;
        const double k = N.reproj ? N.reproj->layerPxPerMapPx(mx, my) : 1.0 / std::sqrt(std::fabs(N.bx * N.by));
        keepView = activeToLayer(N, mx, my, viewX, viewY) && k > 0;
        viewScale = scale_ / k;
    }
    std::vector<std::pair<double, double>> pinAt; // in the new layer's pixels (NaN: not on it)
    if (carry && N.aligned) // else (unrelated grids) they keep their pixel
        for (const SeriesView& p : pins_) {
            double lx = NAN, ly = NAN;
            if (!activeToLayer(N, p.x + 0.5, p.y + 0.5, lx, ly)) lx = ly = NAN;
            pinAt.push_back({lx, ly});
        }
    if (carry && N.reproj && tr_.on && tr_.geo) {
        // The transect's ends in the new layer's CRS (pumpTransect places them).
        const CubeInfo& pi = *prev->session->info;
        bool ok = true;
        for (double* g : {tr_.ga, tr_.gb}) {
            double px, py, lx, ly;
            if (pi.geoToPixel(g[0], g[1], px, py) && activeToLayer(N, px, py, lx, ly))
                N.session->info->pixelToGeo(lx, ly, g[0], g[1]);
            else
                ok = false;
        }
        if (!ok) clearTransect();
    }
    if (prev) saveDisplay(prev->disp);

    active_ = i;
    SeriesLayer& L = layers_[i];
    s_ = L.session.get();
    const CubeInfo& info = *s_->info;
    loadDisplay(L.disp);
    lastInputs_ = L.inputs;
    const int T = info.T();
    xs_.resize(T);
    years_.resize(T);
    zeitYears_.resize(T);
    for (int t = 0; t < T; ++t) {
        xs_[t] = info.layers[t].time;
        years_[t] = info.yearsFromStart(t);
        zeitYears_[t] = info.decimalYear(t);
    }
    if (!modeAvailable(mode_)) mode_ = ModeValue;
    updateAlignment();

    // View
    if (keepView) {
        scale_ = viewScale;
        offset_ = ImVec2(float(canvasSize_.x * 0.5 - viewX * scale_), float(canvasSize_.y * 0.5 - viewY * scale_));
    } else {
        fitRequested_ = true;
        viewTouched_ = false;
    }

    // Pins: remap to the new grid (drop the ones outside it), then re-read.
    std::vector<SeriesView> oldPins = std::move(pins_);
    pins_.clear();
    for (size_t k = 0; k < oldPins.size(); ++k) {
        int px = oldPins[k].x, py = oldPins[k].y;
        if (k < pinAt.size()) {
            const auto [lx, ly] = pinAt[k];
            if (!(lx >= 0 && ly >= 0 && lx < info.width && ly < info.height)) continue;
            px = int(lx);
            py = int(ly);
        }
        if (px < 0 || py < 0 || px >= info.width || py >= info.height) continue;
        SeriesView p;
        p.id = oldPins[k].id;
        p.color = oldPins[k].color;
        p.x = px;
        p.y = py;
        p.values = approxSeries(px, py);
        p.stats = computeSeriesStats(years_, p.values);
        p.request = s_->deferRandomReads() ? 0 : s_->requestSeries(px, py, false);
        pins_.push_back(std::move(p));
    }
    for (SeriesLayer& O : layers_) {
        O.pins.clear();
        O.hover = SeriesView{};
    }
    for (const SeriesView& p : pins_) addOtherPins(p);

    // Cursor and ROI belong to the previous grid.
    hover_ = SeriesView{};
    hoverPending_ = false;
    approxLayers_ = -1;
    clearRoi();
    roiZeitReq_ = 0;
    roiZeitVersion_ = -1;
    roiZeitResult_ = json();
    syncLayerTimes();
    mapDirty_ = true;

    const std::string title = "Janus - " + L.name + " (" + info.description + ")" +
                              (layers_.size() > 1 ? "  [" + std::to_string(layers_.size()) + " layers]" : "");
    glfwSetWindowTitle(window_, title.c_str());
}

void App::reopenLayer(int i, const BandSelection& sel) {
    if (opening_.valid() || i < 0 || i >= int(layers_.size())) return;
    openingSel_ = sel;
    openingSel_->chosen = true;
    openInputs(layers_[i].inputs, true);
    openingSel_.reset();
    openingReplace_ = i;
}

// The same files with another band selection: the layer keeps its place, name,
// visibility, opacity, display mode, date, pins, view and tool results; the
// value ranges go back to automatic since the values changed.
void App::replaceLayerSession(int i, std::shared_ptr<CubeInfo> info, double seconds) {
    SeriesLayer& L = layers_[i];
    const bool wasActive = i == active_;
    if (wasActive) saveDisplay(L.disp);
    const uint64_t oldId = L.session->info->id;
    const int T = info->T();
    LayerDisplay d;
    d.mode = L.disp.mode;
    d.cmap = L.disp.cmap;
    d.t = std::min(L.disp.t, T - 1);
    d.rgb = {0, T / 2, T - 1};
    d.diffRef = std::min(L.disp.diffRef, T - 1);
    const auto viewScale = scale_;
    const ImVec2 viewOffset = offset_;
    const bool touched = viewTouched_;
    if (wasActive) {
        clearRoi();
        s_ = nullptr;
        active_ = -1;
    }
    if (L.roi) L.roi->cancel = true;
    L.roi.reset();
    L.roiGen = 0; // read again from the new session
    L.session = std::make_unique<Session>(info, settings_, [] { glfwPostEmptyEvent(); });
    L.session->openSeconds = seconds;
    L.disp = d;
    L.years.resize(T);
    for (int t = 0; t < T; ++t) L.years[t] = info->yearsFromStart(t);
    for (ResultLayer& R : results_)
        if (R.cubeId == oldId) R.cubeId = info->id;
    if (wasActive) {
        setActive(i);
        scale_ = viewScale;
        offset_ = viewOffset;
        viewTouched_ = touched;
        fitRequested_ = false;
    } else {
        for (SeriesLayer& O : layers_) O.pins.clear();
        for (const SeriesView& p : pins_) addOtherPins(p);
        updateAlignment();
        syncLayerTimes();
        mapDirty_ = true;
    }
}

void App::removeLayer(int i) {
    if (i < 0 || i >= int(layers_.size())) return;
    clearResults(layers_[i].session->info->id);
    if (layers_[i].roi) layers_[i].roi->cancel = true;
    const bool wasActive = i == active_;
    if (wasActive) {
        clearRoi();
        s_ = nullptr;
    }
    layers_.erase(layers_.begin() + i);
    if (layers_.empty()) {
        active_ = -1;
        pins_.clear();
        hover_ = SeriesView{};
        glfwSetWindowTitle(window_, "Janus");
        return;
    }
    if (wasActive) {
        active_ = -1; // nothing to save: the removed layer's state is gone
        setActive(std::min(i, int(layers_.size()) - 1));
    } else {
        if (i < active_) --active_;
        updateAlignment();
        mapDirty_ = true;
    }
}

void App::closeAll() {
    clearResults();
    clearRoi();
    s_ = nullptr;
    layers_.clear();
    active_ = -1;
    pins_.clear();
    hover_ = SeriesView{};
    glfwSetWindowTitle(window_, "Janus");
}

// ---------------------------------------------------------------------------
// Series of the other layers (cursor and pins)
// ---------------------------------------------------------------------------

std::vector<float> App::approxSeriesOf(const Session& s, int x, int y) const {
    const Overview& ov = s.overview;
    const int ox = std::min(ov.w - 1, int(double(x) * ov.w / s.info->width));
    const int oy = std::min(ov.h - 1, int(double(y) * ov.h / s.info->height));
    std::vector<float> v(size_t(ov.T), NAN);
    for (int t = 0; t < ov.T; ++t)
        if (s.gpu.loaded[t]) v[t] = ov.at(t, ox, oy);
    return v;
}

void App::updateOtherHover(int ix, int iy) {
    if (chartLayers_ == 0) return;
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        int lx, ly;
        if (!L.visible || !fromActive(L, ix + 0.5, iy + 0.5, lx, ly)) {
            L.hover = SeriesView{};
            continue;
        }
        if (lx == L.hover.x && ly == L.hover.y) continue;
        L.hover = SeriesView{};
        L.hover.x = lx;
        L.hover.y = ly;
        L.hover.values = approxSeriesOf(*L.session, lx, ly);
        L.hover.stats = computeSeriesStats(L.years, L.hover.values);
        L.hover.color = ImVec4(0.95f, 0.95f, 0.95f, 1);
    }
}

// Exact reads for the other layers (after the same debounce as the active one).
void App::requestOtherSeries(bool hover) {
    if (chartLayers_ == 0) return;
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer() || !L.visible || L.session->deferRandomReads()) continue;
        if (hover && L.hover.x >= 0 && !L.hover.exact)
            L.hover.request = L.session->requestSeries(L.hover.x, L.hover.y, true);
        for (SeriesView& p : L.pins)
            if (!p.exact && p.request == 0) p.request = L.session->requestSeries(p.x, p.y, false);
    }
}

void App::pumpOtherSeries() {
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        if (L.years.empty()) {
            L.years.resize(L.session->info->T());
            for (int t = 0; t < L.session->info->T(); ++t) L.years[t] = L.session->info->yearsFromStart(t);
        }
        for (SeriesResult& r : L.session->takeSeries()) {
            auto apply = [&](SeriesView& v) {
                if (v.request != r.id) return false;
                v.values = std::move(r.values);
                v.exact = true;
                v.stats = computeSeriesStats(L.years, v.values);
                return true;
            };
            if (apply(L.hover)) continue;
            for (SeriesView& p : L.pins)
                if (apply(p)) break;
        }
    }
}

void App::addOtherPins(const SeriesView& pin) {
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        if (L.years.empty()) {
            L.years.resize(L.session->info->T());
            for (int t = 0; t < L.session->info->T(); ++t) L.years[t] = L.session->info->yearsFromStart(t);
        }
        int lx, ly;
        if (!fromActive(L, pin.x + 0.5, pin.y + 0.5, lx, ly)) continue;
        SeriesView p;
        p.id = pin.id;
        p.color = pin.color;
        p.x = lx;
        p.y = ly;
        p.values = approxSeriesOf(*L.session, lx, ly);
        p.stats = computeSeriesStats(L.years, p.values);
        L.pins.push_back(std::move(p));
    }
    requestOtherSeries(false);
}

void App::removePinById(int id) {
    pins_.erase(std::remove_if(pins_.begin(), pins_.end(), [id](const SeriesView& p) { return p.id == id; }),
                pins_.end());
    for (SeriesLayer& L : layers_)
        L.pins.erase(std::remove_if(L.pins.begin(), L.pins.end(), [id](const SeriesView& p) { return p.id == id; }),
                     L.pins.end());
}

void App::clearPins() {
    pins_.clear();
    for (SeriesLayer& L : layers_) L.pins.clear();
}

// ---------------------------------------------------------------------------
// ROI on the other layers
// ---------------------------------------------------------------------------

void App::setRoiRect(const int r[4]) {
    const CubeInfo& info = *s_->info; // clamped as Session::startRoi does
    roiRectXY_[0] = std::clamp(std::min(r[0], r[2]), 0, info.width - 1);
    roiRectXY_[1] = std::clamp(std::min(r[1], r[3]), 0, info.height - 1);
    roiRectXY_[2] = std::clamp(std::max(r[0], r[2]), roiRectXY_[0] + 1, info.width);
    roiRectXY_[3] = std::clamp(std::max(r[1], r[3]), roiRectXY_[1] + 1, info.height);
    roiRect_ = true;
    ++roiGen_;
}

void App::clearOtherRois() {
    for (SeriesLayer& L : layers_) {
        if (L.roi) L.roi->cancel = true;
        L.roi.reset();
        L.roiGen = 0;
        L.roiSeen = -1;
        L.roiMean.clear();
    }
}

// With "All visible layers", each visible layer gets the map's rectangle in
// its own pixels: those whose centres are inside it (the rectangle as a
// polygon through 16 points per edge for a reprojected layer), read like the
// active layer's ROI (full-resolution cache when there is one).
void App::updateOtherRois() {
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        const Session& S = *L.session;
        const CubeInfo& info = *S.info;
        if (roiRect_ && chartLayers_ == 1 && L.visible && L.aligned && L.roiGen != roiGen_ && !S.deferRandomReads()) {
            if (L.roi) L.roi->cancel = true;
            L.roi.reset();
            L.roiGen = roiGen_;
            L.roiSeen = -1;
            L.roiMean.clear();
            const double x0 = roiRectXY_[0], y0 = roiRectXY_[1], x1 = roiRectXY_[2], y1 = roiRectXY_[3];
            std::vector<double> poly;
            double b[4];
            if (L.reproj) {
                poly = L.reproj->polygon(x0, y0, x1, y1, 16);
                if (poly.size() < 6) continue;
                b[0] = b[1] = HUGE_VAL;
                b[2] = b[3] = -HUGE_VAL;
                for (size_t i = 0; i + 1 < poly.size(); i += 2) {
                    b[0] = std::min(b[0], poly[i]);
                    b[1] = std::min(b[1], poly[i + 1]);
                    b[2] = std::max(b[2], poly[i]);
                    b[3] = std::max(b[3], poly[i + 1]);
                }
            } else {
                double ax0, ay0, ax1, ay1;
                activeToLayer(L, x0, y0, ax0, ay0);
                activeToLayer(L, x1, y1, ax1, ay1);
                b[0] = std::min(ax0, ax1);
                b[1] = std::min(ay0, ay1);
                b[2] = std::max(ax0, ax1);
                b[3] = std::max(ay0, ay1);
            }
            // Pixels whose centres are inside: [ceil(b0 - 0.5), ceil(b1 - 0.5)).
            const int lx0 = std::max(0, int(std::ceil(b[0] - 0.5))), ly0 = std::max(0, int(std::ceil(b[1] - 0.5)));
            const int lx1 = std::min(info.width, int(std::ceil(b[2] - 0.5)));
            const int ly1 = std::min(info.height, int(std::ceil(b[3] - 0.5)));
            if (lx1 <= lx0 || ly1 <= ly0) continue; // the ROI misses this layer
            L.roi = L.session->startRoi(lx0, ly0, lx1, ly1, poly);
        }
        if (!L.roi || L.roi->done.load() == L.roiSeen) continue;
        L.roiSeen = L.roi->done.load();
        const int T = info.T();
        L.roiMean.assign(T, NAN);
        L.roiP10.assign(T, NAN);
        L.roiP90.assign(T, NAN);
        for (int t = 0; t < T; ++t) {
            const SampleStats& st = L.roi->perT[t];
            if (st.n == 0) continue;
            L.roiMean[t] = st.mean;
            L.roiP10[t] = st.p10;
            L.roiP90[t] = st.p90;
        }
        if (L.years.size() != size_t(T)) {
            L.years.resize(T);
            for (int t = 0; t < T; ++t) L.years[t] = info.yearsFromStart(t);
        }
        L.roiStats = computeSeriesStats(L.years, L.roiMean);
    }
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

void App::uiLayers() {
    if (!ImGui::Begin("Layers")) {
        ImGui::End();
        return;
    }
    if (layers_.empty()) {
        ImGui::TextDisabled("No layers. Open a series (File, Files panel,\ndrag and drop or the command line).");
        ImGui::End();
        return;
    }
    if (ImGui::Button("Add layer...")) {
        auto files = platform::openFilesDialog();
        if (!files.empty()) openInputs(files, true);
    }
    ImGui::SetItemTooltip("Open another series as a new layer (files)");
    ImGui::SameLine();
    if (ImGui::Button("Add folder...")) {
        const std::string dir = platform::openFolderDialog();
        if (!dir.empty()) openInputs({dir}, true);
    }
    ImGui::SetItemTooltip("Open another series as a new layer (a folder, 1 file per date)");
    ImGui::TextDisabled("Top of the list = drawn on top. Click a name to make it active.");
    ImGui::Separator();

    int moveUp = -1, moveDown = -1, remove = -1, activate = -1;
    // Listed top-down: the last layer is drawn last (on top).
    for (int i = int(layers_.size()) - 1; i >= 0; --i) {
        SeriesLayer& L = layers_[i];
        ImGui::PushID(i);
        if (ImGui::Checkbox("##vis", &L.visible)) mapDirty_ = true;
        ImGui::SetItemTooltip(L.visible ? "Hide this layer" : "Show this layer");
        ImGui::SameLine();
        const bool isActive = i == active_;
        ImGui::PushStyleColor(ImGuiCol_Text, isActive ? ImVec4(0.55f, 0.80f, 1.0f, 1) : ImGui::GetStyleColorVec4(ImGuiCol_Text));
        const std::string label = (isActive ? "> " : "  ") + L.name;
        if (ImGui::Selectable(label.c_str(), isActive, 0, ImVec2(ImGui::GetContentRegionAvail().x - 76, 0)) && !isActive)
            activate = i;
        ImGui::PopStyleColor();
        const CubeInfo& info = *L.session->info;
        ImGui::SetItemTooltip("%s\n%s, %d x %d px, %d dates (%s .. %s)\nCRS: %s", L.inputs.size() == 1 ? L.inputs[0].c_str() : info.firstPath.c_str(),
                              info.description.c_str(), info.width, info.height, info.T(),
                              info.layers.front().label.c_str(), info.layers.back().label.c_str(),
                              info.crsAuthority.empty() ? info.crsName.c_str() : info.crsAuthority.c_str());
        ImGui::SameLine();
        ImGui::BeginDisabled(i == int(layers_.size()) - 1);
        if (ImGui::ArrowButton("##up", ImGuiDir_Up)) moveUp = i;
        ImGui::EndDisabled();
        ImGui::SameLine();
        ImGui::BeginDisabled(i == 0);
        if (ImGui::ArrowButton("##down", ImGuiDir_Down)) moveDown = i;
        ImGui::EndDisabled();
        ImGui::SameLine();
        if (ImGui::SmallButton("x")) remove = i;
        ImGui::SetItemTooltip("Close this layer");

        ImGui::Indent();
        if (!isActive) {
            ImGui::TextDisabled("date %s", info.layers[L.disp.t].label.c_str());
            if (!L.session->overview.complete())
                ImGui::TextDisabled("loading %d/%d", L.session->overview.layersDone(), info.T());
        }
        if (!L.alignNote.empty()) ImGui::TextColored(ImVec4(1.0f, 0.7f, 0.4f, 1), "%s", L.alignNote.c_str());
        if (L.reproj && !isActive) {
            const Reprojection& R = *L.reproj;
            if (R.sameCrs) ImGui::TextDisabled("rotated grid, placed on the map's");
            else ImGui::TextDisabled("reprojected from %s", R.crsText.c_str());
            ImGui::SetItemTooltip("Drawn through a grid of %d x %d points (interpolation error up to %.2g of\n"
                                  "its pixels, made in %.0f ms); cursor, pins and ROI use the exact\n"
                                  "transformation (PROJ). Nearest neighbour: the values are not changed.",
                                  R.gridW, R.gridH, R.gridError, R.buildMs);
        }
        ImGui::SetNextItemWidth(-1);
        if (ImGui::SliderFloat("##opacity", &L.opacity, 0.05f, 1.0f, "opacity %.2f")) mapDirty_ = true;
        uiResultsOf(info.id);
        ImGui::Unindent();
        ImGui::Separator();
        ImGui::PopID();
    }
    if (moveUp >= 0) {
        std::swap(layers_[moveUp], layers_[moveUp + 1]);
        if (active_ == moveUp) active_ = moveUp + 1;
        else if (active_ == moveUp + 1) active_ = moveUp;
        mapDirty_ = true;
    }
    if (moveDown >= 0) {
        std::swap(layers_[moveDown], layers_[moveDown - 1]);
        if (active_ == moveDown) active_ = moveDown - 1;
        else if (active_ == moveDown - 1) active_ = moveDown;
        mapDirty_ = true;
    }
    if (activate >= 0) setActive(activate);
    if (remove >= 0) removeLayer(remove);
    ImGui::End();
}

void App::uiFiles() {
    if (!ImGui::Begin("Files")) {
        ImGui::End();
        return;
    }
    const FileBrowser::Action act = files_.draw();
    if (act.kind != FileBrowser::Action::None && !act.paths.empty())
        openInputs(act.paths, act.kind == FileBrowser::Action::AddLayer && !layers_.empty());
    ImGui::End();
}
