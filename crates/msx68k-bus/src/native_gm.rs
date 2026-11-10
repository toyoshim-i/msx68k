//! Native replacements for SX-Window Graph Manager (`GM`) A-Line APIs.
//!
//! Screens larger than TVRAM cannot be drawn by the 68000 routines forever, so each API moves
//! here one at a time. Every API returns [`TrapResult::PassToVector`] for cases it does not
//! support, which leaves them to the original `SYS/FSX.X` code.

use msx68k_cpu::{CpuState, TrapResult, SR_C, SR_N, SR_V, SR_Z};

use crate::bus::{X68kBus, EXT_TVRAM_BASE};

const SX_GET_DISP_RECT: u16 = 0xA432;
const GM_FRAME_RECT: u16 = 0xA172;
const GM_FILL_RECT: u16 = 0xA173;
const GM_INVERT_RECT: u16 = 0xA1A3;
/// Unnamed in the official headers: draws an animated dashed frame and waits for V-DISP.
const GM_MARCHING_RECT: u16 = 0xA1A4;
const GM_FILL_RGN: u16 = 0xA17B;
const GM_COPY: u16 = 0xA17F;
const GM_PUT_RIMG: u16 = 0xA183;
const GM_DUP_HRIMG: u16 = 0xA188;
const GM_DUP_VRIMG: u16 = 0xA189;

/// `text_base` offset of the pointer to the current `Graph` record.
const CURRENT_GRAPH: u32 = 0x5523A;
/// `text_base` offset of the default drawing procedure table.
const DRIVER_TABLE: u32 = 0x5549E;
/// `text_base` offset of the rectangle procedure, used to confirm the kernel build.
const RECT_PROC: u32 = 0x20B68;
/// `text_base` offsets of the latched and last drawn cursor positions.
const CURSOR_POS: u32 = 0x549F6;
const CURSOR_DRAWN_POS: u32 = 0x5CA44 + 0x1E;
/// Margin around the cursor position within which drawing is left to the 68000 cursor shielding.
const CURSOR_MARGIN: i16 = 64;
/// `text_base` offsets of the routines that draw and erase the software cursor in VRAM.
const CURSOR_DRAW: u32 = 0x521BA;
const CURSOR_ERASE: u32 = 0x521DC;
/// Opening instructions shared by both cursor routines, used to confirm the kernel build.
const CURSOR_PROLOGUE: [u8; 8] = [0x48, 0xE7, 0x00, 0x0C, 0x28, 0x68, 0x00, 0x00];
/// `text_base` offset of the pointer to the `Mouse` record.
const MOUSE_RECORD: u32 = 0x633CA;

// `Mouse` and `MsCsr` record offsets (`SXDEF2.H`).
const MOUSE_POSITION: u32 = 16;
const MOUSE_CURSOR: u32 = 40;
const MOUSE_PLANES: u32 = 44;
const MOUSE_CURSOR_SHOWN: u32 = 50;
const CURSOR_MASK: u32 = 4;
const CURSOR_PATTERN: u32 = 36;
const CURSOR_SIZE: i32 = 16;

// `Graph` record offsets (`SXGRAPH.H`).
const GRAPH_BITMAP: u32 = 0;
const GRAPH_RECT: u32 = 4;
const GRAPH_PROCS: u32 = 12;
const GRAPH_VISIBLE: u32 = 16;
const GRAPH_CLIPPING: u32 = 20;
const GRAPH_DRAW_LEVEL: u32 = 24;
const GRAPH_PEN_MODE: u32 = 26;
const GRAPH_PEN_SIZE: u32 = 32;
const GRAPH_PEN_PAT: u32 = 36;
const GRAPH_EX_PAT: u32 = 40;
const GRAPH_WORK_KIND: u32 = 44;
const GRAPH_FG_COLOR: u32 = 50;
const GRAPH_BG_COLOR: u32 = 52;

// `Bitmap` record offsets (`SXGRAPH.H`).
const BITMAP_TYPE: u32 = 0;
const BITMAP_RECT: u32 = 2;
const BITMAP_BASE: u32 = 10;
const BITMAP_LINE: u32 = 14;
const BITMAP_PAGE: u32 = 16;
const BITMAP_ACTIVE_PAGES: u32 = 20;

/// `Region` records hold a size, bounds, and then row data; a bare rectangle is this long.
const REGION_RECT_SIZE: u32 = 12;
const REGION_DATA: u32 = 12;
const REGION_END: i16 = 32767;

/// Height and width of pen patterns, which are stored as one 16-bit word per row.
const PATTERN_ROWS: usize = 16;

/// Half-open rectangle in the coordinates of its source record.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Rect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

impl Rect {
    fn intersect(self, other: Rect) -> Rect {
        Rect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    fn is_empty(self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }
}

/// The area a fill covers beyond the clipping regions.
#[derive(Debug, Copy, Clone)]
enum FillShape {
    Rect,
    Region(u32),
    /// A rectangle with this hole left undrawn.
    Frame { hole: Rect },
}

/// Row-by-row reader of a `Region`, whose rows toggle the set of covered X intervals.
struct RegionCursor {
    bounds: Rect,
    /// Sorted interval edges covering the current row; `None` for a bare rectangle.
    edges: Option<Vec<i16>>,
    next_row: u32,
}

/// A pen resolved to source bits for each plane and pattern row, in bitmap pixel alignment.
#[derive(Debug, Clone)]
struct PlanePen {
    rows: [[u16; PATTERN_ROWS]; 4],
    op: u8,
}

/// Combines `source` into `dest` within the bits of `mask` for a two-bit logical operation.
fn combine(op: u8, dest: u8, source: u8, mask: u8) -> u8 {
    match op & 3 {
        0 => (dest & !mask) | (source & mask),
        1 => dest & (source | !mask),
        2 => dest | (source & mask),
        _ => dest ^ (source & mask),
    }
}

/// A four-plane `Bitmap` record that native code can reach directly.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct PlanarBitmap {
    rect: Rect,
    base: u32,
    line: u32,
    page: u32,
}

impl PlanarBitmap {
    fn is_screen(&self) -> bool {
        self.base == EXT_TVRAM_BASE
    }

    /// Guest address of the first byte of `row` in `plane`.
    fn row_addr(&self, plane: usize, row: u32) -> u32 {
        self.base + plane as u32 * self.page + row * self.line
    }
}

/// A `GMCopy` style transfer between two planar bitmaps.
struct Transfer {
    source: PlanarBitmap,
    dest: PlanarBitmap,
    /// The destination `Bitmap` record, compared with the current graph's to decide clipping.
    dest_ptr: u32,
    source_rect: Rect,
    dest_rect: Rect,
    mode: u16,
    mask_handle: u32,
    planes: u16,
}

/// Reads the eight bits of packed `bits` starting at `bit_offset`.
fn packed_byte(bits: &[u8], bit_offset: i32) -> u8 {
    if bit_offset < 0 {
        return bits.first().copied().unwrap_or(0) >> (-bit_offset).min(7);
    }
    let index = (bit_offset >> 3) as usize;
    let shift = (bit_offset & 7) as u32;
    let high = bits.get(index).copied().unwrap_or(0);
    if shift == 0 {
        high
    } else {
        (high << shift) | (bits.get(index + 1).copied().unwrap_or(0) >> (8 - shift))
    }
}

impl X68kBus {
    /// Enables native Graph Manager APIs if `text_base` holds the expected kernel.
    pub(crate) fn enable_native_gm(&mut self, text_base: u32) {
        let proc = self.peek_u32(text_base.wrapping_add(DRIVER_TABLE + 8));
        if proc != Some(text_base.wrapping_add(RECT_PROC)) {
            return;
        }
        self.native_gm_text_base = Some(text_base);

        // The host draws the cursor over the screen, so the guest must stop drawing it into
        // VRAM. Its routines keep all bookkeeping outside these two entry points.
        let routines = [CURSOR_DRAW, CURSOR_ERASE].map(|offset| (text_base + offset) as usize);
        let matches = routines.iter().all(|&addr| {
            self.main_ram.get(addr..addr + CURSOR_PROLOGUE.len()) == Some(&CURSOR_PROLOGUE[..])
        });
        if matches {
            for addr in routines {
                self.main_ram[addr..addr + 2].copy_from_slice(&[0x4E, 0x75]); // RTS
            }
            self.host_cursor = true;
        }
    }

    /// Returns the screen bytes the cursor changes, as `(offset, bytes)` pairs aligned to four
    /// bytes within [`X68kBus::active_tvram`], for the presenter to apply over its copy.
    pub fn cursor_overlay(&self) -> Vec<(usize, [u8; 4])> {
        self.cursor_overlay_words().unwrap_or_default()
    }

    fn cursor_overlay_words(&self) -> Option<Vec<(usize, [u8; 4])>> {
        let text_base = self.native_gm_text_base.filter(|_| self.host_cursor)?;
        let mouse = self.peek_u32(text_base + MOUSE_RECORD)?;
        if self.peek_u8(mouse + MOUSE_CURSOR_SHOWN)? == 0 {
            return Some(Vec::new());
        }
        let bitmap = self.peek_u32(mouse)?;
        let screen_base = if self.is_ext_tvram_active() {
            EXT_TVRAM_BASE
        } else {
            0x00E0_0000
        };
        if self.peek_u16(bitmap + BITMAP_TYPE)? != 0
            || self.peek_u32(bitmap + BITMAP_BASE)? != screen_base
        {
            return Some(Vec::new());
        }
        let rect = self.peek_rect(bitmap + BITMAP_RECT)?;
        let line = self.peek_u16(bitmap + BITMAP_LINE)? as usize;
        let page = self.peek_u32(bitmap + BITMAP_PAGE)? as usize;
        let planes = self.peek_u16(mouse + MOUSE_PLANES)?;
        let cursor = self.peek_u32(self.peek_u32(mouse + MOUSE_CURSOR)?)?;
        let x = self.peek_u16(mouse + MOUSE_POSITION)? as i16 as i32
            - self.peek_u16(cursor)? as i16 as i32
            - rect.left as i32;
        let y = self.peek_u16(mouse + MOUSE_POSITION + 2)? as i16 as i32
            - self.peek_u16(cursor + 2)? as i16 as i32
            - rect.top as i32;
        let width = (rect.right - rect.left) as i32;
        let height = (rect.bottom - rect.top) as i32;
        let screen = if self.is_ext_tvram_active() {
            self.ext_tvram.as_deref()?
        } else {
            self.tvram.as_deref()?
        };

        // Same blend as the guest routine: (screen AND NOT mask) XOR pattern, per plane.
        let mut words: Vec<(usize, [u8; 4])> = Vec::new();
        for row in 0..CURSOR_SIZE {
            let vy = y + row;
            if vy < 0 || vy >= height {
                continue;
            }
            let mask = self.peek_u16(cursor + CURSOR_MASK + row as u32 * 2)?;
            for plane in 0..4 {
                if planes & (1 << plane) == 0 {
                    continue;
                }
                let pattern_addr = cursor + CURSOR_PATTERN + (plane * 32 + row * 2) as u32;
                let pattern = self.peek_u16(pattern_addr)?;
                let line_start = plane as usize * page + vy as usize * line;
                for column in 0..CURSOR_SIZE {
                    let vx = x + column;
                    if vx < 0 || vx >= width {
                        continue;
                    }
                    let bit = 15 - column;
                    let (m, p) = ((mask >> bit) & 1, (pattern >> bit) & 1);
                    if m == 0 && p == 0 {
                        continue;
                    }
                    let offset = line_start + (vx as usize >> 3);
                    let aligned = offset & !3;
                    let index = match words.iter().position(|(start, _)| *start == aligned) {
                        Some(index) => index,
                        None => {
                            let mut bytes = [0u8; 4];
                            bytes.copy_from_slice(screen.get(aligned..aligned + 4)?);
                            words.push((aligned, bytes));
                            words.len() - 1
                        }
                    };
                    let byte = &mut words[index].1[offset - aligned];
                    let shift = 7 - (vx as usize & 7);
                    let value = (((*byte >> shift) & 1) as u16 & !m) ^ p;
                    *byte = (*byte & !(1 << shift)) | ((value as u8 & 1) << shift);
                }
            }
        }
        Some(words)
    }

    /// Runs the native implementation of an A-Line API when one applies.
    #[inline]
    pub(crate) fn handle_native_gm(&mut self, state: &mut CpuState, opcode: u16) -> TrapResult {
        let Some(text_base) = self.native_gm_text_base else {
            return TrapResult::PassToVector;
        };
        // The guest answers this from a fixed table indexed by CRT mode, which knows nothing of
        // extended resolutions, so it is needed for correctness rather than as a migration step.
        if opcode == SX_GET_DISP_RECT {
            return match self.sx_get_disp_rect(state) {
                Some(()) => TrapResult::Handled { extra_cycles: 0 },
                None => TrapResult::PassToVector,
            };
        }
        if !self.native_gm_enabled || !self.is_ext_tvram_active() {
            return TrapResult::PassToVector;
        }
        let handled = match opcode {
            GM_FRAME_RECT => self.gm_frame_rect(state, text_base),
            GM_FILL_RECT => self.gm_fill_rect(state, text_base),
            GM_FILL_RGN => self.gm_fill_rgn(state, text_base),
            GM_COPY => self.gm_copy(state, text_base),
            GM_PUT_RIMG => self.gm_put_rimg(state, text_base),
            GM_DUP_HRIMG => self.gm_dup_hrimg(state, text_base),
            GM_DUP_VRIMG => self.gm_dup_vrimg(state, text_base),
            GM_INVERT_RECT => self.gm_invert_rect(state, text_base),
            GM_MARCHING_RECT => self.gm_marching_rect(state, text_base),
            _ => None,
        };
        match handled {
            Some(()) => {
                self.native_gm_calls += 1;
                TrapResult::Handled { extra_cycles: 0 }
            }
            None => TrapResult::PassToVector,
        }
    }

    fn sx_get_disp_rect(&mut self, state: &mut CpuState) -> Option<()> {
        if !self.is_extended_crtmod_active() {
            return None;
        }
        let rect_ptr = self.peek_u32(state.a[7])?;
        let (width, height) = self.crtc.active_resolution();
        let bytes = [0u16, 0, width as u16, height as u16];
        for (i, byte) in bytes.iter().flat_map(|word| word.to_be_bytes()).enumerate() {
            if !self.poke_u8(rect_ptr.wrapping_add(i as u32), byte) {
                return None;
            }
        }
        return_result(state, 0);
        Some(())
    }

    fn gm_copy(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let args = state.a[7];
        let source_ptr = self.peek_u32(args)?;
        let dest_ptr = self.peek_u32(args + 4)?;
        let source_rect = self.peek_rect(self.peek_u32(args + 8)?)?;
        let dest_rect = self.peek_rect(self.peek_u32(args + 12)?)?;
        let mode = self.peek_u16(args + 16)?;
        let mask_handle = self.peek_u32(args + 18)?;

        let (source, source_pages) = self.peek_planar_target(source_ptr)?;
        let (dest, dest_pages) = self.peek_planar_target(dest_ptr)?;
        if source_pages != dest_pages {
            return None;
        }
        let transfer = Transfer {
            source,
            dest,
            dest_ptr,
            source_rect,
            dest_rect,
            mode,
            mask_handle,
            planes: dest_pages,
        };
        self.copy_bits(state, text_base, transfer)
    }

    /// Draws a `RectImg` with its top left at `pt` through the copy path, as the guest does with
    /// a temporary `Bitmap` over the image data.
    fn gm_put_rimg(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let image = self.peek_u32(state.a[7])?;
        let x = self.peek_u16(state.a[7] + 4)? as i16;
        let y = self.peek_u16(state.a[7] + 6)? as i16;
        self.put_rimg(state, text_base, image, x, y)
    }

    /// `GMDupHRImg`: repeats each row of an image at most 16 dots wide across `width` dots.
    ///
    /// Like the guest, every row becomes a 16-dot extended pattern word whose period is the image
    /// width, phased by the bitmap's left edge, and is written to planes 0 and 1.
    fn gm_dup_hrimg(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let image = self.peek_u32(state.a[7])?;
        let x = self.peek_u16(state.a[7] + 4)? as i16;
        let y = self.peek_u16(state.a[7] + 6)? as i16;
        let length = self.peek_u16(state.a[7] + 8)? as i16;
        let bounds = self.peek_rect(image)?;
        let width = bounds.right.wrapping_sub(bounds.left);
        let height = bounds.bottom.wrapping_sub(bounds.top);
        if !(1..=16).contains(&width) || height <= 0 {
            return None;
        }
        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;
        let bitmap_rect = self.peek_rect(self.peek_u32(graph + GRAPH_BITMAP)? + BITMAP_RECT)?;
        let phase = (bitmap_rect.left as u16 as u32) % width as u32;
        let mask = !(0xFFFFu16 >> width);
        let data = image + 8;
        let plane_stride = height as u32 * 2;
        for row in 0..height {
            let mut rows = [[0u16; PATTERN_ROWS]; 4];
            for (plane, plane_rows) in rows.iter_mut().enumerate() {
                let word = self.peek_u16(data + plane as u32 * plane_stride + row as u32 * 2)?;
                let mut bits = ((word & mask) as u32) << 16;
                let mut filled = bits;
                let mut span = width as u32;
                while span < 15 + width as u32 {
                    bits >>= span;
                    filled |= bits;
                    bits = filled;
                    span *= 2;
                }
                *plane_rows = [((bits << phase) >> 16) as u16; PATTERN_ROWS];
            }
            let pen = PlanePen { rows, op: 0 };
            let rect = Rect {
                left: x,
                top: y.wrapping_add(row),
                right: x.wrapping_add(length),
                bottom: y.wrapping_add(row + 1),
            };
            self.fill_with(state, text_base, rect, FillShape::Rect, Some((pen, 0b0011)))?;
        }
        return_result(state, 0);
        Some(())
    }

    /// `GMDupVRImg`: repeats an image at most 16 dots tall down `height` dots.
    ///
    /// The guest reads the image as four planes and picks the source row from the destination
    /// row's unsigned 16-bit coordinate modulo the image height, not from `pt`.
    fn gm_dup_vrimg(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let image = self.peek_u32(state.a[7])?;
        let x = self.peek_u16(state.a[7] + 4)? as i16;
        let y = self.peek_u16(state.a[7] + 6)? as i16;
        let length = self.peek_u16(state.a[7] + 8)? as i16;
        let bounds = self.peek_rect(image)?;
        let width = bounds.right.wrapping_sub(bounds.left);
        let height = bounds.bottom.wrapping_sub(bounds.top);
        if !(1..=16).contains(&height) || width <= 0 || length < 0 {
            return None;
        }
        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;
        let dest_ptr = self.peek_u32(graph + GRAPH_BITMAP)?;
        let (dest, planes) = self.peek_planar_target(dest_ptr)?;
        let line = ((width as u32 + 15) >> 4) * 2;
        let source = PlanarBitmap {
            rect: Rect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            },
            base: image + 8,
            line,
            page: line * height as u32,
        };
        if source.base as usize >= self.main_ram.len() {
            return None;
        }
        for dest_y in y..y.wrapping_add(length) {
            let row = ((dest_y as u16 as u32) % height as u32) as i16;
            let transfer = Transfer {
                source,
                dest,
                dest_ptr,
                source_rect: Rect {
                    left: 0,
                    top: row,
                    right: width,
                    bottom: row + 1,
                },
                dest_rect: Rect {
                    left: x,
                    top: dest_y,
                    right: x.wrapping_add(width),
                    bottom: dest_y + 1,
                },
                mode: 0,
                mask_handle: 0,
                planes,
            };
            self.copy_bits(state, text_base, transfer)?;
        }
        return_result(state, 0);
        Some(())
    }

    /// Copies the image at `image` with its top left at `(x, y)`.
    fn put_rimg(&mut self, state: &mut CpuState, text_base: u32, image: u32, x: i16, y: i16) -> Option<()> {
        let bounds = self.peek_rect(image)?;
        let width = bounds.right.wrapping_sub(bounds.left);
        let height = bounds.bottom.wrapping_sub(bounds.top);

        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;
        let dest_ptr = self.peek_u32(graph + GRAPH_BITMAP)?;
        let (dest, planes) = self.peek_planar_target(dest_ptr)?;
        let image_rect = Rect {
            left: 0,
            top: 0,
            right: width.max(0),
            bottom: height.max(0),
        };
        let line = ((image_rect.right as u32 + 15) >> 4) * 2;
        let source = PlanarBitmap {
            rect: image_rect,
            base: image + 8,
            line,
            page: line * image_rect.bottom as u32,
        };
        if source.base as usize >= self.main_ram.len() {
            return None;
        }
        let drawn = image_rect;
        let transfer = Transfer {
            source,
            dest,
            dest_ptr,
            source_rect: drawn,
            dest_rect: Rect {
                left: x,
                top: y,
                right: x.wrapping_add(drawn.right),
                bottom: y.wrapping_add(drawn.bottom),
            },
            mode: 0,
            mask_handle: 0,
            planes,
        };
        self.copy_bits(state, text_base, transfer)
    }

    /// Copies between planar bitmaps the way `GMCopy` does.
    fn copy_bits(&mut self, state: &mut CpuState, text_base: u32, transfer: Transfer) -> Option<()> {
        let Transfer {
            source,
            dest,
            dest_ptr,
            source_rect,
            dest_rect,
            mode,
            mask_handle,
            planes: active_pages,
        } = transfer;
        if mode >= 8 {
            return None;
        }
        let same_size = source_rect.right - source_rect.left == dest_rect.right - dest_rect.left
            && source_rect.bottom - source_rect.top == dest_rect.bottom - dest_rect.top;
        if !same_size {
            return None;
        }
        if !source.is_screen() && !dest.is_screen() {
            return None;
        }

        let dx = source_rect.left as i32 - dest_rect.left as i32;
        let dy = source_rect.top as i32 - dest_rect.top as i32;
        let readable = source_rect.intersect(source.rect);
        let mut area = dest_rect
            .intersect(dest.rect)
            .intersect(Rect {
                left: (readable.left as i32 - dx) as i16,
                top: (readable.top as i32 - dy) as i16,
                right: (readable.right as i32 - dx) as i16,
                bottom: (readable.bottom as i32 - dy) as i16,
            });

        let mut regions = Vec::new();
        if mask_handle != 0 {
            let mask = self.peek_u32(mask_handle)?;
            area = area.intersect(self.peek_region_bounds(mask)?);
            regions.push(mask);
        }
        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;
        if self.peek_u32(graph + GRAPH_BITMAP)? == dest_ptr {
            if (self.peek_u16(graph + GRAPH_DRAW_LEVEL)? as i16) < 0
                || self.peek_u16(graph + GRAPH_WORK_KIND)? >= 2
                || self.peek_u32(graph + GRAPH_PROCS)? != 0
            {
                return None;
            }
            let visible = self.peek_region(graph + GRAPH_VISIBLE)?;
            let clipping = self.peek_region(graph + GRAPH_CLIPPING)?;
            area = area
                .intersect(self.peek_rect(graph + GRAPH_RECT)?)
                .intersect(self.peek_region_bounds(visible)?)
                .intersect(self.peek_region_bounds(clipping)?);
            regions.push(visible);
            regions.push(clipping);
        }
        if area.is_empty() {
            return_result(state, 0);
            return Some(());
        }

        let mut cursors = Vec::new();
        for region in regions {
            cursors.push(self.region_cursor(region, area.top)?);
        }
        let mut spans = Vec::new();
        for y in area.top..area.bottom {
            for cursor in &mut cursors {
                self.advance_region(cursor, y)?;
            }
            for (left, right) in clipped_spans(area, &cursors) {
                spans.push((y, left, right));
            }
        }

        let touches = |zones: &[Rect], y: i32, left: i32, right: i32| {
            zones.iter().any(|zone| {
                y >= zone.top as i32
                    && y < zone.bottom as i32
                    && left < zone.right as i32
                    && right > zone.left as i32
            })
        };
        let dest_zones = self.cursor_zones(text_base, dest.rect)?;
        let source_zones = self.cursor_zones(text_base, source.rect)?;
        let near_cursor = spans.iter().any(|&(y, left, right)| {
            let (y, left, right) = (y as i32, left as i32, right as i32);
            (dest.is_screen() && touches(&dest_zones, y, left, right))
                || (source.is_screen() && touches(&source_zones, y + dy, left + dx, right + dx))
        });
        if near_cursor {
            return None;
        }

        // Read everything before writing so overlapping copies see the original pixels.
        let mut copies = Vec::with_capacity(spans.len());
        for &(y, left, right) in &spans {
            let width = (right - left) as u32;
            let row = (y as i32 + dy - source.rect.top as i32) as u32;
            let pixel = (left as i32 + dx - source.rect.left as i32) as u32;
            let bytes = (width as usize).div_ceil(8) + 1;
            let shift = pixel & 7;
            let mut planes = [vec![0u8; bytes], vec![0u8; bytes], vec![0u8; bytes], vec![0u8; bytes]];
            for (plane, packed) in planes.iter_mut().enumerate() {
                if active_pages & (1 << plane) == 0 {
                    continue;
                }
                let start = source.row_addr(plane, row) + (pixel >> 3);
                let mut current = self.peek_u8(start)?;
                for (i, out) in packed.iter_mut().enumerate() {
                    let next = self.peek_u8(start + i as u32 + 1).unwrap_or(0);
                    *out = if shift == 0 {
                        current
                    } else {
                        (current << shift) | (next >> (8 - shift))
                    };
                    current = next;
                }
            }
            copies.push(planes);
        }

        let op = mode as u8;
        let invert = if op & 4 != 0 { 0xFFu8 } else { 0 };
        for (&(y, left, right), planes) in spans.iter().zip(&copies) {
            let row = (y as i32 - dest.rect.top as i32) as u32;
            let first_pixel = (left as i32 - dest.rect.left as i32) as u32;
            let last_pixel = (right as i32 - dest.rect.left as i32) as u32 - 1;
            let first_mask = 0xFFu8 >> (first_pixel & 7);
            let last_mask = 0xFFu8 << (7 - (last_pixel & 7));
            for (plane, packed) in planes.iter().enumerate() {
                if active_pages & (1 << plane) == 0 {
                    continue;
                }
                let line = dest.row_addr(plane, row);
                for byte in (first_pixel >> 3)..=(last_pixel >> 3) {
                    let mut mask = 0xFFu8;
                    if byte == first_pixel >> 3 {
                        mask &= first_mask;
                    }
                    if byte == last_pixel >> 3 {
                        mask &= last_mask;
                    }
                    let bits = packed_byte(packed, byte as i32 * 8 - first_pixel as i32) ^ invert;
                    let addr = line + byte;
                    let current = self.peek_u8(addr)?;
                    self.store_plane_byte(addr, combine(op, current, bits, mask));
                }
            }
        }
        return_result(state, 0);
        Some(())
    }

    fn store_plane_byte(&mut self, addr: u32, value: u8) {
        let (slice, offset) = if addr >= EXT_TVRAM_BASE {
            let Some(buffer) = self.ext_tvram.as_mut() else {
                return;
            };
            (buffer.as_mut_slice(), (addr - EXT_TVRAM_BASE) as usize)
        } else {
            (&mut self.main_ram[..], addr as usize)
        };
        if let Some(slot) = slice.get_mut(offset) {
            *slot = value;
        }
    }

    fn gm_invert_rect(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let rect_ptr = self.peek_u32(state.a[7])?;
        let style = self.peek_u16(state.a[7] + 4)?;
        self.dashed_frame(text_base, rect_ptr, style, false)?;
        return_result(state, 0);
        Some(())
    }

    /// Draws the frame with the stored dash style, advances the style for the next call, and
    /// holds the caller until a V-DISP falling edge as the guest routine does by polling `GPIP`.
    fn gm_marching_rect(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let pc = state.ir_pc;
        if let Some((waiting_pc, target)) = self.vsync_wait {
            if waiting_pc == pc {
                if self.mfp.vdisp_falls >= target {
                    self.vsync_wait = None;
                } else {
                    state.pc = pc;
                }
                return Some(());
            }
        }

        let rect_ptr = self.peek_u32(state.a[7])?;
        let style_ptr = self.peek_u32(state.a[7] + 4)?;
        let style = self.peek_u16(style_ptr)?;
        if style_ptr as usize + 2 > self.main_ram.len() {
            return None;
        }
        self.dashed_frame(text_base, rect_ptr, style, true)?;
        let rotated = style.rotate_right(1).to_be_bytes();
        self.main_ram[style_ptr as usize..style_ptr as usize + 2].copy_from_slice(&rotated);
        return_result(state, 0);

        // The guest waits while V-DISP is high, then while it is low, then while it is high.
        let falls = if self.mfp.vdisp_active() { 2 } else { 1 };
        self.vsync_wait = Some((pc, self.mfp.vdisp_falls + falls));
        state.pc = pc;
        Some(())
    }

    /// Draws a one-pixel rectangle outline with a 16-bit dash `style`, clipped to the current
    /// bitmap only, by XOR or, when `set`, by writing the dash bits to every active plane.
    fn dashed_frame(&mut self, text_base: u32, rect_ptr: u32, style: u16, set: bool) -> Option<()> {
        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;
        if (self.peek_u16(graph + GRAPH_DRAW_LEVEL)? as i16) < 0 {
            return Some(());
        }
        let bitmap_ptr = self.peek_u32(graph + GRAPH_BITMAP)?;
        let (bitmap, active_pages) = self.peek_planar_target(bitmap_ptr)?;
        if bitmap.is_screen() && !self.host_cursor {
            return None;
        }
        let rect = self.peek_rect(rect_ptr)?;
        let area = rect.intersect(bitmap.rect);
        if area.is_empty() {
            return Some(());
        }

        // Rotation follows the guest, which takes it from the low word of the bitmap pointer.
        let rotation = (rect.left.wrapping_sub(bitmap_ptr as u16 as i16) & 15) as u32;
        let reversed = style.reverse_bits();
        let top = style.rotate_right(rotation);
        let bottom = reversed.rotate_right(rotation);

        let mut pixels = Vec::new();
        let row = |y: i16, pattern: u16, pixels: &mut Vec<(i16, i16, bool)>| {
            for x in area.left..area.right {
                let physical = (x as i32 - bitmap.rect.left as i32) & 15;
                pixels.push((x, y, (pattern >> (15 - physical)) & 1 != 0));
            }
        };
        if area.top == rect.top {
            row(area.top, top, &mut pixels);
        }
        let height = rect.bottom as i32 - rect.top as i32 - 2;
        if height >= 0 {
            if height > 0 {
                let start = area.top + (area.top == bitmap.rect.top) as i16;
                let end = area.bottom - (area.bottom == bitmap.rect.bottom) as i16;
                let skip = (start - area.top) as i32;
                let column = |x: i16, pattern: u16, pixels: &mut Vec<(i16, i16, bool)>| {
                    for (k, y) in (start..end).enumerate() {
                        let index = (skip + k as i32) & 15;
                        pixels.push((x, y, (pattern >> (15 - index)) & 1 != 0));
                    }
                };
                if area.left == rect.left {
                    column(area.left, reversed, &mut pixels);
                }
                if rect.right - rect.left - 1 != 0 && area.right == rect.right {
                    column(area.right - 1, style, &mut pixels);
                }
            }
            if area.bottom == rect.bottom {
                row(area.bottom - 1, bottom, &mut pixels);
            }
        }

        for (x, y, on) in pixels {
            let px = (x as i32 - bitmap.rect.left as i32) as u32;
            let py = (y as i32 - bitmap.rect.top as i32) as u32;
            let bit = 0x80u8 >> (px & 7);
            for plane in 0..4 {
                if active_pages & (1 << plane) == 0 {
                    continue;
                }
                let addr = bitmap.row_addr(plane, py) + (px >> 3);
                let current = self.peek_u8(addr)?;
                let value = match (set, on) {
                    (false, true) => current ^ bit,
                    (false, false) => current,
                    (true, true) => current | bit,
                    (true, false) => current & !bit,
                };
                self.store_plane_byte(addr, value);
            }
        }
        Some(())
    }

    /// Reads a planar `Bitmap` in Main RAM or on the screen together with its active planes.
    fn peek_planar_target(&self, addr: u32) -> Option<(PlanarBitmap, u16)> {
        if self.peek_u16(addr + BITMAP_TYPE)? != 0 {
            return None;
        }
        let bitmap = PlanarBitmap {
            rect: self.peek_rect(addr + BITMAP_RECT)?,
            base: self.peek_u32(addr + BITMAP_BASE)?,
            line: self.peek_u16(addr + BITMAP_LINE)? as u32,
            page: self.peek_u32(addr + BITMAP_PAGE)?,
        };
        let reachable = bitmap.is_screen() || (bitmap.base as usize) < self.main_ram.len();
        reachable.then_some((bitmap, self.peek_u16(addr + BITMAP_ACTIVE_PAGES)?))
    }

    fn gm_frame_rect(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let rect_ptr = self.peek_u32(state.a[7])?;
        let rect = self.peek_rect(rect_ptr)?;
        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;
        let width = self.peek_u16(graph + GRAPH_PEN_SIZE)? as i16;
        let height = self.peek_u16(graph + GRAPH_PEN_SIZE + 2)? as i16;
        if width <= 0 || height <= 0 {
            return_result(state, 0);
            return Some(());
        }
        let hole = Rect {
            left: rect.left.saturating_add(width),
            top: rect.top.saturating_add(height),
            right: rect.right.saturating_sub(width),
            bottom: rect.bottom.saturating_sub(height),
        };
        self.fill_area(state, text_base, rect, FillShape::Frame { hole })
    }

    fn gm_fill_rect(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let rect_ptr = self.peek_u32(state.a[7])?;
        let rect = self.peek_rect(rect_ptr)?;
        self.fill_area(state, text_base, rect, FillShape::Rect)
    }

    fn gm_fill_rgn(&mut self, state: &mut CpuState, text_base: u32) -> Option<()> {
        let handle = self.peek_u32(state.a[7])?;
        if handle == 0 {
            return None;
        }
        let region = self.peek_u32(handle)?;
        let bounds = self.peek_region_bounds(region)?;
        self.fill_area(state, text_base, bounds, FillShape::Region(region))
    }

    /// Fills `rect` on the screen, limited to `shape`, with the current pen.
    fn fill_area(
        &mut self,
        state: &mut CpuState,
        text_base: u32,
        rect: Rect,
        shape: FillShape,
    ) -> Option<()> {
        self.fill_with(state, text_base, rect, shape, None)
    }

    /// Fills like [`Self::fill_area`], optionally with a given pen and set of planes instead of
    /// the current pen and the bitmap's active planes.
    fn fill_with(
        &mut self,
        state: &mut CpuState,
        text_base: u32,
        rect: Rect,
        shape: FillShape,
        pen_override: Option<(PlanePen, u16)>,
    ) -> Option<()> {
        let graph = self.peek_u32(text_base.wrapping_add(CURRENT_GRAPH))?;

        let drawable = self.peek_u32(graph + GRAPH_PROCS)? == 0
            && (self.peek_u16(graph + GRAPH_DRAW_LEVEL)? as i16) >= 0
            && self.peek_u16(graph + GRAPH_WORK_KIND)? < 2;
        if !drawable {
            return None;
        }

        let bitmap = self.peek_u32(graph + GRAPH_BITMAP)?;
        let is_screen = self.peek_u16(bitmap + BITMAP_TYPE)? == 0
            && self.peek_u32(bitmap + BITMAP_BASE)? == EXT_TVRAM_BASE
            && self.peek_u16(bitmap + BITMAP_LINE)? as usize == self.ext_tvram_row_bytes
            && self.peek_u32(bitmap + BITMAP_PAGE)? as usize == self.ext_tvram_plane_bytes;
        if !is_screen {
            return None;
        }
        let bitmap_rect = self.peek_rect(bitmap + BITMAP_RECT)?;
        let (pen, active_pages) = match pen_override {
            Some(pen) => pen,
            None => (
                self.resolve_pen(graph, bitmap_rect)?,
                self.peek_u16(bitmap + BITMAP_ACTIVE_PAGES)?,
            ),
        };

        let visible = self.peek_region(graph + GRAPH_VISIBLE)?;
        let clipping = self.peek_region(graph + GRAPH_CLIPPING)?;
        let area = rect
            .intersect(bitmap_rect)
            .intersect(self.peek_rect(graph + GRAPH_RECT)?)
            .intersect(self.peek_region_bounds(visible)?)
            .intersect(self.peek_region_bounds(clipping)?);
        if area.is_empty() {
            return_result(state, 0);
            return Some(());
        }

        let mut cursors = vec![
            self.region_cursor(visible, area.top)?,
            self.region_cursor(clipping, area.top)?,
        ];
        if let FillShape::Region(region) = shape {
            cursors.push(self.region_cursor(region, area.top)?);
        }

        let mut spans = Vec::new();
        for y in area.top..area.bottom {
            for cursor in &mut cursors {
                self.advance_region(cursor, y)?;
            }
            for (left, right) in clipped_spans(area, &cursors) {
                match shape {
                    FillShape::Frame { hole }
                        if !hole.is_empty() && y >= hole.top && y < hole.bottom =>
                    {
                        if left < hole.left {
                            spans.push((y, left, right.min(hole.left)));
                        }
                        if right > hole.right {
                            spans.push((y, left.max(hole.right), right));
                        }
                    }
                    _ => spans.push((y, left, right)),
                }
            }
        }

        let cursor_zones = self.cursor_zones(text_base, bitmap_rect)?;
        let near_cursor = spans.iter().any(|&(y, left, right)| {
            cursor_zones
                .iter()
                .any(|zone| y >= zone.top && y < zone.bottom && left < zone.right && right > zone.left)
        });
        if near_cursor {
            return None;
        }

        for (y, left, right) in spans {
            self.fill_screen_span(
                (y as i32 - bitmap_rect.top as i32) as usize,
                (left as i32 - bitmap_rect.left as i32) as usize,
                (right as i32 - bitmap_rect.left as i32) as usize,
                y,
                &pen,
                active_pages,
            );
        }
        return_result(state, 0);
        Some(())
    }

    /// Expands the pen of `graph` into per-plane source rows, or `None` if it is unsupported.
    fn resolve_pen(&self, graph: u32, bitmap_rect: Rect) -> Option<PlanePen> {
        let mode = self.peek_u16(graph + GRAPH_PEN_MODE)?;
        if mode & 0xFF >= 8 {
            return None;
        }
        let inverted = mode & 4 != 0;
        let foreground = self.peek_u16(graph + GRAPH_FG_COLOR)?;
        let background = self.peek_u16(graph + GRAPH_BG_COLOR)?;
        let rotation = (bitmap_rect.left & 15) as u32;

        let mut rows = [[0u16; PATTERN_ROWS]; 4];
        match mode >> 8 {
            kind @ (0 | 1) => {
                let color = if kind == 0 { foreground } else { background };
                let color = if inverted { !color } else { color };
                for (plane, plane_rows) in rows.iter_mut().enumerate() {
                    *plane_rows = [if (color >> plane) & 1 != 0 { 0xFFFF } else { 0 }; PATTERN_ROWS];
                }
            }
            2 => {
                let pattern = self.peek_u32(graph + GRAPH_PEN_PAT)?;
                let (set, clear) = if inverted {
                    (background, foreground)
                } else {
                    (foreground, background)
                };
                for (plane, plane_rows) in rows.iter_mut().enumerate() {
                    let set = if (set >> plane) & 1 != 0 { 0xFFFF } else { 0 };
                    let clear = if (clear >> plane) & 1 != 0 { 0xFFFF } else { 0 };
                    for (row, word) in plane_rows.iter_mut().enumerate() {
                        let bits = self
                            .peek_u16(pattern + row as u32 * 2)?
                            .rotate_left(rotation);
                        *word = (bits & set) | (!bits & clear);
                    }
                }
            }
            3 => {
                let pattern = self.peek_u32(graph + GRAPH_EX_PAT)?;
                let flip = if inverted { 0xFFFF } else { 0 };
                for (plane, plane_rows) in rows.iter_mut().enumerate() {
                    for (row, word) in plane_rows.iter_mut().enumerate() {
                        let offset = (plane * PATTERN_ROWS + row) as u32 * 2;
                        *word = self.peek_u16(pattern + offset)?.rotate_left(rotation) ^ flip;
                    }
                }
            }
            _ => return None,
        }
        Some(PlanePen {
            rows,
            op: mode as u8,
        })
    }

    /// Returns the areas, in `bitmap_rect` coordinates, that need the guest's cursor shielding,
    /// which is none once the host draws the cursor.
    fn cursor_zones(&self, text_base: u32, bitmap_rect: Rect) -> Option<Vec<Rect>> {
        if self.host_cursor {
            return Some(Vec::new());
        }
        let zone = |offset: u32| -> Option<Rect> {
            let x = (self.peek_u16(text_base.wrapping_add(offset))? as i16)
                .saturating_add(bitmap_rect.left);
            let y = (self.peek_u16(text_base.wrapping_add(offset + 2))? as i16)
                .saturating_add(bitmap_rect.top);
            Some(Rect {
                left: x.saturating_sub(CURSOR_MARGIN),
                top: y.saturating_sub(CURSOR_MARGIN),
                right: x.saturating_add(CURSOR_MARGIN),
                bottom: y.saturating_add(CURSOR_MARGIN),
            })
        };
        Some(vec![zone(CURSOR_POS)?, zone(CURSOR_DRAWN_POS)?])
    }

    /// Fills one span, with `pattern_y` selecting the pen row in the bitmap's own coordinates.
    fn fill_screen_span(
        &mut self,
        row: usize,
        left: usize,
        right: usize,
        pattern_y: i16,
        pen: &PlanePen,
        active_pages: u16,
    ) {
        let row_bytes = self.ext_tvram_row_bytes;
        let plane_bytes = self.ext_tvram_plane_bytes;
        let Some(buffer) = self.ext_tvram.as_mut() else {
            return;
        };
        let right = right.min(row_bytes * 8);
        if left >= right {
            return;
        }
        let first = left >> 3;
        let last = (right - 1) >> 3;
        let first_mask = 0xFFu8 >> (left & 7);
        let last_mask = 0xFFu8 << (7 - ((right - 1) & 7));
        let pattern_row = (pattern_y as usize) & (PATTERN_ROWS - 1);

        for plane in 0..4 {
            if active_pages & (1 << plane) == 0 {
                continue;
            }
            let word = pen.rows[plane][pattern_row];
            let line = plane * plane_bytes + row * row_bytes;
            for byte in first..=last {
                let mut mask = 0xFFu8;
                if byte == first {
                    mask &= first_mask;
                }
                if byte == last {
                    mask &= last_mask;
                }
                let source = if byte & 1 == 0 {
                    (word >> 8) as u8
                } else {
                    word as u8
                };
                if let Some(slot) = buffer.get_mut(line + byte) {
                    *slot = combine(pen.op, *slot, source, mask);
                }
            }
        }
    }

    fn peek_u16(&self, addr: u32) -> Option<u16> {
        let hi = self.peek_u8(addr)? as u16;
        let lo = self.peek_u8(addr.wrapping_add(1))? as u16;
        Some((hi << 8) | lo)
    }

    fn peek_u32(&self, addr: u32) -> Option<u32> {
        let hi = self.peek_u16(addr)? as u32;
        let lo = self.peek_u16(addr.wrapping_add(2))? as u32;
        Some((hi << 16) | lo)
    }

    fn peek_rect(&self, addr: u32) -> Option<Rect> {
        Some(Rect {
            left: self.peek_u16(addr)? as i16,
            top: self.peek_u16(addr + 2)? as i16,
            right: self.peek_u16(addr + 4)? as i16,
            bottom: self.peek_u16(addr + 6)? as i16,
        })
    }

    /// Dereferences the `Region` handle stored at `field`, returning the record address.
    fn peek_region(&self, field: u32) -> Option<u32> {
        let handle = self.peek_u32(field)?;
        self.peek_u32(handle)
    }

    fn peek_region_bounds(&self, region: u32) -> Option<Rect> {
        self.peek_rect(region + 4)
    }

    /// Positions a reader on `top`, applying every row at or above it.
    fn region_cursor(&self, region: u32, top: i16) -> Option<RegionCursor> {
        let bounds = self.peek_region_bounds(region)?;
        let is_rect = self.peek_u32(region)? == REGION_RECT_SIZE;
        let mut cursor = RegionCursor {
            bounds,
            edges: (!is_rect).then(Vec::new),
            next_row: region + REGION_DATA,
        };
        self.advance_region(&mut cursor, top)?;
        Some(cursor)
    }

    /// Applies the toggle rows of `cursor` up to and including row `y`.
    fn advance_region(&self, cursor: &mut RegionCursor, y: i16) -> Option<()> {
        let Some(edges) = cursor.edges.as_mut() else {
            return Some(());
        };
        loop {
            let row_y = self.peek_u16(cursor.next_row)? as i16;
            if row_y == REGION_END || row_y > y {
                return Some(());
            }
            let mut addr = cursor.next_row + 2;
            loop {
                let x = self.peek_u16(addr)? as i16;
                addr += 2;
                if x == REGION_END {
                    break;
                }
                match edges.binary_search(&x) {
                    Ok(index) => {
                        edges.remove(index);
                    }
                    Err(index) => edges.insert(index, x),
                }
            }
            cursor.next_row = addr;
        }
    }
}

/// Stores an API result the way the A-Line dispatcher does: in `D0.L` and the condition codes.
fn return_result(state: &mut CpuState, result: u32) {
    state.d[0] = result;
    state.set_flag(SR_N, (result as i32) < 0);
    state.set_flag(SR_Z, result == 0);
    state.set_flag(SR_V, false);
    state.set_flag(SR_C, false);
}

fn spans(area: Rect, region: &RegionCursor) -> Vec<(i16, i16)> {
    let horizontal = |left: i16, right: i16| (left.max(area.left), right.min(area.right));
    match &region.edges {
        None => vec![horizontal(region.bounds.left, region.bounds.right)],
        Some(edges) => edges
            .chunks_exact(2)
            .map(|pair| horizontal(pair[0], pair[1]))
            .collect(),
    }
}

/// Returns the X intervals of the current row covered by every region within `area`.
fn clipped_spans(area: Rect, regions: &[RegionCursor]) -> Vec<(i16, i16)> {
    let mut result = vec![(area.left, area.right)];
    for region in regions {
        let limits = spans(area, region);
        let mut narrowed = Vec::new();
        for &(left, right) in &result {
            for &(limit_left, limit_right) in &limits {
                let span = (left.max(limit_left), right.min(limit_right));
                if span.0 < span.1 {
                    narrowed.push(span);
                }
            }
        }
        result = narrowed;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::CpuSpeedMode;

    const TEXT_BASE: u32 = 0x10000;
    const GRAPH: u32 = 0x1000;
    const BITMAP: u32 = 0x1100;
    const RECT_ARG: u32 = 0x1200;
    const VISIBLE_HANDLE: u32 = 0x1300;
    const CLIPPING_HANDLE: u32 = 0x1304;
    const VISIBLE: u32 = 0x1400;
    const CLIPPING: u32 = 0x1500;
    const STACK: u32 = 0x1800;
    const ROW_BYTES: usize = 32;
    const PLANE_BYTES: usize = ROW_BYTES * 64;

    fn write_u16(bus: &mut X68kBus, addr: u32, value: u16) {
        bus.main_ram_mut()[addr as usize..addr as usize + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_u32(bus: &mut X68kBus, addr: u32, value: u32) {
        bus.main_ram_mut()[addr as usize..addr as usize + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn write_rect(bus: &mut X68kBus, addr: u32, rect: [u16; 4]) {
        for (i, value) in rect.into_iter().enumerate() {
            write_u16(bus, addr + i as u32 * 2, value);
        }
    }

    fn write_rect_region(bus: &mut X68kBus, addr: u32, rect: [u16; 4]) {
        write_u32(bus, addr, REGION_RECT_SIZE);
        write_rect(bus, addr + 4, rect);
    }

    /// A 256x64 screen with a solid foreground pen and rectangular clipping.
    fn test_bus(fill: [u16; 4]) -> (X68kBus, CpuState) {
        let mut bus = X68kBus::new(1024 * 1024, CpuSpeedMode::Burst.timing_config());
        bus.ext_tvram = Some(vec![0; PLANE_BYTES * 4]);
        bus.ext_tvram_row_bytes = ROW_BYTES;
        bus.ext_tvram_plane_bytes = PLANE_BYTES;
        bus.native_gm_text_base = Some(TEXT_BASE);

        let screen = [0, 0, 256, 64];
        write_u32(&mut bus, TEXT_BASE + CURRENT_GRAPH, GRAPH);
        write_u32(&mut bus, GRAPH + GRAPH_BITMAP, BITMAP);
        write_rect(&mut bus, GRAPH + GRAPH_RECT, screen);
        write_u32(&mut bus, GRAPH + GRAPH_VISIBLE, VISIBLE_HANDLE);
        write_u32(&mut bus, GRAPH + GRAPH_CLIPPING, CLIPPING_HANDLE);
        write_u16(&mut bus, GRAPH + GRAPH_FG_COLOR, 0x5);
        write_u32(&mut bus, VISIBLE_HANDLE, VISIBLE);
        write_u32(&mut bus, CLIPPING_HANDLE, CLIPPING);
        write_rect_region(&mut bus, VISIBLE, screen);
        write_rect_region(&mut bus, CLIPPING, screen);

        write_rect(&mut bus, BITMAP + BITMAP_RECT, screen);
        write_u32(&mut bus, BITMAP + BITMAP_BASE, EXT_TVRAM_BASE);
        write_u16(&mut bus, BITMAP + BITMAP_LINE, ROW_BYTES as u16);
        write_u32(&mut bus, BITMAP + BITMAP_PAGE, PLANE_BYTES as u32);
        write_u16(&mut bus, BITMAP + BITMAP_ACTIVE_PAGES, 0xF);

        // Keep the cursor far from the drawing area.
        for offset in [CURSOR_POS, CURSOR_DRAWN_POS] {
            write_u16(&mut bus, TEXT_BASE + offset, 2000);
            write_u16(&mut bus, TEXT_BASE + offset + 2, 2000);
        }

        write_rect(&mut bus, RECT_ARG, fill);
        write_u32(&mut bus, STACK, RECT_ARG);
        let mut state = CpuState::new();
        state.a[7] = STACK;
        state.d[0] = 0xDEAD_BEEF;
        (bus, state)
    }

    fn pixel(bus: &X68kBus, x: usize, y: usize) -> u8 {
        let buffer = bus.ext_tvram.as_ref().unwrap();
        (0..4).fold(0, |color, plane| {
            let byte = buffer[plane * PLANE_BYTES + y * ROW_BYTES + (x >> 3)];
            color | (((byte >> (7 - (x & 7))) & 1) << plane)
        })
    }

    fn run(bus: &mut X68kBus, state: &mut CpuState) -> TrapResult {
        bus.handle_native_gm(state, GM_FILL_RECT)
    }

    #[test]
    fn fills_exactly_the_requested_pixels_across_byte_boundaries() {
        let (mut bus, mut state) = test_bus([3, 2, 21, 5]);
        assert_eq!(run(&mut bus, &mut state), TrapResult::Handled { extra_cycles: 0 });
        assert_eq!(state.d[0], 0);
        assert!(state.flag(SR_Z) && !state.flag(SR_N));
        for y in 0..8 {
            for x in 0..32 {
                let inside = (3..21).contains(&x) && (2..5).contains(&y);
                assert_eq!(pixel(&bus, x, y), if inside { 5 } else { 0 }, "({x},{y})");
            }
        }
    }

    #[test]
    fn applies_the_pen_operation_per_plane() {
        let (mut bus, mut state) = test_bus([0, 0, 8, 1]);
        bus.ext_tvram.as_mut().unwrap()[0] = 0xFF;
        write_u16(&mut bus, GRAPH + GRAPH_PEN_MODE, 3); // G_FORE | G_XOR
        run(&mut bus, &mut state);
        // Color 5 toggles planes 0 and 2 over a pixel that starts as color 1.
        assert_eq!(pixel(&bus, 0, 0), 4);
    }

    #[test]
    fn expands_pen_patterns_in_bitmap_coordinates() {
        let (mut bus, mut state) = test_bus([0, 0, 16, 2]);
        const PATTERN: u32 = 0x1600;
        write_u32(&mut bus, GRAPH + GRAPH_PEN_PAT, PATTERN);
        write_u16(&mut bus, PATTERN, 0x8000);
        write_u16(&mut bus, PATTERN + 2, 0x4000);
        write_u16(&mut bus, GRAPH + GRAPH_BG_COLOR, 0xA);
        write_u16(&mut bus, GRAPH + GRAPH_PEN_MODE, 0x0200);
        run(&mut bus, &mut state);
        assert_eq!(pixel(&bus, 0, 0), 5);
        assert_eq!(pixel(&bus, 1, 0), 0xA);
        assert_eq!(pixel(&bus, 1, 1), 5);
    }

    #[test]
    fn fills_a_region_argument_through_the_visible_area() {
        let (mut bus, mut state) = test_bus([0, 0, 0, 0]);
        const SHAPE: u32 = 0x1700;
        const SHAPE_HANDLE: u32 = 0x1308;
        write_u32(&mut bus, SHAPE_HANDLE, SHAPE);
        write_u32(&mut bus, SHAPE, 0x100);
        write_rect(&mut bus, SHAPE + 4, [2, 0, 6, 2]);
        for (i, value) in [0u16, 2, 6, 0x7FFF, 2, 2, 6, 0x7FFF, 0x7FFF].iter().enumerate() {
            write_u16(&mut bus, SHAPE + REGION_DATA + i as u32 * 2, *value);
        }
        write_u32(&mut bus, STACK, SHAPE_HANDLE);
        write_rect_region(&mut bus, VISIBLE, [4, 0, 256, 64]);
        assert_eq!(
            bus.handle_native_gm(&mut state, GM_FILL_RGN),
            TrapResult::Handled { extra_cycles: 0 }
        );
        for x in 0..8 {
            assert_eq!(pixel(&bus, x, 1), if (4..6).contains(&x) { 5 } else { 0 }, "x={x}");
        }
    }

    #[test]
    fn frames_a_rectangle_with_the_pen_size() {
        let (mut bus, mut state) = test_bus([1, 1, 7, 6]);
        write_u16(&mut bus, GRAPH + GRAPH_PEN_SIZE, 2);
        write_u16(&mut bus, GRAPH + GRAPH_PEN_SIZE + 2, 1);
        bus.handle_native_gm(&mut state, GM_FRAME_RECT);
        for y in 0..7 {
            for x in 0..9 {
                let outer = (1..7).contains(&x) && (1..6).contains(&y);
                let hole = (3..5).contains(&x) && (2..5).contains(&y);
                assert_eq!(pixel(&bus, x, y), if outer && !hole { 5 } else { 0 }, "({x},{y})");
            }
        }
    }

    #[test]
    fn reports_the_actual_display_size_in_extended_modes() {
        let (mut bus, mut state) = test_bus([0, 0, 0, 0]);
        bus.target_resolution = Some((1920, 1080));
        bus.crtc.regs[3] = 1920 / 8;
        write_u32(&mut bus, STACK, RECT_ARG);
        assert_eq!(
            bus.handle_native_gm(&mut state, SX_GET_DISP_RECT),
            TrapResult::Handled { extra_cycles: 0 }
        );
        let (width, height) = bus.crtc.active_resolution();
        assert_eq!(bus.peek_rect(RECT_ARG), Some(Rect {
            left: 0,
            top: 0,
            right: width as i16,
            bottom: height as i16,
        }));
    }

    #[test]
    fn copies_overlapping_screen_areas_without_smearing() {
        let (mut bus, mut state) = test_bus([0, 0, 0, 0]);
        const SOURCE_RECT: u32 = 0x1A00;
        const DEST_RECT: u32 = 0x1A10;
        write_rect(&mut bus, SOURCE_RECT, [0, 0, 8, 1]);
        write_rect(&mut bus, DEST_RECT, [3, 0, 11, 1]);
        write_u32(&mut bus, STACK, BITMAP);
        write_u32(&mut bus, STACK + 4, BITMAP);
        write_u32(&mut bus, STACK + 8, SOURCE_RECT);
        write_u32(&mut bus, STACK + 12, DEST_RECT);
        write_u16(&mut bus, STACK + 16, 0);
        write_u32(&mut bus, STACK + 18, 0);
        bus.ext_tvram.as_mut().unwrap()[0] = 0b1010_0001;
        assert_eq!(
            bus.handle_native_gm(&mut state, GM_COPY),
            TrapResult::Handled { extra_cycles: 0 }
        );
        let row: Vec<u8> = (0..12).map(|x| pixel(&bus, x, 0)).collect();
        assert_eq!(row, [1, 0, 1, 1, 0, 1, 0, 0, 0, 0, 1, 0]);
    }

    #[test]
    fn leaves_scaled_copies_to_the_guest() {
        let (mut bus, mut state) = test_bus([0, 0, 0, 0]);
        const SOURCE_RECT: u32 = 0x1A00;
        const DEST_RECT: u32 = 0x1A10;
        write_rect(&mut bus, SOURCE_RECT, [0, 0, 8, 8]);
        write_rect(&mut bus, DEST_RECT, [0, 0, 16, 16]);
        write_u32(&mut bus, STACK, BITMAP);
        write_u32(&mut bus, STACK + 4, BITMAP);
        write_u32(&mut bus, STACK + 8, SOURCE_RECT);
        write_u32(&mut bus, STACK + 12, DEST_RECT);
        assert_eq!(bus.handle_native_gm(&mut state, GM_COPY), TrapResult::PassToVector);
    }

    #[test]
    fn inverts_a_dashed_outline_without_touching_the_inside() {
        let (mut bus, mut state) = test_bus([0, 0, 0, 0]);
        write_rect(&mut bus, RECT_ARG, [2, 2, 6, 5]);
        write_u32(&mut bus, STACK, RECT_ARG);
        write_u16(&mut bus, STACK + 4, 0xFFFF);
        bus.host_cursor = true;
        assert_eq!(
            bus.handle_native_gm(&mut state, GM_INVERT_RECT),
            TrapResult::Handled { extra_cycles: 0 }
        );
        // Like the guest, the sides also cover the top and bottom rows, so the corners are
        // inverted twice and come back unchanged.
        for y in 1..6 {
            for x in 1..7 {
                let horizontal = (y == 2 || y == 4) && (2..6).contains(&x);
                let vertical = (x == 2 || x == 5) && (2..5).contains(&y);
                let expected = if horizontal ^ vertical { 0xF } else { 0 };
                assert_eq!(pixel(&bus, x as usize, y as usize), expected, "({x},{y})");
            }
        }
    }

    #[test]
    fn marching_frame_holds_the_caller_until_the_display_period_ends() {
        let (mut bus, mut state) = test_bus([0, 0, 0, 0]);
        const STYLE: u32 = 0x1B00;
        write_rect(&mut bus, RECT_ARG, [0, 0, 4, 3]);
        write_u32(&mut bus, STACK, RECT_ARG);
        write_u32(&mut bus, STACK + 4, STYLE);
        write_u16(&mut bus, STYLE, 0x8001);
        bus.host_cursor = true;
        state.ir_pc = 0x2000;
        state.pc = 0x2002;

        bus.handle_native_gm(&mut state, GM_MARCHING_RECT);
        assert_eq!(state.pc, 0x2000, "the call repeats while waiting");
        assert_eq!(bus.peek_u16(STYLE), Some(0xC000));
        let (_, target) = bus.vsync_wait.unwrap();

        bus.mfp.vdisp_falls = target;
        state.pc = 0x2002;
        bus.handle_native_gm(&mut state, GM_MARCHING_RECT);
        assert_eq!(state.pc, 0x2002, "the call completes after the edge");
        assert_eq!(bus.vsync_wait, None);
        assert_eq!(bus.peek_u16(STYLE), Some(0xC000), "the style advances once per call");
    }

    #[test]
    fn draws_only_the_active_planes() {
        let (mut bus, mut state) = test_bus([0, 0, 8, 1]);
        write_u16(&mut bus, BITMAP + BITMAP_ACTIVE_PAGES, 0x3);
        run(&mut bus, &mut state);
        assert_eq!(pixel(&bus, 0, 0), 1);
    }

    #[test]
    fn honors_non_rectangular_visible_regions() {
        let (mut bus, mut state) = test_bus([0, 0, 16, 4]);
        // Rows 1..3 cover x=4..8 only.
        write_u32(&mut bus, VISIBLE, 0x100);
        write_rect(&mut bus, VISIBLE + 4, [4, 1, 8, 3]);
        for (i, value) in [1u16, 4, 8, 0x7FFF, 3, 4, 8, 0x7FFF, 0x7FFF].iter().enumerate() {
            write_u16(&mut bus, VISIBLE + REGION_DATA + i as u32 * 2, *value);
        }
        run(&mut bus, &mut state);
        for y in 0..4 {
            for x in 0..16 {
                let inside = (4..8).contains(&x) && (1..3).contains(&y);
                assert_eq!(pixel(&bus, x, y), if inside { 5 } else { 0 }, "({x},{y})");
            }
        }
    }

    #[test]
    fn maps_window_coordinates_through_the_bitmap_origin() {
        let (mut bus, mut state) = test_bus([10, 10, 12, 11]);
        write_rect(&mut bus, BITMAP + BITMAP_RECT, [8, 8, 264, 72]);
        write_rect(&mut bus, GRAPH + GRAPH_RECT, [8, 8, 264, 72]);
        write_rect_region(&mut bus, VISIBLE, [8, 8, 264, 72]);
        write_rect_region(&mut bus, CLIPPING, [8, 8, 264, 72]);
        run(&mut bus, &mut state);
        assert_eq!(pixel(&bus, 2, 2), 5);
        assert_eq!(pixel(&bus, 10, 10), 0);
    }

    #[test]
    fn leaves_unsupported_cases_to_the_guest() {
        let unsupported: [(u32, u16); 3] = [
            (GRAPH + GRAPH_WORK_KIND, 2),
            (GRAPH + GRAPH_DRAW_LEVEL, 0xFFFF),
            (GRAPH + GRAPH_PEN_MODE, 0x0400),
        ];
        for (field, value) in unsupported {
            let (mut bus, mut state) = test_bus([0, 0, 8, 8]);
            write_u16(&mut bus, field, value);
            assert_eq!(run(&mut bus, &mut state), TrapResult::PassToVector);
            assert_eq!(pixel(&bus, 0, 0), 0);
        }
    }

    #[test]
    fn leaves_drawing_near_the_cursor_to_the_guest() {
        let (mut bus, mut state) = test_bus([0, 0, 8, 8]);
        write_u16(&mut bus, TEXT_BASE + CURSOR_POS, 4);
        write_u16(&mut bus, TEXT_BASE + CURSOR_POS + 2, 4);
        assert_eq!(run(&mut bus, &mut state), TrapResult::PassToVector);
    }
}
