//! VMX68K.X, in `\X68` of the built disk, opens a window and asks the host for a VM to show in it (`docs/VMX68K.md`
//! Section 3.5).

use std::path::Path;

use msx68k_bus::human_fs::{short_name, HumanVolume};
use msx68k_bus::scsi::ExtractedFile;
use msx68k_bus::sx_resource::ResourceLibrary;
use msx68k_bus::{CpuModel, CpuSpeedMode, VmEvent, X68kVm};

/// Hard disk icon on the desktop of the test disk, which opens `\X68`.
const DRIVE_ICON: (i16, i16) = (22, 340);

use crate::session::{boot, boot_with, case_id};

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

/// Opens the speed submenu of VMX68K.X's Configuration icon, in the window at its first place,
/// with the right button, and lets go on its speed `item` (from 0), or away from the menu.
fn choose_speed(vm: &mut X68kVm, item: Option<i16>, name: &str) {
    let point = |vm: &mut X68kVm, at: (i16, i16), steps: usize| {
        vm.bus.set_host_mouse_position(Some(at));
        vm.bus.scc.add_mouse_delta(1, 1);
        vm.run_steps(steps);
    };
    point(vm, (768, 52), 200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    point(vm, (768, 52), 250_000);
    // The menu shows below the point, its frequency item fifth; the submenu opens over it.
    point(vm, (802, 134), 1_500_000);
    screenshot(vm, name);
    // The menus are text over the window, which the host keeps in front of the VM's screen.
    assert_ne!(text_index(vm, (802, 134)), 0, "the frequency item");
    assert_ne!(text_index(vm, (832, 152)), 0, "the submenu");
    point(vm, (832, 134), 250_000);
    let at = item.map_or((300, 300), |item| (832, 136 + item * 16));
    point(vm, at, 250_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    point(vm, at, 750_000);
}

/// Palette index of the primary's text plane at screen `(x, y)`, 0 where it is clear.
fn text_index(vm: &X68kVm, (x, y): (usize, usize)) -> u8 {
    let tvram = vm.bus.active_tvram().unwrap();
    let (row, plane) = (
        vm.bus.active_tvram_row_bytes(),
        vm.bus.active_tvram_plane_bytes(),
    );
    let (at, bit) = (y * row + x / 8, 7 - (x & 7));
    (0..4).fold(0, |v, p| v | ((tvram[p * plane + at] >> bit) & 1) << p)
}

/// Types `text` on the primary's keyboard: lowercase letters, digits, and `:\.`.
fn type_keys(vm: &mut X68kVm, text: &str) {
    for c in text.chars() {
        let code = match c {
            'a'..='z' => {
                let row = b"qwertyuiopasdfghjklzxcvbnm";
                let codes = [
                    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1E, 0x1F, 0x20,
                    0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30,
                ];
                codes[row.iter().position(|&b| b == c as u8).unwrap()]
            }
            '1'..='9' => 0x02 + (c as u8 - b'1'),
            '0' => 0x0B,
            ':' => 0x28,
            '.' => 0x32,
            '\\' => 0x0E,
            _ => panic!("no key for {c:?}"),
        };
        for byte in [code, code | 0x80] {
            vm.bus.mfp.push_keyboard_byte(byte);
            vm.run_steps(300_000);
        }
    }
}

/// What the setup `name` of `\VMX68K` on the VM's disk keeps.
#[derive(Debug, PartialEq, Eq)]
struct Setup {
    speed: u16,
    cpu: u16,
    ram_mb: u16,
    sram: bool,
}

fn saved_setup(vm: &X68kVm, name: &str) -> Option<Setup> {
    let mut image = vm.bus.scsi_hdd_data(0)?.to_vec();
    let volume = HumanVolume::open(&mut image)?;
    let data = volume
        .read_file(&[short_name("VMX68K", "")], &short_name(name, "V68"))
        .or_else(|| {
            volume.read_file(
                &[short_name("VMX68K", "")],
                &short_name(&name.to_ascii_lowercase(), "v68"),
            )
        })?;
    let setup = ResourceLibrary::parse(&data)?;
    let word = |data: &[u8], at: usize| u16::from_be_bytes([data[at], data[at + 1]]);
    let settings = &setup.get(b"V68S", 0)?.data;
    assert_eq!(word(settings, 0), 2, "version 2");
    Some(Setup {
        speed: word(settings, 2),
        cpu: word(settings, 4),
        ram_mb: word(settings, 6),
        sram: setup.get(b"SRAM", 0).is_some(),
    })
}

/// The drives the setup `name` keeps: custom mode, and each drive's kind and text.
fn saved_drives(vm: &X68kVm, name: &str) -> Option<(bool, Vec<(u16, String)>)> {
    let mut image = vm.bus.scsi_hdd_data(0)?.to_vec();
    let volume = HumanVolume::open(&mut image)?;
    let data = volume
        .read_file(&[short_name("VMX68K", "")], &short_name(name, "V68"))
        .or_else(|| {
            volume.read_file(
                &[short_name("VMX68K", "")],
                &short_name(&name.to_ascii_lowercase(), "v68"),
            )
        })?;
    let setup = ResourceLibrary::parse(&data)?;
    let drives = &setup.get(b"V68D", 0)?.data;
    let word = |at: usize| u16::from_be_bytes([drives[at], drives[at + 1]]);
    let entries = (0..word(4) as usize)
        .map(|row| {
            let at = 6 + row * 96;
            let len = drives[at + 2] as usize;
            (
                word(at),
                String::from_utf8_lossy(&drives[at + 3..at + 3 + len]).into_owned(),
            )
        })
        .collect();
    Some((word(2) == 1, entries))
}

/// A 68000 setup with 4 MB.
fn setup(speed: u16, sram: bool) -> Option<Setup> {
    Some(Setup {
        speed,
        cpu: 0,
        ram_mb: 4,
        sram,
    })
}

fn screenshot(vm: &X68kVm, name: &str) {
    let Ok(dir) = std::env::var("MSX68K_SCREENSHOT_DIR") else {
        return;
    };
    let (width, height) = vm.bus.crtc.active_resolution();
    let tvram = vm.bus.active_tvram().unwrap();
    let (row, plane) = (
        vm.bus.active_tvram_row_bytes(),
        vm.bus.active_tvram_plane_bytes(),
    );
    let mut gray = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = y * row + x / 8;
            let bit = 7 - (x & 7);
            let index = (0..4).fold(0, |v, p| v | ((tvram[p * plane + at] >> bit) & 1) << p);
            gray.push(index * 17);
        }
    }
    let path = Path::new(&dir).join(format!("{name}.pgm"));
    let mut data = format!("P5 {width} {height} 255\n").into_bytes();
    data.extend(gray);
    std::fs::write(path, data).unwrap();
}

pub fn vmx68k_opens_a_vm_in_its_window() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };

    // A screen wide enough for the whole window, title bar and close box included.
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..1_250_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(750_000);

    // \X68 lists VMX68K.X after VMX68K.LB, as the first icon of the third row.
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    screenshot(&vm, "x68");
    click(&mut vm, (264, 255), 2, 2_000_000);
    screenshot(&vm, "vmx68k");

    let events = case_events(&mut vm);
    let [VmEvent::Opened { id: 1, window, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    // Shared mode keeps 起動装置 visible but must not open or select its submenu.
    vm.bus.set_host_mouse_position(Some((768, 52)));
    vm.run_steps(200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    vm.run_steps(250_000);
    vm.bus.set_host_mouse_position(Some((802, 118)));
    vm.run_steps(750_000);
    screenshot(&vm, "shared_boot_disabled");
    let disabled: Vec<_> = (111..123).flat_map(|y| (777..825).map(move |x| (x, y)))
        .map(|at| text_index(&vm, at)).collect();
    assert!(disabled.contains(&8) && !disabled.contains(&11),
        "boot device must remain visible in the inactive text color");
    vm.bus.set_host_mouse_position(Some((832, 152)));
    vm.run_steps(250_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(500_000);
    assert_eq!(case_events(&mut vm), [], "shared boot-device entry must be disabled");
    // Left-clicking INTERRUPT and RESET at the left end of the bar sends their VM events.
    click(&mut vm, (59, 52), 1, 500_000);
    assert_eq!(case_events(&mut vm), [VmEvent::Interrupt { id: 1 }]);
    click(&mut vm, (95, 52), 1, 500_000);
    assert_eq!(case_events(&mut vm), [VmEvent::Reset { id: 1 }]);
    // Left-clicking the Joypad icon toggles it on/off; right-clicking opens 割当....
    click(&mut vm, (131, 52), 1, 500_000);
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Joypad {
            id: 1,
            enabled: true,
            map: [0x3C, 0x3E, 0x3B, 0x3D, 0x2A, 0x2B],
        }]
    );
    vm.bus.set_host_mouse_position(Some((131, 52)));
    vm.run_steps(200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    vm.run_steps(250_000);
    vm.bus.set_host_mouse_position(Some((165, 70)));
    vm.run_steps(250_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(1_500_000);
    screenshot(&vm, "joypad_dialog");
    click(&mut vm, (446, 215), 1, 400_000); // A button
    vm.bus.mfp.push_keyboard_byte(0x35); // Space
    vm.bus.mfp.push_keyboard_byte(0xB5);
    vm.run_steps(600_000);
    screenshot(&vm, "joypad_mapped");
    click(&mut vm, (473, 315), 1, 1_000_000); // 設定
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Joypad {
            id: 1,
            enabled: true,
            map: [0x3C, 0x3E, 0x3B, 0x3D, 0x35, 0x2B],
        }]
    );
    click(&mut vm, (131, 52), 1, 500_000);
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Joypad {
            id: 1,
            enabled: false,
            map: [0x3C, 0x3E, 0x3B, 0x3D, 0x35, 0x2B],
        }]
    );
    // The content is VMX68K.X's icon bar over the VM's 768x512 screen, at the window's bounds.
    // SX-Window's icon panel at the top left of the desktop covers a corner of it.
    let layout = vm
        .bus
        .vm_window_layout(window)
        .expect("the window must be shown");
    assert_eq!((layout.left, layout.top), (40, 64));
    // Started without a setup, VMX68K.X makes the default one with what the VM starts with.
    assert_eq!(
        saved_setup(&vm, "DEFAULT"),
        setup(0, false),
        "10 MHz and no SRAM yet"
    );

    // The VM's screen shows where the window leaves the text clear: below the bar.
    assert_ne!(text_index(&vm, (300, 52)), 0, "the bar");
    for at in [(300, 64), (400, 300), (807, 575)] {
        assert_eq!(text_index(&vm, at), 0, "{at:?}");
    }
    // The new window is active, so the keyboard goes to the VM.
    assert_eq!(vm.bus.active_window(), Some(window));
    assert_eq!(layout.visible, [(45, 64, 808, 93), (40, 93, 808, 576)]);

    // The access lamp at the right of the bar is out, and lights while the host says so.
    const LAMP: (usize, usize) = (795, 52);
    const G_DGRAY: u8 = 10;
    const G_RED: u8 = 13;
    assert_eq!(text_index(&vm, LAMP), G_DGRAY);
    vm.bus.set_vm_lamp(case_id(1), true);
    vm.run_steps(500_000);
    assert_eq!(text_index(&vm, LAMP), G_RED);
    vm.bus.set_vm_lamp(case_id(1), false);
    vm.run_steps(500_000);
    assert_eq!(text_index(&vm, LAMP), G_DGRAY);

    // The window follows the VM's screen when the VM changes its mode, the bar and the lamp
    // with it, the screen's top left staying where it was.
    for (width, height) in [(512, 512), (256, 256), (768, 512)] {
        vm.bus.set_vm_screen_size(case_id(1), width, height);
        vm.run_steps(1_500_000);
        screenshot(&vm, &format!("size_{width}x{height}"));
        let layout = vm
            .bus
            .vm_window_layout(window)
            .expect("the window must be shown");
        assert_eq!((layout.left, layout.top), (40, 64), "{width}x{height}");
        let (right, bottom) = layout
            .visible
            .iter()
            .fold((0, 0), |(r, b), v| (r.max(v.2), b.max(v.3)));
        assert_eq!(
            (right, bottom),
            (40 + width as i32, 64 + height as i32),
            "{width}x{height}"
        );
        let (width, height) = (width as usize, height as usize);
        assert_eq!(
            text_index(&vm, (40 + width - 12, 52)),
            G_DGRAY,
            "the lamp at {width}"
        );
        assert_eq!(
            text_index(&vm, (40 + width - 1, 64 + height - 1)),
            0,
            "the screen at {width}"
        );
    }

    // The right button on the Configuration icon pops up its menu, and the CPU's item its
    // speeds; one chosen goes to the host at once. The submenu checks the current speed.
    choose_speed(&mut vm, Some(1), "speeds");
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Speed {
            id: 1,
            mode: CpuSpeedMode::Mhz16
        }]
    );
    choose_speed(&mut vm, Some(3), "speeds_16mhz");
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Speed {
            id: 1,
            mode: CpuSpeedMode::Burst
        }]
    );
    assert_eq!(
        saved_setup(&vm, "DEFAULT"),
        setup(3, false),
        "a speed chosen is saved"
    );

    // SRAM the VM changed is saved in its setup.
    vm.bus
        .set_vm_sram(case_id(1), vec![0x5A; msx68k_bus::vmx68k::VM_SRAM_BYTES]);
    vm.run_steps(750_000);
    assert_eq!(saved_setup(&vm, "DEFAULT"), setup(3, true));
    // Let go away from the menu, nothing is chosen.
    choose_speed(&mut vm, None, "speeds_burst");
    assert_eq!(case_events(&mut vm), []);

    // Dragging the title bar moves the content, with nothing reported by VMX68K.X.
    vm.bus.set_host_mouse_position(Some((400, 29)));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(200_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(300_000);
    for step in 1..=10 {
        vm.bus
            .set_host_mouse_position(Some((400 + step * 10, 29 + step * 10)));
        vm.bus.scc.add_mouse_delta(10, 10);
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(750_000);
    wait_ui(
        &mut vm,
        |vm| {
            vm.bus
                .vm_window_layout(window)
                .is_some_and(|layout| layout.visible == [(140, 164, 908, 676)])
        },
        "moved visible region",
    );
    screenshot(&vm, "moved");
    let layout = vm
        .bus
        .vm_window_layout(window)
        .expect("the window must be shown");
    assert_eq!((layout.left, layout.top), (140, 164));
    assert_eq!(layout.visible, [(140, 164, 908, 676)]);

    // The close box closes the VM.
    click(&mut vm, (897, 129), 1, 750_000);
    screenshot(&vm, "closed");
    assert_eq!(case_events(&mut vm), [VmEvent::Closed { id: 1 }]);
}

pub fn a_setup_opens_vmx68k_with_its_settings() {
    // A setup in \X68, with the VM at Burst and its own SRAM.
    let sram: Vec<u8> = (0..msx68k_bus::vmx68k::VM_SRAM_BYTES)
        .map(|i| (i * 7) as u8)
        .collect();
    let mut raw_name = [b' '; 8];
    raw_name[..2].copy_from_slice(b"AA");
    let setup = ExtractedFile {
        path_dirs: vec![(*b"X68     ", *b"   ", [0; 10])],
        raw_name,
        raw_ext: *b"V68",
        raw_name2: [0; 10],
        data: msx68k_bus::vmx68k_library::setup_library(3, 0, 4, Some(&sram)),
        attr: 0x20,
        time: 0x6000,
        date: 0x5821,
    };
    let Some(mut vm) = boot_with(vec![setup]) else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    // \X68 lists AA.V68 first, by its own icon, and a double-click opens it with VMX68K.X,
    // as \SHELL\ICON.LB says.
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    screenshot(&vm, "x68_setup");
    click(&mut vm, (104, 92), 2, 2_000_000);
    screenshot(&vm, "setup_opened");
    let events = case_events(&mut vm);
    let [VmEvent::Opened {
        speed,
        sram: Some(opened),
        ..
    }] = &events[..]
    else {
        panic!("the setup must open one VM with its SRAM: {events:?}");
    };
    assert_eq!(*speed, CpuSpeedMode::Burst);
    assert!(opened == &sram, "the setup's own SRAM");
}

pub fn vmx68k_runs_a_vm_in_each_of_its_windows() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..1_250_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(750_000);
    // VMX68K.X twice from \X68, the Filer's window brought forward in between.
    let mut windows = Vec::new();
    for n in 1..=2 {
        click(&mut vm, DRIVE_ICON, 2, 750_000);
        click(&mut vm, (264, 255), 2, 2_000_000);
        let events = case_events(&mut vm);
        let [VmEvent::Opened { id, window, .. }] = events[..] else {
            panic!("VMX68K.X must open VM {n}: {events:?}");
        };
        assert_eq!(id, n);
        windows.push(window);
    }
    screenshot(&vm, "two_vms");

    // Each task serves its own VM's requests: both are answered.
    let replies: Vec<_> = [1, 2]
        .map(|id| {
            let (bridge, requests) = msx68k_bus::vmx68k::vm_bridge();
            vm.bus.attach_vm_bridge(case_id(id), requests);
            std::thread::spawn(move || {
                bridge.ask(msx68k_bus::vmx68k::BridgeOp::Dskfre { drive: 1 })
            })
        })
        .into_iter()
        .collect();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !replies.iter().all(|r| r.is_finished()) && std::time::Instant::now() < until {
        vm.run_steps(200_000);
    }
    for (id, reply) in [1, 2].into_iter().zip(replies) {
        assert!(reply.is_finished(), "VM {id}'s request must be served");
        let reply = reply.join().unwrap().expect("a reply");
        assert!(reply.result as i32 >= 0, "VM {id}: {reply:?}");
    }

    // The second window, in front of the first, shows its own VM's lamp, not the first's.
    let lamp = |vm: &X68kVm| {
        let layout = vm.bus.vm_window_layout(windows[1]).unwrap();
        let right = layout.visible.iter().map(|v| v.2).max().unwrap() as usize;
        text_index(vm, (right - 12, layout.top as usize - 12))
    };
    vm.bus.set_vm_lamp(case_id(2), true);
    vm.run_steps(750_000);
    assert_eq!(lamp(&vm), 13, "VM 2's lamp lit");
    vm.bus.set_vm_lamp(case_id(1), true);
    vm.bus.set_vm_lamp(case_id(2), false);
    vm.run_steps(750_000);
    assert_eq!(lamp(&vm), 10, "VM 1's lamp is not VM 2's");
}

pub fn a_window_the_screen_cut_off_is_drawn_when_the_screen_grows() {
    // At 768x512 the window, 576 lines down to its content's bottom, runs off the screen.
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    click(&mut vm, (264, 255), 2, 2_000_000);
    let events = case_events(&mut vm);
    let [VmEvent::Opened { window, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..1_250_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    wait_ui(
        &mut vm,
        |vm| {
            [(400, 520), (400, 575), (800, 300), (800, 575)]
                .iter()
                .all(|&at| text_index(vm, at) == 0)
        },
        "grown window redraw",
    );
    screenshot(&vm, "grown");
    // What was off the screen is clear for the VM's screen, its visible region reaches it,
    // and the window's frame is there.
    for at in [(400, 520), (400, 575), (800, 300), (800, 575)] {
        assert_eq!(text_index(&vm, at), 0, "{at:?}");
    }
    let layout = vm.bus.vm_window_layout(window).unwrap();
    let (right, bottom) = layout
        .visible
        .iter()
        .fold((0, 0), |(r, b), v| (r.max(v.2), b.max(v.3)));
    assert_eq!((right, bottom), (808, 576));
    assert_ne!(text_index(&vm, (400, 576)), 0, "the bottom of the frame");
}

pub fn the_configuration_dialog_sets_the_cpu_and_memory_and_restarts_the_vm() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..1_250_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(750_000);
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    click(&mut vm, (264, 255), 2, 2_000_000);
    let events = case_events(&mut vm);
    let [VmEvent::Opened {
        id: 1,
        cpu: CpuModel::Mc68000,
        ram_mb: 4,
        ..
    }] = events[..]
    else {
        panic!("VMX68K.X must open a 68000 VM with 4 MB: {events:?}");
    };
    let point = |vm: &mut X68kVm, at: (i16, i16), steps: usize| {
        vm.bus.set_host_mouse_position(Some(at));
        vm.bus.scc.add_mouse_delta(1, 1);
        vm.run_steps(steps);
    };

    // 設定..., the Configuration menu's third item, opens the dialog.
    point(&mut vm, (768, 52), 200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    point(&mut vm, (768, 52), 250_000);
    point(&mut vm, (802, 102), 250_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    point(&mut vm, (802, 102), 1_500_000);
    screenshot(&vm, "dialog");
    assert_ne!(
        text_index(&vm, (300, 200)),
        0,
        "the dialog over the VM's screen"
    );

    let path_pixels = |vm: &X68kVm| -> Vec<u8> {
        (125..140)
            .flat_map(|y| (290..535).map(move |x| text_index(vm, (x, y))))
            .collect()
    };
    vm.run_steps(1_000_000);
    let readonly_path = path_pixels(&vm);
    click(&mut vm, (300, 130), 1, 500_000);
    type_keys(&mut vm, "bad");
    assert_eq!(
        path_pixels(&vm),
        readonly_path,
        "normal settings path must be readonly"
    );

    // These graphics settings apply without entering the hardware restart flow.
    click(&mut vm, (228, 381), 1, 350_000);
    click(&mut vm, (228, 409), 1, 350_000);
    click(&mut vm, (402, 482), 1, 750_000);
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::SpriteOptions {
            id: 1,
            options: msx68k_bus::sprite::SpriteOptions::from_bits(3),
        }]
    );
    assert_eq!(vm.bus.vm_sprite_options(case_id(1)).bits(), 3);
    let mut image = vm.bus.scsi_hdd_data(0).unwrap().to_vec();
    let volume = HumanVolume::open(&mut image).unwrap();
    let data = volume
        .read_file(&[short_name("VMX68K", "")], &short_name("DEFAULT", "V68"))
        .unwrap();
    assert_eq!(
        ResourceLibrary::parse(&data)
            .unwrap()
            .get(b"V68G", 0)
            .unwrap()
            .data,
        [0, 3]
    );
    // Reopening initializes both checked controls; toggling them back disables both.
    for at in [(768, 52), (802, 102)] {
        point(&mut vm, at, 200_000);
        vm.bus.scc.set_mouse_buttons(false, at == (768, 52));
        point(&mut vm, at, 250_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    point(&mut vm, (802, 102), 1_000_000);
    click(&mut vm, (228, 381), 1, 350_000);
    click(&mut vm, (228, 409), 1, 350_000);
    click(&mut vm, (402, 482), 1, 750_000);
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::SpriteOptions {
            id: 1,
            options: msx68k_bus::sprite::SpriteOptions::default(),
        }]
    );
    point(&mut vm, (768, 52), 200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    point(&mut vm, (768, 52), 250_000);
    point(&mut vm, (802, 102), 250_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    point(&mut vm, (802, 102), 1_000_000);

    // The 68030, and the slider's knob dragged from 4 MB to 8 MB.
    click(&mut vm, (420, 174), 1, 500_000);
    point(&mut vm, (329, 212), 300_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    point(&mut vm, (329, 212), 300_000);
    for x in (329..=388).step_by(4) {
        point(&mut vm, (x, 212), 100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    point(&mut vm, (388, 212), 250_000);
    screenshot(&vm, "dialog_set");

    // カスタム drives: 詳細... gives three, an image file, a shared drive, and a passthrough one.
    click(&mut vm, (300, 275), 1, 500_000);
    click(&mut vm, (480, 275), 1, 1_500_000);
    wait_ui(
        &mut vm,
        |vm| text_index(vm, (500, 194)) == 11,
        "details fields drawn",
    );
    screenshot(&vm, "dialog_drives");
    click(&mut vm, (275, 156), 1, 500_000);
    click(&mut vm, (275, 156), 1, 500_000);
    click(&mut vm, (290, 222), 1, 500_000);
    click(&mut vm, (360, 242), 1, 500_000);
    click(&mut vm, (500, 202), 1, 500_000);
    type_keys(&mut vm, "a:\\vmx68k\\game.xdf");
    click(&mut vm, (500, 222), 1, 500_000);
    type_keys(&mut vm, "c:");
    click(&mut vm, (500, 242), 1, 500_000);
    type_keys(&mut vm, "b:");
    screenshot(&vm, "dialog_drives_set");
    click(&mut vm, (529, 374), 1, 1_000_000);
    screenshot(&vm, "dialog_drives_kept");
    assert_eq!(
        saved_drives(&vm, "DEFAULT").map(|(custom, _)| custom),
        Some(false),
        "kept, not saved yet"
    );

    // 設定 saves the setup and asks whether to restart the VM now: はい restarts it.
    click(&mut vm, (402, 482), 1, 1_000_000);
    screenshot(&vm, "dialog_restart");
    assert_eq!(case_events(&mut vm), [], "not before the answer");
    assert_eq!(
        saved_setup(&vm, "DEFAULT"),
        Some(Setup {
            speed: 0,
            cpu: 1,
            ram_mb: 8,
            sram: false
        })
    );
    assert_eq!(
        saved_drives(&vm, "DEFAULT"),
        Some((
            true,
            vec![
                (0, "a:\\vmx68k\\game.xdf".into()),
                (1, "c:".into()),
                (2, "b:".into())
            ]
        ))
    );
    click(&mut vm, (610, 198), 1, 1_000_000);
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Restart {
            id: 1,
            cpu: CpuModel::Mc68030,
            ram_mb: 8,
            speed: CpuSpeedMode::Mhz25,
            custom: Some(vec![
                msx68k_bus::vmx68k::CustomDrive::Missing,
                msx68k_bus::vmx68k::CustomDrive::Shared { drive: 3 },
                msx68k_bus::vmx68k::CustomDrive::Passthrough { drive: 2 },
            ])
        }]
    );
    assert_eq!(
        text_index(&vm, (300, 200)),
        0,
        "the VM's screen back where the dialog was"
    );

    // The menu now has the 68030's speeds, 25MHz and 最高速, the second Burst.
    choose_speed(&mut vm, Some(1), "speeds_68030");
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Speed {
            id: 1,
            mode: CpuSpeedMode::Burst
        }]
    );
}

pub fn new_setup_uses_the_common_form_without_changing_its_creator() {
    let Some(mut vm) = boot() else { return };
    vm.bus.request_screen_resolution(1024, 768);
    vm.run_steps(2_000_000);
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    click(&mut vm, (264, 255), 2, 2_000_000);
    assert!(matches!(
        case_events(&mut vm).as_slice(),
        [VmEvent::Opened { id: 1, .. }]
    ));
    let original = saved_drives(&vm, "DEFAULT");
    let original_setup = saved_setup(&vm, "DEFAULT");
    let open_new = |vm: &mut X68kVm| {
        vm.bus.set_host_mouse_position(Some((768, 52)));
        vm.run_steps(200_000);
        vm.bus.scc.set_mouse_buttons(false, true);
        vm.run_steps(250_000);
        vm.bus.set_host_mouse_position(Some((802, 70)));
        vm.run_steps(250_000);
        vm.bus.scc.set_mouse_buttons(false, false);
        vm.run_steps(2_000_000);
        wait_ui(
            vm,
            |vm| {
                let ink = |left, top, right, bottom| {
                    (top..bottom).any(|y| (left..right).any(|x| text_index(vm, (x, y)) == 11))
                };
                (125..137).any(|y| (290..400).any(|x| text_index(vm, (x, y)) == 10))
                    && ink(346, 305, 398, 317)
                    && ink(380, 475, 420, 487)
            },
            "new form fully drawn",
        );
    };
    let set_path = |vm: &mut X68kVm| {
        click(vm, (290, 130), 1, 500_000);
        vm.bus.set_host_mouse_position(Some((286, 130)));
        vm.run_steps(200_000);
        vm.bus.scc.set_mouse_buttons(true, false);
        vm.run_steps(200_000);
        for x in (286..=548).step_by(8) {
            vm.bus.set_host_mouse_position(Some((x, 130)));
            vm.run_steps(100_000);
        }
        vm.bus.scc.set_mouse_buttons(false, false);
        vm.run_steps(300_000);
        type_keys(vm, "a:\\vmx68k\\edit.v68");
    };
    open_new(&mut vm);
    screenshot(&vm, "new_common");
    click(&mut vm, (420, 174), 1, 500_000);
    click(&mut vm, (228, 381), 1, 350_000);
    click(&mut vm, (228, 409), 1, 350_000);
    click(&mut vm, (504, 482), 1, 750_000);
    assert_eq!(saved_setup(&vm, "DEFAULT"), original_setup);
    assert_eq!(saved_drives(&vm, "DEFAULT"), original);
    assert!(saved_setup(&vm, "NEW").is_none());
    assert_eq!(
        case_events(&mut vm),
        [],
        "canceling new must not touch the running VM"
    );

    open_new(&mut vm);
    set_path(&mut vm);
    click(&mut vm, (420, 174), 1, 500_000);
    vm.bus.set_host_mouse_position(Some((329, 212)));
    vm.run_steps(200_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(200_000);
    for x in (329..=388).step_by(3) {
        vm.bus.set_host_mouse_position(Some((x, 212)));
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(500_000);
    click(&mut vm, (300, 275), 1, 500_000);
    click(&mut vm, (480, 275), 1, 1_500_000);
    screenshot(&vm, "new_details_before");
    wait_ui(
        &mut vm,
        |vm| text_index(vm, (500, 194)) == 11,
        "shared details are drawn",
    );
    click(&mut vm, (500, 202), 1, 500_000);
    type_keys(&mut vm, "a:\\vmx68k\\raw.xdf");
    click(&mut vm, (529, 374), 1, 1_000_000);
    // Native CI_ADJBTN: increment the indexed choices with its right arrow.
    click(&mut vm, (369, 311), 2, 1_000_000); // Clamp at the 68030's highest speed.
    click(&mut vm, (347, 311), 1, 500_000); // Decrease to 25MHz.
    click(&mut vm, (369, 311), 1, 500_000);
    screenshot(&vm, "new_frequency");
    click(&mut vm, (369, 347), 7, 1_000_000); // STD -> SCSI2
    let settings_window = vm.bus.active_window();
    click(&mut vm, (369, 347), 6, 1_000_000); // SCSI2 -> ROM, no automatic dialog.
    vm.run_steps(3_000_000);
    screenshot(&vm, "rom_selected");
    assert_eq!(vm.bus.active_window(), settings_window);
    click(&mut vm, (310, 347), 1, 1_000_000); // ROM title opens its address editor.
    assert_ne!(vm.bus.active_window(), settings_window);
    // The existing ROM editor's Cancel returns without changing its address.
    click(&mut vm, (480, 280), 1, 1_000_000);
    assert_eq!(vm.bus.active_window(), settings_window);
    click(&mut vm, (347, 347), 6, 1_000_000); // ROM -> SCSI2.
    screenshot(&vm, "new_boot");
    wait_ui(
        &mut vm,
        |vm| {
            (125..137).any(|y| (290..400).any(|x| text_index(vm, (x, y)) == 10))
                && (305..317).any(|y| (286..334).any(|x| text_index(vm, (x, y)) == 11))
        },
        "common form repainted after selecting a device",
    );
    vm.run_steps(2_000_000); // Finish native redraw before the review screenshot.
    screenshot(&vm, "new_setup");
    click(&mut vm, (228, 381), 1, 600_000);
    click(&mut vm, (228, 409), 1, 600_000);
    click(&mut vm, (228, 430), 1, 600_000);
    click(&mut vm, (228, 454), 1, 600_000);
    click(&mut vm, (402, 482), 1, 2_500_000);
    screenshot(&vm, "new_setup_started");
    assert_eq!(saved_setup(&vm, "DEFAULT"), original_setup);
    assert_eq!(saved_drives(&vm, "DEFAULT"), original);
    assert_eq!(
        saved_setup(&vm, "EDIT"),
        Some(Setup {
            speed: 1,
            cpu: 1,
            ram_mb: 8,
            sram: true
        })
    );
    assert_eq!(
        saved_drives(&vm, "EDIT"),
        Some((true, vec![(0, "a:\\vmx68k\\raw.xdf".into())]))
    );
    let events = case_events(&mut vm);
    let [VmEvent::Opened {
        id: 2,
        cpu: CpuModel::Mc68030,
        ram_mb: 8,
        speed: CpuSpeedMode::Burst,
        sram: Some(sram),
        custom: Some(drives),
        ..
    }, VmEvent::SpriteOptions { id: 2, options }, VmEvent::Joypad { id: 2, enabled: false, .. }] = events.as_slice()
    else {
        panic!("new settings must create one separately configured VM: {events:?}");
    };
    assert_eq!(options.bits(), 3);
    assert_eq!(
        vm.bus.vm_sprite_options(case_id(1)).bits(),
        0,
        "creator unchanged"
    );
    assert_eq!(
        vm.bus.vm_sprite_options(case_id(2)).bits(),
        3,
        "new setup reloaded"
    );
    assert_eq!(msx68k_bus::vmx68k::boot_choice(sram), 7);
    assert!(matches!(
        drives.as_slice(),
        [msx68k_bus::vmx68k::CustomDrive::Image { .. }]
    ));
    let bytes = |vm: &X68kVm| {
        let mut image = vm.bus.scsi_hdd_data(0).unwrap().to_vec();
        HumanVolume::open(&mut image)
            .unwrap()
            .read_file(&[short_name("VMX68K", "")], &short_name("edit", "v68"))
            .unwrap()
    };
    let original_new = bytes(&vm);
    open_new(&mut vm);
    set_path(&mut vm);
    click(&mut vm, (402, 482), 1, 1_000_000);
    assert_eq!(
        bytes(&vm),
        original_new,
        "existing setups must not be overwritten"
    );
    assert_eq!(case_events(&mut vm), []);
    vm.bus.mfp.push_keyboard_byte(0x1D);
    vm.bus.mfp.push_keyboard_byte(0x9D);
    vm.run_steps(500_000);
}

pub fn custom_fd_icons_accept_filer_drops_and_eject_without_reset() {
    floppy_controls(false);
}

pub fn dim_drops_write_omitted_tracks_and_keep_the_image_format() {
    floppy_controls(true);
}

fn floppy_controls(dim: bool) {
    use msx68k_bus::sx_resource::Resource;
    use msx68k_bus::vmx68k::{vm_bridge, BridgeOp};
    let mut setup =
        ResourceLibrary::parse(&msx68k_bus::vmx68k_library::setup_library(0, 0, 4, None)).unwrap();
    setup.insert(
        b"V68D",
        Resource::new(
            0,
            msx68k_bus::vmx68k_library::setup_drives(
                true,
                &[(0, if dim { b"A:\\X68\\ZZZ.DIM" } else { b"" })],
            ),
        ),
    );
    let file = |dir: &[u8; 8], name: &[u8; 8], ext: &[u8; 3], data| ExtractedFile {
        path_dirs: vec![(*dir, [b' '; 3], [0; 10])],
        raw_name: *name,
        raw_ext: *ext,
        raw_name2: [0; 10],
        attr: 0x20,
        time: 0x6000,
        date: 0x5821,
        data,
    };
    let image = if dim {
        let mut image = vec![0; 256];
        image[1] = 1;
        image[3] = 1;
        image.extend(vec![0x5A; 8192]);
        image.extend(vec![0x6B; 8192]);
        image
    } else {
        vec![0x5A; 737_280]
    };
    let file_bytes = image.len();
    let extension = if dim { b"DIM" } else { b"XDF" };
    let expected_path = if dim {
        "A:\\X68\\ZZZ.DIM"
    } else {
        "A:\\X68\\ZZZ.XDF"
    };
    let Some(mut vm) = boot_with(vec![
        file(b"VMX68K  ", b"DEFAULT ", b"V68", setup.to_bytes()),
        file(b"X68     ", b"ZZZ     ", extension, image.clone()),
    ]) else {
        return;
    };
    vm.bus.request_screen_resolution(1024, 768);
    vm.run_steps(2_000_000);
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    click(&mut vm, (264, 255), 2, 2_000_000);
    assert!(matches!(
        case_events(&mut vm).as_slice(),
        [VmEvent::Opened { id: 1, .. }]
    ));
    vm.bus.set_vm_floppies(case_id(1), 3, 0);
    let (bridge, requests) = vm_bridge();
    vm.bus.attach_vm_bridge(case_id(1), requests);
    vm.run_steps(500_000);
    // Move the VM right to uncover the Filer's ZZZ.XDF icon.
    vm.bus.set_host_mouse_position(Some((300, 30)));
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(300_000);
    for x in (310..=680).step_by(10) {
        vm.bus.set_host_mouse_position(Some((x, 30)));
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(500_000);
    // The warm Filer retains icon positions. Enlarge it to reach the fourth row.
    click(&mut vm, (200, 37), 1, 200_000);
    vm.bus.set_host_mouse_position(Some((425, 297)));
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(200_000);
    for y in (307..=427).step_by(10) {
        vm.bus.set_host_mouse_position(Some((425, y)));
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(300_000);
    let source = if dim { (344, 255) } else { (104, 335) };
    click(&mut vm, source, 1, 250_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(300_000);
    for i in 1..=24 {
        vm.bus.set_host_mouse_position(Some((
            source.0 + (583 - source.0) * i / 24,
            source.1 + (52 - source.1) * i / 24,
        )));
        vm.run_steps(150_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(1_500_000);
    screenshot(&vm, "fd_drop");
    let events = case_events(&mut vm);
    let [VmEvent::Media {
        id: 1,
        unit: 0,
        handle,
        len,
    }] = events.as_slice()
    else {
        panic!("drop must open the image without restarting: {events:?}");
    };
    assert_eq!(*len as usize, file_bytes);
    let handle = *handle;
    let mut layout = vm
        .bus
        .take_vm_image(handle)
        .expect("shared parser accepted the dropped image");
    assert_eq!(
        saved_drives(&vm, "DEFAULT"),
        Some((true, vec![(0, expected_path.into())]))
    );
    // Acknowledge application on the VM thread, then the icon can eject again.
    let worker_bridge = bridge.clone();
    let worker = std::thread::spawn(move || {
        let disk =
            msx68k_bus::vmx68k::RemoteDisk::with_handle(worker_bridge.clone(), handle, file_bytes);
        assert_eq!(layout.read(&disk, 0, 1024).unwrap(), vec![0x5A; 1024]);
        if dim {
            // Closing the earlier image frees a lower DOS handle. ImageSync
            // must use DUP2 when reopening so this handle keeps its identity.
            worker_bridge.ask(BridgeOp::MediaDone { unit: 0 }).unwrap();
            // The primary's DOS must extend a packed DIM and preserve its neighbours.
            assert!(layout.write(&disk, 8192 + 1024, vec![0x7C; 1024]));
            assert_eq!(layout.read(&disk, 8192, 1024).unwrap(), vec![0xE5; 1024]);
            assert_eq!(
                layout.read(&disk, 8192 + 1024, 1024).unwrap(),
                vec![0x7C; 1024]
            );
            assert_eq!(layout.read(&disk, 16384, 1024).unwrap(), vec![0x6B; 1024]);
        }
        if !dim {
            worker_bridge.ask(BridgeOp::MediaDone { unit: 0 }).unwrap();
        }
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !worker.is_finished() && std::time::Instant::now() < until {
        vm.run_steps(200_000);
    }
    assert!(worker.is_finished());
    worker.join().unwrap();
    if dim {
        let mut image = vm.bus.scsi_hdd_data(0).unwrap().to_vec();
        let file = HumanVolume::open(&mut image)
            .unwrap()
            .read_file(&[short_name("X68", "")], &short_name("ZZZ", "DIM"))
            .unwrap();
        assert_eq!(file.len(), 256 + 1_261_568);
        let (_, raw) = msx68k_bus::floppy_image::FloppyImage::decode(&file).unwrap();
        assert_eq!(&raw[8192 + 1024..8192 + 2048], vec![0x7C; 1024]);
        assert_eq!(&raw[16384..16384 + 1024], vec![0x6B; 1024]);
    }
    vm.bus.set_vm_floppies(case_id(1), 3 | 1 << 4 | 1 << 8, 0);
    vm.run_steps(500_000);
    assert_eq!(
        text_index(&vm, (591, 57)),
        13,
        "FD0 has its own access lamp"
    );
    click(&mut vm, (583, 52), 2, 1_000_000);
    let events = case_events(&mut vm);
    assert_eq!(
        events,
        [VmEvent::Media {
            id: 1,
            unit: 0,
            handle: u32::MAX,
            len: 0
        }]
    );
    assert_eq!(
        saved_drives(&vm, "DEFAULT"),
        Some((true, vec![(0, String::new())]))
    );
    let worker = std::thread::spawn(move || {
        assert_eq!(
            bridge
                .ask(BridgeOp::HandleRead {
                    handle,
                    offset: 0,
                    len: 1024
                })
                .unwrap()
                .result,
            1024,
            "old handle lives until the VM finishes its I/O"
        );
        bridge.ask(BridgeOp::MediaDone { unit: 0 }).unwrap();
        assert!(
            (bridge
                .ask(BridgeOp::HandleRead {
                    handle,
                    offset: 0,
                    len: 1024
                })
                .unwrap()
                .result as i32)
                < 0,
            "acknowledgement closes the old file"
        );
    });
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !worker.is_finished() && std::time::Instant::now() < until {
        vm.run_steps(200_000);
    }
    assert!(worker.is_finished());
    worker.join().unwrap();

    let mut guest_sram = vm.bus.sram().to_vec();
    guest_sram[12..16].copy_from_slice(&0xBFFFFCu32.to_be_bytes());
    vm.bus.set_vm_sram(case_id(1), guest_sram);
    vm.run_steps(500_000);
    // Move the VM back left so its Configuration icon on the right side of the bar is on-screen.
    vm.bus.set_host_mouse_position(Some((680, 30)));
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(300_000);
    for x in (300..=670).rev().step_by(10) {
        vm.bus.set_host_mouse_position(Some((x, 30)));
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(500_000);
    // Boot submenu, FD1: changing it saves SRAM without a Restart event.
    let point = |vm: &mut X68kVm, at| {
        vm.bus.set_host_mouse_position(Some(at));
        vm.run_steps(250_000);
    };
    point(&mut vm, (768, 52));
    vm.bus.scc.set_mouse_buttons(false, true);
    point(&mut vm, (768, 52));
    point(&mut vm, (802, 118));
    vm.run_steps(1_250_000);
    screenshot(&vm, "boot_menu");
    point(&mut vm, (832, 152));
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(1_000_000);
    let events = case_events(&mut vm);
    assert!(
        matches!(
            events.as_slice(),
            [VmEvent::Boot {
                id: 1,
                choice: 2,
                ..
            }]
        ),
        "Boot menu must save FD1: {events:?}"
    );
    let mut image = vm.bus.scsi_hdd_data(0).unwrap().to_vec();
    let data = HumanVolume::open(&mut image)
        .unwrap()
        .read_file(&[short_name("VMX68K", "")], &short_name("DEFAULT", "V68"))
        .unwrap();
    let saved = ResourceLibrary::parse(&data).unwrap();
    assert_eq!(
        msx68k_bus::vmx68k::boot_choice(&saved.get(b"SRAM", 0).unwrap().data),
        2
    );
    point(&mut vm, (768, 52));
    vm.bus.scc.set_mouse_buttons(false, true);
    point(&mut vm, (768, 52));
    point(&mut vm, (802, 118));
    vm.run_steps(1_250_000);
    point(&mut vm, (832, 334)); // ROM...
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(1_500_000);
    click(&mut vm, (440, 280), 1, 1_000_000); // accept the current BFFFFC address
    assert_eq!(
        case_events(&mut vm),
        [VmEvent::Boot {
            id: 1,
            choice: 13,
            rom: 0xBFFFFC
        }]
    );
    vm.bus.set_vm_floppies(case_id(1), 0, 13);
    vm.run_steps(500_000);
    assert_eq!(
        text_index(&vm, (211, 57)),
        9,
        "retiring the FD icons clears every lamp plane"
    );
}

fn case_events(vm: &mut X68kVm) -> Vec<VmEvent> {
    let base = case_id(1);
    let mut events = vm.bus.take_vm_events();
    for event in &mut events {
        let id = match event {
            VmEvent::Opened { id, .. }
            | VmEvent::Restart { id, .. }
            | VmEvent::Closed { id }
            | VmEvent::Media { id, .. }
            | VmEvent::Boot { id, .. }
            | VmEvent::Speed { id, .. }
            | VmEvent::SpriteOptions { id, .. }
            | VmEvent::Reset { id }
            | VmEvent::Interrupt { id }
            | VmEvent::Joypad { id, .. }
            | VmEvent::VSync { id, .. } => id,
        };
        *id = *id - base + 1;
    }
    events
}

fn wait_ui(vm: &mut X68kVm, ready: impl Fn(&X68kVm) -> bool, label: &str) {
    for _ in 0..100 {
        if ready(vm) {
            return;
        }
        vm.run_steps(50_000);
    }
    screenshot(vm,"ui_timeout");
    assert!(ready(vm), "UI condition timed out: {label}");
}

pub fn settings_allow_filer_drops_and_restore_the_parent() {
    let Some(mut vm) = boot() else { return };
    vm.bus.request_screen_resolution(1024, 768);
    vm.run_steps(2_000_000);
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    click(&mut vm, (264, 255), 2, 2_000_000);
    assert!(matches!(
        case_events(&mut vm).as_slice(),
        [VmEvent::Opened { .. }]
    ));
    vm.bus.set_host_mouse_position(Some((768, 52)));
    vm.run_steps(200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    vm.run_steps(250_000);
    vm.bus.set_host_mouse_position(Some((802, 102)));
    vm.run_steps(250_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(1_500_000);
    click(&mut vm, (300, 275), 1, 500_000);
    let parent = |vm: &X68kVm| -> Vec<u8> {
        (110..410)
            .flat_map(|y| (210..550).map(move |x| text_index(vm, (x, y))))
            .collect()
    };
    wait_ui(
        &mut vm,
        |vm| (341..354).any(|y| (348..378).any(|x| text_index(vm, (x, y)) == 11)),
        "enabled boot selector drawn",
    );
    // Modeless controls repaint independently: the boot selector can appear before
    // the CPU/path and choice controls. Require them together, along with the
    // path label, before saving the redraw baseline.
    wait_ui(
        &mut vm,
        |vm| (166..185).any(|y| (284..382).any(|x| text_index(vm, (x, y)) == 11))
            && (123..145).any(|y| (284..548).any(|x| text_index(vm, (x, y)) == 8))
            && (123..140).any(|y| (212..276).any(|x| text_index(vm, (x, y)) == 11))
            && (305..318).any(|y| (348..378).any(|x| text_index(vm, (x, y)) == 11))
            && (341..354).any(|y| (348..378).any(|x| text_index(vm, (x, y)) == 11)),
        "settings controls and path drawn",
    );
    vm.run_steps(2_000_000);
    let expected = parent(&vm);
    screenshot(&vm, "settings_parent_before");
    click(&mut vm, (480, 275), 1, 1_500_000);
    let details = vm.bus.active_window().unwrap();
    wait_ui(
        &mut vm,
        |vm| text_index(vm, (500, 194)) == 11,
        "details fields drawn",
    );
    click(&mut vm, (275, 156), 1, 500_000); // Two drive rows.
    // Dropping on another row must preserve the currently edited field.
    click(&mut vm, (500, 202), 1, 300_000);
    type_keys(&mut vm, "old.xdf");
    // Activate and resize another application's window while details stays open.
    click(&mut vm, DRIVE_ICON, 2, 750_000);
    assert_ne!(
        vm.bus.active_window(),
        Some(details),
        "the Filer must be usable"
    );
    vm.bus.set_host_mouse_position(Some((425, 297)));
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(200_000);
    for y in (307..=427).step_by(10) {
        vm.bus.set_host_mouse_position(Some((425, y)));
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(300_000);
    let source = (104, 335);
    click(&mut vm, source, 1, 250_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(300_000);
    for i in 1..=24 {
        vm.bus.set_host_mouse_position(Some((
            source.0 + (500 - source.0) * i / 24,
            source.1 + (222 - source.1) * i / 24,
        )));
        vm.run_steps(150_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.run_steps(1_500_000);
    screenshot(&vm, "settings_drop");
    click(&mut vm, (529, 374), 1, 1_000_000);
    wait_ui(
        &mut vm,
        |vm| parent(vm) == expected,
        "settings redrawn after details closes",
    );
    assert_eq!(
        saved_drives(&vm, "DEFAULT").map(|(custom, _)| custom),
        Some(false)
    );
    click(&mut vm, (402, 482), 1, 1_000_000);
    assert_eq!(
        saved_drives(&vm, "DEFAULT"),
        Some((true, vec![(0, "old.xdf".into()), (0, "A:\\X68\\ZZZ.XDF".into())]))
    );
    // Keep the running VM; the next lifecycle scenario must still see normal cleanup.
    click(&mut vm, (680, 198), 1, 1_000_000);
}
