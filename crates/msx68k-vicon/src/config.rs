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
