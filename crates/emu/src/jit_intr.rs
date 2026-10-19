//! Fast paths ("intrinsics") for a few hot functions of the Flash runtime, recognised by their exact code bytes.
//!
//! A Flash content workload used for profiling spent ~30 % of its time in one tiny function of libpepflashplayer, the open-addressing hash
//! lookup of the AVM2/MMgc hashtables (`find(ht, key, tab, cap)`). Compiled without optimisation it keeps its
//! locals on the stack, so every probe pays four checked guest memory accesses. This module replaces the translation of
//! its entry block by straight wasm that computes the same thing from temporaries.
//!
//! Equivalence contract (checked by `jit_tests::intrinsic_find_*` and `tests/progs/jit_intr.c`): after the fast path
//! the machine is bit-for-bit what the original code leaves behind — registers (including the clobbered ECX/EDX),
//! the lazy flags of the final `add esp,8`, ESP/EIP after the `ret`, and the dead stack frame bytes the original code
//! writes below ESP. Anything unusual (a guest memory check that fails, aliasing between the arguments and that dead
//! frame, a probe sequence longer than `MAX_PROBES`) leaves the region *before anything was modified*, with
//! `S_RETRY` at the function entry, so the interpreter runs the original code and produces exactly its faults.
//!
//! The functions are matched byte for byte (they are position independent), so the fast path is used with every
//! runtime build that contains the same code (26.0.0.151, 32.0.0.363 and 32.0.0.465 do) and never anywhere else.
//!
//! The second intrinsic is glibc's `bsearch` (Debian 2.36, i386) called with the runtime's range-test comparator
//! (`key` points at an address, each element is `{start, length}`): the MMgc region lookup is ~10 % of the run, half of
//! it the indirect call of the 36-byte comparator. The comparator is identified by its code bytes at every call.
//!
//! The same comparator is also recognised as a standalone leaf. Its fast path preserves the final CMP flags and
//! both pushed words, and reads the length only on the path that executes `add esi,[ecx+4]` in the original.
//! All 36 executable bytes are dependencies, including any tail outside the decoded entry block.

use super::*;

pub const INTR_NONE: u8 = 0;
pub const INTR_FIND: u8 = 1;
pub const INTR_BSEARCH: u8 = 2;
pub const INTR_RANGE_CMP: u8 = 3;
pub fn code_len(kind: u8) -> Option<u32> {
    Some(match kind { INTR_FIND => FIND_CODE.len(), INTR_BSEARCH => BSEARCH_CODE.len(), INTR_RANGE_CMP => RANGE_CMP_CODE.len(), _ => return None } as u32)
}

/// `find(HashTable *ht, uintptr_t key, uintptr_t *tab, uint32_t cap)`; cdecl, returns the slot index in EAX.
pub const FIND_CODE: [u8; 104] = [
    0x55, 0x89, 0xe5, 0x57, 0x56, 0x83, 0xec, 0x08, 0x8b, 0x45, 0x08, 0x8b, 0x30, 0x83, 0xe6, 0x01, 0xf7, 0xd6, 0x8b, 0x7d, 0x0c, 0x21, 0xf7, 0x8b, 0x45, 0x14, 0x83, 0xe8, 0x01, 0x83, 0xe0, 0xfe, 0x89, 0x45, 0xf4, 0x89, 0xf8,
    0x25, 0xf8, 0xff, 0xff, 0x7f, 0xc1, 0xf8, 0x02, 0x23, 0x45, 0xf4, 0x89, 0xf2, 0x8b, 0x4d, 0x10, 0x23, 0x14, 0x81, 0x39, 0xfa, 0x74, 0x25, 0xb9, 0x0e, 0x00, 0x00, 0x00, 0x85, 0xd2, 0x74, 0x1c, 0x89, 0x7d, 0xf0, 0x83, 0xc1,
    0x02, 0x01, 0xc8, 0x23, 0x45, 0xf4, 0x89, 0xf2, 0x8b, 0x7d, 0x10, 0x23, 0x14, 0x87, 0x3b, 0x55, 0xf0, 0x74, 0x04, 0x85, 0xd2, 0x75, 0xe7, 0x83, 0xc4, 0x08, 0x5e, 0x5f, 0x5d, 0xc3,
];

/// glibc 2.36 `bsearch(key, base, nmemb, size, cmp)` for i386 (Debian bookworm).
pub const BSEARCH_CODE: [u8; 94] = [
    0x55, 0x57, 0x56, 0x53, 0x83, 0xec, 0x0c, 0x8b, 0x7c, 0x24, 0x28, 0x85, 0xff, 0x74, 0x43, 0x8b, 0x6c, 0x24, 0x28, 0x31, 0xff, 0xeb, 0x12, 0x8d, 0xb4, 0x26, 0x00, 0x00, 0x00, 0x00, 0x66, 0x90, 0x74, 0x32, 0x8d, 0x7b, 0x01, 0x39, 0xef, 0x73,
    0x29, 0x8b, 0x74, 0x24, 0x2c, 0x8d, 0x1c, 0x2f, 0x83, 0xec, 0x08, 0xd1, 0xeb, 0x0f, 0xaf, 0xf3, 0x03, 0x74, 0x24, 0x2c, 0x56, 0xff, 0x74, 0x24, 0x2c, 0xff, 0x54, 0x24, 0x40, 0x83, 0xc4, 0x10, 0x85, 0xc0, 0x79, 0xd4, 0x89, 0xdd, 0x39, 0xef,
    0x72, 0xd7, 0x31, 0xf6, 0x83, 0xc4, 0x0c, 0x89, 0xf0, 0x5b, 0x5e, 0x5f, 0x5d, 0xc3,
];
/// The comparator the runtime passes to it: `*key < e[0] ? -1 : *key >= e[0] + e[1]` (a range test).
pub const RANGE_CMP_CODE: [u8; 36] = [
    0x55, 0x89, 0xe5, 0x56, 0x8b, 0x4d, 0x0c, 0x8b, 0x45, 0x08, 0x8b, 0x10, 0x8b, 0x31, 0xb8, 0xff, 0xff, 0xff, 0xff, 0x39, 0xf2, 0x72, 0x0a, 0x03, 0x71, 0x04, 0x31, 0xc0, 0x39, 0xf2, 0x0f, 0x93, 0xc0, 0x5e, 0x5d, 0xc3,
];
/// Offset of the instruction after `call dword [esp+0x40]` (the return address the comparator sees).
const BSEARCH_RET_OFF: u32 = 0x45;
/// Iterations after which the bsearch fast path gives up (a table of 2^32 entries needs 32).
const MAX_BSEARCH_ITERS: i32 = 64;

/// Probe iterations after which the fast path gives up (the interpreter then runs the function, which keeps the
/// guest preemptible if a corrupted table makes the original loop forever).
const MAX_PROBES: i32 = 4096;

/// Which intrinsic, if any, replaces the function that starts at `start` in guest memory.
pub fn detect(mem: &crate::mem::Memory, start: u32) -> u8 {
    let mut find = [0; FIND_CODE.len()];
    if mem.fetch(start, &mut find) == find.len() && find == FIND_CODE { return INTR_FIND; }
    let mut bsearch = [0; BSEARCH_CODE.len()];
    if mem.fetch(start, &mut bsearch) == bsearch.len() && bsearch == BSEARCH_CODE { return INTR_BSEARCH; }
    let mut code = [0; RANGE_CMP_CODE.len()];
    if mem.fetch(start, &mut code) == code.len() && code == RANGE_CMP_CODE {
        INTR_RANGE_CMP
    } else {
        INTR_NONE
    }
}

const EAX: usize = 0;
const ECX: usize = 1;
const EDX: usize = 2;
const EBX: usize = 3;
const ESP: usize = 4;
const EBP: usize = 5;
const ESI: usize = 6;
const EDI: usize = 7;

impl<'a> Fx<'a> {
    /// Standalone range comparator, using the same exact-byte pattern as the bsearch guard.
    /// All reads/checks precede the commit. Aliases of either pushed word retry, since
    /// the original pushes EBP/ESI before reading its arguments or pointed-to data.
    pub(super) fn intr_range_cmp(&mut self, entry: u32) {
        let gb = self.lay.guest_base;
        let (rtab, wtab) = (self.lay.rtab, self.lay.wtab);
        let esp = Self::r32(ESP);
        self.tmp_top = 0;
        self.ins_dirty = false;
        self.guarded = None;
        self.had_store = false;
        self.st = St::Dyn;
        let (lo, ret, keyp, elem, key, start, bound, result) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        self.c.lget(esp).i32c(8).op(I32_SUB).lset(lo);
        self.check_or_retry(esp, 12, rtab, entry);
        self.check_or_retry(lo, 8, wtab, entry);
        for (t, off) in [(ret, 0), (keyp, 4), (elem, 8)] {
            self.c.lget(esp).mem(I32_LOAD, gb + off).lset(t);
        }
        for a in [keyp, elem] {
            self.check_or_retry(a, 4, rtab, entry);
            self.intr_alias_retry_len(a, 4, lo, 8, entry);
        }
        self.c.lget(keyp).mem(I32_LOAD, gb).lset(key);
        self.c.lget(elem).mem(I32_LOAD, gb).ltee(start).lset(bound);
        self.c.i32c(-1).lset(result);
        self.c.lget(key).lget(start).op(I32_GE_U);
        self.if_(BT_EMPTY);
        // The length is not read at all on the below-start path: it may be unmapped.
        let lenp = self.tmp();
        self.c.lget(elem).i32c(4).op(I32_ADD).lset(lenp);
        self.check_or_retry(lenp, 4, rtab, entry);
        self.intr_alias_retry_len(lenp, 4, lo, 8, entry);
        self.c.lget(start).lget(lenp).mem(I32_LOAD, gb).op(I32_ADD).lset(bound);
        self.c.lget(key).lget(bound).op(I32_GE_U).lset(result);
        self.close();
        // Commit the two pushes, even though the epilogue restores both registers.
        for (off, src) in [(-4i32, Self::r32(EBP)), (-8, Self::r32(ESI))] {
            self.c.lget(esp).lget(src).mem(I32_STORE, gb.wrapping_add(off as u32));
        }
        self.c.lget(result);
        self.rset32(EAX);
        self.c.lget(elem);
        self.rset32(ECX);
        self.c.lget(key);
        self.rset32(EDX);
        // Both paths end in CMP EDX,ESI; SETAE/MOV/POP/RET preserve its flags.
        let diff = self.tmp();
        self.c.lget(key).lget(bound).op(I32_SUB).lset(diff);
        self.rec(lk(OP_SUB, 4), V::L(key), V::L(bound), V::L(diff));
        self.c.lget(esp).i32c(4).op(I32_ADD);
        self.rset32(ESP);
        self.push_v(V::L(ret));
        self.exit_class = 2;
        if self.ret_sites.is_empty() {
            self.exit_dyn(S_NEXT);
        } else {
            self.c.lset(L_NEXT);
            self.used_retm = true;
            self.br(LB_RETM);
        }
    }

    /// Retry (leave before the entry instruction) when the 4 bytes at local `a` overlap the dead frame `[lo, lo+20)`.
    fn intr_alias_retry(&mut self, a: u32, lo: u32, entry: u32) {
        self.intr_alias_retry_len(a, 4, lo, 20, entry);
    }

    /// Retry when `[a, a+alen)` overlaps the dead frame `[lo, lo+llen)`.
    fn intr_alias_retry_len(&mut self, a: u32, alen: i32, lo: u32, llen: i32, entry: u32) {
        self.c.lget(a).lget(lo).op(I32_SUB).i32c(alen - 1).op(I32_ADD).i32c(llen + alen - 1).op(I32_LT_U);
        self.if_(BT_EMPTY);
        self.retry_exit(entry);
        self.close();
    }

    /// One probe: `v = tag & tab[idx]`; the load is checked like any guest load and must not touch the dead frame.
    fn intr_probe(&mut self, tab: u32, idx: u32, tag: u32, lo: u32, v: u32, entry: u32) {
        let a = self.tmp();
        self.c.lget(tab).lget(idx).i32c(2).op(I32_SHL).op(I32_ADD).lset(a);
        self.check_or_retry(a, 4, self.lay.rtab, entry);
        self.intr_alias_retry(a, lo, entry);
        self.c.lget(a).mem(I32_LOAD, self.lay.guest_base).lget(tag).op(I32_AND).lset(v);
    }

    /// Emit the fast path of `find`; the chain starting at `entry` (the function entry) is replaced by it.
    pub(super) fn intr_find(&mut self, entry: u32) {
        let gb = self.lay.guest_base;
        let (rtab, wtab) = (self.lay.rtab, self.lay.wtab);
        let esp = Self::r32(ESP);
        self.tmp_top = 0;
        self.ins_dirty = false;
        self.guarded = None;
        self.had_store = false;
        self.st = St::Dyn;

        // ---- checks: nothing below modifies guest state until `commit` ----
        let lo = self.tmp(); // esp0 - 20: the dead frame the original pushes/spills use
        self.c.lget(esp).i32c(20).op(I32_SUB).lset(lo);
        self.check_or_retry(esp, 20, rtab, entry); // return address + the four arguments
        self.check_or_retry(lo, 20, wtab, entry); // dead frame (also refuses code pages)
        let (ret, ht, key, tab, cap) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        for (t, off) in [(ret, 0u32), (ht, 4), (key, 8), (tab, 12), (cap, 16)] {
            self.c.lget(esp).mem(I32_LOAD, gb + off).lset(t);
        }
        self.check_or_retry(ht, 4, rtab, entry);
        self.intr_alias_retry(ht, lo, entry);
        let (tag, k, mask, idx, v, ecx_f, step, iters, kflag) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        // tag = ~(*ht & 1); k = key & tag; mask = (cap - 1) & ~1; idx = ((k & 0x7ffffff8) >> 2) & mask
        self.c.lget(ht).mem(I32_LOAD, gb).i32c(1).op(I32_AND).i32c(-1).op(I32_XOR).lset(tag);
        self.c.lget(key).lget(tag).op(I32_AND).lset(k);
        self.c.lget(cap).i32c(1).op(I32_SUB).i32c(-2).op(I32_AND).lset(mask);
        self.c.lget(k).u32c(0x7fff_fff8).op(I32_AND).i32c(2).op(I32_SHR_U).lget(mask).op(I32_AND).lset(idx);
        self.c.i32c(0).lset(kflag).i32c(0).lset(iters);
        self.intr_probe(tab, idx, tag, lo, v, entry);

        // ---- search ----
        let done = self.new_label();
        self.c.block(BT_EMPTY);
        self.open(done);
        // first probe hit: ECX still holds the table pointer
        self.c.lget(v).lget(k).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.c.lget(tab).lset(ecx_f);
        self.br(done);
        self.close();
        // first probe empty: ECX = 0xe
        self.c.lget(v).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.c.i32c(0xe).lset(ecx_f);
        self.br(done);
        self.close();
        // quadratic probing: step 16, 18, 20, ... (the original also spills `k` to [ebp-0x10] here)
        self.c.i32c(1).lset(kflag).i32c(0xe).lset(step);
        let lp = self.new_label();
        self.c.loop_(BT_EMPTY);
        self.open(lp);
        self.c.lget(step).i32c(2).op(I32_ADD).lset(step);
        self.c.lget(idx).lget(step).op(I32_ADD).lget(mask).op(I32_AND).lset(idx);
        self.intr_probe(tab, idx, tag, lo, v, entry);
        self.c.lget(v).lget(k).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.c.lget(step).lset(ecx_f);
        self.br(done);
        self.close();
        self.c.lget(v).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.c.lget(step).lset(ecx_f);
        self.br(done);
        self.close();
        self.c.lget(iters).i32c(1).op(I32_ADD).ltee(iters).i32c(MAX_PROBES).op(I32_GT_S);
        self.if_(BT_EMPTY);
        self.retry_exit(entry);
        self.close();
        self.br(lp);
        self.close(); // loop
        self.close(); // done

        // ---- commit: every check passed, nothing can fail from here on ----
        // the dead frame the original leaves behind: pushed ebp/edi/esi, the spilled mask and (after the first probe) the key
        for (off, src) in [(-4i32, Self::r32(EBP)), (-8, Self::r32(EDI)), (-12, Self::r32(ESI)), (-16, mask)] {
            self.c.lget(esp).lget(src).mem(I32_STORE, gb.wrapping_add(off as u32));
        }
        self.c.lget(kflag);
        self.if_(BT_EMPTY);
        self.c.lget(esp).lget(k).mem(I32_STORE, gb.wrapping_sub(20));
        self.close();
        self.c.lget(idx);
        self.rset32(EAX);
        self.c.lget(ecx_f);
        self.rset32(ECX);
        self.c.lget(v);
        self.rset32(EDX);
        // flags of the original's last flag-writing instruction: `add esp, 8` with esp = esp0 - 20
        let r = self.tmp();
        self.c.lget(esp).i32c(-12).op(I32_ADD).lset(r);
        self.rec(lk(OP_ADD, 4), V::L(lo), V::C(8), V::L(r));
        // `ret`: ESP = esp0 + 4, continue at the return address
        self.c.lget(esp).i32c(4).op(I32_ADD);
        self.rset32(ESP);
        // charge the probes that were not translated one by one (7 instructions per loop iteration)
        self.c.lget(L_BUD).lget(iters).i32c(7).op(I32_MUL).op(I32_SUB).lset(L_BUD);
        self.push_v(V::L(ret));
        self.exit_class = 2;
        if self.ret_sites.is_empty() {
            self.exit_dyn(S_NEXT);
        } else {
            self.c.lset(L_NEXT);
            self.used_retm = true;
            self.br(LB_RETM);
        }
    }

    /// Emit the fast path of `bsearch`; the chain starting at `entry` (the function entry) is replaced by it.
    pub(super) fn intr_bsearch(&mut self, entry: u32) {
        let gb = self.lay.guest_base;
        let (rtab, wtab) = (self.lay.rtab, self.lay.wtab);
        let esp = Self::r32(ESP);
        self.tmp_top = 0;
        self.ins_dirty = false;
        self.guarded = None;
        self.had_store = false;
        self.st = St::Dyn;

        // ---- checks: nothing below modifies guest state until `commit` ----
        let lo = self.tmp(); // esp0 - 56: the dead frame (4 pushes, the call to the comparator and its frame)
        self.c.lget(esp).i32c(56).op(I32_SUB).lset(lo);
        self.check_or_retry(esp, 24, rtab, entry); // return address + the five arguments
        self.check_or_retry(lo, 56, wtab, entry); // dead frame (also refuses code pages)
        let (ret, keyp, base, nmemb, size, cmp) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        for (t, off) in [(ret, 0u32), (keyp, 4), (base, 8), (nmemb, 12), (size, 16), (cmp, 20)] {
            self.c.lget(esp).mem(I32_LOAD, gb + off).lset(t);
        }
        // the comparator must be the range test: its 36 code bytes are compared at every call. The original *executes* them, so
        // the page must also be executable: it has had instructions decoded from it (rtab bit 2) and the code does not straddle pages
        self.c.lget(cmp).i32c(12).op(I32_SHR_U).mem(I32_LOAD8_U, rtab).i32c(4).op(I32_AND).op(I32_EQZ);
        self.c.lget(cmp).i32c(0xfff).op(I32_AND).i32c(0x1000 - RANGE_CMP_CODE.len() as i32).op(I32_GT_U).op(I32_OR);
        self.if_(BT_EMPTY);
        self.retry_exit(entry);
        self.close();
        self.check_or_retry(cmp, 36, rtab, entry);
        self.intr_alias_retry_len(cmp, 36, lo, 56, entry);
        let w = |i: usize| u64::from_le_bytes(RANGE_CMP_CODE[8 * i..8 * i + 8].try_into().unwrap());
        for i in 0..4 {
            self.c.lget(cmp).mem(I64_LOAD, gb + 8 * i as u32).i64c(w(i) as i64).op(I64_XOR);
            if i > 0 {
                self.c.op(I64_OR);
            }
        }
        self.c.i64c(0).op(I64_NE);
        self.c.lget(cmp).mem(I32_LOAD, gb + 32).u32c(u32::from_le_bytes(RANGE_CMP_CODE[32..36].try_into().unwrap())).op(I32_NE).op(I32_OR);
        self.if_(BT_EMPTY);
        self.retry_exit(entry);
        self.close();

        // ---- search ----
        let (l, u, mid, elem, e0, k, iters, last_u, found) = (self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp(), self.tmp());
        let (ecx_f, edx_f) = (self.tmp(), self.tmp());
        self.c.i32c(0).lset(l).lget(nmemb).lset(u).i32c(0).lset(iters).i32c(0).lset(found).i32c(0).lset(elem).i32c(0).lset(last_u);
        self.c.lget(Self::r32(ECX)).lset(ecx_f).lget(Self::r32(EDX)).lset(edx_f);
        let done = self.new_label();
        self.c.block(BT_EMPTY);
        self.open(done);
        self.c.lget(nmemb).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.br(done);
        self.close();
        // the key is read once: nothing in the loop writes memory it could alias (the dead frame is excluded)
        self.check_or_retry(keyp, 4, rtab, entry);
        self.intr_alias_retry_len(keyp, 4, lo, 56, entry);
        self.c.lget(keyp).mem(I32_LOAD, gb).lset(k);
        let lp = self.new_label();
        self.c.loop_(BT_EMPTY);
        self.open(lp);
        // mid = (l + u) >> 1; elem = base + size * mid (32-bit wrapping, as the original's lea / shr / imul / add)
        self.c.lget(l).lget(u).op(I32_ADD).i32c(1).op(I32_SHR_U).lset(mid);
        self.c.lget(base).lget(size).lget(mid).op(I32_MUL).op(I32_ADD).lset(elem);
        self.check_or_retry(elem, 8, rtab, entry);
        self.intr_alias_retry_len(elem, 8, lo, 56, entry);
        self.c.lget(elem).mem(I32_LOAD, gb).lset(e0);
        self.c.lget(u).lset(last_u);
        self.c.lget(iters).i32c(1).op(I32_ADD).ltee(iters).i32c(MAX_BSEARCH_ITERS).op(I32_GT_S);
        self.if_(BT_EMPTY);
        self.retry_exit(entry);
        self.close();
        // res = -1 (key below the element), 1 (key past its end) or 0 (inside)
        self.c.lget(k).lget(e0).op(I32_LT_U);
        self.if_(BT_EMPTY);
        self.c.lget(mid).lset(u);
        self.else_();
        self.c.lget(k).lget(e0).lget(elem).mem(I32_LOAD, gb + 4).op(I32_ADD).op(I32_GE_U);
        self.if_(BT_EMPTY);
        self.c.lget(mid).i32c(1).op(I32_ADD).lset(l);
        self.else_();
        self.c.i32c(1).lset(found);
        self.br(done);
        self.close();
        self.close();
        self.c.lget(l).lget(u).op(I32_LT_U);
        self.if_(BT_EMPTY);
        self.br(lp);
        self.close();
        self.close(); // loop
        self.close(); // done
        // the comparator leaves the last element in ECX and the key value in EDX (untouched when the table is empty)
        self.c.lget(iters);
        self.if_(BT_EMPTY);
        self.c.lget(elem).lset(ecx_f).lget(k).lset(edx_f);
        self.close();

        // ---- commit: every check passed, nothing can fail from here on ----
        // the dead frame the original leaves behind: pushed ebp/edi/esi/ebx and, after the first comparison, the call frame
        for (off, src) in [(-4i32, Self::r32(EBP)), (-8, Self::r32(EDI)), (-12, Self::r32(ESI)), (-16, Self::r32(EBX))] {
            self.c.lget(esp).lget(src).mem(I32_STORE, gb.wrapping_add(off as u32));
        }
        self.c.lget(iters);
        self.if_(BT_EMPTY);
        for (off, src) in [(-40i32, elem), (-44, keyp), (-52, last_u), (-56, elem)] {
            self.c.lget(esp).lget(src).mem(I32_STORE, gb.wrapping_add(off as u32));
        }
        self.c.lget(esp).u32c(entry.wrapping_add(BSEARCH_RET_OFF)).mem(I32_STORE, gb.wrapping_sub(48));
        self.close();
        // eax = the element found or NULL
        self.c.lget(elem).i32c(0).lget(found).op(I32_SUB).op(I32_AND); // elem & -found
        self.rset32(EAX);
        self.c.lget(ecx_f);
        self.rset32(ECX);
        self.c.lget(edx_f);
        self.rset32(EDX);
        // flags of the original's last flag-writing instruction: `add esp, 12` with esp = esp0 - 28
        let (a1, r) = (self.tmp(), self.tmp());
        self.c.lget(esp).i32c(-28).op(I32_ADD).lset(a1);
        self.c.lget(esp).i32c(-16).op(I32_ADD).lset(r);
        self.rec(lk(OP_ADD, 4), V::L(a1), V::C(12), V::L(r));
        // `ret`: ESP = esp0 + 4, continue at the return address
        self.c.lget(esp).i32c(4).op(I32_ADD);
        self.rset32(ESP);
        // charge the iterations that were not translated one by one (about 30 instructions each)
        self.c.lget(L_BUD).lget(iters).i32c(30).op(I32_MUL).op(I32_SUB).lset(L_BUD);
        self.push_v(V::L(ret));
        self.exit_class = 2;
        if self.ret_sites.is_empty() {
            self.exit_dyn(S_NEXT);
        } else {
            self.c.lset(L_NEXT);
            self.used_retm = true;
            self.br(LB_RETM);
        }
    }
}
