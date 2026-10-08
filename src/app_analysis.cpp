// Analysis panel (View > Analysis): views that need many dates or two layers
// at once. Seasonal: a series by day of the year (one line per year, the mean
// of every year by month, a year x day-of-year heatmap). Classes: the share of
// each class at every date and the transitions between two dates (categorical
// layers). Scatter: the values of two layers or two dates, pixel by pixel, in
// the ROI or the whole image. Classes and scatter use the overview (what the
// map shows before zooming in), so they are immediate on any series.
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <string>
#include <unordered_map>

#include <imgui_internal.h>
#include <implot.h>

#include "glfw.hpp"

namespace {

const char* const kMonths[12] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun",
                                 "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};
const double kMonthStart[13] = {1, 32, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 367}; // day of year

int monthOf(int dayOfYear) {
    int m = 0;
    while (m < 11 && dayOfYear >= kMonthStart[m + 1]) ++m;
    return m;
}

// Month ticks on a day-of-year axis.
void setupMonthAxis(ImAxis axis) {
    double ticks[12];
    for (int m = 0; m < 12; ++m) ticks[m] = kMonthStart[m];
    ImPlot::SetupAxisTicks(axis, ticks, 12, kMonths);
    ImPlot::SetupAxisLimits(axis, 1, 367, ImPlotCond_Always);
}

std::string percent(double share) {
    char b[32];
    std::snprintf(b, sizeof(b), share >= 0.0995 ? "%.1f%%" : "%.2f%%", share * 100);
    return b;
}

} // namespace

void App::uiAnalysis() {
    if (!showAnalysis_) return;
    // The first time: a tab next to the series chart.
    if (const ImGuiWindow* ts = ImGui::FindWindowByName("Time series"); ts && ts->DockId)
        ImGui::SetNextWindowDockID(ts->DockId, ImGuiCond_FirstUseEver);
    ImGui::SetNextWindowSize(ImVec2(720, 420), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Analysis", &showAnalysis_)) {
        ImGui::End();
        return;
    }
    if (!s_) {
        ImGui::TextDisabled("Open a series.");
        ImGui::End();
        return;
    }
    if (ImGui::BeginTabBar("##analysis")) {
        auto flags = [&](int tab) { return an_.selectTab == tab ? ImGuiTabItemFlags_SetSelected : 0; };
        if (ImGui::BeginTabItem("Seasonal", nullptr, flags(0))) {
            uiSeasonal();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Classes", nullptr, flags(1))) {
            uiClassTimeline();
            ImGui::EndTabItem();
        }
        if (ImGui::BeginTabItem("Scatter", nullptr, flags(2))) {
            uiScatter();
            ImGui::EndTabItem();
        }
        an_.selectTab = -1;
        ImGui::EndTabBar();
    }
    ImGui::End();
}

// ---------------------------------------------------------------------------
// Seasonal
// ---------------------------------------------------------------------------

void App::uiSeasonal() {
    const CubeInfo& info = *s_->info;
    if (!info.timeIsDate) {
        ImGui::TextDisabled("The dates of this series are not known (it is ordered by index).");
        return;
    }
    // The series: the cursor, a pin or the ROI mean, as in the chart.
    struct Source {
        std::string key, label;
        const std::vector<float>* values;
    };
    std::vector<Source> sources;
    if (hover_.x >= 0) {
        char l[64];
        std::snprintf(l, sizeof(l), "Cursor (%d, %d)", hover_.x, hover_.y);
        sources.push_back({"cursor", l, &hover_.values});
    }
    for (const SeriesView& p : pins_) {
        char l[64];
        std::snprintf(l, sizeof(l), "Pin %d (%d, %d)", p.id, p.x, p.y);
        sources.push_back({"pin" + std::to_string(p.id), l, &p.values});
    }
    if (!roiMean_.empty()) sources.push_back({"roi", "ROI mean", &roiMean_});
    if (sources.empty()) {
        ImGui::TextDisabled("Hover over the map, place a pin or draw an ROI.");
        return;
    }
    const Source* src = &sources.front();
    for (const Source& s : sources)
        if (s.key == an_.seasonalSource) src = &s;
    ImGui::SetNextItemWidth(ImGui::GetFontSize() * 13);
    if (ImGui::BeginCombo("##source", src->label.c_str())) {
        for (const Source& s : sources)
            if (ImGui::Selectable(s.label.c_str(), &s == src)) an_.seasonalSource = s.key;
        ImGui::EndCombo();
    }
    ImGui::SetItemTooltip("The series shown (the cursor follows the mouse over the map)");
    ImGui::SameLine();
    ImGui::RadioButton("Years overlaid", &an_.seasonalView, 0);
    ImGui::SameLine();
    ImGui::RadioButton("Year x day heatmap", &an_.seasonalView, 1);

    const std::vector<float>& v = *src->values;
    std::vector<double> times(size_t(info.T()));
    for (int t = 0; t < info.T(); ++t) times[t] = info.layers[t].time;
    if (int(v.size()) != info.T()) return;
    const int perYear = observationsPerYear(times);
    if (perYear < 2) {
        ImGui::TextDisabled("About one observation per year: nothing to compare within a year.");
        ImGui::TextDisabled("(For series with several dates a year: monthly, 16-day composites...)");
        return;
    }
    int curYear = 0, curDay = 0;
    yearAndDay(times[t_], curYear, curDay);

    if (an_.seasonalView == 0) {
        // One line per year (colour = year), the current date's year on top;
        // the mean of every year by month, with +/- one standard deviation.
        int y0 = 0, y1 = 0;
        int day = 0;
        yearAndDay(times.front(), y0, day);
        yearAndDay(times.back(), y1, day);
        double mSum[12] = {}, mSq[12] = {};
        int mN[12] = {};
        const float scaleW = ImGui::GetFontSize() * 5;
        if (ImPlot::BeginPlot("##years", ImVec2(-scaleW, -1), ImPlotFlags_NoTitle)) {
            ImPlot::SetupAxes("day of the year", nullptr, ImPlotAxisFlags_None, ImPlotAxisFlags_AutoFit);
            setupMonthAxis(ImAxis_X1);
            ImPlot::SetupLegend(ImPlotLocation_NorthWest);
            std::vector<double> xs, ys;
            auto flushYear = [&](int year) {
                if (xs.empty()) return;
                const bool current = year == curYear;
                const float k = y1 > y0 ? float(year - y0) / float(y1 - y0) : 0.5f;
                ImPlotSpec spec;
                spec.LineColor = theme::onPlot(ImPlot::SampleColormap(k, ImPlotColormap_Viridis));
                spec.LineWeight = current ? 2.5f : 1.0f;
                spec.Marker = ImPlotMarker_Circle;
                spec.MarkerSize = current ? 3.0f : 1.5f;
                spec.MarkerFillColor = spec.LineColor;
                char label[48];
                std::snprintf(label, sizeof(label), current ? "%d (current date)###y%d" : "##y%d", year, year);
                if (!current) spec.Flags = ImPlotItemFlags_NoLegend;
                ImPlot::PlotLine(label, xs.data(), ys.data(), int(xs.size()), spec);
                xs.clear();
                ys.clear();
            };
            int year = -1;
            for (int t = 0; t < info.T(); ++t) {
                int y, d;
                yearAndDay(times[t], y, d);
                if (y != year) flushYear(year);
                year = y;
                if (!std::isfinite(v[t])) continue;
                xs.push_back(d);
                ys.push_back(v[t]);
                const int m = monthOf(d);
                mSum[m] += v[t];
                mSq[m] += double(v[t]) * v[t];
                ++mN[m];
            }
            flushYear(year);
            // Climatology: the mean of all years, month by month.
            std::vector<double> mx, mean, lo, hi;
            for (int m = 0; m < 12; ++m) {
                if (!mN[m]) continue;
                const double mu = mSum[m] / mN[m];
                const double sd = mN[m] > 1 ? std::sqrt(std::max(0.0, (mSq[m] - mN[m] * mu * mu) / (mN[m] - 1))) : 0.0;
                mx.push_back(0.5 * (kMonthStart[m] + kMonthStart[m + 1]));
                mean.push_back(mu);
                lo.push_back(mu - sd);
                hi.push_back(mu + sd);
            }
            if (!mx.empty()) {
                const ImVec4 grey = theme::onPlot(ImVec4(0.85f, 0.85f, 0.85f, 1));
                ImPlotSpec band;
                band.FillColor = grey;
                band.FillAlpha = 0.15f;
                ImPlot::PlotShaded("Mean of all years (+/- 1 sd)", mx.data(), lo.data(), hi.data(), int(mx.size()), band);
                ImPlotSpec line;
                line.LineColor = grey;
                line.LineWeight = 2.0f;
                ImPlot::PlotLine("Mean of all years (+/- 1 sd)", mx.data(), mean.data(), int(mx.size()), line);
            }
            ImPlot::EndPlot();
        }
        ImGui::SameLine();
        ImPlot::ColormapScale("##yearscale", y0, y1, ImVec2(scaleW - ImGui::GetStyle().ItemSpacing.x, -1), "%.0f",
                              ImPlotColormapScaleFlags_None, ImPlotColormap_Viridis);
        return;
    }

    // Heatmap: years (newest on top) x parts of the year, in the layer's colours.
    const int bins = std::clamp(perYear, 2, 46);
    const SeasonalGrid g = seasonalGrid(times, v, bins);
    float lo = INFINITY, hi = -INFINITY;
    for (float m : g.mean)
        if (std::isfinite(m)) lo = std::min(lo, m), hi = std::max(hi, m);
    if (!(lo < hi)) hi = lo + 1;
    const int cmap = cmap_[ModeValue];
    const float scaleW = ImGui::GetFontSize() * 5;
    if (ImPlot::BeginPlot("##heat", ImVec2(-scaleW, -1), ImPlotFlags_NoTitle | ImPlotFlags_NoLegend)) {
        ImPlot::SetupAxes("day of the year", nullptr);
        setupMonthAxis(ImAxis_X1);
        ImPlot::SetupAxisLimits(ImAxis_Y1, g.year0 - 0.5, g.year0 + g.years - 0.5, ImPlotCond_Always);
        ImPlot::SetupAxisFormat(ImAxis_Y1, "%.0f");
        ImPlot::SetupFinish();
        ImDrawList* dl = ImPlot::GetPlotDrawList();
        ImPlot::PushPlotClipRect();
        const ImPlotPoint mouse = ImPlot::GetPlotMousePos();
        int hoverK = -1;
        for (int r = 0; r < g.years; ++r)
            for (int b = 0; b < g.bins; ++b) {
                const size_t k = size_t(r) * g.bins + b;
                const double x0 = 1 + 366.0 * b / g.bins, x1 = 1 + 366.0 * (b + 1) / g.bins;
                const double yb = g.year0 + r - 0.5, yt = yb + 1;
                if (ImPlot::IsPlotHovered() && mouse.x >= x0 && mouse.x < x1 && mouse.y >= yb && mouse.y < yt)
                    hoverK = int(k);
                if (!std::isfinite(g.mean[k])) continue;
                const ImVec4 c = ImPlot::SampleColormap(std::clamp((g.mean[k] - lo) / (hi - lo), 0.0f, 1.0f), cmap);
                dl->AddRectFilled(ImPlot::PlotToPixels(x0, yt), ImPlot::PlotToPixels(x1, yb),
                                  ImGui::ColorConvertFloat4ToU32(c));
            }
        // The current date's cell.
        const int cb = std::min(g.bins - 1, (curDay - 1) * g.bins / 366);
        const double cx0 = 1 + 366.0 * cb / g.bins, cx1 = 1 + 366.0 * (cb + 1) / g.bins;
        dl->AddRect(ImPlot::PlotToPixels(cx0, curYear + 0.5), ImPlot::PlotToPixels(cx1, curYear - 0.5),
                    ImGui::ColorConvertFloat4ToU32(theme::onPlot(ImVec4(1.0f, 0.6f, 0.2f, 1))), 0, 0, 2.0f);
        ImPlot::PopPlotClipRect();
        if (hoverK >= 0) {
            const int r = hoverK / g.bins, b = hoverK % g.bins;
            const int d0 = 1 + 366 * b / g.bins, d1 = 366 * (b + 1) / g.bins;
            if (std::isfinite(g.mean[hoverK]))
                ImGui::SetTooltip("%d, days %d-%d (%s)\n%.4g (mean of %d)", g.year0 + r, d0, d1,
                                  kMonths[monthOf(d0)], g.mean[hoverK], g.n[hoverK]);
            else
                ImGui::SetTooltip("%d, days %d-%d (%s)\nno observation", g.year0 + r, d0, d1, kMonths[monthOf(d0)]);
        }
        ImPlot::EndPlot();
    }
    ImGui::SameLine();
    ImPlot::ColormapScale("##heatscale", lo, hi, ImVec2(scaleW - ImGui::GetStyle().ItemSpacing.x, -1), "%.3g",
                          ImPlotColormapScaleFlags_None, cmap);
}

// ---------------------------------------------------------------------------
// Classes over time and transitions
// ---------------------------------------------------------------------------

// Counts a few dates of the class timeline (all of it in steps, a frame at a
// time, so a large overview never stalls the interface). True when complete.
bool App::countClassTimeline(const SeriesLayer& L, ClassTimeline& c, double budgetMs) {
    const Session& S = *L.session;
    const Overview& ov = S.overview;
    const LayerClasses& C = L.classes;
    uint64_t key = S.info->id * 1000003u + uint64_t(ov.T) * 131u + C.list.size();
    for (const ClassEntry& e : C.list) key = key * 31 + uint64_t(e.value);
    if (c.key != key) {
        c = ClassTimeline{};
        c.key = key;
        for (const ClassEntry& e : C.list) c.values.push_back(e.value);
        c.counts.assign(size_t(ov.T), std::vector<uint32_t>(c.values.size(), 0));
        c.valid.assign(size_t(ov.T), 0);
    }
    if (c.next >= ov.T) return true;
    std::unordered_map<int, int> index;
    for (size_t k = 0; k < c.values.size(); ++k) index[c.values[k]] = int(k);
    const auto t0 = std::chrono::steady_clock::now();
    const size_t per = size_t(ov.w) * ov.h;
    while (c.next < ov.T) {
        const int t = c.next;
        if (!S.gpu.loaded[t]) return false; // the overview is still reading it
        const float* px = ov.layer(t);
        std::vector<uint32_t>& n = c.counts[t];
        int lastV = INT32_MIN, lastK = -1;
        for (size_t i = 0; i < per; ++i) {
            if (!std::isfinite(px[i])) continue;
            const int val = int(std::lround(px[i]));
            if (val != lastV) {
                lastV = val;
                const auto it = index.find(val);
                lastK = it == index.end() ? -1 : it->second;
            }
            if (lastK >= 0) {
                ++n[lastK];
                ++c.valid[t];
            }
        }
        ++c.next;
        if (std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count() > budgetMs) break;
    }
    return c.next >= ov.T;
}

void App::countTransitions(const SeriesLayer& L, int from, int to, Transitions& tr) const {
    const Session& S = *L.session;
    const Overview& ov = S.overview;
    const LayerClasses& C = L.classes;
    tr = Transitions{};
    tr.from = from;
    tr.to = to;
    for (const ClassEntry& e : C.list) tr.values.push_back(e.value);
    const size_t K = tr.values.size();
    tr.m.assign(K * K, 0);
    if (!S.gpu.loaded[from] || !S.gpu.loaded[to]) return;
    std::unordered_map<int, int> index;
    for (size_t k = 0; k < K; ++k) index[tr.values[k]] = int(k);
    const float* a = ov.layer(from);
    const float* b = ov.layer(to);
    for (size_t i = 0, n = size_t(ov.w) * ov.h; i < n; ++i) {
        if (!std::isfinite(a[i]) || !std::isfinite(b[i])) continue;
        const auto ia = index.find(int(std::lround(a[i]))), ib = index.find(int(std::lround(b[i])));
        if (ia == index.end() || ib == index.end()) continue;
        ++tr.m[size_t(ia->second) * K + ib->second];
        ++tr.both;
    }
    tr.done = true;
}

void App::uiClassTimeline() {
    const SeriesLayer* L = activeLayer();
    const LayerClasses* C = activeClasses();
    if (!L || !C || C->list.empty()) {
        ImGui::TextDisabled("For categorical layers (land cover, classes): the share of each class at every\n"
                            "date and the transitions between two dates. Display > Categorical switches it on.");
        return;
    }
    const CubeInfo& info = *s_->info;
    const int T = info.T();
    ClassTimeline& c = an_.classes;
    if (!countClassTimeline(*L, c, 8.0)) {
        ImGui::TextDisabled("Counting the classes: %d of %d dates (the overview's pixels)...", c.next, T);
        glfwPostEmptyEvent(); // the next dates at the next frame
    }
    const size_t K = c.values.size();
    auto entry = [&](size_t k) { return C->find(c.values[k]); };

    ImGui::Checkbox("Stacked", &an_.classStacked);
    ImGui::SetItemTooltip("Stacked areas (the shares add up to 100%%), or one line per class");
    ImGui::SameLine();
    if (ImGui::Button("Copy CSV##shares")) {
        std::string csv = "date";
        for (size_t k = 0; k < K; ++k) csv += "," + (entry(k) ? entry(k)->name : std::to_string(c.values[k]));
        csv += "\n";
        for (int t = 0; t < c.next; ++t) {
            csv += info.layers[t].label;
            for (size_t k = 0; k < K; ++k) {
                char b[32];
                std::snprintf(b, sizeof(b), ",%.4f", c.valid[t] ? 100.0 * c.counts[t][k] / c.valid[t] : 0.0);
                csv += b;
            }
            csv += "\n";
        }
        ImGui::SetClipboardText(csv.c_str());
    }
    ImGui::SetItemTooltip("Copies the share (%%) of each class at every date");
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    ImGui::SetItemTooltip("Share of the pixels with a class at each date, counted on the overview\n"
                          "(%d x %d pixels of the series, every %.3g source pixels).",
                          s_->overview.w, s_->overview.h, s_->overview.factor);

    const float avail = ImGui::GetContentRegionAvail().x;
    const float chartW = std::max(avail * 0.55f, avail - ImGui::GetFontSize() * 34);
    if (ImPlot::BeginPlot("##shares", ImVec2(chartW, -1), ImPlotFlags_NoTitle)) {
        ImPlot::SetupAxes(info.timeIsDate ? nullptr : "date (index)", "% of the pixels", ImPlotAxisFlags_AutoFit,
                          an_.classStacked ? ImPlotAxisFlags_None : ImPlotAxisFlags_AutoFit);
        if (info.timeIsDate) ImPlot::SetupAxisScale(ImAxis_X1, ImPlotScale_Time);
        if (an_.classStacked) ImPlot::SetupAxisLimits(ImAxis_Y1, 0, 100, ImPlotCond_Always);
        ImPlot::SetupLegend(ImPlotLocation_NorthEast, ImPlotLegendFlags_Outside);
        const int n = c.next;
        std::vector<double> lower(size_t(n), 0.0), upper(size_t(n), 0.0), share(size_t(n), 0.0);
        for (size_t k = 0; k < K; ++k) {
            const ClassEntry* e = entry(k);
            if (e && !e->visible) continue;
            for (int t = 0; t < n; ++t) {
                share[t] = c.valid[t] ? 100.0 * c.counts[t][k] / c.valid[t] : 0.0;
                upper[t] = lower[t] + share[t];
            }
            const std::string label = e ? e->name : std::to_string(c.values[k]);
            const ImVec4 col = e ? e->color : ImVec4(0.6f, 0.6f, 0.6f, 1);
            ImPlotSpec spec;
            spec.LineColor = theme::onPlot(col);
            spec.FillColor = col;
            spec.FillAlpha = 0.85f;
            if (an_.classStacked) {
                ImPlot::PlotShaded(label.c_str(), xs_.data(), lower.data(), upper.data(), n, spec);
                lower = upper;
            } else {
                spec.LineWeight = 2.0f;
                spec.Marker = ImPlotMarker_Circle;
                spec.MarkerSize = 2.5f;
                ImPlot::PlotLine(label.c_str(), xs_.data(), share.data(), n, spec);
            }
        }
        double tx = xs_[t_];
        const ImVec4 orange = theme::onPlot(ImVec4(1.0f, 0.6f, 0.2f, 1));
        if (ImPlot::DragLineX(0, &tx, orange, 1.5f, ImPlotDragToolFlags_NoFit)) setT(nearestIndex(xs_, tx));
        if (ImPlot::IsPlotHovered()) {
            const int t = nearestIndex(xs_, ImPlot::GetPlotMousePos().x);
            if (t < n) {
                std::string tip = info.layers[t].label;
                for (size_t k = 0; k < K; ++k)
                    if (c.counts[t][k]) {
                        const ClassEntry* e = entry(k);
                        tip += "\n" + (e ? e->name : std::to_string(c.values[k])) + ": " +
                               percent(double(c.counts[t][k]) / c.valid[t]);
                    }
                ImGui::SetTooltip("%s", tip.c_str());
            }
            if (ImGui::IsMouseReleased(ImGuiMouseButton_Left) && ImGui::GetIO().MouseDragMaxDistanceSqr[0] < 16)
                setT(nearestIndex(xs_, ImPlot::GetPlotMousePos().x));
        }
        ImPlot::EndPlot();
    }

    // Transitions between two dates, pixel by pixel.
    ImGui::SameLine();
    ImGui::BeginChild("##transitions", ImVec2(0, 0));
    ImGui::TextUnformatted("Transitions");
    int& from = an_.transFrom;
    from = std::clamp(from, 0, T - 1);
    const int to = an_.transTo < 0 ? t_ : std::clamp(an_.transTo, 0, T - 1);
    ImGui::SetNextItemWidth(ImGui::GetFontSize() * 8);
    if (ImGui::BeginCombo("from", info.layers[from].label.c_str())) {
        for (int t = 0; t < T; ++t)
            if (ImGui::Selectable(info.layers[t].label.c_str(), t == from)) from = t;
        ImGui::EndCombo();
    }
    ImGui::SameLine();
    ImGui::SetNextItemWidth(ImGui::GetFontSize() * 8);
    const std::string toLabel = an_.transTo < 0 ? info.layers[to].label + " (current)" : info.layers[to].label;
    if (ImGui::BeginCombo("to", toLabel.c_str())) {
        if (ImGui::Selectable("The current date", an_.transTo < 0)) an_.transTo = -1;
        for (int t = 0; t < T; ++t)
            if (ImGui::Selectable(info.layers[t].label.c_str(), an_.transTo == t)) an_.transTo = t;
        ImGui::EndCombo();
    }
    Transitions& tr = an_.trans;
    if (tr.key != c.key || tr.from != from || tr.to != to || !tr.done) {
        countTransitions(*L, from, to, tr);
        tr.key = c.key;
    }
    if (!tr.done) {
        ImGui::TextDisabled("Waiting for both dates in the overview...");
        ImGui::EndChild();
        return;
    }
    const size_t TK = tr.values.size();
    uint64_t changed = 0;
    std::vector<std::pair<uint32_t, size_t>> moves; // count, from * K + to
    for (size_t i = 0; i < TK; ++i)
        for (size_t j = 0; j < TK; ++j)
            if (i != j && tr.m[i * TK + j]) {
                changed += tr.m[i * TK + j];
                moves.push_back({tr.m[i * TK + j], i * TK + j});
            }
    std::sort(moves.begin(), moves.end(), [](const auto& a, const auto& b) { return a.first > b.first; });
    const double both = std::max<double>(1, double(tr.both));
    ImGui::Text("Changed: %s of the pixels with a class at both dates", percent(changed / both).c_str());
    auto nameOf = [&](size_t k) {
        const ClassEntry* e = C->find(tr.values[k]);
        return e ? e->name : std::to_string(tr.values[k]);
    };
    for (size_t i = 0; i < moves.size() && i < 6; ++i) {
        const size_t a = moves[i].second / TK, b = moves[i].second % TK;
        ImGui::BulletText("%s -> %s: %s", nameOf(a).c_str(), nameOf(b).c_str(), percent(moves[i].first / both).c_str());
    }
    ImGui::SameLine();
    if (ImGui::SmallButton("Copy CSV##matrix")) {
        std::string csv = "from \\ to";
        for (size_t j = 0; j < TK; ++j) csv += "," + nameOf(j);
        csv += "\n";
        for (size_t i = 0; i < TK; ++i) {
            csv += nameOf(i);
            for (size_t j = 0; j < TK; ++j) csv += "," + std::to_string(tr.m[i * TK + j]);
            csv += "\n";
        }
        ImGui::SetClipboardText(csv.c_str());
    }
    ImGui::SetItemTooltip("Copies the matrix (pixel counts: rows = from, columns = to)");

    // The matrix: rows = the class at "from", columns = at "to", % of the pixels.
    std::vector<size_t> rows, cols;
    for (size_t i = 0; i < TK; ++i) {
        uint64_t r = 0, cl = 0;
        for (size_t j = 0; j < TK; ++j) r += tr.m[i * TK + j], cl += tr.m[j * TK + i];
        if (r) rows.push_back(i);
        if (cl) cols.push_back(i);
    }
    if (cols.size() > 30) {
        ImGui::TextDisabled("%d classes: too many for a table (Copy CSV has them all).", int(cols.size()));
        ImGui::EndChild();
        return;
    }
    const ImGuiTableFlags flags = ImGuiTableFlags_Borders | ImGuiTableFlags_ScrollX | ImGuiTableFlags_ScrollY |
                                  ImGuiTableFlags_SizingFixedFit;
    if (ImGui::BeginTable("##matrix", int(cols.size()) + 1, flags)) {
        ImGui::TableSetupScrollFreeze(1, 1);
        ImGui::TableSetupColumn("from \\ to");
        for (size_t j : cols) ImGui::TableSetupColumn(nameOf(j).c_str());
        ImGui::TableHeadersRow();
        const ImVec4 accent = theme::accent();
        for (size_t i : rows) {
            ImGui::PushID(int(i));
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            if (const ClassEntry* e = C->find(tr.values[i])) {
                ImGui::ColorButton("##c", e->color, ImGuiColorEditFlags_NoTooltip, ImVec2(ImGui::GetFontSize(), ImGui::GetFontSize()));
                ImGui::SameLine();
            }
            ImGui::TextUnformatted(nameOf(i).c_str());
            for (size_t j : cols) {
                ImGui::TableNextColumn();
                const uint32_t n = tr.m[i * TK + j];
                if (!n) continue;
                const double s = n / both;
                if (i != j) // changes stand out, the stronger the more
                    ImGui::TableSetBgColor(ImGuiTableBgTarget_CellBg,
                                           ImGui::ColorConvertFloat4ToU32(ImVec4(accent.x, accent.y, accent.z,
                                                                                 0.15f + 0.6f * float(std::sqrt(s)))));
                ImGui::TextUnformatted(percent(s).c_str());
                if (ImGui::IsItemHovered())
                    ImGui::SetTooltip("%s -> %s\n%u pixels of the overview", nameOf(i).c_str(), nameOf(j).c_str(), n);
            }
            ImGui::PopID();
        }
        ImGui::EndTable();
    }
    ImGui::EndChild();
}

// ---------------------------------------------------------------------------
// Scatter
// ---------------------------------------------------------------------------

// Pixels of the active layer (the ROI, or all of it), each with the value of
// the X and Y layer/date at the same place, from the overviews.
void App::computeScatter(ScatterData& sc, const SeriesLayer& X, int tx, const SeriesLayer& Y, int ty) const {
    sc.x.clear();
    sc.y.clear();
    const CubeInfo& info = *s_->info;
    int x0 = 0, y0 = 0, x1 = info.width, y1 = info.height;
    if (roiRect_) {
        x0 = roiRectXY_[0], y0 = roiRectXY_[1], x1 = roiRectXY_[2], y1 = roiRectXY_[3];
    }
    const double n = double(x1 - x0) * (y1 - y0);
    const int step = std::max(1, int(std::ceil(std::sqrt(n / 250000.0)))); // up to ~250k pixels
    auto sample = [&](const SeriesLayer& L, int t, double ax, double ay, float& out) {
        const Overview& ov = L.session->overview;
        const CubeInfo& li = *L.session->info;
        if (!L.session->gpu.loaded[t]) return false;
        // active pixel -> this layer's pixel (its ax.. map this layer to the active one)
        const double lx = &L == activeLayer() ? ax : (ax - L.ax) / L.bx, ly = &L == activeLayer() ? ay : (ay - L.ay) / L.by;
        if (lx < 0 || ly < 0 || lx >= li.width || ly >= li.height) return false;
        const int ox = std::min(ov.w - 1, int(lx * ov.w / li.width)), oy = std::min(ov.h - 1, int(ly * ov.h / li.height));
        out = ov.at(t, ox, oy);
        return std::isfinite(out);
    };
    for (int y = y0; y < y1; y += step)
        for (int x = x0; x < x1; x += step) {
            float a, b;
            if (sample(X, tx, x + 0.5, y + 0.5, a) && sample(Y, ty, x + 0.5, y + 0.5, b)) {
                sc.x.push_back(a);
                sc.y.push_back(b);
            }
        }
    // Pearson r, the least squares line y = a + b x, the mean difference and its RMS.
    const size_t m = sc.x.size();
    sc.n = int(m);
    double sx = 0, sy = 0;
    for (size_t i = 0; i < m; ++i) sx += sc.x[i], sy += sc.y[i];
    const double mx = m ? sx / m : 0, my = m ? sy / m : 0;
    double sxx = 0, syy = 0, sxy = 0, sd = 0;
    for (size_t i = 0; i < m; ++i) {
        const double dx = sc.x[i] - mx, dy = sc.y[i] - my;
        sxx += dx * dx, syy += dy * dy, sxy += dx * dy;
        sd += (sc.y[i] - sc.x[i]) * (sc.y[i] - sc.x[i]);
    }
    sc.r = sxx > 0 && syy > 0 ? sxy / std::sqrt(sxx * syy) : NAN;
    sc.b = sxx > 0 ? sxy / sxx : NAN;
    sc.a = my - sc.b * mx;
    sc.meanDiff = my - mx;
    sc.rmsd = m ? std::sqrt(sd / m) : NAN;
    sc.step = step;
}

void App::uiScatter() {
    // Layers on the active layer's grid by an affine map (other CRSs: not here).
    std::vector<const SeriesLayer*> usable;
    for (const SeriesLayer& L : layers_)
        if (&L == activeLayer() || (L.aligned && !L.reproj)) usable.push_back(&L);
    auto pick = [&](const char* id, uint64_t& cube, int& date) -> const SeriesLayer* {
        const SeriesLayer* L = activeLayer();
        for (const SeriesLayer* u : usable)
            if (u->session->info->id == cube) L = u;
        cube = L->session->info->id;
        const CubeInfo& li = *L->session->info;
        ImGui::PushID(id);
        ImGui::TextUnformatted(id);
        ImGui::SameLine();
        ImGui::SetNextItemWidth(ImGui::GetFontSize() * 11);
        if (ImGui::BeginCombo("##layer", L->name.c_str())) {
            for (const SeriesLayer* u : usable)
                if (ImGui::Selectable(u->name.c_str(), u == L)) cube = u->session->info->id;
            ImGui::EndCombo();
        }
        ImGui::SameLine();
        // Dates: a fixed one, the current date or the one before it (when the layer is the active one).
        const bool active = L == activeLayer();
        const int T = li.T();
        const int cur = active ? t_ : std::clamp(L->disp.t, 0, T - 1); // other layers follow the nearest date
        const int t = date == -1 ? cur : date == -2 ? std::max(0, cur - 1) : std::clamp(date, 0, T - 1);
        const std::string label = date == -1   ? li.layers[t].label + " (current)"
                                  : date == -2 ? li.layers[t].label + " (previous)"
                                               : li.layers[t].label;
        ImGui::SetNextItemWidth(ImGui::GetFontSize() * 10);
        if (ImGui::BeginCombo("##date", label.c_str())) {
            if (ImGui::Selectable("The current date", date == -1)) date = -1;
            if (ImGui::Selectable("The date before it", date == -2)) date = -2;
            for (int k = 0; k < T; ++k)
                if (ImGui::Selectable(li.layers[k].label.c_str(), date == k)) date = k;
            ImGui::EndCombo();
        }
        ImGui::PopID();
        an_.scatterT[id[0] == 'X' ? 0 : 1] = t;
        return L;
    };
    const SeriesLayer* X = pick("X", an_.scatterCube[0], an_.scatterDate[0]);
    ImGui::SameLine(0, ImGui::GetFontSize() * 2);
    const SeriesLayer* Y = pick("Y", an_.scatterCube[1], an_.scatterDate[1]);
    const int tx = an_.scatterT[0], ty = an_.scatterT[1];
    if (usable.size() < layers_.size())
        ImGui::TextDisabled("Layers in another CRS are not listed (their pixels are not on this grid).");

    ScatterData& sc = an_.scatter;
    const uint64_t key = (((X->session->info->id * 1000003u + uint64_t(tx)) * 1000003u + Y->session->info->id) * 1000003u +
                          uint64_t(ty)) * 1000003u + roiGen_ * 31u + uint64_t(roiRect_) + dataVersion_ * 7919u +
                         uint64_t(X->session->overview.layersDone()) * 13u + uint64_t(Y->session->overview.layersDone());
    if (sc.key != key || sc.activeCube != s_->info->id) {
        computeScatter(sc, *X, tx, *Y, ty);
        sc.key = key;
        sc.activeCube = s_->info->id;
    }
    ImGui::TextDisabled("%s: %d pixels%s of the overview", roiRect_ ? "ROI" : "Whole image", sc.n,
                        sc.step > 1 ? " (a regular sample)" : "");
    if (sc.n >= 2)
        ImGui::Text("r = %.4f   R2 = %.4f   Y = %.4g + %.4g X   mean(Y - X) = %.4g   RMS(Y - X) = %.4g", sc.r,
                    sc.r * sc.r, sc.a, sc.b, sc.meanDiff, sc.rmsd);
    if (sc.n < 2) {
        ImGui::TextDisabled("No pixel has both values here.");
        return;
    }
    char xl[160], yl[160];
    std::snprintf(xl, sizeof(xl), "%s, %s", X->name.c_str(), X->session->info->layers[tx].label.c_str());
    std::snprintf(yl, sizeof(yl), "%s, %s", Y->name.c_str(), Y->session->info->layers[ty].label.c_str());
    const float scaleW = ImGui::GetFontSize() * 5;
    double lo[2] = {INFINITY, INFINITY}, hi[2] = {-INFINITY, -INFINITY};
    for (int i = 0; i < sc.n; ++i) {
        lo[0] = std::min(lo[0], sc.x[i]), hi[0] = std::max(hi[0], sc.x[i]);
        lo[1] = std::min(lo[1], sc.y[i]), hi[1] = std::max(hi[1], sc.y[i]);
    }
    for (int k = 0; k < 2; ++k)
        if (!(hi[k] > lo[k])) lo[k] -= 0.5, hi[k] += 0.5;
    double maxCount = 0;
    if (ImPlot::BeginPlot("##scatter", ImVec2(-scaleW, -1), ImPlotFlags_NoTitle)) {
        ImPlot::SetupAxes(xl, yl);
        ImPlot::SetupAxesLimits(lo[0], hi[0], lo[1], hi[1], ImPlotCond_Always);
        ImPlot::SetupLegend(ImPlotLocation_NorthWest);
        ImPlot::PushColormap(ImPlotColormap_Viridis);
        ImPlotSpec spec;
        spec.Flags = ImPlotItemFlags_NoLegend;
        maxCount = ImPlot::PlotHistogram2D("##density", sc.x.data(), sc.y.data(), sc.n, 120, 120,
                                           ImPlotRect(lo[0], hi[0], lo[1], hi[1]), spec);
        ImPlot::PopColormap();
        const double l = std::max(lo[0], lo[1]), h = std::min(hi[0], hi[1]);
        if (h > l) {
            const double dx[2] = {l, h};
            ImPlotSpec one;
            one.LineColor = theme::onPlot(ImVec4(0.75f, 0.75f, 0.75f, 1));
            ImPlot::PlotLine("Y = X", dx, dx, 2, one);
        }
        if (std::isfinite(sc.b)) {
            const double fx[2] = {lo[0], hi[0]}, fy[2] = {sc.a + sc.b * lo[0], sc.a + sc.b * hi[0]};
            ImPlotSpec fit;
            fit.LineColor = theme::onPlot(ImVec4(1.0f, 0.6f, 0.2f, 1));
            fit.LineWeight = 2.0f;
            ImPlot::PlotLine("Least squares", fx, fy, 2, fit);
        }
        ImPlot::EndPlot();
    }
    ImGui::SameLine();
    ImPlot::ColormapScale("pixels", 0, std::max(1.0, maxCount), ImVec2(scaleW - ImGui::GetStyle().ItemSpacing.x, -1),
                          "%.0f", ImPlotColormapScaleFlags_None, ImPlotColormap_Viridis);
}
