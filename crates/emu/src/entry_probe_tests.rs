use super::*;

#[test]
fn entry_observation_splits_regions_and_resumes_without_mutating_guest_state() {
    for threshold in [0, 1] {
        install_backend();
        let code = [0xb8, 1, 0, 0, 0, 0x83, 0xc0, 2, 0xcd, 0x82];
        let mut mem = build_mem(91, &code);
        let mut cpu = initial_cpu(&mut Rng::new(91));
        let original = cpu.clone();
        let mut cache = BlockCache::new();
        cache.jit.threshold = threshold;
        cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
        assert!(cache.set_entry_probe(&mut mem, CODE + 5, 7, &code[5..8]));
        assert_eq!(
            cpu.run(&mut mem, &mut cache, 1000),
            Exit::Probe {
                id: 7,
                eip: CODE + 5
            }
        );
        assert_eq!(cpu.r[EAX], 1);
        assert_eq!(&cpu.r[1..], &original.r[1..]);
        assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 3);
        if threshold != 0 {
            assert!(cache.jit.stats.calls > 0);
        }
        cpu.eip = CODE;
        assert_eq!(
            cpu.run(&mut mem, &mut cache, 1000),
            Exit::Probe {
                id: 7,
                eip: CODE + 5
            }
        );
        // A stale code signature must never be interpreted as the verified observation point.
        mem.write_bytes(CODE + 7, &[3]).unwrap();
        cpu.eip = CODE;
        assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Hyper);
        assert_eq!(cpu.r[EAX], 4);
    }
}

#[test]
fn probe_does_not_refire_between_compiled_retry_and_native_progress() {
    install_backend();
    let code = [0x8b, 0x44, 0x24, 0x04, 0xc3, 0xcd, 0x82];
    let mut mem = build_mem(94, &code);
    let mut cpu = initial_cpu(&mut Rng::new(91));
    let sp = cpu.r[ESP];
    mem.w32(sp, CODE + 5).unwrap();
    mem.w32(sp + 4, 99).unwrap();
    let mut cache = BlockCache::new();
    cache.jit.threshold = 100;
    cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    assert!(cache.set_entry_probe(&mut mem, CODE, 9, &code[..5]));
    let trace = crate::lir::Trace::parse("params i32\na = parami 0\nxt a 7\nreti a").unwrap();
    let layout = cache.jit.aot_layout(&mem);
    let artifact = trace
        .compile_region(
            &layout,
            crate::lir::Abi {
                entry: CODE,
                guest_size: GUEST_SIZE as u32,
                trusted_layout: false,
                arguments: vec![crate::lir::Argument::Word(crate::lir::WordSource::Stack(4))],
                checks: vec![],
                bindings: vec![crate::lir::CodeBinding {
                    address: CODE,
                    bytes: code[..5].to_vec(),
                }],
                instruction_cost: 48,
                access_checks: vec![],
                recheck_bindings: true,
                count_completions: false,
            },
        )
        .unwrap();
    cache
        .jit
        .install_precompiled(
            &mut mem,
            &layout,
            &artifact.wasm,
            &[(artifact.region, vec![(CODE, code[..5].to_vec())])],
        )
        .unwrap();
    assert_eq!(
        cpu.run(&mut mem, &mut cache, 1000),
        Exit::Probe { id: 9, eip: CODE }
    );
    assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Hyper);
    assert_eq!(cpu.r[EAX], 99);
    assert_eq!(cache.jit.stats.retries, 1);
}

#[test]
fn overflowing_probe_and_internal_pic_thunk_probe_are_safe() {
    install_backend();
    let mut mem = build_mem(95, &[0x90]);
    let mut cache = BlockCache::new();
    assert!(!cache.set_entry_probe(&mut mem, u32::MAX - 3, 1, &[0; 8]));
    let thunk = CODE + 0x100;
    let relative = thunk - (CODE + 5);
    let mut caller = vec![0xe8];
    caller.extend_from_slice(&relative.to_le_bytes());
    caller.extend_from_slice(&[0xcd, 0x82]);
    mem.write_bytes(CODE, &caller).unwrap();
    mem.write_bytes(thunk, &[0x8b, 0x04, 0x24, 0xc3]).unwrap();
    cache.jit.threshold = 1;
    cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    assert!(cache.set_entry_probe(&mut mem, thunk + 3, 4, &[0xc3]));
    let mut cpu = initial_cpu(&mut Rng::new(91));
    assert_eq!(
        cpu.run(&mut mem, &mut cache, 1000),
        Exit::Probe {
            id: 4,
            eip: thunk + 3
        }
    );
    assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Hyper);
}

#[test]
fn small_scheduler_slices_fall_back_instead_of_livelocking_lir_entry() {
    install_backend();
    let core = DATA + 0x200;
    let mut code:Vec<u8>="558bec83ec18895dfc8b4d088b55108d45f08b1d00000000894df4895df08905000000008b1d000000003bc38b5dfc7308e8000000008b55108b42048b4a082bc18b4df0890d000000008be55dc3".as_bytes().chunks_exact(2).map(|v|u8::from_str_radix(std::str::from_utf8(v).unwrap(),16).unwrap()).collect();
    for (at, value) in [
        (20, core + 48),
        (32, core + 48),
        (38, core + 32),
        (70, core + 48),
    ] {
        code[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mut mem = build_mem(96, &code);
    mem.write_bytes(CODE + 0x100, &[0xcd, 0x82]).unwrap();
    let mut cpu = initial_cpu(&mut Rng::new(91));
    let sp = cpu.r[ESP];
    for (a, v) in [
        (sp, CODE + 0x100),
        (sp + 4, 0),
        (sp + 8, 2),
        (sp + 12, sp + 64),
        (sp + 68, 99),
        (sp + 72, 5),
        (core + 32, sp - 4096),
        (core + 48, 0),
    ] {
        mem.w32(a, v).unwrap();
    }
    let mut cache = BlockCache::new();
    cache.jit.threshold = 100;
    cache.jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    crate::lir::pure::install(
        &mut mem,
        &mut cache.jit,
        CODE,
        core,
        &code,
        "params i32 i32\na = parami 0\nb = parami 1\nr = subi a b\nreti r",
        &[4, 8],
        2,
    )
    .unwrap();
    let mut done = false;
    for _ in 0..32 {
        match cpu.run(&mut mem, &mut cache, 1) {
            Exit::Budget => {}
            Exit::Hyper => {
                done = true;
                break;
            }
            e => panic!("{e:?}"),
        }
    }
    assert!(done);
    assert_eq!(cpu.r[EAX], 94);
    assert_eq!(cpu.jit_lir_completed, 0);
}
