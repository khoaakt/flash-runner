//! Inline translation of packed single-precision SSE arithmetic (ADDPS, SUBPS, MULPS, DIVPS, SQRTPS, MINPS, MAXPS,
//! CMPPS, CVTDQ2PS, CVTTPS2DQ) with wasm f32x4 operations.
//!
//! Same contract as `jit_fp.rs`: the generated code is the interpreter's own fast path, applied to all four lanes at
//! once. If any lane (or MXCSR) is outside the cases where host IEEE-754 arithmetic is bit-exact and cannot miss a flag
//! (round-to-nearest-even, no FTZ/DAZ, PE sticky, normal operands and results; zeros where the result is
//! provably exact), the whole instruction runs in the interpreter instead. A 16-byte memory operand must be
//! 16-byte aligned (else the interpreter raises the fault).

use super::*;
use iced_x86::Register;

#[derive(Clone, Copy, PartialEq)]
enum P {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
}

fn xi(r: Register) -> u32 {
    (r as u32).wrapping_sub(Register::XMM0 as u32)
}
fn is_x(r: Register) -> bool {
    r.is_xmm() && xi(r) < 8
}

pub(super) const VA: u32 = L_V0;
pub(super) const VB: u32 = L_V0 + 1;
pub(super) const VR: u32 = L_V0 + 2;
pub(super) const VM: u32 = L_V0 + 3;

impl<'a> Fx<'a> {
    pub(super) fn v32(&mut self, v: u32) {
        let b = v.to_le_bytes();
        let mut a = [0u8; 16];
        for i in 0..4 {
            a[4 * i..4 * i + 4].copy_from_slice(&b);
        }
        self.c.v128_const(a);
    }

    /// stack: [] -> [mask]: lanes of vector local `v` that are normal numbers (with `zero_ok`: or +-0). Clobbers VM.
    fn v32_normal(&mut self, v: u32, zero_ok: bool) {
        self.c.lget(v).i32c(1).simd(0xab).lset(VM); // m = v << 1
        self.c.lget(VM);
        self.v32(1 << 24);
        self.c.simd(0xb1); // m - 2^24
        self.v32(0xfe << 24);
        self.c.simd(0x3a); // <u
        if zero_ok {
            self.c.lget(VM).v128_const([0; 16]).simd(0x37).simd(0x50); // | (m == 0)
        }
    }

    /// stack: [] -> [mask]: lanes of `v` that are neither NaN nor denormal. Clobbers VM.
    fn v32_ordered(&mut self, v: u32) {
        self.c.lget(v).i32c(1).simd(0xab).lset(VM);
        // NaN: m >u 0xff000000 ; denormal: 0 < m < 2^24
        self.c.lget(VM);
        self.v32(0xff00_0000);
        self.c.simd(0x3c);
        self.c.lget(VM);
        self.v32(1);
        self.c.simd(0xb1);
        self.v32((1 << 24) - 1);
        self.c.simd(0x3a);
        self.c.simd(0x50).simd(0x4d); // not (nan | denormal)
    }

    /// stack: [] -> [mask]: lanes of `v` with a finite value of biased exponent >= 2 (`result_ok32`). Clobbers VM.
    fn v32_result_ok(&mut self, v: u32) {
        self.c.lget(v).i32c(1).simd(0xab).lset(VM);
        self.c.lget(VM);
        self.v32(2 << 24);
        self.c.simd(0xb1);
        self.v32(0xfd << 24);
        self.c.simd(0x3a);
    }

    /// stack: [mask] -> [] ; leave through `slow` unless all lanes of the mask are set.
    pub(super) fn v32_all_or_slow(&mut self, slow: u32) {
        self.c.simd(0xa3).op(I32_EQZ);
        self.br_if_l(slow);
    }

    /// Open the done/slow blocks, load VA = dst and VB = src (guarding a memory operand).
    pub(super) fn ps_begin(&mut self, ins: &Instruction, load_dst: bool) -> Option<(u32, u32, u32)> {
        let d = ins.op0_register();
        if ins.op0_kind() != OpKind::Register || !is_x(d) {
            return None;
        }
        let doff = self.xoff(d);
        let (src, addr) = match ins.op1_kind() {
            OpKind::Register => {
                let s = ins.op1_register();
                if !is_x(s) {
                    return None;
                }
                (Src::Reg(self.xoff(s)), None)
            }
            OpKind::Memory => {
                let a = self.ea_simd(ins)?;
                (Src::Mem(a), Some(a))
            }
            _ => return None,
        };
        let done = self.open_blk();
        let slow = self.open_blk();
        if let Some(a) = addr {
            self.fast_cond(a, 16, self.lay.rtab);
            self.c.lget(a).i32c(15).op(I32_AND).op(I32_EQZ).op(I32_AND).op(I32_EQZ);
            self.br_if_l(slow);
        }
        if load_dst {
            self.c.lget(L_CPU).simd_mem(0x00, doff).lset(VA);
        }
        self.push_src(src);
        self.c.lset(VB);
        Some((done, slow, doff))
    }

    pub(super) fn ps_end(&mut self, ins: &Instruction, done: u32) {
        self.br(done);
        self.close(); // slow
        self.fallback(ins);
        self.close(); // done
    }

    fn ps_arith(&mut self, ins: &Instruction, op: P) -> bool {
        let Some((done, slow, doff)) = self.ps_begin(ins, op != P::Sqrt) else { return false };
        self.mxcsr_or_slow(slow);
        let sqrt = op == P::Sqrt;
        if sqrt {
            // positive normal lanes only
            self.v32_normal(VB, false);
            self.c.lget(VB).i32c(31).simd(0xad).v128_const([0; 16]).simd(0x37).simd(0x4e); // and: sign bit clear
            self.v32_all_or_slow(slow);
            self.c.lget(VB).simd(0xe3).lset(VR);
        } else {
            self.v32_normal(VA, true);
            self.v32_normal(VB, op != P::Div);
            self.c.simd(0x4e);
            self.v32_all_or_slow(slow);
            self.c.lget(VA).lget(VB).simd(match op {
                P::Add => 0xe4,
                P::Sub => 0xe5,
                P::Mul => 0xe6,
                _ => 0xe7,
            });
            self.c.lset(VR);
            // result lanes: normal with exponent >= 2, or an exactly zero result that cannot be an underflow
            self.v32_result_ok(VR);
            self.c.lget(VR).i32c(1).simd(0xab).v128_const([0; 16]).simd(0x37); // result is +-0
            match op {
                P::Add | P::Sub => {}
                P::Mul => {
                    // zero only when an operand was zero
                    self.c.lget(VA).i32c(1).simd(0xab).v128_const([0; 16]).simd(0x37);
                    self.c.lget(VB).i32c(1).simd(0xab).v128_const([0; 16]).simd(0x37);
                    self.c.simd(0x50).simd(0x4e);
                }
                _ => {
                    self.c.lget(VA).i32c(1).simd(0xab).v128_const([0; 16]).simd(0x37);
                    self.c.simd(0x4e);
                }
            }
            self.c.simd(0x50);
            self.v32_all_or_slow(slow);
        }
        self.c.lget(L_CPU).lget(VR).simd_mem(0x0b, doff);
        self.ps_end(ins, done);
        true
    }

    fn ps_minmax(&mut self, ins: &Instruction, is_max: bool) -> bool {
        let Some((done, slow, doff)) = self.ps_begin(ins, true) else { return false };
        self.v32_ordered(VA);
        self.v32_ordered(VB);
        self.c.simd(0x4e);
        self.v32_all_or_slow(slow);
        // result = a where (min: a < b | max: a > b) else b
        self.c.lget(L_CPU).lget(VA).lget(VB).lget(VA).lget(VB).simd(if is_max { 0x44 } else { 0x43 }).simd(0x52);
        self.c.simd_mem(0x0b, doff);
        self.ps_end(ins, done);
        true
    }

    /// CMPPS xmm, xmm/m128, imm8 on lanes that are neither NaN nor denormal.
    fn ps_cmp(&mut self, ins: &Instruction) -> bool {
        let pred = ins.immediate8() & 7;
        let Some((done, slow, doff)) = self.ps_begin(ins, true) else { return false };
        self.v32_ordered(VA);
        self.v32_ordered(VB);
        self.c.simd(0x4e);
        self.v32_all_or_slow(slow);
        self.c.lget(L_CPU);
        match pred {
            0 => {
                self.c.lget(VA).lget(VB).simd(0x41);
            }
            1 => {
                self.c.lget(VA).lget(VB).simd(0x43);
            }
            2 => {
                self.c.lget(VA).lget(VB).simd(0x45);
            }
            3 => {
                self.c.v128_const([0; 16]);
            }
            4 => {
                self.c.lget(VA).lget(VB).simd(0x42);
            }
            5 => {
                self.c.lget(VA).lget(VB).simd(0x46);
            }
            6 => {
                self.c.lget(VA).lget(VB).simd(0x44);
            }
            _ => {
                self.c.v128_const([0xff; 16]);
            }
        }
        self.c.simd_mem(0x0b, doff);
        self.ps_end(ins, done);
        true
    }

    /// CVTDQ2PS (`to_float`) / CVTTPS2DQ / CVTPS2DQ (`round`: to nearest even, the only mode of the fast MXCSR).
    fn ps_cvt(&mut self, ins: &Instruction, to_float: bool, round: bool) -> bool {
        let Some((done, slow, doff)) = self.ps_begin(ins, false) else { return false };
        if to_float {
            // |v| <= 2^24 is exact, otherwise MXCSR must be in the fast configuration
            self.c.lget(VB);
            self.v32(1 << 24);
            self.c.simd(0xae);
            self.v32((1 << 25) + 1);
            self.c.simd(0x3a).simd(0xa3);
            self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_mxcsr).u32c(MX_MASK).op(I32_AND).u32c(MX_FAST).op(I32_EQ).op(I32_OR);
            self.c.op(I32_EQZ);
            self.br_if_l(slow);
            self.c.lget(L_CPU).lget(VB).simd(0xfa).simd_mem(0x0b, doff);
        } else {
            self.mxcsr_or_slow(slow);
            // in range (NaN compares false)
            self.c.lget(VB);
            self.c.v128_const(f32_splat(-2147483648.0)).simd(0x46);
            self.c.lget(VB).v128_const(f32_splat(2147483648.0)).simd(0x43);
            self.c.simd(0x4e);
            self.v32_all_or_slow(slow);
            self.c.lget(L_CPU).lget(VB);
            if round {
                self.c.simd(0x6a); // f32x4.nearest
            }
            self.c.simd(0xf8).simd_mem(0x0b, doff);
        }
        self.ps_end(ins, done);
        true
    }

    /// SHUFPS / SHUFPD xmm, xmm/m128, imm8: element shuffle across dst and src (bit moves, exact).
    fn ps_shuf(&mut self, ins: &Instruction, mn: M) -> bool {
        let imm = ins.immediate8() as usize;
        let mask: [u8; 16] = if mn == M::Shufps {
            lanes(|i| {
                let k = i / 4;
                let sel = (imm >> (2 * k)) & 3;
                ((if k < 2 { 0 } else { 16 }) + sel * 4 + i % 4) as u8
            })
        } else {
            lanes(|i| {
                let k = i / 8;
                let sel = (imm >> k) & 1;
                ((if k == 0 { 0 } else { 16 }) + sel * 8 + i % 8) as u8
            })
        };
        let Some((done, _slow, doff)) = self.ps_begin(ins, true) else { return false };
        self.c.lget(L_CPU).lget(VA).lget(VB).shuffle(mask).simd_mem(0x0b, doff);
        self.ps_end(ins, done);
        true
    }

    /// Entry point from `simd_ins`: true if the instruction was translated.
    pub(super) fn ps_ins(&mut self, ins: &Instruction, mn: M) -> bool {
        match mn {
            M::Addps => self.ps_arith(ins, P::Add),
            M::Subps => self.ps_arith(ins, P::Sub),
            M::Mulps => self.ps_arith(ins, P::Mul),
            M::Divps => self.ps_arith(ins, P::Div),
            M::Sqrtps => self.ps_arith(ins, P::Sqrt),
            M::Minps => self.ps_minmax(ins, false),
            M::Maxps => self.ps_minmax(ins, true),
            M::Cmpps => self.ps_cmp(ins),
            M::Cvtdq2ps => self.ps_cvt(ins, true, false),
            M::Cvttps2dq => self.ps_cvt(ins, false, false),
            M::Cvtps2dq => self.ps_cvt(ins, false, true),
            M::Shufps | M::Shufpd => self.ps_shuf(ins, mn),
            _ => false,
        }
    }
}

pub(super) fn f32_splat(x: f32) -> [u8; 16] {
    let b = x.to_le_bytes();
    let mut a = [0u8; 16];
    for i in 0..4 {
        a[4 * i..4 * i + 4].copy_from_slice(&b);
    }
    a
}

/// MXCSR bits of the fast configuration (see `jit_fp.rs`).
pub(super) const MX_MASK: u32 = 0x6000 | 0x8000 | 0x40 | 0x20;
pub(super) const MX_FAST: u32 = 0x20;
