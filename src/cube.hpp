#pragma once

#include <array>
#include <cstdint>
#include <memory>
#include <string>
#include <unordered_map>
#include <vector>

class GDALDataset;
class GDALRasterBand;

// One "layer" of the cube = one date. Either a file (1 per date) or a band
// of a multiband file.
struct Layer {
    std::string path;
    int band = 1;
    double time = 0;     // Unix seconds (if CubeInfo::timeIsDate) or index
    std::string label;   // displayed text (formatted date or name)
    bool hasNoData = false;
    double noData = 0;
    double scale = 1, offset = 0;
};

enum class TimeGranularity { Index, Year, Month, Day };

struct CubeInfo {
    uint64_t id = 0;     // unique per open (invalidates per-thread readers)
    std::vector<Layer> layers;
    int width = 0, height = 0;
    std::string dataType;
    std::string driver;      // GDAL driver of the 1st layer (e.g. GTiff)
    std::string crsName, crsAuthority;
    bool hasGeoTransform = false;
    std::array<double, 6> geoTransform{0, 1, 0, 0, 0, 1};
    bool timeIsDate = false;
    TimeGranularity granularity = TimeGranularity::Index;
    std::string description; // e.g. "41 files (band 1 of each)"
    std::string firstPath;

    int T() const { return int(layers.size()); }
    // Time in decimal years relative to the 1st layer (for trends).
    double yearsFromStart(int t) const;
    bool pixelToGeo(double px, double py, double& gx, double& gy) const;
    // Decimal year of layer t (e.g. 2003.5); the index when there are no dates.
    double decimalYear(int t) const;
    // Inverse of decimalYear, in the chart's X units (Unix seconds or index).
    double xFromDecimalYear(double y) const;
};

// Writes a VRT with one band per layer, in date order, with nodata and
// scale/offset applied: the exact cube tsv shows, readable by other tools.
bool writeCubeVrt(const CubeInfo& info, const std::string& path, std::string& error);

// Interprets the inputs: a folder, a wildcard pattern (*, ?), a list of files
// or a single file (multiband = 1 band per date). `band` is the band used when
// each file is one date.
std::shared_ptr<CubeInfo> openCube(const std::vector<std::string>& inputs, int band, std::string& error);

std::string formatTime(const CubeInfo& info, double t);

// True for file names with a raster extension tsv lists by default.
bool isRasterPath(const std::string& path);

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

private:
    GDALRasterBand* band(int layer);
    std::shared_ptr<const CubeInfo> info_;
    std::unordered_map<std::string, GDALDataset*> ds_;
};

// The current thread's reader for this cube (created on demand).
CubeReader& threadReader(const std::shared_ptr<const CubeInfo>& info);
