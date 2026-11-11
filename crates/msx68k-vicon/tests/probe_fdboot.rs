use std::path::Path;
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};

#[test]
fn probe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |n: &str| std::fs::read(root.join(n)).unwrap();
    let mut vm = X68kVm::new(VmConfig::app_vm().with_speed_mode(CpuSpeedMode::Mhz10).with_ram_mb(2));
    vm.bus.load_cgrom(&read("roms/CGROM.DAT"));
    vm.bus.load_ipl_rom(&read("roms/IPLROM.DAT"));
    assert!(vm.bus.insert_floppy(0, read("disks/HUMAN302.XDF"), true));
    vm.reset().unwrap();
    for sec in 1..=30 {
        for _ in 0..60 { vm.run_cycles(10_000_000 / 60); }
        let lines = msx68k_vicon::decode_tvram_ank_lines(&vm.bus, 96, 32);
        let text: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
        eprintln!("t={sec}s pc={:06X} {:?}", vm.cpu_state.pc, text.iter().rev().take(3).collect::<Vec<_>>());
        if text.iter().any(|l| l.contains("A>")) { eprintln!("PROMPT"); break; }
    }
}
