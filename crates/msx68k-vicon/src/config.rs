//! Embedder-facing renderer configuration and pixel output formats.

/// Maximum framebuffer dimension supported by default (`3840 × 2160`).
pub const MAX_FB_WIDTH: u32 = 3840;
pub const MAX_FB_HEIGHT: u32 = 2160;

/// Selects which shader generation backend to use.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub enum ShaderGeneration {
    /// Prefer Compute (`wgpu`), fall back to `Gles2`, then `Cpu`.
    #[default]
    Auto,
    /// Single-pass 16×16 tile Compute pipeline (`wgpu`).
    Compute,
    /// Multi-pass vertex/fragment pipeline (`EGL` + `GLES 2.0`).
    Gles2,
    /// Cycle/bit-identical CPU reference compositor.
    Cpu,
}

/// Output pixel format requested by an embedder.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub enum PixelFormat {
    #[default]
    Rgba8,
    Rgb565,
}

/// Configuration injected by an embedder when creating [`crate::ViconCompositor`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct RendererConfig {
    pub generation: ShaderGeneration,
    pub max_width: u32,
    pub max_height: u32,
    pub output_format: PixelFormat,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            generation: ShaderGeneration::Auto,
            max_width: MAX_FB_WIDTH,
            max_height: MAX_FB_HEIGHT,
            output_format: PixelFormat::Rgba8,
        }
    }
}

/// Per-frame timing breakdown (microseconds) optionally collected by GPU backends.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct FrameTimingStats {
    /// CPU-side packing and texture/buffer upload time.
    pub upload_us: u32,
    /// GPU draw execution time (measured via `glFinish` only when timing collection is enabled).
    pub draw_us: u32,
    /// GPU-to-CPU pixel readback (and optional format conversion) time.
    pub readback_us: u32,
}

/// A rendered RGBA8 frame produced by a VICON compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    /// Row-major `RGBA8` pixels (`width * height * 4` bytes).
    pub rgba8: Vec<u8>,
}

impl RenderedFrame {
    /// Returns the number of pixels whose RGB values are non-black (`R != 0 || G != 0 || B != 0`).
    pub fn count_lit_pixels(&self) -> usize {
        self.rgba8
            .chunks_exact(4)
            .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
            .count()
    }

    /// Resizes this frame to `dst_w × dst_h` using bilinear sampling (returns a clone if unchanged).
    pub fn resize_bilinear(&self, dst_w: u32, dst_h: u32) -> Self {
        let dst_w = dst_w.max(1);
        let dst_h = dst_h.max(1);
        if dst_w == self.width && dst_h == self.height {
            return self.clone();
        }
        let sw = self.width.max(1) as usize;
        let sh = self.height.max(1) as usize;
        let dw = dst_w as usize;
        let dh = dst_h as usize;
        let mut out = vec![0u8; dw * dh * 4];
        let x_scale = (sw as f32) / (dw as f32);
        let y_scale = (sh as f32) / (dh as f32);
        for dy in 0..dh {
            let fy = ((dy as f32 + 0.5) * y_scale - 0.5).clamp(0.0, (sh - 1) as f32);
            let y0 = fy.floor() as usize;
            let y1 = (y0 + 1).min(sh - 1);
            let wy = fy - y0 as f32;
            let iwy = 1.0 - wy;
            for dx in 0..dw {
                let fx = ((dx as f32 + 0.5) * x_scale - 0.5).clamp(0.0, (sw - 1) as f32);
                let x0 = fx.floor() as usize;
                let x1 = (x0 + 1).min(sw - 1);
                let wx = fx - x0 as f32;
                let iwx = 1.0 - wx;

                let p00 = (y0 * sw + x0) * 4;
                let p10 = (y0 * sw + x1) * 4;
                let p01 = (y1 * sw + x0) * 4;
                let p11 = (y1 * sw + x1) * 4;
                let dst = (dy * dw + dx) * 4;
                for c in 0..3 {
                    let top = self.rgba8[p00 + c] as f32 * iwx + self.rgba8[p10 + c] as f32 * wx;
                    let bot = self.rgba8[p01 + c] as f32 * iwx + self.rgba8[p11 + c] as f32 * wx;
                    out[dst + c] = (top * iwy + bot * wy + 0.5).clamp(0.0, 255.0) as u8;
                }
                out[dst + 3] = 255;
            }
        }
        Self {
            width: dst_w,
            height: dst_h,
            rgba8: out,
        }
    }

    /// Converts this RGBA8 frame to row-major packed `RGB565`.
    pub fn to_rgb565(&self) -> RenderedFrame565 {
        let mut rgb565 = Vec::with_capacity((self.width * self.height) as usize);
        for px in self.rgba8.chunks_exact(4) {
            let r = (px[0] >> 3) as u16;
            let g = (px[1] >> 2) as u16;
            let b = (px[2] >> 3) as u16;
            rgb565.push((r << 11) | (g << 5) | b);
        }
        RenderedFrame565 {
            width: self.width,
            height: self.height,
            rgb565,
        }
    }
}

/// A rendered packed RGB565 frame produced for embedders targeting 16-bit displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFrame565 {
    pub width: u32,
    pub height: u32,
    /// Row-major packed `RGB565` pixels (`width * height` `u16` values).
    pub rgb565: Vec<u16>,
}
