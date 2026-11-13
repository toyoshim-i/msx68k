//! ROM-free scanout fixtures, including a real MFP raster IRQ handler.
use msx68k_bus::{Bus, CpuSpeedMode, X68kVm};
use msx68k_vicon::{render_bus_frame_cpu, RenderedFrame, ViconGpuCompositor, VmScreen};

const LINE: u32 = 317;
const RED: [u8; 3] = [251, 0, 0];
const GREEN: [u8; 3] = [0, 251, 0];
const BLUE: [u8; 3] = [0, 0, 251];
const BLACK: [u8; 3] = [0, 0, 0];

fn write(vm: &mut X68kVm, addr: u32, value: u16) {
    vm.bus.write_u16(addr, value, true).unwrap();
}
fn setup(vm: &mut X68kVm) {
    vm.bus.crtc.regs[2] = 28;
    vm.bus.crtc.regs[3] = 60;
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 128;
    vm.bus.crtc.regs[20] = 0x0316;
    vm.bus.sysport.contrast = 15;
    write(vm, 0xe82400, 0);
    write(vm, 0xe82500, 0x12e4);
    write(vm, 0xe82600, 1);
    write(vm, 0xe82002, 0x07c0);
    write(vm, 0xe82004, 0x003e);
    // Two vertical stripes, so scrolling changes the sampled color.
    for y in 0..128 {
        write(vm, 0xc00000 + y * 1024, 1);
        write(vm, 0xc00002 + y * 1024, 2);
    }
}
fn fixture() -> X68kVm {
    let mut vm = X68kVm::new_app_vm();
    setup(&mut vm);
    vm
}
fn pixel(frame: &RenderedFrame, x: u32, y: u32) -> [u8; 3] {
    let offset = ((y * frame.width + x) * 4) as usize;
    frame.rgba8[offset..offset + 3].try_into().unwrap()
}
fn verify(vm: &mut X68kVm, points: &[(u32, u32, [u8; 3])]) {
    let cpu = render_bus_frame_cpu(&vm.bus);
    for &(x, y, color) in points {
        assert_eq!(pixel(&cpu, x, y), color, "pixel ({x}, {y})");
    }
    if let Some(mut gpu) = ViconGpuCompositor::new() {
        assert_eq!(gpu.render_bus_frame(&vm.bus).rgba8, cpu.rgba8);
        let mut screen = VmScreen::new(gpu.device(), gpu.queue());
        screen.present(&mut vm.bus);
        let actual = screen
            .output()
            .read_back(gpu.device(), gpu.queue())
            .unwrap();
        assert_eq!(actual.rgba8, cpu.rgba8);
    }
}

#[test]
fn palette_split_keeps_the_completed_frame_during_vblank() {
    let mut vm = fixture();
    vm.bus.advance_peripherals(64 * LINE + 66); // After visible row 63 was sampled.
    write(&mut vm, 0xe82002, 0xf800);
    // Two writes in one scanline are coalesced; only the final palette is latched.
    write(&mut vm, 0xe82002, 0x003e);
    write(&mut vm, 0xe82002, 0xf800);
    vm.bus.advance_peripherals(65 * LINE);
    let history = vm.bus.raster_frame().unwrap();
    assert_eq!(
        history.lines.iter().map(|line| line.y).collect::<Vec<_>>(),
        [0, 64]
    );
    write(&mut vm, 0xe82002, 0x003e); // Next frame, not the already displayed one.
    verify(
        &mut vm,
        &[(0, 0, RED), (0, 63, RED), (0, 64, GREEN), (0, 127, GREEN)],
    );
    vm.bus.advance_peripherals(568 * LINE);
    assert_eq!(vm.bus.raster_frame().unwrap().lines.len(), 1);
    verify(&mut vm, &[(0, 0, BLUE), (0, 127, BLUE)]);
}

#[test]
fn scroll_display_and_contrast_are_latched_per_row() {
    let mut vm = fixture();
    vm.bus.advance_peripherals(32 * LINE + 66);
    write(&mut vm, 0xe80018, 1); // Page 0 X scroll, visible starting row 32.
    vm.bus.advance_peripherals(32 * LINE);
    write(&mut vm, 0xe82600, 0);
    vm.bus.advance_peripherals(32 * LINE);
    write(&mut vm, 0xe82600, 1);
    vm.bus.write_u8(0xe8e001, 0, true).unwrap();
    vm.bus.advance_peripherals(33 * LINE);
    verify(
        &mut vm,
        &[
            (0, 31, RED),
            (0, 32, BLUE),
            (0, 63, BLUE),
            (0, 64, BLACK),
            (0, 127, BLACK),
        ],
    );
}

#[test]
fn a_68000_raster_interrupt_changes_the_lower_half_palette() {
    let mut vm = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz10);
    vm.bus.write_u32(0, 0x10000, true).unwrap();
    vm.bus.write_u32(4, 0x400, true).unwrap();
    vm.bus.write_u32(0x4e * 4, 0x600, true).unwrap();
    // MOVE.W #$2000,SR; BRA.S * -- enable IRQs and wait.
    for (i, word) in [0x46fc, 0x2000, 0x60fe].into_iter().enumerate() {
        write(&mut vm, 0x400 + i as u32 * 2, word);
    }
    // MOVE.W #green,$E82002.L; MOVE.W #1,$1000.L; RTE.
    for (i, word) in [0x33fc, 0xf800, 0xe8, 0x2002, 0x33fc, 1, 0, 0x1000, 0x4e73]
        .into_iter()
        .enumerate()
    {
        write(&mut vm, 0x600 + i as u32 * 2, word);
    }
    vm.reset().unwrap();
    setup(&mut vm);
    write(&mut vm, 0xe80012, 65); // Absolute raster 65 = visible row 64.
    for (addr, value) in [(0xe88007, 0x40), (0xe88013, 0x40), (0xe88017, 0x40)] {
        vm.bus.write_u8(addr, value, true).unwrap();
    }
    for _ in 0..20_000 {
        vm.step();
        if vm.bus.crtc.frame_count() != 0 {
            break;
        }
    }
    assert_eq!(vm.bus.crtc.frame_count(), 1);
    assert_eq!(
        vm.bus.read_u16(0x1000, true).unwrap(),
        1,
        "raster handler ran"
    );
    assert_eq!(
        vm.bus
            .raster_frame()
            .unwrap()
            .lines
            .iter()
            .map(|line| line.y)
            .collect::<Vec<_>>(),
        [0, 64]
    );
    verify(
        &mut vm,
        &[(0, 0, RED), (0, 63, RED), (0, 64, GREEN), (0, 127, GREEN)],
    );
}

#[test]
fn text_palette_scroll_and_graphics_priority_change_mid_frame() {
    let mut vm = fixture();
    for y in 0..128 {
        vm.bus.write_u8(0xe00000 + y * 128, 0x80, true).unwrap();
    }
    write(&mut vm, 0xe82202, 0xf800);
    write(&mut vm, 0xe82600, 0x21);
    vm.bus.advance_peripherals(32 * LINE + 66);
    write(&mut vm, 0xe82202, 0x003e);
    vm.bus.advance_peripherals(32 * LINE);
    write(&mut vm, 0xe82500, 0x10e4); // Graphics ahead of text.
    vm.bus.advance_peripherals(32 * LINE);
    write(&mut vm, 0xe82500, 0x12e4);
    write(&mut vm, 0xe80014, 1); // Shift text off x=0; graphic red shows through.
    vm.bus.advance_peripherals(33 * LINE);
    verify(
        &mut vm,
        &[
            (0, 31, GREEN),
            (0, 32, BLUE),
            (0, 63, BLUE),
            (0, 64, RED),
            (0, 96, RED),
        ],
    );
}

#[test]
fn a_blank_frame_skips_palette_upload_and_reenable_uses_latest_palette() {
    let mut vm = fixture();
    write(&mut vm, 0xe82600, 0);
    vm.bus.advance_peripherals(64 * LINE);
    write(&mut vm, 0xe82002, 0xf800);
    vm.bus.advance_peripherals(65 * LINE);
    verify(&mut vm, &[(0, 0, BLACK), (0, 127, BLACK)]);
    if let Some(gpu) = ViconGpuCompositor::new() {
        let mut screen = VmScreen::new(gpu.device(), gpu.queue());
        screen.present(&mut vm.bus);
        let uploaded = screen.present(&mut vm.bus);
        assert_eq!(uploaded, std::mem::size_of::<msx68k_vicon::ViconUniforms>());
    }
    write(&mut vm, 0xe82600, 1);
    vm.bus.advance_peripherals(568 * LINE);
    verify(&mut vm, &[(0, 0, GREEN), (0, 127, GREEN)]);
}

fn background_fixture() -> X68kVm {
    let mut vm = fixture();
    vm.bus.crtc.regs[20] = 0x0314;
    write(&mut vm, 0xeb080c, 32); // Align BG with CRTC horizontal display.
    write(&mut vm, 0xeb0810, 0x14);
    write(&mut vm, 0xeb0808, 0x201);
    write(&mut vm, 0xe82600, 0x40);
    write(&mut vm, 0xe82202, 0x07c0);
    write(&mut vm, 0xe82204, 0x003e);
    write(&mut vm, 0xe82206, 0x07c0);
    for offset in (0..32).step_by(2) {
        write(&mut vm, 0xeb8000 + offset, 0x1111);
    }
    vm
}

#[test]
fn background_prefetch_preserves_codes_but_uses_the_display_rows_palette() {
    let mut vm = background_fixture();
    write(&mut vm, 0xeb8004, 0x2222);
    vm.bus.advance_peripherals(66); // Prefetch row zero before active display.
    write(&mut vm, 0xeb8000, 0x2222);
    write(&mut vm, 0xe82202, 0xf800); // Palette conversion has not happened yet.
    vm.bus.advance_peripherals(LINE); // Display row zero, prefetch row one.
    write(&mut vm, 0xe82202, 0x07c0); // Later rows use a different palette snapshot.
    write(&mut vm, 0xeb8004, 0x3333); // Row one is already buffered.
    vm.bus.advance_peripherals(128 * LINE);
    // Changes after the frame completed must not replace fetched codes or colors.
    write(&mut vm, 0xeb8000, 0);
    write(&mut vm, 0xe82202, 0);
    verify(&mut vm, &[(0, 0, GREEN), (0, 1, BLUE), (0, 2, RED)]);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.background[0], 1);
    assert_eq!(frame.background[512], 2);
}

#[test]
fn background_fetch_continues_while_output_is_disabled() {
    let mut vm = background_fixture();
    write(&mut vm, 0xe82600, 0);
    write(&mut vm, 0xeb0808, 0); // Both final gates are off during prefetch.
    vm.bus.advance_peripherals(66);
    write(&mut vm, 0xeb8000, 0x2222);
    write(&mut vm, 0xeb0808, 0x201);
    write(&mut vm, 0xe82600, 0x40);
    vm.bus.advance_peripherals(129 * LINE);
    verify(&mut vm, &[(0, 0, RED), (1, 0, RED)]);
}

#[test]
fn background_text_and_graphics_share_the_final_priority_order() {
    let mut vm = background_fixture();
    for y in 0..128 {
        vm.bus.write_u8(0xe00000 + y * 128, 0x80, true).unwrap();
    }
    write(&mut vm, 0xe82202, 0xf800); // Text green.
    write(&mut vm, 0xebc000, 0x0100); // BG palette block one, blue.
    write(&mut vm, 0xe82222, 0x003e);
    write(&mut vm, 0xe82600, 0x61);
    // Graphics red, text green and BG blue at the same pixel.
    for (priority, expected) in [
        (0x1200, GREEN),
        (0x0600, BLUE),
        (0x1000, RED),
        (0x1600, GREEN),
    ] {
        write(&mut vm, 0xe82500, priority);
        verify(&mut vm, &[(0, 0, expected)]);
    }
}

fn sprite_fixture() -> X68kVm {
    let mut vm = background_fixture();
    vm.bus.crtc.regs[6] = 2;
    vm.bus.crtc.regs[7] = 130;
    write(&mut vm, 0xeb080e, 2);
    write(&mut vm, 0xeb0808, 0x200); // Sprites on, BG planes off.
    for (offset, value) in [(0, 16), (2, 16), (4, 1), (6, 3)] {
        write(&mut vm, 0xeb0000 + offset, value);
    }
    for offset in (0..128).step_by(2) {
        write(&mut vm, 0xeb8080 + offset, 0x2222);
    }
    vm
}

#[test]
fn sprite_prefetch_freezes_attributes_and_codes_before_background_fetch() {
    let mut vm = sprite_fixture();
    vm.bus.advance_peripherals(LINE + 65);
    write(&mut vm, 0xeb0000, 16); // One cycle before sprite row zero fetch.
    vm.bus.advance_peripherals(1);
    write(&mut vm, 0xeb8080, 0x3333); // Too late to replace buffered row zero.
    write(&mut vm, 0xeb0000, 32); // Move later rows sixteen pixels to the right.
    write(&mut vm, 0xe82204, 0xf800); // Palette conversion is still pending.
    vm.bus.advance_peripherals(2 * LINE); // Display row zero.
    write(&mut vm, 0xe82204, 0x003e);
    vm.bus.advance_peripherals(128 * LINE);
    verify(&mut vm, &[(0, 0, GREEN), (0, 1, BLACK), (16, 1, BLUE)]);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.background[0], 2);
    assert_eq!(frame.background[512], 0);
    assert_eq!(frame.background[512 + 16], 2);
}

#[test]
fn sprite_fetch_continues_with_both_output_gates_off() {
    let mut vm = sprite_fixture();
    write(&mut vm, 0xeb0808, 0);
    write(&mut vm, 0xe82600, 0);
    vm.bus.advance_peripherals(LINE + 66);
    write(&mut vm, 0xeb8080, 0x3333);
    write(&mut vm, 0xeb0808, 0x200);
    write(&mut vm, 0xe82600, 0x40);
    vm.bus.advance_peripherals(130 * LINE);
    verify(&mut vm, &[(0, 0, BLUE), (15, 0, BLUE)]);
}

#[test]
fn sprite_priority_interleaves_with_background_planes_in_both_gpu_paths() {
    let mut vm = sprite_fixture();
    write(&mut vm, 0xebc000, 0x0100);
    write(&mut vm, 0xebe000, 0x0200);
    write(&mut vm, 0xe82222, 0xf800); // BG0 green.
    write(&mut vm, 0xe82242, 0x07c0); // BG1 red.
    for (priority, control, expected) in [
        (1, 0x219, GREEN),
        (1, 0x218, RED),
        (2, 0x219, GREEN),
        (2, 0x218, BLUE),
        (3, 0x219, BLUE),
    ] {
        write(&mut vm, 0xeb0006, priority);
        write(&mut vm, 0xeb0808, control);
        verify(&mut vm, &[(0, 0, expected)]);
    }
}
