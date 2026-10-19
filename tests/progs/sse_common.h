// Shared helpers for the sse_*/mmx_* differential tests.
// Results are folded into an order-sensitive hash printed once per operation so that the expected
// output stays small; run the binary with any argument (e.g. `-v`) to dump every individual result
// instead, which makes bisecting a mismatch easy (diff the verbose output of the emulator against
// the real thing).
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <emmintrin.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;

static int verbose;
static u64 H = 0xcbf29ce484222325ULL;
static u64 rng_s = 88172645463325252ULL;

static u64 rnd64(void) {
    rng_s ^= rng_s << 13;
    rng_s ^= rng_s >> 7;
    rng_s ^= rng_s << 17;
    return rng_s;
}
static inline void hmix(u64 v) {
    H = (H ^ v) * 0x100000001b3ULL;
    H ^= H >> 29;
}
static inline void hmix128(const void *p) {
    u64 q[2];
    memcpy(q, p, 16);
    if (verbose) printf("  %016llx%016llx\n", (unsigned long long)q[1], (unsigned long long)q[0]);
    hmix(q[0]);
    hmix(q[1]);
}
static inline void hmix64(const void *p) {
    u64 q;
    memcpy(&q, p, 8);
    if (verbose) printf("  %016llx\n", (unsigned long long)q);
    hmix(q);
}
static inline void hmix32(u32 v) {
    if (verbose) printf("  %08x\n", v);
    hmix(v);
}
static void report(const char *name) {
    printf("%-22s %016llx\n", name, (unsigned long long)H);
    H = 0xcbf29ce484222325ULL;
}
#define SETUP(argc) (verbose = (argc) > 1, setvbuf(stdout, 0, _IOFBF, 1 << 16))

// 128-bit integer pool: edge patterns followed by random values (16-byte aligned).
#define NPOOL 40
static __m128i pool[NPOOL] __attribute__((aligned(16)));
static void init_pool(void) {
    static const u64 edge[][2] = {
        {0, 0},
        {~0ULL, ~0ULL},
        {0x8000000000000000ULL, 0x8000000000000000ULL},
        {0x7fffffffffffffffULL, 0x7fffffffffffffffULL},
        {0x0101010101010101ULL, 0x0101010101010101ULL},
        {0x8080808080808080ULL, 0x7f7f7f7f7f7f7f7fULL},
        {0xff00ff00ff00ff00ULL, 0x00ff00ff00ff00ffULL},
        {0x0001000200030004ULL, 0x8000fffe7fff0001ULL},
        {0x00000001ffffffffULL, 0x80000000000000ffULL},
        {0x123456789abcdef0ULL, 0xfedcba9876543210ULL},
    };
    int n = sizeof edge / sizeof edge[0];
    for (int i = 0; i < NPOOL; i++) {
        u64 q[2];
        if (i < n) {
            q[0] = edge[i][0];
            q[1] = edge[i][1];
        } else {
            q[0] = rnd64();
            q[1] = rnd64();
            // sprinkle saturating-boundary bytes/words so clamps are exercised
            if (i % 3 == 0) q[0] = (q[0] & 0x00ff00ff00ff00ffULL) | 0x7f007f0080008000ULL;
            if (i % 5 == 0) q[1] &= 0x00ff00ff00ff00ffULL;
        }
        memcpy(&pool[i], q, 16);
    }
}
