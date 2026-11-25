//! GLSL ES 1.00 shaders for Pass 1 (CYNTHIA sprite spans + BG0/BG1), Pass 2 (VICON),
//! and Pass 3 (optional GPU downsample blit).
//!
//! All standard X68000 paths are strictly bounded to `mediump`-exact integer ranges (`<= 1535`),
//! avoiding bitwise operations, SFU `exp2` precision hazards, and redundant LUT texture fetches.

pub const SPRITE_VS: &str = r#"
attribute vec2 a_pos;
attribute float a_depth;
attribute vec2 a_pat;
attribute vec4 a_meta;

uniform vec2 u_inv_half_size;

varying vec2 v_pat;
varying vec4 v_meta;

void main() {
    gl_Position = vec4(a_pos.x * u_inv_half_size.x - 1.0, a_pos.y * u_inv_half_size.y - 1.0, a_depth, 1.0);
    v_pat = a_pat;
    v_meta = a_meta;
}
"#;

pub const SPRITE_FS: &str = r#"
precision mediump float;

uniform sampler2D u_pcg;
uniform float u_pcg_inv_h;

varying vec2 v_pat;
varying vec4 v_meta;

void main() {
    float pal_block = v_meta.x;
    float absent = v_meta.y;
    float nibble = 15.0;
    if (absent < 0.5) {
        float px = clamp(floor(v_pat.x), 0.0, 15.0);
        float py = clamp(floor(v_pat.y), 0.0, 15.0);
        float qx = step(8.0, px);
        float px_in_q = px - qx * 8.0;
        float px_byte = floor(px_in_q * 0.5);
        float px_odd = px_in_q - px_byte * 2.0;
        float in_blk = qx * 64.0 + py * 4.0 + px_byte;
        float tex_x = v_meta.z * 128.0 + in_blk;
        float tex_y = v_meta.w;
        float b = texture2D(u_pcg, vec2((tex_x + 0.5) / 1024.0, (tex_y + 0.5) * u_pcg_inv_h)).r;
        float b_int = floor(b * 255.0 + 0.5);
        float hi = floor(b_int * 0.0625);
        nibble = mix(hi, b_int - hi * 16.0, px_odd);
    }
    if (nibble < 0.5) {
        discard;
    }
    gl_FragColor = vec4((pal_block * 16.0 + nibble) / 255.0, 0.0, 0.0, 1.0);
}
"#;

pub const BG_VS: &str = r#"
attribute vec2 a_pos;
uniform float u_depth_ndc;

void main() {
    gl_Position = vec4(a_pos, u_depth_ndc, 1.0);
}
"#;

pub const BG_FS: &str = r#"
precision mediump float;

uniform sampler2D u_pcg;
uniform sampler2D u_pcg_table;
uniform sampler2D u_row_state;

uniform float u_plane;
uniform float u_row_inv_h;
uniform float u_tbl_inv_h;
uniform float u_pcg_inv_h;

vec4 row_texel(float col, float y) {
    return texture2D(u_row_state, vec2((col + 0.5) / 16.0, (y + 0.5) * u_row_inv_h));
}

float pcg_byte(float phys_col, float phys_row, float in_blk) {
    float tex_x = phys_col * 128.0 + in_blk;
    return texture2D(u_pcg, vec2((tex_x + 0.5) / 1024.0, (phys_row + 0.5) * u_pcg_inv_h)).r;
}

void main() {
    float x = floor(gl_FragCoord.x);
    float y = floor(gl_FragCoord.y);

    vec4 cfg = row_texel(12.0 + u_plane, y);
    if (cfg.r < 0.5) {
        discard;
    }

    vec4 sc = row_texel(9.0 + u_plane, y);
    float eff_sx = floor(sc.r * 255.0 + 0.5) * 256.0 + floor(sc.g * 255.0 + 0.5);
    float eff_sy = floor(sc.b * 255.0 + 0.5) * 256.0 + floor(sc.a * 255.0 + 0.5);

    float large = step(0.5, row_texel(14.0, y).r);
    float tile_size = mix(8.0, 16.0, large);
    float map_size = mix(512.0, 1024.0, large);

    float sx = x + eff_sx;
    if (sx >= map_size) {
        sx -= map_size;
    }
    float sy = eff_sy;

    float tile_x = floor(sx / tile_size);
    float tile_y = floor(sy / tile_size);

    vec4 tbl_id_t = row_texel(11.0, y);
    float tbl_id = floor(tbl_id_t.r * 255.0 + 0.5) + floor(tbl_id_t.g * 255.0 + 0.5) * 256.0;
    float tbl_v = (tbl_id + 0.5) * u_tbl_inv_h;

    float tile_hi_norm = 1.0;
    float tile_lo_norm = 1.0;
    if (cfg.b < 0.5) {
        float map_sel = floor(cfg.g * 255.0 + 0.5);
        float bank = floor(cfg.a * 255.0 + 0.5);
        float logical_blk = bank * 256.0 + (128.0 + map_sel * 64.0) + tile_y;
        vec4 map_ref = texture2D(u_pcg_table, vec2((logical_blk + 0.5) / 512.0, tbl_v));
        float mv0 = floor(map_ref.r * 255.0 + 0.5);
        float m_row_hi = floor(mv0 * 0.125 + 0.001);
        float m_col = mv0 - m_row_hi * 8.0;
        float m_row = m_row_hi * 256.0 + floor(map_ref.a * 255.0 + 0.5);
        float in_blk = tile_x * 2.0;
        tile_hi_norm = pcg_byte(m_col, m_row, in_blk);
        tile_lo_norm = pcg_byte(m_col, m_row, in_blk + 1.0);
    }

    float th = floor(tile_hi_norm * 255.0 + 0.5);
    float vflip = step(128.0, th);
    float th_rem = th - vflip * 128.0;
    float hflip = step(64.0, th_rem);
    float pal_block = th - floor(th * 0.0625) * 16.0;
    float pattern = floor(tile_lo_norm * 255.0 + 0.5);

    float px = sx - tile_x * tile_size;
    float py = sy - tile_y * tile_size;
    if (hflip > 0.5) {
        px = tile_size - 1.0 - px;
    }
    if (vflip > 0.5) {
        py = tile_size - 1.0 - py;
    }

    float pat_blk = mix(floor(pattern * 0.25), pattern, large);
    float pat_slot = pattern - floor(pattern * 0.25) * 4.0;
    float qx = step(8.0, px) * large;
    float px_in_q = px - qx * 8.0;
    float px_byte = floor(px_in_q * 0.5);
    float px_odd = px_in_q - px_byte * 2.0;
    float pat_in_blk = mix(pat_slot * 32.0, qx * 64.0, large) + py * 4.0 + px_byte;

    vec4 pat_ref = texture2D(u_pcg_table, vec2((pat_blk + 0.5) / 512.0, tbl_v));
    float pv0 = floor(pat_ref.r * 255.0 + 0.5);
    float p_row_hi = floor(pv0 * 0.125 + 0.001);
    float p_col = pv0 - p_row_hi * 8.0;
    float p_row = p_row_hi * 256.0 + floor(pat_ref.a * 255.0 + 0.5);
    float pb = pcg_byte(p_col, p_row, pat_in_blk);
    float pb_int = floor(pb * 255.0 + 0.5);
    float pb_hi = floor(pb_int * 0.0625);
    float nibble = mix(pb_hi, pb_int - pb_hi * 16.0, px_odd);
    if (nibble < 0.5) {
        discard;
    }
    gl_FragColor = vec4((pal_block * 16.0 + nibble) / 255.0, 0.0, 0.0, 1.0);
}
"#;

pub const VICON_VS: &str = r#"
attribute vec2 a_pos;

void main() {
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

/// Pass 2 text-only specialized uniform VICON shader:
/// - Used when no row uses 65536-color mode, VICON state is uniform, standard TVRAM is active,
///   text is enabled, and both GVRAM (disabled or all-zero) and CYNTHIA are inactive.
/// - Eliminates GVRAM/CYNTHIA/priority branching and skips dependent palette fetches on empty
///   TVRAM bytes (`raw == 0`) and `tx_idx == 0` pixels.
pub const VICON_TEXT_ONLY_FS: &str = r#"
precision mediump float;

uniform sampler2D u_tvram;
uniform sampler2D u_palette;

uniform float u_pal_v;
uniform vec2 u_tx_scroll;
uniform vec2 u_y_flip;
uniform vec4 u_pal0_rgba;

float bit_scale(float bit_pos) {
    float s = 1.0;
    float bp = bit_pos;
    if (bp < 4.0) { s *= 0.0625; } else { bp -= 4.0; }
    if (bp < 2.0) { s *= 0.25; } else { bp -= 2.0; }
    if (bp < 1.0) { s *= 0.5; }
    return s;
}

void main() {
    float tx = floor(gl_FragCoord.x) + u_tx_scroll.x;
    if (tx >= 1024.0) {
        tx -= 1024.0;
    }
    float ty = u_y_flip.x + floor(gl_FragCoord.y) * u_y_flip.y + u_tx_scroll.y;
    if (ty >= 1024.0) {
        ty -= 1024.0;
    }
    float byte_x = floor(tx * 0.125);
    float half_sel = step(512.0, ty);
    float tex_y = (ty - half_sel * 512.0 + 0.5) / 512.0;
    float tex_x = (half_sel * 128.0 + byte_x + 0.5) / 256.0;
    vec4 raw = texture2D(u_tvram, vec2(tex_x, tex_y));
    if (raw.r + raw.g + raw.b + raw.a == 0.0) {
        gl_FragColor = u_pal0_rgba;
        return;
    }
    float bit_pos = tx - byte_x * 8.0;
    vec4 tb = floor(raw * 255.0 + 0.5);
    vec4 shifted = floor(tb * bit_scale(bit_pos));
    vec4 bits = shifted - floor(shifted * 0.5) * 2.0;
    float tx_idx = dot(bits, vec4(1.0, 2.0, 4.0, 8.0));
    if (tx_idx < 0.5) {
        gl_FragColor = u_pal0_rgba;
        return;
    }
    vec4 col = texture2D(u_palette, vec2((tx_idx + 0.5) / 512.0, u_pal_v));
    gl_FragColor = vec4(col.rgb, 1.0);
}
"#;

/// Pass 2 fast uniform VICON shader:
/// - Used when no row uses 65536-color mode, VICON state is uniform across rows, and TVRAM is standard.
/// - Zero `u_row_state` fetches, zero `u_lut` fetches, 1-fetch RGBA TVRAM, pre-baked RGBA palette,
///   pre-unpacked plane enables, disabled GVRAM page skipping, and front-to-back layer priority early-out.
pub const VICON_FAST_UNIFORM_FS: &str = r#"
precision mediump float;

uniform sampler2D u_tvram;
uniform sampler2D u_gvram;
uniform sampler2D u_cynthia;
uniform sampler2D u_palette;

uniform float u_cynthia_inv_h;
uniform float u_pal_v;
uniform float u_r0_mode;
uniform float u_tx_en;
uniform vec4 u_gr_en;       // (en0, en1, en2, en3) or (en_1024, 0, 0, 0) when mode >= 4
uniform vec4 u_pal0_rgba;   // pre-baked text palette[0] RGBA (alpha 0 if GRBI==0)
uniform vec4 u_ranks;       // x: gr_rank, y: tx_rank, z: sp_rank, w: cynthia_active
uniform vec4 u_slots;       // p0, p1, p2, p3
uniform vec2 u_tx_scroll;   // tx_sx, tx_sy
uniform vec4 u_gr_scroll01; // gs0x, gs0y, gs1x, gs1y
uniform vec4 u_gr_scroll23; // gs2x, gs2y, gs3x, gs3y
uniform vec2 u_y_flip;      // (0, 1) normal; (height - 1, -1) y-flipped

float bit_scale(float bit_pos) {
    float s = 1.0;
    float bp = bit_pos;
    if (bp < 4.0) { s *= 0.0625; } else { bp -= 4.0; }
    if (bp < 2.0) { s *= 0.25; } else { bp -= 2.0; }
    if (bp < 1.0) { s *= 0.5; }
    return s;
}

vec4 pal_rgba(float col) {
    return texture2D(u_palette, vec2((col + 0.5) / 512.0, u_pal_v));
}

vec4 sample_text(float x, float y) {
    if (u_tx_en < 0.5) {
        return vec4(0.0);
    }
    float tx = x + u_tx_scroll.x;
    if (tx >= 1024.0) {
        tx -= 1024.0;
    }
    float ty = y + u_tx_scroll.y;
    if (ty >= 1024.0) {
        ty -= 1024.0;
    }
    float byte_x = floor(tx * 0.125);
    float half_sel = step(512.0, ty);
    float tex_y = (ty - half_sel * 512.0 + 0.5) / 512.0;
    float tex_x = (half_sel * 128.0 + byte_x + 0.5) / 256.0;
    vec4 raw = texture2D(u_tvram, vec2(tex_x, tex_y));
    if (raw.r + raw.g + raw.b + raw.a == 0.0) {
        return u_pal0_rgba;
    }
    float bit_pos = tx - byte_x * 8.0;
    vec4 tb = floor(raw * 255.0 + 0.5);
    vec4 shifted = floor(tb * bit_scale(bit_pos));
    vec4 bits = shifted - floor(shifted * 0.5) * 2.0;
    float tx_idx = dot(bits, vec4(1.0, 2.0, 4.0, 8.0));
    if (tx_idx < 0.5) {
        return u_pal0_rgba;
    }
    return pal_rgba(tx_idx);
}

vec4 sample_cynthia(float x, float y) {
    if (u_ranks.w < 0.5 || x >= 512.0) {
        return vec4(0.0);
    }
    float code = floor(texture2D(u_cynthia, vec2((x + 0.5) / 512.0, (y + 0.5) * u_cynthia_inv_h)).r * 255.0 + 0.5);
    if (code < 0.5) {
        return vec4(0.0);
    }
    return pal_rgba(code);
}

vec2 gr_scroll_for(float page) {
    if (page < 0.5) { return u_gr_scroll01.xy; }
    if (page < 1.5) { return u_gr_scroll01.zw; }
    if (page < 2.5) { return u_gr_scroll23.xy; }
    return u_gr_scroll23.zw;
}

float gv_nibble(float page, float x, float y, float do_scroll) {
    float gx = x;
    if (gx >= 512.0) { gx = mod(gx, 512.0); }
    float gy = y;
    if (gy >= 512.0) { gy = mod(gy, 512.0); }
    if (do_scroll > 0.5) {
        vec2 sc = gr_scroll_for(page);
        gx += sc.x;
        if (gx >= 512.0) { gx -= 512.0; }
        gy += sc.y;
        if (gy >= 512.0) { gy -= 512.0; }
    }
    vec4 gtex = texture2D(u_gvram, vec2((gx + 0.5) / 512.0, (gy + 0.5) / 512.0));
    float byte_val = mix(gtex.a, gtex.r, step(1.5, page));
    float b_int = floor(byte_val * 255.0 + 0.5);
    float hi = floor(b_int * 0.0625);
    float lo = b_int - hi * 16.0;
    float take_hi = step(0.5, page) * (1.0 - step(1.5, page)) + step(2.5, page);
    return mix(lo, hi, take_hi);
}

vec4 sample_graphics(float x, float y) {
    if (u_r0_mode >= 3.5) {
        if (u_gr_en.x < 0.5) {
            return vec4(0.0);
        }
        float gx = x + u_gr_scroll01.x;
        if (gx >= 1024.0) { gx -= 1024.0; }
        float gy = y + u_gr_scroll01.y;
        if (gy >= 1024.0) { gy -= 1024.0; }
        float qx = step(512.0, gx);
        float qy = step(512.0, gy);
        float slot = qy * 2.0 + qx;
        float page = u_slots.x;
        if (slot > 0.5 && slot < 1.5) { page = u_slots.y; }
        else if (slot >= 1.5 && slot < 2.5) { page = u_slots.z; }
        else if (slot >= 2.5) { page = u_slots.w; }
        float idx = gv_nibble(page, gx - qx * 512.0, gy - qy * 512.0, 0.0);
        if (idx < 0.5) {
            return vec4(0.0);
        }
        return pal_rgba(256.0 + idx);
    }

    if (u_gr_en.x + u_gr_en.y + u_gr_en.z + u_gr_en.w < 0.5) {
        return vec4(0.0);
    }

    if (u_r0_mode < 0.5) {
        if (u_gr_en.x > 0.5) {
            float n0 = gv_nibble(u_slots.x, x, y, 1.0);
            if (n0 > 0.5) { return pal_rgba(256.0 + n0); }
        }
        if (u_gr_en.y > 0.5) {
            float n1 = gv_nibble(u_slots.y, x, y, 1.0);
            if (n1 > 0.5) { return pal_rgba(256.0 + n1); }
        }
        if (u_gr_en.z > 0.5) {
            float n2 = gv_nibble(u_slots.z, x, y, 1.0);
            if (n2 > 0.5) { return pal_rgba(256.0 + n2); }
        }
        if (u_gr_en.w > 0.5) {
            float n3 = gv_nibble(u_slots.w, x, y, 1.0);
            if (n3 > 0.5) { return pal_rgba(256.0 + n3); }
        }
        return vec4(0.0);
    }

    // 256-color mode
    if (u_gr_en.x + u_gr_en.y > 0.5) {
        float c01 = gv_nibble(u_slots.x, x, y, 1.0) + gv_nibble(u_slots.y, x, y, 1.0) * 16.0;
        if (c01 > 0.5) { return pal_rgba(256.0 + c01); }
    }
    if (u_gr_en.z + u_gr_en.w > 0.5) {
        float c23 = gv_nibble(u_slots.z, x, y, 1.0) + gv_nibble(u_slots.w, x, y, 1.0) * 16.0;
        if (c23 > 0.5) { return pal_rgba(256.0 + c23); }
    }
    return vec4(0.0);
}

vec4 eval_ts(float is_text, float x, float y) {
    if (is_text > 0.5) {
        return sample_text(x, y);
    }
    return sample_cynthia(x, y);
}

void main() {
    float x = floor(gl_FragCoord.x);
    float y = u_y_flip.x + floor(gl_FragCoord.y) * u_y_flip.y;

    float gr_rank = u_ranks.x;
    if (gr_rank >= 2.5) {
        vec4 gc = sample_graphics(x, y);
        gl_FragColor = vec4(gc.rgb * step(0.5, gc.a), 1.0);
        return;
    }

    float tx_first = step(u_ranks.y, u_ranks.z);
    vec4 col = vec4(0.0);

    if (gr_rank < 0.5) {
        col = sample_graphics(x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(tx_first, x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(1.0 - tx_first, x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
    } else if (gr_rank < 1.5) {
        col = eval_ts(tx_first, x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = sample_graphics(x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(1.0 - tx_first, x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
    } else {
        col = eval_ts(tx_first, x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(1.0 - tx_first, x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = sample_graphics(x, y);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
    }

    gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
}
"#;

/// Pass 2 fast per-row VICON shader:
/// - Used when no row uses 65536-color mode, but VICON/palette/contrast varies per row or extended TVRAM is active.
/// - Zero `u_lut` fetches, pre-baked RGBA palette, 1-fetch packed RGBA standard TVRAM,
///   lazy GVRAM scroll row fetches, disabled-page skipping, and front-to-back early-out.
pub const VICON_FAST_RASTER_FS: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform sampler2D u_tvram;
uniform sampler2D u_gvram;
uniform sampler2D u_cynthia;
uniform sampler2D u_row_state;
uniform sampler2D u_palette;

uniform float u_row_inv_h;
uniform float u_pal_inv_h;
uniform float u_cynthia_inv_h;
uniform float u_has_row_map;
uniform float u_cynthia_enabled;
uniform float u_is_ext_tvram;
uniform vec4 u_ext_tvram; // x: row_bytes, y: plane_bytes, z: tex_w, w: tex_inv_h
uniform vec2 u_y_flip;    // (0, 1) normal; (height - 1, -1) y-flipped

vec4 row_texel(float col, float y) {
    float ry = mix(0.0, y, u_has_row_map);
    return texture2D(u_row_state, vec2((col + 0.5) / 16.0, (ry + 0.5) * u_row_inv_h));
}

float bit_scale(float bit_pos) {
    float s = 1.0;
    float bp = bit_pos;
    if (bp < 4.0) { s *= 0.0625; } else { bp -= 4.0; }
    if (bp < 2.0) { s *= 0.25; } else { bp -= 2.0; }
    if (bp < 1.0) { s *= 0.5; }
    return s;
}

vec4 pal_rgba(float col, float pal_v) {
    return texture2D(u_palette, vec2((col + 0.5) / 512.0, pal_v));
}

vec4 sample_text(float x, float y, float r2, float pal_v) {
    if (mod(floor(r2 / 32.0), 2.0) < 0.5) {
        return vec4(0.0);
    }
    if (u_is_ext_tvram < 0.5) {
        vec4 r0 = row_texel(0.0, y);
        float tx_sx = floor(r0.r * 255.0 + 0.5) * 256.0 + floor(r0.g * 255.0 + 0.5);
        float tx_sy = floor(r0.b * 255.0 + 0.5) * 256.0 + floor(r0.a * 255.0 + 0.5);
        float tx = mod(x + tx_sx, 1024.0);
        float ty = y + tx_sy;
        if (ty >= 1024.0) {
            ty -= 1024.0;
        }
        float byte_x = floor(tx * 0.125);
        float bit_pos = tx - byte_x * 8.0;
        float half_sel = step(512.0, ty);
        float tex_y = (ty - half_sel * 512.0 + 0.5) / 512.0;
        float tex_x = (half_sel * 128.0 + byte_x + 0.5) / 256.0;
        vec4 tb = floor(texture2D(u_tvram, vec2(tex_x, tex_y)) * 255.0 + 0.5);
        vec4 shifted = floor(tb * bit_scale(bit_pos));
        vec4 bits = shifted - floor(shifted * 0.5) * 2.0;
        float tx_idx = dot(bits, vec4(1.0, 2.0, 4.0, 8.0));
        return pal_rgba(tx_idx, pal_v);
    }

    float byte_x = floor(x * 0.125);
    float bit_pos = x - byte_x * 8.0;
    float s = bit_scale(bit_pos);
    float in_plane = y * u_ext_tvram.x + byte_x;
    float acc = 0.0;
    float wt = 1.0;
    for (int p = 0; p < 4; p++) {
        float addr = float(p) * u_ext_tvram.y + in_plane;
        float ey = floor(addr / u_ext_tvram.z);
        float ex = addr - ey * u_ext_tvram.z;
        float b = floor(texture2D(u_tvram, vec2((ex + 0.5) / u_ext_tvram.z, (ey + 0.5) * u_ext_tvram.w)).r * 255.0 + 0.5);
        float sh = floor(b * s);
        float bit = sh - floor(sh * 0.5) * 2.0;
        acc += bit * wt;
        wt *= 2.0;
    }
    return pal_rgba(acc, pal_v);
}

vec4 sample_cynthia(float x, float y, float r2, float cynthia_active, float pal_v) {
    if (u_cynthia_enabled < 0.5 || cynthia_active < 0.5 || x >= 512.0 || mod(floor(r2 / 64.0), 2.0) < 0.5) {
        return vec4(0.0);
    }
    float code = floor(texture2D(u_cynthia, vec2((x + 0.5) / 512.0, (y + 0.5) * u_cynthia_inv_h)).r * 255.0 + 0.5);
    if (code < 0.5) {
        return vec4(0.0);
    }
    return pal_rgba(code, pal_v);
}

float gv_nibble_row(float page, float x, float y, float do_scroll) {
    float gx = x;
    if (gx >= 512.0) { gx = mod(gx, 512.0); }
    float gy = y;
    if (gy >= 512.0) { gy = mod(gy, 512.0); }
    if (do_scroll > 0.5) {
        vec4 gs = row_texel(1.0 + page, y);
        float gsx = floor(gs.r * 255.0 + 0.5) * 256.0 + floor(gs.g * 255.0 + 0.5);
        float gsy = floor(gs.b * 255.0 + 0.5) * 256.0 + floor(gs.a * 255.0 + 0.5);
        gx += gsx;
        if (gx >= 512.0) { gx -= 512.0; }
        gy += gsy;
        if (gy >= 512.0) { gy -= 512.0; }
    }
    vec4 gtex = texture2D(u_gvram, vec2((gx + 0.5) / 512.0, (gy + 0.5) / 512.0));
    float byte_val = mix(gtex.a, gtex.r, step(1.5, page));
    float b_int = floor(byte_val * 255.0 + 0.5);
    float hi = floor(b_int * 0.0625);
    float lo = b_int - hi * 16.0;
    float take_hi = step(0.5, page) * (1.0 - step(1.5, page)) + step(2.5, page);
    return mix(lo, hi, take_hi);
}

vec4 sample_graphics(float x, float y, float mode, float r2, float pal_v) {
    if (mode >= 3.5) {
        if (mod(floor(r2 / 16.0), 2.0) < 0.5) {
            return vec4(0.0);
        }
        vec4 slots = floor(row_texel(7.0, y) * 255.0 + vec4(0.5));
        vec4 gs0 = row_texel(1.0, y);
        float gs0x = floor(gs0.r * 255.0 + 0.5) * 256.0 + floor(gs0.g * 255.0 + 0.5);
        float gs0y = floor(gs0.b * 255.0 + 0.5) * 256.0 + floor(gs0.a * 255.0 + 0.5);
        float gx = mod(x + gs0x, 1024.0);
        float gy = mod(y + gs0y, 1024.0);
        float qx = step(512.0, gx);
        float qy = step(512.0, gy);
        float slot = qy * 2.0 + qx;
        float page = slots.x;
        if (slot > 0.5 && slot < 1.5) { page = slots.y; }
        else if (slot >= 1.5 && slot < 2.5) { page = slots.z; }
        else if (slot >= 2.5) { page = slots.w; }
        float idx = gv_nibble_row(page, gx - qx * 512.0, gy - qy * 512.0, 0.0);
        if (idx < 0.5) {
            return vec4(0.0);
        }
        return pal_rgba(256.0 + idx, pal_v);
    }

    float en0 = mod(r2, 2.0);
    float en1 = mod(floor(r2 * 0.5), 2.0);
    float en2 = mod(floor(r2 * 0.25), 2.0);
    float en3 = mod(floor(r2 * 0.125), 2.0);
    if (en0 + en1 + en2 + en3 < 0.5) {
        return vec4(0.0);
    }

    vec4 slots = floor(row_texel(7.0, y) * 255.0 + vec4(0.5));
    if (mode < 0.5) {
        if (en0 > 0.5) {
            float n0 = gv_nibble_row(slots.x, x, y, 1.0);
            if (n0 > 0.5) { return pal_rgba(256.0 + n0, pal_v); }
        }
        if (en1 > 0.5) {
            float n1 = gv_nibble_row(slots.y, x, y, 1.0);
            if (n1 > 0.5) { return pal_rgba(256.0 + n1, pal_v); }
        }
        if (en2 > 0.5) {
            float n2 = gv_nibble_row(slots.z, x, y, 1.0);
            if (n2 > 0.5) { return pal_rgba(256.0 + n2, pal_v); }
        }
        if (en3 > 0.5) {
            float n3 = gv_nibble_row(slots.w, x, y, 1.0);
            if (n3 > 0.5) { return pal_rgba(256.0 + n3, pal_v); }
        }
        return vec4(0.0);
    }

    if (en0 + en1 > 0.5) {
        float c01 = gv_nibble_row(slots.x, x, y, 1.0) + gv_nibble_row(slots.y, x, y, 1.0) * 16.0;
        if (c01 > 0.5) { return pal_rgba(256.0 + c01, pal_v); }
    }
    if (en2 + en3 > 0.5) {
        float c23 = gv_nibble_row(slots.z, x, y, 1.0) + gv_nibble_row(slots.w, x, y, 1.0) * 16.0;
        if (c23 > 0.5) { return pal_rgba(256.0 + c23, pal_v); }
    }
    return vec4(0.0);
}

vec4 eval_ts(float is_text, float x, float y, float r2, float cyn_act, float pal_v) {
    if (is_text > 0.5) {
        return sample_text(x, y, r2, pal_v);
    }
    return sample_cynthia(x, y, r2, cyn_act, pal_v);
}

void main() {
    float x = floor(gl_FragCoord.x);
    float y = u_y_flip.x + floor(gl_FragCoord.y) * u_y_flip.y;

    vec4 r5 = row_texel(5.0, y);
    float mode = floor(r5.r * 255.0 + 0.5);
    float r2 = floor(r5.g * 255.0 + 0.5);
    float cynthia_active = r5.a;

    vec4 r8 = row_texel(8.0, y);
    float pal_hi = floor(r8.g * 255.0 + 0.5);
    if (r2 < 0.5 || pal_hi > 254.5) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    float pal_id = floor(r8.r * 255.0 + 0.5) + pal_hi * 256.0;
    float pal_v = (pal_id + 0.5) * u_pal_inv_h;

    vec4 r6 = row_texel(6.0, y);
    float gr_rank = floor(r6.r * 255.0 + 0.5);
    if (gr_rank >= 2.5) {
        vec4 gc = sample_graphics(x, y, mode, r2, pal_v);
        gl_FragColor = vec4(gc.rgb * step(0.5, gc.a), 1.0);
        return;
    }

    float tx_rank = floor(r6.g * 255.0 + 0.5);
    float sp_rank = floor(r6.b * 255.0 + 0.5);
    float tx_first = step(tx_rank, sp_rank);
    vec4 col = vec4(0.0);

    if (gr_rank < 0.5) {
        col = sample_graphics(x, y, mode, r2, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(tx_first, x, y, r2, cynthia_active, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(1.0 - tx_first, x, y, r2, cynthia_active, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
    } else if (gr_rank < 1.5) {
        col = eval_ts(tx_first, x, y, r2, cynthia_active, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = sample_graphics(x, y, mode, r2, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(1.0 - tx_first, x, y, r2, cynthia_active, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
    } else {
        col = eval_ts(tx_first, x, y, r2, cynthia_active, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = eval_ts(1.0 - tx_first, x, y, r2, cynthia_active, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
        col = sample_graphics(x, y, mode, r2, pal_v);
        if (col.a > 0.5) { gl_FragColor = vec4(col.rgb, 1.0); return; }
    }

    gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
}
"#;

/// Pass 2 raw GRBI16 VICON shader (used when any row uses 65536-color byte-split palette mode).
pub const VICON_RAW_FS: &str = r#"
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

uniform sampler2D u_tvram;
uniform sampler2D u_gvram;
uniform sampler2D u_cynthia;
uniform sampler2D u_row_state;
uniform sampler2D u_palette;
uniform sampler2D u_lut;

uniform float u_row_inv_h;
uniform float u_pal_inv_h;
uniform float u_cynthia_inv_h;
uniform float u_has_row_map;
uniform float u_cynthia_enabled;
uniform float u_is_ext_tvram;
uniform vec4 u_ext_tvram; // x: row_bytes, y: plane_bytes, z: tex_w, w: tex_inv_h
uniform vec2 u_y_flip;    // (0, 1) normal; (height - 1, -1) y-flipped

vec4 row_texel(float col, float y) {
    float ry = mix(0.0, y, u_has_row_map);
    return texture2D(u_row_state, vec2((col + 0.5) / 16.0, (ry + 0.5) * u_row_inv_h));
}

float bit_scale(float bit_pos) {
    float s = 1.0;
    float bp = bit_pos;
    if (bp < 4.0) { s *= 0.0625; } else { bp -= 4.0; }
    if (bp < 2.0) { s *= 0.25; } else { bp -= 2.0; }
    if (bp < 1.0) { s *= 0.5; }
    return s;
}

float sample_tvram_idx(float x, float y, float tx_sx, float tx_sy) {
    if (u_is_ext_tvram < 0.5) {
        float tx = mod(x + tx_sx, 1024.0);
        float ty = y + tx_sy;
        if (ty >= 1024.0) {
            ty -= 1024.0;
        }
        float byte_x = floor(tx * 0.125);
        float bit_pos = tx - byte_x * 8.0;
        float half_sel = step(512.0, ty);
        float tex_y = (ty - half_sel * 512.0 + 0.5) / 512.0;
        float tex_x = (half_sel * 128.0 + byte_x + 0.5) / 256.0;
        vec4 tb = floor(texture2D(u_tvram, vec2(tex_x, tex_y)) * 255.0 + 0.5);
        vec4 shifted = floor(tb * bit_scale(bit_pos));
        vec4 bits = shifted - floor(shifted * 0.5) * 2.0;
        return dot(bits, vec4(1.0, 2.0, 4.0, 8.0));
    }

    float byte_x = floor(x * 0.125);
    float bit_pos = x - byte_x * 8.0;
    float s = bit_scale(bit_pos);
    float in_plane = y * u_ext_tvram.x + byte_x;
    float acc = 0.0;
    float wt = 1.0;
    for (int p = 0; p < 4; p++) {
        float addr = float(p) * u_ext_tvram.y + in_plane;
        float ey = floor(addr / u_ext_tvram.z);
        float ex = addr - ey * u_ext_tvram.z;
        float b = floor(texture2D(u_tvram, vec2((ex + 0.5) / u_ext_tvram.z, (ey + 0.5) * u_ext_tvram.w)).r * 255.0 + 0.5);
        float sh = floor(b * s);
        float bit = sh - floor(sh * 0.5) * 2.0;
        acc += bit * wt;
        wt *= 2.0;
    }
    return acc;
}

float gv_nibble(float page, float x, float y, float do_scroll) {
    float gx = x;
    if (gx >= 512.0) { gx = mod(gx, 512.0); }
    float gy = y;
    if (gy >= 512.0) { gy = mod(gy, 512.0); }
    if (do_scroll > 0.5) {
        vec4 gs = row_texel(1.0 + page, y);
        float gsx = floor(gs.r * 255.0 + 0.5) * 256.0 + floor(gs.g * 255.0 + 0.5);
        float gsy = floor(gs.b * 255.0 + 0.5) * 256.0 + floor(gs.a * 255.0 + 0.5);
        gx += gsx;
        if (gx >= 512.0) { gx -= 512.0; }
        gy += gsy;
        if (gy >= 512.0) { gy -= 512.0; }
    }
    vec4 gtex = texture2D(u_gvram, vec2((gx + 0.5) / 512.0, (gy + 0.5) / 512.0));
    float byte_val = mix(gtex.a, gtex.r, step(1.5, page));
    float b_int = floor(byte_val * 255.0 + 0.5);
    float hi = floor(b_int * 0.0625);
    float lo = b_int - hi * 16.0;
    float take_hi = step(0.5, page) * (1.0 - step(1.5, page)) + step(2.5, page);
    return mix(lo, hi, take_hi);
}

vec4 pal_entry(float col, float pal_v) {
    return texture2D(u_palette, vec2((col + 0.5) / 512.0, pal_v));
}

vec4 sample_graphics_grbi(float x, float y, float mode, float r2, vec4 slots, float pal_v, float want_sub) {
    if (mode >= 3.5) {
        if (mod(floor(r2 / 16.0), 2.0) < 0.5) {
            return vec4(0.0);
        }
        vec4 gs0 = row_texel(1.0, y);
        float gs0x = floor(gs0.r * 255.0 + 0.5) * 256.0 + floor(gs0.g * 255.0 + 0.5);
        float gs0y = floor(gs0.b * 255.0 + 0.5) * 256.0 + floor(gs0.a * 255.0 + 0.5);
        float gx = mod(x + gs0x, 1024.0);
        float gy = mod(y + gs0y, 1024.0);
        float qx = step(512.0, gx);
        float qy = step(512.0, gy);
        float slot = qy * 2.0 + qx;
        float page = slots.x;
        if (slot > 0.5 && slot < 1.5) { page = slots.y; }
        else if (slot >= 1.5 && slot < 2.5) { page = slots.z; }
        else if (slot >= 2.5) { page = slots.w; }
        float idx = gv_nibble(page, gx - qx * 512.0, gy - qy * 512.0, 0.0);
        if (idx < 0.5) { return vec4(0.0); }
        vec4 pe = pal_entry(256.0 + idx, pal_v);
        return vec4(pe.r, pe.a, 0.0, 0.0);
    }

    float en0 = mod(r2, 2.0);
    float en1 = mod(floor(r2 * 0.5), 2.0);
    float en2 = mod(floor(r2 * 0.25), 2.0);
    float en3 = mod(floor(r2 * 0.125), 2.0);
    if (en0 + en1 + en2 + en3 < 0.5) {
        return vec4(0.0);
    }

    if (mode < 0.5) {
        float idx = 0.0;
        float sub_idx = 0.0;
        if (en0 > 0.5) {
            float n = gv_nibble(slots.x, x, y, 1.0);
            if (n > 0.5) { idx = n; }
        }
        if (en1 > 0.5 && (idx < 0.5 || (want_sub > 0.5 && sub_idx < 0.5))) {
            float n = gv_nibble(slots.y, x, y, 1.0);
            if (n > 0.5) {
                if (idx < 0.5) { idx = n; } else { sub_idx = n; }
            }
        }
        if (en2 > 0.5 && (idx < 0.5 || (want_sub > 0.5 && sub_idx < 0.5))) {
            float n = gv_nibble(slots.z, x, y, 1.0);
            if (n > 0.5) {
                if (idx < 0.5) { idx = n; } else { sub_idx = n; }
            }
        }
        if (en3 > 0.5 && (idx < 0.5 || (want_sub > 0.5 && sub_idx < 0.5))) {
            float n = gv_nibble(slots.w, x, y, 1.0);
            if (n > 0.5) {
                if (idx < 0.5) { idx = n; } else { sub_idx = n; }
            }
        }
        if (idx < 0.5) { return vec4(0.0); }
        vec4 pe = pal_entry(256.0 + idx, pal_v);
        vec2 sub = vec2(0.0);
        if (sub_idx > 0.5) {
            vec4 se = pal_entry(256.0 + sub_idx, pal_v);
            sub = vec2(se.r, se.a);
        }
        return vec4(pe.r, pe.a, sub.x, sub.y);
    } else if (mode < 1.5) {
        float idx = 0.0;
        float sub_idx = 0.0;
        if (en0 + en1 > 0.5) {
            float c = gv_nibble(slots.x, x, y, 1.0) + gv_nibble(slots.y, x, y, 1.0) * 16.0;
            if (c > 0.5) { idx = c; }
        }
        if (en2 + en3 > 0.5 && (idx < 0.5 || want_sub > 0.5)) {
            float c = gv_nibble(slots.z, x, y, 1.0) + gv_nibble(slots.w, x, y, 1.0) * 16.0;
            if (c > 0.5) {
                if (idx < 0.5) { idx = c; } else { sub_idx = c; }
            }
        }
        if (idx < 0.5) { return vec4(0.0); }
        vec4 pe = pal_entry(256.0 + idx, pal_v);
        vec2 sub = vec2(0.0);
        if (sub_idx > 0.5) {
            vec4 se = pal_entry(256.0 + sub_idx, pal_v);
            sub = vec2(se.r, se.a);
        }
        return vec4(pe.r, pe.a, sub.x, sub.y);
    }

    // 65536-color byte-split palette mode
    float idx_lo = gv_nibble(slots.x, x, y, 1.0) + gv_nibble(slots.y, x, y, 1.0) * 16.0;
    float idx_hi = gv_nibble(slots.z, x, y, 1.0) + gv_nibble(slots.w, x, y, 1.0) * 16.0;
    float lo_odd = idx_lo - floor(idx_lo * 0.5) * 2.0;
    float hi_odd = idx_hi - floor(idx_hi * 0.5) * 2.0;
    vec4 lo_w = pal_entry(256.0 + idx_lo - lo_odd, pal_v);
    vec4 hi_w = pal_entry(257.0 + idx_hi - hi_odd, pal_v);
    float lo_b = mix(lo_w.r, lo_w.a, lo_odd);
    float hi_b = mix(hi_w.r, hi_w.a, hi_odd);
    return vec4(hi_b, lo_b, 0.0, 0.0);
}

vec3 decode_grbi_rgb6(vec2 grbi) {
    vec4 u_hi = texture2D(u_lut, vec2((grbi.x * 255.0 + 0.5) / 256.0, (10.0 + 0.5) / 32.0));
    vec4 u_lo = texture2D(u_lut, vec2((grbi.y * 255.0 + 0.5) / 256.0, (11.0 + 0.5) / 32.0));
    float g5 = floor(u_hi.r * 255.0 + 0.5);
    float r_hi3 = floor(u_hi.g * 255.0 + 0.5);
    float r_lo2 = floor(u_lo.r * 255.0 + 0.5);
    float b5 = floor(u_lo.g * 255.0 + 0.5);
    float ibit = floor(u_lo.b * 255.0 + 0.5);
    return vec3((r_hi3 * 4.0 + r_lo2) * 2.0 + ibit, g5 * 2.0 + ibit, b5 * 2.0 + ibit);
}

float grbi_i_bit(vec2 grbi) {
    float lo_int = floor(grbi.y * 255.0 + 0.5);
    return lo_int - floor(lo_int * 0.5) * 2.0;
}

void main() {
    float x = floor(gl_FragCoord.x);
    float y = u_y_flip.x + floor(gl_FragCoord.y) * u_y_flip.y;

    vec4 r0 = row_texel(0.0, y);
    vec4 r5 = row_texel(5.0, y);
    float mode = floor(r5.r * 255.0 + 0.5);
    float r2 = floor(r5.g * 255.0 + 0.5);
    float cynthia_active = r5.a;

    vec4 r8 = row_texel(8.0, y);
    float pal_hi = floor(r8.g * 255.0 + 0.5);
    float c_row = 16.0 + floor(r8.b * 255.0 + 0.5);
    float c_col_base = floor(r8.a * 255.0 + 0.5);

    if (r2 < 0.5 || pal_hi > 254.5) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    float pal_id = floor(r8.r * 255.0 + 0.5) + pal_hi * 256.0;

    vec4 r6 = row_texel(6.0, y);
    float gr_rank = floor(r6.r * 255.0 + 0.5);
    float tx_rank = floor(r6.g * 255.0 + 0.5);
    float sp_rank = floor(r6.b * 255.0 + 0.5);
    float r6_a = floor(r6.a * 255.0 + 0.5);
    float interlace_dim = step(127.5, r6_a);
    float r6_low = r6_a - interlace_dim * 128.0;
    float cyn_h_scale = floor(r6_low / 32.0);
    float r2_hi = r6_low - cyn_h_scale * 32.0;
    float gt = mod(r2_hi, 2.0);
    float gg = mod(floor(r2_hi * 0.5), 2.0);
    float bp = mod(floor(r2_hi * 0.25), 2.0);
    float hp = mod(floor(r2_hi * 0.125), 2.0);
    float exon = mod(floor(r2_hi * 0.0625), 2.0);
    float want_sub = exon * hp * gg;

    vec4 r7 = row_texel(7.0, y);
    vec4 slots = floor(r7 * 255.0 + vec4(0.5));
    float pal_v = (pal_id + 0.5) * u_pal_inv_h;

    vec2 tx_grbi = vec2(0.0);
    vec2 bg_grbi = vec2(0.0);
    if (gr_rank < 2.5) {
        if (mod(floor(r2 / 32.0), 2.0) > 0.5) {
            float tx_sx = floor(r0.r * 255.0 + 0.5) * 256.0 + floor(r0.g * 255.0 + 0.5);
            float tx_sy = floor(r0.b * 255.0 + 0.5) * 256.0 + floor(r0.a * 255.0 + 0.5);
            float tx_idx = sample_tvram_idx(x, y, tx_sx, tx_sy);
            vec4 te = pal_entry(tx_idx, pal_v);
            tx_grbi = vec2(te.r, te.a);
        }
        if (u_cynthia_enabled > 0.5 && cynthia_active > 0.5 && mod(floor(r2 / 64.0), 2.0) > 0.5) {
            float cx = x;
            if (cyn_h_scale > 0.5 && cyn_h_scale < 1.5) {
                cx = floor(x * 0.5);
            } else if (cyn_h_scale >= 1.5) {
                cx = x * 2.0;
            }
            if (cx < 512.0) {
                float code = floor(texture2D(u_cynthia, vec2((cx + 0.5) / 512.0, (y + 0.5) * u_cynthia_inv_h)).r * 255.0 + 0.5);
                if (code > 0.5) {
                    vec4 ce = pal_entry(code, pal_v);
                    bg_grbi = vec2(ce.r, ce.a);
                }
            }
        }
    }

    vec4 gr_pair = sample_graphics_grbi(x, y, mode, r2, slots, pal_v, want_sub);
    vec2 gr_grbi = gr_pair.xy;
    vec2 gr_sub = gr_pair.zw;

    float tx_first = step(tx_rank, sp_rank);
    vec2 front = mix(bg_grbi, tx_grbi, tx_first);
    vec2 back = mix(tx_grbi, bg_grbi, tx_first);

    vec2 c0 = gr_grbi; float g0 = 1.0;
    vec2 c1 = front;   float g1 = 0.0;
    vec2 c2 = back;    float g2 = 0.0;
    if (gr_rank > 0.5 && gr_rank < 1.5) {
        c0 = front;   g0 = 0.0;
        c1 = gr_grbi; g1 = 1.0;
        c2 = back;    g2 = 0.0;
    } else if (gr_rank >= 1.5 && gr_rank < 2.5) {
        c0 = front;   g0 = 0.0;
        c1 = back;    g1 = 0.0;
        c2 = gr_grbi; g2 = 1.0;
    } else if (gr_rank >= 2.5) {
        c1 = vec2(0.0);
        c2 = vec2(0.0);
    }

    vec2 fg_grbi = vec2(0.0);
    vec2 bg_blend_grbi = vec2(0.0);
    float do_blend = 0.0;

    if (exon < 0.5) {
        fg_grbi = c0;
        if (fg_grbi.x + fg_grbi.y < 0.001) { fg_grbi = c1; }
        if (fg_grbi.x + fg_grbi.y < 0.001) { fg_grbi = c2; }
    } else if (hp < 0.5) {
        if (gr_grbi.x + gr_grbi.y >= 0.001 && grbi_i_bit(gr_grbi) > 0.5) {
            fg_grbi = gr_grbi;
        } else {
            fg_grbi = c0;
            if (fg_grbi.x + fg_grbi.y < 0.001) { fg_grbi = c1; }
            if (fg_grbi.x + fg_grbi.y < 0.001) { fg_grbi = c2; }
        }
    } else {
        vec2 top = vec2(0.0);
        float top_is_gr = 0.0;
        vec2 second = vec2(0.0);
        if (c0.x + c0.y >= 0.001) {
            top = c0; top_is_gr = g0;
            second = (c1.x + c1.y >= 0.001) ? c1 : c2;
        } else if (c1.x + c1.y >= 0.001) {
            top = c1; top_is_gr = g1;
            second = c2;
        } else {
            top = c2; top_is_gr = g2;
        }
        fg_grbi = top;
        if (top.x + top.y >= 0.001) {
            float trigger = max(bp, grbi_i_bit(top));
            if (top_is_gr > 0.5) {
                if (trigger > 0.5 && gg > 0.5 && gr_sub.x + gr_sub.y >= 0.001) {
                    bg_blend_grbi = gr_sub;
                    do_blend = 1.0;
                } else if (trigger > 0.5 && gt > 0.5) {
                    bg_blend_grbi = second;
                    do_blend = 1.0;
                } else if (trigger > 0.5 && gg > 0.5) {
                    bg_blend_grbi = vec2(0.0);
                    do_blend = 1.0;
                }
            } else if (trigger > 0.5 && gt > 0.5) {
                bg_blend_grbi = second;
                do_blend = 1.0;
            }
        }
    }

    if (fg_grbi.x + fg_grbi.y < 0.001) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    vec3 rgb6 = decode_grbi_rgb6(fg_grbi);
    if (do_blend > 0.5) {
        vec3 bg_rgb6 = (bg_blend_grbi.x + bg_blend_grbi.y >= 0.001) ? decode_grbi_rgb6(bg_blend_grbi) : vec3(0.0);
        rgb6 = floor((rgb6 + bg_rgb6) * 0.5);
    }

    float lut_v = (c_row + 0.5) / 32.0;
    float r8_out = texture2D(u_lut, vec2((c_col_base + rgb6.x + 0.5) / 256.0, lut_v)).r;
    float g8_out = texture2D(u_lut, vec2((c_col_base + rgb6.y + 0.5) / 256.0, lut_v)).r;
    float b8_out = texture2D(u_lut, vec2((c_col_base + rgb6.z + 0.5) / 256.0, lut_v)).r;

    if (interlace_dim > 0.5) {
        r8_out = floor(floor(r8_out * 255.0 + 0.5) * 0.75) / 255.0;
        g8_out = floor(floor(g8_out * 255.0 + 0.5) * 0.75) / 255.0;
        b8_out = floor(floor(b8_out * 255.0 + 0.5) * 0.75) / 255.0;
    }

    gl_FragColor = vec4(r8_out, g8_out, b8_out, 1.0);
}
"#;

/// Pass 3 GPU downsample blit vertex shader.
pub const BLIT_VS: &str = r#"
attribute vec2 a_pos;
uniform vec2 u_uv_scale;
varying vec2 v_uv;

void main() {
    gl_Position = vec4(a_pos, 0.0, 1.0);
    v_uv = (a_pos * 0.5 + vec2(0.5)) * u_uv_scale;
}
"#;

/// Pass 3 GPU downsample blit fragment shader.
pub const BLIT_FS: &str = r#"
precision mediump float;

uniform sampler2D u_src;
uniform vec2 u_uv_max;
varying vec2 v_uv;

void main() {
    gl_FragColor = vec4(texture2D(u_src, min(v_uv, u_uv_max)).rgb, 1.0);
}
"#;
