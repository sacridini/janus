#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <functional>
#include <utility>

#include <imgui_internal.h>
#include <implot.h>

#include "glfw.hpp"
#include "render_backend.hpp"
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

// Marker shape per layer, so the series of different layers can be told apart.
ImPlotMarker layerMarker(int i) {
    static const ImPlotMarker m[] = {ImPlotMarker_Circle, ImPlotMarker_Square, ImPlotMarker_Diamond,
                                     ImPlotMarker_Up,     ImPlotMarker_Down,   ImPlotMarker_Cross};
    return m[i % 6];
}

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
    for (auto& j : jobs_) j->cancel(); // don't leave orphan processes behind
    jobs_.clear();
    zeit_.reset();
    closeAll();
    gpu_.shutdown();
}

bool App::init(const AppOptions& opts, std::string& error) {
    opts_ = opts;
    budgetUi_ = int(opts.budgetMB);
    if (!gpu_.init(error)) return false;

    const std::string appData = platform::appDataDir();
    settings_.cacheDir = platform::cacheDir();
    std::error_code ec;
    fs::create_directories(fs::u8path(settings_.cacheDir), ec);
    Overview::pruneCache(settings_.cacheDir, 20ull << 30);
    settings_.overviewBudgetBytes = opts.budgetMB << 20;
    settings_.ioThreads = opts.ioThreads;
    settings_.maxTexSize = std::min<int>(gpu_.maxCubeSide(), 16384);
    settings_.overviewBudgetBytes = std::min<int64_t>(settings_.overviewBudgetBytes, gpu_.maxCubeBytes());

    resultsDir_ = (fs::u8path(appData) / "results").u8string();
    fs::create_directories(fs::u8path(resultsDir_), ec);

    // New file name when the default layout changes (new panels), so the new
    // layout is applied instead of an old saved one.
    iniPath_ = (fs::u8path(appData) / "layout-0.7.ini").u8string();
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
    defaultCmap_ = cmap_;
    return true;
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

void App::openInputs(const std::vector<std::string>& inputs, bool addLayer) {
    if (opening_.valid()) return; // one open at a time
    openingInputs_ = inputs;
    openingAdd_ = addLayer && !layers_.empty();
    openingReplace_ = -1;
    const BandSelection sel = openingSel_ ? *openingSel_ : opts_.sel;
    opening_ = std::async(std::launch::async, [inputs, sel] {
        const auto t0 = std::chrono::steady_clock::now();
        OpenResult r;
        r.info = openCube(inputs, sel, r.error);
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
    const int maxLayers = gpu_.maxDates();
    if (info->T() > maxLayers) {
        error_ = "The series has " + std::to_string(info->T()) + " dates; the GPU supports up to " +
                 std::to_string(maxLayers) + " layers per texture.";
        openErrorPopup_ = true;
        return;
    }

    if (openingReplace_ >= 0) {
        const int i = std::exchange(openingReplace_, -1);
        if (i < int(layers_.size())) {
            replaceLayerSession(i, info, r.seconds);
            return;
        }
    }
    if (!openingAdd_) closeAll();
    SeriesLayer L;
    L.session = std::make_unique<Session>(info, settings_, [] { glfwPostEmptyEvent(); });
    L.session->openSeconds = r.seconds;
    L.inputs = inputs;
    L.name = layerName(*info, inputs);
    const int T = info->T();
    L.disp.rgb = {0, T / 2, T - 1};
    L.disp.cmap = defaultCmap_;
    L.years.resize(T);
    for (int t = 0; t < T; ++t) L.years[t] = info->yearsFromStart(t);
    // A new layer starts at the date nearest to the one currently shown.
    if (const SeriesLayer* A = activeLayer(); A && A->session->info->timeIsDate && info->timeIsDate) {
        const double now = A->session->info->layers[t_].time;
        int best = 0;
        for (int t = 1; t < T; ++t)
            if (std::fabs(info->layers[t].time - now) < std::fabs(info->layers[best].time - now)) best = t;
        L.disp.t = best;
    }
    layers_.push_back(std::move(L));
    if (layers_.size() == 1) {
        fitRequested_ = true;
        viewTouched_ = false;
        nextPinId_ = 1;
    }
    playing_ = false;
    setActive(int(layers_.size()) - 1);
    files_.addRecent(inputs.size() == 1 ? inputs[0] : info->firstPath);
    // Zeit (separate process) starts only now, never on the startup path.
    startZeit();
}

bool App::wantsContinuousFrames() const { return playing_ || roiDragging_ || hoverPending_; }

bool App::modeAvailable(int mode) const {
    if (!s_) return false;
    if (activeClasses()) return mode == ModeValue; // means, trends... of class codes mean nothing
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
        syncLayerTimes();
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

void App::frame() {
    const auto frameStart = std::chrono::steady_clock::now();
    gestures_ = platform::takeGestures();
    if (!pendingDrop.empty() && !opening_.valid()) {
        auto drop = std::move(pendingDrop);
        pendingDrop.clear();
        openInputs(drop);
    }
    if (opening_.valid() && opening_.wait_for(std::chrono::seconds(0)) == std::future_status::ready) finishOpen();
    for (SeriesLayer& L : layers_) // every layer keeps loading and uploading
        if (L.session->pump(gpu_)) mapDirty_ = true;
    if (s_) {
        s_->overview.setFocus(t_);
        pumpSeries();
        pumpOtherSeries();
        for (SeriesLayer& L : layers_) updateClasses(L);
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
            requestOtherSeries(true);
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

    pumpZeit();

    handleShortcuts();
    uiMenu();
    uiDockspace();
    uiLayers();
    uiFiles();
    uiLayer();
    uiPerf();
    uiSeries();
    uiStats();
    uiMap();
    if (zeit_ && zeit_->state() == ZeitClient::State::Ready)
        for (const ZeitTool& t : zeit_->tools()) uiToolWindow(t);
    uiTasks();
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
            v.zeitVersion = -1;
            v.zeitResult = json();
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
    roiZeitVersion_ = -1;
    roiZeitResult_ = json();
}

// (Subsampled) sample of the quantity shown by `mode`. t < 0 = all dates.
std::vector<float> App::collectSample(int mode, int t, size_t maxN) const {
    std::vector<float> out;
    const Overview& ov = s_->overview;
    const size_t px = size_t(ov.w) * ov.h;
    const float* s0 = s_->gpu.hostStats0;
    const float* s1 = s_->gpu.hostStats1;

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
    if (s_ && ImGui::IsKeyChordPressed(ImGuiMod_Ctrl | ImGuiKey_L)) {
        auto files = platform::openFilesDialog();
        if (!files.empty()) openInputs(files, true);
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
        if (ImGui::MenuItem("Add layer: file(s)...", "Ctrl+L", false, s_ != nullptr)) {
            auto files = platform::openFilesDialog();
            if (!files.empty()) openInputs(files, true);
        }
        if (ImGui::MenuItem("Add layer: folder...", nullptr, false, s_ != nullptr)) {
            auto dir = platform::openFolderDialog();
            if (!dir.empty()) openInputs({dir}, true);
        }
        ImGui::Separator();
        if (ImGui::MenuItem("Close active layer", nullptr, false, s_ != nullptr)) removeLayer(active_);
        if (ImGui::MenuItem("Close all", nullptr, false, !layers_.empty())) closeAll();
        ImGui::Separator();
        if (ImGui::MenuItem("Exit", "Alt+F4")) glfwSetWindowShouldClose(window_, 1);
        ImGui::EndMenu();
    }
    if (ImGui::BeginMenu("View")) {
        if (ImGui::MenuItem("Fit map to window", "Home", false, s_ != nullptr)) fitRequested_ = true;
        ImGui::MenuItem("Performance", nullptr, &showPerf_);
        ImGui::Separator();
        if (ImGui::MenuItem("Reset layout")) layoutPending_ = true;
        ImGui::EndMenu();
    }
    uiToolsMenu();
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
    // Left column: Layers / Files on top, Display below (Performance, off by
    // default, opens as a tab next to Display).
    ImGuiID left, rest, bottom, center, bottomLeft, bottomRight, leftTop, leftMid;
    ImGui::DockBuilderSplitNode(dock, ImGuiDir_Left, 0.21f, &left, &rest);
    ImGui::DockBuilderSplitNode(left, ImGuiDir_Down, 0.58f, &leftMid, &leftTop);
    ImGui::DockBuilderSplitNode(rest, ImGuiDir_Down, 0.36f, &bottom, &center);
    ImGui::DockBuilderSplitNode(bottom, ImGuiDir_Right, 0.34f, &bottomRight, &bottomLeft);
    ImGui::DockBuilderDockWindow("Layers", leftTop);
    ImGui::DockBuilderDockWindow("Files", leftTop);
    ImGui::DockBuilderDockWindow("Display", leftMid);
    ImGui::DockBuilderDockWindow("Performance", leftMid);
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
            "  trackpad ............ pinch: zoom, two fingers: pan\n"
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

void App::renderMap(int w, int h, float pixelScale) {
    const float bg[4] = {0.10f, 0.10f, 0.115f, 1.0f};
    gpu_.beginMap(int(std::lround(w * pixelScale)), int(std::lround(h * pixelScale)), bg);
    // Canvas points -> pixels of the map target.
    auto screenRect = [&](double x0, double y0, double x1, double y1, float r[4]) {
        r[0] = float((offset_.x + x0 * scale_) * pixelScale);
        r[1] = float((offset_.y + y0 * scale_) * pixelScale);
        r[2] = float((offset_.x + x1 * scale_) * pixelScale);
        r[3] = float((offset_.y + y1 * scale_) * pixelScale);
    };
    for (const SeriesLayer& L : layers_) {
        if (!L.visible || !L.aligned) continue;
        const bool isActive = &L == activeLayer();
        const Session& S = *L.session;
        const CubeInfo& info = *S.info;
        // Display state: the live members for the active layer, the saved copy otherwise.
        const int mode = isActive ? mode_ : L.disp.mode;
        const int t = isActive ? t_ : L.disp.t;
        const std::array<int, 3>& rgb = isActive ? rgb_ : L.disp.rgb;
        const Range& range = isActive ? range_[mode] : L.disp.range[mode];
        const int cmap = isActive ? cmap_[mode] : L.disp.cmap[mode];
        DrawParams p;
        p.mode = mode;
        p.t = mode == ModeRGB ? rgb[0] : t;
        p.tg = rgb[1];
        p.tb = rgb[2];
        p.lo = range.lo;
        p.hi = range.hi;
        if (L.classes.state == LayerClasses::On && mode == ModeValue) p.classLut = L.classes.lut;
        const auto& loaded = S.gpu.loaded;
        bool ready = mode == ModeRGB ? loaded[rgb[0]] && loaded[rgb[1]] && loaded[rgb[2]]
                     : modeIsTimeDependent(mode) ? loaded[t] : true;
        if (modeNeedsStats(mode) && !S.gpu.statsValid) ready = false;
        double x0, y0, x1, y1;
        toActive(L, 0, 0, x0, y0);
        toActive(L, info.width, info.height, x1, y1);
        float rect[4];
        screenRect(x0, y0, x1, y1, rect);
        if (ready) gpu_.drawCube(S.gpu, rect, p, cmap, L.opacity);
        if (isActive && mode_ == ModeValue && detailLevel_ >= 0) {
            const ViewRect v{-offset_.x / scale_, -offset_.y / scale_, (canvasSize_.x - offset_.x) / scale_,
                             (canvasSize_.y - offset_.y) / scale_, scale_ * pixelScale};
            s_->tiles->forEachVisible(t_, v, [&](GpuTex tex, double x, double y, double sw, double sh) {
                float r[4];
                screenRect(x, y, x + sw, y + sh, r);
                gpu_.drawTile(tex, r, p, cmap, L.opacity);
            });
        }
        for (const ResultLayer& R : results_) {
            if (R.cubeId != info.id || !R.visible || !R.tex) continue;
            double rx0, ry0, rx1, ry1;
            toActive(L, R.x0, R.y0, rx0, ry0);
            toActive(L, R.x0 + R.w, R.y0 + R.h, rx1, ry1);
            float r[4];
            screenRect(rx0, ry0, rx1, ry1, r);
            gpu_.drawOverlay(R.tex, r, R.lo, R.hi, R.cmap, R.opacity * L.opacity);
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
        if (best >= 0) removePinById(pins_[best].id);
    }
    if (hovered && !pins_.empty() &&
        (ImGui::IsKeyPressed(ImGuiKey_Delete, false) || ImGui::IsKeyPressed(ImGuiKey_Backspace, false)))
        removePinById(pins_.back().id);
    // Zoom by a factor around the cursor; zooming out stops at the image extent
    // (whole image in view, centered).
    auto zoomAtCursor = [&](double factor) {
        const double fit = 0.98 * std::min(size.x / info.width, size.y / info.height);
        const double ns = std::clamp(scale_ * factor, fit, std::max(fit, 64.0));
        if (ns <= fit * 1.0001) {
            fitView(size);
        } else {
            const double f = ns / scale_;
            offset_ = ImVec2(float(m.x - (m.x - offset_.x) * f), float(m.y - (m.y - offset_.y) * f));
            scale_ = ns;
            mapDirty_ = true;
        }
        viewTouched_ = true;
    };
    if (hovered && gestures_.pinch != 1.0) zoomAtCursor(gestures_.pinch);
    if (hovered && (io.MouseWheel != 0 || io.MouseWheelH != 0)) {
        if (gestures_.preciseScroll) {
            // Trackpad: two fingers pan. GLFW scales precise deltas by 0.1;
            // x10 gives back points, so the map follows the fingers.
            offset_ += ImVec2(io.MouseWheelH, io.MouseWheel) * 10.0f;
            mapDirty_ = true;
            viewTouched_ = true;
        } else if (io.MouseWheel != 0) {
            zoomAtCursor(std::pow(1.25, io.MouseWheel));
        }
    }
    if (inside && (ix != hover_.x || iy != hover_.y)) {
        hover_.x = ix;
        hover_.y = iy;
        hover_.values = approxSeries(ix, iy);
        hover_.exact = false;
        hover_.stats = computeSeriesStats(years_, hover_.values);
        hover_.request = 0;
        hover_.zeitReq = 0;
        hover_.zeitVersion = -1;
        hover_.zeitResult = json();
        updateOtherHover(ix, iy);
        hover_.color = ImVec4(0.95f, 0.95f, 0.95f, 1);
        approxLayers_ = s_->overview.layersDone();
        hoverPending_ = true;
        hoverSince_ = ImGui::GetTime();
    }

    // Retina: the map is drawn at the density of the viewport it is on, and
    // detail tiles are chosen by screen pixels, not points.
    const float pixelScale = std::max(1.0f, ImGui::GetWindowViewport()->FramebufferScale.x);
    if (pixelScale != mapPixelScale_) {
        mapPixelScale_ = pixelScale;
        mapDirty_ = true;
    }

    // --- Detail tiles ("value" mode only) ---
    int level = -1;
    if (detail_ && mode_ == ModeValue && !s_->deferRandomReads()) {
        const ViewRect v{-offset_.x / scale_, -offset_.y / scale_, (size.x - offset_.x) / scale_,
                         (size.y - offset_.y) / scale_, scale_ * pixelScale};
        level = s_->tiles->update(t_, v, playing_ ? (t_ + 1) % T : -1);
    }
    if (level != detailLevel_) {
        detailLevel_ = level;
        mapDirty_ = true;
    }

    if (mapDirty_) {
        renderMap(int(size.x), int(size.y), pixelScale);
        mapDirty_ = false;
    }

    // --- Drawing + overlays ---
    ImDrawList* dl = ImGui::GetWindowDrawList();
    const bool bottomUp = Gpu::mapBottomUp();
    dl->AddImage(ImTextureRef((ImTextureID)gpu_.mapTexture()), origin, origin + size, ImVec2(0, bottomUp ? 1.f : 0.f),
                 ImVec2(1, bottomUp ? 0.f : 1.f));
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
    const ResultLayer* topResult = nullptr;
    const SeriesLayer* topResultLayer = nullptr;
    for (const SeriesLayer& L : layers_) {
        if (!L.visible || !L.aligned) continue;
        for (const ResultLayer& R : results_)
            if (R.visible && R.cubeId == L.session->info->id) {
                topResult = &R;
                topResultLayer = &L;
            }
    }
    if (topResult) {
        const ImVec2 b0 = origin + ImVec2(10, size.y - (mode_ != ModeRGB ? 78 : 34)), bsz(220, 10);
        dl->AddText(b0 - ImVec2(0, 16), IM_COL32(230, 230, 230, 255), topResult->name.c_str());
        const int n = 48;
        for (int i = 0; i < n; ++i) {
            const ImVec4 c = ImPlot::SampleColormap((i + 0.5f) / n, topResult->cmap);
            dl->AddRectFilled(b0 + ImVec2(bsz.x * i / n, 0), b0 + ImVec2(bsz.x * (i + 1) / n, bsz.y),
                              ImGui::ColorConvertFloat4ToU32(c));
        }
        dl->AddRect(b0, b0 + bsz, IM_COL32(0, 0, 0, 255));
        char lo[32], hi[32];
        std::snprintf(lo, sizeof(lo), "%.4g", topResult->lo);
        std::snprintf(hi, sizeof(hi), "%.4g", topResult->hi);
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
            const float* a = s_->gpu.hostStats0 + p * 4;
            const float* b = s_->gpu.hostStats1 + p * 4;
            const float v[ModeCount] = {0, 0, a[0], a[1], a[2], b[0], b[1], b[1] - b[0], b[2], 0};
            if (mode_ == ModeAnomaly && hover_.x == ix && hover_.y == iy)
                std::snprintf(status + n, sizeof(status) - n, "  |  anomaly %.6g", hover_.values[t_] - a[0]);
            else if (mode_ != ModeAnomaly)
                std::snprintf(status + n, sizeof(status) - n, "  |  %s %.6g%s", kModeNames[mode_], v[mode_],
                              mode_ == ModeSlope ? slopeUnit().c_str() : "");
        } else if (hover_.x == ix && hover_.y == iy && t_ < int(hover_.values.size())) {
            if (const LayerClasses* C = activeClasses())
                std::snprintf(status + n, sizeof(status) - n, "  |  class %s %s", className(*C, hover_.values[t_]).c_str(),
                              hover_.exact ? "" : "(approx.)");
            else
                std::snprintf(status + n, sizeof(status) - n, "  |  value %.6g %s", hover_.values[t_],
                              hover_.exact ? "" : "(approx.)");
        }
    }
    int rx = -1, ry = -1;
    if (inside && topResult && fromActive(*topResultLayer, ix + 0.5, iy + 0.5, rx, ry)) {
        const float v = topResult->valueAt(rx, ry);
        const size_t len = std::strlen(status);
        const int k = std::isnan(v) ? 0 : int(std::lround(v));
        const std::string text = std::isnan(v) ? "-"
                                 : !topResult->classes.empty() && k >= 1 && k <= int(topResult->classes.size())
                                     ? topResult->classes[k - 1]
                                     : std::to_string(v).substr(0, 10);
        std::snprintf(status + len, sizeof(status) - len, "  |  %s %s", topResult->name.c_str(), text.c_str());
    }
    ImGui::TextUnformatted(status);
    ImGui::End();
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

// One file per date with several bands: which band (or normalized difference
// of two) the series shows, and the quality band that hides unusable dates.
void App::uiBands() {
    const CubeInfo& info = *s_->info;
    if (info.bandsPerDate <= 1) return;
    if (selUiFor_ != info.id) {
        selUi_ = info.sel;
        selUiFor_ = info.id;
    }
    ImGui::SeparatorText("Bands");
    auto name = [&](int b) {
        return std::to_string(b) + ": " +
               (b >= 1 && b <= int(info.bandNames.size()) ? info.bandNames[b - 1] : std::string("?"));
    };
    auto bandCombo = [&](const char* id, int& b, const char* none) {
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo(id, b > 0 ? name(b).c_str() : none)) {
            if (none && ImGui::Selectable(none, b == 0)) b = 0;
            for (int k = 1; k <= info.bandsPerDate; ++k)
                if (ImGui::Selectable(name(k).c_str(), k == b)) b = k;
            ImGui::EndCombo();
        }
    };
    ImGui::TextDisabled("Shown band (A)");
    bandCombo("##band", selUi_.band, nullptr);
    bool nd = selUi_.ndBand > 0;
    if (ImGui::Checkbox("Normalized difference with B", &nd)) selUi_.ndBand = nd ? (selUi_.band == 1 ? 2 : 1) : 0;
    ImGui::SetItemTooltip("Shows (A - B) / (A + B): A = NIR and B = Red gives NDVI,\nA = NIR and B = SWIR1 gives NDMI.");
    if (nd) bandCombo("##ndband", selUi_.ndBand, nullptr);
    ImGui::TextDisabled("Quality band");
    bandCombo("##qaband", selUi_.qaBand, "None");
    if (selUi_.qaBand > 0) {
        if (selUi_.qaRule == QaRule::None) selUi_.qaRule = QaRule::Fmask;
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##qarule", qaRuleLabel(selUi_.qaRule))) {
            for (QaRule r : {QaRule::Fmask, QaRule::LandsatC2, QaRule::NonZero})
                if (ImGui::Selectable(qaRuleLabel(r), r == selUi_.qaRule)) selUi_.qaRule = r;
            ImGui::EndCombo();
        }
        ImGui::SetItemTooltip("Dates whose quality code is not usable (cloud, shadow, snow, fill)\n"
                              "become no-data in the map, the charts and the tools.");
    } else {
        selUi_.qaRule = QaRule::None;
    }
    if (selUi_ != info.sel) {
        ImGui::BeginDisabled(opening_.valid());
        if (ImGui::Button("Apply (reopens the layer)", ImVec2(-1, 0))) reopenLayer(active_, selUi_);
        ImGui::EndDisabled();
        if (ImGui::SmallButton("Revert")) selUi_ = info.sel;
    }
}

void App::uiLayer() {
    if (!ImGui::Begin("Display")) {
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

    if (const SeriesLayer* L = activeLayer()) ImGui::TextColored(ImVec4(0.55f, 0.80f, 1.0f, 1), "%s", L->name.c_str());
    ImGui::SeparatorText("Series");
    ImGui::TextWrapped("%s", info.description.c_str());
    ImGui::Text("%d dates: %s to %s", T, info.layers.front().label.c_str(), info.layers.back().label.c_str());
    ImGui::Text("%d x %d px, %s", info.width, info.height, info.dataType.c_str());
    ImGui::TextWrapped("CRS: %s %s", info.crsName.empty() ? "(no CRS)" : info.crsName.c_str(),
                       info.crsAuthority.c_str());
    if (info.hasGeoTransform)
        ImGui::Text("Pixel: %.6g x %.6g", info.geoTransform[1], std::fabs(info.geoTransform[5]));
    uiBands();

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
    uiClasses();
    if (!activeClasses()) { // colormap, range and histogram: continuous data only
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
    } // continuous data

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
    if (layers_.size() > 1) {
        ImGui::SameLine();
        ImGui::SetNextItemWidth(150);
        if (ImGui::Combo("##layers", &chartLayers_, "Active layer\0All visible layers\0")) {
            for (SeriesLayer& L : layers_) L.hover = SeriesView{};
            if (chartLayers_ == 1) {
                if (hover_.x >= 0) updateOtherHover(hover_.x, hover_.y);
                for (SeriesLayer& L : layers_) L.pins.clear();
                for (const SeriesView& p : pins_) addOtherPins(p);
                requestOtherSeries(true);
            }
        }
        ImGui::SetItemTooltip("Series of the active layer only, or of every visible layer at the same\n"
                              "place (different marker per layer; other layers use their own dates)");
    }
    if (roi_) {
        ImGui::SameLine();
        ImGui::Checkbox("p10-p90 band", &showRoiBand_);
    }
    ImGui::SameLine();
    if (ImGui::Button("Copy CSV")) copyCsv();
    ImGui::SetItemTooltip("Copies the series (cursor, pins, ROI) to the clipboard");
    if (!pins_.empty()) {
        ImGui::SameLine();
        if (ImGui::Button("Clear pins")) clearPins();
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
        // Categorical: one tick per class, named; values as they are.
        const LayerClasses* classes = activeClasses();
        if (classes && !classes->list.empty() && classes->list.size() <= 40) {
            std::vector<double> ticks;
            std::vector<std::string> names;
            for (const ClassEntry& c : classes->list) {
                ticks.push_back(c.value);
                names.push_back(c.name);
            }
            std::vector<const char*> labels;
            for (const std::string& n : names) labels.push_back(n.c_str());
            ImPlot::SetupAxisTicks(ImAxis_Y1, ticks.data(), int(ticks.size()), labels.data());
        }

        // Displayed value: raw, anomaly or z-score (using the series' own statistics).
        auto transform = [&](double v, const SeriesStats& st) {
            if (classes) return v;
            if (plotValues_ == 1) return v - st.mean;
            if (plotValues_ == 2) return st.std > 0 ? (v - st.mean) / st.std : 0.0;
            return v;
        };
        auto plotSeries = [&](const char* label, const std::vector<float>& vals, const SeriesStats& st, ImVec4 col,
                              float weight, bool trend = true, const json* model = nullptr) {
            if (int(vals.size()) != T) return;
            std::vector<double> ys(T);
            for (int t = 0; t < T; ++t) ys[t] = transform(vals[t], st);
            ImPlotSpec spec;
            spec.LineColor = col;
            spec.LineWeight = weight;
            spec.MarkerFillColor = col;
            if (classes) { // a class holds until the next date: steps, no trend
                spec.Marker = ImPlotMarker_Circle;
                spec.MarkerSize = 2.5f;
                ImPlot::PlotStairs(label, xs_.data(), ys.data(), T, spec);
                if (model && !pixelTool_.empty()) drawZeitOverlays(label, *model, col, st);
                return;
            }
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
            if (model && !pixelTool_.empty()) drawZeitOverlays(label, *model, col, st);
        };

        if (!roiMean_.empty() && !classes) {
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
            plotSeries(label, roiMean_, roiStats_, yellow, 2.0f, roi_ && roi_->done == T, &roiZeitResult_);
        }
        for (const SeriesView& p : pins_) {
            char label[64];
            std::snprintf(label, sizeof(label), "Pin %d (%d, %d)%s###pin%d", p.id, p.x, p.y,
                          p.exact ? "" : " approx.", p.id);
            plotSeries(label, p.values, p.stats, p.color, 1.5f, true, &p.zeitResult);
        }
        if (hover_.x >= 0) {
            char label[64];
            std::snprintf(label, sizeof(label), "Cursor (%d, %d)%s###cursor", hover_.x, hover_.y,
                          hover_.exact ? "" : " approx.");
            plotSeries(label, hover_.values, hover_.stats, hover_.color, 2.0f, true, &hover_.zeitResult);
        }
        if (chartLayers_ == 1) {
            // Other visible layers: their own dates on the same time axis, one marker shape per layer.
            for (size_t li = 0; li < layers_.size(); ++li) {
                const SeriesLayer& L = layers_[li];
                if (&L == activeLayer() || !L.visible) continue;
                const CubeInfo& li_info = *L.session->info;
                if (li_info.timeIsDate != info.timeIsDate) continue;
                std::vector<double> lx(li_info.T());
                for (int t = 0; t < li_info.T(); ++t) lx[t] = li_info.layers[t].time;
                auto plotOther = [&](const char* label, const SeriesView& v, float weight) {
                    if (int(v.values.size()) != li_info.T()) return;
                    std::vector<double> ys(v.values.size());
                    for (size_t t = 0; t < ys.size(); ++t) {
                        const double x = v.values[t];
                        ys[t] = plotValues_ == 1 ? x - v.stats.mean
                                : plotValues_ == 2 ? (v.stats.std > 0 ? (x - v.stats.mean) / v.stats.std : 0.0) : x;
                    }
                    ImPlotSpec spec;
                    spec.LineColor = ImVec4(v.color.x, v.color.y, v.color.z, 0.85f);
                    spec.LineWeight = weight;
                    spec.Marker = layerMarker(int(li));
                    spec.MarkerSize = 3.5f;
                    spec.MarkerFillColor = v.color;
                    ImPlot::PlotLine(label, lx.data(), ys.data(), int(ys.size()), spec);
                    if (!pixelTool_.empty()) drawZeitOverlays(label, v.zeitResult, v.color, v.stats);
                };
                for (const SeriesView& p : L.pins) {
                    char label[128];
                    std::snprintf(label, sizeof(label), "Pin %d [%s]%s###pin%d_%d", p.id, L.name.c_str(),
                                  p.exact ? "" : " approx.", p.id, int(li));
                    plotOther(label, p, 1.2f);
                }
                if (L.hover.x >= 0) {
                    char label[128];
                    std::snprintf(label, sizeof(label), "Cursor [%s]%s###cursor_%d", L.name.c_str(),
                                  L.hover.exact ? "" : " approx.", int(li));
                    plotOther(label, L.hover, 1.6f);
                }
            }
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
        const json* model = nullptr; // rows from the Zeit tool on the chart
        const CubeInfo* cube = nullptr; // whose dates argMin/argMax refer to (null = active)
        const std::vector<float>* values = nullptr; // the series (categorical summaries)
        const LayerClasses* classes = nullptr;      // set if the series is categorical
        int t = -1;                                  // date shown for that series
    };
    const LayerClasses* activeCls = activeClasses();
    std::vector<Col> cols;
    if (hover_.x >= 0)
        cols.push_back({hover_.exact ? "Cursor" : "Cursor*", &hover_.stats, hover_.color, -1, false, &hover_.zeitResult,
                        nullptr, &hover_.values, activeCls, t_});
    for (size_t i = 0; i < pins_.size(); ++i)
        cols.push_back({"Pin " + std::to_string(pins_[i].id) + (pins_[i].exact ? "" : "*"), &pins_[i].stats,
                        pins_[i].color, int(i), false, &pins_[i].zeitResult, nullptr, &pins_[i].values, activeCls, t_});
    if (!roiMean_.empty())
        cols.push_back({"ROI mean", &roiStats_, ImVec4(1.0f, 0.82f, 0.24f, 1), -1, true, &roiZeitResult_});
    if (chartLayers_ == 1)
        for (const SeriesLayer& L : layers_) {
            if (&L == activeLayer() || !L.visible) continue;
            const LayerClasses* lc = L.classes.state == LayerClasses::On ? &L.classes : nullptr;
            if (L.hover.x >= 0)
                cols.push_back({"Cursor [" + L.name + "]" + (L.hover.exact ? "" : "*"), &L.hover.stats, L.hover.color,
                                -1, false, &L.hover.zeitResult, L.session->info.get(), &L.hover.values, lc, L.disp.t});
            for (const SeriesView& p : L.pins)
                cols.push_back({"Pin " + std::to_string(p.id) + " [" + L.name + "]" + (p.exact ? "" : "*"), &p.stats,
                                p.color, -1, false, &p.zeitResult, L.session->info.get(), &p.values, lc, L.disp.t});
        }
    if (cols.empty()) {
        ImGui::TextDisabled("Hover over the map, click to drop pins\nor Shift+drag for an ROI.");
        ImGui::End();
        return;
    }

    const std::string unit = slopeUnit();
    const CubeInfo* curCube = &info; // the column being drawn (its own dates)
    auto date = [&](int idx) { return idx >= 0 && idx < curCube->T() ? curCube->layers[idx].label : std::string("-"); };
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
        if (activeCls) {
            // Categorical: class summaries (majority, changes...) instead of means and trends.
            std::vector<std::vector<std::pair<std::string, std::string>>> summaries;
            for (const Col& c : cols)
                summaries.push_back(c.classes && c.values ? classSummary(*c.classes, c.cube ? *c.cube : info, *c.values, c.t)
                                                          : std::vector<std::pair<std::string, std::string>>{});
            const auto labels = classSummary(*activeCls, info, {}, -1);
            for (size_t r = 0; r < labels.size(); ++r) {
                ImGui::TableNextRow();
                ImGui::TableNextColumn();
                ImGui::TextDisabled("%s", labels[r].first.c_str());
                for (size_t ci = 0; ci < cols.size(); ++ci) {
                    ImGui::TableNextColumn();
                    ImGui::TextUnformatted(r < summaries[ci].size() ? summaries[ci][r].second.c_str() : "-");
                }
            }
        } else {
            for (const Row& row : rows) {
                ImGui::TableNextRow();
                ImGui::TableNextColumn();
                ImGui::TextDisabled("%s", row.label);
                for (const Col& c : cols) {
                    ImGui::TableNextColumn();
                    curCube = c.cube ? c.cube : &info;
                    ImGui::TextUnformatted(row.f(*c.st).c_str());
                }
            }
        }
        // Rows from the Zeit tool fitted on the chart, in order of first appearance.
        if (!pixelTool_.empty()) {
            std::vector<std::string> names;
            for (const Col& c : cols)
                if (c.model && c.model->is_object())
                    for (const json& r : c.model->value("rows", json::array()))
                        if (r.is_array() && r.size() == 2 && r[0].is_string() &&
                            std::find(names.begin(), names.end(), r[0].get<std::string>()) == names.end())
                            names.push_back(r[0].get<std::string>());
            const ZeitTool* tool = zeit_ ? zeit_->tool(pixelTool_) : nullptr;
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextColored(ImVec4(0.55f, 0.75f, 1.0f, 1), "%s", tool ? tool->name.c_str() : pixelTool_.c_str());
            for (const Col& c : cols) {
                ImGui::TableNextColumn();
                if (c.model && c.model->contains("error")) ImGui::TextDisabled("error");
                else if (!c.model || !c.model->contains("rows")) ImGui::TextDisabled("...");
            }
            for (const std::string& n : names) {
                ImGui::TableNextRow();
                ImGui::TableNextColumn();
                ImGui::TextDisabled("%s", n.c_str());
                for (const Col& c : cols) {
                    ImGui::TableNextColumn();
                    if (!c.model || !c.model->is_object()) continue;
                    for (const json& r : c.model->value("rows", json::array()))
                        if (r.is_array() && r.size() == 2 && r[0] == n && r[1].is_string())
                            ImGui::TextUnformatted(r[1].get<std::string>().c_str());
                }
            }
        }
        ImGui::EndTable();
        // Remove after drawing: the pointers in `cols` point into pins_.
        if (removePin >= 0) removePinById(pins_[removePin].id);
        if (removeRoi) clearRoi();
    }
    ImGui::End();
}

void App::uiPerf() {
    if (!showPerf_) return;
    if (!ImGui::Begin("Performance", &showPerf_)) {
        ImGui::End();
        return;
    }
    ImGuiIO& io = ImGui::GetIO();
    ImGui::Text("Frame: %.2f ms of CPU", frameMs_);
    ImGui::SetItemTooltip("The app only redraws when something changes (mouse, keyboard, new data).\n"
                          "Idle, the CPU sleeps; while animating it runs at %.0f frames/s.", io.Framerate);
    ImGui::Text("Startup: window %.0f ms, first frame %.0f ms", startup_.windowMs, startup_.firstFrameMs);
    ImGui::SetItemTooltip("Since process start: GDAL %.0f ms, window + %s %.0f ms, UI %.0f ms,\n"
                          "opening inputs %.0f ms, first frame %.0f ms",
                          startup_.gdalMs, render::name(), startup_.windowMs, startup_.uiMs, startup_.openMs,
                          startup_.firstFrameMs);
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
    ImGui::SeparatorText("Zeit");
    if (!zeit_ || zeit_->state() == ZeitClient::State::Off) {
        ImGui::TextDisabled("Not started (starts when a series is opened)");
    } else if (zeit_->state() == ZeitClient::State::Starting) {
        ImGui::TextDisabled("Starting in the background...");
    } else if (zeit_->state() == ZeitClient::State::Failed) {
        ImGui::PushTextWrapPos(0);
        ImGui::TextColored(ImVec4(1.0f, 0.55f, 0.45f, 1), "%s", zeit_->error().c_str());
        ImGui::PopTextWrapPos();
    } else {
        ImGui::Text("Zeit %s, Python %s", zeit_->zeitVersion().c_str(), zeit_->pythonVersion().c_str());
        ImGui::Text("Process ready in %.0f ms (background)", zeit_->startupMs());
        if (lastPixelFitMs_ > 0) ImGui::Text("Last pixel fit: %.1f ms", lastPixelFitMs_);
        if (!zeit_->config().bundled) ImGui::TextDisabled("Developer runtime: %s", zeit_->config().python.c_str());
    }

    ImGui::SeparatorText("Settings");
    ImGui::SetNextItemWidth(-1);
    ImGui::SliderInt("##budget", &budgetUi_, 128, 8192, "Overview: %d MB", ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Memory for the cube overview (on the GPU; on macOS shared\nwith the CPU). Larger = more resolution without tiles,\nbut slower to build.");
    if (budgetUi_ != opts_.budgetMB) {
        if (ImGui::Button(s_ ? "Apply (reopens the active layer)" : "Apply", ImVec2(-1, 0))) {
            opts_.budgetMB = budgetUi_;
            settings_.overviewBudgetBytes = int64_t(budgetUi_) << 20;
            if (s_ && !opening_.valid()) {
                const std::vector<std::string> inputs = lastInputs_;
                removeLayer(active_);
                openInputs(inputs, true);
            }
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
