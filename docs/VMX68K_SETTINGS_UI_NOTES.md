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

### 1.6 State of VMX68K
- VMX68K.X (`guest/vmx68k/vmx68k.s`) opens a 768x512 window, `VmOpen`s, waits for events with
  `E_USER1` in its mask, and serves bridge requests (`serve_disk`, 16 kinds). The host wakes it
  with `E_USER1` (`X68kBus::wake_vm_windows`).
- The host places the VM's screen at the window content's left, below the icon bar whose height
  `VmOpen` takes in `D4.W` (`vm_window_layout`), and the app starts VMs at 10 MHz with 4 MB, in
  shared mode unless `--vm-fd0` is given.
- **Step 1 is built** (2026-11-12): the 24-line icon bar with the Configuration icon (a gear,
  `'PLOT'` 1 of `VMX68K.LB`), its menu on the right button ("設定..." inactive until the dialog
  exists, and "68000" with the speed submenu, `'MENU'` 1), and `VmSpeed` (8), which the app
  applies with `VmThread::post`. The speed is not saved yet: that comes with the settings
  library. `VMX68K.LB` is written by the HDD builder and `install_vmx68k`. A VM thread in Burst
  now runs flat out until each frame is due, as the primary does. Tested in
  `crates/msx68k-bus/tests/vmx68k.rs`.

---

## 2. Proposed Implementation (not decided)

### 2.1 Order
1. ~~Icon bar and the Configuration menu with the CPU speed submenu, applied at once.~~ Done
   (Section 1.6).
2. ~~The access lamp.~~ Done (2026-11-13). The floppy icons wait for custom mode, which the
   settings library and dialog bring, as they show only there.
3. The per-VM settings library (`.LB`) with its SRAM, written back whenever it changes, and
   `A:\VMX68K\DEFAULT.V68` without one given; saving what is applied at once starts here.
   Setups are `*.V68`, opened with VMX68K.SX by an `'ICN#'` entry in `\SHELL\ICON.LB`
   (Section 1.7). Decided and built 2026-11-13 ([VMX68K.md](VMX68K.md) Section 2, "A settings
   file per VM").
4. In this order (decided 2026-11-13): 4a the dialog with the CPU kind and memory, saving on
   close and the restart question (done 2026-11-13); 4b drive assignment, shared and custom mode, built in the order ファイル, 共有, パススルー; mock: [images/vmx68k_drive_dialog_mock.png](images/vmx68k_drive_dialog_mock.png); 4c the boot
   device submenu and the floppy icons, which need custom mode; 4d the menu to make a new
   setup (moved from step 3, as it needs a path input). The configuration dialog: CPU kind, memory slider, drive assignment, saving on close and the
   restart question (`DMError` `D_YESNO`).

### 2.2 Pieces
- **`VMX68K.LB`** (built, `crates/msx68k-bus/src/vmx68k_library.rs`) would later also hold the
  boot device submenu and the `'DLOG'`/`'DITL'` of the configuration dialog.
- **Icons** of the later steps as more `'PLOT'` plot images in `VMX68K.LB` (Section 1.5).
- **New application functions** (after `VmSpeed` 8): `VmStatus`, the VM's CPU and speed, lamp
  states, and floppy contents, read by VMX68K.X when the host wakes it with `E_USER1` after a
  lamp or disk change.
- **Lamps**: the FDC finishes commands at once, so a lamp would stay lit for a short while
  (about 0.2 s) after an access; the access lamp would light for floppy, SCSI, and bridge
  requests.

### 2.3 Still Open
- The resources in a VM's `.LB` and their layout, and how double-clicking one opens VMX68K.SX
  with it ([VMX68K.md](VMX68K.md) Section 6).
- The icons' pictures (first versions drawn by Claude, for the user to adjust).
