// flags: -msse2 -mmmx
// MMX <-> SSE conversions (CVTPI2PS/PD, CVT[T]PS2PI, CVT[T]PD2PI) under every rounding mode.
#include <mmintrin.h>
#include "sse_fp_common.h"

static __attribute__((noinline)) __m128 cvtpi2ps(__m128 a, u64 v) { __m64 x = (__m64)v; asm volatile("cvtpi2ps %1, %0" : "+x"(a) : "y"(x)); _mm_empty(); return a; }
static __attribute__((noinline)) __m128 cvtpi2ps_m(__m128 a, const u64 *p) { asm volatile("cvtpi2ps %1, %0" : "+x"(a) : "m"(*p)); _mm_empty(); return a; }
static __attribute__((noinline)) __m128 cvtpi2pd(__m128 a, u64 v) { __m64 x = (__m64)v; asm volatile("cvtpi2pd %1, %0" : "=x"(a) : "y"(x)); _mm_empty(); return a; }
static __attribute__((noinline)) __m128 cvtpi2pd_m(__m128 a, const u64 *p) { asm volatile("cvtpi2pd %1, %0" : "=x"(a) : "m"(*p)); _mm_empty(); return a; }
static __attribute__((noinline)) u64 cvtps2pi(__m128 a) { __m64 r; asm volatile("cvtps2pi %1, %0" : "=y"(r) : "x"(a)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) u64 cvttps2pi(__m128 a) { __m64 r; asm volatile("cvttps2pi %1, %0" : "=y"(r) : "x"(a)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) u64 cvtps2pi_m(const u64 *p) { __m64 r; asm volatile("cvtps2pi %1, %0" : "=y"(r) : "m"(*p)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) u64 cvtpd2pi(__m128 a) { __m64 r; asm volatile("cvtpd2pi %1, %0" : "=y"(r) : "x"(a)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) u64 cvttpd2pi(__m128 a) { __m64 r; asm volatile("cvttpd2pi %1, %0" : "=y"(r) : "x"(a)); u64 o = (u64)r; _mm_empty(); return o; }
static __attribute__((noinline)) u64 cvttpd2pi_m(const __m128 *p) { __m64 r; asm volatile("cvttpd2pi %1, %0" : "=y"(r) : "m"(*p)); u64 o = (u64)r; _mm_empty(); return o; }

int main(int argc, char **argv) {
    SETUP(argc);
    init_fp_pools();
    V a, r, seed;
    seed.u[0] = 0x11111111; seed.u[1] = 0x22222222; seed.u[2] = 0x33333333; seed.u[3] = 0x44444444;
    for (int c = 0; c < NCFG; c++) {
        char nm[64], cn[16];
        cfg_name(c, cn);
        u32 cv = cfg_value(c);
        for (int i = 0; i < NF32; i++) {
            for (int k = 0; k < 4; k++) a.u[k] = pool32[(i + 3 * k) % NF32];
            u64 t;
            setcsr(cv); t = cvtps2pi(a.f); hmix32(getcsr() & 0x3f); hmix64(&t);
            setcsr(cv); t = cvttps2pi(a.f); hmix32(getcsr() & 0x3f); hmix64(&t);
            setcsr(cv); t = cvtps2pi_m(&a.q[0]); hmix32(getcsr() & 0x3f); hmix64(&t);
        }
        for (int i = 0; i < NF64; i++) {
            for (int k = 0; k < 2; k++) a.q[k] = pool64[(i + 7 * k) % NF64];
            u64 t;
            setcsr(cv); t = cvtpd2pi(a.f); hmix32(getcsr() & 0x3f); hmix64(&t);
            setcsr(cv); t = cvttpd2pi(a.f); hmix32(getcsr() & 0x3f); hmix64(&t);
            setcsr(cv); t = cvttpd2pi_m(&a.f); hmix32(getcsr() & 0x3f); hmix64(&t);
        }
        for (int i = 0; i < 64; i++) {
            u64 v = ((u64)pool32[i] << 32) | pool32[(i * 7 + 1) % NF32];
            setcsr(cv); r.f = cvtpi2ps(seed.f, v); hmix32(getcsr() & 0x3f); hmix128(&r);
            setcsr(cv); r.f = cvtpi2ps_m(seed.f, &v); hmix32(getcsr() & 0x3f); hmix128(&r);
            setcsr(cv); r.f = cvtpi2pd(seed.f, v); hmix32(getcsr() & 0x3f); hmix128(&r);
            setcsr(cv); r.f = cvtpi2pd_m(seed.f, &v); hmix32(getcsr() & 0x3f); hmix128(&r);
        }
        sprintf(nm, "mmx cvt %s", cn);
        report(nm);
    }
    setcsr(0x1f80);
    return 0;
}
