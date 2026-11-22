//! GLSL ES 1.00 shaders for Pass 1 (CYNTHIA sprite spans + BG0/BG1) and Pass 2 (VICON).
//!
//! All standard X68000 paths are strictly bounded to `mediump`-exact integer ranges (`<= 1535`),
//! avoiding bitwise operations and large texture coordinates.

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
uniform sampler2D u_lut;
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
        vec4 nib = texture2D(u_lut, vec2((b * 255.0 + 0.5) / 256.0, (8.0 + 0.5) / 32.0));
        nibble = floor(mix(nib.r, nib.g, px_odd) * 255.0 + 0.5);
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
uniform sampler2D u_lut;

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
        float m_row_hi = floor(mv0 / 8.0 + 0.001);
        float m_col = mv0 - m_row_hi * 8.0;
        float m_row = m_row_hi * 256.0 + floor(map_ref.a * 255.0 + 0.5);
        float in_blk = tile_x * 2.0;
        tile_hi_norm = pcg_byte(m_col, m_row, in_blk);
        tile_lo_norm = pcg_byte(m_col, m_row, in_blk + 1.0);
    }

    vec4 hi_info = texture2D(u_lut, vec2((tile_hi_norm * 255.0 + 0.5) / 256.0, (9.0 + 0.5) / 32.0));
    float pal_block = floor(hi_info.r * 255.0 + 0.5);
    float hflip = hi_info.g;
    float vflip = hi_info.b;
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
    float p_row_hi = floor(pv0 / 8.0 + 0.001);
    float p_col = pv0 - p_row_hi * 8.0;
    float p_row = p_row_hi * 256.0 + floor(pat_ref.a * 255.0 + 0.5);
    float pb = pcg_byte(p_col, p_row, pat_in_blk);
    vec4 nib = texture2D(u_lut, vec2((pb * 255.0 + 0.5) / 256.0, (8.0 + 0.5) / 32.0));
    float nibble = floor(mix(nib.r, nib.g, px_odd) * 255.0 + 0.5);
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

pub const VICON_FS: &str = r#"
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

vec4 row_texel(float col, float y) {
    float ry = mix(0.0, y, u_has_row_map);
    return texture2D(u_row_state, vec2((col + 0.5) / 16.0, (ry + 0.5) * u_row_inv_h));
}

float lut_bit(float byte_norm, float bit_pos) {
    return texture2D(u_lut, vec2((byte_norm * 255.0 + 0.5) / 256.0, (bit_pos + 0.5) / 32.0)).r;
}

float sample_tvram_idx(float x, float y, float tx_sx, float tx_sy) {
    float tx = x;
    float ty = y;
    float byte_x = 0.0;
    float bit_pos = 0.0;
    if (u_is_ext_tvram < 0.5) {
        tx = mod(x + tx_sx, 1024.0);
        ty = y + tx_sy;
        if (ty >= 1024.0) {
            ty -= 1024.0;
        }
        byte_x = floor(tx / 8.0);
        bit_pos = tx - byte_x * 8.0;
        float half_sel = step(512.0, ty);
        float tex_y = (ty - half_sel * 512.0 + 0.5) / 512.0;
        float u_base = half_sel * 128.0 + byte_x + 0.5;
        float b0 = lut_bit(texture2D(u_tvram, vec2((0.0 + u_base) / 1024.0, tex_y)).r, bit_pos);
        float b1 = lut_bit(texture2D(u_tvram, vec2((256.0 + u_base) / 1024.0, tex_y)).r, bit_pos);
        float b2 = lut_bit(texture2D(u_tvram, vec2((512.0 + u_base) / 1024.0, tex_y)).r, bit_pos);
        float b3 = lut_bit(texture2D(u_tvram, vec2((768.0 + u_base) / 1024.0, tex_y)).r, bit_pos);
        return b0 + b1 * 2.0 + b2 * 4.0 + b3 * 8.0;
    }

    byte_x = floor(tx / 8.0);
    bit_pos = tx - byte_x * 8.0;
    float in_plane = ty * u_ext_tvram.x + byte_x;
    float acc = 0.0;
    float wt = 1.0;
    for (int p = 0; p < 4; p++) {
        float addr = float(p) * u_ext_tvram.y + in_plane;
        float ey = floor(addr / u_ext_tvram.z);
        float ex = addr - ey * u_ext_tvram.z;
        float b = texture2D(u_tvram, vec2((ex + 0.5) / u_ext_tvram.z, (ey + 0.5) * u_ext_tvram.w)).r;
        acc += lut_bit(b, bit_pos) * wt;
        wt *= 2.0;
    }
    return acc;
}

float gv_nibble(float page, float x, float y, float gx_sc, float gy_sc, float do_scroll) {
    float gx = x;
    if (gx >= 512.0) {
        gx = mod(gx, 512.0);
    }
    float gy = y;
    if (gy >= 512.0) {
        gy = mod(gy, 512.0);
    }
    if (do_scroll > 0.5) {
        gx += gx_sc;
        if (gx >= 512.0) {
            gx -= 512.0;
        }
        gy += gy_sc;
        if (gy >= 512.0) {
            gy -= 512.0;
        }
    }
    vec4 gtex = texture2D(u_gvram, vec2((gx + 0.5) / 512.0, (gy + 0.5) / 512.0));
    float byte_val = mix(gtex.a, gtex.r, step(1.5, page));
    vec4 nib = texture2D(u_lut, vec2((byte_val * 255.0 + 0.5) / 256.0, (8.0 + 0.5) / 32.0));
    float take_hi = step(0.5, page) * (1.0 - step(1.5, page)) + step(2.5, page);
    return floor(mix(nib.g, nib.r, take_hi) * 255.0 + 0.5);
}

vec4 pal_entry(float col, float pal_v) {
    return texture2D(u_palette, vec2((col + 0.5) / 512.0, pal_v));
}

vec2 sample_graphics_grbi(float x, float y, float mode, float r2, vec4 slots, float pal_v) {
    float idx_lo = 0.0;
    float idx_hi = 0.0;
    float got = 0.0;

    if (mode >= 3.5) {
        if (mod(floor(r2 / 16.0), 2.0) < 0.5) {
            return vec2(0.0);
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
        idx_lo = gv_nibble(page, gx - qx * 512.0, gy - qy * 512.0, 0.0, 0.0, 0.0);
    } else {
        float en0 = mod(r2, 2.0);
        float en1 = mod(floor(r2 * 0.5), 2.0);
        float en2 = mod(floor(r2 * 0.25), 2.0);
        float en3 = mod(floor(r2 * 0.125), 2.0);
        if (en0 + en1 + en2 + en3 < 0.5) {
            return vec2(0.0);
        }
        vec4 gs0 = row_texel(1.0, y);
        vec4 gs1 = row_texel(2.0, y);
        vec4 gs2 = row_texel(3.0, y);
        vec4 gs3 = row_texel(4.0, y);

        float p0 = slots.x;
        float p1 = slots.y;
        float p2 = slots.z;
        float p3 = slots.w;

        float gsx[4];
        float gsy[4];
        gsx[0] = floor(gs0.r * 255.0 + 0.5) * 256.0 + floor(gs0.g * 255.0 + 0.5);
        gsy[0] = floor(gs0.b * 255.0 + 0.5) * 256.0 + floor(gs0.a * 255.0 + 0.5);
        gsx[1] = floor(gs1.r * 255.0 + 0.5) * 256.0 + floor(gs1.g * 255.0 + 0.5);
        gsy[1] = floor(gs1.b * 255.0 + 0.5) * 256.0 + floor(gs1.a * 255.0 + 0.5);
        gsx[2] = floor(gs2.r * 255.0 + 0.5) * 256.0 + floor(gs2.g * 255.0 + 0.5);
        gsy[2] = floor(gs2.b * 255.0 + 0.5) * 256.0 + floor(gs2.a * 255.0 + 0.5);
        gsx[3] = floor(gs3.r * 255.0 + 0.5) * 256.0 + floor(gs3.g * 255.0 + 0.5);
        gsy[3] = floor(gs3.b * 255.0 + 0.5) * 256.0 + floor(gs3.a * 255.0 + 0.5);

        float n0 = 0.0;
        float n1 = 0.0;
        float n2 = 0.0;
        float n3 = 0.0;
        if (p0 < 0.5) { n0 = gv_nibble(p0, x, y, gsx[0], gsy[0], 1.0); }
        else if (p0 < 1.5) { n0 = gv_nibble(p0, x, y, gsx[1], gsy[1], 1.0); }
        else if (p0 < 2.5) { n0 = gv_nibble(p0, x, y, gsx[2], gsy[2], 1.0); }
        else { n0 = gv_nibble(p0, x, y, gsx[3], gsy[3], 1.0); }

        if (p1 < 0.5) { n1 = gv_nibble(p1, x, y, gsx[0], gsy[0], 1.0); }
        else if (p1 < 1.5) { n1 = gv_nibble(p1, x, y, gsx[1], gsy[1], 1.0); }
        else if (p1 < 2.5) { n1 = gv_nibble(p1, x, y, gsx[2], gsy[2], 1.0); }
        else { n1 = gv_nibble(p1, x, y, gsx[3], gsy[3], 1.0); }

        if (p2 < 0.5) { n2 = gv_nibble(p2, x, y, gsx[0], gsy[0], 1.0); }
        else if (p2 < 1.5) { n2 = gv_nibble(p2, x, y, gsx[1], gsy[1], 1.0); }
        else if (p2 < 2.5) { n2 = gv_nibble(p2, x, y, gsx[2], gsy[2], 1.0); }
        else { n2 = gv_nibble(p2, x, y, gsx[3], gsy[3], 1.0); }

        if (p3 < 0.5) { n3 = gv_nibble(p3, x, y, gsx[0], gsy[0], 1.0); }
        else if (p3 < 1.5) { n3 = gv_nibble(p3, x, y, gsx[1], gsy[1], 1.0); }
        else if (p3 < 2.5) { n3 = gv_nibble(p3, x, y, gsx[2], gsy[2], 1.0); }
        else { n3 = gv_nibble(p3, x, y, gsx[3], gsy[3], 1.0); }

        if (mode < 0.5) {
            if (en0 > 0.5 && n0 > 0.5) { idx_lo = n0; got = 1.0; }
            else if (en1 > 0.5 && n1 > 0.5) { idx_lo = n1; got = 1.0; }
            else if (en2 > 0.5 && n2 > 0.5) { idx_lo = n2; got = 1.0; }
            else if (en3 > 0.5 && n3 > 0.5) { idx_lo = n3; got = 1.0; }
            if (got < 0.5) { return vec2(0.0); }
        } else if (mode < 1.5) {
            float c01 = n0 + n1 * 16.0;
            float c23 = n2 + n3 * 16.0;
            if ((en0 + en1) > 0.5 && c01 > 0.5) { idx_lo = c01; got = 1.0; }
            else if ((en2 + en3) > 0.5 && c23 > 0.5) { idx_lo = c23; got = 1.0; }
            if (got < 0.5) { return vec2(0.0); }
        } else {
            idx_lo = n0 + n1 * 16.0;
            idx_hi = n2 + n3 * 16.0;
            float lo_odd = texture2D(u_lut, vec2((idx_lo + 0.5) / 256.0, (8.0 + 0.5) / 32.0)).b;
            float hi_odd = texture2D(u_lut, vec2((idx_hi + 0.5) / 256.0, (8.0 + 0.5) / 32.0)).b;
            vec4 lo_w = pal_entry(256.0 + idx_lo - lo_odd, pal_v);
            vec4 hi_w = pal_entry(257.0 + idx_hi - hi_odd, pal_v);
            float lo_b = mix(lo_w.r, lo_w.a, lo_odd);
            float hi_b = mix(hi_w.r, hi_w.a, hi_odd);
            return vec2(hi_b, lo_b);
        }
    }

    vec4 pe = pal_entry(256.0 + idx_lo, pal_v);
    return vec2(pe.r, pe.a);
}

void main() {
    float x = floor(gl_FragCoord.x);
    float y = floor(gl_FragCoord.y);

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
        if (u_cynthia_enabled > 0.5 && cynthia_active > 0.5 && x < 512.0 && mod(floor(r2 / 64.0), 2.0) > 0.5) {
            float code = floor(texture2D(u_cynthia, vec2((x + 0.5) / 512.0, (y + 0.5) * u_cynthia_inv_h)).r * 255.0 + 0.5);
            if (code > 0.5) {
                vec4 ce = pal_entry(code, pal_v);
                bg_grbi = vec2(ce.r, ce.a);
            }
        }
    }

    vec2 gr_grbi = sample_graphics_grbi(x, y, mode, r2, slots, pal_v);

    float tx_first = step(tx_rank, sp_rank);
    vec2 front = mix(bg_grbi, tx_grbi, tx_first);
    vec2 back = mix(tx_grbi, bg_grbi, tx_first);

    vec2 c0 = gr_grbi;
    vec2 c1 = front;
    vec2 c2 = back;
    if (gr_rank > 0.5 && gr_rank < 1.5) {
        c0 = front;
        c1 = gr_grbi;
        c2 = back;
    } else if (gr_rank >= 1.5 && gr_rank < 2.5) {
        c0 = front;
        c1 = back;
        c2 = gr_grbi;
    }

    vec2 final_grbi = c0;
    if (final_grbi.x + final_grbi.y < 0.001) {
        final_grbi = c1;
    }
    if (final_grbi.x + final_grbi.y < 0.001) {
        final_grbi = c2;
    }

    if (final_grbi.x + final_grbi.y < 0.001) {
        gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    vec4 u_hi = texture2D(u_lut, vec2((final_grbi.x * 255.0 + 0.5) / 256.0, (10.0 + 0.5) / 32.0));
    vec4 u_lo = texture2D(u_lut, vec2((final_grbi.y * 255.0 + 0.5) / 256.0, (11.0 + 0.5) / 32.0));

    float g5 = floor(u_hi.r * 255.0 + 0.5);
    float r_hi3 = floor(u_hi.g * 255.0 + 0.5);
    float r_lo2 = floor(u_lo.r * 255.0 + 0.5);
    float b5 = floor(u_lo.g * 255.0 + 0.5);
    float ibit = floor(u_lo.b * 255.0 + 0.5);

    float g6_val = g5 * 2.0 + ibit;
    float r6_val = (r_hi3 * 4.0 + r_lo2) * 2.0 + ibit;
    float b6_val = b5 * 2.0 + ibit;

    float lut_v = (c_row + 0.5) / 32.0;
    float r8_out = texture2D(u_lut, vec2((c_col_base + r6_val + 0.5) / 256.0, lut_v)).r;
    float g8_out = texture2D(u_lut, vec2((c_col_base + g6_val + 0.5) / 256.0, lut_v)).r;
    float b8_out = texture2D(u_lut, vec2((c_col_base + b6_val + 0.5) / 256.0, lut_v)).r;

    gl_FragColor = vec4(r8_out, g8_out, b8_out, 1.0);
}
"#;
