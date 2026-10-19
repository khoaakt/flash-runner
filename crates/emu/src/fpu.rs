//! x87 FPU state and instruction execution.
//!
//! * `fpu_soft.rs`  - bit-exact extended-precision arithmetic, conversions, rounding, FPREM, ...
//! * `fpu_trans.rs` - F2XM1/FYL2X/FYL2XP1/FPATAN/FSIN/FCOS/FPTAN on a 128-bit-significand helper float
//! * this file      - register stack, tag word, control/status words, memory formats, environment
//!   save/restore (FNSTENV/FNSAVE/FXSAVE families) and the instruction dispatcher `exec_x87`.
//!
//! Only masked-exception behaviour is modelled: exception flags are recorded in the status word but no
//! #MF is ever delivered. Stack overflow/underflow produce the "real indefinite" as the SDM prescribes.

use crate::cpu::{Cpu, Exit, XResult, CF, OF, PF, SF as EF_SF, ZF, AF};
use crate::mem::Memory;
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};

#[path = "fpu_soft.rs"]
pub mod soft;
#[path = "fpu_trans.rs"]
mod trans;

use soft::*;

/// 80-bit extended value: 64-bit significand (explicit integer bit) + sign/exponent.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct F80 {
    pub mant: u64,
    pub se: u16,
}

#[derive(Clone)]
pub struct Fpu {
    /// Physical registers R0..R7 (ST(i) = st[(top + i) & 7]).
    pub st: [F80; 8],
    pub top: u8,
    /// Status word WITHOUT the TOP field (TOP is `top`); compose on FNSTSW/FSAVE.
    pub sw: u16,
    pub cw: u16,
    /// 2 bits per physical register: 00 valid, 01 zero, 10 special, 11 empty.
    pub tag: u16,
    pub last_ip: u32,
    pub last_dp: u32,
    pub last_op: u16,
}

impl Default for Fpu {
    fn default() -> Fpu {
        Fpu::new()
    }
}

const ONE: F80 = F80 { mant: 1 << 63, se: 0x3FFF };

#[inline(always)]
fn sti(r: Register) -> usize {
    (r as usize).wrapping_sub(Register::ST0 as usize) & 7
}

/// Last register operand of an instruction that names an ST(i) register.
#[inline(always)]
fn src_reg(ins: &Instruction) -> usize {
    if ins.op_count() >= 2 {
        sti(ins.op1_register())
    } else {
        sti(ins.op0_register())
    }
}

impl Fpu {
    pub fn new() -> Fpu {
        Fpu { st: [F80::default(); 8], top: 0, sw: 0, cw: 0x037f, tag: 0xffff, last_ip: 0, last_dp: 0, last_op: 0 }
    }

    // ---- register stack -------------------------------------------------------------------
    #[inline(always)]
    fn phys(&self, i: usize) -> usize {
        (self.top as usize + i) & 7
    }
    #[inline(always)]
    fn is_empty_phys(&self, p: usize) -> bool {
        (self.tag >> (2 * p)) & 3 == 3
    }
    /// Tag a (non-empty) register holding `v`: 00 valid, 01 zero, 10 special.
    #[inline]
    pub fn tag_of(v: F80) -> u16 {
        let e = v.se & 0x7FFF;
        if e == 0x7FFF {
            2
        } else if e == 0 {
            if v.mant == 0 {
                1
            } else {
                2
            }
        } else if v.mant >> 63 != 0 {
            0
        } else {
            2
        }
    }
    #[inline(always)]
    fn set_phys(&mut self, p: usize, v: F80) {
        self.st[p] = v;
        self.tag = (self.tag & !(3 << (2 * p))) | (Fpu::tag_of(v) << (2 * p));
    }
    /// Read ST(i). An empty register is a stack underflow: IE|SF (C1 = 0) and the indefinite is used.
    #[inline(always)]
    fn rd(&mut self, i: usize) -> F80 {
        let p = self.phys(i);
        if self.is_empty_phys(p) {
            self.sw = (self.sw | IE | SF) & !C1;
            INDEF
        } else {
            self.st[p]
        }
    }
    #[inline(always)]
    fn wr(&mut self, i: usize, v: F80) {
        let p = self.phys(i);
        self.set_phys(p, v);
    }
    #[inline(always)]
    fn push(&mut self, v: F80) {
        let np = (self.top.wrapping_sub(1) & 7) as usize;
        let mut v = v;
        if !self.is_empty_phys(np) {
            // Stack overflow: IE|SF, C1 = 1, the indefinite is pushed.
            self.sw |= IE | SF | C1;
            v = INDEF;
        }
        self.top = np as u8;
        self.set_phys(np, v);
    }
    #[inline(always)]
    fn pop(&mut self) {
        let p = self.top as usize;
        self.tag |= 3 << (2 * p);
        self.top = (self.top + 1) & 7;
    }

    /// Status word as seen by software: TOP merged in, ES/B derived from unmasked exceptions.
    pub fn status(&self) -> u16 {
        let mut s = (self.sw & !(0x3800 | 0x0080 | 0x8000)) | ((self.top as u16 & 7) << 11);
        if self.sw & 0x3F & !self.cw != 0 {
            s |= 0x8080;
        }
        s
    }
    fn set_status(&mut self, v: u16) {
        self.top = ((v >> 11) & 7) as u8;
        self.sw = v & !(0x3800 | 0x0080 | 0x8000);
    }

    /// FNINIT.
    pub fn init(&mut self) {
        self.cw = 0x037F;
        self.sw = 0;
        self.top = 0;
        self.tag = 0xFFFF;
        self.last_ip = 0;
        self.last_dp = 0;
        self.last_op = 0;
    }

    /// Set C3/C2/C0 from a comparison result (C1 untouched).
    fn set_cmp_cc(&mut self, c: Cmp) {
        self.sw &= !(C0 | C2 | C3);
        self.sw |= match c {
            Cmp::Gt => 0,
            Cmp::Lt => C0,
            Cmp::Eq => C3,
            Cmp::Un => C3 | C2 | C0,
        };
    }

    // ---- environment images ---------------------------------------------------------------
    fn env_bytes(&self, cs: u16, ds: u16, wide: bool) -> ([u8; 28], usize) {
        let mut b = [0u8; 28];
        let w16 = |b: &mut [u8], o: usize, v: u16| b[o..o + 2].copy_from_slice(&v.to_le_bytes());
        let w32 = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        if wide {
            w16(&mut b, 0, self.cw);
            w16(&mut b, 4, self.status());
            w16(&mut b, 8, self.tag);
            w32(&mut b, 12, self.last_ip);
            w32(&mut b, 16, cs as u32 | ((self.last_op as u32 & 0x7FF) << 16));
            w32(&mut b, 20, self.last_dp);
            w32(&mut b, 24, ds as u32);
            (b, 28)
        } else {
            // 16-bit protected-mode layout: CW SW TW, FIP offset, FCS selector, FDP offset, FDS selector.
            w16(&mut b, 0, self.cw);
            w16(&mut b, 2, self.status());
            w16(&mut b, 4, self.tag);
            w16(&mut b, 6, self.last_ip as u16);
            w16(&mut b, 8, cs);
            w16(&mut b, 10, self.last_dp as u16);
            w16(&mut b, 12, ds);
            (b, 14)
        }
    }

    /// After loading a tag word from memory only the "empty" information is taken from the image; the
    /// valid/zero/special classes of non-empty registers are derived from the register contents.
    fn retag_from_contents(&mut self) {
        for p in 0..8 {
            if !self.is_empty_phys(p) {
                self.tag = (self.tag & !(3 << (2 * p))) | (Fpu::tag_of(self.st[p]) << (2 * p));
            }
        }
    }

    fn load_env(&mut self, b: &[u8], wide: bool) {
        let r16 = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let r32 = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        if wide {
            self.cw = r16(0);
            self.set_status(r16(4));
            self.tag = r16(8);
            self.last_ip = r32(12);
            self.last_op = ((r32(16) >> 16) & 0x7FF) as u16;
            self.last_dp = r32(20);
        } else {
            self.cw = r16(0);
            self.set_status(r16(2));
            self.tag = r16(4);
            self.last_ip = r16(6) as u32;
            self.last_op = 0;
            self.last_dp = r16(10) as u32;
        }
    }

    fn reg_bytes(&self, i: usize) -> [u8; 10] {
        let v = self.st[self.phys(i)];
        let mut b = [0u8; 10];
        b[..8].copy_from_slice(&v.mant.to_le_bytes());
        b[8..].copy_from_slice(&v.se.to_le_bytes());
        b
    }
    fn reg_from_bytes(b: &[u8]) -> F80 {
        F80 { mant: u64::from_le_bytes(b[..8].try_into().unwrap()), se: u16::from_le_bytes([b[8], b[9]]) }
    }

    /// Abridged (FXSAVE) tag: bit p set when physical register p is not empty.
    fn abridged_tag(&self) -> u8 {
        let mut t = 0u8;
        for p in 0..8 {
            if !self.is_empty_phys(p) {
                t |= 1 << p;
            }
        }
        t
    }
}

// ---- guest memory helpers (protection-checked, restartable on fault) -----------------------
fn read_buf(mem: &Memory, addr: u32, out: &mut [u8]) -> XResult<()> {
    let mut o = 0;
    while out.len() - o >= 8 {
        out[o..o + 8].copy_from_slice(&mem.r64(addr.wrapping_add(o as u32))?.to_le_bytes());
        o += 8;
    }
    while o < out.len() {
        out[o] = mem.r8(addr.wrapping_add(o as u32))?;
        o += 1;
    }
    Ok(())
}

fn write_buf(mem: &mut Memory, addr: u32, src: &[u8]) -> XResult<()> {
    // Probe the last byte first so a fault on a page-straddling store leaves memory untouched.
    if !src.is_empty() {
        let last = addr.wrapping_add(src.len() as u32 - 1);
        let b = mem.r8(last)?;
        mem.w8(last, b)?;
    }
    let mut o = 0;
    while src.len() - o >= 8 {
        mem.w64(addr.wrapping_add(o as u32), u64::from_le_bytes(src[o..o + 8].try_into().unwrap()))?;
        o += 8;
    }
    while o < src.len() {
        mem.w8(addr.wrapping_add(o as u32), src[o])?;
        o += 1;
    }
    Ok(())
}

#[inline(always)]
fn ld80(mem: &Memory, a: u32) -> XResult<F80> {
    Ok(F80 { mant: mem.r64(a)?, se: mem.r16(a.wrapping_add(8))? })
}

#[inline(always)]
fn st80(mem: &mut Memory, a: u32, v: F80) -> XResult<()> {
    // Probe first so that a fault on the second half cannot leave a half-written value behind.
    let tail = a.wrapping_add(9);
    let b = mem.r8(tail)?;
    mem.w8(tail, b)?;
    mem.w64(a, v.mant)?;
    mem.w16(a.wrapping_add(8), v.se)?;
    Ok(())
}

fn arith_op(mn: M) -> Op {
    match mn {
        M::Fadd | M::Faddp | M::Fiadd => Op::Add,
        M::Fsub | M::Fsubp | M::Fisub => Op::Sub,
        M::Fsubr | M::Fsubrp | M::Fisubr => Op::SubR,
        M::Fmul | M::Fmulp | M::Fimul => Op::Mul,
        M::Fdiv | M::Fdivp | M::Fidiv => Op::Div,
        _ => Op::DivR,
    }
}

impl Cpu {
    /// Common prologue of data instructions: C1 is cleared (stack faults / rounding set it again) and
    /// the last-instruction pointer updated.
    #[inline(always)]
    fn fbegin(&mut self, ins: &Instruction) {
        self.fpu.sw &= !C1;
        self.fpu.last_ip = ins.ip() as u32;
    }

    #[inline(always)]
    fn fea(&mut self, ins: &Instruction) -> XResult<u32> {
        let a = self.ea(ins)?;
        self.fpu.last_dp = a;
        Ok(a)
    }

    /// Execute an x87 instruction (mnemonic in F2XM1..=FYL2XP1, including FXSAVE/FXRSTOR).
    pub fn exec_x87(&mut self, mem: &mut Memory, ins: &Instruction) -> XResult<()> {
        let mn = ins.mnemonic();
        match mn {
            // ---- loads --------------------------------------------------------------------
            M::Fld => {
                self.fbegin(ins);
                if ins.op0_kind() == OpKind::Memory {
                    let a = self.fea(ins)?;
                    let v = match ins.memory_size().size() {
                        4 => {
                            let b = mem.r32(a)?;
                            self.fpu.f32_to_f80(b)
                        }
                        8 => {
                            let b = mem.r64(a)?;
                            self.fpu.f64_to_f80(b)
                        }
                        _ => ld80(mem, a)?,
                    };
                    self.fpu.push(v);
                } else {
                    let v = self.fpu.rd(sti(ins.op0_register()));
                    self.fpu.push(v);
                }
            }
            M::Fild => {
                self.fbegin(ins);
                let a = self.fea(ins)?;
                let size = ins.memory_size().size();
                self.fild_core(mem, a, size)?;
            }
            M::Fbld => {
                self.fbegin(ins);
                let a = self.fea(ins)?;
                let mut b = [0u8; 10];
                read_buf(mem, a, &mut b)?;
                self.fpu.push(Fpu::bcd_to_f80(&b));
            }
            M::Fld1 | M::Fldl2t | M::Fldl2e | M::Fldpi | M::Fldlg2 | M::Fldln2 | M::Fldz => {
                self.fbegin(ins);
                let idx = match mn {
                    M::Fld1 => 0,
                    M::Fldl2t => 1,
                    M::Fldl2e => 2,
                    M::Fldpi => 3,
                    M::Fldlg2 => 4,
                    M::Fldln2 => 5,
                    _ => 6,
                };
                let v = self.fpu.fld_const(idx);
                self.fpu.push(v);
            }
            // ---- stores -------------------------------------------------------------------
            M::Fst | M::Fstp | M::Fstpnce => {
                self.fbegin(ins);
                let pop = mn != M::Fst;
                if ins.op0_kind() == OpKind::Memory {
                    let a = self.fea(ins)?;
                    let v = self.fpu.rd(0);
                    match ins.memory_size().size() {
                        4 => {
                            let b = self.fpu.f80_to_f32(v);
                            mem.w32(a, b)?;
                        }
                        8 => {
                            let b = self.fpu.f80_to_f64(v);
                            mem.w64(a, b)?;
                        }
                        _ => st80(mem, a, v)?,
                    }
                } else {
                    let v = self.fpu.rd(0);
                    self.fpu.wr(sti(ins.op0_register()), v);
                }
                if pop {
                    self.fpu.pop();
                }
            }
            M::Fist | M::Fistp | M::Fisttp => {
                self.fbegin(ins);
                let a = self.fea(ins)?;
                let rc = if mn == M::Fisttp { 3 } else { self.fpu.rc() };
                let size = ins.memory_size().size();
                self.fist_core(mem, a, rc, size, mn != M::Fist)?;
            }
            M::Fbstp => {
                self.fbegin(ins);
                let a = self.fea(ins)?;
                let v = self.fpu.rd(0);
                let b = self.fpu.f80_to_bcd(v);
                write_buf(mem, a, &b)?;
                self.fpu.pop();
            }
            M::Fxch => {
                self.fbegin(ins);
                let i = src_reg(ins);
                let a = self.fpu.rd(0);
                let b = self.fpu.rd(i);
                self.fpu.wr(0, b);
                self.fpu.wr(i, a);
            }
            // ---- arithmetic ---------------------------------------------------------------
            M::Fadd | M::Fsub | M::Fsubr | M::Fmul | M::Fdiv | M::Fdivr => {
                self.fbegin(ins);
                let op = arith_op(mn);
                let p = self.fpu.pc_bits();
                if ins.op0_kind() == OpKind::Memory {
                    let a = self.fea(ins)?;
                    let b = if ins.memory_size().size() == 4 {
                        let x = mem.r32(a)?;
                        self.fpu.f32_to_f80(x)
                    } else {
                        let x = mem.r64(a)?;
                        self.fpu.f64_to_f80(x)
                    };
                    let d = self.fpu.rd(0);
                    let r = self.fpu.arith(op, d, b, p);
                    self.fpu.wr(0, r);
                } else {
                    let di = sti(ins.op0_register());
                    let si = sti(ins.op1_register());
                    let d = self.fpu.rd(di);
                    let s = self.fpu.rd(si);
                    let r = self.fpu.arith(op, d, s, p);
                    self.fpu.wr(di, r);
                }
            }
            M::Faddp | M::Fsubp | M::Fsubrp | M::Fmulp | M::Fdivp | M::Fdivrp => {
                self.fbegin(ins);
                let op = arith_op(mn);
                let p = self.fpu.pc_bits();
                let di = sti(ins.op0_register());
                let si = if ins.op_count() >= 2 { sti(ins.op1_register()) } else { 0 };
                let d = self.fpu.rd(di);
                let s = self.fpu.rd(si);
                let r = self.fpu.arith(op, d, s, p);
                self.fpu.wr(di, r);
                self.fpu.pop();
            }
            M::Fiadd | M::Fisub | M::Fisubr | M::Fimul | M::Fidiv | M::Fidivr => {
                self.fbegin(ins);
                let op = arith_op(mn);
                let p = self.fpu.pc_bits();
                let a = self.fea(ins)?;
                let iv = if ins.memory_size().size() == 2 { mem.r16(a)? as i16 as i64 } else { mem.r32(a)? as i32 as i64 };
                let d = self.fpu.rd(0);
                let r = self.fpu.arith(op, d, F80::from_i64(iv), p);
                self.fpu.wr(0, r);
            }
            M::Fchs | M::Fabs => {
                self.fbegin(ins);
                let v = self.fpu.rd(0);
                self.fpu.wr(0, if mn == M::Fchs { v.neg() } else { v.abs() });
            }
            M::Fsqrt => {
                self.fbegin(ins);
                let p = self.fpu.pc_bits();
                let v = self.fpu.rd(0);
                let r = self.fpu.fsqrt(v, p);
                self.fpu.wr(0, r);
            }
            M::Frndint => {
                self.fbegin(ins);
                let v = self.fpu.rd(0);
                let r = self.fpu.frndint(v);
                self.fpu.wr(0, r);
            }
            M::Fscale => {
                self.fbegin(ins);
                let a = self.fpu.rd(0);
                let b = self.fpu.rd(1);
                let r = self.fpu.fscale(a, b);
                self.fpu.wr(0, r);
            }
            M::Fxtract => {
                self.fbegin(ins);
                let v = self.fpu.rd(0);
                let (e, s) = self.fpu.fxtract(v);
                self.fpu.wr(0, e);
                self.fpu.push(s);
            }
            M::Fprem | M::Fprem1 => {
                self.fbegin(ins);
                let a = self.fpu.rd(0);
                let b = self.fpu.rd(1);
                let r = self.fpu.fprem(a, b, mn == M::Fprem1);
                self.fpu.wr(0, r);
            }
            M::F2xm1 => {
                self.fbegin(ins);
                let v = self.fpu.rd(0);
                let r = self.fpu.f2xm1(v);
                self.fpu.wr(0, r);
            }
            M::Fyl2x | M::Fyl2xp1 => {
                self.fbegin(ins);
                let x = self.fpu.rd(0);
                let y = self.fpu.rd(1);
                let r = self.fpu.fyl2x(y, x, mn == M::Fyl2xp1);
                self.fpu.wr(1, r);
                self.fpu.pop();
            }
            M::Fpatan => {
                self.fbegin(ins);
                let x = self.fpu.rd(0);
                let y = self.fpu.rd(1);
                let r = self.fpu.fpatan(y, x);
                self.fpu.wr(1, r);
                self.fpu.pop();
            }
            M::Fsin | M::Fcos | M::Fsincos | M::Fptan => {
                self.fbegin(ins);
                self.fpu.sw &= !C2;
                let x = self.fpu.rd(0);
                let want = match mn {
                    M::Fsin => 1,
                    M::Fcos => 2,
                    M::Fsincos => 3,
                    _ => 4,
                };
                match self.fpu.trig(x, want) {
                    None => self.fpu.sw |= C2, // |x| >= 2^63: operand left unchanged
                    Some(r) => match mn {
                        M::Fsin => self.fpu.wr(0, r[0]),
                        M::Fcos => self.fpu.wr(0, r[1]),
                        M::Fsincos => {
                            self.fpu.wr(0, r[0]);
                            self.fpu.push(r[1]);
                        }
                        _ => {
                            self.fpu.wr(0, r[2]);
                            self.fpu.push(ONE);
                        }
                    },
                }
            }
            // ---- comparison ---------------------------------------------------------------
            M::Fcom | M::Fcomp | M::Fucom | M::Fucomp => {
                self.fbegin(ins);
                let signal = matches!(mn, M::Fcom | M::Fcomp);
                let b = if ins.op0_kind() == OpKind::Memory {
                    let a = self.fea(ins)?;
                    if ins.memory_size().size() == 4 {
                        let x = mem.r32(a)?;
                        self.fpu.f32_to_f80(x)
                    } else {
                        let x = mem.r64(a)?;
                        self.fpu.f64_to_f80(x)
                    }
                } else {
                    self.fpu.rd(src_reg(ins))
                };
                let a = self.fpu.rd(0);
                let c = self.fpu.fcmp(a, b, signal);
                self.fpu.set_cmp_cc(c);
                if matches!(mn, M::Fcomp | M::Fucomp) {
                    self.fpu.pop();
                }
            }
            M::Fcompp | M::Fucompp => {
                self.fbegin(ins);
                let a = self.fpu.rd(0);
                let b = self.fpu.rd(1);
                let c = self.fpu.fcmp(a, b, mn == M::Fcompp);
                self.fpu.set_cmp_cc(c);
                self.fpu.pop();
                self.fpu.pop();
            }
            M::Ficom | M::Ficomp => {
                self.fbegin(ins);
                let a = self.fea(ins)?;
                let iv = if ins.memory_size().size() == 2 { mem.r16(a)? as i16 as i64 } else { mem.r32(a)? as i32 as i64 };
                let d = self.fpu.rd(0);
                let c = self.fpu.fcmp(d, F80::from_i64(iv), true);
                self.fpu.set_cmp_cc(c);
                if mn == M::Ficomp {
                    self.fpu.pop();
                }
            }
            M::Ftst => {
                self.fbegin(ins);
                let a = self.fpu.rd(0);
                let c = self.fpu.fcmp(a, F80::zero(false), true);
                self.fpu.set_cmp_cc(c);
            }
            M::Fxam => {
                self.fpu.last_ip = ins.ip() as u32;
                let p = self.fpu.phys(0);
                let bits = Fpu::fxam_bits(self.fpu.st[p], self.fpu.is_empty_phys(p));
                self.fpu.sw = (self.fpu.sw & !CC_ALL) | bits;
            }
            M::Fcomi | M::Fcomip | M::Fucomi | M::Fucomip => {
                self.fbegin(ins);
                let signal = matches!(mn, M::Fcomi | M::Fcomip);
                let pop = matches!(mn, M::Fcomip | M::Fucomip);
                self.fcomi_core(src_reg(ins), signal, pop);
            }
            M::Fcmovb | M::Fcmove | M::Fcmovbe | M::Fcmovu | M::Fcmovnb | M::Fcmovne | M::Fcmovnbe | M::Fcmovnu => {
                self.fbegin(ins);
                let cond = match mn {
                    M::Fcmovb => self.flag(CF),
                    M::Fcmove => self.flag(ZF),
                    M::Fcmovbe => self.flag(CF) || self.flag(ZF),
                    M::Fcmovu => self.flag(PF),
                    M::Fcmovnb => !self.flag(CF),
                    M::Fcmovne => !self.flag(ZF),
                    M::Fcmovnbe => !(self.flag(CF) || self.flag(ZF)),
                    _ => !self.flag(PF),
                };
                let s = self.fpu.rd(src_reg(ins));
                let _ = self.fpu.rd(0);
                if cond {
                    self.fpu.wr(0, s);
                }
            }
            // ---- control ------------------------------------------------------------------
            M::Fnstcw | M::Fstcw => {
                let a = self.ea(ins)?;
                mem.w16(a, self.fpu.cw)?;
            }
            M::Fldcw => {
                let a = self.ea(ins)?;
                self.fpu.cw = mem.r16(a)?;
            }
            M::Fnstsw | M::Fstsw => {
                let s = self.fpu.status();
                if ins.op0_kind() == OpKind::Memory {
                    let a = self.ea(ins)?;
                    mem.w16(a, s)?;
                } else {
                    self.set_reg(Register::AX, s as u32);
                }
            }
            M::Fnclex | M::Fclex => self.fpu.sw &= !0x80FF,
            M::Fninit | M::Finit => self.fpu.init(),
            M::Fnop | M::Feni | M::Fneni | M::Fdisi | M::Fndisi | M::Fsetpm | M::Fnsetpm => {}
            M::Ffree | M::Ffreep => {
                let p = self.fpu.phys(sti(ins.op0_register()));
                self.fpu.tag |= 3 << (2 * p);
                if mn == M::Ffreep {
                    self.fpu.top = (self.fpu.top + 1) & 7;
                }
            }
            M::Fdecstp => {
                self.fpu.sw &= !C1;
                self.fpu.top = self.fpu.top.wrapping_sub(1) & 7;
            }
            M::Fincstp => {
                self.fpu.sw &= !C1;
                self.fpu.top = (self.fpu.top + 1) & 7;
            }
            M::Fnstenv | M::Fstenv => {
                let a = self.ea(ins)?;
                let wide = ins.memory_size().size() >= 28;
                let (b, n) = self.fpu.env_bytes(self.seg[1], self.seg[3], wide);
                write_buf(mem, a, &b[..n])?;
                self.fpu.cw |= 0x3F; // FNSTENV masks all exceptions
            }
            M::Fldenv => {
                let a = self.ea(ins)?;
                let wide = ins.memory_size().size() >= 28;
                let mut b = [0u8; 28];
                let n = if wide { 28 } else { 14 };
                read_buf(mem, a, &mut b[..n])?;
                self.fpu.load_env(&b[..n], wide);
                self.fpu.retag_from_contents();
            }
            M::Fnsave | M::Fsave => {
                let a = self.ea(ins)?;
                let wide = ins.memory_size().size() >= 108;
                let (env, n) = self.fpu.env_bytes(self.seg[1], self.seg[3], wide);
                let mut buf = [0u8; 108];
                buf[..n].copy_from_slice(&env[..n]);
                for i in 0..8 {
                    buf[n + 10 * i..n + 10 * i + 10].copy_from_slice(&self.fpu.reg_bytes(i));
                }
                write_buf(mem, a, &buf[..n + 80])?;
                self.fpu.init();
            }
            M::Frstor => {
                let a = self.ea(ins)?;
                let wide = ins.memory_size().size() >= 108;
                let n = if wide { 28 } else { 14 };
                let mut buf = [0u8; 108];
                read_buf(mem, a, &mut buf[..n + 80])?;
                self.fpu.load_env(&buf[..n], wide);
                for i in 0..8 {
                    let p = self.fpu.phys(i);
                    self.fpu.st[p] = Fpu::reg_from_bytes(&buf[n + 10 * i..n + 10 * i + 10]);
                }
                self.fpu.retag_from_contents();
            }
            M::Fxsave => {
                let a = self.ea(ins)?;
                let mut buf = [0u8; 512];
                let f = &self.fpu;
                buf[0..2].copy_from_slice(&f.cw.to_le_bytes());
                buf[2..4].copy_from_slice(&f.status().to_le_bytes());
                buf[4] = f.abridged_tag();
                buf[6..8].copy_from_slice(&(f.last_op & 0x7FF).to_le_bytes());
                buf[8..12].copy_from_slice(&f.last_ip.to_le_bytes());
                buf[12..14].copy_from_slice(&self.seg[1].to_le_bytes());
                buf[16..20].copy_from_slice(&f.last_dp.to_le_bytes());
                buf[20..22].copy_from_slice(&self.seg[3].to_le_bytes());
                buf[24..28].copy_from_slice(&self.mxcsr.to_le_bytes());
                buf[28..32].copy_from_slice(&0xFFFFu32.to_le_bytes());
                for i in 0..8 {
                    buf[32 + 16 * i..42 + 16 * i].copy_from_slice(&f.reg_bytes(i));
                    buf[160 + 16 * i..176 + 16 * i].copy_from_slice(&self.xmm[i].to_le_bytes());
                }
                write_buf(mem, a, &buf)?;
            }
            M::Fxrstor => {
                let a = self.ea(ins)?;
                let mut buf = [0u8; 512];
                read_buf(mem, a, &mut buf)?;
                let r16 = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
                let r32 = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]);
                self.fpu.cw = r16(0);
                self.fpu.set_status(r16(2));
                self.fpu.last_op = r16(6) & 0x7FF;
                self.fpu.last_ip = r32(8);
                self.fpu.last_dp = r32(16);
                self.mxcsr = r32(24) & 0xFFFF;
                let abridged = buf[4];
                self.fpu.tag = 0xFFFF;
                for i in 0..8 {
                    let p = self.fpu.phys(i);
                    let v = Fpu::reg_from_bytes(&buf[32 + 16 * i..42 + 16 * i]);
                    self.fpu.st[p] = v;
                    if abridged & (1 << p) != 0 {
                        self.fpu.tag = (self.fpu.tag & !(3 << (2 * p))) | (Fpu::tag_of(v) << (2 * p));
                    }
                    self.xmm[i] = u128::from_le_bytes(buf[160 + 16 * i..176 + 16 * i].try_into().unwrap());
                }
            }
            _ => return Err(Exit::Unimpl { eip: ins.ip() as u32, what: format!("{:?}", ins.code()) }),
        }
        Ok(())
    }

    // ---- cores shared with the JIT's inline helpers (jit.rs `h_*`) ---------------------------
    // Single source of truth: the interpreter arms and the compiled-code helpers run the same code,
    // so an inlined form can never drift from the differential-tested interpreter.

    /// FIST/FISTP: store ST(0) rounded to a `size`-byte integer at `a` (rc 3 = truncate), pop if asked.
    pub fn fist_core(&mut self, m: &mut Memory, a: u32, rc: u8, size: usize, pop: bool) -> XResult<()> {
        let v = self.fpu.rd(0);
        let r = self.fpu.f80_to_int(v, rc, size as u32 * 8);
        match size {
            2 => m.w16(a, r.map_or(0x8000, |x| x as u16))?,
            4 => m.w32(a, r.map_or(0x8000_0000, |x| x as u32))?,
            _ => m.w64(a, r.map_or(0x8000_0000_0000_0000, |x| x as u64))?,
        }
        if pop {
            self.fpu.pop();
        }
        Ok(())
    }

    /// FILD: push the `size`-byte integer at `a`.
    pub fn fild_core(&mut self, m: &Memory, a: u32, size: usize) -> XResult<()> {
        let v = match size {
            2 => m.r16(a)? as i16 as i64,
            4 => m.r32(a)? as i32 as i64,
            _ => m.r64(a)? as i64,
        };
        self.fpu.push(F80::from_i64(v));
        Ok(())
    }

    /// FCOMI/FCOMIP/FUCOMI/FUCOMIP: compare ST(0) with ST(`bi`), set ZF/PF/CF, pop if asked.
    pub fn fcomi_core(&mut self, bi: usize, signal: bool, pop: bool) {
        let a = self.fpu.rd(0);
        let b = self.fpu.rd(bi);
        let c = self.fpu.fcmp(a, b, signal);
        let (z, p, cf) = match c {
            Cmp::Gt => (false, false, false),
            Cmp::Lt => (false, false, true),
            Cmp::Eq => (true, false, false),
            Cmp::Un => (true, true, true),
        };
        self.set_flag(ZF, z);
        self.set_flag(PF, p);
        self.set_flag(CF, cf);
        self.set_flag(OF | EF_SF | AF, false);
        if pop {
            self.fpu.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use iced_x86::{Decoder, DecoderOptions, Instruction};

    fn dec(bytes: &[u8]) -> Vec<String> {
        let mut d = Decoder::with_ip(32, bytes, 0x1000, DecoderOptions::NONE);
        let mut out = vec![];
        while d.can_decode() {
            let mut i = Instruction::default();
            d.decode_out(&mut i);
            out.push(format!("{:?}/{:?}/{}", i.mnemonic(), i.code(), i.op_count()));
        }
        out
    }

    #[test]
    fn decode_shapes() {
        for b in [
            &[0x9B, 0xDF, 0xE0][..], // fstsw ax
            &[0x9B, 0xDD, 0x7D, 0x00],
            &[0x9B, 0xDB, 0xE3],
            &[0x9B, 0xDB, 0xE2],
            &[0x9B, 0xD9, 0x7D, 0x00],
            &[0xD9, 0xC9],             // fxch
            &[0xD8, 0xD1],             // fcom st(1)
            &[0xDE, 0xC1],             // faddp st(1), st
            &[0xDA, 0xE9],             // fucompp
            &[0xDE, 0xD9],             // fcompp
            &[0xD9, 0xD8],             // fstp st(0)  (fstpnce)
            &[0xDD, 0xC8],             // fxch alias
            &[0xDF, 0xC0],             // ffreep
            &[0xD8, 0xD0],
            &[0xDE, 0xE1],             // fsubrp / fsubp
            &[0xDC, 0xE1],
            &[0xD8, 0xE1],
        ] {
            println!("{:02X?} -> {:?}", b, dec(b));
        }
    }

    // ---- instruction-level tests for SDM behaviour the QEMU oracle does not model ---------------------
    use super::*;
    use crate::cpu::Cpu;
    use crate::mem::{Memory, P_MAPPED, P_R, P_W};

    fn rig() -> (Cpu, Memory) {
        let mut mem = Memory::new(1 << 16);
        mem.set_prot(0, 1 << 16, P_R | P_W | P_MAPPED);
        (Cpu::new(), mem)
    }

    /// Execute x87 (and FWAIT) instructions encoded in `code`.
    fn run(c: &mut Cpu, m: &mut Memory, code: &[u8]) {
        let mut d = Decoder::with_ip(32, code, 0x1000, DecoderOptions::NONE);
        while d.can_decode() {
            let ins = d.decode();
            c.eip = ins.next_ip() as u32;
            if ins.mnemonic() == iced_x86::Mnemonic::Wait {
                continue;
            }
            c.exec_x87(m, &ins).unwrap();
        }
    }
    fn st(c: &Cpu, i: usize) -> F80 {
        c.fpu.st[(c.fpu.top as usize + i) & 7]
    }
    const FLD1: [u8; 2] = [0xD9, 0xE8];

    #[test]
    fn stack_overflow_gives_indefinite() {
        let (mut c, mut m) = rig();
        for _ in 0..8 {
            run(&mut c, &mut m, &FLD1);
        }
        assert_eq!(c.fpu.status() & 0x0241, 0, "no fault yet");
        run(&mut c, &mut m, &FLD1);
        assert_eq!(c.fpu.status() & 0x0241, 0x0241, "IE|SF|C1");
        assert_eq!(st(&c, 0), INDEF);
    }

    #[test]
    fn stack_underflow_gives_indefinite() {
        let (mut c, mut m) = rig();
        run(&mut c, &mut m, &[0xD8, 0xC1]); // fadd st0, st1 on an empty stack
        assert_eq!(c.fpu.status() & 0x0241, 0x0041, "IE|SF, C1 clear");
        assert_eq!(st(&c, 0), INDEF);
        // FSTP m32 of an empty register stores the single-precision indefinite
        let mut c = Cpu::new();
        run(&mut c, &mut m, &[0xD9, 0x1D, 0x00, 0x20, 0x00, 0x00]); // fstp dword [0x2000]
        assert_eq!(m.r32(0x2000).unwrap(), 0xFFC0_0000);
        assert_eq!(c.fpu.status() & 0x41, 0x41);
    }

    #[test]
    fn unsupported_formats_are_invalid() {
        let (mut c, mut m) = rig();
        // unnormal: exponent 0x3fff with the integer bit clear
        m.w64(0x2000, 0x4000_0000_0000_0000).unwrap();
        m.w16(0x2008, 0x3FFF).unwrap();
        run(&mut c, &mut m, &[0xDB, 0x2D, 0x00, 0x20, 0x00, 0x00]); // fld tbyte [0x2000]
        assert_eq!(c.fpu.status() & 1, 0, "FLD m80 does not validate");
        run(&mut c, &mut m, &[0xD9, 0xE5]); // fxam
        assert_eq!(c.fpu.status() & 0x4700, 0x0000, "unsupported");
        run(&mut c, &mut m, &[0xD8, 0xC0]); // fadd st0, st0
        assert_eq!(c.fpu.status() & 1, 1);
        assert_eq!(st(&c, 0), INDEF);
        // FIST of NaN stores the integer indefinite
        let mut c = Cpu::new();
        c.fpu.init();
        m.w64(0x2000, 0xC000_0000_0000_0000).unwrap();
        m.w16(0x2008, 0x7FFF).unwrap();
        run(&mut c, &mut m, &[0xDB, 0x2D, 0x00, 0x20, 0x00, 0x00, 0xDB, 0x1D, 0x10, 0x20, 0x00, 0x00]); // fld tbyte; fistp dword
        assert_eq!(m.r32(0x2010).unwrap(), 0x8000_0000);
        assert_eq!(c.fpu.status() & 1, 1);
    }

    #[test]
    fn fprem_quotient_bits() {
        let (mut c, mut m) = rig();
        // 3.0 then 5.0 pushed: ST0 = 5, ST1 = 3
        m.w64(0x2000, 0x4008_0000_0000_0000).unwrap(); // 3.0
        m.w64(0x2008, 0x4014_0000_0000_0000).unwrap(); // 5.0
        let prog = [0xDD, 0x05, 0x00, 0x20, 0x00, 0x00, 0xDD, 0x05, 0x08, 0x20, 0x00, 0x00]; // fld [2000]; fld [2008]
        run(&mut c, &mut m, &prog);
        run(&mut c, &mut m, &[0xD9, 0xF8]); // fprem: 5 mod 3 = 2, q = 1
        assert_eq!(st(&c, 0), F80 { se: 0x4000, mant: 0x8000_0000_0000_0000 });
        let sw = c.fpu.status();
        assert_eq!((sw & 0x4700, sw & 0x0200 != 0), (0x0200, true), "Q0 -> C1");
        let mut c = Cpu::new();
        run(&mut c, &mut m, &prog);
        run(&mut c, &mut m, &[0xD9, 0xF5]); // fprem1: nearest quotient 2, remainder -1
        assert_eq!(st(&c, 0), F80 { se: 0xBFFF, mant: 0x8000_0000_0000_0000 });
        assert_eq!(c.fpu.status() & 0x4700, 0x4000, "Q1 -> C3");
    }

    #[test]
    fn c1_reports_round_up() {
        let (mut c, mut m) = rig();
        // 1.0 + 2^-65 (extended): inexact; round-up mode increments the significand => C1
        m.w64(0x2000, 0x8000_0000_0000_0000).unwrap();
        m.w16(0x2008, 0x3FFF - 65).unwrap();
        let prog = [0xDB, 0x2D, 0x00, 0x20, 0x00, 0x00, 0xD9, 0xE8, 0xDE, 0xC1]; // fld tbyte; fld1; faddp st1
        run(&mut c, &mut m, &prog);
        assert_eq!(c.fpu.status() & 0x0220, 0x0020, "nearest: inexact, not rounded up");
        let mut c = Cpu::new();
        c.fpu.cw = 0x0B7F; // round up
        run(&mut c, &mut m, &prog);
        assert_eq!(c.fpu.status() & 0x0220, 0x0220, "up: inexact and C1");
        assert_eq!(st(&c, 0).mant, 0x8000_0000_0000_0001);
    }

    #[test]
    fn tags_follow_contents() {
        let (mut c, mut m) = rig();
        run(&mut c, &mut m, &[0xD9, 0xEE]); // fldz
        assert_eq!(c.fpu.tag & 0xC000, 0x4000, "zero");
        run(&mut c, &mut m, &[0xD9, 0xE8]); // fld1
        assert_eq!(c.fpu.tag & 0x3000, 0x0000, "valid");
        m.w64(0x2000, 1).unwrap();
        m.w16(0x2008, 0).unwrap();
        run(&mut c, &mut m, &[0xDB, 0x2D, 0x00, 0x20, 0x00, 0x00]); // denormal
        assert_eq!(c.fpu.tag & 0x0C00, 0x0800, "special");
        run(&mut c, &mut m, &[0xDD, 0xD8]); // fstp st0
        assert_eq!(c.fpu.tag & 0x0C00, 0x0C00, "empty after pop");
        run(&mut c, &mut m, &[0xDD, 0xC0]); // ffree st0 (the 1.0)
        assert_eq!(c.fpu.tag & 0x3000, 0x3000);
    }

    #[test]
    fn fcomi_clears_other_flags() {
        let (mut c, mut m) = rig();
        c.fl |= crate::cpu::OF | crate::cpu::SF | crate::cpu::AF;
        run(&mut c, &mut m, &[0xD9, 0xE8, 0xD9, 0xE8, 0xDB, 0xF1]); // fld1; fld1; fcomi st0, st1
        assert!(c.fl & crate::cpu::ZF != 0 && c.fl & crate::cpu::CF == 0 && c.fl & crate::cpu::PF == 0);
        assert_eq!(c.fl & (crate::cpu::OF | crate::cpu::SF | crate::cpu::AF), 0);
    }

    #[test]
    fn trig_out_of_range_sets_c2() {
        let (mut c, mut m) = rig();
        m.w64(0x2000, 0x8000_0000_0000_0000).unwrap();
        m.w16(0x2008, 0x403E).unwrap(); // 2^63
        run(&mut c, &mut m, &[0xDB, 0x2D, 0x00, 0x20, 0x00, 0x00, 0xD9, 0xFE]); // fld tbyte; fsin
        assert_eq!(c.fpu.status() & 0x0400, 0x0400);
        assert_eq!(st(&c, 0), F80 { se: 0x403E, mant: 0x8000_0000_0000_0000 });
        // fptan in range pushes 1.0
        let mut c = Cpu::new();
        run(&mut c, &mut m, &[0xD9, 0xE8, 0xD9, 0xF2]); // fld1; fptan
        assert_eq!(c.fpu.status() & 0x0400, 0);
        assert_eq!(st(&c, 0), F80 { se: 0x3FFF, mant: 0x8000_0000_0000_0000 });
    }

    #[test]
    fn fxsave_roundtrip_and_env() {
        let (mut c, mut m) = rig();
        c.xmm[3] = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210;
        c.mxcsr = 0x1FA0;
        run(&mut c, &mut m, &[0xD9, 0xE8, 0xD9, 0xEE]); // fld1; fldz
        run(&mut c, &mut m, &[0x0F, 0xAE, 0x05, 0x00, 0x30, 0x00, 0x00]); // fxsave [0x3000]
        assert_eq!(m.r32(0x3000 + 28).unwrap(), 0xFFFF, "MXCSR_MASK");
        assert_eq!(m.r8(0x3004).unwrap(), 0xC0, "abridged tag: R7, R6 in use");
        let mut c2 = Cpu::new();
        run(&mut c2, &mut m, &[0x0F, 0xAE, 0x0D, 0x00, 0x30, 0x00, 0x00]); // fxrstor [0x3000]
        assert_eq!(c2.xmm[3], c.xmm[3]);
        assert_eq!(c2.mxcsr, 0x1FA0);
        assert_eq!(c2.fpu.top, 6);
        assert_eq!(c2.fpu.tag & 0xF000, 0x1000, "zero in R6... valid in R7");
        // fnstenv masks all exceptions
        c.fpu.cw = 0x0300;
        run(&mut c, &mut m, &[0xD9, 0x35, 0x00, 0x31, 0x00, 0x00]); // fnstenv [0x3100]
        assert_eq!(m.r16(0x3100).unwrap(), 0x0300, "image has the old CW");
        assert_eq!(c.fpu.cw, 0x033F);
    }

    /// Dev harness: `X87_VEC=in.txt X87_OUT=out.txt cargo test -p emu --lib trans_vectors -- --ignored`
    /// evaluates the transcendental instructions on vectors produced by an external high-precision
    /// reference and writes the raw results back for comparison.
    #[test]
    #[ignore]
    fn trans_vectors() {
        use super::{Fpu, F80};
        let (Ok(inp), Ok(outp)) = (std::env::var("X87_VEC"), std::env::var("X87_OUT")) else { return };
        let text = std::fs::read_to_string(inp).unwrap();
        let mut out = String::new();
        let h = |s: &str| u64::from_str_radix(s, 16).unwrap();
        for line in text.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            let mut f = Fpu::new();
            let a = F80 { se: h(t[1]) as u16, mant: h(t[2]) };
            let b = if t.len() > 4 { F80 { se: h(t[3]) as u16, mant: h(t[4]) } } else { F80::default() };
            let res: Vec<F80> = match t[0] {
                "f2xm1" => vec![f.f2xm1(a)],
                "fyl2x" => vec![f.fyl2x(b, a, false)],  // a = x (ST0), b = y (ST1)
                "fyl2xp1" => vec![f.fyl2x(b, a, true)],
                "fpatan" => vec![f.fpatan(b, a)],       // a = x (ST0), b = y (ST1)
                // each result is requested on its own so that the minimal-kernel paths are exercised
                "trig" => match (f.trig(a, 1), f.trig(a, 2), f.trig(a, 4), f.trig(a, 7)) {
                    (Some(s), Some(c), Some(t), Some(all)) => {
                        assert_eq!((s[0], c[1], t[2]), (all[0], all[1], all[2]));
                        vec![s[0], c[1], t[2]]
                    }
                    _ => vec![],
                },
                _ => panic!("bad op"),
            };
            out.push_str(&format!("{} sw={:04x}", t[0], f.sw));
            for r in res {
                out.push_str(&format!(" {:04x}:{:016x}", r.se, r.mant));
            }
            out.push('\n');
        }
        std::fs::write(outp, out).unwrap();
    }
}
