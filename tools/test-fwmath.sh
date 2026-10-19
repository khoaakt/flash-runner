#!/bin/sh
# Compare guest/pepper-host/fwmath.c with glibc's libm on edge cases and a few million random values (runs in the i386 container).
set -eu
cd "$(dirname "$0")/.."
docker run --rm --platform linux/386 -v "$PWD:/proj" flashrunner-i386-dev sh -c '
cd /tmp && cat > t.c <<"C"
#include <stdio.h>
#include <math.h>
#include <string.h>
#include <stdint.h>
#include <stdlib.h>
double fw_floor(double), fw_ceil(double), fw_trunc(double); float fw_floorf(float), fw_ceilf(float), fw_truncf(float);
static int same(double a, double b) { return memcmp(&a, &b, 8) == 0 || (isnan(a) && isnan(b)); }
static int samef(float a, float b) { return memcmp(&a, &b, 4) == 0 || (isnan(a) && isnan(b)); }
int main(void) {
  double e[] = {0.0,-0.0,0.3,-0.3,0.5,-0.5,1.0,-1.0,1.5,-1.5,2.5,-2.5,4503599627370495.5,-4503599627370495.5,4503599627370496.0,1e300,-1e300,INFINITY,-INFINITY,NAN,1e-300,-1e-300,0.9999999999999999,-0.9999999999999999,123456.789,-123456.789};
  long bad = 0;
  for (unsigned i = 0; i < sizeof e / sizeof *e; i++) {
    double x = e[i];
    if (!same(fw_floor(x), floor(x)) || !same(fw_ceil(x), ceil(x)) || !same(fw_trunc(x), trunc(x))) { printf("BAD double %.17g: floor %g/%g ceil %g/%g trunc %g/%g\n", x, fw_floor(x), floor(x), fw_ceil(x), ceil(x), fw_trunc(x), trunc(x)); bad++; }
    float f = (float)x;
    if (!samef(fw_floorf(f), floorf(f)) || !samef(fw_ceilf(f), ceilf(f)) || !samef(fw_truncf(f), truncf(f))) { printf("BAD float %.9g\n", f); bad++; }
  }
  srand(1);
  for (long i = 0; i < 3000000; i++) {
    uint64_t u = ((uint64_t)rand() << 42) ^ ((uint64_t)rand() << 21) ^ rand(); double x; memcpy(&x, &u, 8);
    if (i & 1) x = (double)(rand() - RAND_MAX / 2) / (1 + (rand() & 1023));
    if (!same(fw_floor(x), floor(x)) || !same(fw_ceil(x), ceil(x)) || !same(fw_trunc(x), trunc(x))) { if (bad++ < 10) printf("BAD double %.17g\n", x); }
    uint32_t w = (uint32_t)rand() ^ ((uint32_t)rand() << 16); float f; memcpy(&f, &w, 4); if (i & 1) f = (float)x;
    if (!samef(fw_floorf(f), floorf(f)) || !samef(fw_ceilf(f), ceilf(f)) || !samef(fw_truncf(f), truncf(f))) { if (bad++ < 10) printf("BAD float %.9g\n", f); }
  }
  printf("%s (%ld mismatches)\n", bad ? "FAIL" : "PASS fwmath matches libm", bad);
  return bad != 0;
}
C
sed "s/^double floor/double fw_floor/; s/^double ceil/double fw_ceil/; s/^double trunc/double fw_trunc/; s/^float floorf/float fw_floorf/; s/^float ceilf/float fw_ceilf/; s/^float truncf/float fw_truncf/" /proj/guest/pepper-host/fwmath.c > fw.c
gcc -O2 -fno-builtin -c fw.c -o fw.o && gcc -O1 t.c fw.o -o t -lm && ./t
'
