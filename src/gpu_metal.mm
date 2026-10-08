// Metal backend of gpu.hpp (macOS). The cube and its statistics live in
// shared-memory buffers read directly by the shaders: on Apple GPUs (unified
// memory) uploading a date is a memcpy, with no staging copy and no texture
// conversion, and the statistics are read back without a GPU copy.
#include "gpu.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>

#include <unistd.h>
#include <map>

#include <implot.h>

#include "metal_context.h"

namespace {

// Same semantics as the GLSL shaders of gpu_gl.cpp: nearest sampling, NaN =
// no data (discarded), the same display modes and statistics. Compiled at run
// time (the Command Line Tools have no offline Metal compiler) with fast math
// off, so isnan() keeps working.
const char* kShaders = R"(
#include <metal_stdlib>
using namespace metal;

struct DrawU {
    float4 rect;        // NDC: x0, y0(top), x1, y1(bottom)
    int mode;
    int source;         // 0 = overview (cube buffer), 1 = detail tile (texture)
    int classes;        // 1 = categorical: colour from the class LUT, by class value
    int hasStats;
    int4 layers;
    float2 range;
    float alpha;
    int pad0;
    int w, h;           // cube size (pixels per date)
    int pad1, pad2;
};

struct VOut {
    float4 pos [[position]];
    float2 uv;
};

vertex VOut vmain(uint vid [[vertex_id]], constant DrawU& u [[buffer(0)]]) {
    float2 c = float2(float(vid & 1), float(vid >> 1));   // triangle strip
    VOut o;
    o.pos = float4(mix(u.rect.xy, u.rect.zw, c), 0.0, 1.0);
    o.uv = c;
    return o;
}

constexpr sampler kNearest(filter::nearest, address::clamp_to_edge);
constexpr sampler kLinear(filter::linear, address::clamp_to_edge);

static float cubeAt(constant DrawU& u, device const float* cube, texture2d<float> tile, float2 uv, uint pix, int t) {
    if (u.source == 1) return tile.sample(kNearest, uv).r;
    return cube[uint(t) * uint(u.w) * uint(u.h) + pix];
}

static float norm(constant DrawU& u, float v) { return clamp((v - u.range.x) / (u.range.y - u.range.x), 0.0, 1.0); }

fragment float4 fmain(VOut in [[stage_in]], constant DrawU& u [[buffer(0)]],
                      device const float* cube [[buffer(1)]],
                      device const float4* stats0 [[buffer(2)]],
                      device const float4* stats1 [[buffer(3)]],
                      texture2d<float> cmap [[texture(0)]],
                      texture2d<float> tile [[texture(1)]],
                      texture2d<float> lut [[texture(2)]]) {
    const int px = min(int(in.uv.x * float(u.w)), u.w - 1), py = min(int(in.uv.y * float(u.h)), u.h - 1);
    const uint pix = uint(py) * uint(u.w) + uint(px);
    if (u.mode == 9) {
        float r = cubeAt(u, cube, tile, in.uv, pix, u.layers.x), g = cubeAt(u, cube, tile, in.uv, pix, u.layers.y),
              b = cubeAt(u, cube, tile, in.uv, pix, u.layers.z);
        if (isnan(r) || isnan(g) || isnan(b)) discard_fragment();
        return float4(norm(u, r), norm(u, g), norm(u, b), u.alpha);
    }
    float v;
    if (u.classes == 1 && u.mode == 0) {
        v = cubeAt(u, cube, tile, in.uv, pix, u.layers.x);
        if (isnan(v)) discard_fragment();
        int c = int(floor(v + 0.5));
        float4 col = (c >= 0 && c < int(lut.get_width())) ? lut.read(uint2(uint(c), 0)) : float4(0.5, 0.5, 0.5, 1.0);
        if (col.a == 0.0) discard_fragment();
        return float4(col.rgb, u.alpha);
    }
    if (u.mode == 0) v = cubeAt(u, cube, tile, in.uv, pix, u.layers.x);
    else {
        const float nanv = as_type<float>(0x7fc00000u);
        float4 s0 = u.hasStats ? stats0[pix] : float4(nanv), s1 = u.hasStats ? stats1[pix] : float4(nanv);
        if (u.mode == 1) v = cubeAt(u, cube, tile, in.uv, pix, u.layers.x) - s0.x;
        else if (u.mode == 2) v = s0.x;
        else if (u.mode == 3) v = s0.y;
        else if (u.mode == 4) v = s0.z;
        else if (u.mode == 5) v = s1.x;
        else if (u.mode == 6) v = s1.y;
        else if (u.mode == 7) v = s1.y - s1.x;
        else v = s1.z;
    }
    if (isnan(v)) discard_fragment();
    return float4(cmap.sample(kLinear, float2(norm(u, v) * (255.0 / 256.0) + 0.5 / 256.0, 0.5)).rgb, u.alpha);
}

struct StatsU {
    int w, h, T, pad;
};

// Per-pixel temporal statistics, in two passes (mean, then centered sums:
// numerically stable). Neighbouring threads read neighbouring pixels.
kernel void stats(constant StatsU& u [[buffer(0)]], device const float* cube [[buffer(1)]],
                  device const float* times [[buffer(2)]], device float4* o0 [[buffer(3)]],
                  device float4* o1 [[buffer(4)]], uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= uint(u.w) || gid.y >= uint(u.h)) return;
    const uint pix = gid.y * uint(u.w) + gid.x, plane = uint(u.w) * uint(u.h);
    const float nanv = as_type<float>(0x7fc00000u);
    float n = 0.0, sv = 0.0, sx = 0.0, mn = 3.0e38, mx = -3.0e38;
    for (int t = 0; t < u.T; ++t) {
        float v = cube[uint(t) * plane + pix];
        if (isnan(v)) continue;
        n += 1.0; sv += v; sx += times[t];
        mn = min(mn, v); mx = max(mx, v);
    }
    if (n < 1.0) { o0[pix] = float4(nanv, nanv, nanv, 0.0); o1[pix] = float4(nanv); return; }
    float mv = sv / n, mxx = sx / n;
    float sxx = 0.0, sxy = 0.0, syy = 0.0;
    for (int t = 0; t < u.T; ++t) {
        float v = cube[uint(t) * plane + pix];
        if (isnan(v)) continue;
        float dx = times[t] - mxx, dy = v - mv;
        sxx += dx * dx; sxy += dx * dy; syy += dy * dy;
    }
    float sd = n > 1.0 ? sqrt(syy / (n - 1.0)) : 0.0;
    float slope = sxx > 0.0 ? sxy / sxx : nanv;
    float r2 = (sxx > 0.0 && syy > 0.0) ? (sxy * sxy) / (sxx * syy) : nanv;
    o0[pix] = float4(mv, sd, slope, n);
    o1[pix] = float4(mn, mx, r2, 0.0);
}
)";

// Mirrors DrawU above (std140-like packing, 80 bytes).
struct DrawUniforms {
    float rect[4];
    int mode, source, classes, hasStats;
    int layers[4];
    float range[2];
    float alpha;
    int pad0;
    int w, h;
    int pad1, pad2;
};
static_assert(sizeof(DrawUniforms) == 80, "must match DrawU in the shader");

struct StatsUniforms {
    int w, h, T, pad;
};

// Retained Metal objects in uint64 handles (GpuCube fields, GpuTex).
uint64_t retain(id obj) { return obj ? uint64_t(uintptr_t(CFBridgingRetain(obj))) : 0; }
template <class T> T borrow(uint64_t h) { return (__bridge T)(void*)uintptr_t(h); }
void release(uint64_t h) {
    if (h) CFRelease((CFTypeRef)(void*)uintptr_t(h));
}

id<MTLTexture> makeTexture(MTLPixelFormat fmt, int w, int h, const void* data, size_t bytesPerRow) {
    id<MTLDevice> dev = janusMetalDevice();
    MTLTextureDescriptor* d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:fmt
                                                                                 width:NSUInteger(w)
                                                                                height:NSUInteger(h)
                                                                             mipmapped:NO];
    d.usage = MTLTextureUsageShaderRead;
    d.storageMode = dev.hasUnifiedMemory ? MTLStorageModeShared : MTLStorageModeManaged;
    id<MTLTexture> t = [dev newTextureWithDescriptor:d];
    if (data) [t replaceRegion:MTLRegionMake2D(0, 0, NSUInteger(w), NSUInteger(h)) mipmapLevel:0 withBytes:data
                   bytesPerRow:bytesPerRow];
    return t;
}

id<MTLBuffer> makeBuffer(size_t bytes) {
    return [janusMetalDevice() newBufferWithLength:std::max<size_t>(bytes, 16) options:MTLResourceStorageModeShared];
}

void sampleColormap(int cmap, unsigned char* px) {
    for (int i = 0; i < 256; ++i) {
        ImVec4 c = ImPlot::SampleColormap(i / 255.0f, cmap);
        px[i * 4 + 0] = (unsigned char)(c.x * 255.0f + 0.5f);
        px[i * 4 + 1] = (unsigned char)(c.y * 255.0f + 0.5f);
        px[i * 4 + 2] = (unsigned char)(c.z * 255.0f + 0.5f);
        px[i * 4 + 3] = 255;
    }
}

} // namespace

// ---------------------------------------------------------------------------
// Device and queue (shared with render_backend_metal.mm)
// ---------------------------------------------------------------------------

id<MTLDevice> janusMetalDevice() {
    static id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    return dev;
}

id<MTLCommandQueue> janusMetalQueue() {
    static id<MTLCommandQueue> q = [janusMetalDevice() newCommandQueue];
    return q;
}

// ---------------------------------------------------------------------------
// GpuCube
// ---------------------------------------------------------------------------

GpuCube::~GpuCube() {
    if (sharesHost) {
        // The buffer outlives us in the command buffers still running, but the
        // memory behind it is the Overview's, freed next: let the queue drain.
        id<MTLCommandBuffer> cb = [janusMetalQueue() commandBuffer];
        [cb commit];
        [cb waitUntilCompleted];
    }
    for (uint64_t h : {cube, stats0, stats1, times}) release(h);
}

void GpuCube::create(int w_, int h_, int T_, const std::vector<float>& timesYears, float* host, size_t hostBytes) {
    w = w_;
    h = h_;
    T = T_;
    loaded.assign(T, false);
    const size_t plane = size_t(w) * h;
    // Unified memory: wrap the Overview's array instead of keeping a second copy
    // (it must be page-aligned and whole pages long).
    const size_t page = size_t(getpagesize());
    id<MTLBuffer> cb = nil;
    if (host && hostBytes >= plane * T * sizeof(float) && uintptr_t(host) % page == 0 && hostBytes % page == 0)
        cb = [janusMetalDevice() newBufferWithBytesNoCopy:host length:hostBytes options:MTLResourceStorageModeShared
                                            deallocator:nil];
    sharesHost = cb != nil;
    cube = retain(cb ? cb : makeBuffer(plane * T * sizeof(float)));
    stats0 = retain(makeBuffer(plane * 4 * sizeof(float)));
    stats1 = retain(makeBuffer(plane * 4 * sizeof(float)));
    id<MTLBuffer> tb = makeBuffer(T * sizeof(float));
    std::memcpy(tb.contents, timesYears.data(), T * sizeof(float));
    times = retain(tb);
}

void GpuCube::uploadLayer(int t, const float* data) {
    // Shared memory: the GPU reads these bytes directly. Only dates not drawn
    // yet are written (loaded[t] is still false), so no frame in flight reads them.
    if (sharesHost) { // already there: the buffer is the Overview's array
        loaded[t] = true;
        return;
    }
    const size_t plane = size_t(w) * h;
    std::memcpy(static_cast<float*>(borrow<id<MTLBuffer>>(cube).contents) + plane * t, data, plane * sizeof(float));
    loaded[t] = true;
}

size_t GpuCube::bytes() const { return size_t(w) * h * (T + 8) * 4; }

// ---------------------------------------------------------------------------
// Gpu
// ---------------------------------------------------------------------------

struct Gpu::Impl {
    id<MTLDevice> dev;
    id<MTLCommandQueue> queue;
    id<MTLRenderPipelineState> pipeOpaque, pipeBlend;
    id<MTLComputePipelineState> pipeStats;
    id<MTLTexture> cmapTex;
    std::map<int, id<MTLTexture>> overlayCmaps; // colormap textures for overlays, by ImPlot colormap
    id<MTLBuffer> dummyBuf;                     // bound where no cube/statistics are
    id<MTLTexture> dummyTile, dummyLut;
    std::map<int, id<MTLTexture>> targets;      // map targets by panel slot (RGBA8, rows top-down)
    id<MTLTexture> target;                      // the one being drawn
    int targetW = 0, targetH = 0;
    id<MTLCommandBuffer> cmd, lastMap;
    id<MTLRenderCommandEncoder> enc;
    // Encoder state of the current map, as in the GL backend: the bound cube
    // stays bound for the tiles drawn after it.
    const GpuCube* boundCube = nullptr;
    int cubeW = 1, cubeH = 1;
    bool hasStats = false, blend = false;
    float alpha = 1.0f;

    id<MTLTexture> colormapTexture(int implotColormap);
    void draw(const float rect[4], int source, const DrawParams& p);
    void beginLayer(int implotColormap, float a);
    void endLayer();
};

Gpu::Gpu() : impl_(std::make_unique<Impl>()) {}
Gpu::~Gpu() = default;

bool Gpu::init(std::string& error) {
    Impl& d = *impl_;
    d.dev = janusMetalDevice();
    d.queue = janusMetalQueue();
    if (!d.dev || !d.queue) {
        error = "no Metal device";
        return false;
    }
    MTLCompileOptions* opts = [MTLCompileOptions new];
    if (@available(macOS 15.0, *)) {
        opts.mathMode = MTLMathModeSafe;
    } else {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        opts.fastMathEnabled = NO;
#pragma clang diagnostic pop
    }
    NSError* err = nil;
    id<MTLLibrary> lib = [d.dev newLibraryWithSource:@(kShaders) options:opts error:&err];
    if (!lib) {
        error = err ? err.localizedDescription.UTF8String : "could not compile the Metal shaders";
        return false;
    }
    MTLRenderPipelineDescriptor* pd = [MTLRenderPipelineDescriptor new];
    pd.vertexFunction = [lib newFunctionWithName:@"vmain"];
    pd.fragmentFunction = [lib newFunctionWithName:@"fmain"];
    pd.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
    d.pipeOpaque = [d.dev newRenderPipelineStateWithDescriptor:pd error:&err];
    // glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA): the same factors for colour and alpha.
    MTLRenderPipelineColorAttachmentDescriptor* ca = pd.colorAttachments[0];
    ca.blendingEnabled = YES;
    ca.sourceRGBBlendFactor = ca.sourceAlphaBlendFactor = MTLBlendFactorSourceAlpha;
    ca.destinationRGBBlendFactor = ca.destinationAlphaBlendFactor = MTLBlendFactorOneMinusSourceAlpha;
    d.pipeBlend = [d.dev newRenderPipelineStateWithDescriptor:pd error:&err];
    d.pipeStats = [d.dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"stats"] error:&err];
    if (!d.pipeOpaque || !d.pipeBlend || !d.pipeStats) {
        error = err ? err.localizedDescription.UTF8String : "could not create the Metal pipelines";
        return false;
    }

    d.dummyBuf = makeBuffer(16 * sizeof(float));
    float* nan = static_cast<float*>(d.dummyBuf.contents);
    for (int i = 0; i < 16; ++i) nan[i] = NAN;
    const float nan1 = NAN;
    d.dummyTile = makeTexture(MTLPixelFormatR32Float, 1, 1, &nan1, 4);
    const unsigned char none[4] = {0, 0, 0, 0};
    d.dummyLut = makeTexture(MTLPixelFormatRGBA8Unorm, 1, 1, none, 4);
    return true;
}

void Gpu::shutdown() {
    Impl& d = *impl_;
    if (d.lastMap) [d.lastMap waitUntilCompleted];
    d.overlayCmaps.clear();
    d.cmapTex = nil;
    d.targets.clear();
    d.target = nil;
    d.lastMap = nil;
}

int Gpu::maxCubeSide() const { return 1 << 20; } // buffers: only the byte limit below
int Gpu::maxDates() const { return 1 << 20; }
int64_t Gpu::maxCubeBytes() const {
    // 32-bit indices in the shaders: at most 2^32 floats per cube.
    return std::min<int64_t>(int64_t(impl_->dev.maxBufferLength), int64_t(4) << 30) * 9 / 10;
}

void Gpu::setColormap(int cmap) {
    Impl& d = *impl_;
    unsigned char px[256 * 4];
    sampleColormap(cmap, px);
    if (!d.cmapTex) d.cmapTex = makeTexture(MTLPixelFormatRGBA8Unorm, 256, 1, px, 256 * 4);
    else [d.cmapTex replaceRegion:MTLRegionMake2D(0, 0, 256, 1) mipmapLevel:0 withBytes:px bytesPerRow:256 * 4];
}

void Gpu::computeStats(GpuCube& c) {
    Impl& d = *impl_;
    id<MTLCommandBuffer> cb = [d.queue commandBuffer];
    id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
    [ce setComputePipelineState:d.pipeStats];
    const StatsUniforms u{c.w, c.h, c.T, 0};
    [ce setBytes:&u length:sizeof(u) atIndex:0];
    [ce setBuffer:borrow<id<MTLBuffer>>(c.cube) offset:0 atIndex:1];
    [ce setBuffer:borrow<id<MTLBuffer>>(c.times) offset:0 atIndex:2];
    [ce setBuffer:borrow<id<MTLBuffer>>(c.stats0) offset:0 atIndex:3];
    [ce setBuffer:borrow<id<MTLBuffer>>(c.stats1) offset:0 atIndex:4];
    [ce dispatchThreads:MTLSizeMake(NSUInteger(c.w), NSUInteger(c.h), 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)];
    [ce endEncoding];
    [cb commit];
    [cb waitUntilCompleted];

    // Shared memory: the CPU reads the results where the kernel wrote them.
    c.hostStats0 = static_cast<const float*>(borrow<id<MTLBuffer>>(c.stats0).contents);
    c.hostStats1 = static_cast<const float*>(borrow<id<MTLBuffer>>(c.stats1).contents);
    c.statsValid = true;
}

void Gpu::beginMap(int w, int h, const float bg[4], int slot) {
    Impl& d = *impl_;
    __strong id<MTLTexture>& tg = d.targets[slot];
    if (!tg || int(tg.width) != w || int(tg.height) != h) {
        MTLTextureDescriptor* td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm
                                                                                      width:NSUInteger(w)
                                                                                     height:NSUInteger(h)
                                                                                  mipmapped:NO];
        td.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead;
        td.storageMode = MTLStorageModePrivate;
        tg = [d.dev newTextureWithDescriptor:td]; // the old one lives on while a frame in flight uses it
    }
    d.target = tg;
    d.targetW = w;
    d.targetH = h;
    MTLRenderPassDescriptor* rp = [MTLRenderPassDescriptor renderPassDescriptor];
    rp.colorAttachments[0].texture = d.target;
    rp.colorAttachments[0].loadAction = MTLLoadActionClear;
    rp.colorAttachments[0].storeAction = MTLStoreActionStore;
    rp.colorAttachments[0].clearColor = MTLClearColorMake(bg[0], bg[1], bg[2], bg[3]);
    d.cmd = [d.queue commandBuffer];
    d.enc = [d.cmd renderCommandEncoderWithDescriptor:rp];
    [d.enc setRenderPipelineState:d.pipeOpaque];
    d.blend = false;
    d.alpha = 1.0f;
    [d.enc setFragmentBuffer:d.dummyBuf offset:0 atIndex:1];
    [d.enc setFragmentBuffer:d.dummyBuf offset:0 atIndex:2];
    [d.enc setFragmentBuffer:d.dummyBuf offset:0 atIndex:3];
    [d.enc setFragmentTexture:d.cmapTex atIndex:0];
    [d.enc setFragmentTexture:d.dummyTile atIndex:1];
    [d.enc setFragmentTexture:d.dummyLut atIndex:2];
    d.boundCube = nullptr;
    d.cubeW = d.cubeH = 1;
    d.hasStats = false;
}

void Gpu::Impl::draw(const float r[4], int source, const DrawParams& p) {
    DrawUniforms u{};
    u.rect[0] = r[0] / targetW * 2.f - 1.f;
    u.rect[1] = 1.f - r[1] / targetH * 2.f;
    u.rect[2] = r[2] / targetW * 2.f - 1.f;
    u.rect[3] = 1.f - r[3] / targetH * 2.f;
    u.mode = p.mode;
    u.source = source;
    u.classes = p.classLut ? 1 : 0;
    u.hasStats = hasStats ? 1 : 0;
    u.layers[0] = p.t;
    u.layers[1] = p.tg;
    u.layers[2] = p.tb;
    u.range[0] = p.lo;
    u.range[1] = p.hi > p.lo ? p.hi : p.lo + 1e-6f;
    u.alpha = alpha;
    u.w = cubeW;
    u.h = cubeH;
    [enc setRenderPipelineState:blend ? pipeBlend : pipeOpaque];
    [enc setVertexBytes:&u length:sizeof(u) atIndex:0];
    [enc setFragmentBytes:&u length:sizeof(u) atIndex:0];
    if (p.classLut) [enc setFragmentTexture:borrow<id<MTLTexture>>(p.classLut) atIndex:2];
    [enc drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4];
}

void Gpu::drawCube(const GpuCube& c, const float rect[4], const DrawParams& p) {
    Impl& d = *impl_;
    if (d.boundCube != &c) {
        [d.enc setFragmentBuffer:borrow<id<MTLBuffer>>(c.cube) offset:0 atIndex:1];
        [d.enc setFragmentBuffer:c.statsValid ? borrow<id<MTLBuffer>>(c.stats0) : d.dummyBuf offset:0 atIndex:2];
        [d.enc setFragmentBuffer:c.statsValid ? borrow<id<MTLBuffer>>(c.stats1) : d.dummyBuf offset:0 atIndex:3];
        d.boundCube = &c;
        d.cubeW = c.w;
        d.cubeH = c.h;
        d.hasStats = c.statsValid;
    }
    d.draw(rect, 0, p);
}

void Gpu::drawTile(GpuTex t, const float rect[4], const DrawParams& p) {
    [impl_->enc setFragmentTexture:borrow<id<MTLTexture>>(t) atIndex:1];
    impl_->draw(rect, 1, p);
}

id<MTLTexture> Gpu::Impl::colormapTexture(int cmap) {
    auto it = overlayCmaps.find(cmap);
    if (it == overlayCmaps.end()) {
        unsigned char px[256 * 4];
        sampleColormap(cmap, px);
        it = overlayCmaps.emplace(cmap, makeTexture(MTLPixelFormatRGBA8Unorm, 256, 1, px, 256 * 4)).first;
    }
    return it->second;
}

void Gpu::Impl::beginLayer(int cmap, float a) {
    [enc setFragmentTexture:colormapTexture(cmap) atIndex:0];
    blend = true;
    alpha = a;
}

void Gpu::Impl::endLayer() {
    alpha = 1.0f;
    blend = false;
    [enc setFragmentTexture:cmapTex atIndex:0];
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

void Gpu::drawOverlay(GpuTex t, const float rect[4], float lo, float hi, int cmap, float alpha) {
    DrawParams p;
    p.mode = ModeValue;
    p.lo = lo;
    p.hi = hi;
    drawTile(t, rect, p, cmap, alpha);
}

void Gpu::endMap() {
    Impl& d = *impl_;
    [d.enc endEncoding];
    [d.cmd commit]; // before the ImGui pass of this frame, on the same queue
    d.lastMap = d.cmd;
    d.enc = nil;
    d.cmd = nil;
}

GpuTex Gpu::mapTexture(int slot) const {
    const auto it = impl_->targets.find(slot);
    return it == impl_->targets.end() ? 0 : uint64_t(uintptr_t((__bridge void*)it->second));
}

void Gpu::releaseMap(int slot) { impl_->targets.erase(slot); } // frames in flight keep their own reference

bool Gpu::mapBottomUp() { return false; }

void Gpu::readMapPixel(int x, int y, unsigned char rgba[4], int slot) {
    Impl& d = *impl_;
    id<MTLBuffer> out = makeBuffer(4);
    id<MTLCommandBuffer> cb = [d.queue commandBuffer];
    id<MTLBlitCommandEncoder> be = [cb blitCommandEncoder];
    [be copyFromTexture:d.targets.at(slot) sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(NSUInteger(x), NSUInteger(y), 0)
             sourceSize:MTLSizeMake(1, 1, 1) toBuffer:out destinationOffset:0 destinationBytesPerRow:4
  destinationBytesPerImage:4];
    [be endEncoding];
    [cb commit];
    [cb waitUntilCompleted];
    std::memcpy(rgba, out.contents, 4);
}

void Gpu::readMap(std::vector<unsigned char>& rgba, int& w, int& h, int slot) {
    Impl& d = *impl_;
    id<MTLTexture> tg = d.targets.at(slot);
    w = int(tg.width);
    h = int(tg.height);
    const size_t row = size_t(w) * 4;
    id<MTLBuffer> out = makeBuffer(row * h);
    id<MTLCommandBuffer> cb = [d.queue commandBuffer];
    id<MTLBlitCommandEncoder> be = [cb blitCommandEncoder];
    [be copyFromTexture:tg sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0)
             sourceSize:MTLSizeMake(NSUInteger(w), NSUInteger(h), 1) toBuffer:out destinationOffset:0
 destinationBytesPerRow:row destinationBytesPerImage:row * h];
    [be endEncoding];
    [cb commit];
    [cb waitUntilCompleted];
    rgba.resize(row * h); // the texture is top-down already
    std::memcpy(rgba.data(), out.contents, row * h);
}

GpuTex Gpu::createClassLut(const unsigned char* rgba, GpuTex t) {
    if (!t) return retain(makeTexture(MTLPixelFormatRGBA8Unorm, kClassLutSize, 1, rgba, kClassLutSize * 4));
    [borrow<id<MTLTexture>>(t) replaceRegion:MTLRegionMake2D(0, 0, kClassLutSize, 1) mipmapLevel:0 withBytes:rgba
                                 bytesPerRow:kClassLutSize * 4];
    return t;
}

GpuTex Gpu::createTileTexture(int w, int h, const float* data) {
    return retain(makeTexture(MTLPixelFormatR32Float, w, h, data, size_t(w) * sizeof(float)));
}

void Gpu::deleteTexture(GpuTex t) { release(t); }
