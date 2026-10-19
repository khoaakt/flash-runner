/* pepper-host: the only native code of ours that runs inside the guest.
 *
 * It dlopen()s a PPAPI plugin (libpepflashplayer.so), hands it the PPB interface tables (generated
 * stubs that trap to the JS embedder), and then turns this thread into a task loop: the embedder
 * feeds it "call this function pointer with these words" tasks (PPP_* calls, completion callbacks).
 * Plugin-created message loops reuse the same loop primitive. See hc.h for the host-call ABI.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <pthread.h>
#include "hc.h"
#include "ppb_table.h"

typedef const void *(*get_interface_t)(const char *);
typedef int32_t (*init_module_t)(int32_t module, get_interface_t get);
typedef const void *(*ppp_get_interface_t)(const char *);

static const void *get_browser_interface(const char *name) {
    for (const struct ppb_iface *i = ppb_ifaces; i->name; i++)
        if (!strcmp(i->name, name)) {
            return i->table;
        }
    /* A few versions Flash probes for are aliases of an interface we already provide. */
    if (!strcmp(name, "PPB_FlashFullscreen;0.1")) return get_browser_interface("PPB_FlashFullscreen;1.0");
    if (!strcmp(name, "PPB_OpenGLES2ChromiumMapSub(Dev);1.0")) return get_browser_interface("PPB_OpenGLES2ChromiumMapSub;1.0");
    char msg[200];
    int n = snprintf(msg, sizeof msg, "get_browser_interface: unknown '%s'", name);
    hc5(HC_LOG, (uint32_t)msg, n, 0, 0, 0);
    return NULL;
}

typedef uint64_t (*w0)(void);
typedef uint64_t (*w1)(uint32_t);
typedef uint64_t (*w2)(uint32_t, uint32_t);
typedef uint64_t (*w3)(uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w4)(uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w5)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w6)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w7)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w8)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w9)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w10)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef uint64_t (*w14)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);

static uint64_t call_words(uint32_t fn, const uint32_t *a, uint32_t n) {
    switch (n) {
    case 0: return ((w0)fn)();
    case 1: return ((w1)fn)(a[0]);
    case 2: return ((w2)fn)(a[0], a[1]);
    case 3: return ((w3)fn)(a[0], a[1], a[2]);
    case 4: return ((w4)fn)(a[0], a[1], a[2], a[3]);
    case 5: return ((w5)fn)(a[0], a[1], a[2], a[3], a[4]);
    case 6: return ((w6)fn)(a[0], a[1], a[2], a[3], a[4], a[5]);
    case 7: return ((w7)fn)(a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
    case 8: return ((w8)fn)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
    case 9: return ((w9)fn)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
    case 10: return ((w10)fn)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9]);
    default: return ((w14)fn)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13]);
    }
}

/* Plugin functions that return a PP_Var by value (PPP_Class_Deprecated::GetProperty/Call, PPP_Instance_Private::
 * GetInstanceObject): the i386 ABI passes a hidden result pointer and the callee pops it, so they have to be called
 * through a correctly typed pointer. a[0] is where the result is stored for the embedder; a[1..] are the real arguments. */
typedef struct { uint32_t type, pad, lo, hi; } pp_var_t;
typedef pp_var_t (*v0)(void);
typedef pp_var_t (*v1)(uint32_t);
typedef pp_var_t (*v2)(uint32_t, uint32_t);
typedef pp_var_t (*v3)(uint32_t, uint32_t, uint32_t);
typedef pp_var_t (*v4)(uint32_t, uint32_t, uint32_t, uint32_t);
typedef pp_var_t (*v5)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef pp_var_t (*v6)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef pp_var_t (*v7)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);
typedef pp_var_t (*v8)(uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t, uint32_t);

static void call_var(uint32_t fn, const uint32_t *a, uint32_t n) {
    pp_var_t r = {0, 0, 0, 0};
    switch (n - 1) {
    case 0: r = ((v0)fn)(); break;
    case 1: r = ((v1)fn)(a[1]); break;
    case 2: r = ((v2)fn)(a[1], a[2]); break;
    case 3: r = ((v3)fn)(a[1], a[2], a[3]); break;
    case 4: r = ((v4)fn)(a[1], a[2], a[3], a[4]); break;
    case 5: r = ((v5)fn)(a[1], a[2], a[3], a[4], a[5]); break;
    case 6: r = ((v6)fn)(a[1], a[2], a[3], a[4], a[5], a[6]); break;
    case 7: r = ((v7)fn)(a[1], a[2], a[3], a[4], a[5], a[6], a[7]); break;
    case 8: r = ((v8)fn)(a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]); break;
    }
    memcpy((void *)a[0], &r, sizeof r);
}

/* Run tasks for `loop` until the embedder says quit. Used for the main loop and by PPB_MessageLoop::Run. */
uint32_t pepper_run_loop(uint32_t loop) {
    task_t t;
    uint32_t prev_id = 0, prev_lo = 0, prev_hi = 0;
    for (;;) {
        uint32_t r = hc5(HC_NEXT_TASK, loop, (uint32_t)&t, prev_id, prev_lo, prev_hi);
        if (r != 0) {
            return r;
        }
        uint64_t ret = 0;
        if (t.nargs & 0x80000000u) call_var(t.fn, t.args, t.nargs & 0xffff);
        else ret = call_words(t.fn, t.args, t.nargs);
        prev_id = t.id; prev_lo = (uint32_t)ret; prev_hi = (uint32_t)(ret >> 32);
    }
}

/* PPB_MessageLoop::Run: executes this thread's loop until PostQuit. */
int32_t pepper_ml_run(int32_t loop) {
    pepper_run_loop((uint32_t)loop);
    return 0;
}

/* PPB_Flash_MessageLoop::Run: nested loop on the calling thread until Quit. */
int32_t pepper_flash_ml_run(int32_t loop) {
    pepper_run_loop((uint32_t)loop);
    return 0;
}

/* Diagnostics (FW_MAPS_SEC=n): every n seconds log a summary of /proc/self/maps so a guest running out of memory can be explained. */
static void *maps_thread(void *arg) {
    unsigned sec = (unsigned)(size_t)arg;
    for (;;) {
        sleep(sec);
        FILE *f = fopen("/proc/self/maps", "r");
        if (!f) continue;
        char line[256];
        unsigned long tot = 0, rw = 0, none = 0, ro = 0, ex = 0; int n = 0;
        struct { unsigned long sz; char perm[5]; char name[40]; } top[6]; memset(top, 0, sizeof top);
        struct { unsigned long sz, a, b; char name[40]; } topx[6]; memset(topx, 0, sizeof topx);
        while (fgets(line, sizeof line, f)) {
            unsigned long a, b; char perm[8] = {0}; char name[64] = {0};
            if (sscanf(line, "%lx-%lx %7s %*s %*s %*s %63s", &a, &b, perm, name) < 3) continue;
            unsigned long sz = b - a; tot += sz; n++;
            if (perm[0] == '-' && perm[1] == '-' && perm[2] == '-') none += sz; else if (perm[2] == 'x') ex += sz; else if (perm[1] == 'w') rw += sz; else ro += sz;
            for (int i = 0; i < 6; i++) if (sz > top[i].sz) { for (int j = 5; j > i; j--) top[j] = top[j - 1]; top[i].sz = sz; memcpy(top[i].perm, perm, 4); top[i].perm[4] = 0; strncpy(top[i].name, name, 39); break; }
            /* executable regions with their addresses: lets a host-side profiler attribute sampled
               guest addresses to plugin / libc / the runtime's own JIT code cache */
            if (perm[2] == 'x') for (int i = 0; i < 6; i++) if (sz > topx[i].sz) { for (int j = 5; j > i; j--) topx[j] = topx[j - 1]; topx[i].sz = sz; topx[i].a = a; topx[i].b = b; strncpy(topx[i].name, name, 39); break; }
        }
        fclose(f);
        char msg[768]; int m = snprintf(msg, sizeof msg, "[maps] mapped=%luMB rw=%luMB none=%luMB ro=%luMB x=%luMB regions=%d | largest:", tot >> 20, rw >> 20, none >> 20, ro >> 20, ex >> 20, n);
        for (int i = 0; i < 6 && top[i].sz && m < 440; i++) m += snprintf(msg + m, sizeof msg - m, " %luMB/%s/%s", top[i].sz >> 20, top[i].perm, top[i].name);
        m += snprintf(msg + m, sizeof msg - m, " | xtop:");
        for (int i = 0; i < 6 && topx[i].sz && m < 680; i++) m += snprintf(msg + m, sizeof msg - m, " %lx-%lx %luMB/%s", topx[i].a, topx[i].b, topx[i].sz >> 20, topx[i].name[0] ? topx[i].name : "anon");
        hc5(HC_LOG, (uint32_t)msg, m, 0, 0, 0);
    }
    return NULL;
}

/* Dedicated thread for PPB_Audio callbacks (message loop 2 in the embedder). */
static void *audio_thread(void *arg) { pepper_run_loop(2); return 0; }

int main(int argc, char **argv) {
    static char arena_scratch[4096];
    /* The arena backs ImageData/Var buffers; Flash keeps several full-stage ImageData alive (2.4 MB each at 1000x600), so 16 MB ran out
       in big games. The embedder sizes it through FW_ARENA_MB (untouched pages cost nothing). */
    const char *amb = getenv("FW_ARENA_MB");
    size_t arena_size = (size_t)(amb && atoi(amb) >= 16 && atoi(amb) <= 512 ? atoi(amb) : 16) << 20;
    void *arena = malloc(arena_size);
    uint32_t module = hc5(HC_INIT, (uint32_t)arena, (uint32_t)arena_size, (uint32_t)arena_scratch, 0, 0);

    char path[512];
    uint32_t n = hc5(HC_PLUGIN_ARG, 0, (uint32_t)path, sizeof path - 1, 0, 0);
    path[n] = 0;
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        char msg[600];
        int m = snprintf(msg, sizeof msg, "dlopen(%s) failed: %s", path, dlerror());
        hc5(HC_LOG, (uint32_t)msg, m, 0, 0, 0);
        return 2;
    }
    { FILE *mf = fopen("/proc/self/maps", "r"); char l[300]; char first[64] = ""; unsigned long lo = ~0ul, hi = 0;   /* where the plugin ended up: lets host-side profiles map addresses back to its code */
      while (mf && fgets(l, sizeof l, mf)) if (strstr(l, "plugin.so") || strstr(l, "libpepflash")) { unsigned long a, b; if (sscanf(l, "%lx-%lx", &a, &b) == 2) { if (a < lo) lo = a; if (b > hi) hi = b; } }
      if (mf) fclose(mf);
      if (hi) { char m[96]; int n = snprintf(m, sizeof m, "plugin mapped %08lx-%08lx", lo, hi); hc5(HC_LOG, (uint32_t)m, n, 0, 0, 0); } (void)first; }
    init_module_t init = (init_module_t)dlsym(h, "PPP_InitializeModule");
    ppp_get_interface_t getif = (ppp_get_interface_t)dlsym(h, "PPP_GetInterface");
    if (!init || !getif) {
        const char *m = "plugin lacks PPP_InitializeModule/PPP_GetInterface";
        hc5(HC_LOG, (uint32_t)m, strlen(m), 0, 0, 0);
        return 3;
    }
    /* A synchronous boundary while all plugin code is mapped and relocated, but before
       PPP_InitializeModule/guest audio threads. The embedder can install verified static
       translations without racing the scheduler or guessing a /proc/maps address. */
    { Dl_info info;
      if (dladdr((void *)init, &info) && info.dli_fbase)
          hc5(HC_PLUGIN_LOADED, (uint32_t)info.dli_fbase, (uint32_t)init, (uint32_t)getif, 0, 0); }
    pthread_t at; pthread_create(&at, 0, audio_thread, 0);
    { const char *ms = getenv("FW_MAPS_SEC"); if (ms && atoi(ms) > 0) { pthread_t mt; pthread_create(&mt, 0, maps_thread, (void *)(size_t)atoi(ms)); } }
    int32_t rc = init((int32_t)module, get_browser_interface);
    hc5(HC_READY, (uint32_t)getif, (uint32_t)rc, 0, 0, 0);
    if (rc != 0) return 4;
    pepper_run_loop(1 /* main loop resource id reserved by the embedder */);
    return 0;
}
