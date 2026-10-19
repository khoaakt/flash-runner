# REA findings

Target analysed: `libpepflashplayer.so` from Adobe Flash Player for Linux **32.0.0.465** (PPAPI, i386), a copy the analyst already held;
sha256 of the library `9cda0c4b8b5ffd57eeb7cbbfd3c0cac2b29699dc71d8a129586d0fff4280b423`.
Provider: Ghidra 12.1.4 through REA. The library itself is **never** committed or bundled in this repository.

Confidence legend: **observed** = returned by a tool from the shipped artifact; **derived** = computed from observed data;
**inferred** = our reading of it; **unknown** = not established.

## Questions the analysis had to answer, and answers

| # | Question | Answer | Evidence |
|---|----------|--------|----------|
| 1 | Which Flash flavour can we host without X11/GTK/GL in the guest? | PPAPI (`libpepflashplayer.so`), ia32 ELF. **observed** | readelf headers/dynamic section; manifest.json (`x-ppapi-arch: ia32`, `x-ppapi-os: linux`) |
| 2 | What does it link against? | `libdl librt libstdc++ libm libgcc_s libpthread libc ld-linux` only — no X11, GTK, NSS, GL, ALSA, curl. **observed** | `llvm-readelf -d` (DT_NEEDED) |
| 3 | What does it export (host -> plugin surface)? | `PPP_GetInterface`, `PPP_InitializeModule`, `PPP_ShutdownBroker`, `PPP_InitializeBroker` (no `PPP_ShutdownModule`). **observed** | `.dynsym`; `procedure_pseudo_code` evidence `ev_cc0bc17c…`, `ev_5a02d42a…` |
| 4 | What does it import (libc surface the emulator must provide)? | 322 undefined symbols: full pthreads, sockets (`socket connect bind listen accept select`), SysV shm (`shmget shmat shmdt`), `makecontext/getcontext/setcontext`, `dlopen/dlsym`, `mprotect/mmap64`, `getrusage`, `sched_getaffinity`, `syscall`, `ioctl`, locale (`newlocale`, `*_l`). **observed** | `.dynsym` UND entries (`$SP/flash-imports.txt`) |
| 5 | Which browser interfaces does it consume? | 107 distinct `PPB_*;version` strings, 105 of which exist in Chromium's tag-86 headers (the other two are version aliases we map). **observed** | REA `search_strings "PPB_"` `ev_56de7572…` |
| 6 | Which interfaces does it provide to the host? | `PPP_Instance;1.1`, `PPP_InputEvent;0.1`, `PPP_Messaging;1.0`, `PPP_MouseLock;1.0`, `PPP_Graphics_3D;1.0`, `PPP_Flash_BrowserOperations;1.0/1.2/1.3`, `PPP_Instance_Private;0.1`, `PPP_Printing(Dev);0.6`, `PPP_TextInput(Dev);0.1`, `PPP_VideoCapture(Dev);0.1`, `PPP_VideoDecoder(Dev);0.11`, `PPP_NetworkState(Dev);0.1`. **observed** | REA `search_strings "PPP_"` `ev_c6444cd9…` |
| 7 | What does it touch in the filesystem? | `/dev/random`, `/dev/hwrng`, `/dev/srandom`, `/usr/lib/locale/locale-archive`, `.macromedia.com/` (cookie domain, not a path), and OpenSSL's built-in `/usr/local/ssl/{,private,certs,cert.pem}` + `/usr/local/lib/engines-1.1` (so TLS is statically inside the plugin). **observed** | REA regex `search_strings` `ev_04da3dbd…` |
| 8 | How big/complex is it? | 43,911 analysed procedures, `.text` 12,852,392 bytes, `.rodata` 1.6 MB, `.rel.dyn` 787,456 bytes (98k relative relocations → ld.so relocation cost), `.bss` ~0.95 MB, 11,917 defined strings. **observed** | REA `binary_overview` `ev_11494e29…` |
| 9 | How does init work? | `PPP_InitializeModule(module, get_browser_interface)` allocates a module object, runs an init routine (`FUN_00c9fa78` at 0xd… in Ghidra coordinates, base 0x10000) that fetches the PPB tables, returns `0` on success and `-2` (`PP_ERROR_FAILED`) if any required interface is missing; the module pointer is kept in a global used by `PPP_GetInterface`. **observed** (control flow) / **inferred** (that the init routine is where interfaces are requested) | `ev_cc0bc17c…` |
| 10 | Does the build contain an end-of-life/kill-switch string? | A regex over defined strings for `killswitch|EOL…|mms.cfg|autoUpdate…|blocked|2021` found only `mms.cfg`, `AutoUpdateDisable`, `SilentAutoUpdateServerDomain`, `autoUpdateDisabled`. The publicly documented behaviour of the final public builds (content blocked after 2021-01-12) is therefore **not located** by this search (**unknown** where/how it is implemented). | `ev_e8a0d2a4…` (negative, bounded to Ghidra-defined strings) |

## Design consequences (derived from the above)
- Only a user-mode Linux/i386 CPU + kernel personality is needed; no X server, no GTK. (Q2)
- The host surface is the PPB table list (Q5) — implemented as generated stubs that trap into JavaScript (`tools/gen-ppapi.py`).
- Threads, futexes, `ucontext`, signals, sockets-as-pipes, `dlopen` of the plugin itself are all required kernel features (Q4).
- Stage3D uses `PPB_Graphics3D` + `PPB_OpenGLES2` (+ extensions listed in Q5) → WebGL2 first, WebGPU later.
- The runtime needs a commercial redistribution licence from Harman for production use; this repo ships no Adobe/Harman code.

## Not established
- Where the end-of-life block is implemented (Q10). We do not circumvent it: the guest clock is a plain VM setting, and the
  supported route for production is a licensed Harman build.
- Whether Flash requires `PPB_Graphics3D` for rendering at all or falls back to its software rasteriser (we answer
  `GetSetting(3DENABLED)=false` until the GL path is wired).
