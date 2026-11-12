//! The msx68k settings app, added to the SX-Window control panel, sets the mouse wheel.

use std::path::Path;

use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd, extract_files_from_xdf};
use msx68k_bus::sx_control_panel::SETTINGS_PANEL_ID;
use msx68k_bus::sx_resource::ResourceLibrary;
use msx68k_bus::sx_wheel::WheelConfig;
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};

/// `コントロール` in Shift_JIS.
const CONTROL_PANEL: &[u8] = b"\x83\x52\x83\x93\x83\x67\x83\x8D\x81\x5B\x83\x8B";

fn read(name: &str) -> Option<Vec<u8>> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(name)).ok()
}

fn sx_disks() -> Vec<(String, Vec<u8>)> {
    (1..=6)
        .filter_map(|i| {
            let name = format!("SXWIN31{i}");
            read(&format!("disks/{name}.XDF")).map(|bytes| (name, bytes))
        })
        .collect()
}

#[test]
fn sharp_control_panel_library_survives_a_rewrite() {
    let Some(library) = sx_disks().iter().find_map(|(_, disk)| {
        extract_files_from_xdf(disk).into_iter().find_map(|f| {
            let mut name = f.raw_name.to_vec();
            name.extend_from_slice(&f.raw_name2);
            (name.starts_with(CONTROL_PANEL) && f.raw_ext == *b"LB ").then_some(f.data)
        })
    }) else {
        eprintln!("SX-Window disks not found, skipping the library test");
        return;
    };
    let parsed = ResourceLibrary::parse(&library).expect("Sharp's library must parse");
    let rewritten = ResourceLibrary::parse(&parsed.to_bytes()).unwrap();
    assert_eq!(rewritten, parsed);
    let codes: Vec<[u8; 4]> = parsed.types.iter().map(|t| t.code).collect();
    assert_eq!(&codes, &[*b"CNTL", *b"CODE", *b"CPNM", *b"DITL", *b"DLOG", *b"IMGS", *b"PAT2"]);
    assert!(parsed.get(b"CODE", SETTINGS_PANEL_ID).is_none(), "the ID is free in Sharp's library");
}

fn click(vm: &mut X68kVm, at: (i16, i16), times: usize, settle: usize) {
    vm.bus.set_host_mouse_position(Some(at));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(200_000);
    for _ in 0..times {
        for left in [true, false] {
            vm.bus.scc.set_mouse_buttons(left, false);
            vm.bus.scc.add_mouse_delta(1, 1);
            vm.run_steps(100_000);
        }
    }
    vm.run_steps(settle);
}

#[test]
fn settings_item_sets_the_wheel_and_saves_it() {
    let (Some(ipl), Some(cgrom), Some(human)) =
        (read("roms/IPLROM.DAT"), read("roms/CGROM.DAT"), read("disks/HUMAN302.XDF"))
    else {
        eprintln!("ROMs or disks not found, skipping the settings test");
        return;
    };
    let sx = sx_disks();
    if sx.is_empty() {
        eprintln!("SX-Window disks not found, skipping the settings test");
        return;
    }
    let sx_refs: Vec<(&str, &[u8])> = sx.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let mut vm = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert!(vm.bus.insert_scsi_hdd(0, build_bootable_sxwin_scsi_hdd(&human, &sx_refs, 32)));
    vm.bus.set_boot_from_scsi(true);
    vm.reset().unwrap();
    vm.run_steps(30_000_000);
    assert_eq!(vm.bus.wheel_config, WheelConfig::default());

    // Open the drive window, then the control panel (コントロール.X, third row, fourth icon,
    // after VMX68K.LB and VMX68K.X in the listing), whose first item is the settings app.
    click(&mut vm, (22, 340), 2, 3_000_000);
    click(&mut vm, (343, 255), 2, 6_000_000);
    click(&mut vm, (113, 107), 1, 6_000_000);

    // The window opens at (180, 120): "Reverse", "3" lines per notch, and "Fast" trackpad.
    for button in [(555, 171), (502, 235), (572, 299)] {
        click(&mut vm, button, 1, 2_000_000);
    }
    let chosen = WheelConfig {
        reverse: true,
        clicks_per_notch: 3,
        trackpad_pixels: 20,
    };
    assert_eq!(vm.bus.wheel_config, chosen);

    // The app saved the settings in its resource library, A:\SYS\MSX68K.LB, which the next
    // boot reads.
    vm.bus.wheel_config = WheelConfig::default();
    vm.bus.load_disk_config_if_present();
    assert_eq!(vm.bus.wheel_config, chosen);
}
