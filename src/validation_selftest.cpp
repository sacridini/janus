// --selftest-validation (validation.hpp): the estimators against the example
// of Olofsson et al. (2014) (numbers of zeit.accuracy, which match the
// paper's), the design against zeit.sampling_design, the strata of synthetic
// maps (buffer, limit, a second map on another grid, bands of a few rows)
// against a pixel-by-pixel count, the sample, the project's file, and, with
// the runtime's Python, the estimates of a labelled project against
// zeit.accuracy run on its files (the code of "Copy Python").
#include "validation.hpp"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <set>
#include <thread>

#include <gdal_priv.h>
#include <ogr_spatialref.h>

#include "platform.hpp"

namespace fs = std::filesystem;

namespace val {

int g_testBandRows = 0; // validation.cpp: rows per band when > 0 (bands with halos in the test)

namespace {

bool near(double a, double b, double tol) {
    return std::fabs(a - b) <= tol * std::max(1.0, std::fabs(b));
}

std::string utm23s() {
    OGRSpatialReference s;
    s.importFromEPSG(32723);
    char* w = nullptr;
    s.exportToWkt(&w);
    std::string r = w;
    CPLFree(w);
    return r;
}

bool writeRaster(const std::string& path, int w, int h, const double gt[6], const std::vector<float>& v, double nodata) {
    GDALDriver* d = GetGDALDriverManager()->GetDriverByName("GTiff");
    GDALDataset* ds = d->Create(path.c_str(), w, h, 1, GDT_Float32, nullptr);
    if (!ds) return false;
    ds->SetGeoTransform(const_cast<double*>(gt));
    ds->SetProjection(utm23s().c_str());
    GDALRasterBand* b = ds->GetRasterBand(1);
    if (!std::isnan(nodata)) b->SetNoDataValue(nodata);
    const bool ok = b->RasterIO(GF_Write, 0, 0, w, h, const_cast<float*>(v.data()), w, h, GDT_Float32, 0, 0) == CE_None;
    GDALClose(ds);
    return ok;
}

std::vector<float> readRaster(const std::string& path, int& w, int& h) {
    GDALDataset* ds = GDALDataset::FromHandle(GDALOpen(path.c_str(), GA_ReadOnly));
    if (!ds) return {};
    w = ds->GetRasterXSize();
    h = ds->GetRasterYSize();
    std::vector<float> v(size_t(w) * h);
    ds->GetRasterBand(1)->RasterIO(GF_Read, 0, 0, w, h, v.data(), w, h, GDT_Float32, 0, 0);
    GDALClose(ds);
    return v;
}

// Olofsson et al. (2014), tables 8-10 (zeit.accuracy's numbers).
const char* testOlofsson(std::string& rep) {
    const double px[4] = {200000, 150000, 3200000, 6450000};
    const int table[4][4] = {{66, 0, 5, 4}, {0, 55, 8, 12}, {1, 0, 153, 11}, {2, 1, 9, 313}};
    AssessInput in;
    in.classes = {"1", "2", "3", "4"};
    for (double p : px) {
        in.stratumArea.push_back(p * 0.09);
        in.mapped.push_back(p * 0.09);
    }
    for (int i = 0; i < 4; ++i)
        for (int j = 0; j < 4; ++j)
            for (int k = 0; k < table[i][j]; ++k) {
                in.stratum.push_back(i);
                in.map.push_back(i);
                in.ref.push_back(j);
            }
    const Assessment a = assess(in);
    char b[256];
    std::snprintf(b, sizeof(b), "  Olofsson et al. (2014): overall %.4f +- %.4f, deforestation %.0f +- %.0f ha\n",
                  a.overall.est, a.overall.hi - a.overall.est, a.area[0].est, a.area[0].ci);
    rep += b;
    const double t = 1e-8;
    if (a.n != 640) return "Olofsson: 640 points expected";
    if (!near(a.overall.est, 0.9465118881, t) || !near(a.overall.se, 0.0094304172, t)) return "Olofsson: overall accuracy";
    if (!near(a.users[1].est, 0.7333333333, t) || !near(a.users[1].se, 0.0514066401, t)) return "Olofsson: user's accuracy";
    if (!near(a.producers[0].est, 0.7486614048, t) || !near(a.producers[0].se, 0.1088315576, t))
        return "Olofsson: producer's accuracy";
    if (!near(a.producers[1].hi, 1.1015600840, t)) return "Olofsson: producer's interval";
    if (!near(a.area[0].est, 21157.7622377622, t) || !near(a.area[0].hi, 27315.2834758526, t)) return "Olofsson: area";
    if (!near(a.area[3].lo, 565104.7966731654, t)) return "Olofsson: area of stable non-forest";
    if (!near(a.confusion[3][0], 0.0039692308, 1e-7) || !near(a.confusion[0][0], 0.0176, 1e-9)) return "Olofsson: matrix";
    if (!near(zOf(0.95), 1.959963984540054, 1e-12) || !near(zOf(0.9), 1.6448536269514722, 1e-12)) return "normal quantile";

    // The design (zeit.sampling_design on the same map).
    std::vector<Stratum> s(4);
    const double ua[4] = {0.7, 0.6, 0.9, 0.95};
    for (int i = 0; i < 4; ++i) {
        s[i].pixels = int64_t(px[i]);
        s[i].area = px[i] * 0.09;
        s[i].weight = px[i] / 1e7;
        s[i].expectedUa = ua[i];
        s[i].change = i < 2;
    }
    if (sampleSize(s, 0.01) != 641) return "sample size (eq. 13)";
    const struct {
        Alloc a;
        int n, min;
        std::vector<int> want;
    } cases[] = {{Alloc::Proportional, 641, 50, {50, 50, 179, 362}}, {Alloc::Equal, 641, 0, {161, 160, 160, 160}},
                 {Alloc::Neyman, 641, 0, {23, 19, 243, 356}},        {Alloc::Proportional, 641, 100, {100, 100, 146, 295}},
                 {Alloc::Neyman, 641, 75, {75, 75, 199, 292}},       {Alloc::Proportional, 300, 100, {100, 100, 100, 193}}};
    for (const auto& c : cases)
        if (allocate(s, c.n, c.a, c.min) != c.want) {
            std::snprintf(b, sizeof(b), "allocation %s, n %d, at least %d", allocName(c.a), c.n, c.min);
            rep += std::string("  ") + b + "\n";
            return "allocation differs from zeit.sampling_design";
        }
    for (int i = 0; i < 4; ++i) s[i].n = allocate(s, 641, Alloc::Proportional, 50)[i];
    const Expected e = expectedErrors(s, 900000, 0.95);
    if (!(e.overallSe > 0.005 && e.overallSe < 0.02) || !e.change) return "expected errors";

    // Interpreters and dates.
    const Agreement g = cohen({"y", "y", "n", "n", "y", ""}, {"y", "n", "n", "n", "y", "n"});
    if (g.n != 5 || !near(g.observed, 0.8, 1e-12) || !near(g.kappa, 0.32 / 0.52, 1e-12)) return "Cohen's kappa";
    const Estimator est({1, 1}, {0, 0, 1, 1}, 0.95);
    const DateAgreement d = dateAgreement(est, {2010, 2012, 2015, 0}, {2010, 2010, 2016, 2016}, 1);
    if (d.n != 3 || !near(d.within.est, 2.0 / 3.0, 1e-12) || d.diffs.at(2) != 1) return "date agreement";
    MapRule named;
    named.kind = MapKind::Intervals;
    named.bins = {-1, -0.1, 1};
    named.names = {{3, "Forest"}, {15, "Pasture"}};
    named.change = {1};
    const MapRule back = MapRule::fromJson(json::parse(named.toJson().dump()));
    if (back.names != named.names || back.bins != named.bins || back.change != named.change || back.kind != named.kind)
        return "a map's rule written and read back";
    if (yearOf("2019-07-14") != 2019 || yearOf("2005") != 2005 || yearOf("12345") != 0 || yearOf("") != 0) return "years of dates";
    return nullptr;
}

} // namespace

const char* selfTest(std::string& rep, const std::string& python) {
    if (const char* e = testOlofsson(rep)) return e;

    // Synthetic maps: A, years of change (0 none, NaN no data in a corner), 30 m in UTM 23S;
    // B, classes at 60 m whose grid starts 10 m off A's.
    const fs::path dir = fs::temp_directory_path() / "janus-selftest-validation";
    std::error_code ec;
    fs::remove_all(dir, ec);
    fs::create_directories(dir, ec);
    const int W = 173, H = 131;
    const double gtA[6] = {500000, 30, 0, 7500000, 0, -30};
    std::vector<float> a(size_t(W) * H, 0.0f);
    for (int y = 0; y < H; ++y)
        for (int x = 0; x < W; ++x) {
            float& v = a[size_t(y) * W + x];
            if (x < 6 && y < 5) v = NAN;
            else if ((x - 40) * (x - 40) + (y - 30) * (y - 30) < 120) v = 2004;
            else if (x > 100 && x < 130 && y > 70 && y < 80) v = 2015;
            else if ((x * 7 + y * 13) % 97 == 0) v = float(2001 + (x + y) % 20);
        }
    const int WB = 90, HB = 70;
    const double gtB[6] = {500010, 60, 0, 7500010, 0, -60};
    std::vector<float> bm(size_t(WB) * HB);
    for (int y = 0; y < HB; ++y)
        for (int x = 0; x < WB; ++x) bm[size_t(y) * WB + x] = float(1 + (x / 9 + y / 7) % 3);
    const std::string pa = (dir / "a.tif").u8string(), pb = (dir / "b.tif").u8string();
    if (!writeRaster(pa, W, H, gtA, a, NAN) || !writeRaster(pb, WB, HB, gtB, bm, 0)) return "cannot write the test maps";

    MapRule ra;
    ra.path = pa;
    ra.kind = MapKind::Events;
    ra.bins = {2000, 2010, 2021};
    MapRule rb;
    rb.path = pb;
    rb.kind = MapKind::Classes;
    rb.change = {2};

    // The expected strata, pixel by pixel.
    auto bAt = [&](int x, int y) -> double { // B under A's pixel centre
        const double gx = gtA[0] + (x + 0.5) * gtA[1], gy = gtA[3] + (y + 0.5) * gtA[5];
        const int bx = int(std::floor((gx - gtB[0]) / gtB[1])), by = int(std::floor((gy - gtB[3]) / gtB[5]));
        if (bx < 0 || by < 0 || bx >= WB || by >= HB) return NAN;
        return bm[size_t(by) * WB + bx];
    };
    Polygon limit = {{3.2, 2.7, 168.4, 9.1, 150.3, 128.6, 10.7, 101.2}};
    struct Case {
        const char* name;
        std::vector<MapRule> maps;
        int buffer;
        bool limit;
    };
    const Case cases[] = {{"events in periods, buffer of 2 px, inside a polygon", {ra}, 2, true},
                          {"agreement of two maps on different grids, buffer of 1 px", {ra, rb}, 1, false}};
    Strata kept;
    std::string keptPath;
    for (const Case& c : cases) {
        StrataSpec spec;
        spec.maps = c.maps;
        spec.buffer = c.buffer;
        if (c.limit) spec.limit = limit;
        const std::string out = (dir / (std::string("strata_") + std::to_string(&c - cases) + ".tif")).u8string();
        Strata s;
        std::string err;
        g_testBandRows = 7;
        const bool ok = computeStrata(spec, out, s, nullptr, err);
        g_testBandRows = 0;
        if (!ok) {
            rep += "  " + err + "\n";
            return "computeStrata failed";
        }
        // Expected key and change per pixel.
        const int M = int(c.maps.size());
        std::vector<int> key(size_t(W) * H, kNone);
        std::vector<char> chg(size_t(W) * H, 0);
        for (int y = 0; y < H; ++y)
            for (int x = 0; x < W; ++x) {
                const size_t i = size_t(y) * W + x;
                if (M == 1) {
                    key[i] = ra.classOf(a[i]);
                    chg[i] = ra.isChange(key[i]);
                } else {
                    const int ka = ra.classOf(a[i]), kb = rb.classOf(bAt(x, y));
                    if (ka == kNone || kb == kNone) continue;
                    key[i] = ra.isChange(ka) + rb.isChange(kb);
                    chg[i] = key[i] > 0;
                }
            }
        int w2 = 0, h2 = 0;
        const std::vector<float> got = readRaster(out, w2, h2);
        if (w2 != W || h2 != H) return "the strata raster has another size";
        std::map<std::string, int64_t> want, have;
        for (const Stratum& t : s.strata) have[t.name] = t.pixels;
        int64_t bad = 0;
        for (int y = 0; y < H; ++y)
            for (int x = 0; x < W; ++x) {
                const size_t i = size_t(y) * W + x;
                std::string name;
                const bool in = !c.limit || pointInPolygon(limit, x + 0.5, y + 0.5);
                if (key[i] != kNone && in) {
                    bool dil = false;
                    for (int dy = -c.buffer; dy <= c.buffer && !dil; ++dy)
                        for (int dx = -c.buffer; dx <= c.buffer && !dil; ++dx) {
                            const int xx = x + dx, yy = y + dy;
                            dil = xx >= 0 && yy >= 0 && xx < W && yy < H && chg[size_t(yy) * W + xx];
                        }
                    if (c.buffer && !chg[i] && dil) name = "buffer (" + std::to_string(c.buffer) + " px)";
                    else if (M == 1) name = ra.className(key[i]);
                    else name = key[i] == 0 ? "no change in any map" : key[i] == 2 ? "change in all 2 maps" : "change in 1 of 2 maps";
                    ++want[name];
                }
                const int code = int(got[i]);
                const int k = code ? s.indexOfCode(code) : -1;
                const std::string gotName = k >= 0 ? s.strata[k].name : "";
                bad += gotName != name;
            }
        char b[200];
        std::snprintf(b, sizeof(b), "  strata, %s: %zu strata, %lld pixel(s) differ\n", c.name, s.strata.size(),
                      (long long)bad);
        rep += b;
        if (bad) return "the strata raster differs from the pixel-by-pixel count";
        if (want != have) return "the strata's pixels differ from the pixel-by-pixel count";
        for (const Stratum& t : s.strata)
            if (!near(t.area, t.pixels * 0.09, 1e-9)) return "the strata's areas (30 m pixels: 0.09 ha)";
        double mappedTotal = 0;
        for (const auto& [k, v] : s.mapped[0]) mappedTotal += v;
        if (!near(mappedTotal, s.totalArea(), 1e-9)) return "the mapped areas do not add up to the strata's";
        if (c.limit) {
            kept = s;
            keptPath = out;
        }
    }

    // The sample: the counts asked, each point in its stratum, no pixel twice, the same again with the same seed;
    // then more points, none of the first.
    std::vector<int> n;
    for (const Stratum& t : kept.strata) n.push_back(int(std::min<int64_t>(t.pixels, 25)));
    std::vector<Point> p1, p2, p3;
    std::string err;
    if (!drawSample(kept, keptPath, n, 7, {}, p1, err) || !drawSample(kept, keptPath, n, 7, {}, p2, err)) {
        rep += "  " + err + "\n";
        return "drawSample failed";
    }
    int w2 = 0, h2 = 0;
    const std::vector<float> st = readRaster(keptPath, w2, h2);
    auto checkPoints = [&](const std::vector<Point>& pts, std::set<std::pair<int, int>>& seen) -> const char* {
        for (const Point& q : pts) {
            if (int(st[size_t(q.row) * W + q.col]) != q.stratum) return "a point is not in its stratum";
            if (!seen.insert({q.row, q.col}).second) return "a pixel drawn twice";
            double x, y;
            kept.grid.centre(q.col, q.row, x, y);
            if (x != q.x || y != q.y) return "a point is not at its pixel's centre";
        }
        return nullptr;
    };
    std::set<std::pair<int, int>> seen;
    if (const char* e = checkPoints(p1, seen)) return e;
    if (p1.size() != p2.size()) return "the same seed gave another sample";
    for (size_t i = 0; i < p1.size(); ++i)
        if (p1[i].row != p2[i].row || p1[i].col != p2[i].col || p1[i].id != int(i) + 1) return "the same seed gave another sample";
    std::map<int, int> per;
    for (const Point& q : p1) ++per[q.stratum];
    for (size_t i = 0; i < kept.strata.size(); ++i)
        if (per[kept.strata[i].code] != n[i]) return "a stratum got another number of points";
    std::vector<int> more(kept.strata.size(), 0);
    more[0] = 10;
    if (!drawSample(kept, keptPath, more, 7, p1, p3, err)) return "drawSample (more points) failed";
    if (p3.size() != 10 || p3.front().id != int(p1.size()) + 1) return "more points: 10, with the ids after the first";
    if (const char* e = checkPoints(p3, seen)) return e;
    {
        Strata counted = kept;
        counted.rowCounts.clear();
        if (!countStrata(keptPath, counted, nullptr, err) || counted.rowCounts != kept.rowCounts)
            return "countStrata differs from computeStrata";
    }
    rep += "  sample: " + std::to_string(p1.size()) + " points, the same with the same seed, 10 more drawn apart\n";

    // The project: written, read back, labelled by two interpreters, adjudicated, merged.
    Project pr;
    pr.path = (dir / "project.gpkg").u8string();
    pr.strataPath = keptPath;
    pr.id = newProjectId();
    pr.spec.maps = {ra};
    pr.spec.buffer = 2;
    pr.spec.limit = limit;
    pr.strata = kept;
    pr.strata.rowCounts.clear();
    pr.seed = 7;
    pr.design = {{"alloc", "proportional"}};
    pr.labelSet = {"Stable", "Deforestation", "Can't tell"};
    pr.points = p1;
    pr.points.insert(pr.points.end(), p3.begin(), p3.end());
    AssessedMap am;
    am.rule = ra;
    am.mapping = defaultMapping(ra, pr.labelSet, {});
    pr.maps = {am};
    if (am.mapping["Deforestation"] != kChange || am.mapping["Stable"] != kNoChange || am.mapping["Can't tell"] != "")
        return "the default translation of labels for a map of events";
    if (!createProject(pr, err)) {
        rep += "  " + err + "\n";
        return "createProject failed";
    }
    // Labels: the map's own class, every 6th point the other way; "ana" all, "bruno" a few, one disagreement.
    std::vector<double> va;
    {
        std::vector<std::array<double, 2>> xy;
        for (const Point& q : pr.points) xy.push_back({q.x, q.y});
        if (!valuesAt(ra, kept.grid.wkt, xy, va, err)) return "valuesAt failed";
        for (size_t i = 0; i < pr.points.size(); ++i) {
            const float v = a[size_t(pr.points[i].row) * W + pr.points[i].col];
            if (!(std::isnan(v) ? std::isnan(va[i]) : va[i] == v)) return "valuesAt: not the map's value";
        }
    }
    std::vector<std::pair<int, Label>> ana, bruno;
    for (size_t i = 0; i < pr.points.size(); ++i) {
        const bool change = ra.isChange(ra.classOf(va[i]));
        Label l;
        l.label = (change != (i % 6 == 0)) ? "Deforestation" : "Stable";
        if (l.label == "Deforestation") l.date = std::to_string(change ? int(va[i]) : 2012) + "-08-01";
        if (i == 3) l.label = "Can't tell";
        l.confidence = 1 + int(i % 3);
        if (i == 5) l.events.push_back({"Deforestation", "2018"});
        l.updated = nowIso();
        ana.push_back({int(i), l});
        if (i < 12) {
            Label m = l;
            if (i == 1) m.label = m.label == "Stable" ? "Deforestation" : "Stable";
            bruno.push_back({int(i), m});
        }
    }
    auto finals = [&](const Project& q) {
        std::vector<FinalRef> f;
        for (size_t i = 0; i < q.points.size(); ++i) {
            FinalRef r;
            r.point = int(i);
            bool conflict = false;
            if (const Label* L = resolve(q.labels, i, conflict)) r.ref = *L;
            r.conflict = conflict;
            f.push_back(r);
        }
        return f;
    };
    Project q = pr;
    for (const auto& [i, l] : ana) (q.labels["ana"].resize(q.points.size()), q.labels["ana"][i] = l);
    for (const auto& [i, l] : bruno) (q.labels["bruno"].resize(q.points.size()), q.labels["bruno"][i] = l);
    if (!saveLabels(pr.path, "ana", ana, {}, err) || !saveLabels(pr.path, "bruno", bruno, finals(q), err)) {
        rep += "  " + err + "\n";
        return "saveLabels failed";
    }
    Project back;
    if (!isProject(pr.path) || isProject(pa)) return "isProject";
    if (!loadProject(pr.path, back, err)) {
        rep += "  " + err + "\n";
        return "loadProject failed";
    }
    if (back.points.size() != pr.points.size() || back.strata.strata.size() != pr.strata.strata.size() ||
        back.id != pr.id || back.seed != 7 || back.labelSet != pr.labelSet || back.maps.size() != 1 ||
        back.maps[0].mapping != am.mapping || back.spec.limit != limit || back.spec.buffer != 2 ||
        fs::u8path(back.strataPath) != fs::u8path(keptPath) || back.strata.mapped.size() != 1)
        return "the project read back differs";
    for (size_t i = 0; i < pr.points.size(); ++i)
        if (back.points[i].row != pr.points[i].row || back.points[i].stratum != pr.points[i].stratum ||
            back.points[i].x != pr.points[i].x)
            return "a point read back differs";
    if (back.labels.size() != 2 || back.labels["ana"][5].events.size() != 1 || back.labels["ana"][7].confidence != 2 ||
        back.labels["bruno"][0].label != ana[0].second.label)
        return "the labels read back differ";
    bool conflict = false;
    if (resolve(back.labels, 1, conflict) || !conflict) return "a disagreement should be a conflict";
    back.labels[kAdjudicated].resize(back.points.size());
    back.labels[kAdjudicated][1] = ana[1].second;
    if (resolve(back.labels, 1, conflict) != &back.labels[kAdjudicated][1]) return "the adjudicated label should settle it";
    std::map<std::string, std::vector<Label>> other;
    std::string otherId;
    if (!readLabels(pr.path, back.points.size(), otherId, other, err) || otherId != pr.id || other["bruno"][11].label.empty())
        return "readLabels (merge) failed";
    rep += "  project: " + std::to_string(back.points.size()) + " points, 2 interpreters, written and read back\n";

    // The estimates of the labelled project, then (with Python) zeit.accuracy on its files.
    back.labels.erase(kAdjudicated);
    std::vector<double> vb;
    std::map<int, double> mappedA;
    if (!mappedAreas(ra, keptPath, back.strata, mappedA, nullptr, err)) return "mappedAreas failed";
    if (mappedA != back.strata.mapped[0]) {
        for (const auto& [k, v] : mappedA)
            if (!near(v, back.strata.mapped[0][k], 1e-9)) return "mappedAreas differs from the strata's pass";
    }
    struct Check {
        const char* name;
        AssessedMap m;
    };
    AssessedMap periods = am, plain = am, classes;
    plain.rule.bins.clear();
    classes.rule = rb;
    classes.mapping = {{"Stable", "1"}, {"Deforestation", "2"}, {"Can't tell", ""}};
    const Check checks[] = {{"events in periods", periods}, {"events", plain}, {"classes (another grid)", classes}};
    for (const Check& c : checks) {
        std::vector<std::array<double, 2>> xy;
        for (const Point& q2 : back.points) xy.push_back({q2.x, q2.y});
        std::vector<double> v;
        if (!valuesAt(c.m.rule, back.strata.grid.wkt, xy, v, err)) return "valuesAt failed";
        std::map<int, double> mp;
        if (!mappedAreas(c.m.rule, keptPath, back.strata, mp, nullptr, err)) return "mappedAreas failed";
        const Prepared pp = prepare(back, c.m, v, mp, 0.95);
        const Assessment as = assess(pp.in);
        char b[256];
        std::snprintf(b, sizeof(b), "  %s: %d points, overall %.4f +- %.4f (%d conflict, %d left out, %d without date)\n",
                      c.name, as.n, as.overall.est, as.overall.hi - as.overall.est, pp.conflicts, pp.excluded, pp.noDate);
        rep += b;
        if (as.n < 50 || pp.conflicts != 1 || pp.excluded != 1) return "the labelled project's points";
        if (python.empty()) continue;
        std::error_code e2;
        if (!fs::exists(fs::u8path(python), e2)) {
            rep += "  (no Python runtime: zeit.accuracy not compared)\n";
            continue;
        }
        const std::string code = pythonSnippet(back, c.m, 0.95) +
                                 "import json\n"
                                 "print(\"JANUS_CHECK \" + json.dumps({\"n\": int(acc.n), \"overall\": float(acc.overall.estimate),"
                                 " \"se\": float(acc.overall.se), \"area\": {str(k): [float(v), float(s)] for k, v, s in"
                                 " zip(acc.area.index, acc.area.estimate, acc.area.se)}}))\n";
        const std::string script = (dir / "check.py").u8string(), log = (dir / "check.txt").u8string();
        {
            std::ofstream f(fs::u8path(script), std::ios::binary);
            f << code;
        }
        platform::ChildProcess proc;
        if (!proc.start({python, "-I", script}, log, err)) return "cannot run Python";
        const int rc = proc.finish();
        std::ifstream f(fs::u8path(log), std::ios::binary);
        const std::string text((std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
        const size_t k = text.find("JANUS_CHECK ");
        if (rc != 0 || k == std::string::npos) {
            rep += "  Python said:\n" + text + "\n  code:\n" + code + "\n";
            return "zeit.accuracy did not run";
        }
        const json j = json::parse(text.substr(k + 12, text.find('\n', k) - k - 12));
        if (j["n"].get<int>() != as.n || !near(j["overall"].get<double>(), as.overall.est, 1e-9) ||
            !near(j["se"].get<double>(), as.overall.se, 1e-9)) {
            rep += "  zeit: " + j.dump() + "\n";
            return "zeit.accuracy gives another overall accuracy";
        }
        for (size_t ci = 0; ci < as.classes.size(); ++ci) {
            if (!j["area"].contains(as.classes[ci])) {
                if (as.area[ci].est == 0) continue;
                rep += "  zeit: " + j.dump() + "\n";
                return "zeit.accuracy has not every class";
            }
            const auto z = j["area"][as.classes[ci]];
            if (!near(z[0].get<double>(), as.area[ci].est, 1e-9) || !near(z[1].get<double>(), as.area[ci].se, 1e-9)) {
                rep += "  class " + as.classes[ci] + ", zeit: " + z.dump() + "\n";
                return "zeit.accuracy gives another area";
            }
        }
        rep += "    the same as zeit.accuracy on the project's files\n";
    }
    const auto years = changeByYear(back, plain, 0.95);
    double sum = 0;
    for (const auto& [y, ar] : years) sum += ar.est;
    if (years.empty() || !(sum > 0)) return "the area of change by year";
    if (platform::getEnv("JANUS_KEEP_VALIDATION").empty()) fs::remove_all(dir, ec);
    return nullptr;
}

int measure(const std::string& path) {
    using Clock = std::chrono::steady_clock;
    auto ms = [](Clock::time_point t0) { return std::chrono::duration<double, std::milli>(Clock::now() - t0).count(); };
    MapRule r;
    r.path = path;
    GDALDataset* ds = GDALDataset::FromHandle(GDALOpen(path.c_str(), GA_ReadOnly));
    if (!ds) {
        std::printf("cannot open %s\n", path.c_str());
        return 1;
    }
    const bool floating = GDALDataTypeIsFloating(ds->GetRasterBand(1)->GetRasterDataType());
    const int W = ds->GetRasterXSize(), H = ds->GetRasterYSize();
    GDALClose(ds);
    r.kind = floating ? MapKind::Events : MapKind::Classes;
    if (!floating) r.change = {1};
    StrataSpec spec;
    spec.maps = {r};
    spec.buffer = 1;
    const std::string out = (fs::temp_directory_path() / "janus-measure-strata.tif").u8string();
    Strata s;
    std::string err;
    Progress prog;
    prog.threads = int(std::max(1u, std::thread::hardware_concurrency()));
    auto t0 = Clock::now();
    if (!computeStrata(spec, out, s, &prog, err)) {
        std::printf("%s\n", err.c_str());
        return 1;
    }
    std::printf("%d x %d (%.0f Mpx, %s, %d threads): strata in %.0f ms (%zu strata)\n", W, H, double(W) * H / 1e6,
                kindName(r.kind), prog.threads, ms(t0), s.strata.size());
    t0 = Clock::now();
    std::map<int, double> area;
    if (!mappedAreas(r, out, s, area, &prog, err)) return 1;
    std::printf("  mapped areas (the strata raster and the map read again): %.0f ms\n", ms(t0));
    t0 = Clock::now();
    Strata counted = s;
    countStrata(out, counted, &prog, err);
    std::printf("  strata counted again (a later draw): %.0f ms\n", ms(t0));
    std::vector<int> n = allocate(s.strata, 1000, Alloc::Proportional, 50);
    std::vector<Point> pts;
    t0 = Clock::now();
    if (!drawSample(s, out, n, 42, {}, pts, err)) return 1;
    std::printf("  %zu points drawn in %.0f ms\n", pts.size(), ms(t0));
    std::vector<std::array<double, 2>> xy;
    for (const Point& p : pts) xy.push_back({p.x, p.y});
    std::vector<double> v;
    t0 = Clock::now();
    valuesAt(r, s.grid.wkt, xy, v, err);
    std::printf("  the map at the points: %.0f ms\n", ms(t0));
    // The estimators on 5000 points, 10 classes, every frame of a busy panel.
    AssessInput in;
    for (int c = 0; c < 10; ++c) {
        in.classes.push_back(std::to_string(c));
        in.stratumArea.push_back(1000.0 * (c + 1));
        in.mapped.push_back(1000.0 * (c + 1));
    }
    for (int i = 0; i < 5000; ++i) {
        in.stratum.push_back(i % 10);
        in.map.push_back(i % 10);
        in.ref.push_back(i % 7 == 0 ? (i + 1) % 10 : i % 10);
    }
    t0 = Clock::now();
    Assessment a;
    for (int k = 0; k < 10; ++k) a = assess(in);
    std::printf("  assess (5000 points, 10 classes): %.2f ms; overall %.4f\n", ms(t0) / 10, a.overall.est);
    std::error_code ec;
    fs::remove(fs::u8path(out), ec);
    return 0;
}

} // namespace val
