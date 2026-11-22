# Graphics Design and Plan

The CPU manages VRAM, registers, emulated time and compact scanline/span
metadata. Pixel rasterization and composition run on the GPU through two
shader-generation backends selected via `RendererConfig` (`ShaderGeneration`
and `PixelFormat`): a single-pass `16×16` tile Compute pipeline (`wgpu-backend`,
shared across primary and VMX screens) and a headless `EGL + GLES 2.0` fallback
(`gles2-backend`) for environments without Compute shaders. The CPU compositor
(`render_bus_frame_cpu` / `ShaderGeneration::Cpu`) serves as a validation
reference and software fallback. See [VMX68K](VMX68K.md) for screen ownership
and threading, and [TESTING](TESTING.md) for the test workflow.

## Current capabilities and remaining work

1. Implemented basic GVRAM access and display: 512-dot 16/256/65536-color modes, 1024-dot
   16-color mode, page assignment, independent scrolling and text priority.
2. Implemented scanline approximation: mid-frame scroll, palette and display
   control changes. Line-start address fetch is separate from horizontal display
   sampling. Mode-specific fetch timing still needs validation, and mid-frame
   text/graphics VRAM writes need a separate history.
3. Implemented GPU-evaluated BG/PCG and sprite composition via active-span
   coalescing (`CynthiaFrame`): 8-dot BG0/BG1, 16-dot BG0, 128 sprite
   attributes, a 32-sprite scanline limit, and 128-byte PCG/map block versioning.
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

The GPU receives a row-to-state index, deduplicated register records, and
independent 16-bit palette snapshots (`RasterFrame.palettes`) rather than a
palette table per raster line. Primary rendering (`render_bus_frame_dirty` /
`render_bus_to_surface_view_dirty`), headless `Gles2Compositor`, and `VmScreen`
all consume physical `DirtyVramPages` for incremental VRAM page uploads while
restoring software cursor overlay changes cleanly. Native extended TVRAM does not
use hardware scanout timing and retains its existing layout.

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

At horizontal display start, scanout captures the next physical raster's BG
control, scroll, origin, and 128-byte PCG/tilemap block references into an
immutable per-row `BgRowState` rather than expanding a CPU pixel buffer. The
last blanking raster prepares the first active row. This is an explicit
scheduling approximation; mode-specific hardware fetch cycles remain to be
validated. Tracking continues while output is disabled; BG enable bits and the
controller's master display gate are evaluated at composition time. A zero
pattern nibble is transparent regardless of the palette block.

The completed frame versions modified 128-byte PCG and tilemap blocks (`pcg_blocks`
and `pcg_tables`), so subsequent PCG or map writes never alter already-fetched
rows. Color conversion uses that output row's shared text/sprite palette. BG0
appears above BG1; their combined output participates in VICON text/sprite
priority and graphics insertion. Special palette-zero interactions remain future
work.

Both GPU generations evaluate BG0/BG1 directly on the GPU without an
intermediate 512-pixel code buffer: the Compute backend samples `pcg_blocks`
within a single `16×16` tile workgroup pass, while the GLES 2.0 backend draws
full-screen BG0/BG1 quads at depth keys `258` and `129` into the offscreen
CYNTHIA pass. Blank BG frames add no CYNTHIA upload. Before a complete scanout
history exists, capture helpers synthesize current BG/sprite state; native
extended TVRAM retains its existing path.

## Sprite scanout

The 128 sprite entries at `$EB0000` contain 10-bit X/Y coordinates, a 16-dot
pattern number, palette block, flips, and priority. Coordinates include a
16-pixel margin relative to the shared sprite/BG origin. Priority zero disables
an entry. The unused attribute half at `$EB0400..$EB07FF` and reserved
controller offsets `$EB0812..$EB7FFF` read as all ones and ignore writes;
bulk initialization of these ranges must not raise a bus error or alias live
registers. These addresses do not add another 128 sprites.

Selection visits register numbers in ascending order and admits at
most 32 enabled sprites intersecting the output row and viewport. Transparent
patterns consume a slot; disabled and entirely offscreen entries do not.

Within a pixel, higher sprite priority wins; equal priorities retain the lower
register number. Zero pattern nibbles expose underlying pixels. Final internal
ordering is sprite priority 3, BG0, sprite priority 2, BG1, sprite priority 1.
BG enable bits are applied at this final step. The controller master gate and
VICON sprite/BG gate suppress output without stopping internal fetch.

Sprite attribute selection and PCG block latching prepare two physical rasters
ahead at horizontal display start; BG state prepares one raster ahead. Using an
incremental 128-bit scanline membership index (`CynthiaTracker`) and block
versioning, vertically adjacent rows with identical visibility, 32-sprite
admission, coordinates, attributes, and PCG blocks coalesce into `16×h`
`SpriteSpan` records (`CynthiaFrame.spans`). Startup needs the preceding
blanking rasters to fill the stages, and reset clears pending rings. Latched
sprite attributes and versioned 128-byte PCG blocks cannot be replaced by later
RAM writes, while each displayed row uses its own palette snapshot.

## Shader generations and embedder configuration

`msx68k-vicon` exposes `ViconCompositor` configured by `RendererConfig`
(`ShaderGeneration::{Auto, Compute, Gles2, Cpu}`, `max_width`, `max_height`,
and `PixelFormat::{Rgba8, Rgb565}`) behind optional cargo features
(`wgpu-backend`, `gles2-backend`) with hardware/environment detection isolated
from core emulation:

- **Compute generation (`wgpu-backend`)**: Sorts `SpriteSpan`s into `16×16` tile
  bins (`O(S)` bucket sort) and runs a single-pass `16×16` workgroup Compute
  shader evaluating CYNTHIA (`SP3 → BG0 → SP2 → BG1 → SP1`), TVRAM, GVRAM,
  palettes, and contrast in one pass with zero intermediate fullscreen code
  textures.
- **GLES 2.0 generation (`gles2-backend`)**: Dynamically loads `libEGL.so.1` and
  `libGLESv2.so.2` for headless `EGL + GLES 2.0` two-pass composition: Pass 1
  rasterizes coalesced `16×h` sprite span quads and BG0/BG1 depth-tested planes
  into a depth-buffered offscreen FBO, and Pass 2 composites TVRAM, GVRAM,
  CYNTHIA, and palettes within an 8-sampler budget and `mediump`-safe standard
  coordinates.
- **CPU reference (`ShaderGeneration::Cpu`)**: Evaluates row-cached CYNTHIA
  scanlines (`CynthiaFrame::rasterize_row`) and VICON priority rules for testing
  and fallback.

## Dynamic sprite options

Both options default to off and can be changed on a running VM without resetting
CPU, RAM, SRAM or drives. Existing fetched rows retain their codes; new settings
apply to subsequent fetches. CPU-visible bank selection takes effect immediately.

- Disable the 32-sprite scanline limit to admit all intersecting entries from the
  existing 128-attribute table. This does not increase the attribute count.
- Enable full pattern memory to add a second independent 32 KiB pattern/map bank,
  allowing 512 large patterns across both banks. Disabling it hides the added
  RAM without erasing it; enabling it again restores access to the same contents.

`$EB0808` bit 10 (MPUCS) selects the bank visible through `$EB8000..$EBFFFF`.
Sprite attribute word 3 bit 2 (SPCS) selects the pattern bank independently for
each sprite. BG control bits 2/5 select BG0/BG1 map storage banks; bits 1/4
independently select TEXT0/TEXT1 within those banks. BG pattern pixels still
come from the standard pattern bank. An absent bank reads as `$FFFF`, ignores
CPU writes and supplies pattern nibble 15 to sprite fetch. Registers remain
shared and accessible regardless of MPUCS.

VMX settings stage both checkboxes until accepted, apply them without a restart
prompt, and save their flags in the setup's optional `V68G` resource. See
[VMX68K](VMX68K.md) for the host call and persistence contract.

## Next implementation steps

1. Validate address-fetch and display-sampling positions for each screen mode,
   including doubled lines and writes on either side of each edge. Keep the
   current timing approximation explicit until measured timing is available.
2. Validate mixed-resolution coordinate scaling and interlace, including sprite
   selection at viewport edges and pipeline behavior during geometry changes.
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
BG and sprite buffers continue fetching while display output is disabled. The
blank-frame snapshot/upload optimization only suppresses final output records
and transfers; it does not freeze internal line preparation.

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
checking which output row changes. Text/graphics VRAM write history, precise
interlace and special effects remain outside the current scope.

```sh
cargo test -p msx68k-bus --test gvram --test vram_pages --test bg --test sprite --test raster -- --test-threads=1
cargo test --release -p msx68k-vicon --test graphics_modes --test raster --test shader_generations -- --test-threads=1
```
