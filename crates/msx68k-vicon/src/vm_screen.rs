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
        let row_bytes = frame.width * 4;
        let padded = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vm_screen_read_back"),
            size: (padded * frame.height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            frame.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
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
            width: frame.width,
            height: frame.height,
            rgba8,
        })
    }
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
