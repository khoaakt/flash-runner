//! Integer / control-flow interpreter. One `exec` call runs one decoded instruction; `self.eip` has
//! already been advanced to the next instruction when `exec` is entered.

use crate::cpu::*;
use crate::mem::Memory;
use iced_x86::{Code, Instruction, Mnemonic as M, OpKind, Register};

fn cc_of(mn: M) -> u8 {
    match mn {
        M::Jo | M::Seto | M::Cmovo => 0,
        M::Jno | M::Setno | M::Cmovno => 1,
        M::Jb | M::Setb | M::Cmovb => 2,
        M::Jae | M::Setae | M::Cmovae => 3,
        M::Je | M::Sete | M::Cmove => 4,
        M::Jne | M::Setne | M::Cmovne => 5,
        M::Jbe | M::Setbe | M::Cmovbe => 6,
        M::Ja | M::Seta | M::Cmova => 7,
        M::Js | M::Sets | M::Cmovs => 8,
        M::Jns | M::Setns | M::Cmovns => 9,
        M::Jp | M::Setp | M::Cmovp => 10,
        M::Jnp | M::Setnp | M::Cmovnp => 11,
        M::Jl | M::Setl | M::Cmovl => 12,
        M::Jge | M::Setge | M::Cmovge => 13,
        M::Jle | M::Setle | M::Cmovle => 14,
        M::Jg | M::Setg | M::Cmovg => 15,
        _ => 0,
    }
}

#[inline(always)]
fn ud(ins: &Instruction) -> Exit {
    Exit::Undef { eip: ins.ip() as u32 }
}

impl Cpu {
    // ---- operand plumbing ------------------------------------------------------------------
    #[inline(always)]
    pub(crate) fn opsz(&self, ins: &Instruction, i: u32) -> u8 {
        match ins.op_kind(i) {
            OpKind::Register => Cpu::reg_size(ins.op_register(i)),
            OpKind::Memory => ins.memory_size().size() as u8,
            _ => 4,
        }
    }

    #[inline(always)]
    pub(crate) fn loc(&self, ins: &Instruction, i: u32) -> XResult<Loc> {
        match ins.op_kind(i) {
            OpKind::Register => Ok(Loc::Reg(ins.op_register(i))),
            OpKind::Memory => Ok(Loc::Mem(self.ea(ins)?, ins.memory_size().size() as u8)),
            _ => Err(ud(ins)),
        }
    }

    #[inline(always)]
    pub(crate) fn ld(&self, m: &Memory, l: Loc) -> XResult<u32> {
        match l {
            Loc::Reg(r) => Ok(self.get_reg(r)),
            Loc::Mem(a, sz) => Ok(m.rn(a, sz)?),
        }
    }

    #[inline(always)]
    pub(crate) fn st(&mut self, m: &mut Memory, l: Loc, v: u32) -> XResult<()> {
        match l {
            Loc::Reg(r) => {
                self.set_reg(r, v);
                Ok(())
            }
            Loc::Mem(a, sz) => Ok(m.wn(a, sz, v)?),
        }
    }

    #[inline(always)]
    pub(crate) fn rd(&self, m: &Memory, ins: &Instruction, i: u32) -> XResult<u32> {
        Ok(match ins.op_kind(i) {
            OpKind::Register => self.get_reg(ins.op_register(i)),
            OpKind::Memory => {
                let a = self.ea(ins)?;
                m.rn(a, ins.memory_size().size() as u8)?
            }
            OpKind::Immediate8 => ins.immediate8() as u32,
            OpKind::Immediate8_2nd => ins.immediate8_2nd() as u32,
            OpKind::Immediate16 => ins.immediate16() as u32,
            OpKind::Immediate32 => ins.immediate32(),
            OpKind::Immediate8to16 => ins.immediate8to16() as u16 as u32,
            OpKind::Immediate8to32 => ins.immediate8to32() as u32,
            OpKind::NearBranch32 => ins.near_branch32(),
            _ => return Err(ud(ins)),
        })
    }

    #[inline(always)]
    pub(crate) fn wr(&mut self, m: &mut Memory, ins: &Instruction, i: u32, v: u32) -> XResult<()> {
        let l = self.loc(ins, i)?;
        self.st(m, l, v)
    }

    fn load_seg(&mut self, idx: usize, sel: u32) {
        self.seg[idx] = sel as u16;
        let slot = (sel >> 3) as i32 - 6;
        let base = if (0..3).contains(&slot) { self.tls_base[slot as usize] } else { 0 };
        match idx {
            4 => self.fs_base = base,
            5 => self.gs_base = base,
            _ => {}
        }
    }

    /// Synthetic view of the flat Linux/i386 GDT this emulator pretends to run on (see `load_seg`):
    /// the user code/data selectors plus the three TLS slots. Everything else (null, kernel DPL0,
    /// LDT) is "not usable from CPL 3". Returns the LAR access-rights word and whether writable.
    fn vdesc(sel: u32) -> Option<(u32, bool)> {
        let s = sel & 0xffff;
        if s & 4 != 0 {
            return None; // LDT selectors: the guest has no LDT
        }
        match s & !3 {
            0x20 => Some((0xc0fb00, false)),                     // __USER_CS: flat 32-bit code, R/A, G, DPL3
            0x28 | 0x30 | 0x38 | 0x40 => Some((0xc0f300, true)), // __USER_DS + TLS entries 6..8: flat data, RW
            _ => None,
        }
    }

    // ---- shifts / rotates ------------------------------------------------------------------
    pub(crate) fn shift(&mut self, mn: M, a: u32, cnt: u32, sz: u8) -> u32 {
        let bits = sz as u32 * 8;
        let m = mask(sz);
        let a = a & m;
        let cnt = cnt & 0x1f;
        if cnt == 0 {
            return a;
        }
        match mn {
            M::Shl | M::Sal => {
                let v = (a as u64) << cnt;
                let res = (v as u32) & m;
                let cf = (((a as u64) << (cnt - 1)) >> (bits - 1)) & 1 != 0;
                self.set_flag(CF, cf);
                self.set_flag(OF, ((res & sign_bit(sz)) != 0) != cf);
                self.set_szp(res, sz);
                self.set_flag(AF, false);
                res
            }
            M::Shr => {
                let res = ((a as u64) >> cnt) as u32;
                self.set_flag(CF, ((a as u64) >> (cnt - 1)) & 1 != 0);
                self.set_flag(OF, a & sign_bit(sz) != 0);
                self.set_szp(res, sz);
                self.set_flag(AF, false);
                res
            }
            M::Sar => {
                let s = sext(a, sz) as i64;
                let res = ((s >> cnt) as u32) & m;
                self.set_flag(CF, (s >> (cnt - 1)) & 1 != 0);
                self.set_flag(OF, false);
                self.set_szp(res, sz);
                self.set_flag(AF, false);
                res
            }
            M::Rol => {
                let e = cnt % bits;
                let res = if e == 0 { a } else { ((a << e) | (a >> (bits - e))) & m };
                self.set_flag(CF, res & 1 != 0);
                self.set_flag(OF, ((res & sign_bit(sz)) != 0) != (res & 1 != 0));
                res
            }
            M::Ror => {
                let e = cnt % bits;
                let res = if e == 0 { a } else { ((a >> e) | (a << (bits - e))) & m };
                let msb = res & sign_bit(sz) != 0;
                self.set_flag(CF, msb);
                self.set_flag(OF, msb != ((res & (sign_bit(sz) >> 1)) != 0));
                res
            }
            M::Rcl => {
                let mut v = a;
                let mut c = self.flag(CF);
                let n = if bits == 32 { cnt } else { cnt % (bits + 1) };
                for _ in 0..n {
                    let out = v & sign_bit(sz) != 0;
                    v = ((v << 1) | c as u32) & m;
                    c = out;
                }
                self.set_flag(CF, c);
                self.set_flag(OF, ((v & sign_bit(sz)) != 0) != c);
                v
            }
            M::Rcr => {
                let mut v = a;
                let mut c = self.flag(CF);
                let n = if bits == 32 { cnt } else { cnt % (bits + 1) };
                let of = ((v & sign_bit(sz)) != 0) != c;
                for _ in 0..n {
                    let out = v & 1 != 0;
                    v = (v >> 1) | if c { sign_bit(sz) } else { 0 };
                    c = out;
                }
                self.set_flag(CF, c);
                self.set_flag(OF, of);
                v
            }
            _ => a,
        }
    }

    fn double_shift(&mut self, left: bool, dst: u32, src: u32, cnt: u32, sz: u8) -> u32 {
        let cnt = cnt & 0x1f;
        if cnt == 0 {
            return dst;
        }
        let bits = sz as u32 * 8;
        let m = mask(sz);
        let (dst, src) = (dst & m, src & m);
        let res;
        let cf;
        if left {
            let v = (((dst as u128) << bits) | src as u128) << cnt;
            res = ((v >> bits) as u32) & m;
            cf = (v >> (2 * bits)) & 1 != 0;
        } else {
            let v = (((src as u128) << bits) | dst as u128) >> (cnt - 1);
            cf = v & 1 != 0;
            res = ((v >> 1) as u32) & m;
        }
        self.set_flag(CF, cf);
        self.set_flag(OF, ((res ^ dst) & sign_bit(sz)) != 0);
        self.set_szp(res, sz);
        self.set_flag(AF, false);
        res
    }

    // ---- CPUID -----------------------------------------------------------------------------
    fn cpuid(&mut self) {
        let (a, b, c, d) = match self.r[EAX] {
            0 => (1, u32::from_le_bytes(*b"Genu"), u32::from_le_bytes(*b"ntel"), u32::from_le_bytes(*b"ineI")),
            // family 6, model 15 (Core 2), stepping 1. SSE/SSE2 only: nothing newer is advertised.
            1 => (0x0000_06f1, 0x0001_0800, 0, self.cpuid_edx),
            0x8000_0000 => (0x8000_0001, 0, 0, 0),
            0x8000_0001 => (0, 0, 0, 0),
            _ => (0, 0, 0, 0),
        };
        self.r[EAX] = a;
        self.r[EBX] = b;
        self.r[ECX] = c;
        self.r[EDX] = d;
    }

    // ---- string instructions ---------------------------------------------------------------
    fn string_op(&mut self, m: &mut Memory, ins: &Instruction) -> XResult<()> {
        let mn = ins.mnemonic();
        let sz: u8 = match ins.code() {
            Code::Movsb_m8_m8 | Code::Cmpsb_m8_m8 | Code::Stosb_m8_AL | Code::Lodsb_AL_m8 | Code::Scasb_AL_m8 => 1,
            Code::Movsw_m16_m16 | Code::Cmpsw_m16_m16 | Code::Stosw_m16_AX | Code::Lodsw_AX_m16 | Code::Scasw_AX_m16 => 2,
            _ => 4,
        };
        let rep = ins.has_rep_prefix() || ins.has_repe_prefix();
        let repne = ins.has_repne_prefix();
        let step: u32 = if self.flag(DF) { (sz as u32).wrapping_neg() } else { sz as u32 };
        let src_base = self.seg_base(ins);
        let compare = matches!(mn, M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Scasb | M::Scasw | M::Scasd);
        loop {
            if (rep || repne) && self.r[ECX] == 0 {
                break;
            }
            match mn {
                M::Movsb | M::Movsw | M::Movsd => {
                    let v = m.rn(self.r[ESI].wrapping_add(src_base), sz)?;
                    m.wn(self.r[EDI], sz, v)?;
                    self.r[ESI] = self.r[ESI].wrapping_add(step);
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                M::Stosb | M::Stosw | M::Stosd => {
                    m.wn(self.r[EDI], sz, self.r[EAX])?;
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                M::Lodsb | M::Lodsw | M::Lodsd => {
                    let v = m.rn(self.r[ESI].wrapping_add(src_base), sz)?;
                    let r = match sz {
                        1 => Register::AL,
                        2 => Register::AX,
                        _ => Register::EAX,
                    };
                    self.set_reg(r, v);
                    self.r[ESI] = self.r[ESI].wrapping_add(step);
                }
                M::Scasb | M::Scasw | M::Scasd => {
                    let v = m.rn(self.r[EDI], sz)?;
                    self.sub_flags(self.r[EAX], v, 0, sz);
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                M::Cmpsb | M::Cmpsw | M::Cmpsd => {
                    let a = m.rn(self.r[ESI].wrapping_add(src_base), sz)?;
                    let b = m.rn(self.r[EDI], sz)?;
                    self.sub_flags(a, b, 0, sz);
                    self.r[ESI] = self.r[ESI].wrapping_add(step);
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                _ => return Err(ud(ins)),
            }
            if rep || repne {
                self.r[ECX] = self.r[ECX].wrapping_sub(1);
                if compare {
                    if rep && !self.flag(ZF) {
                        break;
                    }
                    if repne && self.flag(ZF) {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    // ---- main dispatch ---------------------------------------------------------------------
    pub fn exec(&mut self, m: &mut Memory, ins: &Instruction) -> XResult<()> {
        self.lf_flush(); // JIT hook: fold lazy flags left by compiled code into `fl`
        let mn = ins.mnemonic();
        match mn {
            M::Mov => {
                if ins.op0_kind() == OpKind::Register && ins.op0_register().is_segment_register() {
                    let v = self.rd(m, ins, 1)?;
                    self.load_seg(ins.op0_register().number(), v);
                } else {
                    let v = self.rd(m, ins, 1)?;
                    self.wr(m, ins, 0, v)?;
                }
            }
            M::Movzx => {
                let v = self.rd(m, ins, 1)?;
                self.wr(m, ins, 0, v)?;
            }
            M::Movsx => {
                let s = self.opsz(ins, 1);
                let v = self.rd(m, ins, 1)?;
                self.wr(m, ins, 0, sext(v, s) as u32)?;
            }
            M::Lea => {
                let a = self.ea(ins)?.wrapping_sub(self.seg_base(ins));
                self.wr(m, ins, 0, a)?;
            }
            M::Xchg => {
                let sz = self.opsz(ins, 0);
                let l0 = self.loc(ins, 0)?;
                let l1 = self.loc(ins, 1)?;
                let (a, b) = (self.ld(m, l0)? & mask(sz), self.ld(m, l1)? & mask(sz));
                self.st(m, l0, b)?;
                self.st(m, l1, a)?;
            }
            M::Push => {
                let sz = match ins.op0_kind() {
                    OpKind::Register => Cpu::reg_size(ins.op0_register()),
                    OpKind::Memory => ins.memory_size().size() as u8,
                    OpKind::Immediate16 | OpKind::Immediate8to16 => 2,
                    _ => 4,
                };
                let v = self.rd(m, ins, 0)?;
                if sz == 2 {
                    self.push16(m, v as u16)?;
                } else {
                    self.push32(m, v)?;
                }
            }
            M::Pop => {
                let sz = match ins.op0_kind() {
                    OpKind::Register => Cpu::reg_size(ins.op0_register()),
                    OpKind::Memory => ins.memory_size().size() as u8,
                    _ => 4,
                };
                let v = if sz == 2 { self.pop16(m)? as u32 } else { self.pop32(m)? };
                if ins.op0_kind() == OpKind::Register && ins.op0_register().is_segment_register() {
                    self.load_seg(ins.op0_register().number(), v);
                } else {
                    self.wr(m, ins, 0, v)?;
                }
            }
            M::Pushad => {
                let sp = self.r[ESP];
                for r in [EAX, ECX, EDX, EBX] {
                    self.push32(m, self.r[r])?;
                }
                self.push32(m, sp)?;
                for r in [EBP, ESI, EDI] {
                    self.push32(m, self.r[r])?;
                }
            }
            M::Popad => {
                for r in [EDI, ESI, EBP] {
                    self.r[r] = self.pop32(m)?;
                }
                self.pop32(m)?;
                for r in [EBX, EDX, ECX, EAX] {
                    self.r[r] = self.pop32(m)?;
                }
            }
            M::Pushfd => {
                let v = (self.fl & 0x00fc_ffff) | 2;
                self.push32(m, v)?;
            }
            M::Popfd => {
                let v = self.pop32(m)?;
                self.fl = (self.fl & !POPF_MASK) | (v & POPF_MASK) | 2 | IF;
            }
            M::Pushf => {
                let v = (self.fl & 0xffff) | 2;
                self.push16(m, v as u16)?;
            }
            M::Popf => {
                let v = self.pop16(m)? as u32;
                self.fl = (self.fl & !(POPF_MASK & 0xffff)) | (v & POPF_MASK & 0xffff) | 2 | IF;
            }

            // ---- ALU ----
            M::Add | M::Adc | M::Sub | M::Sbb | M::And | M::Or | M::Xor | M::Cmp | M::Test => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let a = self.ld(m, l)?;
                let b = self.rd(m, ins, 1)?;
                let res = match mn {
                    M::Add => self.add_flags(a, b, 0, sz),
                    M::Adc => {
                        let c = self.flag(CF) as u32;
                        self.add_flags(a, b, c, sz)
                    }
                    M::Sub | M::Cmp => self.sub_flags(a, b, 0, sz),
                    M::Sbb => {
                        let c = self.flag(CF) as u32;
                        self.sub_flags(a, b, c, sz)
                    }
                    M::And | M::Test => self.logic_flags(a & b, sz),
                    M::Or => self.logic_flags(a | b, sz),
                    _ => self.logic_flags(a ^ b, sz),
                };
                if mn != M::Cmp && mn != M::Test {
                    self.st(m, l, res)?;
                }
            }
            M::Inc | M::Dec => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let a = self.ld(m, l)?;
                let cf = self.flag(CF);
                let res = if mn == M::Inc { self.add_flags(a, 1, 0, sz) } else { self.sub_flags(a, 1, 0, sz) };
                self.set_flag(CF, cf);
                self.st(m, l, res)?;
            }
            M::Neg => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let a = self.ld(m, l)?;
                let res = self.sub_flags(0, a, 0, sz);
                self.st(m, l, res)?;
            }
            M::Not => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let a = self.ld(m, l)?;
                self.st(m, l, !a & mask(sz))?;
            }
            M::Mul | M::Imul if ins.op_count() == 1 => {
                let sz = self.opsz(ins, 0);
                let src = self.rd(m, ins, 0)?;
                let signed = mn == M::Imul;
                match sz {
                    1 => {
                        let al = self.r[EAX] & 0xff;
                        let p: u32 = if signed { ((al as u8 as i8 as i32) * (src as u8 as i8 as i32)) as u32 & 0xffff } else { al * (src & 0xff) };
                        self.r[EAX] = (self.r[EAX] & 0xffff_0000) | p;
                        let over = if signed { p as u16 as i16 as i32 != (p as u8 as i8 as i32) } else { p >> 8 != 0 };
                        self.set_flag(CF, over);
                        self.set_flag(OF, over);
                    }
                    2 => {
                        let ax = self.r[EAX] & 0xffff;
                        let p: u32 = if signed { ((ax as u16 as i16 as i32) * (src as u16 as i16 as i32)) as u32 } else { ax * (src & 0xffff) };
                        self.r[EAX] = (self.r[EAX] & 0xffff_0000) | (p & 0xffff);
                        self.r[EDX] = (self.r[EDX] & 0xffff_0000) | (p >> 16);
                        let over = if signed { p as i32 != (p as u16 as i16 as i32) } else { p >> 16 != 0 };
                        self.set_flag(CF, over);
                        self.set_flag(OF, over);
                    }
                    _ => {
                        let eax = self.r[EAX];
                        let (lo, hi, over);
                        if signed {
                            let p = (eax as i32 as i64) * (src as i32 as i64);
                            lo = p as u32;
                            hi = (p >> 32) as u32;
                            over = p != (p as i32 as i64);
                        } else {
                            let p = (eax as u64) * (src as u64);
                            lo = p as u32;
                            hi = (p >> 32) as u32;
                            over = hi != 0;
                        }
                        self.r[EAX] = lo;
                        self.r[EDX] = hi;
                        self.set_flag(CF, over);
                        self.set_flag(OF, over);
                    }
                }
            }
            M::Imul => {
                // 2- and 3-operand forms
                let sz = self.opsz(ins, 0);
                let (a, b) = if ins.op_count() == 3 { (self.rd(m, ins, 1)?, self.rd(m, ins, 2)?) } else { (self.rd(m, ins, 0)?, self.rd(m, ins, 1)?) };
                let p = (sext(a, sz) as i64) * (sext(b, sz) as i64);
                let res = (p as u32) & mask(sz);
                let over = p != sext(res, sz) as i64;
                self.set_flag(CF, over);
                self.set_flag(OF, over);
                self.set_szp(res, sz);
                self.wr(m, ins, 0, res)?;
            }
            M::Div | M::Idiv => {
                let sz = self.opsz(ins, 0);
                let src = self.rd(m, ins, 0)?;
                let signed = mn == M::Idiv;
                let err = Exit::DivideError { eip: ins.ip() as u32 };
                match sz {
                    1 => {
                        let n = self.r[EAX] & 0xffff;
                        let (q, r);
                        if signed {
                            let d = src as u8 as i8 as i32;
                            if d == 0 {
                                return Err(err);
                            }
                            let nn = n as u16 as i16 as i32;
                            let qq = nn / d;
                            if !(-128..=127).contains(&qq) {
                                return Err(err);
                            }
                            q = qq as u32 & 0xff;
                            r = (nn % d) as u32 & 0xff;
                        } else {
                            let d = src & 0xff;
                            if d == 0 || n / d > 0xff {
                                return Err(err);
                            }
                            q = n / d;
                            r = n % d;
                        }
                        self.r[EAX] = (self.r[EAX] & 0xffff_0000) | (r << 8) | q;
                    }
                    2 => {
                        let n = ((self.r[EDX] & 0xffff) << 16) | (self.r[EAX] & 0xffff);
                        let (q, r);
                        if signed {
                            let d = src as u16 as i16 as i32;
                            if d == 0 {
                                return Err(err);
                            }
                            let nn = n as i32;
                            let qq = nn.wrapping_div(d);
                            if !(-32768..=32767).contains(&(nn as i64 / d as i64)) {
                                return Err(err);
                            }
                            q = qq as u32 & 0xffff;
                            r = nn.wrapping_rem(d) as u32 & 0xffff;
                        } else {
                            let d = src & 0xffff;
                            if d == 0 || n / d > 0xffff {
                                return Err(err);
                            }
                            q = n / d;
                            r = n % d;
                        }
                        self.r[EAX] = (self.r[EAX] & 0xffff_0000) | q;
                        self.r[EDX] = (self.r[EDX] & 0xffff_0000) | r;
                    }
                    _ => {
                        let n = ((self.r[EDX] as u64) << 32) | self.r[EAX] as u64;
                        if signed {
                            let d = src as i32 as i64;
                            if d == 0 {
                                return Err(err);
                            }
                            let nn = n as i64;
                            let qq = nn as i128 / d as i128;
                            if qq < i32::MIN as i128 || qq > i32::MAX as i128 {
                                return Err(err);
                            }
                            self.r[EAX] = qq as i32 as u32;
                            self.r[EDX] = (nn as i128 % d as i128) as i32 as u32;
                        } else {
                            let d = src as u64;
                            if d == 0 || n / d > 0xffff_ffff {
                                return Err(err);
                            }
                            self.r[EAX] = (n / d) as u32;
                            self.r[EDX] = (n % d) as u32;
                        }
                    }
                }
            }

            // ---- BCD helpers (legacy; gcc never emits them, hand-written asm does) ----
            M::Daa | M::Das => {
                // The high adjust is decided by the ORIGINAL AL and CF (threshold 0x99), as on real
                // silicon: qemu, Bochs and DOSBox all agree on this; the SDM's sequential "> 0x9F"
                // reading is equivalent for DAA but differs for DAS. Differential-tested against
                // the container oracle (tests/progs/legacy_bcd.c).
                let old_al = (self.r[EAX] & 0xff) as u16;
                let cf0 = self.flag(CF);
                let low = (old_al & 0x0f) > 9 || self.flag(AF);
                let high = old_al > 0x99 || cf0;
                let mut al = old_al;
                if low {
                    al = if mn == M::Daa { (al + 6) & 0xff } else { al.wrapping_sub(6) & 0xff };
                }
                if high {
                    al = if mn == M::Daa { (al + 0x60) & 0xff } else { al.wrapping_sub(0x60) & 0xff };
                }
                // DAA: the low-adjust carry is discarded (it implies old_al >= 0xFA, so `high` fires
                // and CF is set anyway). DAS: the low adjust sets CF on a borrow out of AL.
                let cf = high || (mn == M::Das && low && (old_al < 6 || cf0));
                self.set_flag(AF, low);
                self.set_flag(CF, cf);
                self.set_flag(OF, false);
                self.set_szp(al as u32, 1);
                self.r[EAX] = (self.r[EAX] & !0xff) | al as u32;
            }
            M::Aaa | M::Aas => {
                let al = self.r[EAX] & 0xff;
                let ah = (self.r[EAX] >> 8) & 0xff;
                let adj = (al & 0x0f) > 9 || self.flag(AF);
                let (al, ah) = if adj {
                    if mn == M::Aaa {
                        let icarry = al > 0xf9; // carry out of AL+6 propagates into AH
                        ((al + 6) & 0x0f, (ah + 1 + icarry as u32) & 0xff)
                    } else {
                        let iborrow = al < 6; // borrow out of AL-6 propagates into AH
                        (al.wrapping_sub(6) & 0x0f, ah.wrapping_sub(1 + iborrow as u32) & 0xff)
                    }
                } else {
                    (al & 0x0f, ah)
                };
                self.set_flag(AF, adj);
                self.set_flag(CF, adj);
                self.r[EAX] = (self.r[EAX] & !0xffff) | (ah << 8) | al;
            }
            M::Aam => {
                let base = ins.immediate8();
                if base == 0 {
                    return Err(Exit::DivideError { eip: ins.ip() as u32 });
                }
                let al = (self.r[EAX] & 0xff) as u8;
                let (ah, al) = (al / base, al % base);
                self.r[EAX] = (self.r[EAX] & !0xffff) | (((ah as u32) << 8) | al as u32);
                self.fl &= !(CF | OF | AF); // undefined per SDM; the oracle (qemu) clears them
                self.set_szp(al as u32, 1);
            }
            M::Aad => {
                let base = ins.immediate8() as u32;
                let al = self.r[EAX] & 0xff;
                let ah = (self.r[EAX] >> 8) & 0xff;
                let res = (ah * base + al) & 0xff;
                self.r[EAX] = (self.r[EAX] & !0xffff) | res;
                self.fl &= !(CF | OF | AF);
                self.set_szp(res, 1);
            }

            // ---- shifts ----
            M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let a = self.ld(m, l)?;
                let cnt = if ins.op_count() > 1 { self.rd(m, ins, 1)? } else { 1 };
                let res = self.shift(mn, a, cnt, sz);
                self.st(m, l, res)?;
            }
            M::Shld | M::Shrd => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let dst = self.ld(m, l)?;
                let src = self.rd(m, ins, 1)?;
                let cnt = self.rd(m, ins, 2)?;
                let res = self.double_shift(mn == M::Shld, dst, src, cnt, sz);
                self.st(m, l, res)?;
            }

            // ---- bit ops ----
            M::Bt | M::Bts | M::Btr | M::Btc => {
                let sz = self.opsz(ins, 0);
                let bits = sz as u32 * 8;
                let mut l = self.loc(ins, 0)?;
                let off = self.rd(m, ins, 1)?;
                let bit;
                if let (Loc::Mem(a, s), OpKind::Register) = (l, ins.op1_kind()) {
                    let so = sext(off, sz) as i32;
                    let words = so.div_euclid(bits as i32);
                    l = Loc::Mem(a.wrapping_add((words * sz as i32) as u32), s);
                    bit = so.rem_euclid(bits as i32) as u32;
                } else {
                    bit = off & (bits - 1);
                }
                let v = self.ld(m, l)?;
                self.set_flag(CF, (v >> bit) & 1 != 0);
                let nv = match mn {
                    M::Bts => v | (1 << bit),
                    M::Btr => v & !(1 << bit),
                    M::Btc => v ^ (1 << bit),
                    _ => v,
                };
                if mn != M::Bt {
                    self.st(m, l, nv)?;
                }
            }
            M::Bsf | M::Bsr => {
                let sz = self.opsz(ins, 0);
                let src = self.rd(m, ins, 1)? & mask(sz);
                if src == 0 {
                    self.set_flag(ZF, true);
                } else {
                    self.set_flag(ZF, false);
                    let r = if mn == M::Bsf { src.trailing_zeros() } else { 31 - src.leading_zeros() };
                    self.wr(m, ins, 0, r)?;
                }
            }
            M::Tzcnt | M::Lzcnt => {
                let sz = self.opsz(ins, 0);
                let src = self.rd(m, ins, 1)? & mask(sz);
                let bits = sz as u32 * 8;
                let r = if src == 0 {
                    bits
                } else if mn == M::Tzcnt {
                    src.trailing_zeros()
                } else {
                    src.leading_zeros() - (32 - bits)
                };
                self.set_flag(CF, src == 0);
                self.set_flag(ZF, r == 0);
                self.wr(m, ins, 0, r)?;
            }
            M::Popcnt => {
                let src = self.rd(m, ins, 1)?;
                self.fl &= !(CF | OF | SF | AF | PF);
                self.set_flag(ZF, src == 0);
                self.wr(m, ins, 0, src.count_ones())?;
            }
            M::Bswap => {
                let r = ins.op0_register();
                self.set_reg(r, self.get_reg(r).swap_bytes());
            }
            M::Xadd => {
                let sz = self.opsz(ins, 0);
                let l0 = self.loc(ins, 0)?;
                let l1 = self.loc(ins, 1)?;
                let (a, b) = (self.ld(m, l0)?, self.ld(m, l1)?);
                let sum = self.add_flags(a, b, 0, sz);
                self.st(m, l1, a)?;
                self.st(m, l0, sum)?;
            }
            M::Cmpxchg => {
                let sz = self.opsz(ins, 0);
                let l = self.loc(ins, 0)?;
                let dst = self.ld(m, l)?;
                let acc_reg = match sz {
                    1 => Register::AL,
                    2 => Register::AX,
                    _ => Register::EAX,
                };
                let acc = self.get_reg(acc_reg);
                self.sub_flags(acc, dst, 0, sz);
                if acc & mask(sz) == dst & mask(sz) {
                    let v = self.rd(m, ins, 1)?;
                    self.st(m, l, v)?;
                } else {
                    // The destination is written back even when the compare fails (faults on read-only memory).
                    if let Loc::Mem(..) = l {
                        self.st(m, l, dst)?;
                    }
                    self.set_reg(acc_reg, dst);
                }
            }
            M::Cmpxchg8b => {
                let a = self.ea(ins)?;
                let cur = m.r64(a)?;
                let want = ((self.r[EDX] as u64) << 32) | self.r[EAX] as u64;
                if cur == want {
                    m.w64(a, ((self.r[ECX] as u64) << 32) | self.r[EBX] as u64)?;
                    self.set_flag(ZF, true);
                } else {
                    self.r[EAX] = cur as u32;
                    self.r[EDX] = (cur >> 32) as u32;
                    self.set_flag(ZF, false);
                }
            }

            // ---- conditionals ----
            M::Cmovo | M::Cmovno | M::Cmovb | M::Cmovae | M::Cmove | M::Cmovne | M::Cmovbe | M::Cmova | M::Cmovs | M::Cmovns | M::Cmovp | M::Cmovnp | M::Cmovl | M::Cmovge | M::Cmovle | M::Cmovg => {
                let v = self.rd(m, ins, 1)?;
                if self.cond(cc_of(mn)) {
                    self.wr(m, ins, 0, v)?;
                }
            }
            M::Seto | M::Setno | M::Setb | M::Setae | M::Sete | M::Setne | M::Setbe | M::Seta | M::Sets | M::Setns | M::Setp | M::Setnp | M::Setl | M::Setge | M::Setle | M::Setg => {
                let v = self.cond(cc_of(mn)) as u32;
                self.wr(m, ins, 0, v)?;
            }

            // ---- legacy / protected-mode queries (segment state is flat and cosmetic) ----
            M::Bound => {
                if ins.op1_kind() != OpKind::Memory {
                    return Err(ud(ins)); // bound r32, m64 only
                }
                let a = self.ea(ins)?;
                let sz = self.opsz(ins, 0);
                let idx = sext(self.rd(m, ins, 0)?, sz);
                let lo = sext(m.rn(a, sz)?, sz);
                let hi = sext(m.rn(a.wrapping_add(sz as u32), sz)?, sz);
                if idx < lo || idx > hi {
                    return Err(Exit::Bounds { eip: ins.ip() as u32 });
                }
            }
            M::Arpl => {
                let l = self.loc(ins, 0)?;
                let dst = self.ld(m, l)? & 0xffff;
                let src = self.rd(m, ins, 1)? & 3;
                if (dst & 3) < src {
                    self.set_flag(ZF, true);
                    self.st(m, l, (dst & !3) | src)?;
                } else {
                    self.set_flag(ZF, false);
                }
            }
            M::Les | M::Lds | M::Lfs | M::Lgs | M::Lss => {
                // far pointer: [ea] = offset, [ea+size] = selector; no GDT validation (flat model,
                // same policy as `mov Sreg, ...` above). Memory is read before any register is
                // written. The register-source form is not an instruction (#UD).
                if ins.op1_kind() != OpKind::Memory {
                    return Err(ud(ins));
                }
                let a = self.ea(ins)?;
                let sz = self.opsz(ins, 0);
                let (off, sel) = if sz == 2 {
                    (m.r16(a)? as u32, m.r16(a.wrapping_add(2))?) // m32 with a 16-bit operand size
                } else {
                    (m.r32(a)?, m.r16(a.wrapping_add(4))?)
                };
                self.wr(m, ins, 0, off)?;
                let sr = match mn {
                    M::Les => Register::ES,
                    M::Lds => Register::DS,
                    M::Lfs => Register::FS,
                    M::Lgs => Register::GS,
                    _ => Register::SS,
                };
                self.load_seg(sr.number(), sel as u32);
            }
            M::Lar | M::Lsl => {
                let src = self.rd(m, ins, 1)? & 0xffff;
                match Self::vdesc(src) {
                    Some((ar, _)) => {
                        self.set_flag(ZF, true);
                        self.wr(m, ins, 0, if mn == M::Lar { ar } else { 0xffff_ffff })?;
                    }
                    None => self.set_flag(ZF, false),
                }
            }
            M::Verr | M::Verw => {
                let src = self.rd(m, ins, 0)? & 0xffff; // the selector is the r/m16 operand itself
                let ok = match Self::vdesc(src) {
                    Some((_, w)) => mn == M::Verr || w,
                    None => false,
                };
                self.set_flag(ZF, ok);
            }
            M::Sgdt | M::Sidt => {
                if ins.op0_kind() != OpKind::Memory {
                    return Err(ud(ins)); // the register form is not an instruction
                }
                // synthetic kernel tables at kernel-looking addresses
                let (limit, base) = if mn == M::Sgdt { (0x00ffu16, 0xc000_0000u32) } else { (0x07ffu16, 0xc000_1000u32) };
                let a = self.ea(ins)?;
                m.w16(a, limit)?;
                m.w32(a.wrapping_add(2), base)?;
            }
            M::Sldt | M::Smsw | M::Str => {
                // synthetic Linux/i386 userland view: no LDT, TSS selector 0x40, CR0 low = PE|MP|TS|ET|NE
                let v = match mn {
                    M::Str => 0x40u32,
                    M::Smsw => 0x3b,
                    _ => 0,
                };
                if ins.op0_kind() == OpKind::Register {
                    self.set_reg(ins.op0_register(), v); // 32-bit register destinations zero-extend (SDM)
                } else {
                    m.w16(self.ea(ins)?, v as u16)?;
                }
            }

            // ---- control flow ----
            M::Jmp => {
                self.eip = match ins.op0_kind() {
                    OpKind::NearBranch32 => ins.near_branch32(),
                    OpKind::Register | OpKind::Memory => self.rd(m, ins, 0)?,
                    _ => return Err(ud(ins)),
                };
            }
            M::Jo | M::Jno | M::Jb | M::Jae | M::Je | M::Jne | M::Jbe | M::Ja | M::Js | M::Jns | M::Jp | M::Jnp | M::Jl | M::Jge | M::Jle | M::Jg => {
                if self.cond(cc_of(mn)) {
                    self.eip = ins.near_branch32();
                }
            }
            M::Jecxz | M::Jcxz => {
                // 66 E3 in 32-bit mode tests CX and computes the target with a 16-bit IP.
                let n = if mn == M::Jcxz { self.r[ECX] & 0xffff } else { self.r[ECX] };
                if n == 0 {
                    self.eip = match ins.op0_kind() {
                        OpKind::NearBranch16 => ins.near_branch16() as u32,
                        _ => ins.near_branch32(),
                    };
                }
            }
            M::Loop | M::Loope | M::Loopne => {
                self.r[ECX] = self.r[ECX].wrapping_sub(1);
                let go = self.r[ECX] != 0
                    && match mn {
                        M::Loope => self.flag(ZF),
                        M::Loopne => !self.flag(ZF),
                        _ => true,
                    };
                if go {
                    self.eip = ins.near_branch32();
                }
            }
            M::Call => {
                let target = match ins.op0_kind() {
                    OpKind::NearBranch32 => ins.near_branch32(),
                    OpKind::Register | OpKind::Memory => self.rd(m, ins, 0)?,
                    _ => return Err(ud(ins)),
                };
                let ret = self.eip;
                self.push32(m, ret)?;
                self.eip = target;
            }
            M::Ret => {
                let t = self.pop32(m)?;
                if ins.op_count() == 1 {
                    self.r[ESP] = self.r[ESP].wrapping_add(ins.immediate16() as u32);
                }
                self.eip = t;
            }
            M::Leave => {
                self.r[ESP] = self.r[EBP];
                self.r[EBP] = self.pop32(m)?;
            }
            M::Enter => {
                let size = ins.immediate16() as u32;
                let level = ins.immediate8_2nd() & 31;
                self.push32(m, self.r[EBP])?;
                let frame = self.r[ESP];
                for _ in 1..level {
                    // copy the frame pointers of the enclosing procedures
                    self.r[EBP] = self.r[EBP].wrapping_sub(4);
                    let v = m.r32(self.r[EBP])?;
                    self.push32(m, v)?;
                }
                if level > 0 {
                    self.push32(m, frame)?;
                }
                self.r[EBP] = frame;
                self.r[ESP] = self.r[ESP].wrapping_sub(size);
            }

            // ---- misc ----
            M::Cdq => self.r[EDX] = ((self.r[EAX] as i32) >> 31) as u32,
            M::Cwde => self.r[EAX] = self.r[EAX] as u16 as i16 as i32 as u32,
            M::Cwd => {
                let s = if self.r[EAX] & 0x8000 != 0 { 0xffff } else { 0 };
                self.r[EDX] = (self.r[EDX] & 0xffff_0000) | s;
            }
            M::Cbw => {
                let v = self.r[EAX] as u8 as i8 as i16 as u16 as u32;
                self.r[EAX] = (self.r[EAX] & 0xffff_0000) | v;
            }
            M::Lahf => {
                let f = (self.fl & (SF | ZF | AF | PF | CF)) | 2;
                self.r[EAX] = (self.r[EAX] & !0xff00) | (f << 8);
            }
            M::Sahf => {
                let ah = (self.r[EAX] >> 8) & 0xff;
                let m5 = SF | ZF | AF | PF | CF;
                self.fl = (self.fl & !m5) | (ah & m5);
            }
            M::Clc => self.set_flag(CF, false),
            M::Stc => self.set_flag(CF, true),
            M::Cmc => self.fl ^= CF,
            M::Cld => self.set_flag(DF, false),
            M::Std => self.set_flag(DF, true),
            M::Xlatb => {
                let a = self.r[EBX].wrapping_add(self.r[EAX] & 0xff).wrapping_add(self.seg_base(ins));
                let v = m.r8(a)?;
                self.set_reg(Register::AL, v as u32);
            }
            M::Movsb | M::Movsw | M::Cmpsb | M::Cmpsw | M::Stosb | M::Stosw | M::Stosd | M::Lodsb | M::Lodsw | M::Lodsd | M::Scasb | M::Scasw | M::Scasd => self.string_op(m, ins)?,
            M::Movsd | M::Cmpsd if matches!(ins.op0_kind(), OpKind::MemoryESEDI | OpKind::MemorySegESI) => self.string_op(m, ins)?,
            M::Nop | M::Wait => {}
            M::Pause => return Err(Exit::Budget),
            M::Lfence | M::Sfence | M::Mfence | M::Prefetchnta | M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchw | M::Prefetch => {}
            M::Hlt => return Err(Exit::Halt),
            M::Ud2
            | M::Ud0
            | M::Ud1
            | M::Cli
            | M::Sti
            | M::In
            | M::Out
            | M::Xgetbv
            | M::Xsetbv
            | M::Sysexit
            | M::Lgdt
            | M::Lidt
            | M::Lldt
            | M::Ltr
            | M::Rdmsr
            | M::Wrmsr
            | M::Rdpmc
            | M::Clts
            | M::Invd
            | M::Wbinvd
            | M::Invlpg
            | M::Rsm
            | M::Monitor
            | M::Mwait => return Err(ud(ins)), // privileged / ring-0-only, or gated on a CPUID bit we don't advertise
            M::Int3 | M::Int1 => return Err(Exit::Breakpoint), // int1/icebp traps like int3 (#DB -> SIGTRAP)
            M::Int => {
                return Err(match ins.immediate8() {
                    0x80 => Exit::Syscall,
                    0x82 => Exit::Hyper,
                    n => Exit::Int(n),
                })
            }
            // `sysenter` is the fast syscall gate; SEP is not advertised by CPUID (glibc uses
            // `int 0x80`) but hand-written asm may use it anyway. Same Linux/i386 register
            // convention as `int 0x80` (nr in eax, args ebx/ecx/edx/esi/edi/ebp); the flat model
            // has no kernel entry address, so it traps like `int 0x80` and execution resumes at
            // the next instruction, which is where the kernel's `sysexit` would have landed.
            M::Sysenter => return Err(Exit::Syscall),
            M::Cpuid => self.cpuid(),
            M::Rdtsc => {
                self.tsc = self.tsc.wrapping_add(997);
                self.r[EAX] = self.tsc as u32;
                self.r[EDX] = (self.tsc >> 32) as u32;
            }
            _ => return self.exec_fp_sse(m, ins),
        }
        Ok(())
    }

    fn exec_fp_sse(&mut self, m: &mut Memory, ins: &Instruction) -> XResult<()> {
        let mn = ins.mnemonic() as u32;
        if mn >= M::F2xm1 as u32 && mn <= M::Fyl2xp1 as u32 && ins.mnemonic() != M::Femms {
            self.exec_x87(m, ins)
        } else {
            self.exec_sse(m, ins)
        }
    }
}

#[cfg(test)]
mod tests {
    // Instruction-level tests for the legacy / protected-mode corner of the ISA (BCD adjusts,
    // `enter` nesting, `bound`, far-pointer loads, segment queries, `sysenter`). The bulk behaviour
    // is additionally differential-tested against the i386 container (tests/progs/legacy_*.c).
    use super::*;
    use crate::mem::{P_MAPPED, P_R, P_W};
    use iced_x86::{Decoder, DecoderOptions};

    fn rig() -> (Cpu, Memory) {
        let mut mem = Memory::new(1 << 16);
        mem.set_prot(0, 1 << 16, P_R | P_W | P_MAPPED);
        (Cpu::new(), mem)
    }

    /// Decode and interpret `code` at 0x1000; returns the first exit, if any.
    fn run(c: &mut Cpu, m: &mut Memory, code: &[u8]) -> Option<Exit> {
        let mut d = Decoder::with_ip(32, code, 0x1000, DecoderOptions::NONE);
        while d.can_decode() {
            let ins = d.decode();
            c.eip = ins.next_ip() as u32;
            if let Err(e) = c.exec(m, &ins) {
                return Some(e);
            }
        }
        None
    }

    #[test]
    fn bcd_adjusts_match_silicon_vectors() {
        // DAA (SDM example): 45H + 38H = 7DH -> 83H, AF set, CF clear
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x7d;
        assert!(run(&mut c, &mut m, &[0x27]).is_none());
        assert_eq!(c.r[EAX] & 0xff, 0x83);
        assert!(c.flag(AF) && !c.flag(CF));

        // DAA with both adjustments (original AL > 0x99): AL=9AH -> 00H, CF=AF=ZF=1
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x9a;
        run(&mut c, &mut m, &[0x27]);
        assert_eq!(c.r[EAX] & 0xff, 0x00);
        assert!(c.flag(CF) && c.flag(AF) && c.flag(ZF) && !c.flag(SF));

        // DAA, CF=1 coming in: AL=12H -> 72H, CF stays 1
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x12;
        c.set_flag(CF, true);
        run(&mut c, &mut m, &[0x27]);
        assert_eq!(c.r[EAX] & 0xff, 0x72);
        assert!(c.flag(CF));

        // DAS: AL=DEH -> D8H (low) -> 78H (high), CF=AF=1
        let (mut c, mut m) = rig();
        c.r[EAX] = 0xde;
        run(&mut c, &mut m, &[0x2f]);
        assert_eq!(c.r[EAX] & 0xff, 0x78);
        assert!(c.flag(CF) && c.flag(AF));

        // DAS divergence from the SDM's sequential reading (oracle-verified): the high adjust
        // tests the ORIGINAL AL, so AL=00H with AF only lands on FAH, not 9AH.
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x00;
        c.set_flag(AF, true);
        run(&mut c, &mut m, &[0x2f]);
        assert_eq!(c.r[EAX] & 0xff, 0xfa);
        assert!(c.flag(CF) && c.flag(AF) && c.flag(SF));

        // DAS, CF=1 coming in with no low adjust: AL=05H -> A5H
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x05;
        c.set_flag(CF, true);
        run(&mut c, &mut m, &[0x2f]);
        assert_eq!(c.r[EAX] & 0xff, 0xa5);
        assert!(c.flag(CF));

        // AAA: AX=020CH -> 0302H, CF=AF=1; AX=0209H -> unchanged, CF=AF=0;
        // the carry out of AL+6 propagates into AH: AX=01FCH -> 0302H
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x020c;
        run(&mut c, &mut m, &[0x37]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0302);
        assert!(c.flag(CF) && c.flag(AF));
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x0209;
        run(&mut c, &mut m, &[0x37]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0209);
        assert!(!c.flag(CF) && !c.flag(AF));
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x01fc;
        run(&mut c, &mut m, &[0x37]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0302, "AL+6 carries into AH");

        // AAS: AX=050CH -> 0406H, CF=AF=1; the borrow out of AL-6 propagates: AX=0203H with AF -> 000DH
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x050c;
        run(&mut c, &mut m, &[0x3f]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0406);
        assert!(c.flag(CF) && c.flag(AF));
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x0203;
        c.set_flag(AF, true);
        run(&mut c, &mut m, &[0x3f]);
        assert_eq!(c.r[EAX] & 0xffff, 0x000d);

        // AAM base 10: AL=37H (55) -> AX=0505H; SZP from AL=05 (PF of 0b101 = 1); CF/AF cleared
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x37;
        c.set_flag(CF, true);
        c.set_flag(AF, true);
        run(&mut c, &mut m, &[0xd4, 0x0a]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0505);
        assert!(!c.flag(SF) && !c.flag(ZF) && c.flag(PF) && !c.flag(CF) && !c.flag(AF));

        // AAM base 0 -> #DE at the instruction
        let (mut c, mut m) = rig();
        assert_eq!(run(&mut c, &mut m, &[0xd4, 0x00]), Some(Exit::DivideError { eip: 0x1000 }));

        // AAM's imm8 is unsigned: base 255, AL=FF -> AH=1, AL=0. (qemu sign-extends the immediate
        // and returns AL unchanged here - a qemu decoder bug, which is why the C sweep stays < 0x80.)
        let (mut c, mut m) = rig();
        c.r[EAX] = 0xff;
        run(&mut c, &mut m, &[0xd4, 0xff]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0100);
        assert!(c.flag(ZF));
        // AAD base 255: AL = (2 * 255 + 0xFF) mod 256 = 0xFD, AH = 0
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x02ff;
        run(&mut c, &mut m, &[0xd5, 0xff]);
        assert_eq!(c.r[EAX] & 0xffff, 0x00fd);

        // AAD base 10: AX=0507H -> AL = 5*10+7 = 39H, AH=0
        let (mut c, mut m) = rig();
        c.r[EAX] = 0x0507;
        run(&mut c, &mut m, &[0xd5, 0x0a]);
        assert_eq!(c.r[EAX] & 0xffff, 0x0039);
        assert!(!c.flag(SF) && !c.flag(ZF) && c.flag(PF)); // PF of 0x39 (four bits) = 1
    }

    #[test]
    fn jcxz_tests_cx_and_wraps_at_16_bits() {
        // 67 E3 in 32-bit mode = JCXZ (address-size override). Per the SDM the target is computed
        // with a 16-bit IP; qemu-user deviates and branches to the 32-bit target, so the taken
        // case is pinned here (and never exercised by the container-oracle diff tests).
        let (mut c, mut m) = rig();
        c.r[ECX] = 0x0001_0000; // CX = 0 but ECX != 0 -> taken, (0x1003 - 4) & 0xffff = 0x0fff
        assert!(run(&mut c, &mut m, &[0x67, 0xe3, 0xfc]).is_none());
        assert_eq!(c.eip, 0x0fff);
        let (mut c, mut m) = rig();
        c.r[ECX] = 1; // CX != 0 -> not taken
        run(&mut c, &mut m, &[0x67, 0xe3, 0xfc]);
        assert_eq!(c.eip, 0x1003);
        // plain E3 = JECXZ tests the full ECX
        let (mut c, mut m) = rig();
        c.r[ECX] = 0x0001_0000;
        run(&mut c, &mut m, &[0xe3, 0xfc]);
        assert_eq!(c.eip, 0x1002);
    }

    #[test]
    fn enter_nesting_copies_frame_chain() {
        let (mut c, mut m) = rig();
        m.w32(0x1ffc, 0xaaaa_aaaa).unwrap(); // [ebp-4]
        m.w32(0x1ff8, 0xbbbb_bbbb).unwrap(); // [ebp-8]
        c.r[EBP] = 0x2000;
        c.r[ESP] = 0x4000;
        assert!(run(&mut c, &mut m, &[0xc8, 0x08, 0x00, 0x02]).is_none()); // enter 8, 2
        assert_eq!(m.r32(0x3ffc).unwrap(), 0x2000, "pushed old EBP");
        assert_eq!(m.r32(0x3ff8).unwrap(), 0xaaaa_aaaa, "pushed [EBP-4]");
        assert_eq!(m.r32(0x3ff4).unwrap(), 0x3ffc, "pushed FrameTemp");
        assert_eq!(c.r[EBP], 0x3ffc);
        assert_eq!(c.r[ESP], 0x3fec, "FrameTemp - 8");
        // level 0 keeps the old behaviour
        let (mut c, mut m) = rig();
        c.r[EBP] = 0x2000;
        c.r[ESP] = 0x4000;
        run(&mut c, &mut m, &[0xc8, 0x04, 0x00, 0x00]);
        assert_eq!(m.r32(0x3ffc).unwrap(), 0x2000);
        assert_eq!(c.r[EBP], 0x3ffc);
        assert_eq!(c.r[ESP], 0x3ff8);
    }

    #[test]
    fn bound_traps_outside_bounds() {
        // bound ecx, [eax] = 62 08; bounds at [0x3000] = {5, 10}
        let (mut c, mut m) = rig();
        m.w32(0x3000, 5).unwrap();
        m.w32(0x3004, 10).unwrap();
        c.r[EAX] = 0x3000;
        c.r[ECX] = 7;
        assert!(run(&mut c, &mut m, &[0x62, 0x08]).is_none(), "in range");
        for idx in [4u32, 11, 0xffff_ffff] {
            let (mut c, mut m) = rig();
            m.w32(0x3000, 5).unwrap();
            m.w32(0x3004, 10).unwrap();
            c.r[EAX] = 0x3000;
            c.r[ECX] = idx;
            assert_eq!(run(&mut c, &mut m, &[0x62, 0x08]), Some(Exit::Bounds { eip: 0x1000 }), "idx={:#x}", idx);
        }
        // signed compare: lo = -5, idx = -1 -> in range
        let (mut c, mut m) = rig();
        m.w32(0x3000, 0xffff_fffb).unwrap();
        m.w32(0x3004, 10).unwrap();
        c.r[EAX] = 0x3000;
        c.r[ECX] = 0xffff_ffff;
        assert!(run(&mut c, &mut m, &[0x62, 0x08]).is_none());
        // an unmapped bounds operand faults as SIGSEGV, not #BR
        let (mut c, mut m) = rig();
        c.r[EAX] = 0xf000_0000;
        assert!(matches!(run(&mut c, &mut m, &[0x62, 0x08]), Some(Exit::Segv(_))));
    }

    #[test]
    fn arpl_raises_rpl_and_sets_zf() {
        // arpl word [0x2000], bx = 63 1D disp32
        let (mut c, mut m) = rig();
        m.w16(0x2000, 0x0100).unwrap(); // RPL 0
        c.r[EBX] = 0x0023; // src RPL 3
        assert!(run(&mut c, &mut m, &[0x63, 0x1d, 0x00, 0x20, 0x00, 0x00]).is_none());
        assert_eq!(m.r16(0x2000).unwrap(), 0x0103);
        assert!(c.flag(ZF));
        // src RPL lower or equal -> unchanged, ZF=0
        let (mut c, mut m) = rig();
        m.w16(0x2000, 0x0103).unwrap();
        c.r[EBX] = 0x0020;
        run(&mut c, &mut m, &[0x63, 0x1d, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(m.r16(0x2000).unwrap(), 0x0103);
        assert!(!c.flag(ZF));
    }

    #[test]
    fn far_pointer_loads_set_offset_and_selector() {
        // lfs eax, [0x2000] = 0F B4 05 disp32
        let (mut c, mut m) = rig();
        m.w32(0x2000, 0x1234_5678).unwrap();
        m.w16(0x2004, 0x2b).unwrap();
        assert!(run(&mut c, &mut m, &[0x0f, 0xb4, 0x05, 0x00, 0x20, 0x00, 0x00]).is_none());
        assert_eq!(c.r[EAX], 0x1234_5678);
        assert_eq!(c.seg[4], 0x2b); // FS
        // lds ebx, [0x2000] = C5 1D disp32
        let (mut c, mut m) = rig();
        m.w32(0x2000, 0x9abc_def0).unwrap();
        m.w16(0x2004, 0x2b).unwrap();
        run(&mut c, &mut m, &[0xc5, 0x1d, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(c.r[EBX], 0x9abc_def0);
        assert_eq!(c.seg[3], 0x2b); // DS
        // lss eax, [0x2000] = 0F B2 05 disp32
        let (mut c, mut m) = rig();
        m.w32(0x2000, 0x1111).unwrap();
        m.w16(0x2004, 0x2b).unwrap();
        run(&mut c, &mut m, &[0x0f, 0xb2, 0x05, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(c.r[EAX], 0x1111);
        assert_eq!(c.seg[2], 0x2b); // SS
        // a faulting far-pointer load leaves the destination register untouched
        let (mut c, mut m) = rig();
        c.r[EAX] = 0xdead_beef;
        assert!(matches!(run(&mut c, &mut m, &[0x0f, 0xb4, 0x05, 0x00, 0x00, 0x00, 0x0f]), Some(Exit::Segv(_))));
        assert_eq!(c.r[EAX], 0xdead_beef);
    }

    #[test]
    fn segment_queries_use_the_flat_virtual_gdt() {
        // lar eax, bx = 0F 02 C3
        let (mut c, mut m) = rig();
        c.r[EBX] = 0x23;
        assert!(run(&mut c, &mut m, &[0x0f, 0x02, 0xc3]).is_none());
        assert_eq!(c.r[EAX], 0xc0fb00, "flat code: type R/A, S, DPL3, P, G, D/B");
        assert!(c.flag(ZF));
        c.r[EBX] = 0x2b;
        run(&mut c, &mut m, &[0x0f, 0x02, 0xc3]);
        assert_eq!(c.r[EAX], 0xc0f300, "flat data");
        // RPL0 spellings of the same descriptors are visible from CPL3 (DPL 3 >= RPL 0)
        c.r[EBX] = 0x28;
        run(&mut c, &mut m, &[0x0f, 0x02, 0xc3]);
        assert_eq!(c.r[EAX], 0xc0f300);
        // TLS slots 6..8 (0x33/0x3b/0x43) are valid data descriptors
        for sel in [0x33u32, 0x3b, 0x43] {
            c.r[EBX] = sel;
            run(&mut c, &mut m, &[0x0f, 0x02, 0xc3]);
            assert_eq!(c.r[EAX], 0xc0f300, "sel={:#x}", sel);
        }
        // null / kernel / LDT / unknown selectors: ZF=0, destination unchanged
        for bad in [0x0000u32, 0x0010, 0x1234, 0x002f] {
            let (mut c, mut m) = rig();
            c.r[EAX] = 0xdead;
            c.r[EBX] = bad;
            run(&mut c, &mut m, &[0x0f, 0x02, 0xc3]);
            assert_eq!(c.r[EAX], 0xdead, "sel={:#x}", bad);
            assert!(!c.flag(ZF));
        }
        // lsl eax, bx = 0F 03 C3: every flat descriptor has a 4 GB limit
        let (mut c, mut m) = rig();
        c.r[EBX] = 0x2b;
        run(&mut c, &mut m, &[0x0f, 0x03, 0xc3]);
        assert_eq!(c.r[EAX], 0xffff_ffff);
        assert!(c.flag(ZF));
        // verr bx = 0F 00 E3, verw bx = 0F 00 EB
        let (mut c, mut m) = rig();
        c.r[EBX] = 0x23; // code: readable, not writable
        run(&mut c, &mut m, &[0x0f, 0x00, 0xe3]);
        assert!(c.flag(ZF), "verr code");
        run(&mut c, &mut m, &[0x0f, 0x00, 0xeb]);
        assert!(!c.flag(ZF), "verw code");
        c.r[EBX] = 0x2b; // data: both
        run(&mut c, &mut m, &[0x0f, 0x00, 0xe3]);
        assert!(c.flag(ZF), "verr data");
        run(&mut c, &mut m, &[0x0f, 0x00, 0xeb]);
        assert!(c.flag(ZF), "verw data");
        c.r[EBX] = 0x10; // kernel: neither from CPL3
        run(&mut c, &mut m, &[0x0f, 0x00, 0xe3]);
        assert!(!c.flag(ZF));
    }

    #[test]
    fn descriptor_table_stores_are_synthetic() {
        // sgdt [0x2000] = 0F 01 05 disp32; sidt = 0F 01 0D disp32
        let (mut c, mut m) = rig();
        assert!(run(&mut c, &mut m, &[0x0f, 0x01, 0x05, 0x00, 0x20, 0x00, 0x00]).is_none());
        assert_eq!(m.r16(0x2000).unwrap(), 0x00ff);
        assert_eq!(m.r32(0x2002).unwrap(), 0xc000_0000);
        let (mut c, mut m) = rig();
        run(&mut c, &mut m, &[0x0f, 0x01, 0x0d, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(m.r16(0x2000).unwrap(), 0x07ff);
        assert_eq!(m.r32(0x2002).unwrap(), 0xc000_1000);
        // sldt/str/smsw to a 32-bit register zero-extend (SDM)
        let (mut c, mut m) = rig();
        for (code, want) in [([0x0f, 0x00, 0xc0], 0x00u32), ([0x0f, 0x00, 0xc8], 0x40), ([0x0f, 0x01, 0xe0], 0x3b)] {
            c.r[EAX] = 0xffff_ffff;
            run(&mut c, &mut m, &code);
            assert_eq!(c.r[EAX], want, "{:02x?}", code);
        }
        // memory destinations write 16 bits only: smsw word [0x2000] = 0F 01 25 disp32
        let (mut c, mut m) = rig();
        m.w32(0x2000, 0xffff_ffff).unwrap();
        run(&mut c, &mut m, &[0x0f, 0x01, 0x25, 0x00, 0x20, 0x00, 0x00]);
        assert_eq!(m.r16(0x2000).unwrap(), 0x003b);
        assert_eq!(m.r16(0x2002).unwrap(), 0xffff);
    }

    #[test]
    fn sysenter_traps_as_a_syscall_and_ring0_ops_are_ud() {
        let (mut c, mut m) = rig();
        c.r[EAX] = 20; // __NR_getpid (i386)
        assert_eq!(run(&mut c, &mut m, &[0x0f, 0x34]), Some(Exit::Syscall));
        assert_eq!(c.eip, 0x1002, "resumes after the sysenter");
        assert_eq!(c.r[EAX], 20, "number stays in eax");
        // ring-0-only instructions trap like #UD: sysexit, rdmsr, wrmsr
        for code in [[0x0fu8, 0x35], [0x0f, 0x32], [0x0f, 0x30]] {
            let (mut c, mut m) = rig();
            assert_eq!(run(&mut c, &mut m, &code), Some(Exit::Undef { eip: 0x1000 }), "{:02x?}", code);
        }
        // lgdt [0x2000] = 0F 01 15 disp32
        let (mut c, mut m) = rig();
        assert_eq!(run(&mut c, &mut m, &[0x0f, 0x01, 0x15, 0x00, 0x20, 0x00, 0x00]), Some(Exit::Undef { eip: 0x1000 }));
        // icebp (F1) traps like int3
        let (mut c, mut m) = rig();
        assert_eq!(run(&mut c, &mut m, &[0xf1]), Some(Exit::Breakpoint));
    }

    #[test]
    fn register_forms_of_memory_only_instructions_trap() {
        // lds eax, ebx / bound eax, ebx / sgdt eax are #UD on hardware; whatever iced makes of
        // them, they must trap instead of silently reading bogus addresses.
        for code in [&[0xc5u8, 0xc3][..], &[0x62, 0xc3][..], &[0x0f, 0x01, 0xc0][..]] {
            let (mut c, mut m) = rig();
            c.r[EBX] = 0x1234;
            c.r[EAX] = 0x5678;
            assert!(run(&mut c, &mut m, &code).is_some(), "{:02x?} must trap", code);
            assert_eq!(c.r[EBX], 0x1234);
            assert_eq!(c.r[EAX], 0x5678);
        }
    }
}
