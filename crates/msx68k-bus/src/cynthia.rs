//! CYNTHIA sprite and BG scanout representation with active-span coalescing.
//!
//! Collects per-raster BG row states, 128-byte PCG/map block versions, and vertically
//! coalesced 16-pixel-wide sprite spans (`16×h`) using incremental 128-bit scanline sets.

use crate::{
    bg::{h_scale, map_cynthia_x, origin, word, BG_WIDTH},
    sprite::{SpriteOptions, SPRITES_PER_LINE},
    Crtc,
};

/// 16-byte vertically coalesced sprite span (`16 × (y1 - y0)` pixels).
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct SpriteSpan {
    /// `(x0 as i16 as u16 as u32) | ((y0 as u32) << 16)`
    pub word0: u32,
    /// `(y1 as u32) | ((py0 as u32 & 0xF) << 16) | (hflip << 20) | (vflip << 21)
    ///  | (absent_bank << 22) | ((prio & 3) << 24) | ((pal_block & 0xF) << 28)`
    pub word1: u32,
    /// `(prio_order as u32) | ((attr_index as u32) << 16)`
    pub word2: u32,
    /// Index into `CynthiaFrame::pcg_blocks` (unused when `absent_bank != 0`).
    pub word3: u32,
}

impl SpriteSpan {
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn new(
        x0: i16,
        y0: u16,
        y1: u16,
        py0: u8,
        hflip: bool,
        vflip: bool,
        absent_bank: bool,
        prio: u8,
        pal_block: u8,
        attr_index: u8,
        pcg_block_id: u32,
    ) -> Self {
        let prio_order = prio_order_key(prio, attr_index);
        Self {
            word0: (x0 as u16 as u32) | ((y0 as u32) << 16),
            word1: (y1 as u32)
                | (((py0 as u32) & 0xF) << 16)
                | ((hflip as u32) << 20)
                | ((vflip as u32) << 21)
                | ((absent_bank as u32) << 22)
                | (((prio as u32) & 3) << 24)
                | (((pal_block as u32) & 0xF) << 28),
            word2: (prio_order as u32) | ((attr_index as u32) << 16),
            word3: pcg_block_id,
        }
    }

    #[inline]
    pub const fn x0(self) -> i32 {
        (self.word0 as u16) as i16 as i32
    }

    #[inline]
    pub const fn y0(self) -> u32 {
        self.word0 >> 16
    }

    #[inline]
    pub const fn y1(self) -> u32 {
        self.word1 & 0xFFFF
    }

    #[inline]
    pub const fn py0(self) -> u32 {
        (self.word1 >> 16) & 0xF
    }

    #[inline]
    pub const fn hflip(self) -> bool {
        (self.word1 >> 20) & 1 != 0
    }

    #[inline]
    pub const fn vflip(self) -> bool {
        (self.word1 >> 21) & 1 != 0
    }

    #[inline]
    pub const fn absent_bank(self) -> bool {
        (self.word1 >> 22) & 1 != 0
    }

    #[inline]
    pub const fn prio(self) -> u8 {
        ((self.word1 >> 24) & 3) as u8
    }

    #[inline]
    pub const fn pal_block(self) -> u8 {
        ((self.word1 >> 28) & 0xF) as u8
    }

    #[inline]
    pub const fn prio_order(self) -> u16 {
        (self.word2 & 0xFFFF) as u16
    }

    #[inline]
    pub const fn attr_index(self) -> u8 {
        (self.word2 >> 16) as u8
    }

    #[inline]
    pub const fn pcg_block_id(self) -> u32 {
        self.word3
    }
}

/// Depth/order key interleaving sprite priorities with BG0 (258) and BG1 (129).
/// Higher value is in front (`SP3: 259..=386 > BG0: 258 > SP2: 130..=257 > BG1: 129 > SP1: 1..=128`).
#[inline]
pub const fn prio_order_key(prio: u8, attr_index: u8) -> u16 {
    let tie = 127u16.saturating_sub(attr_index as u16);
    match prio {
        1 => 1 + tie,
        2 => 130 + tie,
        3 => 259 + tie,
        _ => 0,
    }
}

/// Per-output-row BG state and CYNTHIA output gates (16 bytes).
#[repr(C)]
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct BgRowState {
    /// `bg0_sx (0..10) | bg0_sy (10..20) | bg0_map_sel (20) | bg0_bank (21) | bg0_en (22)
    ///  | large_tiles (23) | master_en (24) | full_pattern_mem (25) | bg_valid (26) | row_valid (27)`
    pub word0: u32,
    /// `bg1_sx (0..10) | bg1_sy (10..20) | bg1_map_sel (20) | bg1_bank (21) | bg1_en (22) | vicon_sp_en (23) | h_scale (24..26)`
    pub word1: u32,
    /// `(origin_x as i16 as u16 as u32) | ((source_y as u16 as u32) << 16)`
    pub word2: u32,
    /// Index of this row's PCG/map reference table inside `CynthiaFrame::pcg_tables`.
    pub word3: u32,
}

impl BgRowState {
    #[inline]
    pub const fn row_valid(self) -> bool {
        (self.word0 >> 27) & 1 != 0
    }

    #[inline]
    pub const fn bg_valid(self) -> bool {
        (self.word0 >> 26) & 1 != 0
    }

    #[inline]
    pub const fn master_en(self) -> bool {
        (self.word0 >> 24) & 1 != 0
    }

    #[inline]
    pub const fn vicon_sp_en(self) -> bool {
        (self.word1 >> 23) & 1 != 0
    }

    /// Horizontal resolution scale code (`0` = 1:1, `1` = 2x stretch, `2` = 0.5x shrink).
    #[inline]
    pub const fn h_scale(self) -> u8 {
        ((self.word1 >> 24) & 3) as u8
    }

    #[inline]
    pub const fn full_pattern_mem(self) -> bool {
        (self.word0 >> 25) & 1 != 0
    }

    #[inline]
    pub const fn large_tiles(self) -> bool {
        (self.word0 >> 23) & 1 != 0
    }

    #[inline]
    pub const fn bg0_en(self) -> bool {
        (self.word0 >> 22) & 1 != 0
    }

    #[inline]
    pub const fn bg1_en(self) -> bool {
        (self.word1 >> 22) & 1 != 0
    }

    #[inline]
    pub const fn bg0_scroll(self) -> (u32, u32) {
        (self.word0 & 0x3FF, (self.word0 >> 10) & 0x3FF)
    }

    #[inline]
    pub const fn bg1_scroll(self) -> (u32, u32) {
        (self.word1 & 0x3FF, (self.word1 >> 10) & 0x3FF)
    }

    #[inline]
    pub const fn bg0_map_sel(self) -> bool {
        (self.word0 >> 20) & 1 != 0
    }

    #[inline]
    pub const fn bg0_bank(self) -> bool {
        (self.word0 >> 21) & 1 != 0
    }

    #[inline]
    pub const fn bg1_map_sel(self) -> bool {
        (self.word1 >> 20) & 1 != 0
    }

    #[inline]
    pub const fn bg1_bank(self) -> bool {
        (self.word1 >> 21) & 1 != 0
    }

    #[inline]
    pub const fn origin_x(self) -> i32 {
        (self.word2 as u16) as i16 as i32
    }

    #[inline]
    pub const fn source_y(self) -> i32 {
        ((self.word2 >> 16) as u16) as i16 as i32
    }

    #[inline]
    pub const fn pcg_table_id(self) -> u32 {
        self.word3
    }

    #[inline]
    pub const fn is_cynthia_active(self) -> bool {
        self.row_valid() && self.master_en() && self.vicon_sp_en()
    }
}

/// Frame-level CYNTHIA representation shared across CPU reference, Compute, and GLES 2.0 backends.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CynthiaFrame {
    pub height: u32,
    pub spans: Vec<SpriteSpan>,
    pub bg_rows: Vec<BgRowState>,
    pub pcg_blocks: Vec<[u8; 128]>,
    /// `256` for standard PCG/map bank only, `512` when the extra 32 KiB bank is initialized.
    pub pcg_table_stride: u32,
    pub pcg_tables: Vec<u32>,
    pub has_visible_output: bool,
}

impl CynthiaFrame {
    pub fn empty() -> Self {
        Self::default()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        !self.has_visible_output
    }

    /// Builds a CYNTHIA frame directly from the live bus memory (used before the first completed
    /// scanout frame exists).
    pub fn from_live_bus(
        crtc: &Crtc,
        ram: Option<&[u8]>,
        extra: Option<&[u8]>,
        options: SpriteOptions,
        vicon_r2: u16,
        height: u32,
    ) -> Self {
        if vicon_r2 & 0x40 == 0 || height == 0 {
            return Self::empty();
        }
        let Some(ram) = ram.filter(|r| r.len() >= 0x10000) else {
            return Self::empty();
        };
        let bg_ctl = word(ram, 0x808);
        if bg_ctl & 0x200 == 0 {
            return Self::empty();
        }

        let mut tracker = CynthiaTracker::default();
        let mut builder = CynthiaBuilder::new(height, ram, extra, options);
        for y in 0..height {
            let tag = y as u16;
            builder.fetch_bg_at(crtc, ram, extra, options.full_pattern_memory, y, tag);
            builder.fetch_sprite_at(&mut tracker, crtc, ram, extra, options, y, tag);
            builder.display_row(tag, y, vicon_r2, bg_ctl);
            builder.rotate_rings();
        }
        builder.finish()
    }

    /// CPU reference evaluation of one pixel `(x, y)` in `0..BG_WIDTH × 0..height`.
    pub fn sample_code(&self, x: u32, y: u32) -> u8 {
        if !self.has_visible_output || x >= BG_WIDTH as u32 || y >= self.height {
            return 0;
        }
        let Some(&row) = self.bg_rows.get(y as usize) else {
            return 0;
        };
        if !row.is_cynthia_active() {
            return 0;
        }

        let mut best_code = 0u8;
        let mut best_order = 0u16;
        let ix = x as i32;
        for &span in &self.spans {
            if y < span.y0() || y >= span.y1() {
                continue;
            }
            let x0 = span.x0();
            if ix < x0 || ix >= x0 + 16 {
                continue;
            }
            let order = span.prio_order();
            if order <= best_order {
                continue;
            }
            let nibble = if span.absent_bank() {
                15
            } else {
                let mut px = (ix - x0) as usize;
                let mut py = (span.py0() + (y - span.y0())) as usize;
                if span.hflip() {
                    px = 15 - px;
                }
                if span.vflip() {
                    py = 15 - py;
                }
                let blk = &self.pcg_blocks[span.pcg_block_id() as usize];
                let off = (px / 8) * 64 + py * 4 + (px & 7) / 2;
                (blk[off] >> (4 * (1 - (px & 1)))) & 15
            };
            if nibble != 0 {
                best_code = (span.pal_block() << 4) | nibble;
                best_order = order;
            }
        }

        if best_order > 258 {
            return best_code;
        }

        if row.bg_valid() {
            if row.bg0_en() {
                let bg0 = self.sample_bg_plane(row, 0, x);
                if bg0 != 0 {
                    return bg0;
                }
            }
            if best_order > 129 {
                return best_code;
            }
            if row.bg1_en() && !row.large_tiles() {
                let bg1 = self.sample_bg_plane(row, 1, x);
                if bg1 != 0 {
                    return bg1;
                }
            }
        }

        best_code
    }

    /// Rasterizes an entire `512`-pixel CYNTHIA scanline into `out` (`0` = transparent).
    ///
    /// Returns the horizontal scale code (`0` = 1:1, `1` = 2x, `2` = 0.5x) for output row `y`.
    #[inline]
    pub fn row_h_scale(&self, y: u32) -> u8 {
        self.bg_rows
            .get(y as usize)
            .map_or(0, |row| row.h_scale())
    }

    /// Bit-identical to calling [`Self::sample_code`] for `x in 0..512`, in `O(spans + 512)`.
    pub fn rasterize_row(&self, y: u32, out: &mut [u8; BG_WIDTH]) {
        out.fill(0);
        if !self.has_visible_output || y >= self.height {
            return;
        }
        let Some(&row) = self.bg_rows.get(y as usize) else {
            return;
        };
        if !row.is_cynthia_active() {
            return;
        }

        let mut best_order = [0u16; BG_WIDTH];
        for &span in &self.spans {
            if y < span.y0() || y >= span.y1() {
                continue;
            }
            let x0 = span.x0();
            let start_x = x0.max(0);
            let end_x = (x0 + 16).min(BG_WIDTH as i32);
            if start_x >= end_x {
                continue;
            }
            let order = span.prio_order();
            let absent = span.absent_bank();
            let pal_hi = span.pal_block() << 4;
            let raw_py = (span.py0() + (y - span.y0())) as usize;
            let py = if span.vflip() { 15 - raw_py } else { raw_py };
            let hflip = span.hflip();
            let blk = if absent {
                None
            } else {
                Some(&self.pcg_blocks[span.pcg_block_id() as usize])
            };

            for ix in start_x..end_x {
                let x = ix as usize;
                if order <= best_order[x] {
                    continue;
                }
                let nibble = match blk {
                    None => 15,
                    Some(b) => {
                        let raw_px = (ix - x0) as usize;
                        let px = if hflip { 15 - raw_px } else { raw_px };
                        let off = (px / 8) * 64 + py * 4 + (px & 7) / 2;
                        (b[off] >> (4 * (1 - (px & 1)))) & 15
                    }
                };
                if nibble != 0 {
                    out[x] = pal_hi | nibble;
                    best_order[x] = order;
                }
            }
        }

        if row.bg_valid() {
            if row.bg0_en() {
                self.rasterize_bg_plane(row, 0, out, &mut best_order, 258);
            }
            if row.bg1_en() && !row.large_tiles() {
                self.rasterize_bg_plane(row, 1, out, &mut best_order, 129);
            }
        }
    }

    fn rasterize_bg_plane(
        &self,
        row: BgRowState,
        plane: usize,
        out: &mut [u8; BG_WIDTH],
        best_order: &mut [u16; BG_WIDTH],
        bg_key: u16,
    ) {
        let prep = self.prepare_bg_plane(row, plane);
        for x in 0..BG_WIDTH {
            if best_order[x] > bg_key {
                continue;
            }
            let code = prep.sample_x(self, x as u32);
            if code != 0 {
                out[x] = code;
                best_order[x] = bg_key;
            }
        }
    }

    fn prepare_bg_plane(&self, row: BgRowState, plane: usize) -> PreparedBgPlane<'_> {
        let large = row.large_tiles();
        let size = if large { 16usize } else { 8usize };
        let mask = size * 64 - 1;
        let (scroll_x, scroll_y) = if plane == 0 {
            row.bg0_scroll()
        } else {
            row.bg1_scroll()
        };
        let map_sel = if plane == 0 {
            row.bg0_map_sel()
        } else {
            row.bg1_map_sel()
        };
        let bank = if plane == 0 {
            row.bg0_bank()
        } else {
            row.bg1_bank()
        };
        let map_base = if !map_sel { 0x4000usize } else { 0x6000usize };
        let sy = (row.source_y() as usize + scroll_y as usize) & mask;
        let table_base = row.pcg_table_id() as usize * self.pcg_table_stride as usize;
        let map_blk = if bank && !row.full_pattern_mem() {
            None
        } else {
            let bank_offset = if bank { 256usize } else { 0usize };
            let logical_blk = bank_offset + (map_base >> 7) + (sy / size);
            let phys_blk = self.pcg_tables[table_base + logical_blk] as usize;
            Some(&self.pcg_blocks[phys_blk])
        };
        PreparedBgPlane {
            large,
            size,
            mask,
            origin_x: row.origin_x(),
            scroll_x: scroll_x as i32,
            sy,
            table_base,
            map_blk,
        }
    }

    fn sample_bg_plane(&self, row: BgRowState, plane: usize, x: u32) -> u8 {
        self.prepare_bg_plane(row, plane).sample_x(self, x)
    }
}

struct PreparedBgPlane<'a> {
    large: bool,
    size: usize,
    mask: usize,
    origin_x: i32,
    scroll_x: i32,
    sy: usize,
    table_base: usize,
    map_blk: Option<&'a [u8; 128]>,
}

impl<'a> PreparedBgPlane<'a> {
    #[inline]
    fn sample_x(&self, frame: &CynthiaFrame, x: u32) -> u8 {
        let sx = (x as i32 - self.origin_x + self.scroll_x) as usize & self.mask;
        let tile = match self.map_blk {
            Some(blk) => {
                let in_blk = (sx / self.size) * 2;
                u16::from_be_bytes([blk[in_blk], blk[in_blk + 1]])
            }
            None => 0xFFFFu16,
        };
        let mut px = sx & (self.size - 1);
        let mut py = self.sy & (self.size - 1);
        if tile & 0x4000 != 0 {
            px = self.size - 1 - px;
        }
        if tile & 0x8000 != 0 {
            py = self.size - 1 - py;
        }
        let pattern = (tile & 255) as usize;
        let pat_off = if self.large {
            pattern * 128 + (px / 8) * 64 + py * 4 + (px & 7) / 2
        } else {
            pattern * 32 + py * 4 + px / 2
        };
        let phys_pat_blk = frame.pcg_tables[self.table_base + (pat_off >> 7)] as usize;
        let pat_blk = &frame.pcg_blocks[phys_pat_blk];
        let nibble = (pat_blk[pat_off & 127] >> (4 * (1 - (px & 1)))) & 15;
        if nibble == 0 {
            0
        } else {
            ((tile >> 4) as u8 & 0xF0) | nibble
        }
    }
}

/// Incremental sprite Y-set index (`16 KiB` lazily allocated) and dirty write tracker.
#[derive(Debug)]
pub(crate) struct CynthiaTracker {
    y_sets: Option<Box<[[u64; 2]; 1024]>>,
    attr_dirty: u128,
    pcg_dirty: [[u64; 4]; 2],
    pcg_dirty_any: u8,
    index_initialized: bool,
    enabled: [u64; 2],
    x_visible: [u64; 2],
    latched_attr: [[u16; 4]; 128],
    cached_origin_x: i32,
    cached_viewport_w: i32,
    global_attr_gen: u32,
}

impl Default for CynthiaTracker {
    fn default() -> Self {
        Self {
            y_sets: None,
            attr_dirty: 0,
            pcg_dirty: [[0; 4]; 2],
            pcg_dirty_any: 0,
            index_initialized: false,
            enabled: [0; 2],
            x_visible: [0; 2],
            latched_attr: [[0; 4]; 128],
            cached_origin_x: 0,
            cached_viewport_w: 0,
            global_attr_gen: 0,
        }
    }
}

impl CynthiaTracker {
    #[inline]
    pub fn mark_write(&mut self, offset: usize, extra_bank: bool) {
        if extra_bank {
            let blk = (offset - 0x8000) >> 7;
            self.pcg_dirty[1][blk >> 6] |= 1u64 << (blk & 63);
            self.pcg_dirty_any |= 2;
        } else if offset < 0x0400 {
            self.attr_dirty |= 1u128 << (offset >> 3);
        } else if offset >= 0x8000 {
            let blk = (offset - 0x8000) >> 7;
            self.pcg_dirty[0][blk >> 6] |= 1u64 << (blk & 63);
            self.pcg_dirty_any |= 1;
        }
    }

    #[inline]
    pub fn has_dirty_pcg(&self) -> bool {
        self.pcg_dirty_any != 0
    }

    pub fn take_dirty_pcg(&mut self) -> ([[u64; 4]; 2], u8) {
        let dirty = self.pcg_dirty;
        let any = self.pcg_dirty_any;
        self.pcg_dirty = [[0; 4]; 2];
        self.pcg_dirty_any = 0;
        (dirty, any)
    }

    fn sync_dirty(&mut self, ram: &[u8], origin_x: i32, viewport_w: i32) {
        if !self.index_initialized {
            let y_sets = self
                .y_sets
                .get_or_insert_with(|| vec![[0u64; 2]; 1024].into_boxed_slice().try_into().unwrap());
            y_sets.fill([0u64; 2]);
            self.enabled = [0; 2];
            self.x_visible = [0; 2];
            for i in 0..128 {
                let addr = i * 8;
                let x_w = word(ram, addr);
                let y_w = word(ram, addr + 2);
                let attr_w = word(ram, addr + 4);
                let flags_w = word(ram, addr + 6);
                self.latched_attr[i] = [x_w, y_w, attr_w, flags_w];
                let half = i >> 6;
                let bit = 1u64 << (i & 63);
                if flags_w & 3 != 0 {
                    self.enabled[half] |= bit;
                }
                let left = (x_w & 1023) as i32 - 16 + origin_x;
                if left < viewport_w && left + 16 > 0 {
                    self.x_visible[half] |= bit;
                }
                let (lo, hi) = y_span_range(y_w & 1023);
                for row in &mut y_sets[lo..hi] {
                    row[half] |= bit;
                }
            }
            self.attr_dirty = 0;
            self.cached_origin_x = origin_x;
            self.cached_viewport_w = viewport_w;
            self.index_initialized = true;
            self.global_attr_gen = self.global_attr_gen.wrapping_add(1);
            return;
        }

        let Some(y_sets) = self.y_sets.as_deref_mut() else {
            return;
        };

        let geom_changed = origin_x != self.cached_origin_x || viewport_w != self.cached_viewport_w;
        if geom_changed {
            self.cached_origin_x = origin_x;
            self.cached_viewport_w = viewport_w;
            self.x_visible = [0; 2];
            for i in 0..128 {
                let x_w = self.latched_attr[i][0];
                let left = (x_w & 1023) as i32 - 16 + origin_x;
                if left < viewport_w && left + 16 > 0 {
                    self.x_visible[i >> 6] |= 1u64 << (i & 63);
                }
            }
        }

        let mut dirty = self.attr_dirty;
        self.attr_dirty = 0;
        let mut any_changed = false;
        while dirty != 0 {
            let i = dirty.trailing_zeros() as usize;
            dirty &= dirty - 1;
            let addr = i * 8;
            let words = [
                word(ram, addr),
                word(ram, addr + 2),
                word(ram, addr + 4),
                word(ram, addr + 6),
            ];
            let old = self.latched_attr[i];
            if words == old {
                continue;
            }
            any_changed = true;
            let half = i >> 6;
            let bit = 1u64 << (i & 63);

            let old_y = old[1] & 1023;
            let new_y = words[1] & 1023;
            if old_y != new_y {
                let (old_lo, old_hi) = y_span_range(old_y);
                let (new_lo, new_hi) = y_span_range(new_y);
                if old_hi <= new_lo || new_hi <= old_lo {
                    for row in &mut y_sets[old_lo..old_hi] {
                        row[half] &= !bit;
                    }
                    for row in &mut y_sets[new_lo..new_hi] {
                        row[half] |= bit;
                    }
                } else {
                    for y in old_lo..new_lo {
                        y_sets[y][half] &= !bit;
                    }
                    for y in new_hi..old_hi {
                        y_sets[y][half] &= !bit;
                    }
                    for y in new_lo..old_lo {
                        y_sets[y][half] |= bit;
                    }
                    for y in old_hi..new_hi {
                        y_sets[y][half] |= bit;
                    }
                }
            }

            if words[3] & 3 != 0 {
                self.enabled[half] |= bit;
            } else {
                self.enabled[half] &= !bit;
            }

            let left = (words[0] & 1023) as i32 - 16 + origin_x;
            if left < viewport_w && left + 16 > 0 {
                self.x_visible[half] |= bit;
            } else {
                self.x_visible[half] &= !bit;
            }

            self.latched_attr[i] = words;
        }
        if any_changed {
            self.global_attr_gen = self.global_attr_gen.wrapping_add(1);
        }
    }

    #[inline]
    fn candidates_for(&self, source_y: i32) -> u128 {
        let Some(y_sets) = self.y_sets.as_deref() else {
            return 0;
        };
        let row = y_sets[source_y as usize];
        let lo = row[0] & self.enabled[0] & self.x_visible[0];
        let hi = row[1] & self.enabled[1] & self.x_visible[1];
        (lo as u128) | ((hi as u128) << 64)
    }
}

#[inline]
fn y_span_range(sprite_y: u16) -> (usize, usize) {
    let sy = sprite_y as i32;
    let lo = (sy - 16).clamp(0, 1024) as usize;
    let hi = sy.clamp(0, 1024) as usize;
    (lo, hi)
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
struct PendingSpriteEntry {
    x0: i16,
    py0: u8,
    attr_index: u8,
    prio: u8,
    pal_block: u8,
    flags: u8, // bit0: hflip, bit1: vflip, bit2: absent_bank
    pcg_block_id: u32,
}

impl PendingSpriteEntry {
    #[inline]
    const fn hflip(self) -> bool {
        self.flags & 1 != 0
    }

    #[inline]
    const fn vflip(self) -> bool {
        self.flags & 2 != 0
    }

    #[inline]
    const fn absent_bank(self) -> bool {
        self.flags & 4 != 0
    }
}

#[derive(Debug, Clone)]
struct PendingSpriteRow {
    raster: u16,
    origin_x: i32,
    source_y: i32,
    attr_gen: u32,
    pcg_rev: u32,
    options_bits: u32,
    admitted: u128,
    entries: Vec<PendingSpriteEntry>,
    slot_to_entry: [u8; 128],
}

impl Default for PendingSpriteRow {
    fn default() -> Self {
        Self {
            raster: 0,
            origin_x: 0,
            source_y: 0,
            attr_gen: 0,
            pcg_rev: 0,
            options_bits: 0,
            admitted: 0,
            entries: Vec::new(),
            slot_to_entry: [0; 128],
        }
    }
}

#[derive(Debug, Copy, Clone, Default)]
struct ActiveSpan {
    y0: u16,
    entry: PendingSpriteEntry,
}

#[derive(Debug, Copy, Clone, Default)]
struct BgRowFetch {
    raster: u16,
    row: BgRowState,
}

#[derive(Debug, Clone)]
struct StagedDisplayRow {
    y: u32,
    bg: BgRowState,
    sprite: Option<PendingSpriteRow>,
}

/// Builds a [`CynthiaFrame`] across prefetch and active scanlines.
#[derive(Debug)]
pub(crate) struct CynthiaBuilder {
    frame: CynthiaFrame,
    current_block_map: [u32; 512],
    extra_loaded: bool,
    zero_extra_block_id: Option<u32>,
    pcg_map_revision: u32,
    last_bg_table_rev: u32,
    current_bg_table_id: u32,
    bg_pending: Option<BgRowFetch>,
    sprite_ring: [Option<PendingSpriteRow>; 2],
    staged: Option<StagedDisplayRow>,
    free_entries: Vec<Vec<PendingSpriteEntry>>,
    active_mask: u128,
    active_spans: [ActiveSpan; 128],
    prev_committed_y: Option<u32>,
    prev_source_y: i32,
    prev_origin_x: i32,
    prev_attr_gen: u32,
    prev_pcg_rev: u32,
    prev_options_bits: u32,
}

impl CynthiaBuilder {
    pub fn new(height: u32, ram: &[u8], extra: Option<&[u8]>, options: SpriteOptions) -> Self {
        let mut pcg_blocks = Vec::with_capacity(if options.full_pattern_memory { 512 } else { 256 });
        let mut current_block_map = [0u32; 512];
        for blk in 0..256usize {
            let off = 0x8000 + blk * 128;
            let mut block = [0u8; 128];
            block.copy_from_slice(&ram[off..off + 128]);
            pcg_blocks.push(block);
            current_block_map[blk] = blk as u32;
        }

        let mut extra_loaded = false;
        let mut zero_extra_block_id = None;
        let stride = if options.full_pattern_memory {
            extra_loaded = true;
            if let Some(ex) = extra.filter(|e| e.len() >= 0x8000) {
                for blk in 0..256usize {
                    let off = blk * 128;
                    let mut block = [0u8; 128];
                    block.copy_from_slice(&ex[off..off + 128]);
                    current_block_map[256 + blk] = pcg_blocks.len() as u32;
                    pcg_blocks.push(block);
                }
            } else {
                let z = pcg_blocks.len() as u32;
                pcg_blocks.push([0u8; 128]);
                zero_extra_block_id = Some(z);
                for slot in &mut current_block_map[256..512] {
                    *slot = z;
                }
            }
            512
        } else {
            256
        };

        let mut pcg_tables = Vec::with_capacity(stride);
        pcg_tables.extend_from_slice(&current_block_map[..stride]);

        Self {
            frame: CynthiaFrame {
                height,
                spans: Vec::new(),
                bg_rows: vec![BgRowState::default(); height as usize],
                pcg_blocks,
                pcg_table_stride: stride as u32,
                pcg_tables,
                has_visible_output: false,
            },
            current_block_map,
            extra_loaded,
            zero_extra_block_id,
            pcg_map_revision: 1,
            last_bg_table_rev: 1,
            current_bg_table_id: 0,
            bg_pending: None,
            sprite_ring: [None, None],
            staged: None,
            free_entries: Vec::new(),
            active_mask: 0,
            active_spans: [ActiveSpan::default(); 128],
            prev_committed_y: None,
            prev_source_y: 0,
            prev_origin_x: 0,
            prev_attr_gen: 0,
            prev_pcg_rev: 0,
            prev_options_bits: 0,
        }
    }

    pub fn ensure_extra_loaded(&mut self, extra: Option<&[u8]>) {
        if self.extra_loaded {
            return;
        }
        self.extra_loaded = true;
        if let Some(ex) = extra.filter(|e| e.len() >= 0x8000) {
            for blk in 0..256usize {
                let off = blk * 128;
                let mut block = [0u8; 128];
                block.copy_from_slice(&ex[off..off + 128]);
                self.current_block_map[256 + blk] = self.frame.pcg_blocks.len() as u32;
                self.frame.pcg_blocks.push(block);
            }
        } else {
            let z = self.frame.pcg_blocks.len() as u32;
            self.frame.pcg_blocks.push([0u8; 128]);
            self.zero_extra_block_id = Some(z);
            for slot in &mut self.current_block_map[256..512] {
                *slot = z;
            }
        }
        // Widen existing 256-entry tables to 512-entry stride.
        let z_pad = self.zero_extra_block_id.unwrap_or(0);
        let old_tables = std::mem::take(&mut self.frame.pcg_tables);
        let num_tables = old_tables.len() / 256;
        let mut widened = Vec::with_capacity(num_tables * 512);
        for t in 0..num_tables {
            widened.extend_from_slice(&old_tables[t * 256..(t + 1) * 256]);
            widened.extend(std::iter::repeat_n(z_pad, 256));
        }
        self.frame.pcg_tables = widened;
        self.frame.pcg_table_stride = 512;
        self.pcg_map_revision = self.pcg_map_revision.wrapping_add(1);
    }

    pub fn apply_dirty_pcg(&mut self, dirty: &[[u64; 4]; 2], any: u8, ram: &[u8], extra: Option<&[u8]>) {
        if any == 0 {
            return;
        }
        let mut changed = false;
        if any & 1 != 0 {
            for (word_idx, &bits) in dirty[0].iter().enumerate() {
                let mut m = bits;
                while m != 0 {
                    let b = m.trailing_zeros() as usize;
                    m &= m - 1;
                    let blk = word_idx * 64 + b;
                    let off = 0x8000 + blk * 128;
                    let mut block = [0u8; 128];
                    block.copy_from_slice(&ram[off..off + 128]);
                    self.current_block_map[blk] = self.frame.pcg_blocks.len() as u32;
                    self.frame.pcg_blocks.push(block);
                    changed = true;
                }
            }
        }
        if any & 2 != 0 && self.extra_loaded {
            if let Some(ex) = extra.filter(|e| e.len() >= 0x8000) {
                for (word_idx, &bits) in dirty[1].iter().enumerate() {
                    let mut m = bits;
                    while m != 0 {
                        let b = m.trailing_zeros() as usize;
                        m &= m - 1;
                        let blk = word_idx * 64 + b;
                        let off = blk * 128;
                        let mut block = [0u8; 128];
                        block.copy_from_slice(&ex[off..off + 128]);
                        self.current_block_map[256 + blk] = self.frame.pcg_blocks.len() as u32;
                        self.frame.pcg_blocks.push(block);
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.pcg_map_revision = self.pcg_map_revision.wrapping_add(1);
        }
    }

    pub fn rotate_rings(&mut self) {
        self.bg_pending = None;
        if let Some(mut evicted) = self.sprite_ring[0].take() {
            evicted.entries.clear();
            self.free_entries.push(evicted.entries);
        }
        self.sprite_ring.rotate_left(1);
    }

    pub fn fetch_bg_at(
        &mut self,
        crtc: &Crtc,
        ram: &[u8],
        extra: Option<&[u8]>,
        full_pattern_memory: bool,
        y: u32,
        target_raster: u16,
    ) {
        if full_pattern_memory {
            self.ensure_extra_loaded(extra);
        }
        let mut row = BgRowState::default();
        if let Some((origin_x, source_y)) = origin(crtc, ram, y) {
            if self.last_bg_table_rev != self.pcg_map_revision {
                let stride = self.frame.pcg_table_stride as usize;
                self.current_bg_table_id = (self.frame.pcg_tables.len() / stride) as u32;
                self.frame
                    .pcg_tables
                    .extend_from_slice(&self.current_block_map[..stride]);
                self.last_bg_table_rev = self.pcg_map_revision;
            }
            let control = word(ram, 0x808);
            let large = word(ram, 0x810) & 1 != 0;
            let bg0_sx = (word(ram, 0x800) & 1023) as u32;
            let bg0_sy = (word(ram, 0x802) & 1023) as u32;
            let bg1_sx = (word(ram, 0x804) & 1023) as u32;
            let bg1_sy = (word(ram, 0x806) & 1023) as u32;
            let bg0_map_sel = ((control >> 1) & 1) as u32;
            let bg0_bank = ((control >> 2) & 1) as u32;
            let bg1_map_sel = ((control >> 4) & 1) as u32;
            let bg1_bank = ((control >> 5) & 1) as u32;

            let scale = h_scale(crtc, ram) as u32;
            row.word0 = bg0_sx
                | (bg0_sy << 10)
                | (bg0_map_sel << 20)
                | (bg0_bank << 21)
                | ((large as u32) << 23)
                | ((full_pattern_memory as u32) << 25)
                | (1 << 26); // bg_valid
            row.word1 =
                bg1_sx | (bg1_sy << 10) | (bg1_map_sel << 20) | (bg1_bank << 21) | (scale << 24);
            row.word2 = (origin_x as i16 as u16 as u32) | (((source_y as i16 as u16) as u32) << 16);
            row.word3 = self.current_bg_table_id;
        }
        self.bg_pending = Some(BgRowFetch {
            raster: target_raster,
            row,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fetch_sprite_at(
        &mut self,
        tracker: &mut CynthiaTracker,
        crtc: &Crtc,
        ram: &[u8],
        extra: Option<&[u8]>,
        options: SpriteOptions,
        y: u32,
        target_raster: u16,
    ) {
        if options.full_pattern_memory {
            self.ensure_extra_loaded(extra);
        }
        let Some((origin_x, source_y)) = origin(crtc, ram, y) else {
            self.sprite_ring[1] = None;
            return;
        };
        let scale = h_scale(crtc, ram);
        let viewport_w = map_cynthia_x(crtc.active_resolution().0, scale).min(BG_WIDTH as u32) as i32;
        tracker.sync_dirty(ram, origin_x, viewport_w);

        let mut cand = tracker.candidates_for(source_y);
        let mut admitted = 0u128;
        let mut entries = self
            .free_entries
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(SPRITES_PER_LINE));
        entries.clear();
        let mut slot_to_entry = [0u8; 128];

        let limit = if options.unlimited_sprites {
            128
        } else {
            SPRITES_PER_LINE
        };
        while cand != 0 && entries.len() < limit {
            let i = cand.trailing_zeros() as usize;
            cand &= cand - 1;
            let [x_w, y_w, attr_w, flags_w] = tracker.latched_attr[i];
            let prio = (flags_w & 3) as u8;
            let spcs = flags_w & 4 != 0;
            let absent_bank = spcs && !options.full_pattern_memory;
            let x0 = ((x_w & 1023) as i32 - 16 + origin_x) as i16;
            let py0 = (source_y + 16 - (y_w & 1023) as i32) as u8;
            let hflip = attr_w & 0x4000 != 0;
            let vflip = attr_w & 0x8000 != 0;
            let pal_block = ((attr_w >> 8) & 0xF) as u8;
            let pat = (attr_w & 255) as usize;
            let pcg_block_id = if absent_bank {
                0
            } else {
                let blk_idx = if spcs { 256 + pat } else { pat };
                self.current_block_map[blk_idx]
            };

            admitted |= 1u128 << i;
            slot_to_entry[i] = entries.len() as u8;
            entries.push(PendingSpriteEntry {
                x0,
                py0,
                attr_index: i as u8,
                prio,
                pal_block,
                flags: (hflip as u8) | ((vflip as u8) << 1) | ((absent_bank as u8) << 2),
                pcg_block_id,
            });
        }

        self.sprite_ring[1] = Some(PendingSpriteRow {
            raster: target_raster,
            origin_x,
            source_y,
            attr_gen: tracker.global_attr_gen,
            pcg_rev: self.pcg_map_revision,
            options_bits: options.bits(),
            admitted,
            entries,
            slot_to_entry,
        });
    }

    pub fn display_row(&mut self, raster: u16, y: u32, vicon_r2: u16, bg_control: u16) {
        if y >= self.frame.height {
            return;
        }
        if let Some(staged) = self.staged.as_ref() {
            if staged.y != y {
                let prev = self.staged.take().unwrap();
                self.commit_staged(prev);
            }
        }

        let mut row = match self.bg_pending {
            Some(bg) if bg.raster == raster => bg.row,
            _ => BgRowState::default(),
        };
        let master_en = (bg_control & 0x200 != 0) as u32;
        let bg0_en = (bg_control & 1 != 0) as u32;
        let bg1_en = (bg_control & 8 != 0) as u32;
        let vicon_sp_en = (vicon_r2 & 0x40 != 0) as u32;
        row.word0 = (row.word0 & !((1 << 22) | (1 << 24))) | (bg0_en << 22) | (master_en << 24) | (1 << 27);
        row.word1 = (row.word1 & !((1 << 22) | (1 << 23))) | (bg1_en << 22) | (vicon_sp_en << 23);

        let sprite = self
            .sprite_ring
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|s| s.raster == raster))
            .and_then(Option::take);

        if let Some(mut prev_staged) = self.staged.replace(StagedDisplayRow {
            y,
            bg: row,
            sprite,
        }) {
            if let Some(mut old_sp) = prev_staged.sprite.take() {
                old_sp.entries.clear();
                self.free_entries.push(old_sp.entries);
            }
        }
    }

    fn commit_staged(&mut self, staged: StagedDisplayRow) {
        let y = staged.y;
        self.frame.bg_rows[y as usize] = staged.bg;

        let active_gate = staged.bg.is_cynthia_active();
        let bg_drawn = staged.bg.bg_valid() && (staged.bg.bg0_en() || (staged.bg.bg1_en() && !staged.bg.large_tiles()));

        let Some(mut sp) = staged.sprite else {
            if self.active_mask != 0 {
                self.close_spans(self.active_mask);
                self.active_mask = 0;
            }
            self.prev_committed_y = None;
            if active_gate && bg_drawn {
                self.frame.has_visible_output = true;
            }
            return;
        };

        if active_gate && (bg_drawn || sp.admitted != 0) {
            self.frame.has_visible_output = true;
        }

        let same_env = self.prev_committed_y == Some(y.wrapping_sub(1))
            && y > 0
            && sp.source_y == self.prev_source_y + 1
            && sp.origin_x == self.prev_origin_x
            && sp.options_bits == self.prev_options_bits;

        if !same_env {
            if self.active_mask != 0 {
                self.close_spans(self.active_mask);
            }
            self.open_spans(sp.admitted, y, &sp);
            self.active_mask = sp.admitted;
        } else {
            let stay = self.active_mask & sp.admitted;
            let mut changed_in_stay = 0u128;
            if sp.attr_gen != self.prev_attr_gen || sp.pcg_rev != self.prev_pcg_rev {
                let mut m = stay;
                while m != 0 {
                    let i = m.trailing_zeros() as usize;
                    m &= m - 1;
                    let new_e = sp.entries[sp.slot_to_entry[i] as usize];
                    let act = self.active_spans[i];
                    let expected_py = act.entry.py0 as u32 + (y - act.y0 as u32);
                    if new_e.x0 != act.entry.x0
                        || new_e.prio != act.entry.prio
                        || new_e.pal_block != act.entry.pal_block
                        || new_e.flags != act.entry.flags
                        || new_e.pcg_block_id != act.entry.pcg_block_id
                        || new_e.py0 as u32 != expected_py
                    {
                        changed_in_stay |= 1u128 << i;
                    }
                }
            }
            let to_close = (self.active_mask & !sp.admitted) | changed_in_stay;
            let to_open = (sp.admitted & !self.active_mask) | changed_in_stay;
            if to_close != 0 {
                self.close_spans(to_close);
            }
            if to_open != 0 {
                self.open_spans(to_open, y, &sp);
            }
            self.active_mask = sp.admitted;
        }

        self.prev_committed_y = Some(y);
        self.prev_source_y = sp.source_y;
        self.prev_origin_x = sp.origin_x;
        self.prev_attr_gen = sp.attr_gen;
        self.prev_pcg_rev = sp.pcg_rev;
        self.prev_options_bits = sp.options_bits;
        sp.entries.clear();
        self.free_entries.push(sp.entries);
    }

    fn close_spans(&mut self, mut mask: u128) {
        let Some(prev_y) = self.prev_committed_y else {
            return;
        };
        let y1 = (prev_y + 1) as u16;
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let act = self.active_spans[i];
            let e = act.entry;
            self.frame.spans.push(SpriteSpan::new(
                e.x0,
                act.y0,
                y1,
                e.py0,
                e.hflip(),
                e.vflip(),
                e.absent_bank(),
                e.prio,
                e.pal_block,
                e.attr_index,
                e.pcg_block_id,
            ));
        }
    }

    fn open_spans(&mut self, mut mask: u128, y: u32, sp: &PendingSpriteRow) {
        let y0 = y as u16;
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let entry = sp.entries[sp.slot_to_entry[i] as usize];
            self.active_spans[i] = ActiveSpan { y0, entry };
        }
    }

    pub fn finish(mut self) -> CynthiaFrame {
        if let Some(staged) = self.staged.take() {
            self.commit_staged(staged);
        }
        if self.active_mask != 0 {
            self.close_spans(self.active_mask);
            self.active_mask = 0;
        }
        if !self.frame.has_visible_output {
            return CynthiaFrame::empty();
        }
        self.frame
    }
}
