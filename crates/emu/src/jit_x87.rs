//! Inline translation of the common x87 data-movement instructions: FLD m32/m64, FLD1, FLDZ, FST/FSTP m32/m64,
//! FIST/FISTP m16/m32, FLDCW/FNSTCW, and the register forms FLD/FST/FSTP ST(i), FXCH, FABS, FCHS.
//! (x87 arithmetic stays in the interpreter: it needs the soft-float 80-bit core.)
//!
//! Like the other inline floating-point forms, this only takes the cases whose result and side effects are known
//! exactly and leaves everything else to the interpreter:
//!  * loads: a normal or zero source (denormals set DE, NaN/infinity have their own tags and flags) and a free
//!    register to push into (else the interpreter raises the stack fault);
//!  * stores: a non-empty ST(0) whose value is a normalised number, or zero, that converts *exactly* (no rounding, no
//!    overflow/underflow flag possible);
//!  * integer stores: any normalised value of magnitude below 2^31 (2^15 for m16) with all four rounding modes of the
//!    control word, including the PE / C1 bookkeeping; larger values (IE, integer indefinite) run in the interpreter.
//!  * register forms are exact for any register contents (they only move the 80 bits and retag them as `Fpu::tag_of`);
//!    an empty source register or a full stack (stack faults, indefinite) takes the interpreter;
//!  * FLDCW / FNSTCW just move the 16 bits to / from the control word, as the interpreter does.
//! Every executed form also does what the interpreter's `fbegin` / `fea` do (clear C1, record the last instruction and
//! data pointers) and keeps TOP, the tag word and the register contents exactly as `Fpu::push` / `pop` / `set_phys` would.

use super::*;

const C1: u32 = 0x0200;
const PE: u32 = 0x0020;

impl<'a> Fx<'a> {
    fn x87_ok_layout(&self) -> bool {
        self.lay.mm_stride == 16
    }

    fn x87_blk(&mut self) -> u32 {
        let l = self.new_label();
        self.c.block(BT_EMPTY);
        self.open(l);
        l
    }

    fn x87_br_if(&mut self, l: u32) {
        let d = self.depth(l);
        self.c.br_if(d);
    }

    /// `fbegin` (clear C1, record the instruction pointer) and, for memory forms, `fea` (record the data pointer).
    fn x87_fbegin(&mut self, ip: u32, addr: Option<u32>) {
        let sw = self.lay.cpu_fpu_sw;
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, sw).i32c(!(C1 as i32) & 0xffff).op(I32_AND).mem(I32_STORE16, sw);
        self.c.lget(L_CPU).u32c(ip).mem(I32_STORE, self.lay.cpu_fpu_last_ip);
        if let Some(a) = addr {
            self.c.lget(L_CPU).lget(a).mem(I32_STORE, self.lay.cpu_fpu_last_dp);
        }
    }

    /// Address (wasm local) of physical register `p` (local holding 0..7) inside Cpu, as a base for the register-file offsets.
    fn x87_reg_base(&mut self, p: u32) -> u32 {
        let t = self.tmp();
        self.c.lget(L_CPU).lget(p).i32c(4).op(I32_SHL).op(I32_ADD).lset(t);
        t
    }

    /// Branch to `slow` unless physical register `p` (local) is not empty (`want_empty` = false) / is empty (true).
    fn x87_tag_check(&mut self, p: u32, want_empty: bool, slow: u32) {
        self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_tag).lget(p).i32c(1).op(I32_SHL).op(I32_SHR_U).i32c(3).op(I32_AND).i32c(3).op(if want_empty { I32_NE } else { I32_EQ });
        self.x87_br_if(slow);
    }

    /// Mark register `p` empty (a pop of the current TOP) and advance TOP.
    fn x87_pop(&mut self, top: u32) {
        let tag = self.lay.cpu_fpu_tag;
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, tag).i32c(3).lget(top).i32c(1).op(I32_SHL).op(I32_SHL).op(I32_OR).mem(I32_STORE16, tag);
        self.c.lget(L_CPU).lget(top).i32c(1).op(I32_ADD).i32c(7).op(I32_AND).mem(I32_STORE8, self.lay.cpu_fpu_top);
    }

    /// Push: `np` (local) is the new TOP; mant (i64 local), se (i32 local), tag value (i32 local, 0 valid / 1 zero).
    fn x87_push_store(&mut self, np: u32, mant: u32, se: u32, tagv: u32) {
        let base = self.x87_reg_base(np);
        self.c.lget(base).lget(mant).mem(I64_STORE, self.lay.cpu_mm0);
        self.c.lget(base).lget(se).mem(I32_STORE16, self.lay.cpu_mm_se0);
        let tag = self.lay.cpu_fpu_tag;
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, tag).i32c(3).lget(np).i32c(1).op(I32_SHL).op(I32_SHL).i32c(-1).op(I32_XOR).op(I32_AND);
        self.c.lget(tagv).lget(np).i32c(1).op(I32_SHL).op(I32_SHL).op(I32_OR).mem(I32_STORE16, tag);
        self.c.lget(L_CPU).lget(np).mem(I32_STORE8, self.lay.cpu_fpu_top);
    }

    /// FLD m32 / m64, FLD1, FLDZ.
    fn x87_fld(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let mut addr = None;
        let mut size = 0u8;
        if matches!(mn, M::Fld) {
            if ins.op0_kind() != OpKind::Memory {
                return false;
            }
            size = ins.memory_size().size() as u8;
            if size != 4 && size != 8 {
                return false;
            }
            let (b, x) = (ins.memory_base(), ins.memory_index());
            if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
                return false;
            }
            let Some(a) = self.ea(ins, true) else { return false };
            addr = Some(a);
        }
        let (mant, x) = (L_T64, L_T64 + 1);
        let (se, tagv, np, top) = (self.tmp(), self.tmp(), self.tmp(), self.tmp());
        let done = self.x87_blk();
        let slow = self.x87_blk();
        if let Some(a) = addr {
            self.fast_cond(a, size, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.x87_br_if(slow);
        }
        // a free register to push into
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).lset(top);
        self.c.lget(top).i32c(7).op(I32_ADD).i32c(7).op(I32_AND).lset(np);
        self.x87_tag_check(np, true, slow);
        match mn {
            M::Fld1 => {
                self.c.i64c(i64::MIN).lset(mant);
                self.c.i32c(0x3fff).lset(se);
                self.c.i32c(0).lset(tagv);
            }
            M::Fldz => {
                self.c.i64c(0).lset(mant);
                self.c.i32c(0).lset(se);
                self.c.i32c(1).lset(tagv);
            }
            _ => {
                let a = addr.unwrap();
                if size == 8 {
                    self.c.lget(a).mem(I64_LOAD, self.lay.guest_base).lset(x);
                    // normal or zero only
                    self.c.lget(x).i64c(1).op(I64_SHL).i64c(1 << 53).op(I64_SUB).i64c(0x7fe_i64 << 53).op(I64_LT_U);
                    self.c.lget(x).i64c(1).op(I64_SHL).op(I64_EQZ).op(I32_OR).op(I32_EQZ);
                    self.x87_br_if(slow);
                    // zero?
                    self.c.lget(x).i64c(1).op(I64_SHL).op(I64_EQZ).lset(tagv); // 1 = zero
                    self.c.lget(x).i64c(48).op(I64_SHR_U).op(I32_WRAP_I64).i32c(0x8000).op(I32_AND);
                    self.c.i32c(0);
                    self.c.lget(x).i64c(52).op(I64_SHR_U).op(I32_WRAP_I64).i32c(0x7ff).op(I32_AND).i32c(15360).op(I32_ADD);
                    self.c.lget(tagv).select(); // zero: exponent field 0
                    self.c.op(I32_OR).lset(se);
                    self.c.i64c(0);
                    self.c.lget(x).i64c((1 << 52) - 1).op(I64_AND).i64c(11).op(I64_SHL).i64c(i64::MIN).op(I64_OR);
                    self.c.lget(tagv).select().lset(mant);
                } else {
                    let raw = self.tmp();
                    self.c.lget(a).mem(I32_LOAD, self.lay.guest_base).lset(raw);
                    self.c.lget(raw).i32c(1).op(I32_SHL).i32c(1 << 24).op(I32_SUB).u32c(0xfe << 24).op(I32_LT_U);
                    self.c.lget(raw).i32c(1).op(I32_SHL).op(I32_EQZ).op(I32_OR).op(I32_EQZ);
                    self.x87_br_if(slow);
                    self.c.lget(raw).i32c(1).op(I32_SHL).op(I32_EQZ).lset(tagv);
                    self.c.lget(raw).i32c(16).op(I32_SHR_U).i32c(0x8000).op(I32_AND);
                    self.c.i32c(0);
                    self.c.lget(raw).i32c(23).op(I32_SHR_U).i32c(0xff).op(I32_AND).i32c(16256).op(I32_ADD);
                    self.c.lget(tagv).select();
                    self.c.op(I32_OR).lset(se);
                    self.c.i64c(0);
                    self.c.lget(raw).i32c(0x7f_ffff).op(I32_AND).op(I64_EXTEND_I32_U).i64c(40).op(I64_SHL).i64c(i64::MIN).op(I64_OR);
                    self.c.lget(tagv).select().lset(mant);
                }
            }
        }
        self.x87_fbegin(ip, addr);
        self.x87_push_store(np, mant, se, tagv);
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        self.x87_touched();
        true
    }

    /// The x87 state changed (TOP / tags / registers): MMX knowledge no longer holds.
    fn x87_touched(&mut self) {
        self.mmx_known = false;
        self.mm_tagged = 0;
    }

    /// FST / FSTP m32 / m64 of an exactly representable ST(0).
    fn x87_fst(&mut self, ins: &Instruction, ip: u32, pop: bool) -> bool {
        if ins.op0_kind() != OpKind::Memory {
            return false;
        }
        let size = ins.memory_size().size() as u8;
        if size != 4 && size != 8 {
            return false;
        }
        let (b, x) = (ins.memory_base(), ins.memory_index());
        if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
            return false;
        }
        let Some(a) = self.ea(ins, true) else { return false };
        let (mant, bits) = (L_T64, L_T64 + 1);
        let (se, top, e15, sign, zero) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        let done = self.x87_blk();
        let slow = self.x87_blk();
        self.fast_cond(a, size, self.lay.wtab);
        self.c.op(I32_EQZ);
        self.x87_br_if(slow);
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).lset(top);
        self.x87_tag_check(top, false, slow);
        let base = self.x87_reg_base(top);
        self.c.lget(base).mem(I64_LOAD, self.lay.cpu_mm0).lset(mant);
        self.c.lget(base).mem(I32_LOAD16_U, self.lay.cpu_mm_se0).lset(se);
        self.c.lget(se).i32c(0x7fff).op(I32_AND).lset(e15);
        self.c.lget(se).i32c(15).op(I32_SHR_U).lset(sign);
        // zero: exponent 0 and mantissa 0
        self.c.lget(e15).op(I32_EQZ).lget(mant).op(I64_EQZ).op(I32_AND).lset(zero);
        // otherwise a normalised value with an exponent (and trailing mantissa bits) that fit the destination exactly
        let (lo, n, low_zero_bits): (i32, i32, u32) = if size == 8 { (15361, 2046, 11) } else { (16257, 254, 40) };
        self.c.lget(e15).i32c(lo).op(I32_SUB).i32c(n).op(I32_LT_U);
        self.c.lget(mant).i64c(0).op(I64_LT_S).op(I32_AND);
        self.c.lget(mant).i64c((1i64 << low_zero_bits) - 1).op(I64_AND).op(I64_EQZ).op(I32_AND);
        self.c.lget(zero).op(I32_OR).op(I32_EQZ);
        self.x87_br_if(slow);
        // the converted bits
        if size == 8 {
            self.c.lget(sign).op(I64_EXTEND_I32_U).i64c(63).op(I64_SHL);
            self.c.i64c(0);
            self.c.lget(e15).i32c(15360).op(I32_SUB).op(I64_EXTEND_I32_U).i64c(52).op(I64_SHL);
            self.c.lget(mant).i64c(11).op(I64_SHR_U).i64c((1 << 52) - 1).op(I64_AND).op(I64_OR);
            self.c.lget(zero).select(); // zero: just the sign
            self.c.op(I64_OR).lset(bits);
            self.c.lget(a).lget(bits).mem(I64_STORE, self.lay.guest_base);
        } else {
            self.c.lget(sign).i32c(31).op(I32_SHL);
            self.c.i32c(0);
            self.c.lget(e15).i32c(16256).op(I32_SUB).i32c(23).op(I32_SHL);
            self.c.lget(mant).i64c(40).op(I64_SHR_U).op(I32_WRAP_I64).i32c(0x7f_ffff).op(I32_AND).op(I32_OR);
            self.c.lget(zero).select();
            self.c.op(I32_OR);
            let tb = self.tmp();
            self.c.lset(tb);
            self.c.lget(a).lget(tb).mem(I32_STORE, self.lay.guest_base);
        }
        self.x87_fbegin(ip, Some(a));
        if pop {
            self.x87_pop(top);
        }
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        self.x87_touched();
        true
    }

    /// FIST / FISTP m16 / m32 (all rounding modes; |value| < 2^31 / 2^15 or the interpreter).
    fn x87_fist(&mut self, ins: &Instruction, ip: u32, pop: bool) -> bool {
        if ins.op0_kind() != OpKind::Memory {
            return false;
        }
        let size = ins.memory_size().size() as u8;
        if size != 4 && size != 2 {
            return false;
        }
        let (b, x) = (ins.memory_base(), ins.memory_index());
        if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
            return false;
        }
        let Some(a) = self.ea(ins, true) else { return false };
        let (mant, ip_, frac) = (L_T64, L_T64 + 1, L_T64 + 2);
        let (se, top, e15, sign, inexact, gt, eq, inc, k, rc) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        let done = self.x87_blk();
        let slow = self.x87_blk();
        self.fast_cond(a, size, self.lay.wtab);
        self.c.op(I32_EQZ);
        self.x87_br_if(slow);
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).lset(top);
        self.x87_tag_check(top, false, slow);
        let base = self.x87_reg_base(top);
        self.c.lget(base).mem(I64_LOAD, self.lay.cpu_mm0).lset(mant);
        self.c.lget(base).mem(I32_LOAD16_U, self.lay.cpu_mm_se0).lset(se);
        self.c.lget(se).i32c(0x7fff).op(I32_AND).lset(e15);
        self.c.lget(se).i32c(15).op(I32_SHR_U).lset(sign);
        // classes: zero, or normalised (exponent field 1..0x7ffe, integer bit set)
        let zero = self.tmp();
        self.c.lget(e15).op(I32_EQZ).lget(mant).op(I64_EQZ).op(I32_AND).lset(zero);
        self.c.lget(e15).i32c(1).op(I32_SUB).i32c(0x7ffe).op(I32_LT_U).lget(mant).i64c(0).op(I64_LT_S).op(I32_AND).lget(zero).op(I32_OR).op(I32_EQZ);
        self.x87_br_if(slow);
        // magnitude below 2^31: unbiased exponent at most 30
        self.c.lget(zero).op(I32_EQZ);
        self.c.lget(e15).i32c(16383 + 31).op(I32_GE_U).op(I32_AND);
        self.x87_br_if(slow);
        // rounding
        self.c.i64c(0).lset(ip_);
        self.c.i32c(0).lset(inexact).i32c(0).lset(gt).i32c(0).lset(eq);
        self.c.lget(zero).op(I32_EQZ);
        self.if_(BT_EMPTY);
        // k = 63 - e: how many low bits of the significand are fractional
        self.c.i32c(63 + 16383).lget(e15).op(I32_SUB).lset(k);
        self.c.lget(k).i32c(65).op(I32_GE_S);
        self.if_(BT_EMPTY);
        self.c.i32c(1).lset(inexact); // |v| < 0.5: below the halfway point
        self.else_();
        self.c.lget(k).i32c(64).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.c.i32c(1).lset(inexact);
        self.c.lget(mant).i64c(i64::MIN).op(I64_EQ).lset(eq);
        self.c.lget(eq).op(I32_EQZ).lset(gt);
        self.else_();
        self.c.lget(mant).lget(k).op(I64_EXTEND_I32_U).op(I64_SHR_U).lset(ip_);
        self.c.lget(mant).i64c(1).lget(k).op(I64_EXTEND_I32_U).op(I64_SHL).i64c(1).op(I64_SUB).op(I64_AND).lset(frac);
        self.c.lget(frac).op(I64_EQZ).op(I32_EQZ).lset(inexact);
        let half = self.c.len(); // (unused marker)
        let _ = half;
        self.c.lget(frac).i64c(1).lget(k).i32c(1).op(I32_SUB).op(I64_EXTEND_I32_U).op(I64_SHL).op(I64_GT_U).lset(gt);
        self.c.lget(frac).i64c(1).lget(k).i32c(1).op(I32_SUB).op(I64_EXTEND_I32_U).op(I64_SHL).op(I64_EQ).lset(eq);
        self.close();
        self.close();
        self.close();
        // increment decision by rounding control
        self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_cw).i32c(10).op(I32_SHR_U).i32c(3).op(I32_AND).lset(rc);
        // nearest even: gt | (eq & ip odd) ; down: sign & inexact ; up: !sign & inexact ; toward zero: 0
        self.c.i32c(0).lset(inc);
        self.c.lget(rc).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.c.lget(gt).lget(eq).lget(ip_).op(I32_WRAP_I64).i32c(1).op(I32_AND).op(I32_AND).op(I32_OR).lset(inc);
        self.else_();
        self.c.lget(rc).i32c(1).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.c.lget(sign).lget(inexact).op(I32_AND).lset(inc);
        self.else_();
        self.c.lget(rc).i32c(2).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.c.lget(sign).op(I32_EQZ).lget(inexact).op(I32_AND).lset(inc);
        self.close();
        self.close();
        self.close();
        // the magnitude and its range
        let mag = L_T64 + 3;
        self.c.lget(ip_).lget(inc).op(I64_EXTEND_I32_U).op(I64_ADD).lset(mag);
        let limit: i64 = if size == 4 { 1 << 31 } else { 1 << 15 };
        self.c.lget(sign);
        self.if_(I32);
        self.c.lget(mag).i64c(limit).op(I64_GT_U);
        self.else_();
        self.c.lget(mag).i64c(limit).op(I64_GE_U);
        self.close();
        self.x87_br_if(slow);
        // store the signed integer
        self.c.lget(a);
        self.c.i64c(0).lget(mag).op(I64_SUB).lget(mag).lget(sign).select().op(I32_WRAP_I64);
        self.c.mem(if size == 4 { I32_STORE } else { I32_STORE16 }, self.lay.guest_base);
        self.x87_fbegin(ip, Some(a));
        // flags: PE when inexact, C1 when rounded up
        self.c.lget(inexact);
        self.if_(BT_EMPTY);
        let sw = self.lay.cpu_fpu_sw;
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, sw).i32c(PE as i32).op(I32_OR).lget(inc).i32c(9).op(I32_SHL).op(I32_OR).mem(I32_STORE16, sw);
        self.close();
        if pop {
            self.x87_pop(top);
        }
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        self.x87_touched();
        true
    }

    /// Tag of the 80-bit value (`mant` i64 local, `se` i32 local), as `Fpu::tag_of`: 0 valid, 1 zero, 2 special. Result in a new local.
    /// Branch-free arithmetic: a chain of constant-operand `select`s is miscompiled by wasmi (the engine of the native tests).
    fn x87_tag_of(&mut self, mant: u32, se: u32) -> u32 {
        let (e, nrm, zdn, t) = (self.tmp(), self.tmp(), self.tmp(), self.tmp());
        self.c.lget(se).i32c(0x7fff).op(I32_AND).lset(e);
        // e not 0 / 0x7fff: integer bit set -> 0, else 2 (unnormal / pseudo-denormal)
        self.c.lget(mant).i64c(63).op(I64_SHR_U).op(I32_WRAP_I64).i32c(1).op(I32_XOR).i32c(1).op(I32_SHL).lset(nrm);
        // e == 0: mant == 0 -> 1 (zero), else 2 (denormal)
        self.c.i32c(2).lget(mant).op(I64_EQZ).op(I32_SUB).lset(zdn);
        // r = e == 0 ? zdn : nrm
        self.c.lget(nrm).lget(zdn).lget(nrm).op(I32_XOR).i32c(0).lget(e).op(I32_EQZ).op(I32_SUB).op(I32_AND).op(I32_XOR);
        // e == 0x7fff: special
        self.c.lget(e).i32c(0x7fff).op(I32_EQ).i32c(1).op(I32_SHL).op(I32_OR).lset(t);
        t
    }

    /// Write register `p` (local 0..7) in place (`Fpu::set_phys`): value and tag, TOP untouched.
    fn x87_write_reg(&mut self, p: u32, mant: u32, se: u32, tagv: u32) {
        let base = self.x87_reg_base(p);
        self.c.lget(base).lget(mant).mem(I64_STORE, self.lay.cpu_mm0);
        self.c.lget(base).lget(se).mem(I32_STORE16, self.lay.cpu_mm_se0);
        let tag = self.lay.cpu_fpu_tag;
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, tag).i32c(3).lget(p).i32c(1).op(I32_SHL).op(I32_SHL).i32c(-1).op(I32_XOR).op(I32_AND);
        self.c.lget(tagv).lget(p).i32c(1).op(I32_SHL).op(I32_SHL).op(I32_OR).mem(I32_STORE16, tag);
    }

    /// Read register `p` (local 0..7) into (`mant`, `se`).
    fn x87_read_reg(&mut self, p: u32, mant: u32, se: u32) {
        let base = self.x87_reg_base(p);
        self.c.lget(base).mem(I64_LOAD, self.lay.cpu_mm0).lset(mant);
        self.c.lget(base).mem(I32_LOAD16_U, self.lay.cpu_mm_se0).lset(se);
    }

    /// `top + i` mod 8 in a new local.
    fn x87_phys_i(&mut self, top: u32, i: i32) -> u32 {
        let p = self.tmp();
        self.c.lget(top).i32c(i).op(I32_ADD).i32c(7).op(I32_AND).lset(p);
        p
    }

    /// Index i of the ST(i) operand of an x87 register-form instruction (the last register operand, as the interpreter's `src_reg`).
    fn x87_sti_index(ins: &Instruction, op: u32) -> Option<i32> {
        let r = ins.op_register(op);
        if r >= Register::ST0 && r <= Register::ST7 {
            Some((r as i32) - (Register::ST0 as i32))
        } else {
            None
        }
    }

    /// FLDCW m16 / FNSTCW m16: the interpreter just moves the 16 bits (no stack, tag, flag or pointer effects).
    fn x87_cw(&mut self, ins: &Instruction, ip: u32, store: bool) -> bool {
        if ins.op0_kind() != OpKind::Memory || ins.memory_size().size() != 2 {
            return false;
        }
        let (b, x) = (ins.memory_base(), ins.memory_index());
        if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
            return false;
        }
        let Some(a) = self.ea(ins, true) else { return false };
        let cw = self.lay.cpu_fpu_cw;
        if store {
            let v = self.tmp();
            self.c.lget(L_CPU).mem(I32_LOAD16_U, cw).lset(v);
            self.store_mem(a, V::L(v), 2, ip);
        } else {
            self.load_mem(a, 2, ip);
            let v = self.tmp();
            self.c.lset(v);
            self.c.lget(L_CPU).lget(v).mem(I32_STORE16, cw);
        }
        true
    }

    /// FLD ST(i) / FXCH ST(i) / FST(P) ST(i) / FABS / FCHS: pure register-stack moves, exactly as `Fpu::rd/wr/push/pop`.
    /// An empty source register or a full stack (stack faults) takes the interpreter.
    fn x87_reg_form(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let i = match mn {
            M::Fld | M::Fst | M::Fstp => match Self::x87_sti_index(ins, 0) {
                Some(i) => i,
                None => return false,
            },
            M::Fxch => match Self::x87_sti_index(ins, if ins.op_count() >= 2 { 1 } else { 0 }) {
                Some(i) => i,
                None => return false,
            },
            M::Fabs | M::Fchs => 0,
            _ => return false,
        };
        let (m0, mi) = (L_T64, L_T64 + 1);
        let (s0, si, top) = (self.tmp(), self.tmp(), self.tmp());
        let done = self.x87_blk();
        let slow = self.x87_blk();
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).lset(top);
        match mn {
            M::Fld => {
                let pi = self.x87_phys_i(top, i);
                self.x87_tag_check(pi, false, slow);
                let np = self.x87_phys_i(top, 7);
                self.x87_tag_check(np, true, slow);
                self.x87_read_reg(pi, m0, s0);
                let t = self.x87_tag_of(m0, s0);
                self.x87_fbegin(ip, None);
                self.x87_push_store(np, m0, s0, t);
            }
            M::Fst | M::Fstp => {
                self.x87_tag_check(top, false, slow);
                self.x87_read_reg(top, m0, s0);
                let t = self.x87_tag_of(m0, s0);
                let pd = self.x87_phys_i(top, i);
                self.x87_fbegin(ip, None);
                self.x87_write_reg(pd, m0, s0, t);
                if mn == M::Fstp {
                    self.x87_pop(top);
                }
            }
            M::Fxch => {
                let pi = self.x87_phys_i(top, i);
                self.x87_tag_check(top, false, slow);
                self.x87_tag_check(pi, false, slow);
                self.x87_read_reg(top, m0, s0);
                self.x87_read_reg(pi, mi, si);
                let (t0, ti) = (self.x87_tag_of(m0, s0), self.x87_tag_of(mi, si));
                self.x87_fbegin(ip, None);
                self.x87_write_reg(top, mi, si, ti);
                self.x87_write_reg(pi, m0, s0, t0);
            }
            _ => {
                // FABS / FCHS: only the sign bit of the exponent word changes
                self.x87_tag_check(top, false, slow);
                self.x87_read_reg(top, m0, s0);
                if mn == M::Fabs {
                    self.c.lget(s0).i32c(0x7fff).op(I32_AND).lset(s0);
                } else {
                    self.c.lget(s0).i32c(0x8000).op(I32_XOR).lset(s0);
                }
                let t = self.x87_tag_of(m0, s0);
                self.x87_fbegin(ip, None);
                self.x87_write_reg(top, m0, s0, t);
            }
        }
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        self.x87_touched();
        true
    }

    // ---- FADD / FSUB / FSUBR / FMUL: exact for normal operands and a normal, in-range result ----------------------------------

    /// Product of two normal significands: returns (sign, exp, hi, lo) locals with hi:lo normalised (bit 63 of hi set),
    /// as `Fpu::mul_vals`.
    fn x87_mul_core(&mut self, ma: u32, mb: u32, sa: u32, sb: u32, ea: u32, eb: u32) -> (u32, u32, u32, u32) {
        let (rs, re) = (self.tmp(), self.tmp());
        let (a0, a1, b0, b1) = (L_T64 + 2, L_T64 + 3, L_T64 + 4, L_T64 + 5);
        let (p00, p01, p10, p11) = (L_T64 + 6, L_T64 + 7, L_T64 + 8, L_T64 + 9);
        let (mid, rh, rl) = (L_T64 + 10, L_T64 + 11, L_T64 + 12);
        self.c.lget(sa).lget(sb).op(I32_XOR).lset(rs);
        self.c.lget(ea).lget(eb).op(I32_ADD).lset(re);
        self.c.lget(ma).i64c(0xffff_ffff).op(I64_AND).lset(a0);
        self.c.lget(ma).i64c(32).op(I64_SHR_U).lset(a1);
        self.c.lget(mb).i64c(0xffff_ffff).op(I64_AND).lset(b0);
        self.c.lget(mb).i64c(32).op(I64_SHR_U).lset(b1);
        self.c.lget(a0).lget(b0).op(I64_MUL).lset(p00);
        self.c.lget(a0).lget(b1).op(I64_MUL).lset(p01);
        self.c.lget(a1).lget(b0).op(I64_MUL).lset(p10);
        self.c.lget(a1).lget(b1).op(I64_MUL).lset(p11);
        // mid = (p00 >> 32) + (p01 & m32) + (p10 & m32)
        self.c.lget(p00).i64c(32).op(I64_SHR_U).lget(p01).i64c(0xffff_ffff).op(I64_AND).op(I64_ADD).lget(p10).i64c(0xffff_ffff).op(I64_AND).op(I64_ADD).lset(mid);
        // lo = (p00 & m32) | (mid << 32); hi = p11 + (p01 >> 32) + (p10 >> 32) + (mid >> 32)
        self.c.lget(p00).i64c(0xffff_ffff).op(I64_AND).lget(mid).i64c(32).op(I64_SHL).op(I64_OR).lset(rl);
        self.c.lget(p11).lget(p01).i64c(32).op(I64_SHR_U).op(I64_ADD).lget(p10).i64c(32).op(I64_SHR_U).op(I64_ADD).lget(mid).i64c(32).op(I64_SHR_U).op(I64_ADD).lset(rh);
        // normalise: the product of two significands in [2^63, 2^64) is in [2^126, 2^128)
        self.c.lget(rh).i64c(0).op(I64_LT_S);
        self.if_(BT_EMPTY);
        self.c.lget(re).i32c(1).op(I32_ADD).lset(re);
        self.else_();
        self.c.lget(rh).i64c(1).op(I64_SHL).lget(rl).i64c(63).op(I64_SHR_U).op(I64_OR).lset(rh);
        self.c.lget(rl).i64c(1).op(I64_SHL).lset(rl);
        self.close();
        (rs, re, rh, rl)
    }

    /// x + y (y's sign already flipped for a subtraction) for normal operands, as `Fpu::add_fin`. An exact zero result leaves
    /// through `slow`.
    fn x87_add_core(&mut self, xm: u32, xs: u32, xe: u32, ym: u32, ys: u32, ye: u32, slow: u32) -> (u32, u32, u32, u32) {
        let (m1, m2) = (L_T64 + 2, L_T64 + 3);
        let (bh, bl, gh, gl) = (L_T64 + 4, L_T64 + 5, L_T64 + 6, L_T64 + 7);
        let (sh, sl, rh, rl) = (L_T64 + 8, L_T64 + 9, L_T64 + 10, L_T64 + 11);
        let (s1, e1, s2, e2, swap, d, lz, k) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        let re = self.tmp();
        // order by magnitude: the larger operand first
        self.c.lget(xe).lget(ye).op(I32_NE);
        self.if_(I32);
        self.c.lget(xe).lget(ye).op(I32_LT_S);
        self.else_();
        self.c.lget(xm).lget(ym).op(I64_LT_U);
        self.close();
        self.c.lset(swap);
        self.c.lget(swap);
        self.if_(BT_EMPTY);
        self.c.lget(ym).lset(m1).lget(ys).lset(s1).lget(ye).lset(e1);
        self.c.lget(xm).lset(m2).lget(xs).lset(s2).lget(xe).lset(e2);
        self.else_();
        self.c.lget(xm).lset(m1).lget(xs).lset(s1).lget(xe).lset(e1);
        self.c.lget(ym).lset(m2).lget(ys).lset(s2).lget(ye).lset(e2);
        self.close();
        self.c.lget(e1).lget(e2).op(I32_SUB).lset(d);
        // 63 guard bits below the significands: big = m1 << 63, src = m2 << 63
        self.c.lget(m1).i64c(1).op(I64_SHR_U).lset(bh);
        self.c.lget(m1).i64c(63).op(I64_SHL).lset(bl);
        self.c.lget(m2).i64c(1).op(I64_SHR_U).lset(gh);
        self.c.lget(m2).i64c(63).op(I64_SHL).lset(gl);
        // small = shr_jam128(src, d)
        self.c.lget(d).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.c.lget(gh).lset(sh).lget(gl).lset(sl);
        self.else_();
        self.c.lget(d).i32c(64).op(I32_LT_U);
        self.if_(BT_EMPTY);
        // 1..=63: the only bit of gl is bit 63, which stays inside, so nothing is shifted out below
        self.c.lget(gl).lget(d).op(I64_EXTEND_I32_U).op(I64_SHR_U).lget(gh).i64c(64).lget(d).op(I64_EXTEND_I32_U).op(I64_SUB).op(I64_SHL).op(I64_OR).lset(sl);
        self.c.lget(gh).lget(d).op(I64_EXTEND_I32_U).op(I64_SHR_U).lset(sh);
        self.else_();
        self.c.lget(d).i32c(128).op(I32_LT_U);
        self.if_(BT_EMPTY);
        // 64..=127: sl = gh >> k with the sticky bit of everything shifted out
        self.c.lget(d).i32c(64).op(I32_SUB).lset(k);
        self.c.lget(gh).lget(k).op(I64_EXTEND_I32_U).op(I64_SHR_U);
        self.c.lget(gl).i64c(0).op(I64_NE);
        self.c.lget(gh).i64c(1).lget(k).op(I64_EXTEND_I32_U).op(I64_SHL).i64c(1).op(I64_SUB).op(I64_AND).i64c(0).op(I64_NE).op(I32_OR);
        self.op_i64_or_flag();
        self.c.lset(sl);
        self.c.i64c(0).lset(sh);
        self.else_();
        self.c.i64c(0).lset(sh).i64c(1).lset(sl);
        self.close();
        self.close();
        self.close();
        // sum or difference
        self.c.lget(s1).lget(s2).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.c.lget(bl).lget(sl).op(I64_ADD).lset(rl);
        self.c.lget(bh).lget(sh).op(I64_ADD).lget(rl).lget(bl).op(I64_LT_U).op(I64_EXTEND_I32_U).op(I64_ADD).lset(rh);
        self.else_();
        self.c.lget(bl).lget(sl).op(I64_SUB).lset(rl);
        self.c.lget(bh).lget(sh).op(I64_SUB).lget(bl).lget(sl).op(I64_LT_U).op(I64_EXTEND_I32_U).op(I64_SUB).lset(rh);
        self.c.lget(rh).lget(rl).op(I64_OR).op(I64_EQZ);
        self.x87_br_if(slow); // exact cancellation: a signed zero, left to the interpreter
        self.close();
        // normalise
        self.c.lget(rh).i64c(0).op(I64_NE);
        self.if_(I32);
        self.c.lget(rh).op(I64_CLZ).op(I32_WRAP_I64);
        self.else_();
        self.c.lget(rl).op(I64_CLZ).op(I32_WRAP_I64).i32c(64).op(I32_ADD);
        self.close();
        self.c.lset(lz);
        self.c.lget(lz).i32c(64).op(I32_LT_U);
        self.if_(BT_EMPTY);
        self.c.lget(lz);
        self.if_(BT_EMPTY);
        self.c.lget(rh).lget(lz).op(I64_EXTEND_I32_U).op(I64_SHL).lget(rl).i64c(64).lget(lz).op(I64_EXTEND_I32_U).op(I64_SUB).op(I64_SHR_U).op(I64_OR).lset(rh);
        self.c.lget(rl).lget(lz).op(I64_EXTEND_I32_U).op(I64_SHL).lset(rl);
        self.close();
        self.else_();
        self.c.lget(rl).lget(lz).i32c(64).op(I32_SUB).op(I64_EXTEND_I32_U).op(I64_SHL).lset(rh);
        self.c.i64c(0).lset(rl);
        self.close();
        self.c.lget(e1).i32c(1).op(I32_ADD).lget(lz).op(I32_SUB).lset(re);
        (s1, re, rh, rl)
    }

    /// Stack: [i64 value, i32 flag] -> i64 value | flag (the flag becomes bit 0).
    fn op_i64_or_flag(&mut self) {
        self.c.op(I64_EXTEND_I32_U).op(I64_OR);
    }

    /// Round the normalised significand (`sig`:`low`, exponent `e`, sign `sign`) to the precision and rounding mode of the
    /// control word as `Fpu::round_core` / `round_pack`. Returns the rounded significand (an i64 local); the exponent, `inexact` and
    /// `inc` (rounded up) are written to the i32 locals given.
    /// Results outside the normal range of the format (including a tiny pre-rounding exponent) leave through `slow`.
    #[allow(clippy::too_many_arguments)]
    fn x87_round(&mut self, sign: u32, e: u32, sig: u32, low: u32, re: u32, inexact: u32, inc: u32, slow: u32) -> u32 {
        let (shift, rc, cw) = (self.tmp(), self.tmp(), self.tmp());
        let (gt, eq, lsb) = (self.tmp(), self.tmp(), self.tmp());
        let (mask, dh, hh, hl, sn) = (L_T64 + 13, L_T64 + 14, L_T64 + 2, L_T64 + 3, L_T64 + 15);
        self.c.lget(e).i32c(-16382).op(I32_LT_S);
        self.x87_br_if(slow); // tiny before rounding
        self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_cw).lset(cw);
        // significand width 24 / 53 / 64 -> bits to drop 40 / 11 / 0
        self.c.lget(cw).i32c(8).op(I32_SHR_U).i32c(3).op(I32_AND).lset(rc);
        self.c.lget(rc).op(I32_EQZ);
        self.if_(I32);
        self.c.i32c(40);
        self.else_();
        self.c.lget(rc).i32c(2).op(I32_EQ);
        self.if_(I32);
        self.c.i32c(11);
        self.else_();
        self.c.i32c(0);
        self.close();
        self.close();
        self.c.lset(shift);
        self.c.lget(cw).i32c(10).op(I32_SHR_U).i32c(3).op(I32_AND).lset(rc);
        self.c.i64c(1).lget(shift).op(I64_EXTEND_I32_U).op(I64_SHL).i64c(1).op(I64_SUB).lset(mask);
        self.c.lget(sig).lget(mask).op(I64_AND).lset(dh);
        self.c.lget(dh).lget(low).op(I64_OR).i64c(0).op(I64_NE).lset(inexact);
        // half = 2^(63 + shift) in (hi, lo) halves of the dropped 128-bit value
        self.c.i64c(1).lget(shift).op(I64_EXTEND_I32_U).op(I64_SHL).i64c(1).op(I64_SHR_U).lset(hh);
        self.c.lget(shift).op(I32_EQZ).op(I64_EXTEND_I32_U).i64c(63).op(I64_SHL).lset(hl);
        self.c.lget(dh).lget(hh).op(I64_GT_U).lget(dh).lget(hh).op(I64_EQ).lget(low).lget(hl).op(I64_GT_U).op(I32_AND).op(I32_OR).lset(gt);
        self.c.lget(dh).lget(hh).op(I64_EQ).lget(low).lget(hl).op(I64_EQ).op(I32_AND).lset(eq);
        self.c.lget(sig).lget(shift).op(I64_EXTEND_I32_U).op(I64_SHR_U).op(I32_WRAP_I64).i32c(1).op(I32_AND).lset(lsb);
        // round up?  0: nearest even, 1: down, 2: up, 3: toward zero
        self.c.lget(rc).op(I32_EQZ);
        self.if_(I32);
        self.c.lget(gt).lget(eq).lget(lsb).op(I32_AND).op(I32_OR);
        self.else_();
        self.c.lget(rc).i32c(1).op(I32_EQ);
        self.if_(I32);
        self.c.lget(sign).lget(inexact).op(I32_AND);
        self.else_();
        self.c.lget(rc).i32c(2).op(I32_EQ);
        self.if_(I32);
        self.c.lget(sign).op(I32_EQZ).lget(inexact).op(I32_AND);
        self.else_();
        self.c.i32c(0);
        self.close();
        self.close();
        self.close();
        self.c.lset(inc);
        // kept bits, incremented in the last kept place; a carry out of the significand renormalises
        self.c.lget(sig).lget(mask).i64c(-1).op(I64_XOR).op(I64_AND).lset(sn);
        self.c.lget(e).lset(re);
        self.c.lget(inc);
        self.if_(BT_EMPTY);
        self.c.lget(sn).i64c(1).lget(shift).op(I64_EXTEND_I32_U).op(I64_SHL).op(I64_ADD).lset(sn);
        self.c.lget(sn).op(I64_EQZ);
        self.if_(BT_EMPTY);
        self.c.i64c(i64::MIN).lset(sn);
        self.c.lget(re).i32c(1).op(I32_ADD).lset(re);
        self.close();
        self.close();
        self.c.lget(re).i32c(16383).op(I32_GT_S);
        self.x87_br_if(slow); // overflow
        sn
    }

    /// FADD / FSUB / FSUBR / FMUL and their popping forms: register-register and m32 / m64 operands.
    fn x87_arith(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let (op, pop) = match mn {
            M::Fadd => (0u8, false),
            M::Faddp => (0, true),
            M::Fsub => (1, false),
            M::Fsubp => (1, true),
            M::Fsubr => (2, false),
            M::Fsubrp => (2, true),
            M::Fmul => (3, false),
            M::Fmulp => (3, true),
            _ => return false,
        };
        let mem_form = !pop && ins.op0_kind() == OpKind::Memory;
        let (mut addr, mut msize) = (None, 0u8);
        let (di, si);
        if mem_form {
            msize = ins.memory_size().size() as u8;
            if msize != 4 && msize != 8 {
                return false;
            }
            let (b, x) = (ins.memory_base(), ins.memory_index());
            if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
                return false;
            }
            let Some(a) = self.ea(ins, true) else { return false };
            addr = Some(a);
            di = 0;
            si = 0;
        } else {
            let Some(d0) = Self::x87_sti_index(ins, 0) else { return false };
            di = d0;
            si = if pop && ins.op_count() < 2 {
                0
            } else {
                match Self::x87_sti_index(ins, 1) {
                    Some(i) => i,
                    None => return false,
                }
            };
        }
        let (ma, mb) = (L_T64, L_T64 + 1);
        let (top, sea, seb) = (self.tmp(), self.tmp(), self.tmp());
        let done = self.x87_blk();
        let slow = self.x87_blk();
        if let Some(a) = addr {
            self.fast_cond(a, msize, self.lay.rtab);
            self.c.op(I32_EQZ);
            self.x87_br_if(slow);
        }
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).lset(top);
        let pd = self.x87_phys_i(top, di as i32);
        self.x87_tag_check(pd, false, slow);
        self.x87_read_reg(pd, ma, sea);
        if let Some(a) = addr {
            if msize == 8 {
                // double: biased exponent 1..=0x7fe (zeros, denormals, infinities and NaNs take the interpreter)
                let bits = L_T64 + 2;
                self.c.lget(a).mem(I64_LOAD, self.lay.guest_base).lset(bits);
                let be = self.tmp();
                self.c.lget(bits).i64c(52).op(I64_SHR_U).op(I32_WRAP_I64).i32c(0x7ff).op(I32_AND).lset(be);
                self.c.lget(be).i32c(1).op(I32_SUB).i32c(0x7fe).op(I32_GE_U);
                self.x87_br_if(slow);
                self.c.i64c(i64::MIN).lget(bits).i64c(0xf_ffff_ffff_ffff).op(I64_AND).i64c(11).op(I64_SHL).op(I64_OR).lset(mb);
                self.c.lget(be).i32c(1023 - 16383).op(I32_SUB).lget(bits).i64c(63).op(I64_SHR_U).op(I32_WRAP_I64).i32c(15).op(I32_SHL).op(I32_OR).lset(seb);
            } else {
                let bits = self.tmp();
                self.c.lget(a).mem(I32_LOAD, self.lay.guest_base).lset(bits);
                let be = self.tmp();
                self.c.lget(bits).i32c(23).op(I32_SHR_U).i32c(0xff).op(I32_AND).lset(be);
                self.c.lget(be).i32c(1).op(I32_SUB).i32c(0xfe).op(I32_GE_U);
                self.x87_br_if(slow);
                self.c.i64c(i64::MIN).lget(bits).i32c(0x7f_ffff).op(I32_AND).op(I64_EXTEND_I32_U).i64c(40).op(I64_SHL).op(I64_OR).lset(mb);
                self.c.lget(be).i32c(127 - 16383).op(I32_SUB).lget(bits).i32c(31).op(I32_SHR_U).i32c(15).op(I32_SHL).op(I32_OR).lset(seb);
            }
        } else {
            let ps = self.x87_phys_i(top, si as i32);
            self.x87_tag_check(ps, false, slow);
            self.x87_read_reg(ps, mb, seb);
        }
        // operands must be normal numbers (exponent field 1..=0x7ffe, integer bit set) or zeros; denormals, specials and unnormals go to the interpreter
        let (za, zb) = (self.tmp(), self.tmp());
        for (m, se, z) in [(ma, sea, za), (mb, seb, zb)] {
            self.c.lget(se).i32c(0x7fff).op(I32_AND).op(I32_EQZ).lget(m).op(I64_EQZ).op(I32_AND).lset(z);
            self.c.lget(se).i32c(0x7fff).op(I32_AND).i32c(1).op(I32_SUB).i32c(0x7ffe).op(I32_GE_U);
            self.c.lget(m).i64c(0).op(I64_GE_S).op(I32_OR);
            self.c.lget(z).op(I32_EQZ).op(I32_AND);
            self.x87_br_if(slow);
        }
        let (sga, sgb, ea, eb) = (self.tmp(), self.tmp(), self.tmp(), self.tmp());
        self.c.lget(sea).i32c(15).op(I32_SHR_U).lset(sga);
        self.c.lget(seb).i32c(15).op(I32_SHR_U).lset(sgb);
        self.c.lget(sea).i32c(0x7fff).op(I32_AND).i32c(16383).op(I32_SUB).lset(ea);
        self.c.lget(seb).i32c(0x7fff).op(I32_AND).i32c(16383).op(I32_SUB).lset(eb);
        // add family: x + y with y's sign already flipped for a subtraction (FSUBR: x = b, y = a)
        let (xm, xs, xe, zx, ym, ys, ye, zy) = match op {
            0 => (ma, sga, ea, za, mb, sgb, eb, zb),
            1 => {
                let nb = self.tmp();
                self.c.lget(sgb).i32c(1).op(I32_XOR).lset(nb);
                (ma, sga, ea, za, mb, nb, eb, zb)
            }
            _ => {
                let na = self.tmp();
                self.c.lget(sga).i32c(1).op(I32_XOR).lset(na);
                (mb, sgb, eb, zb, ma, na, ea, za)
            }
        };
        let (rs, re, zres) = (self.tmp(), self.tmp(), self.tmp());
        let (rh, rl) = (L_T64 + 4, L_T64 + 5);
        self.c.i32c(0).lset(zres);
        self.c.lget(za).lget(zb).op(I32_OR);
        self.if_(BT_EMPTY);
        if op == 3 {
            // a zero times anything finite is a zero of the product's sign, exactly
            self.c.i32c(1).lset(zres);
            self.c.lget(sga).lget(sgb).op(I32_XOR).lset(rs);
        } else {
            self.c.lget(zx).lget(zy).op(I32_AND);
            self.if_(BT_EMPTY);
            // 0 + 0: the sign of the operands if they agree, else + (- when rounding down)
            self.c.i32c(1).lset(zres);
            self.c.lget(xs).lget(ys).op(I32_EQ);
            self.if_(I32);
            self.c.lget(xs);
            self.else_();
            self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_cw).i32c(10).op(I32_SHR_U).i32c(3).op(I32_AND).i32c(1).op(I32_EQ);
            self.close();
            self.c.lset(rs);
            self.else_();
            // one zero: the other operand, rounded to the precision of the control word
            self.c.lget(zx);
            self.if_(BT_EMPTY);
            self.c.lget(ys).lset(rs).lget(ye).lset(re).lget(ym).lset(rh);
            self.else_();
            self.c.lget(xs).lset(rs).lget(xe).lset(re).lget(xm).lset(rh);
            self.close();
            self.c.i64c(0).lset(rl);
            self.close();
        }
        self.else_();
        let (cs, ce, ch, cl) = match op {
            3 => self.x87_mul_core(ma, mb, sga, sgb, ea, eb),
            _ => self.x87_add_core(xm, xs, xe, ym, ys, ye, slow),
        };
        self.c.lget(cs).lset(rs).lget(ce).lset(re).lget(ch).lset(rh).lget(cl).lset(rl);
        self.close();
        let (ne, inexact, inc) = (self.tmp(), self.tmp(), self.tmp());
        let sn = L_T64 + 15;
        self.c.i32c(0).lset(ne).i32c(0).lset(inexact).i32c(0).lset(inc).i64c(0).lset(sn);
        self.c.lget(zres).op(I32_EQZ);
        self.if_(BT_EMPTY);
        let sn2 = self.x87_round(rs, re, rh, rl, ne, inexact, inc, slow);
        debug_assert_eq!(sn, sn2);
        self.close();
        // all checks passed: the instruction's effects (as the interpreter's fbegin / fea, arith, wr, pop)
        self.x87_fbegin(ip, addr);
        let sw = self.lay.cpu_fpu_sw;
        self.c.lget(inexact);
        self.if_(BT_EMPTY);
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, sw).i32c(0x20).op(I32_OR).lget(inc).i32c(9).op(I32_SHL).op(I32_OR).mem(I32_STORE16, sw);
        self.close();
        // sign/exponent word: a zero result has biased exponent 0 and tag 1 (zero); anything else is a normal number (tag 0)
        let (se, tagv) = (self.tmp(), self.tmp());
        self.c.lget(ne).i32c(16383).op(I32_ADD).lget(zres).i32c(1).op(I32_SUB).op(I32_AND).lget(rs).i32c(15).op(I32_SHL).op(I32_OR).lset(se);
        self.c.lget(zres).lset(tagv);
        self.x87_write_reg(pd, sn, se, tagv);
        if pop {
            self.x87_pop(top);
        }
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
        self.x87_touched();
        true
    }

    /// Entry point from `ins_ext`: true if the instruction was translated.
    fn x87_int64_helper(&mut self, ins: &Instruction, ip: u32, fist: bool) -> bool {
        if ins.op0_kind() != OpKind::Memory || ins.memory_size().size() != 8 {
            return false;
        }
        let (b, x) = (ins.memory_base(), ins.memory_index());
        if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
            return false;
        }
        let Some(a) = self.ea(ins, true) else { return false }; // include FS/GS bases for loads as well
        self.c.lget(L_CPU).lget(a).u32c(ip).i32c(fist as i32).call(F_FIST_FILD).ltee(L_X);
        self.if_(BT_EMPTY);
        self.c.lget(L_X).i32c(1).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.br(LB_FAULT);
        self.else_();
        self.c.i32c(1).lset(L_SMC);
        self.close();
        self.close();
        self.had_store |= fist;
        self.x87_touched();
        true
    }

    fn x87_compare_helper(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let r = ins.op1_register();
        if ins.op0_register() != Register::ST0 || !(Register::ST0..=Register::ST7).contains(&r) {
            return false;
        }
        let bi = r as u32 - Register::ST0 as u32;
        let signal = matches!(mn, M::Fcomi | M::Fcomip);
        let pop = matches!(mn, M::Fcomip | M::Fucomip);
        self.c.lget(L_CPU).u32c(bi | ((signal as u32) << 3) | ((pop as u32) << 4)).u32c(ip).call(F_FCOMIP).op(DROP);
        self.set_mem_state();
        self.x87_touched();
        true
    }

    fn x87_status_ax(&mut self, ins: &Instruction) -> bool {
        if ins.op0_kind() != OpKind::Register || ins.op0_register() != Register::AX {
            return false;
        }
        let sw = self.tmp();
        self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_sw).lset(sw);
        self.c.lget(L_R0).i32c(-65536).op(I32_AND);
        self.c.lget(sw).i32c(0x477f).op(I32_AND).op(I32_OR);
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).i32c(7).op(I32_AND).i32c(11).op(I32_SHL).op(I32_OR);
        self.c.lget(sw).i32c(0x3f).op(I32_AND);
        self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_cw).i32c(-1).op(I32_XOR).op(I32_AND);
        self.c.i32c(0).op(I32_NE).i32c(0x8080).op(I32_MUL).op(I32_OR).lset(L_R0);
        self.written |= 1;
        true
    }

    pub(super) fn x87_ins(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        if !self.x87_ok_layout() {
            return false;
        }
        match mn {
            M::Fnstsw | M::Fstsw => self.x87_status_ax(ins),
            M::Fild => self.x87_int64_helper(ins, ip, false),
            M::Fistp if ins.memory_size().size() == 8 => self.x87_int64_helper(ins, ip, true),
            M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => self.x87_compare_helper(ins, ip, mn),
            M::Fldcw => self.x87_cw(ins, ip, false),
            M::Fnstcw | M::Fstcw => self.x87_cw(ins, ip, true),
            M::Fld | M::Fst | M::Fstp if ins.op0_kind() == OpKind::Register => self.x87_reg_form(ins, ip, mn),
            M::Fxch | M::Fabs | M::Fchs => self.x87_reg_form(ins, ip, mn),
            M::Fadd | M::Fsub | M::Fsubr | M::Fmul | M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp => self.x87_arith(ins, ip, mn),
            M::Fld | M::Fld1 | M::Fldz => self.x87_fld(ins, ip, mn),
            M::Fst => self.x87_fst(ins, ip, false),
            M::Fstp => self.x87_fst(ins, ip, true),
            M::Fist => self.x87_fist(ins, ip, false),
            M::Fistp => self.x87_fist(ins, ip, true),
            _ => false,
        }
    }
}
