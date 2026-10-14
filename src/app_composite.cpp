// RGB composites of three bands per date, for series with several bands in each
// date's file (Landsat or Sentinel-2 surface reflectance): true colour, colour
// infrared, SWIR... drawn instead of the series' quantity, while the cube, the
// chart, the cursor and the tools keep showing that quantity (a band, NDVI...).
//
// Each date's three bands are read in the background at the overview's size
// (one date at a time, the date shown first; a few dates kept on the GPU), and,
// for the active layer zoomed past the overview, the visible window again at
// the screen's resolution. The stretch (2-98% of each band) is set from the
// first image and kept for every date, so dates compare and play as a film.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <filesystem>

#include <gdal_priv.h>
#include <imgui_internal.h>
#include <implot.h>

#include "glfw.hpp"

namespace fs = std::filesystem;

namespace {

constexpr size_t kCacheBytes = 256ull << 20; // RGBA images of dates kept on the GPU
constexpr int kMaxDetailSide = 4096;

uint64_t mixKey(uint64_t h, uint64_t v) { return (h ^ (v + 0x9e3779b97f4a7c15ull + (h << 6) + (h >> 2))) * 1099511628211ull; }

uint64_t floatBits(float f) {
    uint32_t b;
    std::memcpy(&b, &f, sizeof(b));
    return b;
}

// Band values (3 x w x h) -> RGBA8 through the stretch; transparent where a band has no data.
std::vector<unsigned char> toRgba(const std::vector<float>& v, int w, int h, const float lo[3], const float hi[3]) {
    const size_t n = size_t(w) * h;
    std::vector<unsigned char> out(n * 4);
    for (size_t i = 0; i < n; ++i) {
        bool ok = true;
        for (int c = 0; c < 3; ++c) {
            const float x = v[c * n + i];
            if (!std::isfinite(x)) {
                ok = false;
                break;
            }
            const float f = std::clamp((x - lo[c]) / (hi[c] - lo[c]), 0.0f, 1.0f);
            out[i * 4 + c] = (unsigned char)std::lround(f * 255.0f);
        }
        out[i * 4 + 3] = ok ? 255 : 0;
        if (!ok) out[i * 4] = out[i * 4 + 1] = out[i * 4 + 2] = 0;
    }
    return out;
}

} // namespace

App::BandComposite::~BandComposite() {
    for (auto& [t, I] : imgs)
        if (I.tex) Gpu::deleteTexture(I.tex);
    if (detail.tex) Gpu::deleteTexture(detail.tex);
}

void App::clearCompositeImages(BandComposite& C) {
    for (auto& [t, I] : C.imgs)
        if (I.tex) Gpu::deleteTexture(I.tex);
    C.imgs.clear();
    if (C.detail.tex) Gpu::deleteTexture(C.detail.tex);
    C.detail = {};
    C.detailT = -1;
    C.wantDetail = 0;
    C.last = {};
    mapDirty_ = viewsStale_ = true;
}

// By the band roles (names guessed, or set in a multiband tool's window), else
// by wavelength when the bands have one.
std::vector<App::CompositePreset> App::compositePresets(const CubeInfo& info) {
    struct Want {
        const char* name;
        const char* roles[3];
        double nm[3];
    };
    static const Want kWants[] = {
        {"True colour (red, green, blue)", {"red", "green", "blue"}, {650, 560, 480}},
        {"Colour infrared (NIR, red, green)", {"nir", "red", "green"}, {860, 650, 560}},
        {"SWIR: burns, soil (SWIR2, NIR, red)", {"swir2", "nir", "red"}, {2200, 860, 650}},
        {"Agriculture (SWIR1, NIR, blue)", {"swir1", "nir", "blue"}, {1610, 860, 480}},
        {"Land / water (NIR, SWIR1, red)", {"nir", "swir1", "red"}, {860, 1610, 650}},
    };
    const BandRoles roles = s_ && info.id == s_->info->id ? activeBandRoles() : guessBandRoles(info);
    std::vector<CompositePreset> out;
    for (const Want& w : kWants) {
        CompositePreset p{w.name, {0, 0, 0}};
        for (int c = 0; c < 3; ++c) {
            if (auto it = roles.find(w.roles[c]); it != roles.end()) {
                p.bands[c] = it->second;
            } else if (!info.bandNm.empty()) {
                int best = 0;
                double d = 60; // nm: no band that far is that colour
                for (size_t b = 0; b < info.bandNm.size(); ++b)
                    if (std::fabs(info.bandNm[b] - w.nm[c]) < d) {
                        d = std::fabs(info.bandNm[b] - w.nm[c]);
                        best = int(b) + 1;
                    }
                p.bands[c] = best;
            }
        }
        out.push_back(p);
    }
    return out;
}

int App::compositeDate(const SeriesLayer& L, int t) const {
    const int T = L.session->info->T();
    return std::clamp(L.comp && L.comp->fixedDate ? L.comp->date : t, 0, T - 1);
}

bool App::compositeReady(const SeriesLayer& L, int t) const {
    if (!compositeOn(L)) return true;
    const auto it = L.comp->imgs.find(compositeDate(L, t));
    return it != L.comp->imgs.end() && it->second.tex && it->second.key == L.comp->key;
}

std::string App::compositeTitle(const SeriesLayer& L, int t) const {
    const CubeInfo& info = *L.session->info;
    const BandComposite& C = *L.comp;
    auto name = [&](int b) {
        return b >= 1 && b <= int(info.bandNames.size()) ? info.bandNames[b - 1] : "band " + std::to_string(b);
    };
    char buf[300];
    std::snprintf(buf, sizeof(buf), "%s  |  RGB: %s, %s, %s", info.layers[compositeDate(L, t)].label.c_str(),
                  name(C.bands[0]).c_str(), name(C.bands[1]).c_str(), name(C.bands[2]).c_str());
    return buf;
}

std::string App::compositeStatusAt(double ax, double ay) const {
    const SeriesLayer* L = activeLayer();
    if (!L || !compositeOn(*L)) return "";
    const BandComposite::Values& V = L->comp->last;
    if (V.t != compositeDate(*L, t_) || V.v.empty() || ax < V.x0 || ay < V.y0 || ax >= V.x1 || ay >= V.y1) return "";
    const int i = std::min(V.w - 1, int((ax - V.x0) / (V.x1 - V.x0) * V.w));
    const int j = std::min(V.h - 1, int((ay - V.y0) / (V.y1 - V.y0) * V.h));
    const size_t n = size_t(V.w) * V.h, p = size_t(j) * V.w + i;
    char buf[96];
    std::snprintf(buf, sizeof(buf), "  |  RGB %.5g, %.5g, %.5g%s", V.v[p], V.v[n + p], V.v[2 * n + p],
                  V.detail ? "" : " (overview)");
    return buf;
}

// ---------------------------------------------------------------------------
// Reading and uploading
// ---------------------------------------------------------------------------

void App::pumpComposites() {
    const double now = ImGui::GetTime();
    for (SeriesLayer& L : layers_) {
        if (!L.comp) continue;
        BandComposite& C = *L.comp;
        const std::shared_ptr<const CubeInfo> info = L.session->info;
        if (C.reader && C.reader->cubeId() != info->id) { // reopened with another band selection
            if (C.reading) C.reading->f.wait();
            C.reading.reset();
            C.reader.reset();
            clearCompositeImages(C);
        }
        if (!compositeOn(L)) continue;
        // A read that ended: its image (made with the stretch of now).
        if (C.reading && C.reading->f.wait_for(std::chrono::seconds(0)) == std::future_status::ready) {
            BandComposite::Read r = std::move(*C.reading);
            C.reading.reset();
            std::vector<float> v = r.f.get();
            if (v.empty()) {
                C.error = "the bands could not be read";
            } else if (r.key == C.key || !C.stretched) {
                C.error.clear();
                const size_t n = size_t(r.w) * r.h;
                if (!C.stretched) { // 2-98% of each band, kept for every date
                    for (int c = 0; c < 3; ++c) samplePercentiles(&v[c * n], n, 400000, 0.02, 0.98, C.lo[c], C.hi[c]);
                    C.stretched = true;
                    C.key = mixKey(C.key, 7);
                }
                const std::vector<unsigned char> rgba = toRgba(v, r.w, r.h, C.lo, C.hi);
                BandComposite::Img I;
                I.tex = Gpu::createImageTexture(r.w, r.h, rgba.data());
                I.key = C.key;
                I.w = r.w;
                I.h = r.h;
                I.x0 = r.x0, I.y0 = r.y0, I.x1 = r.x1, I.y1 = r.y1;
                I.used = now;
                BandComposite::Img& slot = r.detail ? C.detail : C.imgs[r.t];
                if (slot.tex) Gpu::deleteTexture(slot.tex);
                slot = I;
                if (r.detail) C.detailT = r.t;
                C.last = {r.t, r.w, r.h, r.detail, r.x0, r.y0, r.x1, r.y1, std::move(v)};
                mapDirty_ = viewsStale_ = true;
            }
        }
        if (C.reading || L.session->deferRandomReads()) continue; // one read at a time; an HDD builds its overview
        const int nb = info->bandsPerDate;
        if (std::any_of(C.bands.begin(), C.bands.end(), [&](int b) { return b < 1 || b > nb; })) {
            C.error = "pick three bands";
            continue;
        }
        if (!C.reader) C.reader = std::make_shared<CubeReader>(info);
        // The dates drawn now: the layer's, and those of the map panels showing it.
        std::vector<int> want = {compositeDate(L, layerDate(L))};
        for (const MapView& v : views_)
            if (v.cube == info->id && !v.ownMode) want.push_back(compositeDate(L, v.ownDate ? v.t : layerDate(L)));
        if (swipe_ && swipeView_.cube == info->id && !swipeView_.ownMode)
            want.push_back(compositeDate(L, swipeView_.ownDate ? swipeView_.t : layerDate(L)));
        auto start = [&](int t, bool detail, double x0, double y0, double x1, double y1, int w, int h) {
            BandComposite::Read r;
            r.t = t;
            r.detail = detail;
            r.key = C.key;
            r.x0 = x0, r.y0 = y0, r.x1 = x1, r.y1 = y1;
            r.w = w;
            r.h = h;
            auto reader = C.reader;
            const std::array<int, 3> bands = C.bands;
            const int ix = int(x0), iy = int(y0), iw = int(x1 - x0), ih = int(y1 - y0);
            r.f = std::async(std::launch::async, [reader, bands, t, ix, iy, iw, ih, w, h] {
                const size_t n = size_t(w) * h;
                std::vector<float> v(3 * n);
                for (int c = 0; c < 3; ++c)
                    if (!reader->readBandWindow(t, bands[c], ix, iy, iw, ih, &v[c * n], w, h)) return std::vector<float>();
                glfwPostEmptyEvent();
                return v;
            });
            C.reading = std::move(r);
        };
        bool started = false;
        for (int t : want) {
            const auto it = C.imgs.find(t);
            if (it == C.imgs.end() || it->second.key != C.key) {
                const Overview& ov = L.session->overview;
                start(t, false, 0, 0, info->width, info->height, ov.w, ov.h);
                started = true;
                break;
            }
            it->second.used = now;
        }
        // The active layer zoomed past the overview: the visible window at the screen's
        // resolution, once the view has been still for a moment.
        if (!started && &L == activeLayer()) {
            const double factor = double(info->width) / std::max(1, L.session->overview.w);
            const double ps = scale_ * mapPixelScale_;
            int win[4];
            if (ps * factor > 1.5 && visibleWindow(win)) {
                const int ww = win[2] - win[0], wh = win[3] - win[1];
                const double k = std::min({1.0, ps, double(kMaxDetailSide) / std::max(ww, wh)});
                const int bw = std::max(1, int(std::ceil(ww * k))), bh = std::max(1, int(std::ceil(wh * k)));
                uint64_t key = mixKey(mixKey(mixKey(mixKey(C.key, uint64_t(want[0])), uint64_t(win[0])), uint64_t(win[1])),
                                      uint64_t(win[2]) * 65536 + uint64_t(win[3]) + uint64_t(bw) * 7919);
                const bool have = C.detail.tex && C.detailT == want[0] && C.detail.key == C.key &&
                                  C.detail.x0 == win[0] && C.detail.y0 == win[1] && C.detail.x1 == win[2] &&
                                  C.detail.y1 == win[3] && C.detail.w == bw;
                if (key != C.wantDetail) {
                    C.wantDetail = key;
                    C.wantSince = now;
                } else if (!have && now - C.wantSince > 0.15) {
                    start(want[0], true, win[0], win[1], win[2], win[3], bw, bh);
                }
            }
        }
        // Dates not drawn lately go first when the cache is full.
        size_t bytes = 0;
        for (const auto& [t, I] : C.imgs) bytes += size_t(I.w) * I.h * 4;
        while (bytes > kCacheBytes && C.imgs.size() > want.size()) {
            auto oldest = C.imgs.end();
            for (auto it = C.imgs.begin(); it != C.imgs.end(); ++it)
                if (std::find(want.begin(), want.end(), it->first) == want.end() &&
                    (oldest == C.imgs.end() || it->second.used < oldest->second.used))
                    oldest = it;
            if (oldest == C.imgs.end()) break;
            bytes -= size_t(oldest->second.w) * oldest->second.h * 4;
            if (oldest->second.tex) Gpu::deleteTexture(oldest->second.tex);
            C.imgs.erase(oldest);
        }
    }
}

void App::drawComposite(const SeriesLayer& L, int t, float alpha, bool detail,
                        const std::function<void(const double*, float*)>& toTarget) {
    BandComposite& C = *const_cast<SeriesLayer&>(L).comp;
    const int d = compositeDate(L, t);
    auto draw = [&](BandComposite::Img& I, bool nearest) {
        double q[4];
        WarpParams warp;
        if (!I.tex || !layerQuad(L, I.x0, I.y0, I.x1, I.y1, q, warp)) return;
        float r[4];
        toTarget(q, r);
        gpu_.drawImage(I.tex, r, alpha, &warp, nearest);
        I.used = ImGui::GetTime();
    };
    if (auto it = C.imgs.find(d); it != C.imgs.end()) draw(it->second, true); // pixels as they are, like the cube
    if (detail && C.detailT == d && C.detail.key == C.key) draw(C.detail, true);
}

// ---------------------------------------------------------------------------
// Display panel
// ---------------------------------------------------------------------------

void App::uiComposite() {
    if (active_ < 0 || active_ >= int(layers_.size())) return;
    SeriesLayer& L = layers_[active_];
    const CubeInfo& info = *L.session->info;
    if (info.bandsPerDate < 3 || info.spectral) return;
    if (!L.comp) L.comp = std::make_unique<BandComposite>();
    BandComposite& C = *L.comp;
    ImGui::SeparatorText("RGB composite");
    const std::vector<CompositePreset> presets = compositePresets(info);
    auto restart = [&] { // other bands: a new stretch, every image read again
        C.stretched = false;
        C.key = mixKey(C.key, 11);
        clearCompositeImages(C);
    };
    if (ImGui::Checkbox("Three bands of each date as RGB", &C.on)) {
        if (C.on && std::all_of(C.bands.begin(), C.bands.end(), [](int b) { return b == 0; })) {
            C.bands = {std::min(3, info.bandsPerDate), 2, 1};
            for (const CompositePreset& p : presets)
                if (std::all_of(p.bands.begin(), p.bands.end(), [](int b) { return b > 0; })) {
                    C.bands = p.bands;
                    break;
                }
            restart();
        }
        mapDirty_ = viewsStale_ = true;
    }
    ImGui::SetItemTooltip("Draws the map as a colour composite of three bands of each date (true colour,\n"
                          "colour infrared...). The chart, the cursor, the statistics and the tools keep\n"
                          "the series' quantity (%s).", info.selectionText().c_str());
    if (!C.on) return;
    ImGui::SetNextItemWidth(-1);
    if (ImGui::BeginCombo("##preset", "Preset...")) {
        for (const CompositePreset& p : presets) {
            const bool ok = std::all_of(p.bands.begin(), p.bands.end(), [](int b) { return b > 0; });
            ImGui::BeginDisabled(!ok);
            char label[160];
            std::snprintf(label, sizeof(label), "%s  %d-%d-%d", p.name, p.bands[0], p.bands[1], p.bands[2]);
            if (ImGui::Selectable(ok ? label : p.name, false) && ok && p.bands != C.bands) {
                C.bands = p.bands;
                restart();
            }
            ImGui::EndDisabled();
        }
        ImGui::EndCombo();
    }
    ImGui::SetItemTooltip("Bands found by their names (or wavelengths); a band's role can be set in a\n"
                          "multiband tool's window (Tools > CCDC...) if a preset is greyed out.");
    const char* chan[3] = {"R", "G", "B"};
    for (int c = 0; c < 3; ++c) {
        ImGui::PushID(c);
        auto name = [&](int b) {
            return std::to_string(b) + ": " + (b >= 1 && b <= int(info.bandNames.size()) ? info.bandNames[b - 1] : "?");
        };
        ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x - ImGui::GetFontSize() * 1.5f);
        if (ImGui::BeginCombo(chan[c], C.bands[c] > 0 ? name(C.bands[c]).c_str() : "?")) {
            for (int b = 1; b <= info.bandsPerDate; ++b)
                if (ImGui::Selectable(name(b).c_str(), b == C.bands[c]) && b != C.bands[c]) {
                    C.bands[c] = b;
                    restart();
                }
            ImGui::EndCombo();
        }
        ImGui::PopID();
    }
    // The date: the map's (it follows the time bar and plays), or one of its own.
    if (ImGui::RadioButton("Map's date", !C.fixedDate)) {
        C.fixedDate = false;
        mapDirty_ = viewsStale_ = true;
    }
    ImGui::SameLine();
    if (ImGui::RadioButton("Fixed date", C.fixedDate)) {
        C.fixedDate = true;
        C.date = t_;
        mapDirty_ = viewsStale_ = true;
    }
    if (C.fixedDate) {
        ImGui::SetNextItemWidth(-1);
        const std::string fmt = info.layers[std::clamp(C.date, 0, info.T() - 1)].label;
        if (ImGui::SliderInt("##cdate", &C.date, 0, info.T() - 1, fmt.c_str())) mapDirty_ = viewsStale_ = true;
        ImGui::SetItemTooltip("The composite stays at this date while the time bar moves the series\n"
                              "(chart, cursor) through the others");
    }
    // Stretch of each channel; kept for every date, so they compare.
    bool changed = false;
    for (int c = 0; c < 3; ++c) {
        ImGui::PushID(10 + c);
        ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x - ImGui::GetFontSize() * 1.5f);
        const float speed = std::max(1e-6f, (C.hi[c] - C.lo[c]) * 0.005f);
        changed |= ImGui::DragFloatRange2(chan[c], &C.lo[c], &C.hi[c], speed, 0, 0, "%.4g", "%.4g");
        if (!(C.hi[c] > C.lo[c])) C.hi[c] = C.lo[c] + 1e-6f;
        ImGui::PopID();
    }
    const BandComposite::Values& V = C.last;
    ImGui::BeginDisabled(V.v.empty());
    if (ImGui::SmallButton("Stretch to this image (2-98%)")) {
        const size_t n = size_t(V.w) * V.h;
        for (int c = 0; c < 3; ++c) samplePercentiles(&V.v[c * n], n, 400000, 0.02, 0.98, C.lo[c], C.hi[c]);
        changed = true;
    }
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("The stretch of the image last read (the visible area when zoomed in)");
    if (changed) {
        // The image last read again at once, the others as they are needed.
        C.key = mixKey(mixKey(mixKey(C.key, floatBits(C.lo[0]) ^ floatBits(C.hi[2])), floatBits(C.lo[1])),
                       floatBits(C.hi[0]) ^ floatBits(C.lo[2]) ^ floatBits(C.hi[1]));
        if (!V.v.empty()) {
            BandComposite::Img& slot = V.detail ? C.detail : C.imgs[V.t];
            const std::vector<unsigned char> rgba = toRgba(V.v, V.w, V.h, C.lo, C.hi);
            if (slot.tex) Gpu::deleteTexture(slot.tex);
            slot.tex = Gpu::createImageTexture(V.w, V.h, rgba.data());
            slot.key = C.key;
        }
        mapDirty_ = viewsStale_ = true;
    }
    if (!C.error.empty()) ImGui::TextColored(theme::warning(), "%s", C.error.c_str());
    ImGui::TextDisabled("Chart and cursor: %s", info.selectionText().c_str());
}

// ---------------------------------------------------------------------------
// --selftest-ui stages 140-145: C (five bands per date, Fmask) opened with an
// overview of half its resolution; true colour from the band names; the map's
// pixels against the bands in the files (the overview's image, then the detail
// read when zoomed in); another date, a fixed date; the series untouched.
// ---------------------------------------------------------------------------

int App::selfTestComposite(const std::vector<std::string>& in) {
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
    // The map's colour at a pixel of the layer against the stretch of the file's bands there.
    auto check = [&](int x, int y, int t, int tolerance, const char* what) -> bool {
        const BandComposite& C = *activeLayer()->comp;
        const ImVec2 saved = offset_; // the pixel at the centre of the 400 x 300 render
        offset_ = ImVec2(float(200 - (x + 0.5) * scale_), float(150 - (y + 0.5) * scale_));
        renderMap(400, 300);
        offset_ = saved;
        unsigned char px[4];
        gpu_.readMapPixel(200, 150, px);
        CubeReader reader(s_->info);
        int e[3];
        for (int c = 0; c < 3; ++c) {
            float v = NAN;
            reader.readBandPixel(t, C.bands[c], x, y, v);
            e[c] = int(std::lround(std::clamp((v - C.lo[c]) / (C.hi[c] - C.lo[c]), 0.0f, 1.0f) * 255));
        }
        const int d = std::max({std::abs(px[0] - e[0]), std::abs(px[1] - e[1]), std::abs(px[2] - e[2])});
        std::printf("    %s: (%d, %d) at %s: map rgb(%d, %d, %d), bands stretched rgb(%d, %d, %d)\n", what, x, y,
                    s_->info->layers[t].label.c_str(), px[0], px[1], px[2], e[0], e[1], e[2]);
        return d <= tolerance;
    };
    switch (st_.stage) {
    case 140:
        if (in.size() < 3) {
            std::printf("OK (%.1f s)\n", now() - st_.t0);
            return 0;
        }
        st_.budget = settings_.overviewBudgetBytes;
        settings_.overviewBudgetBytes = 10 * 100 * 60 * 4 + 4096; // C's overview at 1:2: the detail is read zoomed in
        openInputs({in[2]});
        next("RGB composite: open C (five bands per date) with an overview at 1:2");
        break;
    case 141: {
        if (opening_.valid() || layers_.size() != 1 || !s_->overview.complete() || !s_->gpu.statsValid) break;
        settings_.overviewBudgetBytes = st_.budget;
        const CubeInfo& info = *s_->info;
        if (s_->overview.w >= info.width) return fail("C's overview should be coarser than C");
        const std::vector<CompositePreset> p = compositePresets(info);
        if (p.empty() || p[0].bands != std::array<int, 3>{3, 2, 1} || p[1].bands != std::array<int, 3>{4, 3, 2})
            return fail("true colour should be bands 3-2-1 and colour infrared 4-3-2 (from the band names)");
        SeriesLayer& L = layers_[active_];
        L.comp = std::make_unique<BandComposite>();
        L.comp->on = true;
        L.comp->bands = p[0].bands;
        st_.color[0] = (unsigned char)info.sel.band;
        st_.color[1] = (unsigned char)mode_;
        canvasSize_ = ImVec2(400, 300);
        fitView(canvasSize_);
        viewTouched_ = true;
        setT(1);
        next("true colour (3-2-1) and colour infrared (4-3-2) found by the band names; composite on");
        break;
    }
    case 142: {
        SeriesLayer& L = layers_[active_];
        if (!compositeReady(L, 1)) break;
        const BandComposite& C = *L.comp;
        std::printf("    overview image %d x %d of %d x %d; stretch R %.4g..%.4g G %.4g..%.4g B %.4g..%.4g\n",
                    C.imgs.at(1).w, C.imgs.at(1).h, s_->info->width, s_->info->height, C.lo[0], C.hi[0], C.lo[1],
                    C.hi[1], C.lo[2], C.hi[2]);
        // Each overview pixel is a pixel of the file: the map there shows its three bands.
        const int ow = C.last.w, oh = C.last.h;
        const size_t n = size_t(ow) * oh;
        CubeReader reader(s_->info);
        int same = 0;
        for (int j = 5; j < oh; j += 17)
            for (int i = 3; i < ow; i += 23) {
                float v = NAN;
                const int x = int((i + 0.5) * s_->info->width / ow), y = int((j + 0.5) * s_->info->height / oh);
                reader.readBandPixel(1, C.bands[0], x, y, v);
                same += v == C.last.v[size_t(j) * ow + i];
                (void)n;
            }
        if (same < 10) return fail("the composite's overview should hold the file's band values");
        const int x = int((10 + 0.5) * s_->info->width / ow), y = int((7 + 0.5) * s_->info->height / oh);
        if (!check(x, y, 1, 2, "overview")) return fail("the map should show the three bands, stretched");
        if (s_->info->sel.band != st_.color[0] || mode_ != st_.color[1])
            return fail("the series (chart, cursor) should keep its band and mode");
        // Zoomed in past the overview: the visible window is read again at full resolution.
        scale_ = 8;
        offset_ = ImVec2(float(200 - 60.5 * scale_), float(150 - 40.5 * scale_));
        next("the overview's image: the file's bands, stretched; the series untouched");
        break;
    }
    case 143: {
        const BandComposite& C = *layers_[active_].comp;
        int win[4];
        visibleWindow(win);
        if (!(C.detail.tex && C.detailT == 1 && C.detail.key == C.key && C.detail.x0 == win[0] &&
              C.detail.w == win[2] - win[0]))
            break;
        std::printf("    detail: window %d..%d x %d..%d read at %d x %d\n", win[0], win[2], win[1], win[3], C.detail.w,
                    C.detail.h);
        if (!check(61, 41, 1, 1, "zoomed in") || !check(59, 38, 1, 1, "zoomed in"))
            return fail("zoomed in, every pixel should show its own bands");
        if (compositeStatusAt(61.5, 41.5).find("overview") != std::string::npos)
            return fail("the cursor's band values should come from the detail");
        fitView(canvasSize_);
        setT(4);
        next("zoomed in: the visible window at full resolution, pixel by pixel");
        break;
    }
    case 144: {
        SeriesLayer& L = layers_[active_];
        if (!compositeReady(L, 4)) break;
        int x = 30, y = 20;
        if (!check(x, y, 4, 2, "another date")) return fail("another date should show its own bands");
        // A fixed date: the time bar moves, the composite stays.
        L.comp->fixedDate = true;
        L.comp->date = 1;
        setT(7);
        if (!compositeReady(L, 7) || compositeTitle(L, t_).find(s_->info->layers[1].label) != 0)
            return fail("a fixed date should be drawn (from the cache) whatever the map's date");
        if (!check(x, y, 1, 2, "fixed date")) return fail("the fixed date should be drawn");
        next("another date; a fixed date while the time bar moves");
        break;
    }
    case 145:
        layers_[active_].comp->on = false;
        mapDirty_ = true;
        next("composite off");
        std::printf("OK (%.1f s)\n", now() - st_.t0);
        return 0;
    }
    return -1;
}
