// X68000 VICON GPU Compositor & Surface Presentation Shader (WGSL)
//
// Stage 1 (@compute main):
//   Composites 4-plane TVRAM ($E00000-$E7FFFF), GVRAM ($C00000-$DFFFFF), and
//   16-bit GGGGGRRRRRBBBBBI Text/Graphics Palettes ($E82000-$E823FF) into `output_rgba`.
// Stage 2 (@vertex vs_present / @fragment fs_present):
//   Blits `output_rgba` directly onto the window surface texture on the GPU.

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

// TVRAM: 512 KB = 131,072 u32 words (4 planes of 32,768 u32 words each)
@group(0) @binding(1)
var<storage, read> tvram_words: array<u32>;

// GVRAM: 512 KB = 131,072 u32 words
@group(0) @binding(2)
var<storage, read> gvram_words: array<u32>;

// Palettes: 256 Text palette entries (0..255) + 256 Graphic palette entries (256..511), stored as u32 (low 16 bits = GRBI)
@group(0) @binding(3)
var<storage, read> palettes: array<u32>;

// Output framebuffer: width * height packed RGBA8 words (little-endian u32: R | (G << 8) | (B << 16) | (A << 24))
@group(0) @binding(4)
var<storage, read_write> output_rgba: array<u32>;

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

fn sample_gvram_page0_index(x: u32, y: u32) -> u32 {
    let gx = (x + params.gr0_scroll_x) & 511u;
    let gy = (y + params.gr0_scroll_y) & 511u;
    let byte_addr = ((gy * 512u) + gx) * 2u;
    let word = read_gvram_u16_be(byte_addr);
    let mode = params.r0_mode & 0x07u;
    if (mode >= 4u) {
        // 65536-color direct GRBI mode
        return word;
    } else if ((mode & 1u) == 1u) {
        // 256-color mode
        return word & 0xFFu;
    } else {
        // 16-color mode (page 0 in low nibble)
        return word & 0x0Fu;
    }
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = gid.x;
    let y = gid.y;
    if (x >= params.width || y >= params.height) {
        return;
    }

    let text_enabled = (params.r2_display & 0x0020u) != 0u;
    let gr0_enabled = (params.r2_display & 0x0001u) != 0u;
    let contrast = max(params.contrast, 1u);

    var tx_idx: u32 = 0u;
    if (text_enabled) {
        tx_idx = sample_tvram_index(x, y);
    }

    var gr_idx: u32 = 0u;
    if (gr0_enabled && x < 512u && y < 512u) {
        gr_idx = sample_gvram_page0_index(x, y);
    }

    // Priority between Text (bits 9..8 of R1) and Graphics (bits 11..10 of R1):
    // Smaller rank number (0..2) is closer to the viewer (higher priority).
    let tx_rank = (params.r1_priority >> 8u) & 3u;
    let gr_rank = (params.r1_priority >> 10u) & 3u;
    let text_above_gr = tx_rank <= gr_rank;

    var final_grbi: u32 = palettes[0u]; // Backdrop color (Text palette #0)

    if (text_above_gr) {
        if (text_enabled && tx_idx != 0u) {
            final_grbi = palettes[tx_idx];
        } else if (gr0_enabled && gr_idx != 0u) {
            if ((params.r0_mode & 0x07u) >= 4u) {
                final_grbi = gr_idx;
            } else {
                final_grbi = palettes[256u + (gr_idx & 0xFFu)];
            }
        }
    } else {
        if (gr0_enabled && gr_idx != 0u) {
            if ((params.r0_mode & 0x07u) >= 4u) {
                final_grbi = gr_idx;
            } else {
                final_grbi = palettes[256u + (gr_idx & 0xFFu)];
            }
        } else if (text_enabled && tx_idx != 0u) {
            final_grbi = palettes[tx_idx];
        }
    }

    let out_idx = y * params.width + x;
    output_rgba[out_idx] = grbi_to_packed_rgba8(final_grbi, contrast);
}
