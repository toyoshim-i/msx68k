//! Small ROM-free fixtures assert hardware colors independently of CPU/GPU parity.
use msx68k_bus::{Bus, X68kVm};
use msx68k_vicon::{render_bus_frame_cpu, ViconGpuCompositor};

fn write(vm: &mut X68kVm, addr: u32, value: u16) {
    vm.bus.write_u16(addr, value, true).unwrap();
}
fn fixture() -> X68kVm {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[20] = 0x300;
    vm.bus.crtc.regs[2] = 0;
    vm.bus.crtc.regs[3] = 96;
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 128;
    vm.bus.sysport.contrast = 15;
    write(&mut vm, 0xe82400, 0);
    write(&mut vm, 0xe82500, 0x12e4);
    write(&mut vm, 0xe82600, 15);
    for (index, grbi) in [(1, 0x07c0), (2, 0xf800), (3, 0x003e), (4, 0xfffe)] {
        write(&mut vm, 0xe82000 + index * 2, grbi);
    }
    vm
}
fn check(vm: &X68kVm, gpu: &mut Option<ViconGpuCompositor>, points: &[(u32, u32, [u8; 3])]) {
    let cpu = render_bus_frame_cpu(&vm.bus);
    for &(x, y, rgb) in points {
        let offset = ((y * cpu.width + x) * 4) as usize;
        assert_eq!(&cpu.rgba8[offset..offset + 3], &rgb, "pixel ({x}, {y})");
    }
    if let Some(gpu) = gpu {
        assert_eq!(gpu.render_bus_frame(&vm.bus).rgba8, cpu.rgba8);
    }
}
const RED: [u8; 3] = [251, 0, 0];
const GREEN: [u8; 3] = [0, 251, 0];
const BLUE: [u8; 3] = [0, 0, 251];
const BLACK: [u8; 3] = [0, 0, 0];

#[test]
fn indexed_modes_page_priority_scroll_and_palette_transparency() {
    let mut gpu = ViconGpuCompositor::new();
    let mut vm = fixture();
    write(&mut vm, 0xc00000, 0x4321);
    check(&vm, &mut gpu, &[(0, 0, RED), (512, 0, RED)]);
    write(&mut vm, 0xe82500, 0x121b); // Page 3 is now the first slot.
    check(&vm, &mut gpu, &[(0, 0, [251, 251, 251])]);
    write(&mut vm, 0xe82500, 0x12e4);
    write(&mut vm, 0xc00000, 0x0320); // Zero index lets the next page through.
    check(&vm, &mut gpu, &[(0, 0, GREEN)]);
    write(&mut vm, 0xe82004, 0); // Nonzero index, zero color hides lower graphic pages.
    check(&vm, &mut gpu, &[(0, 0, BLACK)]);
    write(&mut vm, 0xe82004, 0xf800);
    write(&mut vm, 0xc00002, 0x0001);
    vm.bus.crtc.regs[12] = 1; // Page 0 scroll is independent of page 1.
    check(&vm, &mut gpu, &[(0, 0, RED)]);
    vm.bus.crtc.regs[12] = 0;
    write(&mut vm, 0xe82400, 1);
    write(&mut vm, 0xc00000, 0x4321);
    write(&mut vm, 0xe82042, 0x003e); // Index 0x21.
    write(&mut vm, 0xe82086, 0xf800); // Index 0x43.
    write(&mut vm, 0xe82600, 2); // Either enable bit of the pair enables it.
    check(&vm, &mut gpu, &[(0, 0, BLUE)]);
    write(&mut vm, 0xe82600, 8);
    check(&vm, &mut gpu, &[(0, 0, GREEN)]);
    write(&mut vm, 0xe82600, 2);
    vm.bus.crtc.regs[14] = 1; // Upper nibble follows page 1's own scroll.
    check(&vm, &mut gpu, &[(0, 0, RED)]);
    write(&mut vm, 0xe82000, 0xffff);
    write(&mut vm, 0xe82600, 0);
    check(&vm, &mut gpu, &[(0, 0, BLACK)]);
}

#[test]
fn large_quadrants_and_16bit_palette_conversion() {
    let mut gpu = ViconGpuCompositor::new();
    let mut vm = fixture();
    write(&mut vm, 0xc00000, 0x4321);
    vm.bus.crtc.regs[20] = 0x400;
    write(&mut vm, 0xe82400, 4);
    write(&mut vm, 0xe82600, 16);
    check(&vm, &mut gpu, &[(0, 0, RED), (512, 0, GREEN)]);
    vm.bus.crtc.regs[12] = 512;
    vm.bus.crtc.regs[13] = 512;
    check(&vm, &mut gpu, &[(0, 0, [251, 251, 251]), (512, 0, BLUE)]);
    vm.bus.crtc.regs[12] = 0;
    vm.bus.crtc.regs[13] = 0;
    vm.bus.crtc.regs[20] = 0x300;
    write(&mut vm, 0xe82400, 3);
    write(&mut vm, 0xe82600, 4);
    // Code 0x4321: lower byte uses low byte of palette word 0x20;
    // upper byte uses low byte of word 0x43. Together these form 0x07c0.
    write(&mut vm, 0xe82040, 0x55c0);
    write(&mut vm, 0xe82086, 0xaa07);
    check(&vm, &mut gpu, &[(0, 0, RED)]);
    // Even byte codes select the other byte of each palette word.
    write(&mut vm, 0xc00000, 0x4220);
    write(&mut vm, 0xe82040, 0xc055);
    write(&mut vm, 0xe82086, 0x07aa);
    check(&vm, &mut gpu, &[(0, 0, RED)]);
    vm.bus.sysport.contrast = 0;
    check(&vm, &mut gpu, &[(0, 0, BLACK)]);
}

#[test]
fn graphics_rank_inserts_relative_to_text_and_sprite_order() {
    let mut gpu = ViconGpuCompositor::new();
    let mut vm = fixture();
    write(&mut vm, 0xc00000, 1);
    write(&mut vm, 0xe00000, 0x8000);
    write(&mut vm, 0xe82202, 0xf800);
    write(&mut vm, 0xe82600, 0x21);
    // Sprite rank 1, text rank 0. Graphics 0 and 3 are in front; 1/2 behind text.
    for (rank, color) in [(0, RED), (1, GREEN), (2, GREEN), (3, RED)] {
        write(&mut vm, 0xe82500, 0x10e4 | (rank << 8));
        check(&vm, &mut gpu, &[(0, 0, color)]);
    }
    // Put text behind sprite: middle graphics is now in front of text.
    write(&mut vm, 0xe82500, 0x04e4 | 0x100);
    check(&vm, &mut gpu, &[(0, 0, RED)]);
}

#[test]
fn vm_screen_uses_graphics_modes_and_tracks_physical_dirty_pages() {
    let Some(gpu) = ViconGpuCompositor::new() else {
        return;
    };
    let mut screen = msx68k_vicon::VmScreen::new(gpu.device(), gpu.queue());
    let output = screen.output();
    let mut vm = fixture();
    write(&mut vm, 0xc00000, 0x4321);
    for (memory, display, enable) in [(0, 0, 15), (0x100, 1, 3), (0x400, 4, 16)] {
        vm.bus.crtc.regs[20] = memory;
        write(&mut vm, 0xe82400, display);
        write(&mut vm, 0xe82600, enable);
        screen.present(&mut vm.bus);
        let actual = output.read_back(gpu.device(), gpu.queue()).unwrap();
        assert_eq!(actual.rgba8, render_bus_frame_cpu(&vm.bus).rgba8);
    }
    vm.bus.crtc.regs[20] = 0;
    write(&mut vm, 0xe82400, 0);
    write(&mut vm, 0xe82600, 8); // Physical page 3, slot 3.
    write(&mut vm, 0xd83000, 3);
    screen.present(&mut vm.bus);
    let actual = output.read_back(gpu.device(), gpu.queue()).unwrap();
    assert_eq!(actual.rgba8, render_bus_frame_cpu(&vm.bus).rgba8);
    let offset = (12 * actual.width * 4) as usize;
    assert_eq!(&actual.rgba8[offset..offset + 3], &BLUE);
}

#[test]
fn special_priority_and_half_transparency_match_across_backends() {
    let mut gpu = ViconGpuCompositor::new();
    let mut gles2 = msx68k_vicon::ViconCompositor::new(msx68k_vicon::RendererConfig {
        generation: msx68k_vicon::ShaderGeneration::Gles2,
        ..Default::default()
    });
    let mut vm = fixture();

    // Graphics palette:
    //   1 = RED with I=0 (0x07C0 -> r6=62, g6=0, b6=0)
    //   2 = GREEN with I=0 (0xF800 -> r6=0, g6=62, b6=0)
    //   5 = RED with I=1 (0x07C1 -> r6=63, g6=1, b6=1)
    write(&mut vm, 0xe8200a, 0x07c1);
    // Text palette:
    //   1 = BLUE with I=0 (0x003E -> r6=0, g6=0, b6=62)
    //   2 = BLUE with I=1 (0x003F -> r6=1, g6=1, b6=63)
    write(&mut vm, 0xe82202, 0x003e);
    write(&mut vm, 0xe82204, 0x003f);

    // Pixel (0,0): G0=1 (RED, I=0), G1=2 (GREEN, I=0)
    // Pixel (1,0): G0=5 (RED, I=1), G1=2 (GREEN, I=0)
    // Pixel (2,0): G0=5 (RED, I=1), G1=0 (no sub graphic)
    write(&mut vm, 0xc00000, 0x0021);
    write(&mut vm, 0xc00002, 0x0025);
    write(&mut vm, 0xc00004, 0x0005);

    // Text pixels: (0,0)=1 (BLUE I=0), (1,0)=1 (BLUE I=0), (3,0)=2 (BLUE I=1)
    // Plane 0: bits for x=0,1 -> 0xC000; Plane 1: bit for x=3 -> 0x1000
    write(&mut vm, 0xe00000, 0xc000);
    write(&mut vm, 0xe20000, 0x1000);
    // Pixel (3,0): G0=2 (GREEN I=0)
    write(&mut vm, 0xc00006, 0x0002);

    // Text in front of Graphics (gr_rank=2, tx_rank=0, sp_rank=1).
    write(&mut vm, 0xe82500, 0x12e4);

    let check_all = |vm: &X68kVm,
                     gpu: &mut Option<ViconGpuCompositor>,
                     gles2: &mut msx68k_vicon::ViconCompositor,
                     points: &[(u32, u32, [u8; 3])]| {
        let cpu = render_bus_frame_cpu(&vm.bus);
        for &(x, y, rgb) in points {
            let offset = ((y * cpu.width + x) * 4) as usize;
            assert_eq!(&cpu.rgba8[offset..offset + 3], &rgb, "pixel ({x}, {y})");
        }
        if let Some(gpu) = gpu {
            assert_eq!(gpu.render_bus_frame(&vm.bus).rgba8, cpu.rgba8);
        }
        if gles2.active_generation() == msx68k_vicon::ShaderGeneration::Gles2 {
            assert_eq!(gles2.render_frame_readonly(&vm.bus).rgba8, cpu.rgba8);
        }
    };

    // 1. Special priority (EXON=1, HP=0): G0 I=1 promotes above Text; I=0 stays behind Text.
    write(&mut vm, 0xe82600, 0x1023); // EXON | TX | G0 | G1
    check_all(
        &vm,
        &mut gpu,
        &mut gles2,
        &[
            (0, 0, BLUE),          // G0 has I=0 -> Text wins
            (1, 0, [255, 4, 4]),   // G0 has I=1 (0x07C1) -> Graphics promotes above Text
        ],
    );

    // 2. Graphic-to-Graphic half-transparency (EXON=1, HP=1, GG=1), Graphics in front (gr_rank=0).
    write(&mut vm, 0xe82500, 0x10e4); // gr_rank=0
    write(&mut vm, 0xe82600, 0x1a03); // EXON | HP | GG | G0 | G1
    // (1,0): RED(63,1,1) + GREEN(0,62,0) >> 1 = (31,31,0) -> (125,125,0)
    // (2,0): RED(63,1,1) + BLACK(0,0,0)  >> 1 = (31,0,0)  -> (125,0,0)
    check_all(
        &vm,
        &mut gpu,
        &mut gles2,
        &[
            (0, 0, RED),           // I=0 -> opaque RED
            (1, 0, [125, 125, 0]), // I=1 -> 50% blend with G1 GREEN
            (2, 0, [125, 0, 0]),   // I=1, no sub -> 50% half-brightness
        ],
    );

    // 3. Text-to-Graphic half-transparency (EXON=1, HP=1, GT=1), Text in front (gr_rank=2).
    write(&mut vm, 0xe82500, 0x12e4);
    write(&mut vm, 0xe82600, 0x1923); // EXON | HP | GT | TX | G0 | G1
    // (3,0): Text BLUE(1,1,63) + Graphic GREEN(0,62,0) >> 1 = (0,31,31) -> (0,125,125)
    check_all(
        &vm,
        &mut gpu,
        &mut gles2,
        &[
            (0, 0, BLUE),          // Text I=0 -> opaque BLUE
            (3, 0, [0, 125, 125]), // Text I=1 -> 50% blend with underlying Graphic GREEN
        ],
    );

    // 4. Forced half-transparency with BP=1 (EXON=1, HP=1, BP=1, GT=1): I=0 Text also blends.
    write(&mut vm, 0xe82600, 0x1d23); // EXON | HP | BP | GT | TX | G0 | G1
    // (0,0): Text BLUE(0,0,62) + Graphic RED(62,0,0) >> 1 = (31,0,31) -> (125,0,125)
    check_all(&vm, &mut gpu, &mut gles2, &[(0, 0, [125, 0, 125])]);
}

#[test]
fn mixed_resolution_cynthia_scales_2x_across_all_backends() {
    let mut gpu = ViconGpuCompositor::new();
    let mut gles2 = msx68k_vicon::ViconCompositor::new(msx68k_vicon::RendererConfig {
        generation: msx68k_vicon::ShaderGeneration::Gles2,
        ..Default::default()
    });
    let mut vm = fixture();

    // CRTC 512x512 (R20: HF=1, VRES=1, HRES=1 -> 0x0315; R02=16, R03=80 -> 512px wide)
    vm.bus.crtc.regs[20] = 0x0315;
    vm.bus.crtc.regs[2] = 16;
    vm.bus.crtc.regs[3] = 80;
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 128;

    // CYNTHIA 256x256 (HRES=0, VRES=0 -> 0x0010), H-DISP = 10 -> origin_x = 10*8 - (16+4)*4 = 0
    write(&mut vm, 0xeb080c, 10);
    write(&mut vm, 0xeb080e, 0);
    write(&mut vm, 0xeb0810, 0x0010);
    write(&mut vm, 0xeb0808, 0x0201); // Master + BG0 enabled

    // Sprite/BG palette: index 1 = RED, index 2 = GREEN
    write(&mut vm, 0xe82202, 0x07c0);
    write(&mut vm, 0xe82204, 0xf800);

    // BG0 tile (0,0) -> pattern 0:
    //   source_y=0: pixel 0 = 1 (RED), pixel 1 = 2 (GREEN)
    //   source_y=1: pixel 0 = 2 (GREEN), pixel 1 = 1 (RED)
    write(&mut vm, 0xebc000, 0x0000);
    write(&mut vm, 0xeb8000, 0x1200);
    write(&mut vm, 0xeb8004, 0x2100);

    // Enable Sprite/BG only
    write(&mut vm, 0xe82500, 0x12e4);
    write(&mut vm, 0xe82600, 0x0040);

    let cpu = render_bus_frame_cpu(&vm.bus);
    // In 2x horizontal and 2x vertical stretch:
    //   (0,0), (1,0), (0,1), (1,1) -> source (0,0) = RED
    //   (2,0), (3,0), (2,1), (3,1) -> source (1,0) = GREEN
    //   (0,2), (1,2), (0,3), (1,3) -> source (0,1) = GREEN
    //   (2,2), (3,2), (2,3), (3,3) -> source (1,1) = RED
    for (x, y, rgb) in [
        (0, 0, RED),
        (1, 0, RED),
        (0, 1, RED),
        (1, 1, RED),
        (2, 0, GREEN),
        (3, 0, GREEN),
        (0, 2, GREEN),
        (1, 3, GREEN),
        (2, 2, RED),
        (3, 3, RED),
    ] {
        let offset = ((y * cpu.width + x) * 4) as usize;
        assert_eq!(&cpu.rgba8[offset..offset + 3], &rgb, "pixel ({x}, {y})");
    }

    if let Some(gpu) = gpu.as_mut() {
        assert_eq!(gpu.render_bus_frame(&vm.bus).rgba8, cpu.rgba8);
    }
    if gles2.active_generation() == msx68k_vicon::ShaderGeneration::Gles2 {
        assert_eq!(gles2.render_frame_readonly(&vm.bus).rgba8, cpu.rgba8);
    }
}

#[test]
fn interlace_alternates_even_and_odd_field_dimming_across_backends() {
    let mut gpu = ViconGpuCompositor::new();
    let mut gles2 = msx68k_vicon::ViconCompositor::new(msx68k_vicon::RendererConfig {
        generation: msx68k_vicon::ShaderGeneration::Gles2,
        ..Default::default()
    });
    let mut vm = fixture();

    // 15.98 kHz 512x512 interlaced mode (R20: HF=0, VRES=2, HRES=1 -> 0x0009):
    // Each field scans 256 lines (R06=0, R07=256), active_resolution() reports 512 lines.
    vm.bus.crtc.regs[0] = 75;
    vm.bus.crtc.regs[2] = 8;
    vm.bus.crtc.regs[3] = 72; // 512px wide
    vm.bus.crtc.regs[4] = 263; // 264 rasters per field
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 256;
    write(&mut vm, 0xe80028, 0x0009); // triggers horizontal timing recompute + VRES=2
    assert_eq!(vm.bus.crtc.active_resolution(), (512, 512));

    // Fill G0 rows 0 and 1 at x=0 with RED (index 1 -> [251, 0, 0]).
    // Dimmed 75% value: (251 * 3) >> 2 = 188.
    write(&mut vm, 0xc00000, 0x0001);
    write(&mut vm, 0xc00400, 0x0001);
    write(&mut vm, 0xe82600, 0x0001); // G0 only
    let dim_red: [u8; 3] = [188, 0, 0];

    // Advance one full field so Field 0 completes into `completed_frame`.
    let line_cycles = vm.bus.crtc.scanline_cycles();
    vm.bus.advance_peripherals(264 * line_cycles);
    assert_eq!(vm.bus.raster_frame().unwrap().interlace_field, Some(0));

    // Field 0 (even): y=0 is full brightness, y=1 is dimmed 75%.
    let cpu_even = render_bus_frame_cpu(&vm.bus);
    assert_eq!(&cpu_even.rgba8[0..3], &RED);
    assert_eq!(&cpu_even.rgba8[(512 * 4)..(512 * 4 + 3)], &dim_red);
    if let Some(gpu) = gpu.as_mut() {
        assert_eq!(gpu.render_bus_frame(&vm.bus).rgba8, cpu_even.rgba8);
    }
    if gles2.active_generation() == msx68k_vicon::ShaderGeneration::Gles2 {
        assert_eq!(gles2.render_frame_readonly(&vm.bus).rgba8, cpu_even.rgba8);
    }

    // Advance a second full field so Field 1 (odd) completes into `completed_frame`.
    vm.bus.advance_peripherals(264 * line_cycles);
    assert_eq!(vm.bus.raster_frame().unwrap().interlace_field, Some(1));

    // Field 1 (odd): y=0 is dimmed 75%, y=1 is full brightness.
    let cpu_odd = render_bus_frame_cpu(&vm.bus);
    assert_eq!(&cpu_odd.rgba8[0..3], &dim_red);
    assert_eq!(&cpu_odd.rgba8[(512 * 4)..(512 * 4 + 3)], &RED);
    if let Some(gpu) = gpu.as_mut() {
        assert_eq!(gpu.render_bus_frame(&vm.bus).rgba8, cpu_odd.rgba8);
    }
    if gles2.active_generation() == msx68k_vicon::ShaderGeneration::Gles2 {
        assert_eq!(gles2.render_frame_readonly(&vm.bus).rgba8, cpu_odd.rgba8);
    }
}
