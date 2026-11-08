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
	.dc.l	$00000000			; +0x10: heap/work area size (0 bytes)
	.dc.l	0, 0, 0				; +0x14..+0x1C: reserved (12 bytes)

_start:
	* Entry point from Human68k DOS _EXEC (CLI)
	* A0 = PSP pointer
	* A1 = End of program memory block
	* A2 = Human68k command line buffer: 0(a2) = length, 1(a2) = string

	* Check CLI arguments:
	cmpa.w	#0, a2
	beq.s	no_dos_args
	move.l	a2, d0
	btst	#0, d0
	bne.s	no_dos_args		; Address must be even
	movea.l	a2, a1
	tst.b	(a1)+
	beq.s	no_dos_args

skip_dos_spaces:
	move.b	(a1)+, d0
	beq.s	no_dos_args
	cmpi.b	#13, d0
	beq.s	no_dos_args
	cmpi.b	#' ', d0
	beq.s	skip_dos_spaces
	cmpi.b	#9, d0
	beq.s	skip_dos_spaces

	* First non-whitespace argument character found!
	subq.l	#1, a1

	cmpi.b	#'0', (a1)
	beq.s	set_dos_mode_0
	cmpi.b	#'7', (a1)
	beq.s	set_dos_mode_0

	cmpi.b	#'1', (a1)
	bne.s	check_dos_2

	cmpi.b	#'0', 1(a1)
	bne.s	set_dos_mode_1		; '1' -> Mode 1 (1024x768)

	* '1024'
	cmpi.b	#'x', 4(a1)
	beq.s	set_dos_mode_1
	cmpi.b	#'X', 4(a1)
	beq.s	set_dos_mode_1
	bra.s	set_dos_mode_2		; '1024' without x -> Mode 2 (1024x1024)

check_dos_2:
	cmpi.b	#'2', (a1)
	beq.s	set_dos_mode_2
	bra.s	set_dos_mode_0

set_dos_mode_0:
	moveq.l	#0, d1
	bra.s	apply_dos_mode

set_dos_mode_1:
	moveq.l	#1, d1
	bra.s	apply_dos_mode

set_dos_mode_2:
	moveq.l	#2, d1

apply_dos_mode:
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	move.w	d0, -(sp)
	DOS	_EXIT2

no_dos_args:
	* Plain Human68k console fallback: cycle mode 0 -> 1 -> 2 -> 0
	moveq.l	#-1, d1
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	addq.w	#1, d0
	cmpi.w	#3, d0
	bcs.s	apply_dos_cycled
	clr.w	d0

apply_dos_cycled:
	move.w	d0, d1
	ext.l	d1
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	move.w	d0, -(sp)
	DOS	_EXIT2

gui_start:
	* Entered from SX-Window GUI launch (OBJR[+0x08])!
	* Switch SP to application stack:
	lea	app_stack_top(pc), sp

	* Check if CLI arguments were passed in A2 (e.g. from SXCON):
	cmpa.w	#0, a2
	beq.s	no_gui_cli_args
	move.l	a2, d0
	btst	#0, d0
	bne.s	no_gui_cli_args		; Address must be even
	movea.l	a2, a1
	tst.b	(a1)+
	beq.s	no_gui_cli_args

skip_gui_spaces:
	move.b	(a1)+, d0
	beq.s	no_gui_cli_args
	cmpi.b	#13, d0
	beq.s	no_gui_cli_args
	cmpi.b	#' ', d0
	beq.s	skip_gui_spaces
	cmpi.b	#9, d0
	beq.s	skip_gui_spaces

	* First non-whitespace character found in GUI launch arguments!
	subq.l	#1, a1

	cmpi.b	#'0', (a1)
	beq.s	set_gui_mode_0
	cmpi.b	#'7', (a1)
	beq.s	set_gui_mode_0

	cmpi.b	#'1', (a1)
	bne.s	check_gui_2

	cmpi.b	#'0', 1(a1)
	bne.s	set_gui_mode_1

	cmpi.b	#'x', 4(a1)
	beq.s	set_gui_mode_1
	cmpi.b	#'X', 4(a1)
	beq.s	set_gui_mode_1
	bra.s	set_gui_mode_2

check_gui_2:
	cmpi.b	#'2', (a1)
	beq.s	set_gui_mode_2
	bra.s	no_gui_cli_args

set_gui_mode_0:
	moveq.l	#0, d1
	bra.s	apply_gui_cli_mode

set_gui_mode_1:
	moveq.l	#1, d1
	bra.s	apply_gui_cli_mode

set_gui_mode_2:
	moveq.l	#2, d1

apply_gui_cli_mode:
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	clr.l	-(sp)
	SXCALL	TSExit			; $A352
	clr.w	-(sp)
	DOS	_EXIT2

no_gui_cli_args:
	* No CLI arguments: launch interactive GUI window!
	* Query Task ID via TSGetID (takes 0 stack arguments):
	SXCALL	TSGetID			; $A360
	lea	app_task_id(pc), a1
	move.l	d0, (a1)

	* Open SXMODE Window:
	* bounds: left=180, top=120, right=400, bottom=270 (w=220, h=150)
	* WMOpen arguments (pushed right-to-left, total 26 bytes):
	move.l	app_task_id(pc), -(sp)	; 8: refCon (long, Task ID)
	move.w	#-1, -(sp)		; 7: goAwayFlag (word, TRUE = -1)
	move.l	#-1, -(sp)		; 6: behind (long, W_FRONT = -1)
	move.w	#$0310, -(sp)		; 5: procID (word, WI_STD2 << 4)
	clr.w	-(sp)			; 4: visible (word, FALSE = 0)
	pea	win_title(pc)		; 3: title (pointer, Pascal string)
	pea	win_bounds(pc)		; 2: bounds (pointer to Rect)
	clr.l	-(sp)			; 1: wStorage (long, NULL = 0 -> allocate)
	SXCALL	WMOpen			; $A1F9
	lea	26(sp), sp

	move.l	a0, d0
	lea	win_ptr(pc), a1
	move.l	d0, (a1)
	tst.l	d0
	beq	quit_app

	* Set Port to Window
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp

	* Set Foreground color (G_BLACK = 11) & Background color (G_LGRAY = 9)
	move.w	#11, -(sp)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp

	move.w	#9, -(sp)
	SXCALL	GMBackColor		; $A148
	addq.l	#2, sp

	* Set font transfer mode (0 = G_PSET)
	move.w	#0, -(sp)
	SXCALL	GMFontMode		; $A18D
	addq.l	#2, sp

	* Set font kind (0 = System) & font size (12x13)
	move.w	#0, -(sp)
	SXCALL	GMFontKind		; $A18B
	addq.l	#2, sp

	move.l	#$000C000D, -(sp)
	SXCALL	GMFontSize		; $A18E
	addq.l	#4, sp

	* Show Window
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMShow			; $A207
	addq.l	#4, sp

	* Mark window as initially active (WMShow generates activate & update events)
	lea	win_active(pc), a1
	st	(a1)

event_loop:
	pea	event_rec(pc)
	move.w	#$3282, -(sp)		; EM_MSLDOWN | EM_UPDATE | EM_ACTIVATE | EM_SYSTEM1 | EM_SYSTEM2
	SXCALL	TSEventAvail		; $A357
	addq.l	#6, sp

	cmpi.w	#$DFFE, d0		; ER_ABORT
	beq	quit_app

	move.w	event_what(pc), d0
	cmpi.w	#1, d0			; E_MSLDOWN (1)
	beq.s	handle_mouse_down
	cmpi.w	#7, d0			; E_UPDATE (7)
	beq	handle_update
	cmpi.w	#9, d0			; E_ACTIVATE (9)
	beq.s	handle_activate
	cmpi.w	#12, d0			; E_SYSTEM1 (12)
	beq.s	handle_system
	cmpi.w	#13, d0			; E_SYSTEM2 (13)
	beq.s	handle_system
	bra.s	event_loop

handle_activate:
	* Update win_active flag without calling WMSelect (which generates activate events)
	lea	win_active(pc), a1
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	seq	(a1)			; $FF if our window is activated, $00 if deactivated
	bra	event_loop

handle_system:
	move.w	event_what2(pc), d0
	cmpi.w	#1, d0			; ENDTSK (1)
	beq	quit_app
	cmpi.w	#2, d0			; CLOSEALL (2)
	beq	quit_app
	cmpi.w	#32, d0			; WINDOWSELECT (32)
	bne	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra	event_loop

handle_mouse_down:
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	bne	event_loop

	* If window is not currently active, select it first
	tst.b	win_active(pc)
	bne.s	win_is_active
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMSelect		; $A1FE
	addq.l	#4, sp
	bra	event_loop

win_is_active:
	* Delegate tracking to SXCallWindM ($A3A2)
	pea	event_rec(pc)
	move.l	win_ptr(pc), -(sp)
	SXCALL	SXCallWindM		; $A3A2
	addq.l	#8, sp

	cmpi.w	#7, d0			; W_INCLOSE (7)
	beq	quit_app
	cmpi.w	#3, d0			; W_ININSIDE (3)
	bne.s	post_mouse_event

	* Convert global click coordinates to window-local coordinates:
	move.l	event_where(pc), -(sp)
	SXCALL	GMGlobalToLocal		; $A13F
	addq.l	#4, sp

	move.w	d0, d3			; d3 = local Y (low word)
	swap	d0
	move.w	d0, d2			; d2 = local X (high word)

	* Check horizontal bounds: left=20, right=200
	cmpi.w	#20, d2
	blt.s	post_mouse_event
	cmpi.w	#200, d2
	bge.s	post_mouse_event

	* Button 0: top=40..bottom=62
	cmpi.w	#40, d3
	blt.s	post_mouse_event
	cmpi.w	#62, d3
	bge.s	check_btn_1
	moveq.l	#0, d1
	bra.s	apply_gui_mode

check_btn_1:
	* Button 1: top=68..bottom=90
	cmpi.w	#68, d3
	blt.s	post_mouse_event
	cmpi.w	#90, d3
	bge.s	check_btn_2
	moveq.l	#1, d1
	bra.s	apply_gui_mode

check_btn_2:
	* Button 2: top=96..bottom=118
	cmpi.w	#96, d3
	blt.s	post_mouse_event
	cmpi.w	#118, d3
	bge.s	post_mouse_event
	moveq.l	#2, d1

apply_gui_mode:
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	bsr	draw_window_contents

post_mouse_event:
	bra	event_loop

handle_update:
	move.l	event_win(pc), d0
	cmp.l	win_ptr(pc), d0
	bne	event_loop
	move.l	win_ptr(pc), -(sp)
	SXCALL	WMUpdate		; $A20D
	bsr	draw_window_contents
	SXCALL	WMUpdtOver		; $A20E
	addq.l	#4, sp
	bra	event_loop

quit_app:
	move.l	win_ptr(pc), d0
	beq.s	do_ts_exit
	move.l	d0, -(sp)
	SXCALL	WMDispose		; $A1FC
	addq.l	#4, sp

do_ts_exit:
	clr.l	-(sp)
	SXCALL	TSExit			; $A352
	clr.w	-(sp)
	DOS	_EXIT2

draw_window_contents:
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp

	* Query current screen mode
	moveq.l	#-1, d1
	HYPERCALL HYPER_SX_SET_SCREEN_MODE
	lea	active_mode(pc), a1
	move.w	d0, (a1)

	* 0. Draw Title Label ("Screen Resolution:")
	move.l	#$00140012, -(sp)	; x=20 ($0014), y=18 ($0012)
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_label(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 1. Draw Button 0 (" 768 x 512 ")
	move.w	#$0100, -(sp)		; GMPenMode(G_BACK << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	btn0_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; GMPenMode(G_FORE << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	btn0_rect(pc)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp
	move.l	#$0030002D, -(sp)	; LPoint: x=48 ($0030), y=45 ($002D)
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_mode0(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 2. Draw Button 1 ("1024 x 768 ")
	move.w	#$0100, -(sp)		; GMPenMode(G_BACK << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	btn1_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; GMPenMode(G_FORE << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	btn1_rect(pc)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp
	move.l	#$00300049, -(sp)	; LPoint: x=48 ($0030), y=73 ($0049)
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_mode1(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 3. Draw Button 2 ("1024 x 1024")
	move.w	#$0100, -(sp)		; GMPenMode(G_BACK << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	btn2_rect(pc)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; GMPenMode(G_FORE << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	btn2_rect(pc)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp
	move.l	#$002C0065, -(sp)	; LPoint: x=44 ($002C), y=101 ($0065)
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_mode2(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* 4. Invert active button
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
	.dc.b	6, "SXMODE", 0		; Pascal string
	.even

win_bounds:
	.dc.w	180, 120, 400, 270	; left, top, right, bottom (w=220, h=150)

str_label:
	.dc.b	18, "Screen Resolution:", 0
	.even

btn0_rect:
	.dc.w	20, 40, 200, 62		; left, top, right, bottom

btn1_rect:
	.dc.w	20, 68, 200, 90		; left, top, right, bottom

btn2_rect:
	.dc.w	20, 96, 200, 118	; left, top, right, bottom

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
win_active:
	.dc.b	0
	.even

event_rec:
event_what:
	.dc.w	0			; +0: what (short)
event_win:
	.dc.l	0			; +2: whom.win (Window *)
event_when:
	.dc.l	0			; +6: when (ulong)
event_where:
	.dc.l	0			; +10: where (LPoint: x high, y low)
event_how:
event_what2:
	.dc.w	0			; +14: ts.what2 (short)
event_task_id:
	.dc.w	0			; +16: ts.taskID (short)
	.dc.l	0			; +18: padding
	.even

app_stack:
	.ds.b	2048			; 2 KB application stack
	.even
app_stack_top:
	.even

prog_end:
	.end	_start
