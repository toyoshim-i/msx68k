//! System bus interface and fault types for the MC68000 CPU core.

/// Fault raised by the system bus during a memory or I/O transaction.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BusFault {
    /// Bus Error (Vector 2, `$0008`): unmapped memory, write-protected area,
    /// or user-mode access to supervisor-only memory/I/O.
    BusError { addr: u32, is_write: bool },
    /// Address Error (Vector 3, `$000C`): word or longword access at an odd address.
    AddressError { addr: u32, is_write: bool },
}

/// Result returned by a host A-Line, F-Line, or `TRAP #n` hook.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TrapResult {
    /// The trap was handled natively in Rust/ARM; continue normal execution at the updated `PC`.
    Handled { extra_cycles: u32 },
    /// The trap was not handled natively; take the standard 68000 exception vector.
    PassToVector,
    /// Equivalent to [`Self::PassToVector`].
    Unhandled,
}

/// Hook interface for Mixed Mode (`68k` ⇄ `ARM`), Human68k DOS calls (`$FFxx`),
/// FE-function FPU calls (`$FExx`), SX-Window Drawing Manager hooks, and `TRAP #n`.
pub trait TrapHandler<B = ()> {
    /// Intercepts an A-Line (`$Axxx`) instruction before Vector 10 (`$0028`) is taken.
    /// `pc` points to the `$Axxx` opcode word.
    fn on_a_line(
        &mut self,
        _state: &mut crate::state::CpuState,
        _bus: &mut B,
        _opcode: u16,
        _pc: u32,
    ) -> TrapResult {
        TrapResult::PassToVector
    }

    /// Intercepts an F-Line (`$Fxxx`) instruction before Vector 11 (`$002C`) is taken.
    /// `pc` points to the `$Fxxx` opcode word.
    fn on_f_line(
        &mut self,
        _state: &mut crate::state::CpuState,
        _bus: &mut B,
        _opcode: u16,
        _pc: u32,
    ) -> TrapResult {
        TrapResult::PassToVector
    }

    /// Intercepts a `TRAP #0..#15` instruction before Vectors 32..47 are taken.
    /// `pc` points to the instruction immediately following `TRAP #n`.
    fn on_trap(
        &mut self,
        _state: &mut crate::state::CpuState,
        _bus: &mut B,
        _trap_no: u8,
        _pc: u32,
    ) -> TrapResult {
        TrapResult::PassToVector
    }
}

/// Default no-op trap handler that delegates all traps to 68000 exception vectors.
#[derive(Debug, Default, Copy, Clone)]
pub struct NoopTrapHandler;

impl<B> TrapHandler<B> for NoopTrapHandler {}

/// Bus cycle parameters of an address for an MC68030.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct BusCycle030 {
    /// Wait states (clocks beyond the 2-clock minimum bus cycle).
    pub wait: u8,
    /// The port is 16 bits wide, so a long word takes two bus cycles.
    pub port16: bool,
    /// The address may be cached (the system does not assert `CIIN`).
    pub cacheable: bool,
    /// Average DRAM refresh delay per bus cycle, in eighths of a clock.
    pub refresh_eighths: u8,
}

impl Default for BusCycle030 {
    fn default() -> Self {
        Self {
            wait: 0,
            port16: false,
            cacheable: true,
            refresh_eighths: 0,
        }
    }
}

/// Memory and I/O bus interface for the MC68000 CPU.
///
/// The MC68000 has a 16-bit external data bus and a 24-bit address bus (`$000000`–`$FFFFFF`).
/// A standard 8-bit or 16-bit bus cycle takes 4 CPU clocks at zero wait states,
/// and a 32-bit access is split into two 16-bit bus cycles (high word first, then low word).
pub trait Bus {
    /// Reads an 8-bit byte from `addr`.
    fn read_u8(&mut self, addr: u32, supervisor: bool) -> Result<u8, BusFault>;

    /// Reads a 16-bit big-endian word from `addr`.
    fn read_u16(&mut self, addr: u32, supervisor: bool) -> Result<u16, BusFault>;

    /// Reads a 32-bit big-endian longword from `addr` as two 16-bit bus cycles.
    #[inline]
    fn read_u32(&mut self, addr: u32, supervisor: bool) -> Result<u32, BusFault> {
        let hi = self.read_u16(addr, supervisor)? as u32;
        let lo = self.read_u16(addr.wrapping_add(2), supervisor)? as u32;
        Ok((hi << 16) | lo)
    }

    /// Writes an 8-bit byte to `addr`.
    fn write_u8(&mut self, addr: u32, val: u8, supervisor: bool) -> Result<(), BusFault>;

    /// Writes a 16-bit big-endian word to `addr`.
    fn write_u16(&mut self, addr: u32, val: u16, supervisor: bool) -> Result<(), BusFault>;

    /// Writes a 32-bit big-endian longword to `addr` as two 16-bit bus cycles.
    #[inline]
    fn write_u32(&mut self, addr: u32, val: u32, supervisor: bool) -> Result<(), BusFault> {
        self.write_u16(addr, (val >> 16) as u16, supervisor)?;
        self.write_u16(addr.wrapping_add(2), val as u16, supervisor)
    }

    /// Returns the number of extra CPU wait cycles (beyond the base 4 CPU clocks)
    /// incurred by a single 8-bit or 16-bit bus transfer at `addr`.
    ///
    /// Called only when [`crate::policy::CpuPolicy::TRACK_CYCLES`] is `true`.
    #[inline]
    fn wait_cycles_16(&mut self, _addr: u32, _is_write: bool) -> u32 {
        0
    }

    /// Returns the MC68030 bus cycle parameters of `addr`.
    ///
    /// Called only by a cycle-tracked MC68030 policy.
    #[inline]
    fn bus_cycle_030(&mut self, _addr: u32) -> BusCycle030 {
        BusCycle030::default()
    }

    /// Asserts the hardware `RESET` line to external peripherals (invoked by `RESET` opcode).
    fn reset_peripherals(&mut self) {}

    /// Acknowledges an interrupt at priority `level` (1..=7).
    /// Returns `Some(vector_number)` for vectored interrupts, or `None` for autovector (`24 + level`).
    fn acknowledge_interrupt(&mut self, _level: u8) -> Option<u8> {
        None
    }
}
