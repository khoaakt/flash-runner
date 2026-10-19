//! Specialized trusted-layout cdecl adapter for verified, captured integer leafs.
//!
//! This is an emitter, not a native-code verifier. The installer must first prove
//! equivalence, bind **all** native dependencies, and maintain those bindings via
//! normal SMC invalidation. No native bytes are rechecked in the hot adapter.
//! `layout` and the runtime Cpu pointer must be the emulator's stable allocations,
//! not guest-controlled pointers. Guest addresses/protections are always checked.

use super::{error, Error, Instruction, Trace, Type};
use crate::cpu::{Cpu, EAX, ESP};
use crate::jit_x86::{Layout, RegionOut, CACHE_ENTRIES, S_NEXT, S_RETRY};
use crate::wasm_emit::{op, Code, ModuleBuilder, I32};

/// Metadata established by the native equivalence verifier. Offsets are relative
/// to the original ESP or argv, not to an allocated/rebased frame.
#[derive(Clone, Debug)]
pub struct Abi {
    pub entry: u32,
    pub core: u32,
    pub guest_size: u32,
    pub argc: u32,
    pub offsets: Vec<u32>,
    pub stack_low: i32,
    pub frame_offset: i32,
    /// Positive, bounded native instruction charge for a complete leaf call.
    pub instruction_cost: u32,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub wasm: Vec<u8>,
    pub region: RegionOut,
}

// r0 parameters are Cpu and head; ModuleBuilder declares i64 locals first.
const RESULT: u32 = 2;
const SP: u32 = 3;
const ARGV: u32 = 4;
const RET: u32 = 5;
const BUDGET: u32 = 6;
const LOW: u32 = 7;
const PAGE: u32 = 8;

fn reject(c: &mut Code, status: u32) {
    c.if_(0x40).u32c(status).op(op::RETURN).end();
}

fn cpu(c: &mut Code, offset: u32) {
    c.lget(0).mem(op::I32_LOAD, offset);
}

/// The range is already guest-bounded. Its first page is used only after the
/// same-page test succeeds; table allocations are covered by the memory import.
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

/// Emit `r0(cpu, head) -> status`, with the normalized trace private in the same
/// module. Call only after native equivalence verification; dependency binding is
/// deliberately outside this API. Only guard-free, entirely i32 traces qualify.
///
/// Every refusal leaves the complete Cpu and guest memory untouched. A successful
/// call changes only EAX, ESP, EIP, budget, and the u64 completed-call counter. In
/// particular it never transiently links MethodFrame or changes lazy flags. The
/// verifier must prove those omitted writes are nonescaping on this no-call path.
/// With a live `layout.cache`, successful returns tail-chain to a cached caller.
/// Entry by dispatcher or by another region's tail call shares exactly the same
/// completion charge and counter increment.
pub fn compile(trace: &Trace, layout: &Layout, abi: &Abi) -> Result<Output, Error> {
    let bad = |message| error(0, message);
    if trace.result() != Type::I32
        || trace.params().len() != abi.offsets.len()
        || trace.params().iter().any(|ty| *ty != Type::I32)
        || trace.values.iter().any(|value| value.ty != Type::I32)
        || trace
            .instructions
            .iter()
            .any(|ins| matches!(ins, Instruction::Guard { .. }))
    {
        return Err(bad("fast leaf requires a guard-free, entirely i32 trace"));
    }
    if abi.argc > 16
        || abi.offsets.len() > 16
        || abi
            .offsets
            .iter()
            .any(|&offset| offset < 4 || offset > abi.argc * 4 || offset % 4 != 0)
        || !(-4080..=-4).contains(&abi.stack_low)
        || !(abi.stack_low..=-4).contains(&abi.frame_offset)
        || abi.guest_size < 4096
        || abi.entry >= abi.guest_size
        || abi.instruction_cost == 0
        || abi.instruction_cost > i32::MAX as u32
    {
        return Err(bad("invalid verified integer-leaf ABI bounds"));
    }
    let core_end = abi
        .core
        .checked_add(52)
        .filter(|&end| end <= abi.guest_size)
        .ok_or_else(|| bad("core range lies outside guest memory"))?;
    let native = Layout::for_cpu(layout.guest_base, layout.rtab, layout.wtab, layout.cache);
    if [layout.cpu_r, layout.cpu_eip, layout.cpu_budget]
        != [native.cpu_r, native.cpu_eip, native.cpu_budget]
    {
        return Err(bad("fast leaf requires this build's native Cpu layout"));
    }

    // A trusted allocation still needs a representable wasm32 range. Importing
    // the complete extents makes memory bounds an instantiation-time condition,
    // avoiding memory.size and arbitrary layout checks on every invocation.
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
    let argv_size = abi.offsets.iter().copied().max().unwrap_or(0) + 4;
    let mut module = ModuleBuilder::new();
    module.import_memory("env", "memory", memory_end.div_ceil(65536) as u32);
    if layout.cache != 0 {
        module.import_table("env", "table", 0);
    }
    let signature = module.ty(&[I32, I32], &[I32]);
    let semantic = trace.emit_integer_function(&mut module)?;
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
        .ltee(LOW);
    // One unsigned bound rejects both subtraction underflow and sp+16 overflow:
    // a wrapped low is >= 2^32-|stack_low|, strictly above guest_size-stack_size.
    c.u32c(abi.guest_size - stack_size).op(op::I32_GT_U);
    reject(&mut c, S_RETRY);
    c.lget(LOW).u32c(core_end).op(op::I32_LT_U);
    reject(&mut c, S_RETRY);
    page_check(&mut c, layout, LOW, stack_size, true);

    // Both core fields must be readable, and the native MethodFrame link must
    // be writable. Conservatively require all [core+32, core+52) to be R+W.
    let core_start = abi.core + 32;
    if core_start & 4095 > 4096 - 20 {
        c.u32c(S_RETRY).op(op::RETURN);
    } else {
        c.u32c(core_start >> 12)
            .mem(op::I32_LOAD8_U, layout.rtab)
            .u32c(core_start >> 12)
            .mem(op::I32_LOAD8_U, layout.wtab)
            .op(op::I32_AND)
            .u32c(1)
            .op(op::I32_AND)
            .op(op::I32_EQZ);
        reject(&mut c, S_RETRY);
    }
    c.lget(SP)
        .u32c(abi.frame_offset.unsigned_abs())
        .op(op::I32_SUB)
        .u32c(core_start)
        .mem(op::I32_LOAD, layout.guest_base)
        .op(op::I32_LT_U);
    reject(&mut c, S_RETRY);

    // Stack preflight covers ret, env (even though unused), argc, and argv.
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
    c.lget(SP).mem(op::I32_LOAD, layout.guest_base).lset(RET);
    for offset in &abi.offsets {
        c.lget(ARGV).mem(op::I32_LOAD, layout.guest_base + offset);
    }
    // The validated guard-free trace always returns status zero. No dynamic
    // status dispatch, repeated argv loads, or per-argument page checks remain.
    c.call(semantic).op(op::I64_EXTEND_I32_S).lset(RESULT);

    // Atomic commit: all guest reads/protection checks have already succeeded.
    c.lget(0)
        .lget(RESULT)
        .op(op::I32_WRAP_I64)
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
        // All observable state, including the completed-call counter, is already
        // committed before control can leave this module. The live cache/table
        // is runtime-owned and maintained by the ordinary SMC invalidation path.
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
    let r0 = module.add_func(signature, 6, 1, 0, c.b);
    module.name_func(r0, "captured_integer_leaf");
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
    use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

    const CPU: usize = 0x1000;
    const RTAB: u32 = 0x10000;
    const WTAB: u32 = RTAB + (1 << 20);
    const GUEST: u32 = WTAB + (1 << 20);
    const SP0: u32 = 0x8020;
    const ARGV0: u32 = 0x9000;
    const ENTRY: u32 = 0x100;
    const CORE: u32 = 0x3020;
    const SOURCE: &str = "params i32 i32\na = parami 0\nb = parami 1\nr = subi a b\nreti r";

    fn abi() -> Abi {
        Abi {
            entry: ENTRY,
            core: CORE,
            guest_size: 0x10000,
            argc: 2,
            offsets: vec![4, 8],
            stack_low: -28,
            frame_offset: -20,
            instruction_cost: 37,
        }
    }

    fn layout() -> Layout {
        Layout::for_cpu(GUEST, RTAB, WTAB, 0)
    }

    // Reference the existing general bridge, including equivalent semantic
    // guards and full-span preflights. No changes to its emitter are needed.
    fn general(source: &str, abi: &Abi) -> Output {
        let n = abi.offsets.len();
        let full = format!(
            "params {} i32 i32 i32\nsp = parami {n}\nlimit = parami {}\nargv = parami {}\nsize = immi {}\nfsize = immi {}\nfour = immi 4\nend = immi {}\nok0 = geui sp size\nxf ok0 1\nlow = subi sp size\nframe = subi sp fsize\nok1 = geui frame limit\nxf ok1 2\nmin = addi sp four\nok2 = geui argv min\nxf ok2 3\nok3 = geui low end\nxf ok3 4\n{}",
            vec!["i32"; n].join(" "), n + 1, n + 2,
            abi.stack_low.unsigned_abs(), abi.frame_offset.unsigned_abs(), abi.core + 52,
            source.lines().skip(1).collect::<Vec<_>>().join("\n")
        );
        let mut arguments: Vec<_> = abi
            .offsets
            .iter()
            .map(|&offset| Argument::Argv {
                pointer: WordSource::Stack(12),
                offset,
            })
            .collect();
        arguments.extend([
            Argument::Word(WordSource::Gpr(ESP as u8)),
            Argument::Word(WordSource::Memory(abi.core + 32)),
            Argument::Word(WordSource::Stack(12)),
        ]);
        let size = abi.stack_low.unsigned_abs() + 16;
        let scalar = Trace::parse(&full)
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
                    access_checks: vec![
                        AccessCheck {
                            base: WordSource::Gpr(ESP as u8),
                            offset: abi.stack_low,
                            size,
                            write: false,
                        },
                        AccessCheck {
                            base: WordSource::Gpr(ESP as u8),
                            offset: abi.stack_low,
                            size,
                            write: true,
                        },
                        AccessCheck {
                            base: WordSource::Constant(abi.core + 32),
                            offset: 0,
                            size: 20,
                            write: false,
                        },
                        AccessCheck {
                            base: WordSource::Constant(abi.core + 32),
                            offset: 0,
                            size: 20,
                            write: true,
                        },
                        AccessCheck {
                            base: WordSource::Stack(12),
                            offset: 0,
                            size: abi.offsets.iter().copied().max().unwrap_or(0) + 4,
                            write: false,
                        },
                    ],
                    recheck_bindings: false,
                    count_completions: true,
                },
            )
            .unwrap();
        Output {
            wasm: scalar.wasm,
            region: scalar.region,
        }
    }

    struct Rig {
        store: Store<()>,
        memory: Memory,
        function: TypedFunc<(i32, i32), i32>,
    }
    impl Rig {
        fn new(output: &Output) -> Self {
            Self::with_layout(output, &layout())
        }
        fn with_layout(output: &Output, layout: &Layout) -> Self {
            let engine = Engine::default();
            let module = Module::new(&engine, &output.wasm[..]).expect("adapter validates");
            let mut store = Store::new(&engine, ());
            let memory_end = if layout.cache == 0 {
                GUEST + 0x10000
            } else {
                (layout.cache + CACHE_ENTRIES * 16).max(GUEST + 0x10000)
            };
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
                // A wasm caller proves EAX/ESP/EIP/budget/counter are committed
                // before the indirect tail transfer. Return its received head.
                let mut caller = ModuleBuilder::new();
                caller.import_memory("env", "memory", 0);
                let signature = caller.ty(&[I32, I32], &[I32]);
                let mut c = Code::new();
                for (offset, expected) in [
                    (layout.cpu_r, -8i32 as u32),
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
                c.lget(0)
                    .mem(
                        op::I64_LOAD,
                        core::mem::offset_of!(Cpu, jit_lir_completed) as u32,
                    )
                    .i64c(1u64.wrapping_shl(32) as i64)
                    .op(op::I64_NE)
                    .if_(0x40)
                    .unreachable()
                    .end();
                c.lget(1);
                let function = caller.add_func(signature, 0, 0, 0, c.b);
                caller.export_func("caller", function);
                let caller = Module::new(&engine, &caller.finish()[..]).unwrap();
                let caller = linker.instantiate_and_start(&mut store, &caller).unwrap();
                let caller = caller.get_func(&store, "caller").unwrap();
                table
                    .set(
                        &mut store,
                        0,
                        wasmi::Ref::Func(wasmi::Nullable::Val(caller)),
                    )
                    .unwrap();
            }
            let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
            assert!(instance.get_func(&store, "trace").is_none());
            let function = instance.get_typed_func(&store, "r0").unwrap();
            let mut rig = Self {
                store,
                memory,
                function,
            };
            for page in 0..16 {
                rig.permission(RTAB, page, 1);
                rig.permission(WTAB, page, 1);
            }
            rig.guest(SP0, 0x500);
            rig.guest(SP0 + 4, 0x1234);
            rig.guest(SP0 + 8, 2);
            rig.guest(SP0 + 12, ARGV0);
            rig.guest(ARGV0 + 4, 17);
            rig.guest(ARGV0 + 8, 25);
            rig.guest(CORE + 32, SP0 - 4096);
            rig.guest(CORE + 48, 0x12345678);
            let mut cpu = Cpu::new();
            cpu.eip = ENTRY;
            cpu.r = [0xabababab, 2, 3, 4, SP0, 6, 7, 8];
            cpu.jit_budget = 100;
            cpu.jit_lir_completed = u32::MAX as u64;
            cpu.lf_kind = 3;
            cpu.lf_a = 47;
            cpu.lf_b = 3;
            cpu.lf_r = 44;
            // Match the native Cpu image used by the real wasmi runtime backend.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &cpu as *const Cpu as *const u8,
                    rig.memory.data_mut(&mut rig.store).as_mut_ptr().add(CPU),
                    core::mem::size_of::<Cpu>(),
                );
            }
            rig
        }
        fn word(&mut self, address: usize, value: u32) {
            self.memory.data_mut(&mut self.store)[address..address + 4]
                .copy_from_slice(&value.to_le_bytes());
        }
        fn guest(&mut self, address: u32, value: u32) {
            self.word((GUEST + address) as usize, value);
        }
        fn permission(&mut self, table: u32, page: u32, value: u8) {
            self.memory.data_mut(&mut self.store)[(table + page) as usize] = value;
        }
        fn cpu_word(&mut self, offset: u32, value: u32) {
            self.word(CPU + offset as usize, value);
        }
        fn image(&self) -> Vec<u8> {
            self.memory.data(&self.store)[CPU..CPU + core::mem::size_of::<Cpu>()].to_vec()
        }
        fn guest_image(&self) -> Vec<u8> {
            self.memory.data(&self.store)[GUEST as usize..(GUEST + 0x10000) as usize].to_vec()
        }
        fn call(&mut self, head: i32) -> u32 {
            self.function
                .call(&mut self.store, (CPU as i32, head))
                .expect("no guest fault may trap wasm") as u32
        }
    }

    fn put(image: &mut [u8], offset: u32, value: u32) {
        image[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn fast_leaf_matches_general_bridge_and_source() {
        for operation in [
            "addi", "subi", "muli", "andi", "ori", "xori", "lshi", "rshi", "rshui", "eqi", "lti",
            "gti", "lei", "gei", "ltui", "gtui", "leui", "geui",
        ] {
            let source = SOURCE.replace("subi", operation);
            let trace = Trace::parse(&source).unwrap();
            let fast = compile(&trace, &layout(), &abi()).unwrap();
            let general = general(&source, &abi());
            assert_eq!(fast.region.heads, vec![(ENTRY, 0)]);
            assert_eq!(fast.region.chains, vec![(ENTRY, 37)]);
            assert!(
                fast.wasm.len() < general.wasm.len(),
                "{operation}: specialized adapter should be smaller"
            );
            let mut f = Rig::new(&fast);
            let mut g = Rig::new(&general);
            let engine = Engine::default();
            let module = Module::new(&engine, &trace.compile()[..]).unwrap();
            let mut store = Store::new(&engine, ());
            let instance = Linker::new(&engine)
                .instantiate_and_start(&mut store, &module)
                .unwrap();
            let oracle = instance
                .get_typed_func::<(i32, i32), (i32, i64)>(&store, "trace")
                .unwrap();
            let mut seed = 0x9e3779b912345678u64;
            for iteration in 0..128 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let a = seed as u32;
                let b = (seed >> 32) as u32;
                let input = f.image();
                g.memory.data_mut(&mut g.store)[CPU..CPU + input.len()].copy_from_slice(&input);
                for rig in [&mut f, &mut g] {
                    rig.cpu_word(layout().cpu_eip, ENTRY);
                    rig.cpu_word(layout().cpu_r + ESP as u32 * 4, SP0);
                    rig.cpu_word(layout().cpu_budget, 100);
                    rig.guest(ARGV0 + 4, a);
                    rig.guest(ARGV0 + 8, b);
                }
                let before = f.guest_image();
                assert_eq!(f.call(0), S_NEXT, "{operation} {iteration}");
                assert_eq!(g.call(0), S_NEXT);
                assert_eq!(f.image(), g.image(), "{operation} {iteration}");
                assert_eq!(f.guest_image(), before);
                let (status, result) = oracle.call(&mut store, (a as i32, b as i32)).unwrap();
                assert_eq!(status, 0);
                let image = f.image();
                assert_eq!(
                    u32::from_le_bytes(
                        image[layout().cpu_r as usize..layout().cpu_r as usize + 4]
                            .try_into()
                            .unwrap()
                    ),
                    result as u32
                );
            }
        }
    }

    #[test]
    fn fast_leaf_commits_only_return_budget_and_completed_counter() {
        let output = compile(&Trace::parse(SOURCE).unwrap(), &layout(), &abi()).unwrap();
        let mut rig = Rig::new(&output);
        let mut expected = rig.image();
        put(&mut expected, layout().cpu_r, (-8i32) as u32);
        put(&mut expected, layout().cpu_r + ESP as u32 * 4, SP0 + 4);
        put(&mut expected, layout().cpu_eip, 0x500);
        put(&mut expected, layout().cpu_budget, 63);
        let counter = core::mem::offset_of!(Cpu, jit_lir_completed);
        expected[counter..counter + 8].copy_from_slice(&(1u64 << 32).to_le_bytes());
        let memory = rig.guest_image();
        assert_eq!(rig.call(0), S_NEXT);
        assert_eq!(rig.image(), expected); // lazy flags, FP, callee-saved, etc.
        assert_eq!(rig.guest_image(), memory); // no transient MethodFrame/spill writes
    }

    #[test]
    fn fast_leaf_refusals_preserve_complete_cpu_and_guest_images() {
        let output = compile(&Trace::parse(SOURCE).unwrap(), &layout(), &abi()).unwrap();
        for case in 0..26 {
            let mut rig = Rig::new(&output);
            match case {
                0 => rig.cpu_word(layout().cpu_eip, ENTRY + 1),
                1 => {} // invalid head
                2 => rig.cpu_word(layout().cpu_budget, 36),
                3 => rig.cpu_word(layout().cpu_budget, u32::MAX),
                4 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 27), // stack underflow
                5 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, u32::MAX - 3),
                6 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 0xfff4), // sp+16 out of guest
                7 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 0x8000), // low crosses page
                8 => rig.permission(RTAB, SP0 >> 12, 0),
                9 => rig.permission(WTAB, SP0 >> 12, 0),
                10 => rig.permission(RTAB, (CORE + 32) >> 12, 0),
                11 => rig.permission(WTAB, (CORE + 32) >> 12, 0),
                12 => rig.guest(CORE + 32, SP0 - 19), // MethodFrame below limit
                13 => rig.guest(SP0 + 8, 1),
                14 => rig.guest(SP0 + 12, SP0 - 4), // dead-frame alias
                15 => rig.guest(SP0 + 12, SP0),
                16 => rig.guest(SP0 + 12, u32::MAX - 3),
                17 => rig.guest(SP0 + 12, 0xfff8),
                18 => rig.guest(SP0 + 12, 0x9ff8), // argv span crosses page
                19 => rig.permission(RTAB, ARGV0 >> 12, 0),
                20 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, CORE + 60), // core/frame alias
                21 => rig.permission(RTAB, ARGV0 >> 12, 4), // decoded-only bit is not read permission
                22 => rig.permission(WTAB, SP0 >> 12, 2), // adjacent-page bit is not current permission
                23 => rig.cpu_word(layout().cpu_r + ESP as u32 * 4, 0x8ff4), // sp+16 crosses page
                24 => rig.guest(SP0 + 12, u32::MAX),
                _ => rig.guest(CORE + 32, u32::MAX),
            }
            let cpu = rig.image();
            let guest = rig.guest_image();
            assert_eq!(
                rig.call(if case == 1 { -1 } else { 0 }),
                S_RETRY,
                "case {case}"
            );
            assert_eq!(rig.image(), cpu, "case {case}");
            assert_eq!(rig.guest_image(), guest, "case {case}");
        }
    }

    #[test]
    fn fast_leaf_boundary_equality_and_readonly_argv_succeed() {
        let output = compile(&Trace::parse(SOURCE).unwrap(), &layout(), &abi()).unwrap();
        let mut rig = Rig::new(&output);
        rig.guest(CORE + 32, SP0 - 20); // frame == stack limit
        rig.permission(WTAB, ARGV0 >> 12, 0); // argv requires read, not write
        rig.guest(SP0 + 12, 0x9ff4); // maxoff+4 ends exactly at page boundary
        rig.guest(0x9ff8, 3);
        rig.guest(0x9ffc, 5);
        assert_eq!(rig.call(0), S_NEXT);
    }

    #[test]
    fn fast_leaf_tailchains_only_to_live_matching_caller_after_commit() {
        let mut l = layout();
        l.cache = GUEST + 0x10000;
        let output = compile(&Trace::parse(SOURCE).unwrap(), &l, &abi()).unwrap();
        let cache_offset = ((0x500u32 ^ (0x500u32 >> 12)) & (CACHE_ENTRIES - 1)) * 16;
        let cache = (l.cache + cache_offset) as usize;
        for case in 0..4 {
            let mut rig = Rig::with_layout(&output, &l);
            rig.word(cache, if case == 1 { 0x501 } else { 0x500 });
            rig.word(cache + 4, if case == 2 { 0 } else { 1 });
            rig.word(cache + 8, 123);
            if case == 3 {
                rig.cpu_word(l.cpu_budget, 37); // success, but no budget to chain
            }
            let guest = rig.guest_image();
            assert_eq!(
                rig.call(0),
                if case == 0 { 123 } else { S_NEXT },
                "case {case}"
            );
            assert_eq!(rig.guest_image(), guest);
            let image = rig.image();
            let counter = core::mem::offset_of!(Cpu, jit_lir_completed);
            assert_eq!(
                u64::from_le_bytes(image[counter..counter + 8].try_into().unwrap()),
                1u64 << 32
            );
        }
    }

    #[test]
    fn fast_leaf_rejects_invalid_compile_contract_and_short_linear_memory() {
        let trace = Trace::parse(SOURCE).unwrap();
        for case in 0..10 {
            let mut a = abi();
            let mut l = layout();
            match case {
                0 => a.instruction_cost = 0,
                1 => a.stack_low = i32::MIN,
                2 => a.frame_offset = -32,
                3 => a.offsets[0] = 9,
                4 => a.core = u32::MAX - 16,
                5 => a.guest_size = 16,
                6 => l.cpu_r += 4,
                7 => l.guest_base = u32::MAX - 16,
                8 => l.rtab = u32::MAX,
                _ => l.wtab = GUEST,
            }
            assert!(compile(&trace, &l, &a).is_err(), "case {case}");
        }
        for text in [
            "params i32 i32\na = parami 0\nxf a 1\nreti a",
            "params i32 f64\na = argd 1\nr = d2i a\nreti r",
            "params i32 i32\na = immd 1\nb = immi 1\nreti b",
        ] {
            assert!(compile(&Trace::parse(text).unwrap(), &layout(), &abi()).is_err());
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
