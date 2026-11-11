//! MC68030 instruction timing, after Section 11 of the MC68030 User's Manual.
//!
//! Every instruction is split into an effective address part and an operation part, each with
//! the head, tail, instruction-cache case (CC), and no-cache case (NCC) times of the tables in
//! Section 11.6. Consecutive parts overlap by `min(head, previous tail)` (equation 11-2).
//! An instruction whose prefetches all hit the instruction cache uses the CC times with
//! overlap; one that misses uses the NCC times without overlap. Data cache hits and wait
//! states are folded in by the rules of Sections 11.4 and 11.5.

use crate::state::DataSize;

/// `CACR` bits used by the caches.
pub mod cacr {
    pub const EI: u32 = 0x0001;
    pub const FI: u32 = 0x0002;
    pub const CEI: u32 = 0x0004;
    pub const CI: u32 = 0x0008;
    pub const ED: u32 = 0x0100;
    pub const FD: u32 = 0x0200;
    pub const CED: u32 = 0x0400;
    pub const CD: u32 = 0x0800;
    pub const WA: u32 = 0x2000;
}

/// Tags of one 256-byte MC68030 cache: 16 lines of four long words, tagged with the address and
/// the supervisor function code bit. Only tags are kept, so hits change timing, never data.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CacheTags {
    tags: [u32; 16],
    valid: [u8; 16],
}

impl Default for CacheTags {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheTags {
    pub const fn new() -> Self {
        Self {
            tags: [0; 16],
            valid: [0; 16],
        }
    }

    #[inline]
    fn slot(addr: u32, supervisor: bool) -> (usize, u32, u8) {
        let line = ((addr >> 4) & 15) as usize;
        let tag = (addr & !0xFF) | supervisor as u32;
        (line, tag, 1 << ((addr >> 2) & 3))
    }

    /// Returns `true` if the long word holding `addr` is cached.
    #[inline]
    pub fn hit(&self, addr: u32, supervisor: bool) -> bool {
        let (line, tag, bit) = Self::slot(addr, supervisor);
        self.tags[line] == tag && (self.valid[line] & bit) != 0
    }

    /// Caches the long word holding `addr`, replacing the line if its tag differs.
    #[inline]
    pub fn fill(&mut self, addr: u32, supervisor: bool) {
        let (line, tag, bit) = Self::slot(addr, supervisor);
        if self.tags[line] != tag {
            self.tags[line] = tag;
            self.valid[line] = 0;
        }
        self.valid[line] |= bit;
    }

    /// Invalidates every entry.
    pub fn clear(&mut self) {
        self.valid = [0; 16];
    }

    /// Invalidates the long word holding `addr` in either function code space.
    pub fn clear_entry(&mut self, addr: u32) {
        let line = ((addr >> 4) & 15) as usize;
        if (self.tags[line] & !1) == (addr & !0xFF) {
            self.valid[line] &= !(1 << ((addr >> 2) & 3));
        }
    }
}

/// Timing state of an MC68030 that persists between instructions.
#[derive(Debug, Copy, Clone, Default, PartialEq, Eq)]
pub struct Timing030 {
    pub icache: CacheTags,
    pub dcache: CacheTags,
    /// Long word address held by the cache holding register, plus one when empty.
    pub holding: u32,
    /// Tail of the previous instruction, which the next one may overlap.
    pub tail: u16,
    /// DRAM refresh delay not yet added, in eighths of a clock.
    pub refresh: u8,
}

impl Timing030 {
    pub const fn new() -> Self {
        Self {
            icache: CacheTags::new(),
            dcache: CacheTags::new(),
            holding: 1,
            tail: 0,
            refresh: 0,
        }
    }
}

/// Effective address mode, as the timing tables distinguish them.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Ea {
    Dn,
    An,
    Ind,
    PostInc,
    PreDec,
    /// `(d16,An)` or `(d16,PC)`.
    Disp16,
    AbsW,
    AbsL,
    Imm {
        long: bool,
    },
    /// `(d8,An,Xn)` or `(d8,PC,Xn)`.
    Brief,
    /// Full extension word format.
    Full(Full),
}

/// Full format extension word, keyed as the tables' `B` (base) rows.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Full {
    /// Base displacement size: 0 (null), 1 (word), or 2 (long).
    pub base_disp: u8,
    pub indirect: bool,
    /// Outer displacement size: 0 (null), 1 (word), or 2 (long).
    pub outer_disp: u8,
}

impl Full {
    fn row(self) -> usize {
        if self.indirect {
            3 + self.base_disp as usize * 3 + self.outer_disp as usize
        } else {
            self.base_disp as usize
        }
    }
}

/// One table entry.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Part {
    pub head: u16,
    /// The head extends through the head of the operation ("X+op head").
    pub plus_op_head: bool,
    pub tail: u16,
    pub cc: u16,
    pub ncc: u16,
    pub reads: u16,
    pub fetches: u16,
    pub writes: u16,
}

const fn p(head: u16, tail: u16, cc: u16, ncc: u16, reads: u16, fetches: u16, writes: u16) -> Part {
    Part {
        head,
        plus_op_head: false,
        tail,
        cc,
        ncc,
        reads,
        fetches,
        writes,
    }
}

const fn ph(
    head: u16,
    tail: u16,
    cc: u16,
    ncc: u16,
    reads: u16,
    fetches: u16,
    writes: u16,
) -> Part {
    Part {
        plus_op_head: true,
        ..p(head, tail, cc, ncc, reads, fetches, writes)
    }
}

/// Section 11.6.1, full format rows in the order of [`Full::row`].
const FEA_FULL: [Part; 12] = [
    p(4, 0, 6, 7, 1, 1, 0),
    p(4, 0, 8, 10, 1, 1, 0),
    p(4, 0, 12, 13, 1, 2, 0),
    p(4, 0, 10, 10, 2, 1, 0),
    p(4, 0, 12, 13, 2, 1, 0),
    p(4, 0, 12, 14, 2, 2, 0),
    p(4, 0, 12, 13, 2, 1, 0),
    p(4, 0, 14, 16, 2, 2, 0),
    p(4, 0, 14, 17, 2, 2, 0),
    p(4, 0, 16, 17, 2, 2, 0),
    p(4, 0, 18, 20, 2, 2, 0),
    p(4, 0, 18, 21, 2, 3, 0),
];

/// Fetch effective address (Section 11.6.1). Register modes take no time.
pub fn fea(ea: Ea) -> Option<Part> {
    Some(match ea {
        Ea::Dn | Ea::An => return None,
        Ea::Ind => p(1, 1, 3, 3, 1, 0, 0),
        Ea::PostInc => p(0, 1, 3, 3, 1, 0, 0),
        Ea::PreDec => p(2, 2, 4, 4, 1, 0, 0),
        Ea::Disp16 | Ea::AbsW => p(2, 2, 4, 4, 1, 1, 0),
        Ea::AbsL => p(1, 0, 4, 5, 1, 1, 0),
        Ea::Imm { long: false } => p(2, 0, 2, 2, 0, 1, 0),
        Ea::Imm { long: true } => p(4, 0, 4, 4, 0, 1, 0),
        Ea::Brief => p(4, 2, 6, 6, 1, 1, 0),
        Ea::Full(full) => FEA_FULL[full.row()],
    })
}

const FIEA_FULL_W: [Part; 12] = [
    p(6, 0, 8, 9, 1, 1, 0),
    p(6, 0, 10, 12, 1, 2, 0),
    p(10, 0, 14, 16, 1, 2, 0),
    p(6, 0, 12, 12, 2, 1, 0),
    p(6, 0, 14, 15, 2, 2, 0),
    p(6, 0, 14, 16, 2, 2, 0),
    p(6, 0, 14, 15, 2, 2, 0),
    p(6, 0, 16, 18, 2, 2, 0),
    p(6, 0, 16, 19, 2, 3, 0),
    p(6, 0, 18, 19, 2, 2, 0),
    p(6, 0, 20, 22, 2, 3, 0),
    p(6, 0, 20, 23, 2, 3, 0),
];

const FIEA_FULL_L: [Part; 12] = [
    p(8, 0, 10, 11, 1, 2, 0),
    p(8, 0, 12, 14, 1, 2, 0),
    p(12, 0, 16, 18, 1, 3, 0),
    p(8, 0, 14, 14, 2, 2, 0),
    p(8, 0, 16, 17, 2, 2, 0),
    p(8, 0, 16, 18, 2, 3, 0),
    p(8, 0, 16, 17, 2, 2, 0),
    p(8, 0, 18, 20, 2, 3, 0),
    p(8, 0, 18, 21, 2, 3, 0),
    p(8, 0, 20, 21, 2, 3, 0),
    p(8, 0, 22, 24, 2, 3, 0),
    p(8, 0, 22, 25, 2, 4, 0),
];

/// Fetch immediate effective address (Section 11.6.2): an immediate operand of `long` size,
/// or the second word of a two-word instruction, plus the operand at `ea`.
pub fn fiea(ea: Ea, long: bool) -> Part {
    match (ea, long) {
        (Ea::Dn | Ea::An, false) => ph(2, 0, 2, 2, 0, 1, 0),
        (Ea::Dn | Ea::An, true) => ph(4, 0, 4, 4, 0, 1, 0),
        (Ea::Ind, false) => p(1, 1, 3, 4, 1, 1, 0),
        (Ea::Ind, true) => p(1, 0, 4, 5, 1, 1, 0),
        (Ea::PostInc, false) => p(2, 1, 5, 5, 1, 1, 0),
        (Ea::PostInc, true) => p(4, 1, 7, 7, 1, 1, 0),
        (Ea::PreDec, false) => p(2, 2, 4, 4, 1, 1, 0),
        (Ea::PreDec, true) => p(2, 0, 4, 5, 1, 1, 0),
        (Ea::Disp16, false) => p(2, 0, 4, 5, 1, 1, 0),
        (Ea::Disp16, true) => p(4, 0, 6, 8, 1, 2, 0),
        (Ea::AbsW, false) => p(4, 2, 6, 6, 1, 1, 0),
        (Ea::AbsW, true) => p(6, 2, 8, 8, 1, 2, 0),
        (Ea::AbsL, false) => p(3, 0, 6, 7, 1, 2, 0),
        (Ea::AbsL, true) => p(5, 0, 8, 9, 1, 2, 0),
        // Only `#<data>.W,#<data>.L` is listed (an immediate source of a two-word instruction).
        (Ea::Imm { .. }, _) => ph(6, 0, 6, 6, 0, 2, 0),
        (Ea::Brief, false) => p(6, 2, 8, 8, 1, 2, 0),
        (Ea::Brief, true) => p(8, 2, 10, 10, 1, 2, 0),
        (Ea::Full(full), false) => FIEA_FULL_W[full.row()],
        (Ea::Full(full), true) => FIEA_FULL_L[full.row()],
    }
}

const CEA_FULL: [Part; 12] = [
    ph(6, 0, 6, 6, 0, 1, 0),
    p(4, 0, 8, 9, 0, 1, 0),
    p(4, 0, 12, 12, 0, 2, 0),
    p(4, 0, 10, 10, 1, 1, 0),
    p(4, 0, 12, 13, 1, 1, 0),
    p(4, 0, 12, 13, 1, 2, 0),
    p(4, 0, 12, 13, 1, 1, 0),
    p(4, 0, 14, 16, 1, 2, 0),
    p(4, 0, 14, 16, 1, 2, 0),
    p(4, 0, 16, 17, 1, 2, 0),
    p(4, 0, 18, 20, 1, 2, 0),
    p(4, 0, 18, 20, 1, 3, 0),
];

/// Calculate effective address (Section 11.6.3), without fetching the operand.
pub fn cea(ea: Ea) -> Option<Part> {
    Some(match ea {
        Ea::Dn | Ea::An => return None,
        Ea::Ind | Ea::PreDec => ph(2, 0, 2, 2, 0, 0, 0),
        Ea::PostInc => p(0, 0, 2, 2, 0, 0, 0),
        Ea::Disp16 | Ea::AbsW => ph(2, 0, 2, 2, 0, 1, 0),
        Ea::AbsL | Ea::Brief => ph(4, 0, 4, 4, 0, 1, 0),
        Ea::Imm { .. } => return None,
        Ea::Full(full) => CEA_FULL[full.row()],
    })
}

const CIEA_FULL_W: [Part; 12] = [
    ph(8, 0, 8, 8, 0, 1, 0),
    p(6, 0, 10, 11, 0, 2, 0),
    p(6, 0, 14, 15, 0, 2, 0),
    p(6, 0, 12, 12, 1, 1, 0),
    p(6, 0, 14, 15, 1, 2, 0),
    p(6, 0, 14, 15, 1, 2, 0),
    p(6, 0, 14, 15, 1, 2, 0),
    p(6, 0, 16, 18, 1, 2, 0),
    p(6, 0, 16, 18, 1, 3, 0),
    p(6, 0, 18, 19, 1, 2, 0),
    p(6, 0, 20, 22, 1, 3, 0),
    p(6, 0, 20, 22, 1, 3, 0),
];

const CIEA_FULL_L: [Part; 12] = [
    ph(10, 0, 10, 10, 0, 2, 0),
    p(8, 0, 12, 13, 0, 2, 0),
    p(8, 0, 16, 17, 0, 3, 0),
    p(8, 0, 14, 14, 1, 2, 0),
    p(8, 0, 16, 17, 1, 2, 0),
    p(8, 0, 16, 17, 1, 3, 0),
    p(8, 0, 16, 17, 1, 2, 0),
    p(8, 0, 18, 20, 1, 3, 0),
    p(8, 0, 18, 20, 1, 3, 0),
    p(8, 0, 20, 21, 1, 3, 0),
    p(8, 0, 22, 24, 1, 3, 0),
    p(8, 0, 22, 24, 1, 4, 0),
];

/// Calculate immediate effective address (Section 11.6.4): the second word of a two-word
/// instruction plus the address of `ea`.
pub fn ciea(ea: Ea, long: bool) -> Part {
    match (ea, long) {
        (Ea::Dn | Ea::An | Ea::Ind | Ea::PreDec | Ea::Imm { .. }, false) => ph(2, 0, 2, 2, 0, 1, 0),
        (Ea::Dn | Ea::An | Ea::Ind | Ea::PreDec | Ea::Imm { .. }, true) => ph(4, 0, 4, 4, 0, 1, 0),
        (Ea::PostInc, false) => p(2, 0, 4, 4, 0, 1, 0),
        (Ea::PostInc, true) => p(4, 0, 6, 6, 0, 1, 0),
        (Ea::Disp16, false) | (Ea::AbsW, false) => ph(4, 0, 4, 4, 0, 1, 0),
        (Ea::Disp16, true) => ph(6, 0, 6, 7, 0, 2, 0),
        (Ea::AbsW, true) => ph(6, 0, 6, 6, 0, 2, 0),
        (Ea::AbsL | Ea::Brief, false) => ph(6, 0, 6, 6, 0, 2, 0),
        (Ea::AbsL | Ea::Brief, true) => ph(8, 0, 8, 8, 0, 2, 0),
        (Ea::Full(full), false) => CIEA_FULL_W[full.row()],
        (Ea::Full(full), true) => CIEA_FULL_L[full.row()],
    }
}

const JEA_FULL: [Part; 12] = [
    ph(6, 0, 6, 6, 0, 0, 0),
    p(4, 0, 8, 9, 0, 1, 0),
    p(4, 0, 12, 13, 0, 1, 0),
    p(4, 0, 10, 10, 1, 1, 0),
    p(4, 0, 12, 12, 1, 1, 0),
    p(4, 0, 12, 12, 1, 1, 0),
    p(4, 0, 12, 13, 1, 1, 0),
    p(4, 0, 14, 15, 1, 1, 0),
    p(4, 0, 14, 15, 1, 1, 0),
    p(4, 0, 16, 17, 1, 2, 0),
    p(4, 0, 18, 19, 1, 2, 0),
    p(4, 0, 18, 19, 1, 2, 0),
];

/// Jump effective address (Section 11.6.5) for `JMP` and `JSR`.
pub fn jea(ea: Ea) -> Option<Part> {
    Some(match ea {
        Ea::Ind | Ea::AbsW | Ea::AbsL => ph(2, 0, 2, 2, 0, 0, 0),
        Ea::Disp16 => ph(4, 0, 4, 4, 0, 0, 0),
        Ea::Brief => ph(6, 0, 6, 6, 0, 0, 0),
        Ea::Full(full) => JEA_FULL[full.row()],
        _ => return None,
    })
}

const MOVE_FULL: [Part; 12] = [
    p(4, 0, 8, 9, 0, 1, 1),
    p(4, 0, 10, 12, 0, 2, 1),
    p(4, 0, 14, 16, 0, 2, 1),
    p(4, 0, 10, 11, 1, 1, 1),
    p(4, 0, 12, 14, 1, 2, 1),
    p(4, 0, 14, 16, 1, 2, 1),
    p(4, 0, 12, 14, 1, 2, 1),
    p(4, 0, 14, 17, 1, 2, 1),
    p(4, 0, 16, 19, 1, 3, 1),
    p(4, 0, 16, 18, 1, 2, 1),
    p(4, 0, 18, 21, 1, 3, 1),
    p(4, 0, 20, 23, 1, 3, 1),
];

/// `MOVE` to `dst` (Section 11.6.6), including the destination address calculation.
/// `from_register` selects the faster rows for a `Dn` or `An` source.
pub fn move_to(dst: Ea, from_register: bool) -> Part {
    match (dst, from_register) {
        (Ea::Dn | Ea::An, true) => p(2, 0, 2, 2, 0, 1, 0),
        (Ea::Dn | Ea::An, false) => p(0, 0, 2, 2, 0, 1, 0),
        (Ea::Ind | Ea::PostInc, true) => p(0, 1, 3, 4, 0, 1, 1),
        (Ea::PreDec, true) => p(0, 2, 4, 4, 0, 1, 1),
        (Ea::Ind | Ea::PostInc | Ea::PreDec | Ea::Disp16 | Ea::AbsW, _) => p(2, 0, 4, 5, 0, 1, 1),
        (Ea::AbsL, _) => p(0, 0, 6, 7, 0, 2, 1),
        (Ea::Brief, _) => p(4, 0, 6, 7, 0, 1, 1),
        (Ea::Full(full), _) => MOVE_FULL[full.row()],
        (Ea::Imm { .. }, _) => p(0, 0, 2, 2, 0, 1, 0),
    }
}

/// What an executed instruction did, recorded by the core for [`Timing030::evaluate`].
#[derive(Debug, Copy, Clone)]
pub struct Trace {
    /// Effective addresses in the order the instruction resolved them.
    pub eas: [Option<Ea>; 2],
    pub branch_taken: bool,
    /// `DBcc` ran its counter out.
    pub count_expired: bool,
    /// Exception vectors raised while executing (a trace exception may follow another).
    pub exceptions: [Option<u8>; 2],
    pub bus_fault: bool,
    /// An interrupt was taken instead of an instruction (`Some(true)` on the master stack).
    pub interrupt: Option<bool>,
    /// A trap or line A/F instruction was handled natively, with these extra cycles.
    pub native: Option<u32>,
    /// `MOVEM` register count.
    pub registers: u16,
    /// A memory bit field spans five bytes.
    pub wide_field: bool,
    /// A register shift count exceeded the operand size.
    pub long_shift: bool,
    /// `RTE` frame formats, the throwaway one first.
    pub rte_formats: [Option<u8>; 2],
    /// First extension word, for instructions whose timing depends on it.
    pub ext: u16,
    /// Instruction prefetches that missed the instruction cache, and the wait of the last.
    pub fetch_misses: u16,
    pub fetch_wait: u16,
    /// Waits of the first memory-indirect pointer fetch, first operand read (with whether it
    /// hit the data cache), and first write. A wait counts the clocks beyond a 2-clock cycle.
    pub indirect_wait: Option<u16>,
    pub read: Option<(u16, bool)>,
    pub write_wait: Option<u16>,
    /// The core is fetching a memory-indirect pointer.
    pub fetching_pointer: bool,
}

impl Default for Trace {
    fn default() -> Self {
        Self::new()
    }
}

impl Trace {
    pub const fn new() -> Self {
        Self {
            eas: [None; 2],
            branch_taken: false,
            count_expired: false,
            exceptions: [None; 2],
            bus_fault: false,
            interrupt: None,
            native: None,
            registers: 0,
            wide_field: false,
            long_shift: false,
            rte_formats: [None; 2],
            ext: 0,
            fetch_misses: 0,
            fetch_wait: 0,
            indirect_wait: None,
            read: None,
            write_wait: None,
            fetching_pointer: false,
        }
    }

    pub fn push_ea(&mut self, ea: Ea) {
        if let Some(slot) = self.eas.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(ea);
        }
    }

    pub fn push_exception(&mut self, vector: u8) {
        if let Some(slot) = self.exceptions.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(vector);
        }
    }

    pub fn push_rte_format(&mut self, format: u8) {
        if let Some(slot) = self.rte_formats.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(format);
        }
    }

    /// Records a data access with `wait` clocks beyond a 2-clock bus cycle.
    pub fn record_access(&mut self, is_write: bool, wait: u16, hit: bool) {
        if is_write {
            self.write_wait.get_or_insert(wait);
        } else if self.fetching_pointer {
            self.indirect_wait.get_or_insert(wait);
        } else {
            self.read.get_or_insert((wait, hit));
        }
    }
}

/// Which recorded accesses a part accounts for.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Role {
    /// Fetches the operand (`fea`, `fiea`).
    Fetch,
    /// Calculates the address only (`cea`, `ciea`, `jea`).
    Calculate,
    Operation,
    /// `MOVEM` memory to registers, whose waits follow the footnote of Section 11.6.7.
    MovemLoad,
    /// `MOVEM` registers to memory.
    MovemStore,
}

const EXCEPTION: Part = p(0, 0, 18, 20, 1, 2, 4);
const FORMAT_2_EXCEPTION: Part = p(2, 0, 22, 24, 1, 2, 5);
const TRACE: Part = p(0, 0, 22, 24, 1, 2, 5);
const LONG_BUS_FAULT: Part = p(0, 0, 62, 64, 1, 2, 24);

/// Parts of one instruction, in execution order.
struct Parts {
    parts: [(Part, Role); 4],
    len: usize,
}

impl Parts {
    fn new() -> Self {
        Self {
            parts: [(p(0, 0, 0, 0, 0, 0, 0), Role::Operation); 4],
            len: 0,
        }
    }

    fn push(&mut self, part: Part, role: Role) -> &mut Self {
        if self.len < self.parts.len() {
            self.parts[self.len] = (part, role);
            self.len += 1;
        }
        self
    }

    fn push_ea(&mut self, part: Option<Part>, role: Role) -> &mut Self {
        if let Some(part) = part {
            self.push(part, role);
        }
        self
    }

    fn op(&mut self, part: Part) -> &mut Self {
        self.push(part, Role::Operation)
    }

    fn as_slice(&self) -> &[(Part, Role)] {
        &self.parts[..self.len]
    }
}

fn is_register(ea: Option<Ea>) -> bool {
    matches!(ea, Some(Ea::Dn | Ea::An) | None)
}

/// Builds the parts of the instruction `op` from what `trace` recorded.
fn classify(op: u16, trace: &Trace, zero_flag: bool) -> Parts {
    let mut parts = Parts::new();
    let ea0 = trace.eas[0].unwrap_or(Ea::Dn);
    let ea0_reg = is_register(trace.eas[0]);
    let exception = trace.exceptions[0];
    let generic_exception = |vector: u8| matches!(vector, 4 | 8 | 10 | 11 | 14 | 32..=47);

    if let Some(master) = trace.interrupt {
        parts.op(if master {
            p(0, 0, 33, 34, 2, 2, 8)
        } else {
            p(0, 0, 23, 24, 2, 2, 4)
        });
        return parts;
    }
    if trace.bus_fault {
        parts.op(LONG_BUS_FAULT);
        return parts;
    }
    if let Some(extra) = trace.native {
        let extra = extra.min(u16::MAX as u32) as u16;
        parts.op(p(0, 0, 2 + extra, 2 + extra, 0, 1, 0));
        return parts;
    }
    if exception.is_some_and(generic_exception) {
        parts.op(EXCEPTION);
        return parts;
    }
    let trapped = exception.is_some();
    let size_long = |op: u16| (op >> 6) & 3 == 2;
    let rmw = p(0, 1, 3, 4, 0, 1, 1);
    let reg_op = p(2, 0, 2, 2, 0, 1, 0);
    let ea_reg_op = p(0, 0, 2, 2, 0, 1, 0);

    match op >> 12 {
        0x0 => {
            if (op & 0x0138) == 0x0108 {
                parts.op(match (op >> 6) & 7 {
                    4 => p(2, 0, 10, 10, 2, 1, 0),
                    5 => p(2, 0, 14, 14, 4, 1, 0),
                    6 => p(4, 0, 10, 10, 0, 1, 2),
                    _ => p(4, 0, 14, 14, 0, 1, 4),
                });
            } else if (op & 0x0100) != 0 || (op & 0x0F00) == 0x0800 {
                let test = (op >> 6) & 3 == 0;
                if ea0_reg {
                    parts.op(if test {
                        p(4, 0, 4, 4, 0, 1, 0)
                    } else {
                        p(6, 0, 6, 6, 0, 1, 0)
                    });
                } else {
                    if (op & 0x0100) != 0 {
                        parts.push_ea(fea(ea0), Role::Fetch);
                    } else {
                        parts.push(fiea(ea0, false), Role::Fetch);
                    }
                    parts.op(if test {
                        p(0, 0, 4, 4, 0, 1, 0)
                    } else {
                        p(0, 0, 6, 6, 0, 1, 1)
                    });
                }
            } else if matches!(op, 0x003C | 0x023C | 0x0A3C | 0x007C | 0x027C | 0x0A7C) {
                parts.op(p(4, 0, 12, 14, 0, 2, 0));
            } else if (op & 0xF9C0) == 0x00C0 && (op >> 9) & 3 != 3 {
                parts.push(fiea(ea0, false), Role::Fetch);
                parts.op(if trapped {
                    p(2, 0, 40, 42, 2, 3, 4)
                } else if (trace.ext & 0x0800) != 0 {
                    p(2, 0, 18, 18, 1, 1, 0)
                } else {
                    p(2, 0, 20, 20, 1, 1, 0)
                });
            } else if (op & 0xF9FF) == 0x08FC {
                parts.op(if zero_flag {
                    p(2, 0, 24, 26, 2, 2, 2)
                } else {
                    p(2, 0, 24, 24, 2, 2, 0)
                });
            } else if (op & 0xF9C0) == 0x08C0 {
                parts.push(ciea(ea0, false), Role::Calculate);
                parts.op(if zero_flag {
                    p(1, 0, 13, 13, 1, 1, 1)
                } else {
                    p(1, 0, 11, 11, 1, 1, 0)
                });
            } else if (op & 0xFF00) == 0x0E00 {
                parts.push(ciea(ea0, false), Role::Calculate);
                parts.op(if (trace.ext & 0x0800) != 0 {
                    p(2, 1, 5, 6, 0, 1, 1)
                } else {
                    p(3, 0, 7, 7, 1, 1, 0)
                });
            } else {
                // ORI, ANDI, SUBI, ADDI, EORI, CMPI
                parts.push(fiea(ea0, size_long(op)), Role::Fetch);
                let compare = (op >> 9) & 7 == 6;
                parts.op(match (ea0_reg, compare) {
                    (true, _) => reg_op,
                    (false, true) => p(0, 0, 2, 2, 0, 1, 0),
                    (false, false) => rmw,
                });
            }
        }
        0x1..=0x3 => {
            parts.push_ea(fea(ea0), Role::Fetch);
            parts.op(move_to(trace.eas[1].unwrap_or(Ea::Dn), ea0_reg));
        }
        0x4 => classify_group_4(op, trace, ea0, ea0_reg, trapped, &mut parts),
        0x5 => {
            if (op & 0x00C0) == 0x00C0 {
                let mode = (op >> 3) & 7;
                let reg = op & 7;
                if mode == 7 && (2..=4).contains(&reg) {
                    parts.op(match (reg, trapped) {
                        (4, false) => p(4, 0, 4, 4, 0, 1, 0),
                        (2, false) => p(6, 0, 6, 6, 0, 1, 0),
                        (_, false) => p(8, 0, 8, 8, 0, 2, 0),
                        (4, true) => FORMAT_2_EXCEPTION,
                        (2, true) => p(5, 0, 24, 26, 1, 3, 5),
                        (_, true) => p(6, 0, 26, 28, 1, 3, 5),
                    });
                } else if mode == 1 {
                    parts.op(if trace.branch_taken {
                        p(6, 0, 6, 8, 0, 2, 0)
                    } else if trace.count_expired {
                        p(10, 0, 10, 13, 0, 3, 0)
                    } else {
                        p(6, 0, 6, 8, 0, 1, 0)
                    });
                } else if ea0_reg {
                    parts.op(p(4, 0, 4, 4, 0, 1, 0));
                } else {
                    parts.push_ea(cea(ea0), Role::Calculate);
                    parts.op(p(0, 1, 5, 5, 0, 1, 1));
                }
            } else if ea0_reg {
                parts.op(reg_op);
            } else {
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(rmw);
            }
        }
        0x6 => {
            let disp8 = op & 0xFF;
            parts.op(if (op >> 8) & 0xF == 1 {
                p(2, 0, 6, 9, 0, 2, 1)
            } else if trace.branch_taken {
                p(6, 0, 6, 8, 0, 2, 0)
            } else if disp8 == 0 {
                p(6, 0, 6, 6, 0, 1, 0)
            } else if disp8 == 0xFF {
                p(6, 0, 6, 8, 0, 2, 0)
            } else {
                p(4, 0, 4, 4, 0, 1, 0)
            });
        }
        0x7 => {
            parts.op(reg_op);
        }
        0x8 | 0xC => {
            let opmode = (op >> 6) & 7;
            let memory = (op & 8) != 0;
            if (op & 0x00C0) == 0x00C0 {
                // DIVU.W / DIVS.W / MULU.W / MULS.W
                let time = match (op >> 12, (op & 0x0100) != 0) {
                    (0x8, false) => 44,
                    (0x8, true) => 56,
                    _ => 28,
                };
                if trapped {
                    parts.push_ea(fea(ea0), Role::Fetch);
                    parts.op(FORMAT_2_EXCEPTION);
                } else if ea0_reg {
                    parts.op(p(2, 0, time, time, 0, 1, 0));
                } else {
                    parts.push_ea(fea(ea0), Role::Fetch);
                    parts.op(p(
                        if op >> 12 == 0xC { 2 } else { 0 },
                        0,
                        time,
                        time,
                        0,
                        1,
                        0,
                    ));
                }
            } else if (op & 0x01F0) == 0x0100 {
                // SBCD / ABCD
                parts.op(if memory {
                    p(2, 1, 13, 14, 2, 1, 1)
                } else {
                    p(0, 0, 4, 4, 0, 1, 0)
                });
            } else if op >> 12 == 0x8 && (op & 0x01F0) == 0x0140 {
                parts.op(if memory {
                    p(2, 1, 11, 11, 1, 1, 1)
                } else {
                    p(6, 0, 6, 6, 0, 1, 0)
                });
            } else if op >> 12 == 0x8 && (op & 0x01F0) == 0x0180 {
                parts.op(if memory {
                    p(2, 1, 11, 11, 1, 1, 1)
                } else {
                    p(8, 0, 8, 8, 0, 1, 0)
                });
            } else if op >> 12 == 0xC && (op & 0x0130) == 0x0100 {
                parts.op(p(4, 0, 4, 4, 0, 1, 0));
            } else {
                // OR / AND
                let to_ea = (opmode & 4) != 0;
                if ea0_reg {
                    parts.op(reg_op);
                } else {
                    parts.push_ea(fea(ea0), Role::Fetch);
                    parts.op(if to_ea { rmw } else { ea_reg_op });
                }
            }
        }
        0x9 | 0xD => {
            let opmode = (op >> 6) & 7;
            if opmode == 3 || opmode == 7 {
                let word = opmode == 3;
                if ea0_reg {
                    parts.op(if word { p(4, 0, 4, 4, 0, 1, 0) } else { reg_op });
                } else {
                    parts.push_ea(fea(ea0), Role::Fetch);
                    parts.op(if word {
                        p(0, 0, 4, 4, 0, 1, 0)
                    } else {
                        ea_reg_op
                    });
                }
            } else if (op & 0x0130) == 0x0100 {
                parts.op(if (op & 8) != 0 {
                    p(2, 1, 9, 10, 2, 1, 1)
                } else {
                    reg_op
                });
            } else if ea0_reg {
                parts.op(reg_op);
            } else {
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(if (opmode & 4) != 0 { rmw } else { ea_reg_op });
            }
        }
        0xA => {
            parts.op(EXCEPTION);
        }
        0xB => {
            let opmode = (op >> 6) & 7;
            if opmode == 3 || opmode == 7 {
                if ea0_reg {
                    parts.op(p(4, 0, 4, 4, 0, 1, 0));
                } else {
                    parts.push_ea(fea(ea0), Role::Fetch);
                    parts.op(p(0, 0, 4, 4, 0, 1, 0));
                }
            } else if (op & 0x0100) != 0 && (op >> 3) & 7 == 1 {
                parts.op(p(0, 0, 8, 8, 2, 1, 0));
            } else if ea0_reg {
                parts.op(reg_op);
            } else {
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(if (op & 0x0100) != 0 { rmw } else { ea_reg_op });
            }
        }
        0xE => {
            if (op & 0x08C0) == 0x08C0 {
                classify_bitfield(op, trace, ea0, ea0_reg, &mut parts);
            } else if (op & 0x00C0) == 0x00C0 {
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(match ((op >> 9) & 3, (op & 0x0100) != 0) {
                    (0, true) | (3, _) => p(0, 0, 6, 6, 0, 1, 1),
                    _ => p(0, 0, 4, 4, 0, 1, 1),
                });
            } else {
                let by_register = (op & 0x0020) != 0;
                let left = (op & 0x0100) != 0;
                parts.op(match ((op >> 3) & 3, left, by_register) {
                    (0, true, false) => p(2, 0, 6, 6, 0, 1, 0),
                    (0, true, true) => p(4, 0, 8, 8, 0, 1, 0),
                    (0, false, false) | (1, _, false) => p(4, 0, 4, 4, 0, 1, 0),
                    (0, false, true) if trace.long_shift => p(10, 0, 10, 10, 0, 1, 0),
                    (1, _, true) if trace.long_shift => p(8, 0, 8, 8, 0, 1, 0),
                    (0 | 1, _, true) => p(6, 0, 6, 6, 0, 1, 0),
                    (2, _, _) => p(10, 0, 12, 12, 0, 1, 0),
                    (_, _, false) => p(4, 0, 6, 6, 0, 1, 0),
                    (_, _, true) => p(6, 0, 8, 8, 0, 1, 0),
                });
            }
        }
        _ => {
            // MMU instructions have no table; they are timed like MOVEC to a cache register.
            parts.op(p(4, 0, 12, 12, 0, 1, 0));
        }
    }
    parts
}

fn classify_group_4(
    op: u16,
    trace: &Trace,
    ea0: Ea,
    ea0_reg: bool,
    trapped: bool,
    parts: &mut Parts,
) {
    let reg_mode = (op >> 3) & 7;
    let chk = |parts: &mut Parts| {
        if trapped {
            parts.op(if ea0_reg {
                p(4, 0, 28, 30, 1, 3, 4)
            } else {
                p(0, 0, 28, 30, 1, 3, 4)
            });
        } else {
            parts.op(if ea0_reg {
                p(8, 0, 8, 8, 0, 1, 0)
            } else {
                p(0, 0, 8, 8, 0, 1, 0)
            });
        }
    };
    match op {
        0x4E70 => {
            parts.op(p(0, 0, 518, 518, 0, 1, 0));
        }
        0x4E71 => {
            parts.op(p(0, 0, 2, 2, 0, 1, 0));
        }
        0x4E72 => {
            parts.op(p(0, 0, 8, 8, 0, 2, 0));
        }
        0x4E73 => {
            for format in trace.rte_formats.iter().flatten() {
                parts.op(match format {
                    1 => p(1, 0, 12, 12, 4, 0, 0),
                    9 => p(1, 0, 26, 26, 7, 2, 0),
                    0xA => p(1, 0, 36, 38, 10, 2, 0),
                    0xB => p(1, 0, 76, 76, 25, 2, 0),
                    _ => p(1, 0, 18, 20, 4, 2, 0),
                });
            }
            if parts.len == 0 {
                parts.op(p(1, 0, 18, 20, 4, 2, 0));
            }
        }
        0x4E74 => {
            parts.op(p(2, 0, 10, 12, 1, 2, 0));
        }
        0x4E75 => {
            parts.op(p(1, 0, 9, 11, 1, 2, 0));
        }
        0x4E76 => {
            parts.op(if trapped {
                FORMAT_2_EXCEPTION
            } else {
                p(4, 0, 4, 4, 0, 1, 0)
            });
        }
        0x4E77 => {
            parts.op(p(1, 0, 12, 14, 2, 2, 0));
        }
        0x4E7A => {
            parts.op(p(6, 0, 6, 6, 0, 1, 0));
        }
        0x4E7B => {
            // Writing SFC, DFC, or CACR is slower than the other control registers.
            parts.op(if (trace.ext & 0x0FFF) <= 2 {
                p(4, 0, 12, 12, 0, 1, 0)
            } else {
                p(6, 0, 6, 6, 0, 1, 0)
            });
        }
        0x4E50..=0x4E57 => {
            parts.op(p(0, 0, 4, 5, 0, 1, 1));
        }
        0x4808..=0x480F => {
            parts.op(p(2, 0, 6, 7, 0, 2, 1));
        }
        0x4E58..=0x4E5F => {
            parts.op(p(0, 0, 5, 5, 1, 1, 0));
        }
        0x4E60..=0x4E6F | 0x4840..=0x4847 | 0x4880..=0x4887 | 0x48C0..=0x48C7 | 0x49C0..=0x49C7 => {
            // MOVE USP, SWAP, EXT, EXTB
            parts.op(p(4, 0, 4, 4, 0, 1, 0));
        }
        _ if (op & 0xFFC0) == 0x4E80 => {
            parts.push_ea(jea(ea0), Role::Calculate);
            parts.op(p(0, 0, 4, 7, 0, 2, 1));
        }
        _ if (op & 0xFFC0) == 0x4EC0 => {
            parts.push_ea(jea(ea0), Role::Calculate);
            parts.op(p(4, 0, 4, 6, 0, 2, 0));
        }
        _ if (op & 0xFFC0) == 0x4840 => {
            parts.push_ea(cea(ea0), Role::Calculate);
            parts.op(p(0, 2, 4, 4, 0, 1, 1));
        }
        _ if (op & 0xFB80) == 0x4880 => {
            let ea = match reg_mode {
                3 => Ea::PostInc,
                4 => Ea::PreDec,
                _ => ea0,
            };
            let n = trace.registers;
            parts.push(ciea(ea, false), Role::Calculate);
            if (op & 0x0400) != 0 {
                parts.push(p(2, 0, 8 + 4 * n, 8 + 4 * n, n, 1, 0), Role::MovemLoad);
            } else {
                parts.push(p(2, 0, 4 + 2 * n, 4 + 2 * n, 0, 1, n), Role::MovemStore);
            }
        }
        _ if (op & 0xF1C0) == 0x41C0 => {
            parts.push_ea(cea(ea0), Role::Calculate);
            parts.op(p(2, 0, 2, 2, 0, 1, 0));
        }
        _ if (op & 0xF1C0) == 0x4180 || (op & 0xF1C0) == 0x4100 => {
            parts.push_ea(fea(ea0), Role::Fetch);
            chk(parts);
        }
        _ if (op & 0xFFC0) == 0x4C00 => {
            parts.push(fiea(ea0, false), Role::Fetch);
            parts.op(p(2, 0, 44, 44, 0, 1, 0));
        }
        _ if (op & 0xFFC0) == 0x4C40 => {
            parts.push(fiea(ea0, false), Role::Fetch);
            if trapped {
                parts.op(FORMAT_2_EXCEPTION);
            } else {
                let time = if (trace.ext & 0x0800) != 0 { 90 } else { 78 };
                parts.op(p(if ea0_reg { 6 } else { 0 }, 0, time, time, 0, 1, 0));
            }
        }
        _ if (op & 0xFDC0) == 0x40C0 => {
            // MOVE from SR / MOVE from CCR
            if ea0_reg {
                parts.op(p(2, 0, 4, 4, 0, 1, 0));
            } else {
                parts.push_ea(cea(ea0), Role::Calculate);
                parts.op(p(2, 0, 4, 5, 0, 1, 1));
            }
        }
        _ if (op & 0xFFC0) == 0x44C0 => {
            if ea0 == Ea::Dn {
                parts.op(p(4, 0, 4, 4, 0, 1, 0));
            } else {
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(p(0, 0, 4, 4, 0, 1, 0));
            }
        }
        _ if (op & 0xFFC0) == 0x46C0 => {
            parts.push_ea(fea(ea0), Role::Fetch);
            parts.op(p(0, 0, 8, 10, 0, 2, 0));
        }
        _ if (op & 0xFFC0) == 0x4800 => {
            if ea0_reg {
                parts.op(p(0, 0, 6, 6, 0, 1, 0));
            } else {
                // Not in the tables; timed like NEG on memory plus the BCD adjustment.
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(p(0, 1, 7, 8, 0, 1, 1));
            }
        }
        _ if (op & 0xFFC0) == 0x4AC0 => {
            if ea0_reg {
                parts.op(p(4, 0, 4, 4, 0, 1, 0));
            } else {
                parts.push_ea(cea(ea0), Role::Calculate);
                parts.op(p(3, 0, 12, 12, 1, 1, 1));
            }
        }
        _ => {
            // NEGX, CLR, NEG, NOT, TST
            let sub = (op >> 9) & 7;
            if ea0_reg {
                parts.op(if sub == 5 {
                    p(0, 0, 2, 2, 0, 1, 0)
                } else {
                    p(2, 0, 2, 2, 0, 1, 0)
                });
            } else if sub == 1 {
                parts.push_ea(cea(ea0), Role::Calculate);
                parts.op(p(0, 1, 3, 4, 0, 1, 1));
            } else {
                parts.push_ea(fea(ea0), Role::Fetch);
                parts.op(if sub == 5 {
                    p(0, 0, 2, 2, 0, 1, 0)
                } else {
                    p(0, 1, 3, 4, 0, 1, 1)
                });
            }
        }
    }
}

fn classify_bitfield(op: u16, trace: &Trace, ea0: Ea, ea0_reg: bool, parts: &mut Parts) {
    parts.push(ciea(ea0, false), Role::Calculate);
    // Register, memory of up to four bytes, and memory of five bytes.
    let [register, narrow, wide] = match (op >> 8) & 7 {
        0 => [
            p(8, 0, 8, 8, 0, 1, 0),
            p(6, 0, 10, 10, 1, 1, 0),
            p(6, 0, 14, 14, 2, 1, 0),
        ],
        1 | 3 => [
            p(10, 0, 10, 10, 0, 1, 0),
            p(6, 0, 12, 12, 1, 1, 0),
            p(6, 0, 18, 18, 2, 1, 0),
        ],
        5 => [
            p(20, 0, 20, 20, 0, 1, 0),
            p(6, 0, 22, 22, 1, 1, 0),
            p(6, 0, 28, 28, 2, 1, 0),
        ],
        7 => [
            p(12, 0, 12, 12, 0, 1, 0),
            p(6, 0, 12, 12, 1, 1, 1),
            p(6, 0, 18, 18, 2, 1, 2),
        ],
        _ => [
            p(14, 0, 14, 14, 0, 1, 0),
            p(6, 0, 14, 14, 1, 1, 1),
            p(6, 0, 22, 22, 2, 1, 2),
        ],
    };
    parts.op(if ea0_reg {
        register
    } else if trace.wide_field {
        wide
    } else {
        narrow
    });
}

impl Timing030 {
    /// Returns the clocks taken by the instruction `op` that `trace` recorded, and updates
    /// the tail carried into the next instruction.
    pub fn evaluate(&mut self, op: u16, trace: &Trace, zero_flag: bool) -> u32 {
        let mut parts = classify(op, trace, zero_flag);
        for vector in trace.exceptions.iter().flatten() {
            if *vector == 9 {
                parts.op(TRACE);
            }
        }
        let parts = parts.as_slice();
        let read_wait = trace
            .read
            .map_or(0, |(wait, hit)| if hit { 0 } else { wait });
        let write_wait = trace.write_wait.unwrap_or(0);
        let indirect_wait = trace.indirect_wait.unwrap_or(0);

        if trace.fetch_misses > 0 {
            // No-cache case: no overlap, and every bus cycle pays its wait states.
            let mut total = 0u32;
            for (part, _) in parts {
                total += (part.ncc
                    + part.reads * read_wait
                    + part.writes * write_wait
                    + part.fetches * trace.fetch_wait) as u32;
            }
            self.tail = 0;
            return total;
        }

        let mut total = 0i32;
        for (i, &(part, role)) in parts.iter().enumerate() {
            let mut part = part;
            match role {
                Role::Fetch if part.reads > 0 => match trace.read {
                    // Section 11.4: a data cache hit shortens the fetch.
                    Some((_, true)) if part.reads == 1 => {
                        let cut = part.tail.min(1).max(part.tail.saturating_sub(1));
                        part.cc -= cut;
                        part.tail -= cut;
                    }
                    _ if part.reads == 2 => {
                        part.cc += indirect_wait + read_wait;
                        part.tail += read_wait;
                    }
                    _ => {
                        part.cc += read_wait;
                        part.tail += read_wait;
                    }
                },
                Role::Calculate => part.cc += part.reads * indirect_wait,
                Role::MovemLoad => {
                    if read_wait > 2 {
                        part.cc += (read_wait - 2) * part.reads;
                    }
                }
                Role::MovemStore => {
                    let n = part.writes;
                    let w = write_wait;
                    part.cc += n.saturating_sub(1) * w + w.saturating_sub(2);
                    part.tail = n.saturating_sub(1) * w;
                }
                _ => {
                    part.cc += part.reads * read_wait + part.writes * write_wait;
                    if part.writes > 0 {
                        part.tail += write_wait;
                    }
                }
            }
            let head = if part.plus_op_head {
                part.head + parts.get(i + 1).map_or(0, |(next, _)| next.head)
            } else {
                part.head
            };
            total += part.cc as i32 - head.min(self.tail) as i32;
            self.tail = part.tail;
        }
        total.max(0) as u32
    }
}

impl Timing030 {
    /// Returns the clocks an access of `size` takes on `cycle` beyond the 2-clock cycle the
    /// tables assume, adding DRAM refresh delay once it adds up to whole clocks.
    pub fn access_wait(&mut self, cycle: crate::bus::BusCycle030, size: DataSize) -> u16 {
        let cycles = if cycle.port16 && size == DataSize::Long {
            2
        } else {
            1
        };
        let refresh = self.refresh as u16 + cycles * cycle.refresh_eighths as u16;
        self.refresh = (refresh % 8) as u8;
        cycles * (2 + cycle.wait as u16) - 2 + refresh / 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(timing: &mut Timing030, op: u16, eas: &[Ea]) -> u32 {
        let mut trace = Trace::new();
        for &ea in eas {
            trace.push_ea(ea);
        }
        timing.evaluate(op, &trace, false)
    }

    /// Section 11.3.4: ADD.L A1,D1 then a 4-clock SUBA take 6 clocks. The example calls it
    /// SUBA.L, but its numbers are those of the SUBA.W row in Section 11.6.8.
    #[test]
    fn register_example_overlaps_as_in_the_manual() {
        let mut timing = Timing030::new();
        let add = run(&mut timing, 0xD289, &[Ea::An]); // add.l a1,d1
        let suba = run(&mut timing, 0x94C1, &[Ea::Dn]); // suba.w d1,a2
        assert_eq!(add + suba, 6);
    }

    /// Section 11.3.4: the five-instruction example takes 40 clocks.
    #[test]
    fn effective_address_example_matches_the_manual() {
        let mut timing = Timing030::new();
        let full_indirect = Ea::Full(Full {
            base_disp: 0,
            indirect: true,
            outer_disp: 0,
        });
        let total = run(&mut timing, 0xD2A1, &[Ea::PreDec]) // add.l -(a1),d1
            + run(&mut timing, 0xC3B0, &[full_indirect]) // and.l d1,([a2])
            + run(&mut timing, 0x2356, &[Ea::Ind, Ea::Disp16]) // move.l (a6),(8,a1)
            + run(&mut timing, 0x4ADB, &[Ea::PostInc]) // tas (a3)+
            + run(&mut timing, 0x4483, &[Ea::Dn]); // neg.l d3
        assert_eq!(total, 40);
    }

    /// Section 11.3.4: EORI.W #$400,-(A1) then ADDI.L #$6000FF,D1.
    #[test]
    fn immediate_example_matches_the_manual() {
        let mut timing = Timing030::new();
        let total = run(&mut timing, 0x0A61, &[Ea::PreDec]) + run(&mut timing, 0x0681, &[Ea::Dn]);
        // fiea -(An) 4, EORI mem 3 - min(0, 2), fiea Dn 4 - min(6, 1), ADDI 2 - min(2, 0).
        assert_eq!(total, 4 + 3 + 3 + 2);
    }

    /// Runs the data cache examples of Sections 11.4 and 11.5: every operand read hits, and
    /// every bus cycle has `wait` wait states.
    fn data_cache_example(wait: u16) -> u32 {
        let mut timing = Timing030::new();
        let hit = Some((wait, true));
        let full_indirect = Ea::Full(Full {
            base_disp: 0,
            indirect: true,
            outer_disp: 0,
        });
        let mut total = 0;
        let steps: [(u16, &[Ea], bool); 4] = [
            (0xD2A1, &[Ea::PreDec], false),         // add.l -(a1),d1
            (0xC3B2, &[full_indirect], true),       // and.l d1,([a2])
            (0x2356, &[Ea::Ind, Ea::Disp16], true), // move.l (a6),(8,a1)
            (0x4ADB, &[Ea::PostInc], true),         // tas (a3)+
        ];
        for (op, eas, writes) in steps {
            let mut trace = Trace::new();
            for &ea in eas {
                trace.push_ea(ea);
            }
            trace.read = hit;
            // The examples assume the pointer fetch of ([A2]) misses.
            trace.indirect_wait = (op == 0xC3B2).then_some(wait);
            trace.write_wait = writes.then_some(wait);
            total += timing.evaluate(op, &trace, false);
        }
        total
    }

    /// Section 11.4: 37 clocks when the operand reads hit the data cache.
    #[test]
    fn data_cache_hits_match_the_manual() {
        assert_eq!(data_cache_example(0), 37);
    }

    /// Section 11.5: 45 clocks with data cache hits and two wait states.
    #[test]
    fn wait_states_with_data_cache_hits_match_the_manual() {
        assert_eq!(data_cache_example(2), 45);
    }

    #[test]
    fn dram_refresh_adds_a_clock_every_eight_cycles() {
        let mut timing = Timing030::new();
        let ram = crate::bus::BusCycle030 {
            refresh_eighths: 1,
            ..Default::default()
        };
        let waits: Vec<u16> = (0..8).map(|_| timing.access_wait(ram, DataSize::Word)).collect();
        assert_eq!(waits, [0, 0, 0, 0, 0, 0, 0, 1]);
        let io = crate::bus::BusCycle030 {
            wait: 7,
            port16: true,
            cacheable: false,
            refresh_eighths: 0,
        };
        assert_eq!(timing.access_wait(io, DataSize::Long), 2 * 9 - 2);
    }

    #[test]
    fn cache_tags_track_long_words_and_function_codes() {
        let mut cache = CacheTags::new();
        cache.fill(0x1234, true);
        assert!(cache.hit(0x1236, true));
        assert!(!cache.hit(0x1238, true));
        assert!(!cache.hit(0x1234, false));
        cache.fill(0x2234, true);
        assert!(!cache.hit(0x1234, true), "same line, new tag");
        cache.clear_entry(0x2234);
        assert!(!cache.hit(0x2234, true));
    }
}
