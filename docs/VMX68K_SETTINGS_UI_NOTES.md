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
  applied at once with `VmSpeed` (8) and saved.
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
  leaves the running images open. Raw floppy images are mounted in file-row order through
  `RemoteDisk::with_handle`, without copying the whole image. A file that cannot open reserves
  an empty floppy unit; three or four units connect all four drives. The VM boots from its own
  SRAM, without installing the shared boot ROM. HDD images use the same `RemoteDisk`
  handles through the SCSI IOCS and block driver, with separate FDD unit and SCSI ID assignment in row order. Mounting reads nothing and does
  not rewrite SRAM; image data and partition/BPB metadata are read on the VM thread. SCSI
  transfers are split at the existing 32 KB bridge buffer limit and writes go through at once.
  The block driver exposes one unit per HDD target, using each image's first Human68k
  partition (as the resident backend did for target 0). Custom shared and passthrough rows
  remain unsupported; remote DIM images are not handled yet.
- Tests: `crates/msx68k-bus/tests/vmx68k.rs` (window, menu, lamp, sizes, setups, dialogs, two
  VMs) and `crates/msx68k-vicon/tests/vmx68k_shared.rs` (a VM booting in shared mode).
  `crates/msx68k-vicon/tests/vmx68k_disk.rs` also boots a custom raw image from the primary's
  HDD, using a path different from legacy DRIVE0.XDF, checking an absent image and four-drive
  connection. It also boots a custom HDD to `A>`, writes and rereads 64 KB through SCSI and
  VMX68K.X, and checks the primary's image file changed without a flush. A resident HDD boot
  test covers the generated IPL: its next sector loads at `$003000`, not over the executing
  IPL at `$002000` and its trap return PC. The bus tests cover handle counts, 512/1024-byte
  partition metadata, SCSI capacity/ranges, multiple targets, and disconnected bridges.
---

## 2. Proposed Implementation (not decided)

### 2.1 Order
1. ~~Icon bar and the Configuration menu with the CPU speed submenu, applied at once.~~ Done.
2. ~~The access lamp.~~ Done. The floppy icons wait for custom mode, as they show only there.
3. ~~The per-VM setup (`*.V68`) with its SRAM, and `A:\VMX68K\DEFAULT.V68`.~~ Done
   ([VMX68K.md](VMX68K.md) Section 2, "A settings file per VM").
4. In this order (decided 2026-11-13):
   - 4a ~~the configuration dialog with the CPU and memory, saving on 設定 and the restart
     question.~~ Done.
   - 4b drive assignment, shared and custom mode, in the order ファイル, 共有, パススルー (mock:
     [images/vmx68k_drive_dialog_mock.png](images/vmx68k_drive_dialog_mock.png)). The dialogs
     are done; 4b-1 has raw floppy and HDD/SCSI files working, followed by 4b-2 (共有 drives)
     and 4b-3 (パススルー).
   - 4c the boot device submenu and the floppy icons, which need custom mode.
   - 4d the menu to make a new setup, which chooses the boot device it writes in the setup's
     SRAM.

### 2.2 Pieces Still to Build
- **Custom image files on the VM** (4b-1): Raw floppy and HDD/SCSI images now work through
  the existing handle bridge, including boot and writes. Add remote DIM support separately
  if needed: the remote FDC backend accepts raw image sizes only. The SCSI block driver still
  uses each image's first Human68k partition; arbitrary partition layouts need separate work.
- **共有 drives** (4b-2) as remote drives of the VM, after its floppy and SCSI drives.
- **パススルー** (4b-3): a primary floppy drive handed to the VM sector by sector, write-
  protected on the primary meanwhile.
- **Floppy icons and the boot submenu** (4c), more `'PLOT'` icons in `VMX68K.LB`.
- **New setup** (4d): a path input and the boot device to write in the new setup's SRAM.

### 2.3 Still Open
- The resources in a VM's `.LB` and their layout, and how double-clicking one opens VMX68K.SX
  with it ([VMX68K.md](VMX68K.md) Section 6).
- The icons' pictures (first versions drawn by Claude, for the user to adjust).
