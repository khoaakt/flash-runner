// Legacy oddities: `enter` with nesting levels, 16-bit-address `jcxz`, `bound` (#BR -> SIGSEGV),
// `arpl` and `icebp` (#DB -> SIGTRAP). Traps are caught with sigaction + siglongjmp; only
// environment-independent facts are printed.
#define _GNU_SOURCE
#include <stdio.h>
#include <signal.h>
#include <setjmp.h>
#include <string.h>

static sigjmp_buf jb;
static volatile int caught;

static void on_trap(int sig) { caught = sig; siglongjmp(jb, 1); }

static void install(int sig) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_trap;
    sa.sa_flags = SA_NODEFER; // the handler longjmps out; re-arm atomically is not needed
    sigaction(sig, &sa, 0);
}

static unsigned stack[64];

int main(void) {
    setbuf(stdout, NULL);
    install(SIGSEGV);
    install(SIGTRAP);

    // ---- enter $16, $2 on a scratch stack with a known frame chain ----
    // ebp -> chain+2, so [ebp-4] = 0xBBBBBBBB is the frame pointer that gets copied.
    // The asm swaps ebp/esp for the duration of the `enter` and restores both before gcc
    // touches any local again (the function keeps a frame pointer under -g).
    unsigned chain[2] = { 0xaaaaaaaa, 0xbbbbbbbb };
    unsigned ebo, eso, sps, oldebp;
    __asm__ ("movl %%esp, %0" : "=r"(sps));
    __asm__ ("movl %%ebp, %0\n\t"
             "leal 256(%3), %%esp\n\t"
             "movl %4, %%ebp\n\t"
             "enter $16, $2\n\t"
             "movl %%ebp, %1\n\t"
             "movl %%esp, %2\n\t"
             "movl %5, %%esp\n\t"
             "movl %0, %%ebp"
             : "=&r"(oldebp), "=&r"(ebo), "=&r"(eso)
             : "r"(stack), "r"(chain + 2), "r"(sps)
             : "cc", "memory");
    printf("enter16,2: ebp=%ld esp=%ld copy=%08x frame=%ld oldebp=%d\n",
           (long)((char *)ebo - (char *)stack), (long)((char *)eso - (char *)stack),
           stack[62], (long)((char *)(size_t)stack[61] - (char *)stack), stack[63] == (unsigned)(size_t)(chain + 2));

    // ---- enter $4, $0 (the plain form) ----
    __asm__ ("movl %%ebp, %0\n\t"
             "leal 256(%3), %%esp\n\t"
             "movl %4, %%ebp\n\t"
             "enter $4, $0\n\t"
             "movl %%ebp, %1\n\t"
             "movl %%esp, %2\n\t"
             "movl %5, %%esp\n\t"
             "movl %0, %%ebp"
             : "=&r"(oldebp), "=&r"(ebo), "=&r"(eso)
             : "r"(stack), "r"(chain + 2), "r"(sps)
             : "cc", "memory");
    printf("enter4,0: ebp=%ld esp=%ld oldebp=%d\n",
           (long)((char *)ebo - (char *)stack), (long)((char *)eso - (char *)stack), stack[63] == (unsigned)(size_t)(chain + 2));

    // ---- jcxz (67 E3): tests CX only, not ECX ----
    // Only the not-taken case is oracle-tested: on a taken branch the SDM computes the target with
    // a 16-bit IP (which faults anywhere above 64 KB), while qemu-user uses the 32-bit target. The
    // emulator follows the SDM; the wrap is pinned by the Rust unit test
    // jcxz_tests_cx_and_wraps_at_16_bits, and gcc never emits 67 E3 in production code anyway.
    int t;
    unsigned e = 0x00010001; // CX != 0 -> not taken
    __asm__ ("movl %1, %%ecx\n\tjcxz 1f\n\txorl %%eax, %%eax\n\tjmp 2f\n1:\tmovl $1, %%eax\n2:"
             : "=a"(t) : "r"(e) : "ecx", "cc");
    printf("jcxz cx=1: taken=%d\n", t);

    // ---- bound: in range falls through, out of range raises #BR (SIGSEGV on Linux/i386) ----
    int bounds[2] = { 5, 10 };
    int idx = 7;
    __asm__ volatile ("bound %0, %1" :: "r"(idx), "m"(bounds));
    printf("bound 7 in [5,10]: ok\n");
    for (int bad = 4; bad <= 11; bad += 7) {
        idx = bad;
        caught = 0;
        if (sigsetjmp(jb, 1) == 0) {
            __asm__ volatile ("bound %0, %1" :: "r"(idx), "m"(bounds));
            printf("bound %d: NO TRAP\n", idx);
        } else {
            printf("bound %d: trapped sig=%d\n", idx, caught);
        }
    }

    // ---- arpl: raise the RPL of the destination selector, ZF when it changed ----
    for (unsigned pair = 0; pair < 2; pair++) {
        unsigned sel = pair ? 0x0103 : 0x0100, src = pair ? 0x0020 : 0x0023, fl;
        __asm__ ("arpl %w2, %w0\n\tlahf\n\tmovzbl %%ah, %1" : "+r"(sel), "=r"(fl) : "r"(src) : "cc");
        printf("arpl -> %04x zf=%d\n", sel, (fl >> 6) & 1);
    }

    // ---- icebp (F1): #DB -> SIGTRAP ----
    caught = 0;
    if (sigsetjmp(jb, 1) == 0) {
        __asm__ volatile (".byte 0xf1");
        printf("icebp: NO TRAP\n");
    } else {
        printf("icebp: trapped sig=%d\n", caught);
    }
    return 0;
}
