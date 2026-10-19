//! Inline translation of the integer MMX instructions (the MMX forms of the SSE2 integer set, MOVD/MOVQ, PSHUFW,
//! PEXTRW/PINSRW, PMOVMSKB, MOVNTQ, EMMS) on top of the wasm-SIMD helpers of `jit_simd.rs`.
//!
//! MMX registers alias the x87 stack: `Cpu::fpu.st[i].mant` holds the 64 bits. The interpreter (`sse.rs`) does
//! the following around every MMX instruction, and so does the generated code:
//!  * `mm_enter` (before touching a register): if TOP != 0 or some register tag is "empty", set TOP = 0 and re-tag the
//!    empty registers. The fast path is only taken when that is a no-op (`mmx_ok`: TOP == 0, no empty tag);
//!    otherwise the instruction runs in the interpreter, which performs the transition. After the first MMX
//!    instruction of a straight-line run (taken either way) the condition holds until something else could touch the
//!    x87 state (`Fx::mmx_known`).
//!  * `mm_set` (writing a register): mant = value, sign/exponent = 0xffff, tag = "special" (2). The sign/exponent and tag
//!    stores are skipped for registers already known to be in that state (`Fx::mm_tagged`).
//!  * EMMS: tag word = 0xffff, nothing else.
//! Lane-wise operations are computed on the full 128-bit vector (the register's 8 bytes plus 8 bytes of whatever follows
//! it in `Cpu`) and only the low 64 bits are stored: they are independent of the upper half. Pack and the
//! high unpacks have distinct 64-bit definitions (see `Cpu::ibin2`) and get their own shuffles.

use super::*;
use iced_x86::{Code as IC, Register};

fn mi_x(r: Register) -> u32 {
    (r as u32).wrapping_sub(Register::XMM0 as u32)
}

fn mi(r: Register) -> u32 {
    (r as u32).wrapping_sub(Register::MM0 as u32)
}

/// Shuffle mask of the MMX high unpacks of `w`-byte units: interleave units 4/w.. of dst and src.
fn unpack_high64(w: usize) -> [u8; 16] {
    lanes(|i| {
        if i >= 8 {
            return i as u8;
        }
        let unit = i / w;
        let pair = unit / 2;
        let from_src = unit % 2 == 1;
        let byte = 4 + pair * w + i % w;
        (byte + if from_src { 16 } else { 0 }) as u8
    })
}

impl<'a> Fx<'a> {
    fn mm_off(&self, i: u32) -> u32 {
        self.lay.cpu_mm0 + self.lay.mm_stride * i
    }

    /// Push 1 when `Cpu::mm_enter` would not change anything: TOP == 0 and no register tagged empty.
    fn mmx_ok(&mut self) {
        let t = self.tmp();
        self.c.lget(L_CPU).mem(I32_LOAD16_U, self.lay.cpu_fpu_tag).ltee(t).lget(t).i32c(1).op(I32_SHR_U).op(I32_AND).i32c(0x5555).op(I32_AND);
        self.c.lget(L_CPU).mem(I32_LOAD8_U, self.lay.cpu_fpu_top).op(I32_OR).op(I32_EQZ);
    }

    /// Mark MM register `i` as written: sign/exponent 0xffff and a 'special' tag (skipped when already known).
    fn mm_tag(&mut self, i: u32) {
        if self.mm_tagged & (1 << i) != 0 {
            return;
        }
        let se = self.lay.cpu_mm_se0 + self.lay.mm_stride * i;
        self.c.lget(L_CPU).i32c(0xffff).mem(I32_STORE16, se);
        let tag = self.lay.cpu_fpu_tag;
        self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD16_U, tag).i32c(!(3i32 << (2 * i)) & 0xffff).op(I32_AND).i32c(2 << (2 * i)).op(I32_OR).mem(I32_STORE16, tag);
        self.mm_tagged |= 1 << i;
    }

    /// `if (MMX state ok && memory access ok) { fast } else { interpreter }`. `mem` = (address local, size, write).
    fn mmx_guard(&mut self, ins: &Instruction, mem: Option<(u32, u8, bool)>, fast: impl FnOnce(&mut Self)) {
        let mut have = false;
        if !self.mmx_known {
            self.mmx_ok();
            have = true;
        }
        if let Some((a, size, write)) = mem {
            let tab = if write { self.lay.wtab } else { self.lay.rtab };
            self.fast_cond(a, size, tab);
            if have {
                self.c.op(I32_AND);
            }
            have = true;
        }
        if !have {
            fast(self);
            return;
        }
        self.if_(BT_EMPTY);
        fast(self);
        self.else_();
        // Avoid full GPR/lazy-flag synchronization for the common read-only MMX transitions.
        let mn = ins.mnemonic();
        if matches!(mn, M::Movq | M::Pxor) && ins.op0_register().is_mm()
            && (ins.op1_register().is_mm() || matches!(mem, Some((_, 8, false)))) {
            let opdst = ((mi(ins.op0_register())) << 8) | u32::from(mn == M::Pxor);
            self.c.lget(L_CPU).u32c(opdst);
            if let Some((a, _, _)) = mem {
                self.c.i32c(2).lget(a);
            } else {
                self.c.i32c(0).u32c(mi(ins.op1_register()));
            }
            self.c.u32c(ins.ip() as u32).call(F_MM_OP);
            self.if_(BT_EMPTY);
            self.br(LB_FAULT);
            self.close();
        } else {
            self.fallback(ins);
        }
        self.close();
        self.mmx_known = true;
    }

    /// stack: [] -> [v128] for the MMX form of `kind`/`mn` applied to (dst register, src).
    fn mmx_bin_value(&mut self, mn: M, kind: Bin, dst: u32, src: Src) {
        match mn {
            M::Packsswb | M::Packssdw | M::Packuswb => {
                // dst.lo64 | src.lo64 << 64, packed against itself: the low 8 bytes are the two 4-lane halves
                self.c.lget(L_CPU).simd_mem(0x00, dst);
                self.push_src(src);
                self.c.shuffle(lanes(|i| if i < 8 { i as u8 } else { (16 + i - 8) as u8 }));
                self.c.lset(L_V0);
                self.c.lget(L_V0).lget(L_V0).simd(match mn {
                    M::Packsswb => 0x65,
                    M::Packssdw => 0x85,
                    _ => 0x66,
                });
            }
            M::Punpckhbw | M::Punpckhwd | M::Punpckhdq => {
                let w = match mn {
                    M::Punpckhbw => 1,
                    M::Punpckhwd => 2,
                    _ => 4,
                };
                self.c.lget(L_CPU).simd_mem(0x00, dst);
                self.push_src(src);
                self.c.shuffle(unpack_high64(w));
            }
            _ => self.bin_value(kind, dst, src),
        }
    }

    fn mmx_bin(&mut self, ins: &Instruction, mn: M, kind: Bin) -> bool {
        let d = ins.op0_register();
        let dm = mi(d);
        let doff = self.mm_off(dm);
        let (src, mem) = match ins.op1_kind() {
            OpKind::Register => {
                let s = ins.op1_register();
                if !s.is_mm() {
                    return false;
                }
                (Src::Reg(self.mm_off(mi(s))), None)
            }
            OpKind::Memory => {
                let size = ins.memory_size().size();
                if size != 8 && size != 4 {
                    return false;
                }
                let Some(a) = self.ea_simd(ins) else { return false };
                (if size == 8 { Src::Mem64(a) } else { Src::Mem32(a) }, Some((a, size as u8, false)))
            }
            _ => return false,
        };
        self.mmx_guard(ins, mem, |s| {
            s.c.lget(L_CPU);
            s.mmx_bin_value(mn, kind, doff, src);
            s.c.simd_mem_lane(0x5b, doff, 0);
            s.mm_tag(dm);
        });
        true
    }

    fn mmx_shift(&mut self, ins: &Instruction, mn: M) -> bool {
        let (opc, width, arith): (u32, u32, bool) = match mn {
            M::Psllw => (0x8b, 16, false),
            M::Pslld => (0xab, 32, false),
            M::Psllq => (0xcb, 64, false),
            M::Psrlw => (0x8d, 16, false),
            M::Psrld => (0xad, 32, false),
            M::Psrlq => (0xcd, 64, false),
            M::Psraw => (0x8c, 16, true),
            _ => (0xac, 32, true),
        };
        let d = ins.op0_register();
        let dm = mi(d);
        let doff = self.mm_off(dm);
        if ins.op1_kind() == OpKind::Immediate8 {
            let c = ins.immediate8() as u32;
            self.mmx_guard(ins, None, |s| {
                s.c.lget(L_CPU);
                if !arith && c >= width {
                    s.c.v128_const([0; 16]);
                } else {
                    s.c.lget(L_CPU).simd_mem(0x00, doff);
                    s.c.i32c(if arith { c.min(width - 1) } else { c } as i32).simd(opc);
                }
                s.c.simd_mem_lane(0x5b, doff, 0);
                s.mm_tag(dm);
            });
            return true;
        }
        // count = the 64-bit value of an MM register or m64
        let t0 = Self::t64(0);
        let (cnt_src, mem): (Result<u32, u32>, Option<(u32, u8, bool)>) = match ins.op1_kind() {
            OpKind::Register => {
                let s = ins.op1_register();
                if !s.is_mm() {
                    return false;
                }
                (Ok(self.mm_off(mi(s))), None)
            }
            OpKind::Memory => {
                if ins.memory_size().size() != 8 {
                    return false;
                }
                let Some(a) = self.ea_simd(ins) else { return false };
                (Err(a), Some((a, 8, false)))
            }
            _ => return false,
        };
        self.mmx_guard(ins, mem, |s| {
            match cnt_src {
                Ok(off) => {
                    s.c.lget(L_CPU).mem(I64_LOAD, off).lset(t0);
                }
                Err(a) => {
                    s.c.lget(a).mem(I64_LOAD, s.lay.guest_base).lset(t0);
                }
            }
            s.c.lget(L_CPU).simd_mem(0x00, doff).lset(L_V0);
            s.c.lget(L_CPU);
            if !arith {
                s.c.lget(t0).i64c(width as i64).op(I64_LT_U);
                s.if_(V128);
                s.c.lget(L_V0).lget(t0).op(I32_WRAP_I64).simd(opc);
                s.else_();
                s.c.v128_const([0; 16]);
                s.close();
            } else {
                s.c.lget(L_V0);
                s.c.lget(t0).i64c(width as i64 - 1).lget(t0).i64c(width as i64 - 1).op(I64_LT_U).select();
                s.c.op(I32_WRAP_I64).simd(opc);
            }
            s.c.simd_mem_lane(0x5b, doff, 0);
            s.mm_tag(dm);
        });
        true
    }

    fn mmx_movq(&mut self, ins: &Instruction) -> bool {
        let gb = self.lay.guest_base;
        if ins.op0_kind() == OpKind::Register && ins.op0_register().is_mm() {
            let dm = mi(ins.op0_register());
            let doff = self.mm_off(dm);
            match ins.op1_kind() {
                OpKind::Register => {
                    let s = ins.op1_register();
                    if !s.is_mm() {
                        return false;
                    }
                    let soff = self.mm_off(mi(s));
                    self.mmx_guard(ins, None, |x| {
                        x.c.lget(L_CPU).lget(L_CPU).mem(I64_LOAD, soff).mem(I64_STORE, doff);
                        x.mm_tag(dm);
                    });
                }
                OpKind::Memory => {
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.mmx_guard(ins, Some((a, 8, false)), |x| {
                        x.c.lget(L_CPU).lget(a).mem(I64_LOAD, gb).mem(I64_STORE, doff);
                        x.mm_tag(dm);
                    });
                }
                _ => return false,
            }
            return true;
        }
        // movq m64, mm
        if ins.op0_kind() == OpKind::Memory && ins.op1_kind() == OpKind::Register && ins.op1_register().is_mm() {
            let soff = self.mm_off(mi(ins.op1_register()));
            let Some(a) = self.ea_simd(ins) else { return false };
            self.mmx_guard(ins, Some((a, 8, true)), |x| {
                x.c.lget(a).lget(L_CPU).mem(I64_LOAD, soff).mem(I64_STORE, gb);
            });
            return true;
        }
        false
    }

    fn mmx_movd(&mut self, ins: &Instruction) -> bool {
        let gb = self.lay.guest_base;
        if ins.op0_kind() == OpKind::Register && ins.op0_register().is_mm() {
            // movd mm, r/m32
            let dm = mi(ins.op0_register());
            let doff = self.mm_off(dm);
            match ins.op1_kind() {
                OpKind::Register => {
                    let r = ins.op1_register();
                    if !r.is_gpr32() {
                        return false;
                    }
                    self.mmx_guard(ins, None, |x| {
                        x.c.lget(L_CPU).lget(Self::r32(r.number())).op(I64_EXTEND_I32_U).mem(I64_STORE, doff);
                        x.mm_tag(dm);
                    });
                }
                OpKind::Memory => {
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.mmx_guard(ins, Some((a, 4, false)), |x| {
                        x.c.lget(L_CPU).lget(a).mem(I32_LOAD, gb).op(I64_EXTEND_I32_U).mem(I64_STORE, doff);
                        x.mm_tag(dm);
                    });
                }
                _ => return false,
            }
            return true;
        }
        if ins.op1_kind() == OpKind::Register && ins.op1_register().is_mm() {
            let soff = self.mm_off(mi(ins.op1_register()));
            match ins.op0_kind() {
                OpKind::Register => {
                    let r = ins.op0_register();
                    if !r.is_gpr32() {
                        return false;
                    }
                    self.mmx_guard(ins, None, |x| {
                        x.c.lget(L_CPU).mem(I32_LOAD, soff);
                        x.rset32(r.number() as usize);
                    });
                }
                OpKind::Memory => {
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.mmx_guard(ins, Some((a, 4, true)), |x| {
                        x.c.lget(a).lget(L_CPU).mem(I32_LOAD, soff).mem(I32_STORE, gb);
                    });
                }
                _ => return false,
            }
            return true;
        }
        false
    }

    /// CVT[T]PS2PI mm, xmm/m64: two floats to two int32 (nearest even, or truncating) in the fast MXCSR configuration.
    fn mmx_cvtps2pi(&mut self, ins: &Instruction, trunc: bool) -> bool {
        let dm = mi(ins.op0_register());
        let doff = self.mm_off(dm);
        let (src, mem) = match ins.op1_kind() {
            OpKind::Register => {
                let s = ins.op1_register();
                if !s.is_xmm() || mi_x(s) >= 8 {
                    return false;
                }
                (Src::Reg(self.xoff(s)), None)
            }
            OpKind::Memory => {
                if ins.memory_size().size() != 8 {
                    return false;
                }
                let Some(a) = self.ea_simd(ins) else { return false };
                (Src::Mem64(a), Some((a, 8u8, false)))
            }
            _ => return false,
        };
        self.mmx_guard(ins, mem, |x| {
            // the low two lanes only: load 8 bytes, zero the rest
            match src {
                Src::Reg(off) => {
                    x.c.lget(L_CPU).simd_mem(0x5d, off);
                }
                _ => {
                    x.push_src(src);
                }
            }
            x.c.lset(ps::VB);
            // MXCSR fast configuration (for the inexact flag) and both lanes in range (a NaN compares false)
            let slow_cond = x.tmp();
            x.c.lget(L_CPU).mem(I32_LOAD, x.lay.cpu_mxcsr).u32c(ps::MX_MASK).op(I32_AND).u32c(ps::MX_FAST).op(I32_EQ).lset(slow_cond);
            x.c.lget(ps::VB).v128_const(ps::f32_splat(-2147483648.0)).simd(0x46);
            x.c.lget(ps::VB).v128_const(ps::f32_splat(2147483648.0)).simd(0x43);
            x.c.simd(0x4e).simd(0xa3).lget(slow_cond).op(I32_AND);
            x.if_(BT_EMPTY);
            x.c.lget(L_CPU).lget(ps::VB);
            if !trunc {
                x.c.simd(0x6a);
            }
            x.c.simd(0xf8).simd_mem_lane(0x5b, doff, 0);
            x.mm_tag(dm);
            x.else_();
            x.fallback(ins);
            x.close();
        });
        true
    }

    /// Entry point (from `simd_ins`): true if the instruction was an MMX instruction that has been translated.
    pub(super) fn mmx_ins(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let mm_dst = ins.op_count() > 0 && ins.op0_kind() == OpKind::Register && ins.op0_register().is_mm();
        let mm_src = ins.op_count() > 1 && ins.op1_kind() == OpKind::Register && ins.op1_register().is_mm();
        match mn {
            M::Movdq2q if mm_dst => {
                // mm = low 64 bits of xmm
                let s = ins.op1_register();
                if !s.is_xmm() || mi_x(s) >= 8 {
                    return false;
                }
                let dm = mi(ins.op0_register());
                let doff = self.mm_off(dm);
                let soff = self.xoff(s);
                self.mmx_guard(ins, None, |x| {
                    x.c.lget(L_CPU).lget(L_CPU).mem(I64_LOAD, soff).mem(I64_STORE, doff);
                    x.mm_tag(dm);
                });
                true
            }
            M::Movq2dq if mm_src => {
                // xmm = mm zero-extended
                let d = ins.op0_register();
                if !d.is_xmm() || mi_x(d) >= 8 {
                    return false;
                }
                let doff = self.xoff(d);
                let soff = self.mm_off(mi(ins.op1_register()));
                self.mmx_guard(ins, None, |x| {
                    x.c.lget(L_CPU).lget(L_CPU).simd_mem(0x5d, soff).simd_mem(0x0b, doff);
                });
                true
            }
            M::Cvtps2pi | M::Cvttps2pi if mm_dst => self.mmx_cvtps2pi(ins, mn == M::Cvttps2pi),
            M::Emms => {
                self.c.lget(L_CPU).i32c(0xffff).mem(I32_STORE16, self.lay.cpu_fpu_tag);
                self.mmx_known = false;
                self.mm_tagged = 0;
                true
            }
            M::Movq if mm_dst || mm_src => self.mmx_movq(ins),
            M::Movntq if mm_src => self.mmx_movq(ins),
            M::Movd if mm_dst || mm_src => self.mmx_movd(ins),
            M::Pextrw if mm_src && matches!(ins.code(), IC::Pextrw_r32_mm_imm8) => {
                let r = ins.op0_register();
                if !r.is_gpr32() {
                    return false;
                }
                let soff = self.mm_off(mi(ins.op1_register()));
                let sh = 16 * (ins.immediate8() as i64 & 3);
                self.mmx_guard(ins, None, |x| {
                    x.c.lget(L_CPU).mem(I64_LOAD, soff).i64c(sh).op(I64_SHR_U).op(I32_WRAP_I64).i32c(0xffff).op(I32_AND);
                    x.rset32(r.number() as usize);
                });
                true
            }
            M::Pinsrw if mm_dst => {
                let dm = mi(ins.op0_register());
                let doff = self.mm_off(dm);
                let sh = 16 * (ins.immediate8() as i64 & 3);
                // the word is read first (a fault leaves the register untouched)
                let w = if ins.op1_kind() == OpKind::Register {
                    let r = ins.op1_register();
                    if !(r.is_gpr32() || r.is_gpr16()) {
                        return false;
                    }
                    self.rget(r);
                    self.to_tmp()
                } else {
                    if ins.memory_size().size() != 2 {
                        return false;
                    }
                    let Some(a) = self.ea(ins, true) else { return false };
                    self.load_mem(a, 2, ip);
                    self.to_tmp()
                };
                self.mmx_guard(ins, None, |x| {
                    x.c.lget(L_CPU).lget(L_CPU).mem(I64_LOAD, doff).i64c(!(0xffffi64 << sh)).op(I64_AND);
                    x.push_v(w);
                    x.c.i32c(0xffff).op(I32_AND).op(I64_EXTEND_I32_U).i64c(sh).op(I64_SHL).op(I64_OR).mem(I64_STORE, doff);
                    x.mm_tag(dm);
                });
                true
            }
            M::Pmovmskb if mm_src => {
                let r = ins.op0_register();
                if !r.is_gpr32() {
                    return false;
                }
                let soff = self.mm_off(mi(ins.op1_register()));
                self.mmx_guard(ins, None, |x| {
                    x.c.lget(L_CPU).simd_mem(0x00, soff).simd(0x64).i32c(0xff).op(I32_AND);
                    x.rset32(r.number() as usize);
                });
                true
            }
            M::Pshufw if mm_dst => {
                let dm = mi(ins.op0_register());
                let doff = self.mm_off(dm);
                let imm = ins.immediate8() as usize;
                let mask = lanes(|i| {
                    if i < 8 {
                        let sel = (imm >> (2 * (i / 2))) & 3;
                        (sel * 2 + i % 2) as u8
                    } else {
                        i as u8
                    }
                });
                let (src, mem) = match ins.op1_kind() {
                    OpKind::Register => {
                        let s = ins.op1_register();
                        if !s.is_mm() {
                            return false;
                        }
                        (Src::Reg(self.mm_off(mi(s))), None)
                    }
                    OpKind::Memory => {
                        if ins.memory_size().size() != 8 {
                            return false;
                        }
                        let Some(a) = self.ea_simd(ins) else { return false };
                        (Src::Mem64(a), Some((a, 8u8, false)))
                    }
                    _ => return false,
                };
                self.mmx_guard(ins, mem, |x| {
                    x.c.lget(L_CPU);
                    x.push_src(src);
                    x.c.lset(L_V0);
                    x.c.lget(L_V0).lget(L_V0).shuffle(mask);
                    x.c.simd_mem_lane(0x5b, doff, 0);
                    x.mm_tag(dm);
                });
                true
            }
            M::Psllw | M::Pslld | M::Psllq | M::Psrlw | M::Psrld | M::Psrlq | M::Psraw | M::Psrad if mm_dst => self.mmx_shift(ins, mn),
            _ if mm_dst => {
                let Some(kind) = bin_kind(mn) else { return false };
                self.mmx_bin(ins, mn, kind)
            }
            _ => false,
        }
    }
}
