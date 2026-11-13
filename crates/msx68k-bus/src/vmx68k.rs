//! The primary VM's side of VMX68K (`docs/VMX68K.md` Section 3.5): VMX68K.SX opens and closes
//! VMs through msx68k's application entry, and the host finds where each VM's window shows.
//!
//! The bus only keeps the record. The app starts and stops the VMs it reports, each on a
//! thread of its own, and places their screens with [`X68kBus::vm_window_layout`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use msx68k_cpu::{CpuModel, CpuState, TrapResult};

use crate::bus::X68kBus;
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
    Create { path: Vec<u8>, attr: u32, replace: bool },
    /// 15: writes the bytes at `offset` of the primary's file `handle`; with none, cuts the file
    /// there. The result is the count.
    HandleWrite { handle: u32, offset: u32, data: Vec<u8> },
    /// 16: `_FILEDATE` setting `date << 16 | time` of the primary's file `handle`.
    Filedate { handle: u32, datetime: u32 },
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
    (VmBridge { requests }, queue)
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

/// A disk image that the primary VM keeps, which a VM's floppy or SCSI drive accesses through
/// its [`VmBridge`].
#[derive(Debug, Clone)]
pub struct RemoteDisk {
    len: usize,
    bridge: VmBridge,
    /// The primary's file handle of the image, or `None` for drive 0's, which VMX68K.SX keeps.
    handle: Option<u32>,
}

impl RemoteDisk {
    pub fn new(bridge: VmBridge, len: usize) -> Self {
        Self { len, bridge, handle: None }
    }

    /// An image VMX68K.SX opened as the primary's file `handle`, of `len` bytes.
    pub fn with_handle(bridge: VmBridge, handle: u32, len: usize) -> Self {
        Self { len, bridge, handle: Some(handle) }
    }

    /// Size of the image.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reads an image range, splitting transfers to fit VMX68K.X's 32 KB buffer.
    pub(crate) fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        if offset.checked_add(len)? > self.len {
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
        let (op, done) = match self.handle {
            Some(handle) => (BridgeOp::HandleRead { handle, offset, len: count }, count),
            None => (BridgeOp::Read { offset, len: count }, 0),
        };
        self.bridge
            .ask(op)
            .filter(|reply| reply.result == done && reply.data.len() == len)
            .map(|reply| reply.data)
    }

    /// Writes through in chunks; a failed transfer may have written earlier chunks.
    pub(crate) fn write(&self, offset: usize, data: Vec<u8>) -> bool {
        if !offset.checked_add(data.len()).is_some_and(|end| end <= self.len) {
            return false;
        }
        data.chunks(32768).enumerate().all(|(i, bytes)| {
            self.write_chunk(offset + i * 32768, bytes.to_vec())
        })
    }

    fn write_chunk(&self, offset: usize, data: Vec<u8>) -> bool {
        let Ok(offset) = u32::try_from(offset) else { return false };
        let (op, done) = match self.handle {
            Some(handle) => {
                let len = data.len() as u32;
                (BridgeOp::HandleWrite { handle, offset, data }, len)
            }
            None => (BridgeOp::Write { offset, data }, 0),
        };
        self.bridge.ask(op).is_some_and(|reply| reply.result == done)
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
    /// VMX68K.SX chose a speed for the VM from its menu.
    Speed { id: u32, mode: CpuSpeedMode },
    /// The VM was closed.
    Closed { id: u32 },
}

/// A drive of a VM in custom mode, as VMX68K.SX set it up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomDrive {
    /// An image file VMX68K.SX opened, read and written through `handle`, of `len` bytes.
    File { handle: u32, len: u32 },
    /// An image file VMX68K.SX could not open, an empty drive.
    Missing,
    /// The primary's drive `drive` (1 is A:), file-level.
    Shared { drive: u32 },
    /// The primary's floppy drive `drive` (1 is A:), sector-level.
    Passthrough { drive: u32 },
}

/// Bytes of an entry of the drive table `VmOpen` and `VmRestart` take in `A2.L`, after its
/// count (a word): its kind (0 file, 1 shared, 2 passthrough), its handle or drive (-1 for a
/// file that did not open), and the file's size.
const DRIVE_TABLE_ENTRY: u32 = 8;

impl X68kBus {
    /// Installs a custom setup's raw floppy/HDD images and file-level shares.
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
        for &drive in drives {
            match drive {
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
                            target, RemoteDisk::with_handle(bridge.clone(), handle, len as usize),
                        ) =>
                {
                    target += 1;
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
    fn custom_drives(&self, at: u32) -> Option<Vec<CustomDrive>> {
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
                    (0, handle) => CustomDrive::File { handle: handle as u32, len },
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
    /// [`CustomDrive`]), and returns its ID in `D0.L`.
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
        });
        let sram_at = state.d[5] & 0x00FF_FFFF;
        let sram = (sram_at != 0).then(|| {
            (0..VM_SRAM_BYTES as u32)
                .map(|i| self.peek_u8(sram_at + i).unwrap_or(0))
                .collect()
        });
        let custom = self.custom_drives(state.a[2]);
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
        // Numbers in D1..D3, bytes for the buffer, and how many bytes the reply brings back.
        let (numbers, bytes, reply_len): ([u32; 3], Vec<BufferBytes>, u32) = match &op {
            BridgeOp::Read { offset, len } => ([*offset, *len, 0], vec![], *len),
            BridgeOp::Write { offset, data } => ([*offset, data.len() as u32, 0], vec![(0, data.clone())], 0),
            BridgeOp::Open { path, mode } => ([*mode, 0, 0], vec![(0, nul_terminated(path))], 0),
            BridgeOp::Mkdir { path } | BridgeOp::Rmdir { path } | BridgeOp::Delete { path } => {
                ([0, 0, 0], vec![(0, nul_terminated(path))], 0)
            }
            BridgeOp::Rename { from, to } => {
                ([0, 0, 0], vec![(0, nul_terminated(from)), (RENAME_TO, nul_terminated(to))], 0)
            }
            BridgeOp::Chmod { path, attr } => ([*attr, 0, 0], vec![(0, nul_terminated(path))], 0),
            BridgeOp::Create { path, attr, replace } => {
                ([*attr, *replace as u32, 0], vec![(0, nul_terminated(path))], 0)
            }
            BridgeOp::HandleWrite { handle, offset, data } => {
                ([*offset, data.len() as u32, *handle], vec![(0, data.clone())], 0)
            }
            BridgeOp::Filedate { handle, datetime } => ([*datetime, 0, *handle], vec![], 0),
            BridgeOp::HandleRead { handle, offset, len } => ([*offset, *len, *handle], vec![], *len),
            BridgeOp::Close { handle } => ([0, 0, *handle], vec![], 0),
            BridgeOp::Files { attr, path } => {
                ([*attr, 0, 0], vec![(0, vec![0; 53]), (FILES_PATH, nul_terminated(path))], 53)
            }
            BridgeOp::Nfiles { buffer } => ([0, 0, 0], vec![(0, buffer.clone())], 53),
            BridgeOp::Dskfre { drive } => ([*drive, 0, 0], vec![], 8),
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
                let speed = vm_speed(cpu, state.d[3]);
                // Retire the old requests before the task can serve another event
                // with its newly opened image handles.
                self.detach_vm_bridge(id);
                self.vm_windows.events.push(VmEvent::Restart { id, cpu, ram_mb, speed, custom });
                0
            }
            None => u32::MAX,
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
            (BridgeOp::HandleRead { handle: 17, offset: 512, len: 3 }, 3, vec![1, 2, 3]),
            (BridgeOp::HandleWrite { handle: 17, offset: 1024, data: vec![4, 5] }, 2, vec![]),
            (BridgeOp::HandleRead { handle: 17, offset: 0, len: 3 }, 2, vec![1, 2]),
            (BridgeOp::HandleWrite { handle: 17, offset: 0, data: vec![6, 7] }, 1, vec![]),
        ] {
            let request = requests.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert_eq!(request.op, op);
            request.reply.send(Some(BridgeReply { result, data })).unwrap();
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
    }
}
