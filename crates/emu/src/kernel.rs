//! Single-process Linux/i386 personality: ELF launch, cooperative thread scheduler, futexes,
//! memory map bookkeeping. Syscall bodies live in `sys.rs`.

use crate::cpu::*;
use crate::elf;
use crate::mem::*;
use crate::run::BlockCache;
use crate::vfs::Vfs;
use std::collections::{BTreeMap, VecDeque};

/// The guest's clock. Flash asks for the time ~150 000 times per second; every host read is a JS call that allocates a heap number, so the host
/// clock is read at most every 20 000 retired instructions / 16 queries and interpolated by retired instructions in between (monotonic).
#[derive(Default)]
struct TimeCache {
    valid: bool,
    mono: u64,
    ins: u64,
    /// nanoseconds per retired instruction, x1024
    k: u64,
    last: u64,
    calls: u32,
    rt_valid: bool,
    rt_off: u64,
}

pub trait Host {
    /// Monotonic nanoseconds.
    fn now_ns(&mut self) -> u64;
    /// Unix-epoch nanoseconds (guest clock; configurable by the embedder).
    fn realtime_ns(&mut self) -> u64;
    fn random(&mut self, buf: &mut [u8]);
    /// Bytes the guest wrote to stdout (1) / stderr (2).
    fn console(&mut self, fd: u32, data: &[u8]);
}

pub const ENOSYS: i32 = 38;
pub const EPERM: i32 = 1;
pub const ESRCH: i32 = 3;
pub const EINTR: i32 = 4;
pub const EBADF: i32 = 9;
pub const ECHILD: i32 = 10;
pub const EAGAIN: i32 = 11;
pub const ENOMEM: i32 = 12;
pub const EFAULT: i32 = 14;
pub const EMFILE: i32 = 24;
pub const ENOTTY: i32 = 25;
pub const ESPIPE: i32 = 29;
pub const EPIPE: i32 = 32;
pub const ERANGE: i32 = 34;
pub const ETIMEDOUT: i32 = 110;
pub const EAFNOSUPPORT: i32 = 97;

#[derive(Clone, Debug, PartialEq)]
pub enum Wait {
    None,
    Futex { addr: u32, bitset: u32 },
    Sleep,
    /// Parked until the embedder calls `host_complete` (async hypercalls: fetch, decode, ...).
    Host,
    /// Re-execute the pending syscall once `Kernel::generation` moves past `gen`.
    Retry { gen: u64 },
}

pub struct Thread {
    pub tid: u32,
    pub cpu: Cpu,
    pub wait: Wait,
    pub deadline: Option<u64>,
    /// EAX to deliver when a Futex/Sleep wait ends.
    pub wake_val: i32,
    pub clear_child_tid: u32,
    pub robust_list: u32,
    pub zombie: bool,
    /// guest instructions this thread has executed (diagnostics: shows whether more than one thread is busy)
    pub ins: u64,
    /// Absolute deadline remembered across Retry re-executions (poll/select/read timeouts).
    pub retry_deadline: Option<u64>,
    pub sigmask: u64,
    pub pending: u64,
    /// (si_code, si_pid, si_uid) recorded for each pending signal number.
    pub sig_info: [(i32, u32, u32); 65],
    /// Mask to restore when a handler returns after sigsuspend/pause-style temporary masking.
    pub saved_mask: Option<u64>,
    /// x87/SSE state of contexts interrupted by signal handlers (restored by sigreturn), innermost last.
    pub fp_saves: Vec<(crate::fpu::Fpu, [u128; 8], u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Exited(i32),
    /// Nothing runnable; wake the kernel at this monotonic time (None = until external input).
    Idle(Option<u64>),
    /// Guest executed `int 0x82`; registers of thread `tid` hold the request.
    Hyper { tid: u32 },
    /// Entry observation; guest registers and memory have not been modified by the probe.
    Probe { tid: u32, id: u32, eip: u32 },
    /// Guest crashed (unhandled fault). Message includes registers for triage.
    Crash(String),
    /// Slice finished with work remaining; call `run` again.
    Yield,
}

pub struct SigAction {
    pub handler: u32,
    pub flags: u32,
    pub restorer: u32,
    pub mask: u64,
}

pub struct Kernel {
    pub mem: Memory,
    pub cache: BlockCache,
    pub threads: Vec<Thread>,
    pub cur: usize,
    pub next_tid: u32,
    pub vfs: Vfs,
    pub fds: Vec<Option<crate::sys::Fd>>,
    pub pipes: Vec<crate::sys::Pipe>,
    pub host: Box<dyn Host>,
    /// Region bookkeeping for mmap: start -> (len, label).
    pub regions: BTreeMap<u32, (u32, String)>,
    pub mmap_top: u32,
    pub brk_start: u32,
    pub brk_cur: u32,
    pub brk_limit: u32,
    pub stack_lo: u32,
    pub stack_hi: u32,
    pub exe_path: String,
    pub generation: u64,
    pub sigactions: Vec<SigAction>,
    pub exit_code: Option<i32>,
    pub trace_syscalls: bool,
    /// signals raised to the guest per signal number (diagnostics: a working movie raises none of SIGSEGV/SIGBUS)
    pub sig_stats: [u32; 32],
    /// guest system calls by number (reported by `emu_jit_report`)
    pub sys_counts: [u64; 512],
    /// guest clock cache (see `mono_ns`)
    tc: TimeCache,
    /// guest-address sampling profiler (see `emu_eip_prof_*`): eip>>5 -> samples
    pub eip_prof_on: bool,
    pub eip_prof: std::collections::HashMap<u32, u64>,
    pub unimpl_log: Vec<String>,
    pub futex_waiters: VecDeque<(u32, usize)>,
    pub pid: u32,
    pub hyper_pending: Option<u32>,
    /// CPUID.1:EDX advertised to new guest CPUs (FPU TSC CX8 CMOV MMX FXSR SSE SSE2; nothing newer).
    pub cpuid_edx: u32,
}

pub const STACK_SIZE: u32 = 8 << 20;
pub const AT_NULL: u32 = 0;
pub const AT_PHDR: u32 = 3;
pub const AT_PHENT: u32 = 4;
pub const AT_PHNUM: u32 = 5;
pub const AT_PAGESZ: u32 = 6;
pub const AT_BASE: u32 = 7;
pub const AT_FLAGS: u32 = 8;
pub const AT_ENTRY: u32 = 9;
pub const AT_UID: u32 = 11;
pub const AT_EUID: u32 = 12;
pub const AT_GID: u32 = 13;
pub const AT_EGID: u32 = 14;
pub const AT_PLATFORM: u32 = 15;
pub const AT_HWCAP: u32 = 16;
pub const AT_CLKTCK: u32 = 17;
pub const AT_SECURE: u32 = 23;
pub const AT_RANDOM: u32 = 25;
pub const AT_EXECFN: u32 = 31;

impl Kernel {
    pub fn new(mem_size: usize, vfs: Vfs, host: Box<dyn Host>) -> Kernel {
        let mut sigactions = Vec::new();
        for _ in 0..65 {
            sigactions.push(SigAction { handler: 0, flags: 0, restorer: 0, mask: 0 });
        }
        let top = mem_size as u32;
        Kernel {
            mem: Memory::new(mem_size),
            cache: BlockCache::new(),
            threads: Vec::new(),
            cur: 0,
            next_tid: 100,
            vfs,
            fds: Vec::new(),
            pipes: Vec::new(),
            host,
            regions: BTreeMap::new(),
            mmap_top: top - STACK_SIZE - 0x10_0000,
            brk_start: 0,
            brk_cur: 0,
            brk_limit: 0,
            stack_lo: top - STACK_SIZE,
            stack_hi: top,
            exe_path: String::new(),
            generation: 1,
            sigactions,
            exit_code: None,
            trace_syscalls: false,
            sig_stats: [0; 32],
            sys_counts: [0; 512],
            tc: TimeCache::default(),
            eip_prof_on: false,
            eip_prof: std::collections::HashMap::new(),
            unimpl_log: Vec::new(),
            futex_waiters: VecDeque::new(),
            pid: 42,
            hyper_pending: None,
            cpuid_edx: (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26),
        }
    }

    // ---- launch ----------------------------------------------------------------------------
    pub fn spawn(&mut self, path: &str, argv: &[String], envp: &[String]) -> Result<(), String> {
        let ino = self.vfs.lookup(path, true).map_err(|e| format!("{}: errno {}", path, e))?;
        let img = match self.vfs.get(ino).map(|i| i.kind.clone()) {
            Some(crate::vfs::Kind::File(d)) => d,
            _ => return Err(format!("{}: not a file", path)),
        };
        let info = elf::parse(&img)?;
        self.exe_path = self.vfs.normalize(path);
        self.crate_stdio();

        // Executable placement: ET_EXEC at its own addresses, ET_DYN at a fixed base.
        let exe_base = if info.e_type == 3 { 0x0100_0000 } else { 0 };
        let exe = elf::load(&mut self.mem, &img, &info, exe_base)?;
        self.regions.insert(exe_base & !0xfff, (exe.end - (exe_base & !0xfff), self.exe_path.clone()));
        // brk heap right after the executable, 64 MiB reserved.
        self.brk_start = exe.end;
        self.brk_cur = exe.end;
        self.brk_limit = exe.end + (256 << 20);
        self.regions.insert(self.brk_start, (self.brk_limit - self.brk_start, "[heap]".into()));

        // Stack.
        self.mem.set_prot(self.stack_lo, STACK_SIZE, P_MAPPED | P_R | P_W);
        self.regions.insert(self.stack_lo, (STACK_SIZE, "[stack]".into()));

        // Interpreter.
        let mut entry = exe.entry;
        let mut interp_base = 0u32;
        if let Some(ip) = &info.interp {
            let iino = self.vfs.lookup(ip, true).map_err(|e| format!("interp {}: errno {}", ip, e))?;
            let iimg = match self.vfs.get(iino).map(|i| i.kind.clone()) {
                Some(crate::vfs::Kind::File(d)) => d,
                _ => return Err(format!("interp {} not a file", ip)),
            };
            let iinfo = elf::parse(&iimg)?;
            let (lo, hi) = elf::span(&iinfo);
            let len = hi - lo;
            let base = self.alloc_region(len, ip).ok_or("no space for interpreter")?;
            let loaded = elf::load(&mut self.mem, &iimg, &iinfo, base.wrapping_sub(lo))?;
            interp_base = loaded.base;
            entry = loaded.entry;
        }

        // Initial stack: strings, auxv, envp, argv, argc.
        let mut sp = self.stack_hi - 16;
        let push_str = |k: &mut Kernel, s: &str, sp: &mut u32| -> u32 {
            let b = s.as_bytes();
            *sp -= b.len() as u32 + 1;
            let _ = k.mem.write_bytes(*sp, b);
            let _ = k.mem.w8(*sp + b.len() as u32, 0);
            *sp
        };
        let execfn = push_str(self, &self.exe_path.clone(), &mut sp);
        let mut env_ptrs = Vec::new();
        for e in envp.iter().rev() {
            env_ptrs.push(push_str(self, e, &mut sp));
        }
        env_ptrs.reverse();
        let mut arg_ptrs = Vec::new();
        for a in argv.iter().rev() {
            arg_ptrs.push(push_str(self, a, &mut sp));
        }
        arg_ptrs.reverse();
        let platform = push_str(self, "i686", &mut sp);
        sp -= 16;
        let random = sp;
        let mut rnd = [0u8; 16];
        self.host.random(&mut rnd);
        let _ = self.mem.write_bytes(random, &rnd);
        sp &= !0xf;

        let hwcap = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26);
        let auxv: Vec<(u32, u32)> = vec![
            (AT_PHDR, exe.phdr_addr),
            (AT_PHENT, info.phentsize as u32),
            (AT_PHNUM, info.phnum as u32),
            (AT_PAGESZ, 4096),
            (AT_BASE, interp_base),
            (AT_FLAGS, 0),
            (AT_ENTRY, exe.entry),
            (AT_UID, 1000),
            (AT_EUID, 1000),
            (AT_GID, 1000),
            (AT_EGID, 1000),
            (AT_PLATFORM, platform),
            (AT_HWCAP, hwcap),
            (AT_CLKTCK, 100),
            (AT_SECURE, 0),
            (AT_RANDOM, random),
            (AT_EXECFN, execfn),
            (AT_NULL, 0),
        ];
        let words = 1 + arg_ptrs.len() + 1 + env_ptrs.len() + 1 + auxv.len() * 2;
        sp -= (words as u32) * 4;
        sp &= !0xf;
        let mut p = sp;
        let w = |k: &mut Kernel, v: u32, p: &mut u32| {
            let _ = k.mem.w32(*p, v);
            *p += 4;
        };
        w(self, arg_ptrs.len() as u32, &mut p);
        for a in &arg_ptrs {
            w(self, *a, &mut p);
        }
        w(self, 0, &mut p);
        for e in &env_ptrs {
            w(self, *e, &mut p);
        }
        w(self, 0, &mut p);
        for (k, v) in &auxv {
            w(self, *k, &mut p);
            w(self, *v, &mut p);
        }

        let mut cpu = Cpu::new();
        cpu.cpuid_edx = self.cpuid_edx;
        cpu.eip = entry;
        cpu.r[ESP] = sp;
        let tid = self.pid;
        self.threads.push(Thread {
            tid,
            cpu,
            wait: Wait::None,
            deadline: None,
            wake_val: 0,
            clear_child_tid: 0,
            robust_list: 0,
            zombie: false,
            ins: 0,
            retry_deadline: None,
            sigmask: 0,
            pending: 0,
            sig_info: [(0, 0, 0); 65],
            saved_mask: None,
            fp_saves: Vec::new(),
        });
        Ok(())
    }

    /// Find a free page-aligned region below `mmap_top`, top-down, and record it.
    pub fn alloc_region(&mut self, len: u32, label: &str) -> Option<u32> {
        let len = (len + 0xfff) & !0xfff;
        let mut hi = self.mmap_top;
        // Walk existing regions from the top; the first gap that fits wins.
        for (&s, &(l, _)) in self.regions.iter().rev() {
            let e = s + l;
            if s >= hi {
                continue; // entirely above the search window (stack/heap/exe)
            }
            if e > hi {
                hi = s; // straddles the window top (e.g. an earlier MAP_FIXED): it blocks everything above s
                continue;
            }
            if hi - e >= len {
                break;
            }
            hi = s;
        }
        let lo = hi.checked_sub(len)?;
        if lo < self.brk_limit {
            return None;
        }
        self.regions.insert(lo, (len, label.to_string()));
        Some(lo)
    }

    pub fn free_region(&mut self, addr: u32, len: u32) {
        let len = (len + 0xfff) & !0xfff;
        let end = addr + len;
        let overlapping: Vec<u32> = self.regions.range(..end).filter(|(&s, &(l, _))| s + l > addr).map(|(&s, _)| s).collect();
        for s in overlapping {
            let (l, name) = self.regions.remove(&s).unwrap();
            let e = s + l;
            if s < addr {
                self.regions.insert(s, (addr - s, name.clone()));
            }
            if e > end {
                self.regions.insert(end, (e - end, name));
            }
        }
        self.mem.set_prot(addr, len, 0);
        self.mem.zero(addr, len);
    }

    pub fn crate_stdio(&mut self) {
        if self.fds.is_empty() {
            for i in 0..3 {
                self.fds.push(Some(crate::sys::Fd::Console(i)));
            }
        }
    }

    // ---- guest clock ------------------------------------------------------------------------
    /// Monotonic nanoseconds for the guest (cheap: interpolated between host reads).
    pub fn mono_ns(&mut self) -> u64 {
        let ins = self.cache.jit.stats.ins_total;
        let tc = &mut self.tc;
        tc.calls += 1;
        if tc.valid && tc.calls < 16 && ins.wrapping_sub(tc.ins) < 20_000 {
            let t = (tc.mono + ((ins.wrapping_sub(tc.ins) * tc.k) >> 10)).max(tc.last);
            tc.last = t;
            return t;
        }
        self.refresh_clock(ins)
    }

    fn refresh_clock(&mut self, ins: u64) -> u64 {
        let now = self.host.now_ns();
        let tc = &mut self.tc;
        if tc.valid {
            let di = ins.wrapping_sub(tc.ins);
            let dt = now.saturating_sub(tc.mono);
            if di >= 2000 && dt < 2_000_000 {
                tc.k = (tc.k * 7 + ((dt << 10) / di).clamp(64, 8192)) / 8;
            }
        } else {
            tc.k = 512;
        }
        tc.mono = now.max(tc.last);
        tc.last = tc.mono;
        tc.ins = ins;
        tc.calls = 0;
        tc.valid = true;
        tc.mono
    }

    /// Unix-epoch nanoseconds for the guest (the monotonic clock plus the host's offset between the two, read once).
    pub fn real_ns(&mut self) -> u64 {
        if !self.tc.rt_valid {
            let m = self.host.now_ns();
            let r = self.host.realtime_ns();
            self.tc.rt_off = r.wrapping_sub(m);
            self.tc.rt_valid = true;
        }
        let m = self.mono_ns();
        m.wrapping_add(self.tc.rt_off)
    }

    // ---- scheduler -------------------------------------------------------------------------
    fn wake_timeouts(&mut self, now: u64) {
        for t in self.threads.iter_mut() {
            if t.zombie {
                continue;
            }
            if let Some(d) = t.deadline {
                if now >= d {
                    t.deadline = None;
                    match t.wait {
                        Wait::Futex { .. } => {
                            t.wake_val = -ETIMEDOUT;
                            t.wait = Wait::None;
                            t.cpu.r[EAX] = t.wake_val as u32;
                        }
                        Wait::Sleep => {
                            t.wake_val = 0;
                            t.wait = Wait::None;
                            t.cpu.r[EAX] = t.wake_val as u32;
                        }
                        // The syscall is re-executed (EAX already holds its number) and sees its deadline passed.
                        Wait::Retry { .. } => {
                            t.wait = Wait::None;
                        }
                        Wait::Host | Wait::None => {}
                    }
                }
            }
        }
    }

    pub fn bump(&mut self) {
        self.generation += 1;
    }

    fn thread_index(&self, tid: u32) -> Option<usize> {
        self.threads.iter().position(|t| t.tid == tid && !t.zombie)
    }

    /// Park the thread that issued a hypercall until `host_complete`.
    pub fn host_block(&mut self, tid: u32) {
        if let Some(i) = self.thread_index(tid) {
            self.threads[i].wait = Wait::Host;
        }
    }

    /// Finish a hypercall: put the result in EAX and make the thread runnable.
    pub fn host_complete(&mut self, tid: u32, ret: u32) {
        if let Some(i) = self.thread_index(tid) {
            let t = &mut self.threads[i];
            t.cpu.r[EAX] = ret;
            if t.wait == Wait::Host {
                t.wait = Wait::None;
            }
            self.generation += 1;
        }
    }

    pub fn thread_cpu(&mut self, tid: u32) -> Option<&mut Cpu> {
        let i = self.thread_index(tid)?;
        Some(&mut self.threads[i].cpu)
    }

    /// Wake up to `n` threads waiting on `addr` whose bitset intersects `bitset`.
    pub fn futex_wake(&mut self, addr: u32, n: u32, bitset: u32) -> u32 {
        let mut woken = 0;
        for t in self.threads.iter_mut() {
            if woken >= n {
                break;
            }
            if let Wait::Futex { addr: a, bitset: b } = t.wait {
                if a == addr && (b & bitset) != 0 {
                    t.wait = Wait::None;
                    t.deadline = None;
                    t.wake_val = 0;
                    t.cpu.r[EAX] = 0;
                    woken += 1;
                }
            }
        }
        woken
    }

    fn runnable(&self, i: usize) -> bool {
        let t = &self.threads[i];
        if t.zombie {
            return false;
        }
        match t.wait {
            Wait::None => true,
            Wait::Retry { gen } => self.generation != gen,
            _ => false,
        }
    }

    /// Run guest code for at most `slices` scheduling slices of `slice_insns` instructions each.
    pub fn run(&mut self, slice_insns: u64, slices: u32) -> Event {
        self.tc.valid = false; // wall time may have passed outside the guest (idle, hypercalls)
        for _ in 0..slices {
            if let Some(c) = self.exit_code {
                return Event::Exited(c);
            }
            if self.threads.iter().any(|t| t.deadline.is_some() && !t.zombie) {
                let now = self.mono_ns();
                self.wake_timeouts(now);
            }
            let n = self.threads.len();
            let mut pick = None;
            for k in 1..=n {
                let i = (self.cur + k) % n;
                if self.runnable(i) {
                    pick = Some(i);
                    break;
                }
            }
            let Some(i) = pick else {
                let dl = self.threads.iter().filter(|t| !t.zombie).filter_map(|t| t.deadline).min();
                if self.threads.iter().all(|t| t.zombie) {
                    return Event::Exited(self.exit_code.unwrap_or(0));
                }
                return Event::Idle(dl);
            };
            self.cur = i;
            if let Wait::Retry { .. } = self.threads[i].wait {
                // Resume a blocked syscall: rewind onto the `int 0x80` and restore its number.
                self.threads[i].wait = Wait::None;
            }
            if self.threads[i].pending & !self.threads[i].sigmask != 0 {
                self.deliver_pending(i);
            }
            let ins0 = self.cache.jit.stats.ins_total;
            let t0 = if self.eip_prof_on { self.host.now_ns() } else { 0 };
            let exit = {
                let t = &mut self.threads[i];
                t.cpu.run(&mut self.mem, &mut self.cache, slice_insns)
            };
            self.threads[i].ins += self.cache.jit.stats.ins_total - ins0;
            if self.eip_prof_on {
                // sampling profiler: where is the guest when its budget slice runs out (a uniform sample in executed instructions)
                // weighted by the wall time of the slice, so tiny slices (a `pause` spin loop hands over after every iteration) do not drown real work
                let dt = self.host.now_ns().saturating_sub(t0) / 1000;
                *self.eip_prof.entry(self.threads[i].cpu.eip >> 5).or_insert(0) += dt;
            }
            match exit {
                Exit::Probe { id, eip } => return Event::Probe { tid: self.threads[i].tid, id, eip },
                Exit::Budget => {}
                Exit::Syscall => {
                    if let Some(ev) = self.handle_syscall(i) {
                        return ev;
                    }
                }
                Exit::Hyper => {
                    let tid = self.threads[i].tid;
                    self.hyper_pending = Some(tid);
                    return Event::Hyper { tid };
                }
                Exit::Halt => {
                    return Event::Crash(self.crash_report(i, "hlt"));
                }
                other => {
                    // Try signal delivery for synchronous faults, otherwise crash.
                    if let Some(ev) = self.handle_fault(i, other) {
                        return ev;
                    }
                }
            }
        }
        Event::Yield
    }

    pub fn crash_report(&self, i: usize, why: &str) -> String {
        let c = &self.threads[i].cpu;
        format!(
            "{}\n  tid={} eip={:08x} eflags={:08x}\n  eax={:08x} ecx={:08x} edx={:08x} ebx={:08x}\n  esp={:08x} ebp={:08x} esi={:08x} edi={:08x}\n  fs_base={:08x} gs_base={:08x}",
            why, self.threads[i].tid, c.eip, c.fl, c.r[EAX], c.r[ECX], c.r[EDX], c.r[EBX], c.r[ESP], c.r[EBP], c.r[ESI], c.r[EDI], c.fs_base, c.gs_base
        )
    }

    fn handle_fault(&mut self, i: usize, e: Exit) -> Option<Event> {
        let (sig, what) = match &e {
            Exit::Segv(s) => (11, format!("SIGSEGV at {:#010x} ({})", s.addr, if s.write { "write" } else { "read" })),
            Exit::Undef { eip } => (4, format!("SIGILL at {:#010x}", eip)),
            Exit::Unimpl { eip, what } => (4, format!("UNIMPLEMENTED instruction {} at {:#010x}", what, eip)),
            Exit::DivideError { eip } => (8, format!("SIGFPE (divide error) at {:#010x}", eip)),
            Exit::Bounds { eip } => (11, format!("SIGSEGV (#BR bound) at {:#010x}", eip)),
            Exit::Breakpoint => (5, "SIGTRAP int3".to_string()),
            Exit::Int(n) => (11, format!("unsupported int {:#x}", n)),
            _ => (11, "fault".to_string()),
        };
        if self.sigactions[sig].handler > 1 && !matches!(e, Exit::Unimpl { .. }) {
            if self.deliver_signal(i, sig as u32, match &e { Exit::Segv(s) => s.addr, Exit::Bounds { eip } => *eip, _ => 0 }) {
                return None;
            }
        }
        Some(Event::Crash(self.crash_report(i, &what)))
    }
}
