#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include <nlohmann/json.hpp>

// Client side of the tsv <-> Zeit bridge (zeit_bridge/tsv_zeit_bridge.py).
// Zeit runs in a separate Python process: a crash or a slow import there never
// blocks or takes down the viewer.

using json = nlohmann::json;

struct ZeitParam {
    std::string id, label, type, help; // type: int | float | bool | enum
    json def;
    double min = 0, max = 0;
    std::vector<std::string> options, labels;
};

struct ZeitOutput {
    std::string id, name, colormap, unit;
};

struct ZeitTool {
    std::string id, name, category, description;
    std::string requiresTime; // "any", "annual" (one date per year) or "regular" (evenly spaced)
    int minDates = 0;
    int minPerYear = 0; // observations per year needed (e.g. 2 for seasonal models)
    // Multiband tools (e.g. CCDC): band roles they need / can use
    // ("blue", "green", "red", "nir", "swir1", "swir2", "thermal").
    std::vector<std::string> bands, optionalBands;
    bool pixel = false, raster = false;
    std::vector<ZeitParam> params;
    std::vector<ZeitOutput> outputs;
};

struct CubeInfo;
class CubeReader;

// Computing time of a raster job from the bridge's "estimate" reply:
// a fixed cost per chunk (full-width row bands) plus a cost per pixel.
double estimateJobSeconds(const json& estimate, int width, int height);
// Why `tool` cannot run on this series, from its manifest requirements ("" = it can).
std::string toolApplicability(const ZeitTool& tool, const CubeInfo& info);

// Band role -> band of each date (1-based) for multiband tools.
using BandRoles = std::map<std::string, int>;
// From the band names (e.g. "NIR", "SR_B5 (nir)", "swir1"); "Band N" names of 6+
// bands are taken as Blue, Green, Red, NIR, SWIR1, SWIR2 [, thermal].
BandRoles guessBandRoles(const CubeInfo& info);
// Required roles of `tool` that are not mapped ("" = none).
std::string missingBandRoles(const ZeitTool& tool, const BandRoles& roles);
// Extra fields of a pixel run: dates (days since 1970) and what the series
// shows in band roles. Multiband tools also need zeitPixelBands.
json zeitPixelExtras(const CubeInfo& info, const BandRoles& roles);
// Every mapped band and the raw QA codes of the pixel, one value per date.
json zeitPixelBands(CubeReader& reader, const CubeInfo& info, const ZeitTool& tool, const BandRoles& roles, int x,
                    int y);
// Fills the inputs of a raster job (VRTs written to `dir`/`stem`*.vrt for the
// shown band, the normalized-difference band, the QA band and the tool's
// bands; dates).
bool zeitJobInputs(const CubeInfo& info, const ZeitTool& tool, const BandRoles& roles, const std::string& dir,
                   const std::string& stem, json& spec, std::string& error);

struct ZeitConfig {
    std::string python;   // python.exe of the runtime
    std::string bridge;   // tsv_zeit_bridge.py
    std::string logPath;  // stderr of every bridge process goes here
    bool bundled = true;  // false = developer override (keep the user's environment)
};

// A running child process with line-based stdout.
class ZeitProcess {
public:
    ~ZeitProcess();
    bool start(const ZeitConfig& cfg, const std::vector<std::string>& args, std::string& error);
    bool writeLine(const std::string& line);
    void terminate();
    bool running() const;
    int exitCode() const;
    // Called from the reader thread for every stdout line.
    std::function<void(const std::string&)> onLine;
    std::function<void()> onExit;

private:
    void readLoop();
#ifdef _WIN32
    void* process_ = nullptr;
    void* stdinWrite_ = nullptr;
    void* stdoutRead_ = nullptr;
#else
    bool reap(bool wait) const; // collects the exit status once
    int pid_ = -1;
    int stdinFd_ = -1, stdoutFd_ = -1;
    mutable std::mutex waitM_;
    mutable bool reaped_ = false;
    mutable int status_ = 0;
#endif
    std::thread reader_;
};

// python executable of the bundled runtime in `runtimeDir` (Windows embeddable
// layout: python/python.exe; elsewhere a relocatable build: python/bin/python3).
std::string bundledPython(const std::string& runtimeDir);

// One raster run in its own process (cancel = terminate the process).
struct ZeitJob {
    enum class State { Running, Done, Failed, Cancelled };
    std::string title;
    std::string toolId;
    std::string outputDir;
    std::atomic<State> state{State::Running};
    std::atomic<double> progress{0};
    std::chrono::steady_clock::time_point started = std::chrono::steady_clock::now();
    double seconds = 0;
    std::mutex m;
    std::string message, error; // guarded by m
    json result;                // guarded by m
    bool resultsLoaded = false; // main thread only
    std::unique_ptr<ZeitProcess> proc;
    void cancel();
};

struct PixelReply {
    uint64_t id;
    bool ok;
    json result;
    std::string error;
};

class ZeitClient {
public:
    enum class State { Off, Starting, Ready, Failed };

    ~ZeitClient();
    // Starts the bridge in the background (returns immediately).
    void start(const ZeitConfig& cfg);
    State state() const { return state_.load(); }
    std::string error() const;
    std::string zeitVersion() const;
    std::string pythonVersion() const;
    double startupMs() const { return startupMs_.load(); }
    // Valid once state() == Ready (the vector is not modified afterwards).
    const std::vector<ZeitTool>& tools() const { return tools_; }
    const ZeitTool* tool(const std::string& id) const;

    // Per-pixel run: `years` are decimal years, NaN values = missing.
    // `extra`: more fields of the request (dates, bands, QA: see zeitPixelExtras).
    uint64_t runPixel(const std::string& toolId, const json& params, const std::vector<double>& years,
                      const std::vector<float>& values, const json& extra = json::object());
    // Any other request to the serve process (e.g. "estimate"); the reply
    // arrives through takeReplies() with the returned id.
    uint64_t call(const std::string& method, const json& params);
    std::vector<PixelReply> takeReplies();

    std::shared_ptr<ZeitJob> startJob(const json& spec, const std::string& specPath, const std::string& title);

    const ZeitConfig& config() const { return cfg_; }

private:
    void onLine(const std::string& line);

    ZeitConfig cfg_;
    std::unique_ptr<ZeitProcess> proc_;
    std::atomic<State> state_{State::Off};
    std::atomic<bool> shuttingDown_{false};
    mutable std::mutex m_;
    std::string error_, zeitVersion_, pythonVersion_;
    std::vector<ZeitTool> tools_;
    std::vector<PixelReply> replies_;
    std::atomic<uint64_t> nextId_{10};
    std::atomic<double> startupMs_{0};
    std::chrono::steady_clock::time_point t0_;
};
