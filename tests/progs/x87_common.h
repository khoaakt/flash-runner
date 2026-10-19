/* Shared helpers for the x87_* differential tests. All values are handled as raw 80-bit patterns so
 * nothing is constant-folded by the compiler and output does not depend on printf of long double. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

typedef struct __attribute__((packed)) { uint64_t m; uint16_t se; } f80;

#define F(se_, m_) ((f80){ (m_), (se_) })

static uint64_t lcg_state = 0x123456789abcdef1ULL;
static uint64_t lcg(void) {
    lcg_state = lcg_state * 6364136223846793005ULL + 1442695040888963407ULL;
    return lcg_state >> 11 ^ (lcg_state << 20);
}

static void p80(const char *tag, f80 v) { printf("%s %04x:%016llx", tag, v.se, (unsigned long long)v.m); }

/* Masked status word: exception flags + stack fault only (C1 is not modelled by QEMU, C0/C2/C3 are
 * printed explicitly by tests that care). */
#define SWM 0x007f

/* Interesting 80-bit encodings. */
static const f80 pool[] = {
    F(0x0000, 0x0000000000000000ULL), F(0x8000, 0x0000000000000000ULL),   /* +-0 */
    F(0x0000, 0x0000000000000001ULL), F(0x8000, 0x0000000000000001ULL),   /* min denormal */
    F(0x0000, 0x7fffffffffffffffULL),                                     /* max denormal */
    F(0x0000, 0x8000000000000000ULL),                                     /* pseudo-denormal */
    F(0x0001, 0x8000000000000000ULL), F(0x8001, 0x8000000000000000ULL),   /* min normal */
    F(0x0001, 0xffffffffffffffffULL),
    F(0x7ffe, 0xffffffffffffffffULL), F(0xfffe, 0xffffffffffffffffULL),   /* max normal */
    F(0x7ffe, 0x8000000000000000ULL),
    F(0x3fff, 0x8000000000000000ULL), F(0xbfff, 0x8000000000000000ULL),   /* +-1 */
    F(0x4000, 0x8000000000000000ULL), F(0xc000, 0x8000000000000000ULL),   /* +-2 */
    F(0x3ffe, 0x8000000000000000ULL),                                     /* 0.5 */
    F(0x4000, 0xc000000000000000ULL),                                     /* 3 */
    F(0x3fff, 0xffffffffffffffffULL),                                     /* 2 - 2^-63 */
    F(0x3fff, 0x8000000000000001ULL),                                     /* 1 + 2^-63 */
    F(0x3fff, 0x8000000000000400ULL),                                     /* 1 + 2^-53: tie for PC=53 */
    F(0x3fff, 0x8000008000000000ULL),                                     /* 1 + 2^-24: tie for PC=24 */
    F(0x3fff, 0x8000000000000200ULL), F(0x3fff, 0x80000000000007ffULL),
    F(0x4000, 0xc90fdaa22168c235ULL),                                     /* pi */
    F(0x3ffd, 0xaaaaaaaaaaaaaaabULL),                                     /* 1/3 */
    F(0x3fc0, 0x8000000000000000ULL), F(0x403e, 0x8000000000000000ULL),   /* 2^-63, 2^63 */
    F(0x403d, 0xfffffffffffffffeULL),
    F(0x7fff, 0x8000000000000000ULL), F(0xffff, 0x8000000000000000ULL),   /* +-inf */
    F(0x7fff, 0xc000000000000000ULL),                                     /* QNaN indefinite-like */
    F(0xffff, 0xc000000000000000ULL),                                     /* real indefinite */
    F(0x7fff, 0xa000000000000000ULL),                                     /* SNaN */
    F(0x7fff, 0xc000000000000123ULL), F(0x7fff, 0xa000000000000456ULL),
    F(0x3fff, 0xc000000000000000ULL), F(0xbffe, 0xb504f333f9de6484ULL),
    F(0x4033, 0xe000000000000000ULL), F(0x3f01, 0x9999999999999999ULL),
};
#define NPOOL (sizeof(pool) / sizeof(pool[0]))

/* A smaller pool for the full rounding-mode x precision grids. */
static const f80 small_pool[] = {
    F(0x0000, 0x0000000000000000ULL), F(0x8000, 0x0000000000000000ULL),
    F(0x0000, 0x0000000000000003ULL), F(0x0001, 0x8000000000000000ULL),
    F(0x3fff, 0x8000000000000000ULL), F(0xbfff, 0x8000000000000000ULL),
    F(0x3fff, 0x8000000000000400ULL), F(0x3fff, 0x80000000000007ffULL),
    F(0x4000, 0xc90fdaa22168c235ULL), F(0x3ffd, 0xaaaaaaaaaaaaaaabULL),
    F(0x7ffe, 0xffffffffffffffffULL), F(0xfffe, 0xffffffffffffffffULL),
    F(0x7fff, 0x8000000000000000ULL), F(0x7fff, 0xc000000000000000ULL),
    F(0x3fff, 0xfffffffffffffc00ULL), F(0x0001, 0xffffffffffffffffULL),
};
#define NSMALL (sizeof(small_pool) / sizeof(small_pool[0]))

/* Random 80-bit value, biased towards interesting exponents and mantissas. */
static f80 rnd80(void) {
    uint64_t r = lcg(), s = lcg();
    f80 v;
    int kind = r % 8;
    v.m = s | 0x8000000000000000ULL;
    if (kind == 0) v.m = 0x8000000000000000ULL | (s & 0xffff);
    else if (kind == 1) v.m = 0xffffffffffffffffULL - (s & 0xffff);
    else if (kind == 2) v.m = 0x8000000000000000ULL | ((s & 3) << 10) | (s >> 62);
    int e = (r >> 8) % 64;
    uint16_t be;
    if (e < 40) be = 0x3fff - 32 + e;       /* near 1 */
    else if (e < 44) be = 1 + (r >> 20) % 70;
    else if (e < 48) be = 0x7ffe - (r >> 20) % 70;
    else if (e < 51) be = 0;                /* denormals */
    else be = 0x3fff + (int)((r >> 20) % 400) - 200;
    if (be == 0) v.m &= 0x7fffffffffffffffULL;
    v.se = be | (((r >> 40) & 1) << 15);
    return v;
}

/* control words: PC in {24, 53, 64} x RC in {0,1,2,3}, all exceptions masked */
static uint16_t mkcw(int rc, int pcix) {
    static const uint16_t pcb[3] = {0x0000, 0x0200, 0x0300};
    return 0x003f | pcb[pcix] | (rc << 10);
}
