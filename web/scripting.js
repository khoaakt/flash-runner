// Plugin-side scripting objects (PPP_Instance_Private / PPP_Class_Deprecated): lets the embedder call into the
// plugin's own scriptable object — this is how Flash's ExternalInterface.addCallback functions are reached.
import { PP_OK, VT } from './pp-consts.js';

const CLASS = { HasProperty: 0, HasMethod: 4, GetProperty: 8, GetAllPropertyNames: 12, SetProperty: 16, RemoveProperty: 20, Call: 24, Construct: 28, Deallocate: 32 };

export function installScripting(P) {
  /** Ask the plugin for its instance object (PPP_Instance_Private::GetInstanceObject); returns the object's host var id or 0. */
  P.getInstanceObject = async function (instance) {
    const inst = this.instances.get(instance); if (!inst) return 0;
    if (inst.instanceObj !== undefined) return inst.instanceObj;
    const iface = await this.call(this.getPPP, [this.allocCStr('PPP_Instance_Private;0.1')]);
    if (!iface.lo) return (inst.instanceObj = 0);
    const out = this.alloc(16); this.zeroBytes(out, 16);
    await this.postTask(1, this.u32(iface.lo), [out, instance], 0, true);
    const v = this.readVar(out); this.arena.release(out);
    return (inst.instanceObj = v.type === VT.OBJECT ? v.lo : 0);
  };

  const objEntry = (host, id) => { const e = host.vars.get(id); return e && e.obj && e.obj.pluginClass ? e.obj : null; };
  const words = (v) => [v.type, 0, v.lo, v.hi];

  /** Invoke PPP_Class_Deprecated::<op> on a plugin object. args are JS values; returns { ok, value, exception, raw }. */
  P.pluginObjectOp = async function (objId, op, name, args = []) {
    const o = objEntry(this, objId); if (!o) return { ok: false, exception: 'not a plugin object' };
    const fn = this.u32(o.pluginClass + CLASS[op]);
    if (!fn) return { ok: false, exception: 'unsupported' };
    const temps = [];
    const exc = this.alloc(16); this.zeroBytes(exc, 16); temps.push(exc);
    const nameVar = name === undefined ? null : this.mkVar(name);
    let argv = 0;
    const argVars = args.map((a) => this.mkVar(a));
    if (argVars.length) { argv = this.alloc(16 * argVars.length); argVars.forEach((v, i) => this.writeVar(argv + i * 16, v)); temps.push(argv); }
    let result;
    const loop = 1;
    switch (op) {
      case 'HasProperty': case 'HasMethod': { const r = await this.postTask(loop, fn, [o.pluginData, ...words(nameVar), exc]); result = { value: (r.lo & 0xff) !== 0 }; break; }
      case 'GetProperty': case 'Call': {
        const out = this.alloc(16); this.zeroBytes(out, 16); temps.push(out);
        if (op === 'GetProperty') await this.postTask(loop, fn, [out, o.pluginData, ...words(nameVar), exc], 0, true);
        else await this.postTask(loop, fn, [out, o.pluginData, ...words(nameVar), argVars.length, argv, exc], 0, true);
        const rv = this.readVar(out); result = { value: this.varValue(rv), type: rv.type }; this.releaseVar(rv); break;
      }
      case 'SetProperty': await this.postTask(loop, fn, [o.pluginData, ...words(nameVar), ...words(argVars[0] || this.mkVar(undefined)), exc]); result = { value: undefined }; break;
      case 'RemoveProperty': await this.postTask(loop, fn, [o.pluginData, ...words(nameVar), exc]); result = { value: undefined }; break;
      case 'GetAllPropertyNames': {
        const cnt = this.alloc(8); temps.push(cnt); this.w32(cnt, 0); this.w32(cnt + 4, 0);
        await this.postTask(loop, fn, [o.pluginData, cnt, cnt + 4, exc]);
        const n = this.u32(cnt), arr = this.u32(cnt + 4), names = [];
        for (let i = 0; i < n && arr; i++) { const v = this.readVar(arr + i * 16); names.push(this.varValue(v)); }
        result = { value: names }; break;
      }
      default: result = { ok: false, exception: 'bad op' };
    }
    const ev = this.readVar(exc);
    const exception = ev.type !== VT.UNDEFINED ? String(this.varValue(ev)) : null;
    for (const t of temps) this.arena.release(t);
    if (nameVar) this.releaseVar(nameVar);
    for (const v of argVars) this.releaseVar(v);
    return { ok: !exception, exception, ...result };
  };

  /** Page -> Flash: call a callback registered with ExternalInterface.addCallback (or a classic scriptable method). */
  P.invokeInstance = async function (instance, name, args) {
    const oid = await this.getInstanceObject(instance);
    if (!oid) return { ok: false, exception: 'the movie has no scriptable object' };
    const r = await this.pluginObjectOp(oid, 'Call', name, args);
    return { ok: r.ok, exception: r.exception || null, value: r.value };
  };
}
