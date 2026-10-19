#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
static uint32_t rotl(uint32_t x, int n) { return (x << n) | (x >> (32 - n)); }
static int cmp(const void *a, const void *b) { return *(int *)a - *(int *)b; }
int main(void) {
    uint32_t h = 2166136261u; uint64_t acc = 0x123456789abcdefULL; int64_t sacc = -7;
    for (int i = 1; i < 5000; i++) {
        h = (h ^ i) * 16777619u; h = rotl(h, i & 31);
        acc = acc * 6364136223846793005ULL + 1442695040888963407ULL;
        sacc = sacc / (i % 13 + 1) - (int64_t)(acc >> 40) % 1000003;
        acc ^= acc >> 29; if ((i & 63) == 0) acc = (acc << 7) | (acc >> 57);
    }
    printf("h=%u acc=%llu sacc=%lld\n", h, (unsigned long long)acc, (long long)sacc);
    int a[1000]; uint32_t s = 7; for (int i = 0; i < 1000; i++) { s = s * 1664525 + 1013904223; a[i] = (int)(s >> 4) % 100000; }
    qsort(a, 1000, sizeof(int), cmp); long chk = 0; for (int i = 0; i < 1000; i++) chk += (long)a[i] * (i + 1);
    printf("sorted chk=%ld min=%d max=%d\n", chk, a[0], a[999]);
    char buf[256]; snprintf(buf, sizeof buf, "%d|%5d|%-5d|%05d|%x|%X|%o|%lld|%llu|%c|%s|%10.3s|", -42, 42, 42, 42, 255, 255, 8, -1234567890123LL, 18446744073709551615ULL, 'z', "str", "truncate");
    puts(buf);
    printf("strtol=%ld strtoul=%lu atoi=%d\n", strtol("-0x1f", 0, 0), strtoul("4294967295", 0, 10), atoi("  123abc"));
    uint8_t m[100]; for (int i = 0; i < 100; i++) m[i] = i * 7; memmove(m + 3, m, 90); long ms = 0; for (int i = 0; i < 100; i++) ms += m[i] * (i + 1);
    printf("memmove=%ld cmp=%d\n", ms, memcmp(m, m + 1, 10) < 0);
    unsigned bits = 0xf0f0f0f0u; printf("popcnt=%d clz=%d ctz=%d bswap=%08x\n", __builtin_popcount(bits), __builtin_clz(bits), __builtin_ctz(bits), __builtin_bswap32(0x11223344));
    return (int)(h & 0x7f);
}
