# Native Text and Fonts: Status and Plan

This document collects how text reaches the screen in `SYS/FSX.X`, how the outline font manager `IFM.X` fits in, what is native today, and the plan for native fonts. The general drawing replacement, its hook points, and its verification method are in [NATIVE_DRAWING.md](NATIVE_DRAWING.md).

---

## 1. Goal

- **Now**: no text drawing may write the screen from guest code, so that screens above 1024×1024 can leave guest memory (see [NATIVE_DRAWING.md](NATIVE_DRAWING.md) Section 1). This includes text drawn by `IFM.X`, which is required.
- **Next**: make text fast, in particular outline fonts. Rasterizing outlines on the 68000 is slow, and real hardware probably relied on glyph caching, which still has to be investigated.
- **End state**: fonts are handled natively as part of the native SX-Window layer.

---

## 2. Text pipeline in `SYS/FSX.X`

All offsets are from `text_base`.

1. **A-Line entry points**: `GMDrawChar` (`$A18F`), `GMDrawStrL` (`$A190`), `GMDrawStr` (`$A191`), and `GMDrawStrZ` (`$A192`) all become `GMDrawStr` (args: string pointer, offset, length) at `+0x2AC76`, which calls entry `+0` of the graph's procedure table, or of the default table at `+0x5549E` when `Graph.procs` is 0. `GMShadowStrZ` (`$A1A1`, at `+0x1E7DC`) calls the `GMDrawStrZ` body twice: first one dot right and down with foreground 8 and font mode 0, then in place with foreground 11, background 0, and font mode 2 (OR), restoring the colors and `fontMode` afterwards.
2. **Text procedure** (`+0x2921C`, default table `+0`): fetches glyphs and lays out the run. It has separate paths:
   - **Plain text** (no `fontFace`, no `fontSize`): `+0x283F0`.
   - **Sized text**: drawn row by row inside the procedure (`+0x293E6`).
   - **Styled text**: glyphs are drawn into a one-plane work bitmap at `+0x55ACE`, effects are applied by font procedure `+0`, and font procedure `+4` draws the result.
   - Another caller of the same row setup is at `+0x2CAE6`.
3. **Font procedure table** (`+0x554EA`, returned by `GMGetFontProcTbl` `$A1E7`): `+0` (`+0x29C66`) applies the `fontFace` effects in the work bitmap (it does nothing unless one of bits 0, 2, 3, or 4 is set). `+4` (`+0x2A300`, the text blit) draws a one-plane bitmap at the pen location through the font mode and advances `penLoc.x` by the run's width, also when the graph is hidden. Italic runs shift each row right by `(bottom − 1 − y) / 4`. The pen mode is set to `fontMode` for the duration.
4. **Row setup** (`+0x39D36`): from the graph's bitmap type, `fontMode`, and colors, fills a context with a span routine and, for planar bitmaps, one routine and one source inversion word per plane. Font modes 0 to 7 are opaque, modes 16 to 23 draw the foreground only, and other modes fail.
5. **Text span routine** (`+0x381FE`, planar bitmaps): draws one row. Args: `A0` context, `A1` destination row, `A2` one-plane source row, `D0`..`D1` destination dots (exclusive), `D2` source dot. Context: per-plane routines at +4, source inversion words at +20, source and destination left edges at +28 and +30, source and destination page sizes at +32 and +36, and a reversed flag at +40. The per-plane routines clear, set, keep, or invert the span, or copy, AND, OR, or XOR the source:
   - **Opaque modes**: each plane takes the foreground bit where the source is set and the background bit elsewhere. The NOT modes (bit 2) swap the two colors.
   - **Foreground modes**: each plane takes the foreground bit where the source is set, or, for the NOT modes, where it is clear.

Every text row from every path, and from `IFM.X` (Section 4), reaches planar bitmaps through step 5.

---

## 3. Native status

| Part | Status |
| :--- | :--- |
| Text span routine | **Native.** Its first instruction (`MOVEM.L D3-D7/A1-A4,-(SP)`, signature `48 E7 1F 78 4A 28 00 28`) is replaced with the hypercall registered as `GmTextSpan`. The handler maps the context's per-plane routines to their operations, draws the span, and returns. An unknown routine makes it perform the replaced `MOVEM` and continue in the guest. The same routine also draws the rows of guest bitmap copies (see [NATIVE_DRAWING.md](NATIVE_DRAWING.md) Section 3). It needs the host cursor, because the guest shields its own cursor around the whole run rather than each row. |
| `GMDrawStr` and the other text APIs | Guest code, which no longer writes the screen. All 76 text calls in the replay session that reached the span routine matched the guest exactly. |
| Glyph fetch, layout, and styles | Guest code (68000). |
| `IFM.X` | Guest code. Not yet run with the native span routine (Section 5, step 1). |

---

## 4. `IFM.X`

`IFM.X` (`SHELL\IFM.X` on `SXWIN316.XDF`, 57,896 bytes, written in C) is an ordinary SX task that becomes resident when registered in Startup Maintenance. It is not resident in the default installation. When it installs itself it:

- Registers the 23 A-Line APIs `$A500`..`$A516` with `SXSetVector` (`$A423`).
- Takes over `GMFontInfo` (`$A198`), keeping the previous vector.
- Replaces entries `+0` (text), `+44`, and `+48` of the default procedure table obtained with `GMGetStdProcTbl` (`$A1E6`), keeping the previous entries.

Its text procedure checks whether the current font is one of its own and otherwise jumps to the previous procedure, so system fonts still use the path in Section 2. For its own fonts it rasterizes the outline into a one-plane bitmap and draws it with font procedures `+0` and `+4` from `GMGetFontProcTbl`, so its output also reaches the native span routine.

Configuration and fonts:

- `SHELL\IFM.LB` maps font IDs from 512 upward to font files: `VF_1M.FNT`, `VF_1G.FNT`, `MINCHO.VF1` / `.VF2`, `GOTHIC.VF1` / `.VF2`, `MG1R`, `MG4R`, `MG7R`, `KG1S`, `KG4S`, `KG7S`, and `ASKA` (`.VF1` / `.VF2`). These Japanese outline fonts came from commercial packages (書体倶楽部, Zs STAFF PRO-68K, JGFont) and are not on the SX-Window disks.
- `SXWIN313.XDF` `\FONT` holds ten Latin outline fonts in `.IFM` format: Trad, Amadeus, Artist, Baroque, Comp, House, Jack, Nabla, Pank, and Smart.
- The disk image builder rewrites the font paths in `IFM.LB` (`D:\FONT\`) and `IFM.X` (`B:\FONT\`) to `A:\FONT\`.

---

## 5. Plan

1. **Verify `IFM.X` with the native span routine.** Register `IFM.X` in Startup Maintenance, draw with the bundled `.IFM` fonts at 1920×1080, and confirm with the replay harness that the output matches the guest and that no guest code writes the screen.
2. **Investigate the font cache.** Find how `IFM.X` caches rasterized glyphs and how much it helps, and measure where time goes when outline text is drawn.
3. **Native text for system fonts.** Replace `GMDrawStr` and the text procedure natively: glyph fetch from the CGROM, layout, sizes, and styles, so each run is one native call instead of many span hypercalls. When a font manager has replaced procedure table entry `+0`, the call must still go to it.
4. **Native font manager.** Replace `IFM.X` with native outline rasterizing and a glyph cache. How it is installed is still to be decided:
   - **Keep `IFM.X` as the switch**: only accelerate when the user has registered `IFM.X`.
   - **Install automatically when `SYS/FSX.X` starts** (proposed): the host provides `$A500`..`$A516`, `GMFontInfo`, and the text procedure natively and reads the font files from the disk image, using no guest memory. Starting `IFM.X` as well then needs defined behaviour, for example being reported as already resident.
   - **A guest stub compatible with `IFM.X`**: possible, but it costs guest memory and registration with little benefit over installing automatically.
