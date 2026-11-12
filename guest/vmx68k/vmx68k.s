* -----------------------------------------------------------------------------
* VMX68K.X: shows a virtual X68000 in an SX-Window window (docs/VMX68K.md)
* The host runs the VM on a thread of its own and lays its screen over the
* window's content below an icon bar, so this task opens the window, draws the
* bar and pops up its menus, carries out the VM's disk requests when the host
* sends E_USER1 for them, and closes the VM at the end. The bar's icons and the
* menus' submenus are resources of VMX68K.LB, next to this program.
* Drive 0 of the VM is the image at DRIVE0_PATH, read and written through
* Human68k here, so the VM's disks live on the master VM's drives.
* Assembled by rhas and linked by rhlk
* -----------------------------------------------------------------------------

	.include "doscall.mac"
	.include "iocscall.mac"
	.include "sxcall.mac"
	.include "msx68k.mac"

BAR_HEIGHT	.equ	24		; Lines of the icon bar above the VM's screen
SPEED_MENU_ID	.equ	1		; 'MENU' of the CPU's speeds in VMX68K.LB
CONFIG_ICON_ID	.equ	1		; 'PLOT' of the Configuration icon in VMX68K.LB
G_THRU		.equ	0		; Clear text, where the host shows the VM's screen
G_WHITE		.equ	8
G_LGRAY		.equ	9
G_BLACK		.equ	11
G_RED		.equ	13		; The access lamp, lit
G_DGRAY		.equ	10		; The access lamp, out

	.text
	.globl	_start
	.globl	gui_start

objr_header:
	.dc.b	'O','B','J','R'
	.dc.l	prog_end - objr_header		; +0x04: text size
	.dc.l	gui_start - objr_header		; +0x08: SX-Window GUI entry point offset
	.dc.l	$00002000			; +0x0C: stack size (8192 bytes)
	.dc.l	$00000000			; +0x10: heap/work area size (0 bytes)
	.dc.l	0, 0, 0				; +0x14..+0x1C: reserved (12 bytes)

_start:
	* Run from the Human68k command line: it needs SX-Window
	pea	str_needs_sx(pc)
	DOS	_PRINT
	addq.l	#4, sp
	move.w	#1, -(sp)
	DOS	_EXIT2

gui_start:
	lea	app_stack_top(pc), sp

	SXCALL	TSGetID			; $A360
	lea	app_task_id(pc), a1
	move.l	d0, (a1)

	* Open the window, hidden until the VM is there. Its content is the VM's screen.
	move.l	app_task_id(pc), -(sp)	; refCon
	move.w	#-1, -(sp)		; goAwayFlag
	move.l	#-1, -(sp)		; behind = W_FRONT
	move.w	#$0310, -(sp)		; procID = WI_STD2 << 4
	clr.w	-(sp)			; visible = FALSE
	pea	win_title(pc)
	pea	win_bounds(pc)
	clr.l	-(sp)			; wStorage = NULL
	SXCALL	WMOpen			; $A1F9
	lea	26(sp), sp
	lea	win_ptr(pc), a1
	move.l	a0, (a1)
	move.l	a0, d0
	beq	do_ts_exit

	* Open VMX68K.LB and read all of it, leaving no file open; without it the bar
	* stays empty
	pea	library_path(pc)
	SXCALL	TSResOpen		; $A4A1
	addq.l	#4, sp
	move.l	a0, d0
	beq.s	no_library
	SXCALL	TSResLoad		; $A4A5
	lea	library_open(pc), a1
	st	(a1)
no_library:

	* Open drive 0's image, and find its size; without it the drive stays empty
	move.w	#2, -(sp)		; read and write
	pea	drive0_path(pc)
	DOS	_OPEN
	addq.l	#6, sp
	tst.l	d0
	bmi.s	no_drive0
	lea	drive0_fd(pc), a1
	move.w	d0, (a1)
	move.w	#2, -(sp)		; from the end
	clr.l	-(sp)
	move.w	d0, -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi.s	no_drive0
	lea	drive0_size(pc), a1
	move.l	d0, (a1)
no_drive0:

	* Count the master's drives from A:, which a VM in bypass mode sees as its own
	moveq.l	#0, d3
count_drives:
	addq.w	#1, d3
	cmpi.w	#26, d3
	bhi.s	drives_counted
	pea	disk_buffer(pc)
	move.w	d3, -(sp)
	DOS	_GETDPB
	addq.l	#6, sp
	tst.l	d0
	bpl.s	count_drives
drives_counted:
	subq.w	#1, d3

	* Ask the host for a VM; without msx68k there is none, and the task ends
	moveq.l	#MSX68K_FN_VM_OPEN, d0
	move.l	win_ptr(pc), d1
	move.l	drive0_size(pc), d2
	moveq.l	#BAR_HEIGHT, d4		; the VM's screen goes below the bar
	bsr	msx68k_call
	tst.l	d0
	bmi	dispose_window
	lea	vm_id(pc), a1
	move.l	d0, (a1)

	move.l	win_ptr(pc), -(sp)
	SXCALL	WMShow			; $A207
	addq.l	#4, sp
	lea	win_active(pc), a1
	st	(a1)

event_loop:

	pea	event_rec(pc)
	* The host sends E_USER1 to the window when the VM has disk requests
	move.w	#$728A, -(sp)		; EM_USER1 | EM_MSLDOWN | EM_MSRDOWN | EM_UPDATE | EM_ACTIVATE | EM_SYSTEM1 | EM_SYSTEM2
	SXCALL	TSEventAvail		; $A357
	addq.l	#6, sp

	cmpi.w	#$DFFE, d0		; ER_ABORT
	beq	quit_app

	move.w	event_what(pc), d0
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
	lea	win_active(pc), a1
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	seq	(a1)
	bra.s	event_loop

handle_system:
	move.w	event_what2(pc), d0
	cmpi.w	#1, d0			; ENDTSK
	beq	quit_app
	cmpi.w	#2, d0			; CLOSEALL
	beq	quit_app
	cmpi.w	#32, d0			; WINDOWSELECT
	bne.s	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra	event_loop

handle_update:
	* The bar, and the rest clear for the VM's screen
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	bne	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMUpdate		; $A20D
	bsr	draw_bar
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp
	bra	event_loop

handle_mouse_down:
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	bne	event_loop
	tst.b	win_active(pc)
	bne.s	track_window
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra	event_loop
track_window:
	* Dragging and the close box
	pea	event_rec(pc)
	move.l	win_ptr(pc), -(sp)
	SXCALL	SXCallWindM		; $A3A2
	addq.l	#8, sp
	cmpi.w	#7, d0			; W_INCLOSE
	beq	quit_app
	bra	event_loop

handle_right_down:
	* The right button on an icon of the bar pops up its menu, as SX-Window's menus
	* follow the right button
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	bne	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	move.l	event_where(pc), -(sp)
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
	move.l	vm_id(pc), d1
	bsr	msx68k_call
	move.w	drive0_fd(pc), d0
	bmi.s	dispose_window
	move.w	d0, -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
dispose_window:
	* VMX68K.LB goes without being written back
	tst.b	library_open(pc)
	beq.s	library_closed
	SXCALL	TSResRemove		; $A4A4
library_closed:
	move.l	win_ptr(pc), -(sp)
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
	move.l	win_ptr(pc), -(sp)
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
	pea	bar_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; G_FORE << 8 | G_PSET
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	bar_line_rect(pc)
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
	pea	screen_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	move.w	d2, -(sp)
	SXCALL	GMAPage			; $A149
	addq.l	#2, sp

	* The Configuration icon, a plot image whose planes follow its rectangle
	tst.b	library_open(pc)
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
	move.l	vm_id(pc), d1
	bsr	msx68k_call
	tst.l	d0
	bmi	update_lamp_done
	tst.l	d1
	beq.s	update_lamp
	cmp.l	screen_size(pc), d1
	beq.s	update_lamp
	bsr	resize_window
update_lamp:
	andi.b	#1, d0			; the lamp
	cmp.b	lamp_lit(pc), d0
	beq.s	update_lamp_done
	lea	lamp_lit(pc), a1
	move.b	d0, (a1)
	bsr	draw_lamp
update_lamp_done:
	movem.l	(sp)+, d0-d3/a1
	rts

* -----------------------------------------------------------------------------
* Helper: resize_window
* Sizes the window to a VM screen of D1.L = width << 16 | height below the bar,
* moving what is drawn by the right edge, and draws the bar anew.
* -----------------------------------------------------------------------------
resize_window:
	movem.l	d0-d2/a1, -(sp)
	lea	screen_size(pc), a1
	move.l	d1, (a1)
	move.w	d1, d2			; height
	swap	d1			; width
	lea	bar_rect(pc), a1
	move.w	d1, 4(a1)
	lea	bar_line_rect(pc), a1
	move.w	d1, 4(a1)
	lea	screen_rect(pc), a1
	move.w	d1, 4(a1)
	move.w	d2, d0
	addi.w	#BAR_HEIGHT, d0
	move.w	d0, 6(a1)
	* The lamp, 19 dots in from the right
	lea	lamp_rects(pc), a1
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
	move.l	win_ptr(pc), -(sp)
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
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp
	move.w	#$000F, -(sp)		; G_ALLPAGE
	SXCALL	GMAPage			; $A149
	addq.l	#2, sp
	move.w	d0, d2
	clr.w	-(sp)			; G_FORE << 8 | G_PSET
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	lea	lamp_rects(pc), a1
	move.w	#G_BLACK, d0
	bsr.s	draw_lamp_fill
	move.w	#G_WHITE, d0
	bsr.s	draw_lamp_fill
	move.w	#G_DGRAY, d0
	tst.b	lamp_lit(pc)
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
* Pops up the Configuration menu where the right button went down:
* "設定...", for the configuration dialog still to come, and the CPU's name with its
* speeds as a submenu, the current one checked. A speed chosen is applied to the
* VM at once.
* -----------------------------------------------------------------------------
config_menu:
	movem.l	d0-d3/a0-a2, -(sp)
	tst.b	library_open(pc)
	beq	config_menu_done

	* Check the current speed: rebuild the submenu from its items, with '!' before one
	lea	menu_string(pc), a1
	lea	speed_items(pc), a0
	moveq.l	#0, d1
config_speed_item:
	cmp.w	vm_speed(pc), d1
	bne.s	config_speed_copy
	move.b	#'!', (a1)+
config_speed_copy:
	move.b	(a0)+, d0
	beq.s	config_speed_next
	move.b	d0, (a1)+
	bra.s	config_speed_copy
config_speed_next:
	addq.w	#1, d1
	cmpi.w	#SPEED_COUNT, d1
	bcc.s	config_speed_built
	move.b	#',', (a1)+
	bra.s	config_speed_item
config_speed_built:
	clr.b	(a1)
	move.w	#SPEED_MENU_ID, -(sp)
	move.l	#'MENU', -(sp)
	SXCALL	TSRscGet		; $A4AB
	addq.l	#6, sp
	move.l	a0, d0
	beq	config_menu_done
	clr.w	-(sp)			; MI_PLN
	pea	menu_string(pc)
	move.l	a0, -(sp)
	SXCALL	MNConvert2		; $A26B
	lea	10(sp), sp

	clr.w	-(sp)			; MI_PLN
	pea	config_menu_string(pc)
	clr.l	-(sp)			; a new record
	SXCALL	MNConvert2		; $A26B
	lea	10(sp), sp
	move.l	a0, d0
	beq.s	config_menu_done
	movea.l	a0, a2
	move.l	event_where(pc), -(sp)
	move.l	a2, -(sp)
	SXCALL	MNSelect		; $A268
	addq.l	#8, sp
	move.l	d0, d3
	move.l	a2, -(sp)
	SXCALL	MMHdlDispose		; $A038
	addq.l	#4, sp

	* A speed: the submenu's ID in the high word and its item, from 1, in the low
	move.l	d3, d0
	swap	d0
	cmpi.w	#SPEED_MENU_ID, d0
	bne.s	config_menu_done
	move.w	d3, d2
	subq.w	#1, d2
	cmpi.w	#SPEED_COUNT, d2
	bcc.s	config_menu_done
	ext.l	d2
	moveq.l	#MSX68K_FN_VM_SPEED, d0
	move.l	vm_id(pc), d1
	bsr	msx68k_call
	tst.l	d0
	bne.s	config_menu_done
	lea	vm_speed(pc), a1
	move.w	d2, (a1)
config_menu_done:
	movem.l	(sp)+, d0-d3/a0-a2
	rts

* -----------------------------------------------------------------------------
* Helper: serve_disk
* Carries out the VM's bridge requests until there are none: reads and writes of
* drive 0's image, and the master's DOS calls a VM's remote drives need. The
* kinds are those of BridgeOp in crates/msx68k-bus/src/vmx68k.rs.
* -----------------------------------------------------------------------------
serve_disk:
	movem.l	d0-d3/a0-a1, -(sp)
serve_next:
	moveq.l	#MSX68K_FN_VM_POLL, d0
	move.l	vm_id(pc), d1
	lea	disk_buffer(pc), a0
	move.l	a0, d2
	move.l	#DISK_BUFFER_SIZE, d3
	bsr	msx68k_call
	tst.l	d0
	ble	serve_done		; 0: nothing to do, -1: no such VM
	lea	request(pc), a1
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
	move.w	drive0_fd(pc), d0
	bmi	serve_reply
	clr.w	-(sp)			; from the start
	move.l	request_offset(pc), -(sp)
	move.w	d0, -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi	serve_reply
	move.l	request_length(pc), -(sp)
	pea	disk_buffer(pc)
	move.w	drive0_fd(pc), -(sp)
	move.l	request_kind(pc), d0
	cmpi.l	#1, d0
	bne.s	serve_image_write
	DOS	_READ
	bra.s	serve_image_moved
serve_image_write:
	DOS	_WRITE
serve_image_moved:
	lea	10(sp), sp
	cmp.l	request_length(pc), d0
	bne	serve_reply
	moveq.l	#0, d3
	bra	serve_reply

serve_open:
	* 3: open the path in the buffer with the mode given; the reply is the handle
	move.w	request_offset+2(pc), -(sp)
	pea	disk_buffer(pc)
	DOS	_OPEN
	addq.l	#6, sp
	bra	serve_result

serve_handle_read:
	* 4: read at an offset of an open file; the reply is the count
	clr.w	-(sp)
	move.l	request_offset(pc), -(sp)
	move.w	request_handle+2(pc), -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi	serve_result
	move.l	request_length(pc), -(sp)
	pea	disk_buffer(pc)
	move.w	request_handle+2(pc), -(sp)
	DOS	_READ
	lea	10(sp), sp
	bra	serve_result

serve_close:
	* 5: close an open file
	move.w	request_handle+2(pc), -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
	bra	serve_result

serve_files:
	* 6: _FILES for the path at +64 of the buffer, into its first 53 bytes
	move.w	request_offset+2(pc), -(sp)
	pea	disk_buffer+64(pc)
	pea	disk_buffer(pc)
	DOS	_FILES
	lea	10(sp), sp
	bra	serve_result

serve_nfiles:
	* 7: _NFILES with the search buffer the host put back
	pea	disk_buffer(pc)
	DOS	_NFILES
	addq.l	#4, sp
	bra	serve_result

serve_dskfre:
	* 8: free space of a drive (1 = A:) into the buffer
	pea	disk_buffer(pc)
	move.w	request_offset+2(pc), -(sp)
	DOS	_DSKFRE
	addq.l	#6, sp
	bra	serve_result

serve_mkdir:
	* 9: make the directory named in the buffer
	pea	disk_buffer(pc)
	DOS	_MKDIR
	addq.l	#4, sp
	bra	serve_result

serve_rmdir:
	* 10: remove the directory named in the buffer
	pea	disk_buffer(pc)
	DOS	_RMDIR
	addq.l	#4, sp
	bra	serve_result

serve_delete:
	* 11: delete the file named in the buffer
	pea	disk_buffer(pc)
	DOS	_DELETE
	addq.l	#4, sp
	bra	serve_result

serve_rename:
	* 12: rename the file named in the buffer to the name at +256
	pea	disk_buffer+256(pc)
	pea	disk_buffer(pc)
	DOS	_RENAME
	addq.l	#8, sp
	bra	serve_result

serve_chmod:
	* 13: set the attribute given, or read it with -1; the reply is the attribute
	move.w	request_offset+2(pc), -(sp)
	pea	disk_buffer(pc)
	DOS	_CHMOD
	addq.l	#6, sp
	bra	serve_result

serve_create:
	* 14: create the file named in the buffer with the attribute given, replacing one
	* there (_CREATE) or not (_NEWFILE); the reply is the handle
	move.w	request_offset+2(pc), -(sp)
	pea	disk_buffer(pc)
	tst.l	request_length(pc)
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
	move.l	request_offset(pc), -(sp)
	move.w	request_handle+2(pc), -(sp)
	DOS	_SEEK
	addq.l	#8, sp
	tst.l	d0
	bmi	serve_result
	move.l	request_length(pc), -(sp)
	pea	disk_buffer(pc)
	move.w	request_handle+2(pc), -(sp)
	DOS	_WRITE
	lea	10(sp), sp
	bra	serve_result

serve_filedate:
	* 16: set the date and time of an open file
	move.l	request_offset(pc), -(sp)
	move.w	request_handle+2(pc), -(sp)
	DOS	_FILEDATE
	addq.l	#6, sp

serve_result:
	move.l	d0, d3
	* What changes the master's storage is written out at once, not left in Human68k's
	* buffers, so nothing a VM wrote is lost if the machine stops
	move.l	request_kind(pc), d0
	cmpi.l	#5, d0			; _CLOSE
	beq.s	serve_flush
	cmpi.l	#9, d0			; _MKDIR and after
	bcs.s	serve_reply
serve_flush:
	DOS	_FFLUSH
serve_reply:
	moveq.l	#MSX68K_FN_VM_REPLY, d0
	move.l	vm_id(pc), d1
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
library_path:
	.dc.b	"A:\X68\VMX68K.LB", 0
* The Configuration menu, in Shift_JIS: "設定..." for the dialog, not there yet,
* and the CPU with its speeds, an item whose shortcut is ESC and whose check is
* the submenu's 'MENU' ID
config_menu_string:
	.dc.b	"~", $90, $DD, $92, $E8, "...,^", $1B, "|", SPEED_MENU_ID, "68000", 0
* Items of the speed submenu, in the order of MSX68K_FN_VM_SPEED's speeds; Burst
* is "最高速"
speed_items:
	.dc.b	"10MHz", 0, "16MHz", 0, "24MHz", 0, $8D, $C5, $8D, $82, $91, $AC, 0
SPEED_COUNT	.equ	4
	.even
win_bounds:
	.dc.w	40, 40, 808, 552+BAR_HEIGHT	; left, top, right, bottom: the bar, then the VM's 768 x 512
bar_rect:
	.dc.w	0, 0, 768, BAR_HEIGHT-1
bar_line_rect:
	.dc.w	0, BAR_HEIGHT-1, 768, BAR_HEIGHT
screen_rect:
	.dc.w	0, BAR_HEIGHT, 768, BAR_HEIGHT+512
* The access lamp: black at the top and left, white at the bottom and right, and
* the lamp itself
lamp_rects:
	.dc.w	749, 8, 763, 16
	.dc.w	750, 9, 763, 16
	.dc.w	750, 9, 762, 15
config_icon_rect:
	.dc.w	4, 2, 34, 22		; 30 x 20, the 3:2 of SX-Window's icons

win_ptr:
	.dc.l	0
app_task_id:
	.dc.l	0
vm_id:
	.dc.l	0
drive0_fd:
	.dc.w	-1
	.even
drive0_size:
	.dc.l	0
request:
request_kind:
	.dc.l	0
request_offset:
	.dc.l	0
request_length:
	.dc.l	0
request_handle:
	.dc.l	0
win_active:
	.dc.b	0
library_open:
	.dc.b	0
lamp_lit:
	.dc.b	0			; the access lamp as drawn
	.even
screen_size:
	.dc.l	768 << 16 | 512		; the VM's screen the window fits, width << 16 | height
	.even
* Speed the VM runs at, by MSX68K_FN_VM_SPEED's numbers; VMs start at 10 MHz
vm_speed:
	.dc.w	0
* The speed submenu's items as MNConvert2 takes them, each with a '!' at most
menu_string:
	.ds.b	64
	.even

event_rec:
event_what:
	.dc.w	0			; +0: what
event_win:
	.dc.l	0			; +2: whom.win
	.dc.l	0			; +6: when
event_where:
	.dc.l	0			; +10: where
event_what2:
	.dc.w	0			; +14: ts.what2
	.dc.w	0			; +16: ts.taskID
	.dc.l	0			; +18: padding
	.even

* Window tracking (SXCallWindM) goes deep, past 2 KB, into the data above
app_stack:
	.ds.b	8192
	.even
app_stack_top:
	.even

* A read or write of a whole cylinder, both sides, of any floppy format. It comes last, as
* PC-relative references reach only 32 KB.
DISK_BUFFER_SIZE	.equ	32768
disk_buffer:
	.ds.b	DISK_BUFFER_SIZE
	.even

prog_end:
	.end	_start
