use super::*;
use crate::jit_x86::intr::{detect, INTR_NONE, INTR_RANGE_CMP, RANGE_CMP_CODE};

/// Opt-in provenance check against private shipped artifacts, not a gameplay test.
#[test]
#[ignore = "requires vendor-private Flash plugins"]
fn intrinsic_range_cmp_shipped_plugin_signatures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor-private");
    for (name, expected) in [
        ("libpepflashplayer-26.0.0.151.so", 0xa3fd84),
        ("libpepflashplayer-32.0.0.363.so", 0xa4de54),
        ("libpepflashplayer.so", 0xa55fb4),
    ] {
        let bytes = std::fs::read(root.join(name)).unwrap();
        let hits: Vec<usize> = bytes.windows(RANGE_CMP_CODE.len()).enumerate().filter_map(|(i, b)| (b == RANGE_CMP_CODE).then_some(i)).collect();
        assert_eq!(hits, vec![expected], "exact-byte occurrences in {name}");
        assert_eq!(&bytes[..6], b"\x7fELF\x01\x01", "ELF32 little endian");
        let u32_at = |p| u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
        let phoff = u32_at(28);
        let phsize = u16::from_le_bytes(bytes[42..44].try_into().unwrap()) as usize;
        let phnum = u16::from_le_bytes(bytes[44..46].try_into().unwrap()) as usize;
        assert!((0..phnum).any(|i| {
            let p = phoff + i * phsize;
            let off = u32_at(p + 4);
            u32_at(p) == 1 && u32_at(p + 24) & 1 != 0
                && expected >= off && expected + RANGE_CMP_CODE.len() <= off + u32_at(p + 16)
        }), "signature must lie wholly in an executable PT_LOAD segment");
        eprintln!("{name}: one executable RANGE_CMP_CODE signature at file offset {expected:#x}");
    }
}

#[test]
fn intrinsic_range_cmp_matches_interpreter() {
    big_stack(range_cmp_body);
}

fn range_cmp_body() {
    let n: u64 = std::env::var("JIT_INTR_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(3000);
    let before = INTRINSICS_SEEN.with(|c| c.get());
    for seed in 0..n {
        let seed = seed + 0xc090_0000;
        let mut rng = Rng::new(seed);
        let esp = match rng.below(8) {
            0 => STACK + 0x1000 - rng.below(20),
            1 => STACK + 0x10000 + rng.below(20),
            2 => PG_RONLY + 64,
            3 => PG_CODEFLAG + 64,
            _ => STACK + 0x4000 + rng.below(0x1000),
        };
        // ESP here is the caller's stack pointer; after arguments and CALL the
        // intrinsic dead frame occupies [esp-20,esp-12).
        let choose_ptr = |rng: &mut Rng| match rng.below(10) {
            0 => PG_NONE,
            1 => PG_RONLY + 0xffc, // length faults only if key >= start
            2 => esp.wrapping_sub(24).wrapping_add(rng.below(28)),
            3 => PG_WONLY + 64,
            4 => DATA + 0xffe, // unaligned, cross-page load
            _ => DATA + 0x100 + rng.below(0x100),
        };
        let keyp = choose_ptr(&mut rng);
        let elem = choose_ptr(&mut rng);
        let start = rng.next() as u32;
        let len = rng.next() as u32;
        let key = match rng.below(6) {
            0 => start,
            1 => start.wrapping_sub(1),
            2 => start.wrapping_add(len),
            3 => start.wrapping_add(len).wrapping_sub(1),
            _ => rng.next() as u32,
        };
        // Place some leaves across a code page: their non-entry tail must also
        // be tracked as a dependency by the region cache.
        let off = if seed & 1 == 0 { 0x400 } else { 0xfe0 };
        let func = CODE + off;
        let mut image = vec![0xbc];
        image.extend_from_slice(&esp.to_le_bytes());
        for arg in [elem, keyp] {
            image.push(0x68);
            image.extend_from_slice(&arg.to_le_bytes());
        }
        image.push(0xe8);
        let next = CODE + image.len() as u32 + 4;
        image.extend_from_slice(&func.wrapping_sub(next).to_le_bytes());
        image.extend_from_slice(&[0x8d, 0x64, 0x24, 8, 0xcd, 0x80]); // LEA preserves comparator flags
        image.resize(off as usize, 0x90);
        image.extend_from_slice(&RANGE_CMP_CODE);
        let mut cpu = initial_cpu(&mut rng);
        cpu.eip = CODE;
        let init = |m: &mut Memory| {
            let _ = m.write_bytes(elem, &start.to_le_bytes());
            let _ = m.write_bytes(elem.wrapping_add(4), &len.to_le_bytes());
            let _ = m.write_bytes(keyp, &key.to_le_bytes());
        };
        check_case_full(seed, &image, &cpu, None, Some(&init)).unwrap_or_else(|e| panic!("{e}"));
    }
    assert!(INTRINSICS_SEEN.with(|c| c.get()) > before, "fast path must be translated");
}

#[test]
fn intrinsic_range_cmp_requires_every_code_byte() {
    let mut image = RANGE_CMP_CODE.to_vec();
    let mut mem = build_mem(1, &image);
    assert_eq!(detect(&mem, CODE), INTR_RANGE_CMP);
    for i in 0..image.len() {
        image[i] ^= 1;
        mem.write_bytes(CODE, &image).unwrap();
        assert_eq!(detect(&mem, CODE), INTR_NONE, "changed byte {i}");
        image[i] ^= 1;
    }
}

#[test]
fn intrinsic_range_cmp_tracks_undecoded_tail() {
    use crate::mem::{P_MAPPED, P_R, P_W, P_X};
    for protect in [false, true] {
        install_backend();
        let mut mem = Memory::new(32 * 4096);
        mem.set_prot(0, 32 * 4096, P_MAPPED | P_R | P_W | P_X);
        let func = CODE + 0xfe0;
        mem.write_bytes(func, &RANGE_CMP_CODE).unwrap();
        let mut bc = BlockCache::new();
        bc.jit.threshold = 2; // compile only entry, not eagerly decoded successors
        assert!(bc.compile_region(&mut mem, func));
        assert_eq!(bc.jit.stats.intrinsics, 1);
        let tail = func + 34; // POP EBP, beyond entry block and on the next page
        if protect {
            mem.set_prot(tail & !0xfff, 4096, P_MAPPED | P_R | P_W);
        } else {
            mem.w8(tail, 0x90).unwrap();
        }
        bc.sync(&mut mem);
        assert!(!bc.jit.has_entry(func), "tail change must remove dispatch entry");
    }
    let mut image = vec![0x90; 0xfe0];
    image.extend_from_slice(&RANGE_CMP_CODE);
    let mut mem = build_mem(1, &image);
    mem.set_prot(CODE + 0x1000, 4096, P_MAPPED | P_R | P_W);
    assert_eq!(detect(&mem, CODE + 0xfe0), INTR_NONE, "non-executable tail must not match");
}

#[test]
fn all_runtime_intrinsics_guard_executable_tails_and_smc_dependencies() {
    use crate::jit_x86::intr::{FIND_CODE,BSEARCH_CODE,INTR_FIND,INTR_BSEARCH};
    for (kind, code) in [(INTR_FIND,&FIND_CODE[..]),(INTR_BSEARCH,&BSEARCH_CODE[..]),(INTR_RANGE_CMP,&RANGE_CMP_CODE[..])] {
        for protect in [false,true] {
            install_backend();let mut mem=Memory::new(32*4096);mem.set_prot(0,32*4096,P_MAPPED|P_R|P_W|P_X);
            let func=CODE+0xff0;mem.write_bytes(func,code).unwrap();
            assert_eq!(detect(&mem,func),kind);
            let mut cache=BlockCache::new();cache.jit.threshold=2;
            assert!(cache.compile_region(&mut mem,func));
            assert_eq!(cache.jit.stats.intrinsics,1);
            let tail=func+code.len()as u32-1;
            if protect {mem.set_prot(tail&!4095,4096,P_MAPPED|P_R|P_W);assert_eq!(detect(&mem,func),INTR_NONE);}
            else {mem.w8(tail,0x90).unwrap();}
            cache.sync(&mut mem);assert!(!cache.jit.has_entry(func));
        }
    }
}
