//! Inline translation of the SSE/SSE2 *integer* and move instructions with WebAssembly SIMD.
//!
//! Scope: everything whose result is exactly defined independently of the host (integer lanes, bitwise ops,
//! shuffles, moves, movemask). Floating-point arithmetic is deliberately not translated: NaN payloads and
//! the sticky MXCSR flags are not reproducible with wasm's f32x4/f64x2 operations on every host, so those
//! keep running in the interpreter (through the normal fallback). MMX forms (which alias the x87 stack) also
//! stay in the interpreter.
//!
//! XMM registers are not cached: they stay in `Cpu::xmm` (linear memory) and every instruction loads and
//! stores them with `v128.load`/`v128.store` relative to the cpu pointer, so interpreter calls and
//! faults never have to synchronise them.
//!
//! Memory operands are guarded like integer accesses: when the page tables allow, the access is done inline,
//! otherwise (misaligned operand that must fault, page boundary, unmapped/protected/code page) the whole
//! instruction runs in the interpreter via the ordinary fallback, which raises exactly the interpreter's
//! fault. Both branches leave the architectural state identical.

use super::*;
use iced_x86::{Code as IC, Register};

/// How `dst = f(dst, src)` is computed.
#[derive(Clone, Copy)]
enum Bin {
    /// one wasm SIMD instruction `op(dst, src)`
    Op(u32),
    /// one wasm SIMD instruction `op(src, dst)` (PANDN)
    Swap(u32),
    /// `i8x16.shuffle(dst, src)` with these lanes (indices 0..15 = dst, 16..31 = src)
    Shuf([u8; 16]),
    /// PMULHW / PMULHUW
    MulHi(bool),
    Muludq,
    Sadbw,
}

fn lanes(f: impl Fn(usize) -> u8) -> [u8; 16] {
    let mut l = [0u8; 16];
    for (i, v) in l.iter_mut().enumerate() {
        *v = f(i);
    }
    l
}

/// Interleave: take `w`-byte units alternately from a and b, starting at unit `first` of each.
fn unpack(w: usize, high: bool) -> [u8; 16] {
    let base = if high { 8 } else { 0 };
    lanes(|i| {
        let unit = i / w; // 0.. output unit
        let src_unit = unit / 2; // unit index within the 8-byte half
        let from_b = unit % 2 == 1;
        let byte = base + src_unit * w + i % w;
        (byte + if from_b { 16 } else { 0 }) as u8
    })
}

fn bin_kind(mn: M) -> Option<Bin> {
    Some(match mn {
        M::Pand | M::Andps | M::Andpd => Bin::Op(0x4e),
        M::Pandn | M::Andnps | M::Andnpd => Bin::Swap(0x4f),
        M::Por | M::Orps | M::Orpd => Bin::Op(0x50),
        M::Pxor | M::Xorps | M::Xorpd => Bin::Op(0x51),
        M::Paddb => Bin::Op(0x6e),
        M::Paddw => Bin::Op(0x8e),
        M::Paddd => Bin::Op(0xae),
        M::Paddq => Bin::Op(0xce),
        M::Psubb => Bin::Op(0x71),
        M::Psubw => Bin::Op(0x91),
        M::Psubd => Bin::Op(0xb1),
        M::Psubq => Bin::Op(0xd1),
        M::Paddsb => Bin::Op(0x6f),
        M::Paddsw => Bin::Op(0x8f),
        M::Paddusb => Bin::Op(0x70),
        M::Paddusw => Bin::Op(0x90),
        M::Psubsb => Bin::Op(0x72),
        M::Psubsw => Bin::Op(0x92),
        M::Psubusb => Bin::Op(0x73),
        M::Psubusw => Bin::Op(0x93),
        M::Pmullw => Bin::Op(0x95),
        M::Pmaddwd => Bin::Op(0xba),
        M::Pavgb => Bin::Op(0x7b),
        M::Pavgw => Bin::Op(0x9b),
        M::Pminub => Bin::Op(0x77),
        M::Pmaxub => Bin::Op(0x79),
        M::Pminsw => Bin::Op(0x96),
        M::Pmaxsw => Bin::Op(0x98),
        M::Pcmpeqb => Bin::Op(0x23),
        M::Pcmpeqw => Bin::Op(0x2d),
        M::Pcmpeqd => Bin::Op(0x37),
        M::Pcmpgtb => Bin::Op(0x27),
        M::Pcmpgtw => Bin::Op(0x31),
        M::Pcmpgtd => Bin::Op(0x3b),
        M::Packsswb => Bin::Op(0x65),
        M::Packuswb => Bin::Op(0x66),
        M::Packssdw => Bin::Op(0x85),
        M::Punpcklbw => Bin::Shuf(unpack(1, false)),
        M::Punpckhbw => Bin::Shuf(unpack(1, true)),
        M::Punpcklwd => Bin::Shuf(unpack(2, false)),
        M::Punpckhwd => Bin::Shuf(unpack(2, true)),
        M::Punpckldq | M::Unpcklps => Bin::Shuf(unpack(4, false)),
        M::Punpckhdq | M::Unpckhps => Bin::Shuf(unpack(4, true)),
        M::Punpcklqdq | M::Unpcklpd => Bin::Shuf(unpack(8, false)),
        M::Punpckhqdq | M::Unpckhpd => Bin::Shuf(unpack(8, true)),
        M::Pmulhw => Bin::MulHi(true),
        M::Pmulhuw => Bin::MulHi(false),
        M::Pmuludq => Bin::Muludq,
        M::Psadbw => Bin::Sadbw,
        _ => return None,
    })
}

fn xi(r: Register) -> u32 {
    (r as u32).wrapping_sub(Register::XMM0 as u32)
}
fn is_x(r: Register) -> bool {
    r.is_xmm() && xi(r) < 8
}

/// Source of the second operand.
#[derive(Clone, Copy)]
enum Src {
    Reg(u32),
    /// guest address in this local (16-byte operand)
    Mem(u32),
    /// 8 / 4-byte memory operand of an MMX instruction (zero-extended into the vector)
    Mem64(u32),
    Mem32(u32),
}

impl<'a> Fx<'a> {
    fn xoff(&self, r: Register) -> u32 {
        self.lay.cpu_xmm + 16 * xi(r)
    }

    /// push the v128 value of a source operand
    fn push_src(&mut self, s: Src) {
        match s {
            Src::Reg(off) => {
                self.c.lget(L_CPU).simd_mem(0x00, off);
            }
            Src::Mem(a) => {
                self.c.lget(a).simd_mem(0x00, self.lay.guest_base);
            }
            Src::Mem64(a) => {
                self.c.lget(a).simd_mem(0x5d, self.lay.guest_base);
            }
            Src::Mem32(a) => {
                self.c.lget(a).simd_mem(0x5c, self.lay.guest_base);
            }
        }
    }

    /// `if (access ok inline) { body } else { interpreter }` for a `size`-byte access at local `addr`.
    fn simd_guard(&mut self, ins: &Instruction, addr: u32, size: u8, align16: bool, write: bool, body: impl FnOnce(&mut Self)) {
        let tab = if write { self.lay.wtab } else { self.lay.rtab };
        self.fast_cond(addr, size, tab);
        if align16 {
            self.c.lget(addr).i32c(15).op(I32_AND).op(I32_EQZ).op(I32_AND);
        }
        self.if_(BT_EMPTY);
        body(self);
        self.else_();
        self.fallback(ins);
        self.close();
    }

    /// stack: [] -> [v128] for `bin(dst, src)`
    fn bin_value(&mut self, kind: Bin, dst: u32, src: Src) {
        match kind {
            Bin::Op(o) => {
                self.c.lget(L_CPU).simd_mem(0x00, dst);
                self.push_src(src);
                self.c.simd(o);
            }
            Bin::Swap(o) => {
                self.push_src(src);
                self.c.lget(L_CPU).simd_mem(0x00, dst);
                self.c.simd(o);
            }
            Bin::Shuf(l) => {
                self.c.lget(L_CPU).simd_mem(0x00, dst);
                self.push_src(src);
                self.c.shuffle(l);
            }
            Bin::MulHi(signed) => {
                // high halves of the 16x16->32 products: extmul low/high, then keep bytes 2,3 of every dword
                self.c.lget(L_CPU).simd_mem(0x00, dst).lset(L_V0);
                self.push_src(src);
                self.c.lset(L_V0 + 1);
                let (lo, hi) = if signed { (0xbc, 0xbd) } else { (0xbe, 0xbf) };
                self.c.lget(L_V0).lget(L_V0 + 1).simd(lo);
                self.c.lget(L_V0).lget(L_V0 + 1).simd(hi);
                self.c.shuffle([2, 3, 6, 7, 10, 11, 14, 15, 18, 19, 22, 23, 26, 27, 30, 31]);
            }
            Bin::Muludq => {
                // even dwords of each qword: gather them, then unsigned 32x32->64 multiply
                let gather = [0, 1, 2, 3, 8, 9, 10, 11, 0, 1, 2, 3, 8, 9, 10, 11];
                self.c.lget(L_CPU).simd_mem(0x00, dst).lset(L_V0);
                self.c.lget(L_V0).lget(L_V0).shuffle(gather);
                self.push_src(src);
                self.c.lset(L_V0 + 1);
                self.c.lget(L_V0 + 1).lget(L_V0 + 1).shuffle(gather);
                self.c.simd(0xde); // i64x2.extmul_low_i32x4_u
            }
            Bin::Sadbw => {
                // |a-b| per byte, then sum each group of 8 bytes into the low 16 bits of its qword
                self.c.lget(L_CPU).simd_mem(0x00, dst).lset(L_V0);
                self.push_src(src);
                self.c.lset(L_V0 + 1);
                self.c.lget(L_V0).lget(L_V0 + 1).simd(0x79); // max_u
                self.c.lget(L_V0).lget(L_V0 + 1).simd(0x77); // min_u
                self.c.simd(0x71); // i8x16.sub -> absolute differences
                self.c.simd(0x7d); // i16x8.extadd_pairwise_i8x16_u
                self.c.simd(0x7f); // i32x4.extadd_pairwise_i16x8_u -> four dword sums
                self.c.lset(L_V0);
                // d0 += d1 ; d2 += d3 ; clear d1, d3
                self.c.lget(L_V0);
                self.c.lget(L_V0).v128_const([0; 16]).shuffle([4, 5, 6, 7, 16, 17, 18, 19, 12, 13, 14, 15, 16, 17, 18, 19]);
                self.c.simd(0xae); // i32x4.add
                self.c.v128_const([0; 16]).shuffle([0, 1, 2, 3, 16, 17, 18, 19, 8, 9, 10, 11, 16, 17, 18, 19]);
            }
        }
    }

    /// Translate one SSE/SSE2 integer or move instruction. Returns false (nothing emitted) if unsupported.
    pub(super) fn simd_ins(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        if self.mmx_ins(ins, ip, mn) || self.ps_ins(ins, mn) {
            return true;
        }
        match mn {
            M::Cvtss2si => {
                let d = ins.op0_register();
                if !d.is_gpr32() { return false; }
                let dst = d as u32 - Register::EAX as u32;
                let a = match ins.op1_kind() {
                    OpKind::Register if is_x(ins.op1_register()) => None,
                    OpKind::Memory => {
                        let Some(a) = self.ea_simd(ins) else { return false };
                        Some(a)
                    }
                    _ => return false,
                };
                self.c.lget(L_CPU).i32c(a.is_some() as i32);
                if let Some(a) = a {
                    self.c.lget(a);
                } else {
                    self.c.u32c(ins.op1_register() as u32 - Register::XMM0 as u32);
                }
                self.c.u32c(dst).u32c(ip).call(F_CVTSS2SI);
                self.if_(BT_EMPTY);
                self.br(LB_FAULT);
                self.close();
                self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_r + 4 * dst).lset(L_R0 + dst);
                self.written |= 1 << dst;
                true
            }
            // ---- full-register moves ----
            M::Movdqa | M::Movaps | M::Movapd | M::Movntdq | M::Movntps | M::Movntpd | M::Movdqu | M::Movups | M::Movupd => {
                let aligned = matches!(mn, M::Movdqa | M::Movaps | M::Movapd | M::Movntdq | M::Movntps | M::Movntpd);
                if ins.op0_kind() == OpKind::Register {
                    let d = ins.op0_register();
                    if !is_x(d) {
                        return false;
                    }
                    let doff = self.xoff(d);
                    if ins.op1_kind() == OpKind::Register {
                        let s = ins.op1_register();
                        if !is_x(s) {
                            return false;
                        }
                        let soff = self.xoff(s);
                        self.c.lget(L_CPU).lget(L_CPU).simd_mem(0x00, soff).simd_mem(0x0b, doff);
                    } else {
                        let Some(a) = self.ea_simd(ins) else { return false };
                        self.simd_guard(ins, a, 16, aligned, false, |s| {
                            s.c.lget(L_CPU).lget(a).simd_mem(0x00, s.lay.guest_base).simd_mem(0x0b, doff);
                        });
                    }
                } else {
                    let s = ins.op1_register();
                    if !is_x(s) {
                        return false;
                    }
                    let soff = self.xoff(s);
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.simd_guard(ins, a, 16, aligned, true, |s| {
                        s.c.lget(a).lget(L_CPU).simd_mem(0x00, soff).simd_mem(0x0b, s.lay.guest_base);
                    });
                }
                true
            }
            // ---- MOVD / MOVQ ----
            M::Movd => self.simd_movd(ins, ip),
            M::Movq => self.simd_movq(ins),
            M::Movss | M::Movsd if ins.op0_kind() == OpKind::Register && ins.op0_register().is_xmm() || ins.op1_kind() == OpKind::Register && ins.op1_register().is_xmm() => self.simd_movs(ins, mn == M::Movss),
            M::Movlps | M::Movlpd | M::Movhps | M::Movhpd => {
                let lane = matches!(mn, M::Movhps | M::Movhpd) as u8;
                if ins.op0_kind() == OpKind::Register {
                    let d = ins.op0_register();
                    if !is_x(d) || ins.op1_kind() != OpKind::Memory {
                        return false;
                    }
                    let doff = self.xoff(d);
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.simd_guard(ins, a, 8, false, false, |s| {
                        s.c.lget(L_CPU).lget(a).lget(L_CPU).simd_mem(0x00, doff).simd_mem_lane(0x57, s.lay.guest_base, lane).simd_mem(0x0b, doff);
                    });
                } else {
                    let s = ins.op1_register();
                    if !is_x(s) || ins.op0_kind() != OpKind::Memory {
                        return false;
                    }
                    let soff = self.xoff(s);
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.simd_guard(ins, a, 8, false, true, |s| {
                        s.c.lget(a).lget(L_CPU).simd_mem(0x00, soff).simd_mem_lane(0x5b, s.lay.guest_base, lane);
                    });
                }
                true
            }
            M::Movhlps | M::Movlhps => {
                let (d, s) = (ins.op0_register(), ins.op1_register());
                if !is_x(d) || !is_x(s) {
                    return false;
                }
                let (doff, soff) = (self.xoff(d), self.xoff(s));
                self.c.lget(L_CPU).lget(L_CPU).simd_mem(0x00, doff).lget(L_CPU).simd_mem(0x00, soff);
                if mn == M::Movhlps {
                    self.c.shuffle([24, 25, 26, 27, 28, 29, 30, 31, 8, 9, 10, 11, 12, 13, 14, 15]); // dst.lo = src.hi
                } else {
                    self.c.shuffle([0, 1, 2, 3, 4, 5, 6, 7, 16, 17, 18, 19, 20, 21, 22, 23]); // dst.hi = src.lo
                }
                self.c.simd_mem(0x0b, doff);
                true
            }
            M::Pmovmskb | M::Movmskps | M::Movmskpd => {
                let s = ins.op1_register();
                if !is_x(s) || !ins.op0_register().is_gpr32() {
                    return false;
                }
                let soff = self.xoff(s);
                self.c.lget(L_CPU).simd_mem(0x00, soff);
                self.c.simd(match mn {
                    M::Pmovmskb => 0x64,
                    M::Movmskps => 0xa4,
                    _ => 0xc4,
                });
                self.rset(ins.op0_register());
                true
            }
            // ---- shuffles ----
            M::Pshufd | M::Pshuflw | M::Pshufhw => {
                let d = ins.op0_register();
                if !is_x(d) {
                    return false;
                }
                let imm = ins.immediate8() as usize;
                let doff = self.xoff(d);
                let mask: [u8; 16] = match mn {
                    M::Pshufd => lanes(|i| {
                        let sel = (imm >> (2 * (i / 4))) & 3;
                        (sel * 4 + i % 4) as u8
                    }),
                    M::Pshuflw => lanes(|i| {
                        if i < 8 {
                            let sel = (imm >> (2 * (i / 2))) & 3;
                            (sel * 2 + i % 2) as u8
                        } else {
                            i as u8
                        }
                    }),
                    _ => lanes(|i| {
                        if i >= 8 {
                            let sel = (imm >> (2 * ((i - 8) / 2))) & 3;
                            (8 + sel * 2 + i % 2) as u8
                        } else {
                            i as u8
                        }
                    }),
                };
                if ins.op1_kind() == OpKind::Register {
                    let s = ins.op1_register();
                    if !is_x(s) {
                        return false;
                    }
                    let soff = self.xoff(s);
                    self.c.lget(L_CPU).lget(L_CPU).simd_mem(0x00, soff).lset(L_V0);
                    self.c.lget(L_V0).lget(L_V0).shuffle(mask).simd_mem(0x0b, doff);
                } else {
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.simd_guard(ins, a, 16, true, false, |s| {
                        s.c.lget(a).simd_mem(0x00, s.lay.guest_base).lset(L_V0);
                        s.c.lget(L_CPU).lget(L_V0).lget(L_V0).shuffle(mask).simd_mem(0x0b, doff);
                    });
                }
                true
            }
            M::Pslldq | M::Psrldq => {
                let d = ins.op0_register();
                if !is_x(d) {
                    return false;
                }
                let n = ins.immediate8() as usize;
                let doff = self.xoff(d);
                self.c.lget(L_CPU);
                if n > 15 {
                    self.c.v128_const([0; 16]);
                } else {
                    self.c.lget(L_CPU).simd_mem(0x00, doff).v128_const([0; 16]);
                    let mask = if mn == M::Pslldq { lanes(|i| if i < n { 16 } else { (i - n) as u8 }) } else { lanes(|i| if i + n < 16 { (i + n) as u8 } else { 16 }) };
                    self.c.shuffle(mask);
                }
                self.c.simd_mem(0x0b, doff);
                true
            }
            M::Psllw | M::Pslld | M::Psllq | M::Psrlw | M::Psrld | M::Psrlq | M::Psraw | M::Psrad => self.simd_shift(ins, mn),
            M::Pextrw => {
                if !matches!(ins.code(), IC::Pextrw_r32_xmm_imm8) {
                    return false;
                }
                let s = ins.op1_register();
                if !is_x(s) || !ins.op0_register().is_gpr32() {
                    return false;
                }
                let soff = self.xoff(s);
                self.c.lget(L_CPU).simd_mem(0x00, soff).simd_lane(0x19, ins.immediate8() & 7);
                self.rset(ins.op0_register());
                true
            }
            M::Pinsrw => {
                let d = ins.op0_register();
                if !is_x(d) {
                    return false;
                }
                let doff = self.xoff(d);
                let lane = ins.immediate8() & 7;
                // the word is read first (a fault leaves the register untouched)
                let w = if ins.op1_kind() == OpKind::Register {
                    let r = ins.op1_register();
                    if !(r.is_gpr32() || r.is_gpr16()) {
                        return false;
                    }
                    self.rget(r);
                    self.to_tmp()
                } else {
                    let Some(a) = self.ea(ins, true) else { return false };
                    self.load_mem(a, 2, ip);
                    self.to_tmp()
                };
                self.c.lget(L_CPU).lget(L_CPU).simd_mem(0x00, doff);
                self.push_v(w);
                self.c.simd_lane(0x1a, lane).simd_mem(0x0b, doff);
                true
            }
            _ => {
                let Some(kind) = bin_kind(mn) else { return false };
                let d = ins.op0_register();
                if !is_x(d) {
                    return false;
                }
                let doff = self.xoff(d);
                if ins.op1_kind() == OpKind::Register {
                    let s = ins.op1_register();
                    if !is_x(s) {
                        return false;
                    }
                    let soff = self.xoff(s);
                    self.c.lget(L_CPU);
                    self.bin_value(kind, doff, Src::Reg(soff));
                    self.c.simd_mem(0x0b, doff);
                } else {
                    let Some(a) = self.ea_simd(ins) else { return false };
                    self.simd_guard(ins, a, 16, true, false, |s| {
                        s.c.lget(L_CPU);
                        s.bin_value(kind, doff, Src::Mem(a));
                        s.c.simd_mem(0x0b, doff);
                    });
                }
                true
            }
        }
    }

    /// Effective address of the (single) memory operand into a temp, with its segment base; None for 16-bit addressing.
    fn ea_simd(&mut self, ins: &Instruction) -> Option<u32> {
        let (b, x) = (ins.memory_base(), ins.memory_index());
        if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
            return None;
        }
        self.ea(ins, true)
    }

    fn simd_movd(&mut self, ins: &Instruction, ip: u32) -> bool {
        let _ = ip;
        if ins.op0_kind() == OpKind::Register && ins.op0_register().is_xmm() {
            let d = ins.op0_register();
            if !is_x(d) {
                return false;
            }
            let doff = self.xoff(d);
            if ins.op1_kind() == OpKind::Register {
                let r = ins.op1_register();
                if !r.is_gpr32() {
                    return false;
                }
                self.c.lget(L_CPU).v128_const([0; 16]);
                self.rget(r);
                self.c.simd_lane(0x1c, 0).simd_mem(0x0b, doff);
            } else {
                let Some(a) = self.ea_simd(ins) else { return false };
                self.simd_guard(ins, a, 4, false, false, |s| {
                    s.c.lget(L_CPU).lget(a).simd_mem(0x5c, s.lay.guest_base).simd_mem(0x0b, doff);
                });
            }
            true
        } else if ins.op1_kind() == OpKind::Register && ins.op1_register().is_xmm() {
            let s = ins.op1_register();
            if !is_x(s) {
                return false;
            }
            let soff = self.xoff(s);
            if ins.op0_kind() == OpKind::Register {
                let r = ins.op0_register();
                if !r.is_gpr32() {
                    return false;
                }
                self.c.lget(L_CPU).simd_mem(0x00, soff).simd_lane(0x1b, 0);
                self.rset(r);
            } else {
                let Some(a) = self.ea_simd(ins) else { return false };
                self.simd_guard(ins, a, 4, false, true, |s| {
                    s.c.lget(a).lget(L_CPU).simd_mem(0x00, soff).simd_mem_lane(0x5a, s.lay.guest_base, 0);
                });
            }
            true
        } else {
            false // MMX forms
        }
    }

    fn simd_movq(&mut self, ins: &Instruction) -> bool {
        if ins.op0_kind() == OpKind::Register {
            let d = ins.op0_register();
            if !is_x(d) {
                return false;
            }
            let doff = self.xoff(d);
            if ins.op1_kind() == OpKind::Register {
                let s = ins.op1_register();
                if !is_x(s) {
                    return false;
                }
                let soff = self.xoff(s);
                // zero-extends, also reg-reg
                self.c.lget(L_CPU).v128_const([0; 16]).lget(L_CPU).simd_mem(0x00, soff).simd_lane(0x1d, 0).simd_lane(0x1e, 0).simd_mem(0x0b, doff);
            } else {
                let Some(a) = self.ea_simd(ins) else { return false };
                self.simd_guard(ins, a, 8, false, false, |s| {
                    s.c.lget(L_CPU).lget(a).simd_mem(0x5d, s.lay.guest_base).simd_mem(0x0b, doff);
                });
            }
            true
        } else {
            let s = ins.op1_register();
            if !is_x(s) {
                return false;
            }
            let soff = self.xoff(s);
            let Some(a) = self.ea_simd(ins) else { return false };
            self.simd_guard(ins, a, 8, false, true, |s| {
                s.c.lget(a).lget(L_CPU).simd_mem(0x00, soff).simd_mem_lane(0x5b, s.lay.guest_base, 0);
            });
            true
        }
    }

    fn simd_movs(&mut self, ins: &Instruction, single: bool) -> bool {
        let size = if single { 4u8 } else { 8 };
        if ins.op0_kind() == OpKind::Register {
            let d = ins.op0_register();
            if !is_x(d) {
                return false;
            }
            let doff = self.xoff(d);
            if ins.op1_kind() == OpKind::Register {
                let s = ins.op1_register();
                if !is_x(s) {
                    return false;
                }
                let soff = self.xoff(s);
                // merge: the low scalar comes from src, the rest stays
                let mask = lanes(|i| if i < size as usize { 16 + i as u8 } else { i as u8 });
                self.c.lget(L_CPU).lget(L_CPU).simd_mem(0x00, doff).lget(L_CPU).simd_mem(0x00, soff).shuffle(mask).simd_mem(0x0b, doff);
            } else {
                let Some(a) = self.ea_simd(ins) else { return false };
                self.simd_guard(ins, a, size, false, false, |s| {
                    s.c.lget(L_CPU).lget(a).simd_mem(if single { 0x5c } else { 0x5d }, s.lay.guest_base).simd_mem(0x0b, doff);
                });
            }
            true
        } else {
            let s = ins.op1_register();
            if !is_x(s) {
                return false;
            }
            let soff = self.xoff(s);
            let Some(a) = self.ea_simd(ins) else { return false };
            self.simd_guard(ins, a, size, false, true, |s| {
                s.c.lget(a).lget(L_CPU).simd_mem(0x00, soff).simd_mem_lane(if single { 0x5a } else { 0x5b }, s.lay.guest_base, 0);
            });
            true
        }
    }

    fn simd_shift(&mut self, ins: &Instruction, mn: M) -> bool {
        #[derive(PartialEq)]
        enum K {
            Logical,
            Arith,
        }
        let (opc, width, kind): (u32, u32, K) = match mn {
            M::Psllw => (0x8b, 16, K::Logical),
            M::Pslld => (0xab, 32, K::Logical),
            M::Psllq => (0xcb, 64, K::Logical),
            M::Psrlw => (0x8d, 16, K::Logical),
            M::Psrld => (0xad, 32, K::Logical),
            M::Psrlq => (0xcd, 64, K::Logical),
            M::Psraw => (0x8c, 16, K::Arith),
            _ => (0xac, 32, K::Arith),
        };
        let d = ins.op0_register();
        if !is_x(d) {
            return false;
        }
        let doff = self.xoff(d);
        if ins.op1_kind() == OpKind::Immediate8 {
            let c = ins.immediate8() as u32;
            self.c.lget(L_CPU);
            if kind == K::Logical && c >= width {
                self.c.v128_const([0; 16]);
            } else {
                self.c.lget(L_CPU).simd_mem(0x00, doff);
                self.c.i32c(if kind == K::Arith { c.min(width - 1) } else { c } as i32).simd(opc);
            }
            self.c.simd_mem(0x0b, doff);
            return true;
        }
        // count = low 64 bits of an xmm register or an aligned m128
        let t0 = Self::t64(0);
        let finish = |s: &mut Self| {
            // t0 = count; V0 = value to shift
            s.c.lget(L_CPU);
            match kind {
                K::Logical => {
                    s.c.lget(t0).i64c(width as i64).op(I64_LT_U);
                    s.if_(V128);
                    s.c.lget(L_V0).lget(t0).op(I32_WRAP_I64).simd(opc);
                    s.else_();
                    s.c.v128_const([0; 16]);
                    s.close();
                }
                K::Arith => {
                    s.c.lget(L_V0);
                    s.c.lget(t0).i64c(width as i64 - 1).lget(t0).i64c(width as i64 - 1).op(I64_LT_U).select();
                    s.c.op(I32_WRAP_I64).simd(opc);
                }
            }
            s.c.simd_mem(0x0b, doff);
        };
        if ins.op1_kind() == OpKind::Register {
            let sr = ins.op1_register();
            if !is_x(sr) {
                return false;
            }
            let soff = self.xoff(sr);
            self.c.lget(L_CPU).simd_mem(0x00, soff).simd_lane(0x1d, 0).lset(t0);
            self.c.lget(L_CPU).simd_mem(0x00, doff).lset(L_V0);
            finish(self);
        } else {
            let Some(a) = self.ea_simd(ins) else { return false };
            self.simd_guard(ins, a, 16, true, false, |s| {
                s.c.lget(a).simd_mem(0x00, s.lay.guest_base).simd_lane(0x1d, 0).lset(t0);
                s.c.lget(L_CPU).simd_mem(0x00, doff).lset(L_V0);
                finish(s);
            });
        }
        true
    }
}

#[path = "jit_mmx.rs"]
mod mmx;
#[path = "jit_ps.rs"]
mod ps;
