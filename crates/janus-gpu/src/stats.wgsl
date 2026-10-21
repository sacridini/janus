// Per-pixel temporal statistics of the overview, in two passes (mean, then centred sums: numerically stable).
// The largest drop is the largest decrease between consecutive valid observations (NaN skipped), dated at the
// later one; 0 and no date if the series never decreases. Dates whose time is NaN are outside the season and left
// out. The C++'s kStatsFrag (gpu_gl.cpp) as a compute shader: the same operations in the same order.

struct Params {
    t: u32,
    nan_bits: u32, // 0x7fc00000: a NaN made at run time (a NaN constant is an error in WGSL)
};

@group(0) @binding(0) var cube: texture_2d_array<f32>;
@group(0) @binding(1) var<storage, read> times: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var o0: texture_storage_2d<rgba32float, write>; // mean, std, slope, n
@group(0) @binding(4) var o1: texture_storage_2d<rgba32float, write>; // min, max, R2, largest drop
@group(0) @binding(5) var o2: texture_storage_2d<r32float, write>;    // date index of the largest drop

// NaN by its bits: compilers may fold x != x away.
fn is_nan(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(o0);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let px = vec2<i32>(id.xy);
    let nanv = bitcast<f32>(p.nan_bits);
    var n = 0.0;
    var sv = 0.0;
    var sx = 0.0;
    var mn = 3.0e38;
    var mx = -3.0e38;
    var last = 0.0;
    var drop = 0.0;
    var drop_t = nanv;
    for (var t = 0u; t < p.t; t++) {
        let x = times[t];
        if (is_nan(x)) { continue; }
        let v = textureLoad(cube, px, t, 0).r;
        if (is_nan(v)) { continue; }
        if (n > 0.0 && last - v > drop) {
            drop = last - v;
            drop_t = f32(t);
        }
        last = v;
        n += 1.0;
        sv += v;
        sx += x;
        mn = min(mn, v);
        mx = max(mx, v);
    }
    if (n < 1.0) {
        textureStore(o0, px, vec4<f32>(nanv, nanv, nanv, 0.0));
        textureStore(o1, px, vec4<f32>(nanv));
        textureStore(o2, px, vec4<f32>(nanv));
        return;
    }
    let mv = sv / n;
    let mxx = sx / n;
    var sxx = 0.0;
    var sxy = 0.0;
    var syy = 0.0;
    for (var t = 0u; t < p.t; t++) {
        let x = times[t];
        if (is_nan(x)) { continue; }
        let v = textureLoad(cube, px, t, 0).r;
        if (is_nan(v)) { continue; }
        let dx = x - mxx;
        let dy = v - mv;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    var sd = 0.0;
    if (n > 1.0) { sd = sqrt(syy / (n - 1.0)); }
    var slope = nanv;
    if (sxx > 0.0) { slope = sxy / sxx; }
    var r2 = nanv;
    if (sxx > 0.0 && syy > 0.0) { r2 = (sxy * sxy) / (sxx * syy); }
    var d = nanv;
    if (n > 1.0) { d = drop; }
    textureStore(o0, px, vec4<f32>(mv, sd, slope, n));
    textureStore(o1, px, vec4<f32>(mn, mx, r2, d));
    textureStore(o2, px, vec4<f32>(drop_t));
}
