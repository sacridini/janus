// OpenGL 3.3 backend of gpu.hpp (Windows, Linux; macOS with JANUS_RENDERER=GL).
#include "gpu.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <map>
#include <string>

#include <implot.h>

#include "gl.hpp"

namespace {

const char* kVertex = R"(#version 330 core
uniform vec4 uRect;   // NDC: x0, y0(top), x1, y1(bottom)
out vec2 vUV;
void main() {
    vec2 c = vec2(gl_VertexID & 1, gl_VertexID >> 1);   // triangle strip
    gl_Position = vec4(mix(uRect.xy, uRect.zw, c), 0.0, 1.0);
    vUV = c;
}
)";

// Per-pixel temporal statistics of the overview, in two passes (mean, then
// centered sums: numerically stable). The largest drop is the largest decrease
// between consecutive valid observations (NaN skipped), dated at the later one;
// 0 and no date if the series never decreases.
const char* kStatsFrag = R"(#version 330 core
uniform sampler2DArray uCube;
uniform sampler2D uTimes;
uniform int uT;
layout(location = 0) out vec4 o0;   // mean, std, slope, n
layout(location = 1) out vec4 o1;   // min, max, R2, largest drop
layout(location = 2) out float o2;  // date index of the largest drop
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    float nanv = uintBitsToFloat(0x7fc00000u);
    float n = 0.0, sv = 0.0, sx = 0.0, mn = 3.0e38, mx = -3.0e38;
    float last = 0.0, drop = 0.0, dropT = nanv;
    for (int t = 0; t < uT; ++t) {
        float v = texelFetch(uCube, ivec3(p, t), 0).r;
        if (isnan(v)) continue;
        if (n > 0.0 && last - v > drop) { drop = last - v; dropT = float(t); }
        last = v;
        n += 1.0; sv += v; sx += texelFetch(uTimes, ivec2(t, 0), 0).r;
        mn = min(mn, v); mx = max(mx, v);
    }
    if (n < 1.0) { o0 = vec4(nanv, nanv, nanv, 0.0); o1 = vec4(nanv); o2 = nanv; return; }
    float mv = sv / n, mxx = sx / n;
    float sxx = 0.0, sxy = 0.0, syy = 0.0;
    for (int t = 0; t < uT; ++t) {
        float v = texelFetch(uCube, ivec3(p, t), 0).r;
        if (isnan(v)) continue;
        float dx = texelFetch(uTimes, ivec2(t, 0), 0).r - mxx, dy = v - mv;
        sxx += dx * dx; sxy += dx * dy; syy += dy * dy;
    }
    float sd = n > 1.0 ? sqrt(syy / (n - 1.0)) : 0.0;
    float slope = sxx > 0.0 ? sxy / sxx : nanv;
    float r2 = (sxx > 0.0 && syy > 0.0) ? (sxy * sxy) / (sxx * syy) : nanv;
    o0 = vec4(mv, sd, slope, n);
    o1 = vec4(mn, mx, r2, n > 1.0 ? drop : nanv);
    o2 = dropT;
}
)";

const char* kDisplayFrag = R"(#version 330 core
in vec2 vUV;
out vec4 frag;
uniform int uMode;
uniform int uSource;            // 0 = overview (array), 1 = detail tile
uniform sampler2DArray uCube;
uniform sampler2D uStats0;
uniform sampler2D uStats1;
uniform sampler2D uCmap;
uniform sampler2D uTile;
uniform sampler2D uTile2;       // difference on detail tiles: the reference date's tile
uniform sampler2D uStats2;
uniform ivec3 uLayers;
uniform vec2 uRange;
uniform float uAlpha;
uniform int uClasses;           // 1 = categorical: colour from uClassLut, by class value
uniform sampler2D uClassLut;
uniform int uWarp;              // 1 = reprojected layer: uv through the grid (WarpParams)
uniform sampler2D uWarpGrid;    // the layer's uv at the nodes (RG32F)
uniform vec4 uWarpQuad;         // the quad in the grid's domain: x0, y0, w, h
uniform vec4 uWarpSrc;          // the drawn texture in the layer's uv: x0, y0, w, h

vec2 uv;                        // where the cube / tile / statistics are read

float cubeAt(int t) { return uSource == 1 ? texture(uTile, uv).r : texture(uCube, vec3(uv, float(t))).r; }
float refAt(int t) { return uSource == 1 ? texture(uTile2, uv).r : texture(uCube, vec3(uv, float(t))).r; }
float norm(float v) { return clamp((v - uRange.x) / (uRange.y - uRange.x), 0.0, 1.0); }

// Bilinear between the 4 grid nodes around the point, in full float precision
// (texture filtering weights have only ~8 bits), then into the drawn texture.
vec2 warpedUV() {
    vec2 w = uWarpQuad.xy + vUV * uWarpQuad.zw;
    ivec2 n = textureSize(uWarpGrid, 0);
    vec2 g = w * vec2(n - 1);
    ivec2 i = clamp(ivec2(floor(g)), ivec2(0), n - 2);
    vec2 f = g - vec2(i);
    vec2 a = texelFetch(uWarpGrid, i, 0).rg, b = texelFetch(uWarpGrid, i + ivec2(1, 0), 0).rg;
    vec2 c = texelFetch(uWarpGrid, i + ivec2(0, 1), 0).rg, d = texelFetch(uWarpGrid, i + ivec2(1, 1), 0).rg;
    vec2 l = mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
    return (l - uWarpSrc.xy) / uWarpSrc.zw;
}

void main() {
    uv = vUV;
    if (uWarp == 1) {
        uv = warpedUV();
        if (isnan(uv.x) || isnan(uv.y) || uv.x < 0.0 || uv.y < 0.0 || uv.x >= 1.0 || uv.y >= 1.0) discard;
    }
    if (uMode == 9) {
        float r = cubeAt(uLayers.x), g = cubeAt(uLayers.y), b = cubeAt(uLayers.z);
        if (isnan(r) || isnan(g) || isnan(b)) discard;
        frag = vec4(norm(r), norm(g), norm(b), uAlpha);
        return;
    }
    float v;
    if (uClasses == 1 && uMode == 0) {
        v = cubeAt(uLayers.x);
        if (isnan(v)) discard;
        int c = int(floor(v + 0.5));
        vec4 col = (c >= 0 && c < textureSize(uClassLut, 0).x) ? texelFetch(uClassLut, ivec2(c, 0), 0)
                                                                : vec4(0.5, 0.5, 0.5, 1.0);
        if (col.a == 0.0) discard;
        frag = vec4(col.rgb, uAlpha);
        return;
    }
    if (uMode == 0) v = cubeAt(uLayers.x);
    else if (uMode == 10) v = cubeAt(uLayers.x) - refAt(uLayers.y);
    else {
        vec4 s0 = texture(uStats0, uv), s1 = texture(uStats1, uv);
        if (uMode == 1) v = cubeAt(uLayers.x) - s0.x;
        else if (uMode == 2) v = s0.x;
        else if (uMode == 3) v = s0.y;
        else if (uMode == 4) v = s0.z;
        else if (uMode == 5) v = s1.x;
        else if (uMode == 6) v = s1.y;
        else if (uMode == 7) v = s1.y - s1.x;
        else if (uMode == 8) v = s1.z;
        else if (uMode == 11) v = texture(uStats2, uv).r;
        else v = s1.w;
    }
    if (isnan(v)) discard;
    frag = vec4(texture(uCmap, vec2(norm(v) * (255.0 / 256.0) + 0.5 / 256.0, 0.5)).rgb, uAlpha);
}
)";

GLuint compile(GLenum type, const char* src, std::string& err) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, nullptr);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[2048];
        glGetShaderInfoLog(s, sizeof(log), nullptr, log);
        err += log;
        glDeleteShader(s);
        return 0;
    }
    return s;
}

GLuint link(const char* vs, const char* fs, std::string& err) {
    GLuint v = compile(GL_VERTEX_SHADER, vs, err), f = compile(GL_FRAGMENT_SHADER, fs, err);
    if (!v || !f) return 0;
    GLuint p = glCreateProgram();
    glAttachShader(p, v);
    glAttachShader(p, f);
    glLinkProgram(p);
    glDeleteShader(v);
    glDeleteShader(f);
    GLint ok = 0;
    glGetProgramiv(p, GL_LINK_STATUS, &ok);
    if (!ok) {
        char log[2048];
        glGetProgramInfoLog(p, sizeof(log), nullptr, log);
        err += log;
        glDeleteProgram(p);
        return 0;
    }
    return p;
}

void setNearest(GLenum target) {
    glTexParameteri(target, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(target, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(target, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(target, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
}

GLuint makeTex2D(GLint internal, int w, int h, GLenum fmt, GLenum type, const void* data) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    setNearest(GL_TEXTURE_2D);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, internal, w, h, 0, fmt, type, data);
    return t;
}

} // namespace

static GLuint tex(uint64_t h) { return GLuint(h); }

// ---------------------------------------------------------------------------
// GpuCube
// ---------------------------------------------------------------------------

GpuCube::~GpuCube() {
    GLuint texs[] = {tex(cube), tex(stats0), tex(stats1), tex(stats2), tex(times)};
    glDeleteTextures(5, texs);
}

void GpuCube::create(int w_, int h_, int T_, const std::vector<float>& timesYears, float*, size_t) {
    w = w_;
    h = h_;
    T = T_;
    loaded.assign(T, false);
    GLuint c;
    glGenTextures(1, &c);
    glBindTexture(GL_TEXTURE_2D_ARRAY, c);
    setNearest(GL_TEXTURE_2D_ARRAY);
    glTexImage3D(GL_TEXTURE_2D_ARRAY, 0, GL_R32F, w, h, T, 0, GL_RED, GL_FLOAT, nullptr);
    cube = c;
    times = makeTex2D(GL_R32F, T, 1, GL_RED, GL_FLOAT, timesYears.data());
    stats0 = makeTex2D(GL_RGBA32F, w, h, GL_RGBA, GL_FLOAT, nullptr);
    stats1 = makeTex2D(GL_RGBA32F, w, h, GL_RGBA, GL_FLOAT, nullptr);
    stats2 = makeTex2D(GL_R32F, w, h, GL_RED, GL_FLOAT, nullptr);
}

void GpuCube::uploadLayer(int t, const float* data) {
    glBindTexture(GL_TEXTURE_2D_ARRAY, tex(cube));
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexSubImage3D(GL_TEXTURE_2D_ARRAY, 0, 0, 0, t, w, h, 1, GL_RED, GL_FLOAT, data);
    loaded[t] = true;
}

size_t GpuCube::bytes() const { return size_t(w) * h * (T + 9) * 4; }

// ---------------------------------------------------------------------------
// Gpu
// ---------------------------------------------------------------------------

struct Gpu::Impl {
    GLuint vao = 0;
    GLuint progDisplay = 0, progStats = 0;
    GLuint cmapTex = 0, dummy2D = 0, dummyArray = 0;
    std::map<int, GLuint> overlayCmaps; // colormap textures for overlays, by ImPlot colormap
    struct Target {
        GLuint tex = 0;
        int w = 0, h = 0;
    };
    std::map<int, Target> targets;      // map targets, by panel slot
    GLuint fbo = 0, fboColor = 0;       // fboColor: the target being drawn
    int fboW = 0, fboH = 0;
    GLuint statsFbo = 0;
    const GpuCube* boundCube = nullptr;

    void drawQuad(const float rect[4], int source, const DrawParams& p);
    GLuint colormapTexture(int implotColormap);
    void beginLayer(int implotColormap, float alpha);
    void endLayer();
};

Gpu::Gpu() : impl_(std::make_unique<Impl>()) {}
Gpu::~Gpu() = default;

bool Gpu::init(std::string& error) {
    Impl& d = *impl_;
    d.progDisplay = link(kVertex, kDisplayFrag, error);
    d.progStats = link(kVertex, kStatsFrag, error);
    if (!d.progDisplay || !d.progStats) return false;
    glGenVertexArrays(1, &d.vao);

    glUseProgram(d.progDisplay);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uCube"), 0);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uStats0"), 1);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uStats1"), 2);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uCmap"), 3);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uTile"), 4);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uTile2"), 5);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uStats2"), 7);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uClassLut"), 6);
    glUniform1i(glGetUniformLocation(d.progDisplay, "uWarpGrid"), 8);
    glUseProgram(d.progStats);
    glUniform1i(glGetUniformLocation(d.progStats, "uCube"), 0);
    glUniform1i(glGetUniformLocation(d.progStats, "uTimes"), 1);
    glUseProgram(0);

    const float nan4[4] = {NAN, NAN, NAN, NAN};
    d.dummy2D = makeTex2D(GL_RGBA32F, 1, 1, GL_RGBA, GL_FLOAT, nan4);
    glGenTextures(1, &d.dummyArray);
    glBindTexture(GL_TEXTURE_2D_ARRAY, d.dummyArray);
    setNearest(GL_TEXTURE_2D_ARRAY);
    glTexImage3D(GL_TEXTURE_2D_ARRAY, 0, GL_R32F, 1, 1, 1, 0, GL_RED, GL_FLOAT, nan4);

    glGenFramebuffers(1, &d.fbo);
    glGenFramebuffers(1, &d.statsFbo);
    return true;
}

void Gpu::shutdown() {
    Impl& d = *impl_;
    glDeleteProgram(d.progDisplay);
    glDeleteProgram(d.progStats);
    glDeleteVertexArrays(1, &d.vao);
    GLuint texs[] = {d.cmapTex, d.dummy2D, d.dummyArray};
    glDeleteTextures(3, texs);
    for (auto& [_, t] : d.targets) glDeleteTextures(1, &t.tex);
    d.targets.clear();
    for (auto& [_, t] : d.overlayCmaps) glDeleteTextures(1, &t);
    d.overlayCmaps.clear();
    glDeleteFramebuffers(1, &d.fbo);
    glDeleteFramebuffers(1, &d.statsFbo);
}

int Gpu::maxCubeSide() const {
    GLint maxTex = 0;
    glGetIntegerv(GL_MAX_TEXTURE_SIZE, &maxTex);
    return maxTex;
}

int Gpu::maxDates() const {
    GLint maxLayers = 0;
    glGetIntegerv(GL_MAX_ARRAY_TEXTURE_LAYERS, &maxLayers);
    return maxLayers;
}

int64_t Gpu::maxCubeBytes() const { return INT64_MAX; } // bounded by the texture limits above

static void sampleColormap(int cmap, unsigned char* px) {
    for (int i = 0; i < 256; ++i) {
        ImVec4 c = ImPlot::SampleColormap(i / 255.0f, cmap);
        px[i * 4 + 0] = (unsigned char)(c.x * 255.0f + 0.5f);
        px[i * 4 + 1] = (unsigned char)(c.y * 255.0f + 0.5f);
        px[i * 4 + 2] = (unsigned char)(c.z * 255.0f + 0.5f);
        px[i * 4 + 3] = 255;
    }
}

static GLuint makeColormapTexture(const unsigned char* px) {
    GLuint t = makeTex2D(GL_RGBA8, 256, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    return t;
}

void Gpu::setColormap(int cmap) {
    Impl& d = *impl_;
    unsigned char px[256 * 4];
    sampleColormap(cmap, px);
    if (!d.cmapTex) {
        d.cmapTex = makeColormapTexture(px);
    } else {
        glBindTexture(GL_TEXTURE_2D, d.cmapTex);
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 256, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
    }
}

void Gpu::computeStats(GpuCube& c) {
    Impl& d = *impl_;
    glBindFramebuffer(GL_FRAMEBUFFER, d.statsFbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex(c.stats0), 0);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT1, GL_TEXTURE_2D, tex(c.stats1), 0);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT2, GL_TEXTURE_2D, tex(c.stats2), 0);
    const GLenum bufs[3] = {GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT1, GL_COLOR_ATTACHMENT2};
    glDrawBuffers(3, bufs);
    glViewport(0, 0, c.w, c.h);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glUseProgram(d.progStats);
    glUniform1i(glGetUniformLocation(d.progStats, "uT"), c.T);
    glUniform4f(glGetUniformLocation(d.progStats, "uRect"), -1, -1, 1, 1);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D_ARRAY, tex(c.cube));
    glActiveTexture(GL_TEXTURE1);
    glBindTexture(GL_TEXTURE_2D, tex(c.times));
    glBindVertexArray(d.vao);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glBindVertexArray(0);
    glUseProgram(0);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glActiveTexture(GL_TEXTURE0);

    // Read back through a pixel buffer (a DMA copy) into an uninitialized array:
    // 5.5 Mpx in ~45 ms, against ~75 ms with glGetTexImage into a zeroed vector.
    const size_t n = size_t(c.w) * c.h * 4, total = 2 * n + n / 4;
    GLuint pbo;
    glGenBuffers(1, &pbo);
    glBindBuffer(GL_PIXEL_PACK_BUFFER, pbo);
    glBufferData(GL_PIXEL_PACK_BUFFER, GLsizeiptr(total * 4), nullptr, GL_STREAM_READ);
    glPixelStorei(GL_PACK_ALIGNMENT, 4);
    glBindTexture(GL_TEXTURE_2D, tex(c.stats0));
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_FLOAT, nullptr);
    glBindTexture(GL_TEXTURE_2D, tex(c.stats1));
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_FLOAT, (void*)(n * 4));
    glBindTexture(GL_TEXTURE_2D, tex(c.stats2));
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RED, GL_FLOAT, (void*)(2 * n * 4));
    c.statsCopy.reset(new float[total]);
    const void* m = glMapBufferRange(GL_PIXEL_PACK_BUFFER, 0, GLsizeiptr(total * 4), GL_MAP_READ_BIT);
    if (m) std::memcpy(c.statsCopy.get(), m, total * 4);
    else std::fill(c.statsCopy.get(), c.statsCopy.get() + total, NAN);
    glUnmapBuffer(GL_PIXEL_PACK_BUFFER);
    glBindBuffer(GL_PIXEL_PACK_BUFFER, 0);
    glDeleteBuffers(1, &pbo);
    c.hostStats0 = c.statsCopy.get();
    c.hostStats1 = c.statsCopy.get() + n;
    c.hostStats2 = c.statsCopy.get() + 2 * n;
    c.statsValid = true;
}

void Gpu::beginMap(int w, int h, const float bg[4], int slot) {
    Impl& d = *impl_;
    Impl::Target& tg = d.targets[slot];
    if (w != tg.w || h != tg.h || !tg.tex) {
        if (tg.tex) glDeleteTextures(1, &tg.tex);
        tg.tex = makeTex2D(GL_RGBA8, w, h, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
        tg.w = w;
        tg.h = h;
    }
    d.fboColor = tg.tex;
    d.fboW = w;
    d.fboH = h;
    glBindFramebuffer(GL_FRAMEBUFFER, d.fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, d.fboColor, 0);
    const GLenum buf = GL_COLOR_ATTACHMENT0;
    glDrawBuffers(1, &buf);
    glViewport(0, 0, w, h);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(bg[0], bg[1], bg[2], bg[3]);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(d.progDisplay);
    glUniform1f(glGetUniformLocation(d.progDisplay, "uAlpha"), 1.0f);
    glBindVertexArray(d.vao);
    glActiveTexture(GL_TEXTURE3);
    glBindTexture(GL_TEXTURE_2D, d.cmapTex);
    d.boundCube = nullptr;
}

void Gpu::Impl::drawQuad(const float r[4], int source, const DrawParams& p) {
    const float ndc[4] = {r[0] / fboW * 2.f - 1.f, 1.f - r[1] / fboH * 2.f,
                          r[2] / fboW * 2.f - 1.f, 1.f - r[3] / fboH * 2.f};
    glUniform4fv(glGetUniformLocation(progDisplay, "uRect"), 1, ndc);
    glUniform1i(glGetUniformLocation(progDisplay, "uMode"), p.mode);
    glUniform1i(glGetUniformLocation(progDisplay, "uSource"), source);
    glUniform3i(glGetUniformLocation(progDisplay, "uLayers"), p.t, p.tg, p.tb);
    const float hi = p.hi > p.lo ? p.hi : p.lo + 1e-6f;
    glUniform2f(glGetUniformLocation(progDisplay, "uRange"), p.lo, hi);
    glUniform1i(glGetUniformLocation(progDisplay, "uClasses"), p.classLut ? 1 : 0);
    if (p.classLut) {
        glActiveTexture(GL_TEXTURE6);
        glBindTexture(GL_TEXTURE_2D, tex(p.classLut));
    }
    if (p.tile2) {
        glActiveTexture(GL_TEXTURE5);
        glBindTexture(GL_TEXTURE_2D, tex(p.tile2));
    }
    glUniform1i(glGetUniformLocation(progDisplay, "uWarp"), p.warp.grid ? 1 : 0);
    if (p.warp.grid) {
        glUniform4fv(glGetUniformLocation(progDisplay, "uWarpQuad"), 1, p.warp.quad);
        glUniform4fv(glGetUniformLocation(progDisplay, "uWarpSrc"), 1, p.warp.src);
        glActiveTexture(GL_TEXTURE8);
        glBindTexture(GL_TEXTURE_2D, tex(p.warp.grid));
    }
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

void Gpu::drawCube(const GpuCube& c, const float rect[4], const DrawParams& p) {
    Impl& d = *impl_;
    if (d.boundCube != &c) {
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D_ARRAY, tex(c.cube));
        glActiveTexture(GL_TEXTURE1);
        glBindTexture(GL_TEXTURE_2D, c.statsValid ? tex(c.stats0) : d.dummy2D);
        glActiveTexture(GL_TEXTURE2);
        glBindTexture(GL_TEXTURE_2D, c.statsValid ? tex(c.stats1) : d.dummy2D);
        glActiveTexture(GL_TEXTURE7);
        glBindTexture(GL_TEXTURE_2D, c.statsValid ? tex(c.stats2) : d.dummy2D);
        d.boundCube = &c;
    }
    d.drawQuad(rect, 0, p);
}

void Gpu::drawTile(GpuTex t, const float rect[4], const DrawParams& p) {
    glActiveTexture(GL_TEXTURE4);
    glBindTexture(GL_TEXTURE_2D, tex(t));
    impl_->drawQuad(rect, 1, p);
}

GLuint Gpu::Impl::colormapTexture(int cmap) {
    auto it = overlayCmaps.find(cmap);
    if (it == overlayCmaps.end()) {
        unsigned char px[256 * 4];
        sampleColormap(cmap, px);
        it = overlayCmaps.emplace(cmap, makeColormapTexture(px)).first;
    }
    return it->second;
}

void Gpu::Impl::beginLayer(int cmap, float alpha) {
    glActiveTexture(GL_TEXTURE3);
    glBindTexture(GL_TEXTURE_2D, colormapTexture(cmap));
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glUniform1f(glGetUniformLocation(progDisplay, "uAlpha"), alpha);
}

void Gpu::Impl::endLayer() {
    glUniform1f(glGetUniformLocation(progDisplay, "uAlpha"), 1.0f);
    glDisable(GL_BLEND);
    glActiveTexture(GL_TEXTURE3);
    glBindTexture(GL_TEXTURE_2D, cmapTex);
}

void Gpu::drawCube(const GpuCube& c, const float rect[4], const DrawParams& p, int cmap, float alpha) {
    impl_->beginLayer(cmap, alpha);
    drawCube(c, rect, p);
    impl_->endLayer();
}

void Gpu::drawTile(GpuTex t, const float rect[4], const DrawParams& p, int cmap, float alpha) {
    impl_->beginLayer(cmap, alpha);
    drawTile(t, rect, p);
    impl_->endLayer();
}

void Gpu::drawOverlay(GpuTex t, const float rect[4], float lo, float hi, int cmap, float alpha, const WarpParams* warp) {
    DrawParams p;
    p.mode = ModeValue;
    p.lo = lo;
    p.hi = hi;
    if (warp) p.warp = *warp;
    drawTile(t, rect, p, cmap, alpha);
}

void Gpu::endMap() {
    glBindVertexArray(0);
    glUseProgram(0);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glActiveTexture(GL_TEXTURE0);
}

GpuTex Gpu::mapTexture(int slot) const {
    const auto it = impl_->targets.find(slot);
    return it == impl_->targets.end() ? 0 : it->second.tex;
}

void Gpu::releaseMap(int slot) {
    Impl& d = *impl_;
    const auto it = d.targets.find(slot);
    if (it == d.targets.end()) return;
    glDeleteTextures(1, &it->second.tex);
    d.targets.erase(it);
}

bool Gpu::mapBottomUp() { return true; }

void Gpu::readMapPixel(int x, int y, unsigned char rgba[4], int slot) {
    const Impl::Target& tg = impl_->targets.at(slot);
    std::vector<unsigned char> buf(size_t(tg.w) * tg.h * 4);
    glBindTexture(GL_TEXTURE_2D, tg.tex);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_UNSIGNED_BYTE, buf.data());
    const int row = tg.h - 1 - y; // the framebuffer is bottom-up
    for (int c = 0; c < 4; ++c) rgba[c] = buf[(size_t(row) * tg.w + x) * 4 + c];
}

void Gpu::readMap(std::vector<unsigned char>& rgba, int& w, int& h, int slot) {
    const Impl::Target& tg = impl_->targets.at(slot);
    w = tg.w;
    h = tg.h;
    const size_t row = size_t(w) * 4;
    std::vector<unsigned char> buf(row * h);
    glBindTexture(GL_TEXTURE_2D, tg.tex);
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_UNSIGNED_BYTE, buf.data());
    rgba.resize(row * h);
    for (int y = 0; y < h; ++y) // the framebuffer is bottom-up
        std::memcpy(rgba.data() + row * y, buf.data() + row * (h - 1 - y), row);
}

GpuTex Gpu::createClassLut(const unsigned char* rgba, GpuTex t) {
    if (!t) {
        t = makeTex2D(GL_RGBA8, kClassLutSize, 1, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    } else {
        glBindTexture(GL_TEXTURE_2D, tex(t));
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, kClassLutSize, 1, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    }
    glBindTexture(GL_TEXTURE_2D, tex(t));
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    return t;
}

GpuTex Gpu::createTileTexture(int w, int h, const float* data) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    setNearest(GL_TEXTURE_2D);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_R32F, w, h, 0, GL_RED, GL_FLOAT, data);
    return t;
}

GpuTex Gpu::createWarpGrid(int w, int h, const float* rg) {
    return makeTex2D(GL_RG32F, w, h, GL_RG, GL_FLOAT, rg); // read with texelFetch: no filtering
}

void Gpu::deleteTexture(GpuTex t) {
    const GLuint name = tex(t);
    if (name) glDeleteTextures(1, &name);
}
