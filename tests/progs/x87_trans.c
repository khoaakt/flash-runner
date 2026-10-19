// flags: -O1
/* FSIN/FCOS/FSINCOS/FPTAN/FPATAN/F2XM1/FYL2X/FYL2XP1. QEMU evaluates the trigonometric ones through host
 * libm at double precision (and rounds the operand to double), so inputs are exact doubles, results are
 * printed with 10 significant digits and checked against identities with loose tolerances; special
 * values are compared bit-exactly. */
#include "x87_common.h"
#include <math.h>

typedef long double ld;

static ld LD(const f80 *p) { ld r; memcpy(&r, p, 10); return r; }
static f80 F80(ld v) { f80 r; memcpy(&r, &v, 10); return r; }

/* All helpers start from a clean FPU and return through memory so the compiler never has live x87
 * stack entries across the fninit (QEMU also does not clear C2 on every path). */
#define CLOB "st", "st(1)", "st(2)", "st(3)", "st(4)", "st(5)", "st(6)", "st(7)"
static ld do_sin(ld x, uint16_t *sw) { ld r; asm volatile("fninit\n fldt %2\n fsin\n fnstsw %1\n fstpt %0" : "=m"(r), "=m"(*sw) : "m"(x) : CLOB); return r; }
static ld do_cos(ld x, uint16_t *sw) { ld r; asm volatile("fninit\n fldt %2\n fcos\n fnstsw %1\n fstpt %0" : "=m"(r), "=m"(*sw) : "m"(x) : CLOB); return r; }
static ld do_tan(ld x, uint16_t *sw) {
    ld r; uint16_t s;
    asm volatile("fninit\n fldt %2\n fptan\n fnstsw %%ax\n testb $4, %%ah\n jnz 1f\n fstp %%st(0)\n1: fstpt %0" : "=m"(r), "=a"(s) : "m"(x) : "cc", CLOB);
    *sw = s;
    return r;
}
static void do_sincos(ld x, ld *s, ld *c) { asm volatile("fninit\n fldt %2\n fsincos\n fstpt %0\n fstpt %1" : "=m"(*c), "=m"(*s) : "m"(x) : CLOB); }
static ld do_atan2(ld y, ld x) { ld r; asm volatile("fninit\n fldt %1\n fldt %2\n fpatan\n fstpt %0" : "=m"(r) : "m"(y), "m"(x) : CLOB); return r; }
static ld do_f2xm1(ld x) { ld r; asm volatile("fninit\n fldt %1\n f2xm1\n fstpt %0" : "=m"(r) : "m"(x) : CLOB); return r; }
static ld do_yl2x(ld y, ld x) { ld r; asm volatile("fninit\n fldt %1\n fldt %2\n fyl2x\n fstpt %0" : "=m"(r) : "m"(y), "m"(x) : CLOB); return r; }
static ld do_yl2xp1(ld y, ld x) { ld r; asm volatile("fninit\n fldt %1\n fldt %2\n fyl2xp1\n fstpt %0" : "=m"(r) : "m"(y), "m"(x) : CLOB); return r; }

static ld fabsl_(ld v) { return v < 0 ? -v : v; }
/* |a-b| <= scale * 2^-bits */
static int close_(ld a, ld b, ld scale, int bits) { return fabsl_(a - b) <= scale * ldexpl(1.0L, -bits); }

int main(void) {
    static const double base[] = {0.0, 0.1, 0.5, 0.7853981633974483, 1.0, 1.5707963267948966, 2.0, 3.0, 3.141592653589793, 4.0,
        6.283185307179586, 10.0, 100.0, 1000.0, 12345.6789, 1e6, 1e10, 1e15, 1e18, 4e18, 9.0e18, 1e-5, 1e-10, 1e-20, 1e-100,
        0.3, 2.5, 7.0, 20.0, 1e-300, 5e-324};
    for (unsigned i = 0; i < sizeof base / sizeof base[0]; i++)
        for (int sg = 0; sg < 2; sg++) {
            volatile ld x = (ld)(sg ? -base[i] : base[i]);
            ld s, c; uint16_t sw1, sw2, sw3;
            ld a = do_sin(x, &sw1), b = do_cos(x, &sw2), t = do_tan(x, &sw3);
            do_sincos(x, &s, &c);
            int ok = close_(a, s, 1, 52) && close_(b, c, 1, 52) && close_(a * a + b * b, 1, 1, 50) && close_(t * b, a, fabsl_(a) + 1e-30L, 48);
            printf("trig x=%.10Le sin=%.10Le cos=%.10Le tan=%.10Le ok=%d c2=%d\n", x, a, b, t, ok, !!((sw1 | sw2 | sw3) & 0x400));
        }
    /* QEMU's range reduction is not exact beyond a few multiples of pi/2 (neither is real hardware's), */
    /* so only compare the first ones; the emulator's reduction is exact for every |x| < 2^63. */
    for (int k = 1; k <= 10; k++) {
        volatile ld x = (ld)(double)(k * 1.5707963267948966);
        uint16_t sw; ld a = do_sin(x, &sw), b = do_cos(x, &sw);
        printf("kpi2 %d sin=%.6Le cos=%.6Le\n", k, a, b);
    }
    /* out of range (|x| >= 2^63): operand unchanged, C2 set. In range but huge: values only. */
    {
        static const double big[] = {1e19, -1e19, 1e300, 9.2e18, -9.2e18, 4.611686018427388e18};
        for (unsigned i = 0; i < sizeof big / sizeof big[0]; i++) {
            volatile ld x = (ld)big[i];
            uint16_t sw1, sw2; ld a = do_sin(x, &sw1), b = do_tan(x, &sw2);
            f80 ra = F80(a), rb = F80(b);
            if (sw1 & 0x400) printf("big %u sin %04x:%016llx c2=1\n", i, ra.se, (unsigned long long)ra.m);
            else printf("big %u sin %.8Le c2=0\n", i, a);
            if (sw2 & 0x400) printf("big %u tan %04x:%016llx c2=1\n", i, rb.se, (unsigned long long)rb.m);
            else printf("big %u tan %.8Le c2=0\n", i, b);
        }
    }
    /* atan2 grid */
    {
        static const ld v[] = {0.0L, 1.0L, 2.0L, 0.5L, 1e-10L, 1e10L, 3.0L, 1e300L, 1e-300L};
        for (unsigned i = 0; i < sizeof v / sizeof v[0]; i++)
            for (unsigned j = 0; j < sizeof v / sizeof v[0]; j++)
                for (int si = 0; si < 2; si++)
                    for (int sj = 0; sj < 2; sj++) {
                        volatile ld y = si ? -v[i] : v[i], x = sj ? -v[j] : v[j];
                        ld r = do_atan2(y, x);
                        f80 rr = F80(r);
                        printf("atan2 y=%.6Le x=%.6Le r=%.10Le", y, x, r);
                        if (v[i] == 0.0L || v[j] == 0.0L) printf(" bits=%04x:%016llx", rr.se, (unsigned long long)rr.m);
                        printf("\n");
                    }
        static const f80 sp[] = {F(0x7fff, 0x8000000000000000ULL), F(0xffff, 0x8000000000000000ULL), F(0x0000, 0), F(0x8000, 0), F(0x3fff, 0x8000000000000000ULL), F(0xbfff, 0x8000000000000000ULL)};
        for (unsigned i = 0; i < 6; i++)
            for (unsigned j = 0; j < 6; j++) {
                ld r = do_atan2(LD(&sp[i]), LD(&sp[j])); f80 rr = F80(r);
                printf("atan2sp %u %u %04x:%016llx\n", i, j, rr.se, (unsigned long long)rr.m);
            }
    }
    /* 2^x - 1 and logs */
    {
        static const ld xs[] = {0.0L, 1.0L, -1.0L, 0.5L, -0.5L, 0.25L, 0.1L, -0.1L, 1e-10L, -1e-10L, 1e-300L, 0.999999L, -0.999999L, 0.3333333333333333L};
        for (unsigned i = 0; i < sizeof xs / sizeof xs[0]; i++) {
            volatile ld x = xs[i]; ld r = do_f2xm1(x);
            f80 rr = F80(r);
            printf("f2xm1 x=%.10Le r=%.10Le", x, r);
            if (xs[i] == 0.0L || xs[i] == 1.0L || xs[i] == -1.0L || xs[i] == 0.5L || xs[i] == -0.5L) printf(" bits=%04x:%016llx", rr.se, (unsigned long long)rr.m);
            printf("\n");
        }
        static const ld ls[] = {1.0L, 2.0L, 4.0L, 8.0L, 0.5L, 0.125L, 3.0L, 10.0L, 0.1L, 1e10L, 1e-10L, 1e300L, 1e-300L, 1.0000001L, 0.9999999L, 1.5L, 1e4000L, 1e-4000L, 0.0L};
        static const ld ys[] = {1.0L, -1.0L, 2.0L, 0.5L, 0.0L, 3.5L};
        for (unsigned i = 0; i < sizeof ls / sizeof ls[0]; i++)
            for (unsigned j = 0; j < sizeof ys / sizeof ys[0]; j++) {
                volatile ld x = ls[i], y = ys[j];
                ld r = do_yl2x(y, x); f80 rr = F80(r);
                printf("yl2x y=%.3Le x=%.10Le r=%.10Le", y, x, r);
                if (ls[i] == 1.0L || ls[i] == 2.0L || ls[i] == 4.0L || ls[i] == 8.0L || ls[i] == 0.5L || ls[i] == 0.125L || ls[i] == 0.0L) printf(" bits=%04x:%016llx", rr.se, (unsigned long long)rr.m);
                printf("\n");
            }
        static const ld ps[] = {0.0L, 0.1L, -0.1L, 0.2L, -0.2L, 0.25L, 1e-10L, -1e-10L, 1e-300L, 0.29L, 0.4L, 0.414L, -0.29L, 1e-4000L};
        for (unsigned i = 0; i < sizeof ps / sizeof ps[0]; i++)
            for (unsigned j = 0; j < sizeof ys / sizeof ys[0]; j++) {
                volatile ld x = ps[i], y = ys[j];
                ld r = do_yl2xp1(y, x);
                printf("yl2xp1 y=%.3Le x=%.10Le r=%.10Le\n", y, x, r);
            }
        static const f80 sx[] = {F(0xbfff, 0x8000000000000000ULL), F(0x7fff, 0x8000000000000000ULL), F(0xffff, 0x8000000000000000ULL), F(0x7fff, 0xc000000000000000ULL), F(0x8000, 0)};
        for (unsigned i = 0; i < 5; i++)
            for (unsigned j = 0; j < sizeof ys / sizeof ys[0]; j++) {
                ld r = do_yl2x(ys[j], LD(&sx[i])); f80 rr = F80(r);
                printf("yl2xsp %u %u %04x:%016llx\n", i, j, rr.se, (unsigned long long)rr.m);
            }
    }
    /* identity sweeps with random arguments */
    {
        int bad = 0;
        for (int n = 0; n < 4000; n++) {
            uint64_t r = lcg();
            volatile ld x = (ld)((int64_t)(lcg() >> 11) - (1LL << 52)) / (ld)(1LL << (30 + r % 20));
            x = (ld)(double)x;
            ld s, c; do_sincos(x, &s, &c);
            uint16_t sw; ld t = do_tan(x, &sw);
            if (!(close_(s * s + c * c, 1, 1, 50) && close_(t * c, s, fabsl_(s) + 1e-30L, 45))) bad++;
            volatile ld u = (ld)(lcg() >> 20) / (ld)(1ULL << 44) + 0.001L;
            ld e = do_f2xm1(u) + 1.0L;
            ld l = do_yl2x(1.0L, e);
            if (!close_(l, u, 1, 52)) bad++;
            volatile ld y = ((ld)(lcg() >> 20) - 0x7ffffffffULL) / 0x3ffffffffULL, xx = ((ld)(lcg() >> 20) - 0x7ffffffffULL) / 0x3ffffffffULL + 0.000123L;
            ld at = do_atan2(y, xx);
            ld ss, cc; do_sincos(at, &ss, &cc);
            ld rr = sqrtl(y * y + xx * xx);
            if (!(close_(rr * cc, xx, rr, 45) && close_(rr * ss, y, rr, 45))) bad++;
        }
        printf("identity sweep ok=%d\n", bad <= 10);
    }
    return 0;
}
