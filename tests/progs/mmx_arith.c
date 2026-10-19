// flags: -msse2 -mmmx
// MMX (64-bit) integer SIMD: register and memory forms, shifts, pack/unpack, shuffles, moves.
#include <mmintrin.h>
#include "sse_common.h"

#define BIN_OPS(X) \
    X(paddb) X(paddw) X(paddd) X(paddq) X(psubb) X(psubw) X(psubd) X(psubq) \
    X(paddsb) X(paddsw) X(paddusb) X(paddusw) X(psubsb) X(psubsw) X(psubusb) X(psubusw) \
    X(pmullw) X(pmulhw) X(pmulhuw) X(pmuludq) X(pmaddwd) X(pavgb) X(pavgw) \
    X(pminub) X(pmaxub) X(pminsw) X(pmaxsw) X(psadbw) \
    X(pcmpeqb) X(pcmpeqw) X(pcmpeqd) X(pcmpgtb) X(pcmpgtw) X(pcmpgtd) \
    X(pand) X(pandn) X(por) X(pxor) \
    X(packsswb) X(packssdw) X(packuswb) \
    X(punpcklbw) X(punpcklwd) X(punpckldq) X(punpckhbw) X(punpckhwd) X(punpckhdq)

#define DEF(op) \
    static __attribute__((noinline)) u64 op##_rr(u64 a, u64 b) { __m64 x = (__m64)a, y = (__m64)b; asm volatile(#op " %1, %0" : "+y"(x) : "y"(y)); u64 r = (u64)x; _mm_empty(); return r; } \
    static __attribute__((noinline)) u64 op##_rm(u64 a, const u64 *p) { __m64 x = (__m64)a; asm volatile(#op " %1, %0" : "+y"(x) : "m"(*p)); u64 r = (u64)x; _mm_empty(); return r; }
BIN_OPS(DEF)

#define RUN(op) \
    for (int i = 0; i < NPOOL; i++) \
        for (int j = 0; j < NPOOL; j++) { \
            u64 a = ((u64 *)&pool[i])[i & 1], b = ((u64 *)&pool[j])[(j >> 1) & 1]; \
            u64 r = op##_rr(a, b); \
            u64 r2 = op##_rm(a, &b); \
            if (r != r2) printf("MISMATCH rr/rm %s %d %d\n", #op, i, j); \
            hmix64(&r); \
        } \
    report(#op);

#define SHIFTS(X) X(psllw) X(pslld) X(psllq) X(psrlw) X(psrld) X(psrlq) X(psraw) X(psrad)
#define DEFS(op) \
    static __attribute__((noinline)) u64 op##_rr(u64 a, u64 c) { __m64 x = (__m64)a, y = (__m64)c; asm volatile(#op " %1, %0" : "+y"(x) : "y"(y)); u64 r = (u64)x; _mm_empty(); return r; } \
    static __attribute__((noinline)) u64 op##_rm(u64 a, const u64 *p) { __m64 x = (__m64)a; asm volatile(#op " %1, %0" : "+y"(x) : "m"(*p)); u64 r = (u64)x; _mm_empty(); return r; }
SHIFTS(DEFS)
#define IMMSH(op, n) static __attribute__((noinline)) u64 op##_i##n(u64 a) { __m64 x = (__m64)a; asm volatile(#op " $" #n ", %0" : "+y"(x)); u64 r = (u64)x; _mm_empty(); return r; }
#define IMMALL(op) IMMSH(op, 0) IMMSH(op, 1) IMMSH(op, 7) IMMSH(op, 15) IMMSH(op, 16) IMMSH(op, 31) IMMSH(op, 32) IMMSH(op, 63) IMMSH(op, 64) IMMSH(op, 255)
SHIFTS(IMMALL)

#define RUNSH(op) \
    { \
        static const u64 counts[] = {0, 1, 3, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 255, 0x100000000ULL, ~0ULL}; \
        for (int i = 0; i < NPOOL; i++) { \
            u64 a = ((u64 *)&pool[i])[i & 1]; \
            for (unsigned k = 0; k < sizeof counts / sizeof counts[0]; k++) { \
                u64 r = op##_rr(a, counts[k]); u64 r2 = op##_rm(a, &counts[k]); \
                if (r != r2) printf("MISMATCH shift %s\n", #op); \
                hmix64(&r); \
            } \
            u64 t; \
            t = op##_i0(a); hmix64(&t); t = op##_i1(a); hmix64(&t); t = op##_i7(a); hmix64(&t); t = op##_i15(a); hmix64(&t); t = op##_i16(a); hmix64(&t); \
            t = op##_i31(a); hmix64(&t); t = op##_i32(a); hmix64(&t); t = op##_i63(a); hmix64(&t); t = op##_i64(a); hmix64(&t); t = op##_i255(a); hmix64(&t); \
        } \
        report(#op); \
    }

#define PSHUFW(n) \
    static __attribute__((noinline)) u64 pshufw_##n(u64 a) { __m64 x = (__m64)a, r; asm volatile("pshufw $" #n ", %1, %0" : "=y"(r) : "y"(x)); u64 o = (u64)r; _mm_empty(); return o; } \
    static __attribute__((noinline)) u64 pshufwm_##n(const u64 *p) { __m64 r; asm volatile("pshufw $" #n ", %1, %0" : "=y"(r) : "m"(*p)); u64 o = (u64)r; _mm_empty(); return o; }
PSHUFW(0) PSHUFW(27) PSHUFW(228) PSHUFW(255) PSHUFW(57) PSHUFW(147) PSHUFW(78) PSHUFW(177) PSHUFW(85) PSHUFW(170)

#define PEX(n) static __attribute__((noinline)) u32 pextrw_##n(u64 a) { __m64 x = (__m64)a; u32 r; asm volatile("pextrw $" #n ", %1, %0" : "=r"(r) : "y"(x)); _mm_empty(); return r; }
#define PIN(n) static __attribute__((noinline)) u64 pinsrw_##n(u64 a, u32 w) { __m64 x = (__m64)a; asm volatile("pinsrw $" #n ", %1, %0" : "+y"(x) : "r"(w)); u64 r = (u64)x; _mm_empty(); return r; } \
               static __attribute__((noinline)) u64 pinsrwm_##n(u64 a, const u16 *p) { __m64 x = (__m64)a; asm volatile("pinsrw $" #n ", %1, %0" : "+y"(x) : "m"(*p)); u64 r = (u64)x; _mm_empty(); return r; }
PEX(0) PEX(1) PEX(2) PEX(3) PEX(4) PEX(7)
PIN(0) PIN(1) PIN(2) PIN(3) PIN(5)

static __attribute__((noinline)) u32 pmovmskb(u64 a) { __m64 x = (__m64)a; u32 r; asm volatile("pmovmskb %1, %0" : "=r"(r) : "y"(x)); _mm_empty(); return r; }
static __attribute__((noinline)) u64 movd_in(u32 v) { __m64 r; asm volatile("movd %1, %0" : "=y"(r) : "r"(v)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) u32 movd_out(u64 a) { __m64 x = (__m64)a; u32 r; asm volatile("movd %1, %0" : "=r"(r) : "y"(x)); _mm_empty(); return r; }
static __attribute__((noinline)) u64 movd_ld(const u32 *p) { __m64 r; asm volatile("movd %1, %0" : "=y"(r) : "m"(*p)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) void movd_st(u32 *p, u64 a) { __m64 x = (__m64)a; asm volatile("movd %1, %0" : "=m"(*p) : "y"(x) : "memory"); _mm_empty(); }
static __attribute__((noinline)) u64 movq_ld(const u64 *p) { __m64 r; asm volatile("movq %1, %0" : "=y"(r) : "m"(*p)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) void movq_st(u64 *p, u64 a) { __m64 x = (__m64)a; asm volatile("movq %1, %0" : "=m"(*p) : "y"(x) : "memory"); _mm_empty(); }
static __attribute__((noinline)) u64 movq_rr(u64 a, u64 b) { __m64 x = (__m64)a, y = (__m64)b; asm volatile("movq %1, %0" : "=y"(x) : "y"(y)); u64 r = (u64)x; _mm_empty(); return r; }
static __attribute__((noinline)) void movntq(u64 *p, u64 a) { __m64 x = (__m64)a; asm volatile("movntq %1, %0" : "=m"(*p) : "y"(x) : "memory"); _mm_empty(); }
static __attribute__((noinline)) void maskmovq(u64 d, u64 m, void *p) { __m64 x = (__m64)d, y = (__m64)m; asm volatile("maskmovq %1, %0" :: "y"(x), "y"(y), "D"(p) : "memory"); _mm_empty(); }
static __attribute__((noinline)) __m128i movq2dq(u64 a) { __m64 x = (__m64)a; __m128i r; asm volatile("movq2dq %1, %0" : "=x"(r) : "y"(x)); _mm_empty(); return r; }
static __attribute__((noinline)) u64 movdq2q(__m128i a) { __m64 r; asm volatile("movdq2q %1, %0" : "=y"(r) : "x"(a)); u64 o = (u64)r; _mm_empty(); return o; }

int main(int argc, char **argv) {
    SETUP(argc);
    init_pool();
    BIN_OPS(RUN)
    SHIFTS(RUNSH)
    for (int i = 0; i < NPOOL; i++) {
        u64 a = ((u64 *)&pool[i])[i & 1], t;
#define PS(n) t = pshufw_##n(a); hmix64(&t); t = pshufwm_##n(&a); hmix64(&t);
        PS(0) PS(27) PS(228) PS(255) PS(57) PS(147) PS(78) PS(177) PS(85) PS(170)
        hmix32(pextrw_0(a)); hmix32(pextrw_1(a)); hmix32(pextrw_2(a)); hmix32(pextrw_3(a)); hmix32(pextrw_4(a)); hmix32(pextrw_7(a));
        u32 w = (u32)(rnd64() >> 9);
        u16 mw = (u16)(rnd64() >> 11);
        t = pinsrw_0(a, w); hmix64(&t); t = pinsrw_1(a, w); hmix64(&t); t = pinsrw_2(a, w); hmix64(&t); t = pinsrw_3(a, w); hmix64(&t); t = pinsrw_5(a, w); hmix64(&t);
        t = pinsrwm_0(a, &mw); hmix64(&t); t = pinsrwm_2(a, &mw); hmix64(&t);
        hmix32(pmovmskb(a));
    }
    report("pshufw/pextrw/pinsrw/pmovmskb");

    // moves
    static u64 buf[16];
    for (int i = 0; i < NPOOL; i++) {
        u64 a = ((u64 *)&pool[i])[i & 1], t;
        u32 v32 = (u32)(a >> 5);
        t = movd_in(v32); hmix64(&t);
        hmix32(movd_out(a));
        t = movd_ld(&v32); hmix64(&t);
        movd_st((u32 *)((char *)buf + 3), a); hmix64(&buf[0]); hmix64(&buf[1]);
        buf[2] = a; t = movq_ld(&buf[2]); hmix64(&t);
        movq_st(&buf[3], a ^ 0x5555); hmix64(&buf[3]);
        t = movq_rr(a, a >> 1); hmix64(&t);
        movntq(&buf[4], ~a); hmix64(&buf[4]);
        memset(buf + 6, 0xcc, 64);
        maskmovq(a, ((u64 *)&pool[(i * 3 + 1) % NPOOL])[1], (char *)(buf + 6) + (i & 3));
        for (int k = 6; k < 14; k++) hmix64(&buf[k]);
        __m128i x = movq2dq(a); hmix128(&x);
        t = movdq2q(pool[i]); hmix64(&t);
    }
    report("mmx moves");

    // intrinsics sanity (compiler picks the instructions)
    {
        __m64 a = _mm_set_pi16(1, -2, 300, -400), b = _mm_set_pi16(5, 6, -7, 8);
        __m64 r = _mm_add_pi16(a, b);
        r = _mm_mullo_pi16(r, _mm_set1_pi16(3));
        r = _mm_unpacklo_pi8(r, _mm_setzero_si64());
        u64 o = (u64)r;
        _mm_empty();
        hmix64(&o);
        report("intrinsics");
    }
    return 0;
}
