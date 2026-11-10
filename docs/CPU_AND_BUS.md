# MC68000 CPU Core & X68000 System Bus Architecture

This document describes the design and implementation of the `msx68k-cpu` and `msx68k-bus` crates, covering the monomorphized MC68000 instruction engine, the CPU/System Bus wait-state model across `10MHz`, `16MHz`, `24MHz`, and `Burst` modes, the Mixed Mode trap interface, and hardware compatibility verification.

---

## 1. Crate Structure

| Crate | Path | Responsibility |
| :--- | :--- | :--- |
| **`msx68k-cpu`** | `crates/msx68k-cpu` | Monomorphized MC68000 CPU core (`CpuCore<P>`), architectural register state (`CpuState`), ALU/BCD/bit/shift engine, exception handling, and A-Line/F-Line/`TRAP` hooks (`TrapHandler`). |
| **`msx68k-bus`** | `crates/msx68k-bus` | X68000 24-bit physical address map (`MemoryRegion`), CPU & System Bus timing engine (`BusTimingConfig`, `WaitStateTable`), lazy on-demand VRAM/I/O allocation (`X68kBus`), and unified VM runner (`X68kVm`). |

---

## 2. Monomorphized Execution Policies (`CpuPolicy`)

A central requirement of **msx68k** is that cycle-accurate bus contention and wait-state calculations for legacy games must impose **zero runtime overhead** on the Master SX-Window environment running in `Burst` mode.

### 2.1 Compile-Time Specialization
`CpuCore<P: CpuPolicy>` is parameterized over the `CpuPolicy` trait (`crates/msx68k-cpu/src/policy.rs`):

```rust
pub trait CpuPolicy: Copy + Clone + Default + Send + Sync + 'static {
    const TRACK_CYCLES: bool;
}
```

- **`TrackedPolicy` (`TRACK_CYCLES = true`)**:
  - Used for `10MHz`, `16MHz` (XVI), `24MHz` (XVI RedZone), and custom cycle-accurate modes.
  - Accumulates standard Motorola MC68000 base instruction cycles plus per-transfer bus wait states (`Bus::wait_cycles_16`) on every instruction fetch, extension word fetch, and effective address read/write.
  - 32-bit (`Long`) memory accesses are split into two 16-bit bus transfers (`addr` and `addr + 2`), accumulating wait states for both words.
- **`BurstPolicy` (`TRACK_CYCLES = false`)**:
  - Used for the Master SX-Window VM and temporary fast-load boosts (`burst_on_disk_io`).
  - Because `P::TRACK_CYCLES` is a compile-time constant, Rust's dead-code elimination removes all cycle additions and `wait_cycles_16` lookups from the generated machine code.

### 2.2 State Decoupling for Zero-Copy Mode Switching
Architectural state (`CpuState`: `D0–D7`, `A0–A7`, `PC`, `IR`, `SR`, `USP`, `SSP`, interrupt lines, and `total_cycles`) is stored independently of the `CpuPolicy` type parameter. `X68kVm` can switch between `CpuCore<TrackedPolicy>` and `CpuCore<BurstPolicy>` on any instruction boundary (e.g., when entering or leaving disk I/O or changing application speed profiles) without copying registers or memory.

---

## 3. CPU Clock, System Bus Speed & Memory Region Timing

On real X68000 hardware, instruction execution speed is determined not only by the CPU clock frequency, but also by the **System Bus frequency** and the **response latency (`DTACK`) of the accessed memory or peripheral region**.

### 3.1 VM Roles and Speed Presets (`CpuSpeedMode`)

| Mode | CPU Clock | System Bus | Default Role / Target Profile |
| :--- | :--- | :--- | :--- |
| **`CpuSpeedMode::Mhz10`** | `10 MHz` | `10 MHz` | **Default for Isolated App VMs (`VmRole::AppVm`)**. Models original X68000 / ACE / PRO / SUPER timing. |
| **`CpuSpeedMode::Mhz16`** | `16 MHz` | `16 MHz` | **X68000 XVI**. Main RAM and the system bus run at 16 MHz (`0` wait on Main RAM), while 10 MHz video/peripheral accesses incur asymmetric synchronization waits. |
| **`CpuSpeedMode::Mhz24`** | `24 MHz` | `16 MHz` | **X68000 XVI RedZone**. A 24 MHz CPU coupled to a 16 MHz system bus (`+2` CPU wait cycles per 16-bit system bus transfer, plus scaled peripheral/VRAM wait cycles). |
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

- **`FrameBudget::RealTime`** (SX-Window desktop): every 256 instructions, peripherals advance by the host time that has passed, so guest timers keep their real rates at any CPU speed. Time spent rendering is made up gradually, at most 2,000 ticks per check, and a backlog above 100 ms is dropped.
- **`FrameBudget::FastForward`** (boot, before the desktop polls the mouse): peripherals advance a fixed 8 ticks per instruction, so timer waits finish faster than real time.

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
