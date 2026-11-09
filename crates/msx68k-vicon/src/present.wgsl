// X68000 VICON Fullscreen Surface Presentation Shader (WGSL)
//
// Reads the composited `output_rgba` buffer produced by the VICON compute shader
// and draws it directly onto a window surface texture view with aspect-ratio-correct
// viewport scaling.

struct ViconUniforms {
    width: u32,
    height: u32,
    text_scroll_x: u32,
    text_scroll_y: u32,
    gr0_scroll_x: u32,
    gr0_scroll_y: u32,
    r0_mode: u32,
    r1_priority: u32,
    r2_display: u32,
    contrast: u32,
    tvram_row_bytes: u32,
    tvram_plane_bytes: u32,
};

@group(0) @binding(0)
var<uniform> params: ViconUniforms;

@group(0) @binding(1)
var<storage, read> present_rgba: array<u32>;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VertexOutput {
    let x = f32((vid << 1u) & 2u);
    let y = f32(vid & 2u);
    var out: VertexOutput;
    out.clip_pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let w = max(params.width, 1u);
    let h = max(params.height, 1u);
    let px = min(u32(clamp(in.uv.x, 0.0, 0.999999) * f32(w)), w - 1u);
    let py = min(u32(clamp(in.uv.y, 0.0, 0.999999) * f32(h)), h - 1u);
    let packed = present_rgba[py * w + px];

    let r = f32(packed & 0xFFu) / 255.0;
    let g = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b = f32((packed >> 16u) & 0xFFu) / 255.0;
    return vec4<f32>(r, g, b, 1.0);
}
