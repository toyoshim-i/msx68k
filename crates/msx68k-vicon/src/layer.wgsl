// VM Screen Layer Shader (WGSL)
//
// Draws a VM's completed screen texture as a quad over the primary VM's screen, under its text
// plane: only where the primary's text is clear (alpha 0 in its composited buffer), so that
// menus, windows, and the cursor drawn in text stay in front. The scissor rectangles of the
// draw clip it to the visible region of the VM's window.

struct Layer {
    // Clip-space left, top, right, bottom of the quad.
    dest: vec4<f32>,
    // Size of the frame in the texture.
    size: vec2<u32>,
    // Primary screen position of the frame's top-left pixel.
    origin: vec2<i32>,
    // Size of the primary VM's screen.
    primary: vec2<u32>,
    // Destination size in primary screen pixels.
    dest_size: vec2<u32>,
};

@group(0) @binding(0)
var<uniform> layer: Layer;

@group(0) @binding(1)
var frame: texture_2d<f32>;

// The primary VM's composited screen, packed RGBA8 words (shader.wgsl).
@group(0) @binding(2)
var<storage, read> primary_rgba: array<u32>;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    // Two triangles: (0,0) (1,0) (0,1) and (0,1) (1,0) (1,1).
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vid];
    var out: VertexOutput;
    out.clip_pos = vec4<f32>(mix(layer.dest.xy, layer.dest.zw, corner), 0.0, 1.0);
    out.uv = corner;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let size = max(layer.size, vec2<u32>(1u, 1u));
    let dest_size = max(layer.dest_size, vec2<u32>(1u, 1u));
    let px = min(u32(clamp(in.uv.x, 0.0, 0.999999) * f32(size.x)), size.x - 1u);
    let py = min(u32(clamp(in.uv.y, 0.0, 0.999999) * f32(size.y)), size.y - 1u);
    let mx = layer.origin.x + i32(min(u32(clamp(in.uv.x, 0.0, 0.999999) * f32(dest_size.x)), dest_size.x - 1u));
    let my = layer.origin.y + i32(min(u32(clamp(in.uv.y, 0.0, 0.999999) * f32(dest_size.y)), dest_size.y - 1u));
    if (mx < 0 || my < 0 || u32(mx) >= layer.primary.x || u32(my) >= layer.primary.y) {
        discard;
    }
    if ((primary_rgba[u32(my) * layer.primary.x + u32(mx)] >> 24u) != 0u) {
        discard;
    }
    return vec4<f32>(textureLoad(frame, vec2<u32>(px, py), 0).rgb, 1.0);
}
