// Zeit integration: tools menu, tool windows, tasks, result layers and the
// per-pixel models drawn on the chart. The algorithms run in Zeit, in a
// separate Python process (see zeit_client.hpp and zeit_bridge/).
#define IMGUI_DEFINE_MATH_OPERATORS
#include "embedding.hpp"
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <ctime>
#include <filesystem>
#include <fstream>
#include <set>
#include <string_view>

#include <implot.h>

#include "glfw.hpp"
#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

int colormapByName(const std::string& name) {
    const int n = ImPlot::GetColormapCount();
    for (int i = 0; i < n; ++i)
        if (name == ImPlot::GetColormapName(i)) return i;
    return ImPlotColormap_Viridis;
}

// Class k (1-based) gets the k-th colour of a qualitative colormap (enough
// colours for every class: Dark has 8, Paired 12).
void classRange(ResultLayer& r) {
    if (int(r.classes.size()) > ImPlot::GetColormapSize(r.cmap)) r.cmap = ImPlotColormap_Paired;
    r.lo = 0.5f;
    r.hi = float(ImPlot::GetColormapSize(r.cmap)) + 0.5f;
}

std::string timestamp() {
    const std::time_t now = std::time(nullptr);
    const std::tm tm = platform::localTime(now);
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
    const fs::path rt = fs::u8path(platform::resourceDir()) / "runtime";
    c.python = opts_.zeitPython.empty() ? bundledPython(rt.u8string()) : opts_.zeitPython;
    c.bridge = opts_.zeitBridge.empty() ? (rt / "janus_zeit_bridge.py").u8string() : opts_.zeitBridge;
    c.bundled = opts_.zeitPython.empty();
    c.logPath = (fs::u8path(platform::appDataDir()) / "zeit.log").u8string();
    c.threads = settings_.processingThreads();
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
    if (tool.layerInput) return {}; // runs on result maps, whatever the series
    if (embeddingMeta(info).is) // as Zeit itself, which refuses such cubes
        return "a layer of embeddings has no physical unit nor seasonal signal (see the Embeddings panel)";
    if (!tool.bands.empty() && info.bandsPerDate < int(tool.bands.size())) {
        std::string list;
        for (const std::string& b : tool.bands) list += (list.empty() ? "" : ", ") + b;
        return "needs one file per date with several bands: " + list + " (e.g. Landsat surface reflectance)";
    }
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
    // Janus's date labels: the band descriptions of series outputs (read back as the dates).
    std::vector<std::string> labels(info.T());
    for (int t = 0; t < info.T(); ++t) labels[t] = info.layers[t].label;
    spec["labels"] = labels;
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
    // Another processing threads setting: the serve process (pixel fits,
    // estimates) is replaced by one started with it, once that one is ready
    // and this one has no request pending. Raster jobs follow at once
    // (setJobThreads); running jobs keep theirs.
    // A process takes ~0.1 s to exit (up to 2 s while still starting): not on the UI thread.
    auto retire = [this](std::unique_ptr<ZeitClient>& z) {
        if (z) zeitRetired_.push_back(std::async(std::launch::async, [old = std::move(z)]() mutable { old.reset(); }));
    };
    const int threads = settings_.processingThreads();
    const bool settled = std::chrono::steady_clock::now() - threadsChanged_ > std::chrono::seconds(1); // slider let go
    if (zeit_->state() == ZeitClient::State::Ready && zeit_->config().threads != threads) {
        if ((!zeitNext_ || zeitNext_->config().threads != threads) && settled) {
            retire(zeitNext_);
            zeitNext_ = std::make_unique<ZeitClient>();
            zeitNext_->start(zeitConfig());
        }
        bool idle = pixelSent_.empty();
        for (const auto& [id, ui] : toolUi_) idle = idle && ui.estReq == 0;
        if (idle && zeitNext_ && zeitNext_->config().threads == threads &&
            zeitNext_->state() == ZeitClient::State::Ready) {
            retire(zeit_);
            zeit_ = std::move(zeitNext_);
            for (auto& [id, ui] : toolUi_) ui.estKey = json(); // estimated again with the new limit
        }
    } else if (zeitNext_ && zeit_->config().threads == threads) {
        retire(zeitNext_); // back to the serve process' own value
    }
    zeitRetired_.erase(std::remove_if(zeitRetired_.begin(), zeitRetired_.end(),
                                      [](const std::future<void>& f) {
                                          return f.wait_for(std::chrono::seconds(0)) == std::future_status::ready;
                                      }),
                       zeitRetired_.end());
    const auto now = std::chrono::steady_clock::now();
    for (PixelReply& r : zeit_->takeReplies()) {
        auto it = pixelSent_.find(r.id);
        if (it != pixelSent_.end()) {
            lastPixelFitMs_ = std::chrono::duration<double, std::milli>(now - it->second).count();
            pixelSent_.erase(it);
        }
        const json value = r.ok ? r.result : json{{"error", r.error}};
        bool estimate = false;
        for (auto& [id, ui] : toolUi_)
            if (ui.estReq == r.id) {
                ui.estReq = 0;
                ui.estimate = r.ok ? r.result : json();
                ui.estError = r.ok ? "" : r.error;
                estimate = true;
            }
        if (estimate) continue;
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
        for (SeriesLayer& L : layers_) {
            if (done) break;
            if ((done = apply(L.hover))) break;
            for (SeriesView& p : L.pins)
                if ((done = apply(p))) break;
        }
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
            if (o.value("series", false)) { // e.g. the NDFI of every date: a new layer, opened below
                seriesToOpen_.push_back(o.value("path", ""));
                continue;
            }
            ResultLayer L;
            L.cubeId = cubeId;
            L.name = (tool ? tool->name : job->toolId) + ": " + o.value("name", "");
            L.path = o.value("path", "");
            L.unit = o.value("unit", "");
            L.cmap = colormapByName(o.value("colormap", "Viridis"));
            L.classes = o.value("classes", std::vector<std::string>());
            for (const json& cs : o.value("class_series", json::array())) {
                ResultLayer::ClassSeries c;
                for (const json& v : cs.value("x", json::array())) c.x.push_back(v.is_number() ? v.get<double>() : NAN);
                for (const json& v : cs.value("y", json::array())) c.y.push_back(v.is_number() ? v.get<double>() : NAN);
                L.classSeries.push_back(std::move(c));
            }
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
        if (!r.layer.classes.empty()) classRange(r.layer);
        r.layer.tex = Gpu::createTileTexture(r.layer.tw, r.layer.th, r.layer.data.data());
        results_.push_back(std::move(r.layer));
        mapDirty_ = true;
    }
    // Series outputs (e.g. NDFI, a screened series) join as layers, one open at a time; the
    // series being studied stays active.
    if (!seriesToOpen_.empty() && !opening_.valid() && !layers_.empty()) {
        const std::string path = seriesToOpen_.front();
        seriesToOpen_.erase(seriesToOpen_.begin());
        openingKeepActive_ = true;
        openInputs({path}, true);
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
    // Other visible layers on the chart: the same tool on their own series and
    // dates (single-band tools; multiband ones would need each layer's bands).
    if (chartLayers_ == 1 && !multiband)
        for (SeriesLayer& L : layers_) {
            if (&L == activeLayer() || !L.visible) continue;
            const CubeInfo& li = *L.session->info;
            if (!::toolApplicability(*tool, li).empty()) continue;
            std::vector<double> years(li.T());
            for (int t = 0; t < li.T(); ++t) years[t] = li.decimalYear(t);
            const json lextras = zeitPixelExtras(li, BandRoles{});
            auto other = [&](SeriesView& v) {
                if (v.x < 0 || !v.exact || v.zeitReq != 0 || v.zeitVersion == pixelVersion_) return;
                v.zeitReq = zeit_->runPixel(pixelTool_, params, years, v.values, lextras);
                v.zeitVersion = pixelVersion_;
                if (v.zeitReq) pixelSent_[v.zeitReq] = now;
            };
            other(L.hover);
            for (SeriesView& p : L.pins) other(p);
        }
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

// Pixel window of the chosen scope: whole image, visible area or ROI.
void App::toolWindow(const ToolUi& ui, int win[4], const char** scopeName) const {
    const CubeInfo& info = *s_->info;
    win[0] = 0;
    win[1] = 0;
    win[2] = info.width;
    win[3] = info.height;
    *scopeName = "whole image";
    if (ui.scope == 1) {
        win[0] = std::clamp(int(std::floor(-offset_.x / scale_)), 0, info.width - 1);
        win[1] = std::clamp(int(std::floor(-offset_.y / scale_)), 0, info.height - 1);
        win[2] = std::clamp(int(std::ceil((canvasSize_.x - offset_.x) / scale_)), win[0] + 1, info.width);
        win[3] = std::clamp(int(std::ceil((canvasSize_.y - offset_.y) / scale_)), win[1] + 1, info.height);
        *scopeName = "visible area";
    } else if (ui.scope == 2 && roi_) {
        win[0] = roi_->x0;
        win[1] = roi_->y0;
        win[2] = roi_->x1;
        win[3] = roi_->y1;
        *scopeName = "ROI";
    }
}

// Asks the bridge to time the tool on a sample of the series (once the
// parameters have been still for a moment).
void App::updateEstimate(const ZeitTool& tool, ToolUi& ui) {
    if (!s_ || !zeit_ || zeit_->state() != ZeitClient::State::Ready || ui.estReq != 0) return;
    const json key = {{"cube", s_->info->id}, {"params", ui.params}, {"roles", activeBandRoles()}};
    const auto now = std::chrono::steady_clock::now();
    if (key == ui.estKey) return;
    if (ui.estChanged == std::chrono::steady_clock::time_point{}) {
        ui.estChanged = now;
        return;
    }
    if (now - ui.estChanged < std::chrono::milliseconds(600)) return;
    ui.estChanged = {};
    ui.estKey = key;
    ui.estimate = json();
    json spec = {{"tool", tool.id}, {"params", ui.params}};
    std::error_code ec;
    const fs::path work = fs::u8path(platform::appDataDir()) / "work";
    fs::create_directories(work, ec);
    std::string err;
    if (!zeitJobInputs(*s_->info, tool, activeBandRoles(), work.u8string(),
                       seriesName(*s_->info, lastInputs_) + "_" + std::to_string(s_->info->id), spec, err)) {
        ui.estError = err;
        return;
    }
    spec["window"] = {0, 0, s_->info->width, s_->info->height};
    ui.estReq = zeit_->call("estimate", spec);
}

static std::string formatDuration(double s) {
    char buf[64];
    if (s < 1) std::snprintf(buf, sizeof(buf), "under a second");
    else if (s < 90) std::snprintf(buf, sizeof(buf), "~%.0f s", s);
    else if (s < 5400) std::snprintf(buf, sizeof(buf), "~%.0f min", s / 60);
    else std::snprintf(buf, sizeof(buf), "~%.1f h", s / 3600);
    return buf;
}

void App::runTool(const ZeitTool& tool) {
    if (!s_ || !zeit_) return;
    const CubeInfo& info = *s_->info;
    ToolUi& ui = toolUi_[tool.id];
    int win[4];
    const char* scopeName = nullptr;
    toolWindow(ui, win, &scopeName);

    std::error_code ec;
    const std::string name = seriesName(info, lastInputs_);
    const fs::path work = fs::u8path(platform::appDataDir()) / "work";
    fs::create_directories(work, ec);
    json spec = {{"tool", tool.id}, {"params", ui.params}};
    std::string err;
    uint64_t cubeId = info.id;
    std::string mapsScope;
    if (tool.layerInput) {
        // On result maps: the outputs are on the first one's grid and belong to its series.
        const ResultLayer* first = nullptr;
        for (const ZeitParam& p : tool.params)
            if (p.type == "layers" && ui.params.value(p.id, json::array()).size() > 0) {
                const std::string path = ui.params[p.id][0].value("path", "");
                for (const ResultLayer& r : results_)
                    if (r.path == path) first = &r;
            }
        if (!first) {
            error_ = "Choose the maps to run on first.";
            openErrorPopup_ = true;
            return;
        }
        cubeId = first->cubeId;
        win[0] = first->x0;
        win[1] = first->y0;
        win[2] = first->x0 + first->w;
        win[3] = first->y0 + first->h;
        size_t n = 0;
        for (const ZeitParam& p : tool.params)
            if (p.type == "layers") n += ui.params.value(p.id, json::array()).size();
        mapsScope = std::to_string(n) + " maps";
        scopeName = mapsScope.c_str();
    } else if (!zeitJobInputs(info, tool, activeBandRoles(), work.u8string(), name + "_" + std::to_string(info.id),
                              spec, err)) {
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
    jobCube_[job.get()] = cubeId;
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
    ImGui::MenuItem("Log", nullptr, &showZeitLog_);
    ImGui::SetItemTooltip("Each raster run's log (what ran, Python's output, progress, the result)\n"
                          "and zeit.log");
    ImGui::EndMenu();
}

// Parameter of type "patterns" (e.g. TWDTW classes): reference series taken
// from the pins or the ROI mean of the active layer, each with a class name.
// Value: [{"name", "years", "days", "values"}, ...] (days: since 1970, or null).
bool App::uiPatterns(const ZeitParam& p, json& v) {
    if (!v.is_array()) v = json::array();
    bool changed = false;
    ImGui::TextUnformatted(p.label.c_str());
    if (!p.help.empty()) ImGui::SetItemTooltip("%s", p.help.c_str());
    int remove = -1;
    for (size_t k = 0; k < v.size(); ++k) {
        ImGui::PushID(int(k));
        char name[64] = {};
        std::snprintf(name, sizeof(name), "%s", v[k].value("name", "").c_str());
        ImGui::SetNextItemWidth(160);
        if (ImGui::InputText("##name", name, sizeof(name))) {
            v[k]["name"] = std::string(name);
            changed = true;
        }
        ImGui::SameLine();
        ImGui::TextDisabled("%s, %d dates", v[k].value("from", "").c_str(), int(v[k].value("values", json::array()).size()));
        ImGui::SameLine();
        if (ImGui::SmallButton("x")) remove = int(k);
        ImGui::PopID();
    }
    if (remove >= 0) {
        v.erase(size_t(remove));
        changed = true;
    }
    if (v.empty()) ImGui::TextDisabled("No patterns yet: drop pins on known places, then add them here.");
    auto add = [&](const std::string& from, const std::vector<float>& values) {
        const CubeInfo& info = *s_->info;
        json vals = json::array(), days = json::array();
        for (float f : values) vals.push_back(std::isfinite(f) ? json(double(f)) : json());
        for (int t = 0; t < info.T(); ++t) days.push_back(info.timeIsDate ? json(info.unixDay(t)) : json());
        v.push_back({{"name", "Class " + std::to_string(v.size() + 1)},
                     {"from", from},
                     {"years", zeitYears_},
                     {"days", info.timeIsDate ? days : json(nullptr)},
                     {"values", vals}});
        changed = true;
    };
    ImGui::SetNextItemWidth(-1);
    if (s_ && ImGui::BeginCombo("##addpattern", "Add a pattern from...")) {
        for (const SeriesView& pin : pins_) {
            const std::string label = "Pin " + std::to_string(pin.id) + " (" + std::to_string(pin.x) + ", " +
                                      std::to_string(pin.y) + ")" + (pin.exact ? "" : " - reading...");
            ImGui::BeginDisabled(!pin.exact);
            if (ImGui::Selectable(label.c_str())) add("pin " + std::to_string(pin.id), pin.values);
            ImGui::EndDisabled();
        }
        const bool roiDone = roi_ && roi_->done == s_->info->T() && !roiMean_.empty();
        ImGui::BeginDisabled(!roiDone);
        if (ImGui::Selectable("ROI mean")) add("ROI", roiMean_);
        ImGui::EndDisabled();
        if (pins_.empty() && !roiDone) ImGui::TextDisabled("Click the map to drop pins first.");
        ImGui::EndCombo();
    }
    return changed;
}

// Parameter of type "layers" (e.g. the maps the agreement compares): result maps
// of any series, of the parameter's unit, in the order they were checked (the
// first one is the grid). Value: [{"name", "path"}, ...].
bool App::uiLayersParam(const ZeitParam& p, json& v) {
    if (!v.is_array()) v = json::array();
    bool changed = false;
    auto present = [&](const std::string& path) {
        for (const ResultLayer& r : results_)
            if (r.path == path) return true;
        return false;
    };
    for (size_t k = v.size(); k-- > 0;) // maps removed from the Layers panel
        if (!present(v[k].value("path", ""))) {
            v.erase(k);
            changed = true;
        }
    ImGui::TextUnformatted(p.label.c_str());
    if (!p.help.empty()) ImGui::SetItemTooltip("%s", p.help.c_str());
    int shown = 0;
    for (size_t i = 0; i < results_.size(); ++i) {
        const ResultLayer& r = results_[i];
        if (!p.unit.empty() && r.unit != p.unit) continue;
        std::string series;
        for (const SeriesLayer& L : layers_)
            if (L.session->info->id == r.cubeId) series = L.name;
        int at = -1;
        for (size_t k = 0; k < v.size(); ++k)
            if (v[k].value("path", "") == r.path) at = int(k);
        bool on = at >= 0;
        ImGui::PushID(int(i));
        const std::string label = r.name + "  [" + series + "]" + (at == 0 ? "  (grid)" : "") + "###map";
        if (ImGui::Checkbox(label.c_str(), &on)) {
            if (on) v.push_back({{"name", r.name}, {"path", r.path}});
            else v.erase(size_t(at));
            changed = true;
        }
        ImGui::SetItemTooltip("%s", r.path.c_str());
        ImGui::PopID();
        ++shown;
    }
    if (shown == 0)
        ImGui::TextDisabled(p.unit == "year" ? "No map of dates yet: run LandTrendr, CCDC, BFAST or CODED first."
                                             : "No map of this kind yet: run a tool first.");
    return changed;
}

void App::drawClassSeries(const SeriesStats& st) {
    if (!s_ || hover_.x < 0) return;
    const CubeInfo& info = *s_->info;
    for (auto it = results_.rbegin(); it != results_.rend(); ++it) { // the topmost first
        const ResultLayer& r = *it;
        if (r.cubeId != info.id || !r.visible || r.classSeries.empty()) continue;
        const float v = r.valueAt(hover_.x, hover_.y);
        const int k = std::isnan(v) ? 0 : int(std::lround(v));
        if (k < 1 || k > int(r.classSeries.size())) continue;
        const ResultLayer::ClassSeries& c = r.classSeries[k - 1];
        const int n = int(std::min(c.x.size(), c.y.size()));
        if (n == 0) continue;
        std::vector<double> x(n), y(n);
        for (int i = 0; i < n; ++i) {
            x[i] = info.xFromDecimalYear(c.x[i]);
            y[i] = plotValues_ == 1 ? c.y[i] - st.mean
                   : plotValues_ == 2 ? (st.std > 0 ? (c.y[i] - st.mean) / st.std : 0.0) : c.y[i];
        }
        const std::string name = k <= int(r.classes.size()) ? r.classes[k - 1] : "class " + std::to_string(k);
        const std::string label = name + ": typical series###classseries";
        // The class colour, dashed-looking over a dark outline: a model, not data.
        const ImVec4 col = theme::onPlot(ImPlot::GetColormapColor((k - 1) % ImPlot::GetColormapSize(r.cmap), r.cmap));
        ImPlotSpec under;
        under.LineColor = ImVec4(0.05f, 0.05f, 0.08f, 0.9f);
        under.LineWeight = 5.0f;
        ImPlot::PlotLine(label.c_str(), x.data(), y.data(), n, under);
        ImPlotSpec spec;
        spec.LineColor = col;
        spec.LineWeight = 2.5f;
        ImPlot::PlotLine(label.c_str(), x.data(), y.data(), n, spec);
        return; // one map's only
    }
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
    if (!why.empty()) ImGui::TextColored(theme::error(), "Not available for this series: %s", why.c_str());

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
        } else if (p.type == "patterns") {
            changed |= uiPatterns(p, v);
        } else if (p.type == "layers") {
            changed |= uiLayersParam(p, v);
        }
        if (!p.help.empty() && p.type != "patterns" && p.type != "layers") ImGui::SetItemTooltip("%s", p.help.c_str());
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

    if (tool.raster && s_ && tool.layerInput) {
        ImGui::SeparatorText("Run");
        size_t maps = 0;
        for (const ZeitParam& p : tool.params)
            if (p.type == "layers") maps = std::max(maps, ui.params.value(p.id, json::array()).size());
        ImGui::TextDisabled("On the grid of the first map checked; the results go under its series.");
        ImGui::TextDisabled("Output: %s", resultsDir_.c_str());
        ImGui::SameLine();
        if (ImGui::SmallButton("Change...")) {
            const std::string dir = platform::openFolderDialog();
            if (!dir.empty()) resultsDir_ = dir;
        }
        ImGui::BeginDisabled(maps < 2 || zeit_->state() != ZeitClient::State::Ready);
        if (ImGui::Button("Run", ImVec2(-1, 0))) runTool(tool);
        ImGui::EndDisabled();
        if (maps < 2) ImGui::TextDisabled("Check at least two maps above.");
    } else if (tool.raster && s_) {
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
        if (why.empty()) {
            updateEstimate(tool, ui);
            int win[4];
            const char* scopeName = nullptr;
            toolWindow(ui, win, &scopeName);
            const double px = double(win[2] - win[0]) * (win[3] - win[1]);
            const double sec = ui.estimate.is_object() ? estimateJobSeconds(ui.estimate, win[2] - win[0], win[3] - win[1]) : -1;
            if (sec >= 0)
                ImGui::Text("Estimated: %s for %.3g M px", formatDuration(sec).c_str(), px / 1e6);
            else if (!ui.estError.empty())
                ImGui::TextDisabled("No estimate: %s", ui.estError.c_str());
            else
                ImGui::TextDisabled("Estimating the run time...");
            ImGui::SetItemTooltip("Computing time measured on a sample of the series, using every core.\n"
                                  "Reading the data adds to it, mostly on spinning disks.");
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
        const float bw = ImGui::GetFontSize() * 4.5f, gap = ImGui::GetStyle().ItemSpacing.x; // follows the font size
        ImGui::ProgressBar(float(j.progress), ImVec2(-(3 * bw + 2 * gap), 0), overlay);
        ImGui::SameLine();
        if (ImGui::Button("Log", ImVec2(bw, 0))) showZeitLog(j.logPath);
        ImGui::SetItemTooltip("%s", j.logPath.c_str());
        ImGui::SameLine();
        if (st == ZeitJob::State::Running) {
            if (ImGui::Button("Cancel", ImVec2(-1, 0))) j.cancel();
        } else {
            if (ImGui::Button("Folder", ImVec2(bw, 0))) platform::openInExplorer(j.outputDir);
            ImGui::SameLine();
            if (ImGui::Button("Remove", ImVec2(-1, 0))) remove = int(i);
        }
        if (!msg.empty() && st == ZeitJob::State::Running) ImGui::TextDisabled("%s", msg.c_str());
        if (!err.empty()) {
            ImGui::PushTextWrapPos(0);
            ImGui::TextColored(theme::error(), "%s", err.c_str());
            ImGui::PopTextWrapPos();
        }
        ImGui::Separator();
        ImGui::PopID();
    }
    if (remove >= 0) {
        jobCube_.erase(jobs_[remove].get());
        jobs_.erase(jobs_.begin() + remove);
    }
    if (ImGui::SmallButton("Zeit process log")) showZeitLog(zeitLogPath());
    ImGui::SetItemTooltip("zeit.log: the process that fits the chart and estimates run times\n"
                          "(warnings and errors of the pixel runs), and a line per job");
    ImGui::End();
}

std::string App::zeitLogPath() const { return zeit_ ? zeit_->config().logPath : zeitConfig().logPath; }

void App::showZeitLog(const std::string& path) {
    zeitLog_.path = path;
    showZeitLog_ = true;
    ImGui::SetWindowFocus("Log");
}

void App::uiZeitLog() {
    if (!showZeitLog_) return;
    ImGui::SetNextWindowSize(ImVec2(720, 400), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Log", &showZeitLog_)) {
        ImGui::End();
        return;
    }
    LogView& v = zeitLog_;
    // What can be shown: every job's log (newest first), then zeit.log.
    std::vector<std::pair<std::string, std::string>> sources; // title, path
    for (auto it = jobs_.rbegin(); it != jobs_.rend(); ++it)
        if (!(*it)->logPath.empty()) sources.emplace_back((*it)->title, (*it)->logPath);
    const std::string zlog = zeitLogPath();
    sources.emplace_back("Zeit process (zeit.log)", zlog);
    const std::string path = v.path.empty() ? sources.front().second : v.path;
    std::string preview = fs::u8path(path).filename().u8string();
    for (const auto& [title, p] : sources)
        if (p == path) preview = title;

    // The file, followed: its end at first, then what it gains (looked at 4 times a second).
    const double now = ImGui::GetTime();
    std::error_code ec;
    if (path != v.shown || now - v.checked >= 0.25) {
        v.checked = now;
        const uintmax_t size = fs::exists(fs::u8path(path), ec) ? fs::file_size(fs::u8path(path), ec) : 0;
        constexpr uintmax_t kKeep = 2u << 20; // bytes kept (the end of a long zeit.log)
        if (path != v.shown || size < v.size) { // another file, or this one started again
            v.shown = path;
            v.text.clear();
            v.lines.assign(1, 0);
            v.size = size > kKeep ? size - kKeep : 0;
            v.cut = v.size > 0;
        }
        if (size > v.size) {
            std::ifstream f(fs::u8path(path), std::ios::binary);
            f.seekg(std::streamoff(v.size));
            std::string add(size_t(size - v.size), '\0');
            f.read(add.data(), std::streamsize(add.size()));
            add.resize(size_t(f.gcount()));
            v.size += add.size();
            add.erase(std::remove(add.begin(), add.end(), '\r'), add.end());
            size_t from = v.text.size(); // lines: where each starts (the last may be the empty one after a '\n')
            v.text += add;
            if (v.text.size() > 2 * kKeep) { // keep the end only, from a line start
                const size_t nl = v.text.find('\n', v.text.size() - kKeep);
                v.text.erase(0, nl == std::string::npos ? v.text.size() - kKeep : nl + 1);
                v.cut = true;
                v.lines.assign(1, 0);
                from = 0;
            }
            for (size_t k = from; k < v.text.size(); ++k)
                if (v.text[k] == '\n') v.lines.push_back(k + 1);
        }
    }
    const int nLines = v.lines.empty() ? 0 : int(v.lines.size()) - (v.lines.back() == v.text.size() ? 1 : 0);

    ImGui::SetNextItemWidth(ImGui::GetFontSize() * 26);
    if (ImGui::BeginCombo("##source", preview.c_str())) {
        for (const auto& [title, p] : sources) {
            ImGui::PushID(p.c_str());
            if (ImGui::Selectable(title.c_str(), p == path)) v.path = p;
            ImGui::SetItemTooltip("%s", p.c_str());
            ImGui::PopID();
        }
        ImGui::EndCombo();
    }
    ImGui::SameLine();
    if (ImGui::Button("Copy")) ImGui::SetClipboardText(v.text.c_str());
    ImGui::SetItemTooltip("Copies the log shown");
    ImGui::SameLine();
    if (ImGui::Button("Open file")) platform::openInExplorer(path);
    ImGui::SameLine();
    if (ImGui::Button("Folder")) platform::openInExplorer(fs::u8path(path).parent_path().u8string());
    ImGui::TextDisabled("%s%s", path.c_str(), v.cut ? "  (its end)" : "");

    ImGui::BeginChild("##text", ImVec2(0, 0), ImGuiChildFlags_Borders, ImGuiWindowFlags_HorizontalScrollbar);
    if (v.text.empty()) ImGui::TextDisabled(fs::exists(fs::u8path(path), ec) ? "(empty)" : "(no log yet)");
    // Tracebacks and errors in the error colour, warnings in the warning's,
    // the lines Janus writes at a start ("=== date ...") in the accent.
    ImGuiListClipper clipper;
    clipper.Begin(nLines);
    while (clipper.Step())
        for (int i = clipper.DisplayStart; i < clipper.DisplayEnd; ++i) {
            const char* b = v.text.data() + v.lines[i];
            const char* e = v.text.data() + (i + 1 < int(v.lines.size()) ? v.lines[i + 1] - 1 : v.text.size());
            const std::string_view line(b, size_t(e - b));
            auto has = [&](const char* s) { return line.find(s) != std::string_view::npos; };
            const bool err = has("Traceback") || has("Error") || has("Exception") || has("error:") || has("] failed");
            const bool warn = !err && (has("Warning") || has("warning:"));
            if (err || warn || line.rfind("=== ", 0) == 0)
                ImGui::PushStyleColor(ImGuiCol_Text, err ? theme::error() : warn ? theme::warning() : theme::accent());
            ImGui::TextUnformatted(b, e);
            if (err || warn || line.rfind("=== ", 0) == 0) ImGui::PopStyleColor();
        }
    if (ImGui::GetScrollY() >= ImGui::GetScrollMaxY()) ImGui::SetScrollHereY(1.0f); // follows the end, unless scrolled up
    ImGui::EndChild();
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
        ImGui::SetItemTooltip("%s\nRight click: save it, or every output of its run, as GeoTIFF", r.path.c_str());
        if (ImGui::BeginPopupContextItem("result")) {
            uiResultExportMenu(r, "Save as GeoTIFF...");
            const size_t n = runOutputs(r).size();
            if (ImGui::MenuItem(("Save all " + std::to_string(n) + " outputs of this run...").c_str())) openSaveRun(r);
            ImGui::EndPopup();
        }
        ImGui::SameLine(ImGui::GetContentRegionAvail().x + ImGui::GetCursorPosX() - 18);
        if (ImGui::SmallButton("x")) remove = int(i);
        ImGui::SetItemTooltip("Remove this layer (the file stays on disk)");
        if (r.visible && !r.classes.empty()) {
            // Legend of a class map.
            ImGui::Indent();
            const int n = ImPlot::GetColormapSize(r.cmap);
            for (size_t k = 0; k < r.classes.size(); ++k) {
                const ImVec4 c = ImPlot::GetColormapColor(int(k) % n, r.cmap);
                ImGui::ColorButton(("##c" + std::to_string(k)).c_str(), c,
                                   ImGuiColorEditFlags_NoTooltip | ImGuiColorEditFlags_NoBorder, ImVec2(12, 12));
                ImGui::SameLine();
                ImGui::TextUnformatted(r.classes[k].c_str());
            }
            ImGui::SetNextItemWidth(-1);
            if (ImGui::SliderFloat("##opacity", &r.opacity, 0.05f, 1.0f, "opacity %.2f")) mapDirty_ = true;
            ImGui::Unindent();
        } else if (r.visible) {
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
        Gpu::deleteTexture(results_[remove].tex);
        results_.erase(results_.begin() + remove);
        mapDirty_ = true;
    }
}

void App::clearResults(uint64_t cubeId) {
    for (auto it = results_.begin(); it != results_.end();) {
        if (cubeId == 0 || it->cubeId == cubeId) {
            Gpu::deleteTexture(it->tex);
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

// Draws the model returned by a pixel run (same legend entry as the series, so
// hiding a series hides its model). Every model has the same colour, used by no
// series, with a dark outline: a fit is never mistaken for data.
void App::drawZeitOverlays(const char* label, const json& result, const SeriesStats& st) {
    if (!result.is_object() || !s_) return;
    const CubeInfo& info = *s_->info;
    const ImVec4 col = theme::onPlot(ImVec4(0.95f, 0.15f, 0.95f, 1)); // magenta
    const ImVec4 outline(0.05f, 0.05f, 0.08f, 0.9f);
    for (const json& o : result.value("overlays", json::array())) {
        const json xs = o.value("x", json::array()), ys = o.value("y", json::array());
        if (o.value("type", "") == "vlines") { // e.g. break dates: dashed-looking thin full-height lines
            std::vector<double> x;
            for (const json& v : xs)
                if (v.is_number()) x.push_back(info.xFromDecimalYear(v.get<double>()));
            if (x.empty()) continue;
            ImPlotSpec spec;
            spec.LineColor = outline;
            spec.LineWeight = 3.5f;
            ImPlot::PlotInfLines(label, x.data(), int(x.size()), spec);
            spec.LineColor = ImVec4(col.x, col.y, col.z, 0.85f);
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
        spec.LineColor = col;
        spec.LineWeight = 2.5f;
        if (o.value("type", "") == "markers") {
            spec.Marker = ImPlotMarker_Square;
            spec.MarkerSize = 4.0f;
            spec.MarkerFillColor = col;
            spec.MarkerLineColor = ImVec4(0, 0, 0, 1);
            ImPlot::PlotScatter(label, x.data(), y.data(), n, spec);
        } else {
            ImPlotSpec under = spec; // outline: the same line, wider and dark, below
            under.LineColor = outline;
            under.LineWeight = spec.LineWeight + 2.5f;
            ImPlot::PlotLine(label, x.data(), y.data(), n, under);
            ImPlot::PlotLine(label, x.data(), y.data(), n, spec);
        }
    }
}
