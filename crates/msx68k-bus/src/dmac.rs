//! Hitachi HD63450 4-Channel DMA Controller (`$E84000`–`$E85FFF`, Interrupt Level 3).
//!
//! Channel assignments on X68000:
//! - Channel 0 (`$E84000`–`$E8403F`): Floppy Disk Controller (`uPD72065`, `$E94003`)
//! - Channel 1 (`$E84040`–`$E8407F`): SASI Hard Disk Controller (`$E96001`)
//! - Channel 2 (`$E84080`–`$E840BF`): User / Expansion (`IOCS _DMAMOVE`)
//! - Channel 3 (`$E840C0`–`$E840FF`): ADPCM (`MSM6258V`, `$E92003`) + General Control Register (`$E840FF`)

use msx68k_cpu::BusFault;

// CSR (Channel Status Register) bit masks
pub const CSR_COC: u8 = 0x80; // Channel Operation Complete
pub const CSR_BTC: u8 = 0x40; // Block Transfer Complete
pub const CSR_NDT: u8 = 0x20; // Normal Device Termination
pub const CSR_ERR: u8 = 0x10; // Error Bit Set
pub const CSR_ACT: u8 = 0x08; // Channel Active
pub const CSR_DIT: u8 = 0x04; // Done Input Transition
pub const CSR_PCT: u8 = 0x02; // PCL Transition
pub const CSR_PCS: u8 = 0x01; // PCL State

// CCR (Channel Control Register) bit masks
pub const CCR_STR: u8 = 0x80; // Start Operation
pub const CCR_CNT: u8 = 0x40; // Continue Operation
pub const CCR_HLT: u8 = 0x20; // Halt Operation
pub const CCR_SAB: u8 = 0x10; // Software Abort
pub const CCR_ITE: u8 = 0x08; // Interrupt Enable

// CER (Channel Error Register) codes
pub const CER_NONE: u8 = 0x00;
pub const CER_CONFIG: u8 = 0x01;
pub const CER_TIMING: u8 = 0x02;
pub const CER_ADDR_MEM: u8 = 0x05;
pub const CER_ADDR_DEV: u8 = 0x06;
pub const CER_ADDR_BASE: u8 = 0x07;
pub const CER_BUS_MEM: u8 = 0x09;
pub const CER_BUS_DEV: u8 = 0x0a;
pub const CER_BUS_BASE: u8 = 0x0b;
pub const CER_BASE_COUNT: u8 = 0x0f;
pub const CER_COUNT: u8 = 0x0D;
pub const CER_SW_ABORT: u8 = 0x11;

/// State of a single HD63450 DMA channel.
#[derive(Debug, Clone)]
pub struct DmaChannel {
    pub csr: u8,
    pub cer: u8,
    pub dcr: u8,
    pub ocr: u8,
    pub scr: u8,
    pub ccr: u8,
    pub mtc: u16,
    pub mar: u32,
    pub dar: u32,
    pub btc: u16,
    pub bar: u32,
    pub niv: u8,
    pub eiv: u8,
    pub mfc: u8,
    pub cpr: u8,
    pub dfc: u8,
    pub bfc: u8,
    pub irq_pending: bool,
}

impl Default for DmaChannel {
    fn default() -> Self {
        Self::new()
    }
}

impl DmaChannel {
    pub const fn new() -> Self {
        Self {
            csr: CSR_PCS,
            cer: CER_NONE,
            dcr: 0,
            ocr: 0,
            scr: 0,
            ccr: 0,
            mtc: 0,
            mar: 0,
            dar: 0,
            btc: 0,
            bar: 0,
            niv: 0x0F,
            eiv: 0x0F,
            mfc: 0,
            cpr: 0,
            dfc: 0,
            bfc: 0,
            irq_pending: false,
        }
    }

    #[inline]
    pub const fn is_active(&self) -> bool {
        (self.csr & CSR_ACT) != 0 && (self.ccr & CCR_HLT) == 0
    }

    /// Direction bit (`OCR` bit 7): `true` = Device to Memory (`DAR -> MAR`), `false` = Memory to Device (`MAR -> DAR`).
    #[inline]
    pub const fn is_device_to_memory(&self) -> bool {
        (self.ocr & 0x80) != 0
    }

    pub(crate) fn step_mar(&mut self, step: u32) {
        match (self.scr >> 2) & 0x03 {
            1 => self.mar = self.mar.wrapping_add(step),
            2 => self.mar = self.mar.wrapping_sub(step),
            _ => {}
        }
    }

    pub(crate) fn step_dar(&mut self, step: u32) {
        match self.scr & 0x03 {
            1 => self.dar = self.dar.wrapping_add(step),
            2 => self.dar = self.dar.wrapping_sub(step),
            _ => {}
        }
    }

    pub(crate) fn finish_normal(&mut self) {
        self.csr = (self.csr & !CSR_ACT) | CSR_COC;
        self.ccr &= !(CCR_STR | CCR_CNT | CCR_HLT);
        if (self.ccr & CCR_ITE) != 0 {
            self.irq_pending = true;
        }
    }

    pub(crate) fn finish_error(&mut self, err_code: u8) {
        self.cer = err_code;
        self.csr = (self.csr & !CSR_ACT) | CSR_COC | CSR_ERR;
        self.ccr &= !(CCR_STR | CCR_CNT | CCR_HLT);
        if (self.ccr & CCR_ITE) != 0 {
            self.irq_pending = true;
        }
    }
}

/// Hitachi HD63450 4-Channel DMA Controller (`$E84000`–`$E85FFF`).
#[derive(Debug, Clone)]
pub struct Dmac {
    pub channels: [DmaChannel; 4],
    pub gcr: u8,
    diagnostics: Option<bool>,
    reported_unsupported: [u8; 4],
    /// Cached earliest event: the only new test on the idle instruction path.
    pub(crate) next_event: u64,
    pub(crate) deadlines: [u64; 4],
    /// 400 MHz time units: 40 units per 10 MHz peripheral tick.
    pub(crate) clock: u64,
    pub(crate) requests: u8,
    pub(crate) starting: [bool; 4],
    pub(crate) memory_carry: [Option<u8>; 4],
    pub(crate) device_carry: [Option<u8>; 4],
    pub(crate) burst_start: [u64; 4],
    pub(crate) burst_end: [u64; 4],
    pub(crate) tick_fraction: u32,
}

impl Default for Dmac {
    fn default() -> Self {
        Self::new()
    }
}

impl Dmac {
    pub const fn new() -> Self {
        Self {
            channels: [
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
                DmaChannel::new(),
            ],
            gcr: 0,
            diagnostics: None,
            reported_unsupported: [0; 4],
            next_event: u64::MAX,
            deadlines: [u64::MAX; 4],
            clock: 0,
            requests: 0,
            starting: [false; 4],
            memory_carry: [None; 4],
            device_carry: [None; 4],
            burst_start: [0; 4],
            burst_end: [0; 4],
            tick_fraction: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Report known emulation gaps, not reserved registers or ordinary bus faults.
    /// Off by default; at most one message per reason/channel between resets.
    #[cold]
    fn report_unsupported(&mut self, channel: usize, reason: u8, description: &str) {
        let enabled = *self.diagnostics.get_or_insert_with(|| {
            std::env::var_os("MSX68K_LOG_UNIMPLEMENTED").is_some_and(|v| v == "1")
        });
        if !enabled || self.reported_unsupported[channel] & reason != 0 {
            return;
        }
        self.reported_unsupported[channel] |= reason;
        let ch = &self.channels[channel];
        eprintln!(
            "[msx68k][unimplemented][DMAC ch{channel}] {description}; MAR=${:06X} DAR=${:06X} MTC={} DCR=${:02X} OCR=${:02X} SCR=${:02X} CCR=${:02X}",
            ch.mar, ch.dar, ch.mtc, ch.dcr, ch.ocr, ch.scr, ch.ccr,
        );
    }

    /// Returns `true` if any DMA channel has a pending Level 3 interrupt.
    pub fn has_pending_interrupt(&self) -> bool {
        self.channels.iter().any(|ch| ch.irq_pending)
    }

    /// Acknowledges the highest-priority pending Level 3 DMA interrupt and returns its vector (`NIV` or `EIV`).
    pub fn acknowledge_interrupt(&mut self) -> Option<u8> {
        // Find channel with highest priority (lowest CPR value 0..3, then lowest channel index 0..3)
        let mut best_ch: Option<usize> = None;
        for (idx, ch) in self.channels.iter().enumerate() {
            if ch.irq_pending {
                if let Some(prev) = best_ch {
                    if (ch.cpr & 3) < (self.channels[prev].cpr & 3) {
                        best_ch = Some(idx);
                    }
                } else {
                    best_ch = Some(idx);
                }
            }
        }
        let idx = best_ch?;
        let ch = &mut self.channels[idx];
        ch.irq_pending = false;
        if (ch.csr & CSR_ERR) != 0 {
            Some(ch.eiv)
        } else {
            Some(ch.niv)
        }
    }

    #[inline]
    pub fn has_scheduled_work(&self) -> bool {
        self.next_event != u64::MAX
    }

    pub(crate) fn schedule(&mut self, channel: usize, deadline: u64) {
        self.deadlines[channel] = deadline;
        self.next_event = *self.deadlines.iter().min().unwrap();
    }

    /// Device request input, independent of CPU privilege and the memory endpoint.
    pub fn set_request(&mut self, channel: usize, asserted: bool) {
        self.set_request_with_delay(channel, asserted, 40);
    }

    pub(crate) fn set_request_with_delay(&mut self, channel: usize, asserted: bool, cycle: u64) {
        let channel = channel & 3;
        let was_asserted = self.requests & (1 << channel) != 0;
        if asserted {
            self.requests |= 1 << channel;
        } else {
            self.requests &= !(1 << channel);
        }
        let ch = &self.channels[channel];
        if ch.ocr & 2 != 0 && !self.starting[channel] {
            let when = if asserted && ch.is_active() {
                self.clock + cycle
            } else {
                u64::MAX
            };
            if !asserted || !was_asserted || self.deadlines[channel] == u64::MAX {
                self.schedule(channel, when);
            }
        }
    }

    /// Reads an 8-bit byte from the DMAC address window (`$E84000`–`$E85FFF`).
    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        let offset = (a - 0x00E8_4000) as usize;
        if offset >= 0x0100 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        if offset == 0xFF {
            return Ok(self.gcr);
        }
        let ch = &self.channels[(offset >> 6) & 3];
        let reg = offset & 0x3F;
        let val = match reg {
            0x00 => ch.csr,
            0x01 => ch.cer,
            0x04 => ch.dcr,
            0x05 => ch.ocr,
            0x06 => ch.scr,
            0x07 => ch.ccr,
            0x0A => (ch.mtc >> 8) as u8,
            0x0B => ch.mtc as u8,
            0x0C => (ch.mar >> 24) as u8,
            0x0D => (ch.mar >> 16) as u8,
            0x0E => (ch.mar >> 8) as u8,
            0x0F => ch.mar as u8,
            0x14 => (ch.dar >> 24) as u8,
            0x15 => (ch.dar >> 16) as u8,
            0x16 => (ch.dar >> 8) as u8,
            0x17 => ch.dar as u8,
            0x1A => (ch.btc >> 8) as u8,
            0x1B => ch.btc as u8,
            0x1C => (ch.bar >> 24) as u8,
            0x1D => (ch.bar >> 16) as u8,
            0x1E => (ch.bar >> 8) as u8,
            0x1F => ch.bar as u8,
            0x25 => ch.niv,
            0x27 => ch.eiv,
            0x29 => ch.mfc,
            0x2D => ch.cpr,
            0x31 => ch.dfc,
            0x39 => ch.bfc,
            _ => 0,
        };
        Ok(val)
    }

    /// Writes an 8-bit byte to the DMAC address window (`$E84000`–`$E85FFF`).
    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        let offset = (a - 0x00E8_4000) as usize;
        if offset >= 0x0100 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        if offset == 0xFF {
            self.gcr = val & 0x0F;
            return Ok(());
        }
        let ch_idx = (offset >> 6) & 3;
        let reg = offset & 0x3F;
        if self.channels[ch_idx].csr & CSR_ACT != 0
            && matches!(reg, 0x04..=0x06 | 0x0a..=0x0f | 0x14..=0x17 | 0x29 | 0x31)
        {
            self.channels[ch_idx].finish_error(CER_TIMING);
            self.schedule(ch_idx, u64::MAX);
            return Ok(());
        }
        if reg == 0x07 && val & CCR_STR != 0 && val & CCR_SAB == 0 {
            let ch = &self.channels[ch_idx];
            if ch.ocr & 2 != 0 && ch_idx != 0 {
                self.report_unsupported(
                    ch_idx,
                    4,
                    "external device request is not connected (FDC is connected on channel 0)",
                );
            }
        }
        let ch = &mut self.channels[ch_idx];

        match reg {
            0x00 => {
                // Writing 1 to status bits clears them (except ACT and PCS)
                let clear_mask = val & (CSR_COC | CSR_BTC | CSR_NDT | CSR_ERR | CSR_DIT | CSR_PCT);
                ch.csr &= !clear_mask;
                if (val & CSR_ERR) != 0 {
                    ch.cer = CER_NONE;
                }
                if (ch.csr & (CSR_COC | CSR_BTC | CSR_NDT | CSR_ERR)) == 0 {
                    ch.irq_pending = false;
                }
            }
            0x01 => {}
            0x04 => ch.dcr = val,
            0x05 => ch.ocr = val,
            0x06 => ch.scr = val & 0x0F,
            0x07 => {
                let start = val & CCR_STR != 0;
                let abort = val & CCR_SAB != 0;
                ch.ccr = val & (CCR_CNT | CCR_HLT | CCR_ITE);
                if abort {
                    if ch.csr & CSR_ACT != 0 {
                        ch.finish_error(CER_SW_ABORT);
                    }
                } else if start {
                    if ch.csr & (CSR_COC | CSR_BTC | CSR_NDT | CSR_ERR | CSR_ACT) != 0 {
                        ch.finish_error(CER_TIMING);
                    } else if ch.ocr & 0x0c == 4
                        || ch.scr & 0x0c == 0x0c
                        || ch.scr & 3 == 3
                        || ch.dcr & 0xc0 == 0x40
                    {
                        ch.finish_error(CER_CONFIG);
                    } else {
                        ch.csr |= CSR_ACT;
                        self.starting[ch_idx] = true;
                        self.memory_carry[ch_idx] = None;
                        self.device_carry[ch_idx] = None;
                    }
                }
                let runnable = self.channels[ch_idx].is_active()
                    && (self.starting[ch_idx]
                        || self.channels[ch_idx].ocr & 2 == 0
                        || self.requests & (1 << ch_idx) != 0);
                self.schedule(ch_idx, if runnable { self.clock } else { u64::MAX });
            }
            0x0A => ch.mtc = (ch.mtc & 0x00FF) | ((val as u16) << 8),
            0x0B => ch.mtc = (ch.mtc & 0xFF00) | (val as u16),
            0x0C => ch.mar = (ch.mar & 0x00FF_FFFF) | ((val as u32) << 24),
            0x0D => ch.mar = (ch.mar & 0xFF00_FFFF) | ((val as u32) << 16),
            0x0E => ch.mar = (ch.mar & 0xFFFF_00FF) | ((val as u32) << 8),
            0x0F => ch.mar = (ch.mar & 0xFFFF_FF00) | (val as u32),
            0x14 => ch.dar = (ch.dar & 0x00FF_FFFF) | ((val as u32) << 24),
            0x15 => ch.dar = (ch.dar & 0xFF00_FFFF) | ((val as u32) << 16),
            0x16 => ch.dar = (ch.dar & 0xFFFF_00FF) | ((val as u32) << 8),
            0x17 => ch.dar = (ch.dar & 0xFFFF_FF00) | (val as u32),
            0x1A => ch.btc = (ch.btc & 0x00FF) | ((val as u16) << 8),
            0x1B => ch.btc = (ch.btc & 0xFF00) | (val as u16),
            0x1C => ch.bar = (ch.bar & 0x00FF_FFFF) | ((val as u32) << 24),
            0x1D => ch.bar = (ch.bar & 0xFF00_FFFF) | ((val as u32) << 16),
            0x1E => ch.bar = (ch.bar & 0xFFFF_00FF) | ((val as u32) << 8),
            0x1F => ch.bar = (ch.bar & 0xFFFF_FF00) | (val as u32),
            0x25 => ch.niv = val,
            0x27 => ch.eiv = val,
            0x29 => ch.mfc = val,
            0x2D => ch.cpr = val & 0x03,
            0x31 => ch.dfc = val,
            0x39 => ch.bfc = val,
            _ => {}
        }
        Ok(())
    }
}
