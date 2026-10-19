// JIT stress: the remaining integer instructions - string operations (rep/repe/repne, DF both ways), bit
// tests with register and memory operands (including bit offsets outside the operand), bsf/bsr/bswap, xchg/xadd/
// cmpxchg, lahf/sahf, pushf/popf, setcc/cmovcc over all 16 condition codes, xlat and carry-flag instructions.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0x13572468u;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static const u32 edge[] = {0, 1, 2, 0x7f, 0x80, 0xff, 0x7fff, 0x8000, 0xffff, 0x7fffffffu, 0x80000000u, 0xffffffffu, 0x55555555u, 0xaaaaaaaau};
static u32 val(void) { u32 r = rnd(); return (r & 3) ? edge[(r >> 2) % (sizeof edge / sizeof edge[0])] : rnd(); }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }
#define ALLF 0x8d5u

// ---- string instructions ---------------------------------------------------------------------------------
static u8 sa_[512], sb_[512];
static u32 hbuf(u32 h, const u8 *p, int n) { for (int i = 0; i < n; i++) h = mix(h, p[i]); return h; }
#define STR_COPY(NAME, ASM)                                                                              \
    static u32 s_##NAME(void) {                                                                          \
        u32 h = 2166136261u;                                                                             \
        for (int i = 0; i < 400; i++) {                                                                  \
            for (int k = 0; k < 512; k++) { sa_[k] = (u8)rnd(); sb_[k] = (u8)rnd(); }                    \
            u32 n = rnd() % 64, so = 64 + rnd() % 128, dof = 64 + rnd() % 128, fl;                       \
            u8 *src = sa_ + so, *dst = sb_ + dof; u32 ecx = n;                                           \
            __asm__ volatile(ASM "\n\tcld\n\tpushf\n\tpopl %3" : "+S"(src), "+D"(dst), "+c"(ecx), "=r"(fl) : : "eax", "memory", "cc"); \
            h = mix(mix(mix(h, (u32)(src - sa_)), (u32)(dst - sb_)), ecx);                               \
            h = hbuf(hbuf(h, sa_, 512), sb_, 512);                                                       \
        }                                                                                                \
        return h;                                                                                        \
    }
STR_COPY(movsb, "cld\n\trep movsb")
STR_COPY(movsw, "cld\n\trep movsw")
STR_COPY(movsl, "cld\n\trep movsl")
STR_COPY(movsb_df, "std\n\trep movsb")
STR_COPY(movsl_df, "std\n\trep movsl")
STR_COPY(stosb, "movb $0x5a, %%al\n\tcld\n\trep stosb")
STR_COPY(stosw, "movw $0x5a5b, %%ax\n\tcld\n\trep stosw")
STR_COPY(stosl, "movl $0x5a5b5c5d, %%eax\n\tcld\n\trep stosl")
STR_COPY(stosl_df, "movl $0x01020304, %%eax\n\tstd\n\trep stosl")
STR_COPY(single, "cld\n\tmovsb\n\tmovsw\n\tmovsl\n\tstosb\n\tstosl")
#undef STR_COPY

#define STR_CMP(NAME, ASM)                                                                               \
    static u32 s_##NAME(void) {                                                                          \
        u32 h = 2166136261u;                                                                             \
        for (int i = 0; i < 600; i++) {                                                                  \
            for (int k = 0; k < 256; k++) sa_[k] = (u8)(rnd() & 3);                                      \
            memcpy(sb_, sa_, 256);                                                                       \
            if (rnd() & 1) sb_[rnd() & 63] ^= 1 << (rnd() & 3);                                          \
            u32 ecx = 1 + rnd() % 70, fl, eax = rnd() & 3; u8 *src = sa_, *dst = sb_;                   \
            __asm__ volatile("movl %4, %%eax\n\tcld\n\t" ASM "\n\tpushf\n\tpopl %3" : "+S"(src), "+D"(dst), "+c"(ecx), "=r"(fl) : "r"(eax) : "eax", "memory", "cc"); \
            h = mix(mix(mix(h, (u32)(src - sa_)), (u32)(dst - sb_)), ecx);                               \
            h = mix(h, fl & ALLF);                                                                       \
        }                                                                                                \
        return h;                                                                                        \
    }
STR_CMP(repe_cmpsb, "repe cmpsb")
STR_CMP(repne_cmpsb, "repne cmpsb")
STR_CMP(repe_cmpsl, "repe cmpsl")
STR_CMP(repne_scasb, "repne scasb")
STR_CMP(repe_scasb, "repe scasb")
STR_CMP(repne_scasl, "repne scasl")
STR_CMP(cmps_single, "cmpsb")
STR_CMP(scas_single, "scasw")
STR_CMP(lods, "lodsb\n\tlodsw\n\tlodsl")
#undef STR_CMP

// ---- bit instructions ---------------------------------------------------------------------------------------------
static u32 bm[96];
static u32 b_regs(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 4000; i++) {
        u32 a = val(), n = rnd(), r, f;
        u32 x = a;
        __asm__ volatile("btl %2, %1\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(x), "r"(n) : "cc"); h = mix(h, f & 1);
        x = a; __asm__ volatile("btsl %2, %0\n\tpushf\n\tpopl %1" : "+r"(x), "=r"(f) : "r"(n) : "cc"); h = mix(mix(h, x), f & 1);
        x = a; __asm__ volatile("btrl %2, %0\n\tpushf\n\tpopl %1" : "+r"(x), "=r"(f) : "r"(n) : "cc"); h = mix(mix(h, x), f & 1);
        x = a; __asm__ volatile("btcl %2, %0\n\tpushf\n\tpopl %1" : "+r"(x), "=r"(f) : "r"(n) : "cc"); h = mix(mix(h, x), f & 1);
        x = a; __asm__ volatile("btcl $17, %0\n\tpushf\n\tpopl %1" : "+r"(x), "=r"(f) : : "cc"); h = mix(mix(h, x), f & 1);
        u16 w = (u16)a; __asm__ volatile("btsw %w2, %0\n\tpushf\n\tpopl %1" : "+r"(w), "=r"(f) : "r"(n) : "cc"); h = mix(mix(h, w), f & 1);
        w = (u16)a; __asm__ volatile("btrw $9, %0\n\tpushf\n\tpopl %1" : "+r"(w), "=r"(f) : : "cc"); h = mix(mix(h, w), f & 1);
        (void)r;
    }
    return h;
}
static u32 b_mem(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 4000; i++) {
        for (int k = 0; k < 96; k++) bm[k] = rnd();
        int off = (int)(rnd() % 2048) - 1024;           // bit offset relative to bm[48]: reaches outside the 32-bit operand
        u32 f;
        u32 *p = bm + 48;
        __asm__ volatile("btsl %2, (%1)\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(p), "r"(off) : "memory", "cc"); h = mix(h, f & 1);
        __asm__ volatile("btcl %2, (%1)\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(p), "r"(off + 5) : "memory", "cc"); h = mix(h, f & 1);
        __asm__ volatile("btrl %2, 4(%1)\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(p), "r"(off ^ 0x55) : "memory", "cc"); h = mix(h, f & 1);
        __asm__ volatile("btl %2, (%1)\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(p), "r"(off - 3) : "memory", "cc"); h = mix(h, f & 1);
        __asm__ volatile("btsl $77, 8(%1)\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(p) : "memory", "cc"); h = mix(h, f & 1);
        __asm__ volatile("btsw %w2, (%1)\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(p), "r"(off) : "memory", "cc"); h = mix(h, f & 1);
        for (int k = 0; k < 96; k++) h = mix(h, bm[k]);
    }
    return h;
}
static u32 b_scan(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 4000; i++) {
        u32 a = val(), r, f;
        if (a) {
            __asm__ volatile("bsfl %2, %0\n\tpushf\n\tpopl %1" : "=r"(r), "=r"(f) : "r"(a) : "cc"); h = mix(mix(h, r), f & 0x40);
            __asm__ volatile("bsrl %2, %0\n\tpushf\n\tpopl %1" : "=r"(r), "=r"(f) : "r"(a) : "cc"); h = mix(mix(h, r), f & 0x40);
            u16 w = (u16)a;
            if (w) { u16 rw; __asm__ volatile("bsfw %1, %0" : "=r"(rw) : "r"(w) : "cc"); h = mix(h, rw); __asm__ volatile("bsrw %1, %0" : "=r"(rw) : "r"(w) : "cc"); h = mix(h, rw); }
        }
        // zero source: ZF=1 is architecturally defined
        u32 z = 0;
        __asm__ volatile("bsfl %2, %0\n\tpushf\n\tpopl %1" : "=r"(r), "=r"(f) : "r"(z), "0"(0x1234) : "cc"); h = mix(h, f & 0x40);
        u32 s = a;
        __asm__ volatile("bswap %0" : "+r"(s)); h = mix(h, s);
    }
    return h;
}

// ---- exchange / atomics --------------------------------------------------------------------------------------------------
static u32 x_xchg(void) {
    u32 h = 2166136261u;
    static u32 mem;
    for (int i = 0; i < 4000; i++) {
        u32 a = val(), b = val(), c = val(), f;
        mem = b;
        __asm__ volatile("xchgl %0, %1" : "+r"(a), "+m"(mem)); h = mix(mix(h, a), mem);
        u8 bb = (u8)c, cc = (u8)a;
        __asm__ volatile("xchgb %0, %1" : "+q"(bb), "+q"(cc)); h = mix(mix(h, bb), cc);
        mem = c; a = b;
        __asm__ volatile("lock xaddl %1, %0\n\tpushf\n\tpopl %2" : "+m"(mem), "+r"(a), "=r"(f) : : "cc"); h = mix(mix(mix(h, a), mem), f & ALLF);
        u16 m16 = (u16)b, a16 = (u16)c;
        __asm__ volatile("xaddw %1, %0\n\tpushf\n\tpopl %2" : "+m"(m16), "+r"(a16), "=r"(f) : : "cc"); h = mix(mix(mix(h, a16), m16), f & ALLF);
        // cmpxchg: equal and not equal
        mem = b; u32 acc = (i & 1) ? b : ~b, nv = c;
        __asm__ volatile("lock cmpxchgl %3, %1\n\tpushf\n\tpopl %2" : "+a"(acc), "+m"(mem), "=r"(f) : "r"(nv) : "cc"); h = mix(mix(mix(h, acc), mem), f & ALLF);
        u8 m8 = (u8)b, n8 = (u8)c; u32 a8 = (i & 2) ? b : b + 1;
        __asm__ volatile("cmpxchgb %3, %1\n\tpushf\n\tpopl %2" : "+a"(a8), "+m"(m8), "=r"(f) : "q"(n8) : "cc"); h = mix(mix(mix(h, a8), m8), f & ALLF);
        // cmpxchg8b
        static volatile uint64_t m64;
        m64 = ((uint64_t)b << 32) | c;
        u32 ea = (i & 1) ? c : ~c, ed = (i & 1) ? b : ~b, ebx = a, ecx = ~a;
        __asm__ volatile("lock cmpxchg8b %2\n\tpushf\n\tpopl %3" : "+a"(ea), "+d"(ed), "+m"(m64), "=r"(f) : "b"(ebx), "c"(ecx) : "cc");
        h = mix(mix(mix(mix(h, ea), ed), (u32)m64), (u32)(m64 >> 32)); h = mix(h, f & 0x40);
    }
    return h;
}

// ---- flags <-> registers ---------------------------------------------------------------------------------------------------
static u32 f_lahf(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 4000; i++) {
        u32 a = val(), b = val(), r, s;
        __asm__ volatile("cmpl %3, %2\n\tlahf\n\tmovzbl %%ah, %0\n\tmovl %4, %%eax\n\tsahf\n\tsetc %%al\n\tsetz %%ah\n\tmovzwl %%ax, %1"
                         : "=&r"(r), "=&r"(s) : "r"(a), "r"(b), "r"(val() & 0xd5) : "eax", "cc");
        h = mix(mix(h, r & 0xd5), s);
        u32 f;
        __asm__ volatile("stc\n\tpushf\n\tpopl %0" : "=r"(f) : : "cc"); h = mix(h, f & ALLF);
        __asm__ volatile("clc\n\tpushf\n\tpopl %0" : "=r"(f) : : "cc"); h = mix(h, f & ALLF);
        __asm__ volatile("cmpl %2, %1\n\tcmc\n\tpushf\n\tpopl %0" : "=r"(f) : "r"(a), "r"(b) : "cc"); h = mix(h, f & ALLF);
        // popf with arithmetic bits and DF; then string op direction proves DF took effect
        u32 pat = val() & (ALLF | 0x400), fl2, d;
        u8 buf[4] = {1, 2, 3, 4};
        u8 *p = buf + 2;
        __asm__ volatile("pushl %3\n\tpopf\n\tpushf\n\tpopl %0\n\tlodsb\n\tcld\n\tmovl %%esi, %1" : "=&r"(fl2), "=r"(d), "+S"(p) : "r"(pat) : "memory", "cc");
        h = mix(mix(h, fl2 & (ALLF | 0x400)), d - (u32)(uintptr_t)buf);
    }
    return h;
}

// ---- setcc / cmovcc for every condition code ------------------------------------------------------------------------------------
#define CC16(M) M(o) M(no) M(b) M(ae) M(e) M(ne) M(be) M(a) M(s) M(ns) M(p) M(np) M(l) M(ge) M(le) M(g)
static u32 c_cc(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 3000; i++) {
        u32 a = val(), b = val(), bits = 0, mv = 0;
        u8 r;
#define ONE(c) \
        __asm__ volatile("cmpl %2, %1\n\tset" #c " %0" : "=q"(r) : "r"(a), "r"(b) : "cc"); bits = bits * 2 + r; \
        { u32 t = 0x1111, s = 0x2222; __asm__ volatile("cmpl %2, %1\n\tcmov" #c "l %3, %0" : "+r"(t) : "r"(a), "r"(b), "r"(s) : "cc"); mv = mv * 3 + (t == 0x2222); }
        CC16(ONE)
#undef ONE
        h = mix(mix(h, bits), mv);
        // same with a memory source and 16-bit destination
        u32 m = val(); u16 d16 = 0x1234;
        __asm__ volatile("cmpl %2, %1\n\tcmovlw %3, %0" : "+r"(d16) : "r"(a), "r"(b), "m"(m) : "cc");
        h = mix(h, d16);
    }
    return h;
}

static u32 x_xlat(void) {
    u32 h = 2166136261u;
    static u8 tab[256];
    for (int i = 0; i < 256; i++) tab[i] = (u8)(i * 7 + 3);
    for (int i = 0; i < 4000; i++) {
        u32 a = val() & 0xff, r;
        __asm__ volatile("xlatb" : "=a"(r) : "0"(a), "b"(tab) : "memory");
        h = mix(h, r & 0xff);
    }
    return h;
}

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define E(n) {#n, n}
int main(void) {
    static const struct T tests[] = {
        E(s_movsb), E(s_movsw), E(s_movsl), E(s_movsb_df), E(s_movsl_df), E(s_stosb), E(s_stosw), E(s_stosl), E(s_stosl_df), E(s_single),
        E(s_repe_cmpsb), E(s_repne_cmpsb), E(s_repe_cmpsl), E(s_repne_scasb), E(s_repe_scasb), E(s_repne_scasl), E(s_cmps_single), E(s_scas_single), E(s_lods),
        E(b_regs), E(b_mem), E(b_scan), E(x_xchg), E(f_lahf), E(c_cc), E(x_xlat),
    };
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0x13572468u + i * 7919u;
        u32 a = tests[i].f();
        rs = 0x13572468u + i * 7919u;
        u32 b = tests[i].f();
        printf("%-16s %08x%s\n", tests[i].name, a, a == b ? "" : " MISMATCH");
    }
    return 0;
}
