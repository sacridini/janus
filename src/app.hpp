#pragma once

#include <array>
#include <future>
#include <memory>
#include <string>
#include <vector>

#include <imgui.h>

#include "gpu.hpp"
#include "session.hpp"
#include "stats.hpp"

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
};

// Startup timings in ms since process start (shown in the Performance panel).
struct StartupTimes {
    double gdalMs = 0, windowMs = 0, uiMs = 0, openMs = 0, firstFrameMs = 0;
};

struct AppOptions {
    int band = 1;
    int64_t budgetMB = 1024;
    int ioThreads = 0;
};

class App {
public:
    explicit App(GLFWwindow* window);
    ~App();
    bool init(const AppOptions& opts, std::string& error);
    void openInputs(const std::vector<std::string>& inputs);
    void frame();
    bool wantsContinuousFrames() const;
    void setStartupTimes(const StartupTimes& t) { startup_ = t; }

    std::vector<std::string> pendingDrop; // filled by the drag-and-drop callback

private:
    void closeSession();
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
    void renderMap(int w, int h);
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

    GLFWwindow* window_;
    AppOptions opts_;
    SessionSettings settings_;
    Gpu gpu_;
    std::unique_ptr<Session> s_;
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
    bool roiDragging_ = false;
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
    double frameMs_ = 0;
    int budgetUi_ = 1024;
    StartupTimes startup_;
};
