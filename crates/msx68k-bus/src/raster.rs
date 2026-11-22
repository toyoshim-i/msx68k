//! Frame-stable register/palette history, sampled at separate fetch and display stages.
use crate::{
    bg,
    cynthia::{CynthiaBuilder, CynthiaFrame, CynthiaTracker},
    Crtc, Vicon,
};

pub const BLANK_PALETTE_ID: u32 = u32::MAX;
static ZERO_PALETTE: [u16; 512] = [0; 512];

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RasterRegisters {
    pub text_scroll: (u16, u16),
    pub graphic_scroll: [(u16, u16); 4],
    pub mode: u16,
    pub priority: u16,
    pub display: u16,
    pub contrast: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterState {
    pub registers: RasterRegisters,
    /// Index into `RasterFrame::palettes` (or `BLANK_PALETTE_ID` when all planes are off).
    pub palette_id: u32,
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
    /// Independent 16-bit palette snapshots (Text/sprite followed by Graphics).
    pub palettes: Vec<[u16; 512]>,
    /// Fetched sprite spans, BG row states, and PCG/map block history; empty when blank.
    pub cynthia: CynthiaFrame,
    vertical_timing: [u16; 3],
}

impl RasterFrame {
    /// Returns the 512-entry palette snapshot for `palette_id`.
    #[inline]
    pub fn palette(&self, palette_id: u32) -> &[u16; 512] {
        self.palettes.get(palette_id as usize).unwrap_or(&ZERO_PALETTE)
    }

    /// CPU reference evaluation of the CYNTHIA layer at `(x, y)`.
    #[inline]
    pub fn sample_cynthia_code(&self, x: u32, y: u32) -> u8 {
        self.cynthia.sample_code(x, y)
    }
}

#[derive(Debug, Default)]
pub(crate) struct RasterCapture {
    current: Option<RasterFrame>,
    completed: Option<RasterFrame>,
    palette_revision: u64,
    last_palette_id: Option<u32>,
    text_scroll: (u16, u16),
    graphic_scroll: [(u16, u16); 4],
    tracker: CynthiaTracker,
    current_cynthia: Option<CynthiaBuilder>,
    prefetch_cynthia: Option<CynthiaBuilder>,
}

impl RasterCapture {
    pub fn completed(&self) -> Option<&RasterFrame> {
        self.completed.as_ref()
    }

    #[inline]
    pub fn mark_sprite_bg_write(&mut self, offset: usize, extra_bank: bool) {
        self.tracker.mark_write(offset, extra_bank);
    }

    pub fn boundary(&mut self, crtc: &Crtc, rose: bool, fell: bool) {
        if fell {
            if let Some(mut frame) = self.current.take() {
                if let Some(builder) = self.current_cynthia.take() {
                    frame.cynthia = builder.finish();
                }
                self.completed = Some(frame);
            } else {
                self.current_cynthia = None;
            }
        }
        if rose {
            let (width, height) = crtc.active_resolution();
            self.current = Some(RasterFrame {
                width,
                height,
                lines: Vec::new(),
                palettes: Vec::new(),
                cynthia: CynthiaFrame::empty(),
                vertical_timing: [crtc.regs[4], crtc.regs[6], crtc.regs[7]],
            });
            self.last_palette_id = None;
            self.current_cynthia = self.prefetch_cynthia.take();
        }
        // Fetch/address state progresses even when the final output is disabled.
        self.text_scroll = (crtc.text_scroll_x(), crtc.text_scroll_y());
        self.graphic_scroll = std::array::from_fn(|p| crtc.graphic_scroll(p));
    }

    pub fn display(&mut self, crtc: &Crtc, vicon: &Vicon, contrast: u8, ram: Option<&[u8]>) {
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
            self.current_cynthia = None;
            self.prefetch_cynthia = None;
            self.last_palette_id = None;
            return;
        }
        let start = (crtc.regs[6] & 0xfff) + 1;
        let end = (crtc.regs[7] & 0xfff) + 1;
        let span = end.saturating_sub(start).max(1) as u32;
        let y = crtc.current_raster().saturating_sub(start) as u32 * height / span;
        if y >= height {
            return;
        }
        if let Some(builder) = self.current_cynthia.as_mut() {
            builder.display_row(crtc.current_raster(), y, vicon.r2_display(), bg::control(ram));
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
                    palette_id: BLANK_PALETTE_ID,
                },
            );
            return;
        }
        let registers = RasterRegisters {
            text_scroll: self.text_scroll,
            graphic_scroll: self.graphic_scroll,
            mode: vicon.r0_mode(),
            priority: vicon.r1_priority(),
            display: vicon.r2_display(),
            contrast: contrast & 15,
        };
        let vicon_rev = vicon.palette_revision();
        let palette_id = match self.last_palette_id {
            Some(prev_id) if self.palette_revision == vicon_rev => prev_id,
            _ => {
                let mut palettes = [0u16; 512];
                palettes[..256].copy_from_slice(vicon.text_palette());
                palettes[256..].copy_from_slice(vicon.graphic_palette());
                self.palette_revision = vicon_rev;
                if let Some(prev_id) = self.last_palette_id.filter(|&id| frame.palettes[id as usize] == palettes) {
                    prev_id
                } else {
                    let id = frame.palettes.len() as u32;
                    frame.palettes.push(palettes);
                    self.last_palette_id = Some(id);
                    id
                }
            }
        };

        if frame
            .lines
            .last()
            .is_some_and(|line| line.state.registers == registers && line.state.palette_id == palette_id)
        {
            return;
        }
        Self::push(
            frame,
            y,
            RasterState {
                registers,
                palette_id,
            },
        );
    }

    /// Prepare independent sprite and BG stages before their final output gates are known.
    pub fn fetch_sprite_bg(
        &mut self,
        crtc: &Crtc,
        ram: Option<&[u8]>,
        extra: Option<&[u8]>,
        options: crate::sprite::SpriteOptions,
    ) {
        if let Some(b) = self.current_cynthia.as_mut() {
            b.rotate_rings();
        }
        if let Some(b) = self.prefetch_cynthia.as_mut() {
            b.rotate_rings();
        }
        let Some(ram) = ram.filter(|r| r.len() >= 0x10000) else {
            return;
        };

        if self.tracker.has_dirty_pcg() {
            let (dirty, any) = self.tracker.take_dirty_pcg();
            if let Some(b) = self.current_cynthia.as_mut() {
                b.apply_dirty_pcg(&dirty, any, ram, extra);
            }
            if let Some(b) = self.prefetch_cynthia.as_mut() {
                b.apply_dirty_pcg(&dirty, any, ram, extra);
            }
        }

        let total = (crtc.regs[4] & 0xfff).max(256) as u32 + 1;
        let start = (crtc.regs[6] & 0xfff) as u32 + 1;
        let end = (crtc.regs[7] & 0xfff) as u32 + 1;
        if start >= end {
            return;
        }
        let (_, height) = crtc.active_resolution();
        let cur_raster = crtc.current_raster() as u32;

        let next = (cur_raster + 1) % total;
        if next >= start && next < end {
            let y = (next - start) * height / (end - start);
            let builder = self.builder_for_raster(next, cur_raster, height, ram, extra, options);
            builder.fetch_bg_at(
                crtc,
                ram,
                extra,
                options.full_pattern_memory,
                y,
                next as u16,
            );
        }

        // Sprite selection/pattern fetch precedes BG fetch by a separate stage.
        let sprite_next = (cur_raster + 2) % total;
        if sprite_next >= start && sprite_next < end {
            let y = (sprite_next - start) * height / (end - start);
            let builder = if self.current.is_some() && sprite_next > cur_raster {
                self.current_cynthia
                    .get_or_insert_with(|| CynthiaBuilder::new(height, ram, extra, options))
            } else {
                self.prefetch_cynthia
                    .get_or_insert_with(|| CynthiaBuilder::new(height, ram, extra, options))
            };
            builder.fetch_sprite_at(
                &mut self.tracker,
                crtc,
                ram,
                extra,
                options,
                y,
                sprite_next as u16,
            );
        }
    }

    fn builder_for_raster(
        &mut self,
        target_raster: u32,
        cur_raster: u32,
        height: u32,
        ram: &[u8],
        extra: Option<&[u8]>,
        options: crate::sprite::SpriteOptions,
    ) -> &mut CynthiaBuilder {
        if self.current.is_some() && target_raster > cur_raster {
            self.current_cynthia
                .get_or_insert_with(|| CynthiaBuilder::new(height, ram, extra, options))
        } else {
            self.prefetch_cynthia
                .get_or_insert_with(|| CynthiaBuilder::new(height, ram, extra, options))
        }
    }

    fn push(frame: &mut RasterFrame, y: u32, state: RasterState) {
        if frame.lines.last().is_some_and(|line| line.y == y) {
            frame.lines.pop();
        }
        frame.lines.push(RasterLine { y, state });
    }
}
