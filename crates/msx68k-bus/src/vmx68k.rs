//! The primary VM's side of VMX68K (`docs/VMX68K.md` Section 3.5): VMX68K.SX opens and closes
//! VMs through msx68k's application entry, and the host finds where each VM's window shows.
//!
//! The bus only keeps the record. The app starts and stops the VMs it reports, each on a
//! thread of its own, and places their screens with [`X68kBus::vm_window_layout`].

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use msx68k_cpu::{CpuModel, CpuState, TrapResult};

use crate::bus::X68kBus;
use crate::floppy_image::{FloppyImage, ImageStorage, Probe};
use crate::timing::CpuSpeedMode;

/// `SYS/FSX.X` global holding the active window record (`WMActive`), from its text base.
const ACTIVE_WINDOW: u32 = 0x543CA;

/// User event VMX68K.SX gets when a VM has disk requests (`E_USER1` of `SXDEF2.H`).
const E_USER1: u16 = 14;

// Window records (`SXDEF2.H`).
const WINDOW_BITMAP: u32 = 0;
const WINDOW_RECT: u32 = 4;
const WINDOW_VISIBLE_RGN: u32 = 16;
const WINDOW_VISIBLE: u32 = 66;
const BITMAP_RECT: u32 = 2;

/// The channel through which a VM asks VMX68K.SX, in the primary VM, to read or write the
/// primary's storage for it (`docs/VMX68K.md` Section 3.1). The asking thread waits for each
/// reply.
#[derive(Debug, Clone)]
pub struct VmBridge {
    requests: Sender<BridgeRequest>,
    floppies: Vec<(u32, u32, Arc<PassthroughMedia>)>,
}

/// One request to VMX68K.SX, answered with its [`BridgeReply`], or `None` when it could not be
/// carried out.
#[derive(Debug)]
pub struct BridgeRequest {
    pub op: BridgeOp,
    pub reply: Sender<Option<BridgeReply>>,
}

/// What VMX68K.SX does, by the kind `VmPoll` returns in `D0.L`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeOp {
    /// 1: reads `len` bytes of drive 0's image at `offset`.
    Read { offset: u32, len: u32 },
    /// 2: writes the bytes to drive 0's image at `offset`.
    Write { offset: u32, data: Vec<u8> },
    /// 3: opens the primary's file `path` with `mode` (0 read, 1 write, 2 both); the result is
    /// its handle.
    Open { path: Vec<u8>, mode: u32 },
    /// 4: reads `len` bytes at `offset` of the primary's file `handle`; the result is the count.
    HandleRead { handle: u32, offset: u32, len: u32 },
    /// 5: closes the primary's file `handle`.
    Close { handle: u32 },
    /// 6: `_FILES` for `path` with `attr`; the data is the 53-byte buffer.
    Files { attr: u32, path: Vec<u8> },
    /// 7: `_NFILES` with the 53-byte buffer of an earlier search.
    Nfiles { buffer: Vec<u8> },
    /// 8: `_DSKFRE` of `drive` (1 is A:); the data is the 8-byte buffer.
    Dskfre { drive: u32 },
    /// 9: `_MKDIR`.
    Mkdir { path: Vec<u8> },
    /// 10: `_RMDIR`.
    Rmdir { path: Vec<u8> },
    /// 11: `_DELETE`.
    Delete { path: Vec<u8> },
    /// 12: `_RENAME` of `from` to `to`, which may move the file.
    Rename { from: Vec<u8>, to: Vec<u8> },
    /// 13: `_CHMOD` of `path` to `attr`, or reading it with `0xFFFF`; the result is the attribute.
    Chmod { path: Vec<u8>, attr: u32 },
    /// 14: `_CREATE` (replacing a file there) or `_NEWFILE` of `path` with `attr`; the result is
    /// its handle.
    Create {
        path: Vec<u8>,
        attr: u32,
        replace: bool,
    },
    /// 15: writes the bytes at `offset` of the primary's file `handle`; with none, cuts the file
    /// there. The result is the count.
    HandleWrite {
        handle: u32,
        offset: u32,
        data: Vec<u8>,
    },
    /// 16: `_FILEDATE` setting `date << 16 | time` of the primary's file `handle`.
    Filedate { handle: u32, datetime: u32 },
    /// 17: the VM has finished changing media; the task can close its old handle.
    MediaDone {
        unit: u32,
    },
    /// 18: persist a resized image and reopen it under the same DOS handle.
    ImageSync {
        handle: u32,
    },
    FloppyEject {
        unit: u32,
    },
    FloppyRelease {
        unit: u32,
    },
    /// Native requests, completed by the primary bus during VmPoll.
    FloppyRead { unit: u32, offset: u32, len: u32 },
    FloppyWrite {
        unit: u32,
        offset: u32,
        data: Vec<u8>,
    },
}

impl BridgeOp {
    fn kind(&self) -> u32 {
        match self {
            Self::Read { .. } => 1,
            Self::Write { .. } => 2,
            Self::Open { .. } => 3,
            Self::HandleRead { .. } => 4,
            Self::Close { .. } => 5,
            Self::Files { .. } => 6,
            Self::Nfiles { .. } => 7,
            Self::Dskfre { .. } => 8,
            Self::Mkdir { .. } => 9,
            Self::Rmdir { .. } => 10,
            Self::Delete { .. } => 11,
            Self::Rename { .. } => 12,
            Self::Chmod { .. } => 13,
            Self::Create { .. } => 14,
            Self::HandleWrite { .. } => 15,
            Self::Filedate { .. } => 16,
            Self::MediaDone { .. } => 17,
            Self::ImageSync { .. } => 18,
            Self::FloppyEject { .. } | Self::FloppyRelease { .. } => 0,
            Self::FloppyRead { .. } | Self::FloppyWrite { .. } => 0,
        }
    }
}

/// The `D0.L` result of the DOS call VMX68K.SX made, and the bytes it read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeReply {
    pub result: u32,
    pub data: Vec<u8>,
}

/// Bytes for VMX68K.SX's buffer, at an offset into it.
type BufferBytes = (u32, Vec<u8>);

/// Bytes of VMX68K.SX's buffer where `Files` puts the path, after the 53-byte search buffer.
const FILES_PATH: u32 = 64;
/// Bytes of VMX68K.SX's buffer where `Rename` puts the new name, after the old one.
const RENAME_TO: u32 = 256;

/// Returns a [`VmBridge`] and the requests it will make, for [`X68kBus::attach_vm_bridge`].
pub fn vm_bridge() -> (VmBridge, Receiver<BridgeRequest>) {
    let (requests, queue) = mpsc::channel();
    (
        VmBridge {
            requests,
            floppies: Vec::new(),
        },
        queue,
    )
}

impl VmBridge {
    /// Asks VMX68K.SX to carry out `op` and waits for the reply.
    pub fn ask(&self, op: BridgeOp) -> Option<BridgeReply> {
        let (reply, answer) = mpsc::channel();
        self.requests.send(BridgeRequest { op, reply }).ok()?;
        // A closed VM drops its requests, and with them this reply.
        answer.recv().ok()?
    }
}

/// Live media state, shared without transferring the image between VM threads.
#[derive(Debug)]
pub(crate) struct PassthroughMedia(AtomicU64);

impl PassthroughMedia {
    pub(crate) fn new() -> Self {
        Self(AtomicU64::new(0))
    }
    pub(crate) fn update(&self, len: usize, protected: bool) {
        let generation = ((self.0.load(Ordering::Relaxed) >> 33) + 1) & 0x7FFF_FFFF;
        self.0.store(
            len as u64 | ((protected as u64) << 32) | (generation << 33),
            Ordering::Release,
        );
    }
    fn len(&self) -> usize {
        self.0.load(Ordering::Acquire) as u32 as usize
    }
    fn protected(&self) -> bool {
        self.0.load(Ordering::Acquire) & (1 << 32) != 0
    }
}

/// An image or floppy unit the primary keeps, accessed by a VM's FDC or SCSI through
/// its [`VmBridge`].
#[derive(Debug, Clone)]
pub struct RemoteDisk {
    image: Option<Arc<Mutex<FloppyImage>>>,
    len: usize,
    bridge: VmBridge,
    /// The primary's file handle of the image, or `None` for drive 0's, which VMX68K.SX keeps.
    handle: Option<u32>,
    floppy: Option<(u32, Arc<PassthroughMedia>)>,
}

impl RemoteDisk {
    pub fn new(bridge: VmBridge, len: usize) -> Self {
        Self {
            len,
            bridge,
            handle: None,
            floppy: None,
            image: None,
        }
    }

    /// An image VMX68K.SX opened as the primary's file `handle`, of `len` bytes.
    pub fn with_handle(bridge: VmBridge, handle: u32, len: usize) -> Self {
        Self {
            len,
            bridge,
            handle: Some(handle),
            floppy: None,
            image: None,
        }
    }

    pub fn with_image(bridge: VmBridge, handle: u32, len: usize, image: FloppyImage) -> Self {
        let mut disk = Self::with_handle(bridge, handle, len);
        disk.image = Some(Arc::new(Mutex::new(image)));
        disk
    }

    pub(crate) fn image_geometry(&self) -> Option<crate::fdc::FloppyGeometry> {
        self.image
            .as_ref()
            .map(|image| image.lock().unwrap().geometry)
    }

    /// Size of the sector image, independent of the file's container.
    pub fn len(&self) -> usize {
        if let Some(image) = &self.image {
            return image.lock().unwrap().len();
        }
        self.floppy
            .as_ref()
            .map_or(self.len, |(_, media)| media.len())
    }

    pub(crate) fn is_passthrough(&self) -> bool {
        self.floppy.is_some()
    }
    pub(crate) fn media_version(&self) -> Option<u64> {
        self.floppy
            .as_ref()
            .map(|(_, media)| media.0.load(Ordering::Acquire))
    }
    pub(crate) fn write_protected(&self) -> bool {
        self.floppy
            .as_ref()
            .is_some_and(|(_, media)| media.protected())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads an image range, splitting transfers to fit VMX68K.X's 32 KB buffer.
    pub(crate) fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        if let Some(image) = &self.image {
            return image.lock().unwrap().read(self, offset, len);
        }
        if offset.checked_add(len)? > self.len() {
            return None;
        }
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            let count = (len - data.len()).min(32768);
            data.extend(self.read_chunk(offset + data.len(), count)?);
        }
        Some(data)
    }

    fn read_chunk(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let (offset, count) = (u32::try_from(offset).ok()?, u32::try_from(len).ok()?);
        let (op, done) = match (self.floppy.as_ref(), self.handle) {
            (Some((unit, _)), _) => (
                BridgeOp::FloppyRead {
                    unit: *unit,
                    offset,
                    len: count,
                },
                count,
            ),
            (_, Some(handle)) => (
                BridgeOp::HandleRead {
                    handle,
                    offset,
                    len: count,
                },
                count,
            ),
            _ => (BridgeOp::Read { offset, len: count }, 0),
        };
        self.bridge
            .ask(op)
            .filter(|reply| reply.result == done && reply.data.len() == len)
            .map(|reply| reply.data)
    }

    /// Writes through in chunks; a failed transfer may have written earlier chunks.
    pub(crate) fn write(&self, offset: usize, data: Vec<u8>) -> bool {
        if let Some(image) = &self.image {
            return image.lock().unwrap().write(self, offset, data);
        }
        if !offset
            .checked_add(data.len())
            .is_some_and(|end| end <= self.len())
        {
            return false;
        }
        data.chunks(32768)
            .enumerate()
            .all(|(i, bytes)| self.write_chunk(offset + i * 32768, bytes.to_vec()))
    }

    fn write_chunk(&self, offset: usize, data: Vec<u8>) -> bool {
        let Ok(offset) = u32::try_from(offset) else {
            return false;
        };
        let (op, done) = match (self.floppy.as_ref(), self.handle) {
            (Some((unit, _)), _) => {
                let len = data.len() as u32;
                (
                    BridgeOp::FloppyWrite {
                        unit: *unit,
                        offset,
                        data,
                    },
                    len,
                )
            }
            (_, Some(handle)) => {
                let len = data.len() as u32;
                (
                    BridgeOp::HandleWrite {
                        handle,
                        offset,
                        data,
                    },
                    len,
                )
            }
            _ => (BridgeOp::Write { offset, data }, 0),
        };
        self.bridge
            .ask(op)
            .is_some_and(|reply| reply.result == done)
    }
}

/// A change VMX68K.SX made to the set of VMs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmEvent {
    /// A VM was asked for, to be shown in the window at `window`, with a disk image of
    /// `drive0_bytes` for drive 0 (0 for none) that VMX68K.SX reads and writes for it, the
    /// primary's `drives`, from A:, that it can see in shared mode, and from its setup the
    /// `speed` to start at and its own SRAM, if the setup has one yet.
    Opened {
        id: u32,
        window: u32,
        drive0_bytes: u32,
        drives: u32,
        speed: CpuSpeedMode,
        sram: Option<Vec<u8>>,
        /// The CPU and the RAM in MB, from the setup.
        cpu: CpuModel,
        ram_mb: u32,
        /// Custom mode's drives, or `None` in shared mode.
        custom: Option<Vec<CustomDrive>>,
    },
    /// VMX68K.SX set the VM up anew, to restart it at once with its CPU, RAM, speed, and drives.
    Restart {
        id: u32,
        cpu: CpuModel,
        ram_mb: u32,
        speed: CpuSpeedMode,
        custom: Option<Vec<CustomDrive>>,
    },
    Media {
        id: u32,
        unit: u32,
        handle: u32,
        len: u32,
    },
    Boot {
        id: u32,
        choice: u32,
        rom: u32,
    },
    /// VMX68K.SX chose a speed for the VM from its menu.
    Speed { id: u32, mode: CpuSpeedMode },
    SpriteOptions { id: u32, options: crate::sprite::SpriteOptions },
    /// VMX68K.SX pressed the VM's RESET button.
    Reset { id: u32 },
    /// VMX68K.SX pressed the VM's INTERRUPT (NMI) button.
    Interrupt { id: u32 },
    /// VMX68K.SX toggled or remapped keyboard-to-joypad input for VM `id`.
    Joypad {
        id: u32,
        enabled: bool,
        /// Scancodes for `[Up, Down, Left, Right, A, B]`.
        map: [u8; 6],
    },
    /// The VM was closed.
    Closed { id: u32 },
}

impl ImageStorage for RemoteDisk {
    fn flush_layout(&self) -> bool {
        self.handle.is_some_and(|handle| {
            self.bridge
                .ask(BridgeOp::ImageSync { handle })
                .is_some_and(|reply| reply.result == 0)
        })
    }
    fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let mut data = Vec::with_capacity(len);
        while data.len() < len {
            let count = (len - data.len()).min(32768);
            data.extend(self.read_chunk(offset + data.len(), count)?);
        }
        Some(data)
    }
    fn write_at(&self, offset: usize, data: Vec<u8>) -> bool {
        data.chunks(32768)
            .enumerate()
            .all(|(i, bytes)| self.write_chunk(offset + i * 32768, bytes.to_vec()))
    }
}

/// A drive of a VM in custom mode, as VMX68K.SX set it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomDrive {
    /// An image file VMX68K.SX opened, read and written through `handle`, of `len` bytes.
    File { handle: u32, len: u32 },
    /// A container format identified by the shared image parser.
    Image {
        handle: u32,
        len: u32,
        image: FloppyImage,
    },
    /// An image file VMX68K.SX could not open, an empty drive.
    Missing,
    /// The primary's drive `drive` (1 is A:), file-level.
    Shared { drive: u32 },
    /// The primary's floppy drive `drive` (1 is A:), sector-level.
    Passthrough { drive: u32 },
}

/// Bytes of an entry of the drive table `VmOpen` and `VmRestart` take in `A2.L`, after its
/// count (a word): its kind (0 legacy file, 1 shared, 2 passthrough, 3 probed image), its handle or drive (-1 for a
/// file that did not open), and the file's size.
const DRIVE_TABLE_ENTRY: u32 = 8;

/// Menu order: STD, FD0..FD3, SCSI0..SCSI7, ROM.
pub fn boot_choice(sram: &[u8]) -> u32 {
    let word = u16::from_be_bytes([sram[0x18], sram[0x19]]);
    let rom = u32::from_be_bytes(sram[12..16].try_into().unwrap());
    if word == 0 {
        0
    } else if word & 0xF000 == 0x9000 {
        1 + ((word >> 8) & 3) as u32
    } else if (word == 1 || word == 0xA000) && (0xFC0000..=0xFC001C).contains(&rom) && rom % 4 == 0
    {
        5 + (rom - 0xFC0000) / 4
    } else {
        13
    }
}

pub fn apply_boot_choice(sram: &mut [u8], choice: u32, rom: u32) -> bool {
    let word: u16 = match choice {
        0 => 0,
        1..=4 => 0x9070 + ((choice as u16 - 1) << 8),
        5..=12 => {
            sram[12..16].copy_from_slice(&(0xFC0000 + (choice - 5) * 4).to_be_bytes());
            0xA000
        }
        13 if rom <= 0xFFFFFF && rom & 1 == 0 => {
            sram[12..16].copy_from_slice(&rom.to_be_bytes());
            0xA000
        }
        _ => return false,
    };
    sram[0x18..0x1A].copy_from_slice(&word.to_be_bytes());
    true
}

impl X68kBus {
    /// Called on a VM thread, after any earlier FDC command finished.
    pub fn change_vm_floppy(
        &mut self,
        unit: usize,
        disk: Option<RemoteDisk>,
        bridge: &VmBridge,
    ) -> bool {
        let Some(drive) = self.fdc.drives.get(unit) else {
            return false;
        };
        let source = drive
            .remote
            .as_ref()
            .and_then(|disk| disk.floppy.as_ref().map(|(unit, _)| *unit));
        let ok = if disk.is_none() && source.is_some() {
            bridge
                .ask(BridgeOp::FloppyEject {
                    unit: source.unwrap(),
                })
                .is_some_and(|reply| reply.result == 0)
        } else {
            let ok = if let Some(disk) = disk {
                self.fdc.insert_remote_disk(unit, disk, false)
            } else {
                self.fdc.eject_disk(unit);
                true
            };
            if ok {
                self.fdc.media_changed(unit);
                if let Some(unit) = source {
                    bridge.ask(BridgeOp::FloppyRelease { unit });
                }
            }
            ok
        };
        bridge.ask(BridgeOp::MediaDone { unit: unit as u32 });
        ok
    }

    pub fn floppy_state(&self) -> u32 {
        self.fdc
            .drives
            .iter()
            .enumerate()
            .fold(0, |bits, (unit, drive)| {
                bits | ((drive.connected as u32) << unit)
                    | ((drive.is_inserted() as u32) << (4 + unit))
                    | ((drive
                        .remote
                        .as_ref()
                        .is_some_and(RemoteDisk::is_passthrough) as u32)
                        << (12 + unit))
            })
    }

    /// Open VM records and storage resources, for lifecycle diagnostics.
    pub fn vm_resources(&self) -> (Vec<(u32, u32)>, usize, usize, usize) {
        (
            self.vm_windows
                .open
                .iter()
                .map(|vm| (vm.id, vm.window))
                .collect(),
            self.vm_windows.disks.len(),
            self.vm_windows.passthrough.len(),
            self.vm_windows
                .disks
                .iter()
                .map(|(_, disk)| disk.pending.len())
                .sum(),
        )
    }

    pub fn next_vm_id(&self) -> u32 {
        self.vm_windows.next_id + 1
    }
    pub fn set_vm_floppies(&mut self, id: u32, packed: u32, boot: u32) {
        if let Some(open) = self.vm_windows.open.iter_mut().find(|open| open.id == id) {
            open.status.floppies = packed;
            open.status.boot = boot;
        }
    }

    pub(crate) fn hypercall_vm_media(&mut self, state: &mut CpuState) -> TrapResult {
        let (id, unit, handle, len) = (state.d[1], state.d[2], state.d[3], state.d[4]);
        let valid = self.vm_windows.open.iter().any(|open| {
            open.id == id && open.custom && unit < 4 && open.status.floppies & (1 << unit) != 0
        }) && (handle == u32::MAX
            || self.vm_windows.image_formats.contains_key(&handle)
            || crate::fdc::FloppyGeometry::from_image_len(len as usize).is_some());
        state.d[0] = if valid {
            self.vm_windows.events.push(VmEvent::Media {
                id,
                unit,
                handle,
                len,
            });
            0
        } else {
            u32::MAX
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    pub fn take_vm_image(&mut self, handle: u32) -> Option<FloppyImage> {
        self.vm_windows.image_formats.remove(&handle)
    }

    /// Probe a DOS file using a task-supplied prefix. No bridge request or host
    /// file access is made on the primary thread. New formats only extend probe().
    pub(crate) fn hypercall_vm_image(&mut self, state: &mut CpuState) -> TrapResult {
        let (handle, len, size) = (state.d[1], state.d[2] as usize, state.d[3].min(32768));
        self.vm_windows.image_formats.remove(&handle);
        let mut prefix = if size == 0 {
            Vec::new()
        } else {
            self.vm_windows
                .image_prefixes
                .remove(&handle)
                .unwrap_or_default()
        };
        self.vm_windows.image_prefixes.remove(&handle);
        prefix.extend((0..size).map(|i| self.peek_u8(state.a[0] + i).unwrap_or(0)));
        match FloppyImage::probe(len, &prefix) {
            Probe::Need(required) if required > prefix.len() && required <= len => {
                state.d[0] = 2;
                state.d[1] = (required - prefix.len()).min(32768) as u32;
                state.d[2] = prefix.len() as u32;
                self.vm_windows.image_prefixes.insert(handle, prefix);
            }
            Probe::Need(_) | Probe::Unsupported => {
                state.d[0] = 0;
            }
            Probe::Image(image) => {
                self.vm_windows.image_formats.insert(handle, image);
                state.d[0] = 1;
            }
        }
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Check for floppy viruses and repair IPL.
    /// D1: action (0 detect, 1 repair ForceBoot, 2 repair clean IPL).
    pub(crate) fn hypercall_vm_virus(&mut self, state: &mut CpuState) -> TrapResult {
        let action = state.d[1];
        match action {
            0 => {
                // Detect virus
                // a0: buffer, d2: length, d3: is_dim, a1: output buffer for Japanese name
                let buf_addr = state.a[0];
                let len = (state.d[2] as usize).min(8192);
                let is_dim = state.d[3] != 0;
                let sec1_off = if is_dim { 256 } else { 0 };
                if len < sec1_off + 1024 {
                    state.d[0] = 0;
                    state.d[1] = 0;
                    return TrapResult::Handled { extra_cycles: 0 };
                }
                let mut ipl = [0u8; 1024];
                for i in 0..1024 {
                    ipl[i] = self.peek_u8(buf_addr + (sec1_off + i) as u32).unwrap_or(0);
                }
                match crate::virus::detect_ipl_virus(&ipl) {
                    None => {
                        state.d[0] = 0;
                        state.d[1] = 0;
                    }
                    Some(virus) => {
                        state.d[0] = match virus {
                            crate::virus::VirusType::ForceBoot => 1,
                            crate::virus::VirusType::Hodson => 2,
                            crate::virus::VirusType::Scheck => 3,
                            crate::virus::VirusType::Surgeon => 4,
                            crate::virus::VirusType::Unknown => 5,
                        };
                        state.d[1] = if virus.can_self_repair_from_sector4() { 1 } else { 0 };
                        let name_bytes = virus.name_ja_sjis();
                        let out_addr = state.a[1];
                        for (i, &b) in name_bytes.iter().enumerate() {
                            self.poke_u8(out_addr + i as u32, b);
                        }
                        self.poke_u8(out_addr + name_bytes.len() as u32, 0);
                    }
                }
                TrapResult::Handled { extra_cycles: 0 }
            }
            1 => {
                // Repair ForceBoot from Sector 4
                // a0: 4096-byte buffer containing sectors 1..4, d3: is_dim
                let buf_addr = state.a[0];
                let is_dim = state.d[3] != 0;
                let sec1_off = if is_dim { 256 } else { 0 };
                let sec2_off = sec1_off + 1024;
                let sec4_off = sec1_off + 3072;

                // Read 640 bytes from sector 4 and copy to sector 1
                for i in 0..640 {
                    let b = self.peek_u8(buf_addr + (sec4_off + i) as u32).unwrap_or(0);
                    self.poke_u8(buf_addr + (sec1_off + i) as u32, b);
                }

                // If sector 2 has FAT12 header, copy sector 2 (1024 bytes) to sector 4
                let m1 = self.peek_u8(buf_addr + (sec2_off + 1) as u32).unwrap_or(0);
                let m2 = self.peek_u8(buf_addr + (sec2_off + 2) as u32).unwrap_or(0);
                if m1 == 0xFF && m2 == 0xFF {
                    for i in 0..1024 {
                        let b = self.peek_u8(buf_addr + (sec2_off + i) as u32).unwrap_or(0);
                        self.poke_u8(buf_addr + (sec4_off + i) as u32, b);
                    }
                }

                state.d[0] = 0;
                TrapResult::Handled { extra_cycles: 0 }
            }
            2 => {
                // Repair from clean IPL
                // a0: target buffer, d2: target is_dim
                // a1: clean buffer, d3: clean is_dim
                let target_addr = state.a[0];
                let clean_addr = state.a[1];
                let target_sec1 = if state.d[2] != 0 { 256 } else { 0 };
                let clean_sec1 = if state.d[3] != 0 { 256 } else { 0 };

                for i in 0..1024 {
                    let b = self.peek_u8(clean_addr + (clean_sec1 + i) as u32).unwrap_or(0);
                    self.poke_u8(target_addr + (target_sec1 + i) as u32, b);
                }

                state.d[0] = 0;
                TrapResult::Handled { extra_cycles: 0 }
            }
            _ => {
                state.d[0] = u32::MAX;
                TrapResult::Handled { extra_cycles: 0 }
            }
        }
    }

    pub(crate) fn hypercall_vm_boot(&mut self, state: &mut CpuState) -> TrapResult {
        let (id, choice, rom) = (state.d[1], state.d[2], state.a[1]);
        // 0 creates a new setup; -1 edits a staged SRAM buffer without changing
        // any live VM. The latter preserves unrelated settings on normal OK.
        if id == 0 || id == u32::MAX {
            let mut defaults = if id == u32::MAX {
                (0..VM_SRAM_BYTES as u32)
                    .map(|i| self.peek_u8(state.a[0] + i).unwrap_or(0))
                    .collect()
            } else {
                X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config())
                    .sram()
                    .to_vec()
            };
            if id == 0 {
                defaults[8..12].copy_from_slice(&(state.d[3].clamp(1, 12) << 20).to_be_bytes());
            }
            if apply_boot_choice(&mut defaults, choice, rom) {
                for (i, &byte) in defaults.iter().enumerate() {
                    self.poke_u8(state.a[0] + i as u32, byte);
                }
                state.d[0] = 0;
            } else {
                state.d[0] = u32::MAX;
            }
        } else if let Some(open) = self
            .vm_windows
            .open
            .iter_mut()
            .find(|open| open.id == id && open.custom)
        {
            if choice == u32::MAX {
                state.d[0] = open.status.boot;
            } else if let Some(mut sram) = open.runtime_sram.clone() {
                if apply_boot_choice(&mut sram, choice, rom) {
                    open.status.boot = choice;
                    open.runtime_sram = Some(sram.clone());
                    open.sram = Some(sram);
                    open.status.sram = true;
                    self.vm_windows
                        .events
                        .push(VmEvent::Boot { id, choice, rom });
                    state.d[0] = 0;
                } else {
                    state.d[0] = u32::MAX;
                }
            } else {
                state.d[0] = u32::MAX;
            }
        } else {
            state.d[0] = u32::MAX;
        }
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Installs a custom setup's floppy/HDD images and file-level shares.
    /// FDD units, SCSI IDs, and shares each follow row order independently, with
    /// shared drives appended after the physical disks. Missing images reserve
    /// empty floppy units. Returns unsupported rows or images beyond the limits.
    /// Mounting performs no I/O and preserves the setup's SRAM boot choice.
    pub fn install_custom_drives(
        &mut self,
        bridge: VmBridge,
        drives: &[CustomDrive],
    ) -> Vec<CustomDrive> {
        let mut unit = 0;
        let mut target = 0;
        let mut unsupported = Vec::new();
        let mut shared = Vec::new();
        let mut passed = Vec::new();
        for drive in drives.iter().cloned() {
            match drive {
                CustomDrive::Image {
                    handle,
                    len,
                    ref image,
                } if unit < 4 => {
                    self.fdc.insert_remote_disk(
                        unit,
                        RemoteDisk::with_image(bridge.clone(), handle, len as usize, image.clone()),
                        false,
                    );
                    unit += 1;
                }
                CustomDrive::File { handle, len }
                    if crate::fdc::FloppyGeometry::from_image_len(len as usize).is_some()
                        && unit < 4 =>
                {
                    self.fdc.insert_remote_disk(
                        unit,
                        RemoteDisk::with_handle(bridge.clone(), handle, len as usize),
                        false,
                    );
                    unit += 1;
                }
                CustomDrive::File { handle, len }
                    if crate::fdc::FloppyGeometry::from_image_len(len as usize).is_none()
                        && self.insert_remote_scsi_hdd(
                            target,
                            RemoteDisk::with_handle(bridge.clone(), handle, len as usize),
                        ) =>
                {
                    target += 1;
                }
                CustomDrive::Passthrough { drive: letter } if unit < 4 => {
                    if !passed.contains(&letter) {
                        if let Some((_, source, media)) = bridge
                            .floppies
                            .iter()
                            .find(|(drive, _, _)| *drive == letter)
                        {
                            let disk = RemoteDisk {
                                len: 0,
                                image: None,
                                bridge: bridge.clone(),
                                handle: None,
                                floppy: Some((*source, Arc::clone(media))),
                            };
                            self.fdc.insert_remote_disk(unit, disk, false);
                            passed.push(letter);
                        } else {
                            unsupported.push(drive);
                        }
                    } else {
                        unsupported.push(drive);
                    }
                    unit += 1;
                }
                CustomDrive::Missing if unit < 4 => unit += 1,
                CustomDrive::Shared { drive } if (1..=26).contains(&drive) => shared.push(drive),
                _ => unsupported.push(drive),
            }
        }
        // Real machines have two or four units, including empty drives.
        let count = if unit > 2 { 4 } else { 2 };
        for (i, drive) in self.fdc.drives.iter_mut().enumerate() {
            drive.connected = i < count;
        }
        if !shared.is_empty() {
            self.install_remote_drives(bridge, shared);
        }
        unsupported
    }

    /// Reads the drive table at `at`, or `None` for shared mode (0).
    fn custom_drives(&mut self, at: u32) -> Option<Vec<CustomDrive>> {
        // A2 was unspecified before custom drives existed. Do not interpret an
        // older guest's leftover address as a drive table (even an empty one).
        if at >> 24 != 0x56 {
            return None;
        }
        let at = at & 0x00FF_FFFF;
        if at == 0 {
            return None;
        }
        let count = self.peek_u16(at).unwrap_or(0).min(8) as u32;
        let drives = (0..count)
            .map(|row| {
                let entry = at + 2 + row * DRIVE_TABLE_ENTRY;
                let kind = self.peek_u16(entry).unwrap_or(0);
                let arg = self.peek_u16(entry + 2).unwrap_or(0xFFFF);
                let len = self.peek_u32(entry + 4).unwrap_or(0);
                match (kind, arg) {
                    (0, 0xFFFF) => CustomDrive::Missing,
                    (0, handle) => CustomDrive::File { handle: handle as u32, len,
                    },
                    (3, handle) => match self.vm_windows.image_formats.remove(&(handle as u32)) {
                        Some(image) => CustomDrive::Image {
                            handle: handle as u32,
                            len,
                            image,
                        },
                        None => CustomDrive::Missing,
                    },
                    (1, drive) => CustomDrive::Shared { drive: drive as u32 },
                    (_, drive) => CustomDrive::Passthrough { drive: drive as u32 },
                }
            })
            .collect();
        Some(drives)
    }
}

/// Speeds of a VM's CPU by their order in VMX68K.SX's menu, the numbers `VmSpeed` takes.
pub fn vm_speeds(cpu: CpuModel) -> &'static [CpuSpeedMode] {
    match cpu {
        CpuModel::Mc68000 => &[
            CpuSpeedMode::Mhz10,
            CpuSpeedMode::Mhz16,
            CpuSpeedMode::Mhz24,
            CpuSpeedMode::Burst,
        ],
        CpuModel::Mc68030 => &[CpuSpeedMode::Mhz25, CpuSpeedMode::Burst],
    }
}

/// A setup's CPU and RAM as `VmOpen` and `VmRestart` take them: the CPU (0 the 68000, 1 the
/// 68030) in the high word and the RAM in MB in the low, 1 to 12, 0 for the 4 MB of setups
/// that do not say.
fn setup_machine(word: u32) -> (CpuModel, u32) {
    let cpu = if word >> 16 == 1 { CpuModel::Mc68030 } else { CpuModel::Mc68000 };
    let ram_mb = match word & 0xFFFF {
        0 => 4,
        mb => mb.min(12),
    };
    (cpu, ram_mb)
}

/// The speed numbered `index` of `cpu`, or its first.
fn vm_speed(cpu: CpuModel, index: u32) -> CpuSpeedMode {
    let speeds = vm_speeds(cpu);
    speeds.get(index as usize).copied().unwrap_or(speeds[0])
}

/// Bytes of a VM's SRAM, which VMX68K.SX keeps in the VM's setup.
pub const VM_SRAM_BYTES: usize = 16 * 1024;

/// A VM VMX68K.SX has open.
#[derive(Debug, Clone)]
struct OpenVm {
    id: u32,
    /// Its window record.
    window: u32,
    /// Lines of the window's content above the VM's screen, VMX68K.SX's icon bar.
    screen_top: i16,
    /// The VM's CPU, whose speeds `VmSpeed` numbers.
    cpu: CpuModel,
    /// What the app reports of the VM for VMX68K.SX's window, and what `VmStatus` last told.
    status: VmStatus,
    status_told: VmStatus,
    /// Whether VMX68K.SX has been sent `E_USER1` for a status it has not asked about yet.
    status_woken: bool,
    /// The VM's SRAM as it last changed, for VMX68K.SX to save with `VmSram`.
    sram: Option<Vec<u8>>,
    runtime_sram: Option<Vec<u8>>,
    sprite_options: crate::sprite::SpriteOptions,
    custom: bool,
}

/// The state of a VM that VMX68K.SX shows: its access lamp and its screen's size.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct VmStatus {
    lamp: bool,
    /// Whether the VM's SRAM changed and is to be saved.
    sram: bool,
    /// Whether the primary's screen changed size, so that the window is to be drawn anew.
    redraw: bool,
    /// Width and height, `(0, 0)` until the VM shows a frame.
    screen: (u16, u16),
    floppies: u32,
    boot: u32,
}

/// `VmStatus` bit of a lit access lamp.
const STATUS_LAMP: u32 = 1;
/// `VmStatus` bit of SRAM to save, which `VmSram` hands over.
const STATUS_SRAM: u32 = 2;
/// `VmStatus` bit of a window to draw anew, for a screen that changed size.
const STATUS_REDRAW: u32 = 4;

/// The VMs VMX68K.SX has open.
#[derive(Debug, Default)]
pub(crate) struct VmWindows {
    next_id: u32,
    open: Vec<OpenVm>,
    events: Vec<VmEvent>,
    /// Bridge requests of each VM, and the one VMX68K.SX is carrying out with its buffer.
    disks: Vec<(u32, VmDisk)>,
    passthrough: Vec<FloppyLease>,
    image_formats: HashMap<u32, FloppyImage>,
    image_prefixes: HashMap<u32, Vec<u8>>,
}

#[derive(Debug)]
struct FloppyLease {
    owner: u32,
    drive: u32,
    unit: u32,
    media: Arc<PassthroughMedia>,
}

#[derive(Debug)]
struct VmDisk {
    requests: Receiver<BridgeRequest>,
    /// Requests taken from the channel and not answered yet, oldest first.
    pending: VecDeque<BridgeRequest>,
    /// Buffer address and size that `VmPoll` was given for the request at the front.
    buffer: Option<(u32, u32)>,
    /// Whether VMX68K.SX has been sent `E_USER1` for requests it has not polled for yet.
    woken: bool,
}

/// Where a VM's window shows on the primary VM's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmWindowLayout {
    /// Screen position of the top-left of the VM's screen in the window's content.
    pub left: i32,
    pub top: i32,
    /// Visible parts of the content, as screen rectangles `(left, top, right, bottom)`, right
    /// and bottom exclusive.
    pub visible: Vec<(i32, i32, i32, i32)>,
}

impl X68kBus {
    /// Returns SX-Window's active window, the one that gets the keyboard, once it runs.
    pub fn active_window(&self) -> Option<u32> {
        let window = self.peek_u32(self.native_gm_text_base? + ACTIVE_WINDOW)?;
        (window != 0).then_some(window & 0x00FF_FFFF)
    }

    /// Resolves the primary's drive letter through Human68k's DPB chain. Only
    /// DISK2HD units are physical floppy drives; SCSI and remote devices cannot pass.
    fn primary_floppy_unit(&self, drive: u32) -> Option<u32> {
        if !(1..=26).contains(&drive) {
            return None;
        }
        let mut dpb = self.peek_u32(0x1C3C)?;
        for _ in 0..26 {
            if dpb >= self.main_ram().len() as u32 {
                return None;
            }
            if self.peek_u8(dpb)? as u32 + 1 == drive {
                let header = self.peek_u32(dpb + 2)?;
                let name: Vec<_> = (15..22).filter_map(|i| self.peek_u8(header + i)).collect();
                let unit = self.peek_u8(dpb + 1)? as u32;
                return (name == b"DISK2HD" && unit < 4).then_some(unit);
            }
            dpb = self.peek_u32(dpb + 6)?;
        }
        None
    }

    fn reserve_passthrough(&mut self, id: u32, drives: &[CustomDrive]) {
        let mut slots = 0;
        for drive in drives {
            match drive {
                CustomDrive::Missing | CustomDrive::Image { .. } => slots += 1,
                CustomDrive::File { len, .. }
                    if crate::fdc::FloppyGeometry::from_image_len(*len as usize).is_some() =>
                {
                    slots += 1
                }
                _ => {}
            }
            let CustomDrive::Passthrough { drive } = *drive else {
                continue;
            };
            slots += 1;
            if slots > 4 {
                continue;
            }
            let Some(unit) = self.primary_floppy_unit(drive) else {
                continue;
            };
            if self
                .vm_windows
                .passthrough
                .iter()
                .any(|lease| lease.unit == unit)
            {
                continue;
            }
            let source = &mut self.fdc.drives[unit as usize];
            if !source.connected || source.remote.is_some() {
                continue;
            }
            let media = Arc::new(PassthroughMedia::new());
            source.passthrough_media = Some(Arc::clone(&media));
            source.publish_media();
            self.vm_windows.passthrough.push(FloppyLease {
                owner: id,
                drive,
                unit,
                media,
            });
        }
    }

    /// Binds reserved physical drives to the VM's existing disk bridge.
    pub fn bind_vm_passthrough(&self, id: u32, bridge: &mut VmBridge) {
        bridge.floppies = self
            .vm_windows
            .passthrough
            .iter()
            .filter(|lease| lease.owner == id)
            .map(|lease| (lease.drive, lease.unit, Arc::clone(&lease.media)))
            .collect();
    }

    fn release_passthrough(&mut self, id: u32) {
        for lease in self
            .vm_windows
            .passthrough
            .iter()
            .filter(|lease| lease.owner == id)
        {
            // An old VM must see no media even if another VM claims the same unit.
            lease.media.update(0, true);
            self.fdc.drives[lease.unit as usize].passthrough_media = None;
        }
        self.vm_windows
            .passthrough
            .retain(|lease| lease.owner != id);
    }

    fn passthrough_request(&mut self, id: u32, op: &BridgeOp) -> Option<BridgeReply> {
        if let BridgeOp::FloppyEject { unit } | BridgeOp::FloppyRelease { unit } = op {
            let index = self
                .vm_windows
                .passthrough
                .iter()
                .position(|lease| lease.owner == id && lease.unit == *unit)?;
            if matches!(op, BridgeOp::FloppyEject { .. }) {
                self.eject_floppy(*unit as usize);
            } else {
                let lease = self.vm_windows.passthrough.remove(index);
                lease.media.update(0, true);
                self.fdc.drives[*unit as usize].passthrough_media = None;
            }
            return Some(BridgeReply {
                result: 0,
                data: vec![],
            });
        }

        let (unit, offset, len) = match op {
            BridgeOp::FloppyRead { unit, offset, len } => (*unit, *offset as usize, *len as usize),
            BridgeOp::FloppyWrite { unit, offset, data } => (*unit, *offset as usize, data.len()),
            _ => return None,
        };
        if !self
            .vm_windows
            .passthrough
            .iter()
            .any(|lease| lease.owner == id && lease.unit == unit)
        {
            return None;
        }
        let source = self.fdc.drives.get_mut(unit as usize)?;
        let sector = source.geometry.sector_bytes();
        if offset % sector != 0 || len % sector != 0 {
            return None;
        }
        let protected = source.write_protected;
        let bytes = source
            .image
            .as_mut()?
            .get_mut(offset..offset.checked_add(len)?)?;
        let data = match op {
            BridgeOp::FloppyRead { .. } => bytes.to_vec(),
            BridgeOp::FloppyWrite { data, .. } if !protected => {
                bytes.copy_from_slice(data);
                Vec::new()
            }
            _ => return None,
        };
        self.note_drive_access();
        Some(BridgeReply {
            result: len as u32,
            data,
        })
    }

    /// Lets VMX68K.SX carry out the bridge requests of VM `id`.
    pub fn attach_vm_bridge(&mut self, id: u32, requests: Receiver<BridgeRequest>) {
        let disk = VmDisk {
            requests,
            pending: VecDeque::new(),
            buffer: None,
            woken: false,
        };
        self.vm_windows.disks.retain(|(open, _)| *open != id);
        self.vm_windows.disks.push((id, disk));
    }

    /// Drops the bridge requests of VM `id`, which fails the ones its thread waits for.
    pub fn detach_vm_bridge(&mut self, id: u32) {
        self.vm_windows.disks.retain(|(open, _)| *open != id);
        self.release_passthrough(id);
    }

    /// The counter of this machine's drive accesses, floppy, SCSI, and remote, which the app
    /// reads from another thread to light a VM's access lamp ([`Self::set_vm_lamp`]).
    pub fn drive_access_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.fdc.drive_accesses)
    }

    /// Counts a drive access of this machine.
    pub(crate) fn note_drive_access(&self) {
        self.fdc.drive_accesses.fetch_add(1, Ordering::Relaxed);
    }

    /// Lights or puts out the access lamp of VM `id`, which VMX68K.SX shows on its bar. A
    /// change wakes it with `E_USER1` to ask for it with `VmStatus`.
    pub fn set_vm_lamp(&mut self, id: u32, lit: bool) {
        if let Some(open) = self.vm_windows.open.iter_mut().find(|open| open.id == id) {
            open.status.lamp = lit;
        }
    }

    /// Hands VMX68K.SX the SRAM of VM `id`, which changed, to save in the VM's setup. A change
    /// wakes it with `E_USER1`, and it takes the bytes with `VmSram`.
    pub fn set_vm_sram(&mut self, id: u32, sram: Vec<u8>) {
        if let Some(open) = self.vm_windows.open.iter_mut().find(|open| open.id == id) {
            open.runtime_sram = Some(sram.clone());
            open.sram = Some(sram);
            open.status.sram = true;
        }
    }

    /// Has VMX68K.SX draw each of its windows anew, after the primary's screen changed size:
    /// what was off the screen, which SX-Window did not draw, and its visible region, which
    /// the screen clipped, come back with them.
    pub(crate) fn redraw_vm_windows(&mut self) {
        for open in &mut self.vm_windows.open {
            open.status.redraw = true;
        }
    }

    /// Reports the size of the frames VM `id` shows, which VMX68K.SX sizes its window to. A
    /// change wakes it with `E_USER1` to ask for it with `VmStatus`.
    pub fn set_vm_screen_size(&mut self, id: u32, width: u32, height: u32) {
        if let Some(open) = self.vm_windows.open.iter_mut().find(|open| open.id == id) {
            open.status.screen = (width.min(0xFFFF) as u16, height.min(0xFFFF) as u16);
        }
    }

    /// Sends `E_USER1` to the window of each VM with disk requests VMX68K.SX has not polled
    /// for, or a lamp it has not asked about, so it wakes for them rather than polling on every
    /// turn. Called when the guest is about to take an event, when its queue is consistent.
    pub(crate) fn wake_vm_windows(&mut self) {
        let Some(text_base) = self.native_gm_text_base else {
            return;
        };
        for i in 0..self.vm_windows.open.len() {
            let open = &self.vm_windows.open[i];
            let (changed, window) = (open.status != open.status_told && !open.status_woken, open.window);
            if changed && self.post_event(text_base, E_USER1, window, (0, 0)) {
                self.vm_windows.open[i].status_woken = true;
            }
        }
        let mut wake = Vec::new();
        for (id, disk) in &mut self.vm_windows.disks {
            if disk.woken || disk.buffer.is_some() {
                continue;
            }
            while let Ok(request) = disk.requests.try_recv() {
                disk.pending.push_back(request);
            }
            if !disk.pending.is_empty() {
                if let Some(open) = self.vm_windows.open.iter().find(|open| open.id == *id) {
                    wake.push((*id, open.window));
                }
            }
        }
        for (id, window) in wake {
            if self.post_event(text_base, E_USER1, window, (0, 0)) {
                if let Some((_, disk)) = self.vm_windows.disks.iter_mut().find(|(open, _)| *open == id) {
                    disk.woken = true;
                }
            }
        }
    }

    /// Takes the VMs opened and closed since the last call, in order.
    pub fn take_vm_events(&mut self) -> Vec<VmEvent> {
        std::mem::take(&mut self.vm_windows.events)
    }

    /// `VmOpen`: records a VM for the window `D1.L`, with a drive 0 image of `D2.L` bytes that
    /// VMX68K.SX serves, `D3.L` of the primary's drives from A:, its screen `D4.W` lines below
    /// the top of the window's content, the 16 KB of its SRAM at `D5.L` (0 for none yet), and
    /// the speed `D6.L` (as `VmSpeed`'s), its CPU and RAM `D7.L` (`cpu << 16 | MB`, see
    /// [`setup_machine`]), and custom mode's drive table at `A2.L` (0 in shared mode, see
    /// [`CustomDrive`], with tag $56 in A2's high byte), and returns its ID in `D0.L`.
    pub(crate) fn hypercall_vm_open(&mut self, state: &mut CpuState) -> TrapResult {
        let windows = &mut self.vm_windows;
        windows.next_id += 1;
        let id = windows.next_id;
        let window = state.d[1] & 0x00FF_FFFF;
        let (cpu, ram_mb) = setup_machine(state.d[7]);
        windows.open.push(OpenVm {
            id,
            window,
            screen_top: state.d[4] as i16,
            cpu,
            status: VmStatus::default(),
            status_told: VmStatus::default(),
            status_woken: false,
            sram: None,
            runtime_sram: None,
            sprite_options: crate::sprite::SpriteOptions::default(),
            custom: state.a[2] >> 24 == 0x56 && state.a[2] & 0x00FF_FFFF != 0,
        });
        let sram_at = state.d[5] & 0x00FF_FFFF;
        let sram = (sram_at != 0).then(|| {
            (0..VM_SRAM_BYTES as u32)
                .map(|i| self.peek_u8(sram_at + i).unwrap_or(0))
                .collect()
        });
        let custom = self.custom_drives(state.a[2]);
        self.reserve_passthrough(id, custom.as_deref().unwrap_or(&[]));
        let windows = &mut self.vm_windows;
        windows.events.push(VmEvent::Opened {
            id,
            window,
            drive0_bytes: state.d[2],
            drives: state.d[3].min(26),
            speed: vm_speed(cpu, state.d[6]),
            sram,
            cpu,
            ram_mb,
            custom,
        });
        state.d[0] = id;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmPoll`: hands VMX68K.SX the next bridge request of VM `D1.L`, given its buffer at `D2.L`
    /// of `D3.L` bytes. Returns the request's kind in `D0.L` (see [`BridgeOp`]), 0 for none, or
    /// -1 for an unknown VM, its numbers in `D1.L`..`D3.L`, and its bytes in the buffer. A
    /// request stays handed out until `VmReply`.
    pub(crate) fn hypercall_vm_poll(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        if !self.vm_windows.open.iter().any(|open| open.id == id) {
            state.d[0] = u32::MAX;
            return TrapResult::Handled { extra_cycles: 0 };
        }
        let (buffer, size) = (state.d[2] & 0x00FF_FFFF, state.d[3]);
        let Some((_, disk)) = self.vm_windows.disks.iter_mut().find(|(open, _)| *open == id) else {
            state.d[0] = 0;
            return TrapResult::Handled { extra_cycles: 0 };
        };
        disk.woken = false;
        if disk.buffer.is_none() {
            while let Ok(request) = disk.requests.try_recv() {
                disk.pending.push_back(request);
            }
        }
        // One request at a time: the next one waits for the reply to this.
        let op = match disk.pending.front() {
            Some(request) if disk.buffer.is_none() => request.op.clone(),
            _ => {
                state.d[0] = 0;
                return TrapResult::Handled { extra_cycles: 0 };
            }
        };
        if matches!(
            op,
            BridgeOp::FloppyRead { .. } | BridgeOp::FloppyWrite { .. }
                | BridgeOp::FloppyEject { .. }
                | BridgeOp::FloppyRelease { .. }
        ) {
            let reply = self.passthrough_request(id, &op);
            let disk = &mut self
                .vm_windows
                .disks
                .iter_mut()
                .find(|(open, _)| *open == id)
                .unwrap()
                .1;
            if let Some(request) = disk.pending.pop_front() {
                let _ = request.reply.send(reply);
            }
            state.d[0] = 0;
            return TrapResult::Handled { extra_cycles: 0 };
        }
        let disk = &mut self
            .vm_windows
            .disks
            .iter_mut()
            .find(|(open, _)| *open == id)
            .unwrap()
            .1;
        // Numbers in D1..D3, bytes for the buffer, and how many bytes the reply brings back.
        let (numbers, bytes, reply_len): ([u32; 3], Vec<BufferBytes>, u32) = match &op {
            BridgeOp::Read { offset, len } => ([*offset, *len, 0], vec![], *len),
            BridgeOp::Write { offset, data } => {
                ([*offset, data.len() as u32, 0], vec![(0, data.clone())], 0)
            }
            BridgeOp::Open { path, mode } => ([*mode, 0, 0], vec![(0, nul_terminated(path))], 0),
            BridgeOp::Mkdir { path } | BridgeOp::Rmdir { path } | BridgeOp::Delete { path } => {
                ([0, 0, 0], vec![(0, nul_terminated(path))], 0)
            }
            BridgeOp::Rename { from, to } => ([0, 0, 0], vec![(0, nul_terminated(from)), (RENAME_TO, nul_terminated(to))], 0),
            BridgeOp::Chmod { path, attr } => ([*attr, 0, 0], vec![(0, nul_terminated(path))], 0),
            BridgeOp::Create { path, attr, replace } => ([*attr, *replace as u32, 0], vec![(0, nul_terminated(path))], 0),
            BridgeOp::HandleWrite { handle, offset, data } => ([*offset, data.len() as u32, *handle], vec![(0, data.clone())], 0),
            BridgeOp::Filedate { handle, datetime } => ([*datetime, 0, *handle], vec![], 0),
            BridgeOp::HandleRead { handle, offset, len } => ([*offset, *len, *handle], vec![], *len),
            BridgeOp::Close { handle } => ([0, 0, *handle], vec![], 0),
            BridgeOp::Files { attr, path } => ([*attr, 0, 0], vec![(0, vec![0; 53]), (FILES_PATH, nul_terminated(path))], 53),
            BridgeOp::Nfiles { buffer } => ([0, 0, 0], vec![(0, buffer.clone())], 53),
            BridgeOp::Dskfre { drive } => ([*drive, 0, 0], vec![], 8),
            BridgeOp::MediaDone { unit } => ([*unit, 0, 0], vec![], 0),
            BridgeOp::ImageSync { handle } => ([0, 0, *handle], vec![], 0),
            BridgeOp::FloppyRead { .. } | BridgeOp::FloppyWrite { .. }
            | BridgeOp::FloppyEject { .. }
            | BridgeOp::FloppyRelease { .. } => unreachable!(),
        };
        let needed = bytes
            .iter()
            .map(|(at, data)| at + data.len() as u32)
            .chain([reply_len])
            .max()
            .unwrap_or(0);
        if needed > size {
            // The buffer cannot hold it: fail the request rather than leave the VM waiting.
            if let Some(request) = disk.pending.pop_front() {
                let _ = request.reply.send(None);
            }
            state.d[0] = 0;
            return TrapResult::Handled { extra_cycles: 0 };
        }
        disk.buffer = Some((buffer, reply_len));
        for (at, data) in bytes {
            for (i, &byte) in data.iter().enumerate() {
                self.poke_u8(buffer + at + i as u32, byte);
            }
        }
        state.d[0] = op.kind();
        [state.d[1], state.d[2], state.d[3]] = numbers;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmReply`: VMX68K.SX carried out the request `VmPoll` handed out for VM `D1.L`, with the
    /// DOS result `D2.L` (0 for success of an image read or write). Bytes read are in the
    /// buffer. Returns 0, or -1 when no request was handed out.
    pub(crate) fn hypercall_vm_reply(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let result = state.d[2];
        let taken = self
            .vm_windows
            .disks
            .iter_mut()
            .find(|(open, _)| *open == id)
            .and_then(|(_, disk)| Some((disk.buffer.take()?, disk.pending.pop_front()?)));
        let Some(((buffer, reply_len), request)) = taken else {
            state.d[0] = u32::MAX;
            return TrapResult::Handled { extra_cycles: 0 };
        };
        // A handle read brings back as many bytes as it read; the others what their op says.
        let len = match request.op {
            BridgeOp::HandleRead { .. } if (result as i32) >= 0 => result.min(reply_len),
            BridgeOp::HandleRead { .. } => 0,
            _ => reply_len,
        };
        let data = (0..len).map(|i| self.peek_u8(buffer + i).unwrap_or(0)).collect();
        let _ = request.reply.send(Some(BridgeReply { result, data }));
        state.d[0] = 0;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmClose`: forgets the VM `D1.L`. Returns 0, or -1 for an unknown VM.
    pub(crate) fn hypercall_vm_close(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let windows = &mut self.vm_windows;
        let before = windows.open.len();
        windows.open.retain(|open| open.id != id);
        windows.disks.retain(|(open, _)| *open != id);
        state.d[0] = if windows.open.len() < before {
            windows.events.push(VmEvent::Closed { id });
            0
        } else {
            u32::MAX
        };
        self.release_passthrough(id);
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmSpeed`: sets VM `D1.L` to the speed `D2.L` of VMX68K.SX's menu, as [`vm_speeds`]
    /// numbers its CPU's (for the 68000 0 10 MHz, 1 16 MHz, 2 24 MHz, 3 Burst), which the app
    /// applies at once. Returns 0, or -1 for an unknown VM or speed.
    pub(crate) fn hypercall_vm_speed(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let cpu = self.vm_windows.open.iter().find(|open| open.id == id).map(|open| open.cpu);
        state.d[0] = match cpu.and_then(|cpu| vm_speeds(cpu).get(state.d[2] as usize)) {
            Some(&mode) => {
                self.vm_windows.events.push(VmEvent::Speed { id, mode });
                0
            }
            None => u32::MAX,
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    pub fn vm_sprite_options(&self, id: u32) -> crate::sprite::SpriteOptions {
        self.vm_windows
            .open
            .iter()
            .find(|open| open.id == id)
            .map_or(crate::sprite::SpriteOptions::default(), |open| {
                open.sprite_options
            })
    }

    /// D2=-1 queries; otherwise bits 0/1 enable unlimited sprites/full pattern memory.
    pub(crate) fn hypercall_vm_sprite_options(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        state.d[0] = match self.vm_windows.open.iter_mut().find(|open| open.id == id) {
            Some(open) if state.d[2] == u32::MAX => open.sprite_options.bits(),
            Some(open) if state.d[2] & !3 == 0 => {
                let options = crate::sprite::SpriteOptions::from_bits(state.d[2]);
                if options != open.sprite_options {
                    open.sprite_options = options;
                    self.vm_windows
                        .events
                        .push(VmEvent::SpriteOptions { id, options });
                }
                0
            }
            _ => u32::MAX,
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmRestart`: restarts VM `D1.L` with the CPU and RAM `D2.L` (as `VmOpen`'s `D7.L`), the
    /// speed `D3.L` of that CPU, and the drive table at `A2.L` (as `VmOpen`'s), keeping its
    /// window and SRAM. Returns 0, or -1 for an unknown VM.
    pub(crate) fn hypercall_vm_restart(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let (cpu, ram_mb) = setup_machine(state.d[2]);
        let custom = self.custom_drives(state.a[2]);
        state.d[0] = match self.vm_windows.open.iter_mut().find(|open| open.id == id) {
            Some(open) => {
                open.cpu = cpu;
                open.custom = custom.is_some();
                let speed = vm_speed(cpu, state.d[3]);
                // Retire the old requests before the task can serve another event
                // with its newly opened image handles.
                self.detach_vm_bridge(id);
                self.reserve_passthrough(id, custom.as_deref().unwrap_or(&[]));
                self.vm_windows.events.push(VmEvent::Restart { id, cpu, ram_mb, speed, custom });
                0
            }
            None => u32::MAX,
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmReset`: resets VM `D1.L`. Returns 0, or -1 for an unknown VM.
    pub(crate) fn hypercall_vm_reset(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        state.d[0] = if self.vm_windows.open.iter().any(|open| open.id == id) {
            self.vm_windows.events.push(VmEvent::Reset { id });
            0
        } else {
            u32::MAX
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmInterrupt`: triggers a level-7 NMI on VM `D1.L`. Returns 0, or -1 for an unknown VM.
    pub(crate) fn hypercall_vm_interrupt(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        state.d[0] = if self.vm_windows.open.iter().any(|open| open.id == id) {
            self.vm_windows.events.push(VmEvent::Interrupt { id });
            0
        } else {
            u32::MAX
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmJoypad`: sets keyboard-to-joypad mode (`D2.L` bit 0) and 6-byte scancode map
    /// `[Up, Down, Left, Right, A, B]` at `A0.L` for VM `D1.L`. Returns 0 or -1.
    pub(crate) fn hypercall_vm_joypad(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let enabled = (state.d[2] & 1) != 0;
        let addr = state.a[0] & 0x00FF_FFFF;
        let mut map = [0u8; 6];
        for (i, slot) in map.iter_mut().enumerate() {
            *slot = self.peek_u8(addr + i as u32).unwrap_or(0);
        }
        state.d[0] = if self.vm_windows.open.iter().any(|open| open.id == id) {
            self.vm_windows.events.push(VmEvent::Joypad { id, enabled, map });
            0
        } else {
            u32::MAX
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmStatus`: tells VMX68K.SX the state of VM `D1.L`: in `D0.L` bit 0 its access lamp,
    /// bit 1 SRAM to save, and bit 2 a window to draw anew, or -1 for an unknown VM, and in `D1.L` its screen's width and height (`width << 16 |
    /// height`, 0 before its first frame).
    pub(crate) fn hypercall_vm_status(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let Some(open) = self.vm_windows.open.iter_mut().find(|open| open.id == id) else {
            state.d[0] = u32::MAX;
            return TrapResult::Handled { extra_cycles: 0 };
        };
        let status = open.status;
        // A redraw is asked for once.
        open.status.redraw = false;
        open.status_told = open.status;
        open.status_woken = false;
        state.d[0] = if status.lamp { STATUS_LAMP } else { 0 }
            | if status.sram { STATUS_SRAM } else { 0 }
            | if status.redraw { STATUS_REDRAW } else { 0 };
        state.d[1] = (status.screen.0 as u32) << 16 | status.screen.1 as u32;
        state.d[2] = if open.custom { status.floppies } else { 0 };
        state.d[3] = status.boot;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmSram`: copies the SRAM of VM `D1.L` that `VmStatus` reported to save into the buffer
    /// at `D2.L` of `D3.L` bytes, and returns its size, 0 when there is none, or -1 for an
    /// unknown VM.
    pub(crate) fn hypercall_vm_sram(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let (buffer, size) = (state.d[2] & 0x00FF_FFFF, state.d[3] as usize);
        let Some(open) = self.vm_windows.open.iter_mut().find(|open| open.id == id) else {
            state.d[0] = u32::MAX;
            return TrapResult::Handled { extra_cycles: 0 };
        };
        open.status.sram = false;
        open.status_told.sram = false;
        let sram = open.sram.take().unwrap_or_default();
        let len = sram.len().min(size);
        for (i, &byte) in sram[..len].iter().enumerate() {
            self.poke_u8(buffer + i as u32, byte);
        }
        state.d[0] = len as u32;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Reads where the window at `window` shows the VM's screen: its screen position, below
    /// VMX68K.SX's icon bar, and the visible parts of the content there. Returns `None` while
    /// the window is hidden.
    pub fn vm_window_layout(&self, window: u32) -> Option<VmWindowLayout> {
        if self.peek_u8(window + WINDOW_VISIBLE)? == 0 {
            return None;
        }
        let mut content = self.peek_rect(window + WINDOW_RECT)?;
        let screen_top = self
            .vm_windows
            .open
            .iter()
            .find(|open| open.window == window)
            .map_or(0, |open| open.screen_top);
        content.top = content.top.saturating_add(screen_top).min(content.bottom);
        // The bitmap's rectangle is the screen in window coordinates.
        let origin = self.peek_rect(self.peek_u32(window + WINDOW_BITMAP)? + BITMAP_RECT)?;
        let (dx, dy) = (-(origin.left as i32), -(origin.top as i32));
        let visible = self.peek_u32(self.peek_u32(window + WINDOW_VISIBLE_RGN)?)?;
        let rects = self
            .region_rects(visible)?
            .into_iter()
            .filter_map(|(left, top, right, bottom)| {
                let left = left.max(content.left);
                let top = top.max(content.top);
                let right = right.min(content.right);
                let bottom = bottom.min(content.bottom);
                (left < right && top < bottom).then_some((
                    left as i32 + dx,
                    top as i32 + dy,
                    right as i32 + dx,
                    bottom as i32 + dy,
                ))
            })
            .collect();
        Some(VmWindowLayout {
            left: content.left as i32 + dx,
            top: content.top as i32 + dy,
            visible: rects,
        })
    }
}

/// `path` with a NUL after it, as DOS calls take names.
fn nul_terminated(path: &[u8]) -> Vec<u8> {
    let mut bytes = path.to_vec();
    bytes.push(0);
    bytes
}

#[cfg(test)]
mod tests {
    #[test]
    fn mapped_remote_images_translate_io_and_share_updated_layouts() {
        let mut file = vec![0; 256];
        file[1] = 1;
        file[3] = 1;
        file.extend(vec![0x33; 8192]);
        file.extend(vec![0x44; 8192]);
        let Probe::Image(image) = FloppyImage::probe(file.len(), &file) else {
            panic!()
        };
        let (bridge, requests) = vm_bridge();
        let disk = RemoteDisk::with_image(bridge, 8, file.len(), image);
        let worker = std::thread::spawn(move || {
            let clone = disk.clone();
            assert_eq!(disk.read(8192, 1024).unwrap(), vec![0xE5; 1024]);
            assert!(disk.write(8192 + 16, vec![0x7C; 512]));
            assert_eq!(clone.read(8192 + 16, 512).unwrap(), vec![0x7C; 512]);
            assert_eq!(clone.read(16384, 1024).unwrap(), vec![0x44; 1024]);
            assert_eq!(disk.len(), 1_261_568);
            assert!(!disk.write(1_261_568, vec![1]));
        });
        let mut syncs = 0;
        for request in requests {
            let (result, data) = match request.op {
                BridgeOp::HandleRead {
                    handle: 8,
                    offset,
                    len,
                } => {
                    assert!(len <= 32768);
                    (len, file[offset as usize..(offset + len) as usize].to_vec())
                }
                BridgeOp::HandleWrite {
                    handle: 8,
                    offset,
                    data,
                } => {
                    assert!(data.len() <= 32768);
                    let offset = offset as usize;
                    file.resize(file.len().max(offset + data.len()), 0);
                    file[offset..offset + data.len()].copy_from_slice(&data);
                    (data.len() as u32, vec![])
                }
                BridgeOp::ImageSync { handle: 8 } => {
                    syncs += 1;
                    assert_eq!(file.len(), 256 + 1_261_568);
                    assert!(FloppyImage::decode(&file).is_some());
                    (0, vec![])
                }
                op => panic!("unexpected image request: {op:?}"),
            };
            request
                .reply
                .send(Some(BridgeReply { result, data }))
                .unwrap();
        }
        worker.join().unwrap();
        assert_eq!(syncs, 1);
        let (_, raw) = FloppyImage::decode(&file).unwrap();
        assert_eq!(&raw[8192 + 16..8192 + 528], vec![0x7C; 512]);
    }

    #[test]
    fn icon_ejection_keeps_a_physical_lease_and_image_replacement_releases_it() {
        let mut primary = primary_with_floppies();
        let mut state = CpuState::default();
        state.a[2] = 0x56006000;
        for (at, byte) in [(0x6001, 1), (0x6003, 2), (0x6005, 2)] {
            primary.poke_u8(at, byte);
        }
        primary.hypercall_vm_open(&mut state);
        let id = state.d[0];
        let (mut bridge, requests) = vm_bridge();
        primary.bind_vm_passthrough(id, &mut bridge);
        primary.attach_vm_bridge(id, requests);
        let mut child = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        assert!(child
            .install_custom_drives(bridge.clone(), &[CustomDrive::Passthrough { drive: 2 }])
            .is_empty());
        for eject in [true, false] {
            let storage = bridge.clone();
            let worker = std::thread::spawn(move || {
                let disk = (!eject).then(|| RemoteDisk::with_handle(storage.clone(), 9, 737_280));
                assert!(child.change_vm_floppy(0, disk, &storage));
                child
            });
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !worker.is_finished() && std::time::Instant::now() < until {
                state.d[1] = id;
                state.d[2] = 0x8000;
                state.d[3] = 32768;
                primary.hypercall_vm_poll(&mut state);
                if state.d[0] == 17 {
                    assert_eq!(state.d[1], 0, "the old handle's unit");
                    state.d[1] = id;
                    state.d[2] = 0;
                    primary.hypercall_vm_reply(&mut state);
                } else {
                    assert_eq!(state.d[0], 0);
                }
                std::thread::yield_now();
            }
            if !worker.is_finished() {
                primary.detach_vm_bridge(id);
            }
            assert!(
                worker.is_finished(),
                "media change must finish through VmPoll"
            );
            child = worker.join().unwrap();
            if eject {
                assert!(!primary.fdc.drives[0].is_inserted());
                assert!(!child.fdc.drives[0].is_inserted());
                assert_eq!(primary.vm_windows.passthrough.len(), 1);
                assert!(primary.insert_floppy(0, vec![0; 1_474_560], false));
                assert!(child.fdc.drives[0].is_inserted());
            } else {
                assert!(primary.vm_windows.passthrough.is_empty());
                assert!(!primary.fdc.drives[0].is_write_protected());
                assert!(primary.fdc.drives[0].is_inserted());
                assert_eq!(child.fdc.drives[0].remote.as_ref().unwrap().len(), 737_280);
            }
        }
        primary.detach_vm_bridge(id);
    }

    #[test]
    fn boot_choices_preserve_other_sram_and_validate_rom_addresses() {
        let original = vec![0x5A; VM_SRAM_BYTES];
        for choice in 0..14 {
            let mut sram = original.clone();
            assert!(apply_boot_choice(&mut sram, choice, 0xBFFFFC));
            assert_eq!(boot_choice(&sram), choice);
            assert_eq!(&sram[0x20..], &original[0x20..]);
        }
        for (choice, rom) in [(14, 0), (13, 1), (13, 0x1000000)] {
            let mut sram = original.clone();
            assert!(!apply_boot_choice(&mut sram, choice, rom));
            assert_eq!(sram, original, "invalid choices are atomic");
        }
    }

    #[test]
    fn legacy_guests_ignore_unspecified_a2_on_open_and_restart() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        // An old guest may leave a valid RAM address pointing at zeroed memory.
        // Previously this was mistaken for an empty custom drive table.
        state.a[2] = 0x6000;
        bus.hypercall_vm_open(&mut state);
        let id = state.d[0];
        assert!(!bus.vm_windows.open[0].custom);
        assert!(matches!(bus.take_vm_events().as_slice(),
            [VmEvent::Opened { custom: None, .. }]));
        state.d[1] = id;
        bus.hypercall_vm_restart(&mut state);
        assert_eq!(state.d[0], 0);
        assert!(!bus.vm_windows.open[0].custom);
        assert!(matches!(bus.take_vm_events().as_slice(),
            [VmEvent::Restart { custom: None, .. }]));
    }

    #[test]
    fn boot_changes_are_saved_without_resetting_the_vm() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        state.a[2] = 0x56006000; // empty custom table
        bus.hypercall_vm_open(&mut state);
        let id = state.d[0];
        bus.take_vm_events();
        let sram = bus.sram().to_vec();
        bus.set_vm_sram(id, sram.clone());
        state.d[1] = id;
        state.d[2] = 11; // SCSI6
        bus.hypercall_vm_boot(&mut state);
        assert_eq!(state.d[0], 0);
        assert_eq!(
            bus.sram(),
            sram,
            "the primary's own boot choice is untouched"
        );
        assert_eq!(
            bus.take_vm_events(),
            [VmEvent::Boot {
                id,
                choice: 11,
                rom: 0
            }]
        );
        state.d[2] = 0x8000;
        state.d[3] = VM_SRAM_BYTES as u32;
        bus.hypercall_vm_sram(&mut state);
        assert_eq!(state.d[0], VM_SRAM_BYTES as u32);
        let saved: Vec<u8> = (0..VM_SRAM_BYTES)
            .map(|i| bus.peek_u8(0x8000 + i as u32).unwrap())
            .collect();
        assert_eq!(boot_choice(&saved), 11);
        assert_eq!(&saved[0x20..], &sram[0x20..]);
    }

    #[test]
    fn staged_boot_edits_preserve_other_sram_and_do_not_change_live_vms() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let primary = bus.sram().to_vec();
        let staged: Vec<_> = (0..VM_SRAM_BYTES).map(|i| (i * 7) as u8).collect();
        for (i, &byte) in staged.iter().enumerate() {
            bus.poke_u8(0x10000 + i as u32, byte);
        }
        let mut state = CpuState::default();
        state.d[1] = u32::MAX;
        state.d[2] = 11;
        state.a[0] = 0x10000;
        bus.hypercall_vm_boot(&mut state);
        assert_eq!(state.d[0], 0);
        let edited: Vec<_> = (0..VM_SRAM_BYTES)
            .map(|i| bus.peek_u8(0x10000 + i as u32).unwrap())
            .collect();
        assert_eq!(boot_choice(&edited), 11);
        assert_eq!(&edited[..12], &staged[..12]);
        assert_eq!(&edited[0x20..], &staged[0x20..]);
        assert_eq!(bus.sram(), primary);
        assert!(bus.take_vm_events().is_empty());
    }

    fn primary_with_floppies() -> X68kBus {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        // A: is a hard disk; B:/C: are DISK2HD's physical units 0/1.
        let long = |bus: &mut X68kBus, at: u32, value: u32| {
            for (i, byte) in value.to_be_bytes().into_iter().enumerate() {
                bus.poke_u8(at + i as u32, byte);
            }
        };
        long(&mut bus, 0x1C3C, 0x4000);
        for unit in 0..2 {
            let dpb = 0x4000 + unit * 64;
            bus.poke_u8(dpb, unit as u8 + 1);
            bus.poke_u8(dpb + 1, unit as u8);
            long(&mut bus, dpb + 2, 0x5000);
            long(&mut bus, dpb + 6, if unit == 0 { 0x4040 } else { u32::MAX });
        }
        for (i, &byte) in b"DISK2HD".iter().enumerate() {
            bus.poke_u8(0x500F + i as u32, byte);
        }
        assert!(bus.insert_floppy(0, vec![0; 1_261_568], false));
        assert!(bus.insert_floppy(1, vec![0; 1_261_568], true));
        bus
    }

    #[test]
    fn passthrough_is_exclusive_and_tracks_media_changes() {
        use msx68k_cpu::Bus;
        let mut primary = primary_with_floppies();
        let pass = CustomDrive::Passthrough { drive: 2 };
        primary.reserve_passthrough(
            4,
            &[
                CustomDrive::Missing,
                CustomDrive::Missing,
                CustomDrive::Missing,
                CustomDrive::Missing,
                pass.clone(),
            ],
        );
        assert!(
            primary.vm_windows.passthrough.is_empty(),
            "do not lend an unusable fifth FD"
        );
        primary.reserve_passthrough(1, &[pass.clone()]);
        primary.reserve_passthrough(2, &[pass.clone()]);
        primary.reserve_passthrough(3, &[CustomDrive::Passthrough { drive: 1 }]);
        assert_eq!(primary.vm_windows.passthrough.len(), 1);
        let (mut bridge, _) = vm_bridge();
        primary.bind_vm_passthrough(1, &mut bridge);
        let mut child = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        assert!(child.install_custom_drives(bridge, &[pass.clone()]).is_empty());
        assert!(primary.fdc.drives[0].is_write_protected());
        assert!(!child.fdc.drives[0].is_write_protected());
        // The primary's actual FDC rejects writes, not only its UI/status.
        for byte in [0x45, 0, 0, 0, 1, 3, 1, 0x1B, 0xFF] {
            primary.write_u8(0xE94003, byte, true).unwrap();
        }
        let result: Vec<_> = (0..7)
            .map(|_| primary.read_u8(0xE94003, true).unwrap())
            .collect();
        assert_eq!(result[1], crate::fdc::ST1_NW);
        let op = BridgeOp::FloppyWrite {
            unit: 0,
            offset: 1024,
            data: vec![0xA5; 1024],
        };
        assert_eq!(primary.passthrough_request(1, &op).unwrap().result, 1024);
        assert!(primary.fdc.drives[0].image.as_ref().unwrap()[1024..2048]
            .iter()
            .all(|&byte| byte == 0xA5));
        assert!(primary.passthrough_request(2, &op).is_none());
        assert!(primary
            .passthrough_request(
                1,
                &BridgeOp::FloppyRead {
                    unit: 0,
                    offset: 1,
                    len: 1024
                }
            )
            .is_none());

        primary.eject_floppy(0);
        assert!(!child.fdc.drives[0].is_inserted());
        assert!(primary.insert_floppy(0, vec![0; 1_474_560], true));
        assert!(child.fdc.drives[0].is_inserted());
        assert!(child.fdc.drives[0].is_write_protected());
        assert!(
            primary.passthrough_request(1, &op).is_none(),
            "respect the disk's real tab"
        );
        // A same-frame eject/insert is latched as a removal until the IPL
        // reads it, then a following V-Blank announces the inserted medium.
        child.fdc.refresh_remote_media(&mut child.ioi);
        child.write_u8(0xE94005, 1, true).unwrap();
        assert_eq!(child.read_u8(0xE94005, true).unwrap() & 0x80, 0);
        assert_eq!(child.read_u8(0xE94005, true).unwrap() & 0x80, 0);
        child.fdc.refresh_remote_media(&mut child.ioi);
        assert_eq!(child.read_u8(0xE94005, true).unwrap() & 0x80, 0x80);
        for byte in [0x4A, 0] {
            child.write_u8(0xE94003, byte, true).unwrap();
        }
        let id: Vec<_> = (0..7)
            .map(|_| child.read_u8(0xE94003, true).unwrap())
            .collect();
        assert_eq!(
            id[6], 2,
            "READ ID must follow the replacement disk's 512-byte sectors"
        );
        primary.detach_vm_bridge(1);
        assert!(
            !child.fdc.drives[0].is_inserted(),
            "the old owner loses media on detach"
        );
        assert!(
            primary.fdc.drives[0].write_protected,
            "retain the replacement disk's real tab"
        );
        primary.reserve_passthrough(2, &[pass.clone()]);
        assert_eq!(primary.vm_windows.passthrough[0].owner, 2);
        primary.detach_vm_bridge(2);
        assert!(primary.fdc.drives[0].is_write_protected());
    }

    #[test]
    fn passthrough_requests_finish_in_poll_and_close_releases_the_drive() {
        let mut primary = primary_with_floppies();
        let mut state = CpuState::default();
        // A2's table contains one passthrough of the primary's B:.
        state.a[2] = 0x56006000;
        for (at, byte) in [(0x6001, 1), (0x6003, 2), (0x6005, 2)] {
            primary.poke_u8(at, byte);
        }
        primary.hypercall_vm_open(&mut state);
        let id = state.d[0];
        assert!(primary.fdc.drives[0].is_write_protected());
        let (mut bridge, requests) = vm_bridge();
        primary.bind_vm_passthrough(id, &mut bridge);
        primary.attach_vm_bridge(id, requests);
        let reader = std::thread::spawn(move || {
            bridge.ask(BridgeOp::FloppyRead {
                unit: 0,
                offset: 0,
                len: 1024,
            })
        });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reader.is_finished() && std::time::Instant::now() < until {
            state.d[1] = id;
            state.d[2] = 0x8000;
            state.d[3] = 32768;
            primary.hypercall_vm_poll(&mut state);
            assert_eq!(state.d[0], 0, "no new request kind is sent to VMX68K.X");
            std::thread::yield_now();
        }
        primary.detach_vm_bridge(id);
        assert_eq!(
            reader.join().unwrap(),
            Some(BridgeReply {
                result: 1024,
                data: vec![0; 1024]
            })
        );
        // Closing also releases reservations even if no bridge was attached.
        primary.reserve_passthrough(id, &[CustomDrive::Passthrough { drive: 2 }]);
        // Restart changes ownership from B: to C: and restores B:'s protection.
        primary.poke_u8(0x6005, 3);
        state.d[1] = id;
        state.d[2] = 4;
        state.d[3] = 0;
        primary.hypercall_vm_restart(&mut state);
        assert_eq!(state.d[0], 0);
        assert!(!primary.fdc.drives[0].is_write_protected());
        assert_eq!(primary.vm_windows.passthrough[0].unit, 1);
        state.d[1] = id;
        primary.hypercall_vm_close(&mut state);
        assert_eq!(state.d[0], 0);
        assert!(!primary.fdc.drives[0].is_write_protected());
        assert!(
            primary.fdc.drives[1].is_write_protected(),
            "keep C:'s physical tab"
        );
        assert!(primary.vm_windows.passthrough.is_empty());
    }

    #[test]
    fn handle_disk_requires_complete_reads_and_writes() {
        let (bridge, requests) = vm_bridge();
        let worker = std::thread::spawn(move || {
            let disk = RemoteDisk::with_handle(bridge, 17, 4096);
            assert_eq!(disk.read(512, 3), Some(vec![1, 2, 3]));
            assert!(disk.write(1024, vec![4, 5]));
            assert_eq!(disk.read(0, 3), None);
            assert!(!disk.write(0, vec![6, 7]));
        });
        for (op, result, data) in [
            (
                BridgeOp::HandleRead {
                    handle: 17,
                    offset: 512,
                    len: 3,
                },
                3,
                vec![1, 2, 3],
            ),
            (
                BridgeOp::HandleWrite {
                    handle: 17,
                    offset: 1024,
                    data: vec![4, 5],
                },
                2,
                vec![],
            ),
            (
                BridgeOp::HandleRead {
                    handle: 17,
                    offset: 0,
                    len: 3,
                },
                2,
                vec![1, 2],
            ),
            (
                BridgeOp::HandleWrite {
                    handle: 17,
                    offset: 0,
                    data: vec![6, 7],
                },
                1,
                vec![],
            ),
        ] {
            let request = requests
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(request.op, op);
            request
                .reply
                .send(Some(BridgeReply { result, data }))
                .unwrap();
        }
        worker.join().unwrap();
    }
    use super::*;
    use crate::timing::CpuSpeedMode;

    #[test]
    fn vms_open_and_close_in_order() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        state.d[2] = 0;
        state.d[3] = 0;
        state.d[1] = 0x0012_3456;
        bus.hypercall_vm_open(&mut state);
        assert_eq!(state.d[0], 1);
        state.d[1] = 0x0012_3500;
        bus.hypercall_vm_open(&mut state);
        assert_eq!(state.d[0], 2);

        state.d[1] = 1;
        bus.hypercall_vm_poll(&mut state);
        assert_eq!(state.d[0], 0);
        bus.hypercall_vm_close(&mut state);
        assert_eq!(state.d[0], 0);
        bus.hypercall_vm_poll(&mut state);
        assert_eq!(state.d[0], u32::MAX, "a closed VM is unknown");
        bus.hypercall_vm_close(&mut state);
        assert_eq!(state.d[0], u32::MAX);

        assert_eq!(
            bus.take_vm_events(),
            [
                VmEvent::Opened {
                    id: 1,
                    window: 0x0012_3456,
                    drive0_bytes: 0,
                    drives: 0,
                    speed: CpuSpeedMode::Mhz10,
                    sram: None,
                    cpu: CpuModel::Mc68000,
                    ram_mb: 4,
                    custom: None,
                },
                VmEvent::Opened {
                    id: 2,
                    window: 0x0012_3500,
                    drive0_bytes: 0,
                    drives: 0,
                    speed: CpuSpeedMode::Mhz10,
                    sram: None,
                    cpu: CpuModel::Mc68000,
                    ram_mb: 4,
                    custom: None,
                },
                VmEvent::Closed { id: 1 },
            ]
        );
        assert!(bus.take_vm_events().is_empty());
    }

    #[test]
    fn a_setup_brings_its_sram_and_speed_and_gets_the_sram_back() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        let setup = 0x2_0000;
        for i in 0..VM_SRAM_BYTES as u32 {
            bus.poke_u8(setup + i, i as u8);
        }
        state.d[1] = 0x0012_3456;
        state.d[5] = setup;
        state.d[6] = 3;
        bus.hypercall_vm_open(&mut state);
        let id = state.d[0];
        let [VmEvent::Opened { speed, sram: Some(sram), .. }] = &bus.take_vm_events()[..] else {
            panic!("one VM with SRAM");
        };
        assert_eq!(*speed, CpuSpeedMode::Burst);
        assert_eq!(sram.len(), VM_SRAM_BYTES);
        assert_eq!(sram[0x123], 0x23);

        // Changed SRAM is reported, then copied into VMX68K.SX's buffer once.
        bus.set_vm_sram(id, vec![0xA5; VM_SRAM_BYTES]);
        state.d[1] = id;
        bus.hypercall_vm_status(&mut state);
        assert_eq!(state.d[0] & STATUS_SRAM, STATUS_SRAM);
        let buffer = 0x6_0000;
        state.d[1] = id;
        state.d[2] = buffer;
        state.d[3] = VM_SRAM_BYTES as u32;
        bus.hypercall_vm_sram(&mut state);
        assert_eq!(state.d[0], VM_SRAM_BYTES as u32);
        assert_eq!(bus.peek_u8(buffer + 100), Some(0xA5));
        state.d[1] = id;
        bus.hypercall_vm_status(&mut state);
        assert_eq!(state.d[0] & STATUS_SRAM, 0, "saved once");
    }

    #[test]
    fn a_68030_setup_numbers_its_own_speeds_and_restarts() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        state.d[1] = 0x0012_3456;
        state.d[6] = 1;
        state.d[7] = 1 << 16 | 8;
        bus.hypercall_vm_open(&mut state);
        let id = state.d[0];
        let [VmEvent::Opened { cpu, ram_mb, speed, .. }] = &bus.take_vm_events()[..] else {
            panic!("one VM");
        };
        assert_eq!((*cpu, *ram_mb, *speed), (CpuModel::Mc68030, 8, CpuSpeedMode::Burst));

        // The 68030's speeds are 25 MHz and Burst only.
        state.d[1] = id;
        state.d[2] = 0;
        bus.hypercall_vm_speed(&mut state);
        assert_eq!(bus.take_vm_events(), [VmEvent::Speed { id, mode: CpuSpeedMode::Mhz25 }]);
        state.d[1] = id;
        state.d[2] = 2;
        bus.hypercall_vm_speed(&mut state);
        assert_eq!(state.d[0], u32::MAX);

        // The old bridge must stop accepting requests before newly opened handles
        // can be served by VMX68K.X, even before the app consumes the restart event.
        let (old_bridge, requests) = vm_bridge();
        bus.attach_vm_bridge(id, requests);

        // Restarted as a 68000 with 12 MB at 16 MHz, it numbers the 68000's speeds.
        state.d[1] = id;
        state.d[2] = 12;
        state.d[3] = 1;
        bus.hypercall_vm_restart(&mut state);
        assert!(old_bridge.ask(BridgeOp::Read { offset: 0, len: 1 }).is_none());
        assert_eq!(
            bus.take_vm_events(),
            [VmEvent::Restart {
                id,
                cpu: CpuModel::Mc68000,
                ram_mb: 12,
                speed: CpuSpeedMode::Mhz16,
                custom: None
            }]
        );
        state.d[1] = id;
        state.d[2] = 3;
        bus.hypercall_vm_speed(&mut state);
        assert_eq!(bus.take_vm_events(), [VmEvent::Speed { id, mode: CpuSpeedMode::Burst }]);
        state.d[1] = id;
        bus.hypercall_vm_interrupt(&mut state);
        assert_eq!(state.d[0], 0);
        bus.hypercall_vm_reset(&mut state);
        assert_eq!(state.d[0], 0);
        assert_eq!(
            bus.take_vm_events(),
            [VmEvent::Interrupt { id }, VmEvent::Reset { id }]
        );
        state.d[1] = 999;
        bus.hypercall_vm_interrupt(&mut state);
        assert_eq!(state.d[0], u32::MAX);
        bus.hypercall_vm_reset(&mut state);
        assert_eq!(state.d[0], u32::MAX);
    }
}

#[cfg(test)]
mod sprite_option_tests {
    use super::*;

    #[test]
    fn vm_sprite_options_validate_and_preserve_the_live_machine() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        state.d[7] = 4;
        bus.hypercall_vm_open(&mut state);
        let id = state.d[0];
        bus.take_vm_events();
        state.d[1] = id;
        state.d[2] = 3;
        bus.hypercall_vm_sprite_options(&mut state);
        assert_eq!(state.d[0], 0);
        assert_eq!(
            bus.take_vm_events(),
            [VmEvent::SpriteOptions {
                id,
                options: crate::sprite::SpriteOptions::from_bits(3),
            }]
        );
        state.d[2] = u32::MAX;
        bus.hypercall_vm_sprite_options(&mut state);
        assert_eq!(state.d[0], 3);
        assert!(bus.take_vm_events().is_empty());
        state.d[2] = 4;
        bus.hypercall_vm_sprite_options(&mut state);
        assert_eq!(state.d[0], u32::MAX);
        assert_eq!(bus.vm_sprite_options(id).bits(), 3);
        state.d[2] = 4;
        state.d[3] = 0;
        bus.hypercall_vm_restart(&mut state);
        assert_eq!(bus.vm_sprite_options(id).bits(), 3);
    }
}
