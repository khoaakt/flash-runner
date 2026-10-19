// JIT stress: shl shr sar rol ror rcl rcr shld shrd on 8/16/32-bit registers and memory, every count 0..40
// (cl) and several immediates. Only architecturally defined result/EFLAGS bits go into the checksums
// (CF for counts below the operand size, OF only for count 1, AF never).
#include <stdio.h>
#include <stdint.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0x9e3779b9u;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static const u32 edge[] = {0, 1, 2, 0x7f, 0x80, 0x81, 0xff, 0x7fff, 0x8000, 0x8001, 0xffff, 0x7fffffffu, 0x80000000u, 0x80000001u, 0xffffffffu, 0x55555555u, 0xaaaaaaaau, 0x12345678u};
static u32 val(void) { u32 r = rnd(); return (r & 3) ? edge[(r >> 2) % (sizeof edge / sizeof edge[0])] : rnd(); }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }
#define CF 0x1u
#define PF 0x4u
#define AF 0x10u
#define ZF 0x40u
#define SF 0x80u
#define OF 0x800u
#define ALL (CF | PF | AF | ZF | SF | OF)

// flag masks (bits whose value is architecturally defined) for count `cnt` and operand width `bits`
static u32 m_shift(u32 cnt, u32 bits) {
    u32 c = cnt & 31;
    if (c == 0) return ALL;                       // nothing changes: the preset flags survive
    u32 m = SF | ZF | PF;
    if (c < bits) m |= CF;
    if (c == 1) m |= OF;
    return m;
}
static u32 m_rot(u32 cnt, u32 bits) {
    (void)bits;
    u32 c = cnt & 31;
    if (c == 0) return ALL;
    u32 m = SF | ZF | AF | PF | CF;               // SF/ZF/AF/PF are not modified: preset values survive
    if (c == 1) m |= OF;
    return m;
}
static u32 m_dshift(u32 cnt, u32 bits) {
    u32 c = cnt & 31;
    if (c == 0) return ALL;
    u32 m = SF | ZF | PF;
    if (c <= bits) m |= CF;
    if (c == 1) m |= OF;
    return m;
}

#define N_CNT 41
#define REPS 24

#define SH_REG(NAME, INS, SUF, T, CON, BITS, MASK)                                                                              \
    static u32 s_##NAME##SUF##_r(void) {                                                                                         \
        u32 h = 2166136261u;                                                                                                     \
        for (u32 cnt = 0; cnt < N_CNT; cnt++)                                                                                    \
            for (int k = 0; k < REPS; k++) {                                                                                     \
                T a = (T)val(); u32 c = rnd() & 1, fl; u8 n = (u8)cnt;                                                           \
                __asm__ volatile("addl $-1, %2\n\t" INS " %%cl, %0\n\tpushf\n\tpopl %1" : "+" CON(a), "=r"(fl), "+r"(c) : "c"(n) : "cc"); \
                h = mix(mix(h, (u32)a), fl & MASK(cnt, BITS));                                                                   \
            }                                                                                                                    \
        return h;                                                                                                                \
    }                                                                                                                            \
    static u32 s_##NAME##SUF##_m(void) {                                                                                         \
        u32 h = 2166136261u;                                                                                                     \
        static volatile T mem;                                                                                                   \
        for (u32 cnt = 0; cnt < N_CNT; cnt++)                                                                                    \
            for (int k = 0; k < REPS; k++) {                                                                                     \
                u32 c = rnd() & 1, fl; u8 n = (u8)cnt; mem = (T)val();                                                           \
                __asm__ volatile("addl $-1, %2\n\t" INS " %%cl, %0\n\tpushf\n\tpopl %1" : "+m"(mem), "=r"(fl), "+r"(c) : "c"(n) : "cc"); \
                h = mix(mix(h, (u32)mem), fl & MASK(cnt, BITS));                                                                 \
            }                                                                                                                    \
        return h;                                                                                                                \
    }

#define SH_ALL(NAME, INS, MASK)                                  \
    SH_REG(NAME, INS "b", b, u8, "q", 8, MASK)                   \
    SH_REG(NAME, INS "w", w, u16, "r", 16, MASK)                 \
    SH_REG(NAME, INS "l", l, u32, "r", 32, MASK)
SH_ALL(shl, "shl", m_shift)
SH_ALL(shr, "shr", m_shift)
SH_ALL(sar, "sar", m_shift)
SH_ALL(rol, "rol", m_rot)
SH_ALL(ror, "ror", m_rot)
SH_ALL(rcl, "rcl", m_rot)
SH_ALL(rcr, "rcr", m_rot)

// shld / shrd (16-bit forms only for counts <= 16: larger counts are undefined)
#define DSH(NAME, INS, SUF, T, BITS, MAXC)                                                                                       \
    static u32 d_##NAME##SUF(void) {                                                                                             \
        u32 h = 2166136261u;                                                                                                     \
        for (u32 cnt = 0; cnt <= MAXC; cnt++)                                                                                    \
            for (int k = 0; k < REPS; k++) {                                                                                     \
                T a = (T)val(), b = (T)val(); u32 c = rnd() & 1, fl; u8 n = (u8)cnt;                                             \
                __asm__ volatile("addl $-1, %2\n\t" INS " %%cl, %3, %0\n\tpushf\n\tpopl %1" : "+r"(a), "=r"(fl), "+r"(c) : "r"(b), "c"(n) : "cc"); \
                h = mix(mix(h, (u32)a), fl & m_dshift(cnt, BITS));                                                               \
            }                                                                                                                    \
        return h;                                                                                                                \
    }
DSH(shld, "shld", w, u16, 16, 16)
DSH(shld, "shld", l, u32, 32, 40)
DSH(shrd, "shrd", w, u16, 16, 16)
DSH(shrd, "shrd", l, u32, 32, 40)

// immediate counts
#define IMM(NAME, INS, SUF, T, CON, BITS, MASK, K)                                                                            \
    static u32 i_##NAME##SUF##K(void) {                                                                                       \
        u32 h = 2166136261u;                                                                                                  \
        for (int k = 0; k < 400; k++) {                                                                                       \
            T a = (T)val(); u32 c = rnd() & 1, fl;                                                                            \
            __asm__ volatile("addl $-1, %2\n\t" INS " $" #K ", %0\n\tpushf\n\tpopl %1" : "+" CON(a), "=r"(fl), "+r"(c) : : "cc"); \
            h = mix(mix(h, (u32)a), fl & MASK(K, BITS));                                                                      \
        }                                                                                                                     \
        return h;                                                                                                             \
    }
#define IMM_OP(NAME, INS, MASK)                                                                                               \
    IMM(NAME, INS "b", b, u8, "q", 8, MASK, 1) IMM(NAME, INS "b", b, u8, "q", 8, MASK, 3) IMM(NAME, INS "b", b, u8, "q", 8, MASK, 9)    \
    IMM(NAME, INS "w", w, u16, "r", 16, MASK, 1) IMM(NAME, INS "w", w, u16, "r", 16, MASK, 5) IMM(NAME, INS "w", w, u16, "r", 16, MASK, 17) \
    IMM(NAME, INS "l", l, u32, "r", 32, MASK, 1) IMM(NAME, INS "l", l, u32, "r", 32, MASK, 7) IMM(NAME, INS "l", l, u32, "r", 32, MASK, 31)
IMM_OP(shl, "shl", m_shift)
IMM_OP(shr, "shr", m_shift)
IMM_OP(sar, "sar", m_shift)
IMM_OP(rol, "rol", m_rot)
IMM_OP(ror, "ror", m_rot)

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define E3(p, n) {#p #n "b_r", p##n##b_r}, {#p #n "b_m", p##n##b_m}, {#p #n "w_r", p##n##w_r}, {#p #n "w_m", p##n##w_m}, {#p #n "l_r", p##n##l_r}, {#p #n "l_m", p##n##l_m}
#define EI(n) {"i_" #n "b1", i_##n##b1}, {"i_" #n "b3", i_##n##b3}, {"i_" #n "b9", i_##n##b9}, {"i_" #n "w1", i_##n##w1}, {"i_" #n "w5", i_##n##w5}, {"i_" #n "w17", i_##n##w17}, {"i_" #n "l1", i_##n##l1}, {"i_" #n "l7", i_##n##l7}, {"i_" #n "l31", i_##n##l31}
int main(void) {
    static const struct T tests[] = {
        E3(s_, shl), E3(s_, shr), E3(s_, sar), E3(s_, rol), E3(s_, ror), E3(s_, rcl), E3(s_, rcr),
        {"d_shldw", d_shldw}, {"d_shldl", d_shldl}, {"d_shrdw", d_shrdw}, {"d_shrdl", d_shrdl},
        EI(shl), EI(shr), EI(sar), EI(rol), EI(ror),
    };
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0x9e3779b9u + i * 104729u;
        u32 a = tests[i].f();
        rs = 0x9e3779b9u + i * 104729u;
        u32 b = tests[i].f();
        printf("%-12s %08x%s\n", tests[i].name, a, a == b ? "" : " MISMATCH");
    }
    return 0;
}
