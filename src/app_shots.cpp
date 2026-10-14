// The screenshots of the website's guides (--shots TOUR IN OUT, site/*.html):
// the program walks through a guide in a hidden window of a fixed size, one
// stage after the other, and saves the window and some panels as PNGs (the
// frame is read back by render::present). What the tours share, and the tours
// of the Zeit tools; the validation's is in app_validation.cpp.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>

#include <gdal_priv.h>
#include <imgui_internal.h>
#include <implot.h>

#include "glfw.hpp"

namespace fs = std::filesystem;

namespace {

double nowSeconds() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

// RGBA, top row first, to an RGB PNG of the rectangle x, y, w, h (pixels).
bool writeShot(const std::string& path, const unsigned char* rgba, int W, int H, std::array<int, 4> r) {
    const int x0 = std::clamp(r[0], 0, W - 1), y0 = std::clamp(r[1], 0, H - 1);
    const int w = std::clamp(r[2], 1, W - x0), h = std::clamp(r[3], 1, H - y0);
    std::vector<unsigned char> rgb(size_t(w) * h * 3);
    for (int y = 0; y < h; ++y)
        for (int x = 0; x < w; ++x)
            std::copy_n(&rgba[(size_t(y0 + y) * W + x0 + x) * 4], 3, &rgb[(size_t(y) * w + x) * 3]);
    GDALDriver* mem = GetGDALDriverManager()->GetDriverByName("MEM");
    GDALDriver* png = GetGDALDriverManager()->GetDriverByName("PNG");
    if (!mem || !png) return false;
    GDALDataset* ds = mem->Create("", w, h, 3, GDT_Byte, nullptr);
    int bands[3] = {1, 2, 3};
    (void)ds->RasterIO(GF_Write, 0, 0, w, h, rgb.data(), w, h, GDT_Byte, 3, bands, 3, w * 3, 1, nullptr);
    const char* opts[] = {"ZLEVEL=9", nullptr};
    GDALDataset* out = png->CreateCopy(path.c_str(), ds, FALSE, const_cast<char**>(opts), nullptr, nullptr);
    if (out) GDALClose(out);
    GDALClose(ds);
    std::error_code ec;
    fs::remove(fs::u8path(path + ".aux.xml"), ec);
    return out != nullptr;
}

} // namespace

int App::shotsStep(const std::string& tour, const std::string& in, const std::string& out) {
    if (shots_.stage == 0 && shots_.frames == 0) {
        shots_.since = nowSeconds();
        std::error_code ec;
        fs::create_directories(fs::u8path(out), ec);
    }
    if (tour == "validation") return shotsValidationStep(in, out);
    return shotsToolStep(tour, in, out);
}

double App::shotsTime() const { return nowSeconds() - shots_.since; }

void App::shotsNext(const std::string& done) {
    std::printf("[stage %2d] %s\n", shots_.stage, done.c_str());
    std::fflush(stdout);
    ++shots_.stage;
    shots_.since = nowSeconds();
    shots_.frames = 0;
}

std::array<int, 4> App::shotRect(ImVec2 lo, ImVec2 hi, int pad) const {
    const ImVec2 o = ImGui::GetMainViewport()->Pos, k = ImGui::GetIO().DisplayFramebufferScale;
    return {int((lo.x - o.x) * k.x) - pad, int((lo.y - o.y) * k.y) - pad, int((hi.x - lo.x) * k.x) + 2 * pad,
            int((hi.y - lo.y) * k.y) + 2 * pad};
}

std::array<int, 4> App::shotRect(const char* name, bool fit) const {
    const ImGuiWindow* w = ImGui::FindWindowByName(name);
    if (!w) return {0, 0, 0, 0};
    ImVec2 p = w->Pos, s = w->Size;
    if (w->DockNode) {
        p = w->DockNode->Pos;
        s = w->DockNode->Size;
    }
    if (fit) s.y = std::min(s.y, w->DC.CursorMaxPos.y - p.y + ImGui::GetStyle().WindowPadding.y + 6);
    return shotRect(p, p + s, 0);
}

void App::shoot(const std::string& dir, const std::string& name, const ShotCrops& crops) {
    capture_ = [dir = fs::u8path(dir), name, crops](const unsigned char* px, int w, int h) {
        if (!px) {
            std::printf("    %s: this backend does not read its pixels back\n", name.c_str());
            return;
        }
        auto save = [&](const std::string& file, std::array<int, 4> r) {
            const std::string f = (dir / file).u8string();
            const bool ok = writeShot(f, px, w, h, r);
            std::printf("    %s %s (%d x %d)\n", ok ? "wrote" : "COULD NOT WRITE", f.c_str(), r[2], r[3]);
        };
        if (name != "-") save(name + ".png", {0, 0, w, h});
        for (const auto& [suffix, r] : crops) save((name == "-" ? "" : name + "-") + suffix + ".png", r);
        std::fflush(stdout);
    };
}

void App::shotsDock(const std::vector<const char*>& right, const std::vector<const char*>& under) {
    const ImGuiID dock = dockspaceId_;
    ImGui::DockBuilderRemoveNode(dock);
    ImGui::DockBuilderAddNode(dock, ImGuiDockNodeFlags_DockSpace);
    ImGui::DockBuilderSetNodeSize(dock, ImGui::GetMainViewport()->WorkSize);
    ImGuiID rightId, rest, left, mid, leftTop, leftBottom, center, bottom, bottomLeft, bottomRight;
    ImGui::DockBuilderSplitNode(dock, ImGuiDir_Right, 0.37f, &rightId, &rest);
    ImGui::DockBuilderSplitNode(rest, ImGuiDir_Left, 0.25f, &left, &mid);
    ImGui::DockBuilderSplitNode(left, ImGuiDir_Down, 0.55f, &leftBottom, &leftTop);
    ImGui::DockBuilderSplitNode(mid, ImGuiDir_Down, 0.36f, &bottom, &center);
    ImGui::DockBuilderSplitNode(bottom, ImGuiDir_Right, 0.48f, &bottomRight, &bottomLeft);
    ImGui::DockBuilderDockWindow("Layers", leftTop);
    ImGui::DockBuilderDockWindow("Files", leftTop);
    ImGui::DockBuilderDockWindow("Display", leftBottom);
    ImGui::DockBuilderDockWindow("Map", center);
    ImGui::DockBuilderDockWindow("Time series", bottomLeft);
    ImGui::DockBuilderDockWindow("Statistics", bottomRight);
    for (const char* w : under) ImGui::DockBuilderDockWindow(w, bottomRight);
    for (const char* w : right) ImGui::DockBuilderDockWindow(w, rightId);
    ImGui::DockBuilderFinish(dock);
}

bool App::shotsMenu(const char* menu, const std::string& dir, const std::string& file, bool hoverFirst) {
    ImGuiIO& io = ImGui::GetIO();
    const auto it = shots_.menus.find(menu);
    if (it == shots_.menus.end()) return false;
    const ImVec4 m = it->second;
    if (shots_.frames <= 2) { // a click on its title
        io.AddMousePosEvent((m.x + m.z) / 2, (m.y + m.w) / 2);
        io.AddMouseButtonEvent(ImGuiMouseButton_Left, shots_.frames == 1);
        return false;
    }
    ImGuiContext& g = *GImGui;
    const ImGuiWindow* popup = g.OpenPopupStack.empty() ? nullptr : g.OpenPopupStack.back().Window;
    if (!popup) return false;
    io.AddMousePosEvent(hoverFirst ? popup->Pos.x + 60 : (m.x + m.z) / 2,
                        hoverFirst ? popup->Pos.y + g.Style.WindowPadding.y + g.FontSize * 0.5f + 2 : (m.y + m.w) / 2);
    if (shotsTime() < 2) return false;
    // The menu, its title and the tooltip of the item pointed at.
    ImVec2 lo(m.x, m.y), hi = popup->Pos + popup->Size;
    for (ImGuiWindow* w : g.Windows)
        if (w->Active && (w->Flags & ImGuiWindowFlags_Tooltip)) {
            lo = ImMin(lo, w->Pos);
            hi = ImMax(hi, w->Pos + w->Size);
        }
    shoot(dir, "-", {{file, shotRect(lo, hi, 12)}});
    return true;
}

// ---------------------------------------------------------------------------
// The tours of the Zeit tools
// ---------------------------------------------------------------------------

int App::shotsToolStep(const std::string& tour, const std::string& in, const std::string& out) {
    auto fail = [&](const std::string& what) {
        std::printf("FAIL (%s, stage %d): %s\n", tour.c_str(), shots_.stage, what.c_str());
        return 1;
    };
    ++shots_.frames;
    const double t = shotsTime();
    // A run's stages (it starts in one and is waited for in the next) may take long.
    const bool waiting = shots_.job && shots_.stage <= shots_.jobStage + 1;
    if (t > (waiting ? 7200 : 120)) return fail("timeout");
    ImGuiIO& io = ImGui::GetIO();
    io.AddMousePosEvent(-FLT_MAX, -FLT_MAX); // no hover, unless a stage points somewhere

    // --- What the tours share ---
    auto zeitReady = [&] { return zeit_ && zeit_->state() == ZeitClient::State::Ready; };
    auto quiet = [&] {
        if (opening_.valid() || !s_ || !s_->overview.complete() || !resultLoads_.empty()) return false;
        const SeriesLayer* L = activeLayer();
        return !(L && compositeOn(*L) && !compositeReady(*L, t_));
    };
    auto tool = [&](const std::string& id) -> const ZeitTool* {
        if (!zeit_) return nullptr;
        for (const ZeitTool& z : zeit_->tools())
            if (z.id == id) return &z;
        return nullptr;
    };
    // A tool's window open with its defaults and `params`, fitted on the chart.
    auto openTool = [&](const std::string& id, const json& params) {
        const ZeitTool* z = tool(id);
        if (!z) return false;
        ToolUi& ui = toolUi_[id];
        ui.open = true;
        ui.params = json::object();
        for (const ZeitParam& p : z->params) ui.params[p.id] = p.def;
        for (auto it = params.begin(); it != params.end(); ++it) ui.params[it.key()] = it.value();
        pixelTool_ = z->pixel ? id : "";
        ++pixelVersion_;
        return true;
    };
    auto setParams = [&](const std::string& id, const json& params) {
        for (auto it = params.begin(); it != params.end(); ++it) toolUi_[id].params[it.key()] = it.value();
        ++pixelVersion_;
    };
    // Its run on the raster (scope: 0 whole image, 1 visible area, 2 ROI).
    auto run = [&](const std::string& id, int scope) {
        const ZeitTool* z = tool(id);
        if (!z) return false;
        toolUi_[id].scope = scope;
        const size_t before = jobs_.size();
        shots_.results = results_.size();
        runTool(*z);
        if (jobs_.size() == before) return false;
        shots_.job = jobs_.back();
        return true;
    };
    // The run is over and its maps are on the map.
    auto ran = [&]() -> int { // -1 running, 0 done, 1 failed
        if (!shots_.job) return 1;
        if (shots_.job->state == ZeitJob::State::Running) return -1;
        if (shots_.job->state != ZeitJob::State::Done) return 1;
        return shots_.job->resultsLoaded && resultLoads_.empty() && results_.size() > shots_.results ? 0 : -1;
    };
    // Only the result named `name` (e.g. "Year of detection") of the last run drawn.
    auto showResult = [&](const std::string& name) {
        bool found = false;
        for (size_t i = 0; i < results_.size(); ++i) {
            ResultLayer& r = results_[i];
            const bool on = i >= shots_.results && r.name.size() >= name.size() &&
                            r.name.compare(r.name.size() - name.size(), name.size(), name) == 0;
            r.visible = on;
            found |= on;
        }
        mapDirty_ = true;
        return found;
    };
    auto focus = [&](const char* name) {
        if (ImGuiWindow* w = ImGui::FindWindowByName(name)) ImGui::FocusWindow(w);
    };
    auto setColormap = [&](const char* name) {
        const int k = ImPlot::GetColormapIndex(name);
        if (k >= 0) cmap_[ModeValue] = k;
        mapDirty_ = true;
    };
    // A pin at (x, y) in the active layer's CRS.
    auto pinAt = [&](double x, double y) {
        double px, py;
        if (s_ && s_->info->geoToPixel(x, y, px, py)) addPin(int(px), int(py));
    };
    // The map centred on the active layer's pixel (px, py), `scale` screen points per pixel.
    auto zoomTo = [&](double px, double py, float scale) {
        scale_ = scale;
        offset_ = ImVec2(canvasSize_.x / 2 - float(px + 0.5) * scale, canvasSize_.y / 2 - float(py + 0.5) * scale);
        viewTouched_ = true;
        mapDirty_ = true;
    };
    auto zoomToGeo = [&](double x, double y, float scale) {
        double px, py;
        if (s_ && s_->info->geoToPixel(x, y, px, py)) zoomTo(px, py, scale);
    };
    // The first date whose label starts with `prefix` (e.g. "2025-01"), else the last.
    auto dateOf = [&](const char* prefix) {
        for (int k = 0; k < s_->info->T(); ++k)
            if (s_->info->layers[k].label.rfind(prefix, 0) == 0) return k;
        return s_->info->T() - 1;
    };
    // Stages every tour of a raster run has: started (and its task shot), done.
    auto runStage = [&](const std::string& id, int scope, const char* taskShot) -> int { // -1 running, 0 next, 1 failed
        if (shots_.jobStage != shots_.stage) {
            if (!run(id, scope)) return 1;
            shots_.jobStage = shots_.stage;
            showTasks_ = true;
            return -1;
        }
        focus("Tasks");
        if (taskShot && shots_.job->progress < 0.2 && ran() < 0) return -1;
        if (taskShot) shoot(out, "-", {{taskShot, shotRect("Tasks", true)}});
        return 0;
    };
    auto doneStage = [&]() -> int { // -1 running, 0 done (next stage), 1 failed
        const int r = ran();
        if (r != 0) return r;
        char done[64];
        std::snprintf(done, sizeof(done), "the run done in %.0f s",
                      std::chrono::duration<double>(std::chrono::steady_clock::now() - shots_.job->started).count());
        shotsNext(done);
        return 0;
    };
    const std::string toolWin = "###tool_" + tour;

    if (tour == "landtrendr") {
        // IN: an annual series (site/landtrendr.html: NDVI x 10000 of Rondonia, 1985-2024).
        const double cleared[2] = {-63.083966, -10.356003}; // forest until 2003, pasture from 2004
        const double regrown[2] = {-63.344298, -10.220986}; // cleared in 1999, forest again since
        const double forest[2] = {-63.202544, -10.326628};  // forest throughout
        switch (shots_.stage) {
        case 0:
            layoutPending_ = true;
            opts_.sel = BandSelection{};
            openInputs({in});
            shotsNext("the annual series opening");
            break;
        case 1: // three pins: forest cleared in 2004, cleared in 1999 and grown back, forest throughout
            if (opening_.valid() || !s_ || !dockspaceId_) break;
            shotsDock({toolWin.c_str()}, {"Tasks"});
            setColormap("NDVI");
            setT(s_->info->T() - 1);
            pinAt(cleared[0], cleared[1]);
            pinAt(regrown[0], regrown[1]);
            pinAt(forest[0], forest[1]);
            shotsNext(std::to_string(s_->info->T()) + " dates; three pins");
            break;
        case 2:
            if (shots_.frames == 30) fitRequested_ = true; // once the panels have their size
            if (!quiet() || !zeitReady() || t < 4) break;
            shoot(out, "01-series", {{"chart", shotRect("Time series")}});
            shotsNext("the series and three pins");
            break;
        case 3: // the Tools menu
            if (!shotsMenu("Tools", out, "02-menu", false)) {
                if (t > 10) return fail("the Tools menu did not open");
                break;
            }
            shotsNext("the Tools menu");
            break;
        case 4: // LandTrendr fitted on the clearing
            if (shots_.frames == 1) {
                ImGui::ClosePopupsExceptModals();
                clearPins(); // one at a time on the chart (and the click that opened the menu reached the map)
                pinAt(cleared[0], cleared[1]);
                if (!openTool("landtrendr", json::object())) return fail("no LandTrendr tool");
                break;
            }
            if (shots_.frames == 3) focus(toolWin.c_str());
            if (!quiet() || t < 6) break;
            shoot(out, "03-fit",
                  {{"tool", shotRect(toolWin.c_str(), true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("LandTrendr on the clearing");
            break;
        case 5: // looking for gains: the regrowth
            if (shots_.frames == 1) {
                clearPins();
                pinAt(regrown[0], regrown[1]);
                setParams("landtrendr", {{"event_type", "gain"}});
            }
            if (t < 5) break;
            shoot(out, "-", {{"04-gain-chart", shotRect("Time series")}, {"04-gain-stats", shotRect("Statistics")}});
            shotsNext("gain on the regrowth");
            break;
        case 6: // fewer segments
            if (shots_.frames == 1) {
                clearPins();
                pinAt(cleared[0], cleared[1]);
                setParams("landtrendr", {{"event_type", "loss"}, {"max_segments", 2}});
            }
            if (t < 5) break;
            shoot(out, "-", {{"05-two-segments", shotRect("Time series")}});
            shotsNext("two segments at most");
            break;
        case 7: // the run on the whole image: losses of 0.25 NDVI or more that start in forest
            if (shots_.frames == 1) {
                setParams("landtrendr", {{"max_segments", 6}, {"min_magnitude", 2500}, {"pre_val_threshold", 7500}});
                if (!run("landtrendr", 0)) return fail("the run did not start");
                showTasks_ = true;
                break;
            }
            if (shots_.frames == 3) focus("Tasks");
            if (shots_.job->progress < 0.25 && ran() < 0) break;
            shoot(out, "-", {{"06-task", shotRect("Tasks", true)}});
            shotsNext("the run, under way");
            break;
        case 8: {
            const int r = ran();
            if (r < 0) break;
            if (r > 0) return fail("the run failed: " + shots_.job->error);
            char done[64];
            std::snprintf(done, sizeof(done), "the run done in %.0f s",
                          std::chrono::duration<double>(std::chrono::steady_clock::now() - shots_.job->started).count());
            shotsNext(done);
            break;
        }
        case 9:
            if (shots_.frames == 1) {
                if (!showResult("Year of detection")) return fail("no year of detection");
                focus(toolWin.c_str());
            }
            if (!quiet() || t < 3) break;
            shoot(out, "07-yod", {{"layers", shotRect("Layers")}});
            shotsNext("year of detection");
            break;
        case 10:
            if (shots_.frames == 1) showResult("Magnitude");
            if (!quiet() || t < 3) break;
            shoot(out, "08-magnitude");
            shotsNext("magnitude");
            break;
        case 11:
            std::printf("OK\n");
            return 0;
        }
        return -1;
    }
    // Landsat surface reflectance, one file per date with its QA_PIXEL (tools/make_guide_data.py
    // landsat: Rondonia, 2020-2026): the map in SWIR1 / NIR / red, the chart in NDVI.
    const double clearing[2] = {-63.119001, -10.130167}; // forest cleared in May-June 2022
    const double thinned[2] = {-63.309533, -10.294558};  // forest opened in 2021, partly back since
    const double standing[2] = {-63.202544, -10.326628}; // forest throughout
    auto openLandsat = [&]() {
        layoutPending_ = true;
        opts_.sel = BandSelection{};
        opts_.sel.band = 4;   // NIR
        opts_.sel.ndBand = 3; // red: NDVI (the quality band is found by its name)
        openInputs({in});
    };
    auto landsatView = [&]() {
        SeriesLayer& L = layers_[0];
        L.comp = std::make_unique<BandComposite>();
        L.comp->on = true;
        L.comp->bands = {5, 4, 3};
        setT(s_->info->T() - 1);
    };

    if (tour == "ccdc") {
        switch (shots_.stage) {
        case 0:
            openLandsat();
            shotsNext("the Landsat series opening");
            break;
        case 1:
            if (opening_.valid() || !s_ || !dockspaceId_) break;
            shotsDock({"###tool_ccdc", "###tool_ccdc_synthetic"}, {"Tasks"});
            landsatView();
            pinAt(clearing[0], clearing[1]);
            pinAt(thinned[0], thinned[1]);
            pinAt(standing[0], standing[1]);
            shotsNext(std::to_string(s_->info->T()) + " dates; three pins");
            break;
        case 2:
            if (shots_.frames == 30) fitRequested_ = true;
            if (!quiet() || !zeitReady() || t < 5) break;
            shoot(out, "01-series", {{"chart", shotRect("Time series")}});
            shotsNext("the series and three pins");
            break;
        case 3: // CCDC on the clearing
            if (shots_.frames == 1) {
                clearPins();
                pinAt(clearing[0], clearing[1]);
                if (!openTool("ccdc", json::object())) return fail("no CCDC tool");
                break;
            }
            if (shots_.frames == 3) focus("###tool_ccdc");
            if (!quiet() || t < 10) break;
            shoot(out, "02-fit", {{"tool", shotRect("###tool_ccdc", true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("CCDC on the clearing");
            break;
        case 4: // and on the forest opened in 2021
            if (shots_.frames == 1) {
                clearPins();
                pinAt(thinned[0], thinned[1]);
                ++pixelVersion_;
            }
            if (t < 8) break;
            shoot(out, "-", {{"03-thinned-chart", shotRect("Time series")}, {"03-thinned-stats", shotRect("Statistics")}});
            shotsNext("CCDC on the opened forest");
            break;
        case 5: { // the run on the area around the clearing
            if (shots_.frames == 1) {
                clearPins();
                pinAt(clearing[0], clearing[1]);
                zoomToGeo(clearing[0], clearing[1], 3);
            }
            if (shots_.frames < 10) break; // the zoom drawn before the visible area is taken
            const int r = runStage("ccdc", 1, "04-task");
            if (r > 0) return fail("the run did not start");
            if (r == 0) shotsNext("CCDC over the visible area");
            break;
        }
        case 6:
            if (doneStage() > 0) return fail("the run failed: " + shots_.job->error);
            break;
        case 7:
            if (shots_.frames == 1 && !showResult("Largest break date")) return fail("no break date");
            if (shots_.frames == 3) focus("###tool_ccdc");
            if (!quiet() || t < 3) break;
            shoot(out, "05-break-date", {{"layers", shotRect("Layers")}});
            shotsNext("the largest break's date");
            break;
        case 8:
            if (shots_.frames == 1) showResult("Largest break magnitude");
            if (!quiet() || t < 3) break;
            shoot(out, "06-magnitude");
            shotsNext("its magnitude");
            break;
        case 9: // the synthetic image of a cloudy date
            if (shots_.frames == 1) {
                for (ResultLayer& r : results_) r.visible = false;
                setT(dateOf("2025-01"));
                if (!openTool("ccdc_synthetic", json::object())) return fail("no CCDC synthetic image tool");
                break;
            }
            if (shots_.frames == 3) focus("###tool_ccdc_synthetic");
            if (!quiet() || t < 8) break;
            shoot(out, "07-observed", {{"tool", shotRect("###tool_ccdc_synthetic", true)}});
            shotsNext("the observed image of " + s_->info->layers[t_].label);
            break;
        case 10: {
            const int r = runStage("ccdc_synthetic", 1, nullptr);
            if (r > 0) return fail("the synthetic run did not start");
            if (r == 0) shotsNext("the synthetic image under way");
            break;
        }
        case 11:
            if (doneStage() > 0) return fail("the synthetic run failed: " + shots_.job->error);
            break;
        case 12:
            if (shots_.frames == 1 && !showResult("Synthetic (shown series)")) return fail("no synthetic image");
            if (!quiet() || t < 3) break;
            shoot(out, "08-synthetic");
            shotsNext("the synthetic NDVI");
            break;
        case 13:
            std::printf("OK\n");
            return 0;
        }
        return -1;
    }

    if (tour == "coded") {
        const json params = {{"monitor_start", 2022.0}, {"train_years", 2.0}};
        switch (shots_.stage) {
        case 0:
            openLandsat();
            shotsNext("the Landsat series opening");
            break;
        case 1:
            if (opening_.valid() || !s_ || !dockspaceId_) break;
            shotsDock({"###tool_coded", "###tool_ndfi"}, {"Tasks"});
            landsatView();
            pinAt(clearing[0], clearing[1]);
            shotsNext("the clearing pinned");
            break;
        case 2: // the NDFI of every date, over the area around the clearing
            if (shots_.frames == 30) zoomToGeo(clearing[0], clearing[1], 3);
            if (shots_.frames < 40 || !quiet() || !zeitReady()) break;
            if (!toolUi_["ndfi"].open && !openTool("ndfi", json::object())) return fail("no NDFI tool");
            focus("###tool_ndfi");
            if (!ImGui::FindWindowByName("###tool_ndfi") || t < 6) break;
            shoot(out, "-", {{"01-ndfi-tool", shotRect("###tool_ndfi", true)}});
            shotsNext("the NDFI tool");
            break;
        case 3: {
            const int r = runStage("ndfi", 1, nullptr);
            if (r > 0) return fail("the NDFI run did not start");
            if (r == 0) shotsNext("the NDFI under way");
            break;
        }
        case 4:
            if (doneStage() > 0) return fail("the NDFI run failed: " + shots_.job->error);
            break;
        case 5: // the NDFI as a layer, active: its series on the chart
            if (shots_.frames == 1) {
                if (layers_.size() < 2) return fail("the NDFI should open as a layer");
                for (ResultLayer& r : results_) r.visible = false; // the series itself, not its summary maps
                toolUi_["ndfi"].open = false;
                setActive(int(layers_.size()) - 1);
                setColormap("NDVI");
                setT(dateOf("2022-07"));
            }
            if (!quiet() || t < 6) break;
            shoot(out, "02-ndfi", {{"chart", shotRect("Time series")}, {"layers", shotRect("Layers")}});
            shotsNext("the NDFI layer");
            break;
        case 6: // CODED on the clearing (the NDVI layer active again)
            if (shots_.frames == 1) {
                setActive(0);
                setT(s_->info->T() - 1);
                if (!openTool("coded", params)) return fail("no CODED tool");
                break;
            }
            if (shots_.frames == 3) {
                focus("###tool_coded");
                focus("Statistics");
            }
            if (!quiet() || t < 10) break;
            shoot(out, "03-fit", {{"tool", shotRect("###tool_coded", true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("CODED on the clearing");
            break;
        case 7: // and on the forest opened in 2021
            if (shots_.frames == 1) {
                clearPins();
                pinAt(thinned[0], thinned[1]);
                ++pixelVersion_;
            }
            if (t < 8) break;
            shoot(out, "-", {{"04-thinned-chart", shotRect("Time series")}, {"04-thinned-stats", shotRect("Statistics")}});
            shotsNext("CODED on the opened forest");
            break;
        case 8: {
            if (shots_.frames == 1) {
                clearPins();
                pinAt(clearing[0], clearing[1]);
                for (size_t i = 1; i < layers_.size(); ++i) layers_[i].visible = false; // the NDFI under the results
            }
            const int r = runStage("coded", 1, nullptr);
            if (r > 0) return fail("the CODED run did not start");
            if (r == 0) shotsNext("CODED over the visible area");
            break;
        }
        case 9:
            if (doneStage() > 0) return fail("the CODED run failed: " + shots_.job->error);
            break;
        case 10:
            if (shots_.frames == 1 && !showResult("Strata")) return fail("no strata");
            if (shots_.frames == 3) focus("###tool_coded");
            if (!quiet() || t < 3) break;
            shoot(out, "05-strata", {{"layers", shotRect("Layers")}});
            shotsNext("the strata");
            break;
        case 11:
            if (shots_.frames == 1) showResult("First change date");
            if (!quiet() || t < 3) break;
            shoot(out, "06-first-change");
            shotsNext("the first change's date");
            break;
        case 12: { // stricter: 5 RMSEs below the training model
            if (shots_.frames == 1) setParams("coded", {{"thresh", 5.0}});
            const int r = runStage("coded", 1, nullptr);
            if (r > 0) return fail("the second CODED run did not start");
            if (r == 0) shotsNext("CODED again, 5 RMSEs");
            break;
        }
        case 13:
            if (doneStage() > 0) return fail("the second CODED run failed: " + shots_.job->error);
            break;
        case 14:
            if (shots_.frames == 1 && !showResult("Strata")) return fail("no strata");
            if (!quiet() || t < 3) break;
            shoot(out, "07-strata-5");
            shotsNext("the strata at 5 RMSEs");
            break;
        case 15:
            std::printf("OK\n");
            return 0;
        }
        return -1;
    }

    if (tour == "bfast") {
        const int lasting[2] = {435, 153}; // EVI 0.42 a year until 2012, about 0.3 since
        const int dropped[2] = {334, 485}; // lower from 2009
        switch (shots_.stage) {
        case 0:
            layoutPending_ = true;
            opts_.sel = BandSelection{};
            openInputs({in});
            shotsNext("the EVI series opening");
            break;
        case 1:
            if (opening_.valid() || !s_ || !dockspaceId_) break;
            shotsDock({"###tool_bfast", "###tool_bfast_lite", "###tool_bfast_monitor"}, {"Tasks"});
            setColormap("NDVI");
            setT(dateOf("2025-07"));
            addPin(lasting[0], lasting[1]);
            shotsNext(std::to_string(s_->info->T()) + " dates; a pin");
            break;
        case 2:
            if (shots_.frames == 30) fitRequested_ = true;
            if (!quiet() || !zeitReady() || t < 4) break;
            shoot(out, "01-series", {{"chart", shotRect("Time series")}});
            shotsNext("the series");
            break;
        case 3: // BFAST
            if (shots_.frames == 1 && !openTool("bfast", json::object())) return fail("no BFAST tool");
            if (shots_.frames == 3) focus("###tool_bfast");
            if (!quiet() || t < 12) break;
            shoot(out, "02-bfast", {{"tool", shotRect("###tool_bfast", true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("BFAST on the chart");
            break;
        case 4: // BFAST Lite
            if (shots_.frames == 1 && !openTool("bfast_lite", json::object())) return fail("no BFAST Lite tool");
            if (shots_.frames == 3) focus("###tool_bfast_lite");
            if (!quiet() || t < 10) break;
            shoot(out, "03-lite", {{"tool", shotRect("###tool_bfast_lite", true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("BFAST Lite on the chart");
            break;
        case 5: // BFAST Monitor, monitoring from 2010
            if (shots_.frames == 1 && !openTool("bfast_monitor", {{"monitor_start", 2010.0}})) return fail("no BFAST Monitor tool");
            if (shots_.frames == 3) focus("###tool_bfast_monitor");
            if (!quiet() || t < 10) break;
            shoot(out, "04-monitor", {{"tool", shotRect("###tool_bfast_monitor", true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("BFAST Monitor on the chart");
            break;
        case 6: // the other pin
            if (shots_.frames == 1) {
                clearPins();
                addPin(dropped[0], dropped[1]);
                openTool("bfast_lite", json::object());
            }
            if (t < 8) break;
            shoot(out, "-", {{"05-dropped-chart", shotRect("Time series")}});
            shotsNext("BFAST Lite on the other pin");
            break;
        case 7: { // BFAST Monitor (monitoring from 2010) over the whole image: seconds, where BFAST takes an hour
            if (shots_.frames == 1) {
                clearPins();
                addPin(lasting[0], lasting[1]);
                openTool("bfast_monitor", {{"monitor_start", 2010.0}});
                focus("###tool_bfast_monitor");
            }
            if (shots_.frames < 10) break;
            const int r = runStage("bfast_monitor", 0, "06-task");
            if (r > 0) return fail("the run did not start");
            if (r == 0) shotsNext("BFAST Monitor over the image");
            break;
        }
        case 8:
            if (doneStage() > 0) return fail("the run failed: " + shots_.job->error);
            break;
        case 9:
            if (shots_.frames == 1 && !showResult("Break date")) return fail("no break date");
            if (!quiet() || t < 3) break;
            shoot(out, "07-break-date", {{"layers", shotRect("Layers")}});
            shotsNext("the break dates");
            break;
        case 10:
            if (shots_.frames == 1) showResult("Magnitude (median residual)");
            if (!quiet() || t < 3) break;
            shoot(out, "08-magnitude");
            shotsNext("the magnitudes");
            break;
        case 11:
            std::printf("OK\n");
            return 0;
        }
        return -1;
    }

    if (tour == "phenology") {
        const int crop[2] = {71, 319}; // a field: a strong season every year
        switch (shots_.stage) {
        case 0:
            layoutPending_ = true;
            opts_.sel = BandSelection{};
            openInputs({in});
            shotsNext("the EVI series opening");
            break;
        case 1:
            if (opening_.valid() || !s_ || !dockspaceId_) break;
            shotsDock({"###tool_phenology"}, {"Tasks"});
            setColormap("NDVI");
            setT(dateOf("2024-06"));
            addPin(crop[0], crop[1]);
            shotsNext("a pin on a field");
            break;
        case 2: // Phenology on the chart
            if (shots_.frames == 30) fitRequested_ = true;
            if (shots_.frames < 40 || !quiet() || !zeitReady()) break;
            if (!toolUi_["phenology"].open && !openTool("phenology", json::object())) return fail("no Phenology tool");
            focus("###tool_phenology");
            if (!ImGui::FindWindowByName("###tool_phenology") || t < 12) break;
            shoot(out, "01-fit", {{"tool", shotRect("###tool_phenology", true)}, {"chart", shotRect("Time series")}, {"stats", shotRect("Statistics")}});
            shotsNext("Phenology on the chart");
            break;
        case 3: { // the run on the whole image
            const int r = runStage("phenology", 0, "02-task");
            if (r > 0) return fail("the run did not start");
            if (r == 0) shotsNext("Phenology over the image");
            break;
        }
        case 4:
            if (doneStage() > 0) return fail("the run failed: " + shots_.job->error);
            break;
        case 5:
            if (shots_.frames == 1 && !showResult("Start of season (SOS)")) return fail("no start of season");
            if (!quiet() || t < 3) break;
            shoot(out, "03-sos", {{"layers", shotRect("Layers")}});
            shotsNext("the start of the season");
            break;
        case 6:
            if (shots_.frames == 1) showResult("Peak of season (POS)");
            if (!quiet() || t < 3) break;
            shoot(out, "04-pos");
            shotsNext("the peak");
            break;
        case 7:
            if (shots_.frames == 1) showResult("Length of season (EOS - SOS)");
            if (!quiet() || t < 3) break;
            shoot(out, "05-los");
            shotsNext("the length of the season");
            break;
        case 8:
            std::printf("OK\n");
            return 0;
        }
        return -1;
    }
    return fail("unknown tour (validation, landtrendr, ccdc, coded, bfast, phenology)");
}
