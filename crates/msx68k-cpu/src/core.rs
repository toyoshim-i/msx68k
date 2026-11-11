//! Monomorphized MC68000 / MC68030 instruction decoder and execution engine.

use std::marker::PhantomData;

use crate::alu::{
    alu_abcd, alu_add, alu_addx, alu_sbcd, alu_shift_rotate, alu_sub, alu_subx, eval_cc,
    muls_cycles, mulu_cycles, set_nz_clear_vc,
};
use crate::bus::{Bus, BusFault, TrapHandler, TrapResult};
use crate::policy::CpuPolicy;
use crate::timing030::{Ea, Full, Trace, access_wait, cacr};
use crate::state::{
    CpuState, DataSize, ExceptionVector, SR_C, SR_CCR_MASK, SR_M, SR_N, SR_S, SR_T, SR_T0, SR_V,
    SR_X, SR_Z,
};

/// Resolved Effective Address (EA) operand location.
#[derive(Debug, Copy, Clone)]
enum EaLocation {
    DataReg(usize),
    AddrReg(usize),
    Memory(u32),
    Immediate(u32),
}

/// Outcome of a single [`CpuCore::step`] invocation.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    /// CPU cycles consumed during this step (`0` when running under [`crate::policy::BurstPolicy`]).
    pub cycles: u32,
    /// `true` if the CPU is currently halted in `STOP` state.
    pub stopped: bool,
}

/// Monomorphized MC68000 CPU core parameterized by [`CpuPolicy`].
#[derive(Debug, Clone)]
pub struct CpuCore<P: CpuPolicy> {
    pub state: CpuState,
    step_cycles: u32,
    /// What the current instruction did, for MC68030 cycle accounting.
    trace: Trace,
    _policy: PhantomData<P>,
}

impl<P: CpuPolicy> Default for CpuCore<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: CpuPolicy> CpuCore<P> {
    /// Creates a new MC68000 CPU core with default reset state.
    pub const fn new() -> Self {
        Self {
            state: CpuState::new(),
            step_cycles: 0,
            trace: Trace::new(),
            _policy: PhantomData,
        }
    }

    /// Creates an MC68000 CPU core wrapping an existing [`CpuState`].
    pub const fn from_state(state: CpuState) -> Self {
        Self {
            state,
            step_cycles: 0,
            trace: Trace::new(),
            _policy: PhantomData,
        }
    }

    /// Consumes the core and returns the inner [`CpuState`].
    pub const fn into_state(self) -> CpuState {
        self.state
    }

    /// Whether MC68030 cycles are tracked; the MC68000 cycle counts are then ignored.
    const TRACK_030: bool = P::MC68030 && P::TRACK_CYCLES;

    #[inline(always)]
    fn add_cycles(&mut self, cycles: u32) {
        if P::TRACK_CYCLES && !P::MC68030 {
            self.step_cycles = self.step_cycles.wrapping_add(cycles);
        }
    }

    /// Performs a hardware reset by loading initial `SSP` from `$000000` and `PC` from `$000004`.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) -> Result<u32, BusFault> {
        self.step_cycles = 0;
        self.state.stopped = false;
        self.state.nmi_pending = false;
        self.state.sr = SR_S | 0x0700;
        self.state.vbr = 0;
        self.state.cacr = 0;
        self.state.mmu.tc = 0;
        self.state.mmu.tt0 = 0;
        self.state.mmu.tt1 = 0;
        let ssp = self.bus_read_u32(bus, 0)?;
        let pc = self.bus_read_u32(bus, 4)?;
        self.state.a[7] = ssp;
        self.state.ssp = ssp;
        self.state.pc = pc;
        self.state.ir_pc = pc;
        self.add_cycles(40);
        if P::TRACK_CYCLES {
            self.state.total_cycles = self.state.total_cycles.wrapping_add(self.step_cycles as u64);
        }
        Ok(self.step_cycles)
    }

    /// Records an MC68030 data access for cycle accounting and updates the data cache tags.
    fn note_access_030<B: Bus>(&mut self, bus: &mut B, addr: u32, size: DataSize, is_write: bool) {
        let cycle = bus.bus_cycle_030(addr);
        let supervisor = self.state.is_supervisor();
        let control = self.state.cacr;
        let cache = &mut self.state.timing030.dcache;
        let enabled = cycle.cacheable && (control & cacr::ED) != 0;
        let can_fill = enabled && (control & cacr::FD) == 0;
        let hit = enabled && cache.hit(addr, supervisor);
        if !hit && can_fill && (!is_write || (control & cacr::WA) != 0) {
            cache.fill(addr, supervisor);
        }
        // Writes go through to memory even when they hit.
        let cached = hit && !is_write;
        let wait = if cached {
            0
        } else {
            access_wait(cycle.wait, cycle.port16, size)
        };
        self.trace.record_access(is_write, wait, cached);
    }

    /// Accounts an MC68030 instruction prefetch of the long word holding `pc`.
    fn note_fetch_030<B: Bus>(&mut self, bus: &mut B, pc: u32) {
        let long = pc & !3;
        let timing = &mut self.state.timing030;
        if timing.holding == long {
            return;
        }
        timing.holding = long;
        let cycle = bus.bus_cycle_030(long);
        let supervisor = (self.state.sr & SR_S) != 0;
        let enabled = cycle.cacheable && (self.state.cacr & cacr::EI) != 0;
        if enabled && timing.icache.hit(long, supervisor) {
            return;
        }
        if enabled && (self.state.cacr & cacr::FI) == 0 {
            timing.icache.fill(long, supervisor);
        }
        self.trace.fetch_misses += 1;
        self.trace.fetch_wait = access_wait(cycle.wait, cycle.port16, DataSize::Long);
    }

    #[inline(always)]
    fn bus_read_u8<B: Bus>(&mut self, bus: &mut B, addr: u32) -> Result<u8, BusFault> {
        if Self::TRACK_030 {
            self.note_access_030(bus, addr, DataSize::Byte, false);
        } else if P::TRACK_CYCLES {
            let w = bus.wait_cycles_16(addr, false);
            self.add_cycles(w);
        }
        bus.read_u8(addr, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_read_u16<B: Bus>(&mut self, bus: &mut B, addr: u32) -> Result<u16, BusFault> {
        if Self::TRACK_030 {
            self.note_access_030(bus, addr, DataSize::Word, false);
        }
        if P::MC68030 && (addr & 1) != 0 {
            // The MC68030 splits misaligned operands into several bus cycles.
            let hi = bus.read_u8(addr, self.state.is_supervisor())? as u16;
            let lo = bus.read_u8(addr.wrapping_add(1), self.state.is_supervisor())? as u16;
            return Ok((hi << 8) | lo);
        }
        if !P::MC68030 && self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: false,
            });
        }
        if P::TRACK_CYCLES && !P::MC68030 {
            let w = bus.wait_cycles_16(addr, false);
            self.add_cycles(w);
        }
        bus.read_u16(addr, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_read_u32<B: Bus>(&mut self, bus: &mut B, addr: u32) -> Result<u32, BusFault> {
        if Self::TRACK_030 {
            self.note_access_030(bus, addr, DataSize::Long, false);
        }
        if P::MC68030 && (addr & 1) != 0 {
            let hi = self.bus_read_u16(bus, addr)? as u32;
            let lo = self.bus_read_u16(bus, addr.wrapping_add(2))? as u32;
            return Ok((hi << 16) | lo);
        }
        if !P::MC68030 && self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: false,
            });
        }
        if P::TRACK_CYCLES && !P::MC68030 {
            let w0 = bus.wait_cycles_16(addr, false);
            let w1 = bus.wait_cycles_16(addr.wrapping_add(2), false);
            self.add_cycles(w0 + w1);
        }
        bus.read_u32(addr, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_write_u8<B: Bus>(&mut self, bus: &mut B, addr: u32, val: u8) -> Result<(), BusFault> {
        if Self::TRACK_030 {
            self.note_access_030(bus, addr, DataSize::Byte, true);
        } else if P::TRACK_CYCLES {
            let w = bus.wait_cycles_16(addr, true);
            self.add_cycles(w);
        }
        bus.write_u8(addr, val, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_write_u16<B: Bus>(&mut self, bus: &mut B, addr: u32, val: u16) -> Result<(), BusFault> {
        if Self::TRACK_030 {
            self.note_access_030(bus, addr, DataSize::Word, true);
        }
        if P::MC68030 && (addr & 1) != 0 {
            let supervisor = self.state.is_supervisor();
            bus.write_u8(addr, (val >> 8) as u8, supervisor)?;
            return bus.write_u8(addr.wrapping_add(1), val as u8, supervisor);
        }
        if !P::MC68030 && self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: true,
            });
        }
        if P::TRACK_CYCLES && !P::MC68030 {
            let w = bus.wait_cycles_16(addr, true);
            self.add_cycles(w);
        }
        bus.write_u16(addr, val, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_write_u32<B: Bus>(&mut self, bus: &mut B, addr: u32, val: u32) -> Result<(), BusFault> {
        if Self::TRACK_030 {
            self.note_access_030(bus, addr, DataSize::Long, true);
        }
        if P::MC68030 && (addr & 1) != 0 {
            self.bus_write_u16(bus, addr, (val >> 16) as u16)?;
            return self.bus_write_u16(bus, addr.wrapping_add(2), val as u16);
        }
        if !P::MC68030 && self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: true,
            });
        }
        if P::TRACK_CYCLES && !P::MC68030 {
            let w0 = bus.wait_cycles_16(addr, true);
            let w1 = bus.wait_cycles_16(addr.wrapping_add(2), true);
            self.add_cycles(w0 + w1);
        }
        bus.write_u32(addr, val, self.state.is_supervisor())
    }

    #[inline(always)]
    fn fetch_u16<B: Bus>(&mut self, bus: &mut B) -> Result<u16, BusFault> {
        let pc = self.state.pc;
        if P::MC68030 && (pc & 1) != 0 {
            // Unlike data, instructions must stay word aligned.
            return Err(BusFault::AddressError {
                addr: pc,
                is_write: false,
            });
        }
        if Self::TRACK_030 {
            // Prefetches are accounted per long word, not as data accesses.
            self.note_fetch_030(bus, pc);
            let word = bus.read_u16(pc, self.state.is_supervisor())?;
            self.state.pc = pc.wrapping_add(2);
            return Ok(word);
        }
        let word = self.bus_read_u16(bus, pc)?;
        self.state.pc = pc.wrapping_add(2);
        Ok(word)
    }

    #[inline(always)]
    fn fetch_u32<B: Bus>(&mut self, bus: &mut B) -> Result<u32, BusFault> {
        let hi = self.fetch_u16(bus)? as u32;
        let lo = self.fetch_u16(bus)? as u32;
        Ok((hi << 16) | lo)
    }

    #[inline(always)]
    fn push_u16<B: Bus>(&mut self, bus: &mut B, val: u16) -> Result<(), BusFault> {
        let sp = self.state.a[7].wrapping_sub(2);
        self.state.a[7] = sp;
        self.bus_write_u16(bus, sp, val)
    }

    #[inline(always)]
    fn push_u32<B: Bus>(&mut self, bus: &mut B, val: u32) -> Result<(), BusFault> {
        let sp = self.state.a[7].wrapping_sub(4);
        self.state.a[7] = sp;
        self.bus_write_u32(bus, sp, val)
    }

    #[inline(always)]
    fn pop_u16<B: Bus>(&mut self, bus: &mut B) -> Result<u16, BusFault> {
        let sp = self.state.a[7];
        let val = self.bus_read_u16(bus, sp)?;
        self.state.a[7] = sp.wrapping_add(2);
        Ok(val)
    }

    #[inline(always)]
    fn pop_u32<B: Bus>(&mut self, bus: &mut B) -> Result<u32, BusFault> {
        let sp = self.state.a[7];
        let val = self.bus_read_u32(bus, sp)?;
        self.state.a[7] = sp.wrapping_add(4);
        Ok(val)
    }

    /// Returns the address of exception vector `vector_no` (relative to `VBR` on the MC68030).
    #[inline(always)]
    fn vector_addr(&self, vector_no: u8) -> u32 {
        let offset = (vector_no as u32) * 4;
        if P::MC68030 {
            self.state.vbr.wrapping_add(offset)
        } else {
            offset
        }
    }

    /// Pushes a short exception frame: `SR` and `PC`, plus the format/vector word on the MC68030.
    fn push_frame<B: Bus>(
        &mut self,
        bus: &mut B,
        format: u16,
        vector_no: u8,
        pc: u32,
        sr: u16,
    ) -> Result<(), BusFault> {
        if P::MC68030 {
            self.push_u16(bus, (format << 12) | ((vector_no as u16) * 4))?;
        }
        self.push_u32(bus, pc)?;
        self.push_u16(bus, sr)
    }

    /// Checks and services pending hardware interrupts.
    fn check_interrupts<B: Bus>(&mut self, bus: &mut B) -> Result<bool, BusFault> {
        let ipl = self.state.pending_ipl;
        if ipl == 0 {
            return Ok(false);
        }
        let mask = self.state.interrupt_mask();
        let take = if ipl == 7 {
            self.state.nmi_pending || mask < 7
        } else {
            ipl > mask
        };
        if !take {
            return Ok(false);
        }

        if ipl == 7 {
            self.state.nmi_pending = false;
        }
        self.state.stopped = false;

        let saved_sr = self.state.sr;
        let new_sr = (saved_sr & !(SR_T | SR_T0 | 0x0700)) | SR_S | ((ipl as u16) << 8);
        self.state.set_sr(new_sr);

        let vec_no = bus.acknowledge_interrupt(ipl).unwrap_or(24 + ipl);
        self.trace.interrupt = Some(P::MC68030 && (new_sr & SR_M) != 0);
        self.push_frame(bus, 0, vec_no, self.state.pc, saved_sr)?;
        if P::MC68030 && (new_sr & SR_M) != 0 {
            // Interrupts run on the interrupt stack, leaving a throwaway frame there that
            // returns to the master stack.
            self.state.set_sr(new_sr & !SR_M);
            self.push_frame(bus, 1, vec_no, self.state.pc, new_sr)?;
        }
        let handler = self.bus_read_u32(bus, self.vector_addr(vec_no))?;
        self.state.pc = handler;
        self.add_cycles(44);
        Ok(true)
    }

    /// Raises a standard Group 1/2 exception (frame: `SR`, `PC`, and on the MC68030 the
    /// format/vector word, plus the instruction address for format `$2` exceptions).
    pub fn raise_exception<B: Bus>(
        &mut self,
        bus: &mut B,
        vector_no: u8,
        return_pc: u32,
        base_cycles: u32,
    ) -> Result<(), BusFault> {
        self.state.stopped = false;
        self.trace.push_exception(vector_no);
        let saved_sr = self.state.sr;
        self.state.set_sr((saved_sr & !(SR_T | SR_T0)) | SR_S);
        let format = if P::MC68030
            && matches!(
                vector_no,
                v if v == ExceptionVector::ZeroDivide as u8
                    || v == ExceptionVector::ChkInstruction as u8
                    || v == ExceptionVector::TrapvInstruction as u8
                    || v == ExceptionVector::Trace as u8
            ) {
            self.push_u32(bus, self.state.ir_pc)?;
            2
        } else {
            0
        };
        self.push_frame(bus, format, vector_no, return_pc, saved_sr)?;
        let target = self.bus_read_u32(bus, self.vector_addr(vector_no))?;
        self.state.pc = target;
        self.add_cycles(base_cycles);
        Ok(())
    }

    /// Raises a Group 0 fault (Bus Error or Address Error, 7-word MC68000 stack frame).
    fn raise_group0_fault<B: Bus>(&mut self, bus: &mut B, fault: BusFault) {
        self.state.stopped = false;
        self.trace.bus_fault = true;
        let saved_sr = self.state.sr;
        self.state.set_sr((saved_sr & !SR_T) | SR_S);

        let (vec_no, fault_addr, is_write) = match fault {
            BusFault::BusError { addr, is_write } => (ExceptionVector::BusError as u8, addr, is_write),
            BusFault::AddressError { addr, is_write } => {
                (ExceptionVector::AddressError as u8, addr, is_write)
            }
        };

        let fc = if (saved_sr & SR_S) != 0 { 5u16 } else { 1u16 };
        if P::MC68030 {
            self.push_long_bus_fault_frame(bus, vec_no, saved_sr, fault_addr, is_write, fc);
            if let Ok(target) = self.bus_read_u32(bus, self.vector_addr(vec_no)) {
                self.state.pc = target;
            }
            return;
        }

        let rw_bit = if is_write { 0 } else { 1 << 4 };
        let status_word = (self.state.ir & 0xFFE0) | rw_bit | fc;

        // Ensure SP is even before pushing the Group 0 frame so we do not double-fault.
        self.state.a[7] &= !1;
        let _ = self.push_u32(bus, self.state.pc);
        let _ = self.push_u16(bus, saved_sr);
        let _ = self.push_u16(bus, self.state.ir);
        let _ = self.push_u32(bus, fault_addr);
        let _ = self.push_u16(bus, status_word);
        if let Ok(target) = self.bus_read_u32(bus, (vec_no as u32) * 4) {
            self.state.pc = target;
        }
        self.add_cycles(50);
    }

    /// Pushes an MC68030 long bus fault frame (format `$B`, 46 words).
    ///
    /// Only the fields software reads are filled in: `SR`, `PC` (the faulting instruction, so
    /// `RTE` restarts it), the format/vector word, the special status word, and the fault address.
    fn push_long_bus_fault_frame<B: Bus>(
        &mut self,
        bus: &mut B,
        vector_no: u8,
        sr: u16,
        fault_addr: u32,
        is_write: bool,
        fc: u16,
    ) {
        const FRAME_BYTES: u32 = 92;
        const SSW_DF: u16 = 0x0100;
        const SSW_RW: u16 = 0x0040;
        let sp = (self.state.a[7] & !1).wrapping_sub(FRAME_BYTES);
        self.state.a[7] = sp;
        for offset in (0..FRAME_BYTES).step_by(4) {
            let _ = self.bus_write_u32(bus, sp + offset, 0);
        }
        let ssw = SSW_DF | if is_write { 0 } else { SSW_RW } | fc;
        let _ = self.bus_write_u16(bus, sp, sr);
        let _ = self.bus_write_u32(bus, sp + 2, self.state.ir_pc);
        let _ = self.bus_write_u16(bus, sp + 6, 0xB000 | ((vector_no as u16) * 4));
        let _ = self.bus_write_u16(bus, sp + 10, ssw);
        let _ = self.bus_write_u32(bus, sp + 16, fault_addr);
        let _ = self.bus_write_u32(bus, sp + 36, self.state.ir_pc.wrapping_add(4));
    }

    /// Returns from an exception, discarding the extra words of MC68030 frames.
    fn exec_rte<B: Bus>(&mut self, bus: &mut B) -> Result<(), BusFault> {
        loop {
            let new_sr = self.pop_u16(bus)?;
            let new_pc = self.pop_u32(bus)?;
            if P::MC68030 {
                let format = self.pop_u16(bus)? >> 12;
                self.trace.push_rte_format(format as u8);
                let extra = match format {
                    0 | 1 => 0,
                    2 => 4,
                    9 => 12,
                    0xA => 24,
                    0xB => 84,
                    _ => {
                        self.state.a[7] = self.state.a[7].wrapping_sub(8);
                        return self.raise_exception(
                            bus,
                            ExceptionVector::FormatError as u8,
                            self.state.ir_pc,
                            34,
                        );
                    }
                };
                self.state.a[7] = self.state.a[7].wrapping_add(extra);
                self.state.set_sr(new_sr);
                if format == 1 {
                    // A throwaway frame: continue with the frame on the newly selected stack.
                    continue;
                }
            } else {
                self.state.set_sr(new_sr);
            }
            self.state.pc = new_pc;
            self.add_cycles(20);
            return Ok(());
        }
    }

    fn illegal<B: Bus>(&mut self, bus: &mut B) -> Result<(), BusFault> {
        self.raise_exception(
            bus,
            ExceptionVector::IllegalInstruction as u8,
            self.state.ir_pc,
            34,
        )
    }

    fn privilege_violation<B: Bus>(&mut self, bus: &mut B) -> Result<(), BusFault> {
        self.raise_exception(
            bus,
            ExceptionVector::PrivilegeViolation as u8,
            self.state.ir_pc,
            34,
        )
    }

    /// Executes a single instruction (or interrupt/stop cycle).
    pub fn step<B: Bus, T: TrapHandler<B>>(
        &mut self,
        bus: &mut B,
        trap_handler: &mut T,
    ) -> StepOutcome {
        self.step_cycles = 0;
        if Self::TRACK_030 {
            self.trace = Trace::new();
        }

        match self.check_interrupts(bus) {
            Ok(true) => return self.finish_step(),
            Err(fault) => {
                self.raise_group0_fault(bus, fault);
                return self.finish_step();
            }
            Ok(false) => {}
        }

        if self.state.stopped {
            if Self::TRACK_030 {
                self.step_cycles = 4;
                self.state.total_cycles = self.state.total_cycles.wrapping_add(4);
            } else {
                self.add_cycles(4);
                if P::TRACK_CYCLES {
                    self.state.total_cycles =
                        self.state.total_cycles.wrapping_add(self.step_cycles as u64);
                }
            }
            return StepOutcome {
                cycles: self.step_cycles,
                stopped: true,
            };
        }

        let trace_before = self.state.trace_enabled && self.state.flag(SR_T);
        self.state.ir_pc = self.state.pc;

        if let Err(fault) = self.execute_one(bus, trap_handler) {
            self.raise_group0_fault(bus, fault);
        } else if trace_before && !self.state.stopped {
            if let Err(fault) =
                self.raise_exception(bus, ExceptionVector::Trace as u8, self.state.pc, 34)
            {
                self.raise_group0_fault(bus, fault);
            }
        }

        self.finish_step()
    }

    /// Totals the cycles of the step that just ran.
    fn finish_step(&mut self) -> StepOutcome {
        if Self::TRACK_030 {
            let zero = self.state.flag(SR_Z);
            self.step_cycles = self.state.timing030.evaluate(self.state.ir, &self.trace, zero);
        }
        if P::TRACK_CYCLES {
            self.state.total_cycles = self.state.total_cycles.wrapping_add(self.step_cycles as u64);
        }
        StepOutcome {
            cycles: self.step_cycles,
            stopped: self.state.stopped,
        }
    }

    /// Computes the Effective Address (EA) and adds standard Motorola EA calculation cycles.
    fn resolve_ea<B: Bus>(
        &mut self,
        bus: &mut B,
        mode: u8,
        reg: u8,
        size: DataSize,
    ) -> Result<EaLocation, BusFault> {
        if Self::TRACK_030 {
            let ea = match (mode & 7, reg & 7) {
                (0, _) => Some(Ea::Dn),
                (1, _) => Some(Ea::An),
                (2, _) => Some(Ea::Ind),
                (3, _) => Some(Ea::PostInc),
                (4, _) => Some(Ea::PreDec),
                (5, _) | (7, 2) => Some(Ea::Disp16),
                (7, 0) => Some(Ea::AbsW),
                (7, 1) => Some(Ea::AbsL),
                (7, 4) => Some(Ea::Imm {
                    long: size == DataSize::Long,
                }),
                // Indexed modes are recorded once their extension word is known.
                _ => None,
            };
            if let Some(ea) = ea {
                self.trace.push_ea(ea);
            }
        }
        let r = (reg & 7) as usize;
        match mode & 7 {
            0 => Ok(EaLocation::DataReg(r)),
            1 => Ok(EaLocation::AddrReg(r)),
            2 => {
                self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
                Ok(EaLocation::Memory(self.state.a[r]))
            }
            3 => {
                let addr = self.state.a[r];
                let step = if r == 7 && size == DataSize::Byte {
                    2
                } else {
                    size.bytes()
                };
                self.state.a[r] = addr.wrapping_add(step);
                self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
                Ok(EaLocation::Memory(addr))
            }
            4 => {
                let step = if r == 7 && size == DataSize::Byte {
                    2
                } else {
                    size.bytes()
                };
                let addr = self.state.a[r].wrapping_sub(step);
                self.state.a[r] = addr;
                self.add_cycles(if size == DataSize::Long { 10 } else { 6 });
                Ok(EaLocation::Memory(addr))
            }
            5 => {
                let disp = self.fetch_u16(bus)? as i16 as i32;
                let addr = self.state.a[r].wrapping_add(disp as u32);
                self.add_cycles(if size == DataSize::Long { 12 } else { 8 });
                Ok(EaLocation::Memory(addr))
            }
            6 => {
                let ext = self.fetch_u16(bus)?;
                let addr = self.calc_index_ext(bus, self.state.a[r], ext)?;
                self.add_cycles(if size == DataSize::Long { 14 } else { 10 });
                Ok(EaLocation::Memory(addr))
            }
            7 => match r {
                0 => {
                    let addr = (self.fetch_u16(bus)? as i16 as i32) as u32;
                    self.add_cycles(if size == DataSize::Long { 12 } else { 8 });
                    Ok(EaLocation::Memory(addr))
                }
                1 => {
                    let addr = self.fetch_u32(bus)?;
                    self.add_cycles(if size == DataSize::Long { 16 } else { 12 });
                    Ok(EaLocation::Memory(addr))
                }
                2 => {
                    let base_pc = self.state.pc;
                    let disp = self.fetch_u16(bus)? as i16 as i32;
                    self.add_cycles(if size == DataSize::Long { 12 } else { 8 });
                    Ok(EaLocation::Memory(base_pc.wrapping_add(disp as u32)))
                }
                3 => {
                    let base_pc = self.state.pc;
                    let ext = self.fetch_u16(bus)?;
                    let addr = self.calc_index_ext(bus, base_pc, ext)?;
                    self.add_cycles(if size == DataSize::Long { 14 } else { 10 });
                    Ok(EaLocation::Memory(addr))
                }
                4 => {
                    let val = match size {
                        DataSize::Byte => (self.fetch_u16(bus)? & 0xFF) as u32,
                        DataSize::Word => self.fetch_u16(bus)? as u32,
                        DataSize::Long => self.fetch_u32(bus)?,
                    };
                    self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
                    Ok(EaLocation::Immediate(val))
                }
                _ => {
                    self.raise_exception(
                        bus,
                        ExceptionVector::IllegalInstruction as u8,
                        self.state.ir_pc,
                        34,
                    )?;
                    Ok(EaLocation::Immediate(0))
                }
            },
            _ => unreachable!(),
        }
    }

    /// Computes an indexed address from extension word `ext`, which follows the opcode.
    ///
    /// The MC68000 only knows the brief format. The MC68030 also scales the index and has the
    /// full format (bit 8), with base and index suppression, 16/32-bit base displacements, and
    /// memory indirection with an outer displacement.
    fn calc_index_ext<B: Bus>(&mut self, bus: &mut B, base: u32, ext: u16) -> Result<u32, BusFault> {
        let idx_reg = ((ext >> 12) & 7) as usize;
        let is_addr = (ext & 0x8000) != 0;
        let is_long = (ext & 0x0800) != 0;
        let raw_idx = if is_addr {
            self.state.a[idx_reg]
        } else {
            self.state.d[idx_reg]
        };
        let mut index = if is_long {
            raw_idx
        } else {
            (raw_idx as u16 as i16) as i32 as u32
        };
        if !P::MC68030 {
            let disp = (ext as u8 as i8) as i32;
            return Ok(base.wrapping_add(disp as u32).wrapping_add(index));
        }
        index <<= (ext >> 9) & 3;
        if (ext & 0x0100) == 0 {
            if Self::TRACK_030 {
                self.trace.push_ea(Ea::Brief);
            }
            let disp = (ext as u8 as i8) as i32;
            return Ok(base.wrapping_add(disp as u32).wrapping_add(index));
        }
        if Self::TRACK_030 {
            let size = |bits: u16| match bits {
                2 => 1,
                3 => 2,
                _ => 0,
            };
            self.trace.push_ea(Ea::Full(Full {
                base_disp: size((ext >> 4) & 3),
                indirect: (ext & 7) != 0,
                outer_disp: size(ext & 3),
            }));
        }

        let base = if (ext & 0x0080) != 0 { 0 } else { base };
        let index_suppressed = (ext & 0x0040) != 0;
        if index_suppressed {
            index = 0;
        }
        let base_disp = match (ext >> 4) & 3 {
            2 => self.fetch_u16(bus)? as i16 as i32 as u32,
            3 => self.fetch_u32(bus)?,
            _ => 0,
        };
        let indirection = ext & 7;
        if indirection == 0 {
            return Ok(base.wrapping_add(base_disp).wrapping_add(index));
        }
        let outer_disp = match indirection & 3 {
            2 => self.fetch_u16(bus)? as i16 as i32 as u32,
            3 => self.fetch_u32(bus)?,
            _ => 0,
        };
        let pre_indexed = index_suppressed || (indirection & 4) == 0;
        let ptr_addr = if pre_indexed {
            // ([bd,An,Xn],od), or no index
            base.wrapping_add(base_disp).wrapping_add(index)
        } else {
            // ([bd,An],Xn,od)
            base.wrapping_add(base_disp)
        };
        self.trace.fetching_pointer = true;
        let ptr = self.bus_read_u32(bus, ptr_addr);
        self.trace.fetching_pointer = false;
        let ptr = ptr?;
        if pre_indexed {
            Ok(ptr.wrapping_add(outer_disp))
        } else {
            Ok(ptr.wrapping_add(index).wrapping_add(outer_disp))
        }
    }

    fn read_ea<B: Bus>(
        &mut self,
        bus: &mut B,
        loc: EaLocation,
        size: DataSize,
    ) -> Result<u32, BusFault> {
        match loc {
            EaLocation::DataReg(r) => Ok(self.state.read_d(r, size)),
            EaLocation::AddrReg(r) => Ok(self.state.a[r] & size.mask()),
            EaLocation::Immediate(v) => Ok(v & size.mask()),
            EaLocation::Memory(addr) => match size {
                DataSize::Byte => Ok(self.bus_read_u8(bus, addr)? as u32),
                DataSize::Word => Ok(self.bus_read_u16(bus, addr)? as u32),
                DataSize::Long => self.bus_read_u32(bus, addr),
            },
        }
    }

    fn write_ea<B: Bus>(
        &mut self,
        bus: &mut B,
        loc: EaLocation,
        size: DataSize,
        val: u32,
    ) -> Result<(), BusFault> {
        match loc {
            EaLocation::DataReg(r) => {
                self.state.write_d(r, size, val);
                Ok(())
            }
            EaLocation::AddrReg(r) => {
                self.state.a[r] = match size {
                    DataSize::Word => (val as u16 as i16 as i32) as u32,
                    _ => val,
                };
                Ok(())
            }
            EaLocation::Memory(addr) => match size {
                DataSize::Byte => self.bus_write_u8(bus, addr, val as u8),
                DataSize::Word => self.bus_write_u16(bus, addr, val as u16),
                DataSize::Long => self.bus_write_u32(bus, addr, val),
            },
            EaLocation::Immediate(_) => Ok(()),
        }
    }

    /// Decodes and executes a single instruction at `self.state.pc`.
    fn execute_one<B: Bus, T: TrapHandler<B>>(
        &mut self,
        bus: &mut B,
        trap_handler: &mut T,
    ) -> Result<(), BusFault> {
        let op = self.fetch_u16(bus)?;
        self.state.ir = op;

        match (op >> 12) & 0xF {
            0x0 => self.exec_group_0(bus, op),
            0x1 => self.exec_move(bus, op, DataSize::Byte),
            0x2 => self.exec_move(bus, op, DataSize::Long),
            0x3 => self.exec_move(bus, op, DataSize::Word),
            0x4 => self.exec_group_4(bus, trap_handler, op),
            0x5 => self.exec_group_5(bus, op),
            0x6 => self.exec_group_6(bus, op),
            0x7 => {
                // MOVEQ #<data>, Dn
                let reg = ((op >> 9) & 7) as usize;
                let imm = (op as u8 as i8 as i32) as u32;
                self.state.d[reg] = imm;
                set_nz_clear_vc(&mut self.state, DataSize::Long, imm);
                self.add_cycles(4);
                Ok(())
            }
            0x8 => self.exec_group_8(bus, op),
            0x9 => self.exec_add_sub(bus, op, false),
            0xA => {
                // A-Line ($Axxx) Mixed Mode / Unimplemented Trap
                let ir_pc = self.state.ir_pc;
                match trap_handler.on_a_line(&mut self.state, bus, op, ir_pc) {
                    TrapResult::Handled { extra_cycles } => {
                        self.trace.native = Some(extra_cycles);
                        self.add_cycles(4 + extra_cycles);
                        Ok(())
                    }
                    TrapResult::PassToVector | TrapResult::Unhandled => self.raise_exception(
                        bus,
                        ExceptionVector::Line1010Emulator as u8,
                        self.state.ir_pc,
                        34,
                    ),
                }
            }
            0xB => self.exec_group_b(bus, op),
            0xC => self.exec_group_c(bus, op),
            0xD => self.exec_add_sub(bus, op, true),
            0xE => self.exec_group_e(bus, op),
            0xF if P::MC68030 && (op & 0xFFC0) == 0xF000 => self.exec_mmu(bus, op),
            0xF => {
                // F-Line ($Fxxx) Human68k DOS call / SX-Window API / FE-function
                let ir_pc = self.state.ir_pc;
                match trap_handler.on_f_line(&mut self.state, bus, op, ir_pc) {
                    TrapResult::Handled { extra_cycles } => {
                        self.trace.native = Some(extra_cycles);
                        self.add_cycles(4 + extra_cycles);
                        Ok(())
                    }
                    TrapResult::PassToVector | TrapResult::Unhandled => self.raise_exception(
                        bus,
                        ExceptionVector::Line1111Emulator as u8,
                        self.state.ir_pc,
                        34,
                    ),
                }
            }
            _ => unreachable!(),
        }
    }

    fn exec_move<B: Bus>(&mut self, bus: &mut B, op: u16, size: DataSize) -> Result<(), BusFault> {
        let src_mode = ((op >> 3) & 7) as u8;
        let src_reg = (op & 7) as u8;
        let dst_reg = ((op >> 9) & 7) as u8;
        let dst_mode = ((op >> 6) & 7) as u8;

        let src_loc = self.resolve_ea(bus, src_mode, src_reg, size)?;
        let val = self.read_ea(bus, src_loc, size)?;

        if dst_mode == 1 {
            // MOVEA.W / MOVEA.L (does not affect flags)
            let extended = if size == DataSize::Word {
                (val as u16 as i16 as i32) as u32
            } else {
                val
            };
            self.state.a[dst_reg as usize] = extended;
            self.add_cycles(4);
            return Ok(());
        }

        // For -(An) destination on MOVE, MC68000 takes 2 fewer cycles than standard EA resolve
        let dst_loc = if dst_mode == 4 {
            if Self::TRACK_030 {
                self.trace.push_ea(Ea::PreDec);
            }
            let r = dst_reg as usize;
            let step = if r == 7 && size == DataSize::Byte {
                2
            } else {
                size.bytes()
            };
            let addr = self.state.a[r].wrapping_sub(step);
            self.state.a[r] = addr;
            self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
            EaLocation::Memory(addr)
        } else {
            self.resolve_ea(bus, dst_mode, dst_reg, size)?
        };

        self.write_ea(bus, dst_loc, size, val)?;
        set_nz_clear_vc(&mut self.state, size, val);
        self.add_cycles(4);
        Ok(())
    }

    fn exec_group_0<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;

        // MOVEP
        if (op & 0x0138) == 0x0108 {
            let d_reg = ((op >> 9) & 7) as usize;
            let a_reg = (op & 7) as usize;
            let disp = self.fetch_u16(bus)? as i16 as i32;
            let addr = self.state.a[a_reg].wrapping_add(disp as u32);
            let opmode = (op >> 6) & 7;
            match opmode {
                4 => {
                    // MOVEP.W (d16,An), Dn
                    let b0 = self.bus_read_u8(bus, addr)? as u16;
                    let b1 = self.bus_read_u8(bus, addr.wrapping_add(2))? as u16;
                    self.state
                        .write_d(d_reg, DataSize::Word, ((b0 << 8) | b1) as u32);
                    self.add_cycles(16);
                }
                5 => {
                    // MOVEP.L (d16,An), Dn
                    let b0 = self.bus_read_u8(bus, addr)? as u32;
                    let b1 = self.bus_read_u8(bus, addr.wrapping_add(2))? as u32;
                    let b2 = self.bus_read_u8(bus, addr.wrapping_add(4))? as u32;
                    let b3 = self.bus_read_u8(bus, addr.wrapping_add(6))? as u32;
                    self.state.d[d_reg] = (b0 << 24) | (b1 << 16) | (b2 << 8) | b3;
                    self.add_cycles(24);
                }
                6 => {
                    // MOVEP.W Dn, (d16,An)
                    let v = self.state.d[d_reg];
                    self.bus_write_u8(bus, addr, (v >> 8) as u8)?;
                    self.bus_write_u8(bus, addr.wrapping_add(2), v as u8)?;
                    self.add_cycles(16);
                }
                7 => {
                    // MOVEP.L Dn, (d16,An)
                    let v = self.state.d[d_reg];
                    self.bus_write_u8(bus, addr, (v >> 24) as u8)?;
                    self.bus_write_u8(bus, addr.wrapping_add(2), (v >> 16) as u8)?;
                    self.bus_write_u8(bus, addr.wrapping_add(4), (v >> 8) as u8)?;
                    self.bus_write_u8(bus, addr.wrapping_add(6), v as u8)?;
                    self.add_cycles(24);
                }
                _ => unreachable!(),
            }
            return Ok(());
        }

        // Dynamic bit operations (BTST/BCHG/BCLR/BSET Dn, <ea>) or static (#<data>, <ea>)
        if (op & 0x0100) != 0 || (op & 0x0F00) == 0x0800 {
            let bit_num = if (op & 0x0100) != 0 {
                let d_reg = ((op >> 9) & 7) as usize;
                self.state.d[d_reg]
            } else {
                self.add_cycles(4);
                self.fetch_u16(bus)? as u32
            };
            let subop = (op >> 6) & 3;
            let is_reg = mode == 0;
            let size = if is_reg {
                DataSize::Long
            } else {
                DataSize::Byte
            };
            let bit_mask = 1u32 << (bit_num & if is_reg { 31 } else { 7 });
            let loc = self.resolve_ea(bus, mode, reg, size)?;
            let val = self.read_ea(bus, loc, size)?;
            self.state.set_flag(SR_Z, (val & bit_mask) == 0);

            match subop {
                0 => {
                    // BTST
                    self.add_cycles(if is_reg { 6 } else { 4 });
                }
                1 => {
                    // BCHG
                    self.write_ea(bus, loc, size, val ^ bit_mask)?;
                    self.add_cycles(8);
                }
                2 => {
                    // BCLR
                    self.write_ea(bus, loc, size, val & !bit_mask)?;
                    self.add_cycles(if is_reg { 10 } else { 8 });
                }
                3 => {
                    // BSET
                    self.write_ea(bus, loc, size, val | bit_mask)?;
                    self.add_cycles(8);
                }
                _ => unreachable!(),
            }
            return Ok(());
        }

        // ORI / ANDI / EORI to CCR or SR
        if op == 0x003C || op == 0x023C || op == 0x0A3C {
            let imm = (self.fetch_u16(bus)? & 0xFF) as u8;
            let ccr = self.state.ccr();
            let new_ccr = match (op >> 9) & 7 {
                0 => ccr | imm,
                1 => ccr & imm,
                5 => ccr ^ imm,
                _ => ccr,
            };
            self.state.set_ccr(new_ccr);
            self.add_cycles(20);
            return Ok(());
        }
        if op == 0x007C || op == 0x027C || op == 0x0A7C {
            if !self.state.is_supervisor() {
                return self.raise_exception(
                    bus,
                    ExceptionVector::PrivilegeViolation as u8,
                    self.state.ir_pc,
                    34,
                );
            }
            let imm = self.fetch_u16(bus)?;
            let sr = self.state.sr;
            let new_sr = match (op >> 9) & 7 {
                0 => sr | imm,
                1 => sr & imm,
                5 => sr ^ imm,
                _ => sr,
            };
            self.state.set_sr(new_sr);
            self.add_cycles(20);
            return Ok(());
        }

        if P::MC68030 {
            let size_bits = (op >> 9) & 3;
            if (op & 0xF9C0) == 0x00C0 && size_bits != 3 {
                return self.exec_cmp2(bus, op, size_bits);
            }
            if (op & 0xF9FF) == 0x08FC && size_bits >= 2 {
                return self.exec_cas2(bus, size_bits);
            }
            if (op & 0xF9C0) == 0x08C0 && size_bits != 0 {
                return self.exec_cas(bus, op, size_bits);
            }
            if (op & 0xFF00) == 0x0E00 {
                return self.exec_moves(bus, op);
            }
        }

        // Immediate operations: ORI (0), ANDI (1), SUBI (2), ADDI (3), EORI (5), CMPI (6)
        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            2 => DataSize::Long,
            _ => {
                return self.raise_exception(
                    bus,
                    ExceptionVector::IllegalInstruction as u8,
                    self.state.ir_pc,
                    34,
                );
            }
        };
        let imm = match size {
            DataSize::Byte => (self.fetch_u16(bus)? & 0xFF) as u32,
            DataSize::Word => self.fetch_u16(bus)? as u32,
            DataSize::Long => self.fetch_u32(bus)?,
        };
        let loc = self.resolve_ea(bus, mode, reg, size)?;
        let dst = self.read_ea(bus, loc, size)?;

        let kind = (op >> 9) & 7;
        match kind {
            0 => {
                let res = (dst | imm) & size.mask();
                self.write_ea(bus, loc, size, res)?;
                set_nz_clear_vc(&mut self.state, size, res);
            }
            1 => {
                let res = (dst & imm) & size.mask();
                self.write_ea(bus, loc, size, res)?;
                set_nz_clear_vc(&mut self.state, size, res);
            }
            2 => {
                let res = alu_sub(&mut self.state, size, imm, dst, true);
                self.write_ea(bus, loc, size, res)?;
            }
            3 => {
                let res = alu_add(&mut self.state, size, imm, dst);
                self.write_ea(bus, loc, size, res)?;
            }
            5 => {
                let res = (dst ^ imm) & size.mask();
                self.write_ea(bus, loc, size, res)?;
                set_nz_clear_vc(&mut self.state, size, res);
            }
            6 => {
                let _ = alu_sub(&mut self.state, size, imm, dst, false);
            }
            _ => {
                return self.raise_exception(
                    bus,
                    ExceptionVector::IllegalInstruction as u8,
                    self.state.ir_pc,
                    34,
                );
            }
        }
        let is_mem = matches!(loc, EaLocation::Memory(_));
        let base = if size == DataSize::Long {
            if is_mem && kind != 6 {
                20
            } else {
                16
            }
        } else if is_mem && kind != 6 {
            12
        } else {
            8
        };
        self.add_cycles(base);
        Ok(())
    }

    /// `CMP2` / `CHK2 <ea>, Rn` (MC68030).
    fn exec_cmp2<B: Bus>(&mut self, bus: &mut B, op: u16, size_bits: u16) -> Result<(), BusFault> {
        let size = match size_bits {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };
        let ext = self.fetch_u16(bus)?;
        self.trace.ext = ext;
        let mode = ((op >> 3) & 7) as u8;
        let EaLocation::Memory(addr) = self.resolve_ea(bus, mode, (op & 7) as u8, size)? else {
            return self.illegal(bus);
        };
        let lower = self.read_ea(bus, EaLocation::Memory(addr), size)?;
        let upper = self.read_ea(bus, EaLocation::Memory(addr.wrapping_add(size.bytes())), size)?;
        let reg = ((ext >> 12) & 7) as usize;
        // An address register is compared in full against sign-extended bounds.
        let (value, lower, upper) = if (ext & 0x8000) != 0 {
            (
                self.state.a[reg],
                size.sign_extend(lower) as u32,
                size.sign_extend(upper) as u32,
            )
        } else {
            (self.state.read_d(reg, size), lower, upper)
        };
        // Bounds in signed order (lower above upper as unsigned numbers) compare signed.
        let out_of_bounds = if lower <= upper {
            value < lower || value > upper
        } else {
            let wide = |v: u32| if (ext & 0x8000) != 0 { v as i32 } else { size.sign_extend(v) };
            wide(value) < wide(lower) || wide(value) > wide(upper)
        };
        self.state.set_flag(SR_Z, value == lower || value == upper);
        self.state.set_flag(SR_C, out_of_bounds);
        if out_of_bounds && (ext & 0x0800) != 0 {
            return self.raise_exception(
                bus,
                ExceptionVector::ChkInstruction as u8,
                self.state.pc,
                40,
            );
        }
        Ok(())
    }

    /// `CAS Dc, Du, <ea>` (MC68030).
    fn exec_cas<B: Bus>(&mut self, bus: &mut B, op: u16, size_bits: u16) -> Result<(), BusFault> {
        let size = match size_bits {
            1 => DataSize::Byte,
            2 => DataSize::Word,
            _ => DataSize::Long,
        };
        let ext = self.fetch_u16(bus)?;
        let dc = (ext & 7) as usize;
        let du = ((ext >> 6) & 7) as usize;
        let loc = self.resolve_ea(bus, ((op >> 3) & 7) as u8, (op & 7) as u8, size)?;
        if !matches!(loc, EaLocation::Memory(_)) {
            return self.illegal(bus);
        }
        let dest = self.read_ea(bus, loc, size)?;
        let compare = self.state.read_d(dc, size);
        alu_sub(&mut self.state, size, compare, dest, false);
        if self.state.flag(SR_Z) {
            let update = self.state.read_d(du, size);
            self.write_ea(bus, loc, size, update)?;
        } else {
            self.state.write_d(dc, size, dest);
        }
        Ok(())
    }

    /// `CAS2 Dc1:Dc2, Du1:Du2, (Rn1):(Rn2)` (MC68030).
    fn exec_cas2<B: Bus>(&mut self, bus: &mut B, size_bits: u16) -> Result<(), BusFault> {
        let size = if size_bits == 2 {
            DataSize::Word
        } else {
            DataSize::Long
        };
        let ext1 = self.fetch_u16(bus)?;
        let ext2 = self.fetch_u16(bus)?;
        let addr_of = |state: &CpuState, ext: u16| {
            let reg = ((ext >> 12) & 7) as usize;
            if (ext & 0x8000) != 0 {
                state.a[reg]
            } else {
                state.d[reg]
            }
        };
        let addr1 = addr_of(&self.state, ext1);
        let addr2 = addr_of(&self.state, ext2);
        let val1 = self.read_ea(bus, EaLocation::Memory(addr1), size)?;
        let val2 = self.read_ea(bus, EaLocation::Memory(addr2), size)?;
        let (dc1, du1) = ((ext1 & 7) as usize, ((ext1 >> 6) & 7) as usize);
        let (dc2, du2) = ((ext2 & 7) as usize, ((ext2 >> 6) & 7) as usize);
        let (c1, c2) = (self.state.read_d(dc1, size), self.state.read_d(dc2, size));
        alu_sub(&mut self.state, size, c1, val1, false);
        if self.state.flag(SR_Z) {
            alu_sub(&mut self.state, size, c2, val2, false);
        }
        if self.state.flag(SR_Z) {
            let (u1, u2) = (self.state.read_d(du1, size), self.state.read_d(du2, size));
            self.write_ea(bus, EaLocation::Memory(addr1), size, u1)?;
            self.write_ea(bus, EaLocation::Memory(addr2), size, u2)?;
        } else {
            self.state.write_d(dc1, size, val1);
            self.state.write_d(dc2, size, val2);
        }
        Ok(())
    }

    /// `MOVES Rn, <ea>` / `MOVES <ea>, Rn` (MC68030), accessing the space of `SFC` / `DFC`.
    fn exec_moves<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        if !self.state.is_supervisor() {
            return self.privilege_violation(bus);
        }
        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            2 => DataSize::Long,
            _ => return self.illegal(bus),
        };
        let ext = self.fetch_u16(bus)?;
        self.trace.ext = ext;
        let EaLocation::Memory(addr) =
            self.resolve_ea(bus, ((op >> 3) & 7) as u8, (op & 7) as u8, size)?
        else {
            return self.illegal(bus);
        };
        let reg = ((ext >> 12) & 7) as usize;
        let is_addr = (ext & 0x8000) != 0;
        // Function codes 5 and 6 are the supervisor spaces; the bus has no CPU space.
        if (ext & 0x0800) != 0 {
            let supervisor = (self.state.dfc & 4) != 0;
            let val = if is_addr {
                self.state.a[reg]
            } else {
                self.state.d[reg]
            };
            match size {
                DataSize::Byte => bus.write_u8(addr, val as u8, supervisor),
                DataSize::Word => bus.write_u16(addr, val as u16, supervisor),
                DataSize::Long => bus.write_u32(addr, val, supervisor),
            }
        } else {
            let supervisor = (self.state.sfc & 4) != 0;
            let val = match size {
                DataSize::Byte => bus.read_u8(addr, supervisor)? as u32,
                DataSize::Word => bus.read_u16(addr, supervisor)? as u32,
                DataSize::Long => bus.read_u32(addr, supervisor)?,
            };
            if is_addr {
                self.state.a[reg] = size.sign_extend(val) as u32;
            } else {
                self.state.write_d(reg, size, val);
            }
            Ok(())
        }
    }

    fn exec_group_4<B: Bus, T: TrapHandler<B>>(
        &mut self,
        bus: &mut B,
        trap_handler: &mut T,
        op: u16,
    ) -> Result<(), BusFault> {
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;

        if P::MC68030 {
            if let Some(result) = self.exec_group_4_030(bus, op, mode, reg) {
                return result;
            }
        }

        match op {
            0x4AFC => {
                // ILLEGAL
                return self.raise_exception(
                    bus,
                    ExceptionVector::IllegalInstruction as u8,
                    self.state.ir_pc,
                    34,
                );
            }
            0x4E70 => {
                // RESET
                if !self.state.is_supervisor() {
                    return self.raise_exception(
                        bus,
                        ExceptionVector::PrivilegeViolation as u8,
                        self.state.ir_pc,
                        34,
                    );
                }
                bus.reset_peripherals();
                self.add_cycles(132);
                return Ok(());
            }
            0x4E71 => {
                // NOP
                self.add_cycles(4);
                return Ok(());
            }
            0x4E72 => {
                // STOP #<imm>
                if !self.state.is_supervisor() {
                    return self.raise_exception(
                        bus,
                        ExceptionVector::PrivilegeViolation as u8,
                        self.state.ir_pc,
                        34,
                    );
                }
                let imm = self.fetch_u16(bus)?;
                self.state.set_sr(imm);
                self.state.stopped = true;
                self.add_cycles(4);
                return Ok(());
            }
            0x4E73 => {
                // RTE
                if !self.state.is_supervisor() {
                    return self.raise_exception(
                        bus,
                        ExceptionVector::PrivilegeViolation as u8,
                        self.state.ir_pc,
                        34,
                    );
                }
                return self.exec_rte(bus);
            }
            0x4E75 => {
                // RTS
                self.state.pc = self.pop_u32(bus)?;
                self.add_cycles(16);
                return Ok(());
            }
            0x4E76 => {
                // TRAPV
                if self.state.flag(SR_V) {
                    return self.raise_exception(
                        bus,
                        ExceptionVector::TrapvInstruction as u8,
                        self.state.pc,
                        34,
                    );
                }
                self.add_cycles(4);
                return Ok(());
            }
            0x4E77 => {
                // RTR
                let ccr = self.pop_u16(bus)? as u8;
                let new_pc = self.pop_u32(bus)?;
                self.state.set_ccr(ccr);
                self.state.pc = new_pc;
                self.add_cycles(20);
                return Ok(());
            }
            _ => {}
        }

        // TRAP #0..#15
        if (op & 0xFFF0) == 0x4E40 {
            let trap_no = (op & 0x0F) as u8;
            let trap_pc = self.state.pc;
            match trap_handler.on_trap(&mut self.state, bus, trap_no, trap_pc) {
                TrapResult::Handled { extra_cycles } => {
                    self.trace.native = Some(extra_cycles);
                    self.add_cycles(4 + extra_cycles);
                    return Ok(());
                }
                TrapResult::PassToVector | TrapResult::Unhandled => {
                    return self.raise_exception(bus, 32 + trap_no, self.state.pc, 34);
                }
            }
        }

        // LINK An, #<disp>
        if (op & 0xFFF8) == 0x4E50 {
            let r = (op & 7) as usize;
            let disp = self.fetch_u16(bus)? as i16 as i32;
            let old_an = self.state.a[r];
            self.push_u32(bus, old_an)?;
            let sp = self.state.a[7];
            self.state.a[r] = sp;
            self.state.a[7] = sp.wrapping_add(disp as u32);
            self.add_cycles(16);
            return Ok(());
        }

        // UNLK An
        if (op & 0xFFF8) == 0x4E58 {
            let r = (op & 7) as usize;
            self.state.a[7] = self.state.a[r];
            let old_an = self.pop_u32(bus)?;
            self.state.a[r] = old_an;
            self.add_cycles(12);
            return Ok(());
        }

        // MOVE USP, An / MOVE An, USP
        if (op & 0xFFF0) == 0x4E60 {
            if !self.state.is_supervisor() {
                return self.raise_exception(
                    bus,
                    ExceptionVector::PrivilegeViolation as u8,
                    self.state.ir_pc,
                    34,
                );
            }
            let r = (op & 7) as usize;
            if (op & 8) != 0 {
                self.state.a[r] = self.state.get_usp();
            } else {
                self.state.set_usp(self.state.a[r]);
            }
            self.add_cycles(4);
            return Ok(());
        }

        // JSR <ea>
        if (op & 0xFFC0) == 0x4E80 {
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
            if let EaLocation::Memory(target) = loc {
                self.push_u32(bus, self.state.pc)?;
                self.state.pc = target;
                self.add_cycles(14);
                return Ok(());
            }
        }

        // JMP <ea>
        if (op & 0xFFC0) == 0x4EC0 {
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
            if let EaLocation::Memory(target) = loc {
                self.state.pc = target;
                self.add_cycles(6);
                return Ok(());
            }
        }

        // SWAP Dn
        if (op & 0xFFF8) == 0x4840 {
            let r = (op & 7) as usize;
            let v = self.state.d[r].rotate_right(16);
            self.state.d[r] = v;
            set_nz_clear_vc(&mut self.state, DataSize::Long, v);
            self.add_cycles(4);
            return Ok(());
        }

        //PEA <ea>
        if (op & 0xFFC0) == 0x4840 {
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
            if let EaLocation::Memory(addr) = loc {
                self.push_u32(bus, addr)?;
                self.add_cycles(8);
                return Ok(());
            }
        }

        // EXT.W Dn / EXT.L Dn
        if (op & 0xFFB8) == 0x4880 {
            let r = (op & 7) as usize;
            if (op & 0x0040) == 0 {
                // EXT.W
                let v = (self.state.d[r] as u8 as i8 as i16) as u16 as u32;
                self.state.write_d(r, DataSize::Word, v);
                set_nz_clear_vc(&mut self.state, DataSize::Word, v);
            } else {
                // EXT.L
                let v = (self.state.d[r] as u16 as i16 as i32) as u32;
                self.state.d[r] = v;
                set_nz_clear_vc(&mut self.state, DataSize::Long, v);
            }
            self.add_cycles(4);
            return Ok(());
        }

        // MOVEM
        if (op & 0xFB80) == 0x4880 {
            let mask = self.fetch_u16(bus)?;
            self.trace.registers = mask.count_ones() as u16;
            let dir_mem_to_reg = (op & 0x0400) != 0;
            let size = if (op & 0x0040) != 0 {
                DataSize::Long
            } else {
                DataSize::Word
            };
            let step = size.bytes();

            if !dir_mem_to_reg && mode == 4 {
                // Predecrement -(An): mask bits are reversed (bit 0 = A7 .. bit 15 = D0)
                let base_reg = reg as usize;
                let mut addr = self.state.a[base_reg];
                let initial_an = addr;
                for i in 0..16 {
                    if (mask & (1 << i)) != 0 {
                        addr = addr.wrapping_sub(step);
                        let reg_idx = 15 - i;
                        let val = if reg_idx < 8 {
                            self.state.d[reg_idx]
                        } else if reg_idx - 8 == base_reg {
                            initial_an
                        } else {
                            self.state.a[reg_idx - 8]
                        };
                        if size == DataSize::Long {
                            self.bus_write_u32(bus, addr, val)?;
                            self.add_cycles(8);
                        } else {
                            self.bus_write_u16(bus, addr, val as u16)?;
                            self.add_cycles(4);
                        }
                    }
                }
                self.state.a[base_reg] = addr;
                self.add_cycles(8);
                return Ok(());
            }

            let mut addr = if dir_mem_to_reg && mode == 3 {
                self.state.a[reg as usize]
            } else {
                match self.resolve_ea(bus, mode, reg, DataSize::Word)? {
                    EaLocation::Memory(a) => a,
                    _ => {
                        return self.raise_exception(
                            bus,
                            ExceptionVector::IllegalInstruction as u8,
                            self.state.ir_pc,
                            34,
                        );
                    }
                }
            };

            for i in 0..16 {
                if (mask & (1 << i)) != 0 {
                    if dir_mem_to_reg {
                        let val = if size == DataSize::Long {
                            self.add_cycles(8);
                            self.bus_read_u32(bus, addr)?
                        } else {
                            self.add_cycles(4);
                            (self.bus_read_u16(bus, addr)? as i16 as i32) as u32
                        };
                        if i < 8 {
                            self.state.d[i] = val;
                        } else {
                            self.state.a[i - 8] = val;
                        }
                    } else {
                        let val = if i < 8 {
                            self.state.d[i]
                        } else {
                            self.state.a[i - 8]
                        };
                        if size == DataSize::Long {
                            self.add_cycles(8);
                            self.bus_write_u32(bus, addr, val)?;
                        } else {
                            self.add_cycles(4);
                            self.bus_write_u16(bus, addr, val as u16)?;
                        }
                    }
                    addr = addr.wrapping_add(step);
                }
            }
            if dir_mem_to_reg && mode == 3 {
                self.state.a[reg as usize] = addr;
            }
            self.add_cycles(8);
            return Ok(());
        }

        // LEA <ea>, An
        if (op & 0xF1C0) == 0x41C0 {
            let a_reg = ((op >> 9) & 7) as usize;
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
            if let EaLocation::Memory(addr) = loc {
                self.state.a[a_reg] = addr;
                self.add_cycles(4);
                return Ok(());
            }
        }

        // CHK.W <ea>, Dn
        if (op & 0xF1C0) == 0x4180 {
            let d_reg = ((op >> 9) & 7) as usize;
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Word)?;
            let bound = self.read_ea(bus, loc, DataSize::Word)? as u16 as i16;
            let val = self.state.d[d_reg] as u16 as i16;
            self.state.set_flag(SR_Z, val == 0);
            self.state.set_flag(SR_V, false);
            self.state.set_flag(SR_C, false);
            self.state.set_flag(SR_N, val < 0);
            if val < 0 || val > bound {
                return self.raise_exception(
                    bus,
                    ExceptionVector::ChkInstruction as u8,
                    self.state.pc,
                    40,
                );
            }
            self.add_cycles(10);
            return Ok(());
        }

        // MOVE from SR / MOVE to CCR / MOVE to SR
        if (op & 0xFFC0) == 0x40C0 {
            if P::MC68030 && !self.state.is_supervisor() {
                return self.privilege_violation(bus);
            }
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Word)?;
            let sr = self.state.sr as u32;
            self.write_ea(bus, loc, DataSize::Word, sr)?;
            self.add_cycles(if matches!(loc, EaLocation::Memory(_)) { 8 } else { 6 });
            return Ok(());
        }
        if (op & 0xFFC0) == 0x44C0 {
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Word)?;
            let val = self.read_ea(bus, loc, DataSize::Word)? as u8;
            self.state.set_ccr(val);
            self.add_cycles(12);
            return Ok(());
        }
        if (op & 0xFFC0) == 0x46C0 {
            if !self.state.is_supervisor() {
                return self.raise_exception(
                    bus,
                    ExceptionVector::PrivilegeViolation as u8,
                    self.state.ir_pc,
                    34,
                );
            }
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Word)?;
            let val = self.read_ea(bus, loc, DataSize::Word)? as u16;
            self.state.set_sr(val);
            self.add_cycles(12);
            return Ok(());
        }

        // NBCD <ea>
        if (op & 0xFFC0) == 0x4800 {
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Byte)?;
            let val = self.read_ea(bus, loc, DataSize::Byte)? as u8;
            let res = alu_sbcd(&mut self.state, val, 0);
            self.write_ea(bus, loc, DataSize::Byte, res as u32)?;
            self.add_cycles(if matches!(loc, EaLocation::Memory(_)) { 8 } else { 6 });
            return Ok(());
        }

        // TAS <ea>
        if (op & 0xFFC0) == 0x4AC0 {
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Byte)?;
            let val = self.read_ea(bus, loc, DataSize::Byte)?;
            set_nz_clear_vc(&mut self.state, DataSize::Byte, val);
            self.write_ea(bus, loc, DataSize::Byte, val | 0x80)?;
            self.add_cycles(if matches!(loc, EaLocation::Memory(_)) { 10 } else { 4 });
            return Ok(());
        }

        // NEGX (0), CLR (1), NEG (2), NOT (3), TST (5)
        let sub = (op >> 9) & 7;
        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            2 => DataSize::Long,
            _ => {
                return self.raise_exception(
                    bus,
                    ExceptionVector::IllegalInstruction as u8,
                    self.state.ir_pc,
                    34,
                );
            }
        };
        let loc = self.resolve_ea(bus, mode, reg, size)?;
        // The MC68000 reads the operand of CLR before clearing it; the MC68030 only writes.
        let val = if P::MC68030 && sub == 1 {
            0
        } else {
            self.read_ea(bus, loc, size)?
        };

        match sub {
            0 => {
                // NEGX
                let res = alu_subx(&mut self.state, size, val, 0);
                self.write_ea(bus, loc, size, res)?;
            }
            1 => {
                // CLR
                self.write_ea(bus, loc, size, 0)?;
                set_nz_clear_vc(&mut self.state, size, 0);
            }
            2 => {
                // NEG
                let res = alu_sub(&mut self.state, size, val, 0, true);
                self.write_ea(bus, loc, size, res)?;
            }
            3 => {
                // NOT
                let res = (!val) & size.mask();
                self.write_ea(bus, loc, size, res)?;
                set_nz_clear_vc(&mut self.state, size, res);
            }
            5 => {
                // TST
                set_nz_clear_vc(&mut self.state, size, val);
            }
            _ => {
                return self.raise_exception(
                    bus,
                    ExceptionVector::IllegalInstruction as u8,
                    self.state.ir_pc,
                    34,
                );
            }
        }
        let is_mem = matches!(loc, EaLocation::Memory(_));
        let base = if sub == 5 {
            4
        } else if size == DataSize::Long {
            if is_mem { 12 } else { 6 }
        } else if is_mem {
            8
        } else {
            4
        };
        self.add_cycles(base);
        Ok(())
    }

    /// Decodes the group 4 instructions the MC68030 adds, returning `None` for the others.
    fn exec_group_4_030<B: Bus>(
        &mut self,
        bus: &mut B,
        op: u16,
        mode: u8,
        reg: u8,
    ) -> Option<Result<(), BusFault>> {
        let r = reg as usize;
        let result = match op {
            // RTD #<disp>
            0x4E74 => (|| {
                let disp = self.fetch_u16(bus)? as i16 as i32 as u32;
                self.state.pc = self.pop_u32(bus)?;
                self.state.a[7] = self.state.a[7].wrapping_add(disp);
                Ok(())
            })(),
            // MOVEC Rc, Rn / MOVEC Rn, Rc
            0x4E7A | 0x4E7B => self.exec_movec(bus, op),
            // BKPT #<n>: no debugger hardware answers the breakpoint cycle.
            0x4848..=0x484F => self.illegal(bus),
            // LINK.L An, #<disp>
            0x4808..=0x480F => (|| {
                let disp = self.fetch_u32(bus)?;
                let old_an = self.state.a[r];
                self.push_u32(bus, old_an)?;
                let sp = self.state.a[7];
                self.state.a[r] = sp;
                self.state.a[7] = sp.wrapping_add(disp);
                Ok(())
            })(),
            // EXTB.L Dn
            0x49C0..=0x49C7 => {
                let v = self.state.d[r] as u8 as i8 as i32 as u32;
                self.state.d[r] = v;
                set_nz_clear_vc(&mut self.state, DataSize::Long, v);
                Ok(())
            }
            // MULU.L / MULS.L <ea>, Dl or Dh:Dl
            _ if (op & 0xFFC0) == 0x4C00 => self.exec_mul_long(bus, mode, reg),
            // DIVU.L / DIVS.L <ea>, Dq or Dr:Dq
            _ if (op & 0xFFC0) == 0x4C40 => self.exec_div_long(bus, mode, reg),
            // MOVE from CCR
            _ if (op & 0xFFC0) == 0x42C0 => (|| {
                let loc = self.resolve_ea(bus, mode, reg, DataSize::Word)?;
                let ccr = self.state.ccr() as u32;
                self.write_ea(bus, loc, DataSize::Word, ccr)
            })(),
            // CHK.L <ea>, Dn
            _ if (op & 0xF1C0) == 0x4100 => (|| {
                let d_reg = ((op >> 9) & 7) as usize;
                let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
                let bound = self.read_ea(bus, loc, DataSize::Long)? as i32;
                let val = self.state.d[d_reg] as i32;
                if val < 0 || val > bound {
                    self.state.set_flag(SR_N, val < 0);
                    return self.raise_exception(
                        bus,
                        ExceptionVector::ChkInstruction as u8,
                        self.state.pc,
                        40,
                    );
                }
                Ok(())
            })(),
            _ => return None,
        };
        Some(result)
    }

    /// `MOVEC` between a general register and an MC68030 control register.
    fn exec_movec<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        if !self.state.is_supervisor() {
            return self.privilege_violation(bus);
        }
        let ext = self.fetch_u16(bus)?;
        self.trace.ext = ext;
        let reg = ((ext >> 12) & 7) as usize;
        let is_addr = (ext & 0x8000) != 0;
        // CACR bits the MC68030 implements; the clear bits (CEI, CI, CED, CD) always read 0.
        const CACR_STORED: u32 = 0x3313;
        if (op & 1) == 0 {
            let val = match ext & 0x0FFF {
                0x000 => self.state.sfc as u32,
                0x001 => self.state.dfc as u32,
                0x002 => self.state.cacr,
                0x800 => self.state.get_usp(),
                0x801 => self.state.vbr,
                0x802 => self.state.caar,
                0x803 => self.state.get_msp(),
                0x804 => self.state.get_ssp(),
                _ => return self.illegal(bus),
            };
            if is_addr {
                self.state.a[reg] = val;
            } else {
                self.state.d[reg] = val;
            }
        } else {
            let val = if is_addr {
                self.state.a[reg]
            } else {
                self.state.d[reg]
            };
            match ext & 0x0FFF {
                0x000 => self.state.sfc = (val & 7) as u8,
                0x001 => self.state.dfc = (val & 7) as u8,
                0x002 => {
                    self.state.cacr = val & CACR_STORED;
                    let caar = self.state.caar;
                    let timing = &mut self.state.timing030;
                    if (val & cacr::CI) != 0 {
                        timing.icache.clear();
                    } else if (val & cacr::CEI) != 0 {
                        timing.icache.clear_entry(caar);
                    }
                    if (val & cacr::CD) != 0 {
                        timing.dcache.clear();
                    } else if (val & cacr::CED) != 0 {
                        timing.dcache.clear_entry(caar);
                    }
                }
                0x800 => self.state.set_usp(val),
                0x801 => self.state.vbr = val,
                0x802 => self.state.caar = val,
                0x803 => self.state.set_msp(val),
                0x804 => self.state.set_ssp(val),
                _ => return self.illegal(bus),
            }
        }
        Ok(())
    }

    /// `MULU.L` / `MULS.L` (MC68030).
    fn exec_mul_long<B: Bus>(&mut self, bus: &mut B, mode: u8, reg: u8) -> Result<(), BusFault> {
        let ext = self.fetch_u16(bus)?;
        let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
        let src = self.read_ea(bus, loc, DataSize::Long)?;
        let dl = ((ext >> 12) & 7) as usize;
        let dh = (ext & 7) as usize;
        let signed = (ext & 0x0800) != 0;
        let quad = (ext & 0x0400) != 0;
        let product = if signed {
            (src as i32 as i64).wrapping_mul(self.state.d[dl] as i32 as i64) as u64
        } else {
            (src as u64) * (self.state.d[dl] as u64)
        };
        let low = product as u32;
        let (negative, zero, overflow) = if quad {
            self.state.d[dh] = (product >> 32) as u32;
            ((product as i64) < 0, product == 0, false)
        } else {
            let overflow = if signed {
                product as i64 != low as i32 as i64
            } else {
                (product >> 32) != 0
            };
            ((low as i32) < 0, low == 0, overflow)
        };
        self.state.d[dl] = low;
        self.state.set_flag(SR_N, negative);
        self.state.set_flag(SR_Z, zero);
        self.state.set_flag(SR_V, overflow);
        self.state.set_flag(SR_C, false);
        Ok(())
    }

    /// `DIVU.L` / `DIVS.L` / `DIVUL.L` / `DIVSL.L` (MC68030).
    fn exec_div_long<B: Bus>(&mut self, bus: &mut B, mode: u8, reg: u8) -> Result<(), BusFault> {
        let ext = self.fetch_u16(bus)?;
        self.trace.ext = ext;
        let loc = self.resolve_ea(bus, mode, reg, DataSize::Long)?;
        let divisor = self.read_ea(bus, loc, DataSize::Long)?;
        let dq = ((ext >> 12) & 7) as usize;
        let dr = (ext & 7) as usize;
        let signed = (ext & 0x0800) != 0;
        let quad = (ext & 0x0400) != 0;
        if divisor == 0 {
            return self.raise_exception(
                bus,
                ExceptionVector::ZeroDivide as u8,
                self.state.pc,
                38,
            );
        }
        let result = if signed {
            let dividend = if quad {
                (((self.state.d[dr] as u64) << 32) | self.state.d[dq] as u64) as i64
            } else {
                self.state.d[dq] as i32 as i64
            };
            let divisor = divisor as i32 as i64;
            dividend.checked_div(divisor).and_then(|quot| {
                let quot = i32::try_from(quot).ok()?;
                Some((quot as u32, (dividend % divisor) as u32))
            })
        } else {
            let dividend = if quad {
                ((self.state.d[dr] as u64) << 32) | self.state.d[dq] as u64
            } else {
                self.state.d[dq] as u64
            };
            let quot = dividend / divisor as u64;
            u32::try_from(quot)
                .ok()
                .map(|quot| (quot, (dividend % divisor as u64) as u32))
        };
        self.state.set_flag(SR_C, false);
        let Some((quot, rem)) = result else {
            self.state.set_flag(SR_V, true);
            return Ok(());
        };
        if dr != dq {
            self.state.d[dr] = rem;
        }
        self.state.d[dq] = quot;
        set_nz_clear_vc(&mut self.state, DataSize::Long, quot);
        Ok(())
    }

    fn exec_group_5<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;
        let cond = ((op >> 8) & 0xF) as u8;

        if (op & 0x00C0) == 0x00C0 {
            if P::MC68030 && mode == 7 && (2..=4).contains(&reg) {
                // TRAPcc, TRAPcc.W #<data>, TRAPcc.L #<data>
                match reg {
                    2 => self.state.pc = self.state.pc.wrapping_add(2),
                    3 => self.state.pc = self.state.pc.wrapping_add(4),
                    _ => {}
                }
                if eval_cc(self.state.sr, cond) {
                    return self.raise_exception(
                        bus,
                        ExceptionVector::TrapvInstruction as u8,
                        self.state.pc,
                        34,
                    );
                }
                return Ok(());
            }
            if mode == 1 {
                // DBcc Dn, <disp>
                let disp = self.fetch_u16(bus)? as i16 as i32;
                if !eval_cc(self.state.sr, cond) {
                    let r = reg as usize;
                    let low = (self.state.d[r] as u16).wrapping_sub(1);
                    self.state.write_d(r, DataSize::Word, low as u32);
                    if low != 0xFFFF {
                        self.state.pc = self.state.pc.wrapping_sub(2).wrapping_add(disp as u32);
                        self.trace.branch_taken = true;
                        self.add_cycles(10);
                    } else {
                        self.trace.count_expired = true;
                        self.add_cycles(14);
                    }
                } else {
                    self.add_cycles(12);
                }
            } else {
                // Scc <ea>
                let loc = self.resolve_ea(bus, mode, reg, DataSize::Byte)?;
                let truth = eval_cc(self.state.sr, cond);
                let byte_val = if truth { 0xFF } else { 0x00 };
                self.write_ea(bus, loc, DataSize::Byte, byte_val)?;
                let cycles = match (loc, truth) {
                    (EaLocation::Memory(_), _) => 8,
                    (_, true) => 6,
                    (_, false) => 4,
                };
                self.add_cycles(cycles);
            }
            return Ok(());
        }

        // ADDQ / SUBQ #<1..8>, <ea>
        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };
        let raw_q = ((op >> 9) & 7) as u32;
        let q = if raw_q == 0 { 8 } else { raw_q };
        let is_sub = (op & 0x0100) != 0;

        if mode == 1 {
            // ADDQ/SUBQ on An does not affect CCR and always operates on the full 32-bit An
            let r = reg as usize;
            self.state.a[r] = if is_sub {
                self.state.a[r].wrapping_sub(q)
            } else {
                self.state.a[r].wrapping_add(q)
            };
            self.add_cycles(8);
            return Ok(());
        }

        let loc = self.resolve_ea(bus, mode, reg, size)?;
        let dst = self.read_ea(bus, loc, size)?;
        let res = if is_sub {
            alu_sub(&mut self.state, size, q, dst, true)
        } else {
            alu_add(&mut self.state, size, q, dst)
        };
        self.write_ea(bus, loc, size, res)?;
        let is_mem = matches!(loc, EaLocation::Memory(_));
        let base = if size == DataSize::Long {
            if is_mem { 12 } else { 8 }
        } else if is_mem {
            8
        } else {
            4
        };
        self.add_cycles(base);
        Ok(())
    }

    fn exec_group_6<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let cond = ((op >> 8) & 0xF) as u8;
        let disp8 = (op & 0xFF) as u8;
        let base_pc = self.state.pc;
        let disp: i32 = if disp8 == 0 {
            self.fetch_u16(bus)? as i16 as i32
        } else if P::MC68030 && disp8 == 0xFF {
            self.fetch_u32(bus)? as i32
        } else {
            (disp8 as i8) as i32
        };

        if Self::TRACK_030 {
            self.trace.branch_taken = cond != 1 && eval_cc(self.state.sr, cond);
        }
        if cond == 1 {
            // BSR
            self.push_u32(bus, self.state.pc)?;
            self.state.pc = base_pc.wrapping_add(disp as u32);
            self.add_cycles(18);
            return Ok(());
        }

        if eval_cc(self.state.sr, cond) {
            self.state.pc = base_pc.wrapping_add(disp as u32);
            self.add_cycles(10);
        } else {
            self.add_cycles(if disp8 == 0 { 12 } else { 8 });
        }
        Ok(())
    }

    fn exec_group_8<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let rx = ((op >> 9) & 7) as usize;
        let mode = ((op >> 3) & 7) as u8;
        let ry = (op & 7) as u8;

        // DIVU.W / DIVS.W <ea>, Dn
        if (op & 0x00C0) == 0x00C0 {
            let signed = (op & 0x0100) != 0;
            let loc = self.resolve_ea(bus, mode, ry, DataSize::Word)?;
            let divisor = self.read_ea(bus, loc, DataSize::Word)? as u16;
            if divisor == 0 {
                return self.raise_exception(
                    bus,
                    ExceptionVector::ZeroDivide as u8,
                    self.state.pc,
                    38,
                );
            }
            let dividend = self.state.d[rx];
            let mut sr = self.state.sr & (!SR_CCR_MASK | SR_X);
            if signed {
                let num = dividend as i32;
                let den = (divisor as i16) as i32;
                let quot = num / den;
                let rem = num % den;
                if !(-32768..=32767).contains(&quot) {
                    sr |= SR_V | SR_N;
                    self.state.sr = sr;
                } else {
                    let q16 = (quot as i16) as u16;
                    let r16 = (rem as i16) as u16;
                    self.state.d[rx] = ((r16 as u32) << 16) | (q16 as u32);
                    if q16 == 0 {
                        sr |= SR_Z;
                    }
                    if (q16 & 0x8000) != 0 {
                        sr |= SR_N;
                    }
                    self.state.sr = sr;
                }
                self.add_cycles(154);
            } else {
                let den = divisor as u32;
                let quot = dividend / den;
                let rem = dividend % den;
                if quot > 0xFFFF {
                    sr |= SR_V | SR_N;
                    self.state.sr = sr;
                } else {
                    let q16 = quot as u16;
                    let r16 = rem as u16;
                    self.state.d[rx] = ((r16 as u32) << 16) | (q16 as u32);
                    if q16 == 0 {
                        sr |= SR_Z;
                    }
                    if (q16 & 0x8000) != 0 {
                        sr |= SR_N;
                    }
                    self.state.sr = sr;
                }
                self.add_cycles(136);
            }
            return Ok(());
        }

        // PACK / UNPK (MC68030)
        if P::MC68030 && matches!(op & 0x01F0, 0x0140 | 0x0180) {
            return self.exec_pack_unpk(bus, op);
        }

        // SBCD Dy, Dx or SBCD -(Ay), -(Ax)
        if (op & 0x01F0) == 0x0100 {
            let mem_mode = (op & 8) != 0;
            if mem_mode {
                let ay = ry as usize;
                let ax = rx;
                let sy = if ay == 7 { 2 } else { 1 };
                let sx = if ax == 7 { 2 } else { 1 };
                self.state.a[ay] = self.state.a[ay].wrapping_sub(sy);
                let src = self.bus_read_u8(bus, self.state.a[ay])?;
                self.state.a[ax] = self.state.a[ax].wrapping_sub(sx);
                let dst = self.bus_read_u8(bus, self.state.a[ax])?;
                let res = alu_sbcd(&mut self.state, src, dst);
                self.bus_write_u8(bus, self.state.a[ax], res)?;
                self.add_cycles(18);
            } else {
                let src = self.state.d[ry as usize] as u8;
                let dst = self.state.d[rx] as u8;
                let res = alu_sbcd(&mut self.state, src, dst);
                self.state.write_d(rx, DataSize::Byte, res as u32);
                self.add_cycles(6);
            }
            return Ok(());
        }

        // OR <ea>, Dn / OR Dn, <ea>
        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };
        let to_ea = (op & 0x0100) != 0;
        let loc = self.resolve_ea(bus, mode, ry, size)?;
        let ea_val = self.read_ea(bus, loc, size)?;
        let dn_val = self.state.read_d(rx, size);
        let res = (ea_val | dn_val) & size.mask();
        if to_ea {
            self.write_ea(bus, loc, size, res)?;
        } else {
            self.state.write_d(rx, size, res);
        }
        set_nz_clear_vc(&mut self.state, size, res);
        self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
        Ok(())
    }

    /// `PACK` / `UNPK` with their adjustment word (MC68030).
    fn exec_pack_unpk<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let adjust = self.fetch_u16(bus)?;
        let src = (op & 7) as usize;
        let dst = ((op >> 9) & 7) as usize;
        let memory = (op & 8) != 0;
        let predecrement = |state: &mut CpuState, reg: usize| {
            let step = if reg == 7 { 2 } else { 1 };
            state.a[reg] = state.a[reg].wrapping_sub(step);
            state.a[reg]
        };
        if (op & 0x01F0) == 0x0140 {
            let unpacked = if memory {
                let addr = predecrement(&mut self.state, src);
                let lo = self.bus_read_u8(bus, addr)? as u16;
                let addr = predecrement(&mut self.state, src);
                let hi = self.bus_read_u8(bus, addr)? as u16;
                (hi << 8) | lo
            } else {
                self.state.d[src] as u16
            };
            let sum = unpacked.wrapping_add(adjust);
            let packed = (((sum >> 4) & 0xF0) | (sum & 0x0F)) as u8;
            if memory {
                let addr = predecrement(&mut self.state, dst);
                self.bus_write_u8(bus, addr, packed)?;
            } else {
                self.state.write_d(dst, DataSize::Byte, packed as u32);
            }
        } else {
            let packed = if memory {
                let addr = predecrement(&mut self.state, src);
                self.bus_read_u8(bus, addr)?
            } else {
                self.state.d[src] as u8
            };
            let unpacked =
                ((((packed & 0xF0) as u16) << 4) | (packed & 0x0F) as u16).wrapping_add(adjust);
            if memory {
                let addr = predecrement(&mut self.state, dst);
                self.bus_write_u8(bus, addr, unpacked as u8)?;
                let addr = predecrement(&mut self.state, dst);
                self.bus_write_u8(bus, addr, (unpacked >> 8) as u8)?;
            } else {
                self.state.write_d(dst, DataSize::Word, unpacked as u32);
            }
        }
        Ok(())
    }

    fn exec_add_sub<B: Bus>(&mut self, bus: &mut B, op: u16, is_add: bool) -> Result<(), BusFault> {
        let rx = ((op >> 9) & 7) as usize;
        let mode = ((op >> 3) & 7) as u8;
        let ry = (op & 7) as u8;
        let opmode = (op >> 6) & 7;

        // ADDA / SUBA (.W or .L)
        if opmode == 3 || opmode == 7 {
            let size = if opmode == 3 {
                DataSize::Word
            } else {
                DataSize::Long
            };
            let loc = self.resolve_ea(bus, mode, ry, size)?;
            let raw = self.read_ea(bus, loc, size)?;
            let ext = if size == DataSize::Word {
                (raw as u16 as i16 as i32) as u32
            } else {
                raw
            };
            self.state.a[rx] = if is_add {
                self.state.a[rx].wrapping_add(ext)
            } else {
                self.state.a[rx].wrapping_sub(ext)
            };
            self.add_cycles(8);
            return Ok(());
        }

        let size = match opmode & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };

        // ADDX / SUBX when bit 8 is 1 and mode is 0 (Dn) or 1 (-(An))
        if (op & 0x0130) == 0x0100 {
            let mem_mode = (op & 8) != 0;
            if mem_mode {
                let ay = ry as usize;
                let ax = rx;
                let step_y = if ay == 7 && size == DataSize::Byte {
                    2
                } else {
                    size.bytes()
                };
                let step_x = if ax == 7 && size == DataSize::Byte {
                    2
                } else {
                    size.bytes()
                };
                self.state.a[ay] = self.state.a[ay].wrapping_sub(step_y);
                let src = self.read_ea(bus, EaLocation::Memory(self.state.a[ay]), size)?;
                self.state.a[ax] = self.state.a[ax].wrapping_sub(step_x);
                let dst = self.read_ea(bus, EaLocation::Memory(self.state.a[ax]), size)?;
                let res = if is_add {
                    alu_addx(&mut self.state, size, src, dst)
                } else {
                    alu_subx(&mut self.state, size, src, dst)
                };
                self.write_ea(bus, EaLocation::Memory(self.state.a[ax]), size, res)?;
                self.add_cycles(if size == DataSize::Long { 30 } else { 18 });
            } else {
                let src = self.state.read_d(ry as usize, size);
                let dst = self.state.read_d(rx, size);
                let res = if is_add {
                    alu_addx(&mut self.state, size, src, dst)
                } else {
                    alu_subx(&mut self.state, size, src, dst)
                };
                self.state.write_d(rx, size, res);
                self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
            }
            return Ok(());
        }

        let to_ea = (op & 0x0100) != 0;
        let loc = self.resolve_ea(bus, mode, ry, size)?;
        let ea_val = self.read_ea(bus, loc, size)?;
        let dn_val = self.state.read_d(rx, size);

        if to_ea {
            let res = if is_add {
                alu_add(&mut self.state, size, dn_val, ea_val)
            } else {
                alu_sub(&mut self.state, size, dn_val, ea_val, true)
            };
            self.write_ea(bus, loc, size, res)?;
            self.add_cycles(if size == DataSize::Long { 12 } else { 8 });
        } else {
            let res = if is_add {
                alu_add(&mut self.state, size, ea_val, dn_val)
            } else {
                alu_sub(&mut self.state, size, ea_val, dn_val, true)
            };
            self.state.write_d(rx, size, res);
            self.add_cycles(if size == DataSize::Long { 6 } else { 4 });
        }
        Ok(())
    }

    fn exec_group_b<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let rx = ((op >> 9) & 7) as usize;
        let mode = ((op >> 3) & 7) as u8;
        let ry = (op & 7) as u8;
        let opmode = (op >> 6) & 7;

        // CMPA.W / CMPA.L
        if opmode == 3 || opmode == 7 {
            let size = if opmode == 3 {
                DataSize::Word
            } else {
                DataSize::Long
            };
            let loc = self.resolve_ea(bus, mode, ry, size)?;
            let raw = self.read_ea(bus, loc, size)?;
            let ext = if size == DataSize::Word {
                (raw as u16 as i16 as i32) as u32
            } else {
                raw
            };
            let dst_an = self.state.a[rx];
            let _ = alu_sub(&mut self.state, DataSize::Long, ext, dst_an, false);
            self.add_cycles(6);
            return Ok(());
        }

        let size = match opmode & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };

        if (op & 0x0100) != 0 {
            if mode == 1 {
                // CMPM (Ay)+, (Ax)+
                let ay = ry as usize;
                let ax = rx;
                let src_loc = self.resolve_ea(bus, 3, ay as u8, size)?;
                let src = self.read_ea(bus, src_loc, size)?;
                let dst_loc = self.resolve_ea(bus, 3, ax as u8, size)?;
                let dst = self.read_ea(bus, dst_loc, size)?;
                let _ = alu_sub(&mut self.state, size, src, dst, false);
                self.add_cycles(4);
            } else {
                // EOR Dn, <ea>
                let loc = self.resolve_ea(bus, mode, ry, size)?;
                let dst = self.read_ea(bus, loc, size)?;
                let src = self.state.read_d(rx, size);
                let res = (dst ^ src) & size.mask();
                self.write_ea(bus, loc, size, res)?;
                set_nz_clear_vc(&mut self.state, size, res);
                self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
            }
        } else {
            // CMP <ea>, Dn
            let loc = self.resolve_ea(bus, mode, ry, size)?;
            let src = self.read_ea(bus, loc, size)?;
            let dst = self.state.read_d(rx, size);
            let _ = alu_sub(&mut self.state, size, src, dst, false);
            self.add_cycles(if size == DataSize::Long { 6 } else { 4 });
        }
        Ok(())
    }

    fn exec_group_c<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let rx = ((op >> 9) & 7) as usize;
        let mode = ((op >> 3) & 7) as u8;
        let ry = (op & 7) as u8;

        // MULU.W / MULS.W <ea>, Dn
        if (op & 0x00C0) == 0x00C0 {
            let signed = (op & 0x0100) != 0;
            let loc = self.resolve_ea(bus, mode, ry, DataSize::Word)?;
            let src = self.read_ea(bus, loc, DataSize::Word)? as u16;
            let dst = self.state.d[rx] as u16;
            let res = if signed {
                self.add_cycles(muls_cycles(src));
                ((src as i16 as i32) * (dst as i16 as i32)) as u32
            } else {
                self.add_cycles(mulu_cycles(src));
                (src as u32) * (dst as u32)
            };
            self.state.d[rx] = res;
            set_nz_clear_vc(&mut self.state, DataSize::Long, res);
            return Ok(());
        }

        // ABCD Dy, Dx / ABCD -(Ay), -(Ax)
        if (op & 0x01F0) == 0x0100 {
            let mem_mode = (op & 8) != 0;
            if mem_mode {
                let ay = ry as usize;
                let ax = rx;
                let sy = if ay == 7 { 2 } else { 1 };
                let sx = if ax == 7 { 2 } else { 1 };
                self.state.a[ay] = self.state.a[ay].wrapping_sub(sy);
                let src = self.bus_read_u8(bus, self.state.a[ay])?;
                self.state.a[ax] = self.state.a[ax].wrapping_sub(sx);
                let dst = self.bus_read_u8(bus, self.state.a[ax])?;
                let res = alu_abcd(&mut self.state, src, dst);
                self.bus_write_u8(bus, self.state.a[ax], res)?;
                self.add_cycles(18);
            } else {
                let src = self.state.d[ry as usize] as u8;
                let dst = self.state.d[rx] as u8;
                let res = alu_abcd(&mut self.state, src, dst);
                self.state.write_d(rx, DataSize::Byte, res as u32);
                self.add_cycles(6);
            }
            return Ok(());
        }

        // EXG
        if (op & 0x0130) == 0x0100 {
            let ry_idx = ry as usize;
            let opmode = (op >> 3) & 0x1F;
            match opmode {
                0x08 => self.state.d.swap(rx, ry_idx),
                0x09 => self.state.a.swap(rx, ry_idx),
                0x11 => std::mem::swap(&mut self.state.d[rx], &mut self.state.a[ry_idx]),
                _ => {}
            }
            self.add_cycles(6);
            return Ok(());
        }

        // AND <ea>, Dn / AND Dn, <ea>
        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };
        let to_ea = (op & 0x0100) != 0;
        let loc = self.resolve_ea(bus, mode, ry, size)?;
        let ea_val = self.read_ea(bus, loc, size)?;
        let dn_val = self.state.read_d(rx, size);
        let res = (ea_val & dn_val) & size.mask();
        if to_ea {
            self.write_ea(bus, loc, size, res)?;
        } else {
            self.state.write_d(rx, size, res);
        }
        set_nz_clear_vc(&mut self.state, size, res);
        self.add_cycles(if size == DataSize::Long { 8 } else { 4 });
        Ok(())
    }

    fn exec_group_e<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        if P::MC68030 && (op & 0x08C0) == 0x08C0 {
            return self.exec_bitfield(bus, op);
        }
        let left = (op & 0x0100) != 0;

        if (op & 0x00C0) == 0x00C0 {
            // Memory shift/rotate by 1 bit (Word only)
            let kind = ((op >> 9) & 3) as u8;
            let mode = ((op >> 3) & 7) as u8;
            let reg = (op & 7) as u8;
            let loc = self.resolve_ea(bus, mode, reg, DataSize::Word)?;
            let val = self.read_ea(bus, loc, DataSize::Word)?;
            let res = alu_shift_rotate(&mut self.state, DataSize::Word, val, 1, left, kind);
            self.write_ea(bus, loc, DataSize::Word, res)?;
            self.add_cycles(8);
            return Ok(());
        }

        let size = match (op >> 6) & 3 {
            0 => DataSize::Byte,
            1 => DataSize::Word,
            _ => DataSize::Long,
        };
        let kind = ((op >> 3) & 3) as u8;
        let reg = (op & 7) as usize;
        let c_or_r = ((op >> 9) & 7) as usize;
        let count = if (op & 0x0020) != 0 {
            self.state.d[c_or_r] & 63
        } else if c_or_r == 0 {
            8
        } else {
            c_or_r as u32
        };

        if Self::TRACK_030 {
            self.trace.long_shift = count > size.bytes() * 8;
        }
        let val = self.state.read_d(reg, size);
        let res = alu_shift_rotate(&mut self.state, size, val, count, left, kind);
        self.state.write_d(reg, size, res);
        let base = if size == DataSize::Long { 8 } else { 6 };
        self.add_cycles(base + 2 * count);
        Ok(())
    }

    /// `BFTST`, `BFEXTU`, `BFCHG`, `BFEXTS`, `BFCLR`, `BFFFO`, `BFSET`, `BFINS` (MC68030).
    fn exec_bitfield<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        const TST: u16 = 0;
        const EXTU: u16 = 1;
        const CHG: u16 = 2;
        const EXTS: u16 = 3;
        const CLR: u16 = 4;
        const FFO: u16 = 5;
        const SET: u16 = 6;
        const INS: u16 = 7;
        let kind = (op >> 8) & 7;
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;
        let ext = self.fetch_u16(bus)?;
        let offset = if (ext & 0x0800) != 0 {
            self.state.d[((ext >> 6) & 7) as usize] as i32
        } else {
            ((ext >> 6) & 31) as i32
        };
        let width = match (if (ext & 0x0020) != 0 {
            self.state.d[(ext & 7) as usize]
        } else {
            ext as u32
        }) & 31
        {
            0 => 32,
            w => w,
        };
        let dn = ((ext >> 12) & 7) as usize;
        let low_mask = u32::MAX >> (32 - width);
        let writes = matches!(kind, CHG | CLR | SET | INS);
        let pc_relative_ok = !writes;
        let allowed = match mode {
            0 | 2 | 5 | 6 => true,
            7 => reg <= 1 || (pc_relative_ok && reg <= 3),
            _ => false,
        };
        if !allowed {
            return self.illegal(bus);
        }

        // Reads the field, returning it with a function that stores a replacement.
        let (field, location) = if mode == 0 {
            let bit = (offset as u32) & 31;
            let field = self.state.d[reg as usize].rotate_left(bit) >> (32 - width);
            (field, None)
        } else {
            let EaLocation::Memory(base) = self.resolve_ea(bus, mode, reg, DataSize::Byte)? else {
                return self.illegal(bus);
            };
            let addr = base.wrapping_add((offset >> 3) as u32);
            let bit = (offset & 7) as u32;
            let bytes = (bit + width).div_ceil(8);
            self.trace.wide_field = bytes > 4;
            let mut window = 0u64;
            for i in 0..bytes {
                window |= (self.bus_read_u8(bus, addr.wrapping_add(i))? as u64) << (56 - 8 * i);
            }
            let field = ((window << bit) >> (64 - width)) as u32;
            (field, Some((addr, bit, bytes, window)))
        };

        let inserted = self.state.d[dn] & low_mask;
        let flag_source = if kind == INS { inserted } else { field };
        self.state.set_flag(SR_N, (flag_source >> (width - 1)) & 1 != 0);
        self.state.set_flag(SR_Z, flag_source == 0);
        self.state.set_flag(SR_V, false);
        self.state.set_flag(SR_C, false);

        let replacement = match kind {
            TST => None,
            EXTU => {
                self.state.d[dn] = field;
                None
            }
            EXTS => {
                self.state.d[dn] = ((field << (32 - width)) as i32 >> (32 - width)) as u32;
                None
            }
            FFO => {
                let leading = if field == 0 {
                    width
                } else {
                    field.leading_zeros() - (32 - width)
                };
                self.state.d[dn] = (offset as u32).wrapping_add(leading);
                None
            }
            CHG => Some(!field & low_mask),
            CLR => Some(0),
            SET => Some(low_mask),
            INS => Some(inserted),
            _ => unreachable!(),
        };
        let Some(new_field) = replacement else {
            return Ok(());
        };
        match location {
            None => {
                let bit = (offset as u32) & 31;
                let r = reg as usize;
                let mask = (low_mask << (32 - width)).rotate_right(bit);
                let value = (new_field << (32 - width)).rotate_right(bit);
                self.state.d[r] = (self.state.d[r] & !mask) | value;
            }
            Some((addr, bit, bytes, window)) => {
                let shift = 64 - bit - width;
                let mask = (low_mask as u64) << shift;
                let window = (window & !mask) | ((new_field as u64) << shift);
                for i in 0..bytes {
                    self.bus_write_u8(bus, addr.wrapping_add(i), (window >> (56 - 8 * i)) as u8)?;
                }
            }
        }
        Ok(())
    }

    /// MC68030 MMU instructions (`PMOVE`, `PFLUSH`, `PLOAD`, `PTEST`).
    ///
    /// The registers are kept, but addresses are not translated, the translation cache is not
    /// modelled, and `PTEST` reports every address as invalid.
    fn exec_mmu<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        if !self.state.is_supervisor() {
            return self.privilege_violation(bus);
        }
        let ext = self.fetch_u16(bus)?;
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;
        const MMUSR_INVALID: u16 = 0x0400;
        match ext >> 13 {
            // PFLUSH, PFLUSHA, PLOAD: no translation cache to flush or load.
            1 => {
                // PLOAD (mode 0) and PFLUSH by address (mode 6) take an effective address.
                if matches!((ext >> 10) & 7, 0 | 6) {
                    self.resolve_ea(bus, mode, reg, DataSize::Byte)?;
                }
                Ok(())
            }
            // PTEST
            4 => {
                self.resolve_ea(bus, mode, reg, DataSize::Byte)?;
                self.state.mmu.mmusr = MMUSR_INVALID;
                if (ext & 0x0100) != 0 {
                    // The A bit asks for the last descriptor address in An.
                    self.state.a[((ext >> 5) & 7) as usize] = 0;
                }
                Ok(())
            }
            // PMOVE TT0/TT1 (0), TC/SRP/CRP (2), MMUSR (3)
            group @ (0 | 2 | 3) => {
                #[derive(PartialEq)]
                enum Reg {
                    Tc,
                    Srp,
                    Crp,
                    Tt0,
                    Tt1,
                    Mmusr,
                }
                let target = match (group, (ext >> 10) & 7) {
                    (2, 0) => Reg::Tc,
                    (2, 2) => Reg::Srp,
                    (2, 3) => Reg::Crp,
                    (0, 2) => Reg::Tt0,
                    (0, 3) => Reg::Tt1,
                    (3, 0) => Reg::Mmusr,
                    _ => return self.illegal(bus),
                };
                let size = match target {
                    Reg::Mmusr => DataSize::Word,
                    _ => DataSize::Long,
                };
                let quad = matches!(target, Reg::Srp | Reg::Crp);
                let loc = self.resolve_ea(bus, mode, reg, if quad { DataSize::Long } else { size })?;
                if quad && !matches!(loc, EaLocation::Memory(_)) {
                    return self.illegal(bus);
                }
                let to_memory = (ext & 0x0200) != 0;
                if to_memory {
                    match target {
                        Reg::Srp | Reg::Crp => {
                            let EaLocation::Memory(addr) = loc else { unreachable!() };
                            let value = if target == Reg::Srp {
                                self.state.mmu.srp
                            } else {
                                self.state.mmu.crp
                            };
                            self.bus_write_u32(bus, addr, (value >> 32) as u32)?;
                            self.bus_write_u32(bus, addr.wrapping_add(4), value as u32)?;
                        }
                        Reg::Tc => self.write_ea(bus, loc, size, self.state.mmu.tc)?,
                        Reg::Tt0 => self.write_ea(bus, loc, size, self.state.mmu.tt0)?,
                        Reg::Tt1 => self.write_ea(bus, loc, size, self.state.mmu.tt1)?,
                        Reg::Mmusr => self.write_ea(bus, loc, size, self.state.mmu.mmusr as u32)?,
                    }
                } else {
                    match target {
                        Reg::Srp | Reg::Crp => {
                            let EaLocation::Memory(addr) = loc else { unreachable!() };
                            let hi = self.bus_read_u32(bus, addr)? as u64;
                            let lo = self.bus_read_u32(bus, addr.wrapping_add(4))? as u64;
                            if target == Reg::Srp {
                                self.state.mmu.srp = (hi << 32) | lo;
                            } else {
                                self.state.mmu.crp = (hi << 32) | lo;
                            }
                        }
                        Reg::Tc => self.state.mmu.tc = self.read_ea(bus, loc, size)?,
                        Reg::Tt0 => self.state.mmu.tt0 = self.read_ea(bus, loc, size)?,
                        Reg::Tt1 => self.state.mmu.tt1 = self.read_ea(bus, loc, size)?,
                        Reg::Mmusr => self.state.mmu.mmusr = self.read_ea(bus, loc, size)? as u16,
                    }
                }
                Ok(())
            }
            _ => self.illegal(bus),
        }
    }
}
