// Shared helpers for the floating-point SSE tests: edge-value pools and MXCSR access.
#include "sse_common.h"

static inline void setcsr(u32 v) { asm volatile("ldmxcsr %0" ::"m"(v) : "memory"); }
static inline u32 getcsr(void) {
    u32 v;
    asm volatile("stmxcsr %0" : "=m"(v)::"memory");
    return v;
}

#define NF32 64
#define NF64 64
static u32 pool32[NF32] __attribute__((aligned(16)));
static u64 pool64[NF64] __attribute__((aligned(16)));

static int isnan32(u32 x) { return (x & 0x7fffffff) > 0x7f800000; }
static int isnan64(u64 x) { return (x & 0x7fffffffffffffffULL) > 0x7ff0000000000000ULL; }
static int issnan32(u32 x) { return isnan32(x) && !(x & 0x400000); }
static int issnan64(u64 x) { return isnan64(x) && !(x & 0x8000000000000ULL); }

static void init_fp_pools(void) {
    static const u32 e32[] = {
        0x00000000, 0x80000000, 0x3f800000, 0xbf800000, 0x40000000, 0x3f000000, 0x40400000, 0x41200000,
        0x501502f9, 0x2edbe6ff, 0x7f7fffff, 0xff7fffff, 0x00800000, 0x807fffff, 0x007fffff, 0x00000001,
        0x80000001, 0x00000002, 0x00400000, 0x00ffffff, 0x01000000, 0x7f800000, 0xff800000, 0x7fc00000,
        0xffc00000, 0x7fc01234, 0x7fa00000, 0xff800001, 0x3f800001, 0x3f7fffff, 0x4b800000, 0x4b800002,
        0x4f000000, 0x4f800000, 0xcf000000, 0xcf000001, 0x4effffff, 0x3dcccccd, 0x3e99999a, 0x3eaaaaab,
        0x3fc00000, 0x40a00000, 0xc0200000, 0x4b000000, 0x4b000001, 0x3f000001, 0x3effffff, 0x7e800000,
    };
    int n = sizeof e32 / sizeof e32[0];
    for (int i = 0; i < NF32; i++) {
        if (i < n) pool32[i] = e32[i];
        else {
            u64 r = rnd64();
            u32 m = (u32)r & 0x807fffff;
            u32 ex = (u32)(r >> 40) % 0x60 + 0x60;  // moderate exponents so results stay interesting
            pool32[i] = (i % 4 == 0) ? (u32)(r >> 8) : (m | (ex << 23));
        }
    }
    static const u64 e64[] = {
        0x0000000000000000ULL, 0x8000000000000000ULL, 0x3ff0000000000000ULL, 0xbff0000000000000ULL,
        0x4000000000000000ULL, 0x3fe0000000000000ULL, 0x4008000000000000ULL, 0x4024000000000000ULL,
        0x7fefffffffffffffULL, 0xffefffffffffffffULL, 0x0010000000000000ULL, 0x800fffffffffffffULL,
        0x000fffffffffffffULL, 0x0000000000000001ULL, 0x8000000000000001ULL, 0x0000000000000002ULL,
        0x0008000000000000ULL, 0x001fffffffffffffULL, 0x0020000000000000ULL, 0x7ff0000000000000ULL,
        0xfff0000000000000ULL, 0x7ff8000000000000ULL, 0xfff8000000000000ULL, 0x7ff8000000001234ULL,
        0x7ff4000000000000ULL, 0xfff0000000000001ULL, 0x3ff0000000000001ULL, 0x3fefffffffffffffULL,
        0x4330000000000000ULL, 0x4330000000000002ULL, 0x41e0000000000000ULL, 0xc1e0000000000000ULL,
        0xc1e0000000200000ULL, 0x41dfffffffc00000ULL, 0x41dfffffffffffffULL, 0xc1e00000001fffffULL,
        0x3fb999999999999aULL, 0x3fd3333333333333ULL, 0x3fd5555555555555ULL, 0x3ff8000000000000ULL,
        0x4014000000000000ULL, 0xc004000000000000ULL, 0x4340000000000000ULL, 0x3fe0000000000001ULL,
        0x3fdfffffffffffffULL, 0x7fd0000000000000ULL, 0x47efffffe0000000ULL, 0x36a0000000000000ULL,
    };
    n = sizeof e64 / sizeof e64[0];
    for (int i = 0; i < NF64; i++) {
        if (i < n) pool64[i] = e64[i];
        else {
            u64 r = rnd64();
            u64 m = rnd64() & 0x800fffffffffffffULL;
            u64 ex = (r >> 40) % 0x100 + 0x380;
            pool64[i] = (i % 4 == 0) ? rnd64() : (m | (ex << 52));
        }
    }
}

typedef union {
    __m128 f;
    __m128d d;
    __m128i i;
    u32 u[4];
    u64 q[2];
} V;

// The (cfg, flags) space: rounding mode x {none, FTZ, DAZ, FTZ+DAZ}.
#define NCFG 16
// Config index NCFG (16) is "round-nearest, PE flag preset": the only setting in which the emulator's
// host-FPU fast path is eligible, so it must be exercised too.
static u32 cfg_value(int c) {
    if (c == NCFG) return 0x1fa0;
    u32 v = 0x1f80 | ((u32)(c & 3) << 13);
    if (c & 4) v |= 0x8000;  // FTZ
    if (c & 8) v |= 0x40;    // DAZ
    return v;
}
static void cfg_name(int c, char *out) { if (c == NCFG) { strcpy(out, "rc0+pe"); return; } sprintf(out, "rc%d%s%s", c & 3, (c & 4) ? "+ftz" : "", (c & 8) ? "+daz" : ""); }
