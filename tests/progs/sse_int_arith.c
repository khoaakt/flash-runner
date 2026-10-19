// flags: -msse2
// SSE2 128-bit integer SIMD: arithmetic, compare, logic, min/max, pack/unpack, in register and memory forms.
#include "sse_common.h"

#define BIN_OPS(X) \
    X(paddb) X(paddw) X(paddd) X(paddq) X(psubb) X(psubw) X(psubd) X(psubq) \
    X(paddsb) X(paddsw) X(paddusb) X(paddusw) X(psubsb) X(psubsw) X(psubusb) X(psubusw) \
    X(pmullw) X(pmulhw) X(pmulhuw) X(pmuludq) X(pmaddwd) X(pavgb) X(pavgw) \
    X(pminub) X(pmaxub) X(pminsw) X(pmaxsw) X(psadbw) \
    X(pcmpeqb) X(pcmpeqw) X(pcmpeqd) X(pcmpgtb) X(pcmpgtw) X(pcmpgtd) \
    X(pand) X(pandn) X(por) X(pxor) \
    X(packsswb) X(packssdw) X(packuswb) \
    X(punpcklbw) X(punpcklwd) X(punpckldq) X(punpcklqdq) \
    X(punpckhbw) X(punpckhwd) X(punpckhdq) X(punpckhqdq) \
    X(andps) X(andnps) X(orps) X(xorps) X(andpd) X(andnpd) X(orpd) X(xorpd) \
    X(unpcklps) X(unpckhps) X(unpcklpd) X(unpckhpd)

#define DEF(op) \
    static __attribute__((noinline)) __m128i op##_rr(__m128i a, __m128i b) { asm(#op " %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128i op##_rm(__m128i a, const __m128i *p) { asm(#op " %1, %0" : "+x"(a) : "m"(*p)); return a; } \
    static __attribute__((noinline)) __m128i op##_same(__m128i a) { asm(#op " %0, %0" : "+x"(a)); return a; }
BIN_OPS(DEF)

#define RUN(op) \
    for (int i = 0; i < NPOOL; i++) \
        for (int j = 0; j < NPOOL; j++) { \
            __m128i r = op##_rr(pool[i], pool[j]); \
            __m128i r2 = op##_rm(pool[i], &pool[j]); \
            if (memcmp(&r, &r2, 16)) printf("MISMATCH rr/rm %s %d %d\n", #op, i, j); \
            hmix128(&r); \
        } \
    for (int i = 0; i < NPOOL; i++) { __m128i r = op##_same(pool[i]); hmix128(&r); } \
    report(#op);

// Shifts by register/memory count and by immediate.
#define SHIFTS(X) X(psllw) X(pslld) X(psllq) X(psrlw) X(psrld) X(psrlq) X(psraw) X(psrad)
#define DEFS(op) \
    static __attribute__((noinline)) __m128i op##_rr(__m128i a, __m128i c) { asm(#op " %1, %0" : "+x"(a) : "x"(c)); return a; } \
    static __attribute__((noinline)) __m128i op##_rm(__m128i a, const __m128i *p) { asm(#op " %1, %0" : "+x"(a) : "m"(*p)); return a; }
SHIFTS(DEFS)

#define IMMSH(op, n) \
    static __attribute__((noinline)) __m128i op##_i##n(__m128i a) { asm(#op " $" #n ", %0" : "+x"(a)); return a; }
#define IMMALL(op) IMMSH(op, 0) IMMSH(op, 1) IMMSH(op, 7) IMMSH(op, 8) IMMSH(op, 15) IMMSH(op, 16) IMMSH(op, 31) IMMSH(op, 32) IMMSH(op, 33) IMMSH(op, 63) IMMSH(op, 64) IMMSH(op, 65) IMMSH(op, 255)
SHIFTS(IMMALL)

#define RUNSH(op) \
    { \
        static const u64 counts[] = {0, 1, 2, 3, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 255, 256, 0x100000000ULL, 0x100000001ULL, ~0ULL}; \
        for (int i = 0; i < NPOOL; i++) { \
            for (unsigned k = 0; k < sizeof counts / sizeof counts[0]; k++) { \
                __m128i c = _mm_set_epi64x(0x1122334455667788LL, counts[k]); \
                __m128i r = op##_rr(pool[i], c); \
                __m128i r2 = op##_rm(pool[i], &c); \
                if (memcmp(&r, &r2, 16)) printf("MISMATCH shift rr/rm %s\n", #op); \
                hmix128(&r); \
            } \
            __m128i t; \
            t = op##_i0(pool[i]); hmix128(&t); t = op##_i1(pool[i]); hmix128(&t); t = op##_i7(pool[i]); hmix128(&t); \
            t = op##_i8(pool[i]); hmix128(&t); t = op##_i15(pool[i]); hmix128(&t); t = op##_i16(pool[i]); hmix128(&t); \
            t = op##_i31(pool[i]); hmix128(&t); t = op##_i32(pool[i]); hmix128(&t); t = op##_i33(pool[i]); hmix128(&t); \
            t = op##_i63(pool[i]); hmix128(&t); t = op##_i64(pool[i]); hmix128(&t); t = op##_i65(pool[i]); hmix128(&t); \
            t = op##_i255(pool[i]); hmix128(&t); \
        } \
        report(#op); \
    }

// Byte shifts and shuffles with every interesting immediate.
#define BYTESH(n) \
    static __attribute__((noinline)) __m128i pslldq_##n(__m128i a) { asm("pslldq $" #n ", %0" : "+x"(a)); return a; } \
    static __attribute__((noinline)) __m128i psrldq_##n(__m128i a) { asm("psrldq $" #n ", %0" : "+x"(a)); return a; }
BYTESH(0) BYTESH(1) BYTESH(2) BYTESH(3) BYTESH(4) BYTESH(7) BYTESH(8) BYTESH(9) BYTESH(12) BYTESH(15) BYTESH(16) BYTESH(17) BYTESH(255)

#define SHUF(n) \
    static __attribute__((noinline)) __m128i pshufd_##n(const __m128i *p) { __m128i r; asm("pshufd $" #n ", %1, %0" : "=x"(r) : "m"(*p)); return r; } \
    static __attribute__((noinline)) __m128i pshufd_r##n(__m128i a) { __m128i r; asm("pshufd $" #n ", %1, %0" : "=x"(r) : "x"(a)); return r; } \
    static __attribute__((noinline)) __m128i pshuflw_##n(const __m128i *p) { __m128i r; asm("pshuflw $" #n ", %1, %0" : "=x"(r) : "m"(*p)); return r; } \
    static __attribute__((noinline)) __m128i pshufhw_##n(const __m128i *p) { __m128i r; asm("pshufhw $" #n ", %1, %0" : "=x"(r) : "m"(*p)); return r; } \
    static __attribute__((noinline)) __m128i shufps_##n(__m128i a, __m128i b) { asm("shufps $" #n ", %1, %0" : "+x"(a) : "x"(b)); return a; } \
    static __attribute__((noinline)) __m128i shufpd_##n(__m128i a, __m128i b) { asm("shufpd $" #n ", %1, %0" : "+x"(a) : "x"(b)); return a; }
SHUF(0) SHUF(27) SHUF(228) SHUF(255) SHUF(57) SHUF(147) SHUF(78) SHUF(177) SHUF(1) SHUF(2) SHUF(3) SHUF(85) SHUF(170) SHUF(100)

int main(int argc, char **argv) {
    SETUP(argc);
    init_pool();
    BIN_OPS(RUN)
    SHIFTS(RUNSH)
    for (int i = 0; i < NPOOL; i++) {
        __m128i t;
#define BS(n) t = pslldq_##n(pool[i]); hmix128(&t); t = psrldq_##n(pool[i]); hmix128(&t);
        BS(0) BS(1) BS(2) BS(3) BS(4) BS(7) BS(8) BS(9) BS(12) BS(15) BS(16) BS(17) BS(255)
    }
    report("pslldq/psrldq");
    for (int i = 0; i < NPOOL; i++) {
        __m128i t;
#define SH(n) t = pshufd_##n(&pool[i]); hmix128(&t); t = pshufd_r##n(pool[i]); hmix128(&t); t = pshuflw_##n(&pool[i]); hmix128(&t); \
              t = pshufhw_##n(&pool[i]); hmix128(&t); t = shufps_##n(pool[i], pool[(i * 7 + 3) % NPOOL]); hmix128(&t); \
              t = shufpd_##n(pool[i], pool[(i * 7 + 3) % NPOOL]); hmix128(&t);
        SH(0) SH(27) SH(228) SH(255) SH(57) SH(147) SH(78) SH(177) SH(1) SH(2) SH(3) SH(85) SH(170) SH(100)
    }
    report("shuffles");
    return 0;
}
