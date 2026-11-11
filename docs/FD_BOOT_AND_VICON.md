# Floppy Boot, SX-Window v3.1, Peripherals, and `wgpu` VICON Pipeline

This document describes the motherboard peripheral emulation (`crates/msx68k-bus`), GPU-accelerated video compositor (`crates/msx68k-vicon`), and interactive desktop runner (`crates/msx68k-app`) required to boot and interact with standard distributions of **Human68k v3.02** (`HUMAN302.XDF`) and **SX-Window v3.1** (`SXWIN311.XDF`–`SXWIN316.XDF`) from an **X68000 EXPERT-class IPL ROM**.

---

## 1. Boot & Interrupt Hierarchy (`crates/msx68k-bus`)

The X68000 uses four primary vectored interrupt controllers during IPL POST, Human68k floppy boot, and SX-Window GUI execution:

| Priority | Device | Address Range | Vector Base | Primary Boot & Runtime Role |
|---|---|---|---|---|
| **Level 6** | **MC68901 MFP** ([`mfp.rs`](../crates/msx68k-bus/src/mfp.rs)) | `$E88001`–`$E8802F` (odd) | `$40`–`$4F` (`VR` `$E88017`) | `GPIP` sync (`V-DISP`, `H-SYNC`, `CRTC RINT`, `OPMIRQ`), Timer A (V-DISP event count & delay), Timer C (system tick, FDD motor timeout, & mouse polling trigger), USART keyboard receive (`RRDY`) |
| **Level 5** | **Z8530 SCC** ([`peripherals.rs`](../crates/msx68k-bus/src/peripherals.rs)) | `$E98001`–`$E98007` (odd) | `$50`–`$5F` (`WR2` status-modified) | Serial Communications Controller; Channel B (`$E98001` Control, `$E98003` Data) receives 3-byte mouse packets at status-modified vector `$54` |
| **Level 3** | **HD63450 DMAC** ([`dmac.rs`](../crates/msx68k-bus/src/dmac.rs)) | `$E84000`–`$E840FF` | `NIV` (`$E84025`) / `EIV` (`$E84027`) | 4-channel DMA controller; Channel 0 transfers sectors between FDC (`$E94003`) and Main RAM |
| **Level 1** | **I/O Interrupt Controller** ([`fdc.rs`](../crates/msx68k-bus/src/fdc.rs)) | `$E9C001`–`$E9C003` (odd) | `$60`–`$63` (`$E9C003 & $FC`) | Arbitrates FDC (`vec | 0`), FDD (`vec | 1`), SASI HDC (`vec | 2`), and Printer (`vec | 3`) interrupts |

### 8-Bit Odd-Byte Bus Wiring

On real X68000 hardware, 8-bit peripheral chips (`$E86000`–`$E9DFFF`: Supervisor Area Set, MFP, RTC, Printer, System Port, OPM, ADPCM, FDC, HDC, SCC, PPI, and I/O Interrupt Controller) are wired to the lower byte (`D7..D0`, controlled by `LDS`) of the 16-bit data bus:
- **8-bit byte access (`MOVE.B`) at an even address** asserts `UDS` only, causing a hardware **Bus Error (`Vector 2`)** on devices whose chip-select requires `LDS`.
- **16-bit word access (`MOVE.W`) at an even address** asserts both `UDS` and `LDS`, reading `0xFF00 | read_u8(addr | 1)` and writing `val & 0xFF` to `addr | 1` without faulting.

### SRAM Factory Header & Main RAM Size Preservation (`$ED0000`–`$ED005A`)

During cold reset, the X68000 IPL ROM validates the 16 KB battery-backed SRAM (`$ED0000`–`$ED3FFF`) before sizing Main RAM and initializing the Text Palette:
1. **Shift-JIS Signature (`$ED0000`–`$ED0007`)**:
   - Real hardware IPL ROM checks `$ED0000`–`$ED0007` against the 8-byte signature `[0x82, 0x77, b'6', b'8', b'0', b'0', b'0', b'W']` (`"Ｘ68000W"`, where `$82 $77` is the Shift-JIS full-width `'Ｘ'`).
   - If this signature does not match, the IPL ROM treats SRAM as uninitialized and overwrites `$ED0008` (configured Main RAM end address) with the 1 MB factory minimum (`$0010_0000`), which causes SX-Window v3.1 to abort startup with an out-of-memory dialog (`"メモリ容量が足りません 再起動します。"`).
2. **Main RAM Size (`$ED0008`–`$ED000B`) & Default Text Palette (`$ED002E`–`$ED003B`)**:
   - [`X68kBus::new`](../crates/msx68k-bus/src/bus.rs) initializes `$ED0000`–`$ED0059` with the hardware factory default SRAM table (including contrast `$0E`, key repeat timings, and the initial Text Palette entries `$0000`, `$F83E`, `$FFC0`, `$FFFE`, `$DE6C`, `$4022`, `$0302`) and writes the configured `ram_size_bytes` (`4 MB` = `$0040_0000` by default) to `$ED0008`.

---

## 2. Floppy Disk Subsystem (`uPD72065` FDC + FDD + DMAC Ch0 + OPM `CT2`)

Implemented in [`crates/msx68k-bus/src/fdc.rs`](../crates/msx68k-bus/src/fdc.rs) and [`crates/msx68k-bus/src/dmac.rs`](../crates/msx68k-bus/src/dmac.rs):

1. **Drive Probing via OPM `CT2` (`$E90001`/`$E90003` Reg `$1B` bit 6)**:
   - During POST and Human68k initialization, IPLROM asserts `CT2 = 1` on the YM2151 OPM to force the FDC `READY` line high and issues `RECALIBRATE` (`0x07`) across units `0..=3`.
   - Connected drives (`0` and `1`) complete `RECALIBRATE` with `ST0 = ST0_SE | unit` (`0x20 | unit`), while unconnected drives (`2` and `3`) return `ST0_AT | ST0_SE | ST0_EC | unit` (`0x70 | unit`).
2. **Motor & Ready Attention Interrupt (`$E94007`)**:
   - Writing `$E94007` with bit 7 (`Motor ON`) on a drive with an inserted disk transitions the drive to `Ready` (`ST3_RY = 0x20`) and raises an Attention Interrupt (`ST0 = 0xC0 | unit`, `PCN`) serviced via `SENSE INTERRUPT STATUS` (`0x08`).
3. **Sector DMA Transfers (`READ DATA` `0x06`, `WRITE DATA` `0x05`)**:
   - Supports standard 2HD `.XDF` (`1,261,568` bytes: `77` cylinders × `2` heads × `8` sectors × `1024` bytes, `N = 3`) as well as 9-sector 2HD, 18-sector 2HQ, and 2DD disk geometries.
   - `.DIM` images of 2HD, 2HC, and 2HQ media are expanded into raw sector images on insertion; tracks flagged absent in the DIM header read as `$E5` filler.
   - Multi-sector (`EOT`) and multi-track (`MT = 0x80`) reads/writes stream bytes through **HD63450 DMAC Channel 0** (`No Chaining`, `Array Chaining`, and `Link Array Chaining`), updating `MAR`/`MTC`, setting `CSR_COC` (`0x80`), and asserting both DMAC Level 3 and FDC Level 1 interrupts upon completion.

---

## 3. Z8530 SCC Serial Mouse & MC68901 MFP Keyboard Input

### 3.1 Z8530 SCC Channel B 3-Byte Mouse Protocol (`crates/msx68k-bus/src/peripherals.rs`)

On the X68000, the mouse is connected to **Channel B** of the Zilog **Z8530 SCC** (`$E98001` Control, `$E98003` Data):
1. **Timer C `RTS` Polling Edge**:
   - The IPL ROM's MFP Timer C interrupt handler (`$FF1482`) periodically sets the receive counter (`$092A = 3`) and buffer pointer (`$092C = $0930`), then writes `WR5 = $02` (`RTS` bit 1 transitioning `0 -> 1`) to SCC Channel B (`$E98001`).
2. **3-Byte Packet Latching & Level 5 Status-Modified Interrupt (`Vector $54`)**:
   - On the `0 -> 1` transition of `WR5` bit 1 (`RTS`), [`Scc`](../crates/msx68k-bus/src/peripherals.rs) clamps accumulated relative movement (`dx`, `dy` in `-127..=127`) and latches a 3-byte packet into Channel B's receive FIFO:
     - Byte 0 (`status`): bit 0 (`0x01`) = Left Button pressed, bit 1 (`0x02`) = Right Button pressed, bits 4..7 = overflow flags.
     - Byte 1 (`dx`): 8-bit two's-complement horizontal delta (`+` right, `-` left).
     - Byte 2 (`dy`): 8-bit two's-complement vertical delta (`+` down, `-` up).
   - While bytes remain in the FIFO and `WR9` (`MIE` bit 3) + `WR1` (Rx Interrupt Enable bits 4..3) are enabled and not currently in-service (`!ius_b`), `Scc` asserts a **Level 5 interrupt** returning the status-modified vector `(WR2 & 0xF0) | 0x04` (`$54` = Channel B Receive Character Available).
3. **`Reset Highest IUS` (`WR0 = $38`) & SX-Window Mouse Hook**:
   - At the end of each byte's interrupt service routine (`$FF150A`), the IPL ROM writes `$38` (`Reset Highest IUS`) to `$E98001`, clearing `ius_b` so the next byte in the 3-byte packet immediately triggers the next Level 5 interrupt.
   - Once all 3 bytes are received into `$0930..$0932`, the ISR invokes the registered mouse callback at `($0938)` (`SX-Window` cursor/event hook), which updates cursor coordinates, redraws the arrow cursor on TVRAM, and posts `MouseDown` (`1`) / `MouseUp` (`2`) events via `_PostEvent` (`$A0A4`).

### 3.2 MC68901 MFP USART Keyboard (`crates/msx68k-bus/src/mfp.rs`)

- Host keyboard events are translated into 7-bit X68000 hardware scan codes (`0x01`–`0x7F` for key press, `scancode | 0x80` for key release) and queued into [`Mfp::push_keyboard_byte`](../crates/msx68k-bus/src/mfp.rs).
- When the USART Receiver Buffer (`UDR`, `$E8802F`) is empty (`RSR` bit 7 == `0`), the next scan byte is loaded into `UDR`, `RSR` bit 7 (`Buffer Full`) is set, and MFP Channel 12 (`CH_RRDY`, `Vector $4C`) is asserted at Level 6.

---

## 4. CRTC & TVRAM Hardware Acceleration (`crates/msx68k-bus/src/crtc.rs`)

The X68000 has no hardware text character generator mode; both console text and the entire **SX-Window v3.1 GUI desktop** are rendered as bitmaps into the 512 KB 4-plane **TVRAM** (`$E00000`–`$E7FFFF`, 4 × 128 KB planes of `1024×1024×1bpp`). To achieve fast text rendering, window fills, clearing, and scrolling, the CRTC (`$E80000`–`$E804FF`) provides three hardware acceleration mechanisms emulated in [`Crtc`](../crates/msx68k-bus/src/crtc.rs):

1. **Simultaneous Multi-Plane Write (`R21` `$E8002A` bit 8 & bits 7..4)**:
   - When `R21` bit 8 (`0x0100`) is set, a single CPU byte or word write to any TVRAM plane offset mirrors simultaneously across all planes selected by `R21` bits `7..4`.
2. **Write Bitmask (`R21` bit 9 & `R23` `$E8002E`)**:
   - When `R21` bit 9 (`0x0200`) is set, bits set to `1` in `R23` preserve the existing TVRAM bits while bits set to `0` are overwritten by CPU data (`(old & mask) | (new & !mask)`).
3. **Hardware 4-Scanline Raster Copy (`R21` bits 3..0, `R22` `$E8002C`, Operation Port `$E80481` bit 3)**:
   - Copies a 4-scanline (`512`-byte) block from source block `R22 >> 8` (`0..255`) to destination block `R22 & 0xFF` (`0..255`) across all TVRAM planes enabled in `R21 & 0x000F`.

### SX-Window v3.1 Boot & Desktop State

When booting `SXWIN311.XDF`:
1. `IPLROM.DAT` loads the floppy boot sector and `HUMAN.SYS`, processes `CONFIG.SYS` (`USKCG.SYS`, `RSDRV.SYS`, `ASK68K.SYS`), and executes `AUTOEXEC.BAT` (`OPMDRV3.X`, `FDDEVICE.X`, `FLOAT2.X`, `FSX.X`, `TITLE.X -s TITLE.SX`, and `SXWIN.X -@`).
2. `SXWIN.X` loads `SXKERNEL.X`, `HENWIN.X`, `SYSTEM.LB`, `BUILTIN.LB`, and `ICON.LB`, configures VICON to `R0 = $0004`, `R1 = $2484`, `R2 = $003F` (`768×512` resolution), sets the 16-color SX-Window GUI palette in Text Palette `$E82200`–`$E8221F`, and draws the desktop background, the left-hand **Symbol Tray (`シンボルトレイ`)** (SX emblem, workstation icon, `FD0`/`FD1` drive icons, folder/RAM disk icons, and Trash Can), and the mouse cursor directly onto TVRAM.

---

## 5. `wgpu` VICON GPU Compositor (`crates/msx68k-vicon`) & Desktop Runner (`crates/msx68k-app`)

Implemented in [`crates/msx68k-vicon/src/lib.rs`](../crates/msx68k-vicon/src/lib.rs), [`crates/msx68k-vicon/src/shader.wgsl`](../crates/msx68k-vicon/src/shader.wgsl), and [`crates/msx68k-app/src/main.rs`](../crates/msx68k-app/src/main.rs):

- **Compute & Surface Pipeline**:
  - Uploads the 512 KB 4-plane `TVRAM`, 512 KB `GVRAM`, 512-entry `GGGGGRRRRRBBBBBI` Text/Graphics Palettes (`$E82000`–`$E823FF`), and `ViconUniforms` (CRTC active resolution, `R10`/`R11` text scroll, `R12`/`R13` graphic scroll, `VICON R0/R1/R2` mode/priority/enable registers, and System Port contrast) to `wgpu` buffers.
  - Dispatches a `@compute @workgroup_size(16, 16, 1)` WGSL compute pipeline that composites the 4-plane TVRAM and GVRAM layers with 10-bit wrap-around scrolling and 6-bit-per-channel (`5-bit + I-bit`) palette expansion into an `RGBA8` storage buffer, followed by a fullscreen surface presentation pass (`vs_fullscreen` / `fs_present`) with aspect-ratio preservation.
  - Verified in [`crates/msx68k-vicon/tests/vicon_render.rs`](../crates/msx68k-vicon/tests/vicon_render.rs) for 100% pixel-exact parity between the `wgpu` GPU pipeline and the CPU reference compositor across synthetic patterns, Human68k v3.02 CLI boot, and the live SX-Window v3.1 GUI desktop.
- **Interactive Window Controls (`msx68k-app`)**:
  - Mouse motion and left/right clicks inside the window are scaled to CRTC active resolution and forwarded to [`Scc`](../crates/msx68k-bus/src/peripherals.rs).
  - Keyboard input is forwarded to [`Mfp`](../crates/msx68k-bus/src/mfp.rs), while function keys `F1` (`10MHz`), `F2` (`16MHz XVI`), `F3` (`24MHz RedZone`), `F4` (`Burst`), and `F12` (`Reset`) dynamically control the VM at runtime.

