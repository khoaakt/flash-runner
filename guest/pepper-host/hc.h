/* Host-call (hypercall) ABI between pepper-host / PPB stubs and the JavaScript embedder.
 *
 *   eax >= 0x10000  : service call; arguments in ebx, ecx, edx, esi, edi; result in eax (edx:eax for 64-bit)
 *   eax <  0x10000  : PPB method id (see web/ppapi-sigs.js); arguments are the live cdecl frame at
 *                     [esp+4...], result convention described in tools/gen-ppapi.py.
 *
 * Executed with `int $0x82`; the emulator hands control to JS, which resumes the thread when done.
 */
#ifndef HC_H
#define HC_H
#include <stdint.h>

#define HC_LOG        0x10000  /* ebx=ptr ecx=len */
#define HC_NEXT_TASK  0x10001  /* ebx=loop resource ecx=task_t* edx=prev_id esi=prev_lo edi=prev_hi -> 0 task, 1 quit */
#define HC_INIT       0x10002  /* ebx=arena ptr ecx=arena size edx=scratch ptr (4 KiB) -> host module id */
#define HC_READY      0x10003  /* ebx=PPP_GetInterface fn ecx=PPP_InitializeModule result */
#define HC_PLUGIN_ARG 0x10004  /* ebx=index ecx=buf ptr edx=buf len -> length (plugin path/argv delivered by host) */
#define HC_THREAD     0x10005  /* ebx=0 attach-loop-to-this-thread request (loop resource in ecx) */
#define HC_PLUGIN_LOADED 0x10006 /* ebx=ELF load bias ecx=PPP_InitializeModule edx=PPP_GetInterface, before init */

#define TASK_MAX_ARGS 14
typedef struct {
    uint32_t id;
    uint32_t fn;
    uint32_t nargs;
    uint32_t args[TASK_MAX_ARGS];
    uint32_t post_addr, post_len;   /* the host reads this guest range when the task completes (the audio buffer) */
} task_t;

static inline uint32_t hc5(uint32_t nr, uint32_t a, uint32_t b, uint32_t c, uint32_t d, uint32_t e) {
    uint32_t r;
    __asm__ volatile("int $0x82" : "=a"(r) : "a"(nr), "b"(a), "c"(b), "d"(c), "S"(d), "D"(e) : "memory", "cc");
    return r;
}
#endif
