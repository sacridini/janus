// Zeit integration: tools menu, tool windows, tasks, result layers and the
// per-pixel models drawn on the chart. The algorithms run in Zeit, in a
// separate Python process (see zeit_client.hpp and zeit_bridge/).
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <ctime>
#include <filesystem>
#include <set>

#include <implot.h>

#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

int colormapByName(const std::string& name) {
    const int n = ImPlot::GetColormapCount();
    for (int i = 0; i < n; ++i)
        if (name == ImPlot::GetColormapName(i)) return i;
    return ImPlotColormap_Viridis;
}

std::string timestamp() {
    const std::time_t now = std::time(nullptr);
    std::tm tm{};
    localtime_s(&tm, &now);
    char buf[32];
    std::strftime(buf, sizeof(buf), "%Y%m%d-%H%M%S", &tm);
    return buf;
}

std::string seriesName(const CubeInfo& info, const std::vector<std::string>& inputs) {
    fs::path p = fs::u8path(inputs.size() == 1 ? inputs[0] : info.firstPath);
    if (inputs.size() != 1) p = p.parent_path();
    std::string n = p.has_stem() ? p.stem().u8string() : p.filename().u8string();
    if (n.empty()) n = "series";
    for (char& c : n)
        if (c == ' ' || c == ':' || c == '*' || c == '?') c = '_';
    return n;
}

} // namespace

// ---------------------------------------------------------------------------
// Process lifecycle
// ---------------------------------------------------------------------------

ZeitConfig App::zeitConfig() const {
    ZeitConfig c;
    const fs::path rt = fs::u8path(platform::exeDir()) / "runtime";
    c.python = opts_.zeitPython.empty() ? (rt / "python" / "python.exe").u8string() : opts_.zeitPython;
    c.bridge = opts_.zeitBridge.empty() ? (rt / "tsv_zeit_bridge.py").u8string() : opts_.zeitBridge;
    c.bundled = opts_.zeitPython.empty();
    c.logPath = (fs::u8path(platform::appDataDir()) / "zeit.log").u8string();
    return c;
}

// Never on the startup path: called once a series is open (or from the menu).
void App::startZeit() {
    if (!zeit_) zeit_ = std::make_unique<ZeitClient>();
    if (zeit_->state() == ZeitClient::State::Off) zeit_->start(zeitConfig());
}

std::string App::toolApplicability(const ZeitTool& tool) const {
    if (!s_) return "no series is open";
    const CubeInfo& info = *s_->info;
    if (info.T() < tool.minDates)
        return "needs at least " + std::to_string(tool.minDates) + " dates (this series has " +
               std::to_string(info.T()) + ")";
    if (tool.requiresTime == "annual") {
        if (!info.timeIsDate) return "needs dates (none were found in the file names or band descriptions)";
        std::set<int> years;
        for (int t = 0; t < info.T(); ++t) years.insert(int(std::floor(info.decimalYear(t))));
        if (int(years.size()) != info.T()) return "needs an annual series (one date per year)";
    }
    return {};
}

// ---------------------------------------------------------------------------
// Per frame
// ---------------------------------------------------------------------------

void App::pumpZeit() {
    if (!zeit_) return;
    const auto now = std::chrono::steady_clock::now();
    for (PixelReply& r : zeit_->takeReplies()) {
        auto it = pixelSent_.find(r.id);
        if (it != pixelSent_.end()) {
            lastPixelFitMs_ = std::chrono::duration<double, std::milli>(now - it->second).count();
            pixelSent_.erase(it);
        }
        const json value = r.ok ? r.result : json{{"error", r.error}};
        auto apply = [&](SeriesView& v) {
            if (v.zeitReq != r.id) return false;
            v.zeitResult = value;
            v.zeitReq = 0;
            return true;
        };
        if (apply(hover_)) continue;
        bool done = false;
        for (SeriesView& p : pins_)
            if ((done = apply(p))) break;
        if (!done && roiZeitReq_ == r.id) {
            roiZeitResult_ = value;
            roiZeitReq_ = 0;
        }
    }
    if (s_) requestPixelFits();

    // Finished jobs: load their outputs as layers (only for the series they ran on).
    for (auto& job : jobs_) {
        if (job->state != ZeitJob::State::Done || job->resultsLoaded) continue;
        job->resultsLoaded = true;
        // Results belong to the layer the job ran on (skipped if it was closed).
        const uint64_t cubeId = jobCube_[job.get()];
        const SeriesLayer* owner = nullptr;
        for (const SeriesLayer& Ly : layers_)
            if (Ly.session->info->id == cubeId) owner = &Ly;
        if (!owner) continue;
        const CubeInfo& oinfo = *owner->session->info;
        json result;
        {
            std::lock_guard<std::mutex> lk(job->m);
            result = job->result;
        }
        const json win = result.value("window", json::array({0, 0, oinfo.width, oinfo.height}));
        const ZeitTool* tool = zeit_->tool(job->toolId);
        bool first = true;
        for (const json& o : result.value("outputs", json::array())) {
            ResultLayer L;
            L.cubeId = cubeId;
            L.name = (tool ? tool->name : job->toolId) + ": " + o.value("name", "");
            L.path = o.value("path", "");
            L.unit = o.value("unit", "");
            L.cmap = colormapByName(o.value("colormap", "Viridis"));
            L.x0 = win[0];
            L.y0 = win[1];
            L.w = int(win[2]) - L.x0;
            L.h = int(win[3]) - L.y0;
            L.visible = first; // the first output (e.g. year of detection) is shown
            first = false;
            resultLoads_.push_back(std::async(std::launch::async, [L]() mutable {
                LoadedResult r;
                r.ok = loadResultRaster(L.path, 4096, L.data, L.tw, L.th, r.error);
                if (r.ok) autoResultRange(L);
                r.layer = std::move(L);
                glfwPostEmptyEvent();
                return r;
            }));
        }
    }
    for (auto it = resultLoads_.begin(); it != resultLoads_.end();) {
        if (it->wait_for(std::chrono::seconds(0)) != std::future_status::ready) {
            ++it;
            continue;
        }
        LoadedResult r = it->get();
        it = resultLoads_.erase(it);
        if (!r.ok) {
            error_ = r.error;
            openErrorPopup_ = true;
            continue;
        }
        const CubeInfo* oinfo = nullptr;
        for (const SeriesLayer& Ly : layers_)
            if (Ly.session->info->id == r.layer.cubeId) oinfo = Ly.session->info.get();
        if (!oinfo || r.layer.x0 + r.layer.w > oinfo->width || r.layer.y0 + r.layer.h > oinfo->height) continue;
        r.layer.tex = Gpu::createTileTexture(r.layer.tw, r.layer.th, r.layer.data.data());
        results_.push_back(std::move(r.layer));
        mapDirty_ = true;
    }
}

void App::requestPixelFits() {
    if (pixelTool_.empty() || !zeit_ || zeit_->state() != ZeitClient::State::Ready) return;
    const ZeitTool* tool = zeit_->tool(pixelTool_);
    if (!tool || !toolApplicability(*tool).empty()) return;
    const json& params = toolUi_[pixelTool_].params;
    const auto now = std::chrono::steady_clock::now();
    auto request = [&](SeriesView& v) {
        if (v.x < 0 || !v.exact || v.zeitReq != 0 || v.zeitVersion == pixelVersion_) return;
        v.zeitReq = zeit_->runPixel(pixelTool_, params, zeitYears_, v.values);
        v.zeitVersion = pixelVersion_;
        if (v.zeitReq) pixelSent_[v.zeitReq] = now;
    };
    request(hover_);
    for (SeriesView& p : pins_) request(p);
    if (roi_ && roi_->done == s_->info->T() && roiZeitReq_ == 0 && roiZeitVersion_ != pixelVersion_ &&
        !roiMean_.empty()) {
        roiZeitReq_ = zeit_->runPixel(pixelTool_, params, zeitYears_, roiMean_);
        roiZeitVersion_ = pixelVersion_;
        if (roiZeitReq_) pixelSent_[roiZeitReq_] = now;
    }
}

// ---------------------------------------------------------------------------
// Running a tool on the raster
// ---------------------------------------------------------------------------

void App::runTool(const ZeitTool& tool) {
    if (!s_ || !zeit_) return;
    const CubeInfo& info = *s_->info;
    ToolUi& ui = toolUi_[tool.id];

    int win[4] = {0, 0, info.width, info.height};
    const char* scopeName = "whole image";
    if (ui.scope == 1) {
        win[0] = std::clamp(int(std::floor(-offset_.x / scale_)), 0, info.width - 1);
        win[1] = std::clamp(int(std::floor(-offset_.y / scale_)), 0, info.height - 1);
        win[2] = std::clamp(int(std::ceil((canvasSize_.x - offset_.x) / scale_)), win[0] + 1, info.width);
        win[3] = std::clamp(int(std::ceil((canvasSize_.y - offset_.y) / scale_)), win[1] + 1, info.height);
        scopeName = "visible area";
    } else if (ui.scope == 2 && roi_) {
        win[0] = roi_->x0;
        win[1] = roi_->y0;
        win[2] = roi_->x1;
        win[3] = roi_->y1;
        scopeName = "ROI";
    }

    std::error_code ec;
    const std::string name = seriesName(info, lastInputs_);
    const fs::path work = fs::u8path(platform::appDataDir()) / "work";
    fs::create_directories(work, ec);
    const fs::path vrt = work / (name + "_" + std::to_string(info.id) + ".vrt");
    std::string err;
    if (!writeCubeVrt(info, vrt.u8string(), err)) {
        error_ = err;
        openErrorPopup_ = true;
        return;
    }
    const fs::path out = fs::u8path(resultsDir_) / (name + "_" + tool.id + "_" + timestamp());
    fs::create_directories(out, ec);
    if (ec) {
        error_ = "could not create " + out.u8string() + ": " + ec.message();
        openErrorPopup_ = true;
        return;
    }
    const json spec = {{"tool", tool.id},
                       {"params", ui.params},
                       {"input", vrt.u8string()},
                       {"years", zeitYears_},
                       {"nodata", nullptr},
                       {"window", {win[0], win[1], win[2], win[3]}},
                       {"output_dir", out.u8string()}};
    char title[160];
    std::snprintf(title, sizeof(title), "%s - %s (%d x %d px)", tool.name.c_str(), scopeName, win[2] - win[0],
                  win[3] - win[1]);
    auto job = zeit_->startJob(spec, (out / "job.json").u8string(), title);
    jobCube_[job.get()] = info.id;
    jobs_.push_back(job);
    showTasks_ = true;
}

// ---------------------------------------------------------------------------
// UI
// ---------------------------------------------------------------------------

void App::uiToolsMenu() {
    if (!ImGui::BeginMenu("Tools")) return;
    const ZeitClient::State st = zeit_ ? zeit_->state() : ZeitClient::State::Off;
    if (st == ZeitClient::State::Off) {
        if (ImGui::MenuItem("Start Zeit")) startZeit();
        ImGui::TextDisabled("Zeit starts automatically when a series is opened");
    } else if (st == ZeitClient::State::Starting) {
        ImGui::TextDisabled("Zeit is starting...");
    } else if (st == ZeitClient::State::Failed) {
        ImGui::TextDisabled("Zeit is not available");
        ImGui::SetItemTooltip("%s", zeit_->error().c_str());
        if (ImGui::MenuItem("Retry")) {
            zeit_.reset();
            startZeit();
        }
    } else {
        std::vector<std::string> categories;
        for (const ZeitTool& t : zeit_->tools())
            if (std::find(categories.begin(), categories.end(), t.category) == categories.end())
                categories.push_back(t.category);
        for (const std::string& c : categories) {
            ImGui::SeparatorText(c.c_str());
            for (const ZeitTool& t : zeit_->tools()) {
                if (t.category != c) continue;
                ToolUi& ui = toolUi_[t.id];
                if (ImGui::MenuItem(t.name.c_str(), nullptr, ui.open)) ui.open = !ui.open;
            }
        }
        ImGui::Separator();
        ImGui::TextDisabled("Zeit %s (Python %s)", zeit_->zeitVersion().c_str(), zeit_->pythonVersion().c_str());
    }
    ImGui::Separator();
    ImGui::MenuItem("Tasks", nullptr, &showTasks_);
    ImGui::EndMenu();
}

void App::uiToolWindow(const ZeitTool& tool) {
    ToolUi& ui = toolUi_[tool.id];
    if (!ui.open) return;
    if (!ui.params.is_object()) {
        ui.params = json::object();
        for (const ZeitParam& p : tool.params) ui.params[p.id] = p.def;
    }
    ImGui::SetNextWindowSize(ImVec2(440, 620), ImGuiCond_FirstUseEver);
    const std::string title = tool.name + "###tool_" + tool.id;
    if (!ImGui::Begin(title.c_str(), &ui.open)) {
        ImGui::End();
        return;
    }
    ImGui::TextWrapped("%s", tool.description.c_str());
    const std::string why = toolApplicability(tool);
    if (!why.empty()) ImGui::TextColored(ImVec4(1.0f, 0.55f, 0.45f, 1), "Not available for this series: %s", why.c_str());

    ImGui::SeparatorText("Parameters");
    bool changed = false;
    ImGui::PushItemWidth(-200);
    for (const ZeitParam& p : tool.params) {
        ImGui::PushID(p.id.c_str());
        json& v = ui.params[p.id];
        if (p.type == "int") {
            int x = v.is_number() ? v.get<int>() : 0;
            if (ImGui::InputInt(p.label.c_str(), &x)) {
                if (p.max > p.min) x = std::clamp(x, int(p.min), int(p.max));
                v = x;
                changed = true;
            }
        } else if (p.type == "float") {
            double x = v.is_number() ? v.get<double>() : 0.0;
            if (ImGui::InputDouble(p.label.c_str(), &x, 0, 0, "%.4g")) {
                if (p.max > p.min) x = std::clamp(x, p.min, p.max);
                v = x;
                changed = true;
            }
        } else if (p.type == "bool") {
            bool x = v.is_boolean() && v.get<bool>();
            if (ImGui::Checkbox(p.label.c_str(), &x)) {
                v = x;
                changed = true;
            }
        } else if (p.type == "enum") {
            const std::string cur = v.is_string() ? v.get<std::string>() : "";
            int idx = 0;
            for (size_t i = 0; i < p.options.size(); ++i)
                if (p.options[i] == cur) idx = int(i);
            const char* preview = idx < int(p.labels.size()) ? p.labels[idx].c_str() : cur.c_str();
            if (ImGui::BeginCombo(p.label.c_str(), preview)) {
                for (size_t i = 0; i < p.options.size(); ++i)
                    if (ImGui::Selectable(i < p.labels.size() ? p.labels[i].c_str() : p.options[i].c_str(),
                                          int(i) == idx)) {
                        v = p.options[i];
                        changed = true;
                    }
                ImGui::EndCombo();
            }
        }
        if (!p.help.empty()) ImGui::SetItemTooltip("%s", p.help.c_str());
        ImGui::PopID();
    }
    ImGui::PopItemWidth();
    if (ImGui::Button("Reset to defaults")) {
        for (const ZeitParam& p : tool.params) ui.params[p.id] = p.def;
        changed = true;
    }
    if (changed && pixelTool_ == tool.id) ++pixelVersion_;

    if (tool.pixel) {
        ImGui::SeparatorText("On the chart");
        bool on = pixelTool_ == tool.id;
        ImGui::BeginDisabled(!why.empty());
        if (ImGui::Checkbox("Fit the cursor, pins and ROI series", &on)) {
            pixelTool_ = on ? tool.id : "";
            ++pixelVersion_;
        }
        ImGui::EndDisabled();
        ImGui::TextDisabled("Runs on every exact series; the model is drawn over it\n"
                            "and its numbers are added to the Statistics table.");
    }

    if (tool.raster && s_) {
        ImGui::SeparatorText("Run on the raster");
        const char* scopes[] = {"Whole image", "Visible area", "ROI"};
        if (ui.scope == 2 && !roi_) ui.scope = 0;
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##scope", scopes[ui.scope])) {
            for (int i = 0; i < 3; ++i) {
                ImGui::BeginDisabled(i == 2 && !roi_);
                if (ImGui::Selectable(scopes[i], ui.scope == i)) ui.scope = i;
                ImGui::EndDisabled();
            }
            ImGui::EndCombo();
        }
        ImGui::TextDisabled("Output: %s", resultsDir_.c_str());
        ImGui::SameLine();
        if (ImGui::SmallButton("Change...")) {
            const std::string dir = platform::openFolderDialog();
            if (!dir.empty()) resultsDir_ = dir;
        }
        ImGui::BeginDisabled(!why.empty() || zeit_->state() != ZeitClient::State::Ready);
        if (ImGui::Button("Run", ImVec2(-1, 0))) runTool(tool);
        ImGui::EndDisabled();
        ImGui::TextDisabled("Runs in a separate process (all CPU cores); see Tools > Tasks.");
    }
    ImGui::End();
}

void App::uiTasks() {
    if (!showTasks_) return;
    ImGui::SetNextWindowSize(ImVec2(560, 300), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Tasks", &showTasks_)) {
        ImGui::End();
        return;
    }
    if (jobs_.empty()) ImGui::TextDisabled("No tasks yet. Run a tool from the Tools menu.");
    int remove = -1;
    for (size_t i = 0; i < jobs_.size(); ++i) {
        ZeitJob& j = *jobs_[i];
        ImGui::PushID(int(i));
        const ZeitJob::State st = j.state;
        const double el = st == ZeitJob::State::Running
                              ? std::chrono::duration<double>(std::chrono::steady_clock::now() - j.started).count()
                              : j.seconds;
        std::string msg, err;
        {
            std::lock_guard<std::mutex> lk(j.m);
            msg = j.message;
            err = j.error;
        }
        ImGui::TextUnformatted(j.title.c_str());
        char overlay[96];
        const char* stName = st == ZeitJob::State::Running ? "running" : st == ZeitJob::State::Done ? "done"
                             : st == ZeitJob::State::Cancelled ? "cancelled" : "failed";
        std::snprintf(overlay, sizeof(overlay), "%s  %.0f%%  %.0f s", stName, j.progress * 100.0, el);
        ImGui::ProgressBar(float(j.progress), ImVec2(-160, 0), overlay);
        ImGui::SameLine();
        if (st == ZeitJob::State::Running) {
            if (ImGui::Button("Cancel", ImVec2(-1, 0))) j.cancel();
        } else {
            if (ImGui::Button("Folder", ImVec2(75, 0))) platform::openInExplorer(j.outputDir);
            ImGui::SameLine();
            if (ImGui::Button("Remove", ImVec2(-1, 0))) remove = int(i);
        }
        if (!msg.empty() && st == ZeitJob::State::Running) ImGui::TextDisabled("%s", msg.c_str());
        if (!err.empty()) {
            ImGui::PushTextWrapPos(0);
            ImGui::TextColored(ImVec4(1.0f, 0.55f, 0.45f, 1), "%s", err.c_str());
            ImGui::PopTextWrapPos();
        }
        ImGui::Separator();
        ImGui::PopID();
    }
    if (remove >= 0) {
        jobCube_.erase(jobs_[remove].get());
        jobs_.erase(jobs_.begin() + remove);
    }
    if (zeit_ && ImGui::SmallButton("Open Zeit log")) platform::openInExplorer(zeit_->config().logPath);
    ImGui::End();
}

// Tool results of one series, listed under it in the Layers panel.
void App::uiResultsOf(uint64_t cubeId) {
    int remove = -1;
    for (size_t i = 0; i < results_.size(); ++i) {
        ResultLayer& r = results_[i];
        if (r.cubeId != cubeId) continue;
        ImGui::PushID(int(i));
        if (ImGui::Checkbox(r.name.c_str(), &r.visible)) mapDirty_ = true;
        ImGui::SetItemTooltip("%s", r.path.c_str());
        ImGui::SameLine(ImGui::GetContentRegionAvail().x + ImGui::GetCursorPosX() - 18);
        if (ImGui::SmallButton("x")) remove = int(i);
        ImGui::SetItemTooltip("Remove this layer (the file stays on disk)");
        if (r.visible) {
            ImGui::Indent();
            ImGui::SetNextItemWidth(-1);
            if (ImPlot::ColormapButton(ImPlot::GetColormapName(r.cmap), ImVec2(-1, 0), r.cmap))
                ImGui::OpenPopup("rcmap");
            if (ImGui::BeginPopup("rcmap")) {
                for (int c = 4; c < ImPlot::GetColormapCount(); ++c)
                    if (ImPlot::ColormapButton(ImPlot::GetColormapName(c), ImVec2(220, 0), c)) {
                        r.cmap = c;
                        mapDirty_ = true;
                        ImGui::CloseCurrentPopup();
                    }
                ImGui::EndPopup();
            }
            float lo = r.lo, hi = r.hi;
            ImGui::SetNextItemWidth(-60);
            if (ImGui::DragFloatRange2("##range", &lo, &hi, std::max(1e-6f, (r.hi - r.lo) / 300.f), 0, 0, "%.4g",
                                       "%.4g")) {
                r.lo = lo;
                r.hi = std::max(hi, lo + 1e-6f);
                mapDirty_ = true;
            }
            ImGui::SameLine();
            if (ImGui::Button("Auto", ImVec2(-1, 0))) {
                autoResultRange(r);
                mapDirty_ = true;
            }
            ImGui::SetNextItemWidth(-1);
            if (ImGui::SliderFloat("##opacity", &r.opacity, 0.05f, 1.0f, "opacity %.2f")) mapDirty_ = true;
            ImGui::Unindent();
        }
        ImGui::PopID();
    }
    if (remove >= 0) {
        glDeleteTextures(1, &results_[remove].tex);
        results_.erase(results_.begin() + remove);
        mapDirty_ = true;
    }
}

void App::clearResults(uint64_t cubeId) {
    for (auto it = results_.begin(); it != results_.end();) {
        if (cubeId == 0 || it->cubeId == cubeId) {
            glDeleteTextures(1, &it->tex);
            it = results_.erase(it);
        } else {
            ++it;
        }
    }
    if (cubeId == 0) {
        for (auto& f : resultLoads_) f.wait();
        resultLoads_.clear();
    }
    mapDirty_ = true;
}

// Draws the model returned by a pixel run (same legend entry as the series).
void App::drawZeitOverlays(const char* label, const json& result, ImVec4 col, const SeriesStats& st) {
    if (!result.is_object() || !s_) return;
    const CubeInfo& info = *s_->info;
    for (const json& o : result.value("overlays", json::array())) {
        const json xs = o.value("x", json::array()), ys = o.value("y", json::array());
        const int n = int(std::min(xs.size(), ys.size()));
        if (n == 0) continue;
        std::vector<double> x(n), y(n);
        for (int i = 0; i < n; ++i) {
            x[i] = info.xFromDecimalYear(xs[i].is_number() ? xs[i].get<double>() : NAN);
            const double v = ys[i].is_number() ? ys[i].get<double>() : NAN;
            y[i] = plotValues_ == 1 ? v - st.mean : plotValues_ == 2 ? (st.std > 0 ? (v - st.mean) / st.std : 0.0) : v;
        }
        ImPlotSpec spec;
        spec.LineColor = ImVec4(col.x, col.y, col.z, 0.9f);
        spec.LineWeight = 2.5f;
        if (o.value("type", "") == "markers") {
            spec.Marker = ImPlotMarker_Square;
            spec.MarkerSize = 4.0f;
            spec.MarkerFillColor = col;
            spec.MarkerLineColor = ImVec4(0, 0, 0, 1);
            ImPlot::PlotScatter(label, x.data(), y.data(), n, spec);
        } else {
            ImPlot::PlotLine(label, x.data(), y.data(), n, spec);
        }
    }
}
