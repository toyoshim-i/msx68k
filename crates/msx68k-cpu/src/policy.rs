//! Compile-time CPU execution policy for monomorphized cycle accounting.

/// Trait controlling whether the MC68000 core tracks instruction and bus wait cycles.
///
/// Because `TRACK_CYCLES` is an associated constant, Rust monomorphizes the
/// execution loop for [`TrackedPolicy`] and [`BurstPolicy`], completely eliminating
/// cycle additions and bus wait-state lookups in `Burst` mode.
pub trait CpuPolicy: Copy + Clone + Default + Send + Sync + 'static {
    /// When `true`, every instruction adds its base Motorola 68000 cycle count
    /// plus per-region memory/bus wait states for each 16-bit bus transfer.
    /// When `false`, all cycle and wait-state accounting compiles to a no-op.
    const TRACK_CYCLES: bool;

    /// When `true`, the core decodes the MC68030 instruction set, builds MC68030 exception
    /// frames, and allows unaligned data accesses. Must match [`crate::CpuState::model`].
    const MC68030: bool = false;
}

/// Cycle-accurate execution policy used for 10 MHz, 16 MHz (XVI), and 24 MHz (RedZone) modes.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct TrackedPolicy;

impl CpuPolicy for TrackedPolicy {
    const TRACK_CYCLES: bool = true;
}

/// Uncapped zero-wait execution policy used for the Master SX-Window VM and Burst mode.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct BurstPolicy;

impl CpuPolicy for BurstPolicy {
    const TRACK_CYCLES: bool = false;
}

/// Uncapped zero-wait execution policy for an MC68030 CPU.
///
/// There is no cycle-tracked MC68030 policy yet: the MC68030's instruction timing (caches,
/// pipelining, 32-bit bus) differs too much from the MC68000 tables to reuse them.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct Burst030Policy;

impl CpuPolicy for Burst030Policy {
    const TRACK_CYCLES: bool = false;
    const MC68030: bool = true;
}
