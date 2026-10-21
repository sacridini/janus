// The map (gpu_gl.cpp, kVertex and kDisplayFrag): a layer of the cube (or its statistics) through a colormap, a
// detail tile, an RGBA image (a basemap tile, a composite) or a plain colour, on a quad placed in the target,
// straight or through a warp grid (a layer in another CRS). Textures are read as GL_NEAREST reads them: the texel
// under floor(uv * size); images through the sampler chosen (bilinear or nearest).

struct Draw {
    rect: vec4<f32>,       // NDC: x0, y0 (top), x1, y1 (bottom)
    layers: vec4<i32>,     // x, y, z: dates (Rgb: three; Diff: date, reference; Composite: first, last, reducer)
    range: vec2<f32>,
    alpha: f32,
    mode: i32,
    source: i32,           // 0 cube, 1 detail tile, 2 RGBA image, 3 colour
    classes: i32,          // 1: categorical, the colour from the class table by value
    warp: i32,             // 1: through the warp grid
    nearest: i32,          // images: 1 nearest neighbour, 0 bilinear (the sampler bound)
    nan_bits: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    color: vec4<f32>,      // source 3
    warp_quad: vec4<f32>,  // the quad in the grid's domain: x0, y0, w, h
    warp_src: vec4<f32>,   // the drawn texture in the layer's uv: x0, y0, w, h
};

@group(0) @binding(0) var<uniform> u: Draw;

@group(1) @binding(0) var cube: texture_2d_array<f32>;
@group(1) @binding(1) var stats0: texture_2d<f32>;
@group(1) @binding(2) var stats1: texture_2d<f32>;
@group(1) @binding(3) var stats2: texture_2d<f32>;
@group(1) @binding(4) var<storage, read> times: array<f32>;

@group(2) @binding(0) var cmap: texture_2d<f32>;
@group(2) @binding(1) var cmap_sampler: sampler;   // bilinear
@group(2) @binding(2) var image_sampler: sampler;  // bilinear or nearest, chosen per draw (a texture has one
                                                   // sampler on OpenGL)
@group(2) @binding(3) var tile: texture_2d<f32>;
@group(2) @binding(4) var tile2: texture_2d<f32>;
@group(2) @binding(5) var image: texture_2d<f32>;
@group(2) @binding(6) var class_lut: texture_2d<f32>;
@group(2) @binding(7) var warp_grid: texture_2d<f32>;

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

// NaN by its bits: compilers may fold x != x away.
fn is_nan(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u;
}

fn nan() -> f32 {
    return bitcast<f32>(u.nan_bits);
}

var<private> uv: vec2<f32>;

fn texel(size: vec2<u32>) -> vec2<i32> {
    return clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2<i32>(0), vec2<i32>(size) - 1);
}

fn cube_at(t: i32) -> f32 {
    if (u.source == 1) {
        return textureLoad(tile, texel(textureDimensions(tile)), 0).r;
    }
    return textureLoad(cube, texel(textureDimensions(cube)), t, 0).r;
}

fn ref_at(t: i32) -> f32 {
    if (u.source == 1) {
        return textureLoad(tile2, texel(textureDimensions(tile2)), 0).r;
    }
    return textureLoad(cube, texel(textureDimensions(cube)), t, 0).r;
}

fn stats_at(s: texture_2d<f32>) -> vec4<f32> {
    return textureLoad(s, texel(textureDimensions(s)), 0);
}

fn norm(v: f32) -> f32 {
    return clamp((v - u.range.x) / (u.range.y - u.range.x), 0.0, 1.0);
}

// The colour as it leaves: the sRGB bytes it is, into a target that keeps them (GL's framebuffer of the C++, and
// what egui, working in gamma space, shows as they are).
fn out_color(c: vec3<f32>, a: f32) -> vec4<f32> {
    return vec4<f32>(c, a);
}

// Temporal composite of the dates t0..t1 in the season (time not NaN), of the overview: 0 median, 1 mean,
// 2 maximum, 3 minimum, 4 number of observations. The median sorts up to 64 values: with more, every k-th one
// (as compositeValue in app_season.cpp).
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
    if (n < 1.0) { return nan(); }
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

// Bilinear between the 4 grid nodes around the point, in full float precision (texture filtering weights have
// only ~8 bits), then into the drawn texture.
fn warped_uv(vuv: vec2<f32>) -> vec2<f32> {
    let w = u.warp_quad.xy + vuv * u.warp_quad.zw;
    let n = vec2<i32>(textureDimensions(warp_grid));
    let g = w * vec2<f32>(n - 1);
    let i = clamp(vec2<i32>(floor(g)), vec2<i32>(0), n - 2);
    let f = g - vec2<f32>(i);
    let a = textureLoad(warp_grid, i, 0).rg;
    let b = textureLoad(warp_grid, i + vec2<i32>(1, 0), 0).rg;
    let c = textureLoad(warp_grid, i + vec2<i32>(0, 1), 0).rg;
    let d = textureLoad(warp_grid, i + vec2<i32>(1, 1), 0).rg;
    let l = mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
    return (l - u.warp_src.xy) / u.warp_src.zw;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    if (u.source == 3) {
        return out_color(u.color.rgb, u.color.a);
    }
    uv = in.uv;
    if (u.warp == 1) {
        uv = warped_uv(in.uv);
        if (is_nan(uv.x) || is_nan(uv.y) || uv.x < 0.0 || uv.y < 0.0 || uv.x >= 1.0 || uv.y >= 1.0) { discard; }
    }
    if (u.source == 2) {
        // a basemap tile, a composite, embeddings: its colours, bilinear or nearest
        let c = textureSampleLevel(image, image_sampler, uv, 0.0);
        return out_color(c.rgb, c.a * u.alpha);
    }
    let mode = u.mode;
    if (mode == 9) {
        let r = cube_at(u.layers.x);
        let g = cube_at(u.layers.y);
        let b = cube_at(u.layers.z);
        if (is_nan(r) || is_nan(g) || is_nan(b)) { discard; }
        return out_color(vec3<f32>(norm(r), norm(g), norm(b)), u.alpha);
    }
    var v: f32;
    if (u.classes == 1 && mode == 0) {
        v = cube_at(u.layers.x);
        if (is_nan(v)) { discard; }
        let c = i32(floor(v + 0.5));
        let size = i32(textureDimensions(class_lut).x);
        var col = vec4<f32>(0.5, 0.5, 0.5, 1.0);
        if (c >= 0 && c < size) {
            col = textureLoad(class_lut, vec2<i32>(c, 0), 0);
        }
        if (col.a == 0.0) { discard; }
        return out_color(col.rgb, u.alpha);
    }
    if (mode == 0) {
        v = cube_at(u.layers.x);
    } else if (mode == 10) {
        v = cube_at(u.layers.x) - ref_at(u.layers.y);
    } else if (mode == 13) {
        if (u.source == 0) { v = composite(u.layers.x, u.layers.y, u.layers.z); } else { v = nan(); }
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
    return out_color(c.rgb, u.alpha);
}
