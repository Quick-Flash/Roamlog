// GPU map: base map fills and lines plus trip lines. Every draw carries an offset (metres,
// from camera centre to its geometry's origin, computed in f64 on CPU) so 32-bit vertex
// positions stay small and precise anywhere on Earth.

struct View {
    // CSS px per metre.
    scale: f32,
    // Device px per CSS px, for one-device-pixel edge smoothing.
    ratio: f32,
    // Output size, CSS px.
    size: vec2<f32>,
};
@group(0) @binding(0) var<uniform> view: View;

struct Draw {
    offset: vec2<f32>,
    // Line width, CSS px.
    width: f32,
    // 1 for dashed lines.
    dashed: f32,
    // Straight (not premultiplied) colour.
    color: vec4<f32>,
    // Cover rectangle relative to offset, metres: min x, min y, max x, max y.
    rect: vec4<f32>,
};
@group(1) @binding(0) var<uniform> draw: Draw;

fn screen(m: vec2<f32>) -> vec2<f32> {
    let d = (m + draw.offset) * view.scale;
    return vec2<f32>(d.x + view.size.x * 0.5, view.size.y * 0.5 - d.y);
}

fn clip(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p.x / view.size.x * 2.0 - 1.0, 1.0 - p.y / view.size.y * 2.0, 0.0, 1.0);
}

fn premultiplied(alpha: f32) -> vec4<f32> {
    let a = draw.color.a * alpha;
    return vec4<f32>(draw.color.rgb * a, a);
}

// Fills: triangle fans counted into stencil (non-zero winding), then cover over their bounds.
@vertex
fn vs_fill(@location(0) p: vec2<f32>) -> @builtin(position) vec4<f32> {
    return clip(screen(p));
}

@vertex
fn vs_cover(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let corner = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    )[i];
    return clip(screen(mix(draw.rect.xy, draw.rect.zw, corner)));
}

@fragment
fn fs_fill() -> @location(0) vec4<f32> {
    return premultiplied(1.0);
}

// Lines: one instance per segment, drawn as antialiased capsule of constant screen width.
struct Line {
    @builtin(position) clip: vec4<f32>,
    @location(0) p: vec2<f32>,
    @location(1) @interpolate(flat) a: vec2<f32>,
    @location(2) @interpolate(flat) b: vec2<f32>,
};

@vertex
fn vs_line(@builtin(vertex_index) i: u32, @location(0) sa: vec2<f32>, @location(1) sb: vec2<f32>) -> Line {
    let a = screen(sa);
    let b = screen(sb);
    let span = b - a;
    let len = length(span);
    var dir = vec2<f32>(1.0, 0.0);
    if (len > 1e-4) {
        dir = span / len;
    }
    let normal = vec2<f32>(-dir.y, dir.x);
    // Half width plus one device pixel for smoothing.
    let r = draw.width * 0.5 + 1.0 / view.ratio;
    // Two triangles: corners (end, side) in order 0,1,2 / 2,1,3.
    let corner = array<vec2<f32>, 6>(
        vec2<f32>(0.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    )[i];
    var p = a - dir * r;
    if (corner.x > 0.5) {
        p = b + dir * r;
    }
    p = p + normal * corner.y * r;
    var out: Line;
    out.clip = clip(p);
    out.p = p;
    out.a = a;
    out.b = b;
    return out;
}

@fragment
fn fs_line(in: Line) -> @location(0) vec4<f32> {
    let span = in.b - in.a;
    let len2 = dot(span, span);
    var t = 0.0;
    if (len2 > 1e-8) {
        t = clamp(dot(in.p - in.a, span) / len2, 0.0, 1.0);
    }
    let d = length(in.p - (in.a + span * t));
    var alpha = clamp((draw.width * 0.5 - d) * view.ratio + 0.5, 0.0, 1.0);
    // Dashes: 5 px on, 4 px off along segment.
    if (draw.dashed > 0.5 && fract(t * sqrt(len2) / 9.0) > 5.0 / 9.0) {
        alpha = 0.0;
    }
    if (alpha <= 0.0) {
        discard;
    }
    return premultiplied(alpha);
}

// Labels: one instance per glyph quad, positioned in CSS px around label anchor and
// rotated with it. Halo pass (draw.dashed 0) samples green, text pass red.
@group(2) @binding(0) var atlas: texture_2d<f32>;
@group(2) @binding(1) var atlas_sampler: sampler;

struct Text {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) color: vec4<f32>,
};

@vertex
fn vs_text(
    @builtin(vertex_index) i: u32,
    @location(0) anchor: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) size: vec2<f32>,
    @location(3) rot: vec2<f32>,
    @location(4) uv: vec4<f32>,
    @location(5) text: vec4<f32>,
    @location(6) halo: vec4<f32>,
) -> Text {
    let corner = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    )[i];
    let l = local + corner * size;
    let p = anchor + vec2<f32>(rot.x * l.x - rot.y * l.y, rot.y * l.x + rot.x * l.y);
    var out: Text;
    out.clip = clip(p);
    out.uv = mix(uv.xy, uv.zw, corner);
    out.color = select(halo, text, draw.dashed > 0.5);
    return out;
}

@fragment
fn fs_text(in: Text) -> @location(0) vec4<f32> {
    let s = textureSample(atlas, atlas_sampler, in.uv);
    let a = select(s.g, s.r, draw.dashed > 0.5) * in.color.a;
    if (a <= 0.0) {
        discard;
    }
    return vec4<f32>(in.color.rgb * a, a);
}

// Satellite imagery: one opaque texture per tile over draw.rect (metres from offset; rect
// min y is the tile's south edge, image row 0 its north edge).
struct Image {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_image(@builtin(vertex_index) i: u32) -> Image {
    let corner = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    )[i];
    var out: Image;
    out.clip = clip(screen(mix(draw.rect.xy, draw.rect.zw, corner)));
    out.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return out;
}

@fragment
fn fs_image(in: Image) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(atlas, atlas_sampler, in.uv).rgb, 1.0);
}
