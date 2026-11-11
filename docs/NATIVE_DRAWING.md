# Native SX-Window Drawing: Status and Plan

This document collects how msx68k replaces the drawing of `SYS/FSX.X` (Graph Man) with native code, what is native today, how it is verified, and what remains. Text drawing and the font manager have their own document, [NATIVE_FONTS.md](NATIVE_FONTS.md).

---

## 1. Goal

- **Now**: draw screens larger than 1024×1024, which do not fit in TVRAM (`$E00000`), without the guest ever addressing the screen memory. Speed is a side benefit, not the aim.
- **End state**: the whole SX-Window layer runs natively, drawing into a host TrueColor buffer uploaded with `wgpu`.

Screens above 1024×1024 live in a host buffer that screen `Bitmap` records name with the base `EXT_TVRAM_BASE = $F000_0000` (Section 2.5). Guest code cannot reach it, so guest drawing that has not been replaced yet draws nothing there and is reported. Every API the guest would use is therefore being replaced. Screens up to 1024×1024 keep using TVRAM and the guest routines.

---

## 2. How drawing is replaced

### 2.1 Hook points

| Hook | Used for | Guest memory |
| :--- | :--- | :--- |
| A-Line APIs, intercepted by `BusTrapHandler::on_a_line` before the `SYS/FSX.X` dispatcher | Graph Man and system APIs ([`native_gm.rs`](../crates/msx68k-bus/src/native_gm.rs)) | Not patched or consumed |
| First instruction of an internal routine replaced with a hypercall (pattern 3 of [HOOK_AND_SCSI_ARCHITECTURE.md](HOOK_AND_SCSI_ARCHITECTURE.md) Section 2.3) | Routines that `SYS/FSX.X` calls directly, with no A-Line API in front: the text span routine (`$FE8B`) and the pen span routine (`$FE8C`) | One word, verified by signature |
| Cursor draw and erase entries patched to `RTS` | Host cursor | Two words |

An API or case without a native implementation returns `PassToVector` (or, for a hypercall, performs the replaced instruction) and runs the original `SYS/FSX.X` code.

Internal callers do not always go through A-Line APIs. `GMPlotImg` calls the `GMCopyMask` body directly, `GMFrameRgn` and `GMScroll` call the region fill directly, and all text reaches the screen through a routine that is only called directly. Each native API therefore replaces the whole call tree beneath it, and directly called routines need a hypercall hook.

### 2.2 Activation and scope

- Enabled from the `SYS/FSX.X` load tracker (`check_fsx_mouse_hook_patch`) after checking that the drawing procedure table at `text_base + 0x5549E` holds the expected relocated `Rect` pointer at `+8` (`text_base + 0x20B68`). No absolute addresses are assumed: everything is found from `text_base`, which is derived from the mouse handler written to `$0938`, and each patch checks its byte signature first.
- Only screens in the extended framebuffer are handled (`is_ext_tvram_active`). `SXGetDispRect` is the exception (Section 3).
- Destinations must be the planar screen bitmap. Off-screen bitmaps in main RAM are handled where the screen is the other side of a copy.

### 2.3 Calling convention and records

- Arguments start at `A7`. Results go in `D0.L` and the condition codes, because the A-Line dispatcher (`text_base + 0x3C6`) copies the handler's CCR back to the caller. The dispatcher loads the handler address into `D0` before calling it, which some results expose (`GMScroll`).
- Records from the official headers (`SXDEF2.H`, `SXGRAPH.H`):
  - `Graph`: `bmap` +0, `rect` +4, `procs` +12, `visible` +16, `clipping` +20, `drawLvl` +24, `penMode` +26 (kind in the high byte: `G_FORE`, `G_BACK`, `G_PPAT`, `G_EXPAT`; operation 0 to 7 in the low byte, bit 2 meaning NOT), `penLoc` +28, `penSize` +32, `penPat` +36, `exPat` +40, `workKind` +44 (2 or 3 records drawing and is left to the guest), `fgColor` +50, `bgColor` +52, `fontKind` +54, `fontFace` +56, `fontMode` +58, `fontSize` +60.
  - `Bitmap`: `type` +0, `rect` +2, `base` +10, `line` +14, `page` +16, `aPage` +20 (active planes).
  - `Region`: size (12 for a bare rectangle), bounds at +4, then rows of a Y value followed by X edges ending in `32767`. Each row toggles its edges against the previous row, and a Y of `32767` ends the record.
- Current graph pointer at `text_base + 0x5523A`. Screen `Bitmap` record at `text_base + 0x55278`. A-Line handler table at `text_base + 0x400`.

### 2.4 Shared models

- **Clipping**: drawing is limited to the graph rect, the bitmap rect, and the rows of the `visible` and `clipping` regions. A hidden graph (`drawLvl < 0`) draws nothing, but each API still returns what the guest returns in that case.
- **Pens**: every pen resolves to one 16-word pattern per plane. `G_FORE` and `G_BACK` are solid, `G_PPAT` chooses between the foreground and background color from `penPat`, and `G_EXPAT` reads four planes from `exPat`. Pattern words rotate by the bitmap's left edge and rows are indexed by the bitmap Y coordinate. Bit 2 of the operation inverts the color, swaps the two colors for `G_PPAT`, and inverts the pattern for `G_EXPAT`.
- **Logical operations**: copy, AND, OR, and XOR, each optionally with the source inverted.
- **Host cursor**: the host draws the mouse cursor (Section 3), so native drawing never needs the guest's cursor shielding (`MSShieldCsr` / `MSShowCsr`). The native code still leaves spans near the cursor to the guest when the guest cursor is in use.

### 2.5 The screen address and illegal access detection

- `$F000_0000`..`$FEFF_FFFF` is reserved for msx68k extensions (`MSX68K_RESERVED_BASE`, `MSX68K_RESERVED_END`). It avoids every high memory that real machines map (X68030 and Xellent30 at `$0100_0000`, 060turbo at `$1000_0000`) and the `$FFxx_xxxx` block that sign-extended absolute short addresses reach.
- The reservation depends on the VM (`X68kBus::extensions_reserved`). The master SX-Window VM, whose `HUMAN.SYS` 3.02 and SX-Window 3.1 are 68030-safe and use no upper address bits as flags, uses the range for extensions, and any guest access to it is detected. Partial and full VMs run arbitrary software, so there the range mirrors the 24-bit space as on the 68000, like every other address above 24 bits.
- Screen `Bitmap` records hold `EXT_TVRAM_BASE`, the first 16 MB block of the reserved range (`$F000_0000`..`$F0FF_FFFF`).
- Only native drawing writes the screen. In the master VM, a guest access anywhere in the reserved range is counted in `X68kBus::illegal_reserved_accesses`, writes are dropped, and reads return 0. The first access during each A-Line API is reported on standard error with the API and its call site, which `on_a_line` records on every A-Line call.
- `X68kBus::guest_screen_mapped` maps the block for guest code again. Verification against the guest routines (Section 4) and tests that write the screen through the bus set it.
- User-mode access to the reserved range in the master VM raises a bus error, like VRAM and I/O.

---

## 3. Status by API

All of the following are verified against the guest (Section 4) on screens above 1024×1024. Cases in the "Left to the guest" column run the guest routine. Its rows still reach the screen natively when it draws them through the pen or span routines, which nearly every guest drawing routine does; anything else draws nothing on an extended screen and is reported as an illegal access (Section 2.5).

| API | Native behaviour and guest quirks reproduced | Left to the guest |
| :--- | :--- | :--- |
| `GMFrameRect` `$A172`, `GMFillRect` `$A173`, `GMFillRgn` `$A17B` | All pen kinds and the eight operations. | Recording, procedure tables other than the default. |
| `GMFrameRgn` `$A17A` | Fills the region minus the region inset by the pen size. The guest's `InsetRgn` shrinks each row and then each column, which is a box erosion. An inset that leaves nothing frames the whole region, and so does a 0×0 pen on a rectangle. | A 0×0 pen on a shaped region, which the guest draws with stray dots outside it. |
| `GMScroll` `$A17E` | Args: rect, offset (dh, dv), update region handle or 0. With V = visible ∩ clipping ∩ rect ∩ graph rect ∩ bitmap rect, the pixels of V ∩ (V + offset) move, so a pixel moves only when both ends are visible. Given a handle, V − (V + offset) becomes the update region and is filled with the background (pen mode `$0100`). A zero offset, a hidden graph, or an empty V stores an empty region. On the screen bitmap the guest returns the `MSShowCsr` handler address in `D0`. | Update regions that are not rectangles, and handles that do not hold one, since writing them needs the guest heap to resize the handle. |
| `GMCopy` `$A17F` | Same-size transfers with the eight operations, where one side is the screen and both sides have the same active planes. Reads every span before writing, so overlapping copies are correct. | None: scaled copies run the guest routine, whose rows are drawn natively through the span routine. |
| `GMCopyMask` `$A180` | Args: source, destination, mask bitmap, source rect, destination point, mask rect. Copies the pixels whose mask bit is set over the mask rect's size. The mask is not clipped to its bitmap. A hidden graph returns 1 if the source rect meets the source bitmap. A drawn or source-clipped call returns 0 with N and C set, from the guest's last `subq`. | None. Like the guest, it draws through no procedure table and ignores recording, since the guest routine writes the bitmap itself. A source narrower than the mask, or reaching past the sides of its bitmap, is clipped to the source, which fixes a guest bug (Section 6.1). Verified against the guest with a procedure table set (54 drawing cases) and while recording (146), and for narrow sources against the guest given the equivalent clipped arguments (69). |
| `GMPlotImg` `$A182` | Builds the source and mask on the host and draws through the `GMCopyMask` path. The source has `max(pages, highest active plane)` planes; planes past the image are clear except the fourth, which is set. Styles: 0 plain, 1 planes 0 and 1 inverted, 2 planes 0 and 1 ORed and the rest ANDed with a checkerboard, 3 all planes ANDed with it, 4 and 5 the background color, 6 and 7 as 0 and 1 with the mask ANDed with a checkerboard, 8 and 9 the mask ANDed with `$AAAA` or alternating `$DDDD` / `$7777`. | None. |
| `GMPutRImg` `$A183` | Copies a `RectImg` through the `GMCopy` path from a temporary bitmap over its data (line `((w + 15) / 16) * 2`, page `line * h`, the current bitmap's active planes). | None. |
| `GMDupHRImg` `$A188` | Each image row becomes a 16-dot extended pattern word whose period is the image width, phased by the bitmap's left edge, and is written to planes 0 and 1. | None. |
| `GMDupVRImg` `$A189` | Reads the image as four planes and picks each source row from the destination row's unsigned coordinate modulo the image height. | None. |
| `GMInvertRect` `$A1A3`, unnamed `$A1A4` | One-pixel outlines with a 16-bit dash style, clipped to the bitmap only. Rotation comes from the low word of the bitmap pointer, as in the guest, and the sides also cover the top and bottom rows unless the frame is at the bitmap edge. `$A1A3` XORs the dash. `$A1A4` (args: rect, pointer to style) writes it, rotates the stored style right by one, and holds the caller until a V-DISP falling edge by re-executing the A-Line instruction, so interrupts keep running. | None. |
| `GMGetPixel` `$A1AF` | Args: bitmap, point. On the screen bitmap returns the color built from the bits of the active planes, plane 0 lowest, or 0 outside the bitmap. Verified on 500 rewritten calls with random points in and around the screen. | Bitmaps other than the screen, which the guest reads from main RAM. |
| `SXGetDispRect` `$A432` | Returns the actual display size. The guest indexes its CRT table with the low 5 bits of the mode, so mode `$50` reads as 768×512 and popup menus were confined to it. Answered whenever an extended mode is active, independently of the other APIs. | None. |
| Text and bitmap span routine (`$FE8B`, `+0x381FE`) | All text rows, see [NATIVE_FONTS.md](NATIVE_FONTS.md). The bitmap transfer setup at `+0x38040` uses the same routine for planar bitmaps, with a copy, AND, OR, XOR, or no-op routine and an inversion word per plane, four-plane source pages at +32, and a reversed flag at +40 for copies that overlap to the right, which only changes the guest's order. So the rows of every guest copy that uses the transfer setup, including scaled `GMCopy` and the scroll cases left to the guest, are drawn natively. The guest `GMCopyMask` routine writes the bitmap itself instead, which is why `GMCopyMask` handles every case natively. Verified by rewriting live `GMCopy` calls into scaled and stretched copies with every operation (96 cases, 72 drawing) and by running the `GMScroll` cases left to the guest with horizontal, diagonal, and shaped-region offsets (84 cases). | Unknown span routines. |
| Pen span routine (`$FE8C`, `+0x37212`) | Every row that a guest shape fill draws into a planar bitmap: lines, ovals, rounded rects, arcs, polygons, regions, and rects that the APIs above leave to the guest. The pen setup at `+0x369A6` fills a context: the span routine at +0, a routine per plane at +4..+16 (clear, set, keep, invert, or copy, AND, OR, or XOR the pattern), the pattern row offset at +20, and 16 pattern words per plane from +26, already rotated to the bitmap. Args: `A0` context, `A5` graph, `A1` destination row, `D0`..`D1` dots (exclusive, graph coordinates). Verified by rewriting live `GMFillRect` calls into `GMLine`, `GMFrameOval`, `GMFillOval`, `GMFrameRRect`, `GMFillRRect`, `GMFrameArc`, `GMFillArc`, `GMFramePoly`, and `GMFillPoly` with random shapes, pen kinds, operations, and pen sizes: all 205 cases, 159 of them drawing, matched the guest. | Unknown span routines. |
| System error alert box (`$FE8D`..`$FE8F`) | The 48-byte row copy at `+0x4C41E` (save, frame, restore), the 6-byte icon row copy at `+0x4C3CC`, and the glyph store `OR.B D4,(A0)+` at `+0x4C2CA` are replaced with hypercalls. Addresses inside the box's TVRAM window, the 70 rows of 48 bytes at the address stored at `+0x54B16`, are mapped onto plane 0 of the extended screen at the box rect stored at `+0x54B2A`; other addresses are read and written as usual. Only offsets from the box's own address are used, so the guest's address arithmetic may wrap past TVRAM's 1024-dot width. Outside extended modes each hypercall performs the instruction it replaced. Verified by raising TRAP #14 with five error codes at a live call site: the screen box matched, byte for byte, the box the original code draws into TVRAM. The restore path, which needs a key press, is covered only by unit tests. | None. |
| `TSTiniCrtM` `$A34F` | The guest clears TVRAM and returns the screen to mode 16, which copies the extended screen into TVRAM, so the extended screen is cleared first and the guest routine then runs. | Everything else in the call. |
| Mouse cursor | The guest's draw and erase entries (`text_base + 0x521BA` and `+ 0x521DC`, both starting `48E7 000C 2868 0000`) return at once. Each frame the presenter applies the `MsCsr` at `**csrHdl` (`Mouse` +40) at `thePosition` (+16) minus its hot spot when `cached` (+50) is set, as `(screen AND NOT mask) XOR pattern` on each plane in `bits` (+44). The `Mouse` record pointer is at `text_base + 0x633CA`. | None. |

Results that do not match the guest byte for byte, by design:
- **Heap state**: `GMFrameRgn` and `GMScroll` in the guest allocate and free a temporary region, and `GMScroll` may move the update region's block. The native versions do not allocate, so free blocks and master pointers differ while the drawing, the registers, and the region contents match.
- **Scratch registers**: `A0` and the `X` flag are not part of any result and are not reproduced.

---

## 4. Verification

Each API is checked by differential replay on a scripted desktop session (boot, opening, scrolling, and dragging windows, menus, the console) at 1920×1080:

1. At each call of the API, the machine is cloned. The clone runs the guest routine with native drawing disabled and interrupts masked until it returns. The original runs the native version, also with interrupts masked for calls that reach native code below an A-Line API.
2. The two are compared in the screen buffer, `D0`–`D7`, `A1`–`A7`, the condition codes other than `X`, and main RAM outside the 32 KB below each stack pointer, which holds the guest routine's scratch space.
3. The session then continues from the guest result, so later calls see the reference state.

Cases the session never produces are covered by rewriting the arguments of real calls into many combinations and comparing the same way, for example every page count and style of `GMPlotImg` (13,200 cases), random `GMCopyMask` arguments, pen sizes and synthetic L-shaped, ringed, and gapped regions for `GMFrameRgn`, and horizontal, diagonal, and out-of-range offsets with shaped visible regions for `GMScroll`. The same run attributes every remaining guest access to the extended framebuffer to the API that made it, which is how the remaining work is found.

The harness is a scratch tool outside the repository. Unit tests in `native_gm.rs` cover each API's core behaviour.

Current result: in the scripted session, no guest code reads or writes the extended framebuffer.

---

## 5. Plan

In order:

1. **Handle the routines that assume TVRAM.** Done: `TSTiniCrtM` and the system error alert box (Section 6.2).
2. **Find writers outside the session.** Use the illegal access reports (Section 2.5) during ordinary use and with more applications. Shape fills are native through the pen span routine. Copies through the bitmap transfer setup reach the screen through the span routine as well. Paths not yet covered are routines that write a bitmap without the pen or span routines, which the illegal access reports reveal. Replacing the shape APIs at the A-Line level, so that each shape is one native call, is a later performance step.
3. **Close the remaining guest cases.** Most cases in the "Left to the guest" column now draw through the span routines, so what remains is mainly a matter of reproducing the guest results (or fixing guest bugs) at the API level. Guest bugs (Section 6.1) are fixed rather than copied.
4. **Call guest services from native code.** `GMScroll` update regions that are not rectangles need the guest memory manager to resize a handle, and later APIs will need allocations too. This needs a way for a native handler to run a guest routine and resume afterwards.
5. **Remove the guest mapping.** Done: the screen base moved to `$F000_0000` and guest accesses are detected (Section 2.5). The mapping remains only for verification.
6. **Host TrueColor buffer.** Draw into a host buffer uploaded with `wgpu`, keeping the 16-color planar semantics where applications depend on them. Optimise only if performance requires it.
7. **Partial VM for supervisor tasks.** SX applications run in user mode, and user-mode access to I/O and VRAM raises a bus error, so only `SYS/FSX.X` touches the screen. Tasks that switch to supervisor mode (DOS `_SUPER`, IOCS `_B_SUPER` `$81`) are to run with virtualized TVRAM and GVRAM.
8. **MC68030 emulation.** Burst mode and a 25 MHz cycle-accurate mode run, and SX-Window boots on both. Still open: tuning the bus and I/O wait states against real hardware. See [CPU_AND_BUS.md](CPU_AND_BUS.md) Section 7.

---

## 6. Guest Bugs and Quirks

Findings about `SYS/FSX.X` itself, gathered while comparing native and guest drawing. Offsets are from `text_base`.

### 6.1 Bugs

Wrong results or crashes on the original screens too. These are fixed as bugs: the native versions give a correct result rather than copying them. Until then they stay on the guest routine.

| API | Condition | Guest behaviour | Status |
| :--- | :--- | :--- | :--- |
| `GMCopyMask` `$A180` (`+0x24AF2`) | The source rect is narrower than the mask rect, or reaches past the left or right side of the source bitmap | Counts the words to copy from the clipped source, but takes the pixels within those words from the destination clip alone. It copies pixels past the source's edge up to the end of the word, and when the difference is large enough it overruns its row mask buffer on the stack and corrupts the return address, ending in a system error. | Fixed natively: the copy is clipped to the source. |
| `GMFrameRgn` `$A17A` (`+0x2283E`) | A 0×0 pen on a region that is not a rectangle | Draws stray dots outside the region, for example past the inner corner of an L-shaped region. | Left to the guest. To be fixed. |

### 6.2 Limits of the Original Screen Model

Not bugs: `SYS/FSX.X` was written for TVRAM and the standard screen modes, so it naturally does not handle msx68k's extended screens. Dealing with these is part of the screen extension itself.

| Where | Assumption | Status |
| :--- | :--- | :--- |
| All guest drawing routines | Address the screen through `Bitmap.base`, `line`, and `page`, so they need the whole screen to be addressable. Screens above 1024×1024 have no room in the 24-bit address space, so guest drawing there is undefined. | Replaced natively, API by API (Sections 3 and 5). |
| `SXGetDispRect` `$A432` (`+0x15196`) | Indexes its fixed CRT table with the low 5 bits of the mode, so extended mode `$50` reads as mode `$10`, 768×512. Popup menus use this rect as their bounds and were confined to it. | Native: returns the actual display size. |
| Screen mode tables (`+0x552F6` virtual modes, `+0x151C0` CRT table) | Only the standard modes up to 1024×1024, each with base `$E00000`. | The resolution switch writes the extended geometry into the screen records instead (Section 5.6 of the SX-Window reference). |
| `GMNewBits` (`+0x2F23A`) and `GMUnlockBits` (`+0x2F32E`) immediates | New screen bitmaps get base `$E00000`. | The resolution switch rewrites the immediates. |
| `TSTiniCrtM` `$A34F` (`+0x133D8`) | Clears TVRAM at `$E00000` with the CRTC simultaneous plane write (R21), resets the text palette, and returns to mode 16 with `_CONCTRL`, which copies the extended screen into TVRAM. | Native: the extended screen is cleared first. |
| System error alert box (`+0x4BFC4`, reached through TRAP #14, which `SYS/FSX.X` takes over with `_INTVCS`) | Centres a 384×70 box in `SXGetDispRect` and works on plane 0 at `$E00000 + y * 128 + x / 8` (computed at `+0x4C01E`), with a row step of 128 fixed in the code: it saves the box area, draws the frame and icon from templates, ORs the message glyphs from the CGROM, and restores the area when the caller resumes. | Native: the three instructions that touch the box are replaced with hypercalls (Section 3). |
| Screen size during a drawing call | The kernel's size information is read during a call and must not change under it. | Resolution switches requested by the host are applied when a user-mode task calls `TSEventAvail` or `TSGetEvent`. |

### 6.3 Quirks

Odd but deterministic behaviour that callers can observe. The native versions reproduce all of these.

| API | Behaviour |
| :--- | :--- |
| `GMScroll` `$A17E` | On the screen bitmap it returns the `MSShowCsr` handler address in `D0` instead of 0, because its last call, `MSShowCsr`, leaves the address that the A-Line dispatcher loaded into `D0`. |
| `GMCopyMask` `$A180` | The mask rect is not clipped to the mask bitmap. A hidden graph returns 1 if the source rect meets the source bitmap and 0 otherwise, with N set. A call that draws, or whose source clipping leaves nothing, returns 0 with X, N, and C set, from the `subq` that counts down its cursor shielding. |
| `GMPlotImg` `$A182` | A page count of 4 or more or a style of 10 or more returns −1. A bitmap with no active planes returns the page count. An empty rect returns its left edge, sign-extended. Planes beyond the image's are filled clear, except the fourth, which is filled set. |
| `GMFrameRgn` `$A17A` | A 0×0 pen on a rectangle fills the whole rectangle, although insetting by nothing would leave nothing to frame. |
| `GMDupHRImg` `$A188` | Writes planes 0 and 1 only, and phases the repeated pattern by the bitmap's left edge rather than by the drawing position. |
| `GMDupVRImg` `$A189` | Picks each source row from the destination row's unsigned 16-bit coordinate modulo the image height, not from the offset from the drawing position. |
| `GMInvertRect` `$A1A3`, `$A1A4` | The dash rotation comes from the low word of the bitmap record's address. The sides also cover the top and bottom rows unless the frame touches the bitmap edge, so corners are inverted twice and come back unchanged under `GMInvertRect`. |
| `GMScroll` `$A17E`, `GMFrameRgn` `$A17A` | Allocate and free a temporary region, and `GMScroll` may move the update region's block, so heap contents change even though no caller-visible result depends on it. The native versions do not allocate (Section 3). |
