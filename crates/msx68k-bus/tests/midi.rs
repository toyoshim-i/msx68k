use std::sync::{Arc, Mutex};

use msx68k_bus::midi::irq_bit;
use msx68k_bus::{
    Bus, CpuSpeedMode, MidiByteEvent, MidiCallbackPort, MidiIrqLevel, MidiMessageParser, X68kVm,
};

fn load_words(vm: &mut X68kVm, base: u32, words: &[u16]) {
    for (i, &w) in words.iter().enumerate() {
        vm.bus.write_u16(base + (i as u32) * 2, w, true).unwrap();
    }
}

#[test]
fn guest_probe_detects_absent_vs_installed_midi_board_via_bus_error() {
    let mut vm = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz10);
    // Reset vectors: SSP = $0001_0000, PC = $0000_0400
    vm.bus.write_u32(0x0000, 0x0001_0000, true).unwrap();
    vm.bus.write_u32(0x0004, 0x0000_0400, true).unwrap();
    // Bus Error vector (Vector 2 -> $0008): handler at $0000_0500
    vm.bus.write_u32(0x0008, 0x0000_0500, true).unwrap();

    // $0400: MOVEQ #0, D0 ; MOVE.B $00EAFA01.L, D1 ; MOVEQ #1, D0 ; BRA.S *
    load_words(
        &mut vm,
        0x0400,
        &[
            0x7000, // MOVEQ #0, D0
            0x1239, 0x00EA, 0xFA01, // MOVE.B $00EAFA01.L, D1
            0x7001, // MOVEQ #1, D0 (reached only if board exists)
            0x60FE, // BRA.S *
        ],
    );
    // $0500 (Bus Error handler): MOVEQ #-1, D0 ; BRA.S *
    load_words(
        &mut vm,
        0x0500,
        &[
            0x70FF, // MOVEQ #-1, D0
            0x60FE, // BRA.S *
        ],
    );

    // 1. Default (disabled): Bus Error handler runs -> D0 == -1
    vm.reset().unwrap();
    vm.run_cycles(200);
    assert_eq!(vm.cpu_state.d[0], 0xFFFF_FFFF);

    // 2. Installed: probe succeeds -> D0 == 1
    vm.set_midi_board_enabled(0, true);
    vm.reset().unwrap();
    vm.run_cycles(200);
    assert_eq!(vm.cpu_state.d[0], 1);
    assert!(vm.bus.access_flags().sound_io_accessed);
}

#[test]
fn guest_68k_transmits_midi_out_via_callback_and_channel_without_vsync_wait() {
    let mut vm = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz10);
    let captured = Arc::new(Mutex::new(Vec::<MidiByteEvent>::new()));
    let captured_cb = Arc::clone(&captured);

    vm.attach_midi_port(
        0,
        Box::new(MidiCallbackPort::new(move |byte, tick_10mhz| {
            captured_cb
                .lock()
                .unwrap()
                .push(MidiByteEvent { tick_10mhz, byte });
        })),
    );

    // Reset vectors: SSP = $0001_0000, PC = $0000_0400
    vm.bus.write_u32(0x0000, 0x0001_0000, true).unwrap();
    vm.bus.write_u32(0x0004, 0x0000_0400, true).unwrap();

    // $0400:
    //   MOVE.B #$05, $00EAFA03.L   ; select group 5 (FIFO-Tx)
    //   MOVE.B #$90, $00EAFA0D.L   ; Note On ch 0
    //   MOVE.B #$3C, $00EAFA0D.L   ; Middle C
    //   MOVE.B #$7F, $00EAFA0D.L   ; Velocity 127
    //   BRA.S  *
    load_words(
        &mut vm,
        0x0400,
        &[
            0x13FC, 0x0005, 0x00EA, 0xFA03,
            0x13FC, 0x0090, 0x00EA, 0xFA0D,
            0x13FC, 0x003C, 0x00EA, 0xFA0D,
            0x13FC, 0x007F, 0x00EA, 0xFA0D,
            0x60FE,
        ],
    );

    vm.reset().unwrap();
    // Run just enough cycles for the 4 stores (far less than 1 VSYNC = ~166,666 cycles)
    vm.run_cycles(200);
    // First byte is already delivered immediately
    assert_eq!(captured.lock().unwrap().len(), 1);
    assert_eq!(captured.lock().unwrap()[0].byte, 0x90);

    // Run ~1 ms (10,000 cycles @ 10 MHz) -> all 3 bytes shifted out at 31,250 bps (320 us/byte)
    vm.run_cycles(10_000);
    let events = captured.lock().unwrap().clone();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].byte, 0x90);
    assert_eq!(events[1].byte, 0x3C);
    assert_eq!(events[2].byte, 0x7F);
    assert_eq!(events[1].tick_10mhz - events[0].tick_10mhz, 3_200);
    assert_eq!(events[2].tick_10mhz - events[1].tick_10mhz, 3_200);

    // Reassemble via MidiMessageParser
    let mut parser = MidiMessageParser::new();
    let mut messages = Vec::new();
    for ev in &events {
        if let Some(msg) = parser.push(ev.byte) {
            messages.push(msg);
        }
    }
    assert_eq!(messages, vec![vec![0x90, 0x3C, 0x7F]]);
}

#[test]
fn host_midi_in_triggers_irq4_vector_handler_on_68k() {
    let mut vm = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz10);
    let endpoint = vm.attach_midi_channel(0);

    // Reset vectors: SSP = $0001_0000, PC = $0000_0400
    vm.bus.write_u32(0x0000, 0x0001_0000, true).unwrap();
    vm.bus.write_u32(0x0004, 0x0000_0400, true).unwrap();
    // Vector $8A (IVR $80 | RX_READY bit 5 << 1 = $8A -> address $8A * 4 = $0228)
    vm.bus.write_u32(0x0228, 0x0000_0500, true).unwrap();

    // $0400:
    //   MOVE.B #$00, $00EAFA03.L   ; group 0
    //   MOVE.B #$80, $00EAFA09.L   ; R04 IVR = $80
    //   MOVE.B #$20, $00EAFA0D.L   ; R06 IER = RX_READY (bit 5)
    //   ANDI.W #$F8FF, SR          ; unmask interrupts (IPL = 0)
    //   BRA.S  *
    load_words(
        &mut vm,
        0x0400,
        &[
            0x13FC, 0x0000, 0x00EA, 0xFA03,
            0x13FC, 0x0080, 0x00EA, 0xFA09,
            0x13FC, 0x0020, 0x00EA, 0xFA0D,
            0x027C, 0xF8FF, // ANDI.W #$F8FF, SR
            0x60FE, // BRA.S *
        ],
    );

    // $0500 (IRQ4 RX handler):
    //   MOVE.B #$03, $00EAFA03.L   ; group 3 (FIFO-Rx)
    //   MOVE.B $00EAFA0D.L, D2     ; R36 read received byte into D2
    //   ADDQ.L #1, D3              ; count received interrupts
    //   RTE
    load_words(
        &mut vm,
        0x0500,
        &[
            0x13FC, 0x0003, 0x00EA, 0xFA03,
            0x1439, 0x00EA, 0xFA0D,
            0x5283, // ADDQ.L #1, D3
            0x4E73, // RTE
        ],
    );

    vm.reset().unwrap();
    vm.run_cycles(300);
    assert_eq!(vm.cpu_state.d[3], 0);

    // Push a MIDI byte from host -> VM takes IRQ4 vector $8A
    endpoint.send_byte(0xC0).unwrap();
    vm.run_cycles(400);
    assert_eq!(vm.cpu_state.d[3], 1);
    assert_eq!(vm.cpu_state.d[2] & 0xFF, 0xC0);
}

#[test]
fn board1_uses_irq2_independently_from_board0() {
    let mut vm = X68kVm::new_app_vm_with_speed(CpuSpeedMode::Mhz10);
    vm.set_midi_board_enabled(1, true);
    vm.set_midi_board_irq(1, MidiIrqLevel::Irq2);

    // Reset vectors: SSP = $0001_0000, PC = $0000_0400
    vm.bus.write_u32(0x0000, 0x0001_0000, true).unwrap();
    vm.bus.write_u32(0x0004, 0x0000_0400, true).unwrap();
    // Vector $CE (IVR $C0 | GENERAL_TIMER bit 7 << 1 = $CE -> address $CE * 4 = $0338)
    vm.bus.write_u32(0x0338, 0x0000_0500, true).unwrap();

    // $0400: configure Board 1 ($EAFA1x) General Purpose Timer for 5 ticks (40 us = 400 cycles)
    load_words(
        &mut vm,
        0x0400,
        &[
            0x13FC, 0x0000, 0x00EA, 0xFA13, // group 0
            0x13FC, 0x00C0, 0x00EA, 0xFA19, // IVR = $C0
            0x13FC, irq_bit::GENERAL_TIMER as u16, 0x00EA, 0xFA1D, // IER = GENERAL_TIMER
            0x13FC, 0x0008, 0x00EA, 0xFA13, // group 8
            0x13FC, 0x0005, 0x00EA, 0xFA19, // R84 = 5
            0x13FC, 0x0080, 0x00EA, 0xFA1B, // R85 = 0x80 (load & start)
            0x027C, 0xF8FF, // unmask interrupts
            0x60FE,
        ],
    );

    // $0500 (IRQ2 timer handler): clear GT IRQ via R03 ($EAFA17), increment D4, RTE
    load_words(
        &mut vm,
        0x0500,
        &[
            0x13FC, irq_bit::GENERAL_TIMER as u16, 0x00EA, 0xFA17,
            0x5284, // ADDQ.L #1, D4
            0x4E73, // RTE
        ],
    );

    vm.reset().unwrap();
    vm.run_cycles(2_000);
    assert!(vm.cpu_state.d[4] >= 3, "expected periodic IRQ2 timer ticks, got {}", vm.cpu_state.d[4]);
}
