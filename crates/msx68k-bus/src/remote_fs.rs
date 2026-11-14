//! File-level remote drives (`docs/VMX68K.md` Section 2): in shared mode, the
//! primary's drives under the same letters, booted from; in custom mode, selected
//! primary drives appended after the VM's own floppy and SCSI drives.
//!
//! The VM boots through a ROM msx68k provides at `$FC0000`, laid out like the internal SCSI ROM
//! that Human68k already knows how to boot from: the IPL boot routine loads `HUMAN.SYS` from
//! the primary's `A:` through the [`VmBridge`], and the driver install routine builds a remote
//! device (type `$2000`). Human68k then hands the device file-level commands (`_NAMESTS` names,
//! FCBs), which become the primary's DOS calls, carried out by VMX68K.SX. Until the VM's shell
//! starts, its `\CONFIG.SYS` is the primary's `\VMX68K\CONFIG.SYS`, so that it does not start
//! SX-Window again.

use std::collections::HashMap;

use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;
use crate::hypercall::{HypercallId, HYPERCALL_OPCODE};
use crate::vmx68k::{BridgeOp, BridgeReply, VmBridge};

/// Backend interface for servicing remote drive filesystem operations.
pub trait RemoteBridge: Send + Sync + std::fmt::Debug {
    fn ask(&self, op: BridgeOp) -> Option<BridgeReply>;
}

impl RemoteBridge for VmBridge {
    fn ask(&self, op: BridgeOp) -> Option<BridgeReply> {
        self.ask(op)
    }
}

/// Where the boot ROM lives in the VM, the internal SCSI ROM window.
const ROM_BASE: u32 = 0x00FC_0000;
/// IPL boot routine, which the handle at [`ROM_BASE`] points to.
const BOOT_ROUTINE: u32 = ROM_BASE + 0x14;
/// Driver install routine, found by Human68k at the IPL boot routine's address - 16.
const INSTALL_ROUTINE: u32 = ROM_BASE + 0x18;
const STRATEGY_ROUTINE: u32 = ROM_BASE + 0x1C;
const INTERRUPT_ROUTINE: u32 = ROM_BASE + 0x20;
// Custom mode shares the SCSI ROM, leaving its header and entry points intact.
const CUSTOM_STRATEGY: u32 = ROM_BASE + 0x100;
const CUSTOM_INTERRUPT: u32 = ROM_BASE + 0x104;
const CUSTOM_INSTALL: u32 = ROM_BASE + 0x108;
// Human68k installs the boot interface before its built-in floppy driver and
// other ROM interfaces afterwards. Give HDD boot a separate ROM anchor so the
// normal SCSI ROM interface can install custom shares after the physical disks.
const CUSTOM_BOOT_HEADER: u32 = ROM_BASE + 0x200;

/// Where `HUMAN.SYS` runs, its base address and entry.
const HUMAN_BASE: u32 = 0x6800;

/// Bytes VMX68K.SX can move at once, its buffer.
const CHUNK: u32 = 32 * 1024;
const SEARCH_MAGIC: u32 = 0x4D53_5846; // MSXF, in the guest's eight search-work bytes.

// Human68k DOS errors.
const DOS_FILE_NOT_FOUND: u32 = -2i32 as u32;
const DOS_DIRECTORY_NOT_FOUND: u32 = -3i32 as u32;
const DOS_INVALID_ACCESS: u32 = -12i32 as u32;
const DOS_CANNOT_WRITE: u32 = -19i32 as u32;
const DOS_SEEK_ERROR: u32 = -25i32 as u32;

// Device errors in the request header: abort, and the command or unit is unknown.
const DEV_ABORT_UNKNOWN_COMMAND: u16 = 0x1003;

/// The VM's `CONFIG.SYS` when the primary has no `\VMX68K\CONFIG.SYS`: Human68k's own settings,
/// with no SX-Window and `COMMAND.X` as the shell.
const DEFAULT_CONFIG: &[u8] = b"FILES     = 64\r\nBUFFERS   = 20 1024\r\nLASTDRIVE = Z:\r\n";

/// What an FCB of the VM stands for.
#[derive(Debug)]
enum OpenFile {
    /// A file the primary has open, by its handle.
    Primary(u32),
    /// A file the host serves from memory.
    Memory(Vec<u8>),
}

/// The VM's remote drives.
#[derive(Debug)]
pub struct RemoteFs {
    bridge: Box<dyn RemoteBridge>,
    /// The primary's drive number (1 is A:) of each unit.
    drives: Vec<u32>,
    /// Device header built by the install routine, and the request the strategy routine got.
    header: u32,
    request: u32,
    files: HashMap<u32, OpenFile>,
    /// The primary's search buffer behind each guest search ID (buffers can move).
    searches: HashMap<u32, Vec<u8>>,
    next_search_id: u32,
    /// Shared mode boots from these drives; custom mode adds them without booting.
    boot: bool,
    /// Whether `\CONFIG.SYS` is still the VM's own, and the shell whose start ends that.
    own_config: bool,
    shell: Vec<u8>,
}

impl RemoteFs {
    /// Remote drives for the primary's drives `drives` (1 is A:), the first one booted from.
    pub fn new<B: RemoteBridge + 'static>(bridge: B, drives: Vec<u32>) -> Self {
        Self::with_boxed_bridge(Box::new(bridge), drives)
    }

    /// Remote drives using a pre-boxed bridge implementation.
    pub fn with_boxed_bridge(bridge: Box<dyn RemoteBridge>, drives: Vec<u32>) -> Self {
        Self {
            bridge,
            drives,
            header: 0,
            request: 0,
            files: HashMap::new(),
            searches: HashMap::new(),
            next_search_id: 1,
            boot: true,
            own_config: true,
            shell: b"COMMAND.X".to_vec(),
        }
    }
}

/// A `_NAMESTS` name: drive number, path with `\` separators, and file name.
struct Namests {
    path: Vec<u8>,
    name: Vec<u8>,
}

impl Namests {
    /// The primary's path for it on drive `drive` (1 is A:).
    fn primary_path(&self, drive: u32) -> Vec<u8> {
        let mut out = vec![b'@' + drive as u8, b':'];
        out.extend_from_slice(&self.path);
        out.extend_from_slice(&self.name);
        out
    }
}

impl X68kBus {
    /// Makes the VM boot from the remote drive `fs.drives[0]` and gives it the drives. The VM's
    /// own floppy drives, which it does not use so, are disconnected, so that its Human68k gives
    /// them no letters after the remote drives'.
    pub fn install_remote_boot(&mut self, fs: RemoteFs) {
        for drive in &mut self.fdc.drives {
            *drive = crate::fdc::FloppyDrive::new(false);
        }
        let rom = (ROM_BASE - 0x00F0_0000) as usize;
        let [hi, lo] = HYPERCALL_OPCODE.to_be_bytes();
        let mut image = Vec::new();
        image.extend_from_slice(&BOOT_ROUTINE.to_be_bytes()); // IPL boot handle
        image.extend_from_slice(&INSTALL_ROUTINE.to_be_bytes()); // install handle
        image.extend_from_slice(&0u32.to_be_bytes()); // install parameter
        image.extend_from_slice(b"Human68k");
        for _ in [
            BOOT_ROUTINE,
            INSTALL_ROUTINE,
            STRATEGY_ROUTINE,
            INTERRUPT_ROUTINE,
        ] {
            image.extend_from_slice(&[hi, lo, 0x4E, 0x75]); // hypercall; RTS
        }
        self.rom[rom..rom + image.len()].copy_from_slice(&image);
        for (addr, id) in [
            (BOOT_ROUTINE, HypercallId::RemoteBoot),
            (INSTALL_ROUTINE, HypercallId::RemoteInstall),
            (STRATEGY_ROUTINE, HypercallId::RemoteStrategy),
            (INTERRUPT_ROUTINE, HypercallId::RemoteInterrupt),
        ] {
            self.register_hypercall(addr, id);
        }
        self.remote_boot_rom = true;
        // Boot from the ROM at $FC0000, as SWITCH BOOT=ROM$FC0000 would.
        self.sram[0x0C..0x10].copy_from_slice(&ROM_BASE.to_be_bytes());
        self.sram[0x18..0x1A].copy_from_slice(&0x0001u16.to_be_bytes());
        self.remote_fs = Some(Box::new(fs));
    }

    /// Adds file-level remote drives to a custom VM without replacing its boot ROM,
    /// changing SRAM, or disconnecting its physical floppy units. Human68k installs
    /// this device through the SCSI ROM's driver enumeration.
    pub fn install_remote_drives(&mut self, bridge: VmBridge, drives: Vec<u32>) {
        self.install_remote_drives_with_bridge(Box::new(bridge), drives);
    }

    /// Installs remote drives using any implementation of [`RemoteBridge`].
    pub fn install_remote_drives_with_bridge(
        &mut self,
        bridge: Box<dyn RemoteBridge>,
        drives: Vec<u32>,
    ) {
        let mut fs = RemoteFs::with_boxed_bridge(bridge, drives);
        fs.boot = false;
        fs.own_config = false;
        self.remote_fs = Some(Box::new(fs));
        self.enable_scsi_rom();
        let base = (ROM_BASE - 0x00F0_0000) as usize;
        self.rom[base + 0x40..base + 0x44].copy_from_slice(&CUSTOM_INSTALL.to_be_bytes());
        let header = (CUSTOM_BOOT_HEADER - 0x00F0_0000) as usize;
        let boot = CUSTOM_BOOT_HEADER + 0x20;
        self.rom[header..header + 4].copy_from_slice(&boot.to_be_bytes());
        self.rom[header + 0x10..header + 0x14].copy_from_slice(&(ROM_BASE + 0x60).to_be_bytes());
        self.rom[header + 0x14..header + 0x18].copy_from_slice(&(ROM_BASE + 0x64).to_be_bytes());
        self.rom[header + 0x18..header + 0x20].copy_from_slice(b"Human68k");
        let [hi, lo] = HYPERCALL_OPCODE.to_be_bytes();
        for (addr, id) in [
            (CUSTOM_STRATEGY, HypercallId::RemoteStrategy),
            (CUSTOM_INTERRUPT, HypercallId::RemoteInterrupt),
            (CUSTOM_INSTALL, HypercallId::CustomInstall),
            (boot, HypercallId::ScsiRomBoot),
        ] {
            let off = (addr - 0x00F0_0000) as usize;
            self.rom[off..off + 4].copy_from_slice(&[hi, lo, 0x4E, 0x75]);
            self.register_hypercall(addr, id);
        }
    }

    /// Installs host directories as file-level remote drives on the VM.
    /// Drives are mounted after physical floppy and SCSI HDD units.
    pub fn install_host_remote_drives(&mut self, paths: Vec<std::path::PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let drives: Vec<u32> = (1..=paths.len() as u32).collect();
        let bridge = Box::new(crate::host_fs::HostFsBridge::new(paths));
        self.install_remote_drives_with_bridge(bridge, drives);
    }

    /// Answers ROM boot metadata with the HDD-only anchor, preserving SRAM.
    /// Other boot devices keep the IPL's own _BOOTINF handling.
    pub(crate) fn custom_boot_info(&self, state: &mut CpuState) -> TrapResult {
        if self.has_custom_remote_drives()
            && matches!(self.peek_u32(0x0638), Some(0x00FE_0000..=0x00FF_FFFF))
            && self.peek_u8(0x09E0) == Some(1)
            && (ROM_BASE..=ROM_BASE + 28)
                .contains(&u32::from_be_bytes(self.sram[12..16].try_into().unwrap()))
            && self.sram[15] & 3 == 0
        {
            state.d[0] = (self.peek_u8(0x09DF).unwrap_or(0) as u32) << 24 | CUSTOM_BOOT_HEADER;
            // Match the IPL handler's final OR.L condition codes (X unchanged).
            state.sr = (state.sr & !0x0F) | if state.d[0] & 0x8000_0000 != 0 { 8 } else { 0 };
            TrapResult::Handled { extra_cycles: 0 }
        } else {
            TrapResult::PassToVector
        }
    }

    pub(crate) fn has_custom_remote_drives(&self) -> bool {
        self.remote_fs
            .as_ref()
            .is_some_and(|fs| !fs.boot && !fs.drives.is_empty())
    }

    /// `RemoteBoot`: loads `HUMAN.SYS` from the boot drive and runs it.
    pub(crate) fn hypercall_remote_boot(&mut self, state: &mut CpuState) -> TrapResult {
        if let Some(human) = self.read_primary_file(b"\\HUMAN.SYS") {
            // An X executable based at $6800, so it runs where it lies without relocation.
            let word = |at: usize| u32::from_be_bytes(human[at..at + 4].try_into().unwrap());
            if human.len() >= 0x40 && &human[0..2] == b"HU" && word(4) == HUMAN_BASE {
                let text_data = (word(0x0C) + word(0x10)) as usize;
                let bss = word(0x14) as usize;
                let base = HUMAN_BASE as usize;
                if human.len() >= 0x40 + text_data && base + text_data + bss <= self.main_ram.len()
                {
                    self.main_ram[base..base + text_data]
                        .copy_from_slice(&human[0x40..0x40 + text_data]);
                    self.main_ram[base + text_data..base + text_data + bss].fill(0);
                    state.pc = HUMAN_BASE;
                }
            }
        }
        // Without HUMAN.SYS the IPL goes on to its next boot device.
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Reads a whole file of the boot drive through the bridge.
    fn read_primary_file(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        let fs = self.remote_fs.as_ref()?;
        let mut full = vec![b'@' + *fs.drives.first()? as u8, b':'];
        full.extend_from_slice(path);
        let handle = fs
            .bridge
            .ask(BridgeOp::Open {
                path: full,
                mode: 0,
            })?
            .result;
        if (handle as i32) < 0 {
            return None;
        }
        let mut data = Vec::new();
        loop {
            let op = BridgeOp::HandleRead {
                handle,
                offset: data.len() as u32,
                len: CHUNK,
            };
            let reply = fs.bridge.ask(op)?;
            if (reply.result as i32) <= 0 {
                break;
            }
            data.extend_from_slice(&reply.data);
        }
        fs.bridge.ask(BridgeOp::Close { handle });
        Some(data)
    }

    /// `RemoteInstall`: builds the remote device's header at `A1`, once (`D2` 0 on the first
    /// call, -1 returned on the next).
    pub(crate) fn hypercall_remote_install(&mut self, state: &mut CpuState) -> TrapResult {
        if state.d[2] != 0 {
            state.d[2] = u32::MAX;
            return TrapResult::Handled { extra_cycles: 0 };
        }
        state.d[2] = 1;
        let header = state.a[1];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes()); // next driver
        bytes.extend_from_slice(&0x2000u16.to_be_bytes()); // remote device
        let custom = self.has_custom_remote_drives();
        bytes.extend_from_slice(
            &(if custom {
                CUSTOM_STRATEGY
            } else {
                STRATEGY_ROUTINE
            })
            .to_be_bytes(),
        );
        bytes.extend_from_slice(
            &(if custom {
                CUSTOM_INTERRUPT
            } else {
                INTERRUPT_ROUTINE
            })
            .to_be_bytes(),
        );
        bytes.extend_from_slice(b"\x01VMX68K ");
        for (i, &byte) in bytes.iter().enumerate() {
            self.poke_u8(header + i as u32, byte);
        }
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.header = header;
        }
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `RemoteStrategy`: keeps the request header at `A5`.
    pub(crate) fn hypercall_remote_strategy(&mut self, state: &mut CpuState) -> TrapResult {
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.request = state.a[5];
        }
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `RemoteInterrupt`: carries out the request the strategy routine got.
    pub(crate) fn hypercall_remote_interrupt(&mut self, _state: &mut CpuState) -> TrapResult {
        let Some(request) = self.remote_fs.as_ref().map(|fs| fs.request) else {
            return TrapResult::Handled { extra_cycles: 0 };
        };
        let byte = |bus: &Self, at: u32| bus.peek_u8(request + at).unwrap_or(0);
        let unit = byte(self, 1) as usize;
        let command = byte(self, 2) & 0x7F;
        let (error, result) = if command == 0x40 {
            self.remote_init(request);
            (0, 0)
        } else {
            match self
                .remote_fs
                .as_ref()
                .and_then(|fs| fs.drives.get(unit).copied())
            {
                Some(drive) => self.remote_command(request, command, drive),
                None => (DEV_ABORT_UNKNOWN_COMMAND, u32::MAX),
            }
        };
        self.poke_u8(request + 3, error as u8);
        self.poke_u8(request + 4, (error >> 8) as u8);
        self.poke_u32_be(request + 18, result);
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `$40`: reports the units, the driver's end, and the boot unit.
    fn remote_init(&mut self, request: u32) {
        let Some(fs) = self.remote_fs.as_ref() else {
            return;
        };
        let (units, header, boot) = (fs.drives.len() as u8, fs.header, fs.boot);
        // Human68k reuses this header to initialise DISK2HD next without setting its command,
        // which as $40 would run past DISK2HD's jump table; its own initialise is $00.
        self.poke_u8(request + 2, 0);
        self.poke_u8(request + 13, units);
        self.poke_u32_be(request + 14, header + 34);
        if boot {
            self.poke_u8(request + 22, 1);
        }
    }

    /// Carries out a command for the primary's drive `drive`; returns the device error and the
    /// result.
    fn remote_command(&mut self, request: u32, command: u8, drive: u32) -> (u16, u32) {
        let long = |bus: &Self, at: u32| bus.peek_u32(request + at).unwrap_or(0);
        let mode = self.peek_u8(request + 13).unwrap_or(0);
        let (param14, param18, fcb) = (long(self, 14), long(self, 18), long(self, 22));
        let result = match command {
            // CHDIR: the directory must exist; Human68k keeps the current directory.
            0x41 => {
                let mut name = self.namests(param14);
                // CHDIR supplies the directory in the path field; its filename
                // fields are not part of the requested directory.
                name.name.clear();
                let mut path = name.primary_path(drive);
                if path.ends_with(b"\\") && path.len() > 3 {
                    path.pop();
                }
                if path.len() <= 3 || self.primary_files(path, 0x10).is_some_and(|(r, _)| r == 0) {
                    0
                } else {
                    DOS_DIRECTORY_NOT_FOUND
                }
            }
            0x47 => self.remote_files(param14, param18 & 0x00FF_FFFF, mode as u32, drive),
            0x48 => self.remote_nfiles(param18 & 0x00FF_FFFF),
            0x42 => self.remote_path_op(param14, drive, |path| BridgeOp::Mkdir { path }),
            0x43 => self.remote_path_op(param14, drive, |path| BridgeOp::Rmdir { path }),
            0x44 => {
                let to = self.namests(param18).primary_path(drive);
                self.remote_path_op(param14, drive, |from| BridgeOp::Rename { from, to })
            }
            0x45 => self.remote_path_op(param14, drive, |path| BridgeOp::Delete { path }),
            // CHMOD: a new attribute, or -1 to read it; the result is the attribute.
            0x46 => {
                let attr = if mode == 0xFF { 0xFFFF } else { mode as u32 };
                self.remote_path_op(param14, drive, |path| BridgeOp::Chmod { path, attr })
            }
            0x49 => self.remote_create(param14, fcb, mode as u32, param18 != 0, drive),
            0x4A => self.remote_open(param14, fcb, drive),
            0x4B => {
                if let Some(OpenFile::Primary(handle)) =
                    self.remote_fs.as_mut().and_then(|fs| fs.files.remove(&fcb))
                {
                    self.ask(BridgeOp::Close { handle });
                }
                0
            }
            0x4C => self.remote_read(fcb, param14, param18),
            0x4D => self.remote_write(fcb, param14, param18),
            0x4E => {
                let position = self.peek_u32(fcb + 6).unwrap_or(0) as i64;
                let size = self.peek_u32(fcb + 64).unwrap_or(0) as i64;
                let base = match mode {
                    0 => 0,
                    1 => position,
                    _ => size,
                };
                let target = base + param18 as i32 as i64;
                if (0..=size).contains(&target) {
                    self.poke_u32_be(fcb + 6, target as u32);
                    target as u32
                } else {
                    DOS_SEEK_ERROR
                }
            }
            // FILEDATE: reads the date and time the FCB holds, or sets them on the primary's file.
            0x4F => {
                if param18 == 0 {
                    (self.peek_u16(fcb + 60).unwrap_or(0) as u32) << 16
                        | self.peek_u16(fcb + 58).unwrap_or(0) as u32
                } else {
                    match self.remote_fs.as_ref().and_then(|fs| fs.files.get(&fcb)) {
                        Some(OpenFile::Primary(handle)) => {
                            let op = BridgeOp::Filedate {
                                handle: *handle,
                                datetime: param18,
                            };
                            match self.ask(op) {
                                Some(reply) if (reply.result as i32) >= 0 => {
                                    self.poke_u16_be(fcb + 58, param18 as u16);
                                    self.poke_u16_be(fcb + 60, (param18 >> 16) as u16);
                                    reply.result
                                }
                                Some(reply) => reply.result,
                                None => DOS_INVALID_ACCESS,
                            }
                        }
                        _ => DOS_CANNOT_WRITE,
                    }
                }
            }
            0x50 => match self.ask(BridgeOp::Dskfre { drive }) {
                Some(reply) => {
                    for (i, &byte) in reply.data.iter().enumerate() {
                        self.poke_u8(param14 + i as u32, byte);
                    }
                    reply.result
                }
                None => DOS_INVALID_ACCESS,
            },
            // DRVCTRL: a medium is always there and ready.
            0x51 => {
                self.poke_u8(request + 13, 0x02);
                0
            }
            // GETDPB: param14 points at DPB+2, after Human68k's drive/unit bytes.
            // A zero sector size identifies a file-level remote device. Inventing
            // HDD geometry makes callers attempt raw FAT/directory access, and
            // writing past this 16-byte payload corrupts the device pointer.
            0x52 => {
                for i in 0..16 {
                    self.poke_u8(param14 + i, 0);
                }
                0
            }
            // FFLUSH and MEDIACHECK: nothing is cached, and the medium does not change. LOCK:
            // only this VM reaches the files through the primary.
            0x56..=0x58 => 0,
            _ => return (DEV_ABORT_UNKNOWN_COMMAND, u32::MAX),
        };
        (0, result)
    }

    fn ask(&self, op: BridgeOp) -> Option<crate::vmx68k::BridgeReply> {
        self.note_drive_access();
        self.remote_fs.as_ref()?.bridge.ask(op)
    }

    /// The primary's `_FILES` for `path` with `attr`: its result and 53-byte buffer.
    fn primary_files(&self, path: Vec<u8>, attr: u32) -> Option<(u32, Vec<u8>)> {
        let reply = self.ask(BridgeOp::Files { attr, path })?;
        Some((reply.result, reply.data))
    }

    /// Reads a `_NAMESTS` name at `at`.
    fn namests(&self, at: u32) -> Namests {
        let bytes = |from: u32, len: u32| -> Vec<u8> {
            (0..len).map(|i| self.peek_u8(at + from + i).unwrap_or(0)).collect()
        };
        let mut path: Vec<u8> = bytes(2, 65)
            .into_iter()
            .take_while(|&b| b != 0)
            .map(|b| if b == 0x09 { b'\\' } else { b })
            .collect();
        if path.is_empty() {
            path.push(b'\\');
        }
        let trim = |v: Vec<u8>| -> Vec<u8> {
            let end = v.iter().rposition(|&b| b != b' ' && b != 0).map_or(0, |i| i + 1);
            v[..end].to_vec()
        };
        let mut name = trim(bytes(67, 8));
        name.extend(trim(bytes(78, 10)));
        let ext = trim(bytes(75, 3));
        if !ext.is_empty() {
            name.push(b'.');
            name.extend(ext);
        }
        Namests { path, name }
    }

    /// `$47` FILES: searches the primary and fills the VM's 53-byte buffer at `buffer`.
    fn remote_files(&mut self, namests: u32, buffer: u32, attr: u32, drive: u32) -> u32 {
        let path = self.namests(namests).primary_path(drive);
        let Some((result, data)) = self.primary_files(path, attr) else {
            return DOS_INVALID_ACCESS;
        };
        if result == 0 {
            self.copy_found_entry(buffer, &data);
            if let Some(fs) = self.remote_fs.as_mut() {
                let mut id = fs.next_search_id;
                while fs.searches.contains_key(&id) {
                    id = id.wrapping_add(1).max(1);
                }
                fs.next_search_id = id.wrapping_add(1).max(1);
                fs.searches.insert(id, data);
                self.poke_u32_be(buffer + 2, SEARCH_MAGIC);
                self.poke_u32_be(buffer + 6, id);
            }
        } else {
            self.poke_u32_be(buffer + 2, 0);
            self.poke_u32_be(buffer + 6, 0);
        }
        result
    }

    /// `$48` NFILES: goes on with the primary's search behind the VM's buffer.
    fn remote_nfiles(&mut self, buffer: u32) -> u32 {
        if self.peek_u32(buffer + 2) != Some(SEARCH_MAGIC) {
            return DOS_FILE_NOT_FOUND;
        }
        let id = self.peek_u32(buffer + 6).unwrap_or(0);
        let Some(previous) = self.remote_fs.as_ref().and_then(|fs| fs.searches.get(&id).cloned()) else {
            return DOS_FILE_NOT_FOUND;
        };
        let Some(reply) = self.ask(BridgeOp::Nfiles { buffer: previous }) else {
            return DOS_INVALID_ACCESS;
        };
        if reply.result == 0 {
            self.copy_found_entry(buffer, &reply.data);
        }
        if let Some(fs) = self.remote_fs.as_mut() {
            if reply.result == 0 {
                fs.searches.insert(id, reply.data);
            } else {
                fs.searches.remove(&id);
                self.poke_u32_be(buffer + 2, 0);
                self.poke_u32_be(buffer + 6, 0);
            }
        }
        reply.result
    }

    /// Copies the result fields, preserving Human68k's search pattern at 10..21.
    fn copy_found_entry(&mut self, buffer: u32, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate().take(53).skip(21) {
            self.poke_u8(buffer + i as u32, byte);
        }
    }

    /// Carries out a DOS call that takes one of the VM's names, made into the primary's path.
    fn remote_path_op(&mut self, namests: u32, drive: u32, op: impl FnOnce(Vec<u8>) -> BridgeOp) -> u32 {
        let path = self.namests(namests).primary_path(drive);
        self.ask(op(path)).map_or(DOS_INVALID_ACCESS, |reply| reply.result)
    }

    /// `$49` CREATE: makes the primary's file, replacing one there unless `replace` is false
    /// (`_NEWFILE`), and fills the FCB.
    fn remote_create(&mut self, namests: u32, fcb: u32, attr: u32, replace: bool, drive: u32) -> u32 {
        let name = self.namests(namests);
        let path = name.primary_path(drive);
        let handle = match self.ask(BridgeOp::Create { path: path.clone(), attr, replace }) {
            Some(reply) if (reply.result as i32) >= 0 => reply.result,
            Some(reply) => return reply.result,
            None => return DOS_INVALID_ACCESS,
        };
        let (time, date) = match self.primary_files(path, 0x27) {
            Some((0, data)) => (
                u16::from_be_bytes([data[22], data[23]]),
                u16::from_be_bytes([data[24], data[25]]),
            ),
            _ => (0, 0),
        };
        self.fill_fcb(fcb, &name, attr as u8, time, date, 0);
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.files.insert(fcb, OpenFile::Primary(handle));
        }
        0
    }

    /// `$4A` OPEN: opens the primary's file and fills the FCB from its directory entry.
    fn remote_open(&mut self, namests: u32, fcb: u32, drive: u32) -> u32 {
        let mode = (self.peek_u8(fcb + 14).unwrap_or(0) & 0x0F) as u32;
        if mode > 2 {
            return DOS_INVALID_ACCESS;
        }
        let name = self.namests(namests);
        let (own_config, boot_drive, shell) = match self.remote_fs.as_ref() {
            Some(fs) => (fs.own_config, fs.drives[0], fs.shell.clone()),
            None => return DOS_INVALID_ACCESS,
        };
        let is_config = drive == boot_drive
            && name.path == b"\\"
            && name.name.eq_ignore_ascii_case(b"CONFIG.SYS");
        if own_config && !is_config && name.name.eq_ignore_ascii_case(&shell) {
            // The shell starts: from now on the VM sees the primary's own CONFIG.SYS.
            if let Some(fs) = self.remote_fs.as_mut() {
                fs.own_config = false;
            }
        }
        let path = if own_config && is_config {
            let mut own = vec![b'@' + drive as u8, b':'];
            own.extend_from_slice(b"\\VMX68K\\CONFIG.SYS");
            own
        } else {
            name.primary_path(drive)
        };

        let entry = self.primary_files(path.clone(), 0x27).filter(|(r, _)| *r == 0);
        let (file, attr, time, date, size) = match entry {
            Some((_, data)) => {
                let handle = match self.ask(BridgeOp::Open { path, mode }) {
                    Some(reply) if (reply.result as i32) >= 0 => reply.result,
                    Some(reply) => return reply.result,
                    None => return DOS_INVALID_ACCESS,
                };
                let word = |at: usize| u16::from_be_bytes([data[at], data[at + 1]]);
                let size = u32::from_be_bytes(data[26..30].try_into().unwrap());
                (OpenFile::Primary(handle), data[21], word(22), word(24), size)
            }
            // The VM's own CONFIG.SYS, when the primary has none for it, which only reads.
            None if own_config && is_config && mode == 0 => {
                (OpenFile::Memory(DEFAULT_CONFIG.to_vec()), 0x20, 0, 0, DEFAULT_CONFIG.len() as u32)
            }
            None => return DOS_FILE_NOT_FOUND,
        };
        if own_config && is_config {
            let contents = match &file {
                OpenFile::Memory(data) => Some(data.clone()),
                OpenFile::Primary(_) => None,
            };
            let shell = contents.or_else(|| self.read_primary_file(b"\\VMX68K\\CONFIG.SYS")).and_then(|c| shell_of(&c));
            if let (Some(fs), Some(shell)) = (self.remote_fs.as_mut(), shell) {
                fs.shell = shell;
            }
        }

        self.fill_fcb(fcb, &name, attr, time, date, size);
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.files.insert(fcb, file);
        }
        0
    }

    /// Fills the FCB's copy of the directory entry, big-endian, and starts at offset 0.
    fn fill_fcb(&mut self, fcb: u32, name: &Namests, attr: u8, time: u16, date: u16, size: u32) {
        let bytes = |v: &[u8], len: usize| -> Vec<u8> {
            let mut out = v.to_vec();
            out.resize(len, b' ');
            out
        };
        let (stem, ext) = match name.name.iter().rposition(|&b| b == b'.') {
            Some(dot) => (&name.name[..dot], &name.name[dot + 1..]),
            None => (&name.name[..], &b""[..]),
        };
        let mut name2 = stem.get(8..).unwrap_or(&[]).to_vec();
        name2.resize(10, 0);
        for (i, &b) in bytes(&stem[..stem.len().min(8)], 8).iter().enumerate() {
            self.poke_u8(fcb + 36 + i as u32, b);
        }
        for (i, &b) in bytes(ext, 3).iter().enumerate() {
            self.poke_u8(fcb + 44 + i as u32, b);
        }
        self.poke_u8(fcb + 47, attr);
        for (i, &b) in name2.iter().enumerate() {
            self.poke_u8(fcb + 48 + i as u32, b);
        }
        self.poke_u16_be(fcb + 58, time);
        self.poke_u16_be(fcb + 60, date);
        self.poke_u16_be(fcb + 62, 0);
        self.poke_u32_be(fcb + 64, size);
        self.poke_u32_be(fcb + 6, 0);
    }

    /// `$4D` WRITE: writes the VM's memory at `buffer` at the FCB's position, or with `len` 0
    /// cuts the file there.
    fn remote_write(&mut self, fcb: u32, buffer: u32, len: u32) -> u32 {
        let handle = match self.remote_fs.as_ref().and_then(|fs| fs.files.get(&fcb)) {
            Some(OpenFile::Primary(handle)) => *handle,
            Some(OpenFile::Memory(_)) => return DOS_CANNOT_WRITE,
            None => return DOS_INVALID_ACCESS,
        };
        let position = self.peek_u32(fcb + 6).unwrap_or(0);
        let mut done = 0u32;
        loop {
            let want = (len - done).min(CHUNK);
            let data: Vec<u8> = (0..want).map(|i| self.peek_u8(buffer + done + i).unwrap_or(0)).collect();
            let op = BridgeOp::HandleWrite {
                handle,
                offset: position + done,
                data,
            };
            match self.ask(op) {
                Some(reply) if (reply.result as i32) >= 0 => done += reply.result.min(want),
                Some(reply) if done == 0 => return reply.result,
                _ => break,
            }
            if done >= len || want == 0 {
                break;
            }
        }
        let end = position + done;
        let size = self.peek_u32(fcb + 64).unwrap_or(0);
        self.poke_u32_be(fcb + 6, end);
        // Writing past the end grows the file; writing nothing cuts it there.
        self.poke_u32_be(fcb + 64, if len == 0 { end } else { size.max(end) });
        done
    }

    /// `$4C` READ: reads from the FCB's position into the VM's memory at `buffer`.
    fn remote_read(&mut self, fcb: u32, buffer: u32, len: u32) -> u32 {
        let position = self.peek_u32(fcb + 6).unwrap_or(0);
        let size = self.peek_u32(fcb + 64).unwrap_or(0);
        let len = len.min(size.saturating_sub(position));
        let mut done = 0u32;
        while done < len {
            let at = position + done;
            let want = (len - done).min(CHUNK);
            let data = match self.remote_fs.as_ref().and_then(|fs| fs.files.get(&fcb)) {
                Some(OpenFile::Memory(data)) => data[at as usize..(at + want) as usize].to_vec(),
                Some(OpenFile::Primary(handle)) => {
                    let op = BridgeOp::HandleRead {
                        handle: *handle,
                        offset: at,
                        len: want,
                    };
                    match self.ask(op) {
                        Some(reply) if (reply.result as i32) > 0 => reply.data,
                        _ => break,
                    }
                }
                None => return DOS_INVALID_ACCESS,
            };
            for (i, &byte) in data.iter().enumerate() {
                self.poke_u8(buffer + done + i as u32, byte);
            }
            done += data.len() as u32;
            if (data.len() as u32) < want {
                break;
            }
        }
        self.poke_u32_be(fcb + 6, position + done);
        done
    }

    fn poke_u16_be(&mut self, addr: u32, value: u16) {
        self.poke_u8(addr, (value >> 8) as u8);
        self.poke_u8(addr + 1, value as u8);
    }

    fn poke_u32_be(&mut self, addr: u32, value: u32) {
        self.poke_u16_be(addr, (value >> 16) as u16);
        self.poke_u16_be(addr + 2, value as u16);
    }
}

/// The file name of the `SHELL =` line of a `CONFIG.SYS`, if it has one.
fn shell_of(config: &[u8]) -> Option<Vec<u8>> {
    config.split(|&b| b == b'\n').find_map(|line| {
        let line: Vec<u8> = line.iter().copied().filter(|&b| b != b'\r').collect();
        let (key, value) = line.split_at(line.iter().position(|&b| b == b'=')?);
        if !key.trim_ascii().eq_ignore_ascii_case(b"SHELL") {
            return None;
        }
        let command = value[1..].trim_ascii().split(|&b| b == b' ' || b == b'\t').next()?;
        let name = command.rsplit(|&b| b == b'\\' || b == b':').next()?;
        (!name.is_empty()).then(|| name.to_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_results_preserve_the_guests_wildcard_pattern() {
        let mut bus = X68kBus::new(1024 * 1024, crate::CpuSpeedMode::Burst.timing_config());
        let buffer = 0x40000;
        let pattern = b"????????TXT";
        for (i, byte) in pattern.iter().enumerate() {
            bus.poke_u8(buffer + 10 + i as u32, *byte);
        }
        let mut found = vec![0; 53];
        found[10..21].copy_from_slice(b"HELLO   TXT");
        found[21] = 0x20;
        found[30..40].copy_from_slice(b"HELLO.TXT\0");
        bus.copy_found_entry(buffer, &found);
        for (i, byte) in pattern.iter().enumerate() {
            assert_eq!(bus.peek_u8(buffer + 10 + i as u32), Some(*byte));
        }
        assert_eq!(bus.peek_u8(buffer + 21), Some(0x20));
        assert_eq!(bus.peek_u8(buffer + 30), Some(b'H'));
    }

    #[test]
    fn getdpb_writes_only_the_remote_geometry_payload() {
        let mut bus = X68kBus::new(1024 * 1024, crate::CpuSpeedMode::Burst.timing_config());
        let (bridge, _) = crate::vmx68k::vm_bridge();
        bus.install_remote_drives(bridge, vec![1]);
        let request = 0x30000;
        let buffer = 0x40000;
        for i in 0..20 {
            bus.poke_u8(buffer + i, 0xA5);
        }
        // Human68k passes the address AFTER the drive and unit bytes.
        bus.poke_u32_be(request + 14, buffer + 2);
        assert_eq!(bus.remote_command(request, 0x52, 1), (0, 0));
        assert_eq!(bus.peek_u16(buffer), Some(0xA5A5));
        for i in 2..18 {
            assert_eq!(bus.peek_u8(buffer + i), Some(0), "remote geometry byte {i}");
        }
        assert_eq!(bus.peek_u16(buffer + 18), Some(0xA5A5), "device pointer must survive");
    }

    #[test]
    fn custom_shares_preserve_devices_and_boot_state() {
        let (bridge, requests) = crate::vmx68k::vm_bridge();
        let mut bus = X68kBus::new(1024 * 1024, crate::CpuSpeedMode::Burst.timing_config());
        let sram = bus.sram().to_vec();
        bus.install_remote_drives(bridge, vec![2, 1]);
        assert_eq!(bus.sram(), sram);
        assert!(bus.fdc.drives[0].connected && bus.fdc.drives[1].connected);
        assert!(!bus.remote_boot_rom);
        assert_eq!(
            bus.hypercall_at(ROM_BASE + 0x50),
            Some(HypercallId::ScsiRomBoot)
        );
        assert_eq!(bus.peek_u32(ROM_BASE), Some(ROM_BASE + 0x50));
        assert_eq!(
            bus.hypercall_at(CUSTOM_INTERRUPT),
            Some(HypercallId::RemoteInterrupt)
        );
        let fs = bus.remote_fs.as_ref().unwrap();
        assert!(!fs.boot && !fs.own_config);
        assert_eq!(fs.drives, [2, 1]);
        assert!(
            requests.try_recv().is_err(),
            "mounting must not wait on the primary"
        );

        let mut state = CpuState::default();
        state.a[4] = 0x20000;
        state.a[1] = 0x10000;
        bus.hypercall_custom_install(&mut state);
        assert_eq!(state.d[2], 2);
        assert_eq!(state.a[4], 0x20000, "preserve the caller's A4");
        assert_eq!(bus.peek_u16(0x10004), Some(0x2000));
        assert_eq!(bus.peek_u32(0x10006), Some(CUSTOM_STRATEGY));
        assert_eq!(bus.peek_u32(0x1000A), Some(CUSTOM_INTERRUPT));
        let request = 0x30000;
        bus.poke_u8(request + 22, 5);
        bus.remote_init(request);
        assert_eq!(bus.peek_u8(request + 13), Some(2));
        assert_eq!(
            bus.peek_u8(request + 22),
            Some(5),
            "custom shares must not select boot unit 1"
        );
        bus.hypercall_custom_install(&mut state);
        assert_eq!(state.d[2], u32::MAX, "one remote driver only");
    }

    #[test]
    fn hdd_boot_metadata_keeps_shares_after_the_physical_drives() {
        let (bridge, _) = crate::vmx68k::vm_bridge();
        let mut bus = X68kBus::new(1024 * 1024, crate::CpuSpeedMode::Burst.timing_config());
        bus.install_remote_drives(bridge, vec![1]);
        let mut state = CpuState::default();
        bus.poke_u32_be(0x0638, 0x00FF_AA66);
        bus.poke_u8(0x09E0, 1);
        bus.poke_u8(0x09DF, 2); // Keep the IPL's warm-boot flag.
        bus.set_boot_from_scsi(true);
        let sram = bus.sram().to_vec();
        state.sr = 0x201F;
        assert_eq!(
            bus.custom_boot_info(&mut state),
            TrapResult::Handled { extra_cycles: 0 }
        );
        assert_eq!(state.d[0], 0x0200_0000 | CUSTOM_BOOT_HEADER);
        assert_eq!(state.sr, 0x2010);
        assert_eq!(bus.sram(), sram);
        assert_eq!(
            bus.peek_u32(CUSTOM_BOOT_HEADER),
            Some(CUSTOM_BOOT_HEADER + 0x20)
        );
        assert_eq!(
            bus.hypercall_at(CUSTOM_BOOT_HEADER + 0x20),
            Some(HypercallId::ScsiRomBoot)
        );
        // A guest driver replacing _BOOTINF retains control; floppy boots also
        // use the IPL unchanged.
        bus.poke_u32_be(0x0638, 0x20000);
        assert_eq!(bus.custom_boot_info(&mut state), TrapResult::PassToVector);
        bus.poke_u32_be(0x0638, 0x00FF_AA66);
        bus.poke_u8(0x09E0, 0x90);
        assert_eq!(bus.custom_boot_info(&mut state), TrapResult::PassToVector);
    }

    #[test]
    fn the_shell_is_read_from_config_sys() {
        assert_eq!(
            shell_of(b"FILES = 64\r\nSHELL     = \\SHELL\\SXWIN.X -@\r\n").as_deref(),
            Some(&b"SXWIN.X"[..])
        );
        assert_eq!(
            shell_of(b"shell=a:\\bin\\fish.x\r\n").as_deref(),
            Some(&b"FISH.X"[..])
        );
        assert_eq!(shell_of(DEFAULT_CONFIG), None);
    }

    #[test]
    fn host_remote_drives_mount_and_execute_filesystem_commands() {
        let dir = std::env::temp_dir().join(format!("msx68k_bus_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("TEST.TXT"), b"MSX68K HOST FS").unwrap();

        let mut bus = X68kBus::new(1024 * 1024, crate::CpuSpeedMode::Burst.timing_config());
        bus.install_host_remote_drives(vec![dir.clone()]);

        // SCSI ROM and hypercalls should be configured for custom remote driver
        assert!(bus.is_scsi_rom_enabled());
        assert!(bus.has_custom_remote_drives());

        // Install driver at 0x10000
        let mut state = CpuState::default();
        state.a[4] = 0x20000;
        state.a[1] = 0x10000;
        bus.hypercall_custom_install(&mut state);
        assert_eq!(state.d[2], 2);
        assert_eq!(bus.peek_u16(0x10004), Some(0x2000)); // Remote device type

        // Request INIT (Command $40)
        let request = 0x30000;
        bus.poke_u8(request + 1, 0); // Unit 0
        bus.poke_u8(request + 2, 0x40); // Command $40
        bus.poke_u32_be(request + 14, 0);
        bus.remote_init(request);
        assert_eq!(bus.peek_u8(request + 13), Some(1)); // 1 unit

        // Open TEST.TXT (Command $4A)
        // Set up NAMESTS at 0x40000:
        // offset 2: path (root: "\")
        // offset 67: name ("TEST    ")
        // offset 75: ext ("TXT")
        let namests = 0x40000;
        bus.poke_u8(namests + 2, b'\\');
        bus.poke_u8(namests + 3, 0);
        for (i, &b) in b"TEST    ".iter().enumerate() {
            bus.poke_u8(namests + 67 + i as u32, b);
        }
        for (i, &b) in b"TXT".iter().enumerate() {
            bus.poke_u8(namests + 75 + i as u32, b);
        }
        for i in 78..88 {
            bus.poke_u8(namests + i, 0);
        }

        let fcb = 0x50000;
        bus.poke_u8(fcb + 14, 0); // Mode 0 (read only)
        bus.poke_u32_be(request + 14, namests);
        bus.poke_u32_be(request + 22, fcb);

        let (err, res) = bus.remote_command(request, 0x4A, 1);
        assert_eq!(err, 0);
        assert_eq!(res, 0);

        // Verify FCB size was populated correctly
        let size = bus.peek_u32(fcb + 64).unwrap_or(0);
        assert_eq!(size, 14); // "MSX68K HOST FS" is 14 bytes

        // Read 14 bytes from FCB (Command $4C)
        let buf = 0x60000;
        bus.poke_u32_be(request + 14, buf);
        bus.poke_u32_be(request + 18, 14);
        bus.poke_u32_be(request + 22, fcb);
        let (err, read_len) = bus.remote_command(request, 0x4C, 1);
        assert_eq!(err, 0);
        assert_eq!(read_len, 14);

        let mut read_bytes = Vec::new();
        for i in 0..14 {
            read_bytes.push(bus.peek_u8(buf + i).unwrap_or(0));
        }
        assert_eq!(read_bytes, b"MSX68K HOST FS");

        // Close file (Command $4B)
        let (err, res) = bus.remote_command(request, 0x4B, 1);
        assert_eq!(err, 0);
        assert_eq!(res, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
