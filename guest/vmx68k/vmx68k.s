* -----------------------------------------------------------------------------
* VMX68K.X: shows a virtual X68000 in an SX-Window window (docs/VMX68K.md)
* The host runs the VM on a thread of its own and lays its screen over the
* window's content, so this task only opens the window, carries out the VM's
* disk requests when the host sends E_USER1 for them, and closes the VM at the end.
* Drive 0 of the VM is the image at DRIVE0_PATH, read and written through
* Human68k here, so the VM's disks live on the master VM's drives.
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
	move.w	#$7282, -(sp)		; EM_USER1 | EM_MSLDOWN | EM_UPDATE | EM_ACTIVATE | EM_SYSTEM1 | EM_SYSTEM2
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
	cmpi.w	#14, d0			; E_USER1: the VM has disk requests
	bne.s	event_loop
	bsr	serve_disk
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
	move.w	drive0_fd(pc), d0
	bmi.s	dispose_window
	move.w	d0, -(sp)
	DOS	_CLOSE
	addq.l	#2, sp
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
	.even
win_bounds:
	.dc.w	40, 40, 808, 552	; left, top, right, bottom: 768 x 512, the VM's screen

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

* A read or write of a whole cylinder, both sides, of any floppy format. It comes last, as
* PC-relative references reach only 32 KB.
DISK_BUFFER_SIZE	.equ	32768
disk_buffer:
	.ds.b	DISK_BUFFER_SIZE
	.even

prog_end:
	.end	_start
