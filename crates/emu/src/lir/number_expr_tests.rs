//! Nested/multi-argument Number programs. Native bytes are real .363 NanoJIT output for
//! tests/swf-src/LirNumberExpr.hx (see number_expr_data.rs). The SSA below is written independently of
//! the JS extractor, so an extractor/verifier disagreement fails here.
use super::expr_data::{CAPTURED_CORE, CAPTURED_MASK, CAPTURED_POOL, SHAPES};
use super::tests::{encode, hex, initial, layout, mask, Rig, ARGV0, CORE, ENTRY, MASK, SIZE, SP0};
use super::*;
use crate::cpu::{EAX, ECX, EDX};
use crate::mem::{Memory as GuestMemory, P_R, P_W, P_X};

/// (name, argc, argv offsets in SSA parameter order, SSA, program steps or 0 for a single operation)
const F: u32 = RECEIVER_FIELD; // binding flag: a receiver field rather than an argv slot
/// Receiver object (argv[0]) in test memory; fields live at OBJ + offset.
const OBJ: u32 = 0xa000;
/// Test-space base of the constant pool (the captured pool slots are relocated to CONST_POOL + 8 * index).
const CONST_POOL: u32 = 0x7000;
const CASES: [(&str, u32, &[u32], &str, Option<usize>); 31] = [
    ("third", 3, &[20], "params f64\nc = argd 0\nretd c", None),
    ("add_ac", 3, &[4, 20], "params f64 f64\na = argd 0\nc = argd 1\nr = addd a c\nretd r", None),
    ("mul_bc", 3, &[12, 20], "params f64 f64\nb = argd 0\nc = argd 1\nr = muld b c\nretd r", None),
    ("mul_add", 3, &[4, 12, 20], "params f64 f64 f64\na = argd 0\nb = argd 1\nc = argd 2\np = muld a b\nr = addd p c\nretd r", Some(2)),
    ("add_mul", 3, &[4, 12, 20], "params f64 f64 f64\na = argd 0\nb = argd 1\nc = argd 2\np = addd a b\nr = muld p c\nretd r", Some(2)),
    ("sub_div", 3, &[4, 12, 20], "params f64 f64 f64\na = argd 0\nb = argd 1\nc = argd 2\np = subd a b\nr = divd p c\nretd r", Some(2)),
    ("lerp", 3, &[4, 12, 20], "params f64 f64 f64\na = argd 0\nb = argd 1\nt = argd 2\nd = subd b a\nm = muld d t\nr = addd a m\nretd r", Some(3)),
    ("sum_squares", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\np = muld a a\nq = muld b b\nr = addd p q\nretd r", Some(3)),
    ("diff_product", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\ns = addd a b\nd = subd a b\nr = muld s d\nretd r", Some(3)),
    ("neg_mul", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\np = muld a b\nr = negd p\nretd r", Some(2)),
    ("shared_square", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\ns = addd a b\nr = muld s s\nretd r", Some(2)),
    ("distance2", 4, &[4, 20, 12, 28],
     "params f64 f64 f64 f64\na = argd 0\nc = argd 1\nb = argd 2\nd = argd 3\ndx = subd a c\np = muld dx dx\ndy = subd b d\nq = muld dy dy\nr = addd p q\nretd r", Some(5)),
    // constants: pool slots (2.5, 1.5, 0.5, 0.25) and code-immediate integers (2.0, 3.0, 1.0)
    ("scale", 1, &[4], "params f64\na = argd 0\nk = immd bits:4004000000000000\nr = muld a k\nretd r", Some(1)),
    ("offset", 1, &[4], "params f64\na = argd 0\nk = immd bits:3ff8000000000000\nr = addd a k\nretd r", Some(1)),
    ("half_sum", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\ns = addd a b\nk = immd bits:3fe0000000000000\nr = muld s k\nretd r", Some(2)),
    ("affine", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\nk2 = immd bits:4000000000000000\np = muld a k2\nk4 = immd bits:3fd0000000000000\nq = muld b k4\nr = addd p q\nretd r", Some(3)),
    ("shift_scale", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\nk = immd bits:3ff0000000000000\nd = subd a k\nr = muld d b\nretd r", Some(2)),
    ("shared_const", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\nk = immd bits:4008000000000000\np = muld a k\nq = muld b k\nr = addd p q\nretd r", Some(3)),
    // int32 arguments converted exactly (cvtsi2sd, or FILD of a spilled int) and mixed with doubles
    ("int_to_num", 1, &[4], "params i32\na = parami 0\nr = i2d a\nretd r", Some(0)),
    ("half_int", 1, &[4], "params i32\na = parami 0\nd = i2d a\nk = immd bits:3fe0000000000000\nr = muld d k\nretd r", Some(1)),
    ("int_plus_num", 2, &[4, 8], "params i32 f64\na = parami 0\nb = argd 1\nd = i2d a\nr = addd d b\nretd r", Some(1)),
    ("num_plus_int", 2, &[4, 12], "params f64 i32\na = argd 0\nb = parami 1\nd = i2d b\nr = addd a d\nretd r", Some(1)),
    // Std.int: cvttsd2si, EAX result, native range check over a helper call
    ("num_to_int", 1, &[4], "params f64\na = argd 0\nd = d2i a\nreti d", Some(1)),
    ("num_to_int_scaled", 2, &[4, 12], "params f64 f64\na = argd 0\nb = argd 1\np = muld a b\nd = d2i p\nreti d", Some(2)),
    // receiver Number fields: reads, a read-modify-write and a setter (FLD + FST). Bindings F | offset are fields.
    ("f_get_x", 0, &[F | 0x60], "params f64\nfx = argd 0\nretd fx", Some(0)),
    ("f_sum_xy", 0, &[F | 0x60, F | 0x58], "params f64 f64\nfx = argd 0\nfy = argd 1\nr = addd fx fy\nretd r", Some(1)),
    ("f_dist2", 0, &[F | 0x60, F | 0x58], "params f64 f64\nfx = argd 0\nfy = argd 1\np = muld fx fx\nq = muld fy fy\nr = addd p q\nretd r", Some(3)),
    ("f_trunc_x", 0, &[F | 0x60], "params f64\nfx = argd 0\nd = d2i fx\nreti d", Some(1)),
    ("f_scaled", 1, &[4, F | 0x60, F | 0x58], "params f64 f64 f64\nk = argd 0\nfx = argd 1\nfy = argd 2\np = muld fx k\nr = addd p fy\nretd r", Some(2)),
    ("f_set_x", 1, &[4], "params f64\nv = argd 0\nst 96 v\nretd v", Some(0)),
    ("f_move_by", 2, &[4, 12, F | 0x60, F | 0x58],
     "params f64 f64 f64 f64\ndx = argd 0\ndy = argd 1\nfx = argd 2\nfy = argd 3\nnx = addd fx dx\nny = addd fy dy\nst 96 nx\nst 88 ny\nretd nx", Some(2)),
];

/// Bytes of the relocated constant pool (five doubles, zero padded so any 16-byte read is defined).
fn pool_image() -> [u8; 64] {
    let mut image = [0u8; 64];
    for (i, (_, bits)) in CAPTURED_POOL.iter().enumerate() { image[i * 8..i * 8 + 8].copy_from_slice(&bits.to_le_bytes()); }
    image
}
fn read_static(address: u32) -> Result<[u8; 16], String> {
    if address == MASK { return Ok(mask()); }
    match address.checked_sub(CONST_POOL) {
        Some(at) if at as usize + 16 <= 64 && at % 8 == 0 => Ok(pool_image()[at as usize..at as usize + 16].try_into().unwrap()),
        _ => Err("static address is unmapped".into()),
    }
}
fn build(bytes: &[u8], p: &Verified, argc: u32) -> Rig { build_with(bytes, p, &abi(argc), &layout()) }
fn build_with(bytes: &[u8], p: &Verified, abi: &Abi, layout: &Layout) -> Rig {
    let mut rig = Rig::build_with(bytes, p, abi, layout);
    for (i, (_, bits)) in CAPTURED_POOL.iter().enumerate() { rig.double(CONST_POOL + 8 * i as u32, *bits); }
    rig.word(ARGV0, OBJ); // argv[0], the receiver
    rig
}

fn code(name: &str) -> Vec<u8> {
    let mut bytes = hex(SHAPES.iter().find(|s| s.0 == name).unwrap().1);
    let mut moves = vec![(CAPTURED_CORE + 0x30, CORE + 0x30), (CAPTURED_CORE + 0x20, CORE + 0x20), (CAPTURED_MASK, MASK)];
    moves.extend(CAPTURED_POOL.iter().enumerate().map(|(i, (a, _))| (*a, CONST_POOL + 8 * i as u32)));
    for i in 0..bytes.len() - 3 {
        for &(from, to) in &moves {
            if bytes[i..i + 4] == from.to_le_bytes() { bytes[i..i + 4].copy_from_slice(&to.to_le_bytes()); }
        }
    }
    bytes
}
fn verify(bytes: &[u8], ssa: &str, offsets: &[u32]) -> Result<Verified, String> {
    let (text, stores) = split_stores(ssa)?;
    let trace = Trace::parse(&text).unwrap();
    verify_native_with_stores(bytes, ENTRY, CORE, &trace, offsets, &stores, read_static)
}
fn proof(name: &str) -> Verified {
    let case = CASES.iter().find(|c| c.0 == name).unwrap();
    verify(&code(name), case.3, case.2).unwrap_or_else(|e| panic!("{name}: {e}"))
}
fn abi(argc: u32) -> Abi { Abi { entry: ENTRY, core: CORE, guest_size: SIZE, argc } }

/// (end of the first conditional branch, its target) for the stack-limit guard: the fast path starts at the target.
fn fast_path(bytes: &[u8]) -> (usize, usize) {
    let mut decoder = Decoder::with_ip(32, bytes, ENTRY as u64, DecoderOptions::NONE);
    while decoder.can_decode() {
        let i = decoder.decode();
        if i.mnemonic() == M::Jae { return ((i.next_ip32() - ENTRY) as usize, (i.near_branch32() - ENTRY) as usize); }
    }
    panic!("no stack-limit guard");
}
/// Byte ranges the proof never decodes: the overflow call behind `jae`, and for Std.int the helper call behind
/// the `jne` range check. They stay bound for SMC but are only ever run by the native method.
fn skipped(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut decoder = Decoder::with_ip(32, bytes, ENTRY as u64, DecoderOptions::NONE);
    let mut out = Vec::new();
    while decoder.can_decode() {
        let i = decoder.decode();
        if matches!(i.mnemonic(), M::Jae | M::Jne) { out.push((i.next_ip32() - ENTRY) as usize..(i.near_branch32() - ENTRY) as usize); }
    }
    out
}
/// Address of the last x87 instruction on the fast path (FLD, FILD or the FST of a setter): it owns the FPU's last
/// instruction/data pointers, so the oracle is compared right after it executes.
fn last_x87(bytes: &[u8], start: usize) -> u32 {
    let mut decoder = Decoder::with_ip(32, &bytes[start..], (ENTRY as usize + start) as u64, DecoderOptions::NONE);
    let mut last = 0;
    while decoder.can_decode() {
        let i = decoder.decode();
        if matches!(i.mnemonic(), M::Fld | M::Fild | M::Fst | M::Fstp) { last = i.ip32(); }
        if i.mnemonic() == M::Ret { break; }
    }
    assert_ne!(last, 0, "no x87 instruction");
    last
}
fn is_int_result(ssa: &str) -> bool { ssa.contains("\nreti ") }

// Deterministic operand pool: signed zero, denormal, min-normal, ordinary, maximum, infinities,
// quiet/signaling NaNs with distinct payloads and both signs.
const POOL: [u64; 22] = [0x41e0000000000000, 0xc1e0000000000000, 0x41dfffffffc00000, 0xc1dfffffffe00000, 0, 1 << 63, 1, 0x8000000000000001, 0x0010000000000000, 0x3ff4000000000000,
    0xbff0000000000000, 0x7fefffffffffffff, 0x7ff0000000000000, 0xfff0000000000000, 0x7ff0000000001234,
    0xfff0000000004321, 0x7ff8123456789abc, 0xfff8fedcba987654, 0x4004000000000000, 0xc002000000000000,
    0x4202a05f20000000, 0x3f00000000000000];
fn tuples(n: usize, count: usize) -> Vec<Vec<u64>> {
    let mut state = 0x9e3779b97f4a7c15u64 ^ n as u64;
    let mut next = move || { state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (state >> 33) as usize };
    // Exhaustive for one or two inputs, deterministic pseudo-random coverage beyond that.
    if n <= 2 {
        let mut all = vec![vec![]];
        for _ in 0..n { all = all.into_iter().flat_map(|t| POOL.iter().map(move |&v| { let mut t = t.clone(); t.push(v); t })).collect(); }
        return all;
    }
    (0..count).map(|_| (0..n).map(|_| POOL[next() % POOL.len()]).collect()).collect()
}
/// `spread` bounds the binary exponent: Std.int shapes need |x| < 2^31 to stay on the replacement path.
fn ordinary_in(n: usize, count: usize, spread: i64) -> Vec<Vec<u64>> {
    let mut state = 0x2545f4914f6cdd1du64 ^ n as u64;
    let mut next = move || { state ^= state << 13; state ^= state >> 7; state ^= state << 17; state };
    (0..count).map(|i| {
        let mut t: Vec<u64> = (0..n).map(|_| {
            let r = next(); let exp = 1023 + (r % (2 * spread as u64 + 1)) as i64 - spread;
            ((r >> 63) << 63) | ((exp as u64) << 52) | (next() & 0xf_ffff_ffff_ffff)
        }).collect();
        if n >= 2 && i % 5 == 0 { t[1] = t[0]; } // a - a, a * 0 style cancellation
        t
    }).collect()
}
const MXCSR: [u32; 6] = [0x1f80, 0x3f80, 0x5f80, 0x7f80, 0x9fc0, 0xffff];

/// Run the real FP helper that `compile` selected for this proof. `inputs[i]` is the raw value at
/// `offsets[i]`, the SSA parameter order.
fn helper(cpu: &mut Cpu, p: &Verified, offsets: &[u32], inputs: &[u64], ip: u32, dp: u32) -> u32 {
    match &p.shape {
        Shape::Simple(o) => {
            let at = |offset: u32| inputs[offsets.iter().position(|&x| x == offset).unwrap()];
            native_return(cpu, at(o.a), at(o.b), o.kind, ip, dp)
        }
        Shape::Program(g) => {
            // An int32 argv slot reaches the program as its exact double conversion.
            let mut four = [0u64; MAX_INPUTS];
            for (slot, &raw) in inputs.iter().enumerate() {
                four[slot] = if g.ints[slot] { (raw as u32 as i32 as f64).to_bits() } else { raw };
            }
            let mut consts = [0u64; MAX_CONSTS];
            consts[..g.consts.len()].copy_from_slice(&g.consts);
            native_expression(cpu, four, consts, g.pack(), g.result as usize, ip, dp)
        }
    }
}
/// Guest address of the FPU data pointer the proof recorded.
fn data_pointer(p: &Verified) -> u32 {
    match p.data_pointer { DataPointer::Argv(o) => ARGV0 + o, DataPointer::Stack(o) => SP0.wrapping_add(o as u32), DataPointer::Field(o) => OBJ + o }
}
/// The (guest address, bits) writes a successful call must make: the program's stores applied to the oracle values.
fn expected_stores(p: &Verified, offsets: &[u32], inputs: &[u64], ssa: &str, mxcsr: u32) -> Vec<(u32, u64)> {
    let Shape::Program(g) = &p.shape else { return vec![] };
    let mut cpu = initial(); cpu.mxcsr = mxcsr;
    let (_, values) = native_expression_values(&mut cpu, program_inputs(g, inputs, ssa), program_consts(g), g.pack(), g.result as usize, 0, 0);
    let _ = offsets;
    g.stores.iter().map(|&(field, v)| (OBJ + field, values[v as usize])).collect()
}
fn program_inputs(g: &Program, inputs: &[u64], ssa: &str) -> [u64; MAX_INPUTS] {
    let mut four = [0u64; MAX_INPUTS];
    for (slot, &raw) in inputs.iter().enumerate() { four[slot] = if int_slots(ssa)[slot] { (raw as u32 as i32 as f64).to_bits() } else { raw }; }
    let _ = g; four
}
fn program_consts(g: &Program) -> [u64; MAX_CONSTS] { let mut c = [0u64; MAX_CONSTS]; c[..g.consts.len()].copy_from_slice(&g.consts); c }
fn apply(image: &mut [u8], writes: &[(u32, u64)]) { for &(at, bits) in writes { image[at as usize..at as usize + 8].copy_from_slice(&bits.to_le_bytes()); } }

/// Parameter types from the SSA header: true for an int32 slot.
fn int_slots(ssa: &str) -> Vec<bool> { ssa.lines().next().unwrap().split_whitespace().skip(1).map(|t| t == "i32").collect() }
/// Write the inputs where the program reads them: argv slots (int32 = 4 bytes, double = 8) or receiver fields.
fn put(rig: &mut Rig, ssa: &str, offsets: &[u32], inputs: &[u64]) {
    rig.word(ARGV0, OBJ);
    for ((&offset, &value), int) in offsets.iter().zip(inputs).zip(int_slots(ssa)) {
        if offset & F != 0 { rig.double(OBJ + (offset & !F), value) }
        else if int { rig.word(ARGV0 + offset, value as u32) } else { rig.double(ARGV0 + offset, value) }
    }
}
fn put_memory(memory: &mut GuestMemory, ssa: &str, offsets: &[u32], inputs: &[u64]) {
    memory.w32(ARGV0, OBJ).unwrap();
    for ((&offset, &value), int) in offsets.iter().zip(inputs).zip(int_slots(ssa)) {
        if offset & F != 0 { memory.w64(OBJ + (offset & !F), value).unwrap() }
        else if int { memory.w32(ARGV0 + offset, value as u32).unwrap() } else { memory.w64(ARGV0 + offset, value).unwrap() }
    }
}
/// Spread-out sample so exhaustive pools are not dominated by their first entries.
fn sample(n: usize, count: usize) -> Vec<Vec<u64>> {
    let all = tuples(n, count);
    let step = (all.len() / count).max(1);
    all.into_iter().step_by(step).take(count).collect()
}

#[test]
fn nested_native_proof_accepts_real_captures_and_chooses_the_expected_shape() {
    for (name, argc, offsets, ssa, steps) in CASES {
        let p = verify(&code(name), ssa, offsets).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(p.len, code(name).len(), "{name}");
        assert_eq!(p.argc, argc, "{name}");
        let end = offsets.iter().zip(int_slots(ssa)).filter(|(&o, _)| o & F == 0).map(|(&o, int)| o + if int { 4 } else { 8 }).max().unwrap_or(4);
        assert_eq!(p.argv_size, end, "{name}");
        assert_eq!(p.fields.is_empty(), !(offsets.iter().any(|&o| o & F != 0) || ssa.contains("\nst ")), "{name} fields {:?}", p.fields);
        match (&p.shape, steps) {
            (Shape::Simple(_), None) => {}
            (Shape::Program(g), Some(n)) => { assert_eq!(g.steps.len(), n, "{name}"); assert_eq!(g.inputs, offsets, "{name}"); }
            (shape, _) => panic!("{name}: unexpected shape {shape:?}"),
        }
        assert_eq!(p.masks.len(), (name == "neg_mul") as usize, "{name}");
        let pool_slots = match name { "scale" | "offset" | "half_sum" | "affine" | "half_int" => 1, _ => 0 };
        assert_eq!(p.statics.len(), pool_slots, "{name}");
        let integer_immediates = matches!(name, "affine" | "shift_scale" | "shared_const");
        assert_eq!(p.immediates.iter().any(|&b| b != 0), integer_immediates, "{name}");
        compile(&layout(), &abi(argc), &p).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn nested_proof_requires_the_exact_ordered_effects_and_operand_order() {
    for (name, _, offsets, ssa, _) in CASES {
        // Exhaustive single-byte mutation. Only the never-executed overflow call and its argv reload
        // are not decoded by the proof (SMC invalidation still covers them).
        let original = code(name); let skip = skipped(&original);
        for at in 0..original.len() {
            let mut bytes = original.clone(); bytes[at] = 0xcc;
            let accepted = verify(&bytes, ssa, offsets).is_ok();
            assert_eq!(accepted, skip.iter().any(|r| r.contains(&at)), "{name} byte {at}");
        }
        // Semantic mutations of the claimed SSA: swapped operands (even for commutative add/mul, whose
        // NaN payload selection depends on operand order) and every other operation kind.
        let lines: Vec<String> = ssa.lines().map(str::to_string).collect();
        for (n, line) in lines.iter().enumerate() {
            let words: Vec<&str> = line.split_whitespace().collect();
            if words.len() != 5 || words[1] != "=" { continue; } // "<name> = <op> <a> <b>"
            let with = |text: String| { let mut v = lines.clone(); v[n] = text; v.join("\n") };
            if words[3] != words[4] {
                let swapped = with(format!("{} = {} {} {}", words[0], words[2], words[4], words[3]));
                assert!(verify(&code(name), &swapped, offsets).is_err(), "{name} swapped line {n}");
            }
            for other in ["addd", "subd", "muld", "divd"].into_iter().filter(|o| *o != words[2]) {
                let changed = with(format!("{} = {} {} {}", words[0], other, words[3], words[4]));
                assert!(verify(&code(name), &changed, offsets).is_err(), "{name} {other} line {n}");
            }
        }
    }
    // A claimed constant must equal the native bits exactly, in a pool slot or a code immediate.
    for (name, _, offsets, ssa, _) in CASES.iter().filter(|c| c.3.contains("immd")) {
        for line in ssa.lines().filter(|l| l.contains("immd bits:")) {
            let bits = u64::from_str_radix(line.rsplit("bits:").next().unwrap(), 16).unwrap();
            for flipped in [bits ^ 1, bits ^ (1 << 63), bits ^ (1 << 52), 0, 0x7ff8000000000000] {
                let changed = ssa.replace(&format!("bits:{bits:016x}"), &format!("bits:{flipped:016x}"));
                assert!(verify(&code(name), &changed, offsets).is_err(), "{name} constant {bits:x} -> {flipped:x}");
            }
        }
    }
    // A declared-but-never-used constant is not bound to any native read and is refused.
    let unused = CASES.iter().find(|c| c.0 == "scale").unwrap();
    assert!(verify(&code("scale"), &unused.3.replace("retd r", "k2 = immd bits:3ff0000000000000\nretd r"), unused.2).is_err());
    // A claimed program with an extra/missing/dead operation or a wrong parameter binding.
    let native = code("mul_add");
    let three = |body: &str| format!("params f64 f64 f64\na = argd 0\nb = argd 1\nc = argd 2\n{body}");
    for (body, why) in [
        ("p = muld a b\nr = addd p c\nz = addd a b\nretd r", "dead SSE op"),
        ("p = muld a b\nq = addd p c\nr = addd q c\nretd r", "extra op"),
        ("p = muld a b\nretd p", "missing op"),
        ("p = muld a b\nr = addd c p\nretd r", "swapped addend"),
        ("p = muld b a\nr = addd p c\nretd r", "swapped factors"),
        ("p = muld a c\nr = addd p b\nretd r", "wrong operand"),
    ] { assert!(verify(&native, &three(body), &[4, 12, 20]).is_err(), "{why}"); }
    for offsets in [[4, 20, 12], [12, 4, 20], [4, 12, 28]] {
        assert!(verify(&native, &three("p = muld a b\nr = addd p c\nretd r"), &offsets).is_err(), "{offsets:?}");
    }
    // Deep/long programs beyond the bound are refused before any native comparison.
    let long = "params f64\na = argd 0\np = addd a a\nq = addd p p\nr = addd q q\ns = addd r r\nt = addd s s\nu = addd t t\nretd u";
    assert!(verify(&native, long, &[4]).is_err());
    let five = "params f64 f64 f64 f64 f64\na = argd 0\nb = argd 1\nc = argd 2\nd = argd 3\ne = argd 4\ns = addd a b\nr = addd s c\nretd r";
    assert!(verify(&native, five, &[4, 12, 20, 28, 36]).is_err());
}

#[test]
fn nested_program_encoding_and_adapter_bounds_are_strict() {
    let p = proof("distance2");
    let Shape::Program(g) = &p.shape else { panic!() };
    assert_eq!(g.pack() & 7, 5);
    // Malformed programs that cannot come from the verifier are refused by compile().
    let mutate = |f: &dyn Fn(&mut Program)| {
        let mut q = p.clone(); let Shape::Program(h) = &mut q.shape else { unreachable!() }; f(h);
        compile(&layout(), &abi(4), &q)
    };
    assert!(mutate(&|_| {}).is_ok());
    assert!(mutate(&|h| h.steps.clear()).is_err());
    assert!(mutate(&|h| { let s = h.steps[0].clone(); h.steps.extend(std::iter::repeat(s).take(MAX_STEPS)); }).is_err());
    assert!(mutate(&|h| h.steps[0].kind = 0).is_err());
    assert!(mutate(&|h| h.steps[0].kind = 6).is_err());
    assert!(mutate(&|h| h.steps[0].a = STEP0 as u32).is_err(), "step may not read its own result");
    assert!(mutate(&|h| h.steps[1].a = (STEP0 + 3) as u32).is_err(), "forward reference");
    assert!(mutate(&|h| h.steps[0].b = STEP0 as u32 + 9).is_err());
    assert!(mutate(&|h| h.steps[0].a = 4 + 1).is_err());
    assert!(mutate(&|h| h.inputs.push(36)).is_err());
    assert!(mutate(&|h| h.inputs[0] = 5).is_err());
    assert!(mutate(&|h| h.inputs[0] = 12 + 28).is_err());
    assert!(mutate(&|h| h.inputs.clear()).is_err());
    // Negation must be unary.
    let neg = proof("neg_mul"); let mut bad = neg.clone();
    let Shape::Program(h) = &mut bad.shape else { panic!() }; assert_eq!(h.steps[1].kind, 1); h.steps[1].b = 0;
    assert!(compile(&layout(), &abi(2), &bad).is_err());
    for argc in [0, 1, 2, 3, 5, 8, 9] { assert!(compile(&layout(), &abi(argc), &p).is_err(), "argc {argc}"); }
}

#[test]
fn nested_helper_matches_actual_sse_and_fld_including_payloads_flags_and_x87_state() {
    let mut memory = GuestMemory::new(SIZE as usize); memory.set_prot(0, SIZE, P_R | P_W | P_X);
    let (mut compared, mut expected_cases, mut int_accepted, mut int_refused) = (0usize, 0usize, 0usize, 0usize);
    for (name, _, offsets, ssa, _) in CASES {
        expected_cases += MXCSR.len() * tuples(offsets.len(), 1500).len();
        let bytes = code(name); let p = verify(&bytes, ssa, offsets).unwrap();
        let (_, start) = fast_path(&bytes);
        memory.raw_mut()[ENTRY as usize..ENTRY as usize + bytes.len()].copy_from_slice(&bytes);
        memory.w128(MASK, mask()).unwrap();
        for (at, v) in [(SP0, 0x500), (SP0 + 4, 0x1234), (SP0 + 12, ARGV0)] { memory.w32(at, v).unwrap(); }
        for (i, (_, bits)) in CAPTURED_POOL.iter().enumerate() { memory.w64(CONST_POOL + 8 * i as u32, *bits).unwrap(); }
        for &mxcsr in &MXCSR {
            for inputs in tuples(offsets.len(), 1500) {
                put_memory(&mut memory, ssa, offsets, &inputs);
                let pre_object = memory.raw()[OBJ as usize..OBJ as usize + 0x80].to_vec();
                let mut expected = initial(); expected.mxcsr = mxcsr;
                expected.r[EAX] = ARGV0; expected.r[ECX] = ARGV0; expected.r[EDX] = ARGV0; expected.r[crate::cpu::EBP] = SP0 - 4;
                let int = is_int_result(ssa);
                let stop = if int { 0 } else { last_x87(&bytes, start) };
                let mut at = ENTRY as usize + start; let (mut ip, mut dp) = (0, 0);
                loop {
                    let mut decoder = Decoder::with_ip(32, &memory.raw()[at..], at as u64, DecoderOptions::NONE);
                    let i = decoder.decode();
                    let last = if int { i.mnemonic() == M::Cmp } else { i.ip32() == stop };
                    if last && !int { ip = i.ip32(); dp = expected.ea(&i).unwrap(); }
                    expected.exec(&mut memory, &i).unwrap();
                    at += i.len();
                    // x87 results end at the last x87 instruction; Std.int ends at the range check on EAX
                    if last { break; }
                }
                let mut actual = initial(); actual.mxcsr = mxcsr;
                let status = helper(&mut actual, &p, offsets, &inputs, ip, dp);
                if int {
                    // The native range check sends 0x80000000 to a helper call: the oracle (and adapter) refuse exactly then
                    // or earlier (non-normal operand); whenever they accept, EAX and every flag must match the interpreter.
                    let indefinite = expected.r[EAX] == 0x8000_0000;
                    if status != 0 { compared += 1; int_refused += 1; continue; }
                    int_accepted += 1;
                    assert!(!indefinite, "{name} {inputs:x?}");
                    assert_eq!(actual.r[EAX], expected.r[EAX], "{name} mxcsr={mxcsr:x} inputs={inputs:x?}");
                    assert_eq!(actual.mxcsr, expected.mxcsr, "{name} mxcsr={mxcsr:x} inputs={inputs:x?}");
                    expected.r = actual.r; expected.xmm = actual.xmm; expected.fl = actual.fl;
                } else {
                    assert_eq!(status, 0, "{name}");
                    expected.r = actual.r; expected.xmm = actual.xmm;
                    // receiver field stores: the interpreter's object bytes must equal the oracle's stores applied
                    // (FLD/FST round-trips a signaling NaN to a quiet one and the adapter refuses non-normal values,
                    // so only normal and zero stores are compared.)
                    let writes = expected_stores(&p, offsets, &inputs, ssa, mxcsr);
                    let normal = |b: u64| (b >> 52) & 0x7ff != 0x7ff && ((b >> 52) & 0x7ff != 0 || b << 1 == 0);
                    if writes.iter().all(|&(_, b)| normal(b)) {
                        let mut want = pre_object.clone();
                        for (at, bits) in writes { want[(at - OBJ) as usize..(at - OBJ) as usize + 8].copy_from_slice(&bits.to_le_bytes()); }
                        assert_eq!(&memory.raw()[OBJ as usize..OBJ as usize + 0x80], &want[..], "{name} object {inputs:x?}");
                    }
                }
                assert_eq!(encode(&actual), encode(&expected), "{name} mxcsr={mxcsr:x} inputs={inputs:x?}");
                compared += 1;
            }
        }
    }
    assert_eq!(compared, expected_cases);
    assert!(int_accepted > 1000 && int_refused > 1000, "Std.int coverage: accepted={int_accepted} refused={int_refused}");
    assert!(compared > 40_000, "differential coverage collapsed: {compared}");
}

#[test]
fn nested_adapter_executes_the_complete_native_return_and_preserves_caller_state() {
    for (name, argc, offsets, ssa, _) in CASES {
        let bytes = code(name); let p = verify(&bytes, ssa, offsets).unwrap();
        for inputs in ordinary_in(offsets.len(), 60, if is_int_result(ssa) { 8 } else { 20 }) {
            let mut rig = build(&bytes, &p, argc);
            put(&mut rig, ssa, offsets, &inputs);
            let mut c = initial(); c.lf_kind = 3; c.lf_a = 15; c.lf_b = 4; c.lf_r = 11;
            rig.set_cpu(&c); let mut before = rig.guest_mut().to_vec();
            let dp = data_pointer(&p);
            assert_eq!(helper(&mut c, &p, offsets, &inputs, p.fld_ip, dp), 0);
            c.r[ESP] += 4; c.eip = 0x500; c.jit_budget -= p.instructions as i32; c.jit_lir_completed += 1;
            apply(&mut before, &expected_stores(&p, offsets, &inputs, ssa, 0x1fa0));
            assert_eq!(rig.call(0), S_NEXT, "{name}");
            assert_eq!(encode(&rig.cpu()), encode(&c), "{name} {inputs:x?}");
            assert_eq!(rig.guest_mut(), before, "{name}");
        }
    }
}

#[test]
fn nested_complete_native_path_matches_the_adapter_except_abi_volatile_and_dead_spills() {
    for (name, argc, offsets, ssa, _) in CASES {
        let bytes = code(name); let p = verify(&bytes, ssa, offsets).unwrap();
        for inputs in ordinary_in(offsets.len(), 40, if is_int_result(ssa) { 8 } else { 20 }) {
            let mut rig = build(&bytes, &p, argc);
            put(&mut rig, ssa, offsets, &inputs);
            let mut memory = GuestMemory::new(SIZE as usize); memory.set_prot(0, SIZE, P_R | P_W | P_X);
            memory.raw_mut().copy_from_slice(rig.guest_mut());
            let mut expected = initial(); let before = encode(&expected); let mut count = 0;
            while expected.eip != 0x500 {
                let ip = expected.eip as usize;
                let mut decoder = Decoder::with_ip(32, &memory.raw()[ip..], ip as u64, DecoderOptions::NONE);
                let i = decoder.decode(); expected.eip = i.next_ip32(); expected.exec(&mut memory, &i).unwrap();
                count += 1; assert!(count < 128);
            }
            assert_eq!(count, p.instructions, "{name}");
            assert_eq!(rig.call(0), S_NEXT, "{name}"); let actual = rig.cpu();
            for r in [ECX, EDX] { expected.r[r] = actual.r[r]; }
            if !is_int_result(ssa) { expected.r[EAX] = actual.r[EAX]; }   // Std.int returns in EAX: compared exactly
            expected.xmm = actual.xmm; expected.fl = actual.fl;
            expected.jit_budget = actual.jit_budget; expected.jit_lir_completed = actual.jit_lir_completed;
            assert_eq!(encode(&expected), encode(&actual), "{name} {inputs:x?}");
            assert_ne!(before, encode(&actual));
            let original = rig.guest_mut();
            let low = (SP0 as i32 + p.stack_low) as usize;
            assert_eq!(&memory.raw()[..low], &original[..low], "{name}");
            assert_eq!(&memory.raw()[SP0 as usize..], &original[SP0 as usize..], "{name}");
        }
    }
}

#[test]
fn nested_adapter_refusals_are_transactional() {
    for (name, argc, offsets, ssa, _) in CASES {
        let bytes = code(name); let p = verify(&bytes, ssa, offsets).unwrap();
        let has_stores = matches!(&p.shape, Shape::Program(g) if !g.stores.is_empty());
        for case in 0..39 {
            let mut rig = build(&bytes, &p, argc); let mut c = rig.cpu();
            match case {
                0 => c.eip += 1,
                1 => {}
                2 => c.jit_budget = p.instructions as i32 - 1,
                3 => c.jit_budget = -1,
                4 => c.r[ESP] = 12,
                5 => c.r[ESP] = u32::MAX - 3,
                6 => c.r[ESP] = 0x8000,
                7 => rig.permission(RTAB_BASE, SP0 >> 12, 0),
                8 => rig.permission(WTAB_BASE, SP0 >> 12, 0),
                9 => rig.permission(RTAB_BASE, CORE >> 12, 0),
                10 => rig.permission(WTAB_BASE, CORE >> 12, 0),
                11 => rig.word(CORE + 32, SP0 - 19),
                12 => rig.word(SP0 + 8, argc + 1),
                13 => rig.word(SP0 + 12, SP0),
                14 => rig.word(SP0 + 12, u32::MAX - 4),
                15 => rig.word(SP0 + 12, 0xfffd),
                // Highest input slot straddles into a non-readable page.
                16 => rig.word(SP0 + 12, 0xa000 - p.argv_size + 1),
                17 => rig.permission(RTAB_BASE, ARGV0 >> 12, 0),
                18 if is_int_result(ssa) => c.mxcsr = 0x1f80,          // PE not yet sticky: not the fast configuration
                19 if is_int_result(ssa) => c.mxcsr = 0x1fe0,          // DAZ
                18 => c.fpu.top = 8,
                19 => c.fpu.tag &= !(3 << (2 * 2)),
                20 => c.r[ESP] = CORE + 60,
                21 => rig.permission(RTAB_BASE, ARGV0 >> 12, 4),
                22 => rig.permission(WTAB_BASE, SP0 >> 12, 2),
                23 if name == "neg_mul" => rig.guest_mut()[MASK as usize + 7] = 0,
                24 if name == "neg_mul" => rig.permission(RTAB_BASE, MASK >> 12, 0),
                25 if name == "neg_mul" => c.r[ESP] = MASK + 32,
                26 if name == "neg_mul" => rig.guest_mut()[MASK as usize + 15] ^= 1,
                // constant-pool slot: changed bits, unreadable page, or aliasing the omitted frame writes
                27 if !p.statics.is_empty() => { let at = p.statics[0].0; rig.double(at, p.statics[0].1 ^ 1); }
                28 if !p.statics.is_empty() => rig.permission(RTAB_BASE, CONST_POOL >> 12, 0),
                29 if !p.statics.is_empty() => c.r[ESP] = CONST_POOL + 32,
                30 => c.mxcsr = 0x1f80,                                   // PE not yet sticky
                31 => c.mxcsr = 0x7fa0,                                   // round toward zero
                32 if is_int_result(ssa) => {                              // 2^31 * 1.0: native takes the helper call
                    let mut inputs = vec![0x3ff0000000000000u64; offsets.len()]; inputs[0] = 0x41e0000000000000;
                    put(&mut rig, ssa, offsets, &inputs);
                }
                // receiver object: wild pointer, aliasing argv, aliasing the omitted frame writes, unreadable,
                // unwritable (stores), or a field straddling a page
                33 if !p.fields.is_empty() => rig.word(ARGV0, u32::MAX - 4),
                34 if !p.fields.is_empty() => rig.word(ARGV0, ARGV0 - p.fields[0]),
                35 if !p.fields.is_empty() => rig.word(ARGV0, SP0 - 0x80),
                36 if !p.fields.is_empty() => rig.permission(RTAB_BASE, OBJ >> 12, 0),
                37 if has_stores => rig.permission(WTAB_BASE, OBJ >> 12, 0),
                38 if !p.fields.is_empty() => rig.word(ARGV0, OBJ + 0xfff - p.fields.last().unwrap()),
                _ => c.jit_budget = 0,
            }
            rig.set_cpu(&c); let before = rig.image();
            assert_eq!(rig.call(if case == 1 { 1 } else { 0 }), S_RETRY, "{name} case={case}");
            assert_eq!(rig.image(), before, "{name} case={case}");
        }
    }
}
const RTAB_BASE: u32 = super::tests::RTAB;
const WTAB_BASE: u32 = super::tests::WTAB;

#[test]
fn nested_helper_refuses_malformed_programs_and_occupied_x87_without_mutation() {
    let good = proof("mul_add"); let Shape::Program(g) = &good.shape else { panic!() };
    let program = g.pack();
    let attempts: [(u64, &str); 9] = [
        (0, "no steps"), (6, "too many steps"), (program | (1 << 63), "trailing bits"),
        (program & !(7 << 3) | (0 << 3), "kind zero"), (program & !(7 << 3) | (6 << 3), "kind six"),
        (program & !(15 << 6) | (15 << 6), "forward operand a"), (program & !(15 << 10) | (15 << 10), "forward operand b"),
        ((program & !(15 << 6)) | ((STEP0 as u64) << 6), "step reads own result"),
        (1 | (1 | 0 << 3 | 1 << 7) << 3, "neg with different operands"),
    ];
    for (bad, why) in attempts {
        let mut c = initial(); let before = encode(&c);
        assert_eq!(native_expression(&mut c, [1, 2, 3, 4], [0; MAX_CONSTS], bad, STEP0 + 1, 3, 4), S_RETRY, "{why}");
        assert_eq!(encode(&c), before, "{why}");
    }
    for (top, tag, why) in [(8u8, 0xffffu16, "invalid TOP"), (3, 0xff3f & !(3 << 4), "occupied destination")] {
        let mut c = initial(); c.fpu.top = top; c.fpu.tag = tag; let before = encode(&c);
        assert_eq!(native_expression(&mut c, [1, 2, 3, 4], [0; MAX_CONSTS], program, STEP0 + 1, 3, 4), S_RETRY, "{why}");
        assert_eq!(encode(&c), before, "{why}");
    }
    // Every legal top with an empty destination commits exactly one push.
    for top in 0..8u8 {
        let mut c = initial(); c.fpu.top = top; c.fpu.tag = 0xffff;
        assert_eq!(native_expression(&mut c, [0x3ff0000000000000; 4], [0; MAX_CONSTS], program, STEP0 + 1, 10, 20), 0);
        assert_eq!(c.fpu.top, top.wrapping_sub(1) & 7);
        assert_eq!(c.fpu.st[c.fpu.top as usize], crate::fpu::F80 { mant: 0x8000000000000000, se: 0x4000 }, "1*1+1 = 2");
        assert_eq!(c.fpu.last_ip, 10); assert_eq!(c.fpu.last_dp, 20);
    }
}

#[test]
fn nested_sticky_flags_accumulate_across_steps_like_consecutive_native_instructions() {
    // 1/3 is inexact (PE) while the following exact add raises nothing new; the flag must survive
    // and a later invalid operation must add IE on top of it, in both orders.
    let program = Program { inputs: vec![4, 12, 20], ints: vec![false; 3], consts: vec![], result: (STEP0 + 1) as u32, stores: vec![], steps: vec![
        Step { kind: 5, a: 0, b: 1 }, Step { kind: 2, a: STEP0 as u32, b: 2 }] };
    let one = 0x3ff0000000000000u64; let three = 0x4008000000000000u64; let snan = 0x7ff0000000001234u64;
    for (inputs, flags) in [([one, three, 0], 0x20u32), ([one, three, snan], 0x21), ([snan, three, 0], 0x01)] {
        let mut c = initial(); c.mxcsr = 0x1f80;
        assert_eq!(native_expression(&mut c, [inputs[0], inputs[1], inputs[2], 0], [0; MAX_CONSTS], program.pack(), STEP0 + 1, 1, 2), 0);
        assert_eq!(c.mxcsr & 0x3f, flags, "{inputs:x?}");
    }
}

#[test]
fn nested_adapter_is_exact_or_refuses_for_every_edge_value_and_mxcsr() {
    let (mut done, mut refused) = (0usize, 0usize);
    for (name, argc, offsets, ssa, _) in CASES {
        let bytes = code(name); let p = verify(&bytes, ssa, offsets).unwrap();
        let dp = data_pointer(&p);
        let spread = if is_int_result(ssa) { 8 } else { 20 };
        for &mxcsr in &[0x1f80u32, 0x1fa0, 0x3fa0, 0x5fa0, 0x7fa0, 0x9fe0, 0xffff] {
            for inputs in sample(offsets.len(), 400).into_iter().chain(ordinary_in(offsets.len(), 100, spread)) {
                let mut rig = build(&bytes, &p, argc);
                put(&mut rig, ssa, offsets, &inputs);
                let mut c = initial(); c.mxcsr = mxcsr; rig.set_cpu(&c);
                let before = rig.image();
                let mut want_guest = rig.guest_mut().to_vec();
                match rig.call(0) {
                    S_RETRY => { refused += 1; assert_eq!(rig.image(), before, "{name} mxcsr={mxcsr:x} {inputs:x?}"); }
                    S_NEXT => {
                        done += 1;
                        // every guest write must be exactly the program's stores, nothing else
                        apply(&mut want_guest, &expected_stores(&p, offsets, &inputs, ssa, mxcsr));
                        assert_eq!(rig.guest_mut(), want_guest, "{name} mxcsr={mxcsr:x} {inputs:x?}");
                        let mut expected = c.clone();
                        assert_eq!(helper(&mut expected, &p, offsets, &inputs, p.fld_ip, dp), 0);
                        expected.r[ESP] += 4; expected.eip = 0x500; expected.jit_budget -= p.instructions as i32; expected.jit_lir_completed += 1;
                        assert_eq!(encode(&rig.cpu()), encode(&expected), "{name} mxcsr={mxcsr:x} {inputs:x?}");
                    }
                    other => panic!("unexpected status {other}"),
                }
            }
        }
    }
    assert!(done > 1500 && refused > 2000, "done={done} refused={refused}");
}

#[test]
fn nested_adapter_tailchains_only_to_a_live_matching_caller_after_every_effect_is_committed() {
    use crate::jit_x86::CACHE_ENTRIES;
    let mut l = layout(); l.cache = super::tests::GUEST + SIZE;
    // Direct-mapped entry for return address 0x500: [eip, table slot + 1, head, unused].
    let entry = (l.cache + ((0x500u32 ^ (0x500u32 >> 12)) & (CACHE_ENTRIES - 1)) * 16) as usize;
    for name in ["add_ac", "mul_add", "neg_mul", "distance2", "num_to_int", "int_plus_num", "f_get_x", "f_move_by", "f_set_x"] {
        let (_, argc, offsets, ssa, _) = *CASES.iter().find(|c| c.0 == name).unwrap();
        let bytes = code(name); let p = verify(&bytes, ssa, offsets).unwrap();
        let inputs: Vec<u64> = (0..offsets.len()).map(|i| (1.25f64 + i as f64).to_bits()).collect();
        for case in 0..5 {
            let mut rig = build_with(&bytes, &p, &abi(argc), &l);
            put(&mut rig, ssa, offsets, &inputs);
            for (at, value) in [(0, if case == 1 { 0x501 } else { 0x500 }), (4, if case == 2 { 0 } else { 1 }), (8, 123)] {
                rig.raw_memory()[entry + at..entry + at + 4].copy_from_slice(&u32::to_le_bytes(value));
            }
            let mut c = rig.cpu();
            if case == 3 { c.jit_budget = p.instructions as i32; }     // success leaves zero budget: must not chain
            if case == 4 { c.jit_budget = p.instructions as i32 + 1; } // exactly one unit left: chains
            rig.set_cpu(&c);
            let mut reference = c.clone();
            assert_eq!(helper(&mut reference, &p, offsets, &inputs, p.fld_ip, data_pointer(&p)), 0);
            reference.r[ESP] += 4; reference.eip = 0x500; reference.jit_budget -= p.instructions as i32; reference.jit_lir_completed += 1;
            let mut want = rig.guest_mut().to_vec();
            apply(&mut want, &expected_stores(&p, offsets, &inputs, ssa, 0x1fa0));
            let chained = matches!(case, 0 | 4);
            assert_eq!(rig.call(0), if chained { 123 } else { S_NEXT }, "{name} case {case}");
            // The chained exit commits exactly what the dispatcher-return exit commits.
            assert_eq!(encode(&rig.cpu()), encode(&reference), "{name} case {case}");
            assert_eq!(rig.guest_mut(), want, "{name} case {case}");
        }
    }
}
