//! The screen of a VM running on a thread of its own (`docs/VMX68K.md` Section 3.3).
//!
//! A [`VmScreen`] lives on the VM's thread. At each vertical blank it uploads only the VRAM
//! pages written since the previous one, along with the palette and display registers, and
//! composites into one of two output textures. [`VmScreenOutput`], shared with the main thread,
//! tells which texture holds the latest completed frame.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use msx68k_bus::{DirtyVramPages, X68kBus, GVRAM_BYTES, TVRAM_BYTES};

use crate::{RenderedFrame, ViconUniforms};

/// Side of the square output textures, which covers every X68000 screen mode.
pub const VM_SCREEN_MAX: u32 = 1024;

const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A VM screen's two output textures, shared between its VM thread and the main thread.
pub struct VmScreenOutput {
    textures: [wgpu::Texture; 2],
    views: [wgpu::TextureView; 2],
    /// Size of the frame in each texture, as `width << 16 | height`.
    sizes: [AtomicU32; 2],
    /// Frames completed so far; the latest is in texture `(completed - 1) % 2`.
    completed: AtomicU64,
}

/// The latest completed frame of a VM screen.
pub struct CompletedFrame<'a> {
    /// Its number, counting from 1.
    pub number: u64,
    pub width: u32,
    pub height: u32,
    /// The whole output texture; the frame is its top-left `width × height`.
    pub texture: &'a wgpu::Texture,
    pub view: &'a wgpu::TextureView,
}

impl VmScreenOutput {
    /// Returns the latest completed frame, or `None` before the first one.
    pub fn completed(&self) -> Option<CompletedFrame<'_>> {
        let number = self.completed.load(Ordering::Acquire);
        let index = (number.checked_sub(1)? % 2) as usize;
        let size = self.sizes[index].load(Ordering::Relaxed);
        Some(CompletedFrame {
            number,
            width: size >> 16,
            height: size & 0xFFFF,
            texture: &self.textures[index],
            view: &self.views[index],
        })
    }

    /// Reads the latest completed frame back from the GPU, for tests.
    pub fn read_back(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<RenderedFrame> {
        let frame = self.completed()?;
        read_back_texture(device, queue, frame.texture, frame.width, frame.height)
    }
}

/// Reads the top-left `width × height` RGBA8 pixels of `texture` back from the GPU, for tests.
pub fn read_back_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Option<RenderedFrame> {
    let row_bytes = width * 4;
    let padded = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("read_back_texture"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv().ok()?.ok()?;
    let data = slice.get_mapped_range();
    let rgba8 = data
        .chunks_exact(padded as usize)
        .flat_map(|row| &row[..row_bytes as usize])
        .copied()
        .collect();
    Some(RenderedFrame {
        width,
        height,
        rgba8,
    })
}

/// Composites one VM's screen on the GPU, on the VM's thread.
pub struct VmScreen {
    queue: wgpu::Queue,
    device: wgpu::Device,
    pipeline: wgpu::ComputePipeline,
    /// One per output texture.
    bind_groups: [wgpu::BindGroup; 2],
    uniform_buf: wgpu::Buffer,
    tvram_buf: wgpu::Buffer,
    gvram_buf: wgpu::Buffer,
    palette_buf: wgpu::Buffer,
    output: Arc<VmScreenOutput>,
}

impl VmScreen {
    /// Creates a screen on `device`, which the main thread shares for its final pass.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("msx68k_vm_screen_shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(include_str!("vicon.wgsl"), include_str!("screen.wgsl")).into(),
            ),
        });
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniform_buf = buffer(
            "vm_screen_uniforms",
            std::mem::size_of::<ViconUniforms>() as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let tvram_buf = buffer("vm_screen_tvram", TVRAM_BYTES as u64, wgpu::BufferUsages::STORAGE);
        let gvram_buf = buffer("vm_screen_gvram", GVRAM_BYTES as u64, wgpu::BufferUsages::STORAGE);
        let palette_buf = buffer("vm_screen_palettes", 512 * 4, wgpu::BufferUsages::STORAGE);

        let textures = [0, 1].map(|_| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("vm_screen_output"),
                size: wgpu::Extent3d {
                    width: VM_SCREEN_MAX,
                    height: VM_SCREEN_MAX,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OUTPUT_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let views = [0, 1].map(|i| textures[i].create_view(&Default::default()));

        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vm_screen_bind_group_layout"),
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
                storage(1),
                storage(2),
                storage(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: OUTPUT_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let bind_groups = [0, 1].map(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vm_screen_bind_group"),
                layout: &layout,
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
                        resource: wgpu::BindingResource::TextureView(&views[i]),
                    },
                ],
            })
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vm_screen_pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vm_screen_pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        Self {
            queue: queue.clone(),
            device: device.clone(),
            pipeline,
            bind_groups,
            uniform_buf,
            tvram_buf,
            gvram_buf,
            palette_buf,
            output: Arc::new(VmScreenOutput {
                textures,
                views,
                sizes: [AtomicU32::new(0), AtomicU32::new(0)],
                completed: AtomicU64::new(0),
            }),
        }
    }

    /// Returns the output to hand to the main thread.
    pub fn output(&self) -> Arc<VmScreenOutput> {
        Arc::clone(&self.output)
    }

    /// Uploads what changed in `bus` since the previous frame, composites a new frame, and
    /// returns the bytes uploaded. The first frame uploads all of VRAM.
    pub fn present(&mut self, bus: &mut X68kBus) -> usize {
        let mut written = bus.take_written_vram_pages();
        let number = self.output.completed.load(Ordering::Relaxed);
        if number == 0 {
            written = DirtyVramPages::ALL;
        }

        let mut uniforms = ViconUniforms::from_bus(bus);
        uniforms.width = uniforms.width.min(VM_SCREEN_MAX);
        uniforms.height = uniforms.height.min(VM_SCREEN_MAX);
        let mut palettes = [0u32; 512];
        for (slot, &c) in palettes.iter_mut().zip(bus.vicon.text_palette()) {
            *slot = c as u32;
        }
        for (slot, &c) in palettes[256..].iter_mut().zip(bus.vicon.graphic_palette()) {
            *slot = c as u32;
        }
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
        self.queue
            .write_buffer(&self.palette_buf, 0, bytemuck::cast_slice(&palettes));
        let mut uploaded = std::mem::size_of_val(&uniforms) + std::mem::size_of_val(&palettes);

        for (memory, buf, ranges) in [
            (bus.tvram(), &self.tvram_buf, written.tvram_ranges()),
            (bus.gvram(), &self.gvram_buf, written.gvram_ranges()),
        ] {
            // Memory the guest has not touched yet is still zero on both sides.
            let Some(memory) = memory else { continue };
            for range in ranges {
                self.queue
                    .write_buffer(buf, range.start as u64, &memory[range.clone()]);
                uploaded += range.len();
            }
        }

        let index = (number % 2) as usize;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vm_screen_encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("vm_screen_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_groups[index], &[]);
            pass.dispatch_workgroups(uniforms.width.div_ceil(16), uniforms.height.div_ceil(16), 1);
        }
        self.queue.submit(Some(encoder.finish()));

        self.output.sizes[index].store(uniforms.width << 16 | uniforms.height, Ordering::Relaxed);
        self.output.completed.store(number + 1, Ordering::Release);
        uploaded
    }
}

/// A rectangle of the primary VM's screen in its pixels, right and bottom exclusive.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// A VM's screen drawn into the primary VM's screen where its window shows it, under the
/// primary's text plane: only where the text is clear.
pub struct VmLayer<'a> {
    pub output: &'a VmScreenOutput,
    /// Position of the frame's top-left pixel on the primary VM's screen.
    pub left: i32,
    pub top: i32,
    /// Visible region of the window's content rectangle.
    pub visible: &'a [ScreenRect],
}

/// Where the primary VM's screen lands on the render target.
#[derive(Debug, Copy, Clone)]
pub(crate) struct Placement {
    /// Viewport `x, y, width, height` on the target.
    pub viewport: [f32; 4],
    /// Size of the primary VM's screen.
    pub screen: (u32, u32),
    /// Size of the render target.
    pub target: (u32, u32),
}

impl Placement {
    fn to_target(self, x: i32, y: i32) -> (f32, f32) {
        let [vx, vy, vw, vh] = self.viewport;
        (
            vx + x as f32 * vw / self.screen.0.max(1) as f32,
            vy + y as f32 * vh / self.screen.1.max(1) as f32,
        )
    }

    /// The part of `rect` on the target as a scissor rectangle, or `None` if nothing shows.
    fn scissor(&self, rect: &ScreenRect) -> Option<[u32; 4]> {
        let (x0, y0) = self.to_target(rect.left, rect.top);
        let (x1, y1) = self.to_target(rect.right, rect.bottom);
        let clamp = |v: f32, max: u32| (v.round().max(0.0) as u32).min(max);
        let (x0, x1) = (clamp(x0, self.target.0), clamp(x1, self.target.0));
        let (y0, y1) = (clamp(y0, self.target.1), clamp(y1, self.target.1));
        (x0 < x1 && y0 < y1).then_some([x0, y0, x1 - x0, y1 - y0])
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniforms {
    dest: [f32; 4],
    size: [u32; 2],
    origin: [i32; 2],
    primary: [u32; 2],
    _pad: [u32; 2],
}

/// Draws [`VmLayer`]s in the final pass.
pub(crate) struct LayerPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

/// A layer ready to draw: its bindings and scissor rectangles.
pub(crate) struct PreparedLayer {
    bind_group: wgpu::BindGroup,
    scissors: Vec<[u32; 4]>,
}

impl LayerPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("msx68k_vm_layer_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("layer.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vm_layer_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vm_layer_pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vm_layer_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
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
        Self { pipeline, layout }
    }

    /// Binds each layer's latest completed frame, and the primary's composited screen `primary`
    /// whose clear text it shows through. Layers with no frame yet, or nothing visible, are
    /// left out.
    pub fn prepare(
        &self,
        device: &wgpu::Device,
        primary: &wgpu::Buffer,
        layers: &[VmLayer<'_>],
        placement: &Placement,
    ) -> Vec<PreparedLayer> {
        use wgpu::util::DeviceExt;

        let (tw, th) = (placement.target.0.max(1) as f32, placement.target.1.max(1) as f32);
        layers
            .iter()
            .filter_map(|layer| {
                let frame = layer.output.completed()?;
                let scissors: Vec<_> =
                    layer.visible.iter().filter_map(|r| placement.scissor(r)).collect();
                if scissors.is_empty() {
                    return None;
                }
                let (x0, y0) = placement.to_target(layer.left, layer.top);
                let (x1, y1) = placement.to_target(
                    layer.left + frame.width as i32,
                    layer.top + frame.height as i32,
                );
                let uniforms = LayerUniforms {
                    dest: [x0 / tw * 2.0 - 1.0, 1.0 - y0 / th * 2.0, x1 / tw * 2.0 - 1.0, 1.0 - y1 / th * 2.0],
                    size: [frame.width, frame.height],
                    origin: [layer.left, layer.top],
                    primary: [placement.screen.0, placement.screen.1],
                    _pad: [0; 2],
                };
                let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vm_layer_uniforms"),
                    contents: bytemuck::bytes_of(&uniforms),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("vm_layer_bind_group"),
                    layout: &self.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(frame.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: primary.as_entire_binding(),
                        },
                    ],
                });
                Some(PreparedLayer { bind_group, scissors })
            })
            .collect()
    }

    /// Draws `layers` in order, each once per scissor rectangle.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, layers: &[PreparedLayer]) {
        if layers.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        for layer in layers {
            pass.set_bind_group(0, &layer.bind_group, &[]);
            for &[x, y, w, h] in &layer.scissors {
                pass.set_scissor_rect(x, y, w, h);
                pass.draw(0..6, 0..1);
            }
        }
    }
}
