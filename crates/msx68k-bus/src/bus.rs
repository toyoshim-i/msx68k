//! X68000 24-bit system bus implementation with lazy on-demand VRAM allocation,
//! region-aware wait-state lookup, and full on-board peripheral emulation
//! (CRTC, VICON/Palette, DMAC, MFP, RTC, System Port, OPM, ADPCM, FDC/FDD, SASI HDC, SCC, PPI, IOI).

use msx68k_cpu::{Bus, BusFault};

use crate::crtc::Crtc;
use crate::dmac::Dmac;
use crate::fdc::{Fdc, IoInterruptController};
use crate::mfp::Mfp;
use crate::peripherals::{Opm, Ppi, Rtc, Scc, SystemPort};
use crate::timing::{BusTimingConfig, MemoryRegion, WaitStateTable};
use crate::vicon::Vicon;

/// Maximum Main RAM size on X68000 (`12 MB`, `$000000`–`$BFFFFF`).
pub const MAX_MAIN_RAM_BYTES: usize = 12 * 1024 * 1024;
/// GVRAM physical capacity (`512 KB`).
pub const GVRAM_BYTES: usize = 512 * 1024;
/// TVRAM physical capacity (`512 KB`).
pub const TVRAM_BYTES: usize = 512 * 1024;
/// Sprite / PCG / BG window capacity (`64 KB`, `$EB0000`–`$EBFFFF`).
pub const SPRITE_WINDOW_BYTES: usize = 64 * 1024;
/// Core I/O window capacity (`128 KB`, `$E80000`–`$E9FFFF`).
pub const IO_WINDOW_BYTES: usize = 128 * 1024;
/// ROM window capacity (`1 MB`, `$F00000`–`$FFFFFF`).
pub const ROM_WINDOW_BYTES: usize = 1024 * 1024;

/// Flags recording which hardware subsystems a running program has accessed directly.
///
/// Used by the multi-VM manager to decide when a console process inside `コマンド.SX`
/// should be detached into an isolated X68000 VM window (video hardware touched) versus
/// kept in the Master SX-Window VM (only DOS/text or sound I/O touched).
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct HardwareAccessFlags {
    pub gvram_accessed: bool,
    pub tvram_accessed: bool,
    pub crtc_accessed: bool,
    pub palette_vicon_accessed: bool,
    pub sprite_bg_accessed: bool,
    pub sound_io_accessed: bool,
}

impl HardwareAccessFlags {
    /// Returns `true` if any video/display hardware (GVRAM, TVRAM, CRTC, Palette/VICON, Sprite/BG)
    /// was directly accessed, which triggers isolated VM migration in `auto` placement mode.
    #[inline]
    pub const fn requires_isolated_display_vm(&self) -> bool {
        self.gvram_accessed
            || self.tvram_accessed
            || self.crtc_accessed
            || self.palette_vicon_accessed
            || self.sprite_bg_accessed
    }
}

/// X68000 24-bit system bus with configurable CPU/System Bus wait states, on-demand VRAM allocation,
/// and integrated motherboard peripherals.
pub struct X68kBus {
    /// Main RAM (`$000000`..`main_ram.len()`). Accesses between `main_ram.len()` and `$BFFFFF`
    /// raise a hardware Bus Error (`Vector 2`), matching real X68000 memory sizing probes.
    main_ram: Vec<u8>,
    /// Lazily allocated GVRAM (`512 KB`).
    gvram: Option<Box<[u8]>>,
    /// Lazily allocated TVRAM (`512 KB`, `$E00000`–`$E7FFFF`).
    tvram: Option<Box<[u8]>>,
    /// Lazily allocated Sprite / PCG / BG space (`64 KB`, `$EB0000`–`$EBFFFF`).
    sprite_bg: Option<Box<[u8]>>,
    /// Battery-backed SRAM (`$ED0000`–`$ED3FFF`, 16 KB).
    sram: Vec<u8>,
    /// Write-unlock latch for SRAM (`$E8E00D == 0x31`).
    sram_write_enabled: bool,
    /// Supervisor Area Set boundary (`($E86001 + 1) * 8192`).
    supervisor_area_limit: u32,
    /// Enforce supervisor protection on Main RAM below `supervisor_area_limit` and I/O space.
    enforce_supervisor_protection: bool,
    /// Font & IPL ROM (`1 MB`, `$F00000`–`$FFFFFF`).
    rom: Vec<u8>,
    /// Precomputed 4 KB page wait-state table.
    wait_table: WaitStateTable,
    /// Recorded hardware subsystem access flags.
    access_flags: HardwareAccessFlags,

    // On-board X68000 peripherals
    /// CRT Controller (`$E80000`–`$E81FFF`).
    pub crtc: Crtc,
    /// Video Controller & Palette RAM (`$E82000`–`$E83FFF`).
    pub vicon: Vicon,
    /// HD63450 4-Channel DMA Controller (`$E84000`–`$E85FFF`, Level 3).
    pub dmac: Dmac,
    /// MC68901 Multi-Function Peripheral (`$E88000`–`$E89FFF`, Level 6).
    pub mfp: Mfp,
    /// RP5C15 Real-Time Clock (`$E8A000`–`$E8BFFF`).
    pub rtc: Rtc,
    /// System Port (`$E8E000`–`$E8FFFF`).
    pub sysport: SystemPort,
    /// YM2151 FM Sound Generator (`$E90000`–`$E91FFF`).
    pub opm: Opm,
    /// MSM6258V ADPCM status (`$E92000`–`$E93FFF`).
    pub adpcm_active: bool,
    /// uPD72065 Floppy Disk Controller & 4-Drive FDD (`$E94000`–`$E95FFF`).
    pub fdc: Fdc,
    /// Z8530 Serial Communications Controller (`$E98000`–`$E99FFF`).
    pub scc: Scc,
    /// i8255 Programmable Peripheral Interface (`$E9A000`–`$E9BFFF`).
    pub ppi: Ppi,
    /// I/O Interrupt Controller (`$E9C000`–`$E9DFFF`, Level 1).
    pub ioi: IoInterruptController,
}

impl X68kBus {
    /// Creates a new X68000 bus with `ram_size_bytes` (clamped to `12 MB`) and `timing` configuration.
    ///
    /// Video RAM buffers start as `None` (zero allocation overhead) and are instantiated on demand
    /// when first written.
    pub fn new(ram_size_bytes: usize, timing: BusTimingConfig) -> Self {
        let ram_bytes = ram_size_bytes.min(MAX_MAIN_RAM_BYTES);
        let mut sram = vec![0u8; 16 * 1024];
        // Populate standard X68000 SRAM header ("X68000W" at $ED0000) and configured RAM size at $ED0008
        sram[0..8].copy_from_slice(b"\x00X68000W");
        let ram_end = (ram_bytes as u32).to_be_bytes();
        sram[8..12].copy_from_slice(&ram_end);

        Self {
            main_ram: vec![0; ram_bytes],
            gvram: None,
            tvram: None,
            sprite_bg: None,
            sram,
            sram_write_enabled: false,
            supervisor_area_limit: 0,
            enforce_supervisor_protection: false,
            rom: vec![0xFF; ROM_WINDOW_BYTES],
            wait_table: WaitStateTable::new(timing),
            access_flags: HardwareAccessFlags::default(),
            crtc: Crtc::new(),
            vicon: Vicon::new(),
            dmac: Dmac::new(),
            mfp: Mfp::new(),
            rtc: Rtc::new(),
            sysport: SystemPort::new(),
            opm: Opm::new(),
            adpcm_active: false,
            fdc: Fdc::new(),
            scc: Scc::new(),
            ppi: Ppi::new(),
            ioi: IoInterruptController::new(),
        }
    }

    /// Updates the CPU and System Bus timing configuration and rebuilds the 4 KB page wait table.
    pub fn set_timing_config(&mut self, timing: BusTimingConfig) {
        self.wait_table.rebuild(timing);
    }

    /// Returns the active [`BusTimingConfig`].
    #[inline]
    pub const fn timing_config(&self) -> BusTimingConfig {
        self.wait_table.config()
    }

    /// Returns a reference to the precomputed [`WaitStateTable`].
    #[inline]
    pub const fn wait_table(&self) -> &WaitStateTable {
        &self.wait_table
    }

    /// Returns a mutable reference to the [`WaitStateTable`] (for custom range overrides).
    #[inline]
    pub fn wait_table_mut(&mut self) -> &mut WaitStateTable {
        &mut self.wait_table
    }

    /// Returns the configured Main RAM size in bytes.
    #[inline]
    pub fn main_ram_len(&self) -> usize {
        self.main_ram.len()
    }

    /// Direct slice access to Main RAM.
    #[inline]
    pub fn main_ram(&self) -> &[u8] {
        &self.main_ram
    }

    /// Direct mutable slice access to Main RAM.
    #[inline]
    pub fn main_ram_mut(&mut self) -> &mut [u8] {
        &mut self.main_ram
    }

    /// Direct slice access to TVRAM (`512 KB`, `$E00000`–`$E7FFFF`) if allocated.
    #[inline]
    pub fn tvram(&self) -> Option<&[u8]> {
        self.tvram.as_deref()
    }

    /// Direct mutable slice access to TVRAM (`512 KB`), allocating it if not yet allocated.
    #[inline]
    pub fn tvram_mut(&mut self) -> &mut [u8] {
        self.tvram
            .get_or_insert_with(|| vec![0u8; TVRAM_BYTES].into_boxed_slice())
    }

    /// Direct slice access to GVRAM (`512 KB`, `$C00000`–`$DFFFFF`) if allocated.
    #[inline]
    pub fn gvram(&self) -> Option<&[u8]> {
        self.gvram.as_deref()
    }

    /// Direct slice access to the 1 MB ROM window (`$F00000`–`$FFFFFF`).
    #[inline]
    pub fn rom(&self) -> &[u8] {
        &self.rom
    }

    /// Direct slice access to the 16 KB battery-backed SRAM (`$ED0000`–`$ED3FFF`).
    #[inline]
    pub fn sram(&self) -> &[u8] {
        &self.sram
    }

    /// Direct mutable slice access to the 16 KB battery-backed SRAM (`$ED0000`–`$ED3FFF`).
    #[inline]
    pub fn sram_mut(&mut self) -> &mut [u8] {
        &mut self.sram
    }

    /// Loads binary data into Main RAM starting at `dest_addr`.
    pub fn load_ram(&mut self, dest_addr: u32, data: &[u8]) -> Result<(), BusFault> {
        let start = (dest_addr & 0x00FF_FFFF) as usize;
        let end = start.saturating_add(data.len());
        if end > self.main_ram.len() {
            return Err(BusFault::BusError {
                addr: dest_addr,
                is_write: true,
            });
        }
        self.main_ram[start..end].copy_from_slice(data);
        Ok(())
    }

    /// Loads the 768 KB CGROM (font ROM) at `$F00000`–`$FBFFFF`.
    pub fn load_cgrom(&mut self, cgrom_data: &[u8]) {
        let len = cgrom_data.len().min(768 * 1024);
        self.rom[..len].copy_from_slice(&cgrom_data[..len]);
    }

    /// Loads IPL ROM image at `$FE0000`–`$FFFFFF` (128 KB) and mirrors initial reset vectors
    /// (`SSP` and `PC` at `$FF0000`–`$FF0007`) to `$000000`–`$000007`.
    pub fn load_ipl_rom(&mut self, ipl_data: &[u8]) {
        let len = ipl_data.len().min(128 * 1024);
        let offset = ROM_WINDOW_BYTES - len;
        self.rom[offset..offset + len].copy_from_slice(&ipl_data[..len]);
        if ipl_data.len() >= 8 && self.main_ram.len() >= 8 {
            self.main_ram[0..8].copy_from_slice(&self.rom[ROM_WINDOW_BYTES - 64 * 1024..][..8]);
        }
    }

    /// Inserts a floppy disk image (`.XDF` / `.2HD` / `.2DD`) into FDD unit `unit` (`0..=3`).
    pub fn insert_floppy(&mut self, unit: usize, data: Vec<u8>, write_protected: bool) -> bool {
        self.fdc.insert_disk(unit, data, write_protected)
    }

    /// Ejects the floppy disk from FDD unit `unit` (`0..=3`).
    pub fn eject_floppy(&mut self, unit: usize) {
        self.fdc.eject_disk(unit);
    }

    /// Writes a byte directly into the ROM window (`$F00000`–`$FFFFFF`) during setup/testing.
    pub fn write_rom_u8(&mut self, addr: u32, val: u8) {
        let idx = ((addr & 0x00FF_FFFF) - 0x00F0_0000) as usize;
        if idx < self.rom.len() {
            self.rom[idx] = val;
        }
    }

    /// Writes a 16-bit big-endian word directly into the ROM window (`$F00000`–`$FFFFFF`).
    pub fn write_rom_u16(&mut self, addr: u32, val: u16) {
        self.write_rom_u8(addr, (val >> 8) as u8);
        self.write_rom_u8(addr.wrapping_add(1), val as u8);
    }

    /// Advances CRTC scanlines, MFP timers, and OPM timers by `cycles_10mhz` (10 MHz clock cycles).
    pub fn advance_peripherals(&mut self, cycles_10mhz: u32) {
        let sync = self
            .crtc
            .advance(cycles_10mhz, &mut self.tvram, &mut self.gvram);
        self.mfp.update_crtc_sync(sync);
        self.opm.advance(cycles_10mhz);
        self.mfp.set_opm_irq(self.opm.irq_active());
        self.mfp.advance(cycles_10mhz);
    }

    /// Returns the highest pending hardware interrupt priority level (`0..=7`) across all peripherals:
    /// - Level 6: MFP (`MC68901`)
    /// - Level 3: DMAC (`HD63450`)
    /// - Level 1: I/O Interrupt Controller (`FDC`, `FDD`, `HDC`, `PRN`)
    #[inline]
    pub fn highest_pending_ipl(&self) -> u8 {
        if self.mfp.has_pending_interrupt() {
            6
        } else if self.dmac.has_pending_interrupt() {
            3
        } else if self.ioi.has_pending_interrupt() {
            1
        } else {
            0
        }
    }

    /// Returns the accumulated [`HardwareAccessFlags`].
    #[inline]
    pub const fn access_flags(&self) -> HardwareAccessFlags {
        self.access_flags
    }

    /// Clears the accumulated [`HardwareAccessFlags`].
    #[inline]
    pub fn clear_access_flags(&mut self) {
        self.access_flags = HardwareAccessFlags::default();
    }

    /// Returns `true` if GVRAM has been lazily allocated.
    #[inline]
    pub const fn is_gvram_allocated(&self) -> bool {
        self.gvram.is_some()
    }

    /// Returns `true` if TVRAM has been lazily allocated.
    #[inline]
    pub const fn is_tvram_allocated(&self) -> bool {
        self.tvram.is_some()
    }

    #[inline]
    fn record_region_access(&mut self, region: MemoryRegion) {
        match region {
            MemoryRegion::Gvram => self.access_flags.gvram_accessed = true,
            MemoryRegion::Tvram => self.access_flags.tvram_accessed = true,
            MemoryRegion::Crtc => self.access_flags.crtc_accessed = true,
            MemoryRegion::PaletteVicon => self.access_flags.palette_vicon_accessed = true,
            MemoryRegion::SpriteBg => self.access_flags.sprite_bg_accessed = true,
            MemoryRegion::Opm | MemoryRegion::Adpcm => self.access_flags.sound_io_accessed = true,
            _ => {}
        }
    }
}

impl Bus for X68kBus {
    #[inline(always)]
    fn wait_cycles_16(&mut self, addr: u32, _is_write: bool) -> u32 {
        self.wait_table.wait_for_addr(addr)
    }

    fn reset_peripherals(&mut self) {
        self.dmac.reset();
        self.mfp.reset();
        self.opm.reset();
        self.fdc.reset(&mut self.ioi);
    }

    fn acknowledge_interrupt(&mut self, level: u8) -> Option<u8> {
        match level {
            6 => self.mfp.acknowledge_interrupt(),
            3 => self.dmac.acknowledge_interrupt(),
            1 => self.ioi.acknowledge_interrupt(),
            _ => None,
        }
    }

    fn read_u8(&mut self, addr: u32, supervisor: bool) -> Result<u8, BusFault> {
        let a = addr & 0x00FF_FFFF;
        let region = MemoryRegion::from_addr(a);
        self.record_region_access(region);

        match region {
            MemoryRegion::MainRam => {
                if self.enforce_supervisor_protection
                    && !supervisor
                    && a < self.supervisor_area_limit
                {
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    });
                }
                let idx = a as usize;
                self.main_ram.get(idx).copied().ok_or(BusFault::BusError {
                    addr: a,
                    is_write: false,
                })
            }
            MemoryRegion::Gvram => {
                let offset = (a as usize) & (GVRAM_BYTES - 1);
                Ok(self.gvram.as_ref().map_or(0, |buf| buf[offset]))
            }
            MemoryRegion::Tvram => {
                let offset = (a as usize) & (TVRAM_BYTES - 1);
                Ok(self.tvram.as_ref().map_or(0, |buf| buf[offset]))
            }
            MemoryRegion::Crtc => self.crtc.read_u8(a),
            MemoryRegion::PaletteVicon => Ok(self.vicon.read_u8(a)),
            MemoryRegion::Dmac => self.dmac.read_u8(a),
            MemoryRegion::AreaSet => Err(BusFault::BusError {
                addr: a,
                is_write: false,
            }),
            MemoryRegion::Mfp => {
                if (a & 0x3F) == 0x01 && !self.wait_table.config().wait_enabled {
                    // When polling GPIP in zero-wait Burst mode, advance virtual scanline ticks
                    self.advance_peripherals(16);
                }
                self.mfp.read_u8(a)
            }
            MemoryRegion::Rtc => self.rtc.read_u8(a),
            MemoryRegion::Printer => Ok(0xFF),
            MemoryRegion::SysPort => {
                if (a & 1) == 0 {
                    Ok(0xFF)
                } else {
                    Ok(self.sysport.read_u8(a, self.wait_table.config().cpu_mhz))
                }
            }
            MemoryRegion::Opm => self.opm.read_u8(a),
            MemoryRegion::Adpcm => {
                if (a & 1) == 0 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    })
                } else if (a & 3) == 1 {
                    Ok(if self.adpcm_active { 0xC0 } else { 0x40 })
                } else {
                    Ok(0xFF)
                }
            }
            MemoryRegion::Fdc => self.fdc.read_u8(a, &mut self.ioi),
            MemoryRegion::Hdc => {
                if (a & 1) == 0 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    })
                } else {
                    // SASI status ($E96003) = 0x00 (no SASI drive busy/requesting)
                    Ok(0x00)
                }
            }
            MemoryRegion::Scc => self.scc.read_u8(a),
            MemoryRegion::Ppi => self.ppi.read_u8(a),
            MemoryRegion::Ioi => {
                if a >= 0x00E9_E000 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    })
                } else {
                    self.ioi.read_u8(a)
                }
            }
            MemoryRegion::SpriteBg => {
                let offset = (a as usize) & (SPRITE_WINDOW_BYTES - 1);
                if (0x0400..0x0800).contains(&offset) || (0x0812..0x8000).contains(&offset) {
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    });
                }
                Ok(self.sprite_bg.as_ref().map_or(0, |buf| buf[offset]))
            }
            MemoryRegion::Sram => {
                let offset = (a - 0x00ED_0000) as usize;
                self.sram.get(offset).copied().ok_or(BusFault::BusError {
                    addr: a,
                    is_write: false,
                })
            }
            MemoryRegion::Rom => {
                let offset = (a - 0x00F0_0000) as usize;
                if (0x00FC_0000..0x00FE_0000).contains(&a) {
                    // Unpopulated SCSI ROM window ($FC0000-$FDFFFF) raises Bus Error on standard EXPERT
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    });
                }
                Ok(self.rom[offset])
            }
            MemoryRegion::ExpansionIo => Err(BusFault::BusError {
                addr: a,
                is_write: false,
            }),
        }
    }

    fn read_u16(&mut self, addr: u32, supervisor: bool) -> Result<u16, BusFault> {
        let a = addr & 0x00FF_FFFF;
        // Fast path for Main RAM aligned 16-bit read
        if a < 0x00C0_0000 {
            if self.enforce_supervisor_protection && !supervisor && a < self.supervisor_area_limit {
                return Err(BusFault::BusError {
                    addr: a,
                    is_write: false,
                });
            }
            let idx = a as usize;
            if idx + 1 < self.main_ram.len() {
                let hi = self.main_ram[idx] as u16;
                let lo = self.main_ram[idx + 1] as u16;
                return Ok((hi << 8) | lo);
            }
            return Err(BusFault::BusError {
                addr: a,
                is_write: false,
            });
        }
        // 8-bit odd-byte-mapped peripherals ($E88000..$E9DFFF):
        // 16-bit word read activates LDS and reads only the lower byte (a | 1) with upper byte $FF.
        if (0x00E8_8000..0x00E9_E000).contains(&a) {
            let lo = self.read_u8(a | 1, supervisor)? as u16;
            return Ok(0xFF00 | lo);
        }
        let hi = self.read_u8(a, supervisor)? as u16;
        let lo = self.read_u8(a.wrapping_add(1), supervisor)? as u16;
        Ok((hi << 8) | lo)
    }

    fn write_u8(&mut self, addr: u32, val: u8, supervisor: bool) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        let region = MemoryRegion::from_addr(a);
        self.record_region_access(region);

        match region {
            MemoryRegion::MainRam => {
                if self.enforce_supervisor_protection
                    && !supervisor
                    && a < self.supervisor_area_limit
                {
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    });
                }
                let idx = a as usize;
                let slot = self.main_ram.get_mut(idx).ok_or(BusFault::BusError {
                    addr: a,
                    is_write: true,
                })?;
                *slot = val;
                Ok(())
            }
            MemoryRegion::Gvram => {
                let buf = self
                    .gvram
                    .get_or_insert_with(|| vec![0u8; GVRAM_BYTES].into_boxed_slice());
                buf[(a as usize) & (GVRAM_BYTES - 1)] = val;
                Ok(())
            }
            MemoryRegion::Tvram => {
                let buf = self
                    .tvram
                    .get_or_insert_with(|| vec![0u8; TVRAM_BYTES].into_boxed_slice());
                self.crtc.write_tvram_u8(buf, a, val);
                Ok(())
            }
            MemoryRegion::Crtc => {
                self.crtc
                    .write_u8(a, val, &mut self.tvram, &mut self.gvram)
            }
            MemoryRegion::PaletteVicon => {
                self.vicon.write_u8(a, val);
                Ok(())
            }
            MemoryRegion::Dmac => self.dmac.write_u8(a, val, &mut self.main_ram),
            MemoryRegion::AreaSet => {
                if (a & 1) == 0 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    })
                } else {
                    self.supervisor_area_limit = ((val as u32) + 1) * 8192;
                    Ok(())
                }
            }
            MemoryRegion::Mfp => self.mfp.write_u8(a, val),
            MemoryRegion::Rtc => self.rtc.write_u8(a, val),
            MemoryRegion::Printer => Ok(()),
            MemoryRegion::SysPort => {
                if (a & 1) == 1 {
                    match a & 0x0F {
                        0x01 => self.sysport.contrast = val & 0x0F,
                        0x03 => self.sysport.tv_ctrl = val,
                        0x05 => self.sysport.image_unit = val,
                        0x07 => self.sysport.hrl = (val >> 1) & 0x01,
                        0x09 => self.sysport.wait_ctrl = val,
                        0x0D => self.sram_write_enabled = val == 0x31,
                        0x0F => self.sysport.power_ctrl = val & 0x0F,
                        _ => {}
                    }
                }
                Ok(())
            }
            MemoryRegion::Opm => {
                self.opm.write_u8(a, val)?;
                self.fdc.enforced_ready = self.opm.ct2_fdc_enforced_ready();
                self.mfp.set_opm_irq(self.opm.irq_active());
                Ok(())
            }
            MemoryRegion::Adpcm => {
                if (a & 1) == 0 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    })
                } else {
                    if (a & 3) == 1 {
                        if (val & 0x01) != 0 {
                            self.adpcm_active = false;
                        } else if (val & 0x02) != 0 {
                            self.adpcm_active = true;
                        }
                    }
                    Ok(())
                }
            }
            MemoryRegion::Fdc => {
                self.fdc
                    .write_u8(a, val, &mut self.dmac, &mut self.main_ram, &mut self.ioi)
            }
            MemoryRegion::Hdc => {
                if (a & 1) == 0 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    })
                } else {
                    Ok(())
                }
            }
            MemoryRegion::Scc => self.scc.write_u8(a, val),
            MemoryRegion::Ppi => self.ppi.write_u8(a, val),
            MemoryRegion::Ioi => {
                if a >= 0x00E9_E000 {
                    Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    })
                } else {
                    self.ioi.write_u8(a, val)
                }
            }
            MemoryRegion::SpriteBg => {
                let offset = (a as usize) & (SPRITE_WINDOW_BYTES - 1);
                if (0x0400..0x0800).contains(&offset) || (0x0812..0x8000).contains(&offset) {
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    });
                }
                let buf = self
                    .sprite_bg
                    .get_or_insert_with(|| vec![0u8; SPRITE_WINDOW_BYTES].into_boxed_slice());
                buf[offset] = val;
                Ok(())
            }
            MemoryRegion::Sram => {
                let offset = (a - 0x00ED_0000) as usize;
                if offset >= self.sram.len() {
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    });
                }
                if self.sram_write_enabled {
                    self.sram[offset] = val;
                }
                Ok(())
            }
            MemoryRegion::Rom => {
                if (0x00FC_0000..0x00FE_0000).contains(&a) {
                    return Err(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    });
                }
                // Writes to populated ROM are ignored on X68000 hardware.
                Ok(())
            }
            MemoryRegion::ExpansionIo => Err(BusFault::BusError {
                addr: a,
                is_write: true,
            }),
        }
    }

    fn write_u16(&mut self, addr: u32, val: u16, supervisor: bool) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        if a < 0x00C0_0000 {
            if self.enforce_supervisor_protection && !supervisor && a < self.supervisor_area_limit {
                return Err(BusFault::BusError {
                    addr: a,
                    is_write: true,
                });
            }
            let idx = a as usize;
            if idx + 1 < self.main_ram.len() {
                self.main_ram[idx] = (val >> 8) as u8;
                self.main_ram[idx + 1] = val as u8;
                return Ok(());
            }
            return Err(BusFault::BusError {
                addr: a,
                is_write: true,
            });
        }
        if (0x00E0_0000..0x00E8_0000).contains(&a) {
            self.access_flags.tvram_accessed = true;
            let buf = self
                .tvram
                .get_or_insert_with(|| vec![0u8; TVRAM_BYTES].into_boxed_slice());
            self.crtc.write_tvram_u16(buf, a, val);
            return Ok(());
        }
        // 8-bit odd-byte-mapped peripherals ($E86000..$E9DFFF):
        // 16-bit word write activates LDS and writes the low byte to (a | 1).
        if (0x00E8_6000..0x00E9_E000).contains(&a) {
            return self.write_u8(a | 1, val as u8, supervisor);
        }
        self.write_u8(a, (val >> 8) as u8, supervisor)?;
        self.write_u8(a.wrapping_add(1), val as u8, supervisor)
    }
}
