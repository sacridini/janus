#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include "gl.hpp"

#include <cpl_conv.h>
#include <gdal_priv.h>
#include <imgui.h>
#include <imgui_impl_glfw.h>
#include <imgui_impl_opengl3.h>
#include <implot.h>
#include <ogr_srs_api.h>

#include "app.hpp"
#include "platform.hpp"
#include "selftest.hpp"
#include "usage.hpp"

namespace fs = std::filesystem;

// PROJ/GDAL data bundled next to the .exe (same version as the DLLs).
static void configureBundledData() {
    const fs::path share = fs::u8path(platform::exeDir()) / "share";
    std::error_code ec;
    if (fs::exists(share / "proj" / "proj.db", ec)) {
        const std::string proj = (share / "proj").u8string();
        const char* paths[] = {proj.c_str(), nullptr};
        OSRSetPROJSearchPaths(paths);
    }
    if (fs::exists(share / "gdal", ec)) CPLSetConfigOption("GDAL_DATA", (share / "gdal").u8string().c_str());
}

static App* g_app = nullptr;

static void dropCallback(GLFWwindow*, int count, const char** paths) {
    if (!g_app) return;
    g_app->pendingDrop.assign(paths, paths + count);
}

int main(int argc, char** argv) {
    using Clock = std::chrono::steady_clock;
    const auto t0 = Clock::now();
    auto ms = [&] { return std::chrono::duration<double, std::milli>(Clock::now() - t0).count(); };

    platform::init();
    const std::vector<std::string> args = platform::commandLineArgs(argc, argv);

    AppOptions opts;
    std::vector<std::string> inputs;
    bool measureStartup = false; // dev option: print startup timings and exit after the first frame
    bool selftestZeit = false;   // dev option: run the Zeit path without a window
    bool selftestUi = false;     // dev option: drive the layers workflow in a hidden window
    for (size_t i = 0; i < args.size(); ++i) {
        const std::string& a = args[i];
        auto next = [&]() -> const char* { return i + 1 < args.size() ? args[++i].c_str() : nullptr; };
        if (a == "-h" || a == "--help" || a == "/?") {
            platform::attachParentConsole();
            std::fputs(kUsage, stdout);
            return 0;
        } else if (a == "--version") {
            platform::attachParentConsole();
            std::printf("tsv %s (GDAL %s)\n", TSV_VERSION, GDALVersionInfo("RELEASE_NAME"));
            return 0;
        } else if (a == "--band") {
            const char* v = next();
            opts.sel.band = v ? std::max(1, std::atoi(v)) : 1;
        } else if (a == "--budget") {
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
        } else if (a == "--selftest-zeit") {
            selftestZeit = true;
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
    if (opts.zeitPython.empty()) opts.zeitPython = platform::getEnv("TSV_ZEIT_PYTHON");
    if (opts.zeitBridge.empty()) opts.zeitBridge = platform::getEnv("TSV_ZEIT_BRIDGE");

    StartupTimes st;
    configureBundledData();
    GDALAllRegister();
    CPLSetErrorHandler(CPLQuietErrorHandler);
    if (selftestUi) platform::attachParentConsole();
    if (selftestZeit) {
        platform::attachParentConsole();
        const int rc = runZeitSelfTest(inputs, opts.sel, opts.zeitPython, opts.zeitBridge);
        std::fflush(stdout);
        return rc;
    }
    st.gdalMs = ms();

    if (!glfwInit()) return 1;
    glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
    glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 3);
    glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);
#ifdef __APPLE__
    glfwWindowHint(GLFW_OPENGL_FORWARD_COMPAT, GLFW_TRUE); // required for a core profile on macOS
#endif
    glfwWindowHint(GLFW_MAXIMIZED, GLFW_TRUE);
    if (selftestUi) glfwWindowHint(GLFW_VISIBLE, GLFW_FALSE); // never shown, never takes focus
    GLFWwindow* window = glfwCreateWindow(1600, 950, "tsv", nullptr, nullptr);
    if (!window) return 1;
    glfwMakeContextCurrent(window);
    glfwSwapInterval(1);
    if (!gladLoadGL(glfwGetProcAddress)) return 1;
    st.windowMs = ms();

    IMGUI_CHECKVERSION();
    ImGui::CreateContext();
    ImPlot::CreateContext();
    ImGuiIO& io = ImGui::GetIO();
    io.ConfigFlags |= ImGuiConfigFlags_DockingEnable;
    // Multi-viewports: panels can be dragged out of the main window into their
    // own OS windows (e.g. the map on a second monitor, charts on the first).
    io.ConfigFlags |= ImGuiConfigFlags_ViewportsEnable;
    ImGui::StyleColorsDark();
    {
        // Platform windows look like regular OS windows: no rounding, opaque.
        ImGuiStyle& style = ImGui::GetStyle();
        style.WindowRounding = 0.0f;
        style.Colors[ImGuiCol_WindowBg].w = 1.0f;
    }
    ImGui_ImplGlfw_InitForOpenGL(window, true);
    ImGui_ImplOpenGL3_Init("#version 330");

    auto app = std::make_unique<App>(window);
    g_app = app.get();
    std::string error;
    if (!app->init(opts, error)) {
        platform::attachParentConsole();
        std::fprintf(stderr, "Could not initialize the GPU: %s\n", error.c_str());
        return 1;
    }
    glfwSetDropCallback(window, dropCallback);
    st.uiMs = ms();
    if (!inputs.empty() && !selftestUi) app->openInputs(inputs);
    int exitCode = 0;
    st.openMs = ms();

    bool firstFrame = true;
    while (!glfwWindowShouldClose(window)) {
        // Without animation, sleep until an event (mouse, keyboard or a
        // finished background job): the CPU stays idle when nothing changes.
        if (app->wantsContinuousFrames() || firstFrame || selftestUi) glfwPollEvents();
        else glfwWaitEventsTimeout(0.5);

        ImGui_ImplOpenGL3_NewFrame();
        ImGui_ImplGlfw_NewFrame();
        ImGui::NewFrame();
        app->frame();
        if (selftestUi) {
            if (inputs.size() < 2 || inputs.size() > 5) {
                std::fprintf(stderr, "--selftest-ui needs two to five inputs\n");
                exitCode = 2;
                glfwSetWindowShouldClose(window, 1);
            } else if (const int rc = app->selfTestStep(inputs); rc >= 0) {
                std::fflush(stdout);
                exitCode = rc;
                glfwSetWindowShouldClose(window, 1);
            }
        }
        ImGui::Render();

        int fbw, fbh;
        glfwGetFramebufferSize(window, &fbw, &fbh);
        glViewport(0, 0, fbw, fbh);
        glClearColor(0.08f, 0.08f, 0.09f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT);
        ImGui_ImplOpenGL3_RenderDrawData(ImGui::GetDrawData());
        // Panels detached to other windows/monitors (they share this GL context's
        // textures, so the map framebuffer can be shown there too).
        if (io.ConfigFlags & ImGuiConfigFlags_ViewportsEnable) {
            GLFWwindow* current = glfwGetCurrentContext();
            ImGui::UpdatePlatformWindows();
            ImGui::RenderPlatformWindowsDefault();
            glfwMakeContextCurrent(current);
        }
        glfwSwapBuffers(window);

        if (firstFrame) {
            firstFrame = false;
            st.firstFrameMs = ms();
            app->setStartupTimes(st);
            if (measureStartup) {
                platform::attachParentConsole();
                std::printf("startup (ms since process start): gdal %.0f | window+GL %.0f | ui %.0f | "
                            "open inputs %.0f | first frame %.0f\n",
                            st.gdalMs, st.windowMs, st.uiMs, st.openMs, st.firstFrameMs);
                std::fflush(stdout);
                glfwSetWindowShouldClose(window, 1);
            }
        }
    }

    g_app = nullptr;
    app.reset(); // release GL resources and threads before the context goes away
    ImGui_ImplOpenGL3_Shutdown();
    ImGui_ImplGlfw_Shutdown();
    ImPlot::DestroyContext();
    ImGui::DestroyContext();
    glfwDestroyWindow(window);
    glfwTerminate();
    return exitCode;
}
