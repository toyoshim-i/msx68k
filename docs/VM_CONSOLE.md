# VMX68K: X68000 VMs as SX-Window Applications

VMX68K runs software that SX-Window cannot host: programs that drive the graphics
hardware, change screen modes, or expect a plain Human68k console. Such programs
start in an isolated VM rather than being detached after accessing unsupported
hardware in the master VM.

Only decisions are recorded as the plan. Proposals not yet decided are listed in Section 5.

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

## 3. First Goal

Boot Human68k 3.02 from `HUMAN302.XDF` as drive A in a VM, shown in a VMX68K.SX window. The
floppy boot itself already works (`crates/msx68k-bus/tests/fd_boot.rs`).

---

## 4. Known Facts

- `X68kVm` is `Send`, so a VM can move to and run on a thread of its own.
- The VICON compositor (`crates/msx68k-vicon`) currently uploads the whole TVRAM, GVRAM, and
  palette every frame, and draws the host cursor into its TVRAM copy.

---

## 5. Open Questions

- **Screen integration.** How the VM's screen is composed into the window efficiently: proposed
  were one compositor per VM sharing the GPU device, dirty-page uploads at each vertical blank
  through a triple buffer, one final pass that draws each VM into its window's content rectangle
  clipped to the window's visible region, and the cursor as a separate overlay.
- **Drive and bridge transport.** How requests travel between the VM thread and VMX68K.SX
  (proposed: request and reply messages over a channel, with the VM's time stopped while it waits).
- **Bridge hook point.** DOS calls (`$FFxx`) directly, or a remote drive device driver in the VM's
  Human68k.
- **Input routing.** Keyboard and mouse to the VM while its window is active, and how focus
  returns to the desktop.
- **Settings file format** and how double-clicking a settings file opens VMX68K.SX with it.
- **Display scaling** when the VM's screen is larger than the window or the desktop.
- **The special hook** for booting in the master VM's environment.
- **Startup speed** of a VM, and how several VMs and their sound are scheduled and mixed.
