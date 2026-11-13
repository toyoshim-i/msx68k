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
