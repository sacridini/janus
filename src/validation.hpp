#pragma once

// Validation of maps after the good practices of Olofsson et al. (2014): the
// strata of a map (or of several maps), the sample size and its allocation, a
// stratified random sample of pixels, and Stehman's (2014) estimators of the
// accuracies and of the error-adjusted areas from the reference labels of the
// sample; the project that keeps all of it, a GeoPackage. The interface is in
// app_validation.cpp.
//
// The estimators are those of zeit.accuracy (Zeit 0.52.1), so the numbers are
// the same: with the strata being the classes of the map assessed they reduce
// to Olofsson et al. (2014), eqs. 4-11; a sample stratified by one map can
// assess another one. No finite population correction, like Zeit and R's
// sits_accuracy (negligible on maps of thousands of pixels and more).

#include <array>
#include <atomic>
#include <climits>
#include <cmath>
#include <cstdint>
#include <map>
#include <set>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

#include "geom.hpp"

namespace val {

using json = nlohmann::json;

constexpr int kNone = INT_MIN; // a value outside every class

// How a raster's values become classes.
enum class MapKind {
    Classes,   // its integer values (a land-cover map, a class map of Zeit)
    Intervals, // intervals of its values (bins: edges, [a, b)), e.g. a magnitude
    Events,    // a date of change (year of detection): > 0 change, else no change; bins: a class per period
};
const char* kindName(MapKind k);

// A raster and the rule that turns its values into classes.
struct MapRule {
    std::string path;            // GDAL path
    int band = 1;
    std::string name;            // as shown
    MapKind kind = MapKind::Classes;
    std::vector<double> bins;    // Intervals: edges; Events: year edges (empty: change / no change)
    std::map<int, std::string> names; // Classes: value -> name (else the value)
    std::set<int> change;        // Classes: values that are change; Intervals: intervals (1-based)

    // Classes: the value; Intervals: 1.. (the interval); Events: 0 no change, 1.. change
    // (its period; 1 without bins). kNone: none (no data, outside the intervals).
    int classOf(double v) const;
    bool isChange(int cls) const;
    std::string className(int cls) const;
    json toJson() const;
    static MapRule fromJson(const json& j);
};
// As zeit's names of intervals: "0.1-0.5"; years "2000-2009", or "2005" for one year.
std::string intervalName(double lo, double hi, bool years);
// "0, 0.5, 1" -> edges (increasing; "inf" and "-inf" allowed); empty with `error` if not.
std::vector<double> parseBins(const std::string& text, std::string& error);
std::string binsText(const std::vector<double>& bins);

// A raster's grid: size, transform and CRS.
struct Grid {
    int w = 0, h = 0;
    std::array<double, 6> gt{0, 1, 0, 0, 0, 1};
    bool hasGt = false;
    std::string wkt;             // "" = none
    void centre(int col, int row, double& x, double& y) const;
    json toJson() const;
    static Grid fromJson(const json& j);
};
bool readGrid(const std::string& path, Grid& g, std::string& error);
// The area of a pixel on each row: hectares (the sphere's, with the authalic
// radius, in a geographic CRS), or 1 per pixel without a CRS (unit "pixels").
std::vector<double> rowAreas(const Grid& g, std::string& unit);

// Background work: progress in [0, 1], cancelled when asked; `threads` for
// GDAL to decode and compress blocks with (the Settings' processing threads).
struct Progress {
    std::atomic<double> done{0};
    std::atomic<bool> cancel{false};
    int threads = 1;
};

// ---------------------------------------------------------------------------
// Strata
// ---------------------------------------------------------------------------

struct StrataSpec {
    std::vector<MapRule> maps;   // one: its classes are the strata; several: how many call a pixel change ("k of M")
    int buffer = 0;              // > 0: pixels without change within this many pixels of a change (Chebyshev) form a stratum
    Polygon limit;               // only pixels whose centres are inside (the first map's pixels); empty: every pixel
    json toJson() const;
    static StrataSpec fromJson(const json& j);
};

struct Stratum {
    int code = 0;                // its value in the strata raster (1-255; 0 = outside every stratum)
    std::string name;
    int cls = kNone;             // one map: the class of the map it holds
    bool change = false;         // its pixels are change
    bool buffer = false;         // the buffer stratum
    int64_t pixels = 0;
    double area = 0, weight = 0; // weight: share of the area
    double expectedUa = 0.75;    // design: the user's accuracy expected of it
    int n = 0;                   // design: points in it
};

struct Strata {
    Grid grid;
    std::vector<Stratum> strata;                     // in display order
    std::string unit = "ha";                         // of the areas
    std::vector<double> rowArea;                     // area of a pixel on each row
    std::vector<std::vector<int32_t>> rowCounts;     // [stratum][row] pixels (empty unless counted)
    std::vector<std::map<int, double>> mapped;       // [map of the spec] class -> area inside the strata
    double totalArea() const;
    int indexOfCode(int code) const;                 // -1: none
};

// One pass over the maps at full resolution, in bands of rows (other maps read
// on the first one's grid): the raster of the strata written to `out` (GTiff,
// Byte, 0 = no stratum, with the strata's names), the pixels and areas of each
// stratum per row, and the areas of every map's classes inside the strata.
bool computeStrata(const StrataSpec& spec, const std::string& out, Strata& s, Progress* p, std::string& error);
// The pixels of each stratum per row, read from the strata raster (for a sample drawn later).
bool countStrata(const std::string& strataPath, Strata& s, Progress* p, std::string& error);
// The area of each class of `map` over the pixels of the strata raster that are in a stratum
// (the study area), the map read on the strata's grid.
bool mappedAreas(const MapRule& map, const std::string& strataPath, const Strata& s, std::map<int, double>& area,
                 Progress* p, std::string& error);
// The raw values of `map` at points (x, y in the CRS `wkt`): NaN off the map or no data.
bool valuesAt(const MapRule& map, const std::string& wkt, const std::vector<std::array<double, 2>>& xy,
              std::vector<double>& out, std::string& error);

// ---------------------------------------------------------------------------
// Design
// ---------------------------------------------------------------------------

enum class Alloc { Proportional, Equal, Neyman, Manual };
const char* allocName(Alloc a);

// The sample size for a standard error of the overall accuracy (eq. 13).
int sampleSize(const std::vector<Stratum>& s, double targetSe);
// n points by area share (proportional), equally or by area x expected standard deviation (Neyman),
// largest remainders, at least `minPer` per stratum (taken from the strata above it); never more than
// a stratum's pixels. Manual: the strata's own n.
std::vector<int> allocate(const std::vector<Stratum>& s, int n, Alloc a, int minPer);

// The errors a design should give if the strata's accuracies are as expected:
// the overall accuracy's and each user's accuracy's standard error, and the
// confidence interval of the area of change (the errors of a stratum without
// change assumed to be change, as a binary map: Olofsson et al. 2014, sec. 5.1).
struct Expected {
    double overallSe = NAN;
    std::vector<double> userSe;
    bool change = false;
    double changeArea = NAN, changeCi = NAN;
};
Expected expectedErrors(const std::vector<Stratum>& s, double totalArea, double confidence);

// ---------------------------------------------------------------------------
// Sample
// ---------------------------------------------------------------------------

struct Point {
    int id = 0;                  // 1.. (point i has id i + 1)
    int stratum = 0;             // code
    int row = 0, col = 0;        // pixel of the strata raster
    double x = 0, y = 0;         // its centre, in the grid's CRS
};

// n[i] pixels of stratum i (strata order) drawn at random without replacement,
// none of `existing` (a sample made larger keeps its points); ids follow
// theirs. The same strata, seed and existing points give the same points on
// every platform. Needs s.rowCounts.
bool drawSample(const Strata& s, const std::string& strataPath, const std::vector<int>& n, uint64_t seed,
                const std::vector<Point>& existing, std::vector<Point>& out, std::string& error);

// ---------------------------------------------------------------------------
// Response: an interpreter's label of a point
// ---------------------------------------------------------------------------

struct Event {
    std::string label, date;
};
struct Label {
    std::string label;           // class of the label set ("" = not labelled)
    std::string date;            // date of the change, as the series shows it ("" = none)
    std::string notes;
    int confidence = 0;          // 0 not given, 1 low, 2 medium, 3 high
    std::vector<Event> events;   // more events at the point
    std::string updated;         // UTC, ISO 8601
    bool empty() const { return label.empty() && date.empty() && notes.empty() && confidence == 0 && events.empty(); }
};
extern const char* const kAdjudicated; // the "interpreter" whose labels settle disagreements
std::string nowIso();
int yearOf(const std::string& date); // first run of four digits, 0 if none
// The reference of point i: the adjudicated label, else the label of the only
// interpreter or of all when they agree (the first by name); `conflict`: they
// disagree and nobody adjudicated. nullptr: no label (or a conflict).
const Label* resolve(const std::map<std::string, std::vector<Label>>& labels, size_t i, bool& conflict);

// ---------------------------------------------------------------------------
// Estimation
// ---------------------------------------------------------------------------

struct Est {
    double est = NAN, se = NAN, lo = NAN, hi = NAN;
};
double zOf(double confidence); // two-sided normal quantile (1.96 at 0.95)

// Stehman's (2014) estimators of a stratified random sample: means of point
// values and ratios of them, weighted by the area of each stratum. Points with
// stratum < 0 are left out; strata without points are left out and the
// weights of the others scaled up (`covered`: the share of the area they hold).
class Estimator {
public:
    Estimator(const std::vector<double>& stratumArea, const std::vector<int>& stratumOfPoint, double confidence);
    Est mean(const std::vector<double>& y) const;
    Est ratio(const std::vector<double>& y, const std::vector<double>& x) const;
    double z = 1.96;
    double covered = 0;
    std::vector<int> missing;    // strata (with area) without a point

private:
    Est interval(double est, double se) const;
    std::vector<std::vector<int>> members_;
    std::vector<double> weight_;
};

struct ClassArea {
    double mapped = 0, est = NAN, se = NAN, ci = NAN, lo = NAN, hi = NAN;
};
struct Assessment {
    std::vector<std::string> classes;
    std::vector<std::vector<double>> confusion;      // [map][reference]: proportions of area
    std::vector<std::vector<int>> counts;            // [map][reference]: points
    std::vector<std::vector<std::vector<int>>> cells; // [map][reference]: the points
    Est overall;
    std::vector<Est> users, producers;
    std::vector<ClassArea> area;
    double totalArea = 0;
    int n = 0;                   // points used
    double covered = 1;
    std::vector<int> missing;    // strata without a point
};
struct AssessInput {
    std::vector<double> stratumArea;  // per stratum
    std::vector<int> stratum;         // per point: stratum index (-1: none)
    std::vector<int> map, ref;        // per point: class index (-1: none, not labelled, left out)
    std::vector<std::string> classes;
    std::vector<double> mapped;       // per class: the area the map gives it
    double confidence = 0.95;
};
Assessment assess(const AssessInput& in);

// Of the points both the map and the reference call change, with both years:
// the share whose years are at most `tolerance` apart and the mean difference.
struct DateAgreement {
    double tolerance = 1;
    Est within, meanDiff;
    int n = 0;
    std::map<int, int> diffs;    // map year - reference year -> points
};
DateAgreement dateAgreement(const Estimator& e, const std::vector<int>& mapYear, const std::vector<int>& refYear,
                            double tolerance);

// Two interpreters' labels of the same points (pairs where both gave one).
struct Agreement {
    int n = 0;
    double observed = NAN, kappa = NAN;
};
Agreement cohen(const std::vector<std::string>& a, const std::vector<std::string>& b);

// ---------------------------------------------------------------------------
// Project: a GeoPackage
// ---------------------------------------------------------------------------

// A map assessed with the sample, and how the reference labels translate to its classes.
struct AssessedMap {
    MapRule rule;
    std::map<std::string, std::string> mapping; // reference label -> class name ("" = left out)
};

// samples (points in the grid's CRS: sample_id, stratum, stratum_name, row,
// col, and the reference: label, ref_date, confidence, notes, events,
// interpreters, status), labels (sample_id, interpreter, label, date,
// confidence, notes, events, updated), strata and project (key, value) tables;
// the strata raster beside it (strataPath).
struct Project {
    std::string path, strataPath;  // absolute
    std::string id;                // copies of a project share it
    StrataSpec spec;
    Strata strata;                 // without rowCounts
    uint64_t seed = 42;
    json design;                   // alloc, target_se, min_per_stratum, confidence
    std::vector<std::string> labelSet;
    std::vector<Point> points;
    std::map<std::string, std::vector<Label>> labels; // interpreter -> per point
    std::vector<AssessedMap> maps;
};
std::string newProjectId();
// Writes the whole project (over an existing file).
bool createProject(const Project& p, std::string& error);
bool loadProject(const std::string& path, Project& p, std::string& error);
// A GeoPackage written by createProject (opened as a project, not as vector layers).
bool isProject(const std::string& path);
// The project and strata tables (maps, translations, label set, design).
bool saveProjectMeta(const Project& p, std::string& error);
// New points (a larger sample), with the strata table.
bool appendPoints(const Project& p, const std::vector<Point>& pts, std::string& error);
// An interpreter's labels of some points (index, label) and the reference that
// follows for them (index, label or nullptr, conflict) on the samples layer.
struct FinalRef {
    int point = 0;
    Label ref;                   // empty: none
    int interpreters = 0;
    bool conflict = false;
};
bool saveLabels(const std::string& path, const std::string& interpreter, const std::vector<std::pair<int, Label>>& rows,
                const std::vector<FinalRef>& finals, std::string& error);
// The labels of another copy of the project (to merge): every interpreter's, by point.
bool readLabels(const std::string& path, size_t points, std::string& projectId,
                std::map<std::string, std::vector<Label>>& out, std::string& error);
json labelToJson(const Label& l);
Label labelFromJson(const json& j);

// ---------------------------------------------------------------------------
// Assessing a map of the project
// ---------------------------------------------------------------------------

// What a map of events calls the reference labels: "change", "no change" or "" (left out).
extern const char* const kChange;
extern const char* const kNoChange;
// Translations guessed from the names: a label equal to a class (case aside);
// for events, labels of loss and disturbance are change, the rest no change;
// "can't tell", clouds and no data left out. Labels not guessed are absent.
std::map<std::string, std::string> defaultMapping(const MapRule& r, const std::vector<std::string>& labelSet,
                                                  const std::vector<std::string>& classNames);
// The classes of the map: those it has (areas, values at the points) in order.
std::vector<int> mapClasses(const MapRule& r, const std::vector<double>& values, const std::map<int, double>& mapped);

// The estimator's input for one map: its class and the reference's at each
// point, the strata's areas; what was left out and why.
struct Prepared {
    AssessInput in;
    std::vector<int> classKeys;          // the map's class of each of in.classes (kNone: a reference class only)
    std::vector<int> mapYear, refYear;   // maps of events: years at each point (0: none)
    int labelled = 0, unlabelled = 0, conflicts = 0, unmapped = 0, excluded = 0, noDate = 0, offMap = 0;
    std::set<std::string> unmappedLabels;
};
// `use`: the points to count (empty: all), e.g. the first labelled ones.
Prepared prepare(const Project& p, const AssessedMap& m, const std::vector<double>& values,
                 const std::map<int, double>& mapped, double confidence, const std::vector<char>& use = {});

// The area of change of each year from the reference alone (the dates of the
// labels that `m` calls change): the area whose first change in the period is
// in that year, with its interval.
std::vector<std::pair<int, ClassArea>> changeByYear(const Project& p, const AssessedMap& m, double confidence);

// Python (zeit.accuracy) that gives the same estimates from the project's files.
std::string pythonSnippet(const Project& p, const AssessedMap& m, double confidence);

// --selftest-validation: the estimators against Olofsson et al. (2014), the
// design, strata, sample and project on synthetic maps, and (with `python`,
// the runtime's interpreter) the estimates against zeit.accuracy; nullptr = passed.
const char* selfTest(std::string& report, const std::string& python);
// --measure-validation IN: the strata of IN (a buffer of 1 px), its areas, a
// sample of 1000 points, its values at them and the estimators, timed.
int measure(const std::string& path);

} // namespace val
