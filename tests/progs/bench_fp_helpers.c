// flags: -msse2
// Balanced x87 conversions/comparison, status reads, SSE conversion and x87->MMX transitions.
// Fixed results allow the native i386 oracle to verify this is not just a timing loop.
#include <stdint.h>
#include <stdio.h>
int main(void) {
    const int64_t input = 1234567890123LL;
    const float f = 1.5f;
    int64_t output = 0;
    uint64_t mm = 0;
    unsigned sum = 0;
    for (unsigned i = 0; i < 1000000; ++i) {
        unsigned status, converted;
        __asm__ volatile (
            "fildll %[in]\n\t"
            "fistpll %[out]\n\t"
            "fld1\n\tfld1\n\tfcomip %%st(1), %%st\n\tfstp %%st(0)\n\t"
            "fnstsw %%ax\n\t"
            "movss %[f], %%xmm0\n\tcvtss2si %%xmm0, %[cv]\n\t"
            "movq %[in], %%mm0\n\tpxor %%mm0, %%mm0\n\t"
            "movq %%mm0, %[mm]\n\temms"
            : "=a" (status), [cv] "=r" (converted), [out] "=m" (output), [mm] "=m" (mm)
            : [in] "m" (input), [f] "m" (f)
            : "xmm0", "mm0", "st", "cc", "memory");
        sum += converted + (status & 0xffff);
    }
    printf("%lld %llu %u\n", (long long) output, (unsigned long long) mm, sum);
    return output != input || mm != 0 || sum != 2000000;
}
