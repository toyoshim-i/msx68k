//! Boots Human68k 3.02 and SX-Window 3.1 on an MC68030.

use std::path::Path;

use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;
use msx68k_bus::{Bus, CpuModel, CpuSpeedMode, VmConfig, X68kVm};

/// Human68k's MPU type byte (`0` for the MC68000, `3` for the MC68030).
const MPU_TYPE: u32 = 0x0CBC;

fn boot(cpu_model: CpuModel, hdd: Vec<u8>, ipl: &[u8], cgrom: &[u8]) -> X68kVm {
    let mut vm = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4)
            .with_cpu_model(cpu_model),
    );
    vm.bus.load_cgrom(cgrom);
    vm.bus.load_ipl_rom(ipl);
    assert!(vm.bus.insert_scsi_hdd(0, hdd));
    vm.bus.set_boot_from_scsi(true);
    vm.reset().unwrap();
    vm
}

fn desktop_shown(vm: &X68kVm) -> bool {
    let tv_non_zero = vm.bus.tvram().map_or(0, |tv| tv.iter().filter(|&&b| b != 0).count());
    vm.bus.vicon.r0_mode() == 0x0004 && vm.bus.vicon.r2_display() == 0x003F && tv_non_zero > 140_000
}

struct Media {
    cgrom: Vec<u8>,
    hdd: Vec<u8>,
}

/// Loads the CGROM and builds the SX-Window HDD, or returns `None` when they are missing.
fn media() -> Option<Media> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |name: &str| std::fs::read(root.join(name)).ok();
    let (cgrom, human) = (read("roms/CGROM.DAT")?, read("disks/HUMAN302.XDF")?);
    let sx: Vec<(String, Vec<u8>)> = (1..=6)
        .filter_map(|i| {
            let name = format!("SXWIN31{i}");
            read(&format!("disks/{name}.XDF")).map(|bytes| (name, bytes))
        })
        .collect();
    if sx.is_empty() {
        return None;
    }
    let sx_refs: Vec<(&str, &[u8])> = sx.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let hdd = build_bootable_sxwin_scsi_hdd(&human, &sx_refs, 32);
    Some(Media { cgrom, hdd })
}

fn rom(name: &str) -> Option<Vec<u8>> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../roms").join(name)).ok()
}

/// Boots SX-Window on `cpu_model` with `ipl`, checks the desktop and the MPU type Human68k
/// found, and compares the settled desktop with an MC68000 boot on IPL ROM 1.0.
fn check_desktop(cpu_model: CpuModel, ipl: &[u8], media: &Media) {
    let mut vm = boot(cpu_model, media.hdd.clone(), ipl, &media.cgrom);
    for _ in 0..120 {
        vm.run_steps(250_000);
        if desktop_shown(&vm) {
            break;
        }
    }
    assert!(
        desktop_shown(&vm),
        "SX-Window desktop did not appear on {cpu_model:?} (pc={:08X})",
        vm.cpu_state.pc
    );
    assert_eq!(vm.bus.read_u8(MPU_TYPE, true).unwrap(), 3, "Human68k must detect the MC68030");

    // Once settled, the desktop matches the one drawn on an MC68000.
    vm.run_steps(5_000_000);
    let mut reference = boot(CpuModel::Mc68000, media.hdd.clone(), &rom("IPLROM.DAT").unwrap(), &media.cgrom);
    reference.run_steps(30_000_000);
    assert_eq!(reference.bus.read_u8(MPU_TYPE, true).unwrap(), 0);
    assert!(
        vm.bus.tvram().unwrap() == reference.bus.tvram().unwrap(),
        "MC68030 desktop differs from the MC68000 one"
    );
}

#[test]
fn sxwin31_boots_on_x68030_ipl_rom() {
    let (Some(media), Some(ipl)) = (media(), rom("IPLROM30.DAT")) else {
        eprintln!("X68030 IPL ROM, CGROM, or disks not found, skipping");
        return;
    };
    check_desktop(CpuModel::Mc68030, &ipl, &media);
}

/// An MC68030 on the X68000 IPL ROM 1.0 relies on the native `_SYS_STAT`.
#[test]
fn sxwin31_boots_on_mc68030_with_x68000_ipl_rom() {
    let (Some(media), Some(ipl)) = (media(), rom("IPLROM.DAT")) else {
        eprintln!("IPL ROM, CGROM, or disks not found, skipping");
        return;
    };
    check_desktop(CpuModel::Mc68030, &ipl, &media);
}
