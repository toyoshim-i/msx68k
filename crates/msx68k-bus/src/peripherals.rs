//! Core X68000 on-board peripherals:
//! - RTC (`RP5C15`, `$E8A000`–`$E8BFFF`)
//! - System Port (`$E8E000`–`$E8FFFF`)
//! - OPM (`YM2151`, `$E90000`–`$E91FFF`)
//! - ADPCM (`MSM6258V`, `$E92000`–`$E93FFF`)
//! - SASI HDC (`$E96000`–`$E97FFF`)
//! - SCC (`Z8530`, `$E98000`–`$E99FFF`)
//! - PPI (`i8255`, `$E9A000`–`$E9BFFF`)

use msx68k_cpu::BusFault;

/// Ricoh RP5C15 Real-Time Clock (`$E8A001`–`$E8A01F` odd addresses).
#[derive(Debug, Clone)]
pub struct Rtc {
    /// Bank 0 (current time counters `0x00`..`0x0C`) and Bank 1 (alarm/clock setting `0x00`..`0x0C`).
    banks: [[u8; 13]; 2],
    /// Mode register (`0x0D`): bit 3 = Timer EN, bit 2 = Alarm EN, bit 0 = Bank select.
    mode: u8,
    /// Test register (`0x0E`).
    test: u8,
}

impl Default for Rtc {
    fn default() -> Self {
        Self::new()
    }
}

impl Rtc {
    pub fn new() -> Self {
        // Default time: 2026-11-08 (Year 46 from 1980) 12:00:00 Sunday
        let bank0 = [
            0, // 0x0: 1-second
            0, // 0x1: 10-second
            0, // 0x2: 1-minute
            0, // 0x3: 10-minute
            2, // 0x4: 1-hour
            1, // 0x5: 10-hour (12:00)
            0, // 0x6: Day of week (0 = Sunday)
            8, // 0x7: 1-day
            0, // 0x8: 10-day (08)
            1, // 0x9: 1-month
            1, // 0xA: 10-month (11)
            6, // 0xB: 1-year
            4, // 0xC: 10-year (46 -> 1980 + 46 = 2026)
        ];
        let mut bank1 = [0u8; 13];
        bank1[0x0A] = 0x01; // 24-hour mode
        bank1[0x0B] = 0x02; // Leap year counter (2026 % 4 == 2)
        Self {
            banks: [bank0, bank1],
            mode: 0x08,
            test: 0x00,
        }
    }

    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        let reg = ((a >> 1) & 0x0F) as usize;
        let val = match reg {
            0x00..=0x0C => {
                let bank = (self.mode & 0x01) as usize;
                self.banks[bank][reg] & 0x0F
            }
            0x0D => self.mode & 0x0F,
            0x0E => self.test & 0x0F,
            0x0F => 0x00,
            _ => 0x00,
        };
        Ok(0xF0 | val)
    }

    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        let reg = ((a >> 1) & 0x0F) as usize;
        let nibble = val & 0x0F;
        match reg {
            0x00..=0x0C => {
                let bank = (self.mode & 0x01) as usize;
                self.banks[bank][reg] = nibble;
            }
            0x0D => self.mode = nibble,
            0x0E => self.test = nibble,
            _ => {}
        }
        Ok(())
    }
}

/// Yamaha YM2151 FM Operator Type-M (`$E90001` addr, `$E90003` data/status).
#[derive(Debug, Clone)]
pub struct Opm {
    addr_latch: u8,
    regs: [u8; 256],
    status: u8,
    timer_a_sub: u32,
    timer_b_sub: u32,
}

impl Default for Opm {
    fn default() -> Self {
        Self::new()
    }
}

impl Opm {
    pub const fn new() -> Self {
        Self {
            addr_latch: 0,
            regs: [0; 256],
            status: 0,
            timer_a_sub: 0,
            timer_b_sub: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Returns `true` if OPM `CT2` (register `$1B` bit 6) is high, which forces FDC Ready.
    #[inline]
    pub const fn ct2_fdc_enforced_ready(&self) -> bool {
        (self.regs[0x1B] & 0x40) != 0
    }

    /// Returns `true` if OPM is asserting its interrupt line (`GPIP3`).
    #[inline]
    pub const fn irq_active(&self) -> bool {
        (self.status & 0x03) != 0
    }

    /// Advances OPM Timers A and B by `cycles_10mhz` CPU cycles.
    pub fn advance(&mut self, cycles_10mhz: u32) {
        let ctrl = self.regs[0x14];
        // Timer A: 10-bit value from regs[0x10] (hi 8) and regs[0x11] (lo 2)
        if (ctrl & 0x01) != 0 {
            let ta = ((self.regs[0x10] as u32) << 2) | ((self.regs[0x11] & 0x03) as u32);
            let period_10mhz = (1024 - ta).max(1) * 160;
            self.timer_a_sub = self.timer_a_sub.saturating_add(cycles_10mhz);
            if self.timer_a_sub >= period_10mhz {
                self.timer_a_sub %= period_10mhz;
                if (ctrl & 0x04) != 0 {
                    self.status |= 0x01;
                }
            }
        }
        // Timer B: 8-bit value from regs[0x12]
        if (ctrl & 0x02) != 0 {
            let tb = self.regs[0x12] as u32;
            let period_10mhz = (256 - tb).max(1) * 2560;
            self.timer_b_sub = self.timer_b_sub.saturating_add(cycles_10mhz);
            if self.timer_b_sub >= period_10mhz {
                self.timer_b_sub %= period_10mhz;
                if (ctrl & 0x08) != 0 {
                    self.status |= 0x02;
                }
            }
        }
    }

    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        if (a & 3) == 3 {
            // Bit 7 (Busy) is always 0; bits 1..0 are Timer B / Timer A flags
            Ok(self.status & 0x03)
        } else {
            Ok(0xFF)
        }
    }

    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        if (a & 3) == 1 {
            self.addr_latch = val;
        } else if (a & 3) == 3 {
            let r = self.addr_latch as usize;
            self.regs[r] = val;
            if r == 0x14 {
                if (val & 0x10) != 0 {
                    self.status &= !0x01;
                }
                if (val & 0x20) != 0 {
                    self.status &= !0x02;
                }
            }
        }
        Ok(())
    }
}

/// Zilog Z8530 Serial Communications Controller (`$E98001`–`$E98007` odd addresses).
///
/// Channel B (`$E98001` Control, `$E98003` Data) interfaces the X68000 3-byte serial mouse
/// (`[status, dx, dy]`) at Interrupt Priority Level 5 (`Vector $54` / `$55`).
#[derive(Debug, Default, Clone)]
pub struct Scc {
    reg_ptr_a: u8,
    reg_ptr_b: u8,
    regs_a: [u8; 16],
    regs_b: [u8; 16],
    rx_queue_b: [u8; 3],
    rx_head_b: usize,
    rx_len_b: usize,
    ius_b: bool,
    accum_dx: i32,
    accum_dy: i32,
    left_button: bool,
    right_button: bool,
    poll_count: u64,
}

impl Scc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resets SCC registers while preserving host mouse button state.
    pub fn reset(&mut self) {
        self.reg_ptr_a = 0;
        self.reg_ptr_b = 0;
        self.regs_a = [0; 16];
        self.regs_b = [0; 16];
        self.rx_head_b = 0;
        self.rx_len_b = 0;
        self.ius_b = false;
        self.poll_count = 0;
    }

    /// Returns the number of mouse polls triggered by RTS 0 -> 1 transitions.
    #[inline]
    pub const fn poll_count(&self) -> u64 {
        self.poll_count
    }

    /// Accumulates relative mouse movement (`+dx` right, `+dy` down) to be reported on the next
    /// SCC Channel B `RTS` (`WR5` bit 1) polling edge.
    pub fn add_mouse_delta(&mut self, dx: i32, dy: i32) {
        self.accum_dx = self.accum_dx.saturating_add(dx);
        self.accum_dy = self.accum_dy.saturating_add(dy);
    }

    /// Updates left and right mouse button state.
    pub fn set_mouse_buttons(&mut self, left: bool, right: bool) {
        self.left_button = left;
        self.right_button = right;
    }

    /// Returns `true` if SCC Channel B has a pending Level 5 Receive interrupt.
    #[inline]
    pub const fn has_pending_interrupt(&self) -> bool {
        let mie = ((self.regs_b[9] | self.regs_a[9]) & 0x08) != 0;
        let rx_int_en = (self.regs_b[1] & 0x18) != 0;
        mie && rx_int_en && !self.ius_b && self.rx_len_b > 0
    }

    /// Acknowledges the Level 5 SCC interrupt and returns the status-modified vector
    /// (`(WR2 & 0xF0) | 0x04` = `0x54` for Channel B Receive Character Available).
    pub fn acknowledge_interrupt(&mut self) -> Option<u8> {
        if !self.has_pending_interrupt() {
            return None;
        }
        self.ius_b = true;
        let base = if self.regs_b[2] != 0 {
            self.regs_b[2]
        } else if self.regs_a[2] != 0 {
            self.regs_a[2]
        } else {
            0x50
        };
        Some((base & 0xF0) | 0x04)
    }

    pub fn read_u8(&mut self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        match a & 0x07 {
            0x01 => {
                // Channel B control
                let r = (self.reg_ptr_b & 0x0F) as usize;
                self.reg_ptr_b = 0;
                if r == 0 {
                    // RR0: Bit 0 = Rx Char Available (0x01), Bit 2 = Tx Buffer Empty (0x04), Bit 5 = CTS (0x20)
                    let rx_avail = u8::from(self.rx_len_b > 0);
                    Ok(0x24 | rx_avail)
                } else if r == 1 {
                    Ok(0x01) // All Sent
                } else if r == 2 {
                    let base = if self.regs_b[2] != 0 {
                        self.regs_b[2]
                    } else {
                        self.regs_a[2]
                    };
                    if self.rx_len_b > 0 {
                        Ok((base & 0xF0) | 0x04)
                    } else {
                        Ok((base & 0xF0) | 0x06)
                    }
                } else {
                    Ok(self.regs_b[r])
                }
            }
            0x03 => {
                // Channel B data (Mouse 3-byte packet stream)
                if self.rx_len_b > 0 {
                    let b = self.rx_queue_b[self.rx_head_b];
                    self.rx_head_b += 1;
                    self.rx_len_b -= 1;
                    Ok(b)
                } else {
                    Ok(0x00)
                }
            }
            0x05 => {
                // Channel A control
                let r = (self.reg_ptr_a & 0x0F) as usize;
                self.reg_ptr_a = 0;
                if r == 0 {
                    Ok(0x24)
                } else if r == 1 {
                    Ok(0x01)
                } else {
                    Ok(self.regs_a[r])
                }
            }
            0x07 => Ok(0x00), // Channel A data
            _ => Ok(0x00),
        }
    }

    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        match a & 0x07 {
            0x01 => {
                if self.reg_ptr_b == 0 {
                    let cmd = (val >> 3) & 0x07;
                    if cmd == 0x07 || cmd == 0x05 || cmd == 0x06 {
                        // Reset Highest IUS (0x38) / Reset Tx Int (0x28) / Error Reset (0x30)
                        self.ius_b = false;
                    }
                    let high = if cmd == 0x01 { 8 } else { 0 };
                    self.reg_ptr_b = (val & 0x07) | high;
                } else {
                    let r = (self.reg_ptr_b & 0x0F) as usize;
                    let old = self.regs_b[r];
                    self.regs_b[r] = val;
                    self.reg_ptr_b = 0;
                    if r == 5 && (old & 0x02) == 0 && (val & 0x02) != 0 {
                        // RTS (bit 1) 0 -> 1 edge triggers 3-byte mouse packet [status, dx, dy]
                        self.poll_count = self.poll_count.saturating_add(1);
                        let dx = self.accum_dx.clamp(-127, 127);
                        let dy = self.accum_dy.clamp(-127, 127);
                        self.accum_dx -= dx;
                        self.accum_dy -= dy;
                        let status = (if self.right_button { 0x02 } else { 0x00 })
                            | (if self.left_button { 0x01 } else { 0x00 });
                        self.rx_queue_b = [status, (dx as i8) as u8, (dy as i8) as u8];
                        self.rx_head_b = 0;
                        self.rx_len_b = 3;
                        self.ius_b = false;
                    }
                }
            }
            0x05 => {
                if self.reg_ptr_a == 0 {
                    let cmd = (val >> 3) & 0x07;
                    if cmd == 0x07 {
                        self.ius_b = false;
                    }
                    let high = if cmd == 0x01 { 8 } else { 0 };
                    self.reg_ptr_a = (val & 0x07) | high;
                } else {
                    let r = (self.reg_ptr_a & 0x0F) as usize;
                    self.regs_a[r] = val;
                    self.reg_ptr_a = 0;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Intel 8255 Programmable Peripheral Interface (`$E9A001`–`$E9A007` odd addresses).
#[derive(Debug, Clone)]
pub struct Ppi {
    port_a: u8,
    port_b: u8,
    port_c: u8,
}

impl Default for Ppi {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppi {
    pub const fn new() -> Self {
        Self {
            port_a: 0xFF, // Joystick 1 idle
            port_b: 0xFF, // Joystick 2 idle
            port_c: 0x0B, // Default ADPCM pan/rate
        }
    }

    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        match a & 0x07 {
            0x01 => Ok(self.port_a),
            0x03 => Ok(self.port_b),
            0x05 => Ok(self.port_c),
            0x07 => Ok(0x92),
            _ => Ok(0xFF),
        }
    }

    pub fn write_u8(&mut self, addr: u32, val: u8) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if (a & 1) == 0 {
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        match a & 0x07 {
            0x05 => self.port_c = val,
            0x07 => {
                // Bit-set/reset command when bit 7 == 0
                if (val & 0x80) == 0 {
                    let bit = (val >> 1) & 0x07;
                    if (val & 0x01) != 0 {
                        self.port_c |= 1 << bit;
                    } else {
                        self.port_c &= !(1 << bit);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// X68000 System Port (`$E8E000`–`$E8FFFF`).
#[derive(Debug, Clone)]
pub struct SystemPort {
    pub contrast: u8,
    pub tv_ctrl: u8,
    pub image_unit: u8,
    pub hrl: u8,
    pub wait_ctrl: u8,
    pub power_ctrl: u8,
}

impl Default for SystemPort {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemPort {
    pub const fn new() -> Self {
        Self {
            contrast: 0x0E,
            tv_ctrl: 0x00,
            image_unit: 0x00,
            hrl: 0x00,
            wait_ctrl: 0xFF,
            power_ctrl: 0x00,
        }
    }

    pub fn read_u8(&self, addr: u32, cpu_mhz: u32) -> u8 {
        let a = addr & 0x00FF_FFFF;
        match a & 0x0F {
            0x01 => 0xF0 | (self.contrast & 0x0F),
            0x03 => 0xF0 | (self.tv_ctrl & 0x08),
            0x05 => 0xE0 | (self.image_unit & 0x1F),
            // Bit 3 = 1 (Keyboard connected/ready `kbdOn`), Bit 1 = HRL
            0x07 => 0xF4 | 0x08 | ((self.hrl & 0x01) << 1),
            0x09 => self.wait_ctrl,
            // MPU type & clock: 0xFF for 10MHz 68000, 0xFE for 16MHz XVI, 0xDC for 030
            0x0B => {
                if cpu_mhz >= 16 {
                    0xFE
                } else {
                    0xFF
                }
            }
            0x0D => 0xFF,
            0x0F => 0xF0 | (self.power_ctrl & 0x0F),
            _ => 0xFF,
        }
    }
}
