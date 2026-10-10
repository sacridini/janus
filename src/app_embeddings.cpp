// Foundation-model embeddings in the interface (embedding.hpp): a layer of
// embeddings is drawn through its principal components (RGB), the similarity to
// a reference or the change between years, computed on the CPU in the
// background and drawn as an image; the Embeddings panel; and the download of
// embeddings for the visible area through Zeit (a job, progress on the map).
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <ctime>
#include <filesystem>
#include <set>

#include <imgui_internal.h>
#include <implot.h>
#include <gdal_priv.h>
#include <ogr_spatialref.h>

#include "embedding.hpp"
#include "glfw.hpp"
#include "job_pool.hpp"

namespace fs = std::filesystem;

// Per layer of embeddings. Members used by the engine's tasks (store, results
// under m) are declared before the engine: it is destroyed first and joins its
// thread while they still exist.
struct App::EmbeddingLayer {
    enum View { Band = 0, Pca, Similarity, Change, ViewCount };
    EmbeddingMeta meta;
    std::shared_ptr<EmbeddingStore> store;
    int view = Pca;
    // Principal components
    int scope = 0;                              // 0 every pixel of every year, 1 the ROI (local PCA)
    std::array<int, 3> comps{0, 1, 2};          // drawn as R, G, B
    float stretch = 2.0f;                       // percent clipped at each end
    std::shared_ptr<const PcaBasis> basis;      // in use
    uint64_t basisHave = 0, basisSent = 0;
    bool basisLocal = false;                    // the basis in use came from the ROI
    // Similarity
    int refKind = 0;                            // 0 cursor, 1 pin, 2 ROI mean
    int refPin = 0;
    bool refFixedYear = false;
    int refYear = 0;
    std::vector<float> ref;                     // dequantised reference vector
    uint64_t refKey = 0;
    std::string refText;                        // what the reference is, for the title
    // Change
    int changeBase = -1;                        // -1 the previous year, else a fixed year
    // Ranges of the float views (similarity, change)
    struct Range {
        float lo = 0, hi = 1;
        bool manual = false;
        uint64_t key = 0;                       // what the automatic range was taken from
    } range[ViewCount];
    int cmap[ViewCount] = {0, 0, ImPlotColormap_Viridis, ImPlotColormap_Plasma};
    // One image per year: the last one made (its key says for what)
    struct Img {
        GpuTex tex = 0;
        uint64_t key = 0;
        bool rgba = false;
        int w = 0, h = 0;
        std::vector<float> values;              // float views: kept for the value under the cursor
        float lo = 0, hi = 1;                   // its automatic range
        double ms = 0;
    };
    std::vector<Img> img;
    std::map<std::string, uint64_t> slotSent;   // engine slot -> key of the last request
    int yearsSeen = -1;
    int threads = 0;
    // Results posted by the engine's thread
    std::mutex m;
    std::shared_ptr<const PcaBasis> newBasis;
    uint64_t newBasisKey = 0;
    bool newBasisLocal = false;
    struct Done {
        int t = 0;
        int view = 0;
        uint64_t key = 0;
        std::vector<uint8_t> rgba;
        std::vector<float> values;
        float lo = 0, hi = 1;
        double ms = 0;
    };
    std::vector<Done> done;
    std::unique_ptr<EmbeddingEngine> engine;    // last: destroyed first

    ~EmbeddingLayer() {
        engine.reset();
        for (Img& i : img)
            if (i.tex) Gpu::deleteTexture(i.tex);
    }
};

namespace {

uint64_t mix(uint64_t h, uint64_t v) { return (h ^ v) * 0x100000001b3ull + (h >> 29) + 0x9e3779b97f4a7c15ull; }
uint64_t mixF(uint64_t h, float f) {
    uint32_t u;
    std::memcpy(&u, &f, 4);
    return mix(h, u);
}

const char* kViewNames[] = {"Band values (Display panel)", "Principal components (RGB)",
                            "Similarity to a reference", "Change between years"};

std::string stamp() {
    const std::time_t t = std::time(nullptr);
    std::tm tm{};
#ifdef _WIN32
    localtime_s(&tm, &t);
#else
    localtime_r(&t, &tm);
#endif
    char b[32];
    std::strftime(b, sizeof(b), "%Y%m%d_%H%M%S", &tm);
    return b;
}

struct SourceInfo {
    const char* id;
    const char* label;
    int dims;
    const char* help;
};
const SourceInfo kSources[] = {
    {"alphaearth", "AlphaEarth Foundations (64 dims)", 64,
     "Google DeepMind's Satellite Embedding: 64 dimensions per 10 m pixel and year, from\n"
     "optical, radar, lidar and climate data; read from its free copy on Source Cooperative.\n"
     "The first download fetches its file index (78 MB, refreshed monthly)."},
    {"tessera", "TESSERA (128 dims)", 128,
     "TESSERA (University of Cambridge): 128 dimensions per 10 m pixel and year, from\n"
     "Sentinel-1 and Sentinel-2 time series; read from its public Zarr store."},
};
const int kResolutions[] = {10, 20, 30, 60, 100};

} // namespace

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

void App::initEmbeddingLayer(SeriesLayer& L) {
    const EmbeddingMeta meta = embeddingMeta(*L.session->info);
    if (!meta.is) {
        L.emb.reset();
        return;
    }
    L.emb = std::make_shared<EmbeddingLayer>();
    L.emb->meta = meta;
    L.emb->refYear = L.session->info->T() - 1;
    showEmbeddings_ = true; // the panel comes up with the first layer of embeddings
}

int App::layerDate(const SeriesLayer& L) const {
    const int T = L.session->info->T();
    return std::clamp(&L == activeLayer() ? t_ : L.disp.t, 0, T - 1);
}

App::SeriesLayer* App::embeddingLayer() {
    if (active_ >= 0 && active_ < int(layers_.size()) && layers_[active_].emb) return &layers_[active_];
    for (int i = int(layers_.size()) - 1; i >= 0; --i)
        if (layers_[i].emb && layers_[i].visible) return &layers_[i];
    for (int i = int(layers_.size()) - 1; i >= 0; --i)
        if (layers_[i].emb) return &layers_[i];
    return nullptr;
}

bool App::embeddingView(const SeriesLayer& L) const { return L.emb && L.emb->view != EmbeddingLayer::Band; }

// The store pixel under the active layer's pixel (ax, ay); false off the layer.
bool App::embeddingPixel(const SeriesLayer& L, double ax, double ay, size_t& p) const {
    if (!L.emb || !L.emb->store || L.emb->store->w == 0) return false;
    double lx = ax, ly = ay;
    if (&L != activeLayer() && !activeToLayer(L, ax, ay, lx, ly)) return false;
    return L.emb->store->pixelOf(lx, ly, p);
}

// The map's ROI on L, in store pixels (x0, y0, x1, y1).
bool App::embeddingRoi(const SeriesLayer& L, int rect[4]) const {
    if (!roiRect_ || !L.emb || !L.emb->store || L.emb->store->w == 0) return false;
    const EmbeddingStore& S = *L.emb->store;
    double x0 = 1e300, y0 = 1e300, x1 = -1e300, y1 = -1e300;
    const double cx[2] = {double(roiRectXY_[0]), double(roiRectXY_[2])};
    const double cy[2] = {double(roiRectXY_[1]), double(roiRectXY_[3])};
    for (double ax : cx)
        for (double ay : cy) {
            double lx = ax, ly = ay;
            if (&L != activeLayer() && !activeToLayer(L, ax, ay, lx, ly)) return false;
            x0 = std::min(x0, lx);
            y0 = std::min(y0, ly);
            x1 = std::max(x1, lx);
            y1 = std::max(y1, ly);
        }
    rect[0] = std::clamp(int(std::floor(x0 / S.fx)), 0, S.w);
    rect[1] = std::clamp(int(std::floor(y0 / S.fy)), 0, S.h);
    rect[2] = std::clamp(int(std::ceil(x1 / S.fx)), 0, S.w);
    rect[3] = std::clamp(int(std::ceil(y1 / S.fy)), 0, S.h);
    return rect[2] > rect[0] && rect[3] > rect[1];
}

// The map's visible area on L: the bounding box of points along the canvas'
// edges (curved when L is in another CRS).
bool App::embeddingVisibleWindow(const SeriesLayer& L, int win[4]) const {
    if (&L == activeLayer()) return visibleWindow(win);
    if (!s_ || scale_ <= 0) return false;
    const CubeInfo& info = *L.session->info;
    double x0 = 1e300, y0 = 1e300, x1 = -1e300, y1 = -1e300;
    const int n = 16;
    for (int i = 0; i <= n; ++i)
        for (int edge = 0; edge < 4; ++edge) {
            const double f = double(i) / n;
            const double cx = edge < 2 ? f * canvasSize_.x : edge == 2 ? 0.0 : canvasSize_.x;
            const double cy = edge < 2 ? (edge == 0 ? 0.0 : canvasSize_.y) : f * canvasSize_.y;
            double lx, ly;
            if (!activeToLayer(L, (cx - offset_.x) / scale_, (cy - offset_.y) / scale_, lx, ly)) continue;
            x0 = std::min(x0, lx);
            y0 = std::min(y0, ly);
            x1 = std::max(x1, lx);
            y1 = std::max(y1, ly);
        }
    if (x1 < x0) return false;
    win[0] = int(std::clamp(std::floor(x0), 0.0, double(info.width)));
    win[1] = int(std::clamp(std::floor(y0), 0.0, double(info.height)));
    win[2] = int(std::clamp(std::ceil(x1), 0.0, double(info.width)));
    win[3] = int(std::clamp(std::ceil(y1), 0.0, double(info.height)));
    return win[2] > win[0] && win[3] > win[1];
}

bool App::embeddingExportSpec(bool wholeLayer, EmbeddingExport& e, std::string& why) {
    SeriesLayer* L = embeddingLayer();
    if (!L || !embeddingView(*L)) {
        why = "No layer is drawn as embeddings (Embeddings panel).";
        return false;
    }
    const EmbeddingLayer& E = *L->emb;
    const CubeInfo& info = *L->session->info;
    const int t = layerDate(*L);
    e = EmbeddingExport{};
    e.info = L->session->info;
    e.t = t;
    if (wholeLayer) {
        e.win[2] = info.width;
        e.win[3] = info.height;
    } else if (!embeddingVisibleWindow(*L, e.win)) {
        why = "The layer of embeddings is not in view.";
        return false;
    }
    const std::string& year = info.layers[t].label;
    char size[64];
    std::snprintf(size, sizeof(size), "%d x %d px", e.win[2] - e.win[0], e.win[3] - e.win[1]);
    e.metadata = {{"JANUS_EMBEDDING_MODEL", E.meta.label()}, {"JANUS_DATE", year}};
    if (!E.meta.attribution.empty()) e.metadata.emplace_back("JANUS_EMBEDDING_ATTRIBUTION", E.meta.attribution);
    if (!E.meta.license.empty()) e.metadata.emplace_back("JANUS_EMBEDDING_LICENSE", E.meta.license);
    if (E.view == EmbeddingLayer::Pca) {
        if (!E.basis) {
            why = "The principal components are still being fitted.";
            return false;
        }
        e.kind = EmbeddingExport::Pca;
        e.basis = E.basis;
        const PcaBasis& B = *E.basis;
        for (int c = 0; c < B.k; ++c) {
            char name[64];
            std::snprintf(name, sizeof(name), "PC%d (%.1f%% of the variance)", c + 1,
                          B.total > 0 ? 100 * B.eig[c] / B.total : 0.0);
            e.bandNames.push_back(name);
        }
        const std::string what = std::string("principal components") + (E.basisLocal ? " of the ROI" : "");
        e.metadata.emplace_back("JANUS_EMBEDDING_VIEW", what);
        e.metadata.emplace_back("JANUS_PCA_SAMPLES", std::to_string(B.samples) + " vectors of " +
                                                         std::to_string(B.years) + " years");
        e.title = L->name + ": " + what + " " + year + ", " + size + " (" + std::to_string(B.k) + " bands, Float32)";
    } else if (E.view == EmbeddingLayer::Similarity) {
        if (E.ref.empty()) {
            why = "No reference vector yet: hover over the map, drop the reference pin or draw an ROI.";
            return false;
        }
        e.kind = EmbeddingExport::Similarity;
        e.ref = E.ref;
        e.bandNames = {"cosine similarity to " + E.refText};
        e.metadata.emplace_back("JANUS_EMBEDDING_VIEW", e.bandNames[0]);
        e.title = L->name + ": similarity " + year + ", " + size + " (Float32)";
    } else {
        const int t0 = E.changeBase < 0 ? t - 1 : E.changeBase;
        if (t0 < 0 || t0 >= info.T() || t0 == t) {
            why = "No year to compare this one with.";
            return false;
        }
        e.kind = EmbeddingExport::Change;
        e.t0 = t0;
        e.bandNames = {"cosine distance " + year + " - " + info.layers[t0].label};
        e.metadata.emplace_back("JANUS_EMBEDDING_VIEW", e.bandNames[0]);
        e.title = L->name + ": change " + year + " - " + info.layers[t0].label + ", " + size + " (Float32)";
    }
    return true;
}

// ---------------------------------------------------------------------------
// Per frame
// ---------------------------------------------------------------------------

void App::updateEmbeddingRef(SeriesLayer& L, int t) {
    EmbeddingLayer& E = *L.emb;
    const EmbeddingStore& S = *E.store;
    const int year = E.refFixedYear ? std::clamp(E.refYear, 0, S.T - 1) : t;
    if (!S.yearReady(year)) return;
    char what[96];
    auto setVector = [&](size_t p, const char* text) {
        if (!S.valid(year, p)) return;
        const uint64_t key = mix(mix(mix(1, p), uint64_t(year)), uint64_t(E.refKind));
        if (key == E.refKey) return;
        E.ref.resize(S.D);
        S.dequant(year, p, E.ref.data());
        E.refKey = key;
        E.refText = text;
    };
    if (E.refKind == 0 && hover_.x >= 0) {
        size_t p;
        std::snprintf(what, sizeof(what), "the cursor (%s)", L.session->info->layers[year].label.c_str());
        if (embeddingPixel(L, hover_.x + 0.5, hover_.y + 0.5, p)) setVector(p, what);
    } else if (E.refKind == 1) {
        for (const SeriesView& pin : pins_)
            if (pin.id == E.refPin) {
                size_t p;
                std::snprintf(what, sizeof(what), "pin %d (%s)", pin.id, L.session->info->layers[year].label.c_str());
                if (embeddingPixel(L, pin.x + 0.5, pin.y + 0.5, p)) setVector(p, what);
            }
    } else if (E.refKind == 2) {
        int r[4];
        if (!embeddingRoi(L, r)) return;
        const uint64_t key = mix(mix(mix(mix(mix(2, r[0]), r[1]), r[2]), r[3]), uint64_t(year));
        if (key == E.refKey) return;
        std::vector<double> sum(S.D, 0.0);
        std::vector<float> v(S.D);
        size_t n = 0;
        const int step = std::max(1, int(std::sqrt(double(r[2] - r[0]) * (r[3] - r[1]) / 200000.0)));
        for (int y = r[1]; y < r[3]; y += step)
            for (int x = r[0]; x < r[2]; x += step) {
                const size_t p = size_t(y) * S.w + x;
                if (!S.valid(year, p)) continue;
                S.dequant(year, p, v.data());
                for (int d = 0; d < S.D; ++d) sum[d] += v[d];
                ++n;
            }
        if (n == 0) return;
        E.ref.resize(S.D);
        for (int d = 0; d < S.D; ++d) E.ref[d] = float(sum[d] / n);
        E.refKey = key;
        std::snprintf(what, sizeof(what), "the ROI mean (%s)", L.session->info->layers[year].label.c_str());
        E.refText = what;
    }
}

// What the image of year t would be made from now (0: it cannot be made yet).
uint64_t App::embeddingKey(const SeriesLayer& L, int t) const {
    const EmbeddingLayer& E = *L.emb;
    const EmbeddingStore& S = *E.store;
    if (!S.yearReady(t)) return 0;
    switch (E.view) {
    case EmbeddingLayer::Pca: return E.basis ? mix(mix(11, E.basisHave), uint64_t(t)) : 0;
    case EmbeddingLayer::Similarity: return E.ref.empty() ? 0 : mix(mix(12, E.refKey), uint64_t(t));
    case EmbeddingLayer::Change: {
        const int t0 = E.changeBase < 0 ? t - 1 : E.changeBase;
        if (t0 < 0 || t0 >= S.T || t0 == t || !S.yearReady(t0)) return 0;
        return mix(mix(13, uint64_t(t0)), uint64_t(t));
    }
    default: return 0;
    }
}

void App::pumpEmbeddings() {
    pumpEmbeddingDownloads();
    for (SeriesLayer& L : layers_) {
        if (!L.emb) continue;
        EmbeddingLayer& E = *L.emb;
        const CubeInfo& info = *L.session->info;
        const int T = info.T();
        const int threads = settings_.processingThreads();
        if (!E.store) {
            E.store = std::make_shared<EmbeddingStore>();
            E.store->start(L.session->info, int64_t(overviewBudgetMB()) << 20, threads, [] { glfwPostEmptyEvent(); });
            E.engine = std::make_unique<EmbeddingEngine>(threads, [] { glfwPostEmptyEvent(); });
            E.threads = threads;
            E.img.assign(T, {});
        }
        if (E.threads != threads) {
            E.engine->setThreads(threads);
            E.threads = threads;
        }
        EmbeddingStore& S = *E.store;

        // Results of the background work: a new basis, images to upload.
        std::vector<EmbeddingLayer::Done> done;
        {
            std::lock_guard<std::mutex> lk(E.m);
            if (E.newBasis) {
                E.basis = std::move(E.newBasis);
                E.basisHave = E.newBasisKey;
                E.basisLocal = E.newBasisLocal;
            }
            done.swap(E.done);
        }
        for (EmbeddingLayer::Done& d : done) {
            if (d.t < 0 || d.t >= T) continue;
            EmbeddingLayer::Img& I = E.img[d.t];
            if (I.tex) Gpu::deleteTexture(I.tex);
            I.w = S.w;
            I.h = S.h;
            I.key = d.key;
            I.ms = d.ms;
            I.rgba = !d.rgba.empty();
            I.tex = I.rgba ? Gpu::createImageTexture(S.w, S.h, d.rgba.data()) : Gpu::createTileTexture(S.w, S.h, d.values.data());
            I.values = std::move(d.values);
            I.lo = d.lo;
            I.hi = d.hi;
            // The automatic range of a float view comes from the first image of
            // each reference (or base), so the years stay comparable.
            EmbeddingLayer::Range& R = E.range[d.view];
            const uint64_t rk = d.view == EmbeddingLayer::Similarity ? E.refKey : uint64_t(E.changeBase + 7);
            if (!I.rgba && d.view == E.view && !R.manual && R.key != rk) {
                R.lo = d.lo;
                R.hi = d.hi;
                R.key = rk;
            }
            mapDirty_ = true;
            viewsStale_ = true;
        }
        if (S.yearsDone() != E.yearsSeen) {
            E.yearsSeen = S.yearsDone();
            mapDirty_ = true;
        }
        if (E.view == EmbeddingLayer::Band || !S.rangesReady || S.yearsDone() == 0) continue;
        const int t = layerDate(L);

        // Principal components: of every year (fitted again as each year arrives), or
        // of the ROI (local PCA).
        if (E.view == EmbeddingLayer::Pca) {
            int rect[4] = {0, 0, 0, 0};
            const bool local = E.scope == 1 && embeddingRoi(L, rect);
            uint64_t want = mix(mix(mix(21, local), uint64_t(S.yearsDone())), uint64_t(E.comps[0] + 8 * E.comps[1] + 64 * E.comps[2]));
            want = mixF(want, E.stretch);
            if (local)
                for (int v : rect) want = mix(want, uint64_t(v));
            if (want != E.basisHave && want != E.basisSent) {
                E.basisSent = want;
                auto store = E.store;
                EmbeddingLayer* e = &E;
                const std::array<int, 3> comps = E.comps;
                const float stretch = E.stretch;
                E.engine->submit("basis", [e, store, rect, local, comps, stretch, want] {
                    const auto sample = sampleVectors(*store, local ? rect : nullptr, 65536, true, 0);
                    auto B = std::make_shared<PcaBasis>(fitPca(*store, sample, 6, comps, stretch, e->engine->pool()));
                    B->years = store->yearsDone();
                    std::lock_guard<std::mutex> lk(e->m);
                    e->newBasis = std::move(B);
                    e->newBasisKey = want;
                    e->newBasisLocal = local;
                });
            }
        }
        if (E.view == EmbeddingLayer::Similarity) updateEmbeddingRef(L, t);

        // The image of the date shown, and of the next one while playing.
        const int next = playing_ && &L == activeLayer() ? (t + 1) % T : -1;
        for (const auto& [slot, tt] : {std::pair<const char*, int>{"image", t}, {"image+1", next}}) {
            if (tt < 0) continue;
            const uint64_t key = embeddingKey(L, tt);
            if (!key || E.img[tt].key == key || E.slotSent[slot] == key) continue;
            E.slotSent[slot] = key;
            auto store = E.store;
            EmbeddingLayer* e = &E;
            const int view = E.view;
            std::shared_ptr<const PcaBasis> basis = E.basis;
            std::vector<float> ref = E.ref;
            const int t0 = E.changeBase < 0 ? tt - 1 : E.changeBase;
            const int y = tt;
            E.engine->submit(slot, [e, store, view, basis, ref, t0, y, key] {
                const auto c0 = std::chrono::steady_clock::now();
                EmbeddingLayer::Done d;
                d.t = y;
                d.view = view;
                d.key = key;
                JobPool& pool = e->engine->pool();
                if (view == EmbeddingLayer::Pca) {
                    renderPcaRgb(*store, y, *basis, pool, d.rgba);
                } else {
                    if (view == EmbeddingLayer::Similarity) renderSimilarity(*store, y, ref, pool, d.values);
                    else renderChange(*store, y, t0, pool, d.values);
                    percentileRange(d.values, 2.0f, view == EmbeddingLayer::Similarity ? 99.5f : 98.0f, d.lo, d.hi);
                }
                d.ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - c0).count();
                std::lock_guard<std::mutex> lk(e->m);
                e->done.push_back(std::move(d));
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Map
// ---------------------------------------------------------------------------

void App::drawEmbedding(const SeriesLayer& L, int t, float alpha,
                        const std::function<void(const double*, float*)>& toTarget) {
    const EmbeddingLayer& E = *L.emb;
    if (t < 0 || t >= int(E.img.size())) return;
    const EmbeddingLayer::Img& I = E.img[t];
    if (!I.tex) return;
    const CubeInfo& info = *L.session->info;
    double q[4];
    WarpParams warp;
    if (!layerQuad(L, 0, 0, info.width, info.height, q, warp)) return;
    float r[4];
    toTarget(q, r);
    if (I.rgba) {
        gpu_.drawImage(I.tex, r, alpha, &warp, true);
    } else {
        const int view = E.view == EmbeddingLayer::Change ? EmbeddingLayer::Change : EmbeddingLayer::Similarity;
        const EmbeddingLayer::Range& R = E.range[view];
        const bool own = R.manual || R.key != 0;
        gpu_.drawOverlay(I.tex, r, own ? R.lo : I.lo, own ? R.hi : I.hi, E.cmap[view], alpha, &warp);
    }
}

// Title, colour bar and wait message of the main map when the active layer is
// drawn as embeddings. False when it is not.
bool App::embeddingMapInfo(std::string& title, int& cmap, float& lo, float& hi, std::string& wait) const {
    const SeriesLayer* L = activeLayer();
    if (!L || !embeddingView(*L)) return false;
    const EmbeddingLayer& E = *L->emb;
    const CubeInfo& info = *L->session->info;
    const int t = t_;
    cmap = -1;
    char b[200];
    if (E.view == EmbeddingLayer::Pca) {
        std::snprintf(b, sizeof(b), "%s  |  Embeddings: PC%d, PC%d, PC%d%s", info.layers[t].label.c_str(),
                      E.comps[0] + 1, E.comps[1] + 1, E.comps[2] + 1, E.basisLocal ? " of the ROI" : "");
    } else if (E.view == EmbeddingLayer::Similarity) {
        std::snprintf(b, sizeof(b), "%s  |  Cosine similarity to %s", info.layers[t].label.c_str(),
                      E.refText.empty() ? "(no reference)" : E.refText.c_str());
        cmap = E.cmap[EmbeddingLayer::Similarity];
    } else {
        const int t0 = E.changeBase < 0 ? t - 1 : E.changeBase;
        std::snprintf(b, sizeof(b), "%s - %s  |  Change (cosine distance)", info.layers[t].label.c_str(),
                      t0 >= 0 && t0 < info.T() ? info.layers[t0].label.c_str() : "(none)");
        cmap = E.cmap[EmbeddingLayer::Change];
    }
    title = b;
    if (cmap >= 0 && t < int(E.img.size())) {
        const EmbeddingLayer::Range& R = E.range[E.view];
        const EmbeddingLayer::Img& I = E.img[t];
        const bool own = R.manual || R.key != 0;
        lo = own ? R.lo : I.lo;
        hi = own ? R.hi : I.hi;
    }
    const EmbeddingStore* S = E.store.get();
    if (!S || !S->rangesReady) wait = S && S->failed() ? "Embeddings: " + S->error() : "Reading the embeddings...";
    else if (!S->yearReady(t)) wait = "Reading this year...";
    else if (E.view == EmbeddingLayer::Similarity && E.ref.empty())
        wait = E.refKind == 0   ? "Hover over the map: the similarity to the vector under the cursor"
               : E.refKind == 1 ? "Drop the pin chosen as reference (Embeddings panel)"
                                : "Draw an ROI (Shift + drag): the similarity to its mean vector";
    else if (E.view == EmbeddingLayer::Change && (E.changeBase < 0 ? t - 1 : E.changeBase) < 0)
        wait = "The first year has no previous year";
    else if (t < int(E.img.size()) && !E.img[t].tex) wait = "Computing...";
    return true;
}

// The embedding view's value under the cursor, for the status bar ("" = none).
std::string App::embeddingStatusAt(int ix, int iy) const {
    const SeriesLayer* L = activeLayer();
    if (!L || !embeddingView(*L) || !L->emb->store) return "";
    const EmbeddingLayer& E = *L->emb;
    const EmbeddingStore& S = *E.store;
    size_t p;
    if (!embeddingPixel(*L, ix + 0.5, iy + 0.5, p) || !S.yearReady(t_)) return "";
    if (!S.valid(t_, p)) return "  |  no embedding";
    char b[160];
    if (E.view == EmbeddingLayer::Pca && E.basis) {
        std::vector<float> v(S.D);
        S.dequant(t_, p, v.data());
        float pc[3] = {0, 0, 0};
        for (int c = 0; c < 3; ++c)
            for (int d = 0; d < S.D; ++d)
                pc[c] += (v[d] - E.basis->mean[d]) * E.basis->comps[size_t(E.basis->shown[c]) * S.D + d];
        std::snprintf(b, sizeof(b), "  |  PC%d %.3g  PC%d %.3g  PC%d %.3g", E.comps[0] + 1, pc[0], E.comps[1] + 1, pc[1],
                      E.comps[2] + 1, pc[2]);
        return b;
    }
    const EmbeddingLayer::Img& I = E.img[t_];
    if (I.values.size() != S.pixels() || std::isnan(I.values[p])) return "";
    std::snprintf(b, sizeof(b), "  |  %s %.4f", E.view == EmbeddingLayer::Similarity ? "similarity" : "change",
                  I.values[p]);
    return b;
}

// Progress of the embeddings being read and downloaded, top right of the map
// (under the overview's bar): never in the way, always telling when the image comes.
void App::drawEmbeddingNotes(ImDrawList* dl, ImVec2 origin, ImVec2 size) {
    float y = 10.0f + (s_ && !s_->overview.complete() ? 24.0f : 0.0f);
    const float bw = 300.0f;
    auto bar = [&](float frac, const std::string& text, ImU32 col) {
        const ImVec2 p0 = origin + ImVec2(size.x - bw - 10, y), p1 = p0 + ImVec2(bw, 18);
        dl->AddRectFilled(p0, p1, IM_COL32(0, 0, 0, 170), 3);
        dl->AddRectFilled(p0, ImVec2(p0.x + bw * std::clamp(frac, 0.0f, 1.0f), p1.y), col, 3);
        dl->PushClipRect(p0, p1, true);
        dl->AddText(p0 + ImVec2(6, 2), IM_COL32(255, 255, 255, 255), text.c_str());
        dl->PopClipRect();
        y += 24;
    };
    for (const SeriesLayer& L : layers_) {
        if (!L.emb || !L.emb->store || !L.visible) continue;
        const EmbeddingStore& S = *L.emb->store;
        if (S.failed()) {
            bar(1.0f, "Embeddings: " + S.error(), IM_COL32(170, 50, 50, 220));
        } else if (!S.complete() && S.T > 0) {
            char b[96];
            std::snprintf(b, sizeof(b), "Reading embeddings %d/%d years", S.yearsDone(), S.T);
            bar(float(S.yearsDone()) / S.T, b, IM_COL32(70, 130, 220, 220));
        }
    }
    const double now = ImGui::GetTime();
    for (const EmbDownload& d : embDownloads_) {
        const ZeitJob::State st = d.job->state;
        std::string msg;
        {
            std::lock_guard<std::mutex> lk(d.job->m);
            msg = st == ZeitJob::State::Failed ? d.job->error : d.job->message;
        }
        const double el = std::chrono::duration<double>(std::chrono::steady_clock::now() - d.job->started).count();
        char b[256];
        if (st == ZeitJob::State::Running) {
            std::snprintf(b, sizeof(b), "%s: %s (%.0f s)", d.title.c_str(), msg.empty() ? "starting Zeit" : msg.c_str(), el);
            bar(float(d.job->progress), b, IM_COL32(60, 160, 110, 230));
        } else if (st == ZeitJob::State::Failed && now - d.endedAt < 20) {
            std::snprintf(b, sizeof(b), "%s failed: %s (Tools > Tasks > Log)", d.title.c_str(), msg.c_str());
            bar(1.0f, b, IM_COL32(170, 50, 50, 220));
        } else if (st == ZeitJob::State::Done && !d.opened) {
            std::snprintf(b, sizeof(b), "%s: done, opening...", d.title.c_str());
            bar(1.0f, b, IM_COL32(60, 160, 110, 230));
        }
    }
}

// ---------------------------------------------------------------------------
// Download (Zeit job)
// ---------------------------------------------------------------------------

// The active layer's visible area (scope 0) or ROI (scope 1) in longitude and
// latitude, and its size in metres.
bool App::embeddingBounds(int scope, double ll[4], double& wM, double& hM, std::string& why) const {
    if (!s_) {
        why = "open a series first";
        return false;
    }
    const CubeInfo& info = *s_->info;
    if (info.crsWkt.empty() || !info.hasGeoTransform) {
        why = "the active layer has no CRS: the area cannot be placed on the Earth";
        return false;
    }
    int win[4];
    if (scope == 1) {
        if (!roiRect_) {
            why = "draw an ROI first (Shift + drag on the map)";
            return false;
        }
        std::copy(roiRectXY_, roiRectXY_ + 4, win);
    } else if (!visibleWindow(win)) {
        why = "nothing of the layer is visible";
        return false;
    }
    OGRSpatialReference src, dst;
    if (src.importFromWkt(info.crsWkt.c_str()) != OGRERR_NONE) {
        why = "the active layer's CRS is not understood";
        return false;
    }
    dst.importFromEPSG(4326);
    src.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    dst.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    std::unique_ptr<OGRCoordinateTransformation> ct(OGRCreateCoordinateTransformation(&src, &dst));
    if (!ct) {
        why = "no transformation from the active layer's CRS to longitude and latitude";
        return false;
    }
    ll[0] = ll[1] = 1e300;
    ll[2] = ll[3] = -1e300;
    constexpr int kN = 16; // points along each edge: a reprojected rectangle bulges
    for (int i = 0; i <= kN; ++i)
        for (int e = 0; e < 4; ++e) {
            const double f = double(i) / kN;
            const double px = e == 0 ? win[0] : e == 1 ? win[2] : win[0] + f * (win[2] - win[0]);
            const double py = e == 2 ? win[1] : e == 3 ? win[3] : win[1] + f * (win[3] - win[1]);
            double gx, gy;
            if (!info.pixelToGeo(px, py, gx, gy) || !ct->Transform(1, &gx, &gy)) continue;
            ll[0] = std::min(ll[0], gx);
            ll[1] = std::min(ll[1], gy);
            ll[2] = std::max(ll[2], gx);
            ll[3] = std::max(ll[3], gy);
        }
    if (!(ll[2] > ll[0] && ll[3] > ll[1])) {
        why = "the area could not be transformed to longitude and latitude";
        return false;
    }
    const double lat = 0.5 * (ll[1] + ll[3]) * 3.14159265358979 / 180.0;
    wM = (ll[2] - ll[0]) * 111320.0 * std::cos(lat);
    hM = (ll[3] - ll[1]) * 110574.0;
    return true;
}

void App::startEmbeddingDownload() {
    if (!zeit_ || zeit_->state() != ZeitClient::State::Ready) return;
    const SourceInfo& src = kSources[std::clamp(embUi_.source, 0, 1)];
    double ll[4], wM, hM;
    std::string why;
    if (!embeddingBounds(embUi_.area, ll, wM, hM, why)) {
        error_ = why;
        openErrorPopup_ = true;
        return;
    }
    json years = json::array();
    for (int y = embUi_.y0; y <= embUi_.y1; ++y) years.push_back(y);
    const fs::path out = fs::u8path(resultsDir_) / "embeddings" /
                         (std::string(src.id) + "_" + std::to_string(embUi_.y0) + "-" + std::to_string(embUi_.y1) +
                          "_" + stamp());
    std::error_code ec;
    fs::create_directories(out, ec);
    if (ec) {
        error_ = "could not create " + out.u8string() + ": " + ec.message();
        openErrorPopup_ = true;
        return;
    }
    const int res = kResolutions[std::clamp(embUi_.res, 0, int(std::size(kResolutions)) - 1)];
    json spec = {{"tool", "embeddings"},
                 {"params", {{"source", src.id}, {"years", years}, {"bounds", {ll[0], ll[1], ll[2], ll[3]}},
                             {"res", res == 10 ? json(nullptr) : json(res)}}},
                 {"output_dir", out.u8string()}};
    char title[160];
    std::snprintf(title, sizeof(title), "Embeddings %s %d-%d (%.1f x %.1f km, %d m)",
                  src.id == std::string("tessera") ? "TESSERA" : "AlphaEarth", embUi_.y0, embUi_.y1, wM / 1000,
                  hM / 1000, res);
    EmbDownload d;
    d.job = zeit_->startJob(spec, (out / "job.json").u8string(), title);
    d.title = std::string(src.id) == "tessera" ? "TESSERA" : "AlphaEarth";
    d.folder = out.u8string();
    jobs_.push_back(d.job); // also listed in Tools > Tasks (with its log)
    embDownloads_.push_back(std::move(d));
}

void App::pumpEmbeddingDownloads() {
    for (EmbDownload& d : embDownloads_) {
        const ZeitJob::State st = d.job->state;
        if (st != ZeitJob::State::Running && d.endedAt < 0) d.endedAt = ImGui::GetTime();
        if (st == ZeitJob::State::Done && !d.opened && !opening_.valid()) {
            d.opened = true;
            openingKeepActive_ = !layers_.empty(); // added on top; the series being studied stays active
            openInputs({d.folder}, true);
        }
    }
    // Finished downloads go from the map's notes after a while (Tasks keeps them).
    const double now = ImGui::GetTime();
    embDownloads_.erase(std::remove_if(embDownloads_.begin(), embDownloads_.end(),
                                       [&](const EmbDownload& d) {
                                           return d.endedAt >= 0 && (d.opened || now - d.endedAt > 20) &&
                                                  d.job->state != ZeitJob::State::Running;
                                       }),
                        embDownloads_.end());
}

void App::uiEmbeddingDownload(bool inLayers) {
    ImGui::PushID(inLayers ? "embdl-layers" : "embdl-panel");
    const bool open = inLayers ? ImGui::CollapsingHeader("Embeddings") : true;
    if (inLayers) ImGui::SetItemTooltip("Foundation-model embeddings (AlphaEarth, TESSERA) of the visible area,\n"
                                        "downloaded through Zeit and added as a layer");
    if (!open) {
        ImGui::PopID();
        return;
    }
    const float w = ImGui::GetContentRegionAvail().x;
    ImGui::SetNextItemWidth(w);
    const SourceInfo& cur = kSources[std::clamp(embUi_.source, 0, 1)];
    if (ImGui::BeginCombo("##source", cur.label)) {
        for (int i = 0; i < 2; ++i) {
            if (ImGui::Selectable(kSources[i].label, i == embUi_.source)) embUi_.source = i;
            ImGui::SetItemTooltip("%s", kSources[i].help);
        }
        ImGui::EndCombo();
    }
    ImGui::SetItemTooltip("%s", cur.help);
    ImGui::SetNextItemWidth(w * 0.6f);
    if (ImGui::DragIntRange2("##years", &embUi_.y0, &embUi_.y1, 0.1f, 2017, 2025, "from %d", "to %d")) {
        embUi_.y0 = std::clamp(embUi_.y0, 2017, 2025);
        embUi_.y1 = std::clamp(embUi_.y1, embUi_.y0, 2025);
    }
    ImGui::SetItemTooltip("Years to download (one file each). Both products start in 2017;\n"
                          "a year not published yet makes the download fail.");
    ImGui::SameLine();
    ImGui::SetNextItemWidth(-1);
    char resLabel[32];
    std::snprintf(resLabel, sizeof(resLabel), "%d m", kResolutions[embUi_.res]);
    if (ImGui::BeginCombo("##res", resLabel)) {
        for (int i = 0; i < int(std::size(kResolutions)); ++i) {
            char b[48];
            std::snprintf(b, sizeof(b), i == 0 ? "%d m (native)" : "%d m (mean of the cells)", kResolutions[i]);
            if (ImGui::Selectable(b, i == embUi_.res)) embUi_.res = i;
        }
        ImGui::EndCombo();
    }
    ImGui::SetItemTooltip("Cell size. Coarser cells are the mean of the 10 m embeddings under them\n"
                          "(how embeddings are taken to a larger scale): fewer bytes for large areas.");
    ImGui::RadioButton("Visible area", &embUi_.area, 0);
    ImGui::SameLine();
    ImGui::RadioButton("ROI", &embUi_.area, 1);
    double ll[4], wM = 0, hM = 0;
    std::string why;
    const bool ok = embeddingBounds(embUi_.area, ll, wM, hM, why);
    const int res = kResolutions[embUi_.res];
    const int years = embUi_.y1 - embUi_.y0 + 1;
    const double px = ok ? std::ceil(wM / res) * std::ceil(hM / res) : 0;
    const double bytes = px * cur.dims * 2.0 * years; // Int16
    constexpr double kMaxBytes = 8.0 * (1ull << 30);
    if (ok) {
        ImGui::TextDisabled("%.1f x %.1f km, %.0f x %.0f px, %d year%s: ~%.0f MB", wM / 1000, hM / 1000,
                            std::ceil(wM / res), std::ceil(hM / res), years, years > 1 ? "s" : "", bytes / (1 << 20));
        ImGui::SetItemTooltip("Int16 GeoTIFFs, one per year (compressed on disk; the download is about as large).\n"
                              "%.4f, %.4f to %.4f, %.4f (longitude, latitude)", ll[0], ll[1], ll[2], ll[3]);
    } else {
        ImGui::TextColored(theme::warning(), "%s", why.c_str());
    }
    const bool zeitReady = zeit_ && zeit_->state() == ZeitClient::State::Ready;
    const char* block = !ok                ? "the area"
                        : bytes > kMaxBytes ? "too large: zoom in, use an ROI or coarser cells"
                        : !zeitReady        ? "Zeit is not ready"
                                            : nullptr;
    ImGui::BeginDisabled(block != nullptr);
    if (ImGui::Button("Add embeddings", ImVec2(-1, 0))) startEmbeddingDownload();
    ImGui::EndDisabled();
    if (block && ok) ImGui::TextColored(theme::warning(), "%s", block);
    else ImGui::SetItemTooltip("Downloads in the background (progress on the map and in Tools > Tasks);\n"
                               "the layer is added on top when it is ready. Saved under %s", resultsDir_.c_str());
    ImGui::PopID();
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

void App::uiEmbeddings() {
    if (!showEmbeddings_) return;
    if (const ImGuiWindow* d = ImGui::FindWindowByName("Display"); d && d->DockId)
        ImGui::SetNextWindowDockID(d->DockId, ImGuiCond_FirstUseEver); // a tab next to Display
    if (!ImGui::Begin("Embeddings", &showEmbeddings_)) {
        ImGui::End();
        return;
    }
    SeriesLayer* L = embeddingLayer();
    if (!L) {
        ImGui::TextWrapped("No layer of embeddings. Open one (a folder of yearly files from Zeit) or add "
                           "the embeddings of the visible area:");
        uiEmbeddingDownload(false);
        ImGui::End();
        return;
    }
    EmbeddingLayer& E = *L->emb;
    const CubeInfo& info = *L->session->info;
    const int T = info.T();
    const int t = layerDate(*L);
    ImGui::TextColored(theme::accent(), "%s", L->name.c_str());
    ImGui::TextWrapped("%s, %s to %s", E.meta.label().c_str(), info.layers.front().label.c_str(),
                       info.layers.back().label.c_str());
    if (!E.meta.attribution.empty() || !E.meta.license.empty())
        ImGui::SetItemTooltip("%s\nLicence: %s", E.meta.attribution.c_str(), E.meta.license.c_str());
    if (&*L != activeLayer()) ImGui::TextDisabled("(not the active layer: drawn on top of it)");
    if (const EmbeddingStore* S = E.store.get()) {
        if (S->failed()) ImGui::TextColored(theme::error(), "%s", S->error().c_str());
        else if (!S->complete())
            ImGui::TextDisabled("reading %d/%d years (%d x %d px)", S->yearsDone(), S->T, S->w, S->h);
        else
            ImGui::TextDisabled("%d x %d px (1:%.2g), %.0f MB, read in %.1f s", S->w, S->h, S->fx,
                                S->bytes() / 1048576.0, S->seconds());
    }

    ImGui::SeparatorText("View");
    for (int v = EmbeddingLayer::Pca; v < EmbeddingLayer::ViewCount; ++v)
        if (ImGui::RadioButton(kViewNames[v], &E.view, v)) mapDirty_ = true;
    if (ImGui::RadioButton(kViewNames[EmbeddingLayer::Band], &E.view, EmbeddingLayer::Band)) mapDirty_ = true;
    ImGui::SetItemTooltip("One dimension as a band, with every mode of the Display panel");

    if (E.view == EmbeddingLayer::Pca) {
        ImGui::SeparatorText("Principal components");
        if (ImGui::RadioButton("Every pixel and year", &E.scope, 0)) mapDirty_ = true;
        ImGui::SetItemTooltip("One PCA for every year: a colour means the same thing in each year,\n"
                              "so playing the series shows real change.");
        ImGui::SameLine();
        int roi[4];
        const bool hasRoi = embeddingRoi(*L, roi);
        if (ImGui::RadioButton("ROI (local)", &E.scope, 1)) mapDirty_ = true;
        ImGui::SetItemTooltip("PCA of the ROI's pixels only (every year), applied to the whole map: the\n"
                              "colours spread over the differences inside the ROI (outside it, they saturate).\n"
                              "Draw or move the ROI with Shift + drag: it is fitted again at once.");
        if (E.scope == 1 && !hasRoi) ImGui::TextColored(theme::warning(), "Draw an ROI: Shift + drag on the map");
        const int k = E.basis ? E.basis->k : 6;
        const char* names[3] = {"R", "G", "B"};
        for (int c = 0; c < 3; ++c) {
            ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x / 3 - 12);
            int v = E.comps[c] + 1;
            if (ImGui::SliderInt(names[c], &v, 1, k, "PC%d")) {
                E.comps[c] = v - 1;
                mapDirty_ = true;
            }
            if (c < 2) ImGui::SameLine();
        }
        ImGui::SetNextItemWidth(-1);
        ImGui::SliderFloat("##stretch", &E.stretch, 0.0f, 10.0f, "stretch: %.1f%% clipped");
        ImGui::SetItemTooltip("Each component is stretched between these percentiles of the sample");
        if (const PcaBasis* B = E.basis.get()) {
            std::string ev;
            for (int c = 0; c < B->k; ++c) {
                char b[32];
                std::snprintf(b, sizeof(b), "%sPC%d %.1f%%", c ? "  " : "", c + 1,
                              B->total > 0 ? 100.0 * B->eig[c] / B->total : 0.0);
                ev += b;
            }
            ImGui::TextWrapped("%s", ev.c_str());
            ImGui::SetItemTooltip("Share of the variance along each component");
            ImGui::TextDisabled("%d vectors, %.1f ms (%s)", B->samples, B->ms, embsimd::name());
            ImGui::SetItemTooltip("Covariance %.2f ms, eigenvectors %.2f ms (%d iterations of subspace iteration);\n"
                                  "image of a year: %.1f ms", B->covMs, B->eigMs, B->iterations,
                                  t < int(E.img.size()) ? E.img[t].ms : 0.0);
        } else {
            ImGui::TextDisabled("fitting...");
        }
    } else if (E.view == EmbeddingLayer::Similarity) {
        ImGui::SeparatorText("Reference");
        const char* kinds[] = {"Cursor (live)", "Pin", "ROI mean"};
        ImGui::SetNextItemWidth(-1);
        if (ImGui::Combo("##refkind", &E.refKind, kinds, 3)) E.refKey = 0;
        if (E.refKind == 1) {
            if (pins_.empty()) ImGui::TextColored(theme::warning(), "Click on the map to drop a pin");
            for (const SeriesView& p : pins_) {
                char b[32];
                std::snprintf(b, sizeof(b), "Pin %d", p.id);
                ImGui::PushStyleColor(ImGuiCol_Text, p.color);
                if (ImGui::RadioButton(b, E.refPin == p.id)) {
                    E.refPin = p.id;
                    E.refKey = 0;
                }
                ImGui::PopStyleColor();
                ImGui::SameLine();
            }
            ImGui::NewLine();
            if (E.refPin == 0 && !pins_.empty()) {
                E.refPin = pins_.front().id;
                E.refKey = 0;
            }
        }
        if (ImGui::Checkbox("Fixed year", &E.refFixedYear)) E.refKey = 0;
        ImGui::SetItemTooltip("Off: the reference is taken in the year shown. On: always in this year\n"
                              "(e.g. how much each place looks, year after year, like the forest of 2017).");
        if (E.refFixedYear) {
            ImGui::SameLine();
            ImGui::SetNextItemWidth(-1);
            if (ImGui::SliderInt("##refyear", &E.refYear, 0, T - 1, info.layers[std::clamp(E.refYear, 0, T - 1)].label.c_str()))
                E.refKey = 0;
        }
        if (!E.refText.empty()) ImGui::TextDisabled("reference: %s", E.refText.c_str());
    } else if (E.view == EmbeddingLayer::Change) {
        ImGui::SeparatorText("Compared with");
        const std::string cur = E.changeBase < 0 ? "Previous year" : info.layers[std::clamp(E.changeBase, 0, T - 1)].label;
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##base", cur.c_str())) {
            if (ImGui::Selectable("Previous year", E.changeBase < 0)) E.changeBase = -1;
            for (int y = 0; y < T; ++y)
                if (ImGui::Selectable(info.layers[y].label.c_str(), E.changeBase == y)) E.changeBase = y;
            ImGui::EndCombo();
        }
        ImGui::TextDisabled("1 - cosine similarity of each pixel's vectors");
    }
    if (E.view == EmbeddingLayer::Similarity || E.view == EmbeddingLayer::Change) {
        EmbeddingLayer::Range& R = E.range[E.view];
        int& cm = E.cmap[E.view];
        ImGui::SetNextItemWidth(-1);
        if (ImPlot::ColormapButton(ImPlot::GetColormapName(cm), ImVec2(-1, 0), cm)) ImGui::OpenPopup("embcmap");
        if (ImGui::BeginPopup("embcmap")) {
            for (int c = 4; c < ImPlot::GetColormapCount(); ++c)
                if (ImPlot::ColormapButton(ImPlot::GetColormapName(c), ImVec2(220, 0), c)) {
                    cm = c;
                    mapDirty_ = true;
                    ImGui::CloseCurrentPopup();
                }
            ImGui::EndPopup();
        }
        float lo = R.lo, hi = R.hi;
        ImGui::SetNextItemWidth(-60);
        if (ImGui::DragFloatRange2("##embrange", &lo, &hi, 0.002f, -1.0f, 2.0f, "%.3f", "%.3f")) {
            R.lo = lo;
            R.hi = std::max(hi, lo + 1e-4f);
            R.manual = true;
            mapDirty_ = true;
        }
        ImGui::SameLine();
        if (ImGui::Button("Auto##emb", ImVec2(-1, 0))) {
            R.manual = false;
            R.key = 0;
            if (t < int(E.img.size()) && !E.img[t].rgba && E.img[t].tex) {
                R.lo = E.img[t].lo;
                R.hi = E.img[t].hi;
            }
            mapDirty_ = true;
        }
        ImGui::SetItemTooltip("2-99.5%% (similarity) or 2-98%% (change) of the year shown");
    }

    // Charts: the similarity of the cursor and pins to the reference, year by
    // year; their vectors in the year shown.
    const EmbeddingStore* S = E.store.get();
    if (S && S->rangesReady) {
        struct Probe {
            std::string label;
            size_t p;
            ImVec4 color;
        };
        std::vector<Probe> probes;
        size_t p;
        if (hover_.x >= 0 && embeddingPixel(*L, hover_.x + 0.5, hover_.y + 0.5, p))
            probes.push_back({"cursor", p, theme::cursorSeries()});
        for (const SeriesView& pin : pins_)
            if (embeddingPixel(*L, pin.x + 0.5, pin.y + 0.5, p))
                probes.push_back({"pin " + std::to_string(pin.id), p, pin.color});
        if (!E.ref.empty() && E.view == EmbeddingLayer::Similarity) {
            ImGui::SeparatorText("Similarity over the years");
            if (ImPlot::BeginPlot("##embsim", ImVec2(-1, 170))) {
                ImPlot::SetupAxes(nullptr, "cosine", ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit);
                double rn = 0;
                for (float v : E.ref) rn += double(v) * v;
                rn = std::sqrt(rn);
                std::vector<float> v(S->D);
                for (const Probe& pr : probes) {
                    std::vector<double> xs, ys;
                    for (int y = 0; y < T; ++y) {
                        if (!S->yearReady(y) || !S->valid(y, pr.p) || S->norm(y, pr.p) <= 0) continue;
                        S->dequant(y, pr.p, v.data());
                        double dot = 0;
                        for (int d = 0; d < S->D; ++d) dot += double(v[d]) * E.ref[d];
                        xs.push_back(info.decimalYear(y));
                        ys.push_back(dot / (rn * S->norm(y, pr.p)));
                    }
                    ImPlotSpec spec;
                    spec.LineColor = theme::onPlot(pr.color);
                    spec.MarkerFillColor = spec.LineColor;
                    spec.Marker = ImPlotMarker_Circle;
                    spec.MarkerSize = 3.0f;
                    ImPlot::PlotLine(pr.label.c_str(), xs.data(), ys.data(), int(xs.size()), spec);
                }
                const double now[1] = {info.decimalYear(t)};
                ImPlotSpec ls;
                ls.LineColor = ImVec4(1, 1, 1, 0.35f);
                ImPlot::PlotInfLines("##now", now, 1, ls);
                ImPlot::EndPlot();
            }
        }
        if (S->yearReady(t) && !probes.empty()) {
            ImGui::SeparatorText("Latent profile");
            ImGui::SetItemTooltip("The %d values of each vector in %s (they have no physical meaning:\n"
                                  "compare their shapes)", S->D, info.layers[t].label.c_str());
            if (ImPlot::BeginPlot("##embprof", ImVec2(-1, 150))) {
                ImPlot::SetupAxes("dimension", nullptr, ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit);
                std::vector<float> v(S->D);
                std::vector<double> xs(S->D), ys(S->D);
                for (int d = 0; d < S->D; ++d) xs[d] = d;
                for (const Probe& pr : probes) {
                    if (!S->valid(t, pr.p)) continue;
                    S->dequant(t, pr.p, v.data());
                    for (int d = 0; d < S->D; ++d) ys[d] = v[d];
                    ImPlotSpec spec;
                    spec.LineColor = theme::onPlot(pr.color);
                    ImPlot::PlotLine(pr.label.c_str(), xs.data(), ys.data(), S->D, spec);
                }
                ImPlot::EndPlot();
            }
        }
    }
    ImGui::Spacing();
    if (ImGui::CollapsingHeader("Download embeddings")) uiEmbeddingDownload(false);
    ImGui::End();
}

// ---------------------------------------------------------------------------
// --selftest-embeddings-ui DIR [OUT]: a series of embeddings in a hidden window
// ---------------------------------------------------------------------------

int App::selfTestEmbeddingStep(const std::vector<std::string>& in) {
    using namespace std::chrono;
    auto clock = [] { return duration<double>(steady_clock::now().time_since_epoch()).count(); };
    const fs::path out = in.size() > 1 ? fs::u8path(in[1]) : fs::u8path(platform::appDataDir()) / "selftest_embeddings";
    auto fail = [&](const std::string& what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what.c_str());
        return 1;
    };
    auto next = [&](const char* done) {
        std::printf("[%5.1f s] %s\n", clock() - st_.t0, done);
        std::fflush(stdout);
        ++st_.stage;
        st_.since = clock();
        st_.frames = 0;
    };
    // Each view as a PNG figure and as a GeoTIFF of its values (the whole layer,
    // at full resolution), checked against the image shown.
    static std::shared_ptr<ExportJob> png, tif;
    static EmbeddingExport spec;
    auto exportTo = [&](const char* name) {
        PngOptions o;
        o.label = o.legend = true;
        o.marks = true;
        png = exportPng((out / name).u8string(), o);
        std::string why;
        if (embeddingExportSpec(true, spec, why))
            tif = exportEmbeddings((out / fs::u8path(name).replace_extension(".tif")).u8string(), spec);
        else
            std::printf("    no GeoTIFF: %s\n", why.c_str());
    };
    // The GeoTIFF's grid, then its values at the source pixel of every store pixel
    // of a grid: the image's (similarity, change) or the scores of the store's
    // vector (principal components), within the store's Int8 steps.
    auto checkTif = [&](const std::string& path) -> std::string {
        const SeriesLayer* EL = embeddingLayer();
        if (!EL || !EL->emb->store) return "no layer of embeddings";
        const EmbeddingLayer* E = EL->emb.get();
        const EmbeddingStore* S = E->store.get();
        GDALDataset* ds = GDALDataset::Open(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY);
        if (!ds) return "could not open " + path;
        const CubeInfo& info = *spec.info;
        const int K = spec.bands();
        double gt[6];
        const bool geo = ds->GetGeoTransform(gt) == CE_None;
        std::string err;
        if (ds->GetRasterXSize() != info.width || ds->GetRasterYSize() != info.height || ds->GetRasterCount() != K)
            err = "the GeoTIFF should have the layer's size and one band per value";
        else if (info.hasGeoTransform && (!geo || std::fabs(gt[0] - info.geoTransform[0]) > 1e-9 ||
                                          std::fabs(gt[3] - info.geoTransform[3]) > 1e-9 ||
                                          std::fabs(gt[1] - info.geoTransform[1]) > 1e-12))
            err = "the GeoTIFF should be on the layer's grid";
        std::vector<float> band(size_t(info.width) * info.height);
        std::vector<float> v(S->D);
        double worst = 0, bound = 0;
        size_t n = 0, clipped = 0;
        // A vector beyond the store's range is clipped there (Int8): the image is off, not the file.
        auto saturated = [&](int t, size_t p) {
            const int8_t* q = S->vec(t, p);
            return S->valid(t, p) && std::any_of(q, q + S->D, [](int8_t c) { return c == 127 || c == -127; });
        };
        for (int k = 0; k < K && err.empty(); ++k) {
            if (ds->GetRasterBand(k + 1)->RasterIO(GF_Read, 0, 0, info.width, info.height, band.data(), info.width,
                                                   info.height, GDT_Float32, 0, 0, nullptr) != CE_None) {
                err = "could not read " + path;
                break;
            }
            for (int j = 0; j < 16 && err.empty(); ++j)
                for (int i = 0; i < 16 && err.empty(); ++i) {
                    const int sx = (S->w - 1) * i / 15, sy = (S->h - 1) * j / 15;
                    const size_t p = size_t(sy) * S->w + sx;
                    if (saturated(spec.t, p) || (spec.kind == EmbeddingExport::Change && saturated(spec.t0, p))) {
                        clipped += k == 0;
                        continue;
                    }
                    const int x = std::min(info.width - 1, int((sx + 0.5) * S->fx));
                    const int y = std::min(info.height - 1, int((sy + 0.5) * S->fy));
                    const float got = band[size_t(y) * info.width + x];
                    float want = NAN, tol = 0.02f;
                    if (spec.kind == EmbeddingExport::Pca) {
                        if (!S->valid(spec.t, p)) continue;
                        const PcaBasis& B = *spec.basis;
                        S->dequant(spec.t, p, v.data());
                        double sc = 0, step = 0;
                        for (int d = 0; d < S->D; ++d) {
                            const double c = B.comps[size_t(k) * S->D + d];
                            sc += (v[d] - B.mean[d]) * c;
                            step += std::fabs(c) * S->scale[d] * 0.5;
                        }
                        want = float(sc);
                        tol = float(step) + 1e-4f;
                    } else {
                        want = E->img[spec.t].values[p];
                    }
                    if (std::isfinite(want) != std::isfinite(got)) {
                        err = "the GeoTIFF's missing values should be the image's";
                        continue;
                    }
                    if (!std::isfinite(want)) continue;
                    worst = std::max(worst, double(std::fabs(got - want)));
                    bound = std::max(bound, double(tol));
                    if (std::fabs(got - want) > tol) {
                        char b[160];
                        std::snprintf(b, sizeof(b), "band %d at (%d, %d): %.5f in the GeoTIFF, %.5f shown", k + 1, x, y,
                                      got, want);
                        err = b;
                        continue;
                    }
                    ++n;
                }
        }
        GDALClose(ds);
        if (err.empty() && n == 0) err = "no value of the GeoTIFF could be compared";
        if (err.empty() && clipped > 256 / 10) err = "the store's ranges should hold the vectors of every year";
        if (err.empty())
            std::printf("    %s: %d x %d px, %d band(s), %zu values within %.4f of the image (at most %.4f); "
                        "%zu clipped vectors left out\n", fs::u8path(path).filename().u8string().c_str(), info.width,
                        info.height, K, n, worst, bound, clipped);
        return err;
    };
    auto exported = [&]() -> int { // -1 waiting, 0 done, 1 failed
        for (auto* j : {&png, &tif})
            if (*j && (*j)->state == ExportJob::State::Running) return -1;
        bool ok = true;
        for (auto* j : {&png, &tif})
            if (*j && (*j)->state != ExportJob::State::Done) {
                std::lock_guard<std::mutex> lk((*j)->m);
                std::printf("    %s: %s\n", (*j)->title.c_str(), (*j)->error.c_str());
                ok = false;
            }
        if (ok && tif) {
            const std::string err = checkTif(tif->path);
            if (!err.empty()) {
                std::printf("    %s\n", err.c_str());
                ok = false;
            }
        }
        png.reset();
        tif.reset();
        return ok ? 0 : 1;
    };
    // Distinct colours over a grid of the map: the image is not flat.
    auto mapColours = [&] {
        std::set<uint32_t> seen;
        const int W = int(canvasSize_.x * mapPixelScale_), H = int(canvasSize_.y * mapPixelScale_);
        for (int j = 1; j < 24; ++j)
            for (int i = 1; i < 24; ++i) {
                unsigned char c[4];
                gpu_.readMapPixel(W * i / 24, H * j / 24, c, 0);
                seen.insert(uint32_t(c[0]) << 16 | uint32_t(c[1]) << 8 | c[2]);
            }
        return int(seen.size());
    };
    if (st_.stage > 0 && clock() - st_.since > 180) return fail("timeout");
    if (openErrorPopup_) return fail("open: " + error_);
    SeriesLayer* L = embeddingLayer();
    EmbeddingLayer* E = L ? L->emb.get() : nullptr;
    const EmbeddingStore* S = E ? E->store.get() : nullptr;
    const int lt = L ? layerDate(*L) : 0; // the embedding layer's date (it may not be the active one)
    auto current = [&] { // the image of the date shown is up to date
        return S && lt < int(E->img.size()) && E->img[lt].tex && E->img[lt].key == embeddingKey(*L, lt);
    };
    ++st_.frames;
    switch (st_.stage) {
    case 0: {
        st_.t0 = st_.since = clock();
        ImGui::GetIO().IniFilename = nullptr; // the user's layout was read; the test's is not saved
        std::error_code ec;
        fs::create_directories(out, ec);
        openInputs({in[0]});
        std::printf("SIMD kernels: %s\n", embsimd::name());
        ++st_.stage;
        return -1;
    }
    case 1: {
        // Another kind of series: the embeddings of its visible area are downloaded
        // through Zeit and added on top, as the Layers panel does.
        if (!E && !layers_.empty() && !layers_.back().emb && layers_.size() == 1) {
            if (!zeit_ || zeit_->state() != ZeitClient::State::Ready || !s_->overview.complete()) return -1;
            embUi_.source = 0;
            embUi_.y0 = 2023;
            embUi_.y1 = 2024;
            embUi_.res = 0;
            embUi_.area = 0;
            double ll[4], wM, hM;
            std::string why;
            if (!embeddingBounds(0, ll, wM, hM, why)) return fail("bounds: " + why);
            std::printf("    %s: downloading AlphaEarth 2023-2024 for %.4f %.4f %.4f %.4f (%.2f x %.2f km)\n",
                        layers_[0].name.c_str(), ll[0], ll[1], ll[2], ll[3], wM / 1000, hM / 1000);
            startEmbeddingDownload();
            if (embDownloads_.empty()) return fail("the download did not start");
            st_.stage = 100;
            st_.since = clock();
            return -1;
        }
        if (!E) return -1;
        if (S && S->failed()) return fail("store: " + S->error());
        // The basis of every year (the first one may come from the years read first).
        if (!S || !S->complete() || !E->basis || E->basis->years != S->T || E->basisHave != E->basisSent ||
            E->engine->busy() || !current() ||
            st_.frames < 3)
            return -1;
        const PcaBasis& B = *E->basis;
        std::printf("    %s: %d years, store %d x %d px (1:%.2f), %.0f MB, read in %.2f s\n", E->meta.label().c_str(),
                    S->T, S->w, S->h, S->fx, S->bytes() / 1048576.0, S->seconds());
        std::printf("    global PCA: %d vectors in %.1f ms (covariance %.2f, eigen %.2f ms, %d it.); PC1-3 %.1f%% %.1f%% "
                    "%.1f%%; image %.1f ms\n", B.samples, B.ms, B.covMs, B.eigMs, B.iterations,
                    100 * B.eig[0] / B.total, 100 * B.eig[1] / B.total, 100 * B.eig[2] / B.total, E->img[lt].ms);
        { // the same sample, the plain way: mean and covariance in double, every eigenvalue (Jacobi)
            const auto sample = sampleVectors(*S, nullptr, 65536, true, 0);
            const int D = S->D;
            std::vector<double> mean(D, 0.0), C(size_t(D) * D, 0.0);
            std::vector<float> v(D);
            for (const auto& yp : sample) {
                S->dequant(yp.first, yp.second, v.data());
                for (int d = 0; d < D; ++d) mean[d] += v[d];
            }
            for (double& m : mean) m /= double(sample.size());
            for (const auto& yp : sample) {
                S->dequant(yp.first, yp.second, v.data());
                for (int r = 0; r < D; ++r)
                    for (int c = r; c < D; ++c) C[size_t(r) * D + c] += (v[r] - mean[r]) * (v[c] - mean[c]);
            }
            double tr = 0;
            for (int r = 0; r < D; ++r) {
                for (int c = r; c < D; ++c) C[size_t(c) * D + r] = C[size_t(r) * D + c] /= double(sample.size() - 1);
                tr += C[size_t(r) * D + r];
            }
            std::vector<double> vecs, vals;
            topEigen(C, D, 3, vecs, vals);
            double dm = 0;
            for (int d = 0; d < D; ++d) dm = std::max(dm, std::fabs(mean[d] - B.mean[d]));
            std::printf("    plain PCA of the same sample: PC1-3 %.1f%% %.1f%% %.1f%% (total %.4g vs %.4g, max mean diff %.2g, "
                        "eig %.4g %.4g %.4g vs %.4g %.4g %.4g)\n", 100 * vals[0] / tr, 100 * vals[1] / tr, 100 * vals[2] / tr,
                        tr, B.total, dm, vals[0], vals[1], vals[2], B.eig[0], B.eig[1], B.eig[2]);
            for (int c = 0; c < 3; ++c)
                if (std::fabs(vals[c] - B.eig[c]) > 1e-4 * vals[0]) return fail("the PCA differs from the plain one");
            if (std::fabs(tr - B.total) > 1e-4 * tr) return fail("the total variance differs from the plain one");
        }
        const int colours = mapColours();
        std::printf("    %d distinct colours on a 23 x 23 grid of the map\n", colours);
        if (colours < 40) return fail("the principal components should colour the map");
        exportTo("pca.png");
        next("principal components of every year");
        return -1;
    }
    case 2: {
        if (const int r = exported(); r != 0) return r < 0 ? -1 : fail("export");
        // ROI: the central ninth of the image; local PCA.
        const CubeInfo& info = *s_->info;
        const int r[4] = {info.width / 3, info.height / 3, 2 * info.width / 3, 2 * info.height / 3};
        setRoiRect(r);
        E->scope = 1;
        next("pca.png");
        return -1;
    }
    case 3: {
        if (!E->basisLocal || !current() || st_.frames < 3) return -1;
        const PcaBasis& B = *E->basis;
        std::printf("    local PCA: %d vectors in %.1f ms; PC1-3 %.1f%% %.1f%% %.1f%%\n", B.samples, B.ms,
                    100 * B.eig[0] / B.total, 100 * B.eig[1] / B.total, 100 * B.eig[2] / B.total);
        if (B.samples <= 0) return fail("no vectors in the ROI");
        exportTo("pca_roi.png");
        next("local PCA of the ROI");
        return -1;
    }
    case 4: {
        if (const int r = exported(); r != 0) return r < 0 ? -1 : fail("export");
        E->view = EmbeddingLayer::Similarity;
        E->refKind = 1;
        const CubeInfo& info = *s_->info;
        addPin(info.width / 2, info.height / 2);
        E->refPin = pins_.back().id;
        E->refKey = 0;
        mapDirty_ = true;
        next("pca_roi.png");
        return -1;
    }
    case 5: {
        if (!current() || st_.frames < 3) return -1;
        size_t p;
        const SeriesView& pin = pins_.back();
        if (!embeddingPixel(*L, pin.x + 0.5, pin.y + 0.5, p)) return fail("pin off the store");
        const float self = E->img[lt].values[p];
        std::printf("    similarity: pin %d -> %.6f at itself, range %.3f .. %.3f, image %.1f ms\n", pin.id, self,
                    E->img[lt].lo, E->img[lt].hi, E->img[lt].ms);
        if (!(self > 0.999f)) return fail("the similarity of the reference to itself should be 1");
        exportTo("similarity.png");
        next("similarity to a pin");
        return -1;
    }
    case 6: {
        if (const int r = exported(); r != 0) return r < 0 ? -1 : fail("export");
        if (L->session->info->T() < 2) { // one year: no change to show
            next("similarity.png");
            st_.stage = 8;
            return -1;
        }
        E->view = EmbeddingLayer::Change;
        setT(s_->info->T() - 1);
        mapDirty_ = true;
        next("similarity.png");
        return -1;
    }
    case 7: {
        if (!current() || st_.frames < 3) return -1;
        const std::vector<float>& v = E->img[lt].values;
        size_t n = 0;
        for (float x : v)
            if (std::isfinite(x)) {
                if (x < -1e-4f || x > 2.0001f) return fail("a cosine distance outside [0, 2]");
                ++n;
            }
        std::printf("    change %s: %zu pixels, range %.3f .. %.3f, image %.1f ms\n",
                    L->session->info->layers[lt].label.c_str(), n, E->img[lt].lo, E->img[lt].hi, E->img[lt].ms);
        if (n == 0) return fail("no change values");
        exportTo("change.png");
        next("change from the previous year");
        return -1;
    }
    case 8: {
        if (const int r = exported(); r != 0) return r < 0 ? -1 : fail("export");
        // Every year in the principal components, as when playing: time per image.
        E->view = EmbeddingLayer::Pca;
        E->scope = 0;
        setT(0);
        next("change.png");
        return -1;
    }
    case 100: { // the download: progress on the map, then the layer on top, the series still active
        if (!embDownloads_.empty()) {
            const ZeitJob& j = *embDownloads_.back().job;
            if (j.state == ZeitJob::State::Failed) {
                std::lock_guard<std::mutex> lk(const_cast<ZeitJob&>(j).m);
                return fail("download: " + j.error);
            }
            static std::string shown; // each progress message once
            if (j.state == ZeitJob::State::Running) {
                std::lock_guard<std::mutex> lk(const_cast<ZeitJob&>(j).m);
                if (!j.message.empty() && j.message != shown) {
                    shown = j.message;
                    std::printf("    %3.0f%%  %s\n", j.progress * 100.0, j.message.c_str());
                }
            }
        }
        if (!E) return -1;
        if (layers_.size() != 2 || active_ != 0 || layers_[0].emb) return fail("the series should stay active, the embeddings on top");
        std::printf("    added: %s, %s; %s\n", L->name.c_str(), E->meta.label().c_str(),
                    L->reproj ? ("reprojected from " + L->reproj->crsText).c_str() : "same grid");
        if (!L->aligned) return fail("the embeddings could not be placed on the series");
        st_.stage = 1;
        st_.since = clock();
        return -1;
    }
    case 9: {
        if (!current()) return -1;
        if (t_ + 1 < s_->info->T()) {
            setT(t_ + 1);
            return -1;
        }
        double ms = 0;
        for (const auto& i : E->img) ms += i.ms;
        std::printf("    every year in principal components: %.1f ms per image\n", ms / E->img.size());
        st_.stage = 10;
        st_.frames = 0;
        return -1;
    }
    case 10: { // Ctrl+Shift+M (File > Export embeddings as GeoTIFF...): its options popup, named after the view
        ImGuiIO& io = ImGui::GetIO();
        const bool down = st_.frames == 1;
        io.AddKeyEvent(ImGuiMod_Ctrl, down);
        io.AddKeyEvent(ImGuiMod_Shift, down);
        io.AddKeyEvent(ImGuiKey_M, down);
        if (down) return -1;
        const std::string path = exportPath_;
        std::printf("    Ctrl+Shift+M: export popup open: %s, saving to %s\n", ImGui::IsPopupOpen("Export###export") ? "yes" : "no",
                    path.c_str());
        if (!ImGui::IsPopupOpen("Export###export")) return fail("the embeddings export popup should open");
        if (path.size() < 8 || path.compare(path.size() - 8, 8, "_pca.tif") != 0)
            return fail("the embeddings export popup should propose <layer>_<year>_pca.tif");
        ImGui::ClosePopupToLevel(0, false);
        std::printf("OK (%.1f s); images in %s\n", clock() - st_.t0, out.u8string().c_str());
        return 0;
    }
    }
    return -1;
}
