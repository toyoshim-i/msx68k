use msx68k_bus::{Bus, X68kVm};
const LINE: u32 = 317;

fn fixture() -> X68kVm {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 128;
    vm
}

#[test]
fn batched_time_does_not_skip_the_raster_irq_or_frame_boundaries() {
    let mut vm = fixture();
    vm.bus.write_u16(0xe80012, 65, true).unwrap();
    vm.bus.write_u8(0xe88007, 0x40, true).unwrap();
    vm.bus.write_u8(0xe88013, 0x40, true).unwrap();
    vm.bus.advance_peripherals(129 * LINE);
    assert_eq!(vm.bus.crtc.frame_count(), 1);
    assert_eq!(vm.bus.mfp.highest_pending_channel(), Some(14));
    assert_eq!(vm.bus.raster_frame().unwrap().lines.len(), 1);
    // One batch can cross multiple frames without merging their histories.
    vm.bus.advance_peripherals(568 * LINE * 3);
    assert_eq!(vm.bus.crtc.frame_count(), 4);
    assert_eq!(vm.bus.raster_frame().unwrap().lines.len(), 1);
}

#[test]
fn unchanged_palette_writes_coalesce_and_reset_discards_history() {
    let mut vm = fixture();
    for _ in 0..129 {
        vm.bus.write_u16(0xe82202, 0x5294, true).unwrap();
        vm.bus.advance_peripherals(LINE);
    }
    assert_eq!(vm.bus.raster_frame().unwrap().lines.len(), 1);
    vm.reset().unwrap();
    assert!(vm.bus.raster_frame().is_none());
}

#[test]
fn line_doubled_output_maps_two_rasters_to_one_output_row() {
    let mut vm = fixture();
    vm.bus.crtc.regs[7] = 512;
    vm.bus.crtc.regs[20] = 0x0410; // 512 physical rasters, 256 output rows.
    vm.bus.advance_peripherals(64 * LINE + 66);
    vm.bus.write_u16(0xe82202, 0xf800, true).unwrap();
    vm.bus.advance_peripherals((513 - 64) * LINE);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.height, 256);
    assert_eq!(
        frame.lines.iter().map(|line| line.y).collect::<Vec<_>>(),
        [0, 32]
    );
}

#[test]
fn batched_time_preserves_both_horizontal_sync_edges() {
    let mut vm = fixture();
    vm.bus.write_u8(0xe88007, 0x80, true).unwrap();
    vm.bus.write_u8(0xe88013, 0x80, true).unwrap();
    vm.bus.advance_peripherals(LINE);
    assert!(vm.bus.crtc.sync_state().h_sync);
    assert_eq!(vm.bus.mfp.highest_pending_channel(), Some(15));
}

#[test]
fn geometry_changes_discard_partial_history_until_a_full_frame() {
    let mut vm = fixture();
    vm.bus.advance_peripherals(129 * LINE);
    assert!(vm.bus.raster_frame().is_some());
    vm.bus.advance_peripherals((568 - 129 + 1) * LINE);
    vm.bus.crtc.regs[3] -= 4;
    vm.bus.advance_peripherals(LINE);
    assert!(vm.bus.raster_frame().is_none());
    // Finish the invalid frame, then wait until the next complete display ends.
    vm.bus.advance_peripherals((568 + 127) * LINE);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.width, 736);
    assert_eq!(frame.lines[0].y, 0);
}

#[test]
fn display_off_skips_state_until_reenabled() {
    let mut vm = fixture();
    vm.bus.write_u16(0xe82600, 0, true).unwrap();
    vm.bus.advance_peripherals(LINE + 66);
    for y in 1..64 {
        vm.bus.write_u16(0xe82202, y, true).unwrap();
        vm.bus.write_u16(0xe80014, y, true).unwrap();
        vm.bus.advance_peripherals(LINE);
    }
    vm.bus.write_u16(0xe82600, 0x20, true).unwrap();
    vm.bus.advance_peripherals(65 * LINE);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.lines.len(), 2);
    assert_eq!(frame.lines[0].state.registers.display, 0);
    assert_eq!(frame.lines[0].state.palettes, [0; 512]);
    assert_eq!(frame.lines[1].y, 64);
    assert_eq!(frame.lines[1].state.registers.text_scroll.0, 63);
    assert_eq!(frame.lines[1].state.palettes[1], 63);
}

#[test]
fn changing_vertical_origin_also_invalidates_the_row_map() {
    let mut vm = fixture();
    vm.bus.advance_peripherals(129 * LINE);
    vm.bus.advance_peripherals((568 - 129 + 32) * LINE);
    // Height is unchanged, but output row coordinates would move backwards.
    vm.bus.crtc.regs[6] += 16;
    vm.bus.crtc.regs[7] += 16;
    vm.bus.advance_peripherals(LINE);
    assert!(vm.bus.raster_frame().is_none());
    vm.bus.advance_peripherals(568 * LINE);
    vm.bus.advance_peripherals(112 * LINE);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.lines[0].y, 0);
    assert_eq!(frame.height, 128);
}

#[test]
fn address_fetch_and_color_conversion_have_independent_boundaries() {
    let mut vm = fixture();
    vm.bus.advance_peripherals(LINE); // Row zero address fetch.
    vm.bus.write_u16(0xe80014, 7, true).unwrap();
    vm.bus.advance_peripherals(65); // One cycle before horizontal display.
    vm.bus.write_u16(0xe82202, 0xf800, true).unwrap();
    vm.bus.advance_peripherals(1); // Row zero color conversion.
    vm.bus.write_u16(0xe82202, 0x003e, true).unwrap();
    vm.bus.advance_peripherals(128 * LINE);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.lines[0].y, 0);
    assert_eq!(frame.lines[0].state.registers.text_scroll.0, 0);
    assert_eq!(frame.lines[0].state.palettes[1], 0xf800);
    assert_eq!(frame.lines[1].y, 1);
    assert_eq!(frame.lines[1].state.registers.text_scroll.0, 7);
    assert_eq!(frame.lines[1].state.palettes[1], 0x003e);
}
