//! The master VM's side of VMX68K (`docs/VMX68K.md` Section 3.5): VMX68K.SX opens and closes
//! VMs through msx68k's application entry, and the host finds where each VM's window shows.
//!
//! The bus only keeps the record. The app starts and stops the VMs it reports, each on a
//! thread of its own, and places their screens with [`X68kBus::vm_window_layout`].

use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;

// Window records (`SXDEF2.H`).
const WINDOW_BITMAP: u32 = 0;
const WINDOW_RECT: u32 = 4;
const WINDOW_VISIBLE_RGN: u32 = 16;
const WINDOW_VISIBLE: u32 = 66;
const BITMAP_RECT: u32 = 2;

/// A change VMX68K.SX made to the set of VMs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmEvent {
    /// A VM was asked for, to be shown in the window at `window`.
    Opened { id: u32, window: u32 },
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
    /// Takes the VMs opened and closed since the last call, in order.
    pub fn take_vm_events(&mut self) -> Vec<VmEvent> {
        std::mem::take(&mut self.vm_windows.events)
    }

    /// `VmOpen`: records a VM for the window `D1.L` and returns its ID in `D0.L`.
    pub(crate) fn hypercall_vm_open(&mut self, state: &mut CpuState) -> TrapResult {
        let windows = &mut self.vm_windows;
        windows.next_id += 1;
        let id = windows.next_id;
        let window = state.d[1] & 0x00FF_FFFF;
        windows.open.push((id, window));
        windows.events.push(VmEvent::Opened { id, window });
        state.d[0] = id;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmPoll`: the VM `D1.L` has no requests yet. Returns 0, or -1 for an unknown VM.
    pub(crate) fn hypercall_vm_poll(&mut self, state: &mut CpuState) -> TrapResult {
        let known = self.vm_windows.open.iter().any(|&(id, _)| id == state.d[1]);
        state.d[0] = if known { 0 } else { u32::MAX };
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// `VmClose`: forgets the VM `D1.L`. Returns 0, or -1 for an unknown VM.
    pub(crate) fn hypercall_vm_close(&mut self, state: &mut CpuState) -> TrapResult {
        let id = state.d[1];
        let windows = &mut self.vm_windows;
        let before = windows.open.len();
        windows.open.retain(|&(open, _)| open != id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::CpuSpeedMode;

    #[test]
    fn vms_open_and_close_in_order() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        let mut state = CpuState::default();
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
                VmEvent::Opened { id: 1, window: 0x0012_3456 },
                VmEvent::Opened { id: 2, window: 0x0012_3500 },
                VmEvent::Closed { id: 1 },
            ]
        );
        assert!(bus.take_vm_events().is_empty());
    }
}
