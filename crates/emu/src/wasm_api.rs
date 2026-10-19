//! C-ABI surface for the JavaScript embedder (wasm32 only).
//!
//! One global `Kernel`. JS owns scheduling: it calls `emu_run` in a loop and reacts to the returned
//! event code. Guest RAM is a plain region of this module's linear memory (`emu_guest_base`), so the
//! embedder can read/write guest memory directly through a typed-array view.

use crate::cpu::{Cpu, EAX, EBX, ECX, EDX, ESI, EDI, EBP};
use crate::kernel::{Event, Host, Kernel};
use crate::vfs::Vfs;

#[link(wasm_import_module = "env")]
extern "C" {
    fn host_now_ns() -> f64;
    fn host_realtime_ns() -> f64;
    fn host_random(ptr: *mut u8, len: usize);
    fn host_console(fd: u32, ptr: *const u8, len: usize);
}

struct WasmHost;
impl Host for WasmHost {
    fn now_ns(&mut self) -> u64 {
        unsafe { host_now_ns() as u64 }
    }
    fn realtime_ns(&mut self) -> u64 {
        unsafe { host_realtime_ns() as u64 }
    }
    fn random(&mut self, buf: &mut [u8]) {
        unsafe { host_random(buf.as_mut_ptr(), buf.len()) }
    }
    fn console(&mut self, fd: u32, data: &[u8]) {
        unsafe { host_console(fd, data.as_ptr(), data.len()) }
    }
}

struct State {
    vfs: Option<Vfs>,
    k: Option<Kernel>,
    ev_arg: f64,
    ev_msg: Vec<u8>,
}

static mut STATE: State = State { vfs: None, k: None, ev_arg: 0.0, ev_msg: Vec::new() };

#[allow(static_mut_refs)]
fn st() -> &'static mut State {
    unsafe { &mut STATE }
}

fn slice<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

#[no_mangle]
pub extern "C" fn emu_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[no_mangle]
pub extern "C" fn emu_free(ptr: *mut u8, len: usize) {
    unsafe { drop(Vec::from_raw_parts(ptr, 0, len.max(1))) }
}

/// Start a fresh VFS (call before `emu_vfs_load_tar` / `emu_vfs_add_file`).
#[no_mangle]
pub extern "C" fn emu_vfs_new() {
    let mut v = Vfs::new();
    let _ = v.mkdir_p("/work");
    v.cwd = "/work".into();
    st().vfs = Some(v);
}

#[no_mangle]
pub extern "C" fn emu_vfs_load_tar(ptr: *const u8, len: usize) {
    if let Some(v) = st().vfs.as_mut() {
        v.load_tar(slice(ptr, len));
    }
}

#[no_mangle]
pub extern "C" fn emu_vfs_add_file(path: *const u8, path_len: usize, data: *const u8, data_len: usize, mode: u32) {
    let p = String::from_utf8_lossy(slice(path, path_len)).into_owned();
    if let Some(v) = st().vfs.as_mut() {
        let _ = v.create_file(&p, slice(data, data_len).to_vec(), mode);
    }
}

/// Inline fast path for the pure time syscalls, called from compiled regions (see `F_SYS` in
/// jit_x86.rs): 0 = answered (eax set), 1 = the region must exit with `S_SYSCALL` as usual.
/// Native test backends do not have a kernel to ask and always return 1, which is exactly what
/// the interpreter path would do (exit, kernel answers), so both engines agree.
#[no_mangle]
pub extern "C" fn jit_sys_time(cpu: *mut Cpu) -> u32 {
    unsafe {
        let cpu = &mut *cpu;
        let nr = cpu.r[EAX];
        let a = [cpu.r[EBX], cpu.r[ECX], cpu.r[EDX], cpu.r[ESI], cpu.r[EDI], cpu.r[EBP]];
        let Some(k) = st().k.as_mut() else { return 1 };
        match k.time_syscall(nr, a) {
            Some((v, code_page)) => {
                cpu.r[EAX] = v;
                if code_page {
                    2 // answered, but the timespec landed on a code page: S_SYNC resynchronises
                } else {
                    0
                }
            }
            None => 1,
        }
    }
}

/// Create the machine with `mem_mb` MiB of guest RAM. Consumes the VFS built so far.
#[no_mangle]
pub extern "C" fn emu_boot(mem_mb: u32, cpuid_edx_extra: u32) {
    let vfs = st().vfs.take().unwrap_or_else(Vfs::new);
    let mut k = Kernel::new((mem_mb as usize) << 20, vfs, Box::new(WasmHost));
    k.cpuid_edx |= cpuid_edx_extra & 0x7fff_ffff;
    if cpuid_edx_extra & 0x8000_0000 != 0 {
        k.cpuid_edx &= !((1 << 23) | (1 << 24) | (1 << 25) | (1 << 26)); // debugging: hide MMX/SSE from the guest
    }
    st().k = Some(k);
}

fn split_nul(b: &[u8]) -> Vec<String> {
    b.split(|&c| c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect()
}

/// argv/envp are NUL-separated blobs. Returns 0 on success; on failure the message is in the event buffer.
#[no_mangle]
pub extern "C" fn emu_spawn(path: *const u8, path_len: usize, argv: *const u8, argv_len: usize, envp: *const u8, envp_len: usize) -> u32 {
    let p = String::from_utf8_lossy(slice(path, path_len)).into_owned();
    let args = split_nul(slice(argv, argv_len));
    let env = split_nul(slice(envp, envp_len));
    let s = st();
    match s.k.as_mut().unwrap().spawn(&p, &args, &env) {
        Ok(()) => 0,
        Err(e) => {
            s.ev_msg = e.into_bytes();
            1
        }
    }
}

/// JIT tier-up threshold: 0 = off (pure interpreter), 1 = translate every block on first sight, N = after N executions.
#[no_mangle]
pub extern "C" fn emu_set_jit(threshold: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.threshold = threshold;
    }
}

/// Host capabilities for generated code: wasm tail calls (region chaining) and SIMD (inline SSE2 integer ops).
/// Call before `emu_set_jit`; both default to enabled.
#[no_mangle]
pub extern "C" fn emu_set_jit_features(tail_calls: u32, simd: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.use_chain = tail_calls != 0;
        k.cache.jit.use_simd = simd != 0;
    }
}

/// Fast paths for recognised hot runtime functions (jit_intr); on by default. Call before regions are compiled.
#[no_mangle]
pub extern "C" fn emu_set_jit_intrinsics(on: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.intrinsics = on != 0;
    }
}

/// Performance mode: guest loads skip the page check (docs/JIT.md). Off by default; call before regions are compiled.
#[no_mangle]
pub extern "C" fn emu_set_jit_trust_loads(on: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.trust_loads = on != 0;
    }
}

/// Per-chain instruction counters for `emu_jit_hot_report` (must be set before regions are compiled).
#[no_mangle]
pub extern "C" fn emu_set_jit_prof_chains(on: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.prof_chains = on != 0;
    }
}

/// Text report of the hottest compiled chains into the event message buffer; returns its length.
#[no_mangle]
pub extern "C" fn emu_jit_hot_report(n: u32) -> usize {
    let s = st();
    let r = s.k.as_ref().map(|k| k.cache.hot_report(n as usize)).unwrap_or_default();
    s.ev_msg = r.into_bytes();
    s.ev_msg.len()
}

/// Region selection tuning (default 4; 0 = put every already-decoded successor block into the region).
#[no_mangle]
pub extern "C" fn emu_set_jit_region_hot_div(div: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.region_hot_div = div;
    }
}

/// Copy small already-compiled callees into the calling region (0 = off; see `JitState::dup_ins`).
#[no_mangle]
pub extern "C" fn emu_set_jit_dup(n: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.dup_ins = n;
    }
}

/// Collect dispatcher phase timings (see `JitState::profile`).
#[no_mangle]
pub extern "C" fn emu_set_jit_profile(on: u32) {
    if let Some(k) = st().k.as_mut() {
        k.cache.jit.profile = on != 0;
    }
}

/// Compile a normalized, effect-free LIR trace; no Flash memory/call ABI is inferred.
/// Positive length = wasm bytes in the event buffer; -1 = UTF-8 diagnostic in that buffer.
#[no_mangle]
pub extern "C" fn emu_compile_lir(ptr: *const u8, len: usize) -> i32 {
    let result = if len > 1024 * 1024 {
        Err("LIR source exceeds the 1 MiB API limit".to_string())
    } else {
        std::str::from_utf8(slice(ptr, len)).map_err(|e| e.to_string())
            .and_then(|text| crate::lir::Trace::parse(text).map_err(|e| e.to_string()))
            .map(|trace| trace.compile())
    };
    match result {
        Ok(bytes) => {
            st().ev_msg = bytes;
            st().ev_msg.len() as i32
        }
        Err(e) => { st().ev_msg = e.into_bytes(); -1 }
    }
}

/// Addresses for compiling a guarded offline artifact for this particular booted instance.
#[no_mangle]
pub extern "C" fn emu_aot_layout() -> usize {
    let s = st();
    let r = s.k.as_ref().map(|k| {
        let l = k.cache.jit.aot_layout(&k.mem);
        format!("{{\"abi_version\":1,\"guest_base\":{},\"rtab\":{},\"wtab\":{},\"cache\":0,\"simd\":false,\"guest_size\":{}}}",
            l.guest_base, l.rtab, l.wtab, k.mem.size())
    }).unwrap_or_else(|| "null".into());
    s.ev_msg = r.into_bytes();
    s.ev_msg.len()
}

/// Install a locally trusted offline bundle between scheduler calls. A guard mismatch changes no heads.
/// Returns installed region count, or -1 with the diagnostic in the event buffer.
#[no_mangle]
pub extern "C" fn emu_install_aot(ptr: *const u8, len: usize) -> i32 {
    let result = if len > 80 * 1024 * 1024 {
        Err("AOT bundle exceeds the 80 MiB API limit".into())
    } else {
        crate::aot::Artifact::decode(slice(ptr, len)).and_then(|artifact| {
            let k = st().k.as_mut().ok_or("emulator is not booted")?;
            artifact.install(&mut k.mem, &mut k.cache.jit)
        })
    };
    match result {
        Ok(n) => { st().ev_msg.clear(); n as i32 }
        Err(e) => { st().ev_msg = e.into_bytes(); -1 }
    }
}

/// Bind symbolic operands in offline wasm to this instance and plugin load bias, then install.
/// No guest x86 lowering is performed here. Only locally trusted portable bundles are supported.
#[no_mangle]
pub extern "C" fn emu_install_portable_aot(ptr: *const u8, len: usize, load_bias: u32) -> i32 {
    let result = if len > 80 * 1024 * 1024 { Err("portable AOT exceeds 80 MiB".into()) } else {
        crate::aot::DecodedArtifact::decode(slice(ptr, len)).and_then(|artifact| {
            let k = st().k.as_mut().ok_or("emulator is not booted")?;
            let layout = if artifact.layout().cache != 0 { k.cache.jit.lir_layout(&k.mem) } else { k.cache.jit.aot_layout(&k.mem) };
            if artifact.layout().cache != 0 && layout.cache == 0 { return Err("this portable artifact requires wasm tail-call support".into()); }
            let instructions = artifact.decode_fallback_instructions(load_bias)?;
            let tags = k.cache.jit.allocate_offline_tags(&instructions);
            let result = (|| {
                let bound = artifact.bind_with_tags(&layout, load_bias, &tags)?;
                let regions: Vec<_> = bound.outputs.iter().cloned().zip(&bound.regions)
                    .map(|(out, region)| (out, vec![(region.address, region.bytes.clone())])).collect();
                let probes: Vec<_> = k.cache.entry_probes.keys().copied().collect();
                if tags.is_empty() { k.cache.jit.install_precompiled_excluding(&mut k.mem,&layout,&bound.wasm,&regions,&probes) }
                else { k.cache.jit.install_precompiled_fallbacks(&mut k.mem,&layout,&bound.wasm,&regions,&probes,&tags) }
            })();
            if result.as_ref().map_or(true, |&n|n==0) { k.cache.jit.release_offline_tags(&tags); }
            result
        })
    };
    match result {
        Ok(n) => { st().ev_msg.clear(); n as i32 }
        Err(e) => { st().ev_msg = e.into_bytes(); -1 }
    }
}

/// The verified Flash capture frontend may submit the narrowly audited pure SUB leaf.
#[no_mangle]
pub extern "C" fn emu_install_lir_sub_leaf(entry: u32, core: u32, ptr: *const u8, len: usize) -> i32 {
    let result = if len != 78 { Err("invalid scalar native binding length".into()) } else {
        let k = st().k.as_mut().ok_or_else(|| "emulator is not booted".to_string());
        k.and_then(|k| crate::lir::flash_leaf::install_sub_leaf(&mut k.mem, &mut k.cache.jit, entry, core, slice(ptr, len)))
    };
    match result {
        Ok(n) => { st().ev_msg.clear(); n as i32 }
        Err(e) => { st().ev_msg = e.into_bytes(); -1 }
    }
}

/// Text report of JIT statistics into the event message buffer; returns its length.
#[no_mangle]
pub extern "C" fn emu_install_lir_integer_leaf(entry: u32, core: u32, native: *const u8, n: usize,
    source: *const u8, source_len: usize, offsets: *const u8, count: usize, argc: u32) -> i32 {
    let result = (|| {
        if n == 0 || n > 4096 || source_len > 65536 || count > 16 { return Err("captured leaf size limit".to_string()); }
        let text = std::str::from_utf8(slice(source, source_len)).map_err(|e| e.to_string())?;
        let locations: Vec<_> = slice(offsets, count * 4).chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        let k = st().k.as_mut().ok_or("emulator not booted")?;
        crate::lir::pure::install(&mut k.mem, &mut k.cache.jit, entry, core, slice(native, n), text, &locations, argc)
    })();
    match result { Ok(n) => { st().ev_msg.clear(); n as i32 }, Err(e) => { st().ev_msg = e.into_bytes(); -1 } }
}

#[no_mangle]
pub extern "C" fn emu_install_lir_counted_loop(entry: u32, core: u32, native: *const u8, n: usize, metadata: *const u8, metadata_len: usize) -> i32 {
    let result = (|| {
        if n == 0 || n > 4096 || metadata_len > 16384 { return Err("captured loop size limit".to_string()); }
        let k = st().k.as_mut().ok_or("emulator not booted")?;
        crate::lir::counted::install(&mut k.mem, &mut k.cache.jit, entry, core, slice(native, n), slice(metadata, metadata_len))
    })();
    match result { Ok(n) => { st().ev_msg.clear(); n as i32 }, Err(e) => { st().ev_msg = e.into_bytes(); -1 } }
}

/// Verify and install a bounded receiver-field trace with transactional memory effects.
#[no_mangle]
pub extern "C" fn emu_install_lir_memory(entry: u32, core: u32, native: *const u8, native_len: usize,
    source: *const u8, source_len: usize, bindings: *const u8, binding_count: usize,
    fields: *const u8, field_count: usize, stores: *const u8, store_count: usize, argc: u32) -> i32 {
    let result = (|| {
        if native_len == 0 || native_len > 4096 || source_len > 65536 || binding_count > 16
            || field_count > 16 || store_count > 8 {
            return Err("captured receiver-memory size limit".to_string());
        }
        let text = std::str::from_utf8(slice(source,source_len)).map_err(|e|e.to_string())?;
        let words = |p,n| slice(p,n*4).chunks_exact(4).map(|b|u32::from_le_bytes(b.try_into().unwrap())).collect::<Vec<_>>();
        let (arguments, accesses, effects) = (words(bindings,binding_count),words(fields,field_count),words(stores,store_count*2));
        let effects: Vec<_> = effects.chunks_exact(2).map(|pair| (pair[0], pair[1] as usize)).collect();
        let k = st().k.as_mut().ok_or("emulator not booted")?;
        let semantic = crate::lir::Trace::parse(text).map_err(|e|e.to_string())?;
        crate::lir::pure::install_memory_effects(&mut k.mem,&mut k.cache.jit,entry,core,slice(native,native_len),
            &semantic,&arguments,&accesses,&effects,argc)
    })();
    match result {Ok(n)=>{st().ev_msg.clear();n as i32},Err(e)=>{st().ev_msg=e.into_bytes();-1}}
}

/// Verify and install one captured Number leaf. Native x87/SSE effects are
/// proved independently; the frontend's opcode names are not trusted.
#[no_mangle]
pub extern "C" fn emu_install_lir_number(entry: u32, core: u32, native: *const u8, native_len: usize,
    source: *const u8, source_len: usize, offsets: *const u8, offset_count: usize, argc: u32) -> i32 {
    let result = (|| {
        if native_len == 0 || native_len > 4096 || source_len > 8192 || offset_count == 0 || offset_count > 8 {
            return Err("captured Number size limit".to_string());
        }
        let text = std::str::from_utf8(slice(source, source_len)).map_err(|e| e.to_string())?;
        let locations: Vec<_> = slice(offsets, offset_count * 4).chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        let k = st().k.as_mut().ok_or("emulator not booted")?;
        crate::lir::number::install(&mut k.mem, &mut k.cache.jit, entry, core, slice(native, native_len), text, &locations, argc)
    })();
    match result { Ok(n) => { st().ev_msg.clear(); n as i32 }, Err(e) => { st().ev_msg = e.into_bytes(); -1 } }
}

/// Text report of JIT statistics into the event message buffer; returns its length.
#[no_mangle]
pub extern "C" fn emu_jit_report() -> usize {
    let s = st();
    let r = s.k.as_ref().map(|k| {
        let mut r = k.cache.jit.report();
        r += &format!("\n[lir] completed_native_leaf_calls={}", k.threads.iter().fold(0u64, |n, t| n.saturating_add(t.cpu.jit_lir_completed)));
        r += &format!("\n[sig] guest signals raised: SIGSEGV={} SIGBUS={} SIGFPE={} SIGILL={} SIGABRT={} other={}", k.sig_stats[11], k.sig_stats[7], k.sig_stats[8], k.sig_stats[4], k.sig_stats[6], k.sig_stats.iter().sum::<u32>() - k.sig_stats[11] - k.sig_stats[7] - k.sig_stats[8] - k.sig_stats[4] - k.sig_stats[6]);
        { let name = |n: usize| match n { 3 => "read", 4 => "write", 5 => "open", 6 => "close", 13 => "time", 54 => "ioctl", 78 => "gettimeofday", 91 => "munmap", 90 => "mmap", 192 => "mmap2", 125 => "mprotect", 142 => "_newselect", 158 => "sched_yield", 162 => "nanosleep", 168 => "poll", 175 => "rt_sigprocmask", 238 => "tkill", 240 => "futex", 252 => "exit_group", 265 => "clock_gettime", 266 => "clock_getres", 270 => "tgkill", 311 => "set_robust_list", 331 => "pipe2", 295 => "openat", 102 => "socketcall", _ => "" };
          let mut v: Vec<(usize, u64)> = k.sys_counts.iter().copied().enumerate().filter(|&(_, c)| c > 0).collect(); v.sort_by(|a, b| b.1.cmp(&a.1));
          r += &format!("\n[sys] syscalls: {}", v.iter().take(10).map(|&(n, c)| format!("{}({})={}", n, name(n), c)).collect::<Vec<_>>().join(" ")); }
        r += &format!("\n[jit] guest instructions per thread (M): {}", k.threads.iter().filter(|t| t.ins > 0).map(|t| format!("tid{}={}", t.tid, t.ins / 1_000_000)).collect::<Vec<_>>().join(" "));
        r
    }).unwrap_or_default();
    s.ev_msg = r.into_bytes();
    s.ev_msg.len()
}

/// Guest sampling profiler: 1 = on (clears the histogram), 0 = off. `emu_eip_prof_report(n)` writes the n hottest 32-byte buckets
/// ("addr count" lines, most samples first) into the event message buffer and returns its length.
#[no_mangle]
pub extern "C" fn emu_set_eip_prof(on: u32) {
    if let Some(k) = st().k.as_mut() {
        k.eip_prof_on = on != 0;
        if on != 0 {
            k.eip_prof.clear();
        }
    }
}

#[no_mangle]
pub extern "C" fn emu_eip_prof_report(n: u32) -> usize {
    let s = st();
    let mut out = String::new();
    if let Some(k) = s.k.as_ref() {
        let mut v: Vec<(&u32, &u64)> = k.eip_prof.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = k.eip_prof.values().sum();
        out += &format!("total_us {}\n", total);
        for (a, c) in v.into_iter().take(n as usize) {
            out += &format!("{:08x} {}\n", a << 5, c);
        }
    }
    s.ev_msg = out.into_bytes();
    s.ev_msg.len()
}

#[no_mangle]
pub extern "C" fn emu_set_trace(on: u32) {
    if let Some(k) = st().k.as_mut() {
        k.trace_syscalls = on != 0;
    }
}

/// Event codes: 0 yield, 1 exited (arg = code), 2 idle (arg = wake time in ns, -1 = none),
/// 3 hypercall (arg = tid), 4 crash, 5 guarded entry probe (JSON message; arg = id).
#[no_mangle]
pub extern "C" fn emu_run(slice_insns: u32, slices: u32) -> u32 {
    let s = st();
    let ev = s.k.as_mut().unwrap().run(slice_insns as u64, slices);
    match ev {
        Event::Probe { tid, id, eip } => {
            s.ev_arg = id as f64;
            s.ev_msg = format!("{{\"tid\":{tid},\"id\":{id},\"eip\":{eip}}}").into_bytes();
            5
        }
        Event::Yield => 0,
        Event::Exited(c) => {
            s.ev_arg = c as f64;
            1
        }
        Event::Idle(d) => {
            s.ev_arg = d.map(|x| x as f64).unwrap_or(-1.0);
            2
        }
        Event::Hyper { tid } => {
            s.ev_arg = tid as f64;
            3
        }
        Event::Crash(m) => {
            s.ev_msg = m.into_bytes();
            4
        }
    }
}

#[no_mangle]
pub extern "C" fn emu_ev_arg() -> f64 {
    st().ev_arg
}

#[no_mangle]
pub extern "C" fn emu_lir_completed() -> u64 {
    st().k.as_ref().map_or(0, |k| k.threads.iter().fold(0u64, |n, t| n.saturating_add(t.cpu.jit_lir_completed)))
}

#[no_mangle]
pub extern "C" fn emu_guest_readable(address: u32, len: u32) -> u32 {
    let Some(k) = st().k.as_ref() else { return 0 };
    let Some(end) = address.checked_add(len) else { return 0 };
    if end as usize > k.mem.size() { return 0; }
    if len == 0 { return 1; }
    u32::from(((address >> 12)..=((end - 1) >> 12)).all(|pg| k.mem.page_prot(pg << 12) & (crate::mem::P_R | crate::mem::P_MAPPED) == (crate::mem::P_R | crate::mem::P_MAPPED)))
}

/// Install an exact-byte-bound non-mutating entry probe. Setting/removing probes flushes
/// compiled/decoded boundaries; call before loading acceleration bundles, not in hot loops.
#[no_mangle]
pub extern "C" fn emu_set_entry_probe(eip: u32, id: u32, expected: *const u8, len: usize) -> i32 {
    if len > 256 || (id != 0 && len == 0) { return -1; }
    let k = match st().k.as_mut() { Some(k) => k, None => return -1 };
    if !k.cache.set_entry_probe(&mut k.mem, eip, id, slice(expected, len)) { return -1; }
    for thread in &mut k.threads { thread.cpu.jit_probe_resume = 0; }
    0
}
#[no_mangle]
pub extern "C" fn emu_ev_msg_ptr() -> *const u8 {
    st().ev_msg.as_ptr()
}
#[no_mangle]
pub extern "C" fn emu_ev_msg_len() -> usize {
    st().ev_msg.len()
}

/// Start of guest RAM inside the wasm linear memory (guest address 0).
#[no_mangle]
pub extern "C" fn emu_guest_base() -> *const u8 {
    st().k.as_ref().unwrap().mem.raw().as_ptr()
}
#[no_mangle]
pub extern "C" fn emu_guest_size() -> u32 {
    st().k.as_ref().unwrap().mem.size() as u32
}

/// Registers of a thread (index: 0 eax 1 ecx 2 edx 3 ebx 4 esp 5 ebp 6 esi 7 edi 8 eip).
#[no_mangle]
pub extern "C" fn emu_reg_get(tid: u32, idx: u32) -> u32 {
    match st().k.as_mut().unwrap().thread_cpu(tid) {
        Some(c) => if idx < 8 { c.r[idx as usize] } else { c.eip },
        None => 0,
    }
}
#[no_mangle]
pub extern "C" fn emu_reg_set(tid: u32, idx: u32, v: u32) {
    if let Some(c) = st().k.as_mut().unwrap().thread_cpu(tid) {
        if idx < 8 { c.r[idx as usize] = v } else { c.eip = v }
    }
}
#[no_mangle]
pub extern "C" fn emu_host_block(tid: u32) {
    st().k.as_mut().unwrap().host_block(tid);
}
#[no_mangle]
pub extern "C" fn emu_host_complete(tid: u32, ret: u32) {
    st().k.as_mut().unwrap().host_complete(tid, ret);
}
/// Wake the scheduler's bookkeeping after external input (pipes written by JS, etc.).
#[no_mangle]
pub extern "C" fn emu_bump() {
    st().k.as_mut().unwrap().bump();
}

// ---- filesystem access for the embedder ------------------------------------------------------
fn kstr(ptr: *const u8, len: usize) -> String {
    String::from_utf8_lossy(slice(ptr, len)).into_owned()
}

/// Open a guest path and return a descriptor inside the guest process (or -errno).
#[no_mangle]
pub extern "C" fn emu_open_fd(path: *const u8, path_len: usize, flags: u32, mode: u32) -> i32 {
    st().k.as_mut().unwrap().host_open(&kstr(path, path_len), flags, mode)
}

/// Stat: returns 0 and fills out[0..4] = mode, size_lo, size_hi, mtime; or -errno.
#[no_mangle]
pub extern "C" fn emu_stat(path: *const u8, path_len: usize, out: *mut u32) -> i32 {
    match st().k.as_ref().unwrap().host_stat(&kstr(path, path_len)) {
        Ok((mode, size, mtime)) => unsafe {
            *out = mode;
            *out.add(1) = size as u32;
            *out.add(2) = (size >> 32) as u32;
            *out.add(3) = mtime as u32;
            0
        },
        Err(e) => -e,
    }
}

/// Directory listing as "name\0D|F\0..." into the event message buffer; returns entry count or -errno.
#[no_mangle]
pub extern "C" fn emu_readdir(path: *const u8, path_len: usize) -> i32 {
    let s = st();
    match s.k.as_ref().unwrap().host_readdir(&kstr(path, path_len)) {
        Ok(v) => {
            let mut buf = Vec::new();
            for (n, d) in &v {
                buf.extend_from_slice(n.as_bytes());
                buf.push(0);
                buf.push(if *d { b'D' } else { b'F' });
                buf.push(0);
            }
            s.ev_msg = buf;
            v.len() as i32
        }
        Err(e) => -e,
    }
}

#[no_mangle]
pub extern "C" fn emu_unlink(path: *const u8, path_len: usize, recursive: u32) -> i32 {
    match st().k.as_mut().unwrap().host_unlink(&kstr(path, path_len), recursive != 0) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

#[no_mangle]
pub extern "C" fn emu_rename(from: *const u8, from_len: usize, to: *const u8, to_len: usize) -> i32 {
    match st().k.as_mut().unwrap().vfs.rename(&kstr(from, from_len), &kstr(to, to_len)) {
        Ok(()) => 0,
        Err(e) => -e,
    }
}

#[no_mangle]
pub extern "C" fn emu_mkdir_p(path: *const u8, path_len: usize) -> i32 {
    match st().k.as_mut().unwrap().vfs.mkdir_p(&kstr(path, path_len)) {
        Ok(_) => 0,
        Err(e) => -e,
    }
}

/// Write a file into the *running* guest VFS (e.g. restoring persisted storage after boot).
#[no_mangle]
pub extern "C" fn emu_write_file(path: *const u8, path_len: usize, data: *const u8, data_len: usize) -> i32 {
    let k = st().k.as_mut().unwrap();
    match k.vfs.create_file(&kstr(path, path_len), slice(data, data_len).to_vec(), 0o644) {
        Ok(_) => 0,
        Err(e) => -e,
    }
}

/// Dump all regular files below `prefix` into the event message buffer as
/// repeated [u32 path_len][path][u32 data_len][data]; returns the file count.
#[no_mangle]
pub extern "C" fn emu_dump_tree(prefix: *const u8, prefix_len: usize) -> i32 {
    let s = st();
    let files = s.k.as_ref().unwrap().host_dump_tree(&kstr(prefix, prefix_len));
    let mut buf = Vec::new();
    for (p, d) in &files {
        buf.extend_from_slice(&(p.len() as u32).to_le_bytes());
        buf.extend_from_slice(p.as_bytes());
        buf.extend_from_slice(&(d.len() as u32).to_le_bytes());
        buf.extend_from_slice(d);
    }
    s.ev_msg = buf;
    files.len() as i32
}
