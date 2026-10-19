## What and why

<!-- The change, and the problem it solves. Link the issue. -->

## How it was checked

- [ ] `cargo test -p emu --lib`
- [ ] the JS tests / UI build / helper test that cover what I touched (commands in AGENTS.md)
- [ ] Tests that need the Flash runtime, podman or Chrome: <ran / could not run, and which>

## Checklist

- [ ] No Adobe/HARMAN software, third-party assets, keys, tokens, logs or build output in the diff (`git status` checked)
- [ ] JIT/LIR change: fails closed, every effect proven, and measured before enabling (what, on which machine, against what)
- [ ] Docs updated where behavior changed
- [ ] Performance wording says what was measured, not "faster games"
