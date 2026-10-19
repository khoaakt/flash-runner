//! MMX / SSE / SSE2 execution.
//!
//! Layout: this file decodes operands and dispatches; the lane arithmetic lives in `sse_int.rs`
//! (integer SIMD) and `sse_fp.rs` (bit-exact x86 floating point, no host FP state involved).
//!
//! Conventions
//!  * XMM registers are `u128` (little-endian lanes); MMX registers are the low 64 bits of the
//!    x87 physical registers `fpu.st[i].mant` (MM_i = R_i regardless of TOP).
//!  * Every instruction reads all of its memory operands before it writes any register, so a fault
//!    leaves architectural state untouched.
//!  * Legacy-SSE 16-byte memory operands must be 16-byte aligned (misalignment -> #GP, reported as
//!    `Exit::Segv` at the operand address). MOVUPS/MOVUPD/MOVDQU never fault on alignment.
//!  * The FP exception flags in MXCSR are sticky and maintained; all exceptions are treated as
//!    masked (no #XM / SIGFPE), which is what every user-mode program we run configures.
//!  * FXSAVE/FXRSTOR are owned by fpu.rs.

use crate::cpu::*;
use crate::fpu::F80;
use crate::mem::{Memory, Segv};
use crate::sse_fp as fp;
use crate::sse_fp::{Env, F32, F64};
use crate::sse_int as si;
use crate::sse_int::{f32_, f64_, l32, l64};
use iced_x86::{Code, CpuidFeature, Instruction, Mnemonic as M, OpKind, Register};

const LO64: u128 = u64::MAX as u128;

#[inline(always)]
fn xi(r: Register) -> usize {
    r as usize - Register::XMM0 as usize
}
#[inline(always)]
fn mi(r: Register) -> usize {
    r as usize - Register::MM0 as usize
}
#[inline(always)]
fn is_mm(r: Register) -> bool {
    (r as usize).wrapping_sub(Register::MM0 as usize) < 8
}

impl Cpu {
    // ---- MMX register file (aliases the x87 stack) -------------------------------------------
    // Everything MMX-specific that touches fpu.rs state lives in these three helpers.
    //
    /// Any MMX instruction (except EMMS) puts the x87 unit in "MMX mode": TOP=0 and every register
    /// non-empty. The architectural tag is "valid", but FNSTENV/FNSAVE report tags classified from
    /// the register contents (as fpu.rs stores them), so non-empty registers keep their
    /// content-derived tag and only previously-empty ones are re-tagged.
    #[inline(always)]
    pub(crate) fn mm_enter(&mut self) {
        let t = self.fpu.tag;
        if self.fpu.top != 0 || (t & (t >> 1) & 0x5555) != 0 {
            self.fpu.top = 0;
            let mut nt = 0u16;
            for p in 0..8 {
                let cur = (t >> (2 * p)) & 3;
                let tg = if cur == 3 { crate::fpu::Fpu::tag_of(self.fpu.st[p]) } else { cur };
                nt |= tg << (2 * p);
            }
            self.fpu.tag = nt;
        }
    }
    #[inline(always)]
    fn mm_get(&self, i: usize) -> u64 {
        self.fpu.st[i].mant
    }
    /// Writing an MMX register also sets the x87 sign/exponent field to all ones (a "special" tag).
    #[inline(always)]
    pub(crate) fn mm_set(&mut self, i: usize, v: u64) {
        self.fpu.st[i] = F80 { mant: v, se: 0xffff };
        self.fpu.tag = (self.fpu.tag & !(3 << (2 * i))) | (2 << (2 * i));
    }

    // ---- operand access ----------------------------------------------------------------------
    #[inline(always)]
    fn ea_chk(&self, ins: &Instruction, align_mask: u32, write: bool) -> XResult<u32> {
        let a = self.ea(ins)?;
        if a & align_mask != 0 {
            return Err(Exit::Segv(Segv { addr: a, write }));
        }
        Ok(a)
    }

    /// 128-bit source operand: xmm register or m128 (16-byte aligned unless `aligned` is false).
    #[inline(always)]
    fn rx(&self, mem: &Memory, ins: &Instruction, idx: u32, aligned: bool) -> XResult<u128> {
        if ins.op_kind(idx) == OpKind::Register {
            Ok(self.xmm[xi(ins.op_register(idx))])
        } else {
            let a = if aligned { self.ea_chk(ins, 15, false)? } else { self.ea(ins)? };
            Ok(u128::from_le_bytes(mem.r128(a)?))
        }
    }
    /// 64-bit source: mm register, low half of an xmm register, or m64.
    #[inline(always)]
    fn r64any(&self, mem: &Memory, ins: &Instruction, idx: u32) -> XResult<u64> {
        if ins.op_kind(idx) == OpKind::Register {
            let r = ins.op_register(idx);
            Ok(if is_mm(r) { self.mm_get(mi(r)) } else { self.xmm[xi(r)] as u64 })
        } else {
            let a = self.ea(ins)?;
            // PUNPCKL{BW,WD,DQ} mm, mm/m32 only touch 4 bytes of memory (don't fault past a page end).
            if ins.memory_size().size() == 4 {
                Ok(mem.r32(a)? as u64)
            } else {
                Ok(mem.r64(a)?)
            }
        }
    }
    /// 32-bit source: low dword of an xmm register or m32.
    #[inline(always)]
    fn r32x(&self, mem: &Memory, ins: &Instruction, idx: u32) -> XResult<u32> {
        if ins.op_kind(idx) == OpKind::Register {
            Ok(self.xmm[xi(ins.op_register(idx))] as u32)
        } else {
            Ok(mem.r32(self.ea(ins)?)?)
        }
    }
    /// r/m32 integer source.
    #[inline(always)]
    fn ri32(&self, mem: &Memory, ins: &Instruction, idx: u32) -> XResult<u32> {
        if ins.op_kind(idx) == OpKind::Register {
            Ok(self.get_reg(ins.op_register(idx)))
        } else {
            Ok(mem.r32(self.ea(ins)?)?)
        }
    }

    // ---- generic integer SIMD forms ----------------------------------------------------------
    /// dst = f(dst, src) for xmm (128-bit) or mm (64-bit, zero-extended operands, truncated result).
    #[inline(always)]
    fn ibin(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u128, u128) -> u128) -> XResult<()> {
        self.ibin2(mem, ins, &f, &f)
    }
    /// Like `ibin` with a distinct MMX implementation (needed when the 128-bit op is not
    /// equivalent to the 64-bit one on zero-extended inputs: pack, unpack-high).
    #[inline(always)]
    fn ibin2(
        &mut self,
        mem: &Memory,
        ins: &Instruction,
        fx: &impl Fn(u128, u128) -> u128,
        fm: &impl Fn(u128, u128) -> u128,
    ) -> XResult<()> {
        let d = ins.op0_register();
        if is_mm(d) {
            let b = self.r64any(mem, ins, 1)?;
            self.mm_enter();
            let i = mi(d);
            let a = self.mm_get(i);
            self.mm_set(i, fm(a as u128, b as u128) as u64);
        } else {
            let b = self.rx(mem, ins, 1, true)?;
            let i = xi(d);
            self.xmm[i] = fx(self.xmm[i], b);
        }
        Ok(())
    }

    /// Shifts: count is imm8, an xmm/mm register or xmm/m128 / mm/m64 (low 64 bits used, saturating).
    #[inline(always)]
    fn pshift(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u128, u64) -> u128) -> XResult<()> {
        let d = ins.op0_register();
        let cnt: u64 = match ins.op1_kind() {
            OpKind::Immediate8 => ins.immediate8() as u64,
            OpKind::Register => {
                let r = ins.op1_register();
                if is_mm(r) {
                    self.mm_get(mi(r))
                } else {
                    self.xmm[xi(r)] as u64
                }
            }
            _ => {
                if is_mm(d) {
                    mem.r64(self.ea(ins)?)?
                } else {
                    self.rx(mem, ins, 1, true)? as u64
                }
            }
        };
        if is_mm(d) {
            self.mm_enter();
            let i = mi(d);
            let a = self.mm_get(i);
            self.mm_set(i, f(a as u128, cnt) as u64);
        } else {
            let i = xi(d);
            self.xmm[i] = f(self.xmm[i], cnt);
        }
        Ok(())
    }

    // ---- moves -------------------------------------------------------------------------------
    #[inline(always)]
    fn mov128(&mut self, mem: &mut Memory, ins: &Instruction, aligned: bool) -> XResult<()> {
        if ins.op0_kind() == OpKind::Register {
            let v = self.rx(mem, ins, 1, aligned)?;
            self.xmm[xi(ins.op0_register())] = v;
        } else {
            let a = if aligned { self.ea_chk(ins, 15, true)? } else { self.ea(ins)? };
            mem.w128(a, self.xmm[xi(ins.op1_register())].to_le_bytes())?;
        }
        Ok(())
    }

    fn movd(&mut self, mem: &mut Memory, ins: &Instruction) -> XResult<()> {
        let d = ins.op0_register();
        if ins.op0_kind() == OpKind::Register && (d.is_xmm() || is_mm(d)) {
            let v = self.ri32(mem, ins, 1)?;
            if is_mm(d) {
                self.mm_enter();
                self.mm_set(mi(d), v as u64);
            } else {
                self.xmm[xi(d)] = v as u128;
            }
        } else {
            let s = ins.op1_register();
            let v = if is_mm(s) {
                self.mm_enter();
                self.mm_get(mi(s)) as u32
            } else {
                self.xmm[xi(s)] as u32
            };
            if ins.op0_kind() == OpKind::Register {
                self.set_reg(d, v);
            } else {
                mem.w32(self.ea(ins)?, v)?;
            }
        }
        Ok(())
    }

    fn movq(&mut self, mem: &mut Memory, ins: &Instruction) -> XResult<()> {
        if ins.op0_kind() == OpKind::Register {
            let d = ins.op0_register();
            let v = self.r64any(mem, ins, 1)?;
            if is_mm(d) {
                self.mm_enter();
                self.mm_set(mi(d), v);
            } else {
                self.xmm[xi(d)] = v as u128; // zero-extends, also for the reg-reg form
            }
        } else {
            let s = ins.op1_register();
            let v = if is_mm(s) {
                self.mm_enter();
                self.mm_get(mi(s))
            } else {
                self.xmm[xi(s)] as u64
            };
            mem.w64(self.ea(ins)?, v)?;
        }
        Ok(())
    }

    /// MOVSS (`bits` = 32) / MOVSD (`bits` = 64): register-to-register merges, loads zero-extend.
    fn movs(&mut self, mem: &mut Memory, ins: &Instruction, bits: u32) -> XResult<()> {
        let lo_mask: u128 = (1u128 << bits) - 1;
        if ins.op0_kind() == OpKind::Register {
            let d = xi(ins.op0_register());
            if ins.op1_kind() == OpKind::Register {
                let s = self.xmm[xi(ins.op1_register())] & lo_mask;
                self.xmm[d] = (self.xmm[d] & !lo_mask) | s;
            } else {
                let a = self.ea(ins)?;
                self.xmm[d] = if bits == 32 { mem.r32(a)? as u128 } else { mem.r64(a)? as u128 };
            }
        } else {
            let a = self.ea(ins)?;
            let v = self.xmm[xi(ins.op1_register())];
            if bits == 32 {
                mem.w32(a, v as u32)?;
            } else {
                mem.w64(a, v as u64)?;
            }
        }
        Ok(())
    }

    /// MOVLPS/MOVLPD (`hi` = false) and MOVHPS/MOVHPD (`hi` = true) with a memory operand.
    fn movlh(&mut self, mem: &mut Memory, ins: &Instruction, hi: bool) -> XResult<()> {
        let sh = if hi { 64 } else { 0 };
        if ins.op0_kind() == OpKind::Register {
            let d = xi(ins.op0_register());
            let v = mem.r64(self.ea(ins)?)? as u128;
            self.xmm[d] = (self.xmm[d] & !(LO64 << sh)) | (v << sh);
        } else {
            let v = (self.xmm[xi(ins.op1_register())] >> sh) as u64;
            mem.w64(self.ea(ins)?, v)?;
        }
        Ok(())
    }

    fn maskmov(&mut self, mem: &mut Memory, ins: &Instruction, bytes: usize) -> XResult<()> {
        // Implicit destination: DS:[EDI] (segment overrides honoured).
        let base = match ins.segment_prefix() {
            Register::FS => self.fs_base,
            Register::GS => self.gs_base,
            _ => 0,
        };
        let addr = self.r[EDI].wrapping_add(base);
        // iced lists the implicit [EDI] destination as operand 0, so data/mask are operands 1 and 2.
        let k = if ins.op0_kind() == OpKind::Register { 0 } else { 1 };
        let (data, sel): (u128, u128);
        if bytes == 8 {
            self.mm_enter();
            data = self.mm_get(mi(ins.op_register(k))) as u128;
            sel = self.mm_get(mi(ins.op_register(k + 1))) as u128;
        } else {
            data = self.xmm[xi(ins.op_register(k))];
            sel = self.xmm[xi(ins.op_register(k + 1))];
        }
        let (d, s) = (si::l8(data), si::l8(sel));
        for i in 0..bytes {
            if s[i] & 0x80 != 0 {
                mem.w8(addr.wrapping_add(i as u32), d[i])?;
            }
        }
        Ok(())
    }

    // ---- floating-point lane drivers ---------------------------------------------------------
    #[inline(always)]
    fn ps2(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u32, u32, &mut Env) -> u32) -> XResult<()> {
        let d = xi(ins.op0_register());
        let b = l32(self.rx(mem, ins, 1, true)?);
        let a = l32(self.xmm[d]);
        let mut env = Env::new(self.mxcsr);
        let mut r = a;
        for i in 0..4 {
            r[i] = f(a[i], b[i], &mut env);
        }
        self.xmm[d] = f32_(r);
        self.mxcsr |= env.fl;
        Ok(())
    }
    #[inline(always)]
    fn ss2(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u32, u32, &mut Env) -> u32) -> XResult<()> {
        let d = xi(ins.op0_register());
        let b = self.r32x(mem, ins, 1)?;
        let a = self.xmm[d];
        let mut env = Env::new(self.mxcsr);
        let r = f(a as u32, b, &mut env);
        self.xmm[d] = (a & !(u32::MAX as u128)) | r as u128;
        self.mxcsr |= env.fl;
        Ok(())
    }
    #[inline(always)]
    fn pd2(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u64, u64, &mut Env) -> u64) -> XResult<()> {
        let d = xi(ins.op0_register());
        let b = l64(self.rx(mem, ins, 1, true)?);
        let a = l64(self.xmm[d]);
        let mut env = Env::new(self.mxcsr);
        let r = [f(a[0], b[0], &mut env), f(a[1], b[1], &mut env)];
        self.xmm[d] = f64_(r);
        self.mxcsr |= env.fl;
        Ok(())
    }
    #[inline(always)]
    fn sd2(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u64, u64, &mut Env) -> u64) -> XResult<()> {
        let d = xi(ins.op0_register());
        let b = self.r64any(mem, ins, 1)?;
        let a = self.xmm[d];
        let mut env = Env::new(self.mxcsr);
        let r = f(a as u64, b, &mut env);
        self.xmm[d] = (a & !LO64) | r as u128;
        self.mxcsr |= env.fl;
        Ok(())
    }

    fn set_comis_flags(&mut self, (zf, pf, cf): (bool, bool, bool)) {
        self.fl &= !(OF | SF | AF | ZF | PF | CF);
        if zf {
            self.fl |= ZF;
        }
        if pf {
            self.fl |= PF;
        }
        if cf {
            self.fl |= CF;
        }
    }

    // ---- conversions -------------------------------------------------------------------------
    /// 4 x i32 <-> f32 style conversions over a full 128-bit source/dest.
    fn cvt_lanes32(&mut self, mem: &Memory, ins: &Instruction, f: impl Fn(u32, &mut Env) -> u32) -> XResult<()> {
        let d = xi(ins.op0_register());
        let s = l32(self.rx(mem, ins, 1, true)?);
        let mut env = Env::new(self.mxcsr);
        let mut r = s;
        for i in 0..4 {
            r[i] = f(s[i], &mut env);
        }
        self.xmm[d] = f32_(r);
        self.mxcsr |= env.fl;
        Ok(())
    }

    /// Two f64 (xmm/m128) -> two i32 in the low 64 bits (upper zeroed): CVT[T]PD2DQ.
    fn cvt_pd2dq(&mut self, mem: &Memory, ins: &Instruction, trunc: bool) -> XResult<()> {
        let s = l64(self.rx(mem, ins, 1, true)?);
        let mut env = Env::new(self.mxcsr);
        let lo = fp::f2i::<F64>(s[0], trunc, &mut env) as u64;
        let hi = fp::f2i::<F64>(s[1], trunc, &mut env) as u64;
        self.xmm[xi(ins.op0_register())] = (lo | (hi << 32)) as u128;
        self.mxcsr |= env.fl;
        Ok(())
    }

    /// CVT[T]PD2PI mm, xmm/m128.
    fn cvt_pd2pi(&mut self, mem: &Memory, ins: &Instruction, trunc: bool) -> XResult<()> {
        let s = l64(self.rx(mem, ins, 1, true)?);
        let mut env = Env::new(self.mxcsr);
        let lo = fp::f2i::<F64>(s[0], trunc, &mut env) as u64;
        let hi = fp::f2i::<F64>(s[1], trunc, &mut env) as u64;
        self.mm_enter();
        self.mm_set(mi(ins.op0_register()), lo | (hi << 32));
        self.mxcsr |= env.fl;
        Ok(())
    }

    /// CVT[T]PS2PI mm, xmm/m64.
    fn cvt_ps2pi(&mut self, mem: &Memory, ins: &Instruction, trunc: bool) -> XResult<()> {
        let s = self.r64any(mem, ins, 1)?;
        let mut env = Env::new(self.mxcsr);
        let lo = fp::f2i::<F32>(s & 0xffff_ffff, trunc, &mut env) as u64;
        let hi = fp::f2i::<F32>(s >> 32, trunc, &mut env) as u64;
        self.mm_enter();
        self.mm_set(mi(ins.op0_register()), lo | (hi << 32));
        self.mxcsr |= env.fl;
        Ok(())
    }

    /// CVT[T]SS2SI / CVT[T]SD2SI r32, xmm/m32|m64.
    fn cvt_s2si(&mut self, mem: &Memory, ins: &Instruction, double: bool, trunc: bool) -> XResult<()> {
        let mut env = Env::new(self.mxcsr);
        let v = if double {
            let s = self.r64any(mem, ins, 1)?;
            fp::f2i::<F64>(s, trunc, &mut env)
        } else {
            let s = self.r32x(mem, ins, 1)?;
            fp::f2i::<F32>(s as u64, trunc, &mut env)
        };
        self.set_reg(ins.op0_register(), v);
        self.mxcsr |= env.fl;
        Ok(())
    }

    // ---- the dispatcher ----------------------------------------------------------------------
    pub fn exec_sse(&mut self, mem: &mut Memory, ins: &Instruction) -> XResult<()> {
        let m: &mut Memory = mem;
        match ins.mnemonic() {
            // ======== moves ========
            M::Movdqa | M::Movaps | M::Movapd | M::Movntdq | M::Movntps | M::Movntpd => self.mov128(m, ins, true)?,
            M::Movdqu | M::Movups | M::Movupd => self.mov128(m, ins, false)?,
            M::Movd => self.movd(m, ins)?,
            M::Movq => self.movq(m, ins)?,
            M::Movss => self.movs(m, ins, 32)?,
            M::Movsd => self.movs(m, ins, 64)?,
            M::Movlps | M::Movlpd => self.movlh(m, ins, false)?,
            M::Movhps | M::Movhpd => self.movlh(m, ins, true)?,
            M::Movhlps => {
                let (d, s) = (xi(ins.op0_register()), xi(ins.op1_register()));
                self.xmm[d] = (self.xmm[d] & !LO64) | (self.xmm[s] >> 64);
            }
            M::Movlhps => {
                let (d, s) = (xi(ins.op0_register()), xi(ins.op1_register()));
                self.xmm[d] = (self.xmm[d] & LO64) | (self.xmm[s] << 64);
            }
            M::Movdq2q => {
                let v = self.xmm[xi(ins.op1_register())] as u64;
                self.mm_enter();
                self.mm_set(mi(ins.op0_register()), v);
            }
            M::Movq2dq => {
                let v = self.mm_get(mi(ins.op1_register()));
                self.mm_enter();
                self.xmm[xi(ins.op0_register())] = v as u128;
            }
            M::Movmskps => {
                let v = l32(self.xmm[xi(ins.op1_register())]);
                let r = (v[0] >> 31) | ((v[1] >> 31) << 1) | ((v[2] >> 31) << 2) | ((v[3] >> 31) << 3);
                self.set_reg(ins.op0_register(), r);
            }
            M::Movmskpd => {
                let v = l64(self.xmm[xi(ins.op1_register())]);
                self.set_reg(ins.op0_register(), ((v[0] >> 63) | ((v[1] >> 63) << 1)) as u32);
            }
            M::Pmovmskb => {
                let s = ins.op1_register();
                let r = if is_mm(s) {
                    self.mm_enter();
                    si::pmovmskb(self.mm_get(mi(s)) as u128)
                } else {
                    si::pmovmskb(self.xmm[xi(s)])
                };
                self.set_reg(ins.op0_register(), r);
            }
            M::Movnti => {
                let v = self.get_reg(ins.op1_register());
                m.w32(self.ea(ins)?, v)?;
            }
            M::Movntq => {
                let v = self.mm_get(mi(ins.op1_register()));
                self.mm_enter();
                m.w64(self.ea(ins)?, v)?;
            }
            M::Maskmovdqu => self.maskmov(m, ins, 16)?,
            M::Maskmovq => self.maskmov(m, ins, 8)?,
            M::Ldmxcsr => {
                let v = m.r32(self.ea(ins)?)?;
                self.mxcsr = v & 0xffff;
            }
            M::Stmxcsr => {
                let a = self.ea(ins)?;
                m.w32(a, self.mxcsr)?;
            }
            M::Clflush => {
                self.ea(ins)?;
            }
            M::Emms => self.fpu.tag = 0xffff,

            // ======== integer SIMD: arithmetic ========
            M::Paddb => self.ibin(m, ins, si::paddb)?,
            M::Paddw => self.ibin(m, ins, si::paddw)?,
            M::Paddd => self.ibin(m, ins, si::paddd)?,
            M::Paddq => self.ibin(m, ins, si::paddq)?,
            M::Psubb => self.ibin(m, ins, si::psubb)?,
            M::Psubw => self.ibin(m, ins, si::psubw)?,
            M::Psubd => self.ibin(m, ins, si::psubd)?,
            M::Psubq => self.ibin(m, ins, si::psubq)?,
            M::Paddsb => self.ibin(m, ins, si::paddsb)?,
            M::Paddsw => self.ibin(m, ins, si::paddsw)?,
            M::Paddusb => self.ibin(m, ins, si::paddusb)?,
            M::Paddusw => self.ibin(m, ins, si::paddusw)?,
            M::Psubsb => self.ibin(m, ins, si::psubsb)?,
            M::Psubsw => self.ibin(m, ins, si::psubsw)?,
            M::Psubusb => self.ibin(m, ins, si::psubusb)?,
            M::Psubusw => self.ibin(m, ins, si::psubusw)?,
            M::Pmullw => self.ibin(m, ins, si::pmullw)?,
            M::Pmulhw => self.ibin(m, ins, si::pmulhw)?,
            M::Pmulhuw => self.ibin(m, ins, si::pmulhuw)?,
            M::Pmuludq => self.ibin(m, ins, si::pmuludq)?,
            M::Pmaddwd => self.ibin(m, ins, si::pmaddwd)?,
            M::Pavgb => self.ibin(m, ins, si::pavgb)?,
            M::Pavgw => self.ibin(m, ins, si::pavgw)?,
            M::Pminub => self.ibin(m, ins, si::pminub)?,
            M::Pmaxub => self.ibin(m, ins, si::pmaxub)?,
            M::Pminsw => self.ibin(m, ins, si::pminsw)?,
            M::Pmaxsw => self.ibin(m, ins, si::pmaxsw)?,
            M::Psadbw => self.ibin(m, ins, si::psadbw)?,
            M::Pcmpeqb => self.ibin(m, ins, si::pcmpeqb)?,
            M::Pcmpeqw => self.ibin(m, ins, si::pcmpeqw)?,
            M::Pcmpeqd => self.ibin(m, ins, si::pcmpeqd)?,
            M::Pcmpgtb => self.ibin(m, ins, si::pcmpgtb)?,
            M::Pcmpgtw => self.ibin(m, ins, si::pcmpgtw)?,
            M::Pcmpgtd => self.ibin(m, ins, si::pcmpgtd)?,
            // logic (the packed-FP forms are bitwise as well)
            M::Pand | M::Andps | M::Andpd => self.ibin(m, ins, |a, b| a & b)?,
            M::Pandn | M::Andnps | M::Andnpd => self.ibin(m, ins, |a, b| !a & b)?,
            M::Por | M::Orps | M::Orpd => self.ibin(m, ins, |a, b| a | b)?,
            M::Pxor | M::Xorps | M::Xorpd => self.ibin(m, ins, |a, b| a ^ b)?,
            // shifts
            M::Psllw => self.pshift(m, ins, si::psllw)?,
            M::Pslld => self.pshift(m, ins, si::pslld)?,
            M::Psllq => self.pshift(m, ins, si::psllq)?,
            M::Psrlw => self.pshift(m, ins, si::psrlw)?,
            M::Psrld => self.pshift(m, ins, si::psrld)?,
            M::Psrlq => self.pshift(m, ins, si::psrlq)?,
            M::Psraw => self.pshift(m, ins, si::psraw)?,
            M::Psrad => self.pshift(m, ins, si::psrad)?,
            M::Pslldq => {
                let i = xi(ins.op0_register());
                self.xmm[i] = si::pslldq(self.xmm[i], ins.immediate8() as u32);
            }
            M::Psrldq => {
                let i = xi(ins.op0_register());
                self.xmm[i] = si::psrldq(self.xmm[i], ins.immediate8() as u32);
            }
            // pack / unpack (the MMX forms operate on 4+4 lanes, not 8+8)
            M::Packsswb => self.ibin2(m, ins, &si::packsswb, &|a, b| si::packsswb(a | (b << 64), 0))?,
            M::Packssdw => self.ibin2(m, ins, &si::packssdw, &|a, b| si::packssdw(a | (b << 64), 0))?,
            M::Packuswb => self.ibin2(m, ins, &si::packuswb, &|a, b| si::packuswb(a | (b << 64), 0))?,
            M::Punpcklbw => self.ibin(m, ins, si::punpcklbw)?,
            M::Punpcklwd => self.ibin(m, ins, si::punpcklwd)?,
            M::Punpckldq => self.ibin(m, ins, si::punpckldq)?,
            M::Punpckhbw => self.ibin2(m, ins, &si::punpckhbw, &|a, b| si::punpcklbw(a >> 32, b >> 32))?,
            M::Punpckhwd => self.ibin2(m, ins, &si::punpckhwd, &|a, b| si::punpcklwd(a >> 32, b >> 32))?,
            M::Punpckhdq => self.ibin2(m, ins, &si::punpckhdq, &|a, b| si::punpckldq(a >> 32, b >> 32))?,
            M::Punpcklqdq => self.ibin(m, ins, si::punpcklqdq)?,
            M::Punpckhqdq => self.ibin(m, ins, si::punpckhqdq)?,
            // shuffles
            M::Pshufd => {
                let v = self.rx(m, ins, 1, true)?;
                self.xmm[xi(ins.op0_register())] = si::pshufd(v, ins.immediate8());
            }
            M::Pshuflw => {
                let v = self.rx(m, ins, 1, true)?;
                self.xmm[xi(ins.op0_register())] = si::pshuflw(v, ins.immediate8());
            }
            M::Pshufhw => {
                let v = self.rx(m, ins, 1, true)?;
                self.xmm[xi(ins.op0_register())] = si::pshufhw(v, ins.immediate8());
            }
            M::Pshufw => {
                let v = self.r64any(m, ins, 1)?;
                self.mm_enter();
                self.mm_set(mi(ins.op0_register()), si::pshufw(v, ins.immediate8()));
            }
            M::Pextrw => {
                // Only the SSE2 / MMX register-destination forms; the SSE4.1 r/m16 form is #UD.
                if !matches!(ins.code(), Code::Pextrw_r32_xmm_imm8 | Code::Pextrw_r32_mm_imm8) {
                    return Err(Exit::Undef { eip: ins.ip() as u32 });
                }
                let s = ins.op1_register();
                let imm = ins.immediate8() as u32;
                let v = if is_mm(s) {
                    self.mm_enter();
                    ((self.mm_get(mi(s)) >> (16 * (imm & 3))) & 0xffff) as u32
                } else {
                    ((self.xmm[xi(s)] >> (16 * (imm & 7))) & 0xffff) as u32
                };
                self.set_reg(ins.op0_register(), v);
            }
            M::Pinsrw => {
                let w = if ins.op1_kind() == OpKind::Register {
                    self.get_reg(ins.op1_register()) & 0xffff
                } else {
                    m.r16(self.ea(ins)?)? as u32
                };
                let d = ins.op0_register();
                let imm = ins.immediate8() as u32;
                if is_mm(d) {
                    self.mm_enter();
                    let sh = 16 * (imm & 3);
                    let i = mi(d);
                    let v = (self.mm_get(i) & !(0xffffu64 << sh)) | ((w as u64) << sh);
                    self.mm_set(i, v);
                } else {
                    let sh = 16 * (imm & 7);
                    let i = xi(d);
                    self.xmm[i] = (self.xmm[i] & !(0xffffu128 << sh)) | ((w as u128) << sh);
                }
            }

            // ======== floating point: arithmetic ========
            M::Addps => self.ps2(m, ins, |a, b, e| fp::add32(a, b, e))?,
            M::Addss => self.ss2(m, ins, |a, b, e| fp::add32(a, b, e))?,
            M::Addpd => self.pd2(m, ins, |a, b, e| fp::add64(a, b, e))?,
            M::Addsd => self.sd2(m, ins, |a, b, e| fp::add64(a, b, e))?,
            M::Subps => self.ps2(m, ins, |a, b, e| fp::sub32(a, b, e))?,
            M::Subss => self.ss2(m, ins, |a, b, e| fp::sub32(a, b, e))?,
            M::Subpd => self.pd2(m, ins, |a, b, e| fp::sub64(a, b, e))?,
            M::Subsd => self.sd2(m, ins, |a, b, e| fp::sub64(a, b, e))?,
            M::Mulps => self.ps2(m, ins, |a, b, e| fp::mul32(a, b, e))?,
            M::Mulss => self.ss2(m, ins, |a, b, e| fp::mul32(a, b, e))?,
            M::Mulpd => self.pd2(m, ins, |a, b, e| fp::mul64(a, b, e))?,
            M::Mulsd => self.sd2(m, ins, |a, b, e| fp::mul64(a, b, e))?,
            M::Divps => self.ps2(m, ins, |a, b, e| fp::div32(a, b, e))?,
            M::Divss => self.ss2(m, ins, |a, b, e| fp::div32(a, b, e))?,
            M::Divpd => self.pd2(m, ins, |a, b, e| fp::div64(a, b, e))?,
            M::Divsd => self.sd2(m, ins, |a, b, e| fp::div64(a, b, e))?,
            M::Minps => self.ps2(m, ins, |a, b, e| fp::minmax::<F32>(a as u64, b as u64, e, false) as u32)?,
            M::Minss => self.ss2(m, ins, |a, b, e| fp::minmax::<F32>(a as u64, b as u64, e, false) as u32)?,
            M::Minpd => self.pd2(m, ins, |a, b, e| fp::minmax::<F64>(a, b, e, false))?,
            M::Minsd => self.sd2(m, ins, |a, b, e| fp::minmax::<F64>(a, b, e, false))?,
            M::Maxps => self.ps2(m, ins, |a, b, e| fp::minmax::<F32>(a as u64, b as u64, e, true) as u32)?,
            M::Maxss => self.ss2(m, ins, |a, b, e| fp::minmax::<F32>(a as u64, b as u64, e, true) as u32)?,
            M::Maxpd => self.pd2(m, ins, |a, b, e| fp::minmax::<F64>(a, b, e, true))?,
            M::Maxsd => self.sd2(m, ins, |a, b, e| fp::minmax::<F64>(a, b, e, true))?,
            M::Sqrtps => self.ps2(m, ins, |_, b, e| fp::sqrt32(b, e))?,
            M::Sqrtss => self.ss2(m, ins, |_, b, e| fp::sqrt32(b, e))?,
            M::Sqrtpd => self.pd2(m, ins, |_, b, e| fp::sqrt64(b, e))?,
            M::Sqrtsd => self.sd2(m, ins, |_, b, e| fp::sqrt64(b, e))?,
            M::Rcpps => self.ps2(m, ins, |_, b, _| fp::rcp32(b))?,
            M::Rcpss => self.ss2(m, ins, |_, b, _| fp::rcp32(b))?,
            M::Rsqrtps => self.ps2(m, ins, |_, b, _| fp::rsqrt32(b))?,
            M::Rsqrtss => self.ss2(m, ins, |_, b, _| fp::rsqrt32(b))?,
            M::Cmpps => {
                let p = ins.immediate8();
                self.ps2(m, ins, |a, b, e| if fp::cmp_pred::<F32>(a as u64, b as u64, p, e) { !0 } else { 0 })?
            }
            M::Cmpss => {
                let p = ins.immediate8();
                self.ss2(m, ins, |a, b, e| if fp::cmp_pred::<F32>(a as u64, b as u64, p, e) { !0 } else { 0 })?
            }
            M::Cmppd => {
                let p = ins.immediate8();
                self.pd2(m, ins, |a, b, e| if fp::cmp_pred::<F64>(a, b, p, e) { !0 } else { 0 })?
            }
            M::Cmpsd => {
                let p = ins.immediate8();
                self.sd2(m, ins, |a, b, e| if fp::cmp_pred::<F64>(a, b, p, e) { !0 } else { 0 })?
            }
            M::Comiss | M::Ucomiss => {
                let a = self.xmm[xi(ins.op0_register())] as u32;
                let b = self.r32x(m, ins, 1)?;
                let mut env = Env::new(self.mxcsr);
                let r = fp::comis::<F32>(a as u64, b as u64, ins.mnemonic() == M::Comiss, &mut env);
                self.mxcsr |= env.fl;
                self.set_comis_flags(r);
            }
            M::Comisd | M::Ucomisd => {
                let a = self.xmm[xi(ins.op0_register())] as u64;
                let b = self.r64any(m, ins, 1)?;
                let mut env = Env::new(self.mxcsr);
                let r = fp::comis::<F64>(a, b, ins.mnemonic() == M::Comisd, &mut env);
                self.mxcsr |= env.fl;
                self.set_comis_flags(r);
            }

            // ======== floating point: shuffles ========
            M::Shufps => {
                let imm = ins.immediate8() as usize;
                let b = l32(self.rx(m, ins, 1, true)?);
                let d = xi(ins.op0_register());
                let a = l32(self.xmm[d]);
                self.xmm[d] = f32_([a[imm & 3], a[(imm >> 2) & 3], b[(imm >> 4) & 3], b[(imm >> 6) & 3]]);
            }
            M::Shufpd => {
                let imm = ins.immediate8() as usize;
                let b = l64(self.rx(m, ins, 1, true)?);
                let d = xi(ins.op0_register());
                let a = l64(self.xmm[d]);
                self.xmm[d] = f64_([a[imm & 1], b[(imm >> 1) & 1]]);
            }
            M::Unpcklps => self.ibin(m, ins, si::punpckldq)?,
            M::Unpckhps => self.ibin(m, ins, si::punpckhdq)?,
            M::Unpcklpd => self.ibin(m, ins, si::punpcklqdq)?,
            M::Unpckhpd => self.ibin(m, ins, si::punpckhqdq)?,

            // ======== conversions ========
            M::Cvtps2pd => {
                let s = self.r64any(m, ins, 1)?;
                let mut env = Env::new(self.mxcsr);
                let lo = fp::cvt_f2f::<F32, F64>(s & 0xffff_ffff, &mut env);
                let hi = fp::cvt_f2f::<F32, F64>(s >> 32, &mut env);
                self.xmm[xi(ins.op0_register())] = f64_([lo, hi]);
                self.mxcsr |= env.fl;
            }
            M::Cvtpd2ps => {
                let s = l64(self.rx(m, ins, 1, true)?);
                let mut env = Env::new(self.mxcsr);
                let lo = fp::cvt_f2f::<F64, F32>(s[0], &mut env);
                let hi = fp::cvt_f2f::<F64, F32>(s[1], &mut env);
                self.xmm[xi(ins.op0_register())] = (lo | (hi << 32)) as u128;
                self.mxcsr |= env.fl;
            }
            M::Cvtss2sd => self.sd2_from32(m, ins)?,
            M::Cvtsd2ss => self.ss_from64(m, ins)?,
            M::Cvtdq2ps => self.cvt_lanes32(m, ins, |x, e| fp::i2f::<F32>(x as i32, e) as u32)?,
            M::Cvtps2dq => self.cvt_lanes32(m, ins, |x, e| fp::f2i::<F32>(x as u64, false, e))?,
            M::Cvttps2dq => self.cvt_lanes32(m, ins, |x, e| fp::f2i::<F32>(x as u64, true, e))?,
            M::Cvtdq2pd => {
                let s = self.r64any(m, ins, 1)?;
                let mut env = Env::new(self.mxcsr);
                let lo = fp::i2f::<F64>(s as u32 as i32, &mut env);
                let hi = fp::i2f::<F64>((s >> 32) as u32 as i32, &mut env);
                self.xmm[xi(ins.op0_register())] = f64_([lo, hi]);
                self.mxcsr |= env.fl;
            }
            M::Cvtpd2dq => self.cvt_pd2dq(m, ins, false)?,
            M::Cvttpd2dq => self.cvt_pd2dq(m, ins, true)?,
            M::Cvtsi2ss => {
                let v = self.ri32(m, ins, 1)? as i32;
                let mut env = Env::new(self.mxcsr);
                let r = fp::i2f::<F32>(v, &mut env) as u32;
                let d = xi(ins.op0_register());
                self.xmm[d] = (self.xmm[d] & !(u32::MAX as u128)) | r as u128;
                self.mxcsr |= env.fl;
            }
            M::Cvtsi2sd => {
                let v = self.ri32(m, ins, 1)? as i32;
                let mut env = Env::new(self.mxcsr);
                let r = fp::i2f::<F64>(v, &mut env);
                let d = xi(ins.op0_register());
                self.xmm[d] = (self.xmm[d] & !LO64) | r as u128;
                self.mxcsr |= env.fl;
            }
            M::Cvtss2si => self.cvt_s2si(m, ins, false, false)?,
            M::Cvttss2si => self.cvt_s2si(m, ins, false, true)?,
            M::Cvtsd2si => self.cvt_s2si(m, ins, true, false)?,
            M::Cvttsd2si => self.cvt_s2si(m, ins, true, true)?,
            M::Cvtpi2ps => {
                let s = self.r64any(m, ins, 1)?;
                let mut env = Env::new(self.mxcsr);
                let lo = fp::i2f::<F32>(s as u32 as i32, &mut env);
                let hi = fp::i2f::<F32>((s >> 32) as u32 as i32, &mut env);
                self.mm_enter();
                let d = xi(ins.op0_register());
                self.xmm[d] = (self.xmm[d] & !LO64) | (lo | (hi << 32)) as u128;
                self.mxcsr |= env.fl;
            }
            M::Cvtpi2pd => {
                let s = self.r64any(m, ins, 1)?;
                let mut env = Env::new(self.mxcsr);
                let lo = fp::i2f::<F64>(s as u32 as i32, &mut env);
                let hi = fp::i2f::<F64>((s >> 32) as u32 as i32, &mut env);
                self.mm_enter();
                self.xmm[xi(ins.op0_register())] = f64_([lo, hi]);
                self.mxcsr |= env.fl;
            }
            M::Cvtps2pi => self.cvt_ps2pi(m, ins, false)?,
            M::Cvttps2pi => self.cvt_ps2pi(m, ins, true)?,
            M::Cvtpd2pi => self.cvt_pd2pi(m, ins, false)?,
            M::Cvttpd2pi => self.cvt_pd2pi(m, ins, true)?,

            _ => {
                // SSE3 and later are never advertised by CPUID: #UD. Anything else reaching here is
                // a gap worth surfacing.
                let later = ins.code().cpuid_features().iter().any(|f| {
                    matches!(
                        f,
                        CpuidFeature::SSE3
                            | CpuidFeature::SSSE3
                            | CpuidFeature::SSE4_1
                            | CpuidFeature::SSE4_2
                            | CpuidFeature::SSE4A
                            | CpuidFeature::AES
                            | CpuidFeature::PCLMULQDQ
                            | CpuidFeature::POPCNT
                            | CpuidFeature::MOVBE
                            | CpuidFeature::D3NOW
                            | CpuidFeature::PREFETCHW
                    )
                }) || ins.mnemonic() == M::Femms;
                return Err(if later {
                    Exit::Undef { eip: ins.ip() as u32 }
                } else {
                    Exit::Unimpl { eip: ins.ip() as u32, what: format!("{:?}", ins.code()) }
                });
            }
        }
        Ok(())
    }

    /// CVTSS2SD xmm, xmm/m32: low qword replaced, upper preserved.
    fn sd2_from32(&mut self, mem: &Memory, ins: &Instruction) -> XResult<()> {
        let b = self.r32x(mem, ins, 1)?;
        let mut env = Env::new(self.mxcsr);
        let r = fp::cvt_f2f::<F32, F64>(b as u64, &mut env);
        let d = xi(ins.op0_register());
        self.xmm[d] = (self.xmm[d] & !LO64) | r as u128;
        self.mxcsr |= env.fl;
        Ok(())
    }
    /// CVTSD2SS xmm, xmm/m64: low dword replaced, upper 96 bits preserved.
    fn ss_from64(&mut self, mem: &Memory, ins: &Instruction) -> XResult<()> {
        let b = self.r64any(mem, ins, 1)?;
        let mut env = Env::new(self.mxcsr);
        let r = fp::cvt_f2f::<F64, F32>(b, &mut env);
        let d = xi(ins.op0_register());
        self.xmm[d] = (self.xmm[d] & !(u32::MAX as u128)) | r as u128;
        self.mxcsr |= env.fl;
        Ok(())
    }
}
