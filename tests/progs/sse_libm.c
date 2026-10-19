// flags: -O2 -msse2 -mfpmath=sse -fno-builtin-printf
// libc/libm code paths that run SSE2 once CPUID advertises it: printf/strtod float formatting, qsort of
// floats, and libm functions. Exact-bit checks for the correctly rounded ones, 9 digits for transcendentals.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <float.h>
#include <fenv.h>
#include <stdint.h>
#include <xmmintrin.h>

static uint64_t rs = 0xD1B54A32D192ED03ULL;
static uint64_t rnd(void) { rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17; return rs; }

static int cmpd(const void *a, const void *b) { double x = *(const double *)a, y = *(const double *)b; return (x > y) - (x < y); }
static int cmpf(const void *a, const void *b) { float x = *(const float *)a, y = *(const float *)b; return (x > y) - (x < y); }
static int cmpi(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }

int main(void) {
    setvbuf(stdout, 0, _IOLBF, 0);
    static const double vals[] = {0.0, -0.0, 1.0, -1.0, 0.1, 0.5, 1.5, 2.5, 3.14159265358979, 1e10, 1e-10, 123456789.123456789, 1e22, 1e23, 1.7976931348623157e308,
                                  2.2250738585072014e-308, 4.9406564584124654e-324, 1.0 / 3, 2.0 / 3, 100.0, 0.000123456, 65536.0, 4294967296.0, 9007199254740993.0, 0.3, 5e-324};
    for (unsigned i = 0; i < sizeof vals / sizeof vals[0]; i++) {
        printf("%f|%e|%g|%.17g|%a|%10.3f|%-12.2e|%+g\n", vals[i], vals[i], vals[i], vals[i], vals[i], vals[i], vals[i], vals[i]);
    }
    printf("%f %f %f %e\n", HUGE_VAL, -HUGE_VAL, NAN, INFINITY);
    char buf[64];
    for (int i = 0; i < 300; i++) {
        uint64_t b = rnd();
        double d;
        memcpy(&d, &b, 8);
        if (d != d || d - d != 0) continue;
        snprintf(buf, sizeof buf, "%.17g", d);
        double back = strtod(buf, 0);
        printf("%s %s\n", buf, memcmp(&back, &d, 8) ? "ROUNDTRIP-FAIL" : "ok");
        float f = (float)(i * 1.37 - 100);
        snprintf(buf, sizeof buf, "%.9g", f);
        printf("%s %d\n", buf, strtof(buf, 0) == f);
    }
    static const char *strs[] = {"0", "1", "-1.5", "1e308", "1e309", "4.9e-324", "2.2250738585072011e-308", "123.456e-7", ".5", "inf", "nan", "0x1.8p3", "1e-400", "179769313486231570814527423731704356798070e+267"};
    for (unsigned i = 0; i < sizeof strs / sizeof strs[0]; i++) printf("strtod(%s) = %.17g\n", strs[i], strtod(strs[i], 0));

    // qsort
    {
        double a[500]; float b[500]; int c[500];
        for (int i = 0; i < 500; i++) { a[i] = (double)(int64_t)(rnd() >> 20) / 1024.0; b[i] = (float)a[i] * 0.37f; c[i] = (int)(rnd() >> 40); }
        qsort(a, 500, sizeof a[0], cmpd);
        qsort(b, 500, sizeof b[0], cmpf);
        qsort(c, 500, sizeof c[0], cmpi);
        int sorted = 1;
        for (int i = 1; i < 500; i++) sorted &= a[i - 1] <= a[i] && b[i - 1] <= b[i] && c[i - 1] <= c[i];
        printf("qsort sorted=%d first=%.6f last=%.6f %.6f %d\n", sorted, a[0], a[499], b[250], c[100]);
    }

    // libm: correctly rounded or exact operations -> compare bits; transcendental -> 9 digits
    {
        static const double xs[] = {0.0, 0.5, 1.0, 2.0, 2.5, 3.5, -2.5, -0.5, 1e-5, 123.456, 1e15, 4503599627370497.0, 0.1, 10.0, 7.0, 1e300, -3.7};
        for (unsigned i = 0; i < sizeof xs / sizeof xs[0]; i++) {
            double x = xs[i];
            printf("x=%g sqrt=%.17g floor=%g ceil=%g trunc=%g round=%g rint=%g nearbyint=%g lrint=%ld lround=%ld fabs=%g\n", x, sqrt(fabs(x)), floor(x), ceil(x), trunc(x), round(x), rint(x),
                   nearbyint(x), fabs(x) < 2e9 ? lrint(x) : 0L, fabs(x) < 2e9 ? lround(x) : 0L, fabs(x));
            printf("  exp=%.9g log=%.9g sin=%.9g cos=%.9g tan=%.9g atan=%.9g cbrt=%.9g\n", exp(fmod(x, 20)), log(fabs(x) + 1), sin(x), cos(x), tan(x), atan(x), cbrt(x));
            printf("  pow=%.9g fmod=%.9g hypot=%.9g atan2=%.9g sinh=%.9g\n", pow(fabs(x) + 0.1, 1.7), fmod(x, 0.7), hypot(x, 3.0), atan2(x, 2.0), sinh(fmod(x, 10)));
            int e;
            double m = frexp(x, &e);
            printf("  frexp=%g,%d ldexp=%g scalbn=%g copysign=%g\n", m, e, ldexp(m, e + 1), scalbn(x, -3), copysign(x, -1.0));
        }
        // rounding-mode aware conversions through libm
        static const int modes[] = {FE_TONEAREST, FE_DOWNWARD, FE_UPWARD, FE_TOWARDZERO};
        for (int k = 0; k < 4; k++) {
            fesetround(modes[k]);
            // glibc only touches MXCSR when CPUID advertises SSE; set it explicitly so the SSE code in this binary agrees
            _mm_setcsr((_mm_getcsr() & ~0x6000u) | ((unsigned[]){0, 0x2000, 0x4000, 0x6000})[k]);
            volatile double v[] = {2.5, 3.5, -2.5, 2.4999, 2.5001, -0.1, 1e9 + 0.5};
            printf("round mode %d:", k);
            for (int i = 0; i < 7; i++) printf(" %ld %g", lrint(v[i]), rint(v[i]));
            volatile double a = 1.0, b = 3.0;
            double q = a / b;
            printf(" 1/3=%a\n", q);
        }
        fesetround(FE_TONEAREST);
        _mm_setcsr(_mm_getcsr() & ~0x6000u);
        feclearexcept(FE_ALL_EXCEPT);
        volatile double z = 0.0, one = 1.0;
        double r = one / z;
        printf("div by zero: %g flags=%d\n", r, !!fetestexcept(FE_DIVBYZERO));
        feclearexcept(FE_ALL_EXCEPT);
        volatile double tiny = DBL_MIN;
        double u = tiny / 3.0;
        printf("underflow: %g flags=%d,%d\n", u, !!fetestexcept(FE_UNDERFLOW), !!fetestexcept(FE_INEXACT));
    }
    return 0;
}
