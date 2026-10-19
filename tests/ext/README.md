# tests/ext: pages driven by tools/ext-test.mjs

Each page embeds one movie; `node tools/ext-test.mjs --bundled --page tests/ext/<page>.html --seconds 10` loads the unpacked extension in a real
Chrome and opens it (see the script's header for the flags).

Movies come from two places:
- `tests/swf/*.swf`, built from `tests/swf-src/*.hx` (our own probes; `tests/swf/assets/example.mp4` is a generated test pattern for `vid.html`).
- `tests/swf-samples/*.swf`, OpenFL's sample games (`bunny*.html`, `pig.html`, `s3d-*.html`, `ei.html`, ...). They are not in the repository:
  build them with `tools/build-openfl-samples.sh HelloTriangle BunnyMark PiratePig BrowserExternalInterface Stage3DCamera`.
