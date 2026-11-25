//! X68000 VICON GPU compositor and TVRAM/GVRAM/Palette renderer.
//!
//! Provides:
//! - [`ViconCompositor`]: Unified embedder-facing compositor selecting between Compute (`wgpu`),
//!   GLES 2.0 (`EGL` + `GLES 2.0`), and CPU fallback according to [`RendererConfig`].
//! - [`ViconGpuCompositor`]: Hardware-accelerated `wgpu` single-pass 16×16 tile Compute pipeline
//!   that composites 4-plane TVRAM, GVRAM, 16-bit Text/Graphics Palettes, and CYNTHIA sprite
//!   spans + BG0/BG1 + 128B PCG/map block histories.
//! - [`VmScreen`]: Per-VM GPU compositor uploading dirty VRAM pages into double-buffered textures.
//! - [`render_bus_frame_cpu`]: Cycle/bit-identical CPU reference compositor.
//! - [`decode_tvram_ank_lines`]: X68000 CGROM (`$F3A800`, 8×16 ANK) glyph recognizer.

pub mod config;
#[cfg(feature = "gles2-backend")]
pub mod gles2;
mod raster;
#[cfg(feature = "wgpu-backend")]
mod vm_screen;

pub use config::{
    FrameTimingStats, PixelFormat, RenderedFrame, RenderedFrame565, RendererConfig,
    ShaderGeneration, MAX_FB_HEIGHT, MAX_FB_WIDTH,
};
#[cfg(feature = "wgpu-backend")]
pub use vm_screen::{
    read_back_texture, CompletedFrame, ScreenRect, VmLayer, VmScreen, VmScreenOutput, VM_SCREEN_MAX,
};

use bytemuck::{Pod, Zeroable};
#[cfg(feature = "wgpu-backend")]
use msx68k_bus::{DirtyVramPages, GVRAM_BYTES, TVRAM_BYTES};
use msx68k_bus::{X68kBus, TVRAM_PLANE_BYTES};

#[cfg(feature = "wgpu-backend")]
use vm_screen::{LayerPipeline, Placement};

/// Maximum TVRAM buffer bytes (4 bitplanes for 3840×2160 = 4,147,200 bytes).
pub const MAX_TVRAM_BUF_BYTES: usize =
    4 * ((MAX_FB_WIDTH as usize / 8) * (MAX_FB_HEIGHT as usize));

#[cfg(feature = "wgpu-backend")]
const INITIAL_DYNAMIC_BYTES: u64 = 64 * 1024;

/// Uniform parameters uploaded to the VICON compositor shaders.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct ViconUniforms {
    pub width: u32,
    pub height: u32,
    pub text_scroll_x: u32,
    pub text_scroll_y: u32,
    pub gr_scroll_x: [u32; 4],
    pub gr_scroll_y: [u32; 4],
    pub r0_mode: u32,
    pub r1_priority: u32,
    pub r2_display: u32,
    pub contrast: u32,
    pub tvram_row_bytes: u32,
    pub tvram_plane_bytes: u32,
    /// `[0]`: raster row-map height (`0` for uniform frame), `[1]`: bit flags (`FLAG_CYNTHIA_ENABLED`).
    pub reserved: [u32; 2],
}

impl ViconUniforms {
    /// Extracts the current CRTC, VICON, and System Port display parameters from `bus`.
    pub fn from_bus(bus: &X68kBus) -> Self {
        let (w, h) = bus.crtc.active_resolution();
        let scrolls = std::array::from_fn::<_, 4, _>(|p| bus.crtc.graphic_scroll(p));
        Self {
            width: w.clamp(256, MAX_FB_WIDTH),
            height: h.clamp(128, MAX_FB_HEIGHT),
            text_scroll_x: bus.crtc.text_scroll_x() as u32,
            text_scroll_y: bus.crtc.text_scroll_y() as u32,
            gr_scroll_x: scrolls.map(|(x, _)| x as u32),
            gr_scroll_y: scrolls.map(|(_, y)| y as u32),
            r0_mode: bus.vicon.r0_mode() as u32,
            r1_priority: bus.vicon.r1_priority() as u32,
            r2_display: bus.vicon.r2_display() as u32,
            contrast: (bus.sysport.contrast & 0x0F) as u32,
            tvram_row_bytes: bus.active_tvram_row_bytes() as u32,
            tvram_plane_bytes: bus.active_tvram_plane_bytes() as u32,
            reserved: [0; 2],
        }
    }
}

/// Unified embedder-facing VICON compositor supporting Compute (`wgpu`), GLES 2.0, and CPU fallback.
pub struct ViconCompositor {
    config: RendererConfig,
    generation: ShaderGeneration,
    #[cfg(feature = "wgpu-backend")]
    wgpu: Option<ViconGpuCompositor>,
    #[cfg(feature = "gles2-backend")]
    gles2: Option<gles2::Gles2Compositor>,
    pending_fallback: Option<RenderedFrame>,
}

impl ViconCompositor {
    /// Creates a compositor configured by `config`.
    pub fn new(config: RendererConfig) -> Self {
        match config.generation {
            ShaderGeneration::Auto => {
                #[cfg(feature = "wgpu-backend")]
                if let Some(gpu) = ViconGpuCompositor::new_with_config(config) {
                    return Self {
                        config,
                        generation: ShaderGeneration::Compute,
                        wgpu: Some(gpu),
                        #[cfg(feature = "gles2-backend")]
                        gles2: None,
                        pending_fallback: None,
                    };
                }
                #[cfg(feature = "gles2-backend")]
                if let Some(es2) = gles2::Gles2Compositor::new_with_config(config) {
                    return Self {
                        config,
                        generation: ShaderGeneration::Gles2,
                        #[cfg(feature = "wgpu-backend")]
                        wgpu: None,
                        gles2: Some(es2),
                        pending_fallback: None,
                    };
                }
                Self::cpu(config)
            }
            ShaderGeneration::Compute => {
                #[cfg(feature = "wgpu-backend")]
                if let Some(gpu) = ViconGpuCompositor::new_with_config(config) {
                    return Self {
                        config,
                        generation: ShaderGeneration::Compute,
                        wgpu: Some(gpu),
                        #[cfg(feature = "gles2-backend")]
                        gles2: None,
                        pending_fallback: None,
                    };
                }
                Self::cpu(config)
            }
            ShaderGeneration::Gles2 => {
                #[cfg(feature = "gles2-backend")]
                if let Some(es2) = gles2::Gles2Compositor::new_with_config(config) {
                    return Self {
                        config,
                        generation: ShaderGeneration::Gles2,
                        #[cfg(feature = "wgpu-backend")]
                        wgpu: None,
                        gles2: Some(es2),
                        pending_fallback: None,
                    };
                }
                Self::cpu(config)
            }
            ShaderGeneration::Cpu => Self::cpu(config),
        }
    }

    /// Creates a compositor borrowing a host-managed GLES 2.0 context that is already current
    /// on the calling thread.
    ///
    /// # Safety
    /// A valid OpenGL ES 2.0 context must be current on the calling thread during initialization
    /// and every subsequent render/destruction call (unless [`Self::mark_context_lost`] is called
    /// before drop).
    #[cfg(feature = "gles2-backend")]
    pub unsafe fn new_gles2_external(
        config: RendererConfig,
        get_proc_address: Option<gles2::GlGetProcAddressFn>,
    ) -> Option<Self> {
        let es2 = gles2::Gles2Compositor::from_current_context(config, get_proc_address)?;
        Some(Self {
            config,
            generation: ShaderGeneration::Gles2,
            #[cfg(feature = "wgpu-backend")]
            wgpu: None,
            gles2: Some(es2),
            pending_fallback: None,
        })
    }

    fn cpu(config: RendererConfig) -> Self {
        Self {
            config,
            generation: ShaderGeneration::Cpu,
            #[cfg(feature = "wgpu-backend")]
            wgpu: None,
            #[cfg(feature = "gles2-backend")]
            gles2: None,
            pending_fallback: None,
        }
    }

    /// Returns the active shader generation selected for this compositor instance.
    #[inline]
    pub const fn active_generation(&self) -> ShaderGeneration {
        self.generation
    }

    /// Returns the embedder configuration.
    #[inline]
    pub const fn config(&self) -> &RendererConfig {
        &self.config
    }

    /// Returns the active GPU renderer / adapter name (`GL_RENDERER` on GLES 2.0, adapter name on
    /// Compute), or `None` when running on the CPU backend.
    pub fn renderer_name(&self) -> Option<&str> {
        #[cfg(feature = "wgpu-backend")]
        if let Some(gpu) = self.wgpu.as_ref() {
            return Some(&gpu.adapter_info().name);
        }
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_ref() {
            return Some(es2.renderer_name());
        }
        None
    }

    /// Enables or disables per-frame `upload / draw / readback` timing breakdown on GPU backends.
    pub fn set_collect_timing(&mut self, enabled: bool) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.set_collect_timing(enabled);
        }
        let _ = enabled;
    }

    /// Returns the most recent per-frame timing breakdown when timing collection is enabled.
    pub fn last_timing(&self) -> Option<FrameTimingStats> {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_ref() {
            return es2.last_timing();
        }
        None
    }

    /// Renders a frame, consuming dirty VRAM page tracking on `bus` for incremental uploads.
    pub fn render_frame(&mut self, bus: &mut X68kBus) -> RenderedFrame {
        #[cfg(feature = "wgpu-backend")]
        if let Some(gpu) = self.wgpu.as_mut() {
            return gpu.render_bus_frame_dirty(bus);
        }
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.render_bus_frame_dirty(bus);
        }
        render_bus_frame_cpu(bus)
    }

    /// Renders a frame from an immutable `&X68kBus` (full VRAM upload on GPU backends).
    pub fn render_frame_readonly(&mut self, bus: &X68kBus) -> RenderedFrame {
        #[cfg(feature = "wgpu-backend")]
        if let Some(gpu) = self.wgpu.as_mut() {
            return gpu.render_bus_frame(bus);
        }
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.render_bus_frame(bus);
        }
        render_bus_frame_cpu(bus)
    }

    /// Renders a frame at native resolution and scales it to `dst_w × dst_h` (`RGBA8`).
    ///
    /// On the GLES 2.0 backend, downsampling runs on the GPU (Pass 3) prior to `glReadPixels`.
    pub fn render_frame_scaled(
        &mut self,
        bus: &mut X68kBus,
        dst_w: u32,
        dst_h: u32,
    ) -> RenderedFrame {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.render_bus_frame_scaled(bus, dst_w, dst_h);
        }
        self.render_frame(bus).resize_bilinear(dst_w, dst_h)
    }

    /// Renders a frame into packed `RGB565` pixels.
    pub fn render_frame_rgb565(&mut self, bus: &mut X68kBus) -> RenderedFrame565 {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.render_bus_frame_rgb565(bus);
        }
        self.render_frame(bus).to_rgb565()
    }

    /// Renders a frame at native resolution, scales it to `dst_w × dst_h`, and returns `RGB565`.
    ///
    /// On the GLES 2.0 backend, Pass 3 downsampling and (when supported by the driver) direct
    /// 16-bit `RGB565` readback run on the GPU.
    pub fn render_frame_scaled_rgb565(
        &mut self,
        bus: &mut X68kBus,
        dst_w: u32,
        dst_h: u32,
    ) -> RenderedFrame565 {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.render_bus_frame_scaled_rgb565(bus, dst_w, dst_h);
        }
        self.render_frame(bus)
            .resize_bilinear(dst_w, dst_h)
            .to_rgb565()
    }

    /// Submits a native-resolution `RGBA8` frame to the GPU without blocking on readback.
    ///
    /// Pair with [`Self::finish_frame`] to overlap GPU rasterization with the next frame's CPU work.
    pub fn submit_frame(&mut self, bus: &mut X68kBus) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.submit_frame(bus);
            return;
        }
        self.pending_fallback = Some(self.render_frame(bus));
    }

    /// Submits a `dst_w × dst_h` scaled `RGBA8` frame without blocking on readback.
    pub fn submit_frame_scaled(&mut self, bus: &mut X68kBus, dst_w: u32, dst_h: u32) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.submit_frame_scaled(bus, dst_w, dst_h);
            return;
        }
        self.pending_fallback = Some(self.render_frame(bus).resize_bilinear(dst_w, dst_h));
    }

    /// Submits a native-resolution `RGB565` frame without blocking on readback.
    pub fn submit_frame_rgb565(&mut self, bus: &mut X68kBus) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.submit_frame_rgb565(bus);
            return;
        }
        self.pending_fallback = Some(self.render_frame(bus));
    }

    /// Submits a `dst_w × dst_h` scaled `RGB565` frame without blocking on readback.
    pub fn submit_frame_scaled_rgb565(&mut self, bus: &mut X68kBus, dst_w: u32, dst_h: u32) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.submit_frame_scaled_rgb565(bus, dst_w, dst_h);
            return;
        }
        self.pending_fallback = Some(self.render_frame(bus).resize_bilinear(dst_w, dst_h));
    }

    /// Completes readback for a previously submitted `RGBA8` frame.
    pub fn finish_frame(&mut self) -> Option<RenderedFrame> {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.finish_frame();
        }
        self.pending_fallback.take()
    }

    /// Completes readback for a previously submitted `RGB565` frame.
    pub fn finish_frame_rgb565(&mut self) -> Option<RenderedFrame565> {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            return es2.finish_frame_rgb565();
        }
        self.pending_fallback.take().map(|f| f.to_rgb565())
    }

    /// Renders a frame directly into `target_fbo` at native resolution without CPU readback,
    /// consuming dirty VRAM page tracking on `bus`.
    ///
    /// Returns `Some((width, height))` on the GLES 2.0 backend while GL resources are live,
    /// or `None` on non-GLES2 backends (or after [`Self::mark_context_lost`] /
    /// [`Self::destroy_gl_resources`]).
    pub fn render_frame_to_fbo(
        &mut self,
        bus: &mut X68kBus,
        target_fbo: u32,
        flip_y: bool,
    ) -> Option<(u32, u32)> {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            if !es2.has_live_gl_resources() {
                return None;
            }
            return Some(es2.render_bus_frame_to_fbo(bus, target_fbo, flip_y));
        }
        let _ = (bus, target_fbo, flip_y);
        None
    }

    /// Renders a frame directly into `target_fbo` from an immutable `&X68kBus` with a full VRAM
    /// upload and without CPU readback.
    pub fn render_frame_to_fbo_readonly(
        &mut self,
        bus: &X68kBus,
        target_fbo: u32,
        flip_y: bool,
    ) -> Option<(u32, u32)> {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            if !es2.has_live_gl_resources() {
                return None;
            }
            return Some(es2.render_bus_frame_to_fbo_readonly(bus, target_fbo, flip_y));
        }
        let _ = (bus, target_fbo, flip_y);
        None
    }

    /// Notifies that the host GL context was lost or destroyed, suppressing `glDelete*` calls in
    /// `Drop`.
    pub fn mark_context_lost(&mut self) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.mark_context_lost();
        }
    }

    /// Explicitly frees owned GL objects while the borrowed GL context is still current.
    pub fn destroy_gl_resources(&mut self) {
        #[cfg(feature = "gles2-backend")]
        if let Some(es2) = self.gles2.as_mut() {
            es2.destroy_gl_resources();
        }
    }
}

/// GPU render pipeline that presents the composited VICON framebuffer onto a window `wgpu::Surface`.
#[cfg(feature = "wgpu-backend")]
pub struct ViconSurfacePresenter {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    layout: wgpu::BindGroupLayout,
    layers: LayerPipeline,
}

/// Hardware-accelerated `wgpu` VICON compositor.
#[cfg(feature = "wgpu-backend")]
pub struct ViconGpuCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    tvram_buf: wgpu::Buffer,
    gvram_buf: wgpu::Buffer,
    palette_buf: wgpu::Buffer,
    cynthia_meta_buf: wgpu::Buffer,
    cynthia_pcg_buf: wgpu::Buffer,
    output_buf: wgpu::Buffer,
    staging_buf: wgpu::Buffer,
    adapter_info: wgpu::AdapterInfo,
    max_width: u32,
    max_height: u32,
    max_tvram_bytes: usize,
    vram_dirty_seeded: bool,
    prev_cursor_pages: u128,
    ext_tvram_was_active: bool,
}

#[cfg(feature = "wgpu-backend")]
impl ViconGpuCompositor {
    /// Initializes a new `wgpu` VICON compositor, selecting a hardware GPU or software fallback adapter.
    pub fn new() -> Option<Self> {
        Self::new_with_config(RendererConfig::default())
    }

    /// Initializes a new `wgpu` VICON compositor using `config`.
    pub fn new_with_config(config: RendererConfig) -> Option<Self> {
        pollster::block_on(Self::new_with_config_async(config))
    }

    /// Asynchronously initializes the `wgpu` device, buffers, and WGSL compute pipeline.
    pub async fn new_async() -> Option<Self> {
        Self::new_with_config_async(RendererConfig::default()).await
    }

    /// Asynchronously initializes the `wgpu` device, buffers, and WGSL compute pipeline using `config`.
    pub async fn new_with_config_async(config: RendererConfig) -> Option<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
        {
            Some(a) => a,
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: true,
                    })
                    .await?
            }
        };

        let adapter_info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .ok()?;

        Some(Self::with_config(device, queue, adapter_info, config))
    }

    /// Creates a [`ViconGpuCompositor`] using an existing `wgpu::Device` and `wgpu::Queue`
    /// (for example, one initialized alongside a `winit` window `wgpu::Surface`).
    pub fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
    ) -> Self {
        Self::with_config(device, queue, adapter_info, RendererConfig::default())
    }

    /// Creates a [`ViconGpuCompositor`] using an existing `wgpu::Device`, `wgpu::Queue`, and `config`.
    pub fn with_config(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
        config: RendererConfig,
    ) -> Self {
        let max_width = config.max_width.clamp(256, MAX_FB_WIDTH);
        let max_height = config.max_height.clamp(128, MAX_FB_HEIGHT);
        let max_tvram_bytes = (4 * ((max_width as usize).div_ceil(8) * max_height as usize))
            .max(TVRAM_BYTES)
            .min(MAX_TVRAM_BUF_BYTES);
        let max_fb_bytes = (max_width * max_height * 4) as u64;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("msx68k_vicon_shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(include_str!("vicon.wgsl"), include_str!("shader.wgsl")).into(),
            ),
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_uniform_buf"),
            size: std::mem::size_of::<ViconUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let tvram_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_tvram_buf"),
            size: max_tvram_bytes as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let gvram_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_gvram_buf"),
            size: GVRAM_BYTES as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let palette_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_palette_buf"),
            size: INITIAL_DYNAMIC_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let cynthia_meta_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_cynthia_meta_buf"),
            size: INITIAL_DYNAMIC_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let cynthia_pcg_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_cynthia_pcg_buf"),
            size: INITIAL_DYNAMIC_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let output_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_output_buf"),
            size: max_fb_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let staging_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vicon_staging_buf"),
            size: max_fb_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let ro_storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("vicon_bind_group_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    ro_storage(1),
                    ro_storage(2),
                    ro_storage(3),
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    ro_storage(5),
                    ro_storage(6),
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vicon_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vicon_compute_pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let bind_group = Self::build_bind_group(
            &device,
            &bind_group_layout,
            &uniform_buf,
            &tvram_buf,
            &gvram_buf,
            &palette_buf,
            &output_buf,
            &cynthia_meta_buf,
            &cynthia_pcg_buf,
        );

        Self {
            device,
            queue,
            pipeline,
            bind_group_layout,
            bind_group,
            uniform_buf,
            tvram_buf,
            gvram_buf,
            palette_buf,
            cynthia_meta_buf,
            cynthia_pcg_buf,
            output_buf,
            staging_buf,
            adapter_info,
            max_width,
            max_height,
            max_tvram_bytes,
            vram_dirty_seeded: false,
            prev_cursor_pages: 0,
            ext_tvram_was_active: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform_buf: &wgpu::Buffer,
        tvram_buf: &wgpu::Buffer,
        gvram_buf: &wgpu::Buffer,
        palette_buf: &wgpu::Buffer,
        output_buf: &wgpu::Buffer,
        cynthia_meta_buf: &wgpu::Buffer,
        cynthia_pcg_buf: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vicon_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: tvram_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: gvram_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: palette_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: cynthia_meta_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: cynthia_pcg_buf.as_entire_binding(),
                },
            ],
        })
    }

    fn ensure_dynamic_capacities(&mut self, palette_bytes: u64, meta_bytes: u64, pcg_bytes: u64) {
        let mut rebuilt = false;
        let grow = |buf: &mut wgpu::Buffer, label: &str, needed: u64, rebuilt: &mut bool| {
            if needed > buf.size() {
                *buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: needed.next_power_of_two(),
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                *rebuilt = true;
            }
        };
        grow(&mut self.palette_buf, "vicon_palette_buf", palette_bytes, &mut rebuilt);
        grow(&mut self.cynthia_meta_buf, "vicon_cynthia_meta_buf", meta_bytes, &mut rebuilt);
        grow(&mut self.cynthia_pcg_buf, "vicon_cynthia_pcg_buf", pcg_bytes, &mut rebuilt);
        if rebuilt {
            self.bind_group = Self::build_bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.uniform_buf,
                &self.tvram_buf,
                &self.gvram_buf,
                &self.palette_buf,
                &self.output_buf,
                &self.cynthia_meta_buf,
                &self.cynthia_pcg_buf,
            );
        }
    }

    /// Returns a reference to the underlying `wgpu::Device`.
    #[inline]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Returns a reference to the underlying `wgpu::Queue`.
    #[inline]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Creates a [`ViconSurfacePresenter`] that blits the composited VICON framebuffer directly
    /// onto a window `wgpu::Surface` of the given `surface_format` without CPU readback.
    pub fn create_surface_presenter(
        &self,
        surface_format: wgpu::TextureFormat,
    ) -> ViconSurfacePresenter {
        let present_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("msx68k_vicon_present_shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
            });

        let layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("vicon_present_bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Storage { read_only: true },
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

        let pipeline_layout =
            self.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("vicon_present_pipeline_layout"),
                    bind_group_layouts: &[&layout],
                    push_constant_ranges: &[],
                });

        let pipeline =
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("vicon_present_pipeline"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &present_shader,
                        entry_point: Some("vs_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &present_shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: surface_format,
                            blend: Some(wgpu::BlendState::REPLACE),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: Default::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        ..Default::default()
                    },
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vicon_present_bind_group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.output_buf.as_entire_binding(),
                },
            ],
        });

        ViconSurfacePresenter {
            pipeline,
            bind_group,
            layout,
            layers: LayerPipeline::new(&self.device, surface_format),
        }
    }

    /// Draws the host mouse cursor over the uploaded TVRAM copy and returns the touched page mask.
    fn upload_cursor_overlay(&self, bus: &X68kBus, tvram_len: usize) -> u128 {
        let mut touched = DirtyVramPages::default();
        for (offset, bytes) in bus.cursor_overlay() {
            if offset + bytes.len() <= tvram_len {
                self.queue
                    .write_buffer(&self.tvram_buf, offset as u64, &bytes);
                touched.mark_tvram_range(offset..offset + bytes.len());
            }
        }
        touched.tvram
    }

    fn upload_raster_and_cynthia(&mut self, bus: &X68kBus) -> ViconUniforms {
        let mut uniforms = ViconUniforms::from_bus(bus);
        uniforms.width = uniforms.width.min(self.max_width);
        uniforms.height = uniforms.height.min(self.max_height);
        let raster_words = raster::pack_raster_input(bus, &mut uniforms);
        let (cynthia_meta, cynthia_pcg) = {
            let cynthia = raster::cynthia_input(bus, &uniforms);
            if cynthia.is_empty() {
                (Vec::new(), Vec::new())
            } else {
                uniforms.reserved[1] |= raster::FLAG_CYNTHIA_ENABLED;
                raster::pack_cynthia_gpu_input(&cynthia)
            }
        };
        self.ensure_dynamic_capacities(
            (raster_words.len() * 4) as u64,
            (cynthia_meta.len() * 4) as u64,
            (cynthia_pcg.len() * 4) as u64,
        );
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
        if !raster_words.is_empty() {
            self.queue
                .write_buffer(&self.palette_buf, 0, bytemuck::cast_slice(&raster_words));
        }
        if !cynthia_meta.is_empty() {
            self.queue
                .write_buffer(&self.cynthia_meta_buf, 0, bytemuck::cast_slice(&cynthia_meta));
        }
        if !cynthia_pcg.is_empty() {
            self.queue
                .write_buffer(&self.cynthia_pcg_buf, 0, bytemuck::cast_slice(&cynthia_pcg));
        }
        uniforms
    }

    fn upload_bus_buffers_full(&mut self, bus: &X68kBus, zero_if_none: bool) -> ViconUniforms {
        let uniforms = self.upload_raster_and_cynthia(bus);

        if let Some(tvram) = bus.active_tvram() {
            let len = tvram.len().min(self.max_tvram_bytes);
            self.queue.write_buffer(&self.tvram_buf, 0, &tvram[..len]);
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, len);
        } else if zero_if_none {
            let zero = vec![0u8; TVRAM_BYTES];
            self.queue.write_buffer(&self.tvram_buf, 0, &zero);
            self.prev_cursor_pages = 0;
        }

        if let Some(gvram) = bus.gvram() {
            self.queue.write_buffer(&self.gvram_buf, 0, gvram);
        } else if zero_if_none {
            let zero = vec![0u8; GVRAM_BYTES];
            self.queue.write_buffer(&self.gvram_buf, 0, &zero);
        }
        self.vram_dirty_seeded = false;

        uniforms
    }

    fn upload_bus_buffers_dirty(&mut self, bus: &mut X68kBus) -> ViconUniforms {
        let uniforms = self.upload_raster_and_cynthia(bus);

        let mut written = bus.take_written_vram_pages();
        if !self.vram_dirty_seeded {
            written = DirtyVramPages::ALL;
            self.vram_dirty_seeded = true;
        }

        if bus.is_ext_tvram_active() {
            if let Some(tvram) = bus.active_tvram() {
                let len = tvram.len().min(self.max_tvram_bytes);
                self.queue.write_buffer(&self.tvram_buf, 0, &tvram[..len]);
                let _ = self.upload_cursor_overlay(bus, len);
            }
            self.ext_tvram_was_active = true;
            self.prev_cursor_pages = 0;
        } else {
            if self.ext_tvram_was_active {
                written.tvram = u128::MAX;
                self.ext_tvram_was_active = false;
            }
            let tvram_pages = DirtyVramPages {
                tvram: written.tvram | self.prev_cursor_pages,
                gvram: 0,
            };
            if let Some(tvram) = bus.tvram() {
                for range in tvram_pages.tvram_ranges() {
                    let end = range.end.min(self.max_tvram_bytes).min(tvram.len());
                    if range.start < end {
                        self.queue
                            .write_buffer(&self.tvram_buf, range.start as u64, &tvram[range.start..end]);
                    }
                }
            }
            self.prev_cursor_pages = self.upload_cursor_overlay(bus, self.max_tvram_bytes);
        }

        if let Some(gvram) = bus.gvram() {
            for range in written.gvram_ranges() {
                self.queue
                    .write_buffer(&self.gvram_buf, range.start as u64, &gvram[range.clone()]);
            }
        }

        uniforms
    }

    fn dispatch_and_present(
        &self,
        uniforms: ViconUniforms,
        presenter: &ViconSurfacePresenter,
        target_view: &wgpu::TextureView,
        surface_width: u32,
        surface_height: u32,
        layers: &[VmLayer<'_>],
    ) {
        let viewport = letterbox(uniforms.width, uniforms.height, surface_width, surface_height);
        let layers = presenter.layers.prepare(
            &self.device,
            &self.output_buf,
            layers,
            &Placement {
                viewport,
                screen: (uniforms.width, uniforms.height),
                target: (surface_width, surface_height),
            },
        );
        let _ = &presenter.layout;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vicon_surface_render_encoder"),
            });

        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vicon_compute_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.pipeline);
            cpass.set_bind_group(0, &self.bind_group, &[]);
            cpass.dispatch_workgroups(
                uniforms.width.div_ceil(16),
                uniforms.height.div_ceil(16),
                1,
            );
        }

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vicon_present_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            let [vp_x, vp_y, vp_w, vp_h] = viewport;
            rpass.set_viewport(vp_x, vp_y, vp_w, vp_h, 0.0, 1.0);
            rpass.set_pipeline(&presenter.pipeline);
            rpass.set_bind_group(0, &presenter.bind_group, &[]);
            rpass.draw(0..3, 0..1);

            // Layer quads are placed in target coordinates.
            rpass.set_viewport(0.0, 0.0, surface_width.max(1) as f32, surface_height.max(1) as f32, 0.0, 1.0);
            presenter.layers.draw(&mut rpass, &layers);
        }

        self.queue.submit(Some(encoder.finish()));
    }

    /// Composites `bus` video memory on the GPU (full VRAM upload) and blits into `target_view`.
    pub fn render_bus_to_surface_view(
        &mut self,
        bus: &X68kBus,
        presenter: &ViconSurfacePresenter,
        target_view: &wgpu::TextureView,
        surface_width: u32,
        surface_height: u32,
        layers: &[VmLayer<'_>],
    ) {
        let uniforms = self.upload_bus_buffers_full(bus, false);
        self.dispatch_and_present(
            uniforms,
            presenter,
            target_view,
            surface_width,
            surface_height,
            layers,
        );
    }

    /// Composites `bus` video memory on the GPU using dirty 4 KB VRAM page uploads and blits into `target_view`.
    pub fn render_bus_to_surface_view_dirty(
        &mut self,
        bus: &mut X68kBus,
        presenter: &ViconSurfacePresenter,
        target_view: &wgpu::TextureView,
        surface_width: u32,
        surface_height: u32,
        layers: &[VmLayer<'_>],
    ) {
        let uniforms = self.upload_bus_buffers_dirty(bus);
        self.dispatch_and_present(
            uniforms,
            presenter,
            target_view,
            surface_width,
            surface_height,
            layers,
        );
    }

    /// Returns information about the active `wgpu` adapter.
    #[inline]
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    fn execute_and_readback(&self, uniforms: ViconUniforms) -> RenderedFrame {
        let byte_len = (uniforms.width * uniforms.height * 4) as u64;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vicon_render_encoder"),
            });

        {
            let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vicon_compute_pass"),
                timestamp_writes: None,
            });
            cpass.set_pipeline(&self.pipeline);
            cpass.set_bind_group(0, &self.bind_group, &[]);
            cpass.dispatch_workgroups(
                uniforms.width.div_ceil(16),
                uniforms.height.div_ceil(16),
                1,
            );
        }

        encoder.copy_buffer_to_buffer(&self.output_buf, 0, &self.staging_buf, 0, byte_len);
        self.queue.submit(Some(encoder.finish()));

        let slice = self.staging_buf.slice(..byte_len);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let data = slice.get_mapped_range();
        let mut rgba8 = data.to_vec();
        drop(data);
        // Alpha marks where the text plane is clear, for VM layers only: frames are opaque.
        for pixel in rgba8.chunks_exact_mut(4) {
            pixel[3] = 0xFF;
        }
        self.staging_buf.unmap();

        RenderedFrame {
            width: uniforms.width,
            height: uniforms.height,
            rgba8,
        }
    }

    /// Uploads the current `X68kBus` video state to the GPU, executes the WGSL compositor shader,
    /// and reads back the rendered `RGBA8` frame.
    pub fn render_bus_frame(&mut self, bus: &X68kBus) -> RenderedFrame {
        let uniforms = self.upload_bus_buffers_full(bus, true);
        self.execute_and_readback(uniforms)
    }

    /// Uploads only dirty 4 KB VRAM pages (plus cursor overlay restoration), executes the WGSL
    /// compositor shader, and reads back the rendered `RGBA8` frame.
    pub fn render_bus_frame_dirty(&mut self, bus: &mut X68kBus) -> RenderedFrame {
        let uniforms = self.upload_bus_buffers_dirty(bus);
        self.execute_and_readback(uniforms)
    }
}

/// Aspect-ratio-preserving letterbox/pillarbox viewport `x, y, width, height` of a
/// `width × height` screen on a `surface_width × surface_height` target.
#[cfg(feature = "wgpu-backend")]
fn letterbox(width: u32, height: u32, surface_width: u32, surface_height: u32) -> [f32; 4] {
    let sw = surface_width.max(1) as f32;
    let sh = surface_height.max(1) as f32;
    let target_aspect = (width.max(1) as f32) / (height.max(1) as f32);
    let window_aspect = sw / sh;
    if window_aspect > target_aspect {
        let w = sh * target_aspect;
        [(sw - w) * 0.5, 0.0, w, sh]
    } else {
        let h = sw / target_aspect;
        [0.0, (sh - h) * 0.5, sw, h]
    }
}

/// Returns a copy of `tvram` with the host mouse cursor drawn over it.
fn with_cursor_overlay(bus: &X68kBus, tvram: &[u8]) -> Vec<u8> {
    let mut screen = tvram.to_vec();
    for (offset, bytes) in bus.cursor_overlay() {
        if let Some(slot) = screen.get_mut(offset..offset + bytes.len()) {
            slot.copy_from_slice(&bytes);
        }
    }
    screen
}

fn sample_graphics_cpu(
    params: &ViconUniforms,
    gvram: Option<&[u8]>,
    palette: &[u16; 256],
    x: u32,
    y: u32,
    want_sub: bool,
) -> (u16, u16) {
    let enabled = if params.r0_mode & 7 >= 4 {
        params.r2_display & 16
    } else {
        params.r2_display & 15
    };
    if enabled == 0 {
        return (0, 0);
    }
    let nibble = |slot: u32, x: u32, y: u32, scroll: bool| -> u16 {
        let page = (params.r1_priority >> (slot * 2)) & 3;
        let gx = (x + if scroll {
            params.gr_scroll_x[page as usize]
        } else {
            0
        }) & 511;
        let gy = (y + if scroll {
            params.gr_scroll_y[page as usize]
        } else {
            0
        }) & 511;
        let addr = ((gy * 512 + gx) * 2) as usize;
        let word = gvram.map_or(0, |bytes| {
            u16::from_be_bytes([bytes[addr], bytes[addr + 1]])
        });
        (word >> (page * 4)) & 15
    };
    let mode = params.r0_mode & 7;
    let mut index = 0u16;
    let mut sub_index = 0u16;
    if mode >= 4 {
        if params.r2_display & 16 == 0 {
            return (0, 0);
        }
        let gx = (x + params.gr_scroll_x[0]) & 1023;
        let gy = (y + params.gr_scroll_y[0]) & 1023;
        index = nibble((gy >> 9) * 2 + (gx >> 9), gx, gy, false);
    } else {
        let lanes = if mode == 0 {
            1
        } else if mode == 1 {
            2
        } else {
            4
        };
        for first in (0..4).step_by(lanes) {
            if params.r2_display & (((1 << lanes) - 1) << first) == 0 {
                continue;
            }
            let mut candidate = 0;
            for lane in 0..lanes {
                candidate |= nibble((first + lane) as u32, x, y, true) << (lane * 4);
            }
            if candidate != 0 {
                if index == 0 {
                    index = candidate;
                    if !want_sub || lanes == 4 {
                        break;
                    }
                } else {
                    sub_index = candidate;
                    break;
                }
            }
        }
    }
    if mode == 2 || mode == 3 {
        let low = (index & 255) as usize;
        let high = (index >> 8) as usize;
        let lo_word = palette[low & !1];
        let hi_word = palette[(high & !1) + 1];
        let lo = if low & 1 == 0 {
            lo_word >> 8
        } else {
            lo_word & 255
        };
        let hi = if high & 1 == 0 {
            hi_word & 0xff00
        } else {
            hi_word << 8
        };
        (hi | lo, 0)
    } else {
        let top = palette[index as usize];
        let sub = if sub_index != 0 {
            palette[sub_index as usize]
        } else {
            0
        };
        (top, sub)
    }
}

#[inline]
fn grbi_pair_to_rgba(fg: u16, bg: u16, blend: bool, contrast: u32) -> [u8; 4] {
    let i_fg = (fg & 1) as u32;
    let mut g6 = ((((fg >> 11) & 0x1F) as u32) << 1) | i_fg;
    let mut r6 = ((((fg >> 6) & 0x1F) as u32) << 1) | i_fg;
    let mut b6 = ((((fg >> 1) & 0x1F) as u32) << 1) | i_fg;
    if blend {
        let i_bg = (bg & 1) as u32;
        let g6_bg = ((((bg >> 11) & 0x1F) as u32) << 1) | i_bg;
        let r6_bg = ((((bg >> 6) & 0x1F) as u32) << 1) | i_bg;
        let b6_bg = ((((bg >> 1) & 0x1F) as u32) << 1) | i_bg;
        g6 = (g6 + g6_bg) >> 1;
        r6 = (r6 + r6_bg) >> 1;
        b6 = (b6 + b6_bg) >> 1;
    }
    let r8 = (((r6 << 2) | (r6 >> 4)) * contrast / 15) as u8;
    let g8 = (((g6 << 2) | (g6 >> 4)) * contrast / 15) as u8;
    let b8 = (((b6 << 2) | (b6 >> 4)) * contrast / 15) as u8;
    [r8, g8, b8, 0xFF]
}

/// CPU reference compositor evaluating the shared CYNTHIA and VICON specifications.
pub fn render_bus_frame_cpu(bus: &X68kBus) -> RenderedFrame {
    let mut params = ViconUniforms::from_bus(bus);
    let frame = raster::completed_frame(bus, &params);
    let cynthia = raster::cynthia_input(bus, &params);
    let interlace = raster::interlace_field(bus, &params);
    let mut next_line = 0;
    let width = params.width;
    let height = params.height;
    let mut rgba8 = vec![0u8; (width * height * 4) as usize];

    let tvram = bus.active_tvram().map(|tv| with_cursor_overlay(bus, tv));
    let tvram = tvram.as_deref();
    let row_bytes = params.tvram_row_bytes as usize;
    let plane_bytes = params.tvram_plane_bytes as usize;
    let gvram = bus.gvram();
    let mut tx_pal = bus.vicon.text_palette();
    let mut gr_pal = bus.vicon.graphic_palette();
    let mut cynthia_row = [0u8; msx68k_bus::bg::BG_WIDTH];

    for y in 0..height {
        if let Some(frame) = frame {
            while let Some(line) = frame.lines.get(next_line).filter(|line| line.y <= y) {
                params.apply_raster(&line.state);
                let pal = frame.palette(line.state.palette_id);
                tx_pal = pal[..256].try_into().unwrap();
                gr_pal = pal[256..].try_into().unwrap();
                next_line += 1;
            }
        }
        let text_enabled = (params.r2_display & 0x0020) != 0;
        let contrast = params.contrast;
        let tx_rank = (params.r1_priority >> 10) & 3;
        let sp_rank = (params.r1_priority >> 12) & 3;
        let gr_rank = (params.r1_priority >> 8) & 3;
        let exon = (params.r2_display & 0x1000) != 0;
        let hp = (params.r2_display & 0x0800) != 0;
        let bp = (params.r2_display & 0x0400) != 0;
        let gg = (params.r2_display & 0x0200) != 0;
        let gt = (params.r2_display & 0x0100) != 0;
        let want_gr_sub = exon && hp && gg;
        let cynthia_on = (params.r2_display & 0x40 != 0) && gr_rank != 3;
        let dim_row = interlace.is_some_and(|field| ((y & 1) as u8) != field);
        let cynthia_h_scale = if cynthia_on {
            cynthia.row_h_scale(y)
        } else {
            0
        };
        if cynthia_on {
            cynthia.rasterize_row(y, &mut cynthia_row);
        }

        for x in 0..width {
            let mut tx_idx = 0usize;
            if text_enabled {
                if let Some(tv) = tvram {
                    let (tx, ty) = if row_bytes == 128 && plane_bytes == TVRAM_PLANE_BYTES {
                        (
                            (x + params.text_scroll_x) & 1023,
                            (y + params.text_scroll_y) & 1023,
                        )
                    } else {
                        (x, y)
                    };
                    let in_plane = (ty as usize) * row_bytes + ((tx as usize) >> 3);
                    if 3 * plane_bytes + in_plane < tv.len() {
                        let bit = 7 - (tx & 7);
                        let b0 = ((tv[in_plane] >> bit) & 1) as usize;
                        let b1 = ((tv[plane_bytes + in_plane] >> bit) & 1) as usize;
                        let b2 = ((tv[2 * plane_bytes + in_plane] >> bit) & 1) as usize;
                        let b3 = ((tv[3 * plane_bytes + in_plane] >> bit) & 1) as usize;
                        tx_idx = b0 | (b1 << 1) | (b2 << 2) | (b3 << 3);
                    }
                }
            }

            let (gr_color, gr_sub) = sample_graphics_cpu(&params, gvram, gr_pal, x, y, want_gr_sub);
            let tx_color = if text_enabled && gr_rank != 3 {
                tx_pal[tx_idx]
            } else {
                0
            };
            let bg_code = if cynthia_on {
                let cx = msx68k_bus::bg::map_cynthia_x(x, cynthia_h_scale) as usize;
                if cx < cynthia_row.len() {
                    cynthia_row[cx]
                } else {
                    0
                }
            } else {
                0
            };
            let bg_color = if bg_code != 0 && gr_rank != 3 {
                tx_pal[bg_code as usize]
            } else {
                0
            };
            let (front, back) = if tx_rank <= sp_rank {
                (tx_color, bg_color)
            } else {
                (bg_color, tx_color)
            };

            let (fg_grbi, bg_grbi, do_blend) = if !exon {
                let colors = match gr_rank {
                    0 => [gr_color, front, back],
                    1 => [front, gr_color, back],
                    2 => [front, back, gr_color],
                    _ => [gr_color, 0, 0],
                };
                let c = colors.into_iter().find(|&color| color != 0).unwrap_or(0);
                (c, 0, false)
            } else if !hp {
                // Special priority: Graphic pixel with I bit == 1 promotes above Text/Sprite.
                if gr_color != 0 && (gr_color & 1) != 0 {
                    (gr_color, 0, false)
                } else {
                    let colors = match gr_rank {
                        0 => [gr_color, front, back],
                        1 => [front, gr_color, back],
                        2 => [front, back, gr_color],
                        _ => [gr_color, 0, 0],
                    };
                    let c = colors.into_iter().find(|&color| color != 0).unwrap_or(0);
                    (c, 0, false)
                }
            } else {
                // Half-transparency (translucent) mode.
                let ordered = match gr_rank {
                    0 => [(gr_color, true), (front, false), (back, false)],
                    1 => [(front, false), (gr_color, true), (back, false)],
                    2 => [(front, false), (back, false), (gr_color, true)],
                    _ => [(gr_color, true), (0, false), (0, false)],
                };
                let mut top = 0u16;
                let mut top_is_gr = false;
                let mut second = 0u16;
                for (c, is_gr) in ordered {
                    if c != 0 {
                        if top == 0 {
                            top = c;
                            top_is_gr = is_gr;
                        } else {
                            second = c;
                            break;
                        }
                    }
                }
                if top == 0 {
                    (0, 0, false)
                } else {
                    let trigger = bp || (top & 1) != 0;
                    if top_is_gr {
                        if trigger && gg && gr_sub != 0 {
                            (top, gr_sub, true)
                        } else if trigger && gt {
                            (top, second, true)
                        } else if trigger && gg {
                            (top, 0, true)
                        } else {
                            (top, 0, false)
                        }
                    } else if trigger && gt {
                        (top, second, true)
                    } else {
                        (top, 0, false)
                    }
                }
            };

            let mut rgba = grbi_pair_to_rgba(fg_grbi, bg_grbi, do_blend, contrast);
            if dim_row {
                rgba[0] = ((rgba[0] as u32 * 3) >> 2) as u8;
                rgba[1] = ((rgba[1] as u32 * 3) >> 2) as u8;
                rgba[2] = ((rgba[2] as u32 * 3) >> 2) as u8;
            }
            let out_off = ((y * width + x) * 4) as usize;
            rgba8[out_off..out_off + 4].copy_from_slice(&rgba);
        }
    }

    RenderedFrame {
        width,
        height,
        rgba8,
    }
}

/// Decodes visible 8×16 ANK ASCII text lines from TVRAM by matching 8×16 cells against the
/// X68000 CGROM 8×16 font at `$F3A800` (offset `0x03A800` in `bus.rom()`).
pub fn decode_tvram_ank_lines(bus: &X68kBus, max_cols: usize, max_rows: usize) -> Vec<String> {
    let Some(tvram) = bus.active_tvram() else {
        return Vec::new();
    };
    let rom = bus.rom();
    let font_base = 0x03A800usize;
    if rom.len() < font_base + 256 * 16 {
        return Vec::new();
    }

    let scroll_y = bus.crtc.text_scroll_y() as usize;
    let plane_bytes = bus.active_tvram_plane_bytes();
    let row_bytes = bus.active_tvram_row_bytes();
    let cols = max_cols.min(96);
    let rows = max_rows.min(32);
    let mut lines = Vec::with_capacity(rows);

    for row in 0..rows {
        let mut line = String::with_capacity(cols);
        for col in 0..cols {
            let mut cell = [0u8; 16];
            let mut any_bits = false;
            for (dy, slot) in cell.iter_mut().enumerate() {
                let y = if row_bytes == 128 && plane_bytes == TVRAM_PLANE_BYTES {
                    (scroll_y + row * 16 + dy) & 1023
                } else {
                    row * 16 + dy
                };
                let off = y * row_bytes + col;
                if 3 * plane_bytes + off < tvram.len() {
                    // Combine planes 0..3 so text drawn in any foreground color (1..15) is matched
                    let combined = tvram[off]
                        | tvram[plane_bytes + off]
                        | tvram[2 * plane_bytes + off]
                        | tvram[3 * plane_bytes + off];
                    *slot = combined;
                    if combined != 0 {
                        any_bits = true;
                    }
                }
            }

            if !any_bits {
                line.push(' ');
                continue;
            }

            // Match against printable ASCII 0x20..=0x7E in CGROM ($F3A800)
            let mut matched_char: Option<char> = None;
            for code in 0x20u8..=0x7Eu8 {
                let glyph = &rom[font_base + (code as usize) * 16..][..16];
                // Also match inverted glyph (e.g. reverse-video status bar or function keys)
                if cell == glyph || cell.iter().zip(glyph).all(|(&c, &g)| c == !g) {
                    matched_char = Some(code as char);
                    break;
                }
            }
            line.push(matched_char.unwrap_or('#'));
        }
        let trimmed = line.trim_end().to_string();
        lines.push(trimmed);
    }

    lines
}
