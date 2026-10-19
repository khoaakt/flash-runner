// Which LIR families the shipped worker lowers. Every family stays correct and fully tested; this only
// decides which ones may replace native code at runtime.
//
// The flag for each family is derived from the recorded real-Flash A/B below (alternating runs of
// tools/bench-flash-lir.mjs-style timing, medians in ms, guest workload time): a family is on when LIR is
// faster than native on its primary workload, the early-bound/typed call path that real AS3 code uses.
// Re-measure and edit the numbers together with the code; web/lir-policy-test.mjs keeps flags and numbers
// consistent. `others` records workloads that disagree, so a veto stays an informed one.
export const LIR_FAMILIES = Object.freeze(['integer', 'numbers', 'memory', 'loops']);

export const LIR_EVIDENCE = Object.freeze({
  integer: { primary: 'lir-int-bench (typed static Int leaves)', native: 95.5, lir: 74,
    others: ['arithmetic (loop-dominated): 2084 vs 2112 ms, ranges overlap'] },
  numbers: { primary: 'lir-number-bench (nested Number programs)', native: 156, lir: 122,
    others: ['single operation: 126 vs 103 ms', 'constants: 129.5 vs 107 ms', 'int32 -> Number: 141.5 vs 96 ms',
      'Number -> int (Std.int): 105.5 vs 82.5 ms', 'receiver Number fields (getters, arithmetic, read-modify-write): 127 vs 109.5 ms',
      'every call refused (|x| >= 2^31), adapter retired after 4096 refusals: 156 vs 166 ms (0.94x); 0.29x before refusals were charged to the LIR region',
      'before the return chain and inline FP: 156 vs 403 ms (0.39x)'] },
  memory: { primary: 'lir-memory-bench (typed getters/setters)', native: 221, lir: 178.5,
    others: ['lir-memory (calls through Dynamic function objects): 612 vs 659 ms (0.93x, ranges disjoint)'] },
  loops: { primary: 'lir-loops repeats=100', native: 238, lir: 204, others: [] },
});

export const ALL_LIR_FAMILIES = Object.freeze(Object.fromEntries(LIR_FAMILIES.map((f) => [f, true])));
export const WORKER_LIR_FAMILIES = Object.freeze(Object.fromEntries(
  LIR_FAMILIES.map((f) => [f, LIR_EVIDENCE[f].lir < LIR_EVIDENCE[f].native])));

// Accepts {family: bool} or a comma list ("integer,loops"); unknown names are rejected.
export function resolveLirFamilies(spec, base = ALL_LIR_FAMILIES) {
  if (spec == null) return { ...base };
  if (typeof spec === 'string') {
    const names = spec.split(',').filter(Boolean);
    for (const name of names) if (!LIR_FAMILIES.includes(name)) throw new Error(`unknown LIR family ${name}`);
    spec = Object.fromEntries(LIR_FAMILIES.map((f) => [f, names.includes(f)]));
  }
  for (const key of Object.keys(spec)) if (!LIR_FAMILIES.includes(key)) throw new Error(`unknown LIR family ${key}`);
  return { ...base, ...Object.fromEntries(Object.entries(spec).map(([k, v]) => [k, !!v])) };
}
