//! MC68030 instruction set, addressing mode, and exception frame tests.
//!
//! Encodings come from the GNU assembler (`m68k-linux-gnu-as -m68030`).

use msx68k_cpu::{
    Burst030Policy, Bus, BusFault, CpuCore, CpuModel, CpuState, NoopTrapHandler, SR_C, SR_M,
    SR_N, SR_S, SR_V, SR_Z,
};

const CODE: u32 = 0x1000;
const STACK: u32 = 0x8000;
const DATA: u32 = 0x4000;

struct FlatBus {
    mem: Vec<u8>,
    /// A write-only register: reading it raises a bus error.
    write_only: u32,
}

impl Bus for FlatBus {
    fn read_u8(&mut self, addr: u32, _supervisor: bool) -> Result<u8, BusFault> {
        if addr == self.write_only {
            return Err(BusFault::BusError {
                addr,
                is_write: false,
            });
        }
        self.mem.get(addr as usize).copied().ok_or(BusFault::BusError {
            addr,
            is_write: false,
        })
    }

    fn read_u16(&mut self, addr: u32, supervisor: bool) -> Result<u16, BusFault> {
        assert_eq!(addr & 1, 0, "the core must split misaligned words");
        Ok(((self.read_u8(addr, supervisor)? as u16) << 8) | self.read_u8(addr + 1, supervisor)? as u16)
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
        assert_eq!(addr & 1, 0, "the core must split misaligned words");
        self.write_u8(addr, (val >> 8) as u8, supervisor)?;
        self.write_u8(addr + 1, val as u8, supervisor)
    }
}

impl FlatBus {
    fn words(&mut self, addr: u32, words: &[u16]) {
        for (i, w) in words.iter().enumerate() {
            let a = addr as usize + i * 2;
            self.mem[a..a + 2].copy_from_slice(&w.to_be_bytes());
        }
    }

    fn long(&mut self, addr: u32, val: u32) {
        self.mem[addr as usize..addr as usize + 4].copy_from_slice(&val.to_be_bytes());
    }

    fn peek_u16(&self, addr: u32) -> u16 {
        u16::from_be_bytes(self.mem[addr as usize..addr as usize + 2].try_into().unwrap())
    }

    fn peek_u32(&self, addr: u32) -> u32 {
        u32::from_be_bytes(self.mem[addr as usize..addr as usize + 4].try_into().unwrap())
    }
}

fn machine(code: &[u16]) -> (CpuCore<Burst030Policy>, FlatBus) {
    let mut bus = FlatBus {
        mem: vec![0; 0x10000],
        write_only: u32::MAX,
    };
    bus.long(0, STACK);
    bus.long(4, CODE);
    bus.words(CODE, code);
    let mut cpu = CpuCore::<Burst030Policy>::from_state(CpuState::with_model(CpuModel::Mc68030));
    cpu.reset(&mut bus).unwrap();
    (cpu, bus)
}

fn step(cpu: &mut CpuCore<Burst030Policy>, bus: &mut FlatBus, count: usize) {
    for _ in 0..count {
        cpu.step(bus, &mut NoopTrapHandler);
    }
}

#[test]
fn long_multiply_and_divide() {
    // mulu.l d1,d2 / muls.l d1,d3:d2 / divs.l d1,d3:d2 / divul.l d1,d3:d2
    let (mut cpu, mut bus) = machine(&[0x4C01, 0x2000, 0x4C01, 0x2C03, 0x4C41, 0x2C03, 0x4C41, 0x2003]);
    cpu.state.d[1] = 0x10000;
    cpu.state.d[2] = 0x12345;
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.d[2], 0x2345_0000);
    assert!(cpu.state.flag(SR_V), "32-bit product overflowed");

    cpu.state.d[1] = 0xFFFF_FFFE; // -2
    cpu.state.d[2] = 0x4000_0000;
    step(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.state.d[3], cpu.state.d[2]), (0xFFFF_FFFF, 0x8000_0000));
    assert!(cpu.state.flag(SR_N) && !cpu.state.flag(SR_V));

    // -0x8000_0000 / -2 in 64 bits.
    step(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.state.d[2], cpu.state.d[3]), (0x4000_0000, 0));

    cpu.state.d[1] = 7;
    cpu.state.d[2] = 100;
    step(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.state.d[2], cpu.state.d[3]), (14, 2));
}

#[test]
fn divide_by_zero_pushes_a_format_2_frame() {
    // divu.l d1,d2
    let (mut cpu, mut bus) = machine(&[0x4C41, 0x2002]);
    bus.long(5 * 4, 0x2000);
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.pc, 0x2000);
    let sp = cpu.state.a[7];
    assert_eq!(sp, STACK - 12);
    assert_eq!(bus.peek_u16(sp), SR_S | 0x0700);
    assert_eq!(bus.peek_u32(sp + 2), CODE + 4, "return to the next instruction");
    assert_eq!(bus.peek_u16(sp + 6), 0x2014);
    assert_eq!(bus.peek_u32(sp + 8), CODE, "address of the faulting instruction");
}

#[test]
fn extb_and_long_link() {
    // extb.l d0 / link.l a6,#-0x10000
    let (mut cpu, mut bus) = machine(&[0x49C0, 0x480E, 0xFFFF, 0x0000]);
    cpu.state.d[0] = 0x1234_5680;
    cpu.state.a[6] = 0xA6A6;
    step(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.state.d[0], 0xFFFF_FF80);
    assert_eq!(cpu.state.a[6], STACK - 4);
    assert_eq!(cpu.state.a[7], (STACK - 4).wrapping_sub(0x10000));
    assert_eq!(bus.peek_u32(STACK - 4), 0xA6A6);
}

#[test]
fn bit_fields() {
    let (mut cpu, mut bus) = machine(&[
        0xE9C0, 0x1108, // bfextu d0{#4:#8},d1
        0xEBD0, 0x2308, // bfexts (a0){#12:#8},d2
        0xEFD0, 0x390C, // bfins d3,(a0){d4:#12}
        0xEDC0, 0x5000, // bfffo d0{#0:#0},d5
        0xEED0, 0x0005, // bfset (a0){#0:#5}
    ]);
    cpu.state.d[0] = 0x0AB0_0100;
    cpu.state.a[0] = DATA;
    bus.words(DATA, &[0x0008, 0x0000]);
    step(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.state.d[1], 0xAB);
    assert_eq!(cpu.state.d[2], 0xFFFF_FF80);
    assert!(cpu.state.flag(SR_N));

    // Insert 12 bits starting 3 bits before the base byte.
    cpu.state.d[3] = 0xFFF;
    cpu.state.d[4] = (-3i32) as u32;
    bus.words(DATA - 2, &[0, 0, 0]);
    step(&mut cpu, &mut bus, 1);
    assert_eq!(bus.peek_u32(DATA - 2), 0x0007_FF80);
    assert!(cpu.state.flag(SR_N) && !cpu.state.flag(SR_Z));

    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.d[5], 4, "first set bit of 0x0AB00100");

    bus.words(DATA, &[0x0000]);
    step(&mut cpu, &mut bus, 1);
    assert_eq!(bus.peek_u16(DATA), 0xF800);
    assert!(cpu.state.flag(SR_Z));
}

#[test]
fn compare_and_swap() {
    let (mut cpu, mut bus) = machine(&[
        0x0ED0, 0x0040, // cas.l d0,d1,(a0)
        0x0ED0, 0x0040, // cas.l d0,d1,(a0)
        0x0EFC, 0x8080, 0x90C1, // cas2.l d0:d1,d2:d3,(a0):(a1)
    ]);
    cpu.state.a[0] = DATA;
    cpu.state.a[1] = DATA + 4;
    bus.long(DATA, 5);
    cpu.state.d[0] = 5;
    cpu.state.d[1] = 9;
    step(&mut cpu, &mut bus, 1);
    assert_eq!(bus.peek_u32(DATA), 9);
    assert!(cpu.state.flag(SR_Z));
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.d[0], 9, "a failed compare loads the operand");
    assert!(!cpu.state.flag(SR_Z));

    bus.long(DATA + 4, 7);
    cpu.state.d[1] = 7;
    cpu.state.d[2] = 0x22;
    cpu.state.d[3] = 0x33;
    step(&mut cpu, &mut bus, 1);
    assert_eq!((bus.peek_u32(DATA), bus.peek_u32(DATA + 4)), (0x22, 0x33));
}

#[test]
fn bounds_checks() {
    // cmp2.w (a0),d0 / chk2.l (a0),a1
    let (mut cpu, mut bus) = machine(&[0x02D0, 0x0000, 0x04D0, 0x9800]);
    bus.long(6 * 4, 0x2000);
    cpu.state.a[0] = DATA;
    bus.words(DATA, &[0xFFF0, 0x0010]); // -16..16 as words
    cpu.state.d[0] = 0xFFF0;
    step(&mut cpu, &mut bus, 1);
    assert!(cpu.state.flag(SR_Z) && !cpu.state.flag(SR_C));

    bus.long(DATA, 0x100);
    bus.long(DATA + 4, 0x200);
    cpu.state.a[1] = 0x201;
    step(&mut cpu, &mut bus, 1);
    assert!(cpu.state.flag(SR_C));
    assert_eq!(cpu.state.pc, 0x2000, "CHK2 traps when out of bounds");
    assert_eq!(bus.peek_u16(cpu.state.a[7] + 6), 0x2018);
}

#[test]
fn pack_and_unpack() {
    // pack d0,d1,#0 / unpk d0,d1,#0x3030
    let (mut cpu, mut bus) = machine(&[0x8340, 0x0000, 0x8380, 0x3030]);
    cpu.state.d[0] = 0x0507;
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.d[1] & 0xFF, 0x57);
    cpu.state.d[0] = 0x42;
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.d[1] & 0xFFFF, 0x3432);
}

#[test]
fn vector_base_and_format_0_frames() {
    let (mut cpu, mut bus) = machine(&[
        0x4E7B, 0x0801, // movec d0,vbr
        0x4E7A, 0x9801, // movec vbr,a1
        0x4E4F, // trap #15
    ]);
    cpu.state.d[0] = 0x3000;
    bus.long(0x3000 + 47 * 4, 0x2000);
    bus.words(0x2000, &[0x4E73]); // rte
    step(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.state.a[1], 0x3000);
    assert_eq!(cpu.state.pc, 0x2000);
    assert_eq!(bus.peek_u16(cpu.state.a[7] + 6), 47 * 4);
    step(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.state.pc, cpu.state.a[7]), (CODE + 10, STACK));
}

#[test]
fn rtd_trapcc_and_long_branch() {
    let (mut cpu, mut bus) = machine(&[
        0x56FA, 0x0001, // trapne.w #1 (Z set: no trap)
        0x60FF, 0x0000, 0x0010, // bra.l .+0x12
    ]);
    cpu.state.set_ccr(SR_Z as u8);
    step(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.state.pc, CODE + 4 + 2 + 0x10);

    let (mut cpu, mut bus) = machine(&[0x4E74, 0x0008]); // rtd #8
    cpu.state.a[7] = STACK - 4;
    bus.long(STACK - 4, 0x2222);
    step(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.state.pc, cpu.state.a[7]), (0x2222, STACK + 8));
}

#[test]
fn indexed_addressing_modes() {
    let (mut cpu, mut bus) = machine(&[
        0x2030, 0x1C00, // move.l (0,a0,d1.l*4),d0
        0x2430, 0x1326, 0x0008, 0x0004, // move.l ([8,a0],d1.w*2,4),d2
        0x2630, 0x1D12, 0x0010, // move.l ([a0,d1.l*4],16),d3
    ]);
    cpu.state.a[0] = DATA;
    cpu.state.d[1] = 2;
    bus.long(DATA + 8, 0x5000);
    bus.long(0x5000 + 4 + 4, 0xBBBB);
    bus.long(0x5000 + 16, 0xCCCC);
    step(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.state.d[0], 0x5000);
    assert_eq!(cpu.state.d[2], 0xBBBB);
    assert_eq!(cpu.state.d[3], 0xCCCC);
}

#[test]
fn misaligned_data_and_privileged_move_from_sr() {
    let (mut cpu, mut bus) = machine(&[
        0x2010, // move.l (a0),d0
        0x2081, // move.l d1,(a0)
        0x40C0, // move.w sr,d0
    ]);
    bus.long(8 * 4, 0x2000);
    cpu.state.a[0] = DATA + 1;
    bus.long(DATA + 1, 0x1122_3344);
    cpu.state.d[1] = 0x5566_7788;
    step(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.state.d[0], 0x1122_3344);
    assert_eq!(bus.peek_u32(DATA + 1), 0x5566_7788);

    cpu.state.set_sr(0);
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.pc, 0x2000, "MOVE from SR is privileged");
}

#[test]
fn odd_program_counter_pushes_a_long_bus_fault_frame() {
    let (mut cpu, mut bus) = machine(&[0x4EF9, 0x0000, 0x1235]); // jmp $1235
    bus.long(3 * 4, 0x2000);
    step(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.state.pc, 0x2000);
    let sp = cpu.state.a[7];
    assert_eq!(sp, STACK - 92);
    assert_eq!(bus.peek_u16(sp + 6), 0xB00C);
    assert_eq!(bus.peek_u32(sp + 16), 0x1235);
}

#[test]
fn interrupt_on_master_stack_leaves_a_throwaway_frame() {
    let (mut cpu, mut bus) = machine(&[0x4E71, 0x4E71]);
    bus.long(0x64 + 4 * 4, 0x2000); // level 5 autovector
    bus.words(0x2000, &[0x4E73]); // rte
    cpu.state.set_msp(0x6000);
    cpu.state.set_sr(SR_S | SR_M);
    cpu.state.set_ipl(5);
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.pc, 0x2000);
    assert_eq!(cpu.state.a[7], STACK - 8, "handler runs on the interrupt stack");
    assert_eq!(bus.peek_u16(STACK - 8 + 6), 0x1074);
    assert_eq!(bus.peek_u16(0x6000 - 8 + 6), 0x0074);

    cpu.state.set_ipl(0);
    step(&mut cpu, &mut bus, 1);
    assert_eq!((cpu.state.pc, cpu.state.a[7]), (CODE, 0x6000));
    assert_eq!(cpu.state.sr & (SR_S | SR_M), SR_S | SR_M);
    assert_eq!(cpu.state.get_ssp(), STACK);
}

#[test]
fn clr_writes_without_reading() {
    let (mut cpu, mut bus) = machine(&[0x4239, 0x0000, 0x6001]); // clr.b $6001
    bus.long(2 * 4, 0x2000);
    bus.write_only = 0x6001;
    bus.mem[0x6001] = 0xAA;
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.pc, CODE + 6, "no bus error from reading the register");
    assert_eq!(bus.mem[0x6001], 0);
    assert!(cpu.state.flag(SR_Z));
}

#[test]
fn mmu_registers_move_without_translation() {
    let (mut cpu, mut bus) = machine(&[
        0xF000, 0x2400, // pflusha
        0xF010, 0x4C00, // pmove (a0),crp
        0xF011, 0x4000, // pmove (a1),tc
        0xF012, 0x4E00, // pmove crp,(a2)
        0xF012, 0x6200, // pmove mmusr,(a2)
    ]);
    cpu.state.a[0] = DATA;
    cpu.state.a[1] = DATA + 8;
    cpu.state.a[2] = DATA + 16;
    bus.long(DATA, 0x7FFF_0002);
    bus.long(DATA + 4, 0x0000_3000);
    bus.long(DATA + 8, 0x80D8_3440);
    step(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.state.mmu.crp, 0x7FFF_0002_0000_3000);
    assert_eq!(cpu.state.mmu.tc, 0x80D8_3440);
    assert_eq!((bus.peek_u32(DATA + 16), bus.peek_u32(DATA + 20)), (0x7FFF_0002, 0x3000));
    assert_eq!(cpu.state.pc, CODE + 16);

    // The MMU instructions are privileged.
    bus.long(8 * 4, 0x2000);
    cpu.state.set_sr(0);
    step(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.state.pc, 0x2000);
}
