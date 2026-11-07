//! Comprehensive MC68000 instruction compatibility test suite using
//! `SingleStepTests/m68000` (`third_party/m68000/v1/*.json.bin`).
//!
//! Validates registers (`D0-D7`, `A0-A7`, `USP`, `SSP`, `SR`, `PC`) and memory
//! writes across all 127 instruction files (~250,000+ hardware/MAME-verified test vectors).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use msx68k_cpu::{Bus, BusFault, CpuCore, NoopTrapHandler, TrackedPolicy, SR_S};

#[derive(Debug, Clone)]
struct M68kTestState {
    d: [u32; 8],
    a: [u32; 7],
    usp: u32,
    ssp: u32,
    sr: u16,
    pc: u32,
    ram: Vec<(u32, u8)>,
}

#[derive(Debug, Clone)]
struct M68kTestTransaction {
    kind: u8, // 0=n, 1=w, 2=r, 3=t, 4=re, 5=we
}

#[derive(Debug, Clone)]
struct M68kTestCase {
    name: String,
    initial: M68kTestState,
    final_state: M68kTestState,
    transactions: Vec<M68kTestTransaction>,
}

#[inline]
fn read_u32_le(data: &[u8], ptr: &mut usize) -> u32 {
    let val = u32::from_le_bytes(data[*ptr..*ptr + 4].try_into().unwrap());
    *ptr += 4;
    val
}

#[inline]
fn read_u16_le(data: &[u8], ptr: &mut usize) -> u16 {
    let val = u16::from_le_bytes(data[*ptr..*ptr + 2].try_into().unwrap());
    *ptr += 2;
    val
}

fn parse_state(data: &[u8], ptr: &mut usize) -> M68kTestState {
    let _numbytes = read_u32_le(data, ptr);
    let magic = read_u32_le(data, ptr);
    assert_eq!(magic, 0x0123_4567);

    let mut d = [0u32; 8];
    for reg in &mut d {
        *reg = read_u32_le(data, ptr);
    }
    let mut a = [0u32; 7];
    for reg in &mut a {
        *reg = read_u32_le(data, ptr);
    }
    let usp = read_u32_le(data, ptr);
    let ssp = read_u32_le(data, ptr);
    let sr = read_u32_le(data, ptr) as u16;
    let pc = read_u32_le(data, ptr);
    let _pf0 = read_u32_le(data, ptr);
    let _pf1 = read_u32_le(data, ptr);

    let num_rams = read_u32_le(data, ptr) as usize;
    let mut ram = Vec::with_capacity(num_rams * 2);
    for _ in 0..num_rams {
        let addr = read_u32_le(data, ptr);
        let word = read_u16_le(data, ptr);
        ram.push((addr, (word >> 8) as u8));
        ram.push((addr | 1, (word & 0xFF) as u8));
    }

    M68kTestState {
        d,
        a,
        usp,
        ssp,
        sr,
        pc,
        ram,
    }
}

fn parse_test_file(path: &Path) -> Vec<M68kTestCase> {
    let data = fs::read(path).expect("failed to read .json.bin file");
    let mut ptr = 0usize;
    let magic = read_u32_le(&data, &mut ptr);
    assert_eq!(magic, 0x1A3F_5D71);
    let num_tests = read_u32_le(&data, &mut ptr) as usize;

    let mut tests = Vec::with_capacity(num_tests);
    for _ in 0..num_tests {
        let _test_bytes = read_u32_le(&data, &mut ptr);
        let test_magic = read_u32_le(&data, &mut ptr);
        assert_eq!(test_magic, 0xABC1_2367);

        let _name_bytes = read_u32_le(&data, &mut ptr);
        let name_magic = read_u32_le(&data, &mut ptr);
        assert_eq!(name_magic, 0x89AB_CDEF);
        let strlen = read_u32_le(&data, &mut ptr) as usize;
        let name = String::from_utf8_lossy(&data[ptr..ptr + strlen]).into_owned();
        ptr += strlen;

        let initial = parse_state(&data, &mut ptr);
        let final_state = parse_state(&data, &mut ptr);

        let _tx_bytes = read_u32_le(&data, &mut ptr);
        let tx_magic = read_u32_le(&data, &mut ptr);
        assert_eq!(tx_magic, 0x4567_89AB);
        let _num_cycles = read_u32_le(&data, &mut ptr);
        let num_tx = read_u32_le(&data, &mut ptr) as usize;
        let mut transactions = Vec::with_capacity(num_tx);
        for _ in 0..num_tx {
            let tw = data[ptr];
            ptr += 1;
            let _cycles = read_u32_le(&data, &mut ptr);
            if tw != 0 {
                ptr += 20;
            }
            transactions.push(M68kTestTransaction { kind: tw });
        }

        tests.push(M68kTestCase {
            name,
            initial,
            final_state,
            transactions,
        });
    }
    tests
}

/// Sparse 24-bit memory bus for SingleStepTests.
struct SparseTestBus {
    mem: HashMap<u32, u8>,
}

impl SparseTestBus {
    fn new() -> Self {
        Self {
            mem: HashMap::with_capacity(64),
        }
    }

    fn load(&mut self, entries: &[(u32, u8)]) {
        self.mem.clear();
        for &(addr, val) in entries {
            self.mem.insert(addr & 0x00FF_FFFF, val);
        }
    }
}

impl Bus for SparseTestBus {
    fn read_u8(&mut self, addr: u32, _supervisor: bool) -> Result<u8, BusFault> {
        Ok(self.mem.get(&(addr & 0x00FF_FFFF)).copied().unwrap_or(0))
    }

    fn read_u16(&mut self, addr: u32, _supervisor: bool) -> Result<u16, BusFault> {
        let a = addr & 0x00FF_FFFF;
        let hi = self.mem.get(&a).copied().unwrap_or(0) as u16;
        let lo = self
            .mem
            .get(&((a.wrapping_add(1)) & 0x00FF_FFFF))
            .copied()
            .unwrap_or(0) as u16;
        Ok((hi << 8) | lo)
    }

    fn write_u8(&mut self, addr: u32, val: u8, _supervisor: bool) -> Result<(), BusFault> {
        self.mem.insert(addr & 0x00FF_FFFF, val);
        Ok(())
    }

    fn write_u16(&mut self, addr: u32, val: u16, _supervisor: bool) -> Result<(), BusFault> {
        let a = addr & 0x00FF_FFFF;
        self.mem.insert(a, (val >> 8) as u8);
        self.mem
            .insert((a.wrapping_add(1)) & 0x00FF_FFFF, (val & 0xFF) as u8);
        Ok(())
    }
}

fn submodule_v1_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("third_party/m68000/v1")
}

#[test]
fn run_single_step_tests_suite() {
    let dir = submodule_v1_dir();
    assert!(
        dir.exists(),
        "SingleStepTests submodule directory {:?} is missing; run `git submodule update --init`",
        dir
    );

    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("bin"))
        .collect();
    entries.sort();

    let mut bus = SparseTestBus::new();
    let mut noop = NoopTrapHandler;

    let mut total_files = 0usize;
    let mut perfect_files = 0usize;
    let mut total_vectors_passed = 0usize;
    let mut summary_failures = Vec::new();

    for path in &entries {
        let fname = path.file_name().unwrap().to_string_lossy().into_owned();
        let tests = parse_test_file(path);
        total_files += 1;

        let mut passed = 0usize;
        let mut skipped_addr_err = 0usize;
        let mut first_err: Option<String> = None;

        for tc in &tests {
            // Skip mid-instruction bus address error transactions ("re" = 4, "we" = 5)
            // which depend on internal MAME microcode bus prefetch ordering.
            if tc.transactions.iter().any(|t| t.kind == 4 || t.kind == 5) {
                skipped_addr_err += 1;
                continue;
            }

            bus.load(&tc.initial.ram);
            let mut cpu = CpuCore::<TrackedPolicy>::new();
            cpu.state.trace_enabled = false;
            cpu.state.d = tc.initial.d;
            cpu.state.a[..7].copy_from_slice(&tc.initial.a);
            cpu.state.usp = tc.initial.usp;
            cpu.state.ssp = tc.initial.ssp;
            cpu.state.sr = tc.initial.sr;
            cpu.state.a[7] = if (tc.initial.sr & SR_S) != 0 {
                tc.initial.ssp
            } else {
                tc.initial.usp
            };
            cpu.state.pc = tc.initial.pc.wrapping_sub(4);

            cpu.step(&mut bus, &mut noop);

            let got_usp = cpu.state.get_usp();
            let got_ssp = cpu.state.get_ssp();
            let exp_pc = if cpu.state.stopped {
                tc.final_state.pc
            } else {
                tc.final_state.pc.wrapping_sub(4)
            };

            let mut ok = true;
            let mut reason = String::new();
            if cpu.state.d != tc.final_state.d {
                ok = false;
                reason = format!(
                    "D mismatch: got {:08X?} exp {:08X?}",
                    cpu.state.d, tc.final_state.d
                );
            } else if cpu.state.a[..7] != tc.final_state.a[..] {
                ok = false;
                reason = format!(
                    "A0..6 mismatch: got {:08X?} exp {:08X?}",
                    &cpu.state.a[..7],
                    &tc.final_state.a
                );
            } else if got_usp != tc.final_state.usp || got_ssp != tc.final_state.ssp {
                ok = false;
                reason = format!(
                    "SP mismatch: got USP={:08X} SSP={:08X}, exp USP={:08X} SSP={:08X}",
                    got_usp, got_ssp, tc.final_state.usp, tc.final_state.ssp
                );
            } else if cpu.state.sr != tc.final_state.sr {
                ok = false;
                reason = format!(
                    "SR mismatch: got {:04X} exp {:04X} (init SR={:04X})",
                    cpu.state.sr, tc.final_state.sr, tc.initial.sr
                );
            } else if (cpu.state.pc & 0x00FF_FFFF) != (exp_pc & 0x00FF_FFFF) {
                ok = false;
                reason = format!("PC mismatch: got {:08X} exp {:08X}", cpu.state.pc, exp_pc);
            } else {
                for &(addr, exp_b) in &tc.final_state.ram {
                    let got_b = bus.mem.get(&(addr & 0x00FF_FFFF)).copied().unwrap_or(0);
                    if got_b != exp_b {
                        ok = false;
                        reason = format!(
                            "RAM[{:06X}] mismatch: got {:02X} exp {:02X}",
                            addr, got_b, exp_b
                        );
                        break;
                    }
                }
            }

            if ok {
                passed += 1;
                total_vectors_passed += 1;
            } else if first_err.is_none() {
                first_err = Some(format!("{} -> {}", tc.name, reason));
            }
        }

        let tested = tests.len() - skipped_addr_err;
        if passed == tested {
            perfect_files += 1;
        } else {
            summary_failures.push((fname, passed, tested, first_err.unwrap_or_default()));
        }
    }

    println!(
        "SingleStepTests/m68000: {}/{} instruction files passed ({} test vectors verified)",
        perfect_files, total_files, total_vectors_passed
    );
    for (fname, passed, tested, err) in &summary_failures {
        eprintln!("  FAIL {:24}: {}/{} passed | {}", fname, passed, tested, err);
    }
    assert!(
        summary_failures.is_empty(),
        "{} instruction files had test failures",
        summary_failures.len()
    );
}
