//! VMX68K.X, in `\X68` of the built disk, opens a window and asks the host for a VM to show in it (`docs/VMX68K.md`
//! Section 3.5).

use std::path::Path;

use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;
use msx68k_bus::{CpuSpeedMode, VmConfig, VmEvent, X68kVm};

/// Hard disk icon on the desktop of the test disk, which opens `\X68`.
const DRIVE_ICON: (i16, i16) = (22, 340);

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn boot() -> Option<X68kVm> {
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
        VmConfig::master_sx()
            .with_speed_mode(CpuSpeedMode::Burst)
            .with_ram_mb(4),
    );
    vm.bus.load_cgrom(&cgrom);
    vm.bus.load_ipl_rom(&ipl);
    assert!(vm.bus.insert_scsi_hdd(
        0,
        build_bootable_sxwin_scsi_hdd(&human, &sx_refs, 32)
    ));
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

    // \X68 lists VMX68K.X after HDフォーマット.X, as the fourth icon of the second row.
    click(&mut vm, DRIVE_ICON, 2, 3_000_000);
    screenshot(&vm, "x68");
    click(&mut vm, (343, 175), 2, 8_000_000);
    screenshot(&vm, "vmx68k");

    let events = vm.bus.take_vm_events();
    let [VmEvent::Opened { id: 1, window }] = events[..] else {
        panic!("VMX68K.X must open one VM: {events:?}");
    };
    // The content is the VM's 768x512 screen at the window's bounds. SX-Window's icon panel
    // at the top left of the desktop covers a corner of it.
    let layout = vm.bus.vm_window_layout(window).expect("the window must be shown");
    assert_eq!((layout.left, layout.top), (40, 40));
    assert_eq!(layout.visible, [(45, 40, 808, 93), (40, 93, 808, 552)]);

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
    assert_eq!((layout.left, layout.top), (140, 140));
    assert_eq!(layout.visible, [(140, 140, 908, 652)]);

    // The close box closes the VM.
    click(&mut vm, (897, 129), 1, 3_000_000);
    screenshot(&vm, "closed");
    assert_eq!(vm.bus.take_vm_events(), [VmEvent::Closed { id: 1 }]);
}
