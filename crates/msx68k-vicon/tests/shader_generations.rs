//! Cross-generation pixel-exact parity tests verifying:
//!   CPU reference == Compute (wgpu) == GLES 2.0 (EGL + GLES 2.0)

use msx68k_bus::{Bus, X68kVm};
#[cfg(feature = "gles2-backend")]
use msx68k_vicon::gles2::Gles2Compositor;
#[cfg(feature = "wgpu-backend")]
use msx68k_vicon::ViconGpuCompositor;
use msx68k_vicon::{
    render_bus_frame_cpu, PixelFormat, RendererConfig, ShaderGeneration, ViconCompositor,
};

const LINE: u32 = 317;

struct Compositors {
    #[cfg(feature = "wgpu-backend")]
    compute: Option<ViconGpuCompositor>,
    #[cfg(feature = "gles2-backend")]
    gles2: Option<Gles2Compositor>,
}

impl Compositors {
    fn new() -> Self {
        Self {
            #[cfg(feature = "wgpu-backend")]
            compute: ViconGpuCompositor::new(),
            #[cfg(feature = "gles2-backend")]
            gles2: {
                let g = Gles2Compositor::new();
                assert!(g.is_some(), "headless EGL + GLES 2.0 must initialize on host");
                g
            },
        }
    }
}

fn write16(vm: &mut X68kVm, addr: u32, val: u16) {
    vm.bus.write_u16(addr, val, true).unwrap();
}

fn assert_all_generations_match(vm: &mut X68kVm, comps: &mut Compositors, label: &str) {
    let cpu = render_bus_frame_cpu(&vm.bus);
    #[cfg(feature = "wgpu-backend")]
    if let Some(gpu) = comps.compute.as_mut() {
        let got = gpu.render_bus_frame(&vm.bus);
        assert_eq!(got.width, cpu.width, "{label}: compute width");
        assert_eq!(got.height, cpu.height, "{label}: compute height");
        assert_eq!(got.rgba8, cpu.rgba8, "{label}: compute vs cpu parity");
    }
    #[cfg(feature = "gles2-backend")]
    if let Some(es2) = comps.gles2.as_mut() {
        let got = es2.render_bus_frame(&vm.bus);
        assert_eq!(got.width, cpu.width, "{label}: gles2 width");
        assert_eq!(got.height, cpu.height, "{label}: gles2 height");
        if got.rgba8 != cpu.rgba8 {
            for y in 0..cpu.height {
                for x in 0..cpu.width {
                    let off = ((y * cpu.width + x) * 4) as usize;
                    let a = &got.rgba8[off..off + 4];
                    let b = &cpu.rgba8[off..off + 4];
                    assert_eq!(a, b, "{label}: gles2 mismatch at ({x}, {y})");
                }
            }
        }
    }
    let _ = comps;
}

fn new_512_vm(height: u16) -> X68kVm {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[0] = 137;
    vm.bus.crtc.regs[2] = 28;
    vm.bus.crtc.regs[3] = 28 + 64;
    vm.bus.crtc.regs[4] = 567;
    vm.bus.crtc.regs[6] = 10;
    vm.bus.crtc.regs[7] = 10 + height;
    vm.bus.crtc.regs[20] = 0x0315; // 512 wide, non-line-doubled
    vm.bus.sysport.contrast = 15;
    write16(&mut vm, 0xe82400, 0x0000);
    write16(&mut vm, 0xe82500, 0x12e4);
    write16(&mut vm, 0xe82600, 0x0061);
    vm
}

#[test]
fn gvram_all_modes_tvram_scroll_and_vicon_priorities_match_across_generations() {
    let mut comps = Compositors::new();

    let mut vm = new_512_vm(128);
    // Palettes
    for i in 1..256u32 {
        write16(&mut vm, 0xe82000 + i * 2, ((i as u16) * 0x0421) | 1);
        write16(&mut vm, 0xe82200 + i * 2, ((i as u16) * 0x0841) | 0x0001);
    }
    // TVRAM across 4 planes + scroll
    for y in 0..64u32 {
        for col in 0..16u32 {
            vm.bus.write_u8(0xe00000 + y * 128 + col, (y ^ col) as u8, true).unwrap();
            vm.bus.write_u8(0xe20000 + y * 128 + col, (y + col * 3) as u8, true).unwrap();
            vm.bus.write_u8(0xe40000 + y * 128 + col, 0x55, true).unwrap();
            vm.bus.write_u8(0xe60000 + y * 128 + col, 0x0f, true).unwrap();
        }
    }
    vm.bus.crtc.regs[10] = 5;
    vm.bus.crtc.regs[11] = 3;

    // 16-color indexed with independent scrolls & priority slots
    vm.bus.crtc.regs[20] = 0x0315;
    write16(&mut vm, 0xc00000, 0x4321);
    write16(&mut vm, 0xc00002, 0x0f0e);
    write16(&mut vm, 0xc00400, 0x1234);
    vm.bus.crtc.regs[12] = 3;
    vm.bus.crtc.regs[13] = 7;
    vm.bus.crtc.regs[14] = 11;
    vm.bus.crtc.regs[15] = 2;
    write16(&mut vm, 0xe82400, 0);
    write16(&mut vm, 0xe82500, 0x121b);
    write16(&mut vm, 0xe82600, 0x002f);
    assert_all_generations_match(&mut vm, &mut comps, "16-color");

    // 256-color mode
    vm.bus.crtc.regs[20] = 0x0115;
    write16(&mut vm, 0xe82400, 1);
    write16(&mut vm, 0xe82600, 0x0023);
    assert_all_generations_match(&mut vm, &mut comps, "256-color");

    // 65536-color mode
    vm.bus.crtc.regs[20] = 0x0215;
    write16(&mut vm, 0xc00000, 0x4220);
    write16(&mut vm, 0xc00002, 0xfedc);
    write16(&mut vm, 0xe82400, 3);
    write16(&mut vm, 0xe82600, 0x0024);
    assert_all_generations_match(&mut vm, &mut comps, "65536-color");

    // 1024-dot 16-color quadrant mode
    vm.bus.crtc.regs[20] = 0x0715;
    vm.bus.crtc.regs[12] = 510;
    vm.bus.crtc.regs[13] = 510;
    write16(&mut vm, 0xe82400, 4);
    write16(&mut vm, 0xe82600, 0x0030);
    assert_all_generations_match(&mut vm, &mut comps, "1024-dot");
}

#[test]
fn cynthia_sprites_bg_doubler_pcg_palette_and_line_double_match_across_generations() {
    let mut comps = Compositors::new();

    let mut vm = new_512_vm(128);
    vm.bus.crtc.regs[2] = 28;
    vm.bus.crtc.regs[3] = 92;
    vm.bus.crtc.regs[6] = 2;
    vm.bus.crtc.regs[7] = 130;
    vm.bus.crtc.regs[20] = 0x0314;

    for i in 1..256u32 {
        write16(&mut vm, 0xe82200 + i * 2, ((i as u16) * 0x0529) | 1);
    }

    // BG setup + patterns + maps
    write16(&mut vm, 0xeb080c, 32);
    write16(&mut vm, 0xeb080e, 2);
    write16(&mut vm, 0xeb0800, 3); // BG0 scroll X
    write16(&mut vm, 0xeb0802, 5); // BG0 scroll Y
    write16(&mut vm, 0xeb0804, 7); // BG1 scroll X
    write16(&mut vm, 0xeb0806, 9); // BG1 scroll Y
    write16(&mut vm, 0xeb0808, 0x0219); // master + BG0 + BG1 (BG1 uses TEXT1)

    for p in 1..16u32 {
        for off in (0..128).step_by(2) {
            write16(&mut vm, 0xeb8000 + p * 128 + off, (p as u16) * 0x1234 + (off as u16));
        }
    }
    // Map BG0/BG1 with flips & palettes
    for row in 0..8u32 {
        for col in 0..16u32 {
            write16(&mut vm, 0xebc000 + (row * 64 + col) * 2, 0xc104 + ((col as u16) & 3));
            write16(&mut vm, 0xebe000 + (row * 64 + col) * 2, 0x4208 + ((row as u16) & 3));
        }
    }

    // Sprites: prio 1, 2, 3, flips, edge clipping, and extra/absent bank
    let sprites = [
        (0, 16, 16, 0x0101, 3),  // prio 3 at (0, 0..16)
        (1, 24, 20, 0xc202, 2),  // prio 2 flipped at (8, 4..20)
        (2, 8, 12, 0x4303, 1),   // prio 1 left-clipped at (-8, -4..12)
        (3, 504, 24, 0x0404, 7), // prio 3 + SPCS absent bank -> nibble 15
    ];
    for (idx, x, y, attr, flags) in sprites {
        write16(&mut vm, 0xeb0000 + idx * 8, x);
        write16(&mut vm, 0xeb0002 + idx * 8, y);
        write16(&mut vm, 0xeb0004 + idx * 8, attr);
        write16(&mut vm, 0xeb0006 + idx * 8, flags);
    }

    // Check live path parity first
    assert_all_generations_match(&mut vm, &mut comps, "cynthia-live-8x8");

    // Test 16x16 BG + full_pattern_memory enabled
    vm.bus.sprite_options.full_pattern_memory = true;
    write16(&mut vm, 0xeb0808, 0x0601); // MPUCS=1 to write extra bank
    for off in (0..128).step_by(2) {
        write16(&mut vm, 0xeb8200 + off, 0x9abc);
    }
    write16(&mut vm, 0xeb0808, 0x0201); // master + BG0
    write16(&mut vm, 0xeb0810, 0x0001); // 16x16 BG0
    assert_all_generations_match(&mut vm, &mut comps, "cynthia-live-16x16-extbank");

    // Reset to 8x8 and step a full frame with mid-frame XSP sprite split, raster palette, BG scroll, and PCG rewrite
    vm.bus.sprite_options.full_pattern_memory = false;
    write16(&mut vm, 0xeb0810, 0x0000);
    write16(&mut vm, 0xeb0808, 0x0219);
    vm.bus.advance_peripherals(LINE);
    for r in 1..140u32 {
        // Raster color animation
        write16(&mut vm, 0xe82202, ((r as u16) * 0x0317) | 1);
        // Raster BG scroll
        write16(&mut vm, 0xeb0800, (r * 3) as u16);
        if r == 10 {
            // Mid-sprite doubler split
            write16(&mut vm, 0xeb0000, 48);
            write16(&mut vm, 0xeb0004, 0x0505);
        }
        if r == 20 {
            // Mid-frame PCG block rewrite
            for off in (0..128).step_by(2) {
                write16(&mut vm, 0xeb8000 + 2 * 128 + off, 0x7654);
            }
        }
        vm.bus.advance_peripherals(LINE);
    }
    assert!(vm.bus.raster_frame().is_some());
    assert_all_generations_match(&mut vm, &mut comps, "cynthia-raster-completed");
}

#[test]
fn line_doubled_and_midframe_blanking_match_across_generations() {
    let mut comps = Compositors::new();
    let mut vm = new_512_vm(256);
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 512;
    vm.bus.crtc.regs[20] = 0x0410; // 512 physical rasters -> 256 line-doubled output rows

    for i in 1..256u32 {
        write16(&mut vm, 0xe82000 + i * 2, ((i as u16) * 0x0421) | 1);
        write16(&mut vm, 0xe82200 + i * 2, ((i as u16) * 0x0631) | 1);
    }
    for y in 0..256u32 {
        vm.bus.write_u8(0xe00000 + y * 128 + 2, 0xff, true).unwrap();
        write16(&mut vm, 0xc00000 + y * 1024 + 4, (y as u16 & 14).max(2));
    }
    write16(&mut vm, 0xeb080c, 32);
    write16(&mut vm, 0xeb0808, 0x0209); // master + BG1
    for off in (0..128).step_by(2) {
        write16(&mut vm, 0xeb8080 + off, 0x4444);
    }
    for col in 0..16u32 {
        write16(&mut vm, 0xebe000 + col * 2, 0x0104);
    }
    for (off, v) in [(0, 16), (2, 16), (4, 0x0101), (6, 3)] {
        write16(&mut vm, 0xeb0000 + off, v);
    }

    for r in 0..568u32 {
        if r == 100 {
            write16(&mut vm, 0xe82600, 0x0000); // blank display mid-frame
        } else if r == 200 {
            write16(&mut vm, 0xe82600, 0x0061); // re-enable display
        }
        vm.bus.advance_peripherals(LINE);
    }
    let rf = vm.bus.raster_frame().expect("completed 568-line frame");
    assert_eq!(rf.height, 256);
    assert_all_generations_match(&mut vm, &mut comps, "line-doubled-blanking");
}

#[test]
fn dirty_vram_uploads_and_exhaustive_grbi_contrast_match_across_generations() {
    #[cfg(feature = "wgpu-backend")]
    let mut compute = ViconGpuCompositor::new_with_config(RendererConfig {
        generation: ShaderGeneration::Compute,
        max_width: 512,
        max_height: 512,
        output_format: PixelFormat::Rgba8,
    })
    .unwrap();
    #[cfg(feature = "gles2-backend")]
    let mut gles2 = Gles2Compositor::new_with_config(RendererConfig {
        generation: ShaderGeneration::Gles2,
        max_width: 512,
        max_height: 512,
        output_format: PixelFormat::Rgba8,
    })
    .unwrap();

    fn setup_grbi_vm() -> X68kVm {
        let mut vm = X68kVm::new_app_vm();
        vm.bus.crtc.regs[2] = 28;
        vm.bus.crtc.regs[3] = 28 + 32; // 256 px wide
        vm.bus.crtc.regs[6] = 0;
        vm.bus.crtc.regs[7] = 256; // 256 px tall
        vm.bus.crtc.regs[20] = 0x0214; // 65536-color memory view, non-doubled 256 lines
        write16(&mut vm, 0xe82400, 3); // 65536-color VICON mode
        write16(&mut vm, 0xe82600, 0x0024); // Text + Graphics ON

        // Identity 16-bit palette lookup so GVRAM word == GRBI directly:
        // pal[2k] = pal[2k+1] = (2k << 8) | (2k + 1)
        for k in 0..128u16 {
            let word = (2 * k) << 8 | (2 * k + 1);
            write16(&mut vm, 0xe82000 + (2 * k as u32) * 2, word);
            write16(&mut vm, 0xe82000 + (2 * k as u32 + 1) * 2, word);
        }
        for y in 0..256u32 {
            for x in 0..256u32 {
                let grbi = ((y * 256 + x) as u16).max(1);
                write16(&mut vm, 0xc00000 + (y * 512 + x) * 2, grbi);
            }
        }
        vm
    }

    // 256x256 65536-color screen covering all 65,536 GRBI values across contrasts 0, 1, 7, 15
    let mut vm = setup_grbi_vm();
    #[cfg(feature = "gles2-backend")]
    let mut vm_gles = setup_grbi_vm();

    for contrast in [0u8, 1, 7, 15] {
        #[cfg(feature = "wgpu-backend")]
        {
            vm.bus.sysport.contrast = contrast;
            vm.bus
                .write_u8(0xe00000 + (contrast as u32) * 0x2000 + 1, 0xff, true)
                .unwrap();
            write16(&mut vm, 0xe82202, 0x7fff);
            let cpu = render_bus_frame_cpu(&vm.bus);
            let c_frame = compute.render_bus_frame_dirty(&mut vm.bus);
            assert_eq!(c_frame.rgba8, cpu.rgba8, "compute dirty contrast={contrast}");
        }

        #[cfg(feature = "gles2-backend")]
        {
            vm_gles.bus.sysport.contrast = contrast;
            vm_gles
                .bus
                .write_u8(0xe00000 + (contrast as u32) * 0x2000 + 1, 0xff, true)
                .unwrap();
            vm_gles
                .bus
                .write_u8(0xe40000 + (contrast as u32) * 0x1000, 0xaa, true)
                .unwrap();
            write16(&mut vm_gles, 0xe82202, 0x7fff);
            let cpu2 = render_bus_frame_cpu(&vm_gles.bus);
            let g_frame = gles2.render_bus_frame_dirty(&mut vm_gles.bus);
            assert_eq!(g_frame.rgba8, cpu2.rgba8, "gles2 dirty contrast={contrast}");
        }
    }

    // Verify unified ViconCompositor facade for each requested ShaderGeneration
    for gen in [
        ShaderGeneration::Auto,
        ShaderGeneration::Compute,
        ShaderGeneration::Gles2,
        ShaderGeneration::Cpu,
    ] {
        let mut comp = ViconCompositor::new(RendererConfig {
            generation: gen,
            max_width: 512,
            max_height: 512,
            output_format: PixelFormat::Rgb565,
        });
        assert_ne!(
            comp.active_generation(),
            ShaderGeneration::Auto,
            "active_generation must resolve to a concrete backend"
        );
        let f565 = comp.render_frame_rgb565(&mut vm.bus);
        assert_eq!(f565.width, 256);
        assert_eq!(f565.height, 256);
        assert_eq!(f565.rgb565.len(), 256 * 256);
    }
}
