// X68000 VICON & CYNTHIA single-pass GPU compositing shared by Compute entry points (WGSL).
//
// Composites 4-plane TVRAM ($E00000-$E7FFFF), GVRAM ($C00000-$DFFFFF),
// 16-bit GGGGGRRRRRBBBBBI Text/Graphics Palettes ($E82000-$E823FF), and
// CYNTHIA coalesced sprite spans + BG0/BG1 + 128B PCG/map block versions.
// Each entry point file appends its output binding (4), workgroup span cache, and @compute main.

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

struct ResolvedState {
    u: ViconUniforms,
    palette_id: u32,
};

@group(0) @binding(0)
var<uniform> params: ViconUniforms;

@group(0) @binding(1)
var<storage, read> tvram_words: array<u32>;

@group(0) @binding(2)
var<storage, read> gvram_words: array<u32>;

@group(0) @binding(3)
var<storage, read> raster_words: array<u32>;

@group(0) @binding(5)
var<storage, read> cynthia_meta: array<u32>;

@group(0) @binding(6)
var<storage, read> cynthia_pcg: array<u32>;

var<workgroup> wg_spans: array<vec4<u32>, 64>;

fn read_tvram_byte(byte_addr: u32) -> u32 {
    let word = tvram_words[byte_addr >> 2u];
    let shift = (byte_addr & 3u) * 8u;
    return (word >> shift) & 0xFFu;
}

fn read_gvram_u16_be(byte_addr: u32) -> u32 {
    let word = gvram_words[byte_addr >> 2u];
    let shift = (byte_addr & 2u) * 8u;
    let pair = (word >> shift) & 0xFFFFu;
    return ((pair & 0xFFu) << 8u) | ((pair >> 8u) & 0xFFu);
}

fn read_pcg_byte(phys_blk: u32, in_blk: u32) -> u32 {
    let w = cynthia_pcg[phys_blk * 32u + (in_blk >> 2u)];
    return (w >> ((in_blk & 3u) * 8u)) & 0xFFu;
}

fn read_pcg_u16_be(phys_blk: u32, in_blk: u32) -> u32 {
    let w = cynthia_pcg[phys_blk * 32u + (in_blk >> 2u)];
    let pair = (w >> ((in_blk & 2u) * 8u)) & 0xFFFFu;
    return ((pair & 0xFFu) << 8u) | (pair >> 8u);
}

fn read_palette_u16(palette_id: u32, index: u32) -> u32 {
    if (palette_id == 0xFFFFFFFFu) {
        return 0u;
    }
    let palette_base = raster_words[2u];
    let word = raster_words[palette_base + palette_id * 256u + (index >> 1u)];
    return (word >> ((index & 1u) * 16u)) & 0xFFFFu;
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

fn grbi_blend_to_packed_rgba8(fg: u32, bg: u32, contrast: u32) -> u32 {
    let i_fg = fg & 1u;
    let i_bg = bg & 1u;
    let g6 = (((((fg >> 11u) & 0x1Fu) << 1u) | i_fg) + ((((bg >> 11u) & 0x1Fu) << 1u) | i_bg)) >> 1u;
    let r6 = (((((fg >> 6u) & 0x1Fu) << 1u) | i_fg) + ((((bg >> 6u) & 0x1Fu) << 1u) | i_bg)) >> 1u;
    let b6 = (((((fg >> 1u) & 0x1Fu) << 1u) | i_fg) + ((((bg >> 1u) & 0x1Fu) << 1u) | i_bg)) >> 1u;

    let r8 = (((r6 << 2u) | (r6 >> 4u)) * contrast) / 15u;
    let g8 = (((g6 << 2u) | (g6 >> 4u)) * contrast) / 15u;
    let b8 = (((b6 << 2u) | (b6 >> 4u)) * contrast) / 15u;

    return (r8 & 0xFFu) | ((g8 & 0xFFu) << 8u) | ((b8 & 0xFFu) << 16u) | 0xFF000000u;
}

fn resolve_raster_state(y: u32) -> ResolvedState {
    if (params.reserved.x == 0u) {
        return ResolvedState(params, 0u);
    }
    let row_map_len = raster_words[0u];
    let state_offset = raster_words[1u];
    let state_idx = raster_words[4u + min(y, row_map_len - 1u)];
    let base = state_offset + state_idx * 16u;
    var u = params;
    u.text_scroll_x = raster_words[base + 0u];
    u.text_scroll_y = raster_words[base + 1u];
    u.gr_scroll_x = vec4<u32>(
        raster_words[base + 2u],
        raster_words[base + 3u],
        raster_words[base + 4u],
        raster_words[base + 5u],
    );
    u.gr_scroll_y = vec4<u32>(
        raster_words[base + 6u],
        raster_words[base + 7u],
        raster_words[base + 8u],
        raster_words[base + 9u],
    );
    u.r0_mode = raster_words[base + 10u];
    u.r1_priority = raster_words[base + 11u];
    u.r2_display = raster_words[base + 12u];
    u.contrast = raster_words[base + 13u];
    return ResolvedState(u, raster_words[base + 14u]);
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

fn graphics_colors(x: u32, y: u32, state: ViconUniforms, palette_id: u32, want_sub: bool) -> vec2<u32> {
    if (palette_id == 0xFFFFFFFFu) {
        return vec2<u32>(0u, 0u);
    }
    let mode = state.r0_mode & 7u;
    var index = 0u;
    var sub_index = 0u;
    if (mode < 4u && (state.r2_display & 15u) == 0u) {
        return vec2<u32>(0u, 0u);
    }
    if (mode >= 4u) {
        if ((state.r2_display & 16u) == 0u) {
            return vec2<u32>(0u, 0u);
        }
        let gx = (x + state.gr_scroll_x[0]) & 1023u;
        let gy = (y + state.gr_scroll_y[0]) & 1023u;
        index = graphics_nibble((gy >> 9u) * 2u + (gx >> 9u), gx, gy, false, state);
    } else {
        var lanes = 4u;
        if (mode == 0u) { lanes = 1u; }
        if (mode == 1u) { lanes = 2u; }
        for (var first = 0u; first < 4u; first += lanes) {
            if ((state.r2_display & (((1u << lanes) - 1u) << first)) == 0u) {
                continue;
            }
            var candidate = 0u;
            for (var lane = 0u; lane < lanes; lane++) {
                candidate |= graphics_nibble(first + lane, x, y, true, state) << (lane * 4u);
            }
            if (candidate != 0u) {
                if (index == 0u) {
                    index = candidate;
                    if (!want_sub || lanes == 4u) {
                        break;
                    }
                } else {
                    sub_index = candidate;
                    break;
                }
            }
        }
    }
    if (mode == 2u || mode == 3u) {
        let low = index & 255u;
        let high = index >> 8u;
        let lo_word = read_palette_u16(palette_id, 256u + (low & 254u));
        let hi_word = read_palette_u16(palette_id, 257u + (high & 254u));
        let lo = select(lo_word & 255u, lo_word >> 8u, (low & 1u) == 0u);
        let hi = select((hi_word << 8u) & 0xff00u, hi_word & 0xff00u, (high & 1u) == 0u);
        return vec2<u32>(hi | lo, 0u);
    }
    let top = read_palette_u16(palette_id, 256u + index);
    var sub = 0u;
    if (sub_index != 0u) {
        sub = read_palette_u16(palette_id, 256u + sub_index);
    }
    return vec2<u32>(top, sub);
}

fn sample_span_code(span: vec4<u32>, x: u32, y: u32) -> u32 {
    let x0 = (i32(span.x & 0xFFFFu) << 16u) >> 16u;
    let y0 = span.x >> 16u;
    let y1 = span.y & 0xFFFFu;
    let ix = i32(x);
    if (ix < x0 || ix >= x0 + 16 || y < y0 || y >= y1) {
        return 0u;
    }
    let absent = (span.y >> 22u) & 1u;
    var nibble = 15u;
    if (absent == 0u) {
        var px = u32(ix - x0);
        var py = ((span.y >> 16u) & 0xFu) + (y - y0);
        if (((span.y >> 20u) & 1u) != 0u) {
            px = 15u - px;
        }
        if (((span.y >> 21u) & 1u) != 0u) {
            py = 15u - py;
        }
        let off = (px >> 3u) * 64u + py * 4u + ((px & 7u) >> 1u);
        let b = read_pcg_byte(span.w, off);
        nibble = (b >> (4u * (1u - (px & 1u)))) & 15u;
    }
    if (nibble == 0u) {
        return 0u;
    }
    return ((span.y >> 28u) << 4u) | nibble;
}

fn load_tile_span(tile_base: u32, idx: u32, total: u32) -> vec4<u32> {
    if (total <= 64u) {
        return wg_spans[idx];
    }
    let s_idx = cynthia_meta[cynthia_meta[4u] + tile_base + idx];
    let s_off = cynthia_meta[5u] + s_idx * 4u;
    return vec4<u32>(
        cynthia_meta[s_off],
        cynthia_meta[s_off + 1u],
        cynthia_meta[s_off + 2u],
        cynthia_meta[s_off + 3u],
    );
}

fn sample_bg_plane(row: vec4<u32>, plane: u32, x: u32, table_base: u32) -> u32 {
    let large = ((row.x >> 23u) & 1u) != 0u;
    let full_mem = ((row.x >> 25u) & 1u) != 0u;
    let size = select(8u, 16u, large);
    let mask = size * 64u - 1u;
    let word_p = select(row.x, row.y, plane != 0u);
    let scroll_x = word_p & 0x3FFu;
    let scroll_y = (word_p >> 10u) & 0x3FFu;
    let map_sel = (word_p >> 20u) & 1u;
    let bank = (word_p >> 21u) & 1u;
    let origin_x = (i32(row.z & 0xFFFFu) << 16u) >> 16u;
    let source_y = (i32(row.z >> 16u) << 16u) >> 16u;
    let sx = u32(i32(x) - origin_x + i32(scroll_x)) & mask;
    let sy = u32(source_y + i32(scroll_y)) & mask;
    let map_base = select(0x4000u, 0x6000u, map_sel != 0u);
    let at = map_base + ((sy / size) * 64u + sx / size) * 2u;

    var tile = 0xFFFFu;
    if (bank == 0u || full_mem) {
        let logical_blk = select(0u, 256u, bank != 0u) + (at >> 7u);
        let phys_blk = cynthia_meta[cynthia_meta[1u] + table_base + logical_blk];
        tile = read_pcg_u16_be(phys_blk, at & 127u);
    }

    var px = sx & (size - 1u);
    var py = sy & (size - 1u);
    if ((tile & 0x4000u) != 0u) {
        px = size - 1u - px;
    }
    if ((tile & 0x8000u) != 0u) {
        py = size - 1u - py;
    }
    let pattern = tile & 255u;
    let pat_off = select(
        pattern * 32u + py * 4u + (px >> 1u),
        pattern * 128u + (px >> 3u) * 64u + py * 4u + ((px & 7u) >> 1u),
        large,
    );
    let phys_pat_blk = cynthia_meta[cynthia_meta[1u] + table_base + (pat_off >> 7u)];
    let nibble = (read_pcg_byte(phys_pat_blk, pat_off & 127u) >> (4u * (1u - (px & 1u)))) & 15u;
    if (nibble == 0u) {
        return 0u;
    }
    return ((tile >> 4u) & 0xF0u) | nibble;
}

fn load_tile_span_direct(tile_base: u32, idx: u32) -> vec4<u32> {
    let s_idx = cynthia_meta[cynthia_meta[4u] + tile_base + idx];
    let s_off = cynthia_meta[5u] + s_idx * 4u;
    return vec4<u32>(
        cynthia_meta[s_off],
        cynthia_meta[s_off + 1u],
        cynthia_meta[s_off + 2u],
        cynthia_meta[s_off + 3u],
    );
}

fn sample_cynthia_code(
    x: u32,
    y: u32,
    state: ViconUniforms,
    tile_base_in: u32,
    c3_in: u32,
    c2_in: u32,
    c1_in: u32,
) -> u32 {
    if ((params.reserved.y & 1u) == 0u || ((state.r1_priority >> 8u) & 3u) == 3u || (state.r2_display & 64u) == 0u) {
        return 0u;
    }
    let cynthia_h = cynthia_meta[6u];
    if (y >= cynthia_h) {
        return 0u;
    }
    let row_off = cynthia_meta[0u] + y * 4u;
    let row = vec4<u32>(
        cynthia_meta[row_off],
        cynthia_meta[row_off + 1u],
        cynthia_meta[row_off + 2u],
        cynthia_meta[row_off + 3u],
    );
    let gate_bits = (1u << 27u) | (1u << 24u);
    if ((row.x & gate_bits) != gate_bits || ((row.y >> 23u) & 1u) == 0u) {
        return 0u;
    }

    let h_scale = (row.y >> 24u) & 3u;
    var cx = x;
    if (h_scale == 1u) {
        cx = x >> 1u;
    } else if (h_scale == 2u) {
        cx = x << 1u;
    }
    if (cx >= 512u) {
        return 0u;
    }

    var tile_base = tile_base_in;
    var c3 = c3_in;
    var c2 = c2_in;
    var c1 = c1_in;
    let use_wg = h_scale == 0u;
    if (!use_wg) {
        let ctx = cx >> 4u;
        let cty = y >> 4u;
        if (ctx < 32u && cty < cynthia_meta[7u]) {
            let meta_off = cynthia_meta[3u] + (cty * 32u + ctx) * 4u;
            tile_base = cynthia_meta[meta_off];
            c3 = cynthia_meta[meta_off + 1u];
            c2 = cynthia_meta[meta_off + 2u];
            c1 = cynthia_meta[meta_off + 3u];
        } else {
            c3 = 0u;
            c2 = 0u;
            c1 = 0u;
        }
    }

    let total = c3 + c2 + c1;
    for (var k = 0u; k < c3; k++) {
        let span = select(load_tile_span_direct(tile_base, k), load_tile_span(tile_base, k, total), use_wg);
        let code = sample_span_code(span, cx, y);
        if (code != 0u) {
            return code;
        }
    }

    let bg_valid = (row.x & (1u << 26u)) != 0u;
    let table_base = row.w * cynthia_meta[2u];
    if (bg_valid && (row.x & (1u << 22u)) != 0u) {
        let code = sample_bg_plane(row, 0u, cx, table_base);
        if (code != 0u) {
            return code;
        }
    }

    for (var k = c3; k < c3 + c2; k++) {
        let span = select(load_tile_span_direct(tile_base, k), load_tile_span(tile_base, k, total), use_wg);
        let code = sample_span_code(span, cx, y);
        if (code != 0u) {
            return code;
        }
    }

    if (bg_valid && (row.y & (1u << 22u)) != 0u && (row.x & (1u << 23u)) == 0u) {
        let code = sample_bg_plane(row, 1u, cx, table_base);
        if (code != 0u) {
            return code;
        }
    }

    for (var k = c3 + c2; k < total; k++) {
        let span = select(load_tile_span_direct(tile_base, k), load_tile_span(tile_base, k, total), use_wg);
        let code = sample_span_code(span, cx, y);
        if (code != 0u) {
            return code;
        }
    }

    return 0u;
}

struct CompositeResult {
    rgba: u32,
    clear_for_layer: bool,
};

fn apply_interlace_dim(rgba: u32, y: u32) -> u32 {
    if ((params.reserved.y & 2u) == 0u) {
        return rgba;
    }
    let field = (params.reserved.y >> 2u) & 1u;
    if ((y & 1u) == field) {
        return rgba;
    }
    let r = ((rgba & 0xFFu) * 3u) >> 2u;
    let g = (((rgba >> 8u) & 0xFFu) * 3u) >> 2u;
    let b = (((rgba >> 16u) & 0xFFu) * 3u) >> 2u;
    return r | (g << 8u) | (b << 16u) | (rgba & 0xFF000000u);
}

fn composite_pixel(
    x: u32,
    y: u32,
    resolved: ResolvedState,
    tile_base: u32,
    c3: u32,
    c2: u32,
    c1: u32,
) -> CompositeResult {
    let state = resolved.u;
    let palette_id = resolved.palette_id;
    if ((state.r2_display & 0x7Fu) == 0u || palette_id == 0xFFFFFFFFu) {
        return CompositeResult(grbi_to_packed_rgba8(0u, state.contrast), true);
    }

    let gr_rank = (state.r1_priority >> 8u) & 3u;
    let tx_rank = (state.r1_priority >> 10u) & 3u;
    let sp_rank = (state.r1_priority >> 12u) & 3u;

    var tx_idx = 0u;
    var text_on = false;
    var text_color = 0u;
    if ((state.r2_display & 32u) != 0u) {
        tx_idx = sample_tvram_index(x, y, state);
        text_on = true;
    }

    var bg_color = 0u;
    if (gr_rank != 3u) {
        if (text_on) {
            text_color = read_palette_u16(palette_id, tx_idx);
        }
        let bg_code = sample_cynthia_code(x, y, state, tile_base, c3, c2, c1);
        if (bg_code != 0u) {
            bg_color = read_palette_u16(palette_id, bg_code);
        }
    }

    let exon = (state.r2_display & 0x1000u) != 0u;
    let hp = (state.r2_display & 0x0800u) != 0u;
    let gg = (state.r2_display & 0x0200u) != 0u;
    let gr_pair = graphics_colors(x, y, state, palette_id, exon && hp && gg);
    let gr_color = gr_pair.x;
    let gr_sub = gr_pair.y;
    let front = select(bg_color, text_color, tx_rank <= sp_rank);
    let back = select(text_color, bg_color, tx_rank <= sp_rank);

    let text_clear = !text_on || tx_idx == 0u;
    let clear_for_layer = text_clear && gr_color == 0u && bg_color == 0u;

    if (!exon) {
        var colors = vec3<u32>(gr_color, front, back);
        if (gr_rank == 1u) { colors = vec3<u32>(front, gr_color, back); }
        if (gr_rank == 2u) { colors = vec3<u32>(front, back, gr_color); }
        var color = colors.x;
        if (color == 0u) { color = colors.y; }
        if (color == 0u) { color = colors.z; }
        return CompositeResult(apply_interlace_dim(grbi_to_packed_rgba8(color, state.contrast), y), clear_for_layer);
    }

    if (!hp) {
        if (gr_color != 0u && (gr_color & 1u) != 0u) {
            return CompositeResult(apply_interlace_dim(grbi_to_packed_rgba8(gr_color, state.contrast), y), clear_for_layer);
        }
        var colors = vec3<u32>(gr_color, front, back);
        if (gr_rank == 1u) { colors = vec3<u32>(front, gr_color, back); }
        if (gr_rank == 2u) { colors = vec3<u32>(front, back, gr_color); }
        var color = colors.x;
        if (color == 0u) { color = colors.y; }
        if (color == 0u) { color = colors.z; }
        return CompositeResult(apply_interlace_dim(grbi_to_packed_rgba8(color, state.contrast), y), clear_for_layer);
    }

    let bp = (state.r2_display & 0x0400u) != 0u;
    let gt = (state.r2_display & 0x0100u) != 0u;
    var l0 = gr_color; var g0 = true;
    var l1 = front;    var g1 = false;
    var l2 = back;     var g2 = false;
    if (gr_rank == 1u) {
        l0 = front;    g0 = false;
        l1 = gr_color; g1 = true;
        l2 = back;     g2 = false;
    } else if (gr_rank == 2u) {
        l0 = front;    g0 = false;
        l1 = back;     g1 = false;
        l2 = gr_color; g2 = true;
    } else if (gr_rank == 3u) {
        l1 = 0u;
        l2 = 0u;
    }

    var top = 0u;
    var top_is_gr = false;
    var second = 0u;
    if (l0 != 0u) {
        top = l0; top_is_gr = g0;
        second = select(l2, l1, l1 != 0u);
    } else if (l1 != 0u) {
        top = l1; top_is_gr = g1;
        second = l2;
    } else {
        top = l2; top_is_gr = g2;
    }

    if (top == 0u) {
        return CompositeResult(grbi_to_packed_rgba8(0u, state.contrast), clear_for_layer);
    }
    let trigger = bp || ((top & 1u) != 0u);
    if (top_is_gr) {
        if (trigger && gg && gr_sub != 0u) {
            return CompositeResult(apply_interlace_dim(grbi_blend_to_packed_rgba8(top, gr_sub, state.contrast), y), clear_for_layer);
        }
        if (trigger && gt) {
            return CompositeResult(apply_interlace_dim(grbi_blend_to_packed_rgba8(top, second, state.contrast), y), clear_for_layer);
        }
        if (trigger && gg) {
            return CompositeResult(apply_interlace_dim(grbi_blend_to_packed_rgba8(top, 0u, state.contrast), y), clear_for_layer);
        }
        return CompositeResult(apply_interlace_dim(grbi_to_packed_rgba8(top, state.contrast), y), clear_for_layer);
    }
    if (trigger && gt) {
        return CompositeResult(apply_interlace_dim(grbi_blend_to_packed_rgba8(top, second, state.contrast), y), clear_for_layer);
    }
    return CompositeResult(apply_interlace_dim(grbi_to_packed_rgba8(top, state.contrast), y), clear_for_layer);
}
