use super::*;
use crate::cpu::{EAX, ECX, EDX};
use crate::fpu::F80;
use crate::mem::{Memory as GuestMemory, P_R, P_W, P_X};
use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

pub(super) const CPU: usize = 0x1000;
pub(super) const RTAB: u32 = 0x10000;
pub(super) const WTAB: u32 = RTAB + (1 << 20);
pub(super) const GUEST: u32 = WTAB + (1 << 20);
pub(super) const SIZE: u32 = 0x10000;
pub(super) const SP0: u32 = 0x8020;
pub(super) const ARGV0: u32 = 0x9004;
pub(super) const ENTRY: u32 = 0x100;
pub(super) const CORE: u32 = 0x3020;
pub(super) const MASK: u32 = 0x6040;

pub(super) fn hex(s: &str) -> Vec<u8> {
    s.as_bytes().chunks_exact(2).map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap()).collect()
}
// Actual captured .363 no-overflow method, relocated only in absolute core/mask
// addresses. The overflow call is bound but never executed by this adapter.
fn native(kind: u32) -> Vec<u8> {
    let mut bytes = hex("558bec83ec18895dfc8b4d088b45108d55f08b1d50300000894df4895df08915503000008b1d403000003bd38b5dfc7308e8000000008b4510");
    if kind != 0 { bytes[5] = 0x28; }
    match kind {
        0 => bytes.extend(hex("dd4004")),
        1 => bytes.extend(hex("f30f7e4004660f570540600000660fd645e8dd45e8")),
        _ => {
            bytes.extend(hex("f30f7e4804f30f7e400cf20f"));
            bytes.push(match kind { 2 => 0x58, 3 => 0x5c, 4 => 0x59, _ => 0x5e });
            bytes.extend(hex("c8660fd64de8dd45e8"));
        }
    }
    bytes.extend(hex("8b45f08905503000008be55dc3")); bytes
}
fn source(kind: u32) -> (Trace, Vec<u32>) {
    let text = match kind {
        0 => "params f64\na = argd 0\nretd a".into(),
        1 => "params f64\na = argd 0\nr = negd a\nretd r".into(),
        _ => format!("params f64 f64\na = argd 0\nb = argd 1\nr = {} a b\nretd r", ["addd", "subd", "muld", "divd"][(kind - 2) as usize]),
    };
    (Trace::parse(&text).unwrap(), if kind < 2 { vec![4] } else { vec![4, 12] })
}
pub(super) fn mask() -> [u8; 16] { let mut m = [0; 16]; m[7] = 0x80; m }
fn proof(kind: u32) -> Verified {
    let (trace, offsets) = source(kind);
    verify_native(&native(kind), ENTRY, CORE, &trace, &offsets, |a| { assert_eq!(a, MASK); Ok(mask()) }).unwrap()
}
fn abi(kind: u32) -> Abi { Abi { entry: ENTRY, core: CORE, guest_size: SIZE, argc: if kind < 2 { 1 } else { 2 } } }
pub(super) fn layout() -> Layout { Layout::for_cpu(GUEST, RTAB, WTAB, 0) }

// Explicit full-field serialization. Never copy a Rust Cpu/Fpu/F80 object image:
// Rust padding is not initialized data and is not a valid byte-comparison oracle.
pub(super) fn encode(cpu: &Cpu) -> Vec<u8> {
    let mut bytes = vec![0; core::mem::size_of::<Cpu>()];
    macro_rules! put { ($o:expr, $v:expr) => {{ let v = $v.to_le_bytes(); bytes[$o..$o + v.len()].copy_from_slice(&v); }}; }
    macro_rules! field { ($f:ident) => { put!(core::mem::offset_of!(Cpu, $f), cpu.$f); }; }
    for (i, v) in cpu.r.iter().enumerate() { put!(core::mem::offset_of!(Cpu, r) + i * 4, *v); }
    for (i, v) in cpu.seg.iter().enumerate() { put!(core::mem::offset_of!(Cpu, seg) + i * 2, *v); }
    for (i, v) in cpu.tls_base.iter().enumerate() { put!(core::mem::offset_of!(Cpu, tls_base) + i * 4, *v); }
    for (i, v) in cpu.xmm.iter().enumerate() { put!(core::mem::offset_of!(Cpu, xmm) + i * 16, *v); }
    field!(eip); field!(fl); field!(fs_base); field!(gs_base); field!(mxcsr); field!(tsc); field!(cpuid_edx);
    field!(lf_kind); field!(lf_a); field!(lf_b); field!(lf_r); field!(jit_budget); field!(jit_fault);
    field!(jit_fault_addr); field!(jit_fault_write); field!(jit_aux); field!(jit_probe_resume); field!(jit_lir_completed);
    let base = core::mem::offset_of!(Cpu, fpu);
    for (i, v) in cpu.fpu.st.iter().enumerate() {
        let at = base + core::mem::offset_of!(Fpu, st) + i * core::mem::size_of::<F80>();
        put!(at + core::mem::offset_of!(F80, mant), v.mant);
        put!(at + core::mem::offset_of!(F80, se), v.se);
    }
    macro_rules! fp { ($f:ident) => { put!(base + core::mem::offset_of!(Fpu, $f), cpu.fpu.$f); }; }
    fp!(top); fp!(sw); fp!(cw); fp!(tag); fp!(last_ip); fp!(last_dp); fp!(last_op);
    bytes
}
pub(super) fn decode(bytes: &[u8]) -> Cpu {
    let mut cpu = Cpu::new();
    macro_rules! get { ($o:expr, $ty:ty) => { <$ty>::from_le_bytes(bytes[$o..$o + core::mem::size_of::<$ty>()].try_into().unwrap()) }; }
    macro_rules! field { ($f:ident, $ty:ty) => { cpu.$f = get!(core::mem::offset_of!(Cpu, $f), $ty); }; }
    for (i, v) in cpu.r.iter_mut().enumerate() { *v = get!(core::mem::offset_of!(Cpu, r) + i * 4, u32); }
    for (i, v) in cpu.seg.iter_mut().enumerate() { *v = get!(core::mem::offset_of!(Cpu, seg) + i * 2, u16); }
    for (i, v) in cpu.tls_base.iter_mut().enumerate() { *v = get!(core::mem::offset_of!(Cpu, tls_base) + i * 4, u32); }
    for (i, v) in cpu.xmm.iter_mut().enumerate() { *v = get!(core::mem::offset_of!(Cpu, xmm) + i * 16, u128); }
    field!(eip, u32); field!(fl, u32); field!(fs_base, u32); field!(gs_base, u32); field!(mxcsr, u32); field!(tsc, u64); field!(cpuid_edx, u32);
    field!(lf_kind, u32); field!(lf_a, u32); field!(lf_b, u32); field!(lf_r, u32); field!(jit_budget, i32); field!(jit_fault, u32);
    field!(jit_fault_addr, u32); field!(jit_fault_write, u32); field!(jit_aux, u32); field!(jit_probe_resume, u64); field!(jit_lir_completed, u64);
    let base = core::mem::offset_of!(Cpu, fpu);
    for (i, v) in cpu.fpu.st.iter_mut().enumerate() {
        let at = base + core::mem::offset_of!(Fpu, st) + i * core::mem::size_of::<F80>();
        v.mant = get!(at + core::mem::offset_of!(F80, mant), u64);
        v.se = get!(at + core::mem::offset_of!(F80, se), u16);
    }
    macro_rules! fp { ($f:ident, $ty:ty) => { cpu.fpu.$f = get!(base + core::mem::offset_of!(Fpu, $f), $ty); }; }
    fp!(top, u8); fp!(sw, u16); fp!(cw, u16); fp!(tag, u16); fp!(last_ip, u32); fp!(last_dp, u32); fp!(last_op, u16);
    cpu
}
pub(super) fn initial() -> Cpu {
    let mut c = Cpu::new(); c.eip = ENTRY; c.r = [0x11111111, 2, 3, 4, SP0, 6, 7, 8];
    c.jit_budget = 100; c.jit_lir_completed = u32::MAX as u64;
    c.fpu.top = 3; c.fpu.tag = 0xff3f; // old ST0 is physical R3, R2 remains empty
    c.fpu.st[3] = F80 { mant: 0x8000000000000011, se: 0x4001 };
    c.fpu.sw = 0x4700; c.fpu.cw = 0x027f; c.fpu.last_op = 0x123;
    c.fpu.last_ip = 0x22334455; c.fpu.last_dp = 0xaabbccdd;
    c.xmm = [0xabcdef0123456789; 8]; c.mxcsr = 0x1fa0; c // PE already sticky: the inline FP fast configuration
}
pub(super) struct Rig { store: Store<Option<Memory>>, memory: Memory, function: TypedFunc<(i32, i32), i32> }
impl Rig {
    fn new(kind: u32) -> Self { Self::build(&native(kind), &proof(kind), &abi(kind)) }
    /// Real adapter module for `code`/`proof`. It has no host imports: the FP and the x87 push are inline.
    pub(super) fn build(code: &[u8], proof: &Verified, abi: &Abi) -> Self { Self::build_with(code, proof, abi, &layout()) }
    /// With `layout.cache != 0` the module imports the runtime table; slot 0 holds a stub "caller region"
    /// that returns the head it was tail-called with, so a chained exit is visible as that return value.
    pub(super) fn build_with(code: &[u8], proof: &Verified, abi: &Abi, layout: &Layout) -> Self {
        let output = compile(layout, abi, proof).unwrap();
        let engine = Engine::default(); let module = Module::new(&engine, &output.wasm[..]).unwrap();
        let mut store = Store::new(&engine, None);
        let memory_end = if layout.cache == 0 { GUEST + SIZE } else { (layout.cache + CACHE_ENTRIES * 16).max(GUEST + SIZE) };
        let memory = Memory::new(&mut store, MemoryType::new(memory_end.div_ceil(65536), None)).unwrap();
        *store.data_mut() = Some(memory);
        let mut linker = Linker::new(&engine); linker.define("env", "memory", memory).unwrap();
        if layout.cache != 0 {
            let table = wasmi::Table::new(&mut store, wasmi::TableType::new(wasmi::RefType::Func, 1, None), wasmi::Ref::Func(wasmi::Nullable::Null)).unwrap();
            linker.define("env", "table", table).unwrap();
            let mut stub = ModuleBuilder::new(); stub.import_memory("env", "memory", 0);
            let signature = stub.ty(&[I32, I32], &[I32]);
            let mut c = Code::new(); c.lget(1);
            let function = stub.add_func(signature, 0, 0, 0, c.b); stub.export_func("caller", function);
            let stub = Module::new(&engine, &stub.finish()[..]).unwrap();
            let stub = linker.instantiate_and_start(&mut store, &stub).unwrap().get_func(&store, "caller").unwrap();
            table.set(&mut store, 0, wasmi::Ref::Func(wasmi::Nullable::Val(stub))).unwrap();
        }
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let function = instance.get_typed_func(&store, "r0").unwrap();
        let mut rig = Self { store, memory, function };
        for page in 0..16 { rig.permission(RTAB, page, 1); rig.permission(WTAB, page, 1); }
        rig.set_cpu(&initial());
        rig.word(SP0, 0x500); rig.word(SP0 + 4, 0x1234); rig.word(SP0 + 8, abi.argc); rig.word(SP0 + 12, ARGV0);
        rig.word(CORE + 32, SP0 - 4096); rig.word(CORE + 48, 0x12345678);
        rig.guest_mut()[MASK as usize..MASK as usize + 16].copy_from_slice(&mask());
        rig.guest_mut()[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
        rig
    }
    pub(super) fn guest_mut(&mut self) -> &mut [u8] { &mut self.memory.data_mut(&mut self.store)[GUEST as usize..(GUEST + SIZE) as usize] }
    pub(super) fn word(&mut self, address: u32, v: u32) { self.guest_mut()[address as usize..address as usize + 4].copy_from_slice(&v.to_le_bytes()); }
    pub(super) fn double(&mut self, address: u32, v: u64) { self.guest_mut()[address as usize..address as usize + 8].copy_from_slice(&v.to_le_bytes()); }
    pub(super) fn permission(&mut self, table: u32, page: u32, v: u8) { self.memory.data_mut(&mut self.store)[(table + page) as usize] = v; }
    pub(super) fn set_cpu(&mut self, c: &Cpu) { let bytes = encode(c); self.memory.data_mut(&mut self.store)[CPU..CPU + bytes.len()].copy_from_slice(&bytes); }
    pub(super) fn cpu(&self) -> Cpu { decode(&self.memory.data(&self.store)[CPU..CPU + core::mem::size_of::<Cpu>()]) }
    pub(super) fn raw_memory(&mut self) -> &mut [u8] { self.memory.data_mut(&mut self.store) }
    pub(super) fn image(&self) -> Vec<u8> { self.memory.data(&self.store).to_vec() }
    pub(super) fn call(&mut self, head: i32) -> u32 { self.function.call(&mut self.store, (CPU as i32, head)).unwrap() as u32 }
}

#[test]
fn number_native_proof_accepts_real_families_and_rejects_changed_effects() {
    for kind in 0..6 {
        let p = proof(kind); assert_eq!(p.len, native(kind).len());
        assert_eq!(p.frame_offset, -20); assert_eq!(p.stack_low, if kind == 0 { -28 } else { -44 });
        for wrong in 0..6 {
            if wrong == kind { continue; }
            let (trace, offsets) = source(wrong);
            assert!(verify_native(&native(kind), ENTRY, CORE, &trace, &offsets, |_| Ok(mask())).is_err(), "{kind} vs {wrong}");
        }
        for at in [0, 6, 24, 27, 30, 36, 42, 47, native(kind).len() - 1] {
            let mut bytes = native(kind); bytes[at] = 0xcc;
            let (trace, offsets) = source(kind);
            assert!(verify_native(&bytes, ENTRY, CORE, &trace, &offsets, |_| Ok(mask())).is_err(), "{kind} at {at}");
        }
    }
    let (trace, offsets) = source(1);
    assert!(verify_native(&native(1), ENTRY, CORE, &trace, &offsets, |_| Ok([0; 16])).is_err());
    for extra in [hex("8b4018"), hex("8908"), hex("31db"), hex("d9e0"), hex("f20f58c0"), hex("dd400c")] {
        let mut bytes = native(0); bytes.splice(0x3c..0x3c, extra);
        let (trace, offsets) = source(0);
        assert!(verify_native(&bytes, ENTRY, CORE, &trace, &offsets, |_| Ok(mask())).is_err());
    }
}

#[test]
fn number_helper_matches_actual_sse_and_fld_including_payloads_and_all_fp_fields() {
    let mut memory = GuestMemory::new(SIZE as usize); memory.set_prot(0, SIZE, P_R | P_W | P_X);
    let edges = [0, 1 << 63, 1, 0x8000000000000001, 0x0010000000000000, 0x3ff4000000000000,
        0xbff0000000000000, 0x7fefffffffffffff, 0x7ff0000000000000, 0xfff0000000000000,
        0x7ff0000000001234, 0xfff0000000004321, 0x7ff8123456789abc, 0xfff8fedcba987654];
    for kind in 0..6 {
        for &mxcsr in &[0x1f80, 0x3f80, 0x5f80, 0x7f80, 0x9fc0, 0xffff] {
            for &a in &edges { for &b in &edges {
                let mut expected = initial(); expected.mxcsr = mxcsr;
                expected.r[EAX] = ARGV0; expected.r[EBP] = SP0 - 4;
                memory.w64(ARGV0 + 4, a).unwrap(); memory.w64(ARGV0 + 12, b).unwrap(); memory.w128(MASK, mask()).unwrap();
                let code = native(kind); let start = 0x39; let end = match kind { 0 => 0x3c, 1 => 0x4e, _ => 0x4f };
                let mut decoder = Decoder::with_ip(32, &code[start..end], ENTRY as u64 + start as u64, DecoderOptions::NONE);
                let mut ip = 0; let mut dp = 0;
                while decoder.can_decode() {
                    let i = decoder.decode();
                    if i.mnemonic() == M::Fld { ip = i.ip32(); dp = expected.ea(&i).unwrap(); }
                    expected.exec(&mut memory, &i).unwrap();
                }
                let mut actual = initial(); actual.mxcsr = mxcsr;
                assert_eq!(native_return(&mut actual, a, b, kind, ip, dp), 0);
                // SIMD and caller GPR clobbers are permitted by cdecl; everything
                // else, including existing physical x87 slots, is compared.
                expected.r = actual.r; expected.xmm = actual.xmm;
                assert_eq!(encode(&actual), encode(&expected), "kind={kind} mxcsr={mxcsr:x} a={a:x} b={b:x}");
            } }
        }
    }
}

#[test]
fn number_adapter_executes_complete_cdecl_return_and_preserves_caller_state() {
    for kind in 0..6 {
        let p = proof(kind); let mut rig = Rig::new(kind);
        let mut c = initial(); c.lf_kind = 3; c.lf_a = 15; c.lf_b = 4; c.lf_r = 11;
        rig.set_cpu(&c); let a = (-3.5f64).to_bits(); let b = 0.375f64.to_bits();
        rig.double(ARGV0 + 4, a); rig.double(ARGV0 + 12, b);
        let before = rig.guest_mut().to_vec();
        let dp = match p.data_pointer { DataPointer::Argv(o) => ARGV0 + o, DataPointer::Stack(o) => SP0.wrapping_add(o as u32), DataPointer::Field(_) => unreachable!() };
        assert_eq!(native_return(&mut c, a, if kind < 2 { a } else { b }, kind, p.fld_ip, dp), 0);
        c.r[ESP] += 4; c.eip = 0x500; c.jit_budget -= p.instructions as i32; c.jit_lir_completed += 1;
        assert_eq!(rig.call(0), S_NEXT);
        assert_eq!(encode(&rig.cpu()), encode(&c), "kind={kind}");
        assert_eq!(rig.guest_mut(), before);
    }
}

#[test]
fn number_adapter_all_refusals_are_transactional() {
    for kind in [0, 1, 2] { for case in 0..27 {
        let mut rig = Rig::new(kind); let mut c = rig.cpu();
        match case {
            0 => c.eip += 1,
            1 => {},
            2 => c.jit_budget = proof(kind).instructions as i32 - 1,
            3 => c.jit_budget = -1,
            4 => c.r[ESP] = 12,
            5 => c.r[ESP] = u32::MAX - 3,
            6 => c.r[ESP] = 0x8000,
            7 => rig.permission(RTAB, SP0 >> 12, 0),
            8 => rig.permission(WTAB, SP0 >> 12, 0),
            9 => rig.permission(RTAB, CORE >> 12, 0),
            10 => rig.permission(WTAB, CORE >> 12, 0),
            11 => rig.word(CORE + 32, SP0 - 19),
            12 => rig.word(SP0 + 8, abi(kind).argc + 1),
            13 => rig.word(SP0 + 12, SP0),
            14 => rig.word(SP0 + 12, u32::MAX - 4),
            15 => rig.word(SP0 + 12, 0xfffc),
            16 => rig.word(SP0 + 12, 0x9ffc),
            17 => rig.permission(RTAB, ARGV0 >> 12, 0),
            18 => c.fpu.top = 8,
            19 => c.fpu.tag &= !(3 << (2 * 2)),
            20 => c.r[ESP] = CORE + 60,
            21 => rig.permission(RTAB, ARGV0 >> 12, 4),
            22 => rig.permission(WTAB, SP0 >> 12, 2),
            23 if kind == 1 => rig.guest_mut()[MASK as usize + 7] = 0,
            24 if kind == 1 => rig.permission(RTAB, MASK >> 12, 0),
            25 if kind == 1 => c.r[ESP] = MASK + 32,
            26 if kind == 1 => rig.guest_mut()[MASK as usize + 15] ^= 1,
            _ => c.jit_budget = 0,
        }
        rig.set_cpu(&c); let before = rig.image();
        assert_eq!(rig.call(if case == 1 { 1 } else { 0 }), S_RETRY, "kind={kind} case={case}");
        assert_eq!(rig.image(), before, "kind={kind} case={case}");
    } }
}

#[test]
fn number_helper_refusal_and_stack_wrap_preserve_all_fields() {
    for kind in [6, u32::MAX] {
        let mut c = initial(); let before = encode(&c);
        assert_eq!(native_return(&mut c, 1, 2, kind, 3, 4), S_RETRY); assert_eq!(encode(&c), before);
    }
    for top in 0..8 {
        let mut c = initial(); c.fpu.top = top; c.fpu.tag = 0xffff;
        assert_eq!(native_return(&mut c, 0x8000000000000000, 0, 0, 10, 20), 0);
        assert_eq!(c.fpu.top, top.wrapping_sub(1) & 7);
        assert_eq!(c.fpu.st[c.fpu.top as usize], F80 { mant: 0, se: 0x8000 });
        assert_eq!((c.fpu.tag >> (2 * c.fpu.top)) & 3, 1);
    }
}

#[test]
fn number_complete_native_path_matches_adapter_except_abi_volatile_and_dead_spills() {
    for kind in 0..6 {
        let mut rig = Rig::new(kind); let a = 1.25f64.to_bits(); let b = 2.5f64.to_bits();
        rig.double(ARGV0 + 4, a); rig.double(ARGV0 + 12, b);
        let mut memory = GuestMemory::new(SIZE as usize); memory.set_prot(0, SIZE, P_R | P_W | P_X);
        memory.raw_mut().copy_from_slice(rig.guest_mut());
        let mut expected = initial(); let before = encode(&expected); let mut count = 0;
        while expected.eip != 0x500 {
            let ip = expected.eip as usize;
            let mut decoder = Decoder::with_ip(32, &memory.raw()[ip..], ip as u64, DecoderOptions::NONE);
            let i = decoder.decode(); expected.eip = i.next_ip32(); expected.exec(&mut memory, &i).unwrap();
            count += 1; assert!(count < 128);
        }
        assert_eq!(count, proof(kind).instructions);
        assert_eq!(rig.call(0), S_NEXT); let actual = rig.cpu();
        for r in [EAX, ECX, EDX] { expected.r[r] = actual.r[r]; }
        expected.xmm = actual.xmm; expected.fl = actual.fl;
        expected.jit_budget = actual.jit_budget; expected.jit_lir_completed = actual.jit_lir_completed;
        assert_eq!(encode(&expected), encode(&actual), "kind={kind}");
        assert_ne!(before, encode(&actual));
        let original = rig.guest_mut();
        let low = (SP0 as i32 + proof(kind).stack_low) as usize;
        assert_eq!(&memory.raw()[..low], &original[..low]);
        assert_eq!(&memory.raw()[SP0 as usize..], &original[SP0 as usize..]);
    }
}

#[test]
fn number_argc_must_equal_the_proven_argv_read_set() {
    for kind in 0..6 {
        let p = proof(kind); let wanted = abi(kind).argc;
        for argc in 0..=9 {
            let result = compile(&layout(), &Abi { argc, ..abi(kind) }, &p);
            assert_eq!(result.is_ok(), argc == wanted, "kind={kind} argc={argc}");
        }
    }
}

#[test]
fn number_adapter_is_exact_or_refuses_for_every_edge_value_and_mxcsr() {
    let edges = [0, 1 << 63, 1, 0x8000000000000001, 0x0010000000000000, 0x3ff4000000000000,
        0xbff0000000000000, 0x7fefffffffffffff, 0x7ff0000000000000, 0xfff0000000000000,
        0x7ff0000000001234, 0xfff0000000004321, 0x7ff8123456789abc, 0xfff8fedcba987654,
        0x4004000000000000, 0xc002000000000000, 0x3fb999999999999a, 0x4330000000000001];
    let (mut done, mut refused) = (0, 0);
    for kind in 0..6 {
        let p = proof(kind); let dp = match p.data_pointer { DataPointer::Argv(o) => ARGV0 + o, DataPointer::Stack(o) => SP0.wrapping_add(o as u32), DataPointer::Field(_) => unreachable!() };
        for &mxcsr in &[0x1f80, 0x1fa0, 0x3fa0, 0x5fa0, 0x7fa0, 0x9fe0, 0xffff] {
            for &a in &edges { for &b in &edges {
                let mut rig = Rig::new(kind); let mut c = initial(); c.mxcsr = mxcsr; rig.set_cpu(&c);
                rig.double(ARGV0 + 4, a); rig.double(ARGV0 + 12, b);
                let before = rig.image();
                match rig.call(0) {
                    S_RETRY => { refused += 1; assert_eq!(rig.image(), before, "kind={kind} mxcsr={mxcsr:x} a={a:x} b={b:x}"); }
                    S_NEXT => {
                        done += 1;
                        let mut expected = c.clone();
                        assert_eq!(native_return(&mut expected, a, if kind < 2 { a } else { b }, kind, p.fld_ip, dp), 0);
                        expected.r[ESP] += 4; expected.eip = 0x500; expected.jit_budget -= p.instructions as i32; expected.jit_lir_completed += 1;
                        assert_eq!(encode(&rig.cpu()), encode(&expected), "kind={kind} mxcsr={mxcsr:x} a={a:x} b={b:x}");
                    }
                    other => panic!("unexpected status {other}"),
                }
            } }
        }
    }
    // Ordinary operands under the fast configuration must really take the inline path.
    assert!(done > 500 && refused > 1000, "done={done} refused={refused}");
}
