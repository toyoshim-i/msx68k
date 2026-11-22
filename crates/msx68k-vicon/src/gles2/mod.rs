//! OpenGL ES 2.0 (`EGL` + `GLES 2.0`) fallback backend (`wgpu`-independent).
//!
//! Renders CYNTHIA (coalesced `16×h` sprite spans + BG0/BG1) into a scissor-clipped
//! depth-buffered offscreen FBO (Pass 1), composites TVRAM (packed `GL_RGBA` 4-plane),
//! GVRAM, CYNTHIA, and pre-baked RGBA8 palettes (or raw GRBI16 palettes for 65536-color
//! mode) into the output FBO (Pass 2), and optionally downsamples into a target-sized
//! `RGBA8` or `RGB565` FBO (Pass 3) with pipelined `submit` / `finish` readback support.

mod ffi;
mod shaders;

use std::ffi::c_void;
use std::ptr;
use std::time::Instant;

use msx68k_bus::cynthia::CynthiaFrame;
use msx68k_bus::{DirtyVramPages, X68kBus, GVRAM_BYTES, TVRAM_BYTES, TVRAM_PLANE_BYTES};

use crate::config::{
    FrameTimingStats, PixelFormat, RenderedFrame, RenderedFrame565, RendererConfig,
};
use crate::raster;
use crate::ViconUniforms;

use ffi::*;

const MAX_BATCH_SPANS: usize = 16_384;
const VERTS_PER_SPAN: usize = 4;
const FLOATS_PER_VERT: usize = 9; // pos(2) + depth(1) + pat(2) + meta(4)

#[derive(Copy, Clone)]
struct PendingReadback {
    fbo: GLuint,
    width: u32,
    height: u32,
    rgb565_direct: bool,
    upload_us: u32,
    draw_us: u32,
}

/// Headless EGL + OpenGL ES 2.0 VICON compositor.
pub struct Gles2Compositor {
    ctx: EglContextGuard,
    renderer_name: String,
    max_width: u32,
    max_height: u32,
    max_tex_size: u32,
    rgb565_readback_supported: bool,

    prog_sprite: GLuint,
    prog_bg: GLuint,
    prog_vicon_fast_u: GLuint,
    prog_vicon_fast_r: GLuint,
    prog_vicon_raw: GLuint,
    prog_blit: GLuint,

    // Uniform locations: SPRITE
    sp_u_inv_half_size: GLint,
    sp_u_pcg: GLint,
    sp_u_pcg_inv_h: GLint,

    // Uniform locations: BG
    bg_u_depth_ndc: GLint,
    bg_u_pcg: GLint,
    bg_u_pcg_table: GLint,
    bg_u_row_state: GLint,
    bg_u_plane: GLint,
    bg_u_row_inv_h: GLint,
    bg_u_tbl_inv_h: GLint,
    bg_u_pcg_inv_h: GLint,

    // Uniform locations: VICON_FAST_UNIFORM
    vfu_u_tvram: GLint,
    vfu_u_gvram: GLint,
    vfu_u_cynthia: GLint,
    vfu_u_palette: GLint,
    vfu_u_cynthia_inv_h: GLint,
    vfu_u_pal_v: GLint,
    vfu_u_r0_mode: GLint,
    vfu_u_r2_display: GLint,
    vfu_u_ranks: GLint,
    vfu_u_slots: GLint,
    vfu_u_tx_scroll: GLint,
    vfu_u_gr_scroll01: GLint,
    vfu_u_gr_scroll23: GLint,

    // Uniform locations: VICON_FAST_RASTER
    vfr_u_tvram: GLint,
    vfr_u_gvram: GLint,
    vfr_u_cynthia: GLint,
    vfr_u_row_state: GLint,
    vfr_u_palette: GLint,
    vfr_u_row_inv_h: GLint,
    vfr_u_pal_inv_h: GLint,
    vfr_u_cynthia_inv_h: GLint,
    vfr_u_has_row_map: GLint,
    vfr_u_cynthia_enabled: GLint,
    vfr_u_is_ext_tvram: GLint,
    vfr_u_ext_tvram: GLint,

    // Uniform locations: VICON_RAW (65536-color mode)
    vr_u_tvram: GLint,
    vr_u_gvram: GLint,
    vr_u_cynthia: GLint,
    vr_u_row_state: GLint,
    vr_u_palette: GLint,
    vr_u_lut: GLint,
    vr_u_row_inv_h: GLint,
    vr_u_pal_inv_h: GLint,
    vr_u_cynthia_inv_h: GLint,
    vr_u_has_row_map: GLint,
    vr_u_cynthia_enabled: GLint,
    vr_u_is_ext_tvram: GLint,
    vr_u_ext_tvram: GLint,

    // Uniform locations: BLIT (Pass 3 downsample)
    bl_u_src: GLint,
    bl_u_uv_scale: GLint,
    bl_u_uv_max: GLint,

    // Textures
    tex_lut: GLuint,
    tex_tvram: GLuint,
    tex_gvram: GLuint,
    tex_pcg: GLuint,
    tex_pcg_table: GLuint,
    tex_row_state: GLuint,
    tex_palette_rgba: GLuint,
    tex_palette_raw: GLuint,
    tex_cynthia: GLuint,
    tex_output: [GLuint; 2],
    tex_scaled_rgba: [GLuint; 2],

    // Allocated texture / renderbuffer dimensions
    tvram_tex_w: u32,
    tvram_tex_h: u32,
    tvram_is_ext: bool,
    pcg_tex_h: u32,
    pcg_tbl_tex_h: u32,
    row_state_tex_h: u32,
    palette_rgba_tex_h: u32,
    palette_raw_tex_h: u32,
    cynthia_tex_h: u32,
    output_tex_w: [u32; 2],
    output_tex_h: [u32; 2],
    scaled_rgba_w: [u32; 2],
    scaled_rgba_h: [u32; 2],
    scaled_565_w: [u32; 2],
    scaled_565_h: [u32; 2],

    // FBOs & RBOs
    fbo_cynthia: GLuint,
    rbo_cynthia_depth: GLuint,
    fbo_output: [GLuint; 2],
    fbo_scaled_rgba: [GLuint; 2],
    fbo_scaled_565: [GLuint; 2],
    rbo_scaled_565: [GLuint; 2],
    cur_slot: usize,
    pending: Option<PendingReadback>,

    // Geometry buffers
    vbo_quad: GLuint,
    vbo_sprites: GLuint,
    ibo_sprites: GLuint,
    vbo_sprites_cap_bytes: usize,

    // Diagnostics
    collect_timing: bool,
    last_timing: Option<FrameTimingStats>,

    // Dirty tracking & CPU staging scratch
    vram_dirty_seeded: bool,
    prev_cursor_pages: u128,
    ext_tvram_was_active: bool,
    scratch_verts: Vec<f32>,
    scratch_row_state: Vec<u8>,
    scratch_palette_rgba: Vec<u8>,
    scratch_palette_raw: Vec<u8>,
    scratch_pcg: Vec<u8>,
    scratch_pcg_tbl: Vec<u8>,
    scratch_tvram_band: Vec<u8>,
    scratch_ext_tvram: Vec<u8>,
}

impl Gles2Compositor {
    /// Initializes an EGL + OpenGL ES 2.0 compositor if a compatible driver is available.
    pub fn new() -> Option<Self> {
        Self::new_with_config(RendererConfig::default())
    }

    /// Initializes an EGL + OpenGL ES 2.0 compositor using `config`.
    pub fn new_with_config(config: RendererConfig) -> Option<Self> {
        let ctx = EglContextGuard::new_headless()?;
        if !ctx.make_current() {
            return None;
        }
        if ctx.max_texture_units() < 8 {
            return None;
        }
        let max_tex_size = ctx.max_texture_size();
        let renderer_name = ctx.renderer_string();
        let gl = &ctx.gl;

        let prog_sprite = compile_program(
            gl,
            shaders::SPRITE_VS,
            shaders::SPRITE_FS,
            &[(0, "a_pos"), (1, "a_depth"), (2, "a_pat"), (3, "a_meta")],
        )
        .ok()?;
        let prog_bg = compile_program(gl, shaders::BG_VS, shaders::BG_FS, &[(0, "a_pos")]).ok()?;
        let prog_vicon_fast_u = compile_program(
            gl,
            shaders::VICON_VS,
            shaders::VICON_FAST_UNIFORM_FS,
            &[(0, "a_pos")],
        )
        .ok()?;
        let prog_vicon_fast_r = compile_program(
            gl,
            shaders::VICON_VS,
            shaders::VICON_FAST_RASTER_FS,
            &[(0, "a_pos")],
        )
        .ok()?;
        let prog_vicon_raw =
            compile_program(gl, shaders::VICON_VS, shaders::VICON_RAW_FS, &[(0, "a_pos")]).ok()?;
        let prog_blit =
            compile_program(gl, shaders::BLIT_VS, shaders::BLIT_FS, &[(0, "a_pos")]).ok()?;

        unsafe {
            (gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 1);
            (gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
            (gl.glDisable)(GL_BLEND);
            (gl.glDisable)(GL_DITHER);
            (gl.glDisable)(GL_CULL_FACE);
            (gl.glDisable)(GL_SCISSOR_TEST);
        }

        let tex_lut = create_nearest_texture(gl);
        let lut_data = build_lut_rgba();
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, tex_lut);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as GLint,
                256,
                32,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                lut_data.as_ptr() as *const c_void,
            );
        }

        // Standard TVRAM: 256×512 GL_RGBA (4 planes packed into R,G,B,A)
        let tex_tvram = create_nearest_texture(gl);
        let tvram_tex_w = 256;
        let tvram_tex_h = 512;
        unsafe {
            let zero = vec![0u8; TVRAM_BYTES];
            (gl.glBindTexture)(GL_TEXTURE_2D, tex_tvram);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as GLint,
                tvram_tex_w as GLsizei,
                tvram_tex_h as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                zero.as_ptr() as *const c_void,
            );
        }

        let tex_gvram = create_nearest_texture(gl);
        unsafe {
            let zero = vec![0u8; GVRAM_BYTES];
            (gl.glBindTexture)(GL_TEXTURE_2D, tex_gvram);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_LUMINANCE_ALPHA as GLint,
                512,
                512,
                0,
                GL_LUMINANCE_ALPHA,
                GL_UNSIGNED_BYTE,
                zero.as_ptr() as *const c_void,
            );
        }

        let tex_pcg = create_nearest_texture(gl);
        let tex_pcg_table = create_nearest_texture(gl);
        let tex_row_state = create_nearest_texture(gl);
        let tex_palette_rgba = create_nearest_texture(gl);
        let tex_palette_raw = create_nearest_texture(gl);
        let tex_cynthia = create_nearest_texture(gl);
        let tex_output = [create_linear_texture(gl), create_linear_texture(gl)];
        let tex_scaled_rgba = [create_nearest_texture(gl), create_nearest_texture(gl)];

        let mut fbos = [0u32; 7];
        let mut rbos = [0u32; 3];
        let mut vbos = [0u32; 3];
        unsafe {
            (gl.glGenFramebuffers)(fbos.len() as GLsizei, fbos.as_mut_ptr());
            (gl.glGenRenderbuffers)(rbos.len() as GLsizei, rbos.as_mut_ptr());
            (gl.glGenBuffers)(vbos.len() as GLsizei, vbos.as_mut_ptr());
        }
        let fbo_cynthia = fbos[0];
        let fbo_output = [fbos[1], fbos[2]];
        let fbo_scaled_rgba = [fbos[3], fbos[4]];
        let fbo_scaled_565 = [fbos[5], fbos[6]];
        let rbo_cynthia_depth = rbos[0];
        let rbo_scaled_565 = [rbos[1], rbos[2]];
        let [vbo_quad, vbo_sprites, ibo_sprites] = vbos;

        let quad_verts: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        let mut indices = Vec::with_capacity(MAX_BATCH_SPANS * 6);
        for i in 0..MAX_BATCH_SPANS {
            let base = (i * 4) as u16;
            indices.extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 1, base + 3]);
        }
        unsafe {
            (gl.glBindBuffer)(GL_ARRAY_BUFFER, vbo_quad);
            (gl.glBufferData)(
                GL_ARRAY_BUFFER,
                std::mem::size_of_val(&quad_verts) as GLsizeiptr,
                quad_verts.as_ptr() as *const c_void,
                GL_STATIC_DRAW,
            );
            (gl.glBindBuffer)(GL_ELEMENT_ARRAY_BUFFER, ibo_sprites);
            (gl.glBufferData)(
                GL_ELEMENT_ARRAY_BUFFER,
                (indices.len() * 2) as GLsizeiptr,
                indices.as_ptr() as *const c_void,
                GL_STATIC_DRAW,
            );
            (gl.glBindBuffer)(GL_ELEMENT_ARRAY_BUFFER, 0);
        }

        // Probe hardware RGB565 renderbuffer FBO + direct glReadPixels(GL_RGB, GL_UNSIGNED_SHORT_5_6_5)
        let rgb565_readback_supported = probe_rgb565_readback(gl, fbo_scaled_565[0], rbo_scaled_565[0]);

        let mut comp = Self {
            sp_u_inv_half_size: uniform_loc(gl, prog_sprite, "u_inv_half_size"),
            sp_u_pcg: uniform_loc(gl, prog_sprite, "u_pcg"),
            sp_u_pcg_inv_h: uniform_loc(gl, prog_sprite, "u_pcg_inv_h"),

            bg_u_depth_ndc: uniform_loc(gl, prog_bg, "u_depth_ndc"),
            bg_u_pcg: uniform_loc(gl, prog_bg, "u_pcg"),
            bg_u_pcg_table: uniform_loc(gl, prog_bg, "u_pcg_table"),
            bg_u_row_state: uniform_loc(gl, prog_bg, "u_row_state"),
            bg_u_plane: uniform_loc(gl, prog_bg, "u_plane"),
            bg_u_row_inv_h: uniform_loc(gl, prog_bg, "u_row_inv_h"),
            bg_u_tbl_inv_h: uniform_loc(gl, prog_bg, "u_tbl_inv_h"),
            bg_u_pcg_inv_h: uniform_loc(gl, prog_bg, "u_pcg_inv_h"),

            vfu_u_tvram: uniform_loc(gl, prog_vicon_fast_u, "u_tvram"),
            vfu_u_gvram: uniform_loc(gl, prog_vicon_fast_u, "u_gvram"),
            vfu_u_cynthia: uniform_loc(gl, prog_vicon_fast_u, "u_cynthia"),
            vfu_u_palette: uniform_loc(gl, prog_vicon_fast_u, "u_palette"),
            vfu_u_cynthia_inv_h: uniform_loc(gl, prog_vicon_fast_u, "u_cynthia_inv_h"),
            vfu_u_pal_v: uniform_loc(gl, prog_vicon_fast_u, "u_pal_v"),
            vfu_u_r0_mode: uniform_loc(gl, prog_vicon_fast_u, "u_r0_mode"),
            vfu_u_r2_display: uniform_loc(gl, prog_vicon_fast_u, "u_r2_display"),
            vfu_u_ranks: uniform_loc(gl, prog_vicon_fast_u, "u_ranks"),
            vfu_u_slots: uniform_loc(gl, prog_vicon_fast_u, "u_slots"),
            vfu_u_tx_scroll: uniform_loc(gl, prog_vicon_fast_u, "u_tx_scroll"),
            vfu_u_gr_scroll01: uniform_loc(gl, prog_vicon_fast_u, "u_gr_scroll01"),
            vfu_u_gr_scroll23: uniform_loc(gl, prog_vicon_fast_u, "u_gr_scroll23"),

            vfr_u_tvram: uniform_loc(gl, prog_vicon_fast_r, "u_tvram"),
            vfr_u_gvram: uniform_loc(gl, prog_vicon_fast_r, "u_gvram"),
            vfr_u_cynthia: uniform_loc(gl, prog_vicon_fast_r, "u_cynthia"),
            vfr_u_row_state: uniform_loc(gl, prog_vicon_fast_r, "u_row_state"),
            vfr_u_palette: uniform_loc(gl, prog_vicon_fast_r, "u_palette"),
            vfr_u_row_inv_h: uniform_loc(gl, prog_vicon_fast_r, "u_row_inv_h"),
            vfr_u_pal_inv_h: uniform_loc(gl, prog_vicon_fast_r, "u_pal_inv_h"),
            vfr_u_cynthia_inv_h: uniform_loc(gl, prog_vicon_fast_r, "u_cynthia_inv_h"),
            vfr_u_has_row_map: uniform_loc(gl, prog_vicon_fast_r, "u_has_row_map"),
            vfr_u_cynthia_enabled: uniform_loc(gl, prog_vicon_fast_r, "u_cynthia_enabled"),
            vfr_u_is_ext_tvram: uniform_loc(gl, prog_vicon_fast_r, "u_is_ext_tvram"),
            vfr_u_ext_tvram: uniform_loc(gl, prog_vicon_fast_r, "u_ext_tvram"),

            vr_u_tvram: uniform_loc(gl, prog_vicon_raw, "u_tvram"),
            vr_u_gvram: uniform_loc(gl, prog_vicon_raw, "u_gvram"),
            vr_u_cynthia: uniform_loc(gl, prog_vicon_raw, "u_cynthia"),
            vr_u_row_state: uniform_loc(gl, prog_vicon_raw, "u_row_state"),
            vr_u_palette: uniform_loc(gl, prog_vicon_raw, "u_palette"),
            vr_u_lut: uniform_loc(gl, prog_vicon_raw, "u_lut"),
            vr_u_row_inv_h: uniform_loc(gl, prog_vicon_raw, "u_row_inv_h"),
            vr_u_pal_inv_h: uniform_loc(gl, prog_vicon_raw, "u_pal_inv_h"),
            vr_u_cynthia_inv_h: uniform_loc(gl, prog_vicon_raw, "u_cynthia_inv_h"),
            vr_u_has_row_map: uniform_loc(gl, prog_vicon_raw, "u_has_row_map"),
            vr_u_cynthia_enabled: uniform_loc(gl, prog_vicon_raw, "u_cynthia_enabled"),
            vr_u_is_ext_tvram: uniform_loc(gl, prog_vicon_raw, "u_is_ext_tvram"),
            vr_u_ext_tvram: uniform_loc(gl, prog_vicon_raw, "u_ext_tvram"),

            bl_u_src: uniform_loc(gl, prog_blit, "u_src"),
            bl_u_uv_scale: uniform_loc(gl, prog_blit, "u_uv_scale"),
            bl_u_uv_max: uniform_loc(gl, prog_blit, "u_uv_max"),

            ctx,
            renderer_name,
            max_width: config.max_width.clamp(256, crate::MAX_FB_WIDTH),
            max_height: config.max_height.clamp(128, crate::MAX_FB_HEIGHT),
            max_tex_size,
            rgb565_readback_supported,

            prog_sprite,
            prog_bg,
            prog_vicon_fast_u,
            prog_vicon_fast_r,
            prog_vicon_raw,
            prog_blit,

            tex_lut,
            tex_tvram,
            tex_gvram,
            tex_pcg,
            tex_pcg_table,
            tex_row_state,
            tex_palette_rgba,
            tex_palette_raw,
            tex_cynthia,
            tex_output,
            tex_scaled_rgba,

            tvram_tex_w,
            tvram_tex_h,
            tvram_is_ext: false,
            pcg_tex_h: 0,
            pcg_tbl_tex_h: 0,
            row_state_tex_h: 0,
            palette_rgba_tex_h: 0,
            palette_raw_tex_h: 0,
            cynthia_tex_h: 0,
            output_tex_w: [0; 2],
            output_tex_h: [0; 2],
            scaled_rgba_w: [0; 2],
            scaled_rgba_h: [0; 2],
            scaled_565_w: [0; 2],
            scaled_565_h: [0; 2],

            fbo_cynthia,
            rbo_cynthia_depth,
            fbo_output,
            fbo_scaled_rgba,
            fbo_scaled_565,
            rbo_scaled_565,
            cur_slot: 0,
            pending: None,

            vbo_quad,
            vbo_sprites,
            ibo_sprites,
            vbo_sprites_cap_bytes: 0,

            collect_timing: false,
            last_timing: None,

            vram_dirty_seeded: false,
            prev_cursor_pages: 0,
            ext_tvram_was_active: false,
            scratch_verts: Vec::new(),
            scratch_row_state: Vec::new(),
            scratch_palette_rgba: Vec::new(),
            scratch_palette_raw: Vec::new(),
            scratch_pcg: Vec::new(),
            scratch_pcg_tbl: Vec::new(),
            scratch_tvram_band: vec![0u8; 128 * 32 * 4],
            scratch_ext_tvram: Vec::new(),
        };

        if !comp.ensure_cynthia_fbo(512) || !comp.ensure_output_fbo(0, 768, 512) {
            return None;
        }
        Some(comp)
    }

    /// Returns the `GL_RENDERER` string of the active GLES 2.0 driver.
    #[inline]
    pub fn renderer_name(&self) -> &str {
        &self.renderer_name
    }

    /// Returns whether this GLES 2.0 driver supports direct `GL_RGB565` FBO readback.
    #[inline]
    pub const fn supports_direct_rgb565_readback(&self) -> bool {
        self.rgb565_readback_supported
    }

    /// Enables or disables per-frame `upload / draw / readback` timing collection.
    ///
    /// When enabled, `glFinish()` is inserted between draw and readback so GPU execution
    /// time is isolated from `glReadPixels`. Disabled by default for zero overhead.
    #[inline]
    pub fn set_collect_timing(&mut self, enabled: bool) {
        self.collect_timing = enabled;
    }

    /// Returns the most recently collected [`FrameTimingStats`], if timing collection is enabled.
    #[inline]
    pub const fn last_timing(&self) -> Option<FrameTimingStats> {
        self.last_timing
    }

    fn ensure_cynthia_fbo(&mut self, needed_h: u32) -> bool {
        let h = needed_h.next_power_of_two().max(64).min(self.max_tex_size);
        if h <= self.cynthia_tex_h {
            return true;
        }
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_cynthia);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as GLint,
                512,
                h as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            (gl.glBindRenderbuffer)(GL_RENDERBUFFER, self.rbo_cynthia_depth);
            (gl.glRenderbufferStorage)(GL_RENDERBUFFER, GL_DEPTH_COMPONENT16, 512, h as GLsizei);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_cynthia);
            (gl.glFramebufferTexture2D)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                self.tex_cynthia,
                0,
            );
            (gl.glFramebufferRenderbuffer)(
                GL_FRAMEBUFFER,
                GL_DEPTH_ATTACHMENT,
                GL_RENDERBUFFER,
                self.rbo_cynthia_depth,
            );
            let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            if status != GL_FRAMEBUFFER_COMPLETE {
                return false;
            }
        }
        self.cynthia_tex_h = h;
        true
    }

    fn ensure_output_fbo(&mut self, slot: usize, w: u32, h: u32) -> bool {
        let nw = w.clamp(1, self.max_tex_size);
        let nh = h.clamp(1, self.max_tex_size);
        if self.output_tex_w[slot] == nw && self.output_tex_h[slot] == nh {
            return true;
        }
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_output[slot]);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as GLint,
                nw as GLsizei,
                nh as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_output[slot]);
            (gl.glFramebufferTexture2D)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                self.tex_output[slot],
                0,
            );
            let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            if status != GL_FRAMEBUFFER_COMPLETE {
                return false;
            }
        }
        self.output_tex_w[slot] = nw;
        self.output_tex_h[slot] = nh;
        true
    }

    fn ensure_scaled_rgba_fbo(&mut self, slot: usize, w: u32, h: u32) -> bool {
        let nw = w.clamp(1, self.max_tex_size);
        let nh = h.clamp(1, self.max_tex_size);
        if self.scaled_rgba_w[slot] == nw && self.scaled_rgba_h[slot] == nh {
            return true;
        }
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_scaled_rgba[slot]);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_RGBA as GLint,
                nw as GLsizei,
                nh as GLsizei,
                0,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_scaled_rgba[slot]);
            (gl.glFramebufferTexture2D)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                self.tex_scaled_rgba[slot],
                0,
            );
            let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            if status != GL_FRAMEBUFFER_COMPLETE {
                return false;
            }
        }
        self.scaled_rgba_w[slot] = nw;
        self.scaled_rgba_h[slot] = nh;
        true
    }

    fn ensure_scaled_565_fbo(&mut self, slot: usize, w: u32, h: u32) -> bool {
        let nw = w.clamp(1, self.max_tex_size);
        let nh = h.clamp(1, self.max_tex_size);
        if self.scaled_565_w[slot] == nw && self.scaled_565_h[slot] == nh {
            return true;
        }
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindRenderbuffer)(GL_RENDERBUFFER, self.rbo_scaled_565[slot]);
            (gl.glRenderbufferStorage)(GL_RENDERBUFFER, GL_RGB565, nw as GLsizei, nh as GLsizei);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_scaled_565[slot]);
            (gl.glFramebufferRenderbuffer)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_RENDERBUFFER,
                self.rbo_scaled_565[slot],
            );
            let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            if status != GL_FRAMEBUFFER_COMPLETE {
                return false;
            }
        }
        self.scaled_565_w[slot] = nw;
        self.scaled_565_h[slot] = nh;
        true
    }

    fn ensure_2d_tex_height(
        &self,
        tex: GLuint,
        width: u32,
        cur_h: &mut u32,
        needed_h: u32,
        format: GLenum,
    ) {
        let h = needed_h.next_power_of_two().max(16).min(self.max_tex_size);
        if h <= *cur_h {
            return;
        }
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, tex);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                format as GLint,
                width as GLsizei,
                h as GLsizei,
                0,
                format,
                GL_UNSIGNED_BYTE,
                ptr::null(),
            );
        }
        *cur_h = h;
    }

    /// Renders `bus` using full VRAM upload (`&X68kBus` readonly path).
    pub fn render_bus_frame(&mut self, bus: &X68kBus) -> RenderedFrame {
        self.ctx.make_current();
        let t0 = self.collect_timing.then(Instant::now);
        self.upload_vram_full(bus);
        self.submit_common(bus, None, false, t0);
        self.finish_frame().expect("pending readback after submit")
    }

    /// Renders `bus` using dirty 4 KB VRAM page uploads (with cursor overlay restoration).
    pub fn render_bus_frame_dirty(&mut self, bus: &mut X68kBus) -> RenderedFrame {
        self.submit_frame(bus);
        self.finish_frame().expect("pending readback after submit")
    }

    /// Renders `bus` at native resolution and GPU-downsamples to `dst_w × dst_h` (`RGBA8`).
    pub fn render_bus_frame_scaled(
        &mut self,
        bus: &mut X68kBus,
        dst_w: u32,
        dst_h: u32,
    ) -> RenderedFrame {
        self.submit_frame_scaled(bus, dst_w, dst_h);
        self.finish_frame().expect("pending readback after submit")
    }

    /// Renders `bus` into `RGB565` pixels (using direct hardware `RGB565` readback when supported).
    pub fn render_bus_frame_rgb565(&mut self, bus: &mut X68kBus) -> RenderedFrame565 {
        self.submit_frame_rgb565(bus);
        self.finish_frame_rgb565()
            .expect("pending readback after submit")
    }

    /// Renders `bus` at native resolution, GPU-downsamples to `dst_w × dst_h`, and reads back `RGB565`.
    pub fn render_bus_frame_scaled_rgb565(
        &mut self,
        bus: &mut X68kBus,
        dst_w: u32,
        dst_h: u32,
    ) -> RenderedFrame565 {
        self.submit_frame_scaled_rgb565(bus, dst_w, dst_h);
        self.finish_frame_rgb565()
            .expect("pending readback after submit")
    }

    /// Submits a native-resolution `RGBA8` frame to the GPU and flushes without blocking on readback.
    ///
    /// Call [`Self::finish_frame`] after stepping the next frame's CPU work to overlap GPU rasterization
    /// with 68000 emulation.
    pub fn submit_frame(&mut self, bus: &mut X68kBus) {
        self.ctx.make_current();
        let t0 = self.collect_timing.then(Instant::now);
        self.upload_vram_dirty(bus);
        self.submit_common(bus, None, false, t0);
    }

    /// Submits a GPU-downsampled `dst_w × dst_h` `RGBA8` frame and flushes without blocking on readback.
    pub fn submit_frame_scaled(&mut self, bus: &mut X68kBus, dst_w: u32, dst_h: u32) {
        self.ctx.make_current();
        let t0 = self.collect_timing.then(Instant::now);
        self.upload_vram_dirty(bus);
        self.submit_common(bus, Some((dst_w.max(1), dst_h.max(1))), false, t0);
    }

    /// Submits a native-resolution `RGB565` frame and flushes without blocking on readback.
    pub fn submit_frame_rgb565(&mut self, bus: &mut X68kBus) {
        self.ctx.make_current();
        let t0 = self.collect_timing.then(Instant::now);
        self.upload_vram_dirty(bus);
        self.submit_common(bus, None, true, t0);
    }

    /// Submits a GPU-downsampled `dst_w × dst_h` `RGB565` frame and flushes without blocking on readback.
    pub fn submit_frame_scaled_rgb565(&mut self, bus: &mut X68kBus, dst_w: u32, dst_h: u32) {
        self.ctx.make_current();
        let t0 = self.collect_timing.then(Instant::now);
        self.upload_vram_dirty(bus);
        self.submit_common(bus, Some((dst_w.max(1), dst_h.max(1))), true, t0);
    }

    /// Reads back the previously submitted `RGBA8` frame.
    pub fn finish_frame(&mut self) -> Option<RenderedFrame> {
        let p = self.pending.take()?;
        self.ctx.make_current();
        let t_rb = self.collect_timing.then(Instant::now);
        let gl = &self.ctx.gl;
        let mut rgba8 = vec![0u8; (p.width * p.height * 4) as usize];
        unsafe {
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, p.fbo);
            (gl.glReadPixels)(
                0,
                0,
                p.width as GLsizei,
                p.height as GLsizei,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                rgba8.as_mut_ptr() as *mut c_void,
            );
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
        }
        if let Some(t0) = t_rb {
            self.last_timing = Some(FrameTimingStats {
                upload_us: p.upload_us,
                draw_us: p.draw_us,
                readback_us: t0.elapsed().as_micros().min(u32::MAX as u128) as u32,
            });
        }
        Some(RenderedFrame {
            width: p.width,
            height: p.height,
            rgba8,
        })
    }

    /// Reads back the previously submitted `RGB565` frame (direct 16-bit readback if supported,
    /// otherwise `RGBA8` readback + CPU conversion).
    pub fn finish_frame_rgb565(&mut self) -> Option<RenderedFrame565> {
        let p = self.pending.take()?;
        self.ctx.make_current();
        let t_rb = self.collect_timing.then(Instant::now);
        let gl = &self.ctx.gl;
        let out = if p.rgb565_direct {
            let mut rgb565 = vec![0u16; (p.width * p.height) as usize];
            unsafe {
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, p.fbo);
                (gl.glReadPixels)(
                    0,
                    0,
                    p.width as GLsizei,
                    p.height as GLsizei,
                    GL_RGB,
                    GL_UNSIGNED_SHORT_5_6_5,
                    rgb565.as_mut_ptr() as *mut c_void,
                );
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            }
            RenderedFrame565 {
                width: p.width,
                height: p.height,
                rgb565,
            }
        } else {
            let mut rgba8 = vec![0u8; (p.width * p.height * 4) as usize];
            unsafe {
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, p.fbo);
                (gl.glReadPixels)(
                    0,
                    0,
                    p.width as GLsizei,
                    p.height as GLsizei,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    rgba8.as_mut_ptr() as *mut c_void,
                );
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            }
            RenderedFrame {
                width: p.width,
                height: p.height,
                rgba8,
            }
            .to_rgb565()
        };
        if let Some(t0) = t_rb {
            self.last_timing = Some(FrameTimingStats {
                upload_us: p.upload_us,
                draw_us: p.draw_us,
                readback_us: t0.elapsed().as_micros().min(u32::MAX as u128) as u32,
            });
        }
        Some(out)
    }

    pub fn render_bus_with_format(
        &mut self,
        bus: &mut X68kBus,
        format: PixelFormat,
    ) -> RenderedFrame {
        let _ = format;
        self.render_bus_frame_dirty(bus)
    }

    fn upload_cursor_overlay(
        &self,
        bus: &X68kBus,
        ext_active: bool,
        plane_bytes: usize,
    ) -> u128 {
        let gl = &self.ctx.gl;
        let mut touched = DirtyVramPages::default();
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
        }
        if !ext_active {
            let Some(tvram) = bus.tvram() else {
                return 0;
            };
            // Merge cursor overlay bytes across all 4 planes per `in_plane` offset so multi-plane
            // cursor writes at the same (row, col) combine cleanly into packed RGBA texels.
            let mut patches: Vec<(usize, [[u8; 4]; 4])> = Vec::new();
            for (offset, bytes) in bus.cursor_overlay() {
                if offset + 4 <= TVRAM_BYTES {
                    let plane = offset / TVRAM_PLANE_BYTES;
                    let in_plane = offset % TVRAM_PLANE_BYTES;
                    if let Some((_, planes)) = patches.iter_mut().find(|(k, _)| *k == in_plane) {
                        planes[plane] = bytes;
                    } else {
                        let mut planes = [[0u8; 4]; 4];
                        for (p, dst) in planes.iter_mut().enumerate() {
                            let base = p * TVRAM_PLANE_BYTES + in_plane;
                            if base + 4 <= tvram.len() {
                                dst.copy_from_slice(&tvram[base..base + 4]);
                            }
                        }
                        planes[plane] = bytes;
                        patches.push((in_plane, planes));
                    }
                    touched.mark_tvram_range(offset..offset + 4);
                }
            }
            for (in_plane, planes) in patches {
                let row = in_plane / 128;
                let col = in_plane % 128;
                let tex_x = ((row / 512) * 128 + col) as GLint;
                let tex_y = (row % 512) as GLint;
                let mut rgba = [0u8; 16];
                for i in 0..4 {
                    rgba[i * 4] = planes[0][i];
                    rgba[i * 4 + 1] = planes[1][i];
                    rgba[i * 4 + 2] = planes[2][i];
                    rgba[i * 4 + 3] = planes[3][i];
                }
                unsafe {
                    (gl.glTexSubImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        tex_x,
                        tex_y,
                        4,
                        1,
                        GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        rgba.as_ptr() as *const c_void,
                    );
                }
            }
        } else {
            let max_bytes = 4 * plane_bytes;
            for (offset, bytes) in bus.cursor_overlay() {
                if offset + 4 <= max_bytes {
                    let tex_w = self.tvram_tex_w as usize;
                    let y0 = offset / tex_w;
                    let x0 = offset % tex_w;
                    if x0 + 4 <= tex_w {
                        unsafe {
                            (gl.glTexSubImage2D)(
                                GL_TEXTURE_2D,
                                0,
                                x0 as GLint,
                                y0 as GLint,
                                4,
                                1,
                                GL_LUMINANCE,
                                GL_UNSIGNED_BYTE,
                                bytes.as_ptr() as *const c_void,
                            );
                        }
                    } else {
                        for (i, &b) in bytes.iter().enumerate() {
                            let a = offset + i;
                            unsafe {
                                (gl.glTexSubImage2D)(
                                    GL_TEXTURE_2D,
                                    0,
                                    (a % tex_w) as GLint,
                                    (a / tex_w) as GLint,
                                    1,
                                    1,
                                    GL_LUMINANCE,
                                    GL_UNSIGNED_BYTE,
                                    &b as *const u8 as *const c_void,
                                );
                            }
                        }
                    }
                }
            }
        }
        touched.tvram
    }

    fn upload_ext_tvram(&mut self, tvram: &[u8]) {
        let gl = &self.ctx.gl;
        let tex_w = self.max_tex_size.min(2048);
        let tex_h = ((tvram.len() as u32).div_ceil(tex_w))
            .next_power_of_two()
            .max(512)
            .min(self.max_tex_size);
        if !self.tvram_is_ext || self.tvram_tex_w != tex_w || self.tvram_tex_h < tex_h {
            self.tvram_is_ext = true;
            self.tvram_tex_w = tex_w;
            self.tvram_tex_h = tex_h;
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                (gl.glTexImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    GL_LUMINANCE as GLint,
                    tex_w as GLsizei,
                    tex_h as GLsizei,
                    0,
                    GL_LUMINANCE,
                    GL_UNSIGNED_BYTE,
                    ptr::null(),
                );
            }
        }
        let rows = (tvram.len() as u32).div_ceil(tex_w) as usize;
        let padded = rows * tex_w as usize;
        self.scratch_ext_tvram.clear();
        self.scratch_ext_tvram.resize(padded, 0);
        let copy_len = tvram.len().min(padded);
        self.scratch_ext_tvram[..copy_len].copy_from_slice(&tvram[..copy_len]);
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
            (gl.glTexSubImage2D)(
                GL_TEXTURE_2D,
                0,
                0,
                0,
                tex_w as GLsizei,
                rows as GLsizei,
                GL_LUMINANCE,
                GL_UNSIGNED_BYTE,
                self.scratch_ext_tvram.as_ptr() as *const c_void,
            );
        }
    }

    fn ensure_standard_tvram_tex(&mut self) {
        if self.tvram_is_ext || self.tvram_tex_w != 256 || self.tvram_tex_h != 512 {
            self.tvram_is_ext = false;
            self.tvram_tex_w = 256;
            self.tvram_tex_h = 512;
            let gl = &self.ctx.gl;
            let zero = vec![0u8; TVRAM_BYTES];
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                (gl.glTexImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA as GLint,
                    256,
                    512,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    zero.as_ptr() as *const c_void,
                );
            }
        }
    }

    fn upload_standard_tvram_pages(&mut self, tvram: &[u8], pages: u128) {
        if tvram.len() < TVRAM_BYTES {
            return;
        }
        // Collapse 4-plane dirty page bits into 32 vertical bands (each 32 rows × 128 cols).
        let mut band_mask =
            ((pages | (pages >> 32) | (pages >> 64) | (pages >> 96)) & 0xFFFF_FFFF) as u32;
        if band_mask == 0 {
            return;
        }
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
        }
        let p0_all = &tvram[0..TVRAM_PLANE_BYTES];
        let p1_all = &tvram[TVRAM_PLANE_BYTES..2 * TVRAM_PLANE_BYTES];
        let p2_all = &tvram[2 * TVRAM_PLANE_BYTES..3 * TVRAM_PLANE_BYTES];
        let p3_all = &tvram[3 * TVRAM_PLANE_BYTES..4 * TVRAM_PLANE_BYTES];

        while band_mask != 0 {
            let band = band_mask.trailing_zeros() as usize;
            band_mask &= band_mask - 1;
            let off = band * 4096;
            let s0 = &p0_all[off..off + 4096];
            let s1 = &p1_all[off..off + 4096];
            let s2 = &p2_all[off..off + 4096];
            let s3 = &p3_all[off..off + 4096];
            let dst = &mut self.scratch_tvram_band[..4096 * 4];
            for i in 0..4096 {
                let d = i * 4;
                dst[d] = s0[i];
                dst[d + 1] = s1[i];
                dst[d + 2] = s2[i];
                dst[d + 3] = s3[i];
            }
            let half = band / 16;
            let tex_x = (half * 128) as GLint;
            let tex_y = ((band % 16) * 32) as GLint;
            unsafe {
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    tex_x,
                    tex_y,
                    128,
                    32,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    dst.as_ptr() as *const c_void,
                );
            }
        }
    }

    fn upload_gvram_pages(&self, gvram: &[u8], pages: u128) {
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_gvram);
        }
        let mut mask = pages;
        while mask != 0 {
            let start = mask.trailing_zeros() as usize;
            let run = (mask >> start).trailing_ones() as usize;
            let end = start + run;
            mask = if end >= 128 {
                0
            } else {
                mask & !((1u128 << end) - 1)
            };
            let off = start * 4096;
            let byte_len = run * 4096;
            if off + byte_len > gvram.len() {
                continue;
            }
            unsafe {
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    (start * 4) as GLint,
                    512,
                    (run * 4) as GLsizei,
                    GL_LUMINANCE_ALPHA,
                    GL_UNSIGNED_BYTE,
                    gvram[off..].as_ptr() as *const c_void,
                );
            }
        }
    }

    fn upload_vram_full(&mut self, bus: &X68kBus) {
        let row_bytes = bus.active_tvram_row_bytes();
        let plane_bytes = bus.active_tvram_plane_bytes();
        let is_ext = !(row_bytes == 128 && plane_bytes == TVRAM_PLANE_BYTES);
        if is_ext {
            if let Some(tvram) = bus.active_tvram() {
                self.upload_ext_tvram(tvram);
            }
            self.ext_tvram_was_active = true;
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, true, plane_bytes);
        } else {
            self.ensure_standard_tvram_tex();
            if let Some(tvram) = bus.tvram() {
                self.upload_standard_tvram_pages(tvram, u128::MAX);
            } else {
                let gl = &self.ctx.gl;
                let zero = vec![0u8; TVRAM_BYTES];
                unsafe {
                    (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                    (gl.glTexSubImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        0,
                        0,
                        256,
                        512,
                        GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        zero.as_ptr() as *const c_void,
                    );
                }
            }
            self.ext_tvram_was_active = false;
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, false, plane_bytes);
        }

        if let Some(gvram) = bus.gvram() {
            self.upload_gvram_pages(gvram, u128::MAX);
        } else {
            let gl = &self.ctx.gl;
            let zero = vec![0u8; GVRAM_BYTES];
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_gvram);
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    512,
                    512,
                    GL_LUMINANCE_ALPHA,
                    GL_UNSIGNED_BYTE,
                    zero.as_ptr() as *const c_void,
                );
            }
        }
        self.vram_dirty_seeded = false;
    }

    fn upload_vram_dirty(&mut self, bus: &mut X68kBus) {
        let mut written = bus.take_written_vram_pages();
        if !self.vram_dirty_seeded {
            written = DirtyVramPages::ALL;
            self.vram_dirty_seeded = true;
        }
        let row_bytes = bus.active_tvram_row_bytes();
        let plane_bytes = bus.active_tvram_plane_bytes();
        let is_ext = !(row_bytes == 128 && plane_bytes == TVRAM_PLANE_BYTES);
        if is_ext {
            if let Some(tvram) = bus.active_tvram() {
                self.upload_ext_tvram(tvram);
            }
            self.ext_tvram_was_active = true;
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, true, plane_bytes);
        } else {
            if self.ext_tvram_was_active {
                self.ensure_standard_tvram_tex();
                written.tvram = u128::MAX;
                self.ext_tvram_was_active = false;
            }
            let pages = written.tvram | self.prev_cursor_pages;
            if pages != 0 {
                if let Some(tvram) = bus.tvram() {
                    self.upload_standard_tvram_pages(tvram, pages);
                }
            }
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, false, plane_bytes);
        }

        if written.gvram != 0 {
            if let Some(gvram) = bus.gvram() {
                self.upload_gvram_pages(gvram, written.gvram);
            }
        }
    }

    fn submit_common(
        &mut self,
        bus: &X68kBus,
        scaled: Option<(u32, u32)>,
        want_rgb565: bool,
        upload_start: Option<Instant>,
    ) {
        let mut uniforms = ViconUniforms::from_bus(bus);
        uniforms.width = uniforms.width.min(self.max_width);
        uniforms.height = uniforms.height.min(self.max_height);

        let completed = raster::completed_frame(bus, &uniforms);
        let cynthia = raster::cynthia_input(bus, &uniforms);
        let cynthia_enabled = !cynthia.is_empty();
        let is_ext_tvram = !(uniforms.tvram_row_bytes == 128
            && uniforms.tvram_plane_bytes == TVRAM_PLANE_BYTES as u32);

        // Determine if any row uses 65536-color byte-split palette mode
        let any_row_65536 = if let Some(frame) = completed {
            frame.lines.iter().any(|l| {
                let m = l.state.registers.mode & 7;
                m == 2 || m == 3
            })
        } else {
            let m = uniforms.r0_mode & 7;
            m == 2 || m == 3
        };

        // Pass 1 needs per-row `u_row_state` whenever CYNTHIA is enabled, but Pass 2 can still
        // use the zero-row-fetch uniform fast shader if VICON registers + cynthia_active are constant!
        let mut cynthia_active_varies = false;
        let first_cyn_active = if cynthia_enabled {
            cynthia
                .bg_rows
                .first()
                .is_some_and(|r| r.is_cynthia_active())
        } else {
            false
        };
        if cynthia_enabled {
            for y in 1..uniforms.height as usize {
                let act = cynthia
                    .bg_rows
                    .get(y)
                    .is_some_and(|r| r.is_cynthia_active());
                if act != first_cyn_active {
                    cynthia_active_varies = true;
                    break;
                }
            }
        }

        let vicon_has_row_map =
            completed.is_some_and(|f| f.lines.len() > 1) || cynthia_active_varies;
        let need_row_state_upload = vicon_has_row_map || cynthia_enabled || any_row_65536 || is_ext_tvram;
        let row_count = if vicon_has_row_map || cynthia_enabled {
            uniforms.height as usize
        } else {
            1
        };

        // Effective uniform row state (for fast uniform Pass 2 path)
        let mut uniform_row = uniforms;
        let mut uniform_pal_id = 0u32;
        if let Some(frame) = completed {
            if let Some(first) = frame.lines.first() {
                uniform_row.apply_raster(&first.state);
                uniform_pal_id = first.state.palette_id;
            }
        }

        // Build row state and palette textures
        let mut baked_pal_count = 0u32;
        let mut raw_pal_count = 0u32;
        let mut uniform_baked_pal_id = 0u32;

        if !any_row_65536 {
            // Pre-bake `(palette_id, contrast) -> RGBA8` palette rows on CPU!
            self.scratch_palette_rgba.clear();
            let mut baked_keys: Vec<(u32, u8)> = Vec::new();
            let mut get_or_bake = |pal_id: u32,
                                   contrast: u8,
                                   pal_slice: &[u16; 512],
                                   out: &mut Vec<u8>|
             -> u32 {
                if pal_id == u32::MAX {
                    return u32::MAX;
                }
                if let Some(pos) = baked_keys
                    .iter()
                    .position(|&(pid, c)| pid == pal_id && c == contrast)
                {
                    return pos as u32;
                }
                let id = baked_keys.len() as u32;
                baked_keys.push((pal_id, contrast));
                push_palette_baked_rgba(out, pal_slice, contrast);
                id
            };

            if let Some(frame) = completed {
                if need_row_state_upload {
                    self.scratch_row_state.clear();
                    self.scratch_row_state.resize(row_count * 64, 0);
                    let mut next_line = 0usize;
                    let mut cur_state = &frame.lines[0].state;
                    for y in 0..row_count {
                        while let Some(line) =
                            frame.lines.get(next_line).filter(|l| l.y as usize <= y)
                        {
                            cur_state = &line.state;
                            next_line += 1;
                        }
                        let mut u = uniforms;
                        u.apply_raster(cur_state);
                        let raw_pid = cur_state.palette_id;
                        let baked_id = if raw_pid == u32::MAX {
                            u32::MAX
                        } else {
                            let pal = frame.palette(raw_pid);
                            get_or_bake(
                                raw_pid,
                                (u.contrast & 15) as u8,
                                pal,
                                &mut self.scratch_palette_rgba,
                            )
                        };
                        if y == 0 {
                            uniform_baked_pal_id = baked_id;
                        }
                        let bg_row = cynthia.bg_rows.get(y).copied().unwrap_or_default();
                        pack_row_state_entry(
                            &mut self.scratch_row_state[y * 64..(y + 1) * 64],
                            &u,
                            baked_id,
                            bg_row,
                        );
                    }
                } else {
                    uniform_baked_pal_id = if uniform_pal_id == u32::MAX {
                        u32::MAX
                    } else {
                        let pal = frame.palette(uniform_pal_id);
                        get_or_bake(
                            uniform_pal_id,
                            (uniform_row.contrast & 15) as u8,
                            pal,
                            &mut self.scratch_palette_rgba,
                        )
                    };
                }
            } else {
                let mut pal = [0u16; 512];
                pal[..256].copy_from_slice(bus.vicon.text_palette());
                pal[256..].copy_from_slice(bus.vicon.graphic_palette());
                uniform_baked_pal_id = get_or_bake(
                    0,
                    (uniforms.contrast & 15) as u8,
                    &pal,
                    &mut self.scratch_palette_rgba,
                );
                if need_row_state_upload {
                    self.scratch_row_state.clear();
                    self.scratch_row_state.resize(row_count * 64, 0);
                    for y in 0..row_count {
                        let bg_row = cynthia.bg_rows.get(y).copied().unwrap_or_default();
                        pack_row_state_entry(
                            &mut self.scratch_row_state[y * 64..(y + 1) * 64],
                            &uniforms,
                            uniform_baked_pal_id,
                            bg_row,
                        );
                    }
                }
            }

            baked_pal_count = baked_keys.len().max(1) as u32;
            if self.scratch_palette_rgba.is_empty() {
                self.scratch_palette_rgba.resize(512 * 4, 0);
            }
        } else {
            // 65536-color mode: raw GRBI16 LUMINANCE_ALPHA palette + row state
            self.scratch_row_state.clear();
            self.scratch_row_state.resize(row_count * 64, 0);
            self.scratch_palette_raw.clear();
            if let Some(frame) = completed {
                let mut next_line = 0usize;
                let mut cur_state = &frame.lines[0].state;
                for y in 0..row_count {
                    while let Some(line) = frame.lines.get(next_line).filter(|l| l.y as usize <= y)
                    {
                        cur_state = &line.state;
                        next_line += 1;
                    }
                    let mut u = uniforms;
                    u.apply_raster(cur_state);
                    let bg_row = cynthia.bg_rows.get(y).copied().unwrap_or_default();
                    pack_row_state_entry(
                        &mut self.scratch_row_state[y * 64..(y + 1) * 64],
                        &u,
                        cur_state.palette_id,
                        bg_row,
                    );
                }
                raw_pal_count = frame.palettes.len().max(1) as u32;
                self.scratch_palette_raw
                    .reserve(raw_pal_count as usize * 1024);
                if frame.palettes.is_empty() {
                    self.scratch_palette_raw.resize(1024, 0);
                } else {
                    for pal in &frame.palettes {
                        push_palette_la(&mut self.scratch_palette_raw, pal);
                    }
                }
            } else {
                let mut pal = [0u16; 512];
                pal[..256].copy_from_slice(bus.vicon.text_palette());
                pal[256..].copy_from_slice(bus.vicon.graphic_palette());
                push_palette_la(&mut self.scratch_palette_raw, &pal);
                raw_pal_count = 1;
                for y in 0..row_count {
                    let bg_row = cynthia.bg_rows.get(y).copied().unwrap_or_default();
                    pack_row_state_entry(
                        &mut self.scratch_row_state[y * 64..(y + 1) * 64],
                        &uniforms,
                        0,
                        bg_row,
                    );
                }
            }
        }

        // Upload row_state and palette textures
        if need_row_state_upload {
            let mut row_state_h = self.row_state_tex_h;
            self.ensure_2d_tex_height(
                self.tex_row_state,
                16,
                &mut row_state_h,
                row_count as u32,
                GL_RGBA,
            );
            self.row_state_tex_h = row_state_h;
            let gl = &self.ctx.gl;
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_row_state);
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    16,
                    row_count as GLsizei,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    self.scratch_row_state.as_ptr() as *const c_void,
                );
            }
        }

        if !any_row_65536 {
            let mut pal_h = self.palette_rgba_tex_h;
            self.ensure_2d_tex_height(
                self.tex_palette_rgba,
                512,
                &mut pal_h,
                baked_pal_count,
                GL_RGBA,
            );
            self.palette_rgba_tex_h = pal_h;
            let gl = &self.ctx.gl;
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette_rgba);
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    512,
                    baked_pal_count as GLsizei,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    self.scratch_palette_rgba.as_ptr() as *const c_void,
                );
            }
        } else {
            let mut pal_h = self.palette_raw_tex_h;
            self.ensure_2d_tex_height(
                self.tex_palette_raw,
                512,
                &mut pal_h,
                raw_pal_count,
                GL_LUMINANCE_ALPHA,
            );
            self.palette_raw_tex_h = pal_h;
            let gl = &self.ctx.gl;
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette_raw);
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    512,
                    raw_pal_count as GLsizei,
                    GL_LUMINANCE_ALPHA,
                    GL_UNSIGNED_BYTE,
                    self.scratch_palette_raw.as_ptr() as *const c_void,
                );
            }
        }

        // Upload Pass 1 PCG textures before timing boundary if CYNTHIA active
        if cynthia_enabled {
            self.upload_cynthia_textures(&cynthia);
        }

        let upload_us = upload_start
            .map(|t| t.elapsed().as_micros().min(u32::MAX as u128) as u32)
            .unwrap_or(0);
        let draw_start = self.collect_timing.then(Instant::now);

        // Pass 1: CYNTHIA (skipped when empty; scissor-clipped to active viewport width)
        if cynthia_enabled {
            self.draw_cynthia_pass(&cynthia, uniforms.width.min(512));
        }

        // Determine target FBOs for Pass 2 (native) and optional Pass 3 (downsample)
        let slot = self.cur_slot;
        self.cur_slot = 1 - self.cur_slot;

        let (dst_w, dst_h) = match scaled {
            Some((dw, dh)) if dw != uniforms.width || dh != uniforms.height => {
                (dw.min(self.max_tex_size), dh.min(self.max_tex_size))
            }
            _ => (uniforms.width, uniforms.height),
        };
        let need_pass3 = dst_w != uniforms.width || dst_h != uniforms.height;
        let use_direct_565 = want_rgb565 && self.rgb565_readback_supported;

        // If native RGB565 is requested and direct RGB565 FBO is supported, Pass 2 draws directly
        // into `fbo_scaled_565[slot]` (16bpp tile writes, no intermediate RGBA8 FBO needed).
        let (pass2_fbo, pass2_is_565_direct) = if !need_pass3 && use_direct_565 {
            if self.ensure_scaled_565_fbo(slot, uniforms.width, uniforms.height) {
                (self.fbo_scaled_565[slot], true)
            } else {
                self.ensure_output_fbo(slot, uniforms.width, uniforms.height);
                (self.fbo_output[slot], false)
            }
        } else {
            self.ensure_output_fbo(slot, uniforms.width, uniforms.height);
            (self.fbo_output[slot], false)
        };

        // Pass 2: VICON composite at native resolution
        let use_fast_uniform = !any_row_65536 && !vicon_has_row_map && !is_ext_tvram;
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, pass2_fbo);
            (gl.glViewport)(0, 0, uniforms.width as GLsizei, uniforms.height as GLsizei);
            (gl.glDisable)(GL_DEPTH_TEST);
            (gl.glDepthMask)(GL_FALSE);

            if use_fast_uniform
                && ((uniform_row.r2_display & 127) == 0 || uniform_baked_pal_id == u32::MAX)
            {
                // Fully blank uniform frame: clear directly without invoking VICON shader
                (gl.glClearColor)(0.0, 0.0, 0.0, 1.0);
                (gl.glClear)(GL_COLOR_BUFFER_BIT);
            } else if use_fast_uniform {
                (gl.glUseProgram)(self.prog_vicon_fast_u);

                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                (gl.glUniform1i)(self.vfu_u_tvram, 0);

                (gl.glActiveTexture)(GL_TEXTURE0 + 1);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_gvram);
                (gl.glUniform1i)(self.vfu_u_gvram, 1);

                (gl.glActiveTexture)(GL_TEXTURE0 + 2);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_cynthia);
                (gl.glUniform1i)(self.vfu_u_cynthia, 2);

                (gl.glActiveTexture)(GL_TEXTURE0 + 3);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette_rgba);
                (gl.glUniform1i)(self.vfu_u_palette, 3);

                let gr_rank = ((uniform_row.r1_priority >> 8) & 3) as f32;
                let tx_rank = ((uniform_row.r1_priority >> 10) & 3) as f32;
                let sp_rank = ((uniform_row.r1_priority >> 12) & 3) as f32;
                let cyn_act = if cynthia_enabled
                    && first_cyn_active
                    && (uniform_row.r2_display & 64 != 0)
                    && gr_rank < 2.5
                {
                    1.0
                } else {
                    0.0
                };

                (gl.glUniform1f)(
                    self.vfu_u_cynthia_inv_h,
                    1.0 / (self.cynthia_tex_h.max(1) as f32),
                );
                (gl.glUniform1f)(
                    self.vfu_u_pal_v,
                    (uniform_baked_pal_id as f32 + 0.5) / (self.palette_rgba_tex_h as f32),
                );
                (gl.glUniform1f)(self.vfu_u_r0_mode, (uniform_row.r0_mode & 7) as f32);
                (gl.glUniform1f)(self.vfu_u_r2_display, (uniform_row.r2_display & 127) as f32);
                (gl.glUniform4f)(self.vfu_u_ranks, gr_rank, tx_rank, sp_rank, cyn_act);
                (gl.glUniform4f)(
                    self.vfu_u_slots,
                    (uniform_row.r1_priority & 3) as f32,
                    ((uniform_row.r1_priority >> 2) & 3) as f32,
                    ((uniform_row.r1_priority >> 4) & 3) as f32,
                    ((uniform_row.r1_priority >> 6) & 3) as f32,
                );
                (gl.glUniform2f)(
                    self.vfu_u_tx_scroll,
                    (uniform_row.text_scroll_x & 1023) as f32,
                    (uniform_row.text_scroll_y & 1023) as f32,
                );
                let gmask = if (uniform_row.r0_mode & 7) >= 4 {
                    1023
                } else {
                    511
                };
                (gl.glUniform4f)(
                    self.vfu_u_gr_scroll01,
                    (uniform_row.gr_scroll_x[0] & gmask) as f32,
                    (uniform_row.gr_scroll_y[0] & gmask) as f32,
                    (uniform_row.gr_scroll_x[1] & 511) as f32,
                    (uniform_row.gr_scroll_y[1] & 511) as f32,
                );
                (gl.glUniform4f)(
                    self.vfu_u_gr_scroll23,
                    (uniform_row.gr_scroll_x[2] & 511) as f32,
                    (uniform_row.gr_scroll_y[2] & 511) as f32,
                    (uniform_row.gr_scroll_x[3] & 511) as f32,
                    (uniform_row.gr_scroll_y[3] & 511) as f32,
                );

                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_quad);
                (gl.glEnableVertexAttribArray)(0);
                (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 8, ptr::null());
                (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
            } else if !any_row_65536 {
                (gl.glUseProgram)(self.prog_vicon_fast_r);

                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                (gl.glUniform1i)(self.vfr_u_tvram, 0);

                (gl.glActiveTexture)(GL_TEXTURE0 + 1);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_gvram);
                (gl.glUniform1i)(self.vfr_u_gvram, 1);

                (gl.glActiveTexture)(GL_TEXTURE0 + 2);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_cynthia);
                (gl.glUniform1i)(self.vfr_u_cynthia, 2);

                (gl.glActiveTexture)(GL_TEXTURE0 + 3);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_row_state);
                (gl.glUniform1i)(self.vfr_u_row_state, 3);

                (gl.glActiveTexture)(GL_TEXTURE0 + 4);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette_rgba);
                (gl.glUniform1i)(self.vfr_u_palette, 4);

                (gl.glUniform1f)(self.vfr_u_row_inv_h, 1.0 / (self.row_state_tex_h as f32));
                (gl.glUniform1f)(
                    self.vfr_u_pal_inv_h,
                    1.0 / (self.palette_rgba_tex_h as f32),
                );
                (gl.glUniform1f)(
                    self.vfr_u_cynthia_inv_h,
                    1.0 / (self.cynthia_tex_h.max(1) as f32),
                );
                (gl.glUniform1f)(
                    self.vfr_u_has_row_map,
                    if vicon_has_row_map { 1.0 } else { 0.0 },
                );
                (gl.glUniform1f)(
                    self.vfr_u_cynthia_enabled,
                    if cynthia_enabled { 1.0 } else { 0.0 },
                );
                (gl.glUniform1f)(
                    self.vfr_u_is_ext_tvram,
                    if is_ext_tvram { 1.0 } else { 0.0 },
                );
                (gl.glUniform4f)(
                    self.vfr_u_ext_tvram,
                    uniforms.tvram_row_bytes as f32,
                    uniforms.tvram_plane_bytes as f32,
                    self.tvram_tex_w as f32,
                    1.0 / (self.tvram_tex_h as f32),
                );

                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_quad);
                (gl.glEnableVertexAttribArray)(0);
                (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 8, ptr::null());
                (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
            } else {
                (gl.glUseProgram)(self.prog_vicon_raw);

                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                (gl.glUniform1i)(self.vr_u_tvram, 0);

                (gl.glActiveTexture)(GL_TEXTURE0 + 1);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_gvram);
                (gl.glUniform1i)(self.vr_u_gvram, 1);

                (gl.glActiveTexture)(GL_TEXTURE0 + 2);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_cynthia);
                (gl.glUniform1i)(self.vr_u_cynthia, 2);

                (gl.glActiveTexture)(GL_TEXTURE0 + 3);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_row_state);
                (gl.glUniform1i)(self.vr_u_row_state, 3);

                (gl.glActiveTexture)(GL_TEXTURE0 + 4);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette_raw);
                (gl.glUniform1i)(self.vr_u_palette, 4);

                (gl.glActiveTexture)(GL_TEXTURE0 + 5);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_lut);
                (gl.glUniform1i)(self.vr_u_lut, 5);

                (gl.glUniform1f)(self.vr_u_row_inv_h, 1.0 / (self.row_state_tex_h as f32));
                (gl.glUniform1f)(
                    self.vr_u_pal_inv_h,
                    1.0 / (self.palette_raw_tex_h as f32),
                );
                (gl.glUniform1f)(
                    self.vr_u_cynthia_inv_h,
                    1.0 / (self.cynthia_tex_h.max(1) as f32),
                );
                (gl.glUniform1f)(
                    self.vr_u_has_row_map,
                    if vicon_has_row_map { 1.0 } else { 0.0 },
                );
                (gl.glUniform1f)(
                    self.vr_u_cynthia_enabled,
                    if cynthia_enabled { 1.0 } else { 0.0 },
                );
                (gl.glUniform1f)(
                    self.vr_u_is_ext_tvram,
                    if is_ext_tvram { 1.0 } else { 0.0 },
                );
                (gl.glUniform4f)(
                    self.vr_u_ext_tvram,
                    uniforms.tvram_row_bytes as f32,
                    uniforms.tvram_plane_bytes as f32,
                    self.tvram_tex_w as f32,
                    1.0 / (self.tvram_tex_h as f32),
                );

                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_quad);
                (gl.glEnableVertexAttribArray)(0);
                (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 8, ptr::null());
                (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
            }
        }

        // Pass 3: Optional GPU downsample blit to embedder target size
        let (final_fbo, final_565_direct) = if need_pass3 {
            let (dst_fbo, is_565) = if use_direct_565
                && self.ensure_scaled_565_fbo(slot, dst_w, dst_h)
            {
                (self.fbo_scaled_565[slot], true)
            } else {
                self.ensure_scaled_rgba_fbo(slot, dst_w, dst_h);
                (self.fbo_scaled_rgba[slot], false)
            };
            let gl = &self.ctx.gl;
            let ow = self.output_tex_w[slot] as f32;
            let oh = self.output_tex_h[slot] as f32;
            unsafe {
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, dst_fbo);
                (gl.glViewport)(0, 0, dst_w as GLsizei, dst_h as GLsizei);
                (gl.glUseProgram)(self.prog_blit);
                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_output[slot]);
                (gl.glUniform1i)(self.bl_u_src, 0);
                (gl.glUniform2f)(
                    self.bl_u_uv_scale,
                    (uniforms.width as f32) / ow,
                    (uniforms.height as f32) / oh,
                );
                (gl.glUniform2f)(
                    self.bl_u_uv_max,
                    ((uniforms.width as f32) - 0.5) / ow,
                    ((uniforms.height as f32) - 0.5) / oh,
                );
                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_quad);
                (gl.glEnableVertexAttribArray)(0);
                (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 8, ptr::null());
                (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
            }
            (dst_fbo, is_565)
        } else {
            (pass2_fbo, pass2_is_565_direct)
        };

        let gl = &self.ctx.gl;
        let draw_us = if let Some(t0) = draw_start {
            unsafe {
                (gl.glFinish)();
            }
            t0.elapsed().as_micros().min(u32::MAX as u128) as u32
        } else {
            unsafe {
                (gl.glFlush)();
            }
            0
        };

        self.pending = Some(PendingReadback {
            fbo: final_fbo,
            width: dst_w,
            height: dst_h,
            rgb565_direct: final_565_direct,
            upload_us,
            draw_us,
        });
    }

    fn upload_cynthia_textures(&mut self, cynthia: &CynthiaFrame) {
        let cyn_h = cynthia.height.max(1);
        self.ensure_cynthia_fbo(cyn_h);

        // Pack u_pcg (1024 × rows LUMINANCE, 8 blocks of 128B per row)
        let pcg_rows = cynthia.pcg_blocks.len().div_ceil(8).max(1);
        let mut pcg_h = self.pcg_tex_h;
        self.ensure_2d_tex_height(self.tex_pcg, 1024, &mut pcg_h, pcg_rows as u32, GL_LUMINANCE);
        self.pcg_tex_h = pcg_h;

        self.scratch_pcg.clear();
        self.scratch_pcg.resize(pcg_rows * 1024, 0);
        for (i, blk) in cynthia.pcg_blocks.iter().enumerate() {
            let row = i / 8;
            let col = (i % 8) * 128;
            let dst = row * 1024 + col;
            self.scratch_pcg[dst..dst + 128].copy_from_slice(blk);
        }

        // Pack u_pcg_table (512 × num_tables LUMINANCE_ALPHA)
        let stride = cynthia.pcg_table_stride.max(256) as usize;
        let num_tables = (cynthia.pcg_tables.len() / stride).max(1);
        let mut tbl_h = self.pcg_tbl_tex_h;
        self.ensure_2d_tex_height(
            self.tex_pcg_table,
            512,
            &mut tbl_h,
            num_tables as u32,
            GL_LUMINANCE_ALPHA,
        );
        self.pcg_tbl_tex_h = tbl_h;

        self.scratch_pcg_tbl.clear();
        self.scratch_pcg_tbl.resize(num_tables * 512 * 2, 0);
        for t in 0..num_tables {
            let src = &cynthia.pcg_tables[t * stride..(t + 1) * stride];
            let dst = &mut self.scratch_pcg_tbl[t * 1024..t * 1024 + stride * 2];
            for (i, &phys) in src.iter().enumerate() {
                let blk_col = (phys & 7) as u8;
                let blk_row = phys >> 3;
                dst[i * 2] = blk_col | (((blk_row >> 8) as u8 & 1) << 3);
                dst[i * 2 + 1] = (blk_row & 0xFF) as u8;
            }
        }

        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_pcg);
            (gl.glTexSubImage2D)(
                GL_TEXTURE_2D,
                0,
                0,
                0,
                1024,
                pcg_rows as GLsizei,
                GL_LUMINANCE,
                GL_UNSIGNED_BYTE,
                self.scratch_pcg.as_ptr() as *const c_void,
            );
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_pcg_table);
            (gl.glTexSubImage2D)(
                GL_TEXTURE_2D,
                0,
                0,
                0,
                512,
                num_tables as GLsizei,
                GL_LUMINANCE_ALPHA,
                GL_UNSIGNED_BYTE,
                self.scratch_pcg_tbl.as_ptr() as *const c_void,
            );
        }
    }

    fn draw_cynthia_pass(&mut self, cynthia: &CynthiaFrame, active_w: u32) {
        let cyn_h = cynthia.height.max(1);
        let active_w = active_w.clamp(1, 512);

        // Scan BG rows to skip completely disabled BG0/BG1 fullscreen draws
        let mut any_bg0 = false;
        let mut any_bg1 = false;
        for row in cynthia.bg_rows.iter().take(cyn_h as usize) {
            if row.is_cynthia_active() && row.bg_valid() {
                if row.bg0_en() {
                    any_bg0 = true;
                }
                if row.bg1_en() && !row.large_tiles() {
                    any_bg1 = true;
                }
                if any_bg0 && any_bg1 {
                    break;
                }
            }
        }

        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_cynthia);
            (gl.glViewport)(0, 0, 512, cyn_h as GLsizei);
            (gl.glEnable)(GL_SCISSOR_TEST);
            (gl.glScissor)(0, 0, active_w as GLsizei, cyn_h as GLsizei);
            (gl.glEnable)(GL_DEPTH_TEST);
            (gl.glDepthFunc)(GL_LESS);
            (gl.glDepthMask)(1);
            (gl.glClearColor)(0.0, 0.0, 0.0, 0.0);
            (gl.glClearDepthf)(1.0);
            (gl.glClear)(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        }

        // Pass 1A: Sprite span quads (16×h)
        if !cynthia.spans.is_empty() {
            unsafe {
                (gl.glUseProgram)(self.prog_sprite);
                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_pcg);
                (gl.glUniform1i)(self.sp_u_pcg, 0);

                (gl.glUniform2f)(self.sp_u_inv_half_size, 2.0 / 512.0, 2.0 / (cyn_h as f32));
                (gl.glUniform1f)(self.sp_u_pcg_inv_h, 1.0 / (self.pcg_tex_h as f32));
                (gl.glBindBuffer)(GL_ELEMENT_ARRAY_BUFFER, self.ibo_sprites);
                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_sprites);
            }

            let active_w_i32 = active_w as i32;
            for chunk in cynthia.spans.chunks(MAX_BATCH_SPANS) {
                self.scratch_verts.clear();
                self.scratch_verts
                    .reserve(chunk.len() * VERTS_PER_SPAN * FLOATS_PER_VERT);
                let mut quad_count = 0usize;
                for &sp in chunk {
                    let x0_i = sp.x0();
                    if x0_i >= active_w_i32 || x0_i + 16 <= 0 {
                        continue;
                    }
                    let x0 = x0_i as f32;
                    let x1 = x0 + 16.0;
                    let y0 = sp.y0() as f32;
                    let y1 = sp.y1() as f32;
                    let depth = (200.0 - (sp.prio_order() as f32)) / 220.0;
                    let (px0, px1) = if sp.hflip() { (16.0, 0.0) } else { (0.0, 16.0) };
                    let raw0 = sp.py0() as f32;
                    let raw1 = raw0 + (y1 - y0);
                    let (py0, py1) = if sp.vflip() {
                        (16.0 - raw0, 16.0 - raw1)
                    } else {
                        (raw0, raw1)
                    };
                    let pal_blk = sp.pal_block() as f32;
                    let absent = if sp.absent_bank() { 1.0 } else { 0.0 };
                    let blk_col = (sp.pcg_block_id() & 7) as f32;
                    let blk_row = (sp.pcg_block_id() >> 3) as f32;

                    let push_v = |buf: &mut Vec<f32>, x: f32, y: f32, px: f32, py: f32| {
                        buf.extend_from_slice(&[
                            x, y, depth, px, py, pal_blk, absent, blk_col, blk_row,
                        ]);
                    };
                    push_v(&mut self.scratch_verts, x0, y0, px0, py0);
                    push_v(&mut self.scratch_verts, x1, y0, px1, py0);
                    push_v(&mut self.scratch_verts, x0, y1, px0, py1);
                    push_v(&mut self.scratch_verts, x1, y1, px1, py1);
                    quad_count += 1;
                }

                if quad_count == 0 {
                    continue;
                }
                let byte_len = self.scratch_verts.len() * 4;
                unsafe {
                    if byte_len > self.vbo_sprites_cap_bytes {
                        let new_cap = byte_len.next_power_of_two();
                        (gl.glBufferData)(
                            GL_ARRAY_BUFFER,
                            new_cap as GLsizeiptr,
                            ptr::null(),
                            GL_DYNAMIC_DRAW,
                        );
                        self.vbo_sprites_cap_bytes = new_cap;
                    }
                    (gl.glBufferSubData)(
                        GL_ARRAY_BUFFER,
                        0,
                        byte_len as GLsizeiptr,
                        self.scratch_verts.as_ptr() as *const c_void,
                    );
                    let stride_b = (FLOATS_PER_VERT * 4) as GLsizei;
                    (gl.glEnableVertexAttribArray)(0);
                    (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, stride_b, ptr::null());
                    (gl.glEnableVertexAttribArray)(1);
                    (gl.glVertexAttribPointer)(
                        1,
                        1,
                        GL_FLOAT,
                        GL_FALSE,
                        stride_b,
                        8 as *const c_void,
                    );
                    (gl.glEnableVertexAttribArray)(2);
                    (gl.glVertexAttribPointer)(
                        2,
                        2,
                        GL_FLOAT,
                        GL_FALSE,
                        stride_b,
                        12 as *const c_void,
                    );
                    (gl.glEnableVertexAttribArray)(3);
                    (gl.glVertexAttribPointer)(
                        3,
                        4,
                        GL_FLOAT,
                        GL_FALSE,
                        stride_b,
                        20 as *const c_void,
                    );

                    (gl.glDrawElements)(
                        GL_TRIANGLES,
                        (quad_count * 6) as GLsizei,
                        GL_UNSIGNED_SHORT,
                        ptr::null(),
                    );
                }
            }
            unsafe {
                (gl.glDisableVertexAttribArray)(1);
                (gl.glDisableVertexAttribArray)(2);
                (gl.glDisableVertexAttribArray)(3);
                (gl.glBindBuffer)(GL_ELEMENT_ARRAY_BUFFER, 0);
            }
        }

        // Pass 1B: BG0 (order 258) and BG1 (order 129) — only draw enabled planes
        if any_bg0 || any_bg1 {
            unsafe {
                (gl.glUseProgram)(self.prog_bg);

                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_pcg);
                (gl.glUniform1i)(self.bg_u_pcg, 0);

                (gl.glActiveTexture)(GL_TEXTURE0 + 1);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_pcg_table);
                (gl.glUniform1i)(self.bg_u_pcg_table, 1);

                (gl.glActiveTexture)(GL_TEXTURE0 + 2);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_row_state);
                (gl.glUniform1i)(self.bg_u_row_state, 2);

                (gl.glUniform1f)(self.bg_u_row_inv_h, 1.0 / (self.row_state_tex_h as f32));
                (gl.glUniform1f)(self.bg_u_tbl_inv_h, 1.0 / (self.pcg_tbl_tex_h as f32));
                (gl.glUniform1f)(self.bg_u_pcg_inv_h, 1.0 / (self.pcg_tex_h as f32));

                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_quad);
                (gl.glEnableVertexAttribArray)(0);
                (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 8, ptr::null());

                if any_bg0 {
                    (gl.glUniform1f)(self.bg_u_plane, 0.0);
                    (gl.glUniform1f)(self.bg_u_depth_ndc, (200.0 - 258.0) / 220.0);
                    (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
                }

                if any_bg1 {
                    (gl.glUniform1f)(self.bg_u_plane, 1.0);
                    (gl.glUniform1f)(self.bg_u_depth_ndc, (200.0 - 129.0) / 220.0);
                    (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
                }
            }
        }

        unsafe {
            (gl.glDisable)(GL_SCISSOR_TEST);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
        }
    }
}

impl Drop for Gles2Compositor {
    fn drop(&mut self) {
        self.ctx.make_current();
        let gl = &self.ctx.gl;
        let texs = [
            self.tex_lut,
            self.tex_tvram,
            self.tex_gvram,
            self.tex_pcg,
            self.tex_pcg_table,
            self.tex_row_state,
            self.tex_palette_rgba,
            self.tex_palette_raw,
            self.tex_cynthia,
            self.tex_output[0],
            self.tex_output[1],
            self.tex_scaled_rgba[0],
            self.tex_scaled_rgba[1],
        ];
        let fbos = [
            self.fbo_cynthia,
            self.fbo_output[0],
            self.fbo_output[1],
            self.fbo_scaled_rgba[0],
            self.fbo_scaled_rgba[1],
            self.fbo_scaled_565[0],
            self.fbo_scaled_565[1],
        ];
        let rbos = [
            self.rbo_cynthia_depth,
            self.rbo_scaled_565[0],
            self.rbo_scaled_565[1],
        ];
        let vbos = [self.vbo_quad, self.vbo_sprites, self.ibo_sprites];
        unsafe {
            (gl.glDeleteTextures)(texs.len() as GLsizei, texs.as_ptr());
            (gl.glDeleteFramebuffers)(fbos.len() as GLsizei, fbos.as_ptr());
            (gl.glDeleteRenderbuffers)(rbos.len() as GLsizei, rbos.as_ptr());
            (gl.glDeleteBuffers)(vbos.len() as GLsizei, vbos.as_ptr());
            (gl.glDeleteProgram)(self.prog_sprite);
            (gl.glDeleteProgram)(self.prog_bg);
            (gl.glDeleteProgram)(self.prog_vicon_fast_u);
            (gl.glDeleteProgram)(self.prog_vicon_fast_r);
            (gl.glDeleteProgram)(self.prog_vicon_raw);
            (gl.glDeleteProgram)(self.prog_blit);
        }
    }
}

fn create_nearest_texture(gl: &Gles2Procs) -> GLuint {
    let mut tex = 0u32;
    unsafe {
        (gl.glGenTextures)(1, &mut tex);
        (gl.glBindTexture)(GL_TEXTURE_2D, tex);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
    tex
}

fn create_linear_texture(gl: &Gles2Procs) -> GLuint {
    let mut tex = 0u32;
    unsafe {
        (gl.glGenTextures)(1, &mut tex);
        (gl.glBindTexture)(GL_TEXTURE_2D, tex);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
    tex
}

fn probe_rgb565_readback(gl: &Gles2Procs, fbo: GLuint, rbo: GLuint) -> bool {
    unsafe {
        (gl.glBindRenderbuffer)(GL_RENDERBUFFER, rbo);
        (gl.glRenderbufferStorage)(GL_RENDERBUFFER, GL_RGB565, 16, 16);
        (gl.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
        (gl.glFramebufferRenderbuffer)(
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_RENDERBUFFER,
            rbo,
        );
        let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
        let mut fmt: GLint = 0;
        let mut ty: GLint = 0;
        if status == GL_FRAMEBUFFER_COMPLETE {
            (gl.glGetIntegerv)(GL_IMPLEMENTATION_COLOR_READ_FORMAT, &mut fmt);
            (gl.glGetIntegerv)(GL_IMPLEMENTATION_COLOR_READ_TYPE, &mut ty);
        }
        (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
        // Clear any error from drivers lacking GL_RGB565 renderbuffer support
        while (gl.glGetError)() != 0 {}
        status == GL_FRAMEBUFFER_COMPLETE
            && fmt == GL_RGB as GLint
            && ty == GL_UNSIGNED_SHORT_5_6_5 as GLint
    }
}

fn build_lut_rgba() -> Vec<u8> {
    let mut lut = vec![0u8; 256 * 32 * 4];
    let mut put = |row: usize, col: usize, rgba: [u8; 4]| {
        let off = (row * 256 + col) * 4;
        lut[off..off + 4].copy_from_slice(&rgba);
    };

    for b in 0..256usize {
        // Row 10: GRBI high byte (g5, r_hi3)
        put(10, b, [((b >> 3) & 0x1F) as u8, (b & 0x07) as u8, 0, 255]);
        // Row 11: GRBI low byte (r_lo2, b5, i_bit)
        put(
            11,
            b,
            [((b >> 6) & 0x03) as u8, ((b >> 1) & 0x1F) as u8, (b & 1) as u8, 255],
        );
    }

    // Rows 16..19: Precomputed (contrast, c6) -> c8
    for contrast in 0..16usize {
        let c_hi = contrast / 4;
        let c_lo = contrast % 4;
        for c6 in 0..64usize {
            let c8 = (((c6 << 2) | (c6 >> 4)) * contrast / 15) as u8;
            put(16 + c_hi, c_lo * 64 + c6, [c8, c8, c8, 255]);
        }
    }

    lut
}

#[inline]
fn push_palette_baked_rgba(out: &mut Vec<u8>, pal: &[u16; 512], contrast: u8) {
    let c = (contrast & 15) as u32;
    out.reserve(512 * 4);
    for &grbi in pal {
        if grbi == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let i_bit = (grbi & 1) as u32;
            let g6 = ((((grbi >> 11) & 0x1F) as u32) << 1) | i_bit;
            let r6 = ((((grbi >> 6) & 0x1F) as u32) << 1) | i_bit;
            let b6 = ((((grbi >> 1) & 0x1F) as u32) << 1) | i_bit;
            let r8 = (((r6 << 2) | (r6 >> 4)) * c / 15) as u8;
            let g8 = (((g6 << 2) | (g6 >> 4)) * c / 15) as u8;
            let b8 = (((b6 << 2) | (b6 >> 4)) * c / 15) as u8;
            out.extend_from_slice(&[r8, g8, b8, 255]);
        }
    }
}

fn push_palette_la(out: &mut Vec<u8>, pal: &[u16; 512]) {
    for &grbi in pal {
        out.push((grbi >> 8) as u8);
        out.push(grbi as u8);
    }
}

fn pack_row_state_entry(
    dst: &mut [u8],
    u: &ViconUniforms,
    palette_id: u32,
    bg_row: msx68k_bus::cynthia::BgRowState,
) {
    let mut set = |col: usize, rgba: [u8; 4]| {
        dst[col * 4..col * 4 + 4].copy_from_slice(&rgba);
    };

    // 0: text scroll
    set(
        0,
        [
            (u.text_scroll_x >> 8) as u8,
            u.text_scroll_x as u8,
            (u.text_scroll_y >> 8) as u8,
            u.text_scroll_y as u8,
        ],
    );
    // 1..4: graphic scroll 0..3
    for p in 0..4usize {
        set(
            1 + p,
            [
                (u.gr_scroll_x[p] >> 8) as u8,
                u.gr_scroll_x[p] as u8,
                (u.gr_scroll_y[p] >> 8) as u8,
                u.gr_scroll_y[p] as u8,
            ],
        );
    }
    let gr_rank = ((u.r1_priority >> 8) & 3) as u8;
    let tx_rank = ((u.r1_priority >> 10) & 3) as u8;
    let sp_rank = ((u.r1_priority >> 12) & 3) as u8;
    let cynthia_active = bg_row.is_cynthia_active() && (u.r2_display & 64 != 0) && gr_rank != 3;
    set(
        5,
        [
            (u.r0_mode & 7) as u8,
            (u.r2_display & 127) as u8,
            (u.contrast & 15) as u8,
            if cynthia_active { 255 } else { 0 },
        ],
    );
    set(6, [gr_rank, tx_rank, sp_rank, 0]);
    set(
        7,
        [
            (u.r1_priority & 3) as u8,
            ((u.r1_priority >> 2) & 3) as u8,
            ((u.r1_priority >> 4) & 3) as u8,
            ((u.r1_priority >> 6) & 3) as u8,
        ],
    );
    let pal_lo = if palette_id == u32::MAX {
        0xFF
    } else {
        (palette_id & 0xFF) as u8
    };
    let pal_hi = if palette_id == u32::MAX {
        0xFF
    } else {
        ((palette_id >> 8) & 0xFF) as u8
    };
    let contrast = (u.contrast & 15) as u8;
    set(8, [pal_lo, pal_hi, contrast / 4, (contrast % 4) * 64]);

    // 9..14: CYNTHIA BG state
    let large = bg_row.large_tiles();
    let mask = if large { 1023i32 } else { 511i32 };
    let origin_x = bg_row.origin_x();
    let source_y = bg_row.source_y();
    let (bg0_sx, bg0_sy) = bg_row.bg0_scroll();
    let (bg1_sx, bg1_sy) = bg_row.bg1_scroll();
    let bg0_eff_sx = (bg0_sx as i32 - origin_x) & mask;
    let bg0_eff_sy = (source_y + bg0_sy as i32) & mask;
    let bg1_eff_sx = (bg1_sx as i32 - origin_x) & 511;
    let bg1_eff_sy = (source_y + bg1_sy as i32) & 511;

    set(
        9,
        [
            (bg0_eff_sx >> 8) as u8,
            bg0_eff_sx as u8,
            (bg0_eff_sy >> 8) as u8,
            bg0_eff_sy as u8,
        ],
    );
    set(
        10,
        [
            (bg1_eff_sx >> 8) as u8,
            bg1_eff_sx as u8,
            (bg1_eff_sy >> 8) as u8,
            bg1_eff_sy as u8,
        ],
    );
    let tbl_id = bg_row.pcg_table_id();
    set(
        11,
        [(tbl_id & 0xFF) as u8, ((tbl_id >> 8) & 0xFF) as u8, 0, 0],
    );

    let active_gate = bg_row.is_cynthia_active() && bg_row.bg_valid();
    let full_mem = bg_row.full_pattern_mem();
    set(
        12,
        [
            if active_gate && bg_row.bg0_en() { 255 } else { 0 },
            bg_row.bg0_map_sel() as u8,
            if bg_row.bg0_bank() && !full_mem { 255 } else { 0 },
            bg_row.bg0_bank() as u8,
        ],
    );
    set(
        13,
        [
            if active_gate && bg_row.bg1_en() && !large {
                255
            } else {
                0
            },
            bg_row.bg1_map_sel() as u8,
            if bg_row.bg1_bank() && !full_mem { 255 } else { 0 },
            bg_row.bg1_bank() as u8,
        ],
    );
    set(14, [if large { 255 } else { 0 }, 0, 0, 0]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vicon_fs_compiles_with_forced_mediump() {
        let comp = Gles2Compositor::new().expect("headless EGL + GLES 2.0 context required");
        assert!(comp.ctx.make_current());
        let gl = &comp.ctx.gl;

        for (name, fs_src) in [
            ("VICON_FAST_UNIFORM_FS", shaders::VICON_FAST_UNIFORM_FS),
            ("VICON_FAST_RASTER_FS", shaders::VICON_FAST_RASTER_FS),
            ("VICON_RAW_FS", shaders::VICON_RAW_FS),
            ("BLIT_FS", shaders::BLIT_FS),
        ] {
            let forced_mediump = fs_src.replace(
                "#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif",
                "precision mediump float;",
            );
            let vs = if name == "BLIT_FS" {
                shaders::BLIT_VS
            } else {
                shaders::VICON_VS
            };
            let prog = compile_program(gl, vs, &forced_mediump, &[(0, "a_pos")])
                .unwrap_or_else(|e| panic!("{name} failed forced mediump compile: {e}"));
            unsafe {
                (gl.glDeleteProgram)(prog);
                assert_eq!((gl.glGetError)(), 0, "GL error after compiling {name}");
            }
        }
    }
}
