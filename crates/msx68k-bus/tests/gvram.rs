use msx68k_bus::{Bus, X68kVm};

const BASE: u32 = 0xc00000;

#[test]
fn indexed_pages_share_storage_without_aliasing_lanes() {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[20] = 0;
    for page in 0..4 {
        let addr = BASE + page * 0x80000;
        vm.bus
            .write_u16(addr, 0xfff0 | (page as u16 + 1), true)
            .unwrap();
        vm.bus.write_u8(addr, 0xff, true).unwrap();
        assert_eq!(vm.bus.read_u16(addr, true).unwrap(), page as u16 + 1);
        assert_eq!(vm.bus.peek_u8(addr + 1), Some(page as u8 + 1));
    }
    vm.bus.crtc.regs[20] = 0x300;
    assert_eq!(vm.bus.read_u16(BASE, true).unwrap(), 0x4321);
    vm.bus.crtc.regs[20] = 0x100;
    assert_eq!(vm.bus.read_u16(BASE, true).unwrap(), 0x21);
    assert_eq!(vm.bus.read_u16(BASE + 0x80000, true).unwrap(), 0x43);
    vm.bus.write_u16(BASE, 0xffab, true).unwrap();
    vm.bus.crtc.regs[20] = 0x200; // Both 2 and 3 select the 16-bit view.
    assert_eq!(vm.bus.read_u16(BASE, true).unwrap(), 0x43ab);
    vm.bus.write_u8(BASE, 0xcd, true).unwrap();
    assert_eq!(vm.bus.read_u16(BASE, true).unwrap(), 0xcdab);
}

#[test]
fn large_view_maps_quadrants_and_wraps_rows() {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[20] = 0x400;
    for (x, y, value) in [
        (0, 0, 1),
        (512, 0, 2),
        (0, 512, 3),
        (512, 512, 4),
        (1023, 1023, 15),
    ] {
        let addr = BASE + (y * 1024 + x) * 2;
        vm.bus.write_u16(addr, value, true).unwrap();
        assert_eq!(vm.bus.read_u16(addr, true).unwrap(), value);
    }
    vm.bus.crtc.regs[20] = 0x300;
    assert_eq!(vm.bus.read_u16(BASE, true).unwrap(), 0x4321);
    assert_eq!(vm.bus.read_u16(BASE + 0x7fffe, true).unwrap(), 0xf000);
}

#[test]
fn unused_windows_fault_and_unused_bytes_do_not_dirty_vram() {
    let mut vm = X68kVm::new_app_vm();
    for (mode, addr) in [(0x100, 0xd00000), (0x200, 0xc80000), (0x300, 0xdffffe)] {
        vm.bus.crtc.regs[20] = mode;
        assert!(vm.bus.read_u16(addr, true).is_err());
        assert!(vm.bus.write_u16(addr, 0xffff, true).is_err());
        assert_eq!(vm.bus.peek_u8(addr), None);
    }
    vm.bus.crtc.regs[20] = 0;
    vm.bus.take_written_vram_pages();
    vm.bus.write_u8(BASE, 0xff, true).unwrap();
    assert!(vm.bus.take_written_vram_pages().is_empty());
    vm.bus.write_u16(BASE + 0x183000, 5, true).unwrap();
    assert_eq!(
        vm.bus.take_written_vram_pages().gvram_ranges(),
        [0x3000..0x4000]
    );
}

#[test]
fn long_access_crosses_pages_and_scroll_masks_follow_size() {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[20] = 0;
    vm.bus.write_u32(BASE + 0x7fffe, 0x000a000b, true).unwrap();
    assert_eq!(vm.bus.read_u32(BASE + 0x7fffe, true).unwrap(), 0x000a000b);
    for mode in [0, 0x100, 0x300, 0x400] {
        vm.bus.crtc.regs[20] = mode;
        vm.bus.crtc.regs[12..20].fill(1023);
        assert_eq!(
            vm.bus.crtc.graphic_scroll(0),
            if mode == 0x400 {
                (1023, 1023)
            } else {
                (511, 511)
            }
        );
        assert_eq!(vm.bus.crtc.graphic_scroll(1), (511, 511));
    }
}

#[test]
fn guest_instructions_can_write_and_read_separate_planes() {
    let mut vm = X68kVm::new_app_vm();
    // MOVE.W #$0000,$E80028.L; MOVE.W #$000A,$C00000.L;
    // MOVE.W #$000B,$C80000.L; MOVE.W $C00000.L,D0; MOVE.W $C80000.L,D1.
    let program = [
        0x33fc, 0, 0xe8, 0x0028, 0x33fc, 10, 0xc0, 0, 0x33fc, 11, 0xc8, 0, 0x3039, 0xc0, 0, 0x3239,
        0xc8, 0,
    ];
    vm.bus.write_u32(0, 0x10000, true).unwrap();
    vm.bus.write_u32(4, 0x400, true).unwrap();
    for (index, word) in program.into_iter().enumerate() {
        vm.bus
            .write_u16(0x400 + index as u32 * 2, word, true)
            .unwrap();
    }
    vm.reset().unwrap();
    for _ in 0..5 {
        vm.step();
    }
    assert_eq!(vm.cpu_state.d[0] & 0xffff, 10);
    assert_eq!(vm.cpu_state.d[1] & 0xffff, 11);
    assert_eq!(vm.cpu_state.pc, 0x424);
}

#[test]
fn fast_clear_preserves_unselected_packed_pages() {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[20] = 0x300;
    vm.bus.write_u16(BASE, 0x4321, true).unwrap();
    vm.bus.write_u16(0xe8002a, 0x000a, true).unwrap();
    vm.bus.take_written_vram_pages();
    vm.bus.write_u8(0xe80481, 2, true).unwrap();
    vm.bus.advance_peripherals(200_000); // At least one vertical blank.
    assert_eq!(vm.bus.read_u16(BASE, true).unwrap(), 0x0301);
    assert_eq!(vm.bus.take_written_vram_pages().gvram, u128::MAX);
}
