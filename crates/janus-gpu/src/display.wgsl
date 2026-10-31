// The map (gpu_gl.cpp, kVertex and kDisplayFrag): a layer of the cube (or its statistics) through a colormap, a
// detail tile, an RGBA image (a basemap tile, a composite) or a plain colour, on a quad placed in the target,
// straight or through a warp grid (a layer in another CRS). Textures are read as GL_NEAREST reads them: the texel
// under floor(uv * size); images through the sampler chosen (bilinear or nearest).

struct Draw {
    rect: vec4<f32>,       // NDC: x0, y0 (top), x1, y1 (bottom)
    layers: vec4<i32>,     // x, y, z: dates (Rgb: three; Diff: date, reference; Composite: first, last, reducer;
                           // Similarity: x 0 the values, 1 the shape; Phase, as f32 bits: x the first date's
                           // fraction of its year, y and z the mean's range (NaN: no brightness by the mean))
    range: vec2<f32>,
    alpha: f32,
    mode: i32,
    source: i32,           // 0 cube, 1 detail tile, 2 RGBA image, 3 colour
    classes: i32,          // 1: categorical, the colour from the class table by value
    warp: i32,             // 1: through the warp grid
    nearest: i32,          // images: 1 nearest neighbour, 0 bilinear (the sampler bound)
    nan_bits: u32,
    blend: i32,            // Blend: 0 normal, 1 multiply, 2 screen, 3 add, 4 darken, 5 lighten
    highlight: vec2<f32>,  // the values highlighted (lo, hi; none unless hi > lo): the others dimmed
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
// the similarity's reference: its value at every date, then the values' scale
@group(1) @binding(5) var<storage, read> reference: array<f32>;

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
// A layer's blend mode (the pipeline's blend state does the rest, lib.rs blend_state): multiply and darken take
// the colour faded to white by the opacity, screen, add and lighten the colour premultiplied by it; the target's
// alpha is kept.
fn out_color(c: vec3<f32>, a: f32) -> vec4<f32> {
    if (u.blend == 1 || u.blend == 4) {
        return vec4<f32>(mix(vec3<f32>(1.0), c, a), a);
    }
    if (u.blend == 2 || u.blend == 3 || u.blend == 5) {
        return vec4<f32>(c * a, a);
    }
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

// The pixel's series against the reference, over the dates both have (in the season): the shape, their
// correlation; the values, 1 - their RMS difference over the values' scale (1: the same values). In two passes,
// the means first (no cancellation in f32).
fn similarity(shape: bool) -> f32 {
    let n = i32(arrayLength(&reference)) - 1;
    var c = 0.0;
    var mv = 0.0;
    var mr = 0.0;
    for (var t = 0; t < n; t++) {
        let r = reference[t];
        if (is_nan(times[t]) || is_nan(r)) { continue; }
        let v = cube_at(t);
        if (is_nan(v)) { continue; }
        c += 1.0;
        mv += v;
        mr += r;
    }
    if (c < 3.0) { return nan(); }
    mv /= c;
    mr /= c;
    var vv = 0.0;
    var rr = 0.0;
    var vr = 0.0;
    var dd = 0.0;
    for (var t = 0; t < n; t++) {
        let r = reference[t];
        if (is_nan(times[t]) || is_nan(r)) { continue; }
        let v = cube_at(t);
        if (is_nan(v)) { continue; }
        let a = v - mv;
        let b = r - mr;
        vv += a * a;
        rr += b * b;
        vr += a * b;
        let d = v - r;
        dd += d * d;
    }
    if (shape) {
        if (vv <= 0.0 || rr <= 0.0) { return nan(); }
        return vr / sqrt(vv * rr);
    }
    return 1.0 - sqrt(dd / c) / max(reference[n], 1e-30);
}

// When in the year the series peaks: v = m + a cos(θ) + b sin(θ), θ the date's angle in its year (2π times the
// fraction of its time, in years from the first date, plus the first date's fraction of its year), fitted by least squares over the dates with a value (in the season), 6 at least
// and spread over the year (the normal matrix, over their number, of determinant 0.02 at least; 0.25 for dates all
// round the year). The values from the first one (no cancellation in f32). Returns the peak as a fraction of the
// year, the peak-to-peak amplitude 2·sqrt(a² + b²) and the mean m; x NaN without a fit.
fn harmonic(offset: f32) -> vec3<f32> {
    let n = i32(textureNumLayers(cube));
    var v0 = 0.0;
    var c = 0.0;
    var sc = 0.0;
    var ss = 0.0;
    var scc = 0.0;
    var sss = 0.0;
    var scs = 0.0;
    var sv = 0.0;
    var svc = 0.0;
    var svs = 0.0;
    for (var t = 0; t < n; t++) {
        let y = times[t];
        if (is_nan(y)) { continue; }
        var v = cube_at(t);
        if (is_nan(v)) { continue; }
        if (c == 0.0) { v0 = v; }
        v -= v0;
        let th = 6.28318531 * fract(y + offset);
        let co = cos(th);
        let si = sin(th);
        c += 1.0;
        sc += co;
        ss += si;
        scc += co * co;
        sss += si * si;
        scs += co * si;
        sv += v;
        svc += v * co;
        svs += v * si;
    }
    if (c < 6.0) { return vec3<f32>(nan(), 0.0, 0.0); }
    let c0 = vec3<f32>(1.0, sc / c, ss / c);
    let c1 = vec3<f32>(sc / c, scc / c, scs / c);
    let c2 = vec3<f32>(ss / c, scs / c, sss / c);
    let r = vec3<f32>(sv / c, svc / c, svs / c);
    let d = determinant(mat3x3<f32>(c0, c1, c2));
    if (!(d >= 0.02)) { return vec3<f32>(nan(), 0.0, 0.0); }
    let m = determinant(mat3x3<f32>(r, c1, c2)) / d;
    let a = determinant(mat3x3<f32>(c0, r, c2)) / d;
    let b = determinant(mat3x3<f32>(c0, c1, r)) / d;
    let peak = fract(atan2(b, a) / 6.28318531 + 1.0);
    return vec3<f32>(peak, 2.0 * sqrt(a * a + b * b), m + v0);
}

// A colour dimmed to a dark grey: a value outside the highlight.
fn dimmed(c: vec3<f32>, v: f32) -> vec3<f32> {
    if (u.highlight.y > u.highlight.x && !(v >= u.highlight.x && v <= u.highlight.y)) {
        return vec3<f32>(0.06 + 0.3 * dot(c, vec3<f32>(0.299, 0.587, 0.114)));
    }
    return c;
}

// HSV (each 0-1) to RGB.
fn hsv(h: f32, s: f32, v: f32) -> vec3<f32> {
    let p = (vec3<f32>(5.0, 3.0, 1.0) + h * 6.0) % vec3<f32>(6.0);
    return v - v * s * clamp(min(p, 4.0 - p), vec3<f32>(0.0), vec3<f32>(1.0));
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
    if (mode == 15) {
        // the peak's time of year as the hue, the amplitude (the stretch) as the saturation, the mean as the
        // brightness (from a quarter, the hue still seen) or full
        if (u.source != 0) { discard; }
        let h = harmonic(bitcast<f32>(u.layers.x));
        if (is_nan(h.x)) { discard; }
        var bright = 1.0;
        let lo = bitcast<f32>(u.layers.y);
        let hi = bitcast<f32>(u.layers.z);
        if (!is_nan(lo) && !is_nan(hi)) {
            bright = 0.25 + 0.75 * clamp((h.z - lo) / max(hi - lo, 1e-30), 0.0, 1.0);
        }
        return out_color(dimmed(hsv(h.x, norm(h.y), bright), h.y), u.alpha);
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
    } else if (mode == 14) {
        if (u.source == 0) { v = similarity(u.layers.x == 1); } else { v = nan(); }
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
    return out_color(dimmed(c.rgb, v), u.alpha);
}
