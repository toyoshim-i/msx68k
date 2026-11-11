//! A VM on a thread of its own boots Human68k 3.02 from a floppy to the COMMAND.X prompt,
//! the first step of VMX68K (`docs/VMX68K.md`).

use std::path::Path;
use std::time::{Duration, Instant};

use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};

#[test]
fn human68k_boots_to_the_prompt_on_a_vm_thread() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |name: &str| std::fs::read(root.join(name)).ok();
    let (Some(ipl), Some(cgrom), Some(human)) = (
        read("roms/IPLROM.DAT"),
        read("roms/CGROM.DAT"),
        read("disks/HUMAN302.XDF"),
    ) else {
        eprintln!("ROMs or HUMAN302.XDF not found, skipping");
        return;
    };

    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(2),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert!(vm.bus.insert_floppy(0, human, true));
    vm.reset().unwrap();

    // Cycle-accurate at 10 MHz, but without waiting for host time.
    let thread = VmThread::spawn(vm, Pace::Unthrottled);
    let deadline = Instant::now() + Duration::from_secs(120);
    let prompt = loop {
        let (lines, seconds) = thread.with_vm(|vm| {
            (
                msx68k_vicon::decode_tvram_ank_lines(&vm.bus, 96, 32),
                vm.cpu_state.total_cycles as f64 / 10_000_000.0,
            )
        });
        if lines.iter().any(|line| line.starts_with("A>")) {
            break Some(seconds);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let vm = thread.stop();
    let seconds = prompt.expect("COMMAND.X must show its prompt");
    // About 10 s of guest time, as on a real 10 MHz X68000.
    assert!((5.0..30.0).contains(&seconds), "prompt after {seconds:.1} s of guest time");
    assert!(vm.bus.fdc.drives[0].is_inserted());
}
