// Full-resolution cache (FullResCache) in the interface: its section of the
// Performance panel (state, size, read timings, build on demand), the setting
// and budget (in the Settings window, kept in the layout .ini by
// app_settings.cpp), and `--measure-cache`, which times the build and the
// reads with and without it.
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <random>

#include <imgui_internal.h>

namespace {

double nowMs() {
    return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

} // namespace

void App::uiFullRes() {
    FullResCache& c = *s_->fullRes;
    const int T = s_->info->T();
    ImGui::SeparatorText("Full-resolution cache");
    const std::string err = c.error();
    if (!c.started()) {
        if (!err.empty()) {
            ImGui::PushTextWrapPos(0);
            ImGui::TextColored(theme::error(), "Not built: %s", err.c_str());
            ImGui::PopTextWrapPos();
        } else {
            ImGui::TextDisabled(s_->rotational ? "Off: exact series are read from the HDD"
                                               : "Off (the series is on an SSD)");
        }
        if (ImGui::Button("Build it now", ImVec2(-1, 0))) s_->buildFullRes();
        ImGui::SetItemTooltip("Copies the series at full resolution to the cache folder (about %.1f GB),\n"
                              "in the background: exact series, ROI and detail tiles are then read\n"
                              "from there in about a millisecond, even with the files on an HDD.",
                              c.estimatedBytes() / 1e9);
        return;
    }
    if (c.complete()) {
        ImGui::Text("Complete: %d dates, %.2f GB (%s)", T, c.bytes() / 1e9, c.codec());
        if (c.buildSeconds() > 0)
            ImGui::Text("Built in %.0f s (%d dates read with the overview)", c.buildSeconds(), c.datesWithOverview());
    } else {
        char label[96];
        std::snprintf(label, sizeof(label), "%d/%d dates, %.2f of ~%.1f GB", c.datesDone(), T, c.bytes() / 1e9,
                      c.estimatedBytes() / 1e9);
        ImGui::ProgressBar(float(c.datesDone()) / std::max(1, T), ImVec2(-1, 0), label);
        if (!err.empty()) {
            ImGui::PushTextWrapPos(0);
            ImGui::TextColored(theme::error(), "%s", err.c_str());
            ImGui::PopTextWrapPos();
        } else if (c.building()) {
            ImGui::TextDisabled("Building in the background; cached dates are used already");
        } else if (!c.readOnly() && ImGui::Button("Resume", ImVec2(-1, 0))) {
            s_->buildFullRes();
        }
    }
    ImGui::SetItemTooltip("%s", c.path().c_str());
    if (c.seriesHits()) ImGui::Text("Series from the cache: %.2f ms (%d reads)", c.lastSeriesMs(), c.seriesHits());
    if (c.windowHits())
        ImGui::Text("ROI / tile blocks: %.2f ms per date (%d reads)", c.avgWindowMs(), c.windowHits());
}

void App::uiFullResSettings() {
    const char* modes[] = {"Full-resolution cache: on demand", "Full-resolution cache: series on an HDD",
                           "Full-resolution cache: every series"};
    ImGui::SetNextItemWidth(-1);
    if (ImGui::Combo("##fullres", &settings_.fullResMode, modes, IM_ARRAYSIZE(modes))) {
        ImGui::MarkIniSettingsDirty();
        // Applies to the open series too: stop or start their builds.
        for (SeriesLayer& L : layers_) {
            Session& S = *L.session;
            const bool want = settings_.fullResMode == SessionSettings::FullResAll ||
                              (settings_.fullResMode == SessionSettings::FullResHdd && S.rotational);
            if (want && !S.fullRes->complete()) S.buildFullRes();
            else if (!want) S.fullRes->stop();
        }
    }
    ImGui::SetItemTooltip("A copy of each series at full resolution in the cache folder (~1.4x smaller\n"
                          "than raw floats, lossless), built in the background: exact series, ROI\n"
                          "and detail tiles read from it in about a millisecond. On an HDD the\n"
                          "first build shares the overview's read of the files.");
    int gb = int(settings_.fullResBudgetBytes >> 30);
    ImGui::SetNextItemWidth(-1);
    if (ImGui::SliderInt("##fullresBudget", &gb, 8, 2048, "Full-resolution cache budget: %d GB",
                         ImGuiSliderFlags_Logarithmic)) {
        settings_.fullResBudgetBytes = uint64_t(std::max(1, gb)) << 30;
        ImGui::MarkIniSettingsDirty();
    }
    ImGui::SetItemTooltip("The least recently opened series leave the cache when a new one needs room.");
    if (ImGui::Button("Clear full-resolution cache", ImVec2(-1, 0))) FullResCache::clear(settings_.cacheDir);
    if (ImGui::IsItemHovered(ImGuiHoveredFlags_ForTooltip)) // lists the folder: only when asked
        ImGui::SetTooltip("%.1f GB in %s (series open now are kept)", FullResCache::folderBytes(settings_.cacheDir) / 1e9,
                          settings_.cacheDir.c_str());
}

// `jn --measure-cache on|off IN`: opens IN in a hidden window and prints the
// times of the exact series and of a 256x256 ROI read from the source (cold:
// before anything else reads the files), of the overview and full-resolution
// builds (with the cache on or off), then of the same reads from the cache.
// Developer tool for the numbers in README/IDEIAS; IN is only read.
int App::measureCacheStep(const std::vector<std::string>& in) {
    auto& m = measure_;
    const bool on = in.size() >= 2 && in[0] == "on";
    const std::vector<std::string> inputs(in.begin() + 1, in.end());
    auto seriesFromSource = [&](CubeReader& r, int x, int y, std::vector<float>& v) {
        v.assign(m.info->T(), NAN);
        for (int t = 0; t < m.info->T(); ++t) r.readPixel(t, x, y, v[t]);
    };
    switch (m.stage) {
    case 0: {
        ImGui::GetIO().IniFilename = nullptr; // own settings, nothing saved
        if (in.size() < 2 || (in[0] != "on" && in[0] != "off")) {
            std::printf("usage: --measure-cache on|off INPUT...\n");
            return 2;
        }
        std::string error;
        m.info = openCube(inputs, opts_.sel, error);
        if (!m.info) {
            std::printf("%s\n", error.c_str());
            return 1;
        }
        const int W = m.info->width, H = m.info->height, T = m.info->T();
        std::printf("%s: %d x %d x %d dates (%s), cache %s\n", inputs[0].c_str(), W, H, T, m.info->dataType.c_str(),
                    on ? "on" : "off");
        std::mt19937 rng(7);
        for (int i = 0; i < 5; ++i)
            m.px.push_back({int(rng() % unsigned(W)), int(rng() % unsigned(H))});
        m.roi = {W / 2 - 128, H / 2 - 128, 256, 256};
        CubeReader r(m.info);
        for (auto [x, y] : m.px) {
            std::vector<float> v;
            const double t0 = nowMs();
            seriesFromSource(r, x, y, v);
            std::printf("  source series (%d, %d): %.0f ms (cold)\n", x, y, nowMs() - t0);
        }
        std::vector<float> buf(size_t(256) * 256);
        const double t0 = nowMs();
        for (int t = 0; t < T; ++t) r.readWindow(t, m.roi[0], m.roi[1], 256, 256, buf.data(), 256, 256);
        std::printf("  source ROI 256x256 x %d dates: %.0f ms (cold)\n", T, nowMs() - t0);
        settings_.fullResMode = on ? SessionSettings::FullResAll : SessionSettings::FullResOff;
        m.t0 = nowMs();
        openInputs(inputs);
        ++m.stage;
        break;
    }
    case 1:
        if (!s_ || !s_->overview.complete()) break;
        std::printf("  overview: %.1f s (%s), %d x %d\n", s_->overview.buildSeconds(),
                    s_->overview.fromCache() ? "from its cache" : "built", s_->overview.w, s_->overview.h);
        ++m.stage;
        break;
    case 2: {
        FullResCache& c = *s_->fullRes;
        if (on && !c.complete() && (c.building() || s_->bgPool().pending() > 0)) break;
        if (!on || !c.complete()) {
            if (on) std::printf("  full-resolution cache not built: %s\n", c.error().c_str());
            return on ? 1 : 0;
        }
        const double raw = double(m.info->width) * m.info->height * m.info->T() * 4;
        std::printf("  full-resolution cache: %.1f s since open (%.1f s building, %d dates with the overview), "
                    "%.2f GB (raw %.2f GB, %.2fx)\n",
                    (nowMs() - m.t0) / 1000, c.buildSeconds(), c.datesWithOverview(), c.bytes() / 1e9, raw / 1e9,
                    raw / double(c.bytes()));
        CubeReader r(m.info);
        const int T = m.info->T();
        std::mt19937 rng(11);
        for (int i = 0; i < 5; ++i)
            m.px.push_back({int(rng() % unsigned(m.info->width)), int(rng() % unsigned(m.info->height))});
        int bad = 0;
        for (auto [x, y] : m.px) {
            std::vector<float> v(T), src;
            std::vector<char> got(T, 0);
            const double t0 = nowMs();
            const int n = c.readSeries(x, y, v.data(), got.data());
            const double ms = nowMs() - t0;
            const double t1 = nowMs();
            seriesFromSource(r, x, y, src);
            const double srcMs = nowMs() - t1;
            bad += n != T || std::memcmp(v.data(), src.data(), size_t(T) * 4) != 0;
            std::printf("  series (%d, %d): cache %.2f ms, source %.0f ms (files read just now)\n", x, y, ms, srcMs);
        }
        std::vector<float> a(size_t(256) * 256), b(a.size());
        double cacheMs = 0, srcMs = 0;
        for (int t = 0; t < T; ++t) {
            double t0 = nowMs();
            c.readWindow(t, m.roi[0], m.roi[1], 256, 256, a.data(), 256, 256);
            cacheMs += nowMs() - t0;
            t0 = nowMs();
            r.readWindow(t, m.roi[0], m.roi[1], 256, 256, b.data(), 256, 256);
            srcMs += nowMs() - t0;
            bad += std::memcmp(a.data(), b.data(), a.size() * 4) != 0;
        }
        std::printf("  ROI 256x256 x %d dates: cache %.1f ms, source %.0f ms (1 thread each)\n", T, cacheMs, srcMs);
        std::printf(bad ? "FAIL: %d reads differ from the source\n" : "OK: cache reads equal the source\n", bad);
        return bad ? 1 : 0;
    }
    }
    return -1;
}
