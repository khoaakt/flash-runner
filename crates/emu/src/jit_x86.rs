//! IA-32 -> WebAssembly translator (target independent: it only builds bytes; see jit.rs for the runtime).
//!
//! Input is a *region*: a set of already decoded basic blocks (`NodeIn`). Output is one wasm function
//! `(cpu: i32, entry: i32) -> i32` per region. Design notes (details in docs/JIT.md):
//!
//! * Guest GPRs live in wasm locals for the whole function; they are loaded from `Cpu` at entry and
//!   stored at every exit. Anything that can fault is ordered like the interpreter so that the stored
//!   state is exactly the architectural state at the faulting instruction.
//! * EFLAGS arithmetic bits are *lazy*: the last producer is recorded as the tuple `(lk, a, b, r)` in
//!   locals (`jit_flags::eval` is the single definition of what it means). Consumers inside the same
//!   straight-line chain that see the producer statically get fused compares; everything else falls
//!   back to the `mat` helper. The tuple is stored to `Cpu::lf_*` at exits and folded into `fl` by the
//!   interpreter (`Cpu::lf_flush`).
//! * Nodes with a single predecessor reached by fallthrough are merged into their predecessor's chain
//!   (superblocks); chains are dispatched through a `br_table` loop.
//! * Instructions without an inline translation call back into the interpreter (`exec_one`).

use crate::jit_flags::*;
use crate::wasm_emit::op::*;
use crate::wasm_emit::*;
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};
use std::collections::HashMap;

// ---- region exit status codes (returned by compiled functions) ---------------------------------
/// Continue at `cpu.eip` (all state stored).
pub const S_NEXT: u32 = 0;
/// An `Exit` produced by the interpreter helper is pending (state already in `Cpu`).
pub const S_EXIT: u32 = 1;
/// Page fault: `cpu.eip` = faulting instruction, address/direction in `cpu.jit_fault_*`.
pub const S_SEGV: u32 = 2;
/// #DE: `cpu.eip` = faulting instruction.
pub const S_DIVERR: u32 = 3;
/// `int 0x80`; `cpu.eip` = next instruction.
pub const S_SYSCALL: u32 = 4;
/// `int 0x82`; `cpu.eip` = next instruction.
pub const S_HYPER: u32 = 5;
/// `pause`: the interpreter reports it as `Exit::Budget`; `cpu.eip` = next instruction.
pub const S_PAUSE: u32 = 6;
/// Like `S_NEXT` but a store hit decoded code: the dispatcher must resynchronise its caches before the next region
/// runs (compiled code must not chain onwards).
pub const S_SYNC: u32 = 7;
/// A guest memory access did not pass the inline page-table check (page crossing, protection, decoded code, unmapped):
/// `cpu.eip` = the instruction, which has not executed (registers and flags are exactly as before it); the dispatcher runs
/// that block in the interpreter, which performs the access (or raises its fault) with full semantics.
pub const S_RETRY: u32 = 8;

/// Offsets of the `Cpu` fields compiled code touches, and the addresses of the guest memory tables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub cpu_r: u32,
    pub cpu_eip: u32,
    pub cpu_fl: u32,
    pub cpu_fs_base: u32,
    pub cpu_gs_base: u32,
    pub cpu_lf_kind: u32,
    pub cpu_lf_a: u32,
    pub cpu_lf_b: u32,
    pub cpu_lf_r: u32,
    pub cpu_budget: u32,
    pub cpu_xmm: u32,
    pub cpu_mxcsr: u32,
    /// x87/MMX register file: offset of st[0].mant, st[0].se, the stride between registers, TOP (u8) and the tag word (u16)
    pub cpu_mm0: u32,
    pub cpu_mm_se0: u32,
    pub mm_stride: u32,
    pub cpu_fpu_top: u32,
    pub cpu_fpu_tag: u32,
    pub cpu_fpu_sw: u32,
    pub cpu_fpu_cw: u32,
    pub cpu_fpu_last_ip: u32,
    pub cpu_fpu_last_dp: u32,
    pub cpu_fault: u32,
    pub cpu_aux: u32,
    pub guest_base: u32,
    pub rtab: u32,
    pub wtab: u32,
    /// Direct-mapped region lookup table used for chaining (0 = chaining disabled): entries of four u32
    /// (eip, table slot + 1, chain head, unused), `CACHE_ENTRIES` of them.
    pub cache: u32,
    /// Translate SSE2 integer instructions with wasm SIMD.
    pub simd: bool,
    /// Guest memory accesses that fail the inline check call the checked helpers (old behaviour) instead of leaving the
    /// region with `S_RETRY`: bigger code, but no region exit per slow access (used for regions that retry a lot).
    pub slow_inline: bool,
    /// Performance mode: guest *loads* skip the page-table check (stores keep theirs: code pages, write protection). A load from an unmapped or
    /// PROT_NONE page then returns whatever the guest RAM holds there instead of raising SIGSEGV, and one beyond guest RAM traps the wasm instance.
    pub trust_loads: bool,
    /// Profiling: address of an array of u64 counters (one per chain slot) bumped by the chain's instruction count
    /// at every chain entry; 0 = off.
    pub prof: u32,
}

pub const CACHE_ENTRIES: u32 = 16384;

impl Layout {
    /// Field offsets of the running build's `Cpu`; table addresses are supplied by the caller.
    pub fn for_cpu(guest_base: u32, rtab: u32, wtab: u32, cache: u32) -> Layout {
        use crate::cpu::Cpu;
        use core::mem::offset_of;
        Layout {
            cpu_r: offset_of!(Cpu, r) as u32,
            cpu_eip: offset_of!(Cpu, eip) as u32,
            cpu_fl: offset_of!(Cpu, fl) as u32,
            cpu_fs_base: offset_of!(Cpu, fs_base) as u32,
            cpu_gs_base: offset_of!(Cpu, gs_base) as u32,
            cpu_lf_kind: offset_of!(Cpu, lf_kind) as u32,
            cpu_lf_a: offset_of!(Cpu, lf_a) as u32,
            cpu_lf_b: offset_of!(Cpu, lf_b) as u32,
            cpu_lf_r: offset_of!(Cpu, lf_r) as u32,
            cpu_budget: offset_of!(Cpu, jit_budget) as u32,
            cpu_xmm: offset_of!(Cpu, xmm) as u32,
            cpu_mxcsr: offset_of!(Cpu, mxcsr) as u32,
            cpu_mm0: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, st) + offset_of!(crate::fpu::F80, mant)) as u32,
            cpu_mm_se0: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, st) + offset_of!(crate::fpu::F80, se)) as u32,
            mm_stride: core::mem::size_of::<crate::fpu::F80>() as u32,
            cpu_fpu_top: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, top)) as u32,
            cpu_fpu_tag: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, tag)) as u32,
            cpu_fpu_sw: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, sw)) as u32,
            cpu_fpu_cw: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, cw)) as u32,
            cpu_fpu_last_ip: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, last_ip)) as u32,
            cpu_fpu_last_dp: (offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, last_dp)) as u32,
            cpu_fault: offset_of!(Cpu, jit_fault) as u32,
            cpu_aux: offset_of!(Cpu, jit_aux) as u32,
            guest_base,
            rtab,
            wtab,
            cache,
            simd: true,
            slow_inline: false,
            trust_loads: false,
            prof: 0,
        }
    }
}

pub struct NodeIn<'a> {
    pub start: u32,
    /// Address after the last instruction (fallthrough target).
    pub end: u32,
    pub ins: &'a [Instruction],
    /// Non-zero when the function starting here is replaced by a fast path (`jit_intr`, found by its code bytes).
    pub intrinsic: u8,
    /// The node ends in a direct call to a PIC thunk `mov reg,[esp]; ret` (register + 1): translated as "push the return address, reg = it" and treated as a fall-through.
    pub thunk: u8,
}

/// What the runtime needs to know about a translated region.
#[derive(Default, Clone, Debug)]
pub struct RegionOut {
    /// (guest eip, `entry` argument) for every externally enterable chain head.
    pub heads: Vec<(u32, u32)>,
    pub n_ins: u32,
    pub n_fallback: u32,
    /// chains replaced by an intrinsic fast path
    pub n_intr: u32,
    /// per chain: (head eip, instruction count)
    pub chains: Vec<(u32, u32)>,
    /// profiling builds: every region exit site as (class, static target or 0), in profile-slot order
    pub exits: Vec<(u32, u32)>,
    /// profiling builds: guest address and mnemonic of every `mat` call site
    pub mat_sites: Vec<(u32, String)>,
}

/// Per-mnemonic fallback counters (for EMU_STATS-style reporting of what to translate next).
pub type FbStats = HashMap<M, u32>;

// ---- wasm function layout --------------------------------------------------------------------
const L_CPU: u32 = 0;
const L_PC: u32 = 1; // the `entry` parameter doubles as the dispatch variable
/// i64 scratch locals (declared first so their indices do not depend on the i32 temp count)
const N_T64: u32 = 16;
const L_T64: u32 = 2;
/// v128 scratch locals for the SIMD translation
const N_V128: u32 = 4;
const L_V0: u32 = L_T64 + N_T64;
const L_R0: u32 = L_V0 + N_V128; // ..=+7
const L_LK: u32 = L_R0 + 8;
const L_LA: u32 = L_LK + 1;
const L_LB: u32 = L_LK + 2;
const L_LR: u32 = L_LK + 3;
const L_BUD: u32 = L_LK + 4;
const L_NEXT: u32 = L_LK + 5;
const L_STATUS: u32 = L_LK + 6;
const L_SMC: u32 = L_LK + 7;
const L_X: u32 = L_LK + 8;
const L_TMP0: u32 = L_LK + 9;

// imported function indices
const F_EXEC: u32 = 0;
const F_LOAD: u32 = 1;
const F_STORE: u32 = 2;
const F_MAT: u32 = 3;
const F_FIST_FILD: u32 = 5;
const F_FCOMIP: u32 = 6;
const F_CVTSS2SI: u32 = 7;
const F_MM_OP: u32 = 8;
/// `sys_time(cpu) -> 0 handled (eax set) | 1 exit with S_SYSCALL`: the pure time syscalls answered
/// inline so clock polling never tears down the region chain (see `Kernel::time_syscall`).
const F_SYS: u32 = 4;
const N_IMPORTS: u32 = 4;

// label ids
const LB_RETD: u32 = 0;
const LB_EXIT: u32 = 1;
const LB_FAULT: u32 = 2;
const LB_DISP: u32 = 3;
const LB_RETM: u32 = 4;
const LB_DYN: u32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    /// Nothing known: the dynamic `L_LK` decides.
    Dyn,
    /// `cpu.fl` holds the arithmetic flags (and `L_LK == 0`).
    Mem,
    /// The tuple in LA/LB/LR has this (statically known) kind.
    Lazy(u32),
}

/// A value an instruction handler can read: constant or wasm local.
#[derive(Clone, Copy, Debug)]
enum V {
    C(u32),
    L(u32),
}

#[derive(Clone, Copy)]
enum Dst {
    Reg(Register),
    Mem { addr: u32, size: u8 },
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Flow {
    Fall,
    Stop,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Term {
    /// Block ended without a control transfer (length limit): falls into `end`.
    Fall,
    /// Conditional (jcc/loop/jecxz): taken target + fallthrough.
    Cond(u32),
    Jmp(u32),
    /// `call rel32`: callee; the return site (`end`) is entered through `ret`.
    Call(u32),
    /// ret, indirect jmp/call, int, hlt, ...: no statically known successor.
    Other,
}

fn classify(n: &NodeIn) -> Term {
    let Some(last) = n.ins.last() else { return Term::Fall };
    let near = |l: &Instruction| if l.op0_kind() == OpKind::NearBranch32 { Some(l.near_branch32()) } else { None };
    match last.mnemonic() {
        M::Jmp => near(last).map(Term::Jmp).unwrap_or(Term::Other),
        M::Call if n.thunk != 0 => Term::Fall,
        M::Call => near(last).map(Term::Call).unwrap_or(Term::Other),
        M::Ja | M::Jae | M::Jb | M::Jbe | M::Je | M::Jg | M::Jge | M::Jl | M::Jle | M::Jne | M::Jno | M::Jnp | M::Jns | M::Jo | M::Jp | M::Js | M::Loop | M::Loope | M::Loopne | M::Jecxz => {
            near(last).map(Term::Cond).unwrap_or(Term::Other)
        }
        M::Ret | M::Int | M::Int3 | M::Hlt | M::Ud2 | M::Jcxz | M::Jrcxz | M::Sysenter | M::Pause => Term::Other,
        _ => Term::Fall,
    }
}

/// True if the block ends in a direct `call` (its taken edge is the callee).
pub fn ends_in_call(n: &NodeIn) -> bool {
    matches!(classify(n), Term::Call(_))
}

/// Static successors of a decoded block: (branch target / callee, fallthrough or return site).
pub fn successors(n: &NodeIn) -> (Option<u32>, Option<u32>) {
    match classify(n) {
        Term::Fall => (None, Some(n.end)),
        Term::Cond(t) => (Some(t), Some(n.end)),
        Term::Jmp(t) => (Some(t), None),
        Term::Call(t) => (Some(t), Some(n.end)),
        Term::Other => (None, None),
    }
}

pub fn cc_of(mn: M) -> u8 {
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

fn reg_size(r: Register) -> u8 {
    if r.is_gpr32() {
        4
    } else if r.is_gpr16() || r.is_segment_register() {
        2
    } else {
        1
    }
}

fn mask_of(sz: u8) -> u32 {
    match sz {
        1 => 0xff,
        2 => 0xffff,
        _ => 0xffff_ffff,
    }
}
fn sign_of(sz: u8) -> u32 {
    match sz {
        1 => 0x80,
        2 => 0x8000,
        _ => 0x8000_0000,
    }
}

/// Function-local translation state.
struct Fx<'a> {
    lay: &'a Layout,
    c: Code,
    labels: Vec<u32>,
    next_label: u32,
    tmp_top: u32,
    tmp_max: u32,
    st: St,
    /// Bitmask of guest registers written anywhere in the function.
    written: u32,
    had_store: bool,
    fb: &'a mut dyn FnMut(&Instruction) -> u32,
    stats: &'a mut FbStats,
    n_fb: u32,
    n_ins: u32,
    n_intr: u32,
    /// flags possibly read after each instruction of the chain being emitted (see `liveness`)
    live_after: Vec<u32>,
    k: usize,
    live: u32,
    /// guest registers written by translated code anywhere in the region (known from the first pass); only
    /// those can differ from `Cpu` memory and have to be stored around interpreter calls
    known_written: u32,
    ifac: iced_x86::InstructionInfoFactory,
    /// (guest address, chain index) of in-region return sites (the instruction after a `call`)
    ret_sites: Vec<(u32, u32)>,
    /// start address of the node merged after the one being emitted (a `jmp` to it falls through)
    inline_next: Option<u32>,
    /// what kind of control transfer the instruction being translated is (for exit profiling)
    exit_class: u32,
    exit_sites: Vec<(u32, u32)>,
    exit_slot0: u32,
    /// the chain being emitted and the wasm labels that end just before each chain's code
    cur_chain: usize,
    /// label of the wasm `loop` wrapped around the current chain when its code branches back to its own head
    cur_loop: Option<u32>,
    /// register + 1 written by the thunk call that ends the node being emitted (0 = none)
    node_thunk: u8,
    head_labels: Vec<u32>,
    used_retm: bool,
    /// MMX translation: TOP = 0 and no empty tag is known to hold here (set by the first MMX instruction of a straight-line run)
    mmx_known: bool,
    /// guest address / mnemonic of the instruction being translated, and the `mat` call sites seen (profiling)
    cur_ip: u32,
    cur_mn: M,
    mat_sites: Vec<(u32, M)>,
    /// the instruction being translated has already changed registers, flags or memory: a later slow access cannot
    /// simply restart it (`S_RETRY`) and uses the inline checked helper
    ins_dirty: bool,
    /// a store to (local, size) was already guarded before the flags/registers of this instruction were touched
    guarded: Option<(u32, u8)>,
    /// MM registers (bit i) whose x87 sign/exponent field is known to be 0xffff with a 'special' tag
    mm_tagged: u8,
}

impl<'a> Fx<'a> {
    // ---- labels ---------------------------------------------------------------------------
    fn open(&mut self, id: u32) {
        self.labels.push(id);
    }
    fn close(&mut self) {
        self.labels.pop();
        self.c.end();
    }
    fn new_label(&mut self) -> u32 {
        self.next_label += 1;
        LB_DYN + self.next_label
    }
    fn depth(&self, id: u32) -> u32 {
        let pos = self.labels.iter().rposition(|&l| l == id).expect("label not open");
        (self.labels.len() - 1 - pos) as u32
    }
    fn br(&mut self, id: u32) {
        let d = self.depth(id);
        self.c.br(d);
    }
    /// `if` (consumes an i32 condition) with a tracked, anonymous label.
    fn if_(&mut self, bt: u8) {
        self.c.if_(bt);
        self.labels.push(u32::MAX);
    }
    fn else_(&mut self) {
        self.c.else_();
    }

    // ---- temporaries ----------------------------------------------------------------------
    fn tmp(&mut self) -> u32 {
        let t = L_TMP0 + self.tmp_top;
        self.tmp_top += 1;
        if self.tmp_top > self.tmp_max {
            self.tmp_max = self.tmp_top;
        }
        t
    }

    // ---- guest registers --------------------------------------------------------------------
    fn r32(n: usize) -> u32 {
        L_R0 + n as u32
    }

    /// Push the zero-extended value of a GPR.
    fn rget(&mut self, r: Register) {
        let n = r.number();
        if r.is_gpr32() {
            self.c.lget(Self::r32(n));
        } else if r.is_gpr16() {
            self.c.lget(Self::r32(n)).i32c(0xffff).op(I32_AND);
        } else if n < 4 {
            self.c.lget(Self::r32(n)).i32c(0xff).op(I32_AND);
        } else {
            self.c.lget(Self::r32(n - 4)).i32c(8).op(I32_SHR_U).i32c(0xff).op(I32_AND);
        }
    }

    /// Pop a value and write it to a GPR (partial registers merge).
    fn rset(&mut self, r: Register) {
        self.ins_dirty = true;
        let n = r.number();
        if r.is_gpr32() {
            self.written |= 1 << n;
            self.c.lset(Self::r32(n));
        } else if r.is_gpr16() {
            self.written |= 1 << n;
            self.c.i32c(0xffff).op(I32_AND).lget(Self::r32(n)).i32c(0xffff_0000u32 as i32).op(I32_AND).op(I32_OR).lset(Self::r32(n));
        } else if n < 4 {
            self.written |= 1 << n;
            self.c.i32c(0xff).op(I32_AND).lget(Self::r32(n)).i32c(0xffff_ff00u32 as i32).op(I32_AND).op(I32_OR).lset(Self::r32(n));
        } else {
            self.written |= 1 << (n - 4);
            self.c.i32c(0xff).op(I32_AND).i32c(8).op(I32_SHL).lget(Self::r32(n - 4)).i32c(0xffff_00ffu32 as i32).op(I32_AND).op(I32_OR).lset(Self::r32(n - 4));
        }
    }

    fn rset32(&mut self, n: usize) {
        self.ins_dirty = true;
        self.written |= 1 << n;
        self.c.lset(Self::r32(n));
    }

    fn push_v(&mut self, v: V) {
        match v {
            V::C(c) => {
                self.c.u32c(c);
            }
            V::L(l) => {
                self.c.lget(l);
            }
        }
    }

    /// Value on the stack -> a V (constant folding is not attempted).
    fn to_tmp(&mut self) -> V {
        let t = self.tmp();
        self.c.lset(t);
        V::L(t)
    }

    // ---- exits -----------------------------------------------------------------------------
    /// Leave through the common exit with a statically known next eip.
    /// Continue with chain `ci` of this region. A later chain is reached with a plain forward branch (its code starts
    /// right where the label's block ends); an earlier one goes back through the dispatch loop.
    fn goto_chain(&mut self, ci: u32) {
        if ci as usize > self.cur_chain {
            let l = self.head_labels[ci as usize];
            self.br(l);
        } else if ci as usize == self.cur_chain && self.cur_loop.is_some() {
            // a branch back to the head of the chain being emitted: a plain loop, no dispatch
            let l = self.cur_loop.unwrap();
            self.br(l);
        } else {
            self.c.u32c(ci).lset(L_PC);
            self.br(LB_DISP);
        }
    }

    /// Leave with `L_NEXT` already set.
    fn exit_pending(&mut self, class: u32, status: u32) {
        self.prof_exit(class, 0);
        self.c.u32c(status).lset(L_STATUS);
        self.br(LB_EXIT);
    }

    /// The return address in `L_NEXT` against the (address-sorted) return sites; a hit continues in the site's chain,
    /// a miss falls out of the search.
    fn ret_search(&mut self, sites: &[(u32, u32)]) {
        if sites.len() <= 3 {
            for &(addr, ci) in sites {
                self.c.lget(L_NEXT).ip(addr).op(I32_EQ);
                self.if_(BT_EMPTY);
                self.c.u32c(ci).lset(L_PC);
                self.br(LB_DISP);
                self.close();
            }
            return;
        }
        let mid = sites.len() / 2;
        self.c.lget(L_NEXT).ip(sites[mid].0).op(I32_LT_U);
        self.if_(BT_EMPTY);
        self.ret_search(&sites[..mid]);
        self.else_();
        self.ret_search(&sites[mid..]);
        self.close();
    }

    fn exit_to(&mut self, eip: u32, status: u32) {
        self.prof_exit(self.exit_class, eip);
        self.c.ip(eip).lset(L_NEXT).u32c(status).lset(L_STATUS);
        self.br(LB_EXIT);
    }
    /// Leave with the next eip on the wasm stack.
    fn exit_dyn(&mut self, status: u32) {
        let class = if self.exit_class == 0 { 3 } else { self.exit_class };
        self.prof_exit(class, 0);
        self.c.lset(L_NEXT).u32c(status).lset(L_STATUS);
        self.br(LB_EXIT);
    }
    /// Profiling builds: count exits by class (0 jump/fallthrough, 1 call, 2 ret, 3 indirect, 4 other).
    fn prof_exit(&mut self, class: u32, to: u32) {
        if self.lay.prof != 0 {
            let a = self.lay.prof + 8 * class;
            self.c.i32c(0).i32c(0).mem(I64_LOAD, a).i64c(1).op(I64_ADD).mem(I64_STORE, a);
            let k = self.exit_sites.len() as u32;
            self.exit_sites.push((class, to));
            let a = self.lay.prof + 8 * (self.exit_slot0 + k);
            self.c.i32c(0).i32c(0).mem(I64_LOAD, a).i64c(1).op(I64_ADD).mem(I64_STORE, a);
        }
    }

    // ---- flags -----------------------------------------------------------------------------
    /// Record a flag-producing result. Operand locals must already hold masked values.
    fn rec(&mut self, lk: u32, a: V, b: V, r: V) {
        self.ins_dirty = true;
        self.c.u32c(lk).lset(L_LK);
        self.push_v(a);
        self.c.lset(L_LA);
        self.push_v(b);
        self.c.lset(L_LB);
        self.push_v(r);
        self.c.lset(L_LR);
        self.st = St::Lazy(lk);
    }

    /// Flags now live in `cpu.fl` (after an interpreter call).
    fn set_mem_state(&mut self) {
        self.ins_dirty = true;
        self.c.i32c(0).lset(L_LK);
        self.st = St::Mem;
    }

    /// Push the six arithmetic flag bits (EFLAGS positions) for the current state.
    fn note_mat_site(&mut self) {
        if self.lay.prof != 0 {
            self.mat_sites.push((self.cur_ip, self.cur_mn));
        }
    }

    fn flag_bits(&mut self) {
        match self.st {
            St::Mem => {
                self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).i32c(0x8d5).op(I32_AND);
            }
            St::Lazy(lk) if lk_op(lk) == OP_FULL => {
                self.c.lget(L_LR);
            }
            St::Lazy(lk) => {
                self.c.lget(L_CPU).u32c(lk).lget(L_LA).lget(L_LB).lget(L_LR).ip(self.cur_ip).call(F_MAT);
                self.note_mat_site();
            }
            St::Dyn => {
                self.c.lget(L_CPU).lget(L_LK).lget(L_LA).lget(L_LB).lget(L_LR).ip(self.cur_ip).call(F_MAT);
                self.note_mat_site();
            }
        }
    }

    /// Push `value` sign-extended from `sz` bytes (value is masked to `sz`).
    fn sx(&mut self, l: u32, sz: u8) {
        self.c.lget(l);
        match sz {
            1 => {
                self.c.op(I32_EXTEND8_S);
            }
            2 => {
                self.c.op(I32_EXTEND16_S);
            }
            _ => {}
        }
    }

    /// Push a 0/1 for the sign bit of masked local `l`.
    fn sign_bit(&mut self, l: u32, sz: u8) {
        match sz {
            4 => {
                self.c.lget(l).i32c(31).op(I32_SHR_U);
            }
            2 => {
                self.c.lget(l).i32c(15).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
            _ => {
                self.c.lget(l).i32c(7).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
        }
    }

    /// Push 0/1: PF (even parity of the low byte) of local `l`.
    fn parity(&mut self, l: u32) {
        self.c.lget(l).i32c(0xff).op(I32_AND).op(I32_POPCNT).i32c(1).op(I32_AND).op(I32_EQZ);
    }

    /// Push the carry flag a SHL/SHR/SAR tuple (a = value, b = count 1..31) describes.
    fn shift_cf(&mut self, op: u32, sz: u8) {
        let (a, b) = (L_LA, L_LB);
        match op {
            OP_SHR => {
                self.c.lget(a).lget(b).i32c(1).op(I32_SUB).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
            OP_SAR => {
                self.sx(a, sz);
                self.c.lget(b).i32c(1).op(I32_SUB).op(I32_SHR_S).i32c(1).op(I32_AND);
            }
            _ => {
                self.c.lget(a).lget(b).i32c(1).op(I32_SUB).op(I32_SHL).i32c(sz as i32 * 8 - 1).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
        }
    }

    /// Push 0/1 for the base condition (cc >> 1) given a Lazy state with a fused implementation.
    /// Returns false (nothing emitted) when there is none.
    fn fused_base(&mut self, op: u32, sz: u8, base: u8) -> bool {
        let (a, b, r) = (L_LA, L_LB, L_LR);
        let sbit = sign_of(sz);
        match (op, base) {
            // ---- SUB / CMP ----
            (OP_SUB, 0) => {
                self.c.lget(a).lget(b).op(I32_XOR).lget(a).lget(r).op(I32_XOR).op(I32_AND).u32c(sbit).op(I32_AND).op(I32_EQZ).op(I32_EQZ);
            }
            (OP_SUB, 1) => {
                self.c.lget(a).lget(b).op(I32_LT_U);
            }
            (OP_SUB, 3) => {
                self.c.lget(a).lget(b).op(I32_LE_U);
            }
            (OP_SUB, 6) | (OP_SUB, 7) => {
                self.sx(a, sz);
                self.sx(b, sz);
                self.c.op(if base == 6 { I32_LT_S } else { I32_LE_S });
            }
            // ---- ADD ----
            (OP_ADD, 0) => {
                self.c.lget(a).lget(r).op(I32_XOR).lget(b).lget(r).op(I32_XOR).op(I32_AND).u32c(sbit).op(I32_AND).op(I32_EQZ).op(I32_EQZ);
            }
            (OP_ADD, 1) => {
                self.c.lget(r).lget(a).op(I32_LT_U);
            }
            (OP_ADD, 3) => {
                self.c.lget(r).lget(a).op(I32_LT_U).lget(r).op(I32_EQZ).op(I32_OR);
            }
            (OP_ADD, 6) | (OP_ADD, 7) | (OP_INC, 6) | (OP_INC, 7) | (OP_DEC, 6) | (OP_DEC, 7) => {
                // SF != OF  (|| ZF for LE)
                self.sign_bit(r, sz);
                match op {
                    OP_ADD => {
                        self.c.lget(a).lget(r).op(I32_XOR).lget(b).lget(r).op(I32_XOR).op(I32_AND).u32c(sbit).op(I32_AND).op(I32_EQZ).op(I32_EQZ);
                    }
                    OP_INC => {
                        self.c.lget(r).u32c(sbit).op(I32_EQ);
                    }
                    _ => {
                        self.c.lget(a).u32c(sbit).op(I32_EQ);
                    }
                }
                self.c.op(I32_XOR);
                if base == 7 {
                    self.c.lget(r).op(I32_EQZ).op(I32_OR);
                }
            }
            // ---- INC / DEC (CF preserved in b) ----
            (OP_INC, 0) => {
                self.c.lget(r).u32c(sbit).op(I32_EQ);
            }
            (OP_DEC, 0) => {
                self.c.lget(a).u32c(sbit).op(I32_EQ);
            }
            (OP_INC, 1) | (OP_DEC, 1) => {
                self.c.lget(b);
            }
            (OP_INC, 3) | (OP_DEC, 3) => {
                self.c.lget(b).lget(r).op(I32_EQZ).op(I32_OR);
            }
            // ---- LOGIC ----
            (OP_LOGIC, 0) | (OP_LOGIC, 1) => {
                self.c.i32c(0);
            }
            (OP_LOGIC, 3) => {
                self.c.lget(r).op(I32_EQZ);
            }
            (OP_LOGIC, 6) => {
                self.sign_bit(r, sz);
            }
            (OP_LOGIC, 7) => {
                self.sx(r, sz);
                self.c.i32c(0).op(I32_LE_S);
            }
            // ---- SHL / SHR / SAR: a = value, b = count (1..31), r = result ----
            (OP_SHR, 0) => {
                self.c.lget(a).u32c(sbit).op(I32_AND).op(I32_EQZ).op(I32_EQZ);
            }
            (OP_SAR, 0) => {
                self.c.i32c(0);
            }
            (OP_SHL, 0) => {
                // OF = SF != CF
                self.sign_bit(r, sz);
                self.shift_cf(op, sz);
                self.c.op(I32_XOR);
            }
            (OP_SHL | OP_SHR | OP_SAR, 1) => {
                self.shift_cf(op, sz);
            }
            (OP_SHL | OP_SHR | OP_SAR, 3) => {
                self.shift_cf(op, sz);
                self.c.lget(r).op(I32_EQZ).op(I32_OR);
            }
            (OP_SHL | OP_SHR | OP_SAR, 6) | (OP_SHL | OP_SHR | OP_SAR, 7) => {
                // SF != OF (|| ZF for LE); OF = SF ^ CF for SHL (so the first part is CF), (a & sign) for SHR, 0 for SAR
                match op {
                    OP_SHL => {
                        self.shift_cf(op, sz);
                    }
                    OP_SHR => {
                        self.sign_bit(r, sz);
                        self.c.lget(a).u32c(sbit).op(I32_AND).op(I32_EQZ).op(I32_EQZ).op(I32_XOR);
                    }
                    _ => {
                        self.sign_bit(r, sz);
                    }
                }
                if base == 7 {
                    self.c.lget(r).op(I32_EQZ).op(I32_OR);
                }
            }
            // ---- IMUL: a = overflow (0/1), r = result ----
            (OP_IMUL, 0) | (OP_IMUL, 1) => {
                self.c.lget(a).i32c(1).op(I32_AND);
            }
            (OP_IMUL, 3) => {
                self.c.lget(a).i32c(1).op(I32_AND).lget(r).op(I32_EQZ).op(I32_OR);
            }
            (OP_IMUL, 6) | (OP_IMUL, 7) => {
                self.sign_bit(r, sz);
                self.c.lget(a).i32c(1).op(I32_AND).op(I32_XOR);
                if base == 7 {
                    self.c.lget(r).op(I32_EQZ).op(I32_OR);
                }
            }
            // ---- shared by all of the above ----
            (OP_SUB | OP_ADD | OP_INC | OP_DEC | OP_LOGIC | OP_SHL | OP_SHR | OP_SAR | OP_IMUL, 2) => {
                self.c.lget(r).op(I32_EQZ);
            }
            (OP_SUB | OP_ADD | OP_INC | OP_DEC | OP_LOGIC | OP_SHL | OP_SHR | OP_SAR | OP_IMUL, 4) => {
                self.sign_bit(r, sz);
            }
            (OP_SUB | OP_ADD | OP_INC | OP_DEC | OP_LOGIC | OP_SHL | OP_SHR | OP_SAR | OP_IMUL, 5) => {
                self.parity(r);
            }
            _ => return false,
        }
        true
    }

    /// Push 0/1 for condition code `cc` (Intel order) according to the current flag state.
    fn cond(&mut self, cc: u8) {
        let base = cc >> 1;
        let inv = cc & 1 != 0;
        let mut done = false;
        match self.st {
            St::Lazy(lk) => {
                done = self.fused_base(lk_op(lk), lk_size(lk), base);
            }
            St::Dyn => {
                self.cond_dyn(base);
                done = true;
            }
            St::Mem => {}
        }
        if !done {
            self.cond_generic(base);
        }
        if inv {
            self.c.op(I32_EQZ);
        }
    }

    /// Would `fused_base` emit code for this kind and base condition? (Nothing is left behind.)
    fn has_fused(&mut self, op: u32, sz: u8, base: u8) -> bool {
        let saved = std::mem::take(&mut self.c);
        let ok = self.fused_base(op, sz, base);
        self.c = saved;
        ok
    }

    /// Flags of unknown provenance (a chain entered with whatever the previous chain left): test the kind word at run
    /// time for the common ones and use the inline form, and only call the `mat` helper for the rest.
    fn cond_dyn(&mut self, base: u8) {
        let kinds = [lk(OP_LOGIC, 4), lk(OP_SUB, 4), lk(OP_SHR, 4), lk(OP_SAR, 4), lk(OP_ADD, 4), lk(OP_DEC, 4), lk(OP_INC, 4)];
        let mut opened = 0;
        for k in kinds {
            if !self.has_fused(lk_op(k), lk_size(k), base) {
                continue;
            }
            self.c.lget(L_LK).u32c(k).op(I32_EQ);
            self.if_(I32);
            let ok = self.fused_base(lk_op(k), lk_size(k), base);
            debug_assert!(ok);
            self.else_();
            opened += 1;
        }
        // an explicit flag word (comis, lahf-style results): the bits are in L_LR
        self.c.lget(L_LK).u32c(lk(OP_FULL, 4)).op(I32_EQ);
        self.if_(I32);
        self.cond_from_bits(L_LR, base);
        self.else_();
        opened += 1;
        // flags already in cpu.fl: no helper call needed
        self.c.lget(L_LK).op(I32_EQZ);
        self.if_(I32);
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).i32c(0x8d5).op(I32_AND);
        let t = self.tmp();
        self.c.lset(t);
        self.cond_from_bits(t, base);
        self.else_();
        self.cond_generic(base);
        self.close();
        for _ in 0..opened {
            self.close();
        }
    }

    /// Condition `base` of the current state through the six flag bits (calls `mat` when they are not in memory).
    fn cond_generic(&mut self, base: u8) {
        self.flag_bits();
        let t = self.tmp();
        self.c.lset(t);
        self.cond_from_bits(t, base);
    }

    /// Push 0/1 for base condition `base` from the six flag bits in local `t`.
    fn cond_from_bits(&mut self, t: u32, base: u8) {
        match base {
            0 => {
                self.c.lget(t).i32c(11).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
            1 => {
                self.c.lget(t).i32c(1).op(I32_AND);
            }
            2 => {
                self.c.lget(t).i32c(6).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
            3 => {
                self.c.lget(t).i32c(0x41).op(I32_AND).op(I32_EQZ).op(I32_EQZ);
            }
            4 => {
                self.c.lget(t).i32c(7).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
            5 => {
                self.c.lget(t).i32c(2).op(I32_SHR_U).i32c(1).op(I32_AND);
            }
            6 => {
                self.c.lget(t).i32c(7).op(I32_SHR_U).lget(t).i32c(11).op(I32_SHR_U).op(I32_XOR).i32c(1).op(I32_AND);
            }
            _ => {
                self.c.lget(t).i32c(7).op(I32_SHR_U).lget(t).i32c(11).op(I32_SHR_U).op(I32_XOR).lget(t).i32c(6).op(I32_SHR_U).op(I32_OR).i32c(1).op(I32_AND);
            }
        }
    }

    /// Value of CF (0/1) for the current state, used by adc/sbb-style consumers.
    fn carry(&mut self) {
        self.cond(2);
    }

    // ---- memory ----------------------------------------------------------------------------
    fn mem_op_for(size: u8, load: bool) -> u8 {
        match (size, load) {
            (1, true) => I32_LOAD8_U,
            (2, true) => I32_LOAD16_U,
            (_, true) => I32_LOAD,
            (1, false) => I32_STORE8,
            (2, false) => I32_STORE16,
            (_, false) => I32_STORE,
        }
    }

    /// Condition (0/1) on the wasm stack: the access of `size` bytes at local `addr` may use the unchecked path.
    /// Table entries: bit 0 = the page qualifies, bit 1 = the next page qualifies too (`Memory::upd_tab`), so a
    /// bit-1 page needs no end-of-page test; on the last page of a region the access must not cross the page end.
    fn fast_cond(&mut self, addr: u32, size: u8, tab: u32) {
        if self.lay.trust_loads && tab == self.lay.rtab {
            self.c.i32c(1);
            return;
        }
        self.c.lget(addr).i32c(12).op(I32_SHR_U).reloc_mem(I32_LOAD8_U, tab, if tab == self.lay.rtab { 2 } else { 3 });
        if size > 1 {
            let t = self.tmp();
            self.c.lset(t).lget(t).i32c(2).op(I32_AND);
            self.if_(I32);
            self.c.i32c(1);
            self.else_();
            self.c.lget(t).lget(addr).i32c(0xfff).op(I32_AND).i32c(0x1000 - size as i32 + 1).op(I32_LT_U).op(I32_AND);
            self.close();
        }
    }

    /// Leave the region (`S_RETRY`) unless the access of `size` bytes at local `addr` may use the unchecked path
    /// (same condition as `fast_cond`, laid out so the common case is one table load and one bit test).
    fn check_or_retry(&mut self, addr: u32, size: u8, tab: u32, ip: u32) {
        if self.lay.trust_loads && tab == self.lay.rtab {
            return;
        }
        self.c.lget(addr).i32c(12).op(I32_SHR_U).reloc_mem(I32_LOAD8_U, tab, if tab == self.lay.rtab { 2 } else { 3 });
        if size == 1 {
            self.c.op(I32_EQZ);
            self.if_(BT_EMPTY);
            self.retry_exit(ip);
            self.close();
            return;
        }
        let t = self.tmp();
        self.c.lset(t).lget(t).i32c(2).op(I32_AND).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.c.lget(t).lget(addr).i32c(0xfff).op(I32_AND).i32c(0x1000 - size as i32 + 1).op(I32_LT_U).op(I32_AND).op(I32_EQZ);
        self.if_(BT_EMPTY);
        self.retry_exit(ip);
        self.close();
        self.close();
    }

    /// Check a store to (`addr`, `size`) now, before the instruction changes flags or registers; the store itself
    /// (`write_dst` / `store_mem`) then needs no check. Without it a read-modify-write instruction would reach its store
    /// after recording the flags and could only use the inline helper.
    fn guard_store(&mut self, addr: u32, size: u8, ip: u32) {
        if self.lay.slow_inline || self.ins_dirty || self.guarded.is_some() {
            return;
        }
        self.check_or_retry(addr, size, self.lay.wtab, ip);
        self.guarded = Some((addr, size));
    }

    /// Like `dst_of` for an instruction that always writes its destination.
    fn dst_of_w(&mut self, ins: &Instruction, i: u32, ip: u32) -> Dst {
        let d = self.dst_of(ins, i);
        if let Dst::Mem { addr, size } = d {
            self.guard_store(addr, size, ip);
        }
        d
    }

    /// Leave the region before the current instruction so that the interpreter executes it (see `S_RETRY`).
    fn retry_exit(&mut self, ip: u32) {
        self.c.ip(ip).lset(L_NEXT).u32c(S_RETRY).lset(L_STATUS);
        self.br(LB_EXIT);
    }

    /// Guest load; pushes the zero-extended value. A slow access leaves the region (`S_RETRY`), or with
    /// `Layout::slow_inline` goes through the checked helper, whose faults leave through the common fault exit.
    fn load_mem(&mut self, addr: u32, size: u8, ip: u32) {
        if !self.lay.slow_inline && !self.ins_dirty {
            self.check_or_retry(addr, size, self.lay.rtab, ip);
            self.c.lget(addr).reloc_mem(Self::mem_op_for(size, true), self.lay.guest_base, 1);
            return;
        }
        self.fast_cond(addr, size, self.lay.rtab);
        self.if_(I32);
        self.c.lget(addr).reloc_mem(Self::mem_op_for(size, true), self.lay.guest_base, 1);
        self.else_();
        self.c.lget(L_CPU).lget(addr).i32c(size as i32).ip(ip).call(F_LOAD);
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fault);
        self.if_(BT_EMPTY);
        self.br(LB_FAULT);
        self.close();
        self.close();
    }

    /// Guest store of local `val` (low `size` bytes) to local `addr`.
    fn store_mem(&mut self, addr: u32, val: V, size: u8, ip: u32) {
        if self.guarded == Some((addr, size)) {
            // the check was emitted before the instruction changed any state (`guard_store`)
            self.guarded = None;
            self.c.lget(addr);
            self.push_v(val);
            self.c.reloc_mem(Self::mem_op_for(size, false), self.lay.guest_base, 1);
            self.ins_dirty = true;
            return;
        }
        if !self.lay.slow_inline && !self.ins_dirty {
            self.check_or_retry(addr, size, self.lay.wtab, ip);
            self.c.lget(addr);
            self.push_v(val);
            self.c.reloc_mem(Self::mem_op_for(size, false), self.lay.guest_base, 1);
            self.ins_dirty = true;
            return;
        }
        self.fast_cond(addr, size, self.lay.wtab);
        self.ins_dirty = true;
        self.had_store = true;
        self.if_(BT_EMPTY);
        self.c.lget(addr);
        self.push_v(val);
        self.c.reloc_mem(Self::mem_op_for(size, false), self.lay.guest_base, 1);
        self.else_();
        self.c.lget(L_CPU).lget(addr).i32c(size as i32);
        self.push_v(val);
        self.c.ip(ip).call(F_STORE).lset(L_X);
        self.c.lget(L_X);
        self.if_(BT_EMPTY);
        self.c.lget(L_X).i32c(1).op(I32_EQ);
        self.if_(BT_EMPTY);
        self.br(LB_FAULT);
        self.else_();
        self.c.i32c(1).lset(L_SMC);
        self.close();
        self.close();
        self.close();
    }

    /// After an instruction containing stores: leave the region if a store hit decoded code.
    fn smc_check_ip(&mut self, next: u32) {
        if !self.c.record_relocations { self.smc_check(V::C(next)); return; }
        if self.had_store {
            self.c.ip(next);
            let value = self.to_tmp();
            self.smc_check(value);
        }
    }

    fn store_return_ip(&mut self, sp: u32, next: u32, ip: u32) {
        if !self.c.record_relocations { self.store_mem(sp, V::C(next), 4, ip); return; }
        self.c.ip(next);
        let value = self.to_tmp();
        self.store_mem(sp, value, 4, ip);
    }

    fn smc_check(&mut self, next_ip: V) {
        if !self.had_store {
            return;
        }
        self.had_store = false;
        self.c.lget(L_SMC);
        self.if_(BT_EMPTY);
        self.push_v(next_ip);
        self.exit_dyn(S_SYNC);
        self.close();
    }

    /// Linear address of the instruction's memory operand into a temp. `None` => unsupported addressing.
    fn ea(&mut self, ins: &Instruction, with_seg: bool) -> Option<u32> {
        let base = ins.memory_base();
        let index = ins.memory_index();
        if (base != Register::None && !base.is_gpr32()) || (index != Register::None && !index.is_gpr32()) {
            return None;
        }
        let mut have = false;
        if base != Register::None {
            self.c.lget(Self::r32(base.number()));
            have = true;
        }
        if index != Register::None {
            self.c.lget(Self::r32(index.number()));
            let sc = ins.memory_index_scale();
            if sc > 1 {
                self.c.i32c(sc.trailing_zeros() as i32).op(I32_SHL);
            }
            if have {
                self.c.op(I32_ADD);
            }
            have = true;
        }
        let disp = ins.memory_displacement32();
        if disp != 0 || !have {
            self.c.u32c(disp);
            if have {
                self.c.op(I32_ADD);
            }
            have = true;
        }
        let _ = have;
        if with_seg {
            match ins.memory_segment() {
                Register::FS => {
                    self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fs_base).op(I32_ADD);
                }
                Register::GS => {
                    self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_gs_base).op(I32_ADD);
                }
                _ => {}
            }
        }
        let t = self.tmp();
        self.c.lset(t);
        Some(t)
    }

    // ---- operands --------------------------------------------------------------------------
    fn opsz(ins: &Instruction, i: u32) -> u8 {
        match ins.op_kind(i) {
            OpKind::Register => reg_size(ins.op_register(i)),
            OpKind::Memory => ins.memory_size().size() as u8,
            _ => 4,
        }
    }

    /// Can the operand forms of `ins` (operands 0..n) be handled inline?
    fn operands_ok(ins: &Instruction) -> bool {
        let is_lea = ins.mnemonic() == M::Lea;
        for i in 0..ins.op_count() {
            match ins.op_kind(i) {
                OpKind::Register => {
                    let r = ins.op_register(i);
                    if !(r.is_gpr32() || r.is_gpr16() || r.is_gpr8()) {
                        return false;
                    }
                }
                OpKind::Memory => {
                    let b = ins.memory_base();
                    let x = ins.memory_index();
                    if (b != Register::None && !b.is_gpr32()) || (x != Register::None && !x.is_gpr32()) {
                        return false;
                    }
                    let s = ins.memory_size().size();
                    if !is_lea && !(s == 1 || s == 2 || s == 4) {
                        return false;
                    }
                }
                OpKind::Immediate8 | OpKind::Immediate8_2nd | OpKind::Immediate16 | OpKind::Immediate32 | OpKind::Immediate8to16 | OpKind::Immediate8to32 | OpKind::NearBranch32 => {}
                _ => return false,
            }
        }
        true
    }

    fn imm_of(ins: &Instruction, i: u32) -> u32 {
        match ins.op_kind(i) {
            OpKind::Immediate8 => ins.immediate8() as u32,
            OpKind::Immediate8_2nd => ins.immediate8_2nd() as u32,
            OpKind::Immediate16 => ins.immediate16() as u32,
            OpKind::Immediate32 => ins.immediate32(),
            OpKind::Immediate8to16 => ins.immediate8to16() as u16 as u32,
            OpKind::Immediate8to32 => ins.immediate8to32() as u32,
            OpKind::NearBranch32 => ins.near_branch32(),
            _ => 0,
        }
    }

    /// Read a source operand (register / immediate / memory) as a V holding the zero-extended value.
    fn read_src(&mut self, ins: &Instruction, i: u32, ip: u32) -> V {
        match ins.op_kind(i) {
            OpKind::Register => {
                let r = ins.op_register(i);
                if r.is_gpr32() {
                    V::L(Self::r32(r.number()))
                } else {
                    self.rget(r);
                    self.to_tmp()
                }
            }
            OpKind::Memory => {
                let sz = ins.memory_size().size() as u8;
                let a = self.ea(ins, true).unwrap();
                self.load_mem(a, sz, ip);
                self.to_tmp()
            }
            _ => V::C(Self::imm_of(ins, i)),
        }
    }

    /// Destination operand descriptor (computes the effective address for memory).
    fn dst_of(&mut self, ins: &Instruction, i: u32) -> Dst {
        match ins.op_kind(i) {
            OpKind::Register => Dst::Reg(ins.op_register(i)),
            _ => {
                let size = ins.memory_size().size() as u8;
                let addr = self.ea(ins, true).unwrap();
                Dst::Mem { addr, size }
            }
        }
    }

    fn read_dst(&mut self, d: Dst, ip: u32) -> V {
        match d {
            Dst::Reg(r) => {
                if r.is_gpr32() {
                    V::L(Self::r32(r.number()))
                } else {
                    self.rget(r);
                    self.to_tmp()
                }
            }
            Dst::Mem { addr, size } => {
                self.load_mem(addr, size, ip);
                self.to_tmp()
            }
        }
    }

    fn write_dst(&mut self, d: Dst, v: V, ip: u32) {
        match d {
            Dst::Reg(r) => {
                self.push_v(v);
                self.rset(r);
            }
            Dst::Mem { addr, size } => self.store_mem(addr, v, size, ip),
        }
    }

    // ---- fallback to the interpreter -------------------------------------------------------
    fn store_regs(&mut self, mask: u32) {
        for i in 0..8u32 {
            if mask & (1 << i) != 0 {
                self.c.lget(L_CPU).lget(L_R0 + i).mem(I32_STORE, self.lay.cpu_r + 4 * i);
            }
        }
    }
    fn store_tuple(&mut self) {
        let l = self.lay;
        self.c.lget(L_CPU).lget(L_LK).mem(I32_STORE, l.cpu_lf_kind);
        self.c.lget(L_CPU).lget(L_LA).mem(I32_STORE, l.cpu_lf_a);
        self.c.lget(L_CPU).lget(L_LB).mem(I32_STORE, l.cpu_lf_b);
        self.c.lget(L_CPU).lget(L_LR).mem(I32_STORE, l.cpu_lf_r);
    }
    fn load_regs(&mut self, mask: u32) {
        for i in 0..8u32 {
            if mask & (1 << i) != 0 {
                self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_r + 4 * i).lset(L_R0 + i);
            }
        }
    }

    /// Registers the interpreter may write while executing `ins` (bit i = GPR i) and whether it reads or
    /// writes the arithmetic flags. Taken from iced's instruction info (explicit + implicit operands).
    fn fb_usage(&mut self, ins: &Instruction) -> (u32, bool) {
        use iced_x86::OpAccess;
        if !Self::is_simd_fp(ins) {
            return (0xff, true);
        }
        let mut w = 0u32;
        let info = self.ifac.info(ins);
        for ur in info.used_registers() {
            let r = ur.register();
            if r.is_gpr() && !r.is_segment_register() {
                let full = r.full_register32();
                if full.is_gpr32() && matches!(ur.access(), OpAccess::Write | OpAccess::ReadWrite | OpAccess::CondWrite | OpAccess::ReadCondWrite) {
                    w |= 1 << full.number();
                }
            }
        }
        (w, Self::touches_arith_flags(ins))
    }

    /// x87 / MMX / SSE* instructions: the only family where iced's register and EFLAGS information is relied on.
    /// (For integer instructions it is not exact: e.g. it models `rcl r/m8, 9` as a no-op while the
    /// interpreter, like some hardware, still defines OF; those stay on the conservative path.)
    fn is_simd_fp(ins: &Instruction) -> bool {
        use iced_x86::CpuidFeature as F;
        let feats = ins.cpuid_features();
        !feats.is_empty() && feats.iter().all(|f| matches!(f, F::FPU | F::MMX | F::SSE | F::SSE2 | F::SSE3 | F::SSSE3 | F::SSE4_1 | F::SSE4_2))
    }

    /// An instruction that puts the x87 unit into MMX mode the way `Cpu::mm_enter` does and leaves TOP = 0.
    fn is_mmx_ins(ins: &Instruction) -> bool {
        (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_mm())
    }

    fn touches_arith_flags(ins: &Instruction) -> bool {
        use iced_x86::RflagsBits as R;
        if !Self::is_simd_fp(ins) {
            return true;
        }
        ((ins.rflags_read() | ins.rflags_modified()) & (R::OF | R::SF | R::ZF | R::AF | R::CF | R::PF)) != 0
    }

    fn fallback(&mut self, ins: &Instruction) {
        self.ins_dirty = true;
        if !Self::is_mmx_ins(ins) {
            // anything else may change TOP / the tag word / the x87 registers
            self.mmx_known = false;
            self.mm_tagged = 0;
        }
        self.n_fb += 1;
        *self.stats.entry(ins.mnemonic()).or_insert(0) += 1;
        let tag = (self.fb)(ins);
        let (wmask, flags) = self.fb_usage(ins);
        self.store_regs(self.known_written);
        self.store_tuple();
        self.c.lget(L_CPU).fallback_tag(tag).call(F_EXEC).ltee(L_X);
        self.if_(BT_EMPTY);
        // 1 + status: leave without writing back (the interpreter left the state in Cpu)
        self.c.lget(L_CPU).lget(L_BUD).mem(I32_STORE, self.lay.cpu_budget);
        self.c.lget(L_X).i32c(1).op(I32_SUB).lset(L_STATUS);
        self.br(LB_RETD);
        self.close();
        self.load_regs(wmask);
        if flags {
            // the interpreter folded the tuple into cpu.fl and may have changed the flags
            self.set_mem_state();
        }
        // else: the instruction leaves EFLAGS alone; the tuple in the locals is still the truth
    }
}

// =====================================================================================
// Instruction translation
// =====================================================================================

impl<'a> Fx<'a> {
    /// Translate one instruction. `Flow::Stop` means no instruction after it in this chain is reachable.
    fn ins(&mut self, ins: &Instruction, term_edge: &mut dyn FnMut(&mut Fx, EdgeKind) -> bool) -> Flow {
        self.n_ins += 1;
        let ip = ins.ip() as u32;
        let next = ins.next_ip() as u32;
        let mn = ins.mnemonic();
        self.cur_ip = ip;
        self.cur_mn = mn;
        self.tmp_top = 0;
        self.ins_dirty = false;
        self.guarded = None;
        self.exit_class = match mn {
            M::Call => {
                if ins.op0_kind() == OpKind::NearBranch32 {
                    1
                } else {
                    3
                }
            }
            M::Ret => 2,
            M::Jmp if ins.op0_kind() != OpKind::NearBranch32 => 3,
            M::Int | M::Pause | M::Hlt | M::Int3 | M::Ud2 => 4,
            _ => 0,
        };
        self.live = self.live_after.get(self.k).copied().unwrap_or(ALLF);
        self.k += 1;
        let flow = self.ins_inner(ins, ip, next, mn, term_edge);
        flow
    }

    fn ins_inner(&mut self, ins: &Instruction, ip: u32, next: u32, mn: M, edge: &mut dyn FnMut(&mut Fx, EdgeKind) -> bool) -> Flow {
        macro_rules! fb {
            () => {{
                self.fallback(ins);
                self.smc_check_fb(next);
                return Flow::Fall;
            }};
        }
        // Fallback for an instruction that may transfer control: continue wherever the interpreter left eip.
        macro_rules! fb_term {
            () => {{
                self.fallback(ins);
                self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_eip);
                self.exit_dyn(S_NEXT);
                return Flow::Stop;
            }};
        }
        // Offline portable code may only enter emitters whose address operands
        // are audited. Everything else is still compiled offline, as a generic
        // exec_one call with a serialized instruction and an explicit tag site.
        if self.c.record_relocations && !crate::aot::portable_instruction(ins) {
            if ins.flow_control() == iced_x86::FlowControl::Next { fb!(); }
            fb_term!();
        }
        match mn {
            M::Nop => return Flow::Fall,
            M::Mov | M::Movzx | M::Movsx => {
                if !Self::operands_ok(ins) || ins.op0_kind() == OpKind::Register && ins.op0_register().is_segment_register() {
                    fb!();
                }
                let v = self.read_src(ins, 1, ip);
                let v = if mn == M::Movsx {
                    let s = Self::opsz(ins, 1);
                    self.push_v(v);
                    match s {
                        1 => {
                            self.c.op(I32_EXTEND8_S);
                        }
                        2 => {
                            self.c.op(I32_EXTEND16_S);
                        }
                        _ => {}
                    }
                    self.to_tmp()
                } else {
                    v
                };
                let d = self.dst_of(ins, 0);
                self.write_dst(d, v, ip);
                self.smc_check_ip(next);
            }
            M::Lea => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                let a = match self.ea(ins, false) {
                    Some(a) => a,
                    None => fb!(),
                };
                self.c.lget(a);
                self.rset(ins.op0_register());
            }
            M::Push => {
                if !Self::operands_ok(ins) || ins.op0_kind() == OpKind::Memory && ins.memory_size().size() == 2 {
                    fb!();
                }
                let sz: u8 = match ins.op0_kind() {
                    OpKind::Register => reg_size(ins.op0_register()),
                    OpKind::Immediate16 | OpKind::Immediate8to16 => 2,
                    _ => 4,
                };
                if sz != 4 {
                    fb!();
                }
                let v = self.read_src(ins, 0, ip);
                let sp = self.tmp();
                self.c.lget(Self::r32(4)).i32c(4).op(I32_SUB).lset(sp);
                self.store_mem(sp, v, 4, ip);
                self.c.lget(sp);
                self.rset32(4);
                self.smc_check_ip(next);
            }
            M::Pop => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                let sz = match ins.op0_kind() {
                    OpKind::Register => reg_size(ins.op0_register()),
                    OpKind::Memory => ins.memory_size().size() as u8,
                    _ => 4,
                };
                if sz != 4 {
                    fb!();
                }
                // pop32: read [esp], esp += 4, then store to the destination (EA uses the new esp)
                self.load_mem(Self::r32(4), 4, ip);
                let v = self.to_tmp();
                self.c.lget(Self::r32(4)).i32c(4).op(I32_ADD);
                self.rset32(4);
                let d = self.dst_of(ins, 0);
                self.write_dst(d, v, ip);
                self.smc_check_ip(next);
            }
            M::Add | M::Adc | M::Sub | M::Sbb | M::And | M::Or | M::Xor | M::Cmp | M::Test => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                self.alu(ins, mn, ip);
                self.smc_check_ip(next);
            }
            M::Inc | M::Dec => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                self.incdec(ins, mn == M::Inc, ip);
                self.smc_check_ip(next);
            }
            M::Neg | M::Not => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                let sz = Self::opsz(ins, 0);
                let d = self.dst_of_w(ins, 0, ip);
                let a = self.read_dst(d, ip);
                let m = mask_of(sz) as i32;
                if mn == M::Neg {
                    let a2 = self.mask_copy(a, sz);
                    self.c.i32c(0);
                    self.push_v(a2);
                    self.c.op(I32_SUB).i32c(m).op(I32_AND);
                    let r = self.to_tmp();
                    self.rec(lk(OP_SUB, sz), V::C(0), a2, r);
                    self.write_dst(d, r, ip);
                } else {
                    self.push_v(a);
                    self.c.i32c(-1).op(I32_XOR).i32c(m).op(I32_AND);
                    let r = self.to_tmp();
                    self.write_dst(d, r, ip);
                }
                self.smc_check_ip(next);
            }
            M::Shl | M::Sal | M::Shr | M::Sar => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                self.shift(ins, mn, ip);
                self.smc_check_ip(next);
            }
            M::Shld | M::Shrd => {
                if !Self::operands_ok(ins) || Self::opsz(ins, 0) != 4 || ins.op1_kind() != OpKind::Register {
                    fb!();
                }
                self.dshift(ins, mn, ip);
                self.smc_check_ip(next);
            }
            M::Xchg => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                let sz = Self::opsz(ins, 0);
                let d0 = self.dst_of(ins, 0);
                let d1 = self.dst_of(ins, 1);
                let a = self.read_dst(d0, ip);
                let a = self.mask_copy(a, sz);
                let b = self.read_dst(d1, ip);
                let b = self.mask_copy(b, sz);
                self.write_dst(d0, b, ip);
                self.write_dst(d1, a, ip);
                self.smc_check_ip(next);
            }
            M::Cdq => {
                self.c.lget(Self::r32(0)).i32c(31).op(I32_SHR_S);
                self.rset32(2);
            }
            M::Cwde => {
                self.c.lget(Self::r32(0)).op(I32_EXTEND16_S);
                self.rset32(0);
            }
            M::Cbw => {
                self.c.lget(Self::r32(0)).op(I32_EXTEND8_S);
                self.rset(Register::AX);
            }
            M::Cwd => {
                self.c.lget(Self::r32(0)).i32c(15).op(I32_SHR_U).i32c(1).op(I32_AND).op(I32_EQZ).op(I32_EQZ).i32c(0xffff).op(I32_MUL);
                self.rset(Register::DX);
            }
            M::Leave => {
                // esp = ebp; ebp = pop32()
                self.c.lget(Self::r32(5));
                self.rset32(4);
                self.load_mem(Self::r32(4), 4, ip);
                self.rset32(5);
                self.c.lget(Self::r32(4)).i32c(4).op(I32_ADD);
                self.rset32(4);
            }
            M::Seto | M::Setno | M::Setb | M::Setae | M::Sete | M::Setne | M::Setbe | M::Seta | M::Sets | M::Setns | M::Setp | M::Setnp | M::Setl | M::Setge | M::Setle | M::Setg => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                self.cond(cc_of(mn));
                let v = self.to_tmp();
                let d = self.dst_of(ins, 0);
                self.write_dst(d, v, ip);
                self.smc_check_ip(next);
            }
            M::Cmovo | M::Cmovno | M::Cmovb | M::Cmovae | M::Cmove | M::Cmovne | M::Cmovbe | M::Cmova | M::Cmovs | M::Cmovns | M::Cmovp | M::Cmovnp | M::Cmovl | M::Cmovge | M::Cmovle | M::Cmovg => {
                if !Self::operands_ok(ins) {
                    fb!();
                }
                // The source is always read (a memory source can fault even when the move is not taken).
                let v = self.read_src(ins, 1, ip);
                let dst = ins.op0_register();
                self.cond(cc_of(mn));
                self.if_(BT_EMPTY);
                self.push_v(v);
                self.rset(dst);
                self.close();
                // cmov r32: the upper half is zeroed even when not taken is NOT modelled by the interpreter either.
            }

            // ---- control flow ----
            M::Jmp => match ins.op0_kind() {
                OpKind::NearBranch32 => {
                    // jmp to the node that was merged into this chain: just continue with its code
                    if self.inline_next == Some(ins.near_branch32()) {
                        return Flow::Fall;
                    }
                    edge(self, EdgeKind::Static(ins.near_branch32()));
                    return Flow::Stop;
                }
                OpKind::Register | OpKind::Memory => {
                    if !Self::operands_ok(ins) {
                        fb_term!();
                    }
                    let v = self.read_src(ins, 0, ip);
                    self.push_v(v);
                    self.exit_dyn(S_NEXT);
                    return Flow::Stop;
                }
                _ => fb_term!(),
            },
            M::Ja | M::Jae | M::Jb | M::Jbe | M::Je | M::Jg | M::Jge | M::Jl | M::Jle | M::Jne | M::Jno | M::Jnp | M::Jns | M::Jo | M::Jp | M::Js => {
                self.cond(cc_of(mn));
                self.if_(BT_EMPTY);
                edge(self, EdgeKind::Static(ins.near_branch32()));
                self.close();
            }
            M::Jecxz => {
                self.c.lget(Self::r32(1)).op(I32_EQZ);
                self.if_(BT_EMPTY);
                edge(self, EdgeKind::Static(ins.near_branch32()));
                self.close();
            }
            M::Loop | M::Loope | M::Loopne => {
                // ecx -= 1; branch if ecx != 0 (&& ZF / !ZF)
                self.c.lget(Self::r32(1)).i32c(1).op(I32_SUB);
                self.rset32(1);
                self.c.lget(Self::r32(1));
                if mn != M::Loop {
                    self.c.op(I32_EQZ).op(I32_EQZ);
                    self.cond(4);
                    if mn == M::Loopne {
                        self.c.op(I32_EQZ);
                    }
                    self.c.op(I32_AND);
                }
                self.if_(BT_EMPTY);
                edge(self, EdgeKind::Static(ins.near_branch32()));
                self.close();
            }
            M::Call => {
                let target = match ins.op0_kind() {
                    OpKind::NearBranch32 => V::C(ins.near_branch32()),
                    OpKind::Register | OpKind::Memory => {
                        if !Self::operands_ok(ins) {
                            fb_term!();
                        }
                        let v = self.read_src(ins, 0, ip);
                        // copy: the target is read before esp changes (call esp)
                        self.mask_copy(v, 4)
                    }
                    _ => fb_term!(),
                };
                if self.node_thunk != 0 {
                    if let V::C(_) = target {
                        // call + `mov reg,[esp]; ret` of a PIC thunk: the return address is stored (it stays in the dead stack slot, and the store faults like the push),
                        // reg gets it, ESP and the flags are unchanged and execution continues at the return site
                        let reg = (self.node_thunk - 1) as usize;
                        let sp = self.tmp();
                        self.c.lget(Self::r32(4)).i32c(4).op(I32_SUB).lset(sp);
                        self.store_return_ip(sp, next, ip);
                        self.c.ip(next);
                        self.rset32(reg);
                        self.smc_check_ip(next);
                        return Flow::Fall;
                    }
                }
                let sp = self.tmp();
                self.c.lget(Self::r32(4)).i32c(4).op(I32_SUB).lset(sp);
                self.store_return_ip(sp, next, ip);
                self.c.lget(sp);
                self.rset32(4);
                match target {
                    V::C(t) => {
                        self.smc_check_ip(t);
                        edge(self, EdgeKind::Static(t));
                    }
                    v => {
                        // keep the target in a temp that survives the smc check
                        self.smc_check(v);
                        self.push_v(v);
                        self.exit_dyn(S_NEXT);
                    }
                }
                return Flow::Stop;
            }
            M::Ret => {
                self.load_mem(Self::r32(4), 4, ip);
                let t = self.to_tmp();
                let adj = if ins.op_count() == 1 { 4 + ins.immediate16() as u32 } else { 4 };
                self.c.lget(Self::r32(4)).u32c(adj).op(I32_ADD);
                self.rset32(4);
                // returning to a call site of this very region: the shared matcher after the chains jumps straight there
                self.push_v(t);
                if self.ret_sites.is_empty() {
                    self.exit_dyn(S_NEXT);
                } else {
                    self.c.lset(L_NEXT);
                    self.used_retm = true;
                    self.br(LB_RETM);
                }
                return Flow::Stop;
            }
            M::Int => {
                match ins.immediate8() {
                    0x80 => {
                        // The pure time syscalls are answered inline by the kernel (Kernel::time_syscall,
                        // the same function the exit path uses): guests poll the clock millions of times
                        // per session and every full exit tears the region chain down.
                        self.store_regs(0x0b); // eax=0, ecx=1, ebx=3 (not edx=2)
                        self.c.lget(L_CPU).call(F_SYS).ltee(L_X);
                        self.if_(BT_EMPTY); // != 0: not answered inline
                        self.c.lget(L_X).i32c(2).op(I32_EQ);
                         self.if_(BT_EMPTY); // == 2: answered, but into a code page
                        self.load_regs(1); // preserve the helper's return value on S_SYNC as well
                         self.exit_to(next, S_SYNC);
                        self.close();
                        self.exit_to(next, S_SYSCALL);
                        self.close();
                        self.load_regs(1); // eax = the syscall's return value
                        self.exit_to(next, S_NEXT);
                    }
                    0x82 => self.exit_to(next, S_HYPER),
                    _ => fb_term!(),
                }
                return Flow::Stop;
            }
            M::Pause => {
                self.exit_to(next, S_PAUSE);
                return Flow::Stop;
            }
            M::Int3 | M::Hlt | M::Ud2 | M::Jcxz | M::Jrcxz | M::Sysenter => fb_term!(),
            _ => {
                if self.ins_ext(ins, ip, next, mn) {
                    self.smc_check_ip(next);
                    return Flow::Fall;
                }
                fb!()
            }
        }
        Flow::Fall
    }

    /// After a fallback instruction that stored to code: handled by the helper return code, nothing to do.
    fn smc_check_fb(&mut self, _next: u32) {
        self.had_store = false;
    }

    /// A value masked to `sz` bytes that is safe to keep while guest registers change. Operands read through
    /// `read_src`/`read_dst` are already zero-extended temporaries (partial-register reads mask, loads
    /// zero-extend), so only aliases of 32-bit guest registers (sz == 4 there) and wide constants need work.
    fn mask_copy(&mut self, v: V, sz: u8) -> V {
        match v {
            V::C(c) => V::C(c & mask_of(sz)),
            V::L(l) if l >= L_TMP0 => V::L(l),
            V::L(l) => {
                self.c.lget(l);
                if sz != 4 {
                    self.c.u32c(mask_of(sz)).op(I32_AND);
                }
                self.to_tmp()
            }
        }
    }

    fn alu(&mut self, ins: &Instruction, mn: M, ip: u32) {
        let sz = Self::opsz(ins, 0);
        let m = mask_of(sz);
        let d = if mn != M::Cmp && mn != M::Test { self.dst_of_w(ins, 0, ip) } else { self.dst_of(ins, 0) };
        let a = self.read_dst(d, ip);
        let b = self.read_src(ins, 1, ip);
        // a, b masked copies (aliases of 32-bit registers are fine for sz==4)
        let a = if sz == 4 { a } else { self.mask_copy(a, sz) };
        let b = match b {
            V::C(c) => V::C(c & m),
            v if sz == 4 => v,
            v => self.mask_copy(v, sz),
        };
        let op = match mn {
            M::Add => OP_ADD,
            M::Adc => OP_ADC,
            M::Sub | M::Cmp => OP_SUB,
            M::Sbb => OP_SBB,
            _ => OP_LOGIC,
        };
        let carry = if mn == M::Adc || mn == M::Sbb {
            self.carry();
            Some(self.to_tmp())
        } else {
            None
        };
        // result
        self.push_v(a);
        self.push_v(b);
        match mn {
            M::Add | M::Adc => {
                self.c.op(I32_ADD);
            }
            M::Sub | M::Cmp | M::Sbb => {
                self.c.op(I32_SUB);
            }
            M::And | M::Test => {
                self.c.op(I32_AND);
            }
            M::Or => {
                self.c.op(I32_OR);
            }
            _ => {
                self.c.op(I32_XOR);
            }
        }
        if let Some(cv) = carry {
            self.push_v(cv);
            self.c.op(if mn == M::Adc { I32_ADD } else { I32_SUB });
        }
        if sz != 4 {
            self.c.u32c(m).op(I32_AND);
        }
        let r = self.to_tmp();
        self.rec(lk(op, sz), a, b, r);
        if mn != M::Cmp && mn != M::Test {
            self.write_dst(d, r, ip);
        }
    }

    fn incdec(&mut self, ins: &Instruction, inc: bool, ip: u32) {
        let sz = Self::opsz(ins, 0);
        let m = mask_of(sz);
        let d = self.dst_of_w(ins, 0, ip);
        let a = self.read_dst(d, ip);
        let a = if sz == 4 { a } else { self.mask_copy(a, sz) };
        // preserved CF (the interpreter keeps the old CF); skipped when nothing can read it any more
        let cf = if self.live & crate::cpu::CF != 0 {
            self.carry();
            self.to_tmp()
        } else {
            V::C(0)
        };
        self.push_v(a);
        self.c.i32c(1).op(if inc { I32_ADD } else { I32_SUB });
        if sz != 4 {
            self.c.u32c(m).op(I32_AND);
        }
        let r = self.to_tmp();
        self.rec(lk(if inc { OP_INC } else { OP_DEC }, sz), a, cf, r);
        self.write_dst(d, r, ip);
    }

    /// SHLD / SHRD r/m32, r32, imm8|CL (count masked to 5 bits; a zero count changes nothing, not even the flags). The flags are the
    /// interpreter's: CF = last bit shifted out, OF = sign change of the result (for every count), SF/ZF/PF of the result, AF = 0.
    fn dshift(&mut self, ins: &Instruction, mn: M, ip: u32) {
        let left = mn == M::Shld;
        let d = self.dst_of_w(ins, 0, ip);
        let a = self.read_dst(d, ip);
        let a = self.mask_copy(a, 4);
        let s = self.read_src(ins, 1, ip);
        let s = self.mask_copy(s, 4);
        let cnt: V = match ins.op_kind(2) {
            OpKind::Register => {
                // only CL is encodable
                self.c.lget(Self::r32(1)).i32c(31).op(I32_AND);
                self.to_tmp()
            }
            _ => V::C(Self::imm_of(ins, 2) & 0x1f),
        };
        // r = (a << c) | (s >> (32 - c))  /  (a >> c) | (s << (32 - c)), for c in 1..=31
        let result = |f: &mut Fx<'a>, cnt: V| {
            f.push_v(a);
            f.push_v(cnt);
            f.c.op(if left { I32_SHL } else { I32_SHR_U });
            f.push_v(s);
            f.c.i32c(32);
            f.push_v(cnt);
            f.c.op(I32_SUB).op(if left { I32_SHR_U } else { I32_SHL }).op(I32_OR);
        };
        let flags = |f: &mut Fx<'a>, r: u32, cnt: V| {
            // CF: last bit shifted out
            f.push_v(a);
            if left {
                f.c.i32c(32);
                f.push_v(cnt);
                f.c.op(I32_SUB).op(I32_SHR_U);
            } else {
                f.push_v(cnt);
                f.c.i32c(1).op(I32_SUB).op(I32_SHR_U);
            }
            f.c.i32c(1).op(I32_AND);
            f.parity(r);
            f.c.i32c(2).op(I32_SHL).op(I32_OR); // PF
            f.c.lget(r).op(I32_EQZ).i32c(6).op(I32_SHL).op(I32_OR); // ZF
            f.c.lget(r).i32c(31).op(I32_SHR_U).i32c(7).op(I32_SHL).op(I32_OR); // SF
            f.c.lget(r);
            f.push_v(a);
            f.c.op(I32_XOR).i32c(31).op(I32_SHR_U).i32c(11).op(I32_SHL).op(I32_OR); // OF = sign change
            f.rec_full_stack(None);
        };
        let want_flags = self.live & ALLF != 0;
        match cnt {
            V::C(0) => {
                // no change at all (the interpreter still writes the value back)
                self.write_dst(d, a, ip);
            }
            V::C(_) => {
                result(self, cnt);
                let r = self.tmp();
                self.c.lset(r);
                if want_flags {
                    flags(self, r, cnt);
                }
                self.write_dst(d, V::L(r), ip);
            }
            V::L(cl) => {
                // r = cnt == 0 ? a : computed; flags only change when cnt != 0
                let r = self.tmp();
                self.push_v(a);
                self.c.lset(r);
                self.c.lget(cl);
                self.if_(BT_EMPTY);
                result(self, cnt);
                self.c.lset(r);
                self.ins_dirty = true;
                if want_flags {
                    flags(self, r, cnt);
                }
                self.close();
                self.st = St::Dyn;
                self.write_dst(d, V::L(r), ip);
            }
        }
    }

    fn shift(&mut self, ins: &Instruction, mn: M, ip: u32) {
        let sz = Self::opsz(ins, 0);
        let m = mask_of(sz);
        let d = self.dst_of_w(ins, 0, ip);
        let a = self.read_dst(d, ip);
        let a = self.mask_copy(a, sz);
        let op = match mn {
            M::Shl | M::Sal => OP_SHL,
            M::Shr => OP_SHR,
            _ => OP_SAR,
        };
        // count
        let cnt: V = if ins.op_count() > 1 {
            match ins.op_kind(1) {
                OpKind::Register => {
                    // only CL is encodable
                    self.c.lget(Self::r32(1)).i32c(31).op(I32_AND);
                    self.to_tmp()
                }
                _ => V::C(Self::imm_of(ins, 1) & 0x1f),
            }
        } else {
            V::C(1)
        };
        let compute = |s: &mut Fx<'a>, a: V, cnt: V| {
            s.push_v(a);
            if op == OP_SAR {
                match sz {
                    1 => {
                        s.c.op(I32_EXTEND8_S);
                    }
                    2 => {
                        s.c.op(I32_EXTEND16_S);
                    }
                    _ => {}
                }
            }
            s.push_v(cnt);
            s.c.op(match op {
                OP_SHL => I32_SHL,
                OP_SHR => I32_SHR_U,
                _ => I32_SHR_S,
            });
            if sz != 4 {
                s.c.u32c(m).op(I32_AND);
            }
        };
        match cnt {
            V::C(0) => {
                // no change at all (the interpreter still writes the value back)
                self.write_dst(d, a, ip);
            }
            V::C(_) => {
                compute(self, a, cnt);
                let r = self.to_tmp();
                self.rec(lk(op, sz), a, cnt, r);
                self.write_dst(d, r, ip);
            }
            V::L(cl) => {
                // r = cnt == 0 ? a : compute; flags only change when cnt != 0
                let r = self.tmp();
                self.push_v(a);
                self.c.lset(r);
                self.c.lget(cl);
                self.if_(BT_EMPTY);
                compute(self, a, cnt);
                self.c.lset(r);
                self.ins_dirty = true;
                self.c.u32c(lk(op, sz)).lset(L_LK);
                self.push_v(a);
                self.c.lset(L_LA);
                self.c.lget(cl).lset(L_LB);
                self.c.lget(r).lset(L_LR);
                self.close();
                self.st = St::Dyn;
                self.write_dst(d, V::L(r), ip);
            }
        }
    }
}


// =====================================================================================
// Flag liveness (per chain) and the second batch of inline instructions
// =====================================================================================

const ALLF: u32 = crate::cpu::ARITH;
const SZPA: u32 = crate::cpu::PF | crate::cpu::AF | crate::cpu::ZF | crate::cpu::SF;

fn cc_reads(cc: u8) -> u32 {
    use crate::cpu::*;
    match cc >> 1 {
        0 => OF,
        1 => CF,
        2 => ZF,
        3 => CF | ZF,
        4 => SF,
        5 => PF,
        6 => SF | OF,
        _ => ZF | SF | OF,
    }
}

/// Constant (already & 0x1f) shift/rotate count of an instruction, `None` when it is CL.
fn const_count(ins: &Instruction) -> Option<u32> {
    if ins.op_count() < 2 {
        return Some(1);
    }
    match ins.op_kind(1) {
        OpKind::Register => None,
        _ => Some(Fx::imm_of(ins, 1) & 0x1f),
    }
}

/// (flags read, flags definitely overwritten) of an instruction, as program semantics. `reads` is a
/// conservative superset; `kills` only lists flags that are written on every path. Control transfers
/// read everything (the successor may inspect any flag); unknown instructions read everything.
pub fn flag_info(ins: &Instruction) -> (u32, u32) {
    use crate::cpu::*;
    let mn = ins.mnemonic();
    match mn {
        M::Add | M::Sub | M::And | M::Or | M::Xor | M::Cmp | M::Test | M::Neg | M::Cmpxchg | M::Xadd => (0, ALLF),
        M::Adc | M::Sbb => (CF, ALLF),
        M::Inc | M::Dec => (0, ALLF & !CF),
        M::Shl | M::Sal | M::Shr | M::Sar | M::Shld | M::Shrd => match const_count_sh(ins) {
            Some(0) => (0, 0),
            Some(_) => (0, ALLF),
            None => (0, 0),
        },
        M::Rol | M::Ror => match const_count(ins) {
            Some(0) => (0, 0),
            Some(_) => (0, CF | OF),
            None => (0, 0),
        },
        M::Rcl | M::Rcr => (CF, 0),
        M::Imul if ins.op_count() > 1 => (0, ALLF & !AF),
        M::Mul | M::Imul => (0, CF | OF),
        M::Div | M::Idiv => (0, 0),
        M::Bt | M::Bts | M::Btr | M::Btc => (0, CF),
        M::Bsf | M::Bsr => (0, ZF),
        M::Bswap | M::Mov | M::Movzx | M::Movsx | M::Lea | M::Push | M::Pop | M::Xchg | M::Cdq | M::Cwde | M::Cwd | M::Cbw | M::Leave | M::Nop | M::Cld | M::Std | M::Xlatb => (0, 0),
        M::Lahf => (CF | PF | AF | ZF | SF, 0),
        M::Sahf => (0, CF | PF | AF | ZF | SF),
        M::Clc | M::Stc => (0, CF),
        M::Cmc => (CF, 0),
        M::Seto | M::Setno | M::Setb | M::Setae | M::Sete | M::Setne | M::Setbe | M::Seta | M::Sets | M::Setns | M::Setp | M::Setnp | M::Setl | M::Setge | M::Setle | M::Setg => (cc_reads(cc_of(mn)), 0),
        M::Cmovo | M::Cmovno | M::Cmovb | M::Cmovae | M::Cmove | M::Cmovne | M::Cmovbe | M::Cmova | M::Cmovs | M::Cmovns | M::Cmovp | M::Cmovnp | M::Cmovl | M::Cmovge | M::Cmovle | M::Cmovg => (cc_reads(cc_of(mn)), 0),
        _ => {
            // unknown to the translator: ask iced whether the instruction touches the arithmetic flags at all
            if Fx::touches_arith_flags(ins) {
                (ALLF, 0)
            } else {
                (0, 0)
            }
        }
    }
}

fn const_count_sh(ins: &Instruction) -> Option<u32> {
    // shld/shrd: the count is operand 2 (imm8 or CL); shl/shr/sar as in const_count
    match ins.mnemonic() {
        M::Shld | M::Shrd => match ins.op_kind(2) {
            OpKind::Register => None,
            _ => Some(Fx::imm_of(ins, 2) & 0x1f),
        },
        _ => const_count(ins),
    }
}

/// Can executing the instruction raise a guest fault (explicit or implicit memory access, divide error)?
/// A fault exposes the flags *before* the instruction (signal frame), so such instructions read all flags.
fn may_fault(ins: &Instruction) -> bool {
    if matches!(
        ins.mnemonic(),
        M::Push | M::Pop | M::Call | M::Ret | M::Leave | M::Pushad | M::Popad | M::Pushfd | M::Popfd | M::Pushf | M::Popf | M::Enter | M::Div | M::Idiv | M::Xlatb
            | M::Movsb | M::Movsw | M::Movsd | M::Cmpsb | M::Cmpsw | M::Cmpsd | M::Stosb | M::Stosw | M::Stosd | M::Lodsb | M::Lodsw | M::Lodsd | M::Scasb | M::Scasw | M::Scasd
    ) {
        return true;
    }
    (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory)
}

/// live_after[k] = flags possibly read after instruction k of a chain (the chain end and every exit read
/// all flags, and so does every instruction that can fault: the signal frame shows the flags at the fault).
fn liveness(flat: &[&Instruction]) -> Vec<u32> {
    let mut out = vec![0u32; flat.len()];
    let mut live = ALLF;
    for k in (0..flat.len()).rev() {
        let fault = may_fault(flat[k]);
        // a store that faults after the flags were updated exposes the flags *after* the instruction too
        out[k] = if fault { live | ALLF } else { live };
        let (mut r, kill) = flag_info(flat[k]);
        if fault {
            r |= ALLF;
        }
        live = (live & !kill) | r;
    }
    out
}

impl<'a> Fx<'a> {
    /// Push the AF bit (0 or 0x10) a flag tuple of kind `lk` describes, when that has an inline form.
    fn af_of_kind(&mut self, lk: u32) -> bool {
        let (a, b, r) = (L_LA, L_LB, L_LR);
        match lk_op(lk) {
            OP_ADD | OP_ADC | OP_SUB | OP_SBB => {
                self.c.lget(a).lget(b).op(I32_XOR).lget(r).op(I32_XOR).i32c(0x10).op(I32_AND);    // AF = bit 4 of a ^ b ^ r
            }
            OP_INC | OP_DEC => {
                self.c.lget(a).i32c(1).op(I32_XOR).lget(r).op(I32_XOR).i32c(0x10).op(I32_AND);
            }
            OP_LOGIC | OP_SHL | OP_SHR | OP_SAR => {
                self.c.i32c(0);
            }
            OP_FULL => {
                self.c.lget(r).i32c(0x10).op(I32_AND);
            }
            OP_IMUL => {
                self.c.lget(b).i32c(0x10).op(I32_AND);
            }
            _ => return false,
        }
        true
    }

    /// The AF bit of the current flag state as a value (0 or 0x10), or 0 when nothing can read it any more. Unlike
    /// `old_bits` this does not call the `mat` helper for the common flag kinds.
    fn old_af(&mut self) -> V {
        if self.live & crate::cpu::AF == 0 {
            return V::C(0);
        }
        match self.st {
            St::Lazy(lk) => {
                if self.af_of_kind(lk) {
                    return self.to_tmp();
                }
            }
            St::Mem => {
                self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).i32c(0x10).op(I32_AND);
                return self.to_tmp();
            }
            St::Dyn => {
                self.szpa_dyn(crate::cpu::AF);
                return self.to_tmp();
            }
        }
        self.flag_bits();
        self.c.i32c(0x10).op(I32_AND);
        self.to_tmp()
    }

    /// Would `af_of_kind` emit code for this kind? (Nothing is left behind.)
    fn has_af_kind(&mut self, lk: u32) -> bool {
        let saved = std::mem::take(&mut self.c);
        let ok = self.af_of_kind(lk);
        self.c = saved;
        ok
    }

    /// Push the flags in `mask` (EFLAGS bit positions) that a statically known lazy kind describes, without calling the `mat`
    /// helper. Returns false (nothing emitted) when some needed form does not exist.
    fn bits_of_kind(&mut self, lk: u32, want: u32) -> bool {
        use crate::cpu::*;
        let (op, sz) = (lk_op(lk), lk_size(lk));
        if op == OP_FULL {
            return false;
        }
        let parts: [(u32, u8, u32); 5] = [(CF, 1, 0), (PF, 5, 2), (ZF, 2, 6), (SF, 4, 7), (OF, 0, 11)];
        for (bit, base, _) in parts {
            if want & bit != 0 && !self.has_fused(op, sz, base) {
                return false;
            }
        }
        if want & AF != 0 && !self.has_af_kind(lk) {
            return false;
        }
        let mut first = true;
        for (bit, base, shift) in [(CF, 1u8, 0u32), (PF, 5, 2), (AF, 0, 0), (ZF, 2, 6), (SF, 4, 7), (OF, 0, 11)] {
            if want & bit == 0 {
                continue;
            }
            let ok = if bit == AF { self.af_of_kind(lk) } else { self.fused_base(op, sz, base) };
            debug_assert!(ok);
            if shift > 0 {
                self.c.i32c(shift as i32).op(I32_SHL);
            }
            if !first {
                self.c.op(I32_OR);
            }
            first = false;
        }
        if first {
            self.c.i32c(0);
        }
        true
    }

    /// Push the flags in `mask` (a subset of PF|AF|ZF|SF) of a flag tuple whose kind is only known at run time. The sign
    /// position follows the operand size in the low bits of the kind word; the auxiliary carry follows the operation.
    fn szpa_dyn(&mut self, want: u32) {
        use crate::cpu::*;
        debug_assert!(want & !SZPA == 0);
        self.c.lget(L_LK).op(I32_EQZ);
        self.if_(I32);
        // flags in cpu.fl
        self.c.lget(L_CPU).mem(I32_LOAD, self.lay.cpu_fl).u32c(want).op(I32_AND);
        self.else_();
        self.c.lget(L_LK).i32c(2).op(I32_SHR_U).i32c(OP_FULL as i32).op(I32_EQ);
        self.if_(I32);
        // explicit flag word
        self.c.lget(L_LR).u32c(want).op(I32_AND);
        self.else_();
        let mut first = true;
        if want & PF != 0 {
            self.parity(L_LR);
            self.c.i32c(2).op(I32_SHL);
            first = false;
        }
        if want & ZF != 0 {
            self.c.lget(L_LR).op(I32_EQZ).i32c(6).op(I32_SHL);
            if !first {
                self.c.op(I32_OR);
            }
            first = false;
        }
        if want & SF != 0 {
            // sign bit index: 7, 15 or 31 for operand sizes 1, 2, 4
            self.c.lget(L_LR).i32c(8).lget(L_LK).i32c(3).op(I32_AND).op(I32_SHL).i32c(1).op(I32_SUB).op(I32_SHR_U).i32c(1).op(I32_AND).i32c(7).op(I32_SHL);
            if !first {
                self.c.op(I32_OR);
            }
            first = false;
        }
        if want & AF != 0 {
            let op = self.tmp();
            self.c.lget(L_LK).i32c(2).op(I32_SHR_U).lset(op);
            // ADD / ADC / SUB / SBB (1..=4): a ^ b ^ r; INC / DEC (6, 7): a ^ 1 ^ r; IMUL: the preserved bit in b; otherwise 0
            self.c.lget(op).i32c(1).op(I32_SUB).i32c(4).op(I32_LT_U);
            self.if_(I32);
            self.c.lget(L_LA).lget(L_LB).op(I32_XOR).lget(L_LR).op(I32_XOR).i32c(0x10).op(I32_AND);
            self.else_();
            self.c.lget(op).i32c(6).op(I32_SUB).i32c(2).op(I32_LT_U);
            self.if_(I32);
            self.c.lget(L_LA).i32c(1).op(I32_XOR).lget(L_LR).op(I32_XOR).i32c(0x10).op(I32_AND);
            self.else_();
            self.c.lget(op).i32c(OP_IMUL as i32).op(I32_EQ);
            self.if_(I32);
            self.c.lget(L_LB).i32c(0x10).op(I32_AND);
            self.else_();
            self.c.i32c(0);
            self.close();
            self.close();
            self.close();
            if !first {
                self.c.op(I32_OR);
            }
            first = false;
        }
        if first {
            self.c.i32c(0);
        }
        self.close();
        self.close();
    }

    /// The flags in `mask`, but only if some of them are still live after the current instruction.
    fn old_bits(&mut self, mask: u32) -> Option<V> {
        if self.live & mask == 0 {
            return None;
        }
        match self.st {
            St::Lazy(lk) if lk_op(lk) != OP_FULL => {
                if self.bits_of_kind(lk, mask) {
                    return Some(self.to_tmp());
                }
            }
            St::Dyn if mask & !SZPA == 0 => {
                self.szpa_dyn(mask);
                return Some(self.to_tmp());
            }
            _ => {}
        }
        self.flag_bits();
        self.c.u32c(mask).op(I32_AND);
        Some(self.to_tmp())
    }

    /// Record explicit flag bits: `new` is on the stack, `pres` are preserved bits (already masked).
    fn rec_full_stack(&mut self, pres: Option<V>) {
        self.ins_dirty = true;
        if let Some(p) = pres {
            self.push_v(p);
            self.c.op(I32_OR);
        }
        self.c.lset(L_LR);
        self.c.u32c(lk(OP_FULL, 4)).lset(L_LK);
        self.st = St::Lazy(lk(OP_FULL, 4));
    }

    fn t64(i: u32) -> u32 {
        L_T64 + i
    }

    /// Try to translate an instruction not covered by the first batch. Returns false (nothing emitted) if unsupported.
    fn ins_ext(&mut self, ins: &Instruction, ip: u32, next: u32, mn: M) -> bool {
        use crate::cpu::*;
        // Portable AOT uses scalar bit-preserving SSE moves/logic. The ordinary
        // SIMD emitter has interpreter-tag slow branches and absolute RAM offsets;
        // never route these offline shapes through that emitter by accident.
        if self.c.record_relocations && self.portable_sse_bits(ins, ip, mn) { return true; }
        match mn {
            // ---- imul r, r/m [, imm] ----
            M::Imul if ins.op_count() > 1 => {
                if !Self::operands_ok(ins) || ins.op0_kind() != OpKind::Register {
                    return false;
                }
                let sz = Self::opsz(ins, 0);
                let (a, b) = if ins.op_count() == 3 {
                    let a = self.read_src(ins, 1, ip);
                    let b = self.read_src(ins, 2, ip);
                    (a, b)
                } else {
                    let a = self.read_src(ins, 0, ip);
                    let b = self.read_src(ins, 1, ip);
                    (a, b)
                };
                let a = self.mask_copy(a, sz);
                let b = self.mask_copy(b, sz);
                let res = self.tmp();
                let over = self.tmp();
                match sz {
                    4 => {
                        self.push_v(a);
                        self.c.op(I64_EXTEND_I32_S);
                        self.push_v(b);
                        self.c.op(I64_EXTEND_I32_S).op(I64_MUL).ltee(Self::t64(0));
                        self.c.op(I32_WRAP_I64).ltee(res).op(I64_EXTEND_I32_S).lget(Self::t64(0)).op(I64_NE).lset(over);
                    }
                    _ => {
                        let ext = if sz == 2 { I32_EXTEND16_S } else { I32_EXTEND8_S };
                        let p = self.tmp();
                        self.push_v(a);
                        self.c.op(ext);
                        self.push_v(b);
                        self.c.op(ext).op(I32_MUL).ltee(p).u32c(mask_of(sz)).op(I32_AND).ltee(res).op(ext).lget(p).op(I32_NE).lset(over);
                    }
                }
                let afp = self.old_af();
                self.rec(lk(OP_IMUL, sz), V::L(over), afp, V::L(res));
                self.c.lget(res);
                self.rset(ins.op0_register());
                true
            }
            // ---- 32-bit mul / imul / div / idiv (one operand) ----
            M::Mul | M::Imul | M::Div | M::Idiv => {
                if !Self::operands_ok(ins) || Self::opsz(ins, 0) != 4 {
                    return false;
                }
                let src = self.read_src(ins, 0, ip);
                let d = self.mask_copy(src, 4); // copy: eax/edx are rewritten below
                let (t0, t1, t2, t3) = (Self::t64(0), Self::t64(1), Self::t64(2), Self::t64(3));
                if mn == M::Mul || mn == M::Imul {
                    let ext = if mn == M::Mul { I64_EXTEND_I32_U } else { I64_EXTEND_I32_S };
                    self.c.lget(Self::r32(0)).op(ext);
                    self.push_v(d);
                    self.c.op(ext).op(I64_MUL).lset(t0);
                    let lo = self.tmp();
                    let hi = self.tmp();
                    self.c.lget(t0).op(I32_WRAP_I64).lset(lo);
                    self.c.lget(t0).i64c(32).op(I64_SHR_U).op(I32_WRAP_I64).lset(hi);
                    if mn == M::Mul {
                        self.c.lget(hi).op(I32_EQZ).op(I32_EQZ);
                    } else {
                        self.c.lget(t0).lget(lo).op(I64_EXTEND_I32_S).op(I64_NE);
                    }
                    self.c.u32c(CF | OF).op(I32_MUL);
                    let newf = self.to_tmp();
                    let pres = self.old_bits(PF | AF | ZF | SF);
                    self.c.lget(lo);
                    self.rset32(0);
                    self.c.lget(hi);
                    self.rset32(2);
                    self.push_v(newf);
                    self.rec_full_stack(pres);
                    return true;
                }
                // division
                self.push_v(d);
                self.c.op(I32_EQZ);
                self.if_(BT_EMPTY);
                self.exit_to(ip, S_DIVERR);
                self.close();
                if mn == M::Div {
                    self.c.lget(Self::r32(2)).op(I64_EXTEND_I32_U).i64c(32).op(I64_SHL).lget(Self::r32(0)).op(I64_EXTEND_I32_U).op(I64_OR).lset(t0);
                    self.push_v(d);
                    self.c.op(I64_EXTEND_I32_U).lset(t1);
                    self.c.lget(t0).lget(t1).op(I64_DIV_U).lset(t2);
                    self.c.lget(t2).i64c(0xffff_ffff).op(I64_GT_U);
                    self.if_(BT_EMPTY);
                    self.exit_to(ip, S_DIVERR);
                    self.close();
                    self.c.lget(t0).lget(t1).op(I64_REM_U).lset(t3);
                } else {
                    self.c.lget(Self::r32(2)).op(I64_EXTEND_I32_S).i64c(32).op(I64_SHL).lget(Self::r32(0)).op(I64_EXTEND_I32_U).op(I64_OR).lset(t0);
                    self.push_v(d);
                    self.c.op(I64_EXTEND_I32_S).lset(t1);
                    self.c.lget(t1).i64c(-1).op(I64_EQ);
                    self.if_(BT_EMPTY);
                    self.c.i64c(0).lget(t0).op(I64_SUB).lset(t2);
                    self.c.i64c(0).lset(t3);
                    self.else_();
                    self.c.lget(t0).lget(t1).op(I64_DIV_S).lset(t2);
                    self.c.lget(t0).lget(t1).op(I64_REM_S).lset(t3);
                    self.close();
                    self.c.lget(t2).i64c(i32::MIN as i64).op(I64_LT_S).lget(t2).i64c(i32::MAX as i64).op(I64_GT_S).op(I32_OR);
                    self.if_(BT_EMPTY);
                    self.exit_to(ip, S_DIVERR);
                    self.close();
                }
                self.c.lget(t2).op(I32_WRAP_I64);
                self.rset32(0);
                self.c.lget(t3).op(I32_WRAP_I64);
                self.rset32(2);
                true
            }
            // ---- rol / ror ----
            M::Rol | M::Ror => {
                if !Self::operands_ok(ins) {
                    return false;
                }
                let sz = Self::opsz(ins, 0);
                let bits = sz as u32 * 8;
                let d = self.dst_of(ins, 0);
                let a = self.read_dst(d, ip);
                let a = self.mask_copy(a, sz);
                let rol = mn == M::Rol;
                let cnt = const_count(ins);
                // result and flag bits from the rotated value
                let rotate = |s: &mut Fx<'a>, cnt: V| {
                    s.push_v(a);
                    match sz {
                        2 => {
                            s.push_v(a);
                            s.c.i32c(16).op(I32_SHL).op(I32_OR);
                        }
                        1 => {
                            s.c.u32c(0x0101_0101).op(I32_MUL);
                        }
                        _ => {}
                    }
                    s.push_v(cnt);
                    s.c.op(if rol { I32_ROTL } else { I32_ROTR });
                    if sz != 4 {
                        s.c.u32c(mask_of(sz)).op(I32_AND);
                    }
                };
                let flag_bits_of = |s: &mut Fx<'a>, res: u32| {
                    // new CF/OF bits for the rotated result `res`
                    if rol {
                        // cf = res & 1 ; of = msb ^ cf
                        s.c.lget(res).i32c(bits as i32 - 1).op(I32_SHR_U).lget(res).op(I32_XOR).i32c(1).op(I32_AND).i32c(11).op(I32_SHL);
                        s.c.lget(res).i32c(1).op(I32_AND).op(I32_OR);
                    } else {
                        // cf = msb ; of = msb ^ second msb
                        s.c.lget(res).i32c(bits as i32 - 1).op(I32_SHR_U).lget(res).i32c(bits as i32 - 2).op(I32_SHR_U).op(I32_XOR).i32c(1).op(I32_AND).i32c(11).op(I32_SHL);
                        s.c.lget(res).i32c(bits as i32 - 1).op(I32_SHR_U).i32c(1).op(I32_AND).op(I32_OR);
                    }
                };
                match cnt {
                    Some(0) => self.write_dst(d, a, ip),
                    Some(c) => {
                        rotate(self, V::C(c));
                        let res = self.to_tmp();
                        let r = if let V::L(l) = res { l } else { unreachable!() };
                        let pres = self.old_bits(SZPA);
                        flag_bits_of(self, r);
                        self.rec_full_stack(pres);
                        self.write_dst(d, res, ip);
                    }
                    None => {
                        let cl = self.tmp();
                        self.c.lget(Self::r32(1)).i32c(31).op(I32_AND).lset(cl);
                        let pres = self.old_bits(SZPA);
                        let r = self.tmp();
                        self.push_v(a);
                        self.c.lset(r);
                        self.c.lget(cl);
                        self.if_(BT_EMPTY);
                        rotate(self, V::L(cl));
                        self.c.lset(r);
                        flag_bits_of(self, r);
                        self.rec_full_stack(pres);
                        self.close();
                        self.st = St::Dyn;
                        self.write_dst(d, V::L(r), ip);
                    }
                }
                true
            }
            // ---- bt / bts / btr / btc ----
            M::Bt | M::Bts | M::Btr | M::Btc => {
                if !Self::operands_ok(ins) {
                    return false;
                }
                let sz = Self::opsz(ins, 0);
                if sz == 1 {
                    return false;
                }
                let (lg, shsz) = if sz == 4 { (5, 2) } else { (4, 1) };
                let bits = sz as u32 * 8;
                let d0 = self.dst_of(ins, 0);
                let off = self.read_src(ins, 1, ip);
                let (d, bit): (Dst, V) = match (d0, ins.op1_kind()) {
                    (Dst::Mem { addr, size }, OpKind::Register) => {
                        // signed bit offset reaches outside the operand: word index = floor(so / bits)
                        let so = self.tmp();
                        self.push_v(off);
                        if sz == 2 {
                            self.c.op(I32_EXTEND16_S);
                        }
                        self.c.lset(so);
                        let a2 = self.tmp();
                        self.c.lget(so).i32c(lg).op(I32_SHR_S).i32c(shsz).op(I32_SHL).lget(addr).op(I32_ADD).lset(a2);
                        let bit = self.tmp();
                        self.c.lget(so).i32c(bits as i32 - 1).op(I32_AND).lset(bit);
                        (Dst::Mem { addr: a2, size }, V::L(bit))
                    }
                    (_, _) => match off {
                        V::C(c) => (d0, V::C(c & (bits - 1))),
                        v => {
                            self.push_v(v);
                            self.c.i32c(bits as i32 - 1).op(I32_AND);
                            (d0, self.to_tmp())
                        }
                    },
                };
                let v = self.read_dst(d, ip);
                let v = self.mask_copy(v, sz);
                // CF = (v >> bit) & 1
                self.push_v(v);
                self.push_v(bit);
                self.c.op(I32_SHR_U).i32c(1).op(I32_AND);
                let cf = self.to_tmp();
                let pres = self.old_bits(ALLF & !CF);
                self.push_v(cf);
                self.rec_full_stack(pres);
                if mn != M::Bt {
                    self.push_v(v);
                    match mn {
                        M::Bts => {
                            self.c.i32c(1);
                            self.push_v(bit);
                            self.c.op(I32_SHL).op(I32_OR);
                        }
                        M::Btr => {
                            self.c.i32c(1);
                            self.push_v(bit);
                            self.c.op(I32_SHL).i32c(-1).op(I32_XOR).op(I32_AND);
                        }
                        _ => {
                            self.c.i32c(1);
                            self.push_v(bit);
                            self.c.op(I32_SHL).op(I32_XOR);
                        }
                    }
                    let nv = self.to_tmp();
                    self.write_dst(d, nv, ip);
                }
                true
            }
            M::Bsf | M::Bsr => {
                if !Self::operands_ok(ins) {
                    return false;
                }
                let sz = Self::opsz(ins, 0);
                let src = self.read_src(ins, 1, ip);
                let src = self.mask_copy(src, sz);
                let z = self.tmp();
                self.push_v(src);
                self.c.op(I32_EQZ).lset(z);
                let pres = self.old_bits(ALLF & !ZF);
                self.c.lget(z).i32c(6).op(I32_SHL);
                self.rec_full_stack(pres);
                self.c.lget(z).op(I32_EQZ);
                self.if_(BT_EMPTY);
                self.push_v(src);
                if mn == M::Bsf {
                    self.c.op(I32_CTZ);
                } else {
                    self.c.op(I32_CLZ).i32c(31).op(I32_XOR); // 31 - clz == 31 ^ clz for clz in 0..=31
                }
                self.rset(ins.op0_register());
                self.close();
                true
            }
            M::Bswap => {
                let r = ins.op0_register();
                if !r.is_gpr32() {
                    return false;
                }
                let x = Self::r32(r.number());
                self.c.lget(x).i32c(24).op(I32_SHL);
                self.c.lget(x).u32c(0xff00).op(I32_AND).i32c(8).op(I32_SHL).op(I32_OR);
                self.c.lget(x).i32c(8).op(I32_SHR_U).u32c(0xff00).op(I32_AND).op(I32_OR);
                self.c.lget(x).i32c(24).op(I32_SHR_U).op(I32_OR);
                self.rset32(r.number());
                true
            }
            M::Xadd => {
                if !Self::operands_ok(ins) {
                    return false;
                }
                let sz = Self::opsz(ins, 0);
                let d0 = self.dst_of(ins, 0);
                let d1 = self.dst_of(ins, 1);
                let a = self.read_dst(d0, ip);
                let a = self.mask_copy(a, sz);
                let b = self.read_dst(d1, ip);
                let b = self.mask_copy(b, sz);
                self.push_v(a);
                self.push_v(b);
                self.c.op(I32_ADD);
                if sz != 4 {
                    self.c.u32c(mask_of(sz)).op(I32_AND);
                }
                let sum = self.to_tmp();
                self.rec(lk(OP_ADD, sz), a, b, sum);
                self.write_dst(d1, a, ip);
                self.write_dst(d0, sum, ip);
                self.smc_check_ip(next);
                true
            }
            M::Cmpxchg => {
                if !Self::operands_ok(ins) {
                    return false;
                }
                let sz = Self::opsz(ins, 0);
                let d = self.dst_of(ins, 0);
                let dv = self.read_dst(d, ip);
                let dv = self.mask_copy(dv, sz);
                let acc_reg = match sz {
                    1 => Register::AL,
                    2 => Register::AX,
                    _ => Register::EAX,
                };
                self.rget(acc_reg);
                let acc = self.to_tmp();
                self.push_v(acc);
                self.push_v(dv);
                self.c.op(I32_SUB);
                if sz != 4 {
                    self.c.u32c(mask_of(sz)).op(I32_AND);
                }
                let res = self.to_tmp();
                self.rec(lk(OP_SUB, sz), acc, dv, res);
                self.push_v(acc);
                self.push_v(dv);
                self.c.op(I32_EQ);
                self.if_(BT_EMPTY);
                let v = self.read_src(ins, 1, ip);
                self.write_dst(d, v, ip);
                self.else_();
                // like hardware (and the interpreter): a failed compare still writes the destination back
                if let Dst::Mem { .. } = d {
                    self.write_dst(d, dv, ip);
                }
                self.push_v(dv);
                self.rset(acc_reg);
                self.close();
                self.smc_check_ip(next);
                true
            }
            M::Lahf => {
                self.flag_bits();
                self.c.i32c(0xd5).op(I32_AND).i32c(2).op(I32_OR);
                self.rset(Register::AH);
                true
            }
            M::Sahf => {
                self.rget(Register::AH);
                self.c.i32c(0xd5).op(I32_AND);
                let newf = self.to_tmp();
                let pres = self.old_bits(OF);
                self.push_v(newf);
                self.rec_full_stack(pres);
                true
            }
            M::Clc | M::Stc | M::Cmc => {
                let keep = if mn == M::Cmc { ALLF & !CF } else { ALLF & !CF };
                let cur_cf = if mn == M::Cmc {
                    self.carry();
                    Some(self.to_tmp())
                } else {
                    None
                };
                let pres = self.old_bits(keep);
                match mn {
                    M::Clc => {
                        self.c.i32c(0);
                    }
                    M::Stc => {
                        self.c.i32c(1);
                    }
                    _ => {
                        self.push_v(cur_cf.unwrap());
                        self.c.i32c(1).op(I32_XOR);
                    }
                }
                self.rec_full_stack(pres);
                true
            }
            M::Cld | M::Std => {
                let l = self.lay;
                self.c.lget(L_CPU).lget(L_CPU).mem(I32_LOAD, l.cpu_fl);
                if mn == M::Cld {
                    self.c.u32c(!DF).op(I32_AND);
                } else {
                    self.c.u32c(DF).op(I32_OR);
                }
                self.c.mem(I32_STORE, l.cpu_fl);
                true
            }
            M::Prefetchnta | M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchw | M::Prefetch | M::Lfence | M::Sfence | M::Mfence | M::Wait => true, // no-ops in the interpreter too
            M::Movsb | M::Movsw | M::Movsd | M::Stosb | M::Stosw | M::Stosd if self.str_ins(ins, ip, mn) => true,
            _ => self.fp_ins(ins, ip, mn) || self.x87_ins(ins, ip, mn) || (self.lay.simd && self.simd_ins(ins, ip, mn)),
        }
    }

    /// Audited, tag-free SSE data movement and bitwise operations. No floating-
    /// point arithmetic/rounding is involved. Memory alignment/protection failures
    /// return S_RETRY before modifying any state; the dispatcher retains its usual
    /// fault and self-modifying-code semantics. CPU offsets stay ABI-checked.
    fn portable_sse_bits(&mut self, ins: &Instruction, ip: u32, mn: M) -> bool {
        let scalar = matches!(mn, M::Movss | M::Movsd);
        let movq = mn == M::Movq;
        let movd = mn == M::Movd;
        let full = matches!(mn, M::Movapd | M::Movaps | M::Movupd | M::Movups | M::Movdqa | M::Movdqu);
        let logic = match mn {
            M::Xorpd | M::Xorps | M::Pxor => Some(I64_XOR),
            M::Andpd | M::Andps | M::Pand => Some(I64_AND),
            M::Orpd | M::Orps | M::Por => Some(I64_OR),
            _ => None,
        };
        if !(scalar || movq || movd || full || logic.is_some()) { return false; }
        let xmm = |i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_xmm() && ins.op_register(i).number() < 8;
        let mem = |i| ins.op_kind(i) == OpKind::Memory;
        let gpr = |i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_gpr32();
        if ins.op_count() != 2 || !(xmm(0) && (xmm(1) || mem(1) || movd && gpr(1)) || (mem(0) || movd && gpr(0)) && xmm(1)) { return false; }
        if logic.is_some() && !xmm(0) { return false; }
        let size = if movd || mn == M::Movss { 4 } else if scalar || movq { 8 } else { 16 };
        let address = if mem(0) || mem(1) {
            if ins.memory_size().size() != size as usize { return false; }
            let Some(a) = self.ea(ins, true) else { return false };
            let aligned = matches!(mn, M::Movapd | M::Movaps | M::Movdqa) || logic.is_some();
            if aligned {
                self.c.lget(a).i32c(15).op(I32_AND);
                self.if_(BT_EMPTY);
                self.retry_exit(ip);
                self.close();
            }
            self.check_or_retry(a, size, if mem(0) { self.lay.wtab } else { self.lay.rtab }, ip);
            Some(a)
        } else { None };
        let xoff = |i| self.lay.cpu_xmm + 16 * ins.op_register(i).number() as u32;
        let source_offset = if xmm(1) { xoff(1) } else { 0 };
        let dest_offset = if xmm(0) { xoff(0) } else { 0 };
        let load = if size == 4 { I32_LOAD } else { I64_LOAD };
        let store = if size == 4 { I32_STORE } else { I64_STORE };
        if gpr(1) {
            self.c.lget(Self::r32(ins.op1_register().number()));
        } else if mem(1) {
            self.c.lget(address.unwrap()).reloc_mem(load, self.lay.guest_base, 1);
        } else {
            self.c.lget(L_CPU).mem(load, source_offset);
        }
        let low = if size == 4 { self.tmp() } else { L_T64 };
        self.c.lset(low);
        if size == 16 {
            if mem(1) {
                // The +8 is arithmetic on the guest address, NOT an addend to a
                // relocatable Wasm memarg (which must remain exactly guest_base).
                self.c.lget(address.unwrap()).i32c(8).op(I32_ADD).reloc_mem(I64_LOAD, self.lay.guest_base, 1);
            } else {
                self.c.lget(L_CPU).mem(I64_LOAD, source_offset + 8);
            }
            self.c.lset(L_T64 + 1);
        }
        if gpr(0) {
            self.c.lget(low);
            self.rset(ins.op0_register());
        } else if mem(0) {
            self.c.lget(address.unwrap()).lget(low).reloc_mem(store, self.lay.guest_base, 1);
            if size == 16 { self.c.lget(address.unwrap()).i32c(8).op(I32_ADD).lget(L_T64 + 1).reloc_mem(I64_STORE, self.lay.guest_base, 1); }
        } else {
            self.c.lget(L_CPU).lget(low);
            if let Some(op) = logic { self.c.lget(L_CPU).mem(I64_LOAD, dest_offset).op(op); }
            self.c.mem(store, dest_offset);
            if size == 16 {
                self.c.lget(L_CPU).lget(L_T64 + 1);
                if let Some(op) = logic { self.c.lget(L_CPU).mem(I64_LOAD, dest_offset + 8).op(op); }
                self.c.mem(I64_STORE, dest_offset + 8);
            } else if movd || movq || scalar && mem(1) {
                if size == 4 { self.c.lget(L_CPU).i32c(0).mem(I32_STORE, dest_offset + 4); }
                self.c.lget(L_CPU).i64c(0).mem(I64_STORE, dest_offset + 8);
            }
        }
        self.ins_dirty = true;
        true
    }
}

#[path = "jit_simd.rs"]
mod simd;
#[path = "jit_fp.rs"]
mod fp;
#[path = "jit_str.rs"]
mod strops;
#[path = "jit_x87.rs"]
mod x87;
#[path = "jit_intr.rs"]
pub mod intr;
pub use intr::detect as intr_detect;

#[derive(Clone, Copy)]
pub enum EdgeKind {
    Static(u32),
}

// =====================================================================================
// Region / module assembly
// =====================================================================================

struct Plan {
    /// chain index of every node (heads and merged nodes)
    chain_of: Vec<usize>,
    chains: Vec<Vec<usize>>,
    /// node index -> chain index if it is a head
    head_chain: Vec<Option<usize>>,
    by_start: HashMap<u32, usize>,
}

fn plan(nodes: &[NodeIn]) -> Plan {
    let n = nodes.len();
    let mut by_start = HashMap::new();
    for (i, nd) in nodes.iter().enumerate() {
        by_start.entry(nd.start).or_insert(i);
    }
    let term: Vec<Term> = nodes.iter().map(classify).collect();
    let mut preds = vec![0u32; n];
    let mut forced_head = vec![false; n];
    forced_head[0] = true;
    preds[0] += 1;
    let add = |preds: &mut Vec<u32>, t: u32| {
        if let Some(&j) = by_start.get(&t) {
            preds[j] += 1;
        }
    };
    for (i, nd) in nodes.iter().enumerate() {
        match term[i] {
            Term::Fall => add(&mut preds, nd.end),
            Term::Cond(t) => {
                add(&mut preds, t);
                add(&mut preds, nd.end);
            }
            Term::Jmp(t) => add(&mut preds, t),
            Term::Call(t) => {
                add(&mut preds, t);
                // return site: entered through `ret`
                if let Some(&j) = by_start.get(&nd.end) {
                    forced_head[j] = true;
                }
            }
            Term::Other => {}
        }
    }
    // merge: j follows i inline when i's only way on is into j and j has no other entry
    let mut merged_into: Vec<Option<usize>> = vec![None; n];
    let mut next_of: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let target = match term[i] {
            Term::Fall | Term::Cond(_) => Some(nodes[i].end),
            Term::Jmp(t) => Some(t),
            _ => None,
        };
        if let Some(t) = target {
            if let Some(&j) = by_start.get(&t) {
                // For a conditional, `t` must be the fallthrough (not the taken target) to be merged inline.
                let ok_edge = match term[i] {
                    Term::Cond(tt) => tt != nodes[i].end,
                    _ => true,
                };
                if j != i && ok_edge && preds[j] == 1 && !forced_head[j] && merged_into[j].is_none() && next_of[i].is_none() {
                    merged_into[j] = Some(i);
                    next_of[i] = Some(j);
                }
            }
        }
    }
    let mut chains: Vec<Vec<usize>> = Vec::new();
    let mut chain_of = vec![usize::MAX; n];
    let mut head_chain = vec![None; n];
    for i in 0..n {
        if merged_into[i].is_none() {
            let ci = chains.len();
            let mut ch = vec![i];
            chain_of[i] = ci;
            head_chain[i] = Some(ci);
            let mut cur = i;
            while let Some(j) = next_of[cur] {
                ch.push(j);
                chain_of[j] = ci;
                cur = j;
            }
            chains.push(ch);
        }
    }
    // Anything left over (cycle of merged nodes cannot happen, but be safe): own chain.
    for i in 0..n {
        if chain_of[i] == usize::MAX {
            let ci = chains.len();
            chains.push(vec![i]);
            chain_of[i] = ci;
            head_chain[i] = Some(ci);
        }
    }
    Plan { chain_of, chains, head_chain, by_start }
}

/// Imported-function type signatures: exec(cpu, tag) / load(cpu, addr, size, ip) / store(cpu, addr, size, val, ip) / mat(cpu, lk, a, b, r).
pub const IMPORT_MODULE: &str = "env";

fn translate_region(
    mb_types: (u32, u32, u32),
    fi: u32,
    lay: &Layout,
    nodes: &[NodeIn],
    fb: &mut dyn FnMut(&Instruction) -> u32,
    stats: &mut FbStats,
    prof_slot0: u32,
    relocatable: bool,
) -> (u32, u32, Code, RegionOut) {
    // Pass 1 discovers which guest registers translated code writes (nothing else needs storing around
    // interpreter calls); its output is thrown away.
    let mut dummy_stats = FbStats::new();
    let mut dummy_fb = |_: &Instruction| 0u32;
    let (_, _, _, _, written) = emit_region(mb_types, fi, lay, nodes, &mut dummy_fb, &mut dummy_stats, 0xff, prof_slot0, relocatable);
    let (a, b, c, d, _) = emit_region(mb_types, fi, lay, nodes, fb, stats, written, prof_slot0, relocatable);
    (a, b, c, d)
}

fn emit_region(
    mb_types: (u32, u32, u32),
    fi: u32,
    lay: &Layout,
    nodes: &[NodeIn],
    fb: &mut dyn FnMut(&Instruction) -> u32,
    stats: &mut FbStats,
    known_written: u32,
    prof_slot0: u32,
    relocatable: bool,
) -> (u32, u32, Code, RegionOut, u32) {
    let _ = (mb_types, fi);
    let pl = plan(nodes);
    let mut fx = Fx {
        lay,
        c: if relocatable { Code::relocatable() } else { Code::new() },
        labels: Vec::new(),
        next_label: 0,
        tmp_top: 0,
        tmp_max: 0,
        st: St::Dyn,
        written: 0,
        had_store: false,
        fb,
        stats,
        n_fb: 0,
        n_ins: 0,
        n_intr: 0,
        live_after: Vec::new(),
        k: 0,
        live: ALLF,
        known_written,
        ifac: iced_x86::InstructionInfoFactory::new(),
        ret_sites: Vec::new(),
        inline_next: None,
        exit_class: 0,
        exit_sites: Vec::new(),
        exit_slot0: 0,
        cur_chain: 0,
        cur_loop: None,
        node_thunk: 0,
        head_labels: Vec::new(),
        used_retm: false,
        mmx_known: false,
        mm_tagged: 0,
        cur_ip: 0,
        cur_mn: M::INVALID,
        mat_sites: Vec::new(),
        ins_dirty: false,
        guarded: None,
    };
    // return sites that are chain heads: `ret` can jump straight to them
    for (i, nd) in nodes.iter().enumerate() {
        if let Term::Call(_) = classify(nd) {
            if let Some(&j) = pl.by_start.get(&nd.end) {
                if let Some(ci) = pl.head_chain[j] {
                    if !fx.ret_sites.iter().any(|&(a, _)| a == nd.end) && fx.ret_sites.len() < 64 {
                        fx.ret_sites.push((nd.end, ci as u32));
                    }
                }
            }
        }
        let _ = i;
    }
    let nchains = pl.chains.len();
    fx.exit_slot0 = prof_slot0 + nchains as u32 + 1;
    // --- body: dispatch loop ---
    fx.c.block(BT_EMPTY);
    fx.open(LB_RETD);
    fx.c.block(BT_EMPTY);
    fx.open(LB_EXIT);
    fx.c.block(BT_EMPTY);
    fx.open(LB_FAULT);
    fx.c.loop_(BT_EMPTY);
    fx.open(LB_DISP);
    fx.c.block(BT_EMPTY);
    fx.open(LB_RETM);
    // blocks for the chain heads, innermost = chain 0
    let mut head_labels = Vec::new();
    for _ in 0..nchains {
        let l = fx.new_label();
        head_labels.push(l);
    }
    fx.head_labels = head_labels.clone();
    for k in (0..nchains).rev() {
        fx.c.block(BT_EMPTY);
        fx.open(head_labels[k]);
    }
    let targets: Vec<u32> = (0..nchains as u32).collect();
    fx.c.lget(L_PC).br_table(&targets, nchains as u32 - 1);
    // chain k's code follows `end` of its block
    for k in 0..nchains {
        fx.close(); // end of head block k
        fx.cur_chain = k;
        fx.st = St::Dyn;
        fx.mmx_known = false;
        fx.mm_tagged = 0;
        let chain = &pl.chains[k];
        // a chain that can branch back to its own head becomes a wasm loop (the budget check below is part of every iteration)
        let head_start = nodes[chain[0]].start;
        let self_edge = nodes[chain[0]].intrinsic == 0
            && chain.iter().any(|&ni| match classify(&nodes[ni]) {
                Term::Cond(t) | Term::Jmp(t) | Term::Call(t) => t == head_start,
                _ => false,
            });
        fx.cur_loop = None;
        if self_edge {
            let l = fx.new_label();
            fx.c.loop_(BT_EMPTY);
            fx.open(l);
            fx.cur_loop = Some(l);
        }
        // budget: one check per chain entry
        let total: u32 = chain.iter().map(|&ni| nodes[ni].ins.len() as u32).sum();
        {
            let flat: Vec<&Instruction> = chain.iter().flat_map(|&ni| nodes[ni].ins.iter()).collect();
            fx.live_after = liveness(&flat);
            fx.k = 0;
        }
        fx.c.lget(L_BUD).i32c(1).op(I32_LT_S);
        fx.if_(BT_EMPTY);
        fx.exit_class = 4;
        fx.exit_to(nodes[chain[0]].start, S_NEXT);
        fx.close();
        fx.c.lget(L_BUD).u32c(total).op(I32_SUB).lset(L_BUD);
        if lay.prof != 0 {
            let a = lay.prof + 8 * (prof_slot0 + k as u32);
            fx.c.i32c(0).i32c(0).mem(I64_LOAD, a).i64c(total as i64).op(I64_ADD).mem(I64_STORE, a);
        }
        // (regions that retry a lot are recompiled with inline slow paths: the fast path only exists for the retry layout)
        if nodes[chain[0]].intrinsic != 0 && !lay.slow_inline {
            match nodes[chain[0]].intrinsic {
                intr::INTR_FIND => fx.intr_find(nodes[chain[0]].start),
                intr::INTR_BSEARCH => fx.intr_bsearch(nodes[chain[0]].start),
                intr::INTR_RANGE_CMP => fx.intr_range_cmp(nodes[chain[0]].start),
                _ => unreachable!(),
            }
            fx.n_intr += 1;
            continue;
        }
        let mut stopped = false;
        for (pos, &ni) in chain.iter().enumerate() {
            let nd = &nodes[ni];
            let last_node = pos + 1 == chain.len();
            let mut edge = |fx: &mut Fx, e: EdgeKind| -> bool {
                match e {
                    EdgeKind::Static(t) => match pl.by_start.get(&t).and_then(|&j| pl.head_chain[j]) {
                        Some(ci) => {
                            fx.goto_chain(ci as u32);
                            true
                        }
                        None => {
                            fx.exit_to(t, S_NEXT);
                            false
                        }
                    },
                }
            };
            fx.inline_next = chain.get(pos + 1).map(|&nj| nodes[nj].start);
            fx.node_thunk = nd.thunk;
            for ins in nd.ins.iter() {
                let fl = fx.ins(ins, &mut edge);
                if fl == Flow::Stop {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
            // fell off the end of the node
            if last_node {
                edge(&mut fx, EdgeKind::Static(nd.end));
                stopped = true;
            }
        }
        debug_assert!(stopped);
        if !stopped {
            fx.c.unreachable();
        }
        if fx.cur_loop.take().is_some() {
            fx.close(); // the chain's loop
        }
    }
    // all chains end in a branch
    fx.c.unreachable();
    fx.close(); // $retm: `ret` instructions arrive here with the return address in L_NEXT
    if fx.used_retm {
        let mut sites = fx.ret_sites.clone();
        sites.sort();
        fx.ret_search(&sites);
        fx.exit_pending(2, S_NEXT);
    } else {
        fx.c.unreachable();
    }
    fx.close(); // loop
    fx.close(); // $fault block: fault handler code follows
    // fault: eip of the faulting instruction was stored in cpu.jit_aux by the slow helper (read via L_NEXT)
    fx.c.lget(L_CPU).mem(I32_LOAD, lay.cpu_aux).lset(L_NEXT);
    fx.c.u32c(S_SEGV).lset(L_STATUS);
    fx.close(); // $exit block: write back
    // write back
    let written = fx.written;
    for i in 0..8u32 {
        if written & (1 << i) != 0 {
            fx.c.lget(L_CPU).lget(L_R0 + i).mem(I32_STORE, lay.cpu_r + 4 * i);
        }
    }
    fx.store_tuple();
    fx.c.lget(L_CPU).lget(L_NEXT).mem(I32_STORE, lay.cpu_eip);
    fx.c.lget(L_CPU).lget(L_BUD).mem(I32_STORE, lay.cpu_budget);
    if lay.cache != 0 {
        // Chain: a plain "continue at eip" exit with budget left tail-calls the compiled region that starts at the
        // target, if the direct-mapped table knows one (all state is in Cpu). Anything else returns to the dispatcher.
        let cb = lay.cache;
        fx.c.lget(L_STATUS).op(I32_EQZ).lget(L_BUD).i32c(0).op(I32_GT_S).op(I32_AND);
        fx.if_(BT_EMPTY);
        fx.c.lget(L_NEXT).lget(L_NEXT).i32c(12).op(I32_SHR_U).op(I32_XOR).u32c(CACHE_ENTRIES - 1).op(I32_AND).i32c(4).op(I32_SHL).lset(L_X);
        fx.c.lget(L_X).reloc_mem(I32_LOAD, cb, 4).lget(L_NEXT).op(I32_EQ);
        fx.c.lget(L_X).reloc_mem(I32_LOAD, cb + 4, 4).i32c(0).op(I32_NE).op(I32_AND);
        fx.if_(BT_EMPTY);
        fx.c.lget(L_CPU).lget(L_X).reloc_mem(I32_LOAD, cb + 8, 4);
        fx.c.lget(L_X).reloc_mem(I32_LOAD, cb + 4, 4).i32c(1).op(I32_SUB);
        fx.c.return_call_indirect(0, 0);
        fx.close();
        fx.close();
    }
    fx.close(); // $retd
    fx.c.lget(L_STATUS);
    let body = std::mem::take(&mut fx.c);

    // --- prologue ---
    let mut pro = Code::new();
    if lay.prof != 0 {
        let a = lay.prof + 8 * (prof_slot0 + nchains as u32);
        pro.i32c(0).i32c(0).mem(I64_LOAD, a).i64c(1).op(I64_ADD).mem(I64_STORE, a);
    }
    for i in 0..8u32 {
        pro.lget(L_CPU).mem(I32_LOAD, lay.cpu_r + 4 * i).lset(L_R0 + i);
    }
    pro.lget(L_CPU).mem(I32_LOAD, lay.cpu_lf_kind).lset(L_LK);
    pro.lget(L_CPU).mem(I32_LOAD, lay.cpu_lf_a).lset(L_LA);
    pro.lget(L_CPU).mem(I32_LOAD, lay.cpu_lf_b).lset(L_LB);
    pro.lget(L_CPU).mem(I32_LOAD, lay.cpu_lf_r).lset(L_LR);
    pro.lget(L_CPU).mem(I32_LOAD, lay.cpu_budget).lset(L_BUD);
    pro.i32c(0).lset(L_SMC);
    let mut full = pro;
    full.append(&body);

    let mut out = RegionOut { n_ins: fx.n_ins, n_fallback: fx.n_fb, n_intr: fx.n_intr, exits: std::mem::take(&mut fx.exit_sites), mat_sites: fx.mat_sites.iter().map(|(a, m)| (*a, format!("{:?}", m))).collect(), ..Default::default() };
    for ch in pl.chains.iter() {
        out.chains.push((nodes[ch[0]].start, ch.iter().map(|&ni| nodes[ni].ins.len() as u32).sum()));
    }
    for (ni, hc) in pl.head_chain.iter().enumerate() {
        if let Some(ci) = hc {
            out.heads.push((nodes[ni].start, *ci as u32));
        }
    }
    let _ = pl.chain_of;
    let n_i32_locals = (L_TMP0 - L_R0) + fx.tmp_max;
    (n_i32_locals, N_T64, full, out, fx.written)
}

/// Translate a batch of regions into one wasm module exporting `r0..rN`. Imports: memory, table and the four
/// helper functions (see `F_*`). `fb` is called once per fallback instruction and returns the tag that
/// `exec_one` will receive for it.
pub fn translate_module(lay: &Layout, regions: &[Vec<NodeIn>], fb: &mut dyn FnMut(&Instruction) -> u32, stats: &mut FbStats, prof_slot0: u32) -> (Vec<u8>, Vec<RegionOut>) {
    let (wasm, outputs, _) = translate_module_mode(lay, regions, fb, stats, prof_slot0, false);
    (wasm, outputs)
}

/// Like translate_module, additionally retaining explicit assembler relocation sites.
/// Only the audited integer AOT subset may be rebound: other emitter modules do not
/// yet annotate all of their address operands.
pub fn translate_module_relocatable(lay: &Layout, regions: &[Vec<NodeIn>], fb: &mut dyn FnMut(&Instruction) -> u32, stats: &mut FbStats, prof_slot0: u32) -> (Vec<u8>, Vec<RegionOut>, Vec<crate::wasm_emit::Relocation>) {
    translate_module_mode(lay, regions, fb, stats, prof_slot0, true)
}

fn translate_module_mode(lay: &Layout, regions: &[Vec<NodeIn>], fb: &mut dyn FnMut(&Instruction) -> u32, stats: &mut FbStats, prof_slot0: u32, relocatable: bool) -> (Vec<u8>, Vec<RegionOut>, Vec<crate::wasm_emit::Relocation>) {
    let mut mb = ModuleBuilder::new();
    let t_cpu_i32 = mb.ty(&[I32, I32], &[I32]);
    let t4 = mb.ty(&[I32, I32, I32, I32], &[I32]);
    let t5 = mb.ty(&[I32, I32, I32, I32, I32], &[I32]);
    let t6 = mb.ty(&[I32, I32, I32, I32, I32, I32], &[I32]);
    let f_exec = mb.import_func(IMPORT_MODULE, "exec_one", t_cpu_i32);
    let f_load = mb.import_func(IMPORT_MODULE, "load_slow", t4);
    let f_store = mb.import_func(IMPORT_MODULE, "store_slow", t5);
    let f_mat = mb.import_func(IMPORT_MODULE, "mat", t6);
    let t_cpu = mb.ty(&[I32], &[I32]);
    let f_sys = mb.import_func(IMPORT_MODULE, "sys_time", t_cpu);
    let t3 = mb.ty(&[I32, I32, I32], &[I32]);
    let f_int64 = mb.import_func(IMPORT_MODULE, "fist_fild", t4);
    let f_fcomip = mb.import_func(IMPORT_MODULE, "fcomip", t3);
    let f_cvt = mb.import_func(IMPORT_MODULE, "cvtss2si", t5);
    let f_mm = mb.import_func(IMPORT_MODULE, "mm_op", t5);
    debug_assert_eq!((f_int64, f_fcomip, f_cvt, f_mm), (F_FIST_FILD, F_FCOMIP, F_CVTSS2SI, F_MM_OP));
    debug_assert_eq!((f_exec, f_load, f_store, f_mat, f_sys), (F_EXEC, F_LOAD, F_STORE, F_MAT, F_SYS));
    mb.import_memory(IMPORT_MODULE, "memory", 1);
    mb.import_table(IMPORT_MODULE, "table", 0);
    let mut outs = Vec::new();
    let mut slot = prof_slot0;
    for (ri, nodes) in regions.iter().enumerate() {
        let (n32, n64, body, out) = translate_region((t_cpu_i32, t4, t5), ri as u32, lay, nodes, fb, stats, slot, relocatable);
        slot += out.chains.len() as u32 + 1 + out.exits.len() as u32;
        let fidx = mb.add_code_func(t_cpu_i32, n32, n64, N_V128, body);
        mb.export_func(&format!("r{}", ri), fidx);
        // the guest address the region starts at: CPU profiles then show where the time goes in guest terms
        mb.name_func(fidx, &format!("R{:08x}", out.chains.first().map_or(0, |c| c.0)));
        outs.push(out);
    }
    let _ = N_IMPORTS;
    let (wasm, relocations) = mb.finish_relocatable();
    (wasm, outs, relocations)
}
