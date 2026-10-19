//! Transactional cdecl adapter for verified, no-call integer object leafs.
//!
//! This emits guards, not a native-code proof. The installer must prove the full
//! native effects/result, every accessed field (including dead reads), restored
//! frame/callee saves and stack-limit check, and bind all code dependencies via
//! ordinary SMC invalidation. `layout` and the Cpu allocation are runtime-owned.

use super::{error, Error, Instruction, Trace, Type};
use crate::cpu::{Cpu, EAX, ESP};
use crate::jit_x86::{Layout, RegionOut, CACHE_ENTRIES, S_NEXT, S_RETRY};
use crate::wasm_emit::{op, Code, ModuleBuilder, I32};

/// High bit in a binding selects a raw receiver-relative i32 field. Otherwise
/// the binding is a byte offset into argv, including zero (the raw receiver).
pub const RECEIVER_FIELD: u32 = 0x8000_0000;

#[derive(Clone, Debug)]
pub struct Abi {
    pub entry: u32,
    pub core: u32,
    pub guest_size: u32,
    pub argc: u32,
    pub bindings: Vec<u32>,
    /// All native object accesses, not only fields feeding the returned value.
    pub fields: Vec<u32>,
    /// At most one native heap write, proved to store exactly the return value.
    pub store: Option<u32>,
    /// Offsets from entry ESP, as established by the native verifier.
    pub stack_low: i32,
    pub frame_offset: i32,
    pub instruction_cost: u32,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub wasm: Vec<u8>,
    pub region: RegionOut,
}

// r0(cpu, head); all adapter/semantic locals are native wasm i32 values.
const RESULT: u32 = 2;
const SP: u32 = 3;
const ARGV: u32 = 4;
const RET: u32 = 5;
const BUDGET: u32 = 6;
const LOW: u32 = 7;
const PAGE: u32 = 8;
const RECEIVER: u32 = 9;
const OBJECT_LOW: u32 = 10;
const OBJECT_END: u32 = 11;
const ADDRESS: u32 = 12;
const EFFECT_BASE: u32 = 13;

fn reject(c: &mut Code, status: u32) {
    c.if_(0x40).u32c(status).op(op::RETURN).end();
}

fn cpu(c: &mut Code, offset: u32) {
    c.lget(0).mem(op::I32_LOAD, offset);
}

/// Guest bounds/nonwrapping arithmetic must already be established. Crossing a
/// page conservatively retries; only bit zero authorizes the current page.
fn page_check(c: &mut Code, layout: &Layout, address: u32, size: u32, write: bool) {
    c.lget(address)
        .u32c(4095)
        .op(op::I32_AND)
        .u32c(4096 - size)
        .op(op::I32_GT_U);
    reject(c, S_RETRY);
    c.lget(address).u32c(12).op(op::I32_SHR_U).ltee(PAGE);
    c.mem(op::I32_LOAD8_U, layout.rtab);
    if write {
        c.lget(PAGE)
            .mem(op::I32_LOAD8_U, layout.wtab)
            .op(op::I32_AND);
    }
    c.u32c(1).op(op::I32_AND).op(op::I32_EQZ);
    reject(c, S_RETRY);
}

/// Emit an integrated `r0(cpu, head) -> status` region, with private pure i32
/// semantics. All checks finish before any object field is loaded. Refusal,
/// including insufficient budget, is S_RETRY with *all* Cpu/guest bytes intact.
///
/// Success writes only the optional object field, EAX/ESP/EIP, budget, and the
/// completed-call counter. No transient native stack/MethodFrame writes, lazy
/// flag changes, GC/safepoints, runtime calls, or SMC bookkeeping are needed:
/// writable preflights reject decoded-code pages via wtab bit zero. With a live
/// cache, commit precedes the usual indirect tail return to the compiled caller.
pub fn compile(trace: &Trace, layout: &Layout, abi: &Abi) -> Result<Output, Error> {
    let result = trace.instructions.iter().find_map(|ins| if let Instruction::Return(id)=ins {Some(*id)}else{None}).ok_or_else(||error(0,"memory trace has no return"))?;
    let stores: Vec<_> = abi.store.map(|offset|(offset,result)).into_iter().collect();
    compile_effects(trace,layout,abi,&stores)
}

/// All values are evaluated against preflighted inputs, then the proven sequence
/// of writes is committed without a refusing operation between its stores.
pub fn compile_effects(trace: &Trace, layout: &Layout, abi: &Abi, stores: &[(u32,usize)]) -> Result<Output, Error> {
    let bad = |message| error(0, message);
    if stores.len()>8 || stores.iter().any(|&(offset,id)|!abi.fields.contains(&offset) || id>=trace.values.len()) {return Err(bad("invalid memory effect outputs"));}
    if trace.result() != Type::I32
        || trace.params().len() != abi.bindings.len()
        || trace.params().iter().any(|ty| *ty != Type::I32)
        || trace.values.iter().any(|value| value.ty != Type::I32)
        || trace
            .instructions
            .iter()
            .any(|ins| matches!(ins, Instruction::Guard { .. }))
    {
        return Err(bad("memory leaf requires a guard-free, entirely i32 trace"));
    }
    let valid_field = |offset: u32| offset <= 4092 && offset % 4 == 0;
    if abi.argc > 16
        || abi.bindings.len() > 16
        || abi.fields.len() > 16
        || abi.fields.iter().any(|&offset| !valid_field(offset))
        || abi.bindings.iter().any(|&binding| {
            if binding & RECEIVER_FIELD != 0 {
                let offset = binding & !RECEIVER_FIELD;
                !valid_field(offset) || !abi.fields.contains(&offset)
            } else {
                binding % 4 != 0 || binding > abi.argc * 4
            }
        })
        || abi
            .store
            .is_some_and(|offset| !valid_field(offset) || !abi.fields.contains(&offset))
        || !(-4080..=-4).contains(&abi.stack_low)
        || !(abi.stack_low..=-4).contains(&abi.frame_offset)
        || abi.guest_size < 4096
        || abi.entry >= abi.guest_size
        || abi.instruction_cost == 0
        || abi.instruction_cost > i32::MAX as u32
    {
        return Err(bad("invalid verified memory-leaf ABI bounds"));
    }
    let core_end = abi
        .core
        .checked_add(52)
        .filter(|&end| end <= abi.guest_size)
        .ok_or_else(|| bad("core range lies outside guest memory"))?;
    let core_start = abi.core + 32;
    let native = Layout::for_cpu(layout.guest_base, layout.rtab, layout.wtab, layout.cache);
    if [layout.cpu_r, layout.cpu_eip, layout.cpu_budget]
        != [native.cpu_r, native.cpu_eip, native.cpu_budget]
    {
        return Err(bad("memory leaf requires this build's native Cpu layout"));
    }

    // Trusted allocations still need representable wasm32 extents. Requiring
    // them in the import makes linear-memory bounds an instantiation condition.
    let pages = (abi.guest_size as u64).div_ceil(4096);
    let mut extents = vec![
        (layout.guest_base as u64, abi.guest_size as u64),
        (layout.rtab as u64, pages),
        (layout.wtab as u64, pages),
    ];
    if layout.cache != 0 {
        extents.push((layout.cache as u64, CACHE_ENTRIES as u64 * 16));
    }
    let mut memory_end = 0;
    for &(start, size) in &extents {
        let end = start + size;
        if end > 1u64 << 32 {
            return Err(bad("trusted allocation exceeds wasm32 memory"));
        }
        memory_end = memory_end.max(end);
    }
    for allocation in &extents[1..] {
        let guest = extents[0];
        if allocation.0 < guest.0 + guest.1 && guest.0 < allocation.0 + allocation.1 {
            return Err(bad("trusted metadata allocation overlaps guest memory"));
        }
    }

    let stack_size = abi.stack_low.unsigned_abs() + 16;
    let argv_size = (abi.argc + 1) * 4;
    let mut module = ModuleBuilder::new();
    module.import_memory("env", "memory", memory_end.div_ceil(65536) as u32);
    if layout.cache != 0 {
        module.import_table("env", "table", 0);
    }
    let signature = module.ty(&[I32, I32], &[I32]);
    let semantic = trace.emit_integer_function_outputs(&mut module,&stores.iter().map(|&(_,id)|id).collect::<Vec<_>>())?;
    let mut c = Code::new();
    cpu(&mut c, layout.cpu_eip);
    c.u32c(abi.entry).op(op::I32_NE).lget(1).op(op::I32_OR);
    reject(&mut c, S_RETRY);
    cpu(&mut c, layout.cpu_budget);
    c.ltee(BUDGET).u32c(abi.instruction_cost).op(op::I32_LT_S);
    reject(&mut c, S_RETRY);
    cpu(&mut c, layout.cpu_r + ESP as u32 * 4);
    c.ltee(SP)
        .u32c(abi.stack_low.unsigned_abs())
        .op(op::I32_SUB)
        .ltee(LOW)
        .u32c(abi.guest_size - stack_size)
        .op(op::I32_GT_U);
    // The single unsigned bound rejects both low underflow and sp+16 overflow.
    reject(&mut c, S_RETRY);
    c.lget(LOW).u32c(core_end).op(op::I32_LT_U);
    reject(&mut c, S_RETRY);
    page_check(&mut c, layout, LOW, stack_size, true);

    // Match the general native stack contract, including the unlinked frame's
    // writes and every core field in [core+32, core+52), even for no-call leafs.
    c.u32c(core_start).lset(ADDRESS);
    page_check(&mut c, layout, ADDRESS, 20, true);
    c.lget(SP)
        .u32c(abi.frame_offset.unsigned_abs())
        .op(op::I32_SUB)
        .u32c(core_start)
        .mem(op::I32_LOAD, layout.guest_base)
        .op(op::I32_LT_U);
    reject(&mut c, S_RETRY);
    c.lget(SP)
        .mem(op::I32_LOAD, layout.guest_base + 8)
        .u32c(abi.argc)
        .op(op::I32_NE);
    reject(&mut c, S_RETRY);
    c.lget(SP)
        .mem(op::I32_LOAD, layout.guest_base + 12)
        .ltee(ARGV)
        .lget(SP)
        .u32c(4)
        .op(op::I32_ADD)
        .op(op::I32_LT_U);
    reject(&mut c, S_RETRY);
    c.lget(ARGV)
        .u32c(abi.guest_size - argv_size)
        .op(op::I32_GT_U);
    reject(&mut c, S_RETRY);
    page_check(&mut c, layout, ARGV, argv_size, false);
    c.lget(ARGV)
        .mem(op::I32_LOAD, layout.guest_base)
        .lset(RECEIVER);

    if let (Some(&min), Some(&max)) = (abi.fields.iter().min(), abi.fields.iter().max()) {
        // This proves every receiver+offset/end nonwrapping, including tagged or
        // otherwise unusual receiver words: no object/type assumption is made.
        let end = max + 4;
        c.lget(RECEIVER).u32c(abi.guest_size - end).op(op::I32_GT_U);
        reject(&mut c, S_RETRY);
        c.lget(RECEIVER).u32c(min).op(op::I32_ADD).lset(OBJECT_LOW);
        c.lget(RECEIVER).u32c(end).op(op::I32_ADD).lset(OBJECT_END);

        // Reject intersection of the ENTIRE accessed object interval with each
        // native transient-write/input span, not merely the final store address.
        c.lget(OBJECT_LOW)
            .lget(SP)
            .u32c(16)
            .op(op::I32_ADD)
            .op(op::I32_LT_U)
            .lget(LOW)
            .lget(OBJECT_END)
            .op(op::I32_LT_U)
            .op(op::I32_AND);
        reject(&mut c, S_RETRY);
        c.lget(OBJECT_LOW)
            .u32c(core_end)
            .op(op::I32_LT_U)
            .u32c(core_start)
            .lget(OBJECT_END)
            .op(op::I32_LT_U)
            .op(op::I32_AND);
        reject(&mut c, S_RETRY);
        c.lget(OBJECT_LOW)
            .lget(ARGV)
            .u32c(argv_size)
            .op(op::I32_ADD)
            .op(op::I32_LT_U)
            .lget(ARGV)
            .lget(OBJECT_END)
            .op(op::I32_LT_U)
            .op(op::I32_AND);
        reject(&mut c, S_RETRY);

        for &offset in &abi.fields {
            c.lget(RECEIVER).u32c(offset).op(op::I32_ADD).lset(ADDRESS);
            page_check(&mut c, layout, ADDRESS, 4, stores.iter().any(|&(field,_)|field==offset));
        }
    }

    // All potentially refusing operations precede this point; no object field
    // has been loaded yet. Semantic inputs use the original, unmodified memory.
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base).lset(RET);
    for &binding in &abi.bindings {
        if binding & RECEIVER_FIELD != 0 {
            c.lget(RECEIVER)
                .u32c(binding & !RECEIVER_FIELD)
                .op(op::I32_ADD)
                .mem(op::I32_LOAD, layout.guest_base);
        } else {
            c.lget(ARGV).mem(op::I32_LOAD, layout.guest_base + binding);
        }
    }
    c.call(semantic);
    for i in (0..stores.len()).rev() { c.lset(EFFECT_BASE + i as u32); }
    c.lset(RESULT);

    // Atomic commit. A writable object page cannot contain decoded native code,
    // so this accepted path must not invoke slow write/SMC helpers.
    for (i,&(offset,_)) in stores.iter().enumerate() {
        c.lget(RECEIVER)
            .u32c(offset)
            .op(op::I32_ADD)
            .lget(EFFECT_BASE + i as u32)
            .mem(op::I32_STORE, layout.guest_base);
    }
    c.lget(0)
        .lget(RESULT)
        .mem(op::I32_STORE, layout.cpu_r + EAX as u32 * 4);
    c.lget(0)
        .lget(SP)
        .u32c(4)
        .op(op::I32_ADD)
        .mem(op::I32_STORE, layout.cpu_r + ESP as u32 * 4);
    c.lget(0).lget(RET).mem(op::I32_STORE, layout.cpu_eip);
    c.lget(0)
        .lget(BUDGET)
        .u32c(abi.instruction_cost)
        .op(op::I32_SUB)
        .ltee(BUDGET)
        .mem(op::I32_STORE, layout.cpu_budget);
    let completed = core::mem::offset_of!(Cpu, jit_lir_completed) as u32;
    c.lget(0)
        .lget(0)
        .mem(op::I64_LOAD, completed)
        .i64c(1)
        .op(op::I64_ADD)
        .mem(op::I64_STORE, completed);
    if layout.cache != 0 {
        c.lget(BUDGET).i32c(0).op(op::I32_GT_S).if_(0x40);
        c.lget(RET)
            .lget(RET)
            .u32c(12)
            .op(op::I32_SHR_U)
            .op(op::I32_XOR)
            .u32c(CACHE_ENTRIES - 1)
            .op(op::I32_AND)
            .u32c(4)
            .op(op::I32_SHL)
            .lset(LOW);
        c.lget(LOW)
            .mem(op::I32_LOAD, layout.cache)
            .lget(RET)
            .op(op::I32_EQ)
            .lget(LOW)
            .mem(op::I32_LOAD, layout.cache + 4)
            .ltee(ARGV)
            .i32c(0)
            .op(op::I32_NE)
            .op(op::I32_AND)
            .if_(0x40);
        c.lget(0)
            .lget(LOW)
            .mem(op::I32_LOAD, layout.cache + 8)
            .lget(ARGV)
            .i32c(1)
            .op(op::I32_SUB)
            .return_call_indirect(signature, 0);
        c.end().end();
    }
    c.u32c(S_NEXT);
    let r0 = module.add_func(signature, EFFECT_BASE - 2 + stores.len() as u32, 0, 0, c.b);
    module.name_func(r0, "captured_integer_memory_leaf");
    module.export_func("r0", r0);
    Ok(Output {
        wasm: module.finish(),
        region: RegionOut {
            heads: vec![(abi.entry, 0)],
            n_ins: abi.instruction_cost,
            chains: vec![(abi.entry, abi.instruction_cost)],
            ..RegionOut::default()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::{AccessCheck, Argument, CodeBinding, WordSource};
    use crate::mem::{Memory as GuestMemory, P_R, P_W};
    use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

    const CPU: usize = 0x1000;
    const RTAB: u32 = 0x10000;
    const WTAB: u32 = RTAB + (1 << 20);
    const GUEST: u32 = WTAB + (1 << 20);
    const SIZE: u32 = 0x10000;
    const SP0: u32 = 0x8020;
    const ARGV0: u32 = 0x9000;
    const OBJECT: u32 = 0x6040;
    const ENTRY: u32 = 0x100;
    const CORE: u32 = 0x3020;
    const READ: &str = "params i32\na = parami 0\nreti a";
    const ADD: &str = "params i32 i32\na = parami 0\nb = parami 1\nr = addi a b\nreti r";
    const BUMP: &str = "params i32\na = parami 0\none = immi 1\nr = addi a one\nreti r";

    fn abi() -> Abi {
        Abi {
            entry: ENTRY,
            core: CORE,
            guest_size: SIZE,
            argc: 1,
            bindings: vec![RECEIVER_FIELD | 84],
            fields: vec![84],
            store: None,
            stack_low: -28,
            frame_offset: -20,
            instruction_cost: 37,
        }
    }

    fn layout() -> Layout {
        Layout::for_cpu(GUEST, RTAB, WTAB, 0)
    }

    // The existing general bridge is an independent executable oracle for the
    // original full-stack/core/argv contract and successful CPU commit. For
    // these differential cases the receiver is fixed, so its fields can be
    // passed as absolute WordSources without changing the general adapter.
    fn general(source: &str, abi: &Abi) -> Output {
        let n = abi.bindings.len();
        let full = format!(
            "params {} i32 i32 i32\nsp = parami {n}\nlimit = parami {}\nargv = parami {}\nsize = immi {}\nfsize = immi {}\nfour = immi 4\nend = immi {}\nok0 = geui sp size\nxf ok0 1\nlow = subi sp size\nframe = subi sp fsize\nok1 = geui frame limit\nxf ok1 2\nmin = addi sp four\nok2 = geui argv min\nxf ok2 3\nok3 = geui low end\nxf ok3 4\n{}",
            vec!["i32"; n].join(" "), n + 1, n + 2,
            abi.stack_low.unsigned_abs(), abi.frame_offset.unsigned_abs(), abi.core + 52,
            source.lines().skip(1).collect::<Vec<_>>().join("\n")
        );
        let mut arguments: Vec<_> = abi
            .bindings
            .iter()
            .map(|&binding| {
                if binding & RECEIVER_FIELD != 0 {
                    Argument::Word(WordSource::Memory(OBJECT + (binding & !RECEIVER_FIELD)))
                } else {
                    Argument::Argv {
                        pointer: WordSource::Stack(12),
                        offset: binding,
                    }
                }
            })
            .collect();
        arguments.extend([
            Argument::Word(WordSource::Gpr(ESP as u8)),
            Argument::Word(WordSource::Memory(abi.core + 32)),
            Argument::Word(WordSource::Stack(12)),
        ]);
        let mut access_checks = Vec::new();
        for write in [false, true] {
            access_checks.push(AccessCheck {
                base: WordSource::Gpr(ESP as u8),
                offset: abi.stack_low,
                size: abi.stack_low.unsigned_abs() + 16,
                write,
            });
            access_checks.push(AccessCheck {
                base: WordSource::Constant(abi.core + 32),
                offset: 0,
                size: 20,
                write,
            });
        }
        access_checks.push(AccessCheck {
            base: WordSource::Stack(12),
            offset: 0,
            size: (abi.argc + 1) * 4,
            write: false,
        });
        for &offset in &abi.fields {
            access_checks.push(AccessCheck {
                base: WordSource::Constant(OBJECT + offset),
                offset: 0,
                size: 4,
                write: false,
            });
            if abi.store == Some(offset) {
                access_checks.push(AccessCheck {
                    base: WordSource::Constant(OBJECT + offset),
                    offset: 0,
                    size: 4,
                    write: true,
                });
            }
        }
        let out = Trace::parse(&full)
            .unwrap()
            .compile_region(
                &layout(),
                crate::lir::Abi {
                    entry: abi.entry,
                    guest_size: abi.guest_size,
                    trusted_layout: true,
                    arguments,
                    checks: vec![(WordSource::Stack(8), abi.argc)],
                    bindings: vec![CodeBinding {
                        address: abi.entry,
                        bytes: vec![0x90, 0xc3],
                    }],
                    instruction_cost: abi.instruction_cost,
                    access_checks,
                    recheck_bindings: false,
                    count_completions: true,
                },
            )
            .unwrap();
        Output {
            wasm: out.wasm,
            region: out.region,
        }
    }

    struct Rig {
        store: Store<()>,
        memory: Memory,
        function: TypedFunc<(i32, i32), i32>,
        ram: GuestMemory,
    }

    impl Rig {
        fn new(output: &Output, abi: &Abi) -> Self {
            Self::with_layout(output, abi, &layout())
        }

        fn with_layout(output: &Output, abi: &Abi, layout: &Layout) -> Self {
            let engine = Engine::default();
            let module = Module::new(&engine, &output.wasm[..]).expect("adapter validates");
            let mut store = Store::new(&engine, ());
            let memory_end = (GUEST + SIZE).max(if layout.cache == 0 {
                0
            } else {
                layout.cache + CACHE_ENTRIES * 16
            });
            let memory = Memory::new(
                &mut store,
                MemoryType::new(memory_end.div_ceil(65536), None),
            )
            .unwrap();
            let mut linker = Linker::new(&engine);
            linker.define("env", "memory", memory).unwrap();
            if layout.cache != 0 {
                let table = wasmi::Table::new(
                    &mut store,
                    wasmi::TableType::new(wasmi::RefType::Func, 1, None),
                    wasmi::Ref::Func(wasmi::Nullable::Null),
                )
                .unwrap();
                linker.define("env", "table", table).unwrap();
                let mut caller = ModuleBuilder::new();
                caller.import_memory("env", "memory", 0);
                let signature = caller.ty(&[I32, I32], &[I32]);
                let mut c = Code::new();
                // The tail-chain test uses bump(17), returning/storing 18. The
                // caller must observe every commit before receiving its head.
                for (offset, expected) in [
                    (layout.cpu_r, 18),
                    (layout.cpu_r + ESP as u32 * 4, SP0 + 4),
                    (layout.cpu_eip, 0x500),
                    (layout.cpu_budget, 63),
                ] {
                    cpu(&mut c, offset);
                    c.u32c(expected)
                        .op(op::I32_NE)
                        .if_(0x40)
                        .unreachable()
                        .end();
                }
                c.u32c(OBJECT + 84)
                    .mem(op::I32_LOAD, GUEST)
                    .u32c(18)
                    .op(op::I32_NE)
                    .if_(0x40)
                    .unreachable()
                    .end();
                c.lget(0)
                    .mem(
                        op::I64_LOAD,
                        core::mem::offset_of!(Cpu, jit_lir_completed) as u32,
                    )
                    .i64c(1i64 << 32)
                    .op(op::I64_NE)
                    .if_(0x40)
                    .unreachable()
                    .end();
                c.lget(1);
                let function = caller.add_func(signature, 0, 0, 0, c.b);
                caller.export_func("caller", function);
                let caller = Module::new(&engine, &caller.finish()[..]).unwrap();
                let instance = linker.instantiate_and_start(&mut store, &caller).unwrap();
                let function = instance.get_func(&store, "caller").unwrap();
                table
                    .set(
                        &mut store,
                        0,
                        wasmi::Ref::Func(wasmi::Nullable::Val(function)),
                    )
                    .unwrap();
            }
            let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
            assert!(instance.get_func(&store, "trace").is_none());
            let function = instance.get_typed_func(&store, "r0").unwrap();
            let mut ram = GuestMemory::new(SIZE as usize);
            ram.set_prot(0, SIZE, P_R | P_W);
            for (index, byte) in ram.raw_mut().iter_mut().enumerate() {
                *byte = (index as u8).wrapping_mul(13);
            }
            let mut rig = Self {
                store,
                memory,
                function,
                ram,
            };
            rig.memory.data_mut(&mut rig.store)[GUEST as usize..(GUEST + SIZE) as usize]
                .copy_from_slice(rig.ram.raw());
            rig.sync_permissions();
            rig.guest(SP0, 0x500);
            rig.guest(SP0 + 4, 0x1234);
            rig.guest(SP0 + 8, abi.argc);
            rig.guest(SP0 + 12, ARGV0);
            rig.guest(ARGV0, OBJECT);
            rig.guest(ARGV0 + 4, 25);
            rig.guest(ARGV0 + 8, 31);
            rig.guest(OBJECT + 84, 17);
            rig.guest(OBJECT + 88, 25);
            rig.guest(CORE + 32, SP0 - 4096);
            rig.guest(CORE + 48, 0x12345678);
            let mut cpu = Cpu::new();
            cpu.eip = ENTRY;
            cpu.r = [0xabababab, 2, 3, 4, SP0, 6, 7, 8];
            cpu.jit_budget = 100;
            cpu.jit_lir_completed = u32::MAX as u64;
            cpu.jit_probe_resume = (1u64 << 32) | ENTRY as u64;
            cpu.jit_fault = 7;
            cpu.jit_fault_addr = 0xdeadbeef;
            cpu.jit_fault_write = 9;
            cpu.jit_aux = 11;
            cpu.lf_kind = 3;
            cpu.lf_a = 47;
            cpu.lf_b = 3;
            cpu.lf_r = 44;
            cpu.xmm = [0x0123456789abcdef_fedcba9876543210; 8];
            cpu.tsc = 0x123456789abcdef0;
            // Match the actual native Cpu image used by the wasmi backend.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &cpu as *const Cpu as *const u8,
                    rig.memory.data_mut(&mut rig.store).as_mut_ptr().add(CPU),
                    core::mem::size_of::<Cpu>(),
                );
            }
            rig
        }

        fn sync_permissions(&mut self) {
            let (r, w) = self.ram.jit_tables();
            let bytes = self.memory.data_mut(&mut self.store);
            bytes[RTAB as usize..RTAB as usize + r.len()].copy_from_slice(r);
            bytes[WTAB as usize..WTAB as usize + w.len()].copy_from_slice(w);
        }

        fn protect(&mut self, address: u32, prot: u8) {
            self.ram.set_prot(address & !4095, 4096, prot);
            self.sync_permissions();
        }

        fn mark_code(&mut self, address: u32) {
            self.ram.mark_code(address, 4);
            self.sync_permissions();
        }

        fn word(&mut self, address: usize, value: u32) {
            self.memory.data_mut(&mut self.store)[address..address + 4]
                .copy_from_slice(&value.to_le_bytes());
        }

        fn guest(&mut self, address: u32, value: u32) {
            self.word((GUEST + address) as usize, value);
        }

        fn cpu_word(&mut self, offset: u32, value: u32) {
            self.word(CPU + offset as usize, value);
        }

        fn permission(&mut self, table: u32, page: u32, value: u8) {
            self.memory.data_mut(&mut self.store)[(table + page) as usize] = value;
        }

        fn image(&self) -> Vec<u8> {
            self.memory.data(&self.store)[CPU..CPU + core::mem::size_of::<Cpu>()].to_vec()
        }

        fn guest_image(&self) -> Vec<u8> {
            self.memory.data(&self.store)[GUEST as usize..(GUEST + SIZE) as usize].to_vec()
        }

        fn call(&mut self, head: i32) -> u32 {
            self.function
                .call(&mut self.store, (CPU as i32, head))
                .expect("guest refusal must never trap wasm") as u32
        }

        fn refuses_unchanged(&mut self, head: i32) {
            let before = self.memory.data(&self.store).to_vec();
            assert_eq!(self.call(head), S_RETRY);
            assert_eq!(self.memory.data(&self.store), before);
        }
    }

    fn put(image: &mut [u8], offset: u32, value: u32) {
        image[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn expected_cpu(rig: &Rig, abi: &Abi, result: u32, budget: u32) -> Vec<u8> {
        let mut expected = rig.image();
        put(&mut expected, layout().cpu_r, result);
        put(&mut expected, layout().cpu_r + ESP as u32 * 4, SP0 + 4);
        put(&mut expected, layout().cpu_eip, 0x500);
        put(
            &mut expected,
            layout().cpu_budget,
            budget - abi.instruction_cost,
        );
        let counter = core::mem::offset_of!(Cpu, jit_lir_completed);
        let value = u64::from_le_bytes(expected[counter..counter + 8].try_into().unwrap());
        expected[counter..counter + 8].copy_from_slice(&value.wrapping_add(1).to_le_bytes());
        expected
    }

    #[test]
    fn memory_read_write_and_bump_match_source_and_general_bridge() {
        let mut seed = 0x9e3779b912345678u64;
        for method in ["getFirst", "getSum", "addTo", "setFirst", "bump"] {
            let mut a = abi();
            let source = match method {
                "getFirst" => {
                    a.argc = 0;
                    READ
                }
                "getSum" => {
                    a.argc = 0;
                    a.bindings.push(RECEIVER_FIELD | 88);
                    a.fields.push(88);
                    ADD
                }
                "addTo" => {
                    a.bindings.push(4);
                    ADD
                }
                "setFirst" => {
                    a.bindings = vec![4];
                    a.store = Some(84);
                    READ
                }
                _ => {
                    a.argc = 0;
                    a.store = Some(84);
                    BUMP
                }
            };
            let output = compile(&Trace::parse(source).unwrap(), &layout(), &a).unwrap();
            assert_eq!(output.region.heads, vec![(ENTRY, 0)]);
            assert_eq!(output.region.chains, vec![(ENTRY, 37)]);
            assert_eq!(output.region.n_ins, 37);
            let general = general(source, &a);
            let mut fast = Rig::new(&output, &a);
            let mut reference = Rig::new(&general, &a);
            for iteration in 0..128 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let first = seed as u32;
                let second = (seed >> 32) as u32;
                let argument = second.wrapping_mul(31);
                let input = fast.image();
                reference.memory.data_mut(&mut reference.store)[CPU..CPU + input.len()]
                    .copy_from_slice(&input);
                for rig in [&mut fast, &mut reference] {
                    rig.cpu_word(layout().cpu_eip, ENTRY);
                    rig.cpu_word(layout().cpu_r + ESP as u32 * 4, SP0);
                    rig.cpu_word(layout().cpu_budget, 100);
                    rig.guest(OBJECT + 84, first);
                    rig.guest(OBJECT + 88, second);
                    rig.guest(ARGV0 + 4, argument);
                }
                let result = match method {
                    "getFirst" => first,
                    "getSum" => first.wrapping_add(second),
                    "addTo" => first.wrapping_add(argument),
                    "setFirst" => argument,
                    _ => first.wrapping_add(1),
                };
                let expected_cpu = expected_cpu(&fast, &a, result, 100);
                let mut expected_memory = fast.guest_image();
                if let Some(offset) = a.store {
                    put(&mut expected_memory, OBJECT + offset, result);
                }
                assert_eq!(fast.call(0), S_NEXT, "{method} {iteration}");
                assert_eq!(reference.call(0), S_NEXT, "{method} {iteration}");
                assert_eq!(fast.image(), expected_cpu, "{method} {iteration}");
                assert_eq!(fast.image(), reference.image(), "{method} {iteration}");
                assert_eq!(fast.guest_image(), expected_memory, "{method} {iteration}");
                // The general scalar bridge has no store; apply the source
                // oracle's sole heap effect to compare the complete RAM image.
                if let Some(offset) = a.store {
                    reference.guest(OBJECT + offset, result);
                }
                assert_eq!(fast.guest_image(), reference.guest_image());
            }
        }
    }

    #[test]
    fn memory_leaf_preserves_existing_general_stack_contract() {
        let a = abi();
        let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
        let general = general(READ, &a);
        for case in 0..27 {
            let mut fast = Rig::new(&output, &a);
            let mut reference = Rig::new(&general, &a);
            for rig in [&mut fast, &mut reference] {
                match case {
                    0 => rig.cpu_word(layout().cpu_eip, ENTRY + 1),
                    1 => {} // invalid head
                    2 => rig.cpu_word(layout().cpu_budget, 36),
                    3 => rig.cpu_word(layout().cpu_budget, u32::MAX),
                    4 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 27),
                    5 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, u32::MAX - 3),
                    6 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 0xfff4),
                    7 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 0x8000),
                    8 => rig.protect(SP0, P_W),
                    9 => rig.protect(SP0, P_R),
                    10 => rig.protect(CORE + 32, P_W),
                    11 => rig.protect(CORE + 32, P_R),
                    12 => rig.guest(CORE + 32, SP0 - 19),
                    13 => rig.guest(SP0 + 8, 0),
                    14 => rig.guest(SP0 + 12, SP0 - 4),
                    15 => rig.guest(SP0 + 12, SP0),
                    16 => rig.guest(SP0 + 12, u32::MAX - 3),
                    17 => rig.guest(SP0 + 12, 0xfffc),
                    18 => rig.guest(SP0 + 12, 0x9ffc),
                    19 => rig.protect(ARGV0, P_W),
                    20 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, CORE + 60),
                    21 => rig.permission(RTAB, ARGV0 >> 12, 4),
                    22 => rig.permission(WTAB, SP0 >> 12, 2),
                    23 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 0x8ff4),
                    24 => rig.guest(SP0 + 12, u32::MAX),
                    25 => rig.guest(CORE + 32, u32::MAX),
                    _ => rig.cpu_word(layout().cpu_budget, 0),
                }
            }
            let head = if case == 1 { -1 } else { 0 };
            fast.refuses_unchanged(head);
            reference.refuses_unchanged(head);
        }
    }

    #[test]
    fn memory_aliases_with_transient_stack_core_and_all_argv_are_refused() {
        for store in [None, Some(84)] {
            let mut a = abi();
            a.store = store;
            a.fields.push(88);
            a.argc = 2; // even the unused last argv word must not be modified
            let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
            for field_address in [
                SP0 - 28,
                SP0 - 24,
                SP0 - 4,
                SP0,
                SP0 + 4,
                SP0 + 8,
                SP0 + 12,
                CORE + 32,
                CORE + 36,
                CORE + 48,
                ARGV0 - 4,
                ARGV0,
                ARGV0 + 4,
                ARGV0 + 8,
            ] {
                let mut rig = Rig::new(&output, &a);
                rig.guest(ARGV0, field_address - 84);
                rig.refuses_unchanged(0);
            }
        }
        // Neither individual access overlaps the stack, but the accessed
        // object interval straddles it. Conservatively reject the whole span.
        let mut a = abi();
        a.fields = vec![0, 4092];
        a.bindings = vec![RECEIVER_FIELD];
        let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
        let mut rig = Rig::new(&output, &a);
        rig.guest(ARGV0, SP0 - 2048);
        rig.refuses_unchanged(0);
    }

    #[test]
    fn memory_all_native_fields_permissions_codepages_and_wrap_are_checked() {
        for store in [None, Some(84)] {
            let mut a = abi();
            a.fields = vec![84, 4092]; // second field is a dead native read
            a.store = store;
            let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
            for case in 0..12 {
                let mut rig = Rig::new(&output, &a);
                match case {
                    0 => rig.protect(OBJECT + 84, P_W),
                    1 => rig.protect(OBJECT + 4092, 0),
                    2 => rig.permission(RTAB, (OBJECT + 84) >> 12, 4),
                    3 => rig.permission(RTAB, (OBJECT + 4092) >> 12, 2),
                    4 => rig.guest(ARGV0, u32::MAX),
                    5 => rig.guest(ARGV0, u32::MAX - 83),
                    6 => rig.guest(ARGV0, SIZE - 4095),
                    7 => rig.guest(ARGV0, 0x6fa9), // first field crosses a page
                    8 => rig.guest(ARGV0, SIZE),
                    9 => rig.mark_code(SP0),
                    10 => rig.mark_code(CORE + 32),
                    _ => rig.permission(RTAB, ARGV0 >> 12, 2),
                }
                rig.refuses_unchanged(0);
            }
        }
        let mut a = abi();
        a.store = Some(84);
        let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
        for case in 0..4 {
            let mut rig = Rig::new(&output, &a);
            match case {
                0 => rig.protect(OBJECT + 84, P_R),
                1 => rig.mark_code(OBJECT + 84),
                2 => rig.permission(WTAB, (OBJECT + 84) >> 12, 2),
                _ => rig.permission(WTAB, (OBJECT + 84) >> 12, 4),
            }
            rig.refuses_unchanged(0);
            assert!(rig.ram.dirty_code.is_empty());
        }
    }

    #[test]
    fn memory_boundary_equalities_readonly_inputs_and_raw_receiver_are_supported() {
        let mut a = abi();
        a.argc = 0;
        let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
        for receiver in [
            SP0 - 28 - 88,  // object end == stack low
            SP0 + 16 - 84,  // object start == stack end
            CORE + 32 - 88, // object end == core start
            CORE + 52 - 84, // object start == core end
            ARGV0 - 88,     // object end == argv start
            ARGV0 + 4 - 84, // object start == argv end
            SIZE - 88,      // field end == guest end
            OBJECT + 1,     // no alignment/tag/type assumption about receiver
        ] {
            let mut rig = Rig::new(&output, &a);
            rig.guest(ARGV0, receiver);
            rig.guest(receiver + 84, 0xfedcba98);
            rig.guest(CORE + 32, SP0 - 20); // frame == stack limit
            let expected = expected_cpu(&rig, &a, 0xfedcba98, 100);
            let memory = rig.guest_image();
            assert_eq!(rig.call(0), S_NEXT, "receiver {receiver:#x}");
            assert_eq!(rig.image(), expected);
            assert_eq!(rig.guest_image(), memory);
        }
        let mut rig = Rig::new(&output, &a);
        rig.protect(OBJECT, P_R);
        rig.mark_code(OBJECT); // reading code is fine; storing would retry
        rig.protect(ARGV0, P_R);
        rig.guest(SP0 + 12, 0x9ffc); // receiver-only argv ends at page boundary
        rig.guest(0x9ffc, OBJECT);
        assert_eq!(rig.call(0), S_NEXT);

        a.bindings = vec![0, RECEIVER_FIELD | 84];
        let output = compile(&Trace::parse(ADD).unwrap(), &layout(), &a).unwrap();
        let mut rig = Rig::new(&output, &a);
        let expected = expected_cpu(&rig, &a, OBJECT + 17, 100);
        assert_eq!(rig.call(0), S_NEXT);
        assert_eq!(rig.image(), expected);
    }

    #[test]
    fn memory_full_unused_argv_and_unused_store_field_are_preflighted() {
        let mut a = abi();
        a.argc = 16; // no scalar binding references any of these arguments
        let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
        let mut rig = Rig::new(&output, &a);
        rig.guest(SP0 + 12, 0x9fc0); // receiver fits; full 68-byte span does not
        rig.guest(0x9fc0, OBJECT);
        rig.refuses_unchanged(0);
        a.argc = 1;
        a.fields = vec![84, 4092];
        a.store = Some(4092);
        let output = compile(&Trace::parse(READ).unwrap(), &layout(), &a).unwrap();
        let mut rig = Rig::new(&output, &a);
        rig.protect(OBJECT + 4092, P_R);
        rig.refuses_unchanged(0);
        let mut rig = Rig::new(&output, &a);
        let mut expected = rig.guest_image();
        put(&mut expected, OBJECT + 4092, 17);
        assert_eq!(rig.call(0), S_NEXT);
        assert_eq!(rig.guest_image(), expected);
    }

    #[test]
    fn memory_budget_equality_commits_and_live_caller_tailchain_sees_store() {
        let mut a = abi();
        a.argc = 0;
        a.store = Some(84);
        let mut l = layout();
        l.cache = GUEST + SIZE;
        let output = compile(&Trace::parse(BUMP).unwrap(), &l, &a).unwrap();
        let cache_offset = ((0x500u32 ^ (0x500u32 >> 12)) & (CACHE_ENTRIES - 1)) * 16;
        let cache = (l.cache + cache_offset) as usize;
        for case in 0..4 {
            let mut rig = Rig::with_layout(&output, &a, &l);
            rig.word(cache, if case == 1 { 0x501 } else { 0x500 });
            rig.word(cache + 4, if case == 2 { 0 } else { 1 });
            rig.word(cache + 8, 123);
            let budget = if case == 3 { 37 } else { 100 };
            rig.cpu_word(l.cpu_budget, budget);
            let expected = expected_cpu(&rig, &a, 18, budget);
            let mut memory = rig.guest_image();
            put(&mut memory, OBJECT + 84, 18);
            assert_eq!(rig.call(0), if case == 0 { 123 } else { S_NEXT });
            assert_eq!(rig.image(), expected);
            assert_eq!(rig.guest_image(), memory);
        }
    }

    #[test]
    fn memory_invalid_compile_contract_and_short_memory_are_rejected() {
        let trace = Trace::parse(READ).unwrap();
        for case in 0..23 {
            let mut a = abi();
            let mut l = layout();
            match case {
                0 => a.argc = 17,
                1 => a.instruction_cost = 0,
                2 => a.instruction_cost = u32::MAX,
                3 => a.stack_low = i32::MIN,
                4 => a.stack_low = -4081,
                5 => a.frame_offset = -32,
                6 => a.bindings[0] = 8,
                7 => a.bindings[0] = 1,
                8 => a.bindings[0] = RECEIVER_FIELD | 88,
                9 => a.fields[0] = 85,
                10 => a.fields[0] = 4096,
                11 => a.store = Some(88),
                12 => a.store = Some(u32::MAX),
                13 => a.fields = vec![84; 17],
                14 => a.bindings = vec![RECEIVER_FIELD | 84; 17],
                15 => a.core = u32::MAX - 16,
                16 => a.guest_size = 16,
                17 => a.entry = SIZE,
                18 => l.cpu_r += 4,
                19 => l.guest_base = u32::MAX - 16,
                20 => l.rtab = u32::MAX,
                21 => l.wtab = GUEST,
                _ => l.cache = u32::MAX - 16,
            }
            assert!(compile(&trace, &l, &a).is_err(), "case {case}");
        }
        for source in [
            "params i32\na = parami 0\nxf a 1\nreti a",
            "params f64\na = argd 0\nr = d2i a\nreti r",
            "params i32\na = immd 1\nb = immi 1\nreti b",
        ] {
            assert!(compile(&Trace::parse(source).unwrap(), &layout(), &abi()).is_err());
        }
        let output = compile(&trace, &layout(), &abi()).unwrap();
        let engine = Engine::default();
        let module = Module::new(&engine, &output.wasm[..]).unwrap();
        let mut store = Store::new(&engine, ());
        let short = Memory::new(&mut store, MemoryType::new(GUEST / 65536, None)).unwrap();
        let mut linker = Linker::new(&engine);
        linker.define("env", "memory", short).unwrap();
        assert!(linker.instantiate_and_start(&mut store, &module).is_err());
    }
}
