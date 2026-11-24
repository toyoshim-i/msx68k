//! X68000 Virtual Machine wrapper combining the monomorphized MC68000 CPU core
//! and the region-aware X68000 system bus.

use std::time::Instant;

use msx68k_cpu::{
    Burst030Policy, BurstPolicy, BusFault, CpuCore, CpuModel, CpuPolicy, CpuState, StepOutcome,
    Tracked030Policy, TrackedPolicy, TrapHandler,
};

use crate::bus::{X68kBus, MAX_MAIN_RAM_BYTES};
use crate::gdb::DebugState;
use crate::timing::{BusTimingConfig, CpuSpeedMode, VmRole};

/// Default mixed-mode hypercall trap handler for the X68000 virtual machine.
///
/// Hands F-Line instructions to [`X68kBus::handle_hypercall`], which carries out the
/// hypercalls installed at their addresses and passes every other F-Line and A-Line
/// instruction through the standard 68000 exception vectors.
#[derive(Debug, Default, Clone, Copy)]
pub struct BusTrapHandler;

impl TrapHandler<X68kBus> for BusTrapHandler {
    #[inline]
    fn on_a_line(
        &mut self,
        state: &mut CpuState,
        bus: &mut X68kBus,
        opcode: u16,
        pc: u32,
    ) -> msx68k_cpu::TrapResult {
        bus.last_a_line = (opcode, pc);
        if opcode == crate::sx_wheel::EM_GET {
            bus.deliver_wheel_notch();
            bus.wake_vm_windows();
        }
        bus.apply_pending_resolution(state, opcode);
        bus.handle_native_gm(state, opcode)
    }

    #[inline]
    fn on_f_line(
        &mut self,
        state: &mut CpuState,
        bus: &mut X68kBus,
        opcode: u16,
        pc: u32,
    ) -> msx68k_cpu::TrapResult {
        bus.handle_hypercall(state, opcode, pc)
    }

    #[inline]
    fn on_trap(
        &mut self,
        state: &mut CpuState,
        bus: &mut X68kBus,
        trap_no: u8,
        _pc: u32,
    ) -> msx68k_cpu::TrapResult {
        if trap_no == 15 && (state.d[0] & 0xFF) == 0x8E {
            return bus.custom_boot_info(state);
        }
        if trap_no == 15 && (state.d[0] & 0xFF) == 0x10 {
            return bus.handle_iocs_crtmod(state);
        }
        if trap_no == 15 && (state.d[0] & 0xFF) == IOCS_SYS_STAT && state.model == CpuModel::Mc68030
        {
            return iocs_sys_stat(state, bus);
        }
        msx68k_cpu::TrapResult::PassToVector
    }
}

/// IOCS call number of `_SYS_STAT`.
const IOCS_SYS_STAT: u32 = 0xAC;
/// IOCS calls next to `_SYS_STAT` that IPL ROMs before the X68030's leave undefined too.
const IOCS_SYS_STAT_NEIGHBORS: [u32; 3] = [0xAB, 0xAD, 0xB0];

/// Returns the address of the IOCS call table entry for call `number`.
const fn iocs_vector(number: u32) -> u32 {
    0x0400 + number * 4
}

/// Answers IOCS `_SYS_STAT` for an MC68030.
///
/// Human68k 3.02 uses it for cache control once it finds an MC68030, but only the X68030
/// IPL ROM has it. It is answered natively only while the call table entry is the ROM's
/// handler for undefined calls, shared with its neighbors, so the X68030 IPL ROM and drivers
/// that install their own keep handling it.
/// - `D1 = 0`: MPU status. 25 MHz MC68030, no FPU, and no MMU since none is emulated.
/// - `D1 = 1`: cache state (bit 0 instruction cache, bit 1 data cache).
/// - `D1 = 2`: restores the default state, both caches on, returning the previous state.
/// - `D1 = 3`: flushes the caches.
/// - `D1 = 4`: sets the cache state to `D2`, returning the previous state.
fn iocs_sys_stat(state: &mut CpuState, bus: &mut X68kBus) -> msx68k_cpu::TrapResult {
    const CACR_EI: u32 = 0x0001;
    const CACR_ED: u32 = 0x0100;
    const MPU_68030_25MHZ: u32 = (250 << 16) | 3;
    let handler = bus.peek_u32(iocs_vector(IOCS_SYS_STAT));
    let undefined = matches!(handler, Some(0x00FE_0000..=0x00FF_FFFF))
        && IOCS_SYS_STAT_NEIGHBORS
            .iter()
            .any(|&n| bus.peek_u32(iocs_vector(n)) == handler);
    if !undefined {
        return msx68k_cpu::TrapResult::PassToVector;
    }
    let caches = |cacr: u32| (cacr & CACR_EI) | ((cacr & CACR_ED) >> 7);
    let set_caches = |state: &mut CpuState, bits: u32| {
        state.cacr = (state.cacr & !(CACR_EI | CACR_ED)) | (bits & 1) | ((bits & 2) << 7);
    };
    let previous = caches(state.cacr);
    state.d[0] = match state.d[1] {
        0 => MPU_68030_25MHZ,
        1 => previous,
        2 => {
            set_caches(state, 3);
            previous
        }
        3 => 0,
        4 => {
            set_caches(state, state.d[2]);
            previous
        }
        _ => u32::MAX,
    };
    msx68k_cpu::TrapResult::Handled { extra_cycles: 0 }
}

/// Calls `$self.$method::<P, ...>(args)` with the policy `P` for the VM's CPU model and speed.
macro_rules! with_policy {
    ($self:ident, $method:ident $(::<$($rest:ty),*>)? ($($arg:expr),*)) => {
        match ($self.cpu_state.model, $self.is_running_burst()) {
            (CpuModel::Mc68030, true) => $self.$method::<Burst030Policy $($(, $rest)*)?>($($arg),*),
            (CpuModel::Mc68030, false) => $self.$method::<Tracked030Policy $($(, $rest)*)?>($($arg),*),
            (CpuModel::Mc68000, true) => $self.$method::<BurstPolicy $($(, $rest)*)?>($($arg),*),
            (CpuModel::Mc68000, false) => $self.$method::<TrackedPolicy $($(, $rest)*)?>($($arg),*),
        }
    };
}

/// Like [`with_policy`] for loops that always run without cycle tracking.
macro_rules! with_burst_policy {
    ($self:ident, $method:ident $(::<$($rest:ty),*>)? ($($arg:expr),*)) => {
        match $self.cpu_state.model {
            CpuModel::Mc68030 => $self.$method::<Burst030Policy $($(, $rest)*)?>($($arg),*),
            CpuModel::Mc68000 => $self.$method::<BurstPolicy $($(, $rest)*)?>($($arg),*),
        }
    };
}
pub(crate) use with_policy;

/// Configuration for spawning a Primary SX-Window VM or an Isolated Application VM.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct VmConfig {
    /// VM role (`PrimarySx` vs `AppVm`).
    pub role: VmRole,
    /// Selected CPU and System Bus speed mode.
    pub speed_mode: CpuSpeedMode,
    /// Main RAM size in bytes (e.g., `2 * 1024 * 1024` for 2 MB or `12 * 1024 * 1024` for 12 MB).
    pub ram_size_bytes: usize,
    /// Whether to temporarily switch to `Burst` mode during disk I/O operations.
    pub burst_on_disk_io: bool,
    /// Emulated CPU. An MC68030 is cycle-accurate at [`CpuSpeedMode::Mhz25`] (the X68030).
    pub cpu_model: CpuModel,
}

impl VmConfig {
    /// Creates the default configuration for the Primary SX-Window VM:
    /// - Role: [`VmRole::PrimarySx`]
    /// - Speed: [`CpuSpeedMode::Burst`] (uncapped CPU, zero system-bus delay)
    /// - RAM: `12 MB`
    pub const fn primary_sx() -> Self {
        Self {
            role: VmRole::PrimarySx,
            speed_mode: VmRole::PrimarySx.default_speed_mode(),
            ram_size_bytes: MAX_MAIN_RAM_BYTES,
            burst_on_disk_io: true,
            cpu_model: CpuModel::Mc68000,
        }
    }

    /// Creates the default configuration for an Isolated Application VM:
    /// - Role: [`VmRole::AppVm`]
    /// - Speed: [`CpuSpeedMode::Mhz10`] (10 MHz CPU, 10 MHz System Bus)
    /// - RAM: `2 MB` (configurable via [`Self::with_ram_mb`])
    pub const fn app_vm() -> Self {
        Self {
            role: VmRole::AppVm,
            speed_mode: VmRole::AppVm.default_speed_mode(),
            ram_size_bytes: 2 * 1024 * 1024,
            burst_on_disk_io: false,
            cpu_model: CpuModel::Mc68000,
        }
    }

    /// Creates an Isolated Application VM configured with an explicit per-app [`CpuSpeedMode`].
    pub const fn app_vm_with_speed(speed_mode: CpuSpeedMode) -> Self {
        Self {
            role: VmRole::AppVm,
            speed_mode,
            ram_size_bytes: 2 * 1024 * 1024,
            burst_on_disk_io: false,
            cpu_model: CpuModel::Mc68000,
        }
    }

    /// Builder method to override the [`CpuSpeedMode`] (`10MHz`, `16MHz`, `24MHz`, `Burst`, or `Custom`).
    pub const fn with_speed_mode(mut self, speed_mode: CpuSpeedMode) -> Self {
        self.speed_mode = speed_mode;
        self
    }

    /// Builder method to set Main RAM size in megabytes (`1`..=`12`).
    pub const fn with_ram_mb(mut self, mb: usize) -> Self {
        let clamped = if mb > 12 { 12 } else if mb == 0 { 1 } else { mb };
        self.ram_size_bytes = clamped * 1024 * 1024;
        self
    }

    /// Builder method to select the emulated CPU.
    pub const fn with_cpu_model(mut self, cpu_model: CpuModel) -> Self {
        self.cpu_model = cpu_model;
        self
    }

    /// Builder method to enable or disable temporary Burst mode during disk I/O.
    pub const fn with_burst_on_disk_io(mut self, enabled: bool) -> Self {
        self.burst_on_disk_io = enabled;
        self
    }
}

/// Instructions executed between host clock checks while Burst runs until a deadline.
pub(crate) const BURST_SYNC_INSTRUCTIONS: usize = 256;
/// Most peripheral ticks applied at one clock check, so time lost while rendering is made up
/// gradually instead of in a single jump that would merge timer interrupts.
const BURST_MAX_CATCH_UP_TICKS: u64 = 2_000;
/// Backlog above this (100 ms) is dropped, e.g. after the VM was halted or the window hidden.
const BURST_MAX_BACKLOG_TICKS: u64 = 1_000_000;

/// Tracks how many 10 MHz peripheral ticks Burst execution owes to match host time.
#[derive(Debug, Default)]
pub(crate) struct HostClock {
    origin: Option<Instant>,
    advanced: u64,
}

impl HostClock {
    /// Returns the ticks to apply now.
    pub(crate) fn ticks_due(&mut self, now: Instant) -> u32 {
        let origin = *self.origin.get_or_insert(now);
        let due = (now.duration_since(origin).as_nanos() / 100) as u64;
        if due < self.advanced {
            return 0;
        } // DMA has already advanced shared time.
        let owed = (due - self.advanced).min(BURST_MAX_BACKLOG_TICKS);
        self.advanced = due - owed;

        let ticks = owed.min(BURST_MAX_CATCH_UP_TICKS);
        self.advanced += ticks;
        ticks as u32
    }

    fn ahead(&self, now: Instant) -> bool {
        self.origin.is_some_and(|origin| {
            (now.duration_since(origin).as_nanos() / 100) < self.advanced as u128
        })
    }

    /// Restarts from the current moment, discarding time during which the guest was not running.
    pub(crate) fn rebase(&mut self) {
        *self = Self::default();
    }
}

/// How much guest execution one host frame is allowed.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FrameBudget {
    /// Burst flat out until `deadline`, with peripherals following host time.
    RealTime { deadline: Instant },
    /// A CPU clock cycle target ([`X68kVm::run_cycles`]).
    Cycles(u32),
    /// Run until the guest CRTC completes one vertical blank, bounded by `max_cycles`.
    VSync { max_cycles: u32 },
}

/// An X68000 Virtual Machine instance supporting seamless switching between
/// cycle-accurate (`10MHz`, `16MHz`, `24MHz`) and zero-overhead (`Burst`) execution.
pub struct X68kVm {
    config: VmConfig,
    /// Shared architectural CPU state (`D0-D7`, `A0-A7`, `PC`, `SR`, `USP`, `SSP`, `total_cycles`).
    pub cpu_state: CpuState,
    /// X68000 24-bit system bus and memory map.
    pub bus: X68kBus,
    /// Temporary disk I/O burst override flag.
    disk_io_burst_active: bool,
    /// GDB session state, only consulted by the debug execution loop.
    pub(crate) debug: DebugState,
    /// Host time reference for [`FrameBudget::RealTime`].
    pub(crate) host_clock: HostClock,
    /// CPU cycles times 10 not yet turned into whole 10 MHz peripheral ticks.
    tick_remainder: u32,
    dma_cpu_remainder: u32,
}

impl X68kVm {
    /// Creates a new X68000 VM instance from `config`.
    pub fn new(config: VmConfig) -> Self {
        let timing = config.speed_mode.timing_config();
        let mut bus = X68kBus::new(config.ram_size_bytes, timing);
        // Only the primary SX-Window VM runs software known not to use upper address bits as
        // flags, so other VMs keep the 68000's mirroring for compatibility.
        bus.extensions_reserved = config.role == VmRole::PrimarySx;
        bus.cpu_model = config.cpu_model;
        if config.role == VmRole::PrimarySx {
            bus.install_app_entry();
        }
        Self {
            config,
            cpu_state: CpuState::with_model(config.cpu_model),
            bus,
            disk_io_burst_active: false,
            debug: DebugState::default(),
            host_clock: HostClock::default(),
            tick_remainder: 0,
            dma_cpu_remainder: 0,
        }
    }

    /// Creates the Primary SX-Window VM (`Burst` mode, 12 MB RAM).
    pub fn new_primary_sx() -> Self {
        Self::new(VmConfig::primary_sx())
    }

    /// Creates an Isolated Application VM with the default `10 MHz` mode.
    pub fn new_app_vm() -> Self {
        Self::new(VmConfig::app_vm())
    }

    /// Creates an Isolated Application VM with a specific per-application [`CpuSpeedMode`].
    pub fn new_app_vm_with_speed(speed_mode: CpuSpeedMode) -> Self {
        Self::new(VmConfig::app_vm_with_speed(speed_mode))
    }

    /// Returns the VM's [`VmRole`].
    #[inline]
    pub const fn role(&self) -> VmRole {
        self.config.role
    }

    /// Returns the configured [`CpuSpeedMode`].
    #[inline]
    pub const fn speed_mode(&self) -> CpuSpeedMode {
        self.config.speed_mode
    }

    /// Returns the active [`BusTimingConfig`].
    #[inline]
    pub const fn timing_config(&self) -> BusTimingConfig {
        self.bus.timing_config()
    }

    /// Dynamically updates the VM's [`CpuSpeedMode`] (`10MHz`, `16MHz`, `24MHz`, `Burst`, or `Custom`)
    /// and rebuilds the system bus wait-state table.
    pub fn set_speed_mode(&mut self, speed_mode: CpuSpeedMode) {
        self.config.speed_mode = speed_mode;
        self.bus.set_timing_config(speed_mode.timing_config());
    }

    /// Toggles VICON active-display scanline contention on GVRAM and TVRAM.
    pub fn set_vram_display_contention(&mut self, active: bool) {
        let next_timing = self.bus.timing_config().with_vram_contention(active);
        self.bus.set_timing_config(next_timing);
    }

    /// Enables or disables OPM audio sample buffering.
    #[inline]
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.bus.set_audio_enabled(enabled);
    }

    /// Configures whether OPM FM synthesis runs inline or is offloaded via register events.
    #[inline]
    pub fn set_opm_synth_mode(&mut self, mode: crate::opm::OpmSynthMode) {
        self.bus.set_opm_synth_mode(mode);
    }

    /// Dynamically enables or disables the ADPCM 2nd-order Butterworth low-pass filter.
    #[inline]
    pub fn set_adpcm_lpf_enabled(&mut self, enabled: bool) {
        self.bus.set_adpcm_lpf_enabled(enabled);
    }

    /// Returns whether the ADPCM low-pass filter is enabled.
    #[inline]
    pub const fn adpcm_lpf_enabled(&self) -> bool {
        self.bus.adpcm_lpf_enabled()
    }

    /// Drains all generated OPM stereo PCM samples (at 62,500 Hz) into `out`.
    #[inline]
    pub fn take_audio_samples(&mut self, out: &mut Vec<(i16, i16)>) {
        self.bus.take_audio_samples(out);
    }

    /// Drains timestamped OPM register write events and returns the current 62,500 Hz sample clock.
    #[inline]
    pub fn take_audio_events(&mut self, out: &mut Vec<crate::opm::OpmRegEvent>) -> u64 {
        self.bus.take_audio_events(out)
    }

    /// Drains timestamped ADPCM level changes for `Offload` mode.
    #[inline]
    pub fn take_adpcm_events(&mut self, out: &mut Vec<crate::adpcm::AdpcmSampleEvent>) {
        self.bus.take_adpcm_events(out);
    }

    /// Enters or exits temporary disk-I/O Burst mode (when `config.burst_on_disk_io` is enabled).
    pub fn set_disk_io_active(&mut self, active: bool) {
        self.disk_io_burst_active = active && self.config.burst_on_disk_io;
    }

    /// Returns `true` if the VM is currently executing without cycle tracking ([`BurstPolicy`]
    /// or, for an MC68030, [`Burst030Policy`]).
    #[inline]
    pub const fn is_running_burst(&self) -> bool {
        self.disk_io_burst_active || self.config.speed_mode.is_burst()
    }

    /// Returns the emulated CPU.
    #[inline]
    pub const fn cpu_model(&self) -> CpuModel {
        self.cpu_state.model
    }

    /// Wraps the CPU state in a core of policy `P`, runs `f`, and stores the state back.
    #[inline(always)]
    pub(crate) fn with_core<P: CpuPolicy, R>(
        &mut self,
        f: impl FnOnce(&mut CpuCore<P>, &mut X68kBus) -> R,
    ) -> R {
        let mut core = CpuCore::<P>::from_state(std::mem::take(&mut self.cpu_state));
        let result = f(&mut core, &mut self.bus);
        self.cpu_state = core.into_state();
        result
    }

    /// Resets the CPU by loading initial `SSP` and `PC` from `$000000` and `$000004`.
    pub fn reset(&mut self) -> Result<u32, BusFault> {
        self.tick_remainder = 0;
        self.dma_cpu_remainder = 0;
        self.bus.soft_reset();
        with_policy!(self, reset_with())
    }

    fn reset_with<P: CpuPolicy>(&mut self) -> Result<u32, BusFault> {
        self.with_core::<P, _>(|core, bus| core.reset(bus))
    }

    /// Peripheral time, in 10 MHz ticks, that one instruction of `outcome` stands for.
    ///
    /// Tracked cycles carry their remainder over, since an MC68030 instruction often takes
    /// less than one 10 MHz tick.
    #[inline(always)]
    fn peripheral_ticks<P: CpuPolicy>(&mut self, outcome: StepOutcome) -> u32 {
        if P::TRACK_CYCLES {
            let cpu_mhz = self.bus.timing_config().cpu_mhz.max(10);
            let total = outcome.cycles.saturating_mul(10) + self.tick_remainder;
            self.tick_remainder = total % cpu_mhz;
            total / cpu_mhz
        } else if outcome.stopped {
            64
        } else {
            8
        }
    }

    #[inline(always)]
    fn advance_instruction<P: CpuPolicy>(
        bus: &mut X68kBus,
        state: &mut CpuState,
        ticks: u32,
        outcome: StepOutcome,
        remainder: &mut u32,
    ) -> u32 {
        if !bus.dmac.has_scheduled_work() {
            bus.advance_devices(ticks);
            return outcome.cycles;
        }
        Self::advance_dma_instruction::<P>(bus, state, ticks, outcome, remainder)
    }

    #[cold]
    fn advance_dma_instruction<P: CpuPolicy>(
        bus: &mut X68kBus,
        state: &mut CpuState,
        ticks: u32,
        outcome: StepOutcome,
        remainder: &mut u32,
    ) -> u32 {
        let stolen = bus.advance_dma_time(ticks);
        if P::TRACK_CYCLES {
            let total = stolen as u64 * bus.timing_config().cpu_mhz as u64 + *remainder as u64;
            let cycles = (total / 10) as u32;
            *remainder = (total % 10) as u32;
            state.total_cycles += cycles as u64;
            outcome.cycles.saturating_add(cycles)
        } else {
            outcome.cycles
        }
    }

    /// Executes a single instruction using the monomorphized policy matching the active speed mode,
    /// advancing motherboard peripherals (CRTC, MFP, OPM) and routing pending interrupts.
    #[inline]
    pub fn step_with_trap<T: TrapHandler<X68kBus>>(&mut self, trap_handler: &mut T) -> StepOutcome {
        with_policy!(self, step_with::<_>(trap_handler))
    }

    fn step_with<P: CpuPolicy, T: TrapHandler<X68kBus>>(
        &mut self,
        trap_handler: &mut T,
    ) -> StepOutcome {
        self.cpu_state.set_ipl(self.bus.highest_pending_ipl());
        let outcome = self.with_core::<P, _>(|core, bus| core.step(bus, trap_handler));
        let ticks = self.peripheral_ticks::<P>(outcome);
        let cycles = Self::advance_instruction::<P>(
            &mut self.bus,
            &mut self.cpu_state,
            ticks,
            outcome,
            &mut self.dma_cpu_remainder,
        );
        self.cpu_state.set_ipl(self.bus.highest_pending_ipl());
        StepOutcome { cycles, ..outcome }
    }

    /// Executes a single instruction with the default [`BusTrapHandler`].
    #[inline]
    pub fn step(&mut self) -> StepOutcome {
        let mut handler = BusTrapHandler;
        self.step_with_trap(&mut handler)
    }

    /// Executes up to `max_instructions` instructions inside a tight monomorphized loop.
    pub fn run_steps_with_trap<T: TrapHandler<X68kBus>>(
        &mut self,
        max_instructions: usize,
        trap_handler: &mut T,
    ) -> u64 {
        with_policy!(self, run_steps_with::<_>(max_instructions, trap_handler))
    }

    fn run_steps_with<P: CpuPolicy, T: TrapHandler<X68kBus>>(
        &mut self,
        max_instructions: usize,
        trap_handler: &mut T,
    ) -> u64 {
        let mut core = CpuCore::<P>::from_state(std::mem::take(&mut self.cpu_state));
        let mut elapsed = 0u64;
        for _ in 0..max_instructions {
            core.state.set_ipl(self.bus.highest_pending_ipl());
            let out = core.step(&mut self.bus, trap_handler);
            let ticks = self.peripheral_ticks::<P>(out);
            elapsed += Self::advance_instruction::<P>(
                &mut self.bus,
                &mut core.state,
                ticks,
                out,
                &mut self.dma_cpu_remainder,
            ) as u64;
        }
        core.state.set_ipl(self.bus.highest_pending_ipl());
        self.cpu_state = core.into_state();
        elapsed
    }

    /// Executes up to `max_instructions` instructions with the default [`BusTrapHandler`].
    pub fn run_steps(&mut self, max_instructions: usize) -> u64 {
        let mut handler = BusTrapHandler;
        self.run_steps_with_trap(max_instructions, &mut handler)
    }

    /// Executes until at least `target_cycles` CPU clock cycles have elapsed (for cycle-tracked modes).
    /// In `Burst` mode, executes `target_cycles` instructions as a batch budget.
    pub fn run_cycles_with_trap<T: TrapHandler<X68kBus>>(
        &mut self,
        target_cycles: u32,
        trap_handler: &mut T,
    ) -> u32 {
        if self.is_running_burst() {
            with_policy!(
                self,
                run_steps_with::<_>(target_cycles as usize, trap_handler)
            );
            return 0;
        }
        with_policy!(self, run_cycles_until_with::<_>(target_cycles, false, trap_handler))
    }

    fn run_cycles_until_with<P: CpuPolicy, T: TrapHandler<X68kBus>>(
        &mut self,
        target_cycles: u32,
        stop_on_vblank: bool,
        trap_handler: &mut T,
    ) -> u32 {
        let mut core = CpuCore::<P>::from_state(std::mem::take(&mut self.cpu_state));
        let start_frame = self.bus.crtc.frame_count();
        let mut elapsed = 0u32;
        while elapsed < target_cycles
            && (!stop_on_vblank || self.bus.crtc.frame_count() == start_frame)
        {
            core.state.set_ipl(self.bus.highest_pending_ipl());
            let out = core.step(&mut self.bus, trap_handler);
            let ticks = self.peripheral_ticks::<P>(out);
            elapsed = elapsed.saturating_add(Self::advance_instruction::<P>(
                &mut self.bus,
                &mut core.state,
                ticks,
                out,
                &mut self.dma_cpu_remainder,
            ));
        }
        core.state.set_ipl(self.bus.highest_pending_ipl());
        self.cpu_state = core.into_state();
        elapsed
    }

    /// Executes until the guest CRTC completes one vertical blank or `max_cycles` CPU clock cycles elapse.
    pub fn run_until_vblank(&mut self, max_cycles: u32) -> u32 {
        let mut handler = BusTrapHandler;
        if self.is_running_burst() {
            with_policy!(
                self,
                run_steps_with::<_>(max_cycles as usize, &mut handler)
            );
            return 0;
        }
        with_policy!(
            self,
            run_cycles_until_with::<_>(max_cycles, true, &mut handler)
        )
    }

    /// Executes until at least `target_cycles` CPU clock cycles have elapsed with the default [`BusTrapHandler`].
    pub fn run_cycles(&mut self, target_cycles: u32) -> u32 {
        let mut handler = BusTrapHandler;
        self.run_cycles_with_trap(target_cycles, &mut handler)
    }

    /// Executes up to `max_instructions` instructions in Burst mode while pacing peripheral
    /// advancement to exactly `target_ticks_10mhz` (e.g. `166_667` for a 1/60s frame).
    ///
    /// This prevents guest timers (MFP Timer-C 200Hz, V-Blank 55.45Hz, double-click intervals)
    /// from running disproportionately fast when CPU execution is uncapped in Burst mode.
    pub fn run_burst_frame_with_trap<T: TrapHandler<X68kBus>>(
        &mut self,
        max_instructions: usize,
        target_ticks_10mhz: u32,
        trap_handler: &mut T,
    ) -> usize {
        with_burst_policy!(
            self,
            run_burst_frame_with::<_>(max_instructions, target_ticks_10mhz, trap_handler)
        )
    }

    fn run_burst_frame_with<P: CpuPolicy, T: TrapHandler<X68kBus>>(
        &mut self,
        max_instructions: usize,
        target_ticks_10mhz: u32,
        trap_handler: &mut T,
    ) -> usize {
        let state = std::mem::take(&mut self.cpu_state);
        let mut core = CpuCore::<P>::from_state(state);
        let mut elapsed_ticks = 0u32;
        let mut tick_accum = 0usize;
        let mut instructions_executed = 0usize;

        let num_inst = max_instructions.max(1);
        while instructions_executed < max_instructions && elapsed_ticks < target_ticks_10mhz {
            core.state.set_ipl(self.bus.highest_pending_ipl());
            let out = core.step(&mut self.bus, trap_handler);
            instructions_executed += 1;

            if out.stopped {
                let remaining = target_ticks_10mhz.saturating_sub(elapsed_ticks);
                if remaining == 0 {
                    break;
                }
                let step = remaining.min(64);
                let stolen = self.bus.advance_cpu_time(step);
                elapsed_ticks += step + stolen;
            } else {
                tick_accum += target_ticks_10mhz as usize;
                let step = (tick_accum / num_inst) as u32;
                tick_accum %= num_inst;
                let actual_step = step.min(target_ticks_10mhz.saturating_sub(elapsed_ticks));
                // Even a zero-tick Burst instruction can start DMA at this boundary.
                let stolen = self.bus.advance_cpu_time(actual_step);
                elapsed_ticks += actual_step + stolen;
            }
        }

        let remaining = target_ticks_10mhz.saturating_sub(elapsed_ticks);
        if remaining > 0 {
            self.bus.advance_peripherals(remaining);
        }

        core.state.set_ipl(self.bus.highest_pending_ipl());
        self.cpu_state = core.into_state();
        instructions_executed
    }

    /// Executes up to `max_instructions` instructions in Burst mode while pacing peripheral
    /// advancement to `target_ticks_10mhz` using the default [`BusTrapHandler`].
    pub fn run_burst_frame(&mut self, max_instructions: usize, target_ticks_10mhz: u32) -> usize {
        let mut handler = BusTrapHandler;
        self.run_burst_frame_with_trap(max_instructions, target_ticks_10mhz, &mut handler)
    }

    /// Executes Burst code as fast as the host allows until `deadline`, advancing peripherals
    /// to match host time so guest timers run at their real rate.
    pub fn run_burst_real_time(&mut self, deadline: Instant) {
        with_burst_policy!(self, run_burst_real_time_with(deadline))
    }

    fn run_burst_real_time_with<P: CpuPolicy>(&mut self, deadline: Instant) {
        if self.host_clock.ahead(Instant::now()) {
            return;
        }
        let state = std::mem::take(&mut self.cpu_state);
        let mut core = CpuCore::<P>::from_state(state);
        let mut handler = BusTrapHandler;
        'running: loop {
            for _ in 0..BURST_SYNC_INSTRUCTIONS {
                core.state.set_ipl(self.bus.highest_pending_ipl());
                core.step(&mut self.bus, &mut handler);
                if self.bus.dmac.has_scheduled_work() {
                    let now = Instant::now();
                    let ticks = self.host_clock.ticks_due(now);
                    let stolen = self.bus.advance_dma_time(ticks);
                    self.host_clock.advanced += stolen as u64;
                    if self.host_clock.ahead(Instant::now()) || now >= deadline {
                        break 'running;
                    }
                }
            }
            let now = Instant::now();
            let ticks = self.host_clock.ticks_due(now);
            let stolen = self.bus.advance_cpu_time(ticks);
            self.host_clock.advanced += stolen as u64;
            if now >= deadline || self.host_clock.ahead(Instant::now()) {
                break;
            }
        }
        core.state.set_ipl(self.bus.highest_pending_ipl());
        self.cpu_state = core.into_state();
    }

    /// Executes one host frame of guest code as described by `budget`.
    pub fn run_frame(&mut self, budget: FrameBudget) {
        match budget {
            FrameBudget::RealTime { deadline } => self.run_burst_real_time(deadline),
            FrameBudget::Cycles(cycles) => {
                self.run_cycles(cycles);
            }
            FrameBudget::VSync { max_cycles } => {
                self.run_until_vblank(max_cycles);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn dma_time_ahead_of_host_is_paid_before_burst_resumes() {
        let start = Instant::now();
        let mut clock = HostClock::default();
        clock.ticks_due(start);
        clock.advanced += 1000;
        assert!(clock.ahead(start));
        assert_eq!(clock.ticks_due(start + Duration::from_micros(50)), 0);
        assert_eq!(clock.advanced, 1000);
        assert!(!clock.ahead(start + Duration::from_micros(100)));
        assert_eq!(clock.ticks_due(start + Duration::from_micros(110)), 100);
    }

    #[test]
    fn dma_time_counts_toward_cpu_and_burst_budgets() {
        use msx68k_cpu::Bus;
        for speed in [
            CpuSpeedMode::Mhz10,
            CpuSpeedMode::Mhz16,
            CpuSpeedMode::Burst,
        ] {
            let mut vm = X68kVm::new(VmConfig::app_vm().with_speed_mode(speed));
            vm.cpu_state.pc = 0x2000;
            vm.cpu_state.sr = 0; // DMA FC5 works while the CPU runs user code.
            vm.bus.write_u16(0x2000, 0x60fe, true).unwrap(); // BRA.S self
            let ch = &mut vm.bus.dmac.channels[2];
            ch.dcr = 8;
            ch.ocr = 0x11;
            ch.scr = 5;
            ch.mar = 0x3000;
            ch.dar = 0x4000;
            ch.mtc = 100;
            ch.mfc = 5;
            ch.dfc = 5;
            vm.bus.dmac.write_u8(0xe84087, 0x80).unwrap();
            if speed == CpuSpeedMode::Burst {
                assert_eq!(vm.run_burst_frame(1000, 100), 1);
            } else {
                let before = vm.cpu_state.total_cycles;
                let outcome = vm.step();
                assert!(outcome.cycles >= 1000);
                assert_eq!(vm.cpu_state.total_cycles - before, outcome.cycles as u64);
            }
            assert_eq!(vm.bus.dmac.channels[2].mtc, 0);
        }
    }

    #[test]
    fn host_clock_pays_time_off_gradually() {
        let start = Instant::now();
        let mut clock = HostClock::default();
        assert_eq!(clock.ticks_due(start), 0);

        // 1 ms is 10,000 ticks, more than one check may apply.
        let later = start + Duration::from_millis(1);
        let mut paid = 0;
        for _ in 0..5 {
            paid += clock.ticks_due(later);
        }
        assert_eq!(paid, 10_000);
        assert_eq!(clock.ticks_due(later), 0);
    }

    #[test]
    fn host_clock_drops_backlog_beyond_the_limit() {
        let start = Instant::now();
        let mut clock = HostClock::default();
        clock.ticks_due(start);

        let later = start + Duration::from_secs(10);
        let mut paid = 0u64;
        for _ in 0..1_000 {
            paid += clock.ticks_due(later) as u64;
        }
        assert_eq!(paid, BURST_MAX_BACKLOG_TICKS);
    }

    #[test]
    fn vsync_budget_stops_at_next_vblank() {
        use msx68k_cpu::Bus;
        let mut vm = X68kVm::new(VmConfig::app_vm().with_speed_mode(CpuSpeedMode::Mhz10));
        vm.cpu_state.pc = 0x2000;
        vm.cpu_state.sr = 0x2700;
        vm.bus.write_u16(0x2000, 0x60fe, true).unwrap(); // BRA.S self

        let start_frame = vm.bus.crtc.frame_count();
        vm.run_frame(FrameBudget::VSync {
            max_cycles: 400_000,
        });
        assert_eq!(vm.bus.crtc.frame_count(), start_frame + 1);
        // 55.46 Hz at 10 MHz is ~180,300 cycles, well under the 400,000-cycle cap.
        assert!(vm.cpu_state.total_cycles < 250_000);
    }
}
