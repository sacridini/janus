// Interpretation: a vector layer's features gone through one by one, as
// TimeSync does with Landsat: the map centred on each (zoomed in), its series
// on the chart (a pin at a point; a polygon becomes the ROI), a label from the
// label set (keys 1-9), the date of the change (D: the date on the map) and
// notes. Labels are saved as they are given (in the app data, by file and
// layer: opening the same file again resumes the work) and exported as a
// table (CSV, GeoPackage, GeoJSON, Shapefile) with the file's own fields.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <map>

#include <imgui_internal.h>

#include "platform.hpp"

namespace fs = std::filesystem;

namespace {

const std::vector<std::string> kDefaultClasses = {"Stable forest", "Deforestation", "Degradation",
                                                  "Regeneration",  "Agriculture",   "Pasture",
                                                  "Water",         "Cloud / no data", "Other"};

} // namespace

void App::startInterpretation(int vectorId) {
    showInterp_ = true;
    if (lab_.vector == vectorId) return;
    const VectorLayer* V = vectorById(vectorId);
    if (!V) return;
    stopInterpretation();
    lab_.vector = vectorId;
    lab_.classes = kDefaultClasses;
    lab_.entries.assign(V->data->features.size(), LabelEntry{});
    loadLabels();
    // Where the work stopped: the first feature without a label.
    int first = -1;
    for (size_t i = 0; i < lab_.entries.size() && first < 0; ++i)
        if (V->data->features[i].kind != VectorFeature::None && lab_.entries[i].label.empty()) first = int(i);
    for (size_t i = 0; i < lab_.entries.size() && first < 0; ++i)
        if (V->data->features[i].kind != VectorFeature::None) first = int(i);
    if (first >= 0) interpGo(first);
}

void App::stopInterpretation() {
    if (!lab_.vector) return;
    if (lab_.dirty) saveLabels();
    if (lab_.pinId) removePinById(lab_.pinId);
    lab_ = Interpretation{};
}

void App::interpGo(int feature) {
    const VectorLayer* V = vectorById(lab_.vector);
    if (!V || !s_ || feature < 0 || feature >= int(V->data->features.size())) return;
    const VectorFeature& F = V->data->features[feature];
    if (F.kind == VectorFeature::None) return;
    // The previous feature's polygon was made the ROI here: it goes with it.
    if (lab_.current >= 0 && lab_.current < int(V->data->features.size()) &&
        V->data->features[lab_.current].kind == VectorFeature::Area)
        clearRoi();
    lab_.current = feature;
    std::snprintf(lab_.notes, sizeof(lab_.notes), "%s", lab_.entries[feature].notes.c_str());
    lab_.scrollToCurrent = true;
    if (lab_.pinId) removePinById(lab_.pinId);
    lab_.pinId = 0;
    // A polygon: its mean as the ROI's series; anything else: a pin at its point.
    if (F.kind != VectorFeature::Area || !featureAsRoi(*V, feature)) {
        const ImVec2 a = V->anchor.empty() ? ImVec2(NAN, NAN) : V->anchor[feature];
        const CubeInfo& info = *s_->info;
        if (!std::isnan(a.x) && a.x >= 0 && a.y >= 0 && a.x < info.width && a.y < info.height) {
            addPin(int(a.x), int(a.y));
            addOtherPins(pins_.back());
            lab_.pinId = pins_.back().id;
        }
    }
    centerOnFeature(*V, feature, lab_.zoom);
}

void App::interpStep(int dir) {
    const VectorLayer* V = vectorById(lab_.vector);
    if (!V) return;
    const int n = int(V->data->features.size());
    for (int i = lab_.current + dir; i >= 0 && i < n; i += dir) {
        if (V->data->features[i].kind == VectorFeature::None) continue;
        if (lab_.skipLabelled && !lab_.entries[i].label.empty()) continue;
        interpGo(i);
        return;
    }
}

void App::interpLabel(int k) {
    if (lab_.current < 0 || lab_.current >= int(lab_.entries.size()) || k >= int(lab_.classes.size())) return;
    lab_.entries[lab_.current].label = k < 0 ? "" : lab_.classes[k];
    lab_.dirty = true;
    lab_.changedAt = ImGui::GetTime();
    if (k >= 0 && lab_.advance) interpStep(+1);
}

void App::interpSetDate(bool clear) {
    if (!s_ || lab_.current < 0 || lab_.current >= int(lab_.entries.size())) return;
    lab_.entries[lab_.current].date = clear ? "" : s_->info->layers[t_].label;
    lab_.dirty = true;
    lab_.changedAt = ImGui::GetTime();
}

void App::interpShortcuts() {
    if (!showInterp_ || !lab_.vector) return;
    if (ImGui::IsKeyChordPressed(ImGuiKey_N) || ImGui::IsKeyPressed(ImGuiKey_PageDown)) interpStep(+1);
    if (ImGui::IsKeyChordPressed(ImGuiMod_Shift | ImGuiKey_N) || ImGui::IsKeyPressed(ImGuiKey_PageUp)) interpStep(-1);
    for (int k = 0; k < 9; ++k)
        if (ImGui::IsKeyChordPressed(ImGuiKey(ImGuiKey_1 + k)) || ImGui::IsKeyChordPressed(ImGuiKey(ImGuiKey_Keypad1 + k)))
            interpLabel(k);
    if (ImGui::IsKeyChordPressed(ImGuiKey_0) || ImGui::IsKeyChordPressed(ImGuiKey_Keypad0)) interpLabel(-1);
    if (ImGui::IsKeyChordPressed(ImGuiKey_D)) interpSetDate(false);
    if (ImGui::IsKeyChordPressed(ImGuiMod_Shift | ImGuiKey_D)) interpSetDate(true);
}

void App::pumpInterpretation() {
    if (lab_.vector && !vectorById(lab_.vector)) stopInterpretation();
    if (lab_.dirty && ImGui::GetTime() - lab_.changedAt > 1.0) saveLabels(); // a second after the last change
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

std::string App::labelsFile() const {
    const VectorLayer* V = const_cast<App*>(this)->vectorById(lab_.vector);
    if (!V) return "";
    std::error_code ec;
    const std::string abs = fs::absolute(fs::u8path(V->data->path), ec).lexically_normal().u8string();
    char hash[24];
    std::snprintf(hash, sizeof(hash), "%016llx", (unsigned long long)std::hash<std::string>()(abs + "|" + V->data->layer));
    std::string stem = fs::u8path(V->data->path).stem().u8string();
    for (char& c : stem)
        if (std::strchr("\\/:*?\"<>| ", c)) c = '_';
    return (fs::u8path(platform::appDataDir()) / "labels" / fs::u8path(stem + "_" + hash + ".json")).u8string();
}

void App::saveLabels() {
    lab_.dirty = false;
    const VectorLayer* V = vectorById(lab_.vector);
    const std::string path = labelsFile();
    if (!V || path.empty()) return;
    json j;
    j["path"] = V->data->path;
    j["layer"] = V->data->layer;
    j["classes"] = lab_.classes;
    json e = json::array();
    for (size_t i = 0; i < lab_.entries.size(); ++i) {
        const LabelEntry& L = lab_.entries[i];
        if (L.label.empty() && L.date.empty() && L.notes.empty()) continue;
        e.push_back({{"fid", V->data->features[i].fid}, {"label", L.label}, {"date", L.date}, {"notes", L.notes}});
    }
    j["entries"] = std::move(e);
    std::error_code ec;
    fs::create_directories(fs::u8path(path).parent_path(), ec);
    const std::string tmp = path + ".part";
    {
        std::ofstream out(fs::u8path(tmp), std::ios::binary | std::ios::trunc);
        out << j.dump(1);
        if (!out) return;
    }
    fs::remove(fs::u8path(path), ec);
    fs::rename(fs::u8path(tmp), fs::u8path(path), ec);
}

void App::loadLabels() {
    const VectorLayer* V = vectorById(lab_.vector);
    const std::string path = labelsFile();
    std::error_code ec;
    if (!V || path.empty() || !fs::exists(fs::u8path(path), ec)) return;
    try {
        std::ifstream in(fs::u8path(path), std::ios::binary);
        const json j = json::parse(in);
        if (j.contains("classes") && j["classes"].is_array() && !j["classes"].empty())
            lab_.classes = j["classes"].get<std::vector<std::string>>();
        std::map<int64_t, size_t> byFid;
        for (size_t i = 0; i < V->data->features.size(); ++i) byFid[V->data->features[i].fid] = i;
        for (const json& e : j.value("entries", json::array())) {
            const auto it = byFid.find(e.value("fid", int64_t(-1)));
            if (it == byFid.end()) continue;
            LabelEntry& L = lab_.entries[it->second];
            L.label = e.value("label", std::string());
            L.date = e.value("date", std::string());
            L.notes = e.value("notes", std::string());
        }
    } catch (const std::exception&) {
        // A damaged file: the session starts empty (the file is replaced at the first label).
    }
}

std::shared_ptr<ExportJob> App::exportLabels(const std::string& path) {
    const VectorLayer* V = vectorById(lab_.vector);
    if (!V) return nullptr;
    auto data = V->data;
    auto entries = std::make_shared<std::vector<LabelEntry>>(lab_.entries);
    int labelled = 0;
    for (const LabelEntry& e : *entries) labelled += !e.label.empty();
    char title[256];
    std::snprintf(title, sizeof(title), "Labels of %s (%d of %zu features)", V->name.c_str(), labelled, entries->size());
    auto job = startExportJob(title, path, [data, entries, path](ExportJob& j, std::string& error) {
        setExportProgress(j, 0.5);
        return writeVectorTable(path, *data, {"label", "change_date", "notes"}, [&](size_t i, size_t k) {
            const LabelEntry& e = (*entries)[i];
            return k == 0 ? e.label : k == 1 ? e.date : e.notes;
        }, error);
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

void App::uiInterpretation() {
    if (!showInterp_) return;
    // The first time: a tab next to Statistics, beside the chart.
    if (const ImGuiWindow* st = ImGui::FindWindowByName("Statistics"); st && st->DockId)
        ImGui::SetNextWindowDockID(st->DockId, ImGuiCond_FirstUseEver);
    ImGui::SetNextWindowSize(ImVec2(420, 520), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Interpretation", &showInterp_)) {
        ImGui::End();
        return;
    }
    const VectorLayer* V = vectorById(lab_.vector);
    if (!V) {
        ImGui::TextWrapped("Label the features of a vector layer one by one: the map goes to each, its series "
                           "is drawn on the chart, a key gives its label.");
        ImGui::Spacing();
        if (vectors_.empty()) ImGui::TextDisabled("Add a vector layer first (File > Add vector layer...).");
        for (const VectorLayer& W : vectors_) {
            ImGui::PushID(W.id);
            if (ImGui::Button("Start")) startInterpretation(W.id);
            ImGui::SameLine();
            ImGui::TextColored(W.color, "%s", W.name.c_str());
            ImGui::SameLine();
            ImGui::TextDisabled("(%s)", W.data->kindText().c_str());
            ImGui::PopID();
        }
        ImGui::End();
        return;
    }
    const VectorData& D = *V->data;
    const int n = int(D.features.size());
    int labelled = 0;
    for (const LabelEntry& e : lab_.entries) labelled += !e.label.empty();
    ImGui::TextColored(V->color, "%s", V->name.c_str());
    ImGui::SameLine();
    if (ImGui::SmallButton("Stop")) {
        stopInterpretation();
        ImGui::End();
        return;
    }
    ImGui::SetItemTooltip("End this session (its labels stay saved: starting again resumes it)");
    char overlay[64];
    std::snprintf(overlay, sizeof(overlay), "%d of %d labelled", labelled, n);
    ImGui::ProgressBar(n ? float(labelled) / n : 0.0f, ImVec2(-1, 0), overlay);

    // --- The feature shown ---
    if (ImGui::ArrowButton("##prev", ImGuiDir_Left)) interpStep(-1);
    ImGui::SetItemTooltip("Previous (Shift+N, Page Up)");
    ImGui::SameLine();
    if (ImGui::ArrowButton("##next", ImGuiDir_Right)) interpStep(+1);
    ImGui::SetItemTooltip("Next (N, Page Down)");
    ImGui::SameLine();
    if (lab_.current >= 0)
        ImGui::Text("%d / %d   (fid %lld)", lab_.current + 1, n, (long long)D.features[lab_.current].fid);
    ImGui::Checkbox("Skip labelled", &lab_.skipLabelled);
    ImGui::SameLine();
    ImGui::Checkbox("Next after a label", &lab_.advance);
    ImGui::SetNextItemWidth(-1);
    ImGui::SliderFloat("##zoom", &lab_.zoom, 1.0f, 32.0f, "zoom on each: %.0f points per pixel", ImGuiSliderFlags_Logarithmic);

    if (lab_.current >= 0 && lab_.current < n) {
        LabelEntry& E = lab_.entries[lab_.current];
        ImGui::SeparatorText("Label (keys 1-9, 0 clears)");
        const float bw = (ImGui::GetContentRegionAvail().x - ImGui::GetStyle().ItemSpacing.x * 2) / 3;
        for (int k = 0; k < int(lab_.classes.size()); ++k) {
            const bool on = E.label == lab_.classes[k];
            char text[96];
            if (k < 9) std::snprintf(text, sizeof(text), "%d  %s##c%d", k + 1, lab_.classes[k].c_str(), k);
            else std::snprintf(text, sizeof(text), "%s##c%d", lab_.classes[k].c_str(), k);
            if (on) ImGui::PushStyleColor(ImGuiCol_Button, ImGui::GetStyleColorVec4(ImGuiCol_ButtonActive));
            if (ImGui::Button(text, ImVec2(bw, 0))) interpLabel(on ? -1 : k);
            if (on) ImGui::PopStyleColor();
            if (k % 3 != 2 && k + 1 < int(lab_.classes.size())) ImGui::SameLine();
        }
        ImGui::SeparatorText("Date of the change");
        ImGui::Text("%s", E.date.empty() ? "(none)" : E.date.c_str());
        ImGui::SameLine();
        if (ImGui::SmallButton("Map's date (D)")) interpSetDate(false);
        ImGui::SetItemTooltip("The date shown on the map: go to it with the arrows or a click on the chart");
        ImGui::SameLine();
        if (ImGui::SmallButton("Clear (Shift+D)")) interpSetDate(true);
        ImGui::SeparatorText("Notes");
        if (ImGui::InputTextMultiline("##notes", lab_.notes, sizeof(lab_.notes), ImVec2(-1, ImGui::GetTextLineHeight() * 3))) {
            E.notes = lab_.notes;
            lab_.dirty = true;
            lab_.changedAt = ImGui::GetTime();
        }
        const VectorFeature& F = D.features[lab_.current];
        if (!D.fields.empty() && ImGui::CollapsingHeader("Attributes")) {
            if (ImGui::BeginTable("##attrs", 2, ImGuiTableFlags_SizingFixedFit | ImGuiTableFlags_RowBg)) {
                for (size_t k = 0; k < D.fields.size(); ++k) {
                    ImGui::TableNextRow();
                    ImGui::TableNextColumn();
                    ImGui::TextDisabled("%s", D.fields[k].c_str());
                    ImGui::TableNextColumn();
                    ImGui::TextUnformatted(F.values[k].c_str());
                }
                ImGui::EndTable();
            }
        }
    }
    if (ImGui::CollapsingHeader("Label set")) {
        int remove = -1;
        for (int k = 0; k < int(lab_.classes.size()); ++k) {
            ImGui::PushID(k);
            if (ImGui::SmallButton("x")) remove = k;
            ImGui::SameLine();
            ImGui::Text("%s%s", k < 9 ? (std::to_string(k + 1) + "  ").c_str() : "   ", lab_.classes[k].c_str());
            ImGui::PopID();
        }
        if (remove >= 0) {
            lab_.classes.erase(lab_.classes.begin() + remove);
            lab_.dirty = true;
            lab_.changedAt = ImGui::GetTime();
        }
        ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x - 60);
        const bool enter = ImGui::InputText("##new", lab_.newClass, sizeof(lab_.newClass), ImGuiInputTextFlags_EnterReturnsTrue);
        ImGui::SameLine();
        if ((ImGui::Button("Add") || enter) && lab_.newClass[0] &&
            std::find(lab_.classes.begin(), lab_.classes.end(), std::string(lab_.newClass)) == lab_.classes.end()) {
            lab_.classes.push_back(lab_.newClass);
            lab_.newClass[0] = 0;
            lab_.dirty = true;
            lab_.changedAt = ImGui::GetTime();
        }
        ImGui::TextDisabled("Labels already given keep their text when the set changes.");
    }
    ImGui::SeparatorText("Features");
    if (ImGui::Button("Export table...")) {
        const std::string stem = fs::u8path(D.path).stem().u8string() + "_labels";
        const std::string p = askSavePath("Export the labels (.csv, .gpkg, .geojson or .shp)", stem + ".gpkg",
                                          "GeoPackage", "gpkg");
        if (!p.empty()) exportLabels(p);
    }
    ImGui::SetItemTooltip("The file's features and fields with label, change_date and notes:\n"
                          "GeoPackage, GeoJSON or Shapefile (with the geometries), or CSV (with lon/lat)");
    ImGui::SameLine();
    ImGui::TextDisabled("saved as you go");
    ImGui::SetItemTooltip("%s", labelsFile().c_str());
    if (ImGui::BeginTable("##features", 4, ImGuiTableFlags_ScrollY | ImGuiTableFlags_RowBg | ImGuiTableFlags_BordersInnerV,
                          ImVec2(0, std::max(120.0f, ImGui::GetContentRegionAvail().y)))) {
        ImGui::TableSetupScrollFreeze(0, 1);
        ImGui::TableSetupColumn("#", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("label");
        ImGui::TableSetupColumn("date", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("notes");
        ImGui::TableHeadersRow();
        ImGuiListClipper clip;
        clip.Begin(n);
        if (lab_.scrollToCurrent && lab_.current >= 0) clip.IncludeItemByIndex(lab_.current);
        while (clip.Step())
            for (int i = clip.DisplayStart; i < clip.DisplayEnd; ++i) {
                const LabelEntry& e = lab_.entries[i];
                ImGui::TableNextRow();
                ImGui::TableNextColumn();
                char id[32];
                std::snprintf(id, sizeof(id), "%d##row", i + 1);
                if (ImGui::Selectable(id, i == lab_.current, ImGuiSelectableFlags_SpanAllColumns)) interpGo(i);
                if (i == lab_.current && lab_.scrollToCurrent) {
                    ImGui::SetScrollHereY(0.5f);
                    lab_.scrollToCurrent = false;
                }
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(e.label.c_str());
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(e.date.c_str());
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(e.notes.c_str());
            }
        ImGui::EndTable();
    }
    ImGui::End();
}
