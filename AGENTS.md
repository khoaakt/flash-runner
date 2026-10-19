# AGENTS.md

Guidance for AI coding agents (and humans) working in this repository. `CONTRIBUTING.md` is the human-facing version; this file is the short, strict one.

## What this is

An x86-32 user-mode emulator with a Linux personality (Rust, compiled to WebAssembly) that runs Adobe's PPAPI Flash plugin
(`libpepflashplayer.so`, ia32 Linux) in a browser tab, plus the Chrome extension, the JavaScript Pepper (`PPB_*`) host, a wasm JIT with a
verified "LIR" fast path for hot NanoJIT methods, and a native socket helper. **The Flash plugin is not in this repository and must never be.**
Start with `README.md`, then `docs/ARCHITECTURE.md`; `docs/STATUS.md` says what works and what is open.

## Layout

| path | what |
|---|---|
| `crates/emu/` | the emulator (`cpu.rs`, `int.rs`, `fpu*.rs`, `sse*.rs`, `kernel.rs`/`sys.rs`, `mem.rs`), the wasm JIT (`jit*.rs`, `wasm_emit.rs`, design in `docs/JIT.md`), the LIR backend (`lir.rs`, `lir/`), AOT (`aot.rs`), `wasm_api.rs` (the exported wasm surface) |
| `web/` | JS runtime: `loader.js`/`worker.js` (player and worker), `pepper.js` + `ppb-*.js` (Pepper interfaces), `nanojit-*.js` (capture frontends that propose LIR), `lir-policy.js` (which LIR families are on, derived from recorded measurements) |
| `extension/`, `ui/` | Chrome extension (MV3) and its settings/popup UI (React + Vite, built into the extension) |
| `native/helper/` | Go socket helper started by Chrome through native messaging |
| `guest/`, `rootfs/` | code that runs inside the emulator (`pepper-host`, test plugins) and the Debian i386 userland builder (needs podman) |
| `tests/` | `progs/` C programs diffed against an i386 container, `swf-src/` Haxe probes → `swf/`, `ext/` pages for `tools/ext-test.mjs`, `bench/` |
| `tools/` | build scripts, test drivers, installers (`installer/`), `dis/` (small disassembler crate) |
| `third_party/ppapi/` | Chromium PPAPI headers (BSD-3), input of `tools/gen-ppapi.py` |
| `vendor-private/` | **git-ignored**: the Flash runtime(s) and the extension signing key you supply |

## Commands that work from a fresh clone

No Flash runtime, podman, Haxe or Chrome needed. These are what CI runs; run them before saying something works.

```sh
cargo test -p emu --lib                                           # ~30 s; 194 pass, 2 are #[ignore]d because they need the runtime
cargo build --release --target wasm32-unknown-unknown -p emu --lib   # the wasm build must keep compiling (needs: rustup target add wasm32-unknown-unknown)
for t in tools/native-sockets-test.mjs tools/zip-test.mjs web/context-menu-test.mjs web/lir-policy-test.mjs web/log-setting-test.mjs \
         web/nanojit-leaf-test.mjs web/nanojit-loop-test.mjs web/nanojit-memory-test.mjs web/nanojit-number-test.mjs \
         web/plugin-accelerators-test.mjs web/ppb-unit-test.mjs web/probe-observer-test.mjs; do node "$t" || exit 1; done
(cd ui && npm ci && npm run build)                                # type-checks and builds the settings UI
(cd native/helper && go vet ./... && go build -o /tmp/flash-helper . ) && node native/helper-test.mjs /tmp/flash-helper
```

Tested with Rust 1.99 (stable), Node 24, Go 1.27. The JS tools use only Node built-ins: do not add npm dependencies outside `ui/`.

## Commands that need local resources

| needs | commands |
|---|---|
| the runtime in `vendor-private/libpepflashplayer-32.0.0.363.so` and a built `web/emu.wasm` (`tools/build-wasm.sh`, needs `wasm-opt`) | `node web/flash-smoke.mjs`, `node tools/flash-accelerators-test.mjs`, `node tools/bench-flash-lir.mjs`, `node tools/swf-batch.mjs` |
| podman (the i386 container) | `rootfs/build.sh`, `tools/build-guest.sh`, `tools/build-progs.sh`, `tools/difftest.sh`, `node web/pepper-test.mjs`, `tools/build-helper.sh` (the `.deb`) |
| Chrome plus `node tools/build-extension.mjs` | `node tools/ext-test.mjs` (pass `--bundled`, see below) |
| Haxe | rebuilding `tests/swf/*.swf` from `tests/swf-src/*.hx` (`haxe -cp tests/swf-src -main <Class> -swf tests/swf/<name>.swf -swf-version 11.2`) |

After **any** Rust change, rebuild `web/emu.wasm` (`sh tools/build-wasm.sh`) before running a Flash test: the JS side loads the built file, not the source.

## Hard rules

1. **Never commit** the Flash runtime, `*.so` plugins, signing keys (`*.pem`, `*.key`), `*.crx`, `dist/`, build output, core dumps, session logs, tokens or personal paths. `.gitignore` covers them; do not weaken it, and look at `git status` before every commit.
2. **No third-party assets.** Do not vendor sample games, art, audio or video (see `tools/build-openfl-samples.sh`: samples are fetched, never committed). Test media must be generated (`tests/swf/assets/example.mp4` is).
3. **Do not circumvent Adobe's end-of-life block** (builds from 32.0.0.465 on show a "blocked" screen). The project runs builds from before it. Do not add code that defeats it, and do not point to unofficial mirrors of Adobe's software.
4. **Fail closed.** Anything the verifier cannot prove (unsupported shape, stale bytes, ABI mismatch, unknown runtime hash, unsafe memory condition) must run the original native method. A refusal is transactional: it leaves CPU and guest memory untouched.
5. **No performance claim without a recorded measurement.** Say what was measured, on what, against what. A single-machine benchmark is not a statement about a particular game; no controlled measurement of a real game exists. A LIR family is enabled only if its recorded A/B result beats native (`web/lir-policy.js`); a family that loses stays implemented, tested and off.
6. Do not write browser or OS enterprise policy, edit Chrome's protected Preferences, or touch other extensions' data. The extension is installed by the user (Load unpacked) or, on Linux, by the `.deb`'s external-extension JSON.
7. Keep Debian/glibc as the guest userland; keep synchronous wasm-region installation, exact-range SMC invalidation, W^X park/unpark and the 200 ms recompilation cooldown (`docs/STATUS.md` has the evidence).

## Constraints when editing the JIT and LIR code

- Prove native effects before trusting a frontend opcode name.
- Do not skip dead reads, dead stores, aliasing stores or unknown calls during verification.
- Clone symbolic state before exploring a branch arm.
- Preserve callee-saved GPRs, return-address behaviour and the correct EAX or x87 result.
- Caller-volatile registers and non-escaping dead stack slots may differ under cdecl.
- Bind every native byte covered by a successful path so SMC invalidates the replacement.
- Keep imports and generated wasm local indices consistent with `crates/emu/src/wasm_emit.rs`.
- Do not widen a guard to make a test pass.
- A new family needs, in this order: a real capture, a proof of every SSE/x87/memory effect, a Rust differential test against the interpreter, a finite AS3 fixture with its own block in `tools/flash-accelerators-test.mjs`, then a measurement (`tools/bench-flash-lir.mjs`).

## Gotchas

- `tools/ext-test.mjs` defaults to `vendor-private/libpepflashplayer.so`, which may be the post-EOL build that shows Adobe's blocked tile and never runs a movie. Always pass `--bundled` (the runtime inside the built extension) or `--runtime vendor-private/libpepflashplayer-32.0.0.363.so`. It writes screenshots to the git-ignored `tests/out/` unless `--shots` is given, and it turns the Debug log on (`--no-log` for the off state) because its checks read `[flash]` lines.
- `Extensions.loadUnpacked` over CDP is how the tests install the extension into branded Chrome (it ignores `--load-extension` since Chrome 137).
- In the LIR text format `x`, `xt`, `xf` are guard keywords: do not use them as SSA names. Haxe reserves `inline`.
- Chrome only accepts an off-store extension from a file on Linux. On Windows and macOS the user does Load unpacked once (`docs/DISTRIBUTION.md`).
- Files created inside some sandboxes carry `com.apple.provenance`, which `pkgbuild` turns into `._*` entries; `xattr -d` does not remove it.

## Style

Match the surrounding code: Rust 2021 with doc comments that state the invariant, plain ES modules in `web/` and `extension/`, tools in
dependency-free Node. Comments explain why and what is proven, not what the next line does. Keep edits to what the task needs; do not
reformat unrelated code. Tests go next to the code they cover (`*_tests.rs`, `web/*-test.mjs`).

## Before you finish

Run the fresh-clone commands above, `git status` (no stray artifacts), and say plainly what you could not run (the runtime-gated tests, podman, Chrome) instead of implying it passed.
