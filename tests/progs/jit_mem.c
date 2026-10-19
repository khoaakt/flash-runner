// JIT stress: memory operands. Every addressing-mode shape (base, base+disp8/32, index*scale, base+index*scale+disp,
// absolute), %gs-relative TLS accesses, partial-width loads/stores, accesses straddling a page boundary, and
// precise SIGSEGV delivery (guard pages, retry after mprotect) from inside hot loops.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0xc0ffee11u;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }

static u8 buf[8192] __attribute__((aligned(4096)));
static void fill(void) { for (unsigned i = 0; i < sizeof buf; i++) buf[i] = (u8)(i * 31 + (i >> 8) * 7 + 1); }

// ---- addressing-mode shapes -----------------------------------------------------------------------
#define LOAD_FORM(NAME, ASM)                                                                      \
    static u32 a_##NAME(void) {                                                                   \
        u32 h = 2166136261u;                                                                      \
        for (int i = 0; i < 3000; i++) {                                                          \
            u32 base = (u32)(uintptr_t)buf + 64 + (rnd() & 0x3ff), idx = rnd() & 0xff, v;          \
            __asm__ volatile(ASM : "=&r"(v) : "r"(base), "r"(idx) : "memory", "cc");             \
            h = mix(h, v);                                                                        \
        }                                                                                         \
        return h;                                                                                 \
    }
LOAD_FORM(b, "movl (%1), %0")
LOAD_FORM(b8, "movl 8(%1), %0")
LOAD_FORM(bm8, "movl -8(%1), %0")
LOAD_FORM(b1000, "movl 1000(%1), %0")
LOAD_FORM(bi1, "movl (%1,%2), %0")
LOAD_FORM(bi2, "movl (%1,%2,2), %0")
LOAD_FORM(bi4, "movl (%1,%2,4), %0")
LOAD_FORM(bi8, "movl (%1,%2,8), %0")
LOAD_FORM(bi4d8, "movl 12(%1,%2,4), %0")
LOAD_FORM(bi8d32, "movl 300(%1,%2,8), %0")
LOAD_FORM(bim, "movl -16(%1,%2,2), %0")
// lea results are addresses: hash them relative to the base so the checksum is independent of the load address
#define LEA_FORM(NAME, ASM, REL)                                                                  \
    static u32 a_##NAME(void) {                                                                   \
        u32 h = 2166136261u;                                                                      \
        for (int i = 0; i < 3000; i++) {                                                          \
            u32 base = (u32)(uintptr_t)buf + 64 + (rnd() & 0x3ff), idx = rnd() & 0xff, v;          \
            __asm__ volatile(ASM : "=&r"(v) : "r"(base), "r"(idx) : "memory", "cc");             \
            h = mix(h, v - (REL ? base : 0));                                                     \
        }                                                                                         \
        return h;                                                                                 \
    }
LEA_FORM(lea1, "leal 5(%1,%2,4), %0", 1)
LEA_FORM(lea2, "leal (%2,%2,8), %0", 0)
LEA_FORM(lea3, "leal -100(%1), %0", 1)
LEA_FORM(lea4, "leal (%2,%1,1), %0", 1)
LOAD_FORM(movzb, "movzbl 3(%1,%2), %0")
LOAD_FORM(movsb, "movsbl 5(%1,%2,2), %0")
LOAD_FORM(movzw, "movzwl 7(%1), %0")
LOAD_FORM(movsw, "movswl 9(%1,%2), %0")
LOAD_FORM(movzbw, "xorl %0, %0\n\tmovzbw (%1), %w0")
LOAD_FORM(unal2, "movzwl 1(%1,%2), %0")
LOAD_FORM(unal4, "movl 3(%1,%2,2), %0")

// esp / ebp based forms and push/pop with memory operands
static u32 a_stack(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 3000; i++) {
        u32 x = rnd(), y = rnd(), o1, o2, o3;
        __asm__ volatile(
            "pushl %3\n\tpushl %4\n\tpushl 4(%%esp)\n\tpopl %0\n\t"      // o1 = x
            "pushl $0x1234\n\tpopl 4(%%esp)\n\t"                          // overwrite x's slot (pop mem with esp base)
            "movl 4(%%esp), %1\n\t"
            "popl %2\n\tpopl %%eax\n\t"                                   // o3 = y, discard
            : "=&r"(o1), "=&r"(o2), "=&r"(o3) : "r"(x), "r"(y) : "eax", "memory", "cc");
        h = mix(mix(mix(h, o1), o2), o3);
    }
    return h;
}
static u32 a_ebp(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 3000; i++) {
        u32 x = rnd(), r;
        __asm__ volatile("pushl %%ebp\n\tleal -32(%%esp), %%ebp\n\tmovl %1, 8(%%ebp)\n\tmovl 8(%%ebp), %%eax\n\txorl 8(%%ebp), %%eax\n\taddl %1, %%eax\n\tmovl %%eax, 12(%%ebp)\n\tleal (%%ebp,%%eax,1), %%edx\n\tsubl %%ebp, %%edx\n\tsubl 12(%%ebp), %%edx\n\taddl 12(%%ebp), %%edx\n\tmovl %%edx, %0\n\tpopl %%ebp"
                         : "=&r"(r) : "r"(x) : "eax", "edx", "memory", "cc");
        h = mix(mix(h, r), x);
    }
    return h;
}

// ---- thread-local storage through %gs ---------------------------------------------------------------
static __thread u32 tls_counter;
static __thread u32 tls_array[16];
static u32 t_gs(void) {
    u32 h = 2166136261u;
    tls_counter = 0;
    memset(tls_array, 0, sizeof tls_array);
    for (int i = 0; i < 3000; i++) {
        tls_counter += i;
        tls_array[i & 15] = tls_array[(i + 3) & 15] * 3 + tls_counter;
        u32 self, v;
        __asm__ volatile("movl %%gs:0, %0" : "=r"(self));
        h = mix(h, self == (u32)(uintptr_t)__builtin_thread_pointer());
        __asm__ volatile("movl %%gs:(%1), %0" : "=r"(v) : "r"(0) : "memory");
        h = mix(h, v == self);
        h = mix(h, tls_array[i & 15]);
    }
    return mix(h, tls_counter);
}

// ---- partial-width stores into memory -----------------------------------------------------------------
static u32 t_widths(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 3000; i++) {
        u32 a = rnd(), off = rnd() & 0x7ff;
        u8 *p = buf + 2048 + off;
        __asm__ volatile("movb %b0, (%1)\n\tmovw %w0, 2(%1)\n\tmovl %0, 5(%1)\n\tmovb %h0, 1(%1)\n\tmovw $0x1234, 10(%1)\n\tmovb $0x56, 12(%1)\n\tmovl $0x789abcde, 13(%1)"
                         : : "Q"(a), "r"(p) : "memory");
        u32 r;
        __asm__ volatile("movzbl 1(%1), %0\n\tmovzwl 2(%1), %%edx\n\taddl %%edx, %0\n\tmovl 5(%1), %%edx\n\txorl %%edx, %0\n\tmovl 9(%1), %%edx\n\taddl %%edx, %0"
                         : "=&r"(r) : "r"(p) : "edx", "memory", "cc");
        h = mix(h, r);
    }
    return h;
}

// ---- page-straddling accesses ---------------------------------------------------------------------------------
static u32 t_cross(void) {
    u32 h = 2166136261u;
    fill();
    for (int i = 0; i < 3000; i++) {
        u32 off = 4096 - 4 + (rnd() & 3);        // 4092..4095
        u8 *p = buf + off;
        u32 v = rnd(), r16, r32;
        __asm__ volatile("movl %2, (%1)\n\tmovl (%1), %0" : "=r"(r32) : "r"(p), "r"(v) : "memory");
        h = mix(h, r32 == v);
        u8 *q = buf + 4095 - (rnd() & 1);
        __asm__ volatile("movw %w2, (%1)\n\tmovzwl (%1), %0" : "=r"(r16) : "r"(q), "r"(v) : "memory");
        h = mix(h, r16 == (v & 0xffff));
        __asm__ volatile("addl %2, (%1)\n\tmovl (%1), %0" : "=r"(r32) : "r"(p), "r"(v) : "memory", "cc");
        h = mix(mix(h, r32), buf[4095]);
        __asm__ volatile("incl (%0)\n\tdecw 1(%0)" : : "r"(p) : "memory", "cc");
        h = mix(h, buf[4094] ^ buf[4096]);
    }
    return h;
}

// ---- precise page faults -------------------------------------------------------------------------------------------
static sigjmp_buf jb;
static volatile u32 f_addr, f_ip, f_eax, f_ebx, f_ecx, f_esp;
static volatile int n_segv;
static u8 *guard;
static void on_segv(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    (void)sig;
    f_addr = (u32)(uintptr_t)si->si_addr;
    f_ip = (u32)uc->uc_mcontext.gregs[REG_EIP];
    f_eax = (u32)uc->uc_mcontext.gregs[REG_EAX];
    f_ebx = (u32)uc->uc_mcontext.gregs[REG_EBX];
    f_ecx = (u32)uc->uc_mcontext.gregs[REG_ECX];
    f_esp = (u32)uc->uc_mcontext.gregs[REG_ESP];
    n_segv++;
    siglongjmp(jb, 1);
}
// the access must fault; report where (absolute fault address minus the base, ip relative to the asm block)
static volatile u32 blk_ip;
#define FAULT_CASE(h, ASM, ...)                                                                          \
    do {                                                                                                 \
        u32 eax_ = 0x11111111u, ebx_ = 0x22222222u, ecx_ = 0x33333333u;                                  \
        if (sigsetjmp(jb, 0) == 0) {                                                                     \
            __asm__ volatile("call 0f\n0:\tpopl %[bip]\n\t" ASM : [bip] "=m"(blk_ip), "+a"(eax_), "+b"(ebx_), "+c"(ecx_) : __VA_ARGS__ : "memory", "cc", "edx"); \
            h = mix(h, 0xbad);                                                                           \
        } else {                                                                                         \
            h = mix(mix(h, f_addr - (u32)(uintptr_t)guard), f_ip - blk_ip);                              \
            h = mix(mix(mix(h, f_eax), f_ebx), f_ecx);                                                   \
        }                                                                                                \
    } while (0)

// like FAULT_CASE for accesses that straddle into the guard page: the reported address is the first byte
// of the access (this emulator) or of the faulting page (hardware); both lie inside the access.
#define FAULT_STRADDLE(h, ASM, LO, ...)                                                                  \
    do {                                                                                                 \
        u32 eax_ = 0x11111111u, ebx_ = 0x22222222u, ecx_ = 0x33333333u;                                  \
        if (sigsetjmp(jb, 0) == 0) {                                                                     \
            __asm__ volatile("call 0f\n0:\tpopl %[bip]\n\t" ASM : [bip] "=m"(blk_ip), "+a"(eax_), "+b"(ebx_), "+c"(ecx_) : __VA_ARGS__ : "memory", "cc", "edx"); \
            h = mix(h, 0xbad);                                                                           \
        } else {                                                                                         \
            h = mix(mix(h, f_addr >= (LO) && f_addr < (LO) + 4), f_ip - blk_ip);                         \
            h = mix(mix(mix(h, f_eax), f_ebx), f_ecx);                                                   \
        }                                                                                                \
    } while (0)

static u32 t_fault(void) {
    u32 h = 2166136261u;
    n_segv = 0;
    long pg = 4096;
    u8 *m = mmap(0, 3 * pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    guard = m + 2 * pg;
    mprotect(guard, pg, PROT_NONE);
    for (int i = 0; i < 400; i++) {
        u32 a = (u32)(uintptr_t)guard;
        FAULT_CASE(h, "movl (%[p]), %%edx", [p] "r"(a));                                  // read of the guard page
        FAULT_CASE(h, "addl $1, %%eax\n\tmovl %%eax, 4(%[p])", [p] "r"(a));               // store after a register change
        FAULT_CASE(h, "movl $7, %%ebx\n\tmovb %%bl, (%[p],%[i],2)", [p] "r"(a), [i] "r"(i & 3)); // store through an index
        FAULT_CASE(h, "incl 8(%[p])", [p] "r"(a));                                        // read-modify-write
        FAULT_CASE(h, "pushl %%eax\n\tpopl %%ecx\n\taddl %%ecx, (%[p])", [p] "r"(a));     // stack ops before the fault
        FAULT_CASE(h, "xaddl %%eax, 12(%[p])", [p] "r"(a));
        FAULT_CASE(h, "cmpl %%eax, 16(%[p])", [p] "r"(a));
        // straddle: first byte(s) valid, the rest on the guard page; just check that it faults inside the access
        u32 lo = a - 2;
        FAULT_STRADDLE(h, "movl %%eax, (%[p])", lo, [p] "r"(lo));
        FAULT_STRADDLE(h, "movl (%[p]), %%ecx", lo, [p] "r"(lo));
        FAULT_STRADDLE(h, "addw %%ax, (%[p])", lo + 1, [p] "r"(lo + 1));
    }
    // fault, fix the protection in the handler path (as a JIT'd program with a SIGSEGV based allocator would), retry
    mprotect(guard, pg, PROT_READ | PROT_WRITE);
    for (int i = 0; i < 100; i++) {
        *(volatile u32 *)(guard + 16) = rnd();
        h = mix(h, *(volatile u32 *)(guard + 16) != 0);
    }
    munmap(m, 3 * pg);
    return mix(h, n_segv);
}

// retry a faulting instruction after fixing the page in the handler (the handler returns normally)
static volatile int retry_hits;
static u8 *retry_page;
static void on_segv_fix(int sig, siginfo_t *si, void *ucv) {
    (void)sig; (void)ucv;
    retry_hits++;
    mprotect((void *)((uintptr_t)si->si_addr & ~4095u), 4096, PROT_READ | PROT_WRITE);
}
static u32 t_retry(void) {
    u32 h = 2166136261u;
    retry_hits = 0;
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv_fix;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &sa, 0);
    retry_page = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    u32 *p = (u32 *)retry_page;
    for (int round = 0; round < 30; round++) {
        mprotect(retry_page, 4096, PROT_NONE);
        u32 acc = 0;
        for (int i = 0; i < 256; i++) {          // the first iteration faults, the handler opens the page, the store retries
            p[i] += i + round;
            acc += p[i];
        }
        h = mix(mix(h, acc), retry_hits);
    }
    munmap(retry_page, 4096);
    struct sigaction sb;
    memset(&sb, 0, sizeof sb);
    sb.sa_sigaction = on_segv;
    sb.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &sb, 0);
    return h;
}

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define E(n) {#n, n}
int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &sa, 0);
    fill();
    static const struct T tests[] = {
        E(a_b), E(a_b8), E(a_bm8), E(a_b1000), E(a_bi1), E(a_bi2), E(a_bi4), E(a_bi8), E(a_bi4d8), E(a_bi8d32), E(a_bim),
        E(a_lea1), E(a_lea2), E(a_lea3), E(a_lea4), E(a_movzb), E(a_movsb), E(a_movzw), E(a_movsw), E(a_movzbw), E(a_unal2), E(a_unal4),
        E(a_stack), E(a_ebp), E(t_gs), E(t_widths), E(t_cross), E(t_fault), E(t_retry),
    };
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0xc0ffee11u + i * 7919u;
        fill();
        u32 a = tests[i].f();
        rs = 0xc0ffee11u + i * 7919u;
        fill();
        u32 b = tests[i].f();
        printf("%-10s %08x%s\n", tests[i].name, a, a == b ? "" : " MISMATCH");
    }
    return 0;
}
