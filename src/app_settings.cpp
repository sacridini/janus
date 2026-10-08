// Settings window (File > Settings..., Ctrl+,): the interface theme and font
// size, the Files panel, the processing threads (Janus' pools and exports,
// Zeit's processes), the overview memory and the caches. Every value is kept in
// the layout .ini ([Janus][Settings], with the last export folder); --budget
// and --threads override theirs for one run.
#include "app.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <memory>
#include <thread>
#include <vector>

#include <imgui_internal.h>
#include <implot.h>

namespace fs = std::filesystem;

void App::registerSettings() {
    ImGuiSettingsHandler h;
    h.TypeName = "Janus";
    h.TypeHash = ImHashStr("Janus");
    h.UserData = this;
    h.ReadOpenFn = [](ImGuiContext*, ImGuiSettingsHandler*, const char* name) -> void* {
        return std::strcmp(name, "Settings") == 0 ? reinterpret_cast<void*>(1) : nullptr;
    };
    h.ReadLineFn = [](ImGuiContext*, ImGuiSettingsHandler* handler, void*, const char* line) {
        App& app = *static_cast<App*>(handler->UserData);
        SessionSettings& s = app.settings_;
        auto value = [line](const char* key, int& v) {
            const size_t n = std::strlen(key);
            if (std::strncmp(line, key, n) != 0) return false;
            v = std::atoi(line + n);
            return true;
        };
        int v = 0;
        if (value("FullResCache=", v)) s.fullResMode = std::clamp(v, 0, 2);
        else if (value("FullResBudgetGB=", v)) s.fullResBudgetBytes = uint64_t(std::clamp(v, 1, 1 << 20)) << 30;
        else if (value("OverviewBudgetMB=", v)) app.overviewBudgetMB_ = std::clamp(v, 64, 1 << 20);
        else if (value("Threads=", v)) s.threads = std::max(0, v); // 0: the default
        else if (value("FontSize=", v)) app.fontSize_ = std::clamp(v, kMinFontSize, kMaxFontSize);
        else if (value("RevealOpened=", v)) app.revealOpened_ = v != 0;
        else if (std::strncmp(line, "ExportDir=", 10) == 0) app.exportDir_ = line + 10;
        else if (std::strncmp(line, "Favorite=", 9) == 0 && line[9]) app.files_.addFavorite(line + 9);
        else if (std::strncmp(line, "Theme=", 6) == 0)
            for (int i = 0; i < theme::Count; ++i)
                if (std::strcmp(line + 6, theme::name(i)) == 0) app.theme_ = i;
    };
    // Once every value is known: the oldest full-resolution caches go (as the
    // overview's at startup), the budget and the threads apply; the theme at
    // the start of the frame.
    h.ApplyAllFn = [](ImGuiContext*, ImGuiSettingsHandler* handler) {
        App& app = *static_cast<App*>(handler->UserData);
        FullResCache::prune(app.settings_.cacheDir, app.settings_.fullResBudgetBytes);
        app.applyOverviewBudget();
        app.applyThreads();
    };
    h.WriteAllFn = [](ImGuiContext*, ImGuiSettingsHandler* handler, ImGuiTextBuffer* buf) {
        const App& app = *static_cast<App*>(handler->UserData);
        const SessionSettings& s = app.settings_;
        buf->appendf("[Janus][Settings]\nFullResCache=%d\nFullResBudgetGB=%d\nOverviewBudgetMB=%d\nThreads=%d\n"
                     "Theme=%s\nFontSize=%d\nRevealOpened=%d\nExportDir=%s\n",
                     s.fullResMode, int(s.fullResBudgetBytes >> 30), app.overviewBudgetMB_, s.threads,
                     theme::name(app.theme_), app.fontSize_, int(app.revealOpened_), app.exportDir_.c_str());
        for (const std::string& f : app.files_.favorites()) buf->appendf("Favorite=%s\n", f.c_str()); // one line each
        buf->append("\n");
    };
    ImGui::AddSettingsHandler(&h);
}

void App::applyTheme() {
    theme::apply(theme_);
    appliedTheme_ = theme_;
    // The cursor's series: white on the dark themes, near black on the light one.
    hover_.color = theme::cursorSeries();
    for (SeriesLayer& L : layers_) L.hover.color = theme::cursorSeries();
}

// Both of ImGui's fonts are loaded once: the pixel one keeps the usual look at
// 13 px, the scalable one stays sharp at the other sizes. Set between frames.
void App::applyFont() {
    ImGuiIO& io = ImGui::GetIO();
    if (!fontBitmap_) {
        fontBitmap_ = io.Fonts->AddFontDefaultBitmap();
        fontVector_ = io.Fonts->AddFontDefaultVector();
    }
    if (fontSize_ == appliedFontSize_) return;
    io.FontDefault = fontSize_ == kDefaultFontSize ? fontBitmap_ : fontVector_;
    ImGui::GetStyle().FontSizeBase = float(fontSize_);
    appliedFontSize_ = fontSize_;
}

void App::applyThreads() {
    threadsChanged_ = std::chrono::steady_clock::now();
    for (SeriesLayer& L : layers_) L.session->setThreads(settings_);
    if (zeit_) zeit_->setJobThreads(settings_.processingThreads()); // the serve process: pumpZeit
}

void App::applyOverviewBudget() {
    budgetUi_ = overviewBudgetMB();
    settings_.overviewBudgetBytes = std::min<int64_t>(int64_t(budgetUi_) << 20, gpu_.maxCubeBytes());
}

void App::clearOverviewCache() {
    std::error_code ec;
    const std::string keep = s_ ? fs::u8path(s_->overview.cachePath()).filename().u8string() : "";
    for (auto& e : fs::directory_iterator(fs::u8path(settings_.cacheDir), ec))
        if (e.path().extension() == ".januscube" && e.path().filename().u8string() != keep) fs::remove(e.path(), ec);
}

void App::uiSettings() {
    if (!showSettings_) return;
    if (focusSettings_) ImGui::SetNextWindowFocus();
    focusSettings_ = false;
    const ImGuiViewport* vp = ImGui::GetMainViewport();
    ImGui::SetNextWindowPos(vp->GetCenter(), ImGuiCond_FirstUseEver, ImVec2(0.5f, 0.5f));
    ImGui::SetNextWindowSize(ImVec2(480, 560), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Settings", &showSettings_, ImGuiWindowFlags_NoDocking | ImGuiWindowFlags_NoCollapse)) {
        ImGui::End();
        return;
    }

    ImGui::SeparatorText("Interface");
    const char* names[theme::Count];
    for (int i = 0; i < theme::Count; ++i) names[i] = theme::name(i);
    ImGui::SetNextItemWidth(-1);
    if (ImGui::Combo("##theme", &theme_, names, theme::Count)) ImGui::MarkIniSettingsDirty(); // applied next frame
    ImGui::SetItemTooltip("Dark: ImGui's dark colours (the default). Light: dark text on light panels.\n"
                          "Classic: ImGui's original colours. Janus: the program's own colours\n"
                          "(blue on navy panels, orange accents).");
    ImGui::TextDisabled("The map, its colour bars and exported figures keep their colours.");
    ImGui::SetNextItemWidth(-1);
    if (ImGui::SliderInt("##font", &fontSize_, kMinFontSize, kMaxFontSize, "Font size: %d px",
                         ImGuiSliderFlags_AlwaysClamp))
        ImGui::MarkIniSettingsDirty(); // applied next frame
    ImGui::SetItemTooltip("Size of the interface text (and of the labels on the map). 13 px: ImGui's\n"
                          "pixel font; other sizes use its scalable font. Exported figures keep\n"
                          "their text as at 13 px.");
    if (fontSize_ != kDefaultFontSize) {
        char label[64];
        std::snprintf(label, sizeof(label), "Default font size (%d px)", kDefaultFontSize);
        if (ImGui::Button(label, ImVec2(-1, 0))) {
            fontSize_ = kDefaultFontSize;
            ImGui::MarkIniSettingsDirty();
        }
    }
    if (ImGui::Checkbox("Files panel follows what is opened", &revealOpened_)) ImGui::MarkIniSettingsDirty();
    ImGui::SetItemTooltip("Opening a series, a file or a folder expands the Files panel down to it\n"
                          "and scrolls there.");

    ImGui::SeparatorText("Processing");
    const int cores = logicalCores();
    int n = settings_.processingThreads();
    char fmt[64];
    std::snprintf(fmt, sizeof(fmt), "Processing threads: %%d of %d", cores);
    ImGui::SetNextItemWidth(-1);
    if (ImGui::SliderInt("##threads", &n, 1, cores, fmt, ImGuiSliderFlags_AlwaysClamp)) {
        settings_.threads = n;
        applyThreads();
        ImGui::MarkIniSettingsDirty();
    }
    ImGui::SetItemTooltip("CPU threads the processing may use: Janus' readers (overview, exact series,\n"
                          "ROI, detail tiles, transect), the full-resolution cache, exports, and the\n"
                          "Zeit tools (raster jobs and the pixel fits). Fewer leaves cores free for\n"
                          "other programs. Default: every logical core but 2.");
    if (settings_.threads > 0 && settings_.threads != defaultProcessingThreads()) {
        char label[64];
        std::snprintf(label, sizeof(label), "Default (%d: every core but 2)", defaultProcessingThreads());
        if (ImGui::Button(label, ImVec2(-1, 0))) {
            settings_.threads = 0;
            applyThreads();
            ImGui::MarkIniSettingsDirty();
        }
    }
    if (s_) {
        ImGui::TextDisabled("Active layer: %d background readers + %d interactive%s", s_->bgPool().threads(),
                            s_->fgPool().threads(), s_->rotational ? " (HDD: one reader)" : "");
        if (s_->rotational)
            ImGui::SetItemTooltip("A spinning disk is read by one thread whatever this setting:\n"
                                  "two readers were measured 4x slower (the head jumps between files).");
    }
    if (opts_.ioThreads > 0) ImGui::TextDisabled("--threads %d: background readers for this run", opts_.ioThreads);
    if (zeitNext_ && zeitNext_->state() != ZeitClient::State::Failed)
        ImGui::TextDisabled("Zeit: restarting its process with %d threads...", zeitNext_->config().threads);
    else if (zeit_ && zeit_->state() == ZeitClient::State::Ready)
        ImGui::TextDisabled("Zeit: %d threads (raster jobs already running keep theirs)", zeit_->jobThreads());

    ImGui::SeparatorText("Memory and caches");
    ImGui::SetNextItemWidth(-1);
    ImGui::SliderInt("##budget", &budgetUi_, 128, 8192, "Overview memory: %d MB", ImGuiSliderFlags_Logarithmic);
    ImGui::SetItemTooltip("Memory for the cube overview (on the GPU; on macOS shared\nwith the CPU). Larger = more "
                          "resolution without tiles,\nbut slower to build.");
    if (budgetUi_ != overviewBudgetMB()) {
        if (ImGui::Button(s_ ? "Apply (reopens the active layer)" : "Apply", ImVec2(-1, 0))) {
            overviewBudgetMB_ = budgetUi_;
            opts_.budgetMB = 0; // the --budget of this run gives way to the setting
            applyOverviewBudget();
            ImGui::MarkIniSettingsDirty();
            if (s_ && !opening_.valid()) {
                const std::vector<std::string> inputs = lastInputs_;
                removeLayer(active_);
                openInputs(inputs, true);
            }
        }
    } else if (opts_.budgetMB > 0) {
        ImGui::TextDisabled("--budget %d MB for this run", int(opts_.budgetMB));
    }
    if (ImGui::Button("Clear overview cache", ImVec2(-1, 0))) clearOverviewCache();
    ImGui::SetItemTooltip("%s (the active layer's is kept)", settings_.cacheDir.c_str());
    uiFullResSettings();
    ImGui::End();
}

// --selftest-ui: the processing threads reach every open series' pools (and
// their jobs still run after the pools shrink and grow), the exports and the
// Zeit processes; every theme keeps the text and the chart colours Janus draws
// legible (WCAG contrast: 4.5:1 for text, 3:1 for chart series), and the dark
// theme draws the data colours as they always were. Restores both settings.
const char* App::selfTestSettings() {
    const int savedThreads = settings_.threads, savedTheme = theme_;
    auto restore = [&] {
        settings_.threads = savedThreads;
        applyThreads();
        theme_ = savedTheme;
        applyTheme();
    };
    auto failed = [&](const char* what) {
        restore();
        return what;
    };

    // Threads: 3, then 1 (jobs queued with parked workers), then 6.
    for (const int want : {3, 1, 6}) {
        settings_.threads = want;
        applyThreads();
        const int n = settings_.processingThreads();
        for (SeriesLayer& L : layers_) {
            Session& S = *L.session;
            const int bg = opts_.ioThreads > 0 ? opts_.ioThreads : S.rotational ? 1 : std::min(n, 12);
            const int fg = S.rotational ? 1 : std::min(n, 4);
            if (S.bgPool().threads() != bg || S.fgPool().threads() != fg)
                return failed("the processing threads did not reach a layer's pools");
        }
        if (zeitConfig().threads != n || (zeit_ && zeit_->jobThreads() != n))
            return failed("the processing threads did not reach Zeit's configuration");
        // Every pool still runs what it is given.
        auto done = std::make_shared<std::atomic<int>>(0);
        for (SeriesLayer& L : layers_)
            for (JobPool* p : {&L.session->bgPool(), &L.session->fgPool()})
                for (int k = 0; k < 8; ++k) p->submit(-100, [done] { ++*done; });
        const int total = int(layers_.size()) * 16;
        const auto t0 = std::chrono::steady_clock::now();
        while (*done < total && std::chrono::steady_clock::now() - t0 < std::chrono::seconds(20))
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        if (*done < total) return failed("jobs did not run after the pools changed size");
        std::printf("    processing threads %d of %d: active layer %d readers + %d interactive, Zeit %d; %d jobs ran\n",
                    n, logicalCores(), s_->bgPool().threads(), s_->fgPool().threads(), zeitConfig().threads, total);
    }

    // Themes.
    const ImVec4 roi(1.0f, 0.82f, 0.24f, 1), orange(1.0f, 0.6f, 0.2f, 1), histogram(0.45f, 0.62f, 0.90f, 1),
        model(0.95f, 0.15f, 0.95f, 1);
    for (int id = 0; id < theme::Count; ++id) {
        theme_ = id;
        applyTheme();
        const ImVec4 win = theme::background(), text = ImGui::GetStyleColorVec4(ImGuiCol_Text);
        float minText = 99;
        for (const ImVec4& c : {text, ImGui::GetStyleColorVec4(ImGuiCol_TextDisabled), theme::accent(),
                                theme::warning(), theme::error()})
            minText = std::min(minText, theme::contrast(c, win));
        for (const ImGuiCol bg : {ImGuiCol_PopupBg, ImGuiCol_FrameBg, ImGuiCol_MenuBarBg, ImGuiCol_TableHeaderBg})
            minText = std::min(minText, theme::contrast(text, theme::background(bg)));
        std::vector<ImVec4> data = {roi, orange, histogram, model, theme::cursorSeries()};
        for (int i = 1; i <= kPinColorCount; ++i) data.push_back(pinColor(i));
        float minPlot = 99, minHeader = 99;
        int changed = 0;
        for (const ImVec4& c : data) {
            const ImVec4 p = theme::onPlot(c);
            minPlot = std::min(minPlot, theme::contrast(p, theme::plotBackground()));
            minHeader = std::min(minHeader, theme::contrast(theme::onWindow(c, ImGuiCol_TableHeaderBg),
                                                            theme::background(ImGuiCol_TableHeaderBg)));
            changed += p.x != c.x || p.y != c.y || p.z != c.z;
        }
        std::printf("    theme %-7s text >= %.1f:1, chart series >= %.1f:1 (%d of %d adjusted), table headers >= "
                    "%.1f:1\n",
                    theme::name(id), minText, minPlot, changed, int(data.size()), minHeader);
        if (minText < 4.5f) return failed("a theme has text below 4.5:1");
        if (minPlot < 3.0f) return failed("a theme has chart series below 3:1");
        if (minHeader < 4.5f) return failed("a theme has table headers below 4.5:1");
        if (id == theme::Dark && changed) return failed("the dark theme must draw the data colours unchanged");
    }
    restore();
    return nullptr;
}

// --selftest-ui stages 60-62: with Zeit running, another processing threads
// value reaches raster jobs at once and replaces the idle serve process with
// one started with it (51); restoring the value does the same (52). Skipped,
// with a note, when the Zeit runtime is not there.
int App::selfTestZeitThreads() {
    auto secs = [] {
        return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
    };
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    const ZeitClient::State state = zeit_ ? zeit_->state() : ZeitClient::State::Off;
    switch (st_.stage) {
    case 60:
        if (state == ZeitClient::State::Starting) break; // started with the first series: wait for it
        if (state != ZeitClient::State::Ready) {
            std::printf("    Zeit not available (%s): its threads not checked\n",
                        zeit_ ? zeit_->error().c_str() : "not started");
            st_.stage = 8;
            break;
        }
        st_.threads = settings_.threads;
        settings_.threads = settings_.processingThreads() == 2 ? 3 : 2;
        applyThreads();
        if (zeit_->jobThreads() != settings_.threads) return fail("Zeit jobs should get the new threads at once");
        st_.zeitT0 = st_.since = secs();
        ++st_.stage;
        break;
    case 61:
    case 62: {
        const int want = settings_.processingThreads();
        if (state != ZeitClient::State::Ready || zeit_->config().threads != want || zeitNext_) break;
        std::printf("    Zeit serve process replaced by one with %d threads in %.1f s (raster jobs: %d)\n", want,
                    secs() - st_.zeitT0, zeit_->jobThreads());
        if (st_.stage == 62) {
            std::printf("[%5.1f s] Zeit follows the processing threads\n", secs() - st_.t0);
            st_.stage = 8;
            break;
        }
        settings_.threads = st_.threads;
        applyThreads();
        st_.zeitT0 = st_.since = secs();
        ++st_.stage;
        break;
    }
    }
    return -1;
}
