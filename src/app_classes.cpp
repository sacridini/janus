// Categorical series (land cover, masks, classifications...): detection, class
// colours and names, the GPU lookup table and the legend.
//
// A layer is categorical when its file says so (colour table, category names,
// attribute table) or, failing that, when the first date read has only a few
// distinct whole values. The Display panel can override the detection.
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <map>

#include <imgui.h>

namespace {

constexpr int kMaxDetectedClasses = 40;  // more distinct values: continuous data
constexpr int kMaxClasses = 256;         // the most a layer can show as classes
constexpr size_t kMaxSamples = 20000000; // values counted once the overview is complete

// Tableau 20: distinct colours for up to 20 classes without a colour table.
const unsigned char kPalette[20][3] = {
    {31, 119, 180}, {255, 127, 14}, {44, 160, 44},   {214, 39, 40},   {148, 103, 189}, {140, 86, 75},  {227, 119, 194},
    {127, 127, 127}, {188, 189, 34}, {23, 190, 207}, {174, 199, 232}, {255, 187, 120}, {152, 223, 138}, {255, 152, 150},
    {197, 176, 213}, {196, 156, 148}, {247, 182, 210}, {199, 199, 199}, {219, 219, 141}, {158, 218, 229}};

// Counts of the whole values in `data` (every `stride`-th value); false if a
// finite value is not whole or there are more than `maxClasses` of them.
bool countClasses(const float* data, size_t n, size_t stride, int maxClasses, std::map<int, size_t>& counts) {
    for (size_t i = 0; i < n; i += stride) {
        const float v = data[i];
        if (!std::isfinite(v)) continue;
        const float r = std::round(v);
        if (r != v || std::fabs(r) > 1e9f) return false;
        ++counts[int(r)];
        if (int(counts.size()) > maxClasses) return false;
    }
    return true;
}

} // namespace

const App::ClassEntry* App::LayerClasses::find(int value) const {
    auto it = std::lower_bound(list.begin(), list.end(), value,
                               [](const ClassEntry& c, int v) { return c.value < v; });
    return it != list.end() && it->value == value ? &*it : nullptr;
}

// Rebuilds the class list from value counts, keeping names and colours the
// user already edited.
void App::setClasses(SeriesLayer& L, const std::map<int, size_t>& counts) {
    const CubeInfo& info = *L.session->info;
    std::vector<ClassEntry> list;
    // Palette colours already taken by classes that keep theirs: a class that
    // appears later gets the next free one.
    std::vector<bool> used(20, false);
    auto paletteIndex = [](const ImVec4& c) {
        for (int i = 0; i < 20; ++i)
            if (std::lround(c.x * 255) == kPalette[i][0] && std::lround(c.y * 255) == kPalette[i][1] &&
                std::lround(c.z * 255) == kPalette[i][2])
                return i;
        return -1;
    };
    for (const auto& [value, count] : counts)
        if (const ClassEntry* old = L.classes.find(value))
            if (const int i = paletteIndex(old->color); i >= 0) used[i] = true;
    int k = 0;
    auto nextFree = [&] {
        for (int tries = 0; tries < 20 && used[k % 20]; ++tries) ++k;
        used[k % 20] = true;
        return k % 20;
    };
    for (const auto& [value, count] : counts) {
        ClassEntry c;
        c.value = value;
        c.count = count;
        if (const ClassEntry* old = L.classes.find(value)) {
            c.name = old->name;
            c.color = old->color;
            c.visible = old->visible;
        } else {
            auto n = info.classNames.find(value);
            c.name = n != info.classNames.end() ? n->second : "Class " + std::to_string(value);
            auto col = info.classColors.find(value);
            if (col != info.classColors.end()) {
                c.color = ImVec4(col->second[0] / 255.f, col->second[1] / 255.f, col->second[2] / 255.f, 1);
            } else {
                const int i = nextFree();
                c.color = ImVec4(kPalette[i][0] / 255.f, kPalette[i][1] / 255.f, kPalette[i][2] / 255.f, 1);
            }
            c.visible = true;
        }
        list.push_back(c);
    }
    L.classes.list = std::move(list);
    L.classes.lutDirty = true;
}

// Counts the classes over every loaded date (subsampled to kMaxSamples values).
bool App::countLayerClasses(const SeriesLayer& L, int maxClasses, std::map<int, size_t>& counts) const {
    const Session& S = *L.session;
    const Overview& ov = S.overview;
    const size_t per = size_t(ov.w) * ov.h;
    int loaded = 0;
    for (int t = 0; t < ov.T; ++t) loaded += S.gpu.loaded[t] ? 1 : 0;
    if (!loaded || !per) return false;
    const size_t stride = std::max<size_t>(1, per * loaded / kMaxSamples);
    for (int t = 0; t < ov.T; ++t)
        if (S.gpu.loaded[t] && !countClasses(ov.layer(t), per, stride, maxClasses, counts)) return false;
    return true;
}

// Per frame, for every layer: decides whether it is categorical, completes the
// class list once every date is read, and refreshes the GPU lookup table.
void App::updateClasses(SeriesLayer& L) {
    LayerClasses& C = L.classes;
    const Session& S = *L.session;
    const CubeInfo& info = *S.info;
    int firstLoaded = -1;
    for (int t = 0; t < info.T() && firstLoaded < 0; ++t)
        if (S.gpu.loaded[t]) firstLoaded = t;

    if (C.state == LayerClasses::Undecided && firstLoaded >= 0) {
        std::map<int, size_t> counts;
        const Overview& ov = S.overview;
        const bool few = countClasses(ov.layer(firstLoaded), size_t(ov.w) * ov.h, 1,
                                      info.fileCategorical ? kMaxClasses : kMaxDetectedClasses, counts);
        // Declared by the file: classes even if a date has many values (up to kMaxClasses).
        if ((info.fileCategorical || few) && !counts.empty()) {
            C.state = LayerClasses::On;
            setClasses(L, counts);
            if (&L == activeLayer()) mode_ = ModeValue;
            else L.disp.mode = ModeValue;
            mapDirty_ = true;
        } else {
            C.state = LayerClasses::Off;
        }
    }
    if (C.state == LayerClasses::On && !C.complete && S.overview.complete()) {
        // Every date read: the classes that appear later in the series too.
        std::map<int, size_t> counts;
        if (countLayerClasses(L, kMaxClasses, counts)) setClasses(L, counts);
        C.complete = true;
    }
    if (C.lutDirty && C.state == LayerClasses::On) {
        std::vector<unsigned char> lut(size_t(kClassLutSize) * 4, 0);
        for (const ClassEntry& c : C.list) {
            if (c.value < 0 || c.value >= kClassLutSize) continue;
            unsigned char* px = &lut[size_t(c.value) * 4];
            px[0] = (unsigned char)std::lround(c.color.x * 255);
            px[1] = (unsigned char)std::lround(c.color.y * 255);
            px[2] = (unsigned char)std::lround(c.color.z * 255);
            px[3] = c.visible ? 255 : 0;
        }
        C.lut = Gpu::createClassLut(lut.data(), C.lut);
        C.lutDirty = false;
        mapDirty_ = true;
    }
}

const App::LayerClasses* App::activeClasses() const {
    const SeriesLayer* L = activeLayer();
    return L && L->classes.state == LayerClasses::On ? &L->classes : nullptr;
}

std::string App::className(const LayerClasses& C, float v) {
    if (!std::isfinite(v)) return "-";
    const int k = int(std::lround(v));
    const ClassEntry* c = C.find(k);
    return c ? c->name + " (" + std::to_string(k) + ")" : std::to_string(k);
}

// Display panel: categorical switch and the legend (colours and names editable).
void App::uiClasses() {
    SeriesLayer* L = activeLayer() ? &layers_[active_] : nullptr;
    if (!L) return;
    LayerClasses& C = L->classes;
    if (C.state == LayerClasses::Undecided) return;
    bool on = C.state == LayerClasses::On;
    if (ImGui::Checkbox("Categorical (classes)", &on)) {
        if (on) {
            std::map<int, size_t> counts;
            if (countLayerClasses(*L, kMaxClasses, counts) && !counts.empty()) {
                C.state = LayerClasses::On;
                C.complete = L->session->overview.complete();
                setClasses(*L, counts);
                mode_ = ModeValue;
            } else {
                classError_ = "Not categorical: more than " + std::to_string(kMaxClasses) +
                              " distinct values, or values that are not whole numbers.";
            }
        } else {
            C.state = LayerClasses::Off;
        }
        mapDirty_ = true;
    }
    ImGui::SetItemTooltip("Each value is a class with its own colour (land cover, masks, classifications).\n"
                          "Detected from the file's colour table or category names, or from a few whole values.");
    if (!classError_.empty() && C.state != LayerClasses::On) ImGui::TextColored(ImVec4(1, 0.55f, 0.45f, 1), "%s", classError_.c_str());
    if (C.state != LayerClasses::On) return;
    classError_.clear();

    size_t total = 0;
    for (const ClassEntry& c : C.list) total += c.count;
    ImGui::SeparatorText("Classes");
    const ImGuiTableFlags flags = ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit | ImGuiTableFlags_ScrollY;
    const float h = std::min(ImGui::GetTextLineHeightWithSpacing() * (float(C.list.size()) + 1.5f), 320.0f);
    if (ImGui::BeginTable("classes", 4, flags, ImVec2(0, h))) {
        ImGui::TableSetupColumn("", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("Value", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableSetupColumn("Name", ImGuiTableColumnFlags_WidthStretch);
        ImGui::TableSetupColumn("Share", ImGuiTableColumnFlags_WidthFixed);
        ImGui::TableHeadersRow();
        for (size_t i = 0; i < C.list.size(); ++i) {
            ClassEntry& c = C.list[i];
            ImGui::PushID(int(i));
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            if (ImGui::Checkbox("##vis", &c.visible)) C.lutDirty = true;
            ImGui::SetItemTooltip("Show this class on the map");
            ImGui::SameLine();
            if (ImGui::ColorEdit3("##col", &c.color.x, ImGuiColorEditFlags_NoInputs | ImGuiColorEditFlags_NoLabel))
                C.lutDirty = true;
            ImGui::TableNextColumn();
            ImGui::Text("%d", c.value);
            ImGui::TableNextColumn();
            char name[96];
            std::snprintf(name, sizeof(name), "%s", c.name.c_str());
            ImGui::SetNextItemWidth(-1);
            if (ImGui::InputText("##name", name, sizeof(name))) c.name = name;
            ImGui::TableNextColumn();
            ImGui::TextDisabled("%.1f%%", total ? 100.0 * double(c.count) / double(total) : 0.0);
            ImGui::PopID();
        }
        ImGui::EndTable();
    }
    if (!C.complete) ImGui::TextDisabled("Shares from the dates read so far.");
    if (C.list.size() > 0 && C.list.back().value >= kClassLutSize)
        ImGui::TextDisabled("Values from %d on are drawn grey.", kClassLutSize);
}

// Statistics of a categorical series: majority class, changes, last change.
std::vector<std::pair<std::string, std::string>> App::classSummary(const LayerClasses& C, const CubeInfo& info,
                                                                   const std::vector<float>& v, int t) const {
    std::vector<std::pair<std::string, std::string>> rows;
    std::map<int, int> count;
    int changes = 0, last = -1, lastChange = -1, prev = -1, n = 0;
    for (int i = 0; i < int(v.size()); ++i) {
        if (!std::isfinite(v[i])) continue;
        const int k = int(std::lround(v[i]));
        ++count[k];
        ++n;
        if (last >= 0 && k != int(std::lround(v[last]))) {
            ++changes;
            lastChange = i;
            prev = last;
        }
        last = i;
    }
    rows.push_back({"Valid n", std::to_string(n)});
    rows.push_back({"Class at the date", t >= 0 && t < int(v.size()) ? className(C, v[t]) : "-"});
    if (!count.empty()) {
        auto best = std::max_element(count.begin(), count.end(),
                                     [](const auto& a, const auto& b) { return a.second < b.second; });
        char share[32];
        std::snprintf(share, sizeof(share), " %.0f%%", 100.0 * best->second / n);
        rows.push_back({"Majority class", className(C, float(best->first)) + share});
    } else {
        rows.push_back({"Majority class", "-"});
    }
    rows.push_back({"Classes seen", std::to_string(count.size())});
    rows.push_back({"Changes", std::to_string(changes)});
    rows.push_back({"Last change", lastChange < 0 ? "-"
                                                  : info.layers[lastChange].label + ": " + className(C, v[prev]) +
                                                        " -> " + className(C, v[lastChange])});
    return rows;
}
