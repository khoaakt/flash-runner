// flags: -msse2
// SSE/SSE2 data movement: every mov form, aligned/unaligned memory, merges vs. zero-extension, masks.
#include "sse_common.h"

static unsigned char buf[256] __attribute__((aligned(64)));
static unsigned char buf2[256] __attribute__((aligned(64)));

static void fill(unsigned char *b, unsigned seed) {
    for (int i = 0; i < 256; i++) b[i] = (unsigned char)(i * 37 + seed + (i >> 3) * 11);
}
static void dump(const char *name) {
    for (int i = 0; i < 256; i += 8) hmix64(buf + i);
    report(name);
}

// ---- 128-bit loads/stores ---------------------------------------------------------------------
#define LD(insn) static __attribute__((noinline)) __m128i ld_##insn(const void *p) { __m128i r; asm(#insn " %1, %0" : "=x"(r) : "m"(*(const char (*)[16])p)); return r; }
#define ST(insn) static __attribute__((noinline)) void st_##insn(void *p, __m128i v) { asm volatile(#insn " %1, %0" : "=m"(*(char (*)[16])p) : "x"(v) : "memory"); }
LD(movdqa) LD(movdqu) LD(movaps) LD(movups) LD(movapd) LD(movupd)
ST(movdqa) ST(movdqu) ST(movaps) ST(movups) ST(movapd) ST(movupd) ST(movntdq) ST(movntps) ST(movntpd)

#define RR(insn) static __attribute__((noinline)) __m128i rr_##insn(__m128i a, __m128i b) { asm(#insn " %1, %0" : "+x"(a) : "x"(b)); return a; }
RR(movdqa) RR(movaps) RR(movapd) RR(movdqu) RR(movups) RR(movupd) RR(movhlps) RR(movlhps) RR(movss) RR(movsd) RR(movq)

// ---- scalar / half moves ----------------------------------------------------------------------
static __attribute__((noinline)) __m128i ld_movss(__m128i a, const void *p) { asm("movss %1, %0" : "+x"(a) : "m"(*(const float *)p)); return a; }
static __attribute__((noinline)) __m128i ld_movsd(__m128i a, const void *p) { asm("movsd %1, %0" : "+x"(a) : "m"(*(const double *)p)); return a; }
static __attribute__((noinline)) __m128i ld_movss_z(const void *p) { __m128i r = _mm_set1_epi32(-1); asm("pxor %0, %0\n\tpcmpeqd %0, %0\n\tmovss %1, %0" : "=&x"(r) : "m"(*(const float *)p)); return r; }
static __attribute__((noinline)) __m128i ld_movsd_z(const void *p) { __m128i r; asm("pcmpeqd %0, %0\n\tmovsd %1, %0" : "=&x"(r) : "m"(*(const double *)p)); return r; }
static __attribute__((noinline)) void st_movss(void *p, __m128i v) { asm volatile("movss %1, %0" : "=m"(*(float *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) void st_movsd(void *p, __m128i v) { asm volatile("movsd %1, %0" : "=m"(*(double *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) __m128i ld_movlps(__m128i a, const void *p) { asm("movlps %1, %0" : "+x"(a) : "m"(*(const long long *)p)); return a; }
static __attribute__((noinline)) __m128i ld_movhps(__m128i a, const void *p) { asm("movhps %1, %0" : "+x"(a) : "m"(*(const long long *)p)); return a; }
static __attribute__((noinline)) __m128i ld_movlpd(__m128i a, const void *p) { asm("movlpd %1, %0" : "+x"(a) : "m"(*(const long long *)p)); return a; }
static __attribute__((noinline)) __m128i ld_movhpd(__m128i a, const void *p) { asm("movhpd %1, %0" : "+x"(a) : "m"(*(const long long *)p)); return a; }
static __attribute__((noinline)) void st_movlps(void *p, __m128i v) { asm volatile("movlps %1, %0" : "=m"(*(long long *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) void st_movhps(void *p, __m128i v) { asm volatile("movhps %1, %0" : "=m"(*(long long *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) void st_movlpd(void *p, __m128i v) { asm volatile("movlpd %1, %0" : "=m"(*(long long *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) void st_movhpd(void *p, __m128i v) { asm volatile("movhpd %1, %0" : "=m"(*(long long *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) __m128i ld_movq(const void *p) { __m128i r; asm("pcmpeqd %0, %0\n\tmovq %1, %0" : "=&x"(r) : "m"(*(const long long *)p)); return r; }
static __attribute__((noinline)) void st_movq(void *p, __m128i v) { asm volatile("movq %1, %0" : "=m"(*(long long *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) __m128i ld_movd(const void *p) { __m128i r; asm("pcmpeqd %0, %0\n\tmovd %1, %0" : "=&x"(r) : "m"(*(const int *)p)); return r; }
static __attribute__((noinline)) void st_movd(void *p, __m128i v) { asm volatile("movd %1, %0" : "=m"(*(int *)p) : "x"(v) : "memory"); }
static __attribute__((noinline)) __m128i gpr_to_x(u32 v) { __m128i r; asm("pcmpeqd %0, %0\n\tmovd %1, %0" : "=&x"(r) : "r"(v)); return r; }
static __attribute__((noinline)) u32 x_to_gpr(__m128i v) { u32 r; asm("movd %1, %0" : "=r"(r) : "x"(v)); return r; }
static __attribute__((noinline)) void st_movnti(void *p, u32 v) { asm volatile("movnti %1, %0" : "=m"(*(u32 *)p) : "r"(v) : "memory"); }

static __attribute__((noinline)) u32 movmskps(__m128i v) { u32 r; asm("movmskps %1, %0" : "=r"(r) : "x"(v)); return r; }
static __attribute__((noinline)) u32 movmskpd(__m128i v) { u32 r; asm("movmskpd %1, %0" : "=r"(r) : "x"(v)); return r; }
static __attribute__((noinline)) u32 pmovmskb(__m128i v) { u32 r; asm("pmovmskb %1, %0" : "=r"(r) : "x"(v)); return r; }

#define PEX(n) static __attribute__((noinline)) u32 pextrw_##n(__m128i v) { u32 r; asm("pextrw $" #n ", %1, %0" : "=r"(r) : "x"(v)); return r; }
#define PIN(n) static __attribute__((noinline)) __m128i pinsrw_##n(__m128i v, u32 w) { asm("pinsrw $" #n ", %1, %0" : "+x"(v) : "r"(w)); return v; } \
               static __attribute__((noinline)) __m128i pinsrwm_##n(__m128i v, const u16 *p) { asm("pinsrw $" #n ", %1, %0" : "+x"(v) : "m"(*p)); return v; }
PEX(0) PEX(1) PEX(2) PEX(3) PEX(4) PEX(5) PEX(6) PEX(7) PEX(8) PEX(15)
PIN(0) PIN(1) PIN(2) PIN(3) PIN(4) PIN(5) PIN(6) PIN(7) PIN(9)

static __attribute__((noinline)) void maskmov(__m128i d, __m128i m, void *p) {
    asm volatile("maskmovdqu %1, %0" :: "x"(d), "x"(m), "D"(p) : "memory");
}

int main(int argc, char **argv) {
    SETUP(argc);
    init_pool();
    __m128i v;

    // aligned loads / stores of every flavour, all 16-byte offsets within the buffer
    fill(buf, 1);
    for (int off = 0; off < 256; off += 16) {
        v = ld_movdqa(buf + off); hmix128(&v);
        v = ld_movaps(buf + off); hmix128(&v);
        v = ld_movapd(buf + off); hmix128(&v);
    }
    report("aligned loads");
    for (int off = 0; off < 64; off++) {
        v = ld_movdqu(buf + off); hmix128(&v);
        v = ld_movups(buf + off); hmix128(&v);
        v = ld_movupd(buf + off); hmix128(&v);
    }
    report("unaligned loads");
    memset(buf, 0xa5, 256);
    for (int i = 0; i < 16; i++) st_movdqa(buf + 16 * i, pool[i]);
    dump("movdqa store");
    for (int i = 0; i < 16; i++) st_movaps(buf + 16 * i, pool[i + 1]);
    dump("movaps store");
    for (int i = 0; i < 16; i++) st_movapd(buf + 16 * i, pool[i + 2]);
    dump("movapd store");
    for (int i = 0; i < 16; i++) st_movntdq(buf + 16 * i, pool[i + 3]);
    dump("movntdq store");
    for (int i = 0; i < 16; i++) st_movntps(buf + 16 * i, pool[i + 4]);
    dump("movntps store");
    for (int i = 0; i < 16; i++) st_movntpd(buf + 16 * i, pool[i + 5]);
    dump("movntpd store");
    memset(buf, 0x5a, 256);
    for (int i = 0; i < 20; i++) { st_movdqu(buf + 3 + 13 * i, pool[i]); st_movups(buf + 7 + 5 * i, pool[i + 1]); st_movupd(buf + 1 + 9 * i, pool[i + 2]); }
    dump("unaligned stores");

    // register-register forms
    for (int i = 0; i < 12; i++) {
        __m128i a = pool[i], b = pool[i + 12];
        v = rr_movdqa(a, b); hmix128(&v);
        v = rr_movaps(a, b); hmix128(&v);
        v = rr_movapd(a, b); hmix128(&v);
        v = rr_movdqu(a, b); hmix128(&v);
        v = rr_movups(a, b); hmix128(&v);
        v = rr_movupd(a, b); hmix128(&v);
        v = rr_movhlps(a, b); hmix128(&v);
        v = rr_movlhps(a, b); hmix128(&v);
        v = rr_movss(a, b); hmix128(&v);
        v = rr_movsd(a, b); hmix128(&v);
        v = rr_movq(a, b); hmix128(&v);
    }
    report("reg-reg moves");

    // scalar moves: loads (merge vs zero-extend), stores
    fill(buf, 9);
    for (int i = 0; i < 20; i++) {
        v = ld_movss(pool[i], buf + 3 * i); hmix128(&v);
        v = ld_movsd(pool[i], buf + 5 * i); hmix128(&v);
        v = ld_movss_z(buf + 7 * i); hmix128(&v);
        v = ld_movsd_z(buf + 11 * i); hmix128(&v);
        v = ld_movlps(pool[i], buf + i); hmix128(&v);
        v = ld_movhps(pool[i], buf + i + 1); hmix128(&v);
        v = ld_movlpd(pool[i], buf + i + 2); hmix128(&v);
        v = ld_movhpd(pool[i], buf + i + 3); hmix128(&v);
        v = ld_movq(buf + 3 * i); hmix128(&v);
        v = ld_movd(buf + 3 * i + 1); hmix128(&v);
    }
    report("scalar/half loads");
    memset(buf, 0x33, 256);
    for (int i = 0; i < 20; i++) {
        st_movss(buf + 12 * i, pool[i]);
        st_movsd(buf + 12 * i + 4, pool[i + 1]);
        st_movlps(buf + 12 * i + 100, pool[i + 2]);
        st_movhps(buf + 12 * i + 101, pool[i + 3]);
        st_movlpd(buf + 12 * i + 102, pool[i + 4]);
        st_movhpd(buf + 12 * i + 103, pool[i + 5]);
        st_movq(buf + 12 * i + 50, pool[i + 6]);
        st_movd(buf + 12 * i + 51, pool[i + 7]);
        st_movnti(buf + 12 * i + 52, (u32)(rnd64() >> 7));
    }
    dump("scalar/half stores");

    // GPR <-> XMM
    for (int i = 0; i < NPOOL; i++) {
        u32 g = (u32)(rnd64() >> 3);
        v = gpr_to_x(g); hmix128(&v);
        hmix32(x_to_gpr(pool[i]));
    }
    report("movd gpr");

    // masks and element extract/insert
    for (int i = 0; i < NPOOL; i++) {
        hmix32(movmskps(pool[i])); hmix32(movmskpd(pool[i])); hmix32(pmovmskb(pool[i]));
        hmix32(pextrw_0(pool[i])); hmix32(pextrw_1(pool[i])); hmix32(pextrw_2(pool[i])); hmix32(pextrw_3(pool[i]));
        hmix32(pextrw_4(pool[i])); hmix32(pextrw_5(pool[i])); hmix32(pextrw_6(pool[i])); hmix32(pextrw_7(pool[i]));
        hmix32(pextrw_8(pool[i])); hmix32(pextrw_15(pool[i]));
    }
    report("movmsk/pextrw");
    for (int i = 0; i < NPOOL; i++) {
        u32 w = (u32)(rnd64() >> 5);
        u16 mw = (u16)(rnd64() >> 9);
        v = pinsrw_0(pool[i], w); hmix128(&v); v = pinsrw_1(pool[i], w); hmix128(&v); v = pinsrw_2(pool[i], w); hmix128(&v);
        v = pinsrw_3(pool[i], w); hmix128(&v); v = pinsrw_4(pool[i], w); hmix128(&v); v = pinsrw_5(pool[i], w); hmix128(&v);
        v = pinsrw_6(pool[i], w); hmix128(&v); v = pinsrw_7(pool[i], w); hmix128(&v); v = pinsrw_9(pool[i], w); hmix128(&v);
        v = pinsrwm_0(pool[i], &mw); hmix128(&v); v = pinsrwm_5(pool[i], &mw); hmix128(&v); v = pinsrwm_7(pool[i], &mw); hmix128(&v);
    }
    report("pinsrw");

    // maskmovdqu: only bytes whose mask MSB is set are stored
    for (int i = 0; i < 24; i++) {
        fill(buf, 77);
        maskmov(pool[i], pool[(i * 5 + 1) % NPOOL], buf + 16 + (i % 7));
        for (int k = 0; k < 64; k += 8) hmix64(buf + 8 + k);
    }
    report("maskmovdqu");

    // cache/ordering instructions are no-ops for us but must execute
    asm volatile("clflush %0\n\tsfence\n\tlfence\n\tmfence\n\tprefetcht0 %0\n\tprefetchnta %0\n\tpause" :: "m"(buf[0]) : "memory");
    hmix32(0xc0ffee);
    report("clflush/fences");

    // MXCSR round trip
    {
        unsigned saved = _mm_getcsr();
        static const unsigned vals[] = {0x1f80, 0x1f80 | 0x2000, 0x1f80 | 0x4000, 0x1f80 | 0x6000, 0x9fc0, 0x1f80 | 0x40, 0x1f80 | 0x8000};
        for (unsigned k = 0; k < sizeof vals / sizeof vals[0]; k++) {
            _mm_setcsr(vals[k]);
            hmix32(_mm_getcsr());
        }
        _mm_setcsr(saved);
        hmix32(_mm_getcsr());
    }
    report("ldmxcsr/stmxcsr");
    return 0;
}
