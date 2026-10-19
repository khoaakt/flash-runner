//! Native driver: `emu-run [--rootfs rootfs.tar] [--trace] [--mem MB] -- <guest path> [args...]`
//! Runs a guest i386 Linux program to completion. Used by tests and for quick triage.

use emu::kernel::{Event, Host, Kernel};
use emu::vfs::Vfs;
use std::io::Write;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct NativeHost {
    start: Instant,
    seed: u64,
    capture: bool,
    out: Vec<u8>,
    err: Vec<u8>,
}

impl Host for NativeHost {
    fn now_ns(&mut self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
    fn realtime_ns(&mut self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
    }
    fn random(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            *b = self.seed as u8;
        }
    }
    fn console(&mut self, fd: u32, data: &[u8]) {
        if self.capture {
            if fd == 1 { self.out.extend_from_slice(data) } else { self.err.extend_from_slice(data) }
        } else if fd == 1 {
            let _ = std::io::stdout().write_all(data);
            let _ = std::io::stdout().flush();
        } else {
            let _ = std::io::stderr().write_all(data);
        }
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut rootfs: Option<String> = None;
    let mut trace = false;
    let mut mem_mb = 512usize;
    let mut insn_stats = false;
    let mut sse2 = false;
    let mut no_sse2 = false;
    loop {
        match args.first().map(|s| s.as_str()) {
            Some("--rootfs") => { args.remove(0); rootfs = Some(args.remove(0)); }
            Some("--trace") => { args.remove(0); trace = true; }
            Some("--mem") => { args.remove(0); mem_mb = args.remove(0).parse().unwrap(); }
            Some("--sse2") => { args.remove(0); sse2 = true; }
            Some("--no-sse2") => { args.remove(0); no_sse2 = true; }
            Some("--stats") => { args.remove(0); insn_stats = true; }
            Some("--") => { args.remove(0); break; }
            _ => break,
        }
    }
    if args.is_empty() {
        eprintln!("usage: emu-run [--rootfs tar] [--trace] [--mem MB] -- prog [args]");
        std::process::exit(2);
    }
    let mut vfs = Vfs::new();
    if let Some(r) = &rootfs {
        vfs.load_tar(&std::fs::read(r).expect("read rootfs"));
    }
    // Host files named on the command line are copied into /work (read-only convenience).
    let prog = args[0].clone();
    let guest_path = if prog.starts_with('/') && vfs.lookup(&prog, true).is_ok() {
        prog.clone()
    } else {
        let name = std::path::Path::new(&prog).file_name().unwrap().to_string_lossy().into_owned();
        let gp = format!("/work/{}", name);
        vfs.create_file(&gp, std::fs::read(&prog).expect("read program"), 0o755).unwrap();
        gp
    };
    // Shared objects next to the program are mapped into /work so tests can dlopen() them.
    if let Some(dir) = std::path::Path::new(&prog).parent() {
        if let Ok(rd) = std::fs::read_dir(if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { dir }) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.ends_with(".so") {
                    let _ = vfs.create_file(&format!("/work/{}", n), std::fs::read(e.path()).unwrap(), 0o755);
                }
            }
        }
    }
    let _ = vfs.mkdir_p("/work");
    vfs.cwd = "/work".into();
    let host = NativeHost { start: Instant::now(), seed: 0x9e3779b97f4a7c15, capture: false, out: vec![], err: vec![] };
    let mut k = Kernel::new(mem_mb << 20, vfs, Box::new(host));
    k.trace_syscalls = trace;
    let _ = sse2; // SSE2 is advertised by default now
    if no_sse2 { k.cpuid_edx &= !((1 << 23) | (1 << 24) | (1 << 25) | (1 << 26)); }
    let mut argv = vec![guest_path.clone()];
    argv.extend(args[1..].iter().cloned());
    let envp = vec!["PATH=/usr/bin:/bin".to_string(), "HOME=/home/flash".to_string(), "LANG=C".to_string()];
    if let Err(e) = k.spawn(&guest_path, &argv, &envp) {
        eprintln!("spawn failed: {}", e);
        std::process::exit(125);
    }
    let t0 = Instant::now();
    let code = loop {
        match k.run(200_000, 64) {
            Event::Exited(c) => break c,
            Event::Yield => {}
            Event::Probe { tid, id, eip } => { eprintln!("[probe] tid={tid} id={id} eip={eip:#x}"); }
            Event::Idle(Some(dl)) => {
                // nothing runnable: sleep the host until the nearest deadline
                let now = k.host.now_ns();
                if dl > now { std::thread::sleep(std::time::Duration::from_nanos((dl - now).min(50_000_000))); }
            }
            Event::Idle(None) => { eprintln!("[emu] deadlock: every thread blocked with no timeout"); break 124; }
            Event::Hyper { tid } => { eprintln!("[emu] unhandled hypercall from tid {}", tid); break 123; }
            Event::Crash(msg) => { eprintln!("[emu] CRASH: {}", msg); break 139; }
        }
    };
    if insn_stats {
        eprintln!("[emu] blocks decoded: {} elapsed {:?}", k.cache.decoded, t0.elapsed());
    }
    if !k.unimpl_log.is_empty() {
        let mut seen = std::collections::BTreeSet::new();
        for l in &k.unimpl_log { if seen.insert(l.clone()) { eprintln!("[emu] unimplemented {}", l); } }
    }
    std::process::exit(code);
}
