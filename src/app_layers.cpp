// Layers: several series open at once, the Layers and Files panels, and the
// series of the non-active layers under the cursor and the pins.
//
// Map space is the active layer's pixel grid. Other layers are placed through
// their geotransforms (same CRS, no rotation); the active layer's display state
// lives in the App members (mode_, cmap_, range_...) and is swapped in/out of
// LayerDisplay when the active layer changes.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <filesystem>

#include <implot.h>

#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

bool sameCrs(const CubeInfo& a, const CubeInfo& b) {
    if (!a.crsAuthority.empty() || !b.crsAuthority.empty()) return a.crsAuthority == b.crsAuthority;
    return a.crsName == b.crsName;
}

int nearestTime(const std::vector<Layer>& layers, double time) {
    int best = 0;
    for (int i = 1; i < int(layers.size()); ++i)
        if (std::fabs(layers[i].time - time) < std::fabs(layers[best].time - time)) best = i;
    return best;
}

} // namespace

const App::SeriesLayer* App::activeLayer() const {
    return active_ >= 0 && active_ < int(layers_.size()) ? &layers_[active_] : nullptr;
}

std::string App::layerName(const CubeInfo& info, const std::vector<std::string>& inputs) const {
    fs::path p = fs::u8path(inputs.size() == 1 ? inputs[0] : info.firstPath);
    if (inputs.size() != 1) p = p.parent_path();
    std::string n = p.has_stem() ? p.stem().u8string() : p.filename().u8string();
    if (n.empty()) n = p.u8string();
    // Disambiguate equal names (e.g. two folders called "ndvi").
    int dup = 0;
    for (const SeriesLayer& L : layers_)
        if (L.name == n || L.name.rfind(n + " (", 0) == 0) ++dup;
    return dup ? n + " (" + std::to_string(dup + 1) + ")" : n;
}

void App::saveDisplay(LayerDisplay& d) const {
    d.mode = mode_;
    d.rgb = rgb_;
    d.cmap = cmap_;
    d.range = range_;
    d.perDateRange = perDateRange_;
    d.t = t_;
}

void App::loadDisplay(const LayerDisplay& d) {
    mode_ = d.mode;
    rgb_ = d.rgb;
    cmap_ = d.cmap;
    range_ = d.range;
    perDateRange_ = d.perDateRange;
    t_ = d.t;
    histKey_ = ~0ull;
    appliedCmap_ = -1;
}

// Pixel of layer L -> pixel of the active layer.
bool App::toActive(const SeriesLayer& L, double lx, double ly, double& x, double& y) const {
    if (!L.aligned) return false;
    x = L.ax + L.bx * lx;
    y = L.ay + L.by * ly;
    return true;
}

// Pixel of the active layer -> pixel of layer L (false outside it).
bool App::fromActive(const SeriesLayer& L, double x, double y, int& lx, int& ly) const {
    if (!L.aligned || L.bx == 0 || L.by == 0) return false;
    lx = int(std::floor((x - L.ax) / L.bx));
    ly = int(std::floor((y - L.ay) / L.by));
    const CubeInfo& info = *L.session->info;
    return lx >= 0 && ly >= 0 && lx < info.width && ly < info.height;
}

void App::updateAlignment() {
    const SeriesLayer* A = activeLayer();
    if (!A) return;
    const CubeInfo& a = *A->session->info;
    for (SeriesLayer& L : layers_) {
        const CubeInfo& b = *L.session->info;
        L.alignNote.clear();
        if (&L == A) {
            L.ax = L.ay = 0;
            L.bx = L.by = 1;
            L.aligned = true;
            continue;
        }
        if (!a.hasGeoTransform || !b.hasGeoTransform) {
            // Without georeferencing, only identical grids can be overlaid.
            L.aligned = a.width == b.width && a.height == b.height;
            L.ax = L.ay = 0;
            L.bx = L.by = 1;
            if (!L.aligned) L.alignNote = "no georeferencing and a different size: shown only when active";
            continue;
        }
        const auto& ga = a.geoTransform;
        const auto& gb = b.geoTransform;
        if (ga[2] != 0 || ga[4] != 0 || gb[2] != 0 || gb[4] != 0) {
            L.aligned = false;
            L.alignNote = "rotated grid: shown only when active";
            continue;
        }
        if (!sameCrs(a, b)) {
            L.aligned = false;
            L.alignNote = "different CRS (" + (b.crsAuthority.empty() ? b.crsName : b.crsAuthority) +
                          "): shown only when active";
            continue;
        }
        L.aligned = true;
        L.bx = gb[1] / ga[1];
        L.by = gb[5] / ga[5];
        L.ax = (gb[0] - ga[0]) / ga[1];
        L.ay = (gb[3] - ga[3]) / ga[5];
    }
}

// Every non-active layer shows the date nearest to the active layer's date.
void App::syncLayerTimes() {
    const SeriesLayer* A = activeLayer();
    if (!A) return;
    const CubeInfo& a = *A->session->info;
    for (SeriesLayer& L : layers_) {
        if (&L == A) continue;
        const CubeInfo& b = *L.session->info;
        const int t = (a.timeIsDate && b.timeIsDate) ? nearestTime(b.layers, a.layers[t_].time)
                                                     : std::min(t_, b.T() - 1);
        if (t != L.disp.t) {
            L.disp.t = t;
            mapDirty_ = true;
        }
        L.session->overview.setFocus(L.disp.t);
    }
}

void App::setActive(int i) {
    if (i < 0 || i >= int(layers_.size())) return;
    SeriesLayer* prev = active_ >= 0 && active_ < int(layers_.size()) ? &layers_[active_] : nullptr;
    // Keep the same geographic view when switching between aligned layers.
    double geoX = 0, geoY = 0, geoPerPx = 0;
    bool keepView = false;
    if (prev && prev != &layers_[i] && prev->session->info->hasGeoTransform && viewTouched_) {
        const auto& g = prev->session->info->geoTransform;
        geoX = g[0] + (-offset_.x / scale_) * g[1];
        geoY = g[3] + (-offset_.y / scale_) * g[5];
        geoPerPx = g[1] / scale_;
        keepView = true;
    }
    // Pins live in map space: carry them over through geographic coordinates.
    std::vector<std::pair<double, double>> pinGeo;
    if (prev && prev->session->info->hasGeoTransform)
        for (const SeriesView& p : pins_) {
            double gx, gy;
            prev->session->info->pixelToGeo(p.x + 0.5, p.y + 0.5, gx, gy);
            pinGeo.push_back({gx, gy});
        }
    if (prev) saveDisplay(prev->disp);

    active_ = i;
    SeriesLayer& L = layers_[i];
    s_ = L.session.get();
    const CubeInfo& info = *s_->info;
    loadDisplay(L.disp);
    lastInputs_ = L.inputs;
    const int T = info.T();
    xs_.resize(T);
    years_.resize(T);
    zeitYears_.resize(T);
    for (int t = 0; t < T; ++t) {
        xs_[t] = info.layers[t].time;
        years_[t] = info.yearsFromStart(t);
        zeitYears_[t] = info.decimalYear(t);
    }
    if (!modeAvailable(mode_)) mode_ = ModeValue;
    updateAlignment();

    // View
    if (keepView && info.hasGeoTransform && prev->aligned) {
        const auto& g = info.geoTransform;
        scale_ = g[1] / geoPerPx;
        offset_ = ImVec2(float(-(geoX - g[0]) / g[1] * scale_), float(-(geoY - g[3]) / g[5] * scale_));
    } else {
        fitRequested_ = true;
        viewTouched_ = false;
    }

    // Pins: remap to the new grid (drop the ones outside it), then re-read.
    std::vector<SeriesView> oldPins = std::move(pins_);
    pins_.clear();
    for (size_t k = 0; k < oldPins.size(); ++k) {
        int px = oldPins[k].x, py = oldPins[k].y;
        if (k < pinGeo.size() && info.hasGeoTransform) {
            const auto& g = info.geoTransform;
            px = int(std::floor((pinGeo[k].first - g[0]) / g[1]));
            py = int(std::floor((pinGeo[k].second - g[3]) / g[5]));
        }
        if (px < 0 || py < 0 || px >= info.width || py >= info.height) continue;
        SeriesView p;
        p.id = oldPins[k].id;
        p.color = oldPins[k].color;
        p.x = px;
        p.y = py;
        p.values = approxSeries(px, py);
        p.stats = computeSeriesStats(years_, p.values);
        p.request = s_->deferRandomReads() ? 0 : s_->requestSeries(px, py, false);
        pins_.push_back(std::move(p));
    }
    for (SeriesLayer& O : layers_) {
        O.pins.clear();
        O.hover = SeriesView{};
    }
    for (const SeriesView& p : pins_) addOtherPins(p);

    // Cursor and ROI belong to the previous grid.
    hover_ = SeriesView{};
    hoverPending_ = false;
    approxLayers_ = -1;
    clearRoi();
    roiZeitReq_ = 0;
    roiZeitVersion_ = -1;
    roiZeitResult_ = json();
    syncLayerTimes();
    mapDirty_ = true;

    const std::string title = "tsv - " + L.name + " (" + info.description + ")" +
                              (layers_.size() > 1 ? "  [" + std::to_string(layers_.size()) + " layers]" : "");
    glfwSetWindowTitle(window_, title.c_str());
}

void App::removeLayer(int i) {
    if (i < 0 || i >= int(layers_.size())) return;
    clearResults(layers_[i].session->info->id);
    const bool wasActive = i == active_;
    if (wasActive) {
        clearRoi();
        s_ = nullptr;
    }
    layers_.erase(layers_.begin() + i);
    if (layers_.empty()) {
        active_ = -1;
        pins_.clear();
        hover_ = SeriesView{};
        glfwSetWindowTitle(window_, "tsv");
        return;
    }
    if (wasActive) {
        active_ = -1; // nothing to save: the removed layer's state is gone
        setActive(std::min(i, int(layers_.size()) - 1));
    } else {
        if (i < active_) --active_;
        updateAlignment();
        mapDirty_ = true;
    }
}

void App::closeAll() {
    clearResults();
    clearRoi();
    s_ = nullptr;
    layers_.clear();
    active_ = -1;
    pins_.clear();
    hover_ = SeriesView{};
    glfwSetWindowTitle(window_, "tsv");
}

// ---------------------------------------------------------------------------
// Series of the other layers (cursor and pins)
// ---------------------------------------------------------------------------

std::vector<float> App::approxSeriesOf(const Session& s, int x, int y) const {
    const Overview& ov = s.overview;
    const int ox = std::min(ov.w - 1, int(double(x) * ov.w / s.info->width));
    const int oy = std::min(ov.h - 1, int(double(y) * ov.h / s.info->height));
    std::vector<float> v(size_t(ov.T), NAN);
    for (int t = 0; t < ov.T; ++t)
        if (s.gpu.loaded[t]) v[t] = ov.at(t, ox, oy);
    return v;
}

void App::updateOtherHover(int ix, int iy) {
    if (chartLayers_ == 0) return;
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        int lx, ly;
        if (!L.visible || !fromActive(L, ix + 0.5, iy + 0.5, lx, ly)) {
            L.hover = SeriesView{};
            continue;
        }
        if (lx == L.hover.x && ly == L.hover.y) continue;
        L.hover = SeriesView{};
        L.hover.x = lx;
        L.hover.y = ly;
        L.hover.values = approxSeriesOf(*L.session, lx, ly);
        L.hover.stats = computeSeriesStats(L.years, L.hover.values);
        L.hover.color = ImVec4(0.95f, 0.95f, 0.95f, 1);
    }
}

// Exact reads for the other layers (after the same debounce as the active one).
void App::requestOtherSeries(bool hover) {
    if (chartLayers_ == 0) return;
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer() || !L.visible || L.session->deferRandomReads()) continue;
        if (hover && L.hover.x >= 0 && !L.hover.exact)
            L.hover.request = L.session->requestSeries(L.hover.x, L.hover.y, true);
        for (SeriesView& p : L.pins)
            if (!p.exact && p.request == 0) p.request = L.session->requestSeries(p.x, p.y, false);
    }
}

void App::pumpOtherSeries() {
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        if (L.years.empty()) {
            L.years.resize(L.session->info->T());
            for (int t = 0; t < L.session->info->T(); ++t) L.years[t] = L.session->info->yearsFromStart(t);
        }
        for (SeriesResult& r : L.session->takeSeries()) {
            auto apply = [&](SeriesView& v) {
                if (v.request != r.id) return false;
                v.values = std::move(r.values);
                v.exact = true;
                v.stats = computeSeriesStats(L.years, v.values);
                return true;
            };
            if (apply(L.hover)) continue;
            for (SeriesView& p : L.pins)
                if (apply(p)) break;
        }
    }
}

void App::addOtherPins(const SeriesView& pin) {
    for (SeriesLayer& L : layers_) {
        if (&L == activeLayer()) continue;
        if (L.years.empty()) {
            L.years.resize(L.session->info->T());
            for (int t = 0; t < L.session->info->T(); ++t) L.years[t] = L.session->info->yearsFromStart(t);
        }
        int lx, ly;
        if (!fromActive(L, pin.x + 0.5, pin.y + 0.5, lx, ly)) continue;
        SeriesView p;
        p.id = pin.id;
        p.color = pin.color;
        p.x = lx;
        p.y = ly;
        p.values = approxSeriesOf(*L.session, lx, ly);
        p.stats = computeSeriesStats(L.years, p.values);
        L.pins.push_back(std::move(p));
    }
    requestOtherSeries(false);
}

void App::removePinById(int id) {
    pins_.erase(std::remove_if(pins_.begin(), pins_.end(), [id](const SeriesView& p) { return p.id == id; }),
                pins_.end());
    for (SeriesLayer& L : layers_)
        L.pins.erase(std::remove_if(L.pins.begin(), L.pins.end(), [id](const SeriesView& p) { return p.id == id; }),
                     L.pins.end());
}

void App::clearPins() {
    pins_.clear();
    for (SeriesLayer& L : layers_) L.pins.clear();
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

void App::uiLayers() {
    if (!ImGui::Begin("Layers")) {
        ImGui::End();
        return;
    }
    if (layers_.empty()) {
        ImGui::TextDisabled("No layers. Open a series (File, Files panel,\ndrag and drop or the command line).");
        ImGui::End();
        return;
    }
    if (ImGui::Button("Add layer...")) {
        auto files = platform::openFilesDialog();
        if (!files.empty()) openInputs(files, true);
    }
    ImGui::SetItemTooltip("Open another series as a new layer (files)");
    ImGui::SameLine();
    if (ImGui::Button("Add folder...")) {
        const std::string dir = platform::openFolderDialog();
        if (!dir.empty()) openInputs({dir}, true);
    }
    ImGui::SetItemTooltip("Open another series as a new layer (a folder, 1 file per date)");
    ImGui::TextDisabled("Top of the list = drawn on top. Click a name to make it active.");
    ImGui::Separator();

    int moveUp = -1, moveDown = -1, remove = -1, activate = -1;
    // Listed top-down: the last layer is drawn last (on top).
    for (int i = int(layers_.size()) - 1; i >= 0; --i) {
        SeriesLayer& L = layers_[i];
        ImGui::PushID(i);
        if (ImGui::Checkbox("##vis", &L.visible)) mapDirty_ = true;
        ImGui::SetItemTooltip(L.visible ? "Hide this layer" : "Show this layer");
        ImGui::SameLine();
        const bool isActive = i == active_;
        ImGui::PushStyleColor(ImGuiCol_Text, isActive ? ImVec4(0.55f, 0.80f, 1.0f, 1) : ImGui::GetStyleColorVec4(ImGuiCol_Text));
        const std::string label = (isActive ? "> " : "  ") + L.name;
        if (ImGui::Selectable(label.c_str(), isActive, 0, ImVec2(ImGui::GetContentRegionAvail().x - 76, 0)) && !isActive)
            activate = i;
        ImGui::PopStyleColor();
        const CubeInfo& info = *L.session->info;
        ImGui::SetItemTooltip("%s\n%s, %d x %d px, %d dates (%s .. %s)\nCRS: %s", L.inputs.size() == 1 ? L.inputs[0].c_str() : info.firstPath.c_str(),
                              info.description.c_str(), info.width, info.height, info.T(),
                              info.layers.front().label.c_str(), info.layers.back().label.c_str(),
                              info.crsAuthority.empty() ? info.crsName.c_str() : info.crsAuthority.c_str());
        ImGui::SameLine();
        ImGui::BeginDisabled(i == int(layers_.size()) - 1);
        if (ImGui::ArrowButton("##up", ImGuiDir_Up)) moveUp = i;
        ImGui::EndDisabled();
        ImGui::SameLine();
        ImGui::BeginDisabled(i == 0);
        if (ImGui::ArrowButton("##down", ImGuiDir_Down)) moveDown = i;
        ImGui::EndDisabled();
        ImGui::SameLine();
        if (ImGui::SmallButton("x")) remove = i;
        ImGui::SetItemTooltip("Close this layer");

        ImGui::Indent();
        if (!isActive) {
            ImGui::TextDisabled("date %s", info.layers[L.disp.t].label.c_str());
            if (!L.session->overview.complete())
                ImGui::TextDisabled("loading %d/%d", L.session->overview.layersDone(), info.T());
        }
        if (!L.alignNote.empty()) ImGui::TextColored(ImVec4(1.0f, 0.7f, 0.4f, 1), "%s", L.alignNote.c_str());
        ImGui::SetNextItemWidth(-1);
        if (ImGui::SliderFloat("##opacity", &L.opacity, 0.05f, 1.0f, "opacity %.2f")) mapDirty_ = true;
        uiResultsOf(info.id);
        ImGui::Unindent();
        ImGui::Separator();
        ImGui::PopID();
    }
    if (moveUp >= 0) {
        std::swap(layers_[moveUp], layers_[moveUp + 1]);
        if (active_ == moveUp) active_ = moveUp + 1;
        else if (active_ == moveUp + 1) active_ = moveUp;
        mapDirty_ = true;
    }
    if (moveDown >= 0) {
        std::swap(layers_[moveDown], layers_[moveDown - 1]);
        if (active_ == moveDown) active_ = moveDown - 1;
        else if (active_ == moveDown - 1) active_ = moveDown;
        mapDirty_ = true;
    }
    if (activate >= 0) setActive(activate);
    if (remove >= 0) removeLayer(remove);
    ImGui::End();
}

void App::uiFiles() {
    if (!ImGui::Begin("Files")) {
        ImGui::End();
        return;
    }
    const FileBrowser::Action act = files_.draw();
    if (act.kind != FileBrowser::Action::None && !act.paths.empty())
        openInputs(act.paths, act.kind == FileBrowser::Action::AddLayer && !layers_.empty());
    ImGui::End();
}
