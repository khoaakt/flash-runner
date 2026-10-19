// flags: -O1
/* FPREM/FPREM1 (iterated to completion), FSCALE, FXTRACT, FRNDINT, FCHS/FABS and the FLD constants. */
#include "x87_common.h"

#define CCQ(s) ((s) & 0x4700)
/* QEMU never reports the quotient bits (C0/C3/C1) for FPREM1 and does not flag #D for FSCALE/FXTRACT/FRNDINT
 * (the SDM does); mask those so the expected output stays meaningful. */
#define PQ(w, s) ((w) ? ((s) & 0x0400) : CCQ(s))
#define NODE(s) ((s) & SWM & ~0x02)
static int isnan80(f80 v) { return (v.se & 0x7fff) == 0x7fff && (v.m << 1) != 0; }

static void prem(int which, const f80 *a, const f80 *b, f80 *r, uint16_t *sw) {
    uint16_t s;
    if (which == 0)
        asm volatile("fninit\n fldt %[b]\n fldt %[a]\n 1: fprem\n fnstsw %%ax\n testb $4, %%ah\n jnz 1b\n fnstsw %[s]\n fstpt %[r]\n fninit"
                     : [r] "=m"(*r), [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "memory", "st", "st(1)", "eax", "cc");
    else
        asm volatile("fninit\n fldt %[b]\n fldt %[a]\n 1: fprem1\n fnstsw %%ax\n testb $4, %%ah\n jnz 1b\n fnstsw %[s]\n fstpt %[r]\n fninit"
                     : [r] "=m"(*r), [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "memory", "st", "st(1)", "eax", "cc");
    *sw = s;
}

static void scale(const f80 *a, const f80 *b, uint16_t cw, f80 *r, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n fldcw %[cw]\n fldt %[b]\n fldt %[a]\n fscale\n fnstsw %[s]\n fstpt %[r]\n fninit"
                 : [r] "=m"(*r), [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b), [cw] "m"(cw) : "memory", "st", "st(1)");
    *sw = s;
}

static void xtract(const f80 *a, f80 *e, f80 *m, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n fldt %[a]\n fxtract\n fnstsw %[s]\n fstpt %[m]\n fstpt %[e]\n fninit"
                 : [e] "=m"(*e), [m] "=m"(*m), [s] "=m"(s) : [a] "m"(*a) : "memory", "st", "st(1)");
    *sw = s;
}

static void rint1(const f80 *a, uint16_t cw, f80 *r, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n frndint\n fnstsw %[s]\n fstpt %[r]\n fninit"
                 : [r] "=m"(*r), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw) : "memory", "st");
    *sw = s;
}

static void chsabs(int k, const f80 *a, f80 *r) {
    if (k == 0) asm volatile("fninit\n fldt %[a]\n fchs\n fstpt %[r]\n fninit" : [r] "=m"(*r) : [a] "m"(*a) : "memory", "st");
    else asm volatile("fninit\n fldt %[a]\n fabs\n fstpt %[r]\n fninit" : [r] "=m"(*r) : [a] "m"(*a) : "memory", "st");
}

static f80 ldc(int k, uint16_t cw) {
    f80 r;
#define LC(op) asm volatile("fninit\n fldcw %[cw]\n " op "\n fstpt %[r]\n fninit" : [r] "=m"(r) : [cw] "m"(cw) : "memory", "st")
    switch (k) {
    case 0: LC("fld1"); break;
    case 1: LC("fldl2t"); break;
    case 2: LC("fldl2e"); break;
    case 3: LC("fldpi"); break;
    case 4: LC("fldlg2"); break;
    case 5: LC("fldln2"); break;
    default: LC("fldz"); break;
    }
    return r;
}

static f80 rnd_far(void) {
    f80 v = rnd80();
    uint64_t q = lcg();
    if (q % 4 == 0) v.se = (v.se & 0x8000) | (0x3fff + (int)(q >> 8) % 3000 - 1500);
    if (q % 4 == 1) v.se = (v.se & 0x8000) | (0x3fff + (int)(q >> 8) % 140 - 70);
    return v;
}

int main(void) {
    f80 r, r2; uint16_t sw;
    for (unsigned i = 0; i < NPOOL; i++)
        for (unsigned j = 0; j < NPOOL; j++)
            for (int w = 0; w < 2; w++) {
                prem(w, &pool[i], &pool[j], &r, &sw);
                printf("P%d %u %u ", w, i, j); p80("r", r); printf(" %04x\n", PQ(w, sw) | (sw & SWM));
            }
    for (int n = 0; n < 6000; n++) {
        f80 a = rnd_far(), b = rnd_far();
        uint64_t q = lcg();
        if (q % 4 == 0) b.se = (a.se & 0x7fff) - (q >> 8) % 70 + (b.se & 0x8000);
        for (int w = 0; w < 2; w++) {
            prem(w, &a, &b, &r, &sw);
            printf("R%d ", w); p80("a", a); p80(" b", b); p80(" r", r); printf(" %04x\n", PQ(w, sw) | (sw & SWM));
        }
    }
    for (unsigned i = 0; i < NPOOL; i++)
        for (unsigned j = 0; j < NPOOL; j++)
            for (int rc = 0; rc < 4; rc += 3) {
                if (isnan80(pool[i]) && isnan80(pool[j])) continue;
                scale(&pool[i], &pool[j], mkcw(rc, 2), &r, &sw);
                printf("S %u %u %d ", i, j, rc); p80("r", r); printf(" %02x\n", NODE(sw));
            }
    for (int n = 0; n < 5000; n++) {
        f80 a = rnd80(), b;
        uint64_t q = lcg();
        b = F(0x3fff + (q >> 8) % 20 - 4, lcg() | 0x8000000000000000ULL);
        if (q & 1) b.se |= 0x8000;
        if (q % 5 == 0) b = F(0x400d + (q >> 8) % 3, lcg() | 0x8000000000000000ULL), b.se |= (q & 2) << 14;
        if (q % 7 == 0) a.se = (a.se & 0x8000) | 0x3fff;
        scale(&a, &b, mkcw((q >> 4) & 3, 2), &r, &sw);
        printf("T "); p80("a", a); p80(" b", b); p80(" r", r); printf(" %02x\n", NODE(sw));
    }
    for (unsigned i = 0; i < NPOOL; i++) {
        xtract(&pool[i], &r, &r2, &sw);
        printf("X %u ", i); p80("e", r); p80(" m", r2); printf(" %02x\n", NODE(sw));
        chsabs(0, &pool[i], &r); chsabs(1, &pool[i], &r2);
        printf("N %u ", i); p80("chs", r); p80(" abs", r2); printf("\n");
        for (int rc = 0; rc < 4; rc++) {
            rint1(&pool[i], mkcw(rc, 2), &r, &sw);
            printf("I %u %d ", i, rc); p80("r", r); printf(" %02x\n", NODE(sw));
        }
    }
    for (int n = 0; n < 4000; n++) {
        f80 a = rnd80();
        uint64_t q = lcg();
        a.se = (a.se & 0x8000) | (0x3fff + (q >> 8) % 70 - 8);
        if (q % 3 == 0) a.m = (a.m & ~0x3ffULL) | 0x200;
        xtract(&a, &r, &r2, &sw);
        printf("Y "); p80("a", a); p80(" e", r); p80(" m", r2); printf(" %02x\n", NODE(sw));
        rint1(&a, mkcw((q >> 4) & 3, 2), &r, &sw);
        printf("J %d ", (int)((q >> 4) & 3)); p80("a", a); p80(" r", r); printf(" %02x\n", NODE(sw));
    }
    for (int k = 0; k < 7; k++)
        for (int rc = 0; rc < 4; rc++) {
            r = ldc(k, mkcw(rc, 2));
            printf("K %d %d ", k, rc); p80("r", r); printf("\n");
        }
    return 0;
}
