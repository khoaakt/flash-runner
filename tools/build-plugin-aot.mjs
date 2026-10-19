// Compile metadata-seeded plugin CFGs OFFLINE. Runtime binding never invokes the x86 translator.
import { readFileSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { gzipSync } from 'node:zlib';

const root = fileURLToPath(new URL('../', import.meta.url));
const index = process.argv.indexOf('--out');
const out = index < 0 ? join(root, 'web/runtime-aot') : resolve(process.argv[index + 1]);
const complete = process.env.AOT_COMPLETE === '1';
const compress = process.env.AOT_COMPRESS !== '0';
const maxBlocks = Number(process.env.AOT_MAX_BLOCKS || (complete ? 1048576 : 65536));
const maxInstructions = Number(process.env.AOT_MAX_INSTRUCTIONS || (complete ? 5000000 : 1000000));
if (!Number.isInteger(maxBlocks) || maxBlocks < 1 || maxBlocks > 1048576 || !Number.isInteger(maxInstructions) || maxInstructions < 1 || maxInstructions > 5000000) throw new Error('invalid AOT build limits');
mkdirSync(out, { recursive: true });
execFileSync('cargo', ['build', '-p', 'emu', '--bin', 'emu-aot', '--release'], { cwd: root, stdio: 'inherit' });
const images = [];
const runtimes = ['libpepflashplayer-32.0.0.363.so', 'libpepflashplayer-26.0.0.151.so', 'libpepflashplayer.so'];
if (process.env.AOT_RUNTIME && !runtimes.includes(process.env.AOT_RUNTIME)) throw new Error('AOT_RUNTIME must select a known private runtime filename');
for (const file of runtimes.filter((file) => !process.env.AOT_RUNTIME || file === process.env.AOT_RUNTIME)) {
  const input = join(root, 'vendor-private', file);
  if (!existsSync(input)) continue;
  const bytes = readFileSync(input);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  const name = sha256.slice(0, 16) + (complete ? '-complete' : '-functions');
  const prefix = join(out, name);
  execFileSync(join(root, 'target/release/emu-aot'), [input, prefix, complete ? '--plugin-complete' : '--plugin-functions', '--max-blocks', String(maxBlocks), '--max-instructions', String(maxInstructions)], { cwd: root, stdio: 'inherit' });
  const metadata = JSON.parse(readFileSync(prefix + '.index.json', 'utf8'));
  if (metadata.format !== 'flash-runner-aot-index-v1' || !metadata.artifacts.length || metadata.artifacts.length > 64) throw new Error('offline AOT exceeds runtime manifest module limit');
  let wasmBytes = 0;
  let bundleBytes = 0;
  let storedBytes = 0;
  const packaged = [];
  for (const artifact of metadata.artifacts) {
    const wasm = readFileSync(join(out, artifact.file.replace(/\.aot$/, '.wasm')));
    if (!WebAssembly.validate(wasm)) throw new Error(`invalid offline Wasm: ${artifact.file}`);
    wasmBytes += wasm.length;
    const bundle = readFileSync(join(out, artifact.file));
    bundleBytes += bundle.length;
    const stored = compress ? gzipSync(bundle, { level: 9 }) : bundle;
    const file = artifact.file + (compress ? '.gz' : '');
    if (compress) writeFileSync(join(out, file), stored);
    storedBytes += stored.length;
    packaged.push({ ...artifact, file, encoding: compress ? 'gzip' : 'identity', decoded_bytes: bundle.length, stored_bytes: stored.length });
  }
  metadata.coverage.wasm_bytes = wasmBytes;
  metadata.coverage.bundle_bytes = bundleBytes;
  metadata.coverage.packaged_bytes = storedBytes;
  console.log(`${file}: ${(100 * metadata.coverage.compiled_bytes / metadata.coverage.executable_bytes).toFixed(2)}% executable-byte coverage, ${metadata.coverage.budget_frontiers} queued budget frontiers, ${storedBytes} packaged / ${bundleBytes} decoded bytes`);
  images.push({ sha256, size: bytes.length, coverage: metadata.coverage, artifacts: packaged });
}
writeFileSync(join(out, 'manifest.json'), JSON.stringify({ format: 'flash-runner-plugin-accelerators-v1', mode: complete ? 'discovered-with-fallback' : 'bounded', images }, null, 2));
console.log(`compiled static function bundles for ${images.length} runtime images: ${out}`);
