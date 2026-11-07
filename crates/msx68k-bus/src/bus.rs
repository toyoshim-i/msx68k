//! X68000 24-bit system bus implementation with lazy on-demand VRAM/I/O allocation
//! and region-aware wait-state lookup.

use msx68k_cpu::{Bus, BusFault};

use crate::timing::{BusTimingConfig, MemoryRegion, WaitStateTable};

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

/// X68000 24-bit system bus with configurable CPU/System Bus wait states and on-demand allocation.
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
    /// Lazily allocated core I/O space (`128 KB`, `$E80000`–`$E9FFFF`).
    io_space: Option<Box<[u8]>>,
    /// Battery-backed SRAM (`$ED0000`, typically 16 KB).
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
}

impl X68kBus {
    /// Creates a new X68000 bus with `ram_size_bytes` (clamped to `12 MB`) and `timing` configuration.
    ///
    /// Video RAM and peripheral I/O buffers start as `None` (zero allocation overhead)
    /// and are instantiated on demand when first written.
    pub fn new(ram_size_bytes: usize, timing: BusTimingConfig) -> Self {
        let ram_bytes = ram_size_bytes.min(MAX_MAIN_RAM_BYTES);
        Self {
            main_ram: vec![0; ram_bytes],
            gvram: None,
            tvram: None,
            sprite_bg: None,
            io_space: None,
            sram: vec![0; 16 * 1024],
            sram_write_enabled: false,
            supervisor_area_limit: 0,
            enforce_supervisor_protection: false,
            rom: vec![0; ROM_WINDOW_BYTES],
            wait_table: WaitStateTable::new(timing),
            access_flags: HardwareAccessFlags::default(),
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

    /// Loads IPL ROM image at `$FE0000`–`$FFFFFF` (128 KB) and mirrors initial reset vectors
    /// (`SSP` and `PC`) to `$000000`–`$000007`.
    pub fn load_ipl_rom(&mut self, ipl_data: &[u8]) {
        let len = ipl_data.len().min(128 * 1024);
        let offset = ROM_WINDOW_BYTES - len;
        self.rom[offset..offset + len].copy_from_slice(&ipl_data[..len]);
        if ipl_data.len() >= 8 && self.main_ram.len() >= 8 {
            self.main_ram[0..8].copy_from_slice(&self.rom[ROM_WINDOW_BYTES - 64 * 1024..][..8]);
        }
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
            MemoryRegion::SpriteBg => {
                let offset = (a as usize) & (SPRITE_WINDOW_BYTES - 1);
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
                Ok(self.rom[offset])
            }
            MemoryRegion::ExpansionIo => {
                // Unpopulated expansion I/O raises a Bus Error so Human68k/IPL can probe boards.
                Err(BusFault::BusError {
                    addr: a,
                    is_write: false,
                })
            }
            _ => {
                let offset = (a - 0x00E8_0000) as usize;
                if let Some(io) = &self.io_space {
                    Ok(io[offset & (IO_WINDOW_BYTES - 1)])
                } else {
                    Ok(0)
                }
            }
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
                buf[(a as usize) & (TVRAM_BYTES - 1)] = val;
                Ok(())
            }
            MemoryRegion::SpriteBg => {
                let buf = self
                    .sprite_bg
                    .get_or_insert_with(|| vec![0u8; SPRITE_WINDOW_BYTES].into_boxed_slice());
                buf[(a as usize) & (SPRITE_WINDOW_BYTES - 1)] = val;
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
                // Writes to ROM are ignored on X68000 hardware.
                Ok(())
            }
            MemoryRegion::ExpansionIo => Err(BusFault::BusError {
                addr: a,
                is_write: true,
            }),
            _ => {
                if a == 0x00E8_E00D {
                    self.sram_write_enabled = val == 0x31;
                } else if a == 0x00E8_6001 {
                    self.supervisor_area_limit = ((val as u32) + 1) * 8192;
                }
                let offset = (a - 0x00E8_0000) as usize;
                let buf = self
                    .io_space
                    .get_or_insert_with(|| vec![0u8; IO_WINDOW_BYTES].into_boxed_slice());
                buf[offset & (IO_WINDOW_BYTES - 1)] = val;
                Ok(())
            }
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
        self.write_u8(a, (val >> 8) as u8, supervisor)?;
        self.write_u8(a.wrapping_add(1), val as u8, supervisor)
    }
}
