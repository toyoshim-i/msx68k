//! X68000 Video Controller (VICON) and Palette RAM (`$E82000`–`$E83FFF`).
//!
//! Address map:
//! - `$E82000`–`$E821FF`: Graphics Palette (`256` × 16-bit `GGGGGRRRRRBBBBBI`)
//! - `$E82200`–`$E823FF`: Text / Sprite / BG Palette (`256` × 16-bit `GGGGGRRRRRBBBBBI`)
//! - `$E82400`–`$E824FF`: VICON `R0` — Screen Mode (`bits 2..0`)
//! - `$E82500`–`$E825FF`: VICON `R1` — Priority Control (`bits 13..0`)
//! - `$E82600`–`$E826FF`: VICON `R2` — Display Enable & Special Effects (`bits 15..0`)

/// Converts an X68000 16-bit `GGGGGRRRRRBBBBBI` color word into 8-bit `[R, G, B, A]`.
#[inline]
pub const fn grbi_to_rgba8(color: u16) -> [u8; 4] {
    let g6 = (((color >> 11) & 0x1F) << 1) | (color & 1);
    let r6 = (((color >> 6) & 0x1F) << 1) | (color & 1);
    let b6 = (((color >> 1) & 0x1F) << 1) | (color & 1);
    let r8 = ((r6 << 2) | (r6 >> 4)) as u8;
    let g8 = ((g6 << 2) | (g6 >> 4)) as u8;
    let b8 = ((b6 << 2) | (b6 >> 4)) as u8;
    [r8, g8, b8, 0xFF]
}

/// X68000 Video Controller (`$E82000`–`$E83FFF`).
#[derive(Debug, Clone)]
pub struct Vicon {
    /// Graphics palette (`256` entries × 16-bit `GGGGGRRRRRBBBBBI`).
    graphic_palette: [u16; 256],
    palette_revision: u64,
    /// Text / Sprite / BG palette (`256` entries × 16-bit `GGGGGRRRRRBBBBBI`).
    text_palette: [u16; 256],
    /// VICON `R0` (`$E82400`): Screen mode (`bits 2..0`).
    r0_mode: u16,
    /// VICON `R1` (`$E82500`): Priority control (`bits 13..0`).
    r1_priority: u16,
    /// VICON `R2` (`$E82600`): Display ON/OFF & special effects.
    r2_display: u16,
}

impl Default for Vicon {
    fn default() -> Self {
        Self::new()
    }
}

impl Vicon {
    /// Creates a new VICON instance with standard IPLROM default text palette entries.
    pub fn new() -> Self {
        let mut text_palette = [0u16; 256];
        // Default X68000 IPLROM / Human68k text palette:
        text_palette[0] = 0x0000; // Transparent / Black background
        text_palette[1] = 0x5294; // Gray / White text
        text_palette[2] = 0x003E; // Blue / Cyan
        text_palette[3] = 0xFFFE; // Bright White
        Self {
            graphic_palette: [0u16; 256],
            palette_revision: 0,
            text_palette,
            r0_mode: 0x0004,
            r1_priority: 0x12E4,
            r2_display: 0x0020, // Text display ON by default
        }
    }

    /// Resets VICON state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub(crate) fn palette_revision(&self) -> u64 {
        self.palette_revision
    }

    /// Returns the 256-entry Graphics Palette.
    #[inline]
    pub const fn graphic_palette(&self) -> &[u16; 256] {
        &self.graphic_palette
    }

    /// Returns the 256-entry Text / Sprite / BG Palette.
    #[inline]
    pub const fn text_palette(&self) -> &[u16; 256] {
        &self.text_palette
    }

    /// Returns VICON `R0` (`$E82400`).
    #[inline]
    pub const fn r0_mode(&self) -> u16 {
        self.r0_mode
    }

    /// Returns VICON `R1` (`$E82500`).
    #[inline]
    pub const fn r1_priority(&self) -> u16 {
        self.r1_priority
    }

    /// Returns VICON `R2` (`$E82600`).
    #[inline]
    pub const fn r2_display(&self) -> u16 {
        self.r2_display
    }

    /// Returns `true` if the Text plane is enabled for display (`R2` bit 5).
    #[inline]
    pub const fn is_text_enabled(&self) -> bool {
        (self.r2_display & 0x0020) != 0
    }

    /// Returns `true` if Sprite / BG layer is enabled for display (`R2` bit 6).
    #[inline]
    pub const fn is_sprite_enabled(&self) -> bool {
        (self.r2_display & 0x0040) != 0
    }

    /// Returns the 5-bit Graphics page enable mask (`R2` bits 4..0).
    #[inline]
    pub const fn graphic_enable_mask(&self) -> u8 {
        (self.r2_display & 0x001F) as u8
    }

    /// Reads an 8-bit byte from the VICON / Palette address range (`$E82000`–`$E83FFF`).
    pub fn read_u8(&self, addr: u32) -> u8 {
        let a = addr & 0x00FF_FFFF;
        let offset = (a - 0x00E8_2000) as usize;
        let word = if offset < 0x0200 {
            self.graphic_palette[(offset >> 1) & 0xFF]
        } else if offset < 0x0400 {
            self.text_palette[((offset - 0x0200) >> 1) & 0xFF]
        } else if offset < 0x0500 {
            self.r0_mode
        } else if offset < 0x0600 {
            self.r1_priority
        } else if offset < 0x0700 {
            self.r2_display
        } else {
            0
        };
        if (offset & 1) == 0 {
            (word >> 8) as u8
        } else {
            word as u8
        }
    }

    /// Writes an 8-bit byte to the VICON / Palette address range (`$E82000`–`$E83FFF`).
    pub fn write_u8(&mut self, addr: u32, val: u8) {
        let a = addr & 0x00FF_FFFF;
        let offset = (a - 0x00E8_2000) as usize;
        let is_hi = (offset & 1) == 0;
        let update_word = |cur: u16| -> u16 {
            if is_hi {
                (cur & 0x00FF) | ((val as u16) << 8)
            } else {
                (cur & 0xFF00) | (val as u16)
            }
        };

        if offset < 0x0200 {
            self.palette_revision = self.palette_revision.wrapping_add(1);
            let idx = (offset >> 1) & 0xFF;
            self.graphic_palette[idx] = update_word(self.graphic_palette[idx]);
        } else if offset < 0x0400 {
            self.palette_revision = self.palette_revision.wrapping_add(1);
            let idx = ((offset - 0x0200) >> 1) & 0xFF;
            self.text_palette[idx] = update_word(self.text_palette[idx]);
        } else if offset < 0x0500 {
            self.r0_mode = update_word(self.r0_mode) & 0x0007;
        } else if offset < 0x0600 {
            self.r1_priority = update_word(self.r1_priority) & 0x3FFF;
        } else if offset < 0x0700 {
            self.r2_display = update_word(self.r2_display) & 0x9FFF;
        }
    }
}
