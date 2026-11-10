//! Motorola MC68901 Multi-Function Peripheral (`$E88000`–`$E8802F`, Interrupt Level 6).
//!
//! Handles:
//! - GPIP input pins (`H-SYNC`, `CRTC RINT`, `V-DISP`, `OPMIRQ`, `POWER`, `EXPWON`, `ALARM`)
//! - 16 prioritized interrupt channels (`IERA/B`, `IPRA/B`, `ISRA/B`, `IMRA/B`, `VR`)
//! - Timers A, B, C, D (4 MHz base clock with prescalers `4, 10, 16, 50, 64, 100, 200`, plus Timer A Event Count mode)
//! - Keyboard USART (`UCR`, `RSR`, `TSR`, `UDR`)

use std::collections::VecDeque;

use msx68k_cpu::BusFault;

use crate::crtc::CrtcSyncEvents;

/// Prescaler divisor table for MC68901 timers (clocked at 4 MHz on X68000).
/// In 10 MHz CPU cycles, `1` timer tick at prescaler `P` equals `P * 10 / 4 = P * 5 / 2` CPU cycles.
const PRESCALER_DIVISORS: [u32; 8] = [0, 4, 10, 16, 50, 64, 100, 200];

/// Internal state of a single 8-bit MC68901 timer.
#[derive(Debug, Default, Clone)]
struct MfpTimer {
    control: u8,
    reload: u8,
    counter: u8,
    /// Sub-tick accumulator in units of `2 * 10MHz_cycles` (so `prescaler * 5` is one decrement).
    sub_cycles_x2: u32,
}

impl MfpTimer {
    fn write_data(&mut self, val: u8) {
        self.reload = val;
        if (self.control & 0x0F) == 0 {
            self.counter = val;
        }
    }

    fn decrement_one(&mut self) -> bool {
        self.counter = self.counter.wrapping_sub(1);
        if self.counter == 0 {
            self.counter = self.reload;
            true
        } else {
            false
        }
    }

    /// Advances delay-mode timer by `cycles_10mhz` CPU cycles, returning the number of underflows.
    fn advance_delay(&mut self, cycles_10mhz: u32) -> u32 {
        let mode = (self.control & 0x07) as usize;
        let prescaler = PRESCALER_DIVISORS[mode];
        if prescaler == 0 {
            return 0;
        }
        // Threshold in 2x-10MHz-cycles: prescaler * (10 / 4) * 2 = prescaler * 5
        let step_x2 = prescaler * 5;
        self.sub_cycles_x2 = self.sub_cycles_x2.saturating_add(cycles_10mhz * 2);
        let mut underflows = 0;
        while self.sub_cycles_x2 >= step_x2 {
            self.sub_cycles_x2 -= step_x2;
            if self.decrement_one() {
                underflows += 1;
            }
        }
        underflows
    }
}

/// Motorola MC68901 Multi-Function Peripheral (`$E88000`–`$E8802F`).
#[derive(Debug, Clone)]
pub struct Mfp {
    /// General Purpose I/O input pin levels (`GPIP`, `$E88001`).
    gpip: u8,
    /// Number of V-DISP falling edges seen on `GPIP` bit 4.
    pub(crate) vdisp_falls: u64,
    /// Active Edge Register (`AER`, `$E88003`).
    aer: u8,
    /// Data Direction Register (`DDR`, `$E88005`).
    ddr: u8,
    /// 16-bit Interrupt Enable Register (`IERA << 8 | IERB`).
    ier: u16,
    /// 16-bit Interrupt Pending Register (`IPRA << 8 | IPRB`).
    ipr: u16,
    /// 16-bit Interrupt In-Service Register (`ISRA << 8 | ISRB`).
    isr: u16,
    /// 16-bit Interrupt Mask Register (`IMRA << 8 | IMRB`).
    imr: u16,
    /// Vector Register (`VR`, `$E88017`).
    vr: u8,
    /// Timers A (`0`), B (`1`), C (`2`), D (`3`).
    timers: [MfpTimer; 4],
    /// Sync Character Register (`SCR`, `$E88027`).
    scr: u8,
    /// USART Control Register (`UCR`, `$E88029`).
    ucr: u8,
    /// Receiver Status Register (`RSR`, `$E8802B`).
    rsr: u8,
    /// Transmitter Status Register (`TSR`, `$E8802D`).
    tsr: u8,
    /// USART Data Register (`UDR`, `$E8802F`).
    udr: u8,
    /// Queued keyboard scan bytes to deliver via USART receive (`UDR`).
    rx_queue: VecDeque<u8>,
}

impl Default for Mfp {
    fn default() -> Self {
        Self::new()
    }
}

impl Mfp {
    /// Interrupt channel indices (`0..=15`) inside `ier`/`ipr`/`isr`/`imr`:
    pub const CH_GPIP0_ALARM: u8 = 0;
    pub const CH_GPIP1_EXPWON: u8 = 1;
    pub const CH_GPIP2_POWER: u8 = 2;
    pub const CH_GPIP3_OPMIRQ: u8 = 3;
    pub const CH_TIMER_D: u8 = 4;
    pub const CH_TIMER_C: u8 = 5;
    pub const CH_GPIP4_VDISP: u8 = 6;
    pub const CH_GPIP5_UNUSED: u8 = 7;
    pub const CH_TIMER_B: u8 = 8;
    pub const CH_TRERR: u8 = 9;
    pub const CH_TRDY: u8 = 10;
    pub const CH_RERR: u8 = 11;
    pub const CH_RRDY: u8 = 12;
    pub const CH_TIMER_A: u8 = 13;
    pub const CH_GPIP6_CRTC: u8 = 14;
    pub const CH_GPIP7_HSYNC: u8 = 15;

    /// Creates a new MC68901 MFP in power-on reset state.
    pub fn new() -> Self {
        Self {
            // Bit 7 (HSYNC=1), Bit 6 (RINT=1 inactive), Bit 5 (1), Bit 4 (VDISP=0),
            // Bit 3 (OPMIRQ=1 inactive), Bit 2 (POWER=0), Bit 1 (EXPWON=1), Bit 0 (ALARM=0)
            gpip: 0b1110_1010,
            vdisp_falls: 0,
            aer: 0x00,
            ddr: 0x00,
            ier: 0x0000,
            ipr: 0x0000,
            isr: 0x0000,
            imr: 0x0000,
            vr: 0x48, // Default vector base 0x40, SEI=1
            timers: Default::default(),
            scr: 0x00,
            ucr: 0x00,
            rsr: 0x00,
            tsr: 0x80, // Bit 7 = Buffer Empty (BE = 1)
            udr: 0x00,
            rx_queue: VecDeque::new(),
        }
    }

    /// Resets the MFP to power-on state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Enqueues a keyboard byte into the USART receiver buffer.
    pub fn push_keyboard_byte(&mut self, byte: u8) {
        self.rx_queue.push_back(byte);
        self.deliver_next_rx_byte();
    }

    fn deliver_next_rx_byte(&mut self) {
        if (self.rsr & 0x80) == 0 {
            if let Some(b) = self.rx_queue.pop_front() {
                self.udr = b;
                self.rsr |= 0x80; // Buffer Full
                self.request_interrupt(Self::CH_RRDY);
            }
        }
    }

    /// Triggers an interrupt on `channel` (`0..=15`) if enabled in `IER`.
    #[inline]
    pub fn request_interrupt(&mut self, channel: u8) {
        let mask = 1u16 << (channel & 15);
        if (self.ier & mask) != 0 {
            self.ipr |= mask;
        }
    }

    /// Returns `true` if the MFP is currently asserting a Level 6 interrupt to the CPU.
    #[inline]
    pub fn has_pending_interrupt(&self) -> bool {
        self.highest_pending_channel().is_some()
    }

    /// Finds the highest-priority (`15` down to `0`) pending and unmasked interrupt channel,
    /// respecting the In-Service Register (`ISR`) when `VR` bit 3 (`SEI`) is enabled.
    pub fn highest_pending_channel(&self) -> Option<u8> {
        let active = self.ipr & self.ier & self.imr;
        if active == 0 {
            return None;
        }
        let sei = (self.vr & 0x08) != 0;
        for ch in (0..16u8).rev() {
            let bit = 1u16 << ch;
            if sei && (self.isr & bit) != 0 {
                // Higher or equal priority interrupt is currently in service
                return None;
            }
            if (active & bit) != 0 {
                return Some(ch);
            }
        }
        None
    }

    /// Acknowledges the highest-priority pending Level 6 interrupt and returns its vector number.
    pub fn acknowledge_interrupt(&mut self) -> Option<u8> {
        let ch = self.highest_pending_channel()?;
        let bit = 1u16 << ch;
        self.ipr &= !bit;
        if (self.vr & 0x08) != 0 {
            self.isr |= bit;
        }
        Some((self.vr & 0xF0) | ch)
    }

    /// Updates GPIP pins from CRTC sync events and triggers edge-sensitive GPIP interrupts
    /// as well as Timer A in Event Count mode (`TACR == 0x08`).
    pub fn update_crtc_sync(&mut self, sync: CrtcSyncEvents) {
        // Bit 7: H-SYNC (1 = H-Sync, 0 = H-Disp)
        self.set_gpip_bit(7, sync.h_sync, Self::CH_GPIP7_HSYNC);
        // Bit 6: CRTC RINT (active-low: 0 when matching R09, 1 otherwise)
        self.set_gpip_bit(6, !sync.rint_active, Self::CH_GPIP6_CRTC);
        // Bit 4: V-DISP (1 = active display, 0 = V-Blank)
        let prev_vdisp = (self.gpip & 0x10) != 0;
        self.set_gpip_bit(4, sync.v_disp, Self::CH_GPIP4_VDISP);
        if sync.v_disp_fell || (prev_vdisp && !sync.v_disp) {
            self.vdisp_falls = self.vdisp_falls.wrapping_add(1);
        }

        // Timer A Event Count mode (TACR bits 3..0 == 0b1000) counts V-DISP edges matching AER bit 4
        if (self.timers[0].control & 0x0F) == 0x08 && prev_vdisp != sync.v_disp {
            let aer_rising = (self.aer & 0x10) != 0;
            if ((aer_rising && sync.v_disp) || (!aer_rising && !sync.v_disp))
                && self.timers[0].decrement_one()
            {
                self.request_interrupt(Self::CH_TIMER_A);
            }
        }
    }

    /// Returns `true` while `GPIP` bit 4 reports the active display period.
    pub(crate) fn vdisp_active(&self) -> bool {
        self.gpip & 0x10 != 0
    }

    /// Updates the OPM (`YM2151`) interrupt pin on `GPIP` bit 3 (active-low).
    pub fn set_opm_irq(&mut self, active: bool) {
        self.set_gpip_bit(3, !active, Self::CH_GPIP3_OPMIRQ);
    }

    fn set_gpip_bit(&mut self, bit_idx: u8, high: bool, channel: u8) {
        let mask = 1u8 << bit_idx;
        let was_high = (self.gpip & mask) != 0;
        if was_high == high {
            return;
        }
        if high {
            self.gpip |= mask;
        } else {
            self.gpip &= !mask;
        }
        let aer_rising = (self.aer & mask) != 0;
        if (aer_rising && high) || (!aer_rising && !high) {
            self.request_interrupt(channel);
        }
    }

    /// Advances MFP Timers A, B, C, D by `cycles_10mhz` CPU cycles.
    pub fn advance(&mut self, cycles_10mhz: u32) {
        if (self.timers[0].control & 0x08) == 0
            && self.timers[0].advance_delay(cycles_10mhz) > 0
        {
            self.request_interrupt(Self::CH_TIMER_A);
        }
        if (self.timers[1].control & 0x08) == 0
            && self.timers[1].advance_delay(cycles_10mhz) > 0
        {
            self.request_interrupt(Self::CH_TIMER_B);
        }
        if self.timers[2].advance_delay(cycles_10mhz) > 0 {
            self.request_interrupt(Self::CH_TIMER_C);
        }
        if self.timers[3].advance_delay(cycles_10mhz) > 0 {
            self.request_interrupt(Self::CH_TIMER_D);
        }
        self.deliver_next_rx_byte();
    }

    /// Reads an 8-bit byte from the MFP address space (`$E88000`–`$E89FFF`).
    pub fn read_u8(&mut self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            // Even byte access to MFP raises a hardware Bus Error on X68000
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        let reg = ((a - 0x00E8_8001) >> 1) & 0x1F;
        let val = match reg {
            0x00 => self.gpip,
            0x01 => self.aer,
            0x02 => self.ddr,
            0x03 => (self.ier >> 8) as u8,
            0x04 => self.ier as u8,
            0x05 => (self.ipr >> 8) as u8,
            0x06 => self.ipr as u8,
            0x07 => (self.isr >> 8) as u8,
            0x08 => self.isr as u8,
            0x09 => (self.imr >> 8) as u8,
            0x0A => self.imr as u8,
            0x0B => self.vr,
            0x0C => self.timers[0].control,
            0x0D => self.timers[1].control,
            0x0E => ((self.timers[2].control & 0x07) << 4) | (self.timers[3].control & 0x07),
            0x0F => self.timers[0].counter,
            0x10 => self.timers[1].counter,
            0x11 => self.timers[2].counter,
            0x12 => self.timers[3].counter,
            0x13 => self.scr,
            0x14 => self.ucr,
            0x15 => self.rsr,
            0x16 => self.tsr | 0x80,
            0x17 => {
                let d = self.udr;
                self.rsr &= !0x80;
                self.deliver_next_rx_byte();
                d
            }
            _ => 0xFF,
        };
        Ok(val)
    }

    /// Writes an 8-bit byte to the MFP address space (`$E88000`–`$E89FFF`).
    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        let reg = ((a - 0x00E8_8001) >> 1) & 0x1F;
        match reg {
            0x00 => {}
            0x01 => self.aer = val,
            0x02 => self.ddr = val,
            0x03 => {
                self.ier = ((val as u16) << 8) | (self.ier & 0x00FF);
                self.ipr &= self.ier;
            }
            0x04 => {
                self.ier = (self.ier & 0xFF00) | (val as u16);
                self.ipr &= self.ier;
            }
            0x05 => {
                self.ipr &= ((val as u16) << 8) | 0x00FF;
            }
            0x06 => {
                self.ipr &= 0xFF00 | (val as u16);
            }
            0x07 => {
                self.isr &= ((val as u16) << 8) | 0x00FF;
            }
            0x08 => {
                self.isr &= 0xFF00 | (val as u16);
            }
            0x09 => {
                self.imr = ((val as u16) << 8) | (self.imr & 0x00FF);
            }
            0x0A => {
                self.imr = (self.imr & 0xFF00) | (val as u16);
            }
            0x0B => {
                self.vr = val & 0xF8;
                if (self.vr & 0x08) == 0 {
                    self.isr = 0;
                }
            }
            0x0C => {
                self.timers[0].control = val & 0x0F;
            }
            0x0D => {
                self.timers[1].control = val & 0x0F;
            }
            0x0E => {
                self.timers[2].control = (val >> 4) & 0x07;
                self.timers[3].control = val & 0x07;
            }
            0x0F => self.timers[0].write_data(val),
            0x10 => self.timers[1].write_data(val),
            0x11 => self.timers[2].write_data(val),
            0x12 => self.timers[3].write_data(val),
            0x13 => self.scr = val,
            0x14 => self.ucr = val,
            0x15 => self.rsr = (self.rsr & 0x80) | (val & 0x7F),
            0x16 => {
                self.tsr = (val & 0x7F) | 0x80;
                if (val & 0x01) != 0 {
                    self.request_interrupt(Self::CH_TRDY);
                }
            }
            0x17 => {
                // Keyboard command/LED write via UDR; transmitter remains ready
                self.tsr |= 0x80;
                if (self.tsr & 0x01) != 0 {
                    self.request_interrupt(Self::CH_TRDY);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
