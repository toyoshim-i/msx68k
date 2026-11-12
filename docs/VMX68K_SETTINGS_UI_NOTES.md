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

### 1.5 State of VMX68K
- VMX68K.X (`guest/vmx68k/vmx68k.s`) opens a 768x512 window, `VmOpen`s, waits for events with
  `E_USER1` in its mask, and serves bridge requests (`serve_disk`, 16 kinds). The host wakes it
  with `E_USER1` (`X68kBus::wake_vm_windows`).
- The host places the VM's screen at the window content's top-left (`vm_window_layout`), and the
  app starts VMs at 10 MHz with 4 MB, in bypass mode unless `--vm-fd0` is given.

---

## 2. Proposed Implementation (not decided)

### 2.1 Order
1. Icon bar and the Configuration menu with the CPU speed submenu, applied at once.
2. Floppy icons with eject and drop (custom mode), and the access lamp.
3. The per-VM settings library (`.LB`) with its SRAM, the boot device submenu, and the menu to
   make a new setup; saving what is applied at once starts here.
4. The configuration dialog: CPU kind, memory slider, drive assignment, saving on close and the
   restart question (`DMError` `D_YESNO`).

### 2.2 Pieces
- **`VMX68K.LB`** next to `VMX68K.X`, holding the `'MENU'` submenus (CPU speeds, boot devices)
  and later the `'DLOG'`/`'DITL'` of the configuration dialog. The HDD builder
  (`build_bootable_sxwin_scsi_hdd`) and `install_vmx68k` would put it in `\X68`, built with
  `ResourceLibrary`. Check marks would be set before each `MNSelect` by rebuilding the submenu
  record with `MNConvert2`.
- **Icons** as bit images drawn by VMX68K.X into the bar, like the control panel item's
  `'PAT2'` (`crates/msx68k-bus/src/sx_control_panel.rs`), with the drawing call to be chosen.
- **New application functions** (numbers after `VmReply` 7):
  - placing the VM's screen inside the window, below the bar (the host's layout now assumes the
    content's top-left);
  - setting the VM's speed, which the app applies with `VmThread::post`;
  - `VmStatus`: the VM's CPU and speed, lamp states, and floppy contents, read by VMX68K.X when
    the host wakes it with `E_USER1` after a lamp or disk change.
- **Lamps**: the FDC finishes commands at once, so a lamp would stay lit for a short while
  (about 0.2 s) after an access; the access lamp would light for floppy, SCSI, and bridge
  requests.

### 2.3 Still Open
- The resources in a VM's `.LB` and their layout, and how double-clicking one opens VMX68K.SX
  with it ([VMX68K.md](VMX68K.md) Section 6).
- The icons' pictures (first versions drawn by Claude, for the user to adjust).
