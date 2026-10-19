# Third-party notices

This project is licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at your option. It uses and builds on the components below,
each under its own licence. The list is for the source tree; a built extension adds the items marked *build output*.

## Adobe Flash Player: not included

The repository contains **no Adobe or HARMAN code**. The emulator runs a `libpepflashplayer.so` (PPAPI, ia32 Linux) that has to be copied into
`vendor-private/`. Adobe Flash Player is proprietary software of Adobe Inc. / HARMAN International and is not covered by this project's
licence; its use and redistribution are governed by your own agreement with its owner. "Adobe" and "Flash" are trademarks of Adobe Inc.
This project is not affiliated with, endorsed by or sponsored by Adobe or HARMAN.

A build of the extension (`tools/build-extension.mjs`) bundles the Flash runtimes from `vendor-private/` and writes a `NOTICE.txt` saying so. Treat such a build as
containing third-party proprietary software: it is not something to publish.

### Observations of the runtime kept in this repository

Interoperability work needs facts about the runtime. What is kept is small and factual, and none of it is a copy of the runtime's text:

- SHA-256 digests that identify the supported builds (`web/lir-policy.js`, `web/nanojit-capture.js`, `tools/verify-nanojit-abi.py`, `docs/`).
- Structure offsets, symbol and interface names, and one table of NanoJIT opcode operand kinds read from the runtime's data
  (`tools/verify-nanojit-abi.py`, `docs/NANOJIT-ABI.md`, `docs/REA-FINDINGS.md`).
- Machine code that NanoJIT **generated at run time** for this project's own test movies (`tests/swf-src/*.hx`), kept as test fixtures
  (`crates/emu/src/lir/number_expr_data.rs` and a few inline hex strings in the LIR tests). It is output of the JIT for our ActionScript, not
  the runtime's own code. Regenerate it with `tools/gen-number-test-data.mjs`.

## Chromium PPAPI headers (BSD-3-Clause)

`third_party/ppapi/c/` is a verbatim copy of `ppapi/c/` from Chromium tag `86.0.4240.198`, Copyright The Chromium Authors. Licence text:
[`third_party/ppapi/LICENSE`](third_party/ppapi/LICENSE). The headers are only the input of `tools/gen-ppapi.py`.

## Rust crates

The emulator (`crates/emu`) depends on the crates below (versions from `Cargo.lock`); `tools/dis` additionally uses `iced-x86` and `lazy_static`.

| crate | licence |
|---|---|
| iced-x86 1.21 (x86 decoder) | MIT |
| bitflags, hashbrown, lazy_static, proc-macro2, quote, serde, serde_core, serde_derive, syn | MIT OR Apache-2.0 |
| equivalent, indexmap | Apache-2.0 OR MIT |
| string-interner, wasmi, wasmi_collections, wasmi_core, wasmi_ir (WebAssembly interpreter, used by tests) | MIT OR Apache-2.0 |
| libm, spin | MIT |
| foldhash | Zlib |
| unicode-ident | (MIT OR Apache-2.0) AND Unicode-3.0 |
| wasmparser | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT |

## Settings and popup UI (`ui/`; *build output*: bundled into the extension)

React, React DOM, Radix UI, clsx, tailwind-merge, tw-animate-css, sonner (MIT); lucide-react (ISC); class-variance-authority (Apache-2.0).
Components follow the shadcn/ui pattern (MIT). Run `cd ui && npm ls --prod` for the exact tree; all 86 production packages in
`ui/package-lock.json` are MIT, ISC, Apache-2.0 or 0BSD.

## Socket helper (`native/helper`)

Go standard library (BSD-3-Clause) and `golang.org/x/sys` (BSD-3-Clause), compiled into the helper installers.

## Guest userland (*build output*)

`rootfs/build.sh` assembles a minimal Debian GNU/Linux (bookworm, i386) userland, including the GNU C Library, libstdc++ and libgcc_s
(LGPL-2.1-or-later and GPL with runtime exception), into `rootfs/out/rootfs.tar`, which the extension bundles. The sources are available from
Debian: <https://packages.debian.org/source/bookworm/glibc> and `/gcc-12`. The userland is not stored in this repository.

## Not vendored

OpenFL's sample games (`tools/build-openfl-samples.sh`) are fetched from upstream when you build them and are never committed: their art, audio
and video belong to their authors. `tests/swf/assets/example.mp4` is a test pattern generated with ffmpeg for this project.
