//! CPU views of the four packed 512×512, 4-bit graphics pages.
//!
//! The backing store is always 512 KiB of big-endian words. Changing R20 changes
//! the CPU view, never the stored pixels. See docs/GRAPHICS.md.

/// A CPU byte's lane within a physical packed word. A zero mask is the unused
/// upper byte of an indexed pixel; it reads zero and ignores writes.
#[derive(Clone, Copy)]
pub(crate) struct ByteLane {
    pub offset: usize,
    pub shift: u32,
    pub mask: u16,
}

pub(crate) fn byte_lane(addr: u32, r20: u16) -> Option<ByteLane> {
    let offset = (addr - 0x00c0_0000) as usize;
    let mode = (r20 >> 8) & 7;
    let odd = offset & 1 != 0;
    let (word, shift, mask) = match mode {
        0 => (offset & 0x7fffe, ((offset >> 19) * 4) as u32, 0x0f),
        1 if offset < 0x100000 => (offset & 0x7fffe, ((offset >> 19) * 8) as u32, 0xff),
        2 | 3 if offset < 0x80000 => (offset & !1, if odd { 0 } else { 8 }, 0xff),
        4..=7 => {
            let pixel = offset >> 1;
            let x = pixel & 1023;
            let y = pixel >> 10;
            (
                ((y & 511) * 512 + (x & 511)) * 2,
                (((y >> 9) * 2 + (x >> 9)) * 4) as u32,
                0x0f,
            )
        }
        _ => return None,
    };
    Some(ByteLane {
        offset: word,
        shift,
        mask: if mode != 2 && mode != 3 && !odd {
            0
        } else {
            mask
        },
    })
}

impl ByteLane {
    pub fn read(self, bytes: Option<&[u8]>) -> u8 {
        let word = bytes.map_or(0, |b| {
            u16::from_be_bytes([b[self.offset], b[self.offset + 1]])
        });
        ((word >> self.shift) & self.mask) as u8
    }

    pub fn write(self, bytes: &mut [u8], value: u8) {
        let word = u16::from_be_bytes([bytes[self.offset], bytes[self.offset + 1]]);
        let mask = self.mask << self.shift;
        let word = (word & !mask) | (((value as u16) << self.shift) & mask);
        bytes[self.offset..self.offset + 2].copy_from_slice(&word.to_be_bytes());
    }
}
