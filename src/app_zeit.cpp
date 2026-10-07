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
    return ::toolApplicability(tool, *s_->info);
}

std::string toolApplicability(const ZeitTool& tool, const CubeInfo& info) {
    if (!tool.bands.empty() && info.bandsPerDate < int(tool.bands.size()))
        return "needs one file per date with at least " + std::to_string(tool.bands.size()) +
               " bands (e.g. Landsat surface reflectance: blue, green, red, NIR, SWIR1, SWIR2)";
    if (!tool.bands.empty() && !info.timeIsDate)
        return "needs dates (none were found in the file names or band descriptions)";
    if (info.T() < tool.minDates)
        return "needs at least " + std::to_string(tool.minDates) + " dates (this series has " +
               std::to_string(info.T()) + ")";
    if (tool.requiresTime == "annual") {
        if (!info.timeIsDate) return "needs dates (none were found in the file names or band descriptions)";
        std::set<int> years;
        for (int t = 0; t < info.T(); ++t) years.insert(int(std::floor(info.decimalYear(t))));
        if (int(years.size()) != info.T()) return "needs an annual series (one date per year)";
    }
    if (tool.requiresTime == "regular" || tool.minPerYear > 0) {
        if (!info.timeIsDate) return "needs dates (none were found in the file names or band descriptions)";
        // Same rule as the bridge: observations per year from the median spacing.
        std::vector<double> gaps;
        for (int t = 1; t < info.T(); ++t) gaps.push_back(info.decimalYear(t) - info.decimalYear(t - 1));
        std::vector<double> sorted = gaps;
        std::sort(sorted.begin(), sorted.end());
        const double med = sorted.empty() ? 0.0 : sorted[sorted.size() / 2];
        if (med <= 0) return "needs increasing dates";
        const int perYear = std::max(1, int(std::lround(1.0 / med)));
        if (perYear < tool.minPerYear)
            return "needs at least " + std::to_string(tool.minPerYear) + " observations per year (this series has " +
                   std::to_string(perYear) + ")";
        // Regular: months (28-31 days) or 16-day composites wrapping at the year end
        // pass; a missing date must be a band of no-data, not a skipped band.
        if (tool.requiresTime == "regular" && (sorted.front() < 0.4 * med || sorted.back() > 1.6 * med))
            return "needs evenly spaced dates (missing dates must be no-data bands, not skipped)";
    }
    return {};
}

// ---------------------------------------------------------------------------
// Multiband tools: band roles, pixel bands, job inputs
// ---------------------------------------------------------------------------

static const char* kRoles[] = {"blue", "green", "red", "nir", "swir1", "swir2", "thermal"};

BandRoles guessBandRoles(const CubeInfo& info) {
    BandRoles r;
    auto lower = [](std::string s) {
        std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) { return char(std::tolower(c)); });
        s.erase(std::remove_if(s.begin(), s.end(), [](char c) { return c == ' ' || c == '_' || c == '-'; }), s.end());
        return s;
    };
    // Keywords, most specific first; a band takes the first role it matches.
    const std::vector<std::pair<const char*, std::vector<const char*>>> keys = {
        {"swir1", {"swir1", "swir16", "swir161"}}, {"swir2", {"swir2", "swir22", "swir220"}},
        {"thermal", {"thermal", "lwir", "tir", "stb10", "brightness"}}, {"nir", {"nir"}},
        {"blue", {"blue"}}, {"green", {"green"}}, {"red", {"red"}}};
    bool generic = true;
    for (int b = 1; b <= int(info.bandNames.size()); ++b) {
        const std::string n = lower(info.bandNames[b - 1]);
        if (n.rfind("band", 0) != 0) generic = false;
        if (n.find("rededge") != std::string::npos || n.find("qa") != std::string::npos ||
            n.find("fmask") != std::string::npos)
            continue;
        for (const auto& [role, words] : keys) {
            if (r.count(role)) continue;
            bool hit = false;
            for (const char* w : words) hit = hit || n.find(w) != std::string::npos;
            if (hit) {
                r[role] = b;
                break;
            }
        }
    }
    // Unnamed stacks of surface reflectance are usually Blue..SWIR2 in order.
    if (r.empty() && generic && info.bandsPerDate >= 6)
        for (int k = 0; k < 6; ++k) r[kRoles[k]] = k + 1;
    return r;
}

std::string missingBandRoles(const ZeitTool& tool, const BandRoles& roles) {
    std::string out;
    for (const std::string& role : tool.bands)
        if (!roles.count(role)) out += (out.empty() ? "" : ", ") + role;
    return out;
}

json zeitPixelExtras(const CubeInfo& info, const BandRoles& roles) {
    json e = json::object();
    if (info.timeIsDate) {
        std::vector<int64_t> days(info.T());
        for (int t = 0; t < info.T(); ++t) days[t] = info.unixDay(t);
        e["days"] = days;
    }
    if (info.bandsPerDate > 1) {
        auto roleOf = [&](int b) -> json {
            for (const auto& [role, band] : roles)
                if (band == b) return role;
            return nullptr;
        };
        e["shown"] = {{"band", info.sel.ndBand > 0 ? json(nullptr) : roleOf(info.sel.band)},
                      {"nd", info.sel.ndBand > 0 ? json::array({roleOf(info.sel.band), roleOf(info.sel.ndBand)})
                                                 : json(nullptr)}};
    }
    return e;
}

json zeitPixelBands(CubeReader& reader, const CubeInfo& info, const ZeitTool& tool, const BandRoles& roles, int x,
                    int y) {
    json bands = json::object();
    auto series = [&](int band, bool raw) {
        json v = json::array();
        for (int t = 0; t < info.T(); ++t) {
            float f = NAN;
            reader.readBandPixel(t, band, x, y, f, raw);
            v.push_back(std::isfinite(f) ? json(double(f)) : json());
        }
        return v;
    };
    std::vector<std::string> wanted = tool.bands;
    wanted.insert(wanted.end(), tool.optionalBands.begin(), tool.optionalBands.end());
    for (const std::string& role : wanted)
        if (auto it = roles.find(role); it != roles.end()) bands[role] = series(it->second, false);
    json out = {{"bands", bands}};
    if (info.sel.qaBand > 0) {
        out["qa"] = series(info.sel.qaBand, true);
        out["qa_rule"] = qaRuleId(info.sel.qaRule);
    }
    return out;
}

bool zeitJobInputs(const CubeInfo& info, const ZeitTool& tool, const BandRoles& roles, const std::string& dir,
                   const std::string& stem, json& spec, std::string& error) {
    const fs::path d = fs::u8path(dir);
    auto vrt = [&](const std::string& suffix, int band, bool raw) -> std::string {
        const std::string path = (d / (stem + suffix + ".vrt")).u8string();
        return writeCubeVrt(info, path, error, band, raw) ? path : std::string();
    };
    const std::string input = vrt("", 0, false);
    if (input.empty()) return false;
    spec["input"] = input;
    spec["nodata"] = nullptr;
    std::vector<double> years(info.T());
    for (int t = 0; t < info.T(); ++t) years[t] = info.decimalYear(t);
    spec["years"] = years;
    const json extras = zeitPixelExtras(info, roles);
    for (auto it = extras.begin(); it != extras.end(); ++it) spec[it.key()] = it.value();
    if (info.sel.ndBand > 0) {
        const std::string nd = vrt("_b" + std::to_string(info.sel.ndBand), info.sel.ndBand, false);
        if (nd.empty()) return false;
        spec["nd_input"] = nd;
    }
    if (info.sel.qaBand > 0) {
        const std::string qa = vrt("_qa", info.sel.qaBand, true);
        if (qa.empty()) return false;
        spec["qa_input"] = qa;
        spec["qa_rule"] = qaRuleId(info.sel.qaRule);
    }
    std::vector<std::string> wanted = tool.bands;
    wanted.insert(wanted.end(), tool.optionalBands.begin(), tool.optionalBands.end());
    json bands = json::object();
    for (const std::string& role : wanted) {
        auto it = roles.find(role);
        if (it == roles.end()) continue;
        const std::string b = vrt("_" + role, it->second, false);
        if (b.empty()) return false;
        bands[role] = b;
    }
    if (!bands.empty()) spec["bands"] = bands;
    return true;
}

const BandRoles& App::activeBandRoles() {
    if (s_ && bandRolesFor_ != s_->info->id) {
        bandRolesFor_ = s_->info->id;
        if (s_->info->bandNames != bandRolesNames_) {
            bandRoles_ = guessBandRoles(*s_->info);
            bandRolesNames_ = s_->info->bandNames;
        }
        bandReader_.reset();
    }
    return bandRoles_;
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
    const BandRoles& roles = activeBandRoles();
    const bool multiband = !tool->bands.empty();
    if (multiband && !missingBandRoles(*tool, roles).empty()) return;
    const json extras = zeitPixelExtras(*s_->info, roles);
    const auto now = std::chrono::steady_clock::now();
    auto send = [&](SeriesView& v, const json& extra) {
        v.zeitReq = zeit_->runPixel(pixelTool_, params, zeitYears_, v.values, extra);
        v.zeitVersion = pixelVersion_;
        if (v.zeitReq) pixelSent_[v.zeitReq] = now;
    };
    auto request = [&](SeriesView& v) {
        if (v.x < 0 || !v.exact || v.zeitReq != 0 || v.zeitVersion == pixelVersion_) return;
        if (!multiband) return send(v, extras);
        // Multiband: read every band of the pixel in the background first.
        if (bandFetch_) {
            if (bandFetch_->key != v.id || bandFetch_->f.wait_for(std::chrono::seconds(0)) != std::future_status::ready)
                return;
            json bands = bandFetch_->f.get();
            const bool current = bandFetch_->x == v.x && bandFetch_->y == v.y && bandFetch_->version == pixelVersion_;
            bandFetch_.reset();
            if (!current) return; // the cursor moved on: fetched again next frame
            bands.update(extras);
            return send(v, bands);
        }
        if (!bandReader_) bandReader_ = std::make_shared<CubeReader>(s_->info);
        auto reader = bandReader_;
        auto info = s_->info;
        const ZeitTool toolCopy = *tool;
        const BandRoles rolesCopy = roles;
        const int x = v.x, y = v.y;
        bandFetch_ = BandFetch{v.id, x, y, pixelVersion_,
                               std::async(std::launch::async, [reader, info, toolCopy, rolesCopy, x, y] {
                                   json j = zeitPixelBands(*reader, *info, toolCopy, rolesCopy, x, y);
                                   glfwPostEmptyEvent();
                                   return j;
                               })};
    };
    request(hover_);
    for (SeriesView& p : pins_) request(p);
    // ROI fits use the mean of the shown series (single-band tools only).
    if (!multiband && roi_ && roi_->done == s_->info->T() && roiZeitReq_ == 0 && roiZeitVersion_ != pixelVersion_ &&
        !roiMean_.empty()) {
        roiZeitReq_ = zeit_->runPixel(pixelTool_, params, zeitYears_, roiMean_, extras);
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
    json spec = {{"tool", tool.id}, {"params", ui.params}};
    std::string err;
    if (!zeitJobInputs(info, tool, activeBandRoles(), work.u8string(), name + "_" + std::to_string(info.id), spec,
                       err)) {
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
    spec["window"] = {win[0], win[1], win[2], win[3]};
    spec["output_dir"] = out.u8string();
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
    std::string why = toolApplicability(tool);
    if (why.empty() && !tool.bands.empty()) {
        const std::string missing = missingBandRoles(tool, activeBandRoles());
        if (!missing.empty()) why = "choose the band of: " + missing;
    }
    if (!why.empty()) ImGui::TextColored(ImVec4(1.0f, 0.55f, 0.45f, 1), "Not available for this series: %s", why.c_str());

    if (!tool.bands.empty() && s_ && s_->info->bandsPerDate > 1) {
        ImGui::SeparatorText("Bands");
        const CubeInfo& info = *s_->info;
        activeBandRoles();
        std::vector<std::string> roles = tool.bands;
        roles.insert(roles.end(), tool.optionalBands.begin(), tool.optionalBands.end());
        ImGui::PushItemWidth(-200);
        for (const std::string& role : roles) {
            const bool optional = std::find(tool.bands.begin(), tool.bands.end(), role) == tool.bands.end();
            auto it = bandRoles_.find(role);
            const int cur = it == bandRoles_.end() ? 0 : it->second;
            auto label = [&](int b) {
                return b == 0 ? std::string(optional ? "(not used)" : "(choose)")
                              : std::to_string(b) + ": " + info.bandNames[b - 1];
            };
            const std::string lbl = role + (optional ? " (optional)" : "");
            if (ImGui::BeginCombo(lbl.c_str(), label(cur).c_str())) {
                for (int b = 0; b <= info.bandsPerDate; ++b)
                    if (ImGui::Selectable(label(b).c_str(), b == cur)) {
                        if (b == 0) bandRoles_.erase(role);
                        else bandRoles_[role] = b;
                        if (pixelTool_ == tool.id) ++pixelVersion_;
                    }
                ImGui::EndCombo();
            }
        }
        ImGui::PopItemWidth();
        if (info.sel.qaBand > 0)
            ImGui::TextDisabled("Quality: %s (%s)", info.bandNames[info.sel.qaBand - 1].c_str(), qaRuleLabel(info.sel.qaRule));
        else
            ImGui::TextDisabled("No quality band: set one in the Display panel to screen clouds.");
    }

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
        if (o.value("type", "") == "vlines") { // e.g. break dates: dashed-looking thin full-height lines
            std::vector<double> x;
            for (const json& v : xs)
                if (v.is_number()) x.push_back(info.xFromDecimalYear(v.get<double>()));
            if (x.empty()) continue;
            ImPlotSpec spec;
            spec.LineColor = ImVec4(col.x, col.y, col.z, 0.75f);
            spec.LineWeight = 1.5f;
            ImPlot::PlotInfLines(label, x.data(), int(x.size()), spec);
            continue;
        }
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
