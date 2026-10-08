// `jn --selftest-ui A B`: drives the layers workflow in a hidden window, one
// step per frame, without touching the mouse or keyboard. Opens A, adds B as a
// layer, checks the georeferenced alignment, the series of both layers under a
// cursor and a pin, reads back map pixels (layer drawn, layer hidden), switches
// the active layer and closes one. Optional inputs:
//   C  one file per date with several bands and a quality band: opens it and
//      reopens it in place as a normalized difference with the QA mask;
//   D  categorical series with a colour table and class names (file colours);
//   E  categorical series without them (detected from its values).
// Between the series and the map checks, the full-resolution cache of B must
// give exactly the source values (stages 30-32).
#include "app.hpp"

#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>

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
        st_.stage = 30; // full-resolution cache, then back to 6
        break;
    }
    case 30:
    case 31:
    case 32:
        return selfTestFullRes();
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

// Stages 30-32: the full-resolution cache of B. 30: a cache built in a
// temporary folder by the overview pass (full dates read once for both),
// compared value by value (bits) with GDAL: the overview it fills, pixel
// series (block corners and edges), windows and subsampled windows. Then B's
// own cache, built through the session: 31 waits for it and asks for a series
// and an ROI, 32 checks that both came from it and equal the source.
int App::selfTestFullRes() {
    auto fail = [&](const std::string& what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what.c_str());
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    const std::shared_ptr<const CubeInfo> info = s_->info;
    const int W = info->width, H = info->height, T = info->T();
    auto same = [](const float* a, const float* b, size_t n) { return std::memcmp(a, b, n * sizeof(float)) == 0; };
    switch (st_.stage) {
    case 30: {
        namespace fs = std::filesystem;
        std::error_code ec;
        const fs::path dir = fs::temp_directory_path(ec) / ("janus-selftest-" + std::to_string(uint64_t(now() * 1e6)));
        fs::create_directories(dir, ec);
        int series = 0, windows = 0;
        uint64_t bytes = 0;
        {
            FullResCache c(info, dir.u8string());
            if (!c.start(2, 1ull << 40)) return fail("could not start a full-resolution cache: " + c.error());
            CubeReader r(info);
            const int ow = 97, oh = 61; // subsampled like an overview, odd factors
            std::vector<float> ov(size_t(ow) * oh), ref(ov.size());
            for (int t = 0; t < T; ++t) {
                if (c.buildWithOverview(t, ov.data(), ow, oh) != 1) return fail("the overview pass should build every date");
                r.readWindow(t, 0, 0, W, H, ref.data(), ow, oh);
                if (!same(ov.data(), ref.data(), ov.size())) return fail("the overview taken from full dates differs from GDAL's");
            }
            if (!c.complete() || c.datesWithOverview() != T) return fail("the cache should be complete");
            bytes = c.bytes();
            const int px[][2] = {{0, 0}, {W - 1, H - 1}, {63, 63}, {64, 64}, {63, 64}, {W - 1, 0}, {0, H - 1},
                                 {128, 127}, {W / 2, H / 2}, {W - 2, 129}};
            std::vector<float> a(T), b(T);
            std::vector<char> got(T, 0);
            for (const auto& p : px) {
                if (c.readSeries(p[0], p[1], a.data(), got.data()) != T) return fail("every date should be cached");
                for (int t = 0; t < T; ++t) r.readPixel(t, p[0], p[1], b[t]);
                if (!same(a.data(), b.data(), size_t(T))) return fail("a series from the cache differs from the source");
                ++series;
            }
            const int win[][6] = {{0, 0, W, H, W, H},          {10, 7, 200, 150, 200, 150}, {5, 3, W - 9, H - 5, 97, 61},
                                  {63, 63, 2, 2, 2, 2},        {100, 50, 150, 120, 40, 33}, {0, 0, W, H, 256 / 4, 33},
                                  {W - 70, H - 70, 70, 70, 70, 70}, {1, 1, W - 1, H - 1, W / 3, H / 7}};
            for (const auto& w : win) {
                std::vector<float> x(size_t(w[4]) * w[5]), y(x.size());
                for (int t = 0; t < T; ++t) {
                    if (!c.readWindow(t, w[0], w[1], w[2], w[3], x.data(), w[4], w[5])) return fail("window not cached");
                    r.readWindow(t, w[0], w[1], w[2], w[3], y.data(), w[4], w[5]);
                    if (!same(x.data(), y.data(), x.size())) {
                        char msg[128];
                        std::snprintf(msg, sizeof(msg), "window (%d, %d, %d x %d -> %d x %d) differs from the source",
                                      w[0], w[1], w[2], w[3], w[4], w[5]);
                        return fail(msg);
                    }
                }
                ++windows;
            }
        }
        fs::remove_all(dir, ec);
        std::printf("    %d dates read once for the overview and the cache (%s, %.0f KB, raw %.0f KB); %d series and "
                    "%d windows x %d dates equal to GDAL's\n",
                    T, "lossless", bytes / 1024.0, double(W) * H * T * 4 / 1024, series, windows, T);
        if (!s_->buildFullRes()) return fail("B's cache could not start: " + s_->fullRes->error());
        showPerf_ = true; // its section of the Performance panel is drawn while the cache builds and is read
        next("full-resolution cache (temporary) equal to the source");
        break;
    }
    case 31: {
        if (!s_->fullRes->complete()) break;
        st_.hits = s_->fullRes->seriesHits();
        hover_ = SeriesView{};
        hover_.x = 77;
        hover_.y = 131;
        hover_.values = approxSeries(77, 131);
        hover_.request = s_->requestSeries(77, 131, false);
        clearRoi();
        roi_ = s_->startRoi(13, 21, 163, 191);
        roiSeen_ = -1;
        next("B's own cache complete; series and ROI requested");
        break;
    }
    case 32: {
        if (!hover_.exact || !roi_ || roi_->done.load() < T) break;
        CubeReader r(info);
        std::vector<float> v(T);
        for (int t = 0; t < T; ++t) r.readPixel(t, hover_.x, hover_.y, v[t]);
        if (s_->fullRes->seriesHits() == st_.hits) return fail("the series should come from the cache");
        if (!same(hover_.values.data(), v.data(), size_t(T))) return fail("the series differs from the source");
        if (roi_->cachedDates.load() != T) return fail("the ROI should come from the cache");
        for (int t = 0; t < T; ++t) {
            std::vector<float> buf(size_t(roi_->bw) * roi_->bh);
            r.readWindow(t, roi_->x0, roi_->y0, roi_->x1 - roi_->x0, roi_->y1 - roi_->y0, buf.data(), roi_->bw, roi_->bh);
            const SampleStats a = computeSampleStats(buf), &b = roi_->perT[t];
            if (a.n != b.n || a.mean != b.mean || a.p10 != b.p10 || a.p50 != b.p50 || a.p90 != b.p90)
                return fail("the ROI statistics differ from the source's");
        }
        std::printf("    series from the cache in %.2f ms, ROI %d x %d x %d dates in %.1f ms\n",
                    s_->fullRes->lastSeriesMs(), roi_->x1 - roi_->x0, roi_->y1 - roi_->y0, T, roi_->ms.load());
        clearRoi();
        showPerf_ = false;
        next("series and ROI through B's cache equal the source");
        st_.stage = 6;
        break;
    }
    }
    return -1;
}
