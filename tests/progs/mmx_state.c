// flags: -msse2 -mmmx
// MMX/x87 aliasing: MMX instructions force TOP=0 and all tags valid; EMMS empties the tag word.
#include <stdio.h>
#include <stdint.h>
#include <string.h>

struct env { uint32_t cw, sw, tw, ip, cs, dp, ds; };
static void show(const char *what) {
    struct env e;
    asm volatile("fnstenv %0\n\tfldenv %0" : "=m"(e));
    printf("%-26s top=%u tag=%04x\n", what, (e.sw >> 11) & 7, e.tw & 0xffff);
}

static uint64_t in[2] = {0x0123456789abcdefULL, 0xfedcba9876543210ULL};
static uint64_t out[2];

int main(void) {
    asm volatile("fninit");
    show("after fninit");
    asm volatile("fld1\n\tfld1\n\tfld1");
    show("after 3x fld1");
    asm volatile("movq %0, %%mm0" ::"m"(in[0]) : "mm0");
    show("after movq mm0, m64");
    asm volatile("emms");
    show("after emms");
    asm volatile("fld1\n\tfld1\n\tfld1\n\tfld1\n\tfld1");
    show("after 5x fld1");
    asm volatile("paddb %%mm0, %%mm1" ::: "mm0", "mm1");
    show("after paddb mm1, mm0");
    asm volatile("movq %%mm1, %0" : "=m"(out[0])::"mm1");
    show("after movq m64, mm1");
    asm volatile("emms");
    show("after emms again");
    asm volatile("fld1\n\tfld1");
    asm volatile("movd %0, %%mm2" ::"r"(5) : "mm2");
    show("after movd mm2, r32");
    asm volatile("emms");
    asm volatile("fld1");
    asm volatile("pextrw $1, %%mm2, %0" : "=r"(*(int *)&out[1]) : : "mm2");
    show("after pextrw (reads mm)");
    asm volatile("emms");
    show("final emms");
    asm volatile("fninit");
    return 0;
}
