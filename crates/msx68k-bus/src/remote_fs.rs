//! A VM's remote drives in drive bypass mode (`docs/VMX68K.md` Section 2): the master VM's
//! drives under the same letters, bridged at the file level, and booted from.
//!
//! The VM boots through a ROM msx68k provides at `$FC0000`, laid out like the internal SCSI ROM
//! that Human68k already knows how to boot from: the IPL boot routine loads `HUMAN.SYS` from
//! the master's `A:` through the [`VmBridge`], and the driver install routine builds a remote
//! device (type `$2000`). Human68k then hands the device file-level commands (`_NAMESTS` names,
//! FCBs), which become the master's DOS calls, carried out by VMX68K.SX. Until the VM's shell
//! starts, its `\CONFIG.SYS` is the master's `\VMX68K\CONFIG.SYS`, so that it does not start
//! SX-Window again.

use std::collections::HashMap;

use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;
use crate::hypercall::{HypercallId, HYPERCALL_OPCODE};
use crate::vmx68k::{BridgeOp, VmBridge};

/// Where the boot ROM lives in the VM, the internal SCSI ROM window.
const ROM_BASE: u32 = 0x00FC_0000;
/// IPL boot routine, which the handle at [`ROM_BASE`] points to.
const BOOT_ROUTINE: u32 = ROM_BASE + 0x14;
/// Driver install routine, found by Human68k at the IPL boot routine's address - 16.
const INSTALL_ROUTINE: u32 = ROM_BASE + 0x18;
const STRATEGY_ROUTINE: u32 = ROM_BASE + 0x1C;
const INTERRUPT_ROUTINE: u32 = ROM_BASE + 0x20;

/// Where `HUMAN.SYS` runs, its base address and entry.
const HUMAN_BASE: u32 = 0x6800;

/// Bytes VMX68K.SX can move at once, its buffer.
const CHUNK: u32 = 32 * 1024;

// Human68k DOS errors.
const DOS_FILE_NOT_FOUND: u32 = -2i32 as u32;
const DOS_DIRECTORY_NOT_FOUND: u32 = -3i32 as u32;
const DOS_INVALID_ACCESS: u32 = -12i32 as u32;
const DOS_CANNOT_WRITE: u32 = -19i32 as u32;
const DOS_SEEK_ERROR: u32 = -25i32 as u32;

// Device errors in the request header: abort, and the command or unit is unknown.
const DEV_ABORT_UNKNOWN_COMMAND: u16 = 0x1003;

/// The VM's `CONFIG.SYS` when the master has no `\VMX68K\CONFIG.SYS`: Human68k's own settings,
/// with no SX-Window and `COMMAND.X` as the shell.
const DEFAULT_CONFIG: &[u8] = b"FILES     = 64\r\nBUFFERS   = 20 1024\r\nLASTDRIVE = Z:\r\n";

/// What an FCB of the VM stands for.
#[derive(Debug)]
enum OpenFile {
    /// A file the master has open, by its handle.
    Master(u32),
    /// A file the host serves from memory.
    Memory(Vec<u8>),
}

/// The VM's remote drives.
#[derive(Debug)]
pub struct RemoteFs {
    bridge: VmBridge,
    /// The master's drive number (1 is A:) of each unit.
    drives: Vec<u32>,
    /// Device header built by the install routine, and the request the strategy routine got.
    header: u32,
    request: u32,
    files: HashMap<u32, OpenFile>,
    /// The master's search buffer behind each `_FILES` buffer of the VM.
    searches: HashMap<u32, Vec<u8>>,
    /// Whether `\CONFIG.SYS` is still the VM's own, and the shell whose start ends that.
    own_config: bool,
    shell: Vec<u8>,
}

impl RemoteFs {
    /// Remote drives for the master's drives `drives` (1 is A:), the first one booted from.
    pub fn new(bridge: VmBridge, drives: Vec<u32>) -> Self {
        Self {
            bridge,
            drives,
            header: 0,
            request: 0,
            files: HashMap::new(),
            searches: HashMap::new(),
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
    /// The master's path for it on drive `drive` (1 is A:).
    fn master_path(&self, drive: u32) -> Vec<u8> {
        let mut out = vec![b'@' + drive as u8, b':'];
        out.extend_from_slice(&self.path);
        out.extend_from_slice(&self.name);
        out
    }
}

impl X68kBus {
    /// Makes the VM boot from the remote drive `fs.drives[0]` and gives it the drives.
    pub fn install_remote_boot(&mut self, fs: RemoteFs) {
        let rom = (ROM_BASE - 0x00F0_0000) as usize;
        let [hi, lo] = HYPERCALL_OPCODE.to_be_bytes();
        let mut image = Vec::new();
        image.extend_from_slice(&BOOT_ROUTINE.to_be_bytes()); // IPL boot handle
        image.extend_from_slice(&INSTALL_ROUTINE.to_be_bytes()); // install handle
        image.extend_from_slice(&0u32.to_be_bytes()); // install parameter
        image.extend_from_slice(b"Human68k");
        for _ in [BOOT_ROUTINE, INSTALL_ROUTINE, STRATEGY_ROUTINE, INTERRUPT_ROUTINE] {
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

    /// `RemoteBoot`: loads `HUMAN.SYS` from the boot drive and runs it.
    pub(crate) fn hypercall_remote_boot(&mut self, state: &mut CpuState) -> TrapResult {
        if let Some(human) = self.read_master_file(b"\\HUMAN.SYS") {
            // An X executable based at $6800, so it runs where it lies without relocation.
            let word = |at: usize| u32::from_be_bytes(human[at..at + 4].try_into().unwrap());
            if human.len() >= 0x40 && &human[0..2] == b"HU" && word(4) == HUMAN_BASE {
                let text_data = (word(0x0C) + word(0x10)) as usize;
                let bss = word(0x14) as usize;
                let base = HUMAN_BASE as usize;
                if human.len() >= 0x40 + text_data && base + text_data + bss <= self.main_ram.len() {
                    self.main_ram[base..base + text_data].copy_from_slice(&human[0x40..0x40 + text_data]);
                    self.main_ram[base + text_data..base + text_data + bss].fill(0);
                    state.pc = HUMAN_BASE;
                }
            }
        }
        // Without HUMAN.SYS the IPL goes on to its next boot device.
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Reads a whole file of the boot drive through the bridge.
    fn read_master_file(&mut self, path: &[u8]) -> Option<Vec<u8>> {
        let fs = self.remote_fs.as_ref()?;
        let mut full = vec![b'@' + *fs.drives.first()? as u8, b':'];
        full.extend_from_slice(path);
        let handle = fs.bridge.ask(BridgeOp::Open { path: full })?.result;
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
        bytes.extend_from_slice(&STRATEGY_ROUTINE.to_be_bytes());
        bytes.extend_from_slice(&INTERRUPT_ROUTINE.to_be_bytes());
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
            match self.remote_fs.as_ref().and_then(|fs| fs.drives.get(unit).copied()) {
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
        let (units, header) = (fs.drives.len() as u8, fs.header);
        // Human68k reuses this header to initialise DISK2HD next without setting its command,
        // which as $40 would run past DISK2HD's jump table; its own initialise is $00.
        self.poke_u8(request + 2, 0);
        self.poke_u8(request + 13, units);
        self.poke_u32_be(request + 14, header + 34);
        self.poke_u8(request + 22, 1);
    }

    /// Carries out a command for the master's drive `drive`; returns the device error and the
    /// result.
    fn remote_command(&mut self, request: u32, command: u8, drive: u32) -> (u16, u32) {
        let long = |bus: &Self, at: u32| bus.peek_u32(request + at).unwrap_or(0);
        let mode = self.peek_u8(request + 13).unwrap_or(0);
        let (param14, param18, fcb) = (long(self, 14), long(self, 18), long(self, 22));
        let result = match command {
            // CHDIR: the directory must exist; Human68k keeps the current directory.
            0x41 => {
                let name = self.namests(param14);
                let mut path = name.master_path(drive);
                if path.ends_with(b"\\") && path.len() > 3 {
                    path.pop();
                }
                if path.len() <= 3 || self.master_files(path, 0x10).is_some_and(|(r, _)| r == 0) {
                    0
                } else {
                    DOS_DIRECTORY_NOT_FOUND
                }
            }
            0x47 => self.remote_files(param14, param18 & 0x00FF_FFFF, mode as u32, drive),
            0x48 => self.remote_nfiles(param18 & 0x00FF_FFFF),
            0x4A => self.remote_open(param14, fcb, drive),
            0x4B => {
                if let Some(OpenFile::Master(handle)) =
                    self.remote_fs.as_mut().and_then(|fs| fs.files.remove(&fcb))
                {
                    self.ask(BridgeOp::Close { handle });
                }
                0
            }
            0x4C => self.remote_read(fcb, param14, param18),
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
            // FILEDATE: reads the date and time the FCB holds; nothing may be written yet.
            0x4F => {
                if param18 == 0 {
                    (self.peek_u16(fcb + 60).unwrap_or(0) as u32) << 16
                        | self.peek_u16(fcb + 58).unwrap_or(0) as u32
                } else {
                    DOS_CANNOT_WRITE
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
            // GETDPB: a remote device has 0 bytes per sector, and the rest is not used.
            0x52 => {
                for i in 0..16 {
                    self.poke_u8(param14 + i, 0);
                }
                0
            }
            // FFLUSH, and MEDIACHECK: nothing is cached, and the medium does not change.
            0x56 | 0x57 => 0,
            // Writing comes in a later step.
            0x42..=0x46 | 0x49 | 0x4D | 0x53 | 0x54 | 0x58 => DOS_CANNOT_WRITE,
            _ => return (DEV_ABORT_UNKNOWN_COMMAND, u32::MAX),
        };
        (0, result)
    }

    fn ask(&self, op: BridgeOp) -> Option<crate::vmx68k::BridgeReply> {
        self.remote_fs.as_ref()?.bridge.ask(op)
    }

    /// The master's `_FILES` for `path` with `attr`: its result and 53-byte buffer.
    fn master_files(&self, path: Vec<u8>, attr: u32) -> Option<(u32, Vec<u8>)> {
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

    /// `$47` FILES: searches the master and fills the VM's 53-byte buffer at `buffer`.
    fn remote_files(&mut self, namests: u32, buffer: u32, attr: u32, drive: u32) -> u32 {
        let path = self.namests(namests).master_path(drive);
        let Some((result, data)) = self.master_files(path, attr) else {
            return DOS_INVALID_ACCESS;
        };
        if result == 0 {
            self.copy_found_entry(buffer, &data);
        }
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.searches.insert(buffer, data);
        }
        result
    }

    /// `$48` NFILES: goes on with the master's search behind the VM's buffer.
    fn remote_nfiles(&mut self, buffer: u32) -> u32 {
        let Some(previous) = self.remote_fs.as_ref().and_then(|fs| fs.searches.get(&buffer).cloned()) else {
            return DOS_FILE_NOT_FOUND;
        };
        let Some(reply) = self.ask(BridgeOp::Nfiles { buffer: previous }) else {
            return DOS_INVALID_ACCESS;
        };
        if reply.result == 0 {
            self.copy_found_entry(buffer, &reply.data);
        }
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.searches.insert(buffer, reply.data);
        }
        reply.result
    }

    /// Copies what a search found, from attribute to name, into the VM's buffer.
    fn copy_found_entry(&mut self, buffer: u32, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate().take(53).skip(21) {
            self.poke_u8(buffer + i as u32, byte);
        }
    }

    /// `$4A` OPEN: opens the master's file and fills the FCB from its directory entry.
    fn remote_open(&mut self, namests: u32, fcb: u32, drive: u32) -> u32 {
        if self.peek_u8(fcb + 14).unwrap_or(0) & 0x0F != 0 {
            return DOS_CANNOT_WRITE;
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
            // The shell starts: from now on the VM sees the master's own CONFIG.SYS.
            if let Some(fs) = self.remote_fs.as_mut() {
                fs.own_config = false;
            }
        }
        let path = if own_config && is_config {
            let mut own = vec![b'@' + drive as u8, b':'];
            own.extend_from_slice(b"\\VMX68K\\CONFIG.SYS");
            own
        } else {
            name.master_path(drive)
        };

        let entry = self.master_files(path.clone(), 0x21).filter(|(r, _)| *r == 0);
        let (file, attr, time, date, size) = match entry {
            Some((_, data)) => {
                let handle = match self.ask(BridgeOp::Open { path }) {
                    Some(reply) if (reply.result as i32) >= 0 => reply.result,
                    Some(reply) => return reply.result,
                    None => return DOS_INVALID_ACCESS,
                };
                let word = |at: usize| u16::from_be_bytes([data[at], data[at + 1]]);
                let size = u32::from_be_bytes(data[26..30].try_into().unwrap());
                (OpenFile::Master(handle), data[21], word(22), word(24), size)
            }
            // The VM's own CONFIG.SYS, when the master has none for it.
            None if own_config && is_config => {
                (OpenFile::Memory(DEFAULT_CONFIG.to_vec()), 0x20, 0, 0, DEFAULT_CONFIG.len() as u32)
            }
            None => return DOS_FILE_NOT_FOUND,
        };
        if own_config && is_config {
            let contents = match &file {
                OpenFile::Memory(data) => Some(data.clone()),
                OpenFile::Master(_) => None,
            };
            let shell = contents.or_else(|| self.read_master_file(b"\\VMX68K\\CONFIG.SYS")).and_then(|c| shell_of(&c));
            if let (Some(fs), Some(shell)) = (self.remote_fs.as_mut(), shell) {
                fs.shell = shell;
            }
        }

        // The FCB's copy of the directory entry, big-endian.
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
        if let Some(fs) = self.remote_fs.as_mut() {
            fs.files.insert(fcb, file);
        }
        0
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
                Some(OpenFile::Master(handle)) => {
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
    fn the_shell_is_read_from_config_sys() {
        assert_eq!(shell_of(b"FILES = 64\r\nSHELL     = \\SHELL\\SXWIN.X -@\r\n").as_deref(), Some(&b"SXWIN.X"[..]));
        assert_eq!(shell_of(b"shell=a:\\bin\\fish.x\r\n").as_deref(), Some(&b"FISH.X"[..]));
        assert_eq!(shell_of(DEFAULT_CONFIG), None);
    }
}
