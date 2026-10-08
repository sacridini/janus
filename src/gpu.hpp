#pragma once

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

// Renderer-neutral GPU layer: OpenGL 3.3 (gpu_gl.cpp) or Metal (gpu_metal.mm),
// chosen at build time (JANUS_RENDERER). A GpuTex is a GL texture name or a
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
    ModeDiff,        // value(t) - value(reference date), reference in DrawParams::tg
    ModeDropDate,    // largest drop between consecutive valid observations: date index
    ModeDropMag,     // ... and its magnitude (previous - next > 0)
    ModeCount
};

// Cube (overview) on the GPU + per-pixel temporal statistics computed in a
// shader. GL: R32F texture array (1 layer per date) and RGBA32F statistics
// textures. Metal: shared-memory buffers ([t][y][x] floats; float4 per pixel).
struct GpuCube {
    int w = 0, h = 0, T = 0;
    uint64_t cube = 0;          // backend handles (see gpu_gl.cpp / gpu_metal.mm)
    uint64_t stats0 = 0;        // mean, std, slope, valid count
    uint64_t stats1 = 0;        // min, max, R², largest drop
    uint64_t stats2 = 0;        // date index of the largest drop (1 float per pixel)
    uint64_t times = 0;         // T floats: years since the 1st date
    bool statsValid = false;
    std::vector<bool> loaded;
    bool sharesHost = false;    // Metal: the cube buffer is the Overview's array itself
    // CPU-visible statistics once computed, w*h float4 each (Metal: the shared
    // buffers themselves; GL: read back into statsCopy).
    const float* hostStats0 = nullptr;
    const float* hostStats1 = nullptr;
    const float* hostStats2 = nullptr; // w*h floats
    std::unique_ptr<float[]> statsCopy;

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

// A layer in another CRS than the map's (reprojection): each point of the quad
// goes to the layer's own coordinates through a grid made on the CPU
// (createWarpGrid: the layer's uv at nodes over a map-space domain), which the
// shader interpolates bilinearly; points outside the drawn texture are discarded.
struct WarpParams {
    GpuTex grid = 0;              // 0 = none: the quad maps straight onto the texture
    float quad[4] = {0, 0, 1, 1}; // the quad in the grid's domain (0..1): x0, y0, w, h
    float src[4] = {0, 0, 1, 1};  // the drawn texture (cube, tile) in the layer's uv: x0, y0, w, h
};

struct DrawParams {
    int mode = ModeValue;
    int t = 0, tg = 0, tb = 0;   // layers (tg/tb only in RGB mode; tg = reference date in ModeDiff)
    float lo = 0, hi = 1;        // stretch range
    GpuTex classLut = 0;         // categorical data: class value -> colour (see createClassLut)
    GpuTex tile2 = 0;            // ModeDiff on a detail tile: the reference date's tile
    WarpParams warp;             // reprojected layer
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

    // Map: rectangles in pixels of the w x h target (x0, y0, x1, y1). Each map
    // panel draws into a target of its own (`slot`; 0 = the main map).
    void beginMap(int w, int h, const float bg[4], int slot = 0);
    void drawCube(const GpuCube& c, const float rect[4], const DrawParams& p);
    void drawTile(GpuTex tex, const float rect[4], const DrawParams& p);
    // Same, with a colormap of their own and an opacity (layers drawn over each other).
    void drawCube(const GpuCube& c, const float rect[4], const DrawParams& p, int implotColormap, float alpha);
    void drawTile(GpuTex tex, const float rect[4], const DrawParams& p, int implotColormap, float alpha);
    // Single-band raster (e.g. a Zeit result) blended over the map with its own colormap.
    void drawOverlay(GpuTex tex, const float rect[4], float lo, float hi, int implotColormap, float alpha,
                     const WarpParams* warp = nullptr);
    void endMap();
    GpuTex mapTexture(int slot = 0) const;
    void releaseMap(int slot); // a closed panel's target
    // Texture rows: GL framebuffers are bottom-up, Metal textures top-down
    // (decides the UVs the map is shown with).
    static bool mapBottomUp();
    // Pixel (x, y from the top) of a map target; waits for the GPU (tests only).
    void readMapPixel(int x, int y, unsigned char rgba[4], int slot = 0);
    // A whole map target as RGBA8, rows top-down (exports); waits for the GPU.
    void readMap(std::vector<unsigned char>& rgba, int& w, int& h, int slot = 0);

    static GpuTex createTileTexture(int w, int h, const float* data);
    // rgba: kClassLutSize RGBA8 texels. Pass `tex` to update an existing LUT.
    static GpuTex createClassLut(const unsigned char* rgba, GpuTex tex = 0);
    // rg: w x h pairs (RG32F), rows top-down: the warp grid of WarpParams.
    static GpuTex createWarpGrid(int w, int h, const float* rg);
    static void deleteTexture(GpuTex tex);

    struct Impl;

private:
    std::unique_ptr<Impl> impl_;
};
