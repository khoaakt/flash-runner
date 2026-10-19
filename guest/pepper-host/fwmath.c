/* floor / ceil / trunc (double and float) in plain integer arithmetic.
 *
 * glibc's i386 versions save and restore the x87 control word (fnstenv/fldenv): four interpreted x87 instructions per call, and Flash calls
 * these millions of times. Linked into pepper-host with -rdynamic so the dynamic linker binds the plugin's calls here instead of libm.
 * Bit-exact with glibc (signed zeros, infinities, NaN payloads pass through): tools/test-fwmath.sh compares them on edge cases and 3 million
 * random values. Compile with -fno-builtin so the compiler does not turn these back into calls to the functions it is defining.
 */
#include <stdint.h>

enum { FLOOR = 0, CEIL = 1, TRUNC = 2 };

static double round_d(double x, int mode) {
  uint64_t u; __builtin_memcpy(&u, &x, 8);
  int e = (int)(u >> 52 & 0x7ff) - 0x3ff;               /* unbiased exponent */
  if (e >= 52) return x;                                 /* already integral, infinity or NaN */
  int neg = (int)(u >> 63);
  if (e < 0) {                                           /* |x| < 1 */
    if ((u << 1) == 0) return x;                         /* +-0 */
    if (mode == TRUNC) return neg ? -0.0 : 0.0;
    if (mode == FLOOR) return neg ? -1.0 : 0.0;
    return neg ? -0.0 : 1.0;
  }
  uint64_t frac = (1ULL << (52 - e)) - 1;                /* mantissa bits below the binary point */
  if (!(u & frac)) return x;
  if (mode == CEIL ? !neg : (mode == FLOOR && neg)) u += frac;   /* round away from zero: the carry ripples into the exponent when needed */
  u &= ~frac;
  __builtin_memcpy(&x, &u, 8);
  return x;
}

static float round_f(float x, int mode) {
  uint32_t u; __builtin_memcpy(&u, &x, 4);
  int e = (int)(u >> 23 & 0xff) - 0x7f;
  if (e >= 23) return x;
  int neg = (int)(u >> 31);
  if (e < 0) {
    if ((u << 1) == 0) return x;
    if (mode == TRUNC) return neg ? -0.0f : 0.0f;
    if (mode == FLOOR) return neg ? -1.0f : 0.0f;
    return neg ? -0.0f : 1.0f;
  }
  uint32_t frac = (1u << (23 - e)) - 1;
  if (!(u & frac)) return x;
  if (mode == CEIL ? !neg : (mode == FLOOR && neg)) u += frac;
  u &= ~frac;
  __builtin_memcpy(&x, &u, 4);
  return x;
}

double floor(double x) { return round_d(x, FLOOR); }
double ceil(double x) { return round_d(x, CEIL); }
double trunc(double x) { return round_d(x, TRUNC); }
float floorf(float x) { return round_f(x, FLOOR); }
float ceilf(float x) { return round_f(x, CEIL); }
float truncf(float x) { return round_f(x, TRUNC); }
