//! Native differential tests for the JIT translator.
//!
//! The generated wasm is validated and *executed* by wasmi on the host, through the real
//! `Cpu::run` / `BlockCache` tiering path (a wasmi-backed `rt::Backend` stands in for the browser's
//! table-install glue), and compared bit for bit with the interpreter: exit reason, all registers,
//! eip, EFLAGS (after folding lazy flags), the whole guest memory and the TSC.
//!
//! Random programs are built from byte templates (opcode + random ModRM/SIB/displacement/immediate),
//! so every addressing mode, operand size and flag corner gets exercised. Faults, partial register
//! writes, page-crossing accesses, write-only/read-only/PROT_NONE pages and stores into decoded-code
//! pages are all part of the guest memory map.

use super::*;
use crate::mem::*;
use crate::run::BlockCache;
use std::mem::size_of;
use wasmi::{Caller, Engine, Linker, Module, Store, TypedFunc};

#[path = "aot_runtime_tests.rs"]
mod aot_runtime_tests;

#[path = "entry_probe_tests.rs"]
mod entry_probe_tests;

#[path = "lir_region_tests.rs"]
mod lir_region_tests;

#[path = "lir_memory_tests.rs"]
mod lir_memory_tests;

thread_local! { static TEST_TIME_HELPER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

// ---- wasmi memory map ------------------------------------------------------------------------
const CPU_PTR: u32 = 0x1000;
const RTAB: u32 = 0x10000;
const WTAB: u32 = RTAB + (1 << 20);
const GUEST: u32 = WTAB + (1 << 20);
const GUEST_SIZE: usize = 1 << 20;
const WASM_PAGES: u32 = ((GUEST as usize + GUEST_SIZE + 0xffff) >> 16) as u32;

struct Hc {
    cpu: *mut Cpu,
    wmem: Option<wasmi::Memory>,
}
unsafe impl Send for Hc {}
unsafe impl Sync for Hc {}

struct WasmiBackend {
    store: Store<Hc>,
    linker: Linker<Hc>,
    engine: Engine,
    memory: wasmi::Memory,
    funcs: Vec<Option<TypedFunc<(i32, i32), i32>>>,
}

/// wasmi image of the live Rust state <-> back.
fn sync_in(caller: &mut Caller<'_, Hc>) -> (&'static mut Cpu, &'static mut Memory, &'static mut JitState) {
    let wm = caller.data().wmem.unwrap();
    let cpu = unsafe { &mut *caller.data().cpu };
    let (mem, jit) = unsafe { ctx::get() };
    let (mem, jit) = unsafe { (&mut *mem, &mut *jit) };
    let bytes = wm.data_mut(&mut *caller);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().add(CPU_PTR as usize), cpu as *mut Cpu as *mut u8, size_of::<Cpu>()) };
    mem.raw_mut().copy_from_slice(&bytes[GUEST as usize..GUEST as usize + GUEST_SIZE]);
    (cpu, mem, jit)
}
fn sync_out(caller: &mut Caller<'_, Hc>, cpu: &Cpu, mem: &Memory) {
    let wm = caller.data().wmem.unwrap();
    let bytes = wm.data_mut(&mut *caller);
    unsafe { std::ptr::copy_nonoverlapping(cpu as *const Cpu as *const u8, bytes.as_mut_ptr().add(CPU_PTR as usize), size_of::<Cpu>()) };
    bytes[GUEST as usize..GUEST as usize + GUEST_SIZE].copy_from_slice(mem.raw());
}

impl WasmiBackend {
    fn new() -> WasmiBackend {
        let engine = Engine::default();
        let mut store = Store::new(&engine, Hc { cpu: std::ptr::null_mut(), wmem: None });
        let memory = wasmi::Memory::new(&mut store, wasmi::MemoryType::new(WASM_PAGES, None)).unwrap();
        store.data_mut().wmem = Some(memory);
        let table = wasmi::Table::new(&mut store, wasmi::TableType::new(wasmi::RefType::Func, 0, None), wasmi::Ref::Func(wasmi::Nullable::Null)).unwrap();
        let mut linker: Linker<Hc> = Linker::new(&engine);
        linker.define("env", "memory", memory).unwrap();
        linker.define("env", "table", table).unwrap();
        linker
            .func_wrap("env", "exec_one", |mut caller: Caller<'_, Hc>, _cpu: i32, tag: i32| -> i32 {
                let (cpu, mem, jit) = sync_in(&mut caller);
                let ins = *jit.fb_ins(tag as u32);
                if std::env::var("JIT_TRACE").is_ok() {
                    eprintln!("[exec_one] {:?} before: r={:x?} fl={:#x} lf=({},{:#x},{:#x},{:#x})", ins.code(), cpu.r, cpu.fl, cpu.lf_kind, cpu.lf_a, cpu.lf_b, cpu.lf_r);
                }
                let r = h_exec_one(cpu, mem, &ins, &mut jit.pending);
                if std::env::var("JIT_TRACE").is_ok() {
                    eprintln!("[exec_one]   after: r={:x?} fl={:#x} -> {}", cpu.r, cpu.fl, r);
                }
                sync_out(&mut caller, cpu, mem);
                r as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "load_slow", |mut caller: Caller<'_, Hc>, _cpu: i32, addr: i32, size: i32, ip: i32| -> i32 {
                let (cpu, mem, _jit) = sync_in(&mut caller);
                let r = h_load_slow(cpu, mem, addr as u32, size as u32, ip as u32);
                sync_out(&mut caller, cpu, mem);
                r as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "store_slow", |mut caller: Caller<'_, Hc>, _cpu: i32, addr: i32, size: i32, val: i32, ip: i32| -> i32 {
                let (cpu, mem, _jit) = sync_in(&mut caller);
                let r = h_store_slow(cpu, mem, addr as u32, size as u32, val as u32, ip as u32);
                sync_out(&mut caller, cpu, mem);
                r as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "mat", |mut caller: Caller<'_, Hc>, _cpu: i32, lk: i32, la: i32, lb: i32, lr: i32, _ip: i32| -> i32 {
                let wm = caller.data().wmem.unwrap();
                let bytes = wm.data_mut(&mut caller);
                // only `fl` is read from the cpu image
                let fl_off = CPU_PTR as usize + core::mem::offset_of!(Cpu, fl);
                let fl = u32::from_le_bytes(bytes[fl_off..fl_off + 4].try_into().unwrap());
                let mut c = Cpu::default();
                c.fl = fl;
                h_mat(&c, lk as u32, la as u32, lb as u32, lr as u32) as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "sys_time", |mut caller: Caller<'_, Hc>, _cpu: i32| -> i32 {
                if TEST_TIME_HELPER.with(|v| v.get()) {
                    let (cpu, mem, _) = sync_in(&mut caller);
                    if cpu.r[EAX] == 13 {
                        let addr = cpu.r[EBX];
                        mem.w32(addr, 123).unwrap();
                        cpu.r[EAX] = 123;
                        let status = if mem.needs_sync() { 2 } else { 0 };
                        sync_out(&mut caller, cpu, mem);
                        return status;
                    }
                }
                // the native test engine has no kernel to ask: fall back to the syscall exit,
                // exactly like the interpreter path (both engines then agree bit-for-bit)
                1
            })
            .unwrap();
        linker
            .func_wrap("env", "fist_fild", |mut caller: Caller<'_, Hc>, _cpu: i32, addr: i32, ip: i32, fist: i32| -> i32 {
                let (cpu, mem, jit) = sync_in(&mut caller);
                jit.stats.fp_helper_calls[0] += 1;
                let r = h_fist_fild(cpu, mem, addr as u32, ip as u32, fist as u32) as i32;
                sync_out(&mut caller, cpu, mem);
                r
            })
            .unwrap();
        linker
            .func_wrap("env", "fcomip", |mut caller: Caller<'_, Hc>, _cpu: i32, bisp: i32, ip: i32| -> i32 {
                let (cpu, mem, jit) = sync_in(&mut caller);
                jit.stats.fp_helper_calls[1] += 1;
                let r = h_fcomip(cpu, bisp as u32, ip as u32) as i32;
                sync_out(&mut caller, cpu, mem);
                r
            })
            .unwrap();
        linker
            .func_wrap("env", "cvtss2si", |mut caller: Caller<'_, Hc>, _cpu: i32, kind: i32, v: i32, dst: i32, ip: i32| -> i32 {
                let (cpu, mem, jit) = sync_in(&mut caller);
                jit.stats.fp_helper_calls[2] += 1;
                let r = h_cvtss2si(cpu, mem, kind as u32, v as u32, dst as u32, ip as u32) as i32;
                sync_out(&mut caller, cpu, mem);
                r
            })
            .unwrap();
        linker
            .func_wrap("env", "mm_op", |mut caller: Caller<'_, Hc>, _cpu: i32, opdst: i32, kind: i32, v: i32, ip: i32| -> i32 {
                let (cpu, mem, jit) = sync_in(&mut caller);
                jit.stats.fp_helper_calls[3] += 1;
                let r = h_mm_op(cpu, mem, opdst as u32, kind as u32, v as u32, ip as u32) as i32;
                sync_out(&mut caller, cpu, mem);
                r
            })
            .unwrap();
        WasmiBackend { store, linker, engine, memory, funcs: Vec::new() }
    }
}

impl rt::Backend for WasmiBackend {
    fn install(&mut self, bytes: &[u8], n: usize) -> Option<Vec<u32>> {
        if let Ok(path) = std::env::var("JIT_DUMP") {
            let _ = std::fs::write(path, bytes);
        }
        let module = match Module::new(&self.engine, bytes) {
            Ok(m) => m,
            Err(e) => {
                // a translator bug: never let it hide behind the interpreter fallback
                if let Ok(path) = std::env::var("JIT_DUMP_BAD") {
                    let _ = std::fs::write(path, bytes);
                }
                let code = CUR_CODE.with(|c| c.borrow().clone());
                panic!("generated wasm failed validation: {e}\ncode:\n{}", describe(&code));
            }
        };
        let inst = self.linker.instantiate_and_start(&mut self.store, &module).expect("instantiate");
        let mut slots = Vec::new();
        for i in 0..n {
            let f = inst.get_typed_func::<(i32, i32), i32>(&self.store, &format!("r{}", i)).unwrap();
            self.funcs.push(Some(f));
            slots.push((self.funcs.len() - 1) as u32);
        }
        Some(slots)
    }
    fn free(&mut self, slot: u32) {
        self.funcs[slot as usize] = None;
    }
    fn call(&mut self, slot: u32, cpu: &mut Cpu, head: u32) -> u32 {
        let (mem, _jit) = unsafe { ctx::get() };
        let mem = unsafe { &mut *mem };
        {
            let bytes = self.memory.data_mut(&mut self.store);
            unsafe { std::ptr::copy_nonoverlapping(cpu as *const Cpu as *const u8, bytes.as_mut_ptr().add(CPU_PTR as usize), size_of::<Cpu>()) };
            bytes[GUEST as usize..GUEST as usize + GUEST_SIZE].copy_from_slice(mem.raw());
            let (rt, wt) = mem.jit_tables();
            let n = GUEST_SIZE >> 12;
            bytes[RTAB as usize..RTAB as usize + n].copy_from_slice(&rt[..n]);
            bytes[WTAB as usize..WTAB as usize + n].copy_from_slice(&wt[..n]);
        }
        self.store.data_mut().cpu = cpu as *mut Cpu;
        let f = self.funcs[slot as usize].unwrap();
        let st = f.call(&mut self.store, (CPU_PTR as i32, head as i32)).expect("trap in generated code");
        let bytes = self.memory.data_mut(&mut self.store);
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().add(CPU_PTR as usize), cpu as *mut Cpu as *mut u8, size_of::<Cpu>()) };
        mem.raw_mut().copy_from_slice(&bytes[GUEST as usize..GUEST as usize + GUEST_SIZE]);
        if std::env::var("JIT_TRACE").is_ok() {
            eprintln!("[backend] region call slot={} head={} -> status {} eip={:#x} tuple=({:#x},{:#x},{:#x},{:#x})", slot, head, st, cpu.eip, cpu.lf_kind, cpu.lf_a, cpu.lf_b, cpu.lf_r);
        }
        st as u32
    }
}

fn install_backend() {
    rt::BACKEND.with(|b| *b.borrow_mut() = Some(Box::new(WasmiBackend::new())));
}

// ---- guest memory map ------------------------------------------------------------------------
const CODE: u32 = 0x10000;
const DATA: u32 = 0x30000;
const STACK: u32 = 0x40000;
/// page types inside the data window
const PG_RONLY: u32 = DATA + 0x3000;
const PG_WONLY: u32 = DATA + 0x4000;
const PG_NONE: u32 = DATA + 0x5000;
const PG_CODEFLAG: u32 = DATA + 0x6000;

thread_local! {
    /// fuzz_fp: restrict the generated floating-point values to benign ones (moderate normals, zeros, small integers)
    static FP_BENIGN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// fuzz_x87: x87 registers hold plausible values, control word with any rounding mode
    static X87_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// fuzz_mmx: random x87/MMX register file, TOP and tag word at the start
    static MMX_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// fuzz_fp: guest data and XMM registers hold realistic floating-point values instead of random bits
    static FP_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A random double as raw bits, with the classes the scalar fast paths care about.
fn fp_f64(rng: &mut Rng) -> u64 {
    // boundary values: smallest normal, its neighbours, the largest denormal / normal, 1 +- ulp, powers of two
    const SPECIAL: [u64; 16] = [
        0x0010_0000_0000_0000, 0x0010_0000_0000_0001, 0x000f_ffff_ffff_ffff, 0x7fef_ffff_ffff_ffff,
        0x3fef_ffff_ffff_ffff, 0x3ff0_0000_0000_0001, 0x3ff0_0000_0000_0000, 0x4000_0000_0000_0000,
        0x0020_0000_0000_0000, 0x0000_0000_0000_0001, 0x3fe0_0000_0000_0000, 0x7fe0_0000_0000_0000,
        0x41e0_0000_0000_0000, 0xc1e0_0000_0000_0000, 0x41df_ffff_ffc0_0000, 0xc1e0_0000_0020_0000,
    ];
    let benign = FP_BENIGN.with(|f| f.get());
    if !benign && rng.chance(8) {
        return rng.pick(&SPECIAL);
    }
    let sign = if rng.chance(40) { 1u64 << 63 } else { 0 };
    let mant = rng.next() & ((1u64 << 52) - 1);
    let v = match if benign { rng.below(65) } else { rng.below(100) } {
        0..=9 => 0,
        10..=34 => ((rng.below(2000) as f64) - 500.0).to_bits() & !(1u64 << 63),
        35..=64 => (((1023 - 30 + rng.below(60)) as u64) << 52) | mant,
        65..=70 => (((1023 + 900 + rng.below(120)) as u64) << 52) | mant,
        71..=76 => (rng.below(4) as u64) << 52 | mant, // denormal / smallest normals
        77..=80 => (((1023 - 1000 + rng.below(60)) as u64) << 52) | mant, // near underflow
        81..=84 => 0x7ff << 52,
        85..=89 => (0x7ff << 52) | (rng.next() & ((1u64 << 52) - 1)) | 1,
        90..=94 => ((rng.below(0x7ff) as u64) << 52) | mant,
        _ => ((rng.below(1 << 20) as f64) * 0.25).to_bits(),
    };
    v | sign
}

fn fp_f32(rng: &mut Rng) -> u32 {
    const SPECIAL: [u32; 12] = [0x0080_0000, 0x0080_0001, 0x007f_ffff, 0x7f7f_ffff, 0x3f7f_ffff, 0x3f80_0001, 0x3f80_0000, 0x4000_0000, 0x0100_0000, 0x4f00_0000, 0xcf00_0000, 0x4b80_0000];
    let benign = FP_BENIGN.with(|f| f.get());
    if !benign && rng.chance(8) {
        return rng.pick(&SPECIAL);
    }
    let sign = if rng.chance(40) { 1u32 << 31 } else { 0 };
    let mant = (rng.next() as u32) & ((1 << 23) - 1);
    let v = match if benign { rng.below(65) } else { rng.below(100) } {
        0..=9 => 0,
        10..=34 => ((rng.below(2000) as f32) - 500.0).to_bits() & !(1u32 << 31),
        35..=64 => ((127 - 20 + rng.below(40)) << 23) | mant,
        65..=70 => ((127 + 100 + rng.below(28)) << 23) | mant,
        71..=76 => (rng.below(4)) << 23 | mant,
        77..=80 => ((127 - 110 + rng.below(20)) << 23) | mant,
        81..=84 => 0xff << 23,
        85..=89 => (0xff << 23) | mant | 1,
        90..=94 => (rng.below(0xff) << 23) | mant,
        _ => ((rng.below(1 << 14) as f32) * 0.25).to_bits(),
    };
    v | sign
}

fn fp_lane(rng: &mut Rng) -> u64 {
    if rng.chance(50) {
        fp_f64(rng)
    } else {
        (fp_f32(rng) as u64) | ((fp_f32(rng) as u64) << 32)
    }
}

fn build_mem(seed: u64, code: &[u8]) -> Memory {
    let mut m = Memory::new(GUEST_SIZE);
    let mut rng = Rng::new(seed ^ 0x5eed);
    m.set_prot(CODE, 0x10000, P_MAPPED | P_R | P_X);
    m.set_prot(DATA, 0x10000, P_MAPPED | P_R | P_W);
    m.set_prot(PG_RONLY, 0x1000, P_MAPPED | P_R);
    m.set_prot(PG_WONLY, 0x1000, P_MAPPED | P_W);
    m.set_prot(PG_NONE, 0x1000, P_MAPPED);
    m.set_prot(STACK, 0x10000, P_MAPPED | P_R | P_W);
    m.set_prot(0xff000, 0x1000, P_MAPPED | P_R | P_W); // top page: accesses can run off the end of RAM
    // random contents of data + stack + top page
    let fp = FP_MODE.with(|f| f.get());
    for (base, len) in [(DATA, 0x10000usize), (STACK, 0x10000), (0xff000, 0x1000)] {
        let buf: Vec<u8> = if fp {
            (0..len / 8).flat_map(|_| fp_lane(&mut rng).to_le_bytes()).collect()
        } else {
            (0..len).map(|_| if rng.chance(30) { 0 } else { rng.next() as u8 }).collect()
        };
        m.write_bytes(base, &buf).unwrap();
    }
    m.write_bytes(CODE, code).unwrap();
    // a page that stores must treat as "contains decoded code" (slow path + dirty reporting)
    m.mark_code(PG_CODEFLAG, 1);
    m
}

// ---- deterministic rng -----------------------------------------------------------------------
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e3779b97f4a7c15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545f4914f6cdd1d)
    }
    pub fn below(&mut self, n: u32) -> u32 {
        (self.next() >> 33) as u32 % n
    }
    pub fn chance(&mut self, pct: u32) -> bool {
        self.below(100) < pct
    }
    pub fn pick<T: Copy>(&mut self, s: &[T]) -> T {
        s[self.below(s.len() as u32) as usize]
    }
}

const INTERESTING: [u32; 24] = [
    0, 1, 2, 3, 0x7f, 0x80, 0x81, 0xff, 0x100, 0x7fff, 0x8000, 0x8001, 0xffff, 0x10000, 0x7fff_ffff, 0x8000_0000, 0x8000_0001, 0xffff_ffff, 0xffff_fffe, 0xffff_ff80, 0xffff_8000, 31, 32, 33,
];

fn interesting(rng: &mut Rng) -> u32 {
    if rng.chance(70) {
        rng.pick(&INTERESTING)
    } else {
        rng.next() as u32
    }
}

fn ptr_val(rng: &mut Rng, base: u32, pages: u32) -> u32 {
    let pg = rng.below(pages) * 0x1000;
    let off = match rng.below(10) {
        0..=2 => 0xffc + rng.below(4),  // right below a page boundary: crossing accesses
        3 => rng.below(4),
        _ => rng.below(0x1000),
    };
    base + pg + off
}

fn initial_cpu(rng: &mut Rng) -> Cpu {
    let mut c = Cpu::new();
    // value registers
    for r in [EAX, ECX, EDX] {
        c.r[r] = if rng.chance(40) { rng.below(70) } else { interesting(rng) };
    }
    if rng.chance(30) {
        let v = c.r[EAX];
        c.r[ECX] = v;
        if rng.chance(50) {
            c.r[EDX] = v;
        }
    }
    // pointer registers into the data window (offset kept away from the page-type pages most of the time)
    for r in [EBX, ESI, EDI, EBP] {
        c.r[r] = if rng.chance(90) { ptr_val(rng, DATA, 16) } else { interesting(rng) };
        if rng.chance(4) {
            c.r[r] = 0x10_0000 - rng.below(16); // the last bytes of RAM: accesses may run off its end
        }
    }
    c.r[ESP] = STACK + 0x4000 + rng.below(0x400) * 4 + if rng.chance(15) { rng.below(4) } else { 0 };
    for x in c.xmm.iter_mut() {
        let lane = |rng: &mut Rng| -> u64 {
            match rng.below(6) {
                0 => 0,
                1 => u64::MAX,
                2 => 0x8000_0000_8000_0000,
                3 => 0x7fff_7fff_7fff_7fff,
                4 => (rng.next() & 0xff01_ff01_ff01_ff01) | 0x0001_0001,
                _ => rng.next(),
            }
        };
        let (lo, hi) = (lane(rng), lane(rng));
        *x = (lo as u128) | ((hi as u128) << 64);
    }
    if FP_MODE.with(|f| f.get()) {
        for x in c.xmm.iter_mut() {
            *x = (fp_lane(rng) as u128) | ((fp_lane(rng) as u128) << 64);
        }
        // MXCSR: mostly the configuration of the fast path (round to nearest, sticky inexact), otherwise any mode
        c.mxcsr = match rng.below(12) {
            0 => 0x1f80,
            1 => 0x1fa0 | 0x2000, // round down
            2 => 0x1fa0 | 0x6000, // toward zero
            3 => 0x1fa0 | 0x8000, // flush to zero
            4 => 0x1fa0 | 0x40,   // denormals are zero
            5 => 0x1f80 | (rng.next() as u32 & 0x3f),
            _ => 0x1fa0,
        };
    }
    if X87_MODE.with(|f| f.get()) {
        let mut f = crate::fpu::Fpu::new();
        for i in 0..8 {
            c.fpu.st[i] = if rng.chance(10) {
                // encodings the register moves must carry unchanged and tag correctly: unnormal, denormal, pseudo-denormal, infinity, NaN, pseudo-NaN
                let mant = rng.pick(&[0x4000_0000_0000_0000u64, 0x0000_0000_0000_1234, 0x8000_0000_0000_0000, 0xc000_0000_0000_0000, 0x7fff_ffff_ffff_ffff, 0xffff_ffff_ffff_ffff, 0x8000_0000_0000_0001]);
                let se = rng.pick(&[0x0000u16, 0x8000, 0x3fff, 0xc000, 0x7fff, 0xffff, 0x0001, 0x4005]);
                crate::fpu::F80 { mant, se }
            } else if rng.chance(12) {
                // a normal number with a random 64-bit significand (full-precision rounding) and, now and then, an extreme exponent
                let e = if rng.chance(30) { rng.pick(&[1u16, 2, 3, 0x7ffe, 0x7ffd, 0x7ffc, 0x4000, 0x3fff, 0x3ffe, 0x3fc0, 0x403f]) } else { 0x3f00 + rng.below(0x200) as u16 };
                crate::fpu::F80 { mant: rng.next() | (1 << 63), se: e | if rng.chance(50) { 0x8000 } else { 0 } }
            } else if rng.chance(15) {
                crate::fpu::F80 { mant: 0, se: if rng.chance(50) { 0x8000 } else { 0 } }
            } else {
                f.f64_to_f80(fp_f64(rng))
            };
        }
        c.fpu.top = rng.below(8) as u8;
        c.fpu.tag = match rng.below(4) {
            0 => 0x0000,
            1 => 0xffff,
            2 => rng.next() as u16 & 0x5555 | 0xff00 >> rng.below(8) & 0xaaaa, // arbitrary mix of valid / empty / special
            _ => 0x00ff,
        };
        c.fpu.cw = rng.pick(&[0x037fu16, 0x037f, 0x077f, 0x0b7f, 0x0f7f, 0x027f, 0x017f, 0x0f3f, 0x037e, 0x007f, 0x0c7f, 0x087f, 0x0a7f, 0x067f, 0x0e7f, 0x047f]);
        c.fpu.sw = if rng.chance(30) { rng.next() as u16 & 0x47ff } else { 0 };
    }
    if MMX_MODE.with(|f| f.get()) {
        for i in 0..8 {
            c.fpu.st[i] = crate::fpu::F80 { mant: if rng.chance(15) { 0 } else { rng.next() }, se: if rng.chance(50) { 0xffff } else { rng.next() as u16 } };
        }
        c.fpu.top = if rng.chance(75) { 0 } else { rng.below(8) as u8 };
        c.fpu.tag = match rng.below(10) {
            0..=3 => 0x0000,
            4 | 5 => 0xffff,
            6 => 0xaaaa, // all special
            _ => rng.next() as u16,
        };
    }
    if rng.chance(50) {
        // 16-byte aligned pointer registers so that aligned SSE accesses succeed
        for r in [EBX, ESI, EDI, EBP] {
            c.r[r] &= !15;
        }
    }
    c.fs_base = DATA + 0x1000;
    c.gs_base = DATA + 0x7000;
    c.fl = 0x202 | (rng.next() as u32 & ARITH) | if rng.chance(8) { DF } else { 0 };
    c.eip = CODE;
    c
}

// ---- instruction generator -------------------------------------------------------------------
const PTR_REGS: [u8; 5] = [3, 5, 6, 7, 4];
const VAL_REGS: [u8; 3] = [0, 1, 2];

enum Item {
    Plain(Vec<u8>),
    /// rel32 branch: opcode bytes, then 4 bytes of displacement to a later item
    Branch(Vec<u8>),
    /// rel8 branch (loop/jecxz): only emitted when the target is close enough
    Branch8(u8),
}

fn modrm_mem(rng: &mut Rng, reg: u8, out: &mut Vec<u8>) {
    let md = rng.pick(&[0u8, 0, 1, 2]);
    let base_reg = |rng: &mut Rng| if rng.chance(85) { rng.pick(&PTR_REGS) } else { rng.below(8) as u8 };
    let idx_reg = |rng: &mut Rng| if rng.chance(85) { rng.pick(&VAL_REGS) } else { rng.below(8) as u8 };
    let disp8 = |rng: &mut Rng| -> u8 {
        if rng.chance(60) {
            rng.below(16) as u8
        } else {
            rng.next() as u8
        }
    };
    let disp32 = |rng: &mut Rng| -> u32 {
        match rng.below(4) {
            0 => rng.below(0x40),
            1 => (rng.below(0x80) as i32 - 0x40) as u32,
            2 => rng.below(0x2000),
            _ => rng.next() as u32,
        }
    };
    match rng.below(10) {
        0 => {
            // [disp32] absolute (inside the data window mostly)
            out.push((reg << 3) | 5);
            let a = if rng.chance(85) { DATA + rng.below(0x10000) } else { rng.next() as u32 };
            out.extend_from_slice(&a.to_le_bytes());
        }
        1..=4 => {
            // SIB
            let scale = rng.below(4) as u8;
            let index = if rng.chance(15) { 4 } else { idx_reg(rng) };
            let base = base_reg(rng);
            if md == 0 && base == 5 {
                out.push((reg << 3) | 4);
                out.push((scale << 6) | (index << 3) | 5);
                out.extend_from_slice(&(if rng.chance(70) { DATA + rng.below(0x8000) } else { disp32(rng) }).to_le_bytes());
            } else {
                out.push((md << 6) | (reg << 3) | 4);
                out.push((scale << 6) | (index << 3) | base);
                match md {
                    1 => out.push(disp8(rng)),
                    2 => out.extend_from_slice(&disp32(rng).to_le_bytes()),
                    _ => {}
                }
            }
        }
        _ => {
            let mut rm = base_reg(rng);
            if rm == 4 {
                rm = rng.pick(&[3u8, 6, 7]);
            }
            if md == 0 && rm == 5 {
                rm = 3;
            }
            out.push((md << 6) | (reg << 3) | rm);
            match md {
                1 => out.push(disp8(rng)),
                2 => out.extend_from_slice(&disp32(rng).to_le_bytes()),
                _ => {}
            }
        }
    }
}

fn modrm_rr(rng: &mut Rng, mem_pct: u32, out: &mut Vec<u8>) {
    let r = rng.below(8) as u8;
    modrm(rng, r, mem_pct, out)
}
/// reg field = lo + random(0..n)
fn modrm_g(rng: &mut Rng, lo: u8, n: u32, mem_pct: u32, out: &mut Vec<u8>) {
    let r = lo + rng.below(n) as u8;
    modrm(rng, r, mem_pct, out)
}

/// Memory-form ModRM for SSE: often `[ptr + disp8*16]` so aligned accesses can succeed.
fn sse_modrm(rng: &mut Rng, mem_pct: u32, out: &mut Vec<u8>) {
    if rng.chance(mem_pct) && rng.chance(65) {
        let reg = rng.below(8) as u8;
        let base = rng.pick(&[3u8, 6, 7, 5]);
        let disp = (rng.below(16) as u8) * 16;
        out.push(0x40 | (reg << 3) | base);
        out.push(disp);
    } else {
        modrm_rr(rng, mem_pct, out)
    }
}

/// ModRM with a register or memory r/m operand (memory with probability `mem_pct`).
fn modrm(rng: &mut Rng, reg: u8, mem_pct: u32, out: &mut Vec<u8>) {
    if rng.chance(mem_pct) {
        // optional %fs/%gs override
        if rng.chance(8) {
            let p = rng.pick(&[0x64u8, 0x65]);
            // prefix must precede the opcode: caller inserts it via `prefix_pos`
            out.insert(0, p);
        }
        modrm_mem(rng, reg, out);
    } else {
        out.push(0xc0 | (reg << 3) | rng.below(8) as u8);
    }
}

fn imm(rng: &mut Rng, size: u8, out: &mut Vec<u8>) {
    let v = interesting(rng);
    out.extend_from_slice(&v.to_le_bytes()[..size as usize]);
}

fn opsize_prefix(rng: &mut Rng, out: &mut Vec<u8>) -> u8 {
    if rng.chance(25) {
        out.push(0x66);
        2
    } else {
        4
    }
}

/// Generate one instruction of family `fam` (families are listed in FAMILIES).
fn gen_item(rng: &mut Rng, fam: &str) -> Item {
    let mut o: Vec<u8> = Vec::new();
    let mem_pct = 55;
    match fam {
        "alu" => {
            let op = rng.below(8) as u8;
            let form = rng.below(6) as u8;
            let wide = form & 1 == 1;
            let sz = if wide { opsize_prefix(rng, &mut o) } else { 1 };
            match form {
                0..=3 => {
                    o.push((op << 3) | form);
                    modrm_rr(rng, mem_pct, &mut o);
                }
                _ => {
                    o.push((op << 3) | form);
                    imm(rng, sz, &mut o);
                }
            }
        }
        "grp1" => {
            let form = rng.pick(&[0x80u8, 0x81, 0x83]);
            let sz = if form == 0x80 { 1 } else { opsize_prefix(rng, &mut o) };
            o.push(form);
            modrm_rr(rng, mem_pct, &mut o);
            imm(rng, if form == 0x81 { sz } else { 1 }, &mut o);
        }
        "test" => {
            match rng.below(4) {
                0 | 1 => {
                    let w = rng.chance(50);
                    let _ = if w { opsize_prefix(rng, &mut o) } else { 1 };
                    o.push(0x84 | w as u8);
                    modrm_rr(rng, mem_pct, &mut o);
                }
                2 => {
                    let w = rng.chance(50);
                    let sz = if w { opsize_prefix(rng, &mut o) } else { 1 };
                    o.push(0xa8 | w as u8);
                    imm(rng, sz, &mut o);
                }
                _ => {
                    let w = rng.chance(50);
                    let sz = if w { opsize_prefix(rng, &mut o) } else { 1 };
                    o.push(0xf6 | w as u8);
                    modrm(rng, 0, mem_pct, &mut o);
                    imm(rng, sz, &mut o);
                }
            }
        }
        "mov" => match rng.below(9) {
            0..=3 => {
                let op = 0x88 + rng.below(4) as u8;
                if op & 1 == 1 {
                    let _ = opsize_prefix(rng, &mut o);
                }
                o.push(op);
                modrm_rr(rng, mem_pct, &mut o);
            }
            4 => {
                let sz = opsize_prefix(rng, &mut o);
                o.push(0xb8 + rng.below(8) as u8);
                imm(rng, sz, &mut o);
            }
            5 => {
                o.push(0xb0 + rng.below(8) as u8);
                imm(rng, 1, &mut o);
            }
            6 => {
                let w = rng.chance(60);
                let sz = if w { opsize_prefix(rng, &mut o) } else { 1 };
                o.push(0xc6 | w as u8);
                modrm(rng, 0, mem_pct, &mut o);
                imm(rng, sz, &mut o);
            }
            7 => {
                // moffs forms a0-a3
                let w = rng.chance(60);
                if w {
                    let _ = opsize_prefix(rng, &mut o);
                }
                o.push(0xa0 | (rng.below(2) as u8) << 1 | w as u8);
                o.extend_from_slice(&(DATA + rng.below(0xfff0)).to_le_bytes());
            }
            _ => {
                let _ = opsize_prefix(rng, &mut o);
                o.extend_from_slice(&[0x0f, rng.pick(&[0xb6u8, 0xb7, 0xbe, 0xbf])]);
                modrm_rr(rng, mem_pct, &mut o);
            }
        },
        "lea" => {
            let _ = opsize_prefix(rng, &mut o);
            o.push(0x8d);
            modrm_rr(rng, 100, &mut o);
            // lea ignores segment overrides, keep them: exercises the "no segment" path
        }
        "xchg" => {
            if rng.chance(30) {
                o.push(0x90 + rng.below(8) as u8);
            } else {
                let w = rng.chance(75);
                let _ = if w { opsize_prefix(rng, &mut o) } else { 1 };
                o.push(0x86 | w as u8);
                modrm_rr(rng, mem_pct, &mut o);
            }
        }
        "incdec" => match rng.below(3) {
            0 => {
                let _ = opsize_prefix(rng, &mut o);
                o.push(0x40 + rng.below(16) as u8);
            }
            _ => {
                let w = rng.chance(75);
                let _ = if w { opsize_prefix(rng, &mut o) } else { 1 };
                o.push(0xfe | w as u8);
                modrm_g(rng, 0, 2, mem_pct, &mut o);
            }
        },
        "negnot" => {
            let w = rng.chance(75);
            let _ = if w { opsize_prefix(rng, &mut o) } else { 1 };
            o.push(0xf6 | w as u8);
            modrm_g(rng, 2, 2, mem_pct, &mut o);
        }
        "muldiv" => match rng.below(4) {
            0 | 1 => {
                let w = rng.chance(75);
                let _ = if w { opsize_prefix(rng, &mut o) } else { 1 };
                o.push(0xf6 | w as u8);
                modrm_g(rng, 4, 4, mem_pct, &mut o);
            }
            2 => {
                let _ = opsize_prefix(rng, &mut o);
                o.extend_from_slice(&[0x0f, 0xaf]);
                modrm_rr(rng, mem_pct, &mut o);
            }
            _ => {
                let sz = opsize_prefix(rng, &mut o);
                if rng.chance(50) {
                    o.push(0x69);
                    modrm_rr(rng, mem_pct, &mut o);
                    imm(rng, sz, &mut o);
                } else {
                    o.push(0x6b);
                    modrm_rr(rng, mem_pct, &mut o);
                    imm(rng, 1, &mut o);
                }
            }
        },
        "shift" => {
            let form = rng.pick(&[0xc0u8, 0xc1, 0xd0, 0xd1, 0xd2, 0xd3]);
            let _ = if form & 1 == 1 { opsize_prefix(rng, &mut o) } else { 1 };
            o.push(form);
            modrm_rr(rng, mem_pct, &mut o);
            if form < 0xd0 {
                let c = if rng.chance(70) { rng.below(34) as u8 } else { rng.next() as u8 };
                o.push(c);
            }
        }
        "dshift" => {
            let _ = opsize_prefix(rng, &mut o);
            let op = rng.pick(&[0xa4u8, 0xa5, 0xac, 0xad]);
            o.extend_from_slice(&[0x0f, op]);
            modrm_rr(rng, mem_pct, &mut o);
            if op & 1 == 0 {
                o.push(rng.below(34) as u8);
            }
        }
        "pushpop" => match rng.below(8) {
            0 => {
                o.push(0x50 + rng.below(8) as u8);
            }
            1 => {
                o.push(0x58 + rng.below(8) as u8);
            }
            2 => {
                o.push(0x68);
                imm(rng, 4, &mut o);
            }
            3 => {
                o.push(0x6a);
                imm(rng, 1, &mut o);
            }
            4 => {
                o.push(0xff);
                modrm(rng, 6, 80, &mut o);
            }
            5 => {
                o.push(0x8f);
                modrm(rng, 0, 80, &mut o);
            }
            6 => {
                if rng.chance(50) {
                    o.push(0x66);
                }
                o.push(0x50 + rng.below(8) as u8 + 8 * rng.below(2) as u8);
            }
            _ => o.push(rng.pick(&[0xc9u8, 0x60, 0x61, 0x9c, 0x9d])),
        },
        "setcmov" => {
            if rng.chance(50) {
                o.extend_from_slice(&[0x0f, 0x90 + rng.below(16) as u8]);
                modrm_rr(rng, mem_pct, &mut o);
            } else {
                let _ = opsize_prefix(rng, &mut o);
                o.extend_from_slice(&[0x0f, 0x40 + rng.below(16) as u8]);
                modrm_rr(rng, mem_pct, &mut o);
            }
        }
        "bit" => match rng.below(4) {
            0 => {
                let _ = opsize_prefix(rng, &mut o);
                o.extend_from_slice(&[0x0f, rng.pick(&[0xa3u8, 0xab, 0xb3, 0xbb])]);
                modrm_rr(rng, mem_pct, &mut o);
            }
            1 => {
                let _ = opsize_prefix(rng, &mut o);
                o.extend_from_slice(&[0x0f, 0xba]);
                modrm_g(rng, 4, 4, mem_pct, &mut o);
                o.push(rng.next() as u8);
            }
            2 => {
                let _ = opsize_prefix(rng, &mut o);
                o.extend_from_slice(&[0x0f, 0xbc + rng.below(2) as u8]);
                modrm_rr(rng, mem_pct, &mut o);
            }
            _ => o.extend_from_slice(&[0x0f, 0xc8 + rng.below(8) as u8]),
        },
        "atomic" => {
            let w = rng.chance(75);
            let _ = if w { opsize_prefix(rng, &mut o) } else { 1 };
            if rng.chance(30) {
                o.insert(0, 0xf0);
            }
            o.extend_from_slice(&[0x0f, rng.pick(&[0xb0u8, 0xc0]) | w as u8]);
            modrm_rr(rng, mem_pct, &mut o);
        }
        "misc" => match rng.below(14) {
            0 => o.push(0x98),
            1 => o.push(0x99),
            2 => {
                o.push(0x66);
                o.push(0x98 + rng.below(2) as u8);
            }
            3 => o.push(0x9e),
            4 => o.push(0x9f),
            5 => o.push(rng.pick(&[0xf8u8, 0xf9, 0xf5, 0xfc])),
            6 => o.push(0xfd),
            7 => o.push(0xd7),
            8 => o.extend_from_slice(&[0x0f, 0xa2]),
            9 => o.extend_from_slice(&[0x0f, 0x31]),
            10 => o.push(0x90),
            11 => o.extend_from_slice(&[0x0f, 0x1f, 0x00]),
            12 => o.extend_from_slice(&[0xf3, 0x90]),
            _ => o.push(0xfc),
        },
        "string" => {
            // [lea edi, [esi + d]] ; mov ecx, n ; [rep] [66] op   (one plain item holding the instructions)
            if rng.chance(40) {
                o.extend_from_slice(&[0x8d, 0x7e, (rng.below(32) as i32 - 8) as u8]); // overlapping source / destination
            }
            o.push(0xb9);
            let n = if rng.chance(5) { 1000 + rng.below(100) } else { rng.below(40) };
            o.extend_from_slice(&n.to_le_bytes());
            if rng.chance(70) {
                o.push(rng.pick(&[0xf3u8, 0xf2]));
            }
            let op = rng.pick(&[0xa4u8, 0xa5, 0xa6, 0xa7, 0xaa, 0xab, 0xac, 0xad, 0xae, 0xaf]);
            if op & 1 == 1 && rng.chance(25) {
                o.push(0x66);
            }
            o.push(op);
        }
        "pair" => {
            // flag producer immediately followed by a flag consumer (register/immediate operands so that
            // equal / boundary operands are common)
            let sz = rng.pick(&[1u8, 2, 4, 4, 4]);
            if sz == 2 {
                o.push(0x66);
            }
            let w = (sz != 1) as u8;
            let r1 = rng.below(8) as u8;
            let r2 = if rng.chance(25) { r1 } else { rng.below(8) as u8 };
            // half of the time start from a known flag state so partial updaters merge with something real
            if rng.chance(50) {
                if sz == 2 {
                    // the 0x66 prefix above belongs to the producer; emit an unprefixed cmp first
                    o.clear();
                }
                o.push(0x39 + 0x10 * rng.below(2) as u8 * 0); // cmp r/m32, r32
                o.push(0xc0 | ((rng.below(8) as u8) << 3) | rng.below(8) as u8);
                if sz == 2 {
                    o.push(0x66);
                }
            }
            match rng.below(20) {
                0 => {
                    // alu r/m, r (register form): add/or/adc/sbb/and/sub/xor/cmp
                    o.push(((rng.below(8) as u8) << 3) | w);
                    o.push(0xc0 | (r1 << 3) | r2);
                }
                1 => {
                    // group 1 with immediate
                    o.push(0x80 | w);
                    o.push(0xc0 | ((rng.below(8) as u8) << 3) | r2);
                    imm(rng, sz, &mut o);
                }
                2 => {
                    // test
                    o.push(0x84 | w);
                    o.push(0xc0 | (r1 << 3) | r2);
                }
                3 => {
                    o.push(0xfe | w);
                    o.push(0xc0 | ((rng.below(2) as u8) << 3) | r2);
                }
                4 => {
                    o.push(0xf6 | w);
                    o.push(0xc0 | (3 << 3) | r2); // neg
                }
                5 => {
                    o.push(0xc0 | w);
                    o.push(0xc0 | (rng.pick(&[4u8, 5, 7]) << 3) | r2);
                    o.push(rng.below(34) as u8);
                }
                6 => {
                    o.push(0xd0 | w);
                    o.push(0xc0 | (rng.pick(&[4u8, 5, 7]) << 3) | r2);
                }
                7 => {
                    // alu with memory operand (flags from loaded data)
                    o.push(((rng.below(8) as u8) << 3) | 2 | w);
                    modrm(rng, r1, 100, &mut o);
                }
                8 => {
                    o.push(0x3c | w); // cmp al/eax, imm
                    imm(rng, sz, &mut o);
                }
                9 => {
                    // imul r, r/m
                    o.extend_from_slice(&[0x0f, 0xaf, 0xc0 | (r1 << 3) | r2]);
                }
                10 => {
                    // rol/ror by imm / 1 / cl
                    let form = rng.pick(&[0xc0u8, 0xd0, 0xd2]) | w;
                    o.push(form);
                    o.push(0xc0 | (rng.below(2) << 3) as u8 | r2);
                    if form & 0xfe == 0xc0 {
                        o.push(rng.below(34) as u8);
                    }
                }
                11 => {
                    // bt / bts / btr / btc r, r  (16/32 bit only)
                    if sz == 1 {
                        o.push(0x90);
                    } else {
                        o.extend_from_slice(&[0x0f, rng.pick(&[0xa3u8, 0xab, 0xb3, 0xbb]), 0xc0 | (r1 << 3) | r2]);
                    }
                }
                12 => {
                    if sz == 1 {
                        o.push(0x90);
                    } else {
                        o.extend_from_slice(&[0x0f, 0xbc + rng.below(2) as u8, 0xc0 | (r1 << 3) | r2]);
                    }
                }
                13 => o.push(rng.pick(&[0xf8u8, 0xf9, 0xf5])),
                14 => o.push(0x9e),
                15 => {
                    // mul / imul (one operand), 8/16/32
                    o.push(0xf6 | w);
                    o.push(0xc0 | (rng.pick(&[4u8, 5]) << 3) | r2);
                }
                16 => {
                    // div / idiv
                    o.push(0xf6 | w);
                    o.push(0xc0 | (rng.pick(&[6u8, 7]) << 3) | r2);
                }
                17 => {
                    // shld/shrd imm
                    if sz == 1 {
                        o.push(0x90);
                    } else {
                        o.extend_from_slice(&[0x0f, rng.pick(&[0xa4u8, 0xac]), 0xc0 | (r1 << 3) | r2, rng.below(34) as u8]);
                    }
                }
                18 => {
                    o.extend_from_slice(&[0x0f, 0xc0 | w, 0xc0 | (r1 << 3) | r2]); // xadd
                }
                _ => {
                    o.extend_from_slice(&[0x0f, 0xb0 | w, 0xc0 | (r1 << 3) | r2]); // cmpxchg
                }
            }
            // consumer
            match rng.below(5) {
                0 | 1 => {
                    o.extend_from_slice(&[0x0f, 0x90 + rng.below(16) as u8, 0xc0 | rng.below(8) as u8]); // setcc r8
                }
                2 => {
                    o.extend_from_slice(&[0x0f, 0x40 + rng.below(16) as u8, 0xc0 | ((rng.below(8) as u8) << 3) | rng.below(8) as u8]); // cmovcc
                }
                3 => {
                    o.extend_from_slice(&[0x12 | (rng.below(2) as u8) << 3, 0xc0 | ((rng.below(8) as u8) << 3) | rng.below(8) as u8]); // adc/sbb r8,r8
                }
                _ => {
                    o.extend_from_slice(&[0x9f]); // lahf
                }
            }
        }
        "sse" => {
            // SSE / SSE2 / MMX instructions run through the interpreter helper: this checks that the JIT stores
            // and reloads exactly the registers they use (address registers, GPR operands, implicit EAX...).
            match rng.below(16) {
                0 => {
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x10u8, 0x11, 0x28, 0x29])]);
                    modrm_rr(rng, 100, &mut o);
                }
                1 => {
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x58u8, 0x59, 0x5c, 0x5e, 0x51, 0x54, 0x57])]);
                    modrm_rr(rng, 60, &mut o);
                }
                2 => {
                    o.extend_from_slice(&[rng.pick(&[0xf3u8, 0xf2]), 0x0f, rng.pick(&[0x58u8, 0x59, 0x5c, 0x10, 0x11])]);
                    modrm_rr(rng, 60, &mut o);
                }
                3 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0x6e]); // movd xmm, r/m32
                    modrm_rr(rng, 40, &mut o);
                }
                4 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0x7e]); // movd r/m32, xmm
                    modrm_rr(rng, 40, &mut o);
                }
                5 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0xd7, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8]); // pmovmskb r32, xmm
                }
                6 => {
                    o.extend_from_slice(&[0xf3, 0x0f, rng.pick(&[0x2cu8, 0x2d])]); // cvttss2si / cvtss2si
                    modrm_rr(rng, 50, &mut o);
                }
                7 => {
                    o.extend_from_slice(&[rng.pick(&[0xf3u8, 0xf2]), 0x0f, 0x2a]); // cvtsi2ss/sd
                    modrm_rr(rng, 50, &mut o);
                }
                8 => {
                    if rng.chance(50) {
                        o.push(0x66);
                    }
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x2eu8, 0x2f])]); // (u)comiss/sd: writes EFLAGS
                    modrm_rr(rng, 40, &mut o);
                }
                9 => {
                    o.extend_from_slice(&[0x66, 0x0f, rng.pick(&[0xfdu8, 0xef, 0xdb, 0xeb, 0xf8, 0xd4, 0x74, 0x64, 0xe5, 0xd5, 0x60, 0x61])]);
                    modrm_rr(rng, 50, &mut o);
                }
                10 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0x70]); // pshufd
                    modrm_rr(rng, 50, &mut o);
                    o.push(rng.next() as u8);
                }
                11 => {
                    o.extend_from_slice(&[rng.pick(&[0x66u8, 0xf3]), 0x0f, rng.pick(&[0x6fu8, 0x7f])]); // movdqa / movdqu
                    modrm_rr(rng, 100, &mut o);
                }
                12 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0xc5, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8, rng.below(8) as u8]); // pextrw r32, xmm, imm
                }
                13 => {
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x6fu8, 0x7f, 0xfc, 0xfd, 0xef, 0xdb])]); // MMX
                    modrm_rr(rng, 50, &mut o);
                }
                14 => {
                    o.extend_from_slice(&[0x0f, 0x6e]); // movd mm, r/m32
                    modrm_rr(rng, 40, &mut o);
                }
                _ => {
                    o.extend_from_slice(&[0x0f, 0x77]); // emms
                }
            }
        }
        "mmx" => {
            // the MMX instruction set translated inline (jit_mmx.rs)
            let mem = 45;
            match rng.below(46) {
                40 | 41 => {
                    // cvtps2pi / cvttps2pi mm, xmm/m64
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x2cu8, 0x2d])]);
                    sse_modrm(rng, 40, &mut o);
                }
                42 => {
                    o.extend_from_slice(&[0xf2, 0x0f, 0xd6, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8]); // movdq2q mm, xmm
                }
                43 => {
                    o.extend_from_slice(&[0xf3, 0x0f, 0xd6, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8]); // movq2dq xmm, mm
                }
                44 | 45 => {
                    // packed single float next to the MMX code (the colour-transform loops mix both)
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x58u8, 0x59, 0x5b, 0xc6, 0x5d, 0x5f])]);
                    sse_modrm(rng, 40, &mut o);
                    if o[o.len() - 1] == 0xc6 || o[1] == 0xc6 {
                        o.push(rng.next() as u8);
                    }
                }
                0..=19 => {
                    let ops = [
                        0x60u8, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x6b, 0x74, 0x75, 0x76, 0xd4, 0xd5, 0xd8, 0xd9, 0xda, 0xdb, 0xdc, 0xdd, 0xde, 0xdf, 0xe0, 0xe3, 0xe4, 0xe5, 0xe8, 0xe9,
                        0xea, 0xeb, 0xec, 0xed, 0xee, 0xef, 0xf4, 0xf5, 0xf6, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe,
                    ];
                    o.extend_from_slice(&[0x0f, rng.pick(&ops)]);
                    modrm_rr(rng, mem, &mut o);
                }
                20..=23 => {
                    // shifts by mm / m64
                    o.extend_from_slice(&[0x0f, rng.pick(&[0xd1u8, 0xd2, 0xd3, 0xe1, 0xe2, 0xf1, 0xf2, 0xf3])]);
                    modrm_rr(rng, 30, &mut o);
                }
                24..=26 => {
                    // shifts by immediate: 71 (w), 72 (d), 73 (q)
                    let (opc, exts): (u8, &[u8]) = rng.pick(&[(0x71u8, &[2u8, 4, 6][..]), (0x72, &[2, 4, 6][..]), (0x73, &[2, 6][..])]);
                    o.extend_from_slice(&[0x0f, opc, 0xc0 | (rng.pick(exts) << 3) | rng.below(8) as u8]);
                    o.push(if rng.chance(70) { rng.below(70) as u8 } else { rng.next() as u8 });
                }
                27 | 28 => {
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x6fu8, 0x7f, 0xe7])]); // movq mm, mm/m64 / movq mm/m64, mm / movntq
                    if o[1] == 0xe7 {
                        let r = rng.below(8) as u8;
                        modrm(rng, r, 100, &mut o);
                    } else {
                        modrm_rr(rng, 55, &mut o);
                    }
                }
                29 | 30 => {
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x6eu8, 0x7e])]); // movd
                    modrm_rr(rng, 45, &mut o);
                }
                31 => {
                    o.extend_from_slice(&[0x0f, 0x70]); // pshufw
                    modrm_rr(rng, 45, &mut o);
                    o.push(rng.next() as u8);
                }
                32 => {
                    o.extend_from_slice(&[0x0f, 0xc5, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8, rng.next() as u8]); // pextrw
                }
                33 => {
                    o.extend_from_slice(&[0x0f, 0xc4]); // pinsrw
                    modrm_rr(rng, 40, &mut o);
                    o.push(rng.next() as u8);
                }
                34 => {
                    o.extend_from_slice(&[0x0f, 0xd7, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8]); // pmovmskb
                }
                35 => {
                    o.extend_from_slice(&[0x0f, 0x77]); // emms
                }
                _ => {
                    // an x87 instruction in between: fld1 / fldz / fxch / fnop / ffree
                    o.extend_from_slice(&rng.pick(&[&[0xd9u8, 0xe8][..], &[0xd9, 0xee][..], &[0xd9, 0xc9][..], &[0xd9, 0xd0][..], &[0xdd, 0xc0][..]]));
                }
            }
        }
        "x87m" => {
            // x87 data movement translated inline (jit_x87.rs)
            match rng.below(31) {
                0..=2 => {
                    o.push(rng.pick(&[0xd9u8, 0xdd])); // fld m32 / m64
                    modrm_g(rng, 0, 1, 100, &mut o);
                }
                3 => o.extend_from_slice(&[0xd9, rng.pick(&[0xe8u8, 0xee])]), // fld1 / fldz
                4..=6 => {
                    o.push(rng.pick(&[0xd9u8, 0xdd])); // fst / fstp m32 / m64
                    modrm_g(rng, 2, 2, 100, &mut o);
                }
                7..=9 => {
                    o.push(rng.pick(&[0xdbu8, 0xdf])); // fist / fistp m32 (db) / m16 (df)
                    modrm_g(rng, 2, 2, 100, &mut o);
                }
                10 => {
                    o.push(0xd9);
                    modrm_g(rng, 5, 1, 100, &mut o); // fldcw
                }
                11 => o.extend_from_slice(&[0xd9, 0xc8 | rng.below(8) as u8]), // fxch
                12 => o.extend_from_slice(&[0xdf, 0xe0]), // fnstsw ax
                13 => o.extend_from_slice(&[0xdd, 0xd8 | rng.below(8) as u8]), // fstp st(i)
                14 => o.extend_from_slice(&[0xd9, 0xc0 | rng.below(8) as u8]), // fld st(i)
                15 => o.extend_from_slice(&[0xdd, 0xd0 | rng.below(8) as u8]), // fst st(i)
                16 => o.extend_from_slice(&[0xd9, rng.pick(&[0xe0u8, 0xe1])]), // fchs / fabs
                17 => {
                    o.push(0xd9);
                    modrm_g(rng, 7, 1, 100, &mut o); // fnstcw
                }
                18 => o.extend_from_slice(&[0xd9, 0xc9]), // fxch st(1): the common form
                19 => o.extend_from_slice(&[0xdb, 0xe3]), // fninit: empty stack
                20 => o.extend_from_slice(&[0xdd, 0xc0 | rng.below(8) as u8]), // ffree st(i)
                21..=23 => {
                    // fadd / fmul / fsub / fsubr with a m32 (d8) or m64 (dc) operand
                    o.push(rng.pick(&[0xd8u8, 0xdc]));
                    let g = rng.pick(&[0u8, 1, 4, 5]);
                    modrm_g(rng, g, 1, 100, &mut o);
                }
                24..=26 => {
                    let (a, b) = (rng.pick(&[0xd8u8, 0xdc]), rng.pick(&[0xc0u8, 0xc8, 0xe0, 0xe8]) | rng.below(8) as u8);
                    o.extend_from_slice(&[a, b]); // st(0),st(i) / st(i),st(0)
                }
                27..=29 => {
                    let b = rng.pick(&[0xc0u8, 0xc8, 0xe0, 0xe8]) | rng.below(8) as u8;
                    o.extend_from_slice(&[0xde, b]); // faddp / fmulp / fsubp / fsubrp
                }
                _ => {
                    let b = rng.pick(&[0xc1u8, 0xc9, 0xe1, 0xe9]);
                    o.extend_from_slice(&[0xde, b]); // the same with st(1), st(0)
                }
            }
        }
        "fp" => {
            // scalar SSE/SSE2 floating point translated inline (jit_fp.rs): arithmetic, compares, min/max, conversions
            let pre = |rng: &mut Rng| rng.pick(&[0xf2u8, 0xf3]);
            match rng.below(20) {
                0..=4 => {
                    o.extend_from_slice(&[pre(rng), 0x0f, rng.pick(&[0x58u8, 0x59, 0x5c, 0x5e, 0x51, 0x5d, 0x5f])]);
                    sse_modrm(rng, 55, &mut o);
                }
                14..=16 => {
                    // packed single arithmetic / min / max (no prefix)
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x58u8, 0x59, 0x5c, 0x5e, 0x51, 0x5d, 0x5f])]);
                    sse_modrm(rng, 50, &mut o);
                }
                17 => {
                    o.extend_from_slice(&[0x0f, 0xc2]); // cmpps
                    sse_modrm(rng, 50, &mut o);
                    o.push(rng.below(8) as u8);
                }
                18 => {
                    // cvtdq2ps / cvttps2dq / cvtps2dq
                    match rng.below(3) {
                        0 => o.extend_from_slice(&[0x0f, 0x5b]),
                        1 => o.extend_from_slice(&[0xf3, 0x0f, 0x5b]),
                        _ => o.extend_from_slice(&[0x66, 0x0f, 0x5b]),
                    }
                    sse_modrm(rng, 50, &mut o);
                }
                19 => {
                    o.extend_from_slice(&[0xf3, 0x0f, rng.pick(&[0x52u8, 0x53])]); // rsqrtss / rcpss
                    sse_modrm(rng, 55, &mut o);
                }
                5 | 6 => {
                    if rng.chance(50) {
                        o.push(0x66);
                    }
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x2eu8, 0x2f])]); // ucomis / comis
                    sse_modrm(rng, 55, &mut o);
                    // consume the flags
                    o.extend_from_slice(&[0x0f, 0x90 + rng.below(16) as u8, 0xc0 | rng.below(4) as u8]); // setcc r8
                }
                7 | 8 => {
                    o.extend_from_slice(&[pre(rng), 0x0f, 0x2a]); // cvtsi2sd / cvtsi2ss
                    modrm_rr(rng, 40, &mut o);
                }
                9 | 10 => {
                    o.extend_from_slice(&[pre(rng), 0x0f, 0x2c]); // cvttsd2si / cvttss2si
                    modrm_rr(rng, 55, &mut o);
                }
                11 => {
                    o.extend_from_slice(&[pre(rng), 0x0f, 0x5a]); // cvtss2sd / cvtsd2ss
                    sse_modrm(rng, 55, &mut o);
                }
                12 => {
                    o.extend_from_slice(&[pre(rng), 0x0f, rng.pick(&[0x10u8, 0x11])]); // movss/movsd
                    sse_modrm(rng, 70, &mut o);
                }
                _ => {
                    o.extend_from_slice(&[0x0f, 0x18, rng.pick(&[0x08u8, 0x10, 0x18, 0x00]) | 0x06]); // prefetch [esi]
                }
            }
        }
        "simd" => {
            // the SSE/SSE2 integer + move subset the JIT translates inline (see jit_simd.rs)
            let op66 = |o: &mut Vec<u8>| o.push(0x66);
            match rng.below(30) {
                0..=7 => {
                    op66(&mut o);
                    let ops = [
                        0xdbu8, 0xdf, 0xeb, 0xef, 0xfc, 0xfd, 0xfe, 0xd4, 0xf8, 0xf9, 0xfa, 0xfb, 0xec, 0xed, 0xdc, 0xdd, 0xe8, 0xe9, 0xd8, 0xd9, 0xd5, 0xe5, 0xe4, 0xf4, 0xf5, 0xe0, 0xe3, 0xda, 0xde, 0xea, 0xee, 0xf6,
                        0x74, 0x75, 0x76, 0x64, 0x65, 0x66, 0x63, 0x67, 0x6b, 0x60, 0x61, 0x62, 0x6c, 0x68, 0x69, 0x6a, 0x6d,
                    ];
                    o.extend_from_slice(&[0x0f, rng.pick(&ops)]);
                    sse_modrm(rng, 55, &mut o);
                }
                8..=9 => {
                    // float-typed logic ops (bitwise) and unpck*ps/pd
                    if rng.chance(30) {
                        op66(&mut o);
                    }
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x54u8, 0x55, 0x56, 0x57, 0x14, 0x15])]);
                    sse_modrm(rng, 55, &mut o);
                }
                10..=12 => {
                    // moves: movups/movaps/movdqa/movdqu loads and stores
                    let pre = rng.pick(&[0u8, 0x66, 0xf3]);
                    if pre != 0 {
                        o.push(pre);
                    }
                    let opc = if pre == 0 { rng.pick(&[0x10u8, 0x11, 0x28, 0x29]) } else if pre == 0x66 { rng.pick(&[0x10u8, 0x11, 0x28, 0x29, 0x6f, 0x7f]) } else { rng.pick(&[0x6fu8, 0x7f]) };
                    o.extend_from_slice(&[0x0f, opc]);
                    sse_modrm(rng, 70, &mut o);
                }
                13 => {
                    o.extend_from_slice(&[0x66, 0x0f, rng.pick(&[0x6eu8, 0x7e])]);
                    sse_modrm(rng, 50, &mut o);
                }
                14 => {
                    // movq: f3 0f 7e / 66 0f d6
                    if rng.chance(50) {
                        o.extend_from_slice(&[0xf3, 0x0f, 0x7e]);
                    } else {
                        o.extend_from_slice(&[0x66, 0x0f, 0xd6]);
                    }
                    sse_modrm(rng, 50, &mut o);
                }
                15 => {
                    // movss / movsd (scalar moves)
                    o.extend_from_slice(&[rng.pick(&[0xf3u8, 0xf2]), 0x0f, rng.pick(&[0x10u8, 0x11])]);
                    sse_modrm(rng, 60, &mut o);
                }
                16 => {
                    // movlps/movhps/movlpd/movhpd and movhlps/movlhps
                    if rng.chance(40) {
                        o.push(0x66);
                    }
                    o.extend_from_slice(&[0x0f, rng.pick(&[0x12u8, 0x13, 0x16, 0x17])]);
                    sse_modrm(rng, 70, &mut o);
                }
                17 => {
                    // pshufd / pshuflw / pshufhw
                    o.extend_from_slice(&[rng.pick(&[0x66u8, 0xf2, 0xf3]), 0x0f, 0x70]);
                    sse_modrm(rng, 50, &mut o);
                    o.push(rng.next() as u8);
                }
                18..=19 => {
                    // shifts by immediate: 66 0f 71/72/73 /r ib
                    let grp = rng.pick(&[0x71u8, 0x72, 0x73]);
                    let r = match grp {
                        0x71 | 0x72 => rng.pick(&[2u8, 4, 6]),
                        _ => rng.pick(&[2u8, 3, 6, 7]),
                    };
                    o.extend_from_slice(&[0x66, 0x0f, grp, 0xc0 | (r << 3) | rng.below(8) as u8]);
                    o.push(if rng.chance(70) { rng.below(70) as u8 } else { rng.next() as u8 });
                }
                20..=21 => {
                    // shifts by xmm / m128 count
                    let ops = [0xd1u8, 0xd2, 0xd3, 0xe1, 0xe2, 0xf1, 0xf2, 0xf3];
                    o.extend_from_slice(&[0x66, 0x0f, rng.pick(&ops)]);
                    sse_modrm(rng, 50, &mut o);
                }
                22 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0xd7, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8]); // pmovmskb
                }
                23 => {
                    if rng.chance(50) {
                        o.push(0x66);
                    }
                    o.extend_from_slice(&[0x0f, 0x50, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8]); // movmskps/pd
                }
                24 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0xc5, 0xc0 | (rng.below(8) as u8) << 3 | rng.below(8) as u8, rng.next() as u8]); // pextrw
                }
                25 => {
                    o.extend_from_slice(&[0x66, 0x0f, 0xc4]); // pinsrw
                    modrm_rr(rng, 40, &mut o);
                    o.push(rng.next() as u8);
                }
                _ => {
                    // an ALU instruction in between so scalar and SIMD state interleave
                    o.push(0x01);
                    modrm_rr(rng, 30, &mut o);
                }
            }
        }
        "x87" => {
            match rng.below(12) {
                0 => {
                    o.push(rng.pick(&[0xd9u8, 0xdd, 0xdb]));
                    modrm_g(rng, 0, 1, 100, &mut o); // fld m32/m64/m32int
                }
                1 => {
                    o.push(rng.pick(&[0xd8u8, 0xdc, 0xda, 0xde]));
                    modrm_rr(rng, 100, &mut o); // arithmetic with memory
                }
                2 => {
                    o.push(rng.pick(&[0xd9u8, 0xdd, 0xdb, 0xdf]));
                    modrm_g(rng, 2, 2, 100, &mut o); // fst/fstp/fist
                }
                3 => o.extend_from_slice(&[0xdf, 0xe0]), // fnstsw ax
                4 => o.extend_from_slice(&[0xd9, rng.pick(&[0xe8u8, 0xee, 0xe0, 0xe1, 0xe4, 0xfa, 0xfc])]),
                5 => o.extend_from_slice(&[0xde, rng.pick(&[0xc1u8, 0xc9, 0xe9, 0xf9, 0xe1])]),
                6 => o.extend_from_slice(&[rng.pick(&[0xdbu8, 0xdf]), 0xe8 | rng.below(8) as u8]), // fucomi / fucomip: write EFLAGS
                7 => {
                    o.push(0xd9);
                    modrm_g(rng, 5, 3, 100, &mut o); // fldcw / fnstcw
                }
                8 => o.extend_from_slice(&[0xd9, 0xc0 | rng.below(8) as u8]), // fld st(i)
                9 => o.extend_from_slice(&[0xdd, 0xd8 | rng.below(8) as u8]), // fstp st(i)
                10 => o.extend_from_slice(&[0xda, 0xc0 | rng.below(32) as u8 & 0x1f]), // fcmovcc: reads EFLAGS
                _ => o.extend_from_slice(&[0xdb, 0xe3]), // fninit
            }
        }
        "callret" => {
            // call L ; jmp END ; L: ret [n] ; END:   (optionally with a pushed argument that `ret n` pops)
            let n = rng.chance(40);
            if n {
                o.push(0x50 + rng.below(8) as u8);
            }
            o.extend_from_slice(&[0xe8, 0x02, 0, 0, 0, 0xeb, if n { 0x03 } else { 0x01 }]);
            if n {
                o.extend_from_slice(&[0xc2, 0x04, 0x00]);
            } else {
                o.push(0xc3);
            }
        }
        "branch" => match rng.below(10) {
            0..=5 => return Item::Branch(vec![0x0f, 0x80 + rng.below(16) as u8]),
            6 => return Item::Branch(vec![0xe9]),
            7 => return Item::Branch(vec![0xe8]),
            8 => return Item::Branch8(0xe0 + rng.below(4) as u8),
            _ => return Item::Branch8(0x70 + rng.below(16) as u8),
        },
        _ => panic!("unknown family {fam}"),
    }
    Item::Plain(o)
}

fn assemble(rng: &mut Rng, fams: &[&str], n: usize) -> Vec<u8> {
    let mut items: Vec<Item> = Vec::new();
    for _ in 0..n {
        let f = rng.pick(fams);
        items.push(gen_item(rng, f));
    }
    // sizes
    let sz = |it: &Item| match it {
        Item::Plain(b) => b.len(),
        Item::Branch(op) => op.len() + 4,
        Item::Branch8(_) => 2,
    };
    let mut starts = Vec::new();
    let mut pos = 0usize;
    for it in &items {
        starts.push(pos);
        pos += sz(it);
    }
    starts.push(pos); // terminator position
    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let end = starts[i] + sz(it);
        match it {
            Item::Plain(b) => out.extend_from_slice(b),
            Item::Branch(op) => {
                let tgt = starts[i + 1 + rng.below((items.len() - i) as u32) as usize];
                out.extend_from_slice(op);
                out.extend_from_slice(&((tgt as i32 - end as i32) as u32).to_le_bytes());
            }
            Item::Branch8(op) => {
                let j = i + 1 + rng.below((items.len() - i) as u32) as usize;
                let d = starts[j] as i32 - end as i32;
                if d <= 127 {
                    out.extend_from_slice(&[*op, d as u8]);
                } else {
                    out.extend_from_slice(&[0x90, 0x90]);
                }
            }
        }
    }
    out.extend_from_slice(&[0xcd, 0x80]); // int 0x80: ends the run
    out
}

// ---- the differential driver -----------------------------------------------------------------
fn describe(code: &[u8]) -> String {
    let mut s = String::new();
    let mut ip = 0u64;
    let mut dec = iced_x86::Decoder::with_ip(32, code, CODE as u64, iced_x86::DecoderOptions::NONE);
    while dec.can_decode() {
        let i = dec.decode();
        let len = i.len();
        s += &format!("  {:06x}: {:02x?}  {:?}\n", i.ip(), &code[ip as usize..ip as usize + len], i.code());
        ip += len as u64;
    }
    s
}

fn diff_cpu(a: &Cpu, b: &Cpu) -> Option<String> {
    let mut d = Vec::new();
    for i in 0..8 {
        if a.r[i] != b.r[i] {
            d.push(format!("r[{}]: interp {:#x} jit {:#x}", i, a.r[i], b.r[i]));
        }
    }
    if a.eip != b.eip {
        d.push(format!("eip: interp {:#x} jit {:#x}", a.eip, b.eip));
    }
    if a.fl != b.fl {
        d.push(format!("fl: interp {:#x} jit {:#x} (xor {:#x})", a.fl, b.fl, a.fl ^ b.fl));
    }
    if a.xmm != b.xmm {
        for i in 0..8 {
            if a.xmm[i] != b.xmm[i] {
                d.push(format!("xmm{}: interp {:032x} jit {:032x}", i, a.xmm[i], b.xmm[i]));
            }
        }
    }
    if a.mxcsr != b.mxcsr {
        d.push(format!("mxcsr: interp {:#x} jit {:#x}", a.mxcsr, b.mxcsr));
    }
    for i in 0..8 {
        if a.fpu.st[i] != b.fpu.st[i] {
            d.push(format!("st[{}]: interp {:x}:{:04x} jit {:x}:{:04x}", i, a.fpu.st[i].mant, a.fpu.st[i].se, b.fpu.st[i].mant, b.fpu.st[i].se));
        }
    }
    if a.fpu.top != b.fpu.top || a.fpu.tag != b.fpu.tag || a.fpu.sw != b.fpu.sw || a.fpu.cw != b.fpu.cw {
        d.push(format!(
            "fpu: interp top={} tag={:#06x} sw={:#06x} cw={:#06x}; jit top={} tag={:#06x} sw={:#06x} cw={:#06x}",
            a.fpu.top, a.fpu.tag, a.fpu.sw, a.fpu.cw, b.fpu.top, b.fpu.tag, b.fpu.sw, b.fpu.cw
        ));
    }
    if a.tsc != b.tsc {
        d.push(format!("tsc: interp {} jit {}", a.tsc, b.tsc));
    }
    if a.fs_base != b.fs_base || a.gs_base != b.gs_base {
        d.push("segment bases differ".into());
    }
    if d.is_empty() {
        None
    } else {
        Some(d.join("; "))
    }
}

// Run `code` on identical machines with the interpreter and with the JIT (threshold 1) and compare everything.
thread_local! { static CUR_CODE: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) }; }
thread_local! { static EXITS: std::cell::RefCell<std::collections::BTreeMap<String, u32>> = const { std::cell::RefCell::new(std::collections::BTreeMap::new()) }; }

#[test]
fn time_helper_sees_ebx_and_preserves_eax_on_code_store() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { TEST_TIME_HELPER.with(|v| v.set(false)); }
    }
    TEST_TIME_HELPER.with(|v| v.set(true));
    let _reset = Reset;
    for addr in [DATA, PG_CODEFLAG] {
        install_backend();
        let mut code = vec![0xb8, 13, 0, 0, 0, 0xbb]; // mov eax,13; mov ebx,addr
        code.extend_from_slice(&addr.to_le_bytes());
        code.extend_from_slice(&[0xcd, 0x80, 0x89, 0xc2, 0xb8, 1, 0, 0, 0, 0xcd, 0x80]);
        let mut mem = build_mem(1, &code);
        let mut cpu = initial_cpu(&mut Rng::new(1));
        let mut bc = BlockCache::new();
        bc.jit.threshold = 1;
        bc.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
        let exit = cpu.run(&mut mem, &mut bc, 100_000);
        assert!(matches!(exit, Exit::Syscall), "{exit:?}");
        assert_eq!(cpu.r[EAX], 1);
        assert_eq!(cpu.r[EDX], 123, "return value overwritten at {addr:x}");
        assert_eq!(mem.r32(addr).unwrap(), 123);
        assert!(bc.jit.stats.calls > 0);
    }
}

#[test]
fn fp_helper_edges_match_interpreter() {
    FP_HELPERS_SEEN.with(|c| c.set([0; 4]));
    for seed in 0..16u64 {
        let mut rng = Rng::new(seed + 0xf100);
        let mut cpu = initial_cpu(&mut rng);
        cpu.fpu.top = (seed & 7) as u8;
        cpu.fpu.tag = if seed & 1 == 0 { 0xffff } else { 0 };
        cpu.fpu.cw = 0x037f | (((seed >> 2) as u16) << 10);
        cpu.fpu.sw = (rng.next() as u16) & 0x7fff;
        cpu.fpu.st[cpu.fpu.top as usize] = crate::fpu::F80::from_i64(i64::MAX);
        cpu.xmm[0] = [0x3fc00000u128, 0x7f800000, 0x7fc00001, 0xcf000001][seed as usize % 4];
        cpu.mxcsr = 0x1f80 | (((seed as u32) & 3) << 13);
        cpu.fs_base = 0x1000;
        cpu.gs_base = 0x2000;
        let mut ops: Vec<Vec<u8>> = vec![
            vec![0xdf, 0xe0], // fnstsw ax
            vec![0xdf, 0xf1], vec![0xdf, 0xe9], // fcomip / fucomip st0,st1
            vec![0xf3, 0x0f, 0x2d, 0xc0], // cvtss2si eax,xmm0
            vec![0x0f, 0x6f, 0xc1], vec![0x0f, 0xef, 0xc1], // movq / pxor mm0,mm1
        ];
        for addr in [DATA, DATA + 0xffd, PG_RONLY, PG_WONLY, PG_NONE, PG_CODEFLAG, 0xffffd] {
            for prefix in [&[0xdf, 0x3d][..], &[0xdf, 0x2d], &[0xf3, 0x0f, 0x2d, 0x05], &[0x0f, 0x6f, 0x05], &[0x0f, 0xef, 0x05]] {
                let mut op = prefix.to_vec();
                op.extend_from_slice(&addr.to_le_bytes());
                for seg in [0x64, 0x65] {
                    let mut segmented = vec![seg];
                    segmented.extend_from_slice(&op);
                    ops.push(segmented);
                }
                ops.push(op);
            }
        }
        for op in ops {
            let mut code = vec![0x83, 0xc3, 0x01]; // pending lazy ADD flags must survive non-flag helpers
            code.extend_from_slice(&op);
            code.extend_from_slice(&[0x0f, 0x92, 0xc2, 0xcd, 0x80]); // setc dl; syscall
            check_case(seed, &code, &cpu).unwrap_or_else(|e| panic!("op={op:x?}: {e}"));
        }
    }
    assert!(FP_HELPERS_SEEN.with(|c| c.get().iter().all(|&n| n > 0)), "helper paths must actually execute");
}

fn check_case(seed: u64, code: &[u8], cpu0: &Cpu) -> Result<(), String> {
    check_case_with(seed, code, cpu0, None)
}

/// `patch`: (guest address, 8 bytes) written into both memories after the seeded contents.
fn check_case_with(seed: u64, code: &[u8], cpu0: &Cpu, patch: Option<(u32, u64)>) -> Result<(), String> {
    check_case_full(seed, code, cpu0, patch, None)
}

thread_local! { static TRUST_SKIPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
thread_local! { static SKIP_NONTERMINATING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; static NONTERMINATING_SKIPPED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
thread_local! { static THUNKS_SEEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
thread_local! { static INTRINSICS_SEEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
thread_local! { static FP_HELPERS_SEEN: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) }; }

/// `init`: arbitrary extra setup applied to both memories (after the seeded contents and `patch`).
fn check_case_full(seed: u64, code: &[u8], cpu0: &Cpu, patch: Option<(u32, u64)>, init: Option<&dyn Fn(&mut Memory)>) -> Result<(), String> {
    // interpreter
    let mut m1 = build_mem(seed, code);
    if let Some((a, v)) = patch {
        m1.write_bytes(a, &v.to_le_bytes()).unwrap();
    }
    if let Some(f) = init {
        f(&mut m1);
    }
    let mut c1 = cpu0.clone();
    let mut bc1 = BlockCache::new();
    let e1 = c1.run(&mut m1, &mut bc1, 100_000);
    c1.lf_flush();
    let trust = std::env::var("JIT_TRUST").is_ok();
    if trust {
        if let Exit::Segv(sv) = &e1 {
            if !sv.write {
                // the loads-unchecked performance mode does not fault on unreadable memory (docs/JIT.md): not comparable
                TRUST_SKIPPED.with(|c| c.set(c.get() + 1));
                return Ok(());
            }
        }
    }
    if SKIP_NONTERMINATING.with(|c| c.get()) && e1 == Exit::Budget {
        // the original never finishes (e.g. a probe sequence that never reaches an empty slot): where the budget runs out is engine specific
        NONTERMINATING_SKIPPED.with(|c| c.set(c.get() + 1));
        return Ok(());
    }
    EXITS.with(|m| *m.borrow_mut().entry(format!("{:?}", e1).split(|c| c == '{' || c == '(').next().unwrap().to_string()).or_insert(0) += 1);
    // jit
    CUR_CODE.with(|c| *c.borrow_mut() = code.to_vec());
    install_backend();
    let mut m2 = build_mem(seed, code);
    if let Some((a, v)) = patch {
        m2.write_bytes(a, &v.to_le_bytes()).unwrap();
    }
    if let Some(f) = init {
        f(&mut m2);
    }
    let mut c2 = cpu0.clone();
    let mut bc2 = BlockCache::new();
    bc2.jit.threshold = 1;
    bc2.jit.trust_loads = trust;
    bc2.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    let e2 = c2.run(&mut m2, &mut bc2, 100_000);
    FP_HELPERS_SEEN.with(|c| {
        let mut v = c.get();
        for i in 0..4 { v[i] += bc2.jit.stats.fp_helper_calls[i]; }
        c.set(v);
    });
    INTRINSICS_SEEN.with(|c| c.set(c.get() + bc2.jit.stats.intrinsics));
    THUNKS_SEEN.with(|c| c.set(c.get() + bc2.jit.stats.thunk_calls));
    let tuple = (c2.lf_kind, c2.lf_a, c2.lf_b, c2.lf_r);
    c2.lf_flush();
    let mut msg = Vec::new();
    if e1 != e2 {
        msg.push(format!("exit: interp {:?} jit {:?}", e1, e2));
    }
    if let Some(d) = diff_cpu(&c1, &c2) {
        msg.push(d);
    }
    if m1.raw() != m2.raw() {
        let a = m1.raw();
        let b = m2.raw();
        let first = (0..a.len()).find(|&i| a[i] != b[i]).unwrap();
        msg.push(format!("memory differs, first at {:#x}: interp {:#04x} jit {:#04x}", first, a[first], b[first]));
    }
    if msg.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "seed {:#x} exit={:?} (jit exit tuple at stop: {:x?})\ninitial: r={:x?} fl={:#x} fs={:#x} gs={:#x} fpu top={} tag={:#06x} sw={:#06x} cw={:#06x} st={:x?}\ncode:\n{}\n{}",
            seed,
            e1,
            tuple,
            cpu0.r,
            cpu0.fl,
            cpu0.fs_base,
            cpu0.gs_base,
            cpu0.fpu.top,
            cpu0.fpu.tag,
            cpu0.fpu.sw,
            cpu0.fpu.cw,
            cpu0.fpu.st.iter().map(|r| (r.mant, r.se)).collect::<Vec<_>>(),
            describe(code),
            msg.join("\n")
        ))
    }
}

fn fuzz(name: &str, fams: &[&str], iters: u32, max_len: usize) {
    // The wasmi executor recurses on the host stack per executed wasm instruction (its handler
    // chain is not tail-call-optimised in this build), and a fuzz walk through random memory now
    // that (almost) every opcode is implemented can stay inside one chained region run for a large
    // part of the 100k instruction budget. The default 2 MB test-thread stack is not enough for
    // that; run the driver on a big-stack thread so plain `cargo test` works without RUST_MIN_STACK.
    let name = name.to_string();
    let fams: Vec<String> = fams.iter().map(|s| s.to_string()).collect();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let fams: Vec<&str> = fams.iter().map(|s| s.as_str()).collect();
            fuzz_on(&name, &fams, iters, max_len)
        })
        .unwrap()
        .join()
        .unwrap();
}

/// Run a test body on a big-stack thread; see `fuzz` for why (wasmi recurses per wasm instruction).
fn big_stack(f: fn()) {
    std::thread::Builder::new().stack_size(256 << 20).spawn(f).unwrap().join().unwrap();
}

fn fuzz_on(name: &str, fams: &[&str], iters: u32, max_len: usize) {
    let mut failures = 0;
    // JIT_SEED=<seed> replays one failing case (with JIT_DUMP=<file> to keep the generated wasm)
    let only: Option<u64> = std::env::var("JIT_SEED").ok().and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    for i in 0..iters {
        let seed = only.unwrap_or((i as u64) * 7919 + name.len() as u64 * 104729);
        FP_BENIGN.with(|f| f.set(i % 4 != 0));
        let mut rng = Rng::new(seed);
        let n = 1 + rng.below(max_len as u32) as usize;
        let code = assemble(&mut rng, fams, n);
        let cpu = initial_cpu(&mut rng);
        if let Err(e) = check_case(seed, &code, &cpu) {
            eprintln!("=== {name}: MISMATCH #{failures}\n{e}\n");
            failures += 1;
            if failures >= 3 {
                break;
            }
        }
        if only.is_some() {
            break;
        }
    }
    FP_BENIGN.with(|f| f.set(false));
    if std::env::var("JIT_FUZZ_STATS").is_ok() {
        EXITS.with(|m| eprintln!("[fuzz {name}] interpreter exits: {:?}", m.borrow()));
    }
    if std::env::var("JIT_TRUST").is_ok() {
        let skipped = TRUST_SKIPPED.with(|c| c.replace(0));
        eprintln!("[fuzz {name}] trust mode: {skipped} of {} cases skipped (the interpreter faulted on a read)", iters);
    }
    assert_eq!(failures, 0, "{name}: JIT disagrees with the interpreter");
}

fn iters() -> u32 {
    std::env::var("JIT_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(300)
}

// ---- tests -----------------------------------------------------------------------------------
#[test]
fn smoke_mov_add() {
    // mov eax, 5 ; add eax, 7 ; int 0x80
    let code = [0xb8, 5, 0, 0, 0, 0x83, 0xc0, 7, 0xcd, 0x80];
    let mut rng = Rng::new(1);
    let cpu = initial_cpu(&mut rng);
    check_case(1, &code, &cpu).unwrap();
}

/// Replay arbitrary code bytes on the machine state of a seed: JIT_HEX="0fa5e3 13db" JIT_SEED=a4b65c
#[test]
fn repro() {
    let (Ok(hex), Ok(seed)) = (std::env::var("JIT_HEX"), std::env::var("JIT_SEED")) else { return };
    let seed = u64::from_str_radix(seed.trim_start_matches("0x"), 16).unwrap();
    let mut code: Vec<u8> = hex.split_whitespace().flat_map(|w| (0..w.len() / 2).map(move |i| u8::from_str_radix(&w[2 * i..2 * i + 2], 16).unwrap())).collect();
    code.extend_from_slice(&[0xcd, 0x80]);
    // same register file as `fuzz` derives from the seed (the generator consumes rng state first)
    let mut rng = Rng::new(seed);
    let n = 1 + rng.below(40) as usize;
    let _ = n;
    let cpu = initial_cpu(&mut Rng::new(seed ^ 0x77));
    if let Err(e) = check_case(seed, &code, &cpu) {
        panic!("{e}");
    }
}

#[test]
fn flags_eval_matches_interpreter() {
    // jit_flags::eval is the definition of lazy flags: check it against the interpreter's own flag helpers.
    let mut rng = Rng::new(42);
    for _ in 0..20000 {
        let sz = rng.pick(&[1u8, 2, 4]);
        let m = mask(sz);
        let a = interesting(&mut rng) & m;
        let b = interesting(&mut rng) & m;
        let mut c = Cpu::new();
        c.fl = rng.next() as u32 & ARITH | 0x202;
        let carry_in = (c.fl & CF != 0) as u32;
        let check = |c: &Cpu, kind: u32, a: u32, b: u32, r: u32, what: &str| {
            let got = jit_flags::eval(kind, a, b, r);
            assert_eq!(got, c.fl & ARITH, "{what} sz={sz} a={a:#x} b={b:#x} r={r:#x}");
        };
        let r = c.add_flags(a, b, 0, sz);
        check(&c, jit_flags::lk(jit_flags::OP_ADD, sz), a, b, r, "add");
        let mut c2 = Cpu::new();
        c2.fl = c.fl;
        let r = c2.add_flags(a, b, carry_in, sz);
        check(&c2, jit_flags::lk(jit_flags::OP_ADC, sz), a, b, r, "adc");
        let mut c3 = Cpu::new();
        let r = c3.sub_flags(a, b, 0, sz);
        check(&c3, jit_flags::lk(jit_flags::OP_SUB, sz), a, b, r, "sub");
        let mut c4 = Cpu::new();
        c4.fl = c.fl;
        let r = c4.sub_flags(a, b, carry_in, sz);
        check(&c4, jit_flags::lk(jit_flags::OP_SBB, sz), a, b, r, "sbb");
        let mut c5 = Cpu::new();
        let r = c5.logic_flags(a ^ b, sz);
        check(&c5, jit_flags::lk(jit_flags::OP_LOGIC, sz), 0, 0, r, "logic");
        // inc/dec: CF preserved
        for (inc, op) in [(true, jit_flags::OP_INC), (false, jit_flags::OP_DEC)] {
            let mut c6 = Cpu::new();
            c6.fl = c.fl;
            let cf = c6.flag(CF);
            let r = if inc { c6.add_flags(a, 1, 0, sz) } else { c6.sub_flags(a, 1, 0, sz) };
            c6.set_flag(CF, cf);
            check(&c6, jit_flags::lk(op, sz), a, cf as u32, r, if inc { "inc" } else { "dec" });
        }
        // shifts (count 1..31)
        let cnt = 1 + rng.below(31);
        for (mn, op) in [(iced_x86::Mnemonic::Shl, jit_flags::OP_SHL), (iced_x86::Mnemonic::Shr, jit_flags::OP_SHR), (iced_x86::Mnemonic::Sar, jit_flags::OP_SAR)] {
            let mut c7 = Cpu::new();
            c7.fl = 0x202;
            let r = c7.shift(mn, a, cnt, sz);
            check(&c7, jit_flags::lk(op, sz), a, cnt, r, &format!("{:?} cnt={}", mn, cnt));
        }
    }
}

#[test]
fn fuzz_alu() {
    fuzz("alu", &["alu", "grp1", "test"], iters(), 5);
}
#[test]
fn fuzz_mov() {
    fuzz("mov", &["mov", "lea", "xchg"], iters(), 5);
}
#[test]
fn fuzz_incdec_negnot() {
    fuzz("incdec", &["incdec", "negnot", "alu"], iters(), 6);
}
#[test]
fn fuzz_shift() {
    fuzz("shift", &["shift", "dshift", "alu"], iters(), 5);
}
#[test]
fn fuzz_muldiv() {
    fuzz("muldiv", &["muldiv", "alu", "mov"], iters(), 5);
}
#[test]
fn fuzz_stack() {
    fuzz("stack", &["pushpop", "mov", "alu"], iters(), 8);
}
#[test]
fn fuzz_setcmov() {
    fuzz("setcmov", &["setcmov", "alu", "incdec", "shift"], iters(), 6);
}
#[test]
fn fuzz_bit_atomic() {
    fuzz("bit", &["bit", "atomic", "alu"], iters(), 5);
}
#[test]
fn fuzz_misc_string() {
    fuzz("misc", &["misc", "string", "alu"], iters(), 5);
}
#[test]
fn fuzz_branch() {
    fuzz("branch", &["branch", "alu", "incdec", "mov", "setcmov", "callret"], iters(), 12);
}
#[test]
fn fuzz_sse_x87() {
    fuzz("sse", &["sse", "alu", "mov", "pair"], iters(), 6);
    fuzz("x87", &["x87", "alu", "mov", "pair"], iters(), 6);
    fuzz("fp-mix", &["sse", "x87", "alu", "mov", "incdec", "shift", "setcmov", "branch"], iters(), 12);
}
#[test]
fn fuzz_mmx() {
    MMX_MODE.with(|f| f.set(true));
    fuzz("mmx", &["mmx"], iters() * 6, 6);
    fuzz("mmx-mix", &["mmx", "alu", "mov", "branch", "simd", "x87", "mmx"], iters() * 3, 14);
    FP_MODE.with(|f| f.set(true));
    fuzz("mmx-fp", &["mmx", "fp", "mmx"], iters() * 4, 8);
    FP_MODE.with(|f| f.set(false));
    MMX_MODE.with(|f| f.set(false));
}

/// FIST/FISTP/FST/FSTP/FLD on boundary values under every rounding mode.
#[test]
fn x87_data_boundaries_agree_with_interpreter() {
    let vals: [f64; 22] = [
        0.0, -0.0, 0.3, -0.3, 0.5, -0.5, 0.7, 1.0, 1.5, -1.5, 2.5, -2.5, 460.7, -460.7, 32767.5, -32768.5, 2147483647.4, 2147483647.6, -2147483648.0, -2147483648.5, 2147483648.0, 1e-300,
    ];
    let ops: [&[u8]; 8] = [&[0xdb, 0x16], &[0xdb, 0x1e], &[0xdf, 0x16], &[0xdf, 0x1e], &[0xdd, 0x1e], &[0xd9, 0x1e], &[0xdd, 0x16], &[0xd9, 0x16]];
    let mut n = 0;
    for (i, &v) in vals.iter().enumerate() {
        for (j, op) in ops.iter().enumerate() {
            for cw in [0x037fu16, 0x077f, 0x0b7f, 0x0f7f, 0x027f] {
                let seed = 0x9000 + (i * 100 + j * 10) as u64;
                let mut code = op.to_vec();
                code.extend_from_slice(&[0xcd, 0x80]);
                let mut cpu = initial_cpu(&mut Rng::new(seed));
                let mut f = crate::fpu::Fpu::new();
                cpu.fpu.st[7] = f.f64_to_f80(v.to_bits());
                cpu.fpu.top = 7;
                cpu.fpu.tag = 0x3fff | 0x0000; // register 7 valid, the others (bits 0..13) valid too, 6 empty below
                cpu.fpu.tag = 0x0fff;
                cpu.fpu.cw = cw;
                cpu.r[ESI] = DATA + 0x100;
                if let Err(e) = check_case(seed, &code, &cpu) {
                    panic!("value {v} op {j} cw {cw:#x}: {e}");
                }
                n += 1;
            }
        }
    }
    assert!(n > 500);
}

#[test]
fn fuzz_x87_data() {
    FP_MODE.with(|f| f.set(true));
    X87_MODE.with(|f| f.set(true));
    fuzz("x87m", &["x87m"], iters() * 6, 8);
    fuzz("x87m-mix", &["x87m", "x87", "alu", "mov", "mmx", "fp"], iters() * 3, 14);
    X87_MODE.with(|f| f.set(false));
    FP_MODE.with(|f| f.set(false));
}

#[test]
fn fuzz_fp() {
    FP_MODE.with(|f| f.set(true));
    fuzz("fp", &["fp"], iters() * 4, 5);
    fuzz("fp-mix", &["fp", "alu", "mov", "branch", "setcmov", "incdec"], iters() * 2, 12);
    FP_MODE.with(|f| f.set(false));
}
/// Boundary cases of the scalar floating-point fast paths that random operands practically never reach.
#[test]
fn fp_boundaries_agree_with_interpreter() {
    FP_MODE.with(|f| f.set(true));
    let d = |x: f64| x.to_bits();
    let f = |x: f32| x.to_bits() as u64;
    let min_n = 0x0010_0000_0000_0000u64;
    let just_below_one = 0x3fef_ffff_ffff_ffffu64;
    let just_above_one = 0x3ff0_0000_0000_0001u64;
    let pairs: Vec<(u64, u64)> = vec![
        (min_n, just_below_one),
        (min_n, just_above_one),
        (min_n | 1, just_below_one),
        (0x7fef_ffff_ffff_ffff, just_above_one),
        (0x7fef_ffff_ffff_ffff, d(1.0)),
        (d(0.0), d(5.0)),
        (d(5.0), d(0.0)),
        (d(-0.0), d(0.0)),
        (d(1.0), d(3.0)),
        (d(1e-300), d(1e-300)),
        (d(1e300), d(1e300)),
        (d(2147483647.5), d(-2147483648.5)),
        (d(-2147483648.9), d(2147483648.0)),
        (d(f64::INFINITY), d(1.0)),
        (d(f64::NAN), d(1.0)),
        (f(f32::MIN_POSITIVE) | (f(1.0) << 32), f(1.0 - f32::EPSILON / 2.0) | (f(3.0) << 32)),
        (f(f32::MAX), f(1.0 + f32::EPSILON)),
        (f(16777217.0), f(-16777216.0)),
        (f(2147483520.0), f(-2147483648.0)),
        (f(2147483648.0), f(0.0)),
        (0x3680_0000, 0x3f7f_ffff),
    ];
    // every scalar opcode with xmm0 (dst) and xmm1 / [esi] (source), prefix F2 = double, F3 = single, none/66 = compares
    let ops: Vec<Vec<u8>> = {
        let mut v = Vec::new();
        for pre in [0xf2u8, 0xf3] {
            for opc in [0x58u8, 0x59, 0x5c, 0x5e, 0x51, 0x5d, 0x5f, 0x5a] {
                v.push(vec![pre, 0x0f, opc, 0xc1]); // op xmm0, xmm1
                v.push(vec![pre, 0x0f, opc, 0x06]); // op xmm0, [esi]
            }
            v.push(vec![pre, 0x0f, 0x2c, 0xc1]); // cvtt..2si eax, xmm1
            v.push(vec![pre, 0x0f, 0x2c, 0x06]); // cvtt..2si eax, [esi]
        }
        for pre in [&[0x66u8][..], &[][..]] {
            for opc in [0x2eu8, 0x2f] {
                let mut a = pre.to_vec();
                a.extend_from_slice(&[0x0f, opc, 0xc1, 0x0f, 0x92, 0xc3, 0x0f, 0x94, 0xc7, 0x0f, 0x9a, 0xc2]); // + setb bl, sete bh, setp dl
                v.push(a);
            }
        }
        v
    };
    let mut n = 0;
    for (i, &(a, b)) in pairs.iter().enumerate() {
        for (j, op) in ops.iter().enumerate() {
            for mx in [0x1fa0u32, 0x1f80, 0x1fa0 | 0x2000, 0x1fa0 | 0x8000, 0x1fa0 | 0x40] {
                let seed = 0x7000 + (i * 1000 + j * 10) as u64;
                let mut code = op.clone();
                code.extend_from_slice(&[0xcd, 0x80]);
                let mut cpu = initial_cpu(&mut Rng::new(seed));
                cpu.xmm[0] = a as u128 | ((0x1234_5678_9abc_def0u128) << 64);
                cpu.xmm[1] = b as u128 | ((0x0fed_cba9_8765_4321u128) << 64);
                cpu.mxcsr = mx;
                cpu.r[ESI] = DATA + 0x100;
                cpu.r[EAX] = 0x55aa55aa;
                // memory operand: b at [esi]
                if let Err(e) = check_case_with(seed, &code, &cpu, Some((DATA + 0x100, b))) {
                    panic!("pair {i} op {j} mxcsr {mx:#x}: {e}");
                }
                n += 1;
            }
        }
    }
    assert!(n > 1000);
    FP_MODE.with(|f| f.set(false));
}

#[test]
fn fuzz_simd() {
    fuzz("simd", &["simd"], iters() * 3, 5);
    fuzz("simd-mix", &["simd", "alu", "mov", "incdec", "pair", "string"], iters() * 2, 12);
}
#[test]
fn fuzz_pair() {
    fuzz("pair", &["pair"], iters() * 4, 4);
    fuzz("pairs2", &["pair", "branch", "mov"], iters() * 2, 10);
}
#[test]
fn fuzz_all() {
    fuzz(
        "all",
        &["alu", "grp1", "test", "mov", "lea", "xchg", "incdec", "negnot", "muldiv", "shift", "dshift", "pushpop", "setcmov", "bit", "atomic", "misc", "string", "branch", "pair", "callret"],
        iters(),
        14,
    );
}

#[test]
fn iced_flag_info_sanity() {
    use iced_x86::{Decoder, DecoderOptions, RflagsBits as R};
    for (bytes, name) in [(&[0xc0u8, 0x97, 0x0f, 0, 0, 0, 9][..], "rcl imm"), (&[0x0f, 0x2e, 0xc1][..], "ucomiss"), (&[0xdf, 0xe0][..], "fnstsw"), (&[0x0f, 0x58, 0xc1][..], "addps"), (&[0xd1, 0xe0][..], "shl 1"), (&[0xc1, 0xe0, 0][..], "shl 0")] {
        let i = Decoder::with_ip(32, bytes, 0, DecoderOptions::NONE).decode();
        eprintln!("{name:10}: read={:#x} modified={:#x} (arith mask {:#x})", i.rflags_read(), i.rflags_modified(), R::OF | R::SF | R::ZF | R::AF | R::CF | R::PF);
    }
}

/// SMC cooldown: a head whose region was dropped by invalidation must not recompile until the cooldown
/// expires (Flash's AVM2 JIT patches live code in bursts; chasing every patch is a compile storm).
#[test]
fn prot_flips_park_and_unpark_regions_without_recompiling() {
    use crate::mem::{Memory, P_MAPPED, P_R, P_W, P_X};
    use crate::run::BlockCache;
    install_backend();
    let rig = || {
        let mut m = Memory::new(32 * 4096);
        m.set_prot(0, 32 * 4096, P_MAPPED | P_R | P_W | P_X);
        m.write_bytes(CODE, &[0xb8, 1, 0, 0, 0, 0xcd, 0x82]).unwrap();
        m.mark_code(CODE, 8);
        let mut bc = BlockCache::new();
        bc.jit.threshold = 1; // eager: no cooldown, compile at first sight
        (m, bc)
    };
    let (mut m, mut bc) = rig();
    assert!(bc.compile_region(&mut m, CODE));
    let regions0 = bc.jit.stats.regions;
    // W^X flip: the page loses X -> the region parks (module kept), dispatch entry gone
    m.set_prot(CODE, 4096, P_MAPPED | P_R | P_W);
    bc.sync(&mut m);
    assert!(!bc.jit.has_entry(CODE), "parked region is not dispatchable");
    assert_eq!(bc.jit.stats.parked, 1);
    assert_eq!(bc.jit.stats.invalidated, 0, "parking must not drop the module");
    // executable again: unpark re-registers the very same region
    m.set_prot(CODE, 4096, P_MAPPED | P_R | P_W | P_X);
    bc.sync(&mut m);
    assert!(bc.jit.has_entry(CODE), "unparked region dispatches again");
    assert_eq!(bc.jit.stats.unparked, 1);
    assert_eq!(bc.jit.stats.regions, regions0, "no recompile across the flip");
    // a real store into the bytes still drops it
    m.w32(CODE, 0x9090_9090).unwrap();
    bc.sync(&mut m);
    assert!(!bc.jit.has_entry(CODE));
    assert_eq!(bc.jit.stats.inv_store, 1);
    // PROT_NONE (the page is gone) drops what is parked on it outright
    let (mut m, mut bc) = rig();
    assert!(bc.compile_region(&mut m, CODE));
    m.set_prot(CODE, 4096, 0);
    bc.sync(&mut m);
    assert!(!bc.jit.has_entry(CODE));
    assert_eq!(bc.jit.stats.inv_prot, 1);
}

/// SMC cooldown: a head whose region was dropped by invalidation must not recompile until the cooldown
/// expires (Flash's AVM2 JIT patches live code in bursts; chasing every patch is a compile storm).
#[test]
fn smc_cooldown_defers_recompilation_of_invalidated_heads() {
    use crate::mem::{Memory, P_MAPPED, P_R, P_W, P_X};
    use crate::run::BlockCache;
    install_backend();
    let rig = |threshold: u32| {
        let mut m = Memory::new(32 * 4096);
        m.set_prot(0, 32 * 4096, P_MAPPED | P_R | P_W | P_X);
        m.write_bytes(CODE, &[0xb8, 1, 0, 0, 0, 0xcd, 0x82]).unwrap();
        m.mark_code(CODE, 8);
        let mut bc = BlockCache::new();
        bc.jit.threshold = threshold;
        bc.jit.cool_ms = 40;
        (m, bc)
    };
    let (mut m, mut bc) = rig(4);
    assert!(bc.compile_region(&mut m, CODE), "first compile");
    // SMC onto the region's bytes: invalidation drops it and cools the head
    m.w32(CODE, 0x9090_9090).unwrap();
    bc.sync(&mut m);
    assert!(!bc.jit.has_entry(CODE));
    assert!(!bc.compile_region(&mut m, CODE), "the cooldown refuses an immediate recompile");
    std::thread::sleep(std::time::Duration::from_millis(60));
    assert!(bc.compile_region(&mut m, CODE), "after the cooldown the head compiles again");
    // eager mode (threshold 1: fuzz, difftest JIT=1) never cools
    let (mut m, mut bc) = rig(1);
    assert!(bc.compile_region(&mut m, CODE));
    m.w32(CODE, 0x9090_9090).unwrap();
    bc.sync(&mut m);
    assert!(bc.compile_region(&mut m, CODE), "eager mode ignores the cooldown");
}

/// Regression: a `jmp` to a node that was merged into the same chain must continue inline. It used to stop the
/// chain at the `jmp`, never translate the merged node and leave through the dispatcher on every execution.
#[test]
fn jmp_to_merged_node_is_translated_inline() {
    // add eax,ebx ; jmp L ; nop nop ; L: sub eax,ebx ; int 0x80      (L has the jmp as its only predecessor)
    let code: [u8; 10] = [0x01, 0xd8, 0xeb, 0x02, 0x90, 0x90, 0x29, 0xd8, 0xcd, 0x80];
    let mut dec = iced_x86::Decoder::with_ip(32, &code, CODE as u64, iced_x86::DecoderOptions::NONE);
    let mut block0 = Vec::new();
    let mut block1 = Vec::new();
    while dec.can_decode() {
        let i = dec.decode();
        let ip = i.ip() as u32;
        if ip < CODE + 4 {
            block0.push(i);
        } else if ip >= CODE + 6 {
            block1.push(i);
        }
    }
    let nodes = vec![
        crate::jit_x86::NodeIn { start: CODE, end: CODE + 4, ins: &block0, intrinsic: 0, thunk: 0 },
        crate::jit_x86::NodeIn { start: CODE + 6, end: CODE + 10, ins: &block1, intrinsic: 0, thunk: 0 },
    ];
    let lay = crate::jit_x86::Layout::for_cpu(GUEST, RTAB, WTAB, 0);
    let mut stats = crate::jit_x86::FbStats::new();
    let (_, outs) = crate::jit_x86::translate_module(&lay, &[nodes], &mut |_| 0, &mut stats, 0);
    // all four real instructions (add, jmp, sub, int) are translated into the single chain, one head only
    assert_eq!(outs[0].n_ins, 4);
    assert_eq!(outs[0].heads.len(), 1);
}

/// Regression (found with the real game: 9 regions failed to compile with "expected 0 elements on the stack for
/// fallthru"): the memory form of pshufd/pshuflw/pshufhw left a stray value on the wasm stack.
#[test]
fn simd_memory_shuffles_validate_and_agree() {
    for code in [
        &[0x66u8, 0x0f, 0x70, 0x06, 0x1b][..],        // pshufd xmm0, [esi], 0x1b
        &[0xf2, 0x0f, 0x70, 0x0e, 0x4e][..],          // pshuflw xmm1, [esi], 0x4e
        &[0xf3, 0x0f, 0x70, 0x56, 0x10, 0xe4][..],    // pshufhw xmm2, [esi+16], 0xe4
    ] {
        let mut c = code.to_vec();
        c.extend_from_slice(&[0xcd, 0x80]);
        let mut rng = Rng::new(7);
        let mut cpu = initial_cpu(&mut rng);
        cpu.r[ESI] = DATA + 0x2000; // 16-byte aligned, inside the data window
        check_case(7, &c, &cpu).unwrap();
    }
}

/// IMUL leaves AF alone: the JIT has to carry the AF of the previous flag producer through it (it once computed a ^ b instead of a ^ b ^ r).
#[test]
fn imul_preserves_af() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("add [mem],r; imul; int", vec![0x01, 0x57, 0x0d, 0x6b, 0xbc, 0xa4, 0x73, 0x0e, 0x00, 0x00, 0xff, 0xcd, 0x80]),
        ("add; imul; int", vec![0x01, 0xd0, 0x0f, 0xaf, 0xc3, 0xcd, 0x80]),
        ("cmp; imul; int", vec![0x39, 0xd8, 0x0f, 0xaf, 0xc3, 0xcd, 0x80]),
        ("sub; imul; lahf; int", vec![0x29, 0xd8, 0x0f, 0xaf, 0xc3, 0x9f, 0xcd, 0x80]),
    ];
    for (name, code) in cases {
        for i in 0..1500u64 {
            let seed = i * 7919 + 13;
            let mut rng = Rng::new(seed);
            let cpu = initial_cpu(&mut rng);
            if let Err(e) = check_case(seed, &code, &cpu) {
                panic!("{name}: {e}");
            }
        }
    }
}


/// Flag consumers that keep part of the old flags (one-operand mul/imul, rotates, bsf/bsr, bt*, clc/stc/cmc, sahf) after every kind of flag
/// producer, with and without a chain boundary in between (the flag state is then only known at run time): the inline "old flags" forms
/// (`bits_of_kind`, `szpa_dyn`) must agree with the interpreter.
#[test]
fn preserved_flags_match_interpreter() {
    let producers: Vec<(&str, Vec<u8>)> = vec![
        ("add32", vec![0x01, 0xd8]), ("add8", vec![0x00, 0xd8]), ("add16", vec![0x66, 0x01, 0xd8]),
        ("sub32", vec![0x29, 0xd8]), ("sub8", vec![0x28, 0xd8]), ("sub16", vec![0x66, 0x29, 0xd8]),
        ("cmp32", vec![0x39, 0xd8]), ("cmp8", vec![0x38, 0xd8]), ("cmp16", vec![0x66, 0x39, 0xd8]),
        ("and32", vec![0x21, 0xd8]), ("and8", vec![0x20, 0xd8]), ("and16", vec![0x66, 0x21, 0xd8]), ("test32", vec![0x85, 0xd8]),
        ("inc32", vec![0x40]), ("dec32", vec![0x48]), ("inc16", vec![0x66, 0x40]), ("dec8", vec![0xfe, 0xc8]),
        ("shl32", vec![0xc1, 0xe0, 0x03]), ("shr32cl", vec![0xd3, 0xe8]), ("sar32", vec![0xd1, 0xf8]), ("shl8", vec![0xc0, 0xe0, 0x02]), ("shr16", vec![0x66, 0xc1, 0xe8, 0x02]),
        ("imul32", vec![0x0f, 0xaf, 0xc3]), ("imul16", vec![0x66, 0x0f, 0xaf, 0xc3]), ("mul32", vec![0xf7, 0xe3]),
        ("adc32", vec![0x11, 0xd8]), ("sbb32", vec![0x19, 0xd8]), ("adc8", vec![0x10, 0xd8]),
        ("neg32", vec![0xf7, 0xd8]), ("rol32", vec![0xc1, 0xc0, 0x05]), ("bsf", vec![0x0f, 0xbc, 0xc3]), ("stc", vec![0xf9]), ("sahf", vec![0x9e]),
    ];
    let boundaries: Vec<(&str, Vec<u8>)> = vec![
        ("none", vec![]),
        ("jmp0", vec![0xe9, 0, 0, 0, 0]),
        ("jcc-join", vec![0x0f, 0x84, 0x01, 0, 0, 0, 0x90]),   // jz +1 ; nop ; (join)
        ("jcc-join2", vec![0x0f, 0x85, 0x02, 0, 0, 0, 0x90, 0x90]),
    ];
    let consumers: Vec<(&str, Vec<u8>)> = vec![
        ("mul", vec![0xf7, 0xe3]), ("imul1", vec![0xf7, 0xeb]), ("rol", vec![0xc1, 0xc0, 0x03]), ("ror_cl", vec![0xd3, 0xc8]), ("rol8", vec![0xd0, 0xc0]),
        ("bsf", vec![0x0f, 0xbc, 0xc3]), ("bsr", vec![0x0f, 0xbd, 0xc3]), ("bt", vec![0x0f, 0xa3, 0xd8]), ("bts", vec![0x0f, 0xba, 0xe8, 0x03]),
        ("clc", vec![0xf8]), ("stc", vec![0xf9]), ("cmc", vec![0xf5]), ("sahf", vec![0x9e]), ("lahf", vec![0x9f]),
        ("imul2", vec![0x0f, 0xaf, 0xc3]), ("imul3", vec![0x6b, 0xc3, 0x05]), ("imul2_16", vec![0x66, 0x0f, 0xaf, 0xc3]),
    ];
    let seeds: u64 = std::env::var("JIT_PRESERVED_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
    let mut failures = Vec::new();
    let mut n = 0u32;
    for (pi, (pn, p)) in producers.iter().enumerate() {
        for (bi, (bn, b)) in boundaries.iter().enumerate() {
            for (ci, (cn, c)) in consumers.iter().enumerate() {
                for k in 0..seeds {
                    let seed = 0x9f10_0000 + ((pi * 64 + bi * 16 + ci) as u64) * 64 + k;
                    let mut rng = Rng::new(seed);
                    let mut cpu = initial_cpu(&mut rng);
                    // small values now and then so that zero results, carries and sign changes happen
                    if k % 2 == 0 {
                        for r in [EAX, EBX, ECX] {
                            cpu.r[r] = rng.pick(&[0u32, 1, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 3, 0x1000_0001]);
                        }
                    }
                    cpu.r[ECX] = (cpu.r[ECX] & !0x1f) | rng.below(32);
                    let mut code: Vec<u8> = Vec::new();
                    code.extend_from_slice(p);
                    code.extend_from_slice(b);
                    code.extend_from_slice(c);
                    code.extend_from_slice(&[0xcd, 0x80]);
                    n += 1;
                    if let Err(e) = check_case(seed, &code, &cpu) {
                        failures.push(format!("{pn} / {bn} / {cn}: {e}"));
                        if failures.len() >= 3 {
                            break;
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} failures of {} cases, e.g.\n{}", failures.len(), n, failures[0]);
    eprintln!("preserved flags: {} cases", n);
}

/// x87 add / sub / mul at the edges the random generator rarely reaches: exponent differences around every shift boundary of the
/// 128-bit alignment (0, 1, 63, 64, 65, 127, 128, ...), odd and even significands (sticky bits), all-ones significands (rounding
/// carries), exact cancellation, extreme exponents, every precision and rounding mode.
#[test]
fn x87_arith_boundaries_match_interpreter() {
    let diffs: [i32; 20] = [0, 1, 2, 3, 31, 32, 33, 62, 63, 64, 65, 66, 100, 126, 127, 128, 129, 200, 1000, 20000];
    let cws: [u16; 16] = [0x037f, 0x077f, 0x0b7f, 0x0f7f, 0x027f, 0x067f, 0x0a7f, 0x0e7f, 0x007f, 0x047f, 0x087f, 0x0c7f, 0x017f, 0x057f, 0x097f, 0x0d7f];
    // op bytes: d8 /0 fadd st0,st1; d8 e1 fsub st0,st1; d8 e9 fsubr st0,st1; d8 c9 fmul; dc c1 .. (st1 as destination); de c1 faddp; de e9; de e1; de c9
    let ops: [&[u8]; 12] = [&[0xd8, 0xc1], &[0xd8, 0xe1], &[0xd8, 0xe9], &[0xd8, 0xc9], &[0xdc, 0xc1], &[0xdc, 0xe1], &[0xdc, 0xe9], &[0xdc, 0xc9], &[0xde, 0xc1], &[0xde, 0xe1], &[0xde, 0xe9], &[0xde, 0xc9]];
    let mants = |rng: &mut Rng| -> u64 {
        match rng.below(8) {
            0 => u64::MAX,
            1 => 1 << 63,
            2 => (1 << 63) | 1,
            3 => (1 << 63) | (1 << 11),                // exactly half-way for 53 bits
            4 => (1 << 63) | (1 << 40),                // exactly half-way for 24 bits
            5 => u64::MAX << 11,
            6 => (rng.next() | (1 << 63)) & !1,
            _ => rng.next() | (1 << 63),
        }
    };
    let mut failures = Vec::new();
    let mut n = 0u32;
    for (oi, op) in ops.iter().enumerate() {
        for &d in &diffs {
            for k in 0..24u64 {
                let seed = 0x87a10000 + ((oi * 32) as u64 + diffs.iter().position(|&x| x == d).unwrap() as u64) * 64 + k;
                let mut r = Rng::new(seed);
                let mut cpu = initial_cpu(&mut r);
                let e0: i32 = match r.below(6) {
                    0 => 0x3fff,
                    1 => 1 + d.min(20000) + r.below(3) as i32,                 // st1's exponent stays >= 1
                    2 => 0x7ffe - r.below(3) as i32,
                    3 => 0x4000 + r.below(100) as i32,
                    4 => (d + 2).min(0x7ffe),
                    _ => 0x3000 + r.below(0x2000) as i32,
                };
                let e1 = (e0 - d).clamp(1, 0x7ffe);
                let (m0, mut m1) = (mants(&mut r), mants(&mut r));
                if k % 6 == 0 {
                    m1 = m0; // identical magnitudes: cancellation or doubling
                }
                let (s0, s1) = (r.below(2) as u16, r.below(2) as u16);
                let (a, b) = if k % 2 == 0 { (e0, e1) } else { (e1, e0) };
                cpu.fpu.top = 0;
                cpu.fpu.tag = 0xfff0;
                cpu.fpu.st[0] = crate::fpu::F80 { mant: m0, se: (a as u16) | (s0 << 15) };
                cpu.fpu.st[1] = crate::fpu::F80 { mant: m1, se: (b as u16) | (s1 << 15) };
                // zeros (either operand, or both, either sign): exact results, the other operand rounded to the precision
                if k % 8 == 3 || k % 8 == 7 {
                    cpu.fpu.st[0] = crate::fpu::F80 { mant: 0, se: s0 << 15 };
                }
                if k % 8 == 5 || k % 8 == 7 {
                    cpu.fpu.st[1] = crate::fpu::F80 { mant: 0, se: s1 << 15 };
                }
                cpu.fpu.cw = cws[r.below(16) as usize];
                cpu.fpu.sw = if r.chance(30) { 0x0020 } else { 0 };
                let mut code = op.to_vec();
                code.extend_from_slice(&[0xcd, 0x80]);
                n += 1;
                if let Err(e) = check_case(seed, &code, &cpu) {
                    failures.push(e);
                    if failures.len() >= 3 {
                        break;
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{} failures of {} cases, e.g.\n{}", failures.len(), n, failures[0]);
    eprintln!("x87 arith boundaries: {} cases", n);
}

// ---- intrinsic fast paths (jit_intr) ---------------------------------------------------------
#[path = "jit_intr_range_tests.rs"]
mod intr_range_tests;
/// The AVM2 hash `find` (the exact bytes of the runtime function) called from a small driver with random tables and argument layouts
/// must leave exactly the machine the original code leaves: registers, flags, esp/eip, the dead stack frame and every fault.
#[test]
fn intrinsic_find_matches_interpreter() {
    big_stack(intrinsic_find_body);
}

fn intrinsic_find_body() {
    use crate::jit_x86::intr::FIND_CODE;
    let n: u64 = std::env::var("JIT_INTR_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let only: Option<u64> = std::env::var("JIT_SEED").ok().and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    let before = INTRINSICS_SEEN.with(|c| c.get());
    SKIP_NONTERMINATING.with(|c| c.set(true));
    let skipped0 = NONTERMINATING_SKIPPED.with(|c| c.get());
    let mut failures = Vec::new();
    for seed in 0..n {
        let seed = only.unwrap_or(0xf1d0_0000 + seed);
        let mut rng = Rng::new(seed);
        let func = CODE + 0x400;
        // esp: usually deep inside the stack, sometimes straddling a page boundary, sometimes on a code page or at the end of the mapping
        let esp = match rng.below(12) {
            0 => STACK + 0x1000 * (1 + rng.below(14)) + rng.below(0x40),
            1 => STACK + 0x1000 * (1 + rng.below(14)) - rng.below(0x40),
            2 => STACK + 0x10000 - rng.below(0x30),
            3 => PG_CODEFLAG + rng.below(0x80) + 0x20,
            4 => PG_RONLY + 0x30 + rng.below(0x40),
            _ => STACK + 0x4000 + 4 * rng.below(0x600),
        };
        let ncalls = 1 + rng.below(3);
        struct Call { ht: u32, key: u32, tab: u32, cap: u32, htv: u32, words: Vec<u32>, }
        let mut calls = Vec::new();
        for _ in 0..ncalls {
            // a small table that lies on top of the dead frame the original function pushes into: every probe aliases it
            let alias = rng.below(8) == 0;
            let cap = match if alias { 99 } else { rng.below(10) } {
                99 => 1 << (1 + rng.below(4)),
                0 => rng.below(4),
                1 => 1 << (1 + rng.below(9)),
                2 => 0x100 + rng.below(0x600),
                _ => 1 << (3 + rng.below(8)),
            };
            let words_n = cap.min(0x800) as usize;
            let fill = [10u32, 40, 70, 95][rng.below(4) as usize];
            let mut words = Vec::new();
            for _ in 0..words_n {
                words.push(if rng.below(100) < fill { (rng.next() as u32 & 0xffff_fff8) | if rng.chance(15) { 1 } else { 0 } } else { 0 });
            }
            // table: normally inside DATA (RW), sometimes starting close to a protection boundary or inside the dead frame
            let tab = match if alias { 99 } else { rng.below(14) } {
                99 => esp.wrapping_sub(20).wrapping_sub(4 * rng.below(cap.max(1))),
                0 => PG_RONLY + 0x800 + 4 * rng.below(0x100),
                1 => PG_RONLY + 0x1000 - 4 * rng.below(0x40) - 4,
                2 => esp.wrapping_sub(24) + rng.below(40),
                3 => DATA + 0x1000 * rng.below(3) + rng.below(3),
                _ => DATA + 4 * rng.below(0x400),
            };
            let ht = match rng.below(12) {
                0 => esp.wrapping_sub(8 + rng.below(16)),
                1 => PG_NONE + rng.below(8),
                _ => DATA + 0x2800 + 4 * rng.below(0x100),
            };
            let htv = rng.next() as u32;
            let tag = !(htv & 1);
            let key = if words_n > 0 && rng.chance(60) { words[rng.below(words_n as u32) as usize] } else { rng.next() as u32 };
            let _ = tag;
            calls.push(Call { ht, key, tab, cap, htv, words });
        }
        // driver: mov esp, imm; (push cap; push tab; push key; push ht; call func; lea esp,[esp+16])*; int 0x80
        let mut code: Vec<u8> = vec![0xbc];
        code.extend_from_slice(&esp.to_le_bytes());
        for c in &calls {
            for v in [c.cap, c.tab, c.key, c.ht] {
                code.push(0x68);
                code.extend_from_slice(&v.to_le_bytes());
            }
            code.push(0xe8);
            let next = CODE + code.len() as u32 + 4;
            code.extend_from_slice(&func.wrapping_sub(next).to_le_bytes());
            code.extend_from_slice(&[0x8d, 0x64, 0x24, 0x10]); // lea esp,[esp+16]: unlike `add`, it leaves the flags of the call visible
        }
        code.extend_from_slice(&[0xcd, 0x80]);
        assert!(code.len() < 0x400);
        let mut image = code.clone();
        image.resize(0x400, 0x90);
        image.extend_from_slice(&FIND_CODE);
        let mut cpu = initial_cpu(&mut rng);
        cpu.eip = CODE;
        let init = |m: &mut Memory| {
            for c in &calls {
                let _ = m.write_bytes(c.htv_addr(), &c.htv.to_le_bytes());
                let mut bytes = Vec::new();
                for w in &c.words {
                    bytes.extend_from_slice(&w.to_le_bytes());
                }
                let _ = m.write_bytes(c.tab, &bytes);
            }
        };
        impl Call {
            fn htv_addr(&self) -> u32 {
                self.ht
            }
        }
        if let Err(e) = check_case_full(seed, &image, &cpu, None, Some(&init)) {
            failures.push(e);
            if failures.len() >= 3 || only.is_some() {
                break;
            }
        }
    }
    SKIP_NONTERMINATING.with(|c| c.set(false));
    let skipped = NONTERMINATING_SKIPPED.with(|c| c.get()) - skipped0;
    assert!(skipped * 2 < n, "more than half of the cases never terminate ({} of {}): the generator is too hostile", skipped, n);
    let used = INTRINSICS_SEEN.with(|c| c.get()) - before;
    assert!(failures.is_empty(), "{} failures, e.g.\n{}", failures.len(), failures[0]);
    assert!(used > 0, "the fast path was never translated");
    eprintln!("intrinsic find: {} cases ({} skipped: original does not terminate), fast path translated in {} regions", n, skipped, used);
}

/// glibc's `bsearch` (exact bytes) with the runtime's range-test comparator, called from a small driver with random tables, strides,
/// key positions, comparator copies (also corrupted ones) and stack layouts: the fast path must leave exactly the machine the
/// original code leaves (registers, flags, esp/eip, the dead stack frame, every fault) or hand the call to the interpreter.
#[test]
fn intrinsic_bsearch_matches_interpreter() {
    big_stack(intrinsic_bsearch_body);
}

fn intrinsic_bsearch_body() {
    use crate::jit_x86::intr::{BSEARCH_CODE, RANGE_CMP_CODE};
    let n: u64 = std::env::var("JIT_INTR_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
    let only: Option<u64> = std::env::var("JIT_SEED").ok().and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    let before = INTRINSICS_SEEN.with(|c| c.get());
    SKIP_NONTERMINATING.with(|c| c.set(true));
    let skipped0 = NONTERMINATING_SKIPPED.with(|c| c.get());
    let mut failures = Vec::new();
    for seed in 0..n {
        let seed = only.unwrap_or(0xb5e0_0000 + seed);
        let mut rng = Rng::new(seed);
        let func = CODE + 0x400;
        let cmp_main = CODE + 0x600;
        let cmp_data = DATA + 0x2000 + rng.below(0x40) * 4;
        let esp = match rng.below(12) {
            0 => STACK + 0x1000 * (1 + rng.below(14)) + rng.below(0x40),
            1 => STACK + 0x1000 * (1 + rng.below(14)) - rng.below(0x40),
            2 => STACK + 0x10000 - rng.below(0x30),
            3 => PG_CODEFLAG + rng.below(0x80) + 0x40,
            4 => PG_RONLY + 0x50 + rng.below(0x40),
            _ => STACK + 0x4000 + 4 * rng.below(0x600),
        };
        struct Call { keyp: u32, key: u32, base: u32, nmemb: u32, size: u32, cmp: u32, table: Vec<u8> }
        let mut calls = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let nmemb = match rng.below(10) {
                0 => 0,
                1 => 1,
                2 => rng.below(3),
                3 => 0x8000_0000 + rng.below(4),
                4 => 40 + rng.below(100),
                _ => 2 + rng.below(30),
            };
            let size = match rng.below(12) {
                0 => 16,
                1 => 12,
                2 => 4,
                3 => 0,
                4 => 0x1000,
                5 => 0u32.wrapping_sub(8),
                _ => 8,
            };
            let stored = nmemb.min(200) as usize;
            let mut starts = Vec::new();
            let mut a = rng.next() as u32 & 0x00ff_ffff;
            for _ in 0..stored {
                let len = if rng.chance(10) { 0 } else { 1 + rng.below(0x100) };
                let gap = rng.below(0x200);
                // sorted and disjoint (the case the runtime uses); now and then wrapping around 2^32 or out of order
                let (st, ln) = if rng.chance(3) { (rng.next() as u32, rng.next() as u32 & 0xffff_ffff) } else { (a, len) };
                starts.push((st, ln));
                a = a.wrapping_add(len).wrapping_add(gap);
                if rng.chance(2) {
                    a = 0xffff_ff00 + rng.below(0x100);
                }
            }
            let stride = if size == 0 || size > 0x100 && size < 0xffff_fff0 { 8 } else if size > 0xffff_fff0 { 8 } else { size } as usize;
            let mut table = vec![0u8; stored * stride.max(8)];
            for (i, (st, ln)) in starts.iter().enumerate() {
                let o = i * stride;
                if o + 8 <= table.len() {
                    table[o..o + 4].copy_from_slice(&st.to_le_bytes());
                    table[o + 4..o + 8].copy_from_slice(&ln.to_le_bytes());
                }
            }
            let key = match rng.below(8) {
                0 => rng.next() as u32,
                1 if !starts.is_empty() => starts[rng.below(starts.len() as u32) as usize].0,
                2 if !starts.is_empty() => { let (s0, l0) = starts[rng.below(starts.len() as u32) as usize]; s0.wrapping_add(l0) }
                3 if !starts.is_empty() => { let (s0, l0) = starts[rng.below(starts.len() as u32) as usize]; s0.wrapping_add(l0).wrapping_sub(1) }
                4 if !starts.is_empty() => starts[rng.below(starts.len() as u32) as usize].0.wrapping_sub(1),
                5 => 0xffff_ffff,
                6 => 0,
                _ if !starts.is_empty() => { let (s0, l0) = starts[rng.below(starts.len() as u32) as usize]; s0.wrapping_add(rng.below(l0.max(1))) }
                _ => rng.next() as u32,
            };
            let base = match rng.below(14) {
                0 => PG_RONLY + 0x1000 - 8 * rng.below(4) - 4,
                1 => PG_NONE + rng.below(8),
                2 => esp.wrapping_sub(60) + rng.below(64),
                3 => PG_CODEFLAG + 0x100,
                _ => DATA + 0x100 + 8 * rng.below(0x20),
            };
            let keyp = match rng.below(12) {
                0 => esp.wrapping_sub(4 + rng.below(50)),
                1 => PG_NONE + rng.below(8),
                2 => DATA + 0x1000 - 2,
                _ => DATA + 0x2800 + 4 * rng.below(0x100),
            };
            let cmp = match rng.below(14) {
                0 => cmp_data,
                1 => CODE + 0x700,        // not the comparator
                2 => cmp_main + 1 + rng.below(3), // inside it
                3 => esp.wrapping_sub(20),
                _ => cmp_main,
            };
            calls.push(Call { keyp, key, base, nmemb, size, cmp, table });
        }
        let mut code: Vec<u8> = vec![0xbc];
        code.extend_from_slice(&esp.to_le_bytes());
        for c in &calls {
            for v in [c.cmp, c.size, c.nmemb, c.base, c.keyp] {
                code.push(0x68);
                code.extend_from_slice(&v.to_le_bytes());
            }
            code.push(0xe8);
            let next = CODE + code.len() as u32 + 4;
            code.extend_from_slice(&func.wrapping_sub(next).to_le_bytes());
            code.extend_from_slice(&[0x8d, 0x64, 0x24, 0x14]); // lea esp,[esp+20]: keeps the flags of the call visible
        }
        code.extend_from_slice(&[0xcd, 0x80]);
        assert!(code.len() < 0x400);
        let mut image = code.clone();
        image.resize(0x400, 0x90);
        image.extend_from_slice(&BSEARCH_CODE);
        image.resize(0x600, 0x90);
        let mut cmp_bytes = RANGE_CMP_CODE.to_vec();
        image.extend_from_slice(&cmp_bytes);
        image.resize(0x700, 0x90);
        // a comparator with the opposite sense: same length, one byte different
        let mut other = RANGE_CMP_CODE.to_vec();
        other[22] = 0x73; // jae instead of jb
        image.extend_from_slice(&other);
        let mut cpu = initial_cpu(&mut rng);
        cpu.eip = CODE;
        if rng.chance(10) {
            // a corrupted copy in the data window
            cmp_bytes[rng.below(36) as usize] ^= 1 << rng.below(8);
        }
        let init = |m: &mut Memory| {
            for c in &calls {
                let _ = m.write_bytes(c.keyp, &c.key.to_le_bytes());
                let _ = m.write_bytes(c.base, &c.table);
            }
            let _ = m.write_bytes(cmp_data, &cmp_bytes);
        };
        if let Err(e) = check_case_full(seed, &image, &cpu, None, Some(&init)) {
            failures.push(e);
            if failures.len() >= 3 || only.is_some() {
                break;
            }
        }
    }
    SKIP_NONTERMINATING.with(|c| c.set(false));
    let skipped = NONTERMINATING_SKIPPED.with(|c| c.get()) - skipped0;
    assert!(skipped * 3 < n, "more than a third of the cases never terminate ({} of {}): the generator is too hostile", skipped, n);
    let used = INTRINSICS_SEEN.with(|c| c.get()) - before;
    assert!(failures.is_empty(), "{} failures, e.g.\n{}", failures.len(), failures[0]);
    assert!(used > 0, "the fast path was never translated");
    eprintln!("intrinsic bsearch: {} cases ({} skipped: original does not terminate), fast path translated in {} regions", n, skipped, used);
}

/// `call thunk` where the thunk is `mov reg,[esp]; ret` (the i386 PIC idiom) is translated without a call. The result must match the interpreter exactly:
/// the register, ESP, the flags of the surrounding instructions, the return-address slot below ESP and the fault of the push.
#[test]
fn pic_thunk_calls_match_interpreter() {
    let n: u64 = std::env::var("JIT_THUNK_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
    let only: Option<u64> = std::env::var("JIT_SEED").ok().and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    let before = THUNKS_SEEN.with(|c| c.get());
    let mut failures = Vec::new();
    for seed in 0..n {
        let seed = only.unwrap_or(0x7a00_0000 + seed);
        let mut rng = Rng::new(seed);
        let regs = [0u8, 1, 2, 3, 5, 6, 7]; // eax ecx edx ebx ebp esi edi (esp is not a valid thunk register)
        let esp = match rng.below(10) {
            0 => STACK + 0x1000 * (1 + rng.below(14)) + rng.below(8),     // the pushed word may straddle... or sit right above a page boundary
            1 => STACK + 0x1000 * (1 + rng.below(14)) - rng.below(8),
            2 => STACK + 0x10000 + rng.below(8),                           // above the mapping: the push faults
            3 => PG_CODEFLAG + 0x20 + 4 * rng.below(8),
            4 => PG_RONLY + 0x20 + 4 * rng.below(8),
            5 => 4 * rng.below(8),                                         // wraps below zero
            _ => STACK + 0x4000 + 4 * rng.below(0x400),
        };
        let thunk_at = |k: u8| CODE + 0x800 + 8 * k as u32;
        let mut code: Vec<u8> = vec![0xbc];
        code.extend_from_slice(&esp.to_le_bytes());
        for _ in 0..(1 + rng.below(6)) {
            match rng.below(5) {
                0 => code.extend_from_slice(&[0x05, rng.next() as u8, rng.next() as u8, rng.next() as u8, 0x7f]),   // add eax, imm32 (sets flags)
                1 => code.extend_from_slice(&[0x39, 0xd8 + (rng.below(8) as u8)]),                                       // cmp eax, reg
                2 => code.push(0xf9),                                                                                    // stc
                3 => code.extend_from_slice(&[0x83, 0xe9, rng.below(100) as u8]),                                        // sub ecx, imm8
                _ => {}
            }
            let r = rng.pick(&regs);
            code.push(0xe8);
            let next = CODE + code.len() as u32 + 4;
            code.extend_from_slice(&thunk_at(r).wrapping_sub(next).to_le_bytes());
            match rng.below(4) {
                0 => code.extend_from_slice(&[0x83, 0xd0, 0x00]),            // adc eax, 0   (reads CF of what came before the call)
                1 => code.extend_from_slice(&[0x0f, 0x92, 0xc2]),            // setb dl
                2 => code.extend_from_slice(&[0x01, 0xd8]),                  // add eax, ebx
                _ => {}
            }
        }
        code.extend_from_slice(&[0xcd, 0x80]);
        assert!(code.len() < 0x800);
        let mut image = code.clone();
        image.resize(0x800, 0x90);
        for k in 0..8u8 {
            // thunks for registers 0..7 (index 4 = esp is never called): mov reg,[esp] = 8b <00 reg 100> 24 ; ret
            image.extend_from_slice(&[0x8b, (k << 3) | 0x04, 0x24, 0xc3, 0x90, 0x90, 0x90, 0x90]);
        }
        let mut cpu = initial_cpu(&mut rng);
        cpu.eip = CODE;
        if let Err(e) = check_case_full(seed, &image, &cpu, None, None) {
            failures.push(e);
            if failures.len() >= 3 || only.is_some() {
                break;
            }
        }
    }
    let used = THUNKS_SEEN.with(|c| c.get()) - before;
    assert!(failures.is_empty(), "{} failures, e.g.\n{}", failures.len(), failures[0]);
    assert!(used > 0, "thunk calls were never translated inline");
    eprintln!("pic thunk: {} cases, {} thunk calls translated inline", n, used);
}
