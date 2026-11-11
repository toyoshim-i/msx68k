//! X68000 VICON GPU compositor and TVRAM/GVRAM/Palette renderer powered by `wgpu`.
//!
//! Provides:
//! - [`ViconGpuCompositor`]: Hardware-accelerated `wgpu` compute pipeline that composites the
//!   4-plane TVRAM (`$E00000`–`$E7FFFF`), GVRAM (`$C00000`–`$DFFFFF`), and 16-bit `GGGGGRRRRRBBBBBI`
//!   Text/Graphics Palettes (`$E82000`–`$E823FF`) according to CRTC scroll and VICON priority rules.
//! - [`VmScreen`]: The same compositing for a VM on a thread of its own, uploading only the VRAM
//!   pages written since the previous frame into one of two output textures.
//! - [`render_bus_frame_cpu`]: Cycle/bit-identical CPU fallback compositor.
//! - [`decode_tvram_ank_lines`]: X68000 CGROM (`$F3A800`, 8×16 ANK) glyph recognizer that reads
//!   the rendered text rows back from the 4-plane TVRAM bitmap (matching X68000 `TextCopy`).

use bytemuck::{Pod, Zeroable};
use msx68k_bus::{X68kBus, GVRAM_BYTES, TVRAM_BYTES, TVRAM_PLANE_BYTES};

mod vm_screen;
pub use vm_screen::{CompletedFrame, VmScreen, VmScreenOutput, VM_SCREEN_MAX};

/// Maximum framebuffer dimension (`3840 × 2160`).
pub const MAX_FB_WIDTH: u32 = 3840;
pub const MAX_FB_HEIGHT: u32 = 2160;

/// Maximum TVRAM buffer bytes (4 bitplanes for 3840×2160 = 4,147,200 bytes).
pub const MAX_TVRAM_BUF_BYTES: usize =
    4 * ((MAX_FB_WIDTH as usize / 8) * (MAX_FB_HEIGHT as usize));

/// Uniform parameters uploaded to the WGSL VICON compositor shader.
#[repr(C)]
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
pub struct ViconUniforms {
    pub width: u32,
    pub height: u32,
    pub text_scroll_x: u32,
    pub text_scroll_y: u32,
    pub gr0_scroll_x: u32,
    pub gr0_scroll_y: u32,
    pub r0_mode: u32,
    pub r1_priority: u32,
    pub r2_display: u32,
    pub contrast: u32,
    pub tvram_row_bytes: u32,
    pub tvram_plane_bytes: u32,
}

impl ViconUniforms {
    /// Extracts the current CRTC, VICON, and System Port display parameters from `bus`.
    pub fn from_bus(bus: &X68kBus) -> Self {
        let (w, h) = bus.crtc.active_resolution();
        let (gr0_x, gr0_y) = bus.crtc.graphic_scroll(0);
        Self {
            width: w.clamp(256, MAX_FB_WIDTH),
            height: h.clamp(128, MAX_FB_HEIGHT),
            text_scroll_x: bus.crtc.text_scroll_x() as u32,
            text_scroll_y: bus.crtc.text_scroll_y() as u32,
            gr0_scroll_x: gr0_x as u32,
            gr0_scroll_y: gr0_y as u32,
            r0_mode: bus.vicon.r0_mode() as u32,
            r1_priority: bus.vicon.r1_priority() as u32,
            r2_display: bus.vicon.r2_display() as u32,
            contrast: (bus.sysport.contrast & 0x0F) as u32,
            tvram_row_bytes: bus.active_tvram_row_bytes() as u32,
            tvram_plane_bytes: bus.active_tvram_plane_bytes() as u32,
        }
    }
}

/// A rendered RGBA8 frame produced by the VICON compositor.
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
}

/// GPU render pipeline that presents the composited VICON framebuffer onto a window `wgpu::Surface`.
pub struct ViconSurfacePresenter {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

/// Hardware-accelerated `wgpu` VICON compositor.
pub struct ViconGpuCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    tvram_buf: wgpu::Buffer,
    gvram_buf: wgpu::Buffer,
    palette_buf: wgpu::Buffer,
    output_buf: wgpu::Buffer,
    staging_buf: wgpu::Buffer,
    adapter_info: wgpu::AdapterInfo,
}

impl ViconGpuCompositor {
    /// Initializes a new `wgpu` VICON compositor, selecting a hardware GPU or software fallback adapter.
    pub fn new() -> Option<Self> {
        pollster::block_on(Self::new_async())
    }

    /// Asynchronously initializes the `wgpu` device, buffers, and WGSL compute pipeline.
    pub async fn new_async() -> Option<Self> {
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

        Some(Self::from_device(device, queue, adapter_info))
    }

    /// Creates a [`ViconGpuCompositor`] using an existing `wgpu::Device` and `wgpu::Queue`
    /// (for example, one initialized alongside a `winit` window `wgpu::Surface`).
    pub fn from_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
    ) -> Self {
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
            size: MAX_TVRAM_BUF_BYTES as u64,
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
            size: (512 * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let max_fb_bytes = (MAX_FB_WIDTH * MAX_FB_HEIGHT * 4) as u64;
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
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
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

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vicon_bind_group"),
            layout: &bind_group_layout,
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
            ],
        });

        Self {
            device,
            queue,
            pipeline,
            bind_group,
            uniform_buf,
            tvram_buf,
            gvram_buf,
            palette_buf,
            output_buf,
            staging_buf,
            adapter_info,
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
        }
    }

    /// Draws the host mouse cursor over the uploaded TVRAM copy.
    fn upload_cursor_overlay(&self, bus: &X68kBus, tvram_len: usize) {
        for (offset, bytes) in bus.cursor_overlay() {
            if offset + bytes.len() <= tvram_len {
                self.queue
                    .write_buffer(&self.tvram_buf, offset as u64, &bytes);
            }
        }
    }

    fn upload_bus_buffers(&mut self, bus: &X68kBus) -> ViconUniforms {
        let uniforms = ViconUniforms::from_bus(bus);
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        if let Some(tvram) = bus.active_tvram() {
            let len = tvram.len().min(MAX_TVRAM_BUF_BYTES);
            self.queue.write_buffer(&self.tvram_buf, 0, &tvram[..len]);
            self.upload_cursor_overlay(bus, len);
        }

        if let Some(gvram) = bus.gvram() {
            self.queue.write_buffer(&self.gvram_buf, 0, gvram);
        }

        let mut palettes = [0u32; 512];
        for (i, &c) in bus.vicon.text_palette().iter().enumerate() {
            palettes[i] = c as u32;
        }
        for (i, &c) in bus.vicon.graphic_palette().iter().enumerate() {
            palettes[256 + i] = c as u32;
        }
        self.queue
            .write_buffer(&self.palette_buf, 0, bytemuck::cast_slice(&palettes));

        uniforms
    }

    /// Composites `bus` video memory on the GPU and blits the result directly into `target_view`
    /// (with aspect-ratio-preserving letterboxing for window dimensions `surface_width × surface_height`).
    pub fn render_bus_to_surface_view(
        &mut self,
        bus: &X68kBus,
        presenter: &ViconSurfacePresenter,
        target_view: &wgpu::TextureView,
        surface_width: u32,
        surface_height: u32,
    ) {
        let uniforms = self.upload_bus_buffers(bus);
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

            // Compute aspect-ratio-preserving letterbox/pillarbox viewport
            let sw = surface_width.max(1) as f32;
            let sh = surface_height.max(1) as f32;
            let target_aspect = (uniforms.width.max(1) as f32) / (uniforms.height.max(1) as f32);
            let window_aspect = sw / sh;

            let (vp_x, vp_y, vp_w, vp_h) = if window_aspect > target_aspect {
                let w = sh * target_aspect;
                ((sw - w) * 0.5, 0.0, w, sh)
            } else {
                let h = sw / target_aspect;
                (0.0, (sh - h) * 0.5, sw, h)
            };

            rpass.set_viewport(vp_x, vp_y, vp_w, vp_h, 0.0, 1.0);
            rpass.set_pipeline(&presenter.pipeline);
            rpass.set_bind_group(0, &presenter.bind_group, &[]);
            rpass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
    }

    /// Returns information about the active `wgpu` adapter.
    #[inline]
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    /// Uploads the current `X68kBus` video state (TVRAM, GVRAM, Palettes, CRTC/VICON registers)
    /// to the GPU, executes the WGSL compositor shader, and reads back the rendered `RGBA8` frame.
    pub fn render_bus_frame(&mut self, bus: &X68kBus) -> RenderedFrame {
        let uniforms = ViconUniforms::from_bus(bus);
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));

        if let Some(tvram) = bus.active_tvram() {
            let len = tvram.len().min(MAX_TVRAM_BUF_BYTES);
            self.queue.write_buffer(&self.tvram_buf, 0, &tvram[..len]);
            self.upload_cursor_overlay(bus, len);
        } else {
            let zero = vec![0u8; TVRAM_BYTES];
            self.queue.write_buffer(&self.tvram_buf, 0, &zero);
        }

        if let Some(gvram) = bus.gvram() {
            self.queue.write_buffer(&self.gvram_buf, 0, gvram);
        } else {
            let zero = vec![0u8; GVRAM_BYTES];
            self.queue.write_buffer(&self.gvram_buf, 0, &zero);
        }

        let mut palettes = [0u32; 512];
        for (i, &c) in bus.vicon.text_palette().iter().enumerate() {
            palettes[i] = c as u32;
        }
        for (i, &c) in bus.vicon.graphic_palette().iter().enumerate() {
            palettes[256 + i] = c as u32;
        }
        self.queue
            .write_buffer(&self.palette_buf, 0, bytemuck::cast_slice(&palettes));

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
        let rgba8 = data.to_vec();
        drop(data);
        self.staging_buf.unmap();

        RenderedFrame {
            width: uniforms.width,
            height: uniforms.height,
            rgba8,
        }
    }
}

/// CPU reference compositor producing the exact same `RGBA8` framebuffer as [`ViconGpuCompositor`].
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

pub fn render_bus_frame_cpu(bus: &X68kBus) -> RenderedFrame {
    let params = ViconUniforms::from_bus(bus);
    let width = params.width;
    let height = params.height;
    let mut rgba8 = vec![0u8; (width * height * 4) as usize];

    let text_enabled = (params.r2_display & 0x0020) != 0;
    let gr0_enabled = (params.r2_display & 0x0001) != 0;
    let contrast = params.contrast.max(1);
    let tx_rank = (params.r1_priority >> 8) & 3;
    let gr_rank = (params.r1_priority >> 10) & 3;
    let text_above_gr = tx_rank <= gr_rank;

    let tvram = bus.active_tvram().map(|tv| with_cursor_overlay(bus, tv));
    let tvram = tvram.as_deref();
    let row_bytes = params.tvram_row_bytes as usize;
    let plane_bytes = params.tvram_plane_bytes as usize;
    let gvram = bus.gvram();
    let tx_pal = bus.vicon.text_palette();
    let gr_pal = bus.vicon.graphic_palette();

    let grbi_to_rgba = |grbi: u16| -> [u8; 4] {
        let i_bit = (grbi & 1) as u32;
        let g6 = ((((grbi >> 11) & 0x1F) as u32) << 1) | i_bit;
        let r6 = ((((grbi >> 6) & 0x1F) as u32) << 1) | i_bit;
        let b6 = ((((grbi >> 1) & 0x1F) as u32) << 1) | i_bit;

        let r8 = (((r6 << 2) | (r6 >> 4)) * contrast / 15) as u8;
        let g8 = (((g6 << 2) | (g6 >> 4)) * contrast / 15) as u8;
        let b8 = (((b6 << 2) | (b6 >> 4)) * contrast / 15) as u8;
        [r8, g8, b8, 0xFF]
    };

    for y in 0..height {
        for x in 0..width {
            let mut tx_idx = 0usize;
            if text_enabled {
                if let Some(tv) = tvram {
                    let (tx, ty) = if row_bytes == 128 && plane_bytes == TVRAM_PLANE_BYTES {
                        ((x + params.text_scroll_x) & 1023, (y + params.text_scroll_y) & 1023)
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

            let mut gr_idx = 0u16;
            if gr0_enabled && x < 512 && y < 512 {
                if let Some(gv) = gvram {
                    let gx = (x + params.gr0_scroll_x) & 511;
                    let gy = (y + params.gr0_scroll_y) & 511;
                    let addr = (((gy * 512) + gx) * 2) as usize;
                    let word = u16::from_be_bytes([gv[addr], gv[addr + 1]]);
                    let mode = params.r0_mode & 0x07;
                    gr_idx = if mode >= 4 {
                        word
                    } else if (mode & 1) == 1 {
                        word & 0x00FF
                    } else {
                        word & 0x000F
                    };
                }
            }

            let mut final_grbi = tx_pal[0];
            if text_above_gr {
                if text_enabled && tx_idx != 0 {
                    final_grbi = tx_pal[tx_idx];
                } else if gr0_enabled && gr_idx != 0 {
                    final_grbi = if (params.r0_mode & 0x07) >= 4 {
                        gr_idx
                    } else {
                        gr_pal[(gr_idx & 0xFF) as usize]
                    };
                }
            } else if gr0_enabled && gr_idx != 0 {
                final_grbi = if (params.r0_mode & 0x07) >= 4 {
                    gr_idx
                } else {
                    gr_pal[(gr_idx & 0xFF) as usize]
                };
            } else if text_enabled && tx_idx != 0 {
                final_grbi = tx_pal[tx_idx];
            }

            let out_off = ((y * width + x) * 4) as usize;
            rgba8[out_off..out_off + 4].copy_from_slice(&grbi_to_rgba(final_grbi));
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
