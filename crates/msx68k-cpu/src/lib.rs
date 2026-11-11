//! Monomorphized Motorola 68000 / 68030 CPU core (`msx68k-cpu`).
//!
//! Provides [`CpuCore<P>`] parameterized over [`CpuPolicy`] ([`TrackedPolicy`] for
//! cycle-accurate 10/16/24 MHz execution with memory/bus wait-state accounting,
//! [`BurstPolicy`] for zero-overhead uncapped execution, and [`Burst030Policy`] for
//! uncapped MC68030 execution).

pub mod alu;
pub mod bus;
pub mod core;
pub mod policy;
pub mod state;

pub use bus::{Bus, BusFault, NoopTrapHandler, TrapHandler, TrapResult};
pub use core::{CpuCore, StepOutcome};
pub use policy::{Burst030Policy, BurstPolicy, CpuPolicy, TrackedPolicy};
pub use state::{
    CpuModel, CpuState, DataSize, ExceptionVector, SR_C, SR_CCR_MASK, SR_IPM_MASK, SR_M, SR_N,
    SR_S, SR_T, SR_T0, SR_VALID_MASK, SR_VALID_MASK_030, SR_V, SR_X, SR_Z,
};

#[cfg(test)]
mod tests {
    use super::*;

    struct FlatBus {
        mem: Vec<u8>,
        wait_per_word: u32,
    }

    impl FlatBus {
        fn new(size: usize, wait_per_word: u32) -> Self {
            Self {
                mem: vec![0; size],
                wait_per_word,
            }
        }

        fn write_words(&mut self, addr: u32, words: &[u16]) {
            let mut p = addr as usize;
            for &w in words {
                self.mem[p] = (w >> 8) as u8;
                self.mem[p + 1] = w as u8;
                p += 2;
            }
        }
    }

    impl Bus for FlatBus {
        fn read_u8(&mut self, addr: u32, _supervisor: bool) -> Result<u8, BusFault> {
            self.mem.get(addr as usize).copied().ok_or(BusFault::BusError {
                addr,
                is_write: false,
            })
        }

        fn read_u16(&mut self, addr: u32, supervisor: bool) -> Result<u16, BusFault> {
            let hi = self.read_u8(addr, supervisor)? as u16;
            let lo = self.read_u8(addr + 1, supervisor)? as u16;
            Ok((hi << 8) | lo)
        }

        fn write_u8(&mut self, addr: u32, val: u8, _supervisor: bool) -> Result<(), BusFault> {
            let slot = self.mem.get_mut(addr as usize).ok_or(BusFault::BusError {
                addr,
                is_write: true,
            })?;
            *slot = val;
            Ok(())
        }

        fn write_u16(&mut self, addr: u32, val: u16, supervisor: bool) -> Result<(), BusFault> {
            self.write_u8(addr, (val >> 8) as u8, supervisor)?;
            self.write_u8(addr + 1, val as u8, supervisor)
        }

        fn wait_cycles_16(&mut self, _addr: u32, _is_write: bool) -> u32 {
            self.wait_per_word
        }
    }

    #[test]
    fn test_tracked_vs_burst_policy_monomorphization() {
        let mut bus = FlatBus::new(0x10000, 2);
        // Reset vectors: SSP = $8000, PC = $0400
        bus.write_words(0x0000, &[0x0000, 0x8000, 0x0000, 0x0400]);
        // At $0400:
        //   MOVEQ #5, D0       (0x7005) -> 1 fetch (4 + 2 wait = 6 cycles)
        //   ADDQ.L #3, D0      (0x5680) -> 1 fetch (8 + 2 wait = 10 cycles)
        //   MOVE.L D0, ($1000) (0x21C0, 0x1000) -> 2 fetches + 2 word writes
        bus.write_words(0x0400, &[0x7005, 0x5680, 0x21C0, 0x1000]);

        let mut tracked = CpuCore::<TrackedPolicy>::new();
        tracked.reset(&mut bus).unwrap();
        let mut noop = NoopTrapHandler;

        let s1 = tracked.step(&mut bus, &mut noop);
        assert_eq!(s1.cycles, 4 + 2);
        assert_eq!(tracked.state.d[0], 5);

        let s2 = tracked.step(&mut bus, &mut noop);
        assert_eq!(s2.cycles, 8 + 2);
        assert_eq!(tracked.state.d[0], 8);

        let s3 = tracked.step(&mut bus, &mut noop);
        // Base: fetch op (0) + resolve_ea abs.w for Long (12) + move base (4) = 16
        // Wait: 2 fetches (2*2=4) + 1 long write (2*2=4) = 8 -> total 24 cycles
        assert_eq!(s3.cycles, 16 + 8);
        assert_eq!(bus.read_u32(0x1000, true).unwrap(), 8);

        // Now run the exact same program in BurstPolicy: cycles must be 0
        let mut burst = CpuCore::<BurstPolicy>::new();
        burst.reset(&mut bus).unwrap();
        assert_eq!(burst.step(&mut bus, &mut noop).cycles, 0);
        assert_eq!(burst.step(&mut bus, &mut noop).cycles, 0);
        assert_eq!(burst.step(&mut bus, &mut noop).cycles, 0);
        assert_eq!(burst.state.d[0], 8);
        assert_eq!(burst.state.total_cycles, 0);
    }

    #[test]
    fn test_dbra_loop_and_subroutine_call() {
        let mut bus = FlatBus::new(0x10000, 0);
        bus.write_words(0x0000, &[0x0000, 0x8000, 0x0000, 0x0400]);
        // $0400: MOVEQ #0, D0        (0x7000)
        // $0402: MOVEQ #4, D1        (0x7204) -> loop 5 times (4, 3, 2, 1, 0 -> -1)
        // $0404: BSR.S $040C         (0x6106)
        // $0406: DBRA D1, $0404      (0x51C9, 0xFFFC)
        // $040A: NOP                 (0x4E71)
        // $040C: ADDQ.L #2, D0       (0x5480)
        // $040E: RTS                 (0x4E75)
        bus.write_words(
            0x0400,
            &[0x7000, 0x7204, 0x6106, 0x51C9, 0xFFFC, 0x4E71, 0x5480, 0x4E75],
        );

        let mut cpu = CpuCore::<TrackedPolicy>::new();
        cpu.reset(&mut bus).unwrap();
        let mut noop = NoopTrapHandler;

        while cpu.state.pc != 0x040A {
            cpu.step(&mut bus, &mut noop);
        }
        assert_eq!(cpu.state.d[0], 10);
        assert_eq!(cpu.state.d[1] & 0xFFFF, 0xFFFF);
    }

    #[test]
    fn test_f_line_and_a_line_mixed_mode_trap_hooks() {
        struct CustomTrap {
            last_f_line: u16,
            last_a_line: u16,
        }
        impl TrapHandler<FlatBus> for CustomTrap {
            fn on_f_line(
                &mut self,
                _state: &mut CpuState,
                _bus: &mut FlatBus,
                opcode: u16,
                _pc: u32,
            ) -> TrapResult {
                self.last_f_line = opcode;
                TrapResult::Handled { extra_cycles: 10 }
            }
            fn on_a_line(
                &mut self,
                _state: &mut CpuState,
                _bus: &mut FlatBus,
                opcode: u16,
                _pc: u32,
            ) -> TrapResult {
                self.last_a_line = opcode;
                TrapResult::Handled { extra_cycles: 6 }
            }
        }

        let mut bus = FlatBus::new(0x10000, 0);
        bus.write_words(0x0000, &[0x0000, 0x8000, 0x0000, 0x0400]);
        // $0400: DOS _EXIT ($FF00), A-Line ($A123)
        bus.write_words(0x0400, &[0xFF00, 0xA123]);

        let mut cpu = CpuCore::<TrackedPolicy>::new();
        cpu.reset(&mut bus).unwrap();
        let mut trap = CustomTrap {
            last_f_line: 0,
            last_a_line: 0,
        };

        let out1 = cpu.step(&mut bus, &mut trap);
        assert_eq!(trap.last_f_line, 0xFF00);
        assert_eq!(out1.cycles, 14);
        assert_eq!(cpu.state.pc, 0x0402);

        let out2 = cpu.step(&mut bus, &mut trap);
        assert_eq!(trap.last_a_line, 0xA123);
        assert_eq!(out2.cycles, 10);
        assert_eq!(cpu.state.pc, 0x0404);
    }

    #[test]
    fn test_movem_predecrement_and_postincrement() {
        let mut bus = FlatBus::new(0x10000, 0);
        bus.write_words(0x0000, &[0x0000, 0x8000, 0x0000, 0x0400]);
        // MOVEM.L D0-D1/A0, -(SP)  -> mask for -(An): D0=bit15(0x8000), D1=bit14(0x4000), A0=bit7(0x0080) => 0xC080
        // MOVEM.L (SP)+, D2-D3/A1  -> mask for (An)+: D2=bit2(0x0004), D3=bit3(0x0008), A1=bit9(0x0200) => 0x020C
        bus.write_words(0x0400, &[0x48E7, 0xC080, 0x4CDF, 0x020C]);

        let mut cpu = CpuCore::<TrackedPolicy>::new();
        cpu.reset(&mut bus).unwrap();
        cpu.state.d[0] = 0x1122_3344;
        cpu.state.d[1] = 0x5566_7788;
        cpu.state.a[0] = 0x00AA_BBCC;

        let mut noop = NoopTrapHandler;
        cpu.step(&mut bus, &mut noop);
        assert_eq!(cpu.state.a[7], 0x8000 - 12);

        cpu.step(&mut bus, &mut noop);
        assert_eq!(cpu.state.a[7], 0x8000);
        assert_eq!(cpu.state.d[2], 0x1122_3344);
        assert_eq!(cpu.state.d[3], 0x5566_7788);
        assert_eq!(cpu.state.a[1], 0x00AA_BBCC);
    }
}
