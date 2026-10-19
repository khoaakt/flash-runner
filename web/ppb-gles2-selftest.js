// Emulator-free self test of the PPB_Graphics3D / PPB_OpenGLES2* handlers: a fake Pepper host whose "guest memory" is a
// plain ArrayBuffer, driving the real handler tables on a real WebGL2 context (browser only).
//   import { runSelfTest } from './ppb-gles2-selftest.js';  const r = await runSelfTest({ base: 0 });
// `base` is the offset of guest address 0 inside the ArrayBuffer: 0 exercises the zero-copy aligned paths, 1/2 force
// the unaligned fallbacks (copying) for float/int/16-bit data.
import { buildGLES2 } from './ppb-gles2.js';
import { METHODS, PPB } from './ppapi-sigs.js';

const RGBA = 0x1908, UBYTE = 0x1401, FLOAT = 0x1406, TRI = 4;

export async function runSelfTest({ base = 0, verbose = false } = {}) {
  const results = [], logs = [];
  const t = (name, ok, detail = '') => { results.push({ name, ok: !!ok, detail: String(detail) }); };
  const eq = (name, got, want) => t(name, JSON.stringify(got) === JSON.stringify(want), `got ${JSON.stringify(got)} want ${JSON.stringify(want)}`);

  // ---- fake host ------------------------------------------------------------------------------------
  // base >= 2^31 puts "guest RAM" above 2 GiB of a real 2.3 GB wasm memory (lazily committed): the range where signed shifts/coercions break
  const buf = base >= 2 ** 31 ? new WebAssembly.Memory({ initial: 36000 }).buffer : new ArrayBuffer(16 << 20), u8 = new Uint8Array(buf), dv = new DataView(buf);
  let bump = 0x10000;
  const swaps = [], pending = [], hostCalls = [];
  const host = {
    emu: { mem: { buffer: buf }, x: { emu_guest_base: () => base | 0 } },      // like the wasm export: the i32 bit pattern
    log: (s) => logs.push(s), res: new Map(), nextRes: 2, instances: new Map([[1, { id: 1, g2d: 0 }]]),
    threadLoop: new Map(), mainTid: 0,
    opts: { frameMs: 1, onSwap: (inst, canvas, w, h, ctx) => swaps.push({ inst, canvas, w, h, ctx }) },
    newRes(o) { const id = this.nextRes++; o.refs = 1; this.res.set(id, o); return id; },
    getRes(id, type) { const r = this.res.get(id); return r && (!type || r.type === type) ? r : null; },
    releaseRes(id) { const r = this.res.get(id); if (!r) return; if (--r.refs <= 0) { r.onFree && r.onFree(); this.res.delete(id); } },
    alloc(n) { const p = bump; bump += (n + 15) & ~15; if (bump > (buf.byteLength - base - 64)) throw new Error('fake arena exhausted'); return p; },
    arena: { release() {} },
    allocCStr(s) { const b = new TextEncoder().encode(s), p = this.alloc(b.length + 1); u8.set(b, base + p); u8[base + p + b.length] = 0; return p; },
    getPPP: 0x7001, u32(a) { return dv.getUint32(base + a, true); },
    call(fn, args) { hostCalls.push({ fn, args }); return Promise.resolve({ lo: fn === 0x7001 ? 0x4000 : 0, hi: 0 }); },
    finishAsync(tid, cc, loop, run) { if (!cc.func) return new Promise((r) => run(r)); run((res) => pending.push({ cc, res })); return -1; },
  };
  dv.setUint32(base + 0x4000, 0x5555, true);        // PPP_Graphics_3D;1.0 table: Graphics3DContextLost = 0x5555
  const tables = buildGLES2(host);
  let info = {};
  const calls = new Set();
  const mkProxy = (iface, getCtx) => new Proxy({}, {
    get: (_, name) => (...a) => {
      calls.add(iface + '::' + name);
      const fn = tables[iface][name];
      if (!fn) throw new Error('no handler ' + iface + '::' + name);
      const args = a.map((v) => (typeof v === 'number' && v < 0 ? v >>> 0 : v));
      return getCtx ? fn(0, getCtx(), ...args) : fn(0, ...args);
    },
  });
  let ctxId = 0;
  const gl = mkProxy('PPB_OpenGLES2', () => ctxId);
  const g3d = mkProxy('PPB_Graphics3D', null);
  const inst = mkProxy('PPB_OpenGLES2InstancedArrays', () => ctxId), blit = mkProxy('PPB_OpenGLES2FramebufferBlit', () => ctxId);
  const msaa = mkProxy('PPB_OpenGLES2FramebufferMultisample', () => ctxId), feat = mkProxy('PPB_OpenGLES2ChromiumEnableFeature', () => ctxId);
  const qry = mkProxy('PPB_OpenGLES2Query', () => ctxId), vao = mkProxy('PPB_OpenGLES2VertexArrayObject', () => ctxId);
  const dbuf = mkProxy('PPB_OpenGLES2DrawBuffers(Dev)', () => ctxId), msub = mkProxy('PPB_OpenGLES2ChromiumMapSub', () => ctxId);
  const tmap = mkProxy('PPB_GLESChromiumTextureMapping(Dev)', () => ctxId);

  // ---- guest memory helpers ---------------------------------------------------------------------------
  const put = (bytes) => { const p = host.alloc(Math.max(bytes.length, 4)); u8.set(bytes, base + p); return p; };
  const putF = (arr) => put(new Uint8Array(new Float32Array(arr).buffer));
  const putI = (arr) => put(new Uint8Array(new Int32Array(arr).buffer));
  const putU16 = (arr) => put(new Uint8Array(new Uint16Array(arr).buffer));
  const str = (s) => host.allocCStr(s);
  const rd32 = (p) => dv.getInt32(base + p, true), rdu32 = (p) => dv.getUint32(base + p, true), rdf = (p) => dv.getFloat32(base + p, true);
  const rdBytes = (p, n) => Array.from(u8.subarray(base + p, base + p + n));
  const rdStr = (p) => { let e = base + p; while (u8[e]) e++; return new TextDecoder().decode(u8.subarray(base + p, e)); };
  const zero = (p, n) => u8.fill(0, base + p, base + p + n);
  const scratch = host.alloc(4096);
  const geti = (pname) => { gl.GetIntegerv(pname, scratch); return rd32(scratch); };
  const genOne = (fn) => { fn(1, scratch); return rdu32(scratch); };
  const px = (x, y) => { zero(scratch, 8); gl.ReadPixels(x, y, 1, 1, RGBA, UBYTE, scratch); return rdBytes(scratch, 4); };
  const near = (a, b, tol = 2) => a.length === b.length && a.every((v, i) => Math.abs(v - b[i]) <= tol);
  const noErr = (name) => { const e = gl.GetError(); t(name + ': no GL error', e === 0, '0x' + e.toString(16)); };
  const drainErr = () => { let e, n = 0; while ((e = gl.GetError()) !== 0 && n++ < 8); };

  const canvasPx = (canvas, w, h, x, y) => { const c = new OffscreenCanvas(w, h).getContext('2d', { willReadFrequently: true }); c.drawImage(canvas, 0, 0); return Array.from(c.getImageData(x, y, 1, 1).data); };
  function makeProgram(vsSrc, fsSrc) {
    const mk = (type, src) => {
      const s = gl.CreateShader(type);
      const p = str(src); const arr = putI([p]);
      gl.ShaderSource(s, 1, arr, 0); gl.CompileShader(s);
      gl.GetShaderiv(s, 0x8B81, scratch);
      return { s, ok: rd32(scratch) === 1 };
    };
    const v = mk(0x8B31, vsSrc), f = mk(0x8B30, fsSrc);
    const p = gl.CreateProgram();
    gl.AttachShader(p, v.s); gl.AttachShader(p, f.s);
    gl.BindAttribLocation(p, 0, str('a_pos')); gl.BindAttribLocation(p, 1, str('a_uv')); gl.BindAttribLocation(p, 2, str('a_col'));
    gl.LinkProgram(p);
    gl.GetProgramiv(p, 0x8B82, scratch);
    return { p, v, f, vsOk: v.ok, fsOk: f.ok, linked: rd32(scratch) === 1 };
  }
  const VS = 'attribute vec2 a_pos; attribute vec2 a_uv; attribute vec4 a_col; varying vec2 v_uv; varying vec4 v_col; uniform vec2 u_off;\nvoid main(){ v_uv=a_uv; v_col=a_col; gl_Position=vec4(a_pos+u_off,0.0,1.0); }';
  const FS = 'precision mediump float; varying vec2 v_uv; varying vec4 v_col; uniform sampler2D u_tex; uniform float u_mix; uniform vec4 u_arr[2];\nvoid main(){ vec4 t=texture2D(u_tex,v_uv); gl_FragColor=mix(v_col,t,u_mix); gl_FragColor.rgb += u_arr[0].rgb; gl_FragColor.a *= u_arr[1].a; }';

  try {
  // ---- Graphics3D ---------------------------------------------------------------------------------------
  let attr = putI([0x3057, 64, 0x3056, 64, 0x3021, 8, 0x3025, 24, 0x3026, 8, 0x3038]);
  t('Create fails for unknown instance', g3d.Create(99, 0, attr) === 0);
  ctxId = g3d.Create(1, 0, attr);
  t('Graphics3D::Create returns a resource', ctxId > 0, ctxId);
  if (!ctxId) return finish();
  t('IsGraphics3D', g3d.IsGraphics3D(ctxId) === 1 && g3d.IsGraphics3D(12345) === 0);
  const ga = putI([0x3057, 0, 0x3056, 0, 0x3021, 0, 0x3025, 0, 0x3026, 0, 0x3032, 0, 0x3038]);
  eq('GetAttribs ok', g3d.GetAttribs(ctxId, ga), 0);
  eq('GetAttribs values', [rd32(ga + 4), rd32(ga + 12), rd32(ga + 20), rd32(ga + 28) > 0, rd32(ga + 36) > 0], [64, 64, 8, true, true]);
  eq('GetAttribs bad attrib', g3d.GetAttribs(ctxId, putI([0x1234, 0, 0x3038])) < 0, true);
  const mx = host.alloc(16);
  eq('GetAttribMaxValue', [g3d.GetAttribMaxValue(1, 0x3025, mx), rd32(mx)], [0, 24]);
  eq('SetAttribs(swap behavior)', g3d.SetAttribs(ctxId, putI([0x3093, 0x3095, 0x3038])), 0);
  eq('Graphics3D::GetError', g3d.GetError(ctxId), 0);
  t('Create with share_context=bogus fails', g3d.Create(1, 777, attr) === 0);

  // ---- strings and limits -------------------------------------------------------------------------------
  const sv = gl.GetString(0x1F00), sr = gl.GetString(0x1F01), sver = gl.GetString(0x1F02), ssl = gl.GetString(0x8B8C), sext = gl.GetString(0x1F03);
  t('GetString VENDOR/RENDERER/VERSION/GLSL', sv && sr && /^OpenGL ES 2\.0/.test(rdStr(sver)) && /^OpenGL ES GLSL ES 1\.0/.test(rdStr(ssl)), [rdStr(sv), rdStr(sr), rdStr(sver), rdStr(ssl)].join(' | '));
  t('GetString is cached (stable pointer)', gl.GetString(0x1F03) === sext);
  const exts = rdStr(sext).split(' ');
  info = { vendor: rdStr(sv), renderer: rdStr(sr), version: rdStr(sver), glsl: rdStr(ssl), extensions: rdStr(sext) };
  for (const n of ['GL_OES_standard_derivatives', 'GL_EXT_texture_format_BGRA8888', 'GL_OES_packed_depth_stencil', 'GL_ANGLE_instanced_arrays', 'GL_EXT_occlusion_query_boolean', 'GL_OES_vertex_array_object', 'GL_CHROMIUM_map_sub', 'GL_OES_texture_float', 'GL_OES_depth_texture']) t('extension ' + n, exts.includes(n));
  eq('GetString(bad) -> 0 + INVALID_ENUM', [gl.GetString(0x1234), gl.GetError()], [0, 0x500]);
  t('MAX_TEXTURE_SIZE', geti(0xD33) >= 2048);
  t('MAX_VERTEX_ATTRIBS', geti(0x8869) >= 8);
  t('NUM_COMPRESSED_TEXTURE_FORMATS', geti(0x86A2) >= 0);
  gl.GetIntegerv(0xBA2, scratch);
  eq('VIEWPORT initial', [rd32(scratch), rd32(scratch + 4), rd32(scratch + 8), rd32(scratch + 12)], [0, 0, 64, 64]);
  gl.GetFloatv(0xC22, scratch);
  eq('COLOR_CLEAR_VALUE initial', [rdf(scratch), rdf(scratch + 4), rdf(scratch + 8), rdf(scratch + 12)], [0, 0, 0, 0]);
  zero(scratch, 16); gl.GetBooleanv(0xC23, scratch);
  eq('COLOR_WRITEMASK', rdBytes(scratch, 4), [1, 1, 1, 1]);
  gl.GetShaderPrecisionFormat(0x8B30, 0x8DF2, scratch, scratch + 8);
  t('GetShaderPrecisionFormat(fragment, highp float)', rd32(scratch + 8) >= 16 || rd32(scratch + 8) === 0, [rd32(scratch), rd32(scratch + 4), rd32(scratch + 8)].join(','));
  noErr('limits');

  // ---- state and error semantics --------------------------------------------------------------------------
  gl.Enable(0xBE2); eq('IsEnabled(BLEND)', gl.IsEnabled(0xBE2), 1); gl.Disable(0xBE2); eq('IsEnabled after Disable', gl.IsEnabled(0xBE2), 0);
  eq('IsEnabled(DITHER) default', gl.IsEnabled(0xBD0), 1);
  gl.Enable(0x1234); gl.Viewport(0, 0, -1, 5);
  eq('first error wins (INVALID_ENUM before INVALID_VALUE)', gl.GetError(), 0x500);
  eq('GetError clears', gl.GetError(), 0);
  gl.BindBuffer(0x8892, 0); gl.VertexAttribPointer(0, 2, FLOAT, 0, 0, 16);
  eq('client-side vertex array -> INVALID_OPERATION', gl.GetError(), 0x502);
  gl.DrawElements(TRI, 3, 0x1403, 0);
  eq('DrawElements without index buffer -> INVALID_OPERATION', gl.GetError(), 0x502);
  eq('GetUniformLocation(bad program) -> -1 + error', [gl.GetUniformLocation(4242, str('x')) | 0, gl.GetError()], [-1, 0x501]);

  // ---- a triangle from a VBO ------------------------------------------------------------------------------
  gl.Viewport(0, 0, 64, 64); gl.Disable(0xC11);
  gl.ClearColor(0, 0, 1, 1); gl.Clear(0x4100 | 0x400);
  const prog = makeProgram(VS, FS);
  t('vertex+fragment shader compile and link', prog.vsOk && prog.fsOk && prog.linked, JSON.stringify([prog.vsOk, prog.fsOk, prog.linked]));
  gl.UseProgram(prog.p);
  eq('GetIntegerv(CURRENT_PROGRAM)', geti(0x8B8D), prog.p);
  const vbo = genOne((n, p) => gl.GenBuffers(n, p));
  gl.BindBuffer(0x8892, vbo); eq('IsBuffer after bind', gl.IsBuffer(vbo), 1);
  const verts = new Float32Array([-0.8, -0.8, 0, 0, 1, 0, 0, 1, 0.8, -0.8, 1, 0, 1, 0, 0, 1, 0, 0.8, 0.5, 1, 1, 0, 0, 1]);
  gl.BufferData(0x8892, verts.byteLength, putF(verts), 0x88E4);
  gl.GetBufferParameteriv(0x8892, 0x8764, scratch); eq('BUFFER_SIZE', rd32(scratch), verts.byteLength);
  gl.GetBufferParameteriv(0x8892, 0x8765, scratch); eq('BUFFER_USAGE', rd32(scratch), 0x88E4);
  for (const [i, n, off] of [[0, 2, 0], [1, 2, 8], [2, 4, 16]]) { gl.EnableVertexAttribArray(i); gl.VertexAttribPointer(i, n, FLOAT, 0, 32, off); }
  const locOff = gl.GetUniformLocation(prog.p, str('u_off')) | 0, locTex = gl.GetUniformLocation(prog.p, str('u_tex')) | 0, locMix = gl.GetUniformLocation(prog.p, str('u_mix')) | 0, locArr = gl.GetUniformLocation(prog.p, str('u_arr')) | 0;
  t('uniform locations found', locOff >= 0 && locTex >= 0 && locMix >= 0 && locArr >= 0, [locOff, locTex, locMix, locArr].join(','));
  const locArr1 = gl.GetUniformLocation(prog.p, str('u_arr[1]')) | 0;
  t('uniform array element location', locArr1 > locArr || locArr1 >= 0, locArr1);
  eq('GetUniformLocation(missing) = -1', gl.GetUniformLocation(prog.p, str('nope')) | 0, -1);
  gl.Uniform2f(locOff, 0, 0); gl.Uniform1f(locMix, 0); gl.Uniform1i(locTex, 0);
  gl.Uniform4fv(locArr, 2, putF([0, 0, 0, 0, 0, 0, 0, 1]));
  gl.Uniform1f(-1, 1);
  noErr('uniform setup (location -1 ignored)');
  gl.GetUniformfv(prog.p, locArr1, scratch); eq('GetUniformfv(u_arr[1])', [rdf(scratch), rdf(scratch + 4), rdf(scratch + 8), rdf(scratch + 12)], [0, 0, 0, 1]);
  gl.GetUniformfv(prog.p, locMix, scratch); eq('GetUniformfv(u_mix)', rdf(scratch), 0);
  gl.DrawArrays(TRI, 0, 3);
  noErr('DrawArrays');
  eq('triangle pixel (32,20) is red', px(32, 20), [255, 0, 0, 255]);
  eq('outside pixel (2,60) is the clear colour', px(2, 60), [0, 0, 255, 255]);
  gl.GetVertexAttribiv(2, 0x8623, scratch); eq('GetVertexAttribiv SIZE', rd32(scratch), 4);
  gl.GetVertexAttribiv(2, 0x889F, scratch); eq('GetVertexAttribiv BUFFER_BINDING is a GLES name', rd32(scratch), vbo);
  gl.GetVertexAttribPointerv(2, 0x8645, scratch); eq('GetVertexAttribPointerv', rd32(scratch), 16);
  gl.VertexAttrib4f(5, 1, 2, 3, 4); gl.GetVertexAttribfv(5, 0x8626, scratch); eq('VertexAttrib4f / CURRENT_VERTEX_ATTRIB', [rdf(scratch), rdf(scratch + 4), rdf(scratch + 8), rdf(scratch + 12)], [1, 2, 3, 4]);
  gl.VertexAttrib1f(5, 7); gl.VertexAttrib2f(5, 1, 2); gl.VertexAttrib3f(5, 1, 2, 3);
  gl.VertexAttrib1fv(5, putF([9])); gl.VertexAttrib2fv(5, putF([9, 8])); gl.VertexAttrib3fv(5, putF([9, 8, 7])); gl.VertexAttrib4fv(5, putF([9, 8, 7, 6]));
  gl.GetVertexAttribfv(5, 0x8626, scratch); eq('VertexAttrib4fv', [rdf(scratch), rdf(scratch + 4), rdf(scratch + 8), rdf(scratch + 12)], [9, 8, 7, 6]);
  gl.DisableVertexAttribArray(5);

  // program introspection
  gl.GetProgramiv(prog.p, 0x8B86, scratch); const nU = rd32(scratch); t('ACTIVE_UNIFORMS >= 4', nU >= 4, nU);
  gl.GetProgramiv(prog.p, 0x8B89, scratch); eq('ACTIVE_ATTRIBUTES', rd32(scratch), 3);
  gl.GetProgramiv(prog.p, 0x8B85, scratch); eq('ATTACHED_SHADERS', rd32(scratch), 2);
  gl.GetProgramiv(prog.p, 0x8B87, scratch); t('ACTIVE_UNIFORM_MAX_LENGTH', rd32(scratch) >= 7, rd32(scratch));
  gl.GetProgramiv(prog.p, 0x8B8A, scratch); t('ACTIVE_ATTRIBUTE_MAX_LENGTH', rd32(scratch) >= 6, rd32(scratch));
  const names = [];
  for (let i = 0; i < nU; i++) { gl.GetActiveUniform(prog.p, i, 64, scratch, scratch + 4, scratch + 8, scratch + 16); names.push(rdStr(scratch + 16) + ':' + rd32(scratch + 4) + ':' + rd32(scratch)); }
  t('GetActiveUniform enumerates u_arr[0] size 2', names.some((n) => n.startsWith('u_arr[0]:2')), names.join(' '));
  gl.GetActiveAttrib(prog.p, 0, 64, scratch, scratch + 4, scratch + 8, scratch + 16);
  t('GetActiveAttrib', /^a_(pos|uv|col)$/.test(rdStr(scratch + 16)) && rd32(scratch + 8) > 0, rdStr(scratch + 16));
  eq('GetAttribLocation(a_col)', gl.GetAttribLocation(prog.p, str('a_col')), 2);
  gl.GetAttachedShaders(prog.p, 8, scratch, scratch + 16); eq('GetAttachedShaders', [rd32(scratch), rdu32(scratch + 16), rdu32(scratch + 20)], [2, prog.v.s, prog.f.s]);
  gl.ValidateProgram(prog.p);
  gl.GetProgramiv(prog.p, 0x8B83, scratch); t('VALIDATE_STATUS', rd32(scratch) === 1 || rd32(scratch) === 0);
  gl.GetShaderiv(prog.f.s, 0x8B4F, scratch); eq('SHADER_TYPE', rd32(scratch), 0x8B30);
  gl.GetShaderSource(prog.v.s, 4096, scratch, scratch + 16); eq('GetShaderSource round trip', rdStr(scratch + 16), VS);
  gl.GetShaderiv(prog.v.s, 0x8B88, scratch); eq('SHADER_SOURCE_LENGTH', rd32(scratch), VS.length + 1);
  gl.GetProgramInfoLog(prog.p, 256, scratch, scratch + 16); t('GetProgramInfoLog (empty on success)', rd32(scratch) >= 0);

  // failing shader: info log
  const bad = gl.CreateShader(0x8B30);
  gl.ShaderSource(bad, 1, putI([str('precision mediump float; void main(){ gl_FragColor = vec4(undefined_thing); }')]), 0); gl.CompileShader(bad);
  gl.GetShaderiv(bad, 0x8B81, scratch); eq('bad shader COMPILE_STATUS = 0', rd32(scratch), 0);
  gl.GetShaderiv(bad, 0x8B84, scratch); const logLen = rd32(scratch); t('INFO_LOG_LENGTH > 0', logLen > 1, logLen);
  gl.GetShaderInfoLog(bad, 512, scratch, scratch + 16); t('GetShaderInfoLog mentions the identifier', /undefined_thing/.test(rdStr(scratch + 16)), rdStr(scratch + 16));
  eq('GetShaderInfoLog length matches', rd32(scratch), rdStr(scratch + 16).length);
  gl.GetShaderInfoLog(bad, 8, scratch, scratch + 16); eq('GetShaderInfoLog truncates to bufsize-1', rd32(scratch), 7);
  // ShaderSource with explicit lengths and multiple strings
  const ms = gl.CreateShader(0x8B30), s1 = 'precision mediump float;\n', s2 = 'void main(){ gl_FragColor = vec4(1.0); }GARBAGE';
  gl.ShaderSource(ms, 2, putI([str(s1), str(s2)]), putI([s1.length, s2.length - 7])); gl.CompileShader(ms);
  gl.GetShaderiv(ms, 0x8B81, scratch); eq('ShaderSource with count=2 and lengths', rd32(scratch), 1);
  // delete semantics: shader attached to a program stays alive until detached
  const tmpP = gl.CreateProgram(); gl.AttachShader(tmpP, ms); gl.DeleteShader(ms);
  eq('IsShader after delete while attached', gl.IsShader(ms), 1);
  gl.GetShaderiv(ms, 0x8B80, scratch); eq('DELETE_STATUS', rd32(scratch), 1);
  gl.DetachShader(tmpP, ms); eq('IsShader after detach', gl.IsShader(ms), 0);
  gl.DeleteProgram(tmpP); eq('IsProgram after delete', gl.IsProgram(tmpP), 0);
  gl.DeleteShader(bad); drainErr();

  // ---- the default texture (name 0): GLES2 allows parameter calls on it, WebGL would raise "no texture bound" ---------------
  gl.ActiveTexture(0x84C0); gl.BindTexture(0xDE1, 0);
  gl.TexParameteri(0xDE1, 0x2801, 0x2600); gl.TexParameterf(0xDE1, 0x2800, 0x2600); gl.TexParameteriv(0xDE1, 0x2802, putI([0x812F]));
  noErr('TexParameter on the default texture');

  // ---- a textured quad, DrawElements with an index buffer -----------------------------------------------
  const tex = genOne((n, p) => gl.GenTextures(n, p));
  gl.ActiveTexture(0x84C0); gl.BindTexture(0xDE1, tex); eq('IsTexture after bind', gl.IsTexture(tex), 1);
  eq('TEXTURE_BINDING_2D', geti(0x8069), tex);
  gl.PixelStorei(0xCF5, 1);
  gl.TexImage2D(0xDE1, 0, RGBA, 2, 2, 0, RGBA, UBYTE, put(new Uint8Array([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255])));
  gl.PixelStorei(0xCF5, 4);
  for (const [pn, v] of [[0x2801, 0x2600], [0x2800, 0x2600], [0x2802, 0x812F], [0x2803, 0x812F]]) gl.TexParameteri(0xDE1, pn, v);
  gl.GetTexParameteriv(0xDE1, 0x2801, scratch); eq('GetTexParameteriv MIN_FILTER', rd32(scratch), 0x2600);
  gl.TexParameterf(0xDE1, 0x2801, 0x2600); gl.TexParameterfv(0xDE1, 0x2800, putF([0x2600])); gl.TexParameteriv(0xDE1, 0x2803, putI([0x812F]));
  gl.GetTexParameterfv(0xDE1, 0x2800, scratch); eq('GetTexParameterfv MAG_FILTER', rdf(scratch), 0x2600);
  const qv = new Float32Array([-1, -1, 0, 0, 1, 1, 1, 1, 1, -1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, 1, 0, 1, 1, 1, 1, 1]);
  gl.BufferData(0x8892, qv.byteLength, putF(qv), 0x88E4);
  for (const [i, n, off] of [[0, 2, 0], [1, 2, 8], [2, 4, 16]]) gl.VertexAttribPointer(i, n, FLOAT, 0, 32, off);
  const ibo = genOne((n, p) => gl.GenBuffers(n, p));
  gl.BindBuffer(0x8893, ibo); gl.BufferData(0x8893, 12, putU16([0, 1, 2, 0, 2, 3]), 0x88E4);
  gl.Uniform1f(locMix, 1);
  gl.ClearColor(0, 0, 0, 1); gl.Clear(0x4000);
  gl.DrawElements(TRI, 6, 0x1403, 0);
  noErr('DrawElements');
  eq('quad (16,16) red', px(16, 16), [255, 0, 0, 255]); eq('quad (48,16) green', px(48, 16), [0, 255, 0, 255]);
  eq('quad (16,48) blue', px(16, 48), [0, 0, 255, 255]); eq('quad (48,48) white', px(48, 48), [255, 255, 255, 255]);
  gl.BufferSubData(0x8893, 0, 12, putU16([0, 1, 2, 0, 2, 3]));
  noErr('BufferSubData');
  gl.TexSubImage2D(0xDE1, 0, 1, 1, 1, 1, RGBA, UBYTE, put(new Uint8Array([10, 20, 30, 255])));
  gl.DrawElements(TRI, 6, 0x1403, 0); eq('TexSubImage2D updated texel', px(48, 48), [10, 20, 30, 255]);

  // ---- BGRA / float / half float / depth textures ----------------------------------------------------------
  gl.TexImage2D(0xDE1, 0, 0x80E1, 2, 2, 0, 0x80E1, UBYTE, put(new Uint8Array([0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 0, 0, 0, 255])));   // BGRA: red, green / blue, black
  noErr('BGRA TexImage2D');
  gl.DrawElements(TRI, 6, 0x1403, 0);
  eq('BGRA texel -> red', px(16, 16), [255, 0, 0, 255]); eq('BGRA texel -> green', px(48, 16), [0, 255, 0, 255]); eq('BGRA texel -> blue', px(16, 48), [0, 0, 255, 255]);
  gl.TexSubImage2D(0xDE1, 0, 0, 0, 1, 1, 0x80E1, UBYTE, put(new Uint8Array([30, 20, 10, 255])));
  gl.DrawElements(TRI, 6, 0x1403, 0); eq('BGRA TexSubImage2D', px(16, 16), [10, 20, 30, 255]);
  gl.TexImage2D(0xDE1, 0, RGBA, 2, 2, 0, RGBA, FLOAT, putF([0.5, 0, 0, 1, 0, 0.5, 0, 1, 0, 0, 0.5, 1, 1, 1, 1, 1]));
  noErr('RGBA/FLOAT TexImage2D (OES_texture_float)');
  gl.DrawElements(TRI, 6, 0x1403, 0); t('float texel 0.5 -> ~128', near(px(16, 16), [128, 0, 0, 255], 2), px(16, 16));
  gl.TexImage2D(0xDE1, 0, RGBA, 2, 2, 0, RGBA, 0x8D61, putU16([0x3800, 0, 0, 0x3C00, 0, 0x3800, 0, 0x3C00, 0, 0, 0x3800, 0x3C00, 0x3C00, 0x3C00, 0x3C00, 0x3C00]));
  noErr('RGBA/HALF_FLOAT_OES TexImage2D'); gl.DrawElements(TRI, 6, 0x1403, 0); t('half float texel 0.5 -> ~128', near(px(16, 16), [128, 0, 0, 255], 2), px(16, 16));
  // float / half-float ALPHA, LUMINANCE, LUMINANCE_ALPHA: WebGL2 has no texture swizzle, so they are expanded to RGBA on upload
  const quadPx = () => { gl.DrawElements(TRI, 6, 0x1403, 0); return [px(16, 16), px(48, 16), px(16, 48), px(48, 48)]; };
  const nearAll = (got, want) => got.length === want.length && got.every((g, i) => near(g, want[i], 2));
  gl.TexImage2D(0xDE1, 0, 0x1906, 2, 2, 0, 0x1906, FLOAT, putF([0.25, 0.5, 0.75, 1]));
  noErr('ALPHA/FLOAT'); t('ALPHA/FLOAT samples as (0,0,0,a)', nearAll(quadPx(), [[0, 0, 0, 64], [0, 0, 0, 128], [0, 0, 0, 191], [0, 0, 0, 255]]), JSON.stringify(quadPx()));
  gl.TexImage2D(0xDE1, 0, 0x1909, 2, 2, 0, 0x1909, FLOAT, putF([0.25, 0.5, 0.75, 1]));
  t('LUMINANCE/FLOAT samples as (l,l,l,1)', nearAll(quadPx(), [[64, 64, 64, 255], [128, 128, 128, 255], [191, 191, 191, 255], [255, 255, 255, 255]]), JSON.stringify(quadPx()));
  gl.TexSubImage2D(0xDE1, 0, 1, 1, 1, 1, 0x1909, FLOAT, putF([0]));
  t('TexSubImage2D into a float LUMINANCE texture', nearAll(quadPx(), [[64, 64, 64, 255], [128, 128, 128, 255], [191, 191, 191, 255], [0, 0, 0, 255]]), JSON.stringify(quadPx()));
  gl.TexImage2D(0xDE1, 0, 0x190A, 2, 2, 0, 0x190A, FLOAT, putF([1, 0.25, 0.5, 0.5, 0.25, 1, 0, 0.75]));
  t('LUMINANCE_ALPHA/FLOAT samples as (l,l,l,a)', nearAll(quadPx(), [[255, 255, 255, 64], [128, 128, 128, 128], [64, 64, 64, 255], [0, 0, 0, 191]]), JSON.stringify(quadPx()));
  gl.TexImage2D(0xDE1, 0, 0x1906, 2, 2, 0, 0x1906, 0x8D61, putU16([0x3400, 0x3800, 0x3A00, 0x3C00]));
  t('ALPHA/HALF_FLOAT samples as (0,0,0,a)', nearAll(quadPx(), [[0, 0, 0, 64], [0, 0, 0, 128], [0, 0, 0, 191], [0, 0, 0, 255]]), JSON.stringify(quadPx()));
  gl.TexImage2D(0xDE1, 0, 0x1909, 2, 2, 0, 0x1909, 0x8D61, putU16([0x3400, 0x3800, 0x3A00, 0x3C00]));
  t('LUMINANCE/HALF_FLOAT samples as (l,l,l,1)', nearAll(quadPx(), [[64, 64, 64, 255], [128, 128, 128, 255], [191, 191, 191, 255], [255, 255, 255, 255]]), JSON.stringify(quadPx()));
  gl.TexImage2D(0xDE1, 0, 0x190A, 2, 2, 0, 0x190A, 0x8D61, putU16([0x3C00, 0x3400, 0x3800, 0x3800, 0x3400, 0x3C00, 0, 0x3A00]));
  t('LUMINANCE_ALPHA/HALF_FLOAT samples as (l,l,l,a)', nearAll(quadPx(), [[255, 255, 255, 64], [128, 128, 128, 128], [64, 64, 64, 255], [0, 0, 0, 191]]), JSON.stringify(quadPx()));
  gl.PixelStorei(0xCF5, 1); gl.TexImage2D(0xDE1, 0, 0x1906, 3, 2, 0, 0x1906, 0x8D61, putU16([0x3C00, 0x3C00, 0x3C00, 0x3C00, 0x3C00, 0x3C00])); gl.PixelStorei(0xCF5, 4);
  noErr('ALPHA/HALF_FLOAT 3x2 with UNPACK_ALIGNMENT 1 (unpadded rows)');
  gl.TexImage2D(0xDE1, 0, RGBA, 2, 2, 0, RGBA, 0x8033, putU16([0xF00F, 0x0F0F, 0x00FF, 0xFFFF]));
  noErr('RGBA/UNSIGNED_SHORT_4_4_4_4 TexImage2D');
  gl.TexImage2D(0xDE1, 0, 0x1907, 2, 2, 0, 0x1907, UBYTE, put(new Uint8Array(24))); noErr('RGB/UNSIGNED_BYTE (row alignment 4)');
  gl.TexImage2D(0xDE1, 0, 0x1909, 2, 2, 0, 0x1909, UBYTE, put(new Uint8Array(8))); noErr('LUMINANCE');
  gl.TexImage2D(0xDE1, 0, 0x190A, 2, 2, 0, 0x190A, UBYTE, put(new Uint8Array(16))); noErr('LUMINANCE_ALPHA');
  gl.TexImage2D(0xDE1, 0, 0x1906, 2, 2, 0, 0x1906, UBYTE, put(new Uint8Array(8))); noErr('ALPHA');
  gl.TexImage2D(0xDE1, 0, 0x1908, 2, 2, 0, 0x1908, 0x8034, putU16([0xFFFF, 0, 0xFFFF, 1])); noErr('RGBA/UNSIGNED_SHORT_5_5_5_1');
  gl.TexImage2D(0xDE1, 0, 0x1907, 2, 2, 0, 0x1907, 0x8363, putU16([0xF800, 0x07E0, 0x001F, 0xFFFF])); noErr('RGB/UNSIGNED_SHORT_5_6_5');
  const dtex = genOne((n, p) => gl.GenTextures(n, p));
  gl.BindTexture(0xDE1, dtex);
  gl.TexImage2D(0xDE1, 0, 0x1902, 32, 32, 0, 0x1902, 0x1403, 0); noErr('DEPTH_COMPONENT/UNSIGNED_SHORT texture (OES_depth_texture)');
  gl.TexImage2D(0xDE1, 0, 0x1902, 32, 32, 0, 0x1902, 0x1405, 0); noErr('DEPTH_COMPONENT/UNSIGNED_INT texture');
  gl.TexImage2D(0xDE1, 0, 0x84F9, 32, 32, 0, 0x84F9, 0x84FA, 0); noErr('DEPTH_STENCIL/UNSIGNED_INT_24_8 texture (OES_packed_depth_stencil)');
  gl.TexImage2D(0xDE1, 0, 0x1902, 32, 32, 0, 0x1902, 0x1403, 0);
  gl.TexParameteri(0xDE1, 0x2801, 0x2600); gl.TexParameteri(0xDE1, 0x2800, 0x2600);

  // ---- framebuffer object + renderbuffer + ReadPixels ---------------------------------------------------
  const ctex = genOne((n, p) => gl.GenTextures(n, p)); gl.BindTexture(0xDE1, ctex);
  gl.TexImage2D(0xDE1, 0, RGBA, 16, 16, 0, RGBA, UBYTE, 0);
  gl.TexParameteri(0xDE1, 0x2801, 0x2600); gl.TexParameteri(0xDE1, 0x2800, 0x2600);
  const fbo = genOne((n, p) => gl.GenFramebuffers(n, p)), rb = genOne((n, p) => gl.GenRenderbuffers(n, p));
  gl.BindFramebuffer(0x8D40, fbo); eq('IsFramebuffer', gl.IsFramebuffer(fbo), 1); eq('FRAMEBUFFER_BINDING', geti(0x8CA6), fbo);
  gl.BindRenderbuffer(0x8D41, rb); eq('IsRenderbuffer', gl.IsRenderbuffer(rb), 1);
  gl.RenderbufferStorage(0x8D41, 0x88F0, 16, 16);
  gl.GetRenderbufferParameteriv(0x8D41, 0x8D42, scratch); eq('RENDERBUFFER_WIDTH', rd32(scratch), 16);
  gl.GetRenderbufferParameteriv(0x8D41, 0x8D44, scratch); eq('RENDERBUFFER_INTERNAL_FORMAT is the requested DEPTH24_STENCIL8_OES', rd32(scratch), 0x88F0);
  gl.FramebufferTexture2D(0x8D40, 0x8CE0, 0xDE1, ctex, 0);
  gl.FramebufferRenderbuffer(0x8D40, 0x8D00, 0x8D41, rb); gl.FramebufferRenderbuffer(0x8D40, 0x8D20, 0x8D41, rb);
  eq('CheckFramebufferStatus = COMPLETE', gl.CheckFramebufferStatus(0x8D40), 0x8CD5);
  gl.GetFramebufferAttachmentParameteriv(0x8D40, 0x8CE0, 0x8CD0, scratch); eq('ATTACHMENT_OBJECT_TYPE', rd32(scratch), 0x1702);
  gl.GetFramebufferAttachmentParameteriv(0x8D40, 0x8CE0, 0x8CD1, scratch); eq('ATTACHMENT_OBJECT_NAME is a GLES name', rd32(scratch), ctex);
  gl.Viewport(0, 0, 16, 16); gl.ClearColor(1, 0.5, 0, 1); gl.Clear(0x4000 | 0x100 | 0x400);
  t('FBO clear readback', near(px(8, 8), [255, 128, 0, 255], 2), px(8, 8));
  const rp = host.alloc(16 * 16 * 4); gl.ReadPixels(0, 0, 4, 4, RGBA, UBYTE, rp + 1);   // misaligned destination
  t('ReadPixels to an unaligned destination', near(rdBytes(rp + 1, 4), [255, 128, 0, 255], 2));
  gl.ReadPixels(0, 0, 2, 2, 0x80E1, UBYTE, rp); t('ReadPixels BGRA swizzles', near(rdBytes(rp, 4), [0, 128, 255, 255], 2), rdBytes(rp, 4));
  gl.PixelStorei(0xD05, 1); gl.ReadPixels(0, 0, 3, 2, RGBA, UBYTE, rp); gl.PixelStorei(0xD05, 4); noErr('ReadPixels with PACK_ALIGNMENT 1');
  const cp = genOne((n, p) => gl.GenTextures(n, p)); gl.BindTexture(0xDE1, cp);
  gl.CopyTexImage2D(0xDE1, 0, RGBA, 0, 0, 16, 16, 0); noErr('CopyTexImage2D');
  gl.CopyTexSubImage2D(0xDE1, 0, 0, 0, 0, 0, 4, 4); noErr('CopyTexSubImage2D');
  gl.GenerateMipmap(0xDE1); noErr('GenerateMipmap');
  gl.BindTexture(0xDE1, ctex);
  // depth texture attachment
  const fbo2 = genOne((n, p) => gl.GenFramebuffers(n, p)); gl.BindFramebuffer(0x8D40, fbo2);
  gl.BindTexture(0xDE1, dtex); gl.TexImage2D(0xDE1, 0, 0x1902, 32, 32, 0, 0x1902, 0x1403, 0);
  gl.FramebufferTexture2D(0x8D40, 0x8D00, 0xDE1, dtex, 0);
  gl.BindTexture(0xDE1, ctex); gl.TexImage2D(0xDE1, 0, RGBA, 32, 32, 0, RGBA, UBYTE, 0); gl.FramebufferTexture2D(0x8D40, 0x8CE0, 0xDE1, ctex, 0);
  eq('FBO with depth texture is complete', gl.CheckFramebufferStatus(0x8D40), 0x8CD5);
  gl.FramebufferTexture2D(0x8D40, 0x8D00, 0xDE1, 0, 0); noErr('detach attachment');
  gl.BindFramebuffer(0x8D40, 0); eq('FRAMEBUFFER_BINDING back to 0', geti(0x8CA6), 0);
  gl.DeleteFramebuffers(1, putI([fbo2])); eq('IsFramebuffer after delete', gl.IsFramebuffer(fbo2), 0);
  gl.DeleteRenderbuffers(1, putI([rb])); gl.DeleteTextures(1, putI([dtex])); eq('IsTexture after delete', gl.IsTexture(dtex), 0);

  // ---- feature / extension interfaces ----------------------------------------------------------------------
  // derivatives + frag depth + texture lod: compiled through the ESSL 3.00 fallback
  const dprog = makeProgram('attribute vec2 a_pos; attribute vec2 a_uv; attribute vec4 a_col; varying vec2 v_uv; uniform vec2 u_off; void main(){ v_uv=a_uv; gl_Position=vec4(a_pos+u_off,0.0,1.0); }',
    '#extension GL_OES_standard_derivatives : enable\n#extension GL_EXT_shader_texture_lod : enable\nprecision mediump float; varying vec2 v_uv; uniform sampler2D u_tex;\nvoid main(){ float w = fwidth(v_uv.x) * 0.0; vec4 c = texture2DLodEXT(u_tex, v_uv, 0.0) * 0.0; gl_FragColor = vec4(0.25 + w + c.r, 0.5, 0.75, 1.0); }');
  t('derivatives / texture-lod fragment shader compiles (ESSL3 fallback)', dprog.fsOk && dprog.linked, [dprog.vsOk, dprog.fsOk, dprog.linked].join(','));
  if (dprog.linked) { gl.UseProgram(dprog.p); gl.Viewport(0, 0, 64, 64); gl.ClearColor(0, 0, 0, 1); gl.Clear(0x4000); gl.DrawElements(TRI, 6, 0x1403, 0); t('ESSL3-fallback shader output', near(px(32, 32), [64, 128, 191, 255], 2), px(32, 32)); gl.UseProgram(prog.p); }
  // instanced arrays: two instances offset through a per-instance attribute
  const ivs = 'attribute vec2 a_pos; attribute vec2 a_uv; attribute vec4 a_col; varying vec4 v_col; void main(){ v_col=a_col; gl_Position=vec4(a_pos*0.25+a_uv,0.0,1.0); }';
  const ifs = 'precision mediump float; varying vec4 v_col; void main(){ gl_FragColor = v_col; }';
  const iprog = makeProgram(ivs, ifs); t('instancing program', iprog.linked);
  gl.UseProgram(iprog.p);
  const ib = genOne((n, p) => gl.GenBuffers(n, p)), ob = genOne((n, p) => gl.GenBuffers(n, p));
  gl.BindBuffer(0x8892, ib); gl.BufferData(0x8892, 24, putF([-1, -1, 1, -1, 0, 1]), 0x88E4);
  gl.BindBuffer(0x8892, ob); gl.BufferData(0x8892, 16, putF([-0.5, -0.5, 0.5, 0.5]), 0x88E4);
  gl.BindBuffer(0x8892, ib); gl.EnableVertexAttribArray(0); gl.VertexAttribPointer(0, 2, FLOAT, 0, 0, 0);
  gl.BindBuffer(0x8892, ob); gl.EnableVertexAttribArray(1); gl.VertexAttribPointer(1, 2, FLOAT, 0, 0, 0); inst.VertexAttribDivisorANGLE(1, 1);
  gl.DisableVertexAttribArray(2); gl.VertexAttrib4f(2, 0, 1, 0, 1);
  gl.ClearColor(0, 0, 0, 1); gl.Clear(0x4000);
  inst.DrawArraysInstancedANGLE(TRI, 0, 3, 2);
  noErr('DrawArraysInstancedANGLE');
  t('instance 0 drawn', near(px(16, 16), [0, 255, 0, 255]) , px(16, 16)); t('instance 1 drawn', near(px(48, 48), [0, 255, 0, 255]), px(48, 48));
  gl.BindBuffer(0x8893, ibo);
  inst.DrawElementsInstancedANGLE(TRI, 3, 0x1403, 0, 2); noErr('DrawElementsInstancedANGLE');
  inst.VertexAttribDivisorANGLE(1, 0); gl.DisableVertexAttribArray(1);
  // VAO
  const v1 = genOne((n, p) => vao.GenVertexArraysOES(n, p));
  vao.BindVertexArrayOES(v1); eq('IsVertexArrayOES', vao.IsVertexArrayOES(v1), 1); eq('VERTEX_ARRAY_BINDING', geti(0x85B5), v1);
  gl.BindBuffer(0x8892, ib); gl.EnableVertexAttribArray(0); gl.VertexAttribPointer(0, 2, FLOAT, 0, 0, 0);
  gl.Clear(0x4000); gl.DrawArrays(TRI, 0, 3); t('draw through a VAO', near(px(32, 28), [0, 255, 0, 255]), px(32, 28));
  vao.BindVertexArrayOES(0); vao.DeleteVertexArraysOES(1, putI([v1])); eq('IsVertexArrayOES after delete', vao.IsVertexArrayOES(v1), 0);
  noErr('VAO');
  // occlusion queries
  const qid = genOne((n, p) => qry.GenQueriesEXT(n, p));
  qry.BeginQueryEXT(0x8C2F, qid); gl.DrawArrays(TRI, 0, 3); qry.EndQueryEXT(0x8C2F);
  eq('IsQueryEXT', qry.IsQueryEXT(qid), 1);
  let avail = 0; for (let i = 0; i < 200 && !avail; i++) { qry.GetQueryObjectuivEXT(qid, 0x8867, scratch); avail = rdu32(scratch); if (!avail) await new Promise((r) => setTimeout(r, 10)); }
  t('query result becomes available', avail === 1);
  qry.GetQueryObjectuivEXT(qid, 0x8866, scratch); eq('ANY_SAMPLES_PASSED result', rdu32(scratch), 1);
  qry.GetQueryivEXT(0x8C2F, 0x8865, scratch); eq('CURRENT_QUERY_EXT (none active)', rd32(scratch), 0);
  qry.DeleteQueriesEXT(1, putI([qid])); eq('IsQueryEXT after delete', qry.IsQueryEXT(qid), 0);
  // draw buffers
  dbuf.DrawBuffersEXT(1, putI([0x0405])); noErr('DrawBuffersEXT(BACK)');
  // multisample renderbuffer + blit into a texture-backed FBO
  const mrb = genOne((n, p) => gl.GenRenderbuffers(n, p)), mfbo = genOne((n, p) => gl.GenFramebuffers(n, p));
  gl.BindRenderbuffer(0x8D41, mrb); msaa.RenderbufferStorageMultisampleEXT(0x8D41, 4, 0x8058, 16, 16);
  gl.BindFramebuffer(0x8D40, mfbo); gl.FramebufferRenderbuffer(0x8D40, 0x8CE0, 0x8D41, mrb);
  eq('multisample FBO complete', gl.CheckFramebufferStatus(0x8D40), 0x8CD5);
  gl.Viewport(0, 0, 16, 16); gl.ClearColor(0, 1, 1, 1); gl.Clear(0x4000);
  const rtex = genOne((n, p) => gl.GenTextures(n, p)), rfbo = genOne((n, p) => gl.GenFramebuffers(n, p));
  gl.BindTexture(0xDE1, rtex); gl.TexImage2D(0xDE1, 0, RGBA, 16, 16, 0, RGBA, UBYTE, 0); gl.TexParameteri(0xDE1, 0x2801, 0x2600);
  gl.BindFramebuffer(0x8CA9, rfbo); gl.FramebufferTexture2D(0x8CA9, 0x8CE0, 0xDE1, rtex, 0);
  gl.BindFramebuffer(0x8CA8, mfbo);
  eq('READ_FRAMEBUFFER_BINDING', geti(0x8CAA), mfbo); eq('DRAW_FRAMEBUFFER_BINDING', geti(0x8CA6), rfbo);
  blit.BlitFramebufferEXT(0, 0, 16, 16, 0, 0, 16, 16, 0x4000, 0x2600); noErr('BlitFramebufferEXT');
  gl.BindFramebuffer(0x8CA8, rfbo); t('blit result', near(px(8, 8), [0, 255, 255, 255]), px(8, 8));
  gl.BindFramebuffer(0x8D40, 0); gl.Viewport(0, 0, 64, 64);
  eq('EnableFeatureCHROMIUM(allow_buffers_on_multiple_targets)', feat.EnableFeatureCHROMIUM(str('pepper3d_allow_buffers_on_multiple_targets')), 1);
  eq('EnableFeatureCHROMIUM(unknown)', feat.EnableFeatureCHROMIUM(str('bogus')), 0);
  // MapBufferSubDataCHROMIUM: write vertex data through the mapping, then draw it
  gl.UseProgram(prog.p);
  const mb = genOne((n, p) => gl.GenBuffers(n, p)); gl.BindBuffer(0x8892, mb); gl.BufferData(0x8892, 96, 0, 0x88E8);
  const mp = msub.MapBufferSubDataCHROMIUM(0x8892, 0, 96, 0x88B9);
  t('MapBufferSubDataCHROMIUM returns guest memory', mp !== 0);
  u8.set(new Uint8Array(new Float32Array([-1, -1, 0, 0, 1, 0, 0, 1, 3, -1, 0, 0, 1, 0, 0, 1, -1, 3, 0, 0, 1, 0, 0, 1]).buffer), base + mp);
  msub.UnmapBufferSubDataCHROMIUM(mp);
  for (const [i, n, off] of [[0, 2, 0], [1, 2, 8], [2, 4, 16]]) { gl.EnableVertexAttribArray(i); gl.VertexAttribPointer(i, n, FLOAT, 0, 32, off); }
  gl.Uniform1f(locMix, 0); gl.ClearColor(0, 0, 0, 1); gl.Clear(0x4000); gl.DrawArrays(TRI, 0, 3);
  eq('draw from a buffer filled via map/unmap', px(32, 32), [255, 0, 0, 255]);
  // MapTexSubImage2DCHROMIUM
  const mt = genOne((n, p) => gl.GenTextures(n, p)); gl.BindTexture(0xDE1, mt);
  gl.TexImage2D(0xDE1, 0, RGBA, 2, 2, 0, RGBA, UBYTE, 0);
  gl.TexParameteri(0xDE1, 0x2801, 0x2600); gl.TexParameteri(0xDE1, 0x2800, 0x2600); gl.TexParameteri(0xDE1, 0x2802, 0x812F); gl.TexParameteri(0xDE1, 0x2803, 0x812F);
  const tp = tmap.MapTexSubImage2DCHROMIUM(0xDE1, 0, 0, 0, 2, 2, RGBA, UBYTE, 0x88B9);
  t('MapTexSubImage2DCHROMIUM returns guest memory', tp !== 0);
  u8.set([255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255], base + tp);
  tmap.UnmapTexSubImage2DCHROMIUM(tp);
  gl.BufferData(0x8892, qv.byteLength, 0, 0x88E4); gl.BindBuffer(0x8892, vbo);
  for (const [i, n, off] of [[0, 2, 0], [1, 2, 8], [2, 4, 16]]) gl.VertexAttribPointer(i, n, FLOAT, 0, 32, off);
  gl.BindBuffer(0x8892, mb); gl.BufferData(0x8892, qv.byteLength, putF(qv), 0x88E4);
  for (const [i, n, off] of [[0, 2, 0], [1, 2, 8], [2, 4, 16]]) gl.VertexAttribPointer(i, n, FLOAT, 0, 32, off);
  gl.Uniform1f(locMix, 1); gl.Clear(0x4000); gl.DrawElements(TRI, 6, 0x1403, 0);
  eq('draw a texture filled via MapTexSubImage2D', px(32, 32), [255, 0, 255, 255]);
  noErr('map/unmap');
  // regression: Unmap uploads into the object that was MAPPED, not whatever is bound when Unmap is called
  const bA = genOne((n, p) => gl.GenBuffers(n, p)), bB = genOne((n, p) => gl.GenBuffers(n, p)), zeros16 = new Array(16).fill(0);
  gl.BindBuffer(0x8892, bA); gl.BufferData(0x8892, 16, put(new Uint8Array(16)), 0x88E8);
  gl.BindBuffer(0x8892, bB); gl.BufferData(0x8892, 16, put(new Uint8Array(16)), 0x88E8);
  gl.BindBuffer(0x8892, bA);
  const mpa = msub.MapBufferSubDataCHROMIUM(0x8892, 0, 16, 0x88B9);
  const seq16 = Array.from({ length: 16 }, (_, i) => i + 1);
  u8.set(seq16, base + mpa);
  gl.BindBuffer(0x8892, bB);                                  // another buffer is bound when A is unmapped
  msub.UnmapBufferSubDataCHROMIUM(mpa);
  eq('Unmap leaves the current ARRAY_BUFFER binding alone', geti(0x8894), bB);
  const readBuf = (id) => { gl.BindBuffer(0x8892, id); const p = msub.MapBufferSubDataCHROMIUM(0x8892, 0, 16, 0x88B8); const r = rdBytes(p, 16); msub.UnmapBufferSubDataCHROMIUM(p); return r; };
  eq('the mapped buffer (A) received the data', readBuf(bA), seq16);
  eq('the buffer bound at Unmap time (B) was not touched', readBuf(bB), zeros16);
  gl.BindBuffer(0x8892, bA); const mpd = msub.MapBufferSubDataCHROMIUM(0x8892, 0, 16, 0x88B9); gl.BindBuffer(0x8892, 0); gl.DeleteBuffers(1, putI([bA]));
  msub.UnmapBufferSubDataCHROMIUM(mpd); noErr('Unmap after the mapped buffer was deleted');
  eq('MapBufferSubData with nothing bound -> 0 + INVALID_OPERATION', [msub.MapBufferSubDataCHROMIUM(0x8892, 0, 16, 0x88B9), gl.GetError()], [0, 0x502]);
  const tA = genOne((n, p) => gl.GenTextures(n, p)), tB = genOne((n, p) => gl.GenTextures(n, p));
  for (const id of [tA, tB]) { gl.BindTexture(0xDE1, id); gl.TexImage2D(0xDE1, 0, RGBA, 2, 2, 0, RGBA, UBYTE, put(new Uint8Array(16))); }
  gl.BindTexture(0xDE1, tA);
  const pta = tmap.MapTexSubImage2DCHROMIUM(0xDE1, 0, 0, 0, 2, 2, RGBA, UBYTE, 0x88B9);
  const texPat = [10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
  u8.set(texPat, base + pta);
  gl.BindTexture(0xDE1, tB);                                  // another texture is bound when A is unmapped
  tmap.UnmapTexSubImage2DCHROMIUM(pta);
  eq('Unmap leaves the current texture binding alone', geti(0x8069), tB);
  const readTex = (id) => { gl.BindTexture(0xDE1, id); const p = tmap.MapTexSubImage2DCHROMIUM(0xDE1, 0, 0, 0, 2, 2, RGBA, UBYTE, 0x88B8); const r = rdBytes(p, 16); tmap.UnmapTexSubImage2DCHROMIUM(p); return r; };
  eq('the mapped texture (A) received the pixels', readTex(tA), texPat);
  eq('the texture bound at Unmap time (B) was not touched', readTex(tB), zeros16);
  noErr('map/unmap object identity');

  // ---- sharing: a second context on the same WebGL2 context ---------------------------------------------------
  const ctxA = ctxId;
  const ctxB = g3d.Create(1, ctxA, putI([0x3057, 32, 0x3056, 32, 0x3021, 8, 0x3025, 24, 0x3038]));
  t('shared context created', ctxB > 0 && ctxB !== ctxA);
  gl.Viewport(3, 4, 20, 21);                         // state in A
  ctxId = ctxB;
  gl.GetIntegerv(0xBA2, scratch); eq('B has its own viewport state (default)', [rd32(scratch + 8), rd32(scratch + 12)], [32, 32]);
  eq('buffer shared: IsBuffer(vbo) usable from B', (gl.BindBuffer(0x8892, vbo), gl.IsBuffer(vbo)), 1);
  gl.UseProgram(prog.p);                             // programs are shared
  gl.Viewport(0, 0, 32, 32); gl.ClearColor(0.2, 0.4, 0.6, 1); gl.Clear(0x4000 | 0x100);
  t('shared ctx renders into its own surface', near(px(5, 5), [51, 102, 153, 255]), px(5, 5));
  ctxId = ctxA;
  gl.GetIntegerv(0xBA2, scratch); eq('A viewport restored after switching back', [rd32(scratch), rd32(scratch + 4), rd32(scratch + 8), rd32(scratch + 12)], [3, 4, 20, 21]);
  gl.Viewport(0, 0, 64, 64);
  eq('A default framebuffer untouched by B', px(5, 5), [255, 0, 255, 255]);
  ctxId = ctxB;
  g3d.ResizeBuffers(ctxB, 48, 40); gl.Viewport(0, 0, 48, 40); gl.ClearColor(0, 1, 0, 1); gl.Clear(0x4000); eq('ResizeBuffers on the shared surface', px(40, 30), [0, 255, 0, 255]);
  host.instances.get(1).g2d = ctxB;
  const prevSwaps = swaps.length;
  let done = null; const cb = { func: 1, user: 77, flags: 0 };
  eq('SwapBuffers on bound shared context returns PENDING', g3d.SwapBuffers(ctxB, cb), -1);
  eq('second SwapBuffers while pending -> INPROGRESS', g3d.SwapBuffers(ctxB, cb), -11);
  t('onSwap received the frame canvas', swaps.length === prevSwaps + 1 && swaps[swaps.length - 1].w === 48 && swaps[swaps.length - 1].h === 40, JSON.stringify(swaps.length));
  try {      // the FBO-backed surface was blitted into the group's canvas
    const c2 = new OffscreenCanvas(48, 40).getContext('2d', { willReadFrequently: true }); c2.drawImage(swaps[swaps.length - 1].canvas, 0, 0);
    t('FBO-backed surface presented through the canvas', near(Array.from(c2.getImageData(40, 5, 1, 1).data), [0, 255, 0, 255]), Array.from(c2.getImageData(40, 5, 1, 1).data));
  } catch (e) { t('FBO-backed surface presented through the canvas', false, e); }
  await new Promise((r) => setTimeout(r, 30));
  t('SwapBuffers completion callback delivered', pending.length >= 1 && pending[pending.length - 1].cc.user === 77);
  pending.splice(0);
  host.instances.get(1).g2d = 0;
  host.releaseRes(ctxB);
  ctxId = ctxA;
  eq('A still alive after releasing B', g3d.IsGraphics3D(ctxA), 1);
  gl.GetIntegerv(0xBA2, scratch); eq('A state intact', [rd32(scratch + 8), rd32(scratch + 12)], [64, 64]);

  // ---- regression: presenting a secondary context must not touch the primary's surface or its recorded size -------------
  {
    const A = host.getRes(ctxA);
    ctxId = ctxA; gl.Viewport(0, 0, 64, 64); gl.ClearColor(1, 0, 1, 1); gl.Clear(0x4000);                       // the primary shows magenta (64x64)
    const ctxC = g3d.Create(1, ctxA, putI([0x3057, 96, 0x3056, 80, 0x3021, 8, 0x3038]));                          // a LARGER secondary context
    ctxId = ctxC; gl.Viewport(0, 0, 96, 80); gl.ClearColor(0, 1, 1, 1); gl.Clear(0x4000);
    host.instances.get(1).g2d = ctxC;
    const n0 = swaps.length;
    g3d.SwapBuffers(ctxC, { func: 1, user: 1, flags: 0 }); await new Promise((r) => setTimeout(r, 30)); pending.splice(0);
    t('secondary frame is presented at its own size', swaps.length === n0 + 1 && swaps[n0].w === 96 && swaps[n0].h === 80 && swaps[n0].canvas.width === 96 && swaps[n0].canvas.height === 80);
    eq('secondary frame content', canvasPx(swaps[n0].canvas, 96, 80, 70, 40), [0, 255, 255, 255]);
    t('primary: recorded size unchanged by the secondary swap', A.w === 64 && A.h === 64 && A.surf.w === 64 && A.surf.h === 64, [A.w, A.h, A.surf.w, A.surf.h].join('x'));
    const wh = putI([0x3057, 0, 0x3056, 0, 0x3038]); g3d.GetAttribs(ctxA, wh); eq('primary: GetAttribs WIDTH/HEIGHT', [rd32(wh + 4), rd32(wh + 12)], [64, 64]);
    ctxId = ctxA; eq('primary: surface content untouched', px(5, 5), [255, 0, 255, 255]); gl.GetIntegerv(0xBA2, scratch); eq('primary: viewport untouched', [rd32(scratch + 8), rd32(scratch + 12)], [64, 64]);
    host.instances.get(1).g2d = ctxA;
    const n1 = swaps.length;
    g3d.SwapBuffers(ctxA, { func: 1, user: 2, flags: 0 }); await new Promise((r) => setTimeout(r, 30)); pending.splice(0);
    t('primary is presented afterwards at its own size', swaps.length === n1 + 1 && swaps[n1].w === 64 && swaps[n1].h === 64 && swaps[n1].canvas.width === 64 && swaps[n1].canvas.height === 64);
    eq('primary frame content after the secondary swap', canvasPx(swaps[n1].canvas, 64, 64, 5, 5), [255, 0, 255, 255]);
    host.instances.get(1).g2d = 0; host.releaseRes(ctxC); ctxId = ctxA;
    eq('primary still works after the secondary is released', [px(5, 5), g3d.IsGraphics3D(ctxA)], [[255, 0, 255, 255], 1]);
  }

  // ---- SwapBuffers from a non-main guest thread: callback on that thread's loop, blocking mode allowed -------------
  eq('blocking SwapBuffers on the main thread is rejected', tables.PPB_Graphics3D.SwapBuffers(0, ctxA, { func: 0, user: 0, flags: 0 }), -13);
  const blocking = tables.PPB_Graphics3D.SwapBuffers(5, ctxA, { func: 0, user: 0, flags: 0 });
  t('blocking SwapBuffers off the main thread parks the thread (returns a promise)', blocking && typeof blocking.then === 'function');
  eq('...and completes with PP_OK', await blocking, 0);
  host.threadLoop.set(5, 77);
  eq('async SwapBuffers off the main thread', tables.PPB_Graphics3D.SwapBuffers(5, ctxA, { func: 3, user: 4, flags: 0 }), -1);
  await new Promise((r) => setTimeout(r, 30)); pending.splice(0);

  // ---- SwapBuffers on the primary context, resize, loss of unbound frames ----------------------------------
  host.instances.get(1).g2d = ctxA;
  g3d.ResizeBuffers(ctxA, 80, 60);
  t('ResizeBuffers resizes the context surface', host.getRes(ctxA).surf.w === 80 && host.getRes(ctxA).surf.h === 60 && host.getRes(ctxA).w === 80);
  eq('ResizeBuffers(-1) is BADARGUMENT', g3d.ResizeBuffers(ctxA, -1, 5), -4);
  gl.Viewport(0, 0, 80, 60); gl.ClearColor(1, 1, 0, 1); gl.Clear(0x4000);
  const before = swaps.length;
  g3d.SwapBuffers(ctxA, { func: 1, user: 5, flags: 0 });
  t('SwapBuffers presents the primary canvas', swaps.length === before + 1 && swaps[before].canvas === host.getRes(ctxA).grp.canvas);
  await new Promise((r) => setTimeout(r, 30));
  pending.splice(0);
  // read the presented canvas content through a 2D canvas, the way Presenter does
  try {
    const c2 = new OffscreenCanvas(80, 60).getContext('2d', { willReadFrequently: true }); c2.drawImage(swaps[before].canvas, 0, 0);
    t('presented canvas content (drawImage)', near(Array.from(c2.getImageData(40, 30, 1, 1).data), [255, 255, 0, 255]), Array.from(c2.getImageData(40, 30, 1, 1).data));
  } catch (e) { t('presented canvas content (drawImage)', false, e); }

  // ---- error ordering: the FIRST recorded error wins, whether GLES-level or raised by WebGL --------------------
  drainErr();
  gl.UseProgram(0); gl.DrawArrays(TRI, 0, 3); gl.Enable(0x1234);                 // WebGL: INVALID_OPERATION, then ours: INVALID_ENUM
  eq('WebGL error first, GLES error second -> first reported', gl.GetError(), 0x502); eq('...and the later one is dropped', gl.GetError(), 0);
  gl.Enable(0x1234); gl.DrawArrays(TRI, 0, 3);
  eq('GLES error first, WebGL error second -> first reported', gl.GetError(), 0x500); eq('...and the later one is dropped (2)', gl.GetError(), 0);
  // names that were never generated can be bound; deleting a bound object resets the binding
  gl.BindBuffer(0x8892, 777); eq('BindBuffer on an ungenerated name', [gl.IsBuffer(777), geti(0x8894)], [1, 777]);
  gl.DeleteBuffers(1, putI([777])); eq('deleting the bound buffer unbinds it', [gl.IsBuffer(777), geti(0x8894)], [0, 0]);
  const dt = genOne((n, p) => gl.GenTextures(n, p)); gl.BindTexture(0xDE1, dt); gl.DeleteTextures(1, putI([dt]));
  eq('deleting the bound texture unbinds it', geti(0x8069), 0);
  // relink: locations are re-resolved; array element locations address a single element
  gl.UseProgram(prog.p);
  gl.LinkProgram(prog.p); gl.GetProgramiv(prog.p, 0x8B82, scratch); eq('relink', rd32(scratch), 1);
  const lA = gl.GetUniformLocation(prog.p, str('u_arr')) | 0, lA1 = gl.GetUniformLocation(prog.p, str('u_arr[1]')) | 0;
  gl.Uniform4fv(lA, 2, putF([0, 0, 0, 0, 0, 0, 0, 1]));
  gl.Uniform4fv(lA1, 1, putF([0.5, 0.25, 0.125, 1]));
  gl.GetUniformfv(prog.p, lA1, scratch); eq('Uniform4fv on an array element location', [rdf(scratch), rdf(scratch + 4), rdf(scratch + 8), rdf(scratch + 12)], [0.5, 0.25, 0.125, 1]);
  gl.GetUniformfv(prog.p, lA, scratch); eq('element 0 untouched', [rdf(scratch), rdf(scratch + 12)], [0, 0]);
  eq('GetUniformLocation(u_arr[2]) out of range = -1', gl.GetUniformLocation(prog.p, str('u_arr[2]')) | 0, -1);
  gl.GetActiveUniform(prog.p, 0, 64, scratch, scratch + 4, scratch + 8, scratch + 16);
  noErr('relink / array uniforms');
  // ESSL3 fallback for gl_FragDepthEXT and gl_FragData[n]
  const fdp = makeProgram('attribute vec2 a_pos; void main(){ gl_Position = vec4(a_pos, 0.0, 1.0); }',
    '#extension GL_EXT_frag_depth : enable\nprecision mediump float; void main(){ gl_FragColor = vec4(1.0); gl_FragDepthEXT = 0.5; }');
  t('gl_FragDepthEXT shader compiles + links', fdp.fsOk && fdp.linked, [fdp.fsOk, fdp.linked].join(','));
  const mrt = makeProgram('attribute vec2 a_pos; void main(){ gl_Position = vec4(a_pos, 0.0, 1.0); }',
    '#extension GL_EXT_draw_buffers : require\nprecision mediump float; void main(){ gl_FragData[0] = vec4(1.0); gl_FragData[1] = vec4(0.5); }');
  t('gl_FragData[1] (EXT_draw_buffers) shader compiles + links', mrt.fsOk && mrt.linked, [mrt.fsOk, mrt.linked].join(','));
  gl.UseProgram(prog.p);
  drainErr();
  // context attributes
  const pc = g3d.Create(1, 0, putI([0x3057, 16, 0x3056, 16, 0x3031, 4, 0x3032, 1, 0x3093, 0x3094, 0x11000, 0x11001, 0x3038]));
  const pcc = host.getRes(pc);
  t('SAMPLES -> multisampled surface, alpha 0 -> opaque, SWAP_BEHAVIOR=PRESERVED recorded', pcc.surf.samples > 0 && pcc.opaque === true && pcc.swapBehavior === 0x3094, [pcc.surf.samples, pcc.opaque, pcc.swapBehavior].join(','));
  const pq = putI([0x3031, 0, 0x3032, 0, 0x3093, 0, 0x3038]); g3d.GetAttribs(pc, pq);
  t('GetAttribs reports SAMPLES/SAMPLE_BUFFERS/SWAP_BEHAVIOR', rd32(pq + 20) === 0x3094 && rd32(pq + 4) > 0 && rd32(pq + 12) === 1, [rd32(pq + 4), rd32(pq + 12), rd32(pq + 20)].join(','));
  {
    ctxId = pc;
    gl.ClearColor(1, 0.5, 0, 0.3); gl.Clear(0x4000);
    t('ReadPixels from a multisampled default framebuffer (resolved); opaque context reads alpha 1', near(px(8, 8), [255, 128, 0, 255], 2), px(8, 8));
    dbuf.DrawBuffersEXT(1, putI([0x0405])); noErr('DrawBuffersEXT(GL_BACK) on the default framebuffer');
    const ct = genOne((n, p) => gl.GenTextures(n, p)); gl.BindTexture(0xDE1, ct);
    gl.CopyTexImage2D(0xDE1, 0, RGBA, 0, 0, 16, 16, 0); noErr('CopyTexImage2D from a multisampled default framebuffer');
    const bt = genOne((n, p) => gl.GenTextures(n, p)), bf = genOne((n, p) => gl.GenFramebuffers(n, p));
    gl.BindTexture(0xDE1, bt); gl.TexImage2D(0xDE1, 0, RGBA, 16, 16, 0, RGBA, UBYTE, 0);
    gl.BindFramebuffer(0x8CA9, bf); gl.FramebufferTexture2D(0x8CA9, 0x8CE0, 0xDE1, bt, 0);
    blit.BlitFramebufferEXT(0, 0, 16, 16, 0, 0, 16, 16, 0x4000, 0x2600); noErr('BlitFramebufferEXT from a multisampled default framebuffer');
    gl.BindFramebuffer(0x8D40, bf); t('blit from the multisampled default framebuffer copied the resolved colour', near(px(8, 8), [255, 128, 0, 255], 2), px(8, 8));
    gl.ClearColor(0, 0, 1, 0.3); gl.Clear(0x4000);
    t('alpha IS written when a user FBO is bound (opaque mask applies to the default framebuffer only)', near(px(4, 4), [0, 0, 255, 77], 2), px(4, 4));
    gl.BindFramebuffer(0x8D40, 0); gl.ClearColor(0, 1, 0, 0.3); gl.Clear(0x4000);
    t('alpha masked again after rebinding the default framebuffer', near(px(4, 4), [0, 255, 0, 255], 2), px(4, 4));
    ctxId = ctxA;
  }
  host.releaseRes(pc);
  // context loss
  const lc = g3d.Create(1, 0, putI([0x3057, 8, 0x3056, 8, 0x3038]));
  host.getRes(lc).grp.gl.getExtension('WEBGL_lose_context').loseContext();
  await new Promise((r) => setTimeout(r, 60));
  eq('Graphics3D::GetError after context loss', g3d.GetError(lc), -50);
  t('PPP_Graphics_3D::Graphics3DContextLost delivered to the plugin', hostCalls.some((c) => c.fn === 0x5555 && c.args[0] === 1), JSON.stringify(hostCalls.map((c) => c.fn.toString(16))));
  pending.splice(0); g3d.SwapBuffers(lc, { func: 1, user: 9, flags: 0 }); await new Promise((r) => setTimeout(r, 30));
  eq('SwapBuffers callback result after context loss', pending.length && pending[pending.length - 1].res, -50);
  pending.splice(0);
  ctxId = lc; gl.Clear(0x4000); gl.DrawArrays(TRI, 0, 3); gl.BufferData(0x8892, 4, 0, 0x88E4); gl.GetString(0x1F03);   // must not throw
  ctxId = ctxA;
  host.releaseRes(lc);
  // Create fails cleanly when WebGL2 cannot be created
  const savedFactory = host.opts.createGLCanvas; host.opts.createGLCanvas = () => ({ getContext: () => null });
  eq('Create returns 0 when WebGL2 is unavailable', g3d.Create(1, 0, 0), 0);
  host.opts.createGLCanvas = () => { throw new Error('no OffscreenCanvas'); };
  eq('Create returns 0 when the canvas cannot be created', g3d.Create(1, 0, 0), 0);
  host.opts.createGLCanvas = savedFactory;

  // ---- remaining entry points (smoke) -----------------------------------------------------------------------
  gl.BlendColor(0.1, 0.2, 0.3, 0.4); gl.BlendEquation(0x8006); gl.BlendEquationSeparate(0x8006, 0x8006); gl.BlendFunc(1, 0); gl.BlendFuncSeparate(1, 0, 1, 0);
  gl.ClearDepthf(1); gl.ClearStencil(0); gl.ColorMask(1, 1, 1, 1); gl.CullFace(0x405); gl.FrontFace(0x901); gl.DepthFunc(0x201); gl.DepthMask(1); gl.DepthRangef(0, 1);
  gl.Hint(0x8192, 0x1100); gl.LineWidth(1); gl.PolygonOffset(0, 0); gl.SampleCoverage(1, 0); gl.Scissor(0, 0, 80, 60);
  gl.StencilFunc(0x207, 0, 0xffffffff); gl.StencilFuncSeparate(0x404, 0x207, 0, 0xffffffff); gl.StencilMask(0xffffffff); gl.StencilMaskSeparate(0x405, 0xffffffff);
  gl.StencilOp(0x1E00, 0x1E00, 0x1E00); gl.StencilOpSeparate(0x404, 0x1E00, 0x1E00, 0x1E00); gl.Flush(); gl.Finish(); gl.ReleaseShaderCompiler();
  gl.Uniform2f(locOff, 0, 0); gl.Uniform3f(locOff, 0, 0, 0); gl.Uniform4f(locOff, 0, 0, 0, 0);   // wrong sizes: just errors, must not throw
  drainErr();
  gl.Uniform1i(locTex, 0); gl.Uniform2i(-1, 0, 0); gl.Uniform3i(-1, 0, 0, 0); gl.Uniform4i(-1, 0, 0, 0, 0);
  gl.Uniform1fv(-1, 1, putF([0])); gl.Uniform2fv(-1, 1, putF([0, 0])); gl.Uniform3fv(-1, 1, putF([0, 0, 0])); gl.Uniform4fv(-1, 1, putF([0, 0, 0, 0]));
  gl.Uniform1iv(locTex, 1, putI([0])); gl.Uniform2iv(-1, 1, putI([0, 0])); gl.Uniform3iv(-1, 1, putI([0, 0, 0])); gl.Uniform4iv(-1, 1, putI([0, 0, 0, 0]));
  gl.UniformMatrix2fv(-1, 1, 0, putF([1, 0, 0, 1])); gl.UniformMatrix3fv(-1, 1, 0, putF(new Array(9).fill(0))); gl.UniformMatrix4fv(-1, 1, 0, putF(new Array(16).fill(0)));
  noErr('uniform entry points with location -1');
  gl.UniformMatrix4fv(locOff, 1, 1, putF(new Array(16).fill(0)));
  eq('UniformMatrix4fv with transpose=GL_TRUE is INVALID_VALUE (GLES2)', gl.GetError(), 0x501);
  gl.GetUniformiv(prog.p, locTex, scratch); eq('GetUniformiv(sampler)', rd32(scratch), 0);
  gl.CompressedTexImage2D(0xDE1, 0, 0x83F0, 4, 4, 0, 8, put(new Uint8Array(8)));
  const ce = gl.GetError(); t('CompressedTexImage2D(DXT1) accepted or cleanly rejected', ce === 0 || ce === 0x500 || ce === 0x501 || ce === 0x502, '0x' + ce.toString(16));
  gl.CompressedTexSubImage2D(0xDE1, 0, 0, 0, 4, 4, 0x83F0, 8, put(new Uint8Array(8))); drainErr();
  gl.ShaderBinary(1, putI([1]), 0, 0, 0); eq('ShaderBinary -> INVALID_ENUM', gl.GetError(), 0x500);
  gl.DeleteBuffers(2, putI([vbo, ibo])); eq('IsBuffer after DeleteBuffers', [gl.IsBuffer(vbo), gl.IsBuffer(ibo)], [0, 0]);
  gl.DeleteProgram(prog.p); gl.DeleteShader(prog.v.s); gl.DeleteShader(prog.f.s);
  drainErr();

  } catch (e) { t('EXCEPTION', false, (e && e.stack) || e); }
  return finish();

  function finish() {
    const wanted = new Set();
    for (const n of Object.keys(PPB)) {
      if (!/Graphics3D|OpenGLES|GLES/.test(n)) continue;
      const base2 = n.split(';')[0];
      for (const id of PPB[n]) wanted.add((tables[n] ? n : base2) + '::' + METHODS[id].name);
    }
    const uncalled = [...wanted].filter((k) => !calls.has(k) && !calls.has(k.replace(/^PPB_OpenGLES2ChromiumMapSub::/, 'PPB_GLESChromiumTextureMapping(Dev)::')));
    const failed = results.filter((r) => !r.ok);
    return { base, total: results.length, failed, uncalled, logs, results, entryPoints: wanted.size, info };
  }
}
