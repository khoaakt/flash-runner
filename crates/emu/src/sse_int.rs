//! Pure lane helpers for MMX / SSE2 integer SIMD.
//!
//! Everything operates on a `u128` (an XMM register) as little-endian lanes. The 64-bit MMX forms
//! reuse these by zero-extending their operands and truncating the result; the few operations
//! where that is not equivalent (pack, unpack-high, PSADBW is fine, shuffles, PMOVMSKB) have
//! explicit MMX versions in `sse.rs`.

use core::mem::transmute;

#[cfg(target_endian = "big")]
compile_error!("SIMD lane helpers assume a little-endian host (aarch64 / wasm32 / x86)");

#[inline(always)]
pub fn l8(v: u128) -> [u8; 16] {
    v.to_le_bytes()
}
#[inline(always)]
pub fn l16(v: u128) -> [u16; 8] {
    unsafe { transmute(v) }
}
#[inline(always)]
pub fn l32(v: u128) -> [u32; 4] {
    unsafe { transmute(v) }
}
#[inline(always)]
pub fn l64(v: u128) -> [u64; 2] {
    unsafe { transmute(v) }
}
#[inline(always)]
pub fn f8(v: [u8; 16]) -> u128 {
    u128::from_le_bytes(v)
}
#[inline(always)]
pub fn f16(v: [u16; 8]) -> u128 {
    unsafe { transmute(v) }
}
#[inline(always)]
pub fn f32_(v: [u32; 4]) -> u128 {
    unsafe { transmute(v) }
}
#[inline(always)]
pub fn f64_(v: [u64; 2]) -> u128 {
    unsafe { transmute(v) }
}

macro_rules! map2 {
    ($dec:ident, $enc:ident, $a:expr, $b:expr, $f:expr) => {{
        let x = $dec($a);
        let y = $dec($b);
        let mut r = x;
        for i in 0..r.len() {
            r[i] = $f(x[i], y[i]);
        }
        $enc(r)
    }};
}

macro_rules! def2 {
    ($name:ident, $dec:ident, $enc:ident, $f:expr) => {
        #[inline(always)]
        pub fn $name(a: u128, b: u128) -> u128 {
            map2!($dec, $enc, a, b, $f)
        }
    };
}

// ---- add / subtract -------------------------------------------------------------------------
def2!(paddb, l8, f8, |x: u8, y: u8| x.wrapping_add(y));
def2!(paddw, l16, f16, |x: u16, y: u16| x.wrapping_add(y));
def2!(paddd, l32, f32_, |x: u32, y: u32| x.wrapping_add(y));
def2!(paddq, l64, f64_, |x: u64, y: u64| x.wrapping_add(y));
def2!(psubb, l8, f8, |x: u8, y: u8| x.wrapping_sub(y));
def2!(psubw, l16, f16, |x: u16, y: u16| x.wrapping_sub(y));
def2!(psubd, l32, f32_, |x: u32, y: u32| x.wrapping_sub(y));
def2!(psubq, l64, f64_, |x: u64, y: u64| x.wrapping_sub(y));
// saturating
def2!(paddsb, l8, f8, |x: u8, y: u8| (x as i8).saturating_add(y as i8) as u8);
def2!(paddsw, l16, f16, |x: u16, y: u16| (x as i16).saturating_add(y as i16) as u16);
def2!(paddusb, l8, f8, |x: u8, y: u8| x.saturating_add(y));
def2!(paddusw, l16, f16, |x: u16, y: u16| x.saturating_add(y));
def2!(psubsb, l8, f8, |x: u8, y: u8| (x as i8).saturating_sub(y as i8) as u8);
def2!(psubsw, l16, f16, |x: u16, y: u16| (x as i16).saturating_sub(y as i16) as u16);
def2!(psubusb, l8, f8, |x: u8, y: u8| x.saturating_sub(y));
def2!(psubusw, l16, f16, |x: u16, y: u16| x.saturating_sub(y));

// ---- multiply -------------------------------------------------------------------------------
def2!(pmullw, l16, f16, |x: u16, y: u16| (x as i16 as i32).wrapping_mul(y as i16 as i32) as u16);
def2!(pmulhw, l16, f16, |x: u16, y: u16| ((x as i16 as i32 * y as i16 as i32) >> 16) as u16);
def2!(pmulhuw, l16, f16, |x: u16, y: u16| ((x as u32 * y as u32) >> 16) as u16);
// Unsigned 32x32->64 of the even dwords of each qword.
def2!(pmuludq, l64, f64_, |x: u64, y: u64| (x as u32 as u64) * (y as u32 as u64));

#[inline(always)]
pub fn pmaddwd(a: u128, b: u128) -> u128 {
    let (x, y) = (l16(a), l16(b));
    let mut r = [0u32; 4];
    for i in 0..4 {
        let lo = (x[2 * i] as i16 as i32) * (y[2 * i] as i16 as i32);
        let hi = (x[2 * i + 1] as i16 as i32) * (y[2 * i + 1] as i16 as i32);
        r[i] = lo.wrapping_add(hi) as u32; // 0x8000*0x8000*2 wraps to 0x80000000, as on hardware
    }
    f32_(r)
}

// ---- average / min / max / sad --------------------------------------------------------------
def2!(pavgb, l8, f8, |x: u8, y: u8| ((x as u16 + y as u16 + 1) >> 1) as u8);
def2!(pavgw, l16, f16, |x: u16, y: u16| ((x as u32 + y as u32 + 1) >> 1) as u16);
def2!(pminub, l8, f8, |x: u8, y: u8| x.min(y));
def2!(pmaxub, l8, f8, |x: u8, y: u8| x.max(y));
def2!(pminsw, l16, f16, |x: u16, y: u16| (x as i16).min(y as i16) as u16);
def2!(pmaxsw, l16, f16, |x: u16, y: u16| (x as i16).max(y as i16) as u16);

/// Sum of absolute byte differences of each qword, in the low 16 bits of the result qword.
#[inline(always)]
pub fn psadbw(a: u128, b: u128) -> u128 {
    let (x, y) = (l8(a), l8(b));
    let mut s = [0u64; 2];
    for i in 0..16 {
        s[i >> 3] += x[i].abs_diff(y[i]) as u64;
    }
    f64_(s)
}

// ---- compares / logic -----------------------------------------------------------------------
def2!(pcmpeqb, l8, f8, |x: u8, y: u8| if x == y { 0xff } else { 0 });
def2!(pcmpeqw, l16, f16, |x: u16, y: u16| if x == y { 0xffff } else { 0 });
def2!(pcmpeqd, l32, f32_, |x: u32, y: u32| if x == y { !0 } else { 0 });
def2!(pcmpgtb, l8, f8, |x: u8, y: u8| if (x as i8) > (y as i8) { 0xff } else { 0 });
def2!(pcmpgtw, l16, f16, |x: u16, y: u16| if (x as i16) > (y as i16) { 0xffff } else { 0 });
def2!(pcmpgtd, l32, f32_, |x: u32, y: u32| if (x as i32) > (y as i32) { !0 } else { 0 });

// ---- shifts (count is the full 64-bit count operand) ------------------------------------------
macro_rules! shift_fn {
    ($name:ident, $dec:ident, $enc:ident, $bits:expr, |$x:ident, $c:ident| $e:expr, $over:expr) => {
        #[inline(always)]
        pub fn $name(a: u128, count: u64) -> u128 {
            if count >= $bits {
                return $over(a);
            }
            let $c = count as u32;
            let mut r = $dec(a);
            for i in 0..r.len() {
                let $x = r[i];
                r[i] = $e;
            }
            $enc(r)
        }
    };
}
shift_fn!(psllw, l16, f16, 16, |x, c| x << c, |_a| 0);
shift_fn!(pslld, l32, f32_, 32, |x, c| x << c, |_a| 0);
shift_fn!(psllq, l64, f64_, 64, |x, c| x << c, |_a| 0);
shift_fn!(psrlw, l16, f16, 16, |x, c| x >> c, |_a| 0);
shift_fn!(psrld, l32, f32_, 32, |x, c| x >> c, |_a| 0);
shift_fn!(psrlq, l64, f64_, 64, |x, c| x >> c, |_a| 0);
#[inline(always)]
pub fn psraw(a: u128, count: u64) -> u128 {
    let c = count.min(15) as u32;
    let mut r = l16(a);
    for v in r.iter_mut() {
        *v = ((*v as i16) >> c) as u16;
    }
    f16(r)
}
#[inline(always)]
pub fn psrad(a: u128, count: u64) -> u128 {
    let c = count.min(31) as u32;
    let mut r = l32(a);
    for v in r.iter_mut() {
        *v = ((*v as i32) >> c) as u32;
    }
    f32_(r)
}
/// Byte shifts of the whole register (xmm only).
#[inline(always)]
pub fn pslldq(a: u128, n: u32) -> u128 {
    if n > 15 {
        0
    } else {
        a << (8 * n)
    }
}
#[inline(always)]
pub fn psrldq(a: u128, n: u32) -> u128 {
    if n > 15 {
        0
    } else {
        a >> (8 * n)
    }
}

// ---- pack -----------------------------------------------------------------------------------
#[inline(always)]
fn sat_i8(v: i16) -> u8 {
    v.clamp(-128, 127) as i8 as u8
}
#[inline(always)]
fn sat_u8(v: i16) -> u8 {
    v.clamp(0, 255) as u8
}
#[inline(always)]
fn sat_i16(v: i32) -> u16 {
    v.clamp(-32768, 32767) as i16 as u16
}
pub fn packsswb(a: u128, b: u128) -> u128 {
    let (x, y) = (l16(a), l16(b));
    let mut r = [0u8; 16];
    for i in 0..8 {
        r[i] = sat_i8(x[i] as i16);
        r[i + 8] = sat_i8(y[i] as i16);
    }
    f8(r)
}
pub fn packuswb(a: u128, b: u128) -> u128 {
    let (x, y) = (l16(a), l16(b));
    let mut r = [0u8; 16];
    for i in 0..8 {
        r[i] = sat_u8(x[i] as i16);
        r[i + 8] = sat_u8(y[i] as i16);
    }
    f8(r)
}
pub fn packssdw(a: u128, b: u128) -> u128 {
    let (x, y) = (l32(a), l32(b));
    let mut r = [0u16; 8];
    for i in 0..4 {
        r[i] = sat_i16(x[i] as i32);
        r[i + 4] = sat_i16(y[i] as i32);
    }
    f16(r)
}

// ---- unpack / interleave --------------------------------------------------------------------
#[inline(always)]
fn unp<T: Copy, const N: usize>(a: [T; N], b: [T; N], hi: bool) -> [T; N] {
    let base = if hi { N / 2 } else { 0 };
    let mut r = a;
    for i in 0..N / 2 {
        r[2 * i] = a[base + i];
        r[2 * i + 1] = b[base + i];
    }
    r
}
pub fn punpcklbw(a: u128, b: u128) -> u128 {
    f8(unp(l8(a), l8(b), false))
}
pub fn punpckhbw(a: u128, b: u128) -> u128 {
    f8(unp(l8(a), l8(b), true))
}
pub fn punpcklwd(a: u128, b: u128) -> u128 {
    f16(unp(l16(a), l16(b), false))
}
pub fn punpckhwd(a: u128, b: u128) -> u128 {
    f16(unp(l16(a), l16(b), true))
}
pub fn punpckldq(a: u128, b: u128) -> u128 {
    f32_(unp(l32(a), l32(b), false))
}
pub fn punpckhdq(a: u128, b: u128) -> u128 {
    f32_(unp(l32(a), l32(b), true))
}
pub fn punpcklqdq(a: u128, b: u128) -> u128 {
    (a & u64::MAX as u128) | (b << 64)
}
pub fn punpckhqdq(a: u128, b: u128) -> u128 {
    (a >> 64) | (b & !(u64::MAX as u128))
}

// ---- shuffles -------------------------------------------------------------------------------
pub fn pshufd(a: u128, imm: u8) -> u128 {
    let x = l32(a);
    let i = imm as usize;
    f32_([x[i & 3], x[(i >> 2) & 3], x[(i >> 4) & 3], x[(i >> 6) & 3]])
}
pub fn pshuflw(a: u128, imm: u8) -> u128 {
    let x = l16(a);
    let i = imm as usize;
    f16([x[i & 3], x[(i >> 2) & 3], x[(i >> 4) & 3], x[(i >> 6) & 3], x[4], x[5], x[6], x[7]])
}
pub fn pshufhw(a: u128, imm: u8) -> u128 {
    let x = l16(a);
    let i = imm as usize;
    f16([x[0], x[1], x[2], x[3], x[4 + (i & 3)], x[4 + ((i >> 2) & 3)], x[4 + ((i >> 4) & 3)], x[4 + ((i >> 6) & 3)]])
}
/// MMX PSHUFW.
pub fn pshufw(a: u64, imm: u8) -> u64 {
    let i = imm as u32;
    let mut r = 0u64;
    for k in 0..4 {
        r |= ((a >> (16 * ((i >> (2 * k)) & 3))) & 0xffff) << (16 * k);
    }
    r
}

// ---- movemask -------------------------------------------------------------------------------
/// Gather the sign bit of each byte (bit i of the result = bit 7 of byte i).
#[inline(always)]
pub fn pmovmskb(a: u128) -> u32 {
    #[inline(always)]
    fn half(x: u64) -> u32 {
        (((x & 0x8080_8080_8080_8080).wrapping_mul(0x0002_0408_1020_4081)) >> 56) as u32
    }
    half(a as u64) | (half((a >> 64) as u64) << 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(s: &mut u64) -> u128 {
        let mut next = || {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            *s
        };
        ((next() as u128) << 64) | next() as u128
    }

    #[test]
    fn movemask_matches_naive() {
        let mut s = 0x1234_5678_9abc_def1u64;
        for _ in 0..10000 {
            let v = rnd(&mut s);
            let mut want = 0u32;
            for (i, b) in l8(v).iter().enumerate() {
                want |= ((b >> 7) as u32) << i;
            }
            assert_eq!(pmovmskb(v), want);
        }
    }

    #[test]
    fn pshufw_identity_and_reverse() {
        assert_eq!(pshufw(0x0004_0003_0002_0001, 0xE4), 0x0004_0003_0002_0001);
        assert_eq!(pshufw(0x0004_0003_0002_0001, 0x1B), 0x0001_0002_0003_0004);
    }

    #[test]
    fn saturation() {
        assert_eq!(l8(paddsb(f8([127; 16]), f8([1; 16])))[0], 127);
        assert_eq!(l8(psubusb(f8([1; 16]), f8([2; 16])))[0], 0);
        assert_eq!(l32(pmaddwd(f16([0x8000; 8]), f16([0x8000; 8])))[0], 0x8000_0000);
    }
}
