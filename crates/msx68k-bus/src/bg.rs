//! Background address generation and palette-code line buffers.
use crate::{sprite::SpriteLine, Crtc};

pub const BG_WIDTH: usize = 512;

#[derive(Debug, Clone)]
pub struct BgLine {
    planes: [[u8; BG_WIDTH]; 2],
}

pub(crate) fn word(ram: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([ram[offset], ram[offset + 1]])
}

impl Default for BgLine {
    fn default() -> Self {
        Self {
            planes: [[0; BG_WIDTH]; 2],
        }
    }
}

impl BgLine {
    /// Fetch both planes independently of their final display gates.
    pub fn fetch(crtc: &Crtc, ram: Option<&[u8]>, y: u32) -> Self {
        Self::fetch_with_bank(crtc, ram, None, y, false)
    }

    pub fn fetch_with_bank(
        crtc: &Crtc,
        ram: Option<&[u8]>,
        extra: Option<&[u8]>,
        y: u32,
        full: bool,
    ) -> Self {
        let mut line = Self::default();
        let Some(ram) = ram.filter(|ram| ram.len() >= 0x10000) else {
            return line;
        };
        let Some((origin_x, source_y)) = origin(crtc, ram, y) else {
            return line;
        };
        let large = word(ram, 0x810) & 1 != 0;
        let size = if large { 16 } else { 8 };
        let mask = size * 64 - 1;
        let control = word(ram, 0x808);
        for plane in 0..if large { 1 } else { 2 } {
            let scroll_x = (word(ram, 0x800 + plane * 4) & 1023) as usize;
            let scroll_y = (word(ram, 0x802 + plane * 4) & 1023) as usize;
            let select = (control >> (1 + plane * 3)) & 1;
            let bank = control & (4 << (plane * 3)) != 0;
            let map = if select == 0 { 0xc000 } else { 0xe000 };
            let sy = (source_y as usize + scroll_y) & mask;
            for x in 0..BG_WIDTH {
                let sx = (x as i32 - origin_x + scroll_x as i32) as usize & mask;
                let at = map + ((sy / size) * 64 + sx / size) * 2;
                let tile = u16::from_be_bytes([
                    bank_byte(ram, extra, full, bank, at),
                    bank_byte(ram, extra, full, bank, at + 1),
                ]);
                let mut px = sx & (size - 1);
                let mut py = sy & (size - 1);
                if tile & 0x4000 != 0 {
                    px = size - 1 - px;
                }
                if tile & 0x8000 != 0 {
                    py = size - 1 - py;
                }
                let pattern = (tile & 255) as usize;
                let nibble = pattern_pixel(ram, pattern, px, py, large);
                if nibble != 0 {
                    line.planes[plane][x] = ((tile >> 4) as u8 & 0xf0) | nibble;
                }
            }
        }
        line
    }

    /// Resolve BG0 over BG1, retaining palette codes until final color conversion.
    pub fn output(&self, control: u16) -> [u8; BG_WIDTH] {
        self.compose(control, None)
    }

    pub fn output_with_sprites(&self, control: u16, sprites: &SpriteLine) -> [u8; BG_WIDTH] {
        self.compose(control, Some(sprites))
    }

    fn compose(&self, control: u16, sprites: Option<&SpriteLine>) -> [u8; BG_WIDTH] {
        if control & 0x200 == 0 {
            return [0; BG_WIDTH];
        }
        std::array::from_fn(|x| {
            let bg0 = if control & 1 != 0 {
                self.planes[0][x]
            } else {
                0
            };
            let bg1 = if control & 8 != 0 {
                self.planes[1][x]
            } else {
                0
            };
            let (code, priority) = sprites.map_or((0, 0), |line| (line.codes[x], line.priorities[x]));
            match priority {
                3 => code,
                2 if bg0 == 0 => code,
                1 if bg0 == 0 && bg1 == 0 => code,
                _ if bg0 != 0 => bg0,
                _ => bg1,
            }
        })
    }
}

/// Common sprite/BG coordinate origin and supported resolution gate.
pub(crate) fn origin(crtc: &Crtc, ram: &[u8], y: u32) -> Option<(i32, i32)> {
    if word(ram, 0x810) & 0x0a != 0 {
        return None;
    }
    let x = ((word(ram, 0x80c) & 63) as i32 - (crtc.regs[2] & 0xfff) as i32 - 4) * 8;
    let y = y as i64 + (word(ram, 0x80e) & 255) as i64 - (crtc.regs[6] & 0xfff) as i64;
    (0..1024).contains(&y).then_some((x, y as i32))
}

pub(crate) fn pattern_pixel(ram: &[u8], pattern: usize, x: usize, y: usize, large: bool) -> u8 {
    pattern_pixel_bank(ram, None, false, false, pattern, x, y, large)
}

pub(crate) fn pattern_pixel_bank(
    ram: &[u8],
    extra: Option<&[u8]>,
    full: bool,
    bank: bool,
    pattern: usize,
    x: usize,
    y: usize,
    large: bool,
) -> u8 {
    let offset = if large {
        pattern * 128 + (x / 8) * 64 + y * 4 + (x & 7) / 2
    } else {
        pattern * 32 + y * 4 + x / 2
    };
    (bank_byte(ram, extra, full, bank, 0x8000 + offset) >> (4 * (1 - (x & 1)))) & 15
}

pub fn control(ram: Option<&[u8]>) -> u16 {
    ram.filter(|ram| ram.len() >= 0x10000)
        .map_or(0, |ram| word(ram, 0x808))
}

pub(crate) fn bank_byte(
    ram: &[u8],
    extra: Option<&[u8]>,
    full: bool,
    bank: bool,
    offset: usize,
) -> u8 {
    if !bank {
        ram.get(offset).copied().unwrap_or(0)
    } else if !full {
        0xff
    } else {
        extra
            .and_then(|ram| ram.get(offset - 0x8000))
            .copied()
            .unwrap_or(0)
    }
}
