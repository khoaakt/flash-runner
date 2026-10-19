# Security policy

## Reporting a vulnerability

Please report security problems **privately**, through this repository's *Security* tab → *Report a vulnerability* (GitHub private vulnerability
reporting), and not in a public issue. Include what you ran, what you expected and what happened; a minimal movie or input that triggers it
helps most. You will get an answer as soon as a maintainer can read it; this is a volunteer project and there is no guaranteed response time.

Only the current `main` branch is supported. There are no release branches yet.

## What counts

In scope: anything that lets content running in the emulator reach beyond it, or that makes a component do more than it says.

- **Emulator sandbox.** A guest program reading or writing host memory outside its own address space, escaping the Linux personality
  (`crates/emu`) to the host, or crashing the host page or worker from guest input.
- **JIT / LIR soundness.** A replacement that is accepted by the verifier but computes something different from the native code it replaces
  (`crates/emu/src/lir/`, `web/nanojit-*.js`). The design rule is *fail closed*: anything unproven must run the original code.
- **Browser extension** (`extension/`, `web/`). Privilege escalation, a page being able to read the user's runtime or saved data, an injection
  path through the content script or the player frame, a setting that does not do what the settings page says.
- **Socket helper** (`native/helper`). The helper opens raw TCP connections for movies. It only answers this extension's ID and refuses
  loopback, private and link-local destinations by default (`allowHosts` / `denyHosts` / `maxSockets` in `sockets.json`). Bypasses of
  those rules, or anything that lets another extension or a web page talk to it, are in scope.
- **Build and release scripts** that could ship a secret, a signing key or the proprietary runtime by accident.

Out of scope: vulnerabilities in Adobe Flash Player itself. Flash is end-of-life, the builds this project runs have known security bugs, and
the README says so. They run inside the WebAssembly sandbox with no host syscalls and no network except what the Pepper bridge exposes; that
contains them, but it does not make untrusted content safe in other ways (what a movie asks the page to do, for example). Do not play content
you do not trust.

## Handling

A fix is developed privately where that is practical, released on `main`, and credited if you want credit. If you found a way to extract or
redistribute the proprietary runtime from this project, report that the same way.
