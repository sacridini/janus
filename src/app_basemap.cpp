// Basemap in the interface: its section of the Layers panel (source, on/off,
// opacity, a custom XYZ URL), the setting (kept in the layout .ini, but not
// on/off: each session and each series opened start without it, drawn only
// when asked for), the
// Basemap object's life (made after the first frame, only once a source is
// picked and a series is open; a source change retires the old one in the
// background, since a request may still be running), drawing it first in
// every map target and its attribution on the map.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>

#include <imgui_internal.h>

#include "glfw.hpp"

namespace fs = std::filesystem;

namespace {

// "Tiles: host" for a custom source without an attribution of its own.
std::string hostOf(const std::string& url) {
    const size_t a = url.find("://");
    if (a == std::string::npos) return url;
    const size_t b = url.find_first_of("/?", a + 3);
    return url.substr(a + 3, b == std::string::npos ? std::string::npos : b - (a + 3));
}

bool sameSource(const BasemapSource& a, const BasemapSource& b) {
    return a.url == b.url && a.maxZoom == b.maxZoom && a.tileSize == b.tileSize && a.attribution == b.attribution;
}

} // namespace

void App::registerBasemapSettings() {
    ImGuiSettingsHandler h;
    h.TypeName = "JanusBasemap";
    h.TypeHash = ImHashStr("JanusBasemap");
    h.UserData = this;
    h.ReadOpenFn = [](ImGuiContext*, ImGuiSettingsHandler*, const char* name) -> void* {
        return std::strcmp(name, "Settings") == 0 ? reinterpret_cast<void*>(1) : nullptr;
    };
    h.ReadLineFn = [](ImGuiContext*, ImGuiSettingsHandler* handler, void*, const char* line) {
        App& app = *static_cast<App*>(handler->UserData);
        BasemapUi& b = app.bm_;
        auto value = [line](const char* key) -> const char* {
            const size_t n = std::strlen(key);
            return std::strncmp(line, key, n) == 0 ? line + n : nullptr;
        };
        if (const char* v = value("Source=")) b.source = v;
        else if (const char* v = value("Opacity=")) b.opacity = std::clamp(float(std::atof(v)), 0.05f, 1.0f);
        else if (const char* v = value("Url=")) {
            b.url = v;
            std::snprintf(app.bmUrlEdit_, sizeof(app.bmUrlEdit_), "%s", v);
        }
        else if (const char* v = value("MaxZoom=")) b.maxZoom = std::clamp(std::atoi(v), 0, 22);
        else if (const char* v = value("TileSize=")) b.tileSize = std::atoi(v) == 512 ? 512 : 256;
        else if (const char* v = value("Attribution=")) {
            b.attribution = v;
            std::snprintf(app.bmAttrEdit_, sizeof(app.bmAttrEdit_), "%s", v);
        }
    };
    h.WriteAllFn = [](ImGuiContext*, ImGuiSettingsHandler* handler, ImGuiTextBuffer* buf) {
        const BasemapUi& b = static_cast<App*>(handler->UserData)->bm_;
        buf->appendf("[JanusBasemap][Settings]\nSource=%s\nOpacity=%.2f\nUrl=%s\nMaxZoom=%d\nTileSize=%d\n"
                     "Attribution=%s\n\n",
                     b.source.c_str(), b.opacity, b.url.c_str(), b.maxZoom, b.tileSize,
                     b.attribution.c_str());
    };
    ImGui::AddSettingsHandler(&h);
}

bool App::basemapCustomOk(std::string* why) const {
    const std::string problem = basemapUrlProblem(bm_.url);
    if (why) *why = problem;
    return problem.empty();
}

BasemapSource App::basemapSource() const {
    for (const BasemapSource& p : basemapPresets())
        if (p.id == bm_.source) return p;
    BasemapSource s;
    s.id = "custom";
    s.name = "Custom XYZ";
    s.url = bm_.url;
    s.maxZoom = bm_.maxZoom;
    s.tileSize = bm_.tileSize;
    s.attribution = bm_.attribution.empty() ? "Tiles: " + hostOf(bm_.url) : bm_.attribution;
    s.terms = "The provider's terms (a key in the URL is sent with every request)";
    s.connections = 4;
    s.cacheDays = 7;
    return s;
}

bool App::basemapShown() const { return bm_.on && bm_.source != "none" && basemap_ && basemap_->hasMap(); }

std::string App::basemapAttribution() const { return basemapShown() ? basemap_->source().attribution : ""; }

void App::retireBasemap() {
    if (!basemap_) return;
    basemap_->releaseGpu(); // textures go on this thread; the rest may wait for a request
    retiredBasemaps_.push_back(std::async(std::launch::async, [b = std::move(basemap_)]() mutable { b.reset(); }));
    retiredBasemaps_.erase(std::remove_if(retiredBasemaps_.begin(), retiredBasemaps_.end(),
                                          [](std::future<void>& f) {
                                              return f.wait_for(std::chrono::seconds(0)) == std::future_status::ready;
                                          }),
                           retiredBasemaps_.end());
    mapDirty_ = true;
}

// Once per frame. Nothing is made before the third frame (the window shows
// first, even when the setting asks for a basemap), nor without a series.
void App::pumpBasemap() {
    if (bm_.source == "none") {
        retireBasemap();
        basemapWhy_.clear();
        return;
    }
    if (!bm_.on || !s_ || ImGui::GetFrameCount() < 3) return; // off: nothing asked for, the tiles kept
    std::string why;
    if (bm_.source == "custom" && !basemapCustomOk(&why)) {
        retireBasemap();
        basemapWhy_ = why;
        return;
    }
    const BasemapSource src = basemapSource();
    if (basemap_ && !sameSource(basemap_->source(), src)) retireBasemap();
    if (!basemap_) {
        const std::string dir = (fs::u8path(settings_.cacheDir) / "basemap").u8string();
        basemap_ = std::make_unique<Basemap>(src, dir, [] { glfwPostEmptyEvent(); });
        basemapFailedFor_ = 0;
        mapDirty_ = true;
    }
    const CubeInfo& info = *s_->info;
    if (basemapFailedFor_ != info.id) {
        const bool had = basemap_->hasMap();
        if (basemap_->setMap(info, why)) {
            basemapWhy_.clear();
            if (!had) mapDirty_ = true;
        } else {
            basemapWhy_ = why;
            basemapFailedFor_ = info.id; // not tried again for this layer
        }
    }
    basemap_->tick();
    if (basemap_->uploadReady(16)) mapDirty_ = true;
}

void App::drawBasemap(const double view[4], double pxPerMapPx, const float bg[4], float targetW, float targetH,
                      const std::function<void(const double*, float*)>& toTarget) {
    if (!basemapShown()) return;
    bool any = false;
    basemap_->forEachVisible(view, pxPerMapPx, [&](GpuTex tex, const double* q, const WarpParams& w) {
        float r[4];
        toTarget(q, r);
        gpu_.drawImage(tex, r, 1.0f, &w);
        any = true;
    });
    // Opacity: the tiles (of several levels, overlapping) drawn opaque, then
    // faded towards the background, so each place is blended once.
    if (any && bm_.opacity < 1.0f) {
        const float c[4] = {bg[0], bg[1], bg[2], 1.0f - bm_.opacity}, all[4] = {0, 0, targetW, targetH};
        gpu_.fillRect(all, c);
    }
}

// Bottom right of a map canvas: the attribution, always, and a short state
// (offline, a server error) in grey; wrapped to the canvas' width.
void App::drawBasemapNote(ImDrawList* dl, ImVec2 origin, ImVec2 size) {
    if (!basemapShown()) return;
    std::string text = basemap_->source().attribution;
    if (basemap_->offline()) text = "Basemap unavailable (offline)  |  " + text;
    else if (const std::string e = basemap_->error(); !e.empty()) text = "Basemap: " + e + "  |  " + text;
    ImFont* font = ImGui::GetFont();
    const float fs = ImGui::GetFontSize() * 0.85f, pad = 3;
    const float wrap = std::max(80.0f, size.x - 260); // the colour bar is bottom left
    const ImVec2 ts = font->CalcTextSizeA(fs, FLT_MAX, wrap, text.c_str());
    const ImVec2 p = origin + size - ts - ImVec2(pad + 2, pad + 2);
    dl->AddRectFilled(p - ImVec2(pad, pad), p + ts + ImVec2(pad, pad), IM_COL32(0, 0, 0, 140), 2);
    dl->AddText(font, fs, p, IM_COL32(220, 220, 220, 255), text.c_str(), nullptr, wrap);
}

// Layers panel, below the layers (it is drawn under all of them).
void App::uiBasemap() {
    ImGui::PushID("basemap");
    ImGui::SeparatorText("Basemap");
    const auto& presets = basemapPresets();
    std::string current = "None";
    for (const BasemapSource& p : presets)
        if (p.id == bm_.source) current = p.name;
    if (bm_.source == "custom") current = "Custom XYZ URL";
    ImGui::BeginDisabled(bm_.source == "none");
    if (ImGui::Checkbox("##on", &bm_.on)) {
        mapDirty_ = true;
        ImGui::MarkIniSettingsDirty();
    }
    ImGui::EndDisabled();
    ImGui::SetItemTooltip(bm_.on ? "Hide the basemap" : "Show the basemap");
    ImGui::SameLine();
    ImGui::SetNextItemWidth(-1);
    if (ImGui::BeginCombo("##source", current.c_str())) {
        auto pick = [&](const std::string& id) {
            bm_.source = id;
            bm_.on = true;
            basemapFailedFor_ = 0;
            mapDirty_ = true;
            ImGui::MarkIniSettingsDirty();
        };
        if (ImGui::Selectable("None", bm_.source == "none")) pick("none");
        ImGui::SetItemTooltip("Nothing under the layers; nothing is downloaded");
        for (const BasemapSource& p : presets) {
            if (ImGui::Selectable(p.name.c_str(), bm_.source == p.id)) pick(p.id);
            ImGui::SetItemTooltip("%s\nUp to zoom %d. %s", p.attribution.c_str(), p.maxZoom, p.terms.c_str());
        }
        if (ImGui::Selectable("Custom XYZ URL", bm_.source == "custom")) {
            pick("custom");
            std::snprintf(bmUrlEdit_, sizeof(bmUrlEdit_), "%s", bm_.url.c_str());
            std::snprintf(bmAttrEdit_, sizeof(bmAttrEdit_), "%s", bm_.attribution.c_str());
        }
        ImGui::SetItemTooltip("Your own tile service, e.g. with a key (Google Map Tiles API, MapTiler,\n"
                              "Planet...): https://.../{z}/{x}/{y}.png?key=...");
        ImGui::EndCombo();
    }
    if (bm_.source == "none") {
        ImGui::TextDisabled("Satellite imagery or a map under the layers.");
        ImGui::PopID();
        return;
    }
    ImGui::SetNextItemWidth(-1);
    if (ImGui::SliderFloat("##opacity", &bm_.opacity, 0.05f, 1.0f, "opacity %.2f")) {
        mapDirty_ = true;
        ImGui::MarkIniSettingsDirty();
    }
    if (bm_.source == "custom") {
        // Applied when the field is left or Enter is pressed (not at each key).
        ImGui::SetNextItemWidth(-1);
        ImGui::InputTextWithHint("##url", "https://.../{z}/{x}/{y}.png", bmUrlEdit_, sizeof(bmUrlEdit_));
        if (ImGui::IsItemDeactivatedAfterEdit()) {
            bm_.url = bmUrlEdit_;
            basemapFailedFor_ = 0;
            ImGui::MarkIniSettingsDirty();
        }
        ImGui::SetItemTooltip("XYZ tile URL: {z} zoom, {x} column, {y} row from the top ({-y}: from the\n"
                              "bottom, TMS); {s} becomes 'a'. Keys go in the URL. Tiles in Web Mercator\n"
                              "(EPSG:3857). Applied when you leave the field.");
        ImGui::SetNextItemWidth(-1);
        ImGui::InputTextWithHint("##attr", "Attribution (shown on the map)", bmAttrEdit_, sizeof(bmAttrEdit_));
        if (ImGui::IsItemDeactivatedAfterEdit()) {
            bm_.attribution = bmAttrEdit_;
            ImGui::MarkIniSettingsDirty();
        }
        ImGui::SetItemTooltip("The provider's required credit, drawn on the map and in exports");
        ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x * 0.55f);
        if (ImGui::SliderInt("##maxzoom", &bm_.maxZoom, 0, 22, "max zoom %d")) ImGui::MarkIniSettingsDirty();
        ImGui::SetItemTooltip("Finest zoom level the service has (finer views enlarge its tiles)");
        ImGui::SameLine();
        ImGui::SetNextItemWidth(-1);
        int ts = bm_.tileSize == 512 ? 1 : 0;
        if (ImGui::Combo("##tilesize", &ts, "256 px tiles\0512 px tiles\0")) {
            bm_.tileSize = ts ? 512 : 256;
            ImGui::MarkIniSettingsDirty();
        }
    }
    // State: why it is not drawn, offline, a server error, loading.
    const ImVec4 warn(1.0f, 0.7f, 0.4f, 1);
    ImGui::PushTextWrapPos(0);
    if (!s_) {
        ImGui::TextDisabled("Drawn under a georeferenced series once one is open.");
    } else if (!basemapWhy_.empty()) {
        ImGui::TextColored(warn, "Not drawn: %s", basemapWhy_.c_str());
    } else if (basemap_ && bm_.on) {
        if (basemap_->offline()) ImGui::TextDisabled("Basemap unavailable (offline)");
        else if (const std::string e = basemap_->error(); !e.empty()) ImGui::TextColored(warn, "Server: %s", e.c_str());
        else if (const int n = basemap_->inflight()) ImGui::TextDisabled("Loading %d tile%s...", n, n > 1 ? "s" : "");
    }
    if (bm_.source != "custom" || basemapCustomOk()) {
        const BasemapSource src = basemapSource();
        ImGui::TextDisabled("%s", src.attribution.c_str());
        ImGui::SetItemTooltip("%s", src.terms.c_str());
    }
    ImGui::PopTextWrapPos();
    ImGui::PopID();
}

void App::uiBasemapPerf() {
    if (!basemap_) return;
    const Basemap& b = *basemap_;
    ImGui::SeparatorText("Basemap");
    ImGui::Text("%s: %d tiles on GPU (%.0f MB), %d loading", b.source().name.c_str(), b.gpuTiles(),
                b.gpuBytes() / 1048576.0, b.inflight());
    ImGui::Text("Average tile: %.1f ms (%d read)", b.avgLoadMs(), b.loads());
    if (b.firstTileMs() >= 0) ImGui::Text("First tile: %.0f ms after placing the map", b.firstTileMs());
    if (const Reprojection* R = b.map())
        ImGui::Text("Warp grid: %d x %d (%.2g px, %.0f ms)", R->gridW, R->gridH, R->gridError, R->buildMs);
}
