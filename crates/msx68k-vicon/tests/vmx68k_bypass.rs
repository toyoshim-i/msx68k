//! In drive bypass mode, a VM boots Human68k from the master VM's own drive A:, bridged at the
//! file level, with its own CONFIG.SYS so that SX-Window does not start again, and what it
//! writes lands on the master's drive (`docs/VMX68K.md` Section 2).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use msx68k_bus::human_fs::{short_name, HumanVolume};
use msx68k_bus::remote_fs::RemoteFs;
use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;
use msx68k_bus::vm_thread::{Pace, VmThread};
use msx68k_bus::vmx68k::vm_bridge;
use msx68k_bus::{CpuSpeedMode, VmConfig, VmEvent, X68kVm};
use msx68k_vicon::decode_tvram_ank_lines;

/// X68000 key codes of the characters the test types.
fn scancodes(text: &str) -> Vec<(u8, bool)> {
    text.chars()
        .map(|c| match c {
            'a'..='z' => {
                let row = b"qwertyuiopasdfghjklzxcvbnm";
                let codes = [
                    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1E, 0x1F, 0x20, 0x21,
                    0x22, 0x23, 0x24, 0x25, 0x26, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30,
                ];
                (codes[row.iter().position(|&b| b == c as u8).unwrap()], false)
            }
            ' ' => (0x35, false),
            '.' => (0x32, false),
            ':' => (0x28, false),
            '>' => (0x32, true),
            '\\' => (0x0E, false),
            '\n' => (0x1D, false),
            _ => panic!("no key for {c:?}"),
        })
        .collect()
}

/// Types `text` on the VM's keyboard while the master runs, serving the VM's requests.
fn type_text(thread: &VmThread, master: &mut X68kVm, text: &str) {
    const SHIFT: u8 = 0x70;
    for (code, shift) in scancodes(text) {
        let mut bytes = Vec::new();
        if shift {
            bytes.push(SHIFT);
        }
        bytes.extend([code, code | 0x80]);
        if shift {
            bytes.push(SHIFT | 0x80);
        }
        for byte in bytes {
            thread.post(move |vm| vm.bus.mfp.push_keyboard_byte(byte));
            let until = Instant::now() + Duration::from_millis(15);
            while Instant::now() < until {
                master.run_steps(50_000);
            }
        }
    }
}

/// Runs the master, serving the VM, until `done` holds of its HDD or a few seconds pass.
fn wait_for_disk(master: &mut X68kVm, done: impl Fn(&HumanVolume) -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        master.run_steps(500_000);
        let mut image = master.bus.scsi_hdd_data(0).unwrap().to_vec();
        if done(&HumanVolume::open(&mut image).unwrap()) {
            return true;
        }
    }
    false
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
    // B:, the master's first floppy drive, holds a disk for the VM to read, put in after the
    // master booted from its HDD rather than from it.
    assert!(master.bus.insert_floppy(0, human.clone(), true));

    // \X68 lists VMX68K.X after HDフォーマット.X.
    click(&mut master, (22, 340), 2, 3_000_000);
    click(&mut master, (343, 175), 2, 8_000_000);
    let events = master.bus.take_vm_events();
    let [VmEvent::Opened { id, window, drives, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    assert_eq!(drives, 3, "the master has A: and the floppy drives B: and C:");
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
    vm.bus.install_remote_boot(RemoteFs::new(bridge, (1..=drives).collect()));
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
    // What the VM writes goes to the master's drive, through the master's Human68k, which keeps
    // names as typed.
    let dir = [short_name("vmtest", "")];
    type_text(&thread, &mut master, "md vmtest\n");
    assert!(
        wait_for_disk(&mut master, |v| v.read_file(&[], &short_name("vmtest", "")).is_some()),
        "MD must make VMTEST on the master's drive: {:#?}",
        thread_screen.lock().unwrap()
    );
    type_text(&thread, &mut master, "echo hello>vmtest\\hi.txt\n");
    assert!(
        wait_for_disk(&mut master, |v| v.read_file(&dir, &short_name("hi", "txt")).as_deref() == Some(&b"hello\r\n"[..])),
        "ECHO must create VMTEST\\HI.TXT on the master's drive"
    );
    type_text(&thread, &mut master, "ren vmtest\\hi.txt bye.txt\n");
    assert!(
        wait_for_disk(&mut master, |v| v.read_file(&dir, &short_name("bye", "txt")).is_some()
            && v.read_file(&dir, &short_name("hi", "txt")).is_none()),
        "REN must rename it on the master's drive"
    );

    // B: is the master's B:: a listing of its floppy, by its volume name and files (HUMAN.SYS
    // is a system file DIR leaves out), written to A:.
    type_text(&thread, &mut master, "dir b: >dirb.txt\n");
    assert!(
        wait_for_disk(&mut master, |v| v
            .read_file(&[], &short_name("dirb", "txt"))
            .is_some_and(|listing| listing.windows(15).any(|w| w == b"Human68k_Ver3  ")
                && listing.windows(7).any(|w| w == b"COMMAND"))),
        "DIR B: must list the master's floppy: {:?}",
        {
            let mut image = master.bus.scsi_hdd_data(0).unwrap().to_vec();
            let file = HumanVolume::open(&mut image).unwrap().read_file(&[], &short_name("dirb", "txt"));
            file.map(|f| String::from_utf8_lossy(&f).into_owned())
        }
    );

    // Close the VM's disk before the thread, as the app does, so nothing waits for it.
    master.bus.detach_vm_bridge(id);
    drop(thread);
}
