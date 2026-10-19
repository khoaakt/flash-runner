//! Flat 32-bit guest memory.
//!
//! Guest address `a` lives at `data[a]`. The kernel layer decides what is mapped (it owns the mmap
//! allocator); this layer only enforces per-page R/W/X and tracks pages that contain decoded code
//! so stores into them invalidate the block cache (Flash's AVM2 JIT writes machine code at runtime).

pub const PAGE_SHIFT: u32 = 12;
pub const PAGE_SIZE: u32 = 1 << PAGE_SHIFT;

pub const P_R: u8 = 1;
pub const P_W: u8 = 2;
pub const P_X: u8 = 4;
pub const P_MAPPED: u8 = 0x80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segv {
    pub addr: u32,
    pub write: bool,
}

pub type MResult<T> = Result<T, Segv>;

/// Number of pages in the 32-bit guest address space. The JIT fast-path tables always cover all of
/// them so a lookup needs no bounds check: pages beyond the RAM size simply read as 0 (slow path).
pub const ALL_PAGES: usize = 1 << (32 - PAGE_SHIFT);

pub struct Memory {
    data: Vec<u8>,
    prot: Vec<u8>,
    /// Non-zero when the page has had instructions decoded from it.
    code: Vec<u8>,
    /// JIT fast-path tables (derived from `prot`/`code`, see `upd_tab`): `rtab[pg] & 1` iff loads from the page may
    /// skip the checked path; `wtab[pg] & 1` iff stores may (page is R+W and holds no decoded code). Bit 1 is set
    /// when the *next* page qualifies as well, so an access that starts on the page needs no end-of-page test.
    /// `rtab[pg] & 4` iff the page is readable and has had instructions decoded from it (so it is executable).
    rtab: Vec<u8>,
    wtab: Vec<u8>,
    /// Byte ranges written while their page was flagged as code, (start, end); drained by the run
    /// loop, which drops exactly the decoded blocks and compiled regions those bytes overlap.
    pub dirty_code: Vec<(u32, u32)>,
    /// Pages whose protection changed while flagged as code, (page, executable-now). Protection
    /// changes do not modify bytes: compiled regions on them are *parked* (unregistered but kept)
    /// and re-registered when the page becomes executable again, instead of being recompiled.
    pub prot_code: Vec<(u32, bool)>,
    /// `data` was taken straight from `memory.grow`, not from the allocator: it must never be handed back to it.
    foreign_ram: bool,
}

/// Guest RAM, all zero. On wasm the pages come straight from `memory.grow`: the engine hands them out zero-filled and commits them only when
/// they are first touched, so the guest pays for what it uses. Asking the allocator for zeroed memory instead made it write every byte up
/// front: ~0.1 s in Node and ~0.6 s in Chrome per start for the default 768 MiB, and the whole size resident.
#[cfg(target_arch = "wasm32")]
fn zeroed_ram(size: usize) -> (Vec<u8>, bool) {
    let prev = core::arch::wasm32::memory_grow(0, size.div_ceil(65536));
    if prev != usize::MAX {
        // SAFETY: [prev * 64 KiB, +size) is new, zero-filled, owned by nobody else (the allocator never saw it); Drop for Memory forgets the Vec.
        return (unsafe { Vec::from_raw_parts((prev * 65536) as *mut u8, size, size) }, true);
    }
    (vec![0u8; size], false)
}
#[cfg(not(target_arch = "wasm32"))]
fn zeroed_ram(size: usize) -> (Vec<u8>, bool) { (vec![0u8; size], false) }

impl Drop for Memory {
    fn drop(&mut self) {
        if self.foreign_ram { std::mem::forget(std::mem::take(&mut self.data)); }
    }
}

impl Memory {
    pub fn new(size: usize) -> Memory {
        assert!(size % PAGE_SIZE as usize == 0);
        let pages = size >> PAGE_SHIFT;
        let (data, foreign_ram) = zeroed_ram(size);
        Memory {
            data,
            foreign_ram,
            prot: vec![0u8; pages],
            code: vec![0u8; pages],
            rtab: vec![0u8; ALL_PAGES],
            wtab: vec![0u8; ALL_PAGES],
            dirty_code: Vec::new(),
            prot_code: Vec::new(),
        }
    }

    /// Recompute the JIT fast-path bytes of one page. Must be called whenever `prot` or `code` of the page changes.
    #[inline]
    fn upd_tab(&mut self, pg: usize) {
        // an entry depends on its page and on the next one: both change when this page does
        self.upd_entry(pg);
        if pg > 0 {
            self.upd_entry(pg - 1);
        }
    }

    /// (loads, stores) may use the unchecked path on `pg`; false outside guest RAM.
    #[inline]
    fn fast_ok(&self, pg: usize) -> (bool, bool) {
        match self.prot.get(pg) {
            Some(&p) => {
                let r = p & P_R != 0;
                (r, r && p & P_W != 0 && self.code[pg] == 0)
            }
            None => (false, false),
        }
    }

    #[inline]
    fn upd_entry(&mut self, pg: usize) {
        let (r0, w0) = self.fast_ok(pg);
        let (r1, w1) = self.fast_ok(pg + 1);
        self.rtab[pg] = r0 as u8 | ((r0 && r1) as u8) << 1 | ((r0 && self.code[pg] != 0) as u8) << 2;
        self.wtab[pg] = w0 as u8 | ((w0 && w1) as u8) << 1;
    }

    /// The JIT fast-path tables (test harnesses mirror them into their own memory).
    pub fn jit_tables(&self) -> (&[u8], &[u8]) {
        (&self.rtab, &self.wtab)
    }

    /// Address of the guest RAM, of the load table and of the store table (wasm32: linear-memory offsets).
    pub fn jit_addrs(&self) -> (usize, usize, usize) {
        (self.data.as_ptr() as usize, self.rtab.as_ptr() as usize, self.wtab.as_ptr() as usize)
    }

    #[inline]
    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// Raw view for host-side I/O (syscalls, hypercalls). Bypasses protection.
    #[inline]
    pub fn raw(&self) -> &[u8] {
        &self.data
    }
    #[inline]
    pub fn raw_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn set_prot(&mut self, addr: u32, len: u32, p: u8) {
        let first = (addr >> PAGE_SHIFT) as usize;
        let last = ((addr as u64 + len as u64 + PAGE_SIZE as u64 - 1) >> PAGE_SHIFT) as usize;
        for pg in first..last.min(self.prot.len()) {
            self.prot[pg] = p;
            self.upd_tab(pg);
            // Decoded/compiled code on a page whose protection changes (mprotect, munmap, MAP_FIXED
            // remap) must stop being *executed* until the page is executable again - but the bytes are
            // untouched, so this is reported separately from stores: the run loop parks compiled
            // regions here and unparks them when P_X returns (docs/JIT.md).
            if self.code[pg] != 0 {
                self.prot_code.push((pg as u32, p & P_X != 0));
            }
        }
    }

    pub fn page_prot(&self, addr: u32) -> u8 {
        self.prot.get((addr >> PAGE_SHIFT) as usize).copied().unwrap_or(0)
    }

    pub fn range_mapped(&self, addr: u32, len: u32) -> bool {
        if len == 0 {
            return true;
        }
        let end = addr as u64 + len as u64;
        if end > self.data.len() as u64 {
            return false;
        }
        let first = (addr >> PAGE_SHIFT) as usize;
        let last = ((end + PAGE_SIZE as u64 - 1) >> PAGE_SHIFT) as usize;
        self.prot[first..last].iter().all(|p| p & P_MAPPED != 0)
    }

    /// Zero a range (munmap / MAP_ANONYMOUS reuse). Does not touch protection.
    /// True when the run loop must drain `dirty_code` / `prot_code` (see `sync`).
    #[inline(always)]
    pub fn needs_sync(&self) -> bool {
        !self.dirty_code.is_empty() || !self.prot_code.is_empty()
    }

    /// Pages holding decoded code are reported through `dirty_code`, like any other store into them.
    pub fn zero(&mut self, addr: u32, len: u32) {
        let s = addr as usize;
        let e = (s + len as usize).min(self.data.len());
        if s < e {
            self.note_write(s, e - s);
            self.data[s..e].fill(0);
        }
    }

    pub fn mark_code(&mut self, addr: u32, len: u32) {
        let first = (addr >> PAGE_SHIFT) as usize;
        let last = (((addr as u64 + len.max(1) as u64 - 1) >> PAGE_SHIFT) as usize).min(self.code.len() - 1);
        for pg in first..=last {
            self.code[pg] = 1;
            self.upd_tab(pg);
        }
    }

    pub fn clear_code_flag(&mut self, page: u32) {
        if let Some(c) = self.code.get_mut(page as usize) {
            *c = 0;
            self.upd_tab(page as usize);
        }
    }

    #[inline(always)]
    fn check(&self, addr: u32, len: u32, need: u8) -> MResult<usize> {
        let a = addr as usize;
        let end = a + len as usize;
        if end > self.data.len() {
            return Err(Segv { addr, write: need & P_W != 0 });
        }
        let p0 = self.prot[a >> PAGE_SHIFT as usize];
        let p1 = self.prot[(end - 1) >> PAGE_SHIFT as usize];
        if p0 & need != need || p1 & need != need {
            return Err(Segv { addr, write: need & P_W != 0 });
        }
        Ok(a)
    }

    #[inline(always)]
    fn note_write(&mut self, a: usize, len: usize) {
        let p0 = a >> PAGE_SHIFT as usize;
        let p1 = (a + len - 1) >> PAGE_SHIFT as usize;
        let flagged = if p1 == p0 {
            self.code[p0] != 0
        } else {
            (p0..=p1.min(self.code.len() - 1)).any(|pg| self.code[pg] != 0)
        };
        if flagged {
            self.dirty_code.push((a as u32, a as u32 + len as u32));
        }
    }

    // ---- reads -----------------------------------------------------------------------------
    #[inline(always)]
    pub fn r8(&self, addr: u32) -> MResult<u8> {
        let a = self.check(addr, 1, P_R)?;
        Ok(self.data[a])
    }
    #[inline(always)]
    pub fn r16(&self, addr: u32) -> MResult<u16> {
        let a = self.check(addr, 2, P_R)?;
        Ok(u16::from_le_bytes([self.data[a], self.data[a + 1]]))
    }
    #[inline(always)]
    pub fn r32(&self, addr: u32) -> MResult<u32> {
        let a = self.check(addr, 4, P_R)?;
        Ok(u32::from_le_bytes(self.data[a..a + 4].try_into().unwrap()))
    }
    #[inline(always)]
    pub fn r64(&self, addr: u32) -> MResult<u64> {
        let a = self.check(addr, 8, P_R)?;
        Ok(u64::from_le_bytes(self.data[a..a + 8].try_into().unwrap()))
    }
    #[inline(always)]
    pub fn r128(&self, addr: u32) -> MResult<[u8; 16]> {
        let a = self.check(addr, 16, P_R)?;
        Ok(self.data[a..a + 16].try_into().unwrap())
    }
    /// Read `len` (<= 4) bytes zero-extended.
    #[inline(always)]
    pub fn rn(&self, addr: u32, size: u8) -> MResult<u32> {
        match size {
            1 => self.r8(addr).map(|v| v as u32),
            2 => self.r16(addr).map(|v| v as u32),
            _ => self.r32(addr),
        }
    }

    // ---- writes ----------------------------------------------------------------------------
    #[inline(always)]
    pub fn w8(&mut self, addr: u32, v: u8) -> MResult<()> {
        let a = self.check(addr, 1, P_W)?;
        self.note_write(a, 1);
        self.data[a] = v;
        Ok(())
    }
    #[inline(always)]
    pub fn w16(&mut self, addr: u32, v: u16) -> MResult<()> {
        let a = self.check(addr, 2, P_W)?;
        self.note_write(a, 2);
        self.data[a..a + 2].copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    #[inline(always)]
    pub fn w32(&mut self, addr: u32, v: u32) -> MResult<()> {
        let a = self.check(addr, 4, P_W)?;
        self.note_write(a, 4);
        self.data[a..a + 4].copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    #[inline(always)]
    pub fn w64(&mut self, addr: u32, v: u64) -> MResult<()> {
        let a = self.check(addr, 8, P_W)?;
        self.note_write(a, 8);
        self.data[a..a + 8].copy_from_slice(&v.to_le_bytes());
        Ok(())
    }
    #[inline(always)]
    pub fn w128(&mut self, addr: u32, v: [u8; 16]) -> MResult<()> {
        let a = self.check(addr, 16, P_W)?;
        self.note_write(a, 16);
        self.data[a..a + 16].copy_from_slice(&v);
        Ok(())
    }
    #[inline(always)]
    pub fn wn(&mut self, addr: u32, size: u8, v: u32) -> MResult<()> {
        match size {
            1 => self.w8(addr, v as u8),
            2 => self.w16(addr, v as u16),
            _ => self.w32(addr, v),
        }
    }

    /// Fetch up to `buf.len()` instruction bytes; stops at the first non-executable page.
    pub fn fetch(&self, addr: u32, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            let a = addr.wrapping_add(n as u32);
            if a as usize >= self.data.len() || self.prot[(a >> PAGE_SHIFT) as usize] & P_X == 0 {
                break;
            }
            buf[n] = self.data[a as usize];
            n += 1;
        }
        n
    }

    // ---- host-side helpers (no protection check) -------------------------------------------
    pub fn read_bytes(&self, addr: u32, len: u32) -> MResult<&[u8]> {
        let a = addr as usize;
        let e = a.checked_add(len as usize).ok_or(Segv { addr, write: false })?;
        if e > self.data.len() {
            return Err(Segv { addr, write: false });
        }
        Ok(&self.data[a..e])
    }
    pub fn write_bytes(&mut self, addr: u32, src: &[u8]) -> MResult<()> {
        let a = addr as usize;
        let e = a.checked_add(src.len()).ok_or(Segv { addr, write: true })?;
        if e > self.data.len() {
            return Err(Segv { addr, write: true });
        }
        if !src.is_empty() {
            self.note_write(a, src.len());
        }
        self.data[a..e].copy_from_slice(src);
        Ok(())
    }
    pub fn read_cstr(&self, addr: u32, max: usize) -> MResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut a = addr;
        loop {
            let b = self.r8(a)?;
            if b == 0 || out.len() >= max {
                return Ok(out);
            }
            out.push(b);
            a = a.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_with_code(pages: &[usize]) -> Memory {
        let mut m = Memory::new(16 * PAGE_SIZE as usize);
        m.set_prot(0, 16 * PAGE_SIZE, P_MAPPED | P_R | P_W | P_X);
        for &p in pages {
            m.mark_code((p as u32) << PAGE_SHIFT, 1);
        }
        m
    }

    #[test]
    fn bulk_write_flags_every_page_in_range() {
        // read() into a 12 KiB buffer spanning pages 1..=3: one range covering all of them.
        let mut m = mem_with_code(&[1, 2, 3]);
        m.write_bytes(PAGE_SIZE + 0x10, &vec![0xAA; 3 * PAGE_SIZE as usize - 0x20]).unwrap();
        assert_eq!(m.dirty_code, vec![(PAGE_SIZE + 0x10, 4 * PAGE_SIZE - 0x10)]);
        // zero() is a bulk write too
        let mut m = mem_with_code(&[4, 5, 6]);
        m.zero(4 * PAGE_SIZE, 3 * PAGE_SIZE);
        assert_eq!(m.dirty_code, vec![(4 * PAGE_SIZE, 7 * PAGE_SIZE)]);
        // a 4-byte store reports exactly its 4 bytes
        let mut m = mem_with_code(&[1]);
        m.w32(PAGE_SIZE + 8, 1).unwrap();
        assert_eq!(m.dirty_code, vec![(PAGE_SIZE + 8, PAGE_SIZE + 12)]);
    }

    #[test]
    fn prot_change_flags_code_pages() {
        let mut m = mem_with_code(&[2, 3]);
        m.set_prot(2 * PAGE_SIZE, 2 * PAGE_SIZE, 0);
        // protection changes carry no bytes: they are reported separately from stores
        assert!(m.dirty_code.is_empty());
        assert_eq!(m.prot_code, vec![(2, false), (3, false)]);
        // pages without decoded code are not reported
        let mut m = mem_with_code(&[]);
        m.set_prot(0, 4 * PAGE_SIZE, P_MAPPED | P_R);
        assert!(m.dirty_code.is_empty() && m.prot_code.is_empty());
    }

    #[test]
    fn host_buffer_ranges_reject_overflow_and_outside_backing_storage() {
        let mut m = Memory::new(4096);
        assert!(m.read_bytes(u32::MAX - 3, 8).is_err());
        assert!(m.read_bytes(4095, u32::MAX).is_err());
        assert!(m.write_bytes(u32::MAX - 3, &[0; 8]).is_err());
        assert!(m.write_bytes(4095, &[0; 8]).is_err());
        assert_eq!(m.read_bytes(4096, 0).unwrap(), &[]);
    }

    #[test]
    fn fast_path_tables_follow_prot_and_code() {
        let mut m = mem_with_code(&[]);
        let (_, rt, wt) = m.jit_addrs();
        let rd = |a: usize, pg: usize| unsafe { *(a as *const u8).add(pg) };
        // bit 0: the page qualifies; bit 1: the next page qualifies too (so no end-of-page test is needed);
        // load table bit 2: the page is readable and has had instructions decoded from it
        assert_eq!((rd(rt, 1), rd(wt, 1)), (3, 3));
        m.mark_code(PAGE_SIZE, 1);
        assert_eq!((rd(rt, 1), rd(wt, 1)), (7, 0)); // code page: stores take the slow path
        assert_eq!((rd(rt, 0), rd(wt, 0)), (3, 1)); // ... also those starting on the page before it (they may cross into it)
        m.clear_code_flag(1);
        assert_eq!((rd(rt, 0), rd(wt, 0), rd(wt, 1)), (3, 3, 3));
        m.set_prot(PAGE_SIZE, PAGE_SIZE, P_MAPPED | P_R);
        assert_eq!((rd(rt, 1), rd(wt, 1)), (3, 0));
        assert_eq!((rd(rt, 0), rd(wt, 0)), (3, 1));
        m.set_prot(PAGE_SIZE, PAGE_SIZE, P_MAPPED | P_W); // write-only: both slow
        assert_eq!((rd(rt, 1), rd(wt, 1)), (0, 0));
        assert_eq!((rd(rt, 0), rd(wt, 0)), (1, 1));
        assert_eq!(rd(rt, 200_000), 0); // beyond RAM: always the checked path
        // the last RAM page qualifies but has no qualifying successor
        assert_eq!((rd(rt, 15), rd(wt, 15)), (1, 1));
        assert_eq!((rd(rt, 14), rd(wt, 14)), (3, 3));
    }
}
