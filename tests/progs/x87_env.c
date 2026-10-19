// flags: -O1
/* x87 state images: FNSTENV/FNSAVE/FRSTOR/FLDENV/FXSAVE/FXRSTOR, tag word, TOP rotation, FFREE, control
 * word round trips, FNSTSW AX, and random (depth-valid) instruction sequences ending in a state dump. */
#include "x87_common.h"

static uint8_t sav[112] __attribute__((aligned(16)));
static uint8_t fxs[512] __attribute__((aligned(16)));
static uint8_t env[32] __attribute__((aligned(16)));

static uint16_t w16(const uint8_t *b, int o) { return b[o] | b[o + 1] << 8; }

/* Dump control state and registers. mode 0: fxsave (non destructive) then fnsave */
static void dump(const char *tag) {
    asm volatile("fxsave %0" : "=m"(fxs) : : "memory");
    asm volatile("fnsave %0" : "=m"(sav) : : "memory");   /* also re-initialises the FPU */
    printf("%s fx cw=%04x sw=%04x ftw=%02x mask=%08x mxcsr=%08x", tag, w16(fxs, 0), w16(fxs, 2) & ~0x0280, fxs[4],
           *(uint32_t *)(fxs + 28), *(uint32_t *)(fxs + 24));
    for (int i = 0; i < 8; i++) { f80 v; memcpy(&v, fxs + 32 + 16 * i, 10); printf(" "); printf("%04x:%016llx", v.se, (unsigned long long)v.m); }
    printf("\n%s sv cw=%04x sw=%04x tw=%04x", tag, w16(sav, 0), w16(sav, 4) & ~0x0280, w16(sav, 8));
    for (int i = 0; i < 8; i++) { f80 v; memcpy(&v, sav + 28 + 10 * i, 10); printf(" %04x:%016llx", v.se, (unsigned long long)v.m); }
    printf("\n");
}

#define LD(v) asm volatile("fldt %0" : : "m"(v) : "st")
#define OP(s) asm volatile(s : : : "st", "st(1)", "st(2)", "st(3)", "st(4)", "st(5)", "st(6)", "st(7)")
static char trace[400]; static int ntr;
static void TR(const char *s) { ntr += snprintf(trace + ntr, sizeof trace - ntr - 1, "%s;", s); }
#define ROP(s) do { OP(s); TR(s); } while (0)


int main(void) {
    /* 1. clean state */
    asm volatile("fninit");
    dump("init");
    /* 2. every tag class on the stack */
    for (unsigned i = 0; i < NPOOL; i++) {
        asm volatile("fninit");
        LD(pool[i]); LD(pool[(i + 3) % NPOOL]); LD(pool[(i + 7) % NPOOL]);
        dump("cls");
    }
    /* 3. TOP rotation and FFREE */
    asm volatile("fninit");
    LD(pool[12]); LD(pool[14]); LD(pool[22]); LD(pool[0]);
    OP("fincstp"); dump("inc");
    LD(pool[12]); LD(pool[14]); LD(pool[22]); LD(pool[0]);
    OP("fdecstp"); OP("fdecstp"); dump("dec2");
    LD(pool[12]); LD(pool[14]); LD(pool[22]); LD(pool[0]);
    OP("ffree %%st(1)"); dump("free1");
    LD(pool[12]); LD(pool[14]); LD(pool[22]); LD(pool[0]);
    OP("ffree %%st(0)"); OP("ffree %%st(2)"); dump("free02");
    LD(pool[12]); LD(pool[14]);
    OP("fxch %%st(1)"); OP("fstp %%st(1)"); dump("xchstp");
    /* 4. control word round trip */
    for (int rc = 0; rc < 4; rc++)
        for (int pc = 0; pc < 4; pc++) {
            uint16_t cw = 0x003f | (pc << 8) | (rc << 10) | ((rc & 1) << 12), back; uint16_t sw0;
            asm volatile("fninit\n fldcw %1\n fnstcw %0\n fnstsw %2" : "=m"(back), "+m"(cw), "=m"(sw0) : : "st");
            printf("cw %04x -> %04x sw %04x\n", cw, back, sw0);
        }
    /* 5. FSTENV / FLDENV / FRSTOR round trips */
    {
        asm volatile("fninit");
        LD(pool[22]); LD(pool[12]); LD(pool[0]);
        uint16_t cw = 0x027f;
        asm volatile("fldcw %0" : : "m"(cw));
        asm volatile("fnstenv %0" : "=m"(env) : : "memory");
        printf("env cw=%04x sw=%04x tw=%04x\n", w16(env, 0), w16(env, 4) & ~0x0280, w16(env, 8));
        asm volatile("fnsave %0" : "=m"(sav) : : "memory");
        LD(pool[14]);
        asm volatile("frstor %0" : : "m"(sav) : "memory", "st", "st(1)", "st(2)", "st(3)", "st(4)", "st(5)", "st(6)", "st(7)");
        dump("frstor");
        asm volatile("fninit");
        LD(pool[3]); LD(pool[5]); LD(pool[29]);
        asm volatile("fxsave %0" : "=m"(fxs) : : "memory");
        asm volatile("fninit");
        LD(pool[12]);
        asm volatile("fxrstor %0" : : "m"(fxs) : "memory", "st", "st(1)", "st(2)", "st(3)", "st(4)", "st(5)", "st(6)", "st(7)");
        dump("fxrstor");
        asm volatile("fninit");
        LD(pool[12]); LD(pool[14]);
        asm volatile("fnstenv %0" : "=m"(env) : : "memory");
        asm volatile("fninit");
        LD(pool[0]);
        asm volatile("fldenv %0" : : "m"(env) : "memory", "st");
        dump("fldenv");
    }
    /* 6. random depth-valid sequences */
    static uint32_t m32s[8]; static uint64_t m64s[8]; static int32_t i32s[4];
    for (int n = 0; n < 3000; n++) {
        asm volatile("fninit"); trace[0] = 0; ntr = 0;
        int depth = 0, len = 3 + lcg() % 14;
        for (int k = 0; k < len; k++) {
            uint64_t r = lcg();
            int op = r % 30, i = depth ? (r >> 8) % depth : 0;
            f80 v = (r >> 20) % 3 ? pool[(r >> 24) % NPOOL] : rnd80();
            if (op < 6 && depth < 8) {
                switch (op) {
                case 0: case 1: { LD(v); TR("fldt"); } break;
                case 2: { m32s[0] = (uint32_t)(r >> 32) ^ 0x3f800000; asm volatile("flds %0" : : "m"(m32s[0]) : "st"); TR("flds"); } break;
                case 3: { m64s[0] = lcg() & ~0x1fffffffULL; if (!(m64s[0] >> 62 & 1)) m64s[0] ^= 0x3ff0000000000000ULL; asm volatile("fldl %0" : : "m"(m64s[0]) : "st"); TR("fldl"); } break;
                case 4: { i32s[0] = (int32_t)(lcg() >> (40 + r % 24)); asm volatile("fildl %0" : : "m"(i32s[0]) : "st"); TR("fildl"); } break;
                default:
                    switch ((r >> 30) % 7) { case 0: ROP("fld1"); break; case 1: ROP("fldz"); break; case 2: ROP("fldpi"); break; case 3: ROP("fldl2e"); break; case 4: ROP("fldln2"); break; case 5: ROP("fldl2t"); break; default: ROP("fldlg2"); }
                }
                depth++;
            } else if (op < 10 && depth >= 1) {
                switch (i) { case 0: ROP("fst %%st(0)"); break; case 1: ROP("fst %%st(1)"); break; case 2: ROP("fst %%st(2)"); break; case 3: ROP("fst %%st(3)"); break;
                             case 4: ROP("fst %%st(4)"); break; case 5: ROP("fst %%st(5)"); break; case 6: ROP("fst %%st(6)"); break; default: ROP("fst %%st(7)"); }
            } else if (op < 13 && depth >= 1) {
                switch (i) { case 0: ROP("fstp %%st(0)"); break; case 1: ROP("fstp %%st(1)"); break; case 2: ROP("fstp %%st(2)"); break; case 3: ROP("fstp %%st(3)"); break;
                             case 4: ROP("fstp %%st(4)"); break; case 5: ROP("fstp %%st(5)"); break; case 6: ROP("fstp %%st(6)"); break; default: ROP("fstp %%st(7)"); }
                depth--;
            } else if (op < 16 && depth >= 1) {
                switch (i) { case 0: ROP("fxch %%st(0)"); break; case 1: ROP("fxch %%st(1)"); break; case 2: ROP("fxch %%st(2)"); break; case 3: ROP("fxch %%st(3)"); break;
                             case 4: ROP("fxch %%st(4)"); break; case 5: ROP("fxch %%st(5)"); break; case 6: ROP("fxch %%st(6)"); break; default: ROP("fxch %%st(7)"); }
            } else if (op < 22 && depth >= 2) {
                int j = 1 + (r >> 8) % (depth - 1);
                switch ((r >> 30) % 6) {
                case 0: switch (j) { case 1: ROP("fadd %%st(1), %%st"); break; case 2: ROP("fadd %%st(2), %%st"); break; case 3: ROP("fadd %%st(3), %%st"); break; case 4: ROP("fadd %%st(4), %%st"); break; case 5: ROP("fadd %%st(5), %%st"); break; case 6: ROP("fadd %%st(6), %%st"); break; default: ROP("fadd %%st(7), %%st"); } break;
                case 1: switch (j) { case 1: ROP("fmul %%st(1), %%st"); break; case 2: ROP("fmul %%st(2), %%st"); break; case 3: ROP("fmul %%st(3), %%st"); break; case 4: ROP("fmul %%st(4), %%st"); break; case 5: ROP("fmul %%st(5), %%st"); break; case 6: ROP("fmul %%st(6), %%st"); break; default: ROP("fmul %%st(7), %%st"); } break;
                case 2: switch (j) { case 1: ROP("fsub %%st, %%st(1)"); break; case 2: ROP("fsub %%st, %%st(2)"); break; case 3: ROP("fsub %%st, %%st(3)"); break; case 4: ROP("fsub %%st, %%st(4)"); break; case 5: ROP("fsub %%st, %%st(5)"); break; case 6: ROP("fsub %%st, %%st(6)"); break; default: ROP("fsub %%st, %%st(7)"); } break;
                case 3: switch (j) { case 1: ROP("fdivr %%st, %%st(1)"); break; case 2: ROP("fdivr %%st, %%st(2)"); break; case 3: ROP("fdivr %%st, %%st(3)"); break; case 4: ROP("fdivr %%st, %%st(4)"); break; case 5: ROP("fdivr %%st, %%st(5)"); break; case 6: ROP("fdivr %%st, %%st(6)"); break; default: ROP("fdivr %%st, %%st(7)"); } break;
                case 4: switch (j) { case 1: ROP("faddp %%st, %%st(1)"); break; case 2: ROP("faddp %%st, %%st(2)"); break; case 3: ROP("faddp %%st, %%st(3)"); break; case 4: ROP("faddp %%st, %%st(4)"); break; case 5: ROP("faddp %%st, %%st(5)"); break; case 6: ROP("faddp %%st, %%st(6)"); break; default: ROP("faddp %%st, %%st(7)"); } depth--; break;
                default: switch (j) { case 1: ROP("fsubrp %%st, %%st(1)"); break; case 2: ROP("fsubrp %%st, %%st(2)"); break; case 3: ROP("fsubrp %%st, %%st(3)"); break; case 4: ROP("fsubrp %%st, %%st(4)"); break; case 5: ROP("fsubrp %%st, %%st(5)"); break; case 6: ROP("fsubrp %%st, %%st(6)"); break; default: ROP("fsubrp %%st, %%st(7)"); } depth--; break;
                }
            } else if (op < 24 && depth >= 1) {
                switch ((r >> 30) % 6) { case 0: ROP("fchs"); break; case 1: ROP("fabs"); break; case 2: ROP("fabs"); break; case 3: ROP("fchs"); break; case 4: ROP("ftst"); break; default: ROP("fxam"); }
            } else if (op < 26 && depth >= 2) {
                if ((r >> 30) & 1) { ROP("fcompp"); depth -= 2; } else { ROP("fucomp %%st(1)"); depth--; }
            } else if (op < 28 && depth >= 1) {
                ROP("fcom %%st(0)");
            } else if (op == 28 && depth >= 1) {
                static const uint16_t cws[4] = {0x037f, 0x077f, 0x0b7f, 0x0f7f};
                uint16_t cw = cws[(r >> 8) & 3] & ((r >> 12) & 1 ? 0xfcff : 0xffff);
                asm volatile("fldcw %0" : : "m"(cw)); TR("fldcw"); 
            }
        }
        dump(trace);
    }
    return 0;
}
