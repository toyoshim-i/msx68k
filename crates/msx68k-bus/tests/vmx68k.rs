//! VMX68K.X, in `\X68` of the built disk, opens a window and asks the host for a VM to show in it (`docs/VMX68K.md`
//! Section 3.5).

use std::path::Path;

use msx68k_bus::human_fs::{short_name, HumanVolume};
use msx68k_bus::scsi::{build_bootable_sxwin_scsi_hdd_with_files, ExtractedFile};
use msx68k_bus::sx_resource::ResourceLibrary;
use msx68k_bus::{CpuModel, CpuSpeedMode, VmConfig, VmEvent, X68kVm};

/// Hard disk icon on the desktop of the test disk, which opens `\X68`.
const DRIVE_ICON: (i16, i16) = (22, 340);

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn boot() -> Option<X68kVm> {
    boot_with(Vec::new())
}

/// Boots the test disk with `extra` files on it.
fn boot_with(extra: Vec<ExtractedFile>) -> Option<X68kVm> {
    let read = |name: &str| std::fs::read(root().join(name)).ok();
    let (ipl, cgrom, human) = (
        read("roms/IPLROM.DAT")?,
        read("roms/CGROM.DAT")?,
        read("disks/HUMAN302.XDF")?,
    );
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

    let mut vm = X68kVm::new(
        VmConfig::primary_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    let disk = build_bootable_sxwin_scsi_hdd_with_files(&human, &sx_refs, 32, extra);
    assert!(vm.bus.insert_scsi_hdd(0, disk));
    vm.bus.set_boot_from_scsi(true);
    vm.reset().unwrap();
    vm.run_steps(30_000_000);
    Some(vm)
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

/// Opens the speed submenu of VMX68K.X's Configuration icon, in the window at its first place,
/// with the right button, and lets go on its speed `item` (from 0), or away from the menu.
fn choose_speed(vm: &mut X68kVm, item: Option<i16>, name: &str) {
    let point = |vm: &mut X68kVm, at: (i16, i16), steps: usize| {
        vm.bus.set_host_mouse_position(Some(at));
        vm.bus.scc.add_mouse_delta(1, 1);
        vm.run_steps(steps);
    };
    point(vm, (56, 52), 200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    point(vm, (56, 52), 1_000_000);
    // The menu shows below the point, its CPU item second; the submenu opens over it.
    point(vm, (90, 86), 6_000_000);
    screenshot(vm, name);
    // The menus are text over the window, which the host keeps in front of the VM's screen.
    assert_ne!(text_index(vm, (90, 86)), 0, "the CPU's item");
    assert_ne!(text_index(vm, (120, 104)), 0, "the submenu");
    point(vm, (120, 86), 1_000_000);
    let at = item.map_or((300, 300), |item| (120, 88 + item * 16));
    point(vm, at, 1_000_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    point(vm, at, 3_000_000);
}

/// Palette index of the primary's text plane at screen `(x, y)`, 0 where it is clear.
fn text_index(vm: &X68kVm, (x, y): (usize, usize)) -> u8 {
    let tvram = vm.bus.active_tvram().unwrap();
    let (row, plane) = (vm.bus.active_tvram_row_bytes(), vm.bus.active_tvram_plane_bytes());
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
                    0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1E, 0x1F, 0x20, 0x21,
                    0x22, 0x23, 0x24, 0x25, 0x26, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30,
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
    let data = volume.read_file(&[short_name("VMX68K", "")], &short_name(name, "V68"))?;
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
    let data = volume.read_file(&[short_name("VMX68K", "")], &short_name(name, "V68"))?;
    let setup = ResourceLibrary::parse(&data)?;
    let drives = &setup.get(b"V68D", 0)?.data;
    let word = |at: usize| u16::from_be_bytes([drives[at], drives[at + 1]]);
    let entries = (0..word(4) as usize)
        .map(|row| {
            let at = 6 + row * 96;
            let len = drives[at + 2] as usize;
            (word(at), String::from_utf8_lossy(&drives[at + 3..at + 3 + len]).into_owned())
        })
        .collect();
    Some((word(2) == 1, entries))
}

/// A 68000 setup with 4 MB.
fn setup(speed: u16, sram: bool) -> Option<Setup> {
    Some(Setup { speed, cpu: 0, ram_mb: 4, sram })
}

fn screenshot(vm: &X68kVm, name: &str) {
    let Ok(dir) = std::env::var("MSX68K_SCREENSHOT_DIR") else {
        return;
    };
    let (width, height) = vm.bus.crtc.active_resolution();
    let tvram = vm.bus.active_tvram().unwrap();
    let (row, plane) = (vm.bus.active_tvram_row_bytes(), vm.bus.active_tvram_plane_bytes());
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

#[test]
fn vmx68k_opens_a_vm_in_its_window() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };

    // A screen wide enough for the whole window, title bar and close box included.
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..5_000_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(3_000_000);

    // \X68 lists VMX68K.X after VMX68K.LB, as the first icon of the third row.
    click(&mut vm, DRIVE_ICON, 2, 3_000_000);
    screenshot(&vm, "x68");
    click(&mut vm, (104, 255), 2, 8_000_000);
    screenshot(&vm, "vmx68k");

    let events = vm.bus.take_vm_events();
    let [VmEvent::Opened { id: 1, window, drive0_bytes: 0, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    // The content is VMX68K.X's icon bar over the VM's 768x512 screen, at the window's bounds.
    // SX-Window's icon panel at the top left of the desktop covers a corner of it.
    let layout = vm.bus.vm_window_layout(window).expect("the window must be shown");
    assert_eq!((layout.left, layout.top), (40, 64));
    // Started without a setup, VMX68K.X makes the default one with what the VM starts with.
    assert_eq!(saved_setup(&vm, "DEFAULT"), setup(0, false), "10 MHz and no SRAM yet");

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
    vm.bus.set_vm_lamp(1, true);
    vm.run_steps(2_000_000);
    assert_eq!(text_index(&vm, LAMP), G_RED);
    vm.bus.set_vm_lamp(1, false);
    vm.run_steps(2_000_000);
    assert_eq!(text_index(&vm, LAMP), G_DGRAY);

    // The window follows the VM's screen when the VM changes its mode, the bar and the lamp
    // with it, the screen's top left staying where it was.
    for (width, height) in [(512, 512), (256, 256), (768, 512)] {
        vm.bus.set_vm_screen_size(1, width, height);
        vm.run_steps(4_000_000);
        screenshot(&vm, &format!("size_{width}x{height}"));
        let layout = vm.bus.vm_window_layout(window).expect("the window must be shown");
        assert_eq!((layout.left, layout.top), (40, 64), "{width}x{height}");
        let (right, bottom) = layout.visible.iter().fold((0, 0), |(r, b), v| (r.max(v.2), b.max(v.3)));
        assert_eq!((right, bottom), (40 + width as i32, 64 + height as i32), "{width}x{height}");
        let (width, height) = (width as usize, height as usize);
        assert_eq!(text_index(&vm, (40 + width - 12, 52)), G_DGRAY, "the lamp at {width}");
        assert_eq!(text_index(&vm, (40 + width - 1, 64 + height - 1)), 0, "the screen at {width}");
    }

    // The right button on the Configuration icon pops up its menu, and the CPU's item its
    // speeds; one chosen goes to the host at once. The submenu checks the current speed.
    choose_speed(&mut vm, Some(1), "speeds");
    assert_eq!(vm.bus.take_vm_events(), [VmEvent::Speed { id: 1, mode: CpuSpeedMode::Mhz16 }]);
    choose_speed(&mut vm, Some(3), "speeds_16mhz");
    assert_eq!(vm.bus.take_vm_events(), [VmEvent::Speed { id: 1, mode: CpuSpeedMode::Burst }]);
    assert_eq!(saved_setup(&vm, "DEFAULT"), setup(3, false), "a speed chosen is saved");

    // SRAM the VM changed is saved in its setup.
    vm.bus.set_vm_sram(1, vec![0x5A; msx68k_bus::vmx68k::VM_SRAM_BYTES]);
    vm.run_steps(3_000_000);
    assert_eq!(saved_setup(&vm, "DEFAULT"), setup(3, true));
    // Let go away from the menu, nothing is chosen.
    choose_speed(&mut vm, None, "speeds_burst");
    assert_eq!(vm.bus.take_vm_events(), []);

    // Dragging the title bar moves the content, with nothing reported by VMX68K.X.
    vm.bus.set_host_mouse_position(Some((400, 29)));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(200_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    vm.run_steps(300_000);
    for step in 1..=10 {
        vm.bus.set_host_mouse_position(Some((400 + step * 10, 29 + step * 10)));
        vm.bus.scc.add_mouse_delta(10, 10);
        vm.run_steps(100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(3_000_000);
    screenshot(&vm, "moved");
    let layout = vm.bus.vm_window_layout(window).expect("the window must be shown");
    assert_eq!((layout.left, layout.top), (140, 164));
    assert_eq!(layout.visible, [(140, 164, 908, 676)]);

    // The close box closes the VM.
    click(&mut vm, (897, 129), 1, 3_000_000);
    screenshot(&vm, "closed");
    assert_eq!(vm.bus.take_vm_events(), [VmEvent::Closed { id: 1 }]);
}

#[test]
fn a_setup_opens_vmx68k_with_its_settings() {
    // A setup in \X68, with the VM at Burst and its own SRAM.
    let sram: Vec<u8> = (0..msx68k_bus::vmx68k::VM_SRAM_BYTES).map(|i| (i * 7) as u8).collect();
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
    click(&mut vm, DRIVE_ICON, 2, 3_000_000);
    screenshot(&vm, "x68_setup");
    click(&mut vm, (104, 92), 2, 8_000_000);
    screenshot(&vm, "setup_opened");
    let events = vm.bus.take_vm_events();
    let [VmEvent::Opened { speed, sram: Some(opened), .. }] = &events[..] else {
        panic!("the setup must open one VM with its SRAM: {events:?}");
    };
    assert_eq!(*speed, CpuSpeedMode::Burst);
    assert!(opened == &sram, "the setup's own SRAM");
}

#[test]
fn vmx68k_runs_a_vm_in_each_of_its_windows() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..5_000_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(3_000_000);
    // VMX68K.X twice from \X68, the Filer's window brought forward in between.
    let mut windows = Vec::new();
    for n in 1..=2 {
        click(&mut vm, DRIVE_ICON, 2, 3_000_000);
        click(&mut vm, (104, 255), 2, 8_000_000);
        let events = vm.bus.take_vm_events();
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
            vm.bus.attach_vm_bridge(id, requests);
            std::thread::spawn(move || bridge.ask(msx68k_bus::vmx68k::BridgeOp::Dskfre { drive: 1 }))
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
    vm.bus.set_vm_lamp(2, true);
    vm.run_steps(3_000_000);
    assert_eq!(lamp(&vm), 13, "VM 2's lamp lit");
    vm.bus.set_vm_lamp(1, true);
    vm.bus.set_vm_lamp(2, false);
    vm.run_steps(3_000_000);
    assert_eq!(lamp(&vm), 10, "VM 1's lamp is not VM 2's");
}

#[test]
fn a_window_the_screen_cut_off_is_drawn_when_the_screen_grows() {
    // At 768x512 the window, 576 lines down to its content's bottom, runs off the screen.
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    assert_eq!(vm.bus.crtc.active_resolution(), (768, 512));
    click(&mut vm, DRIVE_ICON, 2, 3_000_000);
    click(&mut vm, (104, 255), 2, 8_000_000);
    let events = vm.bus.take_vm_events();
    let [VmEvent::Opened { window, .. }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..5_000_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(5_000_000);
    screenshot(&vm, "grown");
    // What was off the screen is clear for the VM's screen, its visible region reaches it,
    // and the window's frame is there.
    for at in [(400, 520), (400, 575), (800, 300), (800, 575)] {
        assert_eq!(text_index(&vm, at), 0, "{at:?}");
    }
    let layout = vm.bus.vm_window_layout(window).unwrap();
    let (right, bottom) = layout.visible.iter().fold((0, 0), |(r, b), v| (r.max(v.2), b.max(v.3)));
    assert_eq!((right, bottom), (808, 576));
    assert_ne!(text_index(&vm, (400, 576)), 0, "the bottom of the frame");
}

#[test]
fn the_configuration_dialog_sets_the_cpu_and_memory_and_restarts_the_vm() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the VMX68K test");
        return;
    };
    vm.bus.request_screen_resolution(1024, 768);
    for _ in 0..5_000_000 {
        if vm.bus.crtc.active_resolution() == (1024, 768) {
            break;
        }
        vm.step();
    }
    vm.run_steps(3_000_000);
    click(&mut vm, DRIVE_ICON, 2, 3_000_000);
    click(&mut vm, (104, 255), 2, 8_000_000);
    let events = vm.bus.take_vm_events();
    let [VmEvent::Opened { id: 1, cpu: CpuModel::Mc68000, ram_mb: 4, .. }] = events[..] else {
        panic!("VMX68K.X must open a 68000 VM with 4 MB: {events:?}");
    };
    let point = |vm: &mut X68kVm, at: (i16, i16), steps: usize| {
        vm.bus.set_host_mouse_position(Some(at));
        vm.bus.scc.add_mouse_delta(1, 1);
        vm.run_steps(steps);
    };

    // 設定..., the Configuration menu's first item, opens the dialog.
    point(&mut vm, (56, 52), 200_000);
    vm.bus.scc.set_mouse_buttons(false, true);
    point(&mut vm, (56, 52), 1_000_000);
    point(&mut vm, (90, 70), 1_000_000);
    vm.bus.scc.set_mouse_buttons(false, false);
    point(&mut vm, (90, 70), 6_000_000);
    screenshot(&vm, "dialog");
    assert_ne!(text_index(&vm, (300, 200)), 0, "the dialog over the VM's screen");

    // The 68030, and the slider's knob dragged from 4 MB to 8 MB.
    click(&mut vm, (420, 216), 1, 2_000_000);
    point(&mut vm, (361, 244), 300_000);
    vm.bus.scc.set_mouse_buttons(true, false);
    point(&mut vm, (361, 244), 300_000);
    for x in (361..=408).step_by(4) {
        point(&mut vm, (x, 244), 100_000);
    }
    vm.bus.scc.set_mouse_buttons(false, false);
    point(&mut vm, (408, 244), 1_000_000);
    screenshot(&vm, "dialog_set");

    // カスタム drives: 詳細... gives three, an image file, a shared drive, and a passthrough one.
    click(&mut vm, (340, 288), 1, 2_000_000);
    click(&mut vm, (439, 289), 1, 6_000_000);
    screenshot(&vm, "dialog_drives");
    click(&mut vm, (275, 156), 1, 2_000_000);
    click(&mut vm, (275, 156), 1, 2_000_000);
    click(&mut vm, (290, 222), 1, 2_000_000);
    click(&mut vm, (360, 242), 1, 2_000_000);
    click(&mut vm, (500, 202), 1, 2_000_000);
    type_keys(&mut vm, "a:\\vmx68k\\game.xdf");
    click(&mut vm, (500, 222), 1, 2_000_000);
    type_keys(&mut vm, "c:");
    click(&mut vm, (500, 242), 1, 2_000_000);
    type_keys(&mut vm, "b:");
    screenshot(&vm, "dialog_drives_set");
    click(&mut vm, (529, 374), 1, 4_000_000);
    screenshot(&vm, "dialog_drives_kept");
    assert_eq!(saved_drives(&vm, "DEFAULT").map(|(custom, _)| custom), Some(false), "kept, not saved yet");

    // 設定 saves the setup and asks whether to restart the VM now: はい restarts it.
    click(&mut vm, (424, 317), 1, 4_000_000);
    screenshot(&vm, "dialog_restart");
    assert_eq!(vm.bus.take_vm_events(), [], "not before the answer");
    assert_eq!(saved_setup(&vm, "DEFAULT"), Some(Setup { speed: 0, cpu: 1, ram_mb: 8, sram: false }));
    assert_eq!(
        saved_drives(&vm, "DEFAULT"),
        Some((
            true,
            vec![(0, "a:\\vmx68k\\game.xdf".into()), (1, "c:".into()), (2, "b:".into())]
        ))
    );
    click(&mut vm, (610, 198), 1, 4_000_000);
    assert_eq!(
        vm.bus.take_vm_events(),
        [VmEvent::Restart { id: 1, cpu: CpuModel::Mc68030, ram_mb: 8, speed: CpuSpeedMode::Mhz25, custom: Some(vec![
            msx68k_bus::vmx68k::CustomDrive::Missing,
            msx68k_bus::vmx68k::CustomDrive::Shared { drive: 3 },
            msx68k_bus::vmx68k::CustomDrive::Passthrough { drive: 2 },
        ]) }]
    );
    assert_eq!(text_index(&vm, (300, 200)), 0, "the VM's screen back where the dialog was");

    // The menu now has the 68030's speeds, 25MHz and 最高速, the second Burst.
    choose_speed(&mut vm, Some(1), "speeds_68030");
    assert_eq!(vm.bus.take_vm_events(), [VmEvent::Speed { id: 1, mode: CpuSpeedMode::Burst }]);
}
