// OpenGL ES 2.0 (+ the Chromium/Pepper extension set) on top of WebGL2.
//
// This file is the *state tracker + translator*. It knows GLES2 semantics (client-visible integer object
// names, integer uniform locations, a single sticky error flag, GLES texture/renderbuffer formats, the
// BGRA/half-float/float/depth-texture extensions...) and nothing about PPAPI, except that bulk data
// arguments arrive as guest addresses and are read through `GuestMem`. Every real GPU call goes through
// `this.gl`, which only needs the WebGL2 method names used below, so a WebGPU backend can be slotted in
// later by supplying another object with the same method surface (or by re-implementing the handful of
// "backend" spots: draw calls, texture/buffer uploads, FBO attachments, shader compile).
//
// Not supported (documented, see ppb-gles2.js header too):
//  * client-side vertex arrays / client-side index arrays (Pepper's GLES2 has none either): VertexAttribPointer
//    with a non-zero pointer and no ARRAY_BUFFER bound, and DrawElements without an ELEMENT_ARRAY_BUFFER, are
//    GL_INVALID_OPERATION.
//  * GL_FIXED vertex attributes, ShaderBinary (no binary formats), GL_CHROMIUM_* query/sync extras.

export const E = { NONE: 0, ENUM: 0x500, VALUE: 0x501, OP: 0x502, OOM: 0x505, FBOP: 0x506 };

const CAPS = [0xBE2, 0xB44, 0xB71, 0xBD0, 0x8037, 0x809E, 0x80A0, 0xC11, 0xB90];  // BLEND CULL DEPTH DITHER POFILL A2C SCOV SCISSOR STENCIL
function capIdx(cap) {
  switch (cap) {
    case 0xBE2: return 0; case 0xB44: return 1; case 0xB71: return 2; case 0xBD0: return 3; case 0x8037: return 4;
    case 0x809E: return 5; case 0x80A0: return 6; case 0xC11: return 7; case 0xB90: return 8;
  }
  return -1;
}

const utf8 = new TextDecoder();
const utf8enc = new TextEncoder();

// ---------------------------------------------------------------------------------------------------------
// Guest memory access. Views over the *whole* wasm memory are cached per ArrayBuffer identity (the buffer is
// replaced when memory grows), so uploads can pass (bigView, absoluteOffset, length) to WebGL2 without
// allocating a subarray per call.
export class GuestMem {
  constructor(host) { this.host = host; this.buf = null; this.base = 0; this.u8 = null; }
  sync() {
    const b = this.host.emu.mem.buffer;
    if (b !== this.buf) {
      this.buf = b; this.base = this.host.emu.x.emu_guest_base() >>> 0;   // unsigned: above 2 GiB an i32 pointer reads negative
      // lengths and element offsets use >>> / floor, never >>: wasm memory (and the absolute addresses in it) can pass 2 GiB, where a signed shift goes negative
      const n = b.byteLength, n2 = Math.floor(n / 2), n4 = Math.floor(n / 4);
      this.u8 = new Uint8Array(b); this.dv = new DataView(b);
      this.u16 = new Uint16Array(b, 0, n2); this.i32 = new Int32Array(b, 0, n4);
      this.u32 = new Uint32Array(b, 0, n4); this.f32 = new Float32Array(b, 0, n4);
    }
    return this;
  }
  /** Typed-array window onto guest memory: `m.u8[m.base + ptr ...]` is byte `ptr`. Here that is the whole address space.
   *  winW is for ranges the caller fully overwrites. */
  win(ptr, n) { return this.sync(); }
  winW(ptr, n) { return this.sync(); }
  rdF32(a) { this.sync(); return this.dv.getFloat32(this.base + a, true); }
  rdU32(a) { this.sync(); return this.dv.getUint32(this.base + a, true); }
  rdI32(a) { this.sync(); return this.dv.getInt32(this.base + a, true); }
  wrU32(a, v) { this.sync(); this.dv.setUint32(this.base + a, v >>> 0, true); }
  wrI32(a, v) { this.sync(); this.dv.setInt32(this.base + a, v | 0, true); }
  wrF32(a, v) { this.sync(); this.dv.setFloat32(this.base + a, v, true); }
  wrU8(a, v) { this.sync(); this.u8[this.base + a] = v; }
  bytes(a, n) { this.sync(); return this.u8.subarray(this.base + a, this.base + a + n); }
  cstr(a) { this.sync(); const u = this.u8, b = this.base; let e = a; while (u[b + e]) e++; return utf8.decode(u.subarray(b + a, b + e)); }
  /** Write `s` NUL-terminated into a buffer of `bufsize` bytes at `ptr`; store the length (without NUL) at lenPtr. */
  wrStr(ptr, bufsize, s, lenPtr) {
    const b = typeof s === 'string' ? utf8enc.encode(s) : s;
    const n = bufsize > 0 ? Math.min(b.length, bufsize - 1) : 0;
    if (n > 0 || bufsize > 0) { this.sync(); this.u8.set(b.subarray(0, n), this.base + ptr); this.u8[this.base + ptr + n] = 0; }
    if (lenPtr) this.wrI32(lenPtr, n);
  }
  allocCStr(s) { return this.host.allocCStr(s); }
  alloc(n) { return this.host.alloc(n); }
  release(p) { this.host.arena.release(p); }
}

// ---------------------------------------------------------------------------------------------------------
// Pixel format helpers.
function compCount(format) {
  switch (format) {
    case 0x1906: case 0x1909: case 0x1902: case 0x84F9: case 0x1903: return 1;   // ALPHA LUMINANCE DEPTH DEPTH_STENCIL RED
    case 0x190A: case 0x8227: return 2;                                           // LUMINANCE_ALPHA RG
    case 0x1907: return 3;                                                        // RGB
    case 0x1908: case 0x80E1: return 4;                                           // RGBA BGRA
  }
  return 4;
}
function bytesPerPixel(format, type) {
  switch (type) {
    case 0x8033: case 0x8034: case 0x8363: return 2;                              // 4444 5551 565
    case 0x84FA: return 4;                                                        // UNSIGNED_INT_24_8
    case 0x1401: return compCount(format);
    case 0x1403: case 0x8D61: case 0x140B: return 2 * compCount(format);          // USHORT, HALF_FLOAT(_OES)
    case 0x1405: case 0x1406: return 4 * compCount(format);
  }
  return compCount(format);
}
/** Size of a pixel rectangle in client memory under the given pack/unpack alignment (last row not padded). */
export function imageBytes(w, h, format, type, align) {
  if (w <= 0 || h <= 0) return 0;
  const bp = bytesPerPixel(format, type), row = Math.ceil((w * bp) / align) * align;
  return row * (h - 1) + w * bp;
}

// Result of mapTexFormat() (module-level scratch to avoid per-call allocation).
const TF = { ifmt: 0, fmt: 0, type: 0, bgra: false, expand: 0 };   // expand: 1 ALPHA, 2 LUMINANCE, 3 LUMINANCE_ALPHA -> RGBA on the CPU
/**
 * GLES2 (+OES_texture_float, OES_texture_half_float, OES_depth_texture, OES_packed_depth_stencil,
 * EXT_texture_format_BGRA8888) texture format triple -> the WebGL2 triple. WebGL2 needs *sized* internal formats
 * for float / half float / depth textures, has no BGRA and no texture swizzle (WebGL2 dropped TEXTURE_SWIZZLE_*): BGRA and float
 * ALPHA/LUMINANCE/LUMINANCE_ALPHA data are converted on the CPU by the caller (BGRA -> RGBA, A/L/LA -> RGBA16F/32F).
 */
function mapTexFormat(ifmt, fmt, type) {
  TF.ifmt = ifmt; TF.fmt = fmt; TF.type = type; TF.bgra = false; TF.expand = 0;
  if (fmt === 0x80E1) {                       // GL_BGRA_EXT
    TF.bgra = true; TF.fmt = 0x1908;
    if (ifmt === 0x80E1 || ifmt === 0x93A1) TF.ifmt = 0x1908;
    return TF;
  }
  switch (type) {
    case 0x1406:                              // FLOAT
      switch (fmt) {
        case 0x1908: TF.ifmt = 0x8814; break;                                     // RGBA32F
        case 0x1907: TF.ifmt = 0x8815; break;                                     // RGB32F
        case 0x1906: TF.ifmt = 0x8814; TF.fmt = 0x1908; TF.expand = 1; break;     // ALPHA -> (0,0,0,a) stored as RGBA32F
        case 0x1909: TF.ifmt = 0x8814; TF.fmt = 0x1908; TF.expand = 2; break;     // LUMINANCE -> (l,l,l,1)
        case 0x190A: TF.ifmt = 0x8814; TF.fmt = 0x1908; TF.expand = 3; break;     // LUMINANCE_ALPHA -> (l,l,l,a)
        case 0x1902: TF.ifmt = 0x8CAC; break;                                     // DEPTH_COMPONENT32F
      }
      break;
    case 0x8D61: case 0x140B:                 // HALF_FLOAT_OES / HALF_FLOAT
      TF.type = 0x140B;
      switch (fmt) {
        case 0x1908: TF.ifmt = 0x881A; break;                                     // RGBA16F
        case 0x1907: TF.ifmt = 0x881B; break;                                     // RGB16F
        case 0x1906: TF.ifmt = 0x881A; TF.fmt = 0x1908; TF.expand = 1; break;     // as above, stored as RGBA16F
        case 0x1909: TF.ifmt = 0x881A; TF.fmt = 0x1908; TF.expand = 2; break;
        case 0x190A: TF.ifmt = 0x881A; TF.fmt = 0x1908; TF.expand = 3; break;
      }
      break;
    case 0x1403: if (fmt === 0x1902) TF.ifmt = 0x81A5; break;                     // DEPTH_COMPONENT16
    case 0x1405: if (fmt === 0x1902) TF.ifmt = 0x81A6; break;                     // DEPTH_COMPONENT24
    case 0x84FA: if (fmt === 0x84F9) TF.ifmt = 0x88F0; break;                     // DEPTH24_STENCIL8
  }
  return TF;
}
function mapRbFormat(f) {
  switch (f) {
    case 0x81A7: return 0x8CAC;      // DEPTH_COMPONENT32_OES -> DEPTH_COMPONENT32F
    case 0x93A1: return 0x8058;      // BGRA8_EXT -> RGBA8 (the channel order of a renderbuffer is not observable)
    case 0x8051: return 0x8058;      // RGB8_OES -> RGBA8 (RGB8 is not color-renderable in WebGL2)
    case 0x84F9: return 0x88F0;      // DEPTH_STENCIL_OES
    case 0x8D61: return 0x881A;
  }
  return f;
}

// ---------------------------------------------------------------------------------------------------------
// GLSL ES 1.00 -> 3.00 fallback. WebGL2 compiles `#version 100` shaders unchanged, except for the extensions that
// became core in ESSL 3.00 (derivatives, gl_FragDepth, texture LOD, multiple render targets). Shaders that use them
// are rewritten to `#version 300 es` (and so must every shader linked with them: see GLContext.linkProgram).
const SPECIAL_RE = /GL_OES_standard_derivatives|GL_EXT_frag_depth|GL_EXT_shader_texture_lod|GL_EXT_draw_buffers|texture2DLodEXT|texture2DProjLodEXT|textureCubeLodEXT|texture2DGradEXT|gl_FragDepthEXT|\bdFd[xy]\b|\bfwidth\b|gl_FragData\s*\[\s*[1-9]/;
const VERSION_RE = /^\s*#\s*version\b/m;
export function toEssl3(src, isFrag) {
  let s = src.replace(/^[ \t]*#[ \t]*extension[ \t]+(?:GL_OES_standard_derivatives|GL_EXT_frag_depth|GL_EXT_shader_texture_lod|GL_EXT_draw_buffers)\b[^\n]*$/gm, '');
  const decl = [];
  if (isFrag) {
    s = s.replace(/\bvarying\b/g, 'in');
    const outs = new Set();
    s = s.replace(/\bgl_FragData\s*\[\s*(\d+)\s*\]/g, (m, n) => { outs.add(+n); return '_fd' + n; });
    if (/\bgl_FragColor\b/.test(s)) { s = s.replace(/\bgl_FragColor\b/g, '_fc'); decl.push('layout(location=0) out highp vec4 _fc;'); }
    for (const n of outs) decl.push(`layout(location=${n}) out highp vec4 _fd${n};`);
    s = s.replace(/\bgl_FragDepthEXT\b/g, 'gl_FragDepth');
  } else {
    s = s.replace(/\battribute\b/g, 'in').replace(/\bvarying\b/g, 'out');
  }
  s = s.replace(/\btexture2DProjLod(?:EXT)?\b/g, 'textureProjLod').replace(/\btexture2DLod(?:EXT)?\b/g, 'textureLod')
    .replace(/\btextureCubeLod(?:EXT)?\b/g, 'textureLod').replace(/\btexture2DGrad(?:EXT)?\b/g, 'textureGrad')
    .replace(/\btexture2DProj\b/g, 'textureProj').replace(/\btexture2D\b/g, 'texture').replace(/\btextureCube\b/g, 'texture');
  return '#version 300 es\n' + decl.join(' ') + '\n#line 1\n' + s;
}

// ---------------------------------------------------------------------------------------------------------
class Table {
  constructor() { this.m = new Map(); this.n = 1; }
  alloc() { while (this.m.has(this.n)) this.n++; return this.n++; }
}

/**
 * Objects shared by every context of a share group: ONE WebGL2 context. Its canvas is only a transient *staging* surface used
 * while presenting (see GLContext.frameCanvas); every GLContext renders into its own framebuffer object ("default framebuffer").
 */
export class GLGroup {
  constructor(gl, canvas) {
    this.gl = gl; this.canvas = canvas;
    this.cur = null; this.ctxs = new Set();
    this.bufs = new Table(); this.texs = new Table(); this.rbos = new Table(); this.shaders = new Table(); this.progs = new Table();
    this.lost = false;
    this.ext = new Set();                  // WebGL extension names that were successfully enabled
    for (const n of ['EXT_color_buffer_float', 'EXT_color_buffer_half_float', 'OES_texture_float_linear', 'EXT_texture_filter_anisotropic',
      'WEBGL_compressed_texture_s3tc', 'WEBGL_compressed_texture_etc1', 'WEBGL_compressed_texture_etc', 'WEBGL_compressed_texture_astc',
      'WEBGL_compressed_texture_pvrtc', 'EXT_float_blend', 'KHR_parallel_shader_compile']) {
      try { if (gl.getExtension(n)) this.ext.add(n); } catch (e) { /* ignore */ }
    }
    const dbg = gl.getExtension('WEBGL_debug_renderer_info');
    this.vendor = dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR);
    this.renderer = dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);
    this.nUnits = gl.getParameter(gl.MAX_COMBINED_TEXTURE_IMAGE_UNITS);
    this.nAttribs = gl.getParameter(gl.MAX_VERTEX_ATTRIBS);
    this.maxDrawBuffers = gl.getParameter(gl.MAX_DRAW_BUFFERS);
    this.maxSamples = gl.getParameter(gl.MAX_SAMPLES);
    this.extString = this._extensions();
  }
  /** GLES-style extension names that this WebGL2 context can honestly provide. */
  _extensions() {
    const x = this.ext, l = [
      'GL_OES_element_index_uint', 'GL_OES_packed_depth_stencil', 'GL_OES_depth24', 'GL_OES_depth_texture', 'GL_ANGLE_depth_texture',
      'GL_OES_rgb8_rgba8', 'GL_OES_texture_float', 'GL_OES_texture_half_float', 'GL_OES_texture_half_float_linear', 'GL_OES_texture_npot',
      'GL_OES_vertex_array_object', 'GL_OES_fbo_render_mipmap', 'GL_OES_standard_derivatives', 'GL_EXT_blend_minmax',
      'GL_EXT_texture_format_BGRA8888', 'GL_EXT_read_format_bgra', 'GL_EXT_texture_rg', 'GL_ANGLE_instanced_arrays',
      'GL_EXT_occlusion_query_boolean', 'GL_EXT_draw_buffers', 'GL_EXT_frag_depth', 'GL_EXT_shader_texture_lod',
      'GL_ANGLE_framebuffer_blit', 'GL_ANGLE_framebuffer_multisample', 'GL_CHROMIUM_framebuffer_multisample',
      'GL_CHROMIUM_map_sub', 'GL_CHROMIUM_enable_feature',
    ];
    if (x.has('OES_texture_float_linear')) l.push('GL_OES_texture_float_linear');
    if (x.has('EXT_color_buffer_half_float')) l.push('GL_EXT_color_buffer_half_float');
    if (x.has('EXT_color_buffer_float')) l.push('GL_EXT_color_buffer_float', 'GL_CHROMIUM_color_buffer_float_rgba');
    if (x.has('EXT_texture_filter_anisotropic')) l.push('GL_EXT_texture_filter_anisotropic');
    if (x.has('WEBGL_compressed_texture_s3tc')) l.push('GL_EXT_texture_compression_dxt1', 'GL_ANGLE_texture_compression_dxt3', 'GL_ANGLE_texture_compression_dxt5', 'GL_CHROMIUM_texture_compression_dxt5', 'GL_EXT_texture_compression_s3tc');
    if (x.has('WEBGL_compressed_texture_etc1')) l.push('GL_OES_compressed_ETC1_RGB8_texture');
    if (x.has('WEBGL_compressed_texture_pvrtc')) l.push('GL_IMG_texture_compression_pvrtc');
    if (x.has('WEBGL_compressed_texture_astc')) l.push('GL_KHR_texture_compression_astc_ldr');
    return l.join(' ');
  }
}

export class GLContext {
  /**
   * @param {GLGroup} grp   share group (owns the WebGL2 context)
   * @param {GuestMem} mem
   * @param {object} o      { w, h, attrs, log }
   */
  constructor(grp, mem, o) {
    this.type = 'graphics3d';             // PPAPI resource type tag
    this.grp = grp; this.gl = grp.gl; this.mem = mem; this.log = o.log;
    this.attrs = o.attrs; this.w = o.w; this.h = o.h;
    this.lost = false; this.err = 0;
    this.swapBehavior = o.attrs.swap || 0x3095; this.swapPending = false;
    this.fbos = new Table(); this.vaos = new Table(); this.queries = new Table();
    this.bufs = grp.bufs; this.texs = grp.texs; this.rbos = grp.rbos; this.shaders = grp.shaders; this.progs = grp.progs;
    const gl = this.gl;
    this.pv = null; this.po = 0;                     // out-params of the pointer->view helpers
    this._tf32 = new Float32Array(64); this._ti32 = new Int32Array(64); this._sw = null; this._sw32 = null;
    this.strPtr = new Map();                         // GetString cache
    this.maps = new Map();                           // MapBufferSubData / MapTexSubImage2D staging: guest ptr -> info
    const first = grp.ctxs.size === 0;
    this.opaque = !(o.attrs.alpha > 0);              // alpha:false context: alpha writes to its default framebuffer stay masked (alpha == 1)
    // The default framebuffer (name 0) of EVERY context is a private FBO: colour renderbuffer (multisampled on request) +
    // depth/stencil renderbuffer. Nothing is shared between contexts but the GL object namespaces.
    this.surf = { fbo: null, rb: null, ds: null, w: o.w, h: o.h, samples: 0, rfbo: null, rrb: null, rw: 0, rh: 0 };
    this._makeSurface(o.w, o.h);
    grp.cur = null;                                  // building the surface clobbered the GL bindings: re-apply on the next call
    this.defVao = { id: 0, obj: first ? null : gl.createVertexArray(), elem: 0 };
    this.curVao = this.defVao;
    const nu = grp.nUnits;
    this.st = {
      unit: 0, tex2d: new Int32Array(nu), texCube: new Int32Array(nu),
      arrayBuf: 0, drawFbo: 0, readFbo: 0, rbo: 0, prog: null,
      caps: new Uint8Array(9),
      vp: new Int32Array([0, 0, o.w, o.h]), sc: new Int32Array([0, 0, o.w, o.h]),
      clearColor: new Float32Array(4), clearDepth: 1, clearStencil: 0,
      colorMask: new Uint8Array([1, 1, 1, 1]), depthMask: 1, depthFunc: 0x201, depthRange: new Float32Array([0, 1]),
      sFunc: [0x207, 0, 0xFFFFFFFF, 0x207, 0, 0xFFFFFFFF], sOp: [0x1E00, 0x1E00, 0x1E00, 0x1E00, 0x1E00, 0x1E00], sMask: [0xFFFFFFFF, 0xFFFFFFFF],
      blendColor: new Float32Array(4), blendEq: [0x8006, 0x8006], blendFunc: [1, 0, 1, 0],
      cullFace: 0x405, frontFace: 0x901, lineWidth: 1, poly: new Float32Array(2), cov: [1, 0],
      hintMip: 0x1100, hintDeriv: 0x1100, packAlign: 4, unpackAlign: 4,
      gen: new Float32Array(grp.nAttribs * 4),
    };
    for (let i = 0; i < grp.nAttribs; i++) this.st.gen[i * 4 + 3] = 1;
    this.st.caps[3] = 1;                              // DITHER is on by default
    grp.ctxs.add(this);
  }

  // ---- error handling --------------------------------------------------------------------------------
  setErr(e) {
    if (this.err !== 0) return;
    const g = this.gl.getError();                    // an earlier WebGL-generated error wins (GLES keeps the first)
    this.err = (g !== 0 && g !== 0x9242) ? g : e;
  }
  getError() {
    let e = this.err; this.err = 0;
    for (let i = 0, g; i < 16 && (g = this.gl.getError()) !== 0; i++) if (e === 0 && g !== 0x9242) e = g;
    return e;
  }

  // ---- surfaces / context switching ------------------------------------------------------------------
  _makeSurface(w, h) {
    const gl = this.gl, s = this.surf, a = this.attrs, RB = gl.RENDERBUFFER;
    s.w = Math.max(1, w); s.h = Math.max(1, h);
    const wantMs = a.samples > 0 || a.sampleBuffers > 0;
    const hasDS = a.depth > 0 || a.stencil > 0;
    if (!s.fbo) {
      s.fbo = gl.createFramebuffer(); s.rb = gl.createRenderbuffer();
      if (hasDS) s.ds = gl.createRenderbuffer();
    }
    const want = wantMs ? Math.min(a.samples > 0 ? a.samples : 4, this.grp.maxSamples) : 0;
    const store = (fmt) => (want > 0 ? gl.renderbufferStorageMultisample(RB, want, fmt, s.w, s.h) : gl.renderbufferStorage(RB, fmt, s.w, s.h));
    gl.bindFramebuffer(gl.FRAMEBUFFER, s.fbo);
    gl.bindRenderbuffer(RB, s.rb); store(gl.RGBA8);
    s.samples = want > 0 ? gl.getRenderbufferParameter(RB, gl.RENDERBUFFER_SAMPLES) : 0;
    gl.framebufferRenderbuffer(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, RB, s.rb);
    if (s.ds) {
      const both = a.depth > 0 && a.stencil > 0;
      gl.bindRenderbuffer(RB, s.ds); store(both ? 0x88F0 : a.depth > 0 ? 0x81A6 : 0x8D48);
      gl.framebufferRenderbuffer(gl.FRAMEBUFFER, both ? 0x821A : a.depth > 0 ? 0x8D00 : 0x8D20, RB, s.ds);
    }
    if (this.opaque) {                                // alpha:false: the colour buffer's alpha is 1 from the start
      gl.colorMask(false, false, false, true); gl.clearColor(0, 0, 0, 1); gl.clear(gl.COLOR_BUFFER_BIT);
    }
    s.rw = 0;                                         // the lazily created single-sample resolve copy is re-sized on demand
  }
  /** Effective colour write mask: alpha of an opaque context's default framebuffer is never written. */
  _applyColorMask() {
    const m = this.st.colorMask;
    this.gl.colorMask(!!m[0], !!m[1], !!m[2], !!m[3] && !(this.opaque && this.st.drawFbo === 0));
  }
  makeCurrent() {
    const g = this.grp, prev = g.cur;
    g.cur = this;
    if (prev !== this) this.applyState(prev);
  }
  /** Re-establish this context's GL state on the (shared) WebGL context; `prev` = the context that was current (null: unknown). */
  applyState(prev) {
    const gl = this.gl, st = this.st, tb = (t, id) => (id ? (this.texs.m.get(id)?.obj ?? null) : null);
    gl.bindVertexArray(this.curVao.obj);
    gl.bindBuffer(gl.ARRAY_BUFFER, st.arrayBuf ? (this.bufs.m.get(st.arrayBuf)?.obj ?? null) : null);
    gl.bindFramebuffer(0x8CA9, this._fboObj(st.drawFbo)); gl.bindFramebuffer(0x8CA8, this._fboObj(st.readFbo));
    gl.bindRenderbuffer(gl.RENDERBUFFER, st.rbo ? (this.rbos.m.get(st.rbo)?.obj ?? null) : null);
    gl.useProgram(st.prog ? st.prog.obj : null);
    for (let u = 0; u < st.tex2d.length; u++) {
      if (!prev || st.tex2d[u] || st.texCube[u] || prev.st.tex2d[u] || prev.st.texCube[u]) {
        gl.activeTexture(0x84C0 + u); gl.bindTexture(gl.TEXTURE_2D, tb(0, st.tex2d[u])); gl.bindTexture(gl.TEXTURE_CUBE_MAP, tb(0, st.texCube[u]));
      }
    }
    gl.activeTexture(0x84C0 + st.unit);
    for (let i = 0; i < 9; i++) st.caps[i] ? gl.enable(CAPS[i]) : gl.disable(CAPS[i]);
    gl.viewport(st.vp[0], st.vp[1], st.vp[2], st.vp[3]); gl.scissor(st.sc[0], st.sc[1], st.sc[2], st.sc[3]);
    const cc = st.clearColor; gl.clearColor(cc[0], cc[1], cc[2], cc[3]); gl.clearDepth(st.clearDepth); gl.clearStencil(st.clearStencil);
    this._applyColorMask(); gl.depthMask(!!st.depthMask);
    gl.depthFunc(st.depthFunc); gl.depthRange(st.depthRange[0], st.depthRange[1]);
    gl.stencilFuncSeparate(0x404, st.sFunc[0], st.sFunc[1], st.sFunc[2]); gl.stencilFuncSeparate(0x405, st.sFunc[3], st.sFunc[4], st.sFunc[5]);
    gl.stencilOpSeparate(0x404, st.sOp[0], st.sOp[1], st.sOp[2]); gl.stencilOpSeparate(0x405, st.sOp[3], st.sOp[4], st.sOp[5]);
    gl.stencilMaskSeparate(0x404, st.sMask[0]); gl.stencilMaskSeparate(0x405, st.sMask[1]);
    const bc = st.blendColor; gl.blendColor(bc[0], bc[1], bc[2], bc[3]);
    gl.blendEquationSeparate(st.blendEq[0], st.blendEq[1]); gl.blendFuncSeparate(st.blendFunc[0], st.blendFunc[1], st.blendFunc[2], st.blendFunc[3]);
    gl.cullFace(st.cullFace); gl.frontFace(st.frontFace); gl.lineWidth(st.lineWidth); gl.polygonOffset(st.poly[0], st.poly[1]);
    gl.sampleCoverage(st.cov[0], !!st.cov[1]);
    gl.hint(0x8192, st.hintMip); gl.hint(0x8B8B, st.hintDeriv);
    gl.pixelStorei(0xCF5, st.unpackAlign); gl.pixelStorei(0xD05, st.packAlign);
    for (let i = 0; i < this.grp.nAttribs; i++) gl.vertexAttrib4f(i, st.gen[i * 4], st.gen[i * 4 + 1], st.gen[i * 4 + 2], st.gen[i * 4 + 3]);
  }
  _fboObj(id) { return id === 0 ? this.surf.fbo : (this.fbos.m.get(id)?.obj ?? null); }

  /**
   * Present: blit this context's own surface (resolving MSAA) into the group's staging canvas and return it. The consumer
   * (Presenter.presentCanvas) copies the pixels synchronously, so contexts never see each other's frames, and the staging
   * canvas' size is not part of any context's state.
   */
  frameCanvas() {
    const gl = this.gl, c = this.grp.canvas, s = this.surf, st = this.st;
    if (c.width !== s.w) c.width = s.w;
    if (c.height !== s.h) c.height = s.h;
    gl.bindFramebuffer(0x8CA8, s.fbo); gl.bindFramebuffer(0x8CA9, null);
    if (st.caps[7]) gl.disable(0xC11);
    gl.colorMask(true, true, true, true);
    gl.blitFramebuffer(0, 0, s.w, s.h, 0, 0, s.w, s.h, 0x4000, 0x2600);
    gl.flush();
    gl.bindFramebuffer(0x8CA8, this._fboObj(st.readFbo)); gl.bindFramebuffer(0x8CA9, this._fboObj(st.drawFbo));
    if (st.caps[7]) gl.enable(0xC11);
    this._applyColorMask();
    return c;
  }
  /** A multisampled default framebuffer cannot be read directly: resolve it into a single-sample copy and read that. */
  _readBegin() {
    const st = this.st, s = this.surf;
    if (st.readFbo !== 0 || s.samples === 0) return false;
    const gl = this.gl;
    if (!s.rfbo) { s.rfbo = gl.createFramebuffer(); s.rrb = gl.createRenderbuffer(); }
    if (s.rw !== s.w || s.rh !== s.h) {
      gl.bindRenderbuffer(gl.RENDERBUFFER, s.rrb); gl.renderbufferStorage(gl.RENDERBUFFER, gl.RGBA8, s.w, s.h);
      gl.bindFramebuffer(0x8CA9, s.rfbo); gl.framebufferRenderbuffer(0x8CA9, gl.COLOR_ATTACHMENT0, gl.RENDERBUFFER, s.rrb);
      gl.bindRenderbuffer(gl.RENDERBUFFER, st.rbo ? (this.rbos.m.get(st.rbo)?.obj ?? null) : null);
      s.rw = s.w; s.rh = s.h;
    }
    gl.bindFramebuffer(0x8CA8, s.fbo); gl.bindFramebuffer(0x8CA9, s.rfbo);
    if (st.caps[7]) gl.disable(0xC11);
    gl.colorMask(true, true, true, true);
    gl.blitFramebuffer(0, 0, s.w, s.h, 0, 0, s.w, s.h, 0x4000, 0x2600);
    if (st.caps[7]) gl.enable(0xC11);
    this._applyColorMask();
    gl.bindFramebuffer(0x8CA9, this._fboObj(st.drawFbo));
    gl.bindFramebuffer(0x8CA8, s.rfbo);
    return true;
  }
  _readEnd(redirected) { if (redirected) this.gl.bindFramebuffer(0x8CA8, this.surf.fbo); }
  resizeBuffers(w, h) {
    w = Math.max(1, w); h = Math.max(1, h);
    this.w = w; this.h = h;
    this._makeSurface(w, h);
    this.grp.cur = null; this.makeCurrent();         // _makeSurface clobbered bindings
  }
  destroy() {
    const g = this.grp, gl = this.gl;
    if (g.cur === this) g.cur = null;
    g.ctxs.delete(this);
    for (const p of this.strPtr.values()) this.mem.release(p);
    this.strPtr.clear();
    for (const m of this.maps.values()) if (m.ptr) this.mem.release(m.ptr);
    this.maps.clear();
    if (g.ctxs.size === 0) {
      g.closing = true;                              // suppress the 'context lost' report for our own teardown
      try { gl.getExtension('WEBGL_lose_context')?.loseContext(); } catch (e) { /* ignore */ }
    } else {
      try {
        const sf = this.surf;
        gl.deleteFramebuffer(sf.fbo); gl.deleteRenderbuffer(sf.rb); if (sf.ds) gl.deleteRenderbuffer(sf.ds);
        if (sf.rfbo) { gl.deleteFramebuffer(sf.rfbo); gl.deleteRenderbuffer(sf.rrb); }
        for (const e of this.fbos.m.values()) gl.deleteFramebuffer(e.obj);
        for (const e of this.vaos.m.values()) gl.deleteVertexArray(e.obj);
        for (const e of this.queries.m.values()) gl.deleteQuery(e.obj);
        if (this.defVao.obj) gl.deleteVertexArray(this.defVao.obj);
      } catch (e) { /* ignore */ }
    }
  }

  // ---- pointer -> typed-array helpers ----------------------------------------------------------------
  _tmp32(kind, n) {
    if (kind === 0) { if (this._tf32.length < n) this._tf32 = new Float32Array(n); return this._tf32; }
    if (this._ti32.length < n) this._ti32 = new Int32Array(n); return this._ti32;
  }
  /** Float32 view of `n` floats at guest `ptr`: sets this.pv (view) / this.po (element offset). */
  _fv(ptr, n) {
    const m = this.mem.win(ptr, n * 4), abs = m.base + ptr;
    if ((abs & 3) === 0) { this.pv = m.f32; this.po = abs >>> 2; return; }
    const t = this._tmp32(0, n); new Uint8Array(t.buffer, 0, n * 4).set(m.u8.subarray(abs, abs + n * 4)); this.pv = t; this.po = 0;
  }
  _iv(ptr, n) {
    const m = this.mem.win(ptr, n * 4), abs = m.base + ptr;
    if ((abs & 3) === 0) { this.pv = m.i32; this.po = abs >>> 2; return; }
    const t = this._tmp32(1, n); new Uint8Array(t.buffer, 0, n * 4).set(m.u8.subarray(abs, abs + n * 4)); this.pv = t; this.po = 0;
  }
  /** View of pixel data of GL `type` at guest `ptr` (nbytes long) in the element type WebGL requires. */
  _pix(type, ptr, nbytes) {
    const m = this.mem.win(ptr, nbytes), abs = m.base + ptr;
    switch (type) {
      case 0x1401: this.pv = m.u8; this.po = abs; return;
      case 0x1406: if ((abs & 3) === 0) { this.pv = m.f32; this.po = abs >>> 2; return; } break;
      case 0x1405: case 0x84FA: if ((abs & 3) === 0) { this.pv = m.u32; this.po = abs >>> 2; return; } break;
      default: if ((abs & 1) === 0) { this.pv = m.u16; this.po = abs >>> 1; return; }
    }
    // unaligned: copy into a fresh, aligned buffer of the right element type
    const b = new ArrayBuffer((nbytes + 3) & ~3);
    new Uint8Array(b).set(m.u8.subarray(abs, abs + nbytes));
    this.pv = type === 0x1406 ? new Float32Array(b) : (type === 0x1405 || type === 0x84FA) ? new Uint32Array(b) : new Uint16Array(b);
    this.po = 0;
  }
  _swizzle(w, h, align, ptr, write) {
    // scratch buffer (grow-only) for BGRA <-> RGBA conversion of 8-bit 4-component rows
    const stride = Math.ceil((w * 4) / align) * align, total = stride * (h - 1) + w * 4;
    const m = write ? this.mem.winW(ptr, total) : this.mem.win(ptr, total);
    if (!this._sw || this._sw.length < total) { const b = new ArrayBuffer((total + 255) & ~255); this._sw = new Uint8Array(b); this._sw32 = new Uint32Array(b); }
    return { stride, total, m };
  }
  /** float/half-float ALPHA|LUMINANCE|LUMINANCE_ALPHA pixels -> tightly packed RGBA (this.pv/po), exactly what the GLES texture samples as. */
  _expandLA(w, h, type, ptr, mode) {
    const half = type !== 0x1406, comps = mode === 3 ? 2 : 1, esz = half ? 2 : 4, align = this.st.unpackAlign;
    const rowElems = (Math.ceil((w * comps * esz) / align) * align) / esz;
    this._pix(half ? 0x1403 : 0x1406, ptr, imageBytes(w, h, mode === 3 ? 0x190A : 0x1909, half ? 0x8D61 : 0x1406, align));
    const src = this.pv, so = this.po, n = w * h * 4;
    let dst;
    if (half) { if (!this._exH || this._exH.length < n) this._exH = new Uint16Array(n); dst = this._exH; }
    else { if (!this._exF || this._exF.length < n) this._exF = new Float32Array(n); dst = this._exF; }
    const one = half ? 0x3C00 : 1;
    for (let y = 0; y < h; y++) {
      let i = so + y * rowElems, j = y * w * 4;
      for (let x = 0; x < w; x++, i += comps, j += 4) {
        if (mode === 1) { dst[j] = 0; dst[j + 1] = 0; dst[j + 2] = 0; dst[j + 3] = src[i]; }
        else { const v = src[i]; dst[j] = v; dst[j + 1] = v; dst[j + 2] = v; dst[j + 3] = mode === 3 ? src[i + 1] : one; }
      }
    }
    this.pv = dst; this.po = 0;
  }
  _bgraIn(ptr, w, h) {            // guest BGRA -> scratch RGBA; sets pv/po for the upload
    const { stride, total, m } = this._swizzle(w, h, this.st.unpackAlign, ptr, false);
    const abs = m.base + ptr, dst = this._sw;
    if ((abs & 3) === 0 && (stride & 3) === 0) {
      const src = m.u32, d = this._sw32, ws = stride >> 2, so = abs >>> 2;
      for (let y = 0; y < h; y++) {
        let i = so + y * ws, j = y * ws;
        for (let x = 0; x < w; x++, i++, j++) { const v = src[i]; d[j] = (v & 0xFF00FF00) | ((v & 0xFF) << 16) | ((v >>> 16) & 0xFF); }
      }
    } else {
      const src = m.u8;
      for (let y = 0; y < h; y++) {
        let i = abs + y * stride, j = y * stride;
        for (let x = 0; x < w; x++, i += 4, j += 4) { dst[j] = src[i + 2]; dst[j + 1] = src[i + 1]; dst[j + 2] = src[i]; dst[j + 3] = src[i + 3]; }
      }
    }
    this.pv = dst; this.po = 0;
    return total;
  }

  // ---- object name generation / deletion -------------------------------------------------------------
  _gen(tbl, kind, n, out) {
    if (n < 0) return this.setErr(E.VALUE);
    const gl = this.gl;
    for (let i = 0; i < n; i++) {
      const id = tbl.alloc();
      const obj = kind === 0 ? gl.createBuffer() : kind === 1 ? gl.createTexture() : kind === 2 ? gl.createRenderbuffer() : kind === 3 ? gl.createFramebuffer() : kind === 4 ? gl.createVertexArray() : gl.createQuery();
      obj.$id = id;
      tbl.m.set(id, { id, obj, used: false });
      this.mem.wrU32(out + i * 4, id);
    }
  }
  genBuffers(n, p) { this._gen(this.bufs, 0, n, p); }
  genTextures(n, p) { this._gen(this.texs, 1, n, p); }
  genRenderbuffers(n, p) { this._gen(this.rbos, 2, n, p); }
  genFramebuffers(n, p) { this._gen(this.fbos, 3, n, p); }
  genVertexArrays(n, p) { this._gen(this.vaos, 4, n, p); }
  genQueries(n, p) { this._gen(this.queries, 5, n, p); }
  _del(tbl, n, p, fn) {
    if (n < 0) return this.setErr(E.VALUE);
    for (let i = 0; i < n; i++) {
      const id = this.mem.rdU32(p + i * 4), e = tbl.m.get(id);
      if (!e) continue;
      tbl.m.delete(id);
      fn(e);
    }
  }
  deleteBuffers(n, p) {
    this._del(this.bufs, n, p, (e) => {
      if (this.st.arrayBuf === e.id) this.st.arrayBuf = 0;
      if (this.curVao.elem === e.id) this.curVao.elem = 0;
      this.gl.deleteBuffer(e.obj);
    });
  }
  deleteTextures(n, p) {
    this._del(this.texs, n, p, (e) => {
      const st = this.st;
      for (let u = 0; u < st.tex2d.length; u++) { if (st.tex2d[u] === e.id) st.tex2d[u] = 0; if (st.texCube[u] === e.id) st.texCube[u] = 0; }
      this.gl.deleteTexture(e.obj);
    });
  }
  deleteRenderbuffers(n, p) { this._del(this.rbos, n, p, (e) => { if (this.st.rbo === e.id) this.st.rbo = 0; this.gl.deleteRenderbuffer(e.obj); }); }
  deleteFramebuffers(n, p) {
    this._del(this.fbos, n, p, (e) => {
      const wasDraw = this.st.drawFbo === e.id;
      if (wasDraw) this.st.drawFbo = 0;
      if (this.st.readFbo === e.id) this.st.readFbo = 0;
      this.gl.deleteFramebuffer(e.obj);
      if (wasDraw && this.opaque) this._applyColorMask();
    });
  }
  deleteVertexArrays(n, p) {
    this._del(this.vaos, n, p, (e) => { if (this.curVao === e) { this.curVao = this.defVao; this.st.vao = 0; } this.gl.deleteVertexArray(e.obj); });
  }
  deleteQueries(n, p) { this._del(this.queries, n, p, (e) => this.gl.deleteQuery(e.obj)); }
  isBuffer(id) { return this.bufs.m.get(id)?.used ? 1 : 0; }
  isTexture(id) { return this.texs.m.get(id)?.used ? 1 : 0; }
  isRenderbuffer(id) { return this.rbos.m.get(id)?.used ? 1 : 0; }
  isFramebuffer(id) { return this.fbos.m.get(id)?.used ? 1 : 0; }
  isVertexArray(id) { return this.vaos.m.get(id)?.used ? 1 : 0; }
  isQuery(id) { return this.queries.m.get(id)?.used ? 1 : 0; }
  isShader(id) { return this.shaders.m.has(id) ? 1 : 0; }
  isProgram(id) { return this.progs.m.has(id) ? 1 : 0; }
  _get(tbl, id, kind) {            // GLES allows binding names that were never Gen'd: create the object on first bind
    let e = tbl.m.get(id);
    if (e === undefined) {
      const gl = this.gl;
      const obj = kind === 0 ? gl.createBuffer() : kind === 1 ? gl.createTexture() : kind === 2 ? gl.createRenderbuffer() : kind === 3 ? gl.createFramebuffer() : kind === 4 ? gl.createVertexArray() : gl.createQuery();
      obj.$id = id; e = { id, obj, used: false }; tbl.m.set(id, e);
    }
    e.used = true;
    return e;
  }

  // ---- bindings --------------------------------------------------------------------------------------
  bindBuffer(target, id) {
    const gl = this.gl, st = this.st;
    if (target === 0x8892) {
      if (st.arrayBuf === id && id !== 0) return;
      st.arrayBuf = id;
    } else if (target === 0x8893) {
      this.curVao.elem = id;
    } else return this.setErr(E.ENUM);
    gl.bindBuffer(target, id ? this._get(this.bufs, id, 0).obj : null);
  }
  bindTexture(target, id) {
    const gl = this.gl, st = this.st;
    if (target !== 0xDE1 && target !== 0x8513) return this.setErr(E.ENUM);
    const arr = target === 0xDE1 ? st.tex2d : st.texCube;
    if (arr[st.unit] === id) return;
    arr[st.unit] = id;
    gl.bindTexture(target, id ? this._get(this.texs, id, 1).obj : null);
  }
  activeTexture(t) {
    const u = t - 0x84C0;
    if (u < 0 || u >= this.st.tex2d.length) return this.setErr(E.ENUM);
    if (this.st.unit === u) return;
    this.st.unit = u; this.gl.activeTexture(t);
  }
  bindFramebuffer(target, id) {
    const gl = this.gl, st = this.st;
    if (target !== 0x8D40 && target !== 0x8CA8 && target !== 0x8CA9) return this.setErr(E.ENUM);
    const obj = id === 0 ? this.surf.fbo : this._get(this.fbos, id, 3).obj;
    const wasDefault = st.drawFbo === 0;
    if (target !== 0x8CA8) st.drawFbo = id;
    if (target !== 0x8CA9) st.readFbo = id;
    gl.bindFramebuffer(target, obj);
    if (this.opaque && target !== 0x8CA8 && wasDefault !== (id === 0)) this._applyColorMask();
  }
  bindRenderbuffer(target, id) {
    const gl = this.gl;
    if (target !== 0x8D41) return this.setErr(E.ENUM);
    this.st.rbo = id;
    gl.bindRenderbuffer(target, id ? this._get(this.rbos, id, 2).obj : null);
  }
  bindVertexArray(id) {
    const gl = this.gl;
    const e = id === 0 ? this.defVao : this._get(this.vaos, id, 4);
    if (this.curVao === e) return;
    this.curVao = e; gl.bindVertexArray(e.obj);
  }
  useProgram(id) {
    const st = this.st;
    let p = null;
    if (id) { p = this.progs.m.get(id); if (!p) return this.setErr(E.VALUE); }
    if (st.prog === p) return;
    st.prog = p; this.gl.useProgram(p ? p.obj : null);
  }

  // ---- buffers ---------------------------------------------------------------------------------------
  bufferData(target, size, ptr, usage) {
    if (size < 0) return this.setErr(E.VALUE);
    if (target !== 0x8892 && target !== 0x8893) return this.setErr(E.ENUM);
    if (ptr === 0 || size === 0) this.gl.bufferData(target, size, usage);
    else { const m = this.mem.win(ptr, size); this.gl.bufferData(target, m.u8, usage, m.base + ptr, size); }
  }
  bufferSubData(target, offset, size, ptr) {
    if (size < 0 || offset < 0) return this.setErr(E.VALUE);
    if (target !== 0x8892 && target !== 0x8893) return this.setErr(E.ENUM);
    if (size === 0) return;
    const m = this.mem.win(ptr, size); this.gl.bufferSubData(target, offset, m.u8, m.base + ptr, size);
  }

  // ---- shaders and programs --------------------------------------------------------------------------
  createShader(type) {
    if (type !== 0x8B31 && type !== 0x8B30) { this.setErr(E.ENUM); return 0; }
    const id = this.shaders.alloc(), obj = this.gl.createShader(type);
    this.shaders.m.set(id, { id, obj, type, src: '', deleted: false, att: 0, v3: false, obj3: null });
    return id;
  }
  shaderSource(id, count, strs, lens) {
    const s = this.shaders.m.get(id);
    if (!s) return this.setErr(this.progs.m.has(id) ? E.OP : E.VALUE);
    if (count < 0) return this.setErr(E.VALUE);
    let src = '';
    for (let i = 0; i < count; i++) {
      const p = this.mem.rdU32(strs + i * 4);
      const l = lens ? this.mem.rdI32(lens + i * 4) : -1;
      src += l < 0 ? this.mem.cstr(p) : utf8.decode(this.mem.bytes(p, l));
    }
    s.src = src;
    this.gl.shaderSource(s.obj, src);
  }
  compileShader(id) {
    const s = this.shaders.m.get(id), gl = this.gl;
    if (!s) return this.setErr(this.progs.m.has(id) ? E.OP : E.VALUE);
    s.v3 = false;
    if (s.obj3) { gl.deleteShader(s.obj3); s.obj3 = null; }
    gl.compileShader(s.obj);
    // Shaders using extensions that only exist in ESSL 3.00 under WebGL2: find out now whether plain ESSL 1.00 works.
    if (SPECIAL_RE.test(s.src) && !VERSION_RE.test(s.src) && !gl.getShaderParameter(s.obj, gl.COMPILE_STATUS)) {
      const o = this._shader3(s);
      if (gl.getShaderParameter(o, gl.COMPILE_STATUS)) s.v3 = true;
      else { gl.deleteShader(o); s.obj3 = null; }
    }
  }
  _shader3(s) {
    if (s.obj3) return s.obj3;
    const gl = this.gl, o = gl.createShader(s.type);
    gl.shaderSource(o, toEssl3(s.src, s.type === 0x8B30));
    gl.compileShader(o);
    return (s.obj3 = o);
  }
  _shaderLog(s) { return this.gl.getShaderInfoLog(s.v3 ? s.obj3 : s.obj) || ''; }
  _dropShader(s) {
    if (s.deleted && s.att === 0) {
      this.shaders.m.delete(s.id);
      this.gl.deleteShader(s.obj);
      if (s.obj3) this.gl.deleteShader(s.obj3);
    }
  }
  deleteShader(id) {
    const s = this.shaders.m.get(id);
    if (!s) return id ? this.setErr(this.progs.m.has(id) ? E.OP : E.VALUE) : undefined;
    s.deleted = true; this._dropShader(s);
  }
  createProgram() {
    const id = this.progs.alloc(), obj = this.gl.createProgram();
    obj.$id = id;
    this.progs.m.set(id, { id, obj, deleted: false, shaders: [], real: new Set(), uTab: null, uByName: null, maxUL: -1, maxAL: -1 });
    return id;
  }
  attachShader(pid, sid) {
    const p = this.progs.m.get(pid), s = this.shaders.m.get(sid);
    if (!p || !s) return this.setErr(E.VALUE);
    if (p.shaders.includes(s) || p.shaders.some((x) => x.type === s.type)) return this.setErr(E.OP);
    p.shaders.push(s); s.att++;
  }
  detachShader(pid, sid) {
    const p = this.progs.m.get(pid), s = this.shaders.m.get(sid);
    if (!p || !s) return this.setErr(E.VALUE);
    const i = p.shaders.indexOf(s);
    if (i < 0) return this.setErr(E.OP);
    p.shaders.splice(i, 1); s.att--;
    for (const o of [s.obj, s.obj3]) if (o && p.real.has(o)) { this.gl.detachShader(p.obj, o); p.real.delete(o); }
    this._dropShader(s);
  }
  deleteProgram(id) {
    const p = this.progs.m.get(id);
    if (!p) return id ? this.setErr(this.shaders.m.has(id) ? E.OP : E.VALUE) : undefined;
    this.progs.m.delete(id); p.deleted = true;
    this.gl.deleteProgram(p.obj);
    for (const s of p.shaders) { s.att--; this._dropShader(s); }
    p.shaders = [];
  }
  linkProgram(id) {
    const p = this.progs.m.get(id), gl = this.gl;
    if (!p) return this.setErr(this.shaders.m.has(id) ? E.OP : E.VALUE);
    const use3 = p.shaders.some((s) => s.v3);
    for (const o of p.real) gl.detachShader(p.obj, o);
    p.real.clear();
    for (const s of p.shaders) { const o = use3 ? this._shader3(s) : s.obj; gl.attachShader(p.obj, o); p.real.add(o); }
    p.uTab = null; p.uByName = null; p.maxUL = p.maxAL = -1;
    gl.linkProgram(p.obj);
  }
  validateProgram(id) { const p = this.progs.m.get(id); if (!p) return this.setErr(E.VALUE); this.gl.validateProgram(p.obj); }
  bindAttribLocation(pid, index, name) {
    const p = this.progs.m.get(pid); if (!p) return this.setErr(E.VALUE);
    this.gl.bindAttribLocation(p.obj, index, name);
  }
  getAttribLocation(pid, name) {
    const p = this.progs.m.get(pid); if (!p) { this.setErr(E.VALUE); return -1; }
    return this.gl.getAttribLocation(p.obj, name);
  }
  getShaderiv(id, pname) {
    const s = this.shaders.m.get(id), gl = this.gl;
    if (!s) { this.setErr(this.progs.m.has(id) ? E.OP : E.VALUE); return null; }
    switch (pname) {
      case 0x8B4F: return s.type;
      case 0x8B80: return s.deleted ? 1 : 0;
      case 0x8B81: return s.v3 ? 1 : (gl.getShaderParameter(s.obj, gl.COMPILE_STATUS) ? 1 : 0);
      case 0x8B84: { const l = this._shaderLog(s); return l ? utf8enc.encode(l).length + 1 : 0; }
      case 0x8B88: return s.src ? utf8enc.encode(s.src).length + 1 : 0;
    }
    this.setErr(E.ENUM); return null;
  }
  getShaderInfoLog(id) { const s = this.shaders.m.get(id); if (!s) { this.setErr(E.VALUE); return null; } return this._shaderLog(s); }
  getShaderSource(id) { const s = this.shaders.m.get(id); if (!s) { this.setErr(E.VALUE); return null; } return s.src; }
  getProgramInfoLog(id) { const p = this.progs.m.get(id); if (!p) { this.setErr(E.VALUE); return null; } return this.gl.getProgramInfoLog(p.obj) || ''; }
  getProgramiv(id, pname) {
    const p = this.progs.m.get(id), gl = this.gl;
    if (!p) { this.setErr(this.shaders.m.has(id) ? E.OP : E.VALUE); return null; }
    switch (pname) {
      case 0x8B80: return p.deleted ? 1 : 0;
      case 0x8B82: return gl.getProgramParameter(p.obj, gl.LINK_STATUS) ? 1 : 0;
      case 0x8B83: return gl.getProgramParameter(p.obj, gl.VALIDATE_STATUS) ? 1 : 0;
      case 0x8B84: { const l = gl.getProgramInfoLog(p.obj); return l ? utf8enc.encode(l).length + 1 : 0; }
      case 0x8B85: return p.shaders.length;
      case 0x8B86: return gl.getProgramParameter(p.obj, gl.ACTIVE_UNIFORMS);
      case 0x8B89: return gl.getProgramParameter(p.obj, gl.ACTIVE_ATTRIBUTES);
      case 0x8B87: return this._maxNameLen(p, true);
      case 0x8B8A: return this._maxNameLen(p, false);
    }
    this.setErr(E.ENUM); return null;
  }
  _maxNameLen(p, uniform) {
    const gl = this.gl;
    if (!gl.getProgramParameter(p.obj, gl.LINK_STATUS)) return 0;
    if (uniform ? p.maxUL < 0 : p.maxAL < 0) {
      let max = 0;
      const n = gl.getProgramParameter(p.obj, uniform ? gl.ACTIVE_UNIFORMS : gl.ACTIVE_ATTRIBUTES);
      for (let i = 0; i < n; i++) { const a = uniform ? gl.getActiveUniform(p.obj, i) : gl.getActiveAttrib(p.obj, i); if (a) max = Math.max(max, a.name.length + 1); }
      if (uniform) p.maxUL = max; else p.maxAL = max;
    }
    return uniform ? p.maxUL : p.maxAL;
  }
  getAttachedShaders(pid) { const p = this.progs.m.get(pid); if (!p) { this.setErr(E.VALUE); return null; } return p.shaders.map((s) => s.id); }
  getActive(pid, index, uniform) {
    const p = this.progs.m.get(pid); if (!p) { this.setErr(E.VALUE); return null; }
    const a = uniform ? this.gl.getActiveUniform(p.obj, index) : this.gl.getActiveAttrib(p.obj, index);
    if (!a) { this.setErr(E.VALUE); return null; }
    return a;
  }
  getShaderPrecisionFormat(shaderType, precType) {
    const r = this.gl.getShaderPrecisionFormat(shaderType, precType);
    if (!r) { this.setErr(E.ENUM); return null; }
    return r;
  }

  // ---- uniforms --------------------------------------------------------------------------------------
  _buildUniforms(p) {
    const gl = this.gl, n = gl.getProgramParameter(p.obj, gl.LINK_STATUS) ? gl.getProgramParameter(p.obj, gl.ACTIVE_UNIFORMS) : 0;
    const tab = [], by = new Map();
    for (let i = 0; i < n; i++) {
      const a = gl.getActiveUniform(p.obj, i); if (!a) continue;
      const arr = a.name.endsWith('[0]');
      const e = { idx: i, name: arr ? a.name.slice(0, -3) : a.name, raw: a.name, size: a.size, type: a.type, isArr: arr, locs: [] };
      tab[i] = e; by.set(e.name, e); if (arr) by.set(a.name, e);
    }
    p.uTab = tab; p.uByName = by;
    return tab;
  }
  getUniformLocation(pid, name) {
    const p = this.progs.m.get(pid); if (!p) { this.setErr(this.shaders.m.has(pid) ? E.OP : E.VALUE); return -1; }
    const gl = this.gl;
    if (!gl.getProgramParameter(p.obj, gl.LINK_STATUS)) { this.setErr(E.OP); return -1; }
    if (p.uTab === null) this._buildUniforms(p);
    if (name.startsWith('gl_')) return -1;
    let e = p.uByName.get(name);
    if (e) return e.idx;
    const m = /^(.*)\[(\d+)\]$/.exec(name);
    if (m) { e = p.uByName.get(m[1]); if (e && e.isArr && +m[2] < e.size) return e.idx + (+m[2]) * 0x10000; }
    return -1;
  }
  _loc(id) {
    if (id < 0) return null;                                     // -1 is silently ignored
    const p = this.st.prog;
    if (p === null) { this.setErr(E.OP); return null; }
    let tab = p.uTab; if (tab === null) tab = this._buildUniforms(p);
    const e = tab[id & 0xFFFF], el = id >>> 16;
    if (e === undefined || el >= e.size) { this.setErr(E.OP); return null; }
    let l = e.locs[el];
    if (l === undefined) l = e.locs[el] = this.gl.getUniformLocation(p.obj, e.isArr ? e.name + '[' + el + ']' : e.name);
    return l;
  }
  uniform1f(l, x) { const o = this._loc(l); if (o) this.gl.uniform1f(o, x); }
  uniform2f(l, x, y) { const o = this._loc(l); if (o) this.gl.uniform2f(o, x, y); }
  uniform3f(l, x, y, z) { const o = this._loc(l); if (o) this.gl.uniform3f(o, x, y, z); }
  uniform4f(l, x, y, z, w) { const o = this._loc(l); if (o) this.gl.uniform4f(o, x, y, z, w); }
  uniform1i(l, x) { const o = this._loc(l); if (o) this.gl.uniform1i(o, x); }
  uniform2i(l, x, y) { const o = this._loc(l); if (o) this.gl.uniform2i(o, x, y); }
  uniform3i(l, x, y, z) { const o = this._loc(l); if (o) this.gl.uniform3i(o, x, y, z); }
  uniform4i(l, x, y, z, w) { const o = this._loc(l); if (o) this.gl.uniform4i(o, x, y, z, w); }
  uniformNfv(n, l, count, ptr) {
    if (count < 0) return this.setErr(E.VALUE);
    const o = this._loc(l); if (!o || count === 0) return;
    const c = count * n; this._fv(ptr, c);
    switch (n) {
      case 1: this.gl.uniform1fv(o, this.pv, this.po, c); break;
      case 2: this.gl.uniform2fv(o, this.pv, this.po, c); break;
      case 3: this.gl.uniform3fv(o, this.pv, this.po, c); break;
      default: this.gl.uniform4fv(o, this.pv, this.po, c);
    }
  }
  uniformNiv(n, l, count, ptr) {
    if (count < 0) return this.setErr(E.VALUE);
    const o = this._loc(l); if (!o || count === 0) return;
    const c = count * n; this._iv(ptr, c);
    switch (n) {
      case 1: this.gl.uniform1iv(o, this.pv, this.po, c); break;
      case 2: this.gl.uniform2iv(o, this.pv, this.po, c); break;
      case 3: this.gl.uniform3iv(o, this.pv, this.po, c); break;
      default: this.gl.uniform4iv(o, this.pv, this.po, c);
    }
  }
  uniformMatrix(n, l, count, transpose, ptr) {
    if (count < 0 || transpose) return this.setErr(E.VALUE);     // GLES2: transpose must be GL_FALSE
    const o = this._loc(l); if (!o || count === 0) return;
    const c = count * n * n; this._fv(ptr, c);
    switch (n) {
      case 2: this.gl.uniformMatrix2fv(o, false, this.pv, this.po, c); break;
      case 3: this.gl.uniformMatrix3fv(o, false, this.pv, this.po, c); break;
      default: this.gl.uniformMatrix4fv(o, false, this.pv, this.po, c);
    }
  }
  getUniform(pid, loc) {
    const p = this.progs.m.get(pid); if (!p) { this.setErr(E.VALUE); return null; }
    if (p.uTab === null) this._buildUniforms(p);
    const e = p.uTab[loc & 0xFFFF], el = loc >>> 16;
    if (loc < 0 || !e || el >= e.size) { this.setErr(E.OP); return null; }
    let l = e.locs[el]; if (l === undefined) l = e.locs[el] = this.gl.getUniformLocation(p.obj, e.isArr ? e.name + '[' + el + ']' : e.name);
    const v = this.gl.getUniform(p.obj, l);
    return typeof v === 'number' ? [v] : typeof v === 'boolean' ? [v ? 1 : 0] : Array.from(v, (x) => +x);
  }

  // ---- vertex attributes -----------------------------------------------------------------------------
  vertexAttribPointer(idx, size, type, norm, stride, ptr) {
    if (type === 0x140C) return this.setErr(E.ENUM);              // GL_FIXED is not supported
    if (type === 0x8D61) type = 0x140B;                           // HALF_FLOAT_OES
    if (this.st.arrayBuf === 0 && ptr !== 0) return this.setErr(E.OP);   // client-side arrays: unsupported
    this.gl.vertexAttribPointer(idx, size, type, !!norm, stride, ptr);
  }
  vertexAttribNf(idx, n, x, y, z, w) {
    const gl = this.gl, g = this.st.gen;
    if (idx >= this.grp.nAttribs) return this.setErr(E.VALUE);
    const o = idx * 4; g[o] = x; g[o + 1] = n > 1 ? y : 0; g[o + 2] = n > 2 ? z : 0; g[o + 3] = n > 3 ? w : 1;
    gl.vertexAttrib4f(idx, g[o], g[o + 1], g[o + 2], g[o + 3]);
  }
  vertexAttribNfv(idx, n, ptr) {
    const m = this.mem.win(ptr, n * 4), dv = m.dv, o = m.base + ptr;
    this.vertexAttribNf(idx, n, dv.getFloat32(o, true), n > 1 ? dv.getFloat32(o + 4, true) : 0, n > 2 ? dv.getFloat32(o + 8, true) : 0, n > 3 ? dv.getFloat32(o + 12, true) : 1);
  }
  getVertexAttrib(idx, pname) {
    const gl = this.gl;
    if (idx >= this.grp.nAttribs) { this.setErr(E.VALUE); return null; }
    const v = gl.getVertexAttrib(idx, pname);
    if (v === null || v === undefined) return null;
    if (pname === 0x889F) return [v ? v.$id : 0];
    if (typeof v === 'number') return [v];
    if (typeof v === 'boolean') return [v ? 1 : 0];
    return Array.from(v, (x) => +x);
  }

  // ---- draw calls ------------------------------------------------------------------------------------
  drawElements(mode, count, type, offset) {
    if (this.curVao.elem === 0) return this.setErr(E.OP);         // client-side index arrays: unsupported
    this.gl.drawElements(mode, count, type, offset);
  }
  drawElementsInstanced(mode, count, type, offset, n) {
    if (this.curVao.elem === 0) return this.setErr(E.OP);
    this.gl.drawElementsInstanced(mode, count, type, offset, n);
  }

  // ---- capabilities and simple state (shadowed so the context can be re-applied after a context switch) --
  enable(cap) { const i = capIdx(cap); if (i < 0) return this.setErr(E.ENUM); if (this.st.caps[i]) return; this.st.caps[i] = 1; this.gl.enable(cap); }
  disable(cap) { const i = capIdx(cap); if (i < 0) return this.setErr(E.ENUM); if (!this.st.caps[i]) return; this.st.caps[i] = 0; this.gl.disable(cap); }
  isEnabled(cap) { const i = capIdx(cap); if (i < 0) { this.setErr(E.ENUM); return 0; } return this.st.caps[i]; }
  viewport(x, y, w, h) { if (w < 0 || h < 0) return this.setErr(E.VALUE); const v = this.st.vp; v[0] = x; v[1] = y; v[2] = w; v[3] = h; this.gl.viewport(x, y, w, h); }
  scissor(x, y, w, h) { if (w < 0 || h < 0) return this.setErr(E.VALUE); const v = this.st.sc; v[0] = x; v[1] = y; v[2] = w; v[3] = h; this.gl.scissor(x, y, w, h); }
  clearColor(r, g, b, a) { const c = this.st.clearColor; c[0] = r; c[1] = g; c[2] = b; c[3] = a; this.gl.clearColor(r, g, b, a); }
  clearDepth(d) { this.st.clearDepth = d; this.gl.clearDepth(d); }
  clearStencil(s) { this.st.clearStencil = s; this.gl.clearStencil(s); }
  colorMask(r, g, b, a) { const c = this.st.colorMask; c[0] = r ? 1 : 0; c[1] = g ? 1 : 0; c[2] = b ? 1 : 0; c[3] = a ? 1 : 0; this._applyColorMask(); }
  depthMask(f) { this.st.depthMask = f ? 1 : 0; this.gl.depthMask(!!f); }
  depthFunc(f) { this.st.depthFunc = f; this.gl.depthFunc(f); }
  depthRange(n, f) { this.st.depthRange[0] = n; this.st.depthRange[1] = f; this.gl.depthRange(n, f); }
  stencilFuncSeparate(face, func, ref, mask) {
    const s = this.st.sFunc;
    if (face === 0x404 || face === 0x408) { s[0] = func; s[1] = ref; s[2] = mask >>> 0; }
    if (face === 0x405 || face === 0x408) { s[3] = func; s[4] = ref; s[5] = mask >>> 0; }
    this.gl.stencilFuncSeparate(face, func, ref, mask);
  }
  stencilOpSeparate(face, fail, zfail, zpass) {
    const s = this.st.sOp;
    if (face === 0x404 || face === 0x408) { s[0] = fail; s[1] = zfail; s[2] = zpass; }
    if (face === 0x405 || face === 0x408) { s[3] = fail; s[4] = zfail; s[5] = zpass; }
    this.gl.stencilOpSeparate(face, fail, zfail, zpass);
  }
  stencilMaskSeparate(face, mask) {
    const s = this.st.sMask;
    if (face === 0x404 || face === 0x408) s[0] = mask >>> 0;
    if (face === 0x405 || face === 0x408) s[1] = mask >>> 0;
    this.gl.stencilMaskSeparate(face, mask);
  }
  blendColor(r, g, b, a) { const c = this.st.blendColor; c[0] = r; c[1] = g; c[2] = b; c[3] = a; this.gl.blendColor(r, g, b, a); }
  blendEquationSeparate(rgb, a) { this.st.blendEq[0] = rgb; this.st.blendEq[1] = a; this.gl.blendEquationSeparate(rgb, a); }
  blendFuncSeparate(a, b, c, d) { const f = this.st.blendFunc; f[0] = a; f[1] = b; f[2] = c; f[3] = d; this.gl.blendFuncSeparate(a, b, c, d); }
  cullFace(m) { this.st.cullFace = m; this.gl.cullFace(m); }
  frontFace(m) { this.st.frontFace = m; this.gl.frontFace(m); }
  lineWidth(w) { this.st.lineWidth = w; this.gl.lineWidth(w); }
  polygonOffset(f, u) { this.st.poly[0] = f; this.st.poly[1] = u; this.gl.polygonOffset(f, u); }
  sampleCoverage(v, inv) { this.st.cov[0] = v; this.st.cov[1] = inv ? 1 : 0; this.gl.sampleCoverage(v, !!inv); }
  hint(target, mode) {
    if (target === 0x8192) this.st.hintMip = mode; else if (target === 0x8B8B) this.st.hintDeriv = mode; else return this.setErr(E.ENUM);
    this.gl.hint(target, mode);
  }
  pixelStorei(pname, param) {
    if (pname === 0xCF5) this.st.unpackAlign = param; else if (pname === 0xD05) this.st.packAlign = param; else return this.setErr(E.ENUM);
    this.gl.pixelStorei(pname, param);
  }
  drawBuffers(list) {
    if (this.st.drawFbo === 0) {                       // default framebuffer: GL_BACK / GL_NONE (it is a private FBO here)
      if (list.length !== 1 || (list[0] !== 0x0405 && list[0] !== 0)) return this.setErr(E.OP);
      list = [list[0] === 0 ? 0 : 0x8CE0];
    }
    this.gl.drawBuffers(list);
  }

  // ---- textures --------------------------------------------------------------------------------------
  texImage2D(target, level, ifmt, w, h, border, fmt, type, ptr) {
    const gl = this.gl;
    if (w < 0 || h < 0) return this.setErr(E.VALUE);
    const t = mapTexFormat(ifmt, fmt, type), bgra = t.bgra, tf = t.fmt, tt = t.type, ti = t.ifmt, exp = t.expand;
    if (ptr === 0) return gl.texImage2D(target, level, ti, w, h, border, tf, tt, null);
    if (bgra) { this._bgraIn(ptr, w, h); }
    else if (exp) this._expandLA(w, h, type, ptr, exp);
    else this._pix(type, ptr, imageBytes(w, h, fmt, type, this.st.unpackAlign));
    gl.texImage2D(target, level, ti, w, h, border, tf, tt, this.pv, this.po);
  }
  texSubImage2D(target, level, x, y, w, h, fmt, type, ptr) {
    const gl = this.gl;
    if (w < 0 || h < 0) return this.setErr(E.VALUE);
    if (ptr === 0) return this.setErr(E.VALUE);
    const t = mapTexFormat(fmt, fmt, type), bgra = t.bgra, tf = t.fmt, tt = t.type, exp = t.expand;
    if (bgra) { this._bgraIn(ptr, w, h); }
    else if (exp) this._expandLA(w, h, type, ptr, exp);
    else this._pix(type, ptr, imageBytes(w, h, fmt, type, this.st.unpackAlign));
    gl.texSubImage2D(target, level, x, y, w, h, tf, tt, this.pv, this.po);
  }
  compressedTexImage2D(target, level, ifmt, w, h, border, size, ptr) {
    if (size < 0 || w < 0 || h < 0) return this.setErr(E.VALUE);
    const m = this.mem.win(ptr, size); this.gl.compressedTexImage2D(target, level, ifmt, w, h, border, m.u8, m.base + ptr, size);
  }
  compressedTexSubImage2D(target, level, x, y, w, h, fmt, size, ptr) {
    if (size < 0 || w < 0 || h < 0) return this.setErr(E.VALUE);
    const m = this.mem.win(ptr, size); this.gl.compressedTexSubImage2D(target, level, x, y, w, h, fmt, m.u8, m.base + ptr, size);
  }
  copyTexImage2D(target, level, ifmt, x, y, w, h, border) {
    if (ifmt === 0x80E1 || ifmt === 0x93A1) ifmt = 0x1908;
    const r = this._readBegin();
    this.gl.copyTexImage2D(target, level, ifmt, x, y, w, h, border);
    this._readEnd(r);
  }
  copyTexSubImage2D(target, level, xo, yo, x, y, w, h) {
    const r = this._readBegin();
    this.gl.copyTexSubImage2D(target, level, xo, yo, x, y, w, h);
    this._readEnd(r);
  }
  blitFramebuffer(sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1, mask, filter) {
    const r = this._readBegin();
    this.gl.blitFramebuffer(sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1, mask, filter);
    this._readEnd(r);
  }
  // GLES2 lets a client set/get parameters of the default texture (name 0) when nothing is bound; WebGL has no default texture and
  // raises INVALID_OPERATION ("no texture bound"), which would also pollute the guest's error state. The default texture is never
  // complete here (no image), so sampling it gives black either way: dropping the call loses nothing observable.
  _noTex(target) { return !(target === 0xDE1 ? this.st.tex2d : this.st.texCube)[this.st.unit]; }
  texParameter(target, pname, v) {
    if (target !== 0xDE1 && target !== 0x8513) return this.setErr(E.ENUM);
    if (this._noTex(target)) return;
    this.gl.texParameteri(target, pname, v);
  }
  texParameterf(target, pname, v) {
    if (target !== 0xDE1 && target !== 0x8513) return this.setErr(E.ENUM);
    if (this._noTex(target)) return;
    this.gl.texParameterf(target, pname, v);
  }
  getTexParameter(target, pname) {
    if (target !== 0xDE1 && target !== 0x8513) { this.setErr(E.ENUM); return null; }
    if (this._noTex(target)) return null;
    const v = this.gl.getTexParameter(target, pname);
    return v === null || v === undefined ? null : [+v];
  }
  readPixels(x, y, w, h, fmt, type, ptr) {
    if (w < 0 || h < 0) return this.setErr(E.VALUE);
    if (w === 0 || h === 0) return;
    const r = this._readBegin();
    this._readPixels(x, y, w, h, fmt, type, ptr);
    this._readEnd(r);
  }
  _readPixels(x, y, w, h, fmt, type, ptr) {
    const gl = this.gl;
    const align = this.st.packAlign;
    if (fmt === 0x80E1) {                                   // BGRA: read RGBA, swizzle into guest memory
      const { stride, total, m } = this._swizzle(w, h, align, ptr, true);
      gl.readPixels(x, y, w, h, 0x1908, type, this._sw, 0);
      const dst = m.u8, base = m.base + ptr, src = this._sw;
      for (let r = 0; r < h; r++) {
        let i = r * stride, j = base + r * stride;
        for (let c = 0; c < w; c++, i += 4, j += 4) { dst[j] = src[i + 2]; dst[j + 1] = src[i + 1]; dst[j + 2] = src[i]; dst[j + 3] = src[i + 3]; }
      }
      return;
    }
    const nbytes = imageBytes(w, h, fmt, type, align), m = this.mem.winW(ptr, nbytes), abs = m.base + ptr;
    switch (type) {
      case 0x1401: gl.readPixels(x, y, w, h, fmt, type, m.u8, abs); return;
      case 0x1406: if ((abs & 3) === 0) { gl.readPixels(x, y, w, h, fmt, type, m.f32, abs >>> 2); return; } break;
      case 0x1405: if ((abs & 3) === 0) { gl.readPixels(x, y, w, h, fmt, type, m.u32, abs >>> 2); return; } break;
      default: if ((abs & 1) === 0) { gl.readPixels(x, y, w, h, fmt, type, m.u16, abs >>> 1); return; }
    }
    const b = new ArrayBuffer((nbytes + 3) & ~3);           // unaligned destination: read aside, then copy
    const v = type === 0x1406 ? new Float32Array(b) : type === 0x1405 ? new Uint32Array(b) : type === 0x1401 ? new Uint8Array(b) : new Uint16Array(b);
    gl.readPixels(x, y, w, h, fmt, type, v, 0);
    m.u8.set(new Uint8Array(b, 0, nbytes), abs);
  }

  // ---- framebuffers / renderbuffers ------------------------------------------------------------------
  framebufferTexture2D(target, attachment, textarget, id, level) {
    const st = this.st;
    if ((target === 0x8CA8 ? st.readFbo : st.drawFbo) === 0) return this.setErr(E.OP);
    const e = id ? this.texs.m.get(id) : null;
    if (id && !e) return this.setErr(E.OP);
    this.gl.framebufferTexture2D(target, attachment, textarget, e ? e.obj : null, level);
  }
  framebufferRenderbuffer(target, attachment, rbtarget, id) {
    const st = this.st;
    if ((target === 0x8CA8 ? st.readFbo : st.drawFbo) === 0) return this.setErr(E.OP);
    const e = id ? this.rbos.m.get(id) : null;
    if (id && !e) return this.setErr(E.OP);
    this.gl.framebufferRenderbuffer(target, attachment, rbtarget, e ? e.obj : null);
  }
  renderbufferStorage(target, ifmt, w, h) {
    if (w < 0 || h < 0) return this.setErr(E.VALUE);
    const e = this.rbos.m.get(this.st.rbo);
    if (e) { e.fmt = ifmt; e.w = w; e.h = h; }
    this.gl.renderbufferStorage(target, mapRbFormat(ifmt), w, h);
  }
  renderbufferStorageMultisample(target, samples, ifmt, w, h) {
    if (w < 0 || h < 0 || samples < 0) return this.setErr(E.VALUE);
    const e = this.rbos.m.get(this.st.rbo);
    if (e) { e.fmt = ifmt; e.w = w; e.h = h; }
    this.gl.renderbufferStorageMultisample(target, Math.min(samples, this.grp.maxSamples), mapRbFormat(ifmt), w, h);
  }
  getRenderbufferParameter(target, pname) {
    if (target !== 0x8D41) { this.setErr(E.ENUM); return null; }
    if (pname === 0x8D44) { const e = this.rbos.m.get(this.st.rbo); if (e && e.fmt) return [e.fmt]; }
    const v = this.gl.getRenderbufferParameter(target, pname);
    return v === null || v === undefined ? null : [+v];
  }
  getFramebufferAttachmentParameter(target, attachment, pname) {
    const v = this.gl.getFramebufferAttachmentParameter(target, attachment, pname);
    if (v === null || v === undefined) return null;
    if (typeof v === 'object') return [v.$id || 0];
    return [+v];
  }
  getBufferParameter(target, pname) {
    if (target !== 0x8892 && target !== 0x8893) { this.setErr(E.ENUM); return null; }
    const v = this.gl.getBufferParameter(target, pname);
    return v === null || v === undefined ? null : [+v];
  }

  // ---- queries (EXT_occlusion_query_boolean) ---------------------------------------------------------
  beginQuery(target, id) {
    if (target !== 0x8C2F && target !== 0x8D6A) return this.setErr(E.ENUM);
    if (id === 0) return this.setErr(E.VALUE);
    const e = this._get(this.queries, id, 5);
    e.target = target; e.pending = true; e.result = null;
    this.activeQ = this.activeQ || new Map(); this.activeQ.set(target, e);
    this.gl.beginQuery(target, e.obj);
  }
  endQuery(target) {
    if (target !== 0x8C2F && target !== 0x8D6A) return this.setErr(E.ENUM);
    if (!this.activeQ || !this.activeQ.has(target)) return this.setErr(E.OP);
    this.activeQ.delete(target);
    this.gl.endQuery(target);
  }
  getQuery(target, pname) {
    if (target !== 0x8C2F && target !== 0x8D6A) { this.setErr(E.ENUM); return null; }
    if (pname === 0x8865) { const e = this.activeQ && this.activeQ.get(target); return e ? e.id : 0; }
    if (pname === 0x8864) return 32;                 // QUERY_COUNTER_BITS
    this.setErr(E.ENUM); return null;
  }
  getQueryObject(id, pname) {
    const e = this.queries.m.get(id), gl = this.gl;
    if (!e || !e.used || (this.activeQ && this.activeQ.get(e.target) === e)) { this.setErr(E.OP); return null; }
    if (pname === 0x8867) return gl.getQueryParameter(e.obj, gl.QUERY_RESULT_AVAILABLE) ? 1 : 0;
    if (pname === 0x8866) {
      const r = gl.getQueryParameter(e.obj, gl.QUERY_RESULT_AVAILABLE) ? gl.getQueryParameter(e.obj, gl.QUERY_RESULT) : null;
      if (r !== null) e.result = r ? 1 : 0;
      return e.result === null || e.result === undefined ? 0 : e.result;     // not yet available: report 0 (poll AVAILABLE first)
    }
    this.setErr(E.ENUM); return null;
  }

  // ---- state queries ---------------------------------------------------------------------------------
  /** GetIntegerv/GetFloatv/GetBooleanv backend: returns an array of numbers (or a number), or null (error recorded). */
  getParam(pname) {
    const st = this.st, gl = this.gl;
    switch (pname) {
      case 0x8894: return st.arrayBuf;
      case 0x8895: return this.curVao.elem;
      case 0x8CA6: return st.drawFbo;               // FRAMEBUFFER_BINDING == DRAW_FRAMEBUFFER_BINDING
      case 0x8CAA: return st.readFbo;
      case 0x8CA7: return st.rbo;
      case 0x8069: return st.tex2d[st.unit];
      case 0x8514: return st.texCube[st.unit];
      case 0x8B8D: return st.prog ? st.prog.id : 0;
      case 0x85B5: return this.curVao.id;
      case 0x84E0: return 0x84C0 + st.unit;
      case 0x86A2: return gl.getParameter(0x86A3).length;
      case 0x8DF9: return 0;                        // NUM_SHADER_BINARY_FORMATS
      case 0x8DF8: return [];                       // SHADER_BINARY_FORMATS
      case 0x8DFA: return 1;                        // SHADER_COMPILER
      case 0x8CDF: return gl.getParameter(gl.MAX_COLOR_ATTACHMENTS);
      case 0x8824: return this.grp.maxDrawBuffers;
      case 0xB45: return st.cullFace;
    }
    let v;
    try { v = gl.getParameter(pname); } catch (e) { v = null; }
    if (v === null || v === undefined) return null;
    if (typeof v === 'number') return v;
    if (typeof v === 'boolean') return v ? 1 : 0;
    if (typeof v === 'string') { this.setErr(E.ENUM); return null; }
    if (typeof v === 'object' && v.length !== undefined) return Array.from(v, (x) => +x);
    if (typeof v === 'object' && '$id' in v) return v.$id;
    this.setErr(E.ENUM); return null;
  }
  getString(name) {
    let s;
    switch (name) {
      case 0x1F00: s = this.grp.vendor || 'flash-runner'; break;
      case 0x1F01: s = this.grp.renderer || 'flash-runner WebGL2'; break;
      case 0x1F02: s = 'OpenGL ES 2.0 flash-runner (WebGL2)'; break;
      case 0x8B8C: s = 'OpenGL ES GLSL ES 1.0 flash-runner (WebGL2)'; break;
      case 0x1F03: s = this.grp.extString; break;
      default: this.setErr(E.ENUM); return 0;
    }
    let p = this.strPtr.get(name);
    if (!p) { p = this.mem.allocCStr(s); this.strPtr.set(name, p); }
    return p;
  }
  checkFramebufferStatus(target) {
    if (target !== 0x8D40 && target !== 0x8CA8 && target !== 0x8CA9) { this.setErr(E.ENUM); return 0; }
    return this.gl.checkFramebufferStatus(target);
  }

  // ---- CHROMIUM_map_sub / texture mapping: staging memory in the guest arena, uploaded on unmap --------
  // The object (and target/level/rect/alignment) is recorded when it is mapped: Unmap uploads into THAT object whatever is bound
  // by then, and leaves the caller's bindings as they were.
  mapBufferSubData(target, offset, size, access) {
    if (size < 0 || offset < 0) { this.setErr(E.VALUE); return 0; }
    if (target !== 0x8892 && target !== 0x8893) { this.setErr(E.ENUM); return 0; }
    const id = target === 0x8892 ? this.st.arrayBuf : this.curVao.elem, buf = id ? this.bufs.m.get(id) : null;
    if (!buf) { this.setErr(E.OP); return 0; }
    let ptr = 0;
    try { ptr = this.mem.alloc(Math.max(size, 16)); } catch (e) { this.setErr(E.OOM); return 0; }
    if (access === 0x88B8 || access === 0x88BA) {          // READ_ONLY / READ_WRITE: fetch the current contents
      const m = this.mem.winW(ptr, size);
      this.gl.bindBuffer(0x8F36, buf.obj);                  // COPY_READ_BUFFER: not a GLES2 binding point, so nothing to restore
      this.gl.getBufferSubData(0x8F36, offset, m.u8, m.base + ptr, size);
      this.gl.bindBuffer(0x8F36, null);
    }
    this.maps.set(ptr, { kind: 0, ptr, buf, offset, size, access });
    return ptr;
  }
  mapTexSubImage2D(target, level, x, y, w, h, fmt, type, access) {
    if (w < 0 || h < 0) { this.setErr(E.VALUE); return 0; }
    const bt = target === 0xDE1 ? 0xDE1 : (target >= 0x8515 && target <= 0x851A) ? 0x8513 : 0;
    if (!bt) { this.setErr(E.ENUM); return 0; }
    const st = this.st, id = (bt === 0xDE1 ? st.tex2d : st.texCube)[st.unit], tex = id ? this.texs.m.get(id) : null;
    if (!tex) { this.setErr(E.OP); return 0; }
    const n = imageBytes(w, h, fmt, type, st.unpackAlign);
    let ptr = 0;
    try { ptr = this.mem.alloc(Math.max(n, 16)); } catch (e) { this.setErr(E.OOM); return 0; }
    if ((access === 0x88B8 || access === 0x88BA) && n > 0) {
      const m = this.mem.winW(ptr, n);
      m.u8.fill(0, m.base + ptr, m.base + ptr + n);
      this._readTexture(tex, target, level, x, y, w, h, fmt, type, m.base + ptr, m);
    }
    this.maps.set(ptr, { kind: 1, ptr, tex, bt, target, level, x, y, w, h, fmt, type, access, align: st.unpackAlign });
    return ptr;
  }
  _readTexture(tex, target, level, x, y, w, h, fmt, type, absDst, m) {
    // Read back through a temporary FBO (works for colour-renderable textures; others read as zeros).
    const gl = this.gl, st = this.st;
    const fbo = gl.createFramebuffer(), prev = this._fboObj(st.readFbo);
    gl.bindFramebuffer(0x8CA8, fbo);
    gl.framebufferTexture2D(0x8CA8, gl.COLOR_ATTACHMENT0, target, tex.obj, level);
    if (gl.checkFramebufferStatus(0x8CA8) === gl.FRAMEBUFFER_COMPLETE) {
      gl.pixelStorei(0xD05, st.unpackAlign);
      gl.readPixels(x, y, w, h, 0x1908, type, m.u8, absDst);
      gl.pixelStorei(0xD05, st.packAlign);
      if (fmt === 0x80E1 && type === 0x1401) {            // BGRA mapping: hand the data over in BGRA order
        const stride = Math.ceil((w * 4) / st.unpackAlign) * st.unpackAlign, u = m.u8;
        for (let r = 0; r < h; r++) for (let c = 0, o = absDst + r * stride; c < w; c++, o += 4) { const t = u[o]; u[o] = u[o + 2]; u[o + 2] = t; }
      }
    }
    gl.bindFramebuffer(0x8CA8, prev); gl.deleteFramebuffer(fbo);
    gl.getError();
  }
  unmapSub(ptr, kind) {
    const mp = this.maps.get(ptr);
    if (!mp || mp.kind !== kind) return this.setErr(E.OP);
    this.maps.delete(ptr);
    const gl = this.gl;
    if (kind === 0) {
      if (mp.access !== 0x88B8 && this.bufs.m.get(mp.buf.id) === mp.buf) {          // (a deleted buffer: nothing to upload to)
        gl.bindBuffer(0x8F37, mp.buf.obj);                                           // COPY_WRITE_BUFFER: leaves ARRAY/ELEMENT bindings alone
        const m = this.mem.win(ptr, mp.size);
        gl.bufferSubData(0x8F37, mp.offset, m.u8, m.base + ptr, mp.size);
        gl.bindBuffer(0x8F37, null);
      }
    } else if (mp.access !== 0x88B8 && this.texs.m.get(mp.tex.id) === mp.tex) {
      const st = this.st, arr = mp.bt === 0xDE1 ? st.tex2d : st.texCube, prevId = arr[st.unit], swap = prevId !== mp.tex.id, al = st.unpackAlign;
      if (swap) gl.bindTexture(mp.bt, mp.tex.obj);
      if (al !== mp.align) { st.unpackAlign = mp.align; gl.pixelStorei(0xCF5, mp.align); }
      this.texSubImage2D(mp.target, mp.level, mp.x, mp.y, mp.w, mp.h, mp.fmt, mp.type, ptr);
      if (al !== mp.align) { st.unpackAlign = al; gl.pixelStorei(0xCF5, al); }
      if (swap) gl.bindTexture(mp.bt, prevId ? (this.texs.m.get(prevId)?.obj ?? null) : null);
    }
    this.mem.release(ptr);
  }
  enableFeature(feature) {
    switch (feature) {
      case 'pepper3d_allow_buffers_on_multiple_targets': return 1;   // WebGL2 has no restriction
      case 'pepper3d_support_fixed_attribs': return 0;               // GL_FIXED is not available in WebGL2
    }
    return 0;
  }
}
