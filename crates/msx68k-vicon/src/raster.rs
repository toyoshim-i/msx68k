//! Packed video input shared by the primary compositor and VM textures.
use crate::{ViconUniforms, MAX_FB_HEIGHT};
use std::borrow::Cow;
use msx68k_bus::bg::{self, BgLine, BG_WIDTH};
use msx68k_bus::{
    raster::{RasterFrame, RasterState},
    X68kBus, TVRAM_PLANE_BYTES,
};

const _: () = assert!(std::mem::size_of::<ViconUniforms>() == 80);

const STATE_WORDS: usize = 20 + 512;
/// Palette words, row-state records and packed BG codes for at most one full frame.
pub(crate) const VIDEO_BYTES: u64 =
    ((512 + MAX_FB_HEIGHT as usize * (1 + STATE_WORDS + BG_WIDTH / 4)) * 4) as u64;

impl ViconUniforms {
    pub(crate) fn apply_raster(&mut self, state: &RasterState) {
        let registers = &state.registers;
        self.text_scroll_x = registers.text_scroll.0 as u32;
        self.text_scroll_y = registers.text_scroll.1 as u32;
        self.gr_scroll_x = registers.graphic_scroll.map(|(x, _)| x as u32);
        self.gr_scroll_y = registers.graphic_scroll.map(|(_, y)| y as u32);
        self.r0_mode = registers.mode as u32;
        self.r1_priority = registers.priority as u32;
        self.r2_display = registers.display as u32;
        self.contrast = registers.contrast as u32;
    }
}

pub(crate) fn completed_frame<'a>(
    bus: &'a X68kBus,
    params: &ViconUniforms,
) -> Option<&'a RasterFrame> {
    // Native extended TVRAM does not follow hardware scanout timing.
    if params.tvram_plane_bytes != TVRAM_PLANE_BYTES as u32 || params.tvram_row_bytes != 128 {
        return None;
    }
    bus.raster_frame().filter(|frame| {
        frame.width == params.width && frame.height == params.height && !frame.lines.is_empty()
    })
}

fn raster_input(bus: &X68kBus, params: &mut ViconUniforms) -> Vec<u32> {
    let Some(frame) = completed_frame(bus, params) else {
        if params.r2_display & 0x7f == 0 {
            return Vec::new();
        }
        return bus
            .vicon
            .text_palette()
            .iter()
            .chain(bus.vicon.graphic_palette())
            .map(|&v| v as u32)
            .collect();
    };
    params.apply_raster(&frame.lines[0].state);
    if frame.lines.len() == 1 {
        if params.r2_display & 0x7f == 0 {
            return Vec::new();
        }
        return frame.lines[0].state.palettes.map(|v| v as u32).to_vec();
    }
    params.reserved[0] = params.height;
    let mut words = vec![0; 512 + params.height as usize];
    for (index, line) in frame.lines.iter().enumerate() {
        let start = line.y as usize;
        let end = frame
            .lines
            .get(index + 1)
            .map_or(params.height, |next| next.y) as usize;
        let offset = words.len() as u32;
        words[512 + start..512 + end].fill(offset);
        let mut state = *params;
        state.reserved = [0; 2];
        state.apply_raster(&line.state);
        words.extend_from_slice(bytemuck::cast_slice(std::slice::from_ref(&state)));
        if state.r2_display & 0x7f != 0 {
            words.extend(line.state.palettes.map(|v| v as u32));
        }
    }
    debug_assert!(words.len() * 4 <= VIDEO_BYTES as usize);
    words
}

/// BG code history is independent of palette snapshots and has bounded pixel lookup.
pub(crate) fn background_input<'a>(bus: &'a X68kBus, params: &ViconUniforms) -> Cow<'a, [u8]> {
    if let Some(frame) = completed_frame(bus, params) {
        return Cow::Borrowed(&frame.background);
    }
    if params.tvram_plane_bytes != TVRAM_PLANE_BYTES as u32
        || params.tvram_row_bytes != 128
        || params.r2_display & 0x40 == 0
        || bg::control(bus.sprite_bg()) & 0x200 == 0
    {
        return Cow::Borrowed(&[]);
    }
    let mut codes = Vec::with_capacity(params.height as usize * BG_WIDTH);
    for y in 0..params.height {
        codes.extend_from_slice(
            &BgLine::fetch(&bus.crtc, bus.sprite_bg(), y).output(bg::control(bus.sprite_bg())),
        );
    }
    Cow::Owned(codes)
}

pub(crate) fn video_input(bus: &X68kBus, params: &mut ViconUniforms) -> Vec<u32> {
    let mut words = raster_input(bus, params);
    let background = background_input(bus, params);
    if background.iter().any(|&code| code != 0) {
        params.reserved[1] = words.len() as u32;
        words.extend(
            background
                .chunks_exact(4)
                .map(|codes| u32::from_le_bytes(codes.try_into().unwrap())),
        );
    }
    debug_assert!(words.len() * 4 <= VIDEO_BYTES as usize);
    words
}
