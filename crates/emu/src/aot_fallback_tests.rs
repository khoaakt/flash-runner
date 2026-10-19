// Included inside aot::tests::fallback_tests. This rig invokes the real generic
// interpreter helper, not a floating-point approximation or a runtime translator.
use crate::jit::{self, rt::Backend, JitState};
use crate::run::BlockCache;
use std::mem::offset_of;

fn portable(code: &[u8]) -> Artifact {
    let mut image = elf(code);
    put16(&mut image, 16, 3);
    let mut config = options();
    config.layout = Layout::for_cpu(0, 1, 2, 0);
    compile_portable_complete(&image, &config).unwrap()
}

fn region_inputs(artifact: &Artifact) -> Vec<(RegionOut, Vec<(u32, Vec<u8>)>)> {
    artifact.outputs.iter().cloned().zip(&artifact.regions)
        .map(|(out, r)| (out, vec![(r.address, r.bytes.clone())])).collect()
}

// Only defined fields cross the native/wasmi boundary: no copying uninitialized
// Rust padding or interpreting foreign bytes as an entire Cpu/Fpu object.
fn write_cpu(bytes: &mut [u8], layout: &Layout, cpu: &Cpu) {
    for (i, &value) in cpu.r.iter().enumerate() { put32(bytes, CPU + layout.cpu_r as usize + i * 4, value); }
    for (offset, value) in [
        (layout.cpu_eip, cpu.eip), (layout.cpu_fl, cpu.fl),
        (layout.cpu_fs_base, cpu.fs_base), (layout.cpu_gs_base, cpu.gs_base),
        (layout.cpu_lf_kind, cpu.lf_kind), (layout.cpu_lf_a, cpu.lf_a),
        (layout.cpu_lf_b, cpu.lf_b), (layout.cpu_lf_r, cpu.lf_r),
        (layout.cpu_budget, cpu.jit_budget as u32), (layout.cpu_mxcsr, cpu.mxcsr),
        (layout.cpu_fault, cpu.jit_fault), (layout.cpu_aux, cpu.jit_aux),
        (layout.cpu_fpu_last_ip, cpu.fpu.last_ip), (layout.cpu_fpu_last_dp, cpu.fpu.last_dp),
    ] { put32(bytes, CPU + offset as usize, value); }
    for (i, xmm) in cpu.xmm.iter().enumerate() {
        let at = CPU + layout.cpu_xmm as usize + i * 16;
        bytes[at..at + 16].copy_from_slice(&xmm.to_le_bytes());
    }
    for (i, value) in cpu.fpu.st.iter().enumerate() {
        let at = CPU + layout.cpu_mm0 as usize + i * layout.mm_stride as usize;
        bytes[at..at + 8].copy_from_slice(&value.mant.to_le_bytes());
        put16(bytes, CPU + layout.cpu_mm_se0 as usize + i * layout.mm_stride as usize, value.se);
    }
    bytes[CPU + layout.cpu_fpu_top as usize] = cpu.fpu.top;
    for (offset, value) in [(layout.cpu_fpu_cw, cpu.fpu.cw), (layout.cpu_fpu_sw, cpu.fpu.sw), (layout.cpu_fpu_tag, cpu.fpu.tag)] {
        put16(bytes, CPU + offset as usize, value);
    }
    // These are only touched by the Rust helper; preserve them as defined fields.
    put16(bytes, CPU + offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, last_op), cpu.fpu.last_op);
}

fn read_cpu(bytes: &[u8], layout: &Layout, cpu: &mut Cpu) {
    for (i, value) in cpu.r.iter_mut().enumerate() { *value = get32(bytes, CPU + layout.cpu_r as usize + i * 4); }
    cpu.eip = get32(bytes, CPU + layout.cpu_eip as usize);
    cpu.fl = get32(bytes, CPU + layout.cpu_fl as usize);
    cpu.fs_base = get32(bytes, CPU + layout.cpu_fs_base as usize);
    cpu.gs_base = get32(bytes, CPU + layout.cpu_gs_base as usize);
    cpu.lf_kind = get32(bytes, CPU + layout.cpu_lf_kind as usize);
    cpu.lf_a = get32(bytes, CPU + layout.cpu_lf_a as usize);
    cpu.lf_b = get32(bytes, CPU + layout.cpu_lf_b as usize);
    cpu.lf_r = get32(bytes, CPU + layout.cpu_lf_r as usize);
    cpu.jit_budget = get32(bytes, CPU + layout.cpu_budget as usize) as i32;
    cpu.mxcsr = get32(bytes, CPU + layout.cpu_mxcsr as usize);
    cpu.jit_fault = get32(bytes, CPU + layout.cpu_fault as usize);
    cpu.jit_aux = get32(bytes, CPU + layout.cpu_aux as usize);
    cpu.fpu.last_ip = get32(bytes, CPU + layout.cpu_fpu_last_ip as usize);
    cpu.fpu.last_dp = get32(bytes, CPU + layout.cpu_fpu_last_dp as usize);
    for (i, xmm) in cpu.xmm.iter_mut().enumerate() {
        let at = CPU + layout.cpu_xmm as usize + i * 16;
        *xmm = u128::from_le_bytes(bytes[at..at + 16].try_into().unwrap());
    }
    for (i, value) in cpu.fpu.st.iter_mut().enumerate() {
        let at = CPU + layout.cpu_mm0 as usize + i * layout.mm_stride as usize;
        value.mant = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let at = CPU + layout.cpu_mm_se0 as usize + i * layout.mm_stride as usize;
        value.se = u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    }
    cpu.fpu.top = bytes[CPU + layout.cpu_fpu_top as usize];
    let word = |offset: u32| u16::from_le_bytes(bytes[CPU + offset as usize..CPU + offset as usize + 2].try_into().unwrap());
    cpu.fpu.cw = word(layout.cpu_fpu_cw);
    cpu.fpu.sw = word(layout.cpu_fpu_sw);
    cpu.fpu.tag = word(layout.cpu_fpu_tag);
    let at = CPU + offset_of!(Cpu, fpu) + offset_of!(crate::fpu::Fpu, last_op);
    cpu.fpu.last_op = u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
}

struct CompleteBackend {
    machine: Machine,
    cpu: std::sync::Arc<std::sync::Mutex<Cpu>>,
}

impl CompleteBackend {
    fn new(layout: Layout) -> Self {
        let mut machine = Machine::new(layout.clone());
        machine.memory.grow(&mut machine.store, 128).unwrap();
        machine.linker.allow_shadowing(true);
        let memory = machine.memory;
        let cpu = std::sync::Arc::new(std::sync::Mutex::new(Cpu::new()));
        let host_cpu = cpu.clone();
        machine.linker.func_wrap("env", "exec_one", move |mut caller: Caller<'_, ()>, cpu_at: i32, tag: i32| -> i32 {
            assert_eq!(cpu_at as usize, CPU);
            let mut cpu = host_cpu.lock().unwrap();
            read_cpu(memory.data(&caller), &layout, &mut cpu);
            // Cpu::run owns these objects throughout Backend::call. The helper
            // receives no borrow of the backend itself or of wasmi's memory.
            let (mem, jit) = unsafe { jit::ctx::get() };
            let (mem, jit) = unsafe { (&mut *mem, &mut *jit) };
            let guest = layout.guest_base as usize;
            let size = mem.size();
            mem.raw_mut().copy_from_slice(&memory.data(&caller)[guest..guest + size]);
            let ins = *jit.fb_ins(tag as u32);
            jit.stats.exec_calls += 1;
            let result = jit::h_exec_one(&mut cpu, mem, &ins, &mut jit.pending);
            let bytes = memory.data_mut(&mut caller);
            write_cpu(bytes, &layout, &cpu);
            bytes[guest..guest + mem.size()].copy_from_slice(mem.raw());
            result as i32
        }).unwrap();
        Self { machine, cpu }
    }
}

impl Backend for CompleteBackend {
    fn install(&mut self, wasm: &[u8], n: usize) -> Option<Vec<u32>> { self.machine.install(wasm, n) }
    fn free(&mut self, slot: u32) { self.machine.free(slot); }
    fn call(&mut self, slot: u32, cpu: &mut Cpu, head: u32) -> u32 {
        let (mem, _) = unsafe { jit::ctx::get() };
        let mem = unsafe { &mut *mem };
        let layout = &self.machine.layout;
        let guest = layout.guest_base as usize;
        let bytes = self.machine.memory.data_mut(&mut self.machine.store);
        write_cpu(bytes, layout, cpu);
        *self.cpu.lock().unwrap() = cpu.clone();
        bytes[guest..guest + mem.size()].copy_from_slice(mem.raw());
        let (rtab, wtab) = mem.jit_tables();
        let pages = mem.size() >> 12;
        bytes[layout.rtab as usize..layout.rtab as usize + pages].copy_from_slice(&rtab[..pages]);
        bytes[layout.wtab as usize..layout.wtab as usize + pages].copy_from_slice(&wtab[..pages]);
        let status = self.machine.funcs[slot as usize].as_ref().unwrap()
            .call(&mut self.machine.store, (CPU as i32, head as i32)).unwrap();
        let bytes = self.machine.memory.data(&self.machine.store);
        read_cpu(bytes, layout, &mut self.cpu.lock().unwrap());
        *cpu = self.cpu.lock().unwrap().clone();
        let size = mem.size();
        mem.raw_mut().copy_from_slice(&bytes[guest..guest + size]);
        status as u32
    }
}

fn setup(code: &[u8], bias: u32, variant: bool) -> (Memory, BlockCache) {
    let mut mem = Memory::new(0x20000);
    mem.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W | P_X);
    mem.write_bytes(PC + bias, code).unwrap();
    mem.set_prot(0x10000, 0x2000, P_MAPPED | P_R | P_W);
    let mut cache = BlockCache::new();
    cache.jit.threshold = 1000;
    cache.jit.layout_override = Some(if variant { (0x80000, 0x60000, 0x70000, 0) } else { (0x40000, 0x20000, 0x30000, 0) });
    let layout = cache.jit.aot_layout(&mem);
    jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = Some(Box::new(CompleteBackend::new(layout))));
    (mem, cache)
}

fn install(artifact: &Artifact, bias: u32, mem: &mut Memory, jit: &mut JitState, blocked: &[u32]) -> Result<usize, String> {
    let instructions = artifact.decode_fallback_instructions(bias)?;
    let tags = jit.allocate_offline_tags(&instructions);
    let result = (|| {
        let bound = artifact.bind_with_tags(&jit.aot_layout(mem), bias, &tags)?;
        jit.install_precompiled_fallbacks(mem, &bound.layout, &bound.wasm, &region_inputs(&bound), blocked, &tags)
    })();
    if !matches!(result, Ok(n) if n != 0) { jit.release_offline_tags(&tags); }
    result
}

fn same_cpu(actual: &mut Cpu, expected: &mut Cpu) {
    actual.lf_flush(); expected.lf_flush();
    assert_eq!(actual.r, expected.r);
    assert_eq!(actual.eip, expected.eip);
    assert_eq!(actual.fl, expected.fl);
    assert_eq!(actual.xmm, expected.xmm);
    assert_eq!(actual.mxcsr, expected.mxcsr);
    assert_eq!(actual.fpu.st, expected.fpu.st);
    assert_eq!((actual.fpu.top, actual.fpu.tag, actual.fpu.sw, actual.fpu.cw, actual.fpu.last_ip, actual.fpu.last_dp, actual.fpu.last_op),
        (expected.fpu.top, expected.fpu.tag, expected.fpu.sw, expected.fpu.cw, expected.fpu.last_ip, expected.fpu.last_dp, expected.fpu.last_op));
}

#[test]
fn v4_records_exact_bytes_and_only_semantic_exec_one_tag_sites() {
    // Numerical constants collide with local tag 0/1 and the guest PC. None is a
    // relocation unless the assembler explicitly annotated its semantic operand.
    let code = [0xb8, 0, 0, 0, 0, 0x0f, 0xa2, 0xbb, 1, 0, 0, 0, 0xf2, 0x0f, 0x58, 0xc1, 0xcd, 0x80];
    let artifact = portable(&code);
    assert_eq!(artifact.encode_bundle()[7], 4);
    assert_eq!(artifact.fallback, [Fallback { ip: PC + 5, bytes: vec![0x0f, 0xa2] }, Fallback { ip: PC + 12, bytes: vec![0xf2, 0x0f, 0x58, 0xc1] }]);
    assert_eq!(artifact.outputs[0].n_fallback, 2);
    let relocs = artifact.relocations.as_ref().unwrap();
    assert_eq!(relocs.iter().filter(|r| r.kind == 5).map(|r| r.value).collect::<Vec<_>>(), [0, 1]);
    let encoded = artifact.encode_bundle();
    let decoded = Artifact::decode(&encoded).unwrap();
    assert_eq!(decoded.encode_bundle(), encoded);
    assert_eq!(decoded.manifest_json(), artifact.manifest_json());
    let tags = [0x8000_0001, u32::MAX];
    let bound = decoded.bind_with_tags(&options().layout, 0x7000, &tags).unwrap();
    assert!(bound.relocations.is_none());
    for relocation in relocs.iter().filter(|r| r.kind == 5) {
        let at = relocation.offset as usize;
        assert_eq!(bound.wasm[at..at + 5], crate::wasm_emit::fixed_leb(tags[relocation.value as usize], true));
    }
    Module::new(&Engine::default(), &bound.wasm).unwrap();
    assert_eq!(bound.fallback[0].ip, PC + 0x7000 + 5);
    assert!(Artifact::decode(&bound.encode_bundle()).is_ok());
    let address_bound = decoded.bind(&options().layout, 0x2000).unwrap();
    assert!(address_bound.relocations.is_some());
    assert_eq!(address_bound.bind_with_tags(&options().layout, 0x7000, &tags).unwrap().wasm, bound.wasm);
    assert!(decoded.bind_with_tags(&options().layout, 0, &[7]).is_err());
    assert!(decoded.bind_with_tags(&options().layout, 0, &[7, 7]).is_err());
    assert!(decoded.decode_fallback_instructions(u32::MAX).is_err());
}

#[test]
fn freshly_decoded_binding_matches_revalidated_binding_without_copying_wasm() {
    let artifact = portable(&[0x0f, 0xa2, 0xf2, 0x0f, 0x58, 0xc1, 0xcd, 0x80]);
    let bundle = artifact.encode_bundle();
    for bias in [0x2000, 0x7000] {
        let layout = options().layout;
        let tags = [73, 91];
        let reference = artifact.bind_with_tags(&layout, bias, &tags).unwrap();
        let decoded = DecodedArtifact::decode(&bundle).unwrap();
        assert_eq!(decoded.layout(), &artifact.layout);
        assert_eq!(decoded.decode_fallback_instructions(bias).unwrap()[0].ip32(), PC + bias);
        let allocation = decoded.0.wasm.as_ptr();
        let linked = decoded.bind_with_tags(&layout, bias, &tags).unwrap();
        assert_eq!(linked.wasm.as_ptr(), allocation, "validated owner must link in the original decoded allocation");
        assert_eq!(linked.encode_bundle(), reference.encode_bundle());
        Module::new(&Engine::default(), &linked.wasm).unwrap();
    }
    assert!(DecodedArtifact::decode(&bundle[..bundle.len() - 1]).is_err());
    assert!(DecodedArtifact::decode(&bundle).unwrap().bind_with_tags(&options().layout, 0, &[73]).is_err());
    let mut modified = Artifact::decode(&bundle).unwrap();
    modified.outputs[0].n_fallback = 0;
    assert!(modified.bind_with_tags(&options().layout, 0, &[73,91]).is_err(), "public mutable Artifact binding still validates metadata");
}

#[test]
fn v4_rejects_missing_duplicate_corrupt_unguarded_records_and_tag_relocations() {
    let original = portable(&[0x0f, 0xa2, 0xf2, 0x0f, 0x58, 0xc1, 0xcd, 0x80]).encode_bundle();
    let reject = |edit: &dyn Fn(&mut Artifact)| {
        let mut artifact = Artifact::decode(&original).unwrap();
        edit(&mut artifact);
        assert!(Artifact::decode(&artifact.encode_bundle()).is_err());
    };
    reject(&|a| { a.fallback.pop(); });
    reject(&|a| { a.fallback[1] = a.fallback[0].clone(); });
    reject(&|a| { a.fallback[0].bytes[0] ^= 1; });
    reject(&|a| { a.fallback[0].bytes.clear(); });
    reject(&|a| { a.fallback[0].bytes = vec![0x90; 16]; });
    reject(&|a| { a.fallback[0].ip -= 1; });
    reject(&|a| { a.outputs[0].n_fallback -= 1; });
    reject(&|a| { a.relocations.as_mut().unwrap().retain(|r| r.kind != 5 || r.value != 0); });
    reject(&|a| {
        let relocation = a.relocations.as_mut().unwrap().iter_mut().find(|r| r.kind == 5).unwrap();
        relocation.value = 2;
        let at = relocation.offset as usize;
        a.wasm[at..at + 5].copy_from_slice(&crate::wasm_emit::fixed_leb(2, true));
    });
    reject(&|a| {
        let relocation = a.relocations.as_mut().unwrap().iter_mut().find(|r| r.kind == 5 && r.value == 1).unwrap();
        relocation.value = 0;
        let at = relocation.offset as usize;
        a.wasm[at..at + 5].copy_from_slice(&crate::wasm_emit::fixed_leb(0, true));
    });
    reject(&|a| {
        let at = a.relocations.as_ref().unwrap().iter().find(|r| r.kind == 5).unwrap().offset as usize;
        a.wasm[at + 6] = 1; // not exec_one, despite a valid i32.const
    });
    reject(&|a| { a.bundle_version = 3; });
    // Guarded but not exactly one valid instruction is still rejected.
    reject(&|a| { a.regions[0].bytes[1] = 0xff; a.fallback[0].bytes[1] = 0xff; });
}

#[test]
fn v4_full_ieee_fallback_runs_offline_module_at_two_layouts_and_bases() {
    // add eax,1; addsd xmm0,xmm1; mulsd xmm0,xmm2; cvttsd2si eax,xmm0;
    // fld1; fldpi; faddp; fstp qword [edi]; inc ebx; int 0x80.
    let code = [0x83, 0xc0, 1, 0xf2, 0x0f, 0x58, 0xc1, 0xf2, 0x0f, 0x59, 0xc2,
        0xf2, 0x0f, 0x2c, 0xc0, 0xd9, 0xe8, 0xd9, 0xeb, 0xde, 0xc1, 0xdd, 0x1f, 0x43, 0xcd, 0x80];
    let artifact = Artifact::decode(&portable(&code).encode_bundle()).unwrap();
    assert_eq!(artifact.fallback.len(), 7);
    let classes = [0u64, 1 << 63, 1, 0x000f_ffff_ffff_ffff, 0x0010_0000_0000_0000,
        1.0f64.to_bits(), 0x7fef_ffff_ffff_ffff, 0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000, 0x7ff8_1234_5678_9abc, 0x7ff0_0000_0000_0001];
    for (bias, variant) in [(0x2000, false), (0x7000, true)] {
        let (mut mem, mut cache) = setup(&code, bias, variant);
        assert_eq!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).unwrap(), 1);
        for (i, &bits) in classes.iter().enumerate() {
            for rounding in 0..4 {
                let mut cpu = Cpu::new(); cpu.eip = PC + bias;
                cpu.xmm[0] = 0xdead_beef_cafe_babe_0000_0000_0000_0000 | bits as u128;
                cpu.xmm[1] = classes[(i + 3) % classes.len()] as u128;
                cpu.xmm[2] = classes[(i + 5) % classes.len()] as u128;
                cpu.mxcsr = 0x1f80 | rounding << 13 | if i % 2 == 0 { 0x8040 } else { 0 };
                cpu.fpu.cw = 0x037f | (rounding as u16) << 10;
                cpu.r[0] = 0xffff_ffff; cpu.r[3] = 17; cpu.r[7] = 0x10000;
                let mut expected = cpu.clone();
                let mut oracle = Memory::new(mem.size());
                oracle.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W | P_X);
                oracle.write_bytes(PC + bias, &code).unwrap();
                oracle.set_prot(0x10000, 0x2000, P_MAPPED | P_R | P_W);
                assert_eq!(expected.run(&mut oracle, &mut BlockCache::new(), 1000), Exit::Syscall);
                assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
                same_cpu(&mut cpu, &mut expected);
                let differences: Vec<_> = mem.raw().iter().zip(oracle.raw()).enumerate().filter(|(_, (a, b))| a != b).take(16).collect();
                assert!(differences.is_empty(), "memory differs: {differences:?}");
            }
        }
        assert_eq!(cache.jit.stats.translate_ns, 0.0);
        assert_eq!(cache.jit.stats.compile_fail, 0);
        assert_eq!(cache.jit.stats.exec_calls, (classes.len() * 4 * 7) as u64);
        cache.jit.flush_all();
        jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
    }
}

#[test]
fn v4_explicit_complete_compiles_selected_hle_bytes_without_sweeping_text() {
    let mut image = metadata_elf();
    image[0x100..0x110].copy_from_slice(&[0x0f, 0xa2, 0xc3, 0xcc, 0xd9, 0xe8, 0xc3, 0xcc, 0x43, 0xc3, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc]);
    let (artifacts, report) = compile_plugin_complete_bundles(&image).unwrap();
    assert_eq!(report.decoded_instructions, report.compiled_instructions);
    assert_eq!(report.fallback_instructions, 2);
    assert_eq!(report.fallback_bytes, 4);
    assert_eq!(report.budget_frontiers, 0);
    assert_eq!(report.hle_frontiers, 0);
    assert_eq!(report.invalid_frontiers, 0);
    assert!(report.compiled_bytes < report.executable_bytes);
    assert!(artifacts.iter().all(|a| a.encode_bundle()[7] == 4));
    let leaf = portable(&crate::jit_x86::intr::FIND_CODE);
    assert!(leaf.fallback.is_empty());
    assert_eq!(leaf.outputs.iter().map(|out| out.n_ins).sum::<u32>(), 43);
}

#[test]
fn v4_plugin_complete_preserves_hle_entry_and_interior_seeds_before_fusion() {
    use crate::jit::rt::Backend;
    // Metadata-backed root calls FIND; another function follows the exact leaf.
    // An unwind entry deliberately names an interior label of the HLE function.
    let leaf = PC + 7;
    let interior = leaf + 20;
    let mut code = vec![0xe8, 2, 0, 0, 0, 0xcd, 0x80];
    code.extend(crate::jit_x86::intr::FIND_CODE);
    let tail = PC + code.len() as u32;
    code.extend([0x43, 0xcd, 0x80]);
    let mut image = metadata_elf();
    image[0x100..0x100 + code.len()].copy_from_slice(&code);
    put32(&mut image, 0x400 + 2 * 40 + 20, code.len() as u32);
    put32(&mut image, 0x24c, interior.wrapping_sub(0x1100));
    put32(&mut image, 0x298, interior.wrapping_sub(0x1218));
    put32(&mut image, 0x304, tail);
    let (artifacts, report) = compile_plugin_complete_bundles(&image).unwrap();
    assert_eq!(report.hle_frontiers, 1);
    assert_eq!((report.decoded_instructions, report.compiled_instructions), (4, 4));
    assert_eq!(report.compiled_bytes, 10);
    assert_eq!(report.guarded_bytes, code.len());
    assert_eq!(report.budget_frontiers, 0);
    assert!(report.unsupported.is_empty());
    let artifact = &artifacts[0];
    assert_eq!(artifact.outputs[0].n_ins, 4);
    assert!(artifact.outputs[0].heads.iter().all(|&(ip, _)| !(leaf..tail).contains(&ip)));
    assert!(artifact.regions[0].successors.contains(&leaf));
    assert_eq!(artifact.regions[0].bytes, code); // guarding is not translation
    Artifact::decode(&artifact.encode_bundle()).unwrap();
    let mut layout = options().layout; layout.cache = 0x50000;
    let bound = artifact.bind(&layout, 0x2000).unwrap();
    let mut machine = Machine::new(layout);
    machine.memory.grow(&mut machine.store, 100).unwrap();
    let bytes = machine.memory.data_mut(&mut machine.store);
    bytes[bound.layout.wtab as usize..bound.layout.wtab as usize + 16].fill(3);
    let slots = machine.install(&bound.wasm, bound.regions.len()).unwrap();
    let mut cpu = Cpu::new(); cpu.eip = PC + 0x2000; cpu.r[4] = 0xf000; cpu.jit_budget = 100;
    assert_eq!(machine.call_cpu(slots[0], &mut cpu, 0), jit_x86::S_NEXT);
    assert_eq!(cpu.eip, leaf + 0x2000); // must return for runtime HLE dispatch
    assert_eq!(cpu.r[4], 0xeffc);
}

#[test]
fn v4_fallback_fault_exit_has_exact_ip_and_no_later_native_effects() {
    for (shape, pointer) in [
        (vec![0xf2, 0x0f, 0x58, 0x03], 0x1ffff), // addsd xmm0,[ebx] crosses unmapped RAM
        (vec![0xdd, 0x03], 0x12000),              // fld qword [ebx], unreadable page
        (vec![0x0f, 0x0b], 0x10000),             // ud2: defined #UD boundary
        (vec![0xf4], 0x10000),                   // hlt: preserve interpreter exit
        (vec![0x66, 0xc3], 0x12000),             // operand-size ret, never the native ret32 path
    ] {
        let mut code = vec![0x40]; code.extend(shape); code.extend([0x43, 0xcd, 0x80]);
        let artifact = portable(&code);
        let (mut mem, mut cache) = setup(&code, 0x7000, true);
        install(&artifact, 0x7000, &mut mem, &mut cache.jit, &[]).unwrap();
        let mut oracle = Memory::new(mem.size());
        oracle.set_prot(PC + 0x7000, 0x1000, P_MAPPED | P_R | P_W | P_X);
        oracle.write_bytes(PC + 0x7000, &code).unwrap();
        oracle.set_prot(0x10000, 0x2000, P_MAPPED | P_R | P_W);
        let mut actual = Cpu::new(); actual.eip = PC + 0x7000;
        actual.r[3] = pointer; actual.r[4] = pointer;
        actual.xmm[0] = 0xfedc_ba98_7654_3210_7ff0_0000_0000_0001;
        let mut expected = actual.clone();
        let exit = expected.run(&mut oracle, &mut BlockCache::new(), 1000);
        assert_eq!(actual.run(&mut mem, &mut cache, 1000), exit);
        same_cpu(&mut actual, &mut expected);
        assert_eq!(actual.r[3], pointer, "executed native inc after failed fallback");
        assert_eq!(cache.jit.stats.calls, 1);
        assert_eq!(cache.jit.stats.exec_calls, 1);
        cache.jit.flush_all();
        jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
    }
}

#[test]
fn v4_fallback_smc_stops_before_stale_native_code_and_rejects_reinstall() {
    // movsb is generic fallback; it overwrites the following inc eax with nop.
    // The helper must stop the region before the stale native inc is reached.
    let code = [0xa4, 0x40, 0xcd, 0x80];
    let artifact = portable(&code);
    for bias in [0x2000, 0x7000] {
        let (mut mem, mut cache) = setup(&code, bias, false);
        mem.w8(0x10000, 0x90).unwrap();
        assert_eq!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).unwrap(), 1);
        let mut cpu = Cpu::new(); cpu.eip = PC + bias;
        cpu.r[0] = 7; cpu.r[6] = 0x10000; cpu.r[7] = PC + bias + 1;
        assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
        assert_eq!(cpu.r[0], 7);
        assert_eq!(mem.r8(PC + bias + 1).unwrap(), 0x90);
        assert!(!cache.jit.has_entry(PC + bias));
        assert_eq!(cache.jit.stats.inv_store, 1);
        assert!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).is_err());
        cache.jit.flush_all();
        jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
    }
}

#[test]
fn v4_fallback_tags_survive_partial_region_drop_and_rollback_without_aliasing() {
    let mut code = vec![0x0f, 0xa2, 0xcd, 0x80];
    code.resize(FUSION_MAX_SPAN, 0xcc);
    code.extend([0xd9, 0xe8, 0xcd, 0x80]);
    let mut image = elf(&code); put16(&mut image, 16, 3);
    let mut config = options(); config.entries.push(PC + FUSION_MAX_SPAN as u32);
    let artifact = compile_portable_complete(&image, &config).unwrap();
    assert_eq!(artifact.regions.len(), 2);
    assert_eq!(artifact.fallback.len(), 2);
    let bias = 0x2000;
    let (mut mem, mut cache) = setup(&[0x0f, 0xa2, 0xcd, 0x80], bias, false);
    let second = PC + bias + FUSION_MAX_SPAN as u32;
    mem.set_prot(second, 0x1000, P_MAPPED | P_R | P_W | P_X);
    mem.write_bytes(second, &[0xd9, 0xe8, 0xcd, 0x80]).unwrap();
    let instructions = artifact.decode_fallback_instructions(bias).unwrap();
    let tags = cache.jit.allocate_offline_tags(&instructions);
    let bound = artifact.bind_with_tags(&cache.jit.aot_layout(&mem), bias, &tags).unwrap();
    assert_eq!(cache.jit.install_precompiled_fallbacks(&mut mem, &bound.layout, &bound.wasm, &region_inputs(&bound), &[], &tags).unwrap(), 2);
    cache.jit.release_offline_tags(&tags); // cannot release transferred ownership
    mem.w8(PC + bias, 0x90).unwrap(); cache.sync(&mut mem);
    assert!(!cache.jit.has_entry(PC + bias)); assert!(cache.jit.has_entry(second));
    let probe = cache.jit.allocate_offline_tags(&instructions);
    assert!(probe.iter().all(|tag| !tags.contains(tag)));
    cache.jit.release_offline_tags(&probe);
    cache.jit.release_offline_tags(&probe); // rollback is idempotent
    let mut cpu = Cpu::new(); cpu.eip = second;
    assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
    assert_eq!(cpu.fpu.st[7].mant, 1 << 63);
    assert_eq!(cpu.fpu.st[7].se, 0x3fff);
    cache.jit.flush_all();
    let tags_after_drop = cache.jit.allocate_offline_tags(&instructions);
    assert!(tags.iter().all(|tag| tags_after_drop.contains(tag)));
    cache.jit.release_offline_tags(&tags_after_drop);
    jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
}

#[test]
fn v4_fallback_exclusion_rollback_and_independent_module_local_tags() {
    let code = [0xf2, 0x0f, 0x58, 0xc1, 0xcd, 0x80];
    let artifact = portable(&code);
    let (mut mem, mut cache) = setup(&code, 0x2000, false);
    assert_eq!(install(&artifact, 0x2000, &mut mem, &mut cache.jit, &[PC + 0x2000 + 1]).unwrap(), 0);
    assert!(!cache.jit.has_entry(PC + 0x2000));
    // Failed and entirely excluded modules must return reservations for reuse.
    let instructions = artifact.decode_fallback_instructions(0x2000).unwrap();
    let tags = cache.jit.allocate_offline_tags(&instructions);
    assert_eq!(tags, [0]); cache.jit.release_offline_tags(&tags);
    assert_eq!(install(&artifact, 0x2000, &mut mem, &mut cache.jit, &[]).unwrap(), 1);
    let code2 = [0xd9, 0xe8, 0xcd, 0x80];
    let artifact2 = portable(&code2);
    mem.set_prot(PC + 0x7000, 0x1000, P_MAPPED | P_R | P_W | P_X);
    mem.write_bytes(PC + 0x7000, &code2).unwrap();
    assert_eq!(install(&artifact2, 0x7000, &mut mem, &mut cache.jit, &[]).unwrap(), 1);
    for pc in [PC + 0x2000, PC + 0x7000] {
        let mut cpu = Cpu::new(); cpu.eip = pc; cpu.xmm[0] = 1.0f64.to_bits() as u128; cpu.xmm[1] = 2.0f64.to_bits() as u128;
        assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Syscall);
        if pc == PC + 0x2000 { assert_eq!(cpu.xmm[0] as u64, 3.0f64.to_bits()); }
        else { assert_eq!(cpu.fpu.st[7].se, 0x3fff); }
    }
    cache.jit.flush_all();
    jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
}

#[test]
fn v4_batches_allocate_dense_local_tags_only_for_real_second_pass_calls() {
    let mut code = vec![0x0f, 0xa2, 0xc3];
    code.resize(FUSION_MAX_SPAN, 0xcc); code.extend([0xd9, 0xe8, 0xc3]);
    let image = elf(&code);
    let mut config = options(); config.entries.push(PC + FUSION_MAX_SPAN as u32);
    let mut report = DiscoveryReport::default();
    let artifacts = compile_source_batches_complete(&image, &config, segments(&image, 0).unwrap(), true, true, 1, &mut report, true, true, false).unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(report.fallback_instructions, 2);
    for artifact in artifacts {
        assert_eq!(artifact.fallback.len(), 1);
        assert_eq!(artifact.outputs[0].n_fallback, 1);
        assert_eq!(artifact.relocations.as_ref().unwrap().iter().filter(|r| r.kind == 5).map(|r| r.value).collect::<Vec<_>>(), [0]);
        Artifact::decode(&artifact.encode_bundle()).unwrap();
    }
}

#[test]
fn v4_fallback_install_guards_exact_instruction_bytes_execute_permissions_and_layout() {
    let code = [0xf2, 0x0f, 0x58, 0xc1, 0xcd, 0x80];
    let artifact = portable(&code);
    let bias = 0x7000;
    let (mut mem, mut cache) = setup(&code, bias, true);
    let layout = cache.jit.aot_layout(&mem);
    let bound = artifact.bind(&layout, bias).unwrap();
    assert!(bound.guard(&mem, &layout));
    // A change in a fallback-only instruction is a real code dependency, not
    // metadata that can be ignored because the arithmetic executes in Rust.
    mem.w8(PC + bias + 2, 0x59).unwrap();
    assert!(!bound.guard(&mem, &layout));
    assert!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).is_err());
    assert!(!cache.jit.has_entry(PC + bias));
    mem.w8(PC + bias + 2, 0x58).unwrap();
    mem.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W);
    assert!(!bound.guard(&mem, &layout));
    assert!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).is_err());
    mem.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W | P_X);
    let mut wrong = layout.clone(); wrong.cpu_xmm += 16;
    let instructions = artifact.decode_fallback_instructions(bias).unwrap();
    let tags = cache.jit.allocate_offline_tags(&instructions);
    assert!(artifact.bind_with_tags(&wrong, bias, &tags).is_err());
    cache.jit.release_offline_tags(&tags);
    assert_eq!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).unwrap(), 1);
    assert_eq!(cache.jit.stats.aot_regions, 1);
    assert_eq!(install(&artifact, bias, &mut mem, &mut cache.jit, &[]).unwrap(), 0);
    cache.jit.flush_all();
    jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
}

#[test]
fn v4_native_retry_after_fallback_preserves_commit_without_reexecuting_instruction() {
    // fld1 commits through exec_one; the following native load retries because
    // its memory is unreadable. Resume at the load, never push a second x87 value.
    let code = [0xd9, 0xe8, 0x8b, 0x03, 0xcd, 0x80];
    let artifact = portable(&code);
    let bias = 0x2000;
    let (mut mem, mut cache) = setup(&code, bias, false);
    install(&artifact, bias, &mut mem, &mut cache.jit, &[]).unwrap();
    let mut actual = Cpu::new(); actual.eip = PC + bias; actual.r[3] = 0x12000;
    let mut expected = actual.clone();
    let mut oracle = Memory::new(mem.size());
    oracle.set_prot(PC + bias, 0x1000, P_MAPPED | P_R | P_W | P_X);
    oracle.write_bytes(PC + bias, &code).unwrap();
    let expected_exit = expected.run(&mut oracle, &mut BlockCache::new(), 1000);
    assert_eq!(actual.run(&mut mem, &mut cache, 1000), expected_exit);
    same_cpu(&mut actual, &mut expected);
    assert_eq!(actual.eip, PC + bias + 2);
    assert_eq!(actual.fpu.top, 7);
    assert_eq!(cache.jit.stats.retries, 1);
    assert_eq!(cache.jit.stats.exec_calls, 1);
    cache.jit.flush_all();
    jit::rt::BACKEND.with(|backend| *backend.borrow_mut() = None);
}
