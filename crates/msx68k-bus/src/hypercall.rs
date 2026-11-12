//! The native hypercall gateway (`docs/HOOK_AND_SCSI_ARCHITECTURE.md` Section 2.1).
//!
//! Every hypercall is the one F-Line opcode [`HYPERCALL_OPCODE`], and the address it is executed
//! at decides which hypercall it is. Whoever installs a hypercall, as a patch over guest code or
//! a stub in a ROM msx68k provides, registers that address. The opcode anywhere else is an
//! ordinary F-Line exception, as on the real machine.

use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;

/// The only opcode msx68k takes from the F-Line space. Changing it here changes every
/// hypercall.
pub const HYPERCALL_OPCODE: u16 = 0xFBFF;

/// The entry for msx68k's own applications, at the end of the internal SCSI ROM window
/// (`$FC0000`–`$FDFFFF`) and outside the SCSI ROM itself. It is the only part of the window
/// that can be read when no SCSI ROM is emulated.
pub const APP_ENTRY_BASE: u32 = 0x00FD_FFF0;
/// Signature at [`APP_ENTRY_BASE`], followed by [`APP_ENTRY_VERSION`].
pub const APP_ENTRY_SIGNATURE: [u8; 6] = *b"MSX68K";
/// Version word after the signature.
pub const APP_ENTRY_VERSION: u16 = 1;
/// Where applications call the entry with `JSR`, the function number in `D0.L`.
pub const APP_ENTRY: u32 = APP_ENTRY_BASE + 8;

/// What a registered hypercall address stands for.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum HypercallId {
    /// Synchronize SX-Window mouse coordinates (`A4` = `MouseWorkArea` pointer) with host mouse position.
    SxMouseSync,
    /// Boot from attached SCSI HDD (`$FC0000`).
    ScsiRomBoot,
    /// `HUMAN.SYS` `romdb` block driver builder (`JSR (A3)` at `*(P-16)`).
    ScsiRomdbBuild,
    /// `IOCS $F5` vector installer (`JSR (A0)` at `*(P-12)`).
    ScsiIocsInstall,
    /// `IOCS $F5` (`_SCSIDRV`) subfunction dispatcher.
    ScsiIocsDispatch,
    /// `romdb` block driver Strategy routine (`A5` = request packet).
    ScsiDriverStrategy,
    /// `romdb` block driver Interrupt routine (`A5` = request packet).
    ScsiDriverInterrupt,
    /// Native replacement of the `SYS/FSX.X` text span routine, patched over its first
    /// instruction.
    GmTextSpan,
    /// Native replacement of the `SYS/FSX.X` pen span routine, patched over its first
    /// instruction.
    GmPenSpan,
    /// The 48-byte row copy of the `SYS/FSX.X` system error alert box.
    AlertRowCopy,
    /// The 6-byte icon row copy of the alert box.
    AlertIconCopy,
    /// The glyph store of the alert box.
    AlertGlyphOr,
    /// The application entry at [`APP_ENTRY`], whose function `D0.L` selects.
    AppEntry,
    /// A VM's boot ROM for its remote drives: IPL boot, driver install, and the remote
    /// device's strategy and interrupt routines (see [`crate::remote_fs`]).
    RemoteBoot,
    RemoteInstall,
    RemoteStrategy,
    RemoteInterrupt,
}

/// Functions of the application entry, selected by `D0.L`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u32)]
pub enum AppFunction {
    /// Returns [`APP_ENTRY_VERSION`] in `D0.L`.
    Version = 0,
    /// Returns the mouse wheel settings: `D1` reverse, `D2` clicks per notch, `D3` trackpad
    /// pixels per notch.
    GetWheelConfig = 1,
    /// Sets the mouse wheel settings from `D1`..`D3` and returns them as clamped.
    SetWheelConfig = 2,
    /// Queries (`D1.L = -1`) or switches the SX-Window display resolution.
    ScreenMode = 3,
    /// Starts a VM for the window `D1.L` and returns its ID (`docs/VMX68K.md` Section 3.5).
    VmOpen = 4,
    /// Carries out the pending requests of VM `D1.L`, on every pass of VMX68K.SX's event loop.
    VmPoll = 5,
    /// Stops VM `D1.L`.
    VmClose = 6,
    /// Answers the disk request `VmPoll` handed out for VM `D1.L`.
    VmReply = 7,
    /// Sets the speed of VM `D1.L` from VMX68K.SX's menu.
    VmSpeed = 8,
}

impl AppFunction {
    fn from_d0(d0: u32) -> Option<Self> {
        match d0 {
            0 => Some(Self::Version),
            1 => Some(Self::GetWheelConfig),
            2 => Some(Self::SetWheelConfig),
            3 => Some(Self::ScreenMode),
            4 => Some(Self::VmOpen),
            5 => Some(Self::VmPoll),
            6 => Some(Self::VmClose),
            7 => Some(Self::VmReply),
            8 => Some(Self::VmSpeed),
            _ => None,
        }
    }
}

impl X68kBus {
    /// Registers the hypercall at `addr`, where its opcode has been or is about to be placed.
    pub fn register_hypercall(&mut self, addr: u32, id: HypercallId) {
        self.hypercall_sites.insert(addr & 0x00FF_FFFF, id);
    }

    /// Returns the hypercall registered at `addr`, if any.
    pub fn hypercall_at(&self, addr: u32) -> Option<HypercallId> {
        self.hypercall_sites.get(&(addr & 0x00FF_FFFF)).copied()
    }

    /// Forgets the hypercalls patched into RAM, whose code a reset leaves to be replaced.
    pub(crate) fn forget_ram_hypercalls(&mut self) {
        self.hypercall_sites.retain(|&addr, _| addr >= 0x00F0_0000);
    }

    /// Places the application entry at [`APP_ENTRY_BASE`], as the master SX-Window VM has it.
    pub fn install_app_entry(&mut self) {
        let base = (APP_ENTRY_BASE - 0x00F0_0000) as usize;
        let mut entry = [0u8; 16];
        entry[..6].copy_from_slice(&APP_ENTRY_SIGNATURE);
        entry[6..8].copy_from_slice(&APP_ENTRY_VERSION.to_be_bytes());
        entry[8..10].copy_from_slice(&HYPERCALL_OPCODE.to_be_bytes());
        entry[10..12].copy_from_slice(&[0x4E, 0x75]); // RTS
        self.rom[base..base + 16].copy_from_slice(&entry);
        self.app_entry_installed = true;
        self.register_hypercall(APP_ENTRY, HypercallId::AppEntry);
    }

    /// Returns `true` if `addr` lies in the internal SCSI ROM window where nothing is mapped.
    pub(crate) fn in_absent_scsi_rom_window(&self, addr: u32) -> bool {
        let mapped = self.scsi.scsi_rom_enabled
            || self.remote_boot_rom
            || (self.app_entry_installed && addr >= APP_ENTRY_BASE);
        (0x00FC_0000..0x00FE_0000).contains(&addr) && !mapped
    }

    /// Carries out the hypercall registered at `pc`; any other F-Line instruction goes to the
    /// guest's Line 1111 Emulator vector.
    pub fn handle_hypercall(&mut self, state: &mut CpuState, opcode: u16, pc: u32) -> TrapResult {
        if opcode != HYPERCALL_OPCODE {
            return TrapResult::Unhandled;
        }
        let Some(hypercall) = self.hypercall_at(pc) else {
            return TrapResult::Unhandled;
        };

        match hypercall {
            HypercallId::SxMouseSync => self.hypercall_sx_mouse_sync(state),
            HypercallId::ScsiRomBoot => self.hypercall_scsi_rom_boot(state),
            HypercallId::ScsiRomdbBuild => self.hypercall_scsi_romdb_build(state),
            HypercallId::ScsiIocsInstall => self.hypercall_scsi_iocs_install(state),
            HypercallId::ScsiIocsDispatch => self.hypercall_scsi_iocs_dispatch(state),
            HypercallId::ScsiDriverStrategy => self.hypercall_scsi_driver_strategy(state),
            HypercallId::ScsiDriverInterrupt => self.hypercall_scsi_driver_interrupt(state),
            HypercallId::GmTextSpan => self.hypercall_text_span(state),
            HypercallId::GmPenSpan => self.hypercall_pen_span(state),
            HypercallId::AlertRowCopy => self.hypercall_alert_row_copy(state),
            HypercallId::AlertIconCopy => self.hypercall_alert_icon_copy(state),
            HypercallId::AlertGlyphOr => self.hypercall_alert_glyph_or(state),
            HypercallId::RemoteBoot => self.hypercall_remote_boot(state),
            HypercallId::RemoteInstall => self.hypercall_remote_install(state),
            HypercallId::RemoteStrategy => self.hypercall_remote_strategy(state),
            HypercallId::RemoteInterrupt => self.hypercall_remote_interrupt(state),
            HypercallId::AppEntry => match AppFunction::from_d0(state.d[0]) {
                Some(AppFunction::Version) => {
                    state.d[0] = APP_ENTRY_VERSION as u32;
                    TrapResult::Handled { extra_cycles: 0 }
                }
                Some(AppFunction::GetWheelConfig) => self.hypercall_wheel_config(state, false),
                Some(AppFunction::SetWheelConfig) => self.hypercall_wheel_config(state, true),
                Some(AppFunction::ScreenMode) => self.hypercall_sx_set_screen_mode(state),
                Some(AppFunction::VmOpen) => self.hypercall_vm_open(state),
                Some(AppFunction::VmPoll) => self.hypercall_vm_poll(state),
                Some(AppFunction::VmClose) => self.hypercall_vm_close(state),
                Some(AppFunction::VmReply) => self.hypercall_vm_reply(state),
                Some(AppFunction::VmSpeed) => self.hypercall_vm_speed(state),
                None => TrapResult::Unhandled,
            },
        }
    }
}
