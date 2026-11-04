// Vector layers on the map: lines (each segment a capsule: the target pixels within a distance of it, a core and a
// dark rim around it, feathered at its edge), polygons' fills (triangles) and points (squares or discs). A layer's
// lines are the union of their capsules, each pixel shaded once by the segment nearest to it: a first pass writes
// that distance as the depth (the least kept), the second blends the colour where it is the least (depth equal),
// once (the stencil marks the pixel with the draw's number). Quads blended one over the other made specks where a
// line turned back on itself, where vertices almost touched and along segments shorter than a pixel. Each draw has
// a band of depths of its own, the later nearer, so a layer's lines are drawn over the earlier ones.
//
// A level of a layer's points is one storage buffer (x, y relative to the origin of the feature's piece, in the
// map's source pixels; the feature's index); a NaN x breaks a line. The pieces' origins are split in two floats
// (high and low parts) as is the view's centre, so float32 keeps a double's precision where it matters: near the
// view. Each feature's colour and piece come from the layer's buffers.

struct U {
    // target pixels per source pixel
    a: vec2<f32>,
    // the view's centre (source pixels): high and low parts
    c_hi: vec2<f32>,
    c_lo: vec2<f32>,
    // the view's centre in target pixels
    t0: vec2<f32>,
    size: vec2<f32>,
    // the core's width (lines) or diameter (points), the rim's width each side (target pixels)
    width: f32,
    halo: f32,
    halo_color: vec4<f32>,
    feather: f32,
    alpha: f32,
    round: u32,
    // the draw's band of depths: from depth (on its lines) to depth + band (at their rims' outer edge)
    depth: f32,
    band: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

struct Pt {
    x: f32,
    y: f32,
    f: u32,
}

@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var<storage, read> pts: array<Pt>;
@group(1) @binding(1) var<storage, read> colors: array<u32>;
@group(1) @binding(2) var<storage, read> piece_of: array<u32>;
// per piece: its origin's high (xy) and low (zw) parts
@group(1) @binding(3) var<storage, read> origins: array<vec4<f32>>;

struct Out {
    @builtin(position) pos: vec4<f32>,
    @location(0) col: vec4<f32>,
    // along the segment (target pixels from its first point) and across it (from its middle), or a point's place
    // in its square
    @location(1) d: vec2<f32>,
    // the segment's length (target pixels)
    @location(2) @interpolate(flat) len: f32,
}

fn clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / u.size.x * 2.0 - 1.0, 1.0 - p.y / u.size.y * 2.0, 0.0, 1.0);
}

// a point in target pixels
fn place(p: Pt) -> vec2<f32> {
    let o = origins[piece_of[p.f]];
    let rel = (o.xy - u.c_hi) + (o.zw - u.c_lo) + vec2<f32>(p.x, p.y);
    return u.t0 + u.a * rel;
}

fn hidden() -> Out {
    var o: Out;
    o.pos = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    o.col = vec4<f32>(0.0);
    o.d = vec2<f32>(0.0);
    o.len = 0.0;
    return o;
}

// the core's colour inside, the rim's outside, feathered at the outer edge
fn shade(col: vec4<f32>, e: f32) -> vec4<f32> {
    let core = u.width * 0.5;
    let outer = core + u.halo;
    let k = clamp((outer + u.feather - e) / max(u.feather, 1e-3), 0.0, 1.0);
    var c = col;
    if u.halo > 0.0 {
        let m = clamp((e - core) / max(u.feather, 1e-3) + 0.5, 0.0, 1.0);
        c = mix(col, u.halo_color, m);
    }
    return vec4<f32>(c.rgb, c.a * k * u.alpha);
}

@vertex
fn vs_line(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> Out {
    let p0 = pts[ii];
    let p1 = pts[ii + 1u];
    if p0.x != p0.x || p1.x != p1.x {
        return hidden();
    }
    let a = place(p0);
    let b = place(p1);
    let v = b - a;
    let len = length(v);
    var dir = vec2<f32>(1.0, 0.0);
    if len > 1e-6 {
        dir = v / len;
    }
    let n = vec2<f32>(-dir.y, dir.x);
    let hw = u.width * 0.5 + u.halo + u.feather;
    var corner = array<vec2<f32>, 6>(
        vec2<f32>(0.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let c = corner[vi];
    // the ends reach as far beyond the points as the sides: the capsule's round caps (the joints) inside
    let ext = (c.x * 2.0 - 1.0) * hw;
    var o: Out;
    o.pos = clip(mix(a, b, c.x) + dir * ext + n * c.y * hw);
    o.col = unpack4x8unorm(colors[p0.f]);
    o.d = vec2<f32>(c.x * len + ext, c.y * hw);
    o.len = len;
    return o;
}

struct LineOut {
    @location(0) col: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

// both passes: the pixel's distance from the segment as its depth, its colour (the first pass writes no colour)
@fragment
fn fs_line(i: Out) -> LineOut {
    let along = max(max(-i.d.x, i.d.x - i.len), 0.0);
    let e = length(vec2<f32>(along, i.d.y));
    let hw = u.width * 0.5 + u.halo + u.feather;
    if e >= hw {
        discard;
    }
    var o: LineOut;
    o.col = shade(i.col, e);
    // below the band's top: the earlier draw's lines, whose band starts there, are not covered by a tie
    o.depth = u.depth + u.band * 0.999 * (e / hw);
    return o;
}

@vertex
fn vs_fill(@builtin(vertex_index) vi: u32) -> Out {
    let p = pts[vi];
    var o: Out;
    o.pos = clip(place(p));
    o.col = unpack4x8unorm(colors[p.f]);
    o.d = vec2<f32>(0.0);
    o.len = 0.0;
    return o;
}

@fragment
fn fs_fill(i: Out) -> @location(0) vec4<f32> {
    return vec4<f32>(i.col.rgb, i.col.a * u.alpha);
}

@vertex
fn vs_point(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> Out {
    let p = pts[ii];
    if p.x != p.x {
        return hidden();
    }
    var corner = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    let c = corner[vi];
    let r = u.width * 0.5 + u.halo + u.feather;
    var o: Out;
    o.pos = clip(place(p) + c * r);
    o.col = unpack4x8unorm(colors[p.f]);
    o.d = c * r;
    o.len = 0.0;
    return o;
}

@fragment
fn fs_point(i: Out) -> @location(0) vec4<f32> {
    if u.round == 1u {
        return shade(i.col, length(i.d));
    }
    return shade(i.col, max(abs(i.d.x), abs(i.d.y)));
}
