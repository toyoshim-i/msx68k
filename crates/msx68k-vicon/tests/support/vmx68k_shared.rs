//! In shared mode, a VM boots Human68k from the primary VM's own drive A:, bridged at the
//! file level, with its own CONFIG.SYS so that SX-Window does not start again, and what it
//! writes lands on the primary's drive (`docs/VMX68K.md` Section 2).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::boot_with;
use msx68k_bus::human_fs::{short_name, HumanVolume};
use msx68k_bus::remote_fs::RemoteFs;
use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd, ExtractedFile};
use msx68k_bus::sx_resource::{Resource, ResourceLibrary};
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
                    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1E, 0x1F, 0x20,
                    0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30,
                ];
                (
                    codes[row.iter().position(|&b| b == c as u8).unwrap()],
                    false,
                )
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

/// Types `text` on the VM's keyboard while the primary runs, serving the VM's requests.
fn type_text(thread: &VmThread, primary: &mut X68kVm, text: &str) {
    const SHIFT: u8 = 0x70;
    let mut bytes = Vec::new();
    for (code, shift) in scancodes(text) {
        if shift {
            bytes.push(SHIFT);
        }
        bytes.extend([code, code | 0x80]);
        if shift {
            bytes.push(SHIFT | 0x80);
        }
    }
    thread.post(move |vm| {
        for byte in bytes {
            vm.bus.mfp.push_keyboard_byte(byte);
        }
    });
    primary.run_steps(50_000);
}

/// Runs the primary, serving the VM, until `done` holds of its HDD or a few seconds pass.
fn wait_for_disk(primary: &mut X68kVm, done: impl Fn(&HumanVolume) -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        primary.run_steps(500_000);
        let mut image = primary.bus.scsi_hdd_data(0).unwrap().to_vec();
        if done(&HumanVolume::open(&mut image).unwrap()) {
            return true;
        }
    }
    false
}

pub fn a_vm_boots_from_the_primary_vms_drive_a() {
    shared_drives(None, false, false);
}

pub fn custom_shared_drives_follow_the_floppy_drives() {
    shared_drives(Some(false), false, false);
}

pub fn custom_shared_drives_follow_the_hdd_and_floppy_drives() {
    shared_drives(Some(true), false, false);
}

pub fn custom_shared_drives_follow_the_floppy_and_nonboot_hdd() {
    shared_drives(Some(false), true, false);
}

pub fn passthrough_floppy_boots_and_writes_the_primarys_disk() {
    shared_drives(Some(false), false, true);
}

fn shared_drives(custom: Option<bool>, extra_hdd: bool, passthrough: bool) {
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

    let mut extra = Vec::new();
    if let Some(hdd) = custom {
        let image = if hdd {
            build_bootable_sxwin_scsi_hdd(&human, &[], 8)
        } else {
            human.clone()
        };
        let file = ExtractedFile {
            path_dirs: vec![(*b"VMX68K  ", *b"   ", [0; 10])],
            raw_name: if hdd { *b"HDD     " } else { *b"RAW     " },
            raw_ext: if hdd { *b"HDS" } else { *b"XDF" },
            raw_name2: [0; 10],
            attr: 0x20,
            time: 0x6000,
            date: 0x5821,
            data: image,
        };
        let mut machine = X68kVm::new(VmConfig::app_vm().with_ram_mb(4));
        if hdd {
            machine.bus.set_boot_from_scsi(true);
        }
        let sram = hdd.then(|| machine.bus.sram().to_vec());
        let mut setup = ResourceLibrary::parse(&msx68k_bus::vmx68k_library::setup_library(
            0,
            0,
            4,
            sram.as_deref(),
        ))
        .unwrap();
        // Deliberately reverse the primary's A: and B: in the shared rows.
        let mut rows: Vec<(u16, &[u8])> = if passthrough {
            vec![(2, b"B:")]
        } else {
            vec![(
                0,
                if hdd {
                    b"A:\\VMX68K\\HDD.HDS"
                } else {
                    b"A:\\VMX68K\\RAW.XDF"
                },
            )]
        };
        if extra_hdd {
            extra.push(ExtractedFile {
                raw_name: *b"HDD     ",
                raw_ext: *b"HDS",
                data: build_bootable_sxwin_scsi_hdd(&human, &[], 8),
                ..file.clone()
            });
            rows.push((0, b"A:\\VMX68K\\HDD.HDS"));
        }
        rows.extend([(1, b"B:".as_slice()), (1, b"A:".as_slice())]);
        setup.insert(
            b"V68D",
            Resource::new(0, msx68k_bus::vmx68k_library::setup_drives(true, &rows)),
        );
        extra.push(ExtractedFile {
            raw_name: *b"DEFAULT ",
            raw_ext: *b"V68",
            data: setup.to_bytes(),
            ..file.clone()
        });
        extra.push(file);
    }
    let has_hdd = custom == Some(true) || extra_hdd;
    let shared_hdd = if custom.is_none() {
        'a'
    } else if has_hdd {
        'e'
    } else {
        'd'
    };
    let shared_floppy = if custom.is_none() {
        'b'
    } else if has_hdd {
        'd'
    } else {
        'c'
    };
    let absent = if custom.is_none() {
        'd'
    } else if has_hdd {
        'f'
    } else {
        'e'
    };
    let Some(mut primary) = boot_with(extra) else {
        return;
    };
    let tag = primary.case_letter;
    let directory = format!("vmtest{tag}");
    let listing = format!("dirb{tag}");
    let config_copy = format!("cfgcpy{tag}");
    let swap = format!("swap{tag}");
    assert!(primary.bus.insert_floppy(0, human.clone(), !passthrough));
    primary.launch();
    let events = primary.bus.take_vm_events();
    let [VmEvent::Opened {
        id,
        window,
        drives,
        custom: ref custom_drives,
        sram: ref setup_sram,
        ..
    }] = events[..]
    else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    assert_eq!(
        drives, 3,
        "the primary has A: and the floppy drives B: and C:"
    );
    // Another window in front: the host's E_USER1 must still reach VMX68K.X behind it.
    primary.background_guest();
    assert_ne!(primary.bus.active_window(), Some(window));

    // The VM's A: is the primary's A:, which it boots from.
    let (mut bridge, requests) = vm_bridge();
    primary.bus.bind_vm_passthrough(id, &mut bridge);
    primary.bus.attach_vm_bridge(id, requests);
    if passthrough {
        assert!(primary.bus.fdc.drives[0].is_write_protected());
        assert!(
            !primary.bus.fdc.drives[0].write_protected,
            "the physical tab stays unchanged"
        );
    }
    let mut vm = X68kVm::new(
        VmConfig::app_vm()
            .with_speed_mode(CpuSpeedMode::Mhz10)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    if let Some(sram) = setup_sram {
        vm.bus.sram_mut().copy_from_slice(sram);
    }
    if let Some(drives) = custom_drives {
        let sram = vm.bus.sram().to_vec();
        assert!(vm.bus.install_custom_drives(bridge, drives).is_empty());
        assert_eq!(
            vm.bus.sram(),
            sram,
            "custom shares must preserve the boot choice"
        );
    } else {
        vm.bus
            .install_remote_boot(RemoteFs::new(bridge, (1..=drives).collect()));
    }
    vm.reset().unwrap();

    // The VM's thread waits while the primary serves it, so the prompt is looked for there.
    let prompt = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&prompt);
    let thread_screen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let screen = Arc::clone(&thread_screen);
    let accesses = vm.bus.drive_access_counter();
    let thread = primary.thread(
        id,
        VmThread::spawn_with_vblank(vm, Pace::Unthrottled, move |vm| {
            if vm.bus.crtc.frame_count() % 30 == 0 {
                let lines = decode_tvram_ank_lines(&vm.bus, 96, 32);
                if lines.iter().any(|l| l.starts_with("A>")) {
                    seen.store(true, Ordering::Relaxed);
                }
                *screen.lock().unwrap() = lines;
            }
        }),
    );
    let started = Instant::now();
    while !prompt.load(Ordering::Relaxed) {
        if started.elapsed() > Duration::from_secs(60) {
            // Nothing may wait on the disk while the thread is dropped.
            primary.bus.detach_vm_bridge(id);
            let lines = thread_screen.lock().unwrap().clone();
            panic!("the VM must reach COMMAND.X's prompt: {lines:#?}");
        }
        primary.run_steps(200_000);
    }
    // Its remote drives' requests count for the access lamp.
    assert!(
        accesses.load(Ordering::Relaxed) > 0,
        "the remote drive's requests must count"
    );
    // No drive is added beyond the configured shared units and physical drives.
    let marker = format!("done{tag}");
    type_text(
        &thread,
        &mut primary,
        &format!("{absent}:\necho {marker}\n"),
    );
    let until = Instant::now() + Duration::from_secs(10);
    let command_done = |lines: &[String]| {
        lines
            .iter()
            .position(|line| line == &marker)
            .is_some_and(|at| lines[at + 1..].iter().any(|line| line.ends_with('>')))
    };
    while !command_done(&thread_screen.lock().unwrap()) && Instant::now() < until {
        primary.run_steps(100_000);
    }
    assert!(
        command_done(&thread_screen.lock().unwrap()),
        "command did not finish"
    );
    let lines = thread_screen.lock().unwrap().clone();
    let prompts: Vec<_> = lines.iter().filter(|l| l.ends_with('>')).collect();
    assert_eq!(
        prompts.last().map(|l| l.as_str()),
        Some("A>"),
        "{absent}: must not exist: {lines:#?}"
    );

    // What the VM writes goes to the primary's drive, through the primary's Human68k, which keeps
    // names as typed.
    let dir = [short_name(&directory, "")];
    type_text(
        &thread,
        &mut primary,
        &format!("md {shared_hdd}:\\{directory}\n"),
    );
    assert!(
        wait_for_disk(&mut primary, |v| v
            .read_file(&[], &short_name(&directory, ""))
            .is_some()),
        "MD must make VMTEST on the primary's drive: {:#?}",
        thread_screen.lock().unwrap()
    );
    type_text(
        &thread,
        &mut primary,
        &format!("echo hello>{shared_hdd}:\\{directory}\\hi.txt\n"),
    );
    assert!(
        wait_for_disk(&mut primary, |v| v
            .read_file(&dir, &short_name("hi", "txt"))
            .as_deref()
            == Some(&b"hello\r\n"[..])),
        "ECHO must create VMTEST\\HI.TXT on the primary's drive"
    );
    type_text(
        &thread,
        &mut primary,
        &format!("ren {shared_hdd}:\\{directory}\\hi.txt bye.txt\n"),
    );
    assert!(
        wait_for_disk(&mut primary, |v| v
            .read_file(&dir, &short_name("bye", "txt"))
            .is_some()
            && v.read_file(&dir, &short_name("hi", "txt")).is_none()),
        "REN must rename it on the primary's drive"
    );

    // The configured floppy share lists the primary's B:, by its volume name and files
    // (HUMAN.SYS is a system file DIR leaves out), written to the configured HDD share.
    type_text(
        &thread,
        &mut primary,
        &format!("dir {shared_floppy}: >{shared_hdd}:\\{listing}.txt\n"),
    );
    assert!(
        wait_for_disk(&mut primary, |v| v
            .read_file(&[], &short_name(&listing, "txt"))
            .is_some_and(|listing| listing
                .windows(15)
                .any(|w| w == b"Human68k_Ver3  ")
                && listing.windows(7).any(|w| w == b"COMMAND"))),
        "DIR B: must list the primary's floppy: {:?}",
        {
            let mut image = primary.bus.scsi_hdd_data(0).unwrap().to_vec();
            let file = HumanVolume::open(&mut image)
                .unwrap()
                .read_file(&[], &short_name(&listing, "txt"));
            file.map(|f| String::from_utf8_lossy(&f).into_owned())
        }
    );

    if custom.is_some() {
        // A custom share exposes the primary's real CONFIG.SYS, without the
        // boot-only substitution used by shared mode, including its first unit.
        let expected = msx68k_bus::scsi::extract_files_from_xdf(&human)
            .into_iter()
            .find(|f| f.path_dirs.is_empty() && f.raw_name == *b"CONFIG  " && f.raw_ext == *b"SYS")
            .unwrap()
            .data;
        type_text(
            &thread,
            &mut primary,
            &format!("copy {shared_floppy}:\\config.sys {shared_hdd}:\\{config_copy}.txt\n"),
        );
        assert!(
            wait_for_disk(&mut primary, |v| v
                .read_file(&[], &short_name(&config_copy, "txt"))
                .is_some_and(|contents| contents == expected)),
            "custom shares must expose the original CONFIG.SYS: {:#?}",
            thread_screen.lock().unwrap()
        );
    }

    if passthrough {
        for round in 0..2 {
            type_text(&thread, &mut primary, "echo passed>a:\\passon.txt\n");
            let until = Instant::now() + Duration::from_secs(20);
            let mut found = false;
            while Instant::now() < until {
                primary.run_steps(200_000);
                found = msx68k_bus::scsi::extract_files_from_xdf(
                    primary.bus.fdc.drives[0].image.as_ref().unwrap(),
                )
                .iter()
                .any(|file| {
                    file.raw_name.eq_ignore_ascii_case(b"PASSON  ")
                        && file.raw_ext.eq_ignore_ascii_case(b"TXT")
                        && file.data == b"passed\r\n"
                });
                if found {
                    break;
                }
            }
            assert!(
                found,
                "passthrough writes must reach the primary's floppy: {:#?}",
                thread_screen.lock().unwrap()
            );
            if round == 0 {
                // Replace it with another disk of the same geometry. The guest
                // must drop its old FAT/directory cache, not just update capacity.
                primary.bus.eject_floppy(0);
                assert!(primary.bus.insert_floppy(0, human.clone(), false));
                type_text(
                    &thread,
                    &mut primary,
                    &format!("dir a: >{shared_hdd}:\\{swap}.txt\n"),
                );
                assert!(
                    wait_for_disk(&mut primary, |volume| volume
                        .read_file(&[], &short_name(&swap, "txt"))
                        .is_some_and(|text| text.windows(7).any(|w| w == b"COMMAND")
                            && !text.windows(6).any(|w| w.eq_ignore_ascii_case(b"PASSON")))),
                    "the VM must notice a replacement disk of the same format: {:#?}",
                    thread_screen.lock().unwrap()
                );
            }
        }
    }

    // Close the VM's disk before the thread, as the app does, so nothing waits for it.
    primary.close_guest(id);
    drop(thread);
    if passthrough {
        assert!(
            !primary.bus.fdc.drives[0].is_write_protected(),
            "closing restores the primary's access"
        );
        // Its Human68k already listed B: above. After release it must see the new
        // file, rather than a stale FAT/directory cached before the VM's write.
        assert_eq!(
            primary
                .rpc(msx68k_bus::vmx68k::BridgeOp::Files {
                    attr: 0x27,
                    path: b"B:\\PASSON.TXT".to_vec(),
                })
                .result,
            0,
            "the primary must see changes after release"
        );
    }
}
