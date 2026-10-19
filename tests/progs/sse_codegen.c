// flags: -O2 -msse2 -mfpmath=sse -fno-fast-math -ffp-contract=off
// Compiler-generated scalar/packed SSE2 code (what NanoJIT-style code looks like): numerical kernels
// whose result bits must match exactly.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
#include <emmintrin.h>

static uint64_t H = 1469598103934665603ULL;
static void mixd(double d) { uint64_t u; memcpy(&u, &d, 8); H = (H ^ u) * 1099511628211ULL; H ^= H >> 29; }
static void mixf(float f) { uint32_t u; memcpy(&u, &f, 4); H = (H ^ u) * 1099511628211ULL; H ^= H >> 29; }
static void mixi(uint64_t u) { H = (H ^ u) * 1099511628211ULL; H ^= H >> 29; }
static void report(const char *n) { printf("%-16s %016llx\n", n, (unsigned long long)H); H = 1469598103934665603ULL; }

static uint64_t rs = 0x9E3779B97F4A7C15ULL;
static uint64_t rnd(void) { rs ^= rs << 13; rs ^= rs >> 7; rs ^= rs << 17; return rs; }
static double rdbl(void) { return (double)(int64_t)(rnd() >> 11) / 1048576.0 - 4194304.0; }

__attribute__((noinline)) static int mandel(double cr, double ci) {
    double zr = 0, zi = 0;
    int n = 0;
    while (n < 200 && zr * zr + zi * zi <= 4.0) { double t = zr * zr - zi * zi + cr; zi = 2 * zr * zi + ci; zr = t; n++; }
    return n;
}
__attribute__((noinline)) static void nbody(double *x, double *v, int n, int steps) {
    for (int s = 0; s < steps; s++) {
        for (int i = 0; i < n; i++) {
            double ax = 0;
            for (int j = 0; j < n; j++) if (i != j) { double d = x[j] - x[i]; double r2 = d * d + 0.01; ax += d / (r2 * sqrt(r2)); }
            v[i] += ax * 0.001;
        }
        for (int i = 0; i < n; i++) x[i] += v[i] * 0.001;
    }
}
__attribute__((noinline)) static void fft(double *re, double *im, int n) {
    for (int i = 1, j = 0; i < n; i++) { int bit = n >> 1; for (; j & bit; bit >>= 1) j ^= bit; j ^= bit; if (i < j) { double t = re[i]; re[i] = re[j]; re[j] = t; t = im[i]; im[i] = im[j]; im[j] = t; } }
    for (int len = 2; len <= n; len <<= 1) {
        double ang = -2 * 3.14159265358979323846 / len, wr = 1, wi = 0, cr = 1 - 2 * (ang * ang / 8), ci = ang;  // crude twiddle recurrence (bit-exact, no libm)
        for (int i = 0; i < n; i += len) {
            double wr2 = 1, wi2 = 0;
            for (int k = 0; k < len / 2; k++) {
                double ur = re[i + k], ui = im[i + k];
                double vr = re[i + k + len / 2] * wr2 - im[i + k + len / 2] * wi2, vi = re[i + k + len / 2] * wi2 + im[i + k + len / 2] * wr2;
                re[i + k] = ur + vr; im[i + k] = ui + vi; re[i + k + len / 2] = ur - vr; im[i + k + len / 2] = ui - vi;
                double nr = wr2 * cr - wi2 * ci; wi2 = wr2 * ci + wi2 * cr; wr2 = nr;
            }
        }
        (void)wr; (void)wi;
    }
}
__attribute__((noinline)) static void matmul(const float *a, const float *b, float *c, int n) {
    for (int i = 0; i < n; i++) for (int j = 0; j < n; j++) { float s = 0; for (int k = 0; k < n; k++) s += a[i * n + k] * b[k * n + j]; c[i * n + j] = s; }
}
__attribute__((noinline)) static void saxpy_sse(float *y, const float *x, float a, int n) {
    __m128 va = _mm_set1_ps(a);
    for (int i = 0; i + 4 <= n; i += 4) _mm_storeu_ps(y + i, _mm_add_ps(_mm_loadu_ps(y + i), _mm_mul_ps(va, _mm_loadu_ps(x + i))));
}
__attribute__((noinline)) static int conv_i(double d) { return (int)d; }
__attribute__((noinline)) static float conv_f(double d) { return (float)d; }
__attribute__((noinline)) static double conv_d(int i) { return (double)i; }
__attribute__((noinline)) static double fabs_(double d) { return fabs(d); }
__attribute__((noinline)) static double neg_(double d) { return -d; }
__attribute__((noinline)) static double copysign_(double a, double b) { return copysign(a, b); }
__attribute__((noinline)) static double min_(double a, double b) { return a < b ? a : b; }
__attribute__((noinline)) static double max_(double a, double b) { return a > b ? a : b; }
__attribute__((noinline)) static int lt_(double a, double b) { return a < b; }
__attribute__((noinline)) static int eq_(double a, double b) { return a == b; }
__attribute__((noinline)) static int ne_(double a, double b) { return a != b; }
__attribute__((noinline)) static int ge_(double a, double b) { return a >= b; }
__attribute__((noinline)) static int unord_(double a, double b) { return __builtin_isunordered(a, b); }
__attribute__((noinline)) static double sq(double a) { return sqrt(a); }
__attribute__((noinline)) static float sqf(float a) { return sqrtf(a); }
__attribute__((noinline)) static long lr(double d) { return lrint(d); }
__attribute__((noinline)) static double fl(double d) { return floor(d); }
__attribute__((noinline)) static double ce(double d) { return ceil(d); }
__attribute__((noinline)) static double tr(double d) { return trunc(d); }
__attribute__((noinline)) static double rn(double d) { return rint(d); }
__attribute__((noinline)) static double fm(double a, double b) { return fmod(a, b); }

int main(void) {
    for (int py = -20; py <= 20; py++) for (int px = -40; px <= 20; px++) mixi(mandel(px / 20.0, py / 20.0));
    report("mandelbrot");
    { double x[24], v[24]; for (int i = 0; i < 24; i++) { x[i] = i * 1.37 - 9; v[i] = 0; } nbody(x, v, 24, 40); for (int i = 0; i < 24; i++) { mixd(x[i]); mixd(v[i]); } report("nbody"); }
    { double re[256], im[256]; for (int i = 0; i < 256; i++) { re[i] = (i * 37 % 101) / 7.0; im[i] = 0; } fft(re, im, 256); for (int i = 0; i < 256; i++) { mixd(re[i]); mixd(im[i]); } report("fft"); }
    { float a[256], b[256], c[256]; for (int i = 0; i < 256; i++) { a[i] = (float)((i * 13) % 17) / 3.0f; b[i] = (float)((i * 7) % 19) / 5.0f; } matmul(a, b, c, 16); for (int i = 0; i < 256; i++) mixf(c[i]); report("matmul f32"); }
    { float y[103], x[103]; for (int i = 0; i < 103; i++) { y[i] = i * 0.5f; x[i] = 1.0f / (i + 1); } for (int r = 0; r < 20; r++) saxpy_sse(y, x, 0.37f, 103); for (int i = 0; i < 103; i++) mixf(y[i]); report("saxpy sse"); }
    for (int i = 0; i < 3000; i++) {
        double d = rdbl() / (1 + (rnd() & 255));
        if (i % 7 == 0) d = (double)(int32_t)rnd() * 0.5;
        if (i % 11 == 0) d = nearbyint(d) + 0.5;
        mixi((uint32_t)conv_i(d)); mixf(conv_f(d)); mixd(conv_d((int32_t)rnd())); mixd(fabs_(d)); mixd(neg_(d)); mixd(copysign_(d, -1.0));
        double e = i % 5 == 0 ? d : rdbl();
        mixd(min_(d, e)); mixd(max_(d, e)); mixi(lt_(d, e) | eq_(d, e) << 1 | ne_(d, e) << 2 | ge_(d, e) << 3 | unord_(d, e) << 4);
        mixd(sq(fabs(d))); mixf(sqf((float)fabs(d))); mixi((uint64_t)lr(d)); mixd(fl(d)); mixd(ce(d)); mixd(tr(d)); mixd(rn(d)); mixd(fm(d, e + 0.001));
    }
    report("scalar conv/cmp");
    {   // NaN / inf / -0 handling through compiler-generated compare sequences (ucomisd + jp)
        double nan = __builtin_nan(""), inf = __builtin_inf();
        volatile double vals[] = {0.0, -0.0, 1.0, -1.0, inf, -inf, nan, 1e308, 5e-324, -5e-324};
        for (int i = 0; i < 10; i++) for (int j = 0; j < 10; j++) {
            double a = vals[i], b = vals[j];
            mixi(lt_(a, b) | eq_(a, b) << 1 | ne_(a, b) << 2 | ge_(a, b) << 3 | unord_(a, b) << 4);
            mixd(min_(a, b)); mixd(max_(a, b));
        }
        report("nan compares");
    }
    return 0;
}
