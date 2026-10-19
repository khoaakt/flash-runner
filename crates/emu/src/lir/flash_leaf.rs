//! Narrow, fail-closed bridge for one binary-verified NanoJIT pure integer leaf shape.
//! The capture verifier must also establish that the raw LIR returns argv[1]-argv[2].
//! All other methods keep their original native implementation. No plugin code is patched.
use super::{Abi, AccessCheck, Argument, CodeBinding, Trace, WordSource};
use crate::{jit::JitState, mem::Memory};

pub fn verify_native_sub(bytes: &[u8], core: u32) -> bool {
    if bytes.len() != 78 { return false; }
    let fixed: &[(usize, &[u8])] = &[
        (0, &[0x55,0x8b,0xec,0x83,0xec,0x18,0x89,0x5d,0xfc,0x8b,0x4d,0x08,0x8b,0x55,0x10,0x8d,0x45,0xf0,0x8b,0x1d]),
        (24, &[0x89,0x4d,0xf4,0x89,0x5d,0xf0,0x89,0x05]),
        (36, &[0x8b,0x1d]),
        (42, &[0x3b,0xc3,0x8b,0x5d,0xfc,0x73,0x08,0xe8]),
        (54, &[0x8b,0x55,0x10,0x8b,0x42,0x04,0x8b,0x4a,0x08,0x2b,0xc1,0x8b,0x4d,0xf0,0x89,0x0d]),
        (74, &[0x8b,0xe5,0x5d,0xc3]),
    ];
    if fixed.iter().any(|(i, b)| bytes.get(*i..*i + b.len()) != Some(*b)) { return false; }
    let Some(frame) = core.checked_add(48) else { return false };
    let Some(limit) = core.checked_add(32) else { return false };
    for (at, value) in [(20, frame), (32, frame), (38, limit), (70, frame)] {
        if bytes[at..at+4] != value.to_le_bytes() { return false; }
    }
    true
}

pub fn install_sub_leaf(mem: &mut Memory, jit: &mut JitState, entry: u32, core: u32, bytes: &[u8]) -> Result<usize, String> {
    if !verify_native_sub(bytes, core) { return Err("native scalar leaf shape differs".into()); }
    let core_end = core.checked_add(52).ok_or("core address overflow")?;
    let limit = core.checked_add(32).ok_or("core address overflow")?;
    let frame = core.checked_add(48).ok_or("core address overflow")?;
    // The native MethodFrame is ESP-20 and its full stack allocation is ESP-28.
    // Refuse stack-overflow, aliasing, and fault-sensitive cases before any state change.
    let source = format!("params i32 i32 i32 i32 i32\na = parami 0\nb = parami 1\nsp = parami 2\nlimit = parami 3\nargv = parami 4\nn28 = immi 28\nn20 = immi 20\nn4 = immi 4\nend = immi 0x{core_end:08x}\nok0 = geui sp n28\nxf ok0 1\nlow = subi sp n28\nframe = subi sp n20\nok1 = geui frame limit\nxf ok1 2\nargs_min = addi sp n4\nok2 = geui argv args_min\nxf ok2 3\nok3 = leui end low\nxf ok3 4\nresult = subi a b\nreti result");
    let trace = Trace::parse(&source).map_err(|e| e.to_string())?;
    let abi = Abi {
        entry, guest_size: u32::try_from(mem.size()).map_err(|_| "guest too large")?,
        trusted_layout: true,
        arguments: vec![
            Argument::Argv { pointer: WordSource::Stack(12), offset: 4 },
            Argument::Argv { pointer: WordSource::Stack(12), offset: 8 },
            Argument::Word(WordSource::Gpr(4)), Argument::Word(WordSource::Memory(limit)),
            Argument::Word(WordSource::Stack(12)),
        ],
        checks: vec![(WordSource::Stack(8), 2)],
        access_checks: vec![
            AccessCheck { base: WordSource::Gpr(4), offset: -28, size: 28, write: false },
            AccessCheck { base: WordSource::Gpr(4), offset: -28, size: 28, write: true },
            AccessCheck { base: WordSource::Constant(frame), offset: 0, size: 4, write: false },
            AccessCheck { base: WordSource::Constant(frame), offset: 0, size: 4, write: true },
        ],
        bindings: vec![CodeBinding { address: entry, bytes: bytes.to_vec() }],
        instruction_cost: 48, recheck_bindings: false,
        count_completions: true,
    };
    let layout = jit.aot_layout(mem);
    let region = trace.compile_region(&layout, abi).map_err(|e| e.to_string())?;
    let bindings = region.contract.abi.bindings.iter().map(|b| (b.address, b.bytes.clone())).collect();
    let n = jit.install_precompiled(mem, &layout, &region.wasm, &[(region.region, bindings)])?;
    jit.mark_lir_entry(entry);
    Ok(n)
}
