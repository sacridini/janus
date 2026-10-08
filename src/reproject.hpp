#pragma once

// Reprojection: a layer whose CRS differs from the active layer's, or whose
// grid is rotated, placed on the map (the active layer's pixel grid). Exact on
// the CPU through OGR/PROJ (cursor, pins, ROI, tile placement) and, for
// drawing, a grid of the layer's coordinates at nodes of map space that the
// display shader interpolates bilinearly (gpu.hpp, WarpParams).

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "cube.hpp"
#include "gpu.hpp"

class OGRCoordinateTransformation;

class Reprojection {
public:
    // Null if the two cannot be related (no georeferencing, unknown CRS, no
    // transformation between the CRSs); `why` says why. The GPU grid covers the
    // active layer and `margin` x its longer side around it.
    static std::unique_ptr<Reprojection> create(const CubeInfo& active, const CubeInfo& layer, std::string& why,
                                                double margin = 0.6);
    ~Reprojection();
    Reprojection(const Reprojection&) = delete;
    Reprojection& operator=(const Reprojection&) = delete;

    uint64_t activeId = 0, layerId = 0; // the CubeInfo ids it was made for
    bool sameCrs = false;               // only the grids differ (rotation): an affine map
    std::string crsText;                // the layer's CRS as shown (e.g. "EPSG:32722")

    // Active layer's pixels (map space) <-> the layer's pixels; false where the
    // transformation fails (e.g. outside the projection's area of use).
    bool toLayer(double x, double y, double& lx, double& ly) const;
    bool toActive(double lx, double ly, double& x, double& y) const;
    // Many points in place; ok[i] = 0 where it failed.
    void toLayer(size_t n, double* x, double* y, int* ok) const;
    void toActive(size_t n, double* x, double* y, int* ok) const;
    // Map-space bounding box (x0, y0, x1, y1) of the layer's pixels [x0, x1) x
    // [y0, y1), from points along their edges. False if none transforms.
    bool footprint(double x0, double y0, double x1, double y1, double box[4]) const;
    // The map-space rectangle [x0, x1) x [y0, y1) in the layer's pixels: a
    // polygon through `perEdge` points per edge (x, y pairs; failed points left out).
    std::vector<double> polygon(double x0, double y0, double x1, double y1, int perEdge) const;
    // Layer pixels per map pixel around map point (x, y) (square root of the Jacobian).
    double layerPxPerMapPx(double x, double y) const;

    // GPU grid: nodes over `domain`, row by row, each the layer's uv (pixel /
    // size, 0..1 over the layer; NaN where the transformation fails).
    double domain[4] = {0, 0, 0, 0}; // map-space rectangle: x0, y0, x1, y1 (empty: nothing to draw)
    int gridW = 0, gridH = 0;
    std::vector<float> grid;         // gridW * gridH * 2
    double gridError = 0;            // max |grid - exact| at the cells' centres, in the layer's pixels
    double buildMs = 0;
    GpuTex tex = 0;                  // the grid on the GPU, made by the App on the main thread
    bool hasDomain() const { return gridW > 1 && domain[2] > domain[0] && domain[3] > domain[1]; }
    // The layer's pixel at map point (x, y) through the grid, as the shader does.
    bool gridAt(double x, double y, double& lx, double& ly) const;

private:
    Reprojection() = default;
    void buildGrid(const CubeInfo& active, double margin);
    double ga_[6]{}, gb_[6]{};       // geotransforms (pixel -> CRS)
    double ia_[6]{}, ib_[6]{};       // and their inverses
    int lw_ = 0, lh_ = 0;            // the layer's size
    OGRCoordinateTransformation* fwd_ = nullptr; // active CRS -> layer CRS (null: same CRS)
    OGRCoordinateTransformation* inv_ = nullptr;
};
