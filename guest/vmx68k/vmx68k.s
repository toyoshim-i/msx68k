* -----------------------------------------------------------------------------
* VMX68K.X: shows a virtual X68000 in an SX-Window window (docs/VMX68K.md)
* The host runs the VM on a thread of its own and lays its screen over the
* window's content, so this task only opens the window, polls the host for the
* VM's requests on every pass of its event loop, and closes the VM at the end.
* Assembled by rhas and linked by rhlk
* -----------------------------------------------------------------------------

	.include "doscall.mac"
	.include "iocscall.mac"
	.include "sxcall.mac"
	.include "msx68k.mac"

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

	* Ask the host for a VM; without msx68k there is none, and the task ends
	moveq.l	#MSX68K_FN_VM_OPEN, d0
	move.l	win_ptr(pc), d1
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
	moveq.l	#MSX68K_FN_VM_POLL, d0
	move.l	vm_id(pc), d1
	bsr	msx68k_call

	pea	event_rec(pc)
	move.w	#$3282, -(sp)		; EM_MSLDOWN | EM_UPDATE | EM_ACTIVATE | EM_SYSTEM1 | EM_SYSTEM2
	SXCALL	TSEventAvail		; $A357
	addq.l	#6, sp

	cmpi.w	#$DFFE, d0		; ER_ABORT
	beq	quit_app

	move.w	event_what(pc), d0
	cmpi.w	#1, d0			; E_MSLDOWN
	beq.s	handle_mouse_down
	cmpi.w	#7, d0			; E_UPDATE
	beq.s	handle_update
	cmpi.w	#9, d0			; E_ACTIVATE
	beq.s	handle_activate
	cmpi.w	#12, d0			; E_SYSTEM1
	beq.s	handle_system
	cmpi.w	#13, d0			; E_SYSTEM2
	beq.s	handle_system
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
	beq.s	quit_app
	cmpi.w	#2, d0			; CLOSEALL
	beq.s	quit_app
	cmpi.w	#32, d0			; WINDOWSELECT
	bne.s	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra.s	event_loop

handle_update:
	* Nothing to draw: the host shows the VM's screen over the content
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	bne	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMUpdate		; $A20D
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
	beq.s	quit_app
	bra	event_loop

quit_app:
	moveq.l	#MSX68K_FN_VM_CLOSE, d0
	move.l	vm_id(pc), d1
	bsr	msx68k_call
dispose_window:
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMDispose		; $A1FC
	addq.l	#4, sp
do_ts_exit:
	clr.l	-(sp)
	SXCALL	TSExit			; $A352
	clr.w	-(sp)
	DOS	_EXIT2

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
	.even
win_bounds:
	.dc.w	40, 40, 808, 552	; left, top, right, bottom: 768 x 512, the VM's screen

win_ptr:
	.dc.l	0
app_task_id:
	.dc.l	0
vm_id:
	.dc.l	0
win_active:
	.dc.b	0
	.even

event_rec:
event_what:
	.dc.w	0			; +0: what
event_win:
	.dc.l	0			; +2: whom.win
	.dc.l	0			; +6: when
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

prog_end:
	.end	_start
