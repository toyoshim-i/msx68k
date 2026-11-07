//! Monomorphized MC68000 instruction decoder and execution engine.

use std::marker::PhantomData;

use crate::alu::{
    alu_abcd, alu_add, alu_addx, alu_sbcd, alu_shift_rotate, alu_sub, alu_subx, eval_cc,
    muls_cycles, mulu_cycles, set_nz_clear_vc,
};
use crate::bus::{Bus, BusFault, TrapHandler, TrapResult};
use crate::policy::CpuPolicy;
use crate::state::{
    CpuState, DataSize, ExceptionVector, SR_C, SR_CCR_MASK, SR_N, SR_S, SR_T, SR_V, SR_X, SR_Z,
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
            _policy: PhantomData,
        }
    }

    /// Creates an MC68000 CPU core wrapping an existing [`CpuState`].
    pub const fn from_state(state: CpuState) -> Self {
        Self {
            state,
            step_cycles: 0,
            _policy: PhantomData,
        }
    }

    /// Consumes the core and returns the inner [`CpuState`].
    pub const fn into_state(self) -> CpuState {
        self.state
    }

    #[inline(always)]
    fn add_cycles(&mut self, cycles: u32) {
        if P::TRACK_CYCLES {
            self.step_cycles = self.step_cycles.wrapping_add(cycles);
        }
    }

    /// Performs a hardware reset by loading initial `SSP` from `$000000` and `PC` from `$000004`.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) -> Result<u32, BusFault> {
        self.step_cycles = 0;
        self.state.stopped = false;
        self.state.nmi_pending = false;
        self.state.sr = SR_S | 0x0700;
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

    #[inline(always)]
    fn bus_read_u8<B: Bus>(&mut self, bus: &mut B, addr: u32) -> Result<u8, BusFault> {
        if P::TRACK_CYCLES {
            let w = bus.wait_cycles_16(addr, false);
            self.add_cycles(w);
        }
        bus.read_u8(addr, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_read_u16<B: Bus>(&mut self, bus: &mut B, addr: u32) -> Result<u16, BusFault> {
        if self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: false,
            });
        }
        if P::TRACK_CYCLES {
            let w = bus.wait_cycles_16(addr, false);
            self.add_cycles(w);
        }
        bus.read_u16(addr, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_read_u32<B: Bus>(&mut self, bus: &mut B, addr: u32) -> Result<u32, BusFault> {
        if self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: false,
            });
        }
        if P::TRACK_CYCLES {
            let w0 = bus.wait_cycles_16(addr, false);
            let w1 = bus.wait_cycles_16(addr.wrapping_add(2), false);
            self.add_cycles(w0 + w1);
        }
        bus.read_u32(addr, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_write_u8<B: Bus>(&mut self, bus: &mut B, addr: u32, val: u8) -> Result<(), BusFault> {
        if P::TRACK_CYCLES {
            let w = bus.wait_cycles_16(addr, true);
            self.add_cycles(w);
        }
        bus.write_u8(addr, val, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_write_u16<B: Bus>(&mut self, bus: &mut B, addr: u32, val: u16) -> Result<(), BusFault> {
        if self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: true,
            });
        }
        if P::TRACK_CYCLES {
            let w = bus.wait_cycles_16(addr, true);
            self.add_cycles(w);
        }
        bus.write_u16(addr, val, self.state.is_supervisor())
    }

    #[inline(always)]
    fn bus_write_u32<B: Bus>(&mut self, bus: &mut B, addr: u32, val: u32) -> Result<(), BusFault> {
        if self.state.check_address_error && (addr & 1) != 0 {
            return Err(BusFault::AddressError {
                addr,
                is_write: true,
            });
        }
        if P::TRACK_CYCLES {
            let w0 = bus.wait_cycles_16(addr, true);
            let w1 = bus.wait_cycles_16(addr.wrapping_add(2), true);
            self.add_cycles(w0 + w1);
        }
        bus.write_u32(addr, val, self.state.is_supervisor())
    }

    #[inline(always)]
    fn fetch_u16<B: Bus>(&mut self, bus: &mut B) -> Result<u16, BusFault> {
        let pc = self.state.pc;
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
        let new_sr = (saved_sr & !(SR_T | 0x0700)) | SR_S | ((ipl as u16) << 8);
        self.state.set_sr(new_sr);

        let vec_no = bus.acknowledge_interrupt(ipl).unwrap_or(24 + ipl);
        self.push_u32(bus, self.state.pc)?;
        self.push_u16(bus, saved_sr)?;
        let handler = self.bus_read_u32(bus, (vec_no as u32) * 4)?;
        self.state.pc = handler;
        self.add_cycles(44);
        Ok(true)
    }

    /// Raises a standard Group 1/2 exception (4-word stack frame: `SR`, `PC`).
    pub fn raise_exception<B: Bus>(
        &mut self,
        bus: &mut B,
        vector_no: u8,
        return_pc: u32,
        base_cycles: u32,
    ) -> Result<(), BusFault> {
        self.state.stopped = false;
        let saved_sr = self.state.sr;
        self.state.set_sr((saved_sr & !SR_T) | SR_S);
        self.push_u32(bus, return_pc)?;
        self.push_u16(bus, saved_sr)?;
        let target = self.bus_read_u32(bus, (vector_no as u32) * 4)?;
        self.state.pc = target;
        self.add_cycles(base_cycles);
        Ok(())
    }

    /// Raises a Group 0 fault (Bus Error or Address Error, 7-word MC68000 stack frame).
    fn raise_group0_fault<B: Bus>(&mut self, bus: &mut B, fault: BusFault) {
        self.state.stopped = false;
        let saved_sr = self.state.sr;
        self.state.set_sr((saved_sr & !SR_T) | SR_S);

        let (vec_no, fault_addr, is_write) = match fault {
            BusFault::BusError { addr, is_write } => (ExceptionVector::BusError as u8, addr, is_write),
            BusFault::AddressError { addr, is_write } => {
                (ExceptionVector::AddressError as u8, addr, is_write)
            }
        };

        let rw_bit = if is_write { 0 } else { 1 << 4 };
        let fc = if (saved_sr & SR_S) != 0 { 5u16 } else { 1u16 };
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

    /// Executes a single MC68000 instruction (or interrupt/stop cycle).
    pub fn step<B: Bus, T: TrapHandler>(
        &mut self,
        bus: &mut B,
        trap_handler: &mut T,
    ) -> StepOutcome {
        self.step_cycles = 0;

        match self.check_interrupts(bus) {
            Ok(true) => {
                if P::TRACK_CYCLES {
                    self.state.total_cycles =
                        self.state.total_cycles.wrapping_add(self.step_cycles as u64);
                }
                return StepOutcome {
                    cycles: self.step_cycles,
                    stopped: self.state.stopped,
                };
            }
            Err(fault) => {
                self.raise_group0_fault(bus, fault);
                if P::TRACK_CYCLES {
                    self.state.total_cycles =
                        self.state.total_cycles.wrapping_add(self.step_cycles as u64);
                }
                return StepOutcome {
                    cycles: self.step_cycles,
                    stopped: self.state.stopped,
                };
            }
            Ok(false) => {}
        }

        if self.state.stopped {
            self.add_cycles(4);
            if P::TRACK_CYCLES {
                self.state.total_cycles =
                    self.state.total_cycles.wrapping_add(self.step_cycles as u64);
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
                let addr = self.calc_brief_ext(self.state.a[r], ext);
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
                    let addr = self.calc_brief_ext(base_pc, ext);
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

    #[inline]
    fn calc_brief_ext(&self, base: u32, ext: u16) -> u32 {
        let idx_reg = ((ext >> 12) & 7) as usize;
        let is_addr = (ext & 0x8000) != 0;
        let is_long = (ext & 0x0800) != 0;
        let raw_idx = if is_addr {
            self.state.a[idx_reg]
        } else {
            self.state.d[idx_reg]
        };
        let idx_val = if is_long {
            raw_idx as i32
        } else {
            (raw_idx as u16 as i16) as i32
        };
        let disp = (ext as u8 as i8) as i32;
        base.wrapping_add(disp as u32).wrapping_add(idx_val as u32)
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
    fn execute_one<B: Bus, T: TrapHandler>(
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
                match trap_handler.on_a_line(op, self.state.ir_pc) {
                    TrapResult::Handled { extra_cycles } => {
                        self.add_cycles(4 + extra_cycles);
                        Ok(())
                    }
                    TrapResult::PassToVector => self.raise_exception(
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
            0xF => {
                // F-Line ($Fxxx) Human68k DOS call / SX-Window API / FE-function
                match trap_handler.on_f_line(op, self.state.ir_pc) {
                    TrapResult::Handled { extra_cycles } => {
                        self.add_cycles(4 + extra_cycles);
                        Ok(())
                    }
                    TrapResult::PassToVector => self.raise_exception(
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

    fn exec_group_4<B: Bus, T: TrapHandler>(
        &mut self,
        bus: &mut B,
        trap_handler: &mut T,
        op: u16,
    ) -> Result<(), BusFault> {
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;

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
                let new_sr = self.pop_u16(bus)?;
                let new_pc = self.pop_u32(bus)?;
                self.state.pc = new_pc;
                self.state.set_sr(new_sr);
                self.add_cycles(20);
                return Ok(());
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
            match trap_handler.on_trap(trap_no, self.state.pc) {
                TrapResult::Handled { extra_cycles } => {
                    self.add_cycles(4 + extra_cycles);
                    return Ok(());
                }
                TrapResult::PassToVector => {
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
        let val = self.read_ea(bus, loc, size)?;

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

    fn exec_group_5<B: Bus>(&mut self, bus: &mut B, op: u16) -> Result<(), BusFault> {
        let mode = ((op >> 3) & 7) as u8;
        let reg = (op & 7) as u8;
        let cond = ((op >> 8) & 0xF) as u8;

        if (op & 0x00C0) == 0x00C0 {
            if mode == 1 {
                // DBcc Dn, <disp>
                let disp = self.fetch_u16(bus)? as i16 as i32;
                if !eval_cc(self.state.sr, cond) {
                    let r = reg as usize;
                    let low = (self.state.d[r] as u16).wrapping_sub(1);
                    self.state.write_d(r, DataSize::Word, low as u32);
                    if low != 0xFFFF {
                        self.state.pc = self.state.pc.wrapping_sub(2).wrapping_add(disp as u32);
                        self.add_cycles(10);
                    } else {
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
        } else {
            (disp8 as i8) as i32
        };

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

        let val = self.state.read_d(reg, size);
        let res = alu_shift_rotate(&mut self.state, size, val, count, left, kind);
        self.state.write_d(reg, size, res);
        let base = if size == DataSize::Long { 8 } else { 6 };
        self.add_cycles(base + 2 * count);
        Ok(())
    }
}
