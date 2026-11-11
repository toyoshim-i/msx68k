//! Runs an [`X68kVm`] on a host thread of its own, as VMX68K does for the VMs it shows in
//! SX-Window windows (see `docs/VMX68K.md`).
//!
//! The thread owns the VM and runs it one frame of guest time at a time. Between frames it
//! carries out requests from other threads, which get at the VM only that way.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::vm::X68kVm;

/// Guest time run between two looks at the request queue.
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// How fast the thread runs its VM.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Pace {
    /// Guest time follows host time.
    RealTime,
    /// As fast as the host allows, keeping the VM's cycle timing; for tests.
    Unthrottled,
}

type Request = Box<dyn FnOnce(&mut X68kVm) + Send>;

/// A VM running on a thread of its own.
pub struct VmThread {
    requests: Option<Sender<Request>>,
    thread: Option<JoinHandle<X68kVm>>,
}

impl VmThread {
    /// Starts running `vm` on a new thread.
    pub fn spawn(vm: X68kVm, pace: Pace) -> Self {
        let (requests, queue) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("msx68k-vm".into())
            .spawn(move || run(vm, pace, queue))
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
fn run(mut vm: X68kVm, pace: Pace, queue: Receiver<Request>) -> X68kVm {
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
        run_frame(&mut vm);
        frames += 1;
        if pace == Pace::RealTime {
            let due = start + FRAME * frames;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
    }
}

/// Runs one frame of guest time.
fn run_frame(vm: &mut X68kVm) {
    if vm.is_running_burst() {
        // Burst has no guest clock to follow; a fixed share of instructions keeps the thread
        // responsive to requests.
        vm.run_steps(100_000);
    } else {
        let mhz = vm.timing_config().cpu_mhz.max(1);
        vm.run_cycles(mhz * 1_000_000 / 60);
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
}
