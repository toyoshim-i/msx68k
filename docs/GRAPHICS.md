# Graphics Design and Plan

The CPU manages VRAM, registers and emulated time. The GPU composes pixels through
one shared WGSL implementation for the primary VM and VMX screens. The CPU
compositor is a validation reference. See [VMX68K](VMX68K.md) for screen ownership
and threading, and [TESTING](TESTING.md) for the test workflow.

## Current capabilities and remaining work

1. Implemented basic GVRAM access and display: 512-dot 16/256/65536-color modes, 1024-dot
   16-color mode, page assignment, independent scrolling and text priority.
2. Implemented scanline approximation and next refinement: mid-frame scroll, palette and display-control changes. The
   current implementation samples at line boundaries. Before adding BG, validate
   graphics fetch points and attribute-specific latency. Mid-frame VRAM writes
   need a separate history.
3. BG, PCG and sprites: extend GPU composition and model display limits, fetch
   stages and line buffers.
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

At each line boundary, the current implementation samples text and graphics
scroll, VICON mode/priority/display control, contrast and both palettes. A write
during a line affects the following line; repeated writes within a line use the
last value. This is an approximation, not a verified hardware latch rule. The
CRTC still uses approximate 317/625-cycle line periods.

Capture runs from active-display start to active-display end. Entering vertical
blank publishes the completed history, so subsequent register and palette writes
do not recolor the completed frame. Unchanged lines and redundant palette writes
share a state. Reset discards history, and incompatible geometry changes discard
the partial capture. Doubled scanlines map physical rasters to output rows.

When all display planes are off, capture records only the transition to blank
and skips other register and palette snapshots. Re-enabling display samples the
latest state. A fully blank frame needs no palette upload.

The GPU receives a row-to-state index and register/palette records at changes.
Static frames retain the fixed-size palette upload path, while VM screens keep
physical dirty-page VRAM uploads. Native extended TVRAM does not use hardware
scanout timing and retains its existing composition path.

Peripheral advancement visits horizontal-sync edges, line boundaries and vertical
display transitions even in a large time batch. Passing over the raster-IRQ line
in one update must not lose its interrupt.

VRAM itself is still read at render time. Register history alone cannot reproduce
mid-frame VRAM writes or the exact relationship between fast clear and pixels
that were already displayed.

## Fetch timing refinement

Separate the CPU write position from the point where a value is fetched and the
point where it affects output. Graphics scroll, VRAM reads, palette conversion
and display control can belong to different stages. BG and sprite attributes
also need their own fetch and buffering rules. Do not assign one unverified
next-line rule or delay count to every attribute.

The data stored in a line buffer matters: palette codes and converted colors
respond differently to later palette writes. Buffer count alone does not specify
latency. Keep CPU history independent of GPU representation. A future timestamped change
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
BG/sprite rendering and special effects remain outside the current scope.

```sh
cargo test -p msx68k-bus --test gvram --test vram_pages --test raster -- --test-threads=1
cargo test --release -p msx68k-vicon --test graphics_modes --test raster -- --test-threads=1
```
