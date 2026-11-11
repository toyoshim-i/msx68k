//! MC68000 register file, status register flags, and architectural state.

/// Carry flag (bit 0 of CCR/SR).
pub const SR_C: u16 = 0x0001;
/// Overflow flag (bit 1 of CCR/SR).
pub const SR_V: u16 = 0x0002;
/// Zero flag (bit 2 of CCR/SR).
pub const SR_Z: u16 = 0x0004;
/// Negative flag (bit 3 of CCR/SR).
pub const SR_N: u16 = 0x0008;
/// Extend flag (bit 4 of CCR/SR).
pub const SR_X: u16 = 0x0010;
/// Mask of all defined CCR bits (bits 0..=4).
pub const SR_CCR_MASK: u16 = 0x001F;
/// Interrupt Priority Mask (bits 8..=10 of SR).
pub const SR_IPM_MASK: u16 = 0x0700;
/// Supervisor state flag (bit 13 of SR).
pub const SR_S: u16 = 0x2000;
/// Master/interrupt state flag (bit 12 of SR, MC68030 only).
pub const SR_M: u16 = 0x1000;
/// Trace on change of flow flag (bit 14 of SR, MC68030 only).
pub const SR_T0: u16 = 0x4000;
/// Trace mode flag (bit 15 of SR, `T1` on the MC68030).
pub const SR_T: u16 = 0x8000;
/// Mask of all valid MC68000 Status Register bits (`T`, `S`, `I2..I0`, `X`, `N`, `Z`, `V`, `C`).
pub const SR_VALID_MASK: u16 = 0xA71F;
/// Mask of all valid MC68030 Status Register bits (adds `T0` and `M`).
pub const SR_VALID_MASK_030: u16 = 0xF71F;

/// CPU model emulated by a [`CpuState`].
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub enum CpuModel {
    /// MC68000: 24-bit address bus, address errors on odd word accesses, 3-word exception frames.
    #[default]
    Mc68000,
    /// MC68030: 32-bit address bus, 68020 instructions, `VBR`, and stack frames with a format word.
    Mc68030,
}

/// Standard MC68000 exception vector numbers.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ExceptionVector {
    ResetInitialSsp = 0,
    ResetInitialPc = 1,
    BusError = 2,
    AddressError = 3,
    IllegalInstruction = 4,
    ZeroDivide = 5,
    ChkInstruction = 6,
    TrapvInstruction = 7,
    PrivilegeViolation = 8,
    Trace = 9,
    Line1010Emulator = 10,
    Line1111Emulator = 11,
    FormatError = 14,
    SpuriousInterrupt = 24,
}

/// Operand size for MC68000 instructions.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DataSize {
    Byte,
    Word,
    Long,
}

impl DataSize {
    /// Size in bytes (1, 2, or 4).
    #[inline]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Long => 4,
        }
    }

    /// Sign bit mask (`0x80`, `0x8000`, or `0x8000_0000`).
    #[inline]
    pub const fn sign_bit(self) -> u32 {
        match self {
            Self::Byte => 0x80,
            Self::Word => 0x8000,
            Self::Long => 0x8000_0000,
        }
    }

    /// Value mask (`0xFF`, `0xFFFF`, or `0xFFFF_FFFF`).
    #[inline]
    pub const fn mask(self) -> u32 {
        match self {
            Self::Byte => 0xFF,
            Self::Word => 0xFFFF,
            Self::Long => 0xFFFF_FFFF,
        }
    }

    /// Truncates `val` to this operand size.
    #[inline]
    pub const fn mask_val(self, val: u32) -> u32 {
        val & self.mask()
    }

    /// Sign-extends `val` from this operand size to `i32`.
    #[inline]
    pub const fn sign_extend(self, val: u32) -> i32 {
        match self {
            Self::Byte => (val as u8 as i8) as i32,
            Self::Word => (val as u16 as i16) as i32,
            Self::Long => val as i32,
        }
    }
}

/// MC68030 MMU registers. Address translation is not emulated, so they only hold values.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct MmuRegisters {
    /// Translation Control register (`TC`).
    pub tc: u32,
    /// Transparent Translation register 0 (`TT0`).
    pub tt0: u32,
    /// Transparent Translation register 1 (`TT1`).
    pub tt1: u32,
    /// CPU Root Pointer (`CRP`).
    pub crp: u64,
    /// Supervisor Root Pointer (`SRP`).
    pub srp: u64,
    /// MMU Status Register (`MMUSR`).
    pub mmusr: u16,
}

impl MmuRegisters {
    /// Creates the registers in their reset state.
    pub const fn new() -> Self {
        Self {
            tc: 0,
            tt0: 0,
            tt1: 0,
            crp: 0,
            srp: 0,
            mmusr: 0,
        }
    }
}

/// The stack pointer that `A7` currently stands for.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum StackPointer {
    User,
    Supervisor,
    Master,
}

/// Complete architectural state of an MC68000 or MC68030 CPU instance.
///
/// Decoupled from [`crate::policy::CpuPolicy`] so a VM can switch between
/// cycle-accurate (`10MHz` / `16MHz` / `24MHz`) and `Burst` execution policies
/// at any instruction boundary without copying state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuState {
    /// Data registers `D0`–`D7`.
    pub d: [u32; 8],
    /// Address registers `A0`–`A7` (`A7` is the currently active stack pointer).
    pub a: [u32; 8],
    /// Program Counter.
    pub pc: u32,
    /// Address of the currently executing instruction (for exception frames and diagnostics).
    pub ir_pc: u32,
    /// Current instruction opcode register (`IR`).
    pub ir: u16,
    /// Status Register (`SR`).
    pub sr: u16,
    /// Inactive User Stack Pointer (updated when switching out of user mode).
    pub usp: u32,
    /// Inactive Supervisor Stack Pointer (updated when switching out of supervisor mode).
    /// On the MC68030 this is the Interrupt Stack Pointer (`ISP`).
    pub ssp: u32,
    /// Inactive Master Stack Pointer (MC68030, active while `S` and `M` are both set).
    pub msp: u32,
    /// Emulated CPU model.
    pub model: CpuModel,
    /// Vector Base Register (MC68030).
    pub vbr: u32,
    /// Cache Control Register (MC68030). The caches are not emulated.
    pub cacr: u32,
    /// Cache Address Register (MC68030).
    pub caar: u32,
    /// Source Function Code register used by `MOVES` (MC68030).
    pub sfc: u8,
    /// Destination Function Code register used by `MOVES` (MC68030).
    pub dfc: u8,
    /// MC68030 MMU registers, written and read by `PMOVE`.
    pub mmu: MmuRegisters,
    /// `true` when the CPU is halted in `STOP` state waiting for an interrupt/reset.
    pub stopped: bool,
    /// Current external interrupt priority level (`0`..=`7`).
    pub pending_ipl: u8,
    /// Previous IPL state for level-7 NMI edge detection.
    pub nmi_pending: bool,
    /// Whether to enforce odd-address checks (Address Error) on 16/32-bit accesses.
    pub check_address_error: bool,
    /// Whether the `T` (Trace) bit in `SR` raises a Vector 9 exception after instruction execution.
    pub trace_enabled: bool,
    /// Cumulative CPU clock cycles elapsed (only incremented in `TrackedPolicy`).
    pub total_cycles: u64,
}

impl Default for CpuState {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuState {
    /// Creates a new MC68000 state in Supervisor mode with interrupts masked (`$2700`).
    pub const fn new() -> Self {
        Self::with_model(CpuModel::Mc68000)
    }

    /// Creates a new state for `model` in Supervisor mode with interrupts masked (`$2700`).
    pub const fn with_model(model: CpuModel) -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            pc: 0,
            ir_pc: 0,
            ir: 0,
            sr: SR_S | SR_IPM_MASK,
            usp: 0,
            ssp: 0,
            msp: 0,
            model,
            vbr: 0,
            cacr: 0,
            caar: 0,
            sfc: 0,
            dfc: 0,
            mmu: MmuRegisters::new(),
            stopped: false,
            pending_ipl: 0,
            nmi_pending: false,
            check_address_error: true,
            trace_enabled: true,
            total_cycles: 0,
        }
    }

    /// Returns `true` if the CPU is currently in Supervisor mode (`S` bit set).
    #[inline]
    pub const fn is_supervisor(&self) -> bool {
        (self.sr & SR_S) != 0
    }

    /// Returns the current Interrupt Priority Mask (`0`..=`7`).
    #[inline]
    pub const fn interrupt_mask(&self) -> u8 {
        ((self.sr & SR_IPM_MASK) >> 8) as u8
    }

    /// Returns the Condition Code Register (`CCR`, low 8 bits of `SR`).
    #[inline]
    pub const fn ccr(&self) -> u8 {
        (self.sr & SR_CCR_MASK) as u8
    }

    /// Updates only the Condition Code Register (`CCR`) bits.
    #[inline]
    pub fn set_ccr(&mut self, ccr: u8) {
        self.sr = (self.sr & !SR_CCR_MASK) | ((ccr as u16) & SR_CCR_MASK);
    }

    /// Returns which stack pointer `A7` is for `sr`.
    #[inline]
    const fn stack_for(&self, sr: u16) -> StackPointer {
        if (sr & SR_S) == 0 {
            StackPointer::User
        } else if (sr & SR_M) != 0 && matches!(self.model, CpuModel::Mc68030) {
            StackPointer::Master
        } else {
            StackPointer::Supervisor
        }
    }

    /// Updates the full Status Register (`SR`), swapping the active `A7` between `USP`, `SSP`
    /// (`ISP`), and `MSP` if the `S` or `M` bit changes.
    #[inline]
    pub fn set_sr(&mut self, new_sr: u16) {
        let valid = match self.model {
            CpuModel::Mc68000 => SR_VALID_MASK,
            CpuModel::Mc68030 => SR_VALID_MASK_030,
        };
        let new_sr = new_sr & valid;
        let old = self.stack_for(self.sr);
        let new = self.stack_for(new_sr);
        if old != new {
            let a7 = self.a[7];
            *self.inactive_sp(old) = a7;
            self.a[7] = *self.inactive_sp(new);
        }
        self.sr = new_sr;
    }

    #[inline]
    fn inactive_sp(&mut self, sp: StackPointer) -> &mut u32 {
        match sp {
            StackPointer::User => &mut self.usp,
            StackPointer::Supervisor => &mut self.ssp,
            StackPointer::Master => &mut self.msp,
        }
    }

    #[inline]
    fn get_sp(&self, sp: StackPointer) -> u32 {
        if self.stack_for(self.sr) == sp {
            self.a[7]
        } else {
            match sp {
                StackPointer::User => self.usp,
                StackPointer::Supervisor => self.ssp,
                StackPointer::Master => self.msp,
            }
        }
    }

    #[inline]
    fn set_sp(&mut self, sp: StackPointer, val: u32) {
        *self.inactive_sp(sp) = val;
        if self.stack_for(self.sr) == sp {
            self.a[7] = val;
        }
    }

    /// Reads the User Stack Pointer regardless of current privilege mode.
    #[inline]
    pub fn get_usp(&self) -> u32 {
        self.get_sp(StackPointer::User)
    }

    /// Sets the User Stack Pointer regardless of current privilege mode.
    #[inline]
    pub fn set_usp(&mut self, val: u32) {
        self.set_sp(StackPointer::User, val);
    }

    /// Reads the Supervisor Stack Pointer (`ISP` on the MC68030) regardless of current mode.
    #[inline]
    pub fn get_ssp(&self) -> u32 {
        self.get_sp(StackPointer::Supervisor)
    }

    /// Sets the Supervisor Stack Pointer (`ISP` on the MC68030) regardless of current mode.
    #[inline]
    pub fn set_ssp(&mut self, val: u32) {
        self.set_sp(StackPointer::Supervisor, val);
    }

    /// Reads the MC68030 Master Stack Pointer regardless of current mode.
    #[inline]
    pub fn get_msp(&self) -> u32 {
        self.get_sp(StackPointer::Master)
    }

    /// Sets the MC68030 Master Stack Pointer regardless of current mode.
    #[inline]
    pub fn set_msp(&mut self, val: u32) {
        self.set_sp(StackPointer::Master, val);
    }

    /// Sets the external interrupt priority level (`0`..=`7`).
    /// Level 7 (NMI) is edge-triggered on transitions to 7.
    #[inline]
    pub fn set_ipl(&mut self, level: u8) {
        let level = level.min(7);
        if level == 7 && self.pending_ipl < 7 {
            self.nmi_pending = true;
        }
        self.pending_ipl = level;
    }

    /// Writes `val` of `size` into `D[reg]`, preserving upper bits for `Byte` and `Word`.
    #[inline]
    pub fn write_d(&mut self, reg: usize, size: DataSize, val: u32) {
        let mask = size.mask();
        self.d[reg] = (self.d[reg] & !mask) | (val & mask);
    }

    /// Reads `D[reg]` masked to `size`.
    #[inline]
    pub const fn read_d(&self, reg: usize, size: DataSize) -> u32 {
        self.d[reg] & size.mask()
    }

    /// Reads flag state helper.
    #[inline]
    pub const fn flag(&self, mask: u16) -> bool {
        (self.sr & mask) != 0
    }

    /// Sets or clears a specific flag bit in `SR`.
    #[inline]
    pub fn set_flag(&mut self, mask: u16, set: bool) {
        if set {
            self.sr |= mask;
        } else {
            self.sr &= !mask;
        }
    }
}
