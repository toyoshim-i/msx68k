//! Sprite selection and palette-code scanline buffers.
use crate::{
    bg::{origin, pattern_pixel, word, BG_WIDTH},
    Crtc,
};

pub const SPRITES_PER_LINE: usize = 32;

#[derive(Debug, Clone)]
pub struct SpriteLine {
    pub(crate) codes: [u8; BG_WIDTH],
    pub(crate) priorities: [u8; BG_WIDTH],
}

impl Default for SpriteLine {
    fn default() -> Self {
        Self {
            codes: [0; BG_WIDTH],
            priorities: [0; BG_WIDTH],
        }
    }
}

impl SpriteLine {
    /// Select intersecting, enabled sprites in register order, independently of output gates.
    pub fn fetch(crtc: &Crtc, ram: Option<&[u8]>, y: u32) -> Self {
        let mut line = Self::default();
        let Some(ram) = ram.filter(|ram| ram.len() >= 0x10000) else {
            return line;
        };
        let Some((origin_x, source_y)) = origin(crtc, ram, y) else {
            return line;
        };
        let width = crtc.active_resolution().0.min(BG_WIDTH as u32) as i32;
        let mut selected = 0;
        for index in 0..128 {
            let address = index * 8;
            let priority = (word(ram, address + 6) & 3) as u8;
            if priority == 0 {
                continue;
            }
            let left = (word(ram, address) & 1023) as i32 - 16 + origin_x;
            let row = source_y + 16 - (word(ram, address + 2) & 1023) as i32;
            if !(0..16).contains(&row) || left >= width || left + 16 <= 0 {
                continue;
            }
            if selected == SPRITES_PER_LINE {
                break;
            }
            selected += 1;
            let attributes = word(ram, address + 4);
            let py = if attributes & 0x8000 != 0 {
                15 - row
            } else {
                row
            } as usize;
            for dx in 0..16 {
                let x = left + dx;
                if !(0..width).contains(&x) {
                    continue;
                }
                let px = if attributes & 0x4000 != 0 {
                    15 - dx
                } else {
                    dx
                } as usize;
                let nibble = pattern_pixel(ram, (attributes & 255) as usize, px, py, true);
                let x = x as usize;
                if nibble != 0 && (line.codes[x] == 0 || priority > line.priorities[x]) {
                    line.codes[x] = ((attributes >> 4) as u8 & 0xf0) | nibble;
                    line.priorities[x] = priority;
                }
            }
        }
        line
    }
}
