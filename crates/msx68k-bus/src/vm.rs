//! X68000 Virtual Machine wrapper combining the monomorphized MC68000 CPU core
//! and the region-aware X68000 system bus.

use msx68k_cpu::{
    BurstPolicy, BusFault, CpuCore, CpuState, NoopTrapHandler, StepOutcome, TrackedPolicy,
    TrapHandler,
};

use crate::bus::{X68kBus, MAX_MAIN_RAM_BYTES};
use crate::timing::{BusTimingConfig, CpuSpeedMode, VmRole};

/// Configuration for spawning a Master SX-Window VM or an Isolated Application VM.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct VmConfig {
    /// VM role (`MasterSx` vs `AppVm`).
    pub role: VmRole,
    /// Selected CPU and System Bus speed mode.
    pub speed_mode: CpuSpeedMode,
    /// Main RAM size in bytes (e.g., `2 * 1024 * 1024` for 2 MB or `12 * 1024 * 1024` for 12 MB).
    pub ram_size_bytes: usize,
    /// Whether to temporarily switch to `Burst` mode during disk I/O operations.
    pub burst_on_disk_io: bool,
}

impl VmConfig {
    /// Creates the default configuration for the Master SX-Window VM:
    /// - Role: [`VmRole::MasterSx`]
    /// - Speed: [`CpuSpeedMode::Burst`] (uncapped CPU, zero system-bus delay)
    /// - RAM: `12 MB`
    pub const fn master_sx() -> Self {
        Self {
            role: VmRole::MasterSx,
            speed_mode: VmRole::MasterSx.default_speed_mode(),
            ram_size_bytes: MAX_MAIN_RAM_BYTES,
            burst_on_disk_io: true,
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
        }
    }

    /// Creates an Isolated Application VM configured with an explicit per-app [`CpuSpeedMode`].
    pub const fn app_vm_with_speed(speed_mode: CpuSpeedMode) -> Self {
        Self {
            role: VmRole::AppVm,
            speed_mode,
            ram_size_bytes: 2 * 1024 * 1024,
            burst_on_disk_io: false,
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

    /// Builder method to enable or disable temporary Burst mode during disk I/O.
    pub const fn with_burst_on_disk_io(mut self, enabled: bool) -> Self {
        self.burst_on_disk_io = enabled;
        self
    }
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
}

impl X68kVm {
    /// Creates a new X68000 VM instance from `config`.
    pub fn new(config: VmConfig) -> Self {
        let timing = config.speed_mode.timing_config();
        Self {
            config,
            cpu_state: CpuState::new(),
            bus: X68kBus::new(config.ram_size_bytes, timing),
            disk_io_burst_active: false,
        }
    }

    /// Creates the Master SX-Window VM (`Burst` mode, 12 MB RAM).
    pub fn new_master_sx() -> Self {
        Self::new(VmConfig::master_sx())
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

    /// Enters or exits temporary disk-I/O Burst mode (when `config.burst_on_disk_io` is enabled).
    pub fn set_disk_io_active(&mut self, active: bool) {
        self.disk_io_burst_active = active && self.config.burst_on_disk_io;
    }

    /// Returns `true` if the VM is currently executing under the zero-wait [`BurstPolicy`].
    #[inline]
    pub const fn is_running_burst(&self) -> bool {
        self.disk_io_burst_active || self.config.speed_mode.is_burst()
    }

    /// Resets the CPU by loading initial `SSP` and `PC` from `$000000` and `$000004`.
    pub fn reset(&mut self) -> Result<u32, BusFault> {
        let state = std::mem::take(&mut self.cpu_state);
        if self.is_running_burst() {
            let mut core = CpuCore::<BurstPolicy>::from_state(state);
            let res = core.reset(&mut self.bus);
            self.cpu_state = core.into_state();
            res
        } else {
            let mut core = CpuCore::<TrackedPolicy>::from_state(state);
            let res = core.reset(&mut self.bus);
            self.cpu_state = core.into_state();
            res
        }
    }

    /// Executes a single instruction using the monomorphized policy matching the active speed mode,
    /// advancing motherboard peripherals (CRTC, MFP, OPM) and routing pending interrupts.
    #[inline]
    pub fn step_with_trap<T: TrapHandler>(&mut self, trap_handler: &mut T) -> StepOutcome {
        let bus_ipl = self.bus.highest_pending_ipl();
        self.cpu_state.set_ipl(bus_ipl);

        let state = std::mem::take(&mut self.cpu_state);
        if self.is_running_burst() {
            let mut core = CpuCore::<BurstPolicy>::from_state(state);
            let outcome = core.step(&mut self.bus, trap_handler);
            self.cpu_state = core.into_state();
            let virtual_ticks = if outcome.stopped { 64 } else { 8 };
            self.bus.advance_peripherals(virtual_ticks);
            self.cpu_state.set_ipl(self.bus.highest_pending_ipl());
            outcome
        } else {
            let mut core = CpuCore::<TrackedPolicy>::from_state(state);
            let outcome = core.step(&mut self.bus, trap_handler);
            self.cpu_state = core.into_state();
            let cpu_mhz = self.bus.timing_config().cpu_mhz.max(10);
            let ticks_10mhz = (outcome.cycles.saturating_mul(10) / cpu_mhz).max(1);
            self.bus.advance_peripherals(ticks_10mhz);
            self.cpu_state.set_ipl(self.bus.highest_pending_ipl());
            outcome
        }
    }

    /// Executes a single instruction with the default [`NoopTrapHandler`].
    #[inline]
    pub fn step(&mut self) -> StepOutcome {
        let mut noop = NoopTrapHandler;
        self.step_with_trap(&mut noop)
    }

    /// Executes up to `max_instructions` instructions inside a tight monomorphized loop.
    pub fn run_steps_with_trap<T: TrapHandler>(
        &mut self,
        max_instructions: usize,
        trap_handler: &mut T,
    ) -> u64 {
        let state = std::mem::take(&mut self.cpu_state);
        if self.is_running_burst() {
            let mut core = CpuCore::<BurstPolicy>::from_state(state);
            for _ in 0..max_instructions {
                core.state.set_ipl(self.bus.highest_pending_ipl());
                let out = core.step(&mut self.bus, trap_handler);
                let virtual_ticks = if out.stopped { 64 } else { 8 };
                self.bus.advance_peripherals(virtual_ticks);
            }
            core.state.set_ipl(self.bus.highest_pending_ipl());
            self.cpu_state = core.into_state();
            0
        } else {
            let mut core = CpuCore::<TrackedPolicy>::from_state(state);
            let cpu_mhz = self.bus.timing_config().cpu_mhz.max(10);
            let mut elapsed = 0u64;
            for _ in 0..max_instructions {
                core.state.set_ipl(self.bus.highest_pending_ipl());
                let out = core.step(&mut self.bus, trap_handler);
                elapsed += out.cycles as u64;
                let ticks_10mhz = (out.cycles.saturating_mul(10) / cpu_mhz).max(1);
                self.bus.advance_peripherals(ticks_10mhz);
            }
            core.state.set_ipl(self.bus.highest_pending_ipl());
            self.cpu_state = core.into_state();
            elapsed
        }
    }

    /// Executes up to `max_instructions` instructions with the default [`NoopTrapHandler`].
    pub fn run_steps(&mut self, max_instructions: usize) -> u64 {
        let mut noop = NoopTrapHandler;
        self.run_steps_with_trap(max_instructions, &mut noop)
    }

    /// Executes until at least `target_cycles` CPU clock cycles have elapsed (for cycle-tracked modes).
    /// In `Burst` mode, executes `target_cycles` instructions as a batch budget.
    pub fn run_cycles_with_trap<T: TrapHandler>(
        &mut self,
        target_cycles: u32,
        trap_handler: &mut T,
    ) -> u32 {
        let state = std::mem::take(&mut self.cpu_state);
        if self.is_running_burst() {
            let mut core = CpuCore::<BurstPolicy>::from_state(state);
            for _ in 0..target_cycles {
                core.state.set_ipl(self.bus.highest_pending_ipl());
                let out = core.step(&mut self.bus, trap_handler);
                let virtual_ticks = if out.stopped { 64 } else { 8 };
                self.bus.advance_peripherals(virtual_ticks);
            }
            core.state.set_ipl(self.bus.highest_pending_ipl());
            self.cpu_state = core.into_state();
            0
        } else {
            let mut core = CpuCore::<TrackedPolicy>::from_state(state);
            let cpu_mhz = self.bus.timing_config().cpu_mhz.max(10);
            let mut elapsed = 0u32;
            while elapsed < target_cycles {
                core.state.set_ipl(self.bus.highest_pending_ipl());
                let out = core.step(&mut self.bus, trap_handler);
                elapsed = elapsed.saturating_add(out.cycles);
                let ticks_10mhz = (out.cycles.saturating_mul(10) / cpu_mhz).max(1);
                self.bus.advance_peripherals(ticks_10mhz);
            }
            core.state.set_ipl(self.bus.highest_pending_ipl());
            self.cpu_state = core.into_state();
            elapsed
        }
    }
}
