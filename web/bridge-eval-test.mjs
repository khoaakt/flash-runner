// The page bridge's eval-free path (extension/content/bridge-main.js, `evalFree`) against what Flash's own helper scripts do under real eval:
// every script is run both ways in the same fake page and the results must be identical.
import vm from 'node:vm';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../extension/content/bridge-main.js'), 'utf8');
const page = () => {
  const ctx = vm.createContext({
    document: { querySelectorAll: () => [], localName: 'html' }, MutationObserver: class { observe() {} }, setTimeout, clearTimeout, URL,
    addEventListener() {}, console,
  });
  ctx.window = ctx;
  vm.runInContext(`globalThis.window = globalThis; ${src}`, ctx);
  return ctx;
};

// Flash's helper definitions, as recorded from ExecuteScript (32.0.0.363, ExternalInterface.call).
const HELPERS = 'function __flash__arrayToXML(obj) { var s = "<array>"; for (var i=0; i<obj.length; i++) { s += "<property id=\\"" + i + "\\">" + __flash__toXML(obj[i]) + "</property>"; } return s+"</array>"; } function __flash__argumentsToXML(obj,index) { var s = "<arguments>"; for (var i=index; i<obj.length; i++) { s += __flash__toXML(obj[i]); } return s+"</arguments>"; } function __flash__objectToXML(obj) { var s = "<object>"; for (var prop in obj) { s += "<property id=\\"" + prop + "\\">" + __flash__toXML(obj[prop]) + "</property>"; } return s+"</object>"; } function __flash__escapeXML(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/\'/g, "&apos;"); } function __flash__toXML(value) { var type = typeof(value); if (type == "string") { return "<string>" + __flash__escapeXML(value) + "</string>"; } else if (type == "undefined") { return "<undefined/>"; } else if (type == "number") { return "<number>" + value + "</number>"; } else if (value == null) { return "<null/>"; } else if (type == "boolean") { return value ? "<true/>" : "<false/>"; } else if (value instanceof Date) { return "<date>" + value.getTime() + "</date>"; } else if (value instanceof Array) { return __flash__arrayToXML(value); } else if (type == "object") { return __flash__objectToXML(value); } else { return "<null/>"; //???\n } } function __flash__request(name) { return "<invoke name=\\""+name+"\\" returntype=\\"javascript\\">" + __flash__argumentsToXML(arguments,1) + "</invoke>"; }';
const wrap = (call) => `try { __flash__toXML(${call}) ; } catch (e) { "<undefined/>"; }`;

const setup = `
  var seen = [];
  function pageEcho(s, n, b, o) { seen.push(JSON.stringify([s, n, b, o])); return { ok: true, echoed: s, list: [1, 2, 3], none: null, nothing: undefined }; }
  function boom() { throw new Error('x'); }
  function giveDate() { return new Date(86400000); }
  function giveFn() { return function () {}; }
  var ns = { greet: function (n) { return 'hi ' + n + (this === ns ? '!' : '?'); } };
  function noReturn() {}
`;
const calls = [
  'pageEcho("hello",42,true,({a:1,b:[1,2]}))',
  'pageEcho("it\'s \\"quoted\\" <&> \\\\ back\\nnew\\ttab \\u00e9 \\x41",-1.5e3,false,null)',
  'pageEcho(undefined, NaN, Infinity, ({"k-1":"v","__proto__":1}))',
  'pageEcho("",0,true,[])',
  'pageEcho([[1,[2]],({})],.5,true,({a:({b:({c:"d"})})}))',
  'ns.greet("you")', 'noReturn()', 'giveDate()', 'giveFn()', 'boom()', 'missing(1)', 'ns.missing(1)',
  'seen.length', 'ns.greet(seen.length)', 'pageEcho("x",1,true,null).echoed', 'ns.greet(ns.greet("a"))', 'pageEcho(void(0),2,false,[pageEcho("in",0,false,null).list])',
];
let failed = 0;
const t = (name, fn) => { try { fn(); console.log('ok   ' + name); } catch (e) { failed++; console.log('FAIL ' + name + '\n     ' + (e && e.message)); } };

for (const call of calls) {
  t('same answer as eval: ' + call.slice(0, 60), () => {
    const a = page(), b = page();
    vm.runInContext(setup, a); vm.runInContext(setup, b);
    vm.runInContext(HELPERS, a);                                     // eval path: Flash's helpers defined, then the call
    const viaEval = vm.runInContext(wrap(call), a);
    assert.equal(b.window.__flashRunnerBridge.evalFree.exec(HELPERS), undefined);
    const free = b.window.__flashRunnerBridge.evalFree.exec(wrap(call));
    assert.equal(free, viaEval);
    assert.equal(vm.runInContext('seen.join("|")', b), vm.runInContext('seen.join("|")', a), 'the page function saw the same arguments');
  });
}

t('the answer to a call into Flash is parsed like eval would', () => {
  const p = page(), free = p.window.__flashRunnerBridge.evalFree;
  for (const e of ['"flash:hi"', '42.5', '-3', 'true', 'null', 'undefined', '[1,"two",true,null]', '({n:1,arr:[1,"two",true,null],nested:{k:"v"}})', '"a\\"b\\n"']) {
    assert.deepEqual(JSON.parse(JSON.stringify(free.literal(e) ?? null)), JSON.parse(JSON.stringify(vm.runInContext('(' + e + ')', page()) ?? null)), e);
  }
});

t('bare statements (a javascript: link) give the value eval gives', () => {
  for (const script of ['ns.greet("a")', 'seen.push(1); ns.greet("b");', 'void(0)', 'void(seen.push(2))', 'pageEcho("x",1,true,null).list', 'seen.length', 'missing.deeper(1)', 'ns.greet(missing)', '1.5', '"s"']) {
    const a = page(), b = page();
    vm.runInContext(setup, a); vm.runInContext(setup, b);
    let viaEval, viaFree, errEval, errFree;
    try { viaEval = vm.runInContext(script, a); } catch (e) { errEval = e.name; }
    try { viaFree = b.window.__flashRunnerBridge.evalFree.exec(script); } catch (e) { errFree = e.name; }
    assert.equal(errFree, errEval, script);
    assert.equal(JSON.stringify(viaFree), JSON.stringify(viaEval), script);
    assert.equal(vm.runInContext('seen.length', b), vm.runInContext('seen.length', a), script);
  }
});

t('anything that is not a recorded shape is refused, never evaluated', () => {
  const p = page(), free = p.window.__flashRunnerBridge.evalFree;
  vm.runInContext('var hit = 0;', p);
  for (const bad of ['hit = 1', 'try { hit = 1; } catch (e) { "<undefined/>"; }', wrap('pageEcho(hit = 1)'), wrap('pageEcho(function(){})'), wrap('pageEcho(1+1)'), wrap('a.b[0]()'), 'seen.push(1); hit = 1', 'seen.push(1) hit = 1', 'void(seen.push(1)) || 2',
    'try { __flash__toXML(f(1)) ; } catch (e) { hit = 1; }', wrap('f(1)') + ' hit = 1', wrap('f(' + '['.repeat(200) + ']'.repeat(200) + ')')]) {
    assert.throws(() => free.exec(bad), (e) => e.name === 'SyntaxError', bad);
  }
  assert.equal(vm.runInContext('hit', p), 0);
  for (const bad of ['1+1', 'foo()']) assert.throws(() => free.literal(bad), (e) => e.name === 'SyntaxError', bad);
});

console.log(failed ? failed + ' FAILED' : 'all ok');
process.exit(failed ? 1 : 0);
