// Dev server: static files + the headers SharedArrayBuffer needs (COOP/COEP), gzip for big blobs.
//   node web/serve.mjs [--port 8080]
// /vendor/plugin.so is served from FLASH_PLUGIN (a runtime YOU are licensed to use) if set.
import http from 'node:http';
import { createReadStream, statSync, existsSync } from 'node:fs';
import { createGzip } from 'node:zlib';
import { extname, join, normalize, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const portArg = process.argv.indexOf('--port');
const port = +(portArg >= 0 ? process.argv[portArg + 1] : process.env.PORT || 8080); // argv[0] is the node path: never fall back to it
const MIME = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.json': 'application/json', '.swf': 'application/x-shockwave-flash', '.css': 'text/css', '.png': 'image/png', '.tar': 'application/x-tar', '.so': 'application/octet-stream' };
const MAP = {
  '/assets/rootfs.tar': join(root, 'rootfs/out/rootfs.tar'),
  '/assets/pepper-host': join(root, 'guest/out/pepper-host'),
  '/assets/fakeplug.so': join(root, 'guest/out/libfakeplug.so'),
  '/assets/fakegl.so': join(root, 'guest/out/libfakegl.so'),
  '/assets/emu.wasm': join(root, 'web/emu.wasm'),
  '/vendor/plugin.so': process.env.FLASH_PLUGIN || join(root, 'vendor-private/libpepflashplayer.so'),
};

http.createServer((req, res) => {
  let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  if (p === '/') p = '/web/index.html';
  const mapped = /^\/assets\/runtime-aot\/[a-zA-Z0-9.-]+$/.test(p) ? join(root, 'web/runtime-aot', p.split('/').pop()) : MAP[p];
  const file = mapped || normalize(join(root, p));
  if (!mapped && !file.startsWith(root + '/')) { res.writeHead(403).end(); return; }
  if (!existsSync(file) || !statSync(file).isFile()) { res.writeHead(404).end('not found: ' + p); return; }
  const st = statSync(file);
  const headers = {
    'content-type': MIME[extname(file)] || 'application/octet-stream',
    'cross-origin-opener-policy': 'same-origin', 'cross-origin-embedder-policy': 'require-corp', 'cross-origin-resource-policy': 'same-origin',
    'cache-control': 'no-cache',
  };
  headers['etag'] = `"${st.size}-${st.mtimeMs}"`; headers['last-modified'] = st.mtime.toUTCString(); headers['accept-ranges'] = 'bytes';
  const rng = /bytes=(\d*)-(\d*)/.exec(req.headers.range || '');
  if (rng) {
    const a = rng[1] === '' ? st.size - +rng[2] : +rng[1], b = rng[1] !== '' && rng[2] !== '' ? Math.min(+rng[2], st.size - 1) : st.size - 1;
    res.writeHead(206, { ...headers, 'content-range': `bytes ${a}-${b}/${st.size}`, 'content-length': b - a + 1 }); createReadStream(file, { start: a, end: b }).pipe(res); return;
  }
  const gz = /\bgzip\b/.test(req.headers['accept-encoding'] || '') && st.size > 100000 && !file.endsWith('.png');
  if (gz) { headers['content-encoding'] = 'gzip'; res.writeHead(200, headers); createReadStream(file).pipe(createGzip({ level: 6 })).pipe(res); }
  else { headers['content-length'] = st.size; res.writeHead(200, headers); createReadStream(file).pipe(res); }
}).listen(port, () => console.log(`serving ${root} on http://localhost:${port}/  (plugin: ${MAP['/vendor/plugin.so']})`));
