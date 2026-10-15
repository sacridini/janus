// Vector layers: field plots, samples, parcels... read through OGR (vectors.hpp)
// in the background, placed on the active layer's grid (any CRS, through PROJ;
// again whenever the active layer's grid changes) and drawn over every map
// panel with ImGui's lines, cut to what is on screen and thinned to about a
// vertex per screen point. A polygon becomes the ROI (Alt + click); the series
// at every feature are written to a CSV table (Extract series).
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <functional>
#include <memory>

#include <cpl_conv.h>
#include <gdal_priv.h>
#include <imgui_internal.h>
#include <ogr_spatialref.h>
#include <ogrsf_frmts.h>

#include "glfw.hpp"
#include "platform.hpp"
#include "profiler.hpp"

namespace fs = std::filesystem;

namespace {

// Colours of the vector layers: none is the ROI's yellow or a pin's colour.
const ImVec4 kVectorColors[] = {{1.00f, 0.45f, 0.15f, 1}, {0.95f, 0.30f, 0.95f, 1}, {0.70f, 1.00f, 0.20f, 1},
                                {1.00f, 1.00f, 1.00f, 1}, {0.35f, 0.55f, 1.00f, 1}};

uint64_t mixKey(uint64_t h, uint64_t v) { return (h ^ (v + 0x9e3779b97f4a7c15ull + (h << 6) + (h >> 2))) * 1099511628211ull; }

double segmentDistance(double px, double py, double ax, double ay, double bx, double by) {
    const double dx = bx - ax, dy = by - ay, len2 = dx * dx + dy * dy;
    const double k = len2 > 0 ? std::clamp(((px - ax) * dx + (py - ay) * dy) / len2, 0.0, 1.0) : 0.0;
    return std::hypot(px - ax - k * dx, py - ay - k * dy);
}

// Writes `text` as `path` (through a temporary file renamed when complete).
bool writeTextFile(const std::string& path, const std::string& text, std::string& error) {
    const std::string tmp = path + ".part";
    bool ok;
    {
        std::ofstream f(fs::u8path(tmp), std::ios::binary | std::ios::trunc);
        ok = f && f.write(text.data(), std::streamsize(text.size()));
        f.close();
        ok = ok && !f.fail();
    }
    std::error_code ec;
    if (ok) {
        fs::remove(fs::u8path(path), ec);
        fs::rename(fs::u8path(tmp), fs::u8path(path), ec);
        ok = !ec;
    }
    if (!ok) {
        error = "cannot write " + path;
        fs::remove(fs::u8path(tmp), ec);
    }
    return ok;
}

std::string number(double v) {
    if (std::isnan(v)) return "";
    char b[32];
    std::snprintf(b, sizeof(b), "%.7g", v);
    return b;
}

} // namespace

// ---------------------------------------------------------------------------
// Loading and placing
// ---------------------------------------------------------------------------

std::vector<std::string> App::takeVectorInputs(const std::vector<std::string>& inputs) {
    std::vector<std::string> rest, vec;
    std::error_code ec;
    for (const std::string& p : inputs)
        (isVectorPath(p) && !fs::is_directory(fs::u8path(p), ec) ? vec : rest).push_back(p);
    if (!vec.empty()) addVectorFiles(vec);
    return rest;
}

void App::addVectorFiles(const std::vector<std::string>& paths) {
    for (const std::string& path : paths) {
        if (val::isProject(path)) { // a validation project: its sample, labels and maps (app_validation.cpp)
            openValidationProject(path);
            continue;
        }
        vectorLoads_.push_back(std::async(std::launch::async, [path] {
            VectorLoad r;
            r.layers = loadVectorFile(path, r.error);
            glfwPostEmptyEvent();
            return r;
        }));
    }
}

uint64_t App::vectorPlaceKey() const {
    if (!s_) return 0;
    const CubeInfo& info = *s_->info;
    uint64_t h = std::hash<std::string>()(info.crsWkt);
    h = mixKey(h, info.hasGeoTransform);
    for (double g : info.geoTransform) {
        uint64_t b;
        std::memcpy(&b, &g, sizeof(b));
        h = mixKey(h, b);
    }
    return h | 1; // never 0 (not placed)
}

void App::placeVector(VectorLayer& V) {
    V.placedFor = vectorPlaceKey();
    V.px.clear();
    V.box.clear();
    V.anchor.clear();
    if (!s_) {
        V.why = "drawn over a raster series: open one";
        return;
    }
    const CubeInfo& info = *s_->info;
    std::string why;
    const std::unique_ptr<VectorPlacer> P = VectorPlacer::create(*V.data, info.crsWkt, info.hasGeoTransform,
                                                                 info.geoTransform, why);
    if (!P) {
        V.why = "not drawn: " + why;
        return;
    }
    const size_t n = V.data->features.size();
    V.px.resize(n);
    V.box.resize(n);
    V.anchor.resize(n);
    size_t onMap = 0;
    for (size_t i = 0; i < n; ++i) {
        const VectorFeature& F = V.data->features[i];
        V.px[i] = P->placed(F.parts);
        double b[4];
        const bool any = polygonBounds(V.px[i], b);
        V.box[i] = any ? std::array<float, 4>{float(b[0]), float(b[1]), float(b[2]), float(b[3])}
                       : std::array<float, 4>{1, 1, 0, 0};
        double ax = F.cx, ay = F.cy;
        V.anchor[i] = F.kind != VectorFeature::None && P->toPixel(ax, ay) ? ImVec2(float(ax), float(ay))
                                                                            : ImVec2(NAN, NAN);
        onMap += any && b[2] >= 0 && b[3] >= 0 && b[0] <= info.width && b[1] <= info.height;
    }
    V.why.clear();
    V.offMap = onMap == 0; // still drawn: it may be near
}

void App::pumpVectors() {
    PROFILE_ZONE();
    for (size_t k = 0; k < vectorLoads_.size();) {
        if (vectorLoads_[k].wait_for(std::chrono::seconds(0)) != std::future_status::ready) {
            ++k;
            continue;
        }
        VectorLoad r = vectorLoads_[k].get();
        vectorLoads_.erase(vectorLoads_.begin() + long(k));
        if (r.layers.empty()) {
            error_ = r.error;
            openErrorPopup_ = true;
            continue;
        }
        for (auto& d : r.layers) {
            VectorLayer V;
            V.id = nextVectorId_++;
            V.data = d;
            const std::string file = fs::u8path(d->path).filename().u8string();
            V.name = r.layers.size() > 1 || fs::u8path(d->path).stem().u8string() != d->layer ? file + ": " + d->layer : file;
            V.color = kVectorColors[(V.id - 1) % std::size(kVectorColors)];
            vectors_.push_back(std::move(V));
        }
    }
    const uint64_t key = vectorPlaceKey();
    for (VectorLayer& V : vectors_)
        if (V.placedFor != key || (key == 0 && V.why.empty())) placeVector(V);
}

App::VectorLayer* App::vectorById(int id) {
    for (VectorLayer& V : vectors_)
        if (V.id == id) return &V;
    return nullptr;
}

void App::removeVector(int index) {
    if (index < 0 || index >= int(vectors_.size())) return;
    if (lab_.vector == vectors_[index].id) stopInterpretation();
    vectors_.erase(vectors_.begin() + index);
    hoverVector_ = hoverFeature_ = -1;
}

// ---------------------------------------------------------------------------
// Drawing and picking
// ---------------------------------------------------------------------------

void App::drawVectorMarks(ImDrawList* dl, ImVec2 origin, bool forExport) {
    (void)forExport;
    if (vectors_.empty()) return;
    const ImVec2 c0 = dl->GetClipRectMin(), c1 = dl->GetClipRectMax();
    // What is on screen, in active pixels (a margin for the lines' width).
    const double m = 8 / scale_;
    const double vx0 = (c0.x - origin.x - offset_.x) / scale_ - m, vy0 = (c0.y - origin.y - offset_.y) / scale_ - m;
    const double vx1 = (c1.x - origin.x - offset_.x) / scale_ + m, vy1 = (c1.y - origin.y - offset_.y) / scale_ + m;
    auto toScreen = [&](double x, double y) {
        return origin + ImVec2(float(offset_.x + x * scale_), float(offset_.y + y * scale_));
    };
    const ImU32 dark = IM_COL32(0, 0, 0, 170);
    size_t budget = 2000000; // vertices per frame: beyond, the rest waits for a closer zoom
    std::vector<ImVec2> pts;
    const VectorLayer* interp = lab_.vector ? vectorById(lab_.vector) : nullptr;
    auto inView = [&](const std::array<float, 4>& b) {
        return b[0] <= b[2] && b[2] >= vx0 && b[0] <= vx1 && b[3] >= vy0 && b[1] <= vy1;
    };
    for (size_t li = 0; li < vectors_.size(); ++li) {
        const VectorLayer& V = vectors_[li];
        if (!V.visible || V.px.empty()) continue;
        const ImU32 layerCol = ImGui::ColorConvertFloat4ToU32(V.color);
        // Many points in view: squares (4 vertices each, no antialiased edge),
        // ~10x fewer vertices than discs (measured: 20 000 points, 1.7 M -> 0.16 M).
        size_t pointsInView = 0;
        if (V.data->points > 2000)
            for (size_t i = 0; i < V.px.size(); ++i)
                pointsInView += V.data->features[i].kind == VectorFeature::Point && inView(V.box[i]);
        const bool squares = pointsInView > 2000;
        for (size_t i = 0; i < V.px.size() && budget > 0; ++i) {
            const auto& b = V.box[i];
            if (!inView(b)) continue;
            const VectorFeature& F = V.data->features[i];
            const bool hot = (int(li) == hoverVector_ && int(i) == hoverFeature_) || (&V == interp && int(i) == lab_.current);
            const ImU32 col = i < V.colors.size() ? V.colors[i] : layerCol;
            if (F.kind == VectorFeature::Point) {
                for (const Ring& r : V.px[i]) {
                    if (r.size() < 2) continue;
                    const ImVec2 c = toScreen(r[0], r[1]);
                    if (squares && !hot) {
                        dl->AddRectFilled(c - ImVec2(2.5f, 2.5f), c + ImVec2(2.5f, 2.5f), dark);
                        dl->AddRectFilled(c - ImVec2(1.5f, 1.5f), c + ImVec2(1.5f, 1.5f), col);
                        continue;
                    }
                    const float rad = hot ? 6.0f : 4.0f;
                    dl->AddCircleFilled(c, rad + 1.5f, dark, 12);
                    dl->AddCircleFilled(c, rad, col, 12);
                    if (hot) dl->AddCircle(c, rad + 3.0f, IM_COL32(255, 255, 255, 255), 0, 2.0f);
                }
                --budget;
                continue;
            }
            // Smaller than a couple of screen points: a dot.
            if ((b[2] - b[0]) * scale_ < 2 && (b[3] - b[1]) * scale_ < 2) {
                const ImVec2 c = toScreen((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5);
                dl->AddRectFilled(c - ImVec2(1, 1), c + ImVec2(1, 1), col);
                --budget;
                continue;
            }
            const bool closed = F.kind == VectorFeature::Area;
            const float thick = hot ? 3.0f : 1.0f; // whole widths: drawn from the font texture (fewer vertices)
            for (const Ring& r : V.px[i]) {
                pts.clear();
                for (size_t k = 0; k + 1 < r.size(); k += 2) {
                    const ImVec2 p = toScreen(r[k], r[k + 1]);
                    if (!pts.empty() && k + 2 < r.size()) { // a vertex per 1.5 screen points or so
                        const ImVec2 d = p - pts.back();
                        if (d.x * d.x + d.y * d.y < 2.25f) continue;
                    }
                    pts.push_back(p);
                }
                if (pts.size() < 2) continue;
                budget = pts.size() > budget ? 0 : budget - pts.size();
                const ImDrawFlags fl = closed ? ImDrawFlags_Closed : ImDrawFlags_None;
                dl->AddPolyline(pts.data(), int(pts.size()), dark, fl, thick + 2.0f);
                dl->AddPolyline(pts.data(), int(pts.size()), hot ? IM_COL32(255, 255, 255, 255) : col, fl, thick);
            }
        }
    }
    if (budget == 0) {
        const char* note = "Vectors: too many vertices to draw at this zoom; zoom in";
        dl->AddText(ImVec2(c0.x + 10, c1.y - 52), IM_COL32(255, 220, 120, 255), note);
    }
}

bool App::vectorFeatureAt(double x, double y, int& layer, int& feature, float tol) const {
    layer = feature = -1;
    const double t = tol / scale_; // in active pixels
    double bestPoint = HUGE_VAL, bestLine = HUGE_VAL, bestArea = HUGE_VAL;
    int kind = 0; // of the best so far: 3 point, 2 line, 1 area
    for (int li = int(vectors_.size()) - 1; li >= 0; --li) {
        const VectorLayer& V = vectors_[li];
        if (!V.visible || V.px.empty()) continue;
        for (size_t i = 0; i < V.px.size(); ++i) {
            const auto& b = V.box[i];
            if (b[0] > b[2] || x < b[0] - t || x > b[2] + t || y < b[1] - t || y > b[3] + t) continue;
            const VectorFeature& F = V.data->features[i];
            if (F.kind == VectorFeature::Point) {
                for (const Ring& r : V.px[i]) {
                    const double d = r.size() >= 2 ? std::hypot(r[0] - x, r[1] - y) : HUGE_VAL;
                    if (d <= t && d < bestPoint) {
                        bestPoint = d;
                        kind = 3;
                        layer = li;
                        feature = int(i);
                    }
                }
            } else if (F.kind == VectorFeature::Line && kind < 3) {
                for (const Ring& r : V.px[i])
                    for (size_t k = 0; k + 3 < r.size(); k += 2) {
                        const double d = segmentDistance(x, y, r[k], r[k + 1], r[k + 2], r[k + 3]);
                        if (d <= t * 0.7 && d < bestLine) {
                            bestLine = d;
                            kind = 2;
                            layer = li;
                            feature = int(i);
                        }
                    }
            } else if (F.kind == VectorFeature::Area && kind < 2 && pointInPolygon(V.px[i], x, y)) {
                const double a = double(b[2] - b[0]) * (b[3] - b[1]); // nested polygons: the smallest
                if (a < bestArea) {
                    bestArea = a;
                    kind = 1;
                    layer = li;
                    feature = int(i);
                }
            }
        }
    }
    return layer >= 0;
}

bool App::featureAsRoi(const VectorLayer& V, int feature) {
    if (feature < 0 || feature >= int(V.px.size()) || V.data->features[feature].kind != VectorFeature::Area) return false;
    Polygon p = V.px[feature];
    p.erase(std::remove_if(p.begin(), p.end(), [](const Ring& r) { return r.size() < 6; }), p.end());
    return !p.empty() && setRoiPolygon(p);
}

void App::centerOnFeature(const VectorLayer& V, int feature, double minScale) {
    if (!s_ || feature < 0 || feature >= int(V.anchor.size())) return;
    const ImVec2 a = V.anchor[feature];
    if (std::isnan(a.x)) return;
    const ImVec2 c = canvasSize_.x > 0 ? canvasSize_ : ImVec2(800, 600);
    double s = std::max(scale_, std::min(minScale, 64.0));
    const auto& b = V.box[feature];
    if (V.data->features[feature].kind != VectorFeature::Point && b[0] <= b[2]) {
        // A polygon or a line in view whole, with a margin (but no closer than asked).
        const double fit = 0.8 * std::min(c.x / std::max(1.0f, b[2] - b[0]), c.y / std::max(1.0f, b[3] - b[1]));
        s = std::clamp(std::min(s, fit), 0.01, 64.0);
        const ImVec2 mid((b[0] + b[2]) * 0.5f, (b[1] + b[3]) * 0.5f);
        offset_ = ImVec2(float(c.x * 0.5 - mid.x * s), float(c.y * 0.5 - mid.y * s));
    } else {
        offset_ = ImVec2(float(c.x * 0.5 - a.x * s), float(c.y * 0.5 - a.y * s));
    }
    scale_ = s;
    zooming_ = false;
    viewTouched_ = true;
    mapDirty_ = true;
}

void App::uiVectorTooltip() {
    PROFILE_ZONE();
    if (hoverVector_ < 0 || hoverVector_ >= int(vectors_.size())) return;
    const VectorLayer& V = vectors_[hoverVector_];
    if (hoverFeature_ < 0 || hoverFeature_ >= int(V.data->features.size())) return;
    const VectorFeature& F = V.data->features[hoverFeature_];
    // Points always (small targets, aimed at); lines and polygons with Alt held.
    if (F.kind != VectorFeature::Point && !ImGui::GetIO().KeyAlt) return;
    ImGui::BeginTooltip();
    ImGui::TextColored(V.color, "%s", V.name.c_str());
    ImGui::TextDisabled("feature %lld%s", (long long)F.fid,
                        F.kind == VectorFeature::Area   ? "  (Alt + click: use as the ROI)"
                        : F.kind == VectorFeature::Line ? "  (Alt + click: transect along it)"
                                                        : "");
    const size_t shown = std::min<size_t>(V.data->fields.size(), 16);
    if (shown && ImGui::BeginTable("##attrs", 2, ImGuiTableFlags_SizingFixedFit)) {
        for (size_t k = 0; k < shown; ++k) {
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextDisabled("%s", V.data->fields[k].c_str());
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(F.values[k].c_str());
        }
        ImGui::EndTable();
    }
    if (V.data->fields.size() > shown) ImGui::TextDisabled("... %zu more fields", V.data->fields.size() - shown);
    ImGui::EndTooltip();
}

// ---------------------------------------------------------------------------
// Layers panel
// ---------------------------------------------------------------------------

void App::uiVectors() {
    PROFILE_ZONE();
    if (vectors_.empty() && vectorLoads_.empty()) return;
    ImGui::SeparatorText("Vector layers");
    if (!vectorLoads_.empty()) ImGui::TextDisabled("reading %d file(s)...", int(vectorLoads_.size()));
    int remove = -1;
    for (int i = int(vectors_.size()) - 1; i >= 0; --i) { // top-down, as drawn
        VectorLayer& V = vectors_[i];
        const VectorData& D = *V.data;
        ImGui::PushID(V.id);
        ImGui::Checkbox("##vis", &V.visible);
        ImGui::SetItemTooltip(V.visible ? "Hide this vector layer" : "Show this vector layer");
        ImGui::SameLine();
        ImGui::ColorEdit3("##col", &V.color.x, ImGuiColorEditFlags_NoInputs | ImGuiColorEditFlags_NoLabel);
        ImGui::SameLine();
        ImGui::Selectable(V.name.c_str(), lab_.vector == V.id, 0, ImVec2(ImGui::GetContentRegionAvail().x - 26, 0));
        if (ImGui::BeginItemTooltip()) {
            ImGui::TextUnformatted(D.path.c_str());
            ImGui::Text("%s, %zu vertices, read in %.0f ms", D.kindText().c_str(), D.vertices, D.loadMs);
            ImGui::Text("CRS: %s%s", D.crsName.empty() ? "none (read as the raster's)" : D.crsName.c_str(),
                        D.lonLatAssumed ? " (none in the file: the coordinates are longitudes and latitudes)" : "");
            if (!D.fields.empty()) {
                std::string f;
                for (size_t k = 0; k < D.fields.size() && k < 12; ++k) f += (k ? ", " : "") + D.fields[k];
                if (D.fields.size() > 12) f += ", ...";
                ImGui::Text("Fields: %s", f.c_str());
            }
            if (D.truncated) ImGui::TextColored(theme::warning(), "Read up to the vertex limit: the rest is left out.");
            ImGui::TextDisabled("Right click: more");
            ImGui::EndTooltip();
        }
        if (ImGui::BeginPopupContextItem("##ctx")) {
            if (ImGui::MenuItem("Extract series to CSV...", nullptr, false, s_ && V.why.empty())) openExtract(V.id);
            if (ImGui::MenuItem("Interpret (label) its features...", nullptr, false, s_ != nullptr)) startInterpretation(V.id);
            if (ImGui::MenuItem("Zoom to it", nullptr, false, !V.px.empty())) {
                double b[4] = {HUGE_VAL, HUGE_VAL, -HUGE_VAL, -HUGE_VAL};
                for (const auto& x : V.box)
                    if (x[0] <= x[2]) {
                        b[0] = std::min(b[0], double(x[0]));
                        b[1] = std::min(b[1], double(x[1]));
                        b[2] = std::max(b[2], double(x[2]));
                        b[3] = std::max(b[3], double(x[3]));
                    }
                if (b[0] <= b[2] && canvasSize_.x > 0) {
                    const double s = std::clamp(0.9 * std::min(canvasSize_.x / std::max(1.0, b[2] - b[0]),
                                                               canvasSize_.y / std::max(1.0, b[3] - b[1])), 0.01, 64.0);
                    scale_ = s;
                    offset_ = ImVec2(float(canvasSize_.x * 0.5 - (b[0] + b[2]) * 0.5 * s),
                                     float(canvasSize_.y * 0.5 - (b[1] + b[3]) * 0.5 * s));
                    zooming_ = false;
                    viewTouched_ = mapDirty_ = true;
                }
            }
            if (ImGui::MenuItem("Remove")) remove = i;
            ImGui::EndPopup();
        }
        ImGui::SameLine();
        if (ImGui::SmallButton("x")) remove = i;
        ImGui::SetItemTooltip("Remove this vector layer");
        ImGui::Indent();
        ImGui::TextDisabled("%s%s", D.kindText().c_str(), D.crsName.empty() ? "" : (", " + D.crsName).c_str());
        if (!V.why.empty()) ImGui::TextColored(theme::warning(), "%s", V.why.c_str());
        else if (V.offMap) ImGui::TextColored(theme::warning(), "nothing of it over the active layer");
        if (s_ && V.why.empty()) {
            if (ImGui::SmallButton("Extract series...")) openExtract(V.id);
            ImGui::SetItemTooltip("The series of every feature (a point's pixel, a polygon's mean)\n"
                                  "of the active or every visible layer, as a CSV table");
            ImGui::SameLine();
            if (ImGui::SmallButton("Interpret...")) startInterpretation(V.id);
            ImGui::SetItemTooltip("Go through its features one by one (N / Shift+N) and label them (1-9)");
        }
        ImGui::Unindent();
        ImGui::PopID();
    }
    if (remove >= 0) removeVector(remove);
    if (ImGui::SmallButton("Add vector layer...")) {
        auto files = platform::openFilesDialog();
        if (!files.empty()) addVectorFiles(files);
    }
    ImGui::SetItemTooltip("GeoJSON, Shapefile, GeoPackage, KML, CSV of points (x/lon, y/lat columns)...");
    ImGui::TextDisabled("Alt + click a polygon: it becomes the ROI; a line: the transect.");
    ImGui::Separator();
}

// ---------------------------------------------------------------------------
// Extract series
// ---------------------------------------------------------------------------

void App::openExtract(int vectorId) {
    const VectorLayer* V = vectorById(vectorId);
    if (!V || !s_) return;
    extract_.open = true;
    extract_.vector = vectorId;
    std::string stem = fs::u8path(V->data->path).stem().u8string();
    const SeriesLayer* L = activeLayer();
    std::string name = stem + "_series" + (L ? "_" + L->name : std::string()) + ".csv";
    for (char& c : name)
        if (std::strchr("\\/:*?\"<>| ", c)) c = '_';
    const std::string path = (fs::u8path(exportFolder()) / fs::u8path(name)).u8string();
    std::snprintf(extract_.path, sizeof(extract_.path), "%s", path.c_str());
}

void App::uiExtract() {
    PROFILE_ZONE();
    if (extract_.open) {
        ImGui::OpenPopup("Extract series###extract");
        extract_.open = false;
    }
    if (!ImGui::BeginPopupModal("Extract series###extract", nullptr, ImGuiWindowFlags_AlwaysAutoResize)) return;
    const VectorLayer* V = vectorById(extract_.vector);
    if (!V || !s_) {
        ImGui::CloseCurrentPopup();
        ImGui::EndPopup();
        return;
    }
    const VectorData& D = *V->data;
    ImGui::TextColored(V->color, "%s", V->name.c_str());
    ImGui::TextDisabled("%s", D.kindText().c_str());
    ImGui::SeparatorText("Series of");
    if (ImGui::RadioButton("The active layer", !extract_.allLayers)) extract_.allLayers = false;
    int visible = 0;
    for (const SeriesLayer& L : layers_) visible += L.visible;
    char label[96];
    std::snprintf(label, sizeof(label), "Every visible layer (%d)", visible);
    if (ImGui::RadioButton(label, extract_.allLayers)) extract_.allLayers = true;
    ImGui::TextDisabled("Points: the pixel under each. Polygons: the mean of the pixels whose centres\n"
                        "are inside (and their count). Lines are left out. Full resolution, the values\n"
                        "the chart shows (band, normalized difference, quality mask applied).");
    ImGui::SeparatorText("Table");
    if (ImGui::RadioButton("A row per feature, a column per date", extract_.wide)) extract_.wide = true;
    if (ImGui::RadioButton("A row per feature and date (long)", !extract_.wide)) extract_.wide = false;
    ImGui::SetItemTooltip("Easier to read in R (tidyverse) or pandas, and with several layers of different dates");
    ImGui::SeparatorText("Save to");
    ImGui::SetNextItemWidth(ImGui::GetFontSize() * 34);
    ImGui::InputText("##path", extract_.path, sizeof(extract_.path));
    ImGui::SameLine();
    if (ImGui::Button("Browse...")) {
        const fs::path t = fs::u8path(extract_.path);
        const std::string p = askSavePath("Extract series", t.filename().u8string(), "CSV table", "csv",
                                          t.parent_path().u8string());
        if (!p.empty()) std::snprintf(extract_.path, sizeof(extract_.path), "%s", p.c_str());
    }
    std::string target = extract_.path;
    std::error_code ec;
    if (!target.empty() && fs::u8path(target).extension().empty()) target += ".csv";
    const bool folderOk = !target.empty() && fs::is_directory(fs::u8path(target).parent_path(), ec);
    if (!folderOk) ImGui::TextColored(theme::warning(), "Type a file name in an existing folder, or Browse...");
    else if (fs::exists(fs::u8path(target), ec)) ImGui::TextColored(theme::warning(), "The file exists and will be replaced.");
    const bool deferred = s_->deferRandomReads();
    if (deferred) ImGui::TextColored(theme::warning(), "Building the overview from an HDD: available once it ends.");
    ImGui::Spacing();
    ImGui::BeginDisabled(!folderOk || deferred);
    const bool go = ImGui::Button("Extract", ImVec2(120, 0));
    ImGui::EndDisabled();
    ImGui::SameLine();
    if (ImGui::Button("Cancel", ImVec2(120, 0)) || ImGui::IsKeyPressed(ImGuiKey_Escape, false)) ImGui::CloseCurrentPopup();
    if (go) {
        exportDir_ = fs::u8path(target).parent_path().u8string();
        ImGui::MarkIniSettingsDirty();
        extractSeries(*V, extract_.allLayers, extract_.wide, target);
        ImGui::CloseCurrentPopup();
    }
    ImGui::EndPopup();
}

std::shared_ptr<ExportJob> App::extractSeries(const VectorLayer& V, bool allLayers, bool wide, const std::string& path) {
    struct Target {
        std::shared_ptr<const CubeInfo> info;
        std::shared_ptr<FullResCache> cache;
        std::string name;
    };
    std::vector<Target> targets;
    for (const SeriesLayer& L : layers_)
        if (&L == activeLayer() || (allLayers && L.visible))
            targets.push_back({L.session->info, L.session->fullRes, L.name});
    if (targets.empty()) return nullptr;
    // The active layer first: its columns lead.
    std::stable_partition(targets.begin(), targets.end(), [&](const Target& t) { return t.info == s_->info; });
    auto data = V.data;
    char title[256];
    std::snprintf(title, sizeof(title), "Series at %zu features of %s, %zu layer%s", data->features.size(),
                  V.name.c_str(), targets.size(), targets.size() == 1 ? "" : "s");
    auto job = startExportJob(title, path, [data, targets, wide, path](ExportJob& j, std::string& error) {
        const size_t N = data->features.size(), K = targets.size();
        // values[k][i * T + t]; pixels[k][i]: pixels read (1 for a point, those inside a polygon).
        std::vector<std::vector<float>> values(K);
        std::vector<std::vector<int>> pixels(K, std::vector<int>(N, 0));
        size_t work = 0, done = 0;
        for (const Target& tg : targets) work += N * size_t(tg.info->T());
        for (size_t k = 0; k < K && !j.cancel; ++k) {
            const Target& tg = targets[k];
            const CubeInfo& info = *tg.info;
            const int T = info.T();
            values[k].assign(N * size_t(T), NAN);
            std::string why;
            const auto P = VectorPlacer::create(*data, info.crsWkt, info.hasGeoTransform, info.geoTransform, why);
            if (!P) {
                done += N * size_t(T);
                continue;
            }
            CubeReader reader(tg.info);
            auto read = [&](int t, int x, int y, int w, int h, float* buf, int bw, int bh) {
                return (tg.cache && tg.cache->readWindow(t, x, y, w, h, buf, bw, bh)) ||
                       reader.readWindow(t, x, y, w, h, buf, bw, bh);
            };
            // Points: date by date (each date's blocks stay in GDAL's cache), in row order.
            std::vector<std::pair<int, int>> px(N, {-1, -1});
            std::vector<size_t> order;
            for (size_t i = 0; i < N; ++i) {
                const VectorFeature& F = data->features[i];
                if (F.kind != VectorFeature::Point) continue;
                double x = F.parts[0][0], y = F.parts[0][1];
                if (!P->toPixel(x, y)) continue;
                const int ix = int(std::floor(x)), iy = int(std::floor(y));
                if (ix < 0 || iy < 0 || ix >= info.width || iy >= info.height) continue;
                px[i] = {ix, iy};
                pixels[k][i] = 1;
                order.push_back(i);
            }
            std::sort(order.begin(), order.end(), [&](size_t a, size_t b) { return px[a].second < px[b].second; });
            const size_t areas = size_t(std::count_if(data->features.begin(), data->features.end(),
                                                      [](const VectorFeature& F) { return F.kind == VectorFeature::Area; }));
            done += (N - order.size() - areas) * size_t(T); // lines, points off this layer: nothing to read
            for (int t = 0; t < T && !j.cancel; ++t) {
                for (size_t i : order) {
                    float v = NAN;
                    if (read(t, px[i].first, px[i].second, 1, 1, &v, 1, 1)) values[k][i * T + t] = v;
                }
                done += order.size();
                setExportProgress(j, double(done) / double(std::max<size_t>(1, work)));
            }
            // Polygons: the mean of the pixels whose centres are inside, read in their bounding box.
            for (size_t i = 0; i < N && !j.cancel; ++i) {
                const VectorFeature& F = data->features[i];
                if (F.kind != VectorFeature::Area) continue;
                Polygon poly = P->placed(F.parts);
                poly.erase(std::remove_if(poly.begin(), poly.end(), [](const Ring& r) { return r.size() < 6; }),
                           poly.end());
                double b[4];
                done += size_t(T);
                if (poly.empty() || !polygonBounds(poly, b)) continue;
                const int x0 = std::clamp(int(std::floor(b[0])), 0, info.width), y0 = std::clamp(int(std::floor(b[1])), 0, info.height);
                const int x1 = std::clamp(int(std::ceil(b[2])), 0, info.width), y1 = std::clamp(int(std::ceil(b[3])), 0, info.height);
                if (x1 <= x0 || y1 <= y0) continue;
                const int w = x1 - x0, h = y1 - y0;
                const double s = std::min(1.0, std::sqrt(1.0e6 / (double(w) * h))); // large ones read subsampled
                const int bw = std::max(1, int(w * s)), bh = std::max(1, int(h * s));
                const std::vector<char> mask = polygonMask(poly, x0, y0, w, h, bw, bh);
                int inside = 0;
                for (char c : mask) inside += c != 0;
                pixels[k][i] = s < 1.0 ? int(std::lround(inside / (s * s))) : inside;
                if (!inside) continue;
                std::vector<float> buf(size_t(bw) * bh);
                for (int t = 0; t < T && !j.cancel; ++t) {
                    if (!read(t, x0, y0, w, h, buf.data(), bw, bh)) continue;
                    double sum = 0;
                    int n = 0;
                    for (size_t q = 0; q < buf.size(); ++q)
                        if (mask[q] && std::isfinite(buf[q])) {
                            sum += buf[q];
                            ++n;
                        }
                    if (n) values[k][i * T + t] = float(sum / n);
                }
                setExportProgress(j, double(done) / double(std::max<size_t>(1, work)));
            }
        }
        if (j.cancel) return false;
        // The table: the feature's fields, its place in lon/lat, then the values.
        const auto LL = VectorPlacer::lonLat(*data);
        std::string out = "fid";
        for (const std::string& f : data->fields) out += "," + csvField(f);
        out += ",lon,lat";
        auto colName = [&](size_t k, int t) {
            const std::string& d = targets[k].info->layers[t].label;
            return K == 1 ? d : targets[k].name + " " + d;
        };
        auto pixelsName = [&](size_t k) { return K == 1 ? std::string("pixels") : "pixels " + targets[k].name; };
        if (wide) {
            for (size_t k = 0; k < K; ++k) {
                if (data->areas) out += "," + csvField(pixelsName(k));
                for (int t = 0; t < targets[k].info->T(); ++t) out += "," + csvField(colName(k, t));
            }
        } else {
            out += ",layer,date,value";
            if (data->areas) out += ",pixels";
        }
        out += "\n";
        for (size_t i = 0; i < N; ++i) {
            const VectorFeature& F = data->features[i];
            if (F.kind != VectorFeature::Point && F.kind != VectorFeature::Area) continue;
            std::string head = std::to_string(F.fid);
            for (const std::string& v : F.values) head += "," + csvField(v);
            double lon = F.cx, lat = F.cy;
            head += LL->toLonLat(lon, lat) ? "," + number(lon) + "," + number(lat) : std::string(",,");
            if (wide) {
                out += head;
                for (size_t k = 0; k < K; ++k) {
                    const int T = targets[k].info->T();
                    if (data->areas) out += "," + std::to_string(pixels[k][i]);
                    for (int t = 0; t < T; ++t) out += "," + number(values[k][i * T + t]);
                }
                out += "\n";
            } else {
                for (size_t k = 0; k < K; ++k) {
                    const int T = targets[k].info->T();
                    for (int t = 0; t < T; ++t) {
                        out += head + "," + csvField(targets[k].name) + "," + csvField(targets[k].info->layers[t].label) +
                               "," + number(values[k][i * T + t]);
                        if (data->areas) out += "," + std::to_string(pixels[k][i]);
                        out += "\n";
                    }
                }
            }
        }
        return writeTextFile(path, out, error);
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

// ---------------------------------------------------------------------------
// --selftest-ui stages 120-126: B active, A and F (UTM 22S) as layers. Vector
// files written here: points in lon/lat (GeoJSON, one far away), a polygon with
// a hole and a line in UTM 22S (GeoPackage), points in a CSV without a CRS.
// Their places on B's grid, picking, a polygon as the ROI, the series
// extracted (wide over every layer, long for the polygon) against the files,
// the vectors in a PNG; the interpretation: keys, a label and its date, the
// labels saved, resumed and exported (GeoPackage, CSV).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// --measure-vectors IN VEC...: a series and vector files over it, in a hidden
// window: reading, placing, drawing (at the whole view and zoomed in), picking
// and the series extracted at every feature.
// ---------------------------------------------------------------------------

int App::measureVectorsStep(const std::vector<std::string>& in) {
    using Clock = std::chrono::steady_clock;
    auto ms = [](Clock::time_point t) { return std::chrono::duration<double, std::milli>(Clock::now() - t).count(); };
    if (in.size() < 2) {
        std::printf("--measure-vectors needs a series and vector files\n");
        return 2;
    }
    switch (st_.stage) {
    case 0:
        ImGui::GetIO().IniFilename = nullptr;
        bm_ = BasemapUi{};
        st_.t0 = std::chrono::duration<double>(Clock::now().time_since_epoch()).count();
        openInputs({in[0]});
        st_.stage = 1;
        break;
    case 1:
        if (opening_.valid() || layers_.empty() || !s_->overview.complete() || !s_->gpu.statsValid) break;
        addVectorFiles(std::vector<std::string>(in.begin() + 1, in.end()));
        st_.stage = 2;
        break;
    case 2: {
        if (!vectorLoads_.empty() || vectors_.empty()) break;
        for (const VectorLayer& V : vectors_)
            if (!V.placedFor) return -1;
        const CubeInfo& info = *s_->info;
        std::printf("series: %d x %d px, %d dates (%s)\n", info.width, info.height, info.T(), info.description.c_str());
        canvasSize_ = ImVec2(1600, 900);
        for (VectorLayer& V : vectors_) {
            double place = 1e300;
            for (int k = 0; k < 3; ++k) {
                const auto t = Clock::now();
                placeVector(V);
                place = std::min(place, ms(t));
            }
            std::printf("%s: %s, %zu vertices, %s; read in %.0f ms, placed on the series' grid in %.1f ms\n",
                        V.name.c_str(), V.data->kindText().c_str(), V.data->vertices, V.data->crsName.c_str(),
                        V.data->loadMs, place);
        }
        // Drawing: every layer, the whole series in view, then 8 points per pixel at its centre.
        ImDrawList* dl = ImGui::GetForegroundDrawList();
        for (int zoom = 0; zoom < 2; ++zoom) {
            if (zoom == 0) {
                fitView(canvasSize_);
            } else {
                scale_ = 8;
                offset_ = ImVec2(float(canvasSize_.x / 2 - info.width / 2.0 * scale_),
                                 float(canvasSize_.y / 2 - info.height / 2.0 * scale_));
            }
            dl->PushClipRect(ImVec2(0, 0), canvasSize_);
            double best = 1e300;
            int vtx = 0;
            for (int k = 0; k < 5; ++k) {
                const int v0 = dl->VtxBuffer.Size;
                const auto t = Clock::now();
                drawVectorMarks(dl, ImVec2(0, 0));
                best = std::min(best, ms(t));
                vtx = dl->VtxBuffer.Size - v0;
            }
            dl->PopClipRect();
            std::printf("draw, %s (%.2f points per pixel): %.2f ms per frame, %d ImGui vertices\n",
                        zoom ? "zoomed in" : "whole series", scale_, best, vtx);
        }
        fitView(canvasSize_);
        // Picking: the feature under 2000 places.
        uint32_t seed = 12345;
        auto rnd = [&] { return (seed = seed * 1664525u + 1013904223u) / 4294967296.0; };
        const auto t = Clock::now();
        int hits = 0;
        for (int k = 0; k < 2000; ++k) {
            int vl, vf;
            hits += vectorFeatureAt(rnd() * info.width, rnd() * info.height, vl, vf);
        }
        std::printf("picking: %.1f us per place (%d of 2000 on a feature)\n", ms(t) * 1000 / 2000, hits);
        // The series of every feature, each layer to its own table.
        st_.jobs.clear();
        st_.dir = (fs::temp_directory_path() / "janus-measure-vectors").u8string();
        std::error_code ec;
        fs::create_directories(fs::u8path(st_.dir), ec);
        for (const VectorLayer& V : vectors_)
            st_.jobs.push_back(extractSeries(V, false, true, (fs::u8path(st_.dir) / (std::to_string(V.id) + ".csv")).u8string()));
        st_.stage = 3;
        break;
    }
    case 3: {
        for (auto& j : st_.jobs) {
            j->done.wait();
            std::error_code ec;
            std::lock_guard<std::mutex> lk(j->m);
            std::printf("%s: %.2f s, %.0f KB %s\n", j->title.c_str(), j->seconds,
                        double(fs::file_size(fs::u8path(j->path), ec)) / 1024, j->error.c_str());
        }
        std::error_code ec;
        fs::remove_all(fs::u8path(st_.dir), ec);
        return 0;
    }
    }
    return -1;
}

namespace {

std::vector<std::vector<std::string>> readCsv(const std::string& path) {
    std::vector<std::vector<std::string>> rows;
    std::ifstream in(fs::u8path(path), std::ios::binary);
    std::string line;
    while (std::getline(in, line)) {
        if (!line.empty() && line.back() == '\r') line.pop_back();
        std::vector<std::string> row;
        size_t a = 0;
        for (size_t b; (b = line.find(',', a)) != std::string::npos; a = b + 1) row.push_back(line.substr(a, b - a));
        row.push_back(line.substr(a));
        rows.push_back(std::move(row));
    }
    return rows;
}

} // namespace

int App::selfTestVectors(const std::vector<std::string>& in) {
    (void)in;
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
    auto file = [&](const char* name) { return (fs::u8path(st_.dir) / name).u8string(); };
    auto layerOf = [&](const char* fileName) -> VectorLayer* {
        for (VectorLayer& V : vectors_)
            if (fs::u8path(V.data->path).filename().u8string() == fileName) return &V;
        return nullptr;
    };
    // B's pixels: points (the last far away, at 0 N 0 E), the polygon's shell and hole, the line.
    static const double kPts[][2] = {{40.5, 50.5}, {200.25, 120.75}, {280.5, 10.5}, {60.5, 150.5}};
    static const double kShell[] = {100, 40, 180, 40, 180, 110, 100, 110}, kHole[] = {120, 60, 150, 60, 150, 90, 120, 90};
    static const double kLine[] = {20, 180, 90, 185};
    switch (st_.stage) {
    case 120: {
        if (layers_.size() != 3 || active_ != 0) return fail("B should be active, with A and F");
        const CubeInfo& B = *s_->info;
        st_.dir = (fs::temp_directory_path() /
                   ("janus-selftest-vectors-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count())))
                      .u8string();
        std::error_code ec;
        fs::create_directories(fs::u8path(st_.dir), ec);
        OGRSpatialReference utm23, utm22, wgs;
        utm23.importFromEPSG(32723);
        utm22.importFromEPSG(32722);
        wgs.SetWellKnownGeogCS("WGS84");
        for (OGRSpatialReference* s : {&utm23, &utm22, &wgs}) s->SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
        std::unique_ptr<OGRCoordinateTransformation> toWgs(OGRCreateCoordinateTransformation(&utm23, &wgs));
        std::unique_ptr<OGRCoordinateTransformation> to22(OGRCreateCoordinateTransformation(&utm23, &utm22));
        if (!toWgs || !to22) return fail("no transformation between UTM 23S, 22S and WGS 84");
        auto lonLat = [&](double px, double py, double& lon, double& lat) {
            B.pixelToGeo(px, py, lon, lat);
            toWgs->Transform(1, &lon, &lat);
        };
        // Points in lon/lat (GeoJSON) and the same ones in a CSV without a CRS.
        std::string gj = "{\"type\": \"FeatureCollection\", \"features\": [\n", csv = "lon,lat,class\n";
        st_.pts.clear();
        for (int i = 0; i < 5; ++i) {
            double lon = 0, lat = 0;
            if (i < 4) {
                lonLat(kPts[i][0], kPts[i][1], lon, lat);
                st_.pts.push_back({kPts[i][0], kPts[i][1]});
            }
            char b[400];
            std::snprintf(b, sizeof(b),
                          "%s{\"type\": \"Feature\", \"properties\": {\"plot\": %d, \"name\": \"p%d\"}, \"geometry\": "
                          "{\"type\": \"Point\", \"coordinates\": [%.10f, %.10f]}}\n",
                          i ? "," : "", 101 + i, i, lon, lat);
            gj += b;
            std::snprintf(b, sizeof(b), "%.10f,%.10f,c%d\n", lon, lat, i);
            if (i < 4) csv += b;
        }
        gj += "]}\n";
        std::string err;
        if (!writeTextFile(file("plots.geojson"), gj, err) || !writeTextFile(file("samples.csv"), csv, err))
            return fail(err.c_str());
        // A polygon with a hole and a line, in UTM 22S (another CRS than B's).
        GDALDriver* gpkg = GetGDALDriverManager()->GetDriverByName("GPKG");
        GDALDataset* ds = gpkg ? gpkg->Create(file("parcels.gpkg").c_str(), 0, 0, 0, GDT_Unknown, nullptr) : nullptr;
        OGRLayer* L = ds ? ds->CreateLayer("parcels", &utm22, wkbUnknown, nullptr) : nullptr;
        if (!L) return fail("the GeoPackage cannot be written");
        OGRFieldDefn fd("use", OFTString);
        L->CreateField(&fd);
        auto ring = [&](const double* v, int n, OGRSimpleCurve& c) {
            for (int k = 0; k < n; ++k) {
                double x, y;
                B.pixelToGeo(v[k * 2], v[k * 2 + 1], x, y);
                to22->Transform(1, &x, &y);
                c.addPoint(x, y);
            }
        };
        OGRPolygon poly;
        OGRLinearRing shell, hole;
        ring(kShell, 4, shell);
        shell.closeRings();
        ring(kHole, 4, hole);
        hole.closeRings();
        poly.addRing(&shell);
        poly.addRing(&hole);
        OGRLineString line;
        ring(kLine, 2, line);
        for (int k = 0; k < 2; ++k) {
            OGRFeature* f = OGRFeature::CreateFeature(L->GetLayerDefn());
            f->SetField("use", k ? "road" : "pasture");
            f->SetGeometry(k ? static_cast<OGRGeometry*>(&line) : &poly);
            L->CreateFeature(f);
            OGRFeature::DestroyFeature(f);
        }
        GDALClose(ds);
        openInputs({file("plots.geojson"), file("parcels.gpkg"), file("samples.csv")}, true); // vectors only
        next("vector files written (GeoJSON in lon/lat, GeoPackage in UTM 22S, CSV) and added");
        break;
    }
    case 121: {
        VectorLayer *P = layerOf("plots.geojson"), *G = layerOf("parcels.gpkg"), *C = layerOf("samples.csv");
        if (!vectorLoads_.empty() || !P || !G || !C || !P->placedFor || !G->placedFor || !C->placedFor) break;
        if (opening_.valid() || layers_.size() != 3) return fail("vector files should not open as rasters");
        std::printf("    %s: %s, %s; %s: %s, %s; %s: %s, %s%s\n", P->name.c_str(), P->data->kindText().c_str(),
                    P->data->crsName.c_str(), G->name.c_str(), G->data->kindText().c_str(), G->data->crsName.c_str(),
                    C->name.c_str(), C->data->kindText().c_str(), C->data->crsName.c_str(),
                    C->data->lonLatAssumed ? " (assumed)" : "");
        if (P->data->points != 5 || G->data->areas != 1 || G->data->lines != 1 || C->data->points != 4 ||
            !C->data->lonLatAssumed || G->data->crsName != "EPSG:32722")
            return fail("the vector files should read as 5 points, a polygon and a line, 4 points");
        if (P->data->fields != std::vector<std::string>{"plot", "name"}) return fail("the GeoJSON's fields");
        double worst = 0;
        for (size_t i = 0; i < st_.pts.size(); ++i) {
            const Ring& p = P->px[i][0];
            const Ring& c = C->px[i][0];
            worst = std::max({worst, std::hypot(p[0] - st_.pts[i][0], p[1] - st_.pts[i][1]),
                              std::hypot(c[0] - st_.pts[i][0], c[1] - st_.pts[i][1])});
        }
        const Polygon& g = G->px[0];
        for (int k = 0; k < 4; ++k)
            worst = std::max({worst, std::hypot(g[0][k * 2] - kShell[k * 2], g[0][k * 2 + 1] - kShell[k * 2 + 1]),
                              std::hypot(g[1][k * 2] - kHole[k * 2], g[1][k * 2 + 1] - kHole[k * 2 + 1])});
        std::printf("    placed on B: max %.2g px from where they were written (lon/lat and UTM 22S to UTM 23S)\n", worst);
        if (worst > 1e-4) return fail("vector features should be placed where they were written");
        const auto& far = P->box[4]; // 0 N 0 E in UTM 23S: thousands of km away, or not transformed
        if (!(far[0] > far[2] || std::fabs(far[0]) > 1e5 || std::fabs(far[1]) > 1e5))
            return fail("the point at 0 N 0 E should be far off B");
        // Picking: a point, inside the polygon, in its hole (the line is far), on the line.
        int vl, vf;
        const bool csvOnTop = vectorFeatureAt(40.6, 50.4, vl, vf) && &vectors_[vl] == (P > C ? P : C);
        C->visible = false; // its points are the GeoJSON's: the one drawn on top is picked
        const bool onPoint = vectorFeatureAt(40.6, 50.4, vl, vf) && &vectors_[vl] == P && vf == 0;
        const bool inArea = vectorFeatureAt(110, 50, vl, vf) && &vectors_[vl] == G && vf == 0;
        const bool inHole = vectorFeatureAt(135, 75, vl, vf);
        const bool onLine = vectorFeatureAt(55, 182.6, vl, vf) && &vectors_[vl] == G && vf == 1;
        C->visible = true;
        std::printf("    picked: the top point %d, a point %d, the polygon %d, its hole %d, the line %d\n", csvOnTop,
                    onPoint, inArea, inHole, onLine);
        if (!csvOnTop || !onPoint || !inArea || inHole || !onLine)
            return fail("picking: the top layer's point, a point, the polygon, not its hole, the line");
        if (!featureAsRoi(*G, 0) || roiPoly_.size() != 2) return fail("the polygon with its hole should become the ROI");
        // The line (Alt + click on it): the transect along it, written in UTM 22S.
        if (featureAsRoi(*G, 1) || !featureAsTransect(*G, 1)) return fail("the line should become the transect, not the ROI");
        const double lineLen = std::hypot(kLine[2] - kLine[0], kLine[3] - kLine[1]) * 30;
        std::printf("    transect along the line: %zu vertices, (%.3f, %.3f) - (%.3f, %.3f), %.1f %s (%.1f m drawn), %d samples\n",
                    tr_.pts.size(), tr_.a.x, tr_.a.y, tr_.b.x, tr_.b.y, tr_.length, tr_.unit.c_str(), lineLen, tr_.n);
        if (!tr_.on || std::hypot(tr_.a.x - kLine[0], tr_.a.y - kLine[1]) > 1e-3 ||
            std::hypot(tr_.b.x - kLine[2], tr_.b.y - kLine[3]) > 1e-3 || std::fabs(tr_.length - lineLen) > 0.01 ||
            tr_.unit != "m")
            return fail("the transect should follow the line from end to end");
        clearTransect();
        next("placed on B, picked; the polygon (with its hole) as the ROI, the line as the transect");
        break;
    }
    case 122: {
        const int T = s_->info->T();
        if (!roi_ || roi_->done.load() < T) break;
        size_t inside = 0;
        for (char c : roi_->mask) inside += c != 0;
        if (inside != 80 * 70 - 30 * 30) return fail("the ROI of the polygon should be its pixels less the hole's");
        std::printf("    ROI: %zu pixels (the 80 x 70 shell less the 30 x 30 hole)\n", inside);
        VectorLayer *P = layerOf("plots.geojson"), *G = layerOf("parcels.gpkg");
        st_.jobs = {extractSeries(*P, true, true, file("plots_series.csv")),
                    extractSeries(*G, false, false, file("parcels_series.csv"))};
        PngOptions o;
        o.marks = true;
        o.label = o.legend = false;
        st_.jobs.push_back(exportPng(file("vectors.png"), o));
        next("series extracted (points: every layer, wide; the polygon: long) and a PNG");
        break;
    }
    case 123: {
        for (auto& j : st_.jobs) {
            j->done.wait();
            if (j->state != ExportJob::State::Done) {
                std::lock_guard<std::mutex> lk(j->m);
                std::printf("    %s: %s\n", j->title.c_str(), j->error.c_str());
                return fail("an extraction or the PNG failed");
            }
        }
        const SeriesLayer &B = layers_[0], &A = layers_[1];
        const int T = B.session->info->T(), TA = A.session->info->T(), TF = layers_[2].session->info->T();
        // Wide: fid, plot, name, lon, lat, then B's dates, A's, F's.
        const auto w = readCsv(file("plots_series.csv"));
        if (w.size() != 6 || w[0].size() != size_t(5 + T + TA + TF)) return fail("the wide table's shape");
        if (w[0][1] != "plot" || w[0][5] != B.name + " " + B.session->info->layers[0].label)
            return fail("the wide table's header");
        CubeReader rb(B.session->info), ra(A.session->info);
        int compared = 0;
        for (int i = 0; i < 4; ++i) {
            const int x = int(kPts[i][0]), y = int(kPts[i][1]);
            for (int t = 0; t < T; ++t) {
                float v = NAN;
                rb.readPixel(t, x, y, v);
                const std::string& got = w[i + 1][5 + t];
                if (std::isnan(v) != got.empty() || (!got.empty() && std::fabs(std::stod(got) - v) > 1e-6 * (1 + std::fabs(v))))
                    return fail("a point's series on B differs from the file");
                compared += !got.empty();
            }
            for (int t = 0; t < TA; ++t) { // A = B + 100 px: the second point is off A
                float v = NAN;
                if (x + 100 < A.session->info->width) ra.readPixel(t, x + 100, y, v);
                const std::string& got = w[i + 1][5 + T + t];
                if (std::isnan(v) != got.empty() || (!got.empty() && std::fabs(std::stod(got) - v) > 1e-6 * (1 + std::fabs(v))))
                    return fail("a point's series on A differs from the file");
            }
        }
        for (size_t k = 5; k < w[5].size(); ++k)
            if (!w[5][k].empty()) return fail("the point far away should have no values");
        // Long: one row per date, the polygon's mean (= the ROI's) and its pixels; the line left out.
        const auto l = readCsv(file("parcels_series.csv"));
        if (l.size() != size_t(1 + T) || l[0].back() != "pixels") return fail("the long table's shape");
        double worst = 0;
        for (int t = 0; t < T; ++t) {
            const std::string& got = l[1 + t][l[0].size() - 2];
            const float e = roi_->perT[t].n ? roi_->perT[t].mean : NAN;
            if (std::isnan(e) != got.empty()) return fail("the polygon's mean: no data where the ROI has some");
            if (!got.empty()) worst = std::max(worst, std::fabs(std::stod(got) - e));
            if (l[1 + t].back() != "4700") return fail("the polygon's pixels should be 4700");
        }
        std::printf("    wide: %d values of B compared, A's and the far point's gaps empty; long: |mean - ROI| %.2g\n",
                    compared, worst);
        if (worst > 1e-4) return fail("the polygon's mean should be the ROI's");
        // The PNG: a point in the GeoJSON's colour.
        GDALDataset* ds = GDALDataset::Open(file("vectors.png").c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds) return fail("the PNG cannot be opened");
        VectorLayer *P = layerOf("plots.geojson"), *C = layerOf("samples.csv");
        const int X = int((offset_.x + kPts[2][0] * scale_) * mapPixelScale_), Y = int((offset_.y + kPts[2][1] * scale_) * mapPixelScale_);
        unsigned char c[3] = {0, 0, 0};
        int bands[3] = {1, 2, 3};
        ds->RasterIO(GF_Read, X, Y, 1, 1, c, 1, 1, GDT_Byte, 3, bands, 3, 3, 1, nullptr);
        GDALClose(ds);
        const ImVec4 e = (P > C ? P : C)->color; // the CSV's points are the GeoJSON's: the one on top shows
        std::printf("    PNG at the third point: rgb(%d, %d, %d), its layer's colour rgb(%d, %d, %d)\n", c[0], c[1], c[2],
                    int(e.x * 255), int(e.y * 255), int(e.z * 255));
        if (std::abs(c[0] - int(e.x * 255)) > 40 || std::abs(c[1] - int(e.y * 255)) > 40 || std::abs(c[2] - int(e.z * 255)) > 40)
            return fail("the PNG should show the point in its layer's colour");
        // Interpretation of the points: the first one shown, its pin, the map on it.
        startInterpretation(P->id);
        if (lab_.current != 0 || !lab_.pinId) return fail("the interpretation should start at the first point, with a pin");
        const ImVec2 a = P->anchor[0];
        const double cx = offset_.x + a.x * scale_, cy = offset_.y + a.y * scale_;
        if (std::fabs(cx - canvasSize_.x / 2) > 1 || std::fabs(cy - canvasSize_.y / 2) > 1 || scale_ < lab_.zoom - 1e-6)
            return fail("the map should be centred on the point, zoomed in");
        ImGui::GetIO().AddKeyEvent(ImGuiKey_N, true); // as typed: N, then 2, then D
        openExtract(P->id);                           // its popup drawn once, then closed
        st_.hits = lab_.pinId;
        st_.frames = 0;
        next("tables match the files; the PNG; interpretation started on the first point");
        break;
    }
    case 124: {
        ImGuiIO& io = ImGui::GetIO();
        if (++st_.frames == 1) {
            io.AddKeyEvent(ImGuiKey_N, false);
            if (lab_.current != 1) return fail("N should go to the next point");
            const SeriesView* pin = nullptr;
            for (const SeriesView& p : pins_)
                if (p.id == lab_.pinId) pin = &p;
            const bool oldGone = std::none_of(pins_.begin(), pins_.end(), [&](const SeriesView& p) { return p.id == st_.hits; });
            if (!pin || pin->x != 200 || pin->y != 120 || !oldGone) return fail("the pin should move to the point");
            if (!ImGui::IsPopupOpen("Extract series###extract")) return fail("the Extract series popup should open");
            ImGui::ClosePopupToLevel(0, true);
            io.AddKeyEvent(ImGuiKey_2, true);
            break;
        }
        if (st_.frames == 2) {
            io.AddKeyEvent(ImGuiKey_2, false);
            if (lab_.entries[1].label != lab_.classes[1]) return fail("2 should give the second label");
            setT(3);
            io.AddKeyEvent(ImGuiKey_D, true);
            break;
        }
        io.AddKeyEvent(ImGuiKey_D, false);
        if (lab_.entries[1].date != s_->info->layers[3].label) return fail("D should set the date of the change");
        std::snprintf(lab_.notes, sizeof(lab_.notes), "edge, cloud in 2003");
        lab_.entries[1].notes = lab_.notes;
        saveLabels();
        const std::string saved = labelsFile();
        // Resumed: stop, start again (from the file in the app data).
        const int id = lab_.vector, pinId = lab_.pinId;
        stopInterpretation();
        if (std::any_of(pins_.begin(), pins_.end(), [&](const SeriesView& p) { return p.id == pinId; }))
            return fail("stopping should remove its pin");
        startInterpretation(id);
        if (lab_.entries[1].label != lab_.classes[1] || lab_.entries[1].notes != "edge, cloud in 2003" ||
            lab_.current != 0)
            return fail("the labels should be resumed (and the first unlabelled point shown)");
        st_.jobs = {exportLabels(file("labels.gpkg")), exportLabels(file("labels.csv"))};
        st_.reveal = saved;
        st_.frames = 0;
        next("N, 2 and D as typed; the labels saved and resumed; exported");
        break;
    }
    case 125: {
        for (auto& j : st_.jobs) {
            j->done.wait();
            if (j->state != ExportJob::State::Done) {
                std::lock_guard<std::mutex> lk(j->m);
                std::printf("    %s: %s\n", j->title.c_str(), j->error.c_str());
                return fail("the labels' export failed");
            }
        }
        // GeoPackage: the points with their fields and the three new ones.
        GDALDataset* ds = static_cast<GDALDataset*>(GDALOpenEx(file("labels.gpkg").c_str(), GDAL_OF_VECTOR, nullptr, nullptr, nullptr));
        OGRLayer* L = ds ? ds->GetLayer(0) : nullptr;
        bool ok = L && L->GetFeatureCount() == 5;
        int k = 0;
        if (ok) {
            L->ResetReading();
            while (OGRFeature* f = L->GetNextFeature()) {
                const std::string label = f->GetFieldAsString("label"), date = f->GetFieldAsString("change_date");
                const bool want = k == 1;
                ok = ok && (want ? label == lab_.classes[1] && date == s_->info->layers[3].label &&
                                       std::string(f->GetFieldAsString("notes")) == "edge, cloud in 2003"
                                 : label.empty()) &&
                     f->GetFieldAsInteger("plot") == 101 + k && f->GetGeometryRef();
                OGRFeature::DestroyFeature(f);
                ++k;
            }
        }
        if (ds) GDALClose(ds);
        if (!ok) return fail("the GeoPackage of labels should hold the points, their fields and the labels");
        const auto c = readCsv(file("labels.csv"));
        if (c.size() != 6 || c[0].back() != "notes" || c[2][c[0].size() - 3] != lab_.classes[1] || c[2][3].empty())
            return fail("the CSV of labels should hold the label and lon/lat");
        // A polygon in the interpretation: it becomes the ROI.
        startInterpretation(layerOf("parcels.gpkg")->id);
        if (lab_.current != 0 || roiPoly_.size() != 2 || lab_.pinId) return fail("a polygon should be shown as the ROI");
        stopInterpretation();
        std::error_code ec;
        fs::remove(fs::u8path(st_.reveal), ec);
        while (!vectors_.empty()) removeVector(0);
        clearRoi();
        fs::remove_all(fs::u8path(st_.dir), ec);
        st_.reveal.clear();
        next("labels exported (GeoPackage with the geometries, CSV with lon/lat); a polygon interpreted as the ROI");
        st_.stage = 130; // the animation export
        break;
    }
    }
    return -1;
}
