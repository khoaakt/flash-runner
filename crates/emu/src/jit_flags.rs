//! Lazy EFLAGS representation shared by the JIT translator, the Rust runtime and the interpreter hook.
//!
//! Compiled code records the last flag-producing instruction as a tuple `(lk, a, b, r)` (kind+size, the
//! two operands, the result; all masked to the operand size) instead of computing the six arithmetic
//! flags. `eval` turns a tuple into the exact bits the interpreter would have produced (including the
//! interpreter's choices for architecturally undefined flags). `lk == 0` means "flags are in `Cpu::fl`".

use crate::cpu::{AF, CF, OF, PF, SF, ZF};

pub const LK_NONE: u32 = 0;
pub const OP_ADD: u32 = 1;
pub const OP_ADC: u32 = 2;
pub const OP_SUB: u32 = 3;
pub const OP_SBB: u32 = 4;
pub const OP_LOGIC: u32 = 5;
pub const OP_INC: u32 = 6;
pub const OP_DEC: u32 = 7;
/// a = original value, b = count (1..=31), r = result.
pub const OP_SHL: u32 = 8;
pub const OP_SHR: u32 = 9;
pub const OP_SAR: u32 = 10;
/// `r` holds the six arithmetic flag bits directly (positions as in EFLAGS).
pub const OP_FULL: u32 = 11;
/// 2/3-operand imul: a = overflow (0/1), b = preserved AF bit (0 or 0x10), r = result.
pub const OP_IMUL: u32 = 12;

/// Kind word: operation in the high bits, log2(operand size in bytes) in the low two.
#[inline(always)]
pub const fn lk(op: u32, sz: u8) -> u32 {
    (op << 2) | (sz.trailing_zeros())
}
#[inline(always)]
pub const fn lk_op(lk: u32) -> u32 {
    lk >> 2
}
#[inline(always)]
pub const fn lk_size(lk: u32) -> u8 {
    1u8 << (lk & 3)
}

#[inline(always)]
fn mask(sz: u8) -> u32 {
    match sz {
        1 => 0xff,
        2 => 0xffff,
        _ => 0xffff_ffff,
    }
}
#[inline(always)]
fn sign(sz: u8) -> u32 {
    match sz {
        1 => 0x80,
        2 => 0x8000,
        _ => 0x8000_0000,
    }
}

#[inline(always)]
fn szp(r: u32, sz: u8) -> u32 {
    let r = r & mask(sz);
    let mut f = 0;
    if r == 0 {
        f |= ZF;
    }
    if r & sign(sz) != 0 {
        f |= SF;
    }
    if (r as u8).count_ones() & 1 == 0 {
        f |= PF;
    }
    f
}

/// Evaluate a lazy tuple to the six arithmetic flag bits. `a`, `b`, `r` must be masked to the operand size.
pub fn eval(lk: u32, a: u32, b: u32, r: u32) -> u32 {
    let sz = lk_size(lk);
    let m = mask(sz);
    let sb = sign(sz);
    let bits = sz as u32 * 8;
    match lk_op(lk) {
        OP_ADD | OP_ADC => {
            let c = if lk_op(lk) == OP_ADC { r.wrapping_sub(a).wrapping_sub(b) & m } else { 0 };
            let wide = a as u64 + b as u64 + c as u64;
            let mut f = szp(r, sz);
            if wide > m as u64 {
                f |= CF;
            }
            if (a ^ b ^ r) & 0x10 != 0 {
                f |= AF;
            }
            if (a ^ r) & (b ^ r) & sb != 0 {
                f |= OF;
            }
            f
        }
        OP_SUB | OP_SBB => {
            let c = if lk_op(lk) == OP_SBB { a.wrapping_sub(b).wrapping_sub(r) & m } else { 0 };
            let mut f = szp(r, sz);
            if (a as u64) < b as u64 + c as u64 {
                f |= CF;
            }
            if (a ^ b ^ r) & 0x10 != 0 {
                f |= AF;
            }
            if (a ^ b) & (a ^ r) & sb != 0 {
                f |= OF;
            }
            f
        }
        OP_LOGIC => szp(r, sz),
        // For INC/DEC the tuple is (old value, old CF, result): b holds the preserved carry (0/1).
        OP_INC => {
            let mut f = szp(r, sz);
            if b & 1 != 0 {
                f |= CF;
            }
            if (a ^ 1 ^ r) & 0x10 != 0 {
                f |= AF;
            }
            if (a ^ r) & (1 ^ r) & sb != 0 {
                f |= OF;
            }
            f
        }
        OP_DEC => {
            let mut f = szp(r, sz);
            if b & 1 != 0 {
                f |= CF;
            }
            if (a ^ 1 ^ r) & 0x10 != 0 {
                f |= AF;
            }
            if (a ^ 1) & (a ^ r) & sb != 0 {
                f |= OF;
            }
            f
        }
        OP_SHL => {
            let cnt = b;
            let cf = (((a as u64) << (cnt - 1)) >> (bits - 1)) & 1 != 0;
            let mut f = szp(r, sz);
            if cf {
                f |= CF;
            }
            if ((r & sb) != 0) != cf {
                f |= OF;
            }
            f
        }
        OP_SHR => {
            let cnt = b;
            let mut f = szp(r, sz);
            if ((a as u64) >> (cnt - 1)) & 1 != 0 {
                f |= CF;
            }
            if a & sb != 0 {
                f |= OF;
            }
            f
        }
        OP_SAR => {
            let cnt = b;
            let s = match sz {
                1 => a as u8 as i8 as i64,
                2 => a as u16 as i16 as i64,
                _ => a as i32 as i64,
            };
            let mut f = szp(r, sz);
            if (s >> (cnt - 1)) & 1 != 0 {
                f |= CF;
            }
            f
        }
        OP_FULL => r & (CF | PF | AF | ZF | SF | OF),
        OP_IMUL => {
            let mut f = szp(r, sz) | (b & AF);
            if a & 1 != 0 {
                f |= CF | OF;
            }
            f
        }
        _ => 0,
    }
}
