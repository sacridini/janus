// The map: a layer of the cube (or its statistics) through the colormap, on a quad placed in the target. The C++'s
// kVertex and kDisplayFrag (gpu_gl.cpp) for the overview; detail tiles, RGBA images, the warp grid and classes
// come in phase 3. Textures are read as GL_NEAREST reads them: the texel under floor(uv * size).

struct View {
    rect: vec4<f32>,     // NDC: x0, y0 (top), x1, y1 (bottom)
    layers: vec4<i32>,   // x, y, z: dates (or the composite's first and last date and reducer)
    range: vec2<f32>,
    alpha: f32,
    mode: i32,
    t: u32,
    nan_bits: u32,
};

@group(0) @binding(0) var<uniform> u: View;
@group(0) @binding(1) var cube: texture_2d_array<f32>;
@group(0) @binding(2) var stats0: texture_2d<f32>;
@group(0) @binding(3) var stats1: texture_2d<f32>;
@group(0) @binding(4) var stats2: texture_2d<f32>;
@group(0) @binding(5) var cmap: texture_2d<f32>;
@group(0) @binding(6) var cmap_sampler: sampler;
@group(0) @binding(7) var<storage, read> times: array<f32>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let c = vec2<f32>(f32(i & 1u), f32(i >> 1u)); // triangle strip
    var o: VOut;
    o.pos = vec4<f32>(mix(u.rect.xy, u.rect.zw, c), 0.0, 1.0);
    o.uv = c;
    return o;
}

fn is_nan(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u;
}

var<private> uv: vec2<f32>;

fn texel(size: vec2<u32>) -> vec2<i32> {
    return clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2<i32>(0), vec2<i32>(size) - 1);
}

fn cube_at(t: i32) -> f32 {
    return textureLoad(cube, texel(textureDimensions(cube)), t, 0).r;
}

fn stats_at(s: texture_2d<f32>) -> vec4<f32> {
    return textureLoad(s, texel(textureDimensions(s)), 0);
}

// The target is sRGB: colours leave linear and are stored as the sRGB bytes they were.
fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

fn norm(v: f32) -> f32 {
    return clamp((v - u.range.x) / (u.range.y - u.range.x), 0.0, 1.0);
}

// Temporal composite of the dates t0..t1 in the season (time not NaN): 0 median, 1 mean, 2 maximum, 3 minimum,
// 4 number of observations. The median sorts up to 64 values: with more, every k-th one (as compositeValue in
// app_season.cpp).
fn composite(t0: i32, t1: i32, reducer: i32) -> f32 {
    var n = 0.0;
    var s = 0.0;
    var mn = 3.0e38;
    var mx = -3.0e38;
    for (var t = t0; t <= t1; t++) {
        if (is_nan(times[t])) { continue; }
        let v = cube_at(t);
        if (is_nan(v)) { continue; }
        n += 1.0;
        s += v;
        mn = min(mn, v);
        mx = max(mx, v);
    }
    if (reducer == 4) { return n; }
    if (n < 1.0) { return bitcast<f32>(u.nan_bits); }
    if (reducer == 1) { return s / n; }
    if (reducer == 2) { return mx; }
    if (reducer == 3) { return mn; }
    var vals: array<f32, 64>;
    let k = i32(ceil(n / 64.0));
    var i = 0;
    var m = 0;
    for (var t = t0; t <= t1 && m < 64; t++) {
        if (is_nan(times[t])) { continue; }
        let v = cube_at(t);
        if (is_nan(v)) { continue; }
        if (i % k == 0) {
            var j = m;
            m++;
            while (j > 0 && vals[j - 1] > v) {
                vals[j] = vals[j - 1];
                j--;
            }
            vals[j] = v;
        }
        i++;
    }
    if (m % 2 == 1) { return vals[m / 2]; }
    return 0.5 * (vals[m / 2 - 1] + vals[m / 2]);
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    uv = in.uv;
    let mode = u.mode;
    if (mode == 9) {
        let r = cube_at(u.layers.x);
        let g = cube_at(u.layers.y);
        let b = cube_at(u.layers.z);
        if (is_nan(r) || is_nan(g) || is_nan(b)) { discard; }
        return vec4<f32>(linear(vec3<f32>(norm(r), norm(g), norm(b))), u.alpha);
    }
    var v: f32;
    if (mode == 0) {
        v = cube_at(u.layers.x);
    } else if (mode == 10) {
        v = cube_at(u.layers.x) - cube_at(u.layers.y);
    } else if (mode == 13) {
        v = composite(u.layers.x, u.layers.y, u.layers.z);
    } else {
        let s0 = stats_at(stats0);
        let s1 = stats_at(stats1);
        if (mode == 1) { v = cube_at(u.layers.x) - s0.x; }
        else if (mode == 2) { v = s0.x; }
        else if (mode == 3) { v = s0.y; }
        else if (mode == 4) { v = s0.z; }
        else if (mode == 5) { v = s1.x; }
        else if (mode == 6) { v = s1.y; }
        else if (mode == 7) { v = s1.y - s1.x; }
        else if (mode == 8) { v = s1.z; }
        else if (mode == 11) { v = stats_at(stats2).r; }
        else { v = s1.w; }
    }
    if (is_nan(v)) { discard; }
    let c = textureSampleLevel(cmap, cmap_sampler, vec2<f32>(norm(v) * (255.0 / 256.0) + 0.5 / 256.0, 0.5), 0.0);
    return vec4<f32>(linear(c.rgb), u.alpha);
}
