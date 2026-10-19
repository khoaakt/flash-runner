import assert from 'node:assert/strict';
import { preparePluginAccelerators } from './plugin-accelerators.js';
import { gzipSync } from 'node:zlib';

const plugin = new Uint8Array([1, 2, 3, 4]);
const sha256 = [...new Uint8Array(await crypto.subtle.digest('SHA-256', plugin))].map((b) => b.toString(16).padStart(2, '0')).join('');
const url = 'https://example.test/assets/runtime-aot/manifest.json';
const originalFetch = globalThis.fetch;
try {
  let fetches = 0;
  let installs = 0;
  let image = { sha256, size: plugin.length, artifacts: [{ file: 'test.aot' }] };
  globalThis.fetch = async (request) => {
    fetches++;
    return String(request) === url
      ? { ok: true, json: async () => ({ format: 'flash-runner-plugin-accelerators-v1', images: [image] }) }
      : { ok: true, arrayBuffer: async () => Uint8Array.from([5, 6, 7]).buffer };
  };
  const emu = { installPortableAot(bytes, base) {
    assert.deepEqual([...bytes], [5, 6, 7]); assert.equal(base, 0x6dcfa000); installs++; return 3;
  } };
  const a = await preparePluginAccelerators({ emu, plugin, manifestUrl: url });
  assert.equal(a.state.prepared, 1);
  assert.equal(installs, 0, 'must not install before the relocated-plugin boundary');
  assert.equal(a.onPluginLoaded({ loadBias: 0x6dcfa000 }), 3);
  assert.equal(a.state.installed, 3);
  assert.equal(installs, 1);
  assert.equal(a.onPluginLoaded({ loadBias: 0x6dcfa000 }), 3);
  assert.equal(installs, 1, 'source buffers were released; repeated notification must not reinstall');
  assert.equal(a.onPluginLoaded({ loadBias: 0x6ddfa000 }), 0);
  assert.equal(a.onPluginLoaded({ loadBias: -1 }), 0);
  assert.equal(installs, 1, 'a different plugin boundary must not claim earlier installation');
  image = { ...image, sha256: 'bad' };
  const b = await preparePluginAccelerators({ emu, plugin, manifestUrl: url });
  assert.equal(b.state.prepared, 0);
  image = { ...image, sha256, artifacts: [{ file: 'https://evil.test/code.paot' }] };
  const c = await preparePluginAccelerators({ emu, plugin, manifestUrl: url });
  assert.equal(c.state.prepared, 0);
  const before = fetches;
  await preparePluginAccelerators({ emu, plugin, manifestUrl: url, enabled: false });
  assert.equal(fetches, before);
  const compressed = gzipSync(Uint8Array.from([5, 6, 7]));
  globalThis.fetch = async (request) => String(request) === url
    ? { ok: true, json: async () => ({ format: 'flash-runner-plugin-accelerators-v1', images: [image] }) }
    : new Response(compressed);
  image = { sha256, size: plugin.length, artifacts: [{ file: 'test.aot.gz', encoding: 'gzip', decoded_bytes: 3 }] };
  const gzip = await preparePluginAccelerators({ emu, plugin, manifestUrl: url });
  assert.equal(gzip.state.prepared, 1); gzip.onPluginLoaded({ loadBias: 0x6dcfa000 });
  assert.equal(installs, 2);
  for (const decoded of [1, 4, 81 * 1024 * 1024]) {
    image.artifacts[0].decoded_bytes = decoded;
    const bad = await preparePluginAccelerators({ emu, plugin, manifestUrl: url });
    assert.equal(bad.state.prepared, 0, 'compressed stream must match its bounded decoded size');
  }
  console.log('PASS plugin identity, local assets, install boundary, compressed bounds and disabled mode');
} finally { globalThis.fetch = originalFetch; }
