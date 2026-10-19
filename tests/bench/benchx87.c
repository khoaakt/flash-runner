#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <math.h>
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
int main(int argc, char **argv) {
  int mode = argc > 1 ? atoi(argv[1]) : 0; volatile double sink = 0; double t0 = now();
  if (mode == 0) { double x = 1.0; for (long i = 0; i < 40000000; i++) x = x * 1.0000001 + 0.5; sink = x; }
  else if (mode == 1) { long a = 0; double x = 0.25; for (long i = 0; i < 6000000; i++) { unsigned short cw, cw2; __asm__ volatile("fnstcw %0" : "=m"(cw)); cw2 = cw | 0xc00; __asm__ volatile("fldcw %0" :: "m"(cw2)); int r; __asm__ volatile("fldl %1; fistpl %0" : "=m"(r) : "m"(x)); __asm__ volatile("fldcw %0" :: "m"(cw)); a += r; x += 1.37; if (x > 1e6) x = 0.25; } sink = (double)a; }
  else if (mode == 2) { double a = 0; for (long i = 0; i < 3000000; i++) a += sin(i * 0.001) * cos(i * 0.002) + sqrt((double)i); sink = a; }
  else if (mode == 3) { float f[256]; for (int i = 0; i < 256; i++) f[i] = i; double s = 0; for (long k = 0; k < 400000; k++) for (int i = 0; i < 256; i++) s += f[i] * 0.5f; sink = s; }
  printf("mode %d: %.0f ms (sink %g)\n", mode, now() - t0, (double)sink);
  return 0;
}
