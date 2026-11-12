//! The master VM's side of VMX68K (`docs/VMX68K.md` Section 3.5): VMX68K.SX opens and closes
//! VMs through msx68k's application entry, and the host finds where each VM's window shows.
//!
//! The bus only keeps the record. The app starts and stops the VMs it reports, each on a
//! thread of its own, and places their screens with [`X68kBus::vm_window_layout`].

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};

use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;

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

/// The channel through which a VM asks VMX68K.SX, in the master VM, to read or write the
/// master's storage for it (`docs/VMX68K.md` Section 3.1). The asking thread waits for each
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
    /// 3: opens the master's file `path` for reading; the result is its handle.
    Open { path: Vec<u8> },
    /// 4: reads `len` bytes at `offset` of the master's file `handle`; the result is the count.
    HandleRead { handle: u32, offset: u32, len: u32 },
    /// 5: closes the master's file `handle`.
    Close { handle: u32 },
    /// 6: `_FILES` for `path` with `attr`; the data is the 53-byte buffer.
    Files { attr: u32, path: Vec<u8> },
    /// 7: `_NFILES` with the 53-byte buffer of an earlier search.
    Nfiles { buffer: Vec<u8> },
    /// 8: `_DSKFRE` of `drive` (1 is A:); the data is the 8-byte buffer.
    Dskfre { drive: u32 },
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

/// A disk image that the master VM keeps, which a VM's floppy drive reads and writes through
/// its [`VmBridge`].
#[derive(Debug, Clone)]
pub struct RemoteDisk {
    len: usize,
    bridge: VmBridge,
}

impl RemoteDisk {
    pub fn new(bridge: VmBridge, len: usize) -> Self {
        Self { len, bridge }
    }

    /// Size of the image.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Reads `len` bytes at `offset`, waiting for the master VM.
    pub(crate) fn read(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let op = BridgeOp::Read {
            offset: offset as u32,
            len: len as u32,
        };
        self.bridge
            .ask(op)
            .filter(|reply| reply.result == 0 && reply.data.len() == len)
            .map(|reply| reply.data)
    }

    /// Writes `data` at `offset`, waiting for the master VM.
    pub(crate) fn write(&self, offset: usize, data: Vec<u8>) -> bool {
        let op = BridgeOp::Write {
            offset: offset as u32,
            data,
        };
        self.bridge.ask(op).is_some_and(|reply| reply.result == 0)
    }
}

/// A change VMX68K.SX made to the set of VMs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmEvent {
    /// A VM was asked for, to be shown in the window at `window`, with a disk image of
    /// `drive0_bytes` for drive 0 (0 for none) that VMX68K.SX reads and writes for it.
    Opened { id: u32, window: u32, drive0_bytes: u32 },
    /// The VM was closed.
    Closed { id: u32 },
}

/// The VMs VMX68K.SX has open.
#[derive(Debug, Default)]
pub(crate) struct VmWindows {
    next_id: u32,
    /// ID and window of each open VM.
    open: Vec<(u32, u32)>,
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

/// Where a VM's window shows on the master VM's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmWindowLayout {
    /// Screen position of the top-left of the window's content, where the VM's screen goes.
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

    /// Sends `E_USER1` to the window of each VM with disk requests VMX68K.SX has not polled
    /// for, so it wakes for them rather than polling on every turn. Called when the guest is
    /// about to take an event, when its queue is consistent.
    pub(crate) fn wake_vm_windows(&mut self) {
        let Some(text_base) = self.native_gm_text_base else {
            return;
        };
        let mut wake = Vec::new();
        for (id, disk) in &mut self.vm_windows.disks {
            if disk.woken || disk.buffer.is_some() {
                continue;
            }
            while let Ok(request) = disk.requests.try_recv() {
                disk.pending.push_back(request);
            }
            if !disk.pending.is_empty() {
                if let Some(&(_, window)) = self.vm_windows.open.iter().find(|(open, _)| open == id) {
                    wake.push((*id, window));
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
    /// VMX68K.SX serves, and returns its ID in `D0.L`.
    pub(crate) fn hypercall_vm_open(&mut self, state: &mut CpuState) -> TrapResult {
        let windows = &mut self.vm_windows;
        windows.next_id += 1;
        let id = windows.next_id;
        let window = state.d[1] & 0x00FF_FFFF;
        windows.open.push((id, window));
        windows.events.push(VmEvent::Opened {
            id,
            window,
            drive0_bytes: state.d[2],
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
        if !self.vm_windows.open.iter().any(|&(open, _)| open == id) {
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
            BridgeOp::Open { path } => ([0, 0, 0], vec![(0, nul_terminated(path))], 0),
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
        windows.open.retain(|&(open, _)| open != id);
        windows.disks.retain(|(open, _)| *open != id);
        state.d[0] = if windows.open.len() < before {
            windows.events.push(VmEvent::Closed { id });
            0
        } else {
            u32::MAX
        };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Reads where the window at `window` shows its content: the content's screen position
    /// and its visible parts. Returns `None` while the window is hidden.
    pub fn vm_window_layout(&self, window: u32) -> Option<VmWindowLayout> {
        if self.peek_u8(window + WINDOW_VISIBLE)? == 0 {
            return None;
        }
        let content = self.peek_rect(window + WINDOW_RECT)?;
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
    use super::*;
    use crate::timing::CpuSpeedMode;

    #[test]
    fn vms_open_and_close_in_order() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
        state.d[2] = 0;
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
                VmEvent::Opened { id: 1, window: 0x0012_3456, drive0_bytes: 0 },
                VmEvent::Opened { id: 2, window: 0x0012_3500, drive0_bytes: 0 },
                VmEvent::Closed { id: 1 },
            ]
        );
        assert!(bus.take_vm_events().is_empty());
    }
}
