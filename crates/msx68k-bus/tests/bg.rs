use msx68k_bus::{bg::BgLine, Bus, X68kVm};

fn write(vm: &mut X68kVm, offset: u32, value: u16) {
    vm.bus.write_u16(0xeb0000 + offset, value, true).unwrap();
}
fn fixture() -> X68kVm {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[2] = 28;
    vm.bus.crtc.regs[6] = 0;
    write(&mut vm, 0x80c, 32);
    vm
}
fn row(vm: &X68kVm, y: u32, control: u16) -> [u8; 512] {
    BgLine::fetch(&vm.bus.crtc, vm.bus.sprite_bg(), y).output(control)
}

#[test]
fn small_tiles_use_independent_maps_palettes_flips_and_wrapping() {
    let mut vm = fixture();
    write(&mut vm, 0x8000, 0x1200);
    write(&mut vm, 0xc000, 0x0300);
    write(&mut vm, 0xe000, 0x0400);
    write(&mut vm, 0x808, 1 << 4); // BG1 uses the second map.
    let codes = row(&vm, 0, 0x209);
    assert_eq!(&codes[..3], &[0x31, 0x32, 0]);
    assert_eq!(&row(&vm, 0, 0x208)[..3], &[0x41, 0x42, 0]);
    write(&mut vm, 0xe000, 0x0401);
    write(&mut vm, 0x8020, 0x0030);
    assert_eq!(&row(&vm, 0, 0x209)[..3], &[0x31, 0x32, 0x43]);
    write(&mut vm, 0xc000, 0xc300); // Both flips: bottom-right becomes top-left.
    write(&mut vm, 0x801e, 0x0056);
    assert_eq!(&row(&vm, 0, 0x201)[..3], &[0x36, 0x35, 0]);
    write(&mut vm, 0x800, 511);
    write(&mut vm, 0xc07e, 0x0300);
    write(&mut vm, 0xc000, 0x0400);
    write(&mut vm, 0x8002, 0x0007);
    assert_eq!(&row(&vm, 0, 0x201)[..3], &[0x37, 0x41, 0x42]);
}

#[test]
fn large_tiles_address_all_four_quadrants_and_disable_the_second_plane() {
    let mut vm = fixture();
    write(&mut vm, 0x810, 1);
    write(&mut vm, 0xc000, 0x0201);
    for (offset, value) in [
        (0x8080, 0x1000),
        (0x80a0, 0x2000),
        (0x80c0, 0x3000),
        (0x80e0, 0x4000),
    ] {
        write(&mut vm, offset, value);
    }
    assert_eq!(row(&vm, 0, 0x201)[0], 0x21);
    assert_eq!(row(&vm, 0, 0x201)[8], 0x23);
    assert_eq!(row(&vm, 8, 0x201)[0], 0x22);
    assert_eq!(row(&vm, 8, 0x201)[8], 0x24);
    assert_eq!(row(&vm, 0, 0x208), [0; 512]);
}

#[test]
fn output_gates_do_not_control_fetch_and_zero_nibbles_are_transparent() {
    let mut vm = fixture();
    write(&mut vm, 0xc000, 0x0f00); // Palette block alone is transparent.
    assert_eq!(row(&vm, 0, 0x201), [0; 512]);
    write(&mut vm, 0x8000, 0x1000);
    let fetched = BgLine::fetch(&vm.bus.crtc, vm.bus.sprite_bg(), 0);
    write(&mut vm, 0x8000, 0x2000);
    assert_eq!(fetched.output(1), [0; 512]);
    assert_eq!(fetched.output(0x201)[0], 0xf1);
    assert_eq!(row(&vm, 0, 0x201)[0], 0xf2);
}

#[test]
fn vertical_scroll_and_start_registers_choose_the_expected_map_cell() {
    let mut vm = fixture();
    write(&mut vm, 0x802, 511);
    write(&mut vm, 0xc000 + 63 * 128, 0x0200);
    write(&mut vm, 0x801c, 0x1000);
    write(&mut vm, 0x8000, 0x2000);
    assert_eq!(row(&vm, 0, 0x201)[0], 0x21);
    assert_eq!(row(&vm, 1, 0x201)[0], 2);
    write(&mut vm, 0x80c, 33); // Shift the pattern eight dots right.
    assert_eq!(row(&vm, 0, 0x201)[8], 0x21);
    write(&mut vm, 0x80e, 1); // Advance the BG source by one line.
    assert_eq!(row(&vm, 0, 0x201)[8], 2);
}
