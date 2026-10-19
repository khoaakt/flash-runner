//! IA-32 user-mode CPU state, flag arithmetic, register and effective-address helpers.

use crate::mem::Segv;
use iced_x86::{Instruction, Register};

pub const CF: u32 = 1 << 0;
pub const PF: u32 = 1 << 2;
pub const AF: u32 = 1 << 4;
pub const ZF: u32 = 1 << 6;
pub const SF: u32 = 1 << 7;
pub const TF: u32 = 1 << 8;
pub const IF: u32 = 1 << 9;
pub const DF: u32 = 1 << 10;
pub const OF: u32 = 1 << 11;
pub const ARITH: u32 = CF | PF | AF | ZF | SF | OF;
/// Bits a user-mode POPF may change.
pub const POPF_MASK: u32 = ARITH | DF | TF | (1 << 14) | (1 << 18) | (1 << 21);

pub const EAX: usize = 0;
pub const ECX: usize = 1;
pub const EDX: usize = 2;
pub const EBX: usize = 3;
pub const ESP: usize = 4;
pub const EBP: usize = 5;
pub const ESI: usize = 6;
pub const EDI: usize = 7;

/// Why `Cpu::run` stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum Exit {
    /// Instruction budget exhausted (cooperative time slice).
    Budget,
    /// `int 0x80`.
    Syscall,
    /// Guest executed `hlt`.
    Halt,
    Segv(Segv),
    /// `#UD` / unsupported encoding.
    Undef { eip: u32 },
    /// Decoded but not implemented by this emulator yet; carries the mnemonic for triage.
    Unimpl { eip: u32, what: String },
    DivideError { eip: u32 },
    /// `bound` index outside the bounds operand (`#BR`); Linux delivers SIGSEGV at the instruction.
    Bounds { eip: u32 },
    Breakpoint,
    /// `int n` with n != 0x80.
    Int(u8),
    /// Hypercall from the pepper bridge (`int 0x82`).
    Hyper,
    /// Non-mutating host observation before executing a verified guest entry point.
    Probe { id: u32, eip: u32 },
}

impl From<Segv> for Exit {
    fn from(s: Segv) -> Exit {
        Exit::Segv(s)
    }
}

pub type XResult<T> = Result<T, Exit>;

#[derive(Clone, Default)]
#[repr(C)]
pub struct Cpu {
    pub r: [u32; 8],
    pub eip: u32,
    pub fl: u32,
    pub seg: [u16; 6],
    pub fs_base: u32,
    pub gs_base: u32,
    /// Base addresses of the three TLS GDT slots (entries 6..8) set by set_thread_area.
    pub tls_base: [u32; 3],
    pub xmm: [u128; 8],
    pub mxcsr: u32,
    pub fpu: crate::fpu::Fpu,
    pub tsc: u64,
    /// CPUID.1:EDX bits advertised to the guest. SSE/SSE2 are switched on once the corresponding
    /// instructions are implemented, so glibc's ifunc resolvers never pick a variant we cannot run.
    pub cpuid_edx: u32,
    // ---- JIT state (see jit.rs). ----
    /// Lazy arithmetic flags left by compiled code (kind, a, b, result); `lf_kind == 0` means `fl` is current.
    /// Invariant: outside `Cpu::run` (and at the top of every `exec`) `lf_kind` is 0.
    pub lf_kind: u32,
    pub lf_a: u32,
    pub lf_b: u32,
    pub lf_r: u32,
    /// Remaining instruction budget as seen by compiled code (decremented per translated block).
    pub jit_budget: i32,
    /// Set to 1 by the slow-path memory helpers when the access faulted.
    pub jit_fault: u32,
    pub jit_fault_addr: u32,
    pub jit_fault_write: u32,
    /// Extra payload for some exit statuses (interrupt number, ...).
    pub jit_aux: u32,
    /// Resume one observed entry without firing the same probe again (bit 32 = valid).
    pub jit_probe_resume: u64,
    /// Incremented by a native-ABI LIR wrapper only after successful commit, including tail calls.
    pub jit_lir_completed: u64,
}

#[derive(Clone, Copy)]
pub enum Loc {
    Reg(Register),
    Mem(u32, u8),
}

#[inline(always)]
pub fn mask(sz: u8) -> u32 {
    match sz {
        1 => 0xff,
        2 => 0xffff,
        _ => 0xffff_ffff,
    }
}
#[inline(always)]
pub fn sign_bit(sz: u8) -> u32 {
    match sz {
        1 => 0x80,
        2 => 0x8000,
        _ => 0x8000_0000,
    }
}
#[inline(always)]
pub fn sext(v: u32, sz: u8) -> i32 {
    match sz {
        1 => v as u8 as i8 as i32,
        2 => v as u16 as i16 as i32,
        _ => v as i32,
    }
}

impl Cpu {
    pub fn new() -> Cpu {
        let mut c = Cpu::default();
        c.fl = 0x202; // IF + reserved bit 1
        c.mxcsr = 0x1f80;
        c.fpu = crate::fpu::Fpu::new();
        c.cpuid_edx = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15);
        c.seg = [0x2b, 0x23, 0x2b, 0x2b, 0, 0x33]; // ES CS SS DS FS GS (flat user selectors; values are cosmetic)
        c
    }

    // ---- flags -----------------------------------------------------------------------------
    /// Fold the lazy flag tuple left by compiled code into `fl`.
    #[inline(always)]
    pub fn lf_flush(&mut self) {
        if self.lf_kind != 0 {
            self.lf_flush_slow();
        }
    }
    #[cold]
    #[inline(never)]
    pub fn lf_flush_slow(&mut self) {
        let bits = crate::jit_flags::eval(self.lf_kind, self.lf_a, self.lf_b, self.lf_r);
        self.fl = (self.fl & !ARITH) | bits;
        self.lf_kind = 0;
    }

    #[inline(always)]
    pub fn flag(&self, f: u32) -> bool {
        self.fl & f != 0
    }
    #[inline(always)]
    pub fn set_flag(&mut self, f: u32, v: bool) {
        if v {
            self.fl |= f
        } else {
            self.fl &= !f
        }
    }
    #[inline(always)]
    pub fn set_szp(&mut self, res: u32, sz: u8) {
        let r = res & mask(sz);
        self.set_flag(ZF, r == 0);
        self.set_flag(SF, r & sign_bit(sz) != 0);
        self.set_flag(PF, (r as u8).count_ones() & 1 == 0);
    }

    // ---- GPR access ------------------------------------------------------------------------
    #[inline(always)]
    pub fn get_reg(&self, r: Register) -> u32 {
        if r.is_gpr32() {
            self.r[r.number()]
        } else if r.is_gpr16() {
            self.r[r.number()] & 0xffff
        } else if r.is_gpr8() {
            let n = r.number();
            if n < 4 {
                self.r[n] & 0xff
            } else {
                (self.r[n - 4] >> 8) & 0xff
            }
        } else if r.is_segment_register() {
            self.seg[r.number()] as u32
        } else {
            0
        }
    }

    #[inline(always)]
    pub fn set_reg(&mut self, r: Register, v: u32) {
        if r.is_gpr32() {
            self.r[r.number()] = v;
        } else if r.is_gpr16() {
            let i = r.number();
            self.r[i] = (self.r[i] & 0xffff_0000) | (v & 0xffff);
        } else if r.is_gpr8() {
            let n = r.number();
            if n < 4 {
                self.r[n] = (self.r[n] & !0xff) | (v & 0xff);
            } else {
                self.r[n - 4] = (self.r[n - 4] & !0xff00) | ((v & 0xff) << 8);
            }
        } else if r.is_segment_register() {
            self.seg[r.number()] = v as u16;
        }
    }

    #[inline(always)]
    pub fn reg_size(r: Register) -> u8 {
        if r.is_gpr32() {
            4
        } else if r.is_gpr16() || r.is_segment_register() {
            2
        } else {
            1
        }
    }

    // ---- effective address -----------------------------------------------------------------
    #[inline(always)]
    pub fn seg_base(&self, ins: &Instruction) -> u32 {
        match ins.memory_segment() {
            Register::FS => self.fs_base,
            Register::GS => self.gs_base,
            _ => 0,
        }
    }

    /// Linear address of the instruction's memory operand. 16-bit addressing is not supported.
    #[inline(always)]
    pub fn ea(&self, ins: &Instruction) -> XResult<u32> {
        let mut a = ins.memory_displacement32();
        let b = ins.memory_base();
        if b != Register::None {
            if !b.is_gpr32() {
                return Err(Exit::Undef { eip: (ins.ip() as u32) });
            }
            a = a.wrapping_add(self.r[b.number()]);
        }
        let i = ins.memory_index();
        if i != Register::None {
            if !i.is_gpr32() {
                return Err(Exit::Undef { eip: (ins.ip() as u32) });
            }
            a = a.wrapping_add(self.r[i.number()].wrapping_mul(ins.memory_index_scale()));
        }
        Ok(a.wrapping_add(self.seg_base(ins)))
    }

    // ---- ALU -------------------------------------------------------------------------------
    /// a + b + carry with full IA-32 flag semantics for operand size `sz`.
    pub fn add_flags(&mut self, a: u32, b: u32, carry: u32, sz: u8) -> u32 {
        let m = mask(sz);
        let (a, b) = (a & m, b & m);
        let wide = a as u64 + b as u64 + carry as u64;
        let res = (wide as u32) & m;
        self.set_flag(CF, wide > m as u64);
        self.set_flag(AF, (a ^ b ^ res) & 0x10 != 0);
        self.set_flag(OF, ((a ^ res) & (b ^ res) & sign_bit(sz)) != 0);
        self.set_szp(res, sz);
        res
    }

    /// a - b - borrow with full IA-32 flag semantics.
    pub fn sub_flags(&mut self, a: u32, b: u32, borrow: u32, sz: u8) -> u32 {
        let m = mask(sz);
        let (a, b) = (a & m, b & m);
        let res = a.wrapping_sub(b).wrapping_sub(borrow) & m;
        self.set_flag(CF, (a as u64) < (b as u64 + borrow as u64));
        self.set_flag(AF, (a ^ b ^ res) & 0x10 != 0);
        self.set_flag(OF, ((a ^ b) & (a ^ res) & sign_bit(sz)) != 0);
        self.set_szp(res, sz);
        res
    }

    pub fn logic_flags(&mut self, res: u32, sz: u8) -> u32 {
        let res = res & mask(sz);
        self.set_flag(CF, false);
        self.set_flag(OF, false);
        self.set_flag(AF, false);
        self.set_szp(res, sz);
        res
    }

    /// Evaluate a condition code index (0..16, Intel order: O NO B AE E NE BE A S NS P NP L GE LE G).
    pub fn cond(&self, cc: u8) -> bool {
        let r = match cc >> 1 {
            0 => self.flag(OF),
            1 => self.flag(CF),
            2 => self.flag(ZF),
            3 => self.flag(CF) || self.flag(ZF),
            4 => self.flag(SF),
            5 => self.flag(PF),
            6 => self.flag(SF) != self.flag(OF),
            _ => self.flag(ZF) || (self.flag(SF) != self.flag(OF)),
        };
        r ^ (cc & 1 != 0)
    }

    // ---- stack -----------------------------------------------------------------------------
    #[inline(always)]
    pub fn push32(&mut self, m: &mut crate::mem::Memory, v: u32) -> XResult<()> {
        let sp = self.r[ESP].wrapping_sub(4);
        m.w32(sp, v)?;
        self.r[ESP] = sp;
        Ok(())
    }
    #[inline(always)]
    pub fn pop32(&mut self, m: &crate::mem::Memory) -> XResult<u32> {
        let v = m.r32(self.r[ESP])?;
        self.r[ESP] = self.r[ESP].wrapping_add(4);
        Ok(v)
    }
    pub fn push16(&mut self, m: &mut crate::mem::Memory, v: u16) -> XResult<()> {
        let sp = self.r[ESP].wrapping_sub(2);
        m.w16(sp, v)?;
        self.r[ESP] = sp;
        Ok(())
    }
    pub fn pop16(&mut self, m: &crate::mem::Memory) -> XResult<u16> {
        let v = m.r16(self.r[ESP])?;
        self.r[ESP] = self.r[ESP].wrapping_add(2);
        Ok(v)
    }
}
