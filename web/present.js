// Frame presenter: WebGPU (preferred) with a 2D-canvas fallback. Works on a main-thread canvas or
// an OffscreenCanvas inside the worker. Input is BGRA8 premultiplied, exactly what Graphics2D produces.

const WGSL = /* wgsl */ `
struct VOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VOut {
  var p = array<vec2f, 3>(vec2f(-1.0, -1.0), vec2f(3.0, -1.0), vec2f(-1.0, 3.0));
  var o: VOut;
  o.pos = vec4f(p[i], 0.0, 1.0);
  o.uv = vec2f((p[i].x + 1.0) * 0.5, 1.0 - (p[i].y + 1.0) * 0.5);
  return o;
}
@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var tex: texture_2d<f32>;
@fragment fn fs(in: VOut) -> @location(0) vec4f { return textureSample(tex, samp, in.uv); }
`;

export class Presenter {
  static async create(canvas, opts = {}) {
    const p = new Presenter(canvas);
    if (!opts.force2d && typeof navigator !== 'undefined' && navigator.gpu) {
      try { await p.initGPU(); return p; } catch (e) { (opts.log || console.warn)('WebGPU unavailable, falling back to 2D: ' + e); }
    }
    p.init2D();
    return p;
  }
  constructor(canvas) { this.canvas = canvas; this.kind = 'none'; this.tex = null; this.tw = 0; this.th = 0; this.frames = 0; }

  async initGPU() {
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) throw new Error('no adapter');
    const device = await adapter.requestDevice();
    const ctx = this.canvas.getContext('webgpu');
    const format = navigator.gpu.getPreferredCanvasFormat();
    ctx.configure({ device, format, alphaMode: 'premultiplied' });
    const module = device.createShaderModule({ code: WGSL });
    this.pipeline = device.createRenderPipeline({
      layout: 'auto',
      vertex: { module, entryPoint: 'vs' },
      fragment: { module, entryPoint: 'fs', targets: [{ format }] },
      primitive: { topology: 'triangle-list' },
    });
    this.samp = device.createSampler({ magFilter: 'linear', minFilter: 'linear' });
    this.device = device; this.ctx = ctx; this.kind = 'webgpu';
    this.adapterInfo = adapter.info || {};
  }
  init2D() { this.ctx2d = this.canvas.getContext('2d'); this.kind = '2d'; }

  resize(w, h) { if (this.canvas.width !== w) this.canvas.width = w; if (this.canvas.height !== h) this.canvas.height = h; }

  present(bgra, w, h) {
    this.resize(w, h);
    this.frames++;
    if (this.kind === 'webgpu') return this.presentGPU(bgra, w, h);
    return this.present2D(bgra, w, h);
  }

  /** Composite a WebGL OffscreenCanvas (a Graphics3D frame, premultiplied alpha, top row first) into the page canvas. */
  presentCanvas(src, w, h) {
    this.resize(w, h);
    this.frames++;
    if (this.kind === 'webgpu') return this.presentCanvasGPU(src, w, h);
    const c = this.ctx2d;
    c.globalCompositeOperation = 'copy';
    c.drawImage(src, 0, 0);
    c.globalCompositeOperation = 'source-over';
  }

  presentCanvasGPU(src, w, h) {
    const d = this.device;
    if (!this.ctex || this.cw !== w || this.ch !== h) {
      this.ctex?.destroy();
      // copyExternalImageToTexture needs RENDER_ATTACHMENT; COPY_SRC lets snapshot() read the composite input back
      this.ctex = d.createTexture({ size: [w, h], format: 'rgba8unorm', usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST | GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC });
      this.cbind = d.createBindGroup({ layout: this.pipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: this.samp }, { binding: 1, resource: this.ctex.createView() }] });
      this.cw = w; this.ch = h;
    }
    d.queue.copyExternalImageToTexture({ source: src }, { texture: this.ctex, premultipliedAlpha: true }, { width: w, height: h });
    this.drawFullscreen(this.cbind);
  }

  drawFullscreen(bind) {
    const d = this.device;
    this.lastBind = bind;
    const enc = d.createCommandEncoder();
    const pass = enc.beginRenderPass({ colorAttachments: [{ view: this.ctx.getCurrentTexture().createView(), loadOp: 'clear', storeOp: 'store', clearValue: { r: 0, g: 0, b: 0, a: 0 } }] });
    pass.setPipeline(this.pipeline); pass.setBindGroup(0, bind); pass.draw(3); pass.end();
    d.queue.submit([enc.finish()]);
  }

  /** What is currently on the page canvas, as straight RGBA (for tests / screenshots). */
  async snapshot() {
    const w = this.canvas.width, h = this.canvas.height;
    if (this.kind === '2d') return { w, h, data: this.ctx2d.getImageData(0, 0, w, h).data };
    // A WebGPU canvas only keeps its pixels for the task they were drawn in: draw the last frame again and grab it right away.
    if (this.lastBind) this.drawFullscreen(this.lastBind);
    const bmp = await createImageBitmap(this.canvas);
    const c = new OffscreenCanvas(w, h).getContext('2d', { willReadFrequently: true });
    c.drawImage(bmp, 0, 0);
    return { w, h, data: c.getImageData(0, 0, w, h).data };
  }

  presentGPU(bgra, w, h) {
    const d = this.device;
    if (!this.tex || this.tw !== w || this.th !== h) {
      this.tex?.destroy();
      this.tex = d.createTexture({ size: [w, h], format: 'bgra8unorm', usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST });
      this.bind = d.createBindGroup({ layout: this.pipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: this.samp }, { binding: 1, resource: this.tex.createView() }] });
      this.tw = w; this.th = h;
    }
    d.queue.writeTexture({ texture: this.tex }, bgra, { bytesPerRow: w * 4 }, { width: w, height: h });
    this.drawFullscreen(this.bind);
  }

  present2D(bgra, w, h) {
    if (!this.img || this.img.width !== w || this.img.height !== h) this.img = new ImageData(w, h);
    const out = this.img.data;
    for (let i = 0; i < out.length; i += 4) {          // BGRA premul -> RGBA straight (canvas wants straight alpha)
      const a = bgra[i + 3];
      if (a === 255) { out[i] = bgra[i + 2]; out[i + 1] = bgra[i + 1]; out[i + 2] = bgra[i]; out[i + 3] = 255; }
      else if (a === 0) { out[i] = out[i + 1] = out[i + 2] = out[i + 3] = 0; }
      else { const k = 255 / a; out[i] = Math.min(255, bgra[i + 2] * k); out[i + 1] = Math.min(255, bgra[i + 1] * k); out[i + 2] = Math.min(255, bgra[i] * k); out[i + 3] = a; }
    }
    this.ctx2d.putImageData(this.img, 0, 0);
  }
}
