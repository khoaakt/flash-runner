// Legacy BCD adjusts (daa/das/aaa/aas/aam/aad): exhaustive sweeps, FNV-hashed, plus explicit SDM
// vectors. gcc never emits these; hand-written asm and DOS-era code do. Only flags the SDM defines
// are captured (lahf windows are masked per instruction), so undefined-flag differences between
// engines cannot cause false mismatches.
#include <stdio.h>

static unsigned h = 2166136261u;
static void feed(unsigned v) { h = (h ^ v) * 16777619u; }

// daa/das: input EAX = AL | flags<<8 (sahf reads AH), output = AL' | lahf<<8.
// SF/ZF/AF/PF/CF are all defined for daa/das, so the whole word is compared.
static unsigned run_daa(unsigned in) { unsigned r; __asm__ ("sahf\n\tdaa\n\tlahf" : "=a"(r) : "a"(in) : "cc"); return r; }
static unsigned run_das(unsigned in) { unsigned r; __asm__ ("sahf\n\tdas\n\tlahf" : "=a"(r) : "a"(in) : "cc"); return r; }

// aaa/aas/aam/aad: set the flags from %1 with sahf (via a scratch AH), load AX = %2, run, then
// pack AL | AH<<8 | lahf<<16. The caller masks lahf down to the flags the SDM defines:
// CF|AF (0x11) for aaa/aas, SF|ZF|PF (0xC4) for aam/aad.
#define BCD3(name, ins)                                                                                    \
    static unsigned name(unsigned fl, unsigned ax) {                                                       \
        unsigned r;                                                                                        \
        __asm__ ("movl %1, %%eax\n\tshll $8, %%eax\n\torl %1, %%eax\n\tsahf\n\t"                           \
                 "movl %2, %%eax\n\t" ins "\n\t"                                                           \
                 "movl %%eax, %%edx\n\tlahf\n\tmovzbl %%ah, %%eax\n\tshll $16, %%eax\n\t"                  \
                 "andl $0xffff, %%edx\n\torl %%edx, %%eax"                                                 \
                 : "=a"(r) : "r"(fl), "r"(ax) : "cc", "edx");                                              \
        return r;                                                                                          \
    }
BCD3(run_aaa, "aaa")
BCD3(run_aas, "aas")
#define BCD3I(name, ins, base) BCD3(name, ins " $" base)
BCD3I(run_aam2, "aam", "2")
BCD3I(run_aam7, "aam", "7")
BCD3I(run_aam10, "aam", "10")
BCD3I(run_aam16, "aam", "16")
BCD3I(run_aam36, "aam", "36")
BCD3I(run_aam100, "aam", "100")
BCD3I(run_aam64, "aam", "64") // bases >= 0x80 are excluded: qemu sign-extends the imm8 (its AAM returns al unchanged for al=base=255) - a qemu bug, pinned by the Rust unit test instead
BCD3I(run_aad2, "aad", "2")
BCD3I(run_aad7, "aad", "7")
BCD3I(run_aad10, "aad", "10")
BCD3I(run_aad16, "aad", "16")
BCD3I(run_aad36, "aad", "36")
BCD3I(run_aad100, "aad", "100")
BCD3I(run_aad64, "aad", "64")

static const unsigned FL4[4] = { 2, 3, 0x12, 0x13 };        // -, CF, AF, CF|AF (bit1 = reserved)
static const unsigned AH4[4] = { 0, 1, 2, 0xff };
static int verbose;

int main(int argc, char **argv) {
    setbuf(stdout, NULL);
    verbose = argc > 1;
    for (int al = 0; al < 256; al++)
        for (int f = 0; f < 4; f++) {
            unsigned in = al | (FL4[f] << 8);
            unsigned a = run_daa(in), s = run_das(in);
            feed(a);
            feed(s);
            if (verbose) printf("D %02x %x %04x %04x\n", al, f, a, s);
        }
    for (int al = 0; al < 256; al++)
        for (int hi = 0; hi < 4; hi++)
            for (int f = 0; f < 4; f++) {
                unsigned ax = al | (AH4[hi] << 8);
                unsigned r[16];
                r[0] = run_aaa(FL4[f], ax) & 0x0011ffffu;
                r[1] = run_aas(FL4[f], ax) & 0x0011ffffu;
                r[2] = run_aam2(FL4[f], ax) & 0x00c4ffffu;
                r[3] = run_aam7(FL4[f], ax) & 0x00c4ffffu;
                r[4] = run_aam10(FL4[f], ax) & 0x00c4ffffu;
                r[5] = run_aam16(FL4[f], ax) & 0x00c4ffffu;
                r[6] = run_aam36(FL4[f], ax) & 0x00c4ffffu;
                r[7] = run_aam100(FL4[f], ax) & 0x00c4ffffu;
                r[8] = run_aam64(FL4[f], ax) & 0x00c4ffffu;
                r[9] = run_aad2(FL4[f], ax) & 0x00c4ffffu;
                r[10] = run_aad7(FL4[f], ax) & 0x00c4ffffu;
                r[11] = run_aad10(FL4[f], ax) & 0x00c4ffffu;
                r[12] = run_aad16(FL4[f], ax) & 0x00c4ffffu;
                r[13] = run_aad36(FL4[f], ax) & 0x00c4ffffu;
                r[14] = run_aad100(FL4[f], ax) & 0x00c4ffffu;
                r[15] = run_aad64(FL4[f], ax) & 0x00c4ffffu;
                for (int k = 0; k < 16; k++) feed(r[k]);
                if (verbose) {
                    printf("A %04x %x", ax, f);
                    for (int k = 0; k < 16; k++) printf(" %06x", r[k]);
                    printf("\n");
                }
            }
    printf("sweep %08x\n", h);
    // explicit SDM vectors (daa: 45H+38H=7DH -> 83H; aaa: AX=020CH -> 0302H)
    printf("daa 7d=%04x daa 9a=%04x das de=%04x aaa 020c=%06x aas 050c=%06x aam10 37=%06x aad10 0507=%06x\n",
           run_daa(0x7d | 2 << 8) & 0xffff, run_daa(0x9a | 2 << 8) & 0xffff, run_das(0xde | 2 << 8) & 0xffff,
           run_aaa(2, 0x020c) & 0x0011ffff, run_aas(2, 0x050c) & 0x0011ffff,
           run_aam10(2, 0x37) & 0x00c4ffff, run_aad10(2, 0x0507) & 0x00c4ffff);
    return 0;
}
