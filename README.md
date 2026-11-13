# msx68k

**msx68k** is a hybrid Sharp X68000 / SX-Window × ARM execution environment written in Rust and `wgpu`.

Rather than stopping at traditional cycle-accurate hardware emulation, **msx68k** modernizes the classic X68000 GUI environment (**SX-Window**) into a high-resolution (1080p / 4K), TrueColor desktop while preserving 100% compatibility with legacy 68000 games, demos, and tools through on-demand isolated virtual machines and a PowerMac-style (`68k` ⇄ `ARM`) Mixed Mode architecture.

---

## 1. Core Architecture & Design Principles

### 1.1 Dual Execution Model: Shared Primary SX-Window + Isolated VMs
- **Primary SX-Window Environment**: Well-behaved SX-Window applications (`.SX`), command-line tools (`HAS.X`, `GCC`, `LHA.X`), and shared resident drivers (`MXDRV.X`, `ZMUSIC.X`) run cooperatively inside a single shared SX-Window environment at native host speeds (`burst` mode).
- **On-Demand Isolated VMs**: Applications that directly manipulate CRTC registers, GVRAM/TVRAM, or hardware sprites—or trigger a transition to full-screen Human68k mode (`_CRTMOD`)—are seamlessly spun out into dedicated, cycle-accurate X68000 virtual machine instances rendered as individual windows on the SX-Window desktop (or native host OS windows in Rootless mode).

### 1.2 Breaking the 1024×1024 TVRAM Barrier
Original SX-Window renders its entire GUI into the 512 KB Text VRAM (`$E00000`, 1024×1024, 4-bit 16-color planes) using hardcoded 128-byte stride math (`Y * 128 + (X >> 3)`) and CRTC hardware raster copy/bitmask registers.
- Higher-level SX-Window managers (Window Manager, Menu Manager, and application `RastPort` / `BitMap` APIs) already operate on signed 16-bit coordinates (`-32768` to `+32767`).
- By hooking the low-level Drawing Manager (DM) primitives via A-Line/F-Line traps and redirecting rendering to native `wgpu` surfaces, **msx68k** removes the 1024×1024 / 16-color hardware ceiling, enabling Full HD (1920×1080) and 4K (3840×2160) TrueColor SX-Window desktops.
- Current status and plans: [docs/NATIVE_DRAWING.md](docs/NATIVE_DRAWING.md) for drawing and [docs/NATIVE_FONTS.md](docs/NATIVE_FONTS.md) for text and fonts, and [docs/VMX68K.md](docs/VMX68K.md) for X68000 VMs shown as SX-Window applications.
- Test commands and integration-test policy: [docs/TESTING.md](docs/TESTING.md).
- Bugs found but not yet fixed: [docs/KNOWN_BUGS.md](docs/KNOWN_BUGS.md).

### 1.3 Monomorphized 4-Mode MC68000 Core, Bus Timing & Mixed Mode Bridge
The MC68000 CPU engine uses Rust trait monomorphization (`CpuPolicy`) and a region-aware system bus timing model so cycle-accounting and wait-state overhead completely vanish in `burst` mode:

| Mode | CPU Clock | System Bus Clock | Target Profile & Characteristics |
| :--- | :--- | :--- | :--- |
| `68000_10mhz` | 10 MHz | 10 MHz | **Original X68000 / ACE / PRO / SUPER (Default for App VMs)**. Full cycle accuracy with 10 MHz system bus, per-region device wait states (GVRAM, TVRAM, Palette, MFP, SCC, DMAC, OPM, ROM/SRAM), and VICON display contention. |
| `68000_16mhz` | 16 MHz | 16 MHz | **X68000 XVI**. 0-wait 16 MHz Main RAM and 16 MHz system bus with asymmetric synchronization waits when accessing 10 MHz video/peripheral domains. |
| `68000_24mhz` | 24 MHz | 16 MHz | **X68000 XVI RedZone**. 24 MHz CPU coupled to a 16 MHz system bus (+2 CPU wait states per 16-bit system bus transaction, plus scaled peripheral/VRAM wait states). |
| `burst` | Uncapped | Zero Delay | **Primary SX-Window VM Default**. Zero wait-state/cycle accounting overhead (`TRACK_CYCLES = false`). Hardware timers (MFP, OPM, V-Blank/H-Blank) remain strictly locked to wall-clock time. |

- **Per-Application Speed Selection**: The Primary SX-Window VM always runs in `burst` mode, while isolated application VMs default to `68000_10mhz` and can be configured per application (`10MHz`, `16MHz`, `24MHz`, `burst`, or custom CPU/system bus clock pairs). See [docs/CPU_AND_BUS.md](docs/CPU_AND_BUS.md) for the detailed CPU core and bus wait-state specification.

- **Mixed Mode (`68k` ⇄ `ARM`)**: A-Line (`$Axxx`) APIs and one F-Line hypercall opcode (`$FBFF`, recognized by the address it runs at) trap directly into the Rust/ARM native dispatcher without traversing 68k exception vectors, enabling 4-byte native stubs (`[$FBFF, $4E75]`), relocation-aware module patching (`SYS/FSX.X`), and FAT `.X` / `.SX` binaries. See [docs/HOOK_AND_SCSI_ARCHITECTURE.md](docs/HOOK_AND_SCSI_ARCHITECTURE.md) for the Mixed Mode hook architecture and Virtual SCSI (`IOCS $F5`) specification, and [docs/SX_WINDOW_API_AND_HOOK_STATUS.md](docs/SX_WINDOW_API_AND_HOOK_STATUS.md) for the complete SX-Window v3.1 A-Line API catalog (1,050 calls) and Primary Hook Tracking Reference.

### 1.4 `wgpu` VICON Shader Pipeline
For isolated VMs and legacy fallback layers, pixel-by-pixel software compositing is replaced by a single-pass WGSL shader pipeline (`crates/msx68k-vicon`):
- Packed GVRAM (512 KiB), TVRAM, palettes, and CRTC/VICON registers feed the GPU compositor; VM screens upload only dirty VRAM pages.
- The shared WGSL pipeline supports 4-plane TVRAM, 512-dot 16/256/65536-color graphics and 1024-dot 16-color graphics, page selection and scrolling. Scanline register/palette effects share the same shader; VRAM write history and Sprite/PCG/BG rendering are planned; see [docs/GRAPHICS.md](docs/GRAPHICS.md) for scope and implementation order. See [docs/FD_BOOT_AND_VICON.md](docs/FD_BOOT_AND_VICON.md) for the FDC/DMAC/MFP floppy boot subsystem, CRTC/TVRAM hardware acceleration, and `wgpu` VICON pipeline details.

### 1.5 Lazy I/O Instantiation & 3-Tier Straight Storage Bridge
- **Zero-Device Fast VM Spawn**: VMs boot in microseconds via a custom IPL with all peripheral slots initialized to `None`. Chips (CRTC, OPM, ADPCM, MFP, SCC, DMAC, Sprite) and their cycle timers are instantiated only when their memory-mapped I/O ports (`$E80000`–`$EFFFFF`) are first accessed.
- **3-Tier Straight Storage Bridge**: To prevent FAT/directory cache corruption across concurrent VMs without fragile cross-layer translation, disk operations from sub-VMs are forwarded at their exact abstraction level—(1) Human68k DOS calls (`$FFxx`), (2) Disk IOCS (`$40`–`$4F`), or (3) SCSI IOCS (`$F5` `_SCSIDRV`)—directly to the Primary SX-Window VM's corresponding layer with zero-copy DMA buffer mapping.

### 1.6 Audio Sharing & Selective Native TSRs
- **Audio Sharing in the Primary VM**: Sound I/O accesses (`OPM`, `ADPCM`, `MIDI`) do not trigger VM isolation, allowing resident music drivers (`MXDRV.X`, `ZMUSIC.X`, `MCDRV.X`) to remain as authentic 68000 binaries inside the Primary VM (preserving undocumented work-area compatibility with external spectrum analyzers and players).
- **High-Definition Native TSRs**: Pure compute or hardware-bottlenecked TSRs are replaced by 32-byte 68k header stubs backed by native Rust implementations:
  - `PCM8` / `PCM8A` / `PCM8PP`: Bypasses the 15.6 kHz 4-bit ADPCM bottleneck and streams 48/96 kHz 32-bit float multi-channel audio directly to the host mixer while keeping 68k status work areas synchronized.
  - `FLOAT2.X`–`FLOAT4.X`: Routes `$FExx` floating-point traps directly to ARM hardware FPU/NEON instructions.
  - `TwentyOne.X`: Native long-filename and case-preservation support bridged to the host file system.

---

## 2. Quick Start

Place the X68000 ROM images in `roms/` (`roms/IPLROM.DAT`, `roms/CGROM.DAT`) and floppy disk images (`.XDF` / `.2HD` / `.DIM`) in `disks/`:

```bash
# Boot all-in-one SX-Window v3.1 from Virtual SCSI HDD (auto-builds disks/SXWIN31.HDS on first run):
cargo run --release -- --hdd0 disks/SXWIN31.HDS --speed burst

# Boot Human68k v3.02 from floppy disk:
cargo run --release -- --fd0 disks/HUMAN302.XDF

# Boot SX-Window v3.1 from floppy disks (System Disk in FD0, Accessory Disk in FD1) in Burst mode:
cargo run --release -- --fd0 disks/SXWIN311.XDF --fd1 disks/SXWIN315.XDF --speed burst
```

### Debugging Guest Code with GDB
Pass `--gdb <port>` to serve the GDB remote protocol on `127.0.0.1:<port>`. Without this option no socket is opened. The VM halts when a client connects and resumes when it detaches.

```bash
cargo run --release -- --hdd0 disks/SXWIN31.HDS --gdb 1234
gdb-multiarch -ex 'set endian big' -ex 'set architecture m68k:68000' -ex 'target remote :1234'
```

`set endian big` is required because GDB has no executable to learn the byte order from. Memory reads cover RAM, VRAM, SRAM, and ROM. I/O registers are not readable, so inspecting them cannot disturb peripherals.

### Runtime Window Controls (`msx68k-app`)
- **Mouse**: 1:1 pixel-exact synchronization between the host OS cursor and the SX-Window cursor via the `SYS/FSX.X` `SxMouseSync` Mixed Mode hook (with automatic fallback to relative `Z8530 SCC` Channel B movement for non-SX-Window software). Hold the Right mouse button in SX-Window to display the top menu bar.
- **Keyboard**: Forwarded as X68000 hardware scan codes via `MC68901 MFP` USART.
- **Hotkeys**:
  - `F1`: Switch to `10MHz` mode (`CPU 10MHz / Bus 10MHz`)
  - `F2`: Switch to `16MHz` mode (`XVI: CPU 16MHz / Bus 16MHz`)
  - `F3`: Switch to `24MHz` mode (`RedZone: CPU 24MHz / Bus 16MHz`)
  - `F4`: Switch to `Burst` mode (`0 wait`)
  - `F12`: Soft Reset X68000 VM

---

## 3. Target Deployment Modes

1. **ARM SBC Appliance Mode (Bare-Metal Experience)**
   - Runs as `PID 1` on minimal Linux (direct DRM/KMS + Vulkan via `wgpu`, no X11/Wayland/systemd) on Raspberry Pi 5 or RK3588 SBCs, booting straight into the high-resolution SX-Window desktop in seconds.
2. **Desktop OS Integration (Linux / macOS / Windows)**
   - **In-Screen Mode**: Renders the full high-resolution SX-Window desktop inside a single host window or fullscreen display.
   - **Seamless (Rootless) Mode**: Uses `winit` + `wgpu` multi-surface rendering to map individual SX-Window applications and isolated X68000 VMs directly to native host OS windows.

---

## 4. Development Roadmap

- **Phase 1: Core Emulation & `wgpu` VICON Pipeline**
  - Monomorphized 68000/68030 CPU core (`10MHz`, `16MHz`, `030`, `burst`) and lazy on-demand I/O bus.
  - WGSL VICON/CYNTHIA compositor (GVRAM, TVRAM, Sprite/BG, per-scanline raster state).
  - Custom IPL and single-VM boot of unmodified Human68k and SX-Window.
- **Phase 2: TVRAM-Free Native SX-Window Drawing & Native TSR Stubs**
  - A-Line/F-Line Drawing Manager (DM) interception backed by `wgpu` surfaces to unlock 1080p/4K TrueColor SX-Window desktops.
  - Native TSR stubs for `FLOATx`, high-definition `PCM8`, and `TwentyOne`.
- **Phase 3: Multi-VM Isolation & 3-Tier Storage Bridge**
  - Automatic VM detachment on direct CRTC/GVRAM access and `_CRTMOD` transitions.
  - 3-tier straight storage forwarding (DOS / Disk IOCS / SCSI IOCS) and multi-VM audio mixer.
  - Per-executable compatibility profile database (BLAKE3 hash matching).
- **Phase 4: Native OS Core Migration & FAT Binary SDK**
  - Full ARM native replacement of SX-Window window/event management.
  - `.X` / `.SX` FAT binary toolchain and Virtual Bus API for native hardware extensions.
