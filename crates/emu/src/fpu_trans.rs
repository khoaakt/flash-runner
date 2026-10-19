//! Transcendental x87 instructions (F2XM1, FYL2X, FYL2XP1, FPATAN, FSIN, FCOS, FSINCOS, FPTAN).
//!
//! The functions are evaluated in a small software float `W` with a 128-bit significand and an i32
//! exponent (no overflow/underflow inside the evaluation), using integer arithmetic only, and are then
//! rounded once to the 64-bit significand with the current rounding mode. Series are summed to ~2^-128
//! relative accuracy, so results are within a fraction of an ulp of the true value (real hardware is
//! only guaranteed to <1 ulp as well, and its FSIN/FCOS/FPTAN use a 66-bit pi, which we deliberately
//! do not imitate: the reduction here uses 384 bits of 2/pi and is exact for every |x| < 2^63).
//!
//! All the constants below were generated with exact integer arithmetic (Machin pi, atanh/Euler
//! series) and truncated to 128 bits (2/pi: 384 bits).

use super::soft::*;
use super::{F80, Fpu};

/// 128-bit-significand float: value = (-1)^neg * m * 2^(exp-127), m normalised (bit 127 set) or zero.
#[derive(Clone, Copy)]
struct W {
    neg: bool,
    exp: i32,
    m: u128,
}

const ZERO: W = W { neg: false, exp: 0, m: 0 };
const ONE: W = W { neg: false, exp: 0, m: 1 << 127 };
const M64: u128 = u64::MAX as u128;

const PI: W = W { neg: false, exp: 1, m: 0xC90FDAA22168C234C4C6628B80DC1CD1 };
const PI2: W = W { neg: false, exp: 0, m: 0xC90FDAA22168C234C4C6628B80DC1CD1 };
const PI4: W = W { neg: false, exp: -1, m: 0xC90FDAA22168C234C4C6628B80DC1CD1 };
const PI34: W = W { neg: false, exp: 1, m: 0x96CBE3F9990E91A79394C9E8A0A5159C };
const LN2: W = W { neg: false, exp: -1, m: 0xB17217F7D1CF79ABC9E3B39803F2F6AF };
const LOG2E: W = W { neg: false, exp: 0, m: 0xB8AA3B295C17F0BBBE87FED0691D3E88 };
const LOG2_10: W = W { neg: false, exp: 1, m: 0xD49A784BCD1B8AFE492BF6FF4DAFDB4C };
const LOG10_2: W = W { neg: false, exp: -2, m: 0x9A209A84FBCFF7988F8959AC0B7C9178 };
/// Significand of sqrt(2) (value 1.m), used to centre the argument of log.
const SQRT2_M: u128 = 0xB504F333F9DE6484597D89B3754ABE9F;

/// atan(k/16), k = 1..=15 (index k-1); atan(1) = pi/4.
const ATAN16: [(u128, i32); 15] = [
    (0xFFAADDB967EF4E36CB2792DC0E2E0D51, -5),
    (0xFEADD4D5617B6E32C897989F3E888EF7, -4),
    (0xBDCBDA5E72D811347B0B4F881C9C7487, -3),
    (0xFADBAFC96406EB156DC79EF5F7A217E5, -3),
    (0x9B13B9B83F5E5E69C5ABB498D27AF328, -2),
    (0xB7B0CA0F26F784738AA32122DCFE4483, -2),
    (0xD327761E611FE5B6427C95E9001E7136, -2),
    (0xED63382B0DDA7B456FE445ECBC3A8D03, -2),
    (0x832BF4A6D9867E2A4B6A09CB61A515C0, -1),
    (0x8F005D5EF7F59F9B5C835E1665C43747, -1),
    (0x9A2F80E671BDDA204226F8E2204FF3BC, -1),
    (0xA4BC7D1934F7092419A87F2A457DAC9E, -1),
    (0xAEAC4C38B4D8C08014725E2F3E52070A, -1),
    (0xB8053E2BC2319E73CB2DA55210A4443D, -1),
    (0xC0CE85B8AC52664089DD62C46E92FA24, -1),
];

/// 2/pi as a 384-bit binary fraction, most significant limb first.
const TWO_OVER_PI: [u64; 6] = [
    0xA2F9836E4E441529,
    0xFC2757D1F534DDC0,
    0xDB6295993C439041,
    0xFE5163ABDEBBC561,
    0xB7246E3A424DD2E0,
    0x06492EEA09D1921C,
];

impl W {
    fn from_parts(neg: bool, exp: i32, sig: u64) -> W {
        W { neg, exp, m: (sig as u128) << 64 }
    }
    fn from_u64(n: u64) -> W {
        if n == 0 {
            return ZERO;
        }
        let lz = n.leading_zeros();
        W { neg: false, exp: 63 - lz as i32, m: (n as u128) << (64 + lz) }
    }
    fn negated(self) -> W {
        W { neg: !self.neg, ..self }
    }
    fn scale(self, n: i32) -> W {
        if self.m == 0 {
            self
        } else {
            W { exp: self.exp + n, ..self }
        }
    }
    fn is_zero(&self) -> bool {
        self.m == 0
    }
}

fn wmul(a: W, b: W) -> W {
    if a.m == 0 || b.m == 0 {
        return ZERO;
    }
    let (ah, al) = (a.m >> 64, a.m & M64);
    let (bh, bl) = (b.m >> 64, b.m & M64);
    let hh = ah * bh;
    let hl = ah * bl;
    let lh = al * bh;
    let ll = al * bl;
    let mid = (ll >> 64) + (hl & M64) + (lh & M64);
    let hi = hh + (hl >> 64) + (lh >> 64) + (mid >> 64);
    let lo = ((mid & M64) << 64) | (ll & M64);
    let neg = a.neg != b.neg;
    if hi >> 127 != 0 {
        W { neg, exp: a.exp + b.exp + 1, m: hi }
    } else {
        W { neg, exp: a.exp + b.exp, m: (hi << 1) | (lo >> 127) }
    }
}

fn wadd(a: W, b: W) -> W {
    if a.m == 0 {
        return b;
    }
    if b.m == 0 {
        return a;
    }
    let (a, b) = if (a.exp, a.m) >= (b.exp, b.m) { (a, b) } else { (b, a) };
    let d = (a.exp as i64 - b.exp as i64).min(1 << 20) as u32;
    let bm = shr_jam128(b.m, d);
    if a.neg == b.neg {
        let (s, carry) = a.m.overflowing_add(bm);
        if carry {
            W { neg: a.neg, exp: a.exp + 1, m: (s >> 1) | (1 << 127) | (s & 1) }
        } else {
            W { neg: a.neg, exp: a.exp, m: s }
        }
    } else {
        let diff = a.m - bm;
        if diff == 0 {
            return ZERO;
        }
        let lz = diff.leading_zeros();
        W { neg: a.neg, exp: a.exp - lz as i32, m: diff << lz }
    }
}

fn wsub(a: W, b: W) -> W {
    wadd(a, b.negated())
}

/// a / b by restoring division (b != 0).
fn wdiv(a: W, b: W) -> W {
    if a.m == 0 {
        return ZERO;
    }
    let bm = b.m;
    let mut r = a.m;
    let first = r >= bm;
    if first {
        r -= bm;
    }
    let nbits = if first { 127 } else { 128 };
    let mut q = first as u128;
    for _ in 0..nbits {
        let top = r >> 127;
        r <<= 1;
        q <<= 1;
        if top == 1 || r >= bm {
            r = r.wrapping_sub(bm);
            q |= 1;
        }
    }
    if r != 0 {
        q |= 1; // sticky
    }
    W { neg: a.neg != b.neg, exp: a.exp - b.exp - (!first) as i32, m: q }
}

/// a / n for a small integer n (< 2^40).
fn wdiv_small(a: W, n: u64) -> W {
    if a.m == 0 {
        return a;
    }
    let n = n as u128;
    let q1 = a.m / n;
    let r = a.m % n;
    let lz = q1.leading_zeros();
    W { neg: a.neg, exp: a.exp - lz as i32, m: (q1 << lz) | ((r << lz) / n) }
}

/// Round |a| (< 2^63) to the nearest integer (ties up).
fn w_round_u64(a: W) -> u64 {
    if a.m == 0 || a.exp < -1 {
        0
    } else if a.exp == -1 {
        1
    } else {
        let sh = 127 - a.exp as u32;
        ((a.m >> sh) as u64) + ((a.m >> (sh - 1)) & 1) as u64
    }
}

#[inline]
fn series_done(term: W, sum: W) -> bool {
    term.m == 0 || (sum.m != 0 && term.exp < sum.exp - 135)
}

/// 2*atanh(t) = ln((1+t)/(1-t)) for small |t|.
fn ln_atanh(t: W) -> W {
    if t.m == 0 {
        return ZERO;
    }
    let t2 = wmul(t, t);
    let mut term = t;
    let mut sum = t;
    let mut k = 1u64;
    loop {
        term = wmul(term, t2);
        k += 2;
        let d = wdiv_small(term, k);
        sum = wadd(sum, d);
        if series_done(d, sum) {
            break;
        }
    }
    sum.scale(1)
}

/// log2 of a positive W.
fn log2_w(x: W) -> W {
    let mut e = x.exp;
    let mut mm = W { neg: false, exp: 0, m: x.m };
    if x.m > SQRT2_M {
        mm.exp = -1;
        e += 1;
    }
    let t = wdiv(wsub(mm, ONE), wadd(mm, ONE));
    let ln = ln_atanh(t);
    let frac = wmul(ln, LOG2E);
    if e == 0 {
        frac
    } else {
        let ew = W::from_u64(e.unsigned_abs() as u64);
        wadd(if e < 0 { ew.negated() } else { ew }, frac)
    }
}

/// 2^x - 1 for |x| < 2^15.
fn exp2m1(x: W) -> W {
    // x = n + f with |f| <= 1/2 when |x| >= 1; for |x| < 1 use f = x directly.
    let (n, f) = if x.exp < 0 {
        (0i64, x)
    } else {
        let n = w_round_u64(x) as i64;
        let nw = W::from_u64(n as u64);
        let nw = if x.neg { nw.negated() } else { nw };
        (if x.neg { -n } else { n }, wsub(x, nw))
    };
    // expm1(y), y = f ln2, via z = y/2^8, Taylor, then 8 doublings expm1(2a) = expm1(a)(expm1(a)+2).
    let y = wmul(f, LN2);
    let mut p = ZERO;
    if !y.is_zero() {
        let z = y.scale(-8);
        let mut term = z;
        let mut sum = z;
        let mut k = 1u64;
        loop {
            k += 1;
            term = wdiv_small(wmul(term, z), k);
            sum = wadd(sum, term);
            if series_done(term, sum) {
                break;
            }
        }
        p = sum;
        let two = W { neg: false, exp: 1, m: 1 << 127 };
        for _ in 0..8 {
            p = wmul(p, wadd(p, two));
        }
    }
    if n == 0 {
        p
    } else {
        wsub(wadd(p, ONE).scale(n as i32), ONE)
    }
}

/// atan(r) for 0 <= r <= 1.
fn atan_unit(r: W) -> W {
    let k = w_round_u64(r.scale(4));
    let (base, u) = if k == 0 {
        (ZERO, r)
    } else {
        let c = W::from_u64(k).scale(-4);
        let base = if k == 16 {
            PI4
        } else {
            let (m, e) = ATAN16[k as usize - 1];
            W { neg: false, exp: e, m }
        };
        (base, wdiv(wsub(r, c), wadd(ONE, wmul(r, c))))
    };
    if u.is_zero() {
        return base;
    }
    let u2 = wmul(u, u);
    let mut term = u;
    let mut sum = u;
    let mut k = 1u64;
    loop {
        term = wmul(term, u2).negated();
        k += 2;
        let d = wdiv_small(term, k);
        sum = wadd(sum, d);
        if series_done(d, sum) {
            break;
        }
    }
    wadd(base, sum)
}

/// sin(r) and cos(r) for |r| <= ~0.8.
fn sincos_kernel(r: W, want_sin: bool, want_cos: bool) -> (W, W) {
    let r2 = wmul(r, r);
    let mut s = ZERO;
    let mut c = ZERO;
    if want_sin {
        let mut term = r;
        s = r;
        let mut k = 1u64;
        while !r.is_zero() {
            term = wdiv_small(wmul(term, r2), (k + 1) * (k + 2)).negated();
            k += 2;
            s = wadd(s, term);
            if series_done(term, s) {
                break;
            }
        }
    }
    if want_cos {
        let mut term = ONE;
        c = ONE;
        let mut k = 0u64;
        while !r.is_zero() {
            term = wdiv_small(wmul(term, r2), (k + 1) * (k + 2)).negated();
            k += 2;
            c = wadd(c, term);
            if series_done(term, c) {
                break;
            }
        }
    }
    (s, c)
}

/// Bits [lo, lo+64) of a little-endian limb array.
fn get_bits(s: &[u64; 7], lo: u32) -> u64 {
    let limb = (lo / 64) as usize;
    let off = lo % 64;
    let a = if limb < 7 { s[limb] >> off } else { 0 };
    let b = if off != 0 && limb + 1 < 7 { s[limb + 1] << (64 - off) } else { 0 };
    a | b
}

/// Reduce |x| = sig * 2^(e-63) (e in -1..=62) modulo pi/2: returns (quadrant, remainder in [-pi/4, pi/4]).
fn reduce_pio2(e: i32, sig: u64) -> (u32, W) {
    let k = (63 - e) as u32; // x = sig / 2^k
    // S = sig * (2/pi * 2^384)
    let mut s = [0u64; 7];
    let mut carry = 0u128;
    for i in 0..6 {
        let p = (sig as u128) * (TWO_OVER_PI[5 - i] as u128) + carry;
        s[i] = p as u64;
        carry = p >> 64;
    }
    s[6] = carry as u64;
    // x * 2/pi = S / 2^(384+k)
    let n0 = get_bits(&s, 384 + k);
    let f1 = get_bits(&s, 384 + k - 64);
    let f2 = get_bits(&s, 384 + k - 128);
    let f3 = get_bits(&s, 384 + k - 192);
    let (mut n, mut hi, mut lo, neg) = (n0, ((f1 as u128) << 64) | f2 as u128, f3, false);
    let mut neg = neg;
    if f1 >> 63 != 0 {
        // fraction >= 1/2: take the next integer and a negative remainder (2^192 - F).
        n = n.wrapping_add(1);
        neg = true;
        let (l, c) = (!lo).overflowing_add(1);
        lo = l;
        hi = (!hi).wrapping_add(c as u128);
    }
    let fw = if hi != 0 {
        let lz = hi.leading_zeros();
        // top 128 bits of the 192-bit value (hi:lo) << lz
        let m = if lz == 0 {
            hi
        } else if lz <= 64 {
            (hi << lz) | ((lo as u128) >> (64 - lz))
        } else {
            (hi << lz) | ((lo as u128) << (lz - 64))
        };
        W { neg: false, exp: -1 - lz as i32, m }
    } else if lo != 0 {
        let lz = lo.leading_zeros();
        W { neg: false, exp: -129 - lz as i32, m: ((lo << lz) as u128) << 64 }
    } else {
        ZERO
    };
    let mut r = wmul(fw, PI2);
    if neg && !r.is_zero() {
        r.neg = true;
    }
    ((n & 3) as u32, r)
}

fn f80_const(neg: bool) -> F80 {
    F80 { mant: 1 << 63, se: 0x3FFF | ((neg as u16) << 15) }
}

impl Fpu {
    /// Round a W result to extended precision. With `snap`, results within 2^-40 ulp of a 64-bit
    /// value are treated as exact (kills spurious "inexact" on exactly representable results).
    fn w_to_f80(&mut self, w: W, snap: bool) -> F80 {
        if w.m == 0 {
            return F80::zero(w.neg);
        }
        let (mut sig, mut low, mut exp) = ((w.m >> 64) as u64, w.m as u64, w.exp);
        if snap {
            if low < (1 << 24) {
                low = 0;
            } else if low > u64::MAX - (1 << 24) {
                low = 0;
                sig = sig.wrapping_add(1);
                if sig == 0 {
                    sig = 1 << 63;
                    exp += 1;
                }
            }
        }
        self.round_pack(w.neg, exp, sig, low, 64)
    }

    /// F2XM1: 2^x - 1.
    pub fn f2xm1(&mut self, x: F80) -> F80 {
        match self.unpack(x) {
            V::Zero(_) => x,
            V::Inf(false) => x,
            V::Inf(true) => f80_const(true),
            V::Fin(s, e, m) => {
                if e >= 15 {
                    return if s {
                        // -1 + tiny
                        let w = W { neg: true, exp: -1, m: u128::MAX };
                        self.w_to_f80(w, false)
                    } else {
                        self.round_pack(false, 20000, 1 << 63, 0, 64)
                    };
                }
                let r = exp2m1(W::from_parts(s, e, m));
                // Very negative x gives -1 + tiny, which is genuinely inexact; do not snap it.
                self.w_to_f80(r, !(s && e >= 6))
            }
            v => self.nan1(v).unwrap_or(INDEF),
        }
    }

    /// FYL2X (plus1 = false): y*log2(x); FYL2XP1 (plus1 = true): y*log2(x+1).
    pub fn fyl2x(&mut self, y: F80, x: F80, plus1: bool) -> F80 {
        let vy = self.unpack(y);
        let vx = self.unpack(x);
        let bad = |v: V| matches!(v, V::QNan(_) | V::SNan(_) | V::Bad);
        if bad(vy) || bad(vx) {
            // Operand order for NaN selection follows the instruction (ST0 = x first).
            return self.nan2(vx, vy);
        }
        use V::*;
        // Special values of log2(x) (or log2(x+1)): classify as Zero (log=0), NegInf (x=0), PosInf, Neg (x<0), or finite.
        enum L {
            Zero,
            NegInf,
            PosInf,
            Invalid,
            Fin(W, bool), // value, "is negative"
        }
        let l = match vx {
            Zero(_) => {
                if plus1 {
                    L::Zero
                } else {
                    L::NegInf
                }
            }
            Inf(false) => L::PosInf,
            Inf(true) => L::Invalid,
            Fin(sx, ex, mx) => {
                if plus1 {
                    if sx && ex >= 0 {
                        if ex == 0 && mx == 1 << 63 {
                            L::NegInf // x = -1
                        } else {
                            L::Invalid
                        }
                    } else {
                        // log2(1+x)
                        let w = if ex <= -2 {
                            let xw = W::from_parts(sx, ex, mx);
                            let t = wdiv(xw, wadd(xw, W { neg: false, exp: 1, m: 1 << 127 }));
                            wmul(ln_atanh(t), LOG2E)
                        } else {
                            let v = wadd(ONE, W::from_parts(sx, ex, mx));
                            log2_w(v)
                        };
                        L::Fin(w, if w.is_zero() { false } else { w.neg })
                    }
                } else if sx {
                    L::Invalid
                } else if ex == 0 && mx == 1 << 63 {
                    L::Zero
                } else {
                    let w = log2_w(W::from_parts(false, ex, mx));
                    L::Fin(w, w.neg)
                }
            }
            _ => L::Invalid,
        };
        match (l, vy) {
            (L::Invalid, _) => self.invalid(),
            (L::Zero, Inf(_)) => self.invalid(),
            (L::Zero, Zero(sy)) | (L::Zero, Fin(sy, ..)) => {
                // plus1 with x = +-0: result carries sign(y) ^ sign(x)
                let sx = if plus1 { x.sign() } else { false };
                F80::zero(sy ^ sx)
            }
            (L::NegInf, Zero(_)) | (L::PosInf, Zero(_)) => self.invalid(),
            (L::NegInf, Inf(sy)) => F80::inf(!sy),
            (L::PosInf, Inf(sy)) => F80::inf(sy),
            (L::NegInf, Fin(sy, ..)) => {
                self.sw |= ZE;
                F80::inf(!sy)
            }
            (L::PosInf, Fin(sy, ..)) => F80::inf(sy),
            (L::Fin(_, neg), Inf(sy)) => F80::inf(sy ^ neg),
            (L::Fin(_, neg), Zero(sy)) => F80::zero(sy ^ neg),
            (L::Fin(lw, _), Fin(sy, ey, my)) => {
                let r = wmul(W::from_parts(sy, ey, my), lw);
                self.w_to_f80(r, true)
            }
            _ => INDEF,
        }
    }

    /// FPATAN: atan2(y, x) with y = ST(1), x = ST(0).
    pub fn fpatan(&mut self, y: F80, x: F80) -> F80 {
        let vy = self.unpack(y);
        let vx = self.unpack(x);
        let bad = |v: V| matches!(v, V::QNan(_) | V::SNan(_) | V::Bad);
        if bad(vy) || bad(vx) {
            return self.nan2(vx, vy);
        }
        use V::*;
        let sy = y.sign();
        let sx = x.sign();
        let k = |this: &mut Fpu, w: W| {
            let w = W { neg: sy, ..w };
            this.w_to_f80(w, false)
        };
        match (vy, vx) {
            (Zero(_), _) => {
                if sx {
                    k(self, PI)
                } else {
                    F80::zero(sy)
                }
            }
            (Inf(_), Inf(_)) => {
                if sx {
                    k(self, PI34)
                } else {
                    k(self, PI4)
                }
            }
            (Inf(_), _) => k(self, PI2),
            (_, Inf(_)) => {
                if sx {
                    k(self, PI)
                } else {
                    F80::zero(sy)
                }
            }
            (_, Zero(_)) => k(self, PI2),
            (Fin(_, ey, my), Fin(_, ex, mx)) => {
                let ay = W::from_parts(false, ey, my);
                let ax = W::from_parts(false, ex, mx);
                let a = if (ey, my) <= (ex, mx) {
                    atan_unit(wdiv(ay, ax))
                } else {
                    wsub(PI2, atan_unit(wdiv(ax, ay)))
                };
                let a = if sx { wsub(PI, a) } else { a };
                let a = W { neg: sy, ..a };
                self.w_to_f80(a, true)
            }
            _ => INDEF,
        }
    }

    /// sin / cos / tan of x. `want` bit0 = sin, bit1 = cos, bit2 = tan; the matching entries of the
    /// result are valid. `None` when |x| >= 2^63 (caller sets C2 and leaves the operand alone).
    pub fn trig(&mut self, x: F80, want: u32) -> Option<[F80; 3]> {
        let (s, e, m) = match self.unpack(x) {
            V::Zero(_) => return Some([x, f80_const(false), x]),
            V::Fin(s, e, m) => (s, e, m),
            V::Inf(_) => {
                let r = self.invalid();
                return Some([r; 3]);
            }
            v => {
                let r = self.nan1(v).unwrap_or(INDEF);
                return Some([r; 3]);
            }
        };
        if e >= 63 {
            return None;
        }
        let ax = W::from_parts(false, e, m);
        let (q, r) = if e < -1 || (e == -1 && (ax.m, ax.exp) < (PI4.m, PI4.exp)) {
            (0, ax)
        } else {
            reduce_pio2(e, m)
        };
        // sin(x) needs sin(r) for even quadrants and cos(r) for odd ones; cos(x) the other way round.
        let odd = q & 1 != 0;
        let need_s = (want & 1 != 0 && !odd) || (want & 2 != 0 && odd) || want & 4 != 0;
        let need_c = (want & 1 != 0 && odd) || (want & 2 != 0 && !odd) || want & 4 != 0;
        let (sn, cs) = sincos_kernel(r, need_s, need_c);
        // Quadrant rotation: sin(r + q*pi/2), cos(r + q*pi/2).
        let (sin_a, cos_a) = match q {
            0 => (sn, cs),
            1 => (cs, sn.negated()),
            2 => (sn.negated(), cs.negated()),
            _ => (cs.negated(), sn),
        };
        let mut out = [F80::default(); 3];
        if want & 1 != 0 {
            let v = if s { sin_a.negated() } else { sin_a };
            out[0] = self.w_to_f80(v, true);
        }
        if want & 2 != 0 {
            out[1] = self.w_to_f80(cos_a, true);
        }
        if want & 4 != 0 {
            let t = wdiv(sin_a, cos_a);
            let t = if s { t.negated() } else { t };
            out[2] = self.w_to_f80(t, true);
        }
        Some(out)
    }

    /// FLD constants (value rounded according to RC, as Intel hardware does).
    pub fn fld_const(&mut self, which: u8) -> F80 {
        let w = match which {
            0 => return f80_const(false), // 1.0
            1 => LOG2_10,
            2 => LOG2E,
            3 => PI,
            4 => LOG10_2,
            5 => LN2,
            _ => return F80::zero(false),
        };
        // Constants are rounded per RC but, unlike computed results, never raise #P.
        let r = self.round_core(false, w.exp, (w.m >> 64) as u64, w.m as u64, 64, EMIN80, EMAX80);
        F80 { mant: r.sig, se: (r.exp + 16383) as u16 }
    }
}
