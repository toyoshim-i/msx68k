//! Runs an [`X68kVm`] on a host thread of its own, as VMX68K does for the VMs it shows in
//! SX-Window windows (see `docs/VMX68K.md`).
//!
//! The thread owns the VM and runs it one frame of guest time at a time. Between frames it
//! carries out requests from other threads, which get at the VM only that way, and hands the VM
//! to a vertical blank hook after each frame in which the VM's CRTC entered a vertical blank.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::vm::{FrameBudget, X68kVm};

/// Guest time run between two looks at the request queue.
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// Time of each frame a VM in Burst leaves for its vertical blank hook.
const BURST_RESERVE: Duration = Duration::from_millis(3);

/// How fast the thread runs its VM.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Pace {
    /// Guest time follows host time.
    RealTime,
    /// As fast as the host allows, keeping the VM's cycle timing; for tests.
    Unthrottled,
}

type Request = Box<dyn FnOnce(&mut X68kVm) + Send>;
type VblankHook = Box<dyn FnMut(&mut X68kVm) + Send>;

/// A VM running on a thread of its own.
pub struct VmThread {
    requests: Option<Sender<Request>>,
    thread: Option<JoinHandle<X68kVm>>,
}

impl VmThread {
    /// Starts running `vm` on a new thread.
    pub fn spawn(vm: X68kVm, pace: Pace) -> Self {
        Self::spawn_with_vblank(vm, pace, |_| {})
    }

    /// Starts running `vm` on a new thread, calling `on_vblank` there after each frame in which
    /// the VM entered a vertical blank, as a VM's screen is composited.
    pub fn spawn_with_vblank(
        vm: X68kVm,
        pace: Pace,
        on_vblank: impl FnMut(&mut X68kVm) + Send + 'static,
    ) -> Self {
        let (requests, queue) = mpsc::channel();
        let on_vblank: VblankHook = Box::new(on_vblank);
        let thread = thread::Builder::new()
            .name("msx68k-vm".into())
            .spawn(move || run(vm, pace, queue, on_vblank))
            .expect("the VM thread must start");
        Self {
            requests: Some(requests),
            thread: Some(thread),
        }
    }

    /// Runs `f` on the VM between two frames and returns its result.
    pub fn with_vm<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut X68kVm) -> R + Send + 'static,
    ) -> R {
        let (reply, result) = mpsc::channel();
        self.requests
            .as_ref()
            .expect("the VM thread is running")
            .send(Box::new(move |vm| {
                let _ = reply.send(f(vm));
            }))
            .expect("the VM thread must be running");
        result.recv().expect("the VM thread must reply")
    }

    /// Runs `f` on the VM between two frames, without waiting for it, as for input.
    pub fn post(&self, f: impl FnOnce(&mut X68kVm) + Send + 'static) {
        self.requests
            .as_ref()
            .expect("the VM thread is running")
            .send(Box::new(f))
            .expect("the VM thread must be running");
    }

    /// Stops the thread and returns its VM.
    pub fn stop(mut self) -> X68kVm {
        self.requests = None;
        self.thread
            .take()
            .expect("the VM thread is running")
            .join()
            .expect("the VM thread must not panic")
    }
}

impl Drop for VmThread {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs `vm` frame by frame until every sender of `queue` is gone, then returns it.
fn run(mut vm: X68kVm, pace: Pace, queue: Receiver<Request>, mut on_vblank: VblankHook) -> X68kVm {
    let start = Instant::now();
    let mut frames = 0u32;
    loop {
        loop {
            match queue.try_recv() {
                Ok(request) => request(&mut vm),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return vm,
            }
        }
        let vblanks = vm.bus.crtc.frame_count();
        frames += 1;
        let due = start + FRAME * frames;
        if vm.is_running_burst() {
            // Burst has no guest clock to follow: flat out for a frame of host time, as the
            // master VM bursts, leaving time to present the frame when paced.
            let deadline = match pace {
                Pace::RealTime => due.checked_sub(BURST_RESERVE).unwrap_or(due),
                Pace::Unthrottled => Instant::now() + FRAME,
            };
            vm.run_frame(FrameBudget::RealTime { deadline });
        } else {
            let mhz = vm.timing_config().cpu_mhz.max(1);
            vm.run_cycles(mhz * 1_000_000 / 60);
        }
        if vm.bus.crtc.frame_count() != vblanks {
            on_vblank(&mut vm);
        }
        if pace == Pace::RealTime {
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::CpuSpeedMode;
    use crate::vm::VmConfig;

    /// A VM that spins in place: `BRA.S *` at the reset PC.
    fn idle_vm() -> X68kVm {
        let mut vm = X68kVm::new(VmConfig::app_vm().with_speed_mode(CpuSpeedMode::Mhz10));
        for (addr, word) in [(0u32, 0x0001u16), (2, 0x0000), (4, 0x0000), (6, 0x1000)] {
            vm.bus.poke_u8(addr, (word >> 8) as u8);
            vm.bus.poke_u8(addr + 1, word as u8);
        }
        vm.bus.poke_u8(0x1000, 0x60);
        vm.bus.poke_u8(0x1001, 0xFE);
        vm.reset().unwrap();
        vm
    }

    #[test]
    fn real_time_pace_follows_the_host_clock() {
        let thread = VmThread::spawn(idle_vm(), Pace::RealTime);
        let started = Instant::now();
        let start_cycles = thread.with_vm(|vm| vm.cpu_state.total_cycles);
        thread::sleep(Duration::from_millis(500));
        let cycles = thread.with_vm(|vm| vm.cpu_state.total_cycles) - start_cycles;
        let expected = started.elapsed().as_secs_f64() * 10_000_000.0;
        // Within a few frames of host time.
        assert!(
            (cycles as f64 - expected).abs() < expected * 0.2,
            "ran {cycles} cycles in {:?}",
            started.elapsed()
        );
        let vm = thread.stop();
        assert_eq!(vm.cpu_state.pc, 0x1000);
    }

    #[test]
    fn a_vm_switched_to_burst_keeps_host_time_and_takes_requests() {
        for pace in [Pace::RealTime, Pace::Unthrottled] {
            let thread = VmThread::spawn(idle_vm(), pace);
            thread.post(|vm| vm.set_speed_mode(CpuSpeedMode::Burst));
            let start = thread.with_vm(|vm| vm.bus.crtc.frame_count());
            let started = Instant::now();
            thread::sleep(Duration::from_millis(500));
            let frames = thread.with_vm(|vm| vm.bus.crtc.frame_count()) - start;
            // Its peripherals follow host time: about 60 vertical blanks a second.
            let expected = started.elapsed().as_secs_f64() * 60.0;
            assert!(
                (frames as f64 - expected).abs() < expected * 0.3,
                "{pace:?}: {frames} frames in {:?}",
                started.elapsed()
            );
            assert!(thread.with_vm(|vm| vm.is_running_burst()));
        }
    }

    #[test]
    fn posted_requests_run_in_order() {
        let thread = VmThread::spawn(idle_vm(), Pace::Unthrottled);
        for value in 1..=3 {
            thread.post(move |vm| vm.cpu_state.d[0] = vm.cpu_state.d[0] * 10 + value);
        }
        assert_eq!(thread.with_vm(|vm| vm.cpu_state.d[0]), 123);
    }

    #[test]
    fn the_hook_follows_the_vms_vertical_blanks() {
        let (tx, rx) = mpsc::channel();
        let thread = VmThread::spawn_with_vblank(idle_vm(), Pace::Unthrottled, move |vm| {
            let _ = tx.send(vm.bus.crtc.frame_count());
        });
        let counts: Vec<u64> = rx.iter().take(10).collect();
        drop(thread);
        // One call per frame, each after a new vertical blank.
        assert!(counts.windows(2).all(|w| w[1] > w[0]), "{counts:?}");
    }
}
