#pragma once

// Vector data (field plots, samples, parcels...) read through OGR, which comes
// with GDAL: GeoJSON, Shapefile, GeoPackage, KML, FlatGeobuf, GML, and CSV
// tables of points (x/lon and y/lat columns). Geometries are kept in the file's
// coordinates; VectorPlacer puts them on a raster's grid (any CRS, through PROJ).

#include <array>
#include <cstdint>
#include <functional>
#include <memory>
#include <string>
#include <vector>

#include "geom.hpp"

class OGRCoordinateTransformation;

struct VectorFeature {
    int64_t fid = 0;
    enum Kind { None, Point, Line, Area } kind = None; // None: no geometry (kept: rows stay in the file's order)
    // Points: one (x, y) per part; lines: one open ring per line; areas: their
    // rings (shells and holes, even-odd).
    Polygon parts;
    double cx = 0, cy = 0;   // a point on it (inside an area), in the file's coordinates
    std::vector<std::string> values; // one per field, as text ("" = null)
};

struct VectorData {
    std::string path, layer;       // the file and its OGR layer
    std::string crsWkt, crsName;   // "" = none in the file
    bool lonLatAssumed = false;    // no CRS, coordinates within lon/lat ranges: read as WGS 84
    std::vector<std::string> fields;
    std::vector<VectorFeature> features;
    size_t vertices = 0;
    int points = 0, lines = 0, areas = 0, empty = 0;
    bool truncated = false;        // stopped at the vertex limit
    double loadMs = 0;
    std::string kindText() const;  // e.g. "250 points" or "12 polygons, 3 lines"
};

// By extension: .geojson, .json, .shp, .gpkg, .kml, .kmz, .gml, .fgb, .csv, .gpx, .tab.
bool isVectorPath(const std::string& path);
// Every layer of the file that has features (a GeoPackage may hold several);
// empty with `error` if none can be read.
std::vector<std::shared_ptr<VectorData>> loadVectorFile(const std::string& path, std::string& error,
                                                        size_t maxVertices = 20000000);

// File coordinates -> a raster's pixels (x right, y down, continuous) and -> lon/lat.
class VectorPlacer {
public:
    // `rasterWkt` empty: the raster has no CRS (placed only if the vector has none either).
    static std::unique_ptr<VectorPlacer> create(const VectorData& v, const std::string& rasterWkt, bool hasGeoTransform,
                                                const std::array<double, 6>& gt, std::string& why);
    // Only toLonLat (from the file's CRS; it fails without one).
    static std::unique_ptr<VectorPlacer> lonLat(const VectorData& v);
    ~VectorPlacer();
    VectorPlacer(const VectorPlacer&) = delete;
    VectorPlacer& operator=(const VectorPlacer&) = delete;
    bool toPixel(double& x, double& y) const;
    // Many points in place; ok[i] = 0 where it failed.
    void toPixel(size_t n, double* x, double* y, int* ok) const;
    bool toLonLat(double& x, double& y) const; // false without a CRS
    Polygon placed(const Polygon& p) const;    // every vertex to pixels (failed ones left out)

private:
    VectorPlacer() = default;
    OGRCoordinateTransformation* toRaster_ = nullptr; // null: the same CRS
    OGRCoordinateTransformation* toLonLat_ = nullptr;
    double inv_[6] = {0, 1, 0, 0, 0, 1};
    bool lonLatSame_ = false; // the file is in lon/lat already
};

// A CSV field: quoted when it holds a comma, a quote or a line break.
std::string csvField(const std::string& s);

// The vector's features with extra columns (e.g. labels) written as GeoPackage,
// GeoJSON or Shapefile (by extension, through OGR: geometries and fields read
// again from the source) or as CSV (fields, a point's or an area's
// representative point in lon/lat, then the extra columns). `extra(i, k)`: the
// value of extra column k for feature i.
bool writeVectorTable(const std::string& path, const VectorData& v, const std::vector<std::string>& extraNames,
                      const std::function<std::string(size_t, size_t)>& extra, std::string& error);
