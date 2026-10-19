// PPB_OpenGLES2;1.0 (+ InstancedArrays, FramebufferBlit, FramebufferMultisample, ChromiumEnableFeature, Query,
// VertexArrayObject, DrawBuffers(Dev), ChromiumMapSub, GLESChromiumTextureMapping(Dev)) and PPB_Graphics3D;1.0
// on top of WebGL2. `buildGLES2(host)` returns the handler tables merged into the Pepper host by ppb-impl.js.
//
// The handlers here only decode guest arguments (pointers, signed conversions) and forward to GLContext
// (gles2-context.js), which owns the GLES object-name tables, uniform-location tables, sticky error flag and
// the GLES->WebGL2 format translation. All handlers start with `ctxOf(context)`: a single cached Map lookup that
// also makes the context current in its share group.
//
// Limitations (deliberate, shared with Pepper's own GLES2 command buffer):
//  * Client-side vertex/index arrays are NOT supported: VertexAttribPointer must source from an ARRAY_BUFFER and
//    DrawElements from an ELEMENT_ARRAY_BUFFER (otherwise GL_INVALID_OPERATION).
//  * GL_FIXED attributes and ShaderBinary are not available in WebGL2.

import { buildGraphics3D } from './ppb-graphics3d.js';

export function buildGLES2(host) {
  const { impl: g3d, ctxOf, mem: M } = buildGraphics3D(host);

  // ---- output helpers (values -> guest memory) --------------------------------------------------------
  function outInts(p, v) {
    if (typeof v === 'number') { M.wrI32(p, Math.round(v)); return; }
    for (let i = 0; i < v.length; i++) M.wrI32(p + i * 4, Math.round(v[i]));
  }
  function outFloats(p, v) {
    if (typeof v === 'number') { M.wrF32(p, v); return; }
    for (let i = 0; i < v.length; i++) M.wrF32(p + i * 4, v[i]);
  }
  function outBools(p, v) {
    if (typeof v === 'number') { M.wrU8(p, v ? 1 : 0); return; }
    for (let i = 0; i < v.length; i++) M.wrU8(p + i, v[i] ? 1 : 0);
  }
  const f32at = (p) => M.rdF32(p);
  const i32at = (p) => M.rdI32(p);

  const gles = {
    // ---- state ----------------------------------------------------------------------------------------
    ActiveTexture(tid, c, t) { const g = ctxOf(c); if (g) g.activeTexture(t); },
    BlendColor(tid, c, r, gr, b, a) { const g = ctxOf(c); if (g) g.blendColor(r, gr, b, a); },
    BlendEquation(tid, c, m) { const g = ctxOf(c); if (g) g.blendEquationSeparate(m, m); },
    BlendEquationSeparate(tid, c, m, a) { const g = ctxOf(c); if (g) g.blendEquationSeparate(m, a); },
    BlendFunc(tid, c, s, d) { const g = ctxOf(c); if (g) g.blendFuncSeparate(s, d, s, d); },
    BlendFuncSeparate(tid, c, a, b, d, e) { const g = ctxOf(c); if (g) g.blendFuncSeparate(a, b, d, e); },
    Clear(tid, c, mask) { const g = ctxOf(c); if (g) g.gl.clear(mask); },
    ClearColor(tid, c, r, gr, b, a) { const g = ctxOf(c); if (g) g.clearColor(r, gr, b, a); },
    ClearDepthf(tid, c, d) { const g = ctxOf(c); if (g) g.clearDepth(d); },
    ClearStencil(tid, c, s) { const g = ctxOf(c); if (g) g.clearStencil(s | 0); },
    ColorMask(tid, c, r, gr, b, a) { const g = ctxOf(c); if (g) g.colorMask(r & 0xff, gr & 0xff, b & 0xff, a & 0xff); },
    CullFace(tid, c, m) { const g = ctxOf(c); if (g) g.cullFace(m); },
    DepthFunc(tid, c, f) { const g = ctxOf(c); if (g) g.depthFunc(f); },
    DepthMask(tid, c, f) { const g = ctxOf(c); if (g) g.depthMask(f & 0xff); },
    DepthRangef(tid, c, n, f) { const g = ctxOf(c); if (g) g.depthRange(n, f); },
    Disable(tid, c, cap) { const g = ctxOf(c); if (g) g.disable(cap); },
    Enable(tid, c, cap) { const g = ctxOf(c); if (g) g.enable(cap); },
    IsEnabled(tid, c, cap) { const g = ctxOf(c); return g ? g.isEnabled(cap) : 0; },
    FrontFace(tid, c, m) { const g = ctxOf(c); if (g) g.frontFace(m); },
    Hint(tid, c, t, m) { const g = ctxOf(c); if (g) g.hint(t, m); },
    LineWidth(tid, c, w) { const g = ctxOf(c); if (g) g.lineWidth(w); },
    PixelStorei(tid, c, p, v) { const g = ctxOf(c); if (g) g.pixelStorei(p, v | 0); },
    PolygonOffset(tid, c, f, u) { const g = ctxOf(c); if (g) g.polygonOffset(f, u); },
    SampleCoverage(tid, c, v, inv) { const g = ctxOf(c); if (g) g.sampleCoverage(v, inv & 0xff); },
    Scissor(tid, c, x, y, w, h) { const g = ctxOf(c); if (g) g.scissor(x | 0, y | 0, w | 0, h | 0); },
    Viewport(tid, c, x, y, w, h) { const g = ctxOf(c); if (g) g.viewport(x | 0, y | 0, w | 0, h | 0); },
    StencilFunc(tid, c, f, r, m) { const g = ctxOf(c); if (g) g.stencilFuncSeparate(0x408, f, r | 0, m); },
    StencilFuncSeparate(tid, c, face, f, r, m) { const g = ctxOf(c); if (g) g.stencilFuncSeparate(face, f, r | 0, m); },
    StencilMask(tid, c, m) { const g = ctxOf(c); if (g) g.stencilMaskSeparate(0x408, m); },
    StencilMaskSeparate(tid, c, face, m) { const g = ctxOf(c); if (g) g.stencilMaskSeparate(face, m); },
    StencilOp(tid, c, a, b, d) { const g = ctxOf(c); if (g) g.stencilOpSeparate(0x408, a, b, d); },
    StencilOpSeparate(tid, c, face, a, b, d) { const g = ctxOf(c); if (g) g.stencilOpSeparate(face, a, b, d); },
    Flush(tid, c) { const g = ctxOf(c); if (g) g.gl.flush(); },
    Finish(tid, c) { const g = ctxOf(c); if (g) g.gl.finish(); },
    GetError(tid, c) { const g = ctxOf(c); return g ? g.getError() : 0; },

    // ---- object names ---------------------------------------------------------------------------------
    GenBuffers(tid, c, n, p) { const g = ctxOf(c); if (g) g.genBuffers(n | 0, p); },
    GenTextures(tid, c, n, p) { const g = ctxOf(c); if (g) g.genTextures(n | 0, p); },
    GenFramebuffers(tid, c, n, p) { const g = ctxOf(c); if (g) g.genFramebuffers(n | 0, p); },
    GenRenderbuffers(tid, c, n, p) { const g = ctxOf(c); if (g) g.genRenderbuffers(n | 0, p); },
    DeleteBuffers(tid, c, n, p) { const g = ctxOf(c); if (g) g.deleteBuffers(n | 0, p); },
    DeleteTextures(tid, c, n, p) { const g = ctxOf(c); if (g) g.deleteTextures(n | 0, p); },
    DeleteFramebuffers(tid, c, n, p) { const g = ctxOf(c); if (g) g.deleteFramebuffers(n | 0, p); },
    DeleteRenderbuffers(tid, c, n, p) { const g = ctxOf(c); if (g) g.deleteRenderbuffers(n | 0, p); },
    IsBuffer(tid, c, id) { const g = ctxOf(c); return g ? g.isBuffer(id) : 0; },
    IsTexture(tid, c, id) { const g = ctxOf(c); return g ? g.isTexture(id) : 0; },
    IsFramebuffer(tid, c, id) { const g = ctxOf(c); return g ? g.isFramebuffer(id) : 0; },
    IsRenderbuffer(tid, c, id) { const g = ctxOf(c); return g ? g.isRenderbuffer(id) : 0; },
    IsProgram(tid, c, id) { const g = ctxOf(c); return g ? g.isProgram(id) : 0; },
    IsShader(tid, c, id) { const g = ctxOf(c); return g ? g.isShader(id) : 0; },
    BindBuffer(tid, c, t, id) { const g = ctxOf(c); if (g) g.bindBuffer(t, id); },
    BindTexture(tid, c, t, id) { const g = ctxOf(c); if (g) g.bindTexture(t, id); },
    BindFramebuffer(tid, c, t, id) { const g = ctxOf(c); if (g) g.bindFramebuffer(t, id); },
    BindRenderbuffer(tid, c, t, id) { const g = ctxOf(c); if (g) g.bindRenderbuffer(t, id); },

    // ---- buffers --------------------------------------------------------------------------------------
    BufferData(tid, c, t, size, data, usage) { const g = ctxOf(c); if (g) g.bufferData(t, size | 0, data, usage); },
    BufferSubData(tid, c, t, off, size, data) { const g = ctxOf(c); if (g) g.bufferSubData(t, off | 0, size | 0, data); },
    GetBufferParameteriv(tid, c, t, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getBufferParameter(t, pn); if (v) outInts(p, v); },

    // ---- shaders and programs -------------------------------------------------------------------------
    CreateShader(tid, c, type) { const g = ctxOf(c); return g ? g.createShader(type) : 0; },
    CreateProgram(tid, c) { const g = ctxOf(c); return g ? g.createProgram() : 0; },
    DeleteShader(tid, c, s) { const g = ctxOf(c); if (g) g.deleteShader(s); },
    DeleteProgram(tid, c, p) { const g = ctxOf(c); if (g) g.deleteProgram(p); },
    AttachShader(tid, c, p, s) { const g = ctxOf(c); if (g) g.attachShader(p, s); },
    DetachShader(tid, c, p, s) { const g = ctxOf(c); if (g) g.detachShader(p, s); },
    ShaderSource(tid, c, s, count, str, len) { const g = ctxOf(c); if (g) g.shaderSource(s, count | 0, str, len); },
    CompileShader(tid, c, s) { const g = ctxOf(c); if (g) g.compileShader(s); },
    ReleaseShaderCompiler(tid, c) { },
    ShaderBinary(tid, c) { const g = ctxOf(c); if (g) g.setErr(0x500); },       // no binary formats are advertised
    LinkProgram(tid, c, p) { const g = ctxOf(c); if (g) g.linkProgram(p); },
    UseProgram(tid, c, p) { const g = ctxOf(c); if (g) g.useProgram(p); },
    ValidateProgram(tid, c, p) { const g = ctxOf(c); if (g) g.validateProgram(p); },
    BindAttribLocation(tid, c, p, idx, name) { const g = ctxOf(c); if (g) g.bindAttribLocation(p, idx, M.cstr(name)); },
    GetAttribLocation(tid, c, p, name) { const g = ctxOf(c); return g ? g.getAttribLocation(p, M.cstr(name)) : -1; },
    GetUniformLocation(tid, c, p, name) { const g = ctxOf(c); return g ? g.getUniformLocation(p, M.cstr(name)) : -1; },
    GetShaderiv(tid, c, s, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getShaderiv(s, pn); if (v !== null) M.wrI32(p, v); },
    GetProgramiv(tid, c, pr, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getProgramiv(pr, pn); if (v !== null) M.wrI32(p, v); },
    GetShaderInfoLog(tid, c, s, bufsize, len, log) { const g = ctxOf(c); if (!g) return; const v = g.getShaderInfoLog(s); if (v !== null) M.wrStr(log, bufsize | 0, v, len); },
    GetProgramInfoLog(tid, c, pr, bufsize, len, log) { const g = ctxOf(c); if (!g) return; const v = g.getProgramInfoLog(pr); if (v !== null) M.wrStr(log, bufsize | 0, v, len); },
    GetShaderSource(tid, c, s, bufsize, len, src) { const g = ctxOf(c); if (!g) return; const v = g.getShaderSource(s); if (v !== null) M.wrStr(src, bufsize | 0, v, len); },
    GetShaderPrecisionFormat(tid, c, st, pt, range, prec) {
      const g = ctxOf(c); if (!g) return;
      const r = g.getShaderPrecisionFormat(st, pt); if (!r) return;
      M.wrI32(range, r.rangeMin); M.wrI32(range + 4, r.rangeMax); M.wrI32(prec, r.precision);
    },
    GetAttachedShaders(tid, c, p, max, count, shaders) {
      const g = ctxOf(c); if (!g) return;
      const l = g.getAttachedShaders(p); if (!l) return;
      const n = Math.min(l.length, max | 0);
      for (let i = 0; i < n; i++) M.wrU32(shaders + i * 4, l[i]);
      if (count) M.wrI32(count, n);
    },
    GetActiveAttrib(tid, c, p, idx, bufsize, len, size, type, name) {
      const g = ctxOf(c); if (!g) return;
      const a = g.getActive(p, idx, false); if (!a) return;
      if (size) M.wrI32(size, a.size); if (type) M.wrU32(type, a.type);
      if (name) M.wrStr(name, bufsize | 0, a.name, len); else if (len) M.wrI32(len, 0);
    },
    GetActiveUniform(tid, c, p, idx, bufsize, len, size, type, name) {
      const g = ctxOf(c); if (!g) return;
      const a = g.getActive(p, idx, true); if (!a) return;
      if (size) M.wrI32(size, a.size); if (type) M.wrU32(type, a.type);
      if (name) M.wrStr(name, bufsize | 0, a.name, len); else if (len) M.wrI32(len, 0);
    },
    GetUniformfv(tid, c, p, loc, out) { const g = ctxOf(c); if (!g) return; const v = g.getUniform(p, loc | 0); if (v) outFloats(out, v); },
    GetUniformiv(tid, c, p, loc, out) { const g = ctxOf(c); if (!g) return; const v = g.getUniform(p, loc | 0); if (v) outInts(out, v); },
    Uniform1f(tid, c, l, x) { const g = ctxOf(c); if (g) g.uniform1f(l | 0, x); },
    Uniform2f(tid, c, l, x, y) { const g = ctxOf(c); if (g) g.uniform2f(l | 0, x, y); },
    Uniform3f(tid, c, l, x, y, z) { const g = ctxOf(c); if (g) g.uniform3f(l | 0, x, y, z); },
    Uniform4f(tid, c, l, x, y, z, w) { const g = ctxOf(c); if (g) g.uniform4f(l | 0, x, y, z, w); },
    Uniform1i(tid, c, l, x) { const g = ctxOf(c); if (g) g.uniform1i(l | 0, x | 0); },
    Uniform2i(tid, c, l, x, y) { const g = ctxOf(c); if (g) g.uniform2i(l | 0, x | 0, y | 0); },
    Uniform3i(tid, c, l, x, y, z) { const g = ctxOf(c); if (g) g.uniform3i(l | 0, x | 0, y | 0, z | 0); },
    Uniform4i(tid, c, l, x, y, z, w) { const g = ctxOf(c); if (g) g.uniform4i(l | 0, x | 0, y | 0, z | 0, w | 0); },
    Uniform1fv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNfv(1, l | 0, n | 0, v); },
    Uniform2fv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNfv(2, l | 0, n | 0, v); },
    Uniform3fv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNfv(3, l | 0, n | 0, v); },
    Uniform4fv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNfv(4, l | 0, n | 0, v); },
    Uniform1iv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNiv(1, l | 0, n | 0, v); },
    Uniform2iv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNiv(2, l | 0, n | 0, v); },
    Uniform3iv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNiv(3, l | 0, n | 0, v); },
    Uniform4iv(tid, c, l, n, v) { const g = ctxOf(c); if (g) g.uniformNiv(4, l | 0, n | 0, v); },
    UniformMatrix2fv(tid, c, l, n, t, v) { const g = ctxOf(c); if (g) g.uniformMatrix(2, l | 0, n | 0, t & 0xff, v); },
    UniformMatrix3fv(tid, c, l, n, t, v) { const g = ctxOf(c); if (g) g.uniformMatrix(3, l | 0, n | 0, t & 0xff, v); },
    UniformMatrix4fv(tid, c, l, n, t, v) { const g = ctxOf(c); if (g) g.uniformMatrix(4, l | 0, n | 0, t & 0xff, v); },

    // ---- vertex attributes and drawing ----------------------------------------------------------------
    EnableVertexAttribArray(tid, c, i) { const g = ctxOf(c); if (g) g.gl.enableVertexAttribArray(i); },
    DisableVertexAttribArray(tid, c, i) { const g = ctxOf(c); if (g) g.gl.disableVertexAttribArray(i); },
    VertexAttribPointer(tid, c, i, size, type, norm, stride, ptr) { const g = ctxOf(c); if (g) g.vertexAttribPointer(i, size | 0, type, norm & 0xff, stride | 0, ptr); },
    VertexAttrib1f(tid, c, i, x) { const g = ctxOf(c); if (g) g.vertexAttribNf(i, 1, x, 0, 0, 1); },
    VertexAttrib2f(tid, c, i, x, y) { const g = ctxOf(c); if (g) g.vertexAttribNf(i, 2, x, y, 0, 1); },
    VertexAttrib3f(tid, c, i, x, y, z) { const g = ctxOf(c); if (g) g.vertexAttribNf(i, 3, x, y, z, 1); },
    VertexAttrib4f(tid, c, i, x, y, z, w) { const g = ctxOf(c); if (g) g.vertexAttribNf(i, 4, x, y, z, w); },
    VertexAttrib1fv(tid, c, i, p) { const g = ctxOf(c); if (g) g.vertexAttribNfv(i, 1, p); },
    VertexAttrib2fv(tid, c, i, p) { const g = ctxOf(c); if (g) g.vertexAttribNfv(i, 2, p); },
    VertexAttrib3fv(tid, c, i, p) { const g = ctxOf(c); if (g) g.vertexAttribNfv(i, 3, p); },
    VertexAttrib4fv(tid, c, i, p) { const g = ctxOf(c); if (g) g.vertexAttribNfv(i, 4, p); },
    GetVertexAttribfv(tid, c, i, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getVertexAttrib(i, pn); if (v) outFloats(p, v); },
    GetVertexAttribiv(tid, c, i, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getVertexAttrib(i, pn); if (v) outInts(p, v); },
    GetVertexAttribPointerv(tid, c, i, pn, p) {
      const g = ctxOf(c); if (!g) return;
      if (pn !== 0x8645 || i >= g.grp.nAttribs) return g.setErr(pn !== 0x8645 ? 0x500 : 0x501);
      M.wrU32(p, g.gl.getVertexAttribOffset(i, 0x8645));
    },
    DrawArrays(tid, c, mode, first, count) { const g = ctxOf(c); if (g) g.gl.drawArrays(mode, first | 0, count | 0); },
    DrawElements(tid, c, mode, count, type, ptr) { const g = ctxOf(c); if (g) g.drawElements(mode, count | 0, type, ptr); },

    // ---- textures -------------------------------------------------------------------------------------
    TexImage2D(tid, c, t, level, ifmt, w, h, border, fmt, type, px) { const g = ctxOf(c); if (g) g.texImage2D(t, level | 0, ifmt, w | 0, h | 0, border | 0, fmt, type, px); },
    TexSubImage2D(tid, c, t, level, x, y, w, h, fmt, type, px) { const g = ctxOf(c); if (g) g.texSubImage2D(t, level | 0, x | 0, y | 0, w | 0, h | 0, fmt, type, px); },
    CompressedTexImage2D(tid, c, t, level, ifmt, w, h, border, size, data) { const g = ctxOf(c); if (g) g.compressedTexImage2D(t, level | 0, ifmt, w | 0, h | 0, border | 0, size | 0, data); },
    CompressedTexSubImage2D(tid, c, t, level, x, y, w, h, fmt, size, data) { const g = ctxOf(c); if (g) g.compressedTexSubImage2D(t, level | 0, x | 0, y | 0, w | 0, h | 0, fmt, size | 0, data); },
    CopyTexImage2D(tid, c, t, level, ifmt, x, y, w, h, border) { const g = ctxOf(c); if (g) g.copyTexImage2D(t, level | 0, ifmt, x | 0, y | 0, w | 0, h | 0, border | 0); },
    CopyTexSubImage2D(tid, c, t, level, xo, yo, x, y, w, h) { const g = ctxOf(c); if (g) g.copyTexSubImage2D(t, level | 0, xo | 0, yo | 0, x | 0, y | 0, w | 0, h | 0); },
    GenerateMipmap(tid, c, t) { const g = ctxOf(c); if (g) g.gl.generateMipmap(t); },
    TexParameteri(tid, c, t, pn, v) { const g = ctxOf(c); if (g) g.texParameter(t, pn, v | 0); },
    TexParameterf(tid, c, t, pn, v) { const g = ctxOf(c); if (g) g.texParameterf(t, pn, v); },
    TexParameteriv(tid, c, t, pn, p) { const g = ctxOf(c); if (g) g.texParameter(t, pn, i32at(p)); },
    TexParameterfv(tid, c, t, pn, p) { const g = ctxOf(c); if (g) g.texParameterf(t, pn, f32at(p)); },
    GetTexParameteriv(tid, c, t, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getTexParameter(t, pn); if (v) outInts(p, v); },
    GetTexParameterfv(tid, c, t, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getTexParameter(t, pn); if (v) outFloats(p, v); },
    ReadPixels(tid, c, x, y, w, h, fmt, type, px) { const g = ctxOf(c); if (g) g.readPixels(x | 0, y | 0, w | 0, h | 0, fmt, type, px); },

    // ---- framebuffers and renderbuffers ---------------------------------------------------------------
    CheckFramebufferStatus(tid, c, t) { const g = ctxOf(c); return g ? g.checkFramebufferStatus(t) : 0; },
    FramebufferTexture2D(tid, c, t, att, tt, tex, level) { const g = ctxOf(c); if (g) g.framebufferTexture2D(t, att, tt, tex, level | 0); },
    FramebufferRenderbuffer(tid, c, t, att, rt, rb) { const g = ctxOf(c); if (g) g.framebufferRenderbuffer(t, att, rt, rb); },
    RenderbufferStorage(tid, c, t, ifmt, w, h) { const g = ctxOf(c); if (g) g.renderbufferStorage(t, ifmt, w | 0, h | 0); },
    GetRenderbufferParameteriv(tid, c, t, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getRenderbufferParameter(t, pn); if (v) outInts(p, v); },
    GetFramebufferAttachmentParameteriv(tid, c, t, att, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getFramebufferAttachmentParameter(t, att, pn); if (v) outInts(p, v); },

    // ---- queries --------------------------------------------------------------------------------------
    GetBooleanv(tid, c, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getParam(pn); if (v !== null) outBools(p, v); },
    GetIntegerv(tid, c, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getParam(pn); if (v !== null) outInts(p, v); },
    GetFloatv(tid, c, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getParam(pn); if (v !== null) outFloats(p, v); },
    GetString(tid, c, name) { const g = ctxOf(c); return g ? g.getString(name) : 0; },
  };

  // ---- extension interfaces ---------------------------------------------------------------------------
  const instanced = {
    DrawArraysInstancedANGLE(tid, c, mode, first, count, n) { const g = ctxOf(c); if (g) g.gl.drawArraysInstanced(mode, first | 0, count | 0, n | 0); },
    DrawElementsInstancedANGLE(tid, c, mode, count, type, ptr, n) { const g = ctxOf(c); if (g) g.drawElementsInstanced(mode, count | 0, type, ptr, n | 0); },
    VertexAttribDivisorANGLE(tid, c, i, d) { const g = ctxOf(c); if (g) g.gl.vertexAttribDivisor(i, d); },
  };
  const query = {
    GenQueriesEXT(tid, c, n, p) { const g = ctxOf(c); if (g) g.genQueries(n | 0, p); },
    DeleteQueriesEXT(tid, c, n, p) { const g = ctxOf(c); if (g) g.deleteQueries(n | 0, p); },
    IsQueryEXT(tid, c, id) { const g = ctxOf(c); return g ? g.isQuery(id) : 0; },
    BeginQueryEXT(tid, c, t, id) { const g = ctxOf(c); if (g) g.beginQuery(t, id); },
    EndQueryEXT(tid, c, t) { const g = ctxOf(c); if (g) g.endQuery(t); },
    GetQueryivEXT(tid, c, t, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getQuery(t, pn); if (v !== null) M.wrI32(p, v); },
    GetQueryObjectuivEXT(tid, c, id, pn, p) { const g = ctxOf(c); if (!g) return; const v = g.getQueryObject(id, pn); if (v !== null) M.wrU32(p, v); },
  };
  const vao = {
    GenVertexArraysOES(tid, c, n, p) { const g = ctxOf(c); if (g) g.genVertexArrays(n | 0, p); },
    DeleteVertexArraysOES(tid, c, n, p) { const g = ctxOf(c); if (g) g.deleteVertexArrays(n | 0, p); },
    IsVertexArrayOES(tid, c, id) { const g = ctxOf(c); return g ? g.isVertexArray(id) : 0; },
    BindVertexArrayOES(tid, c, id) { const g = ctxOf(c); if (g) g.bindVertexArray(id); },
  };
  const mapsub = {
    MapBufferSubDataCHROMIUM(tid, c, t, off, size, access) { const g = ctxOf(c); return g ? g.mapBufferSubData(t, off | 0, size | 0, access) : 0; },
    UnmapBufferSubDataCHROMIUM(tid, c, mem) { const g = ctxOf(c); if (g) g.unmapSub(mem, 0); },
    MapTexSubImage2DCHROMIUM(tid, c, t, level, x, y, w, h, fmt, type, access) { const g = ctxOf(c); return g ? g.mapTexSubImage2D(t, level | 0, x | 0, y | 0, w | 0, h | 0, fmt, type, access) : 0; },
    UnmapTexSubImage2DCHROMIUM(tid, c, mem) { const g = ctxOf(c); if (g) g.unmapSub(mem, 1); },
  };

  return {
    PPB_Graphics3D: g3d,
    PPB_OpenGLES2: gles,
    PPB_OpenGLES2InstancedArrays: instanced,
    PPB_OpenGLES2FramebufferBlit: {
      BlitFramebufferEXT(tid, c, sx0, sy0, sx1, sy1, dx0, dy0, dx1, dy1, mask, filter) {
        const g = ctxOf(c); if (g) g.blitFramebuffer(sx0 | 0, sy0 | 0, sx1 | 0, sy1 | 0, dx0 | 0, dy0 | 0, dx1 | 0, dy1 | 0, mask, filter);
      },
    },
    PPB_OpenGLES2FramebufferMultisample: {
      RenderbufferStorageMultisampleEXT(tid, c, t, samples, ifmt, w, h) { const g = ctxOf(c); if (g) g.renderbufferStorageMultisample(t, samples | 0, ifmt, w | 0, h | 0); },
    },
    PPB_OpenGLES2ChromiumEnableFeature: {
      EnableFeatureCHROMIUM(tid, c, feature) { const g = ctxOf(c); return g ? g.enableFeature(M.cstr(feature)) : 0; },
    },
    PPB_OpenGLES2Query: query,
    PPB_OpenGLES2VertexArrayObject: vao,
    'PPB_OpenGLES2DrawBuffers(Dev)': {
      DrawBuffersEXT(tid, c, count, bufs) {
        const g = ctxOf(c); if (!g) return;
        count |= 0; if (count < 0 || count > g.grp.maxDrawBuffers) return g.setErr(0x501);
        const list = []; for (let i = 0; i < count; i++) list.push(M.rdU32(bufs + i * 4));
        g.drawBuffers(list);
      },
    },
    PPB_OpenGLES2ChromiumMapSub: mapsub,
    'PPB_GLESChromiumTextureMapping(Dev)': {
      MapTexSubImage2DCHROMIUM: mapsub.MapTexSubImage2DCHROMIUM,
      UnmapTexSubImage2DCHROMIUM: mapsub.UnmapTexSubImage2DCHROMIUM,
    },
  };
}
