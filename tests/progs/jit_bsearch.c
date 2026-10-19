// JIT stress: glibc's bsearch and the Flash runtime's range-test comparator (the exact 94 + 36 code bytes, embedded below) called the way the
// runtime calls them: bsearch(&addr, regions, n, 8, cmp) over {start, length} records. The JIT replaces the pair by a fast path
// (crates/emu/src/jit_intr.rs): results, the registers the original leaves behind (ECX = last element, EDX = key value, the callee-saved ones),
// the dead stack frame words it writes and every fault (a table that runs into a PROT_NONE page: eip, fault address, registers as seen by the
// handler) must come out exactly as on a real CPU, also while the code is still being tiered up (thresholds 1..100) and from several call sites.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
typedef uint8_t u8; typedef uint32_t u32; typedef int32_t i32;
static u32 rs = 0x2badf00du;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }

typedef int (*cmp_t)(const void *, const void *);
extern int real_cmp(const void *key, const void *elem) __attribute__((cdecl));
extern void *real_bsearch(const void *key, const void *base, u32 n, u32 size, cmp_t cmp) __attribute__((cdecl));
extern u32 call_bs(const u32 *keyp, const void *base, u32 n, u32 size, cmp_t cmp, u32 *out) __attribute__((cdecl));
__asm__(".text\n.p2align 4\n.globl real_cmp\n.type real_cmp,@function\nreal_cmp:\n"
        ".byte 0x55, 0x89, 0xe5, 0x56, 0x8b, 0x4d, 0x0c, 0x8b, 0x45, 0x08, 0x8b, 0x10, 0x8b, 0x31, 0xb8, 0xff, 0xff, 0xff, 0xff, 0x39, 0xf2, 0x72, 0x0a, 0x03, 0x71, 0x04, 0x31, 0xc0, 0x39, 0xf2, 0x0f, 0x93, 0xc0, 0x5e, 0x5d, 0xc3\n"
        ".size real_cmp,.-real_cmp\n"
        ".p2align 4\n.globl real_bsearch\n.type real_bsearch,@function\nreal_bsearch:\n"
        ".byte 0x55, 0x57, 0x56, 0x53, 0x83, 0xec, 0x0c, 0x8b, 0x7c, 0x24, 0x28, 0x85, 0xff, 0x74, 0x43, 0x8b, 0x6c, 0x24, 0x28, 0x31, 0xff, 0xeb, 0x12, 0x8d, 0xb4, 0x26, 0x00, 0x00, 0x00, 0x00, 0x66, 0x90, 0x74, 0x32, 0x8d, 0x7b, 0x01, 0x39, 0xef, 0x73\n"
        ".byte 0x29, 0x8b, 0x74, 0x24, 0x2c, 0x8d, 0x1c, 0x2f, 0x83, 0xec, 0x08, 0xd1, 0xeb, 0x0f, 0xaf, 0xf3, 0x03, 0x74, 0x24, 0x2c, 0x56, 0xff, 0x74, 0x24, 0x2c, 0xff, 0x54, 0x24, 0x40, 0x83, 0xc4, 0x10, 0x85, 0xc0, 0x79, 0xd4, 0x89, 0xdd, 0x39, 0xef\n"
        ".byte 0x72, 0xd7, 0x31, 0xf6, 0x83, 0xc4, 0x0c, 0x89, 0xf0, 0x5b, 0x5e, 0x5f, 0x5d, 0xc3\n"
        ".size real_bsearch,.-real_bsearch\n"
        // u32 call_bs(keyp, base, n, size, cmp, out[8]): real_bsearch with sentinels in ebx/esi/edi. Afterwards out[] receives the result, ECX, EDX,
        // the bsearch frame words that hold the upper bound / last element / key pointer (the dead stack the original leaves behind) and a bit mask
        // of callee-saved registers that changed.
        ".globl call_bs\n.type call_bs,@function\ncall_bs:\n"
        "push %ebp\nmov %esp,%ebp\npush %ebx\npush %esi\npush %edi\nsub $48,%esp\n"
        "mov $0x13572468,%ebx\nmov $0x2468ace0,%esi\nmov $0x0f1e2d3c,%edi\n"
        "push 24(%ebp)\npush 20(%ebp)\npush 16(%ebp)\npush 12(%ebp)\npush 8(%ebp)\n"
        "xor %ecx,%ecx\nxor %edx,%edx\n"
        "call real_bsearch\n"
        "mov %eax,-16(%ebp)\nmov %ecx,-20(%ebp)\nmov %edx,-24(%ebp)\n"
        "mov -56(%esp),%eax\nmov %eax,-28(%ebp)\n"
        "mov -44(%esp),%eax\nmov %eax,-32(%ebp)\n"
        "mov -48(%esp),%eax\nmov %eax,-36(%ebp)\n"
        "xor %ecx,%ecx\n"
        "cmp $0x13572468,%ebx\nje 1f\nor $1,%ecx\n1:\n"
        "cmp $0x2468ace0,%esi\nje 2f\nor $2,%ecx\n2:\n"
        "cmp $0x0f1e2d3c,%edi\nje 3f\nor $4,%ecx\n3:\n"
        "mov %ecx,-40(%ebp)\n"
        "mov 28(%ebp),%edx\n"
        "mov -16(%ebp),%eax\nmov %eax,0(%edx)\n"
        "mov -20(%ebp),%eax\nmov %eax,4(%edx)\n"
        "mov -24(%ebp),%eax\nmov %eax,8(%edx)\n"
        "mov -28(%ebp),%eax\nmov %eax,12(%edx)\n"
        "mov -32(%ebp),%eax\nmov %eax,16(%edx)\n"
        "mov -36(%ebp),%eax\nmov %eax,20(%edx)\n"
        "mov -40(%ebp),%eax\nmov %eax,24(%edx)\n"
        "mov -16(%ebp),%eax\n"
        "lea -12(%ebp),%esp\npop %edi\npop %esi\npop %ebx\npop %ebp\nret\n"
        ".size call_bs,.-call_bs\n");

typedef struct { u32 start, len; } rec8;
typedef struct { u32 start, len, pad0, pad1; } rec16;
static rec8 tab8[600];
static rec16 tab16[600];
static u32 keyv;

// reference: the same binary search written in C (index of the record containing key, or -1)
static i32 ref_search(const rec8 *t, u32 n, u32 key) {
    u32 l = 0, u = n;
    while (l < u) {
        u32 mid = (l + u) >> 1;
        if (key < t[mid].start) u = mid;
        else if (key >= t[mid].start + t[mid].len) l = mid + 1;
        else return (i32)mid;
    }
    return -1;
}

// sorted, disjoint regions with random gaps (the shape of the runtime's region list), sometimes touching the top of the address space
static u32 build(u32 n, u32 top) {
    u32 a = top ? 0xffff0000u - n * 0x300 : 0x1000 + (rnd() & 0xfffff);
    for (u32 i = 0; i < n; i++) {
        u32 len = (rnd() % 12 == 0) ? 0 : 1 + (rnd() & 0xff);
        tab8[i].start = a; tab8[i].len = len;
        tab16[i].start = a; tab16[i].len = len; tab16[i].pad0 = rnd(); tab16[i].pad1 = rnd();
        a += len + (rnd() & 0x1ff);
    }
    return n;
}

static void *caller_a(u32 key, u32 n, u32 *out) { keyv = key; return (void *)(uintptr_t)call_bs(&keyv, tab8, n, 8, real_cmp, out); }
static void *caller_b(u32 key, u32 n, u32 *out) { keyv = key; return (void *)(uintptr_t)(call_bs(&keyv, tab16, n, 16, real_cmp, out) + 0); }
static void *caller_c(u32 key, u32 n) { keyv = key; return real_bsearch(&keyv, tab8, n, 8, real_cmp); }
static void *caller_d(u32 key, u32 n) { keyv = key; return bsearch(&keyv, tab8, n, 8, real_cmp); }   // libc's own copy (static: from libc.a, dynamic: from libc.so)

static void lookups(void) {
    u32 h = 2166136261u, n_lookups = 0, bad = 0, badreg = 0, found = 0;
    static const u32 sizes[] = {1, 2, 3, 5, 8, 17, 50, 131, 300, 599};
    for (int round = 0; round < 10 * 3; round++) {
        u32 n = build(sizes[round % 10], round >= 20);
        for (int i = 0; i < 500; i++) {
            u32 key, r = rnd() % 8;
            u32 j = rnd() % n;
            if (r == 0) key = rnd();
            else if (r == 1) key = tab8[j].start;
            else if (r == 2) key = tab8[j].start + tab8[j].len;
            else if (r == 3) key = tab8[j].start + tab8[j].len - 1;
            else if (r == 4) key = tab8[j].start - 1;
            else if (r == 5) key = tab8[j].start + (tab8[j].len ? rnd() % tab8[j].len : 0);
            else key = 0xffffffffu - (rnd() & 3);
            i32 want = ref_search(tab8, n, key);
            u32 out[8], out2[8];
            void *g;
            switch (i % 4) {
            case 0: g = caller_a(key, n, out); break;
            case 1: g = caller_b(key, n, out2); break;
            case 2: g = caller_c(key, n); break;
            default: g = caller_d(key, n); break;
            }
            i32 got = -1;
            if (i % 4 == 1) { if (g) got = (i32)(((u8 *)g - (u8 *)tab16) / 16); }
            else if (g) got = (i32)(((u8 *)g - (u8 *)tab8) / 8);
            if (got != want) bad++;
            found += got >= 0;
            h = mix(h, (u32)got);
            if (i % 4 == 0 || i % 4 == 1) {
                u32 *o = (i % 4 == 0) ? out : out2;
                u32 base = (i % 4 == 0) ? (u32)(uintptr_t)tab8 : (u32)(uintptr_t)tab16;
                // ecx: last element probed; edx: key value; [3]: upper bound at the last probe; [4]: last element again; [5]: key pointer; [6]: clobbered callee-saved mask
                h = mix(h, o[1] - base); h = mix(h, o[2]); h = mix(h, o[3]); h = mix(h, o[4] - base); h = mix(h, o[5] - (u32)(uintptr_t)&keyv);
                badreg += o[6] != 0;
            }
            n_lookups++;
        }
    }
    printf("lookups=%u found=%u mismatches=%u callee-saved clobbered=%u hash=%08x\n", n_lookups, found, bad, badreg, h);
}

// ---- tables that run into a PROT_NONE page: the fault must be delivered by the original code, precisely ----
static sigjmp_buf jb;
static volatile u32 f_ip, f_addr, f_eax, f_ecx, f_edx, f_esi, f_edi, f_ebx;
static void on_segv(int sig, siginfo_t *si, void *ctx) {
    ucontext_t *uc = ctx;
    (void)sig;
    f_ip = (u32)uc->uc_mcontext.gregs[REG_EIP]; f_addr = (u32)(uintptr_t)si->si_addr;
    f_eax = (u32)uc->uc_mcontext.gregs[REG_EAX]; f_ecx = (u32)uc->uc_mcontext.gregs[REG_ECX]; f_edx = (u32)uc->uc_mcontext.gregs[REG_EDX];
    f_esi = (u32)uc->uc_mcontext.gregs[REG_ESI]; f_edi = (u32)uc->uc_mcontext.gregs[REG_EDI]; f_ebx = (u32)uc->uc_mcontext.gregs[REG_EBX];
    siglongjmp(jb, 1);
}
static void faults(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv; sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &sa, 0);
    const u32 pg = 4096;
    u8 *m = mmap(0, 3 * pg, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(m + 2 * pg, pg, PROT_NONE);
    // 600 records of 8 bytes start on the second page: the first 512 are readable, the rest is the guard page
    rec8 *t = (rec8 *)(m + pg);
    u32 a = 0x2000, ok = 1, h = 2166136261u, nfault = 0, nok = 0;
    for (u32 i = 0; i < 512; i++) { t[i].start = a; t[i].len = 1 + (rnd() & 0x3f); a += t[i].len + (rnd() & 0x3f); }
    for (int i = 0; i < 400; i++) {
        u32 key = (i % 4 == 0) ? rnd() : (i % 4 == 1) ? t[rnd() % 512].start : (i % 4 == 2) ? t[rnd() % 256].start + 1 : a + (rnd() & 0xff);   // keys beyond the readable part probe the guard page
        u32 n = 600, out[8];
        if (sigsetjmp(jb, 1) == 0) {
            keyv = key;
            u32 r = call_bs(&keyv, t, n, 8, real_cmp, out);
            h = mix(h, r ? (r - (u32)(uintptr_t)t) : 0xffffffffu); nok++;
            h = mix(mix(h, out[1] - (u32)(uintptr_t)t), out[2]);
            ok &= out[6] == 0;
        } else {
            nfault++;
            h = mix(mix(mix(h, f_ip - (u32)(uintptr_t)real_cmp), f_addr - (u32)(uintptr_t)t), f_eax - (u32)(uintptr_t)&keyv);
            h = mix(mix(h, f_ecx - (u32)(uintptr_t)t), f_edx);
            h = mix(mix(h, f_esi - (u32)(uintptr_t)t), f_ebx);
            if (nfault <= 6) printf("fault %u: eip=real_cmp+%#x addr=tab+%#x eax=&key+%#x ecx=tab+%#x edx=%#x\n", nfault, f_ip - (u32)(uintptr_t)real_cmp, f_addr - (u32)(uintptr_t)t,
                                    f_eax - (u32)(uintptr_t)&keyv, f_ecx - (u32)(uintptr_t)t, f_edx);
        }
    }
    printf("faults: %u faulted, %u completed, callee-saved ok=%u hash=%08x\n", nfault, nok, ok, h);
}

// ---- other comparators and edge arguments: the fast path must hand these to the original code ----
static int other_cmp(const void *k, const void *e) {   // not the range test: orders by start only
    u32 a = *(const u32 *)k, b = ((const u32 *)e)[0];
    return a < b ? -1 : a > b;
}
static void others(void) {
    u32 h = 2166136261u;
    u32 n = build(300, 0);
    for (int i = 0; i < 2000; i++) {
        u32 key = (i & 1) ? tab8[rnd() % n].start : rnd();
        keyv = key;
        void *g = bsearch(&keyv, tab8, n, 8, other_cmp);
        h = mix(h, g ? (u32)(((u8 *)g - (u8 *)tab8) / 8) : 0xffffffffu);
        g = real_bsearch(&keyv, tab8, 0, 8, real_cmp);                       // empty table
        h = mix(h, g != 0);
        g = real_bsearch(&keyv, tab8, n, 0, real_cmp);                      // zero stride: always the first record
        h = mix(h, g != 0);
    }
    printf("others: hash=%08x\n", h);
}

// ---- hot loop: lots of calls from one place so the tiering gets to compile it at every threshold ----
static void hot(void) {
    u32 h = 2166136261u, bad = 0;
    u32 n = build(120, 0);
    for (int i = 0; i < 200000; i++) {
        u32 key = (i & 1) ? tab8[(i * 40503u) % n].start + (u32)(i & 7) : ((u32)i * 2654435761u) & 0xfffff;
        void *g = caller_c(key, n);
        i32 got = g ? (i32)(((u8 *)g - (u8 *)tab8) / 8) : -1;
        bad += got != ref_search(tab8, n, key);
        h = mix(h, (u32)got);
    }
    printf("hot: hash=%08x mismatches=%u\n", h, bad);
}

int main(void) {
    lookups();
    faults();
    others();
    hot();
    return 0;
}
