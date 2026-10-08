#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

// Renderer-neutral GPU layer: OpenGL 3.3 (gpu_gl.cpp) or Metal (gpu_metal.mm),
// chosen at build time (TSV_RENDERER). A GpuTex is a GL texture name or a
// retained id<MTLTexture>; 0 = none. It is also the ImTextureID of that texture.
using GpuTex = uint64_t;

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

// Cube (overview) on the GPU + per-pixel temporal statistics computed in a
// shader. GL: R32F texture array (1 layer per date) and RGBA32F statistics
// textures. Metal: shared-memory buffers ([t][y][x] floats; float4 per pixel).
struct GpuCube {
    int w = 0, h = 0, T = 0;
    uint64_t cube = 0;          // backend handles (see gpu_gl.cpp / gpu_metal.mm)
    uint64_t stats0 = 0;        // mean, std, slope, valid count
    uint64_t stats1 = 0;        // min, max, R², -
    uint64_t times = 0;         // T floats: years since the 1st date
    bool statsValid = false;
    std::vector<bool> loaded;
    bool sharesHost = false;    // Metal: the cube buffer is the Overview's array itself
    // CPU-visible statistics once computed, w*h float4 each (Metal: the shared
    // buffers themselves; GL: read back into statsCopy).
    const float* hostStats0 = nullptr;
    const float* hostStats1 = nullptr;
    std::vector<float> statsCopy;

    GpuCube() = default;
    GpuCube(const GpuCube&) = delete;
    GpuCube& operator=(const GpuCube&) = delete;
    ~GpuCube();
    // host: the Overview's page-aligned [t][y][x] array (hostBytes whole pages),
    // which must outlive the cube. Metal reads it in place, so uploadLayer only
    // marks the date; GL copies each date into its texture array.
    void create(int w, int h, int T, const std::vector<float>& timesYears, float* host, size_t hostBytes);
    void uploadLayer(int t, const float* data);
    size_t bytes() const;
};

struct DrawParams {
    int mode = ModeValue;
    int t = 0, tg = 0, tb = 0;   // layers (tg/tb only in RGB mode)
    float lo = 0, hi = 1;        // stretch range
    GpuTex classLut = 0;         // categorical data: class value -> colour (see createClassLut)
};

// Class colours of categorical data: kClassLutSize texels, value v -> texel v
// (alpha 0 = not a class of the series, drawn transparent).
constexpr int kClassLutSize = 4096;

// Programs, colormap and the framebuffer the map is drawn into.
class Gpu {
public:
    Gpu();
    ~Gpu();
    bool init(std::string& error);
    void shutdown();

    // Limits of the backend: longest side of the overview, dates per cube and
    // bytes of one cube (the overview budget is capped to it).
    int maxCubeSide() const;
    int maxDates() const;
    int64_t maxCubeBytes() const;

    void setColormap(int implotColormap);
    // Computes the temporal statistics (1 shader pass) and reads them back to
    // the CPU (used for the automatic range and the histogram).
    void computeStats(GpuCube& c);

    // Map: rectangles in pixels of the w x h target (x0, y0, x1, y1).
    void beginMap(int w, int h, const float bg[4]);
    void drawCube(const GpuCube& c, const float rect[4], const DrawParams& p);
    void drawTile(GpuTex tex, const float rect[4], const DrawParams& p);
    // Same, with a colormap of their own and an opacity (layers drawn over each other).
    void drawCube(const GpuCube& c, const float rect[4], const DrawParams& p, int implotColormap, float alpha);
    void drawTile(GpuTex tex, const float rect[4], const DrawParams& p, int implotColormap, float alpha);
    // Single-band raster (e.g. a Zeit result) blended over the map with its own colormap.
    void drawOverlay(GpuTex tex, const float rect[4], float lo, float hi, int implotColormap, float alpha);
    void endMap();
    GpuTex mapTexture() const;
    // Texture rows: GL framebuffers are bottom-up, Metal textures top-down
    // (decides the UVs the map is shown with).
    static bool mapBottomUp();
    // Pixel (x, y from the top) of the last map drawn; waits for the GPU (tests only).
    void readMapPixel(int x, int y, unsigned char rgba[4]);

    static GpuTex createTileTexture(int w, int h, const float* data);
    // rgba: kClassLutSize RGBA8 texels. Pass `tex` to update an existing LUT.
    static GpuTex createClassLut(const unsigned char* rgba, GpuTex tex = 0);
    static void deleteTexture(GpuTex tex);

    struct Impl;

private:
    std::unique_ptr<Impl> impl_;
};
