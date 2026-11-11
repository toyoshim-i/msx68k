//! Records which 4 KB pages of TVRAM and GVRAM have been written, so that a screen compositor
//! uploads only those pages at each vertical blank (see `docs/VMX68K.md` Section 3.3).

use std::ops::Range;

use crate::bus::{GVRAM_BYTES, TVRAM_BYTES};
use crate::crtc::TVRAM_PLANE_BYTES;

/// Granularity of the record.
pub const VRAM_PAGE_BYTES: usize = 4096;

const _: () = assert!(TVRAM_BYTES / VRAM_PAGE_BYTES == 128);
const _: () = assert!(GVRAM_BYTES / VRAM_PAGE_BYTES == 128);

/// One bit per 4 KB page of TVRAM and of GVRAM.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct DirtyVramPages {
    pub tvram: u128,
    pub gvram: u128,
}

impl DirtyVramPages {
    /// Every page of both memories.
    pub const ALL: Self = Self {
        tvram: u128::MAX,
        gvram: u128::MAX,
    };

    /// Returns `true` if no page has been written.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.tvram == 0 && self.gvram == 0
    }

    /// Records a write to the TVRAM byte at `offset`.
    #[inline]
    pub fn mark_tvram(&mut self, offset: usize) {
        self.tvram |= 1 << ((offset / VRAM_PAGE_BYTES) & 127);
    }

    /// Records a write at `in_plane` within each TVRAM plane selected by the low 4 bits of
    /// `planes`.
    pub fn mark_tvram_planes(&mut self, planes: u16, in_plane: usize) {
        for plane in 0..4 {
            if planes & (1 << plane) != 0 {
                self.mark_tvram(plane * TVRAM_PLANE_BYTES + in_plane);
            }
        }
    }

    /// Records a write to every TVRAM page overlapping `range`.
    pub fn mark_tvram_range(&mut self, range: Range<usize>) {
        self.tvram |= page_bits(range);
    }

    /// Records a write to the GVRAM byte at `offset`.
    #[inline]
    pub fn mark_gvram(&mut self, offset: usize) {
        self.gvram |= 1 << ((offset / VRAM_PAGE_BYTES) & 127);
    }

    /// Byte ranges of the written TVRAM pages, adjacent pages merged.
    pub fn tvram_ranges(&self) -> Vec<Range<usize>> {
        ranges(self.tvram)
    }

    /// Byte ranges of the written GVRAM pages, adjacent pages merged.
    pub fn gvram_ranges(&self) -> Vec<Range<usize>> {
        ranges(self.gvram)
    }
}

fn page_bits(range: Range<usize>) -> u128 {
    if range.is_empty() {
        return 0;
    }
    let first = range.start / VRAM_PAGE_BYTES;
    let last = ((range.end - 1) / VRAM_PAGE_BYTES).min(127);
    let upto = if last == 127 { u128::MAX } else { (1 << (last + 1)) - 1 };
    upto & !((1u128 << first) - 1)
}

fn ranges(mut bits: u128) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    while bits != 0 {
        let start = bits.trailing_zeros() as usize;
        let run = (bits >> start).trailing_ones() as usize;
        out.push(start * VRAM_PAGE_BYTES..(start + run) * VRAM_PAGE_BYTES);
        bits = if start + run >= 128 { 0 } else { bits & !((1u128 << (start + run)) - 1) };
    }
    out
}

#[cfg(test)]
// Expected ranges are written as arrays, one-element ones included.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_pages_merge_into_one_range() {
        let mut pages = DirtyVramPages::default();
        pages.mark_tvram(0x1FFF);
        pages.mark_tvram(0x2000);
        pages.mark_tvram(0x7_F000);
        pages.mark_gvram(0);
        assert_eq!(pages.tvram_ranges(), [0x1000..0x3000, 0x7_F000..0x8_0000]);
        assert_eq!(pages.gvram_ranges(), [0..0x1000]);
    }

    #[test]
    fn a_range_marks_every_page_it_overlaps() {
        let mut pages = DirtyVramPages::default();
        pages.mark_tvram_range(0x0FFF..0x1001);
        assert_eq!(pages.tvram_ranges(), [0..0x2000]);
        pages.mark_tvram_range(0..TVRAM_BYTES);
        assert_eq!(pages, DirtyVramPages { tvram: u128::MAX, gvram: 0 });
        assert_eq!(DirtyVramPages::ALL.gvram_ranges(), [0..GVRAM_BYTES]);
    }

    #[test]
    fn simultaneous_plane_writes_mark_each_selected_plane() {
        let mut pages = DirtyVramPages::default();
        pages.mark_tvram_planes(0b0101, 0x10);
        assert_eq!(
            pages.tvram_ranges(),
            [0..0x1000, 2 * TVRAM_PLANE_BYTES..2 * TVRAM_PLANE_BYTES + 0x1000]
        );
    }
}
