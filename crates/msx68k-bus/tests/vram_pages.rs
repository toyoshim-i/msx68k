//! The bus records which 4 KB VRAM pages the guest writes, so that a VM's screen uploads only
//! those (`docs/VMX68K.md` Section 3.3).

// Expected ranges are written as arrays, one-element ones included.
#![allow(clippy::single_range_in_vec_init)]

use msx68k_bus::{Bus, DirtyVramPages, VmConfig, X68kVm, TVRAM_PLANE_BYTES};

fn vm() -> X68kVm {
    X68kVm::new(VmConfig::app_vm())
}

#[test]
fn cpu_writes_mark_the_pages_they_reach() {
    let mut vm = vm();
    assert!(vm.bus.take_written_vram_pages().is_empty());

    vm.bus.write_u8(0x00E0_1234, 0xFF, true).unwrap();
    vm.bus.write_u16(0x00E2_0000, 0xFFFF, true).unwrap();
    vm.bus.crtc.regs[20] = 0; // 512-dot indexed addressing.
    vm.bus.write_u16(0x00C0_3000, 0x1234, true).unwrap();
    let pages = vm.bus.take_written_vram_pages();
    assert_eq!(
        pages.tvram_ranges(),
        [0x1000..0x2000, TVRAM_PLANE_BYTES..TVRAM_PLANE_BYTES + 0x1000]
    );
    assert_eq!(pages.gvram_ranges(), [0x3000..0x4000]);
    assert!(vm.bus.take_written_vram_pages().is_empty());

    // Reads leave the record alone.
    vm.bus.read_u16(0x00E0_8000, true).unwrap();
    assert!(vm.bus.take_written_vram_pages().is_empty());
}

#[test]
fn simultaneous_access_marks_each_selected_plane() {
    let mut vm = vm();
    // R21: simultaneous access to planes 0 and 3.
    vm.bus.write_u16(0x00E8_002A, 0x0190, true).unwrap();
    vm.bus.take_written_vram_pages();
    vm.bus.write_u16(0x00E0_0010, 0xFFFF, true).unwrap();
    assert_eq!(
        vm.bus.take_written_vram_pages().tvram_ranges(),
        [0..0x1000, 3 * TVRAM_PLANE_BYTES..3 * TVRAM_PLANE_BYTES + 0x1000]
    );
}

#[test]
fn raster_copy_marks_its_destination() {
    let mut vm = vm();
    // R21 selects planes 0 and 1; R22 copies block 0 to block 64 (byte 0x8000 of a plane).
    vm.bus.write_u16(0x00E8_002A, 0x0003, true).unwrap();
    vm.bus.write_u16(0x00E8_002C, 0x0040, true).unwrap();
    vm.bus.take_written_vram_pages();
    vm.bus.write_u8(0x00E8_0481, 0x08, true).unwrap();
    assert_eq!(
        vm.bus.take_written_vram_pages().tvram_ranges(),
        [0x8000..0x9000, TVRAM_PLANE_BYTES + 0x8000..TVRAM_PLANE_BYTES + 0x9000]
    );
}

#[test]
fn direct_tvram_access_marks_all_of_it() {
    let mut vm = vm();
    vm.bus.tvram_mut()[0] = 1;
    let pages = vm.bus.take_written_vram_pages();
    assert_eq!(pages.tvram, DirtyVramPages::ALL.tvram);
    assert_eq!(pages.gvram, 0);
}
