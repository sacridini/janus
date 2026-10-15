// Validation in the interface (validation.hpp holds the statistics and the
// project file): the sampling design over any map (strata, sample size,
// allocation, the expected errors), the sample drawn into a project (a
// GeoPackage) and interpreted point by point (blind to the maps assessed), and
// the accuracy and error-adjusted areas of every map assessed with the
// sample, recomputed as labels come (microseconds: the maps' values at the
// points and their areas are read once, in the background). Also the Chips
// panel (the current feature at every date, read ahead for the next ones), the
// pixel's square on the map, interpreters' agreement and adjudication, the
// report and the Python that gives the same numbers.
#define IMGUI_DEFINE_MATH_OPERATORS
#include "app.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <deque>
#include <filesystem>
#include <fstream>
#include <random>
#include <set>

#include <gdal_priv.h>
#include <ogrsf_frmts.h>
#include <imgui_internal.h>
#include <implot.h>

#include "glfw.hpp"
#include "platform.hpp"
#include "profiler.hpp"

namespace fs = std::filesystem;

namespace {

const std::vector<std::string> kEventLabels = {"Stable forest", "Deforestation", "Degradation", "Regeneration",
                                               "Agriculture",   "Pasture",       "Water",       "Cloud / no data",
                                               "Other"};

constexpr ImU32 kMine = IM_COL32(80, 200, 120, 255);     // labelled by this interpreter
constexpr ImU32 kOthers = IM_COL32(90, 160, 255, 255);   // by others only
constexpr ImU32 kConflict = IM_COL32(240, 70, 60, 255);  // interpreters disagree
constexpr ImU32 kSettled = IM_COL32(190, 120, 255, 255); // adjudicated
constexpr ImU32 kRight = IM_COL32(80, 200, 120, 255);
constexpr ImU32 kWrong = IM_COL32(240, 70, 60, 255);
constexpr ImU32 kNoRef = IM_COL32(150, 150, 150, 255);

std::string fmtArea(double v) {
    char b[48];
    if (!std::isfinite(v)) return "-";
    if (std::fabs(v) >= 100) std::snprintf(b, sizeof(b), "%.0f", v);
    else if (std::fabs(v) >= 1) std::snprintf(b, sizeof(b), "%.1f", v);
    else std::snprintf(b, sizeof(b), "%.3g", v);
    // thousands separators
    std::string s = b;
    const size_t dot = s.find('.');
    size_t end = dot == std::string::npos ? s.size() : dot;
    const size_t start = s[0] == '-' ? 1 : 0;
    for (int k = int(end) - 3; k > int(start); k -= 3) s.insert(size_t(k), ",");
    return s;
}

std::string fmtPm(const val::Est& e) {
    if (!std::isfinite(e.est)) return "-";
    char b[64];
    std::snprintf(b, sizeof(b), "%.3f +- %.3f", e.est, e.hi - e.est);
    return b;
}

std::string csvCell(const std::string& s) {
    return csvField(s);
}

std::string htmlEsc(const std::string& s) {
    std::string o;
    for (char c : s) {
        if (c == '<') o += "&lt;";
        else if (c == '>') o += "&gt;";
        else if (c == '&') o += "&amp;";
        else if (c == '"') o += "&quot;";
        else o += c;
    }
    return o;
}

// The change of a prepared map as a binary map: its user's and producer's
// accuracy and the area of change (the classes the rule calls change; for the
// reference, those classes and, for maps of events, every period).
struct BinaryChange {
    bool defined = false;
    val::Est users, producers, area;
    double mapped = 0;
};

BinaryChange binaryChange(const val::MapRule& r, const val::Prepared& p) {
    BinaryChange b;
    const size_t C = p.in.classes.size(), N = p.in.stratum.size();
    std::vector<char> isChange(C, 0);
    for (size_t c = 0; c < C; ++c) {
        const int k = p.classKeys[c];
        isChange[c] = k != val::kNone ? r.isChange(k) : (r.kind == val::MapKind::Events && p.in.classes[c] != val::kNoChange);
        b.defined |= isChange[c] != 0;
        if (isChange[c] && c < p.in.mapped.size()) b.mapped += p.in.mapped[c];
    }
    if (!b.defined) return b;
    std::vector<int> st(N, -1);
    for (size_t i = 0; i < N; ++i)
        if (p.in.stratum[i] >= 0 && p.in.map[i] >= 0 && p.in.ref[i] >= 0) st[i] = p.in.stratum[i];
    const val::Estimator e(p.in.stratumArea, st, p.in.confidence);
    std::vector<double> m(N, 0), rf(N, 0), both(N, 0);
    for (size_t i = 0; i < N; ++i) {
        if (st[i] < 0) continue;
        m[i] = isChange[p.in.map[i]];
        rf[i] = isChange[p.in.ref[i]];
        both[i] = m[i] * rf[i];
    }
    b.users = e.ratio(both, m);
    b.producers = e.ratio(both, rf);
    const val::Est a = e.mean(rf);
    double total = 0;
    for (double v : p.in.stratumArea) total += std::max(0.0, v);
    b.area = {a.est * total, a.se * total, a.lo * total, a.hi * total};
    return b;
}

} // namespace

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct App::Validation {
    // --- Design ---
    std::vector<val::MapRule> maps;           // the maps to stratify
    std::vector<std::string> binsText;        // each one's bins, as typed
    std::vector<std::string> changeText;      // each one's change classes, as typed (classes without names)
    int buffer = 0;
    int limit = 0;                            // 0 every pixel, 1 the ROI, 2 a vector layer's polygons
    int limitVector = 0;
    struct StrataJob {
        val::Strata s;
        std::string path, error;
    };
    std::shared_ptr<val::Progress> strataProg;
    std::future<StrataJob> strataJob;
    val::Strata strata;                       // the last computed
    std::string strataPath, strataFor;        // its raster, the spec it was computed for
    int previewCount = 0;
    int alloc = 0;                            // val::Alloc
    float targetSe = 0.01f;
    int minPer = 50;
    bool nAuto = true;
    int n = 0;
    int seed = 42;
    float confidence = 0.95f;
    std::string designError;
    struct DrawJob {
        val::Project p;
        std::string error;
        bool more = false;                    // points added to the open project
        std::vector<val::Point> added;
    };
    std::future<DrawJob> drawJob;
    std::deque<std::string> toOpen;           // rasters to open as layers (one open at a time)
    std::deque<std::string> toClose;          // layers to close (by their file)
    int moreStratum = 0, moreN = 25;

    // --- Project ---
    std::unique_ptr<val::Project> project;
    int vectorId = 0;
    struct MapData {
        std::vector<double> values;
        std::map<int, double> mapped;
        bool valuesReady = false, mappedReady = false;
        std::future<std::pair<std::vector<double>, std::string>> valuesJob;
        std::future<std::pair<std::map<int, double>, std::string>> mappedJob;
        std::shared_ptr<val::Progress> prog = std::make_shared<val::Progress>();
        std::string error;
        uint64_t version = 0;                 // of the cached assessment
        val::Prepared prep;
        val::Assessment a;
        BinaryChange change;
        val::DateAgreement dates;
        float tolerance = -1;
        bool guessed = false;                 // translations guessed for its classes
        bool ready() const { return valuesReady && mappedReady; }
    };
    std::vector<std::unique_ptr<MapData>> data; // per project->maps
    int selected = 0;
    uint64_t version = 1;                     // labels, translations or maps changed
    bool blind = true;
    std::vector<std::string> hidden;          // files of layers the blind mode hid (shown again after)
    int colorBy = 0;                          // 0 state, 1 stratum, 2 the selected map right or wrong
    bool interpretWhenOpen = false;           // opened before any series: interpreted once one is
    uint64_t colorsFor = 0;

    // Saving: labels a second after the last change, the project's tables when they change; one write at a time.
    std::map<int, val::Label> pending;
    std::string pendingAs;
    bool metaDirty = false;
    std::deque<std::function<std::string()>> tasks;
    std::future<std::string> writer;
    std::string writeError;
    std::string message;

    // Accuracy panel
    int matrixMode = 0;                       // 0 points, 1 proportions of area
    float tolerance = 1;
    int convClass = -1;
    struct Conv {
        uint64_t key = 0;
        std::vector<double> n, oa, oaLo, oaHi, area, lo, hi;
    } conv;
    struct ByYear {
        uint64_t key = 0;
        std::vector<std::pair<int, val::ClassArea>> rows;
    } byYear;
    int selectTab = -1;
    int sections = -1;                        // the screenshots: Accuracy's sections open (bits, in order; -1: as left)
    bool section(const char* name, int bit) {
        if (sections >= 0) ImGui::SetNextItemOpen((sections >> bit) & 1);
        return ImGui::CollapsingHeader(name);
    }

    // The pixel's square on the map.
    std::unique_ptr<VectorPlacer> placer;
    uint64_t placerKey = 0;
    int placerVector = 0;

    // --- Chips: the current feature at every date ---
    struct ChipSet {
        uint64_t cube = 0;
        int x0 = 0, y0 = 0, w = 0, h = 0;     // the window read (the layer's pixels)
        int cx = 0, cy = 0;                   // the feature's pixel
        int T = 0;
        bool rgb = false;
        std::vector<float> v;                 // [t][band][y][x], 1 or 3 bands
    };
    struct ChipJob {
        uint64_t key = 0;
        std::shared_future<std::shared_ptr<ChipSet>> f;
        double used = 0;
    };
    std::vector<ChipJob> chips;
    int chipRadius = 12;
    bool chipPerYear = false;
    float chipSize = 84;
    GpuTex chipTex = 0;
    uint64_t chipTexKey = 0;
    int chipCols = 0, chipCell = 0, chipTexW = 0, chipTexH = 0;
    std::vector<int> chipDates;               // the dates drawn, in the texture's order
    std::shared_ptr<ChipSet> chipShown;

    // A map to stratify or to assess, from what is open.
    struct Source {
        std::string label;
        val::MapRule rule;
    };
    static std::vector<Source> sources(App& a);
    static val::MapRule fileRule(const std::string& path);
    // A button and its popup: a map among the open ones, or a raster file.
    static bool pickSource(const std::vector<Source>& src, val::MapRule& out, const char* label);
    MapData& addMapData(App& a, size_t k);
};

std::vector<App::Validation::Source> App::Validation::sources(App& a) {
    std::vector<Source> out;
    for (const ResultLayer& r : a.results_) {
        Source s;
        s.label = r.name;
        s.rule.path = r.path;
        s.rule.name = r.name;
        if (!r.classes.empty()) {
            s.rule.kind = val::MapKind::Classes;
            for (size_t k = 0; k < r.classes.size(); ++k) s.rule.names[int(k) + 1] = r.classes[k];
        } else if (r.unit == "year" || r.unit == "years") {
            s.rule.kind = val::MapKind::Events;
        } else {
            s.rule.kind = val::MapKind::Intervals;
            const double lo = r.lo, hi = r.hi > r.lo ? r.hi : r.lo + 1;
            s.rule.bins = {lo, lo + (hi - lo) / 3, lo + 2 * (hi - lo) / 3, hi};
        }
        out.push_back(std::move(s));
    }
    for (const SeriesLayer& L : a.layers_) {
        const CubeInfo& info = *L.session->info;
        if (info.spectral || !info.hasGeoTransform) continue;
        const int t = a.layerDate(L);
        Source s;
        s.label = L.name + " @ " + info.layers[t].label;
        s.rule.path = info.layers[t].path;
        s.rule.band = info.layers[t].band;
        s.rule.name = s.label;
        if (L.classes.state == LayerClasses::On) {
            s.rule.kind = val::MapKind::Classes;
            for (const ClassEntry& c : L.classes.list) s.rule.names[c.value] = c.name;
        } else {
            s.rule.kind = val::MapKind::Intervals;
            const auto& rg = &L == a.activeLayer() ? a.range_[ModeValue] : L.disp.range[ModeValue];
            const double lo = rg.lo, hi = rg.hi > rg.lo ? rg.hi : rg.lo + 1;
            s.rule.bins = {lo, lo + (hi - lo) / 3, lo + 2 * (hi - lo) / 3, hi};
        }
        out.push_back(std::move(s));
    }
    return out;
}

val::MapRule App::Validation::fileRule(const std::string& path) {
    val::MapRule r;
    r.path = path;
    r.name = fs::u8path(path).filename().u8string();
    r.kind = val::MapKind::Intervals;
    CPLErrorReset();
    if (GDALDataset* ds = GDALDataset::FromHandle(GDALOpenEx(path.c_str(), GDAL_OF_RASTER | GDAL_OF_READONLY, nullptr, nullptr, nullptr))) {
        GDALRasterBand* b = ds->GetRasterBand(1);
        const GDALDataType t = b ? b->GetRasterDataType() : GDT_Float32;
        if (b && !GDALDataTypeIsFloating(t)) r.kind = val::MapKind::Classes;
        if (b)
            if (char** cats = b->GetCategoryNames())
                for (int k = 0; cats[k]; ++k)
                    if (cats[k][0]) r.names[k] = cats[k];
        if (r.kind == val::MapKind::Intervals && b) {
            int ok = 0;
            double mn = b->GetMinimum(&ok), mx = b->GetMaximum(&ok);
            double mm[2];
            if (!ok && b->ComputeRasterMinMax(TRUE, mm) == CE_None) {
                mn = mm[0];
                mx = mm[1];
            }
            const std::string lower = [&] {
                std::string s = r.name;
                for (char& c : s) c = char(std::tolower(static_cast<unsigned char>(c)));
                return s;
            }();
            if (lower.find("yod") != std::string::npos || lower.find("year") != std::string::npos ||
                (mn >= 0 && mx > 1900 && mx < 2200)) {
                r.kind = val::MapKind::Events;
            } else if (mx > mn) {
                r.bins = {mn, mn + (mx - mn) / 3, mn + 2 * (mx - mn) / 3, std::nextafter(mx, HUGE_VAL)};
            }
        }
        GDALClose(ds);
    }
    return r;
}

App::Validation::MapData& App::Validation::addMapData(App& a, size_t k) {
    data.resize(std::max(data.size(), k + 1));
    data[k] = std::make_unique<MapData>();
    MapData& d = *data[k];
    d.prog->threads = a.settings_.processingThreads();
    const val::Project& p = *project;
    const val::AssessedMap& m = p.maps[k];
    // A map of the strata: its areas came with them.
    for (size_t s = 0; s < p.spec.maps.size() && s < p.strata.mapped.size(); ++s) {
        const val::MapRule& r = p.spec.maps[s];
        if (r.path == m.rule.path && r.band == m.rule.band && r.kind == m.rule.kind && r.bins == m.rule.bins) {
            d.mapped = p.strata.mapped[s];
            d.mappedReady = true;
        }
    }
    std::vector<std::array<double, 2>> xy;
    for (const val::Point& q : p.points) xy.push_back({q.x, q.y});
    const std::string wkt = p.strata.grid.wkt;
    d.valuesJob = std::async(std::launch::async, [rule = m.rule, wkt, xy]() {
        std::pair<std::vector<double>, std::string> r;
        val::valuesAt(rule, wkt, xy, r.first, r.second);
        glfwPostEmptyEvent();
        return r;
    });
    if (!d.mappedReady)
        d.mappedJob = std::async(std::launch::async, [rule = m.rule, path = p.strataPath, strata = p.strata, prog = d.prog]() {
            std::pair<std::map<int, double>, std::string> r;
            val::mappedAreas(rule, path, strata, r.first, prog.get(), r.second);
            glfwPostEmptyEvent();
            return r;
        });
    return d;
}

std::string App::interpreterName() const {
    if (!interpreter_.empty()) return interpreter_;
    std::string u = platform::getEnv("USERNAME");
    if (u.empty()) u = platform::getEnv("USER");
    return u.empty() ? "interpreter" : u;
}

bool App::validationProjectOpen() const {
    return val_ && val_->project;
}

bool App::isProjectVector(int vectorId) const {
    return val_ && val_->project && vectorId && val_->vectorId == vectorId;
}

// ---------------------------------------------------------------------------
// Project: opening, closing, saving
// ---------------------------------------------------------------------------

namespace {

std::shared_ptr<VectorData> samplesData(const val::Project& p) {
    auto d = std::make_shared<VectorData>();
    d->path = p.path;
    d->layer = "samples";
    d->crsWkt = p.strata.grid.wkt;
    d->fields = {"sample_id"}; // the stratum stays out of sight (blind interpretation)
    for (const val::Point& q : p.points) {
        VectorFeature f;
        f.fid = q.id;
        f.kind = VectorFeature::Point;
        f.parts = {{q.x, q.y}};
        f.cx = q.x;
        f.cy = q.y;
        f.values = {std::to_string(q.id)};
        d->features.push_back(std::move(f));
        ++d->points;
        ++d->vertices;
    }
    return d;
}

} // namespace

void App::openValidationProject(const std::string& path) {
    val::Project p;
    std::string err;
    if (!val::loadProject(path, p, err)) {
        error_ = err;
        openErrorPopup_ = true;
        return;
    }
    if (!val_) val_ = std::make_shared<Validation>();
    closeValidationProject();
    Validation& v = *val_;
    v.project = std::make_unique<val::Project>(std::move(p));
    val::Project& P = *v.project;
    std::error_code ec;
    if (!fs::exists(fs::u8path(P.strataPath), ec))
        v.message = "The strata raster is missing (" + P.strataPath + "): the areas of maps added now cannot be read.";
    VectorLayer V;
    V.id = nextVectorId_++;
    V.data = samplesData(P);
    V.name = "Sample: " + fs::u8path(P.path).filename().u8string();
    V.color = ImVec4(1.0f, 0.85f, 0.2f, 1.0f);
    vectors_.push_back(std::move(V));
    v.vectorId = vectors_.back().id;
    placeVector(vectors_.back());
    if (P.maps.empty())
        for (const val::MapRule& r : P.spec.maps) {
            val::AssessedMap m;
            m.rule = r;
            P.maps.push_back(m);
        }
    v.data.clear();
    for (size_t k = 0; k < P.maps.size(); ++k) v.addMapData(*this, k);
    v.selected = 0;
    ++v.version;
    v.blind = true;
    showValidation_ = true;
    v.interpretWhenOpen = !s_;
    if (s_) startInterpretation(v.vectorId);
}

void App::closeValidationProject() {
    if (!val_ || !val_->project) return;
    Validation& v = *val_;
    if (lab_.project) stopInterpretation(); // writes what is pending
    validationSaveLabels();
    if (v.metaDirty) {
        val::Project copy = *v.project;
        v.tasks.push_back([copy]() {
            std::string e;
            val::saveProjectMeta(copy, e);
            return e;
        });
        v.metaDirty = false;
    }
    // Every write done before the project goes.
    for (;;) {
        if (v.writer.valid()) {
            const std::string e = v.writer.get();
            if (!e.empty()) v.writeError = e;
        }
        if (v.tasks.empty()) break;
        v.writer = std::async(std::launch::async, v.tasks.front());
        v.tasks.pop_front();
    }
    setValidationBlind(false);
    for (size_t i = 0; i < vectors_.size(); ++i)
        if (vectors_[i].id == v.vectorId) {
            removeVector(int(i));
            break;
        }
    v.vectorId = 0;
    v.data.clear();
    v.project.reset();
    v.placer.reset();
    v.placerVector = 0;
}

void App::validationEntries() {
    if (!val_ || !val_->project) return;
    const val::Project& P = *val_->project;
    lab_.classes = P.labelSet.empty() ? kEventLabels : P.labelSet;
    lab_.entries.assign(P.points.size(), LabelEntry{});
    const auto it = P.labels.find(lab_.as);
    if (it != P.labels.end())
        for (size_t i = 0; i < it->second.size() && i < lab_.entries.size(); ++i) lab_.entries[i] = it->second[i];
}

void App::validationLabelsChanged(int feature) {
    if (!val_ || !val_->project || feature < 0 || feature >= int(lab_.entries.size())) return;
    Validation& v = *val_;
    val::Project& P = *v.project;
    if (!v.pending.empty() && v.pendingAs != lab_.as) validationSaveLabels();
    std::vector<val::Label>& mine = P.labels[lab_.as];
    mine.resize(P.points.size());
    mine[feature] = lab_.entries[feature];
    v.pending[feature] = lab_.entries[feature];
    v.pendingAs = lab_.as;
    ++v.version;
}

void App::validationSaveLabels() {
    if (!val_ || !val_->project || val_->pending.empty()) return;
    Validation& v = *val_;
    const val::Project& P = *v.project;
    std::vector<std::pair<int, val::Label>> rows(v.pending.begin(), v.pending.end());
    std::vector<val::FinalRef> finals;
    for (const auto& [i, l] : rows) {
        val::FinalRef f;
        f.point = i;
        bool conflict = false;
        if (const val::Label* r = val::resolve(P.labels, size_t(i), conflict)) f.ref = *r;
        f.conflict = conflict;
        for (const auto& [who, vv] : P.labels)
            f.interpreters += who != val::kAdjudicated && size_t(i) < vv.size() && !vv[i].label.empty();
        finals.push_back(f);
    }
    v.tasks.push_back([path = P.path, as = v.pendingAs, rows = std::move(rows), finals = std::move(finals)]() {
        std::string e;
        val::saveLabels(path, as, rows, finals, e);
        glfwPostEmptyEvent();
        return e;
    });
    v.pending.clear();
}

void App::validationLabelSetChanged() {
    if (!val_ || !val_->project) return;
    Validation& v = *val_;
    val::Project& P = *v.project;
    P.labelSet = lab_.classes;
    // New labels: their translations guessed for every map.
    for (size_t k = 0; k < P.maps.size(); ++k) {
        std::vector<std::string> names;
        if (k < v.data.size() && v.data[k])
            for (int c : val::mapClasses(P.maps[k].rule, v.data[k]->values, v.data[k]->mapped))
                names.push_back(P.maps[k].rule.className(c));
        for (const auto& [l, t] : val::defaultMapping(P.maps[k].rule, P.labelSet, names))
            if (!P.maps[k].mapping.count(l)) P.maps[k].mapping[l] = t;
    }
    v.metaDirty = true;
    ++v.version;
}

void App::setValidationBlind(bool on) {
    if (!val_) return;
    Validation& v = *val_;
    v.blind = on;
    if (on) return; // pumpValidation hides them
    for (const std::string& f : v.hidden) {
        for (ResultLayer& r : results_)
            if (r.path == f) r.visible = true;
        for (SeriesLayer& L : layers_)
            if (L.inputs.size() == 1 && L.inputs[0] == f) L.visible = true;
    }
    v.hidden.clear();
    mapDirty_ = true;
}

void App::startAdjudication() {
    if (!val_ || !val_->project) return;
    const val::Project& P = *val_->project;
    std::vector<int> conflicts;
    for (size_t i = 0; i < P.points.size(); ++i) {
        bool c = false;
        val::resolve(P.labels, i, c);
        if (c) conflicts.push_back(int(i));
    }
    if (lab_.vector != val_->vectorId) startInterpretation(val_->vectorId);
    if (!lab_.project) return;
    if (lab_.dirty) saveLabels();
    lab_.as = val::kAdjudicated;
    validationEntries();
    lab_.filter = conflicts;
    lab_.filterName = std::to_string(conflicts.size()) + " disagreement(s), adjudicating";
    if (!conflicts.empty()) interpGo(conflicts.front());
    showInterp_ = true;
}

// ---------------------------------------------------------------------------
// Pump (every frame)
// ---------------------------------------------------------------------------

void App::pumpValidation() {
    PROFILE_ZONE();
    if (!val_) return;
    Validation& v = *val_;
    auto ready = [](auto& f) { return f.valid() && f.wait_for(std::chrono::seconds(0)) == std::future_status::ready; };

    // Strata computed: shown as a layer (their raster has names and colours), the previous ones closed.
    if (ready(v.strataJob)) {
        Validation::StrataJob j = v.strataJob.get();
        if (!j.error.empty()) {
            v.designError = j.error;
        } else {
            if (!v.strataPath.empty()) v.toClose.push_back(v.strataPath);
            v.strata = std::move(j.s);
            v.strataPath = j.path;
            v.designError.clear();
            v.toOpen.push_back(j.path);
            for (val::Stratum& s : v.strata.strata) s.expectedUa = s.change || s.buffer ? 0.7 : 0.9;
        }
    }
    // The sample drawn: a new project, or points added to the open one.
    if (ready(v.drawJob)) {
        Validation::DrawJob j = v.drawJob.get();
        if (!j.error.empty()) {
            v.designError = j.error;
        } else if (j.more && v.project && j.p.path == v.project->path) {
            val::Project& P = *v.project;
            P.points.insert(P.points.end(), j.added.begin(), j.added.end());
            P.strata.strata = j.p.strata.strata;
            for (auto& [who, l] : P.labels) l.resize(P.points.size());
            if (VectorLayer* V = vectorById(v.vectorId)) {
                V->data = samplesData(P);
                V->placedFor = 0;
                V->colors.clear();
            }
            if (lab_.project) lab_.entries.resize(P.points.size());
            for (size_t k = 0; k < P.maps.size(); ++k) v.addMapData(*this, k);
            ++v.version;
            v.message = std::to_string(j.added.size()) + " point(s) added to the sample.";
        } else {
            const std::string path = j.p.path;
            if (v.project) closeValidationProject();
            // The preview's raster is replaced by the project's copy.
            if (!v.strataPath.empty()) v.toClose.push_back(v.strataPath);
            v.strataPath.clear();
            v.strataFor.clear();
            openValidationProject(path);
            if (v.project) v.toOpen.push_back(v.project->strataPath);
        }
    }
    // One raster opened at a time (the series being studied stays active); layers closed by file.
    while (!v.toClose.empty()) {
        const std::string f = v.toClose.front();
        v.toClose.pop_front();
        for (int i = int(layers_.size()) - 1; i >= 0; --i)
            if (layers_[i].inputs.size() == 1 && layers_[i].inputs[0] == f && i != active_) removeLayer(i);
    }
    if (!v.toOpen.empty() && !opening_.valid() && !layers_.empty()) {
        const std::string f = v.toOpen.front();
        v.toOpen.pop_front();
        openingKeepActive_ = true;
        openingName_ = "Strata: " + fs::u8path(f).stem().u8string();
        openInputs({f}, true);
    }
    if (!v.project) return;
    val::Project& P = *v.project;
    if (!vectorById(v.vectorId)) { // removed from the Layers panel: the project's points come back
        VectorLayer V;
        V.id = nextVectorId_++;
        V.data = samplesData(P);
        V.name = "Sample: " + fs::u8path(P.path).filename().u8string();
        V.color = ImVec4(1.0f, 0.85f, 0.2f, 1.0f);
        vectors_.push_back(std::move(V));
        v.vectorId = vectors_.back().id;
        v.colorsFor = 0;
    }

    if (v.interpretWhenOpen && s_ && !opening_.valid()) {
        v.interpretWhenOpen = false;
        startInterpretation(v.vectorId);
    }

    // The maps' values at the points and their areas.
    for (auto& dp : v.data) {
        if (!dp) continue;
        Validation::MapData& d = *dp;
        if (ready(d.valuesJob)) {
            auto r = d.valuesJob.get();
            d.values = std::move(r.first);
            d.valuesReady = r.second.empty();
            if (!r.second.empty()) d.error = r.second;
            ++v.version;
        }
        if (ready(d.mappedJob)) {
            auto r = d.mappedJob.get();
            d.mapped = std::move(r.first);
            d.mappedReady = r.second.empty();
            if (!r.second.empty()) d.error = r.second;
            ++v.version;
        }
    }
    // Translations for labels that have none yet (a map added, its classes known).
    for (size_t k = 0; k < P.maps.size() && k < v.data.size(); ++k) {
        Validation::MapData& d = *v.data[k];
        if (!d.ready() || d.guessed) continue;
        d.guessed = true;
        std::vector<std::string> names;
        for (int c : val::mapClasses(P.maps[k].rule, d.values, d.mapped)) names.push_back(P.maps[k].rule.className(c));
        bool added = false;
        for (const auto& [l, t] : val::defaultMapping(P.maps[k].rule, P.labelSet, names))
            if (!P.maps[k].mapping.count(l)) {
                P.maps[k].mapping[l] = t;
                added = true;
            }
        if (added) {
            v.metaDirty = true;
            ++v.version;
        }
    }

    // Writes, one at a time.
    if (ready(v.writer)) {
        const std::string e = v.writer.get();
        v.writeError = e;
    }
    if (v.metaDirty && v.tasks.empty()) {
        v.metaDirty = false;
        val::Project copy = P;
        v.tasks.push_back([copy]() {
            std::string e;
            val::saveProjectMeta(copy, e);
            glfwPostEmptyEvent();
            return e;
        });
    }
    if (!v.writer.valid() && !v.tasks.empty()) {
        v.writer = std::async(std::launch::async, v.tasks.front());
        v.tasks.pop_front();
    }

    // Blind: the maps assessed and the strata are not drawn while interpreting.
    if (v.blind && lab_.project) {
        std::set<std::string> files{P.strataPath};
        for (const val::AssessedMap& m : P.maps) files.insert(m.rule.path);
        for (const val::MapRule& r : P.spec.maps) files.insert(r.path);
        auto hide = [&](const std::string& f) {
            if (std::find(v.hidden.begin(), v.hidden.end(), f) == v.hidden.end()) v.hidden.push_back(f);
            mapDirty_ = true;
        };
        for (ResultLayer& r : results_)
            if (r.visible && files.count(r.path)) {
                r.visible = false;
                hide(r.path);
            }
        for (int i = 0; i < int(layers_.size()); ++i) {
            SeriesLayer& L = layers_[i];
            if (i == active_ || !L.visible || L.inputs.size() != 1) continue;
            if (files.count(L.inputs[0])) {
                L.visible = false;
                hide(L.inputs[0]);
            }
        }
    }

    // The points' colours: their state, their stratum, or right / wrong for the selected map.
    if (VectorLayer* V = vectorById(v.vectorId)) {
        const int by = v.blind && lab_.project ? 0 : v.colorBy;
        const uint64_t key = (v.version * 31 + uint64_t(by)) * 31 + uint64_t(v.selected) + (lab_.as.size() << 20);
        if (key != v.colorsFor || V->colors.size() != P.points.size()) {
            v.colorsFor = key;
            const ImU32 base = ImGui::ColorConvertFloat4ToU32(V->color);
            V->colors.assign(P.points.size(), base);
            const Validation::MapData* d = v.selected < int(v.data.size()) ? v.data[v.selected].get() : nullptr;
            for (size_t i = 0; i < P.points.size(); ++i) {
                ImU32& c = V->colors[i];
                if (by == 1) {
                    const int s = P.strata.indexOfCode(P.points[i].stratum);
                    c = ImGui::ColorConvertFloat4ToU32(ImPlot::GetColormapColor(std::max(0, s), ImPlotColormap_Deep));
                    continue;
                }
                if (by == 2 && d && d->version == v.version && i < d->prep.in.ref.size()) {
                    const int m = d->prep.in.map[i], r = d->prep.in.ref[i];
                    c = r < 0 || m < 0 ? kNoRef : m == r ? kRight : kWrong;
                    continue;
                }
                bool conflict = false;
                const val::Label* r = val::resolve(P.labels, i, conflict);
                const auto mine = P.labels.find(lab_.as.empty() ? interpreterName() : lab_.as);
                const bool byMe = mine != P.labels.end() && i < mine->second.size() && !mine->second[i].label.empty();
                const auto adj = P.labels.find(val::kAdjudicated);
                const bool settled = adj != P.labels.end() && i < adj->second.size() && !adj->second[i].label.empty();
                if (conflict) c = kConflict;
                else if (settled) c = kSettled;
                else if (byMe) c = kMine;
                else if (r) c = kOthers;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Marks: the pixel of the point being interpreted
// ---------------------------------------------------------------------------

void App::drawValidationMarks(ImDrawList* dl, ImVec2 origin) {
    if (!val_ || !val_->project || !lab_.project || !s_) return;
    Validation& v = *val_;
    const val::Project& P = *v.project;
    if (lab_.current < 0 || lab_.current >= int(P.points.size())) return;
    const VectorLayer* V = vectorById(v.vectorId);
    if (!V) return;
    const uint64_t key = vectorPlaceKey();
    if (!v.placer || v.placerKey != key || v.placerVector != v.vectorId) {
        std::string why;
        const CubeInfo& info = *s_->info;
        v.placer = VectorPlacer::create(*V->data, info.crsWkt, info.hasGeoTransform, info.geoTransform, why);
        v.placerKey = key;
        v.placerVector = v.vectorId;
    }
    if (!v.placer) return;
    const val::Point& q = P.points[lab_.current];
    const auto& gt = P.strata.grid.gt;
    ImVec2 c[4];
    const int dx[4] = {0, 1, 1, 0}, dy[4] = {0, 0, 1, 1};
    for (int k = 0; k < 4; ++k) {
        double x = gt[0] + (q.col + dx[k]) * gt[1] + (q.row + dy[k]) * gt[2];
        double y = gt[3] + (q.col + dx[k]) * gt[4] + (q.row + dy[k]) * gt[5];
        if (!v.placer->toPixel(x, y)) return;
        c[k] = origin + ImVec2(float(offset_.x + x * scale_), float(offset_.y + y * scale_));
    }
    // Too small to see: a ring around the point instead.
    const float size = std::max(std::fabs(c[2].x - c[0].x), std::fabs(c[2].y - c[0].y));
    if (size < 6) {
        const ImVec2 m = (c[0] + c[2]) * 0.5f;
        dl->AddCircle(m, 9, IM_COL32(0, 0, 0, 200), 0, 4.0f);
        dl->AddCircle(m, 9, IM_COL32(255, 255, 255, 255), 0, 2.0f);
        return;
    }
    dl->AddQuad(c[0], c[1], c[2], c[3], IM_COL32(0, 0, 0, 200), 4.0f);
    dl->AddQuad(c[0], c[1], c[2], c[3], IM_COL32(255, 255, 0, 255), 2.0f);
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

void App::uiValidationMenu() {
    PROFILE_ZONE();
    const bool open = ImGui::BeginMenu("Validation");
    shots_.menus["Validation"] = ImVec4(ImGui::GetItemRectMin().x, ImGui::GetItemRectMin().y, ImGui::GetItemRectMax().x,
                                        ImGui::GetItemRectMax().y);
    if (!open) return;
    if (!val_) val_ = std::make_shared<Validation>();
    Validation& v = *val_;
    if (ImGui::MenuItem("New sampling design...")) {
        showValidation_ = true;
        v.selectTab = 0;
    }
    ImGui::SetItemTooltip("Strata of a map (its classes, intervals of its values, its changes or the agreement\n"
                          "of several maps), the sample size and its allocation, then a stratified random\n"
                          "sample drawn into a project (Olofsson et al. 2014)");
    if (ImGui::MenuItem("Open project...")) {
        const auto files = platform::openFilesDialog();
        if (!files.empty()) openValidationProject(files.front());
    }
    if (ImGui::MenuItem("Close project", nullptr, false, validationProjectOpen())) closeValidationProject();
    ImGui::Separator();
    ImGui::MenuItem("Validation panel", nullptr, &showValidation_);
    if (ImGui::MenuItem("Interpret the sample", nullptr, false, validationProjectOpen() && s_)) {
        startInterpretation(v.vectorId);
        showInterp_ = true;
    }
    ImGui::MenuItem("Chips", nullptr, &showChips_);
    ImGui::SetItemTooltip("The feature being interpreted at every date, around it");
    ImGui::Separator();
    if (ImGui::MenuItem("Accuracy and areas", nullptr, false, validationProjectOpen())) {
        showValidation_ = true;
        v.selectTab = 1;
    }
    if (ImGui::MenuItem("Export report...", nullptr, false, validationProjectOpen())) {
        const std::string dir = platform::openFolderDialog();
        if (!dir.empty()) exportValidationReport(dir);
    }
    if (ImGui::MenuItem("Copy Python (zeit.accuracy)", nullptr, false, validationProjectOpen() && !v.project->maps.empty())) {
        const int k = std::clamp(v.selected, 0, int(v.project->maps.size()) - 1);
        ImGui::SetClipboardText(val::pythonSnippet(*v.project, v.project->maps[k], v.confidence).c_str());
    }
    ImGui::EndMenu();
}

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

void App::uiValidation() {
    PROFILE_ZONE();
    if (!showValidation_) return;
    if (!val_) val_ = std::make_shared<Validation>();
    Validation& v = *val_;
    if (const ImGuiWindow* st = ImGui::FindWindowByName("Statistics"); st && st->DockId)
        ImGui::SetNextWindowDockID(st->DockId, ImGuiCond_FirstUseEver);
    ImGui::SetNextWindowSize(ImVec2(620, 640), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Validation", &showValidation_)) {
        ImGui::End();
        return;
    }
    if (v.project) {
        const val::Project& P = *v.project;
        int labelled = 0, conflicts = 0;
        for (size_t i = 0; i < P.points.size(); ++i) {
            bool c = false;
            labelled += val::resolve(P.labels, i, c) != nullptr;
            conflicts += c;
        }
        ImGui::TextColored(theme::accent(), "%s", fs::u8path(P.path).filename().u8string().c_str());
        ImGui::SetItemTooltip("%s", P.path.c_str());
        ImGui::SameLine();
        ImGui::TextDisabled("%d of %zu points with a reference%s", labelled, P.points.size(),
                           conflicts ? (", " + std::to_string(conflicts) + " disagreement(s)").c_str() : "");
        ImGui::SameLine();
        bool blind = v.blind;
        if (ImGui::Checkbox("Blind", &blind)) setValidationBlind(blind);
        ImGui::SetItemTooltip("While interpreting, the maps assessed and the strata are not drawn and the points\n"
                              "are coloured by their state only: the reference is not swayed by the maps");
        if (!v.writeError.empty()) ImGui::TextColored(theme::error(), "Not saved: %s", v.writeError.c_str());
    }
    if (!v.message.empty()) {
        ImGui::TextWrapped("%s", v.message.c_str());
        ImGui::SameLine();
        if (ImGui::SmallButton("OK##msg")) v.message.clear();
    }
    if (ImGui::BeginTabBar("##validation")) {
        auto tab = [&](const char* name, int i) {
            const ImGuiTabItemFlags f = v.selectTab == i ? ImGuiTabItemFlags_SetSelected : 0;
            const bool open = ImGui::BeginTabItem(name, nullptr, f);
            if (v.selectTab == i) v.selectTab = -1;
            return open;
        };
        if (tab("Design", 0)) {
            uiValidationDesign();
            ImGui::EndTabItem();
        }
        if (tab("Accuracy", 1)) {
            uiValidationAccuracy();
            ImGui::EndTabItem();
        }
        if (tab("Maps", 2)) {
            uiValidationMaps();
            ImGui::EndTabItem();
        }
        if (tab("Interpreters", 3)) {
            uiValidationInterpreters();
            ImGui::EndTabItem();
        }
        if (tab("Report", 4)) {
            uiValidationReport();
            ImGui::EndTabItem();
        }
        ImGui::EndTabBar();
    }
    ImGui::End();
}

// ---------------------------------------------------------------------------
// Design
// ---------------------------------------------------------------------------

val::StrataSpec App::validationSpec(std::string& limitWhy) {
    Validation& v = *val_;
    val::StrataSpec spec;
    spec.maps = v.maps;
    spec.buffer = v.buffer;
    if (v.maps.empty() || !v.limit) return spec;
    val::Grid g;
    std::string e;
    if (!val::readGrid(v.maps[0].path, g, e) || !g.hasGt) {
        limitWhy = e.empty() ? "the map has no geotransform" : e;
        return spec;
    }
    if (v.limit == 2) {
        if (const VectorLayer* W = vectorById(v.limitVector)) {
            std::string why;
            if (auto pl = VectorPlacer::create(*W->data, g.wkt, true, g.gt, why)) {
                for (const VectorFeature& f : W->data->features)
                    if (f.kind == VectorFeature::Area)
                        for (const Ring& r : pl->placed(f.parts)) spec.limit.push_back(r);
            } else {
                limitWhy = why;
            }
        }
    } else if (v.limit == 1 && s_ && roiRect_) {
        // The ROI (the active layer's pixels) on the map's grid, through their CRS.
        const CubeInfo& info = *s_->info;
        VectorData tmp;
        tmp.crsWkt = info.crsWkt;
        Polygon poly = roiPoly_;
        if (poly.empty())
            poly = {{double(roiRectXY_[0]), double(roiRectXY_[1]), double(roiRectXY_[2]), double(roiRectXY_[1]),
                     double(roiRectXY_[2]), double(roiRectXY_[3]), double(roiRectXY_[0]), double(roiRectXY_[3])}};
        poly = densifyPolygon(poly, 8);
        transformPolygon(poly, [&](double& x, double& y) { return info.pixelToGeo(x, y, x, y); });
        std::string why;
        if (auto pl = VectorPlacer::create(tmp, g.wkt, true, g.gt, why)) spec.limit = pl->placed(poly);
        else limitWhy = why;
    }
    return spec;
}

void App::validationComputeStrata(const val::StrataSpec& spec) {
    Validation& v = *val_;
    v.strataProg = std::make_shared<val::Progress>();
    v.strataProg->threads = settings_.processingThreads();
    const fs::path dir = fs::u8path(platform::appDataDir()) / "validation";
    std::error_code ec;
    fs::create_directories(dir, ec);
    const std::string out = (dir / ("strata_preview_" + std::to_string(++v.previewCount) + ".tif")).u8string();
    v.designError.clear();
    v.strataJob = std::async(std::launch::async, [spec, out, prog = v.strataProg]() {
        Validation::StrataJob j;
        j.path = out;
        val::computeStrata(spec, out, j.s, prog.get(), j.error);
        glfwPostEmptyEvent();
        return j;
    });
}

void App::validationDraw(const std::string& path, const val::StrataSpec& spec) {
    Validation& v = *val_;
    const std::vector<val::Stratum>& S = v.strata.strata;
    val::Project p;
    p.path = fs::absolute(fs::u8path(path)).lexically_normal().u8string();
    p.strataPath = (fs::u8path(p.path).parent_path() / (fs::u8path(p.path).stem().u8string() + "_strata.tif")).u8string();
    p.id = val::newProjectId();
    p.spec = spec;
    p.strata = v.strata;
    p.seed = uint64_t(std::max(0, v.seed));
    p.design = {{"alloc", std::string(val::allocName(val::Alloc(v.alloc))) +
                              (v.alloc != int(val::Alloc::Manual) && v.minPer ? ", at least " + std::to_string(v.minPer) + " per stratum" : "")},
                {"target_se", v.targetSe}, {"min_per_stratum", v.minPer}, {"confidence", v.confidence},
                {"created", val::nowIso()}, {"by", interpreterName()}};
    // The label set: a class map's classes, else land-change labels.
    if (spec.maps.size() == 1 && spec.maps[0].kind == val::MapKind::Classes) {
        for (const val::Stratum& t : S)
            if (!t.buffer && t.cls != val::kNone) p.labelSet.push_back(spec.maps[0].className(t.cls));
        p.labelSet.push_back("Can't tell");
    } else {
        p.labelSet = kEventLabels;
    }
    for (const val::MapRule& r : spec.maps) {
        val::AssessedMap m;
        m.rule = r;
        std::vector<std::string> names;
        if (r.kind == val::MapKind::Classes)
            for (const val::Stratum& t : S)
                if (t.cls != val::kNone) names.push_back(r.className(t.cls));
        m.mapping = val::defaultMapping(r, p.labelSet, names);
        p.maps.push_back(m);
    }
    std::vector<int> n;
    for (const val::Stratum& t : S) n.push_back(t.n);
    v.drawJob = std::async(std::launch::async, [p = std::move(p), n, preview = v.strataPath]() mutable {
        Validation::DrawJob j;
        std::error_code ec;
        fs::copy_file(fs::u8path(preview), fs::u8path(p.strataPath), fs::copy_options::overwrite_existing, ec);
        if (ec) j.error = "cannot copy the strata raster to " + p.strataPath + ": " + ec.message();
        std::string e;
        if (j.error.empty() && !val::drawSample(p.strata, p.strataPath, n, p.seed, {}, p.points, e)) j.error = e;
        p.strata.rowCounts.clear();
        if (j.error.empty() && !val::createProject(p, e)) j.error = e;
        j.p = std::move(p);
        glfwPostEmptyEvent();
        return j;
    });
}

int App::validationMerge(const std::string& path, std::string& msg) {
    Validation& v = *val_;
    val::Project& P = *v.project;
    std::map<std::string, std::vector<val::Label>> other;
    std::string id, err;
    if (!val::readLabels(path, P.points.size(), id, other, err)) {
        msg = err;
        return -1;
    }
    if (id != P.id) {
        msg = "That file is not a copy of this project (another sample).";
        return -1;
    }
    int taken = 0;
    for (auto& [w, l] : other) {
        std::vector<val::Label>& mine = P.labels[w];
        mine.resize(P.points.size());
        std::vector<std::pair<int, val::Label>> rows;
        for (size_t i = 0; i < l.size(); ++i) {
            if (l[i].empty()) continue;
            if (mine[i].empty() || l[i].updated > mine[i].updated) {
                mine[i] = l[i];
                rows.push_back({int(i), l[i]});
            }
        }
        if (rows.empty()) continue;
        taken += int(rows.size());
        std::vector<val::FinalRef> finals;
        for (const auto& [i, x] : rows) {
            val::FinalRef f;
            f.point = i;
            bool c = false;
            if (const val::Label* r = val::resolve(P.labels, size_t(i), c)) f.ref = *r;
            f.conflict = c;
            for (const auto& [ww, vv] : P.labels)
                f.interpreters += ww != val::kAdjudicated && size_t(i) < vv.size() && !vv[i].label.empty();
            finals.push_back(f);
        }
        v.tasks.push_back([path = P.path, w = w, rows, finals]() {
            std::string e;
            val::saveLabels(path, w, rows, finals, e);
            glfwPostEmptyEvent();
            return e;
        });
    }
    if (lab_.project) validationEntries();
    ++v.version;
    msg = std::to_string(taken) + " label(s) merged (the newer one kept where both labelled a point).";
    return taken;
}


void App::uiValidationDesign() {
    PROFILE_ZONE();
    Validation& v = *val_;
    const bool busy = v.strataJob.valid() || v.drawJob.valid();

    // The open project: its strata, and more points.
    if (v.project) {
        val::Project& P = *v.project;
        if (ImGui::CollapsingHeader("The project's sample", ImGuiTreeNodeFlags_DefaultOpen)) {
            std::vector<int> labelled(P.strata.strata.size(), 0), total(P.strata.strata.size(), 0);
            for (size_t i = 0; i < P.points.size(); ++i) {
                const int s = P.strata.indexOfCode(P.points[i].stratum);
                if (s < 0) continue;
                ++total[s];
                bool c = false;
                labelled[s] += val::resolve(P.labels, i, c) != nullptr;
            }
            if (ImGui::BeginTable("##pstrata", 5, ImGuiTableFlags_RowBg | ImGuiTableFlags_BordersInnerV | ImGuiTableFlags_SizingFixedFit)) {
                ImGui::TableSetupColumn("stratum", ImGuiTableColumnFlags_WidthStretch);
                ImGui::TableSetupColumn("area");
                ImGui::TableSetupColumn("weight");
                ImGui::TableSetupColumn("points");
                ImGui::TableSetupColumn("labelled");
                ImGui::TableHeadersRow();
                for (size_t s = 0; s < P.strata.strata.size(); ++s) {
                    const val::Stratum& t = P.strata.strata[s];
                    ImGui::TableNextRow();
                    ImGui::TableNextColumn();
                    ImGui::TextUnformatted(v.blind && lab_.project ? ("stratum " + std::to_string(s + 1)).c_str() : t.name.c_str());
                    ImGui::TableNextColumn();
                    ImGui::Text("%s %s", fmtArea(t.area).c_str(), P.strata.unit.c_str());
                    ImGui::TableNextColumn();
                    ImGui::Text("%.4f", t.weight);
                    ImGui::TableNextColumn();
                    ImGui::Text("%d", total[s]);
                    ImGui::TableNextColumn();
                    ImGui::Text("%d", labelled[s]);
                }
                ImGui::EndTable();
            }
            ImGui::TextDisabled("Seed %llu, %s", (unsigned long long)P.seed, P.design.value("alloc", std::string("")).c_str());
            // More points in a stratum: drawn at random among its pixels not yet sampled (still a stratified random sample).
            ImGui::SetNextItemWidth(220);
            std::vector<std::string> names;
            for (size_t s = 0; s < P.strata.strata.size(); ++s)
                names.push_back(v.blind && lab_.project ? "stratum " + std::to_string(s + 1) : P.strata.strata[s].name);
            v.moreStratum = std::clamp(v.moreStratum, 0, std::max(0, int(names.size()) - 1));
            if (ImGui::BeginCombo("##morestratum", names.empty() ? "" : names[v.moreStratum].c_str())) {
                for (int s = 0; s < int(names.size()); ++s)
                    if (ImGui::Selectable(names[s].c_str(), s == v.moreStratum)) v.moreStratum = s;
                ImGui::EndCombo();
            }
            ImGui::SameLine();
            ImGui::SetNextItemWidth(90);
            ImGui::InputInt("##moren", &v.moreN);
            v.moreN = std::clamp(v.moreN, 1, 100000);
            ImGui::SameLine();
            ImGui::BeginDisabled(busy || names.empty());
            if (ImGui::Button("Add points")) {
                std::vector<int> n(P.strata.strata.size(), 0);
                n[v.moreStratum] = v.moreN;
                val::Project copy = P;
                copy.labels.clear();
                v.drawJob = std::async(std::launch::async, [copy, n]() mutable {
                    Validation::DrawJob j;
                    j.more = true;
                    std::string e;
                    if (!val::countStrata(copy.strataPath, copy.strata, nullptr, e) ||
                        !val::drawSample(copy.strata, copy.strataPath, n, copy.seed, copy.points, j.added, e)) {
                        j.error = e;
                    } else {
                        for (const val::Point& q : j.added) {
                            const int s = copy.strata.indexOfCode(q.stratum);
                            if (s >= 0) ++copy.strata.strata[s].n;
                        }
                        if (!val::appendPoints(copy, j.added, e)) j.error = e;
                    }
                    copy.strata.rowCounts.clear();
                    j.p = std::move(copy);
                    glfwPostEmptyEvent();
                    return j;
                });
            }
            ImGui::EndDisabled();
            ImGui::SetItemTooltip("More points in this stratum, drawn at random among its pixels not sampled yet:\n"
                                  "the sample stays a stratified random sample (e.g. a rare class whose\n"
                                  "user's accuracy is still too uncertain)");
            ImGui::Spacing();
        }
        if (!ImGui::CollapsingHeader("A new design")) return;
    }

    ImGui::TextWrapped("Strata from one or more maps, a stratified random sample of their pixels, "
                       "interpreted point by point: accuracy and areas with confidence intervals "
                       "(Olofsson et al. 2014).");
    // --- The maps ---
    ImGui::SeparatorText("Maps to stratify");
    const auto src = Validation::sources(*this);
    int remove = -1;
    v.binsText.resize(v.maps.size());
    v.changeText.resize(v.maps.size());
    for (int k = 0; k < int(v.maps.size()); ++k) {
        val::MapRule& r = v.maps[k];
        ImGui::PushID(k);
        if (ImGui::SmallButton("x")) remove = k;
        ImGui::SameLine();
        ImGui::TextUnformatted(r.name.c_str());
        ImGui::SetItemTooltip("%s (band %d)", r.path.c_str(), r.band);
        int kind = int(r.kind);
        ImGui::SetNextItemWidth(130);
        ImGui::Indent();
        if (ImGui::Combo("##kind", &kind, "classes\0intervals\0events\0")) r.kind = val::MapKind(kind);
        ImGui::SetItemTooltip("classes: its values (a land-cover or class map)\n"
                              "intervals: ranges of its values (e.g. a magnitude)\n"
                              "events: a date of change (year of detection): > 0 change, else no change");
        if (r.kind != val::MapKind::Classes) {
            if (v.binsText[k].empty()) v.binsText[k] = val::binsText(r.bins);
            char b[256];
            std::snprintf(b, sizeof(b), "%s", v.binsText[k].c_str());
            ImGui::SameLine();
            ImGui::SetNextItemWidth(-1);
            if (ImGui::InputTextWithHint("##bins", r.kind == val::MapKind::Events ? "periods: e.g. 2000, 2010, 2021 (empty: change / no change)"
                                                                                  : "edges: e.g. -1, -0.1, 1",
                                         b, sizeof(b))) {
                v.binsText[k] = b;
                std::string e;
                const std::vector<double> bins = val::parseBins(b, e);
                if (e.empty()) r.bins = bins;
            }
        }
        // Which classes (or intervals) are change: for a buffer, for the agreement of several maps, for the expected errors.
        if (r.kind == val::MapKind::Classes) {
            if (!r.names.empty()) {
                ImGui::TextDisabled("change:");
                for (const auto& [value, name] : r.names) {
                    ImGui::SameLine();
                    bool on = r.change.count(value) > 0;
                    if (ImGui::Checkbox((name + "##ch" + std::to_string(value)).c_str(), &on)) {
                        if (on) r.change.insert(value);
                        else r.change.erase(value);
                    }
                }
            } else {
                char b[256];
                if (v.changeText[k].empty() && !r.change.empty()) {
                    for (int c : r.change) v.changeText[k] += (v.changeText[k].empty() ? "" : ", ") + std::to_string(c);
                }
                std::snprintf(b, sizeof(b), "%s", v.changeText[k].c_str());
                ImGui::SetNextItemWidth(-1);
                if (ImGui::InputTextWithHint("##change", "values that are change, e.g. 3, 15 (for a buffer or several maps)", b, sizeof(b))) {
                    v.changeText[k] = b;
                    r.change.clear();
                    std::string e;
                    for (double c : val::parseBins(std::string(b) + (std::strchr(b, ',') ? "" : ", 1e300"), e))
                        if (c < 1e299) r.change.insert(int(c));
                }
            }
        } else if (r.kind == val::MapKind::Intervals && r.bins.size() >= 2) {
            ImGui::TextDisabled("change:");
            for (int i = 1; i < int(r.bins.size()); ++i) {
                ImGui::SameLine();
                bool on = r.change.count(i) > 0;
                if (ImGui::Checkbox((r.className(i) + "##iv" + std::to_string(i)).c_str(), &on)) {
                    if (on) r.change.insert(i);
                    else r.change.erase(i);
                }
            }
        }
        ImGui::Unindent();
        ImGui::PopID();
    }
    if (remove >= 0) {
        v.maps.erase(v.maps.begin() + remove);
        v.binsText.erase(v.binsText.begin() + remove);
        v.changeText.erase(v.changeText.begin() + remove);
    }
    val::MapRule picked;
    if (Validation::pickSource(src, picked, "Add a map...")) {
        v.maps.push_back(picked);
        v.binsText.push_back(val::binsText(picked.bins));
        v.changeText.push_back("");
    }
    if (v.maps.size() > 1)
        ImGui::TextDisabled("Several maps: a stratum for each number of maps that call a pixel change (k of %d).",
                           int(v.maps.size()));

    // --- Buffer and limit ---
    ImGui::SeparatorText("Buffer and limit");
    ImGui::SetNextItemWidth(160);
    ImGui::SliderInt("buffer (pixels)", &v.buffer, 0, 10);
    ImGui::SetItemTooltip("Pixels without change within this distance of a change form a stratum of their own:\n"
                          "omission errors (change the map missed) are most likely there, and a few points\n"
                          "in it make the area of change much more precise (Olofsson et al. 2020)");
    ImGui::RadioButton("every pixel", &v.limit, 0);
    ImGui::SameLine();
    ImGui::BeginDisabled(!roiRect_);
    ImGui::RadioButton("inside the ROI", &v.limit, 1);
    ImGui::EndDisabled();
    ImGui::SameLine();
    std::vector<const VectorLayer*> polys;
    for (const VectorLayer& W : vectors_)
        if (W.data->areas > 0 && W.id != v.vectorId) polys.push_back(&W);
    ImGui::BeginDisabled(polys.empty());
    ImGui::RadioButton("inside polygons of", &v.limit, 2);
    ImGui::EndDisabled();
    if (v.limit == 2 && !polys.empty()) {
        ImGui::SameLine();
        const VectorLayer* cur = vectorById(v.limitVector);
        if (!cur) v.limitVector = polys.front()->id;
        ImGui::SetNextItemWidth(-1);
        if (ImGui::BeginCombo("##limitv", vectorById(v.limitVector)->name.c_str())) {
            for (const VectorLayer* W : polys)
                if (ImGui::Selectable(W->name.c_str(), W->id == v.limitVector)) v.limitVector = W->id;
            ImGui::EndCombo();
        }
    }
    if (v.limit == 1 && !roiRect_) v.limit = 0;
    if (v.limit == 2 && polys.empty()) v.limit = 0;

    // --- Strata ---
    // What the strata depend on (cheap: the limit is placed on the map only when they are computed).
    std::string specKey;
    {
        json m = json::array();
        for (const val::MapRule& r : v.maps) m.push_back(r.toJson());
        specKey = m.dump() + "|" + std::to_string(v.buffer) + "|" + std::to_string(v.limit) + "|" +
                  (v.limit == 2 ? std::to_string(v.limitVector) : v.limit == 1 ? std::to_string(roiGen_) : "");
    }
    const bool stale = !v.strataPath.empty() && v.strataFor != specKey;
    ImGui::SeparatorText("Strata");
    ImGui::BeginDisabled(v.maps.empty() || busy);
    if (ImGui::Button(v.strataPath.empty() ? "Compute the strata" : "Compute the strata again")) {
        std::string why;
        const val::StrataSpec spec = validationSpec(why);
        if (!why.empty()) {
            v.designError = "The limit cannot be placed on the map: " + why;
        } else {
            validationComputeStrata(spec);
            v.strataFor = specKey;
        }
    }
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("One pass over the maps at full resolution: the strata raster (shown as a layer),\n"
                          "each stratum's pixels and area");
    if (v.strataJob.valid()) {
        ImGui::SameLine();
        ImGui::ProgressBar(float(v.strataProg->done.load()), ImVec2(-60, 0));
        ImGui::SameLine();
        if (ImGui::SmallButton("Cancel")) v.strataProg->cancel = true;
    } else if (stale) {
        ImGui::SameLine();
        ImGui::TextColored(theme::warning(), "the maps or the limit changed");
    }
    if (!v.designError.empty()) ImGui::TextColored(theme::error(), "%s", v.designError.c_str());
    if (v.strataPath.empty() || v.strata.strata.empty()) return;

    // --- Sample size and allocation ---
    std::vector<val::Stratum>& S = v.strata.strata;
    ImGui::SetNextItemWidth(130);
    ImGui::SliderFloat("standard error of the overall accuracy", &v.targetSe, 0.005f, 0.05f, "%.3f");
    const int eq13 = val::sampleSize(S, v.targetSe);
    ImGui::Checkbox("n from it (eq. 13)", &v.nAuto);
    ImGui::SameLine();
    if (v.nAuto) v.n = eq13;
    ImGui::BeginDisabled(v.nAuto);
    ImGui::SetNextItemWidth(110);
    ImGui::InputInt("points", &v.n);
    ImGui::EndDisabled();
    v.n = std::max(1, v.n);
    ImGui::SetNextItemWidth(160);
    ImGui::Combo("allocation", &v.alloc, "proportional\0equal\0Neyman\0by hand\0");
    ImGui::SetItemTooltip("proportional: to the strata's areas; equal: the same in each; Neyman: to area x the\n"
                          "expected standard deviation; by hand: the n column. Rare strata (change) need\n"
                          "50-100 points to estimate their accuracy (Olofsson et al. 2014)");
    if (v.alloc != int(val::Alloc::Manual)) {
        ImGui::SameLine();
        ImGui::SetNextItemWidth(90);
        ImGui::InputInt("at least, per stratum", &v.minPer);
        v.minPer = std::max(0, v.minPer);
        const std::vector<int> n = val::allocate(S, v.n, val::Alloc(v.alloc), v.minPer);
        for (size_t s = 0; s < S.size(); ++s) S[s].n = n[s];
    }
    const val::Expected ex = val::expectedErrors(S, v.strata.totalArea(), v.confidence);
    int total = 0;
    for (const val::Stratum& t : S) total += t.n;
    if (ImGui::BeginTable("##strata", 7, ImGuiTableFlags_RowBg | ImGuiTableFlags_BordersInnerV | ImGuiTableFlags_SizingFixedFit)) {
        ImGui::TableSetupColumn("stratum", ImGuiTableColumnFlags_WidthStretch);
        ImGui::TableSetupColumn("pixels");
        ImGui::TableSetupColumn("area");
        ImGui::TableSetupColumn("weight");
        ImGui::TableSetupColumn("expected UA");
        ImGui::TableSetupColumn("n");
        ImGui::TableSetupColumn("SE of UA");
        ImGui::TableHeadersRow();
        for (size_t s = 0; s < S.size(); ++s) {
            val::Stratum& t = S[s];
            ImGui::PushID(int(s));
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(t.name.c_str());
            if (t.change) {
                ImGui::SameLine();
                ImGui::TextColored(theme::warning(), "change");
            }
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(fmtArea(double(t.pixels)).c_str());
            ImGui::TableNextColumn();
            ImGui::Text("%s %s", fmtArea(t.area).c_str(), v.strata.unit.c_str());
            ImGui::TableNextColumn();
            ImGui::Text("%.4f", t.weight);
            ImGui::TableNextColumn();
            ImGui::SetNextItemWidth(70);
            float ua = float(t.expectedUa);
            if (ImGui::DragFloat("##ua", &ua, 0.005f, 0.5f, 0.99f, "%.2f")) t.expectedUa = ua;
            ImGui::SetItemTooltip("The user's accuracy expected of this stratum: hard classes (change) 0.6-0.8,\n"
                                  "easy ones 0.9-0.95; it sets n (eq. 13) and the Neyman allocation");
            ImGui::TableNextColumn();
            ImGui::SetNextItemWidth(70);
            ImGui::BeginDisabled(v.alloc != int(val::Alloc::Manual));
            ImGui::InputInt("##n", &t.n, 0);
            ImGui::EndDisabled();
            t.n = int(std::clamp<int64_t>(t.n, 0, t.pixels));
            ImGui::TableNextColumn();
            if (s < ex.userSe.size() && std::isfinite(ex.userSe[s])) ImGui::Text("%.3f", ex.userSe[s]);
            else ImGui::TextDisabled("-");
            ImGui::PopID();
        }
        ImGui::EndTable();
    }
    ImGui::Text("%d points in all; area %s %s.", total, fmtArea(v.strata.totalArea()).c_str(), v.strata.unit.c_str());
    if (std::isfinite(ex.overallSe))
        ImGui::Text("Expected: overall accuracy SE %.4f%s", ex.overallSe,
                    ex.change ? ("; area of change " + fmtArea(ex.changeArea) + " +- " + fmtArea(ex.changeCi) + " " +
                                 v.strata.unit + " (" + std::to_string(int(std::lround(100 * v.confidence))) + "%)").c_str()
                              : "");
    else ImGui::TextDisabled("Expected errors: every stratum needs at least 2 points.");
    ImGui::SetItemTooltip("If each stratum's accuracy is as expected: the overall accuracy's standard error and\n"
                          "the interval of the area of change (a stratum's errors taken as change, or as no change)");

    // --- Draw ---
    ImGui::SetNextItemWidth(110);
    ImGui::InputInt("seed", &v.seed);
    ImGui::SetItemTooltip("The same strata, sizes and seed draw the same points");
    ImGui::SameLine();
    ImGui::BeginDisabled(busy || total <= 0 || stale);
    if (ImGui::Button("Draw the sample and create the project...")) {
        const std::string stem = fs::u8path(v.maps[0].path).stem().u8string() + "_validation";
        const std::string path = askSavePath("Validation project (GeoPackage)", stem + ".gpkg", "GeoPackage", "gpkg");
        if (!path.empty()) {
            std::string why;
            const val::StrataSpec spec = validationSpec(why);
            if (why.empty()) validationDraw(path, spec);
            else v.designError = "The limit cannot be placed on the map: " + why;
        }
    }
    ImGui::EndDisabled();
    if (v.drawJob.valid()) {
        ImGui::SameLine();
        ImGui::TextDisabled("drawing...");
    }
}

bool App::Validation::pickSource(const std::vector<Source>& src, val::MapRule& out, const char* label) {
    bool got = false;
    if (ImGui::Button(label)) ImGui::OpenPopup("##pickmap");
    if (ImGui::BeginPopup("##pickmap")) {
        if (src.empty()) ImGui::TextDisabled("No map open: run a tool, open a layer, or pick a file");
        for (const auto& s : src)
            if (ImGui::Selectable(s.label.c_str())) {
                out = s.rule;
                got = true;
            }
        ImGui::Separator();
        if (ImGui::Selectable("A raster file...")) {
            const auto files = platform::openFilesDialog();
            if (!files.empty()) {
                out = fileRule(files.front());
                got = true;
            }
        }
        ImGui::EndPopup();
    }
    return got;
}

// ---------------------------------------------------------------------------
// Accuracy
// ---------------------------------------------------------------------------

void App::uiValidationAccuracy() {
    PROFILE_ZONE();
    Validation& v = *val_;
    if (!v.project) {
        ImGui::TextWrapped("Open a validation project (Validation > Open project...) or draw a sample (Design).");
        return;
    }
    val::Project& P = *v.project;
    if (P.maps.empty()) {
        ImGui::TextDisabled("No map to assess: add one in Maps.");
        return;
    }
    v.selected = std::clamp(v.selected, 0, int(P.maps.size()) - 1);
    ImGui::SetNextItemWidth(-140);
    if (ImGui::BeginCombo("##map", P.maps[v.selected].rule.name.c_str())) {
        for (int k = 0; k < int(P.maps.size()); ++k)
            if (ImGui::Selectable((P.maps[k].rule.name + "##m" + std::to_string(k)).c_str(), k == v.selected)) v.selected = k;
        ImGui::EndCombo();
    }
    ImGui::SameLine();
    ImGui::SetNextItemWidth(-1);
    int conf = int(std::lround(v.confidence * 100));
    if (ImGui::SliderInt("##conf", &conf, 80, 99, "%d%% intervals")) {
        v.confidence = conf / 100.0f;
        ++v.version;
    }
    val::AssessedMap& M = P.maps[v.selected];
    Validation::MapData& d = *v.data[v.selected];
    if (!d.error.empty()) ImGui::TextColored(theme::error(), "%s", d.error.c_str());
    if (!d.ready()) {
        ImGui::TextDisabled("Reading the map at the points and its areas...");
        if (d.mappedJob.valid()) ImGui::ProgressBar(float(d.prog->done.load()), ImVec2(-1, 0));
        return;
    }
    if (d.version != v.version || d.tolerance != v.tolerance) {
        d.prep = val::prepare(P, M, d.values, d.mapped, v.confidence);
        d.a = val::assess(d.prep.in);
        d.change = binaryChange(M.rule, d.prep);
        if (M.rule.kind == val::MapKind::Events) {
            std::vector<int> st(d.prep.in.stratum);
            const val::Estimator e(d.prep.in.stratumArea, st, v.confidence);
            std::vector<int> my(d.prep.mapYear), ry(d.prep.refYear);
            for (size_t i = 0; i < my.size(); ++i)
                if (st[i] < 0) my[i] = ry[i] = 0;
            d.dates = val::dateAgreement(e, my, ry, v.tolerance);
        }
        d.version = v.version;
        d.tolerance = v.tolerance;
    }
    const val::Prepared& pp = d.prep;
    const val::Assessment& a = d.a;
    const int pct = int(std::lround(v.confidence * 100));

    // --- How far along ---
    ImGui::Text("%d point(s) used of %zu", a.n, P.points.size());
    ImGui::SameLine();
    ImGui::TextDisabled("(%d without a label)", pp.unlabelled);
    if (pp.conflicts) {
        ImGui::SameLine();
        ImGui::TextColored(theme::warning(), "%d disagreement(s)", pp.conflicts);
        ImGui::SameLine();
        if (ImGui::SmallButton("Adjudicate")) startAdjudication();
    }
    if (pp.unmapped) {
        std::string ls;
        for (const std::string& l : pp.unmappedLabels) ls += (ls.empty() ? "" : ", ") + l;
        ImGui::TextColored(theme::warning(), "%d point(s) with labels this map has no class for: %s (set below)", pp.unmapped, ls.c_str());
    }
    if (pp.excluded) ImGui::TextDisabled("%d point(s) left out by their label (can't tell...)", pp.excluded);
    if (pp.noDate) ImGui::TextColored(theme::warning(), "%d point(s) of change without a date (the period is unknown)", pp.noDate);
    if (pp.offMap) ImGui::TextDisabled("%d labelled point(s) where the map has no value", pp.offMap);
    if (!a.missing.empty() && a.n) {
        std::string ms;
        for (int s : a.missing) ms += (ms.empty() ? "" : ", ") + (v.blind && lab_.project ? std::to_string(s + 1) : P.strata.strata[s].name);
        ImGui::TextColored(theme::warning(), "No point yet in: %s (the estimates cover %.1f%% of the area)", ms.c_str(), 100 * a.covered);
    }

    // --- Translations ---
    if (v.section("Reference labels -> the map's classes", 0)) {
        std::vector<std::string> targets;
        if (M.rule.kind == val::MapKind::Events) targets = {val::kChange, val::kNoChange};
        else
            for (int c : val::mapClasses(M.rule, d.values, d.mapped)) targets.push_back(M.rule.className(c));
        if (ImGui::BeginTable("##tr", 2, ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingStretchSame)) {
            for (const std::string& l : P.labelSet) {
                ImGui::TableNextRow();
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(l.c_str());
                ImGui::TableNextColumn();
                const auto it = M.mapping.find(l);
                const std::string cur = it == M.mapping.end() ? "(not set)" : it->second.empty() ? "(left out)" : it->second;
                ImGui::SetNextItemWidth(-1);
                if (ImGui::BeginCombo(("##t" + l).c_str(), cur.c_str())) {
                    for (const std::string& t : targets)
                        if (ImGui::Selectable(t.c_str(), it != M.mapping.end() && it->second == t)) {
                            M.mapping[l] = t;
                            v.metaDirty = true;
                            ++v.version;
                        }
                    if (ImGui::Selectable("(left out)", it != M.mapping.end() && it->second.empty())) {
                        M.mapping[l] = "";
                        v.metaDirty = true;
                        ++v.version;
                    }
                    ImGui::EndCombo();
                }
            }
            ImGui::EndTable();
        }
        if (M.rule.kind == val::MapKind::Events)
            ImGui::TextDisabled("A point is change when any of its events is; with periods, by the date of the first.");
    }
    if (!a.n) return;

    // --- Overall ---
    ImGui::SeparatorText("Overall");
    ImGui::Text("Overall accuracy: %.3f +- %.3f (%d%%)", a.overall.est, a.overall.hi - a.overall.est, pct);
    if (d.change.defined) {
        ImGui::Text("Change: user's %s, producer's %s; area %s +- %s %s (mapped %s)", fmtPm(d.change.users).c_str(),
                    fmtPm(d.change.producers).c_str(), fmtArea(d.change.area.est).c_str(),
                    fmtArea(d.change.area.hi - d.change.area.est).c_str(), P.strata.unit.c_str(), fmtArea(d.change.mapped).c_str());
    }

    // --- Error matrix ---
    const size_t C = a.classes.size();
    ImGui::SeparatorText("Error matrix");
    ImGui::RadioButton("points", &v.matrixMode, 0);
    ImGui::SameLine();
    ImGui::RadioButton("proportions of area", &v.matrixMode, 1);
    ImGui::SameLine();
    ImGui::TextDisabled("(rows: map, columns: reference; click a cell for its points)");
    // A table that scrolls sideways is a child window: without a height it would take the rest of the panel.
    const ImGuiStyle& style = ImGui::GetStyle();
    const float matrixH = float(C + 2) * (ImGui::GetTextLineHeight() + 2 * style.CellPadding.y + 1) + style.ScrollbarSize + 2;
    if (ImGui::BeginTable("##matrix", int(C) + 2, ImGuiTableFlags_Borders | ImGuiTableFlags_SizingFixedFit | ImGuiTableFlags_ScrollX,
                          ImVec2(0, matrixH))) {
        ImGui::TableSetupColumn("map \\ ref.");
        for (const std::string& c : a.classes) ImGui::TableSetupColumn(c.c_str());
        ImGui::TableSetupColumn("total");
        ImGui::TableHeadersRow();
        std::vector<double> colTotal(C, 0);
        for (size_t i = 0; i < C; ++i) {
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(a.classes[i].c_str());
            double row = 0;
            for (size_t j = 0; j < C; ++j) {
                ImGui::TableNextColumn();
                const double val = v.matrixMode ? a.confusion[i][j] : a.counts[i][j];
                row += val;
                colTotal[j] += val;
                char t[48];
                if (v.matrixMode) std::snprintf(t, sizeof(t), "%.4f##c%zu_%zu", val, i, j);
                else std::snprintf(t, sizeof(t), "%d##c%zu_%zu", a.counts[i][j], i, j);
                if (i == j) ImGui::TableSetBgColor(ImGuiTableBgTarget_CellBg, IM_COL32(80, 200, 120, 50));
                if (ImGui::Selectable(t, false) && a.counts[i][j]) {
                    lab_.filter = a.cells[i][j];
                    lab_.filterName = "map " + a.classes[i] + ", reference " + a.classes[j];
                    if (lab_.vector != v.vectorId) startInterpretation(v.vectorId);
                    if (!lab_.filter.empty()) interpGo(lab_.filter.front());
                    showInterp_ = true;
                }
            }
            ImGui::TableNextColumn();
            if (v.matrixMode) ImGui::Text("%.4f", row);
            else ImGui::Text("%d", int(row));
        }
        ImGui::TableNextRow();
        ImGui::TableNextColumn();
        ImGui::TextDisabled("total");
        for (size_t j = 0; j < C; ++j) {
            ImGui::TableNextColumn();
            if (v.matrixMode) ImGui::TextDisabled("%.4f", colTotal[j]);
            else ImGui::TextDisabled("%d", int(colTotal[j]));
        }
        ImGui::TableNextColumn();
        ImGui::EndTable();
    }

    // --- Accuracies and areas ---
    ImGui::SeparatorText("Accuracy and areas");
    if (ImGui::BeginTable("##acc", 5, ImGuiTableFlags_RowBg | ImGuiTableFlags_BordersInnerV | ImGuiTableFlags_SizingFixedFit)) {
        ImGui::TableSetupColumn("class", ImGuiTableColumnFlags_WidthStretch);
        ImGui::TableSetupColumn("user's");
        ImGui::TableSetupColumn("producer's");
        ImGui::TableSetupColumn(("mapped (" + P.strata.unit + ")").c_str());
        ImGui::TableSetupColumn(("adjusted (" + P.strata.unit + ")").c_str());
        ImGui::TableHeadersRow();
        for (size_t c = 0; c < C; ++c) {
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(a.classes[c].c_str());
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(fmtPm(a.users[c]).c_str());
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(fmtPm(a.producers[c]).c_str());
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(fmtArea(a.area[c].mapped).c_str());
            ImGui::TableNextColumn();
            ImGui::Text("%s +- %s", fmtArea(a.area[c].est).c_str(), fmtArea(a.area[c].ci).c_str());
        }
        ImGui::EndTable();
    }
    if (ImPlot::BeginPlot("##areas", ImVec2(-1, 200), ImPlotFlags_NoMouseText)) {
        std::vector<double> xs, mapped, est, err, pos;
        std::vector<const char*> labels;
        for (size_t c = 0; c < C; ++c) {
            pos.push_back(double(c));
            labels.push_back(a.classes[c].c_str());
            xs.push_back(double(c) - 0.18);
            mapped.push_back(a.area[c].mapped);
            est.push_back(a.area[c].est);
            err.push_back(a.area[c].ci);
        }
        ImPlot::SetupAxes(nullptr, P.strata.unit.c_str(), ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit);
        ImPlot::SetupAxisTicks(ImAxis_X1, pos.data(), int(C), labels.data());
        ImPlot::SetupLegend(ImPlotLocation_NorthEast);
        ImPlot::PlotBars("mapped", xs.data(), mapped.data(), int(C), 0.34);
        for (double& x : xs) x += 0.36;
        ImPlot::PlotBars("adjusted", xs.data(), est.data(), int(C), 0.34);
        ImPlot::PlotErrorBars("adjusted", xs.data(), est.data(), err.data(), int(C));
        ImPlot::EndPlot();
    }

    // --- The estimates as points came ---
    if (v.section("As the labels came (convergence)", 1)) {
        v.convClass = std::clamp(v.convClass, -1, int(C) - 1);
        ImGui::SetNextItemWidth(220);
        if (ImGui::BeginCombo("##convc", v.convClass < 0 ? "overall accuracy" : ("area of " + a.classes[v.convClass]).c_str())) {
            if (ImGui::Selectable("overall accuracy", v.convClass < 0)) v.convClass = -1;
            for (int c = 0; c < int(C); ++c)
                if (ImGui::Selectable(("area of " + a.classes[c]).c_str(), c == v.convClass)) v.convClass = c;
            ImGui::EndCombo();
        }
        const uint64_t key = ((v.version * 131 + uint64_t(v.selected)) * 131 + uint64_t(v.convClass + 1)) * 131 + uint64_t(conf);
        if (v.conv.key != key) {
            v.conv = Validation::Conv{};
            v.conv.key = key;
            // The labelled points in the order of their labels' times, then the estimates at ~40 steps.
            std::vector<std::pair<std::string, int>> order;
            for (size_t i = 0; i < P.points.size(); ++i) {
                bool c = false;
                if (const val::Label* L = val::resolve(P.labels, i, c)) order.push_back({L->updated, int(i)});
            }
            std::stable_sort(order.begin(), order.end());
            const int steps = std::min<int>(40, int(order.size()));
            std::vector<char> use(P.points.size(), 0);
            size_t next = 0;
            for (int s = 1; s <= steps; ++s) {
                const size_t upto = order.size() * size_t(s) / size_t(steps);
                for (; next < upto; ++next) use[order[next].second] = 1;
                const val::Prepared q = val::prepare(P, M, d.values, d.mapped, v.confidence, use);
                const val::Assessment b = val::assess(q.in);
                if (b.n < 2) continue;
                v.conv.n.push_back(double(upto));
                if (v.convClass < 0) {
                    v.conv.area.push_back(b.overall.est);
                    v.conv.lo.push_back(b.overall.lo);
                    v.conv.hi.push_back(b.overall.hi);
                } else {
                    const auto it = std::find(b.classes.begin(), b.classes.end(), a.classes[v.convClass]);
                    const size_t c = size_t(it - b.classes.begin());
                    v.conv.area.push_back(c < b.area.size() ? b.area[c].est : NAN);
                    v.conv.lo.push_back(c < b.area.size() ? b.area[c].lo : NAN);
                    v.conv.hi.push_back(c < b.area.size() ? b.area[c].hi : NAN);
                }
            }
        }
        if (ImPlot::BeginPlot("##conv", ImVec2(-1, 180), ImPlotFlags_NoLegend)) {
            ImPlot::SetupAxes("labelled points", v.convClass < 0 ? "overall accuracy" : P.strata.unit.c_str(),
                              ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit);
            const int k = int(v.conv.n.size());
            const ImVec4 col = theme::onPlot(ImVec4(0.30f, 0.60f, 1.0f, 1));
            ImPlotSpec band;
            band.FillColor = col;
            band.FillAlpha = 0.25f;
            ImPlot::PlotShaded("interval", v.conv.n.data(), v.conv.lo.data(), v.conv.hi.data(), k, band);
            ImPlotSpec line;
            line.LineColor = col;
            line.LineWeight = 1.5f;
            ImPlot::PlotLine("estimate", v.conv.n.data(), v.conv.area.data(), k, line);
            ImPlot::EndPlot();
        }
        ImGui::TextDisabled("The interval narrows as points are labelled: label until it is narrow enough.");
    }

    // --- Dates ---
    if (M.rule.kind == val::MapKind::Events && v.section("Dates of change", 2)) {
        ImGui::SetNextItemWidth(160);
        ImGui::SliderFloat("tolerance (years)", &v.tolerance, 0, 5, "%.0f");
        v.tolerance = std::round(v.tolerance);
        const val::DateAgreement& g = d.dates;
        if (!g.n) {
            ImGui::TextDisabled("No point that both the map and the reference call change, with dates.");
        } else {
            ImGui::Text("Within +-%.0f year(s): %s; mean difference %.2f years (%d points)", g.tolerance, fmtPm(g.within).c_str(),
                        g.meanDiff.est, g.n);
            std::vector<double> xs, ys;
            for (const auto& [k, n] : g.diffs) {
                xs.push_back(k);
                ys.push_back(n);
            }
            if (ImPlot::BeginPlot("##ddiff", ImVec2(-1, 150), ImPlotFlags_NoLegend)) {
                ImPlot::SetupAxes("map year - reference year", "points", ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit);
                ImPlot::PlotBars("points", xs.data(), ys.data(), int(xs.size()), 0.8);
                ImPlot::EndPlot();
            }
        }
    }
    // --- Area of change by year (the reference's dates) ---
    bool anyChange = false;
    for (const auto& [l, t] : M.mapping) anyChange |= t == val::kChange;
    if (M.rule.kind == val::MapKind::Events && anyChange && v.section("Area of change by year", 3)) {
        const uint64_t key = (v.version * 131 + uint64_t(v.selected)) * 131 + uint64_t(conf);
        if (v.byYear.key != key) {
            v.byYear.key = key;
            v.byYear.rows = val::changeByYear(P, M, v.confidence);
        }
        if (v.byYear.rows.empty()) {
            ImGui::TextDisabled("No dated change in the reference yet.");
        } else {
            std::vector<double> xs, ys, err;
            for (const auto& [y, ar] : v.byYear.rows) {
                xs.push_back(y);
                ys.push_back(ar.est);
                err.push_back(ar.ci);
            }
            if (ImPlot::BeginPlot("##byyear", ImVec2(-1, 190), ImPlotFlags_NoLegend)) {
                ImPlot::SetupAxes("year", P.strata.unit.c_str(), ImPlotAxisFlags_AutoFit, ImPlotAxisFlags_AutoFit);
                ImPlot::SetupAxisFormat(ImAxis_X1, "%.0f");
                ImPlot::PlotBars("area", xs.data(), ys.data(), int(xs.size()), 0.7);
                ImPlot::PlotErrorBars("area", xs.data(), ys.data(), err.data(), int(xs.size()));
                ImPlot::EndPlot();
            }
            ImGui::TextDisabled("From the reference alone: the area whose first change (as this map's translation\n"
                                "reads the labels) falls in each year, +- its %d%% interval.", pct);
        }
    }
}

// ---------------------------------------------------------------------------
// Maps: the same sample assesses several maps
// ---------------------------------------------------------------------------

void App::uiValidationMaps() {
    PROFILE_ZONE();
    Validation& v = *val_;
    if (!v.project) {
        ImGui::TextWrapped("Open a project first: every map added here is assessed with its sample and its labels.");
        return;
    }
    val::Project& P = *v.project;
    ImGui::TextWrapped("One sample, many maps: each map is read at the points and translated from the same "
                       "reference labels; the estimators weigh the points by the project's strata (Stehman 2014), "
                       "so maps other than the one stratified are assessed without bias.");
    int remove = -1;
    // Its own height (scrolling sideways, it is a child window that would take the rest of the panel).
    const float rowH = ImGui::GetFrameHeight() + 2 * ImGui::GetStyle().CellPadding.y;
    const ImVec2 mapsSize(0, rowH * float(P.maps.size() + 1) + ImGui::GetStyle().ScrollbarSize + 2);
    if (ImGui::BeginTable("##maps", 8, ImGuiTableFlags_RowBg | ImGuiTableFlags_BordersInnerV | ImGuiTableFlags_SizingFixedFit | ImGuiTableFlags_ScrollX,
                          mapsSize)) {
        // As wide as the longest name (a stretched column is squeezed in a table that scrolls sideways).
        float nameW = ImGui::CalcTextSize("map").x;
        for (const val::AssessedMap& m : P.maps) nameW = std::max(nameW, ImGui::CalcTextSize(m.rule.name.c_str()).x);
        ImGui::TableSetupColumn("map", ImGuiTableColumnFlags_WidthFixed, nameW);
        ImGui::TableSetupColumn("points");
        ImGui::TableSetupColumn("overall");
        ImGui::TableSetupColumn("change: user's");
        ImGui::TableSetupColumn("producer's");
        ImGui::TableSetupColumn(("mapped (" + P.strata.unit + ")").c_str());
        ImGui::TableSetupColumn(("adjusted (" + P.strata.unit + ")").c_str());
        ImGui::TableSetupColumn("");
        ImGui::TableHeadersRow();
        for (int k = 0; k < int(P.maps.size()); ++k) {
            Validation::MapData& d = *v.data[k];
            ImGui::PushID(k);
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            if (ImGui::Selectable(P.maps[k].rule.name.c_str(), k == v.selected, ImGuiSelectableFlags_AllowOverlap)) {
                v.selected = k;
                v.selectTab = 1;
            }
            ImGui::SetItemTooltip("%s (%s)", P.maps[k].rule.path.c_str(), val::kindName(P.maps[k].rule.kind));
            if (!d.ready()) {
                ImGui::TableNextColumn();
                ImGui::TextDisabled(d.error.empty() ? "reading..." : "error");
                if (!d.error.empty()) ImGui::SetItemTooltip("%s", d.error.c_str());
                for (int c = 0; c < 5; ++c) ImGui::TableNextColumn();
            } else {
                if (d.version != v.version) {
                    d.prep = val::prepare(P, P.maps[k], d.values, d.mapped, v.confidence);
                    d.a = val::assess(d.prep.in);
                    d.change = binaryChange(P.maps[k].rule, d.prep);
                    d.version = v.version;
                    d.tolerance = -1;
                }
                ImGui::TableNextColumn();
                ImGui::Text("%d", d.a.n);
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(fmtPm(d.a.overall).c_str());
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(d.change.defined ? fmtPm(d.change.users).c_str() : "-");
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(d.change.defined ? fmtPm(d.change.producers).c_str() : "-");
                ImGui::TableNextColumn();
                ImGui::TextUnformatted(d.change.defined ? fmtArea(d.change.mapped).c_str() : "-");
                ImGui::TableNextColumn();
                if (d.change.defined)
                    ImGui::Text("%s +- %s", fmtArea(d.change.area.est).c_str(), fmtArea(d.change.area.hi - d.change.area.est).c_str());
                else ImGui::TextUnformatted("-");
            }
            ImGui::TableNextColumn();
            if (ImGui::SmallButton("x")) remove = k;
            ImGui::PopID();
        }
        ImGui::EndTable();
    }
    if (remove >= 0 && P.maps.size() > 1) {
        P.maps.erase(P.maps.begin() + remove);
        v.data.erase(v.data.begin() + remove);
        v.selected = 0;
        v.metaDirty = true;
        ++v.version;
    }
    ImGui::TextDisabled("Change: the classes each map calls change (a map of events: any date).");
    val::MapRule picked;
    if (Validation::pickSource(Validation::sources(*this), picked, "Assess another map...")) {
        val::AssessedMap m;
        m.rule = picked;
        P.maps.push_back(m);
        v.addMapData(*this, P.maps.size() - 1);
        v.metaDirty = true;
        ++v.version;
    }
    ImGui::SetItemTooltip("A Zeit result (LandTrendr, CCDC, BFAST...), a layer at its date or a raster file:\n"
                          "its classes are guessed (a class map, intervals, or dates of change); the\n"
                          "translation of the labels is set in Accuracy");
    // The rule of the selected map (e.g. periods of a map of events).
    if (v.selected < int(P.maps.size())) {
        val::MapRule& r = P.maps[v.selected].rule;
        ImGui::SeparatorText(("How " + r.name + " is read").c_str());
        int kind = int(r.kind);
        ImGui::SetNextItemWidth(130);
        bool changed = ImGui::Combo("##kindm", &kind, "classes\0intervals\0events\0");
        if (changed) r.kind = val::MapKind(kind);
        if (r.kind != val::MapKind::Classes) {
            static char bins[256];
            static int binsFor = -1;
            if (binsFor != v.selected) {
                std::snprintf(bins, sizeof(bins), "%s", val::binsText(r.bins).c_str());
                binsFor = v.selected;
            }
            ImGui::SameLine();
            ImGui::SetNextItemWidth(-60);
            ImGui::InputTextWithHint("##binsm", r.kind == val::MapKind::Events ? "periods (empty: change / no change)" : "edges", bins, sizeof(bins));
            ImGui::SameLine();
            if (ImGui::Button("Apply")) {
                std::string e;
                const std::vector<double> b = val::parseBins(bins, e);
                if (e.empty()) {
                    r.bins = b;
                    changed = true;
                } else {
                    v.message = e;
                }
            }
        }
        if (changed) {
            P.maps[v.selected].mapping.clear();
            v.addMapData(*this, size_t(v.selected));
            v.metaDirty = true;
            ++v.version;
        }
    }
}

// ---------------------------------------------------------------------------
// Interpreters
// ---------------------------------------------------------------------------

void App::uiValidationInterpreters() {
    PROFILE_ZONE();
    Validation& v = *val_;
    if (!v.project) {
        ImGui::TextWrapped("Open a project first.");
        return;
    }
    val::Project& P = *v.project;
    ImGui::Text("You label as \"%s\"", interpreterName().c_str());
    ImGui::SameLine();
    if (ImGui::SmallButton("Change...")) openSettings();
    std::vector<std::string> who;
    for (const auto& [w, l] : P.labels)
        if (w != val::kAdjudicated) who.push_back(w);
    if (ImGui::BeginTable("##who", 3, ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
        ImGui::TableSetupColumn("interpreter", ImGuiTableColumnFlags_WidthStretch);
        ImGui::TableSetupColumn("points");
        ImGui::TableSetupColumn("last label");
        ImGui::TableHeadersRow();
        for (const auto& [w, l] : P.labels) {
            int n = 0;
            std::string last;
            for (const val::Label& x : l)
                if (!x.label.empty()) {
                    ++n;
                    last = std::max(last, x.updated);
                }
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(w.c_str());
            ImGui::TableNextColumn();
            ImGui::Text("%d", n);
            ImGui::TableNextColumn();
            ImGui::TextUnformatted(last.c_str());
        }
        ImGui::EndTable();
    }
    // Agreement of every pair, on the points both labelled.
    if (who.size() >= 2) {
        ImGui::SeparatorText("Agreement");
        if (ImGui::BeginTable("##kappa", 5, ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
            ImGui::TableSetupColumn("interpreters", ImGuiTableColumnFlags_WidthStretch);
            ImGui::TableSetupColumn("both");
            ImGui::TableSetupColumn("agree");
            ImGui::TableSetupColumn("kappa");
            ImGui::TableSetupColumn("dates within 1 yr");
            ImGui::TableHeadersRow();
            for (size_t i = 0; i < who.size(); ++i)
                for (size_t j = i + 1; j < who.size(); ++j) {
                    std::vector<std::string> a, b;
                    int dates = 0, datesOk = 0;
                    const auto& la = P.labels[who[i]];
                    const auto& lb = P.labels[who[j]];
                    for (size_t k = 0; k < P.points.size(); ++k) {
                        a.push_back(k < la.size() ? la[k].label : "");
                        b.push_back(k < lb.size() ? lb[k].label : "");
                        const int ya = k < la.size() ? val::yearOf(la[k].date) : 0, yb = k < lb.size() ? val::yearOf(lb[k].date) : 0;
                        if (ya && yb) {
                            ++dates;
                            datesOk += std::abs(ya - yb) <= 1;
                        }
                    }
                    const val::Agreement g = val::cohen(a, b);
                    ImGui::TableNextRow();
                    ImGui::TableNextColumn();
                    ImGui::Text("%s / %s", who[i].c_str(), who[j].c_str());
                    ImGui::TableNextColumn();
                    ImGui::Text("%d", g.n);
                    ImGui::TableNextColumn();
                    if (g.n) ImGui::Text("%.1f%%", 100 * g.observed);
                    ImGui::TableNextColumn();
                    if (g.n) ImGui::Text("%.3f", g.kappa);
                    ImGui::TableNextColumn();
                    if (dates) ImGui::Text("%d of %d", datesOk, dates);
                }
            ImGui::EndTable();
        }
        ImGui::TextDisabled("Cohen's kappa: the agreement beyond chance (points, not areas).");
    }
    int conflicts = 0;
    for (size_t i = 0; i < P.points.size(); ++i) {
        bool c = false;
        val::resolve(P.labels, i, c);
        conflicts += c;
    }
    ImGui::SeparatorText("Disagreements");
    ImGui::Text("%d point(s) where interpreters disagree", conflicts);
    ImGui::SameLine();
    ImGui::BeginDisabled(!conflicts || !s_);
    if (ImGui::Button("Adjudicate...")) startAdjudication();
    ImGui::EndDisabled();
    ImGui::SetItemTooltip("Goes through them; the label you give settles each (\"%s\"), the others' labels shown",
                          val::kAdjudicated);
    ImGui::SeparatorText("Several interpreters");
    ImGui::TextWrapped("Each interpreter can label a copy of the project (the GeoPackage and its strata raster); "
                       "their labels are then merged here, by point.");
    if (ImGui::Button("Save a copy for an interpreter...")) {
        const std::string path = askSavePath("A copy of the project", fs::u8path(P.path).stem().u8string() + "_copy.gpkg",
                                             "GeoPackage", "gpkg");
        if (!path.empty()) {
            validationSaveLabels();
            v.message.clear();
            std::error_code ec;
            if (v.writer.valid()) v.writer.wait();
            fs::copy_file(fs::u8path(P.path), fs::u8path(path), fs::copy_options::overwrite_existing, ec);
            const fs::path rel = fs::u8path(P.strataPath).lexically_relative(fs::u8path(P.path).parent_path());
            const fs::path to = fs::u8path(path).parent_path() / rel;
            if (!ec && to != fs::u8path(P.strataPath)) {
                fs::create_directories(to.parent_path(), ec);
                fs::copy_file(fs::u8path(P.strataPath), to, fs::copy_options::overwrite_existing, ec);
            }
            v.message = ec ? "The copy failed: " + ec.message() : "Copied to " + path + " (with its strata raster).";
        }
    }
    ImGui::SameLine();
    if (ImGui::Button("Merge labels from a copy...")) {
        const auto files = platform::openFilesDialog();
        if (!files.empty()) validationMerge(files.front(), v.message);
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

namespace {

std::string methodsText(const val::Project& P, int labelled, int interpreters, double confidence) {
    std::string strata;
    for (const val::Stratum& s : P.strata.strata) strata += (strata.empty() ? "" : ", ") + s.name + " (" + std::to_string(s.n) + ")";
    std::string maps;
    for (const val::MapRule& r : P.spec.maps) maps += (maps.empty() ? "" : ", ") + r.name;
    char b[2048];
    std::snprintf(b, sizeof(b),
                  "A stratified random sample of %zu pixels was drawn from %zu strata defined on %s%s%s "
                  "(points per stratum: %s; allocation: %s; seed %llu), following the good practices of "
                  "Olofsson et al. (2014). The reference class of each sample pixel, and the date of any change, "
                  "were interpreted visually from its time series and images in Janus by %d interpreter(s); %d "
                  "points had a reference label. Accuracies (overall, user's and producer's) and error-adjusted "
                  "areas were estimated, with %d%% confidence intervals, by the stratified estimators of "
                  "Stehman (2014), which equal those of Olofsson et al. (2014) when the strata are the map's classes.",
                  P.points.size(), P.strata.strata.size(), maps.c_str(),
                  P.spec.buffer ? (", with a buffer stratum of " + std::to_string(P.spec.buffer) + " pixel(s) around change").c_str() : "",
                  P.spec.limit.empty() ? "" : ", inside a study area", strata.c_str(),
                  P.design.value("alloc", std::string("proportional")).c_str(), (unsigned long long)P.seed, interpreters, labelled,
                  int(std::lround(confidence * 100)));
    return b;
}

const char* kReferences =
    "Olofsson, P., Foody, G. M., Herold, M., Stehman, S. V., Woodcock, C. E., Wulder, M. A. (2014). Good practices for "
    "estimating area and assessing accuracy of land change. Remote Sensing of Environment, 148, 42-57.\n"
    "Stehman, S. V. (2014). Estimating area and map accuracy for stratified random sampling when the strata are "
    "different from the map classes. International Journal of Remote Sensing, 35(13), 4923-4939.";

// A bar chart with error bars as SVG (the report's figures).
std::string svgBars(const std::vector<std::string>& labels, const std::vector<std::vector<double>>& series,
                    const std::vector<std::vector<double>>& errors, const std::vector<std::string>& names,
                    const std::string& unit) {
    const int W = 720, H = 300, L = 70, R = 10, T = 20, B = 60;
    double mx = 0;
    for (size_t s = 0; s < series.size(); ++s)
        for (size_t i = 0; i < series[s].size(); ++i)
            mx = std::max(mx, series[s][i] + (s < errors.size() && i < errors[s].size() ? errors[s][i] : 0));
    if (mx <= 0) mx = 1;
    const char* colors[] = {"#8a8f98", "#2a78d6", "#d64545"};
    std::string o = "<svg viewBox=\"0 0 " + std::to_string(W) + " " + std::to_string(H) +
                    "\" xmlns=\"http://www.w3.org/2000/svg\" font-family=\"sans-serif\" font-size=\"11\">";
    const double pw = W - L - R, ph = H - T - B;
    for (int k = 0; k <= 4; ++k) {
        const double y = T + ph - ph * k / 4;
        char b[256];
        std::snprintf(b, sizeof(b), "<line x1=\"%d\" x2=\"%d\" y1=\"%.1f\" y2=\"%.1f\" stroke=\"#ccc\"/>"
                                    "<text x=\"%d\" y=\"%.1f\" text-anchor=\"end\" fill=\"#555\">%s</text>",
                      L, W - R, y, y, L - 6, y + 4, fmtArea(mx * k / 4).c_str());
        o += b;
    }
    const size_t n = labels.size(), S = series.size();
    const double gw = pw / std::max<size_t>(1, n), bw = gw * 0.7 / std::max<size_t>(1, S);
    for (size_t i = 0; i < n; ++i) {
        for (size_t s = 0; s < S; ++s) {
            const double v = i < series[s].size() ? series[s][i] : 0;
            const double x = L + gw * i + gw * 0.15 + bw * s, h = ph * v / mx;
            char b[512];
            std::snprintf(b, sizeof(b), "<rect x=\"%.1f\" y=\"%.1f\" width=\"%.1f\" height=\"%.1f\" fill=\"%s\"/>", x, T + ph - h,
                          bw * 0.92, h, colors[s % 3]);
            o += b;
            if (s < errors.size() && i < errors[s].size() && errors[s][i] > 0) {
                const double cx = x + bw * 0.46, y0 = T + ph - ph * (v - errors[s][i]) / mx, y1 = T + ph - ph * (v + errors[s][i]) / mx;
                std::snprintf(b, sizeof(b), "<line x1=\"%.1f\" x2=\"%.1f\" y1=\"%.1f\" y2=\"%.1f\" stroke=\"#222\"/>"
                                            "<line x1=\"%.1f\" x2=\"%.1f\" y1=\"%.1f\" y2=\"%.1f\" stroke=\"#222\"/>"
                                            "<line x1=\"%.1f\" x2=\"%.1f\" y1=\"%.1f\" y2=\"%.1f\" stroke=\"#222\"/>",
                              cx, cx, std::min(T + ph, y0), y1, cx - 4, cx + 4, y1, y1, cx - 4, cx + 4, std::min(T + ph, y0),
                              std::min(T + ph, y0));
                o += b;
            }
        }
        char b[512];
        std::snprintf(b, sizeof(b), "<text x=\"%.1f\" y=\"%d\" text-anchor=\"middle\" fill=\"#333\">%s</text>",
                      L + gw * i + gw / 2, T + int(ph) + 16, htmlEsc(labels[i]).c_str());
        o += b;
    }
    for (size_t s = 0; s < names.size(); ++s) {
        char b[512];
        std::snprintf(b, sizeof(b), "<rect x=\"%d\" y=\"%d\" width=\"10\" height=\"10\" fill=\"%s\"/><text x=\"%d\" y=\"%d\">%s</text>",
                      L + 10 + int(s) * 130, H - 18, colors[s % 3], L + 24 + int(s) * 130, H - 9, htmlEsc(names[s]).c_str());
        o += b;
    }
    o += "<text x=\"12\" y=\"" + std::to_string(T + int(ph) / 2) + "\" transform=\"rotate(-90 12 " +
         std::to_string(T + int(ph) / 2) + ")\" text-anchor=\"middle\">" + htmlEsc(unit) + "</text></svg>";
    return o;
}

} // namespace

void App::uiValidationReport() {
    PROFILE_ZONE();
    Validation& v = *val_;
    if (!v.project) {
        ImGui::TextWrapped("Open a project first.");
        return;
    }
    const val::Project& P = *v.project;
    int labelled = 0;
    for (size_t i = 0; i < P.points.size(); ++i) {
        bool c = false;
        labelled += val::resolve(P.labels, i, c) != nullptr;
    }
    int interpreters = 0;
    for (const auto& [w, l] : P.labels) interpreters += w != val::kAdjudicated;
    if (ImGui::Button("Export the report...")) {
        const std::string dir = platform::openFolderDialog();
        if (!dir.empty()) exportValidationReport(dir);
    }
    ImGui::SetItemTooltip("report.html (tables, figures, methods, references) and a CSV of each table, in a folder");
    ImGui::SameLine();
    if (ImGui::Button("Copy Python") && !P.maps.empty()) {
        const int k = std::clamp(v.selected, 0, int(P.maps.size()) - 1);
        ImGui::SetClipboardText(val::pythonSnippet(P, P.maps[k], v.confidence).c_str());
        v.message = "Python for " + P.maps[k].rule.name + " copied: zeit.accuracy on the project's files gives the same numbers.";
    }
    ImGui::SetItemTooltip("zeit.accuracy on the project's GeoPackage and rasters, for the map selected in Accuracy");
    ImGui::SeparatorText("Methods");
    std::string text = methodsText(P, labelled, interpreters, v.confidence);
    ImGui::PushTextWrapPos(0);
    ImGui::TextUnformatted(text.c_str());
    ImGui::PopTextWrapPos();
    if (ImGui::SmallButton("Copy##methods")) ImGui::SetClipboardText((text + "\n\n" + kReferences).c_str());
    ImGui::SeparatorText("References");
    ImGui::PushTextWrapPos(0);
    ImGui::TextDisabled("%s", kReferences);
    ImGui::PopTextWrapPos();
}

std::shared_ptr<ExportJob> App::exportValidationReport(const std::string& folder) {
    if (!val_ || !val_->project) return nullptr;
    Validation& v = *val_;
    const val::Project& P = *v.project;
    // Everything the report needs, computed now (cheap), written in the background.
    struct MapOut {
        std::string name;
        val::Prepared prep;
        val::Assessment a;
        BinaryChange change;
        std::vector<std::pair<int, val::ClassArea>> byYear;
        bool ready = false;
    };
    auto maps = std::make_shared<std::vector<MapOut>>();
    for (size_t k = 0; k < P.maps.size(); ++k) {
        MapOut o;
        o.name = P.maps[k].rule.name;
        const Validation::MapData& d = *v.data[k];
        if (d.ready()) {
            o.prep = val::prepare(P, P.maps[k], d.values, d.mapped, v.confidence);
            o.a = val::assess(o.prep.in);
            o.change = binaryChange(P.maps[k].rule, o.prep);
            if (P.maps[k].rule.kind == val::MapKind::Events) o.byYear = val::changeByYear(P, P.maps[k], v.confidence);
            o.ready = true;
        }
        maps->push_back(std::move(o));
    }
    int labelled = 0;
    for (size_t i = 0; i < P.points.size(); ++i) {
        bool c = false;
        labelled += val::resolve(P.labels, i, c) != nullptr;
    }
    int interpreters = 0;
    std::vector<std::string> who;
    for (const auto& [w, l] : P.labels)
        if (w != val::kAdjudicated) {
            ++interpreters;
            who.push_back(w);
        }
    auto proj = std::make_shared<val::Project>(P);
    const double conf = v.confidence;
    const std::string methods = methodsText(P, labelled, interpreters, conf);
    const std::string html = (fs::u8path(folder) / "report.html").u8string();
    auto job = startExportJob("Validation report", html, [=](ExportJob& j, std::string& error) {
        const fs::path dir = fs::u8path(folder);
        const val::Project& p = *proj;
        const int pct = int(std::lround(conf * 100));
        auto write = [&](const std::string& name, const std::string& text) {
            std::ofstream f(dir / fs::u8path(name), std::ios::binary);
            f << text;
            if (!f) error = "cannot write " + (dir / fs::u8path(name)).u8string();
            return bool(f);
        };
        // Design
        std::string csv = "code,stratum,pixels,area_" + p.strata.unit + ",weight,expected_ua,n\n";
        for (const val::Stratum& s : p.strata.strata) {
            char b[256];
            std::snprintf(b, sizeof(b), ",%lld,%.6f,%.8f,%.3f,%d\n", (long long)s.pixels, s.area, s.weight, s.expectedUa, s.n);
            csv += std::to_string(s.code) + "," + csvCell(s.name) + b;
        }
        if (!write("design.csv", csv)) return false;
        std::string h = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Validation report</title><style>"
                        "body{font-family:system-ui,sans-serif;max-width:980px;margin:24px auto;padding:0 16px;color:#222}"
                        "table{border-collapse:collapse;margin:8px 0 18px}td,th{border:1px solid #ccc;padding:3px 8px;text-align:right}"
                        "th{background:#f3f3f3}td:first-child,th:first-child{text-align:left}.diag{background:#e6f5ea}"
                        "h2{margin-top:32px;border-bottom:1px solid #ddd}p.small{color:#666;font-size:90%}</style></head><body>";
        h += "<h1>Validation report</h1><p class=\"small\">" + htmlEsc(fs::u8path(p.path).filename().u8string()) + " &middot; " +
             htmlEsc(val::nowIso()) + " &middot; Janus</p>";
        h += "<h2>Methods</h2><p>" + htmlEsc(methods) + "</p>";
        h += "<h2>Sampling design</h2><table><tr><th>stratum</th><th>pixels</th><th>area (" + htmlEsc(p.strata.unit) +
             ")</th><th>weight</th><th>expected UA</th><th>points</th></tr>";
        for (const val::Stratum& s : p.strata.strata) {
            char b[256];
            std::snprintf(b, sizeof(b), "</td><td>%s</td><td>%s</td><td>%.4f</td><td>%.2f</td><td>%d</td></tr>",
                          fmtArea(double(s.pixels)).c_str(), fmtArea(s.area).c_str(), s.weight, s.expectedUa, s.n);
            h += "<tr><td>" + htmlEsc(s.name) + b;
        }
        h += "</table>";
        // Comparison of the maps
        std::string cmp = "map,points,overall,overall_ci,change_users,change_users_ci,change_producers,change_producers_ci,"
                          "change_mapped,change_adjusted,change_adjusted_ci\n";
        h += "<h2>Maps</h2><table><tr><th>map</th><th>points</th><th>overall</th><th>change: user's</th><th>producer's</th>"
             "<th>mapped</th><th>adjusted (&plusmn;" + std::to_string(pct) + "%)</th></tr>";
        for (const MapOut& m : *maps) {
            if (!m.ready) continue;
            char b[512];
            std::snprintf(b, sizeof(b), ",%d,%.6f,%.6f,%.6f,%.6f,%.6f,%.6f,%.4f,%.4f,%.4f\n", m.a.n, m.a.overall.est,
                          m.a.overall.hi - m.a.overall.est, m.change.users.est, m.change.users.hi - m.change.users.est,
                          m.change.producers.est, m.change.producers.hi - m.change.producers.est, m.change.mapped,
                          m.change.area.est, m.change.area.hi - m.change.area.est);
            cmp += csvCell(m.name) + b;
            h += "<tr><td>" + htmlEsc(m.name) + "</td><td>" + std::to_string(m.a.n) + "</td><td>" + fmtPm(m.a.overall) +
                 "</td><td>" + (m.change.defined ? fmtPm(m.change.users) : "-") + "</td><td>" +
                 (m.change.defined ? fmtPm(m.change.producers) : "-") + "</td><td>" + (m.change.defined ? fmtArea(m.change.mapped) : "-") +
                 "</td><td>" + (m.change.defined ? fmtArea(m.change.area.est) + " &plusmn; " + fmtArea(m.change.area.hi - m.change.area.est) : "-") +
                 "</td></tr>";
        }
        h += "</table>";
        if (!write("maps.csv", cmp)) return false;
        setExportProgress(j, 0.3);
        // Each map: error matrix, accuracies and areas
        int k = 0;
        for (const MapOut& m : *maps) {
            ++k;
            if (!m.ready) continue;
            const val::Assessment& a = m.a;
            const size_t C = a.classes.size();
            const std::string tag = "map" + std::to_string(k);
            h += "<h2>" + htmlEsc(m.name) + "</h2><p>Overall accuracy " + fmtPm(a.overall) + " (" + std::to_string(pct) +
                 "% interval), " + std::to_string(a.n) + " points.</p>";
            std::string mc = "map\\reference";
            for (const std::string& c : a.classes) mc += "," + csvCell(c);
            mc += "\n";
            std::string mp = mc;
            h += "<table><tr><th>map \\ reference</th>";
            for (const std::string& c : a.classes) h += "<th>" + htmlEsc(c) + "</th>";
            h += "</tr>";
            for (size_t i = 0; i < C; ++i) {
                mc += csvCell(a.classes[i]);
                mp += csvCell(a.classes[i]);
                h += "<tr><td>" + htmlEsc(a.classes[i]) + "</td>";
                for (size_t jj = 0; jj < C; ++jj) {
                    char b[64];
                    std::snprintf(b, sizeof(b), ",%.8f", a.confusion[i][jj]);
                    mp += b;
                    mc += "," + std::to_string(a.counts[i][jj]);
                    std::snprintf(b, sizeof(b), "%d <small>(%.4f)</small>", a.counts[i][jj], a.confusion[i][jj]);
                    h += std::string("<td") + (i == jj ? " class=\"diag\"" : "") + ">" + b + "</td>";
                }
                mc += "\n";
                mp += "\n";
                h += "</tr>";
            }
            h += "</table><p class=\"small\">Points (and proportions of area). Rows: the map; columns: the reference.</p>";
            if (!write(tag + "_matrix_points.csv", mc) || !write(tag + "_matrix_area.csv", mp)) return false;
            std::string ac = "class,users,users_se,users_ci,producers,producers_se,producers_ci,mapped,adjusted,adjusted_se,adjusted_ci\n";
            h += "<table><tr><th>class</th><th>user's</th><th>producer's</th><th>mapped (" + htmlEsc(p.strata.unit) +
                 ")</th><th>adjusted (" + htmlEsc(p.strata.unit) + ")</th></tr>";
            std::vector<std::string> labels;
            std::vector<std::vector<double>> series(2), errs(2);
            for (size_t c = 0; c < C; ++c) {
                char b[512];
                std::snprintf(b, sizeof(b), ",%.6f,%.6f,%.6f,%.6f,%.6f,%.6f,%.4f,%.4f,%.4f,%.4f\n", a.users[c].est, a.users[c].se,
                              a.users[c].hi - a.users[c].est, a.producers[c].est, a.producers[c].se,
                              a.producers[c].hi - a.producers[c].est, a.area[c].mapped, a.area[c].est, a.area[c].se, a.area[c].ci);
                ac += csvCell(a.classes[c]) + b;
                h += "<tr><td>" + htmlEsc(a.classes[c]) + "</td><td>" + fmtPm(a.users[c]) + "</td><td>" + fmtPm(a.producers[c]) +
                     "</td><td>" + fmtArea(a.area[c].mapped) + "</td><td>" + fmtArea(a.area[c].est) + " &plusmn; " +
                     fmtArea(a.area[c].ci) + "</td></tr>";
                labels.push_back(a.classes[c]);
                series[0].push_back(a.area[c].mapped);
                series[1].push_back(std::isfinite(a.area[c].est) ? a.area[c].est : 0);
                errs[0].push_back(0);
                errs[1].push_back(std::isfinite(a.area[c].ci) ? a.area[c].ci : 0);
            }
            h += "</table>";
            h += svgBars(labels, series, errs, {"mapped", "adjusted (+- " + std::to_string(pct) + "%)"}, p.strata.unit);
            if (!write(tag + "_accuracy.csv", ac)) return false;
            if (!m.byYear.empty()) {
                std::string yc = "year,area,se,ci\n";
                std::vector<std::string> yl;
                std::vector<std::vector<double>> ys(1), ye(1);
                for (const auto& [y, ar] : m.byYear) {
                    char b[128];
                    std::snprintf(b, sizeof(b), "%d,%.4f,%.4f,%.4f\n", y, ar.est, ar.se, ar.ci);
                    yc += b;
                    yl.push_back(std::to_string(y));
                    ys[0].push_back(ar.est);
                    ye[0].push_back(ar.ci);
                }
                if (!write(tag + "_change_by_year.csv", yc)) return false;
                h += "<p>Area of change by year (the reference's dates):</p>" +
                     svgBars(yl, ys, ye, {"area of change (+- " + std::to_string(pct) + "%)"}, p.strata.unit);
            }
        }
        setExportProgress(j, 0.7);
        // Interpreters
        if (who.size() >= 2) {
            std::string kc = "a,b,points,agreement,kappa\n";
            h += "<h2>Interpreters</h2><table><tr><th>interpreters</th><th>points</th><th>agreement</th><th>kappa</th></tr>";
            for (size_t i = 0; i < who.size(); ++i)
                for (size_t jj = i + 1; jj < who.size(); ++jj) {
                    std::vector<std::string> a, b;
                    const auto& la = p.labels.at(who[i]);
                    const auto& lb = p.labels.at(who[jj]);
                    for (size_t q = 0; q < p.points.size(); ++q) {
                        a.push_back(q < la.size() ? la[q].label : "");
                        b.push_back(q < lb.size() ? lb[q].label : "");
                    }
                    const val::Agreement g = val::cohen(a, b);
                    char t[128];
                    std::snprintf(t, sizeof(t), ",%d,%.4f,%.4f\n", g.n, g.observed, g.kappa);
                    kc += csvCell(who[i]) + "," + csvCell(who[jj]) + t;
                    std::snprintf(t, sizeof(t), "</td><td>%d</td><td>%.1f%%</td><td>%.3f</td></tr>", g.n, 100 * g.observed, g.kappa);
                    h += "<tr><td>" + htmlEsc(who[i] + " / " + who[jj]) + t;
                }
            h += "</table>";
            if (!write("interpreters.csv", kc)) return false;
        }
        h += "<h2>References</h2><p class=\"small\">" + htmlEsc(kReferences) + "</p></body></html>";
        size_t pos = 0;
        while ((pos = h.find('\n', pos)) != std::string::npos) h.replace(pos, 1, "<br>"), pos += 4;
        if (!write("report.html", h)) return false;
        setExportProgress(j, 1.0);
        return true;
    });
    exports_.push_back(job);
    showExports_ = true;
    return job;
}

// ---------------------------------------------------------------------------
// The interpretation window's part for a project
// ---------------------------------------------------------------------------

void App::uiInterpretationProject() {
    PROFILE_ZONE();
    if (!val_ || !val_->project) return;
    Validation& v = *val_;
    const val::Project& P = *v.project;
    if (lab_.as == val::kAdjudicated) ImGui::TextColored(theme::warning(), "Adjudicating: your label settles the point");
    else ImGui::TextDisabled("Labelling as %s", lab_.as.c_str());
    ImGui::SameLine();
    bool blind = v.blind;
    if (ImGui::Checkbox("Blind##interp", &blind)) setValidationBlind(blind);
    ImGui::SameLine();
    if (ImGui::SmallButton("Chips")) showChips_ = !showChips_;
    ImGui::SetItemTooltip("The point at every date, around it (Validation > Chips)");
    if (!v.blind) {
        ImGui::SameLine();
        ImGui::SetNextItemWidth(-1);
        ImGui::Combo("##colorby", &v.colorBy, "points by state\0points by stratum\0right / wrong (selected map)\0");
    }
    if (lab_.as != val::kAdjudicated) {
        // Not labelled by me yet: the next ones.
        ImGui::SameLine();
        if (ImGui::SmallButton("Mine to do")) {
            lab_.filter.clear();
            for (size_t i = 0; i < lab_.entries.size(); ++i)
                if (lab_.entries[i].label.empty()) lab_.filter.push_back(int(i));
            lab_.filterName = std::to_string(lab_.filter.size()) + " not labelled by " + lab_.as;
            if (!lab_.filter.empty()) interpGo(lab_.filter.front());
        }
    } else if (ImGui::SmallButton("Back to my labels")) {
        if (lab_.dirty) saveLabels();
        lab_.as = interpreterName();
        validationEntries();
        lab_.filter.clear();
        lab_.filterName.clear();
    }
    // Others' labels of this point (all of them when adjudicating; after one's own label otherwise).
    if (lab_.current >= 0 && lab_.current < int(P.points.size())) {
        const size_t i = size_t(lab_.current);
        const bool mineGiven = !lab_.entries[i].label.empty();
        int others = 0;
        for (const auto& [w, l] : P.labels)
            if (w != lab_.as && i < l.size() && !l[i].label.empty()) ++others;
        if (others && (lab_.as == val::kAdjudicated || mineGiven || !v.blind)) {
            if (ImGui::BeginTable("##others", 4, ImGuiTableFlags_RowBg | ImGuiTableFlags_SizingFixedFit)) {
                for (const auto& [w, l] : P.labels) {
                    if (w == lab_.as || i >= l.size() || l[i].label.empty()) continue;
                    ImGui::TableNextRow();
                    ImGui::TableNextColumn();
                    ImGui::TextDisabled("%s", w.c_str());
                    ImGui::TableNextColumn();
                    ImGui::TextUnformatted(l[i].label.c_str());
                    ImGui::TableNextColumn();
                    ImGui::TextUnformatted(l[i].date.c_str());
                    ImGui::TableNextColumn();
                    ImGui::TextDisabled("%s", l[i].notes.c_str());
                }
                ImGui::EndTable();
            }
        } else if (others) {
            ImGui::TextDisabled("%d other interpreter(s) labelled this point (shown after your label)", others);
        }
    }
}

// ---------------------------------------------------------------------------
// Chips: the feature being interpreted at every date
// ---------------------------------------------------------------------------

void App::pumpChips() {
    PROFILE_ZONE();
    if (!showChips_ || !s_ || !lab_.vector) return;
    if (!val_) val_ = std::make_shared<Validation>();
    Validation& v = *val_;
    const VectorLayer* V = vectorById(lab_.vector);
    if (!V || V->anchor.empty()) return;
    const SeriesLayer* L = activeLayer();
    if (!L) return;
    const bool rgb = compositeOn(*L);
    const std::array<int, 3> bands = rgb ? L->comp->bands : std::array<int, 3>{0, 0, 0};
    const auto info = s_->info;
    const int r = v.chipRadius;
    auto keyOf = [&](int cx, int cy) {
        uint64_t k = info->id * 1000003ull ^ (uint64_t(uint32_t(cx)) << 32 | uint32_t(cy));
        k = k * 31 + uint64_t(r);
        for (int b : bands) k = k * 131 + uint64_t(b);
        return k;
    };
    // The current feature and the next two (read ahead: the HDD seeks happen before they are needed).
    std::vector<int> want;
    if (lab_.current >= 0) want.push_back(lab_.current);
    {
        const auto& f = lab_.filter;
        int cur = lab_.current;
        for (int k = 0; k < 2; ++k) {
            int nx = -1;
            if (!f.empty()) {
                const auto it = std::find(f.begin(), f.end(), cur);
                if (it != f.end() && it + 1 != f.end()) nx = *(it + 1);
            } else if (cur + 1 < int(V->anchor.size())) {
                nx = cur + 1;
            }
            if (nx < 0) break;
            want.push_back(nx);
            cur = nx;
        }
    }
    const double now = ImGui::GetTime();
    for (int fi : want) {
        if (fi < 0 || fi >= int(V->anchor.size()) || std::isnan(V->anchor[fi].x)) continue;
        const int cx = int(V->anchor[fi].x), cy = int(V->anchor[fi].y);
        if (cx < 0 || cy < 0 || cx >= info->width || cy >= info->height) continue;
        const uint64_t key = keyOf(cx, cy);
        auto it = std::find_if(v.chips.begin(), v.chips.end(), [&](const Validation::ChipJob& j) { return j.key == key; });
        if (it != v.chips.end()) {
            it->used = now;
            continue;
        }
        Validation::ChipJob j;
        j.key = key;
        j.used = now - (fi == lab_.current ? 0 : 0.5);
        j.f = std::async(std::launch::async, [info, cx, cy, r, rgb, bands]() {
                  auto c = std::make_shared<Validation::ChipSet>();
                  c->cube = info->id;
                  c->cx = cx;
                  c->cy = cy;
                  c->x0 = cx - r;
                  c->y0 = cy - r;
                  c->w = c->h = 2 * r + 1;
                  c->T = info->T();
                  c->rgb = rgb;
                  const int nb = rgb ? 3 : 1, w = c->w;
                  c->v.assign(size_t(c->T) * nb * w * w, NAN);
                  // The part of the window on the layer.
                  const int x0 = std::max(0, c->x0), y0 = std::max(0, c->y0);
                  const int x1 = std::min(info->width, c->x0 + w), y1 = std::min(info->height, c->y0 + w);
                  if (x1 <= x0 || y1 <= y0) return c;
                  CubeReader& rd = threadReader(info);
                  std::vector<float> buf(size_t(x1 - x0) * (y1 - y0));
                  for (int t = 0; t < c->T; ++t)
                      for (int b = 0; b < nb; ++b) {
                          const bool ok = rgb ? rd.readBandWindow(t, bands[b], x0, y0, x1 - x0, y1 - y0, buf.data(), x1 - x0, y1 - y0)
                                              : rd.readWindow(t, x0, y0, x1 - x0, y1 - y0, buf.data(), x1 - x0, y1 - y0);
                          if (!ok) continue;
                          float* dst = &c->v[(size_t(t) * nb + b) * w * w];
                          for (int y = y0; y < y1; ++y)
                              for (int x = x0; x < x1; ++x)
                                  dst[size_t(y - c->y0) * w + (x - c->x0)] = buf[size_t(y - y0) * (x1 - x0) + (x - x0)];
                      }
                  glfwPostEmptyEvent();
                  return c;
              }).share();
        v.chips.push_back(std::move(j));
    }
    // Keep a few.
    while (v.chips.size() > 6) {
        auto old = std::min_element(v.chips.begin(), v.chips.end(),
                                    [](const Validation::ChipJob& a, const Validation::ChipJob& b) { return a.used < b.used; });
        v.chips.erase(old);
    }
}

void App::uiChips() {
    PROFILE_ZONE();
    if (!showChips_) return;
    if (!val_) val_ = std::make_shared<Validation>();
    Validation& v = *val_;
    ImGui::SetNextWindowSize(ImVec2(560, 460), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Chips", &showChips_)) {
        ImGui::End();
        return;
    }
    ImGui::SetNextItemWidth(120);
    ImGui::SliderInt("radius (px)", &v.chipRadius, 4, 40);
    ImGui::SameLine();
    ImGui::SetNextItemWidth(120);
    ImGui::SliderFloat("size", &v.chipSize, 40, 220, "%.0f");
    ImGui::SameLine();
    ImGui::Checkbox("one per year", &v.chipPerYear);
    ImGui::SetItemTooltip("For each year, the date nearest the map's day of the year");
    const VectorLayer* V = lab_.vector ? vectorById(lab_.vector) : nullptr;
    const SeriesLayer* L = activeLayer();
    if (!V || !s_ || !L || lab_.current < 0 || lab_.current >= int(V->anchor.size())) {
        ImGui::TextWrapped("Start an interpretation (a vector layer's Interpret..., or Validation > Interpret the sample): "
                           "the feature being interpreted is shown at every date of the active layer, around it.");
        ImGui::End();
        return;
    }
    const CubeInfo& info = *s_->info;
    const int cx = int(V->anchor[lab_.current].x), cy = int(V->anchor[lab_.current].y);
    std::shared_ptr<Validation::ChipSet> set;
    for (auto& j : v.chips)
        if (j.f.valid() && j.f.wait_for(std::chrono::seconds(0)) == std::future_status::ready) {
            auto c = j.f.get();
            if (c && c->cube == info.id && c->cx == cx && c->cy == cy && c->w == 2 * v.chipRadius + 1 && c->rgb == compositeOn(*L))
                set = c;
        }
    if (!set) {
        ImGui::TextDisabled("Reading the dates around the point...");
        ImGui::End();
        return;
    }
    // The dates shown: all, or one per year (nearest the map's day of the year).
    std::vector<int> dates;
    if (v.chipPerYear && info.timeIsDate) {
        const int64_t d0 = info.unixDay(t_);
        int y0, d0y;
        unsigned m0, dd0;
        civilFromDays(d0, y0, m0, dd0);
        d0y = int(d0 - daysFromCivil(y0, 1, 1));
        std::map<int, std::pair<int, int>> best; // year -> (distance, date)
        for (int t = 0; t < info.T(); ++t) {
            int y;
            unsigned m, dd;
            const int64_t day = info.unixDay(t);
            civilFromDays(day, y, m, dd);
            const int doy = int(day - daysFromCivil(y, 1, 1));
            const int dist = std::abs(doy - d0y);
            auto it = best.find(y);
            if (it == best.end() || dist < it->second.first) best[y] = {dist, t};
        }
        for (const auto& [y, b] : best) dates.push_back(b.second);
    } else {
        for (int t = 0; t < info.T(); ++t) dates.push_back(t);
    }
    // The colours: the map's (its colormap and range, a class's colour, or the composite's stretch).
    const int w = set->w, scaleUp = std::max(1, int(std::ceil(v.chipSize / w)));
    const int cell = w * scaleUp;
    const int cols = std::max(1, int(std::ceil(std::sqrt(double(dates.size())))));
    const int rows = int((dates.size() + cols - 1) / cols);
    const LayerClasses* C = L->classes.state == LayerClasses::On ? &L->classes : nullptr;
    uint64_t key = uint64_t(set.get()) * 1000003ull;
    for (int t : dates) key = key * 31 + uint64_t(t);
    key = key * 31 + uint64_t(cmap_[ModeValue]) * 7 + uint64_t(scaleUp);
    float lo = range_[ModeValue].lo, hi = range_[ModeValue].hi;
    uint32_t lb, hb;
    std::memcpy(&lb, &lo, 4);
    std::memcpy(&hb, &hi, 4);
    key = key * 131 + lb;
    key = key * 131 + hb;
    if (set->rgb)
        for (int b = 0; b < 3; ++b) {
            std::memcpy(&lb, &L->comp->lo[b], 4);
            std::memcpy(&hb, &L->comp->hi[b], 4);
            key = key * 131 + lb * 7 + hb;
        }
    if (key != v.chipTexKey) {
        v.chipTexKey = key;
        const int TW = cols * cell, TH = rows * cell;
        std::vector<unsigned char> rgba(size_t(TW) * TH * 4, 0);
        const int nb = set->rgb ? 3 : 1;
        const int cm = cmap_[ModeValue];
        for (size_t k = 0; k < dates.size(); ++k) {
            const int t = dates[k];
            const int ox = int(k % cols) * cell, oy = int(k / cols) * cell;
            for (int y = 0; y < w; ++y)
                for (int x = 0; x < w; ++x) {
                    unsigned char px[4] = {40, 40, 40, 255};
                    if (set->rgb) {
                        bool any = false;
                        for (int b = 0; b < 3; ++b) {
                            const float val = set->v[(size_t(t) * nb + b) * w * w + size_t(y) * w + x];
                            if (std::isnan(val)) continue;
                            any = true;
                            const float s = (val - L->comp->lo[b]) / std::max(1e-12f, L->comp->hi[b] - L->comp->lo[b]);
                            px[b] = (unsigned char)std::lround(255 * std::clamp(s, 0.0f, 1.0f));
                        }
                        if (!any) px[0] = px[1] = px[2] = 40;
                    } else {
                        const float val = set->v[size_t(t) * w * w + size_t(y) * w + x];
                        if (!std::isnan(val)) {
                            ImVec4 c;
                            if (C) {
                                const ClassEntry* e = C->find(int(std::lround(val)));
                                c = e ? e->color : ImVec4(0.3f, 0.3f, 0.3f, 1);
                            } else {
                                const float s = (val - lo) / std::max(1e-12f, hi - lo);
                                c = ImPlot::SampleColormap(std::clamp(s, 0.0f, 1.0f), cm);
                            }
                            px[0] = (unsigned char)std::lround(255 * c.x);
                            px[1] = (unsigned char)std::lround(255 * c.y);
                            px[2] = (unsigned char)std::lround(255 * c.z);
                        }
                    }
                    for (int sy = 0; sy < scaleUp; ++sy)
                        for (int sx = 0; sx < scaleUp; ++sx) {
                            unsigned char* d = &rgba[(size_t(oy + y * scaleUp + sy) * TW + ox + x * scaleUp + sx) * 4];
                            std::memcpy(d, px, 4);
                        }
                }
        }
        if (v.chipTex) Gpu::deleteTexture(v.chipTex);
        v.chipTex = Gpu::createImageTexture(TW, TH, rgba.data());
        v.chipCols = cols;
        v.chipCell = cell;
        v.chipTexW = TW;
        v.chipTexH = TH;
        v.chipDates = dates;
    }
    // The grid: click a date to show it on the map, double click to make it the date of the change.
    const std::string changeDate = lab_.current < int(lab_.entries.size()) ? lab_.entries[lab_.current].date : "";
    ImGui::BeginChild("##chips", ImVec2(0, 0), ImGuiChildFlags_None, ImGuiWindowFlags_HorizontalScrollbar);
    const float sz = v.chipSize;
    const float avail = ImGui::GetContentRegionAvail().x;
    const int perRow = std::max(1, int(avail / (sz + ImGui::GetStyle().ItemSpacing.x)));
    ImDrawList* dl = ImGui::GetWindowDrawList();
    for (size_t k = 0; k < v.chipDates.size(); ++k) {
        const int t = v.chipDates[k];
        if (k % perRow) ImGui::SameLine();
        ImGui::BeginGroup();
        const ImVec2 uv0(float(int(k % v.chipCols) * v.chipCell) / v.chipTexW, float(int(k / v.chipCols) * v.chipCell) / v.chipTexH);
        const ImVec2 uv1 = uv0 + ImVec2(float(v.chipCell) / v.chipTexW, float(v.chipCell) / v.chipTexH);
        const ImVec2 p0 = ImGui::GetCursorScreenPos();
        ImGui::PushID(int(k));
        ImGui::Image(ImTextureRef((ImTextureID)v.chipTex), ImVec2(sz, sz), uv0, uv1);
        if (ImGui::IsItemClicked()) setT(t);
        if (ImGui::IsItemHovered() && ImGui::IsMouseDoubleClicked(ImGuiMouseButton_Left)) {
            setT(t);
            interpSetDate(false);
        }
        ImGui::SetItemTooltip("%s: click to show it, double click: the date of the change", info.layers[t].label.c_str());
        // The point's pixel, the date on the map and the date of the change.
        const float c0 = (float(v.chipRadius) / (2 * v.chipRadius + 1)) * sz, c1 = c0 + sz / (2 * v.chipRadius + 1);
        dl->AddRect(p0 + ImVec2(c0, c0), p0 + ImVec2(c1, c1), IM_COL32(255, 255, 0, 255), 0, 0, 1.5f);
        if (t == t_) dl->AddRect(p0 - ImVec2(2, 2), p0 + ImVec2(sz + 2, sz + 2), IM_COL32(255, 255, 255, 255), 0, 0, 2.0f);
        if (!changeDate.empty() && info.layers[t].label == changeDate)
            dl->AddRect(p0 - ImVec2(4, 4), p0 + ImVec2(sz + 4, sz + 4), IM_COL32(240, 70, 60, 255), 0, 0, 2.0f);
        ImGui::TextDisabled("%s", info.layers[t].label.c_str());
        ImGui::PopID();
        ImGui::EndGroup();
    }
    ImGui::EndChild();
    ImGui::End();
}

void App::dropValidationTextures() {
    if (!val_) return;
    if (val_->chipTex) Gpu::deleteTexture(val_->chipTex);
    val_->chipTex = 0;
    val_->chipTexKey = 0;
    val_->chips.clear();
}

// ---------------------------------------------------------------------------
// --selftest-ui stages 170-179 (after the NetCDF ones)
// ---------------------------------------------------------------------------

// D (classes 3 Forest, 15 Pasture, 24 Urban, 33 Water; 2000-2007): 170 opens
// it; 171 stratifies its first date (Pasture as change, a buffer of 1 px); 172
// checks the strata against a pixel-by-pixel count and the strata layer, then
// draws 60 points into a project; 173 checks the project, its sample layer and
// the interpretation (blind); 174 labels every point (every 5th wrong) and
// checks the matrix; 175 reads the GeoPackage back, then merges a second
// interpreter's copy; 176 adjudicates a disagreement and adds a second map (D
// in 2007); 177 checks its assessment and opens the chips; 178 waits for them
// and exports the report; 179 checks the report's files and closes the project.
int App::selfTestValidationUi(const std::vector<std::string>& in) {
    auto now = [] { return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count(); };
    auto fail = [&](const std::string& what) {
        std::printf("FAIL (stage %d): %s\n", st_.stage, what.c_str());
        return 1;
    };
    auto next = [&](const std::string& done) {
        std::printf("[%5.1f s] %s\n", now() - st_.t0, done.c_str());
        ++st_.stage;
        st_.since = now();
    };
    if (now() - st_.since > 90) return fail("timeout");
    if (!val_) val_ = std::make_shared<Validation>();
    Validation& v = *val_;
    auto idle = [&] { return !v.writer.valid() && v.tasks.empty() && v.pending.empty(); };
    auto conflicts = [&] {
        int n = 0;
        for (size_t i = 0; i < v.project->points.size(); ++i) {
            bool c = false;
            val::resolve(v.project->labels, i, c);
            n += c;
        }
        return n;
    };
    switch (st_.stage) {
    case 170: {
        if (opening_.valid()) break;
        if (in.size() < 4) return fail("D is the fourth input");
        closeValidationProject();
        stopInterpretation();
        closeAll();
        st_.dir = (fs::temp_directory_path() /
                   ("janus-selftest-validation-ui-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count())))
                      .u8string();
        std::error_code ec;
        fs::create_directories(fs::u8path(st_.dir), ec);
        openInputs({in[3]});
        next("D opened for a validation project");
        break;
    }
    case 171: {
        if (opening_.valid() || !s_ || layers_.empty() || !s_->overview.complete() ||
            layers_[0].classes.state == LayerClasses::Undecided)
            break;
        if (layers_[0].classes.state != LayerClasses::On) return fail("D should be categorical");
        setT(0);
        const auto src = Validation::sources(*this);
        const std::string first = s_->info->layers[0].path;
        const auto it = std::find_if(src.begin(), src.end(), [&](const Validation::Source& s) { return s.rule.path == first; });
        if (it == src.end()) return fail("D's first date is not among the maps to stratify");
        val::MapRule r = it->rule;
        if (r.kind != val::MapKind::Classes || r.names.size() < 4 || r.names[15] != "Pasture")
            return fail("D's first date: classes with their names expected");
        r.change = {15};
        v.maps = {r};
        v.binsText = {""};
        v.changeText = {""};
        v.buffer = 1;
        v.limit = 0;
        std::string why;
        validationComputeStrata(validationSpec(why));
        next("strata of D's classes (Pasture as change, a buffer of 1 px) computing");
        break;
    }
    case 172: {
        if (v.strataJob.valid() || !v.toOpen.empty() || opening_.valid()) break;
        if (!v.designError.empty()) return fail(v.designError);
        // The same strata, pixel by pixel, from the file.
        GDALDataset* ds = GDALDataset::FromHandle(GDALOpen(v.maps[0].path.c_str(), GA_ReadOnly));
        if (!ds) return fail("D's first date cannot be read");
        const int W = ds->GetRasterXSize(), H = ds->GetRasterYSize();
        std::vector<int> a(size_t(W) * H);
        ds->GetRasterBand(1)->RasterIO(GF_Read, 0, 0, W, H, a.data(), W, H, GDT_Int32, 0, 0);
        GDALClose(ds);
        std::map<std::string, int64_t> want;
        const std::map<int, std::string> names = {{3, "Forest"}, {15, "Pasture"}, {24, "Urban"}, {33, "Water"}};
        for (int y = 0; y < H; ++y)
            for (int x = 0; x < W; ++x) {
                const int c = a[size_t(y) * W + x];
                bool near = false;
                for (int dy = -1; dy <= 1; ++dy)
                    for (int dx = -1; dx <= 1; ++dx) {
                        const int xx = x + dx, yy = y + dy;
                        near |= xx >= 0 && yy >= 0 && xx < W && yy < H && a[size_t(yy) * W + xx] == 15;
                    }
                ++want[c != 15 && near ? "buffer (1 px)" : names.at(c)];
            }
        std::map<std::string, int64_t> have;
        for (const val::Stratum& s : v.strata.strata) have[s.name] = s.pixels;
        if (have != want) return fail("the strata differ from the pixel-by-pixel count");
        bool shown = false;
        for (const SeriesLayer& L : layers_) shown |= L.inputs.size() == 1 && L.inputs[0] == v.strataPath;
        if (!shown) return fail("the strata should be shown as a layer");
        v.alloc = int(val::Alloc::Proportional);
        v.minPer = 8;
        const std::vector<int> n = val::allocate(v.strata.strata, 60, val::Alloc::Proportional, 8);
        for (size_t s = 0; s < n.size(); ++s) v.strata.strata[s].n = n[s];
        v.seed = 11;
        std::string why;
        validationDraw((fs::u8path(st_.dir) / "proj.gpkg").u8string(), validationSpec(why));
        next("5 strata as counted pixel by pixel, shown as a layer; drawing 60 points");
        break;
    }
    case 173: {
        if (v.drawJob.valid() || !v.toOpen.empty() || opening_.valid()) break;
        if (!v.designError.empty()) return fail(v.designError);
        if (!v.project) return fail("no project after the draw");
        const val::Project& P = *v.project;
        int n = 0;
        for (const val::Stratum& s : P.strata.strata) n += s.n;
        if (int(P.points.size()) != n || n < 60) return fail("the project should have the points allocated");
        const VectorLayer* V = vectorById(v.vectorId);
        if (!V || V->data->features.size() != P.points.size()) return fail("the sample should be a vector layer");
        if (!lab_.project || lab_.vector != v.vectorId || lab_.as != interpreterName() || !v.blind)
            return fail("the interpretation of the sample should start, blind");
        next("project with " + std::to_string(n) + " points, interpreted blind");
        break;
    }
    case 174: {
        const val::Project& P = *v.project;
        bool strataHidden = false, strataOpen = false;
        for (const SeriesLayer& L : layers_)
            if (L.inputs.size() == 1 && L.inputs[0] == P.strataPath) {
                strataOpen = true;
                strataHidden = !L.visible;
            }
        if (!strataOpen || !strataHidden || v.data.empty() || !v.data[0]->ready()) break;
        const val::MapRule& r = P.maps[0].rule;
        st_.hits = 0;
        for (size_t i = 0; i < P.points.size(); ++i) {
            interpGo(int(i));
            const std::string name = r.className(r.classOf(v.data[0]->values[i]));
            const auto it = std::find(lab_.classes.begin(), lab_.classes.end(), name);
            if (it == lab_.classes.end()) return fail("the label set should hold the map's classes (" + name + ")");
            int k = int(it - lab_.classes.begin());
            if (i % 5 == 0) {
                k = (k + 1) % 4;
                ++st_.hits;
            }
            interpLabel(k);
            interpSetConfidence(1 + int(i % 3));
            if (i == 2) interpAddEvent("Pasture");
        }
        if (lab_.entries[2].events.size() != 1 || lab_.entries[4].confidence != 2) return fail("events and confidence");
        saveLabels();
        const val::Prepared pp = val::prepare(P, P.maps[0], v.data[0]->values, v.data[0]->mapped, 0.95);
        const val::Assessment a = val::assess(pp.in);
        int diag = 0;
        for (size_t c = 0; c < a.classes.size(); ++c) diag += a.counts[c][c];
        if (a.n != int(P.points.size()) || diag != a.n - st_.hits) return fail("the error matrix of the labels given");
        if (!(a.overall.est > 0.5 && a.overall.est < 1)) return fail("the overall accuracy");
        next("every point labelled (" + std::to_string(st_.hits) + " wrong): overall " + fmtPm(a.overall));
        break;
    }
    case 175: {
        if (!idle()) break;
        if (!v.writeError.empty()) return fail(v.writeError);
        const val::Project& P = *v.project;
        val::Project back;
        std::string e;
        if (!val::loadProject(P.path, back, e)) return fail(e);
        const auto& mine = back.labels[interpreterName()];
        for (size_t i = 0; i < P.points.size(); ++i)
            if (i >= mine.size() || mine[i].label != lab_.entries[i].label || mine[i].confidence != lab_.entries[i].confidence)
                return fail("the labels read back differ");
        if (mine[2].events.size() != 1) return fail("the event read back");
        {
            GDALDataset* ds = GDALDataset::FromHandle(GDALOpenEx(P.path.c_str(), GDAL_OF_VECTOR, nullptr, nullptr, nullptr));
            if (!ds) return fail("the GeoPackage cannot be opened");
            OGRLayer* L = ds->ExecuteSQL("SELECT COUNT(*) FROM samples WHERE status = 'labelled'", nullptr, nullptr);
            int64_t n = -1;
            if (L)
                if (OGRFeature* f = L->GetNextFeature()) {
                    n = f->GetFieldAsInteger64(0);
                    OGRFeature::DestroyFeature(f);
                }
            if (L) ds->ReleaseResultSet(L);
            GDALClose(ds);
            if (n != int64_t(P.points.size())) return fail("the samples layer should hold the reference of every point");
        }
        // A second interpreter on a copy: the first 10 points, 3 of them otherwise.
        const std::string copy = (fs::u8path(st_.dir) / "copy.gpkg").u8string();
        std::error_code ec;
        fs::copy_file(fs::u8path(P.path), fs::u8path(copy), fs::copy_options::overwrite_existing, ec);
        std::vector<std::pair<int, val::Label>> rows;
        for (int i = 0; i < 10; ++i) {
            val::Label l = lab_.entries[i];
            if (i == 1 || i == 3 || i == 6) {
                const auto it = std::find(lab_.classes.begin(), lab_.classes.end(), l.label);
                l.label = lab_.classes[(size_t(it - lab_.classes.begin()) + 1) % 4];
            }
            l.updated = val::nowIso();
            rows.push_back({i, l});
        }
        if (!val::saveLabels(copy, "second", rows, {}, e)) return fail(e);
        std::string msg;
        if (validationMerge(copy, msg) != 10) return fail("10 labels should be merged: " + msg);
        next("the GeoPackage read back; 10 labels of a second interpreter merged");
        break;
    }
    case 176: {
        if (!idle()) break;
        if (!v.writeError.empty()) return fail(v.writeError);
        if (conflicts() != 3) return fail("3 disagreements expected, " + std::to_string(conflicts()));
        startAdjudication();
        if (lab_.as != val::kAdjudicated || lab_.filter.size() != 3) return fail("adjudication over the 3 disagreements");
        interpLabel(0);
        saveLabels();
        if (conflicts() != 2) return fail("an adjudicated point should settle");
        lab_.as = interpreterName();
        validationEntries();
        lab_.filter.clear();
        // A second map: D in 2007, assessed with the same sample.
        setT(s_->info->T() - 1);
        const auto src = Validation::sources(*this);
        const std::string last = s_->info->layers.back().path;
        const auto it = std::find_if(src.begin(), src.end(), [&](const Validation::Source& s) { return s.rule.path == last; });
        if (it == src.end()) return fail("D's last date among the maps");
        val::AssessedMap m;
        m.rule = it->rule;
        v.project->maps.push_back(m);
        v.addMapData(*this, v.project->maps.size() - 1);
        v.metaDirty = true;
        ++v.version;
        next("a disagreement adjudicated; D in 2007 added as a second map");
        break;
    }
    case 177: {
        if (v.data.size() < 2 || !v.data[1]->ready() || !idle()) break;
        val::Project& P = *v.project;
        if (P.maps[1].mapping.empty()) break; // its translations, guessed in the pump
        // Every tab of the panel drawn once (colours by stratum and right / wrong too), then on.
        showValidation_ = true;
        if (st_.frames < 10) {
            v.selectTab = st_.frames % 5;
            v.colorBy = st_.frames % 3;
            v.selected = st_.frames % 2;
            ++st_.frames;
            break;
        }
        st_.frames = 0;
        const val::Prepared pp = val::prepare(P, P.maps[1], v.data[1]->values, v.data[1]->mapped, 0.95);
        const val::Assessment a = val::assess(pp.in);
        if (a.n < int(P.points.size()) - 3) return fail("the second map: every point with a reference");
        double area = 0;
        for (const val::ClassArea& c : a.area) area += c.mapped;
        if (std::fabs(area - P.strata.totalArea()) > 1e-6 * area) return fail("the second map's areas over the strata");
        showChips_ = true;
        interpGo(0);
        next("the second map assessed (overall " + fmtPm(a.overall) + "); chips");
        break;
    }
    case 178: {
        if (!v.chipTex) break;
        if (int(v.chipDates.size()) != s_->info->T()) return fail("a chip per date expected");
        std::error_code ec;
        fs::create_directories(fs::u8path(st_.dir) / "report", ec);
        st_.jobs = {exportValidationReport((fs::u8path(st_.dir) / "report").u8string())};
        if (!st_.jobs[0]) return fail("the report did not start");
        const std::string py = val::pythonSnippet(*v.project, v.project->maps[0], 0.95);
        if (py.find("zeit.accuracy(") == std::string::npos || py.find("strata=") == std::string::npos)
            return fail("Copy Python");
        next("chips of every date; the report being written");
        break;
    }
    case 179: {
        if (st_.jobs.empty() || st_.jobs[0]->state == ExportJob::State::Running || !idle()) break;
        if (st_.jobs[0]->state != ExportJob::State::Done) return fail("the report failed: " + st_.jobs[0]->error);
        for (const char* f : {"report.html", "design.csv", "maps.csv", "map1_accuracy.csv", "map1_matrix_points.csv",
                              "map2_matrix_area.csv", "interpreters.csv"}) {
            std::error_code ec;
            if (fs::file_size(fs::u8path(st_.dir) / "report" / f, ec) == 0 || ec) return fail(std::string("the report's ") + f);
        }
        const std::string strata = v.project->strataPath;
        closeValidationProject();
        if (lab_.project || vectorById(v.vectorId)) return fail("closing the project");
        bool shown = false;
        for (const SeriesLayer& L : layers_) shown |= L.inputs.size() == 1 && L.inputs[0] == strata && L.visible;
        if (!shown) return fail("the strata layer should be shown again after the blind interpretation");
        st_.jobs.clear();
        showChips_ = false;
        next("report written; project closed");
        std::error_code ec;
        fs::remove_all(fs::u8path(st_.dir), ec);
        std::printf("OK (%.1f s)\n", now() - st_.t0);
        return 0;
    }
    }
    return -1;
}

// ---------------------------------------------------------------------------
// --shots validation DATA OUT: the screenshots of the validation guide
// ---------------------------------------------------------------------------

// site/validation.html walks through the validation of Global Forest Change's
// loss of 2018-2023 near Novo Progresso with Sentinel-2 dry-season composites
// (tools/make_tutorial_data.py): one stage per step, each saved as PNGs (the
// window, some panels alone; app_shots.cpp). The points are labelled by a rule
// on each one's NBR series (a drop from forest, and whether it grows back), not
// by a person: the numbers on the screenshots show the tool at work, they
// validate nothing.
namespace {

// The NBR, (NIR - SWIR2) / (NIR + SWIR2) (bands 4 and 6), of each composite at
// each point (x, y in the CRS `wkt`); NaN without a clear observation.
std::vector<std::vector<double>> nbrAt(const std::vector<std::string>& files, const std::string& wkt,
                                       const std::vector<val::Point>& pts) {
    std::vector<std::vector<double>> out(pts.size(), std::vector<double>(files.size(), NAN));
    OGRSpatialReference from;
    from.importFromWkt(wkt.c_str());
    from.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
    for (size_t k = 0; k < files.size(); ++k) {
        GDALDataset* ds = GDALDataset::FromHandle(GDALOpen(files[k].c_str(), GA_ReadOnly));
        if (!ds) continue;
        OGRSpatialReference to;
        to.importFromWkt(ds->GetProjectionRef());
        to.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
        OGRCoordinateTransformation* ct = OGRCreateCoordinateTransformation(&from, &to);
        double gt[6];
        ds->GetGeoTransform(gt);
        for (size_t i = 0; i < pts.size() && ct; ++i) {
            double x = pts[i].x, y = pts[i].y;
            if (!ct->Transform(1, &x, &y)) continue;
            const int col = int(std::floor((x - gt[0]) / gt[1])), row = int(std::floor((y - gt[3]) / gt[5]));
            if (col < 0 || row < 0 || col >= ds->GetRasterXSize() || row >= ds->GetRasterYSize()) continue;
            int16_t nir = 0, swir = 0;
            (void)ds->GetRasterBand(4)->RasterIO(GF_Read, col, row, 1, 1, &nir, 1, 1, GDT_Int16, 0, 0);
            (void)ds->GetRasterBand(6)->RasterIO(GF_Read, col, row, 1, 1, &swir, 1, 1, GDT_Int16, 0, 0);
            if (nir > 0 && swir > 0) out[i][k] = double(nir - swir) / double(nir + swir);
        }
        OGRCoordinateTransformation::DestroyCT(ct);
        GDALClose(ds);
    }
    return out;
}

// The centre of a pixel of `year` in the map `path` well inside a clearing of
// that year (the most pixels of it in a 9 x 9 window), in the map's CRS.
bool insideLoss(const std::string& path, int year, double& x, double& y, std::string& wkt) {
    GDALDataset* ds = GDALDataset::FromHandle(GDALOpen(path.c_str(), GA_ReadOnly));
    if (!ds) return false;
    const int W = ds->GetRasterXSize(), H = ds->GetRasterYSize();
    std::vector<int> a(size_t(W) * H);
    (void)ds->GetRasterBand(1)->RasterIO(GF_Read, 0, 0, W, H, a.data(), W, H, GDT_Int32, 0, 0);
    double gt[6];
    ds->GetGeoTransform(gt);
    wkt = ds->GetProjectionRef();
    GDALClose(ds);
    int best = 0, bx = -1, by = -1;
    for (int j = 4; j < H - 4; j += 2)
        for (int i = 4; i < W - 4; i += 2) {
            if (a[size_t(j) * W + i] != year) continue;
            int n = 0;
            for (int dy = -4; dy <= 4; ++dy)
                for (int dx = -4; dx <= 4; ++dx) n += a[size_t(j + dy) * W + i + dx] == year;
            // The most of it, then the nearest the centre.
            const int far = std::abs(i - W / 2) + std::abs(j - H / 2);
            if (n > best || (n == best && far < std::abs(bx - W / 2) + std::abs(by - H / 2))) best = n, bx = i, by = j;
        }
    if (bx < 0) return false;
    x = gt[0] + (bx + 0.5) * gt[1];
    y = gt[3] + (by + 0.5) * gt[5];
    return true;
}

// What an interpreter would call a series of yearly NBR starting in `first`
// (a class of kEventLabels: 0 stable forest, 1 deforestation, 2 degradation,
// 3 regeneration, 5 pasture, 7 no data, 8 other), the year it changed (0:
// none) and the confidence (1-3).
struct Reading {
    int label = 8, year = 0, confidence = 2;
};
Reading readSeries(const std::vector<double>& nbr, int first) {
    constexpr double kForest = 0.45;
    Reading r;
    for (double x : nbr)
        if (!std::isfinite(x)) return {7, 0, 1};
    for (size_t k = 1; k < nbr.size(); ++k) {
        const double drop = nbr[k - 1] - nbr[k];
        if (nbr[k - 1] > kForest && nbr[k] < kForest - 0.05 && drop > 0.15) {
            // Forest again two years on: the canopy opened (logging, fire) and closed.
            const bool back = k + 2 < nbr.size() && nbr[k + 2] > kForest && nbr.back() > kForest;
            return {back ? 2 : 1, first + int(k), drop > 0.3 ? 3 : drop > 0.2 ? 2 : 1};
        }
    }
    // A deep dip that lasts a year and stays above cleared land: the canopy thinned (degradation), at its first year.
    for (size_t k = 1; k + 1 < nbr.size(); ++k) {
        const double before = *std::max_element(nbr.begin(), nbr.begin() + k);
        if (before > kForest + 0.1 && before - nbr[k] > 0.15 && nbr[k] < 0.5 && before - nbr[k + 1] > 0.08)
            return {2, first + int(k), 1};
    }
    const auto [lo, hi] = std::minmax_element(nbr.begin(), nbr.end());
    if (*lo > kForest) r = {0, 0, *lo > kForest + 0.1 ? 3 : 2};
    else if (nbr.front() < kForest && nbr.back() > kForest) r = {3, 0, 2};
    else if (*hi < kForest) r = {5, 0, *hi < kForest - 0.1 ? 3 : 2};
    else r = {8, 0, 1};
    return r;
}

} // namespace

int App::shotsValidationStep(const std::string& data, const std::string& out) {
    auto fail = [&](const std::string& what) {
        std::printf("FAIL (stage %d): %s\n", shots_.stage, what.c_str());
        return 1;
    };
    auto next = [&](const std::string& done) { shotsNext(done); };
    ++shots_.frames;
    const double t = shotsTime();
    if (t > 120) return fail("timeout");
    if (!val_) val_ = std::make_shared<Validation>();
    Validation& v = *val_;
    const fs::path dataDir = fs::u8path(data), outDir = fs::u8path(out);
    const std::string gfc = (dataDir / "gfc_loss_2018_2023.tif").u8string();
    const std::string mapbiomas = (dataDir / "mapbiomas_loss_2018_2023.tif").u8string();
    ImGuiIO& io = ImGui::GetIO();
    io.AddMousePosEvent(-FLT_MAX, -FLT_MAX); // no hover, unless a stage points somewhere
    auto rectOf = [&](const char* name, bool fit = false) { return shotRect(name, fit); };
    auto shoot = [&](const std::string& name, const ShotCrops& crops = {}) { App::shoot(out, name, crops); };
    auto layerOf = [&](const std::string& path) -> SeriesLayer* {
        for (SeriesLayer& L : layers_)
            if (L.inputs.size() == 1 && L.inputs[0] == path) return &L;
        return nullptr;
    };
    auto quiet = [&] {
        if (opening_.valid() || !s_ || !s_->overview.complete()) return false;
        const SeriesLayer* L = activeLayer();
        if (L && compositeOn(*L) && !compositeReady(*L, t_)) return false;
        return !v.strataJob.valid() && !v.drawJob.valid() && v.toOpen.empty() && !v.writer.valid() && v.tasks.empty();
    };
    auto focus = [&](const char* name) {
        if (ImGuiWindow* w = ImGui::FindWindowByName(name)) ImGui::FocusWindow(w);
    };
    auto scrollValidation = [&](bool bottom) {
        if (ImGuiWindow* w = ImGui::FindWindowByName("Validation")) ImGui::SetScrollY(w, bottom ? w->ScrollMax.y : 0.0f);
    };
    // The design's key, as the Design tab makes it (or the strata would be stale).
    auto specKey = [&] {
        json m = json::array();
        for (const val::MapRule& r : v.maps) m.push_back(r.toJson());
        return m.dump() + "|" + std::to_string(v.buffer) + "|" + std::to_string(v.limit) + "|";
    };
    auto yearIndex = [&](int year) {
        for (int k = 0; k < s_->info->T(); ++k)
            if (val::yearOf(s_->info->layers[k].label) == year) return k;
        return s_->info->T() - 1;
    };

    switch (shots_.stage) {
    case 0: { // the composites, the NBR on the chart (NIR and SWIR2: bands 4 and 6)
        interpreter_ = "Ana";
        layoutPending_ = true;
        shots_.dir = (outDir / "work").u8string();
        std::error_code ec;
        fs::remove_all(fs::u8path(shots_.dir), ec);
        fs::create_directories(fs::u8path(shots_.dir), ec);
        opts_.sel = BandSelection{};
        opts_.sel.band = 4;
        opts_.sel.ndBand = 6;
        opts_.sel.chosen = true;
        openInputs({(dataDir / "s2").u8string()});
        next("the Sentinel-2 composites opening (NBR)");
        break;
    }
    case 1: { // in SWIR1 / NIR / red; the map to validate over them
        if (opening_.valid() || layers_.size() != 1 || !s_) break;
        SeriesLayer& L = layers_[0];
        L.comp = std::make_unique<BandComposite>();
        L.comp->on = true;
        L.comp->bands = {5, 4, 3};
        opts_.sel = BandSelection{};
        openInputs({gfc}, true);
        next(std::to_string(s_->info->T()) + " dates in SWIR1 / NIR / red; " + fs::u8path(gfc).filename().u8string() +
             " added as a layer");
        break;
    }
    case 2: { // the tutorial's panels: Validation and Interpretation on the right, Chips under the map
        if (opening_.valid() || layers_.size() != 2 || !dockspaceId_) break;
        shotsDock({"Validation", "Interpretation"}, {"Chips"});
        setActive(0);
        setT(s_->info->T() - 1);
        next("the tutorial's layout; the last date");
        break;
    }
    case 3: {
        if (shots_.frames == 3) {
            fitView(canvasSize_);
            viewTouched_ = true;
            // A pin inside a clearing of 2021: its series on the chart.
            double x, y, px, py;
            std::string wkt;
            if (insideLoss(gfc, 2021, x, y, wkt)) {
                OGRSpatialReference from, to;
                from.importFromWkt(wkt.c_str());
                to.importFromWkt(s_->info->crsWkt.c_str());
                from.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
                to.SetAxisMappingStrategy(OAMS_TRADITIONAL_GIS_ORDER);
                OGRCoordinateTransformation* ct = OGRCreateCoordinateTransformation(&from, &to);
                if (ct && ct->Transform(1, &x, &y) && s_->info->geoToPixel(x, y, px, py)) addPin(int(px), int(py));
                OGRCoordinateTransformation::DestroyCT(ct);
            }
        }
        if (!quiet() || t < 4) break;
        int fbw = 0, fbh = 0;
        glfwGetFramebufferSize(window_, &fbw, &fbh);
        if (fbw != kShotsWidth || fbh != kShotsHeight)
            std::printf("    note: the frame is %d x %d, not %d x %d (display scaling?)\n", fbw, fbh, kShotsWidth, kShotsHeight);
        shoot("01-series-and-map");
        next("the series and the map");
        break;
    }
    case 4: // the Validation menu, open, the pointer on its first item
        if (!shotsMenu("Validation", out, "02-menu", true)) {
            if (t > 10) return fail("the Validation menu did not open");
            break;
        }
        next("the Validation menu");
        break;
    case 5: { // Design: the map read as events in two periods, a buffer of 1 px
        if (shots_.frames == 1) {
            ImGui::ClosePopupsExceptModals();
            while (pins_.size() > 1) removePinById(pins_.back().id); // the click that opened the menu reached the map too
            showValidation_ = true;
            v.selectTab = 0;
            val::MapRule r = Validation::fileRule(gfc);
            r.kind = val::MapKind::Events;
            r.bins = {2018, 2021, 2024};
            v.maps = {r};
            v.binsText = {val::binsText(r.bins)};
            v.changeText = {""};
            v.buffer = 1;
            v.limit = 0;
            break;
        }
        if (shots_.frames == 3) focus("Validation");
        if (t < 2) break;
        shoot("03-design", {{"panel", rectOf("Validation", true)}});
        next("the design, before the strata");
        break;
    }
    case 6: { // the strata, the sample size and its allocation
        if (shots_.frames == 1) {
            std::string why;
            validationComputeStrata(validationSpec(why));
            v.strataFor = specKey();
            break;
        }
        if (v.strataJob.valid() || !v.toOpen.empty() || opening_.valid()) break;
        if (!v.designError.empty()) return fail(v.designError);
        v.alloc = int(val::Alloc::Proportional);
        v.targetSe = 0.015f;
        v.nAuto = true;
        v.minPer = 50;
        v.seed = 42;
        for (val::Stratum& s : v.strata.strata)
            s.expectedUa = s.change ? 0.8 : s.name.find("buffer") != std::string::npos ? 0.85 : 0.95;
        if (SeriesLayer* S = layerOf(v.strataPath)) S->opacity = 0.8f;
        if (!quiet() || t < 4) break;
        std::string strata;
        for (const val::Stratum& s : v.strata.strata)
            strata += "\n      " + s.name + ": " + std::to_string(s.pixels) + " px, " + std::to_string(s.n) + " points";
        shoot("04-strata", {{"panel", rectOf("Validation", true)}});
        next("the strata:" + strata);
        break;
    }
    case 7: { // the sample, drawn into a project; the interpretation starts, blind
        if (shots_.frames == 1) {
            std::string why;
            validationDraw((fs::u8path(shots_.dir) / "gfc_loss_2018_2023_validation.gpkg").u8string(), validationSpec(why));
            break;
        }
        if (v.drawJob.valid() || !v.toOpen.empty() || opening_.valid()) break;
        if (!v.designError.empty()) return fail(v.designError);
        if (!v.project || !lab_.project) return fail("no project, or its interpretation did not start");
        if (shots_.labels.empty()) {
            // What each point shows, read once (labelled in stage 9).
            const val::Project& P = *v.project;
            std::vector<std::string> files;
            for (const Layer& l : s_->info->layers) files.push_back(l.path);
            const auto nbr = nbrAt(files, P.strata.grid.wkt, P.points);
            const int first = val::yearOf(s_->info->layers[0].label);
            std::map<int, int> count;
            for (const auto& series : nbr) {
                const Reading r = readSeries(series, first);
                shots_.labels.push_back(r.label);
                shots_.years.push_back(r.year * 10 + r.confidence);
                ++count[r.label];
            }
            std::string c;
            for (const auto& [l, n] : count) c += " " + kEventLabels[l] + " " + std::to_string(n) + ";";
            std::printf("    the points read:%s\n", c.c_str());
            showValidation_ = true;
            v.selectTab = 0;
            focus("Validation");
            fitView(canvasSize_);
        }
        if (!quiet() || t < 4) break;
        shoot("05-sample", {{"panel", rectOf("Validation", true)}});
        next("the sample: " + std::to_string(v.project->points.size()) + " points, blind");
        break;
    }
    case 8: { // one point being interpreted: a clearing, at the date it shows
        if (shots_.frames == 1) {
            // A clearing in a stratum of change, seen with high confidence, in the middle of the series.
            const val::Project& P = *v.project;
            for (size_t i = 0; i < P.points.size() && shots_.example < 0; ++i) {
                const int year = shots_.years[i] / 10, conf = shots_.years[i] % 10;
                const int s = P.strata.indexOfCode(P.points[i].stratum);
                if (shots_.labels[i] == 1 && conf == 3 && year >= 2020 && year <= 2022 && s >= 0 && P.strata.strata[s].change)
                    shots_.example = int(i);
            }
            if (shots_.example < 0) return fail("no clearing in a stratum of change to show");
            clearPins(); // the first screenshot's
            showInterp_ = true;
            showChips_ = true;
            lab_.zoom = 6;
            interpGo(shots_.example);
            shots_.exampleT = yearIndex(shots_.years[shots_.example] / 10);
            setT(shots_.exampleT);
            break;
        }
        if (shots_.frames == 3) focus("Interpretation");
        if (shots_.frames == 4) focus("Chips");
        if (t < 3) break;
        if (lab_.entries[shots_.example].label.empty()) {
            interpLabel(1);
            interpSetDate(false);
            interpSetConfidence(3);
            break;
        }
        if (!quiet() || !v.chipTex || t < 5) break;
        shoot("06-interpretation",
              {{"panel", rectOf("Interpretation")}, {"chips", rectOf("Chips")}, {"chart", rectOf("Time series")}});
        next("point " + std::to_string(shots_.example + 1) + " interpreted: deforestation in " +
             s_->info->layers[shots_.exampleT].label);
        break;
    }
    case 9: { // every point labelled (by the rule), then the accuracy
        if (shots_.frames == 1) {
            // In a random order, a label every 40 s or so (the order the convergence chart follows).
            const val::Project& P = *v.project;
            std::vector<int> order(P.points.size());
            for (size_t i = 0; i < order.size(); ++i) order[i] = int(i);
            std::mt19937 rng(7);
            std::shuffle(order.begin(), order.end(), rng);
            int k = 0;
            for (const int i : order) {
                if (i == shots_.example) continue;
                lab_.current = i;
                interpLabel(shots_.labels[i]);
                const int year = shots_.years[i] / 10;
                lab_.entries[i].date = year ? s_->info->layers[yearIndex(year)].label : "";
                interpSetConfidence(shots_.years[i] % 10);
                const int s = 9 * 3600 + 40 * ++k;
                char when[32];
                std::snprintf(when, sizeof(when), "2026-10-%02dT%02d:%02d:%02dZ", 1 + s / 86400, s / 3600 % 24, s / 60 % 60, s % 60);
                lab_.entries[i].updated = when;
                validationLabelsChanged(i);
            }
            saveLabels();
            lab_.current = shots_.example;
            showChips_ = false;
            v.selectTab = 1;
            v.sections = 0;
            break;
        }
        if (shots_.frames == 3) focus("Validation");
        if (!quiet() || t < 3) break;
        shoot("07-accuracy", {{"panel", rectOf("Validation", true)}});
        next("every point labelled; the accuracy");
        break;
    }
    case 10: { // how the labels read for this map
        v.sections = 1;
        if (t < 2) break;
        shoot("-", {{"08-translations", rectOf("Validation", true)}});
        next("the translations of the labels");
        break;
    }
    case 11: { // the estimates as the labels came, and the dates
        v.sections = 2 | 4;
        if (shots_.frames > 3) scrollValidation(true);
        if (t < 2) break;
        shoot("-", {{"09-convergence-dates", rectOf("Validation", true)}});
        next("the convergence and the dates");
        break;
    }
    case 12: { // the area of change by year
        v.sections = 8;
        if (shots_.frames > 3) scrollValidation(true);
        if (t < 2) break;
        shoot("-", {{"10-by-year", rectOf("Validation", true)}});
        next("the area of change by year");
        break;
    }
    case 13: { // not blind any more: the map and the points right or wrong
        if (shots_.frames == 1) {
            v.sections = 0;
            scrollValidation(false);
            setValidationBlind(false);
            v.colorBy = 2;
            if (SeriesLayer* S = layerOf(v.project->strataPath)) S->visible = false;
            if (lab_.pinId) removePinById(lab_.pinId);
            lab_.pinId = 0;
            break;
        }
        if (shots_.frames == 3) {
            fitView(canvasSize_);
            setT(s_->info->T() - 1);
        }
        if (!quiet() || t < 4) break;
        shoot("11-right-wrong");
        next("the points right or wrong over the map");
        break;
    }
    case 14: { // a second map, assessed with the same sample
        if (shots_.frames == 1) {
            val::AssessedMap m;
            m.rule = Validation::fileRule(mapbiomas);
            m.rule.kind = val::MapKind::Events;
            m.rule.bins = {2018, 2021, 2024};
            v.project->maps.push_back(m);
            v.addMapData(*this, v.project->maps.size() - 1);
            v.metaDirty = true;
            ++v.version;
            v.selectTab = 2;
            break;
        }
        if (v.data.size() < 2 || !v.data[1]->ready() || v.project->maps[1].mapping.empty() || !quiet() || t < 3) break;
        shoot("-", {{"12-maps", rectOf("Validation", true)}});
        next("MapBiomas assessed with the same sample");
        break;
    }
    case 15: { // a second interpreter's labels, merged
        if (shots_.frames == 1) {
            const val::Project& P = *v.project;
            const std::string copy = (fs::u8path(shots_.dir) / "gfc_loss_2018_2023_validation_bruno.gpkg").u8string();
            if (v.writer.valid()) v.writer.wait();
            std::error_code ec;
            fs::copy_file(fs::u8path(P.path), fs::u8path(copy), fs::copy_options::overwrite_existing, ec);
            // Bruno labels 60 points across the strata, and reads a few clearings as degradation, some pasture as
            // agriculture.
            std::vector<std::pair<int, val::Label>> rows;
            int changed = 0;
            const int n = int(P.points.size());
            for (int k = 0; k < 60 && k < n; ++k) {
                const int i = k * n / 60;
                val::Label l = lab_.entries[i];
                if (changed < 5 && k % 2 == 1 && (shots_.labels[i] == 1 || shots_.labels[i] == 5)) {
                    l.label = shots_.labels[i] == 1 ? "Degradation" : "Agriculture";
                    ++changed;
                }
                l.updated = val::nowIso();
                rows.push_back({i, l});
            }
            std::string e;
            if (!val::saveLabels(copy, "Bruno", rows, {}, e)) return fail(e);
            validationMerge(copy, v.message);
            v.selectTab = 3;
            break;
        }
        if (!quiet() || t < 3) break;
        shoot("-", {{"13-interpreters", rectOf("Validation", true)}});
        next("Bruno's labels merged: " + v.message);
        break;
    }
    case 16: { // adjudication
        if (shots_.frames == 1) {
            v.message.clear();
            setValidationBlind(true);
            startAdjudication();
            if (lab_.filter.empty()) return fail("no disagreement to adjudicate");
            break;
        }
        if (shots_.frames == 3) focus("Interpretation");
        if (!quiet() || t < 4) break;
        shoot("14-adjudication", {{"panel", rectOf("Interpretation")}});
        // Each disagreement settled as Ana read it.
        for (int i : lab_.filter) {
            lab_.current = i;
            interpLabel(shots_.labels[i]);
            const int year = shots_.years[i] / 10;
            lab_.entries[i].date = year ? s_->info->layers[yearIndex(year)].label : "";
            interpSetConfidence(shots_.years[i] % 10);
        }
        saveLabels();
        lab_.as = interpreterName();
        validationEntries();
        lab_.filter.clear();
        lab_.filterName.clear();
        next("the disagreements adjudicated");
        break;
    }
    case 17: { // the report
        if (shots_.frames == 1) {
            setValidationBlind(false);
            const fs::path dir = fs::u8path(shots_.dir) / "report";
            std::error_code ec;
            fs::create_directories(dir, ec);
            shots_.report = exportValidationReport(dir.u8string());
            if (!shots_.report) return fail("the report did not start");
            v.selectTab = 4;
            break;
        }
        if (shots_.frames == 3) focus("Validation");
        showExports_ = false;
        if (shots_.report->state == ExportJob::State::Running || !quiet() || t < 3) break;
        if (shots_.report->state != ExportJob::State::Done) return fail("the report failed: " + shots_.report->error);
        shoot("-", {{"15-report", rectOf("Validation", true)}});
        next("the report: " + (fs::u8path(shots_.dir) / "report" / "report.html").u8string());
        break;
    }
    case 18:
        closeValidationProject();
        std::printf("OK\n");
        return 0;
    }
    return -1;
}
