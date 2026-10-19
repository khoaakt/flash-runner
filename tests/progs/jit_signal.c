// JIT stress: signal delivery from inside hot loops. Self-sent signals (kill) interrupt compiled code at a syscall;
// SIGSEGV/SIGFPE handlers modify the saved register file / EIP / EFLAGS and the interrupted code must resume
// with exactly that state (registers, lazy flags, resume address).
#define _GNU_SOURCE
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <ucontext.h>
#include <unistd.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0x7e57ab1eu;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }

static void install(int sig, void (*h)(int, siginfo_t *, void *)) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sa.sa_sigaction = h;
    sigaction(sig, &sa, 0);
}
static volatile u32 nsig, seen_efl, seen_eax;
static void on_usr1(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    (void)sig; (void)si;
    nsig++;
    seen_efl = (u32)uc->uc_mcontext.gregs[REG_EFL] & 0x8d5;
    seen_eax = (u32)uc->uc_mcontext.gregs[REG_EAX];
    uc->uc_mcontext.gregs[REG_EDI] += 5;                 // visible to the interrupted code after sigreturn
    uc->uc_mcontext.gregs[REG_ESI] ^= 0x1111;
}
static void on_record(int sig, siginfo_t *si, void *ucv) {       // observes only
    ucontext_t *uc = ucv;
    (void)sig; (void)si;
    nsig++;
    seen_efl = (u32)uc->uc_mcontext.gregs[REG_EFL] & 0x8d5;
}
static void on_usr2(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    (void)sig; (void)si;
    nsig++;
    uc->uc_mcontext.gregs[REG_EFL] |= 1;                 // set CF in the interrupted context
}

// self signal from a hot loop: the syscall exits compiled code, the handler edits EDI/ESI, the loop continues
static u32 t_kill_loop(void) {
    u32 h = 2166136261u;
    nsig = 0;
    install(SIGUSR1, on_usr1);
    u32 pid = getpid();
    for (int i = 0; i < 1500; i++) {
        u32 acc = (u32)i * 3, esi_v = (u32)i;
        __asm__ volatile(
            "movl %[acc], %%edi\n\tmovl %[sv], %%esi\n\t"
            "movl $37, %%eax\n\tmovl %[pid], %%ebx\n\tmovl $10, %%ecx\n\tint $0x80\n\t"   // kill(pid, SIGUSR1)
            "movl %%edi, %[acc]\n\tmovl %%esi, %[sv]"
            : [acc] "+m"(acc), [sv] "+m"(esi_v) : [pid] "m"(pid) : "eax", "ebx", "ecx", "edx", "edi", "esi", "memory", "cc");
        h = mix(mix(h, acc), esi_v);
    }
    return mix(h, nsig);
}

// flags are part of the signal frame: set them with a compare right before the syscall
static u32 t_flags_in_frame(void) {
    u32 h = 2166136261u;
    nsig = 0;
    install(SIGUSR1, on_record);
    u32 pid = getpid();
    for (int i = 0; i < 1500; i++) {
        u32 a = rnd(), b = rnd() >> (i & 3), r;
        __asm__ volatile(
            "cmpl %[b], %[a]\n\t"                                         // arithmetic flags now describe a - b
            "movl $37, %%eax\n\tmovl %[pid], %%ebx\n\tmovl $10, %%ecx\n\tint $0x80\n\t"
            "setc %%al\n\tmovzbl %%al, %0"
            : "=r"(r) : [a] "r"(a), [b] "r"(b), [pid] "m"(pid) : "eax", "ebx", "ecx", "memory", "cc");
        // handler saw the flags of `cmp`, and its EDI/ESI edits are harmless here
        h = mix(mix(h, seen_efl), a < b);
        h = mix(h, r);                                                    // CF after the syscall must be unchanged (a < b)
    }
    return mix(h, nsig);
}

// handler sets CF in the saved flags; the code after the syscall must see it
static u32 t_flags_from_handler(void) {
    u32 h = 2166136261u;
    nsig = 0;
    u32 pid = getpid();
    for (int i = 0; i < 1500; i++) {
        u32 r, x = rnd();
        __asm__ volatile(
            "addl $1, %[x]\n\t"                      // clears CF for x != 0xffffffff
            "clc\n\t"
            "movl $37, %%eax\n\tmovl %[pid], %%ebx\n\tmovl $12, %%ecx\n\tint $0x80\n\t"    // SIGUSR2
            "sbbl %0, %0"                            // r = -CF
            : "=r"(r), [x] "+r"(x) : [pid] "m"(pid) : "eax", "ebx", "ecx", "memory", "cc");
        h = mix(h, r);
    }
    return mix(h, nsig);
}

// SIGSEGV on a load; the handler skips the 2-byte instruction and fakes the result
static volatile u32 segv_count;
static void on_segv(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    (void)sig; (void)si;
    segv_count++;
    uc->uc_mcontext.gregs[REG_EIP] += 2;                 // movl (%eax), %ebx is 8b 18
    uc->uc_mcontext.gregs[REG_EBX] = 0x1000 + segv_count;
}
static u32 t_skip_fault(void) {
    u32 h = 2166136261u;
    segv_count = 0;
    static u32 ok[4] = {11, 22, 33, 44};
    for (int i = 0; i < 3000; i++) {
        u32 *p = (i % 5 == 0) ? (u32 *)(uintptr_t)8 : &ok[i & 3];
        u32 v = 0xffffffffu, ecx = (u32)i * 7;
        __asm__ volatile("movl %2, %%eax\n\tmovl (%%eax), %%ebx\n\taddl %%ebx, %1\n\tmovl %%ebx, %0" : "=m"(v), "+r"(ecx) : "r"(p) : "eax", "ebx", "memory", "cc");
        h = mix(mix(h, v), ecx);
    }
    return mix(h, segv_count);
}

// SIGFPE: the handler repairs the operands and skips the 2-byte `div %ecx`
static volatile u32 fpe_count;
static void on_fpe(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    (void)sig; (void)si;
    fpe_count++;
    uc->uc_mcontext.gregs[REG_EAX] = 0x7777;
    uc->uc_mcontext.gregs[REG_EDX] = 0x1;
    uc->uc_mcontext.gregs[REG_EIP] += 2;                 // f7 f1 = div %ecx
}
static u32 t_fpe_resume(void) {
    u32 h = 2166136261u;
    fpe_count = 0;
    for (int i = 0; i < 3000; i++) {
        u32 a = rnd(), d = (i % 4 == 0) ? 0 : (rnd() | 1), q, r;
        __asm__ volatile("xorl %%edx, %%edx\n\tdivl %%ecx" : "=a"(q), "=&d"(r) : "0"(a), "c"(d) : "cc");
        h = mix(mix(h, q), r);
    }
    return mix(h, fpe_count);
}

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define E(n) {#n, n}
int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sa.sa_sigaction = on_usr1;
    sigaction(SIGUSR1, &sa, 0);
    sa.sa_sigaction = on_usr2;
    sigaction(SIGUSR2, &sa, 0);
    sa.sa_sigaction = on_segv;
    sigaction(SIGSEGV, &sa, 0);
    sa.sa_sigaction = on_fpe;
    sigaction(SIGFPE, &sa, 0);
    static const struct T tests[] = {E(t_kill_loop), E(t_flags_in_frame), E(t_flags_from_handler), E(t_skip_fault), E(t_fpe_resume)};
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0x7e57ab1eu + i * 7919u;
        u32 a = tests[i].f();
        rs = 0x7e57ab1eu + i * 7919u;
        u32 b = tests[i].f();
        printf("%-20s %08x%s\n", tests[i].name, a, a == b ? "" : " MISMATCH");
    }
    return 0;
}
