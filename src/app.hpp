#pragma once

#include <array>
#include <chrono>
#include <future>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include <imgui.h>

#include "file_browser.hpp"
#include "platform.hpp"
#include "gpu.hpp"
#include "results.hpp"
#include "session.hpp"
#include "stats.hpp"
#include "zeit_client.hpp"

struct GLFWwindow;

// A series shown in the chart (cursor or pin).
struct SeriesView {
    int id = 0;              // pin number (stable: removing one does not renumber the others)
    int x = -1, y = -1;
    std::vector<float> values;
    bool exact = false;      // false = approximated from the overview (until the read finishes)
    SeriesStats stats;
    uint64_t request = 0;
    ImVec4 color{1, 1, 1, 1};
    // Model fitted by the Zeit tool shown on the chart (e.g. LandTrendr segments).
    uint64_t zeitReq = 0;
    int zeitVersion = -1;    // pixelVersion_ the result belongs to
    json zeitResult;         // {"overlays": [...], "rows": [...]} or {"error": ...}
};

// Startup timings in ms since process start (shown in the Performance panel).
struct StartupTimes {
    double gdalMs = 0, windowMs = 0, uiMs = 0, openMs = 0, firstFrameMs = 0;
};

struct AppOptions {
    BandSelection sel; // band(s) shown when each file is one date
    int64_t budgetMB = 1024;
    int ioThreads = 0;
    std::string zeitPython;  // developer override of the bundled runtime
    std::string zeitBridge;
};

class App {
public:
    explicit App(GLFWwindow* window);
    ~App();
    bool init(const AppOptions& opts, std::string& error);
    // Opens a series; `addLayer` keeps the open ones and adds it as a new layer.
    void openInputs(const std::vector<std::string>& inputs, bool addLayer = false);
    void frame();
    bool wantsContinuousFrames() const;
    void setStartupTimes(const StartupTimes& t) { startup_ = t; }
    // --selftest-ui: one step per frame; -1 while running, then the exit code.
    int selfTestStep(const std::vector<std::string>& inputs);

    std::vector<std::string> pendingDrop; // filled by the drag-and-drop callback

private:
    void closeAll();
    void finishOpen();
    void uiMenu();
    void uiDockspace();
    void uiMap();
    void uiLayer();
    void uiSeries();
    void uiStats();
    void uiPerf();
    void uiPopups();
    void handleShortcuts();
    void pumpSeries();
    void renderMap(int w, int h, float pixelScale = 1.0f); // w, h in canvas points
    void fitView(ImVec2 canvas);
    void setT(int t);
    void addPin(int x, int y);
    void clearRoi();
    void updateRoiSeries();
    std::vector<float> approxSeries(int x, int y) const;
    bool modeAvailable(int mode) const;
    void updateRangeAndHistogram();
    std::vector<float> collectSample(int mode, int t, size_t maxN) const;
    std::string slopeUnit() const;
    void copyCsv();

    // Zeit tools (app_zeit.cpp)
    ZeitConfig zeitConfig() const;
    void startZeit();
    void pumpZeit();
    void requestPixelFits();
    std::string toolApplicability(const ZeitTool& tool) const;
    void runTool(const ZeitTool& tool);
    void uiToolsMenu();
    void uiToolWindow(const ZeitTool& tool);
    void uiTasks();
    void uiResultsOf(uint64_t cubeId);
    void clearResults(uint64_t cubeId = 0); // 0 = every layer
    void drawZeitOverlays(const char* label, const json& result, const SeriesStats& st);

    // Layers (app_layers.cpp)
    struct LayerDisplay;
    struct SeriesLayer;
    void saveDisplay(LayerDisplay& d) const;
    void loadDisplay(const LayerDisplay& d);
    void setActive(int i);
    void removeLayer(int i);
    // Reopens layer i's files with another band selection, in place.
    void reopenLayer(int i, const BandSelection& sel);
    void replaceLayerSession(int i, std::shared_ptr<CubeInfo> info, double seconds);
    void uiBands();
    void updateAlignment();
    void syncLayerTimes();
    bool toActive(const SeriesLayer& L, double lx, double ly, double& ax, double& ay) const;
    bool fromActive(const SeriesLayer& L, double ax, double ay, int& lx, int& ly) const;
    std::vector<float> approxSeriesOf(const Session& s, int x, int y) const;
    void updateOtherHover(int ix, int iy);
    void requestOtherSeries(bool hover);
    void pumpOtherSeries();
    void addOtherPins(const SeriesView& pin);
    void removePinById(int id);
    void clearPins();
    void uiLayers();
    void uiFiles();
    const SeriesLayer* activeLayer() const;
    std::string layerName(const CubeInfo& info, const std::vector<std::string>& inputs) const;

    GLFWwindow* window_;
    AppOptions opts_;
    SessionSettings settings_;
    Gpu gpu_;
    Session* s_ = nullptr;       // session of the active layer (owned by layers_)
    std::vector<std::string> lastInputs_;
    // Opening runs in the background (reading metadata of many files on an
    // HDD or network share can take a while): the window never waits for it.
    struct OpenResult {
        std::shared_ptr<CubeInfo> info;
        std::string error;
        double seconds = 0;
    };
    std::future<OpenResult> opening_;
    std::vector<std::string> openingInputs_;
    std::optional<BandSelection> openingSel_; // band selection of the pending open (default: options)
    int openingReplace_ = -1;
    bool showPerf_ = false;                   // Performance panel (View menu)                 // the pending open replaces this layer's session
    BandSelection selUi_;                     // Display panel: bands being edited
    uint64_t selUiFor_ = 0;                   // cube id selUi_ was loaded from
    std::vector<double> years_;  // time in years since the 1st date (for trends)
    std::string iniPath_;
    bool iniExisted_ = false;
    bool layoutPending_ = true;

    std::string error_;
    bool openErrorPopup_ = false, openHelpPopup_ = false;

    // Time / animation
    int t_ = 0;
    bool playing_ = false;
    float fps_ = 4.f;
    double playAccum_ = 0;

    // Display
    int mode_ = ModeValue;
    std::array<int, 3> rgb_{0, 0, 0};
    std::array<int, ModeCount> cmap_{};
    int appliedCmap_ = -1;
    struct Range {
        float lo = 0, hi = 1;
        bool manual = false;
        uint64_t key = ~0ull;    // data state when it was computed
    };
    std::array<Range, ModeCount> range_{};
    bool perDateRange_ = false;
    bool detail_ = true;
    int detailLevel_ = -1;
    uint64_t dataVersion_ = 0;   // changes when new data arrives (layers, statistics)

    std::vector<double> histX_, histY_;
    uint64_t histKey_ = ~0ull;
    double histBarW_ = 1;

    // Map
    double scale_ = 1;
    ImVec2 offset_{0, 0};
    bool fitRequested_ = true;
    ImVec2 canvasSize_{0, 0};
    bool mapDirty_ = true;
    float mapPixelScale_ = 1.0f; // framebuffer pixels per point of the map's viewport
    bool roiDragging_ = false;
    bool ctrlClick_ = false;     // macOS: this left click is a Control + click (remove a pin)
    ImVec2 roiStart_{0, 0}, roiEnd_{0, 0};

    // Series
    std::vector<double> xs_;     // X axis (Unix seconds or index)
    SeriesView hover_;
    std::vector<SeriesView> pins_;
    std::shared_ptr<RoiData> roi_;
    int roiSeen_ = -1;
    std::vector<float> roiMean_, roiP10_, roiP90_;
    SeriesStats roiStats_;
    bool showRoiBand_ = true;
    int nextPinId_ = 1;
    bool pendingRoi_ = false;    // ROI requested on an HDD while the overview builds
    int pendingRoiRect_[4] = {0, 0, 0, 0};
    // Chart
    int plotStyle_ = 1;          // 0 lines, 1 lines+markers, 2 markers, 3 stems, 4 stairs
    int plotValues_ = 0;         // 0 values, 1 anomaly (- series mean), 2 z-score
    int trendKind_ = 1;          // 0 none, 1 OLS, 2 Sen
    bool yFromMap_ = false;      // Y axis = the map's color range
    double lastSeriesMs_ = 0;
    bool hoverPending_ = false;  // exact series only requested once the mouse rests
    double hoverSince_ = 0;
    int approxLayers_ = -1;      // overview layers used by the approximate series
    bool viewTouched_ = false;   // user already panned/zoomed (don't auto-fit)
    platform::Gestures gestures_; // trackpad pinch and scroll kind of this frame
    double frameMs_ = 0;
    int budgetUi_ = 1024;
    StartupTimes startup_;

    // Zeit
    std::unique_ptr<ZeitClient> zeit_;
    struct ToolUi {
        bool open = false;
        json params;             // current parameter values
        int scope = 0;           // 0 whole image, 1 visible area, 2 ROI
        // Run-time estimate: the bridge times the tool on a sample of the image.
        json estKey;             // inputs the estimate was asked for
        uint64_t estReq = 0;
        json estimate;           // the bridge's timing model (see estimateJobSeconds)
        std::string estError;
        std::chrono::steady_clock::time_point estChanged{};
    };
    void toolWindow(const ToolUi& ui, int win[4], const char** scopeName) const;
    void updateEstimate(const ZeitTool& tool, ToolUi& ui);
    bool uiPatterns(const ZeitParam& p, json& v);
    std::map<std::string, ToolUi> toolUi_;
    // Band roles of the active layer for multiband tools (guessed, editable in
    // the tool window; kept while the layer's bands stay the same).
    BandRoles bandRoles_;
    std::vector<std::string> bandRolesNames_;
    uint64_t bandRolesFor_ = 0;
    const BandRoles& activeBandRoles();
    // Pixel runs of multiband tools read every band of the pixel first, one
    // pixel at a time, in the background.
    std::shared_ptr<CubeReader> bandReader_;
    struct BandFetch {
        int key = 0, x = -1, y = -1, version = -1;
        std::future<json> f;
    };
    std::optional<BandFetch> bandFetch_;
    std::string resultsDir_;     // base folder for tool outputs
    std::string pixelTool_;      // tool fitted on the chart ("" = none)
    int pixelVersion_ = 0;       // bumped when the tool or its parameters change
    std::vector<double> zeitYears_; // decimal year of each date
    uint64_t roiZeitReq_ = 0;
    int roiZeitVersion_ = -1;
    json roiZeitResult_;
    std::map<uint64_t, std::chrono::steady_clock::time_point> pixelSent_;
    double lastPixelFitMs_ = 0;
    std::vector<std::shared_ptr<ZeitJob>> jobs_;
    std::map<const ZeitJob*, uint64_t> jobCube_; // cube id each job was started for
    bool showTasks_ = false;
    std::vector<ResultLayer> results_;
    struct LoadedResult {
        ResultLayer layer;
        bool ok = false;
        std::string error;
    };
    std::vector<std::future<LoadedResult>> resultLoads_;

    // Layers: every open series. The active layer's display state lives in the
    // members above (mode_, cmap_, range_...); the others keep theirs here.
    // Map space = the active layer's pixel grid; other layers are placed
    // through their geotransforms (same CRS, no rotation).
    struct LayerDisplay {
        int mode = ModeValue;
        std::array<int, 3> rgb{0, 0, 0};
        std::array<int, ModeCount> cmap{};
        std::array<Range, ModeCount> range{};
        bool perDateRange = false;
        int t = 0;
    };
    // Categorical series (see app_classes.cpp).
    struct ClassEntry {
        int value = 0;
        std::string name;
        ImVec4 color{1, 1, 1, 1};
        size_t count = 0;          // pixels x dates (overview), for the share
        bool visible = true;
    };
    struct LayerClasses {
        enum State { Undecided, Off, On } state = Undecided;
        bool complete = false;     // counted on every date
        std::vector<ClassEntry> list; // sorted by value
        GpuTex lut = 0;
        bool lutDirty = false;
        const ClassEntry* find(int value) const;
    };
    void setClasses(SeriesLayer& L, const std::map<int, size_t>& counts);
    bool countLayerClasses(const SeriesLayer& L, int maxClasses, std::map<int, size_t>& counts) const;
    void updateClasses(SeriesLayer& L);
    const LayerClasses* activeClasses() const;
    static std::string className(const LayerClasses& C, float v);
    void uiClasses();
    std::vector<std::pair<std::string, std::string>> classSummary(const LayerClasses& C, const CubeInfo& info,
                                                                  const std::vector<float>& v, int t) const;
    std::string classError_;

    struct SeriesLayer {
        std::unique_ptr<Session> session;
        std::vector<std::string> inputs;
        std::string name;
        bool visible = true;
        float opacity = 1.0f;
        LayerDisplay disp;
        // this layer's pixels -> active layer's pixels: x' = ax + bx * x, y' = ay + by * y
        double ax = 0, bx = 1, ay = 0, by = 1;
        bool aligned = true;
        std::string alignNote;
        SeriesView hover;             // cursor series (non-active layers)
        std::vector<SeriesView> pins; // same ids as pins_ (non-active layers)
        std::vector<double> years;    // years since the 1st date (for trends)
        LayerClasses classes;
    };
    std::vector<SeriesLayer> layers_;
    int active_ = -1;
    bool openingAdd_ = false;         // the pending open adds a layer
    int chartLayers_ = 0;             // 0 = active layer only, 1 = all visible layers
    std::array<int, ModeCount> defaultCmap_{};
    FileBrowser files_;
    struct {
        int stage = 0;
        double t0 = 0, since = 0;
        unsigned char color[3] = {0, 0, 0};
    } st_; // --selftest-ui state

};
