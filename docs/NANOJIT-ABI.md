# Flash 32.0.0.363 NanoJIT capture / installation ABI

## Scope and status

Static investigation of **only** `vendor-private/libpepflashplayer-32.0.0.363.so`,
15,441,580 bytes, SHA-256
`e357ccdc5f1bf81b89eaf10a6071a69bef5f758c9511a69586157cd3b72ea9f8`.
ELF32 little-endian i386 ET_DYN. No plugin execution was performed in this pass.
Names below are **inferred source correspondences**; addresses, instructions,
field accesses, and call edges are **observed**. This is not a claim that the
proprietary build equals the public source commit.

**Actionable result:** capture at ELF `0x174002`, immediately before the call to
`Assembler::assemble`, and correlate installation at ELF `0x18a224`. Do not hook
an assumed `Assembler::compile`: the actual Flash emitter calls
`beginAssembly → assemble → endAssembly` directly.

## Coordinates (do not mix these)

**Dynamic follow-up:** the emulator now has exact-byte-bound, non-mutating entry probes at
the three observation sites. `web/nanojit-capture.js` checks the full runtime SHA-256 and object
invariants, captures bounded raw records before assembly, and correlates fragment entry with
setJit's actual argument. A `tests/swf/bench.swf` run captured 32 methods with zero rejections.
The live frontend additionally recognizes one audited pure SUB leaf and emits a native-ABI wasm
region through `lir::Trace::compile_region`; a real run recorded 4,372 Rust dispatcher entries
into that replacement, with no crash. `tools/flash-accelerators-test.mjs` also checks a real AS3
arithmetic/overflow self-test in native, LIR, and LIR+AOT modes using its rendered pass pixel.
These facts supersede the earlier pass's “not run” statements below, **only for this narrow leaf**.
The AS3 fixture's completion counter subsequently confirmed 200,002 successful wasm returns,
not just entries that could have fallen back to native code. Its packaged Chrome extension test
also returned `PASS:2115098112` over the real ExternalInterface bridge with AOT and LIR active.
General numeric opcode handlers, memory/call semantics, exceptions and OSR remain unresolved.

**Integer expansion:** the current frontend no longer requires the 78-byte SUB template. It
extracts a bounded pure i32 return slice (including a single forward return diamond), then
`lir/pure.rs` symbolically executes the actual native no-overflow path(s). Installation requires
identical integer result expressions, restored MethodFrame/callee saves, and no unrecognized
heap/call effects. This independent native proof is necessary because a plausible public
opcode number is not sufficient evidence. The arithmetic integration SWF now replaces 16
methods: add/sub/mul, and/or/xor, shifts, neg/not, composed expressions and conditionals.
Loops, arbitrary loads/stores/calls, float state, exceptions and OSR are still not lowered.
The general-loop exclusion remains, but two exact captured counted-loop streams now have a
separate bounded decoder/adapter (`lir/counted.rs`): sum and xorshift, max 4096 trips, zero active
interrupt poll, complete-call budget and checked memory/alias guards. It verifies every raw word
and native no-poll path, including padding/blinding variations; unsupported native allocations
still fail closed. Chrome's real plugin test confirmed both loop entries and correct AS3 results.
Reproducible A/B harness: `node tools/bench-flash-lir.mjs --workload loops --repeats 30 --runs 3`.
The latest exact-padding-aware run installed both profiles in all samples and measured medians
90 ms native versus 81 ms lowered; earlier profile misses regressed, so coverage is retained in
each benchmark sample and the improvement is not generalized to a real game.

**Receiver-memory expansion:** actual captures from `LirMemory.swf` established raw `argv[0]`
receiver loads, word field reads at +84/+88, and integer stores with no guest calls/barriers on
the accepted native path. The generic symbolic verifier now models declared receiver word loads,
one staged write and read-after-write forwarding. It requires the native stored expression and
return to agree with LIR; omitted/dead reads, unknown stores and calls reject. The adapter checks
the full alias/protection contract before any commit. This is not authorization to omit GC write
barriers in other methods: those methods still require their native path.

All addresses in the following tables are **ELF virtual addresses**, before the
guest load bias `B`. Runtime EIP is `B + ELF_VA`. Ghidra imports this ET_DYN with
image base `0x10000`; its displayed addresses are `ELF_VA + 0x10000`.
This base was printed by the probe and independently corroborated by REA.
The first PT_LOAD has file offset/VA `0`, size `0xe18c73`, R-X, so all code and
the read-size table below have identical file offsets and ELF VAs. The second
PT_LOAD starts at file `0xe18fb0`, VA `0xe19fb0`: its file offset is **VA−0x1000**.
In particular the reader vtable is file `0xe32218`, ELF VA `0xe33218`, Ghidra
`0xe43218`. Never apply the code's identity mapping to this data segment.

## Located functions and anchors

| Correspondence / role | ELF VA | Ghidra | Evidence |
|---|---:|---:|---|
| `BaseExecMgr::verifyJit` | `0x18a508` | `0x19a508` | E1 caller; P1 pseudocode; error anchor below |
| `CodegenLIR::emitMD` | `0x173f02` | `0x183f02` | E1 complete function, 387 bytes |
| `Assembler` constructor | `0x1dfa8a` | `0x1efa8a` | E1 callee, allocation size `0x412c` |
| `Assembler::beginAssembly` | `0x1df30a` | `0x1ef30a` | E1 call; P1 pseudocode |
| `Assembler::assemble` | `0x1e1e60` | `0x1f1e60` | E1 call; P1 pseudocode |
| `Assembler::gen` | `0x1e0910` | `0x1f0910` | P2 reader-vcall / opcode dispatch |
| `Assembler::endAssembly` | `0x1de306` | `0x1ee306` | E1 call; P1 stores fragment entry/code |
| `BaseExecMgr::setJit` | `0x18a224` | `0x19a224` | P1; disassembly exact writes |
| `LirReader::read` | `0x16a878` | `0x17a878` | P2; disassembly controls EAX return |
| `LirReader::finalIns` | `0x16a8ba` | `0x17a8ba` | P2 returns `[this+0xc]` |
| `BaseExecMgr::invokeGeneric` | `0x189250` | `0x199250` | P2 plus disassembly |
| `BaseExecMgr::endCoerce` | `0x188380` | `0x198380` | disassembly call sites below |

E0's only relevant JIT error string is Ghidra `0xcc43c7` / file `0xcb43c7`,
`"JIT failed"`. The PIC anchor at ELF `0x18a5d3` is
`8d 83 d3 83 e2 ff` (`lea [ebx-0x1d7c2d],eax`). EBX's GOT base is `0xe8bff4`:
the thunk return `0x18a519 + 0xd01adb` establishes it. The error branch calls the
string allocator after `emitMD` returned zero, matching public `verifyJit`.
E0's bounded defined-string search did not find NanoJIT/Assembler diagnostic
names; that does not prove their byte strings are absent everywhere.

## Capture: exact state and bytes

The E1 body performs:

```text
173fc9  mov eax,[esi+8]             ; frag
173fcc  mov [esp+4],eax
173fd0  mov [esp],edi               ; assembler
173fd3  e8 32 b3 06 00              ; beginAssembly
173fd8  mov eax,[esi+8]
173fdb  mov edx,[eax+4]             ; lastIns
173fde  mov [ebp-0x24],0
173fe5  lea ecx,[ebx-0x58ddc]       ; reader vtable ELF 0xe33218
173feb  mov [ebp-0x28],ecx
173fee  mov [ebp-0x20],edx          ; reader._ins
173ff1  mov [ebp-0x1c],edx          ; reader._finalIns
173ff4  lea edx,[ebp-0x28]
173ff7  mov [esp+8],edx
173ffb  mov [esp+4],eax
173fff  mov [esp],edi
174002  e8 59 de 06 00              ; assemble
174007  mov eax,[esi+8]
17400a  mov [esp+4],eax
17400e  mov [esp],edi
174011  e8 f0 a2 06 00              ; endAssembly
174018  mov edx,[edi+0x78]          ; assembler error
17401b  test edx,edx
17401d  jne 174058
17401f  mov eax,[esi+8]
174022  mov eax,[eax+0x14]          ; returned method code
```

**Preferred observation hook:** before executing the `call` at `B+0x174002`.
At that point `[ESP]=assembler`, `[ESP+4]=frag`, `[ESP+8]=reader`, `ESI=CodegenLIR`,
`EDI=assembler`, `EAX=frag`. Preserve all guest state; do not emulate a return
from this call. Let the real assembler run for correctness/fallback.
At the callee entry `B+0x1e1e60`, the return address has been pushed: arguments
are now `[ESP+4]`, `[ESP+8]`, `[ESP+12]`, respectively.

Verified offsets, sufficient to capture without knowing `LirBuffer` internals:

| Object | Offset | Observed meaning |
|---|---:|---|
| CodegenLIR | `+0x8` | Fragment pointer |
| CodegenLIR | `+0x28` | MethodInfo pointer (observer call's method argument) |
| Fragment | `+0x4` | last LIns pointer |
| Fragment | `+0x10` | prologue/fragment entry written by endAssembly |
| Fragment | `+0x14` | installed native-code pointer returned by emitMD |
| LirReader | `+0x0` | vtable |
| LirReader | `+0x4` | null upstream-filter pointer at construction |
| LirReader | `+0x8` | next LIns pointer |
| LirReader | `+0xc` | final LIns pointer |
| Assembler | `+0x54` | codeList (passed to JIT observer) |
| Assembler | `+0x6c` | current native instruction pointer / emitted code start |
| Assembler | `+0x78` | error code, zero is success |

P1 `endAssembly` explicitly stores `frag+0x10=genPrologue()` and
`frag+0x14=assembler[0x6c]`; these two code pointers need not be interchangeable.
`Fragment.lirbuf=+0` is source-expected but **not independently dereferenced in
the inspected binary path**. No `LirBuffer` object layout is asserted here.

## Verified raw LIns traversal (not a scalar semantic decoder)

At ELF `0x16a878` the reader reads opcode **byte `[ins+3]`**, saves the original
pointer in EAX for return, subtracts one byte-table entry from the pointer, then
follows skip records until the opcode is not 3. Skip predecessor is **`u32[ins-4]`**.
The read-size table is at file/ELF `0xcb41c0`, Ghidra `0xcc41c0`.
Opcode 0 has step size 0: stop immediately at it, or traversal will never end.
Opcode 3 is a skip; valid reader outputs never expose it.

Reader vtable raw entries at ELF `0xe33218`:
`[0x16a85c,0x16b9e6,0x16a878,0x16a8ba]`. The first two entries are destructors;
**read is vtable+8**, not vtable+0. P2 `gen` invokes `[vtable+8]`, loads
`[returnedLIns+3]`, and returns on zero, corroborating this traversal.

The first 196 read-size bytes, one byte per opcode, are:

```text
00040408080108080108080808010808080808080808080c0c0c0c0c010c0c0c0c10101001101010100c0c010c0c0c0c0c0c1004010c0c0c0c08010c0814010c0c0c0c0c0c0c0c0c0101010101010101010c0c0c0c0c010c0c0c0c0c0c080c0c0c0c08080c0c0c0c0c0c01010101010101010808080c0c0c0c0c0808080c0c0c0c0808080c0c0c0c08080c0c0c0c0c0c0c0c08080c0c0c0c0c10011010100101010108080808080808080814080808080c0101101010101010010101010101040c040404
```

This sequence agrees with the public opcode table's representation sizes for
i386, including unsupported opcodes having size **1** (`sizeof(LInsNone)`),
`Safe` size 8, and the special start=0 reader override. That is structural
corroboration, **not proof of every opcode's semantics or operands**.
Source-corresponding examples are `parami=4`, `reti=7`, `retd=9`, `immi=0x39`,
`immd=0x3b`, `addi=0x5e`, `subi=0x5f`, `muli=0x60`, `addd=0x75`, `d2i=0xa8`.
Only `start=0`, `skip=3`, and the jtbl dispatch's opcode `0x32` were directly
corroborated semantically in the inspected binary code. Do not silently feed
the remaining numeric opcodes to `lir.rs` on this evidence alone.

Safe initial raw capture algorithm: use checked guest reads; keep a visited
address set and a hard instruction/byte cap; emit each address, opcode and
record bytes `[ins+4-size,ins+4)`; stop at start; follow skip pointers exactly
as the reader does. Reject out-of-bounds, unaligned, cyclic and size-1 records.
Capture before assembly mutates LIns register-allocation fields. Preserve all
referenced guest pages if later decoding needs operands outside the record.
Reverse the resulting instruction list only after traversal completes.

## Method installation and actual call ABI

`verifyJit` calls `emitMD` at ELF `0x18a587`, tests EAX, then calls `setJit` at
`0x18a59e` with `(execMgr, MethodInfo*, EAX)` on the stack. In `setJit`:

```text
18a237  mov esi,[ebp+0xc]           ; MethodInfo
18a24a  movzx eax,byte [esi+0x32]
18a24e  and al,0x7f
18a250  or al,0x20                 ; interp bit off, jit bit on
18a252  mov [esi+0x32],al
18a255  or byte [esi+0x34],1        ; apply fastpath
18a259  mov eax,[ebp+0x10]          ; native entry
18a25c  89 46 04                   ; MethodInfo._implGPR = native entry
18a262  call 1943b8                ; canCompileInvoker correspondence
18a277  mov [esi+8],edx            ; separate boxed-Atom invoker
```

This is an observation/correlation hook, **not permission to replace the method
pointer with a wasm table slot**. That field contains a guest x86 address.

Actual compiled-method calls are **i386 cdecl**, not fastcall. In `endCoerce`,
ELF `0x18839a` loads `MethodInfo = [MethodEnv+8]`; at `0x1883c1..0x1883cf` it
places `(MethodEnv*, argc, native_argv)` in `[ESP], [ESP+4], [ESP+8]` then does
`ff 50 04` / `call [MethodInfo+4]`. The normal result is consumed from **EAX**.
The number-return path repeats the call at `0x188462`, followed immediately by
`dd 5c 24 04` / `fstp qword [ESP+4]`: **double returns are x87 ST0**.
The no-return-traits path uses the same call at `0x1884cd`.

Thus a replacement bridge must keep ESP/return-address behavior, callee-saved
GPRs and the appropriate EAX/x87 result. It must distinguish native-argument
layout from boxed `Atom*` invoker input. Parameter representations, optional
arguments, receiver slot, exceptions and OSR still require per-method metadata
or captured LIR verification; three machine arguments are not three scalar AS3
arguments. Public CodegenLIR explicitly selects `ABI_CDECL` at line 1623,
consistent with these actual call sites. NanoJIT's generic ABI_FASTCALL default
is not evidence for the compiled Flash method's ABI.

## Offline / runtime validation boundary

1. Run `python3 tools/verify-nanojit-abi.py PATH_TO_PLUGIN` before enabling hooks;
   it fails closed on digest, hook bytes, pointer-table or read-size differences.
2. In the emulator, observe—not skip—`B+0x174002`. Log the method, fragment,
   assembler, reader, lastIns and raw records, bounded as above.
3. Pair with `B+0x174016` (just after endAssembly): error must be zero; log
   fragment `+0x14`. Pair again with setJit's third argument and verify equality.
4. Observe first dispatch into that exact guest code pointer: record the cdecl
   arguments, signature classification, result register and stack transition.
   Compare capture-disabled/enabled execution first. This pass has **not run**
   those dynamic tests or claimed captures from a real game.
5. Only install a scalar replacement after binary-verified opcode handlers and
   operand fields support the entire region, and its input/output ABI matches.
   Unsupported loads/stores/calls/loops are mandatory fallback; real Flash
   methods generally contain them, including prologue/runtime housekeeping.
   Extracting pure expression regions is not equivalent to compiling whole methods.

## Reproduction / evidence ledger

REA 6.5.0 CLI; Ghidra 12.1.4; Temurin Java 26.0.2.1. No MCP registration,
configuration edits, new software, plugin modification or execution required.

Let `P=vendor-private/libpepflashplayer-32.0.0.363.so` and
`T=<a scratch directory>`.

```sh
rea --help
rea providers --format json
rea search "$P" 'nanojit|LIR|Assembler|jit compilation|JIT' --mode regex --provider ghidra --snapshot "$T/nanojit-snapshot.json" --format json
rea function "$P" 0x183f02 --provider ghidra --snapshot "$T/nanojit-snapshot.json" --format json
objdump -d --start-address=0x173f02 --stop-address=0x174085 "$P"
objdump -d --start-address=0x16a878 --stop-address=0x16a8c5 "$P"
objdump -d --start-address=0x18a224 --stop-address=0x18a281 "$P"
objdump -d --start-address=0x188380 --stop-address=0x1884d8 "$P"
```

* **E0** search: `ev_b3efc412e2af20ada22442d3d70106aabfa3c7c87b66f0c769a45c68063e509c`,
  `$T/nanojit-search.json`.
* **E1** complete emitter dossier:
  `ev_144eb363a688b0d9be960541895121094f5883eeaf65ae63f33e26d18a489279`,
  `$T/nanojit-emitMD-evidence.json`. Includes complete assembly, pseudocode,
  callers/callees, references, body ranges and explicit unresolved indirect calls.
* **P1/P2** focused Ghidra CLI probes (not REA Evidence IDs):
  `$T/nanojit-ghidra-probe.log`, `$T/nanojit-ghidra-reader.log`.
  Script `$T/NanojitProbe.java` imports ELF with `-noanalysis`, disassembles and
  creates only the selected functions, then runs `DecompInterface`. Commands:
  `analyzeHeadless "$T" nanojit-probe -import ABSOLUTE_PLUGIN -noanalysis
  -scriptPath "$T" -postScript NanojitProbe.java 183f02 19a508 19a224 1ef30a
  1f1e60 1ee306 -deleteProject`; second probe arguments
  `17a85c 17b9e6 17a878 17a8ba 1f0910 199250 199d40`.
  The last address was an exploratory, unrelated function; no finding uses it.
  Noanalysis pseudocode cannot model unanalysed helper calling conventions;
  e.g. it omits `read`'s EAX return type, which the disassembly establishes.
* Historical reference only: [adobe/avmplus at 858d034a](https://github.com/adobe/avmplus/tree/858d034a3bd3a54d9b70909386435cf4aec81d21),
  `core/exec-jit.cpp:175–260`, `core/CodegenLIR.cpp:9094–9214`,
  `nanojit/LIR.h:2541–2604`, `nanojit/LIRopcode.tbl`, `nanojit/Assembler.cpp`.

**Unresolved:** full LirBuffer layout, binary-verified scalar operand/immediate
semantics, complete opcode handler mapping, CallInfo layout, native parameter
slot metadata, exception/deoptimization/OSR bridging, live hook counts and
end-to-end replacement execution in a real game. This report deliberately does not claim
the normalized scalar backend accepts a raw NanoJIT buffer.
