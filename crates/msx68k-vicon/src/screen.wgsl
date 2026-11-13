// X68000 VICON GPU Compositor into a VM screen texture (WGSL), appended to vicon.wgsl

@group(0) @binding(4)
var output: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= params.width || y >= params.height) {
        return;
    }
    let offset = raster_offset(y);
    let state = raster_params(offset);
    textureStore(output, vec2<u32>(x, y), unpack4x8unorm(composite(x, y, state, raster_palette_base(offset))));
}
