# MC68000 CPU Core & X68000 System Bus Architecture

This document describes the design and implementation of the `msx68k-cpu` and `msx68k-bus` crates, covering the monomorphized MC68000 / MC68030 instruction engine, the CPU/System Bus wait-state model across `10MHz`, `16MHz`, `24MHz`, and `Burst` modes, the Mixed Mode trap interface, and hardware compatibility verification.

---

## 1. Crate Structure

| Crate | Path | Responsibility |
| :--- | :--- | :--- |
| **`msx68k-cpu`** | `crates/msx68k-cpu` | Monomorphized MC68000 / MC68030 CPU core (`CpuCore<P>`), architectural register state (`CpuState`), ALU/BCD/bit/shift engine, exception handling, and A-Line/F-Line/`TRAP` hooks (`TrapHandler`). |
| **`msx68k-bus`** | `crates/msx68k-bus` | X68000 24-bit physical address map (`MemoryRegion`), CPU & System Bus timing engine (`BusTimingConfig`, `WaitStateTable`), lazy on-demand VRAM/I/O allocation (`X68kBus`), and unified VM runner (`X68kVm`). |

---

## 2. Monomorphized Execution Policies (`CpuPolicy`)

A central requirement of **msx68k** is that cycle-accurate bus contention and wait-state calculations for legacy games must impose **zero runtime overhead** on the Master SX-Window environment running in `Burst` mode.

### 2.1 Compile-Time Specialization
`CpuCore<P: CpuPolicy>` is parameterized over the `CpuPolicy` trait (`crates/msx68k-cpu/src/policy.rs`):

```rust
pub trait CpuPolicy: Copy + Clone + Default + Send + Sync + 'static {
    const TRACK_CYCLES: bool;
    const MC68030: bool = false;
}
```

- **`TrackedPolicy` (`TRACK_CYCLES = true`)**:
  - Used for `10MHz`, `16MHz` (XVI), `24MHz` (XVI RedZone), and custom cycle-accurate modes.
  - Accumulates standard Motorola MC68000 base instruction cycles plus per-transfer bus wait states (`Bus::wait_cycles_16`) on every instruction fetch, extension word fetch, and effective address read/write.
  - 32-bit (`Long`) memory accesses are split into two 16-bit bus transfers (`addr` and `addr + 2`), accumulating wait states for both words.
- **`BurstPolicy` (`TRACK_CYCLES = false`)**:
  - Used for the Master SX-Window VM and temporary fast-load boosts (`burst_on_disk_io`).
  - Because `P::TRACK_CYCLES` is a compile-time constant, Rust's dead-code elimination removes all cycle additions and `wait_cycles_16` lookups from the generated machine code.
- **`Tracked030Policy` (`TRACK_CYCLES = true`, `MC68030 = true`)**:
  - The 25 MHz X68030 (`CpuSpeedMode::Mhz25`). Instruction timing follows the MC68030 User's Manual rather than the MC68000 counts, which this policy ignores (Section 7.4).
- **`Burst030Policy` (`TRACK_CYCLES = false`, `MC68030 = true`)**:
  - Runs an MC68030 without cycle tracking (Section 7).

### 2.2 State Decoupling for Zero-Copy Mode Switching
Architectural state (`CpuState`: `D0–D7`, `A0–A7`, `PC`, `IR`, `SR`, `USP`, `SSP`, interrupt lines, `total_cycles`, the MC68030 registers `MSP`, `VBR`, `CACR`, `CAAR`, `SFC`, `DFC`, and the MC68030 cache tags and instruction overlap) is stored independently of the `CpuPolicy` type parameter. `CpuState::model` names the CPU, and `X68kVm` picks the policy from it and the speed mode (`with_policy!` in `vm.rs`). `X68kVm` can switch between `CpuCore<TrackedPolicy>` and `CpuCore<BurstPolicy>` on any instruction boundary (e.g., when entering or leaving disk I/O or changing application speed profiles) without copying registers or memory.

---

## 3. CPU Clock, System Bus Speed & Memory Region Timing

On real X68000 hardware, instruction execution speed is determined not only by the CPU clock frequency, but also by the **System Bus frequency** and the **response latency (`DTACK`) of the accessed memory or peripheral region**.

### 3.1 VM Roles and Speed Presets (`CpuSpeedMode`)

| Mode | CPU Clock | System Bus | Default Role / Target Profile |
| :--- | :--- | :--- | :--- |
| **`CpuSpeedMode::Mhz10`** | `10 MHz` | `10 MHz` | **Default for Isolated App VMs (`VmRole::AppVm`)**. Models original X68000 / ACE / PRO / SUPER timing. |
| **`CpuSpeedMode::Mhz16`** | `16 MHz` | `16 MHz` | **X68000 XVI**. Main RAM and the system bus run at 16 MHz (`0` wait on Main RAM), while 10 MHz video/peripheral accesses incur asymmetric synchronization waits. |
| **`CpuSpeedMode::Mhz24`** | `24 MHz` | `16 MHz` | **X68000 XVI RedZone**. A 24 MHz CPU coupled to a 16 MHz system bus (`+2` CPU wait cycles per 16-bit system bus transfer, plus scaled peripheral/VRAM wait cycles). |
| **`CpuSpeedMode::Mhz25`** | `25 MHz` | `25 MHz` | **X68030**, with an MC68030 (`VmConfig::with_cpu_model`). Bus waits come from the X68030 table in Section 7.4 instead of Sections 3.2 to 3.4. |
| **`CpuSpeedMode::Burst`** | Uncapped | Zero Delay | **Default for Master SX-Window VM (`VmRole::MasterSx`)**. Zero wait states (`wait_enabled = false`); selectable per application for tools and compilers. |
| **`CpuSpeedMode::Custom`** | Configurable | Configurable | Arbitrary `(cpu_mhz, system_bus_mhz, wait_enabled)` combinations. |

### 3.2 Physical Wait-State Formula

A standard zero-wait MC68000 bus cycle takes **4 clocks** on the System Bus ($f_{\text{bus}}$). Each region in the 16 MB address space is characterized by two physical parameters (`RegionTimingSpec`):
1. $W_{\text{sys}}$ (`sys_bus_waits`): Wait states synchronous to the System Bus (e.g., `0` for Main RAM, `1` for ROM/SRAM/SysPort).
2. $W_{10\text{MHz}}$ (`sub_bus_10mhz_waits`): Additional wait states in the 10 MHz video/peripheral sub-bus domain (`100 ns` units, plus optional `+1` VICON contention during active display scanlines on GVRAM/TVRAM).

For any CPU frequency $f_{\text{cpu}}$ and System Bus frequency $f_{\text{bus}}$, the total CPU clock cycles for a single 8-bit or 16-bit bus transfer are:

$$\text{Total CPU Cycles} = \left\lceil (4 + W_{\text{sys}}) \times \frac{f_{\text{cpu}}}{f_{\text{bus}}} + W_{10\text{MHz}} \times \frac{f_{\text{cpu}}}{10} \right\rceil$$

Since the MC68000 core's base instruction timing already accounts for 4 CPU cycles per 16-bit bus access, the extra wait cycles added per transfer are:

$$\text{Wait Cycles} = \max\left(0,\ \text{Total CPU Cycles} - 4\right)$$

### 3.3 X68000 Memory Map & Computed Wait Cycles (per 16-bit Bus Access)

| Address Range | Region | $W_{\text{sys}}$ | $W_{10\text{MHz}}$ | `10MHz` (Bus 10) | `16MHz` (Bus 16) | `24MHz` (Bus 16) | `Burst` |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| `$000000`–`$BFFFFF` | `MainRam` (up to 12 MB) | 0 | 0 | **0** | **0** | **2** | **0** |
| `$C00000`–`$DFFFFF` | `Gvram` (512 KB) | 0 | 1 (+1 disp) | **1** (2) | **2** (4) | **5** (7) | **0** |
| `$E00000`–`$E7FFFF` | `Tvram` (512 KB) | 0 | 2 (+1 disp) | **2** (3) | **4** (5) | **7** (10) | **0** |
| `$E80000`–`$E81FFF` | `Crtc` | 0 | 1 | **1** | **2** | **5** | **0** |
| `$E82000`–`$E83FFF` | `PaletteVicon` | 0 | 3 | **3** | **5** | **10** | **0** |
| `$E84000`–`$E85FFF` | `Dmac` (HD63450) | 0 | 15 | **15** | **24** | **38** | **0** |
| `$E86000`–`$E87FFF` | `AreaSet` | 1 | 0 | **1** | **1** | **4** | **0** |
| `$E88000`–`$E89FFF` | `Mfp` (MC68901) | 0 | 4 | **4** | **7** | **12** | **0** |
| `$E8A000`–`$E8BFFF` | `Rtc` (RP5C15) | 0 | 2 | **2** | **4** | **7** | **0** |
| `$E8C000`–`$E8FFFF` | `Printer` / `SysPort` | 1 | 0 | **1** | **1** | **4** | **0** |
| `$E90000`–`$E97FFF` | `Opm` / `Adpcm` / `Fdc` / `Hdc` | 0 | 2 | **2** | **4** | **7** | **0** |
| `$E98000`–`$E99FFF` | `Scc` (Z8530) | 0 | 6 | **6** | **10** | **17** | **0** |
| `$E9A000`–`$EAFFFF` | `Ppi` / `Ioi` / `ExpansionIo` | 0 | 2 | **2** | **4** | **7** | **0** |
| `$EB0000`–`$EBFFFF` | `SpriteBg` (CYNTHIA) | 0 | 1 | **1** | **2** | **5** | **0** |
| `$ED0000`–`$EDFFFF` | `Sram` (16–64 KB) | 1 | 0 | **1** | **1** | **4** | **0** |
| `$F00000`–`$FFFFFF` | `Rom` (CGROM / IPLROM) | 1 | 0 | **1** | **1** | **4** | **0** |

### 3.4 O(1) 4 KB Page Wait Table (`WaitStateTable`)
The 16 MB 24-bit address space is divided into 4,096 pages of 4 KB (`PAGE_SHIFT_4K = 12`). `WaitStateTable` precomputes a 4,096-byte array (`[u8; 4096]`) that fits entirely within L1 data cache, resolving `wait_for_addr(addr)` with a single shift and array index:

```rust
let page = ((addr & 0x00FF_FFFF) >> 12) as usize;
self.pages[page] as u32
```

### 3.5 Burst Execution and Guest Time
In the desktop app, Burst has no instruction limit. Each host frame runs guest code on the main thread until a deadline just before rendering, so throughput is whatever the host can deliver.

- **`FrameBudget::RealTime`**, from power-on: every 256 instructions, peripherals advance by the host time that has passed, so guest timers keep their real rates at any CPU speed. Time spent rendering is made up gradually, at most 2,000 ticks per check, and a backlog above 100 ms is dropped.
- Burst speeds up the CPU only. Peripherals never run ahead of host time, not even to skip boot-time timer waits: a title screen is meant to stay up for its time, and a fast-forward that ended when the desktop was recognized left every guest timer running with the CPU (double clicks, game speed) whenever the recognition failed.

---

## 4. Mixed Mode & Trap Interception (`TrapHandler`)

`msx68k-cpu` provides the `TrapHandler` trait to bridge 68000 code and native Rust/ARM code without traversing 68000 exception vectors in memory:

- **`on_a_line(opcode, pc)`**: Intercepts `$Axxx` instructions for Mixed Mode native routine descriptors.
- **`on_f_line(opcode, pc)`**: Intercepts `$Fxxx` instructions for Human68k DOS calls (`$FFxx`), FE-pack floating-point calls (`$FExx`), and SX-Window Drawing Manager (DM) traps.
- **`on_trap(trap_no, pc)`**: Intercepts `TRAP #0..#15` calls.

Each hook returns either `TrapResult::Handled { extra_cycles }` (continuing execution directly at the updated `PC`) or `TrapResult::PassToVector` (falling back to standard 68000 exception vector processing).

---

## 5. On-Demand Resource Allocation & VM Isolation Detection

To allow isolated VMs and console processes to spawn in microseconds:
- `X68kBus` allocates only `MainRam`, `Sram`, and `Rom` upfront; `Gvram` (512 KB), `Tvram` (512 KB), `SpriteBg` (64 KB), and `io_space` (128 KB) are initialized to `None` and allocated on first write.
- Accesses to unmapped Main RAM (`main_ram.len()..$C00000`) or unpopulated Expansion I/O (`$EA0000`, `$EC0000`) raise hardware `BusFault::BusError` (Vector 2), allowing unmodified Human68k/IPLROM startup probes to detect installed RAM size and expansion boards accurately.
- User mode raises `BusFault::BusError` for everything from `$C00000` up (GVRAM, TVRAM, all I/O, SRAM, and ROM) and for Main RAM below the area set boundary (`($E86001 + 1) * 8192`, `$2000` after reset).
- `HardwareAccessFlags` tracks accessed subsystems so the hypervisor can automatically detach a console process into an isolated display VM when it touches `Gvram`, `Tvram`, `Crtc`, `PaletteVicon`, or `SpriteBg` (`requires_isolated_display_vm() == true`), while keeping resident sound drivers (`Opm`, `Adpcm`) inside the shared Master VM.

---

## 6. Compatibility Verification (`SingleStepTests/m68000`)

The MC68000 core is validated against the [SingleStepTests/m68000](https://github.com/SingleStepTests/m68000) suite (checked out as a submodule under `third_party/m68000`).

- **Test Harness**: `crates/msx68k-cpu/tests/single_step_tests.rs` parses the binary `.json.bin` test vectors directly in Rust.
- **Coverage**: **127 / 127 instruction files (261,894 test vectors)** pass 100%, verifying `D0–D7`, `A0–A7`, `USP`, `SSP`, `SR` (all CCR and supervisor flags), `PC`, and modified RAM contents.
- **Verified Microcode Edge Cases**:
  - `CHK.W <ea>, Dn`: Clears `Z, V, C` (setting `Z` if `Dn.W == 0`) and sets `N = (Dn.W < 0)` even when no trap occurs (`0 <= Dn.W <= bound`).
  - `DIVU.W` / `DIVS.W`: Sets undocumented `N = 1` alongside `V = 1` (`Z = 0, C = 0`) on 16-bit quotient overflow.
  - `SBCD`: Computes high-nibble decimal correction (`-$60`) strictly from the 9-bit binary subtraction borrow (`dst < src + X`), while asserting `C = 1, X = 1` if the low-nibble `-$06` adjustment subsequently underflows.

---

## 7. MC68030 Emulation

Selected per VM with `VmConfig::with_cpu_model(CpuModel::Mc68030)`, or `--cpu 68030` in the desktop app, which then loads `roms/IPLROM30.DAT` and runs at 25 MHz unless `--ipl` or `--speed` say otherwise. It runs cycle-accurately at `CpuSpeedMode::Mhz25` (Section 7.4) or in Burst.

Human68k 3.02 and SX-Window 3.1 boot to the desktop on it (`crates/msx68k-bus/tests/boot_030.rs`, which also checks that the desktops match the MC68000 one byte for byte):
- at 25 MHz on the X68030 IPL ROM 1.3, in about 13 s of emulated time (disk I/O itself is instant),
- in Burst on the X68000 IPL ROM 1.0, as on an X68000 with an accelerator board.

### 7.1 What the Core Implements

| Area | MC68030 behavior |
| :--- | :--- |
| Instructions | 68020 additions: `Bcc.L`, `BFxxx`, `CAS`, `CAS2`, `CHK.L`, `CHK2`, `CMP2`, `DIVx.L`, `EXTB.L`, `LINK.L`, `MULx.L`, `PACK`, `UNPK`, `RTD`, `TRAPcc`, `MOVE from CCR`, `MOVEC`, `MOVES`. `BKPT` and the 68020-only `CALLM`/`RTM` raise an illegal instruction exception. `CLR` writes without reading its operand first, unlike the MC68000. |
| MMU instructions | `PMOVE` to and from `TC`, `TT0`, `TT1`, `CRP`, `SRP`, and `MMUSR`, `PFLUSH`, `PLOAD`, and `PTEST` run and keep the registers (`CpuState::mmu`), but addresses are not translated and `PTEST` reports every address as invalid. |
| Addressing | Scaled index, and the full extension word format (base and index suppression, 16/32-bit displacements, pre- and post-indexed memory indirection). |
| Memory access | 32-bit addresses reach the bus. Misaligned word and long data accesses are split into byte cycles. Only instruction fetches from odd addresses raise an address error. |
| Privilege | `MOVE from SR` is privileged. `S` and `M` select `USP`, `ISP`, or `MSP`. |
| Exceptions | Vectors are read relative to `VBR`. Frames carry the format/vector word: format `$0` normally, `$2` (with the instruction address) for `CHK`, `CHK2`, `TRAPcc`, `TRAPV`, divide by zero, and trace, `$1` throwaway frames for interrupts taken with `M` set, and format `$B` for bus and address errors. `RTE` understands formats `$0`, `$1`, `$2`, `$9`, `$A`, and `$B`, and raises a format error for the rest. |
| Control registers | `CACR` enables, freezes, and clears the cache tags used for timing (Section 7.4). Cache contents are not kept, so self-modifying code always sees memory. |

Not implemented:
- **MMU translation.** IPL ROM 1.3 tests the MMU by enabling a translation table that maps `$200000` onto `$F00000` and comparing the two. Without translation the test fails and it finds no MMU, which matches the X68030: it ships with an MC68EC030, and only unofficial modifications for NetBSD fit a full MC68030.
- **FPU.** Coprocessor instructions raise the F-line exception, as on an X68030 without an MC68882. `FLOAT*.X` keeps working through the `$FExx` calls.
- **Format `$A`/`$B` restart.** `RTE` from a bus fault frame reruns the faulted instruction from the start rather than resuming the faulted bus cycle. Side effects of the first attempt, such as a postincrement, are not undone. Handlers that rewrite the stacked `PC`, as the IPL ROM probes do, work.
- `T0` (trace on change of flow) is stored but never traces.

### 7.2 ROMs

- The IPL ROMs come from Sharp's free release, distributed by X68000 LIBRARY (`retropc.net/x68000/software/sharp/x68bios/`): `IPLROM.DAT` (EXPERT, 1.0), `IPLROMXV.DAT` (XVI, 1.1), `IPLROMCO.DAT` (XVI Compact, 1.2), and `IPLROM30.DAT` (X68030, 1.3, the 128 KB at `$FE0000` only). They are kept out of the repository in `roms/`.
- The bundled licence forbids modifying them, so msx68k never patches IPL ROM bytes. Every hook stays outside the ROM: the virtual SCSI ROM at `$FC0000`, F-line hypercalls, and native IOCS calls in `BusTrapHandler::on_trap`. Analysing them is fine, since the ROMs can also be read on real hardware.
- The virtual SCSI ROM stands in for the X68030's internal SCSI ROM unchanged.
- At reset the hardware also shows the ROM at `$000000` until the IPL releases it. The bus only provides the reset `SSP` and `PC` from the ROM.
- CGROM is not part of the free release. The X68030 CGROM is not available yet, so MC68030 VMs use the same `CGROM.DAT` as MC68000 ones.

### 7.3 Firmware Gaps Covered Natively

- **IOCS `$AC` `_SYS_STAT`.** Once Human68k 3.02 detects an MC68030 (it writes `3` to `$0CBC` itself), it calls `_SYS_STAT` for cache control, which only IPL ROM 1.3 has. On IPL ROM 1.0, Human68k stops at an Abort/Retry/Ignore prompt. `BusTrapHandler::on_trap` answers it natively on MC68030 VMs only while its IOCS table entry is the ROM's handler for undefined calls, shared with `$AB`, `$AD`, or `$B0`. IPL ROM 1.3 and drivers that install their own keep handling it. The native answer reports a 25 MHz MC68030 without FPU or MMU, and maps the cache state to `CACR`.

### 7.4 Cycle-Accurate Timing (`Tracked030Policy`)

`crates/msx68k-cpu/src/timing030.rs` follows Section 11 of the MC68030 User's Manual (second edition, 1989). That is the model Motorola gives for predicting MC68030 timing; the MC68030's actual overlap of bus and microcode activity is not documented beyond it.

- **Parts.** Each instruction is split into an effective address part (`fea`, `fiea`, `cea`, `ciea`, or `jea`, Sections 11.6.1 to 11.6.5) and an operation part (Sections 11.6.6 to 11.6.18), each with a head, a tail, an instruction-cache case time (CC), and a no-cache case time (NCC). The core records what each instruction did in a `Trace` (effective address modes, branch outcome, exceptions, register count, bit field width, `RTE` frame formats), and `Timing030::evaluate` picks the rows.
- **Overlap.** A part takes `CC - min(head, previous tail)` (equation 11-2). The tail carries over between instructions.
- **Caches.** 256-byte instruction and data caches are modelled as tags only (16 lines of four long words, tagged with the address and `FC2`). `CACR` enables, freezes, clears, and sets write allocation as on the MC68030. The cache holding register serves sequential prefetches from the same long word.
- **Cache misses.** An instruction whose prefetches all hit uses CC with overlap. One that misses uses NCC without overlap, plus the wait states of each bus cycle (Section 11.5).
- **Data cache hits and waits.** The rules of Sections 11.4 and 11.5 apply: a hit shortens the fetch part, and wait states lengthen CC and, for operand fetches and writes, the tail. `MOVEM` uses its own footnote formulas.
- **Bus waits.** `Bus::bus_cycle_030` gives the wait states, port width, and cacheability of an address. On the X68030 (`MemoryRegion::x68030_bus_cycle`):

| Region | Waits (25 MHz clocks) | Port | Cached |
| :--- | :---: | :---: | :---: |
| Main RAM | `$E8E009` low nibble `n`: `n + 2`, or 0 for static column (`n = 0`), plus 1/8 for DRAM refresh | 32-bit | Yes |
| ROM | `$E8E009` high nibble `n`: `n + 2` | 32-bit | Yes |
| SRAM | 2 | 16-bit | No |
| System port, printer, area set | 4 | 16-bit | No |
| GVRAM, CRTC, video controller, sprites | 6 | 16-bit | No |
| TVRAM | 7 | 16-bit | No |
| Palette | 11 | 16-bit | No |
| OPM, ADPCM, FDC, SCSI, PPI, I/O controller, expansion | 15 | 16-bit | No |
| MFP | 19 | 16-bit | No |
| RTC | 28 | 16-bit | No |
| DMAC | 34 | 16-bit | No |
| SCC | 38 | 16-bit | No |

  A long word on a 16-bit port takes two bus cycles. The refresh delay carries its fraction over, adding a clock every eighth Main RAM cycle. Until the IPL ROM writes `$E8E009`, ROM and RAM have no waits. `$E8E00B` reads `$DC` (MC68030 at 25 MHz) on MC68030 VMs.
- **Instruction timing.** The MC68030 User's Manual tables assume 2-clock bus cycles without waits. The timing model adds the configured memory and peripheral waits separately.
- **Peripheral time.** Tracked VMs carry the fraction of a 10 MHz tick over between instructions, since an MC68030 instruction often takes less than one.

The manual's worked examples (Sections 11.3.4, 11.4, and 11.5) are unit tests, and the core reproduces the 40-clock example end to end (`crates/msx68k-cpu/tests/mc68030.rs`).

Approximations:
- The wait of an instruction's operand reads and of its writes is taken from its first read and first write, multiplied by the table's access counts. Memory-indirect pointer fetches use their own wait.
- Read-modify-write cycles (`TAS`, `CAS`, `CAS2`) may hit the data cache, as in the manual's examples, although the MC68030 forces them to miss.
- The full extension word rows use the general `B` (base) forms; the faster `(d16,An)` forms of the tables are not told apart.
- No table covers `NBCD` on memory, the MMU instructions, or a divide-by-zero trap; they are timed like `NEG` on memory plus two clocks, `MOVEC` to `CACR`, and a `TRAPV` trap.
- The manual's register example times `SUBA.L` with the `SUBA.W` row; the table values are used.
- Burst fills (`IBE`, `DBE`) are not modelled.
- DMA transfers complete at once. When they get timing, the X68030's DMAC runs at 12.5 MHz instead of 10 MHz.
- The X68030 wait states and port widths still need validation against real hardware.

### 7.5 Remaining Plan

- **Tuning against real hardware.** Measure instruction loops and I/O accesses on an X68030 and adjust the wait table and any approximations above.
- **X68030 CGROM** and any other per-model ROM differences.
- **Address map.** The guest now sees 32-bit addresses. X68030 high memory at `$0100_0000` and the reserved extension range at `$F000_0000`..`$FEFF_FFFF` ([NATIVE_DRAWING.md](NATIVE_DRAWING.md) Section 2.5) are reachable as such, but everything else still mirrors the 24-bit space as on the 68000.
- **MMU translation** (low priority). Only unofficial MC68030 upgrades for NetBSD have an MMU.
