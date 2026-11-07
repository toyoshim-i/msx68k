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
pub const CER_BUS_MEM: u8 = 0x09;
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

    fn step_mar(&mut self, step: u32) {
        match (self.scr >> 2) & 0x03 {
            1 => self.mar = self.mar.wrapping_add(step),
            2 => self.mar = self.mar.wrapping_sub(step),
            _ => {}
        }
    }

    fn step_dar(&mut self, step: u32) {
        match self.scr & 0x03 {
            1 => self.dar = self.dar.wrapping_add(step),
            2 => self.dar = self.dar.wrapping_sub(step),
            _ => {}
        }
    }

    fn load_next_chain_block(&mut self, main_ram: &[u8]) -> Result<bool, u8> {
        let chain_mode = (self.ocr >> 2) & 0x03;
        match chain_mode {
            0 => {
                // No chaining; check Continue (CNT)
                if (self.ccr & CCR_CNT) != 0 {
                    self.mar = self.bar;
                    self.mtc = self.btc;
                    self.ccr &= !CCR_CNT;
                    self.csr |= CSR_BTC;
                    if (self.ccr & CCR_ITE) != 0 {
                        self.irq_pending = true;
                    }
                    Ok(self.mtc != 0)
                } else {
                    Ok(false)
                }
            }
            2 => {
                // Array chaining: BAR points to [MAR.L (4 bytes), MTC.W (2 bytes)]
                if self.btc == 0 {
                    return Ok(false);
                }
                let base = (self.bar & 0x00FF_FFFF) as usize;
                if base + 6 > main_ram.len() {
                    return Err(CER_BUS_MEM);
                }
                let next_mar = u32::from_be_bytes([
                    main_ram[base],
                    main_ram[base + 1],
                    main_ram[base + 2],
                    main_ram[base + 3],
                ]);
                let next_mtc = u16::from_be_bytes([main_ram[base + 4], main_ram[base + 5]]);
                self.bar = self.bar.wrapping_add(6);
                self.btc = self.btc.wrapping_sub(1);
                self.mar = next_mar;
                self.mtc = next_mtc;
                Ok(next_mtc != 0)
            }
            3 => {
                // Link array chaining: BAR points to [MAR.L (4), MTC.W (2), NEXT_BAR.L (4)]
                if self.bar == 0 {
                    return Ok(false);
                }
                let base = (self.bar & 0x00FF_FFFF) as usize;
                if base + 10 > main_ram.len() {
                    return Err(CER_BUS_MEM);
                }
                let next_mar = u32::from_be_bytes([
                    main_ram[base],
                    main_ram[base + 1],
                    main_ram[base + 2],
                    main_ram[base + 3],
                ]);
                let next_mtc = u16::from_be_bytes([main_ram[base + 4], main_ram[base + 5]]);
                let next_bar = u32::from_be_bytes([
                    main_ram[base + 6],
                    main_ram[base + 7],
                    main_ram[base + 8],
                    main_ram[base + 9],
                ]);
                self.bar = next_bar;
                self.mar = next_mar;
                self.mtc = next_mtc;
                Ok(next_mtc != 0)
            }
            _ => Err(CER_CONFIG),
        }
    }

    fn finish_normal(&mut self) {
        self.csr = (self.csr & !CSR_ACT) | CSR_COC;
        self.ccr &= !(CCR_STR | CCR_CNT | CCR_HLT);
        if (self.ccr & CCR_ITE) != 0 {
            self.irq_pending = true;
        }
    }

    fn finish_error(&mut self, err_code: u8) {
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
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
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

    /// Transfers a single byte from an external device (e.g., FDC channel 0) into `main_ram` via DMA channel `ch_idx`.
    /// Returns `true` if the byte was transferred, or `false` if the channel is inactive/exhausted.
    pub fn device_to_memory_u8(
        &mut self,
        ch_idx: usize,
        val: u8,
        main_ram: &mut [u8],
    ) -> bool {
        let ch = &mut self.channels[ch_idx & 3];
        if !ch.is_active() || !ch.is_device_to_memory() {
            return false;
        }
        if ch.mtc == 0 {
            match ch.load_next_chain_block(main_ram) {
                Ok(true) => {}
                Ok(false) => {
                    ch.finish_normal();
                    return false;
                }
                Err(code) => {
                    ch.finish_error(code);
                    return false;
                }
            }
        }

        let addr = (ch.mar & 0x00FF_FFFF) as usize;
        if addr >= main_ram.len() {
            ch.finish_error(CER_BUS_MEM);
            return false;
        }
        main_ram[addr] = val;
        ch.step_mar(1);
        ch.mtc = ch.mtc.wrapping_sub(1);

        if ch.mtc == 0 {
            match ch.load_next_chain_block(main_ram) {
                Ok(true) => {}
                Ok(false) => ch.finish_normal(),
                Err(code) => ch.finish_error(code),
            }
        }
        true
    }

    /// Transfers a single byte from `main_ram` to an external device via DMA channel `ch_idx`.
    pub fn memory_to_device_u8(
        &mut self,
        ch_idx: usize,
        main_ram: &[u8],
    ) -> Option<u8> {
        let ch = &mut self.channels[ch_idx & 3];
        if !ch.is_active() || ch.is_device_to_memory() {
            return None;
        }
        if ch.mtc == 0 {
            match ch.load_next_chain_block(main_ram) {
                Ok(true) => {}
                Ok(false) => {
                    ch.finish_normal();
                    return None;
                }
                Err(code) => {
                    ch.finish_error(code);
                    return None;
                }
            }
        }

        let addr = (ch.mar & 0x00FF_FFFF) as usize;
        if addr >= main_ram.len() {
            ch.finish_error(CER_BUS_MEM);
            return None;
        }
        let val = main_ram[addr];
        ch.step_mar(1);
        ch.mtc = ch.mtc.wrapping_sub(1);

        if ch.mtc == 0 {
            match ch.load_next_chain_block(main_ram) {
                Ok(true) => {}
                Ok(false) => ch.finish_normal(),
                Err(code) => ch.finish_error(code),
            }
        }
        Some(val)
    }

    /// Executes an auto-request (`REQG == 0` or `1`) memory-to-memory DMA transfer on channel `ch_idx`.
    fn run_auto_request(&mut self, ch_idx: usize, main_ram: &mut [u8]) {
        let ch = &mut self.channels[ch_idx & 3];
        let size_code = (ch.ocr >> 4) & 0x03;
        let unit_bytes: u32 = match size_code {
            0 | 3 => 1,
            1 => 2,
            2 => 4,
            _ => 1,
        };
        let dar_step = if unit_bytes >= 2 { unit_bytes } else { 2 };

        while ch.is_active() {
            if ch.mtc == 0 {
                match ch.load_next_chain_block(main_ram) {
                    Ok(true) => continue,
                    Ok(false) => {
                        ch.finish_normal();
                        break;
                    }
                    Err(code) => {
                        ch.finish_error(code);
                        break;
                    }
                }
            }

            let m_addr = (ch.mar & 0x00FF_FFFF) as usize;
            let d_addr = (ch.dar & 0x00FF_FFFF) as usize;
            let len = unit_bytes as usize;
            if m_addr + len > main_ram.len() || d_addr + len > main_ram.len() {
                ch.finish_error(CER_BUS_MEM);
                break;
            }

            for b in 0..len {
                if ch.is_device_to_memory() {
                    main_ram[m_addr + b] = main_ram[d_addr + b];
                } else {
                    main_ram[d_addr + b] = main_ram[m_addr + b];
                }
            }
            ch.step_mar(unit_bytes);
            ch.step_dar(dar_step);
            ch.mtc = ch.mtc.wrapping_sub(1);
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
    pub fn write_u8(
        &mut self,
        addr: u32,
        val: u8,
        main_ram: &mut [u8],
    ) -> Result<(), BusFault> {
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
                let start = (val & CCR_STR) != 0;
                let abort = (val & CCR_SAB) != 0;
                ch.ccr = val & (CCR_CNT | CCR_HLT | CCR_ITE);
                if abort {
                    if (ch.csr & CSR_ACT) != 0 {
                        ch.finish_error(CER_SW_ABORT);
                    }
                } else if start {
                    if (ch.csr & (CSR_COC | CSR_BTC | CSR_NDT | CSR_ERR | CSR_ACT)) != 0 {
                        ch.finish_error(CER_TIMING);
                    } else {
                        ch.csr |= CSR_ACT;
                        let chain_mode = (ch.ocr >> 2) & 0x03;
                        if chain_mode == 2 || chain_mode == 3 {
                            match ch.load_next_chain_block(main_ram) {
                                Ok(true) => {}
                                Ok(false) => ch.finish_error(CER_COUNT),
                                Err(code) => ch.finish_error(code),
                            }
                        } else if ch.mtc == 0 {
                            ch.finish_error(CER_COUNT);
                        }
                        if (ch.csr & CSR_ACT) != 0 && (ch.ocr & 0x02) == 0 {
                            // Auto-request mode (REQG = 00 or 01)
                            self.run_auto_request(ch_idx, main_ram);
                        }
                    }
                }
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
