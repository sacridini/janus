// Opening beyond local files: File > Open URL (cloud-optimized GeoTIFFs and
// other rasters over http, S3, GCS, Azure, read by GDAL's network file
// systems: only the header, the overview level the map needs and the blocks
// looked at are downloaded), the variable picker of NetCDF / Zarr / HDF files
// of several variables, and the Display panel's variable and level of such a
// layer (each change reopens the layer in place, like a band selection).
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>

#include <imgui_internal.h>

#include "glfw.hpp"
#include "profiler.hpp"

void App::openUrlDialog(bool add) {
    urlAdd_ = add && s_ != nullptr;
    openUrlPopup_ = true;
}

void App::uiOpenUrl() {
    PROFILE_ZONE();
    if (openUrlPopup_) {
        ImGui::OpenPopup("Open URL");
        openUrlPopup_ = false;
    }
    ImGui::SetNextWindowSize(ImVec2(ImGui::GetFontSize() * 46, 0), ImGuiCond_Appearing);
    if (!ImGui::BeginPopupModal("Open URL", nullptr)) return;
    ImGui::PushTextWrapPos(0);
    ImGui::TextUnformatted("Rasters on the web, opened in place: of a cloud-optimized GeoTIFF only the header, "
                           "the overview level the map needs and the blocks you look at are downloaded.");
    ImGui::PopTextWrapPos();
    ImGui::TextDisabled("One URL per line; several make a series (dates from the file names).");
    if (ImGui::IsWindowAppearing()) ImGui::SetKeyboardFocusHere();
    ImGui::InputTextMultiline("##urls", urlText_, sizeof(urlText_), ImVec2(-1, ImGui::GetTextLineHeight() * 7));
    ImGui::TextDisabled("https://...   s3://bucket/key.tif   gs://bucket/key.tif   az://container/key.tif");
    ImGui::SetItemTooltip("Public S3 and GCS buckets are read without an account. Private ones use the\n"
                          "usual credentials (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY or ~/.aws,\n"
                          "GOOGLE_APPLICATION_CREDENTIALS, AZURE_STORAGE_CONNECTION_STRING...),\n"
                          "or a signed URL (e.g. from Planetary Computer). NetCDF and Zarr open too.");
    if (ImGui::SmallButton("Example: a Sentinel-2 band on AWS")) {
        const char* ex = "https://sentinel-cogs.s3.us-west-2.amazonaws.com/sentinel-s2-l2a-cogs/36/Q/WD/2020/7/"
                         "S2A_36QWD_20200701_0_L2A/B04.tif";
        std::snprintf(urlText_, sizeof(urlText_), "%s", ex);
    }
    ImGui::SetItemTooltip("Band 4 (red) of a Sentinel-2 scene of Element 84's Earth Search COGs:\n"
                          "10980 x 10980 pixels, ~200 MB, opened in a second or two");

    std::vector<std::string> urls;
    for (const char* p = urlText_; *p;) {
        const char* e = std::strpbrk(p, "\r\n");
        std::string line(p, e ? size_t(e - p) : std::strlen(p));
        const size_t a = line.find_first_not_of(" \t\"'"), b = line.find_last_not_of(" \t\"'");
        line = a == std::string::npos ? "" : line.substr(a, b - a + 1);
        if (!line.empty() && line[0] != '#') urls.push_back(line);
        if (!e) break;
        p = e + 1;
    }
    ImGui::BeginDisabled(!s_);
    ImGui::Checkbox("Add as a layer", &urlAdd_);
    ImGui::EndDisabled();
    ImGui::BeginDisabled(urls.empty() || opening_.valid());
    const bool enter = ImGui::IsKeyChordPressed(ImGuiMod_Ctrl | ImGuiKey_Enter);
    if (ImGui::Button("Open", ImVec2(ImGui::GetFontSize() * 6, 0)) || (enter && !urls.empty() && !opening_.valid())) {
        openInputs(urls, urlAdd_ && s_);
        ImGui::CloseCurrentPopup();
    }
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Ctrl+Enter");
    ImGui::SameLine();
    if (ImGui::Button("Cancel", ImVec2(ImGui::GetFontSize() * 6, 0)) || ImGui::IsKeyPressed(ImGuiKey_Escape))
        ImGui::CloseCurrentPopup();
    ImGui::EndPopup();
}

void App::uiVariablePicker() {
    PROFILE_ZONE();
    if (variablePopup_) {
        ImGui::OpenPopup("Choose a variable");
        variablePopup_ = false;
    }
    ImGui::SetNextWindowSize(ImVec2(ImGui::GetFontSize() * 40, 0), ImGuiCond_Appearing);
    if (!ImGui::BeginPopupModal("Choose a variable", nullptr)) return;
    ImGui::Text("%s holds %d variables:", displayName(varFile_).c_str(), int(varChoices_.size()));
    bool open = false;
    const float rows = float(std::min<size_t>(varChoices_.size(), 12));
    if (ImGui::BeginListBox("##vars", ImVec2(-1, ImGui::GetTextLineHeightWithSpacing() * rows + 8))) {
        for (int i = 0; i < int(varChoices_.size()); ++i) {
            const MdVariable& v = varChoices_[size_t(i)];
            ImGui::PushID(i);
            if (ImGui::Selectable(v.name.c_str(), varPick_ == i, ImGuiSelectableFlags_AllowDoubleClick)) {
                varPick_ = i;
                open |= ImGui::IsMouseDoubleClicked(ImGuiMouseButton_Left);
            }
            ImGui::SameLine(ImGui::GetFontSize() * 9);
            ImGui::TextDisabled("%s", v.desc.c_str());
            ImGui::PopID();
        }
        ImGui::EndListBox();
    }
    ImGui::TextDisabled("Another variable can be picked later in the Display panel.");
    ImGui::BeginDisabled(opening_.valid());
    open |= ImGui::Button("Open", ImVec2(ImGui::GetFontSize() * 6, 0)) || ImGui::IsKeyPressed(ImGuiKey_Enter);
    ImGui::EndDisabled();
    if (open && !opening_.valid() && varPick_ >= 0 && varPick_ < int(varChoices_.size())) {
        openInputs({varChoices_[size_t(varPick_)].dataset}, varAdd_);
        ImGui::CloseCurrentPopup();
    }
    ImGui::SameLine();
    if (ImGui::Button("Cancel", ImVec2(ImGui::GetFontSize() * 6, 0)) || ImGui::IsKeyPressed(ImGuiKey_Escape))
        ImGui::CloseCurrentPopup();
    ImGui::EndPopup();
}

void App::reopenLayerInputs(int i, const std::vector<std::string>& inputs, const BandSelection& sel) {
    if (opening_.valid() || i < 0 || i >= int(layers_.size())) return;
    openingSel_ = sel;
    openingSel_->chosen = true;
    openInputs(inputs, true);
    openingSel_.reset();
    openingReplace_ = i;
}

void App::uiMultidim() {
    PROFILE_ZONE();
    const CubeInfo& info = *s_->info;
    // The container's variables, listed in the background (a remote file needs requests).
    const std::string container = containerOf(info.firstPath);
    if (mdVarsFor_ != container && !mdVarsLoad_.valid()) {
        mdVarsFor_ = container;
        mdVars_.clear();
        mdVarsLoad_ = std::async(std::launch::async, [container] {
            auto v = listVariables(container);
            glfwPostEmptyEvent();
            return v;
        });
    }
    if (mdVarsLoad_.valid() && mdVarsLoad_.wait_for(std::chrono::seconds(0)) == std::future_status::ready)
        mdVars_ = mdVarsLoad_.get();

    ImGui::SeparatorText("Variable");
    ImGui::BeginDisabled(opening_.valid());
    if (mdVars_.size() > 1) {
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##variable", info.mdVariable.c_str())) {
            for (const MdVariable& v : mdVars_) {
                if (ImGui::Selectable(v.name.c_str(), v.name == info.mdVariable) && v.name != info.mdVariable)
                    reopenLayerInputs(active_, {v.dataset}, BandSelection{});
                ImGui::SetItemTooltip("%s", v.desc.c_str());
            }
            ImGui::EndCombo();
        }
        ImGui::SetItemTooltip("The file's variables: another one reopens the layer in place\n"
                              "(same name in the Layers panel, date, pins and view kept)");
    } else {
        ImGui::TextUnformatted(info.mdVariable.empty() ? displayName(info.firstPath).c_str() : info.mdVariable.c_str());
    }
    if (!info.mdLevels.empty()) {
        const int cur = std::clamp(info.sel.band, 1, int(info.mdLevels.size())) - 1;
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##level", info.mdLevels[size_t(cur)].c_str())) {
            for (int k = 0; k < int(info.mdLevels.size()); ++k)
                if (ImGui::Selectable(info.mdLevels[size_t(k)].c_str(), k == cur) && k != cur) {
                    BandSelection sel;
                    sel.band = k + 1;
                    reopenLayer(active_, sel);
                }
            ImGui::EndCombo();
        }
        ImGui::SetItemTooltip("The variable has more dimensions than time (e.g. pressure level, depth,\n"
                              "ensemble member): the series shows one value of each");
    }
    ImGui::EndDisabled();
    ImGui::TextDisabled("%s", info.description.c_str());
}

// Stages 160-165 of --selftest-ui: H/clim.nc beside the inputs (written by
// tools/make_selftest_data.py; skipped without it). 160 opens it: two
// variables, so the picker is asked; 161 picks "ta" (time, level, lat, lon),
// 162 checks its noleap dates, a level, the values, draws the composite and the
// season, and asks for the other level; 163 checks it and switches to the other
// variable in place; 164 checks t2m and opens the Open URL dialog; 165 closes it.
int App::selfTestMultidim(const std::vector<std::string>& in) {
    namespace fs = std::filesystem;
    auto now = [] { return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count(); };
    auto fail = [&](const char* what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what);
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done);
        ++st_.stage;
        st_.since = now();
    };
    auto ok = [&] { // then the validation stages (app_validation.cpp), which print OK
        st_.stage = 170;
        st_.since = now();
        return -1;
    };
    if (now() - st_.since > 60) return fail("timeout");
    const std::string file = in.empty() ? "" : (fs::u8path(in[0]).parent_path() / "H" / "clim.nc").u8string();
    // ta at (x, y), date t, level k: 100 k + t + 0.01 y; t2m: 10 + t + 0.1 x.
    auto value = [&](int t, int x, int y) {
        float v = NAN;
        CubeReader(s_->info).readPixel(t, x, y, v);
        return v;
    };
    auto loaded = [&] { return !opening_.valid() && s_ && s_->overview.complete() && s_->gpu.statsValid; };
    switch (st_.stage) {
    case 160: {
        std::error_code ec;
        if (file.empty() || !fs::exists(fs::u8path(file), ec)) {
            std::printf("    (no H/clim.nc beside the inputs: NetCDF not checked)\n");
            return ok();
        }
        closeAll();
        varChoices_.clear();
        openInputs({file});
        next("open H/clim.nc (NetCDF of two variables)");
        break;
    }
    case 161: {
        if (opening_.valid() || varChoices_.empty()) {
            if (!opening_.valid() && !layers_.empty()) return fail("a file of several variables should ask which one");
            break;
        }
        if (varChoices_.size() != 2) return fail("H/clim.nc has two variables");
        const MdVariable* ta = nullptr;
        for (const MdVariable& v : varChoices_) {
            std::printf("    variable %s: %s\n", v.name.c_str(), v.desc.c_str());
            if (v.name == "ta") ta = &v;
        }
        if (!ta || ta->desc.find("air temperature") == std::string::npos) return fail("ta and its long name should be listed");
        ImGui::ClosePopupToLevel(0, true); // the picker, as its Open button does
        openInputs({ta->dataset});
        next("variables listed with their long names; ta picked");
        break;
    }
    case 162: {
        if (!loaded()) break;
        const CubeInfo& info = *s_->info;
        std::printf("    %s; %s .. %s; levels:", info.description.c_str(), info.layers.front().label.c_str(),
                    info.layers.back().label.c_str());
        for (const std::string& l : info.mdLevels) std::printf(" [%s]", l.c_str());
        std::printf("; CRS %s\n", info.crsAuthority.c_str());
        if (!info.multidim || info.mdVariable != "ta" || info.T() != 12 || info.mdLevels.size() != 2 || !info.timeIsDate)
            return fail("ta should open as 12 dates at 2 levels");
        // noleap: day 59 is March 1st (in the real 2020, a leap year, it would be February 29th).
        if (info.layers[2].label != "2020-03" || info.layers[11].label != "2020-12" || info.granularity != TimeGranularity::Month)
            return fail("the CF dates (days since 2020-01-01, noleap) should be the months of 2020");
        if (info.crsAuthority != "EPSG:4326") return fail("a lon/lat grid without a grid mapping should be WGS 84");
        if (std::fabs(value(3, 4, 7) - (3 + 0.07f)) > 1e-4f) return fail("level 1 of ta: wrong values");
        if (layers_[0].name != "clim ta") return fail("the layer should be named after the file and the variable");
        mode_ = ModeComposite; // drawn by the next frames, with the season
        season_ = Season{true, 60, 151};
        mapDirty_ = true;
        BandSelection sel;
        sel.band = 2;
        reopenLayer(active_, sel);
        next("ta: noleap months, level 1000 values; composite and season drawn; level 850 asked");
        break;
    }
    case 163: {
        if (!loaded() || s_->info->sel.band != 2) break;
        if (std::fabs(value(3, 4, 7) - (100 + 3 + 0.07f)) > 1e-4f) return fail("level 2 of ta: wrong values");
        if (layers_.size() != 1 || layers_[0].name != "clim ta") return fail("the level should change in place");
        if (mdVars_.empty()) { // the Display panel lists the file's variables in the background
            ImGui::SetWindowFocus("Display"); // a tab behind another one is not drawn
            break;
        }
        std::string t2m;
        for (const MdVariable& v : mdVars_)
            if (v.name == "t2m") t2m = v.dataset;
        if (t2m.empty()) return fail("the Display panel should list the file's variables");
        reopenLayerInputs(active_, {t2m}, BandSelection{});
        next("level 850 in place; the other variable asked from the Display panel's list");
        break;
    }
    case 164: {
        if (!loaded() || s_->info->mdVariable != "t2m") break;
        if (std::fabs(value(5, 9, 3) - (10 + 5 + 0.9f)) > 1e-4f) return fail("t2m: wrong values");
        if (layers_.size() != 1 || layers_[0].name != "clim t2m" || !s_->info->mdLevels.empty())
            return fail("t2m should replace ta in place, named after it, without levels");
        mode_ = ModeValue;
        season_ = Season{};
        openUrlDialog(false);
        next("t2m in place of ta; the Open URL dialog asked");
        break;
    }
    case 165: {
        if (!ImGui::IsPopupOpen("Open URL")) {
            if (now() - st_.since > 2) return fail("the Open URL dialog should open");
            break;
        }
        ImGui::ClosePopupToLevel(0, true);
        next("the Open URL dialog opened");
        return ok();
    }
    }
    return -1;
}
