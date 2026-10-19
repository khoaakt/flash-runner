// flags: -O1
/* x87 register-register arithmetic: all pool pairs, rounding-mode x precision grids, random vectors,
 * every D8/DC/DE operand-direction form. */
#include "x87_common.h"

#define RUN(insn)                                                                              \
    asm volatile("fninit\n fldcw %[cw]\n fldt %[b]\n fldt %[a]\n " insn                         \
                 "\n fnstsw %[s]\n fstpt %[r]\n fninit"                                         \
                 : [r] "=m"(*r), [s] "=m"(s)                                                    \
                 : [a] "m"(*a), [b] "m"(*b), [cw] "m"(cw)                                       \
                 : "memory", "st", "st(1)")

/* form: 0..5 -> D8 /k (ST0 = ST0 op ST1); 6..11 -> DC (ST1 = ST1 op ST0, result popped into place);
 * 12..17 -> DE (ST1 = ST1 op ST0, pop). k order: add mul sub subr div divr = /0 /1 /4 /5 /6 /7 */
static void bin(int form, const f80 *a, const f80 *b, uint16_t cw, f80 *r, uint16_t *sw) {
    uint16_t s;
    switch (form) {
    case 0: RUN(".byte 0xd8,0xc1"); break;
    case 1: RUN(".byte 0xd8,0xc9"); break;
    case 2: RUN(".byte 0xd8,0xe1"); break;
    case 3: RUN(".byte 0xd8,0xe9"); break;
    case 4: RUN(".byte 0xd8,0xf1"); break;
    case 5: RUN(".byte 0xd8,0xf9"); break;
    case 6: RUN(".byte 0xdc,0xc1\n fnstsw %[s]\n fstp %%st(0)"); break;
    case 7: RUN(".byte 0xdc,0xc9\n fnstsw %[s]\n fstp %%st(0)"); break;
    case 8: RUN(".byte 0xdc,0xe1\n fnstsw %[s]\n fstp %%st(0)"); break;
    case 9: RUN(".byte 0xdc,0xe9\n fnstsw %[s]\n fstp %%st(0)"); break;
    case 10: RUN(".byte 0xdc,0xf1\n fnstsw %[s]\n fstp %%st(0)"); break;
    case 11: RUN(".byte 0xdc,0xf9\n fnstsw %[s]\n fstp %%st(0)"); break;
    case 12: RUN(".byte 0xde,0xc1"); break;
    case 13: RUN(".byte 0xde,0xc9"); break;
    case 14: RUN(".byte 0xde,0xe1"); break;
    case 15: RUN(".byte 0xde,0xe9"); break;
    case 16: RUN(".byte 0xde,0xf1"); break;
    case 17: RUN(".byte 0xde,0xf9"); break;
    }
    *sw = s;
}

static void sqrt1(const f80 *a, uint16_t cw, f80 *r, uint16_t *sw) {
    uint16_t s;
    asm volatile("fninit\n fldcw %[cw]\n fldt %[a]\n fsqrt\n fnstsw %[s]\n fstpt %[r]\n fninit"
                 : [r] "=m"(*r), [s] "=m"(s) : [a] "m"(*a), [cw] "m"(cw) : "memory", "st");
    *sw = s;
}

static void line(const char *tag, int form, const f80 *a, const f80 *b, uint16_t cw, int sqrt_only) {
    f80 r; uint16_t sw;
    if (sqrt_only) sqrt1(a, cw, &r, &sw); else bin(form, a, b, cw, &r, &sw);
    printf("%s %d %04x ", tag, form, cw); p80("a", *a); if (!sqrt_only) p80(" b", *b);
    p80(" r", r); printf(" %02x\n", sw & SWM);
}

int main(void) {
    /* A: all pool pairs, D8 forms, round-to-nearest, extended precision */
    for (unsigned i = 0; i < NPOOL; i++)
        for (unsigned j = 0; j < NPOOL; j++)
            for (int f = 0; f < 6; f++) line("A", f, &pool[i], &pool[j], 0x037f, 0);
    /* B: small pool, every rounding mode and precision, D8 forms */
    for (unsigned i = 0; i < NSMALL; i++)
        for (unsigned j = 0; j < NSMALL; j++)
            for (int rc = 0; rc < 4; rc++)
                for (int pc = 0; pc < 3; pc++)
                    for (int f = 0; f < 6; f++) line("B", f, &small_pool[i], &small_pool[j], mkcw(rc, pc), 0);
    /* C: DC / DE forms, small pool, nearest + toward zero */
    for (unsigned i = 0; i < NSMALL; i++)
        for (unsigned j = 0; j < NSMALL; j++)
            for (int rc = 0; rc < 4; rc += 3)
                for (int f = 6; f < 18; f++) line("C", f, &small_pool[i], &small_pool[j], mkcw(rc, 2), 0);
    /* D: random operands, random rc/pc/form */
    for (int n = 0; n < 6000; n++) {
        f80 a = rnd80(), b = rnd80();
        uint64_t r = lcg();
        line("D", r % 18, &a, &b, mkcw((r >> 8) & 3, (r >> 12) % 3), 0);
    }
    /* E: sqrt grid */
    for (unsigned i = 0; i < NPOOL; i++)
        for (int rc = 0; rc < 4; rc++)
            for (int pc = 0; pc < 3; pc++) line("E", 0, &pool[i], &pool[i], mkcw(rc, pc), 1);
    for (int n = 0; n < 3000; n++) {
        f80 a = rnd80();
        a.se &= 0x7fff;
        uint64_t r = lcg();
        line("F", 0, &a, &a, mkcw((r >> 8) & 3, (r >> 12) % 3), 1);
    }
    return 0;
}
