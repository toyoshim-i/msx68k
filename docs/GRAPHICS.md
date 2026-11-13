# Graphics Design and Plan

The CPU manages VRAM, registers and emulated time. The GPU composes pixels through
one shared WGSL implementation for the primary VM and VMX screens. The CPU
compositor is a validation reference. See [VMX68K](VMX68K.md) for screen ownership
and threading, and [TESTING](TESTING.md) for the test workflow.

## Current capabilities and remaining work

1. Implemented basic GVRAM access and display: 512-dot 16/256/65536-color modes, 1024-dot
   16-color mode, page assignment, independent scrolling and text priority.
2. Implemented scanline approximation: mid-frame scroll, palette and display
   control changes. Line-start address fetch is separate from horizontal display
   sampling. Mode-specific fetch timing still needs validation, and mid-frame
   text/graphics VRAM writes need a separate history.
3. Implemented BG/PCG address generation and palette-code line buffering for
   8-dot BG0/BG1 and 16-dot BG0. Sprite selection and rendering remain pending.
4. Special priority, half transparency, interlace and precise boundary timing.

## Physical GVRAM and CPU views

Physical GVRAM contains 512 KiB of big-endian 16-bit words for 512 by 512 pixels.
Each word stores four 4-bit pages. Changing R20 changes the CPU view without
moving or initializing stored data. The GPU receives this physical layout.

| R20 bits 10..8 | CPU view |
| --- | --- |
| 0 | Four 512 by 512, 4-bit pages; each CPU window spans 512 KiB |
| 1 | Two 512 by 512, 8-bit pages; each pixel's upper byte is unused |
| 2, 3 | One 512 by 512, 16-bit page; both bytes are valid |
| 4..7 | One 1024 by 1024, 4-bit page; quadrants map to the four physical pages |

Unused upper bytes in indexed modes read as zero and ignore writes. Unavailable
page windows in 256/65536-color modes raise a bus error; debugger peeks return no
value. Modes 5..7 use the standard 1024-dot 16-color view, without extended color
modes. Dirty-page tracking records physical offsets rather than CPU-window
addresses.

## Display composition

VICON's display mode is independent of the CPU's R20 memory mode. In 512-dot
modes, priority slots select physical pages. A 256-color pixel combines two
slots; a 65536-color pixel combines four. Each physical page uses its own scroll
registers. In 1024-dot mode, slots select quadrants and page zero's 10-bit scroll
moves the whole image.

A 65536-color VRAM value is a palette code, not a direct GRBI color. Its low
byte selects a byte from an even palette word; its high byte selects a byte
from an odd palette word. These two bytes form the GRBI color. Within graphics,
the first nonzero code wins before color conversion. A nonzero code mapped to
color zero therefore suppresses lower graphics pages while allowing a layer
behind graphics to show through.

R1 bits 11..10 control text priority, bits 13..12 sprite priority, and bits 9..8
the graphics insertion position. Graphics priority is an insertion among three
layers, not a direct numerical comparison with text priority. The current
compositor implements basic text/graphics ordering and color-zero transparency.
Exact zero-priority interactions involving the disabled sprite layer's palette
zero remain future work.

## Completed-frame scanline state

Text and graphics scroll are fetched at each line boundary. VICON mode,
priority, display control, contrast and both palettes are sampled at horizontal
display start. Writes after address fetch cannot move that row, while palette
and control writes during the back porch can still affect it. CPU register
readback remains immediate. Repeated writes before a sample use the last value.

Horizontal display start uses `(R2 + 1) / (R0 + 1)` of the approximate
317/625-cycle line period. Peripheral advancement stops at this edge as well as
sync and line edges. These are separate sampling stages, not a claim of exact
hardware fetch latency. CPU writes retain instruction-level timing; bus-cycle
write timestamps and per-pixel changes are not represented.

Capture runs from active-display start to active-display end. Entering vertical
blank publishes the completed history, so subsequent register and palette writes
do not recolor the completed frame. Unchanged lines and redundant palette writes
share a state. Reset discards history, and incompatible geometry changes discard
the partial capture. Doubled scanlines map physical rasters to output rows.

When all display planes are off, capture records only the transition to blank
and skips output register and palette snapshots. Address fetch continues while
blanked. Re-enabling display uses the fetched scroll and latest display controls. A fully blank frame needs no palette upload.

The GPU receives a row-to-state index and register/palette records at changes.
Static frames retain the fixed-size palette upload path, while VM screens keep
physical dirty-page VRAM uploads. Native extended TVRAM does not use hardware
scanout timing and retains its existing composition path.

Peripheral advancement visits horizontal-sync edges, line boundaries and vertical
display transitions even in a large time batch. Passing over the raster-IRQ line
in one update must not lose its interrupt.

Text and graphics VRAM are still read at render time. Register history alone
cannot reproduce mid-frame VRAM writes or the exact relationship between fast
clear and pixels that were already displayed.

## BG and PCG scanout

The shared sprite/PCG RAM window provides two 64-by-64 tile maps at `$EBC000`
and `$EBE000`. BG control selects each map independently. Horizontal mode zero
uses 8-by-8 tiles and two BG planes; mode one uses 16-by-16 tiles and BG0 only.
Tile entries select a pattern, a 16-color palette block, and independent flips.
PCG stores packed 4-bit pixels; 16-dot patterns contain four 8-dot quadrants.
Scroll wraps in the 512-dot or 1024-dot virtual map. BG start registers align
its coordinates with the CRTC display origin. The visible BG line is limited to
512 pixels. Mixed-resolution scaling, interlace and reserved modes are not yet
validated.

At horizontal display start, scanout fetches the next physical raster's BG
patterns into two independent palette-code line buffers. The last blanking
raster prepares the first active row. This is an explicit scheduling
approximation; mode-specific hardware fetch cycles remain to be validated.
Both buffers progress with output disabled. BG enable bits and the controller's
master display gate are evaluated for the displayed row, independently of fetch.
A zero pattern nibble is transparent regardless of the palette block.

The completed frame retains fetched codes, so subsequent PCG/map writes cannot
replace already fetched pixels. Color conversion uses that output row's shared
text/sprite palette. BG0 appears above BG1; their combined output participates
in VICON text/sprite priority and graphics insertion. Sprite priorities between
the two BG planes and special palette-zero interactions remain future work.

GPU input packs four BG codes per word with a fixed 512-pixel row stride and
bounded lookup. Blank BG frames add no code upload. The main display and VM
textures share composition, including BG opacity for VM-layer visibility.
Before a complete scanout history exists, rendering samples current BG memory;
native extended TVRAM retains its existing path.

## Next implementation steps

1. Validate address-fetch and display-sampling positions for each screen mode,
   including doubled lines and writes on either side of each edge. Keep the
   current timing approximation explicit until measured timing is available.
2. Add sprite selection, its display limits and composition around BG0/BG1.
   Define sprite attribute and pattern fetch points independently of BG fetch.
3. Add text/graphics VRAM write history where rendering from final memory would
   lose already fetched data. Keep CPU bus waits independent of pipeline latency.
4. Extend final composition with special priority and half transparency, then
   complete interlace and mode-specific boundary behavior.

## Fetch timing refinement

Separate the CPU write position from the point where a value is fetched and the
point where it affects output. Graphics scroll, VRAM reads, palette conversion
and display control can belong to different stages. BG and sprite attributes
also need their own fetch and buffering rules. Do not assign one unverified
next-line rule or delay count to every attribute.

The data stored in a line buffer matters: palette codes and converted colors
respond differently to later palette writes. Buffer count alone does not specify
latency. Keep CPU bus waits separate from display-pipeline delays, and distinguish
guest scanout timing from host output buffering. Model-dependent fetch points
need validation in each supported screen mode.

Display-output suppression and internal pipeline progress are separate concerns.
BG buffers continue fetching while display output is disabled. The blank-frame
snapshot/upload optimization must not automatically stop future sprite buffers;
their behavior while disabled needs its own definition.

Keep CPU history independent of GPU representation. A future timestamped change
history should feed attribute-specific latches and delays, then produce display
state. Current row snapshots describe output state, not complete write timing.
Small row values can use fixed-size records, with large palettes or PCG data
shared by reference. Shader access must remain bounded rather than searching a
change log per pixel; measure the cost of indirect references.

## Validation and remaining scope

ROM-free tests cover byte/word/long accesses, cross-mode views, quadrant mapping,
physical dirty pages and 68000 MMIO instructions. Pixel fixtures assert expected
colors before comparing the CPU reference and both GPU output paths. A raster
IRQ handler exercises split-screen palette changes, including boundary rows,
vertical blank and the next frame. These are regression tests of the current
approximation, not evidence of exact hardware latency.

Validate writes immediately before and after each hardware-backed fetch point,
checking which output row changes. VRAM write history, precise interlace,
sprite rendering and special effects remain outside the current scope.

```sh
cargo test -p msx68k-bus --test gvram --test vram_pages --test bg --test raster -- --test-threads=1
cargo test --release -p msx68k-vicon --test graphics_modes --test raster -- --test-threads=1
```
