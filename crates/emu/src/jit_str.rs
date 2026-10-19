//! Inline translation of MOVS/STOS (byte, word, dword; with or without REP/REPNE), the string instructions that
//! dominate memcpy/memset-style code.
//!
//! Semantics follow `Cpu::string_op`: the element is read (MOVS) and then written, only afterwards ESI/EDI are
//! advanced by +-size (DF) and ECX decremented, so a fault leaves exactly the state of the elements completed so far.
//! The per-element code uses the ordinary guest load/store sequences (page-table guard, slow helper, fault exit).
//! Forward REP MOVS / REP STOS over at most one page per operand additionally take a bulk path
//! (`memory.copy` / `memory.fill`) when every byte is known to be accessible, plain RAM (not a decoded-code page)
//! and, for MOVS, the ranges do not overlap in a way that would make a block copy differ from the
//! element-by-element copy. Anything else (segment overrides, 16-bit addressing, long counts, DF=1) uses the
//! per-element loop or the interpreter.

use super::*;
use iced_x86::Register;

const ESI: usize = 6;
const EDI: usize = 7;
const ECX: usize = 1;
const EAX: usize = 0;

impl<'a> Fx<'a> {
    /// `+-size` according to DF, in a temp local.
    fn str_step(&mut self, sz: u8) -> u32 {
        let t = self.tmp();
        self.c.i32c(-(sz as i32)).i32c(sz as i32);
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).i32c(crate::cpu::DF as i32).op(I32_AND);
        self.c.op(SELECT).lset(t);
        t
    }

    fn str_open_blk(&mut self) -> u32 {
        let l = self.new_label();
        self.c.block(BT_EMPTY);
        self.open(l);
        l
    }

    fn str_br_if(&mut self, l: u32) {
        let d = self.depth(l);
        self.c.br_if(d);
    }

    fn str_add_to(&mut self, reg: usize, v: u32) {
        self.c.lget(Self::r32(reg)).lget(v).op(I32_ADD);
        self.rset32(reg);
    }

    /// `br_if fail` unless the `len`-byte range at register `reg` lies inside one page whose table entry (`tab`) is set.
    fn str_page_ok_or(&mut self, reg: usize, len: u32, tab: u32, fail: u32) {
        let r = Self::r32(reg);
        self.c.lget(r).i32c(0xfff).op(I32_AND).lget(len).op(I32_ADD).i32c(0x1000).op(I32_GT_U);
        self.str_br_if(fail);
        self.c.lget(r).i32c(12).op(I32_SHR_U).mem(I32_LOAD8_U, tab).op(I32_EQZ);
        self.str_br_if(fail);
    }

    pub(super) fn str_ins(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let sz: u8 = match mn {
            M::Movsb | M::Stosb => 1,
            M::Movsw | M::Stosw => 2,
            _ => 4,
        };
        let is_movs = matches!(mn, M::Movsb | M::Movsw | M::Movsd);
        if ins.op0_kind() != OpKind::MemoryESEDI || (is_movs && ins.op1_kind() != OpKind::MemorySegESI) {
            return false;
        }
        if is_movs && matches!(ins.memory_segment(), Register::FS | Register::GS) {
            return false;
        }
        let rep = ins.has_rep_prefix() || ins.has_repe_prefix() || ins.has_repne_prefix();
        let step = self.str_step(sz);
        let (esi, edi, ecx) = (Self::r32(ESI), Self::r32(EDI), Self::r32(ECX));
        let one = |s: &mut Self| {
            if is_movs {
                s.load_mem(esi, sz, ip);
                let v = s.to_tmp();
                s.store_mem(edi, v, sz, ip);
                s.str_add_to(ESI, step);
            } else {
                s.store_mem(edi, V::L(Self::r32(EAX)), sz, ip);
            }
            s.str_add_to(EDI, step);
        };
        if !rep {
            one(self);
            return true;
        }
        let done = self.str_open_blk();
        self.c.lget(ecx).op(I32_EQZ);
        self.str_br_if(done);
        // ---- bulk path: forward, at most one page per operand ----
        let fail = self.str_open_blk();
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).i32c(crate::cpu::DF as i32).op(I32_AND);
        self.str_br_if(fail);
        self.c.lget(ecx).u32c(4096 / sz as u32).op(I32_GT_U);
        self.str_br_if(fail);
        let len = self.tmp();
        self.c.lget(ecx).i32c(sz as i32).op(I32_MUL).lset(len);
        let gb = self.lay.guest_base;
        if is_movs {
            self.str_page_ok_or(ESI, len, self.lay.rtab, fail);
            self.str_page_ok_or(EDI, len, self.lay.wtab, fail);
            // a block copy equals the element-by-element copy unless the destination starts inside the source range
            self.c.lget(edi).lget(esi).op(I32_GT_U);
            self.c.lget(edi).lget(esi).lget(len).op(I32_ADD).op(I32_LT_U).op(I32_AND);
            self.str_br_if(fail);
            self.c.lget(edi).u32c(gb).op(I32_ADD);
            self.c.lget(esi).u32c(gb).op(I32_ADD);
            self.c.lget(len).memory_copy();
            self.str_add_to(ESI, len);
        } else {
            self.str_page_ok_or(EDI, len, self.lay.wtab, fail);
            let eax = Self::r32(EAX);
            if sz == 1 {
                self.c.lget(edi).u32c(gb).op(I32_ADD).lget(eax).lget(len).memory_fill();
            } else {
                // all bytes of the pattern equal: memory.fill, otherwise a store loop over the (checked) page
                let (mask, rep_mul) = if sz == 4 { (0xffff_ffffu32, 0x0101_0101u32) } else { (0xffffu32, 0x0101u32) };
                self.c.lget(eax).i32c(0xff).op(I32_AND).u32c(rep_mul).op(I32_MUL).u32c(mask).op(I32_AND);
                self.c.lget(eax).u32c(mask).op(I32_AND).op(I32_EQ);
                self.if_(BT_EMPTY);
                self.c.lget(edi).u32c(gb).op(I32_ADD).lget(eax).lget(len).memory_fill();
                self.else_();
                let (p, n) = (self.tmp(), self.tmp());
                self.c.lget(edi).lset(p);
                self.c.lget(ecx).lset(n);
                let lp = self.new_label();
                self.c.loop_(BT_EMPTY);
                self.open(lp);
                self.c.lget(p).lget(eax).mem(if sz == 4 { I32_STORE } else { I32_STORE16 }, gb);
                self.c.lget(p).i32c(sz as i32).op(I32_ADD).lset(p);
                self.c.lget(n).i32c(1).op(I32_SUB).ltee(n);
                self.str_br_if(lp);
                self.close(); // loop
                self.close(); // if
            }
        }
        self.str_add_to(EDI, len);
        self.c.i32c(0);
        self.rset32(ECX);
        self.br(done);
        self.close(); // fail
        // ---- element by element ----
        let lp = self.new_label();
        self.c.loop_(BT_EMPTY);
        self.open(lp);
        one(self);
        self.c.lget(ecx).i32c(1).op(I32_SUB);
        self.rset32(ECX);
        self.c.lget(ecx);
        self.str_br_if(lp);
        self.close(); // loop
        self.close(); // done
        true
    }
}
