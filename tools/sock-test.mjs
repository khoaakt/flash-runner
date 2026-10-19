// End to end test of Flash raw sockets in Node: a local echo server that also answers socket policy-file requests,
// the real Flash runtime running tests/swf/sock.swf, and the Node socket backend.
//   node tools/sock-test.mjs [--plugin vendor-private/libpepflashplayer-32.0.0.363.so]
import net from 'node:net';
import { spawn } from 'node:child_process';
const arg = (n, d) => { const i = process.argv.indexOf(n); return i < 0 ? d : process.argv[i + 1]; };
const plugin = arg('--plugin', 'vendor-private/libpepflashplayer-32.0.0.363.so');
const log = [];
const server = net.createServer((c) => {
  let first = true;
  c.on('data', (d) => {
    const t = d.toString('latin1');
    log.push(JSON.stringify(t.slice(0, 40)));
    if (first && t.startsWith('<policy-file-request/>')) { c.end('<?xml version="1.0"?><cross-domain-policy><allow-access-from domain="*" to-ports="*"/></cross-domain-policy>\0'); return; }
    first = false; c.write(d);
  });
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const port = server.address().port;
const child = spawn('node', ['web/flash-smoke.mjs', '--plugin', plugin, '--swf', 'tests/swf/sock.swf', '--seconds', '10', '--flashvars', `host=127.0.0.1&port=${port}`]);   // async: the server above must keep running
let text = '';
child.stdout.on('data', (d) => (text += d)); child.stderr.on('data', (d) => (text += d));
await new Promise((r) => child.on('close', r));
const nav = /NAVIGATE -> (.*?) target=/.exec(text)?.[1] ?? '';
server.close();
let failed = 0;
const check = (name, cond, extra = '') => { console.log((cond ? 'PASS ' : 'FAIL ') + name + (cond ? '' : '  ' + extra)); if (!cond) failed++; };
console.log('server saw:', log.join(' '));
console.log('movie reported:', nav.replace(/\|/g, ' | '));
check('Flash requested the socket policy file first', log[0]?.includes('policy-file-request'), log.join(' '));
check('Socket.connect succeeded', /connect=ok/.test(nav), nav);
check('data written by Flash reached the server', log.some((l) => l.includes('hello socket')), log.join(' '));
check('echoed data came back to Flash', /data=hello socket/.test(nav), nav);
check('no unimplemented socket calls', !/UNIMPLEMENTED PPB_(TCP|NetAddress)/.test(text), (text.match(/UNIMPLEMENTED.*/g) || []).join('\n'));
process.exit(failed ? 1 : 0);
