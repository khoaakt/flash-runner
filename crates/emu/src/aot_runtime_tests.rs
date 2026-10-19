//! Offline modules enter the same dispatcher and invalidation lifecycle as normal JIT regions.
use super::*;

fn artifact(
    mem: &Memory,
    jit: &JitState,
    code: &[u8],
) -> (Layout, Vec<u8>, Vec<(RegionOut, Vec<(u32, Vec<u8>)>)>) {
    let ins: Vec<_> =
        iced_x86::Decoder::with_ip(32, code, CODE as u64, iced_x86::DecoderOptions::NONE)
            .into_iter()
            .collect();
    let lay = jit.aot_layout(mem);
    let node = NodeIn {
        start: CODE,
        end: CODE + code.len() as u32,
        ins: &ins,
        intrinsic: 0,
        thunk: 0,
    };
    let (wasm, outs) = translate_module(
        &lay,
        &[vec![node]],
        &mut |_| panic!("unexpected AOT fallback"),
        &mut FbStats::new(),
        0,
    );
    (
        lay,
        wasm,
        vec![(outs[0].clone(), vec![(CODE, code.to_vec())])],
    )
}

fn configured_jit() -> JitState {
    let mut jit = JitState::new();
    jit.threshold = 100;
    jit.layout_override = Some((GUEST, RTAB, WTAB, 0));
    jit
}

#[test]
fn offline_module_executes_through_dispatcher() {
    install_backend();
    let code = [0xb8, 123, 0, 0, 0, 0xbb, 7, 0, 0, 0, 0x01, 0xd8, 0xcd, 0x80];
    let mut mem = build_mem(37, &code);
    let mut bc = BlockCache::new();
    bc.jit = configured_jit();
    let (lay, wasm, regions) = artifact(&mem, &bc.jit, &code);
    assert_eq!(
        bc.jit
            .install_precompiled(&mut mem, &lay, &wasm, &regions)
            .unwrap(),
        1
    );
    let initial = initial_cpu(&mut Rng::new(37));
    let mut actual = initial.clone();
    let mut expected = initial;
    let mut oracle = build_mem(37, &code);
    let expected_exit = expected.run(&mut oracle, &mut BlockCache::new(), 10000);
    let actual_exit = actual.run(&mut mem, &mut bc, 10000);
    expected.lf_flush();
    actual.lf_flush();
    assert_eq!(actual_exit, expected_exit);
    assert!(diff_cpu(&expected, &actual).is_none());
    assert_eq!(mem.raw(), oracle.raw());
    assert!(bc.jit.stats.calls > 0);
    assert_eq!(bc.jit.stats.aot_regions, 1);
}

#[test]
fn offline_guards_reject_before_publishing() {
    install_backend();
    let code = [0xb8, 1, 0, 0, 0, 0xcd, 0x80];
    let mut mem = build_mem(1, &code);
    let mut jit = configured_jit();
    let (lay, wasm, mut regions) = artifact(&mem, &jit, &code);
    let mut wrong = lay.clone();
    wrong.guest_base += 16;
    assert!(jit
        .install_precompiled(&mut mem, &wrong, &wasm, &regions)
        .is_err());
    regions[0].1[0].1[0] ^= 1;
    assert!(jit
        .install_precompiled(&mut mem, &lay, &wasm, &regions)
        .is_err());
    regions[0].1[0].1[0] ^= 1;
    regions[0].0.n_fallback = 1;
    assert!(jit
        .install_precompiled(&mut mem, &lay, &wasm, &regions)
        .is_err());
    assert!(!jit.has_entry(CODE));
    assert_eq!(jit.stats.aot_regions, 0);
}

#[test]
fn offline_regions_park_restore_and_invalidate_by_exact_bytes() {
    install_backend();
    let code = [0xb8, 1, 0, 0, 0, 0xcd, 0x80];
    let mut mem = build_mem(1, &code);
    let mut jit = configured_jit();
    let (lay, wasm, regions) = artifact(&mem, &jit, &code);
    jit.install_precompiled(&mut mem, &lay, &wasm, &regions)
        .unwrap();
    let page = CODE >> 12;
    jit.park_page(page);
    assert!(!jit.has_entry(CODE));
    jit.unpark_page(page, |_| true);
    assert!(jit.has_entry(CODE));
    jit.invalidate_range(page, CODE + 100, CODE + 104, false);
    assert!(jit.has_entry(CODE));
    jit.invalidate_range(page, CODE + 1, CODE + 2, false);
    assert!(!jit.has_entry(CODE));
    assert_eq!(jit.stats.inv_store, 1);
}

#[test]
fn hot_offline_block_yields_to_fused_jit_without_changing_results() {
    install_backend();
    // inc eax; jmp entry. A single cold AOT block eventually promotes to fused JIT.
    let code = [0x40, 0xcd, 0x82];
    let mut mem = build_mem(93, &code);
    let mut cache = BlockCache::new();
    cache.jit = configured_jit();
    cache.jit.threshold = 2;
    let (layout, wasm, regions) = artifact(&mem, &cache.jit, &code);
    cache
        .jit
        .install_precompiled(&mut mem, &layout, &wasm, &regions)
        .unwrap();
    let mut cpu = initial_cpu(&mut Rng::new(91));
    cpu.r[EAX] = 0;
    for _ in 0..100 {
        cpu.eip = CODE;
        assert_eq!(cpu.run(&mut mem, &mut cache, 100), Exit::Hyper);
    }
    assert_eq!(cpu.r[EAX], 100);
    assert!(!cache.jit.is_aot_entry(CODE));
    assert!(cache.jit.has_entry(CODE));
    assert!(cache.jit.stats.aot_dispatches >= 49);
}

#[test]
fn offline_probe_overlap_skips_only_affected_export_without_renumbering_slots() {
    install_backend();let code=[0xb8,1,0,0,0,0xcd,0x82];let mut mem=build_mem(97,&code);
    let second=CODE+0x100;mem.write_bytes(second,&code).unwrap();
    let mut jit=configured_jit();let layout=jit.aot_layout(&mem);
    let first_ins:Vec<_>=iced_x86::Decoder::with_ip(32,&code,CODE as u64,iced_x86::DecoderOptions::NONE).into_iter().collect();
    let second_ins:Vec<_>=iced_x86::Decoder::with_ip(32,&code,second as u64,iced_x86::DecoderOptions::NONE).into_iter().collect();
    let nodes=vec![vec![NodeIn{start:CODE,end:CODE+code.len()as u32,ins:&first_ins,intrinsic:0,thunk:0}],vec![NodeIn{start:second,end:second+code.len()as u32,ins:&second_ins,intrinsic:0,thunk:0}]];
    let (wasm,out)=translate_module(&layout,&nodes,&mut |_|panic!("unexpected fallback"),&mut FbStats::new(),0);
    let regions=vec![(out[0].clone(),vec![(CODE,code.to_vec())]),(out[1].clone(),vec![(second,code.to_vec())])];
    assert_eq!(jit.install_precompiled_excluding(&mut mem,&layout,&wasm,&regions,&[CODE+1]).unwrap(),1);
    assert!(!jit.has_entry(CODE));assert!(jit.has_entry(second));
    let mut cache=BlockCache::new();cache.jit=jit;let mut cpu=initial_cpu(&mut Rng::new(91));cpu.eip=second;
    assert_eq!(cpu.run(&mut mem,&mut cache,1000),Exit::Hyper);assert_eq!(cpu.r[EAX],1);
}

#[test]
fn offline_fallback_tag_lifetime_survives_partial_region_retirement() {
    // Isolate registry ownership from helper semantics (covered by v4 IEEE tests).
    // Two exports retain the same module tag until the LAST export is retired.
    install_backend();
    let code = [0xb8, 1, 0, 0, 0, 0xcd, 0x82];
    let second = CODE + 0x100;
    let mut mem = build_mem(98, &code);
    mem.write_bytes(second, &code).unwrap();
    let mut jit = configured_jit();
    let layout = jit.aot_layout(&mem);
    let instructions: Vec<Vec<_>> = [CODE, second].iter().map(|&ip|
        iced_x86::Decoder::with_ip(32, &code, ip as u64, iced_x86::DecoderOptions::NONE).into_iter().collect()).collect();
    let nodes: Vec<_> = [CODE, second].iter().zip(&instructions).map(|(&ip, ins)|
        vec![NodeIn { start: ip, end: ip + code.len() as u32, ins, intrinsic: 0, thunk: 0 }]).collect();
    let (wasm, outputs) = translate_module(&layout, &nodes, &mut |_| panic!("unexpected fallback"), &mut FbStats::new(), 0);
    let mut regions: Vec<_> = outputs.into_iter().zip([CODE, second]).map(|(out, ip)| (out, vec![(ip, code.to_vec())])).collect();
    regions[0].0.n_fallback = 1;
    let instruction = instructions[0][0];
    let tags = jit.allocate_offline_tags(&[instruction]);
    jit.install_precompiled_fallbacks(&mut mem, &layout, &wasm, &regions, &[], &tags).unwrap();
    let original = tags[0];
    jit.release_offline_tags(&tags); // harmless once installed
    jit.invalidate_range(CODE >> 12, CODE, CODE + 1, false);
    assert!(jit.has_entry(second));
    let other = jit.allocate_offline_tags(&[instruction]);
    assert_ne!(other[0], original, "surviving export still owns its module tags");
    jit.release_offline_tags(&other);
    jit.release_offline_tags(&other); // rollback is idempotent
    jit.invalidate_range(second >> 12, second, second + 1, false);
    let recycled = jit.allocate_offline_tags(&[instruction, instruction]);
    assert_eq!(recycled[0], original);
    assert_ne!(recycled[0], recycled[1], "double rollback must not duplicate free tags");
    jit.release_offline_tags(&recycled);
    jit.flush_all();
}

#[test]
fn malformed_offline_fallback_owner_cannot_publish_region() {
    install_backend();let code=[0xb8,1,0,0,0,0xcd,0x82];let mut mem=build_mem(99,&code);let mut jit=configured_jit();
    let (layout,wasm,mut regions)=artifact(&mem,&jit,&code);regions[0].0.n_fallback=1;
    let instruction=iced_x86::Decoder::with_ip(32,&[0xd9,0xf2],(CODE+0x100)as u64,iced_x86::DecoderOptions::NONE).decode();
    let tags=jit.allocate_offline_tags(&[instruction]);
    assert!(jit.install_precompiled_fallbacks(&mut mem,&layout,&wasm,&regions,&[],&tags).is_err());
    assert!(!jit.has_entry(CODE));jit.release_offline_tags(&tags);
}

#[test]
fn tiny_aot_callee_can_inline_even_when_its_fused_region_is_large() {
    install_backend();
    let callee = CODE + 0x100;
    let big = CODE + 0x200;
    let mut caller = vec![0xe8];
    caller.extend_from_slice(&(callee - (CODE + 5)).to_le_bytes());
    caller.extend_from_slice(&[0xcd, 0x82]);
    let leaf = [0x83, 0xc0, 7, 0xc3];
    let mut large = vec![0x90; 64]; large.push(0xc3);
    let mut mem = build_mem(100, &caller);
    mem.write_bytes(callee, &leaf).unwrap(); mem.write_bytes(big, &large).unwrap();
    let mut cache = BlockCache::new(); cache.jit = configured_jit(); cache.jit.threshold = 1; cache.jit.dup_ins = 8;
    let layout = cache.jit.aot_layout(&mem);
    let decode = |bytes: &[u8], ip| iced_x86::Decoder::with_ip(32, bytes, ip, iced_x86::DecoderOptions::NONE).into_iter().collect::<Vec<_>>();
    let (tiny_ins, large_ins) = (decode(&leaf, callee as u64), decode(&large, big as u64));
    let nodes = vec![vec![NodeIn { start: callee, end: callee + leaf.len() as u32, ins: &tiny_ins, intrinsic: 0, thunk: 0 },
        NodeIn { start: big, end: big + large.len() as u32, ins: &large_ins, intrinsic: 0, thunk: 0 }]];
    let (wasm, output) = translate_module(&layout, &nodes, &mut |_| panic!("unexpected fallback"), &mut FbStats::new(), 0);
    assert!(output[0].n_ins > cache.jit.dup_ins);
    cache.jit.install_precompiled(&mut mem, &layout, &wasm,
        &[(output[0].clone(), vec![(callee, leaf.to_vec()), (big, large)])]).unwrap();
    assert!(cache.compile_region(&mut mem, CODE));
    assert_eq!(cache.jit.stats.dup_ins, 2, "size cap must use the small callee, not its 67-instruction offline group");
    let mut cpu = initial_cpu(&mut Rng::new(91)); cpu.r[EAX] = 5;
    assert_eq!(cpu.run(&mut mem, &mut cache, 1000), Exit::Hyper);
    assert_eq!(cpu.r[EAX], 12);
}
