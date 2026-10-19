// Included in aot::tests so the fused tests share the exact existing runtime rig.

fn fused_compile(image: &[u8], options: &Options, batch_size: usize) -> (Vec<Artifact>, DiscoveryReport) {
    let mut report = DiscoveryReport::default();
    let artifacts = compile_source_batches_mode(image, options, segments(image, options.load_bias).unwrap(), true, true, batch_size, &mut report, true).unwrap();
    (artifacts, report)
}

#[test]
fn fused_loop_diamond_roundtrip_executes_without_dispatching_internal_edges() {
    use crate::jit::rt::Backend;
    let (mut artifacts, report) = fused_compile(&elf(PROGRAM), &options(), 1024);
    let artifact = artifacts.remove(0);
    assert_eq!((report.compiled_blocks, report.fused_regions), (6, 1));
    assert_eq!((report.compiled_instructions, report.compiled_bytes), (11, PROGRAM.len()));
    assert!(report.internal_static_edges >= 6);
    assert!(artifact.outputs[0].heads.len() > 1);
    assert!(artifact.outputs[0].heads.len() < report.compiled_blocks);
    assert_eq!(artifact.entries, [PC]);
    let encoded = artifact.encode_bundle();
    assert_eq!(encoded[7], 3);
    let decoded = Artifact::decode(&encoded).unwrap();
    assert_eq!(decoded.encode_bundle(), encoded);
    assert_eq!(decoded.manifest_json(), artifact.manifest_json());
    let mut machine = Machine::new(decoded.layout.clone());
    let slots = machine.install(&decoded.wasm, 1).unwrap();
    let mut cpu = Cpu::new(); cpu.eip = PC; cpu.jit_budget = 100;
    // The old six-export compilation needs repeated Rust dispatcher calls.
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_SYSCALL);
    assert_eq!((cpu.r[0], cpu.r[3], cpu.r[1]), (10, 42, 0));
    assert_eq!(cpu.eip, PC + PROGRAM.len() as u32);
    let mut cpu = Cpu::new(); cpu.eip = PC; cpu.jit_budget = 1;
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_NEXT);
    assert!(decoded.outputs[0].heads.iter().any(|&(a, _)| a == cpu.eip));
}

#[test]
fn fused_v3_decoder_rejects_fake_heads_chains_flags_counts_and_guard_bounds() {
    let (mut artifacts, _) = fused_compile(&elf(PROGRAM), &options(), 1024);
    let mut artifact = artifacts.remove(0);
    let original_heads = artifact.outputs[0].heads.clone();
    let original_chains = artifact.outputs[0].chains.clone();
    let mut reject = |heads: Vec<(u32, u32)>, chains: Vec<(u32, u32)>| {
        artifact.outputs[0].heads = heads;
        artifact.outputs[0].chains = chains;
        assert!(Artifact::decode(&artifact.encode_bundle()).is_err());
    };
    let mut bad = original_heads.clone(); bad[1].0 = PC - 1;
    reject(bad, original_chains.clone());
    let mut bad = original_heads.clone(); bad[1].0 = PC + PROGRAM.len() as u32;
    reject(bad, original_chains.clone());
    let mut bad = original_heads.clone(); bad[1] = bad[0];
    reject(bad, original_chains.clone());
    let mut bad = original_heads.clone(); bad[1].1 = original_chains.len() as u32;
    reject(bad, original_chains.clone());
    let mut bad = original_chains.clone(); bad[0].1 += 1;
    reject(original_heads.clone(), bad);
    let mut bad = original_chains.clone(); bad[0].1 = 0;
    reject(original_heads.clone(), bad);
    let mut bad = original_chains.clone(); bad[0].0 += 1;
    reject(original_heads.clone(), bad);
    artifact.outputs[0].heads = original_heads;
    artifact.outputs[0].chains = original_chains;
    artifact.entries = vec![PC + 20]; // Merged block is NOT a published head.
    assert!(Artifact::decode(&artifact.encode_bundle()).is_err());
    artifact.entries = vec![PC];
    artifact.regions[0].instruction_count += 1;
    assert!(Artifact::decode(&artifact.encode_bundle()).is_err());
    artifact.regions[0].instruction_count -= 1;
    artifact.regions[0].file_offset = artifact.input_size as u32;
    assert!(Artifact::decode(&artifact.encode_bundle()).is_err());
    artifact.regions[0].file_offset = 0x100;
    let encoded = artifact.encode_bundle();
    for n in [0, 7, 8, 11, encoded.len() / 2, encoded.len() - 1] {
        assert!(Artifact::decode(&encoded[..n]).is_err());
    }
    let mut bad = encoded.clone(); put32(&mut bad, 8, 2);
    assert!(Artifact::decode(&bad).is_err());
    let mut bad = encoded; bad.push(0);
    assert!(Artifact::decode(&bad).is_err());
}

#[test]
fn fused_v3_does_not_relax_v1_v2_single_block_contracts() {
    let image = elf(PROGRAM);
    let fixed = compile(&image, &options()).unwrap();
    assert_eq!(fixed.encode_bundle()[7], 1);
    assert_eq!(Artifact::decode(&fixed.encode_bundle()).unwrap().encode_bundle(), fixed.encode_bundle());
    let portable = compile_portable(&image, &options()).unwrap();
    assert_eq!(portable.encode_bundle()[7], 2);
    assert_eq!(Artifact::decode(&portable.encode_bundle()).unwrap().encode_bundle(), portable.encode_bundle());
    let (mut artifacts, _) = fused_compile(&image, &options(), 1024);
    let mut fused = artifacts.remove(0);
    fused.bundle_version = 2;
    assert_eq!(fused.encode_bundle()[7], 2);
    assert!(Artifact::decode(&fused.encode_bundle()).is_err());
    fused.bundle_version = 1;
    fused.relocations = None;
    assert_eq!(fused.encode_bundle()[7], 1);
    assert!(Artifact::decode(&fused.encode_bundle()).is_err());
}

#[test]
fn fused_rebinding_rejects_wrapping_guard_tail_even_when_heads_fit() {
    let mut image = elf(&[0xb8, 1, 0, 0, 0, 0xc3]); put16(&mut image, 16, 3);
    let (artifacts, _) = fused_compile(&image, &options(), 1024);
    let bias = u32::MAX - PC - 4;
    assert!(artifacts[0].bind(&options().layout, bias).err().unwrap().contains("overflow"));
}

#[test]
fn fused_unsupported_gap_guards_real_bytes_but_remains_a_dispatcher_frontier() {
    use crate::jit::rt::Backend;
    let code = [0xb8, 1, 0, 0, 0, 0x0f, 0xa2, 0x83, 0xc0, 2, 0xcd, 0x80];
    let (mut artifacts, report) = fused_compile(&elf(&code), &options(), 1024);
    let artifact = Artifact::decode(&artifacts.remove(0).encode_bundle()).unwrap();
    assert_eq!((report.compiled_blocks, report.fused_regions, report.compiled_instructions), (2, 1, 3));
    assert_eq!(report.compiled_bytes, code.len() - 2);
    assert_eq!(report.guarded_bytes, code.len());
    assert_eq!(artifact.regions[0].bytes, code);
    assert_eq!(artifact.regions[0].successors, [PC + 5, PC + code.len() as u32]);
    assert_eq!(artifact.outputs[0].heads, [(PC, 0), (PC + 7, 1)]);
    assert_eq!(artifact.outputs[0].n_ins, 3);
    let mut machine = Machine::new(artifact.layout.clone());
    let slots = machine.install(&artifact.wasm, 1).unwrap();
    let mut cpu = Cpu::new(); cpu.eip = PC; cpu.jit_budget = 100;
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_NEXT);
    assert_eq!((cpu.eip, cpu.r[0]), (PC + 5, 1));
    cpu.eip = PC + 7;
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 1), jit_x86::S_SYSCALL);
    assert_eq!(cpu.r[0], 3);
    let mut mem = memory(&code);
    assert!(artifact.guard(&mem, &artifact.layout));
    mem.w8(PC + 5, 0x90).unwrap();
    assert!(!artifact.guard(&mem, &artifact.layout));
}

#[test]
fn fused_direct_edges_never_enter_hle_or_allowlist_gap_bytes() {
    use crate::jit::rt::Backend;
    let mut code = vec![0xe8, 2, 0, 0, 0, 0xcd, 0x80];
    code.extend(crate::jit_x86::intr::FIND_CODE);
    code.extend([0x43, 0xcd, 0x80]);
    let mut config = options(); config.entries.push(PC + 111);
    let (mut artifacts, report) = fused_compile(&elf(&code), &config, 1024);
    let artifact = artifacts.remove(0);
    assert_eq!(artifact.regions.len(), 1);
    assert!(report.hle_frontiers > 0);
    assert_eq!(artifact.regions[0].successors, [PC + 7, PC + code.len() as u32]);
    assert!(artifact.outputs[0].heads.iter().all(|&(a, _)| !(PC + 7..PC + 111).contains(&a)));
    let mut machine = Machine::new(artifact.layout.clone());
    machine.memory.grow(&mut machine.store, 100).unwrap();
    machine.memory.data_mut(&mut machine.store)[artifact.layout.wtab as usize..artifact.layout.wtab as usize + 16].fill(3);
    let slots = machine.install(&artifact.wasm, 1).unwrap();
    let mut cpu = Cpu::new(); cpu.eip = PC; cpu.r[4] = 0xf000; cpu.jit_budget = 100;
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_NEXT);
    assert_eq!(cpu.eip, PC + 7);
    assert_eq!(cpu.r[4], 0xeffc);
    // A target exactly at an excluded range's start is external, even though
    // another allowed seed beyond it is fused into the same guarded span.
    let code = [0xeb, 2, 0xcc, 0xcc, 0x40, 0x41, 0x43, 0xcd, 0x80];
    let mut config = options(); config.entries.push(PC + 6);
    config.allowlist = vec![(PC, PC + 2), (PC + 6, PC + code.len() as u32)];
    let (mut artifacts, _) = fused_compile(&elf(&code), &config, 1024);
    let artifact = artifacts.remove(0);
    assert_eq!(artifact.regions[0].successors, [PC + 4, PC + code.len() as u32]);
    let mut machine = Machine::new(artifact.layout.clone());
    let slots = machine.install(&artifact.wasm, 1).unwrap();
    let mut cpu = Cpu::new(); cpu.eip = PC; cpu.jit_budget = 100;
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_NEXT);
    assert_eq!(cpu.eip, PC + 4);
    assert_eq!(cpu.r, [0; 8]);
}

#[test]
fn fused_region_bounds_split_nodes_instructions_spans_and_source_mappings() {
    // Explicit seeds provide independently justified blocks, not a text sweep.
    for (code, entries, expected) in [
        (vec![0xc3; FUSION_MAX_NODES + 1], (0..=FUSION_MAX_NODES).map(|i| PC + i as u32).collect(), 2),
        ({ let mut b = vec![0x90; FUSION_MAX_INSTRUCTIONS]; b.push(0xc3); b }, vec![PC], 2),
        ({ let mut b = vec![0xcc; FUSION_MAX_SPAN + 1]; b[0] = 0xc3; b[FUSION_MAX_SPAN] = 0xc3; b }, vec![PC, PC + FUSION_MAX_SPAN as u32], 2),
    ] {
        let mut config = options(); config.entries = entries; config.max_blocks = 2048; config.max_instructions = 4096;
        let (artifacts, report) = fused_compile(&elf(&code), &config, 1024);
        assert_eq!(report.fused_regions, expected);
        for artifact in artifacts {
            assert!(artifact.regions.iter().all(|r| r.bytes.len() <= FUSION_MAX_SPAN && r.instruction_count <= FUSION_MAX_INSTRUCTIONS));
            assert!(Artifact::decode(&artifact.encode_bundle()).is_ok());
        }
    }
    let code = [0xc3; 8]; let image = elf(&code);
    let segs = vec![Segment { start: PC, end: PC + 4, file: 0x100 }, Segment { start: PC + 4, end: PC + 8, file: 0x104 }];
    let mut config = options(); config.entries = vec![PC, PC + 4];
    let mut report = DiscoveryReport::default();
    let artifacts = compile_source_batches_mode(&image, &config, segs, true, true, 1024, &mut report, true).unwrap();
    assert_eq!(report.fused_regions, 2); // Adjacent sections must not be spanned.
    assert_eq!(artifacts[0].regions.iter().map(|r| r.bytes.len()).collect::<Vec<_>>(), [1, 1]);
}

#[test]
fn fused_seed_entries_only_claim_actual_heads_and_batches_use_head_zero() {
    // jmp next; inc eax; int80. The explicit seed at +2 is merged internally.
    let mut config = options(); config.entries = vec![PC, PC + 2];
    let (artifacts, report) = fused_compile(&elf(&[0xeb, 0, 0x40, 0xcd, 0x80]), &config, 1024);
    assert_eq!(report.compiled_blocks, 2);
    assert_eq!(artifacts[0].outputs[0].heads, [(PC, 0)]);
    assert_eq!(artifacts[0].entries, [PC]);
    assert!(Artifact::decode(&artifacts[0].encode_bundle()).is_ok());
    // Every installation-sized batch must identify a real entry, even when its
    // region is only reached through the prior batch's static discovery.
    let mut code = vec![0x90; FUSION_MAX_INSTRUCTIONS + 1]; code.extend([0xcd, 0x80]);
    let mut config = options(); config.max_instructions = 4096;
    let (artifacts, _) = fused_compile(&elf(&code), &config, 1);
    assert_eq!(artifacts.len(), 2);
    for artifact in artifacts {
        assert_eq!(artifact.entries, [artifact.outputs[0].heads[0].0]);
        assert!(Artifact::decode(&artifact.encode_bundle()).is_ok());
    }
}

#[test]
fn fused_registry_rebase_gap_smc_and_indirect_merged_entry_fallback_are_safe() {
    let code = [0xb8, 1, 0, 0, 0, 0x0f, 0xa2, 0x83, 0xc0, 2, 0xcd, 0x80];
    let mut image = elf(&code); put16(&mut image, 16, 3);
    let (mut artifacts, _) = fused_compile(&image, &options(), 1024);
    let portable = artifacts.remove(0);
    for bias in [0x2000, 0x7000] {
        let mut mem = Memory::new(0x10000);
        mem.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W | P_X);
        mem.write_bytes(PC + bias, &code).unwrap();
        let mut cache = crate::run::BlockCache::new(); cache.jit.threshold = 1000;
        let layout = cache.jit.aot_layout(&mem);
        let bound = portable.bind(&layout, bias).unwrap();
        assert_eq!(bound.outputs[0].heads, [(PC + bias, 0), (PC + bias + 7, 1)]);
        assert_eq!(bound.regions[0].successors, [PC + bias + 5, PC + bias + code.len() as u32]);
        assert_eq!(bound.encode_bundle()[7], 3);
        assert!(Artifact::decode(&bound.encode_bundle()).is_ok());
        crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = Some(Box::new(Machine::new(layout))));
        assert_eq!(bound.install(&mut mem, &mut cache.jit).unwrap(), 1);
        assert!(cache.jit.has_entry(PC + bias + 7));
        assert!(!cache.jit.has_entry(PC + bias + 5));
        let mut cpu = Cpu::new(); cpu.eip = PC + bias + 7; cpu.r[0] = 5;
        assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
        assert_eq!(cpu.r[0], 7);
        // A write to an untranslated gap invalidates every head atomically.
        mem.w8(PC + bias + 5, 0x90).unwrap(); cache.sync(&mut mem);
        assert!(!cache.jit.has_entry(PC + bias));
        assert!(!cache.jit.has_entry(PC + bias + 7));
        assert!(bound.install(&mut mem, &mut cache.jit).is_err());
        cache.jit.flush_all();
        crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = None);
    }
    let code = [0xeb, 0, 0x40, 0xcd, 0x80];
    let mut config = options(); config.entries.push(PC + 2);
    let (artifacts, _) = fused_compile(&elf(&code), &config, 1024);
    let mut mem = memory(&code); let mut cache = crate::run::BlockCache::new(); cache.jit.threshold = 1000;
    let layout = cache.jit.aot_layout(&mem);
    let bound = artifacts[0].bind(&layout, 0).unwrap();
    crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = Some(Box::new(Machine::new(layout))));
    bound.install(&mut mem, &mut cache.jit).unwrap();
    assert!(!cache.jit.has_entry(PC + 2));
    let mut cpu = Cpu::new(); cpu.eip = PC + 2; cpu.r[0] = 41;
    assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
    assert_eq!(cpu.r[0], 42); // Interpreter/JIT fallback, not a fabricated head.
    cache.jit.flush_all();
    crate::jit::rt::BACKEND.with(|b| *b.borrow_mut() = None);
}

#[test]
fn fused_cache_kind4_relocation_tailchains_across_regions_at_two_live_layouts() {
    use crate::jit::rt::Backend;
    // One fused multi-chain diamond exits to a second region beyond max span.
    let mut code = PROGRAM[..PROGRAM.len() - 2].to_vec();
    code.push(0xe9);
    code.extend((FUSION_MAX_SPAN as i32 - code.len() as i32 - 4).to_le_bytes());
    code.resize(FUSION_MAX_SPAN, 0xcc);
    code.extend([0x43, 0xcd, 0x80]);
    let mut image = elf(&code); put16(&mut image, 16, 3);
    let mut config = options(); config.layout = Layout::for_cpu(0, 1, 2, 3);
    let (mut artifacts, report) = fused_compile(&image, &config, 1024);
    let portable = artifacts.remove(0);
    assert_eq!(report.fused_regions, 2);
    assert!(portable.outputs[0].heads.len() > 1);
    assert!(portable.relocations.as_ref().unwrap().iter().any(|r| r.kind == 4));
    for (bias, guest, rtab, wtab, cache) in [(0x2000, 0x400000, 0x10000, 0x110000, 0x50000), (0x7000, 0x500000, 0x20000, 0x120000, 0x90000)] {
        let layout = Layout::for_cpu(guest, rtab, wtab, cache);
        let bound = portable.bind(&layout, bias).unwrap();
        assert_eq!(bound.encode_bundle()[7], 3);
        assert!(Artifact::decode(&bound.encode_bundle()).is_ok());
        for relocation in portable.relocations.as_ref().unwrap().iter().filter(|r| r.kind == 4) {
            let at = relocation.offset as usize;
            assert_eq!(bound.wasm[at..at + 5], crate::wasm_emit::fixed_leb(cache + relocation.value - 3, false));
        }
        let mut machine = Machine::new(layout); machine.memory.grow(&mut machine.store, 100).unwrap();
        let slots = machine.install(&bound.wasm, 2).unwrap();
        let target = PC + bias + FUSION_MAX_SPAN as u32;
        let index = ((target ^ (target >> 12)) & (jit_x86::CACHE_ENTRIES - 1)) as usize * 16;
        let data = machine.memory.data_mut(&mut machine.store);
        put32(data, cache as usize + index, target);
        put32(data, cache as usize + index + 4, slots[1] + 1);
        put32(data, cache as usize + index + 8, 0);
        let mut cpu = Cpu::new(); cpu.eip = PC + bias; cpu.jit_budget = 100;
        assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_SYSCALL);
        assert_eq!((cpu.r[0], cpu.r[3]), (10, 43));
        assert_eq!(cpu.eip, target + 3);
        // A missing cache entry must return to the dispatcher without entering
        // a gap or incorrectly calling the second function.
        put32(machine.memory.data_mut(&mut machine.store), cache as usize + index + 4, 0);
        let mut cpu = Cpu::new(); cpu.eip = PC + bias; cpu.jit_budget = 100;
        assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_NEXT);
        assert_eq!((cpu.eip, cpu.r[3]), (target, 42));
    }
}

#[test]
fn fused_local_call_and_return_keep_continuation_head_and_pic_immediates() {
    use crate::jit::rt::Backend;
    // mov eax,[ebx]; add eax,PC; call +2; int80; add eax,1; ret.
    // The numerical collision with PC is an x86 immediate, NOT a relocation.
    let code = [0x8b, 0x03, 0x05, 0, 0x10, 0, 0, 0xe8, 2, 0, 0, 0, 0xcd, 0x80, 0x83, 0xc0, 1, 0xc3];
    let mut image = elf(&code); put16(&mut image, 16, 3);
    let (artifacts, report) = fused_compile(&image, &options(), 1024);
    assert_eq!((report.compiled_blocks, report.fused_regions), (3, 1));
    assert_eq!(artifacts[0].outputs[0].heads, [(PC, 0), (PC + 12, 1), (PC + 14, 2)]);
    for (bias, guest, rtab, wtab) in [(0x2000, 0x40000, 0x20000, 0x30000), (0x7000, 0x80000, 0x60000, 0x70000)] {
        let bound = artifacts[0].bind(&Layout::for_cpu(guest, rtab, wtab, 0), bias).unwrap();
        let mut machine = Machine::new(bound.layout.clone()); machine.memory.grow(&mut machine.store, 32).unwrap();
        let data = machine.memory.data_mut(&mut machine.store);
        data[rtab as usize..rtab as usize + 16].fill(3);
        data[wtab as usize..wtab as usize + 16].fill(3);
        put32(data, guest as usize + 0x2000, 42);
        let slots = machine.install(&bound.wasm, 1).unwrap();
        let mut cpu = Cpu::new(); cpu.eip = PC + bias; cpu.r[3] = 0x2000; cpu.r[4] = 0xf000; cpu.jit_budget = 100;
        assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_SYSCALL);
        assert_eq!((cpu.r[0], cpu.r[4], cpu.eip), (42 + PC + 1, 0xf000, PC + bias + 14));
        assert_eq!(get32(machine.memory.data(&machine.store), guest as usize + 0xeffc), PC + bias + 12);
    }
}
