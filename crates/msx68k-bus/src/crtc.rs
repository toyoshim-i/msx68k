//! X68000 CRTC (`HD63450` / custom gate array `$E80000`–`$E807FF`) and TVRAM hardware acceleration.
//!
//! Supports:
//! - CRTC Registers `R00`–`R23` (`$E80000`–`$E8002F`, mirrored every `0x40` up to `$E803FF`)
//! - CRTC Operation Port (`$E80480`–`$E804FF`; `$E80400`–`$E8047F` raises Bus Error)
//! - TVRAM Simultaneous Multi-Plane Write (`R21` bit 8 and bits 7..4)
//! - TVRAM Write Bitmask (`R21` bit 9 and `R23`)
//! - Hardware 4-Scanline Raster Copy (`R21` bits 3..0, `R22`, and `$E80481` bit 3)
//! - Graphic Fast Clear (`R21` bits 3..0 and `$E80481` bit 1)
//! - Scanline / H-SYNC / V-DISP / Raster IRQ (`R09`) state tracking

use msx68k_cpu::BusFault;

use crate::bus::TVRAM_BYTES;

/// Size of a single TVRAM bitplane (`128 KB`, 1024×1024×1bpp).
pub const TVRAM_PLANE_BYTES: usize = 0x20000;
/// Size of a 4-scanline raster copy block (`4 * 128 = 512` bytes).
pub const RASTER_BLOCK_BYTES: usize = 512;

/// Transitions emitted by the CRTC when advancing scanlines/cycles, routed to the MFP (`GPIP`).
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct CrtcSyncEvents {
    /// Current `H-SYNC` level (`true` = in H-Sync pulse, `false` = in H-Display).
    pub h_sync: bool,
    /// Current `V-DISP` level (`true` = active vertical display, `false` = vertical blanking).
    pub v_disp: bool,
    /// `true` if `V-DISP` transitioned from `false` to `true` (start of active display).
    pub v_disp_rose: bool,
    /// `true` if `V-DISP` transitioned from `true` to `false` (start of V-Blank).
    pub v_disp_fell: bool,
    /// Current `RINT` active level (`true` = on matching `R09` raster).
    pub rint_active: bool,
}

/// X68000 CRT Controller (`$E80000`–`$E807FF`).
#[derive(Debug, Clone)]
pub struct Crtc {
    /// CRTC registers `R00`..`R23`.
    regs: [u16; 24],
    /// Operation port bit 3 (`$E80481` bit 3): Hardware Raster Copy active.
    raster_copy_on: bool,
    /// Operation port bit 1 (`$E80481` bit 1): Graphic Fast Clear standby.
    fast_clear_standby: bool,
    /// Current scanline number (`0..=total_rasters`).
    current_raster: u16,
    /// Cycles elapsed within the current scanline (at 10 MHz equivalent).
    cycle_in_raster: u32,
    /// Current `H-SYNC` state (`true` during H-Sync pulse).
    h_sync: bool,
    /// Current `V-DISP` state (`true` during active vertical display).
    v_disp: bool,
    /// Current `RINT` state (`true` when `current_raster == (R09 & 0x03FF)`).
    rint_active: bool,
    /// Monotonic frame counter incremented at the start of every V-Blank.
    frame_count: u64,
}

impl Default for Crtc {
    fn default() -> Self {
        Self::new()
    }
}

impl Crtc {
    /// Creates a new CRTC initialized with standard X68000 768×512 high-resolution timing defaults.
    pub fn new() -> Self {
        let mut regs = [0u16; 24];
        // Default 31.5 kHz 768x512 timing values matching IPLROM initialization:
        regs[0] = 137; // R00: H-Total - 1
        regs[1] = 14;  // R01: H-Sync End
        regs[2] = 28;  // R02: H-Disp Start - 4
        regs[3] = 124; // R03: H-Disp End - 4
        regs[4] = 567; // R04: V-Total - 1 (568 scanlines)
        regs[5] = 5;   // R05: V-Sync End
        regs[6] = 40;  // R06: V-Disp Start - 1
        regs[7] = 552; // R07: V-Disp End - 1
        regs[8] = 27;  // R08: External sync adjust
        regs[9] = 0;   // R09: Raster IRQ line
        regs[20] = 0x0416; // R20: High-res 768x512, 16-color
        Self {
            regs,
            raster_copy_on: false,
            fast_clear_standby: false,
            current_raster: 0,
            cycle_in_raster: 0,
            h_sync: true,
            v_disp: false,
            rint_active: false,
            frame_count: 0,
        }
    }

    /// Resets CRTC state on hardware `RESET`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Returns a slice of all 24 CRTC registers (`R00`..`R23`).
    #[inline]
    pub const fn regs(&self) -> &[u16; 24] {
        &self.regs
    }

    /// Returns CRTC register `idx` (`0..24`).
    #[inline]
    pub const fn reg(&self, idx: usize) -> u16 {
        self.regs[idx]
    }

    /// Returns the Text X scroll offset (`R10 & 0x03FF`).
    #[inline]
    pub const fn text_scroll_x(&self) -> u16 {
        self.regs[10] & 0x03FF
    }

    /// Returns the Text Y scroll offset (`R11 & 0x03FF`).
    #[inline]
    pub const fn text_scroll_y(&self) -> u16 {
        self.regs[11] & 0x03FF
    }

    /// Returns the Graphics page `page` (`0..4`) scroll `(x, y)`.
    #[inline]
    pub const fn graphic_scroll(&self, page: usize) -> (u16, u16) {
        let p = page & 3;
        let mask = if p == 0 && (self.regs[20] & 0x0300) != 0 {
            0x03FF
        } else {
            0x01FF
        };
        (
            self.regs[12 + p * 2] & mask,
            self.regs[13 + p * 2] & mask,
        )
    }

    /// Returns the active display resolution `(width, height)` derived from CRTC timing registers.
    pub fn active_resolution(&self) -> (u32, u32) {
        let h_cols = self.regs[3].saturating_sub(self.regs[2]) as u32;
        let width = match h_cols * 8 {
            w if (256..=1024).contains(&w) => w,
            _ => match self.regs[20] & 0x0003 {
                0 => 256,
                1 => 512,
                _ => 768,
            },
        };
        let v_lines = self.regs[7].saturating_sub(self.regs[6]) as u32;
        let height = if (128..=1024).contains(&v_lines) {
            let v_res = (self.regs[20] >> 2) & 0x03;
            let high_res = (self.regs[20] & 0x0010) != 0;
            if high_res && v_res == 0 {
                // 256-line mode on 31.5kHz monitor doubles scanlines
                (v_lines / 2).max(256)
            } else {
                v_lines
            }
        } else {
            512
        };
        (width, height)
    }

    /// Returns the current scanline (`0..=R04`).
    #[inline]
    pub const fn current_raster(&self) -> u16 {
        self.current_raster
    }

    /// Returns the monotonic frame counter.
    #[inline]
    pub const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Returns the current sync status (`H-SYNC`, `V-DISP`, `RINT`).
    #[inline]
    pub const fn sync_state(&self) -> CrtcSyncEvents {
        CrtcSyncEvents {
            h_sync: self.h_sync,
            v_disp: self.v_disp,
            v_disp_rose: false,
            v_disp_fell: false,
            rint_active: self.rint_active,
        }
    }

    #[inline]
    fn reg_mask(reg_idx: usize) -> u16 {
        match reg_idx {
            0..=3 => 0x00FF,
            4..=7 => 0x03FF,
            8 => 0x00FF,
            9..=11 => 0x03FF,
            12..=13 => 0x03FF,
            14..=19 => 0x01FF,
            20 => 0x071F,
            21 => 0x03FF,
            22 | 23 => 0xFFFF,
            _ => 0,
        }
    }

    /// Reads an 8-bit byte from the CRTC address window (`$E80000`–`$E81FFF`).
    pub fn read_u8(&self, addr: u32) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        if a < 0x00E8_0400 {
            let offset = (a & 0x3F) as usize;
            let reg_idx = offset >> 1;
            if reg_idx >= 24 {
                return Ok(0);
            }
            let val = self.regs[reg_idx];
            if (offset & 1) == 0 {
                Ok((val >> 8) as u8)
            } else {
                Ok(val as u8)
            }
        } else if a < 0x00E8_0480 {
            // $E80400..$E8047F raises a hardware Bus Error on X68000
            Err(BusFault::BusError {
                addr: a,
                is_write: false,
            })
        } else if a < 0x00E8_0500 {
            if (a & 1) == 0 {
                Ok(0)
            } else {
                let b3 = if self.raster_copy_on { 0x08 } else { 0x00 };
                let b1 = if self.fast_clear_standby { 0x02 } else { 0x00 };
                Ok(b3 | b1)
            }
        } else {
            Ok(0)
        }
    }

    /// Writes an 8-bit byte to the CRTC address window (`$E80000`–`$E81FFF`).
    pub fn write_u8(
        &mut self,
        addr: u32,
        val: u8,
        tvram: &mut Option<Box<[u8]>>,
        gvram: &mut Option<Box<[u8]>>,
    ) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if a < 0x00E8_0400 {
            let offset = (a & 0x3F) as usize;
            let reg_idx = offset >> 1;
            if reg_idx < 24 {
                let cur = self.regs[reg_idx];
                let next = if (offset & 1) == 0 {
                    (cur & 0x00FF) | ((val as u16) << 8)
                } else {
                    (cur & 0xFF00) | (val as u16)
                };
                self.regs[reg_idx] = next & Self::reg_mask(reg_idx);

                if reg_idx == 9 {
                    let irq_line = self.regs[9] & 0x03FF;
                    self.rint_active = self.current_raster == irq_line;
                } else if reg_idx == 22 && (offset & 1) == 1 && self.raster_copy_on {
                    // Software updated R22 (Src/Dst block) while Raster Copy is enabled.
                    self.execute_raster_copy(tvram);
                }
            }
            Ok(())
        } else if a < 0x00E8_0480 {
            Err(BusFault::BusError {
                addr: a,
                is_write: true,
            })
        } else if a < 0x00E8_0500 {
            if (a & 1) == 1 {
                let new_rc = (val & 0x08) != 0;
                let new_fc = (val & 0x02) != 0;
                if new_rc {
                    self.execute_raster_copy(tvram);
                }
                if new_fc && !self.fast_clear_standby {
                    self.fast_clear_standby = true;
                    // Also perform fast clear immediately if in V-Blank
                    if !self.v_disp {
                        self.execute_fast_clear(gvram);
                        self.fast_clear_standby = false;
                    }
                }
                self.raster_copy_on = new_rc;
            }
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Writes a byte to TVRAM (`$E00000`–`$E7FFFF`), applying CRTC `R21` simultaneous multi-plane
    /// write and `R23` bitmask rules.
    pub fn write_tvram_u8(&self, tvram: &mut [u8], addr: u32, val: u8) {
        let offset = (addr as usize) & (TVRAM_BYTES - 1);
        let r21 = self.regs[21];
        let use_mask = (r21 & 0x0200) != 0;
        let sim_access = (r21 & 0x0100) != 0;

        let mask = if use_mask {
            if (offset & 1) == 0 {
                (self.regs[23] >> 8) as u8
            } else {
                self.regs[23] as u8
            }
        } else {
            0x00
        };

        if sim_access {
            let in_plane = offset & (TVRAM_PLANE_BYTES - 1);
            let plane_bits = (r21 >> 4) & 0x0F;
            for plane in 0..4 {
                if (plane_bits & (1 << plane)) != 0 {
                    let idx = plane * TVRAM_PLANE_BYTES + in_plane;
                    tvram[idx] = (tvram[idx] & mask) | (val & !mask);
                }
            }
        } else {
            tvram[offset] = (tvram[offset] & mask) | (val & !mask);
        }
    }

    /// Writes a 16-bit big-endian word to TVRAM (`$E00000`–`$E7FFFF`), applying CRTC `R21`
    /// simultaneous multi-plane write and `R23` 16-bit bitmask rules.
    pub fn write_tvram_u16(&self, tvram: &mut [u8], addr: u32, val: u16) {
        let offset = (addr as usize) & (TVRAM_BYTES - 2);
        let r21 = self.regs[21];
        let use_mask = (r21 & 0x0200) != 0;
        let sim_access = (r21 & 0x0100) != 0;

        let mask_hi = if use_mask {
            (self.regs[23] >> 8) as u8
        } else {
            0x00
        };
        let mask_lo = if use_mask {
            self.regs[23] as u8
        } else {
            0x00
        };
        let val_hi = (val >> 8) as u8;
        let val_lo = val as u8;

        if sim_access {
            let in_plane = offset & (TVRAM_PLANE_BYTES - 2);
            let plane_bits = (r21 >> 4) & 0x0F;
            for plane in 0..4 {
                if (plane_bits & (1 << plane)) != 0 {
                    let idx = plane * TVRAM_PLANE_BYTES + in_plane;
                    tvram[idx] = (tvram[idx] & mask_hi) | (val_hi & !mask_hi);
                    tvram[idx + 1] = (tvram[idx + 1] & mask_lo) | (val_lo & !mask_lo);
                }
            }
        } else {
            tvram[offset] = (tvram[offset] & mask_hi) | (val_hi & !mask_hi);
            tvram[offset + 1] = (tvram[offset + 1] & mask_lo) | (val_lo & !mask_lo);
        }
    }

    /// Executes a hardware 4-scanline (`512`-byte) raster copy from `R22` high byte (`src`)
    /// to `R22` low byte (`dst`) across the TVRAM planes selected in `R21 & 0x0F`.
    pub fn execute_raster_copy(&self, tvram: &mut Option<Box<[u8]>>) {
        let plane_mask = self.regs[21] & 0x000F;
        if plane_mask == 0 {
            return;
        }
        let src_block = ((self.regs[22] >> 8) & 0xFF) as usize;
        let dst_block = (self.regs[22] & 0xFF) as usize;
        if src_block == dst_block {
            return;
        }
        let buf = tvram.get_or_insert_with(|| vec![0u8; TVRAM_BYTES].into_boxed_slice());
        let src_off = src_block * RASTER_BLOCK_BYTES;
        let dst_off = dst_block * RASTER_BLOCK_BYTES;
        for plane in 0..4 {
            if (plane_mask & (1 << plane)) != 0 {
                let base = plane * TVRAM_PLANE_BYTES;
                buf.copy_within(
                    (base + src_off)..(base + src_off + RASTER_BLOCK_BYTES),
                    base + dst_off,
                );
            }
        }
    }

    /// Executes Graphic Fast Clear on GVRAM pages enabled in `R21 & 0x0F`.
    pub fn execute_fast_clear(&self, gvram: &mut Option<Box<[u8]>>) {
        let page_mask = self.regs[21] & 0x000F;
        if page_mask == 0 {
            return;
        }
        let Some(buf) = gvram.as_mut() else {
            return;
        };
        if page_mask == 0x000F {
            buf.fill(0);
            return;
        }
        // In 16-color 4-page mode, each word at (y * 512 + x) holds 4 nibbles or pages are interleaved.
        // Clearing selected nibbles across GVRAM:
        let mut clear_mask_u16 = 0xFFFFu16;
        for page in 0..4 {
            if (page_mask & (1 << page)) != 0 {
                clear_mask_u16 &= !(0x000F << (page * 4));
            }
        }
        let hi_mask = (clear_mask_u16 >> 8) as u8;
        let lo_mask = clear_mask_u16 as u8;
        for chunk in buf.chunks_exact_mut(2) {
            chunk[0] &= hi_mask;
            chunk[1] &= lo_mask;
        }
    }

    /// Advances the CRTC timing state by `cycles_10mhz` (10 MHz clock cycles)
    /// and returns any sync transitions for the MFP.
    pub fn advance(
        &mut self,
        cycles_10mhz: u32,
        _tvram: &mut Option<Box<[u8]>>,
        gvram: &mut Option<Box<[u8]>>,
    ) -> CrtcSyncEvents {
        // Approximate 10 MHz cycles per scanline:
        // In 31.5 kHz high-res mode, 1 scanline is ~317 cycles (10_000_000 / 31_500).
        // In 15.98 kHz low-res mode, 1 scanline is ~625 cycles.
        let scanline_cycles: u32 = if (self.regs[20] & 0x0010) != 0 {
            317
        } else {
            625
        };
        let h_sync_cycles: u32 = 40;

        self.cycle_in_raster = self.cycle_in_raster.saturating_add(cycles_10mhz);

        let mut v_disp_rose = false;
        let mut v_disp_fell = false;

        while self.cycle_in_raster >= scanline_cycles {
            self.cycle_in_raster -= scanline_cycles;

            let v_total = (self.regs[4] & 0x03FF).max(256);
            if self.current_raster >= v_total {
                self.current_raster = 0;
            } else {
                self.current_raster += 1;
            }

            let v_disp_start = (self.regs[6] & 0x03FF).saturating_add(1);
            let v_disp_end = (self.regs[7] & 0x03FF).saturating_add(1);
            let next_v_disp =
                self.current_raster >= v_disp_start && self.current_raster < v_disp_end;

            if next_v_disp != self.v_disp {
                if next_v_disp {
                    v_disp_rose = true;
                } else {
                    v_disp_fell = true;
                    self.frame_count = self.frame_count.wrapping_add(1);
                    if self.fast_clear_standby {
                        self.execute_fast_clear(gvram);
                        self.fast_clear_standby = false;
                    }
                }
                self.v_disp = next_v_disp;
            }

            let irq_line = self.regs[9] & 0x03FF;
            self.rint_active = self.current_raster == irq_line;
        }

        self.h_sync = self.cycle_in_raster < h_sync_cycles;

        CrtcSyncEvents {
            h_sync: self.h_sync,
            v_disp: self.v_disp,
            v_disp_rose,
            v_disp_fell,
            rint_active: self.rint_active,
        }
    }
}
