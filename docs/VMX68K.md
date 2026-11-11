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
the host cannot make that happen sooner. So VMX68K.SX polls. On every pass of its event loop it
asks the host for pending requests with a hypercall, carries them out, and returns the results.
Its loop waits for events in a way that also returns when there are none, so it keeps polling
while idle.

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
- The VICON compositor (`crates/msx68k-vicon`) currently uploads the whole TVRAM, GVRAM, and
  palette every frame, and draws the host cursor into its TVRAM copy; both change with Section
  3.3.

---

## 6. Open Questions

- **Bridge hook point.** DOS calls (`$FFxx`) directly, or a remote drive device driver in the VM's
  Human68k.
- **Mouse input to VMs**: relative or absolute motion, pointer capture, and how to leave it.
- **Settings file format** and how double-clicking a settings file opens VMX68K.SX with it.
- **Display scaling**: how a VM screen is scaled, once windows go beyond native size.
- **The special hook** for booting in the master VM's environment.
- **Startup speed** of a VM, and how several VMs and their sound are scheduled and mixed.
