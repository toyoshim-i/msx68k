//! Standalone cold-boot checks, separate from the warm-primary lifecycle suite.
use msx68k_bus::{VmConfig, X68kVm};
use msx68k_vicon::decode_tvram_ank_lines;
use std::path::Path;

#[test]
fn resident_hdd_reaches_the_command_prompt() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| std::fs::read(root.join(path)).ok();
    let (Some(human), Some(ipl), Some(cgrom)) = (
        read("disks/HUMAN302.XDF"),
        read("roms/IPLROM.DAT"),
        read("roms/CGROM.DAT"),
    ) else {
        eprintln!("ROMs or disks not found, skipping");
        return;
    };
    let mut vm = X68kVm::new(VmConfig::app_vm().with_ram_mb(4));
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert!(vm.bus.insert_scsi_hdd(
        0,
        msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd(&human, &[], 8)
    ));
    vm.reset().unwrap();
    vm.run_steps(15_000_000);
    let text = decode_tvram_ank_lines(&vm.bus, 96, 32).join("\n");
    assert!(
        text.lines().any(|l| l.starts_with("A>")),
        "resident HDD must reach COMMAND.X, PC={:08x}\n{text}",
        vm.cpu_state.pc
    );
}

#[test]
fn a_nonzero_scsi_boot_choice_reaches_its_command_prompt() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (Ok(human), Ok(ipl), Ok(cgrom)) = (
        std::fs::read(root.join("disks/HUMAN302.XDF")),
        std::fs::read(root.join("roms/IPLROM.DAT")),
        std::fs::read(root.join("roms/CGROM.DAT")),
    ) else {
        return;
    };
    let mut vm = X68kVm::new(VmConfig::app_vm().with_ram_mb(4));
    vm.bus.load_ipl_rom(&ipl);
    vm.bus.load_cgrom(&cgrom);
    let image = msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd(&human, &[], 8);
    assert!(vm.bus.insert_scsi_hdd(0, image.clone()));
    assert!(vm.bus.insert_scsi_hdd(6, image));
    assert!(msx68k_bus::vmx68k::apply_boot_choice(
        vm.bus.sram_mut(),
        11,
        0
    ));
    vm.reset().unwrap();
    vm.run_steps(20_000_000);
    let text = decode_tvram_ank_lines(&vm.bus, 96, 32).join("\n");
    assert!(
        text.lines().any(|l| l.starts_with("B>")),
        "SCSI6 must boot as B: after SCSI0, PC={:08x}\n{text}",
        vm.cpu_state.pc
    );
    assert_eq!(msx68k_bus::vmx68k::boot_choice(vm.bus.sram()), 11);
    // A later Boot/FD0 reset must discard the preceding SCSI boot's letter.
    assert!(vm.bus.insert_floppy(0, human, false));
    assert!(msx68k_bus::vmx68k::apply_boot_choice(
        vm.bus.sram_mut(),
        1,
        0
    ));
    vm.reset().unwrap();
    vm.run_steps(20_000_000);
    let text = decode_tvram_ank_lines(&vm.bus, 96, 32).join("\n");
    assert!(
        text.lines().any(|l| l.starts_with("A>")),
        "FD0 must boot as A: after switching from SCSI6, PC={:08x}\n{text}",
        vm.cpu_state.pc
    );
}
