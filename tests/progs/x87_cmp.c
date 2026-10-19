// flags: -O1
/* x87 comparisons (FCOM/FUCOM/FCOMI/FUCOMI/FICOM/FTST), FXAM and FCMOVcc. */
#include "x87_common.h"

#define CC(sw) ((sw) & 0x4500)

/* variant: 0 fcom 1 fucom 2 fcomp 3 fucomp 4 fcompp 5 fucompp 6 ftst(a only) */
static uint16_t cmp(int v, const f80 *a, const f80 *b) {
    uint16_t s;
    switch (v) {
    case 0: asm volatile("fninit\n fldt %[b]\n fldt %[a]\n fcom %%st(1)\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)"); break;
    case 1: asm volatile("fninit\n fldt %[b]\n fldt %[a]\n fucom %%st(1)\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)"); break;
    case 2: asm volatile("fninit\n fldt %[b]\n fldt %[a]\n fcomp %%st(1)\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)"); break;
    case 3: asm volatile("fninit\n fldt %[b]\n fldt %[a]\n fucomp %%st(1)\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)"); break;
    case 4: asm volatile("fninit\n fldt %[b]\n fldt %[a]\n fcompp\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)"); break;
    case 5: asm volatile("fninit\n fldt %[b]\n fldt %[a]\n fucompp\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)"); break;
    default: asm volatile("fninit\n fldt %[a]\n ftst\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a) : "st"); break;
    }
    return s;
}

/* FCOMI family: returns EFLAGS (ZF PF CF) via lahf-free pushf, and the status word */
static uint32_t cmpi(int v, const f80 *a, const f80 *b, uint16_t *sw) {
    uint32_t fl; uint16_t s;
#define CI(op) asm volatile("fninit\n fldt %[b]\n fldt %[a]\n " op "\n pushf\n pop %[f]\n fnstsw %[s]\n fninit" : [f] "=r"(fl), [s] "=m"(s) : [a] "m"(*a), [b] "m"(*b) : "st", "st(1)", "cc")
    switch (v) {
    case 0: CI("fcomi %%st(1), %%st"); break;
    case 1: CI("fucomi %%st(1), %%st"); break;
    case 2: CI("fcomip %%st(1), %%st"); break;
    default: CI("fucomip %%st(1), %%st"); break;
    }
    *sw = s;
    return fl & 0x45;
}

static uint16_t fxam(const f80 *a, int empty) {
    uint16_t s;
    if (empty) asm volatile("fninit\n fxam\n fnstsw %[s]\n fninit" : [s] "=m"(s) : : "st");
    else asm volatile("fninit\n fldt %[a]\n fxam\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a) : "st");
    return s;
}

/* FCMOVcc: set EFLAGS via a controlled sahf, then fcmovX st(1) -> st ; return ST0 low mantissa marker */
static uint64_t cmov(int cond, int flagsel) {
    uint64_t r;
    static const f80 one = {0x8000000000000000ULL, 0x3fff}, two = {0x8000000000000000ULL, 0x4000};
    uint32_t ah = flagsel; /* bit0 CF, bit2 PF, bit6 ZF */
    uint16_t se;
#define CM(op) asm volatile("fninit\n fldt %[o]\n fldt %[t]\n mov %[ah], %%eax\n sahf\n " op "\n fstpt %[r]\n fninit" : [r] "=m"(*(f80*)&rr) : [o] "m"(one), [t] "m"(two), [ah] "r"(ah) : "eax", "st", "st(1)", "cc")
    f80 rr;
    switch (cond) {
    case 0: CM("fcmovb %%st(1), %%st"); break;
    case 1: CM("fcmove %%st(1), %%st"); break;
    case 2: CM("fcmovbe %%st(1), %%st"); break;
    case 3: CM("fcmovu %%st(1), %%st"); break;
    case 4: CM("fcmovnb %%st(1), %%st"); break;
    case 5: CM("fcmovne %%st(1), %%st"); break;
    case 6: CM("fcmovnbe %%st(1), %%st"); break;
    default: CM("fcmovnu %%st(1), %%st"); break;
    }
    se = rr.se; r = rr.m; (void)se;
    return r ? (rr.se == 0x3fff ? 1 : 2) : 0;
}

static uint16_t icmp(int kind, const f80 *a, int32_t v) {
    uint16_t s; int16_t v16 = (int16_t)v;
    if (kind == 0) asm volatile("fninit\n fldt %[a]\n ficoml %[v]\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [v] "m"(v) : "st");
    else asm volatile("fninit\n fldt %[a]\n ficoms %[v]\n fnstsw %[s]\n fninit" : [s] "=m"(s) : [a] "m"(*a), [v] "m"(v16) : "st");
    return s;
}

int main(void) {
    for (unsigned i = 0; i < NPOOL; i++)
        for (unsigned j = 0; j < NPOOL; j++) {
            printf("C %u %u", i, j);
            for (int v = 0; v < 6; v++) { uint16_t s = cmp(v, &pool[i], &pool[j]); printf(" %04x", (CC(s) | (s & SWM))); }
            for (int v = 0; v < 4; v++) { uint16_t s; uint32_t f = cmpi(v, &pool[i], &pool[j], &s); printf(" %02x/%02x", f, s & SWM); }
            printf("\n");
        }
    for (unsigned i = 0; i < NPOOL; i++) {
        uint16_t s = cmp(6, &pool[i], &pool[i]);
        uint16_t x = fxam(&pool[i], 0);
        printf("X %u %04x %04x\n", i, CC(s) | (s & SWM), x & 0x4700);
    }
    printf("X empty %04x\n", fxam(&pool[0], 1) & 0x4700);
    for (int c = 0; c < 8; c++)
        for (int fl = 0; fl < 8; fl++) {
            uint32_t f = (fl & 1) | ((fl & 2) ? 4 : 0) | ((fl & 4) ? 0x40 : 0);
            printf("M %d %d %llu\n", c, fl, (unsigned long long)cmov(c, f));
        }
    for (unsigned i = 0; i < NPOOL; i++)
        for (int32_t v = -2; v <= 3; v++) {
            uint16_t s = icmp(0, &pool[i], v), t = icmp(1, &pool[i], v);
            printf("I %u %d %04x %04x\n", i, v, CC(s) | (s & SWM), CC(t) | (t & SWM));
        }
    return 0;
}
