// `jn --selftest-ui A B`: drives the layers workflow in a hidden window, one
// step per frame, without touching the mouse or keyboard. Opens A, adds B as a
// layer, checks the georeferenced alignment, the series of both layers under a
// cursor and a pin, reads back map pixels (layer drawn, layer hidden), switches
// the active layer and closes one. Optional inputs:
//   C  one file per date with several bands and a quality band: opens it and
//      reopens it in place as a normalized difference with the QA mask;
//   D  categorical series with a colour table and class names (file colours);
//   E  categorical series without them (detected from its values).
// With A and B open it also checks the swipe and the space-time transect.
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <thread>

#include <implot.h>

namespace {

double now() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

} // namespace

int App::selfTestStep(const std::vector<std::string>& in) {
    const std::vector<std::string> a = {in[0]}, b = {in[1]};
    const std::vector<std::string> c = in.size() > 2 ? std::vector<std::string>{in[2]} : std::vector<std::string>{};
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    auto loaded = [&](const SeriesLayer& L) {
        return L.session->overview.complete() && (L.session->info->T() < 2 || L.session->gpu.statsValid);
    };
    if (st_.stage > 0 && now() - st_.since > 120) return fail("timeout");

    switch (st_.stage) {
    case 0:
        st_.t0 = st_.since = now();
        openInputs(a);
        next("open A");
        break;
    case 1:
        if (layers_.size() == 1 && loaded(layers_[0]) && layers_[0].classes.state != LayerClasses::Undecided) {
            if (layers_[0].classes.state != LayerClasses::Off) return fail("A (continuous) must not be categorical");
            next("A loaded (continuous)");
        }
        break;
    case 2:
        openInputs(b, true);
        next("add B as a layer");
        break;
    case 3:
        if (layers_.size() == 2 && loaded(layers_[1])) {
            if (active_ != 1) return fail("the added layer should be active");
            const SeriesLayer& A = layers_[0];
            std::printf("    layer A in B's grid: x' = %.3f + %.3f x, y' = %.3f + %.3f y (aligned %d)\n", A.ax, A.bx,
                        A.ay, A.by, int(A.aligned));
            if (!A.aligned) return fail("A should be aligned with B (same CRS)");
            next("B loaded, alignment checked");
        }
        break;
    case 4: {
        // Cursor and a pin at the same place, in the overlap of both layers.
        chartLayers_ = 1;
        const int ix = 50, iy = 100; // B's pixel 50 = A's pixel 150 (B starts 100 px east)
        hover_ = SeriesView{};
        hover_.x = ix;
        hover_.y = iy;
        hover_.values = approxSeries(ix, iy);
        hover_.color = ImVec4(1, 1, 1, 1);
        hover_.request = s_->requestSeries(ix, iy, true);
        updateOtherHover(ix, iy);
        requestOtherSeries(true);
        addPin(ix, iy);
        addOtherPins(pins_.back());
        next("cursor and pin placed");
        break;
    }
    case 5: {
        const SeriesLayer& A = layers_[0];
        const bool ready = hover_.exact && A.hover.exact && !pins_.empty() && pins_[0].exact && !A.pins.empty() &&
                           A.pins[0].exact;
        if (!ready) break;
        std::printf("    cursor: B(%d,%d) = %.3f at %s | A(%d,%d) = %.3f at %s\n", hover_.x, hover_.y,
                    hover_.values[0], s_->info->layers[0].label.c_str(), A.hover.x, A.hover.y, A.hover.values[0],
                    A.session->info->layers[0].label.c_str());
        if (A.hover.x != hover_.x + 100) return fail("A's cursor pixel should be B's + 100");
        if (std::fabs(hover_.values[0] - 2.0f) > 1e-4) return fail("B's first value should be 2.0");
        next("exact series of both layers under the cursor and the pin");
        break;
    }
    case 6: {
        canvasSize_ = ImVec2(400, 300);
        fitView(canvasSize_);
        renderMap(400, 300);
        unsigned char c[4];
        gpu_.readMapPixel(200, 150, c);
        std::printf("    map center with both layers: rgb(%d, %d, %d)\n", c[0], c[1], c[2]);
        st_.color[0] = c[0];
        st_.color[1] = c[1];
        st_.color[2] = c[2];
        layers_[1].visible = false; // hide the active layer: A must show through
        renderMap(400, 300);
        gpu_.readMapPixel(200, 150, c);
        std::printf("    map center with B hidden:    rgb(%d, %d, %d)\n", c[0], c[1], c[2]);
        if (c[0] == st_.color[0] && c[1] == st_.color[1] && c[2] == st_.color[2])
            return fail("hiding the top layer did not change the map");
        {
            // A map panel showing A on the same view draws what the main map
            // shows of A alone.
            MapView v;
            v.id = 99;
            v.cube = layers_[0].session->info->id;
            renderView(v, 400, 300, 1.0f, ImVec2(0, 0));
            unsigned char pv[4];
            gpu_.readMapPixel(200, 150, pv, v.id);
            gpu_.releaseMap(v.id);
            std::printf("    map panel showing A:         rgb(%d, %d, %d)\n", pv[0], pv[1], pv[2]);
            if (pv[0] != c[0] || pv[1] != c[1] || pv[2] != c[2])
                return fail("a map panel of A should draw what the main map shows of A");
        }
        layers_[0].visible = false;
        renderMap(400, 300);
        gpu_.readMapPixel(200, 150, c);
        std::printf("    map center with both hidden: rgb(%d, %d, %d)\n", c[0], c[1], c[2]);
        if (c[0] > 40 || c[1] > 40 || c[2] > 40) return fail("with every layer hidden the map should be empty");
        layers_[0].visible = layers_[1].visible = true;
        if (const char* err = selfTestCompare()) return fail(err);
        next("layer drawing and visibility, swipe, transect");
        break;
    }
    case 7: {
        const int pinBefore = pins_[0].x;
        setActive(0);
        if (s_ != layers_[0].session.get()) return fail("A should be active");
        std::printf("    pin x: %d in B's grid -> %d in A's grid; B in A's grid: x' = %.1f + %.1f x\n", pinBefore,
                    pins_.empty() ? -1 : pins_[0].x, layers_[1].ax, layers_[1].bx);
        if (pins_.empty() || pins_[0].x != pinBefore + 100) return fail("the pin should move to A's grid (+100)");
        next("active layer switched, pin remapped");
        break;
    }
    case 8:
        removeLayer(1);
        if (layers_.size() != 1 || active_ != 0) return fail("closing B should leave A active");
        next("layer closed");
        if (c.empty()) {
            std::printf("OK (%.1f s)\n", now() - st_.t0);
            return 0;
        }
        break;
    case 9:
        openInputs(c);
        next("open C (bands per date)");
        break;
    case 10:
        if (layers_.size() == 1 && loaded(layers_[0]) && layers_[0].session->info->bandsPerDate > 1) {
            const CubeInfo& info = *s_->info;
            std::printf("    %s; %d bands per date, shown: %s\n", info.description.c_str(), info.bandsPerDate,
                        info.selectionText().c_str());
            if (info.sel.qaBand == 0) return fail("the Fmask band should be used automatically");
            addPin(info.width - 10, info.height / 2);
            canvasSize_ = ImVec2(400, 300);
            fitView(canvasSize_);
            scale_ *= 2; // a view of our own, to check it is kept
            viewTouched_ = true;
            st_.since = now();
            st_.color[0] = 0;
            next("C loaded, quality band detected");
        }
        break;
    case 11: {
        if (!pins_.empty() && !pins_[0].exact) break;
        BandSelection sel = s_->info->sel;
        sel.band = 4;   // NIR
        sel.ndBand = 3; // Red -> NDVI
        reopenLayer(active_, sel);
        next("reopen C as ND(NIR, Red)");
        break;
    }
    case 12: {
        const SeriesLayer& L = layers_[0];
        if (opening_.valid() || L.session->info->sel.ndBand != 3 || !loaded(L) || pins_.empty() || !pins_[0].exact)
            break;
        const CubeInfo& info = *s_->info;
        float lo = 1e9f, hi = -1e9f;
        int nan = 0;
        for (float v : pins_[0].values) {
            if (!std::isfinite(v)) {
                ++nan;
                continue;
            }
            lo = std::min(lo, v);
            hi = std::max(hi, v);
        }
        std::printf("    %s (%s); pin at (%d,%d): NDVI %.3f .. %.3f, %d of %d dates masked or missing\n",
                    L.name.c_str(), info.description.c_str(), pins_[0].x, pins_[0].y, lo, hi, nan, info.T());
        if (layers_.size() != 1 || active_ != 0) return fail("the reopened layer should stay in place and active");
        if (lo < -1 || hi > 1) return fail("a normalized difference is within [-1, 1]");
        if (nan == 0) return fail("cloudy dates should be masked by the QA band");
        if (fitRequested_ || !viewTouched_) return fail("the view should be kept");
        next("reopened in place: name, pin and view kept, values are NDVI, clouds masked");
        if (in.size() < 4) {
            std::printf("OK (%.1f s)\n", now() - st_.t0);
            return 0;
        }
        break;
    }
    case 13:
    case 16: {
        const bool withTable = st_.stage == 13;
        if (!withTable && in.size() < 5) {
            std::printf("OK (%.1f s)\n", now() - st_.t0);
            return 0;
        }
        openInputs({in[withTable ? 3 : 4]});
        next(withTable ? "open D (categorical, colour table)" : "open E (categorical, plain values)");
        break;
    }
    case 14:
    case 17: {
        if (opening_.valid() || layers_.size() != 1 || layers_[0].inputs.front() != in[st_.stage == 14 ? 3 : 4] ||
            !loaded(layers_[0]) || layers_[0].classes.state == LayerClasses::Undecided)
            break;
        const LayerClasses& C = layers_[0].classes;
        if (C.state != LayerClasses::On) {
            const CubeInfo& ci = *layers_[0].session->info;
            const Overview& ov = layers_[0].session->overview;
            std::printf("    file categorical %d, %d colours, %d names; overview %dx%d, first values:", int(ci.fileCategorical),
                        int(ci.classColors.size()), int(ci.classNames.size()), ov.w, ov.h);
            for (int i = 0; i < 8 && i < ov.w * ov.h; ++i) std::printf(" %g", ov.layer(0)[i * 97 % (ov.w * ov.h)]);
            std::printf("\n");
            return fail("the series should be detected as categorical");
        }
        std::printf("    %s: %d classes:", layers_[0].session->info->description.c_str(), int(C.list.size()));
        for (const ClassEntry& e : C.list)
            std::printf(" %d=%s rgb(%d,%d,%d)", e.value, e.name.c_str(), int(e.color.x * 255 + 0.5f),
                        int(e.color.y * 255 + 0.5f), int(e.color.z * 255 + 0.5f));
        std::printf("\n");
        std::vector<int> values;
        for (const ClassEntry& e : C.list) values.push_back(e.value);
        if (values != std::vector<int>{3, 15, 24, 33}) return fail("classes should be 3, 15, 24, 33");
        for (size_t i = 0; i < C.list.size(); ++i)
            for (size_t j = i + 1; j < C.list.size(); ++j)
                if (C.list[i].color.x == C.list[j].color.x && C.list[i].color.y == C.list[j].color.y &&
                    C.list[i].color.z == C.list[j].color.z)
                    return fail("every class needs its own colour");
        const ClassEntry* forest = C.find(3);
        if (st_.stage == 14 && (forest->name != "Forest" || int(forest->color.x * 255 + 0.5f) != 31))
            return fail("the file's class names and colours should be used");
        if (!modeAvailable(ModeValue) || modeAvailable(ModeMean)) return fail("only the value mode makes sense");
        next("classes detected");
        break;
    }
    case 15:
    case 18: {
        // Map centre at the first date: forest, drawn in its class colour.
        setT(0);
        canvasSize_ = ImVec2(400, 300);
        fitView(canvasSize_);
        renderMap(400, 300);
        unsigned char px[4];
        gpu_.readMapPixel(200, 150, px);
        const ClassEntry* forest = layers_[0].classes.find(3);
        const int r = int(forest->color.x * 255 + 0.5f), g = int(forest->color.y * 255 + 0.5f),
                  b2 = int(forest->color.z * 255 + 0.5f);
        std::printf("    map centre: rgb(%d, %d, %d), forest colour rgb(%d, %d, %d)\n", px[0], px[1], px[2], r, g, b2);
        if (std::abs(px[0] - r) > 2 || std::abs(px[1] - g) > 2 || std::abs(px[2] - b2) > 2)
            return fail("the map should show the class colour");
        // A pixel that changed class: its summary in the statistics.
        hover_ = SeriesView{};
        hover_.x = 20;
        hover_.y = 40;
        hover_.values = approxSeries(20, 40);
        const auto rows = classSummary(layers_[0].classes, *s_->info, hover_.values, t_);
        for (const auto& [k, v] : rows) std::printf("    %-18s %s\n", k.c_str(), v.c_str());
        if (const char* err = selfTestCompare()) return fail(err);
        next("map in class colours, class summary, transect in class colours");
        break;
    }
    case 19:
        std::printf("OK (%.1f s)\n", now() - st_.t0);
        return 0;
    }
    return -1;
}

// Swipe and transect, with A and B open (B active, both visible); with a
// categorical series, the transect in class colours. Returns the failure, or
// null. Both are left on, so the next frames draw them too.
const char* App::selfTestCompare() {
    if (const LayerClasses* C = activeClasses()) {
        // D/E: pasture (15) left of column 40, forest (3) right of it, at the first date.
        setT(0);
        setTransect(ImVec2(10.5f, 60.5f), ImVec2(190.5f, 60.5f));
        const double w0 = now();
        while (transectRowsRead() < tr_.T) {
            if (now() - w0 > 30) return "timeout reading the transect at full resolution";
            std::this_thread::sleep_for(std::chrono::milliseconds(5));
        }
        updateTransectRows();
        publishTransect();
        renderTransect(tr_.n, tr_.T);
        for (const auto& [i, cls] : {std::pair<int, int>{5, 15}, {100, 3}}) {
            unsigned char px[4];
            gpu_.readMapPixel(i, 0, px, kTransectSlot);
            const ImVec4 c = C->find(cls)->color;
            const int e[3] = {int(c.x * 255 + 0.5f), int(c.y * 255 + 0.5f), int(c.z * 255 + 0.5f)};
            std::printf("    transect at column %d: class %g, rgb(%d, %d, %d), class colour rgb(%d, %d, %d)\n", tr_.px[i],
                        tr_.values[i], px[0], px[1], px[2], e[0], e[1], e[2]);
            if (tr_.values[i] != float(cls) || std::abs(px[0] - e[0]) > 2 || std::abs(px[1] - e[1]) > 2 ||
                std::abs(px[2] - e[2]) > 2)
                return "the transect should be drawn in class colours";
        }
        return nullptr;
    }
    const int W = 400, H = 300;
    canvasSize_ = ImVec2(float(W), float(H));
    fitView(canvasSize_);
    auto same = [](const unsigned char* a, const unsigned char* b) { return a[0] == b[0] && a[1] == b[1] && a[2] == b[2]; };

    // Swipe: A (the other layer) right of the divider, the main map left of it.
    if (swipe_) toggleSwipe();
    toggleSwipe();
    if (!swipe_ || swipeView_.cube != layers_[0].session->info->id) return "the swipe should compare with the other layer";
    swipeX_ = 0.5f;
    const int t0 = t_;
    renderMap(W, H);
    renderSwipe(canvasSize_, 1.0f);
    unsigned char left[4], right[4], main[4], aOnly[4];
    gpu_.readMapPixel(190, 150, left);
    gpu_.readMapPixel(210, 150, right, kSwipeSlot);
    gpu_.readMapPixel(210, 150, main);
    layers_[1].visible = false;
    renderMap(W, H);
    gpu_.readMapPixel(210, 150, aOnly);
    layers_[1].visible = true;
    std::printf("    swipe: left (main map) rgb(%d, %d, %d) | right (A) rgb(%d, %d, %d); there, the main map is\n"
                "           rgb(%d, %d, %d) and A alone rgb(%d, %d, %d)\n",
                left[0], left[1], left[2], right[0], right[1], right[2], main[0], main[1], main[2], aOnly[0], aOnly[1],
                aOnly[2]);
    if (!same(right, aOnly)) return "the right side of the swipe should show layer A";
    if (same(right, main)) return "the right side of the swipe should differ from the main map";
    // The same layer (B) at its own date: what the main map shows at that date.
    const int T = s_->info->T();
    swipeView_.cube = s_->info->id;
    swipeView_.ownDate = true;
    swipeView_.t = 0;
    setT(T - 1);
    renderMap(W, H);
    renderSwipe(canvasSize_, 1.0f);
    gpu_.readMapPixel(210, 150, right, kSwipeSlot);
    gpu_.readMapPixel(210, 150, main);
    setT(0);
    renderMap(W, H);
    gpu_.readMapPixel(210, 150, aOnly);
    std::printf("    swipe: B at %s rgb(%d, %d, %d) next to B at %s rgb(%d, %d, %d)\n", s_->info->layers[0].label.c_str(),
                right[0], right[1], right[2], s_->info->layers[T - 1].label.c_str(), main[0], main[1], main[2]);
    if (!same(right, aOnly) || same(right, main)) return "the swipe should show the layer at its own date";
    setT(t0);

    // Transect along B's row 100, columns 10 to 290: exact values 2 + 0.01 (x - 50) + 0.1 t.
    setTransect(ImVec2(10.5f, 100.5f), ImVec2(290.5f, 100.5f));
    if (!tr_.on || tr_.n != 281 || tr_.T != T) return "the transect should have 281 samples x every date";
    std::printf("    transect: %d samples x %d dates, %.0f %s, %d dates from the overview at once\n", tr_.n, tr_.T,
                tr_.length, tr_.unit.c_str(), int(std::count(tr_.state.begin(), tr_.state.end(), 1)));
    if (tr_.unit != "m" || std::fabs(tr_.length - 280 * 30) > 1e-6) return "the transect should be 8400 m long";
    const Overview& ov = s_->overview;
    for (int t = 0; t < T; ++t)
        for (int i = 0; i < tr_.n; ++i) {
            const int ox = std::min(ov.w - 1, int(double(tr_.px[i]) * ov.w / s_->info->width));
            const int oy = std::min(ov.h - 1, int(double(tr_.py[i]) * ov.h / s_->info->height));
            if (tr_.state[t] == 0 || tr_.values[size_t(t) * tr_.n + i] != ov.at(t, ox, oy))
                return "the transect should be filled from the overview";
        }
    const double w0 = now();
    while (transectRowsRead() < T) {
        if (now() - w0 > 30) return "timeout reading the transect at full resolution";
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }
    updateTransectRows();
    publishTransect();
    double maxErr = 0;
    for (int t = 0; t < T; ++t) {
        if (tr_.state[t] != 2) return "every date should be read at full resolution";
        for (int i = 0; i < tr_.n; ++i)
            maxErr = std::max(maxErr, std::fabs(tr_.values[size_t(t) * tr_.n + i] - (2.0 + 0.01 * (tr_.px[i] - 50) + 0.1 * t)));
    }
    std::printf("    transect at full resolution in %.0f ms, max error %.2g\n", (now() - w0) * 1000, maxErr);
    if (maxErr > 1e-4) return "wrong full-resolution transect values";
    // The image: cell (i, t) in the map's colours.
    const int i = 140, t = 5;
    renderTransect(tr_.n * 2, T * 2);
    unsigned char px[4];
    gpu_.readMapPixel(2 * i + 1, 2 * t + 1, px, kTransectSlot);
    int cmap;
    float lo, hi;
    transectColors(cmap, lo, hi);
    const float v = tr_.values[size_t(t) * tr_.n + i];
    const ImVec4 c = ImPlot::SampleColormap(std::clamp((v - lo) / (hi - lo), 0.0f, 1.0f), cmap);
    const int e[3] = {int(c.x * 255 + 0.5f), int(c.y * 255 + 0.5f), int(c.z * 255 + 0.5f)};
    std::printf("    transect image at (%d, %s): value %.3f -> rgb(%d, %d, %d), colormap rgb(%d, %d, %d)\n", tr_.px[i],
                s_->info->layers[t].label.c_str(), v, px[0], px[1], px[2], e[0], e[1], e[2]);
    if (std::abs(px[0] - e[0]) > 4 || std::abs(px[1] - e[1]) > 4 || std::abs(px[2] - e[2]) > 4)
        return "the transect image should use the layer's colormap and range";
    // Another active layer: the same line on its grid (A's pixels = B's + 100).
    setActive(0);
    pumpTransect();
    const float ax = tr_.a.x;
    const int an = tr_.n;
    setActive(1);
    pumpTransect();
    std::printf("    transect: starts at x %.1f on A (%d samples, clipped to A), %.1f on B again (%d samples)\n", ax, an,
                tr_.a.x, tr_.n);
    if (!tr_.on || std::fabs(ax - 110.5f) > 1e-3f || an != 191 || std::fabs(tr_.a.x - 10.5f) > 1e-3f || tr_.n != 281)
        return "the transect should follow the active layer";
    return nullptr;
}
