//! Exact-build bounded integer loops from complete captured Flash .363 LIR.
//!
//! This is deliberately NOT a general loop optimizer. Every word of the raw LIR
//! stream and every byte of the native no-call path must match an audited shape.
//! Only addresses, the unreachable stack-overflow helper displacement, and a
//! checked pair of constant-blinding fields may vary. The emitted recurrence is
//! decoded from LIR loads/stores/arithmetic, not from source names or native code.
//!
//! Native evidence: nanojit-loops.json records 63/73 and loop-{63,73}.bin, captured
//! at assemble/setJit in the SHA-pinned .363 runtime. Read-only GNU objdump of those
//! bytes establishes the templates below; this is ordinary tool evidence, not a
//! retained REA Evidence record. Other spill/register shapes fail closed.
//! The later nanojit-loops-new/current.json captures also establish the exact
//! xorshift variant without the identity lea ecx,[ecx] alignment instruction.
//!
//! All success-path memory effects are scoped stack spills and a restored
//! MethodFrame link. We omit those only after proving stack/core/argv disjoint,
//! all native reads/writes permitted, no stack overflow, and core+40 == 0. Guest
//! execution is single-threaded and wasm performs no host calls until return, so
//! no active interrupt can be skipped. Out-of-range n or insufficient complete-
//! call budget returns S_RETRY without changing *any* Cpu/guest state; native
//! execution then makes slice-by-slice progress. The maximum trip count is 4096.

use crate::cpu::{Cpu, EAX, ESP};
use crate::jit_x86::{Layout, RegionOut, CACHE_ENTRIES, S_NEXT, S_RETRY};
use crate::wasm_emit::{op, Code, ModuleBuilder, I32};
use std::collections::{BTreeMap, BTreeSet};

const MAX_TRIPS: u32 = 4096;
// Strings enumerate ALL raw words, including metadata, unused loads/constants,
// side exits, and liveness records. @n is the address of record n; c is AvmCore;
// x is opaque metadata used only by a proven-unreachable native call path.
const SUM_LIR: &[&str] = &[
    "0", "100 4000000", "101 4000000", "102 4000000", "0 4000000", "1 4000000", "2 4000000", "3 6000000",
    "c 39000000", "1020030 @8 1b000000", "20004 @7 @4 23000000", "20000 @7 @9 23000000", "20030 @8 @7 23000000",
    "10 6000000", "2 6000000", "1020020 @8 1b000000", "@15 @7 44000000", "@19 @16 31000000", "x x 29000000",
    "33000000", "4 39000000", "20000 @6 1b000000", "3 39000000", "20004 @6 1b000400", "0 39000000", "0 39000400",
    "10 @13 @25 23000400", "18 @13 @25 23000400", "20 @13 @23 23000400", "@41 0 2f000000", "33000000",
    "2020028 @8 1b000000", "@51 @31 c0000000", "1000018 @13 1b000400", "4 39000000", "1 39000000",
    "@35 @33 5e000000", "18 @13 @36 23000400", "1000010 @13 1b000400", "@33 @38 5e000000", "10 @13 @39 23000400",
    "33000000", "1000018 @13 1b000400", "4 39000000", "1000020 @13 1b000400", "@44 @42 40000000",
    "@30 @45 30000000", "1000010 @13 1b000400", "1020000 @7 1b000000", "20030 @8 @48 23000000", "@47 7000000",
    "33000000", "c2000000", "1000000", "x x 29000000", "c3000000", "c1000000", "2000000",
    "@7 c000000", "@4 c000000", "@8 c000000", "@13 c000000", "@20 c000000",
];
const MIX_LIR: &[&str] = &[
    "0", "100 4000000", "101 4000000", "102 4000000", "0 4000000", "1 4000000", "2 4000000", "3 6000000",
    "c 39000000", "1020030 @8 1b000000", "20004 @7 @4 23000000", "20000 @7 @9 23000000", "20030 @8 @7 23000000",
    "14 6000000", "3 6000000", "1020020 @8 1b000000", "@15 @7 44000000", "@19 @16 31000000", "x x 29000000",
    "33000000", "4 39000000", "20000 @6 1b000000", "3 39000000", "20004 @6 1b000400", "20008 @6 1b000400",
    "0 39000000", "0 39000400", "18 @13 @24 23000400", "20 @13 @26 23000400", "28 @13 @23 23000400", "@51 0 2f000000",
    "33000000", "2020028 @8 1b000000", "@61 @32 c0000000", "1000020 @13 1b000400", "4 39000000", "1 39000000",
    "@36 @34 5e000000", "20 @13 @37 23000400", "1000018 @13 1b000400", "d 39000400", "@40 @39 67000000",
    "@41 @39 66000000", "11 39000400", "@43 @42 69000000", "5 39000000", "@44 @42 66000000", "5 39000400",
    "@47 @46 67000000", "@48 @46 66000000", "18 @13 @49 23000400", "33000000", "1000020 @13 1b000400", "4 39000000",
    "1000028 @13 1b000400", "@54 @52 40000000", "@31 @55 30000000", "1000018 @13 1b000400", "1020000 @7 1b000000",
    "20030 @8 @58 23000000", "@57 7000000", "33000000", "c2000000", "1000000", "x x 29000000", "c3000000", "c1000000",
    "2000000", "@7 c000000", "@4 c000000", "@8 c000000", "@13 c000000", "@20 c000000",
];
const SUM_NATIVE: &str = "558bec83ec68895dbc8975ac897da88b450889459c8b75108d5db0b920105d29894da08b3d50105d298945b4897db0891d50105d298b3d40105d293bdf73088b4d9ce87c54c501b8040000008945a48b5e048bf581eebd748640c7868d748640000000008bf581eebd748640c7869574864000000000895de0eb268b1d48105d2985db740b83ec0ce83e00000083c4108b75d88d5e01895dd88b5dd003de895dd08b5dd88b75e03bde0f8cccffffff8b7da88b75ac8b5dbc8b45088b45d08b4db0890d50105d298be55dc3";
const MIX_NATIVE: &str = "558bec83ec78895dac89759c897d988b450889458c8b7d108d5da0b920105d29894d908b3550105d298945a48975a0891d50105d298b3540105d293bde73088b4d8ce83956c501b8040000008945948b5f048b77088975c88d098bf581ee760b73b8c786460b73b800000000895dd8eb398b1d48105d2985db740b83ec0ce85100000083c4108b5dd08d5b01895dd08b5dc88bf3c1e60d33de8bf3c1ee1133de8bf3c1e60533de895dc88b5dd08b75d83bde0f8cb9ffffff8b7d988b759c8b5dac8b45088b45c88b4da0890d50105d298be55dc3";

#[derive(Clone, Debug)]
struct Record { address: u32, words: Vec<u32> }
impl Record {
    fn opcode(&self) -> u8 { (self.words.last().unwrap() >> 24) as u8 }
    fn operand(&self, n: usize) -> u32 { self.words[self.words.len() - 1 - n] }
}
fn word(bytes: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(bytes.get(at..at + 4).ok_or("truncated counted-loop metadata")?.try_into().unwrap()))
}
fn records(metadata: &[u8]) -> Result<Vec<Record>, String> {
    if metadata.len() > 8192 || ![63, 73].contains(&word(metadata, 0)?) { return Err("unsupported counted-loop record count".into()); }
    let count = word(metadata, 0)? as usize;
    let mut at = 4;
    let mut seen = BTreeSet::new();
    let mut spans = Vec::new();
    let mut result = Vec::new();
    for _ in 0..count {
        let address = word(metadata, at)?;
        let size = word(metadata, at + 4)? as usize;
        if ![4, 8, 12, 16].contains(&size) || address % 4 != 0 || !seen.insert(address)
            || address < size as u32 || address.checked_add(4).is_none() { return Err("invalid counted-loop raw record bounds".into()); }
        let low = address + 4 - size as u32;
        if spans.iter().any(|&(a, b)| low < b && a < address + 4) { return Err("overlapping counted-loop raw records".into()); }
        spans.push((low, address + 4));
        let mut words = Vec::new();
        for i in (0..size).step_by(4) { words.push(word(metadata, at + 8 + i)?); }
        result.push(Record { address, words });
        at += 8 + size;
    }
    if at != metadata.len() { return Err("trailing counted-loop metadata".into()); }
    Ok(result)
}
fn verify_records(rs: &[Record], core: u32) -> Result<(), String> {
    let template = if rs.len() == SUM_LIR.len() { SUM_LIR } else { MIX_LIR };
    for (index, (record, text)) in rs.iter().zip(template).enumerate() {
        let tokens: Vec<_> = text.split_whitespace().collect();
        if tokens.len() != record.words.len() { return Err(format!("counted-loop record size {index}")); }
        for (n, (&actual, token)) in record.words.iter().zip(tokens).enumerate() {
            let expected = if let Some(reference) = token.strip_prefix('@') {
                rs[reference.parse::<usize>().unwrap()].address
            } else if token == "c" { core }
            else if token == "x" {
                // Call arguments/CallInfo are never dereferenced on the admitted
                // no-overflow, zero-poll path; no arbitrary executed op is hidden.
                if actual == 0 || actual % 4 != 0 { return Err("invalid unreachable-call metadata pointer".into()); }
                continue;
            } else { u32::from_str_radix(token, 16).unwrap() };
            if actual != expected { return Err(format!("counted-loop raw word {index}:{n} differs")); }
        }
    }
    Ok(())
}
fn unhex(text: &str) -> Vec<u8> {
    text.as_bytes().chunks_exact(2).map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap()).collect()
}
fn put(bytes: &mut [u8], at: usize, value: u32) { bytes[at..at + 4].copy_from_slice(&value.to_le_bytes()); }

#[derive(Clone, Debug)]
pub struct Verified {
    len: usize,
    stack_low: i32,
    frame_offset: i32,
    argc: u32,
    // Conservative complete native instruction charge: base + n * per_trip.
    base_cost: u32,
    per_trip: u32,
    records: Vec<Record>,
}

/// Complete raw-stream plus exact native-path proof. The omitted side-call
/// targets cannot execute under runtime guards. Even their call instructions are
/// byte-bound; only their unused displacement is relocation-tolerant.
pub fn verify(native: &[u8], entry: u32, core: u32, metadata: &[u8]) -> Result<Verified, String> {
    if core.checked_add(52).is_none() || native.len() > 4096 || entry.checked_add(native.len() as u32).is_none() {
        return Err("invalid counted-loop native bounds".into());
    }
    let rs = records(metadata)?;
    verify_records(&rs, core)?;
    let sum = rs.len() == 63;
    let mut expected = unhex(if sum { SUM_NATIVE } else { MIX_NATIVE });
    // NanoJIT may place one exact ESP identity inside the *untaken* overflow
    // path. Normalize its bytes and the JAE's skip length together; no executed
    // instruction, arbitrary padding or relative target is ignored.
    let mut normalized = native.to_vec();
    let overflow_padding = normalized.get(0x3f..0x42) == Some(&[0x8d, 0x24, 0x24]);
    if overflow_padding {
        if normalized.get(0x3d..0x3f) != Some(&[0x73, 0x0b]) { return Err("counted-loop overflow padding branch differs".into()); }
        normalized.drain(0x3f..0x42);
        normalized[0x3e] = 8;
    }
    let native = normalized.as_slice();
    // The assembler sometimes pads the xorshift setup with lea ecx,[ecx] for
    // alignment. Admit exactly that two-byte identity or its complete absence;
    // never wildcard a native instruction or treat arbitrary bytes as padding.
    let mix_padding = !sum && native.get(0x58..0x5a) == Some(&[0x8d, 0x09]);
    let shift = if !sum && !mix_padding { expected.drain(0x58..0x5a); 2 } else { 0 };
    if native.len() < expected.len() { return Err("truncated counted-loop native path".into()); }
    for (at, delta) in [(28, 0), (37, 48), (49, 48), (55, 32), (if sum {125} else {115 - shift}, 40), (if sum {195} else {204 - shift}, 48)] {
        put(&mut expected, at, core + delta);
    }
    // jae at 0x3d skips the sole stack-overflow helper unconditionally once
    // frame >= core.stackLimit is established. Never reinterpret its target.
    expected[0x43..0x47].copy_from_slice(&native[0x43..0x47]);
    let blind = if sum { word(native, 0x56)? } else { word(native, 0x5e - shift)? };
    if sum {
        // ESI = EBP - blind; [ESI + displacement] must be exactly EBP-48/-40
        // modulo 2^32. Merely accepting both random fields would permit a heap
        // write. This arithmetic is checked even when blind has its sign bit set.
        for (at, value) in [(0x56, blind), (0x5c, blind.wrapping_sub(48)), (0x68, blind), (0x6e, blind.wrapping_sub(40))] { put(&mut expected, at, value); }
    } else {
        put(&mut expected, 0x5e - shift, blind);
        put(&mut expected, 0x64 - shift, blind.wrapping_sub(48));
    }
    if native[..expected.len()] != expected { return Err("counted-loop native effects/recurrence differ".into()); }
    // On zero-poll paths: sum has 13 native instructions/trip, mix 22.
    // 16/28 and fixed 80 conservatively include all setup, final condition,
    // return and optional alignment work; n<=4096 prevents charge overflow.
    Ok(Verified { len: expected.len() + if overflow_padding { 3 } else { 0 }, stack_low: if sum { -108 } else { -124 }, frame_offset: if sum { -84 } else { -100 },
        argc: if sum { 1 } else { 2 }, base_cost: 80, per_trip: if sum { 16 } else { 28 }, records: rs })
}

// r0 parameters: cpu, head. All locals i32 except completion counter is operated
// on directly in memory. Frame slots model LIR alloca(13), not guest memory.
const SP: u32 = 2;
const ARGV: u32 = 3;
const RET: u32 = 4;
const BUDGET: u32 = 5;
const LOW: u32 = 6;
const PAGE: u32 = 7;
const N: u32 = 8;
const COST: u32 = 9;
const SLOT_BASE: u32 = 10;
const VALUE_BASE: u32 = 18;
fn cpu(c: &mut Code, offset: u32) { c.lget(0).mem(op::I32_LOAD, offset); }
fn reject(c: &mut Code) { c.if_(0x40).u32c(S_RETRY).op(op::RETURN).end(); }
fn page_check(c: &mut Code, layout: &Layout, address: u32, size: u32, write: bool) {
    c.lget(address).u32c(4095).op(op::I32_AND).u32c(4096 - size).op(op::I32_GT_U); reject(c);
    c.lget(address).u32c(12).op(op::I32_SHR_U).ltee(PAGE).mem(op::I32_LOAD8_U, layout.rtab);
    if write { c.lget(PAGE).mem(op::I32_LOAD8_U, layout.wtab).op(op::I32_AND); }
    c.u32c(1).op(op::I32_AND).op(op::I32_EQZ); reject(c);
}
fn slot(offset: u32) -> u32 { SLOT_BASE + offset / 8 }

/// Emit the LIR frame recurrence as wasm locals. This decoder runs *after* the
/// complete structural proof, and cannot accept an extra dead/effect instruction.
fn emit_records(c: &mut Code, verified: &Verified, layout: &Layout, range: std::ops::Range<usize>) -> Result<(), String> {
    let rs = &verified.records;
    let by_address: BTreeMap<_, _> = rs.iter().enumerate().map(|(i, r)| (r.address, i)).collect();
    for index in range {
        let record = &rs[index];
        let value = |n: usize| -> Result<u32, String> {
            Ok(VALUE_BASE + *by_address.get(&record.operand(n)).ok_or("invalid LIR recurrence operand")? as u32)
        };
        match record.opcode() {
            57 => { c.u32c(record.words[0]).lset(VALUE_BASE + index as u32); }
            27 => {
                let offset = record.words[0] & 0xffff;
                if record.operand(1) == rs[6].address { c.lget(ARGV).mem(op::I32_LOAD, layout.guest_base + offset); }
                else if record.operand(1) == rs[13].address { c.lget(slot(offset)); }
                else { return Err("unproved LIR recurrence load".into()); }
                c.lset(VALUE_BASE + index as u32);
            }
            35 => { c.lget(value(1)?).lset(slot(record.words[0] & 0xffff)); }
            94 | 102 | 103 | 105 | 64 => {
                c.lget(value(1)?).lget(value(2)?).op(match record.opcode() {
                    94 => op::I32_ADD, 102 => op::I32_XOR, 103 => op::I32_SHL, 105 => op::I32_SHR_U, 64 => op::I32_LT_S, _ => unreachable!(),
                }).lset(VALUE_BASE + index as u32);
            }
            _ => return Err("unproved LIR recurrence instruction".into()),
        }
    }
    Ok(())
}

/// Trusted-layout integrated Cpu dispatcher adapter, never a standalone source
/// loop export. Only `verify` can construct Verified and its complete raw graph.
fn compile(verified: &Verified, layout: &Layout, entry: u32, core: u32, guest_size: u32) -> Result<(Vec<u8>, RegionOut), String> {
    let stack_size = verified.stack_low.unsigned_abs() + 16;
    let argv_size = (verified.argc + 1) * 4; // includes the native/LIR dead argv[0] read
    let core_end = core.checked_add(52).filter(|&end| end <= guest_size).ok_or("counted-loop core outside guest")?;
    if guest_size < 4096 || entry >= guest_size || core & 4095 > 4096 - 52 { return Err("counted-loop guest bounds".into()); }
    let native = Layout::for_cpu(layout.guest_base, layout.rtab, layout.wtab, layout.cache);
    if [layout.cpu_r, layout.cpu_eip, layout.cpu_budget] != [native.cpu_r, native.cpu_eip, native.cpu_budget] { return Err("counted-loop Cpu layout mismatch".into()); }
    let pages = (guest_size as u64).div_ceil(4096);
    let mut extents = vec![(layout.guest_base as u64, guest_size as u64), (layout.rtab as u64, pages), (layout.wtab as u64, pages)];
    if layout.cache != 0 { extents.push((layout.cache as u64, CACHE_ENTRIES as u64 * 16)); }
    let mut memory_end = 0;
    for &(start, size) in &extents {
        let end = start + size;
        if end > 1u64 << 32 { return Err("counted-loop layout exceeds wasm32".into()); }
        memory_end = memory_end.max(end);
    }
    for &(start, size) in &extents[1..] {
        if start < extents[0].0 + extents[0].1 && extents[0].0 < start + size { return Err("counted-loop metadata overlaps guest".into()); }
    }
    let mut module = ModuleBuilder::new();
    module.import_memory("env", "memory", memory_end.div_ceil(65536) as u32);
    if layout.cache != 0 { module.import_table("env", "table", 0); }
    let signature = module.ty(&[I32, I32], &[I32]);
    let mut c = Code::new();
    cpu(&mut c, layout.cpu_eip); c.u32c(entry).op(op::I32_NE).lget(1).op(op::I32_OR); reject(&mut c);
    cpu(&mut c, layout.cpu_r + ESP as u32 * 4);
    c.ltee(SP).u32c(verified.stack_low.unsigned_abs()).op(op::I32_SUB).ltee(LOW).u32c(guest_size - stack_size).op(op::I32_GT_U); reject(&mut c);
    c.lget(LOW).u32c(core_end).op(op::I32_LT_U); reject(&mut c);
    page_check(&mut c, layout, LOW, stack_size, true);
    c.u32c((core + 32) >> 12).mem(op::I32_LOAD8_U, layout.rtab)
        .u32c((core + 32) >> 12).mem(op::I32_LOAD8_U, layout.wtab).op(op::I32_AND).u32c(1).op(op::I32_AND).op(op::I32_EQZ); reject(&mut c);
    c.lget(SP).u32c(verified.frame_offset.unsigned_abs()).op(op::I32_SUB).u32c(core + 32).mem(op::I32_LOAD, layout.guest_base).op(op::I32_LT_U); reject(&mut c);
    c.u32c(core + 40).mem(op::I32_LOAD, layout.guest_base); reject(&mut c);
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base + 8).u32c(verified.argc).op(op::I32_NE); reject(&mut c);
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base + 12).ltee(ARGV).u32c(guest_size - argv_size).op(op::I32_GT_U); reject(&mut c);
    // Reject aliases with *all* native temporary stack storage and cdecl inputs,
    // not only the written frame. This also keeps argv away from core above.
    c.lget(ARGV).lget(SP).u32c(16).op(op::I32_ADD).op(op::I32_LT_U); reject(&mut c);
    page_check(&mut c, layout, ARGV, argv_size, false);
    c.lget(ARGV).mem(op::I32_LOAD, layout.guest_base + 4).ltee(N).u32c(MAX_TRIPS).op(op::I32_GT_U); reject(&mut c);
    c.lget(N).u32c(verified.per_trip).op(op::I32_MUL).u32c(verified.base_cost).op(op::I32_ADD).lset(COST);
    cpu(&mut c, layout.cpu_budget); c.ltee(BUDGET).lget(COST).op(op::I32_LT_S); reject(&mut c);
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base).lset(RET);

    let sum = verified.argc == 1;
    emit_records(&mut c, verified, layout, 20..if sum {29} else {30})?;
    c.block(0x40).loop_(0x40);
    // The initial jump goes to this condition, so zero trips remain zero trips.
    let condition = if sum {42..46} else {52..56};
    let condition_value = condition.end - 1;
    emit_records(&mut c, verified, layout, condition)?;
    c.lget(VALUE_BASE + condition_value as u32).op(op::I32_EQZ).br_if(1);
    emit_records(&mut c, verified, layout, if sum {33..41} else {34..51})?;
    c.br(0).end().end();
    let result = if sum {47} else {57};
    emit_records(&mut c, verified, layout, result..result + 1)?;
    // Atomic commit. ABI permits caller-volatile regs/flags to differ; cdecl
    // callee saves and all FP/SIMD state remain untouched. No guest writes.
    c.lget(0).lget(VALUE_BASE + result as u32).mem(op::I32_STORE, layout.cpu_r + EAX as u32 * 4);
    c.lget(0).lget(SP).u32c(4).op(op::I32_ADD).mem(op::I32_STORE, layout.cpu_r + ESP as u32 * 4);
    c.lget(0).lget(RET).mem(op::I32_STORE, layout.cpu_eip);
    c.lget(0).lget(BUDGET).lget(COST).op(op::I32_SUB).ltee(BUDGET).mem(op::I32_STORE, layout.cpu_budget);
    let completed = core::mem::offset_of!(Cpu, jit_lir_completed) as u32;
    c.lget(0).lget(0).mem(op::I64_LOAD, completed).i64c(1).op(op::I64_ADD).mem(op::I64_STORE, completed);
    if layout.cache != 0 {
        c.lget(BUDGET).i32c(0).op(op::I32_GT_S).if_(0x40);
        c.lget(RET).lget(RET).u32c(12).op(op::I32_SHR_U).op(op::I32_XOR).u32c(CACHE_ENTRIES - 1).op(op::I32_AND).u32c(4).op(op::I32_SHL).lset(LOW);
        c.lget(LOW).mem(op::I32_LOAD, layout.cache).lget(RET).op(op::I32_EQ)
            .lget(LOW).mem(op::I32_LOAD, layout.cache + 4).ltee(ARGV).i32c(0).op(op::I32_NE).op(op::I32_AND).if_(0x40);
        c.lget(0).lget(LOW).mem(op::I32_LOAD, layout.cache + 8).lget(ARGV).i32c(1).op(op::I32_SUB).return_call_indirect(signature, 0);
        c.end().end();
    }
    c.u32c(S_NEXT);
    let function = module.add_func(signature, VALUE_BASE + verified.records.len() as u32 - 2, 0, 0, c.b);
    module.name_func(function, "captured_counted_lir"); module.export_func("r0", function);
    Ok((module.finish(), RegionOut { heads: vec![(entry, 0)], n_ins: verified.base_cost, chains: vec![(entry, verified.base_cost)], ..RegionOut::default() }))
}

pub fn install(mem: &mut crate::mem::Memory, jit: &mut crate::jit::JitState, entry: u32, core: u32, native: &[u8], metadata: &[u8]) -> Result<usize, String> {
    let verified = verify(native, entry, core, metadata)?;
    let layout = jit.lir_layout(mem);
    let (wasm, region) = compile(&verified, &layout, entry, core, u32::try_from(mem.size()).map_err(|_| "counted-loop guest too large")?)?;
    let guards = vec![(entry, native[..verified.len].to_vec())];
    let n = jit.install_precompiled(mem, &layout, &wasm, &[(region, guards)])?;
    if n != 0 { jit.mark_lir_entry(entry); }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

    const CPU: usize = 0x1000;
    const RTAB: u32 = 0x10000;
    const WTAB: u32 = RTAB + (1 << 20);
    const GUEST: u32 = WTAB + (1 << 20);
    const GUEST_SIZE: u32 = 0x10000;
    const CORE: u32 = 0x3020;
    const ENTRY: u32 = 0x100;
    const SP0: u32 = 0x8080;
    const ARGV0: u32 = 0x9000;

    // Rebased fixtures reproduce the observed complete profiles. Addresses and
    // constant-blinding fields differ intentionally; no source loop is installed
    // or executed as the purported LIR integration.
    fn metadata(sum: bool, core: u32) -> Vec<u8> {
        let template = if sum { SUM_LIR } else { MIX_LIR };
        let mut bytes = (template.len() as u32).to_le_bytes().to_vec();
        for (i, text) in template.iter().enumerate() {
            let words: Vec<u32> = text.split_whitespace().map(|token| {
                if let Some(index) = token.strip_prefix('@') { 0x100000 + index.parse::<u32>().unwrap() * 32 }
                else if token == "c" { core }
                else if token == "x" { 0x200000 }
                else { u32::from_str_radix(token, 16).unwrap() }
            }).collect();
            bytes.extend_from_slice(&(0x100000 + i as u32 * 32).to_le_bytes());
            bytes.extend_from_slice(&(words.len() as u32 * 4).to_le_bytes());
            for word in words { bytes.extend_from_slice(&word.to_le_bytes()); }
        }
        bytes
    }
    fn native(sum: bool, core: u32, blind: u32) -> Vec<u8> {
        let mut bytes = unhex(if sum { SUM_NATIVE } else { MIX_NATIVE });
        for (at, delta) in [(28, 0), (37, 48), (49, 48), (55, 32), (if sum {125} else {115}, 40), (if sum {195} else {204}, 48)] { put(&mut bytes, at, core + delta); }
        if sum {
            for (at, value) in [(0x56, blind), (0x5c, blind.wrapping_sub(48)), (0x68, blind), (0x6e, blind.wrapping_sub(40))] { put(&mut bytes, at, value); }
        } else { put(&mut bytes, 0x5e, blind); put(&mut bytes, 0x64, blind.wrapping_sub(48)); }
        bytes
    }
    fn proof(sum: bool) -> Verified { verify(&native(sum, CORE, 0xf1780abc), ENTRY, CORE, &metadata(sum, CORE)).unwrap() }

    #[test]
    fn exact_untaken_overflow_padding_keeps_branch_and_bind_span_consistent() {
        for sum in [true, false] {
            let original = native(sum, CORE, 0xf1780abc);
            let mut padded = original.clone(); padded.splice(0x3f..0x3f, [0x8d,0x24,0x24]); padded[0x3e] = 11;
            let proof = verify(&padded, ENTRY, CORE, &metadata(sum,CORE)).unwrap();
            assert_eq!(proof.len, original.len()+3);
            padded[0x3e] = 8;
            assert!(verify(&padded, ENTRY, CORE, &metadata(sum,CORE)).is_err());
        }
    }
    fn layout() -> Layout { Layout::for_cpu(GUEST, RTAB, WTAB, 0) }
    // wasmi's translator has a large fixed stack frame; this also makes the
    // tests work without a process-wide RUST_MIN_STACK environment override.
    fn large_stack(f: impl FnOnce() + Send + 'static) { std::thread::Builder::new().stack_size(16 << 20).spawn(f).unwrap().join().unwrap(); }

    #[test]
    fn complete_raw_templates_and_native_effects_fail_closed() {
        for sum in [true, false] {
            let meta = metadata(sum, CORE);
            let code = native(sum, CORE, 0xf1780abc);
            assert_eq!(verify(&code, ENTRY, CORE, &meta).unwrap().len, code.len());
            for len in 0..code.len() { assert!(verify(&code[..len], ENTRY, CORE, &meta).is_err(), "accepted native truncation {len}"); }
            for len in 0..meta.len() { assert!(verify(&code, ENTRY, CORE, &meta[..len]).is_err(), "accepted raw truncation {len}"); }
            for at in 0..code.len() {
                if (0x43..0x47).contains(&at) { continue; } // proven unreachable helper displacement
                let mut changed = code.clone(); changed[at] ^= 1;
                assert!(verify(&changed, ENTRY, CORE, &meta).is_err(), "accepted mutated native byte {at:#x}");
            }
            // Every actual instruction word, including discarded argv[0] load,
            // poll, saved-register metadata and the return, is checked. Opaque
            // unreachable-call metadata pointers are allowed to relocate only.
            let rs = records(&meta).unwrap();
            let mut at = 4;
            for (i, r) in rs.iter().enumerate() {
                let template = if sum { SUM_LIR } else { MIX_LIR };
                for (n, token) in template[i].split_whitespace().enumerate() {
                    if token == "x" { continue; }
                    let mut changed = meta.clone(); changed[at + 8 + n * 4] ^= 1;
                    assert!(verify(&code, ENTRY, CORE, &changed).is_err(), "accepted changed raw word {i}:{n}");
                }
                at += 8 + r.words.len() * 4;
            }
            let mut trailing = meta.clone(); trailing.extend([0; 4]);
            assert!(verify(&code, ENTRY, CORE, &trailing).is_err());
            // Hostile record envelopes cannot turn slices, overlapping record
            // storage or duplicate identities into a shorter accepted stream.
            for (at, value) in [(0, 62), (4, 0), (4, u32::MAX - 3), (8, u32::MAX),
                (8, 0), (8, 20), (16, 0x100000), (16, 0x100004), (16, 0x100021)] {
                let mut changed = meta.clone(); put(&mut changed, at, value);
                assert!(verify(&code, ENTRY, CORE, &changed).is_err(), "accepted hostile envelope {at}:{value:#x}");
            }
            assert!(verify(&code, u32::MAX, CORE, &meta).is_err());
            assert!(verify(&code, ENTRY, u32::MAX - 32, &meta).is_err());
            // All bit patterns for blinding are supported, only paired arithmetic.
            for blind in [0, 1, 31, 48, 0x80000000, u32::MAX] { assert!(verify(&native(sum, CORE, blind), ENTRY, CORE, &meta).is_ok()); }
            if !sum {
                let mut unpadded = code.clone(); unpadded.drain(0x58..0x5a);
                assert_eq!(verify(&unpadded, ENTRY, CORE, &meta).unwrap().len, code.len() - 2);
                for at in 0..unpadded.len() {
                    if (0x43..0x47).contains(&at) { continue; }
                    let mut changed = unpadded.clone(); changed[at] ^= 1;
                    assert!(verify(&changed, ENTRY, CORE, &meta).is_err(), "accepted mutated unpadded byte {at:#x}");
                }
            }
        }
    }

    struct Rig { store: Store<()>, memory: Memory, function: TypedFunc<(i32, i32), i32> }
    impl Rig {
        fn new(sum: bool, n: u32, seed: u32) -> Self {
            let (wasm, out) = compile(&proof(sum), &layout(), ENTRY, CORE, GUEST_SIZE).unwrap();
            assert_eq!(out.heads, vec![(ENTRY, 0)]);
            assert_eq!(out.n_fallback, 0); assert_eq!(out.n_intr, 0);
            let engine = Engine::default();
            let module = Module::new(&engine, &wasm[..]).expect("counted-loop wasm validates");
            let mut store = Store::new(&engine, ());
            let memory = Memory::new(&mut store, MemoryType::new((GUEST + GUEST_SIZE).div_ceil(65536), None)).unwrap();
            let mut linker = Linker::new(&engine); linker.define("env", "memory", memory).unwrap();
            let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
            assert!(instance.get_func(&store, "trace").is_none());
            let function = instance.get_typed_func(&store, "r0").unwrap();
            let mut rig = Self { store, memory, function };
            for page in 0..GUEST_SIZE / 4096 { rig.permission(RTAB, page, 1); rig.permission(WTAB, page, 1); }
            for (a, v) in [(SP0, 0x500), (SP0 + 4, 0x1234), (SP0 + 8, if sum {1} else {2}), (SP0 + 12, ARGV0),
                (ARGV0, 0x77777777), (ARGV0 + 4, n), (ARGV0 + 8, seed), (CORE + 32, SP0 - 4096), (CORE + 40, 0), (CORE + 48, 0x12345678)] { rig.guest(a, v); }
            let mut cpu = Cpu::new(); cpu.eip = ENTRY; cpu.r = [0xabababab, 2, 3, 4, SP0, 6, 7, 8]; cpu.jit_budget = 200000;
            cpu.jit_lir_completed = u32::MAX as u64; cpu.lf_kind = 3; cpu.lf_a = 47; cpu.lf_b = 3; cpu.lf_r = 44;
            unsafe { core::ptr::copy_nonoverlapping(&cpu as *const Cpu as *const u8, rig.memory.data_mut(&mut rig.store).as_mut_ptr().add(CPU), core::mem::size_of::<Cpu>()); }
            rig
        }
        fn word(&mut self, address: usize, value: u32) { self.memory.data_mut(&mut self.store)[address..address + 4].copy_from_slice(&value.to_le_bytes()); }
        fn guest(&mut self, a: u32, v: u32) { self.word((GUEST + a) as usize, v); }
        fn cpu(&mut self, a: u32, v: u32) { self.word(CPU + a as usize, v); }
        fn permission(&mut self, table: u32, page: u32, v: u8) { self.memory.data_mut(&mut self.store)[(table + page) as usize] = v; }
        fn cpu_image(&self) -> Vec<u8> { self.memory.data(&self.store)[CPU..CPU + core::mem::size_of::<Cpu>()].to_vec() }
        fn guest_image(&self) -> Vec<u8> { self.memory.data(&self.store)[GUEST as usize..(GUEST + GUEST_SIZE) as usize].to_vec() }
        fn call(&mut self, head: i32) -> u32 { self.function.call(&mut self.store, (CPU as i32, head)).expect("guest faults must not trap wasm") as u32 }
    }

    #[test]
    fn lir_loop_recurrences_match_sources_and_preserve_abi_fp_and_heap() {
        large_stack(|| {
        for sum in [true, false] {
            let verified = proof(sum);
            let mut rig = Rig::new(sum, 0, 0);
            let mut rng = 0x9e3779b9u32;
            for n in [0, 1, 2, 3, 17, 127, 255, 999, 4095, 4096] {
                for _ in 0..12 {
                    rng ^= rng << 13; rng ^= rng >> 17; rng ^= rng << 5;
                    let mut expected = if sum {0} else {rng};
                    for i in 0..n { if sum { expected += i; } else { expected ^= expected << 13; expected ^= expected >> 17; expected ^= expected << 5; } }
                    rig.cpu(layout().cpu_eip, ENTRY); rig.cpu(layout().cpu_r + ESP as u32 * 4, SP0); rig.cpu(layout().cpu_budget, 200000);
                    rig.guest(ARGV0 + 4, n); rig.guest(ARGV0 + 8, rng);
                    let mut image = rig.cpu_image(); let memory = rig.guest_image();
                    put(&mut image, layout().cpu_r as usize, expected);
                    put(&mut image, (layout().cpu_r + ESP as u32 * 4) as usize, SP0 + 4);
                    put(&mut image, layout().cpu_eip as usize, 0x500);
                    put(&mut image, layout().cpu_budget as usize, 200000 - verified.base_cost - verified.per_trip * n);
                    let counter = core::mem::offset_of!(Cpu, jit_lir_completed);
                    let completed = u64::from_le_bytes(image[counter..counter + 8].try_into().unwrap()).wrapping_add(1);
                    image[counter..counter + 8].copy_from_slice(&completed.to_le_bytes());
                    assert_eq!(rig.call(0), S_NEXT, "sum={sum} n={n}");
                    assert_eq!(rig.cpu_image(), image, "sum={sum} n={n}");
                    assert_eq!(rig.guest_image(), memory);
                }
            }
        }
        });
    }

    #[test]
    fn counted_loop_guard_refusals_leave_cpu_and_guest_byte_identical() {
        large_stack(|| {
        for sum in [true, false] {
            for case in 0..32 {
                let mut rig = Rig::new(sum, 5, 12345);
                match case {
                    0 => rig.cpu(layout().cpu_eip, ENTRY + 1),
                    1 => {}, // invalid head
                    2 => rig.cpu(layout().cpu_budget, proof(sum).base_cost + 5 * proof(sum).per_trip - 1),
                    3 => rig.cpu(layout().cpu_budget, u32::MAX),
                    4 => rig.cpu(layout().cpu_r + ESP as u32 * 4, 20),
                    5 => rig.cpu(layout().cpu_r + ESP as u32 * 4, u32::MAX - 3),
                    6 => rig.cpu(layout().cpu_r + ESP as u32 * 4, 0xfff4),
                    7 => rig.cpu(layout().cpu_r + ESP as u32 * 4, 0x8000),
                    8 => rig.permission(RTAB, SP0 >> 12, 0),
                    9 => rig.permission(WTAB, SP0 >> 12, 0),
                    10 => rig.permission(RTAB, (CORE + 32) >> 12, 0),
                    11 => rig.permission(WTAB, (CORE + 32) >> 12, 0),
                    12 => rig.guest(CORE + 32, SP0),
                    13 => rig.guest(CORE + 40, 1),
                    14 => rig.guest(CORE + 40, 0x80000000),
                    15 => rig.guest(SP0 + 8, 3),
                    16 => rig.guest(SP0 + 12, SP0 - 16),
                    17 => rig.guest(SP0 + 12, SP0),
                    18 => rig.guest(SP0 + 12, SP0 + 4),
                    19 => rig.guest(SP0 + 12, SP0 + 12),
                    20 => rig.guest(SP0 + 12, u32::MAX - 3),
                    21 => rig.guest(SP0 + 12, 0xfffc),
                    22 => rig.guest(SP0 + 12, 0x9ffc),
                    23 => rig.permission(RTAB, ARGV0 >> 12, 0),
                    24 => rig.cpu(layout().cpu_r + ESP as u32 * 4, CORE + 60),
                    25 => rig.guest(ARGV0 + 4, 4097),
                    26 => rig.guest(ARGV0 + 4, u32::MAX),
                    27 => rig.guest(ARGV0 + 4, 0x80000000),
                    28 => rig.permission(RTAB, ARGV0 >> 12, 4),
                    29 => rig.permission(WTAB, SP0 >> 12, 2),
                    30 => rig.guest(CORE + 32, u32::MAX),
                    _ => rig.guest(SP0 + 12, CORE + 48),
                }
                let cpu = rig.cpu_image(); let guest = rig.guest_image();
                assert_eq!(rig.call(if case == 1 {1} else {0}), S_RETRY, "sum={sum} case={case}");
                assert_eq!(rig.cpu_image(), cpu, "sum={sum} case={case}");
                assert_eq!(rig.guest_image(), guest, "sum={sum} case={case}");
            }
        }
        });
    }

    #[test]
    fn counted_loop_exact_budget_and_readonly_argv_succeed() {
        large_stack(|| {
        for sum in [true, false] {
            let verified = proof(sum);
            let mut rig = Rig::new(sum, 4096, 12345);
            rig.cpu(layout().cpu_budget, verified.base_cost + 4096 * verified.per_trip);
            rig.guest(CORE + 32, SP0 - verified.frame_offset.unsigned_abs());
            rig.permission(WTAB, ARGV0 >> 12, 0);
            assert_eq!(rig.call(0), S_NEXT);
            let image = rig.cpu_image();
            assert_eq!(word(&image, layout().cpu_budget as usize).unwrap(), 0);
        }
        });
    }

    // Minimal no-helper native test backend. Only the verified r0 is compiled;
    // threshold=MAX keeps S_RETRY fallback native blocks in the interpreter.
    struct DispatchBackend {
        engine: Engine, store: Store<()>, memory: Memory,
        functions: Vec<Option<TypedFunc<(i32, i32), i32>>>,
    }
    impl DispatchBackend {
        fn new() -> Self {
            let engine = Engine::default(); let mut store = Store::new(&engine, ());
            let memory = Memory::new(&mut store, MemoryType::new((GUEST + GUEST_SIZE).div_ceil(65536), None)).unwrap();
            Self { engine, store, memory, functions: Vec::new() }
        }
    }
    impl crate::jit::rt::Backend for DispatchBackend {
        fn install(&mut self, bytes: &[u8], n: usize) -> Option<Vec<u32>> {
            assert_eq!(n, 1);
            let module = Module::new(&self.engine, bytes).unwrap(); let mut linker = Linker::new(&self.engine);
            linker.define("env", "memory", self.memory).unwrap();
            let instance = linker.instantiate_and_start(&mut self.store, &module).unwrap();
            let function = instance.get_typed_func(&self.store, "r0").unwrap();
            let slot = self.functions.len() as u32; self.functions.push(Some(function)); Some(vec![slot])
        }
        fn free(&mut self, slot: u32) { self.functions[slot as usize] = None; }
        fn call(&mut self, slot: u32, cpu: &mut Cpu, head: u32) -> u32 {
            let (mem, _) = unsafe { crate::jit::ctx::get() }; let mem = unsafe { &mut *mem };
            let bytes = self.memory.data_mut(&mut self.store);
            unsafe { core::ptr::copy_nonoverlapping(cpu as *const Cpu as *const u8, bytes.as_mut_ptr().add(CPU), core::mem::size_of::<Cpu>()); }
            bytes[GUEST as usize..(GUEST + GUEST_SIZE) as usize].copy_from_slice(mem.raw());
            let (rt, wt) = mem.jit_tables(); let pages = GUEST_SIZE as usize / 4096;
            bytes[RTAB as usize..RTAB as usize + pages].copy_from_slice(&rt[..pages]);
            bytes[WTAB as usize..WTAB as usize + pages].copy_from_slice(&wt[..pages]);
            let status = self.functions[slot as usize].unwrap().call(&mut self.store, (CPU as i32, head as i32)).unwrap();
            let bytes = self.memory.data(&self.store);
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr().add(CPU), cpu as *mut Cpu as *mut u8, core::mem::size_of::<Cpu>()); }
            mem.raw_mut().copy_from_slice(&bytes[GUEST as usize..(GUEST + GUEST_SIZE) as usize]); status as u32
        }
    }
    fn dispatch_fixture(sum: bool, n: u32, seed: u32) -> (crate::mem::Memory, crate::run::BlockCache, Cpu) {
        use crate::mem::{P_MAPPED, P_R, P_W, P_X};
        crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = Some(Box::new(DispatchBackend::new())));
        let mut mem = crate::mem::Memory::new(GUEST_SIZE as usize);
        mem.set_prot(0, GUEST_SIZE, P_MAPPED | P_R | P_W);
        mem.set_prot(0, 4096, P_MAPPED | P_R | P_W | P_X);
        let code = native(sum, CORE, 0xf1780abc); mem.write_bytes(ENTRY, &code).unwrap();
        // A native-only side-exit marker; zero-poll r0 must never reach it.
        mem.write_bytes(ENTRY + code.len() as u32, &[0xcc]).unwrap();
        mem.write_bytes(0x500, &[0xcd, 0x82]).unwrap();
        for (a, v) in [(SP0, 0x500), (SP0 + 4, 0x1234), (SP0 + 8, if sum {1} else {2}), (SP0 + 12, ARGV0),
            (ARGV0, 0x77777777), (ARGV0 + 4, n), (ARGV0 + 8, seed), (CORE + 32, SP0 - 4096), (CORE + 40, 0), (CORE + 48, 0x12345678)] { mem.w32(a, v).unwrap(); }
        let mut cache = crate::run::BlockCache::new(); cache.jit.threshold = u32::MAX;
        cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
        install(&mut mem, &mut cache.jit, ENTRY, CORE, &code, &metadata(sum, CORE)).unwrap();
        let mut cpu = Cpu::new(); cpu.eip = ENTRY; cpu.r = [1, 2, 3, 4, SP0, 6, 7, 8];
        (mem, cache, cpu)
    }

    #[test]
    fn real_dispatcher_counted_loops_native_fallback_slices_and_smc() {
        large_stack(|| {
            use crate::cpu::{Exit, EBX, EBP, ESI, EDI};
            for sum in [true, false] {
                for n in [0, 1, 17, 199, 4096] {
                    let (mut mem, mut cache, mut cpu) = dispatch_fixture(sum, n, 12345);
                    let mut oracle = crate::mem::Memory::new(GUEST_SIZE as usize);
                    oracle.set_prot(0, GUEST_SIZE, crate::mem::P_MAPPED | crate::mem::P_R | crate::mem::P_W | crate::mem::P_X);
                    oracle.raw_mut().copy_from_slice(mem.raw()); let mut expected = cpu.clone();
                    assert_eq!(expected.run(&mut oracle, &mut crate::run::BlockCache::new(), 200000), Exit::Hyper);
                    assert_eq!(cpu.run(&mut mem, &mut cache, 200000), Exit::Hyper);
                    for register in [EAX, ESP, EBX, EBP, ESI, EDI] { assert_eq!(cpu.r[register], expected.r[register], "sum={sum} n={n}"); }
                    assert_eq!(mem.r32(CORE + 48).unwrap(), 0x12345678);
                    assert_eq!(cpu.jit_lir_completed, 1); assert!(cache.jit.stats.lir_dispatches > 0);
                }
                // No full-call budget: S_RETRY must allow native progress, not
                // pin execution at entry forever or falsely count completion.
                for n in [17, 4097, u32::MAX] {
                    let (mut mem, mut cache, mut cpu) = dispatch_fixture(sum, n, 12345);
                    let start = cpu.r[ESP];
                    assert_eq!(cpu.run(&mut mem, &mut cache, 1), Exit::Budget);
                    assert_ne!(cpu.eip, ENTRY); assert_eq!(cpu.jit_lir_completed, 0);
                    let mut finished = false;
                    for _ in 0..20000 {
                        match cpu.run(&mut mem, &mut cache, 7) { Exit::Budget => {}, Exit::Hyper => { finished = true; break; }, other => panic!("native fallback: {other:?}") }
                    }
                    assert!(finished, "native fallback stalled sum={sum} n={n}");
                    assert_eq!(cpu.r[ESP], start + 4); assert_eq!(cpu.jit_lir_completed, 0); assert!(cache.jit.stats.retries > 0);
                }
                // Pending safepoint is never elided: native reaches its side exit.
                let (mut mem, mut cache, mut cpu) = dispatch_fixture(sum, 1, 12345);
                mem.w32(CORE + 40, 1).unwrap();
                let exit = cpu.run(&mut mem, &mut cache, 200000);
                assert_eq!(exit, Exit::Breakpoint, "active poll must execute native-only side exit");
                assert_eq!(cpu.jit_lir_completed, 0); assert!(cache.jit.stats.retries > 0);
                // Code writes retire the verified entry before any replacement
                // can run, even when the modified byte is a discarded read.
                let (mut mem, mut cache, mut cpu) = dispatch_fixture(sum, 1, 12345);
                mem.write_bytes(ENTRY + 0x4f, &[0xcc]).unwrap(); cache.sync(&mut mem);
                assert!(!cache.jit.is_lir_entry(ENTRY));
                assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Breakpoint); assert_eq!(cpu.jit_lir_completed, 0);
                crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = None);
            }
        });
    }

    /// Optional *read-only* verification of actual assemble/setJit evidence.
    /// Bundle format produced with encodeLoopCapture: for each of two records,
    /// entry/core/nativeLen/metadataLen (u32 LE), native bytes, metadata bytes.
    /// No captured address is accessed or executed by this offline test.
    #[test]
    fn supplied_runtime_capture_evidence_verifies_readonly() {
        let Some(path) = std::env::var_os("NANOJIT_LOOP_EVIDENCE") else { return; };
        let bundle = std::fs::read(path).unwrap();
        let mut at = 0;
        for argc in [1, 2] {
            let entry = word(&bundle, at).unwrap(); let core = word(&bundle, at + 4).unwrap();
            let native_len = word(&bundle, at + 8).unwrap() as usize;
            let metadata_len = word(&bundle, at + 12).unwrap() as usize;
            at += 16;
            let native = bundle.get(at..at + native_len).unwrap(); at += native_len;
            let metadata = bundle.get(at..at + metadata_len).unwrap(); at += metadata_len;
            let verified = verify(native, entry, core, metadata).unwrap();
            assert_eq!(verified.argc, argc);
        }
        assert_eq!(at, bundle.len());
    }
}
