//! Packed video input shared by CPU reference, Compute (`wgpu`), and GLES 2.0 backends.

use std::borrow::Cow;

use msx68k_bus::cynthia::CynthiaFrame;
#[cfg(feature = "wgpu-backend")]
use msx68k_bus::cynthia::SpriteSpan;
#[cfg(feature = "wgpu-backend")]
use msx68k_bus::raster::BLANK_PALETTE_ID;
use msx68k_bus::raster::{RasterFrame, RasterState};
use msx68k_bus::{X68kBus, TVRAM_PLANE_BYTES};

use crate::ViconUniforms;

#[cfg(feature = "wgpu-backend")]
pub(crate) const RASTER_HEADER_WORDS: usize = 4;
#[cfg(feature = "wgpu-backend")]
pub(crate) const RASTER_STATE_WORDS: usize = 16;
#[cfg(feature = "wgpu-backend")]
pub(crate) const PALETTE_PACKED_WORDS: usize = 256;
#[cfg(feature = "wgpu-backend")]
pub(crate) const CYNTHIA_HEADER_WORDS: usize = 8;

/// `ViconUniforms::reserved[1]` bit flags.
#[cfg(feature = "wgpu-backend")]
pub(crate) const FLAG_CYNTHIA_ENABLED: u32 = 1 << 0;
#[cfg(feature = "wgpu-backend")]
pub(crate) const FLAG_INTERLACE_ENABLED: u32 = 1 << 1;
#[cfg(feature = "wgpu-backend")]
pub(crate) const FLAG_INTERLACE_ODD_FIELD: u32 = 1 << 2;

/// Returns the active interlace field (`Some(0)` = even, `Some(1)` = odd), or `None` for progressive.
#[inline]
pub(crate) fn interlace_field(bus: &X68kBus, params: &ViconUniforms) -> Option<u8> {
    if let Some(frame) = completed_frame(bus, params) {
        return frame.interlace_field;
    }
    if params.tvram_plane_bytes != TVRAM_PLANE_BYTES as u32 || params.tvram_row_bytes != 128 {
        return None;
    }
    bus.crtc.interlace_field()
}

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

pub(crate) fn cynthia_input<'a>(
    bus: &'a X68kBus,
    params: &ViconUniforms,
) -> Cow<'a, CynthiaFrame> {
    if let Some(frame) = completed_frame(bus, params) {
        return Cow::Borrowed(&frame.cynthia);
    }
    if params.tvram_plane_bytes != TVRAM_PLANE_BYTES as u32 || params.tvram_row_bytes != 128 {
        return Cow::Owned(CynthiaFrame::empty());
    }
    Cow::Owned(CynthiaFrame::from_live_bus(
        &bus.crtc,
        bus.sprite_bg(),
        bus.sprite_bg_extra(),
        bus.sprite_options,
        params.r2_display as u16,
        params.height,
    ))
}

#[cfg(feature = "wgpu-backend")]
#[inline]
fn push_packed_palette(words: &mut Vec<u32>, pal: &[u16; 512]) {
    for pair in pal.chunks_exact(2) {
        words.push((pair[0] as u32) | ((pair[1] as u32) << 16));
    }
}

#[cfg(feature = "wgpu-backend")]
#[inline]
fn push_raster_state(words: &mut Vec<u32>, state: &RasterState) {
    let r = &state.registers;
    words.push(r.text_scroll.0 as u32);
    words.push(r.text_scroll.1 as u32);
    for &(gx, _) in &r.graphic_scroll {
        words.push(gx as u32);
    }
    for &(_, gy) in &r.graphic_scroll {
        words.push(gy as u32);
    }
    words.push(r.mode as u32);
    words.push(r.priority as u32);
    words.push(r.display as u32);
    words.push(r.contrast as u32);
    words.push(state.palette_id);
    words.push(0);
}

#[cfg(feature = "wgpu-backend")]
#[inline]
fn push_uniform_state(words: &mut Vec<u32>, params: &ViconUniforms, palette_id: u32) {
    words.push(params.text_scroll_x);
    words.push(params.text_scroll_y);
    words.extend_from_slice(&params.gr_scroll_x);
    words.extend_from_slice(&params.gr_scroll_y);
    words.push(params.r0_mode);
    words.push(params.r1_priority);
    words.push(params.r2_display);
    words.push(params.contrast);
    words.push(palette_id);
    words.push(0);
}

/// Packs per-scanline register states and independent 16-bit palette generations for Compute shaders.
/// Returns an empty vector when the entire frame has all display planes disabled.
#[cfg(feature = "wgpu-backend")]
pub(crate) fn pack_raster_input(bus: &X68kBus, params: &mut ViconUniforms) -> Vec<u32> {
    params.reserved = [0; 2];
    if let Some(field) = interlace_field(bus, params) {
        params.reserved[1] |= FLAG_INTERLACE_ENABLED;
        if field & 1 != 0 {
            params.reserved[1] |= FLAG_INTERLACE_ODD_FIELD;
        }
    }
    let Some(frame) = completed_frame(bus, params) else {
        if params.r2_display & 0x7f == 0 {
            return Vec::new();
        }
        let mut words = Vec::with_capacity(
            RASTER_HEADER_WORDS + RASTER_STATE_WORDS + PALETTE_PACKED_WORDS,
        );
        words.extend_from_slice(&[0, RASTER_HEADER_WORDS as u32, (RASTER_HEADER_WORDS + RASTER_STATE_WORDS) as u32, 0]);
        push_uniform_state(&mut words, params, 0);
        let mut pal = [0u16; 512];
        pal[..256].copy_from_slice(bus.vicon.text_palette());
        pal[256..].copy_from_slice(bus.vicon.graphic_palette());
        push_packed_palette(&mut words, &pal);
        return words;
    };

    params.apply_raster(&frame.lines[0].state);
    if frame.lines.len() == 1 {
        if params.r2_display & 0x7f == 0 {
            return Vec::new();
        }
        let mut words = Vec::with_capacity(
            RASTER_HEADER_WORDS + RASTER_STATE_WORDS + PALETTE_PACKED_WORDS,
        );
        words.extend_from_slice(&[0, RASTER_HEADER_WORDS as u32, (RASTER_HEADER_WORDS + RASTER_STATE_WORDS) as u32, 0]);
        let mut st = frame.lines[0].state.clone();
        st.palette_id = 0;
        push_raster_state(&mut words, &st);
        push_packed_palette(&mut words, frame.palette(frame.lines[0].state.palette_id));
        return words;
    }

    let row_map_len = params.height as usize;
    let state_offset = RASTER_HEADER_WORDS + row_map_len;
    let palette_offset = state_offset + frame.lines.len() * RASTER_STATE_WORDS;
    params.reserved[0] = params.height;

    let mut words = Vec::with_capacity(palette_offset + frame.palettes.len() * PALETTE_PACKED_WORDS);
    words.extend_from_slice(&[
        row_map_len as u32,
        state_offset as u32,
        palette_offset as u32,
        0,
    ]);
    words.resize(state_offset, 0);
    for (index, line) in frame.lines.iter().enumerate() {
        let start = (line.y as usize).min(row_map_len);
        let end = frame
            .lines
            .get(index + 1)
            .map_or(params.height, |next| next.y) as usize;
        let end = end.min(row_map_len);
        if start < end {
            words[RASTER_HEADER_WORDS + start..RASTER_HEADER_WORDS + end].fill(index as u32);
        }
        push_raster_state(&mut words, &line.state);
    }
    for pal in &frame.palettes {
        push_packed_palette(&mut words, pal);
    }
    let _ = BLANK_PALETTE_ID;
    words
}

/// Orders spans by `(prio desc, attr_index asc)` in `O(S)` and builds the 16×16 tile CSR
/// buffers `(cynthia_meta, cynthia_pcg)` for the single-pass Compute compositor.
#[cfg(feature = "wgpu-backend")]
pub(crate) fn pack_cynthia_gpu_input(cynthia: &CynthiaFrame) -> (Vec<u32>, Vec<u32>) {
    if cynthia.is_empty() || cynthia.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let tiles_y = cynthia.height.div_ceil(16).max(1) as usize;
    let num_tiles = 32 * tiles_y;
    let max_h = cynthia.height;

    // 1. Counting-sort spans into 384 buckets: (3 - prio) * 128 + attr_index.
    let mut bucket_counts = [0u32; 384];
    for &s in &cynthia.spans {
        let prio = s.prio();
        if prio == 0 {
            continue;
        }
        let bucket = (3 - prio as usize) * 128 + (s.attr_index() as usize & 127);
        bucket_counts[bucket] += 1;
    }
    let mut bucket_offsets = [0u32; 384];
    let mut total_spans = 0u32;
    for (b, &cnt) in bucket_counts.iter().enumerate() {
        bucket_offsets[b] = total_spans;
        total_spans += cnt;
    }
    let mut sorted_spans = vec![SpriteSpan::default(); total_spans as usize];
    let mut bucket_cursor = bucket_offsets;
    for &s in &cynthia.spans {
        let prio = s.prio();
        if prio == 0 {
            continue;
        }
        let bucket = (3 - prio as usize) * 128 + (s.attr_index() as usize & 127);
        let dst = bucket_cursor[bucket] as usize;
        bucket_cursor[bucket] += 1;
        sorted_spans[dst] = s;
    }

    // 2. Count tile memberships across the 3 priority bands.
    let mut tile_counts = vec![[0u32; 3]; num_tiles];
    for &s in &sorted_spans {
        let x0 = s.x0();
        let x1 = x0 + 16;
        let y0 = s.y0();
        let y1 = s.y1().min(max_h);
        if x1 <= 0 || x0 >= 512 || y1 <= y0 {
            continue;
        }
        let tx0 = (x0.max(0) as u32) / 16;
        let tx1 = ((x1 - 1).min(511) as u32) / 16;
        let ty0 = y0 / 16;
        let ty1 = (y1 - 1) / 16;
        let band = 3 - s.prio() as usize;
        for ty in ty0..=ty1 {
            let row_base = (ty as usize) * 32;
            for tx in tx0..=tx1 {
                tile_counts[row_base + tx as usize][band] += 1;
            }
        }
    }

    // 3. Prefix sum to compute tile CSR offsets.
    let mut tile_meta = Vec::with_capacity(num_tiles * 4);
    let mut tile_cursors = Vec::with_capacity(num_tiles);
    let mut csr_len = 0u32;
    for cnt in &tile_counts {
        tile_meta.push(csr_len);
        tile_meta.push(cnt[0]);
        tile_meta.push(cnt[1]);
        tile_meta.push(cnt[2]);
        tile_cursors.push(csr_len);
        csr_len += cnt[0] + cnt[1] + cnt[2];
    }

    // 4. Populate tile_spans in sorted `(prio desc, attr_index asc)` order.
    let mut tile_spans = vec![0u32; csr_len as usize];
    for (s_idx, &s) in sorted_spans.iter().enumerate() {
        let x0 = s.x0();
        let x1 = x0 + 16;
        let y0 = s.y0();
        let y1 = s.y1().min(max_h);
        if x1 <= 0 || x0 >= 512 || y1 <= y0 {
            continue;
        }
        let tx0 = (x0.max(0) as u32) / 16;
        let tx1 = ((x1 - 1).min(511) as u32) / 16;
        let ty0 = y0 / 16;
        let ty1 = (y1 - 1) / 16;
        for ty in ty0..=ty1 {
            let row_base = (ty as usize) * 32;
            for tx in tx0..=tx1 {
                let tid = row_base + tx as usize;
                let dst = tile_cursors[tid] as usize;
                tile_cursors[tid] += 1;
                tile_spans[dst] = s_idx as u32;
            }
        }
    }

    let bg_rows_off = CYNTHIA_HEADER_WORDS;
    let pcg_tables_off = bg_rows_off + cynthia.bg_rows.len() * 4;
    let tile_meta_off = pcg_tables_off + cynthia.pcg_tables.len();
    let tile_spans_off = tile_meta_off + tile_meta.len();
    let spans_off = tile_spans_off + tile_spans.len();
    let total_meta_words = spans_off + sorted_spans.len() * 4;

    let mut meta_words = Vec::with_capacity(total_meta_words);
    meta_words.extend_from_slice(&[
        bg_rows_off as u32,
        pcg_tables_off as u32,
        cynthia.pcg_table_stride,
        tile_meta_off as u32,
        tile_spans_off as u32,
        spans_off as u32,
        cynthia.height,
        tiles_y as u32,
    ]);
    for &bg in &cynthia.bg_rows {
        meta_words.extend_from_slice(&[bg.word0, bg.word1, bg.word2, bg.word3]);
    }
    meta_words.extend_from_slice(&cynthia.pcg_tables);
    meta_words.extend_from_slice(&tile_meta);
    meta_words.extend_from_slice(&tile_spans);
    for &sp in &sorted_spans {
        meta_words.extend_from_slice(&[sp.word0, sp.word1, sp.word2, sp.word3]);
    }

    let mut pcg_words = Vec::with_capacity(cynthia.pcg_blocks.len() * 32);
    for blk in &cynthia.pcg_blocks {
        for chunk in blk.chunks_exact(4) {
            pcg_words.push(u32::from_le_bytes(chunk.try_into().unwrap()));
        }
    }

    (meta_words, pcg_words)
}
