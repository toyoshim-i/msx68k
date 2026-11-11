//! Clean-room guest toolchain integration using `rhas` (HAS060X assembler)
//! and `rhlk` (HLK linker) for building Human68k guest `.X` executables.

use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use clap::Parser;
use rhas::{context, error, options, pass};

/// Assembles a 68000 assembly file using `rhas` with specified include directories.
/// Returns the resulting `.o` object bytes.
pub fn assemble_file(source_path: &Path, include_dirs: &[&Path]) -> Result<Vec<u8>, String> {
    let mut args = vec!["rhas".to_string()];
    for inc in include_dirs {
        args.push(format!("-i{}", inc.display()));
    }
    args.push(source_path.to_string_lossy().to_string());

    let opts = options::parse_args(args.iter().skip(1).map(|s| s.as_str()), false)
        .map_err(|e| format!("rhas parse_args error: {e:?}"))?;

    let mut ctx = context::AssemblyContext::new(opts);
    let mut reporter =
        error::StderrReporter::new(ctx.effective_warn_level(), ctx.opts.compat_error_format);
    match pass::assemble(&mut ctx, &mut reporter) {
        Ok(result) => Ok(result.obj_bytes),
        Err(e) => Err(format!("rhas assemble error: {e:?}")),
    }
}

/// Links object files into a Human68k `.X` executable using `rhlk`.
pub fn link_objects(object_files: &[&Path], output_path: &Path) -> Result<(), String> {
    let mut args_vec: Vec<OsString> = vec![
        "rhlk".into(),
        "-o".into(),
        output_path.as_os_str().to_os_string(),
    ];
    for obj in object_files {
        args_vec.push(obj.as_os_str().to_os_string());
    }
    let mut parsed_args = rhlk::cli::Args::parse_from(args_vec.iter());
    if let Err(e) = rhlk::cli::finalize_compat_args(&mut parsed_args, &args_vec) {
        return Err(format!("rhlk finalize error: {e:?}"));
    }
    rhlk::run(parsed_args).map_err(|e| format!("rhlk run error: {e:?}"))
}

/// Assembles a 68000 source file and links it into a Human68k `.X` executable.
/// Returns the raw binary bytes of the `.X` file.
pub fn build_guest_x_executable(
    source_path: &Path,
    include_dirs: &[&Path],
) -> Result<Vec<u8>, String> {
    static BUILDS: AtomicU64 = AtomicU64::new(0);
    let obj_bytes = assemble_file(source_path, include_dirs)?;
    let temp_dir = std::env::temp_dir();
    // Tests build in parallel threads and processes, so a timestamp alone can repeat.
    let unique_id = format!(
        "{}_{}_{}",
        std::process::id(),
        BUILDS.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let temp_obj = temp_dir.join(format!("msx68k_build_{unique_id}.o"));
    let temp_out = temp_dir.join(format!("msx68k_build_{unique_id}.x"));

    std::fs::write(&temp_obj, &obj_bytes).map_err(|e| e.to_string())?;
    let link_res = link_objects(&[&temp_obj], &temp_out);
    let _ = std::fs::remove_file(&temp_obj);
    link_res?;

    let mut x_bytes = std::fs::read(&temp_out).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&temp_out);

    // If payload begins with 'OBJR' at .text (+64) and exec is 0, set entry point to 0x00000020
    if x_bytes.len() >= 68 && &x_bytes[64..68] == b"OBJR" {
        let cur_exec = u32::from_be_bytes([x_bytes[8], x_bytes[9], x_bytes[10], x_bytes[11]]);
        if cur_exec == 0 {
            x_bytes[8..12].copy_from_slice(&0x0000_0020u32.to_be_bytes());
        }
    }
    Ok(x_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Instruction budget for SXMODE.X to format and attempt saving its settings before exiting.
    const MAX_STEPS: usize = 20_000;
    const DOS_STUB: u32 = 0x0800;

    #[test]
    fn test_build_sxmode_x_executable() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_path = workspace_root.join("guest/sxmode/sxmode.s");
        let inc_dir = workspace_root.join("guest/include");

        assert!(source_path.exists(), "sxmode.s must exist");
        assert!(inc_dir.exists(), "guest/include must exist");

        let x_bytes = build_guest_x_executable(&source_path, &[&inc_dir])
            .expect("Must assemble and link sxmode.s into SXMODE.X");

        assert!(x_bytes.len() >= 64, "X executable must have at least 64-byte header");
        assert_eq!(&x_bytes[0..2], b"HU", "X executable must have 'HU' magic");

        let exec = u32::from_be_bytes([x_bytes[8], x_bytes[9], x_bytes[10], x_bytes[11]]);
        let text_size = u32::from_be_bytes([x_bytes[12], x_bytes[13], x_bytes[14], x_bytes[15]]);
        let data_size = u32::from_be_bytes([x_bytes[16], x_bytes[17], x_bytes[18], x_bytes[19]]);
        let bss_size = u32::from_be_bytes([x_bytes[20], x_bytes[21], x_bytes[22], x_bytes[23]]);
        let reloc_size = u32::from_be_bytes([x_bytes[24], x_bytes[25], x_bytes[26], x_bytes[27]]);

        eprintln!(
            "X Header: exec=0x{:08X}, text_size={}, data_size={}, bss_size={}, reloc_size={}",
            exec, text_size, data_size, bss_size, reloc_size
        );
        assert_eq!(exec, 0x0000_0020);
        assert_eq!(&x_bytes[64..68], b"OBJR");
        let gui_entry = u32::from_be_bytes([x_bytes[72], x_bytes[73], x_bytes[74], x_bytes[75]]);
        let stack_size = u32::from_be_bytes([x_bytes[76], x_bytes[77], x_bytes[78], x_bytes[79]]);
        let work_size = u32::from_be_bytes([x_bytes[80], x_bytes[81], x_bytes[82], x_bytes[83]]);
        assert!(
            gui_entry > 0 && gui_entry < text_size,
            "GUI entry offset ({}) must be within text ({})",
            gui_entry,
            text_size
        );
        assert_eq!(stack_size, 0x0000_0800, "OBJR stack size must be 2048");
        assert_eq!(work_size, 0x0000_0000, "OBJR work area size is 0 when no TSInitApp is required");
        assert!(text_size > 0);
        assert_eq!(data_size, 0);
        assert_eq!(bss_size, 0);

        let text_bytes = &x_bytes[64..64 + text_size as usize];
        eprintln!("Text bytes (len={}):", text_bytes.len());
        for (i, chunk) in text_bytes.chunks(16).enumerate() {
            let hex_str = chunk.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
            eprintln!("  0x{:04X}: {}", i * 16, hex_str);
        }
    }

    #[test]
    fn test_execute_built_sxmode_x_on_vm() {
        use crate::vm::X68kVm;
        use msx68k_cpu::Bus;

        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_path = workspace_root.join("guest/sxmode/sxmode.s");
        let inc_dir = workspace_root.join("guest/include");

        let x_bytes = build_guest_x_executable(&source_path, &[&inc_dir])
            .expect("Must assemble and link sxmode.s into SXMODE.X");

        assert_eq!(&x_bytes[0..2], b"HU");
        let exec = u32::from_be_bytes([x_bytes[8], x_bytes[9], x_bytes[10], x_bytes[11]]);
        let text_size = u32::from_be_bytes([x_bytes[12], x_bytes[13], x_bytes[14], x_bytes[15]]);
        assert_eq!(exec, 0x0000_0020);
        assert_eq!(&x_bytes[64..68], b"OBJR");

        let text_payload = &x_bytes[64..64 + text_size as usize];

        let mut vm = X68kVm::new_app_vm();

        // Mock resident SYS/FSX.X kernel structures in Main RAM
        let text_base: u32 = 0x0003_0000;
        vm.bus
            .write_u32(0x0000_0938, text_base + 0x04F82A, true)
            .unwrap();
        let active_rec = text_base + 0x55278;
        vm.bus.write_u16(active_rec + 6, 768, true).unwrap();
        vm.bus.write_u16(active_rec + 8, 512, true).unwrap();

        let crt16 = text_base + 0x15200;
        vm.bus.write_u16(crt16, 768, true).unwrap();
        vm.bus.write_u16(crt16 + 2, 512, true).unwrap();

        let dtgs = text_base + 0x54380;
        let gp = text_base + 0x57000;
        vm.bus.write_u32(dtgs + 4, gp, true).unwrap();
        vm.bus.write_u16(gp + 4, 0, true).unwrap();
        vm.bus.write_u16(gp + 6, 0, true).unwrap();
        vm.bus.write_u16(gp + 8, 768, true).unwrap();
        vm.bus.write_u16(gp + 10, 512, true).unwrap();

        let a4 = text_base + 0x5CA44;
        vm.bus.write_u32(text_base + 0x633CA, a4, true).unwrap();
        vm.bus.write_u16(a4 + 0x34, 0, true).unwrap();
        vm.bus.write_u16(a4 + 0x36, 0, true).unwrap();
        vm.bus.write_u16(a4 + 0x38, 768, true).unwrap();
        vm.bus.write_u16(a4 + 0x3A, 512, true).unwrap();

        // Load SXMODE.X text into RAM at 0x0001_0000
        let code_addr: u32 = 0x0001_0000;
        for (i, &b) in text_payload.iter().enumerate() {
            vm.bus.write_u8(code_addr + i as u32, b, true).unwrap();
        }

        let cmd_addr: u32 = 0x0001_2000;
        let run_sxmode = |vm: &mut X68kVm, cmd: &str| -> (u16, (u32, u32)) {
            // Human68k command line buffer at A2:
            // byte 0 = length of arguments
            // byte 1..1+len = argument characters
            // byte 1+len+1 = null terminator
            let mut cmd_buf = Vec::new();
            cmd_buf.push(cmd.len() as u8);
            cmd_buf.extend_from_slice(cmd.as_bytes());
            cmd_buf.push(0);
            for (i, &b) in cmd_buf.iter().enumerate() {
                vm.bus.write_u8(cmd_addr + i as u32, b, true).unwrap();
            }

            // In Human68k ABI: A0 = Environment / PSP pointer, A2 = Command line buffer pointer
            vm.cpu_state.a[0] = 0x0001_1000;
            vm.cpu_state.a[2] = cmd_addr;
            vm.cpu_state.a[7] = 0x0002_0000;
            vm.cpu_state.pc = code_addr + exec;

            // No DOS is loaded here, so make DOS calls fail through a minimal F-Line handler:
            // ADDQ.L #2, 2(SP) / MOVEQ #-1, D0 / RTE
            vm.bus.write_u32(0x002C, DOS_STUB, true).unwrap();
            for (i, word) in [0x54AFu16, 0x0002, 0x70FF, 0x4E73].iter().enumerate() {
                vm.bus.write_u16(DOS_STUB + i as u32 * 2, *word, true).unwrap();
            }

            let mut steps = 0;
            while steps < MAX_STEPS {
                let cur_pc = vm.cpu_state.pc;
                let opcode = vm.bus.read_u16(cur_pc, true).unwrap();
                if opcode == 0xFF4C {
                    // Reached DOS _EXIT2!
                    break;
                }
                vm.step();
                steps += 1;
            }
            assert!(
                steps < MAX_STEPS,
                "SXMODE.X should reach DOS _EXIT2 ($FF4C) within {MAX_STEPS} steps (steps={steps})"
            );

            let sp = vm.cpu_state.a[7];
            let exit_code = vm.bus.read_u16(sp, true).unwrap();
            let resolution = vm.bus.crtc.active_resolution();
            (exit_code, resolution)
        };

        // 1. Initial resolution is 768x512
        assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));

        // 2. Test cycling with no arguments: 0 -> 1 -> 2 -> 0
        let (code, res) = run_sxmode(&mut vm, "");
        assert_eq!(code, 1, "Mode 0 should cycle to Mode 1");
        assert_eq!(res, (1024, 768));

        let (code, res) = run_sxmode(&mut vm, "");
        assert_eq!(code, 2, "Mode 1 should cycle to Mode 2");
        assert_eq!(res, (1024, 1024));

        let (code, res) = run_sxmode(&mut vm, "");
        assert_eq!(code, 0, "Mode 2 should cycle to Mode 0");
        assert_eq!(res, (768, 512));

        // 3. Test explicit mode switches
        let (code, res) = run_sxmode(&mut vm, "1024x768");
        assert_eq!(code, 1);
        assert_eq!(res, (1024, 768));

        let (code, res) = run_sxmode(&mut vm, "2");
        assert_eq!(code, 2);
        assert_eq!(res, (1024, 1024));

        let (code, res) = run_sxmode(&mut vm, "768x512");
        assert_eq!(code, 0);
        assert_eq!(res, (768, 512));

        // 4. Test with leading whitespace and alternate aliases
        let (code, res) = run_sxmode(&mut vm, "   1 ");
        assert_eq!(code, 1);
        assert_eq!(res, (1024, 768));

        let (code, res) = run_sxmode(&mut vm, "\t1024");
        assert_eq!(code, 2);
        assert_eq!(res, (1024, 1024));

        let (code, res) = run_sxmode(&mut vm, "  0");
        assert_eq!(code, 0);
        assert_eq!(res, (768, 512));
    }

    #[test]
    fn test_execute_built_sxmode_gui_cli_args_on_vm() {
        use crate::vm::X68kVm;
        use msx68k_cpu::Bus;

        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_path = workspace_root.join("guest/sxmode/sxmode.s");
        let inc_dir = workspace_root.join("guest/include");

        let x_bytes = build_guest_x_executable(&source_path, &[&inc_dir])
            .expect("Must assemble and link sxmode.s into SXMODE.X");

        let text_size = u32::from_be_bytes([x_bytes[12], x_bytes[13], x_bytes[14], x_bytes[15]]);
        let gui_entry = u32::from_be_bytes([x_bytes[72], x_bytes[73], x_bytes[74], x_bytes[75]]);
        assert_eq!(&x_bytes[64..68], b"OBJR");

        let text_payload = &x_bytes[64..64 + text_size as usize];

        let mut vm = X68kVm::new_app_vm();

        // Load SXMODE.X text into RAM at 0x0001_0000
        let code_addr: u32 = 0x0001_0000;
        for (i, &b) in text_payload.iter().enumerate() {
            vm.bus.write_u8(code_addr + i as u32, b, true).unwrap();
        }

        struct MockSxTrapHandler;
        impl msx68k_cpu::TrapHandler<crate::bus::X68kBus> for MockSxTrapHandler {
            fn on_f_line(
                &mut self,
                state: &mut msx68k_cpu::CpuState,
                bus: &mut crate::bus::X68kBus,
                opcode: u16,
                pc: u32,
            ) -> msx68k_cpu::TrapResult {
                // No DOS is loaded here, so DOS calls fail instead of reaching an empty vector.
                if opcode >= 0xFF00 && opcode != 0xFF4C && opcode != 0xFF00 {
                    state.d[0] = 0xFFFF_FFFF;
                    state.set_flag(msx68k_cpu::SR_N, true);
                    state.set_flag(msx68k_cpu::SR_Z, false);
                    return msx68k_cpu::TrapResult::Handled { extra_cycles: 0 };
                }
                bus.handle_hypercall(state, opcode, pc)
            }

            fn on_a_line(
                &mut self,
                _state: &mut msx68k_cpu::CpuState,
                _bus: &mut crate::bus::X68kBus,
                opcode: u16,
                _pc: u32,
            ) -> msx68k_cpu::TrapResult {
                match opcode {
                    0xA352 => {
                        // TSExit ($A352)
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    _ => panic!("Unhandled mock SXCALL: 0x{:04X}", opcode),
                }
            }

            fn on_trap(
                &mut self,
                state: &mut msx68k_cpu::CpuState,
                bus: &mut crate::bus::X68kBus,
                trap_no: u8,
                _pc: u32,
            ) -> msx68k_cpu::TrapResult {
                if trap_no == 15 && (state.d[0] & 0xFF) == 0x10 {
                    bus.handle_iocs_crtmod(state)
                } else {
                    msx68k_cpu::TrapResult::PassToVector
                }
            }
        }

        let cmd_addr: u32 = 0x0001_2000;
        let run_gui_cli = |vm: &mut X68kVm, cmd: &str| -> (u32, u32) {
            let mut cmd_buf = Vec::new();
            cmd_buf.push(cmd.len() as u8);
            cmd_buf.extend_from_slice(cmd.as_bytes());
            cmd_buf.push(0);
            for (i, &b) in cmd_buf.iter().enumerate() {
                vm.bus.write_u8(cmd_addr + i as u32, b, true).unwrap();
            }

            vm.cpu_state.a[0] = 0x0001_1000;
            vm.cpu_state.a[2] = cmd_addr;
            vm.cpu_state.a[7] = 0x0002_0000;
            vm.cpu_state.pc = code_addr + gui_entry;

            let mut trap_handler = MockSxTrapHandler;
            let mut steps = 0;
            while steps < MAX_STEPS {
                let cur_pc = vm.cpu_state.pc;
                let opcode = vm.bus.read_u16(cur_pc, true).unwrap();
                if opcode == 0xFF4C {
                    break;
                }
                vm.step_with_trap(&mut trap_handler);
                steps += 1;
            }
            assert!(
                steps < MAX_STEPS,
                "GUI launch with CLI args should process args and exit within {MAX_STEPS} steps (steps={steps})"
            );
            vm.bus.crtc.active_resolution()
        };

        assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
        assert_eq!(run_gui_cli(&mut vm, "1024x768"), (1024, 768));
        assert_eq!(run_gui_cli(&mut vm, "2"), (1024, 1024));
        assert_eq!(run_gui_cli(&mut vm, "0"), (768, 512));
    }

    #[test]
    fn test_execute_built_sxmode_gui_direct_entry_on_vm() {
        use crate::vm::X68kVm;
        use msx68k_cpu::Bus;

        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_path = workspace_root.join("guest/sxmode/sxmode.s");
        let inc_dir = workspace_root.join("guest/include");

        let x_bytes = build_guest_x_executable(&source_path, &[&inc_dir])
            .expect("Must assemble and link sxmode.s into SXMODE.X");

        let text_size = u32::from_be_bytes([x_bytes[12], x_bytes[13], x_bytes[14], x_bytes[15]]);
        let gui_entry = u32::from_be_bytes([x_bytes[72], x_bytes[73], x_bytes[74], x_bytes[75]]);
        assert_eq!(&x_bytes[64..68], b"OBJR");
        assert!(gui_entry > 0 && gui_entry < text_size);

        let text_payload = &x_bytes[64..64 + text_size as usize];

        let mut vm = X68kVm::new_app_vm();

        // Load SXMODE.X text into RAM at 0x0001_0000
        let code_addr: u32 = 0x0001_0000;
        for (i, &b) in text_payload.iter().enumerate() {
            vm.bus.write_u8(code_addr + i as u32, b, true).unwrap();
        }

        // SX-Window Process Manager sets PC directly to code_addr + OBJR[+0x08] (gui_entry)
        vm.cpu_state.a[0] = 0x0001_5000; // TCB allocated by SX-Window
        vm.cpu_state.a[2] = 0; // No CLI arguments (double-clicked in Filer)
        vm.cpu_state.a[7] = 0x0002_0000; // stack
        vm.cpu_state.pc = code_addr + gui_entry;

        struct MockSxTrapHandler {
            event_count: usize,
            win_ptr: u32,
        }

        impl msx68k_cpu::TrapHandler<crate::bus::X68kBus> for MockSxTrapHandler {
            fn on_f_line(
                &mut self,
                state: &mut msx68k_cpu::CpuState,
                bus: &mut crate::bus::X68kBus,
                opcode: u16,
                pc: u32,
            ) -> msx68k_cpu::TrapResult {
                // No DOS is loaded here, so DOS calls fail instead of reaching an empty vector.
                if opcode >= 0xFF00 && opcode != 0xFF4C && opcode != 0xFF00 {
                    state.d[0] = 0xFFFF_FFFF;
                    state.set_flag(msx68k_cpu::SR_N, true);
                    state.set_flag(msx68k_cpu::SR_Z, false);
                    return msx68k_cpu::TrapResult::Handled { extra_cycles: 0 };
                }
                bus.handle_hypercall(state, opcode, pc)
            }

            fn on_a_line(
                &mut self,
                state: &mut msx68k_cpu::CpuState,
                bus: &mut crate::bus::X68kBus,
                opcode: u16,
                _pc: u32,
            ) -> msx68k_cpu::TrapResult {
                match opcode {
                    0xA360 => {
                        // TSGetID ($A360): returns Task ID in D0
                        state.d[0] = 0x0000_0002;
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    0xA1F9 => {
                        // WMOpen ($A1F9): returns WindowPtr in A0 and 0 in D0
                        state.a[0] = self.win_ptr;
                        state.d[0] = 0;
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    0xA131 | 0xA144 | 0xA147 | 0xA148 | 0xA18B | 0xA18D | 0xA18E | 0xA207 | 0xA16E | 0xA172
                    | 0xA173 | 0xA190 | 0xA1A3 | 0xA1FE | 0xA358 | 0xA20D | 0xA20E | 0xA1FC
                    | 0xA352 => {
                        // GMSetGraph, GMPenMode, GMForeColor, GMBackColor, GMFontKind, GMFontMode, GMFontSize,
                        // WMShow, GMMove, GMFrameRect, GMFillRect, GMDrawStrL, GMInvertRect,
                        // WMSelect, TSGetEvent, WMUpdate, WMUpdtOver, WMDispose, TSExit
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    0xA357 => {
                        // TSEventAvail ($A357): args: (sp) = mask: u16, (sp + 2) = event_rec_ptr: u32
                        self.event_count += 1;
                        let sp = state.a[7];
                        let event_ptr = bus.read_u32(sp + 2, true).unwrap();
                        if self.event_count == 1 {
                            // Event 1: Click inside Button 2 (1024x1024)
                            // Window origin is (180, 120), Button 2 is x=20..200, y=96..118.
                            // Local: x=100, y=105 -> Global: x=280, y=225
                            // In SX-Window LPoint, X is high word, Y is low word:
                            bus.write_u16(event_ptr, 1, true).unwrap(); // what = E_MSLDOWN (1)
                            bus.write_u32(event_ptr + 2, self.win_ptr, true).unwrap(); // whom.win
                            bus.write_u32(event_ptr + 10, (280 << 16) | 225, true).unwrap(); // where: (x << 16) | y
                            state.d[0] = 0; // 0 = success
                        } else if self.event_count == 2 {
                            // Event 2: Click on Close Box
                            bus.write_u16(event_ptr, 1, true).unwrap(); // what = E_MSLDOWN (1)
                            bus.write_u32(event_ptr + 2, self.win_ptr, true).unwrap(); // whom.win
                            bus.write_u32(event_ptr + 10, (190 << 16) | 130, true).unwrap(); // where
                            state.d[0] = 0; // 0 = success
                        } else {
                            state.d[0] = 0;
                        }
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    0xA3A2 => {
                        // SXCallWindM ($A3A2):
                        // Event 1: inContent (3)
                        // Event 2: Close Box (7)
                        if self.event_count == 1 {
                            state.d[0] = 3; // W_ININSIDE
                        } else {
                            state.d[0] = 7; // W_INCLOSE
                        }
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    0xA13F => {
                        // GMGlobalToLocal ($A13F): takes LPoint by value (4B on stack)
                        // Returns local LPoint (x:high, y:low) in D0!
                        let sp = state.a[7];
                        let glob_pt = bus.read_u32(sp, true).unwrap();
                        let glob_x = (glob_pt >> 16) as i16;
                        let glob_y = (glob_pt & 0xFFFF) as i16;
                        let loc_x = glob_x - 180;
                        let loc_y = glob_y - 120;
                        state.d[0] = ((loc_x as u16 as u32) << 16) | (loc_y as u16 as u32);
                        msx68k_cpu::TrapResult::Handled { extra_cycles: 4 }
                    }
                    _ => panic!("Unhandled mock SXCALL: 0x{:04X}", opcode),
                }
            }

            fn on_trap(
                &mut self,
                state: &mut msx68k_cpu::CpuState,
                bus: &mut crate::bus::X68kBus,
                trap_no: u8,
                _pc: u32,
            ) -> msx68k_cpu::TrapResult {
                if trap_no == 15 && (state.d[0] & 0xFF) == 0x10 {
                    bus.handle_iocs_crtmod(state)
                } else {
                    msx68k_cpu::TrapResult::PassToVector
                }
            }
        }

        let mut trap_handler = MockSxTrapHandler {
            event_count: 0,
            win_ptr: 0x0004_0000,
        };

        assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));

        let mut steps = 0;
        while steps < 1000 {
            let cur_pc = vm.cpu_state.pc;
            let opcode = vm.bus.read_u16(cur_pc, true).unwrap();
            if opcode == 0xFF4C {
                // Reached DOS _EXIT2!
                break;
            }
            vm.step_with_trap(&mut trap_handler);
            steps += 1;
        }

        assert!(
            steps < 1000,
            "Direct SX-Window GUI launch must process events and reach DOS _EXIT2 within 1000 steps (steps={steps})"
        );
        assert_eq!(
            vm.bus.crtc.active_resolution(),
            (1024, 1024),
            "Button 2 click must switch to 1024x1024"
        );
        assert_eq!(trap_handler.event_count, 2);
    }
}
