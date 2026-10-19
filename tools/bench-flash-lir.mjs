// Alternating A/B of the same real AS3 workload. Reports timing even on regression.
// A green pass pixel and positive wasm completions are mandatory for the LIR arm.
import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../', import.meta.url));
const option = (key, fallback) => { const i = process.argv.indexOf(key); return i < 0 ? fallback : process.argv[i + 1]; };
const runs = Number(option('--runs', 5)), loops = Number(option('--loops', 200000));
const workload = option('--workload', 'arithmetic');
const repeats = Number(option('--repeats', 30));
// --swf PATH benchmarks any fixture that draws elapsed ms at (340,240) and PASS at (350,250) (Haxe -D variants of
// LirNumberBench: const_ops, conv_ops, trunc_ops, single_op); --families a,b limits which LIR families lower.
const swfOption = option('--swf', null), families = option('--families', null);
if (!['arithmetic', 'loops', 'memory', 'number-bench', 'memory-bench', 'int-bench', 'field-bench'].includes(workload) && !swfOption || !Number.isInteger(repeats) || repeats < 1 || repeats > 100) throw new Error('invalid workload/repeats');
if (!Number.isInteger(runs) || runs < 1 || runs > 20 || !Number.isInteger(loops) || loops < 1 || loops > 2000000) throw new Error('invalid benchmark limits');
const samples = [];
for (let run = 0; run < runs; run++) {
  for (const mode of run % 2 ? ['lir', 'native'] : ['native', 'lir']) {
    const args = ['web/flash-smoke.mjs', '--plugin', 'vendor-private/libpepflashplayer-32.0.0.363.so', '--swf', swfOption || `tests/swf/lir-${workload}.swf`,
      '--flashvars', workload === 'arithmetic' ? `loops=${loops}` : `repeats=${repeats}`, '--seconds', '90', '--wait-for-pass', '--read-timing-pixel', '--expect-pixel', '360,260,00ff00'];
    if (mode === 'lir') args.push('--lir', ...(families ? ['--lir-families', families] : []));
    const processResult = spawnSync(process.execPath, args, { cwd: root, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
    const output = processResult.stdout + processResult.stderr;
    if (processResult.status !== 0) throw new Error(output || String(processResult.error));
    const ms = Number(/guest workload (\d+) ms/.exec(output)?.[1]);
    const completed = Number(/completed_native_leaf_calls=(\d+)/.exec(output)?.[1] || 0);
    const loopMethods = Number(/"loweredLoops":(\d+)/.exec(output)?.[1] || 0);
    const numberMethods = Number(/"loweredNumbers":(\d+)/.exec(output)?.[1] || 0);
    const memoryMethods = Number(/"loweredMemory":(\d+)/.exec(output)?.[1] || 0);
    const typedMatch = /typed hot calls (\d+) ms/.exec(output);
    const typedMs = typedMatch ? Number(typedMatch[1]) : null;
    if (!Number.isFinite(ms) || ms <= 0 || (mode === 'lir' && completed === 0)) throw new Error('benchmark did not execute intended workload\n' + output);
    const sample = { mode, run, ms, typedMs, completed, loopMethods, memoryMethods, numberMethods }; samples.push(sample);
    console.log(JSON.stringify(sample));
  }
}
const median = (a) => { a.sort((x,y) => x-y); return (a[Math.floor((a.length-1)/2)] + a[Math.floor(a.length/2)]) / 2; };
const baseline = median(samples.filter((s) => s.mode === 'native').map((s) => s.ms));
const lowered = median(samples.filter((s) => s.mode === 'lir').map((s) => s.ms));
const range = (a) => [Math.min(...a), Math.max(...a)];
const result = { workload: 'real-flash-lir-' + (swfOption || workload), families: families || 'all', loops, nativeRangeMs: range(samples.filter((s) => s.mode === 'native').map((s) => s.ms)), lirRangeMs: range(samples.filter((s) => s.mode === 'lir').map((s) => s.ms)), repeats, runs, samples, nativeMedianMs: baseline, lirMedianMs: lowered,
  speedup: baseline / lowered, allLoopProfilesInstalled: workload === 'loops' ? samples.filter((s) => s.mode === 'lir').every((s) => s.loopMethods === 2) : null };
console.log(JSON.stringify(result, null, 2));
const output = option('--out', null); if (output) writeFileSync(output, JSON.stringify(result, null, 2));
