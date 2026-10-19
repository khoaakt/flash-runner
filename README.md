# flash-runner

> **Experimental. Not affiliated with, endorsed by or sponsored by Adobe or HARMAN.** This repository contains **no Adobe software**: you supply a
> Flash runtime you are entitled to use ([details](#the-flash-runtime), [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)). Flash is end-of-life and
> the builds this runs have known security bugs; do not play content you do not trust.

Run the **unmodified Adobe/Harman Flash runtime** (PPAPI, 32-bit Linux) in a browser tab with
WebAssembly + WebGPU — no Flash plugin, no HTML5 rewrite of the content. Built from scratch; see `docs/ARCHITECTURE.md` for the design and `docs/REA-FINDINGS.md` for the
reverse-engineering evidence it rests on.

```
<embed src="movie.swf">  ──loader.js──►  Worker ──► emu.wasm (x86-32 CPU + Linux personality)
                                              │           └─ guest: ld.so, glibc, pepper-host, libpepflashplayer.so
                                              └─ pepper.js (PPB_* in JavaScript) ─► WebGPU / WebGL2, WebAudio, fetch
```

## The Flash runtime
The extension build bundles two unmodified Adobe Flash Player builds from before Adobe's end-of-life block — 32.0.0.363 (default) and
26.0.0.151, ia32 Linux PPAPI — taken from `vendor-private/` (git-ignored; `tools/build-extension.mjs` fails if they are missing). They are Adobe/HARMAN
software, not covered by this project's licence. A build that contains them is for your own use: do not publish it, and check your own licence terms before giving it to anyone.
A different build can still be chosen from a file in the settings.

What was tested here (all ia32 PPAPI, supplied by the tester; whether a given file is unmodified cannot be verified from this repository):

| build | result |
|-------|--------|
| 32.0.0.363 (March 2020) | plays content; preferred — newest build that predates Adobe's end-of-life block |
| 26.0.0.151 (2017) | plays content |
| 32.0.0.465 (Dec 2020 and later) | shows Adobe's "Flash Player is blocked" screen and never requests the movie; this project does not try to defeat that |

Old Flash builds have known security bugs. They run inside the WebAssembly sandbox (no host syscalls, no network except what the
Pepper bridge exposes), which contains them; it does not make untrusted content safe in other ways (e.g. what the movie asks the page to do).

## Quick start
The unit tests need nothing but Rust: `cargo test -p emu --lib`. Playing a movie needs the runtime and the guest userland:
```sh
. "$HOME/.cargo/env"            # rustup toolchain with wasm32-unknown-unknown
podman machine start            # i386 container used as toolchain + CPU test oracle
tools/build-wasm.sh             # crates/emu -> web/emu.wasm
rootfs/build.sh                 # guest glibc userland -> rootfs/out/rootfs.tar
tools/build-guest.sh            # pepper-host + test plugins (generates PPB stubs from third_party/ppapi)
node web/serve.mjs              # http://localhost:8080/web/index.html   (?plugin=fake for the bundled test plugin)
```
Browser requirements: WebAssembly, `SharedArrayBuffer`/cross-origin isolation (the dev server sets COOP/COEP),
WebGPU for the fast presenter (a 2D-canvas fallback exists).

## Tests
```sh
tools/build-progs.sh            # compile tests/progs/*.c in the i386 container, record the oracle output
tools/difftest.sh '*' --sse2    # native interpreter vs i386 container
RUNNER=wasm tools/difftest.sh '*' --sse2   # same programs inside the wasm build (Node)
node web/pepper-test.mjs        # PPAPI bridge end-to-end with the fake plugin
node web/flash-smoke.mjs --swf tests/swf/testsrc.swf --click 262,172   # real runtime smoke test
```

## What works / what does not
Verified end to end in Chrome through the extension with 32.0.0.363 (see `tools/ext-test.mjs`, `tests/ext/*.html`):
- vector/bitmap rendering, device-font text, input TextFields (typing, caret), mouse and keyboard input, video (embedded FLV, progressive H.264/AAC MP4 via `NetStream`; decoded by the plugin itself)
- fullscreen (enter and leave, with `FULL_SCREEN` events), clipboard paste (Cmd/Ctrl+V), local `.swf` files via the viewer
- known gap: some publishers' games refuse to start without their own desktop launcher (e.g. a local service on 127.0.0.1) — nothing in a browser can supply that
- Flash's right-click menu is shown as an HTML menu; Cut / Copy / Paste through it were verified in Chrome (selection, clipboard read and write)
- Stage3D (`gpu3d` option): Flash's OpenGL ES 2 calls are translated to WebGL2 (`web/gles2-context.js`) — a rotating `HelloTriangle` renders
- audio (WebAudio worklet; it glitches when the emulator cannot keep up), Shared Objects (persisted in IndexedDB across page loads), HTTP(S) networking — `URLLoader` GET/POST, `Loader` child SWFs, `Sound.load` MP3, progressive video, `crossdomain.xml` policy files (all through `fetch`; the extension
  rewrites Referer/Origin to the embedding page so hot-link and CSRF checks behave as in a real plugin)
- `ExternalInterface`: Flash -> page (`ExternalInterface.call`, `ExecuteScript`, window/document access) and page -> Flash
  (`await element.myCallback(args)`; asynchronous because the movie runs in a worker). Details in `docs/ARCHITECTURE.md`.

Not available / known gaps: raw Flash sockets (`Socket`/`XMLSocket`, socket policy servers) work only through the [socket helper](#raw-tcp-sockets-the-helper) because a browser cannot open raw TCP; RTMP/RTMFP streaming and UDP are not available; camera and microphone,
`FileReference` dialogs, gamepads, printing, pages whose CSP forbids `eval` (no `ExecuteScript`). Speed is the main limit: the x86 emulator
is an interpreter with a wasm JIT for hot code (on by default, `docs/JIT.md`; about 5x faster on a bitmap-filter benchmark inside real Flash, 14-26x on integer kernels), so heavy games still run well below native Flash.

Experimental acceleration tools (`emu-lir`, `emu-aot`, guarded runtime HLE) and their deployment
limits are documented in [docs/ACCELERATORS.md](docs/ACCELERATORS.md). Metadata-selected portable AOT
and verified scalar/bounded-loop LIR adapters are wired into real Flash playback; most NanoJIT methods
still use the original path. This is not complete NanoJIT replacement or a measured speedup of a real game.

## Raw TCP sockets: the helper
`flash.net.Socket` / `XMLSocket` (including socket policy files) need a TCP connection, which a browser cannot open. A small helper
program on the user's machine provides it (`native/`), started by Chrome through native messaging; only this extension's id may talk to
it. It refuses loopback / private / link-local destinations unless `~/.flash-runner/sockets.json` contains `{"allowPrivate": true}`
(also supports `allowHosts`/`denyHosts` globs and `maxSockets`). The extension's settings page offers the installer for the user's OS.
The helper is one Go binary (`native/helper`); `tools/build-helper.sh` cross-compiles it and packages the installers the extension bundles
(`FlashHelper-Setup.exe`, `FlashHelper.pkg`, `flash-helper_amd64.deb`; unsigned, so Windows and macOS warn once). Developers: `tools/install-native-host.sh` registers a checkout's build directly; `node tools/ext-test.mjs --native --page tests/ext/sock.html --seconds 10`
drives `tests/swf/sock.swf` through it against an echo + policy server.
When a movie opens a socket and no helper is installed, the player shows a bar over the movie with an Install button, the toolbar button
gets a "!" and the popup offers the install (it opens the settings page at its Connection section); all of it goes away once the helper
answers. `node tools/ext-test.mjs --no-helper --page tests/ext/sock.html` checks that path in Chrome, `node tools/native-sockets-test.mjs`
the detection logic (reported once, retried now and then, withdrawn when the helper appears) without a browser.

## Diagnostics
The options page has a **Debug log** switch (Troubleshooting, off by default): on, the player prints its messages (loading, JIT/LIR status, guest console) to the browser console. It is the only switch: the diagnostics settings below (stored in `chrome.storage.local`, not shown in the options page) print nothing, and cost nothing, while it is off; `tools/ext-test.mjs --diag/--fps-log` turns it on itself. The diagnostics setting ("diag", stored in `chrome.storage.local`): each player then logs a line every 3 s (fps, where the emulator's time goes, busiest PPB
calls, JIT statistics) plus every network fetch and socket connect. The log itself costs 2–3 % (it times every hypercall); for a frame-rate number
use `--fps-only` (a hidden `fpsLog` setting: the worker only counts frames and explains every freeze of more than 150 ms: `[stall] … guest … ms, event-loop … , idle …` and `[blocked] <host call> held the main thread for N ms`) on an otherwise idle machine.

## Layout
| path | what |
|------|------|
| `crates/emu` | Rust: IA-32 interpreter (iced-x86 decoder), x87/SSE/MMX, wasm JIT for hot blocks, ELF loader, Linux syscalls, futex/threads/signals, VFS |
| `guest/pepper-host` | the only native guest code of ours: dlopens the plugin, exposes generated PPB stubs |
| `tools/gen-ppapi.py` | generates stubs + signature table from the vendored Chromium PPAPI headers |
| `web/` | loader, Worker, Pepper host (`pepper.js`, `ppb-*.js`), WebGPU presenter, AudioWorklet, dev server |
| `third_party/ppapi` | Chromium PPAPI C headers (BSD-3-Clause), tag 86.0.4240.198 |
| `extension/`, `ui/`, `native/` | the Chrome extension, its settings UI, and the socket helper |
| `tests/`, `tools/` | test programs, movies and pages; build, test and benchmark drivers |

## Chrome extension
```sh
node tools/build-extension.mjs --zip      # -> dist/extension (and dist/flash-runner-extension.zip)
```
1. `chrome://extensions` → enable *Developer mode* → **Load unpacked** → choose `dist/extension`.
2. The settings page opens on first run: choose your licensed `libpepflashplayer.so` (stored only in your browser profile).
3. Open any page with Flash — `<embed>`/`<object>` movies become players, and links to `.swf` files open in a viewer. A local `.swf` can be played from the toolbar popup ("Open a local .swf…") or by dropping it on the viewer tab.

Automated end-to-end check in a real Chrome (installs the extension over CDP): `node tools/ext-test.mjs --bundled --seconds 10 --shots /tmp/shots`
(`--bundled` runs the Flash 32.0.0.363 shipped inside the extension; without it the test uses `vendor-private/libpepflashplayer.so`, which
may be a post-EOL build that blocks every movie).

### Distributing it
| | what to hand out | how the user installs it |
|---|---|---|
| **Windows, macOS** | `dist/flash-runner-extension.zip` + the helper installer (`FlashHelper-Setup.exe`, `FlashHelper.pkg`; the extension's settings page offers the right one) | unzip, then `chrome://extensions` → *Developer mode* → **Load unpacked** (once). Chrome refuses to install an off-store extension any other way on these systems: it blocks `external_crx` and a CRX, and force-install by policy needs a managed PC (Active Directory, MDM or Chrome Enterprise Core). |
| **Linux** | `dist/flash-runner_<version>_{amd64,arm64}.deb` | `sudo apt install ./flash-runner_*.deb`, restart the browser: nothing else. The package carries the extension as a signed CRX plus the helper. |

```sh
node tools/build-extension.mjs && node tools/build-installer-linux.mjs     # -> dist/flash-runner_<version>_{amd64,arm64}.deb (needs podman)
node tools/installer-linux-test.mjs                                        # checks both packages in containers
```
Linux is the one platform where a file installs the extension: Chrome, Chromium and Edge read `<dir>/<extension id>.json` (`external_crx`
+ `external_version`) at startup and add the CRX without asking ([Chrome docs](https://developer.chrome.com/docs/extensions/how-to/distribute/install-extensions));
removing the package removes the JSON, and the browser drops the extension at its next start. The CRX is signed with
`vendor-private/extension-key.pem` (never packaged), so the extension keeps its ID. The test installs the package in a Chromium container and
sees the extension appear, enabled, then disappear after `dpkg -r`; it has not been run in Chrome or Edge for Linux. Brave and Vivaldi have no
documented directory, so only the helper is registered for Brave. The package replaces the standalone `flash-helper` deb.

### macOS installer (optional)
```sh
node tools/build-extension.mjs && node tools/build-installer.mjs      # -> dist/FlashRunner.pkg (unsigned)
```
An alternative to the zip for macOS: one package for the extension folder (`/Applications/Flash Runner/extension`) and the online game helper
(selected by default; it registers the helper for Chrome, Chromium, Edge, Brave and Vivaldi). The user still does the one manual step
(`chrome://extensions` → *Developer mode* → **Load unpacked** → that folder); its last page says so and `/Applications/Flash Runner/README.txt`
has the rest. To remove everything, run `Uninstall.command` from `/Applications/Flash Runner`. `node tools/installer-test.mjs` checks the package
without installing it. It is unsigned: one you built yourself opens directly, while a copy that was downloaded or AirDropped is quarantined and
macOS asks you to allow it in System Settings → Privacy & Security (*Open Anyway*).

## Documentation
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): the layers and how a call travels from the plugin to the page
- [docs/JIT.md](docs/JIT.md): the wasm JIT: design, guards and measurements
- [docs/ACCELERATORS.md](docs/ACCELERATORS.md), [docs/NANOJIT-ABI.md](docs/NANOJIT-ABI.md): LIR, AOT and guarded HLE; what the NanoJIT capture proves and does not
- [docs/STATUS.md](docs/STATUS.md): what works, what was measured, what is open
- [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md): how the extension reaches users on each OS, and why
- [docs/REA-FINDINGS.md](docs/REA-FINDINGS.md): what was learned about the runtime and how it was established

## Contributing, security, conduct
See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) (for AI coding assistants). Report vulnerabilities privately as described in
[SECURITY.md](SECURITY.md). Participation is covered by the [Code of Conduct](CODE_OF_CONDUCT.md).

## License
Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Unless you state otherwise, a contribution you submit is
licensed the same way, with no additional terms. Third-party components keep their own licences: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
Adobe Flash Player, the runtime you supply, is not part of this project and is not covered by that licence.
