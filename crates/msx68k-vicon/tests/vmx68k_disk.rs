//! A VM opened by VMX68K.X boots Human68k 3.02 from a disk image on the primary VM's HDD,
//! which VMX68K.X reads through the primary's Human68k when the host wakes it with `E_USER1`,
//! even with its window behind another (`docs/VMX68K.md` Sections 3.1 and 3.5).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd_with_files, ExtractedFile};
use msx68k_bus::sx_resource::{Resource, ResourceLibrary};
use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::vmx68k::{vm_bridge, CustomDrive, RemoteDisk};
use msx68k_bus::{CpuSpeedMode, VmConfig, VmEvent, X68kVm};
use msx68k_vicon::decode_tvram_ank_lines;

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
fn a_vm_boots_from_an_image_on_the_primary_vms_drive() {
    boot_image(false, false, false);
}

#[test]
fn a_vm_boots_from_a_custom_image_on_the_primary_vms_drive() {
    boot_image(true, false, false);
}

#[test]
fn a_vm_boots_from_a_custom_hdd_on_the_primary_vms_drive() {
    boot_image(true, true, false);
}

#[test]
fn a_vm_boots_from_a_dim_image_on_the_primary_vms_drive() {
    boot_image(true, false, true);
}

fn boot_image(custom: bool, hdd: bool, dim: bool) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |name: &str| std::fs::read(root.join(name)).ok();
    let (Some(ipl), Some(cgrom), Some(human)) = (
        read("roms/IPLROM.DAT"),
        read("roms/CGROM.DAT"),
        read("disks/HUMAN302.XDF"),
    ) else {
        eprintln!("ROMs or disks not found, skipping");
        return;
    };
    let sx: Vec<(String, Vec<u8>)> = (1..=6)
        .filter_map(|i| read(&format!("disks/SXWIN31{i}.XDF")).map(|b| (format!("SXWIN31{i}"), b)))
        .collect();
    if sx.is_empty() {
        eprintln!("SX-Window disks not found, skipping");
        return;
    }
    let sx_refs: Vec<(&str, &[u8])> = sx.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();

    // The primary's HDD holds the VM's drive 0 at A:\VMX68K\DRIVE0.XDF.
    let mut dir = [b' '; 8];
    dir[..6].copy_from_slice(b"VMX68K");
    let mut name = [b' '; 8];
    name[..6].copy_from_slice(b"DRIVE0");
    let drive0 = ExtractedFile {
        path_dirs: vec![(dir, [b' '; 3], [0u8; 10])],
        raw_name: name,
        raw_ext: *b"XDF",
        raw_name2: [0u8; 10],
        attr: 0x20,
        time: 0x6000,
        date: 0x5821,
        data: human.clone(),
    };
    let mut files = vec![drive0];
    if dim {
        let mut data = vec![1; 256];
        data[0] = 0;
        data.extend_from_slice(&human);
        files[0].data = data;
    }
    let mut sram = None;
    if hdd {
        files[0].data = msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd(&human, &[], 8);
        let mut machine = X68kVm::new(VmConfig::app_vm().with_ram_mb(4));
        machine.bus.set_boot_from_scsi(true);
        sram = Some(machine.bus.sram().to_vec());
    }
    let image_bytes = files[0].data.len();
    if custom {
        // A different image from the legacy DRIVE0 path proves that the configured
        // handle, rather than the old fixed image, reaches the VM.
        files[0].raw_name = *b"CUSTOM  ";
        let mut library = ResourceLibrary::parse(&msx68k_bus::vmx68k_library::setup_library(
            0,
            0,
            4,
            sram.as_deref(),
        ))
        .unwrap();
        let rows: Vec<(u16, &[u8])> = if hdd {
            vec![(0, b"A:\\VMX68K\\CUSTOM.XDF")]
        } else {
            vec![
                (0, b"A:\\VMX68K\\CUSTOM.XDF"),
                (0, b"A:\\VMX68K\\ABSENT.XDF"),
                (0, b"A:\\VMX68K\\CUSTOM.XDF"),
            ]
        };
        library.insert(
            b"V68D",
            Resource::new(0, msx68k_bus::vmx68k_library::setup_drives(true, &rows)),
        );
        let mut setup = files[0].clone();
        setup.raw_name = *b"DEFAULT ";
        setup.raw_ext = *b"V68";
        setup.data = library.to_bytes();
        files.push(setup);
    }
    let mut primary = X68kVm::new(
        VmConfig::primary_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    primary.bus.load_cgrom(&cgrom);
    primary.bus.load_ipl_rom(&ipl);
    assert!(primary.bus.insert_scsi_hdd(
        0,
        build_bootable_sxwin_scsi_hdd_with_files(&human, &sx_refs, 32, files)
    ));
    primary.bus.set_boot_from_scsi(true);
    primary.reset().unwrap();
    primary.run_steps(30_000_000);

    // \X68 lists VMX68K.X after VMX68K.LB, as the first icon of the third row.
    click(&mut primary, (22, 340), 2, 3_000_000);
    click(&mut primary, (104, 255), 2, 8_000_000);
    let events = primary.bus.take_vm_events();
    let [VmEvent::Opened {
        id,
        window,
        drive0_bytes,
        custom: ref custom_drives,
        sram: ref setup_sram,
        ..
    }] = events[..]
    else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    if custom {
        let drives = custom_drives.as_ref().expect("custom drive table");
        if hdd {
            assert!(matches!(drives.as_slice(), [CustomDrive::File { len, .. }]
                if *len as usize == image_bytes));
        } else {
            assert!(matches!(drives.as_slice(), [
                CustomDrive::Image { len, .. }, CustomDrive::Missing, CustomDrive::Image { .. }
            ] if *len as usize == image_bytes));
        }
    } else {
        assert!(custom_drives.is_none());
        assert_eq!(
            drive0_bytes as usize,
            human.len(),
            "VMX68K.X found the image's size"
        );
    }
    // Another window in front: the host's E_USER1 must still reach VMX68K.X behind it.
    click(&mut primary, (22, 340), 2, 3_000_000);
    assert_ne!(primary.bus.active_window(), Some(window));

    // As the app does: drive 0 of the VM is the primary's image, served by VMX68K.X.
    let (bridge, requests) = vm_bridge();
    primary.bus.attach_vm_bridge(id, requests);
    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert_eq!(
        setup_sram, &sram,
        "the setup passes its SRAM through VmOpen"
    );
    if let Some(sram) = setup_sram {
        vm.bus.sram_mut().copy_from_slice(sram);
    }
    let saved_sram = vm.bus.sram().to_vec();
    if let Some(drives) = custom_drives {
        assert!(vm.bus.install_custom_drives(bridge, drives).is_empty());
        assert_eq!(
            vm.bus.sram(),
            saved_sram.as_slice(),
            "mounting preserves the boot choice"
        );
        if hdd {
            assert!(vm.bus.is_scsi_rom_enabled());
            assert!(
                vm.bus.scsi_hdd_data(0).is_none(),
                "the HDD is not cached in memory"
            );
        } else {
            assert!(vm.bus.fdc.drives.iter().all(|drive| drive.connected));
            assert!(!vm.bus.fdc.drives[1].is_inserted());
            assert!(vm.bus.fdc.drives[2].is_inserted());
        }
    } else {
        let disk = RemoteDisk::new(bridge, drive0_bytes as usize);
        assert!(vm.bus.fdc.insert_remote_disk(0, disk, false));
    }
    vm.reset().unwrap();

    // The VM's thread waits while the primary serves it, so the prompt is looked for there.
    let prompt = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&prompt);
    let written = Arc::new(AtomicBool::new(false));
    let verified = Arc::clone(&written);
    let accesses = vm.bus.drive_access_counter();
    let thread = VmThread::spawn_with_vblank(vm, Pace::Unthrottled, move |vm| {
        if !seen.load(Ordering::Relaxed)
            && vm.bus.crtc.frame_count() % 30 == 0
            && decode_tvram_ank_lines(&vm.bus, 96, 32)
                .iter()
                .any(|l| l.starts_with("A>"))
        {
            if hdd {
                // Two bridge chunks through the real VMX68K.X task, written to
                // unused image sectors, then read back through SCSI IOCS.
                let buffer = 0x300000;
                let saved = vm.bus.main_ram()[buffer..buffer + 65536].to_vec();
                let data: Vec<u8> = (0..65536).map(|i| (i * 13) as u8).collect();
                vm.bus.load_ram(buffer as u32, &data).unwrap();
                let mut call = vm.cpu_state.clone();
                call.d[1] = 0x27;
                call.d[2] = (image_bytes / 1024 - 64) as u32;
                call.d[3] = 64;
                call.d[4] = 0;
                call.d[5] = 2;
                call.a[1] = buffer as u32;
                vm.bus
                    .handle_hypercall(&mut call, msx68k_bus::HYPERCALL_OPCODE, 0xFC0068);
                let write_ok = call.d[0] == 0;
                vm.bus.load_ram(buffer as u32, &vec![0; 65536]).unwrap();
                call.d[1] = 0x26;
                vm.bus
                    .handle_hypercall(&mut call, msx68k_bus::HYPERCALL_OPCODE, 0xFC0068);
                verified.store(
                    write_ok && call.d[0] == 0 && vm.bus.main_ram()[buffer..buffer + 65536] == data,
                    Ordering::Relaxed,
                );
                vm.bus.load_ram(buffer as u32, &saved).unwrap();
            }
            seen.store(true, Ordering::Relaxed);
        }
    });
    let started = Instant::now();
    while !prompt.load(Ordering::Relaxed) {
        if started.elapsed() > Duration::from_secs(60) {
            // Nothing may wait on the disk while the thread is dropped.
            primary.bus.detach_vm_bridge(id);
            thread.with_vm(|vm| {
                eprintln!(
                    "PC={:08x} D={:x?} SRAM boot={:x?}",
                    vm.cpu_state.pc,
                    vm.cpu_state.d,
                    &vm.bus.sram()[0x0C..0x1A]
                );
                eprintln!("{}", decode_tvram_ank_lines(&vm.bus, 96, 32).join("\n"));
            });
            panic!("the VM must reach COMMAND.X's prompt");
        }
        primary.run_steps(200_000);
    }
    // Floppy and SCSI accesses use the same access lamp.
    assert!(
        accesses.load(Ordering::Relaxed) > 0,
        "disk accesses must count"
    );
    // Close the VM's disk before the thread, as the app does, so nothing waits for it.
    primary.bus.detach_vm_bridge(id);
    drop(thread);
    if hdd {
        assert!(
            written.load(Ordering::Relaxed),
            "SCSI writes must round-trip through VMX68K.X"
        );
        let volume =
            msx68k_bus::human_fs::HumanVolume::open(primary.bus.scsi_hdd_data_mut(0).unwrap())
                .unwrap();
        let image = volume
            .read_file(
                &[msx68k_bus::human_fs::short_name("VMX68K", "")],
                &msx68k_bus::human_fs::short_name("CUSTOM", "XDF"),
            )
            .unwrap();
        let expected: Vec<u8> = (0..65536).map(|i| (i * 13) as u8).collect();
        assert_eq!(
            &image[image.len() - 65536..],
            expected.as_slice(),
            "the primary's image file holds the write, without a later flush"
        );
    }
}

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
