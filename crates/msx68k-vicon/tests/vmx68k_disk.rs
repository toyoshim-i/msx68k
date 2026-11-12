//! A VM opened by VMX68K.X boots Human68k 3.02 from a disk image on the master VM's HDD,
//! which VMX68K.X reads through the master's Human68k (`docs/VMX68K.md` Sections 3.1 and 3.5).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd_with_files, ExtractedFile};
use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::vmx68k::remote_disk;
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
fn a_vm_boots_from_an_image_on_the_master_vms_drive() {
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

    // The master's HDD holds the VM's drive 0 at A:\VMX68K\DRIVE0.XDF.
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
    let mut master = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    master.bus.load_cgrom(&cgrom);
    master.bus.load_ipl_rom(&ipl);
    assert!(master.bus.insert_scsi_hdd(
        0,
        build_bootable_sxwin_scsi_hdd_with_files(&human, &sx_refs, 32, vec![drive0])
    ));
    master.bus.set_boot_from_scsi(true);
    master.reset().unwrap();
    master.run_steps(30_000_000);

    // \X68 lists VMX68K.X after HDフォーマット.X.
    click(&mut master, (22, 340), 2, 3_000_000);
    click(&mut master, (343, 175), 2, 8_000_000);
    let events = master.bus.take_vm_events();
    let [VmEvent::Opened { id, drive0_bytes, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    assert_eq!(drive0_bytes as usize, human.len(), "VMX68K.X found the image's size");

    // As the app does: drive 0 of the VM is the master's image, served by VMX68K.X.
    let (disk, requests) = remote_disk(drive0_bytes as usize);
    master.bus.attach_vm_disk(id, requests);
    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert!(vm.bus.fdc.insert_remote_disk(0, disk, false));
    vm.reset().unwrap();

    // The VM's thread waits while the master serves it, so the prompt is looked for there.
    let prompt = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&prompt);
    let thread = VmThread::spawn_with_vblank(vm, Pace::Unthrottled, move |vm| {
        if vm.bus.crtc.frame_count() % 30 == 0
            && decode_tvram_ank_lines(&vm.bus, 96, 32).iter().any(|l| l.starts_with("A>"))
        {
            seen.store(true, Ordering::Relaxed);
        }
    });
    let started = Instant::now();
    while !prompt.load(Ordering::Relaxed) {
        if started.elapsed() > Duration::from_secs(60) {
            // Nothing may wait on the disk while the thread is dropped.
            master.bus.detach_vm_disk(id);
            panic!("the VM must reach COMMAND.X's prompt");
        }
        master.run_steps(200_000);
    }
    // Close the VM's disk before the thread, as the app does, so nothing waits for it.
    master.bus.detach_vm_disk(id);
    drop(thread);
}
