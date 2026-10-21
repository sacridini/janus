// `--profile-tour IN`: a fixed tour of a series in a hidden window, for the
// profiler (a build with -DJANUS_TRACY=ON, see profiler.hpp) or on its own
// for its timings: open, the overview and its statistics, the cursor series
// over random places, every date, the display modes, a zoom to full
// resolution, panning and dates there (detail tiles), an ROI, and back.
// Each stage prints its time and frames; in Tracy, each is a span of its own
// (a discontinuous frame set named after it) and a message.
#include "app.hpp"
#include "profiler.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>

namespace {

double nowMs() {
    return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

// Stage names (also the spans in the profiler): index = stage.
const char* const kStages[] = {"",           "open",          "overview",         "hover series",
                               "every date", "display modes", "zoom in",          "pan (full res)",
                               "dates (full res)", "ROI 512x512", "zoom out"};

} // namespace

int App::profileTourStep(const std::vector<std::string>& inputs) {
    auto& p = tour_;
    const double now = nowMs();
    // Ends the stage: its time and frames, then the next one.
    auto next = [&] {
        if (p.stage > 0) {
            const double ms = now - p.since;
            std::printf("  %-18s %8.0f ms  %5d frames  %6.1f ms/frame\n", kStages[p.stage], ms, p.frames,
                        p.frames ? ms / p.frames : 0.0);
            std::fflush(stdout);
            PROFILE_SPAN_END(kStages[p.stage]);
        }
        ++p.stage;
        if (p.stage < int(std::size(kStages))) {
            PROFILE_MESSAGE(kStages[p.stage]);
            PROFILE_SPAN_BEGIN(kStages[p.stage]);
        }
        p.since = now;
        p.frames = 0;
        p.step = 0;
        p.stepFrame = 0;
    };
    // The map centred on source pixel (x, y) at `scale` screen pixels per source pixel.
    auto viewAt = [&](double x, double y, double scale) {
        scale_ = scale;
        offset_ = ImVec2(float(canvasSize_.x * 0.5 - x * scale), float(canvasSize_.y * 0.5 - y * scale));
        zooming_ = false;
        mapDirty_ = true;
    };
    // The cursor on source pixel (x, y), as App::mapInput puts it: the series
    // from the overview right away, the exact one asked for on the next frame.
    auto hoverAt = [&](int x, int y) {
        hover_.x = x;
        hover_.y = y;
        hover_.values = approxSeries(x, y);
        hover_.exact = false;
        hover_.stats = computeSeriesStats(years_, hover_.values);
        hover_.request = 0;
        hover_.zeitReq = 0;
        hover_.zeitVersion = -1;
        hover_.zeitResult = json();
        updateOtherHover(x, y);
        approxLayers_ = s_->overview.layersDone();
        hoverPending_ = true;
        hoverSince_ = ImGui::GetTime() - 1; // as if the mouse had rested
    };
    // Detail tiles of the view on the GPU (a few frames after it changed).
    auto tilesSettled = [&] { return p.frames - p.stepFrame >= 3 && s_->tiles->inflight() == 0; };

    if (p.stage > 0) ++p.frames;
    switch (p.stage) {
    case 0:
        ImGui::GetIO().IniFilename = nullptr; // the default layout and settings, nothing saved
        if (inputs.empty()) {
            std::printf("usage: --profile-tour INPUT...\n");
            return 2;
        }
        std::printf("profile tour: %s\n", inputs[0].c_str());
        p.t0 = now;
        openInputs(inputs);
        next();
        break;
    case 1: // the files opened
        if (!s_) {
            if (!opening_.valid() && now - p.t0 > 2000) {
                std::printf("could not open %s\n", inputs[0].c_str());
                return 1;
            }
            break;
        }
        std::printf("  %d x %d x %d %s, overview %d x %d (%s)\n", s_->info->width, s_->info->height, s_->info->T(),
                    s_->info->dataType.c_str(), s_->overview.w, s_->overview.h,
                    s_->overview.fromCache() ? "from its cache" : "built");
        next();
        break;
    case 2: // the overview (built or from its cache) and its statistics
        if (!s_->overview.complete() || (s_->info->T() >= 2 && !s_->gpu.statsValid)) break;
        next();
        break;
    case 3: { // the cursor over 40 places, each until its exact series came (or 120 frames)
        const int W = s_->info->width, H = s_->info->height;
        if (p.step == 0 || hover_.exact || p.frames - p.stepFrame > 120) {
            if (p.step == 40) {
                next();
                break;
            }
            // A fixed scatter (golden ratio steps): the same places on every run.
            const double u = std::fmod(0.5 + p.step * 0.6180339887, 1.0), v = std::fmod(0.5 + p.step * 0.7548776662, 1.0);
            hoverAt(int(u * (W - 1)), int(v * (H - 1)));
            p.stepFrame = p.frames;
            ++p.step;
        }
        break;
    }
    case 4: // every date, one per frame (an animation over the whole image)
        if (p.step == 0) fitView(canvasSize_);
        if (p.step >= s_->info->T()) {
            setT(0);
            next();
            break;
        }
        setT(p.step++);
        break;
    case 5: { // the display modes that need no options, 10 frames each
        static const int modes[] = {ModeAnomaly, ModeMean, ModeStd, ModeSlope, ModeMin,
                                    ModeMax, ModeAmplitude, ModeDropDate, ModeDropMag, ModeValue};
        if (p.frames % 10 != 1) break;
        if (p.step == int(std::size(modes))) {
            next();
            break;
        }
        mode_ = modes[p.step++];
        mapDirty_ = true;
        break;
    }
    case 6: { // from the whole image to 2 screen pixels per source pixel around the centre, in 60 frames
        const double fit = 0.98 * std::min(canvasSize_.x / s_->info->width, canvasSize_.y / s_->info->height);
        const double k = std::min(1.0, p.frames / 60.0);
        viewAt(s_->info->width * 0.5, s_->info->height * 0.5, fit * std::pow(2.0 / fit, k));
        if (k >= 1.0 && p.stepFrame == 0) p.stepFrame = p.frames;
        if (k >= 1.0 && tilesSettled()) next();
        break;
    }
    case 7: // 12 steps of 300 screen pixels to the east, each once its tiles are on the GPU
        if (p.step == 0 || tilesSettled()) {
            if (p.step == 12) {
                next();
                break;
            }
            ++p.step;
            p.stepFrame = p.frames;
            viewAt(s_->info->width * 0.5 + p.step * 150.0, s_->info->height * 0.5, 2.0);
        }
        break;
    case 8: { // 10 dates at full resolution, each once its tiles are on the GPU
        const int n = std::min(10, s_->info->T());
        if (p.step == 0 || tilesSettled()) {
            if (p.step == n) {
                setT(0);
                next();
                break;
            }
            setT(p.step++ * s_->info->T() / n);
            p.stepFrame = p.frames;
        }
        break;
    }
    case 9: // an ROI of 512 x 512 source pixels at the centre, until its series is complete
        if (p.step == 0) {
            const int cx = s_->info->width / 2, cy = s_->info->height / 2;
            const int r[4] = {cx - 256, cy - 256, cx + 256, cy + 256};
            setRoiShape(r, Polygon{});
            p.step = 1;
        }
        if (roi_ && roi_->done.load() >= s_->info->T()) {
            std::printf("  (ROI read in %.0f ms, %d dates from the full-resolution cache)\n", roi_->ms.load(),
                        roi_->cachedDates.load());
            next();
        }
        break;
    case 10: // back to the whole image
        if (p.frames == 1) fitView(canvasSize_);
        if (p.frames < 30) break;
        next();
        std::printf("  total %.1f s\n", (now - p.t0) / 1000);
        std::fflush(stdout);
        return 0;
    }
    PROFILE_PLOT("tour stage", int64_t(p.stage));
    return -1;
}

// `--dump-golden IN... OUT`: the map of IN in every display mode that needs no
// options, rendered at 400 x 300 with the whole image in view, as raw RGBA
// files (OUT/mode_NN.rgba), with the automatic ranges and a sample of the
// statistics (OUT/golden.txt): the references the Rust port compares its maps
// and statistics with (RUST_MIGRATION.md, phase 0).
int App::dumpGoldenStep(const std::vector<std::string>& args) {
    auto& g = golden_;
    static const int modes[] = {ModeValue, ModeAnomaly, ModeMean,      ModeStd,      ModeSlope,  ModeMin,
                                ModeMax,   ModeAmplitude, ModeR2, ModeDropDate, ModeDropMag};
    char line[256];
    switch (g.stage) {
    case 0: {
        ImGui::GetIO().IniFilename = nullptr; // the default layout and settings, nothing saved
        if (args.size() < 2) {
            std::printf("usage: --dump-golden INPUT... OUT\n");
            return 2;
        }
        g.out = args.back();
        std::error_code ec;
        std::filesystem::create_directories(std::filesystem::u8path(g.out), ec);
        openInputs(std::vector<std::string>(args.begin(), args.end() - 1));
        g.stage = 1;
        break;
    }
    case 1: { // the overview and its statistics, then a sample of the statistics
        if (!s_ || !s_->overview.complete() || (s_->info->T() >= 2 && !s_->gpu.statsValid)) break;
        const Overview& ov = s_->overview;
        std::snprintf(line, sizeof(line), "size %d %d %d overview %d %d\n", s_->info->width, s_->info->height,
                      s_->info->T(), ov.w, ov.h);
        g.text = line;
        const size_t n = size_t(ov.w) * ov.h;
        for (size_t p = 0; s_->gpu.statsValid && p < n; p += 997) {
            const float* a = s_->gpu.hostStats0 + p * 4;
            const float* b = s_->gpu.hostStats1 + p * 4;
            std::snprintf(line, sizeof(line), "stats %zu %.9g %.9g %.9g %.9g %.9g %.9g %.9g %.9g %.9g\n", p, a[0], a[1],
                          a[2], a[3], b[0], b[1], b[2], b[3], s_->gpu.hostStats2[p]);
            g.text += line;
        }
        mode_ = modes[0];
        mapDirty_ = true;
        g.stage = 2;
        break;
    }
    case 2: { // mode_'s range was computed in this frame: its map, then the next mode
        const double W = s_->info->width, H = s_->info->height;
        const double oldScale = scale_;
        const ImVec2 oldOffset = offset_;
        scale_ = 0.98 * std::min(400.0 / W, 300.0 / H);
        offset_ = ImVec2(float((400 - W * scale_) * 0.5), float((300 - H * scale_) * 0.5));
        renderMap(400, 300, 1.0f, -1);
        std::vector<unsigned char> rgba;
        int w = 0, h = 0;
        gpu_.readMap(rgba, w, h, -1);
        scale_ = oldScale;
        offset_ = oldOffset;
        std::snprintf(line, sizeof(line), "mode_%02d.rgba", mode_);
        std::ofstream(std::filesystem::u8path(g.out) / line, std::ios::binary)
            .write(reinterpret_cast<const char*>(rgba.data()), std::streamsize(rgba.size()));
        std::snprintf(line, sizeof(line), "range %d %.9g %.9g cmap %d\n", mode_, range_[mode_].lo, range_[mode_].hi,
                      cmap_[mode_]);
        g.text += line;
        if (++g.step == int(std::size(modes))) {
            const auto path = std::filesystem::u8path(g.out) / "golden.txt";
            std::ofstream(path, std::ios::binary) << g.text;
            std::printf("golden: %d maps and %s\n", g.step, path.u8string().c_str());
            return 0;
        }
        mode_ = modes[g.step];
        mapDirty_ = true;
        break;
    }
    }
    return -1;
}
