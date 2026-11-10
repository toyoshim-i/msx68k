//! GDB remote protocol server for debugging guest 68000 code.
//!
//! Guest execution under a debugger goes through [`X68kVm::run_debug_frame`], a loop separate
//! from the regular `run_*` methods, so normal execution carries no per-instruction cost.

use std::convert::Infallible;
use std::io::{self, ErrorKind, Read};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use gdbstub::arch::{Arch, Registers};
use gdbstub::common::Signal;
use gdbstub::stub::state_machine::GdbStubStateMachine;
use gdbstub::stub::{GdbStub, GdbStubError, SingleThreadStopReason};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep,
    SingleThreadSingleStepOps,
};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwBreakpoint, HwBreakpointOps, SwBreakpoint, SwBreakpointOps,
};
use gdbstub::target::{Target, TargetError, TargetResult};
use msx68k_cpu::{BurstPolicy, CpuCore, CpuPolicy, TrackedPolicy};

use crate::vm::{BusTrapHandler, FrameBudget, X68kVm};

/// Size of the `g` packet payload: `D0-D7`, `A0-A7`, `SR`, `PC` as 32-bit big-endian values.
const REGISTER_BYTES: usize = 18 * 4;

/// How long [`GdbServer::poll`] waits for the next packet of an ongoing exchange.
const EXCHANGE_QUIET: Duration = Duration::from_millis(2);
/// Upper bound on one [`GdbServer::poll`], keeping the host window responsive.
const EXCHANGE_LIMIT: Duration = Duration::from_millis(8);

const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
  <architecture>m68k:68000</architecture>
  <feature name="org.gnu.gdb.m68k.core">
    <reg name="d0" bitsize="32" type="int32"/>
    <reg name="d1" bitsize="32" type="int32"/>
    <reg name="d2" bitsize="32" type="int32"/>
    <reg name="d3" bitsize="32" type="int32"/>
    <reg name="d4" bitsize="32" type="int32"/>
    <reg name="d5" bitsize="32" type="int32"/>
    <reg name="d6" bitsize="32" type="int32"/>
    <reg name="d7" bitsize="32" type="int32"/>
    <reg name="a0" bitsize="32" type="data_ptr"/>
    <reg name="a1" bitsize="32" type="data_ptr"/>
    <reg name="a2" bitsize="32" type="data_ptr"/>
    <reg name="a3" bitsize="32" type="data_ptr"/>
    <reg name="a4" bitsize="32" type="data_ptr"/>
    <reg name="a5" bitsize="32" type="data_ptr"/>
    <reg name="fp" bitsize="32" type="data_ptr"/>
    <reg name="sp" bitsize="32" type="data_ptr"/>
    <reg name="ps" bitsize="32" type="int32"/>
    <reg name="pc" bitsize="32" type="code_ptr"/>
  </feature>
</target>
"#;

/// Marker type describing the MC68000 to `gdbstub`.
pub enum M68000 {}

impl Arch for M68000 {
    type Usize = u32;
    type Registers = M68000Registers;
    type BreakpointKind = usize;
    type RegId = ();

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

/// MC68000 registers in GDB's `g` packet order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct M68000Registers {
    d: [u32; 8],
    a: [u32; 8],
    sr: u32,
    pc: u32,
}

impl Registers for M68000Registers {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        let regs = self.d.iter().chain(&self.a).chain([&self.sr, &self.pc]);
        for byte in regs.flat_map(|reg| reg.to_be_bytes()) {
            write_byte(Some(byte));
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() < REGISTER_BYTES {
            return Err(());
        }
        let mut words = bytes
            .chunks_exact(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]));
        let regs = self
            .d
            .iter_mut()
            .chain(&mut self.a)
            .chain([&mut self.sr, &mut self.pc]);
        for (reg, word) in regs.zip(&mut words) {
            *reg = word;
        }
        Ok(())
    }
}

/// Progress through a [`FrameBudget`], retained across a stop in the middle of a frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FrameProgress {
    instructions: usize,
    elapsed_cycles: u32,
    elapsed_ticks: u32,
    tick_accum: usize,
}

/// Debugger state owned by the VM.
#[derive(Debug, Default)]
pub(crate) struct DebugState {
    breakpoints: Vec<u32>,
    single_step: bool,
    frame: FrameProgress,
}

impl DebugState {
    fn add_breakpoint(&mut self, addr: u32) -> bool {
        let addr = addr & 0x00FF_FFFF;
        if !self.breakpoints.contains(&addr) {
            self.breakpoints.push(addr);
        }
        true
    }

    fn remove_breakpoint(&mut self, addr: u32) -> bool {
        let addr = addr & 0x00FF_FFFF;
        match self.breakpoints.iter().position(|&a| a == addr) {
            Some(index) => {
                self.breakpoints.swap_remove(index);
                true
            }
            None => false,
        }
    }
}

/// Why the debug execution loop returned control to the debugger.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum DebugStop {
    Breakpoint,
    Step,
}

impl X68kVm {
    /// Runs guest code until `budget` is used up or the debugger needs to regain control.
    ///
    /// Peripherals advance exactly as in the matching regular `run_*` method, so guest timers
    /// behave the same under a debugger.
    fn run_debug_frame(&mut self, budget: FrameBudget) -> Option<DebugStop> {
        if self.is_running_burst() {
            self.debug_loop::<BurstPolicy>(budget)
        } else {
            self.debug_loop::<TrackedPolicy>(budget)
        }
    }

    fn debug_loop<P: CpuPolicy>(&mut self, budget: FrameBudget) -> Option<DebugStop> {
        let mut core = CpuCore::<P>::from_state(std::mem::take(&mut self.cpu_state));
        let mut handler = BusTrapHandler;
        let stop = loop {
            if !self.debug.single_step
                && !core.state.stopped
                && self.debug.breakpoints.contains(&(core.state.pc & 0x00FF_FFFF))
            {
                break Some(DebugStop::Breakpoint);
            }

            core.state.set_ipl(self.bus.highest_pending_ipl());
            let out = core.step(&mut self.bus, &mut handler);
            let frame_done = self.account_debug_step(budget, out.cycles, out.stopped);

            if std::mem::take(&mut self.debug.single_step) {
                break Some(DebugStop::Step);
            }
            if frame_done {
                break None;
            }
        };
        core.state.set_ipl(self.bus.highest_pending_ipl());
        self.cpu_state = core.into_state();
        stop
    }

    /// Advances peripherals for one executed step and returns `true` when the frame has ended.
    fn account_debug_step(&mut self, budget: FrameBudget, cycles: u32, stopped: bool) -> bool {
        let running_burst = self.is_running_burst();
        let frame = &mut self.debug.frame;
        frame.instructions += 1;

        let done = match budget {
            FrameBudget::Paced {
                max_instructions,
                target_ticks_10mhz,
            } => {
                let remaining = target_ticks_10mhz.saturating_sub(frame.elapsed_ticks);
                let ticks = if stopped {
                    remaining.min(64)
                } else {
                    frame.tick_accum += target_ticks_10mhz as usize;
                    let step = frame.tick_accum / max_instructions.max(1);
                    frame.tick_accum %= max_instructions.max(1);
                    (step as u32).min(remaining)
                };
                frame.elapsed_ticks += ticks;
                let stop_budget_spent = stopped && remaining == 0;
                let done = stop_budget_spent || frame.instructions >= max_instructions;
                let flush = if done { remaining - ticks } else { 0 };
                self.bus.advance_peripherals(ticks + flush);
                done
            }
            FrameBudget::Instructions(count) => {
                self.bus.advance_peripherals(if stopped { 64 } else { 8 });
                frame.instructions >= count
            }
            FrameBudget::Cycles(target) if running_burst => {
                self.bus.advance_peripherals(if stopped { 64 } else { 8 });
                frame.instructions >= target as usize
            }
            FrameBudget::Cycles(target) => {
                frame.elapsed_cycles = frame.elapsed_cycles.saturating_add(cycles);
                let done = frame.elapsed_cycles >= target;
                let cpu_mhz = self.bus.timing_config().cpu_mhz.max(10);
                self.bus
                    .advance_peripherals((cycles.saturating_mul(10) / cpu_mhz).max(1));
                done
            }
        };
        if done {
            self.debug.frame = FrameProgress::default();
        }
        done
    }
}

impl Target for X68kVm {
    type Arch = M68000;
    type Error = Infallible;

    fn base_ops(&mut self) -> BaseOps<'_, M68000, Infallible> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadBase for X68kVm {
    fn read_registers(&mut self, regs: &mut M68000Registers) -> TargetResult<(), Self> {
        regs.d = self.cpu_state.d;
        regs.a = self.cpu_state.a;
        regs.sr = self.cpu_state.sr as u32;
        regs.pc = self.cpu_state.pc;
        Ok(())
    }

    fn write_registers(&mut self, regs: &M68000Registers) -> TargetResult<(), Self> {
        // `set_sr` swaps A7 on a privilege change, so the new A7 must be stored after it.
        self.cpu_state.set_sr(regs.sr as u16);
        self.cpu_state.d = regs.d;
        self.cpu_state.a = regs.a;
        self.cpu_state.pc = regs.pc;
        Ok(())
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        for (i, byte) in data.iter_mut().enumerate() {
            match self.bus.peek_u8(start_addr.wrapping_add(i as u32)) {
                Some(value) => *byte = value,
                None if i == 0 => return Err(TargetError::NonFatal),
                None => return Ok(i),
            }
        }
        Ok(data.len())
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        for (i, &byte) in data.iter().enumerate() {
            if !self.bus.poke_u8(start_addr.wrapping_add(i as u32), byte) {
                return Err(TargetError::NonFatal);
            }
        }
        Ok(())
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadResume for X68kVm {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Infallible> {
        self.debug.single_step = false;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for X68kVm {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Infallible> {
        self.debug.single_step = true;
        Ok(())
    }
}

impl Breakpoints for X68kVm {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }
}

// Both breakpoint kinds live in the debug loop's table, so neither ever patches guest memory.
impl SwBreakpoint for X68kVm {
    fn add_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.debug.add_breakpoint(addr))
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.debug.remove_breakpoint(addr))
    }
}

impl HwBreakpoint for X68kVm {
    fn add_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.debug.add_breakpoint(addr))
    }

    fn remove_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        Ok(self.debug.remove_breakpoint(addr))
    }
}

type Session = GdbStubStateMachine<'static, X68kVm, TcpStream>;
type SessionError = GdbStubError<Infallible, io::Error>;

/// Serves one GDB client at a time from the host's main loop without blocking it.
///
/// The VM is halted whenever a connected client has not resumed it.
pub struct GdbServer {
    listener: TcpListener,
    session: Option<Session>,
}

impl GdbServer {
    /// Starts listening on the loopback interface at `port`.
    pub fn bind(port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            session: None,
        })
    }

    /// Returns `true` while a client is attached and in control of guest execution.
    pub fn is_connected(&self) -> bool {
        self.session.is_some()
    }

    /// Accepts a new client and handles any packets that have arrived.
    ///
    /// A client waits for each reply before sending its next packet, so once traffic starts this
    /// keeps servicing it briefly instead of adding an event loop wakeup to every round trip.
    pub fn poll(&mut self, vm: &mut X68kVm, budget: FrameBudget) {
        if self.session.is_none() {
            self.accept(vm);
        }
        let give_up = Instant::now() + EXCHANGE_LIMIT;
        let mut quiet_deadline = None;
        while self.is_connected() {
            let mut active = false;
            self.drive(vm, |session, vm| Self::pump(session, vm, &mut active));
            // Stepping runs here rather than on the next frame so a stepping client is not capped
            // at one instruction per frame.
            if vm.debug.single_step {
                self.run_frame(vm, budget);
                active = true;
            }

            let now = Instant::now();
            if active {
                quiet_deadline = Some(now + EXCHANGE_QUIET);
            }
            if quiet_deadline.is_none_or(|deadline| now >= deadline) || now >= give_up {
                break;
            }
        }
    }

    /// Runs one frame of guest code if the client has resumed the VM.
    pub fn run_frame(&mut self, vm: &mut X68kVm, budget: FrameBudget) {
        self.drive(vm, |session, vm| match session {
            GdbStubStateMachine::Running(running) => match vm.run_debug_frame(budget) {
                Some(stop) => {
                    let reason = match stop {
                        DebugStop::Breakpoint => SingleThreadStopReason::SwBreak(()),
                        DebugStop::Step => SingleThreadStopReason::DoneStep,
                    };
                    Self::settle(running.report_stop(vm, reason)?, vm)
                }
                None => Ok(Some(running.into())),
            },
            idle => Ok(Some(idle)),
        });
    }

    fn accept(&mut self, vm: &mut X68kVm) {
        let stream = match self.listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock => return,
            Err(e) => {
                eprintln!("GDB: failed to accept a connection: {e}");
                return;
            }
        };
        if let Err(e) = stream.set_nonblocking(false).and(stream.set_nodelay(true)) {
            eprintln!("GDB: failed to configure the connection: {e}");
            return;
        }
        match GdbStub::new(stream).run_state_machine(vm) {
            Ok(session) => {
                println!("GDB: client connected, VM halted");
                self.session = Some(session);
            }
            Err(e) => eprintln!("GDB: failed to start the session: {e}"),
        }
    }

    /// Applies `action` to the session, ending it and releasing the VM once it closes.
    fn drive(
        &mut self,
        vm: &mut X68kVm,
        action: impl FnOnce(Session, &mut X68kVm) -> Result<Option<Session>, SessionError>,
    ) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.session = action(session, vm).unwrap_or_else(|e| {
            eprintln!("GDB: session error: {e}");
            None
        });
        if self.session.is_none() {
            println!("GDB: client disconnected, VM running");
            vm.debug = DebugState::default();
        }
    }

    fn pump(
        mut session: Session,
        vm: &mut X68kVm,
        active: &mut bool,
    ) -> Result<Option<Session>, SessionError> {
        let conn = match &mut session {
            GdbStubStateMachine::Idle(inner) => inner.borrow_conn(),
            GdbStubStateMachine::Running(inner) => inner.borrow_conn(),
            GdbStubStateMachine::CtrlCInterrupt(inner) => inner.borrow_conn(),
            GdbStubStateMachine::Disconnected(inner) => inner.borrow_conn(),
        };
        let mut buf = [0u8; 4096];
        let received = match read_available(conn, &mut buf) {
            Ok(Some(0)) | Err(_) => return Ok(None),
            Ok(Some(n)) => n,
            Ok(None) => return Ok(Some(session)),
        };
        *active = true;

        for &byte in &buf[..received] {
            session = match session {
                GdbStubStateMachine::Idle(inner) => inner.incoming_data(vm, byte)?,
                GdbStubStateMachine::Running(inner) => inner.incoming_data(vm, byte)?,
                settled => settled,
            };
            session = match Self::settle(session, vm)? {
                Some(session) => session,
                None => return Ok(None),
            };
        }
        Ok(Some(session))
    }

    /// Resolves transient states so a stored session is always idle or running.
    fn settle(session: Session, vm: &mut X68kVm) -> Result<Option<Session>, SessionError> {
        match session {
            GdbStubStateMachine::CtrlCInterrupt(inner) => {
                let reason = SingleThreadStopReason::<u32>::Signal(Signal::SIGINT);
                Self::settle(inner.interrupt_handled(vm, Some(reason))?, vm)
            }
            GdbStubStateMachine::Disconnected(_) => Ok(None),
            session => Ok(Some(session)),
        }
    }
}

/// Reads whatever has arrived on `conn`, returning `None` when nothing is pending.
///
/// The socket stays blocking otherwise so that replies are always written in full.
fn read_available(conn: &mut TcpStream, buf: &mut [u8]) -> io::Result<Option<usize>> {
    conn.set_nonblocking(true)?;
    let result = conn.read(buf);
    conn.set_nonblocking(false)?;
    match result {
        Ok(n) => Ok(Some(n)),
        Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timing::CpuSpeedMode;
    use crate::vm::VmConfig;
    use std::io::Write;

    const PROGRAM_PC: u32 = 0x1000;
    const LOOP_BUDGET: FrameBudget = FrameBudget::Paced {
        max_instructions: 1000,
        target_ticks_10mhz: 166_667,
    };

    /// A VM spinning in `ADDQ.L #1,D0` / `BRA.S` at [`PROGRAM_PC`].
    fn loop_vm(speed_mode: CpuSpeedMode) -> X68kVm {
        let mut vm = X68kVm::new(VmConfig::app_vm().with_speed_mode(speed_mode));
        vm.bus.main_ram_mut()[PROGRAM_PC as usize..PROGRAM_PC as usize + 4]
            .copy_from_slice(&[0x52, 0x80, 0x60, 0xFC]);
        vm.cpu_state.pc = PROGRAM_PC;
        vm.cpu_state.a[7] = 0x8000;
        vm
    }

    fn assert_same_machine(a: &X68kVm, b: &X68kVm) {
        assert_eq!(a.cpu_state, b.cpu_state);
        assert_eq!(format!("{:?}", a.bus.crtc), format!("{:?}", b.bus.crtc));
        assert_eq!(format!("{:?}", a.bus.mfp), format!("{:?}", b.bus.mfp));
    }

    #[test]
    fn debug_frame_matches_regular_execution_for_every_budget() {
        for (mode, budget) in [
            (CpuSpeedMode::Burst, LOOP_BUDGET),
            (CpuSpeedMode::Burst, FrameBudget::Instructions(1000)),
            (CpuSpeedMode::Mhz10, FrameBudget::Cycles(10_000)),
        ] {
            let mut regular = loop_vm(mode);
            regular.run_frame(budget);

            let mut debug = loop_vm(mode);
            assert_eq!(debug.run_debug_frame(budget), None);

            assert_same_machine(&regular, &debug);
            assert_eq!(debug.debug.frame, FrameProgress::default());
        }
    }

    #[test]
    fn stopping_mid_frame_does_not_disturb_frame_pacing() {
        let mut regular = loop_vm(CpuSpeedMode::Burst);
        regular.run_frame(LOOP_BUDGET);

        let mut debug = loop_vm(CpuSpeedMode::Burst);
        debug.debug.single_step = true;
        assert_eq!(debug.run_debug_frame(LOOP_BUDGET), Some(DebugStop::Step));
        assert_eq!(debug.debug.frame.instructions, 1);
        assert_eq!(debug.run_debug_frame(LOOP_BUDGET), None);

        assert_same_machine(&regular, &debug);
    }

    #[test]
    fn breakpoint_stops_before_the_instruction_runs() {
        let mut vm = loop_vm(CpuSpeedMode::Burst);
        vm.debug.add_breakpoint(0xAB00_0000 | (PROGRAM_PC + 2));
        assert_eq!(vm.run_debug_frame(LOOP_BUDGET), Some(DebugStop::Breakpoint));
        assert_eq!(vm.cpu_state.pc, PROGRAM_PC + 2);
        assert_eq!(vm.cpu_state.d[0], 1);
        assert_eq!(&vm.bus.main_ram()[PROGRAM_PC as usize + 2..PROGRAM_PC as usize + 4], &[0x60, 0xFC]);
    }

    #[test]
    fn peek_has_no_side_effects_and_skips_io() {
        let mut vm = loop_vm(CpuSpeedMode::Burst);
        assert_eq!(vm.bus.peek_u8(PROGRAM_PC), Some(0x52));
        assert_eq!(vm.bus.peek_u8(0x00C0_0000), Some(0));
        assert!(!vm.bus.access_flags().gvram_accessed);
        assert_eq!(vm.bus.peek_u8(0x00E8_802F), None);
        assert!(vm.bus.poke_u8(PROGRAM_PC, 0x4E));
        assert_eq!(vm.bus.peek_u8(PROGRAM_PC), Some(0x4E));
        assert!(!vm.bus.poke_u8(0x00E8_802F, 0));
    }

    fn checksum_packet(body: &str) -> Vec<u8> {
        let sum = body.bytes().fold(0u8, u8::wrapping_add);
        format!("${body}#{sum:02x}").into_bytes()
    }

    /// Expands GDB's `<char>*<count>` run-length encoding.
    fn expand_rle(body: &[u8]) -> String {
        let mut out = Vec::new();
        let mut bytes = body.iter().copied();
        while let Some(b) = bytes.next() {
            if b == b'*' {
                let last = *out.last().unwrap();
                let count = bytes.next().unwrap() - 29;
                out.extend(std::iter::repeat_n(last, count as usize));
            } else {
                out.push(b);
            }
        }
        String::from_utf8(out).unwrap()
    }

    struct Client {
        stream: TcpStream,
        server: GdbServer,
        vm: X68kVm,
    }

    impl Client {
        fn pump_until_reply(&mut self) -> String {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut reply = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                assert!(Instant::now() < deadline, "no reply: {reply:?}");
                self.server.poll(&mut self.vm, LOOP_BUDGET);
                self.server.run_frame(&mut self.vm, LOOP_BUDGET);
                while let Ok(1) = self.stream.read(&mut byte) {
                    if byte[0] != b'+' {
                        reply.push(byte[0]);
                    }
                    if reply.len() >= 4 && reply[reply.len() - 3] == b'#' {
                        self.stream.write_all(b"+").unwrap();
                        let end = reply.len() - 3;
                        return expand_rle(&reply[1..end]);
                    }
                }
            }
        }

        fn request(&mut self, body: &str) -> String {
            self.stream.write_all(&checksum_packet(body)).unwrap();
            self.pump_until_reply()
        }
    }

    fn connect() -> Client {
        let mut server = GdbServer::bind(0).unwrap();
        let addr = server.listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        let mut vm = loop_vm(CpuSpeedMode::Burst);
        server.poll(&mut vm, LOOP_BUDGET);
        assert!(server.is_connected());
        Client { stream, server, vm }
    }

    #[test]
    fn remote_session_reads_state_and_hits_breakpoints() {
        let mut client = connect();

        let regs = client.request("g");
        assert_eq!(regs.len(), REGISTER_BYTES * 2);
        assert!(regs.ends_with("00001000"), "{regs}");
        assert_eq!(client.request("m1000,4"), "528060fc");

        assert_eq!(client.request("Z0,1002,2"), "OK");
        client.request("c");
        assert_eq!(client.vm.cpu_state.pc, PROGRAM_PC + 2);
        assert_eq!(client.request("z0,1002,2"), "OK");

        client.request("s");
        assert_eq!(client.vm.cpu_state.pc, PROGRAM_PC);

        assert_eq!(client.request("D"), "OK");
        client.server.poll(&mut client.vm, LOOP_BUDGET);
        assert!(!client.server.is_connected());
    }
}
