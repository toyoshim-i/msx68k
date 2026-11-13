* -----------------------------------------------------------------------------
* VMX68K.X: shows a virtual X68000 in an SX-Window window (docs/VMX68K.md)
* The host runs the VM on a thread of its own and lays its screen over the
* window's content below an icon bar, so this task opens the window, draws the
* bar and pops up its menus, carries out the VM's disk requests when the host
* sends E_USER1 for them, and closes the VM at the end. The bar's icons and the
* menus' submenus are resources of VMX68K.LB, next to this program. The VM's
* setup, its settings and its own SRAM, is a resource library too, *.V68, given on
* the command line, as the Filer gives it (\SHELL\ICON.LB), or DEFAULT.V68.
* Drive 0 of the VM is the image at DRIVE0_PATH, read and written through
* Human68k here, so the VM's disks live on the primary VM's drives.
* Assembled by rhas and linked by rhlk
* -----------------------------------------------------------------------------

	.include "doscall.mac"
	.include "iocscall.mac"
	.include "sxcall.mac"
	.include "msx68k.mac"

BAR_HEIGHT	.equ	24		; Lines of the icon bar above the VM's screen
SPEED_MENU_ID	.equ	1		; 'MENU' of the CPU's speeds in VMX68K.LB
CONFIG_ICON_ID	.equ	1		; 'PLOT' of the Configuration icon in VMX68K.LB
SETUP_TYPE	.equ	'V68S'		; The setup's settings: version.w, speed.w, CPU.w, RAM MB.w
SETUP_VERSION	.equ	2		; Version 1 had the speed only, for a 68000 with 4 MB
SETUP_SIZE	.equ	8
CONFIG_DIALOG_ID	.equ	1	; 'DLOG' and 'DITL' of the configuration dialog in VMX68K.LB
ITEM_OK		.equ	1		; Its items (crates/msx68k-bus/src/vmx68k_library.rs)
ITEM_CANCEL	.equ	2
ITEM_68000	.equ	3
ITEM_68030	.equ	4
ITEM_MEMORY	.equ	5
ITEM_MEMORY_SIZE	.equ	6
ITEM_SHARED	.equ	12		; The ドライブ row: 共有, カスタム, and 詳細...
ITEM_CUSTOM	.equ	13
ITEM_DETAILS	.equ	14
DRIVE_DIALOG_ID	.equ	2		; The ドライブの詳細 dialog
ITEM_FEWER	.equ	3		; Its - and + buttons and the number of drives
ITEM_MORE	.equ	4
ITEM_COUNT	.equ	5
ITEM_ROWS	.equ	13		; Its rows: number, ファイル, 共有, パススルー, text field
ROW_ITEMS	.equ	5
DRIVE_ROWS	.equ	8
* A setup's drives, 'V68D': version.w, mode.w (0 shared, 1 custom), count.w, then
* DRIVE_ROWS entries of kind.w (0 ファイル, 1 共有, 2 パススルー) and a Pascal
* string of up to 92 bytes, a path or a drive letter
DRIVES_TYPE	.equ	'V68D'
DRV_MODE	.equ	2
DRV_COUNT	.equ	4
DRV_ENTRIES	.equ	6
ENTRY_SIZE	.equ	96
DRIVES_SIZE	.equ	DRV_ENTRIES+DRIVE_ROWS*ENTRY_SIZE
WINDOW_KIND	.equ	64		; Window.kind: W_DIALOG is bit 1
WINDOW_UPDATE	.equ	80
WINDOW_TASK	.equ	110
DIALOG_ITEMS	.equ	114
DIALOG_TEDIT	.equ	118		; A dialog record's TEdit handle, after its window
DIALOG_RECORD	.equ	136		; A dialog record, 128 bytes, kept in the work area, and
					; after it the record's offset in the work area, from which
					; dialog_filter finds the work area: A5 is not this task's then
D_YESNO		.equ	4		; DMError's はい/いいえ
SRAM_TYPE	.equ	'SRAM'		; The VM's own SRAM
SRAM_SIZE	.equ	16384
G_THRU		.equ	0		; Clear text, where the host shows the VM's screen
G_WHITE		.equ	8
G_LGRAY		.equ	9
G_BLACK		.equ	11
G_RED		.equ	13		; The access lamp, lit
G_DGRAY		.equ	10		; The access lamp, out

* -----------------------------------------------------------------------------
* The work area, this task's own, at A5: VMX68K.X is an R-type module, which SX-
* Window shares among the tasks it runs, so everything that changes lives here,
* past the stack at its end
* -----------------------------------------------------------------------------
PATH_MAX	.equ	92		; Task records hold a 90-byte name, then the command line
TASK_COMMAND	.equ	90
STACK_SIZE	.equ	8192		; Window tracking (SXCallWindM) goes past 2 KB
DISK_BUFFER_SIZE	.equ	32768	; A whole cylinder, both sides, of any floppy format

	.offset	0
win_ptr:	.ds.l	1
app_task_id:	.ds.l	1
vm_id:		.ds.l	1
drive0_size:	.ds.l	1
request:
request_kind:	.ds.l	1
request_offset:	.ds.l	1
request_length:	.ds.l	1
request_handle:	.ds.l	1
setup_handle:	.ds.l	1		; pseudo-handles to setup_rec and sram_buffer
sram_handle:	.ds.l	1
screen_size:	.ds.l	1		; the VM's screen the window fits, width << 16 | height
drive0_fd:	.ds.w	1
vm_speed:	.ds.w	1		; by MSX68K_FN_VM_SPEED's numbers for run_cpu
vm_cpu:		.ds.w	1		; the setup's CPU, 0 the 68000, 1 the 68030
vm_ram:		.ds.w	1		; the setup's RAM in MB, 1 to 12
run_cpu:	.ds.w	1		; the CPU the VM runs with, whose speeds the menu shows
dlg_cpu:	.ds.w	1		; the dialog's choices
dlg_ram:	.ds.w	1
dialog_end:	.ds.w	1		; ENDTSK/CLOSEALL must unwind both settings windows
dlg_edit:	.ds.w	1		; active settings text item (not Dialog.edit's internal offset)
dlg_type:	.ds.w	1		; DIGet's item type, handle, and rectangle
dlg_hdl:	.ds.l	1
dlg_rect:	.ds.w	4
dlg_ptr:	.ds.l	1		; the dialog the dlg_ helpers work on
cfg_ptr:	.ds.l	1		; the configuration dialog's, while ドライブの詳細 is open
cfg_dialog:	.ds.b	DIALOG_RECORD	; the configuration dialog's record
		.ds.w	1		; its offset
det_dialog:	.ds.b	DIALOG_RECORD	; ドライブの詳細's
		.ds.w	1
drives_handle:	.ds.l	1		; a pseudo-handle to vm_drives
setup_rec:	.ds.w	4		; version, speed, CPU, RAM
event_rec:
event_what:	.ds.w	1		; +0: what
event_win:	.ds.l	1		; +2: whom.win
		.ds.l	1		; +6: when
event_where:	.ds.l	1		; +10: where
event_what2:	.ds.w	1		; +14: ts.what2
		.ds.w	1		; +16: ts.taskID
		.ds.l	1		; +18: padding
rects:
bar_rect:	.ds.w	4
bar_line_rect:	.ds.w	4
screen_rect:	.ds.w	4
lamp_rects:	.ds.w	12
win_active:	.ds.b	1
library_open:	.ds.b	1
lamp_lit:	.ds.b	1		; the access lamp as drawn
has_sram:	.ds.b	1		; sram_buffer holds the VM's SRAM
setup_found:	.ds.b	1		; the setup file is there
setup_is_default:	.ds.b	1	; the setup is DEFAULT.V68
start_config:	.ds.b	1
	.even
library_path:	.ds.b	PATH_MAX
setup_path:	.ds.b	PATH_MAX
menu_string:	.ds.b	64		; the speed submenu's items as MNConvert2 takes them
config_string:	.ds.b	128		; the Configuration menu's
boot_string:	.ds.b	160
fd_state:	.ds.l	1
vm_boot:	.ds.w	1
new_boot:	.ds.w	1
fd_rect:	.ds.w	4
pending_old:	.ds.w	4
pending_paths:	.ds.b	4*(PATH_MAX+2)
media_busy:	.ds.b	4
size_text:	.ds.b	6		; the memory's size, " 4MB", a Pascal string
	.even
run_drives:	.ds.b	DRIVES_SIZE	; choices attached to the running image handles
media_save:	.ds.w	1
vm_drives:	.ds.b	DRIVES_SIZE	; the setup's drives
dlg_drives:	.ds.b	DRIVES_SIZE	; the configuration dialog's
det_drives:	.ds.b	DRIVES_SIZE	; ドライブの詳細's
pass_flush_result:	.ds.l	1	; primary flush before FD ownership changes
drive_table:	.ds.w	1		; count, then kind.w, handle.w, size.l
		.ds.b	DRIVE_ROWS*8
new_path:	.ds.b	PATH_MAX
old_path:	.ds.b	PATH_MAX
new_command:	.ds.b	PATH_MAX+12
image_path:	.ds.b	PATH_MAX+2
dlg_text:	.ds.b	256		; a dialog item's text
VARS_SIZE:
sram_buffer:	.ds.b	SRAM_SIZE	; the VM's own SRAM, kept in its setup
disk_buffer:	.ds.b	DISK_BUFFER_SIZE
		.ds.b	STACK_SIZE
WORK_SIZE:

	.text
	.globl	_start
	.globl	gui_start

* An R-type module, which SX-Window runs more than once, a VM in each window,
* sharing the module and giving each task a work area of its own.
objr_header:
	.dc.b	'O','B','J','R'
	.dc.l	prog_end - objr_header		; +0x04: text size
	.dc.l	gui_start - objr_header		; +0x08: SX-Window GUI entry point offset
	.dc.l	WORK_SIZE			; +0x0C: work area size, the stack at its end
	.dc.l	0, 0, 0, 0			; +0x10..+0x1C: reserved

_start:
	* Run from the Human68k command line: it needs SX-Window
	pea	str_needs_sx(pc)
	DOS	_PRINT
	addq.l	#4, sp
	move.w	#1, -(sp)
	DOS	_EXIT2

gui_start:
	* A1 is this task's work area, and the stack is already at its end
	movea.l	a1, a5
	lea	(a5), a0
	move.w	#VARS_SIZE/2-1, d0
clear_work:
	clr.w	(a0)+
	dbra	d0, clear_work
	move.l	#-1, pending_old(a5)
	move.l	#-1, pending_old+4(a5)
	move.w	#-1, drive0_fd(a5)
	move.l	#768 << 16 | 512, screen_size(a5)
	move.w	#4, vm_ram(a5)		; what setups that do not say have
	move.w	#1, vm_drives(a5)	; shared, one drive
	move.w	#1, vm_drives+DRV_COUNT(a5)
	lea	rects_init(pc), a0
	lea	rects(a5), a1
	moveq.l	#RECTS_SIZE/2-1, d0
copy_rects:
	move.w	(a0)+, (a1)+
	dbra	d0, copy_rects

	SXCALL	TSGetID			; $A360
	lea	app_task_id(a5), a1
	move.l	d0, (a1)

	* Open the window, hidden until the VM is there. Its content is the VM's screen.
	move.l	app_task_id(a5), -(sp)	; refCon
	move.w	#-1, -(sp)		; goAwayFlag
	move.l	#-1, -(sp)		; behind = W_FRONT
	move.w	#$0310, -(sp)		; procID = WI_STD2 << 4
	clr.w	-(sp)			; visible = FALSE
	pea	win_title(pc)
	pea	win_bounds(pc)
	clr.l	-(sp)			; wStorage = NULL
	SXCALL	WMOpen			; $A1F9
	lea	26(sp), sp
	lea	win_ptr(a5), a1
	move.l	a0, (a1)
	move.l	a0, d0
	beq	do_ts_exit

	* This program's own resources are its name with .LB for its extension, and the
	* VM's setup is the command line's first word, or the default one
	move.w	#-1, -(sp)		; this task
	pea	disk_buffer(a5)
	SXCALL	TSGetTdb		; $A35B
	addq.l	#6, sp
	bsr	take_paths
	bsr	load_setup

	* Open VMX68K.LB and read all of it, leaving no file open; without it the bar
	* stays empty
	pea	library_path(a5)
	SXCALL	TSResOpen		; $A4A1
	addq.l	#4, sp
	move.l	a0, d0
	beq.s	no_library
	SXCALL	TSResLoad		; $A4A5
	lea	library_open(a5), a1
	st	(a1)
no_library:

	* Open drive 0's image, and find its size; without it the drive stays empty
	move.w	#2, -(sp)		; read and write
	pea	drive0_path(pc)
	DOS	_OPEN
	addq.l	#6, sp
	tst.l	d0
	bmi.s	no_drive0
	lea	drive0_fd(a5), a1
	move.w	d0, (a1)
	move.w	#2, -(sp)		; from the end
	clr.l	-(sp)
	move.w	d0, -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi.s	no_drive0
	lea	drive0_size(a5), a1
	move.l	d0, (a1)
no_drive0:

	* Count the primary's drives from A:, which a VM in shared mode sees as its own
	moveq.l	#0, d3
count_drives:
	addq.w	#1, d3
	cmpi.w	#26, d3
	bhi.s	drives_counted
	pea	disk_buffer(a5)
	move.w	d3, -(sp)
	DOS	_GETDPB
	addq.l	#6, sp
	tst.l	d0
	bpl.s	count_drives
drives_counted:
	subq.w	#1, d3

	bsr	open_custom_drives

	* Ask the host for a VM; without msx68k there is none, and the task ends
	moveq.l	#MSX68K_FN_VM_OPEN, d0
	move.l	win_ptr(a5), d1
	move.l	drive0_size(a5), d2
	moveq.l	#BAR_HEIGHT, d4		; the VM's screen goes below the bar
	moveq.l	#0, d5			; the VM's SRAM from its setup, if it has one
	tst.b	has_sram(a5)
	beq.s	vm_open_sram
	lea	sram_buffer(a5), a0
	move.l	a0, d5
vm_open_sram:
	move.w	vm_speed(a5), d6
	ext.l	d6
	move.w	vm_cpu(a5), d7
	swap	d7
	move.w	vm_ram(a5), d7
	move.w	vm_cpu(a5), run_cpu(a5)
	bsr	custom_drive_table
	bsr	msx68k_call
	tst.l	d0
	bmi	dispose_window
	lea	vm_id(a5), a1
	move.l	d0, (a1)
	* A setup that is not there yet is made with what the VM starts with
	tst.b	setup_found(a5)
	bne.s	setup_kept
	bsr	save_setup
setup_kept:

	move.l	win_ptr(a5), -(sp)
	SXCALL	WMShow			; $A207
	addq.l	#4, sp
	lea	win_active(a5), a1
	st	(a1)
	tst.b	start_config(a5)
	beq.s	event_loop
	bsr	config_dialog

event_loop:

	pea	event_rec(a5)
	* The host sends E_USER1 to the window when the VM has disk requests
	move.w	#$728A, -(sp)		; EM_USER1 | EM_MSLDOWN | EM_MSRDOWN | EM_UPDATE | EM_ACTIVATE | EM_SYSTEM1 | EM_SYSTEM2
	SXCALL	TSEventAvail		; $A357
	addq.l	#6, sp

	cmpi.w	#$DFFE, d0		; ER_ABORT
	beq	quit_app

	move.w	event_what(a5), d0
	cmpi.w	#1, d0			; E_MSLDOWN
	beq	handle_mouse_down
	cmpi.w	#3, d0			; E_MSRDOWN
	beq	handle_right_down
	cmpi.w	#7, d0			; E_UPDATE
	beq.s	handle_update
	cmpi.w	#9, d0			; E_ACTIVATE
	beq.s	handle_activate
	cmpi.w	#12, d0			; E_SYSTEM1
	beq.s	handle_system
	cmpi.w	#13, d0			; E_SYSTEM2
	beq.s	handle_system
	cmpi.w	#14, d0			; E_USER1: disk requests, or the lamp changed
	bne.s	event_loop
	bsr	serve_disk
	bsr	update_status
	bra.s	event_loop

handle_activate:
	lea	win_active(a5), a1
	move.l	event_win(a5), d0
	cmp.l	win_ptr(a5), d0
	seq	(a1)
	bra.s	event_loop

handle_system:
	move.w	event_what2(a5), d0
	cmpi.w	#1, d0			; ENDTSK
	beq	quit_app
	cmpi.w	#2, d0			; CLOSEALL
	beq	quit_app
	cmpi.w	#81, d0			; DRAGEND
	beq	handle_drop
	cmpi.w	#32, d0			; WINDOWSELECT
	bne.s	event_loop
	move.l	win_ptr(a5), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra	event_loop

handle_update:
	* The bar, and the rest clear for the VM's screen
	move.l	event_win(a5), d0
	cmp.l	win_ptr(a5), d0
	bne	event_loop
	move.l	win_ptr(a5), -(sp)
	SXCALL	WMUpdate		; $A20D
	bsr	draw_bar
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp
	bra	event_loop

handle_mouse_down:
	move.l	event_win(a5), d0
	cmp.l	win_ptr(a5), d0
	bne	event_loop
	tst.b	win_active(a5)
	bne.s	track_window
	move.l	win_ptr(a5), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra	event_loop
track_window:
	bsr	fd_hit
	tst.l	d4
	bmi.s	track_window_frame
	clr.b	image_path(a5)
	bsr	change_floppy
	bra	event_loop
track_window_frame:
	* Dragging and the close box
	pea	event_rec(a5)
	move.l	win_ptr(a5), -(sp)
	SXCALL	SXCallWindM		; $A3A2
	addq.l	#8, sp
	cmpi.w	#7, d0			; W_INCLOSE
	beq	quit_app
	bra	event_loop

handle_right_down:
	* The right button on an icon of the bar pops up its menu, as SX-Window's menus
	* follow the right button
	move.l	event_win(a5), d0
	cmp.l	win_ptr(a5), d0
	bne	event_loop
	move.l	win_ptr(a5), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	move.l	event_where(a5), -(sp)
	SXCALL	GMGlobalToLocal		; $A13F
	addq.l	#4, sp
	move.l	d0, -(sp)
	pea	config_icon_rect(pc)
	SXCALL	GMPtInRect		; $A156
	addq.l	#8, sp
	tst.b	d0
	beq	event_loop
	bsr	config_menu
	bra	event_loop

quit_app:
	moveq.l	#MSX68K_FN_VM_CLOSE, d0
	move.l	vm_id(a5), d1
	bsr	msx68k_call
	move.w	drive0_fd(a5), d0
	bmi.s	dispose_window
	move.w	d0, -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
dispose_window:
	bsr	close_custom_drives
	* VMX68K.LB goes without being written back
	tst.b	library_open(a5)
	beq.s	library_closed
	SXCALL	TSResRemove		; $A4A4
library_closed:
	move.l	win_ptr(a5), -(sp)
	SXCALL	WMDispose		; $A1FC
	addq.l	#4, sp
do_ts_exit:
	clr.l	-(sp)
	SXCALL	TSExit			; $A352
	clr.w	-(sp)
	DOS	_EXIT2

* -----------------------------------------------------------------------------
* Helper: draw_bar
* Draws the icon bar at the top of the window's content: the button face, a line
* under it, and the icons. The rest is left clear (G_THRU): the host shows the
* VM's screen only where the text is clear, under it, so that menus and windows
* over this one stay in front.
* -----------------------------------------------------------------------------
draw_bar:
	movem.l	d0-d2/a0-a1, -(sp)
	move.l	win_ptr(a5), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	move.w	#G_BLACK, -(sp)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp
	move.w	#G_LGRAY, -(sp)
	SXCALL	GMBackColor		; $A148
	addq.l	#2, sp
	move.w	#$0100, -(sp)		; G_BACK << 8 | G_PSET
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	bar_rect(a5)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; G_FORE << 8 | G_PSET
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	bar_line_rect(a5)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	* Clear in all four planes: windows draw in planes 0 and 1 over plane 3 set
	move.w	#$000F, -(sp)		; G_ALLPAGE
	SXCALL	GMAPage			; $A149
	addq.l	#2, sp
	move.w	d0, d2
	move.w	#G_THRU, -(sp)
	SXCALL	GMBackColor		; $A148
	addq.l	#2, sp
	move.w	#$0100, -(sp)		; G_BACK << 8 | G_PSET
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	screen_rect(a5)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	move.w	d2, -(sp)
	SXCALL	GMAPage			; $A149
	addq.l	#2, sp

	* The Configuration icon, a plot image whose planes follow its rectangle
	tst.b	library_open(a5)
	beq.s	draw_bar_done
	move.w	#CONFIG_ICON_ID, -(sp)
	move.l	#'PLOT', -(sp)
	SXCALL	TSRscGet		; $A4AB
	addq.l	#6, sp
	move.l	a0, d0
	beq.s	draw_bar_done
	movea.l	(a0), a0
	clr.w	-(sp)			; two planes, plain
	pea	config_icon_rect(pc)
	pea	8(a0)
	SXCALL	GMPlotImg		; $A182
	lea	10(sp), sp
draw_bar_done:
	bsr	draw_floppies
	bsr	draw_lamp
	movem.l	(sp)+, d0-d2/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper: update_status
* Asks the host for the VM's state: sizes the window to the VM's screen when the
* VM changed its mode, and draws the access lamp if it changed.
* -----------------------------------------------------------------------------
update_status:
	movem.l	d0-d3/a1, -(sp)
	moveq.l	#MSX68K_FN_VM_STATUS, d0
	move.l	vm_id(a5), d1
	bsr	msx68k_call
	tst.l	d0
	bmi	update_lamp_done
	tst.l	d1
	beq.s	update_lamp
	cmp.l	screen_size(a5), d1
	beq.s	update_lamp
	bsr	resize_window
update_lamp:
	move.w	d3, vm_boot(a5)
	cmp.l	fd_state(a5), d2
	beq.s	update_media_done
	move.l	d2, fd_state(a5)
	bsr	draw_floppies
update_media_done:
	btst	#2, d0			; the primary's screen changed size
	beq.s	update_lamp_sram
	bsr	redraw_window
update_lamp_sram:
	btst	#1, d0			; SRAM to save
	beq.s	update_lamp_bit
	bsr	take_sram
update_lamp_bit:
	andi.b	#1, d0			; the lamp
	cmp.b	lamp_lit(a5), d0
	beq.s	update_lamp_done
	lea	lamp_lit(a5), a1
	move.b	d0, (a1)
	bsr	draw_lamp
update_lamp_done:
	movem.l	(sp)+, d0-d3/a1
	rts

* -----------------------------------------------------------------------------
* Helper: redraw_window
* Hides and shows the window again, so that SX-Window works out its visible region
* and draws it anew, frame and content, after the primary's screen changed size:
* what the old screen cut off was never drawn.
* -----------------------------------------------------------------------------
redraw_window:
	movem.l	d0-d2/a0-a1, -(sp)
	move.l	win_ptr(a5), -(sp)
	SXCALL	WMHide			; $A208
	SXCALL	WMShow			; $A207
	addq.l	#4, sp
	movem.l	(sp)+, d0-d2/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper:take_sram
* Takes the VM's SRAM that changed from the host and saves it in the setup.
* -----------------------------------------------------------------------------
take_sram:
	movem.l	d0-d3/a0-a1, -(sp)
	moveq.l	#MSX68K_FN_VM_SRAM, d0
	move.l	vm_id(a5), d1
	lea	sram_buffer(a5), a0
	move.l	a0, d2
	move.l	#SRAM_SIZE, d3
	bsr	msx68k_call
	cmpi.l	#SRAM_SIZE, d0
	bne.s	take_sram_done
	lea	has_sram(a5), a1
	st	(a1)
	bsr	save_setup
take_sram_done:
	movem.l	(sp)+, d0-d3/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper:take_paths
* From this task's record in disk_buffer, makes library_path, the program's name
* with .LB for its extension, and setup_path, the command line's first word or
* the default setup.
* -----------------------------------------------------------------------------
take_paths:
	movem.l	d0-d2/a0-a2, -(sp)
	lea	disk_buffer(a5), a0	; Task.name
	lea	library_path(a5), a1
	suba.l	a2, a2			; after the last '.'
	moveq.l	#PATH_MAX-4, d1
take_name:
	move.b	(a0)+, d0
	move.b	d0, (a1)+
	beq.s	take_name_done
	cmpi.b	#'.', d0
	bne.s	take_name_next
	movea.l	a1, a2
take_name_next:
	dbra	d1,take_name
	clr.b	(a1)
take_name_done:
	move.l	a2, d0
	beq.s	take_command
	move.b	#'L', (a2)+
	move.b	#'B', (a2)+
	clr.b	(a2)

take_command:
	* A newly created setup opens its configuration dialog in its own task.
	lea	disk_buffer+TASK_COMMAND(a5),a0
	moveq.l	#0,d0
	move.b	(a0),d0
	cmpi.w	#6,d0
	bcs.s	take_command_start
	lea	-5(a0,d0.w),a1
	lea	setup_option(pc),a2
	moveq.l	#5,d0
take_option_compare:
	cmpm.b	(a1)+,(a2)+
	bne.s	take_command_start
	dbra	d0,take_option_compare
	st	start_config(a5)
take_command_start:
	lea	disk_buffer+TASK_COMMAND(a5), a0	; LASCII: its length, then its text
	moveq.l	#0, d1
	move.b	(a0)+, d1
	lea	setup_path(a5), a1
take_command_skip:
	subq.w	#1, d1
	bmi.s	take_command_default
	move.b	(a0)+, d0
	cmpi.b	#' ', d0
	bls.s	take_command_skip
	moveq.l	#PATH_MAX-2,d0
	cmpi.b	#34,-1(a0)
	beq.s	take_quoted_copy
take_command_copy:
	move.b	-1(a0),(a1)+
	subq.w	#1,d1
	bmi.s	take_command_end
	cmpi.b	#' ',(a0)+
	bls.s	take_command_end
	dbra	d0,take_command_copy
	bra.s	take_command_end
take_quoted_copy:
	subq.w	#1,d1
	bmi.s	take_command_end
	move.b	(a0)+,d2
	cmpi.b	#34,d2
	beq.s	take_command_end
	move.b	d2,(a1)+
	dbra	d0,take_quoted_copy
take_command_end:
	clr.b	(a1)
	bra.s	take_paths_done
take_command_default:
	lea	default_setup_path(pc), a0
take_command_default_copy:
	move.b	(a0)+, (a1)+
	bne.s	take_command_default_copy
	lea	setup_is_default(a5), a1
	st	(a1)
take_paths_done:
	movem.l	(sp)+, d0-d2/a0-a2
	rts

* -----------------------------------------------------------------------------
* Helper: load_setup
* Reads the VM's speed and SRAM from its setup, and drops the library unwritten.
* Sets setup_found when the setup is there.
* -----------------------------------------------------------------------------
load_setup:
	movem.l	d0-d1/a0-a1, -(sp)
	pea	setup_path(a5)
	SXCALL	TSResOpen		; $A4A1
	addq.l	#4, sp
	move.l	a0, d0
	beq	load_setup_done
	SXCALL	TSResLoad		; $A4A5
	lea	setup_found(a5), a1
	st	(a1)

	clr.w	-(sp)
	move.l	#SETUP_TYPE, -(sp)
	SXCALL	TSRscGet		; $A4AB
	addq.l	#6, sp
	move.l	a0, d0
	beq.s	load_setup_sram
	movea.l	(a0), a0
	move.w	2(a0), vm_speed(a5)
	cmpi.w	#2, (a0)		; version 2 has the CPU and the RAM
	bcs.s	load_setup_speed
	move.w	4(a0), d0
	cmpi.w	#1, d0
	bhi.s	load_setup_ram
	move.w	d0, vm_cpu(a5)
load_setup_ram:
	move.w	6(a0), d0
	beq.s	load_setup_speed
	cmpi.w	#12, d0
	bhi.s	load_setup_speed
	move.w	d0, vm_ram(a5)
load_setup_speed:
	* A speed the CPU does not have is its first
	move.w	vm_cpu(a5), d0
	bsr	speed_list
	cmp.w	vm_speed(a5), d1
	bhi.s	load_setup_sram
	clr.w	vm_speed(a5)

load_setup_sram:
	clr.w	-(sp)
	move.l	#SRAM_TYPE, -(sp)
	SXCALL	TSRscGet		; $A4AB
	addq.l	#6, sp
	move.l	a0, d0
	beq.s	load_setup_close
	movea.l	(a0), a0
	lea	sram_buffer(a5), a1
	move.w	#SRAM_SIZE/4-1, d1
load_setup_copy:
	move.l	(a0)+, (a1)+
	dbra	d1, load_setup_copy
	lea	has_sram(a5), a1
	st	(a1)

load_setup_close:
	clr.w	-(sp)
	move.l	#DRIVES_TYPE, -(sp)
	SXCALL	TSRscGet		; $A4AB
	addq.l	#6, sp
	move.l	a0, d0
	beq.s	load_setup_remove
	movea.l	(a0), a0
	lea	vm_drives(a5), a1
	move.w	#DRIVES_SIZE/2-1, d1
load_setup_drives:
	move.w	(a0)+, (a1)+
	dbra	d1, load_setup_drives
load_setup_remove:
	SXCALL	TSResRemove		; $A4A4
load_setup_done:
	movem.l	(sp)+, d0-d1/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper: save_setup
* Writes the VM's speed and, once there is one, its SRAM to its setup, making the
* setup, and for the default one its directory, when it is not there.
* -----------------------------------------------------------------------------
save_setup:
	movem.l	d0-d3/a0-a1, -(sp)
	tst.b	setup_is_default(a5)
	beq.s	save_setup_open
	pea	default_setup_dir(pc)
	DOS	_MKDIR			; there already, it fails, as it may
	addq.l	#4, sp
save_setup_open:
	lea	setup_rec(a5), a0
	move.w	#SETUP_VERSION, (a0)
	move.w	vm_speed(a5), 2(a0)
	move.w	vm_cpu(a5), 4(a0)
	move.w	vm_ram(a5), 6(a0)
	lea	setup_handle(a5), a1	; pseudo-handles to the records
	move.l	a0, (a1)+
	lea	sram_buffer(a5), a0
	move.l	a0, (a1)

	pea	setup_path(a5)
	SXCALL	TSResOpen		; $A4A1
	addq.l	#4, sp
	move.l	a0, d0
	bne.s	save_setup_add
	SXCALL	TSResNew		; $A4A0
save_setup_add:
	clr.w	-(sp)
	move.l	#SETUP_TYPE, -(sp)
	SXCALL	TSRscRemove		; $A4AC
	addq.l	#6, sp
	move.l	#SETUP_SIZE, -(sp)
	pea	setup_handle(a5)
	clr.w	-(sp)
	move.l	#SETUP_TYPE, -(sp)
	SXCALL	TSRscAdd		; $A4AA
	lea	14(sp), sp

	lea	vm_drives(a5), a0
	move.l	a0, drives_handle(a5)
	clr.w	-(sp)
	move.l	#DRIVES_TYPE, -(sp)
	SXCALL	TSRscRemove		; $A4AC
	addq.l	#6, sp
	move.l	#DRIVES_SIZE, -(sp)
	pea	drives_handle(a5)
	clr.w	-(sp)
	move.l	#DRIVES_TYPE, -(sp)
	SXCALL	TSRscAdd		; $A4AA
	lea	14(sp), sp

	tst.b	has_sram(a5)
	beq.s	save_setup_close
	clr.w	-(sp)
	move.l	#SRAM_TYPE, -(sp)
	SXCALL	TSRscRemove		; $A4AC
	addq.l	#6, sp
	move.l	#SRAM_SIZE, -(sp)
	pea	sram_handle(a5)
	clr.w	-(sp)
	move.l	#SRAM_TYPE, -(sp)
	SXCALL	TSRscAdd		; $A4AA
	lea	14(sp), sp

save_setup_close:
	pea	setup_path(a5)
	SXCALL	TSResClose		; $A4A2
	addq.l	#4, sp
	lea	setup_found(a5), a1
	st	(a1)
	movem.l	(sp)+, d0-d3/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper: resize_window
* Sizes the window to a VM screen of D1.L = width << 16 | height below the bar,
* moving what is drawn by the right edge, and draws the bar anew.
* -----------------------------------------------------------------------------
resize_window:
	movem.l	d0-d2/a1, -(sp)
	lea	screen_size(a5), a1
	move.l	d1, (a1)
	move.w	d1, d2			; height
	swap	d1			; width
	lea	bar_rect(a5), a1
	move.w	d1, 4(a1)
	lea	bar_line_rect(a5), a1
	move.w	d1, 4(a1)
	lea	screen_rect(a5), a1
	move.w	d1, 4(a1)
	move.w	d2, d0
	addi.w	#BAR_HEIGHT, d0
	move.w	d0, 6(a1)
	* The lamp, 19 dots in from the right
	lea	lamp_rects(a5), a1
	move.w	d1, d0
	subi.w	#19, d0
	move.w	d0, 0(a1)
	addq.w	#1, d0
	move.w	d0, 8(a1)
	move.w	d0, 16(a1)
	move.w	d1, d0
	subq.w	#5, d0
	move.w	d0, 4(a1)
	move.w	d0, 12(a1)
	subq.w	#1, d0
	move.w	d0, 20(a1)

	move.w	#-1, -(sp)		; fUpdate: draw it anew
	move.w	d2, d0			; sizePt: the height with the bar in its low word,
	addi.w	#BAR_HEIGHT, d0
	move.w	d0, -(sp)
	move.w	d1, -(sp)		; and the width in its high word
	move.l	win_ptr(a5), -(sp)
	SXCALL	WMSize			; $A203
	lea	10(sp), sp
	* Shrinking exposes nothing to update: the lamp has moved all the same
	bsr	draw_bar
	movem.l	(sp)+, d0-d2/a1
	rts

* -----------------------------------------------------------------------------
* Helper: draw_lamp
* Draws the access lamp at the right of the bar, red while lit, in a recess as
* the icons are. The colors need all four planes.
* -----------------------------------------------------------------------------
draw_lamp:
	movem.l	d0-d2/a0-a1, -(sp)
	move.l	win_ptr(a5), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	move.w	#$000F, -(sp)		; G_ALLPAGE
	SXCALL	GMAPage			; $A149
	addq.l	#2, sp
	move.w	d0, d2
	clr.w	-(sp)			; G_FORE << 8 | G_PSET
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	lea	lamp_rects(a5), a1
	move.w	#G_BLACK, d0
	bsr.s	draw_lamp_fill
	move.w	#G_WHITE, d0
	bsr.s	draw_lamp_fill
	move.w	#G_DGRAY, d0
	tst.b	lamp_lit(a5)
	beq.s	draw_lamp_face
	move.w	#G_RED, d0
draw_lamp_face:
	bsr.s	draw_lamp_fill
	move.w	d2, -(sp)
	SXCALL	GMAPage			; $A149
	addq.l	#2, sp
	movem.l	(sp)+, d0-d2/a0-a1
	rts
* Fills the rectangle at A1 with color D0.W and moves A1 to the next
draw_lamp_fill:
	move.w	d0, -(sp)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp
	move.l	a1, -(sp)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	addq.l	#8, a1
	rts

* -----------------------------------------------------------------------------
* Helper: config_menu
* Pops up the Configuration menu where the right button went down: "設定...",
* the configuration dialog, and the name of the CPU the VM runs with, its speeds
* as a submenu, the current one checked. A speed chosen is applied to the VM at
* once.
* -----------------------------------------------------------------------------
config_menu:
	movem.l	d0-d4/a0-a2, -(sp)
	tst.b	library_open(a5)
	beq	config_menu_done

	* Check the current speed: rebuild the submenu from its items, with '!' before one
	move.w	run_cpu(a5), d0
	bsr	speed_list
	move.w	d1, d4			; how many
	lea	menu_string(a5), a1
	moveq.l	#0, d1
config_speed_item:
	cmp.w	vm_speed(a5), d1
	bne.s	config_speed_copy
	move.b	#'!', (a1)+
config_speed_copy:
	move.b	(a0)+, d0
	beq.s	config_speed_next
	move.b	d0, (a1)+
	bra.s	config_speed_copy
config_speed_next:
	addq.w	#1, d1
	cmp.w	d4, d1
	bcc.s	config_speed_built
	move.b	#',', (a1)+
	bra.s	config_speed_item
config_speed_built:
	clr.b	(a1)

	* The menu: its head, then the CPU's name
	lea	config_string(a5), a1
	lea	config_menu_head(pc), a0
config_head_copy:
	move.b	(a0)+, (a1)+
	bne.s	config_head_copy
	subq.l	#1, a1
	lea	cpu_names(pc), a0
	move.w	run_cpu(a5), d0
	mulu	#6, d0
	adda.w	d0, a0
config_name_copy:
	move.b	(a0)+, (a1)+
	bne.s	config_name_copy
	subq.l	#1,a1
	tst.l	fd_state(a5)
	beq.s	config_no_boot
	lea	boot_menu_head(pc),a0
config_boot_head:
	move.b	(a0)+,(a1)+
	bne.s	config_boot_head
	subq.l	#1,a1
config_no_boot:
	lea	new_menu_item(pc),a0
config_new_head:
	move.b	(a0)+,(a1)+
	bne.s	config_new_head
	bsr	build_boot_menu
	move.w	#SPEED_MENU_ID, -(sp)
	move.l	#'MENU', -(sp)
	SXCALL	TSRscGet		; $A4AB
	addq.l	#6, sp
	move.l	a0, d0
	beq	config_menu_done
	clr.w	-(sp)			; MI_PLN
	pea	menu_string(a5)
	move.l	a0, -(sp)
	SXCALL	MNConvert2		; $A26B
	lea	10(sp), sp

	clr.w	-(sp)			; MI_PLN
	pea	config_string(a5)
	clr.l	-(sp)			; a new record
	SXCALL	MNConvert2		; $A26B
	lea	10(sp), sp
	move.l	a0, d0
	beq	config_menu_done
	movea.l	a0, a2
	move.l	event_where(a5), -(sp)
	move.l	a2, -(sp)
	SXCALL	MNSelect		; $A268
	addq.l	#8, sp
	move.l	d0, d3
	move.l	a2, -(sp)
	SXCALL	MMHdlDispose		; $A038
	addq.l	#4, sp

	* "設定...", item 1 of the menu itself, opens the dialog
	cmpi.l	#1, d3
	bne.s	config_menu_speed
	bsr	config_dialog
	bra	config_menu_done
config_menu_speed:
	moveq.l	#3,d0
	tst.l	fd_state(a5)
	beq.s	config_new_number
	addq.w	#1,d0
config_new_number:
	cmp.l	d0,d3
	bne.s	config_not_new
	bsr	new_setup
	bra	config_menu_done
config_not_new:
	move.l	d3,d0
	swap	d0
	cmpi.w	#2,d0
	bne.s	config_not_boot
	moveq.l	#0,d2
	move.w	d3,d2
	subq.w	#1,d2
	movea.l	sram_buffer+12(a5),a1
	cmpi.w	#13,d2
	bne.s	config_boot_apply
	bsr	rom_dialog
	tst.l	d0
	bmi	config_menu_done
config_boot_apply:
	moveq.l	#MSX68K_FN_VM_BOOT,d0
	move.l	vm_id(a5),d1
	bsr	msx68k_call
	tst.l	d0
	bne	config_menu_done
	move.w	d2,vm_boot(a5)
	bsr	take_sram
	bra	config_menu_done
config_not_boot:
	* A speed: the submenu's ID in the high word and its item, from 1, in the low
	move.l	d3, d0
	swap	d0
	cmpi.w	#SPEED_MENU_ID, d0
	bne	config_menu_done
	move.w	d3, d2
	subq.w	#1, d2
	cmp.w	d4, d2
	bcc.s	config_menu_done
	ext.l	d2
	moveq.l	#MSX68K_FN_VM_SPEED, d0
	move.l	vm_id(a5), d1
	bsr	msx68k_call
	tst.l	d0
	bne	config_menu_done
	lea	vm_speed(a5), a1
	move.w	d2, (a1)
	bsr	save_setup
config_menu_done:
	movem.l	(sp)+, d0-d4/a0-a2
	rts

* -----------------------------------------------------------------------------
* Helper: speed_list
* For the CPU D0.W (0 the 68000, 1 the 68030), returns its speeds' names in A0
* and how many in D1.W.
* -----------------------------------------------------------------------------
speed_list:
	lea	speed_items_68000(pc), a0
	moveq.l	#4, d1
	tst.w	d0
	beq.s	speed_list_done
	lea	speed_items_68030(pc), a0
	moveq.l	#2, d1
speed_list_done:
	rts

* -----------------------------------------------------------------------------
* Helper: config_dialog
* The configuration dialog: the CPU's kind and the memory. What 設定 changes is
* saved in the setup, and, as it takes effect when the VM restarts, an alert asks
* whether to restart it now.
* -----------------------------------------------------------------------------
config_dialog:
	movem.l	d0-d3/a0-a2, -(sp)
	clr.w	dialog_end(a5)
	move.w	vm_cpu(a5), dlg_cpu(a5)
	move.w	vm_ram(a5), dlg_ram(a5)
	move.w	#cfg_dialog, cfg_dialog+DIALOG_RECORD(a5)
	move.l	#-1, -(sp)		; in front
	pea	cfg_dialog(a5)		; its record in the work area
	move.w	#CONFIG_DIALOG_ID, -(sp)
	SXCALL	DMRefer			; $A2C4
	lea	10(sp), sp
	move.l	a0, d0
	beq	config_dialog_done
	move.l	app_task_id(a5),WINDOW_TASK(a0)	; route events to this task
	bclr	#1,WINDOW_KIND+1(a0)		; ordinary task window: do not lock application switching
	move.l	a0, dlg_ptr(a5)
	lea	config_labels(pc), a1
	bsr	dlg_labels
	lea	vm_drives(a5), a0
	lea	dlg_drives(a5), a1
	bsr	copy_drives
	bsr	dlg_show_mode
	bsr	dlg_show_cpu
	moveq.l	#ITEM_MEMORY, d0
	bsr	dlg_item
	move.w	dlg_ram(a5), -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueSet		; $A290
	addq.l	#6, sp
	bsr	dlg_show_size
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	WMShow			; $A207
	SXCALL	WMSelect
	SXCALL	GMSetGraph		; $A131
	SXCALL	DMDraw			; $A2C8
	addq.l	#4, sp

config_dialog_loop:
	bsr	dialog_control
	cmpi.w	#ITEM_OK, d0
	beq	config_dialog_ok
	cmpi.w	#ITEM_CANCEL, d0
	beq	config_dialog_close
	cmpi.w	#ITEM_68000, d0
	beq	config_dialog_68000
	cmpi.w	#ITEM_68030, d0
	beq	config_dialog_68030
	cmpi.w	#ITEM_SHARED, d0
	beq	config_dialog_shared
	cmpi.w	#ITEM_CUSTOM, d0
	beq	config_dialog_custom
	cmpi.w	#ITEM_DETAILS, d0
	beq	config_dialog_details
	cmpi.w	#ITEM_MEMORY, d0
	bne	config_dialog_loop
	moveq.l	#ITEM_MEMORY, d0
	bsr	dlg_item
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueGet		; $A291
	addq.l	#4, sp
	cmpi.w	#1, d0
	bge.s	config_dialog_min
	moveq.l	#1, d0
config_dialog_min:
	cmpi.w	#12, d0
	ble.s	config_dialog_max
	moveq.l	#12, d0
config_dialog_max:
	move.w	d0, dlg_ram(a5)
	bsr	dlg_show_size
	* DITSet does not draw the text anew
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	DMDraw			; $A2C8
	addq.l	#4, sp
	bra	config_dialog_loop
config_dialog_shared:
	clr.w	dlg_drives+DRV_MODE(a5)
	bsr	dlg_show_mode
	bra	config_dialog_loop
config_dialog_custom:
	move.w	#1, dlg_drives+DRV_MODE(a5)
	bsr	dlg_show_mode
	bra	config_dialog_loop
config_dialog_details:
	tst.w	dlg_drives+DRV_MODE(a5)
	beq	config_dialog_loop
	bsr	drive_dialog
	tst.w	dialog_end(a5)
	bne	config_dialog_close
	bra	config_dialog_loop
config_dialog_68000:
	clr.w	dlg_cpu(a5)
	bsr	dlg_show_cpu
	bra	config_dialog_loop
config_dialog_68030:
	move.w	#1, dlg_cpu(a5)
	bsr	dlg_show_cpu
	bra	config_dialog_loop

config_dialog_ok:
	bsr	dlg_dispose
	move.w	dlg_cpu(a5), d0
	move.w	dlg_ram(a5), d1
	cmp.w	vm_cpu(a5), d0
	bne.s	config_dialog_changed
	cmp.w	vm_ram(a5), d1
	bne.s	config_dialog_changed
	bsr	drives_differ
	beq	config_dialog_done
config_dialog_changed:
	lea	dlg_drives(a5), a0
	lea	vm_drives(a5), a1
	bsr	copy_drives
	move.w	dlg_cpu(a5), d0
	move.w	dlg_ram(a5), d1
	* Another CPU starts at its first speed
	cmp.w	vm_cpu(a5), d0
	beq.s	config_dialog_save
	clr.w	vm_speed(a5)
config_dialog_save:
	move.w	d0, vm_cpu(a5)
	move.w	d1, vm_ram(a5)
	bsr	save_setup
	pea	restart_message(pc)
	move.w	#D_YESNO, -(sp)
	SXCALL	DMError			; $A2F6
	addq.l	#6, sp
	cmpi.w	#1, d0			; はい
	bne	config_dialog_done
	bsr	close_custom_drives
	bsr	open_custom_drives
	moveq.l	#MSX68K_FN_VM_RESTART, d0
	move.l	vm_id(a5), d1
	move.w	vm_cpu(a5), d2
	swap	d2
	move.w	vm_ram(a5), d2
	move.w	vm_speed(a5), d3
	ext.l	d3
	bsr	custom_drive_table
	bsr	msx68k_call
	tst.l	d0
	bne	config_dialog_done
	move.w	vm_cpu(a5), run_cpu(a5)
	bra	config_dialog_done
config_dialog_close:
	bsr	dlg_dispose
config_dialog_done:
	move.l	win_ptr(a5), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	movem.l	(sp)+, d0-d3/a0-a2
	tst.w	dialog_end(a5)
	bne	quit_app
	rts

* The running VM's image handles belong to this task, independently of the saved
* choices (which can change without a restart). A2 is zero for shared mode.
custom_drive_table:
	suba.l	a2, a2
	tst.w	vm_drives+DRV_MODE(a5)
	beq.s	custom_drive_table_done
	lea	drive_table(a5), a2
	* Older guests leave A2 unspecified. Tag this extension explicitly.
	move.l	a2,-(sp)
	move.b	#$56,(sp)
	movea.l	(sp)+,a2
custom_drive_table_done:
	rts

open_custom_drives:
	movem.l	d0-d7/a0-a4, -(sp)
	lea	vm_drives(a5),a0
	lea	run_drives(a5),a1
	bsr	copy_drives
	* Flush primary file-system writes before VmOpen/VmRestart lends any FD.
	DOS	_FFLUSH
	move.l	d0, pass_flush_result(a5)
	clr.w	drive_table(a5)
	tst.w	vm_drives+DRV_MODE(a5)
	beq	open_custom_done
	move.w	vm_drives+DRV_COUNT(a5), d7
	cmpi.w	#DRIVE_ROWS, d7
	bls	open_custom_count
	moveq.l	#DRIVE_ROWS, d7
open_custom_count:
	move.w	d7, drive_table(a5)
	subq.w	#1, d7
	bmi	open_custom_done
	lea	vm_drives+DRV_ENTRIES(a5), a3
	lea	drive_table+2(a5), a4
open_custom_next:
	move.w	(a3), (a4)
	move.w	#-1, 2(a4)
	clr.l	4(a4)
	tst.w	(a3)
	bne	open_custom_letter
	moveq.l	#0, d1
	move.b	2(a3), d1
	cmpi.w	#PATH_MAX, d1
	bhi	open_custom_advance
	lea	3(a3), a0
	lea	image_path(a5), a1
	bra	open_custom_copy_test
open_custom_copy:
	move.b	(a0)+, (a1)+
open_custom_copy_test:
	dbra	d1, open_custom_copy
	clr.b	(a1)
	move.w	#2, -(sp)
	pea	image_path(a5)
	DOS	_OPEN
	addq.l	#6, sp
	tst.l	d0
	bmi	open_custom_advance
	move.w	d0, 2(a4)
	move.w	#2, -(sp)
	clr.l	-(sp)
	move.w	d0, -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bpl	open_custom_size
open_custom_bad_file:
	move.w	2(a4), -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
	move.w	#-1, 2(a4)
	bra	open_custom_advance
open_custom_size:
	move.l	d0,4(a4)
	moveq.l	#0,d3
	move.w	2(a4),d3
	bsr	probe_image
	tst.l	d0
	bmi	open_custom_bad_file
	cmpi.l	#1,d0
	bne	open_custom_advance
	move.w	#3,(a4)		; a floppy identified by the shared parser
	bra	open_custom_advance
open_custom_letter:
	cmpi.w	#2, (a3)
	bne	open_custom_letter_ok
	tst.l	pass_flush_result(a5)
	bmi	open_custom_advance
open_custom_letter_ok:
	moveq.l	#0, d0
	move.b	3(a3), d0
	andi.w	#$DF, d0
	subi.w	#'A'-1, d0
	move.w	d0, 2(a4)
open_custom_advance:
	lea	ENTRY_SIZE(a3), a3
	addq.l	#8, a4
	dbra	d7, open_custom_next
open_custom_done:
	movem.l	(sp)+, d0-d7/a0-a4
	rts

close_custom_drives:
	movem.l	d0/d7/a4, -(sp)
	move.w	drive_table(a5), d7
	clr.w	drive_table(a5)
	subq.w	#1, d7
	bmi	close_custom_done
	lea	drive_table+2(a5), a4
close_custom_next:
	cmpi.w	#3,(a4)
	beq	close_custom_file
	tst.w	(a4)
	bne	close_custom_advance
close_custom_file:
	move.w	2(a4), d0
	bmi	close_custom_advance
	move.w	d0, -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
close_custom_advance:
	addq.l	#8, a4
	dbra	d7, close_custom_next
close_custom_done:
	moveq.l	#3,d7
	lea	pending_old(a5),a4
close_pending:
	move.w	(a4),d0
	move.w	#-1,(a4)+
	tst.w	d0
	bmi	close_pending_next
	move.w	d0,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
close_pending_next:
	dbra	d7,close_pending
	clr.l	media_busy(a5)
	movem.l	(sp)+, d0/d7/a4
	rts

* Copies the drives at A0 to A1
copy_drives:
	movem.l	d0/a0-a1, -(sp)
	move.w	#DRIVES_SIZE/2-1, d0
copy_drives_word:
	move.w	(a0)+, (a1)+
	dbra	d0, copy_drives_word
	movem.l	(sp)+, d0/a0-a1
	rts

* Sets Z when dlg_drives are the setup's
drives_differ:
	movem.l	d0/a0-a1, -(sp)
	lea	dlg_drives(a5), a0
	lea	vm_drives(a5), a1
	move.w	#DRIVES_SIZE/2-1, d0
drives_differ_word:
	cmpm.w	(a0)+, (a1)+
	dbne	d0, drives_differ_word
	movem.l	(sp)+, d0/a0-a1
	rts

* Sets the dialog's static texts from the table at A1: item.b, then a Pascal
* string, up to item 0
dlg_labels:
	movem.l	d0/a1, -(sp)
dlg_labels_next:
	moveq.l	#0, d0
	move.b	(a1)+, d0
	beq.s	dlg_labels_done
	bsr	dlg_item
	move.l	a1, -(sp)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITSet			; $A2D2
	lea	10(sp), sp
	moveq.l	#0, d0
	move.b	(a1), d0
	lea	1(a1, d0.w), a1
	bra.s	dlg_labels_next
dlg_labels_done:
	movem.l	(sp)+, d0/a1
	rts

* Sets the ドライブ row to dlg_drives' mode: its switches, and 詳細... inactive in
* shared mode
dlg_show_mode:
	movem.l	d0-d2/a0-a1, -(sp)
	moveq.l	#ITEM_SHARED, d0
	bsr	dlg_item
	moveq.l	#0, d1
	tst.w	dlg_drives+DRV_MODE(a5)
	seq	d1
	andi.w	#1, d1
	move.w	d1, -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueSet		; $A290
	addq.l	#6, sp
	moveq.l	#ITEM_CUSTOM, d0
	bsr	dlg_item
	move.w	dlg_drives+DRV_MODE(a5), -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueSet		; $A290
	addq.l	#6, sp
	moveq.l	#ITEM_DETAILS, d0
	bsr	dlg_item
	move.w	#255, d1		; inactive
	tst.w	dlg_drives+DRV_MODE(a5)
	beq.s	dlg_show_mode_shine
	moveq.l	#0, d1
dlg_show_mode_shine:
	move.w	d1, -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMShine			; $A298
	addq.l	#6, sp
	movem.l	(sp)+, d0-d2/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper: drive_dialog
* ドライブの詳細, custom mode's drives: how many, and each one's kind and path or
* drive letter. 設定 keeps the changes in dlg_drives, for the configuration
* dialog's 設定 to save.
* -----------------------------------------------------------------------------
drive_dialog:
	movem.l	d0-d4/a0-a2, -(sp)
	clr.w	dlg_edit(a5)
	lea	dlg_drives(a5), a0
	lea	det_drives(a5), a1
	bsr	copy_drives
	move.l	dlg_ptr(a5), cfg_ptr(a5)
	move.w	#det_dialog, det_dialog+DIALOG_RECORD(a5)
	move.l	#-1, -(sp)
	pea	det_dialog(a5)
	move.w	#DRIVE_DIALOG_ID, -(sp)
	SXCALL	DMRefer			; $A2C4
	lea	10(sp), sp
	move.l	a0, d0
	beq	drive_dialog_back
	move.l	app_task_id(a5),WINDOW_TASK(a0)	; route events to this task
	bclr	#1,WINDOW_KIND+1(a0)		; ordinary task window: do not lock application switching
	move.l	a0, dlg_ptr(a5)
	lea	drive_labels(pc), a1
	bsr	dlg_labels
	moveq.l	#0, d3
drive_dialog_row:
	* The row's number, kind, and text
	lea	dlg_text(a5), a0
	move.b	#1, (a0)
	move.b	d3, d0
	addi.b	#'1', d0
	move.b	d0, 1(a0)
	bsr	row_item
	move.w	d4, d0
	bsr	dlg_item
	pea	dlg_text(a5)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITSet			; $A2D2
	lea	10(sp), sp
	move.w	d4, d0
	addq.w	#4, d0
	bsr	dlg_item
	bsr	row_entry
	pea	2(a2)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITSet			; $A2D2
	lea	10(sp), sp
	bsr	det_show_kind
	addq.w	#1, d3
	cmpi.w	#DRIVE_ROWS, d3
	bcs.s	drive_dialog_row
	bsr	det_show_count
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	WMShow			; $A207
	SXCALL	WMSelect
	SXCALL	GMSetGraph		; $A131
	SXCALL	DMDraw			; $A2C8
	addq.l	#4, sp

drive_dialog_loop:
	bsr	dialog_control
	cmpi.w	#ITEM_OK, d0
	beq	drive_dialog_ok
	cmpi.w	#ITEM_CANCEL, d0
	beq	drive_dialog_close
	cmpi.w	#ITEM_FEWER, d0
	beq.s	drive_dialog_fewer
	cmpi.w	#ITEM_MORE, d0
	beq.s	drive_dialog_more
	subi.w	#ITEM_ROWS, d0
	bcs.s	drive_dialog_loop
	ext.l	d0
	divu	#ROW_ITEMS, d0
	move.w	d0, d3			; the row
	swap	d0			; its item: 1 to 3 the kinds
	subq.w	#1, d0
	cmpi.w	#3, d0
	bcc.s	drive_dialog_loop
	bsr	row_entry
	move.w	d0, (a2)
	bsr	det_show_kind
	bra.s	drive_dialog_loop
drive_dialog_fewer:
	cmpi.w	#1, det_drives+DRV_COUNT(a5)
	bls.s	drive_dialog_loop
	subq.w	#1, det_drives+DRV_COUNT(a5)
	bra.s	drive_dialog_count
drive_dialog_more:
	cmpi.w	#DRIVE_ROWS, det_drives+DRV_COUNT(a5)
	bcc	drive_dialog_loop
	addq.w	#1, det_drives+DRV_COUNT(a5)
drive_dialog_count:
	bsr	det_show_count
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	DMDraw			; $A2C8
	addq.l	#4, sp
	bra	drive_dialog_loop

drive_dialog_ok:
	bsr	dialog_store_edit
	* Each row's text, up to 92 bytes
	moveq.l	#0, d3
drive_dialog_take:
	bsr	row_item
	move.w	d4, d0
	addq.w	#4, d0
	bsr	dlg_item
	pea	dlg_text(a5)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITGet			; $A2D1
	lea	10(sp), sp
	bsr	row_entry
	lea	dlg_text(a5), a0
	moveq.l	#0, d0
	move.b	(a0), d0
	cmpi.w	#92, d0
	bls.s	drive_dialog_trim
	moveq.l	#92, d0
drive_dialog_trim:
	* Without the spaces the field ends with
	tst.w	d0
	beq.s	drive_dialog_len
	cmpi.b	#' ', (a0, d0.w)
	bne.s	drive_dialog_len
	subq.w	#1, d0
	bra.s	drive_dialog_trim
drive_dialog_len:
	lea	2(a2), a1
	move.b	d0, (a1)+
	addq.l	#1, a0
	bra.s	drive_dialog_copy_next
drive_dialog_copy:
	move.b	(a0)+, (a1)+
drive_dialog_copy_next:
	dbra	d0, drive_dialog_copy
	addq.w	#1, d3
	cmpi.w	#DRIVE_ROWS, d3
	bcs.s	drive_dialog_take
	lea	det_drives(a5), a0
	lea	dlg_drives(a5), a1
	bsr	copy_drives
drive_dialog_close:
	bsr	dlg_dispose
drive_dialog_back:
	move.l	cfg_ptr(a5), dlg_ptr(a5)
	clr.l	cfg_ptr(a5)
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	WMSelect
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	movem.l	(sp)+, d0-d4/a0-a2
	rts

* D4.W: the first item of row D3.W
row_item:
	move.w	d3, d4
	mulu	#ROW_ITEMS, d4
	addi.w	#ITEM_ROWS, d4
	rts

* A2: det_drives' entry of row D3.W
row_entry:
	move.l	d0, -(sp)
	move.w	d3, d0
	mulu	#ENTRY_SIZE, d0
	lea	det_drives+DRV_ENTRIES(a5), a2
	adda.l	d0, a2
	move.l	(sp)+, d0
	rts

* Sets row D3.W's kind switches to its entry's kind
det_show_kind:
	movem.l	d0-d4/a0-a2, -(sp)
	bsr	row_item
	bsr	row_entry
	moveq.l	#0, d2
det_show_kind_next:
	move.w	d4, d0
	addq.w	#1, d0
	add.w	d2, d0
	bsr	dlg_item
	moveq.l	#0, d1
	cmp.w	(a2), d2
	seq	d1
	andi.w	#1, d1
	move.w	d1, -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueSet		; $A290
	addq.l	#6, sp
	addq.w	#1, d2
	cmpi.w	#3, d2
	bcs.s	det_show_kind_next
	movem.l	(sp)+, d0-d4/a0-a2
	rts

* Shows the number of drives, and their rows only
det_show_count:
	movem.l	d0-d4/a0-a2, -(sp)
	lea	dlg_text(a5), a0
	move.b	#1, (a0)
	move.w	det_drives+DRV_COUNT(a5), d0
	addi.b	#'0', d0
	move.b	d0, 1(a0)
	moveq.l	#ITEM_COUNT, d0
	bsr	dlg_item
	pea	dlg_text(a5)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITSet			; $A2D2
	lea	10(sp), sp
	moveq.l	#0, d3
det_show_row:
	bsr	row_item
	moveq.l	#ROW_ITEMS-1, d2
det_show_item:
	move.w	d4, -(sp)
	move.l	dlg_ptr(a5), -(sp)
	cmp.w	det_drives+DRV_COUNT(a5), d3
	bcc.s	det_hide_item
	SXCALL	DIShow			; $A2D9
	bra.s	det_shown_item
det_hide_item:
	SXCALL	DIHide			; $A2D8
det_shown_item:
	addq.l	#6, sp
	addq.w	#1, d4
	dbra	d2, det_show_item
	addq.w	#1, d3
	cmpi.w	#DRIVE_ROWS, d3
	bcs.s	det_show_row
	movem.l	(sp)+, d0-d4/a0-a2
	rts

* Closes the dialog, whose record is in the work area
dlg_dispose:
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	DMClose			; $A2C5
	addq.l	#4, sp
	clr.l	dlg_ptr(a5)
	rts

* Finds the dialog's item D0.W: its type, handle, and rectangle in dlg_*
dlg_item:
	movem.l	d0-d2/a0-a1, -(sp)
	pea	dlg_rect(a5)
	pea	dlg_hdl(a5)
	pea	dlg_type(a5)
	move.w	d0, -(sp)
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	DIGet			; $A2CF
	lea	18(sp), sp
	movem.l	(sp)+, d0-d2/a0-a1
	rts

* Sets the CPU's radio buttons to dlg_cpu
dlg_show_cpu:
	movem.l	d0-d2/a0-a1, -(sp)
	moveq.l	#ITEM_68000, d0
	bsr	dlg_item
	moveq.l	#0, d1
	tst.w	dlg_cpu(a5)
	seq	d1
	andi.w	#1, d1
	move.w	d1, -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueSet		; $A290
	addq.l	#6, sp
	moveq.l	#ITEM_68030, d0
	bsr	dlg_item
	move.w	dlg_cpu(a5), -(sp)
	move.l	dlg_hdl(a5), -(sp)
	SXCALL	CMValueSet		; $A290
	addq.l	#6, sp
	movem.l	(sp)+, d0-d2/a0-a1
	rts

* Shows dlg_ram as the memory's size, " 4MB" or "12MB"
dlg_show_size:
	movem.l	d0-d2/a0-a1, -(sp)
	lea	size_text(a5), a0
	move.b	#4, (a0)+
	moveq.l	#0, d0
	move.w	dlg_ram(a5), d0
	divu	#10, d0
	move.b	#' ', d1
	tst.w	d0
	beq.s	dlg_size_tens
	move.b	#'0', d1
	add.b	d0, d1
dlg_size_tens:
	move.b	d1, (a0)+
	swap	d0
	addi.b	#'0', d0
	move.b	d0, (a0)+
	move.b	#'M', (a0)+
	move.b	#'B', (a0)
	moveq.l	#ITEM_MEMORY_SIZE, d0
	bsr	dlg_item
	pea	size_text(a5)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITSet			; $A2D2
	lea	10(sp), sp
	movem.l	(sp)+, d0-d2/a0-a1
	rts

* -----------------------------------------------------------------------------
* Modeless settings: obtain this task's events through Task Man, leaving the
* Filer and other applications usable. Only control tracking holds the mouse.
* Returns a clicked enabled item, as DMControl does, preserving callers' state.
dialog_control:
	movem.l	d1-d7/a0-a4,-(sp)
dialog_control_next:
	pea	event_rec(a5)
	move.w	#$72AA,-(sp)
	SXCALL	TSEventAvail
	addq.l	#6,sp
	cmpi.w	#$DFFE,d0
	beq	dialog_control_abort
	move.w	event_what(a5),d0
	cmpi.w	#14,d0
	beq	dialog_control_user
	cmpi.w	#7,d0
	beq	dialog_control_update
	cmpi.w	#12,d0
	beq	dialog_control_system
	cmpi.w	#13,d0
	beq	dialog_control_system
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	GMSetGraph
	addq.l	#4,sp
	* Calls may overwrite D0: read the event again.
	move.w	event_what(a5),d0
	beq	dialog_control_text
	cmpi.w	#5,d0
	beq	dialog_control_text
	cmpi.w	#1,d0
	beq	dialog_control_click
	cmpi.w	#3,d0
	beq	dialog_control_text
	bra	dialog_control_next
dialog_control_user:
	bsr	serve_disk
	bsr	update_status
	bra	dialog_control_next
dialog_control_update:
	pea	event_rec(a5)
	move.l	dlg_ptr(a5),-(sp)
	bsr	dialog_filter
	addq.l	#8,sp
	bra	dialog_control_next
dialog_control_system:
	move.w	event_what2(a5),d0
	cmpi.w	#1,d0
	beq	dialog_control_abort
	cmpi.w	#2,d0
	beq	dialog_control_abort
	cmpi.w	#81,d0
	bne	dialog_control_next
	bsr	dialog_drop
	bra	dialog_control_next
dialog_control_click:
	move.l	event_win(a5),d0
	cmp.l	dlg_ptr(a5),d0
	bne	dialog_control_next
	SXCALL	WMActive
	move.l	a0,d0
	cmp.l	dlg_ptr(a5),d0
	beq.s	dialog_control_selected
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	WMSelect
	addq.l	#4,sp
dialog_control_selected:
	move.l	event_where(a5),-(sp)
	SXCALL	GMGlobalToLocal
	addq.l	#4,sp
	move.l	d0,d5
	movea.l	dlg_ptr(a5),a0
	movea.l	DIALOG_ITEMS(a0),a0
	movea.l	(a0),a0
	move.w	(a0),d6		; DITL stores count minus one
	moveq.l	#1,d7
dialog_control_hit:
	move.w	d7,d0
	bsr	dlg_item
	move.l	d5,-(sp)
	pea	dlg_rect(a5)
	SXCALL	GMPtInRect
	addq.l	#8,sp
	tst.b	d0
	beq	dialog_control_hit_next
	move.w	dlg_type(a5),d0
	andi.w	#$7F,d0
	cmpi.w	#$10,d0
	beq	dialog_control_edit
	btst	#7,dlg_type+1(a5)
	bne	dialog_control_next
	cmpi.w	#4,d0
	bcs	dialog_control_next
	cmpi.w	#7,d0
	bhi	dialog_control_next
	move.l	#-1,-(sp)
	move.l	d5,-(sp)
	move.l	dlg_hdl(a5),-(sp)
	SXCALL	CMCheck
	lea	12(sp),sp
	tst.w	d0
	ble	dialog_control_next
	move.w	d7,d0
	bra	dialog_control_return
dialog_control_hit_next:
	addq.w	#1,d7
	dbra	d6,dialog_control_hit
	bra	dialog_control_next
dialog_control_edit:
	movea.l	dlg_ptr(a5),a0
	cmp.w	dlg_edit(a5),d7		; don't reset selection on repeated clicks
	beq	dialog_control_text
	bsr	dialog_store_edit
	move.w	d7,dlg_edit(a5)
	clr.l	-(sp)
	clr.l	-(sp)
	move.w	d7,-(sp)
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	DITSelect
	lea	14(sp),sp
dialog_control_text:
	SXCALL	WMActive
	move.l	a0,d0
	cmp.l	dlg_ptr(a5),d0
	bne	dialog_control_next
	movea.l	dlg_ptr(a5),a0
	move.l	DIALOG_TEDIT(a0),d0
	beq	dialog_control_next
	pea	event_rec(a5)
	move.l	d0,-(sp)
	SXCALL	TMEvent
	addq.l	#8,sp
	bra	dialog_control_next
dialog_control_abort:
	move.w	#1,dialog_end(a5)
dialog_control_cancel:
	moveq.l	#ITEM_CANCEL,d0
dialog_control_return:
	movem.l	(sp)+,d1-d7/a0-a4
	rts

* Commit Text Man's current buffer before switching fields or reading the rows.
* DMControl normally performs this synchronization itself.
dialog_store_edit:
	movem.l	d0-d2/a0-a1,-(sp)
	movea.l	dlg_ptr(a5),a0
	move.w	dlg_edit(a5),d2
	ble.s	dialog_store_done
	move.l	#PATH_MAX,-(sp)
	pea	dlg_text+1(a5)
	move.l	DIALOG_TEDIT(a0),-(sp)
	SXCALL	TMGetText
	lea	12(sp),sp
	tst.l	d0
	bmi.s	dialog_store_done
	move.b	d0,dlg_text(a5)
	move.w	d2,d0
	bsr	dlg_item
	pea	dlg_text(a5)
	move.l	dlg_hdl(a5),-(sp)
	move.w	dlg_type(a5),-(sp)
	SXCALL	DITSet
	lea	10(sp),sp
dialog_store_done:
	movem.l	(sp)+,d0-d2/a0-a1
	rts

* A single Filer path dropped on an active details row replaces that field and
* selects File. The settings remain staged until both dialogs are accepted.
dialog_drop:
	tst.l	cfg_ptr(a5)
	beq	dialog_drop_done
	move.l	event_win(a5),d0
	cmp.l	dlg_ptr(a5),d0
	bne	dialog_drop_done
	bsr	dialog_store_edit
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	GMSetGraph
	addq.l	#4,sp
	move.l	event_where(a5),-(sp)
	SXCALL	GMGlobalToLocal
	addq.l	#4,sp
	move.l	d0,d5
	moveq.l	#0,d3
dialog_drop_row:
	bsr	row_item
	move.w	d4,d0
	addq.w	#4,d0
	bsr	dlg_item
	move.l	d5,-(sp)
	pea	dlg_rect(a5)
	SXCALL	GMPtInRect
	addq.l	#8,sp
	tst.b	d0
	bne	dialog_drop_take
	addq.w	#1,d3
	cmp.w	det_drives+DRV_COUNT(a5),d3
	bcs	dialog_drop_row
	bra	dialog_drop_done
dialog_drop_take:
	SXCALL	TSGetDrag
	tst.l	d0
	bne	dialog_drop_done
	move.l	#PATH_MAX,-(sp)
	pea	image_path(a5)
	move.l	4(a0),-(sp)
	SXCALL	TSCellToStr
	lea	12(sp),sp
	tst.l	d0
	ble	dialog_drop_done
	cmpi.l	#PATH_MAX,d0
	bcc	dialog_drop_done
	lea	dlg_text(a5),a0
	move.b	d0,(a0)+
	lea	image_path(a5),a1
	subq.w	#1,d0
dialog_drop_copy:
	move.b	(a1)+,(a0)+
	dbra	d0,dialog_drop_copy
	pea	dlg_text(a5)
	move.l	dlg_hdl(a5),-(sp)
	move.w	dlg_type(a5),-(sp)
	SXCALL	DITSet
	lea	10(sp),sp
	bsr	row_entry
	clr.w	(a2)
	bsr	det_show_kind
	bsr	row_item
	addq.w	#4,d4
	move.w	d4,dlg_edit(a5)
	move.l	#$7FFFFFFF,-(sp)
	clr.l	-(sp)
	move.w	d4,-(sp)
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	DITSelect
	lea	14(sp),sp
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	DMDraw
	addq.l	#4,sp
	move.w	#1,-(sp)
	SXCALL	TSEndDrag
	addq.l	#2,sp
dialog_drop_done:
	rts

* -----------------------------------------------------------------------------
* Helper: dialog_filter (dlgPtr, eventRec on the stack), which DMControl calls
* with each event: the VM's requests are served and the window drawn meanwhile.
* Returns 0, the event as it is. A5 is not this task's work area here: it is
* found from the dialog's record, which is in it.
* -----------------------------------------------------------------------------
dialog_filter:
	movem.l	d1-d7/a0-a5, -(sp)
	* The work area, from the dialog's record in it
	movea.l	4+52(sp), a5
	move.w	DIALOG_RECORD(a5), d0
	suba.w	d0, a5
	movea.l	8+52(sp), a0		; the event
	move.w	(a0), d0
	bne.s	dialog_filter_user
	* E_IDLE: the text field's cursor blinks, in ドライブの詳細, the dialog with them
	tst.l	cfg_ptr(a5)
	beq.s	dialog_filter_done
	movea.l	dlg_ptr(a5), a0
	move.l	DIALOG_TEDIT(a0), d0
	beq.s	dialog_filter_done
	move.l	d0, -(sp)
	SXCALL	TMIdle			; $A30E
	addq.l	#4, sp
	bra.s	dialog_filter_done
dialog_filter_user:
	cmpi.w	#14, d0			; E_USER1
	bne.s	dialog_filter_update
	bsr	serve_disk
	bsr	update_status
	bra.s	dialog_filter_back
dialog_filter_update:
	cmpi.w	#7, d0			; E_UPDATE
	bne.s	dialog_filter_done
	move.l	2(a0), d0		; whom
	cmp.l	win_ptr(a5), d0
	bne.s	dialog_filter_dialog
	move.l	d0, -(sp)
	SXCALL	WMUpdate		; $A20D
	bsr	draw_bar
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp
	bra.s	dialog_filter_back
dialog_filter_dialog:
	* Never consume an update without painting the exposed items.
	move.l	d0, -(sp)
	SXCALL	WMUpdate		; $A20D
	movea.l	(sp),a0
	move.l	WINDOW_UPDATE(a0),-(sp)		; Window.update
	move.l	a0,-(sp)
	SXCALL	DIUpdate
	addq.l	#8,sp
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp
dialog_filter_back:
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
dialog_filter_done:
	moveq.l	#0, d0
	movem.l	(sp)+, d1-d7/a0-a5
	rts

* -----------------------------------------------------------------------------
* Helper: serve_disk
* Carries out the VM's bridge requests until there are none: reads and writes of
* drive 0's image, and the primary's DOS calls a VM's remote drives need. The
* kinds are those of BridgeOp in crates/msx68k-bus/src/vmx68k.rs.
* -----------------------------------------------------------------------------
serve_disk:
	movem.l	d0-d3/a0-a1, -(sp)
serve_next:
	moveq.l	#MSX68K_FN_VM_POLL, d0
	move.l	vm_id(a5), d1
	lea	disk_buffer(a5), a0
	move.l	a0, d2
	move.l	#DISK_BUFFER_SIZE, d3
	bsr	msx68k_call
	tst.l	d0
	ble	serve_done		; 0: nothing to do, -1: no such VM
	lea	request(a5), a1
	move.l	d0, (a1)+		; kind
	move.l	d1, (a1)+		; offset, attribute, or drive
	move.l	d2, (a1)+		; length
	move.l	d3, (a1)		; handle
	moveq.l	#-1, d3			; a kind it does not know fails
	cmpi.l	#SERVE_KINDS, d0
	bhi	serve_reply
	add.w	d0, d0
	move.w	serve_table-2(pc, d0.w), d0
	jmp	serve_table(pc, d0.w)
serve_table:
	.dc.w	serve_image-serve_table		; 1 read drive 0's image
	.dc.w	serve_image-serve_table		; 2 write drive 0's image
	.dc.w	serve_open-serve_table		; 3 _OPEN
	.dc.w	serve_handle_read-serve_table	; 4 read a handle
	.dc.w	serve_close-serve_table		; 5 _CLOSE
	.dc.w	serve_files-serve_table		; 6 _FILES
	.dc.w	serve_nfiles-serve_table	; 7 _NFILES
	.dc.w	serve_dskfre-serve_table	; 8 _DSKFRE
	.dc.w	serve_mkdir-serve_table		; 9 _MKDIR
	.dc.w	serve_rmdir-serve_table		; 10 _RMDIR
	.dc.w	serve_delete-serve_table	; 11 _DELETE
	.dc.w	serve_rename-serve_table	; 12 _RENAME
	.dc.w	serve_chmod-serve_table		; 13 _CHMOD
	.dc.w	serve_create-serve_table	; 14 _CREATE or _NEWFILE
	.dc.w	serve_handle_write-serve_table	; 15 write a handle
	.dc.w	serve_filedate-serve_table	; 16 _FILEDATE
	.dc.w	serve_media_done-serve_table	; 17 old medium is no longer in use
	.dc.w	serve_image_sync-serve_table	; 18 commit resized image metadata
SERVE_KINDS	.equ	18

serve_image_sync:
	* 18: commit a container's changed file length. Human68k updates the
	* directory on _CLOSE, not _FFLUSH. Restore the same DOS handle with _DUP2.
	movem.l	d1-d2/d4-d7/a0-a3,-(sp)
	move.w	request_handle+2(a5),d7
	lea	drive_table+2(a5),a1
	lea	run_drives+DRV_ENTRIES(a5),a2
	move.w	drive_table(a5),d6
	subq.w	#1,d6
	bmi	image_sync_pending
image_sync_find:
	tst.w	(a1)
	beq	image_sync_compare
	cmpi.w	#3,(a1)
	bne	image_sync_next
image_sync_compare:
	cmp.w	2(a1),d7
	beq	image_sync_row
image_sync_next:
	addq.l	#8,a1
	lea	ENTRY_SIZE(a2),a2
	dbra	d6,image_sync_find
image_sync_pending:
	lea	pending_old(a5),a1
	lea	pending_paths(a5),a0
	moveq.l	#3,d6
image_sync_pending_find:
	cmp.w	(a1)+,d7
	beq	image_sync_path
	lea	PATH_MAX+2(a0),a0
	dbra	d6,image_sync_pending_find
	bra	image_sync_done
image_sync_row:
	lea	2(a2),a0
image_sync_path:
	moveq.l	#0,d6
	move.b	(a0)+,d6
	lea	image_path(a5),a1
	bra	image_sync_copy_test
image_sync_copy:
	move.b	(a0)+,(a1)+
image_sync_copy_test:
	dbra	d6,image_sync_copy
	clr.b	(a1)
	move.w	d7,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
	tst.l	d0
	bmi	image_sync_result
	move.w	#2,-(sp)
	pea	image_path(a5)
	DOS	_OPEN
	addq.l	#6,sp
	tst.l	d0
	bmi	image_sync_result
	cmp.w	d7,d0
	beq	image_sync_ok
	move.w	d0,d1
	move.w	d7,-(sp)
	move.w	d1,-(sp)
	DOS	_DUP2
	addq.l	#4,sp
	move.l	d0,d2
	move.w	d1,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
	move.l	d2,d0
	tst.l	d0
	bmi	image_sync_result
image_sync_ok:
	moveq.l	#0,d0
image_sync_result:
	move.l	d0,d3
image_sync_done:
	movem.l	(sp)+,d1-d2/d4-d7/a0-a3
	bra	serve_reply

serve_media_done:
	move.l	request_offset(a5), d0
	cmpi.l	#4, d0
	bcc	serve_reply
	lea	media_busy(a5), a0
	clr.b	(a0,d0.w)
	add.w	d0,d0
	lea	pending_old(a5), a0
	move.w	(a0,d0.w), d1
	move.w	#-1,(a0,d0.w)
	tst.w	d1
	bmi.s	serve_media_closed
	move.w	d1,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
serve_media_closed:
	moveq.l	#0,d3
	bra	serve_reply

serve_image:
	* 1, 2: read or write drive 0's image; the reply is 0, or -1 when it fails
	moveq.l	#-1, d3
	move.w	drive0_fd(a5), d0
	bmi	serve_reply
	clr.w	-(sp)			; from the start
	move.l	request_offset(a5), -(sp)
	move.w	d0, -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi	serve_reply
	move.l	request_length(a5), -(sp)
	pea	disk_buffer(a5)
	move.w	drive0_fd(a5), -(sp)
	move.l	request_kind(a5), d0
	cmpi.l	#1, d0
	bne.s	serve_image_write
	DOS	_READ
	bra.s	serve_image_moved
serve_image_write:
	DOS	_WRITE
serve_image_moved:
	lea	10(sp), sp
	cmp.l	request_length(a5), d0
	bne	serve_reply
	moveq.l	#0, d3
	bra	serve_reply

serve_open:
	* 3: open the path in the buffer with the mode given; the reply is the handle
	move.w	request_offset+2(a5), -(sp)
	pea	disk_buffer(a5)
	DOS	_OPEN
	addq.l	#6, sp
	bra	serve_result

serve_handle_read:
	* 4: read at an offset of an open file; the reply is the count
	clr.w	-(sp)
	move.l	request_offset(a5), -(sp)
	move.w	request_handle+2(a5), -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi	serve_result
	move.l	request_length(a5), -(sp)
	pea	disk_buffer(a5)
	move.w	request_handle+2(a5), -(sp)
	DOS	_READ
	lea	10(sp), sp
	bra	serve_result

serve_close:
	* 5: close an open file
	move.w	request_handle+2(a5), -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
	bra	serve_result

serve_files:
	* 6: _FILES for the path at +64 of the buffer, into its first 53 bytes
	move.w	request_offset+2(a5), -(sp)
	pea	disk_buffer+64(a5)
	pea	disk_buffer(a5)
	DOS	_FILES
	lea	10(sp), sp
	bra	serve_result

serve_nfiles:
	* 7: _NFILES with the search buffer the host put back
	pea	disk_buffer(a5)
	DOS	_NFILES
	addq.l	#4, sp
	bra	serve_result

serve_dskfre:
	* 8: free space of a drive (1 = A:) into the buffer
	pea	disk_buffer(a5)
	move.w	request_offset+2(a5), -(sp)
	DOS	_DSKFRE
	addq.l	#6, sp
	bra	serve_result

serve_mkdir:
	* 9: make the directory named in the buffer
	pea	disk_buffer(a5)
	DOS	_MKDIR
	addq.l	#4, sp
	bra	serve_result

serve_rmdir:
	* 10: remove the directory named in the buffer
	pea	disk_buffer(a5)
	DOS	_RMDIR
	addq.l	#4, sp
	bra	serve_result

serve_delete:
	* 11: delete the file named in the buffer
	pea	disk_buffer(a5)
	DOS	_DELETE
	addq.l	#4, sp
	bra	serve_result

serve_rename:
	* 12: rename the file named in the buffer to the name at +256
	pea	disk_buffer+256(a5)
	pea	disk_buffer(a5)
	DOS	_RENAME
	addq.l	#8, sp
	bra	serve_result

serve_chmod:
	* 13: set the attribute given, or read it with -1; the reply is the attribute
	move.w	request_offset+2(a5), -(sp)
	pea	disk_buffer(a5)
	DOS	_CHMOD
	addq.l	#6, sp
	bra	serve_result

serve_create:
	* 14: create the file named in the buffer with the attribute given, replacing one
	* there (_CREATE) or not (_NEWFILE); the reply is the handle
	move.w	request_offset+2(a5), -(sp)
	pea	disk_buffer(a5)
	tst.l	request_length(a5)
	beq	serve_newfile
	DOS	_CREATE
	bra	serve_created
serve_newfile:
	DOS	_NEWFILE
serve_created:
	addq.l	#6, sp
	bra	serve_result

serve_handle_write:
	* 15: write the buffer at an offset of an open file, or cut it there with nothing;
	* the reply is the count
	clr.w	-(sp)
	move.l	request_offset(a5), -(sp)
	move.w	request_handle+2(a5), -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi	serve_result
	move.l	request_length(a5), -(sp)
	pea	disk_buffer(a5)
	move.w	request_handle+2(a5), -(sp)
	DOS	_WRITE
	lea	10(sp), sp
	bra	serve_result

serve_filedate:
	* 16: set the date and time of an open file
	move.l	request_offset(a5), -(sp)
	move.w	request_handle+2(a5), -(sp)
	DOS	_FILEDATE
	addq.l	#6, sp

serve_result:
	move.l	d0, d3
	* What changes the primary's storage is written out at once, not left in Human68k's
	* buffers, so nothing a VM wrote is lost if the machine stops
	move.l	request_kind(a5), d0
	cmpi.l	#5, d0			; _CLOSE
	beq.s	serve_flush
	cmpi.l	#9, d0			; _MKDIR and after
	bcs.s	serve_reply
serve_flush:
	DOS	_FFLUSH
serve_reply:
	moveq.l	#MSX68K_FN_VM_REPLY, d0
	move.l	vm_id(a5), d1
	move.l	d3, d2
	bsr	msx68k_call
	bra	serve_next
serve_done:
	movem.l	(sp)+, d0-d3/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper: msx68k_call (see msx68k.mac)
* -----------------------------------------------------------------------------
	MSX68K_CALL_ROUTINE

* -----------------------------------------------------------------------------
* Per-unit FD rectangles and state. D4 is the unit, preserved by SX calls.
fd_rectangle:
	move.w	d4,d0
	mulu	#36,d0
	addi.w	#40,d0
	lea	fd_rect(a5),a0
	move.w	d0,(a0)+
	move.w	#2,(a0)+
	addi.w	#30,d0
	move.w	d0,(a0)+
	move.w	#22,(a0)
	rts

draw_floppies:
	movem.l	d0-d5/a0-a1,-(sp)
	move.l	win_ptr(a5),-(sp)
	SXCALL	GMSetGraph
	addq.l	#4,sp
	moveq.l	#0,d4
draw_fd_next:
	move.l	fd_state(a5),d5
	bsr	fd_rectangle
	* Erase retired icons and all lamp planes when the connected set changes.
	move.w	#$000F,-(sp)
	SXCALL	GMAPage
	addq.l	#2,sp
	move.w	d0,-(sp)
	move.w	#G_LGRAY,-(sp)
	SXCALL	GMForeColor
	addq.l	#2,sp
	clr.w	-(sp)
	SXCALL	GMPenMode
	addq.l	#2,sp
	pea	fd_rect(a5)
	SXCALL	GMFillRect
	addq.l	#4,sp
	SXCALL	GMAPage
	addq.l	#2,sp
	btst	d4,d5
	beq	draw_fd_skip
	move.w	d4,d0
	add.w	d0,d0
	addi.w	#10,d0
	move.w	d4,d1
	addq.w	#4,d1
	btst	d1,d5
	beq	draw_fd_resource
	addq.w	#1,d0
draw_fd_resource:
	move.w	d0,-(sp)
	move.l	#'PLOT',-(sp)
	SXCALL	TSRscGet
	addq.l	#6,sp
	move.l	a0,d0
	beq	draw_fd_skip
	movea.l	(a0),a0
	clr.w	-(sp)
	pea	fd_rect(a5)
	pea	8(a0)
	SXCALL	GMPlotImg
	lea	10(sp),sp
	addi.w	#21,fd_rect(a5)
	subi.w	#3,fd_rect+4(a5)
	move.w	#16,fd_rect+2(a5)
	move.w	#19,fd_rect+6(a5)
	moveq.l	#G_DGRAY,d0
	move.w	d4,d1
	addq.w	#8,d1
	btst	d1,d5
	beq	draw_fd_lamp
	moveq.l	#G_RED,d0
draw_fd_lamp:
	move.w	d0,d3
	move.w	#$000F,-(sp)
	SXCALL	GMAPage
	addq.l	#2,sp
	move.w	d0,-(sp)
	move.w	d3,-(sp)
	SXCALL	GMForeColor
	addq.l	#2,sp
	clr.w	-(sp)
	SXCALL	GMPenMode
	addq.l	#2,sp
	pea	fd_rect(a5)
	SXCALL	GMFillRect
	addq.l	#4,sp
	SXCALL	GMAPage
	addq.l	#2,sp
draw_fd_skip:
	addq.w	#1,d4
	cmpi.w	#4,d4
	bcs	draw_fd_next
	movem.l	(sp)+,d0-d5/a0-a1
	rts

* Returns unit in D4, -1 outside connected custom-mode FD icons.
fd_hit:
	move.l	win_ptr(a5),-(sp)
	SXCALL	GMSetGraph
	addq.l	#4,sp
	move.l	event_where(a5),-(sp)
	SXCALL	GMGlobalToLocal
	addq.l	#4,sp
	move.l	d0,d5
	moveq.l	#0,d4
fd_hit_next:
	move.l	fd_state(a5),d0
	btst	d4,d0
	beq	fd_hit_skip
	bsr	fd_rectangle
	move.l	d5,-(sp)
	pea	fd_rect(a5)
	SXCALL	GMPtInRect
	addq.l	#8,sp
	tst.b	d0
	bne	fd_hit_done
fd_hit_skip:
	addq.w	#1,d4
	cmpi.w	#4,d4
	bcs	fd_hit_next
	moveq.l	#-1,d4
fd_hit_done:
	rts

handle_drop:
	move.l	event_win(a5),d0
	cmp.l	win_ptr(a5),d0
	bne	event_loop
	bsr	fd_hit
	tst.l	d4
	bmi	event_loop
	SXCALL	TSGetDrag
	tst.l	d0
	bne	event_loop
	move.l	#PATH_MAX,-(sp)
	pea	image_path(a5)
	move.l	4(a0),-(sp)
	SXCALL	TSCellToStr
	lea	12(sp),sp
	tst.l	d0
	ble	event_loop
	cmpi.l	#PATH_MAX,d0
	bcc	event_loop
	* Filer strings contain the file's full name; reject multiple cells.
	lea	image_path(a5),a0
	clr.b	(a0,d0.w)
	bsr	change_floppy
	move.w	#1,-(sp)
	SXCALL	TSEndDrag
	addq.l	#2,sp
	bra	event_loop

* D0 file length, D3 DOS handle -> D0 1 floppy, 0 other, -1 read error.
* The host requests the prefix it needs; no format constants live in this task.
probe_image:
	movem.l	d1-d7/a0-a2,-(sp)
	move.l	d0,d6
	move.l	d3,d7
	moveq.l	#0,d5
probe_image_next:
	moveq.l	#MSX68K_FN_VM_IMAGE,d0
	move.l	d7,d1
	move.l	d6,d2
	move.l	d5,d3
	lea	disk_buffer(a5),a0
	bsr	msx68k_call
	cmpi.l	#2,d0
	bne	probe_image_done
	cmpi.l	#DISK_BUFFER_SIZE,d1
	bhi	probe_image_error
	cmp.l	d6,d1
	bhi	probe_image_error
	tst.l	d1
	beq	probe_image_error
	move.l	d1,d5
	move.l	d2,d4
	add.l	d1,d2
	bcs	probe_image_error
	cmp.l	d6,d2
	bhi	probe_image_error
	clr.w	-(sp)
	move.l	d4,-(sp)
	move.w	d7,-(sp)
	DOS	_SEEK
	addq.l	#8,sp
	tst.l	d0
	bmi	probe_image_error
	move.l	d5,-(sp)
	pea	disk_buffer(a5)
	move.w	d7,-(sp)
	DOS	_READ
	lea	10(sp),sp
	cmp.l	d5,d0
	bne	probe_image_error
	bra	probe_image_next
probe_image_error:
	moveq.l	#-1,d0
probe_image_done:
	movem.l	(sp)+,d1-d7/a0-a2
	rts

* Change the selected medium without a reset. Keep the old handle until kind 17
* acknowledges completion of earlier I/O on the VM thread.
change_floppy:
	movem.l	d0-d7/a0-a4,-(sp)
	lea	media_busy(a5),a0
	tst.b	(a0,d4.w)
	bne	change_fd_done
	lea	run_drives(a5),a0
	lea	vm_drives(a5),a1
	move.w	#DRIVES_SIZE/2-1,d0
change_fd_compare:
	cmpm.w	(a0)+,(a1)+
	dbne	d0,change_fd_compare
	seq	media_save(a5)
	move.w	d4,d7
	moveq.l	#0,d6
	lea	drive_table+2(a5),a4
	lea	run_drives+DRV_ENTRIES(a5),a3
change_fd_find:
	cmp.w	drive_table(a5),d6
	bcc	change_fd_append
	cmpi.w	#2,(a4)
	beq	change_fd_slot
	cmpi.w	#3,(a4)
	beq	change_fd_slot
	tst.w	(a4)
	bne	change_fd_find_next
	tst.w	2(a4)
	bmi	change_fd_slot
	bra	change_fd_find_next
change_fd_slot:
	tst.w	d7
	beq	change_fd_found
	subq.w	#1,d7
change_fd_find_next:
	addq.w	#1,d6
	addq.l	#8,a4
	lea	ENTRY_SIZE(a3),a3
	bra	change_fd_find
change_fd_append:
	move.w	d6,d0
	add.w	d7,d0
	cmpi.w	#DRIVE_ROWS,d0
	bcc	change_fd_done
	* Spare controller units need an explicit empty row in the saved setup.
	clr.w	(a4)
	move.w	#-1,2(a4)
	clr.l	4(a4)
	clr.w	(a3)
	clr.b	2(a3)
	tst.w	d7
	beq	change_fd_found
	subq.w	#1,d7
	bra	change_fd_find_next
change_fd_found:
	moveq.l	#-1,d3
	moveq.l	#0,d5
	tst.b	image_path(a5)
	beq	change_fd_request
	move.w	#2,-(sp)
	pea	image_path(a5)
	DOS	_OPEN
	addq.l	#6,sp
	tst.l	d0
	bmi	change_fd_error
	move.l	d0,d3
	move.w	#2,-(sp)
	clr.l	-(sp)
	move.w	d3,-(sp)
	DOS	_SEEK
	addq.l	#8,sp
	move.l	d0,d5
	bsr	probe_image
	cmpi.l	#1,d0
	bne	change_fd_reject
change_fd_request:
	move.l	d4,d2
	move.l	d5,d4
	moveq.l	#MSX68K_FN_VM_MEDIA,d0
	move.l	vm_id(a5),d1
	bsr	msx68k_call
	move.l	d2,d4
	tst.l	d0
	bne	change_fd_reject
	lea	media_busy(a5),a0
	st	(a0,d4.w)
	move.w	d4,d0
	add.w	d0,d0
	lea	pending_old(a5),a0
	cmpi.w	#3,(a4)
	beq	change_fd_old_file
	tst.w	(a4)
	bne	change_fd_no_old
change_fd_old_file:
	move.w	2(a4),(a0,d0.w)
	move.w	d4,d0
	mulu	#PATH_MAX+2,d0
	lea	pending_paths(a5),a0
	adda.w	d0,a0
	lea	2(a3),a1
	moveq.l	#(PATH_MAX+2)/2-1,d0
change_fd_keep_path:
	move.w	(a1)+,(a0)+
	dbra	d0,change_fd_keep_path
change_fd_no_old:
	cmp.w	drive_table(a5),d6
	bcs	change_fd_count_kept
	move.w	d6,d0
	addq.w	#1,d0
	move.w	d0,drive_table(a5)
	move.w	d0,run_drives+DRV_COUNT(a5)
change_fd_count_kept:
	* Physical eject keeps its passthrough row for reinsertion by the primary.
	cmpi.w	#2,(a4)
	bne	change_fd_save
	tst.l	d3
	bmi	change_fd_done
change_fd_save:
	clr.w	(a4)
	tst.l	d3
	bmi	change_fd_empty
	move.w	#3,(a4)
change_fd_empty:
	move.w	d3,2(a4)
	move.l	d5,4(a4)
	clr.w	(a3)
	lea	image_path(a5),a0
	lea	3(a3),a1
	moveq.l	#0,d0
change_fd_path:
	move.b	(a0)+,(a1)+
	beq	change_fd_path_done
	addq.w	#1,d0
	bra	change_fd_path
change_fd_path_done:
	move.b	d0,2(a3)
	tst.b	media_save(a5)
	beq	change_fd_done
	lea	run_drives(a5),a0
	lea	vm_drives(a5),a1
	bsr	copy_drives
	bsr	save_setup
	bra	change_fd_done
change_fd_reject:
	tst.l	d3
	bmi	change_fd_error
	move.w	d3,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
change_fd_error:
	pea	media_error(pc)
	bsr	show_error
	addq.l	#4,sp
change_fd_done:
	movem.l	(sp)+,d0-d7/a0-a4
	rts

build_boot_menu:
	movem.l	d0-d3/a0-a1,-(sp)
	lea	boot_items(pc),a0
	lea	boot_string(a5),a1
	moveq.l	#0,d3
boot_menu_item:
	cmp.w	vm_boot(a5),d3
	bne	boot_menu_copy
	move.b	#'!',(a1)+
boot_menu_copy:
	move.b	(a0)+,d0
	beq	boot_menu_next
	move.b	d0,(a1)+
	bra	boot_menu_copy
boot_menu_next:
	addq.w	#1,d3
	cmpi.w	#14,d3
	bcc	boot_menu_built
	move.b	#',',(a1)+
	bra	boot_menu_item
boot_menu_built:
	clr.b	(a1)
	move.w	#2,-(sp)
	move.l	#'MENU',-(sp)
	SXCALL	TSRscGet
	addq.l	#6,sp
	move.l	a0,d0
	beq	boot_menu_done
	clr.w	-(sp)
	pea	boot_string(a5)
	move.l	a0,-(sp)
	SXCALL	MNConvert2
	lea	10(sp),sp
boot_menu_done:
	movem.l	(sp)+,d0-d3/a0-a1
	rts

* A small native dialog shared by New setup (3) and ROM address (4).
* D0 resource ID, A1 Pascal initial text. Returns 0 accepted, -1 cancelled;
* accepted text is NUL-terminated in image_path, whitespace trimmed.
text_dialog:
	movem.l	d1-d4/a0-a2,-(sp)
	move.w	d0,d4
	move.l	a1,a2
	move.w	#cfg_dialog,cfg_dialog+DIALOG_RECORD(a5)
	move.l	#-1,-(sp)
	pea	cfg_dialog(a5)
	move.w	d0,-(sp)
	SXCALL	DMRefer
	lea	10(sp),sp
	move.l	a0,d0
	beq	text_dialog_cancelled
	move.l	a0,dlg_ptr(a5)
	moveq.l	#3,d0
	bsr	dlg_item
	move.l	a2,-(sp)
	move.l	dlg_hdl(a5),-(sp)
	move.w	dlg_type(a5),-(sp)
	SXCALL	DITSet
	lea	10(sp),sp
	cmpi.w	#3,d4
	bne	text_dialog_show
	bsr	new_boot_draw
text_dialog_show:
	move.l	dlg_ptr(a5),-(sp)
	SXCALL	WMShow
	SXCALL	GMSetGraph
	SXCALL	DMDraw
	addq.l	#4,sp
text_dialog_loop:
	pea	dialog_filter(pc)
	SXCALL	DMControl
	addq.l	#4,sp
	cmpi.w	#1,d0
	beq	text_dialog_ok
	cmpi.w	#2,d0
	beq	text_dialog_cancel
	cmpi.w	#3,d4
	bne	text_dialog_loop
	subi.w	#4,d0
	cmpi.w	#14,d0
	bcc	text_dialog_loop
	move.w	d0,new_boot(a5)
	bsr	new_boot_draw
	bra	text_dialog_loop
text_dialog_ok:
	moveq.l	#3,d0
	bsr	dlg_item
	pea	dlg_text(a5)
	move.l	dlg_hdl(a5),-(sp)
	move.w	dlg_type(a5),-(sp)
	SXCALL	DITGet
	lea	10(sp),sp
	moveq.l	#0,d1
	move.b	dlg_text(a5),d1
	cmpi.w	#PATH_MAX-1,d1
	bls	text_dialog_trim
	moveq.l	#PATH_MAX-1,d1
text_dialog_trim:
	lea	dlg_text(a5),a0
	tst.w	d1
	beq	text_dialog_cancel
	cmpi.b	#' ',(a0,d1.w)
	bne	text_dialog_copy_start
	subq.w	#1,d1
	bra	text_dialog_trim
text_dialog_copy_start:
	lea	image_path(a5),a1
	addq.l	#1,a0
	subq.w	#1,d1
text_dialog_copy:
	move.b	(a0)+,(a1)+
	dbra	d1,text_dialog_copy
	clr.b	(a1)
	bsr	dlg_dispose
	moveq.l	#0,d0
	bra	text_dialog_done
text_dialog_cancel:
	bsr	dlg_dispose
text_dialog_cancelled:
	moveq.l	#-1,d0
text_dialog_done:
	movem.l	(sp)+,d1-d4/a0-a2
	rts

new_boot_draw:
	movem.l	d0-d3,-(sp)
	moveq.l	#0,d3
new_boot_draw_next:
	move.w	d3,d0
	addq.w	#4,d0
	bsr	dlg_item
	moveq.l	#0,d0
	cmp.w	new_boot(a5),d3
	seq	d0
	andi.w	#1,d0
	move.w	d0,-(sp)
	move.l	dlg_hdl(a5),-(sp)
	SXCALL	CMValueSet
	addq.l	#6,sp
	addq.w	#1,d3
	cmpi.w	#14,d3
	bcs	new_boot_draw_next
	movem.l	(sp)+,d0-d3
	rts

rom_dialog:
	movem.l	d1-d4/a0,-(sp)
	move.l	a1,d1
	lsl.l	#8,d1
	lea	dlg_text(a5),a0
	move.b	#6,(a0)+
	moveq.l	#5,d3
rom_address_digit:
	rol.l	#4,d1
	moveq.l	#0,d0
	move.b	d1,d0
	andi.w	#15,d0
	addi.w	#'0',d0
	cmpi.w	#'9',d0
	bls	rom_address_store
	addq.w	#7,d0
rom_address_store:
	move.b	d0,(a0)+
	dbra	d3,rom_address_digit
	moveq.l	#4,d0
	lea	dlg_text(a5),a1
	bsr	text_dialog
	tst.l	d0
	bmi	rom_dialog_done
	lea	image_path(a5),a0
	moveq.l	#0,d1
	moveq.l	#0,d3
rom_parse:
	moveq.l	#0,d0
	move.b	(a0)+,d0
	beq	rom_parse_end
	subi.w	#'0',d0
	cmpi.w	#9,d0
	bls	rom_digit
	subi.w	#7,d0
	cmpi.w	#10,d0
	bcs	rom_invalid
	cmpi.w	#15,d0
	bls	rom_digit
	subi.w	#32,d0
	cmpi.w	#10,d0
	bcs	rom_invalid
	cmpi.w	#15,d0
	bhi	rom_invalid
rom_digit:
	addq.w	#1,d3
	cmpi.w	#6,d3
	bhi	rom_invalid
	lsl.l	#4,d1
	or.w	d0,d1
	bra	rom_parse
rom_parse_end:
	tst.w	d3
	beq	rom_invalid
	btst	#0,d1
	bne	rom_invalid
	movea.l	d1,a1
	moveq.l	#0,d0
	bra	rom_dialog_done
rom_invalid:
	pea	rom_error(pc)
	bsr	show_error
	addq.l	#4,sp
	moveq.l	#-1,d0
rom_dialog_done:
	movem.l	(sp)+,d1-d4/a0
	rts

* New files inherit CPU, memory, and drives, with fresh SRAM. Choose Boot with
* the same checked submenu before committing the file. The running VM is kept.
new_setup:
	movem.l	d0-d7/a0-a4,-(sp)
	clr.w	new_boot(a5)
	moveq.l	#3,d0
	lea	new_default(pc),a1
	bsr	text_dialog
	tst.l	d0
	bmi	new_setup_done
	lea	image_path(a5),a0
	lea	new_path(a5),a1
new_path_copy:
	move.b	(a0)+,(a1)+
	bne	new_path_copy
	* Never overwrite another setup.
	clr.w	-(sp)
	pea	new_path(a5)
	DOS	_OPEN
	addq.l	#6,sp
	tst.l	d0
	bmi	new_setup_boot
	move.w	d0,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
	pea	new_exists(pc)
	bsr	show_error
	addq.l	#4,sp
	bra	new_setup_done
new_setup_boot:
	moveq.l	#0,d6
	move.w	new_boot(a5),d6
	movea.l	sram_buffer+12(a5),a1
	cmpi.w	#13,d6
	bne	new_setup_create
	bsr	rom_dialog
	tst.l	d0
	bmi	new_setup_done
new_setup_create:
	* Snapshot current settings; no event dispatch runs while these are borrowed.
	lea	sram_buffer(a5),a0
	lea	disk_buffer(a5),a2
	move.w	#SRAM_SIZE/4-1,d0
new_backup_sram:
	move.l	(a0)+,(a2)+
	dbra	d0,new_backup_sram
	lea	setup_path(a5),a0
	lea	old_path(a5),a2
	moveq.l	#PATH_MAX/2-1,d0
new_backup_path:
	move.w	(a0)+,(a2)+
	dbra	d0,new_backup_path
	move.b	has_sram(a5),d7
	move.b	setup_is_default(a5),d5
	move.b	setup_found(a5),d4
	moveq.l	#MSX68K_FN_VM_BOOT,d0
	moveq.l	#0,d1
	move.l	d6,d2
	moveq.l	#0,d3
	move.w	vm_ram(a5),d3
	lea	sram_buffer(a5),a0
	bsr	msx68k_call
	tst.l	d0
	bmi	new_restore
	lea	new_path(a5),a0
	lea	setup_path(a5),a2
new_use_path:
	move.b	(a0)+,(a2)+
	bne	new_use_path
	st	has_sram(a5)
	clr.b	setup_is_default(a5)
	move.w	vm_drives+DRV_MODE(a5),d6
	move.w	#1,vm_drives+DRV_MODE(a5)
	bsr	save_setup
	move.w	d6,vm_drives+DRV_MODE(a5)
new_restore:
	move.b	d7,has_sram(a5)
	move.b	d5,setup_is_default(a5)
	move.b	d4,setup_found(a5)
	lea	old_path(a5),a0
	lea	setup_path(a5),a2
	moveq.l	#PATH_MAX/2-1,d0
new_restore_path:
	move.w	(a0)+,(a2)+
	dbra	d0,new_restore_path
	lea	disk_buffer(a5),a0
	lea	sram_buffer(a5),a2
	move.w	#SRAM_SIZE/4-1,d0
new_restore_sram:
	move.l	(a0)+,(a2)+
	dbra	d0,new_restore_sram
	clr.w	-(sp)
	pea	new_path(a5)
	DOS	_OPEN
	addq.l	#6,sp
	tst.l	d0
	bmi	new_setup_failed
	move.w	d0,-(sp)
	DOS	_CLOSE
	addq.l	#2,sp
	* The task manager takes a Pascal command line. Quote paths containing spaces.
	lea	new_command+1(a5),a2
	move.b	#34,(a2)+
	lea	new_path(a5),a0
	moveq.l	#2,d1
new_command_copy:
	move.b	(a0)+,d0
	beq	new_command_end
	move.b	d0,(a2)+
	addq.w	#1,d1
	bra	new_command_copy
new_command_end:
	move.b	#34,(a2)+
	move.b	#' ',(a2)+
	lea	setup_option(pc),a0
	moveq.l	#5,d0
new_command_option:
	move.b	(a0)+,(a2)+
	dbra	d0,new_command_option
	addq.w	#7,d1
	move.b	d1,new_command(a5)
	clr.w	-(sp)		; resource ID
	clr.l	-(sp)		; resource type
	clr.l	-(sp)		; inherited environment
	pea	new_command(a5)
	move.l	app_task_id(a5),-(sp)
	move.w	#2,-(sp)		; mode1.b = 0, mode2.b = 2: share this R module
	SXCALL	TSFock
	lea	20(sp),sp
	tst.l	d0
	bpl	new_setup_done
new_setup_failed:
	pea	new_error(pc)
	bsr	show_error
	addq.l	#4,sp
new_setup_done:
	movem.l	(sp)+,d0-d7/a0-a4
	rts

* Pointer argument is on the caller's stack.
show_error:
	move.l	4(sp),-(sp)
	move.w	#1,-(sp)
	SXCALL	DMError
	addq.l	#6,sp
	rts

* Data
* -----------------------------------------------------------------------------
str_needs_sx:
	.dc.b	"VMX68K.X runs on SX-Window under msx68k.", 13, 10, 0
win_title:
	.dc.b	6, "VMX68K", 0
drive0_path:
	.dc.b	"A:\VMX68K\DRIVE0.XDF", 0
default_setup_path:
	.dc.b	"A:\VMX68K\DEFAULT.V68", 0
default_setup_dir:
	.dc.b	"A:\VMX68K", 0
* The Configuration menu, in Shift_JIS: "設定..." for the dialog, and the CPU with
* its speeds, an item whose shortcut is ESC and whose check is the submenu's
* 'MENU' ID, followed by the CPU's name
config_menu_head:
	.dc.b	$90, $DD, $92, $E8, "...,^", $1B, "|", SPEED_MENU_ID, 0
* The CPUs' names, 6 bytes each
boot_menu_head:
	.dc.b	",^",$1B,"|",2,"Boot",0
new_menu_item:
	.dc.b	",",$90,$56,$82,$B5,$82,$A2,$90,$DD,$92,$E8,"...",0
boot_items:
	.dc.b	"STD",0,"FD0",0,"FD1",0,"FD2",0,"FD3",0
	.dc.b	"SCSI0",0,"SCSI1",0,"SCSI2",0,"SCSI3",0,"SCSI4",0,"SCSI5",0,"SCSI6",0,"SCSI7",0,"ROM...",0
setup_option:
	.dc.b	"/setup"
new_default:
	.dc.b	17,"A:\VMX68K\NEW.V68",0
new_exists:
	.dc.b	$93, $AF, $82, $B6, $96, $BC, $91, $4F, $82, $CC, $90, $DD, $92, $E8, $83, $74, $83, $40, $83, $43, $83, $8B, $82, $AA, $82, $A0, $82, $E8, $82, $DC, $82, $B7, $81, $42, 0
new_error:
	.dc.b	$90, $DD, $92, $E8, $82, $CC, $8D, $EC, $90, $AC, $82, $DC, $82, $BD, $82, $CD, $8B, $4E, $93, $AE, $82, $AA, $82, $C5, $82, $AB, $82, $DC, $82, $B9, $82, $F1, $81, $42, 0
rom_error:
	.dc.b	$8B, $F4, $90, $94, $82, $CC, $52, $4F, $4D, $83, $41, $83, $68, $83, $8C, $83, $58, $82, $F0, $31, $36, $90, $69, $90, $94, $36, $8C, $85, $88, $C8, $93, $E0, $82, $C5, $93, $FC, $97, $CD, $82, $B5, $82, $C4, $82, $AD, $82, $BE, $82, $B3, $82, $A2, $81, $42, 0
media_error:
	.dc.b	$91, $CE, $89, $9E, $82, $B7, $82, $E9, $46, $44, $83, $43, $83, $81, $81, $5B, $83, $57, $82, $F0, $8A, $4A, $82, $AF, $82, $DC, $82, $B9, $82, $F1, $81, $42, 0
	.even
cpu_names:
	.dc.b	"68000", 0, "68030", 0
* Items of the speed submenus, in the order of MSX68K_FN_VM_SPEED's speeds;
* Burst is "最高速"
speed_items_68000:
	.dc.b	"10MHz", 0, "16MHz", 0, "24MHz", 0, $8D, $C5, $8D, $82, $91, $AC, 0
speed_items_68030:
	.dc.b	"25MHz", 0, $8D, $C5, $8D, $82, $91, $AC, 0
* The dialogs' static texts: item.b, then a Pascal string, up to item 0
config_labels:
	.dc.b	7, 8, $56, $4D, $82, $CC, $90, $DD, $92, $E8		; VMの設定
	.dc.b	8, 3, "CPU"
	.dc.b	9, 6, $83, $81, $83, $82, $83, $8A		; メモリ
	.dc.b	10, 5, "68000"
	.dc.b	11, 5, "68030"
	.dc.b	15, 8, $83, $68, $83, $89, $83, $43, $83, $75		; ドライブ
	.dc.b	16, 4, $8B, $A4, $97, $4C		; 共有
	.dc.b	17, 8, $83, $4A, $83, $58, $83, $5E, $83, $80		; カスタム
	.dc.b	0
drive_labels:
	.dc.b	6, 14, $83, $68, $83, $89, $83, $43, $83, $75, $82, $CC, $8F, $DA, $8D, $D7		; ドライブの詳細
	.dc.b	7, 10, $83, $68, $83, $89, $83, $43, $83, $75, $90, $94		; ドライブ数
	.dc.b	8, 4, $94, $D4, $8D, $86		; 番号
	.dc.b	9, 8, $83, $74, $83, $40, $83, $43, $83, $8B		; ファイル
	.dc.b	10, 4, $8B, $A4, $97, $4C		; 共有
	.dc.b	11, 10, $83, $70, $83, $58, $83, $58, $83, $8B, $81, $5B		; パススルー
	.dc.b	12, 8, $8A, $84, $82, $E8, $93, $96, $82, $C4		; 割り当て
	.dc.b	0
	.even
* "VMの再起動で有効になります。今すぐ再起動しますか？"
restart_message:
	.dc.b	$56, $4D, $82, $CC, $8D, $C4, $8B, $4E, $93, $AE, $82, $C5, $97, $4C, $8C, $F8, $82, $C9, $82, $C8, $82, $E8, $82, $DC, $82, $B7, $81, $42, $8D, $A1, $82, $B7, $82, $AE, $8D, $C4, $8B, $4E, $93, $AE, $82, $B5, $82, $DC, $82, $B7, $82, $A9, $81, $48, 0
	.even
win_bounds:
	.dc.w	40, 40, 808, 552+BAR_HEIGHT	; left, top, right, bottom: the bar, then the VM's 768 x 512
* What the rectangles in the work area start as, for the VM's 768 x 512: the bar,
* the line under it, the VM's screen, and the access lamp, black at the top and
* left, white at the bottom and right, and the lamp itself
rects_init:
	.dc.w	0, 0, 768, BAR_HEIGHT-1
	.dc.w	0, BAR_HEIGHT-1, 768, BAR_HEIGHT
	.dc.w	0, BAR_HEIGHT, 768, BAR_HEIGHT+512
	.dc.w	749, 8, 763, 16
	.dc.w	750, 9, 763, 16
	.dc.w	750, 9, 762, 15
RECTS_SIZE	.equ	6*8
config_icon_rect:
	.dc.w	4, 2, 34, 22		; 30 x 20, the 3:2 of SX-Window's icons

prog_end:
	.end	_start
