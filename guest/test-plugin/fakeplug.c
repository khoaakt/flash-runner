/* A tiny PPAPI plugin used to test the pepper bridge end to end without the real Flash runtime:
 * paints an animated gradient with Graphics2D, round-trips a string through PPB_Var/Messaging,
 * schedules work with CallOnMainThread, and reports progress with PostMessage. */
#include <stdint.h>
#include <string.h>
#include <stdio.h>
#include "ppapi/c/pp_errors.h"
#include "ppapi/c/pp_module.h"
#include "ppapi/c/pp_var.h"
#include "ppapi/c/ppb.h"
#include "ppapi/c/ppb_core.h"
#include "ppapi/c/ppb_graphics_2d.h"
#include "ppapi/c/ppb_image_data.h"
#include "ppapi/c/ppb_instance.h"
#include "ppapi/c/ppb_messaging.h"
#include "ppapi/c/ppb_var.h"
#include "ppapi/c/ppb_view.h"
#include "ppapi/c/ppb_input_event.h"
#include "ppapi/c/ppb_url_loader.h"
#include "ppapi/c/ppb_url_request_info.h"
#include "ppapi/c/ppb_url_response_info.h"
#include "ppapi/c/ppb_audio.h"
#include "ppapi/c/ppb_audio_config.h"
#include "ppapi/c/ppp.h"
#include "ppapi/c/ppp_instance.h"
#include "ppapi/c/ppp_input_event.h"
#include "ppapi/c/ppp_messaging.h"

typedef struct PPB_Core_1_0 PPB_Core_1_0; typedef struct PPB_Var_1_2 PPB_Var_1_2; typedef struct PPB_Messaging_1_0 PPB_Messaging_1_0;
typedef struct PPB_Instance_1_0 PPB_Instance_1_0; typedef struct PPB_View_1_1 PPB_View_1_1; typedef struct PPB_Graphics2D_1_1 PPB_Graphics2D_1_1;
typedef struct PPB_ImageData_1_0 PPB_ImageData_1_0; typedef struct PPB_InputEvent_1_0 PPB_InputEvent_1_0; typedef struct PPB_MouseInputEvent_1_1 PPB_MouseInputEvent_1_1;
typedef struct PPP_Instance_1_1 PPP_Instance_1_1; typedef struct PPP_InputEvent_0_1 PPP_InputEvent_0_1; typedef struct PPP_Messaging_1_0 PPP_Messaging_1_0;
typedef struct PPB_URLLoader_1_0 PPB_URLLoader_1_0; typedef struct PPB_URLRequestInfo_1_0 PPB_URLRequestInfo_1_0; typedef struct PPB_URLResponseInfo_1_0 PPB_URLResponseInfo_1_0;
static const PPB_URLLoader_1_0 *urlloader; static const PPB_URLRequestInfo_1_0 *urlreq; static const PPB_URLResponseInfo_1_0 *urlresp;
typedef struct PPB_Audio_1_1 PPB_Audio_1_1; typedef struct PPB_AudioConfig_1_1 PPB_AudioConfig_1_1;
static const PPB_Audio_1_1 *audio; static const PPB_AudioConfig_1_1 *aconf; static PP_Resource g_audio; static int audio_cbs;
static PPB_GetInterface get_if;
static const PPB_Core_1_0 *core;
static const PPB_Var_1_2 *var;
static const PPB_Messaging_1_0 *msg;
static const PPB_Instance_1_0 *inst;
static const PPB_View_1_1 *view;
static const PPB_Graphics2D_1_1 *g2d;
static const PPB_ImageData_1_0 *img;
static const PPB_InputEvent_1_0 *inev;
static const PPB_MouseInputEvent_1_1 *mouse;

static PP_Instance g_inst;
static PP_Resource g_g2d, g_img;
static struct PP_Size g_size;
static int frame, key_events, mouse_events;

static void say(const char *s) {
    struct PP_Var v = var->VarFromUtf8(s, (uint32_t)strlen(s));
    msg->PostMessage(g_inst, v);
    var->Release(v);
}

static void audio_cb(void *buf, uint32_t bytes, PP_TimeDelta latency, void *user) {
    int16_t *s = buf; uint32_t frames = bytes / 4; static uint32_t phase;
    for (uint32_t i = 0; i < frames; i++, phase++) { int16_t v = (int16_t)(((phase % 109) < 54 ? 8000 : -8000)); s[2*i] = v; s[2*i+1] = v; }
    if (++audio_cbs == 3) { char b[64]; snprintf(b, sizeof b, "audio cb 3 frames=%u", frames); msg->PostMessage(g_inst, var->VarFromUtf8(b, strlen(b))); }
}
static PP_Resource g_loader; static char g_body[512]; static int g_body_len;
static void read_cb(void *u, int32_t n) {
    if (n > 0) { g_body_len += n; urlloader->ReadResponseBody(g_loader, g_body + g_body_len, (int32_t)sizeof g_body - g_body_len - 1, (struct PP_CompletionCallback){ read_cb, 0, 0 }); return; }
    g_body[g_body_len] = 0; char b[600]; snprintf(b, sizeof b, "url body (%d bytes): %s", g_body_len, g_body); say(b);
}
static void open_cb(void *u, int32_t r) {
    char b[100]; PP_Resource ri = urlloader->GetResponseInfo(g_loader);
    struct PP_Var st = var->VarFromUtf8("", 0); (void)st;
    snprintf(b, sizeof b, "url open result=%d info=%d", r, ri != 0); say(b);
    if (r == 0) urlloader->ReadResponseBody(g_loader, g_body, (int32_t)sizeof g_body - 1, (struct PP_CompletionCallback){ read_cb, 0, 0 });
}
static void fetch_url(const char *url) {
    g_loader = urlloader->Create(g_inst); PP_Resource rq = urlreq->Create(g_inst);
    struct PP_Var u = var->VarFromUtf8(url, (uint32_t)strlen(url));
    urlreq->SetProperty(rq, PP_URLREQUESTPROPERTY_URL, u);
    int32_t r = urlloader->Open(g_loader, rq, (struct PP_CompletionCallback){ open_cb, 0, 0 });
    char b[80]; snprintf(b, sizeof b, "url open started r=%d", r); say(b);
}
static void paint(void);
static void flushed(void *user, int32_t result) {
    char b[96];
    snprintf(b, sizeof b, "flushed frame=%d result=%d user=%d", frame, result, (int)(intptr_t)user);
    say(b);
    if (++frame < 3) core->CallOnMainThread(0, (struct PP_CompletionCallback){ (PP_CompletionCallback_Func)(void *)paint, 0, PP_COMPLETIONCALLBACK_FLAG_NONE }, 0);
}

static void paint_cb(void *u, int32_t r) { paint(); }

static void paint(void) {
    uint32_t *px = (uint32_t *)img->Map(g_img);
    for (int y = 0; y < g_size.height; y++)
        for (int x = 0; x < g_size.width; x++) {
            uint32_t r = (x * 255 / g_size.width + frame * 40) & 255, g = y * 255 / g_size.height, b = (frame * 90) & 255;
            px[y * g_size.width + x] = 0xff000000u | (r << 16) | (g << 8) | b;   /* BGRA premul in memory = 0xAARRGGBB on LE */
        }
    img->Unmap(g_img);
    struct PP_Point origin = { 0, 0 };
    g2d->PaintImageData(g_g2d, g_img, &origin, 0);
    g2d->Flush(g_g2d, (struct PP_CompletionCallback){ flushed, (void *)(intptr_t)frame, PP_COMPLETIONCALLBACK_FLAG_NONE });
}

static PP_Bool DidCreate(PP_Instance i, uint32_t argc, const char *argn[], const char *argv[]) {
    g_inst = i;
    char b[200]; int n = snprintf(b, sizeof b, "didcreate argc=%u", argc);
    for (uint32_t k = 0; k < argc && n < 150; k++) n += snprintf(b + n, sizeof b - n, " %s=%s", argn[k], argv[k]);
    say(b);
#ifdef FAKEPLUG_FP
    snprintf(b, sizeof b, "time_ok=%d", core->GetTime() > 0.0);
    say(b);
#endif
    for (uint32_t k = 0; k < argc; k++) if (!strcmp(argn[k], "fetch")) fetch_url(argv[k]);
    for (uint32_t k = 0; k < argc; k++) if (!strcmp(argn[k], "audio")) {
        uint32_t rate = aconf->RecommendSampleRate(i), fr = aconf->RecommendSampleFrameCount(i, rate, 1024);
        PP_Resource cfg = aconf->CreateStereo16Bit(i, rate, fr);
        g_audio = audio->Create(i, cfg, audio_cb, 0); audio->StartPlayback(g_audio);
        snprintf(b, sizeof b, "audio started rate=%u frames=%u", rate, fr); say(b);
    }
    return PP_TRUE;
}
static void DidDestroy(PP_Instance i) { say("didcreate destroyed"); }
static void DidChangeView(PP_Instance i, PP_Resource v) {
    struct PP_Rect r; view->GetRect(v, &r);
    char b[120]; snprintf(b, sizeof b, "view %dx%d fullscreen=%d visible=%d", r.size.width, r.size.height, view->IsFullscreen(v), view->IsVisible(v));
    say(b);
    if (g_g2d) return;
    g_size = r.size;
    g_g2d = g2d->Create(i, &g_size, PP_FALSE);
    inst->BindGraphics(i, g_g2d);
    g_img = img->Create(i, PP_IMAGEDATAFORMAT_BGRA_PREMUL, &g_size, PP_TRUE);
    core->CallOnMainThread(0, (struct PP_CompletionCallback){ paint_cb, 0, PP_COMPLETIONCALLBACK_FLAG_NONE }, 0);
}
static void DidChangeFocus(PP_Instance i, PP_Bool f) {}
static PP_Bool HandleDocumentLoad(PP_Instance i, PP_Resource l) { return PP_FALSE; }
static const PPP_Instance_1_1 instance_iface = { DidCreate, DidDestroy, DidChangeView, DidChangeFocus, HandleDocumentLoad };

static PP_Bool HandleInputEvent(PP_Instance i, PP_Resource ev) {
    PP_InputEvent_Type t = inev->GetType(ev);
    char b[100];
    if (t == PP_INPUTEVENT_TYPE_MOUSEDOWN || t == PP_INPUTEVENT_TYPE_MOUSEMOVE) {
        struct PP_Point p = mouse->GetPosition(ev);
        snprintf(b, sizeof b, "mouse type=%d at %d,%d button=%d", t, p.x, p.y, mouse->GetButton(ev));
        say(b); mouse_events++;
    } else { snprintf(b, sizeof b, "input type=%d", t); say(b); key_events++; }
    return PP_TRUE;
}
static const PPP_InputEvent_0_1 input_iface = { HandleInputEvent };

static void HandleMessage(PP_Instance i, struct PP_Var m) {
    uint32_t len = 0; const char *s = var->VarToUtf8(m, &len);
    char b[200]; snprintf(b, sizeof b, "echo:%.*s", (int)len, s ? s : "");
    say(b);
}
static const PPP_Messaging_1_0 messaging_iface = { HandleMessage };

int32_t PPP_InitializeModule(PP_Module m, PPB_GetInterface get) {
    get_if = get;
    core = get(PPB_CORE_INTERFACE_1_0); var = get(PPB_VAR_INTERFACE_1_2); msg = get(PPB_MESSAGING_INTERFACE_1_0);
    inst = get(PPB_INSTANCE_INTERFACE_1_0); view = get(PPB_VIEW_INTERFACE_1_1); g2d = get(PPB_GRAPHICS_2D_INTERFACE_1_1);
    img = get(PPB_IMAGEDATA_INTERFACE_1_0); urlloader = get(PPB_URLLOADER_INTERFACE_1_0); audio = get(PPB_AUDIO_INTERFACE_1_1); aconf = get(PPB_AUDIO_CONFIG_INTERFACE_1_1); urlreq = get(PPB_URLREQUESTINFO_INTERFACE_1_0); urlresp = get(PPB_URLRESPONSEINFO_INTERFACE_1_0); inev = get(PPB_INPUT_EVENT_INTERFACE_1_0); mouse = get(PPB_MOUSE_INPUT_EVENT_INTERFACE_1_1);
    return (core && var && msg && inst && view && g2d && img && inev && mouse) ? PP_OK : PP_ERROR_NOINTERFACE;
}
void PPP_ShutdownModule(void) {}
const void *PPP_GetInterface(const char *name) {
    if (!strcmp(name, PPP_INSTANCE_INTERFACE_1_1)) return &instance_iface;
    if (!strcmp(name, PPP_INPUT_EVENT_INTERFACE_0_1)) return &input_iface;
    if (!strcmp(name, PPP_MESSAGING_INTERFACE_1_0)) return &messaging_iface;
    return 0;
}
