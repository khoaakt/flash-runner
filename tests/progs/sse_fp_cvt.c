// flags: -msse2 -mfpmath=sse
// All SSE/SSE2 conversions under every rounding mode and DAZ/FTZ setting, bits + MXCSR flags hashed.
#include "sse_fp_common.h"

#define X2X(name, insn, T) \
    static __attribute__((noinline)) T name##_r(T a, T b) { asm volatile(insn " %1, %0" : "=x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) T name##_m(T a, const T *p) { asm volatile(insn " %1, %0" : "=x"(a) : "m"(*p)); return a; }
X2X(cvtps2pd, "cvtps2pd", __m128) X2X(cvtpd2ps, "cvtpd2ps", __m128) X2X(cvtdq2ps, "cvtdq2ps", __m128)
X2X(cvtps2dq, "cvtps2dq", __m128) X2X(cvttps2dq, "cvttps2dq", __m128) X2X(cvtdq2pd, "cvtdq2pd", __m128)
X2X(cvtpd2dq, "cvtpd2dq", __m128) X2X(cvttpd2dq, "cvttpd2dq", __m128)

// scalar forms merge into the destination, so seed it with a recognisable pattern
#define SC(name, insn) \
    static __attribute__((noinline)) __m128 name(__m128 a, __m128 b) { asm volatile(insn " %1, %0" : "+x"(a) : "x"(b)); return a; }
SC(cvtss2sd, "cvtss2sd") SC(cvtsd2ss, "cvtsd2ss")
static __attribute__((noinline)) __m128 cvtss2sd_m(__m128 a, const float *p) { asm volatile("cvtss2sd %1, %0" : "+x"(a) : "m"(*p)); return a; }
static __attribute__((noinline)) __m128 cvtsd2ss_m(__m128 a, const double *p) { asm volatile("cvtsd2ss %1, %0" : "+x"(a) : "m"(*p)); return a; }
static __attribute__((noinline)) __m128 cvtsi2ss(__m128 a, int v) { asm volatile("cvtsi2ss %1, %0" : "+x"(a) : "r"(v)); return a; }
static __attribute__((noinline)) __m128 cvtsi2sd(__m128 a, int v) { asm volatile("cvtsi2sd %1, %0" : "+x"(a) : "r"(v)); return a; }
static __attribute__((noinline)) __m128 cvtsi2ss_m(__m128 a, const int *p) { asm volatile("cvtsi2ss %1, %0" : "+x"(a) : "m"(*p)); return a; }
static __attribute__((noinline)) __m128 cvtsi2sd_m(__m128 a, const int *p) { asm volatile("cvtsi2sd %1, %0" : "+x"(a) : "m"(*p)); return a; }
static __attribute__((noinline)) int cvtss2si(__m128 b) { int r; asm volatile("cvtss2si %1, %0" : "=r"(r) : "x"(b)); return r; }
static __attribute__((noinline)) int cvttss2si(__m128 b) { int r; asm volatile("cvttss2si %1, %0" : "=r"(r) : "x"(b)); return r; }
static __attribute__((noinline)) int cvtsd2si(__m128 b) { int r; asm volatile("cvtsd2si %1, %0" : "=r"(r) : "x"(b)); return r; }
static __attribute__((noinline)) int cvttsd2si(__m128 b) { int r; asm volatile("cvttsd2si %1, %0" : "=r"(r) : "x"(b)); return r; }
static __attribute__((noinline)) int cvtss2si_m(const float *p) { int r; asm volatile("cvtss2si %1, %0" : "=r"(r) : "m"(*p)); return r; }
static __attribute__((noinline)) int cvttsd2si_m(const double *p) { int r; asm volatile("cvttsd2si %1, %0" : "=r"(r) : "m"(*p)); return r; }

static int ipool[64];
static void init_ipool(void) {
    static const int e[] = {0, 1, -1, 2, -2, 3, 16777215, 16777216, 16777217, -16777217, 16777219, 33554435, 2147483647, -2147483647 - 1, -2147483647,
                            1073741824, 1073741823, 2147483520, 2147483584, 2147483583, 1000000007, -1000000007, 8388609, 8388607, 123456789, -123456789,
                            0x7fffff80, 0x7fffffc0, 0x7fffffa0, 255, 256, 65535};
    int n = sizeof e / sizeof e[0];
    for (int i = 0; i < 64; i++) ipool[i] = i < n ? e[i] : (int)(rnd64() >> (i % 5 * 4));
}

static void mixf(void) { hmix32(getcsr() & 0x3f); }

int main(int argc, char **argv) {
    SETUP(argc);
    init_fp_pools();
    init_ipool();
    V a, b, r, seed;
    seed.u[0] = 0x11111111; seed.u[1] = 0x22222222; seed.u[2] = 0x33333333; seed.u[3] = 0x44444444;
    for (int c = 0; c < NCFG; c++) {
        char nm[64], cn[16];
        cfg_name(c, cn);
        u32 cv = cfg_value(c);
        // float source conversions (4 lanes of f32 / 2 lanes of f64 in the source)
        for (int i = 0; i < NF32; i++) {
            for (int k = 0; k < 4; k++) a.u[k] = pool32[(i + 3 * k) % NF32];
            setcsr(cv); r.f = cvtps2pd_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtps2pd_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtps2dq_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtps2dq_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvttps2dq_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtss2sd(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtss2sd_m(seed.f, (const float *)&a.f); mixf(); hmix128(&r);
            setcsr(cv); hmix32(cvtss2si(a.f)); mixf();
            setcsr(cv); hmix32(cvttss2si(a.f)); mixf();
            setcsr(cv); hmix32(cvtss2si_m((const float *)&a.f)); mixf();
        }
        sprintf(nm, "cvt f32 sources %s", cn); report(nm);
        for (int i = 0; i < NF64; i++) {
            for (int k = 0; k < 2; k++) a.q[k] = pool64[(i + 7 * k) % NF64];
            setcsr(cv); r.f = cvtpd2ps_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtpd2ps_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtpd2dq_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtpd2dq_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvttpd2dq_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvttpd2dq_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtsd2ss(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtsd2ss_m(seed.f, (const double *)&a.f); mixf(); hmix128(&r);
            setcsr(cv); hmix32(cvtsd2si(a.f)); mixf();
            setcsr(cv); hmix32(cvttsd2si(a.f)); mixf();
            setcsr(cv); hmix32(cvttsd2si_m((const double *)&a.f)); mixf();
        }
        sprintf(nm, "cvt f64 sources %s", cn); report(nm);
        for (int i = 0; i < 64; i++) {
            for (int k = 0; k < 4; k++) a.u[k] = (u32)ipool[(i + 5 * k) % 64];
            setcsr(cv); r.f = cvtdq2ps_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtdq2ps_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtdq2pd_r(seed.f, a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtdq2pd_m(seed.f, &a.f); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtsi2ss(seed.f, ipool[i]); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtsi2sd(seed.f, ipool[i]); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtsi2ss_m(seed.f, &ipool[i]); mixf(); hmix128(&r);
            setcsr(cv); r.f = cvtsi2sd_m(seed.f, &ipool[i]); mixf(); hmix128(&r);
        }
        sprintf(nm, "cvt int sources %s", cn); report(nm);
    }
    setcsr(0x1f80);
    return 0;
}
