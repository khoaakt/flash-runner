//! Inline translation of the scalar SSE/SSE2 floating-point instructions (ADDSD/SS, SUBSD/SS, MULSD/SS,
//! DIVSD/SS, SQRTSD/SS, MINSD/SS, MAXSD/SS, (U)COMISD/SS, CVTSI2SD/SS, CVTSS2SD, CVTSD2SS, CVTTSD2SI, CVTTSS2SI).
//!
//! The interpreter (`sse_fp.rs`) evaluates floating point on raw bit patterns and only uses the host FPU on a
//! fast path where host IEEE-754 arithmetic is provably bit-exact and cannot miss an exception flag:
//! round-to-nearest-even, no FTZ/DAZ, the inexact flag already sticky in MXCSR, normal operands, normal result.
//! The code generated here takes exactly that fast path (wasm's f32/f64 operations are IEEE-754 RNE on every
//! host) and nothing else: whenever a condition does not hold the instruction runs in the interpreter through
//! the ordinary fallback, which then does everything the interpreter does (NaN selection, denormals, sticky
//! flags, ...). Both branches leave the architectural state identical. XMM registers stay in `Cpu::xmm`, only the
//! low lane is read or written, the rest of the register is untouched.
//!
//! Extensions over the interpreter's own fast path that are still exact (checked against the interpreter by the
//! fuzz family `fp`):
//!  * a zero operand of ADD/SUB/MUL (and the dividend of DIV) with an exactly zero result (no flag can be raised);
//!  * the comparisons and MIN/MAX accept any operand that is not a NaN or a denormal (zeros and infinities included);
//!  * int -> double is always exact, int -> single only needs the MXCSR condition for |v| > 2^24, single -> double
//!    needs nothing but a normal or zero operand.

use super::*;
use iced_x86::Register;

/// MXCSR bits that must be: rounding control 0, FTZ 0, DAZ 0, PE (sticky inexact) 1.
const MX_MASK: u32 = 0x6000 | 0x8000 | 0x40 | 0x20;
const MX_FAST: u32 = 0x20;

fn xi(r: Register) -> u32 {
    (r as u32).wrapping_sub(Register::XMM0 as u32)
}
fn is_x(r: Register) -> bool {
    r.is_xmm() && xi(r) < 8
}

#[derive(Clone, Copy, PartialEq)]
enum Ar {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
}

/// Where the second operand comes from.
#[derive(Clone, Copy)]
enum Src {
    /// byte offset of the XMM register inside Cpu
    Reg(u32),
    /// guest address in this local (already checked readable)
    Mem(u32),
}

impl<'a> Fx<'a> {
    fn fp_xoff(&self, r: Register) -> u32 {
        self.lay.cpu_xmm + 16 * xi(r)
    }

    pub(super) fn open_blk(&mut self) -> u32 {
        let l = self.new_label();
        self.c.block(BT_EMPTY);
        self.open(l);
        l
    }

    pub(super) fn br_if_l(&mut self, l: u32) {
        let d = self.depth(l);
        self.c.br_if(d);
    }

    /// Push the bits of the source operand (i64 for 8 bytes, i32 for 4 bytes).
    fn fp_push_src(&mut self, s: Src, dbl: bool) {
        match s {
            Src::Reg(off) => {
                self.c.lget(L_CPU).mem(if dbl { I64_LOAD } else { I32_LOAD }, off);
            }
            Src::Mem(a) => {
                self.c.lget(a).mem(if dbl { I64_LOAD } else { I32_LOAD }, self.lay.guest_base);
            }
        }
    }

    /// `1` on the stack when the f64 bits in local `t` are a normal number (or, with `zero_ok`, +-0).
    fn f64_normal(&mut self, t: u32, zero_ok: bool) {
        self.c.lget(t).i64c(1).op(I64_SHL).i64c(1 << 53).op(I64_SUB).i64c(0x7fe_i64 << 53).op(I64_LT_U);
        if zero_ok {
            self.c.lget(t).i64c(1).op(I64_SHL).op(I64_EQZ).op(I32_OR);
        }
    }
    pub(super) fn f32_normal(&mut self, t: u32, zero_ok: bool) {
        self.c.lget(t).i32c(1).op(I32_SHL).i32c(1 << 24).op(I32_SUB).u32c(0xfe << 24).op(I32_LT_U);
        if zero_ok {
            self.c.lget(t).i32c(1).op(I32_SHL).op(I32_EQZ).op(I32_OR);
        }
    }
    /// `1` when the result bits in `t` have a biased exponent of at least 2 and are finite (`result_ok` of the interpreter).
    fn f64_result_ok(&mut self, t: u32) {
        self.c.lget(t).i64c(1).op(I64_SHL).i64c(2 << 53).op(I64_SUB).i64c(0x7fd_i64 << 53).op(I64_LT_U);
    }
    fn f32_result_ok(&mut self, t: u32) {
        self.c.lget(t).i32c(1).op(I32_SHL).u32c(2 << 24).op(I32_SUB).u32c(0xfd << 24).op(I32_LT_U);
    }
    /// `1` when the bits are neither a NaN nor a denormal.
    fn f64_ordered_normalish(&mut self, t: u32) {
        // m = x << 1 ; NaN: m > 0xffe0...; denormal: 0 < m < 2^53
        self.c.lget(t).i64c(1).op(I64_SHL).i64c((0xffe0_0000_0000_0000u64) as i64).op(I64_GT_U);
        self.c.lget(t).i64c(1).op(I64_SHL).i64c(1).op(I64_SUB).i64c((1i64 << 53) - 1).op(I64_LT_U).op(I32_OR);
        self.c.op(I32_EQZ);
    }
    fn f32_ordered_normalish(&mut self, t: u32) {
        self.c.lget(t).i32c(1).op(I32_SHL).u32c(0xff00_0000).op(I32_GT_U);
        self.c.lget(t).i32c(1).op(I32_SHL).i32c(1).op(I32_SUB).i32c((1 << 24) - 1).op(I32_LT_U).op(I32_OR);
        self.c.op(I32_EQZ);
    }
    /// `br_if slow` unless MXCSR is in the interpreter's fast-path configuration.
    pub(super) fn mxcsr_or_slow(&mut self, slow: u32) {
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_mxcsr).u32c(MX_MASK).op(I32_AND).u32c(MX_FAST).op(I32_NE);
        self.br_if_l(slow);
    }

    /// Resolve operand 1: an XMM register, or a memory operand (address computed now, guard emitted as a branch to `slow`).
    /// Returns None (nothing emitted) for forms that are not handled.
    fn fp_operand(&mut self, ins: &Instruction, size: u8) -> Option<(Option<u32>, Src)> {
        match ins.op1_kind() {
            OpKind::Register => {
                let s = ins.op1_register();
                if !is_x(s) {
                    return None;
                }
                Some((None, Src::Reg(self.fp_xoff(s))))
            }
            OpKind::Memory => {
                let (b, x) = (ins.memory_base(), ins.memory_index());
                if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
                    return None;
                }
                let a = self.ea(ins, true)?;
                let _ = size;
                Some((Some(a), Src::Mem(a)))
            }
            _ => None,
        }
    }

    /// `ADDSD`-style arithmetic and `SQRTSD`: dst.low = dst.low op src.
    fn fp_arith(&mut self, ins: &Instruction, ar: Ar, dbl: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return false;
        }
        let size: u8 = if dbl { 8 } else { 4 };
        let Some((addr, src)) = self.fp_operand(ins, size) else { return false };
        let doff = self.fp_xoff(d);
        let ld = if dbl { I64_LOAD } else { I32_LOAD };
        let st = if dbl { I64_STORE } else { I32_STORE };
        // operand locals: i64 scratch for doubles, i32 temps for singles
        let (ta, tb, tr) = if dbl { (L_T64, L_T64 + 1, L_T64 + 2) } else { (self.tmp(), self.tmp(), self.tmp()) };
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, size, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.c.lget(L_CPU).mem(ld, doff).lset(ta);
        self.fp_push_src(src, dbl);
        self.c.lset(tb);
        self.mxcsr_or_slow(slow);
        // operand classes
        let sqrt = ar == Ar::Sqrt;
        if !sqrt {
            if dbl {
                self.f64_normal(ta, ar != Ar::Div || true);
            } else {
                self.f32_normal(ta, true);
            }
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        let b_zero_ok = matches!(ar, Ar::Add | Ar::Sub | Ar::Mul);
        if dbl {
            self.f64_normal(tb, b_zero_ok);
        } else {
            self.f32_normal(tb, b_zero_ok);
        }
        self.c.op(I32_EQZ);
        self.br_if_l(slow);
        if sqrt {
            // positive operand only
            if dbl {
                self.c.lget(tb).i64c(63).op(I64_SHR_U).op(I64_EQZ);
            } else {
                self.c.lget(tb).i32c(31).op(I32_SHR_U).op(I32_EQZ);
            }
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        // the host operation
        if dbl {
            if !sqrt {
                self.c.lget(ta).op(F64_REINTERPRET_I64);
            }
            self.c.lget(tb).op(F64_REINTERPRET_I64);
            self.c.op(match ar {
                Ar::Add => F64_ADD,
                Ar::Sub => F64_SUB,
                Ar::Mul => F64_MUL,
                Ar::Div => F64_DIV,
                Ar::Sqrt => F64_SQRT,
            });
            self.c.op(I64_REINTERPRET_F64).lset(tr);
        } else {
            if !sqrt {
                self.c.lget(ta).op(F32_REINTERPRET_I32);
            }
            self.c.lget(tb).op(F32_REINTERPRET_I32);
            self.c.op(match ar {
                Ar::Add => F32_ADD,
                Ar::Sub => F32_SUB,
                Ar::Mul => F32_MUL,
                Ar::Div => F32_DIV,
                Ar::Sqrt => F32_SQRT,
            });
            self.c.op(I32_REINTERPRET_F32).lset(tr);
        }
        if !sqrt {
            // result: finite, exponent >= 2; or an exactly zero result that cannot have been an underflow
            if dbl {
                self.f64_result_ok(tr);
                self.c.lget(tr).i64c(1).op(I64_SHL).op(I64_EQZ);
            } else {
                self.f32_result_ok(tr);
                self.c.lget(tr).i32c(1).op(I32_SHL).op(I32_EQZ);
            }
            match ar {
                Ar::Add | Ar::Sub => {}
                Ar::Mul => {
                    // zero only if an operand was zero
                    if dbl {
                        self.c.lget(ta).i64c(1).op(I64_SHL).op(I64_EQZ).lget(tb).i64c(1).op(I64_SHL).op(I64_EQZ).op(I32_OR);
                    } else {
                        self.c.lget(ta).i32c(1).op(I32_SHL).op(I32_EQZ).lget(tb).i32c(1).op(I32_SHL).op(I32_EQZ).op(I32_OR);
                    }
                    self.c.op(I32_AND);
                }
                _ => {
                    // Div: zero only if the dividend was zero
                    if dbl {
                        self.c.lget(ta).i64c(1).op(I64_SHL).op(I64_EQZ);
                    } else {
                        self.c.lget(ta).i32c(1).op(I32_SHL).op(I32_EQZ);
                    }
                    self.c.op(I32_AND);
                }
            }
            self.c.op(I32_OR).op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.c.lget(L_CPU).lget(tr).mem(st, doff);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        true
    }

    /// COMISD/UCOMISD/COMISS/UCOMISS: ZF, PF, CF from the ordering, OF = SF = AF = 0.
    fn fp_comis(&mut self, ins: &Instruction, dbl: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return false;
        }
        let size: u8 = if dbl { 8 } else { 4 };
        let Some((addr, src)) = self.fp_operand(ins, size) else { return false };
        let doff = self.fp_xoff(d);
        let ld = if dbl { I64_LOAD } else { I32_LOAD };
        let (ta, tb) = if dbl { (L_T64, L_T64 + 1) } else { (self.tmp(), self.tmp()) };
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, size, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.c.lget(L_CPU).mem(ld, doff).lset(ta);
        self.fp_push_src(src, dbl);
        self.c.lset(tb);
        for t in [ta, tb] {
            if dbl {
                self.f64_ordered_normalish(t);
            } else {
                self.f32_ordered_normalish(t);
            }
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        // CF = a < b ; ZF = a == b (ordered operands: exactly one of CF/ZF/neither)
        if dbl {
            self.c.lget(ta).op(F64_REINTERPRET_I64).lget(tb).op(F64_REINTERPRET_I64).op(F64_LT);
            self.c.lget(ta).op(F64_REINTERPRET_I64).lget(tb).op(F64_REINTERPRET_I64).op(F64_EQ);
        } else {
            self.c.lget(ta).op(F32_REINTERPRET_I32).lget(tb).op(F32_REINTERPRET_I32).op(F32_LT);
            self.c.lget(ta).op(F32_REINTERPRET_I32).lget(tb).op(F32_REINTERPRET_I32).op(F32_EQ);
        }
        self.c.i32c(6).op(I32_SHL).op(I32_OR);
        self.rec_full_stack(None);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        // the interpreter left the flags in cpu.fl: bring them into the same representation as the fast path
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).i32c(0x8d5).op(I32_AND).lset(L_LR);
        self.c.u32c(lk(OP_FULL, 4)).lset(L_LK);
        self.close(); // done
        self.st = St::Lazy(lk(OP_FULL, 4));
        true
    }

    /// MINSD/MAXSD/MINSS/MAXSS on ordered, non-denormal operands: the second operand unless the compare holds.
    fn fp_minmax(&mut self, ins: &Instruction, dbl: bool, is_max: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return false;
        }
        let size: u8 = if dbl { 8 } else { 4 };
        let Some((addr, src)) = self.fp_operand(ins, size) else { return false };
        let doff = self.fp_xoff(d);
        let ld = if dbl { I64_LOAD } else { I32_LOAD };
        let st = if dbl { I64_STORE } else { I32_STORE };
        let (ta, tb) = if dbl { (L_T64, L_T64 + 1) } else { (self.tmp(), self.tmp()) };
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, size, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.c.lget(L_CPU).mem(ld, doff).lset(ta);
        self.fp_push_src(src, dbl);
        self.c.lset(tb);
        for t in [ta, tb] {
            if dbl {
                self.f64_ordered_normalish(t);
            } else {
                self.f32_ordered_normalish(t);
            }
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        // result = pick_a ? a : b with pick_a = (max ? b < a : a < b); ties and +-0 pairs give b
        self.c.lget(L_CPU).lget(ta).lget(tb);
        let (x, y) = if is_max { (tb, ta) } else { (ta, tb) };
        if dbl {
            self.c.lget(x).op(F64_REINTERPRET_I64).lget(y).op(F64_REINTERPRET_I64).op(F64_LT);
        } else {
            self.c.lget(x).op(F32_REINTERPRET_I32).lget(y).op(F32_REINTERPRET_I32).op(F32_LT);
        }
        self.c.op(SELECT).mem(st, doff);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        true
    }

    /// CVTSI2SD / CVTSI2SS xmm, r/m32.
    fn fp_cvtsi2f(&mut self, ins: &Instruction, ip: u32, dbl: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return false;
        }
        match ins.op1_kind() {
            OpKind::Register if ins.op1_register().is_gpr32() => {}
            OpKind::Memory => {
                let (b, x) = (ins.memory_base(), ins.memory_index());
                if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) || ins.memory_size().size() != 4 {
                    return false;
                }
            }
            _ => return false,
        }
        let doff = self.fp_xoff(d);
        let v = self.read_src(ins, 1, ip);
        if dbl {
            // an int32 is always exactly representable: no flags, no rounding
            self.c.lget(L_CPU);
            self.push_v(v);
            self.c.op(F64_CONVERT_I32_S).op(I64_REINTERPRET_F64).mem(I64_STORE, doff);
            return true;
        }
        let done = self.open_blk();
        let slow = self.open_blk();
        // |v| <= 2^24 is exact; beyond that the result is inexact and MXCSR must be in the fast configuration
        self.push_v(v);
        self.c.i32c(1 << 24).op(I32_ADD).u32c((1 << 25) + 1).op(I32_LT_U);
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_mxcsr).u32c(MX_MASK).op(I32_AND).u32c(MX_FAST).op(I32_EQ).op(I32_OR);
        self.c.op(I32_EQZ);
        self.br_if_l(slow);
        self.c.lget(L_CPU);
        self.push_v(v);
        self.c.op(F32_CONVERT_I32_S).op(I32_REINTERPRET_F32).mem(I32_STORE, doff);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        true
    }

    /// CVTSS2SD (`to_double`) / CVTSD2SS xmm, xmm/m.
    fn fp_cvt_f2f(&mut self, ins: &Instruction, to_double: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return false;
        }
        let size: u8 = if to_double { 4 } else { 8 };
        let Some((addr, src)) = self.fp_operand(ins, size) else { return false };
        let doff = self.fp_xoff(d);
        let tb = if to_double { self.tmp() } else { L_T64 + 1 };
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, size, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.fp_push_src(src, !to_double);
        self.c.lset(tb);
        if to_double {
            // widening is exact; a normal or zero operand raises nothing
            self.f32_normal(tb, true);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
            self.c.lget(L_CPU).lget(tb).op(F32_REINTERPRET_I32).op(F64_PROMOTE_F32).op(I64_REINTERPRET_F64).mem(I64_STORE, doff);
        } else {
            self.mxcsr_or_slow(slow);
            self.f64_normal(tb, true);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
            let tr = self.tmp();
            self.c.lget(tb).op(F64_REINTERPRET_I64).op(F32_DEMOTE_F64).op(I32_REINTERPRET_F32).lset(tr);
            // a normal single result, or zero from a zero operand
            self.f32_result_ok(tr);
            self.c.lget(tb).i64c(1).op(I64_SHL).op(I64_EQZ).op(I32_OR).op(I32_EQZ);
            self.br_if_l(slow);
            self.c.lget(L_CPU).lget(tr).mem(I32_STORE, doff);
        }
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        true
    }

    /// CVTTSD2SI / CVTTSS2SI r32, xmm/m.
    fn fp_cvtt2si(&mut self, ins: &Instruction, dbl: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !d.is_gpr32() {
            return false;
        }
        let size: u8 = if dbl { 8 } else { 4 };
        let Some((addr, src)) = self.fp_operand(ins, size) else { return false };
        let tb = if dbl { L_T64 + 1 } else { self.tmp() };
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, size, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.fp_push_src(src, dbl);
        self.c.lset(tb);
        self.mxcsr_or_slow(slow);
        // in range (a NaN fails both compares): truncation then cannot raise anything but the already sticky PE
        if dbl {
            self.c.lget(tb).op(F64_REINTERPRET_I64).f64c(-2147483649.0).op(F64_GT);
            self.c.lget(tb).op(F64_REINTERPRET_I64).f64c(2147483648.0).op(F64_LT).op(I32_AND);
        } else {
            self.c.lget(tb).op(F32_REINTERPRET_I32).f32c(-2147483648.0).op(F32_GE);
            self.c.lget(tb).op(F32_REINTERPRET_I32).f32c(2147483648.0).op(F32_LT).op(I32_AND);
        }
        self.c.op(I32_EQZ);
        self.br_if_l(slow);
        if dbl {
            self.c.lget(tb).op(F64_REINTERPRET_I64).op(I32_TRUNC_F64_S);
        } else {
            self.c.lget(tb).op(F32_REINTERPRET_I32).op(I32_TRUNC_F32_S);
        }
        self.rset(d);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        true
    }

    /// RCPSS / RSQRTSS xmm, xmm/m32 on a normal operand with a normal result (see `rcp32` / `rsqrt32`: computed in f64).
    fn fp_recip(&mut self, ins: &Instruction, sqrt: bool) -> bool {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return false;
        }
        let Some((addr, src)) = self.fp_operand(ins, 4) else { return false };
        let doff = self.fp_xoff(d);
        let tb = self.tmp();
        let tr = self.tmp();
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, 4, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
        }
        self.fp_push_src(src, false);
        self.c.lset(tb);
        // a normal operand (sqrt: also positive)
        self.f32_normal(tb, false);
        if sqrt {
            self.c.lget(tb).i32c(31).op(I32_SHR_U).op(I32_EQZ).op(I32_AND);
        }
        self.c.op(I32_EQZ);
        self.br_if_l(slow);
        self.c.f64c(1.0);
        self.c.lget(tb).op(F32_REINTERPRET_I32).op(F64_PROMOTE_F32);
        if sqrt {
            self.c.op(F64_SQRT);
        }
        self.c.op(F64_DIV).op(F32_DEMOTE_F64).op(I32_REINTERPRET_F32).lset(tr);
        // a flushed (zero / denormal) result takes the interpreter's special case
        self.c.lget(tr).i32c(1).op(I32_SHL).u32c(1 << 24).op(I32_LT_U);
        self.br_if_l(slow);
        self.c.lget(L_CPU).lget(tr).mem(I32_STORE, doff);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        true
    }

    /// Entry point from `ins_ext`: true if the instruction was translated.
    pub(super) fn fp_ins(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        match mn {
            M::Addsd => self.fp_arith(ins, Ar::Add, true),
            M::Subsd => self.fp_arith(ins, Ar::Sub, true),
            M::Mulsd => self.fp_arith(ins, Ar::Mul, true),
            M::Divsd => self.fp_arith(ins, Ar::Div, true),
            M::Sqrtsd => self.fp_arith(ins, Ar::Sqrt, true),
            M::Addss => self.fp_arith(ins, Ar::Add, false),
            M::Subss => self.fp_arith(ins, Ar::Sub, false),
            M::Mulss => self.fp_arith(ins, Ar::Mul, false),
            M::Divss => self.fp_arith(ins, Ar::Div, false),
            M::Sqrtss => self.fp_arith(ins, Ar::Sqrt, false),
            M::Comisd | M::Ucomisd => self.fp_comis(ins, true),
            M::Comiss | M::Ucomiss => self.fp_comis(ins, false),
            M::Minsd => self.fp_minmax(ins, true, false),
            M::Maxsd => self.fp_minmax(ins, true, true),
            M::Minss => self.fp_minmax(ins, false, false),
            M::Maxss => self.fp_minmax(ins, false, true),
            M::Rcpss => self.fp_recip(ins, false),
            M::Rsqrtss => self.fp_recip(ins, true),
            M::Cvtsi2sd => self.fp_cvtsi2f(ins, ip, true),
            M::Cvtsi2ss => self.fp_cvtsi2f(ins, ip, false),
            M::Cvtss2sd => self.fp_cvt_f2f(ins, true),
            M::Cvtsd2ss => self.fp_cvt_f2f(ins, false),
            M::Cvttsd2si => self.fp_cvtt2si(ins, true),
            M::Cvttss2si => self.fp_cvtt2si(ins, false),
            _ => false,
        }
    }
}
