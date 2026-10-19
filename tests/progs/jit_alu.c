// JIT stress: every integer ALU form (add adc sub sbb and or xor cmp test inc dec neg not) on 8/16/32-bit
// registers and memory with edge-biased random operands. For each op/size a checksum of the result and of
// the *architecturally defined* EFLAGS bits is printed, so any single-bit difference shows up.
// The loops are long enough to be compiled by the JIT at any reasonable threshold.
#include <stdio.h>
#include <stdint.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0x2545f491u;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static const u32 edge[] = {0, 1, 2, 3, 0x7f, 0x80, 0x81, 0xff, 0x100, 0x7fff, 0x8000, 0x8001, 0xffff, 0x10000, 0x7fffffffu, 0x80000000u, 0x80000001u, 0xfffffffeu, 0xffffffffu, 0x55555555u, 0xaaaaaaaau, 0xf, 0x10, 0x1f};
static u32 val(void) { u32 r = rnd(); return (r & 3) ? edge[(r >> 2) % (sizeof edge / sizeof edge[0])] : rnd(); }
#define N 1500
#define FL_ALL 0x8d5u
#define FL_NOAF 0x8c5u
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }

// op with an incoming carry: "addl $-1, c" sets CF iff c != 0, giving deterministic preset flags.
#define DEFINE_REG(NAME, INS, SUF, T, CON, MASK)                                                                   \
    static u32 t_##NAME##SUF##_r(void) {                                                                            \
        u32 h = 2166136261u;                                                                                         \
        for (int i = 0; i < N; i++) {                                                                                \
            T a = (T)val(), b = (T)val(); u32 c = rnd() & 1, fl;                                                     \
            __asm__ volatile("addl $-1, %2\n\t" INS " %3, %0\n\tpushf\n\tpopl %1" : "+" CON(a), "=r"(fl), "+r"(c) : CON(b) : "cc"); \
            h = mix(mix(h, (u32)a), fl & (MASK));                                                                    \
        }                                                                                                            \
        return h;                                                                                                    \
    }                                                                                                                \
    static u32 t_##NAME##SUF##_m(void) {                                                                            \
        u32 h = 2166136261u;                                                                                         \
        static volatile T mem;                                                                                       \
        for (int i = 0; i < N; i++) {                                                                                \
            T b = (T)val(); u32 c = rnd() & 1, fl; mem = (T)val();                                                   \
            __asm__ volatile("addl $-1, %2\n\t" INS " %3, %0\n\tpushf\n\tpopl %1" : "+m"(mem), "=r"(fl), "+r"(c) : CON(b) : "cc"); \
            h = mix(mix(h, (u32)mem), fl & (MASK));                                                                  \
        }                                                                                                            \
        return h;                                                                                                    \
    }

#define DEFINE_ALL_SIZES(NAME, INS, MASK)                         \
    DEFINE_REG(NAME, INS "b", b, u8, "q", MASK)                   \
    DEFINE_REG(NAME, INS "w", w, u16, "r", MASK)                  \
    DEFINE_REG(NAME, INS "l", l, u32, "r", MASK)

DEFINE_ALL_SIZES(add, "add", FL_ALL)
DEFINE_ALL_SIZES(adc, "adc", FL_ALL)
DEFINE_ALL_SIZES(sub, "sub", FL_ALL)
DEFINE_ALL_SIZES(sbb, "sbb", FL_ALL)
DEFINE_ALL_SIZES(and, "and", FL_NOAF)
DEFINE_ALL_SIZES(or, "or", FL_NOAF)
DEFINE_ALL_SIZES(xor, "xor", FL_NOAF)
DEFINE_ALL_SIZES(cmp, "cmp", FL_ALL)
DEFINE_ALL_SIZES(test, "test", FL_NOAF)

// one-operand forms
#define DEFINE_UNARY(NAME, INS, SUF, T, CON, MASK)                                                                  \
    static u32 u_##NAME##SUF##_r(void) {                                                                            \
        u32 h = 2166136261u;                                                                                         \
        for (int i = 0; i < N; i++) {                                                                                \
            T a = (T)val(); u32 c = rnd() & 1, fl;                                                                   \
            __asm__ volatile("addl $-1, %2\n\t" INS " %0\n\tpushf\n\tpopl %1" : "+" CON(a), "=r"(fl), "+r"(c) : : "cc");  \
            h = mix(mix(h, (u32)a), fl & (MASK));                                                                    \
        }                                                                                                            \
        return h;                                                                                                    \
    }                                                                                                                \
    static u32 u_##NAME##SUF##_m(void) {                                                                            \
        u32 h = 2166136261u;                                                                                         \
        static volatile T mem;                                                                                       \
        for (int i = 0; i < N; i++) {                                                                                \
            u32 c = rnd() & 1, fl; mem = (T)val();                                                                   \
            __asm__ volatile("addl $-1, %2\n\t" INS " %0\n\tpushf\n\tpopl %1" : "+m"(mem), "=r"(fl), "+r"(c) : : "cc");  \
            h = mix(mix(h, (u32)mem), fl & (MASK));                                                                  \
        }                                                                                                            \
        return h;                                                                                                    \
    }
#define DEFINE_UNARY_ALL(NAME, INS, MASK) \
    DEFINE_UNARY(NAME, INS "b", b, u8, "q", MASK) DEFINE_UNARY(NAME, INS "w", w, u16, "r", MASK) DEFINE_UNARY(NAME, INS "l", l, u32, "r", MASK)
DEFINE_UNARY_ALL(inc, "inc", FL_ALL)
DEFINE_UNARY_ALL(dec, "dec", FL_ALL)
DEFINE_UNARY_ALL(neg, "neg", FL_ALL)
DEFINE_UNARY_ALL(not, "not", FL_ALL)

// immediate forms (sign-extended imm8 and full imm) on a register
static u32 t_imm(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < N; i++) {
        u32 a = val(), fl, r = a, c = rnd() & 1;
        __asm__ volatile("addl $-1, %2\n\taddl $0x7fffffff, %0\n\tsubl $-128, %0\n\txorl $0x80000000, %0\n\tadcl $-1, %0\n\tpushf\n\tpopl %1" : "+r"(r), "=r"(fl), "+r"(c) : : "cc");
        h = mix(mix(h, r), fl & FL_ALL);
        u16 w = (u16)a;
        __asm__ volatile("cmpw $0x8000, %0\n\tpushf\n\tpopl %1" : "+r"(w), "=r"(fl) : : "cc");
        h = mix(h, fl & FL_ALL);
        u8 bb = (u8)a;
        __asm__ volatile("addb $0x80, %0\n\tsbbb $1, %0\n\tpushf\n\tpopl %1" : "+q"(bb), "=r"(fl) : : "cc");
        h = mix(mix(h, bb), fl & FL_ALL);
    }
    return h;
}

// partial register writes: AL/AH/AX interplay
static u32 t_partial(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < N; i++) {
        u32 in[3] = {val(), val(), val()}, o[4];
        __asm__ volatile(
            "movl 0(%1), %%eax\n\tmovl 4(%1), %%ebx\n\tmovl 8(%1), %%ecx\n\tmovl %%eax, %%edx\n\txorl %%ebx, %%edx\n\t"
            "movb %%bl, %%al\n\tmovb %%ch, %%ah\n\taddb %%ah, %%al\n\tmovzbl %%al, %%edx\n\tmovsbl %%ah, %%ecx\n\txchgb %%al, %%ah\n\t"
            "movw %%cx, %%bx\n\taddw %%bx, %%ax\n\tmovb %%dl, %%bh\n\torb %%ch, %%dh\n\t"
            "movl %%eax, 0(%0)\n\tmovl %%ebx, 4(%0)\n\tmovl %%ecx, 8(%0)\n\tmovl %%edx, 12(%0)"
            : : "r"(o), "r"(in) : "eax", "ebx", "ecx", "edx", "cc", "memory");
        h = mix(mix(mix(mix(h, o[0]), o[1]), o[2]), o[3]);
    }
    return h;
}

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define ENT(n) {#n, n}
int main(void) {
    static const struct T tests[] = {
        ENT(t_addb_r), ENT(t_addb_m), ENT(t_addw_r), ENT(t_addw_m), ENT(t_addl_r), ENT(t_addl_m),
        ENT(t_adcb_r), ENT(t_adcb_m), ENT(t_adcw_r), ENT(t_adcw_m), ENT(t_adcl_r), ENT(t_adcl_m),
        ENT(t_subb_r), ENT(t_subb_m), ENT(t_subw_r), ENT(t_subw_m), ENT(t_subl_r), ENT(t_subl_m),
        ENT(t_sbbb_r), ENT(t_sbbb_m), ENT(t_sbbw_r), ENT(t_sbbw_m), ENT(t_sbbl_r), ENT(t_sbbl_m),
        ENT(t_andb_r), ENT(t_andb_m), ENT(t_andw_r), ENT(t_andw_m), ENT(t_andl_r), ENT(t_andl_m),
        ENT(t_orb_r), ENT(t_orb_m), ENT(t_orw_r), ENT(t_orw_m), ENT(t_orl_r), ENT(t_orl_m),
        ENT(t_xorb_r), ENT(t_xorb_m), ENT(t_xorw_r), ENT(t_xorw_m), ENT(t_xorl_r), ENT(t_xorl_m),
        ENT(t_cmpb_r), ENT(t_cmpb_m), ENT(t_cmpw_r), ENT(t_cmpw_m), ENT(t_cmpl_r), ENT(t_cmpl_m),
        ENT(t_testb_r), ENT(t_testb_m), ENT(t_testw_r), ENT(t_testw_m), ENT(t_testl_r), ENT(t_testl_m),
        ENT(u_incb_r), ENT(u_incb_m), ENT(u_incw_r), ENT(u_incw_m), ENT(u_incl_r), ENT(u_incl_m),
        ENT(u_decb_r), ENT(u_decb_m), ENT(u_decw_r), ENT(u_decw_m), ENT(u_decl_r), ENT(u_decl_m),
        ENT(u_negb_r), ENT(u_negb_m), ENT(u_negw_r), ENT(u_negw_m), ENT(u_negl_r), ENT(u_negl_m),
        ENT(u_notb_r), ENT(u_notb_m), ENT(u_notw_r), ENT(u_notw_m), ENT(u_notl_r), ENT(u_notl_m),
        ENT(t_imm), ENT(t_partial),
    };
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0x2545f491u + i * 7919u;
        // run twice: the second run executes the (by then compiled) code again and must agree
        u32 a = tests[i].f();
        rs = 0x2545f491u + i * 7919u;
        u32 b = tests[i].f();
        printf("%-10s %08x%s\n", tests[i].name, a, a == b ? "" : " MISMATCH");
    }
    return 0;
}
