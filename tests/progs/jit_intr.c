// JIT stress: the hash-lookup function of the Flash runtime (the exact 104 bytes at 0x153202 of libpepflashplayer 32.0.0.363) called the way
// the runtime calls it. The JIT replaces this function by a fast path (crates/emu/src/jit_intr.rs): the results, the callee-saved registers and
// every fault (a table that runs into a PROT_NONE page: eip, fault address and registers as seen by the handler) must come out exactly as on a
// real CPU, also while the code is still being tiered up (thresholds 1..100) and when it is called from several places.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <ucontext.h>
typedef uint8_t u8; typedef uint32_t u32; typedef int32_t i32;
static u32 rs = 0x1badf00du;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }

extern u32 real_find(const u32 *ht, u32 key, const u32 *tab, u32 cap) __attribute__((cdecl));
extern u32 call_checked(const u32 *ht, u32 key, const u32 *tab, u32 cap, u32 *ok) __attribute__((cdecl));
__asm__(".text\n.globl real_find\n.type real_find,@function\nreal_find:\n.byte 0x55, 0x89, 0xe5, 0x57, 0x56, 0x83, 0xec, 0x08, 0x8b, 0x45, 0x08, 0x8b, 0x30, 0x83, 0xe6, 0x01, 0xf7, 0xd6, 0x8b, 0x7d, 0x0c, 0x21, 0xf7, 0x8b, 0x45, 0x14, 0x83, 0xe8, 0x01, 0x83, 0xe0, 0xfe, 0x89, 0x45, 0xf4, 0x89, 0xf8, 0x25, 0xf8, 0xff, 0xff, 0x7f, 0xc1, 0xf8, 0x02, 0x23, 0x45, 0xf4, 0x89, 0xf2, 0x8b, 0x4d, 0x10, 0x23, 0x14, 0x81, 0x39, 0xfa, 0x74, 0x25, 0xb9, 0x0e, 0x00, 0x00, 0x00, 0x85, 0xd2, 0x74, 0x1c, 0x89, 0x7d, 0xf0, 0x83, 0xc1, 0x02, 0x01, 0xc8, 0x23, 0x45, 0xf4, 0x89, 0xf2, 0x8b, 0x7d, 0x10, 0x23, 0x14, 0x87, 0x3b, 0x55, 0xf0, 0x74, 0x04, 0x85, 0xd2, 0x75, 0xe7, 0x83, 0xc4, 0x08, 0x5e, 0x5f, 0x5d, 0xc3\n.size real_find,.-real_find\n"
        // u32 call_checked(ht, key, tab, cap, u32 *ok): real_find with sentinels in ebx/esi/edi; clears *ok if any callee-saved register changed
        ".globl call_checked\n.type call_checked,@function\ncall_checked:\n"
        "push %ebp\nmov %esp,%ebp\npush %ebx\npush %esi\npush %edi\n"
        "mov $0x13572468,%ebx\nmov $0x2468ace0,%esi\nmov $0x0f1e2d3c,%edi\n"
        "push 20(%ebp)\npush 16(%ebp)\npush 12(%ebp)\npush 8(%ebp)\n"
        "call real_find\n"
        "add $16,%esp\n"
        "mov 24(%ebp),%ecx\n"
        "cmp $0x13572468,%ebx\njne 1f\ncmp $0x2468ace0,%esi\njne 1f\ncmp $0x0f1e2d3c,%edi\njne 1f\njmp 2f\n"
        "1: movl $0,(%ecx)\n"
        "2: pop %edi\npop %esi\npop %ebx\npop %ebp\nret\n"
        ".size call_checked,.-call_checked\n");

static u32 ref_find(const u32 *ht, u32 key, const u32 *tab, u32 cap) {
    u32 tag = ~(*ht & 1u), k = key & tag, mask = (cap - 1) & ~1u;
    u32 idx = (u32)((i32)(k & 0x7ffffff8u) >> 2) & mask, v = tag & tab[idx];
    if (v == k || v == 0) return idx;
    u32 step = 14;
    for (;;) { step += 2; idx = (idx + step) & mask; v = tag & tab[idx]; if (v == k || v == 0) return idx; }
}
static u32 tabmem[4096 + 8];
static u32 ht_word;

// tables the lookups can terminate on: insert random keys with the same probing, leave a good share of slots empty
static u32 build(u32 cap, u32 pct, u32 *keys) {
    memset(tabmem, 0, sizeof tabmem);
    u32 n = cap / 2 * pct / 100, mask = (cap - 1) & ~1u;
    for (u32 i = 0; i < n; i++) {
        u32 k = (rnd() | 0x10000000u) & ~7u, idx = ((k & 0x7ffffff8u) >> 2) & mask, step = 14;
        keys[i] = k;
        while (tabmem[idx]) { step += 2; idx = (idx + step) & mask; }
        tabmem[idx] = k; tabmem[idx + 1] = i + 1;
    }
    return n;
}

static u32 caller_a(u32 key, u32 cap, u32 *ok) { return call_checked(&ht_word, key, tabmem, cap, ok); }
static u32 caller_b(u32 key, u32 cap, u32 *ok) { u32 r = call_checked(&ht_word, key, tabmem, cap, ok); return r + (key & 0); }
static u32 caller_c(u32 key, u32 cap) { return real_find(&ht_word, key, tabmem, cap); }

static void lookups(void) {
    u32 keys[2048], h = 2166136261u, bad = 0, badreg = 0, n_lookups = 0;
    static const u32 caps[] = {8, 16, 64, 128, 256, 512, 1024, 2048, 4096};   // powers of two, like the runtime's tables (other sizes make the probe sequence cycle)
    static const u32 fills[] = {10, 35, 60, 80};
    for (int round = 0; round < 9 * 4 * 3; round++) {
        u32 cap = caps[round % 9], pct = fills[(round / 9) % 4];
        ht_word = (round & 1) ? 0x3 : 0x10;                                // bit 0 selects the key mask
        u32 n = build(cap, pct, keys), ok = 1;
        for (int i = 0; i < 700; i++) {
            u32 key = (i & 1) && n ? keys[rnd() % n] : (rnd() | 0x10000000u) & ~7u;
            u32 want = ref_find(&ht_word, key, tabmem, cap), got;
            switch (i % 3) { case 0: got = caller_a(key, cap, &ok); break; case 1: got = caller_b(key, cap, &ok); break; default: got = caller_c(key, cap); }
            if (got != want) bad++;
            h = mix(h, got);
            n_lookups++;
        }
        badreg += !ok;
    }
    printf("lookups=%u mismatches=%u callee-saved clobbered=%u hash=%08x\n", n_lookups, bad, badreg, h);
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
    // an 8 KiB table that starts on the second page: its first half is readable, its second half is the guard page, and nothing beyond it can be reached
    u32 *tab = (u32 *)(m + pg), ok = 1, h = 2166136261u, nfault = 0, nok = 0;
    for (u32 i = 0; i < pg / 4; i++) tab[i] = (rnd() % 100 < 25) ? 0 : ((rnd() | 0x10000000u) & ~7u);
    ht_word = 0;
    for (int i = 0; i < 400; i++) {
        u32 key = (rnd() | 0x10000000u) & ~7u, cap = 2048;            // cap claims 8 KiB, only the first 4 KiB are readable
        if (sigsetjmp(jb, 1) == 0) {
            u32 r = call_checked(&ht_word, key, tab, cap, &ok);
            h = mix(h, r); nok++;
        } else {
            nfault++;
            h = mix(mix(mix(h, f_ip - (u32)(uintptr_t)real_find), f_addr - (u32)(uintptr_t)tab), f_eax);
            h = mix(mix(mix(h, f_edx), f_esi), 0);
            int in_loop = (f_ip - (u32)(uintptr_t)real_find == 0x55);                  // first probe: ecx = table, edi = key; probe loop: ecx = step, edi = table
            u32 edi_v = in_loop ? f_edi - (u32)(uintptr_t)tab : f_edi, ecx_v = in_loop ? f_ecx : f_ecx - (u32)(uintptr_t)tab;
            h = mix(mix(mix(h, edi_v), ecx_v), f_ebx);
            if (nfault <= 6) printf("fault %u: eip=real_find+%#x addr=tab+%#x eax=%#x edx=%#x esi=%#x ecx=%#x edi=%#x ebx=%#x\n", nfault, f_ip - (u32)(uintptr_t)real_find, f_addr - (u32)(uintptr_t)tab,
                   f_eax, f_edx, f_esi, ecx_v, edi_v, f_ebx);
        }
    }
    printf("faults: %u faulted, %u completed, callee-saved ok=%u hash=%08x\n", nfault, nok, ok, h);
}

// ---- hot loop: lots of calls from one place so the tiering gets to compile it at every threshold ----
static void hot(void) {
    u32 keys[2048], h = 2166136261u, ok = 1;
    u32 n = build(2048, 60, keys);
    for (int i = 0; i < 300000; i++) {
        u32 key = (i & 1) ? keys[(i * 40503u) % n] : (((u32)i * 2654435761u) | 0x10000000u) & ~7u;
        h = mix(h, caller_a(key, 2048, &ok));
    }
    printf("hot: hash=%08x ok=%u\n", h, ok);
}

int main(void) {
    lookups();
    faults();
    hot();
    return 0;
}
