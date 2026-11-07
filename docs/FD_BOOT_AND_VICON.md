# Floppy Boot Subsystem, CRTC/TVRAM Acceleration, and `wgpu` VICON Pipeline

This document describes the motherboard peripheral emulation (`crates/msx68k-bus`) and GPU-accelerated video compositor (`crates/msx68k-vicon`) required to boot standard distributions of **Human68k** (such as `HUMAN302.XDF`) from an **X68000 EXPERT-class IPL ROM**.

---

## 1. Boot & Interrupt Hierarchy (`crates/msx68k-bus`)

The X68000 uses three primary vectored interrupt controllers during IPL POST and Human68k floppy boot:

| Priority | Device | Address Range | Vector Base | Primary Boot Role |
|---|---|---|---|---|
| **Level 6** | **MC68901 MFP** ([`mfp.rs`](../crates/msx68k-bus/src/mfp.rs)) | `$E88001`–`$E8802F` (odd) | `$40`–`$4F` (`VR` `$E88017`) | `GPIP` sync (`V-DISP`, `H-SYNC`, `CRTC RINT`, `OPMIRQ`), Timer A (V-DISP event count & delay), Timer C (system tick & FDD motor timeout), USART keyboard receive (`RRDY`) |
| **Level 3** | **HD63450 DMAC** ([`dmac.rs`](../crates/msx68k-bus/src/dmac.rs)) | `$E84000`–`$E840FF` | `NIV` (`$E84025`) / `EIV` (`$E84027`) | 4-channel DMA controller; Channel 0 transfers sectors between FDC (`$E94003`) and Main RAM |
| **Level 1** | **I/O Interrupt Controller** ([`fdc.rs`](../crates/msx68k-bus/src/fdc.rs)) | `$E9C001`–`$E9C003` (odd) | `$60`–`$63` (`$E9C003 & $FC`) | Arbitrates FDC (`vec | 0`), FDD (`vec | 1`), SASI HDC (`vec | 2`), and Printer (`vec | 3`) interrupts |

### 8-Bit Odd-Byte Bus Wiring

On real X68000 hardware, 8-bit peripheral chips (`$E86000`–`$E9DFFF`: Supervisor Area Set, MFP, RTC, Printer, System Port, OPM, ADPCM, FDC, HDC, SCC, PPI, and I/O Interrupt Controller) are wired to the lower byte (`D7..D0`, controlled by `LDS`) of the 16-bit data bus:
- **8-bit byte access (`MOVE.B`) at an even address** asserts `UDS` only, causing a hardware **Bus Error (`Vector 2`)** on devices whose chip-select requires `LDS`.
- **16-bit word access (`MOVE.W`) at an even address** asserts both `UDS` and `LDS`, reading `0xFF00 | read_u8(addr | 1)` and writing `val & 0xFF` to `addr | 1` without faulting.

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
   - Multi-sector (`EOT`) and multi-track (`MT = 0x80`) reads/writes stream bytes through **HD63450 DMAC Channel 0** (`No Chaining`, `Array Chaining`, and `Link Array Chaining`), updating `MAR`/`MTC`, setting `CSR_COC` (`0x80`), and asserting both DMAC Level 3 and FDC Level 1 interrupts upon completion.

---

## 3. CRTC & TVRAM Hardware Acceleration (`crates/msx68k-bus/src/crtc.rs`)

The X68000 has no hardware text character generator mode; all console text is rendered as bitmaps into the 512 KB 4-plane **TVRAM** (`$E00000`–`$E7FFFF`, 4 × 128 KB planes of `1024×1024×1bpp`). To achieve fast text rendering, clearing, and scrolling, the CRTC (`$E80000`–`$E804FF`) provides three hardware acceleration mechanisms emulated in [`Crtc`](../crates/msx68k-bus/src/crtc.rs):

1. **Simultaneous Multi-Plane Write (`R21` `$E8002A` bit 8 & bits 7..4)**:
   - When `R21` bit 8 (`0x0100`) is set, a single CPU byte or word write to any TVRAM plane offset mirrors simultaneously across all planes selected by `R21` bits `7..4`.
2. **Write Bitmask (`R21` bit 9 & `R23` `$E8002E`)**:
   - When `R21` bit 9 (`0x0200`) is set, bits set to `1` in `R23` preserve the existing TVRAM bits while bits set to `0` are overwritten by CPU data (`(old & mask) | (new & !mask)`).
3. **Hardware 4-Scanline Raster Copy (`R21` bits 3..0, `R22` `$E8002C`, Operation Port `$E80481` bit 3)**:
   - Copies a 4-scanline (`512`-byte) block from source block `R22 >> 8` (`0..255`) to destination block `R22 & 0xFF` (`0..255`) across all TVRAM planes enabled in `R21 & 0x000F`.

---

## 4. `wgpu` VICON GPU Compositor (`crates/msx68k-vicon`)

Implemented in [`crates/msx68k-vicon/src/lib.rs`](../crates/msx68k-vicon/src/lib.rs) and [`crates/msx68k-vicon/src/shader.wgsl`](../crates/msx68k-vicon/src/shader.wgsl):

- Uploads the 512 KB 4-plane `TVRAM`, 512 KB `GVRAM`, 512-entry `GGGGGRRRRRBBBBBI` Text/Graphics Palettes (`$E82000`–`$E823FF`), and `ViconUniforms` (CRTC active resolution, `R10`/`R11` text scroll, `R12`/`R13` graphic scroll, `VICON R0/R1/R2` mode/priority/enable registers, and System Port contrast) to `wgpu` buffers.
- Dispatches a `@compute @workgroup_size(16, 16, 1)` WGSL compute pipeline that composites the 4-plane TVRAM and GVRAM layers with 10-bit wrap-around scrolling and 6-bit-per-channel (`5-bit + I-bit`) palette expansion into an `RGBA8` framebuffer.
- Verified for 100% pixel-exact parity between the `wgpu` Vulkan pipeline and the CPU reference compositor across synthetic patterns and a full Human68k v3.02 floppy boot (`A>` prompt).
