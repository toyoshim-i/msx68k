# VMX68K: X68000 VMs as SX-Window Applications

VMX68K runs software that SX-Window cannot host: programs that drive the graphics
hardware, change screen modes, or expect a plain Human68k console. Such programs
start in an isolated VM rather than being detached after accessing unsupported
hardware in the primary VM.

Only decisions are recorded as the plan. Proposals not yet decided are listed in Section 6.

---

## 1. Why Not Detach Programs from the Primary VM

`コンソール.X` hooks standard input and output, starts `COMMAND.X`, and runs programs from it, all
inside the primary VM. Moving such a program to its own VM once it touches unsupported hardware is
impractical: most of what it shares with the primary VM (the SX-Window kernel, its heap, other
tasks, Human68k's state) must not be shown to the new VM. So programs that need a real X68000
run in a VM of their own from the start.

---

## 2. Decisions

- **VMX68K.SX.** An SX-Window application that shows a virtual X68000's screen as one of its
  windows. The virtual machine itself runs on a host thread of its own.
- **An ordinary emulator first.** The VM starts out close to a standalone X68000 emulator, and
  host bridges are added to it step by step. Booting Human68k depends strongly on its
  environment (the IPL on disk, `HUMAN.SYS`, `CONFIG.SYS`, `COMMAND.X`, `AUTOEXEC.BAT`): booting
  the primary VM's own drive A would start SX-Window inside the VM again. Booting in the primary's
  environment therefore needs a special hook, to be designed later.
- **Several boot environments.** Users keep any number of VM setups, and boot from disk images or
  physical disks.
- **Settings.** From the application's menus, graphically: drive assignments (host drives or
  virtual files such as disk images) and memory size, among others. A setup is saved as a settings
  file, and opening the file boots a VM with the same settings.
- **Settings menus.** VMX68K.SX's window has a bar of icons above the VM's screen, pictures
  rather than words, as in `シャーペン.X`, each in the style of SX-Window's icons: a picture in
  a rectangular recess, 3:2 wide. The right button on one pops up its menu, as SX-Window's
  menus follow the right button (`MNSelect`). Menus are in Japanese, as SX-Window is. The icons and the
  submenus are resources of `VMX68K.LB`, next to `VMX68K.X`, since the Menu Man takes
  submenus only as `'MENU'` resources of an open library. There is no
  Save button: what takes effect at once is applied and saved when chosen, and what needs a
  restart is changed in a dialog, saved when the dialog closes, and followed, if anything
  changed, by a dialog saying the settings take effect when the VM restarts and asking whether
  to restart now or later by hand.
  - **Configuration**: one icon whose menu has, in order: "設定..." to open the
    configuration dialog; the current CPU's name (such as 68000), a submenu of that CPU's
    speeds, applied and saved when chosen (for the 68000: 10MHz, 16MHz, 24MHz, and 最高速,
    msx68k's Burst); and, in custom mode only, "Boot", a submenu of the
    boot devices kept in the VM's SRAM (STD, FD, SCSI, ROM, as `SWITCH.X` sets them), written
    and saved when chosen and taking effect at the next reset, as on the real machine. In shared
    mode the boot device stays on msx68k's boot ROM and the item is not shown; the VM's
    CONFIG.SYS decides what it starts.
  - **The configuration dialog** sets the CPU's kind, the memory size with a slider, and drive
    assignment. It is a dialog of SX-Window's Dialog Man, `'DLOG'`/`'DITL'` resources of
    `VMX68K.LB` with SX-Window's own controls. The CPU is the 68000 (10, 16, 24 MHz and Burst)
    or the 68030 (25 MHz and Burst, booting from `IPLROM30.DAT`), and the menu's CPU name and
    speeds follow it; memory is 1 to 12 MB in 1 MB steps. 設定 saves the setup (`'V68S'` version 2: speed,
    CPU, RAM) and, when the CPU or memory changed, an alert (`DMError`, はい/いいえ) asks
    whether to restart the VM now; はい restarts it with `VmRestart` (11), which keeps its
    window, drives, and SRAM, and いいえ leaves the change for the VM's next start. Radio buttons
    are label-less switches, as in Sharp's dialogs, and static texts get their text with
    `DITSet` when the dialog opens. While it is open, its filter serves the VM's requests, so the
    VM does not wait for it.
  - **Drive assignment** (decided 2026-11-13, mock in the notes) is in one of two modes, chosen
    in the configuration dialog's ドライブ row: **共有** (shared mode, with the path of the
    CONFIG.SYS the VM boots with: every drive is the primary VM's, under the same letter) or
    **カスタム** (custom mode). Custom mode's 詳細... button, which shared mode greys out, opens
    the ドライブの詳細 dialog: the number of drives, and for each a kind, **ファイル** (a floppy or
    HDD image file, given by its path), **共有** (one of the primary VM's drives, file-level, as
    shared mode has them), or **パススルー** (one of the primary VM's floppy drives handed to the VM
    sector by sector, a real FD to it, write-protected on the primary meanwhile), and what it is
    given, a path or a drive letter such as `C:`. Its 設定 keeps the changes until the
    configuration dialog's 設定 saves them with the rest and asks about the restart once; that
    dialog's 取消 drops them too. Paths are typed into text fields; dropping a file from the
    Filer comes later. An HDD image goes on the VM's SCSI and is the VM's alone. Custom mode
    keeps the machine's own limits: two or four floppy drives, and the drive letters Human68k
    gives them. The VM boots as its SRAM says, and making a new setup (4d) chooses the boot
    device it writes there.
  - **Floppy drives**: in custom mode, an icon for each floppy drive shows whether a disk is in
    and has an access lamp. Clicking it ejects the disk, and dropping an image on it from the
    Filer swaps it in, both at once and without a restart. Shared mode, which leaves the VM's
    floppy drives unused, shows none.
  - **Access lamp**: one lamp lights whenever the VM reaches any of its drives, floppy, SCSI, or
    remote, as the front panel's lamp of the real machine does, so that access shows in shared
    mode too. It sits at the right of the bar, red while lit, and stays lit 0.2 s after an
    access, since emulated accesses are over at once. The app counts the VM's accesses (FDC
    reads, writes, and READ ID; SCSI reads and writes; remote drive requests) and sets the
    lamp on the primary, which wakes VMX68K.SX with `E_USER1` to read it with `VmStatus` (9).
- **A settings file per VM.** Each VM setup is one resource library (`.LB`, as `MSX68K.LB`)
  holding its settings and its own SRAM, kept apart from the primary's, named `*.V68`: SX-Window
  opens a document with its application by an `'ICN#'` entry of `\SHELL\ICON.LB` matching its
  name, and `*.lb` is taken there. An entry for `*.v68`, with its own file icon, opens
  VMX68K.SX with the file; the HDD builder adds it to new disks and `install_vmx68k` to an
  existing one. Started without a setup, VMX68K.SX uses `A:\VMX68K\DEFAULT.V68`, made when
  missing. The VM's SRAM is written back to its setup whenever it changes.
  - **Contents**: `'V68S'` 0, the version (1) and the speed (as `VmSpeed`'s), and `'SRAM'` 0,
    the VM's 16 KB of SRAM once it has one (`vmx68k_library::setup_library`).
  - **Opening**: the setup's path is VMX68K.SX's first command line word, which the Filer gives
    (`vmx68k.x %`). VMX68K.SX reads the setup, drops the library unwritten, and passes the speed
    and SRAM to `VmOpen` (`D6.L`, `D5.L`); the app starts the VM with them, the SRAM's RAM size
    set to the VM's. VMX68K.SX finds its own `VMX68K.LB` from its program's name in its task
    record (`TSGetTdb`), as Sharp's skeleton does.
  - **Saving**: when a speed is chosen, and when the VM's SRAM changes. The app takes the SRAM
    from the VM's thread once it stays unchanged for 0.5 s, so a program writing it byte by
    byte is saved once; the primary wakes VMX68K.SX, which takes it with `VmSram` (10) and
    writes the setup, as SXMODE.X writes `MSX68K.LB`.
- **New VM setups.** A menu makes a new setup at a path given, and edits and starts it.
- **Disk images.** The first test boots `disks/HUMAN302.XDF` from the host directly. As soon as
  the VM shows its screen in the window, images come from the primary VM's drives. Floppy and hard
  disk images are read and written on every access, written through, never cached whole: still
  orders of magnitude faster than real disks, and closer to what users expect.
- **Host bridges.** Later, the VM's IOCS and DOS calls are hooked and carried out as calls of the
  VMX68K.SX task in the primary VM, so the primary VM's Human68k stays the only owner of its file
  systems.
- **Shared mode, the default.** A VM sees the primary VM's storage as it is, with the same
  drive letters, bridged at the file level: each drive is a remote drive in the VM's Human68k,
  whose file operations VMX68K.SX carries out through the primary VM's Human68k. Sector access
  to the primary's disks is not given, since two Human68ks caching one file system would break
  it.
  The VM's own floppy drives are disconnected then, so that its Human68k gives them no
  letters after the remote drives'.
- **Floppy image mode.** VMs can still get floppy images on their own FDD, for software that
  drives the FDD directly, as a mode the VM's settings switch to. The FDD's drives then come
  after the remote drives.
- **Booting from a remote drive.** A boot routine loads `HUMAN.SYS` through the bridge
  and makes the bridged drive A: the boot drive, without sector access.
- **A VM's own CONFIG.SYS.** A VM must not start SX-Window, which the primary's `CONFIG.SYS`
  starts (`DEVICE = FSX.X`, `PROGRAM = TITLE.X`, `SHELL = SXWIN.X`). The bridge serves
  `\VMX68K\CONFIG.SYS` when the VM opens `\CONFIG.SYS` of its boot drive, until the VM's shell
  starts, so that afterwards the VM sees the primary's file as it is. If ending it there proves
  hard, the substitution may stay for good. `AUTOEXEC.BAT` is shared as it is, since it has no
  part in starting SX-Window.
- **Language.** VMX68K.SX is written in assembly, with the clean-room macros of `guest/include`
  and the in-process `rhas` / `rhlk` toolchain.
- **Devices.** The existing device emulation is enough for the first goal.

---

## 3. VM Threads, Disks, and Screens

The primary VM and VMX68K.SX keep running on the main thread. Each VM runs on a host thread of its
own and owns its `X68kVm`, at the pace of real time for its speed mode.

### 3.1 Disk Requests
A disk request is one command at the layer the VM emulates, and its reply comes from VMX68K.SX,
which reads or writes the disk image through the primary VM's DOS.

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
windows work the same way as the primary VM.

- **Each VM thread composites its own screen.** VMs share the GPU device and queue, which `wgpu`
  allows across threads. Each VM has its own VRAM, palette, and scanline state buffers, its own
  compute pass, and two output textures, one being written while the other is complete; an
  atomic frame number tells which one is complete.
- **Only changed pages travel.** VRAM writes are recorded per 4 KB page. At each vertical blank
  of the VM, its thread uploads the changed pages, the scanline table, the palette, and the
  sprite, BG, and page registers straight to its own GPU buffers, and dispatches its compute
  pass. VRAM never leaves the CPU except for that one upload, and no VRAM is copied between
  threads. The primary VM's screen moves to the same scheme instead of uploading every buffer
  every frame.
- **The final pass runs on the main thread.** It draws the primary VM's screen, then each VM's
  completed texture into its window's content rectangle, clipped to the window's visible region
  as a list of scissor rectangles, and under the primary's text plane: only where the text is
  clear (palette index 0, SX-Window's `G_THRU`), as the X68000 shows graphics under clear text.
  VMX68K.SX clears its screen area in all four planes (`GMAPage`), so menus, other windows,
  and the cursor that SX-Window draws in text over it stay in front.
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
- The SX-Window cursor is text, so it shows over VM windows like the rest of SX-Window.

### 3.5 VMX68K.SX and the Host
- **Calls.** VMX68K.SX reaches the host through msx68k's application entry
  ([HOOK_AND_SCSI_ARCHITECTURE.md](HOOK_AND_SCSI_ARCHITECTURE.md) Section 2.1): `VmOpen` (4)
  takes the window (`D1.L` = `WindowPtr`) and returns the VM's ID, `VmPoll` (5) is called on
  every pass of the event loop, and `VmClose` (6) stops the VM. `VmOpen` also takes the
  lines of the content above the VM's screen (`D4.W`, the icon bar), and `VmSpeed` (8) sets
  the VM's speed from the menu.
- **One VM per window, any number.** VMX68K.SX is an R-type module (`OBJR`): SX-Window runs
  it as often as it is started, sharing the module and giving each task a work area of its own
  (A1 at entry, then A5, the stack at its end; the size at `+$0C` of the header), which holds
  everything that changes. An O-type module (`OBJO`) cannot run twice.
- **When the primary's screen grows**, SX-Window does not draw what the old screen cut off, and
  windows' visible regions stay clipped to it, so the host has VMX68K.SX hide and show its
  windows (`VmStatus` bit 2), which draws them anew.
- **Window position and visible region.** VMX68K.SX does not report them. The host reads them
  from the window's `Graph` every frame: its rectangle, its bitmap's rectangle (which places it
  on the screen), and its visible region, which become the `VmLayer`'s position and scissor
  rectangles. So moving, covering, and uncovering the window need no code in VMX68K.SX.
- **Window size.** The window's content is the icon bar, 24 lines, over the VM's screen at its
  native size (768×512 for Human68k 3.02 at boot). When the VM changes its mode, the app
  reports the size of its frames to the primary, which wakes VMX68K.SX with `E_USER1`; it reads
  the size with `VmStatus` (`D1.L` = `width << 16 | height`) and sizes its window with `WMSize`,
  keeping its top left, and draws the bar anew to the new width.
- **First disk.** The host read the floppy image for drive 0 of new VMs itself at first, named by
  a command-line option of the app. Images come from the primary VM's drives now that the VM's
  screen shows in its window (Section 2).
- **Custom image files.** The settings GUI's ファイル rows now open raw floppy and HDD images on
  the primary's drives and pass their handles to the host at `VmOpen`/`VmRestart` in `A2.L`.
  Reads and writes go through VMX68K.X's DOS calls; images stay open until restart or exit.
  Missing files leave empty units, and three or four floppy units connect all four drives.
  HDD files use the same handles through SCSI IOCS and the block driver, without a whole-image
  cache; FDD units and SCSI IDs are assigned independently in row order. SCSI transfers larger
  than VMX68K.X's 32 KB buffer use several bridge requests. Mounting preserves the setup's
  SRAM boot choice. The legacy `A:\VMX68K\DRIVE0.XDF` service remains for compatibility tests.
  Custom 共有 rows add file-level remote drives after its FD and SCSI drives, preserving
  row order within the shared entries. They expose the primary's original files, including
  CONFIG.SYS, without shared mode's boot substitution. Passthrough rows and remote DIM images
  are still pending (see the settings UI notes).

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
  counts as writing all of TVRAM. The extended screen of the primary VM is not recorded.
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
  primary VM's HDD to `A>` this way (`crates/msx68k-vicon/tests/vmx68k_disk.rs`).
- Shared mode, first step (`crates/msx68k-bus/src/remote_fs.rs`): a VM's ROM window holds
  a boot ROM at `$FC0000` laid out like the internal SCSI ROM (IPL boot handle, install handle
  at the boot routine - 16, `'Human68k'`), and SRAM boots from it. The IPL boot routine reads
  `HUMAN.SYS` from the primary's `A:` through the bridge, puts its text and data at `$6800`,
  clears its bss, and jumps there. The install routine builds a remote device (`$2000`) with a
  unit for each of the primary's drives from `A:`, which VMX68K.X counts with `_GETDPB` when it
  starts and passes to `VmOpen` in `D3.L`, so the VM's letters are the primary's. Its initialise command writes `$00` over its command code,
  since Human68k reuses the request to initialise DISK2HD without setting it, which `$40`
  would send past DISK2HD's jump table. CHDIR, FILES, NFILES, OPEN
  for reading, READ, SEEK, CLOSE, FILEDATE, DSKFRE, DRVCTRL, GETDPB, FFLUSH, and MEDIACHECK
  become the primary's DOS calls (`VmPoll` kinds 3 to 8); the seek position and size live in
  the FCB. MKDIR, RMDIR, RENAME, DELETE, CHMOD, CREATE (and `_NEWFILE`), OPEN for writing,
  WRITE (with 0 bytes cutting the file), and setting FILEDATE become the primary's DOS calls
  too (`VmPoll` kinds 9 to 16), and LOCK succeeds. VMX68K.X calls `_FFLUSH` after every call
  that changes the primary's storage, so nothing a VM wrote waits in the primary's buffers.
  Names keep the case they were typed in, as on Human68k's own drives. Until the VM opens the shell named by its
  CONFIG.SYS (`COMMAND.X` by default), `\CONFIG.SYS` of the boot drive opens
  `\VMX68K\CONFIG.SYS`, or, when the primary has none, a CONFIG.SYS the host serves from
  memory (`FILES`, `BUFFERS`, `LASTDRIVE` only). Human68k 3.02 boots this way, runs the
  primary's `AUTOEXEC.BAT`, and reaches `A>`; `MD`, `ECHO` into a file, and `REN` typed in it
  land on the primary's drive, and `DIR B:` lists the floppy in the primary's `B:`
  (`crates/msx68k-vicon/tests/vmx68k_shared.rs`).
  The app starts VMs this way unless `--vm-fd0` names a floppy image.
- The host posts `E_USER1` when the guest is about to take an event (`EMGet`), into the Event
  Manager's queue the way `EMSet` would, with `whom` the VM's window, once per batch of requests
  until VMX68K.X polls. SX-Window's own mask (`$03BF`) leaves user events out, so the host posts
  them past it. The Task Manager hands the event to the window's task even behind another window.
  Booting Human68k takes about 290 wake-ups and 580 polls; with null events instead, VMX68K.X
  polled about 11,000 times.
- The compositor shader has no scanline table, sprites, or BG yet; it composites text and
  graphic page 0 from the state at the end of the frame.
- The final pass (`render_bus_to_surface_view`) draws the primary VM's screen, then each
  `VmLayer` (a VM's latest completed texture at a position on the primary VM's screen) once per
  rectangle of its visible region, as a scissor rectangle; rectangles are scaled with the
  primary VM's screen and clipped to the target (`crates/msx68k-vicon/tests/vm_layer.rs`). The
  app passes no layers until VMX68K.SX reports its windows.
- The primary VM's VICON compositor (`crates/msx68k-vicon`) still uploads the whole TVRAM, GVRAM,
  and palette every frame, and draws the host cursor into its TVRAM copy, so the cursor goes
  under VM layers (accepted for now, Section 3.4); both change with Section 3.3.

---

## 6. Open Questions

- **Mouse input to VMs**: relative or absolute motion, pointer capture, and how to leave it.
- **Settings file contents**: the resources in a VM's `.LB` and their layout, and how
  double-clicking one opens VMX68K.SX with it.
- **Display scaling**: how a VM screen is scaled, once windows go beyond native size.
- **The special hook** for booting in the primary VM's environment.
- **Startup speed** of a VM, and how several VMs and their sound are scheduled and mixed.
