#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <unistd.h>
#include <time.h>
static volatile int stop;
static void *spin(void *p) { volatile unsigned x = 1; while (!stop) x = x * 1664525u + 1013904223u; return 0; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
int main(int argc, char **argv) {
  int n = argc > 1 ? atoi(argv[1]) : 0; pthread_t th[8];
  for (int i = 0; i < n; i++) pthread_create(&th[i], 0, spin, 0);
  double max = 0, sum = 0; int it = 20;
  for (int i = 0; i < it; i++) { double t0 = now(); usleep(10000); double d = now() - t0; sum += d; if (d > max) max = d; }
  printf("spinners=%d usleep(10ms): avg %.1f ms max %.1f ms\n", n, sum / it, max);
  stop = 1; for (int i = 0; i < n; i++) pthread_join(th[i], 0);
  printf("joined\n");
  return 0;
}
