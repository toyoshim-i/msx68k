# Hardware Implementation Gaps

Audit of unimplemented, stubbed, or approximated X68000 / X68030 hardware features in `msx68k`.
Each section references the corresponding source files and existing design documents.

---

## 1. Graphics (VICON / CRTC / CYNTHIA)

See also [GRAPHICS.md](GRAPHICS.md) for the planned graphics roadmap.

- **Mid-Frame TVRAM / GVRAM Write History** (low priority / deferred):
  - Register, palette, and BG/Sprite states are captured per scanline, but TVRAM and GVRAM pixels are read from frame-final memory at render time, so mid-frame VRAM updates and mid-frame Graphic Fast Clear timing are not reproduced.
  - Note: VICON special priority & half transparency (`R2` `EXON`/`HP`/`BP`/`GG`/`GT`), CRTC dot-clock horizontal timing, mixed-resolution CYNTHIA scaling, and interlaced field alternation (with 75% phosphor persistence dimming on non-scanned lines) are implemented across CPU, Compute, and GLES 2.0 backends.

---

## 2. Real-Time Clock (RP5C15, `$E8A000`)

- **No Alarm / 1 Hz / 16 Hz Interrupts** (not currently needed):
  - Live host local-time synchronization is implemented (`crates/msx68k-bus/src/peripherals.rs`), while alarm matching and periodic RTC interrupt outputs are not wired to `Mfp::CH_GPIP0_ALARM` (`GPIP` bit 0).

---

## 3. Input, Serial & On-Board I/O (PPI / SCC / MFP / Printer / System Port)

- **Joystick 2P & Multi-Button Pads (`i8255 PPI`, `$E9A000`)**:
  - Port B (`$E9A003`, Joystick 2) is hardcoded to `0xFF` and not connected to host input (`crates/msx68k-bus/src/peripherals.rs`).
  - Strobe-based multiplexed pad protocols via PPI Port C (e.g., Mega Drive 6-button pad, Cyber Stick, FM Towns pad) are not implemented.
- **RS-232C (`Z8530 SCC` Channel A, `$E98005`–`$E98007`)**:
  - Only Channel B (3-byte serial mouse) is implemented; Channel A returns fixed dummy status/data (`0x24` / `0x00`).
- **MFP (`MC68901`, `$E88000`)**:
  - Keyboard USART transmit commands (`UDR` writes for LEDs, key repeat, TV control, MSCTRL, etc.) are acknowledged as ready without applying state (`crates/msx68k-bus/src/mfp.rs`).
  - Timer pulse-width measurement modes (`control` `0x09..=0x0F`) and Timer B event count mode are not implemented (only Timer A `V-DISP` event count is implemented).
- **Printer Port (`$E8C000`)**:
  - Stubbed as `0xFF` on read and ignored on write (`crates/msx68k-bus/src/bus.rs`).
- **System Port (`$E8E000`)**:
  - Soft power-off via `$E8E00F` and full reset-time ROM mirroring at `$000000` (beyond reset `SSP`/`PC`) are simplified.

---

## 4. Storage (FDC / SASI / SCSI)

- **FDC (`uPD72065`, `$E94000`) Commands & Image Formats**:
  - `WRITE ID` / `FORMAT` (`0x0D`), `READ DIAGNOSTIC` (`0x02`), and `SCAN` (`0x11`, `0x19`, `0x1D`) are not handled in `Fdc::execute_command` (`crates/msx68k-bus/src/fdc.rs`) and return `ST0_IC` (Invalid Command).
  - `crates/msx68k-bus/src/floppy_image.rs` supports Raw (`.XDF`, etc.) and `.DIM` images only; `.D88` (variable sector sizes, CRC/deleted-mark errors, copy-protected tracks) and physical seek/rotational latency are not implemented.
- **SASI HDC (`$E96000`) & DMAC Channel 1**:
  - Stubbed as `0x00` on read and no-op on write (`crates/msx68k-bus/src/bus.rs`); SASI DMA request source on DMAC Ch1 is unconnected.
- **Hardware SCSI Controller (`MB89352 SPC`, `$E96020` / `$EA0000`)**:
  - SCSI is emulated via Virtual SCSI ROM + `IOCS $F5` hypercalls (`crates/msx68k-bus/src/scsi.rs`). Software or OSes (such as NetBSD/x68k or custom SCSI drivers) that access `MB89352` registers directly are not supported.

---

## 5. Sound & MIDI (ADPCM / OPM / YM3802)

- **ADPCM Recording (`MSM6258V`, `$E92001` bit 2)**:
  - Writing bit 2 sets `recording = true` in `crates/msx68k-bus/src/adpcm.rs`, but audio capture/encoding and DMAC Ch3 device-to-memory transfers are not implemented.
  - Note: `Dmac::write_u8` (`crates/msx68k-bus/src/dmac.rs`) still checks `ch_idx != 0` for its `MSX68K_LOG_UNIMPLEMENTED` external-request diagnostic even though ADPCM Ch3 is now connected.
- **OPM (`YM2151`) Hardware Edge Differences**:
  - Documented in [OPM.md](OPM.md): YM3012 floating-point DAC quantization and L/R half-sample offset, test register `$01` bits other than LFO reset, CSM mode when `AR < 62`, TDM write pipeline latency, and Ch7 noise amplitude granularity.
- **CZ-6BM1 MIDI (`YM3802`) Advanced MCS Features**:
  - Core Tx/Rx FIFOs, interrupts, General Purpose Timer, and MIDI Clock Timer are implemented (`crates/msx68k-bus/src/midi.rs`); playback/click counter (IRQ bit 0), Break/Off-Line detection, and hardware MIDI thru/filter registers are not implemented.

---

## 6. CPU, Memory & Expansion Bus (MC68000 / MC68030)

See also [CPU_AND_BUS.md](CPU_AND_BUS.md) Sections 7.1, 7.4, and 7.5.

- **FPU (`MC68881` / `MC68882`)**:
  - Coprocessor instructions raise an F-Line exception (matching a stock X68030 without an FPU).
- **MC68030 MMU Address Translation**:
  - `PMOVE`/`PFLUSH`/`PLOAD`/`PTEST` hold register values, but address translation is not performed (matching the stock `MC68EC030`).
- **High Memory (`$0100_0000`+)**:
  - Main RAM is capped at 12 MB (`$000000`–`$BFFFFF`); 32-bit addresses outside msx68k's reserved extension range still mirror the 24-bit bus rather than exposing X68030 / Xellent30 high memory.
- **MC68030 Execution / Cache Edge Cases**:
  - `SR` `T0` (trace on change of flow) is stored but does not trigger trace exceptions.
  - `RTE` from Format `$A`/`$B` bus-fault frames restarts the faulted instruction rather than resuming the faulted bus cycle.
  - Instruction/data caches model tags for timing only, not cached data contents (so self-modifying code always observes memory directly).
- **Other Expansion Boards**:
  - Expansion I/O outside CZ-6BM1 MIDI (`$EAFA00`–`$EAFA1F`) — such as Neptune-X / Nereid Ethernet or Mercury-Unit PCM — raises a Bus Error.
