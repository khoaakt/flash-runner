//! Signal delivery (i386 sigframe / rt_sigframe) and sigreturn.

use crate::cpu::*;
use crate::kernel::*;
use crate::sys::SysRes;

const SA_SIGINFO: u32 = 4;
const SA_NODEFER: u32 = 0x4000_0000;
const SA_RESETHAND: u32 = 0x8000_0000;

const SIGCHLD: u32 = 17;
const SIGURG: u32 = 23;
const SIGWINCH: u32 = 28;
const SIGCONT: u32 = 18;

fn bit(sig: u32) -> u64 {
    1u64 << (sig - 1)
}

impl Kernel {
    /// kill/tgkill target thread `ti`.
    pub fn post_signal(&mut self, ti: usize, sig: u32, code: i32, pid: u32, uid: u32) -> SysRes {
        if sig > 64 {
            return SysRes::Ret(-EINVAL_);
        }
        let h = self.sigactions[sig as usize].handler;
        if h == 1 {
            return SysRes::Ret(0); // SIG_IGN
        }
        if h == 0 {
            if matches!(sig, SIGCHLD | SIGURG | SIGWINCH | SIGCONT) {
                return SysRes::Ret(0);
            }
            return SysRes::ExitProcess(128 + sig as i32);
        }
        let t = &mut self.threads[ti];
        t.pending |= bit(sig);
        t.sig_info[sig as usize] = (code, pid, uid);
        if t.pending & !t.sigmask != 0 {
            // Interrupt a blocked syscall so the handler runs promptly; the syscall returns EINTR.
            match t.wait {
                Wait::None => {}
                Wait::Retry { .. } => {
                    // The pending `int 0x80` had been rewound for re-execution: step back over it instead.
                    t.wait = Wait::None;
                    t.deadline = None;
                    t.retry_deadline = None;
                    t.cpu.eip = t.cpu.eip.wrapping_add(2);
                    t.cpu.r[EAX] = (-EINTR) as u32;
                }
                _ => {
                    t.wait = Wait::None;
                    t.deadline = None;
                    t.cpu.r[EAX] = (-EINTR) as u32;
                }
            }
        }
        self.generation += 1;
        SysRes::Ret(0)
    }

    pub fn deliver_pending(&mut self, i: usize) {
        let t = &self.threads[i];
        let ready = t.pending & !t.sigmask;
        if ready == 0 {
            return;
        }
        let sig = ready.trailing_zeros() + 1;
        self.threads[i].pending &= !bit(sig);
        let info = self.threads[i].sig_info[sig as usize];
        self.deliver_signal_info(i, sig, 0, info);
    }

    /// Build a signal frame on the thread's stack and redirect it into the handler.
    pub fn deliver_signal(&mut self, i: usize, sig: u32, fault_addr: u32) -> bool {
        self.deliver_signal_info(i, sig, fault_addr, (if sig == 11 { 1 } else { 0 }, 0, 0))
    }

    pub fn deliver_signal_info(&mut self, i: usize, sig: u32, fault_addr: u32, info_in: (i32, u32, u32)) -> bool {
        if let Some(c) = self.sig_stats.get_mut(sig as usize) {
            *c += 1;
        }
        let sa = SigAction { handler: self.sigactions[sig as usize].handler, flags: self.sigactions[sig as usize].flags, restorer: self.sigactions[sig as usize].restorer, mask: self.sigactions[sig as usize].mask };
        if sa.handler <= 1 {
            return false;
        }
        let rt = sa.flags & SA_SIGINFO != 0;
        let cpu = self.threads[i].cpu.clone();
        let old_mask = self.threads[i].saved_mask.take().unwrap_or(self.threads[i].sigmask);
        let size: u32 = if rt { 260 } else { 216 };
        let mut sp = (cpu.r[ESP].wrapping_sub(size)) & !0xf;
        sp = sp.wrapping_sub(4);
        let ctx_off: u32 = if rt { 164 } else { 8 };
        let words = [
            cpu.seg[5] as u32, cpu.seg[4] as u32, cpu.seg[0] as u32, cpu.seg[3] as u32,
            cpu.r[EDI], cpu.r[ESI], cpu.r[EBP], cpu.r[ESP], cpu.r[EBX], cpu.r[EDX], cpu.r[ECX], cpu.r[EAX],
            if sig == 11 { 14 } else { 0 }, if sig == 11 { 6 } else { 0 }, cpu.eip, cpu.seg[1] as u32, cpu.fl, cpu.r[ESP], cpu.seg[2] as u32, 0, old_mask as u32, fault_addr,
        ];
        let m = &mut self.mem;
        let ok = (|| -> Result<(), crate::mem::Segv> {
            m.w32(sp, sa.restorer)?;
            m.w32(sp + 4, sig)?;
            if rt {
                m.w32(sp + 8, sp + 16)?;
                m.w32(sp + 12, sp + 144)?;
                // siginfo
                let mut info = [0u8; 128];
                info[0..4].copy_from_slice(&sig.to_le_bytes());
                info[8..12].copy_from_slice(&info_in.0.to_le_bytes());
                if sig == 11 || sig == 7 || sig == 4 || sig == 8 {
                    info[12..16].copy_from_slice(&fault_addr.to_le_bytes()); // si_addr
                } else {
                    info[12..16].copy_from_slice(&info_in.1.to_le_bytes()); // si_pid
                    info[16..20].copy_from_slice(&info_in.2.to_le_bytes()); // si_uid
                }
                m.write_bytes(sp + 16, &info)?;
                m.w32(sp + 144, 0)?; // uc_flags
                m.w32(sp + 148, 0)?; // uc_link
                m.w32(sp + 252, old_mask as u32)?;
                m.w32(sp + 256, (old_mask >> 32) as u32)?;
            }
            for (k, w) in words.iter().enumerate() {
                m.w32(sp + ctx_off + k as u32 * 4, *w)?;
            }
            Ok(())
        })();
        if ok.is_err() {
            return false;
        }
        if !rt {
            // legacy frame keeps the high 32 mask bits in `extramask`
            if self.mem.w32(sp + 208, (old_mask >> 32) as u32).is_err() {
                return false;
            }
        }
        let t = &mut self.threads[i];
        if t.fp_saves.len() >= 16 {
            t.fp_saves.remove(0); // handlers that siglongjmp out never return here; bound the leak
        }
        t.fp_saves.push((t.cpu.fpu.clone(), t.cpu.xmm, t.cpu.mxcsr));
        t.cpu.fpu.init(); // a handler starts with a clean x87 state, like the kernel does
        t.cpu.r[ESP] = sp;
        t.cpu.eip = sa.handler;
        t.cpu.fl &= !(DF | TF);
        let add = sa.mask | if sa.flags & SA_NODEFER != 0 { 0 } else { bit(sig) };
        t.sigmask |= add;
        if sa.flags & SA_RESETHAND != 0 {
            self.sigactions[sig as usize].handler = 0;
        }
        true
    }

    pub fn sys_rt_sigreturn(&mut self, i: usize) -> SysRes {
        // `ret` out of the handler popped pretcode, so ESP points at the `sig` slot.
        // nr 173 -> rt frame (esp-4), nr 119 -> legacy frame (esp-8); the caller passes the rt layout.
        let esp = self.threads[i].cpu.r[ESP];
        let frame = esp.wrapping_sub(4);
        self.restore_frame(i, frame, true)
    }

    pub fn sys_sigreturn(&mut self, i: usize) -> SysRes {
        let esp = self.threads[i].cpu.r[ESP];
        let frame = esp.wrapping_sub(8);
        self.restore_frame(i, frame, false)
    }

    fn restore_frame(&mut self, i: usize, frame: u32, rt: bool) -> SysRes {
        let off = if rt { 164 } else { 8 };
        let mut w = [0u32; 22];
        for (k, slot) in w.iter_mut().enumerate() {
            match self.mem.r32(frame + off + k as u32 * 4) {
                Ok(v) => *slot = v,
                Err(_) => return SysRes::ExitProcess(139),
            }
        }
        if rt {
            if let (Ok(lo), Ok(hi)) = (self.mem.r32(frame + 252), self.mem.r32(frame + 256)) {
                self.threads[i].sigmask = (lo as u64) | ((hi as u64) << 32);
            }
        } else if let (Ok(lo), Ok(hi)) = (self.mem.r32(frame + off + 80), self.mem.r32(frame + 208)) {
            self.threads[i].sigmask = (lo as u64) | ((hi as u64) << 32);
        }
        if let Some((fpu, xmm, mxcsr)) = self.threads[i].fp_saves.pop() {
            let c = &mut self.threads[i].cpu;
            c.fpu = fpu;
            c.xmm = xmm;
            c.mxcsr = mxcsr;
        }
        let c = &mut self.threads[i].cpu;
        c.seg[5] = w[0] as u16;
        c.seg[4] = w[1] as u16;
        c.r[EDI] = w[4];
        c.r[ESI] = w[5];
        c.r[EBP] = w[6];
        c.r[ESP] = w[7];
        c.r[EBX] = w[8];
        c.r[EDX] = w[9];
        c.r[ECX] = w[10];
        c.r[EAX] = w[11];
        c.eip = w[14];
        c.fl = (w[16] & POPF_MASK) | 2 | IF;
        // Re-derive segment bases from the restored selectors.
        let slot5 = (w[0] >> 3) as i32 - 6;
        if (0..3).contains(&slot5) {
            c.gs_base = c.tls_base[slot5 as usize];
        }
        SysRes::NoRet
    }
}

const EINVAL_: i32 = 22;
