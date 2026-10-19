//! Linux/i386 system calls (the subset glibc, libstdc++ and the Flash plugin actually use).

use crate::cpu::*;
use crate::kernel::*;
use crate::mem::*;
use crate::vfs::*;
use std::collections::VecDeque;

pub enum Fd {
    Console(u32),
    File { ino: u32, off: u64, flags: u32 },
    Dir { ino: u32, pos: usize },
    Dev { major: u32, minor: u32, flags: u32 },
    /// Synthesized read-only file (/proc/...)
    Synth { data: Vec<u8>, off: usize },
    PipeR(usize),
    PipeW(usize),
    Sock { rx: usize, tx: usize },
}

pub struct Pipe {
    pub buf: VecDeque<u8>,
    pub readers: u32,
    pub writers: u32,
}

pub enum SysRes {
    Ret(i32),
    /// Syscall re-executes when the kernel generation changes.
    Retry,
    /// Thread state already set (futex/sleep); value arrives via wake_val.
    Blocked,
    ExitThread,
    ExitProcess(i32),
    /// Registers were fully replaced (sigreturn); do not write EAX.
    NoRet,
}

use SysRes::*;

fn neg(e: i32) -> SysRes {
    Ret(-e)
}

const O_CREAT: u32 = 0x40;
const O_EXCL: u32 = 0x80;
const O_TRUNC: u32 = 0x200;
const O_APPEND: u32 = 0x400;
const O_DIRECTORY: u32 = 0x10000;

const CLONE_VM: u32 = 0x100;
const CLONE_THREAD: u32 = 0x10000;
const CLONE_SETTLS: u32 = 0x80000;
const CLONE_PARENT_SETTID: u32 = 0x100000;
const CLONE_CHILD_CLEARTID: u32 = 0x200000;
const CLONE_CHILD_SETTID: u32 = 0x1000000;

macro_rules! mf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(_) => return Ret(-EFAULT),
        }
    };
}

impl Kernel {
    fn arg(&self, i: usize, n: usize) -> u32 {
        // i386 syscall args: ebx ecx edx esi edi ebp
        const IDX: [usize; 6] = [EBX, ECX, EDX, ESI, EDI, EBP];
        self.threads[i].cpu.r[IDX[n]]
    }

    pub fn handle_syscall(&mut self, i: usize) -> Option<Event> {
        let nr = self.threads[i].cpu.r[EAX];
        self.sys_counts[(nr as usize).min(511)] += 1;
        let args = [self.arg(i, 0), self.arg(i, 1), self.arg(i, 2), self.arg(i, 3), self.arg(i, 4), self.arg(i, 5)];
        let res = self.syscall(i, nr, args);
        match res {
            Ret(v) => {
                if self.trace_syscalls {
                    crate::log::log(&format!("[sys] tid={} nr={} args={:x?} -> {}", self.threads[i].tid, nr, args, v));
                }
                self.threads[i].cpu.r[EAX] = v as u32;
                self.threads[i].retry_deadline = None;
            }
            Retry => {
                // Re-run the syscall instruction after the next kernel event (`int 0x80` and
                // `sysenter` are both 2 bytes; eax is restored because it may hold a result).
                let t = &mut self.threads[i];
                t.cpu.eip = t.cpu.eip.wrapping_sub(2);
                t.cpu.r[EAX] = nr;
                t.wait = Wait::Retry { gen: self.generation };
            }
            Blocked | NoRet => {}
            ExitThread => {
                self.thread_exit(i);
                if self.threads.iter().all(|t| t.zombie) {
                    return Some(Event::Exited(self.exit_code.unwrap_or(0)));
                }
            }
            ExitProcess(code) => {
                self.exit_code = Some(code);
                return Some(Event::Exited(code));
            }
        }
        None
    }

    fn thread_exit(&mut self, i: usize) {
        let ctid = self.threads[i].clear_child_tid;
        self.threads[i].zombie = true;
        if ctid != 0 {
            let _ = self.mem.w32(ctid, 0);
            self.futex_wake(ctid, u32::MAX, u32::MAX);
        }
        self.bump();
    }

    // ---- user memory helpers ---------------------------------------------------------------
    fn cstr(&self, addr: u32) -> Result<String, ()> {
        let b = self.mem.read_cstr(addr, 4096).map_err(|_| ())?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }

    fn put(&mut self, addr: u32, data: &[u8]) -> Result<(), ()> {
        self.mem.write_bytes(addr, data).map_err(|_| ())
    }

    fn alloc_fd(&mut self, fd: Fd) -> i32 {
        for (i, s) in self.fds.iter_mut().enumerate() {
            if s.is_none() {
                *s = Some(fd);
                return i as i32;
            }
        }
        self.fds.push(Some(fd));
        (self.fds.len() - 1) as i32
    }

    fn resolve(&self, dirfd: i32, path: &str) -> String {
        if path.starts_with('/') || dirfd == -100 {
            return path.to_string();
        }
        if let Some(Some(Fd::Dir { ino, .. })) = self.fds.get(dirfd as usize) {
            // Reconstruct a path for the dir inode (linear search; rare).
            if let Some(p) = self.path_of(*ino) {
                return format!("{}/{}", p, path);
            }
        }
        path.to_string()
    }

    fn path_of(&self, target: u32) -> Option<String> {
        fn walk(v: &Vfs, cur: u32, target: u32, prefix: &str, depth: u32) -> Option<String> {
            if cur == target {
                return Some(if prefix.is_empty() { "/".into() } else { prefix.to_string() });
            }
            if depth > 12 {
                return None;
            }
            if let Some(Inode { kind: Kind::Dir(m), .. }) = v.get(cur) {
                for (n, &c) in m {
                    if let Some(Inode { kind: Kind::Dir(_), .. }) = v.get(c) {
                        if let Some(p) = walk(v, c, target, &format!("{}/{}", prefix, n), depth + 1) {
                            return Some(p);
                        }
                    }
                }
            }
            None
        }
        walk(&self.vfs, 1, target, "", 0)
    }

    fn synth_proc(&self, path: &str) -> Option<Vec<u8>> {
        let s = match path {
            "/proc/self/maps" => self.proc_maps(),
            "/proc/self/cmdline" => format!("{}\0", self.exe_path),
            "/proc/self/status" => format!("Name:\tflash\nState:\tR (running)\nTgid:\t{}\nPid:\t{}\nPPid:\t1\nThreads:\t{}\nVmRSS:\t100000 kB\n", self.pid, self.pid, self.threads.iter().filter(|t| !t.zombie).count()),
            "/proc/self/stat" => format!("{} (flash) R 1 {} {} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0 0\n", self.pid, self.pid, self.pid),
            "/proc/meminfo" => format!("MemTotal:       {} kB\nMemFree:        {} kB\nMemAvailable:   {} kB\nBuffers:        0 kB\nCached:         0 kB\nSwapTotal:      0 kB\nSwapFree:       0 kB\n", self.mem.size() / 1024, self.mem.size() / 2048, self.mem.size() / 2048),
            "/proc/cpuinfo" => "processor\t: 0\nvendor_id\t: GenuineIntel\ncpu family\t: 6\nmodel\t\t: 15\nmodel name\t: Emulated Core 2\ncpu MHz\t\t: 2000.000\nflags\t\t: fpu tsc cx8 cmov mmx fxsr sse sse2\n\n".to_string(),
            "/proc/stat" => "cpu  1 0 1 100 0 0 0 0 0 0\n".to_string(),
            "/sys/devices/system/cpu/online" | "/sys/devices/system/cpu/possible" => "0\n".to_string(),
            "/proc/sys/kernel/osrelease" => "5.15.0-flash\n".to_string(),
            _ => return None,
        };
        Some(s.into_bytes())
    }

    fn proc_maps(&self) -> String {
        let mut out = String::new();
        let mut a = 0u32;
        let end = self.mem.size() as u32;
        let label_at = |addr: u32| -> String {
            self.regions.range(..=addr).next_back().filter(|(&s, &(l, _))| addr < s + l).map(|(_, (_, n))| n.clone()).unwrap_or_default()
        };
        while a < end {
            let p = self.mem.page_prot(a);
            if p & P_MAPPED == 0 {
                a += PAGE_SIZE;
                continue;
            }
            let start = a;
            let name = label_at(a);
            while a < end && self.mem.page_prot(a) == p && label_at(a) == name {
                a += PAGE_SIZE;
            }
            out.push_str(&format!(
                "{:08x}-{:08x} {}{}{}p 00000000 00:00 0 {}{}\n",
                start,
                a,
                if p & P_R != 0 { 'r' } else { '-' },
                if p & P_W != 0 { 'w' } else { '-' },
                if p & P_X != 0 { 'x' } else { '-' },
                if name.is_empty() { "" } else { "                       " },
                name
            ));
        }
        out
    }

    // ---- stat ------------------------------------------------------------------------------
    fn fill_stat64(&mut self, addr: u32, s: &Stat) -> Result<(), ()> {
        let mut b = [0u8; 96];
        fn w32(b: &mut [u8], o: usize, v: u32) {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        w32(&mut b, 0, 0x801); // st_dev lo
        w32(&mut b, 12, s.ino);
        w32(&mut b, 16, s.mode);
        w32(&mut b, 20, s.nlink);
        w32(&mut b, 24, s.uid);
        w32(&mut b, 28, s.gid);
        w32(&mut b, 32, (s.rdev.0 << 8) | s.rdev.1);
        b[44..52].copy_from_slice(&s.size.to_le_bytes());
        w32(&mut b, 52, 4096);
        let blocks = (s.size + 511) / 512;
        b[56..64].copy_from_slice(&blocks.to_le_bytes());
        w32(&mut b, 64, s.mtime as u32);
        w32(&mut b, 72, s.mtime as u32);
        w32(&mut b, 80, s.mtime as u32);
        b[88..96].copy_from_slice(&(s.ino as u64).to_le_bytes());
        self.put(addr, &b)
    }

    fn fill_statx(&mut self, addr: u32, s: &Stat) -> Result<(), ()> {
        let mut b = [0u8; 256];
        b[0..4].copy_from_slice(&0x7ffu32.to_le_bytes()); // stx_mask: basic stats
        b[4..8].copy_from_slice(&4096u32.to_le_bytes());
        b[16..20].copy_from_slice(&s.nlink.to_le_bytes());
        b[20..24].copy_from_slice(&s.uid.to_le_bytes());
        b[24..28].copy_from_slice(&s.gid.to_le_bytes());
        b[28..30].copy_from_slice(&(s.mode as u16).to_le_bytes());
        b[32..40].copy_from_slice(&(s.ino as u64).to_le_bytes());
        b[40..48].copy_from_slice(&s.size.to_le_bytes());
        b[48..56].copy_from_slice(&((s.size + 511) / 512).to_le_bytes());
        for off in [64usize, 80, 96, 112] {
            b[off..off + 8].copy_from_slice(&(s.mtime).to_le_bytes());
        }
        b[128..132].copy_from_slice(&s.rdev.0.to_le_bytes());
        b[132..136].copy_from_slice(&s.rdev.1.to_le_bytes());
        self.put(addr, &b)
    }

    fn stat_path(&self, path: &str, follow: bool) -> Result<Stat, i32> {
        if self.synth_proc(path).is_some() {
            return Ok(Stat { ino: 0xfffff, mode: S_IFREG | 0o444, nlink: 1, uid: 0, gid: 0, size: 0, mtime: 0, rdev: (0, 0) });
        }
        let ino = self.vfs.lookup(path, follow)?;
        self.vfs.stat_ino(ino).ok_or(ENOENT)
    }

    fn stat_fd(&self, fd: i32) -> Result<Stat, i32> {
        match self.fds.get(fd as usize).and_then(|f| f.as_ref()) {
            Some(Fd::File { ino, .. }) | Some(Fd::Dir { ino, .. }) => self.vfs.stat_ino(*ino).ok_or(EBADF),
            Some(Fd::Console(_)) => Ok(Stat { ino: 1, mode: S_IFCHR | 0o620, nlink: 1, uid: 0, gid: 0, size: 0, mtime: 0, rdev: (136, 0) }),
            Some(Fd::Dev { major, minor, .. }) => Ok(Stat { ino: 2, mode: S_IFCHR | 0o666, nlink: 1, uid: 0, gid: 0, size: 0, mtime: 0, rdev: (*major, *minor) }),
            Some(Fd::Synth { data, .. }) => Ok(Stat { ino: 3, mode: S_IFREG | 0o444, nlink: 1, uid: 0, gid: 0, size: data.len() as u64, mtime: 0, rdev: (0, 0) }),
            Some(_) => Ok(Stat { ino: 4, mode: 0o010000 | 0o600, nlink: 1, uid: 0, gid: 0, size: 0, mtime: 0, rdev: (0, 0) }),
            None => Err(EBADF),
        }
    }

    // ---- open ------------------------------------------------------------------------------
    fn do_open(&mut self, path: &str, flags: u32, mode: u32) -> SysRes {
        let full = self.vfs.normalize(path);
        if let Some(data) = self.synth_proc(&full) {
            return Ret(self.alloc_fd(Fd::Synth { data, off: 0 }));
        }
        if full.starts_with("/proc/") || full.starts_with("/sys/") {
            return neg(ENOENT);
        }
        let ino = match self.vfs.lookup(&full, true) {
            Ok(i) => {
                if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                    return neg(EEXIST);
                }
                i
            }
            Err(e) if e == ENOENT && flags & O_CREAT != 0 => match self.vfs.create_file(&full, Vec::new(), mode & !0o022) {
                Ok(i) => i,
                Err(e) => return neg(e),
            },
            Err(e) => return neg(e),
        };
        let kind = self.vfs.get(ino).map(|i| i.kind.clone());
        match kind {
            Some(Kind::Dir(_)) => {
                if flags & 3 != 0 {
                    return neg(EISDIR);
                }
                Ret(self.alloc_fd(Fd::Dir { ino, pos: 0 }))
            }
            Some(Kind::CharDev(major, minor)) => Ret(self.alloc_fd(Fd::Dev { major, minor, flags })),
            Some(Kind::File(_)) => {
                if flags & O_DIRECTORY != 0 {
                    return neg(ENOTDIR);
                }
                if flags & O_TRUNC != 0 && flags & 3 != 0 {
                    if let Some(Inode { kind: Kind::File(d), .. }) = self.vfs.get_mut(ino) {
                        d.clear();
                    }
                }
                Ret(self.alloc_fd(Fd::File { ino, off: 0, flags }))
            }
            _ => neg(ENOENT),
        }
    }

    // ---- read/write ------------------------------------------------------------------------
    fn do_read(&mut self, i: usize, fd: i32, buf: u32, count: u32) -> SysRes {
        let count = count.min(1 << 26) as usize;
        let slot = match self.fds.get_mut(fd as usize) {
            Some(Some(f)) => f,
            _ => return neg(EBADF),
        };
        let mut tmp: Vec<u8>;
        match slot {
            Fd::Console(0) => return Ret(0),
            Fd::Console(_) => return neg(EBADF),
            Fd::File { ino, off, .. } => {
                let ino = *ino;
                let o = *off as usize;
                tmp = match self.vfs.get(ino) {
                    Some(Inode { kind: Kind::File(d), .. }) => {
                        if o >= d.len() {
                            Vec::new()
                        } else {
                            d[o..(o + count).min(d.len())].to_vec()
                        }
                    }
                    _ => return neg(EBADF),
                };
                if let Some(Some(Fd::File { off, .. })) = self.fds.get_mut(fd as usize) {
                    *off += tmp.len() as u64;
                }
            }
            Fd::Dir { .. } => return neg(EISDIR),
            Fd::Dev { major, minor, .. } => {
                let (ma, mi) = (*major, *minor);
                tmp = vec![0u8; count.min(1 << 16)];
                match (ma, mi) {
                    (1, 3) => tmp.clear(),
                    (1, 5) => {}
                    (1, 8) | (1, 9) | (10, 183) => self.host.random(&mut tmp),
                    _ => tmp.clear(),
                }
            }
            Fd::Synth { data, off } => {
                let o = *off;
                tmp = if o >= data.len() { Vec::new() } else { data[o..(o + count).min(data.len())].to_vec() };
                *off += tmp.len();
            }
            Fd::PipeR(p) | Fd::Sock { rx: p, .. } => {
                let p = *p;
                let pipe = &mut self.pipes[p];
                if pipe.buf.is_empty() {
                    if pipe.writers == 0 {
                        return Ret(0);
                    }
                    return Retry;
                }
                let n = count.min(pipe.buf.len());
                tmp = pipe.buf.drain(..n).collect();
                self.generation += 1;
            }
            Fd::PipeW(_) => return neg(EBADF),
        }
        let _ = i;
        let n = tmp.len();
        mf!(self.put(buf, &tmp));
        Ret(n as i32)
    }

    fn do_write(&mut self, fd: i32, buf: u32, count: u32) -> SysRes {
        let data = mf!(self.mem.read_bytes(buf, count)).to_vec();
        let slot = match self.fds.get_mut(fd as usize) {
            Some(Some(f)) => f,
            _ => return neg(EBADF),
        };
        match slot {
            Fd::Console(n) if *n >= 1 => {
                let n = *n;
                self.host.console(n, &data);
                Ret(count as i32)
            }
            Fd::File { ino, off, flags } => {
                let ino = *ino;
                let append = *flags & O_APPEND != 0;
                let o = *off as usize;
                let newoff;
                match self.vfs.get_mut(ino) {
                    Some(Inode { kind: Kind::File(d), .. }) => {
                        let start = if append { d.len() } else { o };
                        if d.len() < start + data.len() {
                            d.resize(start + data.len(), 0);
                        }
                        d[start..start + data.len()].copy_from_slice(&data);
                        newoff = (start + data.len()) as u64;
                    }
                    _ => return neg(EBADF),
                }
                if let Some(Some(Fd::File { off, .. })) = self.fds.get_mut(fd as usize) {
                    *off = newoff;
                }
                Ret(count as i32)
            }
            Fd::Dev { .. } => Ret(count as i32),
            Fd::PipeW(p) | Fd::Sock { tx: p, .. } => {
                let p = *p;
                if self.pipes[p].readers == 0 {
                    return neg(EPIPE);
                }
                self.pipes[p].buf.extend(data.iter());
                self.generation += 1;
                Ret(count as i32)
            }
            _ => neg(EBADF),
        }
    }

    fn readv_writev(&mut self, i: usize, fd: i32, iov: u32, cnt: u32, write: bool) -> SysRes {
        let mut total = 0i32;
        for k in 0..cnt {
            let base = mf!(self.mem.r32(iov + k * 8));
            let len = mf!(self.mem.r32(iov + k * 8 + 4));
            if len == 0 {
                continue;
            }
            let r = if write { self.do_write(fd, base, len) } else { self.do_read(i, fd, base, len) };
            match r {
                Ret(n) if n >= 0 => {
                    total += n;
                    if (n as u32) < len {
                        break;
                    }
                }
                Ret(e) => return if total > 0 { Ret(total) } else { Ret(e) },
                Retry => return if total > 0 { Ret(total) } else { Retry },
                other => return other,
            }
        }
        Ret(total)
    }

    // ---- mmap ------------------------------------------------------------------------------
    fn do_mmap(&mut self, addr: u32, len: u32, prot: u32, flags: u32, fd: i32, off: u64) -> SysRes {
        if len == 0 {
            return neg(EINVAL);
        }
        let len = (len + 0xfff) & !0xfff;
        let anon = flags & 0x20 != 0;
        let fixed = flags & 0x10 != 0;
        let base = if fixed {
            if addr & 0xfff != 0 || (addr as u64 + len as u64) > self.mem.size() as u64 {
                return neg(EINVAL);
            }
            self.free_region(addr, len);
            self.regions.insert(addr, (len, String::new()));
            addr
        } else {
            match self.alloc_region(len, "") {
                Some(a) => a,
                None => return neg(ENOMEM),
            }
        };
        let p = (prot as u8 & 7) | P_MAPPED;
        self.mem.set_prot(base, len, P_MAPPED | P_R | P_W);
        self.mem.zero(base, len);
        if !anon {
            let data: Vec<u8> = match self.fds.get(fd as usize).and_then(|f| f.as_ref()) {
                Some(Fd::File { ino, .. }) => match self.vfs.get(*ino) {
                    Some(Inode { kind: Kind::File(d), .. }) => {
                        let o = off as usize;
                        if o >= d.len() {
                            Vec::new()
                        } else {
                            d[o..(o + len as usize).min(d.len())].to_vec()
                        }
                    }
                    _ => return neg(EBADF),
                },
                _ => return neg(EBADF),
            };
            let _ = self.mem.write_bytes(base, &data);
            if let Some(Some(Fd::File { ino, .. })) = self.fds.get(fd as usize) {
                if let Some(path) = self.path_hint(*ino) {
                    if let Some(r) = self.regions.get_mut(&base) {
                        r.1 = path;
                    }
                }
            }
        }
        self.mem.set_prot(base, len, p);
        Ret(base as i32)
    }

    fn path_hint(&self, ino: u32) -> Option<String> {
        // Cheap: only used for /proc/self/maps labels of file mappings.
        fn find(v: &Vfs, dir: u32, target: u32, prefix: &str, depth: u32) -> Option<String> {
            if depth > 6 {
                return None;
            }
            if let Some(Inode { kind: Kind::Dir(m), .. }) = v.get(dir) {
                for (n, &c) in m {
                    let p = format!("{}/{}", prefix, n);
                    if c == target {
                        return Some(p);
                    }
                    if let Some(Inode { kind: Kind::Dir(_), .. }) = v.get(c) {
                        if let Some(r) = find(v, c, target, &p, depth + 1) {
                            return Some(r);
                        }
                    }
                }
            }
            None
        }
        find(&self.vfs, 1, ino, "", 0)
    }

    fn do_brk(&mut self, new: u32) -> u32 {
        if new == 0 || new < self.brk_start || new > self.brk_limit {
            return self.brk_cur;
        }
        let old_end = (self.brk_cur + 0xfff) & !0xfff;
        let new_end = (new + 0xfff) & !0xfff;
        if new_end > old_end {
            self.mem.set_prot(old_end, new_end - old_end, P_MAPPED | P_R | P_W);
        } else if new_end < old_end {
            self.mem.set_prot(new_end, old_end - new_end, 0);
            self.mem.zero(new_end, old_end - new_end);
        }
        self.brk_cur = new;
        new
    }

    // ---- time ------------------------------------------------------------------------------
    fn clock_ns(&mut self, clk: u32) -> u64 {
        match clk {
            0 | 5 => self.real_ns(),
            _ => self.mono_ns(),
        }
    }

    /// The pure time syscalls (no thread/fs/state beyond the clocks): shared between the kernel
    /// switch and the JIT's inline fast path (`jit_sys_time`), so both answer bit-identically.
    /// `None` = not a time syscall. Faulting timespec pointers yield -EFAULT exactly like the
    /// `mf!` path in the big switch.
    pub fn time_syscall(&mut self, nr: u32, a: [u32; 6]) -> Option<(u32, bool)> {
        let d0 = self.mem.dirty_code.len();
        let efault = (-EFAULT) as u32;
        let done = |v: u32, d0: usize, dirty: &Vec<(u32, u32)>| Some((v, dirty.len() != d0));
        match nr {
            78 => {
                let ns = self.real_ns();
                if a[0] != 0
                    && (self.mem.w32(a[0], (ns / 1_000_000_000) as u32).is_err()
                        || self.mem.w32(a[0] + 4, ((ns % 1_000_000_000) / 1000) as u32).is_err())
                {
                    return done(efault, d0, &self.mem.dirty_code);
                }
                done(0, d0, &self.mem.dirty_code)
            }
            265 | 403 => {
                let ns = self.clock_ns(a[0]);
                if nr == 265 {
                    // 32-bit timespec: two 4-byte fields
                    if self.mem.w32(a[1], (ns / 1_000_000_000) as u32).is_err() || self.mem.w32(a[1] + 4, (ns % 1_000_000_000) as u32).is_err() {
                        return done(efault, d0, &self.mem.dirty_code);
                    }
                } else if self.mem.w64(a[1], ns / 1_000_000_000).is_err() || self.mem.w64(a[1] + 8, ns % 1_000_000_000).is_err() {
                    // clock_gettime64: full 64-bit fields - a 4-byte write leaves the upper half of the
                    // guest's timespec64 as stack garbage (glibc reads i64) and deadlines explode
                    return done(efault, d0, &self.mem.dirty_code);
                }
                done(0, d0, &self.mem.dirty_code)
            }
            266 | 406 => {
                if a[1] != 0 && (self.mem.w32(a[1], 0).is_err() || self.mem.w32(a[1] + 4, 1_000_000).is_err()) {
                    return done(efault, d0, &self.mem.dirty_code);
                }
                done(0, d0, &self.mem.dirty_code)
            }
            _ => None,
        }
    }

    // ---- the big switch --------------------------------------------------------------------
    pub fn syscall(&mut self, i: usize, nr: u32, a: [u32; 6]) -> SysRes {
        if let Some((v, _)) = self.time_syscall(nr, a) {
            return Ret(v as i32);
        }
        match nr {
            1 => ExitThread,
            252 => ExitProcess(a[0] as i32),
            3 => self.do_read(i, a[0] as i32, a[1], a[2]),
            4 => self.do_write(a[0] as i32, a[1], a[2]),
            145 => self.readv_writev(i, a[0] as i32, a[1], a[2], false),
            146 => self.readv_writev(i, a[0] as i32, a[1], a[2], true),
            5 | 295 => {
                let (dirfd, p, fl, mode) = if nr == 295 { (a[0] as i32, a[1], a[2], a[3]) } else { (-100, a[0], a[1], a[2]) };
                let path = mf!(self.cstr(p));
                let path = self.resolve(dirfd, &path);
                self.do_open(&path, fl, mode)
            }
            6 => match self.fds.get_mut(a[0] as usize) {
                Some(s @ Some(_)) => {
                    match s.take() {
                        Some(Fd::PipeR(p)) => {
                            self.pipes[p].readers -= 1;
                            self.generation += 1;
                        }
                        Some(Fd::PipeW(p)) => {
                            self.pipes[p].writers -= 1;
                            self.generation += 1;
                        }
                        Some(Fd::Sock { rx, tx }) => {
                            self.pipes[rx].readers -= 1;
                            self.pipes[tx].writers -= 1;
                            self.generation += 1;
                        }
                        _ => {}
                    }
                    Ret(0)
                }
                _ => neg(EBADF),
            },
            19 | 140 => {
                // lseek(fd, off, whence) / _llseek(fd, hi, lo, result*, whence)
                let (off, whence, res_ptr) = if nr == 19 { (a[1] as i32 as i64, a[2], 0) } else { ((((a[1] as u64) << 32) | a[2] as u64) as i64, a[4], a[3]) };
                let (size, cur) = match self.fds.get(a[0] as usize).and_then(|f| f.as_ref()) {
                    Some(Fd::File { ino, off, .. }) => (self.vfs.stat_ino(*ino).map(|s| s.size).unwrap_or(0) as i64, *off as i64),
                    Some(Fd::Synth { data, off }) => (data.len() as i64, *off as i64),
                    Some(Fd::Dir { .. }) => (0, 0),
                    Some(Fd::Dev { .. }) | Some(Fd::Console(_)) => return Ret(0),
                    Some(_) => return neg(ESPIPE),
                    None => return neg(EBADF),
                };
                let newoff = match whence {
                    0 => off,
                    1 => cur + off,
                    2 => size + off,
                    _ => return neg(EINVAL),
                };
                if newoff < 0 {
                    return neg(EINVAL);
                }
                match self.fds.get_mut(a[0] as usize) {
                    Some(Some(Fd::File { off, .. })) => *off = newoff as u64,
                    Some(Some(Fd::Synth { off, .. })) => *off = newoff as usize,
                    Some(Some(Fd::Dir { pos, .. })) => *pos = newoff as usize,
                    _ => {}
                }
                if nr == 140 {
                    mf!(self.put(res_ptr, &(newoff as u64).to_le_bytes()));
                    Ret(0)
                } else {
                    Ret(newoff as i32)
                }
            }
            180 | 181 => {
                // pread64/pwrite64(fd, buf, count, off_lo, off_hi)
                let off = ((a[4] as u64) << 32) | a[3] as u64;
                let saved = match self.fds.get(a[0] as usize).and_then(|f| f.as_ref()) {
                    Some(Fd::File { off, .. }) => *off,
                    _ => return neg(ESPIPE),
                };
                if let Some(Some(Fd::File { off: o, .. })) = self.fds.get_mut(a[0] as usize) {
                    *o = off;
                }
                let r = if nr == 180 { self.do_read(i, a[0] as i32, a[1], a[2]) } else { self.do_write(a[0] as i32, a[1], a[2]) };
                if let Some(Some(Fd::File { off: o, .. })) = self.fds.get_mut(a[0] as usize) {
                    *o = saved;
                }
                r
            }
            33 | 307 => {
                let (dirfd, p) = if nr == 307 { (a[0] as i32, a[1]) } else { (-100, a[0]) };
                let path = mf!(self.cstr(p));
                let path = self.resolve(dirfd, &path);
                match self.stat_path(&path, true) {
                    Ok(_) => Ret(0),
                    Err(e) => neg(e),
                }
            }
            195 | 196 | 197 | 300 => {
                let (res, out) = match nr {
                    195 | 196 => {
                        let path = mf!(self.cstr(a[0]));
                        (self.stat_path(&path, nr == 195), a[1])
                    }
                    197 => (self.stat_fd(a[0] as i32), a[1]),
                    _ => {
                        let path = mf!(self.cstr(a[1]));
                        let flags = a[3];
                        if path.is_empty() && flags & 0x1000 != 0 {
                            (self.stat_fd(a[0] as i32), a[2])
                        } else {
                            let path = self.resolve(a[0] as i32, &path);
                            (self.stat_path(&path, flags & 0x100 == 0), a[2])
                        }
                    }
                };
                match res {
                    Ok(s) => {
                        mf!(self.fill_stat64(out, &s));
                        Ret(0)
                    }
                    Err(e) => neg(e),
                }
            }
            383 => {
                // statx(dirfd, path, flags, mask, buf)
                let path = mf!(self.cstr(a[1]));
                let res = if path.is_empty() && a[2] & 0x1000 != 0 {
                    self.stat_fd(a[0] as i32)
                } else {
                    let p = self.resolve(a[0] as i32, &path);
                    self.stat_path(&p, a[2] & 0x100 == 0)
                };
                match res {
                    Ok(s) => {
                        mf!(self.fill_statx(a[4], &s));
                        Ret(0)
                    }
                    Err(e) => neg(e),
                }
            }
            85 | 305 => {
                let (dirfd, p, buf, sz) = if nr == 305 { (a[0] as i32, a[1], a[2], a[3]) } else { (-100, a[0], a[1], a[2]) };
                let path = mf!(self.cstr(p));
                let path = self.resolve(dirfd, &path);
                let target = if path == "/proc/self/exe" { Some(self.exe_path.clone()) } else {
                    match self.vfs.lookup(&path, false) {
                        Ok(ino) => match self.vfs.get(ino) {
                            Some(Inode { kind: Kind::Symlink(t), .. }) => Some(t.clone()),
                            _ => return neg(EINVAL),
                        },
                        Err(e) => return neg(e),
                    }
                };
                let t = target.unwrap();
                let n = t.len().min(sz as usize);
                mf!(self.put(buf, &t.as_bytes()[..n]));
                Ret(n as i32)
            }
            183 => {
                let cwd = format!("{}\0", self.vfs.cwd);
                if cwd.len() > a[1] as usize {
                    return neg(ERANGE);
                }
                mf!(self.put(a[0], cwd.as_bytes()));
                Ret(cwd.len() as i32)
            }
            12 => {
                let path = mf!(self.cstr(a[0]));
                let full = self.vfs.normalize(&path);
                match self.vfs.lookup(&full, true) {
                    Ok(_) => {
                        self.vfs.cwd = full;
                        Ret(0)
                    }
                    Err(e) => neg(e),
                }
            }
            10 | 301 => {
                let (dirfd, p) = if nr == 301 { (a[0] as i32, a[1]) } else { (-100, a[0]) };
                let path = mf!(self.cstr(p));
                let path = self.resolve(dirfd, &path);
                match self.vfs.unlink(&path) {
                    Ok(()) => Ret(0),
                    Err(e) => neg(e),
                }
            }
            39 | 296 => {
                let (dirfd, p, mode) = if nr == 296 { (a[0] as i32, a[1], a[2]) } else { (-100, a[0], a[1]) };
                let path = mf!(self.cstr(p));
                let path = self.resolve(dirfd, &path);
                match self.vfs.mkdir(&path, mode) {
                    Ok(()) => Ret(0),
                    Err(e) => neg(e),
                }
            }
            40 => {
                let path = mf!(self.cstr(a[0]));
                match self.vfs.unlink(&path) {
                    Ok(()) => Ret(0),
                    Err(e) => neg(e),
                }
            }
            38 => {
                let from = mf!(self.cstr(a[0]));
                let to = mf!(self.cstr(a[1]));
                match self.vfs.rename(&from, &to) {
                    Ok(()) => Ret(0),
                    Err(e) => neg(e),
                }
            }
            220 | 141 => {
                // getdents64 / getdents
                let (ino, pos) = match self.fds.get(a[0] as usize).and_then(|f| f.as_ref()) {
                    Some(Fd::Dir { ino, pos }) => (*ino, *pos),
                    _ => return neg(EBADF),
                };
                let entries: Vec<(String, u32, u8)> = match self.vfs.get(ino) {
                    Some(Inode { kind: Kind::Dir(m), .. }) => {
                        let mut v = vec![(".".to_string(), ino, 4u8), ("..".to_string(), ino, 4u8)];
                        for (n, &c) in m {
                            let t = match self.vfs.get(c).map(|x| x.mode & S_IFMT) {
                                Some(S_IFDIR) => 4,
                                Some(S_IFLNK) => 10,
                                Some(S_IFCHR) => 2,
                                _ => 8,
                            };
                            v.push((n.clone(), c, t));
                        }
                        v
                    }
                    _ => return neg(ENOTDIR),
                };
                let mut out: Vec<u8> = Vec::new();
                let mut k = pos;
                while k < entries.len() {
                    let (name, eino, ty) = &entries[k];
                    let rec = if nr == 220 { (19 + name.len() + 1 + 7) & !7 } else { (10 + name.len() + 1 + 1 + 3) & !3 };
                    if out.len() + rec > a[2] as usize {
                        break;
                    }
                    let start = out.len();
                    out.resize(start + rec, 0);
                    if nr == 220 {
                        out[start..start + 8].copy_from_slice(&(*eino as u64).to_le_bytes());
                        out[start + 8..start + 16].copy_from_slice(&((k + 1) as u64).to_le_bytes());
                        out[start + 16..start + 18].copy_from_slice(&(rec as u16).to_le_bytes());
                        out[start + 18] = *ty;
                        out[start + 19..start + 19 + name.len()].copy_from_slice(name.as_bytes());
                    } else {
                        out[start..start + 4].copy_from_slice(&eino.to_le_bytes());
                        out[start + 4..start + 8].copy_from_slice(&((k + 1) as u32).to_le_bytes());
                        out[start + 8..start + 10].copy_from_slice(&(rec as u16).to_le_bytes());
                        out[start + 10..start + 10 + name.len()].copy_from_slice(name.as_bytes());
                        out[start + rec - 1] = *ty;
                    }
                    k += 1;
                }
                if out.is_empty() && k < entries.len() {
                    return neg(EINVAL);
                }
                mf!(self.put(a[1], &out));
                if let Some(Some(Fd::Dir { pos, .. })) = self.fds.get_mut(a[0] as usize) {
                    *pos = k;
                }
                Ret(out.len() as i32)
            }
            54 => {
                // ioctl
                match (self.fds.get(a[0] as usize).and_then(|f| f.as_ref()), a[1]) {
                    (None, _) => neg(EBADF),
                    (Some(Fd::PipeR(p)), 0x541b) | (Some(Fd::Sock { rx: p, .. }), 0x541b) => {
                        let n = self.pipes[*p].buf.len() as u32;
                        mf!(self.put(a[2], &n.to_le_bytes()));
                        Ret(0)
                    }
                    (Some(_), 0x5421) | (Some(_), 0x5451) | (Some(_), 0x5450) => Ret(0), // FIONBIO / FIOCLEX / FIONCLEX
                    (Some(_), _) => neg(ENOTTY),
                }
            }
            55 | 221 => {
                // fcntl: F_DUPFD=0 F_GETFD=1 F_SETFD=2 F_GETFL=3 F_SETFL=4
                match a[1] {
                    0 | 1030 => {
                        let f = match self.fds.get(a[0] as usize) {
                            Some(Some(_)) => (),
                            _ => return neg(EBADF),
                        };
                        let _ = f;
                        let new = self.dup_fd(a[0] as i32, a[2] as usize);
                        Ret(new)
                    }
                    1 | 2 => Ret(0),
                    3 => match self.fds.get(a[0] as usize).and_then(|f| f.as_ref()) {
                        Some(Fd::File { flags, .. }) | Some(Fd::Dev { flags, .. }) => Ret(*flags as i32),
                        Some(_) => Ret(2),
                        None => neg(EBADF),
                    },
                    4 | 5 | 6 | 7 | 12 | 13 | 14 => Ret(0),
                    _ => neg(EINVAL),
                }
            }
            41 | 63 | 330 => {
                if nr == 41 {
                    return Ret(self.dup_fd(a[0] as i32, 0));
                }
                self.dup2_fd(a[0] as i32, a[1] as usize)
            }
            42 | 331 => {
                let p = self.pipes.len();
                self.pipes.push(Pipe { buf: VecDeque::new(), readers: 1, writers: 1 });
                let r = self.alloc_fd(Fd::PipeR(p));
                let w = self.alloc_fd(Fd::PipeW(p));
                mf!(self.put(a[0], &(r as u32).to_le_bytes()));
                mf!(self.put(a[0] + 4, &(w as u32).to_le_bytes()));
                Ret(0)
            }
            360 | 102 => {
                // socketpair(domain, type, proto, sv[2]) or socketcall multiplexer
                let (call, args_ptr) = if nr == 102 { (a[0], a[1]) } else { (8, 0) };
                if nr == 102 && call != 8 {
                    return neg(EAFAULT_OR_NOSYS(call));
                }
                let sv = if nr == 102 { mf!(self.mem.r32(args_ptr + 12)) } else { a[3] };
                let (p1, p2) = (self.pipes.len(), self.pipes.len() + 1);
                self.pipes.push(Pipe { buf: VecDeque::new(), readers: 1, writers: 1 });
                self.pipes.push(Pipe { buf: VecDeque::new(), readers: 1, writers: 1 });
                let f1 = self.alloc_fd(Fd::Sock { rx: p1, tx: p2 });
                let f2 = self.alloc_fd(Fd::Sock { rx: p2, tx: p1 });
                mf!(self.put(sv, &(f1 as u32).to_le_bytes()));
                mf!(self.put(sv + 4, &(f2 as u32).to_le_bytes()));
                Ret(0)
            }
            359 => neg(EAFNOSUPPORT), // socket
            143 | 148 | 118 | 94 | 95 | 15 | 207 | 212 | 182 | 198 | 60 | 36 | 133 => Ret(0), // flock fdatasync fsync fchmod fchown chmod ... umask sync
            93 | 194 => {
                // ftruncate(64)
                let len = if nr == 194 { ((a[2] as u64) << 32) | a[1] as u64 } else { a[1] as u64 };
                match self.fds.get(a[0] as usize).and_then(|f| f.as_ref()) {
                    Some(Fd::File { ino, .. }) => {
                        let ino = *ino;
                        if let Some(Inode { kind: Kind::File(d), .. }) = self.vfs.get_mut(ino) {
                            d.resize(len as usize, 0);
                        }
                        Ret(0)
                    }
                    _ => neg(EBADF),
                }
            }

            // ---- memory ----
            45 => Ret(self.do_brk(a[0]) as i32),
            192 => self.do_mmap(a[0], a[1], a[2], a[3], a[4] as i32, (a[5] as u64) * 4096),
            90 => {
                let p = a[0];
                let w = |k: u32| self.mem.r32(p + k * 4);
                match (w(0), w(1), w(2), w(3), w(4), w(5)) {
                    (Ok(ad), Ok(l), Ok(pr), Ok(fl), Ok(fd), Ok(of)) => self.do_mmap(ad, l, pr, fl, fd as i32, of as u64),
                    _ => neg(EFAULT),
                }
            }
            91 => {
                if a[0] & 0xfff != 0 {
                    return neg(EINVAL);
                }
                self.free_region(a[0], a[1]);
                Ret(0)
            }
            125 => {
                if a[0] & 0xfff != 0 {
                    return neg(EINVAL);
                }
                let len = (a[1] + 0xfff) & !0xfff;
                if !self.mem.range_mapped(a[0], len) {
                    return neg(ENOMEM);
                }
                self.mem.set_prot(a[0], len, (a[2] as u8 & 7) | P_MAPPED);
                Ret(0)
            }
            163 => {
                // mremap(old, oldlen, newlen, flags): grow/shrink in place when possible, else move.
                let (old, olen, nlen) = (a[0], (a[1] + 0xfff) & !0xfff, (a[2] + 0xfff) & !0xfff);
                if nlen <= olen {
                    self.free_region(old + nlen, olen - nlen);
                    return Ret(old as i32);
                }
                if a[3] & 1 == 0 {
                    return neg(ENOMEM);
                }
                let Some(new) = self.alloc_region(nlen, "") else { return neg(ENOMEM) };
                let prot = self.mem.page_prot(old);
                self.mem.set_prot(new, nlen, P_MAPPED | P_R | P_W);
                let data = self.mem.raw()[old as usize..(old + olen) as usize].to_vec();
                let _ = self.mem.write_bytes(new, &data);
                self.mem.set_prot(new, nlen, prot);
                self.free_region(old, olen);
                Ret(new as i32)
            }
            219 | 150 | 151 | 152 | 153 | 218 => Ret(0), // madvise mlock munlock mlockall munlockall mincore

            // ---- identity / limits ----
            20 | 224 => Ret(if nr == 20 { self.pid } else { self.threads[i].tid } as i32),
            64 => Ret(1),
            24 | 199 | 201 | 47 | 200 | 202 | 49 | 50 => Ret(1000),
            122 => {
                let mut b = [0u8; 390];
                for (k, s) in ["Linux", "flash", "5.15.0-flash", "#1 SMP", "i686", "(none)"].iter().enumerate() {
                    b[k * 65..k * 65 + s.len()].copy_from_slice(s.as_bytes());
                }
                mf!(self.put(a[0], &b));
                Ret(0)
            }
            116 => {
                let mut b = [0u8; 64];
                let up = (self.mono_ns() / 1_000_000_000) as u32;
                b[0..4].copy_from_slice(&up.to_le_bytes());
                let ram = (self.mem.size() as u32) / 1;
                b[16..20].copy_from_slice(&ram.to_le_bytes());
                b[20..24].copy_from_slice(&(ram / 2).to_le_bytes());
                b[40..42].copy_from_slice(&1u16.to_le_bytes());
                b[52..56].copy_from_slice(&1u32.to_le_bytes());
                mf!(self.put(a[0], &b));
                Ret(0)
            }
            76 | 191 | 340 => {
                // getrlimit / ugetrlimit / prlimit64(pid, res, new, old)
                let (res, out) = if nr == 340 { (a[1], a[3]) } else { (a[0], a[1]) };
                let (cur, max): (u64, u64) = match res {
                    3 => (STACK_SIZE as u64, u64::MAX),   // RLIMIT_STACK
                    7 => (1024, 4096),                    // NOFILE
                    _ => (u64::MAX, u64::MAX),
                };
                if out != 0 {
                    if nr == 340 {
                        mf!(self.put(out, &cur.to_le_bytes()));
                        mf!(self.put(out + 8, &max.to_le_bytes()));
                    } else {
                        mf!(self.put(out, &(cur.min(0xffff_ffff) as u32).to_le_bytes()));
                        mf!(self.put(out + 4, &(max.min(0xffff_ffff) as u32).to_le_bytes()));
                    }
                }
                Ret(0)
            }
            75 => Ret(0),
            77 | 43 => {
                mf!(self.put(a[0], &[0u8; 72]));
                Ret(0)
            }
            242 => {
                // sched_getaffinity(pid, len, mask) -> one CPU
                if a[1] < 4 {
                    return neg(EINVAL);
                }
                mf!(self.put(a[2], &1u32.to_le_bytes()));
                Ret(4)
            }
            241 | 156 | 157 | 154 | 155 | 159 | 160 | 161 => Ret(0),
            158 => Retry_yield(),
            172 => Ret(0), // prctl
            258 => {
                self.threads[i].clear_child_tid = a[0];
                Ret(self.threads[i].tid as i32)
            }
            311 => {
                self.threads[i].robust_list = a[0];
                Ret(0)
            }
            312 => neg(ENOSYS),
            386 => neg(ENOSYS), // rseq
            355 => {
                let mut buf = vec![0u8; a[1].min(1 << 20) as usize];
                self.host.random(&mut buf);
                mf!(self.put(a[0], &buf));
                Ret(buf.len() as i32)
            }

            // ---- time ----
            13 => {
                let s = (self.real_ns() / 1_000_000_000) as u32;
                if a[0] != 0 {
                    mf!(self.put(a[0], &s.to_le_bytes()));
                }
                Ret(s as i32)
            }
            78 => {
                let ns = self.real_ns();
                if a[0] != 0 {
                    mf!(self.put(a[0], &((ns / 1_000_000_000) as u32).to_le_bytes()));
                    mf!(self.put(a[0] + 4, &(((ns % 1_000_000_000) / 1000) as u32).to_le_bytes()));
                }
                Ret(0)
            }
            265 | 403 => {
                let ns = self.clock_ns(a[0]);
                if nr == 265 {
                    mf!(self.put(a[1], &((ns / 1_000_000_000) as u32).to_le_bytes()));
                    mf!(self.put(a[1] + 4, &((ns % 1_000_000_000) as u32).to_le_bytes()));
                } else {
                    mf!(self.put(a[1], &(ns / 1_000_000_000).to_le_bytes()));
                    mf!(self.put(a[1] + 8, &(ns % 1_000_000_000).to_le_bytes()));
                }
                Ret(0)
            }
            266 | 406 => {
                if a[1] != 0 {
                    mf!(self.put(a[1], &0u32.to_le_bytes()));
                    mf!(self.put(a[1] + 4, &1_000_000u32.to_le_bytes()));
                }
                Ret(0)
            }
            162 | 267 | 407 => {
                // nanosleep(req, rem) / clock_nanosleep(clk, flags, req, rem)
                let (flags, req, wide) = match nr {
                    162 => (0, a[0], false),
                    267 => (a[1], a[2], false),
                    _ => (a[1], a[2], true),
                };
                let (sec, nsec) = if wide {
                    (mf!(self.mem.r64(req)), mf!(self.mem.r64(req + 8)))
                } else {
                    (mf!(self.mem.r32(req)) as u64, mf!(self.mem.r32(req + 4)) as u64)
                };
                let dur = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
                let now = self.mono_ns();
                let deadline = if flags & 1 != 0 {
                    // TIMER_ABSTIME against the selected clock; convert realtime to monotonic.
                    let clk_now = self.clock_ns(if nr == 162 { 1 } else { a[0] });
                    now + dur.saturating_sub(clk_now)
                } else {
                    now + dur
                };
                let t = &mut self.threads[i];
                t.wait = Wait::Sleep;
                t.deadline = Some(deadline);
                t.wake_val = 0;
                Blocked
            }

            // ---- futex / threads ----
            240 | 422 => self.sys_futex(i, a, nr == 422),
            243 => {
                // set_thread_area(user_desc*)
                let d = a[0];
                let mut entry = mf!(self.mem.r32(d));
                let base = mf!(self.mem.r32(d + 4));
                if entry == u32::MAX {
                    entry = 6;
                    mf!(self.mem.w32(d, entry));
                }
                if !(6..9).contains(&entry) {
                    return neg(EINVAL);
                }
                self.threads[i].cpu.tls_base[(entry - 6) as usize] = base;
                Ret(0)
            }
            244 => neg(ENOSYS),
            120 => self.sys_clone(i, a),
            238 | 270 | 37 => {
                // tkill(tid, sig) / tgkill(tgid, tid, sig) / kill(pid, sig)
                let (tid, sig) = match nr {
                    270 => (a[1], a[2]),
                    238 => (a[0], a[1]),
                    _ => (self.threads[i].tid, a[1]),
                };
                if sig == 0 {
                    return Ret(0);
                }
                let Some(ti) = self.threads.iter().position(|t| t.tid == tid && !t.zombie) else { return neg(ESRCH) };
                // tkill/tgkill report SI_TKILL (-6); kill reports SI_USER (0). glibc's SIGCANCEL handler checks this.
                let code = if nr == 37 { 0 } else { -6 };
                let pid = self.pid;
                self.post_signal(ti, sig, code, pid, 1000)
            }
            174 => {
                // rt_sigaction(sig, act, old, sigsetsize)
                let sig = a[0] as usize;
                if sig == 0 || sig > 64 || sig == 9 || sig == 19 {
                    return neg(EINVAL);
                }
                if a[2] != 0 {
                    let sa = &self.sigactions[sig];
                    let mut b = [0u8; 20];
                    b[0..4].copy_from_slice(&sa.handler.to_le_bytes());
                    b[4..8].copy_from_slice(&sa.flags.to_le_bytes());
                    b[8..12].copy_from_slice(&sa.restorer.to_le_bytes());
                    b[12..20].copy_from_slice(&sa.mask.to_le_bytes());
                    mf!(self.put(a[2], &b));
                }
                if a[1] != 0 {
                    let h = mf!(self.mem.r32(a[1]));
                    let f = mf!(self.mem.r32(a[1] + 4));
                    let r = mf!(self.mem.r32(a[1] + 8));
                    let m = mf!(self.mem.r64(a[1] + 12));
                    self.sigactions[sig] = SigAction { handler: h, flags: f, restorer: r, mask: m };
                }
                Ret(0)
            }
            175 => {
                // rt_sigprocmask(how, set, oldset, size)
                let old = self.threads[i].sigmask;
                if a[2] != 0 {
                    mf!(self.put(a[2], &old.to_le_bytes()));
                }
                if a[1] != 0 {
                    let set = mf!(self.mem.r64(a[1]));
                    let t = &mut self.threads[i];
                    match a[0] {
                        0 => t.sigmask |= set,
                        1 => t.sigmask &= !set,
                        2 => t.sigmask = set,
                        _ => return neg(EINVAL),
                    }
                    t.sigmask &= !((1 << 8) | (1 << 18)); // SIGKILL/SIGSTOP unblockable
                }
                Ret(0)
            }
            126 => {
                // sigprocmask(how, set*, oldset*) with 32-bit old_sigset_t
                let old = self.threads[i].sigmask;
                if a[2] != 0 {
                    mf!(self.put(a[2], &(old as u32).to_le_bytes()));
                }
                if a[1] != 0 {
                    let set = mf!(self.mem.r32(a[1])) as u64;
                    let t = &mut self.threads[i];
                    match a[0] {
                        0 => t.sigmask |= set,
                        1 => t.sigmask &= !set,
                        2 => t.sigmask = (t.sigmask & !0xffff_ffff) | set,
                        _ => return neg(EINVAL),
                    }
                    t.sigmask &= !((1 << 8) | (1 << 18));
                }
                Ret(0)
            }
            73 | 176 => {
                // sigpending / rt_sigpending
                let p = self.threads[i].pending & self.threads[i].sigmask;
                if nr == 73 { mf!(self.put(a[0], &(p as u32).to_le_bytes())); } else { mf!(self.put(a[0], &p.to_le_bytes())); }
                Ret(0)
            }
            29 | 179 | 72 => {
                // pause / rt_sigsuspend(mask, size) / sigsuspend(mask): sleep until a signal handler has run
                if nr != 29 {
                    let m = if nr == 179 { mf!(self.mem.r64(a[0])) } else { mf!(self.mem.r32(a[0])) as u64 };
                    let t = &mut self.threads[i];
                    t.saved_mask = Some(t.sigmask);
                    t.sigmask = m & !((1 << 8) | (1 << 18));
                }
                let t = &mut self.threads[i];
                t.wait = Wait::Sleep;
                t.deadline = None;
                t.wake_val = -EINTR;
                // A signal may already be deliverable under the new mask.
                if t.pending & !t.sigmask != 0 {
                    t.wait = Wait::None;
                    return Ret(-EINTR);
                }
                Blocked
            }
            186 => Ret(0), // sigaltstack (not honoured)
            173 => self.sys_rt_sigreturn(i),
            119 => self.sys_sigreturn(i),
            177 => neg(ENOSYS),

            // ---- polling ----
            168 | 309 => self.sys_poll(i, a, nr == 309),
            142 | 82 | 308 => self.sys_select(i, nr, a),

            // known-harmless stubs
            435 => neg(ENOSYS), // clone3: glibc falls back to clone
            0x1000.. => neg(ENOSYS),
            _ => {
                self.unimpl_log.push(format!("syscall {} ({:x?})", nr, a));
                if self.trace_syscalls {
                    crate::log::log(&format!("[sys] UNIMPLEMENTED nr={} args={:x?}", nr, a));
                }
                neg(ENOSYS)
            }
        }
    }

    fn dup_fd(&mut self, fd: i32, min: usize) -> i32 {
        let Some(Some(f)) = self.fds.get(fd as usize) else { return -EBADF };
        let copy = match f {
            Fd::Console(n) => Fd::Console(*n),
            Fd::File { ino, off, flags } => Fd::File { ino: *ino, off: *off, flags: *flags },
            Fd::Dir { ino, pos } => Fd::Dir { ino: *ino, pos: *pos },
            Fd::Dev { major, minor, flags } => Fd::Dev { major: *major, minor: *minor, flags: *flags },
            Fd::Synth { data, off } => Fd::Synth { data: data.clone(), off: *off },
            Fd::PipeR(p) => {
                let p = *p;
                self.pipes[p].readers += 1;
                Fd::PipeR(p)
            }
            Fd::PipeW(p) => {
                let p = *p;
                self.pipes[p].writers += 1;
                Fd::PipeW(p)
            }
            Fd::Sock { rx, tx } => {
                let (rx, tx) = (*rx, *tx);
                self.pipes[rx].readers += 1;
                self.pipes[tx].writers += 1;
                Fd::Sock { rx, tx }
            }
        };
        while self.fds.len() < min {
            self.fds.push(None);
        }
        for k in min..self.fds.len() {
            if self.fds[k].is_none() {
                self.fds[k] = Some(copy);
                return k as i32;
            }
        }
        self.fds.push(Some(copy));
        (self.fds.len() - 1) as i32
    }

    fn dup2_fd(&mut self, old: i32, new: usize) -> SysRes {
        if old as usize == new {
            return Ret(new as i32);
        }
        if self.fds.get(old as usize).and_then(|f| f.as_ref()).is_none() {
            return neg(EBADF);
        }
        if new < self.fds.len() {
            self.fds[new] = None;
        }
        let fd = self.dup_fd(old, new);
        Ret(fd)
    }

    // ---- futex -----------------------------------------------------------------------------
    fn sys_futex(&mut self, i: usize, a: [u32; 6], wide_time: bool) -> SysRes {
        let (uaddr, op, val, timeout, uaddr2, val3) = (a[0], a[1], a[2], a[3], a[4], a[5]);
        let cmd = op & 127;
        let realtime = op & 256 != 0;
        match cmd {
            0 | 9 => {
                // FUTEX_WAIT / FUTEX_WAIT_BITSET
                let cur = mf!(self.mem.r32(uaddr));
                if cur != val {
                    return neg(EAGAIN);
                }
                let bitset = if cmd == 9 { val3 } else { u32::MAX };
                if bitset == 0 {
                    return neg(EINVAL);
                }
                let mut deadline = None;
                if timeout != 0 {
                    let (sec, nsec) = if wide_time { (mf!(self.mem.r64(timeout)), mf!(self.mem.r64(timeout + 8))) } else { (mf!(self.mem.r32(timeout)) as u64, mf!(self.mem.r32(timeout + 4)) as u64) };
                    let dur = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
                    let now = self.mono_ns();
                    deadline = Some(if cmd == 9 {
                        let clk = if realtime { self.real_ns() } else { now };
                        now + dur.saturating_sub(clk)
                    } else {
                        now + dur
                    });
                }
                let t = &mut self.threads[i];
                t.wait = Wait::Futex { addr: uaddr, bitset };
                t.deadline = deadline;
                t.wake_val = 0;
                Blocked
            }
            1 | 10 => {
                let bitset = if cmd == 10 { val3 } else { u32::MAX };
                Ret(self.futex_wake(uaddr, val, bitset) as i32)
            }
            3 | 4 => {
                // FUTEX_REQUEUE / CMP_REQUEUE: wake `val`, move up to `timeout` (val2) waiters to uaddr2
                if cmd == 4 {
                    let cur = mf!(self.mem.r32(uaddr));
                    if cur != val3 {
                        return neg(EAGAIN);
                    }
                }
                let woken = self.futex_wake(uaddr, val, u32::MAX);
                let mut moved = 0;
                for t in self.threads.iter_mut() {
                    if moved >= timeout {
                        break;
                    }
                    if let Wait::Futex { addr, bitset } = t.wait {
                        if addr == uaddr {
                            t.wait = Wait::Futex { addr: uaddr2, bitset };
                            moved += 1;
                        }
                    }
                }
                Ret((woken + moved) as i32)
            }
            5 => {
                // FUTEX_WAKE_OP: val3 encodes op/cmp
                let enc = val3;
                let op_ = (enc >> 28) & 0xf;
                let cmp = (enc >> 24) & 0xf;
                let mut oparg = (enc >> 12) & 0xfff;
                let mut cmparg = enc & 0xfff;
                if oparg & 0x800 != 0 { oparg |= 0xffff_f000; }
                if cmparg & 0x800 != 0 { cmparg |= 0xffff_f000; }
                let oldval = mf!(self.mem.r32(uaddr2));
                let oparg = if op_ & 8 != 0 { 1u32 << (oparg & 31) } else { oparg };
                let newval = match op_ & 7 {
                    0 => oparg,
                    1 => oldval.wrapping_add(oparg),
                    2 => oldval | oparg,
                    3 => oldval & !oparg,
                    4 => oldval ^ oparg,
                    _ => return neg(ENOSYS),
                };
                mf!(self.mem.w32(uaddr2, newval));
                let mut n = self.futex_wake(uaddr, val, u32::MAX);
                let ok = match cmp {
                    0 => oldval == cmparg,
                    1 => oldval != cmparg,
                    2 => (oldval as i32) < (cmparg as i32),
                    3 => (oldval as i32) <= (cmparg as i32),
                    4 => (oldval as i32) > (cmparg as i32),
                    5 => (oldval as i32) >= (cmparg as i32),
                    _ => false,
                };
                if ok {
                    n += self.futex_wake(uaddr2, timeout, u32::MAX);
                }
                Ret(n as i32)
            }
            _ => neg(ENOSYS),
        }
    }

    fn sys_clone(&mut self, i: usize, a: [u32; 6]) -> SysRes {
        let (flags, stack, ptid, tls, ctid) = (a[0], a[1], a[2], a[3], a[4]);
        if flags & CLONE_VM == 0 || flags & CLONE_THREAD == 0 {
            return neg(ENOSYS); // fork-style clone unsupported
        }
        let tid = self.next_tid;
        self.next_tid += 1;
        let mut cpu = self.threads[i].cpu.clone();
        cpu.jit_lir_completed = 0;
        cpu.r[EAX] = 0;
        if stack != 0 {
            cpu.r[ESP] = stack;
        }
        if flags & CLONE_SETTLS != 0 {
            let entry = mf!(self.mem.r32(tls));
            let base = mf!(self.mem.r32(tls + 4));
            let e = if entry == u32::MAX { 6 } else { entry };
            if !(6..9).contains(&e) {
                return neg(EINVAL);
            }
            cpu.tls_base[(e - 6) as usize] = base;
            cpu.gs_base = base; // child's %gs selector is inherited and points at this entry
            cpu.seg[5] = ((e << 3) | 3) as u16;
        }
        if flags & CLONE_PARENT_SETTID != 0 {
            mf!(self.mem.w32(ptid, tid));
        }
        if flags & CLONE_CHILD_SETTID != 0 {
            mf!(self.mem.w32(ctid, tid));
        }
        let clear = if flags & CLONE_CHILD_CLEARTID != 0 { ctid } else { 0 };
        let mask = self.threads[i].sigmask;
        self.threads.push(Thread { tid, cpu, wait: Wait::None, deadline: None, wake_val: 0, clear_child_tid: clear, robust_list: 0, zombie: false, ins: 0, retry_deadline: None, sigmask: mask, pending: 0, sig_info: [(0, 0, 0); 65], saved_mask: None, fp_saves: Vec::new() });
        Ret(tid as i32)
    }

    // ---- poll / select ---------------------------------------------------------------------
    fn fd_ready(&self, fd: i32) -> (bool, bool) {
        // (readable, writable)
        match self.fds.get(fd as usize).and_then(|f| f.as_ref()) {
            Some(Fd::PipeR(p)) => (!self.pipes[*p].buf.is_empty() || self.pipes[*p].writers == 0, false),
            Some(Fd::PipeW(_)) => (false, true),
            Some(Fd::Sock { rx, tx }) => (!self.pipes[*rx].buf.is_empty() || self.pipes[*rx].writers == 0, self.pipes[*tx].readers > 0),
            Some(_) => (true, true),
            None => (false, false),
        }
    }

    fn timeout_deadline(&mut self, i: usize, ms: Option<u64>) -> Option<u64> {
        if let Some(d) = self.threads[i].retry_deadline {
            return Some(d);
        }
        let d = ms.map(|m| self.mono_ns() + m * 1_000_000);
        self.threads[i].retry_deadline = d;
        d
    }

    fn block_retry(&mut self, i: usize, deadline: Option<u64>) -> SysRes {
        self.threads[i].deadline = deadline;
        Retry
    }

    fn sys_poll(&mut self, i: usize, a: [u32; 6], is_ppoll: bool) -> SysRes {
        let (fds, nfds) = (a[0], a[1]);
        let timeout_ms: Option<u64> = if is_ppoll {
            if a[2] == 0 { None } else {
                let s = mf!(self.mem.r32(a[2])) as u64;
                let n = mf!(self.mem.r32(a[2] + 4)) as u64;
                Some(s * 1000 + n / 1_000_000)
            }
        } else if (a[2] as i32) < 0 { None } else { Some(a[2] as u64) };
        let mut ready = 0;
        for k in 0..nfds {
            let base = fds + k * 8;
            let fd = mf!(self.mem.r32(base)) as i32;
            let ev = mf!(self.mem.r16(base + 4));
            let (r, w) = self.fd_ready(fd);
            let mut rev = 0u16;
            if fd >= 0 && self.fds.get(fd as usize).map_or(true, |f| f.is_none()) {
                rev = 0x20; // POLLNVAL
            } else {
                if ev & 1 != 0 && r { rev |= 1; }
                if ev & 4 != 0 && w { rev |= 4; }
                if let Some(Some(Fd::PipeR(p))) = self.fds.get(fd as usize) {
                    if self.pipes[*p].writers == 0 && self.pipes[*p].buf.is_empty() { rev |= 0x10; }
                }
            }
            mf!(self.mem.w16(base + 6, rev));
            if rev != 0 { ready += 1; }
        }
        if ready > 0 { return Ret(ready); }
        let dl = self.timeout_deadline(i, timeout_ms);
        if let Some(d) = dl {
            if self.mono_ns() >= d { return Ret(0); }
        }
        self.block_retry(i, dl)
    }

    fn sys_select(&mut self, i: usize, nr: u32, a: [u32; 6]) -> SysRes {
        // old select(82) takes a pointer to 5 args; _newselect(142) and pselect6(308) pass them directly.
        let (n, rd, wr, ex, tv) = if nr == 82 {
            let p = a[0];
            (mf!(self.mem.r32(p)), mf!(self.mem.r32(p + 4)), mf!(self.mem.r32(p + 8)), mf!(self.mem.r32(p + 12)), mf!(self.mem.r32(p + 16)))
        } else {
            (a[0], a[1], a[2], a[3], a[4])
        };
        let timeout_ms: Option<u64> = if tv == 0 { None } else {
            let s = mf!(self.mem.r32(tv)) as u64;
            let u = mf!(self.mem.r32(tv + 4)) as u64;
            Some(if nr == 308 { s * 1000 + u / 1_000_000 } else { s * 1000 + u / 1000 })
        };
        let words = ((n + 31) / 32) as usize;
        let load = |k: &Kernel, p: u32| -> Result<Vec<u32>, ()> {
            if p == 0 { return Ok(vec![0; words]); }
            (0..words).map(|w| k.mem.r32(p + w as u32 * 4).map_err(|_| ())).collect()
        };
        let rin = mf!(load(self, rd));
        let win = mf!(load(self, wr));
        let mut rout = vec![0u32; words];
        let mut wout = vec![0u32; words];
        let mut ready = 0;
        for fd in 0..n as usize {
            let (w, b) = (fd / 32, fd % 32);
            let (r, wri) = self.fd_ready(fd as i32);
            if rin[w] >> b & 1 != 0 && r { rout[w] |= 1 << b; ready += 1; }
            if win[w] >> b & 1 != 0 && wri { wout[w] |= 1 << b; ready += 1; }
        }
        if ready > 0 || n == 0 && timeout_ms == Some(0) {
            for (p, v) in [(rd, &rout), (wr, &wout)] {
                if p != 0 { for (k, x) in v.iter().enumerate() { mf!(self.mem.w32(p + k as u32 * 4, *x)); } }
            }
            if ex != 0 { for k in 0..words { mf!(self.mem.w32(ex + k as u32 * 4, 0)); } }
            return Ret(ready);
        }
        let dl = self.timeout_deadline(i, timeout_ms);
        if let Some(d) = dl {
            if self.mono_ns() >= d {
                for p in [rd, wr, ex] { if p != 0 { for k in 0..words { mf!(self.mem.w32(p + k as u32 * 4, 0)); } } }
                return Ret(0);
            }
        }
        self.block_retry(i, dl)
    }
}

#[allow(non_snake_case)]
fn EAFAULT_OR_NOSYS(_call: u32) -> i32 {
    ENOSYS
}

/// sched_yield: end the slice by treating it as an immediately-satisfied Retry on the next event.
#[allow(non_snake_case)]
fn Retry_yield() -> SysRes {
    Ret(0)
}

// ---- embedder-facing helpers (used by the JS Pepper host through wasm_api) ----------------------
impl Kernel {
    /// Open `path` in the guest VFS and install the descriptor in the process fd table.
    /// Returns the guest fd, or -errno.
    pub fn host_open(&mut self, path: &str, flags: u32, mode: u32) -> i32 {
        self.crate_stdio();
        match self.do_open(path, flags, mode) {
            Ret(v) => v,
            _ => -ENOSYS,
        }
    }

    /// (mode, size, mtime) or -errno.
    pub fn host_stat(&self, path: &str) -> Result<(u32, u64, i64), i32> {
        let s = self.stat_path(path, true)?;
        Ok((s.mode, s.size, s.mtime))
    }

    /// Directory entries as (name, is_dir).
    pub fn host_readdir(&self, path: &str) -> Result<Vec<(String, bool)>, i32> {
        let ino = self.vfs.lookup(path, true)?;
        match self.vfs.get(ino) {
            Some(Inode { kind: Kind::Dir(m), .. }) => Ok(m.iter().map(|(n, &c)| (n.clone(), matches!(self.vfs.get(c).map(|i| i.mode & S_IFMT), Some(S_IFDIR)))).collect()),
            _ => Err(ENOTDIR),
        }
    }

    pub fn host_unlink(&mut self, path: &str, recursive: bool) -> Result<(), i32> {
        let ino = self.vfs.lookup(path, false)?;
        if let Some(Inode { kind: Kind::Dir(m), .. }) = self.vfs.get(ino) {
            if !m.is_empty() && !recursive {
                return Err(ENOTEMPTY);
            }
        }
        self.vfs.unlink(path)
    }

    pub fn host_read_file(&self, path: &str) -> Result<Vec<u8>, i32> {
        let ino = self.vfs.lookup(path, true)?;
        match self.vfs.get(ino) {
            Some(Inode { kind: Kind::File(d), .. }) => Ok(d.clone()),
            _ => Err(EISDIR),
        }
    }

    /// Serialize every regular file below `prefix` as a flat list (path, bytes) for persistence.
    pub fn host_dump_tree(&self, prefix: &str) -> Vec<(String, Vec<u8>)> {
        fn walk(v: &Vfs, ino: u32, path: String, out: &mut Vec<(String, Vec<u8>)>) {
            match v.get(ino) {
                Some(Inode { kind: Kind::Dir(m), .. }) => {
                    for (n, &c) in m {
                        walk(v, c, format!("{}/{}", path, n), out);
                    }
                }
                Some(Inode { kind: Kind::File(d), .. }) => out.push((path, d.clone())),
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Ok(ino) = self.vfs.lookup(prefix, true) {
            walk(&self.vfs, ino, prefix.trim_end_matches('/').to_string(), &mut out);
        }
        out
    }
}
