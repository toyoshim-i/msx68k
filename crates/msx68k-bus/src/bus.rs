//! X68000 24-bit system bus implementation with lazy on-demand VRAM allocation,
//! region-aware wait-state lookup, and full on-board peripheral emulation
//! (CRTC, VICON/Palette, DMAC, MFP, RTC, System Port, OPM, ADPCM, FDC/FDD, SASI HDC, SCC, PPI, IOI).

use msx68k_cpu::{Bus, BusCycle030, BusFault, CpuModel};

use crate::crtc::{Crtc, TVRAM_PLANE_BYTES};
use crate::dmac::Dmac;
use crate::fdc::{Fdc, IoInterruptController};
use crate::mfp::Mfp;
use crate::peripherals::{Opm, Ppi, Rtc, Scc, SystemPort};
use crate::timing::{BusTimingConfig, MemoryRegion, WaitStateTable};
use crate::vicon::Vicon;
use crate::vram_pages::DirtyVramPages;

/// Maximum Main RAM size on X68000 (`12 MB`, `$000000`–`$BFFFFF`).
pub const MAX_MAIN_RAM_BYTES: usize = 12 * 1024 * 1024;
/// GVRAM physical capacity (`512 KB`).
pub const GVRAM_BYTES: usize = 512 * 1024;
/// TVRAM physical capacity (`512 KB`).
pub const TVRAM_BYTES: usize = 512 * 1024;
/// Main RAM protected from user mode until the guest writes the area set register.
const RESET_SUPERVISOR_AREA: u32 = 0x2000;
/// Everything from here up (VRAM, I/O, SRAM, and ROM) is reachable only in supervisor mode.
const SUPERVISOR_ONLY_BASE: u32 = 0x00C0_0000;
/// Start of the address range reserved for msx68k extensions, `$F000_0000`..=`$FEFF_FFFF`.
///
/// Real machines leave it to motherboard mirrors: it lies above the X68030 and Xellent30 high
/// memory at `$0100_0000` and the 060turbo high memory at `$1000_0000`, and below the
/// `$FFxx_xxxx` block that sign-extended absolute short addresses reach. Addresses above 24 bits
/// that no extension uses mirror the 24-bit space as on the 68000.
pub const MSX68K_RESERVED_BASE: u32 = 0xF000_0000;
/// End (exclusive) of the address range reserved for msx68k extensions.
pub const MSX68K_RESERVED_END: u32 = 0xFF00_0000;
/// Base address that screen `Bitmap` records hold for resolutions beyond 1024×1024, the first
/// block of the reserved range.
pub const EXT_TVRAM_BASE: u32 = MSX68K_RESERVED_BASE;
/// Size of the block at [`EXT_TVRAM_BASE`] that belongs to the extended screen.
pub const EXT_TVRAM_WINDOW: u32 = 0x0100_0000;

/// Whether `addr` falls in the range reserved for msx68k extensions.
#[inline]
pub fn is_reserved_addr(addr: u32) -> bool {
    addr.wrapping_sub(MSX68K_RESERVED_BASE) < MSX68K_RESERVED_END - MSX68K_RESERVED_BASE
}

/// Whether `addr` falls in the extended screen block at [`EXT_TVRAM_BASE`].
#[inline]
pub fn is_ext_screen_addr(addr: u32) -> bool {
    addr.wrapping_sub(EXT_TVRAM_BASE) < EXT_TVRAM_WINDOW
}
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
/// kept in the Primary SX-Window VM (only DOS/text or sound I/O touched).
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
    pub(crate) main_ram: Vec<u8>,
    /// Lazily allocated GVRAM (`512 KB`).
    pub(crate) gvram: Option<Box<[u8]>>,
    /// Lazily allocated TVRAM (`512 KB`, `$E00000`–`$E7FFFF`).
    pub(crate) tvram: Option<Box<[u8]>>,
    /// Addresses where a hypercall has been installed (see [`crate::hypercall`]).
    pub(crate) hypercall_sites: std::collections::HashMap<u32, crate::hypercall::HypercallId>,
    /// VMs that VMX68K.SX has opened (see [`crate::vmx68k`]).
    pub(crate) vm_windows: crate::vmx68k::VmWindows,
    /// A VM's remote drives in shared mode (see [`crate::remote_fs`]), and whether their
    /// boot ROM is mapped in the SCSI ROM window.
    pub(crate) remote_fs: Option<Box<crate::remote_fs::RemoteFs>>,
    pub(crate) remote_boot_rom: bool,
    /// Whether the application entry is mapped at the end of the SCSI ROM window.
    pub(crate) app_entry_installed: bool,
    /// TVRAM and GVRAM pages written since the last [`Self::take_written_vram_pages`].
    vram_written: DirtyVramPages,
    raster: crate::raster::RasterCapture,
    /// Lazily allocated Sprite / PCG / BG space (`64 KB`, `$EB0000`–`$EBFFFF`).
    sprite_bg: Option<Box<[u8]>>,
    /// Independent extra 32 KiB pattern/map bank; retained while disabled.
    sprite_bg_extra: Option<Box<[u8]>>,
    /// Live scanout options, preserved across soft resets.
    pub sprite_options: crate::sprite::SpriteOptions,
    /// Battery-backed SRAM (`$ED0000`–`$ED3FFF`, 16 KB).
    pub(crate) sram: Vec<u8>,
    /// Write-unlock latch for SRAM (`$E8E00D == 0x31`).
    sram_write_enabled: bool,
    /// Whether battery-backed SRAM has been modified since last persistence check.
    pub(crate) sram_dirty: bool,
    /// Supervisor Area Set boundary (`($E86001 + 1) * 8192`).
    supervisor_area_limit: u32,
    /// Font & IPL ROM (`1 MB`, `$F00000`–`$FFFFFF`).
    pub(crate) rom: Vec<u8>,
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
    /// Virtual SCSI Controller (`$FC0000` ROM + `IOCS $F5` + `romdb`).
    pub scsi: crate::scsi::ScsiController,
    /// Host OS mouse pointer in guest screen coordinates.
    pub host_mouse_pos: Option<(i16, i16)>,
    /// Host wheel notches waiting to be delivered to SX-Window.
    pub(crate) pending_wheel: crate::sx_wheel::PendingWheel,
    /// Wheel direction and speed settings.
    pub wheel_config: crate::sx_wheel::WheelConfig,
    /// Configured display target resolution `(width, height)` across reboots and mode switches.
    pub target_resolution: Option<(u32, u32)>,
    /// Screen buffer for resolutions beyond 1024×1024, whose screen `Bitmap` records hold
    /// [`EXT_TVRAM_BASE`] as their base. Only native drawing reaches it unless
    /// [`Self::guest_screen_mapped`] is set.
    pub(crate) ext_tvram: Option<Vec<u8>>,
    /// `SYS/FSX.X` load address once its Graph Manager APIs may be replaced natively.
    pub(crate) native_gm_text_base: Option<u32>,
    /// Whether the host draws the mouse cursor in place of `SYS/FSX.X`.
    pub(crate) host_cursor: bool,
    /// A native call stalled until a V-DISP falling edge, as `(pc, falls to reach)`.
    pub(crate) vsync_wait: Option<(u32, u64)>,
    /// Resolution waiting for a safe point in the guest scheduler.
    pub(crate) pending_resolution: Option<(u32, u32)>,
    /// Allows native Graph Manager APIs; clear to run everything through the guest routines.
    pub native_gm_enabled: bool,
    /// Number of Graph Manager calls completed natively.
    pub native_gm_calls: u64,
    /// Whether the reserved range serves msx68k extensions, as in the primary SX-Window VM,
    /// whose `HUMAN.SYS` and `SYS/FSX.X` use no address bits above 24 as flags. Otherwise the
    /// range mirrors the 24-bit space like every other address above it, for compatibility.
    pub extensions_reserved: bool,
    /// CPU the VM runs, for the system port's MPU identification and MC68030 bus timing.
    pub cpu_model: CpuModel,
    /// Lets guest code read and write the extended screen, as verification against the guest
    /// drawing routines needs. Otherwise such accesses are dropped and counted as illegal.
    pub guest_screen_mapped: bool,
    /// Number of guest accesses to the reserved range that no extension serves to the guest,
    /// including the extended screen while it is not mapped.
    pub illegal_reserved_accesses: u64,
    /// The last A-Line opcode and its address, for reporting illegal reserved range accesses.
    pub(crate) last_a_line: (u16, u32),
    /// A-Line opcodes already reported for illegal reserved range accesses.
    reported_screen_accesses: std::collections::HashSet<u16>,
    pub(crate) ext_tvram_row_bytes: usize,
    pub(crate) ext_tvram_plane_bytes: usize,
    /// Cached extended planar TVRAM preserved across downscaling to standard resolutions.
    pub(crate) saved_ext_tvram: Option<(u32, u32, usize, usize, Vec<u8>)>,
    /// High watermark of desktop dimensions seen so far.
    pub(crate) max_desktop_size: (u32, u32),
    /// Host OS native display resolution for fullscreen 1:1 mode.
    pub host_native_resolution: (u32, u32),
    /// Whether host native fullscreen mode is active.
    pub fullscreen_mode: bool,
}

impl X68kBus {
    /// Creates a new X68000 bus with `ram_size_bytes` (clamped to `12 MB`) and `timing` configuration.
    ///
    /// Video RAM buffers start as `None` (zero allocation overhead) and are instantiated on demand
    /// when first written.
    pub fn new(ram_size_bytes: usize, timing: BusTimingConfig) -> Self {
        let ram_bytes = ram_size_bytes.min(MAX_MAIN_RAM_BYTES);
        let mut sram = vec![0u8; 16 * 1024];
        // Populate standard X68000 SRAM factory defaults ("Ｘ68000W" = $82 $77 "68000W" at $ED0000,
        // boot vectors, contrast, key repeat, and default Text Palette at $ED002E..$ED003B)
        // and write the configured Main RAM size at $ED0008 so IPLROM preserves it.
        const DEFAULT_SRAM_HEADER: [u8; 0x5A] = [
            0x82, 0x77, 0x36, 0x38, 0x30, 0x30, 0x30, 0x57, 0x00, 0x10, 0x00, 0x00, 0x00, 0xBF,
            0xFF, 0xFC, 0x00, 0xED, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x4E, 0x07,
            0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x07, 0x00, 0x0E, 0x00,
            0x0D, 0x00, 0x00, 0x00, 0x00, 0x00, 0xF8, 0x3E, 0xFF, 0xC0, 0xFF, 0xFE, 0xDE, 0x6C,
            0x40, 0x22, 0x03, 0x02, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0xFF, 0xDC, 0x00, 0x04, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x20,
            0x00, 0x09, 0xF9, 0x01, 0x00, 0x00,
        ];
        sram[0..DEFAULT_SRAM_HEADER.len()].copy_from_slice(&DEFAULT_SRAM_HEADER);
        let ram_end = (ram_bytes as u32).to_be_bytes();
        sram[8..12].copy_from_slice(&ram_end);

        Self {
            main_ram: vec![0; ram_bytes],
            gvram: None,
            tvram: None,
            vram_written: DirtyVramPages::default(),
            raster: crate::raster::RasterCapture::default(),
            hypercall_sites: Default::default(),
            app_entry_installed: false,
            remote_fs: None,
            remote_boot_rom: false,
            vm_windows: Default::default(),
            sprite_bg: None,
            sprite_bg_extra: None,
            sprite_options: crate::sprite::SpriteOptions::default(),
            sram,
            sram_write_enabled: false,
            sram_dirty: false,
            supervisor_area_limit: RESET_SUPERVISOR_AREA,
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
            scsi: crate::scsi::ScsiController::new(),
            host_mouse_pos: None,
            pending_wheel: Default::default(),
            wheel_config: Default::default(),
            target_resolution: None,
            ext_tvram: None,
            native_gm_text_base: None,
            host_cursor: false,
            vsync_wait: None,
            pending_resolution: None,
            native_gm_enabled: true,
            native_gm_calls: 0,
            extensions_reserved: false,
            cpu_model: CpuModel::Mc68000,
            guest_screen_mapped: false,
            illegal_reserved_accesses: 0,
            last_a_line: (0, 0),
            reported_screen_accesses: std::collections::HashSet::new(),
            ext_tvram_row_bytes: 0,
            ext_tvram_plane_bytes: 0,
            saved_ext_tvram: None,
            max_desktop_size: (0, 0),
            host_native_resolution: (1920, 1080),
            fullscreen_mode: false,
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
        self.vram_written.tvram = u128::MAX;
        self.tvram
            .get_or_insert_with(|| vec![0u8; TVRAM_BYTES].into_boxed_slice())
    }

    /// Returns the TVRAM and GVRAM pages written since the last call, and forgets them.
    pub fn take_written_vram_pages(&mut self) -> DirtyVramPages {
        std::mem::take(&mut self.vram_written)
    }

    /// Records a write to the TVRAM byte at `offset`, in every plane it reaches.
    fn mark_tvram_write(&mut self, offset: usize) {
        let r21 = self.crtc.regs[21];
        if r21 & 0x0100 != 0 {
            self.vram_written
                .mark_tvram_planes(r21 >> 4, offset % TVRAM_PLANE_BYTES);
        } else {
            self.vram_written.mark_tvram(offset);
        }
    }

    /// Returns `true` if the extended planar TVRAM at `0x1000_0000` is active.
    #[inline]
    pub fn is_ext_tvram_active(&self) -> bool {
        self.ext_tvram.is_some() && self.ext_tvram_plane_bytes > 0
    }

    /// Returns the currently active planar TVRAM buffer (extended TVRAM if active, else standard TVRAM).
    #[inline]
    pub fn active_tvram(&self) -> Option<&[u8]> {
        if self.is_ext_tvram_active() {
            self.ext_tvram.as_deref()
        } else {
            self.tvram.as_deref()
        }
    }

    /// Returns the scanline row bytes of the active planar TVRAM (128 for standard TVRAM).
    #[inline]
    pub fn active_tvram_row_bytes(&self) -> usize {
        if self.is_ext_tvram_active() {
            self.ext_tvram_row_bytes
        } else {
            128
        }
    }

    /// Returns the plane byte stride of the active planar TVRAM (128 KB for standard TVRAM).
    #[inline]
    pub fn active_tvram_plane_bytes(&self) -> usize {
        if self.is_ext_tvram_active() {
            self.ext_tvram_plane_bytes
        } else {
            crate::crtc::TVRAM_PLANE_BYTES
        }
    }

    /// Returns the maximum desktop dimensions allocated during live resizing sessions.
    #[inline]
    pub fn max_desktop_size(&self) -> (u32, u32) {
        self.max_desktop_size
    }

    /// Returns `true` if an extended CRT mode ($50) or resolution beyond standard 768x512 is active.
    pub fn is_extended_crtmod_active(&self) -> bool {
        self.fullscreen_mode
            || self.target_resolution.is_some_and(|(w, h)| (w, h) != (768, 512))
            || (self.sram.get(0x7E).copied().unwrap_or(0) & 0x3F) == 0x30
    }

    /// Packed physical GVRAM (`512 KiB`), independent of the CPU window mode.
    #[inline]
    pub fn gvram(&self) -> Option<&[u8]> {
        self.gvram.as_deref()
    }

    /// Direct slice access to Sprite/PCG/BG RAM (`64 KB`, `$EB0000`–`$EBFFFF`) if allocated.
    #[inline]
    pub fn sprite_bg(&self) -> Option<&[u8]> {
        self.sprite_bg.as_deref()
    }

    /// Extra pattern/map RAM without the shared registers.
    pub fn sprite_bg_extra(&self) -> Option<&[u8]> {
        self.sprite_bg_extra.as_deref()
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

    /// Returns whether battery-backed SRAM has been modified since last persistence check, clearing the flag.
    pub fn take_sram_dirty(&mut self) -> bool {
        let d = self.sram_dirty;
        self.sram_dirty = false;
        d
    }

    /// Performs a hardware soft reset: resets peripherals, CRTC/VICON, extended display mode,
    /// supervisor protection boundaries, and re-mirrors IPLROM reset vectors to `$000000`–`$000007`.
    pub fn soft_reset(&mut self) {
        if self.rom.len() >= ROM_WINDOW_BYTES && self.main_ram.len() >= 8 {
            let ipl_vectors =
                &self.rom[ROM_WINDOW_BYTES - 64 * 1024..ROM_WINDOW_BYTES - 64 * 1024 + 8];
            if ipl_vectors != [0xFF; 8] {
                self.main_ram[0..8].copy_from_slice(ipl_vectors);
            }
        }
        self.supervisor_area_limit = RESET_SUPERVISOR_AREA;
        self.sram_write_enabled = false;
        self.forget_ram_hypercalls();
        self.scsi.reset_driver();

        self.crtc = Crtc::new();
        self.raster = crate::raster::RasterCapture::default();
        self.vicon = Vicon::new();
        self.dmac.reset();
        self.mfp.reset();
        self.opm.reset();
        self.scc.reset();
        self.fdc.reset(&mut self.ioi);

        self.ext_tvram = None;
        self.native_gm_text_base = None;
        self.pending_resolution = None;
        self.host_cursor = false;
        self.vsync_wait = None;
        self.ext_tvram_row_bytes = 0;
        self.ext_tvram_plane_bytes = 0;
        self.saved_ext_tvram = None;
        self.max_desktop_size = (0, 0);
        self.target_resolution = None;
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

    /// Inserts a floppy disk image (`.XDF` / `.2HD` / `.2DD` / `.DIM`) into FDD unit `unit` (`0..=3`).
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

    /// Whether a guest access to `addr` goes to the reserved range rather than the 24-bit space.
    #[inline]
    fn in_reserved_range(&self, addr: u32) -> bool {
        self.extensions_reserved && is_reserved_addr(addr)
    }

    /// Notes a guest access to the reserved range and returns whether it reaches the extended
    /// screen.
    ///
    /// The range lies above the 68000's 24-bit address bus, so only code that took an address
    /// from an extension gets here, such as guest drawing that native code has not replaced
    /// taking the screen base from a `Bitmap` record.
    #[cold]
    fn guest_reserved_access(&mut self, addr: u32, is_write: bool) -> bool {
        if self.guest_screen_mapped && is_ext_screen_addr(addr) {
            return true;
        }
        self.illegal_reserved_accesses += 1;
        let (opcode, pc) = self.last_a_line;
        if self.reported_screen_accesses.insert(opcode) {
            eprintln!(
                "Illegal {} of reserved address ${addr:08X} during A-Line ${opcode:04X} called at ${pc:06X}",
                if is_write { "write" } else { "read" }
            );
        }
        false
    }

    /// Reads a memory byte for a debugger without touching any bus or peripheral state.
    ///
    /// Returns `None` for I/O regions, whose reads may have side effects, and for unmapped memory.
    pub fn peek_u8(&self, addr: u32) -> Option<u8> {
        if is_ext_screen_addr(addr) {
            let off = (addr - EXT_TVRAM_BASE) as usize;
            return self.ext_tvram.as_ref().and_then(|b| b.get(off).copied());
        }
        let a = addr & 0x00FF_FFFF;
        match MemoryRegion::from_addr(a) {
            MemoryRegion::MainRam => self.main_ram.get(a as usize).copied(),
            MemoryRegion::Gvram => {
                crate::gvram::byte_lane(a, self.crtc.regs[20])
                    .map(|lane| lane.read(self.gvram.as_deref()))
            }
            MemoryRegion::Tvram => {
                let offset = (a as usize) & (TVRAM_BYTES - 1);
                Some(self.tvram.as_ref().map_or(0, |buf| buf[offset]))
            }
            MemoryRegion::Sram => self.sram.get((a - 0x00ED_0000) as usize).copied(),
            MemoryRegion::Rom => {
                if self.in_absent_scsi_rom_window(a) {
                    return None;
                }
                self.rom.get((a - 0x00F0_0000) as usize).copied()
            }
            _ => None,
        }
    }

    /// Writes a Main RAM or ROM byte for a debugger, returning `false` for any other address.
    pub fn poke_u8(&mut self, addr: u32, val: u8) -> bool {
        if is_ext_screen_addr(addr) {
            return false;
        }
        match MemoryRegion::from_addr(addr & 0x00FF_FFFF) {
            MemoryRegion::MainRam => self.write_u8(addr, val, true).is_ok(),
            MemoryRegion::Rom => {
                self.write_rom_u8(addr, val);
                true
            }
            _ => false,
        }
    }

    /// Most recently completed register/palette and BG code history. Text/graphics VRAM is frame-final.
    pub fn raster_frame(&self) -> Option<&crate::raster::RasterFrame> {
        self.raster.completed()
    }

    /// Advances peripherals without skipping scanline edges, even for a large host-time batch.
    pub fn advance_peripherals(&mut self, mut cycles_10mhz: u32) {
        while cycles_10mhz != 0 {
            let until_sync = self.crtc.cycles_to_next_sync();
            let step = cycles_10mhz
                .min(until_sync)
                .min(self.crtc.cycles_to_next_display());
            let previous_raster = self.crtc.current_raster();
            let sync = self.crtc.advance(
                step,
                &mut self.tvram,
                &mut self.gvram,
                &mut self.vram_written,
            );
            if self.crtc.current_raster() != previous_raster {
                self.raster.boundary(
                    &self.crtc,
                    sync.v_disp_rose,
                    sync.v_disp_fell,
                );
            }
            if self.crtc.at_display_start() {
                self.raster.display(
                    &self.crtc,
                    &self.vicon,
                    self.sysport.contrast,
                    self.sprite_bg.as_deref(),
                );
                self.raster.fetch_sprite_bg(
                    &self.crtc,
                    self.sprite_bg.as_deref(),
                    self.sprite_bg_extra.as_deref(),
                    self.sprite_options,
                );
            }
            if sync.v_disp_fell {
                self.fdc.refresh_remote_media(&mut self.ioi);
            }
            self.mfp.update_crtc_sync(sync);
            self.opm.advance(step);
            self.mfp.set_opm_irq(self.opm.irq_active());
            self.mfp.advance(step);
            cycles_10mhz -= step;
        }
    }

    /// Returns the highest pending hardware interrupt priority level (`0..=7`) across all peripherals:
    /// - Level 6: MFP (`MC68901`)
    /// - Level 5: SCC (`Z8530` Mouse / RS-232C)
    /// - Level 3: DMAC (`HD63450`)
    /// - Level 1: I/O Interrupt Controller (`FDC`, `FDD`, `HDC`, `PRN`)
    #[inline]
    pub fn highest_pending_ipl(&self) -> u8 {
        if self.mfp.has_pending_interrupt() {
            6
        } else if self.scc.has_pending_interrupt() {
            5
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

    /// Returns `true` once the guest shows its graphics desktop, every layer on, and is actively
    /// polling the mouse. The graphics mode in VICON R0 is left out: SX-Window picks it from its
    /// environment, `$0004` on some setups and `$0003` on others.
    #[inline]
    pub const fn is_mouse_polling_active(&self) -> bool {
        self.vicon.r2_display() == 0x003F
            && self.scc.poll_count() >= 8
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

    fn bus_cycle_030(&mut self, addr: u32) -> BusCycle030 {
        if self.in_reserved_range(addr) {
            // msx68k extensions answer from the host, without bus cycles worth modelling.
            return BusCycle030::default();
        }
        let a = addr & 0x00FF_FFFF;
        let (rom_wait, ram_wait) = self.sysport.x68030_memory_waits();
        MemoryRegion::from_addr(a).x68030_bus_cycle(a, rom_wait, ram_wait)
    }

    fn reset_peripherals(&mut self) {
        self.dmac.reset();
        self.mfp.reset();
        self.opm.reset();
        self.scc.reset();
        self.fdc.reset(&mut self.ioi);
    }

    fn acknowledge_interrupt(&mut self, level: u8) -> Option<u8> {
        match level {
            6 => self.mfp.acknowledge_interrupt(),
            5 => self.scc.acknowledge_interrupt(),
            3 => self.dmac.acknowledge_interrupt(),
            1 => self.ioi.acknowledge_interrupt(),
            _ => None,
        }
    }

    fn read_u8(&mut self, addr: u32, supervisor: bool) -> Result<u8, BusFault> {
        if !supervisor && (addr & 0x00FF_FFFF >= SUPERVISOR_ONLY_BASE || self.in_reserved_range(addr)) {
            return Err(BusFault::BusError {
                addr: addr & 0x00FF_FFFF,
                is_write: false,
            });
        }
        if self.in_reserved_range(addr) {
            if !self.guest_reserved_access(addr, false) {
                return Ok(0);
            }
            let off = (addr - EXT_TVRAM_BASE) as usize;
            return Ok(self.ext_tvram.as_ref().and_then(|b| b.get(off).copied()).unwrap_or(0));
        }
        let a = addr & 0x00FF_FFFF;
        let region = MemoryRegion::from_addr(a);
        self.record_region_access(region);

        match region {
            MemoryRegion::MainRam => {
                if !supervisor && a < self.supervisor_area_limit
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
                let lane = crate::gvram::byte_lane(a, self.crtc.regs[20])
                    .ok_or(BusFault::BusError {
                        addr: a,
                        is_write: false,
                    })?;
                Ok(lane.read(self.gvram.as_deref()))
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
                    if (a & 0x0F) == 0x0B && self.cpu_model == CpuModel::Mc68030 {
                        // MPU type and clock of the 25 MHz X68030.
                        return Ok(0xDC);
                    }
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
                if offset >= 0x8000 && crate::bg::control(self.sprite_bg()) & 0x400 != 0 {
                    return Ok(if self.sprite_options.full_pattern_memory {
                        self.sprite_bg_extra.as_ref().map_or(0, |buf| buf[offset - 0x8000])
                    } else {
                        0xff
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
                if self.in_absent_scsi_rom_window(a) {
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
        if !supervisor && (addr & 0x00FF_FFFF >= SUPERVISOR_ONLY_BASE || self.in_reserved_range(addr)) {
            return Err(BusFault::BusError {
                addr: addr & 0x00FF_FFFF,
                is_write: false,
            });
        }
        if self.in_reserved_range(addr) {
            if !self.guest_reserved_access(addr, false) {
                return Ok(0);
            }
            let off = (addr - EXT_TVRAM_BASE) as usize;
            if let Some(ref b) = self.ext_tvram {
                if off + 1 < b.len() {
                    return Ok(((b[off] as u16) << 8) | (b[off + 1] as u16));
                }
            }
            return Ok(0);
        }
        let a = addr & 0x00FF_FFFF;
        // Fast path for Main RAM aligned 16-bit read
        if a < 0x00C0_0000 {
            if !supervisor && a < self.supervisor_area_limit {
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
        if !supervisor && (addr & 0x00FF_FFFF >= SUPERVISOR_ONLY_BASE || self.in_reserved_range(addr)) {
            return Err(BusFault::BusError {
                addr: addr & 0x00FF_FFFF,
                is_write: true,
            });
        }
        if self.in_reserved_range(addr) {
            if !self.guest_reserved_access(addr, true) {
                return Ok(());
            }
            self.access_flags.tvram_accessed = true;
            let off = (addr - EXT_TVRAM_BASE) as usize;
            let plane_bytes = self.ext_tvram_plane_bytes;
            if let Some(ref mut b) = self.ext_tvram {
                if plane_bytes > 0 {
                    self.crtc.write_planar_vram_u8(b, plane_bytes, off, val);
                }
            }
            return Ok(());
        }
        let a = addr & 0x00FF_FFFF;
        let region = MemoryRegion::from_addr(a);
        self.record_region_access(region);

        match region {
            MemoryRegion::MainRam => {
                if !supervisor && a < self.supervisor_area_limit
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
                if (0x0000_0938..=0x0000_093B).contains(&a) {
                    self.check_fsx_mouse_hook_patch();
                }
                Ok(())
            }
            MemoryRegion::Gvram => {
                let lane = crate::gvram::byte_lane(a, self.crtc.regs[20])
                    .ok_or(BusFault::BusError {
                        addr: a,
                        is_write: true,
                    })?;
                if lane.mask != 0 {
                    let buf = self
                        .gvram
                        .get_or_insert_with(|| vec![0u8; GVRAM_BYTES].into_boxed_slice());
                    lane.write(buf, val);
                    self.vram_written.mark_gvram(lane.offset);
                }
                Ok(())
            }
            MemoryRegion::Tvram => {
                let buf = self
                    .tvram
                    .get_or_insert_with(|| vec![0u8; TVRAM_BYTES].into_boxed_slice());
                self.crtc.write_tvram_u8(buf, a, val);
                self.mark_tvram_write((a as usize) & (TVRAM_BYTES - 1));
                Ok(())
            }
            MemoryRegion::Crtc => {
                let res = self.crtc
                    .write_u8(a, val, &mut self.tvram, &mut self.gvram, &mut self.vram_written);
                if let Some((w, h)) = self.target_resolution {
                    if (w, h) != (768, 512) {
                        let offset = (a & 0x3F) as usize;
                        let reg_idx = offset >> 1;
                        if matches!(reg_idx, 2 | 3 | 4 | 6 | 7 | 10 | 11 | 20) {
                            self.crtc.regs[2] = 0;
                            self.crtc.regs[3] = w.div_ceil(8) as u16;
                            self.crtc.exact_width = (!w.is_multiple_of(8)).then_some(w);
                            self.crtc.regs[4] = (h as u16) + 32;
                            self.crtc.regs[6] = 0;
                            self.crtc.regs[7] = h as u16;
                            self.crtc.regs[10] = 0;
                            self.crtc.regs[11] = 0;
                            if h > 512 {
                                self.crtc.regs[20] = 0x001F;
                            }
                        }
                    }
                }
                res
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
                if offset >= 0x8000 && crate::bg::control(self.sprite_bg()) & 0x400 != 0 {
                    if self.sprite_options.full_pattern_memory {
                        let buf = self.sprite_bg_extra
                            .get_or_insert_with(|| vec![0; 0x8000].into_boxed_slice());
                        buf[offset - 0x8000] = val;
                    }
                    return Ok(());
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
                    self.sram_dirty = true;
                }
                Ok(())
            }
            MemoryRegion::Rom => {
                if self.in_absent_scsi_rom_window(a) {
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
        if !supervisor && (addr & 0x00FF_FFFF >= SUPERVISOR_ONLY_BASE || self.in_reserved_range(addr)) {
            return Err(BusFault::BusError {
                addr: addr & 0x00FF_FFFF,
                is_write: true,
            });
        }
        if self.in_reserved_range(addr) {
            if !self.guest_reserved_access(addr, true) {
                return Ok(());
            }
            self.access_flags.tvram_accessed = true;
            let off = (addr - EXT_TVRAM_BASE) as usize;
            let plane_bytes = self.ext_tvram_plane_bytes;
            if let Some(ref mut b) = self.ext_tvram {
                if plane_bytes > 0 {
                    self.crtc.write_planar_vram_u16(b, plane_bytes, off, val);
                }
            }
            return Ok(());
        }
        let a = addr & 0x00FF_FFFF;
        if a < 0x00C0_0000 {
            if !supervisor && a < self.supervisor_area_limit {
                return Err(BusFault::BusError {
                    addr: a,
                    is_write: true,
                });
            }
            let idx = a as usize;
            if idx + 1 < self.main_ram.len() {
                self.main_ram[idx] = (val >> 8) as u8;
                self.main_ram[idx + 1] = val as u8;
                if (0x0000_0938..=0x0000_093B).contains(&a)
                    || (0x0000_0938..=0x0000_093B).contains(&(a + 1))
                {
                    self.check_fsx_mouse_hook_patch();
                }
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
            self.mark_tvram_write((a as usize) & (TVRAM_BYTES - 2));
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
