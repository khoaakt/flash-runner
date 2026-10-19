//! Software arithmetic for the x87 80-bit extended format.
//!
//! Everything here works on explicit bit patterns (`F80`) with integer arithmetic only, so results are
//! bit-identical on aarch64, x86 and wasm32. Exceptions are recorded in `Fpu::sw` (masked-exception
//! behaviour only; no #MF is ever delivered).
//!
//! Value model: a finite non-zero operand is unpacked to `(sign, exp, sig)` with `sig` normalised (bit 63
//! set) and value = sig * 2^(exp-63), i.e. `exp` is the unbiased exponent of the leading bit. Denormal
//! and pseudo-denormal operands are normalised on unpack (and raise DE).

use super::{F80, Fpu};

pub const IE: u16 = 0x0001;
pub const DE: u16 = 0x0002;
pub const ZE: u16 = 0x0004;
pub const OE: u16 = 0x0008;
pub const UE: u16 = 0x0010;
pub const PE: u16 = 0x0020;
pub const SF: u16 = 0x0040;
pub const C0: u16 = 0x0100;
pub const C1: u16 = 0x0200;
pub const C2: u16 = 0x0400;
pub const C3: u16 = 0x4000;
pub const CC_ALL: u16 = C0 | C1 | C2 | C3;

/// "Real indefinite": the QNaN produced by masked invalid-operation exceptions.
pub const INDEF: F80 = F80 { mant: 0xC000_0000_0000_0000, se: 0xFFFF };
pub const QUIET: u64 = 0x4000_0000_0000_0000;

pub const EMIN80: i32 = -16382;
pub const EMAX80: i32 = 16383;

impl F80 {
    #[inline(always)]
    pub const fn zero(sign: bool) -> F80 {
        F80 { mant: 0, se: (sign as u16) << 15 }
    }
    #[inline(always)]
    pub const fn inf(sign: bool) -> F80 {
        F80 { mant: 1 << 63, se: 0x7FFF | ((sign as u16) << 15) }
    }
    #[inline(always)]
    pub const fn sign(self) -> bool {
        self.se & 0x8000 != 0
    }
    #[inline(always)]
    pub const fn neg(self) -> F80 {
        F80 { mant: self.mant, se: self.se ^ 0x8000 }
    }
    #[inline(always)]
    pub const fn abs(self) -> F80 {
        F80 { mant: self.mant, se: self.se & 0x7FFF }
    }
    /// Exact conversion of a sign and a 64-bit magnitude.
    #[inline]
    pub fn from_mag(sign: bool, mag: u64) -> F80 {
        if mag == 0 {
            return F80::zero(sign);
        }
        let lz = mag.leading_zeros();
        F80 { mant: mag << lz, se: (16383 + 63 - lz as u16) | ((sign as u16) << 15) }
    }
    pub fn from_i64(v: i64) -> F80 {
        F80::from_mag(v < 0, v.unsigned_abs())
    }
}

/// Classified operand. NaNs keep their raw encoding.
#[derive(Clone, Copy, Debug)]
pub enum V {
    Zero(bool),
    /// sign, unbiased exponent of the leading bit, normalised significand
    Fin(bool, i32, u64),
    Inf(bool),
    QNan(F80),
    SNan(F80),
    /// Unsupported encoding (unnormal, pseudo-NaN, pseudo-infinity): operations raise #IA.
    Bad,
}

/// Pure classification. The bool is true for denormal / pseudo-denormal encodings.
#[inline]
pub fn classify(f: F80) -> (V, bool) {
    let sign = f.se & 0x8000 != 0;
    let be = (f.se & 0x7FFF) as i32;
    let m = f.mant;
    if be == 0x7FFF {
        if m >> 63 == 0 {
            (V::Bad, false)
        } else if m << 1 == 0 {
            (V::Inf(sign), false)
        } else if m & QUIET != 0 {
            (V::QNan(f), false)
        } else {
            (V::SNan(f), false)
        }
    } else if be == 0 {
        if m == 0 {
            (V::Zero(sign), false)
        } else {
            let lz = m.leading_zeros();
            (V::Fin(sign, EMIN80 - lz as i32, m << lz), true)
        }
    } else if m >> 63 == 0 {
        (V::Bad, false)
    } else {
        (V::Fin(sign, be - 16383, m), false)
    }
}

#[inline]
pub fn shr_jam128(v: u128, d: u32) -> u128 {
    if d == 0 {
        v
    } else if d < 128 {
        (v >> d) | (((v << (128 - d)) != 0) as u128)
    } else {
        (v != 0) as u128
    }
}

/// Result of rounding a (sig:low) pair to `p` significand bits in a format with exponent range emin..=emax.
#[derive(Clone, Copy)]
pub struct Rounded {
    pub sig: u64,
    pub exp: i32,
    /// Result was tiny (below the normal range) before rounding.
    pub tiny: bool,
    pub over: bool,
    pub inexact: bool,
    /// Magnitude was incremented by the rounding (C1).
    pub inc: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Add,
    Sub,
    SubR,
    Mul,
    Div,
    DivR,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmp {
    Gt,
    Lt,
    Eq,
    Un,
}

fn isqrt128(n: u128) -> (u64, u128) {
    // Digit-by-digit square root: returns (floor(sqrt(n)), n - floor^2).
    let mut x = n;
    let mut c = 0u128;
    let mut d = 1u128 << 126;
    while d > x {
        d >>= 2;
    }
    while d != 0 {
        if x >= c + d {
            x -= c + d;
            c = (c >> 1) + d;
        } else {
            c >>= 1;
        }
        d >>= 2;
    }
    (c as u64, x)
}

impl Fpu {
    #[inline(always)]
    pub fn rc(&self) -> u8 {
        ((self.cw >> 10) & 3) as u8
    }
    /// Significand width selected by CW.PC (only FADD/FSUB/FMUL/FDIV/FSQRT honour it).
    #[inline(always)]
    pub fn pc_bits(&self) -> u32 {
        match (self.cw >> 8) & 3 {
            0 => 24,
            2 => 53,
            _ => 64,
        }
    }

    #[inline(always)]
    pub fn raise(&mut self, f: u16) {
        self.sw |= f;
    }

    /// Unpack, raising DE for denormal operands.
    #[inline(always)]
    pub fn unpack(&mut self, f: F80) -> V {
        let (v, den) = classify(f);
        if den {
            self.sw |= DE;
        }
        v
    }

    #[inline]
    pub fn invalid(&mut self) -> F80 {
        self.sw |= IE;
        INDEF
    }

    // ---- rounding ---------------------------------------------------------------------------
    /// Round the 128-bit value (sig:low) * 2^(exp-63), sig normalised, to `p` significand bits.
    pub fn round_core(&self, sign: bool, exp: i32, sig: u64, low: u64, p: u32, emin: i32, emax: i32) -> Rounded {
        let rc = self.rc();
        let (mut e, mut s, mut l) = (exp, sig, low);
        let mut tiny = false;
        if e < emin {
            // Tininess is detected after rounding (as QEMU / SoftFloat do for x86): the result is not
            // tiny if rounding to p bits with an unbounded exponent would carry up to 2^emin.
            tiny = !(e == emin - 1 && self.carries_to_next(sign, s, l, p));
            // Denormalise (shift right, collecting a sticky bit) and round at the same bit position.
            let v = shr_jam128(((s as u128) << 64) | l as u128, (emin as i64 - e as i64).min(1 << 20) as u32);
            s = (v >> 64) as u64;
            l = v as u64;
            e = emin;
        }
        let shift = 64 - p;
        let mask: u64 = if shift == 0 { 0 } else { (1u64 << shift) - 1 };
        let dropped: u128 = (((s & mask) as u128) << 64) | l as u128;
        let inexact = dropped != 0;
        let half: u128 = 1u128 << (63 + shift);
        let lsb = (s >> shift) & 1 != 0;
        let inc = match self_rc_mode(rc) {
            RM::Near => dropped > half || (dropped == half && lsb),
            RM::Down => sign && inexact,
            RM::Up => !sign && inexact,
            RM::Zero => false,
        };
        let mut sn = s & !mask;
        if inc {
            sn = sn.wrapping_add(1u64 << shift);
            if sn == 0 {
                sn = 1u64 << 63;
                e += 1;
            }
        }
        Rounded { sig: sn, exp: e, tiny, over: e > emax, inexact, inc }
    }

    /// Would rounding (sig:low) to p bits carry out of the significand (all ones + increment)?
    fn carries_to_next(&self, sign: bool, s: u64, l: u64, p: u32) -> bool {
        let shift = 64 - p;
        let mask: u64 = if shift == 0 { 0 } else { (1u64 << shift) - 1 };
        if (s | mask) != u64::MAX {
            return false;
        }
        let dropped: u128 = (((s & mask) as u128) << 64) | l as u128;
        let half: u128 = 1u128 << (63 + shift);
        match self_rc_mode(self.rc()) {
            RM::Near => dropped >= half, // all-ones kept bits are odd, so a tie rounds up
            RM::Down => sign && dropped != 0,
            RM::Up => !sign && dropped != 0,
            RM::Zero => false,
        }
    }

    #[inline]
    fn note_round(&mut self, r: &Rounded) {
        if r.inexact {
            self.sw |= PE;
            if r.inc {
                self.sw |= C1;
            }
        }
        if r.tiny && (r.inexact || self.cw & 0x10 == 0) {
            self.sw |= UE;
        }
    }

    /// True when a masked overflow produces infinity (rather than the largest finite number).
    #[inline]
    fn overflow_to_inf(&self, sign: bool) -> bool {
        match self_rc_mode(self.rc()) {
            RM::Near => true,
            RM::Down => sign,
            RM::Up => !sign,
            RM::Zero => false,
        }
    }

    /// Round and pack into the 80-bit format with `p` significand bits.
    pub fn round_pack(&mut self, sign: bool, exp: i32, sig: u64, low: u64, p: u32) -> F80 {
        let r = self.round_core(sign, exp, sig, low, p, EMIN80, EMAX80);
        self.note_round(&r);
        if r.over {
            self.sw |= OE | PE;
            return if self.overflow_to_inf(sign) {
                self.sw |= C1;
                F80::inf(sign)
            } else {
                F80 { mant: !0u64 << (64 - p), se: 0x7FFE | ((sign as u16) << 15) }
            };
        }
        let be = if r.sig >> 63 != 0 { (r.exp + 16383) as u16 } else { 0 };
        F80 { mant: r.sig, se: be | ((sign as u16) << 15) }
    }

    /// Round to double precision / exponent range and return the IEEE-754 binary64 bits.
    pub fn pack_f64(&mut self, sign: bool, exp: i32, sig: u64, low: u64) -> u64 {
        let r = self.round_core(sign, exp, sig, low, 53, -1022, 1023);
        self.note_round(&r);
        let sbit = (sign as u64) << 63;
        if r.over {
            self.sw |= OE | PE;
            return sbit | if self.overflow_to_inf(sign) { 0x7FF0_0000_0000_0000 } else { 0x7FEF_FFFF_FFFF_FFFF };
        }
        let fe = if r.tiny && r.sig >> 63 == 0 { 1 } else { (r.exp + 1023) as u64 };
        sbit | (((fe - 1) << 52) + (r.sig >> 11))
    }

    pub fn pack_f32(&mut self, sign: bool, exp: i32, sig: u64, low: u64) -> u32 {
        let r = self.round_core(sign, exp, sig, low, 24, -126, 127);
        self.note_round(&r);
        let sbit = (sign as u32) << 31;
        if r.over {
            self.sw |= OE | PE;
            return sbit | if self.overflow_to_inf(sign) { 0x7F80_0000 } else { 0x7F7F_FFFF };
        }
        let fe = if r.tiny && r.sig >> 63 == 0 { 1 } else { (r.exp + 127) as u64 };
        sbit | ((((fe - 1) << 23) + (r.sig >> 40)) as u32)
    }

    // ---- memory-format conversions ----------------------------------------------------------
    pub fn f32_to_f80(&mut self, b: u32) -> F80 {
        let sign = b >> 31 != 0;
        let be = (b >> 23) & 0xFF;
        let frac = (b & 0x7F_FFFF) as u64;
        let s = (sign as u16) << 15;
        if be == 0xFF {
            if frac == 0 {
                return F80::inf(sign);
            }
            if frac & (1 << 22) == 0 {
                self.sw |= IE;
            }
            return F80 { mant: (1 << 63) | QUIET | (frac << 40), se: 0x7FFF | s };
        }
        if be == 0 {
            if frac == 0 {
                return F80::zero(sign);
            }
            self.sw |= DE;
            let lz = frac.leading_zeros();
            // value = frac * 2^-149 = (frac<<lz) * 2^(-149 - lz)
            return F80 { mant: frac << lz, se: ((-149 + 63 - lz as i32 + 16383) as u16) | s };
        }
        F80 { mant: (1 << 63) | (frac << 40), se: ((be as i32 - 127 + 16383) as u16) | s }
    }

    pub fn f64_to_f80(&mut self, b: u64) -> F80 {
        let sign = b >> 63 != 0;
        let be = ((b >> 52) & 0x7FF) as i32;
        let frac = b & 0xF_FFFF_FFFF_FFFF;
        let s = (sign as u16) << 15;
        if be == 0x7FF {
            if frac == 0 {
                return F80::inf(sign);
            }
            if frac & (1 << 51) == 0 {
                self.sw |= IE;
            }
            return F80 { mant: (1 << 63) | QUIET | (frac << 11), se: 0x7FFF | s };
        }
        if be == 0 {
            if frac == 0 {
                return F80::zero(sign);
            }
            self.sw |= DE;
            let lz = frac.leading_zeros();
            return F80 { mant: frac << lz, se: ((-1074 + 63 - lz as i32 + 16383) as u16) | s };
        }
        F80 { mant: (1 << 63) | (frac << 11), se: ((be - 1023 + 16383) as u16) | s }
    }

    pub fn f80_to_f64(&mut self, f: F80) -> u64 {
        match classify(f).0 {
            V::Zero(s) => (s as u64) << 63,
            V::Inf(s) => ((s as u64) << 63) | 0x7FF0_0000_0000_0000,
            V::QNan(_) => ((f.sign() as u64) << 63) | 0x7FF0_0000_0000_0000 | ((f.mant >> 11) & 0xF_FFFF_FFFF_FFFF),
            V::SNan(_) => {
                self.sw |= IE;
                ((f.sign() as u64) << 63) | 0x7FF0_0000_0000_0000 | (1 << 51) | ((f.mant >> 11) & 0xF_FFFF_FFFF_FFFF)
            }
            V::Bad => {
                self.sw |= IE;
                0xFFF8_0000_0000_0000
            }
            V::Fin(s, e, m) => {
                if classify(f).1 {
                    self.sw |= DE; // denormal source (QEMU / hardware flag #D on narrowing stores)
                }
                self.pack_f64(s, e, m, 0)
            }
        }
    }

    pub fn f80_to_f32(&mut self, f: F80) -> u32 {
        match classify(f).0 {
            V::Zero(s) => (s as u32) << 31,
            V::Inf(s) => ((s as u32) << 31) | 0x7F80_0000,
            V::QNan(_) => ((f.sign() as u32) << 31) | 0x7F80_0000 | ((f.mant >> 40) as u32 & 0x7F_FFFF),
            V::SNan(_) => {
                self.sw |= IE;
                ((f.sign() as u32) << 31) | 0x7F80_0000 | (1 << 22) | ((f.mant >> 40) as u32 & 0x7F_FFFF)
            }
            V::Bad => {
                self.sw |= IE;
                0xFFC0_0000
            }
            V::Fin(s, e, m) => {
                if classify(f).1 {
                    self.sw |= DE;
                }
                self.pack_f32(s, e, m, 0)
            }
        }
    }

    // ---- integers ---------------------------------------------------------------------------
    /// Round |value| = sig * 2^(exp-63) (exp <= 63) to an integer magnitude. Returns
    /// (magnitude, inexact, rounded_up).
    pub fn int_round(&self, sign: bool, exp: i32, sig: u64, rc: u8) -> (u128, bool, bool) {
        let v = (sig as u128) << 64; // 64 fraction bits
        let s = 127 - exp; // >= 64
        let (ip, nz, cmp): (u128, bool, core::cmp::Ordering) = if s >= 129 {
            (0, true, core::cmp::Ordering::Less)
        } else if s == 128 {
            (0, true, v.cmp(&(1u128 << 127)))
        } else {
            let dropped = v & ((1u128 << s) - 1);
            (v >> s, dropped != 0, dropped.cmp(&(1u128 << (s - 1))))
        };
        use core::cmp::Ordering::*;
        let inc = match self_rc_mode(rc) {
            RM::Near => cmp == Greater || (cmp == Equal && ip & 1 == 1),
            RM::Down => sign && nz,
            RM::Up => !sign && nz,
            RM::Zero => false,
        };
        (ip + inc as u128, nz, inc && nz)
    }

    /// Convert to a signed integer of `bits` bits (16/32/64) with rounding mode `rc`.
    /// `None` = invalid (IE raised); the caller stores the integer indefinite.
    pub fn f80_to_int(&mut self, f: F80, rc: u8, bits: u32) -> Option<i64> {
        match classify(f).0 {
            V::Zero(_) => Some(0),
            V::Fin(s, e, m) => {
                if e >= 64 {
                    self.sw |= IE;
                    return None;
                }
                let (mag, nz, inc) = self.int_round(s, e, m, rc);
                let lim = 1u128 << (bits - 1);
                if if s { mag > lim } else { mag >= lim } {
                    self.sw |= IE;
                    return None;
                }
                if nz {
                    self.sw |= PE;
                    if inc {
                        self.sw |= C1;
                    }
                }
                Some(if s { (mag as i128).wrapping_neg() as i64 } else { mag as i64 })
            }
            _ => {
                self.sw |= IE;
                None
            }
        }
    }

    pub fn frndint(&mut self, f: F80) -> F80 {
        let rc = self.rc();
        match self.unpack(f) {
            V::Zero(_) | V::Inf(_) | V::QNan(_) => f,
            V::SNan(_) => {
                self.sw |= IE;
                F80 { mant: f.mant | QUIET, se: f.se }
            }
            V::Bad => self.invalid(),
            V::Fin(s, e, m) => {
                if e >= 63 {
                    return f;
                }
                let (mag, nz, inc) = self.int_round(s, e, m, rc);
                if nz {
                    self.sw |= PE;
                    if inc {
                        self.sw |= C1;
                    }
                }
                F80::from_mag(s, mag as u64)
            }
        }
    }

    /// Packed BCD (18 digits + sign) from 10 bytes.
    pub fn bcd_to_f80(b: &[u8; 10]) -> F80 {
        let mut v: u64 = 0;
        for i in (0..9).rev() {
            v = v * 100 + (b[i] >> 4) as u64 * 10 + (b[i] & 15) as u64;
        }
        F80::from_mag(b[9] & 0x80 != 0, v)
    }

    /// FBSTP conversion; invalid results give the BCD indefinite.
    pub fn f80_to_bcd(&mut self, f: F80) -> [u8; 10] {
        const BCD_INDEF: [u8; 10] = [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF];
        let rc = self.rc();
        let (sign, mag) = match classify(f).0 {
            V::Zero(s) => (s, 0u128),
            V::Fin(s, e, m) => {
                if e >= 64 {
                    self.sw |= IE;
                    return BCD_INDEF;
                }
                let (mag, nz, inc) = self.int_round(s, e, m, rc);
                if mag >= 1_000_000_000_000_000_000 {
                    self.sw |= IE;
                    return BCD_INDEF;
                }
                if nz {
                    self.sw |= PE;
                    if inc {
                        self.sw |= C1;
                    }
                }
                (s, mag)
            }
            _ => {
                self.sw |= IE;
                return BCD_INDEF;
            }
        };
        let mut out = [0u8; 10];
        let mut v = mag as u64;
        for byte in out.iter_mut().take(9) {
            let d = (v % 100) as u8;
            *byte = ((d / 10) << 4) | (d % 10);
            v /= 100;
        }
        out[9] = if sign { 0x80 } else { 0 };
        out
    }

    // ---- NaN handling -----------------------------------------------------------------------
    /// Result of a two-operand operation when at least one operand is a NaN or unsupported.
    pub(super) fn nan2(&mut self, a: V, b: V) -> F80 {
        if matches!(a, V::Bad) || matches!(b, V::Bad) {
            return self.invalid();
        }
        let nan_of = |v: V| match v {
            V::QNan(f) => Some((f, false)),
            V::SNan(f) => Some((f, true)),
            _ => None,
        };
        let (na, nb) = (nan_of(a), nan_of(b));
        let pick_b = match (na, nb) {
            (Some(_), None) => false,
            (None, Some(_)) => true,
            (Some((fa, sna)), Some((fb, snb))) => {
                // SNaN vs QNaN: the QNaN wins; same class: larger significand, ties to the smaller
                // sign/exponent field (Intel SDM table 4-7, matches QEMU's pickNaN for x86).
                let a_larger = if fa.mant != fb.mant { fa.mant > fb.mant } else { fa.se < fb.se };
                match (sna, snb) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => !a_larger,
                }
            }
            (None, None) => false,
        };
        if matches!(a, V::SNan(_)) || matches!(b, V::SNan(_)) {
            self.sw |= IE;
        }
        let (f, _) = if pick_b { nb.unwrap() } else { na.unwrap() };
        F80 { mant: f.mant | QUIET, se: f.se }
    }

    /// Single-operand NaN/unsupported handling.
    pub fn nan1(&mut self, v: V) -> Option<F80> {
        match v {
            V::QNan(f) => Some(f),
            V::SNan(f) => {
                self.sw |= IE;
                Some(F80 { mant: f.mant | QUIET, se: f.se })
            }
            V::Bad => Some(self.invalid()),
            _ => None,
        }
    }

    // ---- arithmetic -------------------------------------------------------------------------
    /// a OP b rounded to `p` significand bits (Op::SubR = b - a, Op::DivR = b / a).
    pub fn arith(&mut self, op: Op, a: F80, b: F80, p: u32) -> F80 {
        let (a, b) = if matches!(op, Op::SubR | Op::DivR) { (b, a) } else { (a, b) };
        let (va, da) = classify(a);
        let (vb, db) = classify(b);
        if matches!(va, V::QNan(_) | V::SNan(_) | V::Bad) || matches!(vb, V::QNan(_) | V::SNan(_) | V::Bad) {
            return self.nan2(va, vb); // NaN operands take priority over the denormal-operand flag
        }
        // The denormal-operand flag is dropped when the operation raises invalid or zero-divide.
        let saved = self.sw & 0x7F;
        self.sw &= !0x7F;
        let r = match op {
            Op::Add => self.add_vals(va, vb, false, p),
            Op::Sub | Op::SubR => self.add_vals(va, vb, true, p),
            Op::Mul => self.mul_vals(va, vb, p),
            Op::Div | Op::DivR => self.div_vals(va, vb, p),
        };
        if (da || db) && self.sw & (IE | ZE) == 0 {
            self.sw |= DE;
        }
        self.sw |= saved;
        r
    }

    /// Run `f`; the denormal-operand flag is dropped if the operation raised invalid or zero-divide
    /// (those take priority over #D, as in QEMU / the SDM exception priority table).
    pub fn de_filtered<R>(&mut self, f: impl FnOnce(&mut Fpu) -> R) -> R {
        let saved = self.sw & 0x7F;
        self.sw &= !0x7F;
        let r = f(self);
        if self.sw & (IE | ZE) != 0 {
            self.sw &= !DE;
        }
        self.sw |= saved;
        r
    }

    fn add_vals(&mut self, a: V, b: V, sub: bool, p: u32) -> F80 {
        use V::*;
        match (a, b) {
            (Fin(sa, ea, ma), Fin(sb0, eb, mb)) => self.add_fin(sa, ea, ma, sb0 ^ sub, eb, mb, p),
            (Inf(sa), Inf(sb0)) => {
                if sa == (sb0 ^ sub) {
                    F80::inf(sa)
                } else {
                    self.invalid()
                }
            }
            (Inf(s), _) => F80::inf(s),
            (_, Inf(s)) => F80::inf(s ^ sub),
            (Zero(sa), Zero(sb0)) => F80::zero(if sa == (sb0 ^ sub) { sa } else { self.rc() == 1 }),
            (Zero(_), Fin(sb0, e, m)) => self.round_pack(sb0 ^ sub, e, m, 0, p),
            (Fin(s, e, m), Zero(_)) => self.round_pack(s, e, m, 0, p),
            _ => INDEF,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_fin(&mut self, sa: bool, ea: i32, ma: u64, sb: bool, eb: i32, mb: u64, p: u32) -> F80 {
        // Order by magnitude so that the larger operand comes first.
        let swap = if ea != eb { ea < eb } else { ma < mb };
        let (sa, ea, ma, sb, eb, mb) = if swap { (sb, eb, mb, sa, ea, ma) } else { (sa, ea, ma, sb, eb, mb) };
        let d = (ea as i64 - eb as i64).min(1 << 20) as u32;
        // 63 guard bits below the significand; bit 126 is the leading bit, bit 127 is carry headroom.
        let big = (ma as u128) << 63;
        let small = shr_jam128((mb as u128) << 63, d);
        let sum = if sa == sb {
            big + small
        } else {
            let diff = big - small;
            if diff == 0 {
                return F80::zero(self.rc() == 1);
            }
            diff
        };
        let lz = sum.leading_zeros();
        let sum = sum << lz;
        self.round_pack(sa, ea + 1 - lz as i32, (sum >> 64) as u64, sum as u64, p)
    }

    fn mul_vals(&mut self, a: V, b: V, p: u32) -> F80 {
        use V::*;
        match (a, b) {
            (Fin(sa, ea, ma), Fin(sb, eb, mb)) => {
                let prod = (ma as u128) * (mb as u128);
                let (prod, e) = if prod >> 127 != 0 { (prod, ea + eb + 1) } else { (prod << 1, ea + eb) };
                self.round_pack(sa ^ sb, e, (prod >> 64) as u64, prod as u64, p)
            }
            (Inf(_), Zero(_)) | (Zero(_), Inf(_)) => self.invalid(),
            (Inf(sa), Inf(sb)) => F80::inf(sa ^ sb),
            (Inf(sa), Fin(sb, ..)) | (Fin(sa, ..), Inf(sb)) => F80::inf(sa ^ sb),
            (Zero(sa), Zero(sb)) | (Zero(sa), Fin(sb, ..)) | (Fin(sa, ..), Zero(sb)) => F80::zero(sa ^ sb),
            _ => INDEF,
        }
    }

    fn div_vals(&mut self, a: V, b: V, p: u32) -> F80 {
        use V::*;
        match (a, b) {
            (Fin(sa, ea, ma), Fin(sb, eb, mb)) => {
                // Quotient with 64 fraction bits: q in [2^64, 2^65) and a remainder.
                let num = (ma as u128) << 64;
                let mut q = num / mb as u128;
                let mut rem = num - q * mb as u128;
                let mut e = ea - eb;
                if ma < mb {
                    // q has only 64 bits: develop one more quotient bit.
                    rem <<= 1;
                    q <<= 1;
                    if rem >= mb as u128 {
                        rem -= mb as u128;
                        q |= 1;
                    }
                    e -= 1;
                }
                let sig = (q >> 1) as u64;
                let low = (((q & 1) as u64) << 63) | (rem != 0) as u64;
                self.round_pack(sa ^ sb, e, sig, low, p)
            }
            (Zero(_), Zero(_)) | (Inf(_), Inf(_)) => self.invalid(),
            (Inf(sa), Fin(sb, ..)) | (Inf(sa), Zero(sb)) => F80::inf(sa ^ sb),
            (Fin(sa, ..), Zero(sb)) => {
                self.sw |= ZE;
                F80::inf(sa ^ sb)
            }
            (Zero(sa), Fin(sb, ..)) | (Zero(sa), Inf(sb)) | (Fin(sa, ..), Inf(sb)) => F80::zero(sa ^ sb),
            _ => INDEF,
        }
    }

    pub fn fsqrt(&mut self, a: F80, p: u32) -> F80 {
        self.de_filtered(|f| f.fsqrt_inner(a, p))
    }

    fn fsqrt_inner(&mut self, a: F80, p: u32) -> F80 {
        match self.unpack(a) {
            V::Zero(_) => a,
            V::Inf(false) => a,
            V::Inf(true) => self.invalid(),
            V::Fin(true, ..) => self.invalid(),
            V::Fin(false, e, m) => {
                // value = m * 2^(e-63). Pick n = m << (64 or 63) so the exponent below is even.
                let (n, re) = if e & 1 != 0 { ((m as u128) << 64, (e - 1) >> 1) } else { ((m as u128) << 63, e >> 1) };
                let (r, rem) = isqrt128(n);
                // Round bit: sqrt(n) >= r + 1/2  <=>  rem > r (exact ties are impossible).
                let low = if rem == 0 {
                    0
                } else if rem > r as u128 {
                    (1u64 << 63) | 1
                } else {
                    1
                };
                self.round_pack(false, re, r, low, p)
            }
            v => self.nan1(v).unwrap_or(INDEF),
        }
    }

    /// FSCALE: a * 2^trunc(b).
    pub fn fscale(&mut self, a: F80, b: F80) -> F80 {
        let va = self.unpack(a);
        let vb = self.unpack(b);
        if matches!(va, V::QNan(_) | V::SNan(_) | V::Bad) || matches!(vb, V::QNan(_) | V::SNan(_) | V::Bad) {
            return self.nan2(va, vb);
        }
        use V::*;
        match (va, vb) {
            (Zero(_), Inf(false)) | (Inf(_), Inf(true)) => self.invalid(),
            (Zero(_), _) | (Inf(_), _) => a,
            (Fin(s, ..), Inf(false)) => F80::inf(s),
            (Fin(s, ..), Inf(true)) => F80::zero(s),
            (Fin(s, e, m), Zero(_)) => self.round_pack(s, e, m, 0, 64),
            (Fin(s, e, m), Fin(sb, eb, mb)) => {
                let n: i32 = if eb < 0 {
                    0
                } else if eb >= 20 {
                    if sb { -(1 << 20) } else { 1 << 20 }
                } else {
                    let mag = (mb >> (63 - eb)) as i32;
                    if sb { -mag } else { mag }
                };
                self.round_pack(s, e + n, m, 0, 64)
            }
            _ => INDEF,
        }
    }

    /// FXTRACT: returns (exponent, significand).
    pub fn fxtract(&mut self, a: F80) -> (F80, F80) {
        match self.unpack(a) {
            V::Fin(s, e, m) => (F80::from_i64(e as i64), F80 { mant: m, se: 0x3FFF | ((s as u16) << 15) }),
            V::Zero(s) => {
                self.sw |= ZE;
                (F80::inf(true), F80::zero(s))
            }
            V::Inf(s) => (F80::inf(false), F80::inf(s)),
            v => {
                let n = self.nan1(v).unwrap_or(INDEF);
                (n, n)
            }
        }
    }

    /// FPREM / FPREM1 (completed in one step: C2 is always cleared). Returns the remainder and sets
    /// C0/C3/C1 to the low three quotient bits Q2/Q1/Q0.
    pub fn fprem(&mut self, a: F80, b: F80, nearest: bool) -> F80 {
        let (va, da) = classify(a);
        let (vb, db) = classify(b);
        if matches!(va, V::QNan(_) | V::SNan(_) | V::Bad) || matches!(vb, V::QNan(_) | V::SNan(_) | V::Bad) {
            return self.nan2(va, vb);
        }
        self.sw &= !CC_ALL;
        if (da || db) && !matches!(va, V::Inf(_)) && !matches!(vb, V::Zero(_)) {
            self.sw |= DE; // invalid operations (inf % x, x % 0) take priority over #D
        }
        use V::*;
        let (sa, ea, ma, eb, mb) = match (va, vb) {
            (Inf(_), _) | (_, Zero(_)) => return self.invalid(),
            (Zero(_), _) => return a,
            (Fin(sa, ea, ma), Inf(_)) => return self.prem_pack(sa, ma, ea),
            (Fin(sa, ea, ma), Fin(_, eb, mb)) => (sa, ea, ma, eb, mb),
            _ => return INDEF,
        };
        let d = ea as i64 - eb as i64;
        let mut q: u64 = 0;
        let mut neg = false; // remainder has the opposite sign of the dividend
        if d < 0 {
            // |a| < |b|: quotient 0, except FPREM1 rounds |a|/|b| > 1/2 up to 1.
            // With ea = eb-1: |a| = ma * 2^(eb-64), |b| = 2*mb * 2^(eb-64); |a| > |b|/2 <=> ma > mb.
            if nearest && d == -1 && ma > mb {
                let mag = (2u128 * mb as u128 - ma as u128) as u64; // |b| - |a|, in units of 2^(eb-64)
                self.set_q(1);
                return self.prem_pack(!sa, mag, eb - 1);
            }
            self.set_q(0);
            return self.prem_pack(sa, ma, ea); // |a| < |b|: unchanged (in canonical encoding)
        }
        // d >= 0: long division of ma * 2^d by mb, 63 bits at a time.
        let mut r = ma;
        if r >= mb {
            r -= mb;
            q = 1;
        }
        let mut rem_shift = d;
        while rem_shift > 0 {
            let k = rem_shift.min(63) as u32;
            let ext = (r as u128) << k;
            let qk = ext / mb as u128;
            r = (ext - qk * mb as u128) as u64;
            q = (q << k).wrapping_add(qk as u64);
            rem_shift -= k as i64;
        }
        if nearest {
            // Round the quotient to nearest even: compare 2r with mb.
            let twice = (r as u128) << 1;
            if twice > mb as u128 || (twice == mb as u128 && q & 1 == 1) {
                q = q.wrapping_add(1);
                r = mb - r;
                neg = true;
            }
        }
        self.set_q(q);
        if r == 0 {
            return F80::zero(sa);
        }
        self.prem_pack(sa ^ neg, r, eb)
    }

    fn set_q(&mut self, q: u64) {
        if q & 4 != 0 {
            self.sw |= C0;
        }
        if q & 2 != 0 {
            self.sw |= C3;
        }
        if q & 1 != 0 {
            self.sw |= C1;
        }
    }

    /// Pack mag * 2^(eb-63) exactly (the remainder never needs rounding).
    fn prem_pack(&mut self, sign: bool, mag: u64, eb: i32) -> F80 {
        if mag == 0 {
            return F80::zero(sign);
        }
        let lz = mag.leading_zeros();
        self.round_pack(sign, eb - lz as i32, mag << lz, 0, 64)
    }

    // ---- comparison -------------------------------------------------------------------------
    /// Compare a with b. `signal_qnan`: FCOM/FCOMI raise IE for QNaN operands, FUCOM/FUCOMI do not.
    pub fn fcmp(&mut self, a: F80, b: F80, signal_qnan: bool) -> Cmp {
        let (va, da) = classify(a);
        let (vb, db) = classify(b);
        let bad = |v: V| matches!(v, V::Bad | V::SNan(_));
        if bad(va) || bad(vb) {
            self.sw |= IE;
            return Cmp::Un;
        }
        if matches!(va, V::QNan(_)) || matches!(vb, V::QNan(_)) {
            if signal_qnan {
                self.sw |= IE;
            }
            return Cmp::Un;
        }
        if da || db {
            self.sw |= DE;
        }
        fn key(v: V) -> (bool, u8, i32, u64) {
            match v {
                V::Zero(_) => (false, 0, 0, 0),
                V::Fin(s, e, m) => (s, 1, e, m),
                V::Inf(s) => (s, 2, 0, 0),
                _ => (false, 0, 0, 0),
            }
        }
        let (sa, ca, ea, ma) = key(va);
        let (sb, cb, eb, mb) = key(vb);
        if ca == 0 && cb == 0 {
            return Cmp::Eq;
        }
        if sa != sb {
            return if sa { Cmp::Lt } else { Cmp::Gt };
        }
        let mag = (ca, ea, ma).cmp(&(cb, eb, mb));
        let mag = if sa { mag.reverse() } else { mag };
        match mag {
            core::cmp::Ordering::Greater => Cmp::Gt,
            core::cmp::Ordering::Less => Cmp::Lt,
            core::cmp::Ordering::Equal => Cmp::Eq,
        }
    }

    /// Condition-code bits (C3,C2,C1,C0) produced by FXAM for register contents `f`.
    pub fn fxam_bits(f: F80, empty: bool) -> u16 {
        let c1 = if f.sign() { C1 } else { 0 };
        if empty {
            return c1 | C3 | C0;
        }
        let (v, den) = classify(f);
        c1 | match v {
            V::Bad => 0,
            V::QNan(_) | V::SNan(_) => C0,
            V::Inf(_) => C2 | C0,
            V::Zero(_) => C3,
            V::Fin(..) => {
                if den {
                    C3 | C2
                } else {
                    C2
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RM {
    Near,
    Down,
    Up,
    Zero,
}

#[inline(always)]
fn self_rc_mode(rc: u8) -> RM {
    match rc & 3 {
        0 => RM::Near,
        1 => RM::Down,
        2 => RM::Up,
        _ => RM::Zero,
    }
}
