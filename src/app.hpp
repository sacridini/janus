#pragma once

#include <array>
#include <atomic>
#include <chrono>
#include <future>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include <imgui.h>

#include "file_browser.hpp"
#include "platform.hpp"
#include "gpu.hpp"
#include "reproject.hpp"
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

const char* modeName(int mode); // display mode (DisplayMode) as shown in the interface
// Reference date of the difference mode at date t: a fixed date, or the
// previous one (ref < 0; -1 at the first date: no difference).
int diffRefDate(int ref, int t, int T);

// A file being written in the background (map PNG, GeoTIFF; see app_export.cpp).
struct ExportJob {
    enum class State { Running, Done, Failed, Cancelled };
    std::string title, path;
    std::atomic<State> state{State::Running};
    std::atomic<double> progress{0};
    std::atomic<bool> cancel{false};
    std::mutex m;
    std::string error;       // under m
    double seconds = 0;      // set before state leaves Running
    std::chrono::steady_clock::time_point started = std::chrono::steady_clock::now();
    std::future<void> done;  // last: destroyed first, waits for the thread
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
    // --measure-cache on|off IN: times the full-resolution cache (app_fullres.cpp).
    int measureCacheStep(const std::vector<std::string>& inputs);

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
    // w, h in canvas points; slot: GPU target (0 = the main map); background: RGBA (default: the map's)
    void renderMap(int w, int h, float pixelScale = 1.0f, int slot = 0, const float* background = nullptr);
    // Mouse and keys over a map canvas (pins, ROI, pan, zoom, the hovered pixel of
    // the active layer). `origin`: where the main canvas' (0, 0) is on screen for
    // this panel (panels share one view, centered alike); `size`: the main canvas.
    void mapInput(ImVec2 origin, ImVec2 size, int panel, int& ix, int& iy, bool& inside);
    // Pins, ROI, the hovered pixel (or a cross where another panel's cursor is).
    void drawMapMarks(ImDrawList* dl, ImVec2 origin, int panel, int ix, int iy, bool inside);
    struct MapView;
    void newMapView();
    void uiMapViews();
    void uiMapView(MapView& v);
    static void autoRangeOf(int mode, std::vector<float>& sample, float& lo, float& hi);
    void renderView(MapView& v, int w, int h, float pixelScale, ImVec2 shift);
    void fitView(ImVec2 canvas);
    void setT(int t);
    void addPin(int x, int y);
    void clearRoi();
    void updateRoiSeries();
    std::vector<float> approxSeries(int x, int y) const;
    bool modeAvailable(int mode) const;
    void updateRangeAndHistogram();
    // diffRef: reference date of the difference mode (see diffRef_).
    std::vector<float> collectSample(const Session& S, int mode, int t, size_t maxN, int diffRef = 0) const;
    std::string slopeUnit() const;
    void copyCsv();

    // Export (app_export.cpp): the map as PNG, values and the rendered view as
    // GeoTIFF, Zeit results. Rendering is on the GPU (main thread); reading the
    // data and writing files in the background, listed in the Exports window.
    struct PngOptions {
        int scale = 1;           // x the on-screen resolution (rendered at it, not upscaled)
        int background = 0;      // 0 the map's (dark), 1 white, 2 transparent
        bool label = true;       // date and mode, top left
        bool legend = true;      // colour bar or classes, bottom left
        bool marks = false;      // pins and ROI
    };
    void uiExportMenu();         // File menu entries
    void uiExport();             // options popup
    void uiExports();            // progress window
    void uiResultExportMenu(const ResultLayer& r, const char* label); // menu item: save it as GeoTIFF
    bool exportScaleFits(int scale) const;
    // The main map's view rendered offscreen at `scale` x its on-screen resolution,
    // RGBA rows top-down; `transparent`: alpha = coverage (two renders), else over `bg`.
    void renderExport(int scale, bool transparent, const float bg[4], std::vector<unsigned char>& rgba, int& w,
                      int& h);
    std::shared_ptr<ExportJob> exportPng(const std::string& path, const PngOptions& o);
    std::shared_ptr<ExportJob> exportValues(const std::string& path, bool wholeImage);
    std::shared_ptr<ExportJob> exportView(const std::string& path, int scale);
    std::shared_ptr<ExportJob> exportResult(const ResultLayer& r, const std::string& path);
    bool visibleWindow(int win[4]) const; // visible part of the active layer, in its pixels
    std::string exportName(const std::string& suffix) const;
    std::string askSavePath(const char* title, const std::string& name, const char* filter, const char* ext);
    const char* selfTestExports(); // nullptr = passed

    // Full-resolution cache (app_fullres.cpp)
    void registerFullResSettings();
    void uiFullRes();
    void uiFullResSettings();
    int selfTestFullRes(); // --selftest-ui stages 30-32 (app_selftest.cpp)
    int selfTestReproject(const std::string& f); // stages 40-45: F, B reprojected (app_selftest.cpp)

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
    // A map panel's bar (layer, own date, own mode) and its automatic range; returns its layer.
    SeriesLayer* uiViewBar(MapView& v);
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
    // Same as fromActive without rounding or bounds (the layer's continuous pixel coordinates).
    bool activeToLayer(const SeriesLayer& L, double ax, double ay, double& lx, double& ly) const;
    // Where L's pixels [x0, x1) x [y0, y1) are drawn in map space (q: x0, y0, x1, y1
    // in the active layer's pixels) and the warp that goes with it (none for a
    // layer on the same CRS and grid lines). False if nothing of it can be drawn.
    bool layerQuad(const SeriesLayer& L, double x0, double y0, double x1, double y1, double q[4], WarpParams& w) const;
    // The map-space rectangle [x0, x1) x [y0, y1) in L's own pixels (what to read
    // of it) and the zoom there: target pixels per L pixel, at `pxPerMapPx`.
    ViewRect layerView(const SeriesLayer& L, double x0, double y0, double x1, double y1, double pxPerMapPx) const;
    // ROI on the other layers (app_layers.cpp): each one's own pixels under the
    // map's rectangle, with "All visible layers".
    void setRoiRect(const int r[4]); // a new ROI rectangle (active pixels), for every layer
    void updateOtherRois();
    void clearOtherRois();
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
    int diffRef_ = 0;            // reference date of the difference mode (-1 = previous date)
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
    bool zooming_ = false;       // mouse wheel zoom on its way to zoomTarget_
    double zoomTarget_ = 1;
    ImVec2 zoomAnchor_{0, 0};    // canvas point that stays put while zooming
    bool roiDragging_ = false;
    bool ctrlClick_ = false;     // macOS: this left click is a Control + click (remove a pin)
    ImVec2 roiStart_{0, 0}, roiEnd_{0, 0};

    // Series
    std::vector<double> xs_;     // X axis (Unix seconds or index)
    SeriesView hover_;
    std::vector<SeriesView> pins_;
    // Extra map panels (View > New map view), next to the main map: one layer
    // each, with its own date and display mode if wanted, always on the same
    // area (they share scale_ and offset_, each canvas centered like the main one).
    struct MapView {
        int id = 0;              // window "Map <id>" and GPU target slot
        uint64_t cube = 0;       // CubeInfo id of the layer shown
        bool ownDate = false;    // else the layer's date
        int t = 0;
        bool ownMode = false;    // else the layer's display mode
        int mode = ModeValue;
        Range range;             // automatic range of an own mode
        bool docked = false;     // split next to the main map once
        bool dirty = true;
        ImVec2 size{0, 0};
        float pixelScale = 1.0f;
        int detailLevel = -1;
    };
    std::vector<MapView> views_;
    int nextViewId_ = 2;
    int focusedViewId_ = 0;      // map panel focused in the last frame (0 = none)
    int closeViewId_ = 0;        // map panel to close (Ctrl+W, View menu)
    int mouseInPanel_ = -1, prevMouseInPanel_ = -1; // map panel under the mouse (0 = main)
    bool viewsStale_ = false;    // the main map was redrawn: so must the panels

    // Swipe (app_swipe.cpp): the main map left of a draggable divider, a
    // comparison right of it (another layer, or the same one at another date or
    // in another mode), drawn like a map panel into a target of its own.
    static constexpr int kSwipeSlot = 1;      // GPU target ("Map" is 0, panels 2, 3...)
    bool swipe_ = false;
    float swipeX_ = 0.5f;                     // divider, fraction of the canvas width
    bool swipeDragging_ = false;
    MapView swipeView_;
    void toggleSwipe();
    void uiSwipeBar();
    bool swipeInput(ImVec2 origin, ImVec2 size);
    void renderSwipe(ImVec2 size, float pixelScale);
    void drawSwipe(ImDrawList* dl, ImVec2 origin, ImVec2 size, float pixelScale);

    // Space-time transect (app_transect.cpp): a line on the map; the Transect
    // panel shows distance x date in the layer's colours (a Hovmoeller diagram).
    static constexpr int kTransectSlot = -1;  // GPU target of the image
    struct TransectExact;                     // full-resolution rows read in the background
    struct Transect {
        bool on = false;
        uint64_t cube = 0;                    // layer (CubeInfo id) it was sampled on
        ImVec2 a{0, 0}, b{0, 0};              // end points, active layer's pixels
        bool geo = false;                     // georeferenced end points (kept across layers)
        double ga[2] = {0, 0}, gb[2] = {0, 0};
        int n = 0, T = 0;                     // samples, dates
        std::vector<int> px, py;              // full-resolution pixel of each sample
        double length = 0;                    // A -> B in `unit`
        std::string unit;
        std::vector<float> values;            // [t][i]: exact rows where read, else the overview
        std::vector<char> state;              // per date: 0 no data yet, 1 overview, 2 full resolution
        std::vector<float> shown;             // values or anomalies (the image)
        std::shared_ptr<TransectExact> job;
        bool pending = false;                 // HDD: the exact read waits for the overview
        bool dirty = false;                   // rows changed since the image was made
        double published = -1;                // when (ImGui time)
        int kind = 0;                         // 0 values, 1 anomaly (- each sample's mean)
        Range range;                          // used when the map has none for that mode
        GpuTex tex = 0;                       // `shown` as a float texture
        uint64_t version = 0, drawnKey = ~0ull;
        bool fit = true;                      // reset the panel's axes
        int hoverI = -1, hoverT = -1;         // cell hovered in the panel
        bool docked = false;
    } tr_;
    bool transectMode_ = false;               // a drag on the map draws the line (T)
    int transectPanel_ = -1;                  // map panel where the line is being drawn
    bool transectCancel_ = false;             // Esc during the drag
    ImVec2 transectStart_{0, 0}, transectEnd_{0, 0};
    void setTransect(ImVec2 a, ImVec2 b);
    void clearTransect();
    void startTransectExact();
    int transectRowsRead() const;             // dates read at full resolution so far
    void pumpTransect();
    bool updateTransectRows();                // true if a row changed
    void publishTransect();                   // values -> image (anomaly, range, texture)
    void transectColors(int& cmap, float& lo, float& hi) const;
    bool transectInput(int panel, double sx, double sy);
    void drawTransectMarks(ImDrawList* dl, ImVec2 origin);
    void renderTransect(int w, int h);
    void uiTransect();
    void copyTransectCsv();
    // View menu entries and keys of both (S, T, Esc).
    void uiCompareMenu();
    void compareShortcuts();
    const char* selfTestCompare();
    std::shared_ptr<RoiData> roi_;
    int roiSeen_ = -1;
    std::vector<float> roiMean_, roiP10_, roiP90_;
    SeriesStats roiStats_;
    bool showRoiBand_ = true;
    int nextPinId_ = 1;
    bool pendingRoi_ = false;    // ROI requested on an HDD while the overview builds
    int pendingRoiRect_[4] = {0, 0, 0, 0};
    bool roiRect_ = false;       // the map has an ROI rectangle (roiRectXY_, active pixels)
    int roiRectXY_[4] = {0, 0, 0, 0};
    uint64_t roiGen_ = 1;        // changes with every new or cleared ROI
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

    // Export
    std::vector<std::shared_ptr<ExportJob>> exports_;
    bool showExports_ = false;
    int exportKind_ = 0;         // options popup: 0 none, 1 PNG, 2 values, 3 rendered view
    bool exportPopup_ = false;   // open it in this frame
    PngOptions png_;
    int viewScale_ = 1;
    bool valuesWhole_ = false;   // whole image, else the visible area
    std::string exportDir_;      // folder of the last export

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
    // through their geotransforms (same CRS, no rotation) or reprojected.
    struct LayerDisplay {
        int mode = ModeValue;
        std::array<int, 3> rgb{0, 0, 0};
        int diffRef = 0;
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
        bool aligned = true;          // can be placed on the map (straight, or through reproj)
        std::string alignNote;
        // Another CRS than the active layer's, or a rotated grid: placed through
        // OGR/PROJ (exact on the CPU, a warp grid on the GPU) instead of ax..by.
        std::unique_ptr<Reprojection> reproj;
        // The map's ROI on this layer (non-active layers, "All visible layers").
        std::shared_ptr<RoiData> roi; // null: not started, or the ROI misses the layer
        uint64_t roiGen = 0;          // roiGen_ it belongs to
        int roiSeen = -1;
        std::vector<float> roiMean, roiP10, roiP90;
        SeriesStats roiStats;
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
        int hits = 0;
        int64_t budget = 0;   // overview budget to restore
        int frames = 0;
    } st_; // --selftest-ui state
    struct {
        int stage = 0;
        std::shared_ptr<CubeInfo> info;
        std::vector<std::pair<int, int>> px;
        std::array<int, 4> roi{};
        double t0 = 0;
    } measure_; // --measure-cache state

};
