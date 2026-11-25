//! Core X68000 on-board peripherals:
//! - RTC (`RP5C15`, `$E8A000`–`$E8BFFF`)
//! - System Port (`$E8E000`–`$E8FFFF`)
//! - OPM (`YM2151`, `$E90000`–`$E91FFF`)
//! - ADPCM (`MSM6258V`, `$E92000`–`$E93FFF`)
//! - SASI HDC (`$E96000`–`$E97FFF`)
//! - SCC (`Z8530`, `$E98000`–`$E99FFF`)
//! - PPI (`i8255`, `$E9A000`–`$E9BFFF`)

use std::time::{SystemTime, UNIX_EPOCH};

use msx68k_cpu::BusFault;

/// Broken-down calendar date/time for RP5C15 RTC registers.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct RtcDateTime {
    year: i32, // full year, e.g. 2026
    month: u8, // 1..=12
    day: u8,   // 1..=31
    wday: u8,  // 0 = Sunday .. 6 = Saturday
    hour: u8,  // 0..=23
    min: u8,   // 0..=59
    sec: u8,   // 0..=59
}

impl RtcDateTime {
    fn from_unix_secs_utc(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let sod = secs.rem_euclid(86_400) as u32;
        let hour = (sod / 3_600) as u8;
        let min = ((sod % 3_600) / 60) as u8;
        let sec = (sod % 60) as u8;
        let wday = (days + 4).rem_euclid(7) as u8; // 1970-01-01 was Thursday (4)

        // Howard Hinnant's civil-from-days algorithm
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097) as u64;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = (yoe as i64) + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let year = (y + i64::from(m <= 2)) as i32;

        Self {
            year,
            month: m,
            day: d,
            wday,
            hour,
            min,
            sec,
        }
    }

    fn now_local() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        #[cfg(unix)]
        {
            use std::ffi::{c_char, c_int, c_long};
            #[repr(C)]
            struct Tm {
                tm_sec: c_int,
                tm_min: c_int,
                tm_hour: c_int,
                tm_mday: c_int,
                tm_mon: c_int,
                tm_year: c_int,
                tm_wday: c_int,
                tm_yday: c_int,
                tm_isdst: c_int,
                _gmtoff: c_long,
                _zone: *const c_char,
                _pad: [usize; 4],
            }
            extern "C" {
                fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
            }
            let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
            let ptr = unsafe { localtime_r(&secs, tm.as_mut_ptr()) };
            if !ptr.is_null() {
                let tm = unsafe { tm.assume_init() };
                return Self {
                    year: tm.tm_year + 1900,
                    month: (tm.tm_mon + 1).clamp(1, 12) as u8,
                    day: tm.tm_mday.clamp(1, 31) as u8,
                    wday: tm.tm_wday.rem_euclid(7) as u8,
                    hour: tm.tm_hour.clamp(0, 23) as u8,
                    min: tm.tm_min.clamp(0, 59) as u8,
                    sec: tm.tm_sec.clamp(0, 59) as u8,
                };
            }
        }

        Self::from_unix_secs_utc(secs)
    }

    fn to_rp5c15_bank0(self, hour24: bool) -> ([u8; 13], u8) {
        let yr = (self.year.saturating_sub(1980).max(0) % 100) as u8;
        let leap = (self.year.rem_euclid(4)) as u8;
        let (h1, h10) = if hour24 {
            (self.hour % 10, self.hour / 10)
        } else {
            let pm = u8::from(self.hour >= 12) << 1;
            let h12 = self.hour % 12;
            (h12 % 10, pm | (h12 / 10))
        };
        let bank0 = [
            self.sec % 10,   // 0x0: 1-second
            self.sec / 10,   // 0x1: 10-second
            self.min % 10,   // 0x2: 1-minute
            self.min / 10,   // 0x3: 10-minute
            h1,              // 0x4: 1-hour
            h10,             // 0x5: 10-hour (bit 1 = AM/PM in 12h mode)
            self.wday,       // 0x6: Day of week (0 = Sunday)
            self.day % 10,   // 0x7: 1-day
            self.day / 10,   // 0x8: 10-day
            self.month % 10, // 0x9: 1-month
            self.month / 10, // 0xA: 10-month
            yr % 10,         // 0xB: 1-year (offset from 1980)
            yr / 10,         // 0xC: 10-year
        ];
        (bank0, leap)
    }
}

/// Ricoh RP5C15 Real-Time Clock (`$E8A001`–`$E8A01F` odd addresses).
#[derive(Debug, Clone)]
pub struct Rtc {
    /// Bank 0 (current time counters `0x00`..`0x0C`) and Bank 1 (alarm/clock setting `0x00`..`0x0C`).
    banks: [[u8; 13]; 2],
    /// Mode register (`0x0D`): bit 3 = Timer EN, bit 2 = Alarm EN, bit 0 = Bank select.
    mode: u8,
    /// Test register (`0x0E`).
    test: u8,
    /// When `true` (default), Bank 0 reads reflect the host's current local wall-clock time.
    /// Cleared if the guest explicitly writes Bank 0 time registers.
    host_sync: bool,
}

impl Default for Rtc {
    fn default() -> Self {
        Self::new()
    }
}

impl Rtc {
    pub fn new() -> Self {
        let now = RtcDateTime::now_local();
        let (bank0, leap) = now.to_rp5c15_bank0(true);
        let mut bank1 = [0u8; 13];
        bank1[0x0A] = 0x01; // 24-hour mode
        bank1[0x0B] = leap; // Leap year counter (year % 4)
        Self {
            banks: [bank0, bank1],
            mode: 0x08,
            test: 0x00,
            host_sync: true,
        }
    }

    /// Re-enables or disables live host wall-clock synchronization for Bank 0 reads.
    #[inline]
    pub fn set_host_sync(&mut self, enabled: bool) {
        self.host_sync = enabled;
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
                if self.host_sync {
                    let hour24 = (self.banks[1][0x0A] & 0x01) != 0;
                    let (bank0, leap) = RtcDateTime::now_local().to_rp5c15_bank0(hour24);
                    if bank == 0 {
                        bank0[reg] & 0x0F
                    } else if reg == 0x0B {
                        leap & 0x0F
                    } else {
                        self.banks[1][reg] & 0x0F
                    }
                } else {
                    self.banks[bank][reg] & 0x0F
                }
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
                if bank == 0 && self.host_sync {
                    let hour24 = (self.banks[1][0x0A] & 0x01) != 0;
                    let (bank0, leap) = RtcDateTime::now_local().to_rp5c15_bank0(hour24);
                    self.banks[0] = bank0;
                    self.banks[1][0x0B] = leap;
                    self.host_sync = false;
                }
                self.banks[bank][reg] = nibble;
            }
            0x0D => self.mode = nibble,
            0x0E => self.test = nibble,
            _ => {}
        }
        Ok(())
    }
}

pub use crate::opm::Opm;

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

    /// Sets Joystick 1 (Port A, `$E9A001`, active-low) state.
    #[inline]
    pub fn set_port_a(&mut self, value: u8) {
        self.port_a = value;
    }

    /// Returns PPI Port C (`$E9A005`, ADPCM pan/rate/clock) state.
    #[inline]
    pub const fn port_c(&self) -> u8 {
        self.port_c
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

    /// ROM and RAM wait states of an X68030, in 25 MHz clocks, as set through `$E8E009`.
    ///
    /// The high nibble sets ROM waits (`n + 2`). The low nibble sets RAM waits (`n + 2`), and
    /// 0 selects static column mode without waits. Until the IPL ROM writes it, both are 0.
    pub const fn x68030_memory_waits(&self) -> (u8, u8) {
        if self.wait_ctrl == 0xFF {
            return (0, 0);
        }
        let ram = self.wait_ctrl & 15;
        let ram = if ram == 0 { 0 } else { ram + 2 };
        ((self.wait_ctrl >> 4) + 2, ram)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_utc_conversion_and_bcd_encoding() {
        // 2026-11-25 12:34:56 UTC (Wednesday = 3)
        // Days from 1970-01-01 to 2026-11-25: 20782
        let dt = RtcDateTime::from_unix_secs_utc(20_782 * 86_400 + 12 * 3_600 + 34 * 60 + 56);
        assert_eq!(
            dt,
            RtcDateTime {
                year: 2026,
                month: 11,
                day: 25,
                wday: 3,
                hour: 12,
                min: 34,
                sec: 56,
            }
        );
        let (bank0, leap) = dt.to_rp5c15_bank0(true);
        assert_eq!(leap, 2); // 2026 % 4 == 2
        assert_eq!(bank0, [6, 5, 4, 3, 2, 1, 3, 5, 2, 1, 1, 6, 4]);
    }

    #[test]
    fn rtc_reads_live_host_time_and_allows_guest_override() {
        let mut rtc = Rtc::new();
        let sec_lo = rtc.read_u8(0xE8A001).unwrap() & 0x0F;
        let sec_hi = rtc.read_u8(0xE8A003).unwrap() & 0x0F;
        let year_lo = rtc.read_u8(0xE8A017).unwrap() & 0x0F;
        let year_hi = rtc.read_u8(0xE8A019).unwrap() & 0x0F;
        assert!(sec_hi * 10 + sec_lo < 60);
        // Host year must be >= 2020 (offset >= 40 from 1980)
        assert!(year_hi * 10 + year_lo >= 40);

        // Writing Bank 0 detaches host sync and preserves written nibbles.
        rtc.write_u8(0xE8A001, 0x07).unwrap();
        assert_eq!(rtc.read_u8(0xE8A001).unwrap(), 0xF7);
    }
}
