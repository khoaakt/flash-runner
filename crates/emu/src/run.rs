//! Block cache + run loop. Blocks are straight-line runs of decoded instructions ending at a
//! control-transfer; decode results are cached by start address and dropped when a store hits a
//! page that has had code decoded from it (Flash's NanoJIT patches code in place).
//!
//! On wasm32 hot blocks are additionally translated into WebAssembly regions (see jit.rs / docs/JIT.md);
//! the interpreter below stays the tier-0 engine and the fallback for anything the translator declines.

use crate::cpu::*;
use crate::jit::{self, JitState};
use crate::jit_x86::{self, NodeIn};
use crate::mem::{Memory, PAGE_SHIFT, P_MAPPED, P_X};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic as M};
use std::cell::Cell;
use std::collections::HashMap;

pub struct Block {
    pub ins: Vec<Instruction>,
    pub start: u32,
    pub end: u32,
    /// Executions by the interpreter (tier-up counter).
    pub hot: Cell<u32>,
    /// Translation was attempted and failed: never try again for this block.
    pub nojit: Cell<bool>,
    /// The block ends in a direct `call` to a PIC thunk (`mov reg,[esp]; ret`, the i386 `__x86.get_pc_thunk.*`): register number + 1 (0 = no) and the thunk's address.
    /// The JIT turns such a call into "store the return address, reg = return address" (see `jit_x86::classify`).
    pub thunk: u8,
    pub thunk_target: u32,
}

#[derive(Default)]
pub struct BlockCache {
    blocks: HashMap<u32, std::rc::Rc<Block>>,
    /// page -> block start addresses touching that page
    by_page: HashMap<u32, Vec<u32>>,
    pub decoded: u64,
    pub jit: JitState,
    /// Explicit guest entry observations; empty in the ordinary runtime.
    pub entry_probes: HashMap<u32, (u32, Vec<u8>)>,
}

fn ends_block(mn: M) -> bool {
    matches!(
        mn,
        M::Jmp | M::Call | M::Ret | M::Int | M::Int3 | M::Hlt | M::Ud2 | M::Loop | M::Loope | M::Loopne | M::Jecxz | M::Jcxz | M::Jrcxz | M::Sysenter | M::Pause
            | M::Ja | M::Jae | M::Jb | M::Jbe | M::Je | M::Jg | M::Jge | M::Jl | M::Jle | M::Jne | M::Jno | M::Jnp | M::Jns | M::Jo | M::Jp | M::Js
    )
}

impl BlockCache {
    pub fn new() -> BlockCache {
        BlockCache::default()
    }

    /// Rebuild boundaries so no cached/merged region can run across a newly observed entry.
    pub fn set_entry_probe(&mut self, mem: &mut Memory, eip: u32, id: u32, expected: &[u8]) -> bool {
        if id != 0 && (expected.is_empty() || expected.len() > 256
            || eip.checked_add(expected.len() as u32).is_none()
            || mem.read_bytes(eip, expected.len() as u32).ok() != Some(expected)
            || ((eip >> PAGE_SHIFT)..=((eip + (expected.len() as u32 - 1)) >> PAGE_SHIFT))
                .any(|pg| mem.page_prot(pg << PAGE_SHIFT) & P_X == 0)) { return false; }
        self.flush_all(mem);
        if id == 0 { self.entry_probes.remove(&eip); }
        else { self.entry_probes.insert(eip, (id, expected.to_vec())); }
        true
    }

    pub fn flush_all(&mut self, mem: &mut Memory) {
        for (&pg, _) in self.by_page.iter() {
            mem.clear_code_flag(pg);
        }
        self.blocks.clear();
        self.by_page.clear();
        self.jit.flush_all();
    }

    /// Drop every block (and compiled region) overlapped by a byte range written since the last call.
    pub fn sync(&mut self, mem: &mut Memory) {
        let dirty: Vec<(u32, u32)> = std::mem::take(&mut mem.dirty_code);
        let mut touched: Vec<u32> = Vec::new();
        for (s, e) in dirty {
            for pg in (s >> PAGE_SHIFT)..=(e.wrapping_sub(1) >> PAGE_SHIFT) {
                touched.push(pg);
                if let Some(starts) = self.by_page.get_mut(&pg) {
                    let mut dropped = Vec::new();
                    starts.retain(|&st| {
                        let keep = match self.blocks.get(&st) {
                            Some(b) => b.end <= s || b.start >= e,
                            None => false, // already dropped through another page of the same range
                        };
                        if !keep {
                            dropped.push(st);
                        }
                        keep
                    });
                    for st in dropped {
                        self.blocks.remove(&st);
                    }
                    if starts.is_empty() {
                        self.by_page.remove(&pg);
                    }
                }
                self.jit.invalidate_range(pg, s, e, false);
            }
        }
        touched.sort_unstable();
        touched.dedup();
        // Protection changes do not modify bytes: park / unpark compiled regions instead of
        // recompiling them (Flash flips W^X around every nanojit emission).
        for (pg, x) in std::mem::take(&mut mem.prot_code) {
            touched.push(pg);
            if x {
                self.jit.unpark_page(pg, |p| mem.page_prot(p << PAGE_SHIFT) & P_X != 0);
                continue;
            }
            if mem.page_prot(pg << PAGE_SHIFT) & P_MAPPED != 0 {
                self.jit.park_page(pg);
            } else {
                // the page is gone (munmap / PROT_NONE remap): nothing can come back
                self.jit.invalidate_range(pg, pg << PAGE_SHIFT, (pg + 1) << PAGE_SHIFT, true);
            }
            if let Some(starts) = self.by_page.remove(&pg) {
                for st in starts {
                    self.blocks.remove(&st);
                }
            }
        }
        touched.sort_unstable();
        touched.dedup();
        for pg in touched {
            // keep the store-side code flag while anything decoded/compiled lives on the page (both
            // indexes are pruned as blocks/regions die); once the page holds no code at all, unflag
            // it so plain data stores stay cheap - the next decode re-flags it
            if !self.by_page.contains_key(&pg) && !self.jit.page_in_use(pg) {
                mem.clear_code_flag(pg);
            }
        }
    }

    pub(crate) fn decode(&mut self, mem: &mut Memory, start: u32) -> Result<std::rc::Rc<Block>, Exit> {
        let mut ins_vec = Vec::with_capacity(16);
        let mut ip = start;
        loop {
            if ip != start && self.entry_probes.contains_key(&ip) { break; }
            let mut buf = [0u8; 15];
            let n = mem.fetch(ip, &mut buf);
            if n == 0 {
                if ins_vec.is_empty() {
                    return Err(Exit::Segv(crate::mem::Segv { addr: ip, write: false }));
                }
                break;
            }
            let mut dec = Decoder::with_ip(32, &buf[..n], ip as u64, DecoderOptions::NONE);
            let mut i = Instruction::default();
            dec.decode_out(&mut i);
            if i.is_invalid() {
                if ins_vec.is_empty() {
                    return Err(Exit::Undef { eip: ip });
                }
                break;
            }
            let mn = i.mnemonic();
            ip = ip.wrapping_add(i.len() as u32);
            ins_vec.push(i);
            if ends_block(mn) || ins_vec.len() >= 64 {
                break;
            }
        }
        let end = ip;
        mem.mark_code(start, end.wrapping_sub(start).max(1));
        // a direct call whose target is exactly `mov reg,[esp]; ret`
        let (mut thunk, mut thunk_target) = (0u8, 0u32);
        if let Some(last) = ins_vec.last() {
            if last.mnemonic() == M::Call && last.op0_kind() == iced_x86::OpKind::NearBranch32 {
                let t = last.near_branch32();
                let mut b = [0u8; 15];
                if !(0..4).any(|offset| self.entry_probes.contains_key(&t.wrapping_add(offset))) && mem.fetch(t, &mut b) >= 4 && b[0] == 0x8b && (b[1] & 0xc7) == 0x04 && b[2] == 0x24 && b[3] == 0xc3 && (b[1] >> 3) & 7 != 4 {
                    thunk = ((b[1] >> 3) & 7) + 1;
                    thunk_target = t;
                    mem.mark_code(t, 4);
                }
            }
        }
        let blk = std::rc::Rc::new(Block { ins: ins_vec, start, end, hot: Cell::new(0), nojit: Cell::new(false), thunk, thunk_target });
        let first = start >> PAGE_SHIFT;
        let last = end.wrapping_sub(1) >> PAGE_SHIFT;
        for pg in first..=last {
            self.by_page.entry(pg).or_default().push(start);
        }
        if thunk != 0 && thunk_target >> PAGE_SHIFT != first && thunk_target >> PAGE_SHIFT != last {
            // the translation depends on the bytes of the thunk: a write to its page must drop this block
            self.by_page.entry(thunk_target >> PAGE_SHIFT).or_default().push(start);
        }
        self.blocks.insert(start, blk.clone());
        self.decoded += 1;
        Ok(blk)
    }

    fn get(&mut self, mem: &mut Memory, eip: u32) -> Result<std::rc::Rc<Block>, Exit> {
        if let Some(b) = self.blocks.get(&eip) {
            return Ok(b.clone());
        }
        self.decode(mem, eip)
    }

    /// Collect the hot region around `start` (already decoded successors reachable through direct
    /// edges; with threshold 1 successors are decoded eagerly) and hand it to the JIT.
    /// The decoded blocks reachable from the callee `a` (all of which already ran often enough or head a compiled region),
    /// or None if they add up to more than `cap` instructions.
    fn collect_callee(&self, a: u32, min_hot: u32, seen: &std::collections::HashSet<u32>, cap: usize) -> Option<Vec<std::rc::Rc<Block>>> {
        let mut out: Vec<std::rc::Rc<Block>> = Vec::new();
        let mut local = std::collections::HashSet::new();
        let mut stack = vec![a];
        let mut total = 0usize;
        while let Some(x) = stack.pop() {
            if self.entry_probes.contains_key(&x) { return None; }
            if seen.contains(&x) || !local.insert(x) {
                continue;
            }
            let Some(b) = self.blocks.get(&x) else { continue };
            if x != a && !self.jit.has_entry(x) && b.hot.get() < min_hot {
                continue;
            }
            total += b.ins.len();
            if total > cap {
                return None;
            }
            let node = NodeIn { start: b.start, end: b.end, ins: &b.ins, intrinsic: 0, thunk: b.thunk };
            let (taken, fall) = jit_x86::successors(&node);
            if let Some(f) = fall {
                stack.push(f);
            }
            if let Some(t) = taken {
                stack.push(t);
            }
            out.push(b.clone());
        }
        // the entry block must exist and be first
        if out.first().map(|b| b.start) != Some(a) {
            return None;
        }
        Some(out)
    }

    /// Hot chains plus the most executed region exits, with what each exit's target is (profiling builds).
    pub fn hot_report(&self, n: usize) -> String {
        let mut out = self.jit.hot_report(n);
        let cls = ["jump/fall", "call", "ret", "indirect", "other"];
        for (cnt, class, from, to) in self.jit.hot_exits(40) {
            let what = if to == 0 {
                String::new()
            } else if let Some(n) = self.jit.region_ins(to) {
                format!("-> compiled region of {} ins", n)
            } else {
                match self.blocks.get(&to) {
                    Some(b) => format!("-> not compiled: block len {} hot {}", b.ins.len(), b.hot.get()),
                    None => "-> not decoded".to_string(),
                }
            };
            out += &format!("[jit] exit {:>6}M {:<9} region @{:08x} to @{:08x} {}\n", cnt / 1_000_000, cls[(class as usize).min(4)], from, to, what);
        }
        out
    }

    pub fn compile_region(&mut self, mem: &mut Memory, start: u32) -> bool {
        if self.entry_probes.contains_key(&start) { return false; }
        let eager = self.jit.threshold <= 1;
        if !eager && self.jit.cooled(start) {
            return false; // SMC cooldown: interpret until the patch burst settles (docs/JIT.md)
        }
        // Only blocks that are themselves reasonably hot join the region: cold paths stay in the interpreter, which
        // keeps the amount of generated code (and V8's compile time for it) proportional to what really runs.
        let min_hot = if eager || self.jit.region_hot_div == 0 { 0 } else { (self.jit.threshold / self.jit.region_hot_div).max(2) };
        let mut order: Vec<std::rc::Rc<Block>> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![(start, false)];
        let mut total = 0usize;
        let mut dup_total = 0usize;
        while let Some((a, via_call)) = stack.pop() {
            if self.entry_probes.contains_key(&a) { continue; }
            if order.len() >= jit::MAX_NODES || total >= jit::MAX_INS {
                break;
            }
            if seen.contains(&a) {
                continue;
            }
            if a != start && self.jit.region_hot_div != 0 && self.jit.has_entry(a) {
                // Already compiled as part of another region. A small callee is copied in so that the call and its `ret`
                // do not leave this wasm function; anything else is left through the chaining table.
                // (a function that has an intrinsic fast path is never copied: the copy would run the generic translation of it)
                if (via_call || self.jit.dup_ins & jit::DUP_JUMPS != 0) && self.jit.dup_ins & 0xffff != 0 && !self.jit.is_lir_entry(a) && !(self.jit.intrinsics && jit_x86::intr_detect(mem, a) != 0) {
                    // Offline blocks have not necessarily visited the decoder. Let the normal
                    // hot-caller tier inline tiny AOT/PIC/PLT thunks too, rather than permanently
                    // adding a dispatcher round-trip to every shared-library call.
                    // A fused AOT region's total size says nothing about this
                    // entry's callee size (e.g. a two-instruction PIC/PLT leaf).
                    // Decode the entry once; collect_callee enforces the actual
                    // instruction cap before anything is copied into the caller.
                    if self.jit.is_aot_entry(a) && !self.blocks.contains_key(&a) {
                        let _ = self.decode(mem, a);
                    }
                    if let Some(copy) = self.collect_callee(a, min_hot, &seen, (self.jit.dup_ins & 0xffff) as usize) {
                        if order.len() + copy.len() <= jit::MAX_NODES && total + copy.iter().map(|b| b.ins.len()).sum::<usize>() <= jit::MAX_INS {
                            for blk in copy {
                                seen.insert(blk.start);
                                let node = NodeIn { start: blk.start, end: blk.end, ins: &blk.ins, intrinsic: 0, thunk: blk.thunk };
                                let (taken, fall) = jit_x86::successors(&node);
                                total += blk.ins.len();
                                dup_total += blk.ins.len();
                                if let Some(f) = fall {
                                    stack.push((f, false));
                                }
                                if let Some(t) = taken {
                                    stack.push((t, jit_x86::ends_in_call(&node)));
                                }
                                order.push(blk);
                            }
                            continue;
                        }
                    }
                }
                seen.insert(a);
                continue;
            }
            seen.insert(a);
            let blk = match self.blocks.get(&a) {
                Some(b) => {
                    if a != start && b.hot.get() < min_hot {
                        continue;
                    }
                    b.clone()
                }
                None if eager || a == start => match self.decode(mem, a) {
                    Ok(b) => b,
                    Err(_) => continue,
                },
                None => continue,
            };
            let node = NodeIn { start: blk.start, end: blk.end, ins: &blk.ins, intrinsic: 0, thunk: blk.thunk };
            let (taken, fall) = jit_x86::successors(&node);
            let is_call = jit_x86::ends_in_call(&node);
            total += blk.ins.len();
            // push the fallthrough first so the taken edge is explored first
            if let Some(f) = fall {
                stack.push((f, false));
            }
            if let Some(t) = taken {
                stack.push((t, is_call));
            }
            order.push(blk);
        }
        self.jit.stats.dup_ins += dup_total as u64;
        if order.is_empty() {
            return false;
        }
        if self.jit.profile {
            let desc: Vec<String> = order.iter().map(|b| format!("{:x}+{}:h{}", b.start, b.ins.len(), b.hot.get())).collect();
            let skipped: Vec<String> = seen
                .iter()
                .filter(|a| !order.iter().any(|b| b.start == **a))
                .map(|a| format!("{:x}:{}", a, if self.jit.has_entry(*a) { "compiled".to_string() } else { self.blocks.get(a).map(|b| format!("h{}", b.hot.get())).unwrap_or("undecoded".to_string()) }))
                .collect();
            crate::log::log(&format!("[jit] region @{:x}: {} | skipped {}", start, desc.join(" "), skipped.join(" ")));
        }
        if self.jit.intrinsics {
            for b in &order {
                if let Some(len) = jit_x86::intr::code_len(jit_x86::intr_detect(mem, b.start)) {
                    // All assumed bytes, not only the decoded entry block, must
                    // report stores and protection changes to invalidation.
                    mem.mark_code(b.start, len);
                }
            }
        }
        self.jit.compile(mem, &order)
    }
}

impl Cpu {
    fn entry_probe(&mut self, mem: &Memory, cache: &BlockCache) -> Option<Exit> {
        let resume = std::mem::take(&mut self.jit_probe_resume);
        if resume == ((1u64 << 32) | self.eip as u64) { self.jit_probe_resume = resume; return None; }
        let (id, expected) = cache.entry_probes.get(&self.eip)?;
        let last = self.eip.checked_add(expected.len() as u32 - 1)?;
        if ((self.eip >> PAGE_SHIFT)..=(last >> PAGE_SHIFT)).any(|pg| mem.page_prot(pg << PAGE_SHIFT) & P_X == 0)
            || mem.read_bytes(self.eip, expected.len() as u32).ok() != Some(expected.as_slice()) { return None; }
        self.jit_probe_resume = (1u64 << 32) | self.eip as u64;
        Some(Exit::Probe { id: *id, eip: self.eip })
    }
    /// Run until an event the kernel must handle. `budget` counts instructions.
    pub fn run(&mut self, mem: &mut Memory, cache: &mut BlockCache, budget: u64) -> Exit {
        if cache.jit.threshold > 0 && cfg!(any(target_arch = "wasm32", test)) {
            jit::set_ctx(mem, &mut cache.jit);
            self.jit_budget = budget.min(i32::MAX as u64) as i32;
            let b0 = self.jit_budget;
            let r = self.run_mixed(mem, cache);
            cache.jit.stats.ins_total += (b0 - self.jit_budget).max(0) as u64;
            self.lf_flush();
            return r;
        }
        self.run_interp(mem, cache, budget)
    }

    /// Tier 0 + tier 1: interpret cold blocks, count executions, translate hot ones, run compiled regions.
    fn run_mixed(&mut self, mem: &mut Memory, cache: &mut BlockCache) -> Exit {
        use crate::jit_x86::*;
        let mut after_region = false;
        // set when a region gave up on an access: that block is interpreted once, whatever is compiled at its address
        let mut force_interp = false;
        loop {
            if self.jit_budget <= 0 {
                return Exit::Budget;
            }
            if mem.needs_sync() {
                cache.sync(mem);
            }
            if !force_interp && !cache.entry_probes.is_empty() {
                if let Some(event) = self.entry_probe(mem, cache) { return event; }
            }
            let forced = std::mem::replace(&mut force_interp, false);
            if let Some(ent) = if forced { None } else { cache.jit.lookup(self.eip) } {
                if cache.jit.should_promote_aot(ent.region) {
                    cache.jit.promote_aot(ent.region);
                    let threshold = cache.jit.threshold;
                    if let Ok(block) = cache.get(mem, self.eip) { block.hot.set(threshold); }
                    continue;
                }
                cache.jit.note_dispatch(ent.region);
                cache.jit.stats.lookup_hit += 1;
                if after_region {
                    cache.jit.stats.ret_hit += 1;
                }
                after_region = true;
                let t0 = if cache.jit.profile { jit::rt::now_ns() } else { 0.0 };
                let st = unsafe { jit::rt::call(ent.slot, self as *mut Cpu, ent.head) };
                // Preserve observation acknowledgment across compile/retry redispatch,
                // but clear it once native progress or a non-retry event is observed.
                if !matches!(st, S_RETRY | S_PAUSE) || self.eip != (self.jit_probe_resume as u32) { self.jit_probe_resume = 0; }
                if cache.jit.profile {
                    cache.jit.stats.t_region_ns += jit::rt::now_ns() - t0;
                }
                match st {
                    S_NEXT | S_SYNC => {
                        cache.jit.stats.ret_next += 1;
                        continue;
                    }
                    S_RETRY => {
                        cache.jit.blame_retry(ent.region, self.eip);
                        force_interp = true;
                        continue;
                    }
                    S_EXIT => return cache.jit.pending.take().unwrap_or(Exit::Undef { eip: self.eip }),
                    S_SEGV => {
                        self.jit_fault = 0;
                        return Exit::Segv(crate::mem::Segv { addr: self.jit_fault_addr, write: self.jit_fault_write != 0 });
                    }
                    S_DIVERR => return Exit::DivideError { eip: self.eip },
                    S_SYSCALL => return Exit::Syscall,
                    S_HYPER => return Exit::Hyper,
                    S_PAUSE => return Exit::Budget,
                    _ => return Exit::Undef { eip: self.eip },
                }
            }
            if !forced {
                cache.jit.stats.lookup_miss += 1;
                if after_region {
                    cache.jit.stats.ret_cold += 1;
                }
            }
            after_region = false;
            let blk = match cache.get(mem, self.eip) {
                Ok(b) => b,
                Err(e) => return e,
            };
            if !blk.nojit.get() && !forced {
                let hot = blk.hot.get().saturating_add(1);
                blk.hot.set(hot);
                if hot >= cache.jit.threshold {
                    if cache.compile_region(mem, self.eip) && cache.jit.lookup(self.eip).is_some() {
                        continue; // dispatch into the new region
                    }
                    blk.nojit.set(true);
                }
            }
            cache.jit.stats.interp_blocks += 1;
            cache.jit.stats.interp_ins += blk.ins.len() as u64;
            let t_i = if cache.jit.profile { jit::rt::now_ns() } else { 0.0 };
            for ins in blk.ins.iter() {
                self.jit_probe_resume = 0;
                self.eip = ins.next_ip() as u32;
                if let Err(e) = self.exec(mem, ins) {
                    if matches!(e, Exit::Segv(_) | Exit::Undef { .. } | Exit::Unimpl { .. } | Exit::DivideError { .. } | Exit::Bounds { .. }) {
                        self.eip = ins.ip() as u32;
                    }
                    return e;
                }
                if mem.needs_sync() {
                    cache.sync(mem);
                    break;
                }
                if self.eip != ins.next_ip() as u32 {
                    break;
                }
            }
            if cache.jit.profile {
                cache.jit.stats.t_interp_ns += jit::rt::now_ns() - t_i;
            }
            self.jit_budget = self.jit_budget.saturating_sub(blk.ins.len() as i32);
        }
    }

    /// The plain interpreter loop (no JIT).
    fn run_interp(&mut self, mem: &mut Memory, cache: &mut BlockCache, mut budget: u64) -> Exit {
        loop {
            if mem.needs_sync() {
                cache.sync(mem);
            }
            if !cache.entry_probes.is_empty() {
                if let Some(event) = self.entry_probe(mem, cache) { return event; }
            }
            let blk = match cache.get(mem, self.eip) {
                Ok(b) => b,
                Err(e) => return e,
            };
            for ins in blk.ins.iter() {
                self.jit_probe_resume = 0;
                self.eip = ins.next_ip() as u32;
                if let Err(e) = self.exec(mem, ins) {
                    if matches!(e, Exit::Segv(_) | Exit::Undef { .. } | Exit::Unimpl { .. } | Exit::DivideError { .. } | Exit::Bounds { .. }) {
                        self.eip = ins.ip() as u32;
                    }
                    return e;
                }
                if mem.needs_sync() {
                    // Store hit decoded code: the rest of this block may be stale.
                    cache.sync(mem);
                    break;
                }
                if self.eip != ins.next_ip() as u32 {
                    break; // control transfer (taken branch / call / ret)
                }
            }
            let n = blk.ins.len() as u64;
            if budget <= n {
                return Exit::Budget;
            }
            budget -= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{P_MAPPED, P_R, P_W, P_X};

    const CODE: u32 = 0x2000;

    fn setup() -> (Memory, BlockCache, Cpu) {
        let mut m = Memory::new(16 * 4096);
        m.set_prot(0, 16 * 4096, P_MAPPED | P_R | P_W | P_X);
        let mut c = Cpu::new();
        c.r[ESP] = 0xf000;
        (m, BlockCache::new(), c)
    }

    fn mov_eax_hyper(v: u8) -> [u8; 7] {
        [0xb8, v, 0, 0, 0, 0xcd, 0x82]
    }

    fn run_at(cpu: &mut Cpu, m: &mut Memory, bc: &mut BlockCache, eip: u32) -> Exit {
        cpu.eip = eip;
        cpu.run(m, bc, 1000)
    }

    #[test]
    fn multi_page_host_write_invalidates_middle_page() {
        let (mut m, mut bc, mut cpu) = setup();
        m.write_bytes(CODE, &mov_eax_hyper(123)).unwrap();
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 123);
        // A 12 KiB "read()" destination spanning pages 1..=3 that overwrites the code on page 2.
        let mut buf = vec![0x90u8; 3 * 4096 - 0x20];
        buf[(CODE - 0x1010) as usize..][..7].copy_from_slice(&mov_eax_hyper(45));
        m.write_bytes(0x1010, &buf).unwrap();
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 45, "stale decoded block survived a multi-page write");
    }

    #[test]
    fn cached_block_does_not_survive_protection_change() {
        for newprot in [0u8, P_MAPPED, P_MAPPED | P_R | P_W] {
            let (mut m, mut bc, mut cpu) = setup();
            m.write_bytes(CODE, &mov_eax_hyper(1)).unwrap();
            assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
            m.set_prot(CODE, 4096, newprot); // mprotect(PROT_NONE / RW) or munmap
            match run_at(&mut cpu, &mut m, &mut bc, CODE) {
                Exit::Segv(s) => assert_eq!((s.addr, s.write), (CODE, false)),
                e => panic!("expected a fetch fault after mprotect({:#x}), got {:?}", newprot, e),
            }
            // restoring X lets the code run again (re-decoded)
            m.set_prot(CODE, 4096, P_MAPPED | P_R | P_W | P_X);
            assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
        }
    }

    #[test]
    fn writes_only_drop_the_blocks_and_regions_they_overlap() {
        // two blocks on one code page, a store into the gap between them: both must survive;
        // a store onto the first block's bytes drops exactly that one (SMC at write granularity,
        // not page granularity - Flash's AVM2 JIT emits into pages full of live code).
        let (mut m, mut bc, mut cpu) = setup();
        // block A: mov eax,1 ; int 0x82   at CODE; block B: mov eax,2 ; int 0x82  at CODE+16
        let a = [0xb8u8, 1, 0, 0, 0, 0xcd, 0x82];
        let b = [0xb8u8, 2, 0, 0, 0, 0xcd, 0x82];
        m.write_bytes(CODE, &a).unwrap();
        m.write_bytes(CODE + 16, &b).unwrap();
        m.mark_code(CODE, 32);
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 1);
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE + 16), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 2);
        assert!(bc.blocks.contains_key(&CODE) && bc.blocks.contains_key(&(CODE + 16)));
        // a store into the gap: same page, no overlap with either block
        m.w32(CODE + 8, 0xdead_beef).unwrap();
        bc.sync(&mut m);
        assert!(bc.blocks.contains_key(&CODE), "gap write must not drop block A");
        assert!(bc.blocks.contains_key(&(CODE + 16)), "gap write must not drop block B");
        // a store onto block A's opcode bytes drops only block A
        m.w32(CODE, 0x90909090).unwrap();
        bc.sync(&mut m);
        assert!(!bc.blocks.contains_key(&CODE), "overwriting block A must drop it");
        assert!(bc.blocks.contains_key(&(CODE + 16)), "block B survives");
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE + 16), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 2);
    }

    #[test]
    fn remap_with_new_code_runs_new_code() {
        // munmap + MAP_FIXED mmap of the same page with different code: zero() + set_prot() + write
        let (mut m, mut bc, mut cpu) = setup();
        m.write_bytes(CODE, &mov_eax_hyper(7)).unwrap();
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
        m.set_prot(CODE, 4096, 0);
        m.zero(CODE, 4096);
        m.set_prot(CODE, 4096, P_MAPPED | P_R | P_W | P_X);
        m.write_bytes(CODE, &mov_eax_hyper(8)).unwrap();
        assert_eq!(run_at(&mut cpu, &mut m, &mut bc, CODE), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 8);
    }
}
