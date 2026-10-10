#pragma once

#include <array>
#include <cstdint>
#include <map>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

class GDALDataset;
class GDALRasterBand;

// Nodata and scale/offset of one source band.
struct BandMeta {
    bool hasNoData = false;
    double noData = 0;
    double scale = 1, offset = 0;
};

// How the quality band marks usable observations.
enum class QaRule {
    None,
    Fmask,     // Fmask codes: 0 clear land, 1 water usable; 2 shadow, 3 snow, 4 cloud, 255 fill masked
    LandsatC2, // Landsat Collection 2 QA_PIXEL bits: fill, dilated cloud, cloud, shadow, snow masked
    NonZero,   // generic mask: 0 = masked, anything else usable
};
const char* qaRuleId(QaRule r);    // "fmask", "landsat_c2", "nonzero", "none"
const char* qaRuleLabel(QaRule r); // for the interface
bool qaUsable(QaRule r, uint32_t code);

// When every date is a file with several bands (e.g. Landsat surface
// reflectance + QA): what the series shows. Ignored for a single multiband
// file, where each band is a date.
struct BandSelection {
    int band = 1;   // shown band (1-based)
    int ndBand = 0; // > 0: show the normalized difference (band - ndBand) / (band + ndBand)
    int qaBand = 0; // > 0: quality band; unusable observations become no-data
    QaRule qaRule = QaRule::None;
    bool chosen = false; // set by the user: no automatic quality band
    bool operator==(const BandSelection& o) const {
        return band == o.band && ndBand == o.ndBand && qaBand == o.qaBand && qaRule == o.qaRule;
    }
    bool operator!=(const BandSelection& o) const { return !(*this == o); }
};

// One "layer" of the cube = one date. Either a file (1 per date) or a band
// of a multiband file.
struct Layer {
    std::string path;
    int band = 1;        // the shown band of the file (absolute)
    // The date's bands are bandBase + 1 .. bandBase + bandsPerDate of the file
    // (0, except embeddings of several years in one file: see openCube).
    int bandBase = 0;
    double time = 0;     // Unix seconds (if CubeInfo::timeIsDate) or index
    std::string label;   // displayed text (formatted date or name)
    BandMeta meta;       // of `band`
    BandMeta ndMeta;     // of the normalized-difference band, if any
};

enum class TimeGranularity { Index, Year, Month, Day };

struct CubeInfo {
    uint64_t id = 0;     // unique per open (invalidates per-thread readers)
    std::vector<Layer> layers;
    int width = 0, height = 0;
    std::string dataType;
    std::string driver;      // GDAL driver of the 1st layer (e.g. GTiff)
    std::string crsName, crsAuthority;
    std::string crsWkt;      // WKT2 of the CRS ("" = none), for reprojection
    bool hasGeoTransform = false;
    std::array<double, 6> geoTransform{0, 1, 0, 0, 0, 1};
    bool timeIsDate = false;
    TimeGranularity granularity = TimeGranularity::Index;
    std::string description; // e.g. "41 files (band 1 of each)"
    std::string firstPath;
    // One file per date: bands of each file (names from the 1st file's band
    // descriptions, "Band N" otherwise) and what the series shows.
    int bandsPerDate = 1;
    std::vector<std::string> bandNames;
    BandSelection sel;
    // Categorical data (classes, e.g. land cover) declared by the file: a
    // colour table, category names or a raster attribute table on the band.
    bool fileCategorical = false;
    std::map<int, std::array<unsigned char, 4>> classColors; // value -> RGBA
    std::map<int, std::string> classNames;                   // value -> name
    // Zeit's ZEIT_EMBEDDING tag of the first file (JSON: source, model...): a
    // layer of foundation-model embeddings (see embedding.hpp).
    std::string embeddingTag;

    int T() const { return int(layers.size()); }
    // Time in decimal years relative to the 1st layer (for trends).
    double yearsFromStart(int t) const;
    bool pixelToGeo(double px, double py, double& gx, double& gy) const;
    bool geoToPixel(double gx, double gy, double& px, double& py) const;
    // Decimal year of layer t (e.g. 2003.5); the index when there are no dates.
    double decimalYear(int t) const;
    // Inverse of decimalYear, in the chart's X units (Unix seconds or index).
    double xFromDecimalYear(double y) const;
    // Days since 1970-01-01 of layer t (dates only).
    int64_t unixDay(int t) const;
    // Short text for the shown quantity, e.g. "Band 4" or "ND(nir, red), QA fmask".
    std::string selectionText() const;
};

// Writes a VRT with one band per layer, in date order, with nodata and
// scale/offset applied, readable by other tools. `sourceBand` = 0 writes the
// shown band; another value writes that band of every date (one file per
// date), e.g. the second band of a normalized difference or the QA band
// (`raw`: no nodata or scaling, for quality codes). The normalized difference
// and the QA mask themselves are not in the VRT: the reader applies them.
bool writeCubeVrt(const CubeInfo& info, const std::string& path, std::string& error, int sourceBand = 0,
                  bool raw = false);

// Interprets the inputs: a folder, a wildcard pattern (*, ?), a list of files
// or a single file (multiband = 1 band per date). `sel` chooses the band(s)
// used when each file is one date.
std::shared_ptr<CubeInfo> openCube(const std::vector<std::string>& inputs, const BandSelection& sel,
                                   std::string& error);

std::string formatTime(const CubeInfo& info, double t);

// True for file names with a raster extension Janus lists by default.
bool isRasterPath(const std::string& path);
// What files would open as, from their names alone (dates as openCube reads
// them): "41 rasters, 1985 to 2025, yearly"; empty without files.
std::string describeSeriesFiles(const std::vector<std::string>& names);

// Reader with its own GDAL handles. Not thread-safe: use threadReader().
class CubeReader {
public:
    explicit CubeReader(std::shared_ptr<const CubeInfo> info);
    ~CubeReader();
    CubeReader(const CubeReader&) = delete;
    CubeReader& operator=(const CubeReader&) = delete;

    uint64_t cubeId() const { return info_->id; }
    // Reads the window (x,y,w,h) resampled (nearest neighbour) to bw x bh.
    // Nodata becomes NaN; the band's scale/offset are applied.
    bool readWindow(int layer, int x, int y, int w, int h, float* buf, int bw, int bh);
    bool readPixel(int layer, int x, int y, float& value);
    // One source band of a date at a pixel, scale applied (nodata -> NaN unless `raw`).
    bool readBandPixel(int layer, int band, int x, int y, float& value, bool raw = false);

private:
    GDALRasterBand* band(int layer, int b = 0);
    bool readRaw(int layer, int b, int x, int y, int w, int h, float* buf, int bw, int bh);
    std::vector<float> tmp_;
    std::shared_ptr<const CubeInfo> info_;
    std::unordered_map<std::string, GDALDataset*> ds_;
};

// The current thread's reader for this cube (created on demand).
CubeReader& threadReader(const std::shared_ptr<const CubeInfo>& info);
