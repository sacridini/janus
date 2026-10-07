#pragma once

#include <string>
#include <vector>

#include "gl.hpp"

// A single-band raster produced by a tool (e.g. LandTrendr's year of
// detection), drawn over the map on the cube's pixel grid.
struct ResultLayer {
    uint64_t cubeId = 0;    // series (layer) the result belongs to
    std::string name;       // e.g. "LandTrendr: Year of detection"
    std::string path;
    std::string unit;
    int cmap = 0;           // ImPlot colormap
    int x0 = 0, y0 = 0, w = 0, h = 0; // window in cube pixels
    int tw = 0, th = 0;     // size of the (possibly downsampled) copy below
    std::vector<float> data; // tw x th, NaN = no value
    GLuint tex = 0;
    float lo = 0, hi = 1;
    bool visible = true;
    float opacity = 1.0f;

    float valueAt(int cubeX, int cubeY) const; // NaN outside or without value
};

// Reads band 1 of `path`, downsampled (nearest) so the longer side is at most
// `maxDim`. Safe to call from a background thread.
bool loadResultRaster(const std::string& path, int maxDim, std::vector<float>& data, int& tw, int& th,
                      std::string& error);

// Automatic display range: min..max for years/durations, 2-98% otherwise.
void autoResultRange(ResultLayer& r);
