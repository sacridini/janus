#pragma once

#include <atomic>
#include <chrono>
#include <cstdint>
#include <functional>
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
    bool pixel = false, raster = false;
    std::vector<ZeitParam> params;
    std::vector<ZeitOutput> outputs;
};

struct CubeInfo;
// Why `tool` cannot run on this series, from its manifest requirements ("" = it can).
std::string toolApplicability(const ZeitTool& tool, const CubeInfo& info);

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
    void* process_ = nullptr;
    void* stdinWrite_ = nullptr;
    void* stdoutRead_ = nullptr;
    std::thread reader_;
};

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
    uint64_t runPixel(const std::string& toolId, const json& params, const std::vector<double>& years,
                      const std::vector<float>& values);
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
