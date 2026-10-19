#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <math.h>
static double vals[] = {0.0,-0.0,0.7,-0.7,1.0,-1.0,1.5,-1.5,2.5,0.3333,-0.3333,3.14159265358979,-3.14159265358979,6.28318530717959,100.0,-100.0,1e5,-1e5,1e10,-1e10,1e15,1e16,-1e16,1e20,1e300,-1e300,1e-5,-1e-5,1e-300,4.9e-324,NAN,INFINITY,-INFINITY,0.5,-0.5,123456.789,-98765.4321,0.9999999,1.0000001,2147483647.0,-2147483648.0,4294967296.0,9007199254740993.0};
#define N (sizeof vals / sizeof *vals)
static uint64_t h(double d) { uint64_t u; memcpy(&u, &d, 8); if (d != d) u = 0x7ff8000000000000ull; return u; }
int main(int argc, char **argv) {
  const char *f = argv[1]; uint64_t acc = 1469598103934665603ull;
#define MIX(v) acc = (acc ^ h(v)) * 1099511628211ull
#define D1(n) if (!strcmp(f, #n)) { for (unsigned i = 0; i < N; i++) { volatile double x = vals[i]; MIX(n(x)); } goto out; }
#define D2(n) if (!strcmp(f, #n)) { for (unsigned i = 0; i < N; i++) for (unsigned j = 0; j < N; j += 3) { volatile double x = vals[i], y = vals[j]; MIX(n(x, y)); } goto out; }
#define F1(n) if (!strcmp(f, #n)) { for (unsigned i = 0; i < N; i++) { volatile float x = (float)vals[i]; MIX((double)n(x)); } goto out; }
#define F2(n) if (!strcmp(f, #n)) { for (unsigned i = 0; i < N; i++) for (unsigned j = 0; j < N; j += 3) { volatile float x = (float)vals[i], y = (float)vals[j]; MIX((double)n(x, y)); } goto out; }
  D1(acos) D1(asin) D1(atan) D2(atan2) D1(cos) D1(sin) D1(tan) D1(exp) D1(log) D1(log10) D2(pow) D2(fmod) D1(sqrt) D1(ceil) D1(floor) D1(trunc) D1(nearbyint) D1(rint)
  F1(acosf) F1(expf) F1(logf) F2(powf) F1(sqrtf) F1(sinf) F1(cosf) F1(floorf) F1(ceilf) F1(truncf) F1(nearbyintf) F1(rintf)
  if (!strcmp(f, "sincosf")) { for (unsigned i = 0; i < N; i++) { volatile float x = (float)vals[i]; float s, c; sincosf(x, &s, &c); MIX(s); MIX(c); } goto out; }
  if (!strcmp(f, "sincos")) { for (unsigned i = 0; i < N; i++) { volatile double x = vals[i]; double s, c; sincos(x, &s, &c); MIX(s); MIX(c); } goto out; }
  if (!strcmp(f, "lrint")) { for (unsigned i = 0; i < N; i++) { volatile double x = vals[i]; if (fabs(x) < 2e9) MIX((double)lrint(x)); } goto out; }
  if (!strcmp(f, "lrintf")) { for (unsigned i = 0; i < N; i++) { volatile float x = (float)vals[i]; if (fabsf(x) < 2e9f) MIX((double)lrintf(x)); } goto out; }
  if (!strcmp(f, "frexp")) { for (unsigned i = 0; i < N; i++) { volatile double x = vals[i]; int e; MIX(frexp(x, &e)); MIX(e); } goto out; }
  if (!strcmp(f, "modf")) { for (unsigned i = 0; i < N; i++) { volatile double x = vals[i]; double ip; MIX(modf(x, &ip)); MIX(ip); } goto out; }
  printf("unknown\n"); return 2;
out: printf("%s %016llx\n", f, (unsigned long long)acc); return 0;
}
