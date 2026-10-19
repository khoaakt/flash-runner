# Debugging and diagnostics

## Debug log

The options page has a **Debug log** switch (Troubleshooting, off by default). When it is on, the player prints its messages to the browser
console: loading, JIT and LIR status, the guest's console output. It is the only switch; the settings below print nothing and cost nothing
while it is off. `tools/ext-test.mjs --diag` and `--fps-log` turn it on by themselves.

## Extra settings

Both are stored in `chrome.storage.local` and are not shown in the options page.

- `diag`: every player logs a line every 3 s (fps, where the emulator's time goes, the busiest PPB calls, JIT statistics) plus every network fetch
  and socket connect. The log itself costs 2-3 % because it times every hypercall.
- `fpsLog`: use this for a frame-rate number. The worker only counts frames and explains every freeze of more than 150 ms:
  `[stall] ... guest ... ms, event-loop ... , idle ...` and `[blocked] <host call> held the main thread for N ms`. Measure on an otherwise idle machine.

## Firefox

`tools/firefox-test.mjs` drives Firefox through geckodriver (WebDriver over HTTP, no npm packages): it installs `dist/firefox` as a temporary add-on, seeds the settings, opens a test page from
`tests/ext/` and checks that the player starts, the socket helper answers, Referer and Origin are rewritten, and the Flash runtime initialises. It needs Firefox 140 or newer and
[geckodriver](https://github.com/mozilla/geckodriver/releases).

```sh
node tools/build-extension.mjs --firefox
node tools/firefox-test.mjs --geckodriver /path/to/geckodriver --page tests/ext/compat.html --seconds 15 --shots /tmp/shots
node tools/firefox-test.mjs --geckodriver /path/to/geckodriver --page tests/ext/input.html --clicks "100,30" --keys "h,i" --shots /tmp/shots   # type into a Flash field
```

The screenshot lands in `--shots` (default `tests/out`). `--headed` shows the window; `FF_TEST_LOG=<file>` saves Firefox's console output (the `[flash]` lines appear there because the test turns the
Debug log on). WebDriver cannot open `moz-extension://` pages itself, so the test opens the settings page from Firefox's privileged context (`geckodriver --allow-system-access`).
`node tools/firefox-manifest-test.mjs` checks the Firefox manifest without a browser and runs in CI.

## Measuring performance

Only numbers taken in Chrome through the extension describe the extension. Timings from `web/flash-smoke.mjs` under Node differ, and on Windows
they are distorted by the timer tick, so use them to shortlist, not to decide.

```sh
node tools/ext-test.mjs --bundled --page tests/ext/bench.html --seconds 28 --fps-log --no-shots
node tools/ext-test.mjs --bundled --page tests/ext/bench.html --seconds 26 --no-shots --cpu --shots /tmp/prof   # CPU profile of the emulator worker
node tools/ext-test.mjs --bundled --page tests/ext/bench.html --setting jitTrustLoads=false                       # change a stored setting for one run
```

`--cpu` writes `worker-N.cpuprofile` files (open them in Chrome DevTools). The profiler perturbs timing, so use it to see where time goes and
run without it to measure. Alternate the variants you compare and repeat each at least five times.
