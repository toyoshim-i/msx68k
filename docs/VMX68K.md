# VMX68K: X68000 VMs as SX-Window Applications

VMX68K runs software that SX-Window cannot host: programs that drive the graphics
hardware, change screen modes, or expect a plain Human68k console. Such programs
start in an isolated VM rather than being detached after accessing unsupported
hardware in the master VM.

Only decisions are recorded as the plan. Proposals not yet decided are listed in Section 6.

---

## 1. Why Not Detach Programs from the Master VM

`コンソール.X` hooks standard input and output, starts `COMMAND.X`, and runs programs from it, all
inside the master VM. Moving such a program to its own VM once it touches unsupported hardware is
impractical: most of what it shares with the master VM (the SX-Window kernel, its heap, other
tasks, Human68k's state) must not be shown to the new VM. So programs that need a real X68000
run in a VM of their own from the start.

---

## 2. Decisions

- **VMX68K.SX.** An SX-Window application that shows a virtual X68000's screen as one of its
  windows. The virtual machine itself runs on a host thread of its own.
- **An ordinary emulator first.** The VM starts out close to a standalone X68000 emulator, and
  host bridges are added to it step by step. Booting Human68k depends strongly on its
  environment (the IPL on disk, `HUMAN.SYS`, `CONFIG.SYS`, `COMMAND.X`, `AUTOEXEC.BAT`): booting
  the master VM's own drive A would start SX-Window inside the VM again. Booting in the master's
  environment therefore needs a special hook, to be designed later.
- **Several boot environments.** Users keep any number of VM setups, and boot from disk images or
  physical disks.
- **Settings.** From the application's menus, graphically: drive assignments (host drives or
  virtual files such as disk images) and memory size, among others. A setup is saved as a settings
  file, and opening the file boots a VM with the same settings.
- **Disk images.** The first test boots `disks/HUMAN302.XDF` from the host directly. As soon as
  the VM shows its screen in the window, images come from the master VM's drives. Floppy and hard
  disk images are read and written on every access, written through, never cached whole: still
  orders of magnitude faster than real disks, and closer to what users expect.
- **Host bridges.** Later, the VM's IOCS and DOS calls are hooked and carried out as calls of the
  VMX68K.SX task in the master VM, so the master VM's Human68k stays the only owner of its file
  systems.
- **Drive bypass mode, the default.** A VM sees the master VM's storage as it is, with the same
  drive letters, bridged at the file level: each drive is a remote drive in the VM's Human68k,
  whose file operations VMX68K.SX carries out through the master VM's Human68k. Sector access
  to the master's disks is not given, since two Human68ks caching one file system would break
  it.
- **Booting from a remote drive.** A boot routine loads `HUMAN.SYS` through the bridge
  and makes the bridged drive A: the boot drive, without sector access.
- **A VM's own CONFIG.SYS.** A VM must not start SX-Window, which the master's `CONFIG.SYS`
  starts (`DEVICE = FSX.X`, `PROGRAM = TITLE.X`, `SHELL = SXWIN.X`). The bridge serves
  `\VMX68K\CONFIG.SYS` when the VM opens `\CONFIG.SYS` of its boot drive. `AUTOEXEC.BAT` is
  shared as it is, since it has no part in starting SX-Window.
- **Language.** VMX68K.SX is written in assembly, with the clean-room macros of `guest/include`
  and the in-process `rhas` / `rhlk` toolchain.
- **Devices.** The existing device emulation is enough for the first goal.

---

## 3. VM Threads, Disks, and Screens

The master VM and VMX68K.SX keep running on the main thread. Each VM runs on a host thread of its
own and owns its `X68kVm`, at the pace of real time for its speed mode.

### 3.1 Disk Requests
A disk request is one command at the layer the VM emulates, and its reply comes from VMX68K.SX,
which reads or writes the disk image through the master VM's DOS.

| Device | One request | While the reply is pending |
| :--- | :--- | :--- |
| Floppy disk | One FDC command. A read asks for every sector the command will transfer (from its C, H, R, N up to EOT) when the command is issued; a write sends each sector once the VM has written it. | The VM keeps running on its emulated timing, seek and rotation included, since programs play music while they load. It stops only when it reaches the moment the FDC hands a sector to the DMAC (or the result phase of a write) without the reply, and resumes with its time made consistent once the reply arrives. |
| SCSI | One IOCS `$F5` call, which msx68k already handles natively. | The VM stops until the reply arrives, so the call still completes at once as the guest sees it. |

### 3.2 Picking Up Requests
SX-Window multitasks cooperatively: VMX68K.SX runs only when the scheduler gives it a turn, and
the host cannot make that happen sooner. So the host sends VMX68K.SX an event when a VM has
requests, as if SX-Window delivered it: `E_USER1`, addressed to the VM's window. VMX68K.SX waits
for events as usual, and on `E_USER1` asks the host for the pending requests with a hypercall,
carries them out, and returns the results. It takes no turns while there is nothing to do.

### 3.3 Screens
All composition runs in GPU shaders, once per frame. Effects that change registers within a frame,
such as raster scrolling and raster palettes, reach the shader as a table of each scanline's
state (CRTC scroll, palette, priority), recorded as the scanlines are processed. VMs shown in
windows work the same way as the master VM.

- **Each VM thread composites its own screen.** VMs share the GPU device and queue, which `wgpu`
  allows across threads. Each VM has its own VRAM, palette, and scanline state buffers, its own
  compute pass, and two output textures, one being written while the other is complete; an
  atomic frame number tells which one is complete.
- **Only changed pages travel.** VRAM writes are recorded per 4 KB page. At each vertical blank
  of the VM, its thread uploads the changed pages, the scanline table, the palette, and the
  sprite, BG, and page registers straight to its own GPU buffers, and dispatches its compute
  pass. VRAM never leaves the CPU except for that one upload, and no VRAM is copied between
  threads. The master VM's screen moves to the same scheme instead of uploading every buffer
  every frame.
- **The final pass runs on the main thread.** It draws the master VM's screen, then each VM's
  completed texture into its window's content rectangle, clipped to the window's visible region
  as a list of scissor rectangles, then the cursor as a layer of its own.
- **The load is bounded.** A still screen uploads a few kilobytes per frame. Even when every VRAM
  page changes every frame, the upload cannot exceed the VRAM itself, about 1 MB per frame,
  whatever speed the VM runs at.
- Windows start at the VM's native size; scaling comes later.

### 3.4 Input
- While the VMX68K.SX window is active, the host keyboard goes to the VM unconditionally, as
  SX-Window gives input to the active window. It goes natively, straight into the VM's MFP,
  rather than through SX-Window events, so the VM sees physical key information.
- The mouse is left out of the first version: relative motion would need pointer capture and a
  way out of it, which few early uses need.
- For now the SX-Window cursor may go under VM windows. Over a VM's screen it reads as the
  pointer being captured by the VM, so no cursor layer is needed before VM windows appear.

### 3.5 VMX68K.SX and the Host
- **Calls.** VMX68K.SX reaches the host through msx68k's application entry
  ([HOOK_AND_SCSI_ARCHITECTURE.md](HOOK_AND_SCSI_ARCHITECTURE.md) Section 2.1): `VmOpen` (4)
  takes the window (`D1.L` = `WindowPtr`) and returns the VM's ID, `VmPoll` (5) is called on
  every pass of the event loop, and `VmClose` (6) stops the VM.
- **Window position and visible region.** VMX68K.SX does not report them. The host reads them
  from the window's `Graph` every frame: its rectangle, its bitmap's rectangle (which places it
  on the screen), and its visible region, which become the `VmLayer`'s position and scissor
  rectangles. So moving, covering, and uncovering the window need no code in VMX68K.SX.
- **Window size.** The window's content is the VM's screen at its native size, fixed for now
  (768×512 for Human68k 3.02 at boot).
- **First disk.** The host read the floppy image for drive 0 of new VMs itself at first, named by
  a command-line option of the app. Images come from the master VM's drives now that the VM's
  screen shows in its window (Section 2).
- **Drive 0's image, for now.** Until the settings GUI chooses images, VMX68K.SX serves the file
  at a fixed path, `A:\VMX68K\DRIVE0.XDF`, as the VM's drive 0.

## 4. First Goal

Boot Human68k 3.02 from `HUMAN302.XDF` as drive A in a VM, shown in a VMX68K.SX window. The
floppy boot itself already works (`crates/msx68k-bus/tests/fd_boot.rs`).

---

## 5. Known Facts

- `X68kVm` is `Send`, so a VM can move to and run on a thread of its own.
- `VmThread` (`crates/msx68k-bus/src/vm_thread.rs`) runs a VM on a thread of its own, one frame
  of guest time at a time, at real-time pace or unthrottled for tests. Other threads reach the
  VM only through requests it carries out between frames. On it, Human68k 3.02 boots from
  `disks/HUMAN302.XDF` to the `A>` prompt in about 10 s of guest time at 10 MHz
  (`crates/msx68k-vicon/tests/vm_thread_boot.rs`).
- The bus records the 4 KB TVRAM and GVRAM pages written by the CPU, raster copy, and graphic
  fast clear (`crates/msx68k-bus/src/vram_pages.rs`); direct host access through `tvram_mut`
  counts as writing all of TVRAM. The extended screen of the master VM is not recorded.
- `VmScreen` (`crates/msx68k-vicon/src/vm_screen.rs`) composites a VM's screen on its thread
  through a `VmThread` vertical blank hook, called after each frame of guest time in which the
  VM entered a vertical blank. It uploads the written pages, the palette, and the display
  registers, and dispatches into one of two 1024×1024 textures; `VmScreenOutput` gives the main
  thread the latest completed one. At Human68k's `A>` prompt a frame uploads about 2 KB, and
  10 KB when the cursor blinks (`crates/msx68k-vicon/tests/vm_screen.rs`).
- VMX68K.X opens `A:\VMX68K\DRIVE0.XDF` for reading and writing when it starts and passes its size
  to `VmOpen`. The app then gives the VM's drive 0 a remote disk (`RemoteDisk`), unless
  `--vm-fd0` names an image for the host to read itself. The FDC finishes a command at once, so
  the VM waits where the command runs: a read asks for every sector to EOT, and the other side
  for a multi-track command, in one request; a write sends the written sectors in one request
  before the result phase. VMX68K.X carries requests out one at a time with DOS `_SEEK`,
  `_READ`, and `_WRITE` on a 32 KB buffer (`VmPoll`, `VmReply`). Human68k 3.02 boots from the
  master VM's HDD to `A>` this way (`crates/msx68k-vicon/tests/vmx68k_disk.rs`).
- The host posts `E_USER1` when the guest is about to take an event (`EMGet`), into the Event
  Manager's queue the way `EMSet` would, with `whom` the VM's window, once per batch of requests
  until VMX68K.X polls. SX-Window's own mask (`$03BF`) leaves user events out, so the host posts
  them past it. The Task Manager hands the event to the window's task even behind another window.
  Booting Human68k takes about 290 wake-ups and 580 polls; with null events instead, VMX68K.X
  polled about 11,000 times.
- The compositor shader has no scanline table, sprites, or BG yet; it composites text and
  graphic page 0 from the state at the end of the frame.
- The final pass (`render_bus_to_surface_view`) draws the master VM's screen, then each
  `VmLayer` (a VM's latest completed texture at a position on the master VM's screen) once per
  rectangle of its visible region, as a scissor rectangle; rectangles are scaled with the
  master VM's screen and clipped to the target (`crates/msx68k-vicon/tests/vm_layer.rs`). The
  app passes no layers until VMX68K.SX reports its windows.
- The master VM's VICON compositor (`crates/msx68k-vicon`) still uploads the whole TVRAM, GVRAM,
  and palette every frame, and draws the host cursor into its TVRAM copy, so the cursor goes
  under VM layers (accepted for now, Section 3.4); both change with Section 3.3.

---

## 6. Open Questions

- **Mouse input to VMs**: relative or absolute motion, pointer capture, and how to leave it.
- **Settings file format** and how double-clicking a settings file opens VMX68K.SX with it.
- **Display scaling**: how a VM screen is scaled, once windows go beyond native size.
- **The special hook** for booting in the master VM's environment.
- **Startup speed** of a VM, and how several VMs and their sound are scheduled and mixed.
