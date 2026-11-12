//! Hypercalls are one F-Line opcode recognized by the address it runs at, and msx68k's own
//! applications reach the host through an entry at the end of the internal SCSI ROM window
//! (`docs/HOOK_AND_SCSI_ARCHITECTURE.md` Section 2.1).

use std::path::Path;

use msx68k_bus::{
    AppFunction, Bus, HypercallId, VmConfig, X68kVm, APP_ENTRY, APP_ENTRY_BASE, HYPERCALL_OPCODE,
};

const CODE: u32 = 0x0001_0000;
const LINE_F_HANDLER: u32 = 0x0002_0000;

/// A VM in supervisor mode whose Line 1111 Emulator vector points at [`LINE_F_HANDLER`].
fn vm(config: VmConfig) -> X68kVm {
    let mut vm = X68kVm::new(config);
    vm.bus.write_u32(0x2C, LINE_F_HANDLER, true).unwrap();
    vm.cpu_state.a[7] = 0x0000_8000;
    vm.cpu_state.sr = 0x2700;
    vm
}

fn call(vm: &mut X68kVm, target: u32) {
    // JSR target.L, then the code continues at CODE + 6.
    vm.bus.write_u16(CODE, 0x4EB9, true).unwrap();
    vm.bus.write_u32(CODE + 2, target, true).unwrap();
    vm.cpu_state.pc = CODE;
    vm.step();
}

#[test]
fn the_opcode_elsewhere_is_an_ordinary_line_f_exception() {
    let mut vm = vm(VmConfig::master_sx());
    vm.bus.write_u16(CODE, HYPERCALL_OPCODE, true).unwrap();
    vm.cpu_state.pc = CODE;
    vm.step();
    assert_eq!(vm.cpu_state.pc, LINE_F_HANDLER);
}

#[test]
fn a_registered_address_runs_its_hypercall() {
    let mut vm = vm(VmConfig::master_sx());
    call(&mut vm, APP_ENTRY);
    vm.cpu_state.d[0] = AppFunction::Version as u32;
    vm.step(); // The hypercall
    vm.step(); // RTS
    assert_eq!(vm.cpu_state.pc, CODE + 6);
    assert_eq!(vm.cpu_state.d[0], 1);
}

#[test]
fn an_unknown_function_is_a_line_f_exception() {
    let mut vm = vm(VmConfig::master_sx());
    call(&mut vm, APP_ENTRY);
    vm.cpu_state.d[0] = 0x4D58_0000;
    vm.step();
    assert_eq!(vm.cpu_state.pc, LINE_F_HANDLER);
}

#[test]
fn only_the_master_vm_has_the_entry_and_only_the_entry_shows() {
    let mut master = vm(VmConfig::master_sx());
    let signature: Vec<u8> = (0..8)
        .map(|i| master.bus.read_u8(APP_ENTRY_BASE + i, true).unwrap())
        .collect();
    assert_eq!(signature, b"MSX68K\x00\x01");
    // With no SCSI ROM, nothing else in the window answers, so msx68k is not taken for a
    // machine with internal SCSI.
    assert!(master.bus.read_u8(APP_ENTRY_BASE - 1, true).is_err());
    assert!(master.bus.read_u8(0x00FC_0024, true).is_err());
    // User mode cannot reach it, as with the rest of the ROM.
    assert!(master.bus.read_u8(APP_ENTRY_BASE, false).is_err());

    let mut app = vm(VmConfig::app_vm());
    assert!(app.bus.read_u8(APP_ENTRY_BASE, true).is_err());
    assert_eq!(app.bus.hypercall_at(APP_ENTRY), None);
}

#[test]
fn a_reset_forgets_hypercalls_patched_into_ram() {
    let mut vm = vm(VmConfig::master_sx());
    vm.bus.register_hypercall(CODE, HypercallId::GmTextSpan);
    vm.bus.soft_reset();
    assert_eq!(vm.bus.hypercall_at(CODE), None);
    assert_eq!(vm.bus.hypercall_at(APP_ENTRY), Some(HypercallId::AppEntry));
}

#[test]
fn the_guest_include_matches_the_host() {
    let mac = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guest/include/msx68k.mac"),
    )
    .unwrap();
    let value = |name: &str| -> u32 {
        let line = mac
            .lines()
            .find(|line| line.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("{name} must be defined"));
        let field = line.split_whitespace().nth(2).unwrap();
        match field.strip_prefix('$') {
            Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
            None => field.parse().unwrap(),
        }
    };
    assert_eq!(value("MSX68K_ENTRY_BASE"), APP_ENTRY_BASE);
    assert_eq!(value("MSX68K_ENTRY"), APP_ENTRY);
    assert_eq!(value("MSX68K_VERSION"), msx68k_bus::hypercall::APP_ENTRY_VERSION as u32);
    for (name, function) in [
        ("MSX68K_FN_VERSION", AppFunction::Version),
        ("MSX68K_FN_GET_WHEEL", AppFunction::GetWheelConfig),
        ("MSX68K_FN_SET_WHEEL", AppFunction::SetWheelConfig),
        ("MSX68K_FN_SCREEN_MODE", AppFunction::ScreenMode),
        ("MSX68K_FN_VM_OPEN", AppFunction::VmOpen),
        ("MSX68K_FN_VM_POLL", AppFunction::VmPoll),
        ("MSX68K_FN_VM_CLOSE", AppFunction::VmClose),
        ("MSX68K_FN_VM_REPLY", AppFunction::VmReply),
        ("MSX68K_FN_VM_SPEED", AppFunction::VmSpeed),
        ("MSX68K_FN_VM_STATUS", AppFunction::VmStatus),
    ] {
        assert_eq!(value(name), function as u32, "{name}");
    }
}
