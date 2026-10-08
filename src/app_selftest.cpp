// `jn --selftest-ui A B`: drives the layers workflow in a hidden window, one
// step per frame, without touching the mouse or keyboard. Opens A, adds B as a
// layer, checks the georeferenced alignment, the series of both layers under a
// cursor and a pin, reads back map pixels (layer drawn, layer hidden), switches
// the active layer and closes one; with both layers shown it also exports the
// map (PNG) and the values and the view (GeoTIFF) and checks the files (see
// selfTestExports). Optional inputs:
//   C  one file per date with several bands and a quality band: opens it and
//      reopens it in place as a normalized difference with the QA mask, then
//      checks the difference and largest-drop maps (GPU against the CPU);
//   D  categorical series with a colour table and class names (file colours);
//   E  categorical series without them (detected from its values).
//   F  B reprojected to another CRS (nearest): added as a layer over A and B,
//      it must be drawn where B is, give B's series and B's ROI (stages 40-45).
// With A and B open it also checks the swipe and the space-time transect, and
// then the basemap from a local tile pyramid (stages 50-56, no network).
// Between the series and the map checks, the full-resolution cache of B must
// give exactly the source values (stages 30-32).
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <functional>

#include <gdal_priv.h>
#include <ogr_spatialref.h>
#include <sstream>
#include <thread>

#include <imgui_internal.h>
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
        ImGui::GetIO().IniFilename = nullptr; // the user's layout was read; the test's settings are not saved
        bm_ = BasemapUi{};                    // no basemap, whatever the user's setting
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
        if (const char* e = selfTestExports()) return fail(e);
        std::printf("[%5.1f s] exports: PNG and GeoTIFFs match the map and the source values\n", now() - st_.t0);
        if (const char* err = selfTestCompare()) return fail(err);
        next("layer drawing and visibility, exports, swipe, transect");
        st_.stage = in.size() > 5 ? 40 : 50; // F: reprojection (40-45); the basemap (50-56); then back to 7
        break;
    }
    case 40:
    case 41:
    case 42:
    case 43:
    case 44:
    case 45:
        return selfTestReproject(in[5]);
    case 50:
    case 51:
    case 52:
    case 53:
    case 54:
    case 55:
    case 56:
        return selfTestBasemap();
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

// Exports, in stage 6 (both layers on a 400 x 300 view, B active): files written
// by the same background jobs as the File menu, read back with GDAL and compared
// with the map's pixels and the source values. Returns nullptr when they pass.
const char* App::selfTestExports() {
    namespace fs = std::filesystem;
    std::error_code ec;
    const fs::path dir = fs::temp_directory_path() /
                         ("janus-selftest-export-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    fs::create_directories(dir, ec);
    struct Cleanup {
        fs::path dir;
        ~Cleanup() {
            std::error_code e;
            fs::remove_all(dir, e);
        }
    } cleanup{dir};
    auto file = [&](const char* name) { return (dir / name).u8string(); };
    auto wait = [&](const std::shared_ptr<ExportJob>& j, const char* what) {
        if (!j) return false;
        j->done.wait();
        std::lock_guard<std::mutex> lk(j->m);
        std::printf("    %-40s %6.0f ms %8.1f KB %s\n", what, j->seconds * 1000,
                    double(fs::file_size(fs::u8path(j->path), ec)) / 1024, j->error.c_str());
        return j->state == ExportJob::State::Done;
    };
    struct Image {
        int w = 0, h = 0, bands = 0;
        std::vector<unsigned char> px; // pixel-interleaved
        double gt[6] = {0, 1, 0, 0, 0, 1};
    };
    auto readImage = [](const std::string& path, Image& im) {
        GDALDataset* ds = GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds) return false;
        im.w = ds->GetRasterXSize();
        im.h = ds->GetRasterYSize();
        im.bands = ds->GetRasterCount();
        im.px.resize(size_t(im.w) * im.h * im.bands);
        ds->GetGeoTransform(im.gt);
        const bool ok = ds->RasterIO(GF_Read, 0, 0, im.w, im.h, im.px.data(), im.w, im.h, GDT_Byte, im.bands, nullptr,
                                     im.bands, GSpacing(im.w) * im.bands, 1, nullptr) == CE_None;
        GDALClose(ds);
        return ok;
    };
    // Float32 values of a GeoTIFF == the layer's values at date t_ in the window.
    auto checkValues = [&](const std::string& path, const int win[4]) -> const char* {
        const CubeInfo& info = *s_->info;
        GDALDataset* ds = GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds) return "the GeoTIFF cannot be opened";
        const int w = win[2] - win[0], h = win[3] - win[1];
        double gt[6];
        int hasNd = 0;
        const double nd = ds->GetRasterBand(1)->GetNoDataValue(&hasNd);
        const bool sized = ds->GetRasterXSize() == w && ds->GetRasterYSize() == h && ds->GetRasterCount() == 1 &&
                           ds->GetRasterBand(1)->GetRasterDataType() == GDT_Float32;
        const bool geo = ds->GetGeoTransform(gt) == CE_None;
        const std::string wkt = ds->GetProjectionRef() ? ds->GetProjectionRef() : "";
        std::vector<float> got(size_t(w) * h), want(size_t(w) * h);
        bool read = sized && ds->GetRasterBand(1)->RasterIO(GF_Read, 0, 0, w, h, got.data(), w, h, GDT_Float32, 0, 0,
                                                            nullptr) == CE_None;
        GDALClose(ds);
        if (!read) return "the GeoTIFF should be Float32, the size of the window";
        if (!hasNd || !std::isnan(nd)) return "the GeoTIFF's no data should be NaN";
        const auto& g = info.geoTransform;
        const double x0 = g[0] + win[0] * g[1] + win[1] * g[2], y0 = g[3] + win[0] * g[4] + win[1] * g[5];
        std::printf("      %d x %d px at (%d, %d), origin %.3f %.3f, pixel %.3f, CRS %s\n", w, h, win[0], win[1], gt[0],
                    gt[3], gt[1], wkt.empty() ? "none" : wkt.substr(0, 24).c_str());
        if (info.hasGeoTransform && (!geo || std::fabs(gt[0] - x0) > 1e-6 || std::fabs(gt[3] - y0) > 1e-6 ||
                                     gt[1] != g[1] || gt[5] != g[5]))
            return "the GeoTIFF should be on the layer's grid";
        if (!info.crsName.empty() && wkt.empty()) return "the GeoTIFF should carry the layer's CRS";
        CubeReader reader(s_->info);
        if (!reader.readWindow(t_, win[0], win[1], w, h, want.data(), w, h)) return "the source cannot be read";
        size_t diff = 0, nan = 0;
        for (size_t i = 0; i < got.size(); ++i) {
            nan += std::isnan(want[i]);
            if (!(got[i] == want[i] || (std::isnan(got[i]) && std::isnan(want[i])))) ++diff;
        }
        if (diff) return "the GeoTIFF should hold the layer's values";
        std::printf("      values identical to the source (%zu no data)\n", nan);
        return nullptr;
    };

    mapPixelScale_ = 1.0f;
    canvasSize_ = ImVec2(400, 300);
    fitView(canvasSize_);
    renderMap(400, 300);
    std::vector<unsigned char> screen;
    int sw = 0, sh = 0;
    gpu_.readMap(screen, sw, sh);
    unsigned char c[4];
    gpu_.readMapPixel(200, 150, c);
    if (sw != 400 || sh != 300 || std::memcmp(c, &screen[(size_t(150) * 400 + 200) * 4], 4) != 0)
        return "readMap should agree with readMapPixel";

    // PNG at 1x, the map's background, no label or legend: the map's pixels.
    PngOptions o;
    o.label = o.legend = false;
    Image im;
    if (!wait(exportPng(file("map.png"), o), "map as PNG, 1x") || !readImage(file("map.png"), im))
        return "the PNG export failed";
    if (im.w != 400 || im.h != 300 || im.bands != 3) return "the PNG should be 400 x 300, RGB";
    size_t diff = 0;
    for (size_t i = 0; i < size_t(im.w) * im.h; ++i)
        for (int k = 0; k < 3; ++k) diff += im.px[i * 3 + k] != screen[i * 4 + k];
    if (diff) return "the PNG should hold the map's pixels";

    // 2x with label, legend and pins: rendered at 800 x 600 (not enlarged), marks over it.
    o.scale = 2;
    o.label = o.legend = o.marks = true;
    std::vector<unsigned char> hi;
    int hw = 0, hh = 0;
    renderExport(2, false, nullptr, hi, hw, hh);
    if (!wait(exportPng(file("map2x.png"), o), "map as PNG, 2x, label, legend, pins") ||
        !readImage(file("map2x.png"), im))
        return "the 2x PNG export failed";
    if (im.w != 800 || im.h != 600 || hw != 800 || hh != 600) return "the 2x PNG should be 800 x 600";
    auto same = [&](int x, int y) {
        const size_t i = size_t(y) * im.w + x;
        return im.px[i * 3] == hi[i * 4] && im.px[i * 3 + 1] == hi[i * 4 + 1] && im.px[i * 3 + 2] == hi[i * 4 + 2];
    };
    if (!same(600, 150)) return "the 2x PNG should hold the 2x render away from the marks";
    if (same(24, 20)) return "the 2x PNG should have the label";
    if (same(30, 600 - 30)) return "the 2x PNG should have the legend";

    // Transparent background: alpha 0 exactly where the map shows its background.
    o = PngOptions{};
    o.background = 2;
    o.label = o.legend = false;
    if (!wait(exportPng(file("map_alpha.png"), o), "map as PNG, transparent") || !readImage(file("map_alpha.png"), im))
        return "the transparent PNG export failed";
    if (im.bands != 4) return "the transparent PNG should be RGBA";
    size_t clear = 0, opaque = 0;
    const unsigned char* bg = nullptr;
    for (size_t i = 0; i < size_t(im.w) * im.h; ++i) {
        const unsigned char* p = &im.px[i * 4];
        const unsigned char* s = &screen[i * 4];
        if (p[3] == 0) {
            if (!bg) bg = s;
            if (std::memcmp(bg, s, 3) != 0) return "transparent pixels should be the map's background";
            ++clear;
        } else if (p[3] == 255) {
            if (std::memcmp(p, s, 3) != 0) return "opaque pixels should keep the map's colours";
            ++opaque;
        } else {
            return "opaque layers should give alpha 0 or 255";
        }
    }
    std::printf("      %zu transparent, %zu opaque pixels\n", clear, opaque);
    if (!clear || !opaque) return "the transparent PNG should have both";

    // Values of the active layer (B) at the date: whole image, then the visible area zoomed in.
    const CubeInfo& info = *s_->info;
    int win[4] = {0, 0, info.width, info.height};
    if (!wait(exportValues(file("values.tif"), true), "values as GeoTIFF, whole image")) return "the values export failed";
    if (const char* e = checkValues(file("values.tif"), win)) return e;
    offset_ = ImVec2(200 - (200 - offset_.x) * 4, 150 - (150 - offset_.y) * 4); // zoom x4 around the centre
    scale_ *= 4;
    if (!visibleWindow(win)) return "the zoomed view should show part of the layer";
    if (!wait(exportValues(file("visible.tif"), false), "values as GeoTIFF, visible area")) return "the values export failed";
    if (const char* e = checkValues(file("visible.tif"), win)) return e;
    if (win[2] - win[0] >= info.width) return "the visible area should be smaller than the image";
    fitView(canvasSize_);

    // Rendered view: RGBA as shown, on a grid derived from the view.
    if (!wait(exportView(file("view.tif"), 1), "rendered view as GeoTIFF, 1x") || !readImage(file("view.tif"), im))
        return "the view export failed";
    if (im.w != 400 || im.h != 300 || im.bands != 4) return "the view GeoTIFF should be 400 x 300, RGBA";
    for (size_t i = 0; i < size_t(im.w) * im.h; ++i)
        if (im.px[i * 4 + 3] == 255 && std::memcmp(&im.px[i * 4], &screen[i * 4], 3) != 0)
            return "the view GeoTIFF should hold the map's colours";
    double gx = 0, gy = 0;
    info.pixelToGeo((200.5 - offset_.x) / scale_, (150.5 - offset_.y) / scale_, gx, gy);
    const double vx = im.gt[0] + 200.5 * im.gt[1] + 150.5 * im.gt[2], vy = im.gt[3] + 200.5 * im.gt[4] + 150.5 * im.gt[5];
    std::printf("      view pixel %.4f (layer pixel / zoom), centre at %.3f %.3f (layer: %.3f %.3f)\n", im.gt[1], vx, vy,
                gx, gy);
    if (std::fabs(vx - gx) > 1e-6 * std::max(1.0, std::fabs(gx)) || std::fabs(vy - gy) > 1e-6 * std::max(1.0, std::fabs(gy)))
        return "the view GeoTIFF should be georeferenced like the map";

    // A Zeit result is a GeoTIFF already: saved as a copy.
    ResultLayer r;
    r.name = "Test result";
    r.path = layers_[0].session->info->layers[0].path;
    if (!wait(exportResult(r, file("result.tif")), "result as GeoTIFF (copy)")) return "the result export failed";
    if (fs::file_size(fs::u8path(r.path), ec) != fs::file_size(fs::u8path(file("result.tif")), ec))
        return "the saved result should be a copy of the file";

    exports_.clear();
    showExports_ = false;
    mapDirty_ = true;
    return nullptr;
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

// Stages 40-45: F = B reprojected from UTM 23S to UTM 22S (nearest, 15 m),
// added over A and B. F is opened with a small overview budget (~1:2), so a
// map panel of it zoomed in reads detail tiles. 40 adds F; 41 makes B active
// again (F reprojected on its grid), checks the warp grid against PROJ and asks
// for the cursor, pin and ROI series of every layer; 42 compares F's with B's;
// 43 the main map (F's overview through the grid against the exact
// transformation); 44 a map panel of F at full resolution against B's own
// pixels, and the cost; 45 F active (B reprojected on its grid), then closed.
int App::selfTestReproject(const std::string& f) {
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    auto rgbAt = [](const std::vector<unsigned char>& img, int w, int x, int y) { return &img[(size_t(y) * w + x) * 4]; };
    const int W = 400, H = 300;
    switch (st_.stage) {
    case 40:
        st_.budget = settings_.overviewBudgetBytes;
        settings_.overviewBudgetBytes = 6 << 20; // F's overview at ~1:2: zoomed in, its tiles are drawn
        openInputs({f}, true);
        next("add F (B reprojected to UTM 22S) as a layer");
        break;
    case 41: {
        if (opening_.valid() || layers_.size() != 3 || !layers_[2].session->overview.complete() ||
            !layers_[2].session->gpu.statsValid)
            break;
        settings_.overviewBudgetBytes = st_.budget;
        if (active_ != 2 || !layers_[1].reproj || !layers_[1].aligned) return fail("F should be active, B reprojected on its grid");
        const int pinX = pins_.empty() ? -1 : pins_[0].x, pinY = pins_.empty() ? -1 : pins_[0].y;
        setActive(1);
        const SeriesLayer& F = layers_[2];
        if (!F.aligned || !F.reproj || !F.reproj->tex) return fail("F should be reprojected onto B's grid");
        const Reprojection& R = *F.reproj;
        // The grid (what the shader interpolates) against the exact transformation.
        double maxErr = 0, roundTrip = 0;
        for (int k = 0; k < 4000; ++k) {
            const double x = 300 * std::fmod(k * 0.6180339887, 1.0), y = 200 * std::fmod(k * 0.7548776662, 1.0);
            double ex, ey, gx, gy, bx, by;
            if (!R.toLayer(x, y, ex, ey) || !R.gridAt(x, y, gx, gy) || !R.toActive(ex, ey, bx, by))
                return fail("points of B should transform to F and back");
            maxErr = std::max(maxErr, std::hypot(gx - ex, gy - ey));
            roundTrip = std::max(roundTrip, std::hypot(bx - x, by - y));
        }
        std::printf("    F on B's grid: %s, grid %d x %d over map x %.0f..%.0f y %.0f..%.0f, made in %.1f ms;\n"
                    "    grid vs PROJ: %.2g F px at the cells' centres, %.2g at 4000 points; round trip %.2g px\n",
                    R.crsText.c_str(), R.gridW, R.gridH, R.domain[0], R.domain[2], R.domain[1], R.domain[3], R.buildMs,
                    R.gridError, maxErr, roundTrip);
        std::printf("    F: %d x %d px, overview 1:%.2f; pin carried B (%d, %d) -> F -> B (%d, %d)\n",
                    F.session->info->width, F.session->info->height, F.session->overview.factor, pinX, pinY,
                    pins_.empty() ? -1 : pins_[0].x, pins_.empty() ? -1 : pins_[0].y);
        if (maxErr > 0.01 || roundTrip > 1e-6) return fail("the warp grid should be within 0.01 px of PROJ");
        // At the scale of a Landsat scene (UTM 23S, 7441 x 7317 px of 30 m): layers in
        // UTM 22S, in geographic coordinates and on a grid rotated by 10 degrees.
        {
            auto wkt = [](int epsg) {
                OGRSpatialReference s;
                s.importFromEPSG(epsg);
                char* w = nullptr;
                const char* o[] = {"FORMAT=WKT2_2019", nullptr};
                s.exportToWkt(&w, o);
                std::string r = w ? w : "";
                CPLFree(w);
                return r;
            };
            auto cube = [](uint64_t id, int w, int h, std::array<double, 6> gt, std::string crs, const char* name) {
                CubeInfo c;
                c.id = id;
                c.width = w;
                c.height = h;
                c.hasGeoTransform = true;
                c.geoTransform = gt;
                c.crsWkt = std::move(crs);
                c.crsAuthority = name;
                return c;
            };
            const double k = 3.14159265358979 / 180 * 10;
            const CubeInfo scene = cube(~1ull, 7441, 7317, {400000, 30, 0, 7600000, 0, -30}, wkt(32723), "EPSG:32723");
            const CubeInfo others[] = {
                cube(~2ull, 12000, 12000, {950000, 30, 0, 7700000, 0, -30}, wkt(32722), "EPSG:32722"),
                cube(~3ull, 14000, 14000, {-46.5, 0.00025, 0, -21.0, 0, -0.00025}, wkt(4326), "EPSG:4326"),
                cube(~4ull, 10000, 10000, {380000, 30 * std::cos(k), 30 * std::sin(k), 7620000, 30 * std::sin(k), -30 * std::cos(k)},
                     wkt(32723), "EPSG:32723, rotated 10 degrees"),
                cube(~5ull, 36000, 18000, {-180, 0.01, 0, 90, 0, -0.01}, wkt(4326), "EPSG:4326, the whole world")};
            for (const CubeInfo& o : others) {
                std::string why;
                const std::unique_ptr<Reprojection> S = Reprojection::create(scene, o, why);
                if (!S) return fail(("a Landsat-sized layer could not be reprojected: " + why).c_str());
                double err = 0;
                for (int i = 0; i < 4000; ++i) {
                    const double x = 7441 * std::fmod(i * 0.6180339887, 1.0), y = 7317 * std::fmod(i * 0.7548776662, 1.0);
                    double ex, ey, gx, gy;
                    if (S->toLayer(x, y, ex, ey) && S->gridAt(x, y, gx, gy)) err = std::max(err, std::hypot(gx - ex, gy - ey));
                }
                std::printf("    Landsat-sized scene, layer in %s: grid %d x %d over x %.0f..%.0f y %.0f..%.0f\n"
                            "      in %.1f ms, error %.2g px (cells' centres), %.2g px (4000 points)\n",
                            o.crsAuthority.c_str(), S->gridW, S->gridH, S->domain[0], S->domain[2], S->domain[1],
                            S->domain[3], S->buildMs, S->gridError, err);
                if (err > 0.05) return fail("the warp grid should be within 0.05 px at the scale of a scene");
                if (S->domain[0] > 0 || S->domain[1] > 0 || S->domain[2] < 7441 || S->domain[3] < 7317)
                    return fail("the grid should cover the scene (every layer covers it)");
            }
        }
        if (pins_.empty() || pins_[0].x != 50 || pins_[0].y != 100) return fail("the pin should come back to B's (50, 100)");
        if (F.session->overview.factor < 1.5) return fail("F's overview should be coarser than F (tiles)");
        // Cursor (the pin is there too) and an ROI, on every visible layer.
        chartLayers_ = 1;
        hover_ = SeriesView{};
        hover_.x = 50;
        hover_.y = 100;
        hover_.values = approxSeries(50, 100);
        hover_.request = s_->requestSeries(50, 100, true);
        updateOtherHover(50, 100);
        requestOtherSeries(true);
        clearRoi();
        const int r[4] = {13, 21, 163, 191};
        setRoiRect(r);
        roi_ = s_->startRoi(r[0], r[1], r[2], r[3]);
        roiSeen_ = -1;
        next("B active again, F reprojected on its grid; cursor, pin and ROI on every layer");
        break;
    }
    case 42: {
        const SeriesLayer& A = layers_[0];
        const SeriesLayer& F = layers_[2];
        const int T = s_->info->T();
        if (!hover_.exact || !F.hover.exact || pins_.empty() || !pins_[0].exact || F.pins.empty() || !F.pins[0].exact ||
            roiSeen_ != T || !F.roi || F.roiSeen != F.session->info->T() || !A.roi || A.roiSeen != T)
            break;
        double ex, ey;
        F.reproj->toLayer(50.5, 100.5, ex, ey);
        std::printf("    cursor B (50, 100) -> F (%d, %d) (PROJ: %.3f, %.3f): %.4f at %s in both\n", F.hover.x, F.hover.y,
                    ex, ey, F.hover.values[0], F.session->info->layers[0].label.c_str());
        if (F.hover.x != int(ex) || F.hover.y != int(ey)) return fail("F's cursor pixel should be PROJ's");
        if (F.hover.values != hover_.values) return fail("F's series under the cursor should be B's");
        if (F.pins[0].values != pins_[0].values) return fail("F's series at the pin should be B's");
        double dMean = 0, dP = 0, dA = 0;
        int nB = 0, nF = 0;
        for (int t = 0; t < T; ++t) {
            dMean = std::max(dMean, double(std::fabs(F.roiMean[t] - roiMean_[t])));
            dP = std::max({dP, double(std::fabs(F.roiP10[t] - roiP10_[t])), double(std::fabs(F.roiP90[t] - roiP90_[t]))});
            dA = std::max(dA, double(std::fabs(A.roiMean[t] - roiMean_[t] - 0.5f))); // A = B + 0.5 there
            nB = roi_->perT[t].n;
            nF = F.roi->perT[t].n;
            if (A.roi->perT[t].n != nB) return fail("A's ROI should have B's pixels (same grid lines)");
        }
        std::printf("    ROI 150 x 170 px of B: %d px on B, %d on A (|A - B - 0.5| %.2g), %d on F (15 m); F vs B:\n"
                    "    mean within %.2g, p10/p90 within %.2g (B's mean %.4f .. %.4f)\n",
                    nB, A.roi->perT[0].n, dA, nF, dMean, dP, roiMean_[0], roiMean_[T - 1]);
        if (dA > 1e-4) return fail("A's ROI should be B's + 0.5");
        if (nF < 3 * nB || nF > 5 * nB) return fail("F's ROI should have ~4 pixels per pixel of B");
        if (dMean > 0.01 || dP > 0.02) return fail("F's ROI should agree with B's");
        next("F's cursor and pin series equal B's; the ROI on A, B and F agrees");
        break;
    }
    case 43: {
        SeriesLayer& F = layers_[2];
        const float lo = 1.4f, hi = 6.0f; // the same colours for B and F
        mode_ = ModeValue;
        range_[ModeValue] = Range{lo, hi, true, 0};
        F.disp.mode = ModeValue;
        F.disp.range[ModeValue] = Range{lo, hi, true, 0};
        F.disp.cmap[ModeValue] = cmap_[ModeValue];
        layers_[0].visible = layers_[1].visible = false; // F alone
        canvasSize_ = ImVec2(float(W), float(H));
        fitView(canvasSize_);
        renderMap(W, H);
        std::vector<unsigned char> img;
        int w = 0, h = 0;
        gpu_.readMap(img, w, h);
        // Each sampled target pixel against F's overview texel PROJ puts there.
        const Overview& ov = F.session->overview;
        const CubeInfo& fi = *F.session->info;
        int checked = 0, edge = 0, bad = 0;
        for (int sy = 3; sy < H; sy += 5)
            for (int sx = 3; sx < W; sx += 5) {
                const double mx = (sx + 0.5 - offset_.x) / scale_, my = (sy + 0.5 - offset_.y) / scale_;
                double lx, ly;
                if (!F.reproj->toLayer(mx, my, lx, ly)) continue;
                const double ox = lx / fi.width * ov.w, oy = ly / fi.height * ov.h;
                if (!(ox >= 0 && oy >= 0 && ox < ov.w && oy < ov.h)) continue;
                const double fx = ox - std::floor(ox), fy = oy - std::floor(oy);
                if (fx < 0.02 || fx > 0.98 || fy < 0.02 || fy > 0.98) { // on a texel's edge: either side
                    ++edge;
                    continue;
                }
                const float v = ov.at(F.disp.t, int(ox), int(oy));
                if (std::isnan(v)) continue;
                const ImVec4 c = ImPlot::SampleColormap(std::clamp((v - lo) / (hi - lo), 0.0f, 1.0f), cmap_[ModeValue]);
                const unsigned char* p = rgbAt(img, w, sx, sy);
                const int e[3] = {int(c.x * 255 + 0.5f), int(c.y * 255 + 0.5f), int(c.z * 255 + 0.5f)};
                if (std::abs(p[0] - e[0]) > 3 || std::abs(p[1] - e[1]) > 3 || std::abs(p[2] - e[2]) > 3) {
                    if (++bad <= 3)
                        std::printf("    (%d, %d): F's overview %.4g -> rgb(%d, %d, %d), map rgb(%d, %d, %d)\n", sx, sy, v,
                                    e[0], e[1], e[2], p[0], p[1], p[2]);
                }
                ++checked;
            }
        std::printf("    main map, F alone (overview through the grid): %d pixels match PROJ's texel, %d wrong "
                    "(%d on texel edges skipped)\n", checked - bad, bad, edge);
        if (bad || checked < 2000) return fail("F's overview should be drawn where PROJ puts it");
        st_.frames = 0; // next: zoomed in on B's pixels 100..199 x 50..124 (4 target pixels per pixel of B)
        next("main map: F's overview through the warp grid where PROJ puts it");
        break;
    }
    case 44: {
        SeriesLayer& F = layers_[2];
        // F's tiles, as its map panel asks for them each frame (the frame's own
        // map may have fitted the view to its canvas in between: set it again).
        canvasSize_ = ImVec2(float(W), float(H));
        scale_ = 4;
        offset_ = ImVec2(-100 * 4.0f, -50 * 4.0f);
        const ViewRect v = layerView(F, -offset_.x / scale_, -offset_.y / scale_, (W - offset_.x) / scale_,
                                     (H - offset_.y) / scale_, scale_);
        const int level = F.session->tiles->update(F.disp.t, v, -1);
        if (level != 0) return fail("F zoomed in should read full-resolution tiles");
        if (++st_.frames < 3 || F.session->tiles->inflight() > 0) break;
        MapView pv;
        pv.id = 98;
        pv.cube = F.session->info->id;
        pv.detailLevel = level;
        renderView(pv, W, H, 1.0f, ImVec2(0, 0));
        std::vector<unsigned char> fImg, bImg, ovImg;
        int w = 0, h = 0;
        gpu_.readMap(fImg, w, h, pv.id);
        layers_[1].visible = true; // B alone (its overview is B itself)
        F.visible = false;
        renderMap(W, H);
        gpu_.readMap(bImg, w, h);
        F.visible = true; // F's overview alone, for comparison
        layers_[1].visible = false;
        renderMap(W, H);
        gpu_.readMap(ovImg, w, h);
        int same = 0, diff = 0, ovDiff = 0;
        for (int by = 50; by < 125; ++by)
            for (int bx = 100; bx < 200; ++bx) {
                const int sx = (bx - 100) * 4 + 2, sy = (by - 50) * 4 + 2;
                const bool eq = std::memcmp(rgbAt(fImg, w, sx, sy), rgbAt(bImg, w, sx, sy), 3) == 0;
                same += eq;
                diff += !eq;
                ovDiff += std::memcmp(rgbAt(ovImg, w, sx, sy), rgbAt(bImg, w, sx, sy), 3) != 0;
            }
        std::printf("    map panel of F at full resolution (%d tiles on the GPU): %d of %d pixels of B identical,\n"
                    "    %d differ (F's overview alone: %d differ)\n",
                    F.session->tiles->gpuTiles(), same, same + diff, diff, ovDiff);
        if (diff) return fail("F drawn at full resolution should land on B's pixels");
        // Cost: renders followed by a read back (which waits for the GPU).
        auto ms = [&](const std::function<void()>& render, int slot) {
            const double t0 = now();
            unsigned char px[4];
            for (int k = 0; k < 40; ++k) render();
            gpu_.readMapPixel(0, 0, px, slot);
            return (now() - t0) * 1000 / 40;
        };
        fitView(canvasSize_);
        layers_[1].visible = true;
        F.visible = false;
        const double alone = ms([&] { renderMap(W, H); }, 0);
        F.visible = true;
        const double both = ms([&] { renderMap(W, H); }, 0);
        scale_ = 4;
        offset_ = ImVec2(-100 * 4.0f, -50 * 4.0f);
        const double panel = ms([&] { renderView(pv, W, H, 1.0f, ImVec2(0, 0)); }, pv.id);
        const double t0 = now();
        for (int k = 0; k < 100; ++k) (void)layerView(F, 100, 50, 200, 125, 4.0);
        const double viewUs = (now() - t0) * 1e6 / 100;
        gpu_.releaseMap(pv.id);
        std::printf("    cost per render (400 x 300, incl. a read back): B alone %.3f ms, B + F reprojected %.3f ms,\n"
                    "    F's panel with its tiles %.3f ms; F's view in its pixels (tile requests) %.1f us\n",
                    alone, both, panel, viewUs);
        fitView(canvasSize_);
        next("F at full resolution lands on B's pixels");
        break;
    }
    case 45: {
        SeriesLayer& F = layers_[2];
        // F active: the view and the pin carried over through the same transformation.
        layers_[0].visible = layers_[1].visible = true;
        scale_ *= 3;
        viewTouched_ = true;
        const double mx = (canvasSize_.x * 0.5 - offset_.x) / scale_, my = (canvasSize_.y * 0.5 - offset_.y) / scale_;
        double ex, ey, px, py;
        F.reproj->toLayer(mx, my, ex, ey);
        F.reproj->toLayer(50.5, 100.5, px, py);
        // The transect left on by selfTestCompare (B's row 100, columns 10 to 290) follows too.
        if (!tr_.on || tr_.cube != s_->info->id || std::fabs(tr_.a.x - 10.5f) > 0.01f || std::fabs(tr_.a.y - 100.5f) > 0.01f)
            return fail("the transect should still be on B's row 100 (after F active and back)");
        double ta[2], tb[2];
        F.reproj->toLayer(tr_.a.x, tr_.a.y, ta[0], ta[1]);
        F.reproj->toLayer(tr_.b.x, tr_.b.y, tb[0], tb[1]);
        setActive(2);
        pumpTransect();
        std::printf("    transect on F: (%.2f, %.2f) - (%.2f, %.2f), PROJ (%.2f, %.2f) - (%.2f, %.2f), %d samples\n", tr_.a.x,
                    tr_.a.y, tr_.b.x, tr_.b.y, ta[0], ta[1], tb[0], tb[1], tr_.n);
        if (!tr_.on || std::hypot(tr_.a.x - ta[0], tr_.a.y - ta[1]) > 0.01 || std::hypot(tr_.b.x - tb[0], tr_.b.y - tb[1]) > 0.01)
            return fail("the transect should follow F's grid");
        const double nx = (canvasSize_.x * 0.5 - offset_.x) / scale_, ny = (canvasSize_.y * 0.5 - offset_.y) / scale_;
        std::printf("    F active: view centre B (%.2f, %.2f) -> F (%.2f, %.2f) (PROJ %.2f, %.2f); pin at F (%d, %d);\n"
                    "    B on F's grid: %s, grid %d x %d\n",
                    mx, my, nx, ny, ex, ey, pins_.empty() ? -1 : pins_[0].x, pins_.empty() ? -1 : pins_[0].y,
                    layers_[1].reproj ? layers_[1].reproj->crsText.c_str() : "-",
                    layers_[1].reproj ? layers_[1].reproj->gridW : 0, layers_[1].reproj ? layers_[1].reproj->gridH : 0);
        if (std::hypot(nx - ex, ny - ey) > 0.01) return fail("the view should stay on the same place");
        if (pins_.empty() || pins_[0].x != int(px) || pins_[0].y != int(py)) return fail("the pin should move to F's grid");
        if (!layers_[1].reproj || !layers_[1].aligned) return fail("B should be reprojected on F's grid");
        setActive(1);
        if (pins_.empty() || pins_[0].x != 50 || pins_[0].y != 100) return fail("the pin should come back to B's grid");
        removeLayer(2);
        range_[ModeValue].manual = false;
        range_[ModeValue].key = ~0ull;
        viewTouched_ = false;
        clearRoi();
        if (layers_.size() != 2 || active_ != 1) return fail("closing F should leave B active");
        next("F active (B reprojected on its grid), back to B, F closed");
        st_.stage = 50; // the basemap
        break;
    }
    }
    return -1;
}

// Stages 50-56: the basemap, from a tile pyramid written to a temporary folder
// and read through GDAL's WMS driver as a file:// XYZ source (no network). So
// far, with the basemap None, nothing may have been made nor any WMS dataset
// opened. 50 checks that and a layer without a CRS, writes the pyramid (Web
// Mercator cells of 32 pixels of the finest level, red and green by cell,
// blue by zoom level; one tile of the finest level left out) and picks it; 51
// waits for the tiles of a 400 x 300 view on B; 52 checks the map pixels away
// from cell and tile edges against PROJ (B's pixels -> its CRS -> EPSG:3857
// -> the cell's colour; the coarse preview where the tile is missing), that the
// layers are drawn over it (opaque, then B at half opacity) and the basemap's
// own opacity, and times it; 53 a map panel and the swipe showing the basemap
// alone; 54 the attribution in the PNG and the view GeoTIFF; 55-56 back to
// None: the Basemap goes and nothing is drawn.
int App::selfTestBasemap() {
    namespace fs = std::filesystem;
    auto fail = [&](const std::string& what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what.c_str());
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
        st_.frames = 0;
    };
    constexpr int W = 400, H = 300, kZ = 13;
    constexpr double E = 20037508.342789244;
    const double tileSpan = 2 * E / (1 << kZ), cell = tileSpan / 8; // 32 pixels of zoom 13
    const fs::path dir = fs::temp_directory_path() / "janus-selftest-basemap";
    const CubeInfo& info = *s_->info;
    // The test's view: B fitted to 400 x 300, zoomed 1.5x about the centre.
    auto setView = [&] {
        canvasSize_ = ImVec2(float(W), float(H));
        fitView(canvasSize_);
        const ImVec2 c(W * 0.5f, H * 0.5f);
        offset_ = ImVec2(c.x - (c.x - offset_.x) * 1.5f, c.y - (c.y - offset_.y) * 1.5f);
        scale_ *= 1.5;
        viewTouched_ = true;
    };
    auto view = [&](double v[4]) {
        v[0] = -offset_.x / scale_;
        v[1] = -offset_.y / scale_;
        v[2] = (W - offset_.x) / scale_;
        v[3] = (H - offset_.y) / scale_;
    };
    OGRSpatialReference sa, sm;
    sa.importFromWkt(info.crsWkt.c_str());
    sm.importFromEPSG(3857);
    sa.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    sm.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    std::unique_ptr<OGRCoordinateTransformation, void (*)(OGRCoordinateTransformation*)> ct(
        OGRCreateCoordinateTransformation(&sa, &sm), OGRCoordinateTransformation::DestroyCT);
    if (!ct) return fail("no transformation from B's CRS to EPSG:3857");
    // Canvas point (pixel units) -> Web Mercator, exactly (PROJ).
    auto merc = [&](double cx, double cy, double& X, double& Y) {
        info.pixelToGeo((cx - offset_.x) / scale_, (cy - offset_.y) / scale_, X, Y);
        return ct->Transform(1, &X, &Y) == TRUE;
    };
    auto colour = [&](double X, double Y, int z, unsigned char c[3]) {
        const long long a = (long long)std::floor((X + E) / cell), b = (long long)std::floor((E - Y) / cell);
        c[0] = (unsigned char)(40 + 25 * (a % 8));
        c[1] = (unsigned char)(40 + 25 * (b % 8));
        c[2] = (unsigned char)(60 + 12 * z);
    };
    // The zoom 13 tile left out: under canvas point (100, 150) of the view.
    auto missing = [&](int& tx, int& ty) {
        setView();
        double X, Y;
        merc(100.5, 150.5, X, Y);
        tx = int(std::floor((X + E) / tileSpan));
        ty = int(std::floor((E - Y) / tileSpan));
    };
    auto render = [&](std::vector<unsigned char>& img) {
        renderMap(W, H);
        int w = 0, h = 0;
        gpu_.readMap(img, w, h);
    };
    auto near = [](const unsigned char* a, const unsigned char* b, int tol) {
        return std::abs(a[0] - b[0]) <= tol && std::abs(a[1] - b[1]) <= tol && std::abs(a[2] - b[2]) <= tol;
    };
    SeriesLayer& A = layers_[0];
    SeriesLayer& B = layers_[1];
    std::error_code ec;
    switch (st_.stage) {
    case 50: {
        int n = 0, wms = 0;
        GDALDataset** open = GDALDataset::GetOpenDatasets(&n);
        for (int i = 0; i < n; ++i)
            if (open[i]->GetDriver() && std::strcmp(open[i]->GetDriver()->GetDescription(), "WMS") == 0) ++wms;
        std::printf("    basemap None so far: Basemap %s, %d WMS datasets opened, %d open now\n",
                    basemap_ ? "made" : "not made", Basemap::datasetsOpened(), wms);
        if (basemap_ || Basemap::datasetsOpened() || wms) return fail("with the basemap None nothing may be made or opened");
        fs::remove_all(dir, ec);
        fs::create_directories(dir, ec);
        std::string url = dir.generic_u8string();
        url = "file://" + std::string(url.rfind('/', 0) == 0 ? "" : "/") + url + "/{z}/{x}/{y}.png";
        {
            CubeInfo c = info; // no CRS: not placed, with the reason
            c.crsWkt.clear();
            c.id = ~0ull;
            BasemapSource s;
            s.url = url;
            Basemap b(s, dir.u8string(), {});
            std::string why;
            if (b.setMap(c, why) || why.find("no CRS") == std::string::npos) return fail("a layer without a CRS: " + why);
            std::printf("    a layer without a CRS: \"%s\"\n", why.c_str());
        }
        // The pyramid over B and what the map can show around it.
        int mx, my;
        missing(mx, my);
        const double m = 1.6 * std::max(info.width, info.height);
        double box[4] = {HUGE_VAL, HUGE_VAL, -HUGE_VAL, -HUGE_VAL};
        for (double py : {-m, info.height + m})
            for (double px : {-m, info.width + m}) {
                double X, Y;
                info.pixelToGeo(px, py, X, Y);
                ct->Transform(1, &X, &Y);
                box[0] = std::min(box[0], X);
                box[1] = std::min(box[1], Y);
                box[2] = std::max(box[2], X);
                box[3] = std::max(box[3], Y);
            }
        GDALDriver* mem = GetGDALDriverManager()->GetDriverByName("MEM");
        GDALDriver* png = GetGDALDriverManager()->GetDriverByName("PNG");
        if (!mem || !png) return fail("GDAL has no MEM or PNG driver");
        std::vector<unsigned char> px(256 * 256 * 3);
        int tiles = 0;
        const double t0 = now();
        for (int z = 0; z <= kZ; ++z) {
            const double s = 2 * E / (1 << z), res = s / 256;
            const int x0 = int(std::floor((box[0] + E) / s)), x1 = int(std::floor((box[2] + E) / s));
            const int y0 = int(std::floor((E - box[3]) / s)), y1 = int(std::floor((E - box[1]) / s));
            for (int ty = y0; ty <= y1; ++ty)
                for (int tx = x0; tx <= x1; ++tx) {
                    if (z == kZ && tx == mx && ty == my) continue;
                    for (int j = 0; j < 256; ++j)
                        for (int i = 0; i < 256; ++i)
                            colour(-E + (tx * 256 + i + 0.5) * res, E - (ty * 256 + j + 0.5) * res, z,
                                   &px[size_t(j * 256 + i) * 3]);
                    const fs::path f = dir / std::to_string(z) / std::to_string(tx) / (std::to_string(ty) + ".png");
                    fs::create_directories(f.parent_path(), ec);
                    GDALDataset* ds = mem->Create("", 256, 256, 3, GDT_Byte, nullptr);
                    int bands[3] = {1, 2, 3};
                    ds->RasterIO(GF_Write, 0, 0, 256, 256, px.data(), 256, 256, GDT_Byte, 3, bands, 3, 256 * 3, 1, nullptr);
                    GDALDataset* out = png->CreateCopy(f.u8string().c_str(), ds, FALSE, nullptr, nullptr, nullptr);
                    if (out) GDALClose(out);
                    GDALClose(ds);
                    if (!out) return fail("could not write the tile " + f.u8string());
                    ++tiles;
                }
        }
        std::printf("    %d tiles (zoom 0-%d) written in %.0f ms; zoom %d tile %d/%d left out\n", tiles, kZ,
                    (now() - t0) * 1000, kZ, mx, my);
        bm_ = BasemapUi{};
        bm_.source = "custom";
        bm_.url = url;
        bm_.maxZoom = kZ;
        bm_.attribution = "Janus self-test tiles";
        bm_.opacity = 0.75f;
        // The setting through its handler of the layout file: written, read back.
        {
            ImGuiContext* ctx = ImGui::GetCurrentContext();
            ImGuiSettingsHandler* h = ImGui::FindSettingsHandler("JanusBasemap");
            ImGuiTextBuffer text;
            if (h) h->WriteAllFn(ctx, h, &text);
            const BasemapUi want = bm_;
            bm_ = BasemapUi{};
            void* entry = h ? h->ReadOpenFn(ctx, h, "Settings") : nullptr;
            std::istringstream lines(text.c_str());
            std::string line;
            std::getline(lines, line); // [JanusBasemap][Settings]
            while (entry && std::getline(lines, line))
                if (!line.empty()) h->ReadLineFn(ctx, h, entry, line.c_str());
            if (bm_.source != want.source || bm_.url != want.url || bm_.maxZoom != want.maxZoom || !bm_.on ||
                std::fabs(bm_.opacity - 0.75f) > 1e-6f || bm_.attribution != want.attribution)
                return fail(std::string("the basemap setting should be read back as written:\n") + text.c_str());
            bm_.opacity = 1.0f;
        }
        basemapFailedFor_ = 0;
        next("local tile pyramid picked as a custom XYZ basemap (file://), setting written and read back");
        break;
    }
    case 51: {
        if (!basemap_ || !basemap_->hasMap()) {
            if (!basemapWhy_.empty()) return fail("the basemap is not drawn: " + basemapWhy_);
            break;
        }
        setView();
        double v[4];
        view(v);
        const int z = basemap_->update(v, scale_);
        if (st_.frames++ == 0) {
            const Reprojection& R = *basemap_->map();
            std::printf("    warp grid to Web Mercator: %d x %d nodes, error %.3g px, made in %.1f ms\n", R.gridW, R.gridH,
                        R.gridError, R.buildMs);
        }
        if (basemap_->inflight() > 0) break;
        std::printf("    zoom %d: %d tiles read in %.0f ms (first after %.0f ms), %.1f ms each\n", z, basemap_->loads(),
                    (now() - st_.since) * 1000, basemap_->firstTileMs(), basemap_->avgLoadMs());
        if (z != kZ) return fail("the view should ask for zoom 13");
        if (basemap_->offline() || !basemap_->error().empty()) return fail("reading the tiles failed: " + basemap_->error());
        next("tiles of the view read (GDAL WMS, file://)");
        break;
    }
    case 52: {
        setView();
        int mx, my;
        missing(mx, my);
        std::vector<unsigned char> base, empty, none, both, bOnly, half, faded;
        A.visible = B.visible = false;
        render(base);
        bm_.on = false;
        render(empty);
        // Every 5th pixel, away from cell and tile edges (bilinear sampling mixes
        // neighbours there): the colour of PROJ's cell.
        int checked = 0, bad = 0, fallback = 0, worst = 0;
        for (int py = 2; py < H - 2; py += 5)
            for (int px = 2; px < W - 2; px += 5) {
                double X, Y;
                if (!merc(px + 0.5, py + 0.5, X, Y)) continue;
                const long long c0 = (long long)std::floor((X + E) / cell), r0 = (long long)std::floor((E - Y) / cell);
                // Where the zoom 13 tile is missing, the coarse preview (zoom 10,
                // 8x larger pixels) shows through.
                const bool gap = int(std::floor((X + E) / tileSpan)) == mx && int(std::floor((E - Y) / tileSpan)) == my;
                const double edge = gap ? 6.0 : 0.8; // screen pixels from a cell's edge (> half a texel)
                bool interior = true;
                for (const auto& [dx, dy] : {std::pair<double, double>{-edge, 0}, {edge, 0}, {0, -edge}, {0, edge}}) {
                    double X2, Y2;
                    interior = interior && merc(px + 0.5 + dx, py + 0.5 + dy, X2, Y2) &&
                               (long long)std::floor((X2 + E) / cell) == c0 && (long long)std::floor((E - Y2) / cell) == r0;
                }
                if (!interior) continue; // cells are inside tiles: also away from tile edges
                unsigned char want[3];
                colour(X, Y, gap ? kZ - 3 : kZ, want);
                const unsigned char* got = &base[(size_t(py) * W + px) * 4];
                const int d = std::max({std::abs(got[0] - want[0]), std::abs(got[1] - want[1]), std::abs(got[2] - want[2])});
                worst = std::max(worst, d);
                ++checked;
                fallback += gap;
                if (d > 2 && ++bad <= 3)
                    std::printf("    pixel (%d, %d): rgb(%d, %d, %d), PROJ's cell rgb(%d, %d, %d)\n", px, py, got[0], got[1],
                                got[2], want[0], want[1], want[2]);
            }
        std::printf("    basemap alone: %d map pixels on PROJ's cell colour (%d from zoom 10 where the zoom 13 tile is\n"
                    "    missing), %d differ, largest difference %d\n",
                    checked, fallback, bad, worst);
        if (bad || checked < 1500 || fallback < 50) return fail("the basemap should land where PROJ puts it");
        // Under the layers: they cover it where they have data.
        A.visible = B.visible = true;
        render(none);
        bm_.on = true;
        render(both);
        size_t covered = 0, wrong = 0;
        for (size_t i = 0; i < size_t(W) * H; ++i) {
            const bool layer = !near(&none[i * 4], &empty[i * 4], 0);
            covered += layer;
            wrong += !near(&both[i * 4], layer ? &none[i * 4] : &base[i * 4], 0);
        }
        std::printf("    under the layers: %zu of %d pixels show a layer, %zu differ\n", covered, W * H, wrong);
        if (wrong || covered < size_t(W) * H / 4) return fail("the layers should be drawn over the basemap");
        // B at half opacity (A hidden): half B, half the basemap.
        A.visible = false;
        bm_.on = false;
        render(bOnly);
        bm_.on = true;
        B.opacity = 0.5f;
        render(half);
        B.opacity = 1.0f;
        size_t inB = 0;
        wrong = 0;
        for (size_t i = 0; i < size_t(W) * H; ++i) {
            if (near(&bOnly[i * 4], &empty[i * 4], 0)) continue;
            ++inB;
            unsigned char want[3];
            for (int k = 0; k < 3; ++k) want[k] = (unsigned char)std::lround(0.5 * bOnly[i * 4 + k] + 0.5 * base[i * 4 + k]);
            wrong += !near(&half[i * 4], want, 2);
        }
        // The basemap at half opacity, alone: half way to the background.
        B.visible = false;
        bm_.opacity = 0.5f;
        render(faded);
        bm_.opacity = 1.0f;
        size_t wrongFade = 0;
        for (size_t i = 0; i < size_t(W) * H; ++i) {
            unsigned char want[3];
            for (int k = 0; k < 3; ++k) want[k] = (unsigned char)std::lround(0.5 * base[i * 4 + k] + 0.5 * empty[i * 4 + k]);
            wrongFade += !near(&faded[i * 4], want, 2);
        }
        A.visible = B.visible = true;
        std::printf("    B at opacity 0.5 over it: %zu of %zu pixels off; basemap at opacity 0.5: %zu off\n", wrong, inB,
                    wrongFade);
        if (wrong || inB < size_t(W) * H / 4) return fail("a layer's opacity should show the basemap through");
        if (wrongFade) return fail("the basemap's opacity should fade it towards the background");
        // Costs: the per-frame request of a view whose tiles are all there, and a render.
        double v[4];
        view(v);
        const double t0 = now();
        for (int i = 0; i < 200; ++i) basemap_->update(v, scale_);
        const double updateUs = (now() - t0) / 200 * 1e6;
        auto renderMs = [&](bool on) {
            bm_.on = on;
            unsigned char px[4];
            renderMap(W, H);
            gpu_.readMapPixel(0, 0, px);
            const double s = now();
            for (int i = 0; i < 20; ++i) {
                renderMap(W, H);
                gpu_.readMapPixel(0, 0, px);
            }
            bm_.on = true;
            return (now() - s) / 20 * 1000;
        };
        const double off = renderMs(false), on = renderMs(true);
        std::printf("    per frame: request of the view %.1f us; render + readback %.3f ms without, %.3f ms with it\n",
                    updateUs, off, on);
        next("basemap on PROJ's place, under the layers, opacity of both");
        break;
    }
    case 53: {
        setView();
        std::vector<unsigned char> base, bOnly, img;
        A.visible = B.visible = false;
        render(base);
        A.visible = B.visible = true;
        auto panel = [&](MapView& v) {
            renderView(v, W, H, 1.0f, ImVec2(0, 0));
            int w = 0, h = 0;
            gpu_.readMap(img, w, h, v.id);
            gpu_.releaseMap(v.id);
        };
        MapView v;
        v.id = 98;
        v.cube = B.session->info->id;
        v.basemapOnly = true;
        panel(v);
        if (img != base) return fail("a map panel with the basemap only should draw the basemap alone");
        v.basemapOnly = false;
        panel(v);
        A.visible = false;
        render(bOnly);
        A.visible = true;
        if (img != bOnly) return fail("a map panel of B should draw B over the basemap, as the main map");
        if (!swipe_) toggleSwipe();
        const MapView keep = swipeView_;
        swipeView_.basemapOnly = true;
        renderSwipe(canvasSize_, 1.0f);
        int w = 0, h = 0;
        gpu_.readMap(img, w, h, kSwipeSlot);
        swipeView_ = keep;
        swipeView_.dirty = true;
        if (img != base) return fail("the swipe's comparison should be able to show the basemap alone");
        next("map panel and swipe: the basemap alone, a layer over it");
        break;
    }
    case 54: {
        setView();
        std::vector<unsigned char> both;
        render(both);
        const fs::path out = dir / "export";
        fs::create_directories(out, ec);
        PngOptions o;
        o.label = o.legend = false;
        auto done = [](const std::shared_ptr<ExportJob>& j) {
            if (!j) return false;
            j->done.wait();
            return j->state == ExportJob::State::Done;
        };
        const std::string pngPath = (out / "map.png").u8string(), tifPath = (out / "view.tif").u8string();
        if (!done(exportPng(pngPath, o)) || !done(exportView(tifPath, 1))) return fail("the exports failed");
        exports_.clear();
        showExports_ = false;
        GDALDataset* ds = GDALDataset::Open(pngPath.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds || ds->GetRasterXSize() != W || ds->GetRasterYSize() != H) return fail("the PNG should be 400 x 300");
        std::vector<unsigned char> img(size_t(W) * H * 3);
        ds->RasterIO(GF_Read, 0, 0, W, H, img.data(), W, H, GDT_Byte, 3, nullptr, 3, W * 3, 1, nullptr);
        GDALClose(ds);
        size_t away = 0, credit = 0;
        for (int y = 0; y < H; ++y)
            for (int x = 0; x < W; ++x) {
                const bool same = near(&img[(size_t(y) * W + x) * 3], &both[(size_t(y) * W + x) * 4], 0);
                if (x < W - 200 || y < H - 30) away += !same;
                else credit += !same;
            }
        ds = GDALDataset::Open(tifPath.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        const char* c = ds ? ds->GetMetadataItem("TIFFTAG_COPYRIGHT") : nullptr;
        const std::string tag = c ? c : "";
        if (ds) GDALClose(ds);
        std::printf("    PNG: %zu pixels of the attribution box bottom right, %zu differ elsewhere; view GeoTIFF:\n"
                    "    copyright \"%s\"\n",
                    credit, away, tag.c_str());
        if (away || credit < 100) return fail("the PNG should be the map with the attribution bottom right");
        if (tag.find("Janus self-test tiles") == std::string::npos)
            return fail("the view GeoTIFF should carry the attribution");
        next("attribution in the PNG and the view GeoTIFF");
        break;
    }
    case 55:
        bm_.source = "none"; // the next frame retires the Basemap
        next("basemap None again");
        break;
    case 56: {
        setView();
        std::vector<unsigned char> img, none;
        render(img);
        bm_.on = false;
        render(none);
        bm_.on = true;
        if (basemap_ || basemapShown()) return fail("with the basemap None the Basemap should be gone");
        if (img != none) return fail("with the basemap None nothing should be drawn under the layers");
        retiredBasemaps_.clear(); // its datasets closed before the files go
        fs::remove_all(dir, ec);
        viewTouched_ = false;
        mapDirty_ = true;
        next("basemap gone, the map as before");
        st_.stage = 7;
        break;
    }
    }
    return -1;
}
