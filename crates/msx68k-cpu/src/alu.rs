//! MC68000 arithmetic, logical, shift/rotate, BCD, and condition-code evaluation helpers.

use crate::state::{CpuState, DataSize, SR_C, SR_CCR_MASK, SR_N, SR_V, SR_X, SR_Z};

/// Evaluates the 16 standard MC68000 condition codes (`cc` = `0..=15`).
#[inline]
pub const fn eval_cc(sr: u16, cc: u8) -> bool {
    let c = (sr & SR_C) != 0;
    let v = (sr & SR_V) != 0;
    let z = (sr & SR_Z) != 0;
    let n = (sr & SR_N) != 0;

    match cc & 0x0F {
        0x0 => true,              // T  (True)
        0x1 => false,             // F  (False)
        0x2 => !c && !z,          // HI (High)
        0x3 => c || z,            // LS (Low or Same)
        0x4 => !c,                // CC / HS (Carry Clear)
        0x5 => c,                 // CS / LO (Carry Set)
        0x6 => !z,                // NE (Not Equal)
        0x7 => z,                 // EQ (Equal)
        0x8 => !v,                // VC (Overflow Clear)
        0x9 => v,                 // VS (Overflow Set)
        0xA => !n,                // PL (Plus)
        0xB => n,                 // MI (Minus)
        0xC => n == v,            // GE (Greater or Equal)
        0xD => n != v,            // LT (Less Than)
        0xE => !z && (n == v),    // GT (Greater Than)
        0xF => z || (n != v),     // LE (Less or Equal)
        _ => unreachable!(),
    }
}

/// Sets `N` and `Z` according to `res` of `size`, clears `V` and `C`, and preserves `X`.
#[inline]
pub fn set_nz_clear_vc(state: &mut CpuState, size: DataSize, res: u32) {
    let res = size.mask_val(res);
    let mut flags = state.sr & ( !SR_CCR_MASK | SR_X );
    if res == 0 {
        flags |= SR_Z;
    }
    if (res & size.sign_bit()) != 0 {
        flags |= SR_N;
    }
    state.sr = flags;
}

/// Performs `dst + src` of `size` and updates `X, N, Z, V, C`.
#[inline]
pub fn alu_add(state: &mut CpuState, size: DataSize, src: u32, dst: u32) -> u32 {
    let mask = size.mask() as u64;
    let sign = size.sign_bit();
    let s = (src as u64) & mask;
    let d = (dst as u64) & mask;
    let r64 = d + s;
    let res = (r64 & mask) as u32;

    let carry = r64 > mask;
    let overflow = ((!(src ^ dst) & (src ^ res)) & sign) != 0;

    let mut sr = state.sr & !SR_CCR_MASK;
    if carry {
        sr |= SR_C | SR_X;
    }
    if overflow {
        sr |= SR_V;
    }
    if res == 0 {
        sr |= SR_Z;
    }
    if (res & sign) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    res
}

/// Performs `dst - src` of `size` and updates `X, N, Z, V, C` (or leaves `X` unchanged for `CMP`).
#[inline]
pub fn alu_sub(state: &mut CpuState, size: DataSize, src: u32, dst: u32, update_x: bool) -> u32 {
    let mask = size.mask() as u64;
    let sign = size.sign_bit();
    let s = (src as u64) & mask;
    let d = (dst as u64) & mask;
    let borrow = s > d;
    let res = (d.wrapping_sub(s) & mask) as u32;

    let overflow = (((src ^ dst) & (res ^ dst)) & sign) != 0;

    let preserve_mask = if update_x { !SR_CCR_MASK } else { !SR_CCR_MASK | SR_X };
    let mut sr = state.sr & preserve_mask;
    if borrow {
        sr |= SR_C;
        if update_x {
            sr |= SR_X;
        }
    }
    if overflow {
        sr |= SR_V;
    }
    if res == 0 {
        sr |= SR_Z;
    }
    if (res & sign) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    res
}

/// Performs `ADDX`: `dst + src + X` of `size`, where `Z` is only cleared (never set) when non-zero.
#[inline]
pub fn alu_addx(state: &mut CpuState, size: DataSize, src: u32, dst: u32) -> u32 {
    let mask = size.mask() as u64;
    let sign = size.sign_bit();
    let x = u64::from(state.flag(SR_X));
    let s = (src as u64) & mask;
    let d = (dst as u64) & mask;
    let r64 = d + s + x;
    let res = (r64 & mask) as u32;

    let carry = r64 > mask;
    let overflow = ((!(src ^ dst) & (src ^ res)) & sign) != 0;

    let prev_z = state.sr & SR_Z;
    let mut sr = state.sr & !SR_CCR_MASK;
    if carry {
        sr |= SR_C | SR_X;
    }
    if overflow {
        sr |= SR_V;
    }
    if res == 0 {
        sr |= prev_z;
    }
    if (res & sign) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    res
}

/// Performs `SUBX`: `dst - src - X` of `size`, where `Z` is only cleared (never set) when non-zero.
#[inline]
pub fn alu_subx(state: &mut CpuState, size: DataSize, src: u32, dst: u32) -> u32 {
    let mask = size.mask() as u64;
    let sign = size.sign_bit();
    let x = u64::from(state.flag(SR_X));
    let s = (src as u64) & mask;
    let d = (dst as u64) & mask;
    let subtrahend = s + x;
    let borrow = subtrahend > d;
    let res = (d.wrapping_sub(subtrahend) & mask) as u32;

    let overflow = (((src ^ dst) & (res ^ dst)) & sign) != 0;

    let prev_z = state.sr & SR_Z;
    let mut sr = state.sr & !SR_CCR_MASK;
    if borrow {
        sr |= SR_C | SR_X;
    }
    if overflow {
        sr |= SR_V;
    }
    if res == 0 {
        sr |= prev_z;
    }
    if (res & sign) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    res
}

/// Performs `ABCD`: packed BCD addition `dst + src + X`.
#[inline]
pub fn alu_abcd(state: &mut CpuState, src: u8, dst: u8) -> u8 {
    let x = u16::from(state.flag(SR_X));
    let s = src as u16;
    let d = dst as u16;
    let binary_sum = d + s + x;

    let lo = (d & 0x0F) + (s & 0x0F) + x;
    let mut corf: u16 = 0;
    if lo > 9 {
        corf += 0x06;
    }
    let hi = (d & 0xF0) + (s & 0xF0) + (lo & 0xF0) + (if lo > 9 && (lo & 0xF0) == 0 { 0x10 } else { 0 });
    let _ = hi;
    if binary_sum > 0x99 || ((binary_sum + corf) & 0x1F0) > 0x90 {
        corf += 0x60;
    }
    let res16 = binary_sum.wrapping_add(corf);
    let res = (res16 & 0xFF) as u8;
    let carry = (res16 & 0x100) != 0 || binary_sum > 0xFF;
    let overflow = (!binary_sum & res16 & 0x80) != 0;

    let prev_z = state.sr & SR_Z;
    let mut sr = state.sr & !SR_CCR_MASK;
    if carry {
        sr |= SR_C | SR_X;
    }
    if overflow {
        sr |= SR_V;
    }
    if res == 0 {
        sr |= prev_z;
    }
    if (res & 0x80) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    res
}

/// Performs `SBCD`: packed BCD subtraction `dst - src - X`.
#[inline]
pub fn alu_sbcd(state: &mut CpuState, src: u8, dst: u8) -> u8 {
    let x = u16::from(state.flag(SR_X));
    let s = src as u16;
    let d = dst as u16;
    let binary_diff = d.wrapping_sub(s).wrapping_sub(x);

    let lo_borrow = (d & 0x0F) < ((s & 0x0F) + x);
    let hi_borrow = d < (s + x) || (binary_diff & 0x1F0) > 0x90;

    let mut corf: u16 = 0;
    if lo_borrow {
        corf += 0x06;
    }
    if hi_borrow {
        corf += 0x60;
    }

    let res16 = binary_diff.wrapping_sub(corf);
    let res = (res16 & 0xFF) as u8;
    let borrow = hi_borrow || (d < s + x);
    let overflow = (binary_diff & !res16 & 0x80) != 0;

    let prev_z = state.sr & SR_Z;
    let mut sr = state.sr & !SR_CCR_MASK;
    if borrow {
        sr |= SR_C | SR_X;
    }
    if overflow {
        sr |= SR_V;
    }
    if res == 0 {
        sr |= prev_z;
    }
    if (res & 0x80) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    res
}

/// Executes a shift or rotate operation (`kind`: 0=AS, 1=LS, 2=ROX, 3=RO).
pub fn alu_shift_rotate(
    state: &mut CpuState,
    size: DataSize,
    val: u32,
    count: u32,
    left: bool,
    kind: u8,
) -> u32 {
    let sign = size.sign_bit();
    let mask = size.mask();
    let mut v = val & mask;
    let mut c_out = false;
    let mut x_out = state.flag(SR_X);
    let mut v_out = false;

    if count == 0 {
        // On MC68000, a shift count of 0 leaves X unaffected, clears V, and clears C
        // EXCEPT for ROXL/ROXR where C is set to X!
        c_out = kind == 2 && x_out;
    } else {
        for _ in 0..count {
            match (kind, left) {
                // ASL
                (0, true) => {
                    let msb_before = (v & sign) != 0;
                    c_out = msb_before;
                    x_out = c_out;
                    v = (v << 1) & mask;
                    let msb_after = (v & sign) != 0;
                    if msb_before != msb_after {
                        v_out = true;
                    }
                }
                // ASR
                (0, false) => {
                    let msb = v & sign;
                    c_out = (v & 1) != 0;
                    x_out = c_out;
                    v = ((v >> 1) | msb) & mask;
                }
                // LSL
                (1, true) => {
                    c_out = (v & sign) != 0;
                    x_out = c_out;
                    v = (v << 1) & mask;
                }
                // LSR
                (1, false) => {
                    c_out = (v & 1) != 0;
                    x_out = c_out;
                    v = (v >> 1) & mask;
                }
                // ROXL
                (2, true) => {
                    let next_c = (v & sign) != 0;
                    v = ((v << 1) | u32::from(x_out)) & mask;
                    c_out = next_c;
                    x_out = next_c;
                }
                // ROXR
                (2, false) => {
                    let next_c = (v & 1) != 0;
                    v = (v >> 1) | (if x_out { sign } else { 0 });
                    c_out = next_c;
                    x_out = next_c;
                }
                // ROL
                (3, true) => {
                    c_out = (v & sign) != 0;
                    v = ((v << 1) | u32::from(c_out)) & mask;
                }
                // ROR
                (3, false) => {
                    c_out = (v & 1) != 0;
                    v = (v >> 1) | (if c_out { sign } else { 0 });
                }
                _ => unreachable!(),
            }
        }
    }

    let mut sr = state.sr & !SR_CCR_MASK;
    if x_out {
        sr |= SR_X;
    }
    if c_out {
        sr |= SR_C;
    }
    if v_out {
        sr |= SR_V;
    }
    if v == 0 {
        sr |= SR_Z;
    }
    if (v & sign) != 0 {
        sr |= SR_N;
    }
    state.sr = sr;
    v
}

/// Computes exact MC68000 execution cycles for `MULU.W <ea>, Dn` (excluding EA fetch cycles).
/// Base is 38 cycles + 2 cycles for each `1` bit in the 16-bit source operand.
#[inline]
pub const fn mulu_cycles(src: u16) -> u32 {
    38 + 2 * src.count_ones()
}

/// Computes exact MC68000 execution cycles for `MULS.W <ea>, Dn` (excluding EA fetch cycles).
/// Base is 38 cycles + 2 cycles for each `01` or `10` bit transition in `(src << 1)`.
#[inline]
pub const fn muls_cycles(src: u16) -> u32 {
    let s = (src as i16 as i32) << 1;
    let transitions = ((s ^ (s >> 1)) as u32) & 0xFFFF;
    38 + 2 * transitions.count_ones()
}
