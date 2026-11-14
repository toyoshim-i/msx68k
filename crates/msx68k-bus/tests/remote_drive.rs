//! Exercise the host drive through real Human68k DOS calls after SX-Window boots.
use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;
use msx68k_bus::{CpuSpeedMode, VmConfig, X68kVm};
use msx68k_cpu::Bus;
use std::path::Path;

fn dos(vm: &mut X68kVm, opcode: u16, args: &[u8]) -> i32 {
    let code = 0x0030_0000;
    let stack = 0x0031_0000;
    vm.bus.write_u16(code, opcode, true).unwrap();
    vm.bus.write_u16(code + 2, 0x60FE, true).unwrap();
    for (i, b) in args.iter().enumerate() {
        vm.bus.write_u8(stack + i as u32, *b, true).unwrap();
    }
    vm.cpu_state.pc = code;
    vm.cpu_state.sr = 0x2700;
    vm.cpu_state.a[7] = stack;
    for _ in 0..1_000_000 {
        vm.step();
        if vm.cpu_state.pc == code + 2 {
            return vm.cpu_state.d[0] as i32;
        }
    }
    panic!(
        "DOS {opcode:04x} did not return: PC={:08x}",
        vm.cpu_state.pc
    );
}

#[test]
fn sxwindow_host_drive_label_geometry_and_directory_access() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let required = [
        "roms/IPLROM.DAT",
        "roms/CGROM.DAT",
        "disks/HUMAN302.XDF",
        "disks/SXWIN311.XDF",
    ];
    if required.iter().any(|path| !root.join(path).exists()) {
        eprintln!("ROMs or Human68k/SX-Window disks not found, skipping host drive boot test");
        return;
    }
    let ipl = std::fs::read(root.join(required[0])).unwrap();
    let cgrom = std::fs::read(root.join(required[1])).unwrap();
    let human = std::fs::read(root.join(required[2])).unwrap();
    let sx_disks: Vec<_> = (1..=6)
        .filter_map(|i| {
            let name = format!("SXWIN31{i}");
            std::fs::read(root.join(format!("disks/{name}.XDF")))
                .ok()
                .map(|data| (name, data))
        })
        .collect();
    let sx_refs: Vec<_> = sx_disks
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    let hdd = build_bootable_sxwin_scsi_hdd(&human, &sx_refs, 32);
    let dir = std::env::temp_dir().join(format!("msx68k_sxremote_{}", std::process::id()));
    let mount = dir.join("HOSTSHARE");
    std::fs::create_dir_all(mount.join("SUBDIR")).unwrap();
    std::fs::write(mount.join("SXWIN31.HDS"), b"test image").unwrap();
    std::fs::write(mount.join("HELLO.TXT"), b"hello from host").unwrap();
    let mut vm = X68kVm::new(
        VmConfig::primary_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    vm.bus.load_ipl_rom(&ipl);
    vm.bus.load_cgrom(&cgrom);
    assert!(vm.bus.insert_scsi_hdd(0, hdd));
    vm.bus.set_boot_from_scsi(true);
    vm.bus.install_host_remote_drives(vec![mount]);
    vm.reset().unwrap();
    vm.run_steps(25_000_000);
    assert_eq!(vm.bus.vicon.r0_mode(), 4, "SX-Window must boot");
    let buffer: u32 = 0x0032_0000;
    let mut args = 4u16.to_be_bytes().to_vec(); // D: follows HDD A: and floppies B:/C:.
    args.extend_from_slice(&buffer.to_be_bytes());
    for i in 0..100 {
        vm.bus.write_u8(buffer + i, 0xA5, true).unwrap();
    }
    assert_eq!(dos(&mut vm, 0xFF32, &args), 0);
    assert_eq!(
        vm.bus.read_u16(buffer + 2, true).unwrap(),
        0,
        "remote sector size"
    );
    assert_eq!(vm.bus.read_u8(buffer + 94, true).unwrap(), 0xA5);

    let path: u32 = buffer + 0x100;
    for (i, b) in b"D:\\*.*\0".iter().enumerate() {
        vm.bus.write_u8(path + i as u32, *b, true).unwrap();
    }
    let mut args = buffer.to_be_bytes().to_vec();
    args.extend_from_slice(&path.to_be_bytes());
    args.extend_from_slice(&8u16.to_be_bytes());
    assert_eq!(dos(&mut vm, 0xFF4E, &args), 0);
    assert_eq!(vm.bus.read_u8(buffer + 21, true).unwrap(), 8);
    assert_eq!(
        &vm.bus.main_ram()[buffer as usize + 30..buffer as usize + 40],
        b"HOSTSHARE\0"
    );
    assert!(dos(&mut vm, 0xFF4F, &buffer.to_be_bytes()) < 0);

    args[8..10].copy_from_slice(&0x37u16.to_be_bytes());
    assert_eq!(dos(&mut vm, 0xFF4E, &args), 0);
    // Filers may move a _FILES buffer before continuing the same search.
    let next_buffer = buffer + 0x200;
    for i in 0..53 {
        let byte = vm.bus.read_u8(buffer + i, true).unwrap();
        vm.bus.write_u8(next_buffer + i, byte, true).unwrap();
    }
    let mut names = Vec::new();
    loop {
        let raw = &vm.bus.main_ram()[buffer as usize + 30..buffer as usize + 53];
        names.push(String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap()).into_owned());
        if dos(&mut vm, 0xFF4F, &next_buffer.to_be_bytes()) < 0 {
            break;
        }
        for i in 0..53 {
            let byte = vm.bus.read_u8(next_buffer + i, true).unwrap();
            vm.bus.write_u8(buffer + i, byte, true).unwrap();
        }
        assert!(names.len() < 10, "directory enumeration must terminate");
    }
    assert!(names.contains(&"HELLO.TXT".to_string()), "{names:?}");
    assert!(names.contains(&"SUBDIR".to_string()), "{names:?}");

    for (i, b) in b"D:\\HELLO.TXT\0".iter().enumerate() {
        vm.bus.write_u8(path + i as u32, *b, true).unwrap();
    }
    let mut args = path.to_be_bytes().to_vec();
    args.extend_from_slice(&0u16.to_be_bytes());
    let handle = dos(&mut vm, 0xFF3D, &args);
    assert!(handle >= 0, "OPEN failed: {handle}");
    let mut args = (handle as u16).to_be_bytes().to_vec();
    args.extend_from_slice(&buffer.to_be_bytes());
    args.extend_from_slice(&15u32.to_be_bytes());
    assert_eq!(dos(&mut vm, 0xFF3F, &args), 15);
    assert_eq!(
        &vm.bus.main_ram()[buffer as usize..buffer as usize + 15],
        b"hello from host"
    );
    assert_eq!(dos(&mut vm, 0xFF3E, &(handle as u16).to_be_bytes()), 0);

    for (i, b) in b"D:\\SUBDIR\0".iter().enumerate() {
        vm.bus.write_u8(path + i as u32, *b, true).unwrap();
    }
    assert_eq!(dos(&mut vm, 0xFF3B, &path.to_be_bytes()), 0);
    std::fs::remove_dir_all(dir).unwrap();
}
