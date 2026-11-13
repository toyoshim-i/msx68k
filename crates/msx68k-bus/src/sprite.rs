//! Sprite selection and palette-code scanline buffers.
use crate::{
    bg::{origin, pattern_pixel_bank, word, BG_WIDTH},
    Crtc,
};

pub const SPRITES_PER_LINE: usize = 32;

/// Host-side options; changing these does not reset the emulated machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpriteOptions {
    pub unlimited_sprites: bool,
    pub full_pattern_memory: bool,
}

impl SpriteOptions {
    pub fn bits(self) -> u32 {
        self.unlimited_sprites as u32 | ((self.full_pattern_memory as u32) << 1)
    }
    pub fn from_bits(bits: u32) -> Self {
        Self {
            unlimited_sprites: bits & 1 != 0,
            full_pattern_memory: bits & 2 != 0,
        }
    }
}

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
        Self::fetch_with_options(crtc, ram, None, y, SpriteOptions::default())
    }

    pub fn fetch_with_options(
        crtc: &Crtc,
        ram: Option<&[u8]>,
        extra: Option<&[u8]>,
        y: u32,
        options: SpriteOptions,
    ) -> Self {
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
            let flags = word(ram, address + 6);
            let priority = (flags & 3) as u8;
            if priority == 0 {
                continue;
            }
            let left = (word(ram, address) & 1023) as i32 - 16 + origin_x;
            let row = source_y + 16 - (word(ram, address + 2) & 1023) as i32;
            if !(0..16).contains(&row) || left >= width || left + 16 <= 0 {
                continue;
            }
            if !options.unlimited_sprites && selected == SPRITES_PER_LINE {
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
                let nibble = pattern_pixel_bank(
                    ram,
                    extra,
                    options.full_pattern_memory,
                    flags & 4 != 0,
                    (attributes & 255) as usize,
                    px,
                    py,
                    true,
                );
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
