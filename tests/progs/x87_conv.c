// flags: -O1
/* x87 conversions: m32/m64 loads and stores (all rounding modes), FILD/FIST/FISTP/FISTTP,
 * FBLD/FBSTP, plus the m32/m64/int memory-operand arithmetic forms. */
#include "x87_common.h"

static void ld32(uint32_t v, f80 *r, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n flds %[v]\n fnstsw %[s]\n fstpt %[r]\n fninit" : [r] "=m"(*r), [s] "=m"(s) : [v] "m"(v) : "memory", "st");
    *sw = s;
}
static void ld64(uint64_t v, f80 *r, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n fldl %[v]\n fnstsw %[s]\n fstpt %[r]\n fninit" : [r] "=m"(*r), [s] "=m"(s) : [v] "m"(v) : "memory", "st");
    *sw = s;
}
static uint32_t st32(const f80 *a, uint16_t cw, uint16_t *sw) {
    uint16_t s; uint32_t o;
    asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n fstps %[o]\n fnstsw %[s]\n fninit" : [o] "=m"(o), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw) : "memory", "st");
    *sw = s;
    return o;
}
static uint64_t st64(const f80 *a, uint16_t cw, uint16_t *sw) {
    uint16_t s; uint64_t o;
    asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n fstpl %[o]\n fnstsw %[s]\n fninit" : [o] "=m"(o), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw) : "memory", "st");
    *sw = s;
    return o;
}

static uint32_t rnd32(void) {
    uint64_t r = lcg();
    uint32_t m = (uint32_t)(lcg() & 0x7fffff);
    switch (r % 8) {
    case 0: m = 0; break;
    case 1: m = 0x7fffff; break;
    case 2: m &= 0x3; break;
    }
    uint32_t e;
    switch ((r >> 4) % 8) {
    case 0: e = 0; break;
    case 1: e = 0xff; break;
    case 2: e = 1; break;
    case 3: e = 0xfe; break;
    default: e = 127 - 12 + (r >> 8) % 40;
    }
    return ((r >> 20) & 1) << 31 | e << 23 | m;
}
static uint64_t rnd64(void) {
    uint64_t r = lcg();
    uint64_t m = lcg() & 0xfffffffffffffULL;
    switch (r % 8) {
    case 0: m = 0; break;
    case 1: m = 0xfffffffffffffULL; break;
    case 2: m &= 0x3; break;
    }
    uint64_t e;
    switch ((r >> 4) % 8) {
    case 0: e = 0; break;
    case 1: e = 0x7ff; break;
    case 2: e = 1; break;
    case 3: e = 0x7fe; break;
    default: e = 1023 - 12 + (r >> 8) % 40;
    }
    return ((r >> 20) & 1) << 63 | e << 52 | m;
}

#define IRUN(insn, ty)                                                                                      \
    do {                                                                                                    \
        ty o = 0; uint16_t s;                                                                               \
        asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n " insn "\n fnstsw %[s]\n fninit"                    \
                     : [o] "=m"(o), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw) : "memory", "st");              \
        *sw = s; return (uint64_t)o;                                                                         \
    } while (0)

/* kind 0..5: fists, fistl, fistpll, fisttps, fisttpl, fisttpll */
static uint64_t fist(int kind, const f80 *a, uint16_t cw, uint16_t *sw) {
    switch (kind) {
    case 0: IRUN("fistps %[o]", uint16_t);
    case 1: IRUN("fistpl %[o]", uint32_t);
    case 2: IRUN("fistpll %[o]", uint64_t);
    case 3: IRUN("fisttps %[o]", uint16_t);
    case 4: IRUN("fisttpl %[o]", uint32_t);
    default: IRUN("fisttpll %[o]", uint64_t);
    }
}

static void fild(int kind, uint64_t v, f80 *r, uint16_t *sw) {
    uint16_t s; int16_t v16 = (int16_t)v; int32_t v32 = (int32_t)v; int64_t v64 = (int64_t)v;
    switch (kind) {
    case 0: asm volatile("fninit\n filds %[v]\n fnstsw %[s]\n fstpt %[r]\n fninit" : [r] "=m"(*r), [s] "=m"(s) : [v] "m"(v16) : "memory", "st"); break;
    case 1: asm volatile("fninit\n fildl %[v]\n fnstsw %[s]\n fstpt %[r]\n fninit" : [r] "=m"(*r), [s] "=m"(s) : [v] "m"(v32) : "memory", "st"); break;
    default: asm volatile("fninit\n fildll %[v]\n fnstsw %[s]\n fstpt %[r]\n fninit" : [r] "=m"(*r), [s] "=m"(s) : [v] "m"(v64) : "memory", "st"); break;
    }
    *sw = s;
}

static void bld(const uint8_t *b, f80 *r) {
    asm volatile("fninit\n fbld %[b]\n fstpt %[r]\n fninit" : [r] "=m"(*r) : [b] "m"(*(const uint8_t(*)[10])b) : "memory", "st");
}
static void bst(const f80 *a, uint16_t cw, uint8_t *o, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n fbstp %[o]\n fnstsw %[s]\n fninit"
                 : [o] "=m"(*(uint8_t(*)[10])o), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw) : "memory", "st");
    *sw = s;
}

/* memory-operand arithmetic: kind 0..5 = add mul sub subr div divr for m32 (D8), m64 (DC), m16int (DE), m32int (DA) */
#define MOP(insn, val)                                                                                         \
    asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n " insn "\n fnstsw %[s]\n fstpt %[r]\n fninit"          \
                 : [r] "=m"(*r), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw), [v] "m"(val) : "memory", "st")
static void memop(int form, int k, const f80 *a, uint64_t raw, uint16_t cw, f80 *r, uint16_t *sw) {
    uint16_t s; uint32_t v32 = (uint32_t)raw; uint64_t v64 = raw; int16_t v16 = (int16_t)raw; int32_t i32 = (int32_t)raw;
    switch (form * 8 + k) {
    case 0: MOP("fadds %[v]", v32); break;   case 1: MOP("fmuls %[v]", v32); break;
    case 2: MOP("fsubs %[v]", v32); break;   case 3: MOP("fsubrs %[v]", v32); break;
    case 4: MOP("fdivs %[v]", v32); break;   case 5: MOP("fdivrs %[v]", v32); break;
    case 8: MOP("faddl %[v]", v64); break;   case 9: MOP("fmull %[v]", v64); break;
    case 10: MOP("fsubl %[v]", v64); break;  case 11: MOP("fsubrl %[v]", v64); break;
    case 12: MOP("fdivl %[v]", v64); break;  case 13: MOP("fdivrl %[v]", v64); break;
    case 16: MOP("fiadds %[v]", v16); break; case 17: MOP("fimuls %[v]", v16); break;
    case 18: MOP("fisubs %[v]", v16); break; case 19: MOP("fisubrs %[v]", v16); break;
    case 20: MOP("fidivs %[v]", v16); break; case 21: MOP("fidivrs %[v]", v16); break;
    case 24: MOP("fiaddl %[v]", i32); break; case 25: MOP("fimull %[v]", i32); break;
    case 26: MOP("fisubl %[v]", i32); break; case 27: MOP("fisubrl %[v]", i32); break;
    case 28: MOP("fidivl %[v]", i32); break; case 29: MOP("fidivrl %[v]", i32); break;
    }
    *sw = s;
}

int main(void) {
    f80 r; uint16_t sw;
    /* L32 / L64: loads of random patterns */
    for (int n = 0; n < 3000; n++) {
        uint32_t v = rnd32(); ld32(v, &r, &sw);
        printf("L32 %08x ", v); p80("r", r); printf(" %02x\n", sw & SWM);
    }
    for (int n = 0; n < 3000; n++) {
        uint64_t v = rnd64(); ld64(v, &r, &sw);
        printf("L64 %016llx ", (unsigned long long)v); p80("r", r); printf(" %02x\n", sw & SWM);
    }
    /* S32 / S64: stores of pool + random values in every rounding mode (extended precision control) */
    for (unsigned i = 0; i < NPOOL; i++)
        for (int rc = 0; rc < 4; rc++) {
            uint16_t cw = mkcw(rc, 2);
            uint32_t a = st32(&pool[i], cw, &sw); printf("S32 %d ", rc); p80("a", pool[i]); printf(" %08x %02x\n", a, sw & SWM);
            uint64_t b = st64(&pool[i], cw, &sw); printf("S64 %d ", rc); p80("a", pool[i]); printf(" %016llx %02x\n", (unsigned long long)b, sw & SWM);
        }
    for (int n = 0; n < 6000; n++) {
        f80 a = rnd80();
        uint64_t q = lcg();
        if (q % 3 == 0) { /* values near the f32/f64 range edges */
            a.se = (a.se & 0x8000) | ((q & 1) ? 0x3fff + 127 - 2 + (q >> 8) % 6 : 0x3fff - 126 - 26 + (q >> 8) % 30);
        } else if (q % 3 == 1) {
            a.se = (a.se & 0x8000) | ((q & 1) ? 0x3fff + 1023 - 2 + (q >> 8) % 6 : 0x3fff - 1022 - 55 + (q >> 8) % 60);
        }
        uint16_t cw = mkcw((q >> 4) & 3, 2);
        uint32_t x = st32(&a, cw, &sw); printf("R32 %d ", (int)((q >> 4) & 3)); p80("a", a); printf(" %08x %02x\n", x, sw & SWM);
        uint64_t y = st64(&a, cw, &sw); printf("R64 %d ", (int)((q >> 4) & 3)); p80("a", a); printf(" %016llx %02x\n", (unsigned long long)y, sw & SWM);
    }
    /* FILD */
    static const uint64_t iv[] = {0, 1, 2, 3, 0x7fff, 0x8000, 0xffff, 0x7fffffff, 0x80000000ULL, 0xffffffffULL, 0x100000000ULL,
        0x7fffffffffffffffULL, 0x8000000000000000ULL, 0xffffffffffffffffULL, 0xfffffffffffffffeULL, 0x123456789abcdefULL,
        0x0020000000000001ULL, 0x4000000000000001ULL, 0xfffffffffffff800ULL, 0x00000000deadbeefULL};
    for (unsigned i = 0; i < sizeof iv / sizeof iv[0]; i++)
        for (int k = 0; k < 3; k++) { fild(k, iv[i], &r, &sw); printf("I%d %016llx ", k, (unsigned long long)iv[i]); p80("r", r); printf(" %02x\n", sw & SWM); }
    /* FIST family over the pool and random values, all rounding modes */
    for (int n = 0; n < (int)NPOOL + 4000; n++) {
        f80 a;
        uint64_t q = lcg();
        if (n < (int)NPOOL) a = pool[n];
        else {
            a = rnd80();
            if (q % 2) a.se = (a.se & 0x8000) | (0x3fff + (q >> 8) % 70 - 8); /* magnitude 2^-8 .. 2^62 */
            if (q % 5 == 0) a.m = (a.m & ~0x3ffULL) | 0x200; /* exact .5-ish ties at integer boundaries */
        }
        for (int rc = 0; rc < 4; rc++)
            for (int k = 0; k < 6; k++) {
                if ((k == 0 || k == 3) && n >= (int)NPOOL && (n & 1)) continue;
                uint64_t o = fist(k, &a, mkcw(rc, 2), &sw);
                printf("F%d %d ", k, rc); p80("a", a); printf(" %016llx %02x\n", (unsigned long long)o, sw & SWM);
            }
    }
    /* BCD */
    for (int n = 0; n < 2500; n++) {
        uint8_t b[10]; uint64_t q = lcg();
        for (int i = 0; i < 9; i++) { uint8_t d1 = (lcg() % 10), d2 = (lcg() % 10); b[i] = d1 << 4 | d2; if (q % 4 == 0 && i > (int)(q >> 4) % 9) b[i] = 0; }
        b[9] = (q >> 20) & 1 ? 0x80 : 0;
        bld(b, &r);
        printf("B1 "); for (int i = 9; i >= 0; i--) printf("%02x", b[i]); printf(" "); p80("r", r); printf("\n");
    }
    for (int n = 0; n < (int)NPOOL + 3000; n++) {
        f80 a;
        uint64_t q = lcg();
        if (n < (int)NPOOL) a = pool[n];
        else { a = rnd80(); a.se = (a.se & 0x8000) | (0x3fff + (q >> 8) % 70 - 8); if (q % 4 == 0) a.se = (a.se & 0x8000) | (0x3fff + 55 + (q >> 12) % 6); }
        for (int rc = 0; rc < 4; rc++) {
            uint8_t o[10]; bst(&a, mkcw(rc, 2), o, &sw);
            printf("B2 %d ", rc); p80("a", a); printf(" "); for (int i = 9; i >= 0; i--) printf("%02x", o[i]); printf(" %02x\n", sw & SWM);
        }
    }
    /* memory-operand arithmetic */
    for (int n = 0; n < 8000; n++) {
        f80 a = rnd80(); uint64_t q = lcg(); uint64_t raw;
        int form = (q >> 3) & 3, k = (q >> 6) % 6;
        if (form == 0) raw = rnd32(); else if (form == 1) raw = rnd64(); else raw = lcg() >> (q % 40);
        if (n < 50) a = pool[n % NPOOL];
        int pcx = (q >> 12) % 3;
        /* QEMU rounds m64/m32int operands to the 24-bit precision when PC=24 (the SDM says the operand
         * is converted exactly), so only use PC=24 where the operand fits in 24 bits anyway. */
        if (pcx == 0 && (form == 1 || form == 3)) pcx = 1;
        uint16_t cw = mkcw((q >> 10) & 3, pcx);
        memop(form, k, &a, raw, cw, &r, &sw);
        printf("M%d.%d %04x ", form, k, cw); p80("a", a); printf(" %016llx ", (unsigned long long)raw); p80("r", r); printf(" %02x\n", sw & SWM);
    }
    return 0;
}
