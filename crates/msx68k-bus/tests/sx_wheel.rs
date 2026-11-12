//! Mouse wheel notches click the scroll bar arrows of SX-Window's active window.

use std::path::Path;

use msx68k_bus::scsi::build_bootable_sxwin_scsi_hdd;
use msx68k_bus::{Bus, CpuSpeedMode, VmConfig, X68kVm};

/// `SYS/FSX.X` global holding the active window record.
const ACTIVE_WINDOW: u32 = 0x543CA;
/// Hard disk icon on the desktop of the test disk.
const DRIVE_ICON: (i16, i16) = (22, 340);

fn boot() -> Option<X68kVm> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |name: &str| std::fs::read(root.join(name)).ok();
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
    assert!(vm.bus.insert_scsi_hdd(0, build_bootable_sxwin_scsi_hdd(&human, &sx_refs, 32)));
    vm.bus.set_boot_from_scsi(true);
    vm.reset().unwrap();
    vm.run_steps(30_000_000);
    Some(vm)
}

fn text_base(vm: &mut X68kVm) -> u32 {
    vm.bus.read_u32(0x938, true).unwrap() - 0x04F82A
}

fn set_buttons(vm: &mut X68kVm, left: bool) {
    vm.bus.scc.set_mouse_buttons(left, false);
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(100_000);
}

/// Returns the values of the active window's controls, in list order.
fn control_values(vm: &mut X68kVm) -> Vec<i16> {
    let base = text_base(vm);
    let window = vm.bus.read_u32(base + ACTIVE_WINDOW, true).unwrap();
    let mut values = Vec::new();
    let mut next = vm.bus.read_u32(window + 98, true).unwrap();
    while next != 0 {
        let control = vm.bus.read_u32(next, true).unwrap();
        if control == 0 {
            break;
        }
        values.push(vm.bus.read_u16(control + 18, true).unwrap() as i16);
        next = vm.bus.read_u32(control, true).unwrap();
    }
    values
}

fn wheel(vm: &mut X68kVm, notches: i32, horizontal: bool) {
    vm.bus.post_mouse_wheel(notches, horizontal);
    vm.run_steps(1_000_000);
}

#[test]
fn wheel_scrolls_the_active_window_and_skips_windows_without_scroll_bars() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the wheel test");
        return;
    };

    // The desktop has no scroll bars, so these notches are dropped rather than kept.
    assert!(control_values(&mut vm).is_empty());
    wheel(&mut vm, 5, false);

    // Open the drive window, which has a horizontal and a vertical scroll bar.
    vm.bus.set_host_mouse_position(Some(DRIVE_ICON));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(200_000);
    for _ in 0..2 {
        set_buttons(&mut vm, true);
        set_buttons(&mut vm, false);
    }
    vm.run_steps(3_000_000);
    assert_eq!(control_values(&mut vm), [0, 0], "horizontal and vertical bars at rest");

    // Each notch is one arrow click: 32 dots of the icon view.
    wheel(&mut vm, 3, false);
    assert_eq!(control_values(&mut vm), [0, 96]);
    wheel(&mut vm, -1, false);
    assert_eq!(control_values(&mut vm), [0, 64]);

    // Shift turns the wheel to the horizontal bar, here as far left as it goes.
    wheel(&mut vm, -2, true);
    assert_eq!(control_values(&mut vm), [-10, 64]);
}

fn double_click(vm: &mut X68kVm, at: (i16, i16), settle: usize) {
    vm.bus.set_host_mouse_position(Some(at));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(200_000);
    for _ in 0..2 {
        set_buttons(vm, true);
        set_buttons(vm, false);
    }
    vm.run_steps(settle);
}

#[test]
fn wheel_scrolls_general_scroll_bars() {
    let Some(mut vm) = boot() else {
        eprintln!("ROMs or disks not found, skipping the wheel test");
        return;
    };

    // Open \X68, go up to the root, open シャーペン, and run シャーペン.X, whose window
    // scroll bars are general ones (CDEF 66 and 67), as are those of lists in its dialogs.
    double_click(&mut vm, DRIVE_ICON, 3_000_000);
    vm.bus.set_host_mouse_position(Some((398, 36)));
    vm.bus.scc.add_mouse_delta(1, 1);
    vm.run_steps(200_000);
    set_buttons(&mut vm, true);
    set_buttons(&mut vm, false);
    vm.run_steps(3_000_000);
    double_click(&mut vm, (376, 285), 4_000_000);
    double_click(&mut vm, (327, 235), 15_000_000);
    // Two hidden buttons, then the horizontal and the vertical bar, all at rest.
    assert_eq!(control_values(&mut vm), [0, 0, 0, 0]);

    // An empty document has nothing to scroll vertically, but Shift scrolls it sideways.
    wheel(&mut vm, 2, true);
    let values = control_values(&mut vm);
    assert!(values[2] > 0, "the horizontal bar must move: {values:?}");
}
