//! X68000 CPU clock modes, System Bus frequency configuration, and per-region memory wait-state model.

/// Number of 4 KB pages in the 24-bit (`16 MB`) MC68000 address space.
pub const PAGE_COUNT_4K: usize = 4096;
/// Shift amount to convert a 24-bit address to a 4 KB page index.
pub const PAGE_SHIFT_4K: u32 = 12;

/// Role of a virtual machine instance within the hybrid environment.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VmRole {
    /// Master SX-Window environment (defaults to [`CpuSpeedMode::Burst`]).
    MasterSx,
    /// Isolated application VM (defaults to [`CpuSpeedMode::Mhz10`], selectable per application).
    AppVm,
}

impl VmRole {
    /// Returns the default [`CpuSpeedMode`] for this VM role.
    #[inline]
    pub const fn default_speed_mode(self) -> CpuSpeedMode {
        match self {
            Self::MasterSx => CpuSpeedMode::Burst,
            Self::AppVm => CpuSpeedMode::Mhz10,
        }
    }
}

/// CPU and System Bus speed presets selectable per VM or application profile.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CpuSpeedMode {
    /// Original X68000 / ACE / PRO / SUPER: 10 MHz CPU, 10 MHz System Bus.
    Mhz10,
    /// X68000 XVI: 16 MHz CPU, 16 MHz System Bus.
    Mhz16,
    /// X68000 XVI RedZone: 24 MHz CPU, 16 MHz System Bus.
    Mhz24,
    /// Uncapped host speed with zero system-bus or memory wait states.
    Burst,
    /// Custom CPU and System Bus clock configuration.
    Custom(BusTimingConfig),
}

impl CpuSpeedMode {
    /// Converts the preset into a concrete [`BusTimingConfig`].
    #[inline]
    pub const fn timing_config(self) -> BusTimingConfig {
        match self {
            Self::Mhz10 => BusTimingConfig::mhz10(),
            Self::Mhz16 => BusTimingConfig::mhz16_xvi(),
            Self::Mhz24 => BusTimingConfig::mhz24_redzone(),
            Self::Burst => BusTimingConfig::burst(),
            Self::Custom(cfg) => cfg,
        }
    }

    /// Returns `true` if this mode executes with zero cycle/wait accounting (`Burst`).
    #[inline]
    pub const fn is_burst(self) -> bool {
        !self.timing_config().wait_enabled
    }
}

/// Concrete CPU clock and System Bus timing configuration.
///
/// Models the interaction between:
/// 1. The **CPU clock** (`cpu_mhz`: 10 MHz, 16 MHz, 24 MHz, or uncapped),
/// 2. The **System Bus clock** (`system_bus_mhz`: 10 MHz on original X68000,
///    16 MHz on XVI / RedZone, or 0 delay in Burst), and
/// 3. Each memory/device region's inherent response characteristics.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BusTimingConfig {
    /// CPU clock frequency in MHz (`0` when uncapped in Burst mode).
    pub cpu_mhz: u32,
    /// System bus clock frequency in MHz (`10` for 10MHz, `16` for 16/24MHz, `0` for Burst).
    pub system_bus_mhz: u32,
    /// Whether cycle and bus wait-state accounting is enabled.
    pub wait_enabled: bool,
    /// Additional 10 MHz video-bus contention waits on GVRAM/TVRAM during active display scanlines.
    pub vram_display_contention: bool,
}

impl BusTimingConfig {
    /// 10 MHz CPU / 10 MHz System Bus (Original X68000 / ACE / PRO / SUPER).
    pub const fn mhz10() -> Self {
        Self {
            cpu_mhz: 10,
            system_bus_mhz: 10,
            wait_enabled: true,
            vram_display_contention: false,
        }
    }

    /// 16 MHz CPU / 16 MHz System Bus (X68000 XVI).
    pub const fn mhz16_xvi() -> Self {
        Self {
            cpu_mhz: 16,
            system_bus_mhz: 16,
            wait_enabled: true,
            vram_display_contention: false,
        }
    }

    /// 24 MHz CPU / 16 MHz System Bus (X68000 XVI RedZone).
    pub const fn mhz24_redzone() -> Self {
        Self {
            cpu_mhz: 24,
            system_bus_mhz: 16,
            wait_enabled: true,
            vram_display_contention: false,
        }
    }

    /// Burst mode: uncapped CPU and zero system-bus delay.
    pub const fn burst() -> Self {
        Self {
            cpu_mhz: 0,
            system_bus_mhz: 0,
            wait_enabled: false,
            vram_display_contention: false,
        }
    }

    /// Creates a custom CPU / System Bus frequency configuration.
    pub const fn custom(cpu_mhz: u32, system_bus_mhz: u32, wait_enabled: bool) -> Self {
        Self {
            cpu_mhz,
            system_bus_mhz,
            wait_enabled,
            vram_display_contention: false,
        }
    }

    /// Returns a copy with VICON active-display VRAM contention toggled.
    pub const fn with_vram_contention(mut self, active: bool) -> Self {
        self.vram_display_contention = active;
        self
    }

    /// Computes the extra CPU wait cycles (above the standard 4 CPU cycles per 16-bit bus transfer)
    /// for a region given this CPU and System Bus clock configuration.
    ///
    /// ### Physical Timing Model
    /// - A standard zero-wait MC68000 bus cycle takes `4` clocks on the System Bus (`system_bus_mhz`),
    ///   plus `sys_bus_waits` wait states synchronous to the System Bus (e.g., `0` for Main RAM,
    ///   `1` for ROM/SRAM/SysPort).
    /// - Regions bridged to the 10 MHz video/peripheral subsystem (GVRAM, TVRAM, Palette, CRTC,
    ///   OPM, ADPCM, MFP, SCC, DMAC, etc.) additionally incur `sub_bus_10mhz_waits` cycles in the
    ///   10 MHz domain (`100 ns` units).
    /// - Converting total transaction duration to `cpu_mhz` cycles yields:
    ///   $$\text{Total CPU Cycles} = \left\lceil (4 + W_{\text{sys}}) \times \frac{f_{\text{cpu}}}{f_{\text{bus}}} + W_{10\text{MHz}} \times \frac{f_{\text{cpu}}}{10} \right\rceil$$
    ///   and $\text{Wait Cycles} = \max(0, \text{Total CPU Cycles} - 4)$.
    #[inline]
    pub const fn compute_region_wait(self, region: MemoryRegion) -> u8 {
        if !self.wait_enabled || self.cpu_mhz == 0 || self.system_bus_mhz == 0 {
            return 0;
        }

        let spec = region.timing_spec(self.vram_display_contention);
        let sys_cycles = 4 + (spec.sys_bus_waits as u64);
        let sub_10mhz_cycles = spec.sub_bus_10mhz_waits as u64;

        let cpu = self.cpu_mhz as u64;
        let bus = self.system_bus_mhz as u64;

        // Compute ceiling of:
        //   sys_cycles * (cpu / bus) + sub_10mhz_cycles * (cpu / 10)
        // using common denominator (bus * 10):
        let denom = bus * 10;
        let numer = sys_cycles * cpu * 10 + sub_10mhz_cycles * cpu * bus;
        let total_cpu_cycles = numer.div_ceil(denom);

        if total_cpu_cycles > 4 {
            let wait = total_cpu_cycles - 4;
            if wait > 255 {
                255
            } else {
                wait as u8
            }
        } else {
            0
        }
    }
}

/// Physical bus response parameters for a memory-mapped region.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct RegionTimingSpec {
    /// Wait cycles synchronous to the System Bus (10 MHz on original, 16 MHz on XVI/RedZone).
    pub sys_bus_waits: u8,
    /// Wait cycles synchronous to the 10 MHz video/peripheral sub-bus.
    pub sub_bus_10mhz_waits: u8,
}

/// Address regions in the 16 MB (`$000000`–`$FFFFFF`) X68000 memory map.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum MemoryRegion {
    /// `$000000`–`$BFFFFF`: Main RAM (up to 12 MB).
    MainRam,
    /// `$C00000`–`$DFFFFF`: Graphic VRAM (GVRAM, 512 KB mapped across 2 MB window).
    Gvram,
    /// `$E00000`–`$E7FFFF`: Text VRAM (TVRAM, 512 KB, 4 planes).
    Tvram,
    /// `$E80000`–`$E81FFF`: CRTC registers and raster copy control.
    Crtc,
    /// `$E82000`–`$E83FFF`: Palette RAM and VICON priority/mode control.
    PaletteVicon,
    /// `$E84000`–`$E85FFF`: HD63450 DMAC (4 channels).
    Dmac,
    /// `$E86000`–`$E87FFF`: Supervisor Area Set register.
    AreaSet,
    /// `$E88000`–`$E89FFF`: MC68901 MFP (timers, USART, GPIP interrupts).
    Mfp,
    /// `$E8A000`–`$E8BFFF`: RP5C15 Real-Time Clock.
    Rtc,
    /// `$E8C000`–`$E8DFFF`: Printer port.
    Printer,
    /// `$E8E000`–`$E8FFFF`: System port (contrast, TV control, wait control, HRL, etc.).
    SysPort,
    /// `$E90000`–`$E91FFF`: YM2151 OPM FM sound generator.
    Opm,
    /// `$E92000`–`$E93FFF`: MSM6258V ADPCM voice synthesizer.
    Adpcm,
    /// `$E94000`–`$E95FFF`: uPD72065 Floppy Disk Controller & drive select.
    Fdc,
    /// `$E96000`–`$E97FFF`: SASI / Internal SCSI Hard Disk Controller.
    Hdc,
    /// `$E98000`–`$E99FFF`: Z8530 SCC serial & mouse controller.
    Scc,
    /// `$E9A000`–`$E9BFFF`: i8255 PPI joystick port.
    Ppi,
    /// `$E9C000`–`$E9FFFF`: I/O Interrupt controller.
    Ioi,
    /// `$EA0000`–`$EAFFFF` & `$EC0000`–`$ECFFFF`: Expansion I/O (SCSI ROM/SPC, MIDI, etc.).
    ExpansionIo,
    /// `$EB0000`–`$EBFFFF`: CYNTHIA Sprite / PCG / BG registers and RAM.
    SpriteBg,
    /// `$ED0000`–`$EDFFFF`: Battery-backed SRAM (16–64 KB) + unmapped `$EE0000`–`$EFFFFF`.
    Sram,
    /// `$F00000`–`$FFFFFF`: CGROM (Font ROM) and IPLROM.
    Rom,
}

impl MemoryRegion {
    /// Classifies any 24-bit physical address into its [`MemoryRegion`].
    #[inline]
    pub const fn from_addr(addr: u32) -> Self {
        let a = addr & 0x00FF_FFFF;
        match a >> 16 {
            0x00..=0xBF => Self::MainRam,
            0xC0..=0xDF => Self::Gvram,
            0xE0..=0xE7 => Self::Tvram,
            0xE8 => match (a >> 13) & 7 {
                0 => Self::Crtc,
                1 => Self::PaletteVicon,
                2 => Self::Dmac,
                3 => Self::AreaSet,
                4 => Self::Mfp,
                5 => Self::Rtc,
                6 => Self::Printer,
                _ => Self::SysPort,
            },
            0xE9 => match (a >> 13) & 7 {
                0 => Self::Opm,
                1 => Self::Adpcm,
                2 => Self::Fdc,
                3 => Self::Hdc,
                4 => Self::Scc,
                5 => Self::Ppi,
                _ => Self::Ioi,
            },
            0xEA | 0xEC => Self::ExpansionIo,
            0xEB => Self::SpriteBg,
            0xED..=0xEF => Self::Sram,
            0xF0..=0xFF => Self::Rom,
            _ => unreachable!(),
        }
    }

    /// Returns the physical wait-state specification for this region.
    #[inline]
    pub const fn timing_spec(self, vram_display_contention: bool) -> RegionTimingSpec {
        let extra_vram = if vram_display_contention { 1 } else { 0 };
        match self {
            Self::MainRam => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 0,
            },
            Self::Gvram => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 1 + extra_vram,
            },
            Self::Tvram => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 2 + extra_vram,
            },
            Self::Crtc | Self::SpriteBg => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 1,
            },
            Self::PaletteVicon => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 3,
            },
            Self::Dmac => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 15,
            },
            Self::Mfp => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 4,
            },
            Self::Scc => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 6,
            },
            Self::Rtc
            | Self::Opm
            | Self::Adpcm
            | Self::Fdc
            | Self::Hdc
            | Self::Ppi
            | Self::Ioi
            | Self::ExpansionIo => RegionTimingSpec {
                sys_bus_waits: 0,
                sub_bus_10mhz_waits: 2,
            },
            Self::AreaSet | Self::Printer | Self::SysPort | Self::Sram | Self::Rom => {
                RegionTimingSpec {
                    sys_bus_waits: 1,
                    sub_bus_10mhz_waits: 0,
                }
            }
        }
    }
}

/// Precomputed 4 KB page wait-state lookup table for the entire 16 MB address space.
#[derive(Clone)]
pub struct WaitStateTable {
    config: BusTimingConfig,
    /// Extra CPU wait cycles per 16-bit bus access indexed by `(addr & 0xFF_FFFF) >> 12`.
    pages: [u8; PAGE_COUNT_4K],
}

impl std::fmt::Debug for WaitStateTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitStateTable")
            .field("config", &self.config)
            .finish()
    }
}

impl WaitStateTable {
    /// Builds a 4 KB page wait-state table for the given [`BusTimingConfig`].
    pub fn new(config: BusTimingConfig) -> Self {
        let mut table = Self {
            config,
            pages: [0; PAGE_COUNT_4K],
        };
        table.rebuild(config);
        table
    }

    /// Recomputes all 4096 page entries for a new [`BusTimingConfig`].
    pub fn rebuild(&mut self, config: BusTimingConfig) {
        self.config = config;
        if !config.wait_enabled {
            self.pages.fill(0);
            return;
        }
        for (page_idx, slot) in self.pages.iter_mut().enumerate() {
            let addr = (page_idx as u32) << PAGE_SHIFT_4K;
            let region = MemoryRegion::from_addr(addr);
            *slot = config.compute_region_wait(region);
        }
    }

    /// Overrides the wait cycles for a specific address range `[start_addr..=end_addr]`.
    pub fn override_range(&mut self, start_addr: u32, end_addr: u32, wait_cycles: u8) {
        let start_page = ((start_addr & 0x00FF_FFFF) >> PAGE_SHIFT_4K) as usize;
        let end_page = ((end_addr & 0x00FF_FFFF) >> PAGE_SHIFT_4K) as usize;
        for slot in &mut self.pages[start_page..=end_page.min(PAGE_COUNT_4K - 1)] {
            *slot = wait_cycles;
        }
    }

    /// Returns the active [`BusTimingConfig`].
    #[inline]
    pub const fn config(&self) -> BusTimingConfig {
        self.config
    }

    /// Fast O(1) lookup of extra CPU wait cycles for a 16-bit bus access at `addr`.
    #[inline(always)]
    pub const fn wait_for_addr(&self, addr: u32) -> u32 {
        let page = ((addr & 0x00FF_FFFF) >> PAGE_SHIFT_4K) as usize;
        self.pages[page] as u32
    }
}
