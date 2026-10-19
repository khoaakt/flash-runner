#!/usr/bin/env node
// Attribute profiler output to guest modules: parses a console log for the pepper-host
// `[maps] ... xtop:` ranges (executable mappings with names) and buckets both the V8 CPU profile's
// "hottest generated regions" (R<addr>, % of worker self time) and the emulator's EIPPROF histogram
// (guest address -> microseconds of scheduling-slice wall time, biased towards slice ends) into
// plugin / libc / ld / pepper-host / anonymous-x (the runtime's own JIT code cache) / other.
//   node tools/prof-split.mjs path/to/log.txt
const txt = (await import('node:fs')).readFileSync(process.argv[2], 'utf8');

const ranges = [];
for (const m of txt.matchAll(/\[maps\][^\n]*\| xtop:((?: [0-9a-f]+-[0-9a-f]+ \d+MB\/\S+)+)/g)) {
  ranges.length = 0; // keep the latest maps snapshot
  for (const r of m[1].trim().split(/\s+/)) {
    const [a, b, , name] = r.split(/-|MB\//);
    ranges.push([parseInt(a, 16), parseInt(b, 16), name]);
  }
}
const label = (addr) => {
  for (const [a, b, name] of ranges) if (addr >= a && addr < b) {
    if (name === 'anon') return 'jit-code-cache(anon-x)';
    if (name.includes('plugin')) return 'plugin(.so)';
    if (name.includes('libc')) return 'libc';
    if (name.includes('ld-') || name.includes('ld.so')) return 'ld.so';
    if (name.includes('pepper')) return 'pepper-host';
    return name;
  }
  return 'unmapped/other';
};

const bucket = (entries) => {
  const out = new Map();
  let total = 0;
  for (const [addr, w] of entries) {
    const k = label(addr);
    out.set(k, (out.get(k) || 0) + w);
    total += w;
  }
  return [out, total];
};
const show = (title, entries) => {
  const [out, total] = bucket(entries);
  if (!total) return;
  console.log(`\n${title} (total weight ${total})`);
  for (const [k, v] of [...out].sort((a, b) => b[1] - a[1])) console.log(`  ${(100 * v / total).toFixed(1).padStart(5)}%  ${k}`);
};

// V8 CPU profile: "  2.5%  R6de97190 ..." under the hottest-regions header
const regions = [];
let inRegions = false;
for (const line of txt.split('\n')) {
  if (/hottest generated regions/.test(line)) { inRegions = true; continue; }
  if (inRegions) {
    const m = line.match(/^\s*([0-9.]+)%\s+R([0-9a-f]+)/);
    if (m) regions.push([parseInt(m[2], 16), +m[1]]);
    else if (line.trim() && !/^\s*[0-9.]+%/.test(line)) inRegions = false;
  }
}
show('worker self time by guest module (V8 CPU profile, region heads)', regions);

// EIPPROF histogram blocks: take the last (most recent window)
const blocks = [...txt.matchAll(/EIPPROF\ntotal_us (\d+)\n((?:[0-9a-f]+ \d+\n?)+)/g)];
if (blocks.length) {
  const [, totalUs, body] = blocks[blocks.length - 1];
  const entries = [...body.matchAll(/([0-9a-f]+) (\d+)/g)].map((m) => [parseInt(m[1], 16), +m[2]]);
  show(`eipprof slice-end wall time by guest module (total ${(+totalUs / 1e6).toFixed(1)} s; biased to slice ends)`, entries);
  console.log('\ntop buckets:');
  for (const [a, w] of entries.sort((x, y) => y[1] - x[1]).slice(0, 12)) {
    console.log(`  ${(100 * w / +totalUs).toFixed(1).padStart(5)}%  ${a.toString(16)}  ${(w / 1e6).toFixed(2)}s  ${label(a)}`);
  }
}
if (!ranges.length) console.log('(no `xtop` maps line found: this log predates the named-maps pepper-host)');
