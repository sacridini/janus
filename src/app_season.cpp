// Season and temporal composites. The season is a window of days of the year
// (e.g. 150-270, or Nov-Feb across the new year): the dates outside it are
// left out of the temporal statistics (their time is NaN in the cube's dates
// on the GPU), the composites, the playback and the arrows, and the chart
// highlights it. A composite ("Temporal composite" mode) reduces the dates of a
// window around the shown date (its year, some days before and after, or the
// whole series) to one map: median, mean, maximum, minimum or the number of
// observations, computed in the map shader over the overview's dates every
// time the map is drawn, so changing the date, the season or the reducer
// costs one frame.
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>

#include <implot.h>

namespace {

const char* kReducers[] = {"Median", "Mean", "Maximum", "Minimum", "Observations"};
const char* kMonths[] = {"Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"};

int dayOfYear(double unixSec, int* year = nullptr) {
    int y;
    unsigned m, d;
    const int64_t day = int64_t(std::floor(unixSec / 86400.0));
    civilFromDays(day, y, m, d);
    if (year) *year = y;
    return int(day - daysFromCivil(y, 1, 1)) + 1;
}

// "Jun 1" for a day of the year (of a common year).
std::string monthDay(int doy) {
    int y;
    unsigned m, d;
    civilFromDays(daysFromCivil(2001, 1, 1) + std::clamp(doy, 1, 366) - 1, y, m, d);
    if (doy == 366) return "Dec 31+";
    char b[16];
    std::snprintf(b, sizeof(b), "%s %u", kMonths[m - 1], d);
    return b;
}

// First day of a month of a common year.
int monthStart(int m) { return int(daysFromCivil(2001, unsigned(m), 1) - daysFromCivil(2001, 1, 1)) + 1; }
int monthEnd(int m) { return m == 12 ? 365 : monthStart(m + 1) - 1; }

} // namespace

uint64_t App::seasonKey() const { return season_.on ? uint64_t(season_.start) * 1000 + uint64_t(season_.end) + 1 : 0; }

bool App::inSeason(const CubeInfo& info, int t) const {
    if (!seasonActive(info) || t < 0 || t >= info.T()) return true;
    const int d = dayOfYear(info.layers[t].time);
    return season_.start <= season_.end ? d >= season_.start && d <= season_.end
                                        : d >= season_.start || d <= season_.end;
}

int App::seasonYear(const CubeInfo& info, int t) const {
    int y = 0;
    const int d = dayOfYear(info.layers[t].time, &y);
    // A season across the new year (e.g. Nov-Feb) belongs to the year it ends in.
    return seasonActive(info) && season_.start > season_.end && d >= season_.start ? y + 1 : y;
}

void App::temporalWindow(const CubeInfo& info, int t, int& t0, int& t1) const {
    const int T = info.T();
    t = std::clamp(t, 0, T - 1);
    t0 = 0;
    t1 = T - 1;
    if (!info.timeIsDate || info.spectral || comp_.window == 2) return;
    t0 = t1 = t;
    if (comp_.window == 0) { // the date's year (dates are sorted: its dates are contiguous)
        const int y = seasonYear(info, t);
        while (t0 > 0 && seasonYear(info, t0 - 1) == y) --t0;
        while (t1 < T - 1 && seasonYear(info, t1 + 1) == y) ++t1;
    } else {
        const double half = comp_.days * 86400.0, c = info.layers[t].time;
        while (t0 > 0 && info.layers[t0 - 1].time >= c - half) --t0;
        while (t1 < T - 1 && info.layers[t1 + 1].time <= c + half) ++t1;
    }
}

bool App::temporalParams(const Session& S, int t, DrawParams& p) const {
    int t0, t1;
    temporalWindow(*S.info, t, t0, t1);
    p.mode = ModeComposite;
    p.t = t0;
    p.tg = t1;
    p.tb = comp_.reducer;
    for (int k = t0; k <= t1; ++k)
        if (!S.gpu.loaded[k]) return false;
    return true;
}

float App::temporalAt(const Session& S, size_t pix, int t0, int t1) const {
    const CubeInfo& info = *S.info;
    const Overview& ov = S.overview;
    float n = 0, s = 0, mn = 3.0e38f, mx = -3.0e38f;
    for (int t = t0; t <= t1; ++t) {
        if (!inSeason(info, t) || !S.gpu.loaded[t]) continue;
        const float v = ov.layer(t)[pix];
        if (std::isnan(v)) continue;
        n += 1;
        s += v;
        mn = std::min(mn, v);
        mx = std::max(mx, v);
    }
    if (comp_.reducer == 4) return n;
    if (n < 1) return NAN;
    if (comp_.reducer == 1) return s / n;
    if (comp_.reducer == 2) return mx;
    if (comp_.reducer == 3) return mn;
    // The median of at most 64 values, every k-th one beyond: as the shader.
    float vals[64];
    const int k = int(std::ceil(n / 64.0f));
    int i = 0, m = 0;
    for (int t = t0; t <= t1 && m < 64; ++t) {
        if (!inSeason(info, t) || !S.gpu.loaded[t]) continue;
        const float v = ov.layer(t)[pix];
        if (std::isnan(v)) continue;
        if (i % k == 0) {
            int j = m++;
            while (j > 0 && vals[j - 1] > v) {
                vals[j] = vals[j - 1];
                --j;
            }
            vals[j] = v;
        }
        ++i;
    }
    return m % 2 == 1 ? vals[m / 2] : 0.5f * (vals[m / 2 - 1] + vals[m / 2]);
}

std::string App::seasonText() const { return monthDay(season_.start) + " - " + monthDay(season_.end); }

std::string App::temporalText(const CubeInfo& info, int t) const {
    int t0, t1;
    temporalWindow(info, t, t0, t1);
    int n = 0;
    for (int k = t0; k <= t1; ++k) n += inSeason(info, k);
    std::string window;
    if (!info.timeIsDate || info.spectral || comp_.window == 2) {
        window = "all dates";
    } else if (comp_.window == 0) {
        const int y = seasonYear(info, t);
        char b[32];
        if (seasonActive(info) && season_.start > season_.end) std::snprintf(b, sizeof(b), "%d/%02d", y - 1, y % 100);
        else std::snprintf(b, sizeof(b), "%d", y);
        window = b;
    } else {
        window = "+-" + std::to_string(comp_.days) + " days of " + info.layers[std::clamp(t, 0, info.T() - 1)].label;
    }
    std::string s = std::string(kReducers[comp_.reducer]) + " of " + std::to_string(n) + " date" + (n == 1 ? "" : "s") +
                    ", " + window;
    if (seasonActive(info)) s += " (" + seasonText() + ")";
    return s;
}

int App::stepDate(int t, int dir) const {
    if (!s_) return t + dir;
    const CubeInfo& info = *s_->info;
    const int T = info.T();
    // A composite of the date's year: year by year.
    if (mode_ == ModeComposite && comp_.window == 0 && info.timeIsDate && !info.spectral) {
        int t0, t1;
        temporalWindow(info, t, t0, t1);
        t = dir > 0 ? (t1 + 1) % T : (t0 - 1 + T) % T;
        if (dir < 0) { // the first date of that year
            temporalWindow(info, t, t0, t1);
            t = t0;
        }
        dir = dir > 0 ? 1 : -1;
        if (inSeason(info, t)) return t;
    }
    if (!seasonActive(info)) return t + dir;
    for (int k = 1; k <= T; ++k) {
        const int c = (((t + dir * k) % T) + T) % T;
        if (inSeason(info, c)) return c;
    }
    return t + dir; // no date in the season
}

void App::pumpSeason() {
    for (SeriesLayer& L : layers_) {
        Session& S = *L.session;
        const CubeInfo& info = *S.info;
        const uint64_t key = (seasonActive(info) ? seasonKey() : 0) * 1000003ull + info.id;
        if (L.seasonApplied == key) continue;
        const bool first = L.seasonApplied == 0 && !seasonActive(info);
        L.seasonApplied = key;
        if (first) continue; // a new layer without a season: its dates are already on the GPU
        std::vector<float> times = S.timesYears;
        for (int t = 0; t < info.T(); ++t)
            if (!inSeason(info, t)) times[size_t(t)] = NAN;
        S.gpu.setTimes(times);
        S.gpu.statsValid = false; // computed again (Session::pump) with the season's dates
        mapDirty_ = true;
    }
}

void App::uiSeason() {
    const CubeInfo& info = *s_->info;
    if (!info.timeIsDate || info.spectral || info.T() < 2) return;
    ImGui::SeparatorText("Season");
    bool changed = ImGui::Checkbox("Only part of the year", &season_.on);
    ImGui::SetItemTooltip("Days of the year (e.g. a crop's summer, the dry months): the dates outside\n"
                          "are left out of the temporal statistics (mean, trend...), the composites,\n"
                          "playback and the arrow keys, and the chart highlights the season.\n"
                          "Applies to every layer with dates.");
    if (season_.on) {
        struct Preset {
            const char* name;
            int m0, m1;
        };
        static const Preset presets[] = {{"Dec - Feb (DJF)", 12, 2}, {"Mar - May (MAM)", 3, 5}, {"Jun - Aug (JJA)", 6, 8},
                                         {"Sep - Nov (SON)", 9, 11}, {"Apr - Sep", 4, 9},       {"Oct - Mar", 10, 3},
                                         {"May - Oct", 5, 10},       {"Nov - Apr", 11, 4}};
        const char* current = "Months...";
        for (const Preset& p : presets)
            if (season_.start == monthStart(p.m0) && season_.end == monthEnd(p.m1)) current = p.name;
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##seasonPreset", current)) {
            for (const Preset& p : presets)
                if (ImGui::Selectable(p.name, current == p.name)) {
                    season_.start = monthStart(p.m0);
                    season_.end = monthEnd(p.m1);
                    changed = true;
                }
            ImGui::EndCombo();
        }
        const std::string f0 = "from " + monthDay(season_.start) + " (day %d)", f1 = "to " + monthDay(season_.end) + " (day %d)";
        ImGui::SetNextItemWidth(-1);
        changed |= ImGui::SliderInt("##seasonStart", &season_.start, 1, 366, f0.c_str(), ImGuiSliderFlags_AlwaysClamp);
        ImGui::SetNextItemWidth(-1);
        changed |= ImGui::SliderInt("##seasonEnd", &season_.end, 1, 366, f1.c_str(), ImGuiSliderFlags_AlwaysClamp);
        ImGui::SetItemTooltip("A start after the end wraps over the new year (e.g. Nov 1 - Feb 28);\n"
                              "such a season belongs to the year it ends in");
        int n = 0;
        for (int t = 0; t < info.T(); ++t) n += inSeason(info, t);
        if (n == 0) ImGui::TextColored(theme::warning(), "No date of the series falls in the season.");
        else ImGui::TextDisabled("%d of %d dates in the season", n, info.T());
    }
    if (changed) {
        if (!inSeason(info, t_)) setT(stepDate(t_, 1));
        for (Range& r : range_) r.key = ~0ull;
        mapDirty_ = true;
    }
}

void App::uiTemporalOptions() {
    const CubeInfo& info = *s_->info;
    ImGui::SetNextItemWidth(-1);
    if (ImGui::BeginCombo("##reducer", kReducers[comp_.reducer])) {
        for (int r = 0; r < 5; ++r)
            if (ImGui::Selectable(kReducers[r], r == comp_.reducer)) {
                comp_.reducer = r;
                range_[ModeComposite].key = ~0ull;
                mapDirty_ = true;
            }
        ImGui::EndCombo();
    }
    ImGui::SetItemTooltip("Of each pixel's valid observations in the window (and the season):\n"
                          "the median (of up to 64 of them: every k-th one beyond), the mean,\n"
                          "the maximum (e.g. greenest NDVI), the minimum, or how many there are");
    const bool dated = info.timeIsDate && !info.spectral;
    if (dated) {
        static const char* windows[] = {"The date's year", "Days around the date", "The whole series"};
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##window", windows[comp_.window])) {
            for (int w = 0; w < 3; ++w)
                if (ImGui::Selectable(windows[w], w == comp_.window)) {
                    comp_.window = w;
                    range_[ModeComposite].key = ~0ull;
                    mapDirty_ = true;
                }
            ImGui::EndCombo();
        }
        ImGui::SetItemTooltip("Which dates go into the composite: those of the shown date's year (the\n"
                              "slider and playback then go year by year), those up to some days\n"
                              "before and after it (a moving window), or all of them");
        if (comp_.window == 1) {
            ImGui::SetNextItemWidth(-1);
            if (ImGui::SliderInt("##days", &comp_.days, 1, 365, "+- %d days", ImGuiSliderFlags_Logarithmic)) {
                range_[ModeComposite].key = ~0ull;
                mapDirty_ = true;
            }
        }
    }
    ImGui::TextDisabled("%s", temporalText(info, t_).c_str());
    ImGui::TextDisabled("Computed on the GPU from the overview.");
}

void App::plotSeasonBands(const CubeInfo& info) const {
    if (!seasonActive(info)) return;
    const ImPlotRect lim = ImPlot::GetPlotLimits();
    int y0, y1;
    dayOfYear(lim.X.Min, &y0);
    dayOfYear(lim.X.Max, &y1);
    if (y1 - y0 > 400) return; // zoomed out over centuries: nothing readable
    ImDrawList* dl = ImPlot::GetPlotDrawList();
    ImVec4 c = theme::accent();
    c.w = 0.10f;
    const ImU32 col = ImGui::ColorConvertFloat4ToU32(c);
    ImPlot::PushPlotClipRect();
    for (int y = y0 - 1; y <= y1; ++y) {
        const double jan1 = double(daysFromCivil(y, 1, 1)) * 86400.0;
        const double a = jan1 + (season_.start - 1) * 86400.0;
        double b = jan1 + season_.end * 86400.0;
        if (season_.start > season_.end) b += double(daysFromCivil(y + 1, 1, 1) - daysFromCivil(y, 1, 1)) * 86400.0;
        const ImVec2 p0 = ImPlot::PlotToPixels(a, lim.Y.Max), p1 = ImPlot::PlotToPixels(b, lim.Y.Min);
        dl->AddRectFilled(p0, p1, col);
    }
    ImPlot::PopPlotClipRect();
}
