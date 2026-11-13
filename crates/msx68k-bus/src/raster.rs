//! Frame-stable register/palette history, latched at scanline boundaries.
use crate::{Crtc, Vicon};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RasterRegisters {
    pub text_scroll: (u16, u16),
    pub graphic_scroll: [(u16, u16); 4],
    pub mode: u16,
    pub priority: u16,
    pub display: u16,
    pub contrast: u8,
}

impl RasterRegisters {
    fn capture(crtc: &Crtc, vicon: &Vicon, contrast: u8) -> Self {
        Self {
            text_scroll: (crtc.text_scroll_x(), crtc.text_scroll_y()),
            graphic_scroll: std::array::from_fn(|p| crtc.graphic_scroll(p)),
            mode: vicon.r0_mode(),
            priority: vicon.r1_priority(),
            display: vicon.r2_display(),
            contrast: contrast & 15,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterState {
    pub registers: RasterRegisters,
    /// Text/sprite palette followed by graphics palette.
    pub palettes: [u16; 512],
}

#[derive(Debug, Clone)]
pub struct RasterLine {
    /// First output row using this state, inclusive.
    pub y: u32,
    pub state: RasterState,
}

#[derive(Debug, Clone)]
pub struct RasterFrame {
    pub width: u32,
    pub height: u32,
    pub lines: Vec<RasterLine>,
    vertical_timing: [u16; 3],
}

#[derive(Debug, Default)]
pub(crate) struct RasterCapture {
    current: Option<RasterFrame>,
    completed: Option<RasterFrame>,
    palette_revision: u64,
}

impl RasterCapture {
    pub fn completed(&self) -> Option<&RasterFrame> {
        self.completed.as_ref()
    }

    pub fn boundary(&mut self, crtc: &Crtc, vicon: &Vicon, contrast: u8, rose: bool, fell: bool) {
        if fell {
            self.completed = self.current.take();
        }
        if rose {
            let (width, height) = crtc.active_resolution();
            self.current = Some(RasterFrame {
                width,
                height,
                lines: Vec::new(),
                vertical_timing: [crtc.regs[4], crtc.regs[6], crtc.regs[7]],
            });
        }
        if !crtc.sync_state().v_disp {
            return;
        }
        let Some(frame) = self.current.as_mut() else {
            return;
        };
        let (width, height) = crtc.active_resolution();
        // Geometry changes need a new full frame; do not mix incompatible line maps.
        if (width, height) != (frame.width, frame.height)
            || frame.vertical_timing != [crtc.regs[4], crtc.regs[6], crtc.regs[7]]
        {
            self.current = None;
            self.completed = None;
            return;
        }
        let start = (crtc.regs[6] & 0xfff) + 1;
        let end = (crtc.regs[7] & 0xfff) + 1;
        let span = end.saturating_sub(start).max(1) as u32;
        let y = crtc.current_raster().saturating_sub(start) as u32 * height / span;
        if y >= height {
            return;
        }
        // Blank runs need only one OFF marker. Do not sample unrelated registers
        // or copy palettes until a display plane is enabled again.
        if vicon.r2_display() & 0x7f == 0 {
            if frame
                .lines
                .last()
                .is_some_and(|line| line.state.registers.display == 0)
            {
                return;
            }
            Self::push(
                frame,
                y,
                RasterState {
                    registers: RasterRegisters::default(),
                    palettes: [0; 512],
                },
            );
            return;
        }
        let registers = RasterRegisters::capture(crtc, vicon, contrast);
        if frame.lines.last().is_some_and(|line| {
            line.state.registers == registers && self.palette_revision == vicon.palette_revision()
        }) {
            return;
        }
        let mut palettes = [0; 512];
        palettes[..256].copy_from_slice(vicon.text_palette());
        palettes[256..].copy_from_slice(vicon.graphic_palette());
        self.palette_revision = vicon.palette_revision();
        let state = RasterState {
            registers,
            palettes,
        };
        if frame.lines.last().is_some_and(|line| line.state == state) {
            return;
        }
        Self::push(frame, y, state);
    }

    fn push(frame: &mut RasterFrame, y: u32, state: RasterState) {
        if frame.lines.last().is_some_and(|line| line.y == y) {
            frame.lines.pop();
        }
        frame.lines.push(RasterLine { y, state });
    }
}
