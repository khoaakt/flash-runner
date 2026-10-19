// flags: -msse2
// Legacy-SSE 16-byte memory operands must be 16-byte aligned (#GP -> SIGSEGV); movu*/scalar/64-bit forms must not fault.
#include <signal.h>
#include <setjmp.h>
#include "sse_common.h"

static sigjmp_buf jb;
static volatile int faulted;
static void handler(int sig) { faulted = sig; siglongjmp(jb, 1); }

static unsigned char mem[128] __attribute__((aligned(64)));

#define TRY(name, stmt) \
    do { \
        faulted = 0; \
        if (!sigsetjmp(jb, 1)) { stmt; } \
        printf("%-34s %s\n", name, faulted ? "SIGSEGV" : "ok"); \
    } while (0)

#define LDOP(insn, off) asm volatile(insn " %0, %%xmm1" :: "m"(*(volatile char (*)[16])(mem + (off))) : "xmm1")
#define LDOP64(insn, off) asm volatile(insn " %0, %%xmm1" :: "m"(*(volatile char (*)[8])(mem + (off))) : "xmm1")
#define LDOP32(insn, off) asm volatile(insn " %0, %%xmm1" :: "m"(*(volatile char (*)[4])(mem + (off))) : "xmm1")
#define STOP(insn, off) asm volatile(insn " %%xmm1, %0" : "=m"(*(volatile char (*)[16])(mem + (off))) :: "memory")
#define LDIMM(insn, off) asm volatile(insn " $27, %0, %%xmm1" :: "m"(*(volatile char (*)[16])(mem + (off))) : "xmm1")

int main(void) {
    setvbuf(stdout, 0, _IOLBF, 0);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = handler;
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGBUS, &sa, 0);
    memset(mem, 1, sizeof mem);

    // loads at aligned / misaligned offsets
    TRY("movaps load aligned", LDOP("movaps", 16));
    TRY("movaps load +1", LDOP("movaps", 17));
    TRY("movaps load +8", LDOP("movaps", 24));
    TRY("movaps load +15", LDOP("movaps", 31));
    TRY("movapd load +4", LDOP("movapd", 20));
    TRY("movdqa load aligned", LDOP("movdqa", 32));
    TRY("movdqa load +2", LDOP("movdqa", 34));
    TRY("movups load +1", LDOP("movups", 17));
    TRY("movupd load +3", LDOP("movupd", 19));
    TRY("movdqu load +7", LDOP("movdqu", 23));
    // stores
    TRY("movaps store aligned", STOP("movaps", 48));
    TRY("movaps store +4", STOP("movaps", 52));
    TRY("movapd store +8", STOP("movapd", 56));
    TRY("movdqa store +1", STOP("movdqa", 49));
    TRY("movups store +1", STOP("movups", 49));
    TRY("movdqu store +9", STOP("movdqu", 57));
    TRY("movntps store aligned", STOP("movntps", 64));
    TRY("movntps store +4", STOP("movntps", 68));
    TRY("movntpd store +8", STOP("movntpd", 72));
    TRY("movntdq store +1", STOP("movntdq", 65));
    // ALU ops with m128 operand
    TRY("addps m128 aligned", LDOP("addps", 16));
    TRY("addps m128 +4", LDOP("addps", 20));
    TRY("mulpd m128 +8", LDOP("mulpd", 24));
    TRY("paddb m128 +1", LDOP("paddb", 17));
    TRY("pxor m128 aligned", LDOP("pxor", 16));
    TRY("pxor m128 +2", LDOP("pxor", 18));
    TRY("andps m128 +4", LDOP("andps", 20));
    TRY("punpcklbw m128 +8", LDOP("punpcklbw", 24));
    TRY("cvtdq2ps m128 +4", LDOP("cvtdq2ps", 20));
    TRY("cvtps2dq m128 +4", LDOP("cvtps2dq", 20));
    TRY("cvtpd2ps m128 +8", LDOP("cvtpd2ps", 24));
    TRY("sqrtps m128 +4", LDOP("sqrtps", 20));
    TRY("cmpltps m128 +4", LDOP("cmpltps", 20));
    TRY("pshufd m128 +4", LDIMM("pshufd", 20));
    TRY("pshufd m128 aligned", LDIMM("pshufd", 16));
    TRY("pshuflw m128 +4", LDIMM("pshuflw", 20));
    TRY("shufps m128 +4", LDIMM("shufps", 20));
    // (psllw xmm, m128 at a misaligned address: real hardware faults, QEMU does not -> not tested)
    TRY("psllq m128 aligned", LDOP("psllq", 16));
    // scalar / 64-bit / 32-bit memory operands never fault on alignment
    TRY("addss m32 +1", LDOP32("addss", 17));
    TRY("addsd m64 +1", LDOP64("addsd", 17));
    TRY("movss m32 +3", LDOP32("movss", 19));
    TRY("movsd m64 +5", LDOP64("movsd", 21));
    TRY("movlps m64 +1", LDOP64("movlps", 17));
    TRY("movhpd m64 +3", LDOP64("movhpd", 19));
    TRY("cvtps2pd m64 +1", LDOP64("cvtps2pd", 17));
    TRY("cvtdq2pd m64 +2", LDOP64("cvtdq2pd", 18));
    TRY("ucomiss m32 +1", LDOP32("ucomiss", 17));
    TRY("comisd m64 +1", LDOP64("comisd", 17));
    TRY("cvtss2sd m32 +1", LDOP32("cvtss2sd", 17));
    TRY("movq m64 +1", LDOP64("movq", 17));
    TRY("movd m32 +1", LDOP32("movd", 17));
    TRY("punpcklbw (movq) +0", LDOP("movdqa", 80));
    return 0;
}
