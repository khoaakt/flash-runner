//! IEEE-754 / x86 SSE floating-point semantics, implemented on raw bit patterns.
//!
//! Nothing here touches host float state: arithmetic is done in integers (`u128` for products,
//! quotients and square roots), so results, NaN selection, rounding modes (MXCSR.RC), DAZ/FTZ and
//! the sticky exception flags are identical on aarch64, x86 and wasm32.  The only places the host
//! FPU is used are (a) a fast path for RNE add/sub/mul/div on *normal* operands with a *normal*
//! result, where host IEEE-754 is bit-exact on every target, and (b) RCPPS/RSQRTPS, which are
//! approximations on real hardware anyway.
//!
//! Generic over the format via the [`Fp`] marker trait ([`F32`], [`F64`]); raw bits travel as `u64`
//! (f32 values are zero-extended).
//!
//! x86 specifics implemented here:
//!  * NaN operand selection for SSE: first source if it is a NaN (SNaN quieted), else the second.
//!  * Invalid operation -> "real indefinite" QNaN: 0xFFC00000 / 0xFFF8000000000000.
//!  * Tininess is detected AFTER rounding; UE is only set when also inexact.
//!  * FTZ (only when UM is masked) flushes any tiny result to +-0 and sets UE|PE.
//!  * DAZ treats denormal inputs as zero and suppresses DE.
//!  * MIN/MAX return the second operand when either is NaN or both are zero.
//!
//! Exceptions are always treated as masked (no #XM): flags accumulate in `Env::fl`, which uses the
//! MXCSR bit positions so the caller just ORs it into MXCSR.

pub const IE: u32 = 0x01;
pub const DE: u32 = 0x02;
pub const ZE: u32 = 0x04;
pub const OE: u32 = 0x08;
pub const UE: u32 = 0x10;
pub const PE: u32 = 0x20;

/// Per-instruction FP environment snapshot (from MXCSR) plus the flags raised so far.
#[derive(Clone, Copy)]
pub struct Env {
    /// Rounding control: 0 nearest-even, 1 down, 2 up, 3 toward zero.
    pub rc: u32,
    pub daz: bool,
    /// FZ && UM (flush-to-zero only takes effect while underflow is masked).
    pub ftz: bool,
    /// RNE, no FTZ/DAZ and PE already sticky: the host fast path cannot miss any flag.
    fast: bool,
    pub fl: u32,
}

impl Env {
    #[inline(always)]
    pub fn new(mxcsr: u32) -> Env {
        Env {
            rc: (mxcsr >> 13) & 3,
            daz: mxcsr & 0x40 != 0,
            ftz: mxcsr & 0x8000 != 0 && mxcsr & 0x800 != 0,
            fast: mxcsr & (0x6000 | 0x8000 | 0x40 | PE) == PE,
            fl: 0,
        }
    }
}

pub trait Fp {
    const E: u32;
    const M: u32;
    const BIAS: i32 = (1 << (Self::E - 1)) - 1;
    const SIGN: u64 = 1u64 << (Self::E + Self::M);
    /// Exponent-field mask (also the bit pattern of +infinity).
    const EXPM: u64 = ((1u64 << Self::E) - 1) << Self::M;
    const FRAC: u64 = (1u64 << Self::M) - 1;
    const QBIT: u64 = 1u64 << (Self::M - 1);
    /// x86 "real indefinite" QNaN.
    const DNAN: u64 = Self::SIGN | Self::EXPM | Self::QBIT;
}
pub struct F32;
pub struct F64;
impl Fp for F32 {
    const E: u32 = 8;
    const M: u32 = 23;
}
impl Fp for F64 {
    const E: u32 = 11;
    const M: u32 = 52;
}

#[inline(always)]
pub fn is_nan<T: Fp>(x: u64) -> bool {
    (x & !T::SIGN) > T::EXPM
}
#[inline(always)]
fn is_snan<T: Fp>(x: u64) -> bool {
    is_nan::<T>(x) && x & T::QBIT == 0
}
#[inline(always)]
fn quiet<T: Fp>(x: u64) -> u64 {
    x | T::QBIT
}
#[inline(always)]
fn is_den<T: Fp>(x: u64) -> bool {
    x & T::EXPM == 0 && x & T::FRAC != 0
}
#[inline(always)]
fn zero<T: Fp>(sign: bool) -> u64 {
    if sign {
        T::SIGN
    } else {
        0
    }
}
#[inline(always)]
fn inf<T: Fp>(sign: bool) -> u64 {
    T::EXPM | if sign { T::SIGN } else { 0 }
}
/// DAZ input flush: denormal -> signed zero.
#[inline(always)]
fn daz_in<T: Fp>(x: u64, env: &Env) -> u64 {
    if env.daz && x & T::EXPM == 0 {
        x & T::SIGN
    } else {
        x
    }
}

/// SSE NaN selection for a two-source operation; `None` if neither operand is a NaN.
#[inline(always)]
fn nan2<T: Fp>(a: u64, b: u64, env: &mut Env) -> Option<u64> {
    let (an, bn) = (is_nan::<T>(a), is_nan::<T>(b));
    if !(an | bn) {
        return None;
    }
    if is_snan::<T>(a) | is_snan::<T>(b) {
        env.fl |= IE;
    }
    Some(quiet::<T>(if an { a } else { b }))
}

#[derive(Clone, Copy, PartialEq)]
enum K {
    Zero,
    Fin,
    Inf,
}

/// Unpacked non-NaN value: |x| = m * 2^e with `m` normalized so bit `M` is set (when `Fin`).
#[derive(Clone, Copy)]
struct U {
    sign: bool,
    kind: K,
    e: i32,
    m: u64,
}

#[inline(always)]
fn unpack<T: Fp>(x: u64, env: &mut Env) -> U {
    let sign = x & T::SIGN != 0;
    let ex = ((x & T::EXPM) >> T::M) as i32;
    let fr = x & T::FRAC;
    if ex == 0 {
        if fr == 0 || env.daz {
            return U { sign, kind: K::Zero, e: 0, m: 0 };
        }
        env.fl |= DE;
        let sh = fr.leading_zeros() - (63 - T::M);
        U { sign, kind: K::Fin, e: 1 - T::BIAS - T::M as i32 - sh as i32, m: fr << sh }
    } else if ex == (1 << T::E) - 1 {
        U { sign, kind: K::Inf, e: 0, m: 0 }
    } else {
        U { sign, kind: K::Fin, e: ex - T::BIAS - T::M as i32, m: fr | (1u64 << T::M) }
    }
}

/// Round `s * 2^e` (s != 0; its lowest bit may be a "sticky" jam) to format `T` under `env.rc`,
/// raising OE/UE/PE as appropriate, and return the packed bits.
fn round_pack<T: Fp>(sign: bool, e: i32, s: u128, env: &mut Env) -> u64 {
    let lz = s.leading_zeros();
    let s = s << lz;
    // Exponent of the leading bit (value is in [2^ue, 2^(ue+1))).
    let ue = e + 127 - lz as i32;
    let p = (T::M + 1) as i32;
    let emin = 1 - T::BIAS;
    let tiny_pre = ue < emin;
    // Mantissa bits that can be kept: fewer when the result lands in the denormal range.
    let keep = if tiny_pre { p - (emin - ue) } else { p };
    let mut q: u64;
    let rb: bool;
    let st: bool;
    if keep > 0 {
        let sh = (128 - keep) as u32;
        q = (s >> sh) as u64;
        rb = (s >> (sh - 1)) & 1 != 0;
        st = s & ((1u128 << (sh - 1)) - 1) != 0;
    } else if keep == 0 {
        q = 0;
        rb = true; // value in [0.5, 1) of the smallest denormal
        st = (s << 1) != 0;
    } else {
        q = 0;
        rb = false;
        st = true;
    }
    let inexact = rb | st;
    let inc = match env.rc {
        0 => rb && (st || q & 1 != 0),
        1 => sign && inexact,
        2 => !sign && inexact,
        _ => false,
    };
    q += inc as u64;
    let sgn = if sign { T::SIGN } else { 0 };
    if !tiny_pre {
        let mut ue = ue;
        if q >> p != 0 {
            q >>= 1;
            ue += 1;
        }
        if ue > T::BIAS {
            env.fl |= OE | PE;
            let to_inf = match env.rc {
                0 => true,
                1 => sign,
                2 => !sign,
                _ => false,
            };
            return sgn | if to_inf { T::EXPM } else { (T::EXPM - (1u64 << T::M)) | T::FRAC };
        }
        if inexact {
            env.fl |= PE;
        }
        return sgn | (((ue + T::BIAS) as u64) << T::M) | (q & T::FRAC);
    }
    // Tiny before rounding. It is still not "tiny after rounding" if rounding to p bits (unbounded
    // exponent range) would carry up to the smallest normal number.
    let mut tiny = true;
    if ue == emin - 1 {
        let sh = (128 - p) as u32;
        let qp = (s >> sh) as u64;
        let rbp = (s >> (sh - 1)) & 1 != 0;
        let stp = s & ((1u128 << (sh - 1)) - 1) != 0;
        let incp = match env.rc {
            0 => rbp && (stp || qp & 1 != 0),
            1 => sign && (rbp | stp),
            2 => !sign && (rbp | stp),
            _ => false,
        };
        if incp && qp == (1u64 << p) - 1 {
            tiny = false;
        }
    }
    if tiny && env.ftz {
        env.fl |= UE | PE;
        return sgn;
    }
    if tiny && inexact {
        env.fl |= UE;
    }
    if inexact {
        env.fl |= PE;
    }
    // q is the denormal fraction; q == 2^M naturally encodes the smallest normal.
    sgn | q
}

// ---- add / sub / mul / div / sqrt ---------------------------------------------------------------

pub fn add<T: Fp>(a: u64, b: u64, env: &mut Env, sub: bool) -> u64 {
    if let Some(r) = nan2::<T>(a, b, env) {
        return r;
    }
    let ua = unpack::<T>(a, env);
    let mut ub = unpack::<T>(b, env);
    if sub {
        ub.sign = !ub.sign;
    }
    match (ua.kind, ub.kind) {
        (K::Inf, K::Inf) => {
            if ua.sign == ub.sign {
                inf::<T>(ua.sign)
            } else {
                env.fl |= IE;
                T::DNAN
            }
        }
        (K::Inf, _) => inf::<T>(ua.sign),
        (_, K::Inf) => inf::<T>(ub.sign),
        (K::Zero, K::Zero) => zero::<T>(if ua.sign == ub.sign { ua.sign } else { env.rc == 1 }),
        (K::Zero, _) => round_pack::<T>(ub.sign, ub.e, ub.m as u128, env),
        (_, K::Zero) => round_pack::<T>(ua.sign, ua.e, ua.m as u128, env),
        _ => {
            let (x, y) = if (ua.e, ua.m) >= (ub.e, ub.m) { (ua, ub) } else { (ub, ua) };
            let d = (x.e - y.e) as u32;
            // 62 spare low bits keep every bit of the aligned smaller operand unless the exponent
            // gap is huge, in which case the jammed sticky bit is all that matters.
            let big = (x.m as u128) << 62;
            let mut small = (y.m as u128) << 62;
            if d != 0 {
                small = if d >= 127 {
                    1
                } else {
                    let lost = small & ((1u128 << d) - 1) != 0;
                    (small >> d) | lost as u128
                };
            }
            let s = if x.sign == y.sign {
                big + small
            } else {
                let s = big - small;
                if s == 0 {
                    return zero::<T>(env.rc == 1);
                }
                s
            };
            round_pack::<T>(x.sign, x.e - 62, s, env)
        }
    }
}

pub fn mul<T: Fp>(a: u64, b: u64, env: &mut Env) -> u64 {
    if let Some(r) = nan2::<T>(a, b, env) {
        return r;
    }
    let ua = unpack::<T>(a, env);
    let ub = unpack::<T>(b, env);
    let sign = ua.sign != ub.sign;
    match (ua.kind, ub.kind) {
        (K::Inf, K::Zero) | (K::Zero, K::Inf) => {
            env.fl |= IE;
            T::DNAN
        }
        (K::Inf, _) | (_, K::Inf) => inf::<T>(sign),
        (K::Zero, _) | (_, K::Zero) => zero::<T>(sign),
        _ => round_pack::<T>(sign, ua.e + ub.e, ua.m as u128 * ub.m as u128, env),
    }
}

pub fn div<T: Fp>(a: u64, b: u64, env: &mut Env) -> u64 {
    if let Some(r) = nan2::<T>(a, b, env) {
        return r;
    }
    let fl0 = env.fl;
    let ua = unpack::<T>(a, env);
    let ub = unpack::<T>(b, env);
    let sign = ua.sign != ub.sign;
    match (ua.kind, ub.kind) {
        (K::Inf, K::Inf) | (K::Zero, K::Zero) => {
            env.fl = fl0 | IE;
            T::DNAN
        }
        (K::Inf, _) => inf::<T>(sign),
        (_, K::Inf) => zero::<T>(sign),
        (_, K::Zero) => {
            // IE/ZE outrank the denormal-operand exception: DE is not reported alongside them.
            env.fl = fl0 | ZE;
            inf::<T>(sign)
        }
        (K::Zero, _) => zero::<T>(sign),
        _ => {
            let n = (ua.m as u128) << 70;
            let d = ub.m as u128;
            let q = n / d;
            let sticky = (n - q * d != 0) as u128;
            round_pack::<T>(sign, ua.e - ub.e - 70, q | sticky, env)
        }
    }
}

/// floor(sqrt(n)) for n < 2^126.
fn isqrt(n: u128) -> u64 {
    // Host sqrt (exact IEEE on every target) gives ~53 correct bits; one Newton correction computed
    // from the exact integer residual plus a fix-up loop makes the result exact.
    let r0 = (n as f64).sqrt() as u64;
    let r0 = r0.max(1);
    let resid = n as i128 - (r0 as u128 * r0 as u128) as i128;
    let delta = (resid as f64 / (2.0 * r0 as f64)) as i64;
    let mut r = (r0 as i128 + delta as i128).max(1) as u64;
    while (r as u128) * (r as u128) > n {
        r -= 1;
    }
    while ((r + 1) as u128) * ((r + 1) as u128) <= n {
        r += 1;
    }
    r
}

pub fn sqrt<T: Fp>(a: u64, env: &mut Env) -> u64 {
    if is_nan::<T>(a) {
        if is_snan::<T>(a) {
            env.fl |= IE;
        }
        return quiet::<T>(a);
    }
    let fl0 = env.fl;
    let u = unpack::<T>(a, env);
    match u.kind {
        K::Zero => return zero::<T>(u.sign),
        K::Inf => {
            if u.sign {
                env.fl = fl0 | IE;
                return T::DNAN;
            }
            return inf::<T>(false);
        }
        K::Fin => {}
    }
    if u.sign {
        env.fl = fl0 | IE; // IE outranks DE
        return T::DNAN;
    }
    let (mut m, mut e) = (u.m as u128, u.e);
    if e & 1 != 0 {
        m <<= 1;
        e -= 1;
    }
    let n = m << 72;
    let r = isqrt(n);
    let exact = (r as u128) * (r as u128) == n;
    round_pack::<T>(false, e / 2 - 36, r as u128 | (!exact) as u128, env)
}

// ---- fast paths (host IEEE-754 is bit-exact for RNE when operands and result are normal) --------

// A fast-path RESULT must have biased exponent >= 2, not merely >= 1: a product/quotient that lies
// just below the smallest normal but rounds (at denormal precision) onto it is "tiny after
// rounding" and must raise UE, which only the soft path knows how to do.
#[inline(always)]
fn normal64(x: u64) -> bool {
    let e = (x >> 52) & 0x7ff;
    e.wrapping_sub(1) < 0x7fe
}
#[inline(always)]
fn normal32(x: u32) -> bool {
    let e = (x >> 23) & 0xff;
    e.wrapping_sub(1) < 0xfe
}
#[inline(always)]
fn result_ok64(x: u64) -> bool {
    ((x >> 52) & 0x7ff).wrapping_sub(2) < 0x7fd
}
#[inline(always)]
fn result_ok32(x: u32) -> bool {
    ((x >> 23) & 0xff).wrapping_sub(2) < 0xfd
}

macro_rules! fast_op {
    ($name:ident, $slow:ident, $t:ty, $ft:ty, $normal:ident, $res_ok:ident, |$x:ident, $y:ident| $e:expr) => {
        #[inline(always)]
        pub fn $name(a: $t, b: $t, env: &mut Env) -> $t {
            if env.fast && $normal(a) && $normal(b) {
                let $x = <$ft>::from_bits(a);
                let $y = <$ft>::from_bits(b);
                let r: $ft = $e;
                let rb = r.to_bits();
                if $res_ok(rb) {
                    return rb;
                }
            }
            $slow(a, b, env)
        }
    };
}

fn add32_s(a: u32, b: u32, env: &mut Env) -> u32 {
    add::<F32>(a as u64, b as u64, env, false) as u32
}
fn sub32_s(a: u32, b: u32, env: &mut Env) -> u32 {
    add::<F32>(a as u64, b as u64, env, true) as u32
}
fn mul32_s(a: u32, b: u32, env: &mut Env) -> u32 {
    mul::<F32>(a as u64, b as u64, env) as u32
}
fn div32_s(a: u32, b: u32, env: &mut Env) -> u32 {
    div::<F32>(a as u64, b as u64, env) as u32
}
fn add64_s(a: u64, b: u64, env: &mut Env) -> u64 {
    add::<F64>(a, b, env, false)
}
fn sub64_s(a: u64, b: u64, env: &mut Env) -> u64 {
    add::<F64>(a, b, env, true)
}
fn mul64_s(a: u64, b: u64, env: &mut Env) -> u64 {
    mul::<F64>(a, b, env)
}
fn div64_s(a: u64, b: u64, env: &mut Env) -> u64 {
    div::<F64>(a, b, env)
}

fast_op!(add32, add32_s, u32, f32, normal32, result_ok32, |x, y| x + y);
fast_op!(sub32, sub32_s, u32, f32, normal32, result_ok32, |x, y| x - y);
fast_op!(mul32, mul32_s, u32, f32, normal32, result_ok32, |x, y| x * y);
fast_op!(div32, div32_s, u32, f32, normal32, result_ok32, |x, y| x / y);
fast_op!(add64, add64_s, u64, f64, normal64, result_ok64, |x, y| x + y);
fast_op!(sub64, sub64_s, u64, f64, normal64, result_ok64, |x, y| x - y);
fast_op!(mul64, mul64_s, u64, f64, normal64, result_ok64, |x, y| x * y);
fast_op!(div64, div64_s, u64, f64, normal64, result_ok64, |x, y| x / y);

#[inline(always)]
pub fn sqrt64(a: u64, env: &mut Env) -> u64 {
    // Positive normal operand: the result is normal; PE is already sticky so exactness is moot.
    if env.fast && normal64(a) && a >> 63 == 0 {
        return f64::from_bits(a).sqrt().to_bits();
    }
    sqrt::<F64>(a, env)
}
#[inline(always)]
pub fn sqrt32(a: u32, env: &mut Env) -> u32 {
    if env.fast && normal32(a) && a >> 31 == 0 {
        return f32::from_bits(a).sqrt().to_bits();
    }
    sqrt::<F32>(a as u64, env) as u32
}

// ---- min / max / compare ------------------------------------------------------------------------

/// a < b for two non-NaN values (+0 and -0 compare equal).
#[inline(always)]
fn lt<T: Fp>(a: u64, b: u64) -> bool {
    let (sa, sb) = (a & T::SIGN != 0, b & T::SIGN != 0);
    let (ma, mb) = (a & !T::SIGN, b & !T::SIGN);
    if ma == 0 && mb == 0 {
        return false;
    }
    if sa != sb {
        return sa;
    }
    if sa {
        ma > mb
    } else {
        ma < mb
    }
}
#[inline(always)]
fn eq<T: Fp>(a: u64, b: u64) -> bool {
    let (ma, mb) = (a & !T::SIGN, b & !T::SIGN);
    (ma == 0 && mb == 0) || a == b
}

/// MINPS/MAXPS lane: returns the second operand if either is NaN, or if the compare is false
/// (which includes +0 vs -0). A NaN of either kind raises IE.
pub fn minmax<T: Fp>(a: u64, b: u64, env: &mut Env, is_max: bool) -> u64 {
    // With DAZ the comparison sees denormals as zero, but the selected operand is forwarded as is.
    let (fa, fb) = (daz_in::<T>(a, env), daz_in::<T>(b, env));
    if is_nan::<T>(fa) | is_nan::<T>(fb) {
        env.fl |= IE;
        return b;
    }
    if is_den::<T>(fa) | is_den::<T>(fb) {
        env.fl |= DE;
    }
    let pick_a = if is_max { lt::<T>(fb, fa) } else { lt::<T>(fa, fb) };
    if pick_a {
        a
    } else {
        b
    }
}

/// CMPPS/CMPSS predicate (0..7) on one lane; true = all-ones mask.
pub fn cmp_pred<T: Fp>(a: u64, b: u64, pred: u8, env: &mut Env) -> bool {
    let a = daz_in::<T>(a, env);
    let b = daz_in::<T>(b, env);
    let nan = is_nan::<T>(a) | is_nan::<T>(b);
    let p = pred & 7;
    if nan {
        // Predicates 1,2,5,6 are "signaling": any NaN raises IE; the quiet ones only for SNaN.
        if is_snan::<T>(a) | is_snan::<T>(b) | matches!(p, 1 | 2 | 5 | 6) {
            env.fl |= IE;
        }
    } else if is_den::<T>(a) | is_den::<T>(b) {
        env.fl |= DE;
    }
    match p {
        0 => !nan && eq::<T>(a, b),
        1 => !nan && lt::<T>(a, b),
        2 => !nan && (lt::<T>(a, b) || eq::<T>(a, b)),
        3 => nan,
        4 => nan || !eq::<T>(a, b),
        5 => nan || !lt::<T>(a, b),
        6 => nan || !(lt::<T>(a, b) || eq::<T>(a, b)),
        _ => !nan,
    }
}

/// COMISx / UCOMISx: returns (ZF, PF, CF). `signal_qnan` is true for COMIS (any NaN raises IE).
pub fn comis<T: Fp>(a: u64, b: u64, signal_qnan: bool, env: &mut Env) -> (bool, bool, bool) {
    let a = daz_in::<T>(a, env);
    let b = daz_in::<T>(b, env);
    if is_nan::<T>(a) | is_nan::<T>(b) {
        if signal_qnan | is_snan::<T>(a) | is_snan::<T>(b) {
            env.fl |= IE;
        }
        return (true, true, true);
    }
    if is_den::<T>(a) | is_den::<T>(b) {
        env.fl |= DE;
    }
    if lt::<T>(a, b) {
        (false, false, true)
    } else if lt::<T>(b, a) {
        (false, false, false)
    } else {
        (true, false, false)
    }
}

// ---- conversions --------------------------------------------------------------------------------

/// Float -> float (CVTSS2SD / CVTSD2SS). NaN payloads are widened/truncated and quieted.
pub fn cvt_f2f<S: Fp, D: Fp>(a: u64, env: &mut Env) -> u64 {
    let sign = if a & S::SIGN != 0 { D::SIGN } else { 0 };
    if is_nan::<S>(a) {
        if is_snan::<S>(a) {
            env.fl |= IE;
        }
        let f = (a & S::FRAC) | S::QBIT;
        let f = if D::M > S::M { f << (D::M - S::M) } else { f >> (S::M - D::M) };
        return sign | D::EXPM | f;
    }
    let u = unpack::<S>(a, env);
    match u.kind {
        K::Zero => sign,
        K::Inf => sign | D::EXPM,
        K::Fin => round_pack::<D>(u.sign, u.e, u.m as u128, env),
    }
}

/// Signed 32-bit integer -> float (CVTSI2SS/SD, CVTDQ2PS/PD, CVTPI2PS/PD).
pub fn i2f<T: Fp>(v: i32, env: &mut Env) -> u64 {
    if v == 0 {
        return 0;
    }
    round_pack::<T>(v < 0, 0, v.unsigned_abs() as u128, env)
}

/// Float -> signed 32-bit integer under MXCSR.RC (or truncating). Overflow, NaN and infinity give
/// the "integer indefinite" 0x80000000 and IE.
pub fn f2i<T: Fp>(a: u64, trunc: bool, env: &mut Env) -> u32 {
    const INDEF: u32 = 0x8000_0000;
    if is_nan::<T>(a) {
        env.fl |= IE;
        return INDEF;
    }
    // Float->int conversions can raise IE and PE only: a denormal source is not reported (DE).
    let de0 = env.fl & DE;
    let u = unpack::<T>(a, env);
    env.fl = (env.fl & !DE) | de0;
    match u.kind {
        K::Zero => return 0,
        K::Inf => {
            env.fl |= IE;
            return INDEF;
        }
        K::Fin => {}
    }
    let (ip, rb, st): (u64, bool, bool);
    if u.e >= 0 {
        if u.e + T::M as i32 >= 32 {
            env.fl |= IE;
            return INDEF;
        }
        ip = u.m << u.e;
        rb = false;
        st = false;
    } else {
        let sh = (-u.e) as u32;
        if sh >= 64 {
            ip = 0;
            rb = false;
            st = true;
        } else {
            ip = u.m >> sh;
            rb = (u.m >> (sh - 1)) & 1 != 0;
            st = u.m & ((1u64 << (sh - 1)) - 1) != 0;
        }
    }
    let inexact = rb | st;
    let rc = if trunc { 3 } else { env.rc };
    let inc = match rc {
        0 => rb && (st || ip & 1 != 0),
        1 => u.sign && inexact,
        2 => !u.sign && inexact,
        _ => false,
    };
    let ip = ip + inc as u64;
    let ok = if u.sign { ip <= 0x8000_0000 } else { ip <= 0x7fff_ffff };
    if !ok {
        env.fl |= IE;
        return INDEF;
    }
    if inexact {
        env.fl |= PE;
    }
    if u.sign {
        (ip as u32).wrapping_neg()
    } else {
        ip as u32
    }
}

// ---- reciprocal / reciprocal square root (approximations on hardware) ---------------------------
//
// Real CPUs return implementation-specific 12-bit-accurate values (Intel and AMD differ).  We return
// the correctly rounded result (relative error <= 2^-24, well inside the architectural 1.5*2^-12),
// computed via host f64 division/sqrt which is exact IEEE and therefore deterministic everywhere.
// Special cases follow the SDM: denormal inputs are treated as zero, tiny results flush to zero,
// no exception flags are raised.

pub fn rcp32(a: u32) -> u32 {
    if is_nan::<F32>(a as u64) {
        return a | 0x0040_0000;
    }
    let sign = a & 0x8000_0000;
    let ex = (a >> 23) & 0xff;
    if ex == 0 {
        return sign | 0x7f80_0000;
    }
    if ex == 0xff {
        return sign;
    }
    let r = (1.0f64 / f32::from_bits(a) as f64) as f32;
    let rb = r.to_bits();
    if (rb >> 23) & 0xff == 0 {
        sign
    } else {
        rb
    }
}

pub fn rsqrt32(a: u32) -> u32 {
    if is_nan::<F32>(a as u64) {
        return a | 0x0040_0000;
    }
    let sign = a & 0x8000_0000;
    let ex = (a >> 23) & 0xff;
    if ex == 0 {
        return sign | 0x7f80_0000; // +-0 and denormals -> +-inf
    }
    if sign != 0 {
        return F32::DNAN as u32; // negative (including -inf)
    }
    if ex == 0xff {
        return 0;
    }
    ((1.0f64 / (f32::from_bits(a) as f64).sqrt()) as f32).to_bits()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        /// Random f64 bits biased toward interesting exponents.
        fn f64(&mut self) -> u64 {
            let r = self.next();
            match r % 8 {
                0 => r & 0x800f_ffff_ffff_ffff, // denormal / zero
                1 => (r & 0x800f_ffff_ffff_ffff) | (((r >> 60) % 6 + 0x3fc) << 52),
                2 => r,
                _ => (r & 0x800f_ffff_ffff_ffff) | ((0x3f0 + (r >> 56) % 0x20) << 52),
            }
        }
        fn f32(&mut self) -> u32 {
            let r = self.next() as u32;
            match r % 8 {
                0 => r & 0x807f_ffff,
                1 => (r & 0x807f_ffff) | ((r >> 28) + 0x7c) << 23,
                2 => r,
                _ => (r & 0x807f_ffff) | (0x70 + (r >> 26) % 0x20) << 23,
            }
        }
    }

    fn canon64(x: u64, any_nan_in: bool) -> u64 {
        if f64::from_bits(x).is_nan() && !any_nan_in {
            F64::DNAN
        } else {
            x
        }
    }

    #[test]
    fn f64_rne_matches_host() {
        let mut g = Rng(0x9e3779b97f4a7c15);
        for i in 0..300_000 {
            let (a, b) = (g.f64(), g.f64());
            let nan_in = f64::from_bits(a).is_nan() || f64::from_bits(b).is_nan();
            if nan_in {
                continue;
            }
            let (x, y) = (f64::from_bits(a), f64::from_bits(b));
            let mut e = Env::new(0x1f80);
            assert_eq!(add::<F64>(a, b, &mut e, false), canon64((x + y).to_bits(), false), "add {a:x} {b:x} #{i}");
            assert_eq!(add::<F64>(a, b, &mut e, true), canon64((x - y).to_bits(), false), "sub {a:x} {b:x}");
            assert_eq!(mul::<F64>(a, b, &mut e), canon64((x * y).to_bits(), false), "mul {a:x} {b:x}");
            assert_eq!(div::<F64>(a, b, &mut e), canon64((x / y).to_bits(), false), "div {a:x} {b:x}");
            assert_eq!(sqrt::<F64>(a, &mut e), canon64(x.sqrt().to_bits(), false), "sqrt {a:x}");
            // fast-path wrappers must agree too
            let mut ef = Env::new(0x1fa0);
            assert_eq!(add64(a, b, &mut ef), canon64((x + y).to_bits(), false));
            assert_eq!(mul64(a, b, &mut ef), canon64((x * y).to_bits(), false));
            assert_eq!(div64(a, b, &mut ef), canon64((x / y).to_bits(), false));
            assert_eq!(sqrt64(a, &mut ef), canon64(x.sqrt().to_bits(), false));
        }
    }

    #[test]
    fn f32_rne_matches_host() {
        let mut g = Rng(0x1234_5678_9abc_def1);
        for _ in 0..300_000 {
            let (a, b) = (g.f32(), g.f32());
            let (x, y) = (f32::from_bits(a), f32::from_bits(b));
            if x.is_nan() || y.is_nan() {
                continue;
            }
            // classify by bits: with optimisations on, `r.is_nan()` on a sqrt() result was folded to false for negative inputs
            let canon = |r: f32| if r.to_bits() & 0x7fff_ffff > 0x7f80_0000 { F32::DNAN as u32 } else { r.to_bits() };
            let mut e = Env::new(0x1f80);
            let (a64, b64) = (a as u64, b as u64);
            assert_eq!(add::<F32>(a64, b64, &mut e, false) as u32, canon(x + y), "add {a:x} {b:x}");
            assert_eq!(add::<F32>(a64, b64, &mut e, true) as u32, canon(x - y), "sub {a:x} {b:x}");
            assert_eq!(mul::<F32>(a64, b64, &mut e) as u32, canon(x * y), "mul {a:x} {b:x}");
            assert_eq!(div::<F32>(a64, b64, &mut e) as u32, canon(x / y), "div {a:x} {b:x}");
            assert_eq!(sqrt::<F32>(a64, &mut e) as u32, canon(x.sqrt()), "sqrt {a:x}");
            let mut ef = Env::new(0x1fa0);
            assert_eq!(add32(a, b, &mut ef), canon(x + y));
            assert_eq!(mul32(a, b, &mut ef), canon(x * y));
            assert_eq!(sqrt32(a, &mut ef), canon(x.sqrt()));
        }
    }

    #[test]
    fn cvt_matches_host() {
        let mut g = Rng(0xdead_beef_cafe_f00d);
        for _ in 0..200_000 {
            let a = g.f64();
            let x = f64::from_bits(a);
            if x.is_nan() {
                continue;
            }
            let mut e = Env::new(0x1f80);
            assert_eq!(cvt_f2f::<F64, F32>(a, &mut e) as u32, (x as f32).to_bits(), "d2s {a:x}");
            let s = g.f32();
            let sx = f32::from_bits(s);
            if !sx.is_nan() {
                assert_eq!(cvt_f2f::<F32, F64>(s as u64, &mut e), (sx as f64).to_bits());
            }
            let i = g.next() as i32;
            assert_eq!(i2f::<F32>(i, &mut e) as u32, (i as f32).to_bits());
            assert_eq!(i2f::<F64>(i, &mut e), (i as f64).to_bits());
            // f64 -> i32 truncation agrees with Rust's saturating cast away from the overflow range
            if x.abs() < 2147483647.0 {
                assert_eq!(f2i::<F64>(a, true, &mut e) as i32, x as i32, "cvtt {a:x}");
                assert_eq!(f2i::<F64>(a, false, &mut e) as i32, x.round_ties_even() as i32, "cvt {a:x}");
            }
        }
    }

    #[test]
    fn directed_rounding_brackets_exact() {
        // RD <= RNE <= RU and RZ is whichever of RD/RU is toward zero; check on random ops.
        let mut g = Rng(42);
        for _ in 0..100_000 {
            let (a, b) = (g.f64(), g.f64());
            if f64::from_bits(a).is_nan() || f64::from_bits(b).is_nan() {
                continue;
            }
            for op in 0..4 {
                let run = |rc: u32| {
                    let mut e = Env::new(0x1f80 | (rc << 13));
                    match op {
                        0 => add::<F64>(a, b, &mut e, false),
                        1 => add::<F64>(a, b, &mut e, true),
                        2 => mul::<F64>(a, b, &mut e),
                        _ => div::<F64>(a, b, &mut e),
                    }
                };
                let (n, d, u, z) = (run(0), run(1), run(2), run(3));
                let (fn_, fd, fu, fz) = (f64::from_bits(n), f64::from_bits(d), f64::from_bits(u), f64::from_bits(z));
                if fn_.is_nan() {
                    continue;
                }
                assert!(fd <= fn_ && fn_ <= fu, "bracket op{op} {a:x} {b:x}");
                let toward_zero = if fn_ >= 0.0 && !(fn_ == 0.0 && n >> 63 != 0) { d } else { u };
                if fd != 0.0 || fu != 0.0 {
                    assert!(z == toward_zero || fz == 0.0, "rz op{op} {a:x} {b:x}");
                }
            }
        }
    }

    #[test]
    fn ftz_daz_flags() {
        // 0x007fffff (largest f32 denormal) * (1 + 2^-23) rounds up to the smallest normal: not tiny
        // after rounding, so no flush and no UE even with FTZ.
        let mut e = Env::new(0x1f80 | 0x8000);
        let r = mul::<F32>(0x007f_ffff, 0x3f80_0001, &mut e) as u32;
        assert_eq!(r, 0x0080_0000);
        assert_eq!(e.fl & UE, 0);
        // A plainly tiny result flushes with UE|PE.
        let mut e = Env::new(0x1f80 | 0x8000);
        assert_eq!(mul::<F32>(0x0040_0000, 0x3f00_0000, &mut e) as u32, 0);
        assert_eq!(e.fl & (UE | PE), UE | PE);
        // DAZ: denormal input is zero, no DE.
        let mut e = Env::new(0x1f80 | 0x40);
        assert_eq!(add::<F32>(1, 0x3f80_0000, &mut e, false) as u32, 0x3f80_0000);
        assert_eq!(e.fl & DE, 0);
        // 1/0 -> inf, ZE
        let mut e = Env::new(0x1f80);
        assert_eq!(div::<F32>(0x3f80_0000, 0, &mut e) as u32, 0x7f80_0000);
        assert_eq!(e.fl, ZE);
    }

    #[test]
    fn fast_path_underflow_boundary() {
        // 2^-126 * (1 - 2^-24) is exactly representable with 24 bits but tiny: the host rounds it onto
        // the smallest normal at denormal precision. UE|PE must still be raised with PE preset.
        let mut e = Env::new(0x1fa0);
        assert_eq!(mul32(0x0080_0000, 0x3f7f_ffff, &mut e), 0x0080_0000);
        assert_eq!(e.fl & (UE | PE), UE | PE);
        let mut e = Env::new(0x1fa0);
        assert_eq!(div32(0x0080_0000, 0x3f80_0001, &mut e), 0x007f_ffff);
        let mut e = Env::new(0x1fa0);
        assert_eq!(mul64(0x0010_0000_0000_0000, 0x3fef_ffff_ffff_ffff, &mut e), 0x0010_0000_0000_0000);
        assert_eq!(e.fl & (UE | PE), UE | PE);
        // and the soft path agrees
        let mut e = Env::new(0x1f80);
        assert_eq!(mul::<F32>(0x0080_0000, 0x3f7f_ffff, &mut e), 0x0080_0000);
        assert_eq!(e.fl & (UE | PE), UE | PE);
    }

    #[test]
    fn nan_rules() {
        let mut e = Env::new(0x1f80);
        let (snan, qnan) = (0x7fa0_0001u64, 0x7fc0_0003u64);
        assert_eq!(add::<F32>(snan, qnan, &mut e, false), 0x7fe0_0001);
        assert_eq!(e.fl & IE, IE);
        let mut e = Env::new(0x1f80);
        assert_eq!(add::<F32>(qnan, snan, &mut e, false), qnan);
        assert_eq!(minmax::<F32>(qnan, 0x3f80_0000, &mut e, false), 0x3f80_0000);
        assert_eq!(minmax::<F32>(0x3f80_0000, qnan, &mut e, true), qnan);
        // inf - inf
        let mut e = Env::new(0x1f80);
        assert_eq!(add::<F32>(0x7f80_0000, 0x7f80_0000, &mut e, true), 0xffc0_0000);
    }

    #[test]
    fn nan_selection_follows_sdm_not_x87() {
        // SSE: first source operand wins whenever it is a NaN; otherwise the second.
        let (s1, s2, q1, q2) = (0x7fa0_0001u64, 0x7fa0_0002u64, 0x7fc0_0003u64, 0x7fc0_0009u64);
        let mut e = Env::new(0x1f80);
        assert_eq!(mul::<F32>(q1, q2, &mut e), q1); // larger payload is q2, but first wins
        assert_eq!(e.fl, 0);
        assert_eq!(div::<F32>(s1, s2, &mut e), 0x7fe0_0001);
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80);
        assert_eq!(add::<F32>(q1, s2, &mut e, false), q1); // QNaN first, SNaN second: QNaN, but IE raised
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80);
        assert_eq!(add::<F32>(0x3f80_0000, q2, &mut e, true), q2); // sub must not flip the NaN's sign
        assert_eq!(add::<F32>(0x3f80_0000, 0xffc0_0005, &mut e, true), 0xffc0_0005);
        assert_eq!(e.fl, 0);
        let mut e = Env::new(0x1f80);
        assert_eq!(sqrt::<F64>(0xfff0_0000_0000_0001, &mut e), 0xfff8_0000_0000_0001);
        assert_eq!(e.fl, IE);
        // widening keeps the payload, narrowing truncates it; both quiet
        let mut e = Env::new(0x1f80);
        assert_eq!(cvt_f2f::<F32, F64>(0x7fa0_0001, &mut e), 0x7ffc_0000_2000_0000);
        assert_eq!(cvt_f2f::<F64, F32>(0x7ff0_0000_0000_0001, &mut e), 0x7fc0_0000);
        assert_eq!(cvt_f2f::<F64, F32>(0xfff9_ffff_e000_0000, &mut e), 0xffcf_ffff);
    }

    #[test]
    fn compare_and_minmax_nan_semantics() {
        let (q, one) = (0x7fc0_0000u64, 0x3f80_0000u64);
        let mut e = Env::new(0x1f80);
        assert!(!cmp_pred::<F32>(q, one, 0, &mut e)); // EQ is quiet
        assert_eq!(e.fl, 0);
        assert!(cmp_pred::<F32>(q, one, 4, &mut e)); // NEQ
        assert!(!cmp_pred::<F32>(q, one, 1, &mut e)); // LT signals on QNaN
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80);
        assert!(cmp_pred::<F32>(q, one, 5, &mut e)); // NLT
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80);
        assert_eq!(comis::<F32>(q, one, false, &mut e), (true, true, true));
        assert_eq!(e.fl, 0);
        assert_eq!(comis::<F32>(q, one, true, &mut e), (true, true, true));
        assert_eq!(e.fl, IE);
        assert_eq!(comis::<F32>(one, 0x4000_0000, false, &mut e), (false, false, true));
        assert_eq!(comis::<F32>(0x8000_0000, 0, false, &mut e), (true, false, false));
        // min/max return the second operand for NaN and for +0/-0
        let mut e = Env::new(0x1f80);
        assert_eq!(minmax::<F32>(one, q, &mut e, false), q);
        assert_eq!(minmax::<F32>(0, 0x8000_0000, &mut e, false), 0x8000_0000);
        assert_eq!(minmax::<F32>(0x8000_0000, 0, &mut e, true), 0);
    }

    #[test]
    fn float_to_int_boundaries() {
        let mut e = Env::new(0x1f80);
        let d = |x: f64| x.to_bits();
        assert_eq!(f2i::<F64>(d(2147483647.0), false, &mut e), 0x7fff_ffff);
        assert_eq!(e.fl, 0);
        assert_eq!(f2i::<F64>(d(-2147483648.0), false, &mut e), 0x8000_0000);
        assert_eq!(e.fl, 0); // representable: not an overflow
        assert_eq!(f2i::<F64>(d(2147483648.0), false, &mut e), 0x8000_0000);
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80);
        assert_eq!(f2i::<F64>(d(2147483647.5), false, &mut e), 0x8000_0000); // RNE rounds up to 2^31
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80);
        assert_eq!(f2i::<F64>(d(2147483647.5), true, &mut e), 0x7fff_ffff); // truncation stays in range
        assert_eq!(e.fl, PE);
        let mut e = Env::new(0x1f80);
        assert_eq!(f2i::<F64>(d(-2147483648.5), false, &mut e), 0x8000_0000); // ties-to-even gives -2^31
        assert_eq!(e.fl, PE);
        let mut e = Env::new(0x1f80);
        assert_eq!(f2i::<F64>(d(-2147483649.0), false, &mut e), 0x8000_0000);
        assert_eq!(e.fl, IE);
        let mut e = Env::new(0x1f80 | 0x2000); // round down
        assert_eq!(f2i::<F64>(d(-0.5), false, &mut e), 0xffff_ffff);
        let mut e = Env::new(0x1f80 | 0x4000); // round up
        assert_eq!(f2i::<F64>(d(0.25), false, &mut e), 1);
        assert_eq!(f2i::<F32>(0x7fc0_0000, false, &mut e), 0x8000_0000);
    }

    #[test]
    fn rcp_rsqrt_within_spec() {
        let mut g = Rng(7);
        for _ in 0..50_000 {
            let a = g.f32() & 0x7fff_ffff;
            let x = f32::from_bits(a);
            if !x.is_normal() {
                continue;
            }
            let r = f32::from_bits(rcp32(a));
            if r != 0.0 && r.is_finite() {
                assert!(((r as f64) * (x as f64) - 1.0).abs() < 1.5 / 4096.0);
            }
            let s = f32::from_bits(rsqrt32(a));
            assert!(((s as f64) * (x as f64).sqrt() - 1.0).abs() < 1.5 / 4096.0, "rsqrt {a:x} -> {:x}", s.to_bits());
        }
    }
}
