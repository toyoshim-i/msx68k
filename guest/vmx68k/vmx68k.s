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
ITEM_LABELS	.equ	7		; Static texts: the title, CPU, メモリ, 68000, 68030
LABEL_COUNT	.equ	5
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
dlg_type:	.ds.w	1		; DIGet's item type, handle, and rectangle
dlg_hdl:	.ds.l	1
dlg_rect:	.ds.w	4
dlg_ptr:	.ds.l	1		; the dialog's record
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
library_path:	.ds.b	PATH_MAX
setup_path:	.ds.b	PATH_MAX
menu_string:	.ds.b	64		; the speed submenu's items as MNConvert2 takes them
config_string:	.ds.b	32		; the Configuration menu's
size_text:	.ds.b	6		; the memory's size, " 4MB", a Pascal string
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
	move.w	#-1, drive0_fd(a5)
	move.l	#768 << 16 | 512, screen_size(a5)
	move.w	#4, vm_ram(a5)		; what setups that do not say have
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

	* Count the primary's drives from A:, which a VM in bypass mode sees as its own
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
* Helper: take_sram
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
* Helper: take_paths
* From this task's record in disk_buffer, makes library_path, the program's name
* with .LB for its extension, and setup_path, the command line's first word or
* the default setup.
* -----------------------------------------------------------------------------
take_paths:
	movem.l	d0-d1/a0-a2, -(sp)
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
	dbra	d1, take_name
	clr.b	(a1)
take_name_done:
	move.l	a2, d0
	beq.s	take_command
	move.b	#'L', (a2)+
	move.b	#'B', (a2)+
	clr.b	(a2)

take_command:
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
	moveq.l	#PATH_MAX-2, d0
take_command_copy:
	move.b	-1(a0), (a1)+
	subq.w	#1, d1
	bmi.s	take_command_end
	cmpi.b	#' ', (a0)+
	bls.s	take_command_end
	dbra	d0, take_command_copy
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
	movem.l	(sp)+, d0-d1/a0-a2
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
	beq.s	config_menu_done
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
	bra.s	config_menu_done
config_menu_speed:
	* A speed: the submenu's ID in the high word and its item, from 1, in the low
	move.l	d3, d0
	swap	d0
	cmpi.w	#SPEED_MENU_ID, d0
	bne.s	config_menu_done
	move.w	d3, d2
	subq.w	#1, d2
	cmp.w	d4, d2
	bcc.s	config_menu_done
	ext.l	d2
	moveq.l	#MSX68K_FN_VM_SPEED, d0
	move.l	vm_id(a5), d1
	bsr	msx68k_call
	tst.l	d0
	bne.s	config_menu_done
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
	movem.l	d0-d3/a0-a1, -(sp)
	move.w	vm_cpu(a5), dlg_cpu(a5)
	move.w	vm_ram(a5), dlg_ram(a5)
	move.l	#-1, -(sp)		; in front
	clr.l	-(sp)			; its record on the heap
	move.w	#CONFIG_DIALOG_ID, -(sp)
	SXCALL	DMRefer			; $A2C4
	lea	10(sp), sp
	move.l	a0, d0
	beq	config_dialog_done
	move.l	a0, dlg_ptr(a5)
	* The labels, static text items from ITEM_LABELS on
	lea	dialog_labels(pc), a1
	moveq.l	#ITEM_LABELS, d3
config_dialog_label:
	move.w	d3, d0
	bsr	dlg_item
	move.l	a1, -(sp)
	move.l	dlg_hdl(a5), -(sp)
	move.w	dlg_type(a5), -(sp)
	SXCALL	DITSet			; $A2D2
	lea	10(sp), sp
	moveq.l	#0, d0
	move.b	(a1), d0
	lea	1(a1, d0.w), a1
	addq.w	#1, d3
	cmpi.w	#ITEM_LABELS+LABEL_COUNT, d3
	bcs.s	config_dialog_label
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
	SXCALL	GMSetGraph		; $A131
	SXCALL	DMDraw			; $A2C8
	addq.l	#4, sp

config_dialog_loop:
	pea	dialog_filter(pc)
	SXCALL	DMControl		; $A2C7
	addq.l	#4, sp
	cmpi.w	#ITEM_OK, d0
	beq	config_dialog_ok
	cmpi.w	#ITEM_CANCEL, d0
	beq	config_dialog_close
	cmpi.w	#ITEM_68000, d0
	beq	config_dialog_68000
	cmpi.w	#ITEM_68030, d0
	beq	config_dialog_68030
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
	beq	config_dialog_done
config_dialog_changed:
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
	moveq.l	#MSX68K_FN_VM_RESTART, d0
	move.l	vm_id(a5), d1
	move.w	vm_cpu(a5), d2
	swap	d2
	move.w	vm_ram(a5), d2
	move.w	vm_speed(a5), d3
	ext.l	d3
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
	movem.l	(sp)+, d0-d3/a0-a1
	rts

* Disposes of the dialog
dlg_dispose:
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	DMDispose		; $A2C6
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
* Helper: dialog_filter (dlgPtr, eventRec on the stack), which DMControl calls
* with each event: the VM's requests are served and the window drawn meanwhile.
* Returns 0, the event as it is.
* -----------------------------------------------------------------------------
dialog_filter:
	movem.l	d1-d7/a0-a4, -(sp)
	movea.l	8+48(sp), a0		; the event
	move.w	(a0), d0
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
	* The dialog's own, which DMControl draws
	move.l	d0, -(sp)
	SXCALL	WMUpdate		; $A20D
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp
dialog_filter_back:
	move.l	dlg_ptr(a5), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
dialog_filter_done:
	moveq.l	#0, d0
	movem.l	(sp)+, d1-d7/a0-a4
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
SERVE_KINDS	.equ	16

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
cpu_names:
	.dc.b	"68000", 0, "68030", 0
* Items of the speed submenus, in the order of MSX68K_FN_VM_SPEED's speeds;
* Burst is "最高速"
speed_items_68000:
	.dc.b	"10MHz", 0, "16MHz", 0, "24MHz", 0, $8D, $C5, $8D, $82, $91, $AC, 0
speed_items_68030:
	.dc.b	"25MHz", 0, $8D, $C5, $8D, $82, $91, $AC, 0
* The dialog's labels, Pascal strings in the order of its items from ITEM_LABELS:
* "VMの設定", "CPU", "メモリ", "68000", "68030"
dialog_labels:
	.dc.b	8, $56, $4D, $82, $CC, $90, $DD, $92, $E8
	.dc.b	3, "CPU"
	.dc.b	6, $83, $81, $83, $82, $83, $8A
	.dc.b	5, "68000"
	.dc.b	5, "68030"
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
