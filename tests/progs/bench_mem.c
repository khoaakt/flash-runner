// memcpy-ish word loop, byte-wise checksum, and a memset-ish loop over a working set.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#define N 65536
static uint8_t a[N], b[N];
static uint32_t wa[N / 4], wb[N / 4];
__attribute__((noinline)) static void copy_words(uint32_t *d, const uint32_t *s, int n) { for (int i = 0; i < n; i++) d[i] = s[i]; }
__attribute__((noinline)) static uint32_t checksum(const uint8_t *p, int n) {
    uint32_t h = 2166136261u;
    for (int i = 0; i < n; i++) { h ^= p[i]; h *= 16777619u; }
    return h;
}
__attribute__((noinline)) static void fill(uint8_t *p, int n, uint8_t v) { for (int i = 0; i < n; i++) p[i] = (uint8_t)(v + i); }
int main(void) {
    uint32_t acc = 0;
    for (int i = 0; i < N / 4; i++) wa[i] = i * 2654435761u;
    for (int r = 0; r < 200; r++) {
        copy_words(wb, wa, N / 4);
        fill(a, N, (uint8_t)r);
        memcpy(b, a, N);
        acc += checksum(b, N) + wb[r & 1023];
    }
    printf("acc=%u\n", acc);
    return 0;
}
