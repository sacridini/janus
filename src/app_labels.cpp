// Interpretation: a vector layer's features gone through one by one, as
// TimeSync does with Landsat: the map centred on each (zoomed in), its series
// on the chart (a pin at a point; a polygon becomes the ROI), a label from the
// label set (keys 1-9), the date of the change (D: the date on the map) and
// notes, its confidence and more events. Labels are saved as they are given
// (in the app data, by file and layer: opening the same file again resumes
// the work) and exported as a table (CSV, GeoPackage, GeoJSON, Shapefile) with
// the file's own fields; the sample of a validation project saves them in its
// GeoPackage instead (app_validation.cpp).
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
    if (isProjectVector(vectorId)) {
        lab_.project = true;
        lab_.as = interpreterName();
        validationEntries();
    } else {
        loadLabels();
    }
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
    if (!lab_.filter.empty()) { // only the features of the filter, in its order
        const auto& f = lab_.filter;
        const auto it = std::find(f.begin(), f.end(), lab_.current);
        const int pos = it != f.end() ? int(it - f.begin()) : dir > 0 ? -1 : int(f.size());
        for (int k = pos + dir; k >= 0 && k < int(f.size()); k += dir) {
            const int i = f[k];
            if (i < 0 || i >= int(lab_.entries.size()) || V->data->features[i].kind == VectorFeature::None) continue;
            if (lab_.skipLabelled && !lab_.entries[i].label.empty()) continue;
            interpGo(i);
            return;
        }
        return;
    }
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
    interpTouched();
    if (k >= 0 && lab_.advance) interpStep(+1);
}

void App::interpSetDate(bool clear) {
    if (!s_ || lab_.current < 0 || lab_.current >= int(lab_.entries.size())) return;
    lab_.entries[lab_.current].date = clear ? "" : s_->info->layers[t_].label;
    interpTouched();
}

void App::interpSetConfidence(int c) {
    if (lab_.current < 0 || lab_.current >= int(lab_.entries.size())) return;
    lab_.entries[lab_.current].confidence = std::clamp(c, 0, 3);
    interpTouched();
}

void App::interpAddEvent(const std::string& label) {
    if (!s_ || label.empty() || lab_.current < 0 || lab_.current >= int(lab_.entries.size())) return;
    lab_.entries[lab_.current].events.push_back({label, s_->info->layers[t_].label});
    interpTouched();
}

void App::interpTouched() {
    if (lab_.current < 0 || lab_.current >= int(lab_.entries.size())) return;
    lab_.entries[lab_.current].updated = val::nowIso();
    lab_.dirty = true;
    lab_.changedAt = ImGui::GetTime();
    if (lab_.project) validationLabelsChanged(lab_.current);
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
    for (int c = 1; c <= 3; ++c) // Shift+1-3: low, medium, high confidence
        if (ImGui::IsKeyChordPressed(ImGuiMod_Shift | ImGuiKey(ImGuiKey_0 + c))) interpSetConfidence(c);
    if (ImGui::IsKeyChordPressed(ImGuiKey_E) && lab_.current >= 0 && lab_.current < int(lab_.entries.size())) {
        // An event: the label chosen for events, else the feature's label, at the map's date.
        const std::string l = lab_.eventLabel[0] ? std::string(lab_.eventLabel) : lab_.entries[lab_.current].label;
        interpAddEvent(l);
    }
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
    if (lab_.project) {
        validationSaveLabels();
        return;
    }
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
        if (L.empty()) continue;
        json o = val::labelToJson(L);
        o["fid"] = V->data->features[i].fid;
        e.push_back(std::move(o));
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
            lab_.entries[it->second] = val::labelFromJson(e);
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

    if (lab_.project) uiInterpretationProject();
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
        // Confidence: how sure the interpreter is (kept with the label).
        ImGui::AlignTextToFramePadding();
        ImGui::TextUnformatted("Confidence");
        static const char* kConf[] = {"low", "medium", "high"};
        for (int c = 1; c <= 3; ++c) {
            ImGui::SameLine();
            const bool on = E.confidence == c;
            if (on) ImGui::PushStyleColor(ImGuiCol_Button, ImGui::GetStyleColorVec4(ImGuiCol_ButtonActive));
            char t[32];
            std::snprintf(t, sizeof(t), "%s##conf%d", kConf[c - 1], c);
            if (ImGui::SmallButton(t)) interpSetConfidence(on ? 0 : c);
            if (on) ImGui::PopStyleColor();
            ImGui::SetItemTooltip("Shift+%d", c);
        }
        // More events at the feature (e.g. degraded, then cleared): a label and a date each.
        if (ImGui::TreeNodeEx("##events", ImGuiTreeNodeFlags_SpanAvailWidth | (E.events.empty() ? 0 : ImGuiTreeNodeFlags_DefaultOpen),
                              "Other events (%d)", int(E.events.size()))) {
            int remove = -1;
            for (int k = 0; k < int(E.events.size()); ++k) {
                ImGui::PushID(k);
                if (ImGui::SmallButton("x")) remove = k;
                ImGui::SameLine();
                ImGui::Text("%s  %s", E.events[k].date.c_str(), E.events[k].label.c_str());
                ImGui::PopID();
            }
            if (remove >= 0) {
                E.events.erase(E.events.begin() + remove);
                interpTouched();
            }
            ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x * 0.5f);
            if (ImGui::BeginCombo("##evlabel", lab_.eventLabel[0] ? lab_.eventLabel : "(the label)")) {
                if (ImGui::Selectable("(the label)", !lab_.eventLabel[0])) lab_.eventLabel[0] = 0;
                for (const std::string& c : lab_.classes)
                    if (ImGui::Selectable(c.c_str(), c == lab_.eventLabel))
                        std::snprintf(lab_.eventLabel, sizeof(lab_.eventLabel), "%s", c.c_str());
                ImGui::EndCombo();
            }
            ImGui::SameLine();
            if (ImGui::SmallButton("Add at the map's date (E)"))
                interpAddEvent(lab_.eventLabel[0] ? std::string(lab_.eventLabel) : E.label);
            ImGui::TreePop();
        }
        ImGui::SeparatorText("Notes");
        if (ImGui::InputTextMultiline("##notes", lab_.notes, sizeof(lab_.notes), ImVec2(-1, ImGui::GetTextLineHeight() * 3))) {
            E.notes = lab_.notes;
            interpTouched();
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
        bool changed = false;
        if (remove >= 0) {
            lab_.classes.erase(lab_.classes.begin() + remove);
            changed = true;
        }
        ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x - 60);
        const bool enter = ImGui::InputText("##new", lab_.newClass, sizeof(lab_.newClass), ImGuiInputTextFlags_EnterReturnsTrue);
        ImGui::SameLine();
        if ((ImGui::Button("Add") || enter) && lab_.newClass[0] &&
            std::find(lab_.classes.begin(), lab_.classes.end(), std::string(lab_.newClass)) == lab_.classes.end()) {
            lab_.classes.push_back(lab_.newClass);
            lab_.newClass[0] = 0;
            changed = true;
        }
        if (changed) {
            if (lab_.project) {
                validationLabelSetChanged();
            } else {
                lab_.dirty = true;
                lab_.changedAt = ImGui::GetTime();
            }
        }
        ImGui::TextDisabled("Labels already given keep their text when the set changes.");
    }
    ImGui::SeparatorText(lab_.filter.empty() ? "Features" : ("Features: " + lab_.filterName).c_str());
    if (!lab_.filter.empty()) {
        if (ImGui::SmallButton("Show all")) {
            lab_.filter.clear();
            lab_.filterName.clear();
        }
        ImGui::SameLine();
    }
    if (!lab_.project) {
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
    } else {
        ImGui::TextDisabled("saved in the project as you go");
        ImGui::SetItemTooltip("%s", D.path.c_str());
    }
    // The rows: every feature, or the filter's.
    const int rows = lab_.filter.empty() ? n : int(lab_.filter.size());
    auto rowFeature = [&](int r) { return lab_.filter.empty() ? r : lab_.filter[r]; };
    if (ImGui::BeginTable("##features", 5, ImGuiTableFlags_ScrollY | ImGuiTableFlags_RowBg | ImGuiTableFlags_BordersInnerV,
                          ImVec2(0, std::max(120.0f, ImGui::GetContentRegionAvail().y)))) {
        ImGui::TableSetupScrollFreeze(0, 1);
        ImGui::TableSetupColumn("#", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("label");
        ImGui::TableSetupColumn("date", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("conf.", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("notes");
        ImGui::TableHeadersRow();
        int currentRow = -1;
        if (lab_.scrollToCurrent && lab_.current >= 0)
            for (int r = 0; r < rows && currentRow < 0; ++r)
                if (rowFeature(r) == lab_.current) currentRow = r;
        ImGuiListClipper clip;
        clip.Begin(rows);
        if (currentRow >= 0) clip.IncludeItemByIndex(currentRow);
        while (clip.Step())
            for (int r = clip.DisplayStart; r < clip.DisplayEnd; ++r) {
                const int i = rowFeature(r);
                if (i < 0 || i >= int(lab_.entries.size())) continue;
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
                if (!e.events.empty()) {
                    ImGui::SameLine();
                    ImGui::TextDisabled("+%d", int(e.events.size()));
                }
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(e.date.c_str());
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(e.confidence == 1 ? "low" : e.confidence == 2 ? "med." : e.confidence == 3 ? "high" : "");
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(e.notes.c_str());
            }
        ImGui::EndTable();
    }
    ImGui::End();
}
