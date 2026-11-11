* -----------------------------------------------------------------------------
* SXMODE.X: msx68k settings for SX-Window (screen resolution and mouse wheel),
* run from the Filer, the control panel, or the command line (resolution only)
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
	bsr	parse_cli_arg
	bra.s	apply_dos_mode

no_dos_args:
	* Plain Human68k console fallback: cycle mode 0 -> 1 -> 2 -> 0
	moveq.l	#$10, d0
	move.w	#$0150, d1
	moveq.l	#-1, d2
	trap	#15
	* Find current mode index:
	clr.w	d0
	lea	preset_d2_table(pc), a2
find_cycle_loop:
	cmp.l	(a2)+, d1
	beq.s	found_cycle
	addq.w	#1, d0
	cmpi.w	#3, d0
	bcs.s	find_cycle_loop
	clr.w	d0
found_cycle:
	addq.w	#1, d0
	cmpi.w	#3, d0
	bcs.s	apply_dos_cycled
	clr.w	d0

apply_dos_cycled:
	move.w	d0, d1
	* Fall through to apply_dos_mode

apply_dos_mode:
	move.w	d1, -(sp)		; Save target mode_idx for _EXIT2
	bsr	apply_mode_by_index
	bsr	save_config_file
	move.w	(sp)+, -(sp)
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
	bsr	parse_cli_arg

apply_gui_cli_mode:
	bsr	apply_mode_by_index
	bsr	save_config_file
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

	* Open the settings window:
	* bounds: left=180, top=120, right=620, bottom=396 (w=440, h=276)
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
	bne	post_mouse_event

	* Set Port to Window before converting coordinates
	move.l	win_ptr(pc), -(sp)
	SXCALL	GMSetGraph		; $A131
	addq.l	#4, sp

	* Convert global click coordinates to window-local coordinates:
	move.l	event_where(pc), -(sp)
	SXCALL	GMGlobalToLocal		; $A13F
	addq.l	#4, sp

	move.w	d0, d3			; d3 = local Y (low word)
	swap	d0
	move.w	d0, d2			; d2 = local X (high word)

	* Check horizontal bounds: left=20, right=200
	cmpi.w	#20, d2
	blt	check_wheel_click
	cmpi.w	#200, d2
	bge	check_wheel_click

	* Check vertical bounds: top=40, bottom=258
	cmpi.w	#40, d3
	blt	check_wheel_click
	cmpi.w	#258, d3
	bge	check_wheel_click

	* Determine which of the 8 buttons was clicked (height 22, spacing 6, stride 28)
	sub.w	#40, d3
	clr.w	d4
check_click_loop:
	cmpi.w	#22, d3
	bge.s	next_click_btn
	* Inside button d4!
	move.w	d4, d1
	bra.s	apply_gui_mode

next_click_btn:
	sub.w	#28, d3
	bmi.s	post_mouse_event	; Click in 6px gap between buttons
	addq.w	#1, d4
	cmpi.w	#8, d4
	bcs.s	check_click_loop
	bra.s	post_mouse_event

apply_gui_mode:
	bsr	apply_mode_by_index
	bsr	save_config_file
	bsr	draw_window_contents
	bra	event_loop

check_wheel_click:
	* d2, d3 = local click position; find the wheel setting button under it
	lea	wheel_widgets(pc), a2
	lea	wheel_widgets_end(pc), a3
wheel_click_loop:
	cmpa.l	a3, a2
	bcc	post_mouse_event
	cmp.w	(a2), d2
	blt.s	wheel_click_next
	cmp.w	4(a2), d2
	bge.s	wheel_click_next
	cmp.w	2(a2), d3
	blt.s	wheel_click_next
	cmp.w	6(a2), d3
	bge.s	wheel_click_next
	* Store the button's value in its setting, then apply and save the settings
	move.w	10(a2), d0
	subq.w	#1, d0
	add.w	d0, d0
	lea	wheel_reverse(pc), a0
	move.w	12(a2), 0(a0, d0.w)
	bsr	set_wheel_config
	bsr	save_config_file
	bsr	draw_window_contents
	bra.s	post_mouse_event
wheel_click_next:
	lea	WIDGET_SIZE(a2), a2
	bra.s	wheel_click_loop

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

	bsr	query_wheel_config

	* Query current screen mode via IOCS _CRTMOD
	moveq.l	#$10, d0
	move.w	#$0150, d1
	moveq.l	#-1, d2
	trap	#15
	* D1.L = (cur_w << 16) | cur_h, D2.L = fullscreen (1 or 0)

	clr.w	d4
	tst.l	d2
	bne.s	is_fs_active
	lea	preset_d2_table(pc), a2
	clr.w	d3
find_mode_loop:
	cmp.l	(a2)+, d1
	beq.s	found_active_mode
	addq.w	#1, d3
	cmpi.w	#7, d3
	bcs.s	find_mode_loop
	bra.s	store_active_mode
found_active_mode:
	move.w	d3, d4
	bra.s	store_active_mode
is_fs_active:
	moveq.l	#7, d4
store_active_mode:
	lea	active_mode(pc), a1
	move.w	d4, (a1)

	* 0. Draw Title Label ("Screen Resolution:")
	move.l	#$00140012, -(sp)	; x=20, y=18
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_label(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* Draw all 8 buttons (stride 28)
	clr.w	d3
draw_btn_loop:
	* FillRect background
	move.w	#$0100, -(sp)		; GMPenMode(G_BACK << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp

	move.w	d3, d0
	lsl.w	#3, d0			; d0 = d3 * 8 (8 bytes per Rect)
	lea	btn0_rect(pc), a0
	pea	0(a0, d0.w)		; pushes runtime address of btn[d3]_rect!
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp

	* FrameRect outline
	clr.w	-(sp)			; GMPenMode(G_FORE << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp

	move.w	d3, d0
	lsl.w	#3, d0			; d0 = d3 * 8 (8 bytes per Rect)
	lea	btn0_rect(pc), a0
	pea	0(a0, d0.w)		; pushes runtime address of btn[d3]_rect!
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp

	* Move to text pos: x = btn_text_x[d3], y = 45 + d3 * 28
	move.w	d3, d0
	mulu.w	#28, d0
	add.w	#45, d0			; y
	move.w	d3, d1
	add.w	d1, d1			; d3 * 2
	lea	btn_text_x(pc), a0
	move.w	0(a0, d1.w), d1		; x
	swap	d1
	move.w	d0, d1			; (x << 16) | y
	move.l	d1, -(sp)
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp

	* Draw text string
	move.w	d3, d0
	lsl.w	#4, d0			; d0 = d3 * 16 (16 bytes per string)
	lea	str_mode0(pc), a0
	pea	0(a0, d0.w)		; pushes runtime address of str_mode[d3]!
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	addq.w	#1, d3
	cmpi.w	#8, d3
	bcs.s	draw_btn_loop

	* Invert active button
	move.w	#2, -(sp)		; GMForeColor(2) (XOR mask)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp

	move.w	#3, -(sp)		; GMPenMode(3) (G_XOR)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp

	move.w	active_mode(pc), d0
	andi.w	#7, d0
	lsl.w	#3, d0			; d0 = active_mode * 8
	lea	btn0_rect(pc), a0
	pea	0(a0, d0.w)		; pushes runtime address of btn[active_mode]_rect!
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp

	clr.w	-(sp)			; GMPenMode(0) (G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp

	move.w	#11, -(sp)		; GMForeColor(11) (G_BLACK)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp

	bsr	draw_wheel_widgets
	rts

* -----------------------------------------------------------------------------
* Helper: draw_wheel_widgets
* Draws the wheel setting labels and buttons, inverting the selected ones
* -----------------------------------------------------------------------------
draw_wheel_widgets:
	movem.l	d0-d3/a0-a4, -(sp)
	move.l	#$00E60012, -(sp)	; x=230, y=18
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_wheel_dir(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp
	move.l	#$00E6005A, -(sp)	; x=230, y=90
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_wheel_clicks(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp
	move.l	#$00E6009A, -(sp)	; x=230, y=154
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	str_wheel_trackpad(pc)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	lea	wheel_widgets(pc), a2
	lea	wheel_widgets_end(pc), a3
draw_widget_loop:
	cmpa.l	a3, a2
	bcc	draw_widgets_done

	* Background and frame
	move.w	#$0100, -(sp)		; GMPenMode(G_BACK << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	(a2)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; GMPenMode(G_FORE << 8 | G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	(a2)
	SXCALL	GMFrameRect		; $A172
	addq.l	#4, sp

	* Caption, centered for the 6-dot-wide system font
	lea	wheel_strings(pc), a4
	adda.w	8(a2), a4
	moveq.l	#0, d0
	move.b	(a4), d0
	mulu.w	#6, d0
	move.w	4(a2), d1
	sub.w	(a2), d1
	sub.w	d0, d1
	asr.w	#1, d1
	add.w	(a2), d1
	swap	d1
	move.w	2(a2), d1
	addq.w	#5, d1
	move.l	d1, -(sp)
	SXCALL	GMMove			; $A16E
	addq.l	#4, sp
	pea	(a4)
	SXCALL	GMDrawStrL		; $A190
	addq.l	#4, sp

	* Invert the button whose value is the current setting
	move.w	10(a2), d0
	subq.w	#1, d0
	add.w	d0, d0
	lea	wheel_reverse(pc), a0
	move.w	0(a0, d0.w), d0
	cmp.w	12(a2), d0
	bne.s	draw_widget_next
	move.w	#2, -(sp)		; GMForeColor(2) (XOR mask)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp
	move.w	#3, -(sp)		; GMPenMode(3) (G_XOR)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	pea	(a2)
	SXCALL	GMFillRect		; $A173
	addq.l	#4, sp
	clr.w	-(sp)			; GMPenMode(0) (G_PSET)
	SXCALL	GMPenMode		; $A144
	addq.l	#2, sp
	move.w	#11, -(sp)		; GMForeColor(11) (G_BLACK)
	SXCALL	GMForeColor		; $A147
	addq.l	#2, sp

draw_widget_next:
	lea	WIDGET_SIZE(a2), a2
	bra	draw_widget_loop
draw_widgets_done:
	movem.l	(sp)+, d0-d3/a0-a4
	rts

* -----------------------------------------------------------------------------
* Helper: query_wheel_config
* Reads the host wheel settings into wheel_reverse, wheel_clicks, wheel_pixels
* -----------------------------------------------------------------------------
query_wheel_config:
	movem.l	d0-d3/a0, -(sp)
	moveq.l	#0, d0
	HYPERCALL HYPER_SX_WHEEL_CONFIG	; $FE89
	lea	wheel_reverse(pc), a0
	move.w	d1, (a0)+
	move.w	d2, (a0)+
	move.w	d3, (a0)
	movem.l	(sp)+, d0-d3/a0
	rts

* -----------------------------------------------------------------------------
* Helper: set_wheel_config
* Applies wheel_reverse, wheel_clicks, wheel_pixels on the host, keeping the
* values it clamps them to
* -----------------------------------------------------------------------------
set_wheel_config:
	movem.l	d0-d3/a0, -(sp)
	lea	wheel_reverse(pc), a0
	moveq.l	#0, d1
	move.w	(a0)+, d1
	moveq.l	#0, d2
	move.w	(a0)+, d2
	moveq.l	#0, d3
	move.w	(a0), d3
	moveq.l	#1, d0
	HYPERCALL HYPER_SX_WHEEL_CONFIG	; $FE89
	lea	wheel_reverse(pc), a0
	move.w	d1, (a0)+
	move.w	d2, (a0)+
	move.w	d3, (a0)
	movem.l	(sp)+, d0-d3/a0
	rts

* -----------------------------------------------------------------------------
* Helper: append_cfg_line
* Appends the key at (A1), the decimal value of D0.W, and CR LF at (A0)+
* -----------------------------------------------------------------------------
append_cfg_line:
	move.b	(a1)+, (a0)+
	bne.s	append_cfg_line
	subq.l	#1, a0
	bsr	format_dec_u16
	move.b	#13, (a0)+
	move.b	#10, (a0)+
	rts

* -----------------------------------------------------------------------------
* Helper: apply_mode_by_index
* Input: D1.W = mode index (0..7)
* -----------------------------------------------------------------------------
apply_mode_by_index:
	movem.l	d0-d2/a0, -(sp)
	andi.w	#7, d1
	lsl.w	#2, d1
	lea	preset_d2_table(pc), a0
	move.l	0(a0, d1.w), d2		; D2.L = target D2 for _CRTMOD
	moveq.l	#$10, d0		; IOCS _CRTMOD ($10)
	move.w	#$0150, d1		; Extended mode $50
	trap	#15
	movem.l	(sp)+, d0-d2/a0
	rts

* -----------------------------------------------------------------------------
* Helper: save_config_file
* Saves A:\SYS\MSX68K.CFG with SCREEN_WIDTH, SCREEN_HEIGHT, FULLSCREEN
* -----------------------------------------------------------------------------
save_config_file:
	movem.l	d0-d4/a0-a1, -(sp)

	* Query active resolution and fullscreen flag via IOCS _CRTMOD
	moveq.l	#$10, d0
	move.w	#$0150, d1
	moveq.l	#-1, d2
	trap	#15
	* D1.L = (w << 16) | h, D2.L = fullscreen (1 or 0)

	lea	cfg_buf(pc), a0

	* 1. Write "SCREEN_WIDTH="
	lea	str_cfg_w(pc), a1
copy_cfg_w:
	move.b	(a1)+, (a0)+
	bne.s	copy_cfg_w
	subq.l	#1, a0

	* Format width (D1 high word)
	move.l	d1, d0
	swap	d0
	bsr	format_dec_u16

	* Append \r\n
	move.b	#13, (a0)+
	move.b	#10, (a0)+

	* 2. Write "SCREEN_HEIGHT="
	lea	str_cfg_h(pc), a1
copy_cfg_h:
	move.b	(a1)+, (a0)+
	bne.s	copy_cfg_h
	subq.l	#1, a0

	* Format height (D1 low word)
	move.w	d1, d0
	bsr	format_dec_u16

	* Append \r\n
	move.b	#13, (a0)+
	move.b	#10, (a0)+

	* 3. Write "FULLSCREEN="
	lea	str_cfg_fs(pc), a1
copy_cfg_fs:
	move.b	(a1)+, (a0)+
	bne.s	copy_cfg_fs
	subq.l	#1, a0

	* Format fullscreen flag (0 or 1)
	move.b	d2, d0
	andi.b	#1, d0
	addi.b	#'0', d0
	move.b	d0, (a0)+

	* Append \r\n
	move.b	#13, (a0)+
	move.b	#10, (a0)+

	* 4. Wheel settings
	bsr	query_wheel_config
	lea	str_cfg_wheel_reverse(pc), a1
	move.w	wheel_reverse(pc), d0
	bsr	append_cfg_line
	lea	str_cfg_wheel_clicks(pc), a1
	move.w	wheel_clicks(pc), d0
	bsr	append_cfg_line
	lea	str_cfg_wheel_pixels(pc), a1
	move.w	wheel_pixels(pc), d0
	bsr	append_cfg_line

	* Total length = a0 - cfg_buf
	lea	cfg_buf(pc), a1
	move.l	a0, d3
	sub.l	a1, d3			; D3.L = length to write

	* Open/create A:\SYS\MSX68K.CFG using DOS _CREATE ($FF3C)
	move.w	#$0020, -(sp)		; mode: archive/write
	pea	cfg_path(pc)		; path
	DOS	_CREATE			; $FF3C
	addq.l	#6, sp

	tst.l	d0
	bmi	save_cfg_done		; If failed (D0 < 0), return
	move.w	d0, d4			; D4.W = file handle

	* Write buffer using DOS _WRITE ($FF40)
	move.l	d3, -(sp)		; len
	pea	cfg_buf(pc)		; buf
	move.w	d4, -(sp)		; handle
	DOS	_WRITE			; $FF40
	lea	10(sp), sp

	* Close handle using DOS _CLOSE ($FF3E)
	move.w	d4, -(sp)		; handle
	DOS	_CLOSE			; $FF3E
	addq.l	#2, sp

save_cfg_done:
	movem.l	(sp)+, d0-d4/a0-a1
	rts

* -----------------------------------------------------------------------------
* Helper: format_dec_u16
* Formats unsigned 16-bit number in D0.W into ASCII decimal at (A0)+
* -----------------------------------------------------------------------------
format_dec_u16:
	movem.l	d1-d3, -(sp)
	clr.w	d3			; digit count
fmt_dec_div_loop:
	andi.l	#$FFFF, d0
	divu.w	#10, d0
	swap	d0			; remainder in low word
	move.w	d0, -(sp)
	addq.w	#1, d3
	clr.w	d0
	swap	d0			; quotient in low word
	tst.w	d0
	bne.s	fmt_dec_div_loop

fmt_dec_pop_loop:
	move.w	(sp)+, d1
	addi.b	#'0', d1
	move.b	d1, (a0)+
	subq.w	#1, d3
	bne.s	fmt_dec_pop_loop

	movem.l	(sp)+, d1-d3
	rts

* -----------------------------------------------------------------------------
* Helper: parse_cli_arg
* Input: A1 = pointer to first non-whitespace character
* Output: D1.W = mode index (0..7)
* -----------------------------------------------------------------------------
parse_cli_arg:
	cmpi.b	#'0', (a1)
	beq	arg_mode_0
	cmpi.b	#'7', (a1)
	bne	arg_not_7
	cmpi.b	#'6', 1(a1)
	beq	arg_mode_0		; "768" -> Mode 0
	bra	arg_mode_7		; "7" -> Mode 7
arg_not_7:
	cmpi.b	#'f', (a1)
	beq	arg_mode_7
	cmpi.b	#'F', (a1)
	beq.w	arg_mode_7

	cmpi.b	#'1', (a1)
	bne	arg_check_2_to_6

	* Starts with '1':
	cmpi.b	#'0', 1(a1)
	bne	arg_check_12_or_16_or_19

	* Starts with "10" (1024):
	cmpi.b	#'x', 4(a1)
	beq	arg_mode_1		; 1024x768 -> Mode 1
	cmpi.b	#'X', 4(a1)
	beq	arg_mode_1
	bra	arg_mode_2		; 1024 without x -> Mode 2

arg_check_12_or_16_or_19:
	cmpi.b	#'2', 1(a1)		; "1280"
	bne	arg_check_16_or_19
	cmpi.b	#'7', 5(a1)		; "1280x720"
	beq	arg_mode_3
	bra	arg_mode_4		; "1280x960"

arg_check_16_or_19:
	cmpi.b	#'6', 1(a1)		; "1600"
	beq	arg_mode_5
	cmpi.b	#'9', 1(a1)		; "1920"
	beq	arg_mode_6
	bra	arg_mode_1		; "1" -> Mode 1

arg_check_2_to_6:
	cmpi.b	#'2', (a1)
	beq	arg_mode_2
	cmpi.b	#'3', (a1)
	beq	arg_mode_3
	cmpi.b	#'4', (a1)
	beq	arg_mode_4
	cmpi.b	#'5', (a1)
	beq	arg_mode_5
	cmpi.b	#'6', (a1)
	beq	arg_mode_6
	bra	arg_mode_0

arg_mode_0:
	moveq.l	#0, d1
	rts
arg_mode_1:
	moveq.l	#1, d1
	rts
arg_mode_2:
	moveq.l	#2, d1
	rts
arg_mode_3:
	moveq.l	#3, d1
	rts
arg_mode_4:
	moveq.l	#4, d1
	rts
arg_mode_5:
	moveq.l	#5, d1
	rts
arg_mode_6:
	moveq.l	#6, d1
	rts
arg_mode_7:
	moveq.l	#7, d1
	rts

* -----------------------------------------------------------------------------
* Data Section
* -----------------------------------------------------------------------------
win_title:
	.dc.b	11, "MSX68K ", $90, $DD, $92, $E8, 0	; Pascal string "MSX68K 設定" (Shift_JIS)
	.even

win_bounds:
	.dc.w	180, 120, 620, 396	; left, top, right, bottom (w=440, h=276)

* Wheel setting buttons: rect, caption offset from wheel_strings, setting, value
WG_DIRECTION	.equ	1		; wheel_reverse
WG_CLICKS	.equ	2		; wheel_clicks
WG_TRACKPAD	.equ	3		; wheel_pixels
WIDGET_SIZE	.equ	16

wheel_widgets:
	.dc.w	230, 40, 320, 62, str_normal-wheel_strings, WG_DIRECTION, 0, 0
	.dc.w	330, 40, 420, 62, str_reverse-wheel_strings, WG_DIRECTION, 1, 0
	.dc.w	230, 104, 262, 126, str_one-wheel_strings, WG_CLICKS, 1, 0
	.dc.w	268, 104, 300, 126, str_two-wheel_strings, WG_CLICKS, 2, 0
	.dc.w	306, 104, 338, 126, str_three-wheel_strings, WG_CLICKS, 3, 0
	.dc.w	344, 104, 376, 126, str_four-wheel_strings, WG_CLICKS, 4, 0
	.dc.w	382, 104, 414, 126, str_five-wheel_strings, WG_CLICKS, 5, 0
	.dc.w	230, 168, 290, 190, str_slow-wheel_strings, WG_TRACKPAD, 80, 0
	.dc.w	296, 168, 356, 190, str_medium-wheel_strings, WG_TRACKPAD, 40, 0
	.dc.w	362, 168, 422, 190, str_fast-wheel_strings, WG_TRACKPAD, 20, 0
wheel_widgets_end:

wheel_strings:
str_normal:
	.dc.b	6, "Normal", 0
str_reverse:
	.dc.b	7, "Reverse", 0
str_one:
	.dc.b	1, "1", 0
str_two:
	.dc.b	1, "2", 0
str_three:
	.dc.b	1, "3", 0
str_four:
	.dc.b	1, "4", 0
str_five:
	.dc.b	1, "5", 0
str_slow:
	.dc.b	4, "Slow", 0
str_medium:
	.dc.b	6, "Medium", 0
str_fast:
	.dc.b	4, "Fast", 0
	.even

str_wheel_dir:
	.dc.b	16, "Wheel direction:", 0
str_wheel_clicks:
	.dc.b	17, "Lines per notch: ", 0
str_wheel_trackpad:
	.dc.b	15, "Trackpad speed:", 0
	.even

str_label:
	.dc.b	18, "Screen Resolution:", 0
	.even

btn0_rect:
	.dc.w	20, 40, 200, 62		; left, top, right, bottom
btn1_rect:
	.dc.w	20, 68, 200, 90
btn2_rect:
	.dc.w	20, 96, 200, 118
btn3_rect:
	.dc.w	20, 124, 200, 146
btn4_rect:
	.dc.w	20, 152, 200, 174
btn5_rect:
	.dc.w	20, 180, 200, 202
btn6_rect:
	.dc.w	20, 208, 200, 230
btn7_rect:
	.dc.w	20, 236, 200, 258

btn_text_x:
	.dc.w	48, 48, 44, 48, 48, 44, 44, 44

preset_d2_table:
	.dc.l	$03000200	; 0: 768 x 512
	.dc.l	$04000300	; 1: 1024 x 768
	.dc.l	$04000400	; 2: 1024 x 1024
	.dc.l	$050002D0	; 3: 1280 x 720
	.dc.l	$050003C0	; 4: 1280 x 960
	.dc.l	$064004B0	; 5: 1600 x 1200
	.dc.l	$07800438	; 6: 1920 x 1080
	.dc.l	$00000000	; 7: Native Full (D2.L = 0)

str_mode0:
	.dc.b	11, " 768 x 512 ", 0, 0, 0, 0
str_mode1:
	.dc.b	11, "1024 x 768 ", 0, 0, 0, 0
str_mode2:
	.dc.b	11, "1024 x 1024", 0, 0, 0, 0
str_mode3:
	.dc.b	11, "1280 x 720 ", 0, 0, 0, 0
str_mode4:
	.dc.b	11, "1280 x 960 ", 0, 0, 0, 0
str_mode5:
	.dc.b	11, "1600 x 1200", 0, 0, 0, 0
str_mode6:
	.dc.b	11, "1920 x 1080", 0, 0, 0, 0
str_mode7:
	.dc.b	11, "Native Full", 0, 0, 0, 0

str_cfg_w:
	.dc.b	"SCREEN_WIDTH=", 0
str_cfg_h:
	.dc.b	"SCREEN_HEIGHT=", 0
str_cfg_fs:
	.dc.b	"FULLSCREEN=", 0
str_cfg_wheel_reverse:
	.dc.b	"WHEEL_REVERSE=", 0
str_cfg_wheel_clicks:
	.dc.b	"WHEEL_CLICKS=", 0
str_cfg_wheel_pixels:
	.dc.b	"WHEEL_TRACKPAD_PIXELS=", 0
cfg_path:
	.dc.b	"A:\SYS\MSX68K.CFG", 0
	.even

cfg_buf:
	.ds.b	256
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
* Host wheel settings, in this order (indexed by setting number - 1)
wheel_reverse:
	.dc.w	0
wheel_clicks:
	.dc.w	1
wheel_pixels:
	.dc.w	40

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
