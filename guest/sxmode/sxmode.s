* -----------------------------------------------------------------------------
* SXMODE.X: Dynamic Screen Resolution Switcher & Interactive GUI for SX-Window
* Assembled by rhas and linked by rhlk
* -----------------------------------------------------------------------------

	.include "doscall.mac"
	.include "iocscall.mac"
	.include "sxcall.mac"

	.text
	.globl	_start
	.globl	gui_start

objr_header:
	.dc.b	'O','B','J','R'
	.dc.l	prog_end - objr_header		; +0x04: text size
	.dc.l	gui_start - objr_header		; +0x08: SX-Window GUI entry point offset from text start!
	.dc.l	$00000800			; +0x0C: stack size (2048 bytes)
	.dc.l	$00001800			; +0x10: heap/work area size (6144 bytes)
	.dc.l	0, 0, 0				; +0x14..+0x1C: reserved (12 bytes)

_start:
	* Entry point from Human68k DOS _EXEC (CLI)
	* A0 = PSP pointer
	* A1 = End of program memory block
	* A2 = Human68k command line buffer: 0(a2) = length, 1(a2) = string

	* Check CLI arguments:
	cmpa.w	#0, a2
	beq.s	no_cli_args
	move.l	a2, d0
	btst	#0, d0
	bne.s	no_cli_args		; Address must be even
	movea.l	a2, a1
	tst.b	(a1)+
	beq.s	no_cli_args

skip_spaces:
	move.b	(a1)+, d0
	beq.s	no_cli_args
	cmpi.b	#13, d0
	beq.s	no_cli_args
	cmpi.b	#' ', d0
	beq.s	skip_spaces
	cmpi.b	#9, d0
	beq.s	skip_spaces

	* First non-whitespace argument character found!
	subq.l	#1, a1

	cmpi.b	#'0', (a1)
	beq.s	set_mode_0
	cmpi.b	#'7', (a1)
	beq.s	set_mode_0

	cmpi.b	#'1', (a1)
	bne.s	check_2

	cmpi.b	#'0', 1(a1)
	bne.s	set_mode_1	; '1' -> Mode 1 (1024x768)

	* '1024'
	cmpi.b	#'x', 4(a1)
	beq.s	set_mode_1
	cmpi.b	#'X', 4(a1)
	beq.s	set_mode_1
	bra.s	set_mode_2	; '1024' without x -> Mode 2 (1024x1024)

check_2:
	cmpi.b	#'2', (a1)
	beq.s	set_mode_2
	bra.s	set_mode_0

set_mode_0:
	moveq.l	#0, d1
	bra.s	apply_cli_mode

set_mode_1:
	moveq.l	#1, d1
	bra.s	apply_cli_mode

set_mode_2:
	moveq.l	#2, d1

apply_cli_mode:
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	move.w	d0, -(sp)
	DOS	_EXIT2

no_cli_args:
	* No CLI arguments: check if SX-Window (SYS/FSX.X) is active!
	* Check Vector #10 (Line 1010 Emulator / A-Line at $0000_0028)
	move.l	($00000028).w, d0
	cmpi.l	#$00E00000, d0
	bcc.s	fallback_cycle_mode	; >= $E00000 means IPLROM/BIOS default stub (no SX-Window)
	tst.l	d0
	beq.s	fallback_cycle_mode

	* Running under SX-Window! Launch interactive GUI dialog
	bra.s	gui_start

fallback_cycle_mode:
	* Plain Human68k console fallback: cycle mode 0 -> 1 -> 2 -> 0
	moveq.l	#-1, d1
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	addq.w	#1, d0
	cmpi.w	#3, d0
	bcs.s	apply_cycled
	clr.w	d0

apply_cycled:
	move.w	d0, d1
	ext.l	d1
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	move.w	d0, -(sp)
	DOS	_EXIT2

gui_start:
	* Entered from SX-Window GUI launch (OBJR[+0x08]) or from CLI fallback!
	* When launched by SX-Window Process Manager:
	*   A0 points to Task Control Block, and 8(A0) contains the Task ID!
	moveq.l	#0, d2
	cmpa.w	#0, a0
	beq.s	check_task_id
	move.l	8(a0), d2		; d2 = Task ID from TDB[8]!

check_task_id:
	* Switch SP to application stack:
	lea	app_stack_top(pc), sp

	* Ensure we have a valid positive Task ID:
	tst.l	d2
	bgt.s	init_app
	* Query TSGetID(-1) if Task ID wasn't positive:
	move.w	#-1, -(sp)
	SXCALL	TSGetID
	addq.l	#2, sp
	move.l	d0, d2
	tst.l	d2
	bgt.s	init_app
	moveq.l	#1, d2

init_app:
	lea	app_task_id(pc), a1
	move.l	d2, (a1)

	* Initialize SX-Window application:
	* TSInitApp ($A350) takes:
	*   Arg 1: work_area buffer pointer (pea work_area(pc))
	*   Arg 2: Task ID (move.l d2, -(sp))
	*   Arg 3: flags = 0 (clr.l -(sp))
	clr.l	-(sp)			; Arg 3: flags = 0
	move.l	d2, -(sp)		; Arg 2: Task ID
	pea	work_area(pc)		; Arg 1: 6 KB work area buffer
	SXCALL	TSInitApp		; $A350
	lea	12(sp), sp

	* Open SXMODE Window:
	* bounds: top=120, left=180, bottom=270, right=400 (w=220, h=150)
	move.l	app_task_id(pc), -(sp)	; refCon (Task ID)
	move.w	#-1, -(sp)		; goAwayFlag (-1)
	move.l	#-1, -(sp)		; behind (-1)
	move.w	#$0200, -(sp)		; procID ($0200 = document window)
	move.w	#-1, -(sp)		; visible (-1)
	pea	win_title(pc)		; title (Pascal string: 6, "SXMODE")
	pea	win_bounds(pc)		; bounds rect
	clr.l	-(sp)			; wStorage (0 = allocate)
	SXCALL	WMOpen			; $A1F9
	lea	26(sp), sp
	lea	win_ptr(pc), a1
	move.l	a0, (a1)

	* Set Port to Window
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp

	* Set text font & size
	move.w	#0, -(sp)
	SXCALL	GMFontKind		; $A18B
	addq.l	#2, sp
	move.l	#$000C000D, -(sp)	; 12x13 font
	SXCALL	GMFontSize		; $A18E
	addq.l	#4, sp

	* Draw initial window contents
	bsr	draw_window_contents

event_loop:
	pea	event_rec(pc)
	move.w	#$FFFF, -(sp)
	SXCALL	TSEventAvail		; $A357
	addq.l	#6, sp
	tst.w	d0
	beq.s	event_loop

	move.w	event_what(pc), d0
	cmpi.w	#7, d0			; updateEvt (7) in SX-Window!
	beq	do_update_evt
	cmpi.w	#12, d0			; appQuitEvt (12)
	beq	quit_app
	cmpi.w	#1, d0			; mouseDown (1)
	bne.s	event_loop

do_mouse_down:
	move.l	event_msg(pc), d0	; Clicked WindowPtr
	cmp.l	win_ptr(pc), d0
	bne.s	event_loop

	* Ensure window is front/active and port is set
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp

	* Delegate window drag / close box / content test to SXCallWindM ($A3A2)
	pea	event_rec(pc)
	move.l	win_ptr(pc), -(sp)
	SXCALL	SXCallWindM		; $A3A2
	addq.l	#8, sp
	tst.w	d0
	bmi.s	event_loop
	cmpi.w	#7, d0			; D0 == 7: Close Box confirmed!
	beq	quit_app
	cmpi.w	#3, d0			; D0 == 3: inContent click!
	bne.s	post_mouse_yield

	* Convert global click point to window-local coordinates via $A13F!
	* $A13F takes Point BY VALUE (4 bytes) and returns local Point [y:word, x:word] in D0!
	move.l	event_where(pc), -(sp)
	SXCALL	GMGlobalToLocal		; $A13F
	addq.l	#4, sp
	move.w	d0, d2			; d2 = local X (low word)
	swap	d0
	move.w	d0, d3			; d3 = local Y (high word)

	* Check X bounds (common to all 3 buttons: left=20, right=200)
	cmpi.w	#20, d2
	blt.s	post_mouse_yield
	cmpi.w	#200, d2
	bge.s	post_mouse_yield

	* Button 0: y=16..46
	cmpi.w	#16, d3
	blt.s	post_mouse_yield
	cmpi.w	#46, d3
	bge.s	check_btn_1
	moveq.l	#0, d1
	bra.s	apply_gui_mode

check_btn_1:
	* Button 1: y=56..86
	cmpi.w	#56, d3
	blt.s	post_mouse_yield
	cmpi.w	#86, d3
	bge.s	check_btn_2
	moveq.l	#1, d1
	bra.s	apply_gui_mode

check_btn_2:
	* Button 2: y=96..126
	cmpi.w	#96, d3
	blt.s	post_mouse_yield
	cmpi.w	#126, d3
	bge.s	post_mouse_yield
	moveq.l	#2, d1

apply_gui_mode:
	* Switch resolution via hypercall!
	HYPERCALL HYPER_SX_SET_SCREEN_MODE

	* Redraw window buttons so new active mode is highlighted
	bsr	draw_window_contents

post_mouse_yield:
	pea	event_rec(pc)
	move.w	#$FFFF, -(sp)
	SXCALL	TSGetEvent		; $A358
	addq.l	#6, sp
	bra	event_loop

do_update_evt:
	move.l	event_msg(pc), d0	; Target WindowPtr
	cmp.l	win_ptr(pc), d0
	bne	event_loop
	move.l	win_ptr(pc), -(sp)	; Push win_ptr ONCE
	SXCALL	WMUpdate		; $A20D
	bsr	draw_window_contents
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp			; Pop win_ptr ONCE
	bra	event_loop

quit_app:
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMDispose		; $A1FC
	addq.l	#4, sp
	SXCALL	TSExit			; $A352
	clr.w	-(sp)
	DOS	_EXIT2

draw_window_contents:
	* Ensure port is set to our window
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp

	* Query current screen mode
	moveq.l	#-1, d1
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	lea	active_mode(pc), a1
	move.w	d0, (a1)

	* 1. Draw Button 0 (" 768 x 512 ")
	pea	btn0_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	pea	btn0_rect(pc)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp
	move.l	#$00220030, -(sp)	; y=34, x=48
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_mode0(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 2. Draw Button 1 ("1024 x 768 ")
	pea	btn1_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	pea	btn1_rect(pc)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp
	move.l	#$004A0030, -(sp)	; y=74, x=48
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_mode1(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 3. Draw Button 2 ("1024 x 1024")
	pea	btn2_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	pea	btn2_rect(pc)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp
	move.l	#$00720028, -(sp)	; y=114, x=40
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_mode2(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 4. Invert active button ($A1A3: push mode word, push Rect * ptr, pop 6 bytes)
	move.w	active_mode(pc), d0
	beq.s	invert_0
	cmpi.w	#1, d0
	beq.s	invert_1
	clr.w	-(sp)			; mode = 0
	pea	btn2_rect(pc)		; rect
	bra.s	do_invert
invert_0:
	clr.w	-(sp)			; mode = 0
	pea	btn0_rect(pc)		; rect
	bra.s	do_invert
invert_1:
	clr.w	-(sp)			; mode = 0
	pea	btn1_rect(pc)		; rect
do_invert:
	SXCALL	GMInvertRect		; $A1A3
	addq.l	#6, sp
	rts

* -----------------------------------------------------------------------------
* Data Section
* -----------------------------------------------------------------------------
win_title:
	.dc.b	6, "SXMODE", 0		; Pascal string (length byte + ASCII + null)
	.even

win_bounds:
	.dc.w	120, 180, 270, 400	; top, left, bottom, right (w=220, h=150)

btn0_rect:
	.dc.w	16, 20, 46, 200		; top, left, bottom, right

btn1_rect:
	.dc.w	56, 20, 86, 200

btn2_rect:
	.dc.w	96, 20, 126, 200

str_mode0:
	.dc.b	11, " 768 x 512 ", 0
	.even

str_mode1:
	.dc.b	11, "1024 x 768 ", 0
	.even

str_mode2:
	.dc.b	11, "1024 x 1024", 0
	.even

win_ptr:
	.dc.l	0
app_task_id:
	.dc.l	0
active_mode:
	.dc.w	0

event_rec:
event_what:
	.dc.w	0
event_msg:
	.dc.l	0
event_when:
	.dc.l	0
event_where:
	.dc.l	0
event_mods:
	.dc.w	0

	.even
work_area:
	.ds.b	6144			; 6 KB work area required by $A350 TSInitApp (clears 5,962 bytes!)
	.even

app_stack:
	.ds.b	2048			; 2 KB application stack
	.even
app_stack_top:
	.even

prog_end:
	.end	_start
