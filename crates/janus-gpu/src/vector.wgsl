// Vector layers on the map: lines (each segment a quad of a width in target pixels, its ends extended to close the
// joints, its edges feathered, a dark rim around its core), polygons' fills (triangles) and points (squares or
// discs). A level of a layer's points is one storage buffer (x, y relative to the origin of the feature's piece, in
// the map's source pixels; the feature's index); a NaN x breaks a line. The pieces' origins are split in two floats
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
    _pad: u32,
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
    // across the line (target pixels from its middle), or a point's place in its square
    @location(1) d: vec2<f32>,
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
    // the ends reach the core's half width beyond the points: the joints closed
    let along = mix(a, b, c.x) + dir * (c.x * 2.0 - 1.0) * (u.width * 0.5);
    var o: Out;
    o.pos = clip(along + n * c.y * hw);
    o.col = unpack4x8unorm(colors[p0.f]);
    o.d = vec2<f32>(c.y * hw, 0.0);
    return o;
}

@fragment
fn fs_line(i: Out) -> @location(0) vec4<f32> {
    return shade(i.col, abs(i.d.x));
}

@vertex
fn vs_fill(@builtin(vertex_index) vi: u32) -> Out {
    let p = pts[vi];
    var o: Out;
    o.pos = clip(place(p));
    o.col = unpack4x8unorm(colors[p.f]);
    o.d = vec2<f32>(0.0);
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
    return o;
}

@fragment
fn fs_point(i: Out) -> @location(0) vec4<f32> {
    if u.round == 1u {
        return shade(i.col, length(i.d));
    }
    return shade(i.col, max(abs(i.d.x), abs(i.d.y)));
}
