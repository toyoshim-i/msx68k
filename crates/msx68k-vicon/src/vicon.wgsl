// X68000 VICON compositing shared by the compositor entry points (WGSL)
//
// Composites 4-plane TVRAM ($E00000-$E7FFFF), GVRAM ($C00000-$DFFFFF), and
// 16-bit GGGGGRRRRRBBBBBI Text/Graphics Palettes ($E82000-$E823FF) into one pixel.
// Each entry point file appends its output binding (4) and its @compute main.

struct ViconUniforms {
    width: u32,
    height: u32,
    text_scroll_x: u32,
    text_scroll_y: u32,
    gr_scroll_x: vec4<u32>,
    gr_scroll_y: vec4<u32>,
    r0_mode: u32,
    r1_priority: u32,
    r2_display: u32,
    contrast: u32,
    tvram_row_bytes: u32,
    tvram_plane_bytes: u32,
    reserved: vec2<u32>,
};

@group(0) @binding(0)
var<uniform> params: ViconUniforms;

// TVRAM: 512 KB = 131,072 u32 words (4 planes of 32,768 u32 words each)
@group(0) @binding(1)
var<storage, read> tvram_words: array<u32>;

// GVRAM: 512 KB = 131,072 u32 words
@group(0) @binding(2)
var<storage, read> gvram_words: array<u32>;

// Palettes: 256 Text palette entries (0..255) + 256 Graphic palette entries (256..511), stored as u32 (low 16 bits = GRBI)
@group(0) @binding(3)
var<storage, read> palettes: array<u32>;

fn read_tvram_byte(byte_addr: u32) -> u32 {
    let word = tvram_words[byte_addr >> 2u];
    let shift = (byte_addr & 3u) * 8u;
    return (word >> shift) & 0xFFu;
}

fn read_gvram_u16_be(byte_addr: u32) -> u32 {
    let word = gvram_words[byte_addr >> 2u];
    let shift = (byte_addr & 2u) * 8u;
    let pair = (word >> shift) & 0xFFFFu;
    // Swap big-endian bytes to 16-bit value
    return ((pair & 0xFFu) << 8u) | ((pair >> 8u) & 0xFFu);
}

fn grbi_to_packed_rgba8(grbi: u32, contrast: u32) -> u32 {
    let i_bit = grbi & 1u;
    let g6 = (((grbi >> 11u) & 0x1Fu) << 1u) | i_bit;
    let r6 = (((grbi >> 6u) & 0x1Fu) << 1u) | i_bit;
    let b6 = (((grbi >> 1u) & 0x1Fu) << 1u) | i_bit;

    let r8 = (((r6 << 2u) | (r6 >> 4u)) * contrast) / 15u;
    let g8 = (((g6 << 2u) | (g6 >> 4u)) * contrast) / 15u;
    let b8 = (((b6 << 2u) | (b6 >> 4u)) * contrast) / 15u;

    return (r8 & 0xFFu) | ((g8 & 0xFFu) << 8u) | ((b8 & 0xFFu) << 16u) | 0xFF000000u;
}

// A row maps to a packed uniform record followed by its 512 palette words.
fn raster_offset(y: u32) -> u32 {
    if (params.reserved.x == 0u) { return 0u; }
    return palettes[512u + min(y, params.reserved.x - 1u)];
}

fn raster_params(offset: u32) -> ViconUniforms {
    var state = params;
    if (offset == 0u) { return state; }
    state.text_scroll_x = palettes[offset + 2u];
    state.text_scroll_y = palettes[offset + 3u];
    for (var p = 0u; p < 4u; p++) {
        state.gr_scroll_x[p] = palettes[offset + 4u + p];
        state.gr_scroll_y[p] = palettes[offset + 8u + p];
    }
    state.r0_mode = palettes[offset + 12u];
    state.r1_priority = palettes[offset + 13u];
    state.r2_display = palettes[offset + 14u];
    state.contrast = palettes[offset + 15u];
    return state;
}

fn raster_palette_base(offset: u32) -> u32 {
    return select(offset + 20u, 0u, offset == 0u);
}

fn sample_tvram_index(x: u32, y: u32, state: ViconUniforms) -> u32 {
    let row_bytes = state.tvram_row_bytes;
    let plane_bytes = state.tvram_plane_bytes;
    var tx = x;
    var ty = y;
    if (row_bytes == 128u && plane_bytes == 0x20000u) {
        tx = (x + state.text_scroll_x) & 1023u;
        ty = (y + state.text_scroll_y) & 1023u;
    }
    let in_plane_byte = (ty * row_bytes) + (tx >> 3u);
    let bit_pos = 7u - (tx & 7u);

    let b0 = (read_tvram_byte(0u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;
    let b1 = (read_tvram_byte(1u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;
    let b2 = (read_tvram_byte(2u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;
    let b3 = (read_tvram_byte(3u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;

    return b0 | (b1 << 1u) | (b2 << 2u) | (b3 << 3u);
}

fn graphics_nibble(slot: u32, x: u32, y: u32, scroll: bool, state: ViconUniforms) -> u32 {
    let page = (state.r1_priority >> (slot * 2u)) & 3u;
    var gx = x;
    var gy = y;
    if (scroll) {
        gx += state.gr_scroll_x[page];
        gy += state.gr_scroll_y[page];
    }
    let word = read_gvram_u16_be((((gy & 511u) * 512u) + (gx & 511u)) * 2u);
    return (word >> (page * 4u)) & 15u;
}

fn graphics_color(x: u32, y: u32, state: ViconUniforms, palette_base: u32) -> u32 {
    let mode = state.r0_mode & 7u;
    var index = 0u;
    if (mode < 4u && (state.r2_display & 15u) == 0u) { return 0u; }
    if (mode >= 4u) {
        if ((state.r2_display & 16u) == 0u) { return 0u; }
        let gx = (x + state.gr_scroll_x[0]) & 1023u;
        let gy = (y + state.gr_scroll_y[0]) & 1023u;
        index = graphics_nibble((gy >> 9u) * 2u + (gx >> 9u), gx, gy, false, state);
    } else {
        var lanes = 4u;
        if (mode == 0u) { lanes = 1u; }
        if (mode == 1u) { lanes = 2u; }
        for (var first = 0u; first < 4u; first += lanes) {
            if ((state.r2_display & (((1u << lanes) - 1u) << first)) == 0u) { continue; }
            var candidate = 0u;
            for (var lane = 0u; lane < lanes; lane++) {
                candidate |= graphics_nibble(first + lane, x, y, true, state) << (lane * 4u);
            }
            if (candidate != 0u) { index = candidate; break; }
        }
    }
    if (mode == 2u || mode == 3u) {
        let low = index & 255u;
        let high = index >> 8u;
        let lo_word = palettes[palette_base + 256u + (low & 254u)];
        let hi_word = palettes[palette_base + 257u + (high & 254u)];
        let lo = select(lo_word & 255u, lo_word >> 8u, (low & 1u) == 0u);
        let hi = select((hi_word << 8u) & 0xff00u, hi_word & 0xff00u, (high & 1u) == 0u);
        return hi | lo;
    }
    return palettes[palette_base + 256u + index];
}

// Whether the text plane is clear (palette index 0, SX-Window's G_THRU) at (x, y), where
// what is under the text shows: VM screens are drawn there, under the text plane.
fn text_is_clear(x: u32, y: u32, state: ViconUniforms) -> bool {
    if ((state.r2_display & 0x0020u) == 0u) {
        return true;
    }
    return sample_tvram_index(x, y, state) == 0u;
}

// BG buffers retain palette codes; the current row's palette converts them here.
fn background_color(x: u32, y: u32, state: ViconUniforms, palette_base: u32) -> u32 {
    if (params.reserved.y == 0u || x >= 512u || ((state.r1_priority >> 8u) & 3u) == 3u || (state.r2_display & 64u) == 0u) { return 0u; }
    let pixel = y * 512u + x;
    let code = (palettes[params.reserved.y + pixel / 4u] >> ((pixel & 3u) * 8u)) & 255u;
    if (code == 0u) { return 0u; }
    return palettes[palette_base + code];
}

// Packed RGBA8 of the pixel at (x, y).
fn composite(x: u32, y: u32, state: ViconUniforms, palette_base: u32) -> u32 {
    let gr_rank = (state.r1_priority >> 8u) & 3u;
    let tx_rank = (state.r1_priority >> 10u) & 3u;
    let sp_rank = (state.r1_priority >> 12u) & 3u;
    var text_color = 0u;
    var bg_color = 0u;
    if (gr_rank != 3u) {
        if ((state.r2_display & 32u) != 0u) {
            text_color = palettes[palette_base + sample_tvram_index(x, y, state)];
        }
        bg_color = background_color(x, y, state, palette_base);
    }
    let gr_color = graphics_color(x, y, state, palette_base);
    let front = select(bg_color, text_color, tx_rank <= sp_rank);
    let back = select(text_color, bg_color, tx_rank <= sp_rank);
    var colors = vec3<u32>(gr_color, front, back);
    if (gr_rank == 1u) { colors = vec3<u32>(front, gr_color, back); }
    if (gr_rank == 2u) { colors = vec3<u32>(front, back, gr_color); }
    var color = colors.x;
    if (color == 0u) { color = colors.y; }
    if (color == 0u) { color = colors.z; }
    return grbi_to_packed_rgba8(color, state.contrast);
}
