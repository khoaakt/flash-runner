// flags: -msse2 -mfpmath=sse
// SSE/SSE2 FP arithmetic (add sub mul div min max sqrt, scalar + packed, float + double) over edge-case
// pools under every rounding mode and FTZ/DAZ setting; result bits AND the sticky MXCSR flags are hashed.
#include "sse_fp_common.h"

#define FP_OPS(X) X(add) X(sub) X(mul) X(div) X(min) X(max)

#define DEF(op) \
    static __attribute__((noinline)) __m128 op##_ps(__m128 a, __m128 b) { asm volatile(#op "ps %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128 op##_ss(__m128 a, __m128 b) { asm volatile(#op "ss %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128 op##_psm(__m128 a, const __m128 *p) { asm volatile(#op "ps %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128 op##_ssm(__m128 a, const float *p) { asm volatile(#op "ss %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128d op##_pd(__m128d a, __m128d b) { asm volatile(#op "pd %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128d op##_sd(__m128d a, __m128d b) { asm volatile(#op "sd %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128d op##_pdm(__m128d a, const __m128d *p) { asm volatile(#op "pd %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128d op##_sdm(__m128d a, const double *p) { asm volatile(#op "sd %1, %0" : "+x"(a) : "m"(*p)); return a; }
FP_OPS(DEF)

#define UN_OPS(X) X(sqrt)
#define DEFU(op) \
    static __attribute__((noinline)) __m128 op##_ps(__m128 a, __m128 b) { asm volatile(#op "ps %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128 op##_ss(__m128 a, __m128 b) { asm volatile(#op "ss %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128d op##_pd(__m128d a, __m128d b) { asm volatile(#op "pd %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128d op##_sd(__m128d a, __m128d b) { asm volatile(#op "sd %1, %0" : "+x"(a) : "x"(b)); return a; }
UN_OPS(DEFU)

// Pairs where both operands are NaN are skipped: which NaN the hardware returns there is
// implementation-defined enough that QEMU (our oracle) differs from the SDM; unit-tested in Rust.
static u32 fix32(u32 a, u32 b) { return (isnan32(a) && isnan32(b)) ? 0x3f800000 : b; }
static u64 fix64(u64 a, u64 b) { return (isnan64(a) && isnan64(b)) ? 0x3ff0000000000000ULL : b; }

static void mix_flags(void) {
    u32 c = getcsr();
    hmix32(c & 0x3f);
}

#define RUN_OP(op) \
    for (int c = 0; c <= NCFG; c++) { \
        char nm[48], cn[16]; \
        cfg_name(c, cn); \
        u32 cv = cfg_value(c); \
        V a, b, r; \
        for (int i = 0; i < NF32; i++) \
            for (int j = 0; j < NF32; j++) { \
                u32 x0 = pool32[i], y0 = fix32(x0, pool32[j]); \
                u32 x1 = pool32[(i + 1) % NF32], y1 = fix32(x1, pool32[(j + 3) % NF32]); \
                u32 x2 = pool32[(i + 7) % NF32], y2 = fix32(x2, pool32[(j + 11) % NF32]); \
                u32 x3 = pool32[(i + 19) % NF32], y3 = fix32(x3, pool32[(j + 29) % NF32]); \
                a.u[0] = x0; a.u[1] = x1; a.u[2] = x2; a.u[3] = x3; \
                b.u[0] = y0; b.u[1] = y1; b.u[2] = y2; b.u[3] = y3; \
                setcsr(cv); r.f = op##_ps(a.f, b.f); mix_flags(); hmix128(&r); \
                setcsr(cv); r.f = op##_ss(a.f, b.f); mix_flags(); hmix128(&r); \
                if ((i + j) % 5 == 0) { \
                    setcsr(cv); r.f = op##_psm(a.f, &b.f); mix_flags(); hmix128(&r); \
                    setcsr(cv); r.f = op##_ssm(a.f, (const float *)&b.f); mix_flags(); hmix128(&r); \
                } \
            } \
        sprintf(nm, #op "ps/ss %s", cn); \
        report(nm); \
        for (int i = 0; i < NF64; i++) \
            for (int j = 0; j < NF64; j++) { \
                u64 x0 = pool64[i], y0 = fix64(x0, pool64[j]); \
                u64 x1 = pool64[(i + 5) % NF64], y1 = fix64(x1, pool64[(j + 13) % NF64]); \
                a.q[0] = x0; a.q[1] = x1; b.q[0] = y0; b.q[1] = y1; \
                setcsr(cv); r.d = op##_pd(a.d, b.d); mix_flags(); hmix128(&r); \
                setcsr(cv); r.d = op##_sd(a.d, b.d); mix_flags(); hmix128(&r); \
                if ((i + j) % 5 == 0) { \
                    setcsr(cv); r.d = op##_pdm(a.d, &b.d); mix_flags(); hmix128(&r); \
                    setcsr(cv); r.d = op##_sdm(a.d, (const double *)&b.d); mix_flags(); hmix128(&r); \
                } \
            } \
        sprintf(nm, #op "pd/sd %s", cn); \
        report(nm); \
    }

#define RUN_UN(op) \
    for (int c = 0; c <= NCFG; c++) { \
        char nm[48], cn[16]; \
        cfg_name(c, cn); \
        u32 cv = cfg_value(c); \
        V a, b, r; \
        for (int i = 0; i < NF32; i++) { \
            a.u[0] = 0x3f800000; a.u[1] = 0x40000000; a.u[2] = 0x40400000; a.u[3] = 0x40800000; \
            b.u[0] = pool32[i]; b.u[1] = pool32[(i + 1) % NF32]; b.u[2] = pool32[(i + 2) % NF32]; b.u[3] = pool32[(i + 3) % NF32]; \
            setcsr(cv); r.f = op##_ps(a.f, b.f); mix_flags(); hmix128(&r); \
            setcsr(cv); r.f = op##_ss(a.f, b.f); mix_flags(); hmix128(&r); \
        } \
        sprintf(nm, #op "ps/ss %s", cn); \
        report(nm); \
        for (int i = 0; i < NF64; i++) { \
            a.q[0] = 0x3ff0000000000000ULL; a.q[1] = 0x4000000000000000ULL; \
            b.q[0] = pool64[i]; b.q[1] = pool64[(i + 1) % NF64]; \
            setcsr(cv); r.d = op##_pd(a.d, b.d); mix_flags(); hmix128(&r); \
            setcsr(cv); r.d = op##_sd(a.d, b.d); mix_flags(); hmix128(&r); \
        } \
        sprintf(nm, #op "pd/sd %s", cn); \
        report(nm); \
    }

int main(int argc, char **argv) {
    SETUP(argc);
    init_fp_pools();
    FP_OPS(RUN_OP)
    UN_OPS(RUN_UN)
    setcsr(0x1f80);
    return 0;
}
