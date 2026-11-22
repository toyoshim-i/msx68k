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
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32,
) {
    var tile_base = 0u;
    var c3 = 0u;
    var c2 = 0u;
    var c1 = 0u;
    if ((params.reserved.y & 1u) != 0u) {
        if (wg_id.x < 32u && wg_id.y < cynthia_meta[7u]) {
            let meta_off = cynthia_meta[3u] + (wg_id.y * 32u + wg_id.x) * 4u;
            tile_base = cynthia_meta[meta_off];
            c3 = cynthia_meta[meta_off + 1u];
            c2 = cynthia_meta[meta_off + 2u];
            c1 = cynthia_meta[meta_off + 3u];
            let total = c3 + c2 + c1;
            if (total > 0u && total <= 64u && lidx < total) {
                let s_idx = cynthia_meta[cynthia_meta[4u] + tile_base + lidx];
                let s_off = cynthia_meta[5u] + s_idx * 4u;
                wg_spans[lidx] = vec4<u32>(
                    cynthia_meta[s_off],
                    cynthia_meta[s_off + 1u],
                    cynthia_meta[s_off + 2u],
                    cynthia_meta[s_off + 3u],
                );
            }
        }
        workgroupBarrier();
    }

    let x = gid.x;
    let y = gid.y;
    if (x >= params.width || y >= params.height) {
        return;
    }

    let resolved = resolve_raster_state(y);
    let res = composite_pixel(x, y, resolved, tile_base, c3, c2, c1);
    var rgba = res.rgba;
    if (res.clear_for_layer) {
        rgba = rgba & 0x00FFFFFFu;
    }
    output_rgba[y * params.width + x] = rgba;
}
