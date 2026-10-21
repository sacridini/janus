#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include "glfw.hpp"

#include <cpl_conv.h>
#include <gdal_priv.h>
#include <imgui.h>
#include <implot.h>
#include <ogr_srs_api.h>

#include "app.hpp"
#include "cube.hpp"
#include "embedding.hpp"
#include "platform.hpp"
#include "profiler.hpp"
#include "render_backend.hpp"
#include "selftest.hpp"
#include "theme.hpp"
#include "usage.hpp"
#include "validation.hpp"

namespace fs = std::filesystem;

// PROJ/GDAL data bundled with the program (same version as the libraries).
static void configureBundledData() {
    const fs::path share = fs::u8path(platform::resourceDir()) / "share";
    std::error_code ec;
    if (fs::exists(share / "proj" / "proj.db", ec)) {
        const std::string proj = (share / "proj").u8string();
        const char* paths[] = {proj.c_str(), nullptr};
        OSRSetPROJSearchPaths(paths);
    }
    if (fs::exists(share / "gdal", ec)) CPLSetConfigOption("GDAL_DATA", (share / "gdal").u8string().c_str());
    // GDAL drivers built as plugins (netCDF, HDF5): the bundled ones only, next
    // to the libraries (Windows: the program's folder; Linux: lib/; macOS:
    // Frameworks/). A build tree has them from the post-build step.
    const fs::path exe = fs::u8path(platform::exeDir());
    for (const fs::path& plugins : {exe / "gdalplugins", exe / "lib" / "gdalplugins", exe / ".." / "Frameworks" / "gdalplugins"})
        if (fs::exists(plugins, ec)) {
            CPLSetConfigOption("GDAL_DRIVER_PATH", plugins.lexically_normal().u8string().c_str());
            break;
        }
}

static App* g_app = nullptr;

static void dropCallback(GLFWwindow* window, int count, const char** paths) {
    if (!g_app) return;
    g_app->pendingDrop.assign(paths, paths + count);
    // Shift held: added as layers. While dragging, the window may not have the
    // keyboard, so the OS is asked as well as GLFW.
    g_app->pendingDropAdd = platform::shiftHeld() || glfwGetKey(window, GLFW_KEY_LEFT_SHIFT) == GLFW_PRESS ||
                            glfwGetKey(window, GLFW_KEY_RIGHT_SHIFT) == GLFW_PRESS;
}

int main(int argc, char** argv) {
    PROFILE_THREAD("main");
    using Clock = std::chrono::steady_clock;
    const auto t0 = Clock::now();
    auto ms = [&] { return std::chrono::duration<double, std::milli>(Clock::now() - t0).count(); };

    platform::init();
    const std::vector<std::string> args = platform::commandLineArgs(argc, argv);

    AppOptions opts;
    std::vector<std::string> inputs;
    bool measureStartup = false; // dev option: print startup timings and exit after the first frame
    bool selftestZeit = false;   // dev option: run the Zeit path without a window
    bool selftestEmb = false;    // dev option: embedding kernels and PCA, with timings
    bool measureVal = false;     // dev option: time the strata, sample and estimators on a map
    bool selftestVal = false;    // dev option: sampling design, strata, sample and estimators of validation.hpp
    bool selftestEmbUi = false;  // dev option: a series of embeddings in every view, hidden window
    bool selftestUi = false;     // dev option: drive the layers workflow in a hidden window
    bool measureCache = false;   // dev option: time the full-resolution cache in a hidden window
    bool measureVectors = false; // dev option: time vector layers over a series in a hidden window
    bool profileTour = false;    // dev option: a fixed tour of a series in a hidden window (for Tracy)
    bool dumpGolden = false;     // dev option: maps and statistics for the Rust port to compare with
    std::string shots;           // dev option: a guide's screenshots (--shots TOUR), hidden window
    bool describe = false;       // dev option: print how the inputs open, without a window
    for (size_t i = 0; i < args.size(); ++i) {
        const std::string& a = args[i];
        auto next = [&]() -> const char* { return i + 1 < args.size() ? args[++i].c_str() : nullptr; };
        if (a == "-h" || a == "--help" || a == "/?") {
            platform::attachParentConsole();
            std::fputs(kUsage, stdout);
            return 0;
        } else if (a == "--version") {
            platform::attachParentConsole();
            std::printf("Janus %s (GDAL %s)\n", JANUS_VERSION, GDALVersionInfo("RELEASE_NAME"));
            return 0;
        } else if (a == "--band") {
            const char* v = next();
            opts.sel.band = v ? std::max(1, std::atoi(v)) : 1;
        } else if (a == "--budget") { // this run only: Settings keeps its own value
            const char* v = next();
            opts.budgetMB = v ? std::max(64, std::atoi(v)) : opts.budgetMB;
        } else if (a == "--threads") {
            const char* v = next();
            opts.ioThreads = v ? std::max(1, std::atoi(v)) : 0;
        } else if (a == "--zeit-python") {
            const char* v = next();
            if (v) opts.zeitPython = v;
        } else if (a == "--zeit-bridge") {
            const char* v = next();
            if (v) opts.zeitBridge = v;
        } else if (a == "--selftest-ui") {
            selftestUi = true;
        } else if (a == "--measure-cache") {
            measureCache = selftestUi = true;
        } else if (a == "--measure-vectors") {
            measureVectors = selftestUi = true;
        } else if (a == "--profile-tour") {
            profileTour = selftestUi = true;
        } else if (a == "--dump-golden") {
            dumpGolden = selftestUi = true;
        } else if (a == "--shots") {
            const char* v = next();
            shots = v ? v : "";
            selftestUi = true;
        } else if (a == "--describe") {
            describe = true;
        } else if (a == "--selftest-zeit") {
            selftestZeit = true;
        } else if (a == "--selftest-embeddings") {
            selftestEmb = true;
        } else if (a == "--selftest-validation") {
            selftestVal = true;
        } else if (a == "--measure-validation") {
            measureVal = true;
        } else if (a == "--selftest-embeddings-ui") {
            selftestEmbUi = selftestUi = true;
        } else if (a == "--measure-startup") {
            measureStartup = true;
        } else if (a.rfind("--", 0) == 0) {
            platform::attachParentConsole();
            std::fprintf(stderr, "Unknown option: %s\n\n%s", a.c_str(), kUsage);
            return 2;
        } else {
            inputs.push_back(a);
        }
    }

    // Developer override of the bundled Zeit runtime (also via environment).
    if (opts.zeitPython.empty()) opts.zeitPython = platform::getEnv("JANUS_ZEIT_PYTHON");
    if (opts.zeitBridge.empty()) opts.zeitBridge = platform::getEnv("JANUS_ZEIT_BRIDGE");

    StartupTimes st;
    configureBundledData();
    // GDAL's block cache: 512 MB instead of 5% of the RAM. Closing a file frees
    // its cached blocks under a global lock, one file at a time: with ~3 GB of
    // 1-row strips (a series read whole for its overview), closing Janus or a
    // layer took 15-20 s; with 512 MB it takes under a second, and detail tiles
    // read as fast (--profile-tour). GDAL_CACHEMAX in the environment wins.
    if (!CPLGetConfigOption("GDAL_CACHEMAX", nullptr)) CPLSetConfigOption("GDAL_CACHEMAX", "512");
    GDALAllRegister();
    configureRemoteAccess();
    CPLSetErrorHandler(CPLQuietErrorHandler);
    if (selftestUi) platform::attachParentConsole();
    if (describe) {
        platform::attachParentConsole();
        const int rc = describeInputs(inputs, opts.sel);
        std::fflush(stdout);
        return rc;
    }
    if (selftestEmb) {
        platform::attachParentConsole();
        std::string report;
        const char* err = embeddingSelfTest(report, opts.ioThreads > 0 ? opts.ioThreads : defaultProcessingThreads());
        std::fputs(report.c_str(), stdout);
        std::printf("%s\n", err ? (std::string("FAIL: ") + err).c_str() : "OK");
        std::fflush(stdout);
        return err ? 1 : 0;
    }
    if (measureVal) {
        platform::attachParentConsole();
        const int rc = inputs.empty() ? 2 : val::measure(inputs[0]);
        std::fflush(stdout);
        return rc;
    }
    if (selftestVal) {
        platform::attachParentConsole();
        // zeit.accuracy is run on a labelled project when the runtime is there.
        const std::string python = !opts.zeitPython.empty()
                                       ? opts.zeitPython
                                       : bundledPython((fs::u8path(platform::resourceDir()) / "runtime").u8string());
        std::string report;
        const char* err = val::selfTest(report, python);
        std::fputs(report.c_str(), stdout);
        std::printf("%s\n", err ? (std::string("FAIL: ") + err).c_str() : "OK");
        std::fflush(stdout);
        return err ? 1 : 0;
    }
    if (selftestZeit) {
        platform::attachParentConsole();
        // --threads N: Zeit's threads (else the default of the Settings value).
        const int rc = runZeitSelfTest(inputs, opts.sel, opts.zeitPython, opts.zeitBridge,
                                       opts.ioThreads > 0 ? opts.ioThreads : defaultProcessingThreads());
        std::fflush(stdout);
        return rc;
    }
    st.gdalMs = ms();

#ifdef __APPLE__
    // Inside an .app, GLFW would make Contents/Resources the current folder:
    // relative paths from the command line (jn folder/) would then break.
    glfwInitHint(GLFW_COCOA_CHDIR_RESOURCES, GLFW_FALSE);
#endif
    if (!glfwInit()) return 1;
    std::string error;
    GLFWwindow* window = render::createWindow(1600, 950, "Janus", !selftestUi, error);
    if (!window) {
        platform::attachParentConsole();
        std::fprintf(stderr, "Could not open the window (%s): %s\n", render::name(), error.c_str());
        return 1;
    }
    if (!shots.empty()) { // the same size on every machine, not the screen's (glfwRestoreWindow would show it)
        glfwSetWindowSize(window, App::kShotsWidth, App::kShotsHeight);
    }
    st.windowMs = ms();

    IMGUI_CHECKVERSION();
    ImGui::CreateContext();
    ImPlot::CreateContext();
    ImGuiIO& io = ImGui::GetIO();
    io.ConfigFlags |= ImGuiConfigFlags_DockingEnable;
    // Multi-viewports: panels can be dragged out of the main window into their
    // own OS windows (e.g. the map on a second monitor, charts on the first).
    io.ConfigFlags |= ImGuiConfigFlags_ViewportsEnable;
    theme::apply(theme::Dark); // the one in Settings once the layout file is read
    render::initImGui(window);

    auto app = std::make_unique<App>(window);
    g_app = app.get();
    if (!app->init(opts, error)) {
        platform::attachParentConsole();
        std::fprintf(stderr, "Could not initialize the GPU: %s\n", error.c_str());
        return 1;
    }
    // The screenshots in the default settings and layout: the user's are neither read nor written.
    if (!shots.empty()) io.IniFilename = nullptr;
    glfwSetDropCallback(window, dropCallback);
    st.uiMs = ms();
    if (!inputs.empty() && !selftestUi) app->openInputs(inputs);
    int exitCode = 0;
    st.openMs = ms();

    bool firstFrame = true;
    while (!glfwWindowShouldClose(window)) {
        // Without animation, sleep until an event (mouse, keyboard or a
        // finished background job): the CPU stays idle when nothing changes.
        {
            PROFILE_ZONE_N("events");
            if (app->wantsContinuousFrames() || firstFrame || selftestUi) glfwPollEvents();
            else glfwWaitEventsTimeout(0.5);
        }
        // Documents from the Finder (macOS): opened like a drop on the window.
        if (std::vector<std::string> docs = platform::takeOpenRequests(); !docs.empty()) app->pendingDrop = std::move(docs);

        app->applyFont();
        render::newFrame();
        {
            PROFILE_ZONE_N("ImGui::NewFrame");
            ImGui::NewFrame();
        }
        app->frame();
        if (measureCache) {
            if (const int rc = app->measureCacheStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        } else if (dumpGolden) {
            if (const int rc = app->dumpGoldenStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        } else if (profileTour) {
            if (const int rc = app->profileTourStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        } else if (measureVectors) {
            if (const int rc = app->measureVectorsStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        } else if (selftestEmbUi) {
            if (inputs.empty()) {
                std::fprintf(stderr, "--selftest-embeddings-ui needs a folder of embeddings\n");
                exitCode = 2;
                glfwSetWindowShouldClose(window, 1);
            } else if (const int rc = app->selfTestEmbeddingStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        } else if (!shots.empty()) {
            if (inputs.size() != 2) {
                std::fprintf(stderr, "--shots TOUR needs its input and the output folder\n");
                exitCode = 2;
                glfwSetWindowShouldClose(window, 1);
            } else if (const int rc = app->shotsStep(shots, inputs[0], inputs[1]); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        } else if (selftestUi) {
            if (inputs.size() < 2 || inputs.size() > 7) {
                std::fprintf(stderr, "--selftest-ui needs two to seven inputs\n");
                exitCode = 2;
                glfwSetWindowShouldClose(window, 1);
            } else if (const int rc = app->selfTestStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        }
        {
            PROFILE_ZONE_N("ImGui::Render");
            ImGui::Render();
        }

        float clear[4];
        theme::clearColor(clear);
        render::present(window, clear, app->takeCapture());
        PROFILE_FRAME();

        if (firstFrame) {
            firstFrame = false;
            st.firstFrameMs = ms();
            app->setStartupTimes(st);
            if (measureStartup) {
                platform::attachParentConsole();
                std::printf("startup (ms since process start): gdal %.0f | window+%s %.0f | ui %.0f | "
                            "open inputs %.0f | first frame %.0f\n",
                            st.gdalMs, render::name(), st.windowMs, st.uiMs, st.openMs, st.firstFrameMs);
                std::fflush(stdout);
                glfwSetWindowShouldClose(window, 1);
            }
        }
    }

    // io.IniFilename points into App: save the layout while it is alive, so
    // DestroyContext does not write to a freed file name.
    PROFILE_ZONE_N("shutdown");
    if (io.IniFilename) ImGui::SaveIniSettingsToDisk(io.IniFilename);
    io.IniFilename = nullptr;
    g_app = nullptr;
    app.reset(); // release GPU resources and threads before the context goes away
    {
        PROFILE_ZONE_N("ImGui and window");
        render::shutdownImGui();
        ImPlot::DestroyContext();
        ImGui::DestroyContext();
        glfwDestroyWindow(window);
        glfwTerminate();
    }
    return exitCode;
}
