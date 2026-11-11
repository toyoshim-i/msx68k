//! Mouse wheel support for SX-Window.
//!
//! SX-Window has no wheel, and applications scroll their windows themselves when one of their
//! scroll bar arrows is clicked. So each wheel notch clicks an arrow of the active window's
//! scroll bar: a left-down and a left-up event at the arrow go into the event queue, just as
//! `EMSet` would post them. `CMCheck` does not track the mouse on scroll bar arrows, so the
//! click takes effect without moving the cursor. Whatever the pointer is over, only the active
//! window scrolls, since inactive tasks should not get to handle events.

use msx68k_cpu::{CpuState, TrapResult};

use crate::bus::X68kBus;

/// `EMGet`: the guest is about to take an event, so the queue is consistent.
pub(crate) const EM_GET: u16 = 0xA0A5;

// Globals of `SYS/FSX.X`, as offsets from its text base.
/// Active window record (`WMActive`).
const ACTIVE_WINDOW: u32 = 0x543CA;
/// Event Manager work area (the `A5` of `EMSet`).
const EVENT_MANAGER: u32 = 0x543E2;
/// System time, the `when` of new events.
const SYSTEM_TIME: u32 = 0x549F2;
/// Pointer to the keyboard state, whose modifier long word is the `how` of new events.
const KEYBOARD_STATE: u32 = 0x54AA6;
/// Copy of the last `how`, which `EMSet` keeps too.
const LAST_HOW: u32 = 0x54A00;
/// Heads of the resource map chains searched by `RMHdlToRsc`.
const RESOURCE_MAPS: [u32; 2] = [0x53B6A, 0x53B6E];

// Event Manager work area.
const EM_HEAD: u32 = 2;
const EM_TAIL: u32 = 6;
const EM_MASK: u32 = 1546;
const EM_FREE: u32 = 1548;

// Event queue entries.
const ENTRY_NEXT: u32 = 0;
const ENTRY_KIND: u32 = 4;
const ENTRY_WHAT: u32 = 6;
const ENTRY_WHOM: u32 = 8;
const ENTRY_WHEN: u32 = 12;
const ENTRY_WHERE: u32 = 16;
const ENTRY_HOW: u32 = 20;
/// Kind word `EMSet` gives every entry it posts.
const POSTED_ENTRY: u16 = 3;

const E_MSLDOWN: u16 = 1;
const E_MSLUP: u16 = 2;

// Window records.
const WINDOW_BITMAP: u32 = 0;
const WINDOW_VISIBLE_RGN: u32 = 16;
const WINDOW_VISIBLE: u32 = 66;
const WINDOW_CONTROLS: u32 = 98;
const BITMAP_RECT: u32 = 2;

// Control records.
const CONTROL_RECT: u32 = 8;
const CONTROL_VISIBLE: u32 = 16;
const CONTROL_HILITE: u32 = 17;
const CONTROL_DEF: u32 = 24;
/// `hilite` of an inactive control.
const CONTROL_INACTIVE: u8 = 255;

/// Resource type of control definitions.
const CDEF: u32 = u32::from_be_bytes(*b"CDEF");
/// Window scroll bars (`CI_SCLBRWH`, `CI_SCLBRWV`).
const HORIZONTAL_SCROLL_BAR: i16 = 0x40;
const VERTICAL_SCROLL_BAR: i16 = 0x41;

/// User settings for the wheel, kept in `MSX68K.LB` ([`crate::sx_settings`]) and set by the
/// guest settings app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WheelConfig {
    /// Scrolls the other way, as with "natural" scrolling.
    pub reverse: bool,
    /// Arrow clicks per notch.
    pub clicks_per_notch: u8,
    /// Trackpad scrolling, in host pixels, that makes one notch.
    pub trackpad_pixels: u16,
}

impl WheelConfig {
    pub const CLICKS_RANGE: std::ops::RangeInclusive<u8> = 1..=10;
    pub const TRACKPAD_PIXELS_RANGE: std::ops::RangeInclusive<u16> = 5..=400;

    /// Returns the setting with every value brought into its range.
    pub fn clamped(self) -> Self {
        Self {
            reverse: self.reverse,
            clicks_per_notch: self
                .clicks_per_notch
                .clamp(*Self::CLICKS_RANGE.start(), *Self::CLICKS_RANGE.end()),
            trackpad_pixels: self.trackpad_pixels.clamp(
                *Self::TRACKPAD_PIXELS_RANGE.start(),
                *Self::TRACKPAD_PIXELS_RANGE.end(),
            ),
        }
    }
}

impl Default for WheelConfig {
    fn default() -> Self {
        Self {
            reverse: false,
            clicks_per_notch: 1,
            trackpad_pixels: 40,
        }
    }
}

/// Wheel notches waiting to be delivered.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingWheel {
    /// Positive scrolls down.
    vertical: i32,
    /// Positive scrolls right.
    horizontal: i32,
}

impl X68kBus {
    /// Queues wheel notches from the host: positive `notches` scroll down, or right when
    /// `horizontal` (Shift held), before the user's direction and speed settings apply.
    pub fn post_mouse_wheel(&mut self, notches: i32, horizontal: bool) {
        let config = self.wheel_config;
        let direction = if config.reverse { -1 } else { 1 };
        let notches = notches.saturating_mul(config.clicks_per_notch as i32 * direction);
        let wheel = &mut self.pending_wheel;
        let axis = if horizontal {
            &mut wheel.horizontal
        } else {
            &mut wheel.vertical
        };
        *axis = axis.saturating_add(notches);
    }

    /// `GetWheelConfig` and `SetWheelConfig` application functions: return, after setting them
    /// when `set`, the wheel settings in `D1.L` (1 to reverse), `D2.L` (arrow clicks per
    /// notch), and `D3.L` (trackpad pixels per notch). Values out of range are clamped. `D0.L`
    /// returns 0.
    pub(crate) fn hypercall_wheel_config(&mut self, state: &mut CpuState, set: bool) -> TrapResult {
        if set {
            self.wheel_config = WheelConfig {
                reverse: state.d[1] != 0,
                clicks_per_notch: state.d[2].min(u8::MAX as u32) as u8,
                trackpad_pixels: state.d[3].min(u16::MAX as u32) as u16,
            }
            .clamped();
        }
        let config = self.wheel_config;
        state.d[0] = 0;
        state.d[1] = config.reverse as u32;
        state.d[2] = config.clicks_per_notch as u32;
        state.d[3] = config.trackpad_pixels as u32;
        TrapResult::Handled { extra_cycles: 0 }
    }

    /// Delivers one queued wheel notch as an arrow click, when the guest is about to read
    /// its event queue. A notch with no scroll bar to go to is dropped.
    pub(crate) fn deliver_wheel_notch(&mut self) {
        let Some(text_base) = self.native_gm_text_base else {
            return;
        };
        let wheel = self.pending_wheel;
        let (horizontal, toward_end) = if wheel.vertical != 0 {
            self.pending_wheel.vertical -= wheel.vertical.signum();
            (false, wheel.vertical > 0)
        } else if wheel.horizontal != 0 {
            self.pending_wheel.horizontal -= wheel.horizontal.signum();
            (true, wheel.horizontal > 0)
        } else {
            return;
        };
        if let Some(point) = self.scroll_arrow(text_base, horizontal, toward_end) {
            if self.post_event(text_base, E_MSLDOWN, point) {
                self.post_event(text_base, E_MSLUP, point);
            }
        }
    }

    /// Returns the global position of an arrow of the active window's scroll bar, if it has
    /// one that is shown, active, and not covered.
    fn scroll_arrow(&self, text_base: u32, horizontal: bool, toward_end: bool) -> Option<(i16, i16)> {
        let window = self.peek_u32(text_base + ACTIVE_WINDOW)?;
        if window == 0 || self.peek_u8(window + WINDOW_VISIBLE)? == 0 {
            return None;
        }
        let kind = if horizontal {
            HORIZONTAL_SCROLL_BAR
        } else {
            VERTICAL_SCROLL_BAR
        };
        // Take the outermost bar of the kind, which is the window's own.
        let mut bar = None;
        let mut next = self.peek_u32(window + WINDOW_CONTROLS)?;
        for _ in 0..256 {
            if next == 0 {
                break;
            }
            let control = self.peek_u32(next)?;
            if control == 0 {
                break;
            }
            let usable = self.peek_u8(control + CONTROL_VISIBLE)? != 0
                && self.peek_u8(control + CONTROL_HILITE)? != CONTROL_INACTIVE;
            if usable && self.cdef_id(text_base, self.peek_u32(control + CONTROL_DEF)?) == Some(kind) {
                let rect = self.peek_rect(control + CONTROL_RECT)?;
                let outer = if horizontal { rect.bottom } else { rect.right };
                if bar.is_none_or(|(best, _)| outer > best) {
                    bar = Some((outer, rect));
                }
            }
            next = self.peek_u32(control)?;
        }
        let (_, rect) = bar?;

        // The scroll bar definitions (CDEF 64 and 65) put both arrows in a box at the middle
        // of the bar, 51 dots long vertically and 53 horizontally, split at the middle.
        let (x, y) = if horizontal {
            let middle = ((rect.left as i32 + rect.right as i32) >> 1) as i16;
            (if toward_end { middle + 14 } else { middle - 13 }, rect.top + 9)
        } else {
            let middle = ((rect.top as i32 + rect.bottom as i32) >> 1) as i16;
            (rect.left + 9, if toward_end { middle + 13 } else { middle - 12 })
        };
        // A click on a part of the bar that another window covers would go to that window.
        let visible = self.peek_u32(self.peek_u32(window + WINDOW_VISIBLE_RGN)?)?;
        if !self.region_contains(visible, x, y)? {
            return None;
        }
        let bitmap = self.peek_u32(window + WINDOW_BITMAP)?;
        let origin = self.peek_rect(bitmap + BITMAP_RECT)?;
        Some((x.wrapping_sub(origin.left), y.wrapping_sub(origin.top)))
    }

    /// Returns the ID of the control definition resource `handle`, as `RMHdlToRsc` finds it.
    fn cdef_id(&self, text_base: u32, handle: u32) -> Option<i16> {
        for head in RESOURCE_MAPS {
            let mut map_handle = self.peek_u32(text_base + head)?;
            for _ in 0..64 {
                if map_handle == 0 {
                    break;
                }
                let map = self.peek_u32(map_handle)?;
                let types = self.peek_u32(map)?.min(256);
                for t in 0..types {
                    let entry = map + 16 + t * 8;
                    if self.peek_u32(entry)? != CDEF {
                        continue;
                    }
                    let ids = self.peek_u32(self.peek_u32(entry + 4)?)?;
                    for i in 0..self.peek_u32(ids)?.min(4096) {
                        let resource = ids + 16 + i * 16;
                        if self.peek_u32(resource + 8)? == handle {
                            return Some(self.peek_u16(resource)? as i16);
                        }
                    }
                }
                map_handle = self.peek_u32(map + 4)?;
                if map_handle == self.peek_u32(text_base + RESOURCE_MAPS[0])? {
                    break;
                }
            }
        }
        None
    }

    /// Posts a mouse event at the global `point` the way `EMSet` does. Returns `false` when
    /// the queue is full or the event is masked.
    fn post_event(&mut self, text_base: u32, what: u16, point: (i16, i16)) -> bool {
        let em = text_base + EVENT_MANAGER;
        let posted = (|| {
            let entry = self.peek_u32(em + EM_FREE)?;
            let mask = self.peek_u16(em + EM_MASK)?;
            if entry == 0 || (mask >> what) & 1 == 0 {
                return None;
            }
            let when = self.peek_u32(text_base + SYSTEM_TIME)?;
            let how = self.peek_u32(self.peek_u32(text_base + KEYBOARD_STATE)? + 16)?;
            let tail = self.peek_u32(em + EM_TAIL)?;
            let next_free = self.peek_u32(entry + ENTRY_NEXT)?;
            Some((entry, next_free, tail, when, how))
        })();
        let Some((entry, next_free, tail, when, how)) = posted else {
            return false;
        };
        let place = ((point.0 as u16 as u32) << 16) | point.1 as u16 as u32;
        let writes = [
            (em + EM_FREE, next_free),
            (entry + ENTRY_NEXT, 0),
            (entry + ENTRY_WHOM, 0),
            (entry + ENTRY_WHEN, when),
            (entry + ENTRY_WHERE, place),
            (entry + ENTRY_HOW, how),
            (text_base + LAST_HOW, how),
        ];
        for (addr, value) in writes {
            self.poke_u32(addr, value);
        }
        self.poke_u16(entry + ENTRY_KIND, POSTED_ENTRY);
        self.poke_u16(entry + ENTRY_WHAT, what);
        if tail == 0 {
            self.poke_u32(em + EM_HEAD, entry);
        } else {
            self.poke_u32(tail + ENTRY_NEXT, entry);
        }
        self.poke_u32(em + EM_TAIL, entry);
        true
    }

    fn poke_u16(&mut self, addr: u32, value: u16) {
        self.poke_u8(addr, (value >> 8) as u8);
        self.poke_u8(addr + 1, value as u8);
    }

    fn poke_u32(&mut self, addr: u32, value: u32) {
        self.poke_u16(addr, (value >> 16) as u16);
        self.poke_u16(addr + 2, value as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::CpuSpeedMode;

    #[test]
    fn settings_reverse_and_multiply_notches() {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        bus.wheel_config = WheelConfig {
            reverse: true,
            clicks_per_notch: 3,
            trackpad_pixels: 40,
        };
        bus.post_mouse_wheel(1, false);
        bus.post_mouse_wheel(-2, true);
        assert_eq!(
            bus.pending_wheel,
            PendingWheel {
                vertical: -3,
                horizontal: 6
            }
        );
    }

    #[test]
    fn settings_stay_in_range() {
        let wild = WheelConfig {
            reverse: false,
            clicks_per_notch: 0,
            trackpad_pixels: 9999,
        };
        assert_eq!(
            wild.clamped(),
            WheelConfig {
                reverse: false,
                clicks_per_notch: 1,
                trackpad_pixels: 400
            }
        );
    }
}

