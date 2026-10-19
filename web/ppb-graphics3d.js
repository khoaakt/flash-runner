// PPB_Graphics3D;1.0 on WebGL2.
//
// A Graphics3D resource is a GLContext (gles2-context.js). The first context of a share group creates an OffscreenCanvas +
// WebGL2 context; contexts created with `share_context` live on the same WebGL2 context (so buffers/textures/shaders/
// programs/renderbuffers are really shared). Every context, shared or not, has its OWN default framebuffer (a private FBO);
// the canvas is only a staging surface that SwapBuffers blits the swapping context's frame into.
//
// SwapBuffers hands the staging canvas to `host.opts.onSwap(instance, canvas, w, h, ctx)` (worker.js -> Presenter.presentCanvas)
// when the context is bound to its instance via PPB_Instance::BindGraphics, and completes the callback after one
// display refresh (opts.frameMs, default 16 ms).

import { PP_OK, PP_ERROR_BADRESOURCE, PP_ERROR_BADARGUMENT, PP_ERROR_INPROGRESS, PP_ERROR_BLOCKS_MAIN_THREAD } from './pp-consts.js';
import { GLContext, GLGroup, GuestMem } from './gles2-context.js';

const PP_ERROR_CONTEXT_LOST = -50;
const A = {
  ALPHA: 0x3021, BLUE: 0x3022, GREEN: 0x3023, RED: 0x3024, DEPTH: 0x3025, STENCIL: 0x3026, SAMPLES: 0x3031, SAMPLE_BUFFERS: 0x3032,
  NONE: 0x3038, HEIGHT: 0x3056, WIDTH: 0x3057, SWAP: 0x3093, PRESERVED: 0x3094, DESTROYED: 0x3095, GPU: 0x11000, LOW_POWER: 0x11001, PERF: 0x11002, SINGLE: 0x3085,
};

export function buildGraphics3D(host) {
  const mem = host.makeGuestMem ? host.makeGuestMem() : new GuestMem(host);
  const log = (s) => host.log(s);
  let lastId = 0, lastCtx = null;

  /** Resolve a context resource (cached) and make it the current context of its share group. */
  function ctxOf(id) {
    let c;
    if (id === lastId) c = lastCtx;
    else {
      c = host.res.get(id);
      if (!c || c.type !== 'graphics3d') return null;
      lastId = id; lastCtx = c;
    }
    if (c.grp.cur !== c) c.makeCurrent();
    return c;
  }

  // PPP_Graphics_3D;1.0::Graphics3DContextLost(instance). The interface is not in the generated PPP signature table, so it is
  // looked up through PPP_GetInterface by name; its only member is the function pointer at offset 0.
  let ppp3d;                                         // undefined: not looked up yet; 0: the plugin does not implement it
  function notifyLost(grp) {
    if (!host.getPPP || typeof host.call !== 'function') return;
    const insts = new Set(); for (const c of grp.ctxs) insts.add(c.instance);
    const go = (p) => { if (p) for (const i of insts) host.call(host.u32(p), [i]); };
    if (ppp3d !== undefined) return go(ppp3d);
    ppp3d = 0;
    host.call(host.getPPP, [host.allocCStr('PPP_Graphics_3D;1.0')]).then((r) => { ppp3d = r.lo >>> 0; go(ppp3d); }, () => {});
  }

  const canvasFactory = (w, h) => (host.opts.createGLCanvas ? host.opts.createGLCanvas(w, h) : new OffscreenCanvas(w, h));
  let probe;                                         // a throw-away context for GetAttribMaxValue
  function limits() {
    if (probe !== undefined) return probe;
    probe = { samples: 4, size: 4096 };
    try {
      const gl = canvasFactory(1, 1).getContext('webgl2');
      if (gl) { probe = { samples: gl.getParameter(gl.MAX_SAMPLES), size: Math.min(gl.getParameter(gl.MAX_RENDERBUFFER_SIZE), gl.getParameter(gl.MAX_TEXTURE_SIZE)) }; gl.getExtension('WEBGL_lose_context')?.loseContext(); }
    } catch (e) { /* defaults */ }
    return probe;
  }

  function parseAttribs(p) {
    const a = { alpha: 0, red: 0, green: 0, blue: 0, depth: 0, stencil: 0, samples: 0, sampleBuffers: 0, width: 0, height: 0, swap: 0, gpu: A.PERF, single: 0 };
    if (!p) return a;
    for (let i = 0; i < 64; i++, p += 8) {
      const k = mem.rdU32(p);
      if (k === A.NONE) return a;
      const v = mem.rdI32(p + 4);
      switch (k) {
        case A.ALPHA: a.alpha = v; break; case A.RED: a.red = v; break; case A.GREEN: a.green = v; break; case A.BLUE: a.blue = v; break;
        case A.DEPTH: a.depth = v; break; case A.STENCIL: a.stencil = v; break;
        case A.SAMPLES: a.samples = v; break; case A.SAMPLE_BUFFERS: a.sampleBuffers = v; break;
        case A.WIDTH: a.width = v; break; case A.HEIGHT: a.height = v; break;
        case A.SWAP: a.swap = v; break; case A.GPU: a.gpu = v; break; case A.SINGLE: a.single = v; break;
        default: log(`PPB_Graphics3D::Create: unknown attribute 0x${k.toString(16)}`); return null;
      }
    }
    return null;                                     // unterminated list
  }

  const impl = {
    GetAttribMaxValue(tid, instance, attribute, value) {
      if (!host.instances.get(instance)) return PP_ERROR_BADRESOURCE;
      if (!value) return PP_ERROR_BADARGUMENT;
      const l = limits();
      switch (attribute) {
        case A.ALPHA: case A.RED: case A.GREEN: case A.BLUE: case A.STENCIL: mem.wrI32(value, 8); break;
        case A.DEPTH: mem.wrI32(value, 24); break;
        case A.SAMPLES: mem.wrI32(value, l.samples); break;
        case A.SAMPLE_BUFFERS: mem.wrI32(value, 1); break;
        case A.WIDTH: case A.HEIGHT: mem.wrI32(value, l.size); break;
        default: return PP_ERROR_BADARGUMENT;
      }
      return PP_OK;
    },

    Create(tid, instance, share, attribs) {
      if (!host.instances.get(instance)) return 0;
      const a = parseAttribs(attribs);
      if (!a) return 0;
      let shareCtx = null;
      if (share) { shareCtx = host.getRes(share, 'graphics3d'); if (!shareCtx) return 0; }
      const w = Math.max(1, a.width), h = Math.max(1, a.height);
      let grp;
      if (shareCtx) grp = shareCtx.grp;
      else {
        let gl = null, canvas = null;
        try {
          canvas = canvasFactory(w, h);
          // The canvas is only the group's staging surface for presenting frames (every context renders into its own FBO),
          // so it needs no depth/stencil/MSAA of its own.
          gl = canvas.getContext('webgl2', {
            alpha: true, depth: false, stencil: false, antialias: false, premultipliedAlpha: true,
            preserveDrawingBuffer: false, powerPreference: a.gpu === A.LOW_POWER ? 'low-power' : 'high-performance',
          });
        } catch (e) { log('PPB_Graphics3D::Create: ' + e); }
        if (!gl) { log('PPB_Graphics3D::Create: WebGL2 is not available'); return 0; }
        grp = new GLGroup(gl, canvas);
        const lost = (e) => {
          if (e.preventDefault) e.preventDefault();
          grp.lost = true; for (const c of grp.ctxs) c.lost = true;
          if (!grp.closing) { log('PPB_Graphics3D: WebGL context lost'); notifyLost(grp); }
        };
        canvas.addEventListener?.('webglcontextlost', lost);
      }
      const ctx = new GLContext(grp, mem, { w, h, attrs: a, log });
      ctx.actual = { alpha: a.alpha > 0, depth: a.depth > 0, stencil: a.stencil > 0, samples: ctx.surf.samples };
      ctx.lost = grp.lost;
      ctx.instance = instance;
      const id = host.newRes(ctx);
      ctx.id = id;
      ctx.onFree = () => { if (lastId === id) { lastId = 0; lastCtx = null; } ctx.destroy(); };
      return id;
    },

    IsGraphics3D(tid, r) { return host.getRes(r, 'graphics3d') ? 1 : 0; },

    GetAttribs(tid, c, list) {
      const ctx = host.getRes(c, 'graphics3d'); if (!ctx) return PP_ERROR_BADRESOURCE;
      if (!list) return PP_ERROR_BADARGUMENT;
      for (let p = list, i = 0; i < 64; i++, p += 8) {
        const k = mem.rdU32(p);
        if (k === A.NONE) return PP_OK;
        let v;
        switch (k) {
          case A.RED: case A.GREEN: case A.BLUE: v = 8; break;
          case A.ALPHA: v = ctx.actual.alpha ? 8 : 0; break;
          case A.DEPTH: v = ctx.actual.depth ? 24 : 0; break;
          case A.STENCIL: v = ctx.actual.stencil ? 8 : 0; break;
          case A.SAMPLES: v = ctx.actual.samples; break;
          case A.SAMPLE_BUFFERS: v = ctx.actual.samples > 0 ? 1 : 0; break;
          case A.WIDTH: v = ctx.w; break; case A.HEIGHT: v = ctx.h; break;
          case A.SWAP: v = ctx.swapBehavior; break;
          case A.GPU: v = ctx.attrs.gpu; break;
          case A.SINGLE: v = 0; break;
          default: return PP_ERROR_BADARGUMENT;
        }
        mem.wrI32(p + 4, v);
      }
      return PP_ERROR_BADARGUMENT;
    },

    SetAttribs(tid, c, list) {
      const ctx = host.getRes(c, 'graphics3d'); if (!ctx) return PP_ERROR_BADRESOURCE;
      if (!list) return PP_ERROR_BADARGUMENT;
      for (let p = list, i = 0; i < 64; i++, p += 8) {
        const k = mem.rdU32(p);
        if (k === A.NONE) return PP_OK;
        const v = mem.rdI32(p + 4);
        if (k !== A.SWAP || (v !== A.PRESERVED && v !== A.DESTROYED)) return PP_ERROR_BADARGUMENT;
        ctx.swapBehavior = v;     // the surface is a private FBO that is never cleared by SwapBuffers, so both behaviours hold
      }
      return PP_ERROR_BADARGUMENT;
    },

    GetError(tid, c) {
      const ctx = host.getRes(c, 'graphics3d'); if (!ctx) return PP_ERROR_BADRESOURCE;
      return ctx.lost ? PP_ERROR_CONTEXT_LOST : PP_OK;
    },

    ResizeBuffers(tid, c, w, h) {
      const ctx = host.getRes(c, 'graphics3d'); if (!ctx) return PP_ERROR_BADRESOURCE;
      w |= 0; h |= 0;
      if (w < 0 || h < 0) return PP_ERROR_BADARGUMENT;
      ctx.resizeBuffers(w, h);
      return PP_OK;
    },

    SwapBuffers(tid, c, cc) {
      const ctx = host.getRes(c, 'graphics3d'); if (!ctx) return PP_ERROR_BADRESOURCE;
      if (!cc.func && tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;   // a blocking SwapBuffers is only legal off the main thread
      if (ctx.swapPending) return PP_ERROR_INPROGRESS;
      const lost = ctx.lost;
      if (!lost) {
        if (ctx.grp.cur !== ctx) ctx.makeCurrent();
        const inst = host.instances.get(ctx.instance);
        if (inst && inst.g2d === ctx.id && host.opts.onSwap) {
          const src = ctx.frameCanvas();               // blits this context's own surface into the staging canvas (implicit flush)
          host.opts.onSwap(ctx.instance, src, ctx.surf.w, ctx.surf.h, ctx);
        } else ctx.gl.flush();                         // not on screen: SwapBuffers has no visible effect
      }
      ctx.swapPending = true;
      return host.finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
        setTimeout(() => { ctx.swapPending = false; done(lost || ctx.lost ? PP_ERROR_CONTEXT_LOST : PP_OK); }, host.opts.frameMs ?? 16);
      });
    },
  };
  return { impl, ctxOf, mem };
}
