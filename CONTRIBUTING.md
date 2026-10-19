# Contributing

Thanks for helping. This project is a Flash-plugin emulator, a JIT and a browser extension; changes to the emulator and the JIT are held to a
high bar for correctness because a wrong answer here silently corrupts a program that looks like it runs. Bug reports, tests, docs and
small fixes are all welcome.

By contributing you agree that your work is licensed under the project's licence, [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE), at the
user's option, with no additional terms. Please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Report security problems privately, as
described in [SECURITY.md](SECURITY.md).

## What you can and cannot bring

- **Do not** include Adobe/HARMAN software, or links to unofficial copies of it. The runtime stays on the machine of whoever runs the project, in
  the git-ignored `vendor-private/`.
- **Do not** commit third-party assets (sample games, art, audio, video), signing keys, tokens, session logs or build output. Test media must be generated.
- Do not add code that defeats Flash's end-of-life block, or that installs the extension by tampering with the browser's protected settings.
- Disassembly or captures from the runtime belong in the repository only as small, factual fixtures with a note on where they came from
  (see "Observations of the runtime" in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)).

## Getting set up

You need Rust (stable) with the wasm target (`rustup target add wasm32-unknown-unknown`), Node 24 and, for the UI, `npm`. For the socket helper, Go.
These work from a fresh clone, with no Flash runtime:

```sh
cargo test -p emu --lib
cargo build --release --target wasm32-unknown-unknown -p emu --lib
node web/lir-policy-test.mjs                  # and the other web/*-test.mjs and tools/*-test.mjs listed in AGENTS.md
(cd ui && npm ci && npm run build)
(cd native/helper && go vet ./... && go build -o /tmp/flash-helper .) && node native/helper-test.mjs /tmp/flash-helper
```

Running real movies needs a copy of Flash Player in `vendor-private/` (see `vendor-private/README.md`), a built
`web/emu.wasm` (`tools/build-wasm.sh`) and the guest userland (`rootfs/build.sh`, `tools/build-guest.sh`; both need podman). The README has the
full sequence, and [AGENTS.md](AGENTS.md) lists which command needs what. Please say in your pull request which of these you could run.

**Firefox.** `node tools/build-extension.mjs --firefox` also writes `dist/firefox`; `node tools/firefox-manifest-test.mjs` checks its manifest with no browser, and `tools/firefox-test.mjs` drives a real Firefox
(needs geckodriver, see `docs/DEBUGGING.md`). The socket helper has a separate manifest for Firefox: change both when you change one.

**Windows.** Use Git Bash for the `.sh` scripts and put `cargo`, Go, Node, Python 3 and `podman` on `PATH`. The scripts call `docker`, so give podman a
`docker` alias or shim; `podman machine init && podman machine start` first. `git config core.fileMode false` stops the lost executable bit of
the checked-out scripts from showing up as changes. `--zip` of `tools/build-extension.mjs` needs a `zip` command (otherwise
`tar -a -cf dist/flash-runner-extension.zip -C dist/extension .`). The macOS package (`FlashHelper.pkg`, `FlashRunner.pkg`) can only be built on macOS.
Timing numbers from Node on Windows are not representative of the extension: measure with `tools/ext-test.mjs` in Chrome (`--page`, `--cpu`).

## Making a change

1. Open an issue first for anything larger than a fix, especially in `crates/emu/src/lir/` or the JIT.
2. Keep the change focused. Do not reformat unrelated code.
3. Add or update a test next to the code (`crates/emu/src/*_tests.rs`, `web/*-test.mjs`). A bug fix starts with a test that fails.
4. For JIT/LIR work, follow the rules in [AGENTS.md](AGENTS.md#constraints-when-editing-the-jit-and-lir-code): fail closed, prove every effect, measure before
   enabling. State what you measured, on which machine, against what; do not generalize a benchmark to "games run faster".
5. Update the docs that describe the behavior you changed (`docs/`, the README).
6. Open the pull request using the template. CI runs the fresh-clone commands above.

Commit messages: a short imperative subject, then why the change is needed.

## Using an AI coding assistant

Welcome, with the same bar as any contribution: you are responsible for every line, you have run the tests, and you can explain the change.
[AGENTS.md](AGENTS.md) is written for assistants; point yours at it.
