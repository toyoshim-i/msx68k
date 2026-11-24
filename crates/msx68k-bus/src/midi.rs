//! SHARP CZ-6BM1 MIDI Interface Board (Yamaha YM3802 MCS).
//!
//! Maps up to two CZ-6BM1 boards into the X68000 expansion I/O window:
//! - Board 0 (standard): `$EAFA00`–`$EAFA0F` (odd byte addresses `$EAFA01..=$EAFA0F`), default IRQ4.
//! - Board 1 (optional): `$EAFA10`–`$EAFA1F` (odd byte addresses `$EAFA11..=$EAFA1F`), default IRQ2.
//!
//! Host applications (such as `czx`) attach an external [`MidiPort`] implementation or a
//! [`MidiChannelEndpoint`] via [`crate::X68kVm::attach_midi_port`] /
//! [`crate::X68kVm::attach_midi_channel`]. Outgoing bytes are pushed immediately from the VM
//! thread as the YM3802 transmitter shifts them out (with guest 10 MHz timestamps), and incoming
//! bytes are polled non-blockingly as peripherals advance so MIDI traffic never waits for VSYNC.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

use msx68k_cpu::BusFault;

/// Base address of CZ-6BM1 Board 0 (`$EAFA00`–`$EAFA0F`).
pub const MIDI_BOARD0_BASE: u32 = 0x00EA_FA00;
/// Base address of CZ-6BM1 Board 1 (`$EAFA10`–`$EAFA1F`).
pub const MIDI_BOARD1_BASE: u32 = 0x00EA_FA10;
/// Exclusive end of the two-board CZ-6BM1 I/O window.
pub const MIDI_WINDOW_END: u32 = 0x00EA_FA20;

/// Number of CZ-6BM1 board slots supported.
pub const MIDI_BOARD_COUNT: usize = 2;

/// YM3802 hardware receive FIFO capacity (bytes).
const RX_FIFO_CAPACITY: usize = 128;
/// YM3802 hardware transmit FIFO capacity (bytes).
const TX_FIFO_CAPACITY: usize = 16;

/// 10 MHz cycles per YM3802 timer tick (`1 MHz / 8 = 125 kHz` -> `8 us` = `80` cycles @ 10 MHz).
const TIMER_PRESCALE_10MHZ: u32 = 80;
/// 10 MHz cycles per MIDI byte at 31,250 bps (10 bits/byte = `320 us` = `3,200` cycles @ 10 MHz).
const TX_BYTE_CYCLES_10MHZ: u32 = 3_200;

/// Interrupt Service / Enable / Clear bit masks (`R02`, `R03`, `R06`).
pub mod irq_bit {
    /// Bit 0 (highest priority, vector offset `+$00`): Playback / Click Counter.
    pub const PLAYBACK_COUNTER: u8 = 0x01;
    /// Bit 1 (vector offset `+$02`): Real-Time Message Detect.
    pub const REALTIME_DETECT: u8 = 0x02;
    /// Bit 2 (vector offset `+$04`): Break Detect.
    pub const BREAK_DETECT: u8 = 0x04;
    /// Bit 3 (vector offset `+$06`): Off-Line Detect.
    pub const OFFLINE_DETECT: u8 = 0x08;
    /// Bit 4 (vector offset `+$08`): FIFO-Tx Empty / Ready.
    pub const TX_EMPTY: u8 = 0x10;
    /// Bit 5 (vector offset `+$0A`): FIFO-Rx Ready (Data Available).
    pub const RX_READY: u8 = 0x20;
    /// Bit 6 (vector offset `+$0C`): MIDI Clock Timer.
    pub const MIDI_CLOCK_TIMER: u8 = 0x40;
    /// Bit 7 (lowest priority, vector offset `+$0E`): General Purpose Timer.
    pub const GENERAL_TIMER: u8 = 0x80;
}

/// Hardware IRQ level selectable by the CZ-6BM1 board jumper.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum MidiIrqLevel {
    /// Interrupt Priority Level 2 (`IRQ2`, typical for Board 1).
    Irq2 = 2,
    /// Interrupt Priority Level 4 (`IRQ4`, factory default for Board 0).
    Irq4 = 4,
}

/// Timestamped outgoing MIDI byte produced by a YM3802 board.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct MidiByteEvent {
    /// Guest peripheral clock at transmission time, in 10 MHz ticks (`100 ns` units).
    pub tick_10mhz: u64,
    /// Raw MIDI byte transmitted by the guest.
    pub byte: u8,
}

/// Host-side MIDI I/O endpoint attached to a CZ-6BM1 board.
///
/// Called directly from the VM execution thread without waiting for VSYNC.
/// Implementations must not block.
pub trait MidiPort: Send {
    /// Called immediately when the YM3802 shifts out a MIDI byte.
    fn send_byte(&mut self, byte: u8, tick_10mhz: u64);

    /// Non-blocking poll for incoming MIDI bytes from the host.
    fn try_recv_byte(&mut self) -> Option<u8> {
        None
    }
}

/// Output-only [`MidiPort`] backed by a user closure.
pub struct MidiCallbackPort<F> {
    on_send: F,
}

impl<F: FnMut(u8, u64) + Send> MidiCallbackPort<F> {
    /// Wraps `on_send(byte, tick_10mhz)` as a [`MidiPort`].
    pub fn new(on_send: F) -> Self {
        Self { on_send }
    }
}

impl<F: FnMut(u8, u64) + Send> MidiPort for MidiCallbackPort<F> {
    #[inline]
    fn send_byte(&mut self, byte: u8, tick_10mhz: u64) {
        (self.on_send)(byte, tick_10mhz);
    }
}

/// VM-side [`MidiPort`] bridged over `std::sync::mpsc` channels.
pub struct MidiChannelPort {
    tx_out: Sender<MidiByteEvent>,
    rx_in: Receiver<u8>,
}

impl MidiPort for MidiChannelPort {
    #[inline]
    fn send_byte(&mut self, byte: u8, tick_10mhz: u64) {
        let _ = self.tx_out.send(MidiByteEvent { tick_10mhz, byte });
    }

    #[inline]
    fn try_recv_byte(&mut self) -> Option<u8> {
        match self.rx_in.try_recv() {
            Ok(byte) => Some(byte),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

/// Host-side handle paired with a [`MidiChannelPort`].
///
/// A host thread (e.g. a dedicated MIDI output thread in `czx`) can block on [`Self::recv`]
/// for low-latency delivery of outgoing bytes, and push incoming bytes from any OS MIDI
/// callback via [`Self::send_byte`] or [`Self::send_bytes`].
pub struct MidiChannelEndpoint {
    rx_out: Receiver<MidiByteEvent>,
    tx_in: Sender<u8>,
}

impl MidiChannelEndpoint {
    /// Blocks until the VM transmits the next MIDI byte, or returns `Err` if the port was dropped.
    pub fn recv(&self) -> Result<MidiByteEvent, mpsc::RecvError> {
        self.rx_out.recv()
    }

    /// Non-blocking receive of the next outgoing MIDI byte.
    pub fn try_recv(&self) -> Result<MidiByteEvent, TryRecvError> {
        self.rx_out.try_recv()
    }

    /// Sends a single incoming MIDI byte into the VM's YM3802 Rx FIFO.
    pub fn send_byte(&self, byte: u8) -> Result<(), mpsc::SendError<u8>> {
        self.tx_in.send(byte)
    }

    /// Sends a slice of incoming MIDI bytes into the VM's YM3802 Rx FIFO.
    pub fn send_bytes(&self, bytes: &[u8]) -> Result<(), mpsc::SendError<u8>> {
        for &b in bytes {
            self.tx_in.send(b)?;
        }
        Ok(())
    }

    /// Splits this endpoint into its raw `(outgoing_rx, incoming_tx)` channel halves.
    pub fn into_parts(self) -> (Receiver<MidiByteEvent>, Sender<u8>) {
        (self.rx_out, self.tx_in)
    }
}

/// Creates a paired `(vm_port, host_endpoint)` connected via `std::sync::mpsc` channels.
pub fn midi_channel() -> (MidiChannelPort, MidiChannelEndpoint) {
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_in, rx_in) = mpsc::channel();
    (
        MidiChannelPort { tx_out, rx_in },
        MidiChannelEndpoint { rx_out, tx_in },
    )
}

/// Incremental parser that reassembles a raw MIDI byte stream (including Running Status,
/// SysEx, and interleaved System Real-Time bytes) into complete MIDI messages.
#[derive(Debug, Default, Clone)]
pub struct MidiMessageParser {
    running_status: u8,
    expected_len: usize,
    in_sysex: bool,
    buf: Vec<u8>,
}

impl MidiMessageParser {
    /// Creates a new empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets parser state and discards any partial message.
    pub fn reset(&mut self) {
        self.running_status = 0;
        self.expected_len = 0;
        self.in_sysex = false;
        self.buf.clear();
    }

    /// Feeds one raw MIDI byte and returns a complete message when ready.
    pub fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        // System Real-Time messages ($F8..=$FF) may appear anywhere without
        // disturbing Running Status or an in-progress message.
        if byte >= 0xF8 {
            return Some(vec![byte]);
        }

        if byte >= 0x80 {
            if byte == 0xF0 {
                // Start of SysEx
                self.running_status = 0;
                self.expected_len = 0;
                self.in_sysex = true;
                self.buf.clear();
                self.buf.push(0xF0);
                return None;
            }
            if byte == 0xF7 {
                // End of SysEx
                self.running_status = 0;
                if self.in_sysex {
                    self.in_sysex = false;
                    self.buf.push(0xF7);
                    return Some(std::mem::take(&mut self.buf));
                }
                return None;
            }

            // Any other status byte aborts an unfinished SysEx.
            self.in_sysex = false;
            self.buf.clear();

            if byte < 0xF0 {
                // Channel Voice message: establishes Running Status.
                self.running_status = byte;
                self.expected_len = match byte & 0xF0 {
                    0xC0 | 0xD0 => 2,
                    _ => 3,
                };
                self.buf.push(byte);
                None
            } else {
                // System Common ($F1..=$F6): clears Running Status.
                self.running_status = 0;
                match byte {
                    0xF1 | 0xF3 => {
                        self.expected_len = 2;
                        self.buf.push(byte);
                        None
                    }
                    0xF2 => {
                        self.expected_len = 3;
                        self.buf.push(byte);
                        None
                    }
                    0xF6 => {
                        self.expected_len = 0;
                        Some(vec![0xF6])
                    }
                    _ => {
                        self.expected_len = 0;
                        None
                    }
                }
            }
        } else {
            // Data byte ($00..=$7F)
            if self.in_sysex {
                self.buf.push(byte);
                return None;
            }
            if self.buf.is_empty() {
                if self.running_status == 0 {
                    return None;
                }
                self.buf.push(self.running_status);
                self.expected_len = match self.running_status & 0xF0 {
                    0xC0 | 0xD0 => 2,
                    _ => 3,
                };
            }
            self.buf.push(byte);
            if self.expected_len > 0 && self.buf.len() >= self.expected_len {
                let msg = std::mem::take(&mut self.buf);
                return Some(msg);
            }
            None
        }
    }
}

/// Emulation of a single SHARP CZ-6BM1 board (Yamaha YM3802 MCS).
pub struct MidiBoard {
    /// Whether this board is installed on the expansion bus (absent boards raise `BusError`).
    enabled: bool,
    /// Hardware jumper IRQ level (`IRQ4` or `IRQ2`).
    irq_level: MidiIrqLevel,
    /// Attached external host port.
    port: Option<Box<dyn MidiPort>>,

    /// Register group select (`R01 & 0x0F`, groups `0..=9`).
    reg_group: u8,
    /// Raw register storage for groups `0..=9`, registers `+4..=+7` (`regs[group][offset]`).
    regs: [[u8; 4]; 10],

    /// Interrupt Vector Register base (`R04 & 0xE0`).
    ivr_base: u8,
    /// Interrupt Enable Register (`R06`).
    ier: u8,
    /// Raw pending interrupt flags (`R02` reports `isr & ier`).
    isr: u8,

    /// Receive FIFO (up to 128 bytes).
    rx_fifo: VecDeque<u8>,
    /// Receive FIFO overflow flag (`R34` bit 6).
    rx_overflow: bool,
    /// Last received Real-Time message byte (`R14` / `R15` helper).
    last_realtime: u8,

    /// Transmit FIFO (up to 16 bytes).
    tx_fifo: VecDeque<u8>,
    /// Remaining 10 MHz cycles until the current shifting byte finishes on the MIDI wire.
    tx_shift_remaining: u32,
    /// Guest 10 MHz clock accumulator for timestamps.
    clock_10mhz: u64,

    /// Prescaler accumulator toward the 125 kHz (8 us) timer tick.
    timer_prescale_acc: u32,

    /// 14-bit General Purpose Timer reload value (`R84` / `R85`).
    general_timer_reload: u16,
    /// 14-bit General Purpose Timer active down-counter.
    general_timer_counter: u16,

    /// 14-bit MIDI Clock Timer reload value (`R86` / `R87`).
    midi_timer_reload: u16,
    /// 14-bit MIDI Clock Timer active down-counter.
    midi_timer_counter: u16,
}

impl std::fmt::Debug for MidiBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MidiBoard")
            .field("enabled", &self.enabled)
            .field("irq_level", &self.irq_level)
            .field("port_attached", &self.port.is_some())
            .field("reg_group", &self.reg_group)
            .field("ivr_base", &self.ivr_base)
            .field("ier", &self.ier)
            .field("isr", &self.isr)
            .field("rx_fifo_len", &self.rx_fifo.len())
            .field("tx_fifo_len", &self.tx_fifo.len())
            .finish()
    }
}

impl MidiBoard {
    /// Creates a new (initially disabled) CZ-6BM1 board configured for `irq_level`.
    pub fn new(irq_level: MidiIrqLevel) -> Self {
        Self {
            enabled: false,
            irq_level,
            port: None,
            reg_group: 0,
            regs: [[0; 4]; 10],
            ivr_base: 0,
            ier: 0,
            isr: 0,
            rx_fifo: VecDeque::with_capacity(RX_FIFO_CAPACITY),
            rx_overflow: false,
            last_realtime: 0,
            tx_fifo: VecDeque::with_capacity(TX_FIFO_CAPACITY),
            tx_shift_remaining: 0,
            clock_10mhz: 0,
            timer_prescale_acc: 0,
            general_timer_reload: 0,
            general_timer_counter: 0,
            midi_timer_reload: 0,
            midi_timer_counter: 0,
        }
    }

    /// Resets YM3802 register/FIFO/timer state while keeping board installation, IRQ jumper,
    /// and the attached host [`MidiPort`].
    pub fn reset(&mut self) {
        self.reg_group = 0;
        self.regs = [[0; 4]; 10];
        self.ivr_base = 0;
        self.ier = 0;
        self.isr = 0;
        self.rx_fifo.clear();
        self.rx_overflow = false;
        self.last_realtime = 0;
        self.tx_fifo.clear();
        self.tx_shift_remaining = 0;
        self.timer_prescale_acc = 0;
        self.general_timer_reload = 0;
        self.general_timer_counter = 0;
        self.midi_timer_reload = 0;
        self.midi_timer_counter = 0;
    }

    /// Returns whether this board is installed.
    #[inline]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Installs or removes this board from the expansion bus.
    #[inline]
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns the configured IRQ jumper level.
    #[inline]
    pub const fn irq_level(&self) -> MidiIrqLevel {
        self.irq_level
    }

    /// Changes the board's IRQ jumper level (`IRQ4` or `IRQ2`).
    #[inline]
    pub fn set_irq_level(&mut self, irq_level: MidiIrqLevel) {
        self.irq_level = irq_level;
    }

    /// Attaches or detaches the external host [`MidiPort`].
    #[inline]
    pub fn set_port(&mut self, port: Option<Box<dyn MidiPort>>) {
        self.port = port;
    }

    /// Returns `true` if a host [`MidiPort`] is currently attached.
    #[inline]
    pub fn has_port(&self) -> bool {
        self.port.is_some()
    }

    /// Pushes an incoming MIDI byte directly into the YM3802 Rx FIFO.
    pub fn push_rx_byte(&mut self, byte: u8) {
        if !self.enabled {
            return;
        }
        if byte >= 0xF8 {
            self.last_realtime = byte;
            self.isr |= irq_bit::REALTIME_DETECT;
        }
        if self.rx_fifo.len() < RX_FIFO_CAPACITY {
            self.rx_fifo.push_back(byte);
            self.isr |= irq_bit::RX_READY;
        } else {
            self.rx_overflow = true;
        }
    }

    /// Returns `true` if this board is asserting an enabled interrupt.
    #[inline]
    pub const fn has_pending_interrupt(&self) -> bool {
        self.enabled && (self.isr & self.ier) != 0
    }

    /// Computes the YM3802 interrupt vector byte for `R00` and CPU IACK.
    ///
    /// Bit 0 has highest priority (`+$00`) down to bit 7 (`+$0E`).
    /// If no enabled interrupt is pending, returns `(ivr_base & 0xE0) | 0x10`.
    #[inline]
    fn current_vector(&self) -> u8 {
        let active = self.isr & self.ier;
        if active == 0 {
            return (self.ivr_base & 0xE0) | 0x10;
        }
        let bit = active.trailing_zeros() as u8;
        (self.ivr_base & 0xE0) | (bit << 1)
    }

    /// Acknowledges the highest-priority enabled interrupt and returns its 68000 vector number.
    pub fn acknowledge_interrupt(&mut self) -> Option<u8> {
        if !self.has_pending_interrupt() {
            return None;
        }
        Some(self.current_vector())
    }

    /// Advances YM3802 timers, MIDI transmitter shift register, and host Rx polling
    /// by `cycles_10mhz` 10 MHz peripheral ticks.
    pub fn advance(&mut self, cycles_10mhz: u32) {
        if !self.enabled {
            return;
        }

        // 1. Poll host-side MIDI IN without blocking.
        while let Some(byte) = self.port.as_mut().and_then(|p| p.try_recv_byte()) {
            self.push_rx_byte(byte);
            if self.rx_fifo.len() >= RX_FIFO_CAPACITY {
                break;
            }
        }

        // 2. Advance MIDI transmitter shift register (31,250 bps = 3,200 cycles/byte @ 10 MHz).
        let mut remaining = cycles_10mhz;
        while remaining > 0 && self.tx_shift_remaining > 0 {
            let step = remaining.min(self.tx_shift_remaining);
            self.clock_10mhz += step as u64;
            self.tx_shift_remaining -= step;
            remaining -= step;

            if self.tx_shift_remaining == 0 {
                if let Some(next_byte) = self.tx_fifo.pop_front() {
                    if let Some(port) = self.port.as_mut() {
                        port.send_byte(next_byte, self.clock_10mhz);
                    }
                    self.tx_shift_remaining = TX_BYTE_CYCLES_10MHZ;
                    if self.tx_fifo.is_empty() {
                        self.isr |= irq_bit::TX_EMPTY;
                    }
                } else {
                    self.isr |= irq_bit::TX_EMPTY;
                }
            }
        }
        self.clock_10mhz += remaining as u64;

        // 3. Advance 125 kHz (8 us) timers: General Purpose Timer (R84/R85) and MIDI Clock Timer (R86/R87).
        self.timer_prescale_acc += cycles_10mhz;
        let ticks = self.timer_prescale_acc / TIMER_PRESCALE_10MHZ;
        self.timer_prescale_acc %= TIMER_PRESCALE_10MHZ;

        if ticks > 0 {
            if self.general_timer_counter > 0 {
                let mut t = ticks;
                while t > 0 {
                    if t >= self.general_timer_counter as u32 {
                        t -= self.general_timer_counter as u32;
                        self.isr |= irq_bit::GENERAL_TIMER;
                        if self.general_timer_reload > 0 {
                            self.general_timer_counter = self.general_timer_reload;
                        } else {
                            self.general_timer_counter = 0;
                            break;
                        }
                    } else {
                        self.general_timer_counter -= t as u16;
                        break;
                    }
                }
            }

            if self.midi_timer_counter > 0 {
                let mut t = ticks;
                while t > 0 {
                    if t >= self.midi_timer_counter as u32 {
                        t -= self.midi_timer_counter as u32;
                        self.isr |= irq_bit::MIDI_CLOCK_TIMER;
                        if self.midi_timer_reload > 0 {
                            self.midi_timer_counter = self.midi_timer_reload;
                        } else {
                            self.midi_timer_counter = 0;
                            break;
                        }
                    } else {
                        self.midi_timer_counter -= t as u16;
                        break;
                    }
                }
            }
        }
    }

    /// Reads an 8-bit register from this board (`addr` in `$EAFA00..=$EAFA0F` or `$EAFA10..=$EAFA1F`).
    pub fn read_u8(&mut self, addr: u32) -> Result<u8, BusFault> {
        if !self.enabled || (addr & 1) == 0 {
            return Err(BusFault::BusError {
                addr,
                is_write: false,
            });
        }
        let port = ((addr >> 1) & 0x07) as usize;
        let val = match port {
            // R00: Interrupt Vector Register (read)
            0 => self.current_vector(),
            // R01: Register Group Select (write-only)
            1 => 0xFF,
            // R02: Interrupt Service Register (read)
            2 => self.isr & self.ier,
            // R03: Interrupt Clear Register (write-only)
            3 => 0xFF,
            // Rx4..Rx7: Group-selected registers
            4..=7 => self.read_group_reg(port - 4),
            _ => unreachable!(),
        };
        Ok(val)
    }

    /// Writes an 8-bit register on this board (`addr` in `$EAFA00..=$EAFA0F` or `$EAFA10..=$EAFA1F`).
    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        if !self.enabled || (addr & 1) == 0 {
            return Err(BusFault::BusError {
                addr,
                is_write: true,
            });
        }
        let port = ((addr >> 1) & 0x07) as usize;
        match port {
            // R00: read-only
            0 => {}
            // R01: Register Group Select (+ bit 7 IC software reset)
            1 => {
                if (val & 0x80) != 0 {
                    self.reset();
                }
                self.reg_group = val & 0x0F;
            }
            // R02: read-only
            2 => {}
            // R03: Interrupt Clear Register
            3 => {
                self.isr &= !val;
                // Level-like FIFO-Rx status stays active if data remains unread.
                if !self.rx_fifo.is_empty() {
                    self.isr |= irq_bit::RX_READY;
                }
            }
            // Rx4..Rx7: Group-selected registers
            4..=7 => self.write_group_reg(port - 4, val),
            _ => unreachable!(),
        }
        Ok(())
    }

    fn read_group_reg(&mut self, offset: usize) -> u8 {
        match (self.reg_group, offset) {
            // Group 1: Real-time / counter readback
            (1, 0) => self.last_realtime,
            // Group 3: FIFO-Rx Status (R34) and Data (R36)
            (3, 0) => {
                let mut status = 0u8;
                if !self.rx_fifo.is_empty() {
                    status |= 0x80; // RDY (data available)
                }
                if self.rx_overflow {
                    status |= 0x40; // OVF
                }
                status
            }
            (3, 2) => {
                let byte = self.rx_fifo.pop_front().unwrap_or(0);
                if self.rx_fifo.is_empty() {
                    self.isr &= !irq_bit::RX_READY;
                }
                byte
            }
            // Group 5: FIFO-Tx Status (R54)
            (5, 0) => {
                let mut status = 0u8;
                if self.tx_fifo.is_empty() {
                    status |= 0x80; // EMP (FIFO empty)
                }
                if self.tx_fifo.len() < TX_FIFO_CAPACITY {
                    status |= 0x40; // RDY (space available)
                }
                if self.tx_shift_remaining > 0 {
                    status |= 0x01; // BUSY (shifter active)
                }
                status
            }
            (g, off) if (g as usize) < self.regs.len() => self.regs[g as usize][off],
            _ => 0xFF,
        }
    }

    fn write_group_reg(&mut self, offset: usize, val: u8) {
        if (self.reg_group as usize) < self.regs.len() {
            self.regs[self.reg_group as usize][offset] = val;
        }
        match (self.reg_group, offset) {
            // Group 0: Interrupt Control
            (0, 0) => {
                // R04: Interrupt Vector Offset (upper 3 bits)
                self.ivr_base = val & 0xE0;
            }
            (0, 2) => {
                // R06: Interrupt Enable Register
                self.ier = val;
                if (val & irq_bit::TX_EMPTY) != 0 && self.tx_fifo.is_empty() {
                    self.isr |= irq_bit::TX_EMPTY;
                }
                if (val & irq_bit::RX_READY) != 0 && !self.rx_fifo.is_empty() {
                    self.isr |= irq_bit::RX_READY;
                }
            }
            // Group 3: FIFO-Rx Control (R35)
            (3, 1) => {
                if (val & 0x80) != 0 {
                    self.rx_fifo.clear();
                    self.rx_overflow = false;
                    self.isr &= !irq_bit::RX_READY;
                }
            }
            // Group 5: FIFO-Tx Control (R55) & Data (R56)
            (5, 1) => {
                if (val & 0x80) != 0 {
                    self.tx_fifo.clear();
                    self.tx_shift_remaining = 0;
                    self.isr |= irq_bit::TX_EMPTY;
                }
            }
            (5, 2) => {
                self.enqueue_tx_byte(val);
            }
            // Group 8: Timers (R84/R85 General Purpose Timer, R86/R87 MIDI Clock Timer)
            (8, 0) => {
                // R84: General Timer Low
                self.general_timer_reload = (self.general_timer_reload & 0x3F00) | (val as u16);
            }
            (8, 1) => {
                // R85: General Timer High (bit 7 = load/start, bits 5..0 = upper 6 bits)
                self.general_timer_reload =
                    (self.general_timer_reload & 0x00FF) | (((val & 0x3F) as u16) << 8);
                if (val & 0x80) != 0 {
                    self.general_timer_counter = self.general_timer_reload;
                }
            }
            (8, 2) => {
                // R86: MIDI Clock Timer Low
                self.midi_timer_reload = (self.midi_timer_reload & 0x3F00) | (val as u16);
            }
            (8, 3) => {
                // R87: MIDI Clock Timer High (bit 7 = load/start, bits 5..0 = upper 6 bits)
                self.midi_timer_reload =
                    (self.midi_timer_reload & 0x00FF) | (((val & 0x3F) as u16) << 8);
                if (val & 0x80) != 0 {
                    self.midi_timer_counter = self.midi_timer_reload;
                }
            }
            _ => {}
        }
    }

    fn enqueue_tx_byte(&mut self, byte: u8) {
        if self.tx_shift_remaining == 0 && self.tx_fifo.is_empty() {
            // Shifter is idle: emit immediately for minimum latency and pace subsequent bytes
            // at the hardware 31,250 bps MIDI wire rate.
            if let Some(port) = self.port.as_mut() {
                port.send_byte(byte, self.clock_10mhz);
            }
            self.tx_shift_remaining = TX_BYTE_CYCLES_10MHZ;
            self.isr &= !irq_bit::TX_EMPTY;
        } else if self.tx_fifo.len() < TX_FIFO_CAPACITY {
            self.tx_fifo.push_back(byte);
            self.isr &= !irq_bit::TX_EMPTY;
        }
    }
}

/// Controller managing up to two SHARP CZ-6BM1 MIDI boards (`$EAFA00`–`$EAFA1F`).
#[derive(Debug)]
pub struct MidiController {
    boards: [MidiBoard; MIDI_BOARD_COUNT],
}

impl Default for MidiController {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiController {
    /// Creates a new controller with two disabled boards (Board 0 on IRQ4, Board 1 on IRQ2).
    pub fn new() -> Self {
        Self {
            boards: [
                MidiBoard::new(MidiIrqLevel::Irq4),
                MidiBoard::new(MidiIrqLevel::Irq2),
            ],
        }
    }

    /// Resets both boards (preserving installation state, IRQ jumper settings, and host ports).
    pub fn reset(&mut self) {
        for board in &mut self.boards {
            board.reset();
        }
    }

    /// Returns a reference to board `index` (`0` or `1`).
    #[inline]
    pub fn board(&self, index: usize) -> Option<&MidiBoard> {
        self.boards.get(index)
    }

    /// Returns a mutable reference to board `index` (`0` or `1`).
    #[inline]
    pub fn board_mut(&mut self, index: usize) -> Option<&mut MidiBoard> {
        self.boards.get_mut(index)
    }

    /// Returns `true` if `addr` falls within an enabled CZ-6BM1 board's I/O range.
    #[inline]
    pub fn is_mapped_addr(&self, addr: u32) -> bool {
        match addr {
            MIDI_BOARD0_BASE..MIDI_BOARD1_BASE => self.boards[0].is_enabled(),
            MIDI_BOARD1_BASE..MIDI_WINDOW_END => self.boards[1].is_enabled(),
            _ => false,
        }
    }

    /// Advances all enabled MIDI boards by `cycles_10mhz` 10 MHz ticks.
    #[inline]
    pub fn advance(&mut self, cycles_10mhz: u32) {
        for board in &mut self.boards {
            if board.is_enabled() {
                board.advance(cycles_10mhz);
            }
        }
    }

    /// Returns `true` if any enabled board configured on `level` has a pending interrupt.
    #[inline]
    pub fn has_pending_interrupt(&self, level: MidiIrqLevel) -> bool {
        self.boards
            .iter()
            .any(|b| b.irq_level() == level && b.has_pending_interrupt())
    }

    /// Acknowledges the first pending interrupt on `level` and returns its 68000 vector number.
    pub fn acknowledge_interrupt(&mut self, level: MidiIrqLevel) -> Option<u8> {
        for board in &mut self.boards {
            if board.irq_level() == level && board.has_pending_interrupt() {
                return board.acknowledge_interrupt();
            }
        }
        None
    }

    /// Reads an 8-bit register from the CZ-6BM1 window (`$EAFA00`–`$EAFA1F`).
    pub fn read_u8(&mut self, addr: u32) -> Result<u8, BusFault> {
        match addr {
            MIDI_BOARD0_BASE..MIDI_BOARD1_BASE => self.boards[0].read_u8(addr),
            MIDI_BOARD1_BASE..MIDI_WINDOW_END => self.boards[1].read_u8(addr),
            _ => Err(BusFault::BusError {
                addr,
                is_write: false,
            }),
        }
    }

    /// Writes an 8-bit register to the CZ-6BM1 window (`$EAFA00`–`$EAFA1F`).
    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        match addr {
            MIDI_BOARD0_BASE..MIDI_BOARD1_BASE => self.boards[0].write_u8(addr, val),
            MIDI_BOARD1_BASE..MIDI_WINDOW_END => self.boards[1].write_u8(addr, val),
            _ => Err(BusFault::BusError {
                addr,
                is_write: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_board_raises_bus_error_and_even_addresses_fault() {
        let mut ctrl = MidiController::new();
        assert!(ctrl.read_u8(0x00EA_FA01).is_err());
        assert!(ctrl.write_u8(0x00EA_FA03, 0x00).is_err());

        ctrl.board_mut(0).unwrap().set_enabled(true);
        assert!(ctrl.read_u8(0x00EA_FA00).is_err());
        assert!(ctrl.write_u8(0x00EA_FA02, 0x00).is_err());
        assert!(ctrl.read_u8(0x00EA_FA01).is_ok());
        // Board 1 is still disabled
        assert!(ctrl.read_u8(0x00EA_FA11).is_err());
    }

    #[test]
    fn tx_emits_first_byte_immediately_and_paces_subsequent_bytes() {
        let (port, endpoint) = midi_channel();
        let mut ctrl = MidiController::new();
        {
            let b0 = ctrl.board_mut(0).unwrap();
            b0.set_enabled(true);
            b0.set_port(Some(Box::new(port)));
        }

        // Select group 5 (FIFO-Tx)
        ctrl.write_u8(0x00EA_FA03, 0x05).unwrap();
        // Status: EMP | RDY = 0xC0
        assert_eq!(ctrl.read_u8(0x00EA_FA09).unwrap(), 0xC0);

        // Write a 3-byte Note On message: 0x90, 0x3C, 0x7F
        ctrl.write_u8(0x00EA_FA0D, 0x90).unwrap();
        ctrl.write_u8(0x00EA_FA0D, 0x3C).unwrap();
        ctrl.write_u8(0x00EA_FA0D, 0x7F).unwrap();

        // First byte emitted immediately at tick 0 (no VSYNC wait)
        assert_eq!(
            endpoint.try_recv().unwrap(),
            MidiByteEvent {
                tick_10mhz: 0,
                byte: 0x90
            }
        );
        assert!(endpoint.try_recv().is_err());

        // Advance 3,200 cycles (320 us @ 10 MHz) -> second byte
        ctrl.advance(3_200);
        assert_eq!(
            endpoint.try_recv().unwrap(),
            MidiByteEvent {
                tick_10mhz: 3_200,
                byte: 0x3C
            }
        );

        // Advance another 3,200 cycles -> third byte
        ctrl.advance(3_200);
        assert_eq!(
            endpoint.try_recv().unwrap(),
            MidiByteEvent {
                tick_10mhz: 6_400,
                byte: 0x7F
            }
        );
    }

    #[test]
    fn rx_from_channel_populates_fifo_and_raises_irq4_with_vector() {
        let (port, endpoint) = midi_channel();
        let mut ctrl = MidiController::new();
        {
            let b0 = ctrl.board_mut(0).unwrap();
            b0.set_enabled(true);
            b0.set_port(Some(Box::new(port)));
        }

        // Group 0: IVR = $80, IER = RX_READY (bit 5)
        ctrl.write_u8(0x00EA_FA03, 0x00).unwrap();
        ctrl.write_u8(0x00EA_FA09, 0x80).unwrap();
        ctrl.write_u8(0x00EA_FA0D, irq_bit::RX_READY).unwrap();
        assert!(!ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));

        // Host pushes MIDI bytes from external thread/callback
        endpoint.send_bytes(&[0x90, 0x40, 0x64]).unwrap();
        ctrl.advance(10);

        assert!(ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));
        // Vector = 0x80 | (5 << 1) = 0x8A
        assert_eq!(ctrl.acknowledge_interrupt(MidiIrqLevel::Irq4), Some(0x8A));
        assert_eq!(ctrl.read_u8(0x00EA_FA05).unwrap(), irq_bit::RX_READY);

        // Select group 3 (FIFO-Rx) and read all three bytes
        ctrl.write_u8(0x00EA_FA03, 0x03).unwrap();
        assert_eq!(ctrl.read_u8(0x00EA_FA09).unwrap() & 0x80, 0x80);
        assert_eq!(ctrl.read_u8(0x00EA_FA0D).unwrap(), 0x90);
        assert_eq!(ctrl.read_u8(0x00EA_FA0D).unwrap(), 0x40);
        assert_eq!(ctrl.read_u8(0x00EA_FA0D).unwrap(), 0x64);

        // FIFO empty -> RX_READY clears
        assert_eq!(ctrl.read_u8(0x00EA_FA09).unwrap() & 0x80, 0x00);
        assert!(!ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));
        assert_eq!(ctrl.read_u8(0x00EA_FA01).unwrap(), 0x90); // 0x80 | 0x10 (no pending IRQ)
    }

    #[test]
    fn general_purpose_timer_fires_periodic_irq() {
        let mut ctrl = MidiController::new();
        ctrl.board_mut(0).unwrap().set_enabled(true);

        // Group 0: IVR = $E0, enable General Purpose Timer (bit 7)
        ctrl.write_u8(0x00EA_FA03, 0x00).unwrap();
        ctrl.write_u8(0x00EA_FA09, 0xE0).unwrap();
        ctrl.write_u8(0x00EA_FA0D, irq_bit::GENERAL_TIMER).unwrap();

        // Group 8: General Timer reload = 10 ticks (10 * 8 us = 80 us = 800 cycles @ 10 MHz)
        ctrl.write_u8(0x00EA_FA03, 0x08).unwrap();
        ctrl.write_u8(0x00EA_FA09, 10).unwrap();
        ctrl.write_u8(0x00EA_FA0B, 0x80).unwrap(); // bit 7 = load & start

        ctrl.advance(799);
        assert!(!ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));

        ctrl.advance(1);
        assert!(ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));
        // Vector = 0xE0 | (7 << 1) = 0xEE
        assert_eq!(ctrl.acknowledge_interrupt(MidiIrqLevel::Irq4), Some(0xEE));

        // Clear via ICR (R03)
        ctrl.write_u8(0x00EA_FA07, irq_bit::GENERAL_TIMER).unwrap();
        assert!(!ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));

        // Fires again automatically after another 800 cycles
        ctrl.advance(800);
        assert!(ctrl.has_pending_interrupt(MidiIrqLevel::Irq4));
    }

    #[test]
    fn message_parser_handles_running_status_sysex_and_realtime() {
        let mut parser = MidiMessageParser::new();

        // Note On with Running Status
        assert_eq!(parser.push(0x90), None);
        assert_eq!(parser.push(0x3C), None);
        assert_eq!(parser.push(0x7F), Some(vec![0x90, 0x3C, 0x7F]));
        // Running status: next two data bytes emit another Note On
        assert_eq!(parser.push(0x40), None);
        // Interleaved Real-Time clock ($F8) does not disturb in-progress message
        assert_eq!(parser.push(0xF8), Some(vec![0xF8]));
        assert_eq!(parser.push(0x64), Some(vec![0x90, 0x40, 0x64]));

        // SysEx with interleaved Active Sensing ($FE)
        assert_eq!(parser.push(0xF0), None);
        assert_eq!(parser.push(0x41), None);
        assert_eq!(parser.push(0xFE), Some(vec![0xFE]));
        assert_eq!(parser.push(0x10), None);
        assert_eq!(parser.push(0xF7), Some(vec![0xF0, 0x41, 0x10, 0xF7]));
    }
}
