#pragma once

#include <cstddef>
#include <map>
#include <string>
#include <vector>

#include "gl.hpp"

enum DisplayMode {
    ModeValue = 0,   // value at date t
    ModeAnomaly,     // value(t) - temporal mean
    ModeMean,
    ModeStd,
    ModeSlope,       // OLS slope per year
    ModeMin,
    ModeMax,
    ModeAmplitude,   // max - min
    ModeR2,
    ModeRGB,         // composite of 3 dates
    ModeCount
};

// Cube (overview) on the GPU: R32F texture array (1 layer per date) +
// per-pixel temporal statistics computed in a shader.
struct GpuCube {
    int w = 0, h = 0, T = 0;
    GLuint cube = 0;            // GL_TEXTURE_2D_ARRAY R32F
    GLuint stats0 = 0;          // RGBA32F: mean, std, slope, valid count
    GLuint stats1 = 0;          // RGBA32F: min, max, R², -
    GLuint times = 0;           // T x 1 R32F: years since the 1st date
    bool statsValid = false;
    std::vector<bool> loaded;

    GpuCube() = default;
    GpuCube(const GpuCube&) = delete;
    GpuCube& operator=(const GpuCube&) = delete;
    ~GpuCube();
    void create(int w, int h, int T, const std::vector<float>& timesYears);
    void uploadLayer(int t, const float* data);
    size_t bytes() const;
};

struct DrawParams {
    int mode = ModeValue;
    int t = 0, tg = 0, tb = 0;   // layers (tg/tb only in RGB mode)
    float lo = 0, hi = 1;        // stretch range
    GLuint classLut = 0;         // categorical data: class value -> colour (see createClassLut)
};

// Class colours of categorical data: kClassLutSize texels, value v -> texel v
// (alpha 0 = not a class of the series, drawn transparent).
constexpr int kClassLutSize = 4096;

// Programs, colormap and the framebuffer the map is drawn into.
class Gpu {
public:
    bool init(std::string& error);
    void shutdown();

    void setColormap(int implotColormap);
    // Computes the temporal statistics (1 shader pass) and reads them back to
    // the CPU (used for the automatic range and the histogram).
    void computeStats(GpuCube& c, std::vector<float>& s0, std::vector<float>& s1);

    // Map: rectangles in canvas pixels (x0, y0, x1, y1).
    void beginMap(int w, int h, const float bg[4]);
    void drawCube(const GpuCube& c, const float rect[4], const DrawParams& p);
    void drawTile(GLuint tex, const float rect[4], const DrawParams& p);
    // Same, with a colormap of their own and an opacity (layers drawn over each other).
    void drawCube(const GpuCube& c, const float rect[4], const DrawParams& p, int implotColormap, float alpha);
    void drawTile(GLuint tex, const float rect[4], const DrawParams& p, int implotColormap, float alpha);
    // Single-band raster (e.g. a Zeit result) blended over the map with its own colormap.
    void drawOverlay(GLuint tex, const float rect[4], float lo, float hi, int implotColormap, float alpha);
    void endMap();
    GLuint mapTexture() const { return fboColor_; }

    static GLuint createTileTexture(int w, int h, const float* data);
    // rgba: kClassLutSize RGBA8 texels. Pass `tex` to update an existing LUT.
    static GLuint createClassLut(const unsigned char* rgba, GLuint tex = 0);

private:
    void drawQuad(const float rect[4], int source, const DrawParams& p);
    GLuint colormapTexture(int implotColormap);
    void beginLayer(int implotColormap, float alpha);
    void endLayer();

    GLuint vao_ = 0;
    GLuint progDisplay_ = 0, progStats_ = 0;
    GLuint cmapTex_ = 0, dummy2D_ = 0, dummyArray_ = 0;
    std::map<int, GLuint> overlayCmaps_; // colormap textures for overlays, by ImPlot colormap
    GLuint fbo_ = 0, fboColor_ = 0;
    int fboW_ = 0, fboH_ = 0;
    GLuint statsFbo_ = 0;
    const GpuCube* boundCube_ = nullptr;
};
