// `jn --selftest-ui A B`: drives the layers workflow in a hidden window, one
// step per frame, without touching the mouse or keyboard. Opens A, adds B as a
// layer, checks the georeferenced alignment, the series of both layers under a
// cursor and a pin, reads back map pixels (layer drawn, layer hidden), switches
// the active layer and closes one. Optional inputs:
//   C  one file per date with several bands and a quality band: opens it and
//      reopens it in place as a normalized difference with the QA mask, then
//      checks the difference and largest-drop maps (GPU against the CPU);
//   D  categorical series with a colour table and class names (file colours);
//   E  categorical series without them (detected from its values).
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>

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

    // Difference and largest-drop maps of the active layer (statistics computed):
    // the GPU statistics against the CPU rule for every overview pixel, then map
    // pixels against the colour expected from the CPU value. nullptr = passed.
    auto checkChangeMaps = [&]() -> const char* {
        const Overview& ov = s_->overview;
        const CubeInfo& info = *s_->info;
        const int T = info.T();
        const float* s1 = s_->gpu.hostStats1;
        const float* s2 = s_->gpu.hostStats2;
        int drops = 0, gappy = 0, ties = 0;
        double maxErr = 0;
        std::vector<float> v(T);
        for (size_t p = 0; p < size_t(ov.w) * ov.h; ++p) {
            bool gap = false;
            for (int t = 0; t < T; ++t) {
                v[t] = ov.layer(t)[p];
                gap |= std::isnan(v[t]);
            }
            double mag;
            int at;
            largestDrop(v, mag, at);
            const float gMag = s1[p * 4 + 3], gAt = s2[p];
            if (std::isnan(mag) != std::isnan(gMag) || (at < 0) != std::isnan(gAt)) return "drop: NaN where the CPU has none";
            if (std::isnan(mag)) continue;
            maxErr = std::max(maxErr, std::fabs(mag - gMag));
            if (std::fabs(mag - gMag) > 1e-5) return "drop magnitude differs from the CPU";
            if (at >= 0 && int(gAt) != at) {
                if (++ties > 3) return "drop date differs from the CPU"; // only a near tie may differ
                continue;
            }
            drops += at >= 0;
            gappy += at >= 0 && gap;
        }
        std::printf("    largest drop: %d pixels with a drop (%d with masked dates), max |GPU - CPU| %.2g, %d near ties\n",
                    drops, gappy, maxErr, ties);
        if (drops == 0 || gappy == 0) return "the test data should have drops, some across masked dates";

        // Map pixels: the colour of the CPU value, or the background where there is none.
        canvasSize_ = ImVec2(400, 300);
        fitView(canvasSize_);
        auto expect = [&](int mode, float lo, float hi, int ix, int iy, float value) -> bool {
            mode_ = mode;
            range_[mode] = Range{lo, hi, true, 0};
            renderMap(400, 300);
            unsigned char px[4];
            const int cx = int(offset_.x + (ix + 0.5) * scale_), cy = int(offset_.y + (iy + 0.5) * scale_);
            gpu_.readMapPixel(cx, cy, px);
            int e[3] = {26, 26, 29}; // background (renderMap's clear colour)
            if (!std::isnan(value)) {
                const ImVec4 c = ImPlot::SampleColormap(std::clamp((value - lo) / (hi - lo), 0.0f, 1.0f), cmap_[mode]);
                e[0] = int(c.x * 255 + 0.5f);
                e[1] = int(c.y * 255 + 0.5f);
                e[2] = int(c.z * 255 + 0.5f);
            }
            const bool ok = std::abs(px[0] - e[0]) <= 3 && std::abs(px[1] - e[1]) <= 3 && std::abs(px[2] - e[2]) <= 3;
            if (!ok)
                std::printf("    mode %d at (%d,%d): value %.4g, map rgb(%d, %d, %d), expected rgb(%d, %d, %d)\n",
                            mode, ix, iy, value, px[0], px[1], px[2], e[0], e[1], e[2]);
            return ok;
        };
        const int savedT = t_, savedRef = diffRef_;
        const int pts[][2] = {{10, 10}, {60, 70}, {150, 30}, {180, 100}, {120, 60}};
        int checked = 0;
        for (const auto& q : pts) {
            const int ix = q[0] * info.width / 200, iy = q[1] * info.height / 120;
            const std::vector<float> sv = approxSeries(ix, iy);
            double mag;
            int at;
            largestDrop(sv, mag, at);
            if (!expect(ModeDropMag, 0, 0.05f, ix, iy, float(mag))) return "largest drop magnitude: wrong colour on the map";
            if (!expect(ModeDropDate, 0, float(T - 1), ix, iy, at >= 0 ? float(at) : NAN))
                return "largest drop date: wrong colour on the map";
            // Difference: fixed reference (first date), then the previous date.
            for (int ref : {0, -1})
                for (int t : {1, 2, 3, T - 1}) {
                    diffRef_ = ref;
                    t_ = t;
                    const int r = ref < 0 ? t - 1 : ref;
                    if (!expect(ModeDiff, -0.05f, 0.05f, ix, iy, sv[t] - sv[r])) return "difference: wrong colour on the map";
                    checked += !std::isnan(sv[t] - sv[r]);
                }
        }
        std::printf("    map pixels of the drop and difference modes match the CPU (%d finite differences)\n", checked);
        t_ = savedT;
        diffRef_ = savedRef;
        mode_ = ModeValue;
        mapDirty_ = true;
        return nullptr;
    };

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
        next("layer drawing and visibility");
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
        if (const char* e = checkChangeMaps()) return fail(e);
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
        next("map in class colours, class summary");
        break;
    }
    case 19:
        std::printf("OK (%.1f s)\n", now() - st_.t0);
        return 0;
    }
    return -1;
}
