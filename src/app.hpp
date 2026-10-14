#pragma once

#include <array>
#include <atomic>
#include <chrono>
#include <functional>
#include <future>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include <imgui.h>

#include "basemap.hpp"
#include "file_browser.hpp"
#include "geom.hpp"
#include "platform.hpp"
#include "gpu.hpp"
#include "reproject.hpp"
#include "results.hpp"
#include "session.hpp"
#include "stats.hpp"
#include "theme.hpp"
#include "vectors.hpp"
#include "zeit_client.hpp"

struct GLFWwindow;
struct EmbeddingExport;

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
    int64_t budgetMB = 0; // --budget, this run only (0: the Settings value)
    int ioThreads = 0;    // --threads: background readers, this run only (0: from the processing threads)
    std::string zeitPython;  // developer override of the bundled runtime
    std::string zeitBridge;
};

const char* modeName(int mode); // display mode (DisplayMode) as shown in the interface
const char* modeName(int mode, const CubeInfo& info); // same, "RGB composite (3 bands)" on a spectrum
constexpr int kPinColorCount = 8;
ImVec4 pinColor(int id);        // pin `id` (from 1) on the map and, made legible, on the chart
// Reference date of the difference mode at date t: a fixed date, or the
// previous one (ref < 0; -1 at the first date: no difference).
int diffRefDate(int ref, int t, int T);
// Index of the value nearest to x in the sorted xs (a date on a chart's axis).
int nearestIndex(const std::vector<double>& xs, double x);

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
// Runs `work` in a background thread as an export (Exports window); it returns
// false with the error set on failure. Progress in [0, 1] wakes the interface.
std::shared_ptr<ExportJob> startExportJob(std::string title, std::string path,
                                          std::function<bool(ExportJob&, std::string&)> work);
void setExportProgress(ExportJob& j, double p);

class App {
public:
    explicit App(GLFWwindow* window);
    ~App();
    bool init(const AppOptions& opts, std::string& error);
    // Opens a series; `addLayer` keeps the open ones and adds it as a new layer.
    void openInputs(const std::vector<std::string>& inputs, bool addLayer = false);
    // Before ImGui::NewFrame: the interface font and its size (Settings).
    void applyFont();
    void frame();
    bool wantsContinuousFrames() const;
    static constexpr int kDefaultFontSize = 13, kMinFontSize = 10, kMaxFontSize = 28;
    void setStartupTimes(const StartupTimes& t) { startup_ = t; }
    // --selftest-ui: one step per frame; -1 while running, then the exit code.
    int selfTestStep(const std::vector<std::string>& inputs);
    // --measure-cache on|off IN: times the full-resolution cache (app_fullres.cpp).
    int measureCacheStep(const std::vector<std::string>& inputs);
    // --measure-vectors IN VEC...: times reading, placing, drawing, picking and
    // extracting vector layers over a series (app_vectors.cpp).
    int measureVectorsStep(const std::vector<std::string>& inputs);
    // --selftest-embeddings-ui DIR [OUT]: a series of embeddings in every view,
    // checked and saved as PNGs (app_embeddings.cpp).
    int selfTestEmbeddingStep(const std::vector<std::string>& inputs);

    std::vector<std::string> pendingDrop; // filled by the drag-and-drop callback
    bool pendingDropAdd = false;          // dropped with Shift held: added as layers

private:
    void closeAll();
    void finishOpen();
    void uiMenu();
    void uiDockspace();
    void dockDefaultLayout(unsigned dock); // ImGuiID of the dock space
    void uiMap();
    void uiLayer();
    void uiSeries();
    void drawRoiBoxes(const char* label, ImVec4 col, const std::function<double(double)>& f);
    void uiStats();
    void uiPerf();
    void uiPopups();
    void handleShortcuts();
    void resetLayout();          // panels docked as at the first start, floating windows as when first opened
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
    // "Copy Python" (app_python.cpp): which = 0 the cursor, a pin's id, -1 the ROI.
    std::string pythonSnippet(int which);
    void uiCopyPython();
    std::string pythonCopied_;   // the last code copied (self-test)
    int selfTestPython();        // --selftest-ui stages 150-154

    // Export (app_export.cpp): the map as PNG, values and the rendered view as
    // GeoTIFF, Zeit results. Rendering is on the GPU (main thread); reading the
    // data and writing files in the background, listed in the Exports window.
    struct PngOptions {
        int scale = 1;           // x the on-screen resolution (rendered at it, not upscaled)
        int background = 0;      // 0 the map's (dark), 1 white, 2 transparent
        bool label = true;       // date and mode, top left
        bool legend = true;      // colour bar or classes, bottom left
        bool marks = false;      // pins and ROI
        bool scaleBar = false;   // ground distance, bottom right
    };
    // What is drawn over a rendered map (PNG, animation frames).
    struct FigureOverlay {
        int label = 1;           // 0 none, 1 date and mode (one line), 2 the date stamp of an animation (large)
        bool legend = true, marks = false, scaleBar = false;
    };
    // Over `img` (w x h RGBA rows, rendered by renderExport at `scale`): the
    // label, legend, marks, scale bar and the basemap's credit, as the map draws them.
    void drawFigureOverlay(std::vector<unsigned char>& img, int w, int h, int scale, const FigureOverlay& o);
    // Metres on the ground per pixel of the active layer at its pixel (x, y),
    // along a row (geodesic, whatever the CRS); 0 when it cannot be known.
    double groundMetresPerPixel(double x, double y) const;
    void uiExportMenu();         // File menu entries
    void uiExport();             // options popup
    void uiExports();            // progress window
    void uiResultExportMenu(const ResultLayer& r, const char* label); // menu item: save it as GeoTIFF
    void exportShortcuts();      // Ctrl+E, Ctrl+Shift+E/V/M, Ctrl+S, Ctrl+Shift+S, Ctrl+J
    // Zeit results: the one drawn on top (else the active layer's last, else the
    // last), the outputs of its run (the results in the same folder), a file name
    // made of the layer's name and the output's.
    const ResultLayer* topResult() const;
    std::vector<const ResultLayer*> runOutputs(const ResultLayer& r) const;
    std::string resultFileName(const ResultLayer& r) const;
    void saveResult(const ResultLayer& r); // asks where (system dialog), then copies it
    void openSaveRun(const ResultLayer& r); // the "Save all outputs" popup, for r's run
    std::vector<std::shared_ptr<ExportJob>> exportRun(const ResultLayer& r, const std::string& folder);
    void uiSaveRun();
    bool exportScaleFits(int scale) const;
    // The main map's view rendered offscreen at `scale` x its on-screen resolution,
    // RGBA rows top-down; `transparent`: alpha = coverage (two renders), else over `bg`.
    void renderExport(int scale, bool transparent, const float bg[4], std::vector<unsigned char>& rgba, int& w,
                      int& h);
    std::shared_ptr<ExportJob> exportPng(const std::string& path, const PngOptions& o);
    std::shared_ptr<ExportJob> exportValues(const std::string& path, bool wholeImage);
    std::shared_ptr<ExportJob> exportView(const std::string& path, int scale);
    std::shared_ptr<ExportJob> exportResult(const ResultLayer& r, const std::string& path);
    // The embedding view shown (principal components, similarity, change) at full
    // resolution, as Float32 GeoTIFF bands (app_embeddings.cpp sets it up).
    std::shared_ptr<ExportJob> exportEmbeddings(const std::string& path, const EmbeddingExport& e);
    bool visibleWindow(int win[4]) const; // visible part of the active layer, in its pixels
    std::string exportName(const std::string& suffix) const;
    // `folder`: where the dialog starts (empty: the last export's folder).
    std::string askSavePath(const char* title, const std::string& name, const char* filter, const char* ext,
                            const std::string& folder = "");
    std::string exportFolder() const; // the last export's, else the active layer's
    // The path typed in the export popup: absolute (from exportFolder()), with `ext`.
    std::string exportTarget(const char* ext) const;
    const char* selfTestExports(); // nullptr = passed

    // Animation (app_animation.cpp): the map over a range of dates, rendered
    // offscreen one frame per UI frame (the view frozen behind a modal progress
    // popup) and encoded in the background: our own GIF writer (gif.hpp), or
    // MP4 / WebM through an external ffmpeg fed raw frames on its standard input.
    struct AnimOptions {
        int from = 0, to = -1;       // dates (to < 0: the last)
        int step = 1;                // every n-th date
        float fps = 4.f;
        int scale = 1;               // x the on-screen resolution
        int format = 0;              // 0 GIF, 1 MP4 (H.264), 2 WebM (VP9)
        int background = 0;          // 0 the map's (dark), 1 white
        bool stamp = true, legend = true, scaleBar = true, marks = false;
        bool dither = false;         // GIF: error diffusion (smoother gradients, larger files)
    } anim_;
    struct AnimRun;                  // the export being rendered
    std::shared_ptr<AnimRun> animRun_;
    std::string ffmpegPath_;         // Settings: "" = next to Janus, else on PATH
    static const char* animExt(int format);
    std::string findFfmpeg() const;  // the ffmpeg that would be used, "" if none
    std::vector<int> animDates(const AnimOptions& o) const;
    void uiAnimationOptions(bool& canSave); // the export popup's options (exportKind_ 5)
    std::shared_ptr<ExportJob> startAnimation(const std::string& path, const AnimOptions& o);
    void pumpAnimation();            // once per frame: the next date, its frame when ready
    void uiAnimation();              // the progress popup
    void uiFfmpegSettings();         // Settings window
    int selfTestAnimation(const std::string& a); // --selftest-ui stages 130-139 (A reopened)

    // Settings window (app_settings.cpp): theme, processing threads, memory and
    // caches, kept in the layout .ini.
    void registerSettings();
    void uiSettings();
    void applyTheme();               // theme_ to ImGui, ImPlot and the cursor series
    void applyThreads();             // settings_.threads to the open series and Zeit
    void applyOverviewBudget();      // for the series opened from now on
    int overviewBudgetMB() const { return opts_.budgetMB > 0 ? int(opts_.budgetMB) : overviewBudgetMB_; }
    void clearOverviewCache();
    const char* selfTestSettings();  // nullptr = passed
    int selfTestZeitThreads();       // --selftest-ui stages 50-52
    void openSettings() { showSettings_ = focusSettings_ = true; }
    bool showSettings_ = false, focusSettings_ = false;
    int theme_ = 0;                  // theme::Id
    int appliedTheme_ = -1;
    int fontSize_ = kDefaultFontSize; // interface text, in points
    int appliedFontSize_ = -1;
    ImFont* fontBitmap_ = nullptr;   // ImGui's pixel font, at its 13 px
    ImFont* fontVector_ = nullptr;   // ImGui's scalable font, at any other size
    bool revealOpened_ = true;       // the Files panel goes to the series opened
    int overviewBudgetMB_ = 1024;    // Settings value (--budget overrides it for one run)

    // Full-resolution cache (app_fullres.cpp)
    void uiFullRes();
    void uiFullResSettings();
    int selfTestFullRes(); // --selftest-ui stages 30-32 (app_selftest.cpp)
    int selfTestReproject(const std::string& f); // stages 40-45: F, B reprojected (app_selftest.cpp)
    int selfTestBasemap();                       // stages 50-55: a local tile pyramid (app_selftest.cpp)
    int selfTestSpectra(const std::string& g);   // stages 90-100: G, hyperspectral (app_selftest.cpp)
    // Stage 100: G's spectrum in principal components and similarity (app_embeddings.cpp).
    int selfTestSpectralViews(const std::function<double(int, int, int)>& expected, int T);

    // Basemap (app_basemap.cpp): web imagery or a map under every layer, in
    // Web Mercator, placed on the active layer's grid (basemap.hpp).
    void registerBasemapSettings();
    void pumpBasemap();               // once per frame: source, placement, uploads
    void retireBasemap();             // GPU resources now, the rest in the background
    bool basemapShown() const;        // on, a source picked and placed on the active layer
    BasemapSource basemapSource() const; // the chosen preset, or the custom URL
    bool basemapCustomOk(std::string* why = nullptr) const;
    std::string basemapAttribution() const; // "" when not shown
    // Into the current map target, first: `view` = map-space rectangle,
    // `toTarget` = map space -> target pixels (x0, y0, x1, y1).
    void drawBasemap(const double view[4], double pxPerMapPx, const float bg[4], float targetW, float targetH,
                     const std::function<void(const double*, float*)>& toTarget);
    void drawBasemapNote(ImDrawList* dl, ImVec2 origin, ImVec2 size); // attribution and state, bottom right
    void uiBasemap();                 // Layers panel
    void toggleSatelliteBasemap();    // S key
    void uiBasemapPerf();             // Performance panel
    struct BasemapUi {                // the setting (layout .ini)
        std::string source = "none";  // "none", a preset's id or "custom"
        bool on = false;              // not kept: hidden in each session and each series opened until ticked
        float opacity = 1.0f;
        std::string url;              // custom XYZ template
        int maxZoom = 19;
        int tileSize = 256;
        std::string attribution;
    } bm_;
    char bmUrlEdit_[1024] = "";       // custom fields being edited
    char bmAttrEdit_[256] = "";
    std::unique_ptr<Basemap> basemap_;
    std::string basemapWhy_;          // why it is not drawn (e.g. the active layer has no CRS)
    uint64_t basemapFailedFor_ = 0;   // CubeInfo id it could not be placed on
    std::vector<std::future<void>> retiredBasemaps_;

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
    // Log window (Tools > Log): a raster job's log or zeit.log (the process
    // that fits the chart), followed as the file grows.
    void uiZeitLog();
    void showZeitLog(const std::string& path);
    std::string zeitLogPath() const; // zeit.log
    void uiResultsOf(uint64_t cubeId);
    void clearResults(uint64_t cubeId = 0); // 0 = every layer
    void drawZeitOverlays(const char* label, const json& result, const SeriesStats& st);
    // Typical series of the class under the cursor in visible class maps that have them (SOM).
    void drawClassSeries(const SeriesStats& st);

    // Layers (app_layers.cpp)
    struct LayerDisplay;
    struct SeriesLayer;
    struct EmbeddingLayer; // app_embeddings.cpp
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
    void uiSpectralIndex(); // a spectrum: an index of its bands added as a layer
    bool addSpectralIndexLayer(const SpectralIndex& s); // over the active spectrum; false if it cannot
    int spectralIndex_ = 0; // its choice among the indices the spectrum covers
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
    std::string openingName_;                 // name of the layer it adds ("" = from its files)
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
        bool basemapOnly = false; // the basemap alone (no layer), e.g. as the swipe's comparison
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

    // Analysis panel (app_analysis.cpp): a series by day of the year, the
    // classes over time and their transitions, a scatter of two layers/dates.
    struct ClassTimeline {
        uint64_t key = 0;                          // layer, dates and classes counted
        std::vector<int> values;                   // class values (columns)
        std::vector<std::vector<uint32_t>> counts; // [t][k] pixels of the overview
        std::vector<uint32_t> valid;               // [t] pixels with a class
        int next = 0;                              // next date to count (T: complete)
    };
    struct Transitions {
        uint64_t key = 0;
        int from = -1, to = -1;
        bool done = false;
        std::vector<int> values;   // class values
        std::vector<uint32_t> m;   // [from class][to class] pixels
        uint64_t both = 0;         // pixels with a class at both dates
    };
    struct ScatterData {
        uint64_t key = 0, activeCube = 0;
        std::vector<double> x, y;
        int n = 0, step = 1;
        double r = NAN, a = NAN, b = NAN, meanDiff = NAN, rmsd = NAN;
    };
    struct AnalysisUi {
        std::string seasonalSource = "cursor"; // "cursor", "pin<id>", "roi"
        int seasonalView = 0;                  // 0 years overlaid, 1 year x day heatmap
        bool classStacked = true;
        int transFrom = 0, transTo = -1;       // dates; -1 = the current date
        uint64_t scatterCube[2] = {0, 0};      // X and Y layers (0 / unknown: the active one)
        int scatterDate[2] = {-2, -1};         // a date, -1 the current one, -2 the one before
        int scatterT[2] = {0, 0};              // the dates they resolve to
        int selectTab = -1;                    // a tab to bring to the front (self-test)
        ClassTimeline classes;
        Transitions trans;
        ScatterData scatter;
    } an_;
    bool showAnalysis_ = false;
    void uiAnalysis();
    void uiSeasonal();
    void uiClassTimeline();
    void uiScatter();
    bool countClassTimeline(const SeriesLayer& L, ClassTimeline& c, double budgetMs);
    void countTransitions(const SeriesLayer& L, int from, int to, Transitions& tr) const;
    void computeScatter(ScatterData& sc, const SeriesLayer& X, int tx, const SeriesLayer& Y, int ty) const;
    const char* selfTestAnalysis(int part); // nullptr = passed
    void copyTransectCsv();
    // View menu entries and keys of both (S, T, Esc).
    void uiCompareMenu();
    void compareShortcuts();
    const char* selfTestCompare();
    std::shared_ptr<RoiData> roi_;
    int roiSeen_ = -1;
    std::vector<float> roiMean_, roiP10_, roiP90_, roiP25_, roiP50_, roiP75_;
    SeriesStats roiStats_;
    int roiSpread_ = 1;          // ROI on the chart: 0 mean only, 1 + p10-p90 band, 2 + box plot per date
    bool showRoiBand_ = true;    // roiSpread_ > 0 (the other layers' ROI: a band)
    int nextPinId_ = 1;
    bool pendingRoi_ = false;    // ROI requested on an HDD while the overview builds
    int pendingRoiRect_[4] = {0, 0, 0, 0};
    bool roiRect_ = false;       // the map has an ROI (roiRectXY_: its bounding box, active pixels)
    int roiRectXY_[4] = {0, 0, 0, 0};
    Polygon roiPoly_;            // a polygonal ROI (active pixels; empty: the rectangle roiRectXY_)
    uint64_t roiGen_ = 1;        // changes with every new or cleared ROI

    // Polygonal ROI (app_roi.cpp): R, then clicks add vertices and a drag draws
    // a freehand outline (lasso); Enter, a double click or the first vertex
    // closes it. Its statistics are those of the pixels whose centres are inside.
    bool polyMode_ = false;
    std::vector<ImVec2> polyDraft_; // vertices so far (active pixels)
    int polyPanel_ = -1;         // map panel where the left button went down in polygon mode
    ImVec2 polyPress_{0, 0};     // where (active pixels)
    bool polyLasso_ = false;     // and moved since: this stroke is freehand
    bool polyDouble_ = false;    // a double click: closes the polygon
    // A new ROI: `r` its bounding box (active pixels), `poly` its outline (empty:
    // the rectangle); the active layer's statistics start (or wait for the HDD).
    void setRoiShape(const int r[4], const Polygon& poly);
    bool setRoiPolygon(const Polygon& poly); // false if it encloses no pixel's centre
    bool roiContains(double x, double y) const; // active pixels; the whole map without an ROI
    // The map's ROI on layer L, in its own pixels (reprojected or scaled); empty
    // for a rectangle on a layer on the same grid lines.
    Polygon roiOnLayer(const SeriesLayer& L) const;
    bool polygonInput(int panel, double sx, double sy);
    void finishPolygon();
    void roiShortcuts();        // R, Enter, Backspace, Esc
    void drawRoiMarks(ImDrawList* dl, ImVec2 origin);
    int selfTestRoiPolygon(const std::vector<std::string>& in); // --selftest-ui stages 110-115

    // Vector layers (app_vectors.cpp): points, lines and polygons read through
    // OGR (GeoJSON, Shapefile, GeoPackage, KML, CSV of points...) drawn over
    // every map, placed on the active layer's grid from any CRS; a polygon as
    // the ROI (Alt + click); the series at every feature written to a table.
    struct VectorLayer {
        int id = 0;
        std::shared_ptr<VectorData> data;
        std::string name;
        bool visible = true;
        ImVec4 color{1.0f, 0.45f, 0.15f, 1.0f};
        uint64_t placedFor = 0;                 // vectorPlaceKey() it was placed for (0: not yet)
        std::string why;                        // why it is not on the map ("": placed)
        bool offMap = false;                    // placed, but nothing of it over the active layer
        std::vector<Polygon> px;                // each feature in the active layer's pixels
        std::vector<std::array<float, 4>> box;  // their bounding boxes (x0, y0, x1, y1; empty: x0 > x1)
        std::vector<ImVec2> anchor;             // a point of each (inside an area; NaN: none)
    };
    std::vector<VectorLayer> vectors_;
    int nextVectorId_ = 1;
    struct VectorLoad {
        std::vector<std::shared_ptr<VectorData>> layers;
        std::string error;
    };
    std::vector<std::future<VectorLoad>> vectorLoads_;
    int hoverVector_ = -1, hoverFeature_ = -1; // vectors_ index and feature under the cursor (-1: none)
    // Splits inputs: vector files are loaded as vector layers; returns the rest.
    std::vector<std::string> takeVectorInputs(const std::vector<std::string>& inputs);
    void addVectorFiles(const std::vector<std::string>& paths);
    void pumpVectors();                          // finished loads; placing on the active layer's grid
    uint64_t vectorPlaceKey() const;             // the active layer's CRS and grid
    void placeVector(VectorLayer& V);
    void drawVectorMarks(ImDrawList* dl, ImVec2 origin, bool forExport = false);
    // The feature under (x, y), active pixels, within `tol` screen points of a point or a line.
    bool vectorFeatureAt(double x, double y, int& layer, int& feature, float tol = 6.0f) const;
    bool featureAsRoi(const VectorLayer& V, int feature); // false if it is no polygon or misses the map
    void centerOnFeature(const VectorLayer& V, int feature, double minScale);
    VectorLayer* vectorById(int id);
    void uiVectors();                            // the Layers panel's section
    void uiVectorTooltip();                      // attributes of the feature under the cursor
    void removeVector(int index);
    std::string vectorError_;
    // Extract series: every feature's series (points: the pixel; polygons: the
    // mean of the pixels inside) of the active or every visible layer, as CSV.
    struct ExtractUi {
        bool open = false;                       // open the popup in this frame
        int vector = 0;                          // VectorLayer id
        bool allLayers = false;
        bool wide = true;                        // a row per feature, else a row per feature and date
        char path[1024] = "";
    } extract_;
    void openExtract(int vectorId);
    void uiExtract();
    std::shared_ptr<ExportJob> extractSeries(const VectorLayer& V, bool allLayers, bool wide, const std::string& path);
    int selfTestVectors(const std::vector<std::string>& in); // --selftest-ui stages 120-125

    // Interpretation (app_labels.cpp): a vector layer's features one after the
    // other (N / Shift+N), the map centred on each, its series on the chart (a
    // pin; a polygon becomes the ROI), labelled with keys 1-9, a date of change
    // (D: the map's date) and notes; saved as it goes, exported as a table.
    struct LabelEntry {
        std::string label;   // a class of the label set ("" = not labelled)
        std::string date;    // date of the change, as the series shows it ("" = none)
        std::string notes;
    };
    struct Interpretation {
        int vector = 0;                      // VectorLayer id (0: none)
        int current = -1;                    // feature shown
        std::vector<std::string> classes;    // the label set (keys 1-9)
        std::vector<LabelEntry> entries;     // one per feature
        bool advance = false;                // to the next feature after a label
        bool skipLabelled = false;           // N / Shift+N skip labelled features
        float zoom = 4.0f;                   // at least this many screen points per pixel
        int pinId = 0;                       // the pin at the current feature (0: none)
        bool dirty = false;                  // saved after a short while
        double changedAt = 0;
        char notes[512] = "";
        char newClass[64] = "";
        char exportPath[1024] = "";
        bool exportOpen = false;
        bool scrollToCurrent = false;
    } lab_;
    bool showInterp_ = false;
    void startInterpretation(int vectorId);
    void stopInterpretation();
    void interpGo(int feature);
    void interpStep(int dir);                // +1 next, -1 previous
    void interpLabel(int k);                 // class k of the label set (-1: clear)
    void interpSetDate(bool clear);
    void interpShortcuts();
    void pumpInterpretation();               // autosave
    void uiInterpretation();
    std::string labelsFile() const;          // app data: the session's labels
    void saveLabels();
    void loadLabels();
    std::shared_ptr<ExportJob> exportLabels(const std::string& path);
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
    int exportKind_ = 0;         // options popup: 0 none, 1 PNG, 2 values, 3 rendered view, 4 embeddings
    bool exportPopup_ = false;   // open it in this frame
    PngOptions png_;
    int viewScale_ = 1;
    bool valuesWhole_ = false;   // whole image, else the visible area
    std::string exportDir_;      // folder of the last export (kept in the layout .ini)
    char exportPath_[1024] = ""; // options popup: where the file goes (editable, or Browse...)
    struct SaveRunUi {
        bool open = false;       // open the popup in this frame
        std::string of;          // path of one output of the run (results are removed by index)
        char folder[1024] = "";
    } saveRun_;

    // Zeit
    std::unique_ptr<ZeitClient> zeit_;
    // Serve process started with a new processing threads setting: replaces
    // zeit_ once it is ready and zeit_ has no request pending (pumpZeit).
    std::unique_ptr<ZeitClient> zeitNext_;
    std::vector<std::future<void>> zeitRetired_; // replaced serve processes shutting down
    std::chrono::steady_clock::time_point threadsChanged_{}; // last change of the processing threads
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
    bool uiLayersParam(const ZeitParam& p, json& v); // "layers": result maps a tool runs on
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
    int pixelMapT_ = -1;         // the map's date of a tool fitted on it (ZeitTool::mapDate)
    std::vector<double> zeitYears_; // decimal year of each date
    uint64_t roiZeitReq_ = 0;
    int roiZeitVersion_ = -1;
    json roiZeitResult_;
    std::map<uint64_t, std::chrono::steady_clock::time_point> pixelSent_;
    double lastPixelFitMs_ = 0;
    std::vector<std::shared_ptr<ZeitJob>> jobs_;
    std::map<const ZeitJob*, uint64_t> jobCube_; // cube id each job was started for
    std::vector<std::string> seriesToOpen_; // series outputs of finished jobs (e.g. NDFI), opened as layers
    bool showTasks_ = false;
    bool showZeitLog_ = false;
    struct LogView {
        std::string path;          // the file shown ("": the newest job's, else zeit.log)
        std::string shown;         // the file `text` comes from
        std::string text;          // its end (up to a few MB), without '\r'
        std::vector<size_t> lines; // where each line starts in text
        uintmax_t size = 0;        // bytes of the file read so far
        bool cut = false;          // the file's start is not in text
        double checked = -1;       // ImGui time of the last look at the file
    } zeitLog_;
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

    // RGB composite of three bands of each date's file (app_composite.cpp), drawn
    // instead of the series' quantity; the cube, the chart and the cursor keep it.
    struct BandComposite {
        BandComposite() = default;
        ~BandComposite(); // its textures
        BandComposite(const BandComposite&) = delete;
        BandComposite& operator=(const BandComposite&) = delete;
        bool on = false;
        std::array<int, 3> bands{0, 0, 0};       // R, G, B: bands of each date's file (1-based)
        bool fixedDate = false;                  // a date of its own, else the layer's
        int date = 0;
        float lo[3] = {0, 0, 0}, hi[3] = {1, 1, 1}; // stretch of each channel (band values)
        bool stretched = false;                  // set from the first image read (kept for every date)
        uint64_t key = 1;                        // bands and stretch the images were made with
        struct Img {
            GpuTex tex = 0;
            uint64_t key = 0;
            int w = 0, h = 0;
            double x0 = 0, y0 = 0, x1 = 0, y1 = 0; // the window, in the layer's pixels
            double used = 0;                     // ImGui time it was last drawn (cache)
        };
        std::map<int, Img> imgs;                 // per date, at overview resolution
        Img detail;                              // the active layer's visible window at screen resolution
        int detailT = -1;
        // The last image read, as values (re-stretched at once, the cursor's values).
        struct Values {
            int t = -1, w = 0, h = 0;
            bool detail = false;
            double x0 = 0, y0 = 0, x1 = 0, y1 = 0;
            std::vector<float> v;                // 3 x w x h
        } last;
        struct Read {
            int t = -1;
            bool detail = false;
            uint64_t key = 0;
            double x0 = 0, y0 = 0, x1 = 0, y1 = 0;
            int w = 0, h = 0;
            std::future<std::vector<float>> f;
        };
        std::optional<Read> reading;
        uint64_t wantDetail = 0;                 // window asked for (view key) and since when
        double wantSince = 0;
        std::shared_ptr<CubeReader> reader;
        std::string error;
    };

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
        std::shared_ptr<EmbeddingLayer> emb; // a layer of embeddings (app_embeddings.cpp)
        std::unique_ptr<BandComposite> comp; // three bands as RGB (app_composite.cpp)
    };
    std::vector<SeriesLayer> layers_;
    int active_ = -1;

    // Embeddings (app_embeddings.cpp): layers of foundation-model embeddings
    // drawn through their principal components, the similarity to a reference
    // or the change between years; their download through Zeit.
    void initEmbeddingLayer(SeriesLayer& L);
    // RGB composites of three bands per date (app_composite.cpp).
    struct CompositePreset {
        const char* name;
        std::array<int, 3> bands;            // 0 where the series has no such band
    };
    std::vector<CompositePreset> compositePresets(const CubeInfo& info);
    bool compositeOn(const SeriesLayer& L) const { return L.comp && L.comp->on && !embeddingView(L); }
    int compositeDate(const SeriesLayer& L, int t) const; // the date drawn when the map shows t
    bool compositeReady(const SeriesLayer& L, int t) const;
    void pumpComposites();
    void drawComposite(const SeriesLayer& L, int t, float alpha, bool detail,
                       const std::function<void(const double*, float*)>& toTarget);
    void clearCompositeImages(BandComposite& C);
    std::string compositeTitle(const SeriesLayer& L, int t) const;
    std::string compositeStatusAt(double ax, double ay) const; // the band values under the cursor
    void uiComposite();                      // Display panel, layers with several bands per date
    int selfTestComposite(const std::vector<std::string>& in); // --selftest-ui stages 140-145
    void showSpectralAnalysis();                 // the Embeddings panel, the active spectrum in principal components
    int layerDate(const SeriesLayer& L) const;   // the date a layer shows
    SeriesLayer* embeddingLayer();               // the active layer if it holds embeddings, else the top one
    bool embeddingView(const SeriesLayer& L) const; // drawn as embeddings (not as one band)
    bool embeddingPixel(const SeriesLayer& L, double ax, double ay, size_t& p) const;
    // The map's ROI on L's store (x0, y0, x1, y1) and, for a polygon or another
    // CRS, `mask`: one cell per store pixel of rect, 1 where its centre is inside.
    bool embeddingRoi(const SeriesLayer& L, int rect[4], std::vector<char>* mask = nullptr) const;
    void updateEmbeddingRef(SeriesLayer& L, int t);
    uint64_t embeddingKey(const SeriesLayer& L, int t) const;
    void pumpEmbeddings();
    void drawEmbedding(const SeriesLayer& L, int t, float alpha,
                       const std::function<void(const double*, float*)>& toTarget);
    bool embeddingMapInfo(std::string& title, int& cmap, float& lo, float& hi, std::string& wait) const;
    std::string embeddingStatusAt(int ix, int iy) const;
    void drawEmbeddingNotes(ImDrawList* dl, ImVec2 origin, ImVec2 size);
    bool embeddingBounds(int scope, double ll[4], double& wM, double& hM, std::string& why) const;
    // The view of embeddingLayer() for a file, over the visible area or the whole
    // layer; false with `why` when it cannot be written yet (or is not shown).
    bool embeddingExportSpec(bool wholeLayer, EmbeddingExport& e, std::string& why);
    bool embeddingVisibleWindow(const SeriesLayer& L, int win[4]) const; // in L's pixels
    void startEmbeddingDownload();
    void pumpEmbeddingDownloads();
    void uiEmbeddingDownload(bool inLayers);
    void uiEmbeddings();
    bool showEmbeddings_ = false;
    struct {
        int source = 0;            // kSources
        int y0 = 2017, y1 = 2024;
        int res = 0;               // kResolutions
        int area = 0;              // 0 visible area, 1 ROI
    } embUi_;
    struct EmbDownload {
        std::shared_ptr<ZeitJob> job;
        std::string title, folder;
        bool opened = false;
        double endedAt = -1;       // ImGui time
    };
    std::vector<EmbDownload> embDownloads_;
    bool openingKeepActive_ = false; // the pending open adds a layer without making it active
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
        std::vector<std::shared_ptr<ExportJob>> jobs;
        bool swipeBefore = false;
        int threads = 0;      // processing threads setting to restore
        int fontSize = 0;     // font size setting to restore
        std::string reveal;   // the file the Files panel should go to
        double zeitT0 = 0;
        std::string dir;      // files the stage writes (removed at its end)
        std::vector<std::array<double, 2>> pts; // expected places (active pixels)
    } st_; // --selftest-ui state
    struct {
        int stage = 0;
        std::shared_ptr<CubeInfo> info;
        std::vector<std::pair<int, int>> px;
        std::array<int, 4> roi{};
        double t0 = 0;
    } measure_; // --measure-cache state

};
