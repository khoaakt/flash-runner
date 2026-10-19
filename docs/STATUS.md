# Status

What works, what was measured and what is still open in the emulator, its JIT / LIR acceleration and the browser extension. Numbers come from
one Apple Silicon laptop (`web/flash-smoke.mjs` yields through a `MessageChannel` like the worker since a later change, so harness numbers taken after it are not directly comparable with the ones recorded here). Nothing here is a claim about frame rate in a particular game: no controlled measurement of a real game exists yet.
The rules for changing the code are in [AGENTS.md](../AGENTS.md); how the extension is distributed is in [DISTRIBUTION.md](DISTRIBUTION.md).

## Design decisions to keep

- The accelerators are tied to one runtime build: `vendor-private/libpepflashplayer-32.0.0.363.so`, SHA-256
  `e357ccdc5f1bf81b89eaf10a6071a69bef5f758c9511a69586157cd3b72ea9f8`. An unknown hash must keep original NanoJIT execution.
- Never commit the proprietary plugin, private keys, generated distributions, build outputs or core dumps.
- Keep Debian/glibc as the guest userland. Do not migrate to Alpine/musl.
- Keep synchronous wasm-region installation. An asynchronous experiment left about 21M pending hits and did not improve performance.
- Keep exact-range SMC invalidation, W^X park/unpark, and the 200 ms recompilation cooldown.
- Fail closed. An unsupported shape, stale bytes, ABI mismatch, unknown hash or unsafe memory condition must execute the original native method.

## LIR policy: what the shipped worker lowers

`web/lir-policy.js` decides which families (`integer`, `numbers`, `memory`, `loops`) the worker may lower.
Each flag is **derived from recorded A/B evidence in that file** (`LIR_EVIDENCE`): on when LIR beats native on the
family's primary workload (the typed call path real AS3 code uses). `web/lir-policy-test.mjs` keeps flags and
numbers consistent. Override per session with `m.lirFamilies` (worker message), or
`flash-smoke --lir --lir-families integer,numbers` / `tools/bench-flash-lir.mjs --families ...`. Tests and
`flash-smoke --lir` default to every family so correctness is proven regardless of the policy.

Measured on real Flash (alternating runs, guest ms, native vs LIR, 3,000,000 leaf calls unless noted):

| family / workload | native | LIR | speedup |
|---|---|---|---|
| integer: typed static Int leaves (`lir-int-bench`) | 95.5 | 74 | 1.29x |
| loops: sum/xorshift profiles (`lir-loops`, repeats=100) | 238 | 204 | 1.17x |
| numbers: nested programs (`lir-number-bench`) | 156 | 122 | 1.28x |
| numbers: single operation | 126 | 103 | 1.22x |
| numbers: constants | 129.5 | 107 | 1.21x |
| numbers: int32 -> Number | 141.5 | 96 | 1.47x |
| numbers: Number -> int (`Std.int`) | 105.5 | 82.5 | 1.28x |
| numbers: receiver fields (`lir-field-bench`) | 127 | 109.5 | 1.16x |
| memory: typed getters/setters (`lir-memory-bench`) | 221 | 178.5 | 1.24x |
| memory: calls through `Dynamic` closures (`lir-memory`) | 612 | 659 | **0.93x** |
| integer: loop-dominated `arithmetic` | 2084 | 2112 | 0.99x (ranges overlap) |

Two lessons. (1) The Number family was **0.39x** until two defects were fixed: its adapter returned `S_NEXT` to the
Rust dispatcher after every call (integer/memory/loop adapters already tail-chain to the cached caller region, and a
native leaf is copied into its caller so it never leaves the wasm function), and it called a cross-module soft-float
helper per SSE step. Now it tail-chains and does the FP and the x87 push inline on the interpreter's fast path.
`[jit] ... calls=` in `JIT_REPORT_FULL=1 node web/flash-smoke.mjs ...` shows the dispatcher entries per workload.
(2) A benchmark that routes every call through the AVM's generic invoker (`Dynamic`) measures the invoker, not the
leaf; the memory family loses 7% there and wins 24% on typed calls. The policy follows the typed result; the dynamic
number is recorded so a veto is informed. **No measurement from a real game exists.** Whether a real game's hot leaves look like
these workloads is unknown; see "Problems still open" 5.

**Refusals are charged to the LIR region and bounded.** Every adapter refuses transactionally (`S_RETRY`) and the
native method then runs, which costs about 200 ns per refused call (dispatcher bounce + interpreting one block). A
method whose arguments keep violating a guard (MXCSR without sticky PE, non-normal values, |x| >= 2^31, a full x87
stack, an unusable receiver) would pay that on every call: with every `Std.int` argument out of range LIR measured
**0.29x**. The dispatcher used to charge a refusal to the region it had entered, but a tail-chained adapter refuses with
`cpu.eip` still at its own entry, so the caller region was blamed (and dropped) and the adapter never was. Now
`JitState::blame_retry(entered, eip)` charges the LIR region that owns `eip`, and `lir_retry_limit` (4096) refusals retire
it (`lir_retired_for_refusing=` in `JIT_REPORT_FULL=1`); the native method is then translated normally. The same
workload now measures **0.94x**. A method that refuses only some of the time is retired once its total refusals reach
the limit, losing its speedup afterwards; the limit is a total, not a ratio (per-region completions are not tracked).

## Firefox

The extension builds for Firefox with `node tools/build-extension.mjs --firefox` (details in [DISTRIBUTION.md](DISTRIBUTION.md#firefox)). Measured with `tools/firefox-test.mjs` on Windows, Firefox 157.0.1,
geckodriver 0.37.1 (temporary add-on, headless): the add-on installs and gets the all-sites host permission; the player replaces the `<embed>`; the Flash runtime initialises and renders
`compat.swf` (`bitmap_ms=83` on one run, `urlloader`, sound channel and Shared Objects report ok); typing into an input TextField works (`input.swf`); Referer and Origin of the movie's
requests are those of the embedding page (the rewrite rule matches the extension's host, a per-install UUID in Firefox); the socket helper answers over Firefox native messaging.
The presenter is the 2D one (no WebGPU adapter in this headless run). Not checked: Stage3D, audio output, macOS or Linux, Firefox ESR, a signed install, performance against Chrome.

## Debug log setting (extension)

`log` (default `false`) in `extension/options/runtime-store.js`, switch "Debug log" under Troubleshooting in the options page
(`ui/src/options/Options.tsx`; the section is not called "Advanced" because `tools/ext-test.mjs` asserts the settings page has no
"Advanced" disclosure). **It is the only switch:** `logEnabled(settings)` is `settings.log`, and `diag`/`fpsLog` (still in
`chrome.storage.local`, no longer shown by the React options page) only take effect while it is on, so a profile that still
holds `diag: true` from the old settings page stays silent. `player.js`/`viewer.js` pass `log`, `profile` and `fpsLog` accordingly;
`web/loader.js` forwards `log` in the boot message and drops `log` messages when it is false; `web/worker.js` posts nothing and does
not decode the guest console. Pages that do not pass `log` (the web demo, `flash-smoke`) keep logging. Genuine error reports
(`console.error` for unhandled rejections, startup errors, socket-helper problems) are not part of it. `web/log-setting-test.mjs`
locks the default and the wiring; `EXT_TEST_LOGS=1 node tools/ext-test.mjs --page tests/ext/lir-leaf.html [--diag --fps-log] [--no-log]` (ext-test turns the Debug log on, because its checks read `[flash]` lines; `--no-log` tests the off state)
runs the built extension in real Chrome and prints every console line with the script that produced it (no `[flash]` line with `--no-log`, with or without
`--diag --fps-log`; a `url:`/`referrer:` pair from `chrome-extension://aifjj…/wtp.js` is another extension in the test
profile). In a user's Chrome only lines prefixed `[flash]` are ours.
`tools/ext-test.mjs` sets `log: true` itself. Rebuild the extension to ship it
(`node tools/build-extension.mjs`, then reload it in chrome://extensions and reload the game tab).

## Context menu text actions (Cut / Copy / Paste)

Reported: select text in a movie, right-click, Copy, and nothing reaches the clipboard. Reproduced and fixed in
`web/loader.js` (`showMenu`, the `contextmenu` handler); `web/context-menu-test.mjs` locks the wiring. Root causes:

1. Clicking a row of the HTML menu moved keyboard focus off the canvas; the blur was reported to Flash as lost focus, the text
   field dropped its selection, and `selected=2` (Copy) came back to Flash which then never called `PPB_Flash_Clipboard::WriteData`.
   The menu now swallows `mousedown` (no focus change) and returns focus to the movie after a choice.
2. Flash enables Paste only when `IsFormatAvailable(plaintext)` is true while the menu is built, and the worker knows the clipboard
   only after Ctrl+V or a copy made inside Flash. The `contextmenu` handler now reads `navigator.clipboard.readText()` and sends
   `clipboard-text` to the worker before it sends the context-menu event to Flash.

Proof in real Chrome (`tools/ext-test.mjs`; add `--spy-clipboard` to record every `navigator.clipboard.writeText` and its result,
`--menu-click LABEL` to click a menu item with real input events, `--drag x1,y1,x2,y2` to select with the mouse, `--clip TEXT` to put text
on the clipboard from outside Flash). Page coordinates are CSS pixels of `tests/ext/input.html` (the movie is scaled, the input is
about y 21-41):

```sh
R="--runtime vendor-private/libpepflashplayer-32.0.0.363.so --page tests/ext/input.html --seconds 8"
node tools/ext-test.mjs $R --clicks 60,30 --keys h,e,l,l,o,^a --rightclick 60,30 --menu-click Copy --spy-clipboard --expect-clipboard hello
node tools/ext-test.mjs $R --clicks 60,30 --keys h,e,l,l,o,^a --rightclick 60,30 --menu-click Cut  --spy-clipboard --expect-clipboard hello
node tools/ext-test.mjs $R --drag 23,61,105,61 --rightclick 60,61 --menu-click Copy --spy-clipboard --expect-clipboard "yped: (nothing)"
node tools/ext-test.mjs $R --clicks 60,30 --clip "from outside" --rightclick 60,30 --menu-click Paste     # screenshot: field shows the text
```

Before the fix the Copy run logged `clipboard calls => []`. ext-test now enables focus emulation for the page and the player frame
whenever it sends keys or a right click (a headless test frame is not focused), and turns the Debug log on by default
(`--no-log` to test the off state) because its checks read `[flash]` lines.

## What is actually working

### Integer LIR

`web/nanojit-leaf.js` proposes SSA. `crates/emu/src/lir/pure.rs` independently proves it against
the complete native no-overflow path. The replacement is emitted from SSA, never by translating
the x86 instruction sequence.

Verified coverage:

- arithmetic, shifts, comparisons, and one conditional selection;
- nested forward diamonds up to depth 4, including real `min`, `abs`, `clamp`, and `sign`;
- the arithmetic fixture lowered **20 of 21** methods. The remaining refusal is a real call and
  must stay native.

The verifier explores each branch arm from a cloned machine. It separately tracks decoded offsets
and path-visited offsets so shared join tails are decoded once but cannot be revisited as a loop.
A failing proof installs nothing.

### Receiver memory

`web/nanojit-memory.js`, `pure.rs`, and `lir/memory_region.rs` support bounded receiver-field
reads and up to eight ordered stores. Real Flash ran seven methods, including a two-field
exchange. Native disassembly showed field 88 stored before field 84, and the verifier preserves
that order. Transactional checks happen before any field is mutated.

### Counted loops

`web/nanojit-loop.js` and `lir/counted.rs` support only the captured sum and xorshift profiles.
They have poll, stack, budget, and SMC guards. This is not general loop support.

### Number / x87 returns

`web/nanojit-number.js` (proposes SSA from the captured LIR) and `lir/number.rs` (independently proves the native
bytes, then emits the adapter) run in the real capture pipeline. Verified shapes, all through the same adapter:

- **Arithmetic.** Identity, negation, add/sub/mul/div over double arguments; nested programs of at most 5 steps and 4
  inputs, with CSE-shared values (`distance2` is 4 inputs, 5 steps).
- **Constants.** At most 2 distinct. A pool slot (`movsd xmm,[static]`) is bound by address: the real guest bytes are read
  at install and rechecked on every call, and a slot read but unused is refused. An integer-valued constant
  (`mov r,imm` + `xorpd x,x` + `cvtsi2sd`) lives in code bytes the SMC binding already covers.
- **int32 arguments.** Alone or mixed with doubles (argv is packed from offset 4, int32 = 4 bytes, double = 8, no
  padding), through `cvtsi2sd` or `fild` of a spilled int. int32 -> double is exact, so no flag is involved.
- **`Std.int`.** `cvttsd2si` + the `cmp eax,0x80000000; jne` idiom over a helper call. The adapter returns EAX, no x87
  push, and refuses whenever |x| >= 2^31 (the only case native takes the call). The JS frontend recognizes the whole
  idiom in order (d2i, store slot, compare, branch, call, store, label, load, reti).
- **Receiver Number fields.** `mov eax,[argv]` is the receiver; `movq xmm,[eax+f]` / `fld [eax+f]` read a field;
  `movq [eax+f],xmm` / `fst qword [eax+f]` write one; a reload after a store yields the stored value (the verifier
  models memory, the JS frontend forwards it). Stores are written only after every guard and the whole computation
  succeeded. SSA carries stores as `st <field> <name>` lines (stripped and resolved in `install`).

**Adapter.** A self-contained wasm function with no host imports. It mirrors, guard for guard, the interpreter's host-FP
fast path that the emulator JIT already inlines (`jit_fp.rs`): MXCSR round-to-nearest, no FTZ/DAZ, PE already sticky;
operands normal or zero (divisor non-zero); results finite with exponent >= 2 or an exact zero that cannot be an
underflow. Anything else (including NaN/denormal/infinite operands, a full x87 stack, a stored non-normal double)
returns `S_RETRY` with nothing mutated and the native method runs. The `FLD m64` push is done inline. A successful return
tail-chains to the caller region (`layout.cache`). Receiver access reuses the memory family's invariants: the receiver
word is untrusted; every receiver+field span is in bounds and non-wrapping; the whole accessed object interval is apart
from the transient frame, MethodFrame and argv (and from every static slot/mask when anything is stored); every field
page is readable, and writable for stored fields (W^X: decoded-code pages are not writable).

**Proof.** The native flag-raising SSE sequence must equal the SSA effect sequence in order and with operand order (the
surviving NaN payload and sticky flags depend on it); a value nothing consumes is a dead effect and is refused; native
receiver stores must equal the proposed stores, in order. argc is the packed layout the code read (a gap must be whole
doubles and counts as unread parameters); a wrong guess only makes the runtime `[SP+8] == argc` guard refuse, since
nothing else depends on argc. `native_return` / `native_expression_values` are the exact soft-float oracle used by the
tests; they are `#[cfg(test)]`, not part of the runtime.

**Tests.** `lir/number_expr_tests.rs` + `lir/number_expr_data.rs` hold 31 native methods captured from the NanoJIT output for our own test SWFs (relocated), with
independent hand-written SSA: acceptance, exhaustive single-byte mutation of every proven byte (only never-decoded call
bytes may change), operand-swap/op-kind/constant-bit mutation of the SSA, a differential against the interpreter running
the real native bytes (>40,000 cases over six MXCSR values with signed zero, denormals, NaN payloads, infinities, int32
extremes, and object bytes after stores), full adapter execution, the complete native path vs the adapter (registers,
x87, memory), transactional refusals (guest, argv, receiver, MXCSR, range), exact-or-refuse over every edge value and
MXCSR, and the tail chain. Corrupting the field-store address or swapping subtraction operands in the oracle was
confirmed to fail several of them. Regenerate the data with `tools/gen-number-test-data.mjs` (see its header) and
disassemble captured bytes with `tools/dis`.

Fixtures (all finite and ordinary; edge values belong in the Rust tests, adding them to AS3 stops the movie painting):
`LirNumbers.hx` (6 methods, `numbers` block asserts exactly 6), `LirNumberExpr.hx` (24: three-argument singles, nested,
constants, int32 conversions, `Std.int`), `LirNumberFields.hx` (7). Haxe reserves `x`, `xt`, `xf` as guard keywords in the
LIR text format and `inline` as a Haxe keyword: do not use them as SSA/variable names.

Real adapter bugs fixed earlier that must not return: `ModuleBuilder::add_func` places i64 locals before i32 locals (the
adapter has 9 i64 value locals, so every i32 index is `2 + N_VALUES + k`); and a stack write can cross into the next page,
so permission is checked for every page the stack spans.

### AOT

The default package is the smaller fused direct-lowering tier: 59,044 blocks fused to 1,064
regions in 2 modules, 7.42% executable-byte coverage, with 22,365 budget frontiers. It installs
about 1,062 regions in roughly 100–115 ms. It is not a complete static recompilation.

The discovered-graph fallback compiled 2,605,012 non-HLE instructions and 4,948 regions, but used
about 2.24 GiB and about 419 ms binding. Keep it experimental. Do not make it the default without
a new correctness and performance measurement.

Portable AOT uses explicit symbolic relocation sites only. Never guess constants or retranslate
x86 during binding.

## Problems still open

1. **Real calls are not replaceable.** Captured call methods prepare MethodEnv, argc, argv, and have error/exception
   branches. Opcode records contain pointers, but CallInfo, GC roots, safepoints, and unwinding are not verified.
   `tests/swf-src/LirCalls.hx` only generates evidence; it does not authorize replacement. `Math.floor/sin/...` are such
   calls, which bounds how much Number code can ever be replaced.
2. **General loops remain native.** Only two exact profiles are supported. There is no OSR.
3. **Floating/int coverage still missing** (each needs a real capture first, then proof + differential test + fixture):
   compare-and-select (`min`/`max`: `ucomisd` + `jbe` + `fld`/`fstp st(0)`/`fld`, i.e. branch exploration plus x87 stack
   state); Boolean results from compares; integer arithmetic before a conversion (`(a + b) * 0.25` with Int `a`, `b`:
   `add eax,ecx` then `cvtsi2sd`; the Number verifier has no integer ALU); Number fields combined with the integer
   memory family (int fields next to Number fields in one method); receiver stores of `int`/pointer values from the
   Number adapter; more than 2 distinct constants or 5 steps. Wasm f64 semantics must not silently replace native
   MXCSR/x87 behavior: the adapter refuses anything outside the interpreter fast path.
4. **The original NanoJIT assembler still runs once** for every supported method. Replacement affects later execution only.
5. **No controlled profile of a real game exists.** Profiling one needs a page you are entitled to play (often with a
   session token only you have). Without it the families above are prioritized from synthetic fixtures. A census (which captured methods
   lowered, which refusal reasons dominate, how many calls reach each) from a real session would rank the missing shapes
   by actual call counts. Endpoint EIP samples are syscall-biased; do not revive the withdrawn `0x65631ba0` attribution.
6. **Memory on the dynamic-call path is 0.93x** (see the table): the generic invoker path, not the leaf, dominates and the
   adapter's guards are not free there. Unprofiled. If a game leans on `Function`/closure calls this matters.
7. **Capture cost grew.** `numberCaptureOpcode` now accepts calls, `std`, `eqi`, `reti` so the `Std.int` idiom and field
   stores can be captured; more methods are captured (up to 128 records each) and handed to the extractor before being
   refused. Bounded by the existing work limits, not measured on a large game. The `loweredWork` budget (1,000,000 records) is
   refunded for methods that end up refused, so the refused majority cannot starve real candidates; `before` (100,000
   methods seen) and `lowered` (512) are unchanged.
8. **Default AOT speedup is workload-dependent.** Do not broaden coverage merely to increase the percentage.

## Next implementation order

1. Rebuild wasm, run `node tools/flash-accelerators-test.mjs`, then inspect one new real capture before changing a
   verifier (`tools/dis` for the bytes, `node -e` over the capture JSON for the LIR records).
2. If a real session can be profiled: add the census described in problem 5 and let it rank the missing shapes.
   Otherwise take them in this order (cheapest, most common in game code first): compare-and-select and Boolean
   results; integer ALU before conversion; Number+int fields in one method.
3. For every new family: capture the native bytes, prove all SSE/x87/memory effects, add a Rust differential test against
   the interpreter and a finite AS3 fixture in its own file with its own block in `flash-accelerators-test.mjs`, then
   **measure** with `tools/bench-flash-lir.mjs --swf <fixture> --families <family>` (alternating runs; typed calls) and
   record the numbers in `web/lir-policy.js`. A family that loses stays implemented, tested and off.
4. For calls, disassemble the real method and identify CallInfo, arguments, roots, and exception edges before writing a
   lowering. If any of those is unverified, refuse the method.
5. Add general loops only with explicit safepoints and a bounded deoptimization path. Do not lower an unbounded loop
   directly to wasm.
6. Keep every expansion behind the existing fail-closed frontend. Measure each one separately.

## Verification commands

Everything except the first block needs the Flash runtime you supply in `vendor-private/`; building SWFs needs Haxe, and building
the wasm and guest needs the toolchain from the README. `cargo test -p emu --lib` needs none of them.

```sh
cargo test -p emu --lib
node web/nanojit-leaf-test.mjs
node web/nanojit-number-test.mjs
node web/lir-policy-test.mjs
sh tools/build-wasm.sh
haxe -cp tests/swf-src -main LirNumbers      -swf tests/swf/lir-numbers.swf       -swf-version 11.2
haxe -cp tests/swf-src -main LirNumberExpr   -swf tests/swf/lir-number-expr.swf   -swf-version 11.2
haxe -cp tests/swf-src -main LirNumberFields -swf tests/swf/lir-number-fields.swf -swf-version 11.2
haxe -cp tests/swf-src -main LirNumberBench  -swf tests/swf/lir-number-bench.swf  -swf-version 11.2   # -D const_ops|conv_ops|trunc_ops|single_op for variants
haxe -cp tests/swf-src -main LirFieldBench   -swf tests/swf/lir-field-bench.swf   -swf-version 11.2
haxe -cp tests/swf-src -main LirIntBench     -swf tests/swf/lir-int-bench.swf     -swf-version 11.2
haxe -cp tests/swf-src -main LirMemoryBench  -swf tests/swf/lir-memory-bench.swf  -swf-version 11.2
node tools/flash-accelerators-test.mjs
node tools/bench-flash-lir.mjs --workload number-bench --repeats 100 --runs 5 --families numbers
node tools/bench-flash-lir.mjs --swf path/to/variant.swf --repeats 100 --runs 4 --families numbers    # any timing-pixel fixture
JIT_REPORT_FULL=1 node web/flash-smoke.mjs --plugin vendor-private/libpepflashplayer-32.0.0.363.so --swf <swf> --lir ...
```

Useful focused commands:

```sh
cargo test -p emu --lib native_conditional
cargo test -p emu --lib number_
node web/flash-smoke.mjs --plugin vendor-private/libpepflashplayer-32.0.0.363.so \
  --swf tests/swf/lir-numbers.swf --seconds 8 --wait-for-pass --lir \
  --expect-pixel 360,260,00ff00
```

A green pixel is necessary but not sufficient. Also check `lowered`, `loweredNumbers`,
`loweredMemory`, `completed_native_leaf_calls`, `crashed=no`, and the refusal counters.

## Constraints for future edits

The rules for editing the verifier, the adapters and the JIT are in [AGENTS.md](../AGENTS.md#constraints-when-editing-the-jit-and-lir-code).
