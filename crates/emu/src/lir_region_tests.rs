//! Synthetic native ABI fixtures, NOT verified proprietary Flash captures.
//! Use the real Cpu::run dispatcher and wasmi backend, including S_RETRY.
use super::*;
use crate::lir::{Abi, Argument, CodeBinding, Trace, WordSource};

fn captured_sub_fixture(core: u32) -> Vec<u8> {
    let hex = "558bec83ec18895dfc8b4d088b55108d45f08b1d00000000894df4895df08905000000008b1d000000003bc38b5dfc7308e8000000008b55108b42048b4a082bc18b4df0890d000000008be55dc3";
    let mut bytes: Vec<u8> = hex.as_bytes().chunks_exact(2).map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap()).collect();
    for (at, value) in [(20, core + 48), (32, core + 48), (38, core + 32), (70, core + 48)] { bytes[at..at+4].copy_from_slice(&value.to_le_bytes()); }
    bytes
}

#[test]
fn captured_integer_graphs_execute_and_refuse_fault_sensitive_inputs() {
    let core = DATA + 0x200;
    let cases = [("addi",vec![0x03,0xc1]),("subi",vec![0x2b,0xc1]),("xori",vec![0x33,0xc1]),("andi",vec![0x23,0xc1]),
        ("ori",vec![0x0b,0xc1]),("muli",vec![0x0f,0xaf,0xc1]),("lshi",vec![0xd3,0xe0]),("rshi",vec![0xd3,0xf8]),("rshui",vec![0xd3,0xe8])];
    let mut rng = Rng::new(0x1a33);
    for (operation, instruction) in cases {
        let mut code = captured_sub_fixture(core);
        code.splice(0x3f..0x41, instruction);
        for k in 0..24 {
            let (a,b) = (rng.next() as u32, rng.next() as u32);
            install_backend();
            let mut memory = build_mem(92, &code);
            let mut cpu = initial_cpu(&mut Rng::new(91)); let sp = cpu.r[ESP];
            memory.write_bytes(CODE + 0x100, &[0xcd,0x82]).unwrap();
            for (address,value) in [(sp,CODE+0x100),(sp+4,0x1234),(sp+8,2),(sp+12,sp+64),(sp+68,a),(sp+72,b),(core+32,sp-4096),(core+48,0x12345678)] { memory.w32(address,value).unwrap(); }
            let mut cache = BlockCache::new();cache.jit.threshold=100;cache.jit.layout_override=Some((GUEST,RTAB,WTAB,0));
            let source=format!("params i32 i32\na = parami 0\nb = parami 1\nr = {operation} a b\nreti r");
            crate::lir::pure::install(&mut memory,&mut cache.jit,CODE,core,&code,&source,&[4,8],2).unwrap();
            if k==0 { memory.w32(sp+12,PG_NONE).unwrap(); }
            let mut oracle=build_mem(92,&code);oracle.raw_mut().copy_from_slice(memory.raw());let mut expected=cpu.clone();
            let exit=expected.run(&mut oracle,&mut BlockCache::new(),1000);
            assert_eq!(cpu.run(&mut memory,&mut cache,1000),exit,"{operation} case {k}");
            if k==0 { assert!(cache.jit.stats.retries>0); assert_eq!(cpu.jit_lir_completed,0); continue; }
            for reg in [EAX,ESP,EBX,EBP,ESI,EDI] { assert_eq!(cpu.r[reg],expected.r[reg],"{operation} case {k}"); }
            assert_eq!(memory.r32(core+48).unwrap(),0x12345678);assert_eq!(cpu.jit_lir_completed,1);
        }
    }
}

#[test]
fn captured_flash_sub_leaf_matches_cdecl_results_and_smc() {
    let core = DATA + 0x200;
    let bytes = captured_sub_fixture(core);
    assert!(crate::lir::flash_leaf::verify_native_sub(&bytes, core));
    for a in [0u32, 1, u32::MAX, 0x80000000, 0x7fffffff] {
        for b in [0u32, 1, u32::MAX, 0x80000000, 0x7fffffff] {
            install_backend();
            let mut memory = build_mem(91, &bytes);
            let mut cpu = initial_cpu(&mut Rng::new(91));
            let sp = cpu.r[ESP];
            memory.write_bytes(CODE + 0x100, &[0xcd, 0x82]).unwrap();
            for (address, value) in [(sp, CODE + 0x100), (sp + 4, 0x1234), (sp + 8, 2), (sp + 12, sp + 64), (sp + 68, a), (sp + 72, b), (core + 32, sp - 4096), (core + 48, 0x12345678)] { memory.w32(address, value).unwrap(); }
            let mut oracle = build_mem(91, &bytes); oracle.raw_mut().copy_from_slice(memory.raw());
            let mut expected = cpu.clone();
            let expected_exit = expected.run(&mut oracle, &mut BlockCache::new(), 1000);
            let mut cache = BlockCache::new();
            cache.jit.threshold = 100;
            cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
            crate::lir::flash_leaf::install_sub_leaf(&mut memory, &mut cache.jit, CODE, core, &bytes).unwrap();
            assert_eq!(cpu.run(&mut memory, &mut cache, 1000), expected_exit);
            assert_eq!(cpu.r[EAX], a.wrapping_sub(b));
            for reg in [EAX, ESP, EBX, EBP, ESI, EDI] { assert_eq!(cpu.r[reg], expected.r[reg]); }
            assert_eq!(memory.r32(core + 48).unwrap(), 0x12345678);
            assert!(cache.jit.stats.lir_dispatches > 0);
            assert_eq!(cache.jit.stats.retries, 0);
            assert_eq!(cpu.jit_lir_completed, 1);
            // A patch inside the native method must retire the captured LIR replacement.
            memory.write_bytes(CODE + 0x3f, &[0x03]).unwrap(); // sub eax,ecx -> add eax,ecx
            cpu.eip = CODE; cpu.r[ESP] = sp;
            assert_eq!(cpu.run(&mut memory, &mut cache, 1000), Exit::Hyper);
            assert_eq!(cpu.r[EAX], a.wrapping_add(b));
            assert!(!cache.jit.is_lir_entry(CODE));
        }
    }
}

#[test]
fn scalar_region_native_leaf_and_guard_retry_use_real_dispatcher() {
    // mov eax,[esp+4]; ret; int 0x80. This particular leaf preserves flags
    // and every non-return register, unlike an arbitrary cdecl implementation.
    let code = [0x8b, 0x44, 0x24, 0x04, 0xc3, 0xcd, 0x80];
    for value in [0, 1, u32::MAX, 0x80000000] {
        for guarded in [false, true] {
            install_backend();
            let mut mem = build_mem(37, &code);
            let mut bc = BlockCache::new();
            bc.jit.threshold = 100;
            bc.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
            let layout = bc.jit.aot_layout(&mem);
            let text = if guarded {
                "params i32\na = parami 0\nxt a 9\nreti a"
            } else {
                "params i32\na = parami 0\nreti a"
            };
            let artifact = Trace::parse(text)
                .unwrap()
                .compile_region(
                    &layout,
                    Abi {
                        guest_size: GUEST_SIZE as u32,
                        trusted_layout: false,
                        access_checks: Vec::new(),
                        recheck_bindings: true,
                        count_completions: false,
                        entry: CODE,
                        arguments: vec![Argument::Word(WordSource::Stack(4))],
                        checks: vec![],
                        bindings: vec![CodeBinding {
                            address: CODE,
                            bytes: code[..5].to_vec(),
                        }],
                        instruction_cost: 2,
                    },
                )
                .unwrap();
            let bindings = artifact
                .contract
                .abi
                .bindings
                .iter()
                .map(|b| (b.address, b.bytes.clone()))
                .collect();
            bc.jit
                .install_precompiled(
                    &mut mem,
                    &layout,
                    &artifact.wasm,
                    &[(artifact.region, bindings)],
                )
                .unwrap();
            let mut initial = initial_cpu(&mut Rng::new(37));
            mem.w32(initial.r[ESP], CODE + 5).unwrap();
            mem.w32(initial.r[ESP] + 4, value).unwrap();
            let mut oracle = build_mem(37, &code);
            oracle.raw_mut().copy_from_slice(mem.raw());
            let mut expected = initial.clone();
            let expected_exit = expected.run(&mut oracle, &mut BlockCache::new(), 100);
            let actual_exit = initial.run(&mut mem, &mut bc, 100);
            expected.lf_flush();
            initial.lf_flush();
            assert_eq!(actual_exit, expected_exit);
            assert!(
                diff_cpu(&expected, &initial).is_none(),
                "{:?}",
                diff_cpu(&expected, &initial)
            );
            assert_eq!(mem.raw(), oracle.raw());
            assert!(bc.jit.stats.calls > 0);
            assert_eq!(bc.jit.stats.retries > 0, guarded && value != 0);
        }
    }
}

#[test]
fn a_lir_adapter_that_keeps_refusing_is_retired_and_the_native_method_takes_over() {
    let core = DATA + 0x200;
    let bytes = captured_sub_fixture(core);
    install_backend();
    let mut memory = build_mem(91, &bytes);
    let mut cpu = initial_cpu(&mut Rng::new(91));
    let sp = cpu.r[ESP];
    memory.write_bytes(CODE + 0x100, &[0xcd, 0x82]).unwrap();
    for (address, value) in [(sp, CODE + 0x100), (sp + 4, 0x1234), (sp + 8, 2), (sp + 12, sp + 64), (sp + 68, 40), (sp + 72, 2), (core + 32, sp - 4096), (core + 48, 0x12345678)] { memory.w32(address, value).unwrap(); }
    let mut cache = BlockCache::new();
    cache.jit.threshold = 100;
    cache.jit.lir_retry_limit = 8;
    cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    crate::lir::flash_leaf::install_sub_leaf(&mut memory, &mut cache.jit, CODE, core, &bytes).unwrap();
    assert!(cache.jit.is_lir_entry(CODE));
    // The argv pointer is unusable, so the adapter refuses every call and the native code (here: the interpreter,
    // which resolves the same call identically) runs it.
    memory.w32(sp + 12, PG_NONE).unwrap();
    let mut refusing = 0;
    for call in 0..40 {
        cpu.eip = CODE; cpu.r[ESP] = sp;
        let before = cache.jit.stats.retries;
        let _ = cpu.run(&mut memory, &mut cache, 1000);
        if cache.jit.stats.retries > before { refusing += 1; }
        if !cache.jit.is_lir_entry(CODE) { assert!(call + 1 >= 8, "retired only after the limit, got call {call}"); break; }
    }
    assert!(!cache.jit.is_lir_entry(CODE), "the refusing adapter must be retired");
    assert_eq!(cache.jit.stats.lir_degraded, 1);
    assert_eq!(refusing, 8, "exactly lir_retry_limit refusals are paid");
    // Afterwards the native method keeps producing the right result with valid arguments, and nothing refuses.
    memory.w32(sp + 12, sp + 64).unwrap();
    let retries = cache.jit.stats.retries;
    for _ in 0..300 {
        cpu.eip = CODE; cpu.r[ESP] = sp;
        let _ = cpu.run(&mut memory, &mut cache, 1000);
        assert_eq!(cpu.r[EAX], 38);
    }
    assert_eq!(cache.jit.stats.retries, retries);
    assert!(!cache.jit.is_lir_entry(CODE));
}

#[test]
fn a_refusal_is_blamed_on_the_lir_region_even_when_the_dispatcher_entered_another_region() {
    let core = DATA + 0x200;
    let bytes = captured_sub_fixture(core);
    install_backend();
    let mut memory = build_mem(91, &bytes);
    let mut jit = JitState::new();
    jit.threshold = 100;
    jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    jit.lir_retry_limit = 5;
    crate::lir::flash_leaf::install_sub_leaf(&mut memory, &mut jit, CODE, core, &bytes).unwrap();
    assert!(jit.is_lir_entry(CODE));
    // A caller region the dispatcher entered is unrelated to the refusing entry: it must not be charged.
    for _ in 0..4 { jit.blame_retry(u32::MAX, CODE); }
    assert!(jit.is_lir_entry(CODE), "below the limit nothing is retired");
    jit.blame_retry(u32::MAX, CODE);
    assert!(!jit.is_lir_entry(CODE));
    assert_eq!(jit.stats.lir_degraded, 1);
    // An address that is not a LIR entry falls back to the entered region (unknown here: nothing happens, no panic).
    jit.blame_retry(u32::MAX, CODE + 4);
    assert_eq!(jit.stats.lir_degraded, 1);
}
