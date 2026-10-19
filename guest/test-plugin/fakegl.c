/* A tiny PPAPI plugin that exercises PPB_Graphics3D + PPB_OpenGLES2 end to end (emulator -> pepper-host -> JS -> WebGL2),
 * and reports what it observed with PPB_Messaging::PostMessage so web/gl-test.html can assert on it.
 *
 * NO FLOATING POINT: the x87/SSE units of the emulator may not be ready, so every GLfloat is passed as an IEEE-754 bit
 * pattern (the GLES entry points that take floats are called through function-pointer types that take uint32_t in the
 * same cdecl stack slot).  Build with -O1 (tools/build-guest.sh). */
#include <stdint.h>
#include <string.h>
#include <stdio.h>
#include <pthread.h>
#include "ppapi/c/pp_errors.h"
#include "ppapi/c/pp_module.h"
#include "ppapi/c/pp_var.h"
#include "ppapi/c/ppb.h"
#include "ppapi/c/ppb_core.h"
#include "ppapi/c/ppb_graphics_3d.h"
#include "ppapi/c/ppb_opengles2.h"
#include "ppapi/c/ppb_instance.h"
#include "ppapi/c/ppb_messaging.h"
#include "ppapi/c/ppb_var.h"
#include "ppapi/c/ppb_view.h"
#include "ppapi/c/ppp.h"
#include "ppapi/c/ppp_instance.h"
#include "ppapi/c/ppp_messaging.h"

typedef struct PPB_Core_1_0 PPB_Core_1_0; typedef struct PPB_Var_1_2 PPB_Var_1_2; typedef struct PPB_Messaging_1_0 PPB_Messaging_1_0;
typedef struct PPB_Instance_1_0 PPB_Instance_1_0; typedef struct PPB_View_1_1 PPB_View_1_1;
typedef struct PPP_Instance_1_1 PPP_Instance_1_1; typedef struct PPP_Messaging_1_0 PPP_Messaging_1_0;
static const PPB_Core_1_0 *core; static const PPB_Var_1_2 *var; static const PPB_Messaging_1_0 *msg;
static const PPB_Instance_1_0 *inst; static const PPB_View_1_1 *view;
static const struct PPB_Graphics3D_1_0 *g3d;
static const struct PPB_OpenGLES2_1_0 *gl;

/* IEEE-754 single precision bit patterns */
#define F_0 0x00000000u
#define F_1 0x3f800000u
#define F_Q 0x3e800000u    /* 0.25 */
#define F_NQ 0xbe800000u   /* -0.25 */
#define F_N1 0xbf800000u   /* -1.0 */

#define GL_COLOR_BUFFER_BIT 0x4000
#define GL_DEPTH_BUFFER_BIT 0x100
#define GL_ARRAY_BUFFER 0x8892
#define GL_ELEMENT_ARRAY_BUFFER 0x8893
#define GL_STATIC_DRAW 0x88E4
#define GL_FLOAT 0x1406
#define GL_TRIANGLES 4
#define GL_UNSIGNED_SHORT 0x1403
#define GL_UNSIGNED_BYTE 0x1401
#define GL_TEXTURE_2D 0xDE1
#define GL_RGBA 0x1908
#define GL_NEAREST 0x2600
#define GL_TEXTURE_MIN_FILTER 0x2801
#define GL_TEXTURE_MAG_FILTER 0x2800
#define GL_TEXTURE_WRAP_S 0x2802
#define GL_TEXTURE_WRAP_T 0x2803
#define GL_CLAMP_TO_EDGE 0x812F
#define GL_VERTEX_SHADER 0x8B31
#define GL_FRAGMENT_SHADER 0x8B30
#define GL_COMPILE_STATUS 0x8B81
#define GL_LINK_STATUS 0x8B82
#define GL_FRAMEBUFFER 0x8D40
#define GL_COLOR_ATTACHMENT0 0x8CE0
#define GL_VENDOR 0x1F00
#define GL_VERSION 0x1F02
#define GL_EXTENSIONS 0x1F03
#define GL_SHADING_LANGUAGE_VERSION 0x8B8C
#define GL_MAX_TEXTURE_SIZE 0xD33
#define GL_VIEWPORT 0xBA2
#define GL_TEXTURE0 0x84C0

typedef void (*ClearColor_t)(PP_Resource, uint32_t, uint32_t, uint32_t, uint32_t);
typedef void (*Uniform1f_t)(PP_Resource, GLint, uint32_t);
typedef void (*Uniform2f_t)(PP_Resource, GLint, uint32_t, uint32_t);
#define ClearColor(r, g, b, a) ((ClearColor_t)(void *)gl->ClearColor)(ctx, r, g, b, a)
#define Uniform1f(l, x) ((Uniform1f_t)(void *)gl->Uniform1f)(ctx, l, x)
#define Uniform2f(l, x, y) ((Uniform2f_t)(void *)gl->Uniform2f)(ctx, l, x, y)

static PP_Instance g_inst;
static PP_Resource ctx;
static GLuint prog, vbo_tri, vbo_quad, ibo_quad, tex2x2;
static GLint loc_mix, loc_tex, loc_off;
static int frames, started;

static void say(const char *s) {
    struct PP_Var v = var->VarFromUtf8(s, (uint32_t)strlen(s));
    msg->PostMessage(g_inst, v);
    var->Release(v);
}
#define SAY(...) do { char b_[400]; snprintf(b_, sizeof b_, __VA_ARGS__); say(b_); } while (0)

static const char *VS =
    "attribute vec2 a_pos; attribute vec2 a_uv; attribute vec4 a_col; varying vec2 v_uv; varying vec4 v_col; uniform vec2 u_off;\n"
    "void main() { v_uv = a_uv; v_col = a_col; gl_Position = vec4(a_pos + u_off, 0.0, 1.0); }\n";
static const char *FS =
    "precision mediump float; varying vec2 v_uv; varying vec4 v_col; uniform sampler2D u_tex; uniform float u_mix;\n"
    "void main() { gl_FragColor = mix(v_col, texture2D(u_tex, v_uv), u_mix); }\n";
static const char *BADFS = "precision mediump float;\nvoid main() { gl_FragColor = vec4(undefined_thing); }\n";

static GLuint compile(GLenum type, const char *src, int *ok, char *log, int logsz) {
    GLuint s = gl->CreateShader(ctx, type);
    const char *srcs[1] = { src };
    gl->ShaderSource(ctx, s, 1, srcs, 0);
    gl->CompileShader(ctx, s);
    GLint st = 0, n = 0;
    gl->GetShaderiv(ctx, s, GL_COMPILE_STATUS, &st);
    *ok = st;
    if (log) { gl->GetShaderInfoLog(ctx, s, logsz, &n, log); log[n < logsz ? n : logsz - 1] = 0; }
    return s;
}

static const uint32_t tri_verts[] = {      /* pos.xy, uv.xy, col.rgba (yellow) */
    F_NQ, F_NQ, F_0, F_0, F_1, F_1, F_0, F_1,
    F_Q,  F_NQ, F_1, F_0, F_1, F_1, F_0, F_1,
    F_0,  F_Q,  F_0, F_1, F_1, F_1, F_0, F_1,
};
static const uint32_t quad_verts[] = {     /* full-screen quad, white vertex colour, uv 0..1 */
    F_N1, F_N1, F_0, F_0, F_1, F_1, F_1, F_1,
    F_1,  F_N1, F_1, F_0, F_1, F_1, F_1, F_1,
    F_1,  F_1,  F_1, F_1, F_1, F_1, F_1, F_1,
    F_N1, F_1,  F_0, F_1, F_1, F_1, F_1, F_1,
};
static const uint16_t quad_idx[] = { 0, 1, 2, 0, 2, 3 };
static const uint8_t tex_px[16] = { 255, 0, 0, 255,  0, 255, 0, 255,   0, 0, 255, 255,  255, 255, 255, 255 };  /* row 0: red green, row 1: blue white */

static void setup_attribs(GLuint vbo) {
    gl->BindBuffer(ctx, GL_ARRAY_BUFFER, vbo);
    gl->EnableVertexAttribArray(ctx, 0); gl->VertexAttribPointer(ctx, 0, 2, GL_FLOAT, 0, 32, (const void *)0);
    gl->EnableVertexAttribArray(ctx, 1); gl->VertexAttribPointer(ctx, 1, 2, GL_FLOAT, 0, 32, (const void *)8);
    gl->EnableVertexAttribArray(ctx, 2); gl->VertexAttribPointer(ctx, 2, 4, GL_FLOAT, 0, 32, (const void *)16);
}

static void draw_frame(int size) {
    gl->Viewport(ctx, 0, 0, size, size);
    ClearColor(F_0, F_0, F_0, F_1);
    gl->Clear(ctx, GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    gl->UseProgram(ctx, prog);
    gl->ActiveTexture(ctx, GL_TEXTURE0);
    gl->BindTexture(ctx, GL_TEXTURE_2D, tex2x2);
    gl->Uniform1i(ctx, loc_tex, 0);
    Uniform2f(loc_off, F_0, F_0);
    /* textured quad: DrawElements from an element buffer */
    Uniform1f(loc_mix, F_1);
    setup_attribs(vbo_quad);
    gl->BindBuffer(ctx, GL_ELEMENT_ARRAY_BUFFER, ibo_quad);
    gl->DrawElements(ctx, GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, (const void *)0);
    /* vertex-coloured triangle on top: DrawArrays */
    Uniform1f(loc_mix, F_0);
    setup_attribs(vbo_tri);
    gl->DrawArrays(ctx, GL_TRIANGLES, 0, 3);
}

static void read_px(int x, int y, uint8_t *o) { o[0] = o[1] = o[2] = o[3] = 7; gl->ReadPixels(ctx, x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, o); }

static void swap_cb(void *user, int32_t result);

/* GL from another guest thread (Flash renders on its own thread): draw the same picture and swap with a BLOCKING callback. */
static void *gl_thread(void *arg) {
    uint8_t p[4];
    struct PP_CompletionCallback block = { 0, 0, PP_COMPLETIONCALLBACK_FLAG_NONE };   /* == PP_BlockUntilComplete() */
    int32_t r;
    /* A second context that SHARES the first one's objects (program, buffers, texture) but has its own, smaller default
     * framebuffer: draw the same scene into it, put it on screen first, then go back to the primary. */
    PP_Resource primary = ctx;
    int32_t a2[] = { PP_GRAPHICS3DATTRIB_ALPHA_SIZE, 8, PP_GRAPHICS3DATTRIB_DEPTH_SIZE, 24, PP_GRAPHICS3DATTRIB_WIDTH, 32, PP_GRAPHICS3DATTRIB_HEIGHT, 32, PP_GRAPHICS3DATTRIB_NONE };
    PP_Resource c2 = g3d->Create(g_inst, primary, a2);
    if (c2) {
        ctx = c2;
        draw_frame(32);
        read_px(8, 8, p); SAY("shared bl=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
        read_px(24, 8, p); SAY("shared br=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
        inst->BindGraphics(g_inst, c2);
        r = g3d->SwapBuffers(c2, block); SAY("shared swap result=%d", r);
        ctx = primary;
        inst->BindGraphics(g_inst, primary);
        core->ReleaseResource(c2);
    } else SAY("shared create failed");
    draw_frame(64);
    read_px(16, 16, p); SAY("thread quad bl=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
    r = g3d->SwapBuffers(ctx, block);
    SAY("thread swap result=%d", r);
    SAY("gl-test done");
    return 0;
}
static void do_swap(void) {
    struct PP_CompletionCallback cb = { swap_cb, 0, PP_COMPLETIONCALLBACK_FLAG_NONE };
    int32_t r = g3d->SwapBuffers(ctx, cb);
    if (r != PP_OK_COMPLETIONPENDING) SAY("swap start result=%d", r);
}
static void swap_cb(void *user, int32_t result) {
    SAY("swap done frame=%d result=%d", frames, result);
    if (++frames < 3) { draw_frame(64); do_swap(); }
    else {
        pthread_t th;
        int rc = pthread_create(&th, 0, gl_thread, 0);
        if (rc != 0) { SAY("thread create failed rc=%d", rc); SAY("gl-test done"); }
    }
}

static void run_tests(void) {
    char b[200]; int32_t n;
    int32_t attribs[] = { PP_GRAPHICS3DATTRIB_ALPHA_SIZE, 8, PP_GRAPHICS3DATTRIB_DEPTH_SIZE, 24, PP_GRAPHICS3DATTRIB_STENCIL_SIZE, 8,
                          PP_GRAPHICS3DATTRIB_WIDTH, 64, PP_GRAPHICS3DATTRIB_HEIGHT, 64, PP_GRAPHICS3DATTRIB_NONE };
    ctx = g3d->Create(g_inst, 0, attribs);
    SAY("g3d create ok=%d valid=%d", ctx != 0, ctx ? g3d->IsGraphics3D(ctx) : 0);
    if (!ctx) { SAY("gl-test done"); return; }
    int32_t q[] = { PP_GRAPHICS3DATTRIB_ALPHA_SIZE, 0, PP_GRAPHICS3DATTRIB_DEPTH_SIZE, 0, PP_GRAPHICS3DATTRIB_STENCIL_SIZE, 0,
                    PP_GRAPHICS3DATTRIB_WIDTH, 0, PP_GRAPHICS3DATTRIB_HEIGHT, 0, PP_GRAPHICS3DATTRIB_NONE };
    n = g3d->GetAttribs(ctx, q);
    SAY("attribs r=%d alpha=%d depth=%d stencil=%d w=%d h=%d", n, q[1], q[3], q[5], q[7], q[9]);
    SAY("bind=%d", inst->BindGraphics(g_inst, ctx));

    const char *s;
    s = (const char *)gl->GetString(ctx, GL_VERSION);   SAY("version=%s", s ? s : "(null)");
    s = (const char *)gl->GetString(ctx, GL_SHADING_LANGUAGE_VERSION); SAY("glsl=%s", s ? s : "(null)");
    s = (const char *)gl->GetString(ctx, GL_VENDOR);    SAY("vendor=%s", s ? s : "(null)");
    s = (const char *)gl->GetString(ctx, GL_EXTENSIONS);
    SAY("ext derivatives=%d bgra=%d packed_ds=%d instanced=%d occlusion=%d vao=%d map_sub=%d len=%d",
        s && strstr(s, "GL_OES_standard_derivatives") != 0, s && strstr(s, "GL_EXT_texture_format_BGRA8888") != 0,
        s && strstr(s, "GL_OES_packed_depth_stencil") != 0, s && strstr(s, "GL_ANGLE_instanced_arrays") != 0,
        s && strstr(s, "GL_EXT_occlusion_query_boolean") != 0, s && strstr(s, "GL_OES_vertex_array_object") != 0,
        s && strstr(s, "GL_CHROMIUM_map_sub") != 0, s ? (int)strlen(s) : 0);
    GLint v[4] = { 0, 0, 0, 0 };
    gl->GetIntegerv(ctx, GL_MAX_TEXTURE_SIZE, v); SAY("maxtex_ok=%d", v[0] >= 2048);
    gl->GetIntegerv(ctx, GL_VIEWPORT, v); SAY("viewport=%d,%d,%d,%d", v[0], v[1], v[2], v[3]);

    /* a shader that does not compile: the info log must say why */
    int ok; char log[160];
    GLuint bad = compile(GL_FRAGMENT_SHADER, BADFS, &ok, log, sizeof log);
    (void)bad;
    SAY("badshader status=%d log_has_identifier=%d", ok, strstr(log, "undefined_thing") != 0);

    int vok, fok;
    GLuint vs = compile(GL_VERTEX_SHADER, VS, &vok, 0, 0), fs = compile(GL_FRAGMENT_SHADER, FS, &fok, 0, 0);
    prog = gl->CreateProgram(ctx);
    gl->AttachShader(ctx, prog, vs); gl->AttachShader(ctx, prog, fs);
    gl->BindAttribLocation(ctx, prog, 0, "a_pos"); gl->BindAttribLocation(ctx, prog, 1, "a_uv"); gl->BindAttribLocation(ctx, prog, 2, "a_col");
    gl->LinkProgram(ctx, prog);
    GLint linked = 0; gl->GetProgramiv(ctx, prog, GL_LINK_STATUS, &linked);
    loc_mix = gl->GetUniformLocation(ctx, prog, "u_mix"); loc_tex = gl->GetUniformLocation(ctx, prog, "u_tex"); loc_off = gl->GetUniformLocation(ctx, prog, "u_off");
    SAY("program vs=%d fs=%d linked=%d locs_ok=%d", vok, fok, linked, loc_mix >= 0 && loc_tex >= 0 && loc_off >= 0);
    GLint nu = 0, na = 0; gl->GetProgramiv(ctx, prog, 0x8B86, &nu); gl->GetProgramiv(ctx, prog, 0x8B89, &na);
    GLsizei len = 0, size = 0; GLenum type = 0; char name[32];
    gl->GetActiveAttrib(ctx, prog, 0, sizeof name, &len, &size, &type, name);
    SAY("active uniforms=%d attribs=%d first_attrib_len=%d type_ok=%d", nu, na, len, type == 0x8B50 || type == 0x8B52);
    gl->UseProgram(ctx, prog);

    gl->GenBuffers(ctx, 1, &vbo_tri); gl->BindBuffer(ctx, GL_ARRAY_BUFFER, vbo_tri); gl->BufferData(ctx, GL_ARRAY_BUFFER, sizeof tri_verts, tri_verts, GL_STATIC_DRAW);
    gl->GenBuffers(ctx, 1, &vbo_quad); gl->BindBuffer(ctx, GL_ARRAY_BUFFER, vbo_quad); gl->BufferData(ctx, GL_ARRAY_BUFFER, sizeof quad_verts, quad_verts, GL_STATIC_DRAW);
    gl->GenBuffers(ctx, 1, &ibo_quad); gl->BindBuffer(ctx, GL_ELEMENT_ARRAY_BUFFER, ibo_quad); gl->BufferData(ctx, GL_ELEMENT_ARRAY_BUFFER, sizeof quad_idx, quad_idx, GL_STATIC_DRAW);
    gl->GenTextures(ctx, 1, &tex2x2); gl->ActiveTexture(ctx, GL_TEXTURE0); gl->BindTexture(ctx, GL_TEXTURE_2D, tex2x2);
    gl->TexImage2D(ctx, GL_TEXTURE_2D, 0, GL_RGBA, 2, 2, 0, GL_RGBA, GL_UNSIGNED_BYTE, tex_px);
    gl->TexParameteri(ctx, GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST); gl->TexParameteri(ctx, GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    gl->TexParameteri(ctx, GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE); gl->TexParameteri(ctx, GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);

    /* the final picture, read back from the default framebuffer before it is swapped */
    draw_frame(64);
    uint8_t p[4];
    read_px(16, 16, p); SAY("quad bl=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
    read_px(48, 16, p); SAY("quad br=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
    read_px(16, 48, p); SAY("quad tl=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
    read_px(48, 48, p); SAY("quad tr=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);
    read_px(32, 28, p); SAY("tri center=%d,%d,%d,%d", p[0], p[1], p[2], p[3]);

    /* framebuffer object with a texture colour attachment */
    GLuint fbo = 0, ftex = 0;
    gl->GenTextures(ctx, 1, &ftex); gl->BindTexture(ctx, GL_TEXTURE_2D, ftex);
    gl->TexImage2D(ctx, GL_TEXTURE_2D, 0, GL_RGBA, 8, 8, 0, GL_RGBA, GL_UNSIGNED_BYTE, 0);
    gl->GenFramebuffers(ctx, 1, &fbo); gl->BindFramebuffer(ctx, GL_FRAMEBUFFER, fbo);
    gl->FramebufferTexture2D(ctx, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, ftex, 0);
    GLenum fst = gl->CheckFramebufferStatus(ctx, GL_FRAMEBUFFER);
    gl->Viewport(ctx, 0, 0, 8, 8); ClearColor(F_0, F_1, F_0, F_1); gl->Clear(ctx, GL_COLOR_BUFFER_BIT);
    read_px(4, 4, p); SAY("fbo status=%x px=%d,%d,%d,%d", fst, p[0], p[1], p[2], p[3]);
    gl->BindFramebuffer(ctx, GL_FRAMEBUFFER, 0); gl->BindTexture(ctx, GL_TEXTURE_2D, tex2x2);

    /* error flag semantics */
    SAY("err_before=%x", gl->GetError(ctx));
    gl->Enable(ctx, 0x1234); gl->Viewport(ctx, 0, 0, (GLsizei)-1, 5);
    SAY("err_first=%x", gl->GetError(ctx)); SAY("err_cleared=%x", gl->GetError(ctx));
    SAY("g3d_error=%d resize=%d", g3d->GetError(ctx), g3d->ResizeBuffers(ctx, 64, 64));

    draw_frame(64);
    do_swap();
}

static PP_Bool DidCreate(PP_Instance i, uint32_t argc, const char *argn[], const char *argv[]) { g_inst = i; say("didcreate"); return PP_TRUE; }
static void DidDestroy(PP_Instance i) {}
static void DidChangeView(PP_Instance i, PP_Resource v) {
    if (started) return;
    started = 1;
    run_tests();
}
static void DidChangeFocus(PP_Instance i, PP_Bool f) {}
static PP_Bool HandleDocumentLoad(PP_Instance i, PP_Resource l) { return PP_FALSE; }
static const PPP_Instance_1_1 instance_iface = { DidCreate, DidDestroy, DidChangeView, DidChangeFocus, HandleDocumentLoad };
static void HandleMessage(PP_Instance i, struct PP_Var m) {}
static const PPP_Messaging_1_0 messaging_iface = { HandleMessage };

int32_t PPP_InitializeModule(PP_Module m, PPB_GetInterface get) {
    core = get(PPB_CORE_INTERFACE_1_0); var = get(PPB_VAR_INTERFACE_1_2); msg = get(PPB_MESSAGING_INTERFACE_1_0);
    inst = get(PPB_INSTANCE_INTERFACE_1_0); view = get(PPB_VIEW_INTERFACE_1_1);
    g3d = get(PPB_GRAPHICS_3D_INTERFACE_1_0); gl = get(PPB_OPENGLES2_INTERFACE_1_0);
    return (core && var && msg && inst && view && g3d && gl) ? PP_OK : PP_ERROR_NOINTERFACE;
}
void PPP_ShutdownModule(void) {}
const void *PPP_GetInterface(const char *name) {
    if (!strcmp(name, PPP_INSTANCE_INTERFACE_1_1)) return &instance_iface;
    if (!strcmp(name, PPP_MESSAGING_INTERFACE_1_0)) return &messaging_iface;
    return 0;
}
