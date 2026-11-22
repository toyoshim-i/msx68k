//! OpenGL ES 2.0 (`EGL` + `GLES 2.0`) fallback backend (`wgpu`-independent).
//!
//! Renders CYNTHIA (coalesced `16×h` sprite spans + BG0/BG1) into a single depth-buffered
//! offscreen FBO (Pass 1) and composites TVRAM, GVRAM, CYNTHIA, and independent 16-bit
//! palette generations into the output FBO (Pass 2), strictly within the 8-sampler limit
//! and `mediump`-safe coordinate bounds.

mod ffi;
mod shaders;

use std::ffi::c_void;
use std::ptr;

use msx68k_bus::cynthia::CynthiaFrame;
use msx68k_bus::{DirtyVramPages, X68kBus, GVRAM_BYTES, TVRAM_BYTES, TVRAM_PLANE_BYTES};

use crate::config::{PixelFormat, RenderedFrame, RenderedFrame565, RendererConfig};
use crate::raster;
use crate::ViconUniforms;

use ffi::*;

const MAX_BATCH_SPANS: usize = 16_384;
const VERTS_PER_SPAN: usize = 4;
const FLOATS_PER_VERT: usize = 9; // pos(2) + depth(1) + pat(2) + meta(4)

/// Headless EGL + OpenGL ES 2.0 VICON compositor.
pub struct Gles2Compositor {
    ctx: EglContextGuard,
    renderer_name: String,
    max_width: u32,
    max_height: u32,
    max_tex_size: u32,

    prog_sprite: GLuint,
    prog_bg: GLuint,
    prog_vicon: GLuint,

    // Uniform locations
    sp_u_inv_half_size: GLint,
    sp_u_pcg: GLint,
    sp_u_lut: GLint,
    sp_u_pcg_inv_h: GLint,

    bg_u_depth_ndc: GLint,
    bg_u_pcg: GLint,
    bg_u_pcg_table: GLint,
    bg_u_row_state: GLint,
    bg_u_lut: GLint,
    bg_u_plane: GLint,
    bg_u_row_inv_h: GLint,
    bg_u_tbl_inv_h: GLint,
    bg_u_pcg_inv_h: GLint,

    vi_u_tvram: GLint,
    vi_u_gvram: GLint,
    vi_u_cynthia: GLint,
    vi_u_row_state: GLint,
    vi_u_palette: GLint,
    vi_u_lut: GLint,
    vi_u_row_inv_h: GLint,
    vi_u_pal_inv_h: GLint,
    vi_u_cynthia_inv_h: GLint,
    vi_u_has_row_map: GLint,
    vi_u_cynthia_enabled: GLint,
    vi_u_is_ext_tvram: GLint,
    vi_u_ext_tvram: GLint,

    // Textures
    tex_lut: GLuint,
    tex_tvram: GLuint,
    tex_gvram: GLuint,
    tex_pcg: GLuint,
    tex_pcg_table: GLuint,
    tex_row_state: GLuint,
    tex_palette: GLuint,
    tex_cynthia: GLuint,
    tex_output: GLuint,

    // Allocated texture dimensions
    tvram_tex_w: u32,
    tvram_tex_h: u32,
    pcg_tex_h: u32,
    pcg_tbl_tex_h: u32,
    row_state_tex_h: u32,
    palette_tex_h: u32,
    cynthia_tex_h: u32,
    output_tex_w: u32,
    output_tex_h: u32,

    // FBOs & RBOs
    fbo_cynthia: GLuint,
    rbo_cynthia_depth: GLuint,
    fbo_output: GLuint,

    // Geometry buffers
    vbo_quad: GLuint,
    vbo_sprites: GLuint,
    ibo_sprites: GLuint,
    vbo_sprites_cap_bytes: usize,

    // Dirty tracking & CPU staging scratch
    vram_dirty_seeded: bool,
    prev_cursor_pages: u128,
    ext_tvram_was_active: bool,
    scratch_verts: Vec<f32>,
    scratch_row_state: Vec<u8>,
    scratch_palette: Vec<u8>,
    scratch_pcg: Vec<u8>,
    scratch_pcg_tbl: Vec<u8>,
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
        let prog_vicon =
            compile_program(gl, shaders::VICON_VS, shaders::VICON_FS, &[(0, "a_pos")]).ok()?;

        unsafe {
            (gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 1);
            (gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
            (gl.glDisable)(GL_BLEND);
            (gl.glDisable)(GL_DITHER);
            (gl.glDisable)(GL_CULL_FACE);
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

        let tex_tvram = create_nearest_texture(gl);
        let tvram_tex_w = 1024;
        let tvram_tex_h = 512;
        unsafe {
            let zero = vec![0u8; TVRAM_BYTES];
            (gl.glBindTexture)(GL_TEXTURE_2D, tex_tvram);
            (gl.glTexImage2D)(
                GL_TEXTURE_2D,
                0,
                GL_LUMINANCE as GLint,
                tvram_tex_w as GLsizei,
                tvram_tex_h as GLsizei,
                0,
                GL_LUMINANCE,
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
        let tex_palette = create_nearest_texture(gl);
        let tex_cynthia = create_nearest_texture(gl);
        let tex_output = create_nearest_texture(gl);

        let mut fbos = [0u32; 2];
        let mut rbo = 0u32;
        let mut vbos = [0u32; 3];
        unsafe {
            (gl.glGenFramebuffers)(2, fbos.as_mut_ptr());
            (gl.glGenRenderbuffers)(1, &mut rbo);
            (gl.glGenBuffers)(3, vbos.as_mut_ptr());
        }
        let [fbo_cynthia, fbo_output] = fbos;
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
        }

        let mut comp = Self {
            sp_u_inv_half_size: uniform_loc(gl, prog_sprite, "u_inv_half_size"),
            sp_u_pcg: uniform_loc(gl, prog_sprite, "u_pcg"),
            sp_u_lut: uniform_loc(gl, prog_sprite, "u_lut"),
            sp_u_pcg_inv_h: uniform_loc(gl, prog_sprite, "u_pcg_inv_h"),

            bg_u_depth_ndc: uniform_loc(gl, prog_bg, "u_depth_ndc"),
            bg_u_pcg: uniform_loc(gl, prog_bg, "u_pcg"),
            bg_u_pcg_table: uniform_loc(gl, prog_bg, "u_pcg_table"),
            bg_u_row_state: uniform_loc(gl, prog_bg, "u_row_state"),
            bg_u_lut: uniform_loc(gl, prog_bg, "u_lut"),
            bg_u_plane: uniform_loc(gl, prog_bg, "u_plane"),
            bg_u_row_inv_h: uniform_loc(gl, prog_bg, "u_row_inv_h"),
            bg_u_tbl_inv_h: uniform_loc(gl, prog_bg, "u_tbl_inv_h"),
            bg_u_pcg_inv_h: uniform_loc(gl, prog_bg, "u_pcg_inv_h"),

            vi_u_tvram: uniform_loc(gl, prog_vicon, "u_tvram"),
            vi_u_gvram: uniform_loc(gl, prog_vicon, "u_gvram"),
            vi_u_cynthia: uniform_loc(gl, prog_vicon, "u_cynthia"),
            vi_u_row_state: uniform_loc(gl, prog_vicon, "u_row_state"),
            vi_u_palette: uniform_loc(gl, prog_vicon, "u_palette"),
            vi_u_lut: uniform_loc(gl, prog_vicon, "u_lut"),
            vi_u_row_inv_h: uniform_loc(gl, prog_vicon, "u_row_inv_h"),
            vi_u_pal_inv_h: uniform_loc(gl, prog_vicon, "u_pal_inv_h"),
            vi_u_cynthia_inv_h: uniform_loc(gl, prog_vicon, "u_cynthia_inv_h"),
            vi_u_has_row_map: uniform_loc(gl, prog_vicon, "u_has_row_map"),
            vi_u_cynthia_enabled: uniform_loc(gl, prog_vicon, "u_cynthia_enabled"),
            vi_u_is_ext_tvram: uniform_loc(gl, prog_vicon, "u_is_ext_tvram"),
            vi_u_ext_tvram: uniform_loc(gl, prog_vicon, "u_ext_tvram"),

            ctx,
            renderer_name,
            max_width: config.max_width.clamp(256, crate::MAX_FB_WIDTH),
            max_height: config.max_height.clamp(128, crate::MAX_FB_HEIGHT),
            max_tex_size,

            prog_sprite,
            prog_bg,
            prog_vicon,

            tex_lut,
            tex_tvram,
            tex_gvram,
            tex_pcg,
            tex_pcg_table,
            tex_row_state,
            tex_palette,
            tex_cynthia,
            tex_output,

            tvram_tex_w,
            tvram_tex_h,
            pcg_tex_h: 0,
            pcg_tbl_tex_h: 0,
            row_state_tex_h: 0,
            palette_tex_h: 0,
            cynthia_tex_h: 0,
            output_tex_w: 0,
            output_tex_h: 0,

            fbo_cynthia,
            rbo_cynthia_depth: rbo,
            fbo_output,

            vbo_quad,
            vbo_sprites,
            ibo_sprites,
            vbo_sprites_cap_bytes: 0,

            vram_dirty_seeded: false,
            prev_cursor_pages: 0,
            ext_tvram_was_active: false,
            scratch_verts: Vec::new(),
            scratch_row_state: Vec::new(),
            scratch_palette: Vec::new(),
            scratch_pcg: Vec::new(),
            scratch_pcg_tbl: Vec::new(),
            scratch_ext_tvram: Vec::new(),
        };

        if !comp.ensure_cynthia_fbo(512) || !comp.ensure_output_fbo(768, 512) {
            return None;
        }
        Some(comp)
    }

    /// Returns the GL_RENDERER string of the active GLES 2.0 driver.
    #[inline]
    pub fn renderer_name(&self) -> &str {
        &self.renderer_name
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

    fn ensure_output_fbo(&mut self, w: u32, h: u32) -> bool {
        if w <= self.output_tex_w && h <= self.output_tex_h {
            return true;
        }
        let nw = w.max(self.output_tex_w).min(self.max_tex_size);
        let nh = h.max(self.output_tex_h).min(self.max_tex_size);
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_output);
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
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_output);
            (gl.glFramebufferTexture2D)(
                GL_FRAMEBUFFER,
                GL_COLOR_ATTACHMENT0,
                GL_TEXTURE_2D,
                self.tex_output,
                0,
            );
            let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            if status != GL_FRAMEBUFFER_COMPLETE {
                return false;
            }
        }
        self.output_tex_w = nw;
        self.output_tex_h = nh;
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
        self.upload_vram_full(bus);
        self.render_common(bus)
    }

    /// Renders `bus` using dirty 4 KB VRAM page uploads (with cursor overlay restoration).
    pub fn render_bus_frame_dirty(&mut self, bus: &mut X68kBus) -> RenderedFrame {
        self.ctx.make_current();
        self.upload_vram_dirty(bus);
        self.render_common(bus)
    }

    /// Renders `bus` into `RGB565` pixels (matching [`PixelFormat::Rgb565`]).
    pub fn render_bus_frame_rgb565(&mut self, bus: &mut X68kBus) -> RenderedFrame565 {
        self.render_bus_frame_dirty(bus).to_rgb565()
    }

    pub fn render_bus_with_format(
        &mut self,
        bus: &mut X68kBus,
        format: PixelFormat,
    ) -> RenderedFrame {
        let _ = format;
        self.render_bus_frame_dirty(bus)
    }

    fn upload_cursor_overlay(&self, bus: &X68kBus, ext_active: bool, row_bytes: usize, plane_bytes: usize) -> u128 {
        let gl = &self.ctx.gl;
        let mut touched = DirtyVramPages::default();
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
        }
        if !ext_active {
            for (offset, bytes) in bus.cursor_overlay() {
                if offset + 4 <= TVRAM_BYTES {
                    let plane = offset / TVRAM_PLANE_BYTES;
                    let in_plane = offset % TVRAM_PLANE_BYTES;
                    let row = in_plane / 128;
                    let col = in_plane % 128;
                    let tex_x = (plane * 256 + (row / 512) * 128 + col) as GLint;
                    let tex_y = (row % 512) as GLint;
                    unsafe {
                        (gl.glTexSubImage2D)(
                            GL_TEXTURE_2D,
                            0,
                            tex_x,
                            tex_y,
                            4,
                            1,
                            GL_LUMINANCE,
                            GL_UNSIGNED_BYTE,
                            bytes.as_ptr() as *const c_void,
                        );
                    }
                    touched.mark_tvram_range(offset..offset + 4);
                }
            }
        } else {
            let max_bytes = 4 * plane_bytes;
            let _ = row_bytes;
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
        let tex_h = ((tvram.len() as u32).div_ceil(tex_w)).next_power_of_two().max(512).min(self.max_tex_size);
        if self.tvram_tex_w != tex_w || self.tvram_tex_h < tex_h {
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
        if self.tvram_tex_w != 1024 || self.tvram_tex_h != 512 {
            self.tvram_tex_w = 1024;
            self.tvram_tex_h = 512;
            let gl = &self.ctx.gl;
            let zero = vec![0u8; TVRAM_BYTES];
            unsafe {
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
                (gl.glTexImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    GL_LUMINANCE as GLint,
                    1024,
                    512,
                    0,
                    GL_LUMINANCE,
                    GL_UNSIGNED_BYTE,
                    zero.as_ptr() as *const c_void,
                );
            }
        }
    }

    fn upload_standard_tvram_pages(&self, tvram: &[u8], pages: u128) {
        let gl = &self.ctx.gl;
        unsafe {
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
        }
        let mut mask = pages;
        while mask != 0 {
            let page = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let off = page * 4096;
            if off + 4096 > tvram.len() {
                continue;
            }
            let plane = page / 32;
            let row_in_plane = (page % 32) * 32;
            let half = row_in_plane / 512;
            let tex_x = (plane * 256 + half * 128) as GLint;
            let tex_y = (row_in_plane % 512) as GLint;
            unsafe {
                (gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    tex_x,
                    tex_y,
                    128,
                    32,
                    GL_LUMINANCE,
                    GL_UNSIGNED_BYTE,
                    tvram[off..].as_ptr() as *const c_void,
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
            mask = if end >= 128 { 0 } else { mask & !((1u128 << end) - 1) };
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
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, true, row_bytes, plane_bytes);
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
                        1024,
                        512,
                        GL_LUMINANCE,
                        GL_UNSIGNED_BYTE,
                        zero.as_ptr() as *const c_void,
                    );
                }
            }
            self.ext_tvram_was_active = false;
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, false, row_bytes, plane_bytes);
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
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, true, row_bytes, plane_bytes);
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
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, false, row_bytes, plane_bytes);
        }

        if written.gvram != 0 {
            if let Some(gvram) = bus.gvram() {
                self.upload_gvram_pages(gvram, written.gvram);
            }
        }
    }

    fn render_common(&mut self, bus: &X68kBus) -> RenderedFrame {
        let mut uniforms = ViconUniforms::from_bus(bus);
        uniforms.width = uniforms.width.min(self.max_width);
        uniforms.height = uniforms.height.min(self.max_height);

        let completed = raster::completed_frame(bus, &uniforms);
        let cynthia = raster::cynthia_input(bus, &uniforms);

        let has_row_map = completed.is_some_and(|f| f.lines.len() > 1) || !cynthia.is_empty();
        let row_count = if has_row_map { uniforms.height as usize } else { 1 };

        // Pack u_row_state (16 RGBA texels = 64 bytes per row)
        self.scratch_row_state.clear();
        self.scratch_row_state.resize(row_count * 64, 0);

        // Pack u_palette (512 LUMINANCE_ALPHA texels = 1024 bytes per palette)
        self.scratch_palette.clear();
        let num_palettes: u32;

        if let Some(frame) = completed {
            let mut next_line = 0usize;
            let mut cur_state = &frame.lines[0].state;
            for y in 0..row_count {
                while let Some(line) = frame.lines.get(next_line).filter(|l| l.y as usize <= y) {
                    cur_state = &line.state;
                    next_line += 1;
                }
                let mut u = uniforms;
                u.apply_raster(cur_state);
                let pal_id = cur_state.palette_id;
                let bg_row = cynthia.bg_rows.get(y).copied().unwrap_or_default();
                pack_row_state_entry(
                    &mut self.scratch_row_state[y * 64..(y + 1) * 64],
                    &u,
                    pal_id,
                    bg_row,
                );
            }
            num_palettes = frame.palettes.len().max(1) as u32;
            self.scratch_palette.reserve(num_palettes as usize * 1024);
            if frame.palettes.is_empty() {
                self.scratch_palette.resize(1024, 0);
            } else {
                for pal in &frame.palettes {
                    push_palette_la(&mut self.scratch_palette, pal);
                }
            }
        } else {
            let mut pal = [0u16; 512];
            pal[..256].copy_from_slice(bus.vicon.text_palette());
            pal[256..].copy_from_slice(bus.vicon.graphic_palette());
            push_palette_la(&mut self.scratch_palette, &pal);
            num_palettes = 1;
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

        // Upload u_row_state and u_palette
        let mut row_state_h = self.row_state_tex_h;
        self.ensure_2d_tex_height(self.tex_row_state, 16, &mut row_state_h, row_count as u32, GL_RGBA);
        self.row_state_tex_h = row_state_h;

        let mut pal_h = self.palette_tex_h;
        self.ensure_2d_tex_height(self.tex_palette, 512, &mut pal_h, num_palettes, GL_LUMINANCE_ALPHA);
        self.palette_tex_h = pal_h;

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
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette);
            (gl.glTexSubImage2D)(
                GL_TEXTURE_2D,
                0,
                0,
                0,
                512,
                num_palettes as GLsizei,
                GL_LUMINANCE_ALPHA,
                GL_UNSIGNED_BYTE,
                self.scratch_palette.as_ptr() as *const c_void,
            );
        }

        // Pass 1: CYNTHIA (skipped completely when empty)
        let cynthia_enabled = !cynthia.is_empty();
        if cynthia_enabled {
            self.render_cynthia_pass(&cynthia);
        }

        // Pass 2: VICON composite
        self.ensure_output_fbo(uniforms.width, uniforms.height);
        let gl = &self.ctx.gl;
        let is_ext_tvram = !(uniforms.tvram_row_bytes == 128
            && uniforms.tvram_plane_bytes == TVRAM_PLANE_BYTES as u32);
        unsafe {
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_output);
            (gl.glViewport)(0, 0, uniforms.width as GLsizei, uniforms.height as GLsizei);
            (gl.glDisable)(GL_DEPTH_TEST);
            (gl.glDepthMask)(GL_FALSE);

            (gl.glUseProgram)(self.prog_vicon);

            (gl.glActiveTexture)(GL_TEXTURE0);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_tvram);
            (gl.glUniform1i)(self.vi_u_tvram, 0);

            (gl.glActiveTexture)(GL_TEXTURE0 + 1);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_gvram);
            (gl.glUniform1i)(self.vi_u_gvram, 1);

            (gl.glActiveTexture)(GL_TEXTURE0 + 2);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_cynthia);
            (gl.glUniform1i)(self.vi_u_cynthia, 2);

            (gl.glActiveTexture)(GL_TEXTURE0 + 3);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_row_state);
            (gl.glUniform1i)(self.vi_u_row_state, 3);

            (gl.glActiveTexture)(GL_TEXTURE0 + 4);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_palette);
            (gl.glUniform1i)(self.vi_u_palette, 4);

            (gl.glActiveTexture)(GL_TEXTURE0 + 5);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_lut);
            (gl.glUniform1i)(self.vi_u_lut, 5);

            (gl.glUniform1f)(self.vi_u_row_inv_h, 1.0 / (self.row_state_tex_h as f32));
            (gl.glUniform1f)(self.vi_u_pal_inv_h, 1.0 / (self.palette_tex_h as f32));
            (gl.glUniform1f)(self.vi_u_cynthia_inv_h, 1.0 / (self.cynthia_tex_h.max(1) as f32));
            (gl.glUniform1f)(self.vi_u_has_row_map, if has_row_map { 1.0 } else { 0.0 });
            (gl.glUniform1f)(self.vi_u_cynthia_enabled, if cynthia_enabled { 1.0 } else { 0.0 });
            (gl.glUniform1f)(self.vi_u_is_ext_tvram, if is_ext_tvram { 1.0 } else { 0.0 });
            (gl.glUniform4f)(
                self.vi_u_ext_tvram,
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

        let mut rgba8 = vec![0u8; (uniforms.width * uniforms.height * 4) as usize];
        unsafe {
            (gl.glReadPixels)(
                0,
                0,
                uniforms.width as GLsizei,
                uniforms.height as GLsizei,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                rgba8.as_mut_ptr() as *mut c_void,
            );
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
        }

        RenderedFrame {
            width: uniforms.width,
            height: uniforms.height,
            rgba8,
        }
    }

    fn render_cynthia_pass(&mut self, cynthia: &CynthiaFrame) {
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

            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, self.fbo_cynthia);
            (gl.glViewport)(0, 0, 512, cyn_h as GLsizei);
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

                (gl.glActiveTexture)(GL_TEXTURE0 + 1);
                (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_lut);
                (gl.glUniform1i)(self.sp_u_lut, 1);

                (gl.glUniform2f)(self.sp_u_inv_half_size, 2.0 / 512.0, 2.0 / (cyn_h as f32));
                (gl.glUniform1f)(self.sp_u_pcg_inv_h, 1.0 / (self.pcg_tex_h as f32));
                (gl.glBindBuffer)(GL_ELEMENT_ARRAY_BUFFER, self.ibo_sprites);
                (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_sprites);
            }

            for chunk in cynthia.spans.chunks(MAX_BATCH_SPANS) {
                self.scratch_verts.clear();
                self.scratch_verts
                    .reserve(chunk.len() * VERTS_PER_SPAN * FLOATS_PER_VERT);
                for &sp in chunk {
                    let x0 = sp.x0() as f32;
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
                    (gl.glVertexAttribPointer)(1, 1, GL_FLOAT, GL_FALSE, stride_b, 8 as *const c_void);
                    (gl.glEnableVertexAttribArray)(2);
                    (gl.glVertexAttribPointer)(2, 2, GL_FLOAT, GL_FALSE, stride_b, 12 as *const c_void);
                    (gl.glEnableVertexAttribArray)(3);
                    (gl.glVertexAttribPointer)(3, 4, GL_FLOAT, GL_FALSE, stride_b, 20 as *const c_void);

                    (gl.glDrawElements)(
                        GL_TRIANGLES,
                        (chunk.len() * 6) as GLsizei,
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

        // Pass 1B: BG0 (order 258) and BG1 (order 129)
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

            (gl.glActiveTexture)(GL_TEXTURE0 + 3);
            (gl.glBindTexture)(GL_TEXTURE_2D, self.tex_lut);
            (gl.glUniform1i)(self.bg_u_lut, 3);

            (gl.glUniform1f)(self.bg_u_row_inv_h, 1.0 / (self.row_state_tex_h as f32));
            (gl.glUniform1f)(self.bg_u_tbl_inv_h, 1.0 / (self.pcg_tbl_tex_h as f32));
            (gl.glUniform1f)(self.bg_u_pcg_inv_h, 1.0 / (self.pcg_tex_h as f32));

            (gl.glBindBuffer)(GL_ARRAY_BUFFER, self.vbo_quad);
            (gl.glEnableVertexAttribArray)(0);
            (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 8, ptr::null());

            // BG0 (depth key 258)
            (gl.glUniform1f)(self.bg_u_plane, 0.0);
            (gl.glUniform1f)(self.bg_u_depth_ndc, (200.0 - 258.0) / 220.0);
            (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);

            // BG1 (depth key 129)
            (gl.glUniform1f)(self.bg_u_plane, 1.0);
            (gl.glUniform1f)(self.bg_u_depth_ndc, (200.0 - 129.0) / 220.0);
            (gl.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);

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
            self.tex_palette,
            self.tex_cynthia,
            self.tex_output,
        ];
        let fbos = [self.fbo_cynthia, self.fbo_output];
        let vbos = [self.vbo_quad, self.vbo_sprites, self.ibo_sprites];
        unsafe {
            (gl.glDeleteTextures)(texs.len() as GLsizei, texs.as_ptr());
            (gl.glDeleteFramebuffers)(fbos.len() as GLsizei, fbos.as_ptr());
            (gl.glDeleteRenderbuffers)(1, &self.rbo_cynthia_depth);
            (gl.glDeleteBuffers)(vbos.len() as GLsizei, vbos.as_ptr());
            (gl.glDeleteProgram)(self.prog_sprite);
            (gl.glDeleteProgram)(self.prog_bg);
            (gl.glDeleteProgram)(self.prog_vicon);
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

fn build_lut_rgba() -> Vec<u8> {
    let mut lut = vec![0u8; 256 * 32 * 4];
    let mut put = |row: usize, col: usize, rgba: [u8; 4]| {
        let off = (row * 256 + col) * 4;
        lut[off..off + 4].copy_from_slice(&rgba);
    };

    for b in 0..256usize {
        // Rows 0..7: TVRAM 1-bit extraction (MSB = bit 7 at col 0)
        for r in 0..8usize {
            let bit = ((b >> (7 - r)) & 1) as u8;
            put(r, b, [bit * 255, 0, 0, 255]);
        }
        // Row 8: Nibbles + bit0 + bit7
        put(
            8,
            b,
            [
                (b >> 4) as u8,
                (b & 15) as u8,
                ((b & 1) as u8) * 255,
                (((b >> 7) & 1) as u8) * 255,
            ],
        );
        // Row 9: BG tile high byte (pal_block, hflip, vflip)
        put(
            9,
            b,
            [
                (b & 15) as u8,
                if b & 0x40 != 0 { 255 } else { 0 },
                if b & 0x80 != 0 { 255 } else { 0 },
                255,
            ],
        );
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
    let pal_lo = if palette_id == u32::MAX { 0xFF } else { (palette_id & 0xFF) as u8 };
    let pal_hi = if palette_id == u32::MAX { 0xFF } else { ((palette_id >> 8) & 0xFF) as u8 };
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
    set(11, [(tbl_id & 0xFF) as u8, ((tbl_id >> 8) & 0xFF) as u8, 0, 0]);

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
            if active_gate && bg_row.bg1_en() && !large { 255 } else { 0 },
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
        let forced_mediump = shaders::VICON_FS.replace(
            "#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif",
            "precision mediump float;",
        );
        assert_ne!(
            forced_mediump,
            shaders::VICON_FS,
            "precision block replacement must match VICON_FS"
        );
        let prog = compile_program(gl, shaders::VICON_VS, &forced_mediump, &[(0, "a_pos")])
            .expect("VICON_FS must compile cleanly under forced precision mediump float");
        unsafe {
            (gl.glDeleteProgram)(prog);
            assert_eq!((gl.glGetError)(), 0);
        }
    }
}
