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

fn sample_tvram_index(x: u32, y: u32) -> u32 {
    let row_bytes = params.tvram_row_bytes;
    let plane_bytes = params.tvram_plane_bytes;
    var tx = x;
    var ty = y;
    if (row_bytes == 128u && plane_bytes == 0x20000u) {
        tx = (x + params.text_scroll_x) & 1023u;
        ty = (y + params.text_scroll_y) & 1023u;
    }
    let in_plane_byte = (ty * row_bytes) + (tx >> 3u);
    let bit_pos = 7u - (tx & 7u);

    let b0 = (read_tvram_byte(0u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;
    let b1 = (read_tvram_byte(1u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;
    let b2 = (read_tvram_byte(2u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;
    let b3 = (read_tvram_byte(3u * plane_bytes + in_plane_byte) >> bit_pos) & 1u;

    return b0 | (b1 << 1u) | (b2 << 2u) | (b3 << 3u);
}

fn graphics_nibble(slot: u32, x: u32, y: u32, scroll: bool) -> u32 {
    let page = (params.r1_priority >> (slot * 2u)) & 3u;
    var gx = x;
    var gy = y;
    if (scroll) {
        gx += params.gr_scroll_x[page];
        gy += params.gr_scroll_y[page];
    }
    let word = read_gvram_u16_be((((gy & 511u) * 512u) + (gx & 511u)) * 2u);
    return (word >> (page * 4u)) & 15u;
}

fn graphics_color(x: u32, y: u32) -> u32 {
    let mode = params.r0_mode & 7u;
    var index = 0u;
    if (mode < 4u && (params.r2_display & 15u) == 0u) { return 0u; }
    if (mode >= 4u) {
        if ((params.r2_display & 16u) == 0u) { return 0u; }
        let gx = (x + params.gr_scroll_x[0]) & 1023u;
        let gy = (y + params.gr_scroll_y[0]) & 1023u;
        index = graphics_nibble((gy >> 9u) * 2u + (gx >> 9u), gx, gy, false);
    } else {
        var lanes = 4u;
        if (mode == 0u) { lanes = 1u; }
        if (mode == 1u) { lanes = 2u; }
        for (var first = 0u; first < 4u; first += lanes) {
            if ((params.r2_display & (((1u << lanes) - 1u) << first)) == 0u) { continue; }
            var candidate = 0u;
            for (var lane = 0u; lane < lanes; lane++) {
                candidate |= graphics_nibble(first + lane, x, y, true) << (lane * 4u);
            }
            if (candidate != 0u) { index = candidate; break; }
        }
    }
    if (mode == 2u || mode == 3u) {
        let low = index & 255u;
        let high = index >> 8u;
        let lo_word = palettes[256u + (low & 254u)];
        let hi_word = palettes[257u + (high & 254u)];
        let lo = select(lo_word & 255u, lo_word >> 8u, (low & 1u) == 0u);
        let hi = select((hi_word << 8u) & 0xff00u, hi_word & 0xff00u, (high & 1u) == 0u);
        return hi | lo;
    }
    return palettes[256u + index];
}

// Whether the text plane is clear (palette index 0, SX-Window's G_THRU) at (x, y), where
// what is under the text shows: VM screens are drawn there, under the text plane.
fn text_is_clear(x: u32, y: u32) -> bool {
    if ((params.r2_display & 0x0020u) == 0u) {
        return true;
    }
    return sample_tvram_index(x, y) == 0u;
}

// Packed RGBA8 of the pixel at (x, y).
fn composite(x: u32, y: u32) -> u32 {
    let gr_rank = (params.r1_priority >> 8u) & 3u;
    let tx_rank = (params.r1_priority >> 10u) & 3u;
    let sp_rank = (params.r1_priority >> 12u) & 3u;
    let text_above_gr = gr_rank == 2u || (gr_rank == 1u && tx_rank <= sp_rank);
    var text_color = 0u;
    if ((params.r2_display & 32u) != 0u && gr_rank != 3u) {
        text_color = palettes[sample_tvram_index(x, y)];
    }
    let gr_color = graphics_color(x, y);
    var color = text_color;
    if (text_above_gr) {
        if (text_color == 0u) { color = gr_color; }
    } else if (gr_color != 0u) {
        color = gr_color;
    }
    return grbi_to_packed_rgba8(color, params.contrast);
}
