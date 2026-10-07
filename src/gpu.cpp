#include "gpu.hpp"

#include <cmath>
#include <string>

#include <implot.h>

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
// centered sums: numerically stable).
const char* kStatsFrag = R"(#version 330 core
uniform sampler2DArray uCube;
uniform sampler2D uTimes;
uniform int uT;
layout(location = 0) out vec4 o0;   // mean, std, slope, n
layout(location = 1) out vec4 o1;   // min, max, R2, -
void main() {
    ivec2 p = ivec2(gl_FragCoord.xy);
    float nanv = uintBitsToFloat(0x7fc00000u);
    float n = 0.0, sv = 0.0, sx = 0.0, mn = 3.0e38, mx = -3.0e38;
    for (int t = 0; t < uT; ++t) {
        float v = texelFetch(uCube, ivec3(p, t), 0).r;
        if (isnan(v)) continue;
        n += 1.0; sv += v; sx += texelFetch(uTimes, ivec2(t, 0), 0).r;
        mn = min(mn, v); mx = max(mx, v);
    }
    if (n < 1.0) { o0 = vec4(nanv, nanv, nanv, 0.0); o1 = vec4(nanv); return; }
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
    o1 = vec4(mn, mx, r2, 0.0);
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
uniform ivec3 uLayers;
uniform vec2 uRange;
uniform float uAlpha;

float cubeAt(int t) { return uSource == 1 ? texture(uTile, vUV).r : texture(uCube, vec3(vUV, float(t))).r; }
float norm(float v) { return clamp((v - uRange.x) / (uRange.y - uRange.x), 0.0, 1.0); }

void main() {
    if (uMode == 9) {
        float r = cubeAt(uLayers.x), g = cubeAt(uLayers.y), b = cubeAt(uLayers.z);
        if (isnan(r) || isnan(g) || isnan(b)) discard;
        frag = vec4(norm(r), norm(g), norm(b), uAlpha);
        return;
    }
    float v;
    if (uMode == 0) v = cubeAt(uLayers.x);
    else {
        vec4 s0 = texture(uStats0, vUV), s1 = texture(uStats1, vUV);
        if (uMode == 1) v = cubeAt(uLayers.x) - s0.x;
        else if (uMode == 2) v = s0.x;
        else if (uMode == 3) v = s0.y;
        else if (uMode == 4) v = s0.z;
        else if (uMode == 5) v = s1.x;
        else if (uMode == 6) v = s1.y;
        else if (uMode == 7) v = s1.y - s1.x;
        else v = s1.z;
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

// ---------------------------------------------------------------------------
// GpuCube
// ---------------------------------------------------------------------------

GpuCube::~GpuCube() {
    GLuint texs[] = {cube, stats0, stats1, times};
    glDeleteTextures(4, texs);
}

void GpuCube::create(int w_, int h_, int T_, const std::vector<float>& timesYears) {
    w = w_;
    h = h_;
    T = T_;
    loaded.assign(T, false);
    glGenTextures(1, &cube);
    glBindTexture(GL_TEXTURE_2D_ARRAY, cube);
    setNearest(GL_TEXTURE_2D_ARRAY);
    glTexImage3D(GL_TEXTURE_2D_ARRAY, 0, GL_R32F, w, h, T, 0, GL_RED, GL_FLOAT, nullptr);
    times = makeTex2D(GL_R32F, T, 1, GL_RED, GL_FLOAT, timesYears.data());
    stats0 = makeTex2D(GL_RGBA32F, w, h, GL_RGBA, GL_FLOAT, nullptr);
    stats1 = makeTex2D(GL_RGBA32F, w, h, GL_RGBA, GL_FLOAT, nullptr);
}

void GpuCube::uploadLayer(int t, const float* data) {
    glBindTexture(GL_TEXTURE_2D_ARRAY, cube);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexSubImage3D(GL_TEXTURE_2D_ARRAY, 0, 0, 0, t, w, h, 1, GL_RED, GL_FLOAT, data);
    loaded[t] = true;
}

size_t GpuCube::bytes() const { return size_t(w) * h * (T + 8) * 4; }

// ---------------------------------------------------------------------------
// Gpu
// ---------------------------------------------------------------------------

bool Gpu::init(std::string& error) {
    progDisplay_ = link(kVertex, kDisplayFrag, error);
    progStats_ = link(kVertex, kStatsFrag, error);
    if (!progDisplay_ || !progStats_) return false;
    glGenVertexArrays(1, &vao_);

    glUseProgram(progDisplay_);
    glUniform1i(glGetUniformLocation(progDisplay_, "uCube"), 0);
    glUniform1i(glGetUniformLocation(progDisplay_, "uStats0"), 1);
    glUniform1i(glGetUniformLocation(progDisplay_, "uStats1"), 2);
    glUniform1i(glGetUniformLocation(progDisplay_, "uCmap"), 3);
    glUniform1i(glGetUniformLocation(progDisplay_, "uTile"), 4);
    glUseProgram(progStats_);
    glUniform1i(glGetUniformLocation(progStats_, "uCube"), 0);
    glUniform1i(glGetUniformLocation(progStats_, "uTimes"), 1);
    glUseProgram(0);

    const float nan4[4] = {NAN, NAN, NAN, NAN};
    dummy2D_ = makeTex2D(GL_RGBA32F, 1, 1, GL_RGBA, GL_FLOAT, nan4);
    glGenTextures(1, &dummyArray_);
    glBindTexture(GL_TEXTURE_2D_ARRAY, dummyArray_);
    setNearest(GL_TEXTURE_2D_ARRAY);
    glTexImage3D(GL_TEXTURE_2D_ARRAY, 0, GL_R32F, 1, 1, 1, 0, GL_RED, GL_FLOAT, nan4);

    glGenFramebuffers(1, &fbo_);
    glGenFramebuffers(1, &statsFbo_);
    return true;
}

void Gpu::shutdown() {
    glDeleteProgram(progDisplay_);
    glDeleteProgram(progStats_);
    glDeleteVertexArrays(1, &vao_);
    GLuint texs[] = {cmapTex_, dummy2D_, dummyArray_, fboColor_};
    glDeleteTextures(4, texs);
    for (auto& [_, t] : overlayCmaps_) glDeleteTextures(1, &t);
    overlayCmaps_.clear();
    glDeleteFramebuffers(1, &fbo_);
    glDeleteFramebuffers(1, &statsFbo_);
}

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
    unsigned char px[256 * 4];
    sampleColormap(cmap, px);
    if (!cmapTex_) {
        cmapTex_ = makeTex2D(GL_RGBA8, 256, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    } else {
        glBindTexture(GL_TEXTURE_2D, cmapTex_);
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, 256, 1, GL_RGBA, GL_UNSIGNED_BYTE, px);
    }
}

void Gpu::computeStats(GpuCube& c, std::vector<float>& s0, std::vector<float>& s1) {
    glBindFramebuffer(GL_FRAMEBUFFER, statsFbo_);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, c.stats0, 0);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT1, GL_TEXTURE_2D, c.stats1, 0);
    const GLenum bufs[2] = {GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT1};
    glDrawBuffers(2, bufs);
    glViewport(0, 0, c.w, c.h);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glUseProgram(progStats_);
    glUniform1i(glGetUniformLocation(progStats_, "uT"), c.T);
    glUniform4f(glGetUniformLocation(progStats_, "uRect"), -1, -1, 1, 1);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D_ARRAY, c.cube);
    glActiveTexture(GL_TEXTURE1);
    glBindTexture(GL_TEXTURE_2D, c.times);
    glBindVertexArray(vao_);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    glBindVertexArray(0);
    glUseProgram(0);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glActiveTexture(GL_TEXTURE0);

    s0.resize(size_t(c.w) * c.h * 4);
    s1.resize(size_t(c.w) * c.h * 4);
    glPixelStorei(GL_PACK_ALIGNMENT, 4);
    glBindTexture(GL_TEXTURE_2D, c.stats0);
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_FLOAT, s0.data());
    glBindTexture(GL_TEXTURE_2D, c.stats1);
    glGetTexImage(GL_TEXTURE_2D, 0, GL_RGBA, GL_FLOAT, s1.data());
    c.statsValid = true;
}

void Gpu::beginMap(int w, int h, const float bg[4]) {
    if (w != fboW_ || h != fboH_) {
        if (fboColor_) glDeleteTextures(1, &fboColor_);
        fboColor_ = makeTex2D(GL_RGBA8, w, h, GL_RGBA, GL_UNSIGNED_BYTE, nullptr);
        glBindFramebuffer(GL_FRAMEBUFFER, fbo_);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, fboColor_, 0);
        fboW_ = w;
        fboH_ = h;
    }
    glBindFramebuffer(GL_FRAMEBUFFER, fbo_);
    const GLenum buf = GL_COLOR_ATTACHMENT0;
    glDrawBuffers(1, &buf);
    glViewport(0, 0, w, h);
    glDisable(GL_BLEND);
    glDisable(GL_SCISSOR_TEST);
    glClearColor(bg[0], bg[1], bg[2], bg[3]);
    glClear(GL_COLOR_BUFFER_BIT);
    glUseProgram(progDisplay_);
    glUniform1f(glGetUniformLocation(progDisplay_, "uAlpha"), 1.0f);
    glBindVertexArray(vao_);
    glActiveTexture(GL_TEXTURE3);
    glBindTexture(GL_TEXTURE_2D, cmapTex_);
    boundCube_ = nullptr;
}

void Gpu::drawQuad(const float r[4], int source, const DrawParams& p) {
    const float ndc[4] = {r[0] / fboW_ * 2.f - 1.f, 1.f - r[1] / fboH_ * 2.f,
                          r[2] / fboW_ * 2.f - 1.f, 1.f - r[3] / fboH_ * 2.f};
    glUniform4fv(glGetUniformLocation(progDisplay_, "uRect"), 1, ndc);
    glUniform1i(glGetUniformLocation(progDisplay_, "uMode"), p.mode);
    glUniform1i(glGetUniformLocation(progDisplay_, "uSource"), source);
    glUniform3i(glGetUniformLocation(progDisplay_, "uLayers"), p.t, p.tg, p.tb);
    const float hi = p.hi > p.lo ? p.hi : p.lo + 1e-6f;
    glUniform2f(glGetUniformLocation(progDisplay_, "uRange"), p.lo, hi);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

void Gpu::drawCube(const GpuCube& c, const float rect[4], const DrawParams& p) {
    if (boundCube_ != &c) {
        glActiveTexture(GL_TEXTURE0);
        glBindTexture(GL_TEXTURE_2D_ARRAY, c.cube);
        glActiveTexture(GL_TEXTURE1);
        glBindTexture(GL_TEXTURE_2D, c.statsValid ? c.stats0 : dummy2D_);
        glActiveTexture(GL_TEXTURE2);
        glBindTexture(GL_TEXTURE_2D, c.statsValid ? c.stats1 : dummy2D_);
        boundCube_ = &c;
    }
    drawQuad(rect, 0, p);
}

void Gpu::drawTile(GLuint tex, const float rect[4], const DrawParams& p) {
    glActiveTexture(GL_TEXTURE4);
    glBindTexture(GL_TEXTURE_2D, tex);
    drawQuad(rect, 1, p);
}

void Gpu::drawOverlay(GLuint tex, const float rect[4], float lo, float hi, int cmap, float alpha) {
    auto it = overlayCmaps_.find(cmap);
    if (it == overlayCmaps_.end()) {
        unsigned char px[256 * 4];
        sampleColormap(cmap, px);
        it = overlayCmaps_.emplace(cmap, makeColormapTexture(px)).first;
    }
    glActiveTexture(GL_TEXTURE3);
    glBindTexture(GL_TEXTURE_2D, it->second);
    glEnable(GL_BLEND);
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glUniform1f(glGetUniformLocation(progDisplay_, "uAlpha"), alpha);
    DrawParams p;
    p.mode = ModeValue;
    p.lo = lo;
    p.hi = hi;
    drawTile(tex, rect, p);
    glUniform1f(glGetUniformLocation(progDisplay_, "uAlpha"), 1.0f);
    glDisable(GL_BLEND);
    glActiveTexture(GL_TEXTURE3);
    glBindTexture(GL_TEXTURE_2D, cmapTex_);
}

void Gpu::endMap() {
    glBindVertexArray(0);
    glUseProgram(0);
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glActiveTexture(GL_TEXTURE0);
}

GLuint Gpu::createTileTexture(int w, int h, const float* data) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    setNearest(GL_TEXTURE_2D);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_R32F, w, h, 0, GL_RED, GL_FLOAT, data);
    return t;
}
