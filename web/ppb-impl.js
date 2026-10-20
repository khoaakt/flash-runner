// PPB_* implementations. `buildImpl(host)` returns { 'PPB_Name' | 'PPB_Name;ver': { Method(tid, ...args) } }.
// Handlers run with `this` = the Pepper host. Return values follow the signature table:
//   i/p: number|boolean, d/f: number, V: {type,lo,hi}, P/F: [x,y]; BLOCK parks the guest thread;
//   a Promise parks it until resolved.

import { PP_OK, PP_OK_COMPLETIONPENDING, PP_ERROR_FAILED, PP_ERROR_BADARGUMENT, PP_ERROR_BADRESOURCE, PP_ERROR_NOTSUPPORTED, PP_ERROR_BLOCKS_MAIN_THREAD, PP_ERROR_NO_MESSAGE_LOOP, PP_ERROR_INPROGRESS, VT, BLOCK, PP_COMPLETIONCALLBACK_FLAG_OPTIONAL } from './pp-consts.js';

import { buildNet } from './ppb-net.js';
import { buildAudio } from './ppb-audio.js';
import { buildFlash } from './ppb-flash.js';
import { buildMisc } from './ppb-misc.js';
import { buildGLES2 } from './ppb-gles2.js';
import { buildSockets } from './ppb-sockets.js';
import { buildFiles } from './ppb-files.js';
import { buildMedia } from './ppb-media.js';
import { buildUdp } from './ppb-udp.js';

export function buildImpl(host) {
  const impl = {};
  const now = () => performance.now();

  /** Complete an async PPB operation: invoke `cc` later with `result`, or unblock a blocking caller. */
  function finishAsync(tid, cc, loopOf, run) {
    // Returns what the PPB call should return (pp_completion_callback.h contract).
    if (!cc.func) {                                  // blocking call: legal off the main thread only
      if (tid === host.mainTid) return PP_ERROR_BLOCKS_MAIN_THREAD;
      return new Promise((resolve) => run((result) => resolve(result)));
    }
    if ((cc.flags & PP_COMPLETIONCALLBACK_FLAG_OPTIONAL) && !host.threadLoop.get(tid)) return PP_ERROR_NO_MESSAGE_LOOP;
    run((result) => host.postTask(loopOf, cc.func, [cc.user, result >>> 0]));
    return PP_OK_COMPLETIONPENDING;
  }
  host.finishAsync = finishAsync;

  /** Finish a call whose outcome is already known (typically an error detected up front). */
  function complete(tid, cc, result) {
    if (!cc.func) return tid === host.mainTid && result < 0 ? result : result;           // blocking: result returned directly
    if (cc.flags & PP_COMPLETIONCALLBACK_FLAG_OPTIONAL) return result;                   // optional: may complete synchronously
    host.postTask(host.threadLoop.get(tid) || 1, cc.func, [cc.user, result >>> 0]);      // required: always via the callback
    return PP_OK_COMPLETIONPENDING;
  }
  host.complete = complete;

  impl.PPB_Core = {
    AddRefResource(tid, r) { const o = host.res.get(r); if (o) o.refs++; },
    ReleaseResource(tid, r) { host.releaseRes(r); },
    GetTime() { return Date.now() / 1000; },
    GetTimeTicks() { return now() / 1000; },
    CallOnMainThread(tid, delay, cc, result) { host.postTask(1, cc.func, [cc.user, result >>> 0], delay > 0 ? delay : 0); },
    IsMainThread(tid) { return tid === host.mainTid ? 1 : 0; },
  };

  // ---- PPB_Var ---------------------------------------------------------------------------
  const varImpl = {
    AddRef(tid, v) { host.addRefVar(v); },
    Release(tid, v) { host.releaseVar(v); },
    VarFromUtf8(tid, data, len) { return host.mkVar(new TextDecoder().decode(host.ro(data, len))); },
    VarToUtf8(tid, v, lenPtr) {
      const e = v.type === VT.STRING ? host.vars.get(v.lo) : null;
      if (!e) { if (lenPtr) host.w32(lenPtr, 0); return 0; }
      if (!e.ptr) { const b = new TextEncoder().encode(e.s); e.ptr = host.allocBytes(b); e.len = b.length; }
      if (lenPtr) host.w32(lenPtr, e.len);
      return e.ptr;
    },
    VarToResource() { return 0; },
    VarFromResource(tid, r) { return host.mkVar(undefined); },
  };
  impl.PPB_Var = varImpl;
  impl['PPB_Var;1.0'] = { ...varImpl, VarFromUtf8(tid, module, data, len) { return varImpl.VarFromUtf8(tid, data, len); } };

  // ---- instance / view / messaging ---------------------------------------------------------
  impl.PPB_Instance = {
    BindGraphics(tid, instance, device) {
      const inst = host.instances.get(instance); if (!inst) return 0;
      if (device === 0) { const old = inst.g2d; inst.g2d = 0; if (old) host.releaseRes(old); return 1; }
      const d = host.getRes(device);
      if (!d || (d.type !== 'graphics2d' && d.type !== 'graphics3d')) return 0;
      if (inst.g2d === device) return 1;
      d.refs++;                                       // the instance keeps its bound device alive
      const old = inst.g2d; inst.g2d = device; if (old) host.releaseRes(old);
      host.opts.onBind && host.opts.onBind(instance, d);
      return 1;
    },
    IsFullFrame() { return 0; },
  };
  impl.PPB_View = {
    IsView(tid, r) { return host.getRes(r, 'view') ? 1 : 0; },
    GetRect(tid, r, p) { const v = host.getRes(r, 'view'); if (!v) return 0; host.wi32(p, v.rect.x); host.wi32(p + 4, v.rect.y); host.wi32(p + 8, v.rect.w); host.wi32(p + 12, v.rect.h); return 1; },
    IsFullscreen(tid, r) { return host.getRes(r, 'view')?.fullscreen ? 1 : 0; },
    IsVisible(tid, r) { return host.getRes(r, 'view')?.visible ? 1 : 0; },
    IsPageVisible(tid, r) { return host.getRes(r, 'view')?.pageVisible ? 1 : 0; },
    GetClipRect(tid, r, p) { const v = host.getRes(r, 'view'); if (!v) return 0; host.wi32(p, 0); host.wi32(p + 4, 0); host.wi32(p + 8, v.rect.w); host.wi32(p + 12, v.rect.h); return 1; },
    GetDeviceScale(tid, r) { return host.getRes(r, 'view')?.scale ?? 1; },
    GetCSSScale() { return 1; },
    GetScrollOffset(tid, r, p) { host.wi32(p, 0); host.wi32(p + 4, 0); return 1; },
  };
  impl.PPB_Messaging = {
    PostMessage(tid, instance, v) { host.onMessage(instance, host.varValue(v)); },
  };
  impl.PPB_Console = { Log() {}, LogWithSource(tid, instance, level, src, msg) { host.log(`console[${level}] ${host.varValue(src)}: ${host.varValue(msg)}`); } };

  // ---- message loops ---------------------------------------------------------------------
  impl.PPB_MessageLoop = {
    Create(tid, instance) { const id = host.newRes({ type: 'messageloop' }); host.loop(id); return id; },
    GetForMainThread() { host.res.get(1).refs++; return 1; },
    GetCurrent(tid) { const l = host.threadLoop.get(tid); if (!l) return 0; host.res.get(l).refs++; return l; },
    AttachToCurrentThread(tid, loop) {
      if (!host.getRes(loop, 'messageloop')) return PP_ERROR_BADRESOURCE;
      const prev = host.threadLoop.get(tid);
      if (prev === loop) return PP_OK;
      if (prev) return PP_ERROR_INPROGRESS;
      const ml = host.loop(loop);
      if (ml.thread && ml.thread !== tid) return PP_ERROR_INPROGRESS;      // a loop attaches to one thread only
      host.res.get(loop).refs++;                                          // attachment keeps the loop alive
      host.threadLoop.set(tid, loop); ml.thread = tid; return PP_OK;
    },
    Run() { return PP_ERROR_FAILED; },   // replaced by a guest-native implementation (pepper_ml_run)
    PostWork(tid, loop, cc, delay) {
      if (!host.getRes(loop, 'messageloop')) return PP_ERROR_BADRESOURCE;
      if (!cc.func) return PP_ERROR_BADARGUMENT;
      if (host.loop(loop).destroyed) return PP_ERROR_FAILED;
      host.postTask(loop, cc.func, [cc.user, 0], Number(delay));
      return PP_OK;
    },
    PostQuit(tid, loop, destroy) {
      if (!host.getRes(loop, 'messageloop')) return PP_ERROR_BADRESOURCE;
      const l = host.loop(loop);
      l.queue.push({ quit: true, destroy: !!destroy, loopId: loop });   // runs after all previously posted work
      host.pump(l);
      return PP_OK;
    },
  };

  // ---- ImageData / Graphics2D ------------------------------------------------------------
  const PP_IMAGEDATAFORMAT_BGRA_PREMUL = 0;
  impl.PPB_ImageData = {
    GetNativeImageDataFormat() { return PP_IMAGEDATAFORMAT_BGRA_PREMUL; },
    IsImageDataFormatSupported(tid, f) { return f === PP_IMAGEDATAFORMAT_BGRA_PREMUL ? 1 : 0; },   // only the native format is offered
    Create(tid, instance, format, sizePtr, zero) {
      const w = host.i32(sizePtr), h = host.i32(sizePtr + 4);
      if (w <= 0 || h <= 0 || w * h > 1 << 26 || format !== PP_IMAGEDATAFORMAT_BGRA_PREMUL) return 0;
      let ptr; try { ptr = host.alloc(w * h * 4); } catch { host.log(`PPB_ImageData::Create(${w}x${h}): guest arena exhausted`); return 0; }   // fail the call (resource 0), never hand back an error code as a resource
      if (zero) host.zeroBytes(ptr, w * h * 4);
      return host.newRes({ type: 'imagedata', w, h, format, ptr, stride: w * 4, onFree() { host.arena.release(ptr); } });
    },
    IsImageData(tid, r) { return host.getRes(r, 'imagedata') ? 1 : 0; },
    Describe(tid, r, p) {
      const o = host.getRes(r, 'imagedata'); if (!o) return 0;
      host.wi32(p, o.format); host.wi32(p + 4, o.w); host.wi32(p + 8, o.h); host.wi32(p + 12, o.stride); return 1;
    },
    Map(tid, r) { return host.getRes(r, 'imagedata')?.ptr ?? 0; },
    Unmap() {},
  };

  const g2d = {
    Create(tid, instance, sizePtr, opaque) {
      const w = host.i32(sizePtr), h = host.i32(sizePtr + 4);
      if (w <= 0 || h <= 0 || w * h > 1 << 26) return 0;
      const o = { type: 'graphics2d', instance, w, h, opaque: !!opaque, scale: 1, ops: [], back: new Uint8Array(w * h * 4), flushWait: null, onFree() { for (const op of o.ops) if (op.imId) host.releaseRes(op.imId); o.ops.length = 0; } };
      return host.newRes(o);
    },
    IsGraphics2D(tid, r) { return host.getRes(r, 'graphics2d') ? 1 : 0; },
    Describe(tid, r, sizePtr, opaquePtr) {
      const o = host.getRes(r, 'graphics2d'); if (!o) return 0;
      host.wi32(sizePtr, o.w); host.wi32(sizePtr + 4, o.h); host.wi32(opaquePtr, o.opaque ? 1 : 0); return 1;
    },
    PaintImageData(tid, r, image, tl, src) {
      const o = host.getRes(r, 'graphics2d'), im = host.getRes(image, 'imagedata'); if (!o || !im) return;
      const dx = host.i32(tl), dy = host.i32(tl + 4);
      const s = src ? { x: host.i32(src), y: host.i32(src + 4), w: host.i32(src + 8), h: host.i32(src + 12) } : { x: 0, y: 0, w: im.w, h: im.h };
      im.refs++;                                      // pixels must outlive a ReleaseResource(image) before Flush
      o.ops.push({ kind: 'paint', im, imId: image, dx, dy, s });
    },
    Scroll(tid, r, clip, amount) {
      const o = host.getRes(r, 'graphics2d'); if (!o) return;
      const c = clip ? { x: host.i32(clip), y: host.i32(clip + 4), w: host.i32(clip + 8), h: host.i32(clip + 12) } : { x: 0, y: 0, w: o.w, h: o.h };
      o.ops.push({ kind: 'scroll', c, dx: host.i32(amount), dy: host.i32(amount + 4) });
    },
    ReplaceContents(tid, r, image) {
      const o = host.getRes(r, 'graphics2d'), im = host.getRes(image, 'imagedata'); if (!o || !im) return;
      im.refs++;
      o.ops.push({ kind: 'replace', im, imId: image });
    },
    Flush(tid, r, cc) {
      const o = host.getRes(r, 'graphics2d'); if (!o) return PP_ERROR_BADRESOURCE;
      if (o.flushWait) return -11; // PP_ERROR_INPROGRESS: a previous Flush has not completed
      applyOps(o);
      if (host.instances.get(o.instance)?.g2d === r) host.onFrame(o.instance, o.back, o.w, o.h, o);   // only the bound context is shown
      return finishAsync(tid, cc, host.threadLoop.get(tid) || 1, (done) => {
        o.flushWait = true;
        setTimeout(() => { o.flushWait = null; done(PP_OK); }, host.opts.frameMs ?? 16);
      });
    },
    SetScale(tid, r, s) { const o = host.getRes(r, 'graphics2d'); if (!o || !(s > 0)) return 0; o.scale = s; return 1; },
    GetScale(tid, r) { return host.getRes(r, 'graphics2d')?.scale ?? 1; },
  };
  impl.PPB_Graphics2D = g2d;

  function applyOps(o) {
    const back = o.back, bw = o.w, bh = o.h;
    for (const op of o.ops) {
      if (op.kind === 'paint') {
        const { im, dx, dy, s } = op;
        // Source rect clipped to the image, then the destination (dx + rect offset) clipped to the surface.
        let sx = s.x, sy = s.y, sw = s.w, sh = s.h;
        if (sx < 0) { sw += sx; sx = 0; } if (sy < 0) { sh += sy; sy = 0; }
        sw = Math.min(sw, im.w - sx); sh = Math.min(sh, im.h - sy);
        let tx = dx + sx, ty = dy + sy;                      // image is placed at top_left; the rect then selects a part of it
        if (tx < 0) { sx -= tx; sw += tx; tx = 0; }
        if (ty < 0) { sy -= ty; sh += ty; ty = 0; }
        sw = Math.min(sw, bw - tx); sh = Math.min(sh, bh - ty);
        if (sw <= 0 || sh <= 0) continue;
        const src = host.ro(im.ptr + sy * im.w * 4, sh * im.w * 4);          // just the rows being painted (remote backends fetch only those)
        for (let y = 0; y < sh; y++) back.set(src.subarray((y * im.w + sx) * 4, (y * im.w + sx + sw) * 4), ((ty + y) * bw + tx) * 4);
      } else if (op.kind === 'replace') {
        if (op.im.w === bw && op.im.h === bh) back.set(host.ro(op.im.ptr, op.im.w * op.im.h * 4));
      } else if (op.kind === 'scroll') {
        const { c, dx, dy } = op;
        const tmp = new Uint8Array(c.w * c.h * 4);
        for (let y = 0; y < c.h; y++) tmp.set(back.subarray(((c.y + y) * bw + c.x) * 4, ((c.y + y) * bw + c.x + c.w) * 4), y * c.w * 4);
        for (let y = 0; y < c.h; y++) {
          const ty = c.y + y + dy; if (ty < c.y || ty >= c.y + c.h) continue;
          for (let x = 0; x < c.w; x++) { const tx = c.x + x + dx; if (tx < c.x || tx >= c.x + c.w) continue; back.set(tmp.subarray((y * c.w + x) * 4, (y * c.w + x) * 4 + 4), (ty * bw + tx) * 4); }
        }
      }
    }
    for (const op of o.ops) if (op.imId) host.releaseRes(op.imId);
    o.ops.length = 0;
  }

  // ---- input events ----------------------------------------------------------------------
  const ie = (r) => host.getRes(r, 'inputevent');
  impl.PPB_InputEvent = {
    RequestInputEvents(tid, instance, classes) { const i = host.instances.get(instance); if (i) i.inputClasses |= classes; return PP_OK; },
    RequestFilteringInputEvents(tid, instance, classes) { const i = host.instances.get(instance); if (i) { i.inputClasses |= classes; i.filtering = (i.filtering | 0) | classes; } return PP_OK; },
    ClearInputEventRequest(tid, instance, classes) { const i = host.instances.get(instance); if (i) i.inputClasses &= ~classes; },
    IsInputEvent(tid, r) { return ie(r) ? 1 : 0; },
    GetType(tid, r) { return ie(r)?.etype ?? -1; },
    GetTimeStamp(tid, r) { return ie(r)?.ts ?? 0; },
    GetModifiers(tid, r) { return ie(r)?.mods ?? 0; },
  };
  impl.PPB_MouseInputEvent = {
    IsMouseInputEvent(tid, r) { return ie(r)?.mouse ? 1 : 0; },
    GetButton(tid, r) { return ie(r)?.mouse?.button ?? -1; },
    GetPosition(tid, r) { const m = ie(r)?.mouse; return [m?.x ?? 0, m?.y ?? 0]; },
    GetClickCount(tid, r) { return ie(r)?.mouse?.clicks ?? 0; },
    GetMovement(tid, r) { const m = ie(r)?.mouse; return [m?.mx ?? 0, m?.my ?? 0]; },
  };
  impl.PPB_WheelInputEvent = {
    IsWheelInputEvent(tid, r) { return ie(r)?.wheel ? 1 : 0; },
    GetDelta(tid, r) { const w = ie(r)?.wheel; return [w?.dx ?? 0, w?.dy ?? 0]; },
    GetTicks(tid, r) { const w = ie(r)?.wheel; return [w?.tx ?? 0, w?.ty ?? 0]; },
    GetScrollByPage(tid, r) { return ie(r)?.wheel?.page ? 1 : 0; },
  };
  impl.PPB_KeyboardInputEvent = {
    IsKeyboardInputEvent(tid, r) { return ie(r)?.key ? 1 : 0; },
    GetKeyCode(tid, r) { return ie(r)?.key?.code ?? 0; },
    GetCharacterText(tid, r) { return host.mkVar(ie(r)?.key?.text ?? ''); },
    GetCode(tid, r) { return host.mkVar(ie(r)?.key?.codeStr ?? ''); },
  };

  Object.assign(impl, buildNet(host), buildAudio(host), buildFlash(host), buildMisc(host), buildGLES2(host), buildSockets(host), buildFiles(host), buildMedia(host), buildUdp(host));
  return impl;
}
