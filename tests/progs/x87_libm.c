// flags: -mfpmath=387 -mno-sse -O1
/* glibc's own x87 code paths: libm (double and long double), printf/strtod/strtold with long double. */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fenv.h>
#include <stdint.h>

static volatile double dv[64];
static volatile long double lv[64];

static uint64_t st = 0x9e3779b97f4a7c15ULL;
static uint64_t rnd(void) { st ^= st << 13; st ^= st >> 7; st ^= st << 17; return st; }
static double rd(double lo, double hi) { return lo + (hi - lo) * (double)(rnd() >> 11) / 9007199254740992.0; }

#define P1(f, x) printf(#f "(%.17g) = %.15g\n", (double)(x), f(x))
#define PL1(f, x) printf(#f "(%.17Lg) = %.18Lg\n", (long double)(x), f(x))

int main(void) {
    static const double args[] = {0.0, 1.0, -1.0, 0.5, -0.5, 2.0, 3.0, 10.0, 0.1, 0.7, 1.5, 100.0, 1e-5, 12345.678, -7.25, 1e10, 3.14159265358979, 1.0e-300, 5e-324, 700.0, -700.0};
    for (unsigned i = 0; i < sizeof args / sizeof args[0]; i++) {
        volatile double x = args[i];
        P1(sin, x); P1(cos, x); P1(tan, x); P1(atan, x); P1(exp, x); P1(expm1, x); P1(sinh, x); P1(cosh, x); P1(tanh, x);
        P1(fabs, x); P1(floor, x); P1(ceil, x); P1(trunc, x); P1(round, x); P1(rint, x); P1(cbrt, x);
        if (x > 0) { P1(log, x); P1(log10, x); P1(log2, x); P1(log1p, x); P1(sqrt, x); P1(exp2, x); }
        if (x >= -1 && x <= 1) { P1(asin, x); P1(acos, x); }
        printf("lrint(%.17g)=%ld llrint=%lld\n", x, lrint(x), llrint(x));
    }
    for (int i = 0; i < 400; i++) {
        double x = rd(-50, 50), y = rd(0.01, 40);
        printf("r %d sin=%.14g cos=%.14g tan=%.14g atan2=%.14g exp=%.14g log=%.14g pow=%.14g hypot=%.14g fmod=%.17g rem=%.17g\n", i,
               sin(x), cos(x), tan(x), atan2(x, y), exp(x / 3), log(y), pow(y, x / 10), hypot(x, y), fmod(x, y), remainder(x, y));
    }
    for (int i = 0; i < 200; i++) {
        long double x = (long double)rd(-30, 30) + (long double)rd(0, 1) * 1e-10L, y = (long double)rd(0.5, 20);
        printf("l %d sinl=%.12Lg cosl=%.12Lg expl=%.18Lg logl=%.18Lg powl=%.18Lg atan2l=%.18Lg sqrtl=%.18Lg fmodl=%.18Lg\n", i, sinl(x), cosl(x), expl(x / 4), logl(y),
               powl(y, x / 8), atan2l(x, y), sqrtl(y), fmodl(x, y));
    }
    /* printf/strtod round trips */
    static const char *strs[] = {"0", "1", "-1", "0.1", "3.14159265358979323846", "1e308", "1e-308", "4.9e-324", "123456789.123456789", "1e400", "-1e-400",
                                 "0x1.8p+3", "inf", "nan", "1.7976931348623157e308", "2.2250738585072014e-308", "9007199254740993", "0.30000000000000004"};
    for (unsigned i = 0; i < sizeof strs / sizeof strs[0]; i++) {
        double d = strtod(strs[i], NULL);
        long double l = strtold(strs[i], NULL);
        printf("strtod(%s) = %.17g %a ; strtold = %.21Lg %La\n", strs[i], d, d, l, l);
    }
    for (int i = 0; i < 300; i++) {
        double d = rd(-1e6, 1e6) * pow(10, (int)(rnd() % 40) - 20);
        long double l = (long double)d * (1.0L + (long double)(rnd() % 1000) * 1e-20L);
        char buf[128];
        snprintf(buf, sizeof buf, "%.20g", d);
        double back = strtod(buf, NULL);
        printf("p %d %.10f %e %g %.17g %d | %Lf %Le %.19Lg\n", i, d, d, d, d, back == d, l, l, l);
    }
    /* special values and rounding modes through fesetround + rint/lrint */
    static const int modes[] = {FE_TONEAREST, FE_DOWNWARD, FE_UPWARD, FE_TOWARDZERO};
    for (int m = 0; m < 4; m++) {
        fesetround(modes[m]);
        for (double x = -3.5; x <= 3.5; x += 0.5) printf("rm%d %g rint=%g lrint=%ld nearbyint=%g\n", m, x, rint(x), lrint(x), nearbyint(x));
        volatile double a = 1.0, b = 3.0;
        printf("rm%d 1/3=%.20g sum=%.20g sqrt2=%.20g\n", m, a / b, a / b + 1e-17, sqrt(2.0 + (a - 1.0)));
    }
    fesetround(FE_TONEAREST);
    printf("nan: %g %g %g %g\n", nan(""), -nan(""), INFINITY - INFINITY, 0.0 * INFINITY);
    printf("isnan %d isinf %d signbit %d fpclassify %d %d %d\n", isnan(nan("")), isinf(-INFINITY), signbit(-0.0), fpclassify(1e-310), fpclassify(0.0), fpclassify(NAN));
    int e; double fr = frexp(1234.5678, &e); double ip; double fp = modf(-12.75, &ip);
    printf("frexp %g %d modf %g %g ldexp %g scalbn %g\n", fr, e, fp, ip, ldexp(0.75, 10), scalbn(1.5, -1070));
    return 0;
}
