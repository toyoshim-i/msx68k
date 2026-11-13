# VMX68K.SX Settings UI: Research Notes and Handoff

Notes for implementing the settings UI decided in [VMX68K.md](VMX68K.md) Section 2 ("Settings
menus" and the items after it). Section 1 is what the SX-Window references establish; Section 2 is
a proposed way to build it, **not decided**, to be confirmed with the user as each part is built.

---

## 1. Facts from the SX-Window References

The UI contracts below cover the Workroom headers, SX call identifiers, and the
official SX-Window programming manuals.

### 1.1 Menus (Menu Man, `MENU.H`)
- `MNConvert2` (`$A26B`, dev kit p. 358) builds a menu record from a string: items separated by
  commas, with special characters for a shortcut, an inactive item, and a check mark, and escapes
  (`\xhh`) for raw bytes. Given an existing record handle, it rebuilds that record, which is how a
  menu's check marks can follow the current state.
- `MNSelect` (`$A268`, p. 356) shows a menu from a global point and returns the chosen item number
  in the low word, and for an item of a submenu the submenu's `'MENU'` resource ID in the high
  word; 0 when nothing was chosen. `MNSelect3` (`$A26C`) also takes the item to show at the point
  and a rectangle to stay inside.
- **Submenus** (dev kit p. 77, column): an item that opens a submenu has ESC (`$1B`) as its
  shortcut and the submenu's `'MENU'` resource ID in its check field. The submenu must be a
  `'MENU'` resource of a resource library that is open. The kit's `sample.x` needs its
  `sample.lb` for this.
- `MNRefer` (`$A267`) makes a menu record from the `'MENU'` resource of an ID.
- A menu record and a `'MENU'` resource share one layout: `ID.w`, `width.w`, `hight.w`, `def.l`
  (definition handle), `enable.l` (one bit per item), `defData.l`, `number.w`, then per item
  `shortCut.b`, `check.b`, `length.b`, and the title. Seen in Sharp's libraries on the
  SX-Window disks (for example `'MENU'` `$F000`, the 5 items starting with カット).
- The kit's sample (dev kit pp. 89-90) pops a menu up from a click in a window: `GMGlobalToLocal`,
  `GMPtInRect` to find the area clicked, `MNConvert2(NULL, string, MI_PLN)`, `MNSelect`, then
  `MMHdlDispose` of the record.
- **`MNSelect` follows the right button only** (tried in VMX68K.X): called on a left press, it
  returns 0 at once and shows nothing. SX-Window's popup menus are all right-button ones.
- The string's special characters, from the Workroom samples (`ATTRIB.C`, `TEDIT.C`): `^` before
  a shortcut, `~` for an inactive item, `!` for a check mark, `|` to put the next byte in the
  check field. In assembly the submenu item is `"^", $1B, "|", id` before its title, raw bytes
  needing no `\x` escapes. `MNConvert2` given the handle of a loaded `'MENU'` resource rebuilds
  it in place, and the submenu then shows the new check marks.

### 1.2 Dialogs and Controls (`DIALOG.H`, `CONTROL.H`)
- Resources `'DLOG'` (bounds, `defID`, `visible`, `close`, `taskID`, `itemID`, title) and `'DITL'`
  (a count, then items: handle, bounds, `type.b`, `size.b`, data). Sharp's libraries hold many
  (for example `コントロール.LB`, whose `'DITL'` `$F3AC` holds 11 items).
- Calls: `DMOpen` (`$A2C3`), `DMRefer` (`$A2C4`, from a `'DLOG'` ID), `DMClose`, `DMDispose`,
  `DMControl` (`$A2C7`, with a filter procedure), `DIGet`/`DISet` (item handle and bounds),
  `DITGet`/`DITSet` (item text), `DIHide`/`DIShow`.
- `DMError` (`$A2F6`) shows an alert of a type: `D_CONFIRM` (1), `D_YESNO` (4), `D_SAVE`,
  `D_EXEC`, `D_CONT`. `D_YESNO` fits the restart question.
- Control definitions (`'CDEF'` IDs): `CI_CHRBTN` 0 push button, `CI_RADBTN` 1 radio button,
  `CI_CHKBOX` 2 check box, `CI_SLDVOL` `$13` slider, `CI_ADJBTN` `$14`, then the scroll bars
  `$40`..`$43` ([SX_WINDOW_API_AND_HOOK_STATUS.md](SX_WINDOW_API_AND_HOOK_STATUS.md) Section 8.1).
- Text input: Text Man (`TMNew` `$A30B` and on); the dialog record keeps a `TEdit` handle.

### 1.3 Files from the Filer
- SX-Window has no standard file dialog that the references show. Applications take files by
  drag and drop: when a drag ends, the Task Man event 81 (`DRAGEND`) arrives; the application checks
  that the drop is over its window and then takes the drag record (`Drag`: size, cell list
  handle, task ID, origin, image; `TASK.H`), whose cells (`Cell`: kind, size, data) name what was
  dragged. `TSCellToStr` (`$A427`) extracts a string, such as a path, from a cell list
  (supplement p. 301). The supplement's sample `GRPSMPL.S` handles `DRAGEND` in full
  (`sxwindow_programming_1991_supplement.txt` around line 18077, and line 16299).
- The programming book explains drags and `DRAGEND` around `sxwindow_programming_1991.txt` line
  16402.
- Opening a document: the Task Man event `OPENFILE` (20) and the `OpenFile` record (`name`,
  `mode`, `ID`) in `TASK.H`. How the Filer chooses the application for a double-clicked file is
  still to be found.

### 1.4 Resources
- `TSResOpen` (`$A4A1`) opens a resource library by path and returns its handle; `TSRscGet`
  (`$A4AB`) fetches a resource by type and ID; `TSResIDList` (`$A4BF`) lists IDs. msx68k already
  reads and writes libraries (`crates/msx68k-bus/src/sx_resource.rs`) and adds resources to
  `コントロール.LB` ([SX_WINDOW_API_AND_HOOK_STATUS.md](SX_WINDOW_API_AND_HOOK_STATUS.md)
  Section 9).
- An application's own library sits next to it under its name (`CGビジョン.X` and
  `CGビジョン.LB`).
- `TSResLoad` (`$A4A5`) reads all of the current map's resources and closes its file;
  `TSResRemove` (`$A4A4`) drops the map without writing it, where `TSResClose` writes it back.

### 1.5 Plot Images (`GMPlotImg`, `$A182`)
- `GMPlotImg(image, rect, mode)`: the high byte of the mode is the image's planes (0 meaning 2),
  the low byte the style (0 plain). The image is those planes, then a mask plane, each of rows
  of whole words (`native_gm.rs` `gm_plot_img`, which follows the guest). A `'PAT2'` icon has
  no mask, so it is not a plot image.
- Over SX-Window's 4-plane text screen, a two-plane image gets plane 3 set and plane 2 clear:
  neither plane is white (8), plane 0 light gray (9), plane 1 dark gray (10), both black (11).
- Windows draw in planes 0 and 1 over plane 3 set, so a color 0 fill gives white (8), not clear
  text. `GMAPage` (`$A149`, `G_ALLPAGE` = `$0F`) makes drawing reach all four planes, as the
  Workroom's `SUBWIN.C` does to clear a subwindow; VMX68K.X clears its screen area so, and the
  host shows the VM's screen only where the primary's text is clear.
- SX-Window's icons are pictures in a rectangular recess: in the Filer's, a raised plate (white
  top and left, black bottom and right) holds a recess (black top and left, white bottom and
  right) over dark gray, and the picture is white with a black shadow to the bottom right.

### 1.8 Dialog Resources, as Built for VMX68K.X
- `'DLOG'`: rectangle, `$0240` (dialog frame), visible 0, close 0, task 0, `'DITL'` ID, empty
  title. `'CNTL'`: rectangle, value, visibility (-1), maximum, minimum, `'CDEF'` × 16 (`$0130`
  the slider), reference, title. `'DITL'` items: SX-Window steps to the next item past the
  data's size, so data is padded to an even size inside it, or DMRefer faults (system error 2).
- Type 7 items name a `'CNTL'` by its ID; radio buttons (type 5) are 33×9 switches without a
  label; how static text (type 8) keeps its text is unknown, `DITSet` sets it.
- Editable text items, as the Workroom's samples (`ダイアログ.LB`, `ファイル操作.LB`) have them,
  are type `$90` (`D_EDTTXT` with `D_DISABL`: they handle their clicks and keys and do not
  return from `DMControl`), their data as many spaces as they hold, a Pascal string: the
  field's size. `DITGet` returns the text with its trailing spaces. The samples' radio buttons
  and check boxes have no data.
- `DMControl` calls the filter with A5 not the task's: VMX68K.X keeps its dialog records in its
  work area (`DMRefer` takes the storage; `DMClose` then, not `DMDispose`), with each record's
  offset after it, and the filter finds the work area from the dialog it is given.
- `DMControl` returns a slider's item once its drag ends; `DITSet` does not draw the text anew
  (`DMDraw` does). `DMError` with `D_YESNO` returns 1 for はい.

### 1.7 Documents and Their Applications (`\SHELL\ICON.LB`)
- The Filer's icons and what a double-click runs come from the `'ICN#'` resources of
  `\SHELL\ICON.LB`, `IcKind` records of `TASK.H`: the icon's resource type and ID (`'PAT3'`),
  attributes, left-click handling, the right-press menus, then Pascal strings of a file name
  pattern (`*.pix`, `key.sys`), the program to run, and its parameters, where `%` stands for
  the file (`*.pix` → `ｷｬﾝﾊﾞｽ.x %`, `*.bat` → `command.x %`). Programs are named without a
  path. User entries start at ID `$80`; the first match in ID order wins, so names come before
  patterns (`key.sys` `$89` before `*.sys` `$8C`). `*.lb` (`$91`) has no program.
- The icons, `'PAT3'`, are plot images (Section 1.5): their rectangle, planes 0 and 1, and the
  mask, about 48×34, a plate white at its top and left with a black shadow at its bottom
  and right.
- A program started so finds `%` replaced by the file's path on its command line, in its task
  record (`TSGetTdb`, `$A35B`: a 90-byte name, the program's path, then the command line as a
  Pascal string).
- Sharp's installer (`ｲﾝｽﾄｰﾗ.X`, "アイコン登録") registers applications there, as `*.pen` →
  `ｼｬｰﾍﾟﾝ.x %` and `*.ins` → `ｲﾝｽﾄｰﾗ.x`.

### 1.6 State of VMX68K (2026-11-13)
- **VMX68K.X** (`guest/vmx68k/vmx68k.s`) is an R-type module, its state in the task's work area
  (A5), so any number of VMs run, one per window. It opens a window with a 24-line icon bar
  over the VM's screen (`VmOpen` `D4.W`), serves the VM's bridge requests when the host sends
  `E_USER1`, and reads the VM's state with `VmStatus` (9): the access lamp, the screen's size,
  which the window follows (`WMSize`), SRAM to save, and a redraw after the primary's screen
  grew.
- **Menu** (right button on the gear icon): 設定..., and the CPU's name with its speeds,
  applied at once with `VmSpeed` (8) and saved; custom-only Boot; and 新しい設定....
- **Setups** are `*.V68` libraries (`'V68S'` speed, CPU, RAM; `'SRAM'`; `'V68D'` drives),
  opened from the Filer through `\SHELL\ICON.LB`, or `A:\VMX68K\DEFAULT.V68`; SRAM is saved as
  it changes (`VmSram` 10).
- **Configuration dialog** (4a): CPU 68000 or 68030, memory 1 to 12 MB, and the ドライブ row,
  共有 or カスタム with 詳細... (4b), opening ドライブの詳細: 1 to 8 drives, each ファイル, 共有, or
  パススルー with a path or drive letter. 設定 saves the setup and asks whether to restart the VM
  now (`VmRestart` 11).
- **The VM** starts with the setup's CPU (68030 from `IPLROM30.DAT`), RAM, speed, and SRAM, in
  shared mode: its drives are the primary's, under the same letters, and its own floppy drives
  get no letters. Custom mode now passes a drive table to `VmOpen`/`VmRestart` in `A2.L`
  (0 for shared mode; count.w, then kind.w, handle/drive.w, size.l per row). VMX68K.X opens
  file images read/write and owns their handles until restart or exit; saving a setup and declining restart
  leaves the running images open. Floppy images are mounted in file-row order through
  `RemoteDisk::with_handle`, without copying the whole image. A file that cannot open reserves
  an empty floppy unit; three or four units connect all four drives. The VM boots from its own
  SRAM, without installing the shared boot ROM. HDD images use the same `RemoteDisk`
  handles through the SCSI IOCS and block driver, with separate FDD unit and SCSI ID assignment in row order. Mounting reads nothing and does
  not rewrite SRAM; image data and partition/BPB metadata are read on the VM thread. SCSI
  transfers are split at the existing 32 KB bridge buffer limit and writes go through at once.
  The block driver exposes one unit per HDD target, using each image's first Human68k
  partition (as the resident backend did for target 0). Custom 共有 rows now map each selected
  primary drive, in row order, to file-level remote units after the VM's own FD and SCSI
  drives. They reuse shared mode's DOS bridge, expose the primary's real CONFIG.SYS, and
  preserve SRAM and the physical drives. パススルー rows now lend primary FD units exclusively,
  with live media state and sector reads/writes through the same bridge. RAW and DIM files
  are parsed by the same format registry as command-line mounts (details below).
- **Custom driver installation**: Human68k installs a ROM boot device before its built-in FD
  driver, and non-boot ROM interfaces afterwards. For HDD boot with custom shares, `_BOOTINF`
  reports the HDD-only ROM anchor at `$FC0200` instead of `$FC0000`, preserving the IPL's boot
  flags and leaving SRAM unchanged. The standard `$FC0000` interface then installs shares
  after the built-in FD driver, and also installs any HDDs not already registered (FD boot).
  Its `CustomInstall` stub is at `$FC0108`, with the remote strategy/interrupt at `$FC0100` /
  `$FC0104`. The normal IPL `_BOOTINF` handles other boot devices and guest-installed handlers.
  A reset clears the SCSI driver enumeration state so non-boot HDDs are installed again.
  Verified against Human68k 3.02's `$7E70` (boot/built-in driver setup), `$7F0C` (ROM driver
  enumeration), and `$6AC2` (non-boot SCSI registration). The IPL's `_BOOTINF` reads its flags
  from `$09DF`/`$09E0` and its ROM boot address from SRAM `$ED000C`.
- **Passthrough ownership**: VMX68K.X flushes primary DOS writes before opening/restarting a
  custom VM; if the flush fails, its passthrough rows are unavailable. `VmOpen` / `VmRestart`
  resolve each primary drive letter through Human68k's DPB chain at `$1C3C`: the DPB's drive,
  unit, device-header pointer, and next pointer, and the `DISK2HD` name identify physical FD
  units. HDD/remote drives, already lent units, and nested remote FDs cannot be lent. Failed
  passthrough rows leave an empty floppy slot and are reported by the host.
  `bind_vm_passthrough` attaches the reserved units to the VM's bridge before mounting. The
  primary FDC reports write-protect and rejects writes while lent, separately from the disk's
  real tab. The VM uses live atomic media length/tab/generation metadata, not an image copy.
  Native `FloppyRead` / `FloppyWrite` requests are completed inside `VmPoll`, on the primary
  thread, with owner/range/sector-alignment checks; no new external request kind or DOS file
  operation is sent to VMX68K.X. The VM's FDC retains its own command/DMA behavior.
  Media generation changes raise an FDD interrupt at V-Blank. A removal stays latched until
  firmware reads it; the following V-Blank announces insertion, so even an eject/insert between
  frames cannot collapse into an unchanged presence bit. This invalidates Human68k's cache
  even when replacing a disk with another of the same format. Bridge detach, close, restart,
  and VM startup failure release ownership, invalidate the old VM's media state, and preserve
  the current disk's real tab (including a disk inserted while lent).
- **Shared image formats**: `floppy_image.rs` owns the format registry (`FloppyImage::probe`),
  decoded geometry, and logical-sector to file-offset mapping. Both `Fdc::insert_disk` and
  VMX use it. RAW and DIM 2HD/2HC/2HQ are supported; adding a format there covers both paths.
  The guest does not recognize extensions, header types or raw sizes. `VmImage` (14) takes
  D1 DOS handle, D2 file length, D3 chunk length (0 starts a probe), A0 chunk data. Result
  D0=2 requests D1 bytes at file offset D2; D0=1 caches the recognized layout by handle,
  D0=0 means another file type. Prefixes accumulate natively in 32 KB chunks, not whole images.
  Runtime drive-table kind 3 carries the handle and physical file length; `VmOpen`/`VmRestart`
  consume its layout into `CustomDrive::Image`. Live replacement transfers it with the
  existing Media event. Kind 0 retains legacy raw/HDD behavior.
  `RemoteDisk` shares the mutable layout across its clones and uses its explicit geometry,
  while its `ImageStorage` transport remains format-independent. Missing DIM tracks read $E5.
  A write to one expands packed tracks backwards in place using one-track buffers, fills
  omitted tracks, and updates the DIM flags. `ImageSync` (poll kind 18, D3 handle) closes and
  reopens the file, using `_DUP2` if the reopened handle differs: `_FFLUSH` alone leaves the
  extended file's directory length stale. Pending old-media paths are retained until kind 17
  retires their handles, so a layout change also works while a swap is queued.
  Unit tests cover every supported DIM type, packed/full layouts, local/remote sector parity,
  bounded transfers, shared layout updates and persistence. UI tests drop a packed DIM, write
  an omitted track through actual Human68k DOS, force handle reassignment, reopen the resulting
  file and verify its header, length, preserved neighbours and new contents. A VM boots
  Human68k from a DIM stored on the primary HDD even when its filename ends in XDF.
- **Live floppy controls (4c)**: PLOT IDs 10–17 are FD0–3, empty/inserted. `VmStatus`
  returns connected units in D2 bits 0–3, inserted media in 4–7, per-unit lamps in 8–11,
  and passthrough sources in 12–15; shared mode returns zero. The app applies the same 0.2 s
  afterglow as the global lamp. Left-click ejects, DRAGEND (81) accepts a single Filer path
  with `TSGetDrag` / `TSCellToStr`. `VmMedia` (12), D1 ID, D2 unit, D3 new handle (-1 eject),
  D4 file length, posts a change to the VM thread. Existing I/O finishes first; kind 17
  (`MediaDone`, D1 unit) then lets the task close its retained old handle. Per-unit busy flags
  prevent overlapping swaps. Native FloppyEject keeps a physical lease for reinsertion;
  FloppyRelease relinquishes it when an image replaces it. Running assignments have their
  own copy, so live exchanges cannot overwrite different assignments pending restart.
- **Boot (4c)**: MENU 2 checks STD, FD0–3, SCSI0–7, ROM. `VmBoot` (13), D1 ID,
  D2 choice 0–13, A1 ROM address, changes the VM's SRAM and supplies it through `VmSram`
  for immediate saving. The VM thread applies it without a reset. D2=-1 queries; D1=0
  generates fresh SRAM at A0 with RAM MB in D3, for new setups. `VmStatus` returns the
  current choice in D3. SRAM $18 is $0000 for STD, $9070+(unit<<8) for FD, $A000 for ROM;
  SCSI uses ROM handles $FC0000+4*target. Each target's boot stub has Human68k metadata and
  the generated disk IPL preserves D4, so a nonzero target boots and gets its correct letter.
- **New setup (4d)**: DLOG/DITL 3 takes the new path and the initial boot choice. DLOG/DITL 4
  edits a ROM address, initially the current vector, rejecting odd or out-of-range values.
  A new file inherits CPU, RAM, speed and drive assignments, uses custom mode and fresh SRAM,
  and leaves the original VM/setup intact. Existing files are rejected. TSFock ($A351) shares
  the R module with a separate work area (mode word $0002, a 20-byte argument block), passing
  the quoted path and /setup to open the new task's configuration dialog. Paths accept quotes.
- **Verification**: the VMX68K UI test drags an actual Filer image onto an FD, reads its bytes
  through the bridge, ejects it, checks the old handle stays open until acknowledgement and
  closes afterwards, and selects Boot/FD1 and checks saved SRAM. Another creates a setup
  with SCSI2, checks the original assignments, and observes the second VM/configuration
  dialog, then rejects creating the same path again without changing its bytes.
  `vmx68k_disk.rs` also boots SCSI6 with SCSI0 present, checks its B: prompt, and resets
  to FD0 and checks A:; reset clears the preceding SCSI boot's target and drive-letter state.
- Testing: `crates/msx68k-vicon/tests/vmx68k_lifecycle.rs` runs the UI, disk, shared-drive
  and passthrough scenarios sequentially in one primary. Cold-boot checks remain separate.
  See [TESTING.md](TESTING.md) for commands, subset selection, diagnostics and lifecycle
  policy; the runner itself is the maintained list of scenarios.

---

## 2. Proposed Implementation (not decided)

### 2.1 Order
1. ~~Icon bar and the Configuration menu with the CPU speed submenu, applied at once.~~ Done.
2. ~~The access lamp.~~ Done, including per-unit lamps on custom-mode floppy icons.
3. ~~The per-VM setup (`*.V68`) with its SRAM, and `A:\VMX68K\DEFAULT.V68`.~~ Done
   ([VMX68K.md](VMX68K.md) Section 2, "A settings file per VM").
4. In this order (decided 2026-11-13):
   - 4a ~~the configuration dialog with the CPU and memory, saving on 設定 and the restart
     question.~~ Done.
   - 4b drive assignment, shared and custom mode, in the order ファイル, 共有, パススルー (mock:
     [images/vmx68k_drive_dialog_mock.png](images/vmx68k_drive_dialog_mock.png)). The dialogs
     are done; 4b-1 has raw floppy and HDD/SCSI files working and 4b-2 (共有 drives) is done.
     4b-3 (パススルー) is done too.
   - 4c ~~the boot device submenu and the floppy icons, which need custom mode.~~ Done.
   - 4d ~~the menu to make a new setup, which chooses the boot device it writes in the setup's
     SRAM.~~ Done.

### 2.2 Pieces Still to Build
- **HDD partition layouts**: RAW/DIM floppy and HDD/SCSI images work through the shared
  image/handle bridge, including boot and writes. The SCSI block driver still uses each
  image's first Human68k partition; arbitrary partition layouts need separate work.

### 2.3 Still Open
- The resources in a VM's `.LB` and their layout, and how double-clicking one opens VMX68K.SX
  with it ([VMX68K.md](VMX68K.md) Section 6).
- The icons' pictures (first versions drawn by Claude, for the user to adjust).
