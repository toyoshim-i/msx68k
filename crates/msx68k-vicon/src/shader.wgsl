// X68000 VICON GPU Compositor into a storage buffer (WGSL), appended to vicon.wgsl
//
// Stage 1 (@compute main):
//   Composites the screen into `output_rgba`.
// Stage 2 (@vertex vs_present / @fragment fs_present, present.wgsl):
//   Blits `output_rgba` directly onto the window surface texture on the GPU.

// Output framebuffer: width * height packed RGBA8 words (little-endian u32: R | (G << 8) | (B << 16) | (A << 24)).
// Alpha is 0 where the text plane is clear, so that VM layers show there, and 255 elsewhere.
@group(0) @binding(4)
var<storage, read_write> output_rgba: array<u32>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    var rgba = composite(x, y);
    if (text_is_clear(x, y)) {
        rgba = rgba & 0x00FFFFFFu;
    }
    output_rgba[y * params.width + x] = rgba;
}
