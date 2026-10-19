//! Conservative, offline ELF32/i386 -> wasm compilation.
//!
//! Addresses in `entries` and `allowlist` are ELF virtual addresses (NOT file offsets).
//! `load_bias` is added to them; `layout.guest_base` is instead the wasm linear-memory
//! address of guest RAM. No relocations, linking, ELF loading or runtime installation
//! are performed here. Fixed artifacts require the same bytes/layout. Portable v2/v3/v4
//! artifacts bind explicit Wasm operands to live pointers and ELF load bias, without
//! translating any x86 at runtime. v4 additionally decodes recorded fallback bytes
//! to register generic interpreter calls, never to generate Wasm. CPU field offsets
//! remain ABI checked.
//! Plugin builds fuse nearby supported blocks into bounded multi-node regions;
//! static edges inside a region stay in Wasm. Only actual translator chain heads
//! are published for dispatcher entry. Unsupported operations and HLE entries stay
//! dispatcher frontiers in strict/partial mode. Explicit complete mode serializes
//! unsupported instructions and their semantic tag relocations instead.
//!
//! Before installation, validate layout, executable permissions and exact bytes;
//! thereafter every dispatch must remain protected by normal JIT invalidation on
//! writes/remaps (including compiled stores). `guard_region` supplies a byte check.
//! S_RETRY / other exits require the existing interpreter/dispatcher semantics.

use crate::jit_x86::{self, FbStats, Layout, NodeIn, RegionOut};
use crate::mem::{Memory, PAGE_SIZE, P_MAPPED, P_X};
use iced_x86::{Code, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write;

#[derive(Clone, Debug)]
pub struct Options {
    pub load_bias: u32,
    pub entries: Vec<u32>,
    /// Optional profile allowlist, half-open ELF virtual-address ranges. Discovery
    /// outside these ranges becomes an explicit dispatcher exit, never silent code.
    pub allowlist: Vec<(u32, u32)>,
    pub max_instructions: usize,
    pub max_blocks: usize,
    pub layout: Layout,
}

#[derive(Clone, Debug)]
pub struct Region {
    pub address: u32,
    pub file_offset: u32,
    pub bytes: Vec<u8>,
    pub instruction_count: usize,
    pub successors: Vec<u32>,
    /// A common dynamic exit kind, if any. Mixed fused exit kinds have no single
    /// label; static successors include external continuation/return-site edges.
    pub dynamic_exit: Option<&'static str>,
}

/// One generic interpreter call, recorded from the actual file-backed instruction
/// bytes during offline lowering. `ip` is a guest VA including `Artifact::load_bias`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fallback {
    pub ip: u32,
    pub bytes: Vec<u8>,
}

pub struct Artifact {
    /// v1/v2 retain the single-block contract; v3 carries fused chain metadata and
    /// v4 adds exact fallback records. Both retain an explicit portable flag.
    bundle_version: u8,
    /// Some while portable symbols (including local tags) need binding. Offsets
    /// refer to fixed five-byte LEB operands, never inferred from constant values.
    pub relocations: Option<Vec<crate::wasm_emit::Relocation>>,
    /// Module-local indices in kind-5 relocations index this vector. Empty before v4.
    pub fallback: Vec<Fallback>,
    pub wasm: Vec<u8>,
    pub layout: Layout,
    pub load_bias: u32,
    pub entries: Vec<u32>,
    pub allowlist: Vec<(u32, u32)>,
    pub input_size: usize,
    /// FNV-1a is a reproducibility identifier, NOT a cryptographic authenticity check.
    pub input_id: u64,
    pub regions: Vec<Region>,
    /// Index-aligned with regions and exports r0..rN. Pass these exact heads and
    /// spans to the existing JitState installation/registration path.
    pub outputs: Vec<RegionOut>,
}

fn identity(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

// Keep the manifest independent of serde and explicitly enumerate every baked-in offset.
fn layout_values(l: &Layout) -> Vec<(&'static str, u32)> {
    macro_rules! fields { ($($f:ident),*) => { vec![$((stringify!($f), l.$f)),*] }; }
    let mut v = fields!(
        cpu_r,
        cpu_eip,
        cpu_fl,
        cpu_fs_base,
        cpu_gs_base,
        cpu_lf_kind,
        cpu_lf_a,
        cpu_lf_b,
        cpu_lf_r,
        cpu_budget,
        cpu_xmm,
        cpu_mxcsr,
        cpu_mm0,
        cpu_mm_se0,
        mm_stride,
        cpu_fpu_top,
        cpu_fpu_tag,
        cpu_fpu_sw,
        cpu_fpu_cw,
        cpu_fpu_last_ip,
        cpu_fpu_last_dp,
        cpu_fault,
        cpu_aux,
        guest_base,
        rtab,
        wtab,
        cache,
        prof
    );
    v.extend([
        ("simd", l.simd as u32),
        ("slow_inline", l.slow_inline as u32),
        ("trust_loads", l.trust_loads as u32),
    ]);
    v
}

/// Consumes a freshly decoded bundle without exposing mutable metadata. The
/// runtime can link it in place instead of serializing/decoding a second copy.
/// Ordinary public Artifact binding still revalidates potentially edited fields.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct DecodedArtifact(Artifact);

#[cfg(any(target_arch = "wasm32", test))]
impl DecodedArtifact {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> { Artifact::decode(bytes).map(Self) }
    pub(crate) fn layout(&self) -> &Layout { &self.0.layout }
    pub(crate) fn decode_fallback_instructions(&self, bias: u32) -> Result<Vec<Instruction>, String> {
        self.0.decode_fallback_instructions(bias)
    }
    pub(crate) fn bind_with_tags(self, layout: &Layout, bias: u32, tags: &[u32]) -> Result<Artifact, String> {
        self.0.bind_validated(layout, bias, Some(tags))
    }
}

impl Artifact {
    /// Link offline machine code for this instance. No guest decoding/translation.
    /// With v4 fallback, local tag operands remain unchanged and relocations stay
    /// present until `bind_with_tags` supplies the instance's allocated tags.
    /// Exact live-byte and executable-page guards run again during `install`.
    pub fn bind(&self, layout: &Layout, load_bias: u32) -> Result<Self, String> {
        self.bind_inner(layout, load_bias, None)
    }

    /// Resolve explicit exec_one tag sites as well as layout/IP symbols. `tags`
    /// must be index-aligned with `fallback`, allocated by the target JitState.
    /// This is linking, not instruction lowering; failure transfers no ownership.
    pub fn bind_with_tags(&self, layout: &Layout, load_bias: u32, tags: &[u32]) -> Result<Self, String> {
        if tags.len() != self.fallback.len() || tags.iter().copied().collect::<BTreeSet<_>>().len() != tags.len() {
            return Err("AOT fallback tag count/uniqueness mismatch".into());
        }
        self.bind_inner(layout, load_bias, Some(tags))
    }

    /// Decode only serialized instructions, never live code or a CFG and never
    /// translate. Relative operands are reconstructed at the requested ELF bias.
    pub fn decode_fallback_instructions(&self, load_bias: u32) -> Result<Vec<Instruction>, String> {
        self.validate_fallback()?;
        self.fallback.iter().map(|record| {
            let ip = record.ip.checked_sub(self.load_bias).and_then(|a| a.checked_add(load_bias)).ok_or("AOT fallback address overflow")?;
            ip.checked_add(record.bytes.len() as u32).ok_or("AOT fallback address overflow")?;
            let ins = Decoder::with_ip(32, &record.bytes, ip as u64, DecoderOptions::NONE).decode();
            if ins.is_invalid() || ins.len() != record.bytes.len() { return Err("invalid AOT fallback instruction".into()); }
            Ok(ins)
        }).collect()
    }

    fn bind_inner(&self, layout: &Layout, load_bias: u32, tags: Option<&[u32]>) -> Result<Self, String> {
        Self::decode(&self.encode_bundle())?.bind_validated(layout, load_bias, tags)
    }

    // Only decode() or the non-mutable DecodedArtifact owner may enter this path.
    // No host-visible Artifact is trusted merely because its fields look plausible.
    fn bind_validated(mut self, layout: &Layout, load_bias: u32, tags: Option<&[u32]>) -> Result<Self, String> {
        if let Some(tags) = tags {
            if tags.len() != self.fallback.len() || tags.iter().copied().collect::<BTreeSet<_>>().len() != tags.len() {
                return Err("AOT fallback tag count/uniqueness mismatch".into());
            }
        }
        let relocations = self.relocations.take().ok_or("artifact is not portable")?;
        let mut expected = self.layout.clone();
        expected.guest_base = layout.guest_base;
        expected.rtab = layout.rtab;
        expected.wtab = layout.wtab;
        if self.layout.cache != 0 { expected.cache = layout.cache; }
        // Portable SSE moves/logic use scalar Wasm integer instructions. This
        // capability bit does not change the ABI or generated instructions.
        expected.simd = layout.simd;
        if self.layout.cache != 0 && layout.cache == 0 { return Err("portable chaining artifact requires a live supported cache".into()); }
        if &expected != layout { return Err("portable AOT CPU ABI or safety modes differ".into()); }
        let previous_bias = self.load_bias;
        let rebase = |a: u32| a.checked_sub(previous_bias).and_then(|v| v.checked_add(load_bias)).ok_or_else(|| "portable AOT address overflow".to_string());
        let mut previous_end = 0;
        let keep_relocations = tags.is_none() && !self.fallback.is_empty();
        let mut remaining = Vec::new();
        for r in relocations {
            let offset = r.offset as usize;
            if offset < previous_end || offset.checked_add(5).is_none_or(|e| e > self.wasm.len()) {
                return Err("invalid/overlapping AOT relocation".into());
            }
            if self.wasm[offset..offset + 5] != crate::wasm_emit::fixed_leb(r.value, matches!(r.kind, 0 | 5)) {
                return Err("AOT relocation operand mismatch".into());
            }
            let value = match r.kind {
                0 => rebase(r.value)?,
                1 if r.value == self.layout.guest_base => layout.guest_base,
                2 if r.value == self.layout.rtab => layout.rtab,
                3 if r.value == self.layout.wtab => layout.wtab,
                4 if self.layout.cache != 0 && r.value.checked_sub(self.layout.cache).is_some_and(|n| matches!(n, 0 | 4 | 8)) =>
                    layout.cache.checked_add(r.value - self.layout.cache).ok_or("AOT cache relocation overflow")?,
                5 if (r.value as usize) < self.fallback.len() => tags.map_or(r.value, |tags| tags[r.value as usize]),
                _ => return Err("invalid AOT relocation symbol".into()),
            };
            self.wasm[offset..offset + 5].copy_from_slice(&crate::wasm_emit::fixed_leb(value, matches!(r.kind, 0 | 5)));
            if keep_relocations { remaining.push(crate::wasm_emit::Relocation { offset: r.offset, kind: r.kind, value }); }
            previous_end = offset + 5;
        }
        for a in &mut self.entries { *a = rebase(*a)?; }
        for r in &mut self.regions {
            r.address = rebase(r.address)?;
            r.address.checked_add(r.bytes.len() as u32).ok_or("portable AOT guard address overflow")?;
            for a in &mut r.successors { *a = rebase(*a)?; }
        }
        for out in &mut self.outputs {
            for (a, _) in out.heads.iter_mut().chain(&mut out.chains) { *a = rebase(*a)?; }
            for (a, _) in &mut out.mat_sites { *a = rebase(*a)?; }
            for (_, target) in &mut out.exits { if *target != 0 { *target = rebase(*target)?; } }
        }
        for record in &mut self.fallback { record.ip = rebase(record.ip)?; }
        self.layout = layout.clone();
        self.load_bias = load_bias;
        // Retain updated address symbols too: an address-bound artifact with local
        // tags may still be rebound without silently keeping old absolute IPs.
        self.relocations = if keep_relocations { Some(remaining) } else { None };
        Ok(self)
    }
    /// Runtime entry point: decode, guard and install through normal JIT lifecycle.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        Self::decode_bundle(bytes)
    }

    pub fn install(
        &self,
        mem: &mut Memory,
        jit: &mut crate::jit::JitState,
    ) -> Result<usize, String> {
        if self.relocations.is_some() { return Err("bind portable AOT before installation".into()); }
        if self.outputs.len() != self.regions.len() {
            return Err("inconsistent region metadata".into());
        }
        let regions: Vec<_> = self
            .outputs
            .iter()
            .cloned()
            .zip(&self.regions)
            .map(|(out, r)| (out, vec![(r.address, r.bytes.clone())]))
            .collect();
        jit.install_precompiled(mem, &self.layout, &self.wasm, &regions)
    }

    /// Preserve previously compiled loader thunks when installing a portable block bundle.
    pub fn install_available(&self, mem: &mut Memory, jit: &mut crate::jit::JitState) -> Result<usize, String> {
        if self.relocations.is_some() || self.outputs.len() != self.regions.len() { return Err("bind a valid portable artifact first".into()); }
        let regions: Vec<_> = self.outputs.iter().cloned().zip(&self.regions)
            .map(|(out, r)| (out, vec![(r.address, r.bytes.clone())])).collect();
        jit.install_precompiled_available(mem, &self.layout, &self.wasm, &regions)
    }
    /// Binary transport for Rust/browser runtime integration. All integers are LE;
    /// vectors/byte strings have u32 lengths. Use `decode_bundle`, not hand-written
    /// parsers. The bundle is trusted executable input, NOT an authenticated format.
    pub fn encode_bundle(&self) -> Vec<u8> {
        fn word(b: &mut Vec<u8>, n: u32) {
            b.extend(n.to_le_bytes());
        }
        fn blob(b: &mut Vec<u8>, v: &[u8]) {
            word(b, v.len().try_into().expect("AOT vector too large"));
            b.extend(v);
        }
        fn words(b: &mut Vec<u8>, v: &[u32]) {
            word(b, v.len() as u32);
            for &x in v {
                word(b, x);
            }
        }
        fn pairs(b: &mut Vec<u8>, v: &[(u32, u32)]) {
            word(b, v.len() as u32);
            for &(x, y) in v {
                word(b, x);
                word(b, y);
            }
        }
        let mut b = b"FWAOT\0\0\x01".to_vec();
        b[7] = if self.bundle_version >= 3 { self.bundle_version } else if self.relocations.is_some() { 2 } else { 1 };
        if b[7] >= 3 { word(&mut b, self.relocations.is_some() as u32); }
        words(
            &mut b,
            &layout_values(&self.layout)
                .iter()
                .map(|(_, v)| *v)
                .collect::<Vec<_>>(),
        );
        word(&mut b, self.load_bias);
        b.extend((self.input_size as u64).to_le_bytes());
        b.extend(self.input_id.to_le_bytes());
        words(&mut b, &self.entries);
        pairs(&mut b, &self.allowlist);
        blob(&mut b, &self.wasm);
        word(&mut b, self.regions.len() as u32);
        for (r, o) in self.regions.iter().zip(&self.outputs) {
            word(&mut b, r.address);
            word(&mut b, r.file_offset);
            blob(&mut b, &r.bytes);
            word(&mut b, r.instruction_count as u32);
            words(&mut b, &r.successors);
            word(
                &mut b,
                match r.dynamic_exit {
                    None => 0,
                    Some("return") => 1,
                    Some("interrupt") => 2,
                    Some("pause") => 3,
                    Some("indirect-call") => 4,
                    Some("indirect-jump") => 5,
                    _ => unreachable!(),
                },
            );
            pairs(&mut b, &o.heads);
            word(&mut b, o.n_ins);
            word(&mut b, o.n_fallback);
            word(&mut b, o.n_intr);
            pairs(&mut b, &o.chains);
            pairs(&mut b, &o.exits);
            word(&mut b, o.mat_sites.len() as u32);
            for (a, name) in &o.mat_sites {
                word(&mut b, *a);
                blob(&mut b, name.as_bytes());
            }
        }
        if let Some(relocations) = &self.relocations {
            word(&mut b, relocations.len() as u32);
            for r in relocations { word(&mut b, r.offset); word(&mut b, r.kind); word(&mut b, r.value); }
        }
        if b[7] >= 4 {
            word(&mut b, self.fallback.len() as u32);
            for record in &self.fallback { word(&mut b, record.ip); blob(&mut b, &record.bytes); }
        }
        b
    }

    /// Decode v1/v2 single-block, v3 fused or v4 fallback-bearing bundles.
    /// In v3 the word after the magic is flags (bit 0 = portable relocations).
    /// Wasm validation,
    /// ABI equality, byte/permission guards and JIT registration are still required.
    pub fn decode_bundle(bytes: &[u8]) -> Result<Self, String> {
        struct Reader<'a>(&'a [u8]);
        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
                let v = self.0.get(..n).ok_or("truncated AOT bundle")?;
                self.0 = &self.0[n..];
                Ok(v)
            }
            fn word(&mut self) -> Result<u32, String> {
                Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
            }
            fn wide(&mut self) -> Result<u64, String> {
                Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
            }
            fn count(&mut self, stride: usize) -> Result<usize, String> {
                let n = self.word()? as usize;
                if n > self.0.len() / stride {
                    return Err("invalid AOT vector length".into());
                }
                Ok(n)
            }
            fn blob(&mut self) -> Result<Vec<u8>, String> {
                let n = self.count(1)?;
                Ok(self.take(n)?.to_vec())
            }
            fn words(&mut self) -> Result<Vec<u32>, String> {
                let n = self.count(4)?;
                (0..n).map(|_| self.word()).collect()
            }
            fn pairs(&mut self) -> Result<Vec<(u32, u32)>, String> {
                let n = self.count(8)?;
                (0..n).map(|_| Ok((self.word()?, self.word()?))).collect()
            }
        }
        if bytes.len() > 256 * 1024 * 1024 {
            return Err("AOT bundle exceeds 256 MiB limit".into());
        }
        let mut r = Reader(bytes);
        let header = r.take(8)?;
        if &header[..7] != b"FWAOT\0\0" || !matches!(header[7], 1 | 2 | 3 | 4) {
            return Err("unsupported AOT bundle version".into());
        }
        let version = header[7];
        let portable = if version >= 3 {
            match r.word()? {
                0 => false,
                1 => true,
                _ => return Err("unsupported AOT bundle flags".into()),
            }
        } else { version == 2 };
        let values = r.words()?;
        let mut layout = Layout::default();
        if values.len() != layout_values(&layout).len() {
            return Err("unsupported layout ABI".into());
        }
        let mut values = values.into_iter();
        macro_rules! fields { ($($f:ident),*) => { $(layout.$f = values.next().unwrap();)* }; }
        fields!(
            cpu_r,
            cpu_eip,
            cpu_fl,
            cpu_fs_base,
            cpu_gs_base,
            cpu_lf_kind,
            cpu_lf_a,
            cpu_lf_b,
            cpu_lf_r,
            cpu_budget,
            cpu_xmm,
            cpu_mxcsr,
            cpu_mm0,
            cpu_mm_se0,
            mm_stride,
            cpu_fpu_top,
            cpu_fpu_tag,
            cpu_fpu_sw,
            cpu_fpu_cw,
            cpu_fpu_last_ip,
            cpu_fpu_last_dp,
            cpu_fault,
            cpu_aux,
            guest_base,
            rtab,
            wtab,
            cache,
            prof
        );
        let flags: Vec<_> = values.collect();
        if flags.iter().any(|&x| x > 1) {
            return Err("invalid layout boolean".into());
        }
        layout.simd = flags[0] != 0;
        layout.slow_inline = flags[1] != 0;
        layout.trust_loads = flags[2] != 0;
        if (layout.cache != 0 && !portable && version < 3) || layout.prof != 0 || layout.slow_inline || layout.trust_loads {
            return Err("unsafe AOT layout mode".into());
        }
        let load_bias = r.word()?;
        let input_size = usize::try_from(r.wide()?).map_err(|_| "input size overflow")?;
        let input_id = r.wide()?;
        let entries = r.words()?;
        let allowlist = r.pairs()?;
        let wasm = r.blob()?;
        if wasm.get(..8) != Some(b"\0asm\x01\0\0\0") {
            return Err("invalid wasm module header".into());
        }
        let count = r.count(4)?;
        if count == 0 || count > 1024 {
            return Err("AOT bundle must contain 1..=1024 regions".into());
        }
        let mut regions = Vec::new();
        let mut outputs = Vec::new();
        let mut addresses = BTreeSet::new();
        for _ in 0..count {
            let address = r.word()?;
            let file_offset = r.word()?;
            let bytes = r.blob()?;
            let instruction_count = r.word()? as usize;
            let successors = r.words()?;
            let dynamic_exit = match r.word()? {
                0 => None,
                1 => Some("return"),
                2 => Some("interrupt"),
                3 => Some("pause"),
                4 => Some("indirect-call"),
                5 => Some("indirect-jump"),
                _ => return Err("invalid dynamic exit".into()),
            };
            let heads = r.pairs()?;
            let n_ins = r.word()?;
            let n_fallback = r.word()?;
            let n_intr = r.word()?;
            let chains = r.pairs()?;
            let exits = r.pairs()?;
            let n = r.count(8)?;
            let mat_sites = (0..n)
                .map(|_| {
                    Ok((
                        r.word()?,
                        String::from_utf8(r.blob()?).map_err(|_| "invalid AOT UTF-8")?,
                    ))
                })
                .collect::<Result<_, String>>()?;
            if bytes.is_empty()
                || instruction_count == 0
                || instruction_count > bytes.len()
                || address.checked_add(bytes.len() as u32).is_none()
                || file_offset as u64 + bytes.len() as u64 > input_size as u64
                || (version < 3 && heads != [(address, 0)])
                || n_ins as usize != instruction_count
                || (version < 4 && n_fallback != 0) || n_fallback > n_ins
                || n_intr != 0
            {
                return Err("inconsistent AOT region metadata".into());
            }
            let end = address + bytes.len() as u32;
            if heads.first() != Some(&(address, 0))
                || heads.len() != chains.len()
                || heads.is_empty()
                || heads.iter().any(|&(a, entry)| {
                    a < address || a >= end || !addresses.insert(a)
                        || chains.get(entry as usize).is_none_or(|&(head, count)| head != a || count == 0)
                })
                || chains.iter().try_fold(0u32, |n, &(_, count)| n.checked_add(count)) != Some(n_ins)
                || (version >= 3 && (heads.len() > FUSION_MAX_NODES || instruction_count > FUSION_MAX_INSTRUCTIONS || bytes.len() > FUSION_MAX_SPAN))
            {
                return Err("inconsistent AOT chain metadata".into());
            }
            regions.push(Region {
                address,
                file_offset,
                bytes,
                instruction_count,
                successors,
                dynamic_exit,
            });
            outputs.push(RegionOut {
                heads,
                n_ins,
                n_fallback,
                n_intr,
                chains,
                exits,
                mat_sites,
            });
        }
        let relocations = if portable {
            let n = r.count(12)?;
            let mut relocations = Vec::with_capacity(n);
            let mut end = 0usize;
            for _ in 0..n {
                let item = crate::wasm_emit::Relocation { offset: r.word()?, kind: r.word()?, value: r.word()? };
                let offset = item.offset as usize;
                if item.kind > if version >= 4 { 5 } else { 4 } || offset < end || offset.checked_add(5).is_none_or(|e| e > wasm.len()) || wasm[offset..offset+5] != crate::wasm_emit::fixed_leb(item.value, matches!(item.kind, 0 | 5)) {
                    return Err("invalid AOT relocation".into());
                }
                end = offset + 5;
                relocations.push(item);
            }
            Some(relocations)
        } else { None };
        let fallback = if version >= 4 {
            let n = r.count(9)?;
            (0..n).map(|_| Ok(Fallback { ip: r.word()?, bytes: r.blob()? })).collect::<Result<Vec<_>, String>>()?
        } else { Vec::new() };
        if !r.0.is_empty() || entries.is_empty() || entries.iter().any(|a| !addresses.contains(a)) {
            return Err("invalid AOT entries or trailing bytes".into());
        }
        let artifact = Artifact {
            bundle_version: version,
            relocations,
            fallback,
            wasm,
            layout,
            load_bias,
            entries,
            allowlist,
            input_size,
            input_id,
            regions,
            outputs,
        };
        artifact.decode_fallback_instructions(load_bias)?;
        Ok(artifact)
    }

    /// Verify fallback ownership and actual byte provenance, not guessed opcode or
    /// constant patterns. Decoding is deliberately limited to these exact records.
    fn validate_fallback(&self) -> Result<(), String> {
        if self.outputs.len() != self.regions.len() { return Err("inconsistent AOT region metadata".into()); }
        let count = self.outputs.iter().try_fold(0usize, |n, out| n.checked_add(out.n_fallback as usize)).ok_or("AOT fallback count overflow")?;
        if count != self.fallback.len() || self.bundle_version < 4 && count != 0 {
            return Err("AOT fallback record count mismatch".into());
        }
        let mut counts = vec![0u32; self.regions.len()];
        let mut spans: Vec<_> = self.regions.iter().enumerate().map(|(i, r)| (r.address, i)).collect();
        spans.sort_unstable();
        if self.bundle_version >= 4 && spans.windows(2).any(|pair| {
            let region = &self.regions[pair[0].1];
            region.address as u64 + region.bytes.len() as u64 > pair[1].0 as u64
        }) { return Err("overlapping AOT fallback guard ownership".into()); }
        let mut addresses = BTreeSet::new();
        for record in &self.fallback {
            if !(1..=15).contains(&record.bytes.len()) || !addresses.insert(record.ip) || record.ip.checked_add(record.bytes.len() as u32).is_none() {
                return Err("invalid/duplicate AOT fallback record".into());
            }
            let pos = spans.partition_point(|&(address, _)| address <= record.ip);
            let &(_, index) = spans.get(pos.wrapping_sub(1)).ok_or("unguarded AOT fallback record")?;
            let region = &self.regions[index];
            let at = (record.ip - region.address) as usize;
            if region.bytes.get(at..at + record.bytes.len()) != Some(record.bytes.as_slice()) {
                return Err("AOT fallback bytes differ from code guard".into());
            }
            counts[index] += 1;
        }
        if counts.iter().zip(&self.outputs).any(|(&n, out)| n != out.n_fallback) { return Err("AOT fallback region count mismatch".into()); }
        if let Some(relocations) = &self.relocations {
            let mut seen = vec![false; count];
            for r in relocations.iter().filter(|r| r.kind == 5) {
                let at = r.offset as usize;
                let item = seen.get_mut(r.value as usize).ok_or("AOT fallback relocation index out of range")?;
                if *item || at == 0 || self.wasm.get(at - 1) != Some(&crate::wasm_emit::op::I32_CONST)
                    || self.wasm.get(at + 5..at + 7) != Some(&[crate::wasm_emit::op::CALL, 0][..])
                { return Err("invalid AOT exec_one tag relocation".into()); }
                *item = true;
            }
            if seen.iter().any(|&present| !present) { return Err("missing AOT fallback tag relocation".into()); }
        }
        Ok(())
    }

    /// Complete pre-install check against live guest memory, including executable
    /// mappings. This does not replace runtime invalidation/mark_code registration.
    pub fn guard(&self, mem: &Memory, layout: &Layout) -> bool {
        self.matches_layout(layout)
            && self.regions.iter().enumerate().all(|(i, _)| {
                self.guard_region(i, |a| {
                    if mem.page_prot(a) & (P_MAPPED | P_X) != (P_MAPPED | P_X) {
                        return None;
                    }
                    mem.raw().get(a as usize).copied()
                })
            })
    }
    pub fn matches_layout(&self, layout: &Layout) -> bool {
        layout_values(&self.layout) == layout_values(layout)
    }

    /// Check the original input identity AND exact compiled bytes. Not authentication.
    pub fn matches_input(&self, image: &[u8]) -> bool {
        image.len() == self.input_size
            && identity(image) == self.input_id
            && self.regions.iter().all(|r| {
                image.get(r.file_offset as usize..r.file_offset as usize + r.bytes.len())
                    == Some(r.bytes.as_slice())
            })
    }

    /// `read` must read live guest bytes, NOT the ELF file. Permissions/layout must
    /// also be checked by the caller. Returns false for an invalid region index.
    pub fn guard_region(&self, index: usize, mut read: impl FnMut(u32) -> Option<u8>) -> bool {
        self.regions.get(index).is_some_and(|r| {
            r.bytes
                .iter()
                .enumerate()
                .all(|(i, b)| read(r.address + i as u32) == Some(*b))
        })
    }

    /// Versioned JSON manifest. Exports are r0..rN, each (cpu:i32, entry:i32=0)->i32.
    /// All integer offsets are decimal; byte strings and FNV identities are hex.
    pub fn manifest_json(&self) -> String {
        let version = if self.bundle_version >= 3 { self.bundle_version } else if self.relocations.is_some() { 2 } else { 1 };
        let mut s = format!("{{\n\"format\":\"flash-runner-aot-v{version}\",\n\"portable\":{},\n\"input_size\":{},\"input_fnv1a64\":\"{:016x}\",\"module_fnv1a64\":\"{:016x}\",\n\"load_bias\":{},\"entries\":{:?},\n\"allowlist\":[", self.relocations.is_some(), self.input_size, self.input_id, identity(&self.wasm), self.load_bias, self.entries);
        for (i, (a, b)) in self.allowlist.iter().enumerate() {
            if i != 0 {
                s.push(',');
            }
            write!(s, "[{a},{b}]").unwrap();
        }
        s.push_str("],\n\"layout\":{");
        for (i, (key, value)) in layout_values(&self.layout).iter().enumerate() {
            if i != 0 {
                s.push(',');
            }
            write!(s, "\"{key}\":{value}").unwrap();
        }
        s.push_str("},\n\"abi\":{\"module\":\"env\",\"memory\":\"memory (min 1 page)\",\"table\":\"table (funcref, min 0)\",\"export\":\"(cpu:i32,entry:i32=0)->status:i32\",\"imports\":{");
        for (i, (name, n)) in [
            ("exec_one", 2),
            ("load_slow", 4),
            ("store_slow", 5),
            ("mat", 6),
            ("sys_time", 1),
            ("fist_fild", 4),
            ("fcomip", 3),
            ("cvtss2si", 5),
            ("mm_op", 5),
        ]
        .iter()
        .enumerate()
        {
            if i != 0 {
                s.push(',');
            }
            write!(s, "\"{name}\":\"({})->i32\"", vec!["i32"; *n].join(",")).unwrap();
        }
        s.push_str("},\"status\":[\"NEXT\",\"EXIT\",\"SEGV\",\"DIVERR\",\"SYSCALL\",\"HYPER\",\"PAUSE\",\"SYNC\",\"RETRY\"]},\n\"fallback_tags\":[");
        for (i, record) in self.fallback.iter().enumerate() {
            if i != 0 { s.push(','); }
            write!(s, "{{\"index\":{i},\"ip\":{},\"bytes_hex\":\"", record.ip).unwrap();
            for byte in &record.bytes { write!(s, "{byte:02x}").unwrap(); }
            s.push_str("\"}");
        }
        s.push_str("],\n\"regions\":[");
        for (i, r) in self.regions.iter().enumerate() {
            if i != 0 {
                s.push(',');
            }
            write!(s, "\n{{\"export\":\"r{i}\",\"entry\":0,\"address\":{},\"file_offset\":{},\"instruction_count\":{},\"successors\":{:?},\"dynamic_exit\":{},\"guard_hex\":\"", r.address, r.file_offset, r.instruction_count, r.successors, r.dynamic_exit.map_or("null".into(), |v| format!("\"{v}\""))).unwrap();
            for b in &r.bytes {
                write!(s, "{b:02x}").unwrap();
            }
            let o = &self.outputs[i];
            s.push_str("\",\"heads\":[");
            for (j, (address, entry)) in o.heads.iter().enumerate() {
                if j != 0 {
                    s.push(',');
                }
                write!(s, "[{address},{entry}]").unwrap();
            }
            write!(
                s,
                "],\"n_ins\":{},\"n_fallback\":{},\"n_intr\":{},\"chains\":[",
                o.n_ins, o.n_fallback, o.n_intr
            )
            .unwrap();
            for (j, (address, count)) in o.chains.iter().enumerate() {
                if j != 0 {
                    s.push(',');
                }
                write!(s, "[{address},{count}]").unwrap();
            }
            s.push_str("]}");
        }
        s.push_str("\n],\"relocations\":[");
        if let Some(relocations) = &self.relocations {
            for (i, r) in relocations.iter().enumerate() {
                if i != 0 { s.push(','); }
                write!(s, "{{\"offset\":{},\"kind\":{},\"value\":{}}}", r.offset, r.kind, r.value).unwrap();
            }
        }
        s.push_str("]}\n");
        s
    }
}

struct Segment {
    start: u32,
    end: u32,
    file: u32,
}

// Intentionally parse only the ELF header and PT_LOADs: the general loader also
// parses PT_INTERP, which is unnecessary for offline compilation of untrusted input.
fn segments(image: &[u8], bias: u32) -> Result<Vec<Segment>, String> {
    let u16at = |o| u16::from_le_bytes(image[o..o + 2].try_into().unwrap());
    let u32at = |o| u32::from_le_bytes(image[o..o + 4].try_into().unwrap());
    if image.len() < 52 || &image[..7] != b"\x7fELF\x01\x01\x01" {
        return Err("expected ELF32 little-endian version 1".into());
    }
    let kind = u16at(16);
    if !matches!(kind, 2 | 3) || u16at(18) != 3 || u32at(20) != 1 {
        return Err("expected i386 ET_EXEC/ET_DYN".into());
    }
    if kind == 2 && bias != 0 {
        return Err("ET_EXEC requires zero load bias".into());
    }
    let phoff = u32at(28) as usize;
    let stride = u16at(42) as usize;
    let count = u16at(44) as usize;
    if stride < 32
        || phoff
            .checked_add(stride * count)
            .is_none_or(|e| e > image.len())
    {
        return Err("invalid program header table".into());
    }
    let mut result = Vec::new();
    let mut loads = Vec::new();
    for i in 0..count {
        let o = phoff + i * stride;
        if u32at(o) != 1 {
            continue;
        }
        let file = u32at(o + 4);
        let start = bias
            .checked_add(u32at(o + 8))
            .ok_or("segment address overflow")?;
        let size = u32at(o + 16);
        let memsize = u32at(o + 20);
        let end = start
            .checked_add(memsize)
            .ok_or("segment address overflow")?;
        if size > memsize
            || file.checked_add(size).is_none()
            || file as u64 + size as u64 > image.len() as u64
        {
            return Err("invalid PT_LOAD file range".into());
        }
        if loads.iter().any(|&(a, b)| start < b && a < end) {
            return Err("overlapping PT_LOAD segments are not supported".into());
        }
        loads.push((start, end));
        if u32at(o + 24) & 1 != 0 {
            result.push(Segment {
                start,
                end: start + size,
                file,
            });
        }
    }
    if result.is_empty() {
        return Err("no file-backed executable PT_LOAD".into());
    }
    Ok(result)
}

fn source<'a>(image: &'a [u8], segs: &[Segment], address: u32) -> Option<(u32, &'a [u8])> {
    let s = segs
        .iter()
        .find(|s| address >= s.start && address < s.end)?;
    let offset = s.file + (address - s.start);
    Some((
        offset,
        &image[offset as usize..(s.file as usize + (s.end - s.start) as usize)],
    ))
}

struct Decoded {
    ins: Instruction,
    offset: u32,
    successors: Vec<u32>,
    terminal: bool,
    dynamic: Option<&'static str>,
    supported: bool,
}

pub fn compile(image: &[u8], options: &Options) -> Result<Artifact, String> {
    let segs = segments(image, options.load_bias)?;
    compile_source(image, options, segs, false, false)
}

/// Offline PIC compiler for audited integer and bit-preserving SSE paths. ELF text
/// relocations that alter code fail exact
/// runtime guards; immediate/displacement data are deliberately NOT rebased. Their
/// meaning is determined by x86 bytes, not guessed from their numeric value.
pub fn compile_portable(image: &[u8], options: &Options) -> Result<Artifact, String> {
    let segs = segments(image, options.load_bias)?;
    if options.layout.rtab == options.layout.wtab { return Err("portable AOT needs distinct table symbols".into()); }
    compile_source(image, options, segs, true, false)
}

/// Compile supported blocks along decoded static control flow, including supported
/// continuations after unsupported instructions. Unsupported operations themselves
/// and existing exact-byte HLE leaves remain dispatcher frontiers. Never sweep
/// arbitrary .text bytes, which may contain data or unreachable instruction streams.
pub fn compile_portable_partial(image: &[u8], options: &Options) -> Result<Artifact, String> {
    let segs = segments(image, options.load_bias)?;
    if options.layout.rtab == options.layout.wtab { return Err("portable AOT needs distinct table symbols".into()); }
    compile_source(image, options, segs, true, true)
}

/// Compile every valid discovered instruction, including unsupported FP/SIMD and
/// operand shapes as generic interpreter calls emitted offline. This does not
/// infer indirect targets or sweep text, and does not add interpreter semantics.
pub fn compile_portable_complete(image: &[u8], options: &Options) -> Result<Artifact, String> {
    let segs = segments(image, options.load_bias)?;
    if options.layout.rtab == options.layout.wtab { return Err("portable AOT needs distinct table symbols".into()); }
    let mut artifacts = compile_source_batches_complete(image, options, segs, true, true, options.max_blocks, &mut DiscoveryReport::default(), true, true, false)?;
    if artifacts.len() != 1 { return Err("expected a single AOT module".into()); }
    Ok(artifacts.remove(0))
}

/// Offline discovery evidence. Counts are static coverage, never execution counts.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryReport {
    pub symbol_entries: usize,
    pub unwind_entries: usize,
    pub initializer_entries: usize,
    pub relocated_code_pointers: usize,
    pub executable_bytes: usize,
    pub decoded_instructions: usize,
    pub compiled_instructions: usize,
    /// Actual instruction bytes, excluding uncompiled bytes inside fused guards.
    pub compiled_bytes: usize,
    /// Generic exec_one call sites/bytes compiled offline; not native FP lowering.
    pub fallback_instructions: usize,
    pub fallback_bytes: usize,
    /// Supported blocks before region fusion, not Wasm export count.
    pub compiled_blocks: usize,
    pub fused_regions: usize,
    pub exported_heads: usize,
    pub internal_static_edges: usize,
    /// Includes uncompiled gaps protected by the conservative contiguous guard.
    pub guarded_bytes: usize,
    pub hle_frontiers: usize,
    /// Distinct queued static addresses left by the offline resource budget.
    pub budget_frontiers: usize,
    /// Invalid/truncated streams cannot be serialized as an actual Instruction.
    pub invalid_frontiers: usize,
    pub unsupported: BTreeMap<String, usize>,
}

impl DiscoveryReport {
    pub fn json(&self) -> String {
        let mut s = format!("{{\"symbol_entries\":{},\"unwind_entries\":{},\"initializer_entries\":{},\"relocated_code_pointers\":{},\"executable_bytes\":{},\"decoded_instructions\":{},\"compiled_instructions\":{},\"compiled_bytes\":{},\"compiled_blocks\":{},\"fused_regions\":{},\"exported_heads\":{},\"internal_static_edges\":{},\"guarded_bytes\":{},\"hle_frontiers\":{},\"budget_frontiers\":{},\"unsupported\":{{", self.symbol_entries, self.unwind_entries, self.initializer_entries, self.relocated_code_pointers, self.executable_bytes, self.decoded_instructions, self.compiled_instructions, self.compiled_bytes, self.compiled_blocks, self.fused_regions, self.exported_heads, self.internal_static_edges, self.guarded_bytes, self.hle_frontiers, self.budget_frontiers);
        for (i, (mn, n)) in self.unsupported.iter().enumerate() {
            if i != 0 { s.push(','); }
            write!(s, "\"{mn}\":{n}").unwrap();
        }
        write!(s, "}},\"fallback_instructions\":{},\"fallback_bytes\":{},\"invalid_frontiers\":{}}}", self.fallback_instructions, self.fallback_bytes, self.invalid_frontiers).unwrap();
        s
    }
}

/// Function seeds must be justified by ELF metadata, not a sweep of .text. The
/// supported unwind index is the GNU ELF32 encoding shipped by these plugins:
/// version 1, pcrel/sdata4 frame pointer, udata4 count, datarel/sdata4 table.
/// Unknown encodings fail closed instead of guessing instruction boundaries.
fn plugin_entries(image: &[u8], loads: &[Segment]) -> Result<(Vec<u32>, Vec<Segment>, DiscoveryReport), String> {
    let word = |o: usize| -> Result<u32, String> {
        let bytes = image.get(o..o.checked_add(4).ok_or("ELF offset overflow")?).ok_or("truncated ELF section")?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    };
    let shoff = word(32)? as usize;
    let stride = u16::from_le_bytes(image[46..48].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(image[48..50].try_into().unwrap()) as usize;
    let names_index = u16::from_le_bytes(image[50..52].try_into().unwrap()) as usize;
    if stride < 40 || shoff.checked_add(stride * count).is_none_or(|n| n > image.len()) {
        return Err("invalid ELF section table".into());
    }
    let section_bytes = |section: usize| -> Result<&[u8], String> {
        let offset = word(section + 16)? as usize;
        let len = word(section + 20)? as usize;
        image.get(offset..offset.checked_add(len).ok_or("ELF section size overflow")?).ok_or_else(|| "invalid ELF section file range".into())
    };
    if names_index >= count { return Err("invalid ELF section names".into()); }
    let names = section_bytes(shoff + names_index * stride)?;
    // Executable PT_LOADs can also contain read-only data. For metadata discovery
    // narrow to file-backed SHF_ALLOC|SHF_EXECINSTR sections, and verify their
    // virtual/file mapping against PT_LOAD rather than trusting section flags alone.
    let mut segs = Vec::new();
    for i in 0..count {
        let section = shoff + i * stride;
        if word(section + 8)? & 6 != 6 || word(section + 4)? == 8 { continue; }
        let start = word(section + 12)?;
        let len = word(section + 20)?;
        if len == 0 { continue; }
        let end = start.checked_add(len).ok_or("ELF code section overflow")?;
        let file = word(section + 16)?;
        if !loads.iter().any(|s| start >= s.start && end <= s.end && file as u64 == s.file as u64 + (start - s.start) as u64) {
            return Err("ELF code section differs from executable PT_LOAD".into());
        }
        if segs.iter().any(|s: &Segment| start < s.end && s.start < end) { return Err("overlapping ELF code sections".into()); }
        section_bytes(section)?;
        segs.push(Segment { start, end, file });
    }
    if segs.is_empty() { return Err("no ELF executable sections".into()); }
    let mut entries = BTreeSet::new();
    let mut symbols = BTreeSet::new();
    let mut unwind = BTreeSet::new();
    let mut initializers = BTreeSet::new();
    let mut code_pointers = BTreeSet::new();
    let mut report = DiscoveryReport { executable_bytes: segs.iter().map(|s| (s.end - s.start) as usize).sum(), ..DiscoveryReport::default() };
    for i in 0..count {
        let section = shoff + i * stride;
        let name = names.get(word(section)? as usize..).ok_or("invalid ELF section name")?;
        let end = name.iter().position(|b| *b == 0).ok_or("unterminated ELF section name")?;
        let name = &name[..end];
        let kind = word(section + 4)?;
        // R_386_RELATIVE in allocated non-code data declares an actual pointer,
        // not a guessed integer in .text. Its addend can identify address-taken
        // functions/callbacks and switch labels omitted from a stripped symtab.
        // This is static address evidence, not a claim the pointer was executed.
        if kind == 9 {
            let data = section_bytes(section)?;
            if word(section + 36)? != 8 || data.len() % 8 != 0 { return Err("invalid ELF32 relocation table".into()); }
            for row in data.chunks_exact(8) {
                let address = u32::from_le_bytes(row[..4].try_into().unwrap());
                let info = u32::from_le_bytes(row[4..].try_into().unwrap());
                if info != 8 || address & 3 != 0 { continue; } // R_386_RELATIVE, STN_UNDEF
                for j in 0..count {
                    let h = shoff + j * stride;
                    let start = word(h + 12)?;
                    let len = word(h + 20)?;
                    if word(h + 8)? & 6 != 2 || word(h + 4)? == 8 || address < start || address as u64 + 4 > start as u64 + len as u64 { continue; }
                    let data = section_bytes(h)?;
                    let offset = (address - start) as usize;
                    let pc = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    if source(image, &segs, pc).is_some() { code_pointers.insert(pc); }
                    break;
                }
            }
        }
        if name == b".eh_frame_hdr" {
            let data = section_bytes(section)?;
            if data.get(..4) != Some(&[1, 0x1b, 0x03, 0x3b]) || data.len() < 12 { return Err("unsupported ELF unwind header encoding".into()); }
            let n = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
            if n > (data.len() - 12) / 8 { return Err("truncated ELF unwind index".into()); }
            let base = word(section + 12)?;
            let frame = base.checked_add(4).and_then(|a| a.checked_add_signed(i32::from_le_bytes(data[4..8].try_into().unwrap()))).ok_or("ELF unwind pointer overflow")?;
            let mut previous = None;
            for row in data[12..12 + n * 8].chunks_exact(8) {
                let pc = base.checked_add_signed(i32::from_le_bytes(row[..4].try_into().unwrap())).ok_or("ELF unwind address overflow")?;
                let fde = base.checked_add_signed(i32::from_le_bytes(row[4..].try_into().unwrap())).ok_or("ELF unwind FDE overflow")?;
                // Both pointers must have actual section-backed provenance. An FDE
                // pointer into text must not be accepted as an unwind declaration.
                let in_frame = (0..count).any(|j| {
                    let h = shoff + j * stride;
                    let Ok(start) = word(h + 12) else { return false };
                    let Ok(len) = word(h + 20) else { return false };
                    let Ok(name) = word(h).and_then(|n| names.get(n as usize..).ok_or_else(|| "invalid section name".into())) else { return false };
                    if !name.starts_with(b".eh_frame\0") || start != frame || fde < start || fde as u64 + 16 > start as u64 + len as u64 { return false; }
                    let Ok(data) = section_bytes(h) else { return false };
                    let offset = (fde - start) as usize;
                    let fde_len = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    let cie = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
                    // Verify the ordinary ELF32 FDE's initial location agrees
                    // with the search table; extended lengths/other encodings
                    // are intentionally not guessed by this plugin-specific path.
                    let location = i32::from_le_bytes(data[offset + 8..offset + 12].try_into().unwrap());
                    fde_len >= 12 && (offset as u64 + 4 + fde_len as u64) <= data.len() as u64 && cie != 0 && cie as usize <= offset + 4 && fde.checked_add(8).and_then(|a| a.checked_add_signed(location)) == Some(pc)
                });
                if previous.is_some_and(|a| a >= pc) || !in_frame || source(image, &segs, pc).is_none() { return Err("invalid ELF unwind function entry".into()); }
                previous = Some(pc);
                unwind.insert(pc);
            }
        }
        if matches!(kind, 14 | 15) || matches!(name, b".ctors" | b".dtors") {
            let data = section_bytes(section)?;
            if data.len() % 4 != 0 { return Err("invalid ELF initializer array".into()); }
            for bytes in data.chunks_exact(4) {
                let pc = u32::from_le_bytes(bytes.try_into().unwrap());
                if pc != 0 && pc != u32::MAX && source(image, &segs, pc).is_some() { initializers.insert(pc); }
            }
        }
        if !matches!(kind, 2 | 11) { continue; } // SHT_SYMTAB / SHT_DYNSYM
        let offset = word(section + 16)? as usize;
        let size = word(section + 20)? as usize;
        let link = word(section + 24)? as usize;
        let symbol_size = word(section + 36)? as usize;
        if link >= count || symbol_size < 16 || size % symbol_size != 0 || offset.checked_add(size).is_none_or(|n| n > image.len()) {
            return Err("invalid ELF dynamic symbol table".into());
        }
        let strings = shoff + link * stride;
        let start = word(strings + 16)? as usize;
        let len = word(strings + 20)? as usize;
        let strings = image.get(start..start.checked_add(len).ok_or("ELF string size overflow")?).ok_or("invalid ELF strings")?;
        for symbol in (offset..offset + size).step_by(symbol_size) {
            let name = word(symbol)? as usize;
            let name = strings.get(name..).ok_or("invalid ELF symbol name")?;
            let end = name.iter().position(|b| *b == 0).ok_or("unterminated ELF symbol name")?;
            let _ = &name[..end];
            let pc = word(symbol + 4)?;
            let index = u16::from_le_bytes(image[symbol + 14..symbol + 16].try_into().unwrap()) as usize;
            if image[symbol + 12] & 15 == 2 && index > 0 && index < count && source(image, &segs, pc).is_some() {
                symbols.insert(pc);
            }
        }
    }
    report.symbol_entries = symbols.len();
    report.unwind_entries = unwind.len();
    report.initializer_entries = initializers.len();
    report.relocated_code_pointers = code_pointers.len();
    entries.extend(symbols);
    entries.extend(unwind);
    entries.extend(initializers);
    entries.extend(code_pointers);
    Ok((entries.into_iter().collect(), segs, report))
}

/// Compatibility entry point for a single installation-sized bundle. Prefer
/// `compile_plugin_bundles` for real plugins, whose safe CFG exceeds 1024 blocks.
pub fn compile_plugin_startup(image: &[u8]) -> Result<Artifact, String> {
    let loads = segments(image, 0)?;
    let (_, segs, _) = plugin_entries(image, &loads)?;
    // Preserve the old two-export API; broad function coverage is deliberately a
    // separate batched API rather than silently dropping blocks from one module.
    let word = |o: usize| u32::from_le_bytes(image[o..o + 4].try_into().unwrap());
    let shoff = word(32) as usize;
    let stride = u16::from_le_bytes(image[46..48].try_into().unwrap()) as usize;
    let count = u16::from_le_bytes(image[48..50].try_into().unwrap()) as usize;
    let mut entries = Vec::new();
    for i in 0..count {
        let h = shoff + i * stride;
        if word(h + 4) != 11 { continue; }
        let strings = shoff + word(h + 24) as usize * stride;
        let start = word(strings + 16) as usize;
        let names = &image[start..start + word(strings + 20) as usize];
        let offset = word(h + 16) as usize;
        for at in (offset..offset + word(h + 20) as usize).step_by(word(h + 36) as usize) {
            let name = &names[word(at) as usize..];
            if (name.starts_with(b"PPP_InitializeModule\0") || name.starts_with(b"PPP_GetInterface\0")) && image[at + 12] & 15 == 2 && image[at + 14..at + 16] != [0, 0] { entries.push(word(at + 4)); }
        }
    }
    compile_source(image, &Options { load_bias: 0, entries, allowlist: Vec::new(), max_instructions: 100_000, max_blocks: 1024, layout: Layout::for_cpu(0, 1, 2, 0) }, segs, true, true)
}

/// Compile bounded direct CFGs from symbol/unwind/initializer function entries and
/// declared relocated pointers into code, traversing decoded unsupported operations
/// but never compiling those operations or inferring indirect targets. Fuse
/// nearby blocks into multi-node regions before partitioning offline to honor the
/// runtime's existing 1024-region/module installation limit.
pub fn compile_plugin_bundles(image: &[u8]) -> Result<(Vec<Artifact>, DiscoveryReport), String> {
    compile_plugin_bundles_with_limits(image, 1_000_000, 65_536)
}

/// Bounded variant for package size/startup tuning. At most 64 installation-sized
/// modules may be referenced by the existing browser accelerator manifest.
pub fn compile_plugin_bundles_with_limits(image: &[u8], max_instructions: usize, max_blocks: usize) -> Result<(Vec<Artifact>, DiscoveryReport), String> {
    compile_plugin_bundles_mode(image, max_instructions, max_blocks, false)
}

/// Compile all valid discovered non-HLE instructions, preserving exact known HLE
/// functions as explicit dispatcher frontiers before any node construction/fusion.
/// Defaults exhaust the configured maximum discovery budget; dynamic targets and
/// invalid streams remain explicitly outside the static coverage claim.
pub fn compile_plugin_complete_bundles(image: &[u8]) -> Result<(Vec<Artifact>, DiscoveryReport), String> {
    compile_plugin_complete_bundles_with_limits(image, 5_000_000, 1_048_576)
}

pub fn compile_plugin_complete_bundles_with_limits(image: &[u8], max_instructions: usize, max_blocks: usize) -> Result<(Vec<Artifact>, DiscoveryReport), String> {
    compile_plugin_bundles_mode(image, max_instructions, max_blocks, true)
}

fn compile_plugin_bundles_mode(image: &[u8], max_instructions: usize, max_blocks: usize, complete: bool) -> Result<(Vec<Artifact>, DiscoveryReport), String> {
    if max_blocks == 0 || max_blocks > 1_048_576 || max_instructions == 0 || max_instructions > 5_000_000 { return Err("plugin AOT requires 1..=1048576 discovery blocks and 1..=5000000 instructions".into()); }
    let loads = segments(image, 0)?;
    let (entries, segs, mut report) = plugin_entries(image, &loads)?;
    // Symbolic nonzero cache base: bind requires the instance's live cache and
    // tail-call capability. Engines without it retain ordinary JIT/interpreter.
    let options = Options { load_bias: 0, entries, allowlist: Vec::new(), max_instructions, max_blocks, layout: Layout::for_cpu(0, 1, 2, 3) };
    let artifacts = compile_source_batches_complete(image, &options, segs, true, true, 1024, &mut report, true, complete, complete)?;
    if artifacts.len() > 64 { return Err("fused plugin output exceeds 64 installable modules".into()); }
    Ok((artifacts, report))
}

/// Locate exact known Flash leaf functions in executable ELF bytes, then compile
/// their real integer instructions (not runtime intrinsics) entirely offline.
pub fn compile_flash_leaves(image: &[u8]) -> Result<Artifact, String> {
    let segs = segments(image, 0)?;
    let mut entries = Vec::new();
    let mut allowlist = Vec::new();
    for s in &segs {
        let data = &image[s.file as usize..(s.file + s.end - s.start) as usize];
        for code in [&crate::jit_x86::intr::FIND_CODE[..], &crate::jit_x86::intr::RANGE_CMP_CODE[..]] {
            for (i, bytes) in data.windows(code.len()).enumerate() {
                if bytes == code {
                    let a = s.start + i as u32;
                    entries.push(a);
                    allowlist.push((a, a + code.len() as u32));
                }
            }
        }
    }
    if entries.is_empty() { return Err("no exact supported Flash leaf functions found".into()); }
    compile_portable(image, &Options { load_bias: 0, entries, allowlist, max_instructions: 100_000, max_blocks: 1024, layout: Layout::for_cpu(0, 1, 2, 0) })
}

/// Prepare already-loaded guest code using absolute guest entries/allowlist. This
/// shares the offline decoder and never executes guest code or installs modules.
/// `load_bias` must be zero: relocation has already happened in live memory.
pub fn prepare(mem: &Memory, options: &Options) -> Result<Artifact, String> {
    if options.load_bias != 0 {
        return Err("prepare takes absolute addresses and requires load_bias=0".into());
    }
    let size =
        u32::try_from(mem.size()).map_err(|_| "guest memory size exceeds AOT address range")?;
    let mut segs: Vec<Segment> = Vec::new();
    for start in (0..size).step_by(PAGE_SIZE as usize) {
        if mem.page_prot(start) & (P_MAPPED | P_X) != (P_MAPPED | P_X) {
            continue;
        }
        let end = start.saturating_add(PAGE_SIZE).min(size);
        if let Some(last) = segs.last_mut().filter(|s| s.end == start) {
            last.end = end;
        } else {
            segs.push(Segment {
                start,
                end,
                file: start,
            });
        }
    }
    compile_source(mem.raw(), options, segs, false, false)
}

fn compile_source(image: &[u8], options: &Options, segs: Vec<Segment>, portable: bool, partial: bool) -> Result<Artifact, String> {
    let mut artifacts = compile_source_batches(image, options, segs, portable, partial, options.max_blocks, &mut DiscoveryReport::default())?;
    if artifacts.len() != 1 { return Err("expected a single AOT module".into()); }
    Ok(artifacts.remove(0))
}

fn compile_source_batches(image: &[u8], options: &Options, segs: Vec<Segment>, portable: bool, partial: bool, batch_size: usize, report: &mut DiscoveryReport) -> Result<Vec<Artifact>, String> {
    compile_source_batches_mode(image, options, segs, portable, partial, batch_size, report, false)
}

const FUSION_MAX_NODES: usize = 96;
const FUSION_MAX_INSTRUCTIONS: usize = 1500;
const FUSION_MAX_SPAN: usize = 8192;

struct Group {
    blocks: std::ops::Range<usize>,
    region: Region,
}

/// Guard one file-backed section/PT_LOAD mapping, but translate only the actual
/// blocks. In particular neither a gap nor an unsupported frontier becomes a node
/// or a branch target just because its bytes are protected by this guard.
fn group_blocks(image: &[u8], segs: &[Segment], blocks: &[Region], fuse: bool) -> Result<Vec<Group>, String> {
    let mut groups = Vec::new();
    let mut first = 0;
    while first < blocks.len() {
        let head = &blocks[first];
        if !fuse {
            groups.push(Group { blocks: first..first + 1, region: head.clone() });
            first += 1;
            continue;
        }
        let segment = segs.iter().find(|s| head.address >= s.start && head.address < s.end).ok_or("AOT block outside executable source")?;
        let mut end = first + 1;
        let mut tail = head.address + head.bytes.len() as u32;
        let mut instruction_count = head.instruction_count;
        while end < blocks.len() && end - first < FUSION_MAX_NODES {
            let next = &blocks[end];
            let next_tail = next.address + next.bytes.len() as u32;
            if next.address < tail || next_tail > segment.end
                || next.file_offset as u64 != segment.file as u64 + (next.address - segment.start) as u64
                || next_tail - head.address > FUSION_MAX_SPAN as u32
                || instruction_count + next.instruction_count > FUSION_MAX_INSTRUCTIONS
            {
                break;
            }
            instruction_count += next.instruction_count;
            tail = next_tail;
            end += 1;
        }
        let starts: BTreeSet<_> = blocks[first..end].iter().map(|r| r.address).collect();
        let successors: BTreeSet<_> = blocks[first..end].iter().flat_map(|r| r.successors.iter().copied()).filter(|a| !starts.contains(a)).collect();
        // A fused region can have several dynamic exits; this legacy summary is
        // populated only when all of them have the same kind. Actual lowering
        // and dispatcher semantics are described by the nodes, not this label.
        let dynamic: BTreeSet<_> = blocks[first..end].iter().filter_map(|r| r.dynamic_exit).collect();
        let len = (tail - head.address) as usize;
        if tail > segment.end || head.file_offset as u64 + len as u64 > image.len() as u64 {
            return Err("AOT guard crosses executable source boundary".into());
        }
        groups.push(Group {
            blocks: first..end,
            region: Region {
                address: head.address,
                file_offset: head.file_offset,
                bytes: image[head.file_offset as usize..head.file_offset as usize + len].to_vec(),
                instruction_count,
                successors: successors.into_iter().collect(),
                dynamic_exit: if dynamic.len() == 1 { dynamic.first().copied() } else { None },
            },
        });
        first = end;
    }
    Ok(groups)
}

fn compile_source_batches_mode(image: &[u8], options: &Options, segs: Vec<Segment>, portable: bool, partial: bool, batch_size: usize, report: &mut DiscoveryReport, fuse: bool) -> Result<Vec<Artifact>, String> {
    compile_source_batches_complete(image, options, segs, portable, partial, batch_size, report, fuse, false, false)
}

fn compile_source_batches_complete(image: &[u8], options: &Options, segs: Vec<Segment>, portable: bool, partial: bool, batch_size: usize, report: &mut DiscoveryReport, fuse: bool, complete: bool, preserve_hle: bool) -> Result<Vec<Artifact>, String> {
    if options.entries.is_empty() || options.max_instructions == 0 || options.max_blocks == 0 {
        return Err("explicit entries and nonzero discovery limits required".into());
    }
    if (options.layout.cache != 0 && !portable)
        || options.layout.prof != 0
        || options.layout.trust_loads
        || options.layout.slow_inline
    {
        return Err("AOT requires profiling off, checked loads and slow_inline=false; cache symbols require portable mode".into());
    }
    let relocate = |a: u32| {
        a.checked_add(options.load_bias)
            .ok_or_else(|| "address overflow".to_string())
    };
    let ranges: Vec<_> = options
        .allowlist
        .iter()
        .map(|&(a, b)| {
            if a >= b {
                return Err("empty/reversed allowlist range".into());
            }
            Ok((relocate(a)?, relocate(b)?))
        })
        .collect::<Result<_, String>>()?;
    let allowed = |a: u32| ranges.is_empty() || ranges.iter().any(|&(s, e)| a >= s && a < e);
    let mut entries = options
        .entries
        .iter()
        .map(|&a| relocate(a))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable();
    entries.dedup();
    for &a in &entries {
        if !allowed(a) || source(image, &segs, a).is_none() {
            return Err(format!(
                "entry {a:#x} is not allowed executable file-backed code"
            ));
        }
    }
    // An exact known HLE function is deliberately not AOT code. Find its full
    // byte range before discovery: metadata may also seed an interior label, and
    // skipping only the entry would let a fused Wasm edge bypass the faster HLE.
    // This scans for byte signatures only, never decodes/sweeps arbitrary text.
    let mut hle_ranges = BTreeMap::new();
    if preserve_hle {
        for segment in &segs {
            let data = &image[segment.file as usize..segment.file as usize + (segment.end - segment.start) as usize];
            for pattern in [&jit_x86::intr::FIND_CODE[..], &jit_x86::intr::RANGE_CMP_CODE[..], &jit_x86::intr::BSEARCH_CODE[..]] {
                for (at, bytes) in data.windows(pattern.len()).enumerate() {
                    if bytes == pattern {
                        let start = segment.start + at as u32;
                        hle_ranges.insert(start, start + pattern.len() as u32);
                    }
                }
            }
        }
    }
    let mut hle_frontiers = BTreeSet::new();
    let mut heads: BTreeSet<u32> = entries.iter().copied().collect();
    heads.extend(segs.iter().map(|s| s.start));
    let mut queue: VecDeque<_> = entries.iter().copied().collect();
    let mut decoded: BTreeMap<u32, Decoded> = BTreeMap::new();
    let mut occupied = BTreeMap::new();
    while let Some(pc) = queue.pop_front() {
        if decoded.contains_key(&pc) || !allowed(pc) {
            continue;
        }
        // Leaving this image (or the profile allowlist) is a normal dispatcher
        // edge, not a promise that all possible callees have been compiled.
        let Some((offset, bytes)) = source(image, &segs, pc) else {
            continue;
        };
        if let Some((&start, &end)) = hle_ranges.range(..=pc).next_back() {
            if pc < end {
                if hle_frontiers.insert(start) { report.hle_frontiers += 1; }
                continue;
            }
        }
        if partial && !complete && [&jit_x86::intr::FIND_CODE[..], &jit_x86::intr::RANGE_CMP_CODE[..], &jit_x86::intr::BSEARCH_CODE[..]].iter().any(|pattern| bytes.starts_with(pattern)) {
            report.hle_frontiers += 1;
            continue; // Keep the established, faster HLE entry path intact.
        }
        // Keep the explicit discovery budgets independent of the fused export
        // count. Unfinished queued addresses remain visible in coverage; fewer
        // Wasm modules are not evidence of greater static image coverage.
        // Strict/single-module callers still get a hard error.
        if partial && batch_size < options.max_blocks && (decoded.len() >= options.max_instructions || heads.len().saturating_add(2) > options.max_blocks) {
            report.budget_frontiers = queue.iter().copied().chain([pc]).filter(|a| !decoded.contains_key(a) && allowed(*a)).collect::<BTreeSet<_>>().len();
            break;
        }
        if decoded.len() >= options.max_instructions {
            return Err("instruction discovery limit exceeded".into());
        }
        let mut decoder = Decoder::with_ip(32, bytes, pc as u64, DecoderOptions::NONE);
        let ins = decoder.decode();
        if ins.is_invalid() {
            if partial { report.invalid_frontiers += 1; continue; }
            return Err(format!("invalid/truncated instruction at {pc:#x}"));
        }
        // Only audited paths use inline symbolic emitters. Partial mode excludes
        // other instructions; complete mode emits generic calls for them offline.
        let mut supported = complete || !portable || portable_instruction(&ins);
        if !supported && !partial {
            return Err(format!("instruction {:?} at {pc:#x} is outside portable AOT subset", ins.mnemonic()));
        }
        let end = pc
            .checked_add(ins.len() as u32)
            .ok_or("instruction address overflow")?;
        if hle_ranges.range(pc..end).next().is_some() {
            return Err(format!("instruction overlaps preserved HLE frontier at {pc:#x}"));
        }
        if partial && !complete {
            let control_ok = match ins.flow_control() {
                FlowControl::Next => true,
                FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch | FlowControl::Call => ins.op0_kind() == OpKind::NearBranch32,
                FlowControl::Return => matches!(ins.code(), Code::Retnd | Code::Retnd_imm16),
                FlowControl::IndirectCall => ins.code() == Code::Call_rm32,
                FlowControl::IndirectBranch => ins.code() == Code::Jmp_rm32,
                FlowControl::Interrupt => ins.mnemonic() == Mnemonic::Int && matches!(ins.immediate8(), 0x80 | 0x82),
                _ => false,
            };
            if !control_ok { continue; }
            // Reject unsupported operand forms before they enter the CFG. This
            // is offline lowering only, and is never called by bind/install.
            if supported {
                let node = vec![NodeIn { start: pc, end, ins: std::slice::from_ref(&ins), intrinsic: 0, thunk: 0 }];
                let mut fallback = false;
                jit_x86::translate_module_relocatable(&options.layout, &[node], &mut |_| { fallback = true; 0 }, &mut FbStats::new(), 0);
                supported = !fallback;
            }
            if !supported { *report.unsupported.entry(format!("{:?}", ins.mnemonic())).or_default() += 1; }
        }
        if !(pc..end).all(allowed) {
            return Err(format!("instruction crosses allowlist boundary at {pc:#x}"));
        }
        for a in pc..end {
            if occupied.insert(a, pc).is_some() {
                return Err(format!("overlapping instruction streams at {pc:#x}"));
            }
        }
        let mut successors = Vec::new();
        let mut terminal = true;
        let mut dynamic = None;
        match ins.flow_control() {
            FlowControl::Next if ins.mnemonic() == Mnemonic::Pause => {
                dynamic = Some("pause");
                successors.push(end);
            }
            FlowControl::Next => {
                successors.push(end);
                terminal = false;
            }
            FlowControl::ConditionalBranch
            | FlowControl::UnconditionalBranch
            | FlowControl::Call => {
                if ins.op0_kind() != OpKind::NearBranch32 {
                    if !complete { return Err(format!("unsupported far branch at {pc:#x}")); }
                    // The generic fallback executes the exact far/16-bit control
                    // operation. Only a valid direct near target is static evidence.
                    if ins.op0_kind() == OpKind::NearBranch16 { successors.push(ins.near_branch16() as u32); }
                    dynamic = Some(if ins.flow_control() == FlowControl::Call { "indirect-call" } else { "indirect-jump" });
                    if ins.flow_control() != FlowControl::UnconditionalBranch { successors.push(end); queue.push_back(end); }
                } else {
                    successors.push(ins.near_branch32());
                    if ins.flow_control() != FlowControl::UnconditionalBranch { successors.push(end); }
                }
            }
            FlowControl::Return if matches!(ins.code(), Code::Retnd | Code::Retnd_imm16) => {
                dynamic = Some("return")
            }
            FlowControl::IndirectCall if portable && ins.code() == Code::Call_rm32 => {
                dynamic = Some("indirect-call");
                successors.push(end);
                queue.push_back(end);
            }
            FlowControl::IndirectBranch if portable && ins.code() == Code::Jmp_rm32 => dynamic = Some("indirect-jump"),
            FlowControl::Interrupt
                if ins.mnemonic() == Mnemonic::Int && matches!(ins.immediate8(), 0x80 | 0x82) =>
            {
                dynamic = Some("interrupt");
                successors.push(end);
            }
            _ if complete => {
                dynamic = Some(match ins.flow_control() {
                    FlowControl::Return => "return",
                    FlowControl::IndirectCall => "indirect-call",
                    FlowControl::IndirectBranch => "indirect-jump",
                    _ => "interrupt",
                });
                if matches!(ins.flow_control(), FlowControl::IndirectCall | FlowControl::Interrupt) {
                    successors.push(end);
                    queue.push_back(end);
                }
            }
            _ => {
                return Err(format!(
                    "unsupported control flow {:?} at {pc:#x}",
                    ins.flow_control()
                ))
            }
        }
        if terminal || !supported {
            heads.extend(successors.iter().copied());
        }
        if dynamic.is_none() {
            queue.extend(successors.iter().copied());
        }
        decoded.insert(
            pc,
            Decoded {
                ins,
                offset,
                successors,
                terminal,
                dynamic,
                supported,
            },
        );
    }
    let mut consumed = BTreeSet::new();
    if partial {
        entries.retain(|a| decoded.get(a).is_some_and(|d| d.supported));
    }
    let mut regions = Vec::new();
    let mut instructions = Vec::new();
    // Include every non-fallthrough target and seed as a block boundary, even if
    // it was decoded earlier through a straight-line predecessor.
    let mut pending = heads.clone();
    while let Some(start) = pending.pop_first() {
        if !decoded.get(&start).is_some_and(|d| d.supported) || consumed.contains(&start) {
            continue;
        }
        if regions.len() >= options.max_blocks {
            return Err("basic block limit exceeded".into());
        }
        let first = &decoded[&start];
        let mut pc = start;
        let mut block = Vec::new();
        let last = loop {
            let d = &decoded[&pc];
            consumed.insert(pc);
            block.push(d.ins);
            let next = pc + d.ins.len() as u32;
            let split = fuse && (block.len() >= FUSION_MAX_INSTRUCTIONS
                || decoded.get(&next).is_some_and(|d| next as u64 + d.ins.len() as u64 - start as u64 > FUSION_MAX_SPAN as u64));
            if split && !d.terminal && decoded.get(&next).is_some_and(|d| d.supported) {
                heads.insert(next);
                pending.insert(next);
            }
            if d.terminal || heads.contains(&next) || !decoded.get(&next).is_some_and(|d| d.supported) {
                break d;
            }
            pc = next;
        };
        let len = (pc - start) as usize + last.ins.len();
        regions.push(Region {
            address: start,
            file_offset: first.offset,
            bytes: image[first.offset as usize..first.offset as usize + len].to_vec(),
            instruction_count: block.len(),
            successors: last.successors.clone(),
            dynamic_exit: last.dynamic,
        });
        instructions.push(block);
    }
    report.decoded_instructions = decoded.len();
    report.compiled_instructions = consumed.len();
    report.compiled_blocks = regions.len();
    report.compiled_bytes = regions.iter().map(|r| r.bytes.len()).sum();
    if consumed.len() != decoded.values().filter(|d| d.supported).count() {
        return Err("internal CFG coverage error".into());
    }
    if regions.is_empty() { return Err("no supported non-HLE AOT entries".into()); }
    let groups = group_blocks(image, &segs, &regions, fuse)?;
    report.fused_regions = groups.len();
    report.guarded_bytes = groups.iter().map(|g| g.region.bytes.len()).sum();
    for group in &groups {
        let starts: BTreeSet<_> = regions[group.blocks.clone()].iter().map(|r| r.address).collect();
        for i in group.blocks.clone() {
            let r = &regions[i];
            let node = NodeIn { start: r.address, end: r.address + r.bytes.len() as u32, ins: &instructions[i], intrinsic: 0, thunk: 0 };
            let (target, continuation) = jit_x86::successors(&node);
            // Interrupt/pause/indirect-call continuations are dispatcher exits,
            // not internal static transfers, even if their target is grouped.
            report.internal_static_edges += target.into_iter().chain(continuation).filter(|a| starts.contains(a)).count();
        }
    }
    let mut artifacts = Vec::new();
    let input_id = identity(image);
    for groups in groups.chunks(batch_size) {
        let nodes: Vec<_> = groups.iter().map(|group| {
            group.blocks.clone().map(|i| {
                let r = &regions[i];
                NodeIn { start: r.address, end: r.address + r.bytes.len() as u32, ins: &instructions[i], intrinsic: 0, thunk: 0 }
            }).collect()
        }).collect();
        let mut fallback = Vec::new();
        let mut fallback_error = None;
        let (wasm, outputs, relocations) = jit_x86::translate_module_relocatable(
            &options.layout,
            &nodes,
            &mut |ins| {
                let tag = fallback.len() as u32;
                if let Some((_, bytes)) = source(image, &segs, ins.ip32()).filter(|(_, bytes)| bytes.len() >= ins.len()) {
                    fallback.push(Fallback { ip: ins.ip32(), bytes: bytes[..ins.len()].to_vec() });
                    if complete { *report.unsupported.entry(format!("{:?}", ins.mnemonic())).or_default() += 1; }
                } else { fallback_error = Some(format!("fallback instruction has no source bytes at {:#x}", ins.ip32())); }
                tag
            },
            &mut FbStats::new(),
            0,
        );
        if let Some(error) = fallback_error { return Err(error); }
        if !complete && (!fallback.is_empty() || outputs.iter().any(|o| o.n_fallback != 0)) {
            return Err(format!("runtime interpreter tags required at {:?}; AOT rejects fallback instructions", fallback.iter().map(|r| r.ip).collect::<Vec<_>>()));
        }
        report.fallback_instructions += fallback.len();
        report.fallback_bytes += fallback.iter().map(|r| r.bytes.len()).sum::<usize>();
        if outputs.iter().zip(groups).any(|(out, group)| out.n_ins as usize != group.region.instruction_count) {
            return Err("fused AOT instruction accounting mismatch".into());
        }
        let actual_heads: BTreeSet<_> = outputs.iter().flat_map(|out| out.heads.iter().map(|&(a, _)| a)).collect();
        report.exported_heads += actual_heads.len();
        let mut batch_entries: Vec<_> = entries.iter().copied().filter(|a| actual_heads.contains(a)).collect();
        if batch_entries.is_empty() { batch_entries.push(groups[0].region.address); }
        let artifact = Artifact {
            bundle_version: if complete { 4 } else if fuse { 3 } else if portable { 2 } else { 1 },
            relocations: portable.then_some(relocations),
            fallback,
            wasm,
            layout: options.layout.clone(),
            load_bias: options.load_bias,
            entries: batch_entries,
            allowlist: options.allowlist.clone(),
            input_size: image.len(),
            input_id,
            regions: groups.iter().map(|g| g.region.clone()).collect(),
            outputs,
        };
        artifact.validate_fallback()?;
        artifacts.push(artifact);
    }
    Ok(artifacts)
}

pub(crate) fn portable_instruction(ins: &Instruction) -> bool {
    use Mnemonic::*;
    if matches!(ins.code(), Code::Leavew | Code::Retnw | Code::Retnw_imm16) { return false; }
    if matches!(ins.flow_control(), FlowControl::ConditionalBranch | FlowControl::UnconditionalBranch | FlowControl::Call) && ins.op0_kind() != OpKind::NearBranch32 { return false; }
    matches!(ins.mnemonic(), Mov | Movzx | Movsx | Lea | Push | Pop | Leave | Nop | Add | Sub | Adc | Sbb | And | Or | Xor | Cmp | Test | Inc | Dec | Neg | Not | Shl | Shr | Sar | Sal | Imul | Xchg | Cdq | Cwde | Ret | Call | Jmp | Int | Pause)
        || matches!(ins.mnemonic(), Movss | Movsd | Movq | Movd | Movapd | Movaps | Movupd | Movups | Movdqa | Movdqu | Xorpd | Xorps | Pxor | Andpd | Andps | Pand | Orpd | Orps | Por | Cvtsi2sd)
            && (0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_xmm())
        || matches!(ins.mnemonic(), Prefetchnta | Prefetcht0 | Prefetcht1 | Prefetcht2 | Prefetchw | Prefetch | Lfence | Sfence | Mfence | Wait)
        || matches!(ins.mnemonic(), Cbw | Cwd | Shld | Shrd | Mul | Div | Idiv | Rol | Ror | Bt | Bts | Btr | Btc | Bsf | Bsr | Bswap | Xadd | Cmpxchg | Lahf | Sahf | Clc | Stc | Cmc | Cld | Std | Cmovo | Cmovno | Cmovb | Cmovae | Cmove | Cmovne | Cmovbe | Cmova | Cmovs | Cmovns | Cmovp | Cmovnp | Cmovl | Cmovge | Cmovle | Cmovg)
        || matches!(ins.flow_control(), FlowControl::ConditionalBranch)
        || matches!(ins.mnemonic(), Seta | Setae | Setb | Setbe | Sete | Setg | Setge | Setl | Setle | Setne | Setno | Setnp | Setns | Seto | Setp | Sets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{Cpu, Exit, ARITH};
    use crate::mem::{P_R, P_W};
    use wasmi::{Caller, Engine, Linker, Module, Store, TypedFunc};

    const PC: u32 = 0x1000;
    const CPU: usize = 0x1000;
    // Finite loop and diamond, ending at int 0x80: eax=10, ebx=42, ecx=0.
    const PROGRAM: &[u8] = &[
        0xb8, 0, 0, 0, 0, 0xb9, 4, 0, 0, 0, 0x01, 0xc8, 0x49, 0x75, 0xfb, 0x83, 0xf8, 10, 0x75, 7,
        0xbb, 42, 0, 0, 0, 0xeb, 5, 0xbb, 99, 0, 0, 0, 0xcd, 0x80,
    ];

    fn put16(b: &mut [u8], o: usize, n: u16) {
        b[o..o + 2].copy_from_slice(&n.to_le_bytes());
    }
    fn put32(b: &mut [u8], o: usize, n: u32) {
        b[o..o + 4].copy_from_slice(&n.to_le_bytes());
    }
    fn get32(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn elf(code: &[u8]) -> Vec<u8> {
        let mut b = vec![0; 0x100 + code.len()];
        b[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        put16(&mut b, 16, 2);
        put16(&mut b, 18, 3);
        put32(&mut b, 20, 1);
        put32(&mut b, 24, PC);
        put32(&mut b, 28, 52);
        put16(&mut b, 40, 52);
        put16(&mut b, 42, 32);
        put16(&mut b, 44, 1);
        put32(&mut b, 52, 1);
        put32(&mut b, 56, 0x100);
        put32(&mut b, 60, PC);
        put32(&mut b, 68, code.len() as u32);
        put32(&mut b, 72, code.len() as u32);
        put32(&mut b, 76, 5);
        put32(&mut b, 80, 0x1000);
        b[0x100..].copy_from_slice(code);
        b
    }
    fn metadata_elf() -> Vec<u8> {
        let mut b = elf(&[0xc3; 16]);
        put16(&mut b, 16, 3);
        b.resize(0x600, 0);
        // The executable PT_LOAD deliberately includes data/metadata: discovery
        // must still restrict code pointers and CFGs to .text, not all PF_X bytes.
        put32(&mut b, 68, 0x300); put32(&mut b, 72, 0x300);
        put32(&mut b, 32, 0x400); put16(&mut b, 46, 40); put16(&mut b, 48, 9); put16(&mut b, 50, 1);
        let names = b"\0.shstrtab\0.text\0.dynstr\0.dynsym\0.eh_frame_hdr\0.eh_frame\0.ctors\0.rel.dyn\0";
        b[0x180..0x180 + names.len()].copy_from_slice(names);
        let strings = b"\0PPP_GetInterface\0";
        b[0x1d0..0x1d0 + strings.len()].copy_from_slice(strings);
        let rows = [
            (b".shstrtab".as_slice(), 3, 0, 0, 0x180, names.len() as u32, 0, 0),
            (b".text", 1, 6, PC, 0x100, 16, 0, 0),
            (b".dynstr", 3, 2, 0, 0x1d0, strings.len() as u32, 0, 0),
            (b".dynsym", 11, 2, 0, 0x200, 48, 3, 16),
            (b".eh_frame_hdr", 1, 2, 0x1100, 0x240, 20, 0, 0),
            (b".eh_frame", 1, 2, 0x1200, 0x280, 36, 0, 0),
            (b".ctors", 1, 3, 0x1300, 0x300, 16, 0, 0),
            (b".rel.dyn", 9, 2, 0, 0x340, 16, 0, 8),
        ];
        for (i, (name, kind, flags, addr, file, size, link, entsize)) in rows.into_iter().enumerate() {
            let h = 0x400 + (i + 1) * 40;
            let name_offset = names.windows(name.len() + 1).position(|v| v[..name.len()] == *name && v[name.len()] == 0).unwrap();
            for (off, value) in [(0, name_offset as u32), (4, kind), (8, flags), (12, addr), (16, file), (20, size), (24, link), (36, entsize)] { put32(&mut b, h + off, value); }
        }
        put32(&mut b, 0x210, 1); put32(&mut b, 0x214, PC); b[0x21c] = 0x12; put16(&mut b, 0x21e, 2);
        put32(&mut b, 0x220, 1); put32(&mut b, 0x224, PC + 8); b[0x22c] = 0x11; put16(&mut b, 0x22e, 2); // STT_OBJECT is NOT a function seed
        b[0x240..0x244].copy_from_slice(&[1, 0x1b, 3, 0x3b]);
        put32(&mut b, 0x244, 0xfc); put32(&mut b, 0x248, 1);
        put32(&mut b, 0x24c, (PC + 4).wrapping_sub(0x1100)); put32(&mut b, 0x250, 0x110);
        put32(&mut b, 0x280, 12); // CIE followed by one ordinary FDE
        put32(&mut b, 0x290, 16); put32(&mut b, 0x294, 20);
        put32(&mut b, 0x298, (PC + 4).wrapping_sub(0x1218)); put32(&mut b, 0x29c, 1);
        put32(&mut b, 0x300, u32::MAX); put32(&mut b, 0x304, PC + 6); put32(&mut b, 0x308, 0); put32(&mut b, 0x30c, PC + 0x100);
        put32(&mut b, 0x340, 0x1304); put32(&mut b, 0x344, 8);
        put32(&mut b, 0x348, 0x130c); put32(&mut b, 0x34c, 8); // pointer to non-code PF_X data must be ignored
        b
    }
    fn options() -> Options {
        let mut layout = Layout::for_cpu(0x400000, 0x10000, 0x110000, 0);
        layout.simd = false;
        Options {
            load_bias: 0,
            entries: vec![PC],
            allowlist: vec![],
            max_instructions: 1000,
            max_blocks: 100,
            layout,
        }
    }
    fn memory(code: &[u8]) -> Memory {
        let mut m = Memory::new(0x10000);
        m.set_prot(PC, 0x1000, P_MAPPED | P_R | P_W | P_X);
        m.write_bytes(PC, code).unwrap();
        m
    }
    fn error(code: &[u8], o: &Options) -> String {
        compile(&elf(code), o).err().expect("must reject")
    }

    #[test]
    fn discovery_guards_and_bundle_roundtrip() {
        let image = elf(PROGRAM);
        let a = compile(&image, &options()).unwrap();
        assert_eq!(a.regions.len(), 6);
        assert_eq!(a.outputs.len(), a.regions.len());
        assert!(a.outputs.iter().all(|o| o.n_fallback == 0 && o.n_intr == 0));
        assert_eq!(
            a.regions.iter().map(|r| r.instruction_count).sum::<usize>(),
            11
        );
        assert!(a.matches_input(&image));
        let mut changed = image.clone();
        changed[0x101] ^= 1;
        assert!(!a.matches_input(&changed));
        let mut mem = memory(PROGRAM);
        assert!(a.guard(&mem, &a.layout));
        let mut wrong = a.layout.clone();
        wrong.cpu_r += 4;
        assert!(!a.guard(&mem, &wrong));
        mem.w8(PC + 1, 1).unwrap();
        assert!(!a.guard(&mem, &a.layout));
        mem.w8(PC + 1, 0).unwrap();
        mem.set_prot(PC, 0x1000, P_MAPPED | P_R);
        assert!(!a.guard(&mem, &a.layout));
        let bundle = a.encode_bundle();
        let b = Artifact::decode(&bundle).unwrap();
        assert_eq!(a.manifest_json(), b.manifest_json());
        assert_eq!(bundle, b.encode_bundle());
        for n in [0, 7, 8, 12, bundle.len() / 2, bundle.len() - 1] {
            assert!(Artifact::decode(&bundle[..n]).is_err());
        }
        let mut trailing = bundle.clone();
        trailing.push(0);
        assert!(Artifact::decode(&trailing).is_err());
        let mut bad = bundle;
        bad[7] = 99;
        assert!(Artifact::decode(&bad).is_err());
    }

    #[test]
    fn bounded_rejects_indirection_fallback_overlap_and_bad_elf() {
        let o = options();
        assert!(error(&[0xff, 0xe0], &o).contains("unsupported control flow")); // jmp eax
        assert!(error(&[0xff, 0xd0], &o).contains("unsupported control flow")); // call eax
        assert!(error(&[0x66, 0xc3], &o).contains("unsupported control flow")); // 16-bit ret is not lowered correctly by the shared translator
        assert!(error(&[0x0f, 0xa2, 0xcd, 0x80], &o).contains("runtime interpreter tags")); // cpuid
        assert!(error(&[0x0f], &o).contains("invalid/truncated"));
        let mut limit = o.clone();
        limit.max_instructions = 1;
        assert!(error(PROGRAM, &limit).contains("instruction discovery limit"));
        limit = o.clone();
        limit.max_blocks = 1;
        assert!(error(PROGRAM, &limit).contains("basic block limit"));
        limit = o.clone();
        limit.entries.push(PC + 1);
        assert!(error(PROGRAM, &limit).contains("overlapping instruction"));
        limit = o.clone();
        limit.entries.clear();
        assert!(error(PROGRAM, &limit).contains("explicit entries"));
        for len in 0..52 {
            assert!(compile(&vec![0; len], &o).is_err());
        }
        let mut image = elf(PROGRAM);
        put32(&mut image, 56, u32::MAX);
        assert!(compile(&image, &o).is_err());
    }

    #[test]
    fn allowlist_external_edges_relocation_and_loaded_prepare() {
        let mut o = options();
        o.allowlist = vec![(PC, PC + 10)];
        let a = compile(&elf(PROGRAM), &o).unwrap();
        assert_eq!(a.regions.len(), 1);
        assert_eq!(a.regions[0].successors, [PC + 10]);
        // A direct call outside the supplied image is an explicit dispatcher edge.
        let a = compile(&elf(&[0xe8, 0xfb, 0x0f, 0, 0, 0xcd, 0x80]), &options()).unwrap();
        assert_eq!(a.regions[0].successors, [PC + 0x1000, PC + 5]);
        let mut image = elf(PROGRAM);
        put16(&mut image, 16, 3);
        o = options();
        o.load_bias = 0x2000;
        let a = compile(&image, &o).unwrap();
        assert_eq!(a.entries, [PC + 0x2000]);
        assert_eq!(a.regions[1].address, PC + 0x2000 + 10);
        assert!(Artifact::decode(&a.encode_bundle()).is_ok());
        o = options();
        let m = memory(PROGRAM);
        let a = prepare(&m, &o).unwrap();
        assert_eq!(a.regions.len(), 6);
        assert!(a.guard(&m, &o.layout));
        assert_eq!(a.regions[0].file_offset, PC);
        o.load_bias = 1;
        assert!(prepare(&m, &o).is_err());
        // Supported dynamic return exit; no attempt to guess its destination.
        let a = compile(&elf(&[0xc3]), &options()).unwrap();
        assert_eq!(a.regions[0].dynamic_exit, Some("return"));
    }

    struct Machine {
        engine: Engine,
        store: Store<()>,
        linker: Linker<()>,
        memory: wasmi::Memory,
        table: wasmi::Table,
        funcs: Vec<Option<TypedFunc<(i32, i32), i32>>>,
        layout: Layout,
    }
    impl Machine {
        fn new(layout: Layout) -> Self {
            let engine = Engine::default();
            let mut store = Store::new(&engine, ());
            let memory = wasmi::Memory::new(&mut store, wasmi::MemoryType::new(1, None)).unwrap();
            let table = wasmi::Table::new(
                &mut store,
                wasmi::TableType::new(wasmi::RefType::Func, 0, None),
                wasmi::Ref::Func(wasmi::Nullable::Null),
            )
            .unwrap();
            let mut linker = Linker::new(&engine);
            linker.define("env", "memory", memory).unwrap();
            linker.define("env", "table", table).unwrap();
            linker
                .func_wrap("env", "exec_one", |_: i32, _: i32| -> i32 {
                    panic!("no fallback tags permitted")
                })
                .unwrap();
            linker
                .func_wrap(
                    "env",
                    "load_slow",
                    |_: i32, _: i32, _: i32, _: i32| -> i32 { panic!("unexpected load helper") },
                )
                .unwrap();
            linker
                .func_wrap(
                    "env",
                    "store_slow",
                    |_: i32, _: i32, _: i32, _: i32, _: i32| -> i32 {
                        panic!("unexpected store helper")
                    },
                )
                .unwrap();
            let fl_off = layout.cpu_fl as usize;
            linker
                .func_wrap(
                    "env",
                    "mat",
                    move |caller: Caller<'_, ()>,
                          cpu: i32,
                          lk: i32,
                          a: i32,
                          b: i32,
                          r: i32,
                          _ip: i32|
                          -> i32 {
                        let fl = get32(memory.data(&caller), cpu as usize + fl_off);
                        if lk == 0 {
                            fl as i32
                        } else {
                            ((fl & !ARITH)
                                | crate::jit_flags::eval(lk as u32, a as u32, b as u32, r as u32))
                                as i32
                        }
                    },
                )
                .unwrap();
            linker
                .func_wrap("env", "sys_time", |_: i32| -> i32 { 1 })
                .unwrap();
            linker
                .func_wrap(
                    "env",
                    "fist_fild",
                    |_: i32, _: i32, _: i32, _: i32| -> i32 { panic!("unexpected x87 helper") },
                )
                .unwrap();
            linker
                .func_wrap("env", "fcomip", |_: i32, _: i32, _: i32| -> i32 {
                    panic!("unexpected x87 helper")
                })
                .unwrap();
            linker
                .func_wrap(
                    "env",
                    "cvtss2si",
                    |_: i32, _: i32, _: i32, _: i32, _: i32| -> i32 {
                        panic!("unexpected SSE helper")
                    },
                )
                .unwrap();
            linker
                .func_wrap(
                    "env",
                    "mm_op",
                    |_: i32, _: i32, _: i32, _: i32, _: i32| -> i32 {
                        panic!("unexpected MMX helper")
                    },
                )
                .unwrap();
            Self {
                engine,
                store,
                linker,
                memory,
                table,
                funcs: Vec::new(),
                layout,
            }
        }
        fn call_cpu(&mut self, slot: u32, cpu: &mut Cpu, head: u32) -> u32 {
            let l = &self.layout;
            let image = self.memory.data_mut(&mut self.store);
            for (i, &r) in cpu.r.iter().enumerate() {
                put32(image, CPU + l.cpu_r as usize + 4 * i, r);
            }
            for (off, v) in [
                (l.cpu_eip, cpu.eip),
                (l.cpu_fl, cpu.fl),
                (l.cpu_lf_kind, cpu.lf_kind),
                (l.cpu_lf_a, cpu.lf_a),
                (l.cpu_lf_b, cpu.lf_b),
                (l.cpu_lf_r, cpu.lf_r),
                (l.cpu_budget, cpu.jit_budget as u32),
                (l.cpu_fs_base, cpu.fs_base),
                (l.cpu_gs_base, cpu.gs_base),
                (l.cpu_mxcsr, cpu.mxcsr),
            ] {
                put32(image, CPU + off as usize, v);
            }
            for (i, value) in cpu.xmm.iter().enumerate() {
                let off = CPU + l.cpu_xmm as usize + i * 16;
                image[off..off + 16].copy_from_slice(&value.to_le_bytes());
            }
            let status = self.funcs[slot as usize]
                .as_ref()
                .unwrap()
                .call(&mut self.store, (CPU as i32, head as i32))
                .unwrap() as u32;
            let image = self.memory.data(&self.store);
            for (i, r) in cpu.r.iter_mut().enumerate() {
                *r = get32(image, CPU + l.cpu_r as usize + 4 * i);
            }
            cpu.eip = get32(image, CPU + l.cpu_eip as usize);
            cpu.fl = get32(image, CPU + l.cpu_fl as usize);
            cpu.lf_kind = get32(image, CPU + l.cpu_lf_kind as usize);
            cpu.lf_a = get32(image, CPU + l.cpu_lf_a as usize);
            cpu.lf_b = get32(image, CPU + l.cpu_lf_b as usize);
            cpu.lf_r = get32(image, CPU + l.cpu_lf_r as usize);
            cpu.jit_budget = get32(image, CPU + l.cpu_budget as usize) as i32;
            cpu.mxcsr = get32(image, CPU + l.cpu_mxcsr as usize);
            for (i, value) in cpu.xmm.iter_mut().enumerate() {
                let off = CPU + l.cpu_xmm as usize + i * 16;
                *value = u128::from_le_bytes(image[off..off + 16].try_into().unwrap());
            }
            status
        }
    }
    impl crate::jit::rt::Backend for Machine {
        fn install(&mut self, bytes: &[u8], n: usize) -> Option<Vec<u32>> {
            let module = Module::new(&self.engine, bytes).expect("AOT wasm validation");
            let instance = self
                .linker
                .instantiate_and_start(&mut self.store, &module)
                .unwrap();
            Some(
                (0..n)
                    .map(|i| {
                        let slot = self.funcs.len() as u32;
                        self.funcs.push(Some(
                            instance
                                .get_typed_func(&self.store, &format!("r{i}"))
                                .unwrap(),
                        ));
                        self.table.grow(&mut self.store, 1, wasmi::Ref::Func(wasmi::Nullable::Null)).unwrap();
                        self.table.set(&mut self.store, slot as u64, wasmi::Ref::Func(wasmi::Nullable::Val(*self.funcs[slot as usize].as_ref().unwrap().func()))).unwrap();
                        slot
                    })
                    .collect(),
            )
        }
        fn free(&mut self, slot: u32) {
            self.funcs[slot as usize] = None;
        }
        fn call(&mut self, slot: u32, cpu: &mut Cpu, head: u32) -> u32 {
            self.call_cpu(slot, cpu, head)
        }
    }

    #[test]
    fn generated_wasm_executes_and_matches_interpreter() {
        use crate::jit::rt::Backend;
        let a = compile(&elf(PROGRAM), &options()).unwrap();
        let mut machine = Machine::new(a.layout.clone());
        let slots = machine.install(&a.wasm, a.regions.len()).unwrap();
        let mut actual = Cpu::new();
        actual.eip = PC;
        actual.jit_budget = 100;
        let mut done = false;
        for _ in 0..100 {
            let i = a
                .regions
                .iter()
                .position(|r| r.address == actual.eip)
                .expect("static CFG dispatch");
            let status = machine.call_cpu(slots[i], &mut actual, 0);
            if status == jit_x86::S_SYSCALL {
                done = true;
                break;
            }
            assert_eq!(status, jit_x86::S_NEXT);
        }
        assert!(done);
        actual.lf_flush();
        let mut expected = Cpu::new();
        expected.eip = PC;
        let mut mem = memory(PROGRAM);
        let mut exit = None;
        for _ in 0..100 {
            let ins = Decoder::with_ip(
                32,
                &mem.raw()[expected.eip as usize..],
                expected.eip as u64,
                DecoderOptions::NONE,
            )
            .decode();
            expected.eip = ins.next_ip32();
            if let Err(e) = expected.exec(&mut mem, &ins) {
                exit = Some(e);
                break;
            }
        }
        assert_eq!(exit, Some(Exit::Syscall));
        assert_eq!(actual.r, expected.r);
        assert_eq!(actual.eip, expected.eip);
        assert_eq!(actual.fl, expected.fl);
        assert_eq!(actual.r[0], 10);
        assert_eq!(actual.r[3], 42);
    }

    #[test]
    fn portable_chaining_cache_relocations_follow_live_instance_layout() {
        let mut image = elf(PROGRAM); put16(&mut image, 16, 3);
        let mut config = options(); config.layout = Layout::for_cpu(0, 1, 2, 3);
        let portable = compile_portable(&image, &config).unwrap();
        assert!(portable.relocations.as_ref().unwrap().iter().any(|r| r.kind == 4));
        for cache_address in [0x50000, 0x70000] {
            let mut mem = Memory::new(0x10000);
            mem.set_prot(PC + 0x2000, 0x1000, P_MAPPED | P_R | P_W | P_X);
            mem.write_bytes(PC + 0x2000, PROGRAM).unwrap();
            let mut cache = crate::run::BlockCache::new(); cache.jit.threshold = 1000;
            cache.jit.layout_override = Some((0x400000, 0x10000, 0x110000, cache_address));
            let layout = cache.jit.lir_layout(&mem);
            let bound = portable.bind(&layout, 0x2000).unwrap();
            for relocation in portable.relocations.as_ref().unwrap().iter().filter(|r| r.kind == 4) {
                let at = relocation.offset as usize;
                assert_eq!(bound.wasm[at..at+5], crate::wasm_emit::fixed_leb(cache_address + relocation.value - 3, false));
            }
            use crate::jit::rt::Backend;
            let mut machine = Machine::new(layout);
            machine.memory.grow(&mut machine.store, 100).unwrap();
            let slots = machine.install(&bound.wasm, bound.regions.len()).unwrap();
            let mut cpu = Cpu::new(); cpu.eip = PC + 0x2000; cpu.jit_budget = 1000;
            for _ in 0..20 {
                let i = bound.regions.iter().position(|r| r.address == cpu.eip).unwrap();
                if machine.call_cpu(slots[i], &mut cpu, 0) == jit_x86::S_SYSCALL { break; }
            }
            assert_eq!((cpu.r[0], cpu.r[3]), (10, 42));
        }
    }

    #[test]
    fn portable_bundle_rebinds_runs_and_invalidates_at_two_load_bases() {
        let mut image = elf(PROGRAM);
        put16(&mut image, 16, 3);
        let mut options = options();
        options.layout = Layout::for_cpu(0, 1, 2, 0);
        let encoded = compile_portable(&image, &options).unwrap().encode_bundle();
        for bias in [0x2000, 0x7000] {
            let portable = Artifact::decode(&encoded).unwrap();
            let mut mem = Memory::new(0x10000);
            mem.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W | P_X);
            mem.write_bytes(PC + bias, PROGRAM).unwrap();
            let mut cache = crate::run::BlockCache::default();
            cache.jit.threshold = 1000;
            let layout = cache.jit.aot_layout(&mem);
            let bound = portable.bind(&layout, bias).unwrap();
            assert!(bound.guard(&mem, &layout));
            assert!(bound.outputs.iter().all(|o| o.n_fallback == 0));
            crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = Some(Box::new(Machine::new(layout))));
            assert_eq!(bound.install(&mut mem, &mut cache.jit).unwrap(), 6);
            let mut cpu = Cpu::new();
            cpu.eip = PC + bias;
            assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
            assert_eq!((cpu.r[0], cpu.r[3]), (10, 42));
            mem.w8(PC + bias + 1, 1).unwrap();
            cache.sync(&mut mem);
            assert!(!cache.jit.has_entry(PC + bias));
            assert!(bound.install(&mut mem, &mut cache.jit).is_err());
            cache.jit.flush_all();
            crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = None);
        }
    }

    #[test]
    fn portable_memory_calls_and_constant_collisions_use_symbolic_relocations() {
        use crate::jit::rt::Backend;
        // mov eax,[ebx]; add eax,0x1000; call +2; int80; add eax,1; ret.
        // The immediate is numerically equal to PC, and MUST NOT be relocated.
        let code = [0x8b, 0x03, 0x05, 0, 0x10, 0, 0, 0xe8, 2, 0, 0, 0, 0xcd, 0x80, 0x83, 0xc0, 1, 0xc3];
        let mut image = elf(&code);
        put16(&mut image, 16, 3);
        let mut options = options();
        options.layout = Layout::for_cpu(0, 1, 2, 0);
        let portable = compile_portable(&image, &options).unwrap();
        for (bias, gb, rt, wt) in [(0x2000, 0x40000, 0x20000, 0x30000), (0x7000, 0x80000, 0x60000, 0x70000)] {
            let mut layout = Layout::for_cpu(gb, rt, wt, 0);
            layout.simd = false;
            let bound = portable.bind(&layout, bias).unwrap();
            let mut machine = Machine::new(layout);
            machine.memory.grow(&mut machine.store, 32).unwrap();
            let data = machine.memory.data_mut(&mut machine.store);
            data[rt as usize..rt as usize + 16].fill(3);
            data[wt as usize..wt as usize + 16].fill(3);
            put32(data, gb as usize + 0x2000, 42);
            let slots = machine.install(&bound.wasm, bound.regions.len()).unwrap();
            let mut cpu = Cpu::new();
            cpu.eip = PC + bias;
            cpu.r[3] = 0x2000;
            cpu.r[4] = 0xf000;
            cpu.jit_budget = 100;
            let mut done = false;
            for _ in 0..20 {
                let index = bound.regions.iter().position(|r| r.address == cpu.eip).unwrap();
                let status = machine.call_cpu(slots[index], &mut cpu, 0);
                if status == jit_x86::S_SYSCALL { done = true; break; }
                assert_eq!(status, jit_x86::S_NEXT);
            }
            assert!(done);
            assert_eq!(cpu.r[0], 42 + 0x1000 + 1);
            assert_eq!(cpu.r[4], 0xf000);
            assert_eq!(get32(machine.memory.data(&machine.store), gb as usize + 0xeffc), PC + bias + 12);
        }
        let mut bad_layout = portable.layout.clone();
        bad_layout.cpu_eip += 4;
        assert!(portable.bind(&bad_layout, 0).is_err());
        let mut corrupt = portable.encode_bundle();
        *corrupt.last_mut().unwrap() ^= 0x80;
        assert!(Artifact::decode(&corrupt).is_err());
        // Unsupported helper-emitter instructions cannot accidentally get a
        // nominally portable artifact with missing pointer/IP relocations.
        assert!(compile_portable(&elf(&[0xd9, 0xe8, 0xc3]), &options).is_err());
        assert!(compile_portable(&elf(&[0x0f, 0xa2, 0xc3]), &options).is_err());
        for code in [&[0xff, 0xe0][..], &[0xff, 0xd0, 0xcd, 0x80][..]] {
            let a = compile_portable(&elf(code), &options).unwrap();
            assert!(a.regions[0].dynamic_exit.unwrap().starts_with("indirect-"));
            assert!(Artifact::decode(&a.encode_bundle()).is_ok());
        }
    }

    #[test]
    fn offline_exact_flash_leaf_discovery_compiles_real_code() {
        let mut code = crate::jit_x86::intr::FIND_CODE.to_vec();
        code.extend_from_slice(&crate::jit_x86::intr::RANGE_CMP_CODE);
        let a = compile_flash_leaves(&elf(&code)).unwrap();
        assert_eq!(a.entries, [PC, PC + 104]);
        assert!(a.relocations.as_ref().unwrap().len() > 10);
        assert!(a.outputs.iter().all(|o| o.n_intr == 0 && o.n_fallback == 0));
        assert_eq!(Artifact::decode(&a.encode_bundle()).unwrap().encode_bundle(), a.encode_bundle());
    }

    #[test]
    fn prepared_bundle_installs_runs_and_invalidates_through_real_registry() {
        let mut mem = memory(PROGRAM);
        let mut cache = crate::run::BlockCache::default();
        cache.jit.threshold = 1000;
        let mut o = options();
        o.layout = cache.jit.aot_layout(&mem);
        let a = Artifact::decode(&prepare(&mem, &o).unwrap().encode_bundle()).unwrap();
        crate::jit::rt::BACKEND
            .with(|b| *b.borrow_mut() = Some(Box::new(Machine::new(o.layout.clone()))));
        assert_eq!(a.install(&mut mem, &mut cache.jit).unwrap(), 6);
        assert!(cache.jit.has_entry(PC));
        assert!(a.install(&mut mem, &mut cache.jit).is_err()); // no duplicate publication
        let mut cpu = Cpu::new();
        cpu.eip = PC;
        assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
        assert_eq!(cpu.r[0], 10);
        assert_eq!(cpu.r[3], 42);
        mem.w8(PC + 1, 1).unwrap();
        assert!(mem.needs_sync());
        cache.sync(&mut mem);
        assert!(!cache.jit.has_entry(PC));
        assert!(a.install(&mut mem, &mut cache.jit).is_err()); // stale byte guard
        cache.jit.flush_all();
        crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = None);
    }

    #[test]
    fn partial_discovery_crosses_only_decoded_frontiers_and_batches_offline() {
        let code = [0xb8, 1, 0, 0, 0, 0x0f, 0xa2, 0x83, 0xc0, 2, 0xff, 0xd3, 0xcd, 0x80];
        let image = elf(&code);
        let mut o = options();
        o.layout = Layout::for_cpu(0, 1, 2, 0);
        let mut report = DiscoveryReport::default();
        let artifacts = compile_source_batches(&image, &o, segments(&image, 0).unwrap(), true, true, 1, &mut report).unwrap();
        assert_eq!(artifacts.len(), 3);
        assert_eq!(report.unsupported.get("Cpuid"), Some(&1));
        assert_eq!(report.decoded_instructions, 5);
        assert_eq!(report.compiled_instructions, 4);
        assert_eq!(artifacts[0].regions[0].successors, [PC + 5]);
        assert_eq!(artifacts[1].regions[0].address, PC + 7);
        assert_eq!(artifacts[1].regions[0].dynamic_exit, Some("indirect-call"));
        assert_eq!(artifacts[2].regions[0].address, PC + 12);
        for a in artifacts {
            assert_eq!(a.outputs[0].n_fallback, 0);
            assert!(Artifact::decode(&a.encode_bundle()).is_ok());
        }
        // A decodable unsupported entry preserves later static code, but bytes
        // after a return are NOT swept, and known HLE leaves are NOT replaced.
        let a = compile_portable_partial(&elf(&[0x0f, 0xa2, 0x40, 0xc3, 0x43]), &o).unwrap();
        assert_eq!(a.regions.len(), 1);
        assert_eq!(a.regions[0].bytes, [0x40, 0xc3]);
        let mut code = vec![0xe8, 2, 0, 0, 0, 0xcd, 0x80];
        code.extend(crate::jit_x86::intr::FIND_CODE);
        let a = compile_portable_partial(&elf(&code), &o).unwrap();
        assert_eq!(a.regions.len(), 2);
        assert!(a.regions.iter().all(|r| r.address < PC + 7));
        let mut overlap = o;
        overlap.entries.push(PC + 1);
        assert!(compile_portable_partial(&image, &overlap).err().unwrap().contains("overlapping"));
    }

    #[test]
    fn metadata_discovery_validates_symbols_unwind_relocations_and_code_sections() {
        let image = metadata_elf();
        let discover = |b: &[u8]| plugin_entries(b, &segments(b, 0).unwrap());
        let (entries, segs, report) = discover(&image).unwrap();
        assert_eq!(entries, [PC, PC + 4, PC + 6]);
        assert_eq!(segs.len(), 1);
        assert_eq!(report.executable_bytes, 16);
        assert_eq!((report.symbol_entries, report.unwind_entries, report.initializer_entries, report.relocated_code_pointers), (1, 1, 1, 1));
        let (artifacts, _) = compile_plugin_bundles(&image).unwrap();
        assert_eq!(artifacts[0].regions.len(), 1);
        assert_eq!(artifacts[0].outputs[0].heads.iter().map(|&(a, _)| a).collect::<Vec<_>>(), entries);
        for (at, value) in [(0x248, 2), (0x250, 0), (0x298, 0), (0x400 + 2 * 40 + 16, 0x101), (0x400 + 4 * 40 + 24, 99), (0x400 + 8 * 40 + 36, 12)] {
            let mut bad = image.clone(); put32(&mut bad, at, value);
            assert!(discover(&bad).is_err(), "accepted bad metadata at {at:x}");
        }
        let mut bad = image.clone(); bad[0x243] = 0xff;
        assert!(discover(&bad).err().unwrap().contains("encoding"));
    }

    #[test]
    fn partitioned_discovery_reports_budget_frontiers_without_claiming_full_coverage() {
        let image = elf(PROGRAM);
        let mut o = options(); o.max_blocks = 4;
        let mut report = DiscoveryReport::default();
        let artifacts = compile_source_batches(&image, &o, segments(&image, 0).unwrap(), true, true, 1, &mut report).unwrap();
        assert!(report.budget_frontiers > 0);
        assert!(report.compiled_instructions < 11);
        assert!(artifacts.len() <= 4);
    }

    #[test]
    fn portable_sse_bit_shapes_execute_at_two_layouts_and_bases() {
        use crate::jit::rt::Backend;
        // Every admitted SSE mnemonic and operand direction, including scalar
        // merge/zero rules. Compare independently with the real interpreter.
        let mut shapes: Vec<Vec<u8>> = Vec::new();
        for prefix in [0xf3, 0xf2] {
            for (op, rm) in [(0x10, 0xc1), (0x10, 0x03), (0x11, 0x03)] { shapes.push(vec![prefix, 0x0f, op, rm]); }
        }
        for (prefix, load, store) in [(Some(0x66), 0x28, 0x29), (None, 0x28, 0x29), (Some(0x66), 0x10, 0x11), (None, 0x10, 0x11), (Some(0x66), 0x6f, 0x7f), (Some(0xf3), 0x6f, 0x7f)] {
            for (op, rm) in [(load, 0xc1), (load, 0x03), (store, 0x03)] {
                let mut b: Vec<u8> = prefix.into_iter().collect(); b.extend([0x0f, op, rm]); shapes.push(b);
            }
        }
        shapes.extend([vec![0xf3, 0x0f, 0x7e, 0xc1], vec![0xf3, 0x0f, 0x7e, 0x03], vec![0x66, 0x0f, 0xd6, 0x03]]);
        for (op, rm) in [(0x6e, 0xc0), (0x6e, 0x03), (0x7e, 0xc0), (0x7e, 0x03)] { shapes.push(vec![0x66, 0x0f, op, rm]); }
        for (prefix, op) in [(Some(0x66), 0x57), (None, 0x57), (Some(0x66), 0xef), (Some(0x66), 0x54), (None, 0x54), (Some(0x66), 0xdb), (Some(0x66), 0x56), (None, 0x56), (Some(0x66), 0xeb)] {
            for rm in [0xc1, 0x03] { let mut b: Vec<u8> = prefix.into_iter().collect(); b.extend([0x0f, op, rm]); shapes.push(b); }
        }
        shapes.extend([vec![0xf2, 0x0f, 0x2a, 0xc0], vec![0xf2, 0x0f, 0x2a, 0x03]]);
        shapes.push(vec![0x64, 0xf2, 0x0f, 0x10, 0x03]); // FS is live state, not an ELF relocation
        for hint in [0x03, 0x0b, 0x13, 0x1b] { shapes.push(vec![0x0f, 0x18, hint]); }
        shapes.extend([vec![0x0f, 0x0d, 0x03], vec![0x0f, 0x0d, 0x0b], vec![0x0f, 0xae, 0xe8], vec![0x0f, 0xae, 0xf8], vec![0x0f, 0xae, 0xf0], vec![0x9b]]);
        for shape in shapes {
            let mut code = shape.clone(); code.extend([0xcd, 0x80]);
            let mut image = elf(&code); put16(&mut image, 16, 3);
            let mut o = options(); o.layout = Layout::for_cpu(0, 1, 2, 0);
            let portable = compile_portable(&image, &o).unwrap();
            assert!(portable.outputs.iter().all(|o| o.n_fallback == 0));
            for (bias, gb, rt, wt) in [(0x2000, 0x40000, 0x20000, 0x30000), (0x7000, 0x80000, 0x60000, 0x70000)] {
                let layout = Layout::for_cpu(gb, rt, wt, 0);
                let bound = portable.bind(&layout, bias).unwrap();
                let mut machine = Machine::new(layout);
                machine.memory.grow(&mut machine.store, 32).unwrap();
                let bytes = machine.memory.data_mut(&mut machine.store);
                bytes[rt as usize..rt as usize + 16].fill(3);
                bytes[wt as usize..wt as usize + 16].fill(3);
                let pattern: Vec<u8> = (0..64).map(|i| (i * 17 + 3) as u8).collect();
                bytes[gb as usize + 0x2000..gb as usize + 0x2040].copy_from_slice(&pattern);
                let mut actual = Cpu::new(); actual.eip = PC + bias; actual.jit_budget = 100;
                actual.r[0] = 0xfedcba98; actual.r[3] = 0x2000; actual.fs_base = 16;
                actual.xmm[0] = 0xffab_cd12_3456_7890_1234_5678_ffff_ffff;
                actual.xmm[1] = 0x0123_4567_89ab_cdef_ffe0_9876_5432_1000;
                let mut expected = actual.clone();
                let mut mem = Memory::new(0x10000);
                mem.set_prot(0x2000, 0x1000, P_MAPPED | P_R | P_W);
                mem.write_bytes(0x2000, &pattern).unwrap();
                let ins = Decoder::with_ip(32, &shape, (PC + bias) as u64, DecoderOptions::NONE).decode();
                expected.eip = ins.next_ip32(); expected.exec(&mut mem, &ins).unwrap();
                let slots = machine.install(&bound.wasm, bound.regions.len()).unwrap();
                assert_eq!(machine.call_cpu(slots[0], &mut actual, 0), jit_x86::S_SYSCALL, "{shape:02x?}");
                assert_eq!(actual.r, expected.r, "{shape:02x?}");
                assert_eq!(actual.xmm, expected.xmm, "{shape:02x?}");
                assert_eq!(actual.mxcsr, expected.mxcsr, "{shape:02x?}");
                assert_eq!(&machine.memory.data(&machine.store)[gb as usize + 0x2000..gb as usize + 0x2040], &mem.raw()[0x2000..0x2040], "{shape:02x?}");
                assert_eq!(actual.eip, PC + bias + code.len() as u32);
            }
        }
    }

    #[test]
    fn portable_sse_memory_guards_retry_before_state_changes() {
        use crate::jit::rt::Backend;
        for code in [&[0x66, 0x0f, 0x28, 0x03][..], &[0x66, 0x0f, 0x29, 0x03], &[0xf2, 0x0f, 0x10, 0x03], &[0xf2, 0x0f, 0x11, 0x03]] {
            let mut code = code.to_vec(); code.extend([0xcd, 0x80]);
            let mut image = elf(&code); put16(&mut image, 16, 3);
            let mut o = options(); o.layout = Layout::for_cpu(0, 1, 2, 0);
            let portable = compile_portable(&image, &o).unwrap();
            for (bias, gb, rt, wt) in [(0x2000, 0x40000, 0x20000, 0x30000), (0x7000, 0x80000, 0x60000, 0x70000)] {
                for (addr, read, write) in [(0x2000, 0, 0), (0x2001, 3, 3), (0x2ffc, 1, 1), (0x2000, 3, 0)] {
                    let bound = portable.bind(&Layout::for_cpu(gb, rt, wt, 0), bias).unwrap();
                    let mut machine = Machine::new(bound.layout.clone()); machine.memory.grow(&mut machine.store, 32).unwrap();
                    let bytes = machine.memory.data_mut(&mut machine.store); bytes[rt as usize + 2] = read; bytes[wt as usize + 2] = write;
                    let slots = machine.install(&bound.wasm, bound.regions.len()).unwrap();
                    let mut cpu = Cpu::new(); cpu.eip = PC + bias; cpu.jit_budget = 100; cpu.r[3] = addr; cpu.xmm[0] = u128::MAX;
                    let before = cpu.clone();
                    let status = machine.call_cpu(slots[0], &mut cpu, 0);
                    let aligned = code[0] == 0x66;
                    let store = code[2] == 0x29 || code[2] == 0x11;
                    let retry = read == 0 || aligned && addr & 15 != 0 || addr == 0x2ffc || store && write == 0;
                    assert_eq!(status, if retry { jit_x86::S_RETRY } else { jit_x86::S_SYSCALL }, "{code:02x?} addr={addr:x}");
                    if retry { assert_eq!(cpu.eip, before.eip); assert_eq!(cpu.r, before.r); assert_eq!(cpu.xmm, before.xmm); }
                }
            }
        }
    }

    include!("aot_grouping_tests.rs");
    mod fallback_tests {
        use super::*;
        include!("aot_fallback_tests.rs");
    }
}
