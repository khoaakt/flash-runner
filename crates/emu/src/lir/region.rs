//! Explicit, externally verified scalar leaf ABI bridge. This is not a Flash
//! capture decoder or proof that any proprietary method satisfies this contract.
use super::*;
use crate::cpu::{Cpu, EAX, ESP};
use crate::jit_x86::{Layout, RegionOut, S_NEXT, S_RETRY};

/// Location of a raw native i386 word at method entry. Stack offsets are from
/// the original ESP (offset zero is the cdecl return address).
#[derive(Clone, Debug)]
pub enum WordSource {
    Gpr(u8),
    Stack(u32),
    Memory(u32),
    Constant(u32),
}

#[derive(Clone, Debug)]
pub struct AccessCheck { pub base: WordSource, pub offset: i32, pub size: u32, pub write: bool }

/// Representation is inferred from the normalized parameter: raw signed i32
/// bits or IEEE binary64 bits. No Atom tagging/unboxing is implicit.
#[derive(Clone, Debug)]
pub enum Argument {
    Word(WordSource),
    Stack(u32),
    Argv { pointer: WordSource, offset: u32 },
}

/// Exact native bytes supplied by the verified capture, including every code
/// or immutable-data dependency, not just a guessed entry signature.
#[derive(Clone, Debug)]
pub struct CodeBinding {
    pub address: u32,
    pub bytes: Vec<u8>,
}

/// Opt-in assertion by the capture verifier: the original is a bounded pure
/// scalar leaf with these native input locations, raw i32 EAX return, cdecl ret,
/// and no other observable effects beyond cdecl-volatile registers/flags and
/// nonescaping dead stack bytes. This bridge preserves those volatile fields;
/// callees need not have done so. Callee-saved state, guest-visible memory, fault
/// conditions and FP state must be proved by the verifier, not inferred from source.
/// Float intermediates must match wasm IEEE semantics without FP-state effects.
#[derive(Clone, Debug)]
pub struct Abi {
    pub entry: u32,
    pub guest_size: u32,
    /// Set only by the integrated verifier: table/base pointers are stable, trusted
    /// emulator allocations. Guest bounds and protection checks are never skipped.
    pub trusted_layout: bool,
    pub arguments: Vec<Argument>,
    /// Optional exact entry-word checks (e.g. argc or methodenv identity).
    pub checks: Vec<(WordSource, u32)>,
    pub bindings: Vec<CodeBinding>,
    /// Positive upper bound charged atomically on normal completion. No region
    /// chaining is emitted, so recursive return targets still exhaust budget.
    pub instruction_cost: u32,
    pub access_checks: Vec<AccessCheck>,
    /// False only when the registered dispatcher maintains all dependency spans through SMC.
    pub recheck_bindings: bool,
    pub count_completions: bool,
}

#[derive(Clone, Debug)]
pub struct RegionContract {
    pub abi: Abi,
    /// Installer must byte-verify dependencies and register these spans for
    /// invalidation; executable entry/code permissions remain its responsibility.
    pub dependency_spans: Vec<(u32, u32)>,
}

#[derive(Clone, Debug)]
pub struct ScalarRegion {
    pub wasm: Vec<u8>,
    pub region: RegionOut,
    pub contract: RegionContract,
}

fn reject(c: &mut Code, status: u32) {
    c.if_(0x40).u32c(status).op(op::RETURN).end();
}

// Guard native linear-memory ranges using i64, never wrapped wasm32 addition.
fn native_range(c: &mut Code, local: u32, offset: u32, size: u32) {
    c.lget(local)
        .op(op::I64_EXTEND_I32_U)
        .i64c(offset as i64 + size as i64)
        .op(op::I64_ADD)
        .op(0x3f)
        .op(0) // memory.size 0
        .op(op::I64_EXTEND_I32_U)
        .i64c(16)
        .op(op::I64_SHL)
        .op(0x56); // i64.gt_u
    reject(c, S_RETRY);
}

struct Bridge<'a> {
    c: Code,
    layout: &'a Layout,
    addr: u32,
    esp: u32,
    guest_size: u32,
    trusted_layout: bool,
}
impl Bridge<'_> {
    fn cpu(&mut self, offset: u32) {
        self.c.lget(0).mem(op::I32_LOAD, offset);
    }
    /// addr is a checked, nonwrapping guest address. Conservative same-page
    /// reads retry on crossings, even if both pages happen to be readable.
    fn guest_read(&mut self, size: u32) {
        self.guest_check(size, false);
        self.c.lget(self.addr);
        self.c.mem(match size { 1 => op::I32_LOAD8_U, 4 => op::I32_LOAD, _ => op::I64_LOAD }, self.layout.guest_base);
    }
    fn guest_check(&mut self, size: u32, write: bool) {
        self.c.lget(self.addr).u32c(self.guest_size - size).op(op::I32_GT_U);
        reject(&mut self.c, S_RETRY);
        let table = if write { self.layout.wtab } else { self.layout.rtab };
        self.c
            .lget(self.addr)
            .u32c(4095)
            .op(op::I32_AND)
            .u32c(4096 - size)
            .op(op::I32_GT_U);
        reject(&mut self.c, S_RETRY);
        if self.trusted_layout {
            self.c.lget(self.addr).u32c(12).op(op::I32_SHR_U).mem(op::I32_LOAD8_U, table)
                .u32c(1).op(op::I32_AND).op(op::I32_EQZ);
            reject(&mut self.c, S_RETRY);
            return;
        }
        // Standalone/untrusted-layout mode also checks linear table pointer arithmetic.
        self.c
            .lget(self.addr)
            .u32c(12)
            .op(op::I32_SHR_U)
            .op(op::I64_EXTEND_I32_U)
            .i64c(table as i64)
            .op(op::I64_ADD)
            .i64c(u32::MAX as i64)
            .op(0x56);
        reject(&mut self.c, S_RETRY);
        self.c
            .lget(self.addr)
            .u32c(12)
            .op(op::I32_SHR_U)
            .u32c(table)
            .op(op::I32_ADD)
            .lset(1);
        native_range(&mut self.c, 1, 0, 1);
        self.c
            .lget(1)
            .mem(op::I32_LOAD8_U, 0)
            .u32c(1)
            .op(op::I32_AND)
            .op(op::I32_EQZ);
        reject(&mut self.c, S_RETRY);
        self.c
            .lget(self.addr)
            .op(op::I64_EXTEND_I32_U)
            .i64c(self.layout.guest_base as i64 + size as i64)
            .op(op::I64_ADD)
            .i64c(1i64 << 32)
            .op(0x56);
        reject(&mut self.c, S_RETRY);
        native_range(&mut self.c, self.addr, self.layout.guest_base, size);
    }
    fn offset(&mut self, offset: u32) {
        // addr + offset must not wrap guest address space.
        self.c
            .lget(self.addr)
            .u32c(u32::MAX - offset)
            .op(op::I32_GT_U);
        reject(&mut self.c, S_RETRY);
        self.c
            .lget(self.addr)
            .u32c(offset)
            .op(op::I32_ADD)
            .lset(self.addr);
    }
    fn word(&mut self, source: &WordSource) {
        match source {
            WordSource::Constant(value) => { self.c.u32c(*value); }
            WordSource::Memory(address) => { self.c.u32c(*address).lset(self.addr); self.guest_read(4); }
            WordSource::Gpr(reg) => self.cpu(self.layout.cpu_r + *reg as u32 * 4),
            WordSource::Stack(offset) => {
                self.c.lget(self.esp).lset(self.addr);
                self.offset(*offset);
                self.guest_read(4);
            }
        }
    }
}

impl Trace {
    /// Emit only `r0(cpu, head) -> dispatcher status`; the normalized trace is
    /// private and invoked directly inside wasm, with no JavaScript trace hook.
    /// Every refusal returns S_RETRY with the complete original Cpu untouched.
    /// Normal completion changes only EAX, ESP, EIP, and jit_budget.
    pub fn compile_region(&self, layout: &Layout, abi: Abi) -> Result<ScalarRegion, Error> {
        let bad = |msg| error(0, msg);
        if self.result != Type::I32 {
            return Err(bad(
                "native f64/x87 F80 return not implemented; no Atom return conversion",
            ));
        }
        if abi.arguments.len() != self.params.len()
            || abi.guest_size < 4096
            || abi.instruction_cost == 0
            || abi.instruction_cost > i32::MAX as u32
        {
            return Err(bad(
                "parameter mapping or positive bounded instruction cost required",
            ));
        }
        // Never permit guessed Cpu offsets. for_cpu reflects this build's native
        // layout (including alignment); wasm32 callers use their own build.
        let native = Layout::for_cpu(layout.guest_base, layout.rtab, layout.wtab, layout.cache);
        if [layout.cpu_r, layout.cpu_eip, layout.cpu_budget]
            != [native.cpu_r, native.cpu_eip, native.cpu_budget]
        {
            return Err(bad("region requires this build's native Cpu layout"));
        }
        let valid_word = |w: &WordSource| !matches!(w, WordSource::Gpr(r) if *r >= 8);
        for (arg, ty) in abi.arguments.iter().zip(&self.params) {
            match arg {
                Argument::Word(w) if *ty != Type::I32 || !valid_word(w) => {
                    return Err(bad("word argument requires i32 and valid GPR"))
                }
                Argument::Argv { pointer, .. } if !valid_word(pointer) => {
                    return Err(bad("invalid argv pointer GPR"))
                }
                _ => {}
            }
        }
        if abi.checks.iter().any(|(w, _)| !valid_word(w)) {
            return Err(bad("invalid check GPR"));
        }
        if abi.access_checks.len() > 16 || abi.access_checks.iter().any(|a| !valid_word(&a.base) || a.size == 0 || a.size > 4096 || a.size > abi.guest_size) {
            return Err(bad("invalid access preflight"));
        }
        let total: usize = abi.bindings.iter().map(|b| b.bytes.len()).sum();
        if total == 0
            || total > 65536
            || !abi
                .bindings
                .iter()
                .any(|b| b.address == abi.entry && !b.bytes.is_empty())
        {
            return Err(bad(
                "exact entry byte binding and bounded dependencies required",
            ));
        }
        let mut spans = Vec::new();
        for binding in &abi.bindings {
            let end = binding
                .address
                .checked_add(binding.bytes.len() as u32)
                .ok_or_else(|| bad("dependency range overflow"))?;
            if binding.bytes.is_empty() {
                return Err(bad("empty dependency"));
            }
            spans.push((binding.address, end));
        }
        let mut module = ModuleBuilder::new();
        module.import_memory("env", "memory", 0);
        if layout.cache != 0 { module.import_table("env", "table", 0); }
        let trace = self.emit_function(&mut module);
        // Local 2 is i64 result; remaining locals are i32.
        let (esp, addr, ret, status) = (3, 4, 5, 6);
        let mut b = Bridge {
            c: Code::new(),
            layout,
            addr,
            esp,
            guest_size: abi.guest_size,
            trusted_layout: abi.trusted_layout,
        };
        b.c.lget(1).op(op::I32_EQZ).op(op::I32_EQZ);
        reject(&mut b.c, S_RETRY);
        if !abi.trusted_layout { native_range(&mut b.c, 0, 0, core::mem::size_of::<Cpu>() as u32); }
        b.cpu(layout.cpu_eip);
        b.c.u32c(abi.entry).op(op::I32_NE);
        reject(&mut b.c, S_RETRY);
        b.cpu(layout.cpu_budget);
        b.c.u32c(abi.instruction_cost).op(op::I32_LT_S);
        reject(&mut b.c, S_RETRY);
        b.cpu(layout.cpu_r + ESP as u32 * 4);
        b.c.lset(esp);
        // ret must not wrap ESP, even when the return-address load is readable.
        b.c.lget(esp).u32c(u32::MAX - 4).op(op::I32_GT_U);
        reject(&mut b.c, S_RETRY);
        b.c.lget(esp).lset(addr);
        b.guest_read(4);
        b.c.lset(ret);
        for binding in abi.bindings.iter().filter(|_| abi.recheck_bindings) {
            for (i, byte) in binding.bytes.iter().enumerate() {
                b.c.u32c(binding.address + i as u32).lset(addr);
                b.guest_read(1);
                b.c.u32c(*byte as u32).op(op::I32_NE);
                reject(&mut b.c, S_RETRY);
            }
        }
        for (word, expected) in &abi.checks {
            b.word(word);
            b.c.u32c(*expected).op(op::I32_NE);
            reject(&mut b.c, S_RETRY);
        }
        for access in &abi.access_checks {
            b.word(&access.base); b.c.lset(addr);
            if access.offset < 0 {
                let amount = access.offset.unsigned_abs();
                b.c.lget(addr).u32c(amount).op(op::I32_LT_U); reject(&mut b.c, S_RETRY);
                b.c.lget(addr).u32c(amount).op(op::I32_SUB).lset(addr);
            } else { b.offset(access.offset as u32); }
            b.guest_check(access.size, access.write);
        }
        for (argument, ty) in abi.arguments.iter().zip(&self.params) {
            match argument {
                Argument::Word(word) => b.word(word),
                Argument::Stack(offset) => {
                    b.c.lget(esp).lset(addr);
                    b.offset(*offset);
                    b.guest_read(if *ty == Type::I32 { 4 } else { 8 });
                }
                Argument::Argv { pointer, offset } => {
                    b.word(pointer);
                    b.c.lset(addr);
                    b.offset(*offset);
                    b.guest_read(if *ty == Type::I32 { 4 } else { 8 });
                }
            }
            if *ty == Type::F64 {
                b.c.op(op::F64_REINTERPRET_I64);
            }
        }
        b.c.call(trace).lset(2).lset(status);
        b.c.lget(status);
        reject(&mut b.c, S_RETRY);
        // Transaction commit: no potentially failing reads remain.
        b.c.lget(0)
            .lget(2)
            .op(op::I32_WRAP_I64)
            .mem(op::I32_STORE, layout.cpu_r + EAX as u32 * 4);
        b.c.lget(0)
            .lget(esp)
            .u32c(4)
            .op(op::I32_ADD)
            .mem(op::I32_STORE, layout.cpu_r + ESP as u32 * 4);
        b.c.lget(0).lget(ret).mem(op::I32_STORE, layout.cpu_eip);
        b.c.lget(0);
        b.cpu(layout.cpu_budget);
        b.c.u32c(abi.instruction_cost)
            .op(op::I32_SUB)
            .mem(op::I32_STORE, layout.cpu_budget);
        if abi.count_completions {
            let offset = core::mem::offset_of!(Cpu, jit_lir_completed) as u32;
            b.c.lget(0).lget(0).mem(op::I64_LOAD, offset).i64c(1).op(op::I64_ADD).mem(op::I64_STORE, offset);
        }
        if layout.cache != 0 {
            // The result and budget are committed. Return directly to a compiled caller
            // through the existing cache, avoiding one Rust scheduling round-trip per call.
            b.cpu(layout.cpu_budget); b.c.i32c(0).op(op::I32_GT_S).if_(0x40);
            b.c.lget(ret).lget(ret).i32c(12).op(op::I32_SHR_U).op(op::I32_XOR)
                .u32c(crate::jit_x86::CACHE_ENTRIES - 1).op(op::I32_AND).i32c(4).op(op::I32_SHL).lset(addr);
            b.c.lget(addr).mem(op::I32_LOAD, layout.cache).lget(ret).op(op::I32_EQ)
                .lget(addr).mem(op::I32_LOAD, layout.cache + 4).i32c(0).op(op::I32_NE).op(op::I32_AND).if_(0x40);
            let signature = module.ty(&[I32, I32], &[I32]);
            b.c.lget(0).lget(addr).mem(op::I32_LOAD, layout.cache + 8)
                .lget(addr).mem(op::I32_LOAD, layout.cache + 4).i32c(1).op(op::I32_SUB).return_call_indirect(signature, 0);
            b.c.end().end();
        }
        b.c.u32c(S_NEXT);
        let signature = module.ty(&[I32, I32], &[I32]);
        let r0 = module.add_func(signature, 4, 1, 0, b.c.b);
        module.export_func("r0", r0);
        let region = RegionOut {
            heads: vec![(abi.entry, 0)],
            n_ins: abi.instruction_cost,
            chains: vec![(abi.entry, abi.instruction_cost)],
            ..RegionOut::default()
        };
        Ok(ScalarRegion {
            wasm: module.finish(),
            region,
            contract: RegionContract {
                abi,
                dependency_spans: spans,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{Memory, P_R, P_W};
    use wasmi::{Engine, Linker, Module, Store, TypedFunc};

    const CPU: usize = 0x1000;
    const RTAB: u32 = 0x10000;
    const GUEST: u32 = RTAB + (1 << 20);
    const ENTRY: u32 = 0x100;

    fn abi() -> Abi {
        Abi {
            entry: ENTRY,
            guest_size: 0x10000,
            trusted_layout: false,
            access_checks: Vec::new(),
            recheck_bindings: true,
            count_completions: false,
            arguments: vec![Argument::Argv {
                pointer: WordSource::Stack(12),
                offset: 8,
            }],
            checks: vec![(WordSource::Stack(4), 0x1234), (WordSource::Stack(8), 1)],
            bindings: vec![CodeBinding {
                address: ENTRY,
                bytes: vec![0x90, 0xc3],
            }],
            instruction_cost: 7,
        }
    }

    struct Rig {
        store: Store<()>,
        memory: wasmi::Memory,
        function: TypedFunc<(i32, i32), i32>,
        layout: Layout,
    }
    impl Rig {
        fn new() -> Self {
            Self::with("params f64\na = argd 0\nb = d2i a\nxt b 9\nreti b", abi())
        }
        fn with(text: &str, abi: Abi) -> Self {
            let layout = Layout::for_cpu(GUEST, RTAB, 0, 0);
            // Double argv, normalized conversion, guard, raw i32 result.
            let trace = Trace::parse(text).unwrap();
            let artifact = trace.compile_region(&layout, abi).unwrap();
            assert_eq!(artifact.region.heads, vec![(ENTRY, 0)]);
            let engine = Engine::default();
            let module = Module::new(&engine, &artifact.wasm[..]).unwrap();
            let mut store = Store::new(&engine, ());
            let memory = wasmi::Memory::new(
                &mut store,
                wasmi::MemoryType::new((GUEST + 0x10000) / 65536, None),
            )
            .unwrap();
            let mut linker = Linker::new(&engine);
            linker.define("env", "memory", memory).unwrap();
            let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
            assert!(instance.get_func(&store, "trace").is_none());
            let function = instance.get_typed_func(&store, "r0").unwrap();
            let mut mem = Memory::new(0x10000);
            mem.set_prot(0, 0x10000, P_R | P_W);
            mem.raw_mut()[ENTRY as usize..ENTRY as usize + 2].copy_from_slice(&[0x90, 0xc3]);
            mem.w32(0x8000, 0x500).unwrap();
            mem.w32(0x8004, 0x1234).unwrap();
            mem.w32(0x8008, 1).unwrap();
            mem.w32(0x800c, 0x9000).unwrap();
            mem.raw_mut()[0x9008..0x9010].copy_from_slice(&0.5f64.to_le_bytes());
            let bytes = memory.data_mut(&mut store);
            bytes[GUEST as usize..GUEST as usize + mem.size()].copy_from_slice(mem.raw());
            bytes[RTAB as usize..RTAB as usize + (1 << 20)].copy_from_slice(mem.jit_tables().0);
            let mut cpu = Cpu::new();
            cpu.eip = ENTRY;
            cpu.r = [0xabababab, 2, 3, 4, 0x8000, 6, 7, 8];
            cpu.jit_budget = 20;
            cpu.lf_kind = 3;
            cpu.lf_a = 47;
            // Same native Cpu image ABI as the real wasmi runtime backend.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    &cpu as *const Cpu as *const u8,
                    bytes.as_mut_ptr().add(CPU),
                    core::mem::size_of::<Cpu>(),
                );
            }
            Self {
                store,
                memory,
                function,
                layout,
            }
        }
        fn word(&mut self, offset: usize, value: u32) {
            self.memory.data_mut(&mut self.store)[offset..offset + 4]
                .copy_from_slice(&value.to_le_bytes());
        }
        fn cpu_image(&self) -> Vec<u8> {
            self.memory.data(&self.store)[CPU..CPU + core::mem::size_of::<Cpu>()].to_vec()
        }
        fn call(&mut self) -> u32 {
            self.function
                .call(&mut self.store, (CPU as i32, 0))
                .unwrap() as u32
        }
    }

    #[test]
    fn native_argv_marshalling_commit_is_transactional() {
        let mut rig = Rig::new();
        let mut expected = rig.cpu_image();
        let put = |image: &mut Vec<u8>, offset: u32, value: u32| {
            image[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes())
        };
        put(&mut expected, rig.layout.cpu_r, 0);
        put(&mut expected, rig.layout.cpu_r + 16, 0x8004);
        put(&mut expected, rig.layout.cpu_eip, 0x500);
        put(&mut expected, rig.layout.cpu_budget, 13);
        assert_eq!(rig.call(), S_NEXT);
        assert_eq!(rig.cpu_image(), expected); // includes lazy flags, x87, all other fields
    }

    #[test]
    fn native_refusals_preserve_entire_cpu_image() {
        for case in 0..10 {
            let mut rig = Rig::new();
            match case {
                0 => rig.memory.data_mut(&mut rig.store)
                    [GUEST as usize + 0x9008..GUEST as usize + 0x9010]
                    .copy_from_slice(&1.5f64.to_le_bytes()), // taken trace guard
                1 => rig.word(GUEST as usize + 0x8004, 9), // methodenv
                2 => rig.word(GUEST as usize + 0x8008, 2), // argc
                3 => rig.word(GUEST as usize + 0x800c, 0xfffffffcu32), // argv addition wrap
                4 => rig.word(GUEST as usize + 0x800c, 0x9ff4), // crossing f64
                5 => rig.memory.data_mut(&mut rig.store)[RTAB as usize + 9] = 0, // unreadable argv
                6 => rig.memory.data_mut(&mut rig.store)[GUEST as usize + ENTRY as usize] ^= 1, // stale exact code bytes
                7 => rig.word(CPU + rig.layout.cpu_r as usize + 16, u32::MAX - 3), // ret wrap
                8 => rig.word(CPU + rig.layout.cpu_budget as usize, 6),
                _ => rig.word(CPU + rig.layout.cpu_r as usize + 16, 0x8ffe), // crossing ret load
            }
            let before = rig.cpu_image();
            assert_eq!(
                rig.call(),
                S_RETRY,
                "case {case}"
            );
            assert_eq!(rig.cpu_image(), before, "case {case}");
        }
    }

    #[test]
    fn native_unsupported_abi_and_missing_binding_rejected() {
        let layout = Layout::for_cpu(GUEST, RTAB, 0, 0);
        let int = Trace::parse("params\na = immi 1\nreti a").unwrap();
        let double = Trace::parse("params\na = immd 1\nretd a").unwrap();
        assert!(double
            .compile_region(&layout, abi())
            .unwrap_err()
            .message
            .contains("F80"));
        let mut empty = abi();
        empty.arguments.clear();
        empty.bindings.clear();
        assert!(int.compile_region(&layout, empty).is_err());
        let mut wrong = layout.clone();
        wrong.cpu_r += 4;
        assert!(int.compile_region(&wrong, abi()).is_err());
    }

    #[test]
    fn native_mixed_sources_and_budget_bound_return_cycles() {
        let mut abi = abi();
        abi.arguments = vec![
            Argument::Word(WordSource::Gpr(1)),
            Argument::Stack(8),
            Argument::Argv {
                pointer: WordSource::Gpr(2),
                offset: 0,
            },
        ];
        abi.checks = vec![(WordSource::Gpr(3), 4)];
        let mut rig = Rig::with("params i32 i32 i32\na = parami 0\nb = parami 1\nc = parami 2\nd = addi a b\ne = addi d c\nreti e", abi);
        let r = CPU + rig.layout.cpu_r as usize;
        rig.word(r + 8, 0x9000);
        rig.word(GUEST as usize + 0x9000, 10);
        // Return back to the same region, with a second ret slot and input.
        rig.word(GUEST as usize + 0x8000, ENTRY);
        rig.word(GUEST as usize + 0x8004, ENTRY);
        rig.word(GUEST as usize + 0x800c, 1);
        assert_eq!(rig.call(), S_NEXT);
        let eax = u32::from_le_bytes(
            rig.cpu_image()[rig.layout.cpu_r as usize..rig.layout.cpu_r as usize + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(eax, 13); // ECX 2 + stack argc 1 + argv 10
        assert_eq!(rig.call(), S_NEXT);
        let before = rig.cpu_image();
        assert_eq!(rig.call(), S_RETRY); // 20 - 2*7 < 7: native fallback makes progress
        assert_eq!(rig.cpu_image(), before);
    }
}
