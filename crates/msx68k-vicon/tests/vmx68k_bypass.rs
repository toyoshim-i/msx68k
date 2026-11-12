//! In drive bypass mode, a VM boots Human68k from the master VM's own drive A:, bridged at the
//! file level, with its own CONFIG.SYS so that SX-Window does not start again
//! (`docs/VMX68K.md` Section 2).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use msx68k_bus::remote_fs::RemoteFs;
use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;
use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::vmx68k::vm_bridge;
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
fn a_vm_boots_from_the_master_vms_drive_a() {
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

    let mut master = X68kVm::new(
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    master.bus.load_cgrom(&cgrom);
    master.bus.load_ipl_rom(&ipl);
    assert!(master.bus.insert_scsi_hdd(
        0,
        build_bootable_sxwin_scsi_hdd(&human, &sx_refs, 32)
    ));
    master.bus.set_boot_from_scsi(true);
    master.reset().unwrap();
    master.run_steps(30_000_000);

    // \X68 lists VMX68K.X after HDフォーマット.X.
    click(&mut master, (22, 340), 2, 3_000_000);
    click(&mut master, (343, 175), 2, 8_000_000);
    let events = master.bus.take_vm_events();
    let [VmEvent::Opened { id, window, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    // Another window in front: the host's E_USER1 must still reach VMX68K.X behind it.
    click(&mut master, (22, 340), 2, 3_000_000);
    assert_ne!(master.bus.active_window(), Some(window));

    // The VM's A: is the master's A:, which it boots from.
    let (bridge, requests) = vm_bridge();
    master.bus.attach_vm_bridge(id, requests);
    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    vm.bus.install_remote_boot(RemoteFs::new(bridge, vec![1]));
    vm.reset().unwrap();

    // The VM's thread waits while the master serves it, so the prompt is looked for there.
    let prompt = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&prompt);
    let thread_screen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let screen = Arc::clone(&thread_screen);
    let thread = VmThread::spawn_with_vblank(vm, Pace::Unthrottled, move |vm| {
        if vm.bus.crtc.frame_count() % 30 == 0 {
            let lines = decode_tvram_ank_lines(&vm.bus, 96, 32);
            if lines.iter().any(|l| l.starts_with("A>")) {
                seen.store(true, Ordering::Relaxed);
            }
            *screen.lock().unwrap() = lines;
        }
    });
    let started = Instant::now();
    while !prompt.load(Ordering::Relaxed) {
        if started.elapsed() > Duration::from_secs(60) {
            // Nothing may wait on the disk while the thread is dropped.
            master.bus.detach_vm_bridge(id);
            let lines = thread_screen.lock().unwrap().clone();
            panic!("the VM must reach COMMAND.X's prompt: {lines:#?}");
        }
        master.run_steps(200_000);
    }
    // Close the VM's disk before the thread, as the app does, so nothing waits for it.
    master.bus.detach_vm_bridge(id);
    drop(thread);
}
