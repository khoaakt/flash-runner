// flags: -msse2 -mfpmath=sse
// CMPPS/SS/PD/SD predicates 0..7, COMISS/UCOMISS/COMISD/UCOMISD (EFLAGS + MXCSR.IE/DE), RCP/RSQRT sanity.
#include "sse_fp_common.h"

#define CMPS(n) \
    static __attribute__((noinline)) __m128 cmpps_##n(__m128 a, __m128 b) { asm volatile("cmpps $" #n ", %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128 cmpss_##n(__m128 a, __m128 b) { asm volatile("cmpss $" #n ", %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128 cmppsm_##n(__m128 a, const __m128 *p) { asm volatile("cmpps $" #n ", %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128 cmpssm_##n(__m128 a, const float *p) { asm volatile("cmpss $" #n ", %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128d cmppd_##n(__m128d a, __m128d b) { asm volatile("cmppd $" #n ", %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128d cmpsd_##n(__m128d a, __m128d b) { asm volatile("cmpsd $" #n ", %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128d cmppdm_##n(__m128d a, const __m128d *p) { asm volatile("cmppd $" #n ", %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128d cmpsdm_##n(__m128d a, const double *p) { asm volatile("cmpsd $" #n ", %1, %0" : "+x"(a) : "m"(*p)); return a; }
CMPS(0) CMPS(1) CMPS(2) CMPS(3) CMPS(4) CMPS(5) CMPS(6) CMPS(7)

static __attribute__((noinline)) u32 comiss(__m128 a, __m128 b) { u32 f; asm volatile("comiss %2, %1\n\tpushfl\n\tpopl %0" : "=r"(f) : "x"(a), "x"(b) : "cc"); return f & 0x8d5; }
static __attribute__((noinline)) u32 ucomiss(__m128 a, __m128 b) { u32 f; asm volatile("ucomiss %2, %1\n\tpushfl\n\tpopl %0" : "=r"(f) : "x"(a), "x"(b) : "cc"); return f & 0x8d5; }
static __attribute__((noinline)) u32 comisd(__m128d a, __m128d b) { u32 f; asm volatile("comisd %2, %1\n\tpushfl\n\tpopl %0" : "=r"(f) : "x"(a), "x"(b) : "cc"); return f & 0x8d5; }
static __attribute__((noinline)) u32 ucomisd(__m128d a, __m128d b) { u32 f; asm volatile("ucomisd %2, %1\n\tpushfl\n\tpopl %0" : "=r"(f) : "x"(a), "x"(b) : "cc"); return f & 0x8d5; }
static __attribute__((noinline)) u32 comissm(__m128 a, const float *p) { u32 f; asm volatile("comiss %2, %1\n\tpushfl\n\tpopl %0" : "=r"(f) : "x"(a), "m"(*p) : "cc"); return f & 0x8d5; }
static __attribute__((noinline)) u32 ucomisdm(__m128d a, const double *p) { u32 f; asm volatile("ucomisd %2, %1\n\tpushfl\n\tpopl %0" : "=r"(f) : "x"(a), "m"(*p) : "cc"); return f & 0x8d5; }

static void mixf(void) { hmix32(getcsr() & 0x3f); }

int main(int argc, char **argv) {
    SETUP(argc);
    init_fp_pools();
    V a, b, r;
    for (int cfg = 0; cfg < 3; cfg++) {  // plain, DAZ (denormals compare as zero), FTZ (no effect)
        u32 cv = cfg == 0 ? 0x1f80 : cfg == 1 ? 0x1fc0 : 0x9f80;
        char nm[40];
#define DOCMP(n) \
        for (int i = 0; i < NF32; i++) \
            for (int j = 0; j < NF32; j++) { \
                for (int k = 0; k < 4; k++) { a.u[k] = pool32[(i + 5 * k) % NF32]; b.u[k] = pool32[(j + 11 * k) % NF32]; } \
                setcsr(cv); r.f = cmpps_##n(a.f, b.f); mixf(); hmix128(&r); \
                setcsr(cv); r.f = cmpss_##n(a.f, b.f); mixf(); hmix128(&r); \
                if ((i + j) % 3 == 0) { setcsr(cv); r.f = cmppsm_##n(a.f, &b.f); mixf(); hmix128(&r); setcsr(cv); r.f = cmpssm_##n(a.f, (const float *)&b.f); mixf(); hmix128(&r); } \
            } \
        sprintf(nm, "cmp%sps/ss pred%d cfg%d", "", n, cfg); report(nm); \
        for (int i = 0; i < NF64; i++) \
            for (int j = 0; j < NF64; j++) { \
                for (int k = 0; k < 2; k++) { a.q[k] = pool64[(i + 5 * k) % NF64]; b.q[k] = pool64[(j + 11 * k) % NF64]; } \
                setcsr(cv); r.d = cmppd_##n(a.d, b.d); mixf(); hmix128(&r); \
                setcsr(cv); r.d = cmpsd_##n(a.d, b.d); mixf(); hmix128(&r); \
                if ((i + j) % 3 == 0) { setcsr(cv); r.d = cmppdm_##n(a.d, &b.d); mixf(); hmix128(&r); setcsr(cv); r.d = cmpsdm_##n(a.d, (const double *)&b.d); mixf(); hmix128(&r); } \
            } \
        sprintf(nm, "cmp%spd/sd pred%d cfg%d", "", n, cfg); report(nm);
        DOCMP(0) DOCMP(1) DOCMP(2) DOCMP(3) DOCMP(4) DOCMP(5) DOCMP(6) DOCMP(7)

        for (int i = 0; i < NF32; i++)
            for (int j = 0; j < NF32; j++) {
                a.u[0] = pool32[i]; b.u[0] = pool32[j];
                setcsr(cv); hmix32(comiss(a.f, b.f)); mixf();
                setcsr(cv); hmix32(ucomiss(a.f, b.f)); mixf();
                setcsr(cv); hmix32(comissm(a.f, (const float *)&b.u[0])); mixf();
            }
        sprintf(nm, "comiss/ucomiss cfg%d", cfg); report(nm);
        for (int i = 0; i < NF64; i++)
            for (int j = 0; j < NF64; j++) {
                a.q[0] = pool64[i]; b.q[0] = pool64[j];
                setcsr(cv); hmix32(comisd(a.d, b.d)); mixf();
                setcsr(cv); hmix32(ucomisd(a.d, b.d)); mixf();
                setcsr(cv); hmix32(ucomisdm(a.d, (const double *)&b.q[0])); mixf();
            }
        sprintf(nm, "comisd/ucomisd cfg%d", cfg); report(nm);
    }
    setcsr(0x1f80);

    // RCPPS / RSQRTPS: only the architectural accuracy (|rel err| <= 1.5*2^-12) is portable.
    {
        int bad = 0, n = 0;
        for (int i = 0; i < 4000; i++) {
            u32 bits = (u32)rnd64();
            bits = (bits & 0x807fffff) | ((((bits >> 24) % 200) + 20) << 23);  // normal, moderate exponent
            float x = *(float *)&bits;
            float ax = x < 0 ? -x : x;
            V in, o;
            in.f = _mm_set1_ps(ax);
            o.f = _mm_rcp_ps(in.f);
            double rr = (double)o.f[0] * (double)ax;
            if (rr < 1 - 1.5 / 4096 || rr > 1 + 1.5 / 4096) bad++;
            o.f = _mm_rsqrt_ps(in.f);
            double q = (double)o.f[0] * __builtin_sqrt((double)ax);
            if (q < 1 - 1.5 / 4096 || q > 1 + 1.5 / 4096) bad++;
            n += 2;
        }
        printf("rcp/rsqrt accuracy: %d checks, %d out of tolerance\n", n, bad);
        // special values (identical on every implementation)
        static const u32 sp[] = {0x00000000, 0x80000000, 0x7f800000, 0xff800000, 0x7fc00000, 0xffc00000, 0xbf800000, 0xc0800000, 0x3f800000, 0x40800000, 0x3e800000};
        for (unsigned i = 0; i < sizeof sp / sizeof sp[0]; i++) {
            V in, o;
            in.u[0] = sp[i]; in.u[1] = sp[(i + 1) % 11]; in.u[2] = sp[(i + 2) % 11]; in.u[3] = sp[(i + 3) % 11];
            o.f = _mm_rcp_ps(in.f);
            // zero/inf/negative handling is architecturally defined; the rest is approximate on real CPUs
            for (int k = 0; k < 4; k++) {
                u32 s = in.u[k];
                if (s == 0 || s == 0x80000000 || s == 0x7f800000 || s == 0xff800000)
                    printf("rcp(%08x) = %08x\n", s, o.u[k]);
            }
            o.f = _mm_rsqrt_ps(in.f);
            for (int k = 0; k < 4; k++) {
                u32 s = in.u[k];
                if (s == 0 || s == 0x80000000 || s == 0x7f800000 || s == 0xff800000 || s == 0xbf800000 || s == 0xc0800000)
                    printf("rsqrt(%08x) = %08x\n", s, o.u[k]);
            }
        }
    }
    return 0;
}
