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
    assert_eq!(*frame.palette(frame.lines[0].state.palette_id), [0; 512]);
    assert_eq!(frame.lines[1].y, 64);
    assert_eq!(frame.lines[1].state.registers.text_scroll.0, 63);
    assert_eq!(frame.palette(frame.lines[1].state.palette_id)[1], 63);
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
    assert_eq!(frame.palette(frame.lines[0].state.palette_id)[1], 0xf800);
    assert_eq!(frame.lines[1].y, 1);
    assert_eq!(frame.lines[1].state.registers.text_scroll.0, 7);
    assert_eq!(frame.palette(frame.lines[1].state.palette_id)[1], 0x003e);
}

#[test]
fn cynthia_active_span_coalescing_handles_doubler_limit_palette_pcg_and_line_double() {
    use msx68k_bus::{bg::BgLine, cynthia::CynthiaFrame, sprite::SpriteLine};

    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[2] = 28;
    vm.bus.crtc.regs[3] = 92; // 512px wide
    vm.bus.crtc.regs[6] = 2;
    vm.bus.crtc.regs[7] = 130; // 128 lines
    vm.bus.crtc.regs[20] = 0x0314;
    vm.bus.write_u16(0xe82600, 0x40, true).unwrap(); // VICON sprite/BG ON
    vm.bus.write_u16(0xeb080c, 32, true).unwrap();
    vm.bus.write_u16(0xeb080e, 2, true).unwrap();
    vm.bus.write_u16(0xeb0808, 0x209, true).unwrap(); // Master + BG0 + BG1

    // Fill patterns 1..=5
    for pat in 1..=5u32 {
        for off in (0..128).step_by(2) {
            vm.bus
                .write_u16(0xeb8000 + pat * 128 + off, (pat as u16) * 0x1111, true)
                .unwrap();
        }
    }
    // Map BG0/BG1 with distinct tiles (8-dot pattern 4 is at $EB8080 = 16-dot pattern 1)
    vm.bus.write_u16(0xebc000, 0x0104, true).unwrap();
    vm.bus.write_u16(0xebc080, 0x0104, true).unwrap();
    vm.bus.write_u16(0xebe000, 0x0208, true).unwrap();

    // Sprite 0: static 16x16 at (16, 16) -> (x=0, y=0..16), pal=1, pat=3, prio=3
    for (off, v) in [(0, 16), (2, 16), (4, 0x0103), (6, 3)] {
        vm.bus.write_u16(0xeb0000 + off, v, true).unwrap();
    }
    // Sprite 1..=33 at y=32..48, x=48.. (overlapping rows 16..32, sprite 32 pushed out initially)
    for i in 1..=33u32 {
        let base = 0xeb0000 + i * 8;
        let x = if i == 32 { 64 } else { 48 + i as u16 };
        vm.bus.write_u16(base, x, true).unwrap();
        vm.bus.write_u16(base + 2, 32, true).unwrap();
        vm.bus.write_u16(base + 4, 0x0204, true).unwrap();
        vm.bus.write_u16(base + 6, 2, true).unwrap();
    }

    // Also test parity with BgLine::output_with_sprites(SpriteLine) on live bus
    let live = CynthiaFrame::from_live_bus(
        &vm.bus.crtc,
        vm.bus.sprite_bg(),
        vm.bus.sprite_bg_extra(),
        vm.bus.sprite_options,
        vm.bus.vicon.r2_display(),
        128,
    );
    for y in 0..48u32 {
        let ref_codes = BgLine::fetch_with_bank(
            &vm.bus.crtc,
            vm.bus.sprite_bg(),
            vm.bus.sprite_bg_extra(),
            y,
            false,
        )
        .output_with_sprites(
            0x209,
            &SpriteLine::fetch_with_options(
                &vm.bus.crtc,
                vm.bus.sprite_bg(),
                vm.bus.sprite_bg_extra(),
                y,
                vm.bus.sprite_options,
            ),
        );
        for x in 0..512u32 {
            assert_eq!(live.sample_code(x, y), ref_codes[x as usize], "live ({x},{y})");
        }
    }
    // Static 16-row sprites should be coalesced into 1 span per sprite (33 admitted sprites total)
    assert_eq!(live.spans.len(), 33);

    // Now test mid-frame raster updates:
    // - Palette color animation on every scanline must NOT split sprite spans.
    // - Mid-sprite rewrite of sprite 0 at y=8 (XSP doubler) splits sprite 0 into two spans.
    // - Mid-frame disable of sprite 1 on a single row y=20 admits sprite 32 on that single row!
    // - Mid-frame PCG block 3 rewrite splits affected spans cleanly.
    vm.bus.advance_peripherals(LINE); // raster 1 (sprite prefetch for y=0 is at raster 1)
    for r in 1..132u32 {
        // Palette animation every scanline
        vm.bus
            .write_u16(0xe82202, (r as u16) * 0x0101, true)
            .unwrap();
        if r == 9 {
            // Prefetch for y=8 (sprite_next = 11): rewrite sprite 0 to x=32, pat=5 (doubler / mid-split)
            vm.bus.write_u16(0xeb0000, 32, true).unwrap();
            vm.bus.write_u16(0xeb0004, 0x0105, true).unwrap();
        }
        if r == 19 {
            // Prefetch for y=18: temporarily disable sprite 1 so sprite 32 gets admitted on y=18!
            vm.bus.write_u16(0xeb0008 + 6, 0, true).unwrap();
        }
        if r == 20 {
            // Re-enable sprite 1 for y=19..32
            vm.bus.write_u16(0xeb0008 + 6, 2, true).unwrap();
        }
        if r == 25 {
            // Rewrite PCG pattern 4 mid-way
            for off in (0..128).step_by(2) {
                vm.bus
                    .write_u16(0xeb8000 + 4 * 128 + off, 0x7777, true)
                    .unwrap();
            }
        }
        vm.bus.advance_peripherals(LINE);
    }

    let frame = vm.bus.raster_frame().unwrap();
    // Palette changes every scanline -> many RasterLines, yet spans stayed coalesced!
    assert!(frame.lines.len() > 64);
    assert!(
        frame.cynthia.spans.len() < 120,
        "expected coalesced spans, got {}",
        frame.cynthia.spans.len()
    );
    // Sprite 0: y=0..8 at x=0 (pat=3), y=8..16 at x=16 (pat=5)
    assert_eq!(frame.sample_cynthia_code(0, 7), 0x13);
    assert_eq!(frame.sample_cynthia_code(0, 8), 0x11); // BG0 shows where sprite 0 moved away
    assert_eq!(frame.sample_cynthia_code(16, 8), 0x15);
    assert_eq!(frame.sample_cynthia_code(60, 17), 0x24); // Covered by sprite 28 on y=17
    assert!(
        frame.cynthia.spans.iter().any(|s| s.attr_index() == 33 && s.y0() == 18 && s.y1() == 19),
        "sprite 33 must be admitted only on y=18"
    );
    // Sprite 1 should be split into y=16..18 and y=19..24 and y=24..32 (due to PCG 4 rewrite at r=25 -> sprite_next=27 -> y=24)
    let sp1_spans: Vec<_> = frame
        .cynthia
        .spans
        .iter()
        .filter(|s| s.attr_index() == 1)
        .map(|s| (s.y0(), s.y1(), s.pcg_block_id()))
        .collect();
    assert_eq!(sp1_spans.len(), 3, "sprite 1 spans: {sp1_spans:?}");
    assert_eq!((sp1_spans[0].0, sp1_spans[0].1), (16, 18));
    assert_eq!((sp1_spans[1].0, sp1_spans[1].1), (19, 24));
    assert_eq!((sp1_spans[2].0, sp1_spans[2].1), (24, 32));
    assert_ne!(sp1_spans[1].2, sp1_spans[2].2);
    assert_eq!(frame.sample_cynthia_code(33, 23), 0x24);
    assert_eq!(frame.sample_cynthia_code(33, 24), 0x27);

    let mut row_buf = [0u8; 512];
    for y in [0, 7, 8, 17, 18, 23, 24, 31, 32] {
        frame.cynthia.rasterize_row(y, &mut row_buf);
        for x in 0..512u32 {
            assert_eq!(
                row_buf[x as usize],
                frame.sample_cynthia_code(x, y),
                "rasterize_row mismatch at ({x}, {y})"
            );
        }
    }
}

#[test]
fn cynthia_line_doubled_mode_matches_output_row_mapping() {
    let mut vm = X68kVm::new_app_vm();
    vm.bus.crtc.regs[2] = 28;
    vm.bus.crtc.regs[3] = 92;
    vm.bus.crtc.regs[6] = 0;
    vm.bus.crtc.regs[7] = 512;
    vm.bus.crtc.regs[20] = 0x0410; // 512 physical rasters -> 256 output rows
    vm.bus.write_u16(0xe82600, 0x40, true).unwrap();
    vm.bus.write_u16(0xeb080c, 32, true).unwrap();
    vm.bus.write_u16(0xeb0808, 0x200, true).unwrap();
    for off in (0..128).step_by(2) {
        vm.bus.write_u16(0xeb8080 + off, 0x4444, true).unwrap();
    }
    for (off, v) in [(0, 16), (2, 16), (4, 0x0101), (6, 3)] {
        vm.bus.write_u16(0xeb0000 + off, v, true).unwrap();
    }
    vm.bus.advance_peripherals(568 * LINE);
    let frame = vm.bus.raster_frame().unwrap();
    assert_eq!(frame.height, 256);
    assert_eq!(frame.sample_cynthia_code(0, 0), 0x14);
    assert_eq!(frame.sample_cynthia_code(0, 15), 0x14);
    assert_eq!(frame.sample_cynthia_code(0, 16), 0);
}
