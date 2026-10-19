//! JIT runtime: region registry, installation of translated modules, invalidation and the helper
//! functions compiled code calls back into. The x86 -> wasm translation itself is in `jit_x86.rs`
//! (target independent); only the install/call glue at the bottom is `wasm32`-specific.

use crate::cpu::*;
use crate::jit_flags;
use crate::jit_x86::*;
use crate::mem::{Memory, PAGE_SHIFT};
use crate::run::Block;
use iced_x86::{Instruction, Mnemonic};
use std::collections::HashMap;
use std::rc::Rc;

// =====================================================================================
// Helpers called from compiled code. The `_core` functions work on explicit arguments so the native
// test harness can call them; the wasm32 `extern "C"` wrappers at the bottom find the live objects
// through `CTX`.
// =====================================================================================

/// FISTP m64 (fist=1) / FILD m64 (fist=0) from compiled code: the shared soft-float cores
/// (`Cpu::fist_core` / `fild_core`, the same code the interpreter runs) on the live Cpu image -
/// no register/tuple sync, no decode dispatch. Returns like `h_store_slow`: 0 done, 1 guest fault
/// (jit_fault/jit_aux set), 2 done but the store hit decoded code (S_SYNC at the end of the ins).
pub fn h_fist_fild(cpu: &mut Cpu, mem: &mut Memory, addr: u32, ip: u32, fist: u32) -> u32 {
    cpu.fpu.sw &= !0x0200; // fbegin: clear C1
    cpu.fpu.last_ip = ip;
    cpu.fpu.last_dp = addr; // fea
    let r = if fist != 0 {
        let rc = cpu.fpu.rc();
        cpu.fist_core(mem, addr, rc, 8, true)
    } else {
        cpu.fild_core(mem, addr, 8)
    };
    match r {
        Ok(()) => {
            if fist != 0 && mem.needs_sync() {
                2
            } else {
                0
            }
        }
        Err(Exit::Segv(s)) => {
            cpu.jit_fault = 1;
            cpu.jit_fault_addr = s.addr;
            cpu.jit_fault_write = s.write as u32;
            cpu.jit_aux = ip;
            1
        }
        Err(e) => unreachable!("integer x87 memory core returned {e:?}"),
    }
}

/// FCOMIP/FUCOMIP st0, st(i): bisp = i | signal<<3 | pop<<4. Writes ZF/PF/CF straight into `fl`.
pub fn h_fcomip(cpu: &mut Cpu, bisp: u32, ip: u32) -> u32 {
    cpu.fpu.sw &= !0x0200;
    cpu.fpu.last_ip = ip;
    cpu.fcomi_core((bisp & 7) as usize, bisp & 8 != 0, bisp & 16 != 0);
    0
}

/// CVTSS2SI r32, xmm/m32: bit-exact soft-float conversion with MXCSR rounding and sticky flags
/// (the interpreter's `cvt_s2si`). kind 0 = xmm index, 1 = m32 address. Returns like `h_load_slow`.
pub fn h_cvtss2si(cpu: &mut Cpu, mem: &mut Memory, kind: u32, v: u32, dst: u32, ip: u32) -> u32 {
    let bits: u32 = match kind {
        0 => cpu.xmm[v as usize] as u32,
        _ => match mem.r32(v) {
            Ok(x) => x,
            Err(s) => {
                cpu.jit_fault = 1;
                cpu.jit_fault_addr = s.addr;
                cpu.jit_fault_write = 0;
                cpu.jit_aux = ip;
                return 1;
            }
        },
    };
    let mut env = crate::sse_fp::Env::new(cpu.mxcsr);
    let r = crate::sse_fp::f2i::<crate::sse_fp::F32>(bits as u64, false, &mut env);
    cpu.mxcsr |= env.fl;
    cpu.r[dst as usize] = r;
    0
}

/// MOVQ (op=0) / PXOR (op=1) with an MMX destination, including the `mm_enter` x87-state transition
/// that used to send these to the interpreter whenever the state was not MMX-clean (the dominant
/// exec_one family in rasterizer-heavy guests). opdst = op | dst<<8; kind: 0 = mm src (v = index),
/// 1 = xmm src (v = index), 2 = m64 (v = address). Returns like `h_load_slow`.
pub fn h_mm_op(cpu: &mut Cpu, mem: &mut Memory, opdst: u32, kind: u32, v: u32, ip: u32) -> u32 {
    let (op, dst) = (opdst & 0xff, (opdst >> 8) & 7);
    let src64: u64 = match kind {
        0 => cpu.fpu.st[v as usize].mant,
        1 => cpu.xmm[v as usize] as u64,
        _ => match mem.r64(v) {
            Ok(x) => x,
            Err(s) => {
                cpu.jit_fault = 1;
                cpu.jit_fault_addr = s.addr;
                cpu.jit_fault_write = 0;
                cpu.jit_aux = ip;
                return 1;
            }
        },
    };
    cpu.mm_enter();
    let cur = cpu.fpu.st[dst as usize].mant;
    let nv = if op == 0 { src64 } else { cur ^ src64 };
    cpu.mm_set(dst as usize, nv);
    0
}

/// `exec_one`: run one fallback instruction in the interpreter.
/// Returns 0 to continue inline, else `1 + status` meaning "leave the region now, state is in `Cpu`".
pub fn h_exec_one(cpu: &mut Cpu, mem: &mut Memory, ins: &Instruction, pending: &mut Option<Exit>) -> u32 {
    cpu.eip = ins.next_ip() as u32;
    match cpu.exec(mem, ins) {
        Ok(()) => {
            if mem.needs_sync() {
                1 + S_NEXT // the instruction stored to decoded code: stop so the cache is resynchronised
            } else {
                0
            }
        }
        Err(e) => {
            if matches!(e, Exit::Segv(_) | Exit::Undef { .. } | Exit::Unimpl { .. } | Exit::DivideError { .. } | Exit::Bounds { .. }) {
                cpu.eip = ins.ip() as u32;
            }
            *pending = Some(e);
            1 + S_EXIT
        }
    }
}

/// Checked guest load; on a fault records it in `cpu` and returns 0.
pub fn h_load_slow(cpu: &mut Cpu, mem: &Memory, addr: u32, size: u32, ip: u32) -> u32 {
    match mem.rn(addr, size as u8) {
        Ok(v) => v,
        Err(s) => {
            cpu.jit_fault = 1;
            cpu.jit_fault_addr = s.addr;
            cpu.jit_fault_write = s.write as u32;
            cpu.jit_aux = ip;
            0
        }
    }
}

/// Checked guest store. 0 = done, 1 = fault (recorded in `cpu`), 2 = done but decoded code was hit.
pub fn h_store_slow(cpu: &mut Cpu, mem: &mut Memory, addr: u32, size: u32, val: u32, ip: u32) -> u32 {
    match mem.wn(addr, size as u8, val) {
        Ok(()) => {
            if !mem.needs_sync() {
                0
            } else {
                2
            }
        }
        Err(s) => {
            cpu.jit_fault = 1;
            cpu.jit_fault_addr = s.addr;
            cpu.jit_fault_write = s.write as u32;
            cpu.jit_aux = ip;
            1
        }
    }
}

/// Evaluate the six arithmetic flags from a lazy tuple (`lk == 0`: they are in `cpu.fl`).
pub fn h_mat(cpu: &Cpu, lk: u32, la: u32, lb: u32, lr: u32) -> u32 {
    if lk == 0 {
        cpu.fl & ARITH
    } else {
        jit_flags::eval(lk, la, lb, lr)
    }
}

// =====================================================================================
// Region registry
// =====================================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RegionKind { Jit, Aot, Lir }

pub struct Region {
    pub kind: RegionKind,
    pub slot: u32,
    pub heads: Vec<(u32, u32)>,
    pub pages: Vec<u32>,
    /// byte ranges of the region's blocks; invalidation drops the region only when a written
    /// range actually overlaps one of these (or a thunk page), not on any write to the page
    pub spans: Vec<(u32, u32)>,
    pub thunk_pages: Vec<u32>,
    pub tags: Vec<u32>,
    /// Module-level fallback ownership: do not copy every module tag into every region.
    shared_tags: Option<Rc<Vec<u32>>>,
    pub n_ins: u32,
    /// taken out of the dispatch tables while one of its pages lacks P_X (see `park_page`)
    pub parked: bool,
    /// times this region left through `S_RETRY`
    pub retries: u32,
    pub aot_hot: u32,
}

#[derive(Clone, Copy)]
pub struct Entry {
    pub region: u32,
    pub slot: u32,
    pub head: u32,
}

#[derive(Default)]
pub struct JitStats {
    /// instructions copied into a region from callees that already head another region (`JitState::dup_ins`)
    pub dup_ins: u64,
    pub regions: u64,
    pub nodes: u64,
    pub ins_translated: u64,
    pub ins_fallback: u64,
    pub wasm_bytes: u64,
    pub invalidated: u64,
    pub calls: u64,
    pub compile_fail: u64,
    /// helper calls made by compiled code
    pub exec_calls: u64,
    /// Direct floating-point helpers: integer x87 load/store, compare, scalar convert, MMX transition.
    pub fp_helper_calls: [u64; 4],
    /// Regions installed from an offline-compiled, byte-guarded artifact.
    pub aot_regions: u64,
    pub aot_promoted: u64,
    pub aot_exec_calls: u64,
    pub lir_regions: u64,
    /// LIR adapters retired because they kept refusing
    pub lir_degraded: u64,
    /// Rust dispatcher entries only; direct wasm tail-call transfers are not counted here.
    pub aot_dispatches: u64,
    pub lir_dispatches: u64,
    pub mat_calls: u64,
    /// mat calls by flag kind (`lk`; index 0 = flags already in Cpu::fl)
    pub mat_by_kind: Vec<u64>,
    /// guest instructions retired (budget consumed: chain granular for compiled code) inside `Cpu::run`
    pub ins_total: u64,
    /// returns from a region to the dispatcher whose target was compiled (chaining miss) / not compiled (cold)
    pub ret_hit: u64,
    pub ret_cold: u64,
    /// exec_one calls by mnemonic
    pub exec_by_mn: Vec<u32>,
    pub slow_loads: u64,
    /// regions left through `S_RETRY` (a guest access the inline check refused), and regions recompiled with inline slow paths because of it
    pub retries: u64,
    /// regions parked / restored around protection flips (W^X), and regions dropped by stores into
    /// their bytes vs. by protection changes (docs/JIT.md, SMC section)
    pub parked: u64,
    pub unparked: u64,
    pub inv_store: u64,
    pub inv_prot: u64,
    /// chains replaced by an intrinsic fast path (jit_intr)
    pub intrinsics: u64,
    /// calls to PIC thunks translated without a call
    pub thunk_calls: u64,
    pub degraded: u64,
    pub slow_stores: u64,
    /// wall time (ns) spent translating (Rust) and installing (JS compile+instantiate); wasm32 only
    pub translate_ns: f64,
    pub install_ns: f64,
    /// blocks / instructions executed by the interpreter while the JIT is on
    pub interp_blocks: u64,
    pub interp_ins: u64,
    /// compiled-region calls from the dispatcher that returned with each status
    pub ret_next: u64,
    pub ret_other: u64,
    /// dispatcher lookups: found a compiled entry / had to interpret
    pub lookup_hit: u64,
    pub lookup_miss: u64,
    /// of the hits: the wasm-side chaining table already held the entry (should be ~0) / held another one (collision)
    pub hit_present: u64,
    pub hit_collision: u64,
    pub hit_empty: u64,
    /// opt-in (`JitState::profile`): wall time in compiled regions (includes the helpers they call),
    /// in the exec_one helper, in the mat helper and in interpreted blocks
    pub t_region_ns: f64,
    pub t_exec_ns: f64,
    pub t_mat_ns: f64,
    pub t_interp_ns: f64,
}

pub struct JitState {
    /// 0 = JIT off, 1 = translate every block on first sight (and eagerly decode its successors), N = after N executions.
    pub threshold: u32,
    regions: HashMap<u32, Region>,
    next_region: u32,
    map: HashMap<u32, Entry>,
    /// small direct-mapped front cache in front of `map`
    front: Vec<(u32, Entry)>,
    by_page: HashMap<u32, Vec<u32>>,
    fb: Vec<Instruction>,
    fb_free: Vec<u32>,
    /// Shared ownership for offline fallback tags referenced by several fused regions.
    fb_shared: std::collections::HashSet<u32>,
    fb_reserved: std::collections::HashSet<u32>,
    pub pending: Option<Exit>,
    pub stats: JitStats,
    pub fb_hits: HashMap<Mnemonic, u32>,
    /// Collect wall-clock timings of the dispatcher's phases (adds a few percent of overhead).
    pub profile: bool,
    /// Region selection: a successor block joins a region only if it ran at least `threshold / region_hot_div`
    /// times in the interpreter (0 = the original policy: every already decoded successor joins, even if another region already contains it).
    pub region_hot_div: u32,
    /// A call/jump target that already heads another region is copied into the region being built (so the transfer
    /// and, for calls, the matching `ret` stay inside one wasm function) when everything reachable from it is at
    /// most `dup_ins & 0xffff` instructions (0 = never). Bit 16 (`DUP_JUMPS`) extends this from call targets to all
    /// static edges.
    pub dup_ins: u32,
    /// Region start addresses whose regions retried a lot: compiled with inline slow paths from now on.
    slow_starts: std::collections::HashSet<u32>,
    /// Retries after which a region is recompiled with inline slow paths.
    pub retry_limit: u32,
    /// Refusals after which a LIR adapter is retired and the native method runs again. A refused call costs about
    /// 200 ns (a dispatcher bounce plus interpreting one block), so a method that keeps refusing must not keep paying.
    pub lir_retry_limit: u32,
    /// Heads of regions dropped by SMC invalidation, with the deadline (ns, `rt::now_ns` clock) before
    /// which they must not be recompiled: Flash's AVM2 JIT patches live method bodies in bursts, and
    /// recompiling on every invalidation turns into a compile storm whose modules mostly die young
    /// (47k modules / 21M dispatcher hits on trampoline-less slots in one game session). While a head
    /// cools, its blocks run in the interpreter; 0 disables the cooldown (eager/test modes bypass it too).
    cool: HashMap<u32, f64>,
    pub cool_ms: u64,
    /// Use wasm tail calls to chain regions / use wasm SIMD for inline SSE2 integer instructions.
    pub use_chain: bool,
    pub use_simd: bool,
    /// Replace recognised hot runtime functions by their fast paths (`jit_intr`).
    pub intrinsics: bool,
    /// Performance mode for loads (`Layout::trust_loads`).
    pub trust_loads: bool,
    /// Per-chain retired-instruction counters (see `Layout::prof`) and what each slot is.
    pub prof_chains: bool,
    prof_counters: Vec<u64>,
    prof_meta: Vec<(u32, u32, Vec<Mnemonic>, u32)>,
    /// profiling: `mat` calls by (guest address, flag kind) and what instruction sits at each call site
    mat_by_site: HashMap<(u32, u32), u64>,
    mat_site_names: HashMap<u32, String>,
    /// Tests only: (guest_base, rtab, wtab, cache) addresses inside the test harness's wasm memory.
    pub layout_override: Option<(u32, u32, u32, u32)>,
    /// Direct-mapped chaining table read by compiled code (see `Layout::cache`); entries (eip, slot + 1, head, 0).
    cache: Vec<[u32; 4]>,
}

const FRONT: usize = 1024;
/// `dup_ins` flag: also copy small compiled targets of jumps and fallthroughs, not only of calls.
pub const DUP_JUMPS: u32 = 0x10000;
pub const DUP_DEFAULT: u32 = DUP_JUMPS | 100;
/// `prof_meta` marker (in the instruction-count field) for a region exit site.
const EXIT_META: u32 = 0x8000_0000;
/// Limits for one region.
pub const MAX_NODES: usize = 96;
pub const MAX_INS: usize = 1500;

impl Default for JitState {
    fn default() -> Self {
        JitState::new()
    }
}

impl JitState {
    pub fn new() -> JitState {
        let none = Entry { region: u32::MAX, slot: 0, head: 0 };
        JitState {
            threshold: 0,
            regions: HashMap::new(),
            next_region: 0,
            map: HashMap::new(),
            front: vec![(1, none); FRONT],
            by_page: HashMap::new(),
            fb: Vec::new(),
            fb_free: Vec::new(),
            fb_shared: std::collections::HashSet::new(),
            fb_reserved: std::collections::HashSet::new(),
            pending: None,
            stats: JitStats { mat_by_kind: vec![0; 64], exec_by_mn: vec![0; 8192], ..Default::default() },
            fb_hits: HashMap::new(),
            layout_override: None,
            profile: false,
            region_hot_div: 4,
            dup_ins: DUP_DEFAULT,
            slow_starts: std::collections::HashSet::new(),
            retry_limit: 2000,
            lir_retry_limit: 4096,
            cool: HashMap::new(),
            cool_ms: 200,
            use_chain: true,
            use_simd: true,
            intrinsics: true,
            trust_loads: false,
            prof_chains: false,
            prof_counters: Vec::new(),
            prof_meta: Vec::new(),
            mat_by_site: HashMap::new(),
            mat_site_names: HashMap::new(),
            cache: vec![[0; 4]; CACHE_ENTRIES as usize],
        }
    }

    #[inline(always)]
    pub fn lookup(&mut self, eip: u32) -> Option<Entry> {
        let i = (eip as usize ^ (eip as usize >> 10)) & (FRONT - 1);
        let (k, e) = self.front[i];
        if k == eip && e.region != u32::MAX {
            // Heal the wasm-side chaining table: a colliding head may have evicted this entry.
            let idx = Self::cache_idx(eip);
            let c = self.cache[idx];
            if c[0] != eip || c[1] == 0 {
                if self.profile {
                    if c[1] != 0 {
                        self.stats.hit_collision += 1;
                    } else {
                        self.stats.hit_empty += 1;
                    }
                }
                self.cache[idx] = [eip, e.slot + 1, e.head, 0];
            } else if self.profile {
                self.stats.hit_present += 1;
            }
            return Some(e);
        }
        if self.map.is_empty() {
            return None;
        }
        let e = *self.map.get(&eip)?;
        self.front[i] = (eip, e);
        self.cache_put(eip, e);
        Some(e)
    }

    /// A region of `ent` left through `S_RETRY`. After `retry_limit` such exits the region is dropped and its start is recompiled
    /// with inline slow paths (`Layout::slow_inline`): a persistent slow access then costs a helper call, not a region exit.
    pub fn note_retry(&mut self, region: u32) {
        self.stats.retries += 1;
        let lir = self.lir_retry_limit;
        let Some(r) = self.regions.get_mut(&region) else { return };
        r.retries += 1;
        if r.kind == RegionKind::Lir {
            // Retire the adapter: the native method (compiled normally once hot) takes over for good.
            if r.retries >= lir {
                self.stats.lir_degraded += 1;
                self.drop_region(region);
            }
            return;
        }
        if r.retries >= self.retry_limit {
            let start = r.heads.first().map(|h| h.0);
            if let Some(st) = start {
                self.slow_starts.insert(st);
            }
            self.stats.degraded += 1;
            self.drop_region(region);
        }
    }

    /// A region left through `S_RETRY`. A LIR adapter reached through a tail chain refuses with `cpu.eip` still at its
    /// own entry, while the dispatcher only knows the (caller) region it entered: blame the LIR region that owns `eip`
    /// when there is one, otherwise the region the dispatcher entered.
    pub fn blame_retry(&mut self, entered: u32, eip: u32) {
        let lir = self.map.get(&eip).map(|e| e.region).filter(|r| self.regions.get(r).is_some_and(|r| r.kind == RegionKind::Lir));
        self.note_retry(lir.unwrap_or(entered));
    }

    /// Size (translated instructions) of the region whose entry is `eip`.
    pub fn region_ins(&self, eip: u32) -> Option<u32> {
        self.map.get(&eip).and_then(|e| self.regions.get(&e.region)).map(|r| r.n_ins)
    }

    pub fn has_entry(&self, eip: u32) -> bool {
        self.map.contains_key(&eip)
    }
    pub fn is_lir_entry(&self, eip: u32) -> bool {
        self.map.get(&eip).and_then(|e| self.regions.get(&e.region)).is_some_and(|r| r.kind == RegionKind::Lir)
    }
    pub fn is_aot_entry(&self, eip: u32) -> bool {
        self.map.get(&eip).and_then(|e| self.regions.get(&e.region)).is_some_and(|r| r.kind == RegionKind::Aot)
    }
    /// Offline tier-1 blocks must not permanently prevent hot-region formation.
    /// Promote after enough direct dispatches; normal JIT selection then fuses the
    /// surrounding already-decoded paths. The original offline artifact stays offline.
    pub fn should_promote_aot(&mut self, id: u32) -> bool {
        let Some(region) = self.regions.get_mut(&id) else { return false };
        if region.kind != RegionKind::Aot { return false; }
        region.aot_hot = region.aot_hot.saturating_add(1);
        region.aot_hot >= self.threshold.max(50)
    }
    pub fn promote_aot(&mut self, id: u32) { self.stats.aot_promoted += 1; self.drop_region(id); }

    pub fn fb_ins(&self, tag: u32) -> &Instruction {
        &self.fb[tag as usize]
    }

    fn alloc_tag(fb: &mut Vec<Instruction>, free: &mut Vec<u32>, ins: &Instruction) -> u32 {
        if let Some(t) = free.pop() {
            fb[t as usize] = *ins;
            t
        } else {
            fb.push(*ins);
            (fb.len() - 1) as u32
        }
    }

    /// Translate `blocks` (entry = blocks[0]) into one region and install it. Returns false if it could not be installed.
    pub fn compile(&mut self, mem: &Memory, blocks: &[Rc<Block>]) -> bool {
        let (gb, rt, wt, cb) = match self.layout_override {
            Some(t) => t,
            None => {
                let (g, r, w) = mem.jit_addrs();
                (g as u32, r as u32, w as u32, if cfg!(target_arch = "wasm32") { self.cache.as_ptr() as usize as u32 } else { 0 })
            }
        };
        let mut lay = Layout::for_cpu(gb, rt, wt, if self.use_chain { cb } else { 0 });
        lay.simd = self.use_simd;
        lay.slow_inline = blocks.first().map_or(false, |b| self.slow_starts.contains(&b.start));
        lay.trust_loads = self.trust_loads;
        if self.prof_chains {
            if self.prof_counters.capacity() == 0 {
                self.prof_counters = Vec::with_capacity(1 << 20); // never reallocated: compiled code holds its address
                self.prof_counters.resize(1 << 20, 0);
            }
            lay.prof = self.prof_counters.as_ptr() as usize as u32;
        }
        if self.prof_chains && self.prof_meta.is_empty() {
            for _ in 0..8 {
                self.prof_meta.push((0xffff_ffff, 0, Vec::new(), 0)); // reserved: exit-class counters
            }
        }
        let prof_slot0 = self.prof_meta.len() as u32;
        let nodes: Vec<NodeIn> = blocks.iter().map(|b| NodeIn { start: b.start, end: b.end, ins: &b.ins, intrinsic: if self.intrinsics { crate::jit_x86::intr_detect(mem, b.start) } else { 0 }, thunk: b.thunk }).collect();
        let mut tags: Vec<u32> = Vec::new();
        let mut hits = FbStats::new();
        let t_start = rt::now_ns();
        let (bytes, outs) = {
            let fb = &mut self.fb;
            let free = &mut self.fb_free;
            let tags = &mut tags;
            translate_module(
                &lay,
                &[nodes],
                &mut |ins: &Instruction| {
                    let t = Self::alloc_tag(fb, free, ins);
                    tags.push(t);
                    t
                },
                &mut hits,
                prof_slot0,
            )
        };
        let out = &outs[0];
        let t_mid = rt::now_ns();
        let slots = rt::install(&bytes, 1);
        self.stats.translate_ns += t_mid - t_start;
        self.stats.install_ns += rt::now_ns() - t_mid;
        let slots = match slots {
            Some(s) => s,
            None => {
                self.stats.compile_fail += 1;
                for t in tags {
                    self.fb_free.push(t);
                }
                return false;
            }
        };
        for (m, c) in hits {
            *self.fb_hits.entry(m).or_insert(0) += c;
        }
        if self.prof_chains {
            for (ip, name) in &out.mat_sites {
                self.mat_site_names.insert(*ip, name.clone());
            }
            for &(eip, n) in &out.chains {
                let mns: Vec<Mnemonic> = blocks.iter().find(|b| b.start == eip).map(|b| b.ins.iter().take(18).map(|i| i.mnemonic()).collect()).unwrap_or_default();
                self.prof_meta.push((eip, n, mns, 0));
            }
            self.prof_meta.push((0, 0, Vec::new(), 0)); // region-entry counter slot
            let from = out.chains.first().map(|c| c.0).unwrap_or(0);
            for &(class, to) in &out.exits {
                self.prof_meta.push((to, EXIT_META | class, Vec::new(), from));
            }
        }
        let id = self.next_region;
        self.next_region += 1;
        let mut pages: Vec<u32> = Vec::new();
        let mut spans: Vec<(u32, u32)> = Vec::new();
        let mut thunk_pages: Vec<u32> = Vec::new();
        for b in blocks {
            let first = b.start >> PAGE_SHIFT;
            // The comparator entry block ends at its conditional branch, but the
            // intrinsic assumes the complete leaf, including its epilogue.
            let end = if self.intrinsics {
                crate::jit_x86::intr::code_len(crate::jit_x86::intr_detect(mem, b.start))
                    .map_or(b.end, |len| b.start.saturating_add(len))
            } else { b.end };
            let last = end.wrapping_sub(1) >> PAGE_SHIFT;
            for pg in first..=last {
                if !pages.contains(&pg) {
                    pages.push(pg);
                }
            }
            spans.push((b.start, end));
            if b.thunk != 0 {
                // the thunk's bytes were assumed when its call was translated; thunks are tiny and
                // shared, so they keep page granularity
                let pg = b.thunk_target >> PAGE_SHIFT;
                if !pages.contains(&pg) {
                    pages.push(pg);
                }
                if !thunk_pages.contains(&pg) {
                    thunk_pages.push(pg);
                }
            }
        }
        for &pg in &pages {
            self.by_page.entry(pg).or_default().push(id);
        }
        for &(eip, head) in &out.heads {
            let e = *self.map.entry(eip).or_insert(Entry { region: id, slot: slots[0], head });
            if e.region == id {
                self.cache_put(eip, e);
            }
        }
        self.stats.regions += 1;
        self.stats.nodes += blocks.len() as u64;
        self.stats.ins_translated += out.n_ins as u64;
        self.stats.ins_fallback += out.n_fallback as u64;
        self.stats.intrinsics += out.n_intr as u64;
        self.stats.thunk_calls += blocks.iter().filter(|b| b.thunk != 0).count() as u64;
        self.stats.wasm_bytes += bytes.len() as u64;
        self.regions.insert(id, Region { kind: RegionKind::Jit, slot: slots[0], heads: out.heads.clone(), pages, spans, thunk_pages, tags, shared_tags: None, n_ins: out.n_ins, parked: false, retries: 0, aot_hot: 0 });
        true
    }

    /// Safe layout for offline artifacts: no embedded chaining/profiling pointers and no unchecked loads.
    /// Guest/table addresses are instance-specific; callers must compile for this exact layout.
    pub fn aot_layout(&self, mem: &Memory) -> Layout {
        let (gb, rt, wt) = match self.layout_override {
            Some((g, r, w, _)) => (g, r, w),
            None => {
                let (g, r, w) = mem.jit_addrs();
                (g as u32, r as u32, w as u32)
            }
        };
        let mut lay = Layout::for_cpu(gb, rt, wt, 0);
        lay.simd = false; // portable AOT default; no machine-specific SIMD assumption
        lay
    }
    /// Native LIR adapters and symbolically bound AOT can use the live chaining table;
    /// offline files must never bake its instance-specific address in without a relocation.
    pub fn lir_layout(&self, mem: &Memory) -> Layout {
        let mut layout = self.aot_layout(mem);
        if self.use_chain {
            layout.cache = self.layout_override.map_or_else(|| if cfg!(target_arch = "wasm32") { self.cache.as_ptr() as usize as u32 } else { 0 }, |t| t.3);
        }
        layout
    }

    /// Install trusted offline compiler output, never an arbitrary downloaded wasm module.
    /// Byte/layout guards are compatibility checks, not a sandbox or artifact authentication.
    /// Every dependency must be included in `guards`; fallback tags require explicit
    /// reservation ownership, while opaque intrinsic assumptions are forbidden.
    pub fn install_precompiled(
        &mut self,
        mem: &mut Memory,
        layout: &Layout,
        wasm: &[u8],
        regions: &[(RegionOut, Vec<(u32, Vec<u8>)>)],
    ) -> Result<usize, String> {
        self.install_precompiled_inner(mem, layout, wasm, regions, false, &[], None)
    }

    /// Portable block bundles may share already-hot PIC thunks with the loader.
    /// Keep existing entries and discard only the new slots for those entire regions.
    pub fn install_precompiled_available(&mut self, mem: &mut Memory, layout: &Layout, wasm: &[u8],
        regions: &[(RegionOut, Vec<(u32, Vec<u8>)>)]) -> Result<usize, String> {
        self.install_precompiled_inner(mem, layout, wasm, regions, true, &[], None)
    }
    pub fn install_precompiled_excluding(&mut self, mem: &mut Memory, layout: &Layout, wasm: &[u8],
        regions: &[(RegionOut, Vec<(u32, Vec<u8>)>)], blocked: &[u32]) -> Result<usize, String> {
        self.install_precompiled_inner(mem, layout, wasm, regions, true, blocked, None)
    }

    /// Reserve decoded offline fallback records transactionally. The caller patches
    /// explicit tag relocation sites only; no x86->wasm translation takes place here.
    pub fn allocate_offline_tags(&mut self, instructions: &[Instruction]) -> Vec<u32> {
        instructions.iter().map(|ins| {
            let tag = Self::alloc_tag(&mut self.fb, &mut self.fb_free, ins);
            self.fb_reserved.insert(tag);
            tag
        }).collect()
    }
    pub fn release_offline_tags(&mut self, tags: &[u32]) {
        // Idempotent on rollback: an installed/shared tag or already returned
        // reservation must never be inserted twice into the free list.
        for &tag in tags { if self.fb_reserved.remove(&tag) { self.fb_free.push(tag); } }
    }
    pub fn install_precompiled_fallbacks(&mut self, mem: &mut Memory, layout: &Layout, wasm: &[u8],
        regions: &[(RegionOut, Vec<(u32, Vec<u8>)>)], blocked: &[u32], tags: &[u32]) -> Result<usize, String> {
        self.install_precompiled_inner(mem, layout, wasm, regions, true, blocked, Some(tags))
    }

    fn install_precompiled_inner(&mut self, mem: &mut Memory, layout: &Layout, wasm: &[u8],
        regions: &[(RegionOut, Vec<(u32, Vec<u8>)>)], keep_existing: bool, blocked: &[u32], offline_tags: Option<&[u32]>) -> Result<usize, String> {
        if self.threshold == 0 { return Err("AOT requires the compiled-code dispatcher (JIT enabled)".into()); }
        if *layout != self.aot_layout(mem) && *layout != self.lir_layout(mem) { return Err("compiled layout/CPU ABI differs from this instance".into()); }
        if regions.is_empty() || regions.len() > 1024 || wasm.len() > 64 * 1024 * 1024 {
            return Err("AOT artifact exceeds installation limits".into());
        }
        self.next_region.checked_add(regions.len() as u32).ok_or("AOT region IDs exhausted")?;
        let mut seen = std::collections::HashSet::new();
        let skip: Vec<bool> = regions.iter().map(|(out, guards)|
            (keep_existing && out.heads.iter().any(|&(ip, _)| self.has_entry(ip)))
                || guards.iter().any(|(start, bytes)| blocked.iter().any(|&point| point >= *start && (point as u64) < *start as u64 + bytes.len() as u64))).collect();
        let mut guard_bytes = 0usize;
        for (out, guards) in regions {
            if (out.n_fallback != 0 && offline_tags.is_none()) || out.n_intr != 0 || out.heads.is_empty() || out.n_ins == 0 {
                return Err("AOT requires nonempty regions, explicit fallback ownership and no opaque intrinsics".into());
            }
            if guards.is_empty() { return Err("AOT region has no code guards".into()); }
            for (start, bytes) in guards {
                guard_bytes = guard_bytes.checked_add(bytes.len()).ok_or("AOT guard size overflow")?;
                if bytes.is_empty() || bytes.len() > u32::MAX as usize || guard_bytes > 16 * 1024 * 1024 {
                    return Err("AOT guard size exceeds limits".into());
                }
                let end = start.checked_add(bytes.len() as u32).ok_or("AOT code span wraps")?;
                if mem.read_bytes(*start, bytes.len() as u32).map_err(|_| "AOT code is unreadable")? != bytes {
                    return Err(format!("AOT code guard mismatch at {start:#x}"));
                }
                for pg in (*start >> PAGE_SHIFT)..=((end - 1) >> PAGE_SHIFT) {
                    if mem.page_prot(pg << PAGE_SHIFT) & crate::mem::P_X == 0 {
                        return Err("AOT code is not executable".into());
                    }
                }
            }
            for &(eip, head) in &out.heads {
                if (!keep_existing && self.has_entry(eip)) || !seen.insert(eip) {
                    return Err(format!("AOT head already registered at {eip:#x}"));
                }
                if !guards.iter().any(|(a, b)| eip >= *a && (eip as u64) < *a as u64 + b.len() as u64)
                    || !out.chains.iter().any(|&(a, _)| a == eip) || head as usize >= out.chains.len() {
                    return Err("AOT head lies outside its guarded code/chain metadata".into());
                }
            }
        }
        let installing = skip.iter().filter(|&&v| !v).count();
        if installing == 0 { return Ok(0); }
        if let Some(tags) = offline_tags {
            if tags.len() > 262144 { return Err("offline fallback reservation limit".into()); }
            let unique: std::collections::HashSet<_> = tags.iter().copied().collect();
            if unique.len() != tags.len() || tags.iter().any(|&tag| tag as usize >= self.fb.len() || !self.fb_reserved.contains(&tag) || self.fb_shared.contains(&tag)) {
                return Err("invalid offline fallback tag ownership".into());
            }
            if regions.iter().any(|(out, _)| out.n_fallback != 0) && tags.is_empty() { return Err("offline fallback metadata missing".into()); }
            let mut raw_intervals: Vec<_> = regions.iter().flat_map(|(_, spans)| spans)
                .map(|(start, bytes)| (*start, *start as u64 + bytes.len() as u64)).collect();
            raw_intervals.sort_unstable();
            let mut intervals: Vec<(u32, u64)> = Vec::new();
            for (start, end) in raw_intervals {
                if let Some(last) = intervals.last_mut().filter(|last| last.1 >= start as u64) {
                    last.1 = last.1.max(end);
                } else { intervals.push((start, end)); }
            }
            for &tag in tags {
                let ins = &self.fb[tag as usize];
                let ip = ins.ip32();
                let end = ip as u64 + ins.len() as u64;
                let i = intervals.partition_point(|&(start, _)| start <= ip);
                if i == 0 || intervals[i - 1].1 < end { return Err("offline fallback instruction lacks guarded code dependency".into()); }
            }
        }
        let slots = rt::install(wasm, regions.len()).ok_or("AOT wasm compilation/installation failed")?;
        if slots.len() != regions.len() {
            for s in slots { rt::free(s); }
            return Err("AOT installation returned the wrong number of slots".into());
        }
        let shared_tags = offline_tags.filter(|tags| !tags.is_empty()).map(|tags| Rc::new(tags.to_vec()));
        if let Some(tags) = &shared_tags { for &tag in tags.iter() { self.fb_reserved.remove(&tag); self.fb_shared.insert(tag); } }
        // No fallible work after installation: publish all heads together between scheduler slices.
        for (((out, guards), slot), skip) in regions.iter().zip(slots).zip(skip) {
            if skip { rt::free(slot); continue; }
            let id = self.next_region;
            self.next_region += 1;
            let mut pages = Vec::new();
            let mut spans = Vec::new();
            for (start, bytes) in guards {
                let end = *start + bytes.len() as u32;
                mem.mark_code(*start, bytes.len() as u32);
                spans.push((*start, end));
                for pg in (*start >> PAGE_SHIFT)..=((end - 1) >> PAGE_SHIFT) {
                    if !pages.contains(&pg) { pages.push(pg); }
                }
            }
            for &pg in &pages { self.by_page.entry(pg).or_default().push(id); }
            for &(eip, head) in &out.heads {
                let ent = Entry { region: id, slot, head };
                self.map.insert(eip, ent);
                self.cache_put(eip, ent);
            }
            self.stats.regions += 1;
            self.stats.aot_regions += 1;
            self.stats.nodes += spans.len() as u64;
            self.stats.ins_translated += out.n_ins as u64;
            self.stats.ins_fallback += out.n_fallback as u64;
            self.regions.insert(id, Region {
                kind: RegionKind::Aot,
                slot, heads: out.heads.clone(), pages, spans, thunk_pages: Vec::new(),
                tags: Vec::new(), shared_tags: shared_tags.clone(), n_ins: out.n_ins, parked: false, retries: 0, aot_hot: 0,
            });
        }
        self.stats.wasm_bytes += wasm.len() as u64;
        Ok(installing)
    }

    #[inline]
    fn cache_idx(eip: u32) -> usize {
        ((eip ^ (eip >> 12)) & (CACHE_ENTRIES - 1)) as usize
    }

    pub fn note_dispatch(&mut self, region: u32) {
        self.stats.calls += 1;
        match self.regions.get(&region).map(|r| r.kind) {
            Some(RegionKind::Aot) => self.stats.aot_dispatches += 1,
            Some(RegionKind::Lir) => self.stats.lir_dispatches += 1,
            _ => {}
        }
    }

    pub fn mark_lir_entry(&mut self, eip: u32) {
        if let Some(entry) = self.map.get(&eip) {
            if let Some(region) = self.regions.get_mut(&entry.region) {
                if region.kind == RegionKind::Aot {
                    region.kind = RegionKind::Lir;
                    self.stats.aot_regions -= 1;
                    self.stats.lir_regions += 1;
                }
            }
        }
    }

    fn cache_put(&mut self, eip: u32, e: Entry) {
        self.cache[Self::cache_idx(eip)] = [eip, e.slot + 1, e.head, 0];
    }

    fn drop_region(&mut self, id: u32) {
        self.unregister(id);
        let Some(r) = self.regions.remove(&id) else { return };
        for pg in &r.pages {
            if let Some(v) = self.by_page.get_mut(pg) {
                v.retain(|&i| i != id);
                if v.is_empty() {
                    self.by_page.remove(pg);
                }
            }
        }
        rt::free(r.slot);
        for t in r.tags {
            self.fb_free.push(t);
        }
        if let Some(tags) = r.shared_tags {
            if Rc::strong_count(&tags) == 1 {
                for &tag in tags.iter() { self.fb_shared.remove(&tag); self.fb_free.push(tag); }
            }
        }
        self.stats.invalidated += 1;
    }

    /// Take a region out of the dispatch map, the front cache and the chaining table, keeping its
    /// compiled module and table slot (see `park_page`).
    fn unregister(&mut self, id: u32) {
        let Some(r) = self.regions.get(&id) else { return };
        let heads = r.heads.clone();
        let slot = r.slot;
        for (eip, _) in &heads {
            if self.map.get(eip).map(|e| e.region) == Some(id) {
                self.map.remove(eip);
            }
            // the chaining table must never point at a slot that is not dispatchable
            let c = &mut self.cache[Self::cache_idx(*eip)];
            if c[0] == *eip && c[1] == slot + 1 {
                *c = [0; 4];
            }
        }
        for f in self.front.iter_mut() {
            if f.1.region == id {
                f.1.region = u32::MAX;
            }
        }
    }

    fn register(&mut self, id: u32) {
        let Some(r) = self.regions.get(&id) else { return };
        let heads = r.heads.clone();
        let slot = r.slot;
        for (eip, head) in &heads {
            let e = *self.map.entry(*eip).or_insert(Entry { region: id, slot, head: *head });
            if e.region == id {
                self.cache_put(*eip, e);
            }
        }
    }

    /// A code page lost P_X (mprotect W^X flip, munmap pending): its regions stop being dispatchable
    /// but keep their compiled modules - protection changes do not modify bytes, so recompiling here
    /// would be pure waste (Flash flips W^X around every nanojit emission: ~19k mprotects per session).
    pub fn park_page(&mut self, page: u32) {
        let Some(ids) = self.by_page.get(&page).cloned() else { return };
        for id in ids {
            let park = match self.regions.get(&id) {
                Some(r) => !r.parked,
                None => false,
            };
            if park {
                self.unregister(id);
                if let Some(r) = self.regions.get_mut(&id) {
                    r.parked = true;
                }
                self.stats.parked += 1;
            }
        }
    }

    /// A code page became executable again: re-register every parked region whose pages are all
    /// executable. Regions parked over a page that never comes back are dropped with it eventually
    /// (a store into the page drops them, `flush_all` frees them).
    pub fn unpark_page(&mut self, page: u32, exec: impl Fn(u32) -> bool) {
        let Some(ids) = self.by_page.get(&page).cloned() else { return };
        for id in ids {
            let Some((heads, pages, parked)) = self.regions.get(&id).map(|r| (r.heads.clone(), r.pages.clone(), r.parked)) else { continue };
            if !parked || !pages.iter().all(|&pg| exec(pg)) {
                continue; // not parked, or another page of the region is still not executable
            }
            // a head taken over by another region while parked makes this one unreachable
            let taken = heads.iter().any(|(e, _)| matches!(self.map.get(e), Some(e2) if e2.region != id));
            if taken {
                self.drop_region(id);
            } else {
                self.register(id);
                if let Some(r) = self.regions.get_mut(&id) {
                    r.parked = false;
                }
                self.stats.unparked += 1;
            }
        }
    }

    /// Drop every region whose code bytes overlap `[s, e)` (candidate set: regions with code on `page`).
    /// Flash's AVM2 JIT emits fresh method bodies into pages that already hold live JITted code; page
    /// granularity would drop every hot region sharing the page on each emission, which turns into a
    /// recompile storm (and, with background compiles, into regions that never go live at all).
    pub fn invalidate_range(&mut self, page: u32, s: u32, e: u32, prot: bool) {
        let Some(ids) = self.by_page.get(&page).cloned() else { return };
        for id in ids {
            let hit = match self.regions.get(&id) {
                Some(r) => r.spans.iter().any(|&(a, b)| a < e && b > s) || r.thunk_pages.contains(&page),
                None => false,
            };
            if hit {
                let heads = self.regions.get(&id).map(|r| r.heads.clone()).unwrap_or_default();
                self.drop_region(id);
                if prot {
                    self.stats.inv_prot += 1;
                } else {
                    self.stats.inv_store += 1;
                }
                if self.cool_ms > 0 {
                    let deadline = rt::now_ns() + self.cool_ms as f64 * 1_000_000.0;
                    for (h, _) in heads {
                        self.cool.insert(h, deadline);
                    }
                }
            }
        }
        if let Some(v) = self.by_page.get_mut(&page) {
            v.retain(|id| self.regions.contains_key(id));
            if v.is_empty() {
                self.by_page.remove(&page);
            }
        }
    }

    /// True while `eip` is the head of a recently invalidated region and the cooldown has not expired.
    pub fn cooled(&mut self, eip: u32) -> bool {
        let Some(&deadline) = self.cool.get(&eip) else { return false };
        if rt::now_ns() >= deadline {
            self.cool.remove(&eip);
            false
        } else {
            true
        }
    }

    /// True while any compiled region still has code on `page` (the store fast-path flag stays set).
    pub fn page_in_use(&self, page: u32) -> bool {
        self.by_page.contains_key(&page)
    }

    pub fn flush_all(&mut self) {
        let ids: Vec<u32> = self.regions.keys().copied().collect();
        for id in ids {
            self.drop_region(id);
        }
        self.by_page.clear();
        debug_assert!(self.fb_shared.is_empty());
    }

    /// The hottest chains by retired instructions (needs `prof_chains`).
    /// The `n` most executed region exit sites: (count, class, region head eip, static target or 0).
    pub fn hot_exits(&self, n: usize) -> Vec<(u64, u32, u32, u32)> {
        let mut v: Vec<(u64, u32, u32, u32)> = (0..self.prof_meta.len())
            .filter(|&i| self.prof_meta[i].1 & EXIT_META != 0)
            .map(|i| (self.prof_counters[i], self.prof_meta[i].1 & 0xff, self.prof_meta[i].3, self.prof_meta[i].0))
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.truncate(n);
        v
    }

    pub fn hot_report(&self, n: usize) -> String {
        let mut total: u64 = 0;
        let (mut chain_entries, mut region_entries, mut nchains) = (0u64, 0u64, 0u64);
        for (i, m) in self.prof_meta.iter().enumerate() {
            if m.0 == 0xffff_ffff {
                continue;
            }
            if m.1 & EXIT_META != 0 {
                continue;
            }
            if m.1 == 0 {
                region_entries += self.prof_counters[i];
            } else {
                total += self.prof_counters[i];
                chain_entries += self.prof_counters[i] / m.1 as u64;
                nchains += 1;
            }
        }
        let ex = &self.prof_counters;
        let exits = format!("[jit] region exits by class: jump/fall={}M call={}M ret(unmatched)={}M indirect={}M other={}M\n", ex[0] / 1_000_000, ex[1] / 1_000_000, ex[2] / 1_000_000, ex[3] / 1_000_000, ex[4] / 1_000_000);
        let mut idx: Vec<usize> = (0..self.prof_meta.len()).filter(|&i| self.prof_meta[i].1 != 0 && self.prof_meta[i].1 & EXIT_META == 0).collect();
        idx.sort_by(|&a, &b| self.prof_counters[b].cmp(&self.prof_counters[a]));
        let mut out = format!("[jit] hot chains (counted instructions {}M over {} chains): chain entries {}M (avg {:.1} ins), region entries {}M (avg {:.1} chains/entry)\n", total / 1_000_000, nchains, chain_entries / 1_000_000, total as f64 / chain_entries.max(1) as f64, region_entries / 1_000_000, chain_entries as f64 / region_entries.max(1) as f64);
        out += &exits;
        {
            let mut v: Vec<(&(u32, u32), &u64)> = self.mat_by_site.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            let total: u64 = self.mat_by_site.values().sum();
            out += &format!("[jit] mat calls {}M; top sites:\n", total / 1_000_000);
            for ((ip, lk), c) in v.into_iter().take(16) {
                out += &format!("[jit]  mat {:>6}M @{:08x} kind {} (op {} sz {}) in {}\n", c / 1_000_000, ip, lk, lk >> 2, 1u32 << (lk & 3), self.mat_site_names.get(ip).map(|s| s.as_str()).unwrap_or("?"));
            }
        }
        let mut acc = 0u64;
        for &i in idx.iter().take(n) {
            let (eip, len, mns, _) = &self.prof_meta[i];
            acc += self.prof_counters[i];
            out += &format!("[jit]  {:>6.2}% (cum {:>5.1}%) @{:08x} len={:<3} {}\n", 100.0 * self.prof_counters[i] as f64 / total.max(1) as f64, 100.0 * acc as f64 / total.max(1) as f64, eip, len, mns.iter().map(|m| format!("{:?}", m)).collect::<Vec<_>>().join(" "));
        }
        out
    }

    pub fn report(&self) -> String {
        let s = &self.stats;
        let mut v: Vec<(&Mnemonic, &u32)> = self.fb_hits.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let top: Vec<String> = v.iter().take(12).map(|(m, c)| format!("{:?}={}", m, c)).collect();
        format!(
            "[jit] regions={} nodes={} ins={} dup_ins={} fallback_ins={} intrinsics={} wasm_bytes={} invalidated={} calls={} fail={} live={} parked={} unparked={} inv_store={} inv_prot={}\n[jit] helper calls: exec_one={} mat={} slow_load={} slow_store={}\n[jit] mat by kind: {}\n[jit] guest ins retired ~{}M (interpreted {}M); returns to dispatcher: target compiled {} / cold {}\n[jit] exec_one by mnemonic: {}\n[jit] interpreter: blocks={} ins={}; region returns to dispatcher: next={} other={}; dispatcher lookups: hit={} miss={} (hit but table present={} collided={} empty={})\n[jit] time: translate {:.0} ms, install(js compile) {:.0} ms; profile: regions {:.0} ms (of which exec_one {:.0}, mat {:.0}), interpreter {:.0} ms\n[jit] fallback sites: {}",
            s.regions,
            s.nodes,
            s.ins_translated,
            s.dup_ins,
            s.ins_fallback,
            s.intrinsics,
            s.wasm_bytes,
            s.invalidated,
            s.calls,
            s.compile_fail,
            self.regions.len(),
            s.parked,
            s.unparked,
            s.inv_store,
            s.inv_prot,
            s.exec_calls,
            s.mat_calls,
            s.slow_loads,
            s.slow_stores,
            s.mat_by_kind.iter().enumerate().filter(|(_, &c)| c > 0).map(|(k, c)| format!("{}:{}={}", k, if k == 0 { "fl".to_string() } else { format!("op{}/sz{}", k >> 2, 1 << (k & 3)) }, c)).collect::<Vec<_>>().join(" "),
            s.ins_total / 1_000_000,
            s.interp_ins / 1_000_000,
            s.ret_hit,
            s.ret_cold,
            { let mut v: Vec<(usize, u32)> = s.exec_by_mn.iter().copied().enumerate().filter(|&(_, c)| c > 0).collect(); v.sort_by(|a, b| b.1.cmp(&a.1)); v.iter().take(16).map(|&(m, c)| format!("{:?}={}", iced_x86::Code::values().nth(m).map(|x| format!("{:?}", x)).unwrap_or_default(), c)).collect::<Vec<_>>().join(" ") },
            s.interp_blocks,
            s.interp_ins,
            s.ret_next,
            s.ret_other,
            s.lookup_hit,
            s.lookup_miss,
            s.hit_present,
            s.hit_collision,
            s.hit_empty,
            s.translate_ns / 1e6,
            s.install_ns / 1e6,
            s.t_region_ns / 1e6,
            s.t_exec_ns / 1e6,
            s.t_mat_ns / 1e6,
            s.t_interp_ns / 1e6,
            top.join(" ")
        ) + &format!("\n[jit] direct fp helpers: int64={} compare={} cvtss2si={} mmx_transition={}",
            s.fp_helper_calls[0], s.fp_helper_calls[1], s.fp_helper_calls[2], s.fp_helper_calls[3])
          + &format!("\n[jit] offline regions installed={}", s.aot_regions)
          + &format!("\n[jit] offline hot regions promoted_to_jit={}", s.aot_promoted)
          + &format!("\n[aot] serialized_fallback_calls={}", s.aot_exec_calls)
          + &format!("\n[accel] Rust dispatcher entries: aot={} lir={} lir_regions={} lir_retired_for_refusing={}", s.aot_dispatches, s.lir_dispatches, s.lir_regions, s.lir_degraded)
          + &self.exit_class_line()
    }

    /// Exact counts of region exits by class (with `prof_chains`): the transfers between compiled regions / to the dispatcher.
    fn exit_class_line(&self) -> String {
        if self.prof_counters.len() < 5 || !self.prof_chains {
            return String::new();
        }
        let ex = &self.prof_counters;
        format!("\n[jit] region exits (exact): jump/fall={} call={} ret(unmatched)={} indirect={} other(budget)={}", ex[0], ex[1], ex[2], ex[3], ex[4])
    }
}

// =====================================================================================
// Context + wasm32 glue
// =====================================================================================

/// The live `Memory` / `JitState` the helpers operate on (set by `Cpu::run` before entering compiled code).
pub mod ctx {
    use super::*;
    #[cfg(target_arch = "wasm32")]
    static mut CTX: (*mut Memory, *mut JitState) = (std::ptr::null_mut(), std::ptr::null_mut());
    #[cfg(not(target_arch = "wasm32"))]
    thread_local! {
        static CTX: std::cell::Cell<(*mut Memory, *mut JitState)> = const { std::cell::Cell::new((std::ptr::null_mut(), std::ptr::null_mut())) };
    }

    #[cfg(target_arch = "wasm32")]
    pub fn set(mem: &mut Memory, jit: &mut JitState) {
        unsafe { CTX = (mem, jit) }
    }
    #[cfg(target_arch = "wasm32")]
    pub unsafe fn get() -> (*mut Memory, *mut JitState) {
        CTX
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set(mem: &mut Memory, jit: &mut JitState) {
        CTX.with(|c| c.set((mem, jit)))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub unsafe fn get() -> (*mut Memory, *mut JitState) {
        CTX.with(|c| c.get())
    }
}
pub use ctx::set as set_ctx;

#[cfg(target_arch = "wasm32")]
mod exports {
    use super::*;
    #[no_mangle]
    pub extern "C" fn jit_fist_fild(cpu: *mut Cpu, addr: u32, ip: u32, fist: u32) -> u32 {
        unsafe {
            let (mem, jit) = ctx::get();
            (*jit).stats.fp_helper_calls[0] += 1;
            h_fist_fild(&mut *cpu, &mut *mem, addr, ip, fist)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_fcomip(cpu: *mut Cpu, bisp: u32, ip: u32) -> u32 {
        unsafe {
            (*ctx::get().1).stats.fp_helper_calls[1] += 1;
            h_fcomip(&mut *cpu, bisp, ip)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_cvtss2si(cpu: *mut Cpu, kind: u32, v: u32, dst: u32, ip: u32) -> u32 {
        unsafe {
            let (mem, jit) = ctx::get();
            (*jit).stats.fp_helper_calls[2] += 1;
            h_cvtss2si(&mut *cpu, &mut *mem, kind, v, dst, ip)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_mm_op(cpu: *mut Cpu, opdst: u32, kind: u32, v: u32, ip: u32) -> u32 {
        unsafe {
            let (mem, jit) = ctx::get();
            (*jit).stats.fp_helper_calls[3] += 1;
            h_mm_op(&mut *cpu, &mut *mem, opdst, kind, v, ip)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_exec_one(cpu: *mut Cpu, tag: u32) -> u32 {
        unsafe {
            let (mem, jit) = ctx::get();
            let jit = &mut *jit;
            jit.stats.exec_calls += 1;
            if jit.fb_shared.contains(&tag) { jit.stats.aot_exec_calls += 1; }
            let ins: Instruction = *jit.fb_ins(tag);
            jit.stats.exec_by_mn[(ins.code() as usize).min(8191)] += 1;
            if jit.profile {
                let t = rt::now_ns();
                let r = h_exec_one(&mut *cpu, &mut *mem, &ins, &mut jit.pending);
                jit.stats.t_exec_ns += rt::now_ns() - t;
                return r;
            }
            h_exec_one(&mut *cpu, &mut *mem, &ins, &mut jit.pending)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_load_slow(cpu: *mut Cpu, addr: u32, size: u32, ip: u32) -> u32 {
        unsafe {
            (*ctx::get().1).stats.slow_loads += 1;
            h_load_slow(&mut *cpu, &*ctx::get().0, addr, size, ip)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_store_slow(cpu: *mut Cpu, addr: u32, size: u32, val: u32, ip: u32) -> u32 {
        unsafe {
            (*ctx::get().1).stats.slow_stores += 1;
            h_store_slow(&mut *cpu, &mut *ctx::get().0, addr, size, val, ip)
        }
    }
    #[no_mangle]
    pub extern "C" fn jit_mat(cpu: *mut Cpu, lk: u32, la: u32, lb: u32, lr: u32, ip: u32) -> u32 {
        unsafe {
            let jit = &mut *ctx::get().1;
            jit.stats.mat_calls += 1;
            jit.stats.mat_by_kind[(lk as usize).min(63)] += 1;
            if jit.prof_chains {
                *jit.mat_by_site.entry((ip, lk)).or_insert(0) += 1;
            }
            if jit.profile {
                let t = rt::now_ns();
                let r = h_mat(&*cpu, lk, la, lb, lr);
                jit.stats.t_mat_ns += rt::now_ns() - t;
                return r;
            }
            h_mat(&*cpu, lk, la, lb, lr)
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub mod rt {
    use super::*;
    #[link(wasm_import_module = "env")]
    extern "C" {
        /// Compile+instantiate the module, put its `n` exported functions r0..rN into the indirect function
        /// table and write their table indices to `out`. Returns 0 on success.
        fn host_jit_install(ptr: *const u8, len: usize, n: u32, out: *mut u32) -> i32;
        fn host_jit_free(slot: u32);
        fn host_now_ns() -> f64;
    }
    pub fn now_ns() -> f64 {
        unsafe { host_now_ns() }
    }
    pub fn install(bytes: &[u8], n: usize) -> Option<Vec<u32>> {
        let mut out = vec![0u32; n];
        let rc = unsafe { host_jit_install(bytes.as_ptr(), bytes.len(), n as u32, out.as_mut_ptr()) };
        if rc == 0 {
            Some(out)
        } else {
            None
        }
    }
    pub fn free(slot: u32) {
        unsafe { host_jit_free(slot) }
    }
    /// Call a compiled region (a table slot) with the cpu and the chain head to enter.
    #[inline(always)]
    pub unsafe fn call(slot: u32, cpu: *mut Cpu, head: u32) -> u32 {
        let f: extern "C" fn(*mut Cpu, u32) -> u32 = core::mem::transmute(slot as usize);
        f(cpu, head)
    }
}

/// Off wasm32 compiled code cannot run; unit tests plug in a wasmi-backed `Backend` instead.
#[cfg(not(target_arch = "wasm32"))]
pub mod rt {
    use super::*;
    use std::cell::RefCell;
    pub trait Backend {
        fn install(&mut self, bytes: &[u8], n: usize) -> Option<Vec<u32>>;
        fn free(&mut self, slot: u32);
        fn call(&mut self, slot: u32, cpu: &mut Cpu, head: u32) -> u32;
    }
    thread_local! {
        pub static BACKEND: RefCell<Option<Box<dyn Backend>>> = const { RefCell::new(None) };
        static T0: std::time::Instant = std::time::Instant::now();
    }
    pub fn now_ns() -> f64 {
        T0.with(|t0| t0.elapsed().as_secs_f64() * 1e9)
    }
    pub fn install(bytes: &[u8], n: usize) -> Option<Vec<u32>> {
        BACKEND.with(|b| b.borrow_mut().as_mut()?.install(bytes, n))
    }
    pub fn free(slot: u32) {
        BACKEND.with(|b| {
            if let Some(be) = b.borrow_mut().as_mut() {
                be.free(slot)
            }
        })
    }
    pub unsafe fn call(slot: u32, cpu: *mut Cpu, head: u32) -> u32 {
        BACKEND.with(|b| b.borrow_mut().as_mut().expect("no JIT backend").call(slot, &mut *cpu, head))
    }
}

#[cfg(test)]
#[path = "jit_tests.rs"]
mod jit_tests;
