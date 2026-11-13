* Test-only hidden control task. Requests use the production VMX disk server.
test_start:
	movea.l a1,a5
	lea (a5),a0
	move.w #VARS_SIZE/2-1,d0
test_clear:
	clr.w (a0)+
	dbra d0,test_clear
	move.w #-1,drive0_fd(a5)
	move.l #-1,pending_old(a5)
	move.l #-1,pending_old+4(a5)
	SXCALL TSGetID
	move.l d0,app_task_id(a5)
	move.l d0,-(sp)
	move.w #-1,-(sp)
	move.l #-1,-(sp)
	move.w #$0310,-(sp)
	clr.w -(sp)
	pea win_title(pc)
	pea win_bounds(pc)
	clr.l -(sp)
	SXCALL WMOpen
	lea 26(sp),sp
	move.l a0,win_ptr(a5)
	moveq #MSX68K_FN_VM_OPEN,d0
	move.l a0,d1
	moveq #0,d2
	moveq #0,d3
	moveq #24,d4
	moveq #0,d5
	moveq #0,d6
	moveq #4,d7
	suba.l a2,a2
	bsr msx68k_call
	move.l d0,vm_id(a5)
	bra event_loop

* Private test-controller messages carried by the normal file bridge.
test_run:
	lea 4(a0),a1
test_command:
	tst.b (a1)+
	bne test_command
	clr.l -(sp)
	clr.l -(sp)
	pea (a1)
	pea 4(a0)
	clr.w -(sp)
	SXCALL $A353
	lea 18(sp),sp
	bra serve_result

test_fds:
	movem.l d4-d7/a2,-(sp)
	lea disk_buffer(a5),a2
	moveq #0,d7
	move.w #255,d6
test_dup:
	clr.w -(sp)
	DOS _DUP
	addq.l #2,sp
	tst.l d0
	bmi test_close_dups
	move.w d0,(a2)+
	addq.l #1,d7
	dbra d6,test_dup
test_close_dups:
	move.w d7,d6
	subq.w #1,d6
	bmi test_fds_done
test_close_dup:
	move.w -(a2),-(sp)
	DOS _CLOSE
	addq.l #2,sp
	dbra d6,test_close_dup
test_fds_done:
	move.l d7,d3
	movem.l (sp)+,d4-d7/a2
	bra serve_reply

test_show:
	move.l win_ptr(a5),-(sp)
	SXCALL WMShow
	SXCALL WMSelect
	addq.l #4,sp
	moveq #0,d3
	bra serve_reply

test_hide:
	move.l win_ptr(a5),-(sp)
	SXCALL WMHide
	addq.l #4,sp
	moveq #0,d3
	bra serve_reply
