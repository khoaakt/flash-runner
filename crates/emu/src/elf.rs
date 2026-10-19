//! Minimal ELF32 (i386) loader for ET_EXEC / ET_DYN plus PT_INTERP resolution.

use crate::mem::{Memory, P_MAPPED, P_R, P_W, P_X};

pub const PT_LOAD: u32 = 1;
pub const PT_INTERP: u32 = 3;
pub const PT_PHDR: u32 = 6;
pub const PT_TLS: u32 = 7;
pub const PT_GNU_STACK: u32 = 0x6474_e551;

#[derive(Debug, Clone, Copy)]
pub struct Phdr {
    pub p_type: u32,
    pub offset: u32,
    pub vaddr: u32,
    pub filesz: u32,
    pub memsz: u32,
    pub flags: u32,
    pub align: u32,
}

#[derive(Debug)]
pub struct ElfInfo {
    pub e_type: u16,
    pub entry: u32,
    pub phoff: u32,
    pub phnum: u16,
    pub phentsize: u16,
    pub phdrs: Vec<Phdr>,
    pub interp: Option<String>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn parse(img: &[u8]) -> Result<ElfInfo, String> {
    if img.len() < 52 || &img[0..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if img[4] != 1 || img[5] != 1 {
        return Err("not ELF32 little-endian".into());
    }
    if u16le(img, 18) != 3 {
        return Err(format!("not i386 (e_machine={})", u16le(img, 18)));
    }
    let e_type = u16le(img, 16);
    let entry = u32le(img, 24);
    let phoff = u32le(img, 28);
    let phentsize = u16le(img, 42);
    let phnum = u16le(img, 44);
    let mut phdrs = Vec::new();
    let mut interp = None;
    for i in 0..phnum as usize {
        let o = phoff as usize + i * phentsize as usize;
        if o + 32 > img.len() {
            return Err("truncated program headers".into());
        }
        let p = Phdr {
            p_type: u32le(img, o),
            offset: u32le(img, o + 4),
            vaddr: u32le(img, o + 8),
            filesz: u32le(img, o + 16),
            memsz: u32le(img, o + 20),
            flags: u32le(img, o + 24),
            align: u32le(img, o + 28),
        };
        if p.p_type == PT_INTERP {
            let s = p.offset as usize;
            let e = img[s..].iter().position(|&b| b == 0).map(|n| s + n).ok_or("bad PT_INTERP")?;
            interp = Some(String::from_utf8_lossy(&img[s..e]).into_owned());
        }
        phdrs.push(p);
    }
    Ok(ElfInfo { e_type, entry, phoff, phnum, phentsize, phdrs, interp })
}

pub struct Loaded {
    pub base: u32,
    pub entry: u32,
    /// Address where the program headers are visible in memory (for AT_PHDR).
    pub phdr_addr: u32,
    /// Highest mapped address (page aligned) — initial brk.
    pub end: u32,
}

fn prot_of(flags: u32) -> u8 {
    let mut p = P_MAPPED;
    if flags & 4 != 0 {
        p |= P_R;
    }
    if flags & 2 != 0 {
        p |= P_W;
    }
    if flags & 1 != 0 {
        p |= P_X;
    }
    p
}

/// Map `img` at `base` (0 for ET_EXEC). Returns layout facts. `img` must outlive nothing: bytes are copied.
pub fn load(mem: &mut Memory, img: &[u8], info: &ElfInfo, base: u32) -> Result<Loaded, String> {
    let mut end = 0u32;
    let mut phdr_addr = 0u32;
    for p in &info.phdrs {
        if p.p_type == PT_PHDR {
            phdr_addr = base.wrapping_add(p.vaddr);
        }
        if p.p_type != PT_LOAD {
            continue;
        }
        let start = base.wrapping_add(p.vaddr);
        let page_start = start & !0xfff;
        let file_end = start.wrapping_add(p.filesz);
        let mem_end = start.wrapping_add(p.memsz);
        let page_end = (mem_end.wrapping_add(0xfff)) & !0xfff;
        if page_end as usize > mem.size() {
            return Err(format!("segment at {:#x}..{:#x} exceeds guest memory", page_start, page_end));
        }
        // Map RW first so we can fill, then apply the final protection.
        mem.set_prot(page_start, page_end - page_start, P_MAPPED | P_R | P_W);
        if p.filesz > 0 {
            let src = img.get(p.offset as usize..(p.offset + p.filesz) as usize).ok_or("segment outside file")?;
            mem.write_bytes(start, src).map_err(|e| format!("{:?}", e))?;
        }
        if mem_end > file_end {
            mem.zero(file_end, mem_end - file_end);
        }
        mem.set_prot(page_start, page_end - page_start, prot_of(p.flags));
        end = end.max(page_end);
        if phdr_addr == 0 && p.offset <= info.phoff && info.phoff < p.offset + p.filesz {
            phdr_addr = start.wrapping_add(info.phoff - p.offset);
        }
    }
    Ok(Loaded { base, entry: base.wrapping_add(info.entry), phdr_addr, end })
}

/// Total span (lowest page start, highest page end) of PT_LOAD segments, used to pick a base for ET_DYN.
pub fn span(info: &ElfInfo) -> (u32, u32) {
    let mut lo = u32::MAX;
    let mut hi = 0u32;
    for p in info.phdrs.iter().filter(|p| p.p_type == PT_LOAD) {
        lo = lo.min(p.vaddr & !0xfff);
        hi = hi.max((p.vaddr + p.memsz + 0xfff) & !0xfff);
    }
    (lo, hi)
}
