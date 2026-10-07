#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <functional>

#include <imgui_internal.h>
#include <implot.h>

#include "gl.hpp"
#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

// Latin-1 only: ImGui's default font has no arrows, dashes, etc.
const char* kModeNames[ModeCount] = {
    "Value at date",
    "Anomaly (value - mean)",
    "Temporal mean",
    "Temporal std. deviation",
    "Linear trend (OLS)",
    "Temporal minimum",
    "Temporal maximum",
    "Amplitude (max - min)",
    "Trend R²",
    "Multitemporal RGB (3 dates)",
};

bool modeIsTimeDependent(int m) { return m == ModeValue || m == ModeAnomaly || m == ModeRGB; }
bool modeNeedsStats(int m) { return m != ModeValue && m != ModeRGB; }

std::string escapePercent(const std::string& s) {
    std::string out;
    for (char c : s) {
        out += c;
        if (c == '%') out += '%';
    }
    return out;
}

std::vector<double> toDouble(const std::vector<float>& v) { return std::vector<double>(v.begin(), v.end()); }

int nearestIndex(const std::vector<double>& xs, double x) {
    auto it = std::lower_bound(xs.begin(), xs.end(), x);
    if (it == xs.end()) return int(xs.size()) - 1;
    if (it == xs.begin()) return 0;
    return (x - *(it - 1) < *it - x) ? int(it - xs.begin()) - 1 : int(it - xs.begin());
}

} // namespace

App::App(GLFWwindow* window) : window_(window) {}

App::~App() {
    if (opening_.valid()) opening_.wait();
    closeSession();
    gpu_.shutdown();
}

bool App::init(const AppOptions& opts, std::string& error) {
    opts_ = opts;
    budgetUi_ = int(opts.budgetMB);
    if (!gpu_.init(error)) return false;

    const std::string appData = platform::appDataDir();
    settings_.cacheDir = (fs::u8path(appData) / "cache").u8string();
    std::error_code ec;
    fs::create_directories(fs::u8path(settings_.cacheDir), ec);
    Overview::pruneCache(settings_.cacheDir, 20ull << 30);
    settings_.overviewBudgetBytes = opts.budgetMB << 20;
    settings_.ioThreads = opts.ioThreads;
    GLint maxTex = 0;
    glGetIntegerv(GL_MAX_TEXTURE_SIZE, &maxTex);
    settings_.maxTexSize = std::min<int>(maxTex, 16384);

    iniPath_ = (fs::u8path(appData) / "layout.ini").u8string();
    iniExisted_ = fs::exists(fs::u8path(iniPath_), ec);
    ImGui::GetIO().IniFilename = iniPath_.c_str();
    layoutPending_ = !iniExisted_;

    // Extra colormap designed for vegetation indices.
    const ImVec4 ndvi[] = {{0.55f, 0.33f, 0.14f, 1}, {0.80f, 0.66f, 0.42f, 1}, {0.96f, 0.91f, 0.60f, 1},
                           {0.62f, 0.80f, 0.36f, 1}, {0.22f, 0.56f, 0.22f, 1}, {0.04f, 0.30f, 0.10f, 1}};
    ImPlot::AddColormap("NDVI", ndvi, 6, false);
    for (int m = 0; m < ModeCount; ++m) cmap_[m] = ImPlotColormap_Viridis;
    cmap_[ModeAnomaly] = cmap_[ModeSlope] = ImPlotColormap_BrBG;
    cmap_[ModeStd] = cmap_[ModeAmplitude] = ImPlotColormap_Plasma;
    return true;
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

void App::openInputs(const std::vector<std::string>& inputs) {
    if (opening_.valid()) return; // one open at a time
    openingInputs_ = inputs;
    const int band = opts_.band;
    opening_ = std::async(std::launch::async, [inputs, band] {
        const auto t0 = std::chrono::steady_clock::now();
        OpenResult r;
        r.info = openCube(inputs, band, r.error);
        r.seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
        glfwPostEmptyEvent(); // wake the UI loop
        return r;
    });
}

// Main thread: called once the background open has finished.
void App::finishOpen() {
    OpenResult r = opening_.get();
    const std::vector<std::string> inputs = std::move(openingInputs_);
    auto info = r.info;
    if (!info) {
        error_ = r.error;
        openErrorPopup_ = true;
        return;
    }
    GLint maxLayers = 0;
    glGetIntegerv(GL_MAX_ARRAY_TEXTURE_LAYERS, &maxLayers);
    if (info->T() > maxLayers) {
        error_ = "The series has " + std::to_string(info->T()) + " dates; the GPU supports up to " +
                 std::to_string(maxLayers) + " layers per texture.";
        openErrorPopup_ = true;
        return;
    }

    closeSession();
    s_ = std::make_unique<Session>(info, settings_, [] { glfwPostEmptyEvent(); });
    s_->openSeconds = r.seconds;
    lastInputs_ = inputs;

    const int T = info->T();
    xs_.resize(T);
    years_.resize(T);
    for (int t = 0; t < T; ++t) {
        xs_[t] = info->layers[t].time;
        years_[t] = info->yearsFromStart(t);
    }
    t_ = 0;
    playing_ = false;
    rgb_ = {0, T / 2, T - 1};
    if (!modeAvailable(mode_)) mode_ = ModeValue;
    for (auto& r : range_) r = Range{};
    histKey_ = ~0ull;
    hover_ = SeriesView{};
    pins_.clear();
    nextPinId_ = 1;
    roi_.reset();
    roiMean_.clear();
    roiSeen_ = -1;
    fitRequested_ = true;
    viewTouched_ = false;
    hoverPending_ = false;
    approxLayers_ = -1;
    mapDirty_ = true;

    const std::string where = inputs.size() == 1 ? inputs[0] : fs::u8path(info->firstPath).parent_path().u8string();
    const std::string title = "tsv - " + where + " (" + info->description + ")";
    glfwSetWindowTitle(window_, title.c_str());
}

void App::closeSession() {
    if (roi_) roi_->cancel = true;
    roi_.reset();
    s_.reset();
    glfwSetWindowTitle(window_, "tsv");
}

bool App::wantsContinuousFrames() const { return playing_ || roiDragging_ || hoverPending_; }

bool App::modeAvailable(int mode) const {
    if (!s_) return false;
    return mode == ModeValue || mode == ModeRGB || s_->info->T() >= 2;
}

std::string App::slopeUnit() const { return s_ && s_->info->timeIsDate ? "/yr" : "/step"; }

void App::setT(int t) {
    if (!s_) return;
    const int T = s_->info->T();
    t = ((t % T) + T) % T;
    if (t != t_) {
        t_ = t;
        mapDirty_ = true;
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

void App::frame() {
    const auto frameStart = std::chrono::steady_clock::now();
    if (!pendingDrop.empty() && !opening_.valid()) {
        auto drop = std::move(pendingDrop);
        pendingDrop.clear();
        openInputs(drop);
    }
    if (opening_.valid() && opening_.wait_for(std::chrono::seconds(0)) == std::future_status::ready) finishOpen();
    if (s_) {
        s_->overview.setFocus(t_);
        if (s_->pump(gpu_)) mapDirty_ = true;
        pumpSeries();
        // Cursor series: approximated (overview) right away; exact once the
        // mouse rests. On an HDD, while the overview builds, the exact one is
        // not read: the extra seeks would slow the build down a lot.
        if (hover_.x >= 0 && !hover_.exact && s_->overview.layersDone() != approxLayers_) {
            approxLayers_ = s_->overview.layersDone();
            hover_.values = approxSeries(hover_.x, hover_.y);
            hover_.stats = computeSeriesStats(years_, hover_.values);
        }
        if (hoverPending_ && ImGui::GetTime() - hoverSince_ >= 0.12) {
            hoverPending_ = false;
            if (!s_->deferRandomReads()) hover_.request = s_->requestSeries(hover_.x, hover_.y, true);
        }
        if (!s_->deferRandomReads()) {
            for (SeriesView& p : pins_) // pins created while the HDD overview was building
                if (!p.exact && p.request == 0) p.request = s_->requestSeries(p.x, p.y, false);
            if (pendingRoi_) {
                roi_ = s_->startRoi(pendingRoiRect_[0], pendingRoiRect_[1], pendingRoiRect_[2], pendingRoiRect_[3]);
                roiSeen_ = -1;
                pendingRoi_ = false;
            }
        }
        updateRoiSeries();
        if (playing_) {
            playAccum_ += ImGui::GetIO().DeltaTime;
            if (playAccum_ >= 1.0 / fps_) {
                playAccum_ = 0;
                setT(t_ + 1);
            }
        }
        if (cmap_[mode_] != appliedCmap_) {
            gpu_.setColormap(cmap_[mode_]);
            appliedCmap_ = cmap_[mode_];
            mapDirty_ = true;
        }
        updateRangeAndHistogram();
    } else if (appliedCmap_ < 0) {
        gpu_.setColormap(cmap_[ModeValue]);
        appliedCmap_ = cmap_[ModeValue];
    }

    handleShortcuts();
    uiMenu();
    uiDockspace();
    uiLayer();
    uiPerf();
    uiSeries();
    uiStats();
    uiMap();
    uiPopups();
    const double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - frameStart).count();
    frameMs_ = frameMs_ * 0.9 + ms * 0.1;
}

void App::pumpSeries() {
    for (SeriesResult& r : s_->takeSeries()) {
        auto apply = [&](SeriesView& v) {
            if (v.request != r.id) return false;
            v.values = std::move(r.values);
            v.exact = true;
            v.stats = computeSeriesStats(years_, v.values);
            lastSeriesMs_ = r.ms;
            return true;
        };
        if (apply(hover_)) continue;
        for (SeriesView& p : pins_)
            if (apply(p)) break;
    }
}

std::vector<float> App::approxSeries(int x, int y) const {
    const Overview& ov = s_->overview;
    const int ox = std::min(ov.w - 1, int(double(x) * ov.w / s_->info->width));
    const int oy = std::min(ov.h - 1, int(double(y) * ov.h / s_->info->height));
    std::vector<float> v(size_t(ov.T), NAN);
    for (int t = 0; t < ov.T; ++t)
        if (s_->gpu.loaded[t]) v[t] = ov.at(t, ox, oy);
    return v;
}

void App::addPin(int x, int y) {
    SeriesView p;
    p.x = x;
    p.y = y;
    p.values = approxSeries(x, y);
    p.stats = computeSeriesStats(years_, p.values);
    p.request = s_->deferRandomReads() ? 0 : s_->requestSeries(x, y, false);
    p.id = nextPinId_++;
    // Vivid colors that contrast with the map colormaps and the dark
    // background; no yellow (ROI) and no orange (current-date line).
    static const ImVec4 kPinColors[] = {
        {1.00f, 0.36f, 0.42f, 1}, {0.25f, 0.80f, 1.00f, 1}, {0.76f, 0.52f, 1.00f, 1}, {0.55f, 0.95f, 0.40f, 1},
        {1.00f, 0.55f, 0.85f, 1}, {0.30f, 1.00f, 0.80f, 1}, {1.00f, 0.66f, 0.52f, 1}, {0.56f, 0.70f, 1.00f, 1}};
    p.color = kPinColors[(p.id - 1) % IM_ARRAYSIZE(kPinColors)];
    pins_.push_back(std::move(p));
}

void App::clearRoi() {
    if (roi_) roi_->cancel = true;
    roi_.reset();
    pendingRoi_ = false;
    roiMean_.clear();
    mapDirty_ = true;
}

void App::updateRoiSeries() {
    if (!roi_) return;
    const int done = roi_->done.load();
    if (done == roiSeen_) return;
    roiSeen_ = done;
    const int T = s_->info->T();
    roiMean_.assign(T, NAN);
    roiP10_.assign(T, NAN);
    roiP90_.assign(T, NAN);
    for (int t = 0; t < T; ++t) {
        const SampleStats& st = roi_->perT[t];
        if (st.n == 0) continue;
        roiMean_[t] = st.mean;
        roiP10_[t] = st.p10;
        roiP90_[t] = st.p90;
    }
    roiStats_ = computeSeriesStats(years_, roiMean_);
}

// (Subsampled) sample of the quantity shown by `mode`. t < 0 = all dates.
std::vector<float> App::collectSample(int mode, int t, size_t maxN) const {
    std::vector<float> out;
    const Overview& ov = s_->overview;
    const size_t px = size_t(ov.w) * ov.h;
    const std::vector<float>& s0 = s_->stats0;
    const std::vector<float>& s1 = s_->stats1;

    auto overLayers = [&](auto get) {
        std::vector<int> layers;
        if (t >= 0) {
            if (s_->gpu.loaded[t]) layers.push_back(t);
        } else {
            for (int i = 0; i < ov.T; ++i)
                if (s_->gpu.loaded[i]) layers.push_back(i);
        }
        if (layers.empty()) return;
        const size_t total = layers.size() * px;
        const size_t step = std::max<size_t>(1, total / maxN);
        out.reserve(total / step + 1);
        for (size_t i = (step / 2) % px; i < total; i += step) {
            const int L = layers[i / px];
            const size_t p = i % px;
            const float v = get(L, p);
            if (!std::isnan(v)) out.push_back(v);
        }
    };
    auto overPixels = [&](auto get) {
        if (!s_->gpu.statsValid) return;
        const size_t step = std::max<size_t>(1, px / maxN);
        out.reserve(px / step + 1);
        for (size_t p = 0; p < px; p += step) {
            const float v = get(p);
            if (!std::isnan(v)) out.push_back(v);
        }
    };

    switch (mode) {
    case ModeValue:
    case ModeRGB: overLayers([&](int L, size_t p) { return ov.layer(L)[p]; }); break;
    case ModeAnomaly:
        if (s_->gpu.statsValid) overLayers([&](int L, size_t p) { return ov.layer(L)[p] - s0[p * 4]; });
        break;
    case ModeMean: overPixels([&](size_t p) { return s0[p * 4 + 0]; }); break;
    case ModeStd: overPixels([&](size_t p) { return s0[p * 4 + 1]; }); break;
    case ModeSlope: overPixels([&](size_t p) { return s0[p * 4 + 2]; }); break;
    case ModeMin: overPixels([&](size_t p) { return s1[p * 4 + 0]; }); break;
    case ModeMax: overPixels([&](size_t p) { return s1[p * 4 + 1]; }); break;
    case ModeAmplitude: overPixels([&](size_t p) { return s1[p * 4 + 1] - s1[p * 4 + 0]; }); break;
    case ModeR2: overPixels([&](size_t p) { return s1[p * 4 + 2]; }); break;
    default: break;
    }
    return out;
}

void App::updateRangeAndHistogram() {
    if (modeNeedsStats(mode_) && !s_->gpu.statsValid) return;
    const uint64_t version = uint64_t(s_->overview.layersDone()) * 2 + (s_->gpu.statsValid ? 1 : 0);
    const bool timeDep = modeIsTimeDependent(mode_);

    Range& r = range_[mode_];
    const int rangeT = (perDateRange_ && timeDep) ? t_ : -1;
    const uint64_t rkey = version * 1000003ull + uint64_t(rangeT + 1) * 8 + (perDateRange_ ? 1 : 0);
    if (!r.manual && r.key != rkey) {
        std::vector<float> s = collectSample(mode_, rangeT, 2000000);
        if (!s.empty()) {
            if (mode_ == ModeR2) {
                r.lo = 0;
                r.hi = 1;
            } else if (mode_ == ModeAnomaly || mode_ == ModeSlope) {
                for (float& v : s) v = std::fabs(v);
                float lo, hi;
                samplePercentiles(s.data(), s.size(), s.size(), 0.0, 0.98, lo, hi);
                r.lo = -hi;
                r.hi = hi;
            } else {
                samplePercentiles(s.data(), s.size(), s.size(), 0.02, 0.98, r.lo, r.hi);
            }
            r.key = rkey;
            mapDirty_ = true;
        }
    }

    const int histT = timeDep ? (mode_ == ModeRGB ? rgb_[0] : t_) : -1;
    const uint64_t hkey = version * 1000003ull + uint64_t(histT + 1) * 16 + uint64_t(mode_);
    if (hkey != histKey_) {
        histKey_ = hkey;
        histX_.clear();
        histY_.clear();
        std::vector<float> s = collectSample(mode_, histT, 400000);
        if (s.size() > 10) {
            float lo, hi;
            samplePercentiles(s.data(), s.size(), s.size(), 0.005, 0.995, lo, hi);
            const int bins = 64;
            histBarW_ = (hi - lo) / bins;
            histX_.resize(bins);
            histY_.assign(bins, 0);
            for (int i = 0; i < bins; ++i) histX_[i] = lo + (i + 0.5) * histBarW_;
            for (float v : s) {
                const int b = int((v - lo) / histBarW_);
                if (b >= 0 && b < bins) histY_[b] += 1;
            }
            for (double& y : histY_) y = 100.0 * y / s.size();
        }
    }
}

// ---------------------------------------------------------------------------
// Shortcuts, menu, layout
// ---------------------------------------------------------------------------

void App::handleShortcuts() {
    ImGuiIO& io = ImGui::GetIO();
    if (ImGui::IsKeyChordPressed(ImGuiMod_Ctrl | ImGuiKey_O)) {
        auto files = platform::openFilesDialog();
        if (!files.empty()) openInputs(files);
    }
    if (ImGui::IsKeyChordPressed(ImGuiMod_Ctrl | ImGuiMod_Shift | ImGuiKey_O)) {
        auto dir = platform::openFolderDialog();
        if (!dir.empty()) openInputs({dir});
    }
    if (io.WantTextInput || !s_) return;
    if (ImGui::IsKeyPressed(ImGuiKey_LeftArrow)) setT(t_ - 1);
    if (ImGui::IsKeyPressed(ImGuiKey_RightArrow)) setT(t_ + 1);
    if (ImGui::IsKeyPressed(ImGuiKey_Space, false)) playing_ = !playing_;
    if (ImGui::IsKeyPressed(ImGuiKey_Home, false)) fitRequested_ = true;
    if (ImGui::IsKeyPressed(ImGuiKey_Escape, false)) roiDragging_ = false;
}

void App::uiMenu() {
    if (!ImGui::BeginMainMenuBar()) return;
    if (ImGui::BeginMenu("File")) {
        if (ImGui::MenuItem("Open file(s)...", "Ctrl+O")) {
            auto files = platform::openFilesDialog();
            if (!files.empty()) openInputs(files);
        }
        if (ImGui::MenuItem("Open folder...", "Ctrl+Shift+O")) {
            auto dir = platform::openFolderDialog();
            if (!dir.empty()) openInputs({dir});
        }
        if (ImGui::MenuItem("Close", nullptr, false, s_ != nullptr)) closeSession();
        ImGui::Separator();
        if (ImGui::MenuItem("Exit", "Alt+F4")) glfwSetWindowShouldClose(window_, 1);
        ImGui::EndMenu();
    }
    if (ImGui::BeginMenu("View")) {
        if (ImGui::MenuItem("Fit map to window", "Home", false, s_ != nullptr)) fitRequested_ = true;
        if (ImGui::MenuItem("Reset layout")) layoutPending_ = true;
        ImGui::EndMenu();
    }
    if (ImGui::BeginMenu("Help")) {
        if (ImGui::MenuItem("Shortcuts and usage")) openHelpPopup_ = true;
        ImGui::EndMenu();
    }
    if (s_) {
        ImGui::SameLine(ImGui::GetWindowWidth() - 360);
        ImGui::TextDisabled("%s", s_->info->description.c_str());
    }
    ImGui::EndMainMenuBar();
}

void App::uiDockspace() {
    const ImGuiID dock = ImGui::DockSpaceOverViewport(0, nullptr, ImGuiDockNodeFlags_None);
    if (!layoutPending_) return;
    layoutPending_ = false;
    const ImGuiViewport* vp = ImGui::GetMainViewport();
    ImGui::DockBuilderRemoveNode(dock);
    ImGui::DockBuilderAddNode(dock, ImGuiDockNodeFlags_DockSpace);
    ImGui::DockBuilderSetNodeSize(dock, vp->WorkSize);
    ImGuiID left, rest, bottom, center, bottomLeft, bottomRight, leftTop, leftBottom;
    ImGui::DockBuilderSplitNode(dock, ImGuiDir_Left, 0.21f, &left, &rest);
    ImGui::DockBuilderSplitNode(left, ImGuiDir_Down, 0.30f, &leftBottom, &leftTop);
    ImGui::DockBuilderSplitNode(rest, ImGuiDir_Down, 0.36f, &bottom, &center);
    ImGui::DockBuilderSplitNode(bottom, ImGuiDir_Right, 0.34f, &bottomRight, &bottomLeft);
    ImGui::DockBuilderDockWindow("Layer", leftTop);
    ImGui::DockBuilderDockWindow("Performance", leftBottom);
    ImGui::DockBuilderDockWindow("Map", center);
    ImGui::DockBuilderDockWindow("Time series", bottomLeft);
    ImGui::DockBuilderDockWindow("Statistics", bottomRight);
    ImGui::DockBuilderFinish(dock);
}

void App::uiPopups() {
    if (openErrorPopup_) {
        ImGui::OpenPopup("Error");
        openErrorPopup_ = false;
    }
    if (ImGui::BeginPopupModal("Error", nullptr, ImGuiWindowFlags_AlwaysAutoResize)) {
        ImGui::PushTextWrapPos(560);
        ImGui::TextUnformatted(error_.c_str());
        ImGui::PopTextWrapPos();
        if (ImGui::Button("OK", ImVec2(120, 0))) ImGui::CloseCurrentPopup();
        ImGui::EndPopup();
    }
    if (openHelpPopup_) {
        ImGui::OpenPopup("Shortcuts and usage");
        openHelpPopup_ = false;
    }
    if (ImGui::BeginPopupModal("Shortcuts and usage", nullptr, ImGuiWindowFlags_AlwaysAutoResize)) {
        ImGui::TextUnformatted(
            "Open: Ctrl+O (files), Ctrl+Shift+O (folder), or drop onto the window\n"
            "Command line: tsv <folder | file.tif | pattern_*.tif ...> [--band N]\n\n"
            "Map\n"
            "  drag ................ pan\n"
            "  mouse wheel ......... zoom\n"
            "  hover ............... pixel series (exact once the mouse rests)\n"
            "  click ............... drop a pin (compare pixels)\n"
            "  right click on pin .. remove it (Delete removes the last one)\n"
            "  Shift + drag ........ rectangular ROI (mean and p10-p90 per date)\n"
            "  Home ................ fit to window\n\n"
            "Time\n"
            "  left/right arrows ... previous / next date\n"
            "  space ............... play / pause\n"
            "  click on the chart .. go to that date");
        if (ImGui::Button("Close", ImVec2(120, 0))) ImGui::CloseCurrentPopup();
        ImGui::EndPopup();
    }
}

// ---------------------------------------------------------------------------
// Map
// ---------------------------------------------------------------------------

void App::fitView(ImVec2 c) {
    const double W = s_->info->width, H = s_->info->height;
    scale_ = 0.98 * std::min(c.x / W, c.y / H);
    offset_ = ImVec2(float((c.x - W * scale_) * 0.5), float((c.y - H * scale_) * 0.5));
    mapDirty_ = true;
}

void App::renderMap(int w, int h) {
    const float bg[4] = {0.10f, 0.10f, 0.115f, 1.0f};
    gpu_.beginMap(w, h, bg);
    if (s_) {
        const CubeInfo& info = *s_->info;
        DrawParams p;
        p.mode = mode_;
        p.t = mode_ == ModeRGB ? rgb_[0] : t_;
        p.tg = rgb_[1];
        p.tb = rgb_[2];
        p.lo = range_[mode_].lo;
        p.hi = range_[mode_].hi;
        const auto& loaded = s_->gpu.loaded;
        bool ready = mode_ == ModeRGB ? loaded[rgb_[0]] && loaded[rgb_[1]] && loaded[rgb_[2]]
                     : modeIsTimeDependent(mode_) ? loaded[t_] : true;
        if (modeNeedsStats(mode_) && !s_->gpu.statsValid) ready = false;
        if (ready) {
            const float rect[4] = {offset_.x, offset_.y, float(offset_.x + info.width * scale_),
                                   float(offset_.y + info.height * scale_)};
            gpu_.drawCube(s_->gpu, rect, p);
        }
        if (mode_ == ModeValue && detailLevel_ >= 0) {
            const ViewRect v{-offset_.x / scale_, -offset_.y / scale_, (canvasSize_.x - offset_.x) / scale_,
                             (canvasSize_.y - offset_.y) / scale_, scale_};
            s_->tiles->forEachVisible(t_, v, [&](GLuint tex, double x, double y, double sw, double sh) {
                const float r[4] = {float(offset_.x + x * scale_), float(offset_.y + y * scale_),
                                    float(offset_.x + (x + sw) * scale_), float(offset_.y + (y + sh) * scale_)};
                gpu_.drawTile(tex, r, p);
            });
        }
    }
    gpu_.endMap();
}

void App::uiMap() {
    ImGui::PushStyleVar(ImGuiStyleVar_WindowPadding, ImVec2(6, 6));
    const bool open = ImGui::Begin("Map", nullptr, ImGuiWindowFlags_NoScrollbar | ImGuiWindowFlags_NoScrollWithMouse);
    ImGui::PopStyleVar();
    if (!open) {
        ImGui::End();
        return;
    }
    if (!s_ && opening_.valid()) {
        const std::string msg = "Opening " + (openingInputs_.size() == 1 ? openingInputs_[0]
                                              : std::to_string(openingInputs_.size()) + " inputs") + "...";
        const ImVec2 ts = ImGui::CalcTextSize(msg.c_str());
        const ImVec2 avail = ImGui::GetContentRegionAvail();
        ImGui::SetCursorPos(ImGui::GetCursorPos() + ImMax(ImVec2(0, 0), (avail - ts) * 0.5f));
        ImGui::TextDisabled("%s", msg.c_str());
        ImGui::End();
        return;
    }
    if (!s_) {
        const char* msg = "Open a time series: File > Open (Ctrl+O), drop files/a folder onto the window\n"
                          "or call it from the command line: tsv <folder | series.tif | files_*.tif>";
        const ImVec2 ts = ImGui::CalcTextSize(msg);
        const ImVec2 avail = ImGui::GetContentRegionAvail();
        ImGui::SetCursorPos(ImGui::GetCursorPos() + ImMax(ImVec2(0, 0), (avail - ts) * 0.5f));
        ImGui::TextDisabled("%s", msg);
        ImGui::End();
        return;
    }
    const CubeInfo& info = *s_->info;
    const int T = info.T();
    ImGuiIO& io = ImGui::GetIO();

    // --- Time bar ---
    if (ImGui::ArrowButton("##prev", ImGuiDir_Left)) setT(t_ - 1);
    ImGui::SameLine();
    if (ImGui::Button(playing_ ? "Pause" : " Play ", ImVec2(60, 0))) playing_ = !playing_;
    ImGui::SameLine();
    if (ImGui::ArrowButton("##next", ImGuiDir_Right)) setT(t_ + 1);
    ImGui::SameLine();
    ImGui::SetNextItemWidth(std::max(120.0f, ImGui::GetContentRegionAvail().x - 250));
    int t = t_;
    const std::string fmt = escapePercent(info.layers[t_].label);
    if (ImGui::SliderInt("##time", &t, 0, T - 1, fmt.c_str())) setT(t);
    ImGui::SameLine();
    ImGui::SetNextItemWidth(130);
    ImGui::SliderFloat("##fps", &fps_, 0.5f, 30.f, "%.1f dates/s", ImGuiSliderFlags_Logarithmic);
    ImGui::SameLine();
    ImGui::TextDisabled("%d/%d", t_ + 1, T);

    // --- Canvas ---
    const ImVec2 origin = ImGui::GetCursorScreenPos();
    const ImVec2 avail = ImGui::GetContentRegionAvail();
    const float statusH = ImGui::GetTextLineHeightWithSpacing();
    const ImVec2 size(std::max(avail.x, 64.0f), std::max(avail.y - statusH, 64.0f));
    if (size.x != canvasSize_.x || size.y != canvasSize_.y) {
        canvasSize_ = size;
        mapDirty_ = true;
        if (!viewTouched_) fitRequested_ = true; // e.g. the docking layout is still settling
    }
    if (fitRequested_) {
        fitView(size);
        fitRequested_ = false;
    }

    ImGui::InvisibleButton("##map", size, ImGuiButtonFlags_MouseButtonLeft);
    const bool hovered = ImGui::IsItemHovered();
    const bool active = ImGui::IsItemActive();
    const ImVec2 m = io.MousePos - origin;
    const double sx = (m.x - offset_.x) / scale_, sy = (m.y - offset_.y) / scale_;
    const int ix = int(std::floor(sx)), iy = int(std::floor(sy));
    const bool inside = hovered && ix >= 0 && iy >= 0 && ix < info.width && iy < info.height;

    if (ImGui::IsItemActivated() && io.KeyShift) {
        roiDragging_ = true;
        roiStart_ = roiEnd_ = ImVec2(float(sx), float(sy));
    }
    if (active) {
        if (roiDragging_) roiEnd_ = ImVec2(float(sx), float(sy));
        else if (ImGui::IsMouseDragging(ImGuiMouseButton_Left, 2.0f) && (io.MouseDelta.x || io.MouseDelta.y)) {
            offset_ += io.MouseDelta;
            mapDirty_ = true;
            viewTouched_ = true;
        }
    }
    if (ImGui::IsItemDeactivated()) {
        if (roiDragging_) {
            roiDragging_ = false;
            if (std::fabs(roiEnd_.x - roiStart_.x) >= 1 && std::fabs(roiEnd_.y - roiStart_.y) >= 1) {
                clearRoi();
                const int r[4] = {int(std::floor(roiStart_.x)), int(std::floor(roiStart_.y)), int(std::ceil(roiEnd_.x)),
                                  int(std::ceil(roiEnd_.y))};
                if (s_->deferRandomReads()) { // HDD still building: start when done
                    std::copy(r, r + 4, pendingRoiRect_);
                    pendingRoi_ = true;
                } else {
                    roi_ = s_->startRoi(r[0], r[1], r[2], r[3]);
                    roiSeen_ = -1;
                }
            }
        } else if (io.MouseDragMaxDistanceSqr[0] < 16 && inside) {
            addPin(ix, iy);
        }
    }
    if (hovered && ImGui::IsMouseClicked(ImGuiMouseButton_Right)) { // remove the nearest pin
        int best = -1;
        float bestD = 12.0f * 12.0f;
        for (size_t i = 0; i < pins_.size(); ++i) {
            const ImVec2 c = origin + ImVec2(float(offset_.x + (pins_[i].x + 0.5) * scale_),
                                             float(offset_.y + (pins_[i].y + 0.5) * scale_));
            const ImVec2 d = c - io.MousePos;
            if (d.x * d.x + d.y * d.y < bestD) {
                bestD = d.x * d.x + d.y * d.y;
                best = int(i);
            }
        }
        if (best >= 0) pins_.erase(pins_.begin() + best);
    }
    if (hovered && !pins_.empty() &&
        (ImGui::IsKeyPressed(ImGuiKey_Delete, false) || ImGui::IsKeyPressed(ImGuiKey_Backspace, false)))
        pins_.pop_back();
    if (hovered && io.MouseWheel != 0) {
        const double fit = std::min(size.x / info.width, size.y / info.height);
        const double ns = std::clamp(scale_ * std::pow(1.25, io.MouseWheel), fit * 0.05, 64.0);
        const double f = ns / scale_;
        offset_ = ImVec2(float(m.x - (m.x - offset_.x) * f), float(m.y - (m.y - offset_.y) * f));
        scale_ = ns;
        mapDirty_ = true;
        viewTouched_ = true;
    }
    if (inside && (ix != hover_.x || iy != hover_.y)) {
        hover_.x = ix;
        hover_.y = iy;
        hover_.values = approxSeries(ix, iy);
        hover_.exact = false;
        hover_.stats = computeSeriesStats(years_, hover_.values);
        hover_.request = 0;
        hover_.color = ImVec4(0.95f, 0.95f, 0.95f, 1);
        approxLayers_ = s_->overview.layersDone();
        hoverPending_ = true;
        hoverSince_ = ImGui::GetTime();
    }

    // --- Detail tiles ("value" mode only) ---
    int level = -1;
    if (detail_ && mode_ == ModeValue && !s_->deferRandomReads()) {
        const ViewRect v{-offset_.x / scale_, -offset_.y / scale_, (size.x - offset_.x) / scale_,
                         (size.y - offset_.y) / scale_, scale_};
        level = s_->tiles->update(t_, v, playing_ ? (t_ + 1) % T : -1);
    }
    if (level != detailLevel_) {
        detailLevel_ = level;
        mapDirty_ = true;
    }

    if (mapDirty_) {
        renderMap(int(size.x), int(size.y));
        mapDirty_ = false;
    }

    // --- Drawing + overlays ---
    ImDrawList* dl = ImGui::GetWindowDrawList();
    dl->AddImage(ImTextureRef((ImTextureID)(intptr_t)gpu_.mapTexture()), origin, origin + size, ImVec2(0, 1),
                 ImVec2(1, 0));
    dl->PushClipRect(origin, origin + size, true);
    auto toScreen = [&](double x, double y) {
        return origin + ImVec2(float(offset_.x + x * scale_), float(offset_.y + y * scale_));
    };
    if (inside && scale_ >= 6) // outline of the pixel under the cursor
        dl->AddRect(toScreen(ix, iy), toScreen(ix + 1, iy + 1), IM_COL32(255, 255, 255, 200));
    for (size_t i = 0; i < pins_.size(); ++i) {
        const ImVec2 c = toScreen(pins_[i].x + 0.5, pins_[i].y + 0.5);
        // Dark ring + white ring: visible over any colormap.
        dl->AddCircle(c, 8.5f, IM_COL32(0, 0, 0, 220), 0, 3.0f);
        dl->AddCircleFilled(c, 6.5f, ImGui::ColorConvertFloat4ToU32(pins_[i].color));
        dl->AddCircle(c, 6.5f, IM_COL32(255, 255, 255, 255), 0, 1.5f);
        char num[8];
        std::snprintf(num, sizeof(num), "%d", pins_[i].id);
        const ImVec2 tp = c + ImVec2(11, -18), ts = ImGui::CalcTextSize(num);
        dl->AddRectFilled(tp - ImVec2(3, 1), tp + ts + ImVec2(3, 1), IM_COL32(0, 0, 0, 190), 3.0f);
        dl->AddText(tp, ImGui::ColorConvertFloat4ToU32(pins_[i].color), num);
    }
    const ImU32 roiCol = IM_COL32(255, 210, 60, 255);
    if (roiDragging_) dl->AddRect(toScreen(roiStart_.x, roiStart_.y), toScreen(roiEnd_.x, roiEnd_.y), roiCol, 0.0f, ImDrawFlags_None, 2.0f);
    else if (pendingRoi_) {
        const ImVec2 a = toScreen(pendingRoiRect_[0], pendingRoiRect_[1]);
        dl->AddRect(a, toScreen(pendingRoiRect_[2], pendingRoiRect_[3]), roiCol, 0.0f, ImDrawFlags_None, 1.0f);
        dl->AddText(a + ImVec2(4, 2), roiCol, "ROI: waiting for the overview (HDD)");
    }
    else if (roi_) {
        dl->AddRect(toScreen(roi_->x0, roi_->y0), toScreen(roi_->x1, roi_->y1), roiCol, 0.0f, ImDrawFlags_None, 2.0f);
        if (roi_->done < T) {
            char buf[64];
            std::snprintf(buf, sizeof(buf), "ROI %d/%d", roi_->done.load(), T);
            dl->AddText(toScreen(roi_->x0, roi_->y0) + ImVec2(4, 2), roiCol, buf);
        }
    }

    // View title (date + mode) and color bar
    char title[160];
    if (mode_ == ModeRGB)
        std::snprintf(title, sizeof(title), "RGB: %s / %s / %s", info.layers[rgb_[0]].label.c_str(),
                      info.layers[rgb_[1]].label.c_str(), info.layers[rgb_[2]].label.c_str());
    else if (modeIsTimeDependent(mode_))
        std::snprintf(title, sizeof(title), "%s  |  %s", info.layers[t_].label.c_str(), kModeNames[mode_]);
    else
        std::snprintf(title, sizeof(title), "%s  |  %d dates", kModeNames[mode_], T);
    dl->AddText(origin + ImVec2(11, 9), IM_COL32(0, 0, 0, 200), title);
    dl->AddText(origin + ImVec2(10, 8), IM_COL32(255, 255, 255, 255), title);

    std::string wait;
    if (modeNeedsStats(mode_) && !s_->gpu.statsValid)
        wait = "Computing statistics: waiting for the complete overview...";
    else if (modeIsTimeDependent(mode_) && !s_->gpu.loaded[mode_ == ModeRGB ? rgb_[0] : t_])
        wait = "Loading this date...";
    if (!wait.empty()) {
        const ImVec2 ts = ImGui::CalcTextSize(wait.c_str());
        dl->AddText(origin + (size - ts) * 0.5f, IM_COL32(220, 220, 220, 255), wait.c_str());
    }
    if (!s_->overview.complete()) {
        char buf[96];
        std::snprintf(buf, sizeof(buf), "Building overview %d/%d%s", s_->overview.layersDone(), T,
                      s_->overview.fromCache() ? " (cache)" : "");
        const float frac = float(s_->overview.layersDone()) / T;
        const ImVec2 p0 = origin + ImVec2(size.x - 250, 10), p1 = p0 + ImVec2(240, 18);
        dl->AddRectFilled(p0, p1, IM_COL32(0, 0, 0, 160), 3);
        dl->AddRectFilled(p0, ImVec2(p0.x + 240 * frac, p1.y), IM_COL32(70, 130, 220, 220), 3);
        dl->AddText(p0 + ImVec2(6, 2), IM_COL32(255, 255, 255, 255), buf);
    }
    if (mode_ != ModeRGB) {
        const ImVec2 b0 = origin + ImVec2(10, size.y - 34), bsz(220, 10);
        const int n = 48;
        for (int i = 0; i < n; ++i) {
            const ImVec4 c = ImPlot::SampleColormap((i + 0.5f) / n, cmap_[mode_]);
            dl->AddRectFilled(b0 + ImVec2(bsz.x * i / n, 0), b0 + ImVec2(bsz.x * (i + 1) / n, bsz.y),
                              ImGui::ColorConvertFloat4ToU32(c));
        }
        dl->AddRect(b0, b0 + bsz, IM_COL32(0, 0, 0, 255));
        char lo[32], hi[32];
        std::snprintf(lo, sizeof(lo), "%.4g", range_[mode_].lo);
        std::snprintf(hi, sizeof(hi), "%.4g", range_[mode_].hi);
        dl->AddText(b0 + ImVec2(0, 12), IM_COL32(230, 230, 230, 255), lo);
        dl->AddText(b0 + ImVec2(bsz.x - ImGui::CalcTextSize(hi).x, 12), IM_COL32(230, 230, 230, 255), hi);
    }
    dl->PopClipRect();

    // --- Status bar ---
    char status[256];
    int n = std::snprintf(status, sizeof(status), "zoom %.1f%%", scale_ * 100.0);
    if (detailLevel_ >= 0)
        n += std::snprintf(status + n, sizeof(status) - n, " (detail 1:%d)", 1 << detailLevel_);
    else
        n += std::snprintf(status + n, sizeof(status) - n, " (overview 1:%.1f)", s_->overview.factor);
    if (inside) {
        n += std::snprintf(status + n, sizeof(status) - n, "  |  col %d  row %d", ix, iy);
        double gx, gy;
        if (info.pixelToGeo(ix + 0.5, iy + 0.5, gx, gy))
            n += std::snprintf(status + n, sizeof(status) - n, "  |  x %.6f  y %.6f", gx, gy);
        if (modeNeedsStats(mode_) && s_->gpu.statsValid) {
            // Displayed quantity (mean, trend...) at the overview pixel under the cursor.
            const Overview& ov = s_->overview;
            const size_t p = size_t(std::min(ov.h - 1, int(double(iy) * ov.h / info.height))) * ov.w +
                             size_t(std::min(ov.w - 1, int(double(ix) * ov.w / info.width)));
            const float* a = &s_->stats0[p * 4];
            const float* b = &s_->stats1[p * 4];
            const float v[ModeCount] = {0, 0, a[0], a[1], a[2], b[0], b[1], b[1] - b[0], b[2], 0};
            if (mode_ == ModeAnomaly && hover_.x == ix && hover_.y == iy)
                std::snprintf(status + n, sizeof(status) - n, "  |  anomaly %.6g", hover_.values[t_] - a[0]);
            else if (mode_ != ModeAnomaly)
                std::snprintf(status + n, sizeof(status) - n, "  |  %s %.6g%s", kModeNames[mode_], v[mode_],
                              mode_ == ModeSlope ? slopeUnit().c_str() : "");
        } else if (hover_.x == ix && hover_.y == iy && t_ < int(hover_.values.size())) {
            std::snprintf(status + n, sizeof(status) - n, "  |  value %.6g %s", hover_.values[t_],
                          hover_.exact ? "" : "(approx.)");
        }
    }
    ImGui::TextUnformatted(status);
    ImGui::End();
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

void App::uiLayer() {
    if (!ImGui::Begin("Layer")) {
        ImGui::End();
        return;
    }
    if (!s_) {
        ImGui::TextWrapped("No series open.");
        if (ImGui::Button("Open file(s)...", ImVec2(-1, 0))) {
            auto files = platform::openFilesDialog();
            if (!files.empty()) openInputs(files);
        }
        if (ImGui::Button("Open folder...", ImVec2(-1, 0))) {
            auto dir = platform::openFolderDialog();
            if (!dir.empty()) openInputs({dir});
        }
        ImGui::End();
        return;
    }
    const CubeInfo& info = *s_->info;
    const int T = info.T();

    ImGui::SeparatorText("Series");
    ImGui::TextWrapped("%s", info.description.c_str());
    ImGui::Text("%d dates: %s to %s", T, info.layers.front().label.c_str(), info.layers.back().label.c_str());
    ImGui::Text("%d x %d px, %s", info.width, info.height, info.dataType.c_str());
    ImGui::TextWrapped("CRS: %s %s", info.crsName.empty() ? "(no CRS)" : info.crsName.c_str(),
                       info.crsAuthority.c_str());
    if (info.hasGeoTransform)
        ImGui::Text("Pixel: %.6g x %.6g", info.geoTransform[1], std::fabs(info.geoTransform[5]));

    ImGui::SeparatorText("Display");
    ImGui::SetNextItemWidth(-1);
    if (ImGui::BeginCombo("##mode", kModeNames[mode_], ImGuiComboFlags_HeightLargest)) {
        for (int m = 0; m < ModeCount; ++m) {
            ImGui::BeginDisabled(!modeAvailable(m));
            if (ImGui::Selectable(kModeNames[m], m == mode_)) {
                mode_ = m;
                mapDirty_ = true;
            }
            ImGui::EndDisabled();
        }
        ImGui::EndCombo();
    }
    if (modeNeedsStats(mode_)) {
        ImGui::TextDisabled(s_->gpu.statsValid ? "Computed on the GPU for every pixel (overview)."
                                                : "Waiting for the complete overview...");
        if (mode_ == ModeSlope) ImGui::TextDisabled("Unit: value%s", slopeUnit().c_str());
    }

    if (mode_ == ModeRGB) {
        const char* names[3] = {"R", "G", "B"};
        for (int c = 0; c < 3; ++c) {
            ImGui::SetNextItemWidth(-30);
            const std::string f = escapePercent(info.layers[rgb_[c]].label);
            if (ImGui::SliderInt(names[c], &rgb_[c], 0, T - 1, f.c_str())) mapDirty_ = true;
        }
    } else {
        ImGui::SetNextItemWidth(-1);
        if (ImPlot::ColormapButton(ImPlot::GetColormapName(cmap_[mode_]), ImVec2(-1, 0), cmap_[mode_]))
            ImGui::OpenPopup("colormaps");
        if (ImGui::BeginPopup("colormaps")) {
            for (int c = 4; c < ImPlot::GetColormapCount(); ++c) // 0-3 are qualitative
                if (ImPlot::ColormapButton(ImPlot::GetColormapName(c), ImVec2(220, 0), c)) {
                    cmap_[mode_] = c;
                    ImGui::CloseCurrentPopup();
                }
            ImGui::EndPopup();
        }
    }

    Range& r = range_[mode_];
    float lo = r.lo, hi = r.hi;
    ImGui::SetNextItemWidth(-60);
    if (ImGui::DragFloatRange2("##range", &lo, &hi, std::max(1e-6f, (r.hi - r.lo) / 300.f), 0, 0, "%.4g", "%.4g")) {
        r.lo = lo;
        r.hi = std::max(hi, lo + 1e-6f);
        r.manual = true;
        mapDirty_ = true;
    }
    ImGui::SameLine();
    if (ImGui::Button("Auto", ImVec2(-1, 0))) {
        r.manual = false;
        r.key = ~0ull;
    }
    ImGui::SetItemTooltip("Automatic range: 2-98%% percentiles (symmetric for anomaly and trend)");
    if (modeIsTimeDependent(mode_)) {
        if (ImGui::Checkbox("Per-date range", &perDateRange_)) r.key = ~0ull;
        ImGui::SetItemTooltip("Off: same range for every date (colors comparable through time)");
    }

    if (!histX_.empty() && ImPlot::BeginPlot("##hist", ImVec2(-1, 130), ImPlotFlags_CanvasOnly)) {
        ImPlot::SetupAxes(nullptr, nullptr, ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit | ImPlotAxisFlags_NoTickLabels);
        ImPlotSpec bars;
        bars.FillColor = ImVec4(0.45f, 0.62f, 0.90f, 1);
        bars.LineColor = ImVec4(0, 0, 0, 0);
        ImPlot::PlotBars("##h", histX_.data(), histY_.data(), int(histX_.size()), histBarW_, bars);
        ImPlotSpec lines;
        lines.LineColor = ImVec4(1.0f, 0.6f, 0.2f, 1);
        lines.LineWeight = 1.5f;
        const double lim[2] = {r.lo, r.hi};
        ImPlot::PlotInfLines("##lim", lim, 2, lines);
        ImPlot::EndPlot();
    }

    ImGui::SeparatorText("Detail");
    if (ImGui::Checkbox("Full resolution when zoomed in", &detail_)) mapDirty_ = true;
    ImGui::SetItemTooltip("Past the overview resolution, reads tiles of the visible area\n"
                          "in the background ('Value at date' mode only).");
    ImGui::End();
}

void App::uiSeries() {
    if (!ImGui::Begin("Time series")) {
        ImGui::End();
        return;
    }
    if (!s_) {
        ImGui::TextDisabled("Open a series and hover over the map.");
        ImGui::End();
        return;
    }
    const CubeInfo& info = *s_->info;
    const int T = info.T();

    // --- Chart options ---
    ImGui::SetNextItemWidth(140);
    ImGui::Combo("##style", &plotStyle_, "Lines\0Lines + markers\0Markers\0Stems\0Stairs\0");
    ImGui::SetItemTooltip("Series style");
    ImGui::SameLine();
    ImGui::SetNextItemWidth(170);
    ImGui::Combo("##values", &plotValues_, "Values\0Anomaly (- mean)\0Standardized (z-score)\0");
    ImGui::SetItemTooltip("Values: as stored in the raster\n"
                          "Anomaly: each series minus its own mean (compares shape, not level)\n"
                          "Z-score: (value - mean) / std (compares series of different scales)");
    ImGui::SameLine();
    ImGui::SetNextItemWidth(150);
    ImGui::Combo("##trend", &trendKind_, "No trend\0OLS trend\0Sen trend\0");
    ImGui::SetItemTooltip("Trend line of each series (same color, thinner)\n"
                          "OLS: least squares, sensitive to outliers\n"
                          "Sen: median of pairwise slopes, robust to outliers");
    ImGui::SameLine();
    ImGui::BeginDisabled(plotValues_ != 0);
    ImGui::Checkbox("Y = map range", &yFromMap_);
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Locks the Y axis to the map's color range (compare without the axis jumping)");
    if (roi_) {
        ImGui::SameLine();
        ImGui::Checkbox("p10-p90 band", &showRoiBand_);
    }
    ImGui::SameLine();
    if (ImGui::Button("Copy CSV")) copyCsv();
    ImGui::SetItemTooltip("Copies the series (cursor, pins, ROI) to the clipboard");
    if (!pins_.empty()) {
        ImGui::SameLine();
        if (ImGui::Button("Clear pins")) pins_.clear();
    }
    if (roi_) {
        ImGui::SameLine();
        if (ImGui::Button("Clear ROI")) clearRoi();
    }
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    ImGui::SetItemTooltip("Chart lines:\n"
                          "  white ............. pixel under the cursor\n"
                          "  colored ........... pins (same number and color as on the map)\n"
                          "  yellow + band ..... ROI mean and p10-p90\n"
                          "  thin, same color .. each series' trend (OLS or Sen)\n"
                          "  orange vertical ... current date (drag or click to change)\n\n"
                          "Click a legend entry to hide/show that series.\n"
                          "Pins: right click the pin on the map, Delete (last one)\n"
                          "or the x in the statistics table removes them.");

    ImGui::PushID(int(info.id));
    if (ImPlot::BeginPlot("##series", ImVec2(-1, -1), ImPlotFlags_NoTitle)) {
        const bool lockY = yFromMap_ && plotValues_ == 0;
        ImPlot::SetupAxes(info.timeIsDate ? nullptr : "date (index)",
                          plotValues_ == 1 ? "anomaly" : plotValues_ == 2 ? "z-score" : nullptr,
                          ImPlotAxisFlags_None, lockY ? ImPlotAxisFlags_None : ImPlotAxisFlags_AutoFit);
        if (info.timeIsDate) ImPlot::SetupAxisScale(ImAxis_X1, ImPlotScale_Time);
        const double pad = T > 1 ? (xs_.back() - xs_.front()) * 0.03 : 1.0;
        ImPlot::SetupAxisLimits(ImAxis_X1, xs_.front() - pad, xs_.back() + pad, ImPlotCond_Once);
        if (lockY) {
            const Range& r = range_[ModeValue];
            const double m = (r.hi - r.lo) * 0.05;
            ImPlot::SetupAxisLimits(ImAxis_Y1, r.lo - m, r.hi + m, ImPlotCond_Always);
        }
        ImPlot::SetupLegend(ImPlotLocation_NorthWest);

        // Displayed value: raw, anomaly or z-score (using the series' own statistics).
        auto transform = [&](double v, const SeriesStats& st) {
            if (plotValues_ == 1) return v - st.mean;
            if (plotValues_ == 2) return st.std > 0 ? (v - st.mean) / st.std : 0.0;
            return v;
        };
        auto plotSeries = [&](const char* label, const std::vector<float>& vals, const SeriesStats& st, ImVec4 col,
                              float weight, bool trend = true) {
            if (int(vals.size()) != T) return;
            std::vector<double> ys(T);
            for (int t = 0; t < T; ++t) ys[t] = transform(vals[t], st);
            ImPlotSpec spec;
            spec.LineColor = col;
            spec.LineWeight = weight;
            spec.MarkerFillColor = col;
            switch (plotStyle_) {
            case 0: ImPlot::PlotLine(label, xs_.data(), ys.data(), T, spec); break;
            case 1:
                spec.Marker = ImPlotMarker_Circle;
                spec.MarkerSize = 2.5f;
                ImPlot::PlotLine(label, xs_.data(), ys.data(), T, spec);
                break;
            case 2:
                spec.Marker = ImPlotMarker_Circle;
                spec.MarkerSize = 3.5f;
                ImPlot::PlotScatter(label, xs_.data(), ys.data(), T, spec);
                break;
            case 3:
                spec.Marker = ImPlotMarker_Circle;
                spec.MarkerSize = 2.5f;
                ImPlot::PlotStems(label, xs_.data(), ys.data(), T, plotValues_ == 0 ? st.mean : 0.0, spec);
                break;
            default: ImPlot::PlotStairs(label, xs_.data(), ys.data(), T, spec); break;
            }
            if (trend && trendKind_ != 0 && st.n >= 2) {
                // Same label = same legend entry (hidden together with the series).
                const double a = trendKind_ == 1 ? st.olsIntercept : st.senIntercept;
                const double b = trendKind_ == 1 ? st.olsSlope : st.senSlope;
                const double tx[2] = {xs_.front(), xs_.back()};
                const double ty[2] = {transform(a + b * years_.front(), st), transform(a + b * years_.back(), st)};
                ImPlotSpec ts;
                ts.LineColor = ImVec4(col.x, col.y, col.z, 0.6f);
                ts.LineWeight = 1.2f;
                ImPlot::PlotLine(label, tx, ty, 2, ts);
            }
        };

        if (!roiMean_.empty()) {
            const ImVec4 yellow(1.0f, 0.82f, 0.24f, 1);
            const char* label = roi_ && roi_->sampled ? "ROI mean (subsampled)" : "ROI mean";
            if (showRoiBand_) {
                std::vector<double> a(T), b(T);
                for (int t = 0; t < T; ++t) {
                    a[t] = transform(roiP10_[t], roiStats_);
                    b[t] = transform(roiP90_[t], roiStats_);
                }
                ImPlotSpec spec;
                spec.FillColor = yellow;
                spec.FillAlpha = 0.18f;
                ImPlot::PlotShaded(label, xs_.data(), a.data(), b.data(), T, spec);
            }
            // The trend of a partially computed ROI would be misleading: wait for every date.
            plotSeries(label, roiMean_, roiStats_, yellow, 2.0f, roi_ && roi_->done == T);
        }
        for (const SeriesView& p : pins_) {
            char label[64];
            std::snprintf(label, sizeof(label), "Pin %d (%d, %d)%s###pin%d", p.id, p.x, p.y,
                          p.exact ? "" : " approx.", p.id);
            plotSeries(label, p.values, p.stats, p.color, 1.5f);
        }
        if (hover_.x >= 0) {
            char label[64];
            std::snprintf(label, sizeof(label), "Cursor (%d, %d)%s###cursor", hover_.x, hover_.y,
                          hover_.exact ? "" : " approx.");
            plotSeries(label, hover_.values, hover_.stats, hover_.color, 2.0f);
        }

        double tx = xs_[t_];
        const ImVec4 orange(1.0f, 0.6f, 0.2f, 1);
        if (ImPlot::DragLineX(0, &tx, orange, 1.5f, ImPlotDragToolFlags_NoFit)) setT(nearestIndex(xs_, tx));
        ImPlot::TagX(xs_[t_], orange, "%s", info.layers[t_].label.c_str());
        if (ImPlot::IsPlotHovered() && ImGui::IsMouseReleased(ImGuiMouseButton_Left) &&
            ImGui::GetIO().MouseDragMaxDistanceSqr[0] < 16)
            setT(nearestIndex(xs_, ImPlot::GetPlotMousePos().x));
        ImPlot::EndPlot();
    }
    ImGui::PopID();
    ImGui::End();
}

void App::uiStats() {
    if (!ImGui::Begin("Statistics")) {
        ImGui::End();
        return;
    }
    if (!s_) {
        ImGui::End();
        return;
    }
    const CubeInfo& info = *s_->info;
    struct Col {
        std::string name;
        const SeriesStats* st;
        ImVec4 color;
        int pin;                 // index into pins_ (-1 = not a pin)
        bool roi;
    };
    std::vector<Col> cols;
    if (hover_.x >= 0) cols.push_back({hover_.exact ? "Cursor" : "Cursor*", &hover_.stats, hover_.color, -1, false});
    for (size_t i = 0; i < pins_.size(); ++i)
        cols.push_back({"Pin " + std::to_string(pins_[i].id) + (pins_[i].exact ? "" : "*"), &pins_[i].stats,
                        pins_[i].color, int(i), false});
    if (!roiMean_.empty()) cols.push_back({"ROI mean", &roiStats_, ImVec4(1.0f, 0.82f, 0.24f, 1), -1, true});
    if (cols.empty()) {
        ImGui::TextDisabled("Hover over the map, click to drop pins\nor Shift+drag for an ROI.");
        ImGui::End();
        return;
    }

    const std::string unit = slopeUnit();
    auto date = [&](int idx) { return idx >= 0 ? info.layers[idx].label : std::string("-"); };
    auto num = [](double v) {
        char b[32];
        std::snprintf(b, sizeof(b), "%.5g", v);
        return std::string(b);
    };
    struct Row {
        const char* label;
        std::function<std::string(const SeriesStats&)> f;
    };
    const Row rows[] = {
        {"Valid n", [](const SeriesStats& s) { return std::to_string(s.n); }},
        {"Mean", [&](const SeriesStats& s) { return num(s.mean); }},
        {"Median", [&](const SeriesStats& s) { return num(s.median); }},
        {"Std. deviation", [&](const SeriesStats& s) { return num(s.std); }},
        {"CV", [&](const SeriesStats& s) { return num(s.cv); }},
        {"Minimum", [&](const SeriesStats& s) { return num(s.min) + " (" + date(s.argMin) + ")"; }},
        {"Maximum", [&](const SeriesStats& s) { return num(s.max) + " (" + date(s.argMax) + ")"; }},
        {"Amplitude", [&](const SeriesStats& s) { return num(s.max - s.min); }},
        {"OLS trend", [&](const SeriesStats& s) { return num(s.olsSlope) + unit; }},
        {"R\xC2\xB2 (OLS)", [&](const SeriesStats& s) { return num(s.r2); }},
        {"Sen's slope", [&](const SeriesStats& s) { return num(s.senSlope) + unit; }},
        {"Mann-Kendall Z", [&](const SeriesStats& s) { return num(s.mkZ); }},
        {"p-value (MK)", [&](const SeriesStats& s) { return num(s.mkP); }},
        {"Trend (5%)", [](const SeriesStats& s) -> std::string {
             if (s.n < 3) return "-";
             if (s.mkP >= 0.05) return "not significant";
             return s.mkZ > 0 ? "increasing" : "decreasing";
         }},
    };

    const ImGuiTableFlags flags = ImGuiTableFlags_Borders | ImGuiTableFlags_RowBg | ImGuiTableFlags_ScrollX |
                                  ImGuiTableFlags_ScrollY | ImGuiTableFlags_SizingFixedFit;
    if (ImGui::BeginTable("stats", 1 + int(cols.size()), flags)) {
        ImGui::TableSetupScrollFreeze(1, 1);
        ImGui::TableSetupColumn("Metric");
        for (const Col& c : cols) ImGui::TableSetupColumn(c.name.c_str());
        ImGui::TableNextRow(ImGuiTableRowFlags_Headers);
        ImGui::TableNextColumn();
        ImGui::TableHeader("Metric");
        int removePin = -1;
        bool removeRoi = false;
        for (size_t ci = 0; ci < cols.size(); ++ci) {
            const Col& c = cols[ci];
            ImGui::TableNextColumn();
            ImGui::PushID(int(ci));
            if (c.pin >= 0 || c.roi) {
                ImGui::PushStyleVar(ImGuiStyleVar_FramePadding, ImVec2(3, 0));
                if (ImGui::SmallButton("x")) {
                    if (c.roi) removeRoi = true;
                    else removePin = c.pin;
                }
                ImGui::PopStyleVar();
                ImGui::SetItemTooltip(c.roi ? "Remove the ROI" : "Remove this pin");
                ImGui::SameLine(0, ImGui::GetStyle().ItemInnerSpacing.x);
            }
            ImGui::PushStyleColor(ImGuiCol_Text, c.color);
            ImGui::TableHeader(c.name.c_str());
            ImGui::PopStyleColor();
            ImGui::PopID();
        }
        for (const Row& row : rows) {
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextDisabled("%s", row.label);
            for (const Col& c : cols) {
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(row.f(*c.st).c_str());
            }
        }
        ImGui::EndTable();
        // Remove after drawing: the pointers in `cols` point into pins_.
        if (removePin >= 0) pins_.erase(pins_.begin() + removePin);
        if (removeRoi) clearRoi();
    }
    ImGui::End();
}

void App::uiPerf() {
    if (!ImGui::Begin("Performance")) {
        ImGui::End();
        return;
    }
    ImGuiIO& io = ImGui::GetIO();
    ImGui::Text("Frame: %.2f ms of CPU", frameMs_);
    ImGui::SetItemTooltip("The app only redraws when something changes (mouse, keyboard, new data).\n"
                          "Idle, the CPU sleeps; while animating it runs at %.0f frames/s.", io.Framerate);
    ImGui::Text("Startup: window %.0f ms, first frame %.0f ms", startup_.windowMs, startup_.firstFrameMs);
    ImGui::SetItemTooltip("Since process start: GDAL %.0f ms, window + OpenGL %.0f ms, UI %.0f ms,\n"
                          "opening inputs %.0f ms, first frame %.0f ms",
                          startup_.gdalMs, startup_.windowMs, startup_.uiMs, startup_.openMs, startup_.firstFrameMs);
    if (s_) {
        const Overview& ov = s_->overview;
        const double MB = 1024.0 * 1024.0;
        ImGui::Text("Disk: %s", s_->rotational ? "spinning HDD" : "SSD / network");
        ImGui::Text("Threads: %d background readers + %d interactive", s_->bgPool().threads(), s_->fgPool().threads());
        ImGui::Text("Open (metadata): %.0f ms", s_->openSeconds * 1000);
        ImGui::SeparatorText("Overview (GPU)");
        ImGui::Text("%d x %d x %d dates (1:%.1f)", ov.w, ov.h, ov.T, ov.factor);
        ImGui::Text("Memory: %.0f MB (cube + statistics)", s_->gpu.bytes() / MB);
        if (ov.complete())
            ImGui::Text(ov.fromCache() ? "Read from cache in %.2f s" : "Built in %.2f s (saved to cache)",
                        ov.buildSeconds());
        else
            ImGui::Text("Loading %d/%d...", ov.layersDone(), ov.T);
        if (s_->deferRandomReads())
            ImGui::TextDisabled("Pins, ROI and detail wait for the build (HDD)");
        if (ov.failedLayers()) ImGui::TextColored(ImVec4(1, 0.5f, 0.4f, 1), "%d dates failed", ov.failedLayers());
        if (s_->gpu.statsValid) ImGui::Text("Temporal statistics: %.1f ms", s_->statsMs);
        ImGui::SeparatorText("On-demand reads");
        ImGui::Text("Tiles: %d on GPU (%.0f MB), %d reading", s_->tiles->gpuTiles(), s_->tiles->gpuBytes() / MB,
                    s_->tiles->inflight());
        ImGui::Text("Average tile: %.1f ms", s_->tiles->avgLoadMs());
        ImGui::Text("Exact series (%d dates): %.1f ms", s_->info->T(), lastSeriesMs_);
        if (roi_ && roi_->done == s_->info->T()) ImGui::Text("ROI: %.0f ms", roi_->ms.load());
        ImGui::Text("Queue: %d (background), %d (interactive)", s_->bgPool().pending(), s_->fgPool().pending());
    }
    ImGui::SeparatorText("Settings");
    ImGui::SetNextItemWidth(-1);
    ImGui::SliderInt("##budget", &budgetUi_, 128, 8192, "Overview: %d MB", ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("GPU memory for the cube overview. Larger = more resolution\nwithout tiles, but slower to build.");
    if (budgetUi_ != opts_.budgetMB) {
        if (ImGui::Button(s_ ? "Apply (reopens the series)" : "Apply", ImVec2(-1, 0))) {
            opts_.budgetMB = budgetUi_;
            settings_.overviewBudgetBytes = int64_t(budgetUi_) << 20;
            if (s_) openInputs(std::vector<std::string>(lastInputs_));
        }
    }
    if (ImGui::Button("Clear overview cache", ImVec2(-1, 0))) {
        std::error_code ec;
        const std::string keep = s_ ? fs::u8path(s_->overview.cachePath()).filename().u8string() : "";
        for (auto& e : fs::directory_iterator(fs::u8path(settings_.cacheDir), ec))
            if (e.path().extension() == ".tsvcube" && e.path().filename().u8string() != keep) fs::remove(e.path(), ec);
    }
    ImGui::SetItemTooltip("%s", settings_.cacheDir.c_str());
    ImGui::End();
}

void App::copyCsv() {
    if (!s_) return;
    const CubeInfo& info = *s_->info;
    std::string csv = "date";
    if (hover_.x >= 0) csv += ",cursor_" + std::to_string(hover_.x) + "_" + std::to_string(hover_.y);
    for (size_t i = 0; i < pins_.size(); ++i)
        csv += ",pin" + std::to_string(pins_[i].id) + "_" + std::to_string(pins_[i].x) + "_" + std::to_string(pins_[i].y);
    if (!roiMean_.empty()) csv += ",roi_mean,roi_p10,roi_p90";
    csv += "\n";
    auto add = [&](float v) {
        char b[32];
        std::snprintf(b, sizeof(b), ",%.7g", v);
        csv += std::isnan(v) ? std::string(",") : std::string(b);
    };
    for (int t = 0; t < info.T(); ++t) {
        csv += info.layers[t].label;
        if (hover_.x >= 0) add(hover_.values[t]);
        for (const SeriesView& p : pins_) add(p.values[t]);
        if (!roiMean_.empty()) {
            add(roiMean_[t]);
            add(roiP10_[t]);
            add(roiP90_[t]);
        }
        csv += "\n";
    }
    ImGui::SetClipboardText(csv.c_str());
}
