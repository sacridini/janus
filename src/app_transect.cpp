// Space-time transect (a Hovmoeller diagram): a line drawn on the map (Ctrl +
// drag, or T then drag) is sampled at every date, and the Transect panel shows
// the distance along it (X) against the dates (Y, oldest on top) in the
// layer's colours. It is filled at once from the overview in RAM, then refined
// with full-resolution values read in the background (one job per date on the
// interactive pool; on an HDD only once the overview is built). The image is
// drawn by the map renderer: the values are a float texture drawn like a
// detail tile into a target of its own, so colormaps and class colours are
// the map's.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>

#include <imgui_internal.h>
#include <implot.h>

namespace {

constexpr int kMaxSamples = 1024; // columns of the image: longer lines are subsampled
const ImU32 kLineCol = IM_COL32(80, 230, 255, 255);

// Clips the segment a-b to [0, w] x [0, h] (Liang-Barsky).
bool clipSegment(ImVec2& a, ImVec2& b, float w, float h) {
    double t0 = 0, t1 = 1;
    const double dx = b.x - a.x, dy = b.y - a.y;
    const double p[4] = {-dx, dx, -dy, dy}, q[4] = {a.x, w - a.x, a.y, h - a.y};
    for (int i = 0; i < 4; ++i) {
        if (p[i] == 0) {
            if (q[i] < 0) return false;
            continue;
        }
        const double r = q[i] / p[i];
        if (p[i] < 0) t0 = std::max(t0, r);
        else t1 = std::min(t1, r);
    }
    if (t0 >= t1) return false;
    const ImVec2 a0 = a;
    a = ImVec2(float(a0.x + t0 * dx), float(a0.y + t0 * dy));
    b = ImVec2(float(a0.x + t1 * dx), float(a0.y + t1 * dy));
    return true;
}

std::string formatDistance(double d, const std::string& unit) {
    char b[48];
    if (unit == "m" && std::fabs(d) >= 10000) std::snprintf(b, sizeof(b), "%.2f km", d / 1000);
    else std::snprintf(b, sizeof(b), "%.5g %s", d, unit.c_str());
    return b;
}

} // namespace

// Full-resolution rows, written by the readers (one job per date).
struct App::TransectExact {
    int n = 0, T = 0;
    std::vector<int> px, py;
    std::vector<float> values;                    // [t][i]
    std::unique_ptr<std::atomic<bool>[]> rowDone; // set once the row is written
    std::atomic<int> done{0};
    std::atomic<bool> cancel{false};
};

// a, b: the active layer's pixels (map space).
void App::setTransect(ImVec2 a, ImVec2 b) {
    clearTransect();
    if (!s_) return;
    const CubeInfo& info = *s_->info;
    Transect& tr = tr_;
    // The line as drawn, georeferenced: another layer samples its own part of it.
    tr.geo = info.hasGeoTransform && info.pixelToGeo(a.x, a.y, tr.ga[0], tr.ga[1]) &&
             info.pixelToGeo(b.x, b.y, tr.gb[0], tr.gb[1]);
    if (!clipSegment(a, b, float(info.width), float(info.height))) {
        clearTransect();
        return;
    }
    tr.on = true;
    tr.cube = info.id;
    tr.a = a;
    tr.b = b;
    double g[4] = {0, 0, 0, 0}; // clipped end points
    if (tr.geo) {
        info.pixelToGeo(a.x, a.y, g[0], g[1]);
        info.pixelToGeo(b.x, b.y, g[2], g[3]);
    }
    const double lenPx = std::hypot(double(b.x - a.x), double(b.y - a.y));
    tr.n = std::clamp(int(std::ceil(lenPx)) + 1, 2, kMaxSamples);
    tr.T = info.T();
    tr.px.resize(tr.n);
    tr.py.resize(tr.n);
    for (int i = 0; i < tr.n; ++i) {
        const double f = double(i) / (tr.n - 1);
        tr.px[i] = std::clamp(int(std::floor(a.x + f * (b.x - a.x))), 0, info.width - 1);
        tr.py[i] = std::clamp(int(std::floor(a.y + f * (b.y - a.y))), 0, info.height - 1);
    }
    // Length: metres on the ellipsoid's mean sphere for geographic CRSs (EPSG:4xxx,
    // degrees), map units otherwise (metres for an EPSG projected CRS), else pixels.
    if (tr.geo) {
        const std::string& auth = info.crsAuthority;
        if (auth.size() == 9 && auth.compare(0, 6, "EPSG:4") == 0) {
            const double k = 3.14159265358979323846 / 180, R = 6371008.8;
            const double la1 = g[1] * k, la2 = g[3] * k, dla = la2 - la1, dlo = (g[2] - g[0]) * k;
            const double h = std::sin(dla / 2) * std::sin(dla / 2) +
                             std::cos(la1) * std::cos(la2) * std::sin(dlo / 2) * std::sin(dlo / 2);
            tr.length = 2 * R * std::asin(std::min(1.0, std::sqrt(h)));
            tr.unit = "m";
        } else {
            tr.length = std::hypot(g[2] - g[0], g[3] - g[1]);
            tr.unit = auth.empty() ? "map units" : "m";
        }
    }
    if (!tr.geo || !(tr.length > 0)) {
        tr.length = lenPx;
        tr.unit = "px";
    }
    tr.values.assign(size_t(tr.n) * tr.T, NAN);
    tr.state.assign(tr.T, 0);
    if (s_->deferRandomReads()) tr.pending = true; // HDD still building the overview
    else startTransectExact();
    updateTransectRows();
    publishTransect();
}

void App::clearTransect() {
    if (tr_.job) tr_.job->cancel = true;
    if (tr_.tex) Gpu::deleteTexture(tr_.tex);
    gpu_.releaseMap(kTransectSlot);
    const bool docked = tr_.docked;
    const int kind = tr_.kind;
    tr_ = Transect{};
    tr_.docked = docked;
    tr_.kind = kind;
}

void App::startTransectExact() {
    auto job = std::make_shared<TransectExact>();
    job->n = tr_.n;
    job->T = tr_.T;
    job->px = tr_.px;
    job->py = tr_.py;
    job->values.assign(size_t(job->n) * job->T, NAN);
    job->rowDone = std::make_unique<std::atomic<bool>[]>(size_t(job->T));
    const std::shared_ptr<const CubeInfo> info = s_->info;
    for (int t = 0; t < job->T; ++t) // after the cursor series, tiles and the ROI; oldest date first
        s_->fgPool().submit(20 + t, [job, info, t] {
            CubeReader& reader = threadReader(info);
            float* row = job->values.data() + size_t(t) * job->n;
            for (int i = 0; i < job->n; ++i) {
                if (job->cancel) return;
                if (i > 0 && job->px[i] == job->px[i - 1] && job->py[i] == job->py[i - 1]) row[i] = row[i - 1];
                else if (!reader.readPixel(t, job->px[i], job->py[i], row[i])) row[i] = NAN;
            }
            job->rowDone[t].store(true, std::memory_order_release);
            ++job->done;
        });
    tr_.job = job;
    tr_.pending = false;
}

int App::transectRowsRead() const { return tr_.job ? tr_.job->done.load() : 0; }

bool App::updateTransectRows() {
    Transect& tr = tr_;
    const Overview& ov = s_->overview;
    const CubeInfo& info = *s_->info;
    bool changed = false;
    for (int t = 0; t < tr.T; ++t) {
        if (tr.state[t] == 2) continue;
        float* row = tr.values.data() + size_t(t) * tr.n;
        if (tr.job && tr.job->rowDone[t].load(std::memory_order_acquire)) {
            std::memcpy(row, tr.job->values.data() + size_t(t) * tr.n, sizeof(float) * tr.n);
            tr.state[t] = 2;
            changed = true;
        } else if (tr.state[t] == 0 && s_->gpu.loaded[t]) { // same pixels as the approximate series
            for (int i = 0; i < tr.n; ++i) {
                const int ox = std::min(ov.w - 1, int(double(tr.px[i]) * ov.w / info.width));
                const int oy = std::min(ov.h - 1, int(double(tr.py[i]) * ov.h / info.height));
                row[i] = ov.at(t, ox, oy);
            }
            tr.state[t] = 1;
            changed = true;
        }
    }
    tr.dirty |= changed;
    return changed;
}

void App::publishTransect() {
    Transect& tr = tr_;
    const int n = tr.n, T = tr.T;
    tr.shown = tr.values;
    const bool anomaly = tr.kind == 1 && !activeClasses();
    if (anomaly)
        for (int i = 0; i < n; ++i) {
            double s = 0;
            int k = 0;
            for (int t = 0; t < T; ++t)
                if (const float v = tr.values[size_t(t) * n + i]; std::isfinite(v)) {
                    s += v;
                    ++k;
                }
            const float mean = k ? float(s / k) : NAN;
            for (int t = 0; t < T; ++t) tr.shown[size_t(t) * n + i] -= mean;
        }
    // Own range (used while the map has none for this mode), from a sample.
    std::vector<float> smp;
    const size_t step = std::max<size_t>(1, tr.shown.size() / 200000);
    for (size_t k = 0; k < tr.shown.size(); k += step)
        if (std::isfinite(tr.shown[k])) smp.push_back(tr.shown[k]);
    if (!smp.empty()) autoRangeOf(anomaly ? ModeAnomaly : ModeValue, smp, tr.range.lo, tr.range.hi);
    if (tr.tex) Gpu::deleteTexture(tr.tex);
    tr.tex = Gpu::createTileTexture(n, T, tr.shown.data());
    ++tr.version;
    tr.dirty = false;
    tr.published = ImGui::GetTime();
}

// The map's colormap and range for that mode when it has one (automatic range
// already computed, or set by hand), else the transect's own range.
void App::transectColors(int& cmap, float& lo, float& hi) const {
    const int m = tr_.kind == 1 && !activeClasses() ? ModeAnomaly : ModeValue;
    cmap = cmap_[m];
    const Range& r = range_[m].manual || range_[m].key != ~0ull ? range_[m] : tr_.range;
    lo = r.lo;
    hi = r.hi;
}

void App::pumpTransect() {
    if (!tr_.on) return;
    if (!s_) {
        clearTransect();
        return;
    }
    if (s_->info->id != tr_.cube) {
        // Another active layer (or the same one reopened): the same place on it.
        const CubeInfo& info = *s_->info;
        if (tr_.geo && info.hasGeoTransform) {
            const auto& g = info.geoTransform;
            const ImVec2 a(float((tr_.ga[0] - g[0]) / g[1]), float((tr_.ga[1] - g[3]) / g[5]));
            const ImVec2 b(float((tr_.gb[0] - g[0]) / g[1]), float((tr_.gb[1] - g[3]) / g[5]));
            setTransect(a, b); // cleared if it is outside this layer
        } else {
            clearTransect();
        }
        return;
    }
    if (tr_.pending && !s_->deferRandomReads()) startTransectExact();
    updateTransectRows();
    // The image is remade at most 5 times a second while rows keep coming.
    int waiting = 0;
    for (int t = 0; t < tr_.T; ++t) waiting += tr_.state[t] != (tr_.job ? 2 : 1);
    if (tr_.dirty && (waiting == 0 || ImGui::GetTime() - tr_.published >= 0.2)) publishTransect();
}

bool App::transectInput(int panel, double sx, double sy) {
    const ImGuiIO& io = ImGui::GetIO();
    if (ImGui::IsItemActivated() && !io.KeyShift && (io.KeyCtrl || transectMode_)) {
        transectPanel_ = panel;
        transectCancel_ = false;
        transectStart_ = transectEnd_ = ImVec2(float(sx), float(sy));
    }
    if (transectPanel_ != panel) return false;
    if (ImGui::IsItemActive()) {
        if (!transectCancel_) transectEnd_ = ImVec2(float(sx), float(sy));
        return true;
    }
    // Released: a line of a few points at least.
    transectPanel_ = -1;
    const ImVec2 d = (transectEnd_ - transectStart_) * float(scale_);
    if (!transectCancel_ && d.x * d.x + d.y * d.y >= 25) {
        setTransect(transectStart_, transectEnd_);
        transectMode_ = false;
    }
    return true;
}

void App::drawTransectMarks(ImDrawList* dl, ImVec2 origin) {
    auto toScreen = [&](ImVec2 p) {
        return origin + ImVec2(float(offset_.x + p.x * scale_), float(offset_.y + p.y * scale_));
    };
    auto line = [&](ImVec2 a, ImVec2 b) {
        const ImVec2 A = toScreen(a), B = toScreen(b);
        dl->AddLine(A, B, IM_COL32(0, 0, 0, 200), 4.0f);
        dl->AddLine(A, B, kLineCol, 2.0f);
        const char* names[2] = {"A", "B"};
        for (int k = 0; k < 2; ++k) {
            const ImVec2 p = k ? B : A;
            dl->AddCircleFilled(p, 4.5f, kLineCol);
            dl->AddCircle(p, 4.5f, IM_COL32(0, 0, 0, 220), 0, 1.5f);
            dl->AddText(p + ImVec2(6, -16) + ImVec2(1, 1), IM_COL32(0, 0, 0, 220), names[k]);
            dl->AddText(p + ImVec2(6, -16), kLineCol, names[k]);
        }
    };
    if (transectPanel_ >= 0 && !transectCancel_) {
        line(transectStart_, transectEnd_);
    } else if (tr_.on) {
        line(tr_.a, tr_.b);
        if (tr_.hoverI >= 0) { // the cell hovered in the Transect panel
            const float f = float(tr_.hoverI) / float(tr_.n - 1);
            const ImVec2 c = toScreen(tr_.a + (tr_.b - tr_.a) * f);
            dl->AddCircle(c, 7.0f, IM_COL32(0, 0, 0, 220), 0, 4.0f);
            dl->AddCircle(c, 7.0f, IM_COL32(255, 255, 255, 255), 0, 2.0f);
        }
    }
    if (transectMode_ && transectPanel_ < 0 && ImGui::IsItemHovered()) {
        const ImVec2 p = ImGui::GetIO().MousePos + ImVec2(16, 12);
        const char* hint = "drag: transect (Esc cancels)";
        dl->AddText(p + ImVec2(1, 1), IM_COL32(0, 0, 0, 220), hint);
        dl->AddText(p, kLineCol, hint);
    }
}

void App::renderTransect(int w, int h) {
    const float bg[4] = {0.10f, 0.10f, 0.115f, 1.0f}; // no data
    gpu_.beginMap(w, h, bg, kTransectSlot);
    DrawParams p;
    int cmap;
    transectColors(cmap, p.lo, p.hi);
    if (const LayerClasses* C = activeClasses()) p.classLut = C->lut;
    const float rect[4] = {0, 0, float(w), float(h)};
    if (tr_.tex) gpu_.drawTile(tr_.tex, rect, p, cmap, 1.0f);
    gpu_.endMap();
}

void App::uiTransect() {
    if (!tr_.on || !s_) return;
    if (!tr_.docked) {
        // Layouts saved before this panel existed: a tab next to the chart.
        tr_.docked = true;
        if (!ImGui::FindWindowSettingsByID(ImHashStr("Transect")))
            if (const ImGuiWindow* ts = ImGui::FindWindowByName("Time series"); ts && ts->DockId)
                ImGui::DockBuilderDockWindow("Transect", ts->DockId);
    }
    bool open = true;
    const bool shown = ImGui::Begin("Transect", &open);
    if (!open) {
        ImGui::End();
        clearTransect();
        return;
    }
    if (!shown) {
        tr_.hoverI = tr_.hoverT = -1;
        ImGui::End();
        return;
    }
    Transect& tr = tr_;
    const CubeInfo& info = *s_->info;
    const LayerClasses* classes = activeClasses();
    const int n = tr.n, T = tr.T;

    // --- Bar ---
    if (const SeriesLayer* L = activeLayer()) ImGui::TextColored(theme::accent(), "%s", L->name.c_str());
    ImGui::SameLine();
    int exact = 0;
    for (char s : tr.state) exact += s == 2;
    ImGui::TextDisabled("%s, %d samples  |  %s", formatDistance(tr.length, tr.unit).c_str(), n,
                        tr.pending                ? "overview; full resolution once the overview is built (HDD)"
                        : exact == T              ? "full resolution"
                        : tr.job                  ? "overview, full resolution coming..."
                                                  : "overview");
    if (tr.job && exact < T) {
        ImGui::SameLine();
        ImGui::TextDisabled("%d/%d", exact, T);
    }
    ImGui::BeginDisabled(classes != nullptr);
    ImGui::SetNextItemWidth(170);
    if (ImGui::Combo("##kind", &tr.kind, "Values\0Anomaly (- sample mean)\0")) publishTransect();
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Values: as on the map ('Value at date' colours and range)\n"
                          "Anomaly: each place minus its own mean through time");
    ImGui::SameLine();
    if (ImGui::Button("Copy CSV")) copyTransectCsv();
    ImGui::SetItemTooltip("Copies the transect to the clipboard: one row per date, one column per\n"
                          "sample (header: distance from A)");
    ImGui::SameLine();
    if (ImGui::Button("Clear")) {
        ImGui::End();
        clearTransect();
        return;
    }
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    ImGui::SetItemTooltip("Distance along the line from A (X) against the dates (Y, oldest on top).\n"
                          "Hover: distance, date and value, marked on the map. Click: go to that date.\n"
                          "Draw another line: Ctrl + drag on the map (or T, then drag).\n"
                          "Filled from the overview at once, then with full-resolution values.");

    // --- Image ---
    const double sp = tr.length / (n - 1); // sample spacing
    const double x0 = -sp / 2, x1 = tr.length + sp / 2;
    const ImPlotCond cond = tr.fit ? ImPlotCond_Always : ImPlotCond_Once;
    tr.fit = false;
    tr.hoverI = tr.hoverT = -1;
    const float avail = std::max(60.0f, ImGui::GetContentRegionAvail().y - 2 * ImGui::GetFrameHeight());
    ImGui::PushID(int(tr.cube));
    if (ImPlot::BeginPlot("##transect", ImVec2(-1, -1), ImPlotFlags_NoTitle | ImPlotFlags_NoLegend | ImPlotFlags_NoMouseText)) {
        const std::string xl = "distance from A (" + tr.unit + ")";
        ImPlot::SetupAxes(xl.c_str(), nullptr, ImPlotAxisFlags_None, ImPlotAxisFlags_Invert);
        ImPlot::SetupAxisLimits(ImAxis_X1, x0, x1, cond);
        ImPlot::SetupAxisLimits(ImAxis_Y1, 0, T, cond);
        // Date ticks: every k-th date, so the labels do not overlap.
        const int k = std::max(1, int(std::ceil(T * ImGui::GetTextLineHeight() * 1.5f / avail)));
        std::vector<double> ticks;
        std::vector<const char*> labels;
        for (int t = 0; t < T; t += k) {
            ticks.push_back(t + 0.5);
            labels.push_back(info.layers[t].label.c_str());
        }
        ImPlot::SetupAxisTicks(ImAxis_Y1, ticks.data(), int(ticks.size()), labels.data());
        ImPlot::SetupFinish();

        // Rendered with whole cells of pixels (crisp when scaled), up to 4096 per side.
        const ImVec2 ps = ImPlot::GetPlotSize();
        const float scale = std::max(1.0f, ImGui::GetWindowViewport()->FramebufferScale.x);
        const int cw = std::clamp(int(std::ceil(ps.x * scale / n)), 1, std::max(1, 4096 / n));
        const int ch = std::clamp(int(std::ceil(ps.y * scale / T)), 1, std::max(1, 4096 / T));
        int cmap;
        float lo, hi;
        transectColors(cmap, lo, hi);
        uint64_t key = tr.version * 1000003ull ^ (uint64_t(cmap) << 48) ^ (uint64_t(cw) << 32) ^ (uint64_t(ch) << 40);
        uint32_t bits[2];
        std::memcpy(&bits[0], &lo, 4);
        std::memcpy(&bits[1], &hi, 4);
        key = key * 31 + bits[0];
        key = key * 31 + bits[1];
        if (classes)
            for (const ClassEntry& c : classes->list)
                key = key * 31 + ImGui::ColorConvertFloat4ToU32(c.color) + (c.visible ? 1 : 0);
        if (key != tr.drawnKey) {
            renderTransect(n * cw, T * ch);
            tr.drawnKey = key;
        }
        // Inverted Y: the image's bottom-left corner (uv0) is the last date.
        const bool bottomUp = Gpu::mapBottomUp();
        ImPlot::PlotImage("##img", ImTextureRef((ImTextureID)gpu_.mapTexture(kTransectSlot)), ImPlotPoint(x0, 0),
                          ImPlotPoint(x1, T), ImVec2(0, bottomUp ? 0.f : 1.f), ImVec2(1, bottomUp ? 1.f : 0.f));

        ImDrawList* dl = ImPlot::GetPlotDrawList();
        ImPlot::PushPlotClipRect();
        // Current date (orange, like the chart).
        dl->AddRect(ImPlot::PlotToPixels(x0, t_), ImPlot::PlotToPixels(x1, t_ + 1), IM_COL32(255, 153, 51, 255), 0,
                    ImDrawFlags_None, 1.5f);
        // The map cursor, when it is on the line.
        if (prevMouseInPanel_ >= 0 && hover_.x >= 0) {
            const ImVec2 p(hover_.x + 0.5f, hover_.y + 0.5f), ab = tr.b - tr.a;
            const float len2 = ab.x * ab.x + ab.y * ab.y;
            const float u = len2 > 0 ? ((p.x - tr.a.x) * ab.x + (p.y - tr.a.y) * ab.y) / len2 : -1;
            const ImVec2 off = (tr.a + ab * u - p) * float(scale_);
            if (u >= 0 && u <= 1 && off.x * off.x + off.y * off.y < 100) {
                const ImVec2 a = ImPlot::PlotToPixels(u * tr.length, 0), b = ImPlot::PlotToPixels(u * tr.length, T);
                dl->AddLine(a, b, IM_COL32(0, 0, 0, 200), 3.0f);
                dl->AddLine(a, b, IM_COL32(255, 255, 255, 230), 1.0f);
            }
        }
        // Hovered cell: outline, tooltip, marked on the map.
        if (ImPlot::IsPlotHovered()) {
            const ImPlotPoint m = ImPlot::GetPlotMousePos();
            const int i = int(std::lround(m.x / sp)), t = int(std::floor(m.y));
            if (i >= 0 && i < n && t >= 0 && t < T) {
                tr.hoverI = i;
                tr.hoverT = t;
                dl->AddRect(ImPlot::PlotToPixels(i * sp - sp / 2, t), ImPlot::PlotToPixels(i * sp + sp / 2, t + 1),
                            IM_COL32(255, 255, 255, 255), 0, ImDrawFlags_None, 1.5f);
                const float v = tr.values[size_t(t) * n + i];
                ImGui::BeginTooltip();
                ImGui::Text("%s  |  %s", formatDistance(i * sp, tr.unit).c_str(), info.layers[t].label.c_str());
                if (classes) ImGui::Text("class %s", className(*classes, v).c_str());
                else if (tr.kind == 1) ImGui::Text("value %.6g  (anomaly %.4g)", v, tr.shown[size_t(t) * n + i]);
                else ImGui::Text("value %.6g", v);
                ImGui::TextDisabled("col %d  row %d%s", tr.px[i], tr.py[i],
                                    tr.state[t] == 2 ? "" : tr.state[t] == 1 ? "  (approx., overview)" : "  (not loaded)");
                ImGui::EndTooltip();
                if (ImGui::IsMouseReleased(ImGuiMouseButton_Left) && ImGui::GetIO().MouseDragMaxDistanceSqr[0] < 16)
                    setT(t);
            }
        }
        ImPlot::PopPlotClipRect();
        ImPlot::EndPlot();
    }
    ImGui::PopID();
    ImGui::End();
}

void App::copyTransectCsv() {
    if (!tr_.on || !s_) return;
    const Transect& tr = tr_;
    const CubeInfo& info = *s_->info;
    const double sp = tr.length / (tr.n - 1);
    std::string csv = "date";
    char b[40];
    for (int i = 0; i < tr.n; ++i) {
        std::snprintf(b, sizeof(b), ",%.7g", i * sp);
        csv += b;
    }
    csv += "\n";
    for (int t = 0; t < tr.T; ++t) {
        csv += info.layers[t].label;
        for (int i = 0; i < tr.n; ++i) {
            const float v = tr.shown[size_t(t) * tr.n + i];
            if (std::isnan(v)) {
                csv += ",";
            } else {
                std::snprintf(b, sizeof(b), ",%.7g", v);
                csv += b;
            }
        }
        csv += "\n";
    }
    ImGui::SetClipboardText(csv.c_str());
}
