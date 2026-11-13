use msx68k_bus::{bg::BgLine, sprite::SpriteLine, Bus, X68kVm};

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
fn sprite(vm: &mut X68kVm, index: u32, x: u16, y: u16, attributes: u16, priority: u16) {
    for (offset, value) in [(0, x), (2, y), (4, attributes), (6, priority)] {
        write(vm, index * 8 + offset, value);
    }
}
fn fill(vm: &mut X68kVm, pattern: u32, code: u16) {
    for offset in (0..128).step_by(2) {
        write(vm, 0x8000 + pattern * 128 + offset, code * 0x1111);
    }
}
fn output(vm: &X68kVm, y: u32) -> [u8; 512] {
    BgLine::default().output_with_sprites(
        0x200,
        &SpriteLine::fetch(&vm.bus.crtc, vm.bus.sprite_bg(), y),
    )
}

#[test]
fn coordinates_quadrants_flips_and_clipping_use_sixteen_dot_patterns() {
    let mut vm = fixture();
    for (offset, code) in [(0, 1), (32, 2), (64, 3), (96, 4)] {
        write(&mut vm, 0x8080 + offset, code << 12);
    }
    sprite(&mut vm, 0, 16, 16, 0x0201, 1);
    assert_eq!(output(&vm, 0)[0], 0x21);
    assert_eq!(output(&vm, 8)[0], 0x22);
    assert_eq!(output(&vm, 0)[8], 0x23);
    assert_eq!(output(&vm, 8)[8], 0x24);
    sprite(&mut vm, 0, 16, 16, 0xc201, 1);
    assert_eq!(output(&vm, 7)[7], 0x24);
    assert_eq!(output(&vm, 15)[15], 0x21);
    fill(&mut vm, 1, 5);
    sprite(&mut vm, 0, 1, 1, 0x0201, 1);
    assert_eq!(&output(&vm, 0)[..2], &[0x25, 0]);
    assert_eq!(output(&vm, 1), [0; 512]);
}

#[test]
fn higher_priority_wins_and_equal_priority_preserves_lower_register_number() {
    let mut vm = fixture();
    fill(&mut vm, 1, 1);
    fill(&mut vm, 2, 2);
    sprite(&mut vm, 0, 16, 16, 0x0101, 1);
    sprite(&mut vm, 1, 16, 16, 0x0202, 3);
    assert_eq!(output(&vm, 0)[0], 0x22);
    write(&mut vm, 6, 3);
    assert_eq!(output(&vm, 0)[0], 0x11);
    write(&mut vm, 0x8080, 0x0100); // Transparent front pixel exposes the other sprite.
    assert_eq!(&output(&vm, 0)[..2], &[0x22, 0x11]);
}

#[test]
fn sprite_priorities_interleave_with_both_background_planes() {
    let mut vm = fixture();
    fill(&mut vm, 1, 1);
    write(&mut vm, 0x8200, 0x3333); // Small pattern 16.
    write(&mut vm, 0x8220, 0x4444); // Small pattern 17.
    write(&mut vm, 0xc000, 0x0210);
    write(&mut vm, 0xe000, 0x0311);
    write(&mut vm, 0x808, 2 << 4);
    let bg = BgLine::fetch(&vm.bus.crtc, vm.bus.sprite_bg(), 0);
    for (priority, both, only_bg1) in [(1, 0x23, 0x34), (2, 0x23, 0x11), (3, 0x11, 0x11)] {
        sprite(&mut vm, 0, 16, 16, 0x0101, priority);
        let line = SpriteLine::fetch(&vm.bus.crtc, vm.bus.sprite_bg(), 0);
        assert_eq!(bg.output_with_sprites(0x209, &line)[0], both);
        assert_eq!(bg.output_with_sprites(0x208, &line)[0], only_bg1);
        assert_eq!(bg.output_with_sprites(9, &line), [0; 512]);
    }
}

#[test]
fn line_limit_counts_blank_patterns_but_excludes_disabled_and_offscreen_entries() {
    let mut vm = fixture();
    fill(&mut vm, 1, 1);
    for index in 0..32 {
        sprite(&mut vm, index, 16, 16, 0, 1); // Selected, even though the pattern is blank.
    }
    sprite(&mut vm, 32, 16, 16, 1, 3);
    assert_eq!(output(&vm, 0)[0], 0);
    write(&mut vm, 6, 0);
    assert_eq!(output(&vm, 0)[0], 1);
    write(&mut vm, 6, 1);
    write(&mut vm, 0, 0); // Entirely to the left.
    assert_eq!(output(&vm, 0)[0], 1);
    // The limit uses the active viewport, not the full code-buffer capacity.
    vm.bus.crtc.regs[3] = 60;
    write(&mut vm, 0, 300);
    assert_eq!(output(&vm, 0)[0], 1);
    write(&mut vm, 0, 16);
    write(&mut vm, 2, 32); // Different vertical line.
    assert_eq!(output(&vm, 0)[0], 1);
}
