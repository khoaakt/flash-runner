// Load only artifacts packaged alongside the emulator, matched to the complete runtime image.
// The actual installation runs synchronously at HC_PLUGIN_LOADED before plugin initialization.
export async function preparePluginAccelerators({ emu, plugin, manifestUrl, log = () => {}, enabled = true }) {
  const state = { prepared: 0, installed: 0, rejected: 0, loadBias: null, coverage: null };
  const artifacts = [];
  if (enabled && manifestUrl) {
    try {
      const url = new URL(manifestUrl);
      const response = await fetch(url);
      if (!response.ok) throw new Error(`manifest HTTP ${response.status}`);
      const manifest = await response.json();
      if (manifest.format !== 'flash-runner-plugin-accelerators-v1' || !Array.isArray(manifest.images)) throw new Error('unsupported accelerator manifest');
      if (manifest.mode && !['bounded', 'discovered-with-fallback'].includes(manifest.mode)) throw new Error('unsupported accelerator build mode');
      // Only an explicit full-discovery build opts into the higher memory cost.
      const preparationLimit = (manifest.mode === 'discovered-with-fallback' ? 384 : 128) * 1024 * 1024;
      const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', plugin));
      const hash = [...digest].map((b) => b.toString(16).padStart(2, '0')).join('');
      const image = manifest.images.find((i) => i.sha256 === hash && i.size === plugin.length);
      if (image) {
        state.coverage = image.coverage || null;
        if (!Array.isArray(image.artifacts) || image.artifacts.length > 64) throw new Error('invalid artifact list');
        let totalBytes = 0;
        for (const spec of image.artifacts) {
          if (typeof spec?.file !== 'string' || !/^[a-zA-Z0-9.-]+\.aot(?:\.gz)?$/.test(spec.file)) throw new Error('invalid artifact filename');
          const asset = new URL(spec.file, url);
          if (asset.protocol !== url.protocol || asset.host !== url.host || asset.origin !== url.origin || !asset.pathname.startsWith(new URL('./', url).pathname)) throw new Error('artifact outside packaged asset directory');
          const r = await fetch(asset);
          if (!r.ok) throw new Error(`artifact HTTP ${r.status}`);
          if (spec.encoding && !['identity', 'gzip'].includes(spec.encoding)) throw new Error('unsupported artifact encoding');
          const compressed = spec.encoding === 'gzip';
          if (compressed !== spec.file.endsWith('.gz')) throw new Error('artifact encoding/filename mismatch');
          let bytes;
          if (compressed) {
            if (!Number.isSafeInteger(spec.decoded_bytes) || spec.decoded_bytes < 1 || spec.decoded_bytes > 80 * 1024 * 1024
              || totalBytes + spec.decoded_bytes > preparationLimit || typeof DecompressionStream === 'undefined') throw new Error('invalid compressed artifact bounds/support');
            const reader = r.body.pipeThrough(new DecompressionStream('gzip')).getReader();
            bytes = new Uint8Array(spec.decoded_bytes);
            let written = 0;
            try {
              for (;;) {
                const { value, done } = await reader.read();
                if (done) break;
                if (value.length > bytes.length - written) throw new Error('decompressed artifact exceeds declared size');
                bytes.set(value, written); written += value.length;
              }
              if (written !== bytes.length) throw new Error('truncated compressed artifact');
            } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
          } else bytes = new Uint8Array(await r.arrayBuffer());
          if (!bytes.length || bytes.length > 80 * 1024 * 1024) throw new Error('invalid artifact size');
          totalBytes += bytes.length;
          if (totalBytes > preparationLimit) throw new Error('packaged artifacts exceed per-runtime memory limit');
          artifacts.push(bytes);
        }
      }
      state.prepared = artifacts.length;
      log(`[aot] prepared ${artifacts.length} static artifacts for runtime ${hash.slice(0, 12)}`);
    } catch (e) {
      // Missing/incompatible acceleration must not stop the original Flash runtime.
      artifacts.length = 0;
      state.prepared = 0;
      log(`[aot] unavailable: ${e.message || e}`);
    }
  }
  return {
    state,
    onPluginLoaded({ loadBias }) {
      if (!Number.isInteger(loadBias) || loadBias < 0 || loadBias > 0xffffffff
        || (state.loadBias !== null && state.loadBias !== loadBias)) {
        state.rejected++;
        log('[aot] ignored inconsistent relocated-plugin boundary');
        return 0;
      }
      state.loadBias = loadBias;
      const started = performance.now();
      for (const bytes of artifacts) {
        try { state.installed += emu.installPortableAot(bytes, loadBias); }
        catch (e) { state.rejected++; log(`[aot] rejected artifact: ${e.message || e}`); }
      }
      if (artifacts.length) log(`[aot] plugin base=0x${loadBias.toString(16)} installed=${state.installed} regions rejected=${state.rejected} bind+install_ms=${(performance.now() - started).toFixed(1)}`);
      artifacts.length = 0; // compilation/install completed; release source bundle buffers
      return state.installed;
    },
  };
}
