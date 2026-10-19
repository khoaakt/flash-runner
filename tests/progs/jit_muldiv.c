// JIT stress: mul/imul (1, 2 and 3 operand), div/idiv at 8/16/32 bits with edge operands. Divide errors
// (#DE: divide by zero and quotient overflow) must raise SIGFPE at exactly the faulting instruction with the
// registers untouched; the handler's view (ip offset, eax, edx) is part of the checksum.
#define _GNU_SOURCE
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <ucontext.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0x1234abcdu;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static const u32 edge[] = {0, 1, 2, 3, 0x7f, 0x80, 0x81, 0xff, 0x100, 0x7fff, 0x8000, 0x8001, 0xffff, 0x10000, 0x7fffffffu, 0x80000000u, 0x80000001u, 0xfffffffeu, 0xffffffffu, 0xffffff80u, 0xffff8000u, 0x55555555u};
static u32 val(void) { u32 r = rnd(); return (r & 3) ? edge[(r >> 2) % (sizeof edge / sizeof edge[0])] : rnd(); }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }
#define CF 1u
#define OF 0x800u
#define ALLF 0x8d5u

static sigjmp_buf jb;
static volatile u32 cur_ip, fault_ip, fault_eax, fault_edx, nfaults;
static volatile int got_sig;
static void on_fpe(int sig, siginfo_t *si, void *ucv) {
    ucontext_t *uc = ucv;
    got_sig = sig;
    fault_ip = (u32)uc->uc_mcontext.gregs[REG_EIP];
    fault_eax = (u32)uc->uc_mcontext.gregs[REG_EAX];
    fault_edx = (u32)uc->uc_mcontext.gregs[REG_EDX];
    (void)si;
    nfaults++;
    siglongjmp(jb, 1);
}

// Run one division; returns 1 and fills `fh` with the fault view if it trapped.
#define DIV_TEST(NAME, INS, CODE)                                                                                      \
    static u32 d_##NAME(void) {                                                                                         \
        u32 h = 2166136261u;                                                                                            \
        for (int i = 0; i < 2500; i++) {                                                                                \
            u32 a = val(), d = val(), dx = val();                                                                       \
            u32 ea = a, ed = dx;                                                                                        \
            if (sigsetjmp(jb, 0)) {                                                                                     \
                h = mix(h, 0xdead0000u + (fault_ip - cur_ip));                                                          \
                h = mix(mix(h, fault_eax), fault_edx);                                                                  \
                continue;                                                                                               \
            }                                                                                                           \
            CODE                                                                                                        \
            h = mix(mix(h, ea), ed);                                                                                    \
        }                                                                                                               \
        return h;                                                                                                       \
    }
#define ASM_DIV8(INS)  __asm__ volatile("call 0f\n0:\tpopl %[ipm]\n\t" INS " %b[dd]" : [ipm] "=m"(cur_ip), "+a"(ea) : [dd] "q"(d) : "cc");
#define ASM_DIV16(INS) __asm__ volatile("call 0f\n0:\tpopl %[ipm]\n\t" INS " %w[dd]" : [ipm] "=m"(cur_ip), "+a"(ea), "+d"(ed) : [dd] "r"(d) : "cc");
#define ASM_DIV32(INS) __asm__ volatile("call 0f\n0:\tpopl %[ipm]\n\t" INS " %[dd]" : [ipm] "=m"(cur_ip), "+a"(ea), "+d"(ed) : [dd] "r"(d) : "cc");
DIV_TEST(div8, "divb", ASM_DIV8("divb"))
DIV_TEST(idiv8, "idivb", ASM_DIV8("idivb"))
DIV_TEST(div16, "divw", ASM_DIV16("divw"))
DIV_TEST(idiv16, "idivw", ASM_DIV16("idivw"))
DIV_TEST(div32, "divl", ASM_DIV32("divl"))
DIV_TEST(idiv32, "idivl", ASM_DIV32("idivl"))

// divisor in memory (a faulting memory read must win over the divide error check order: both stay precise)
static u32 d_div32m(void) {
    u32 h = 2166136261u;
    static volatile u32 mem;
    for (int i = 0; i < 2500; i++) {
        u32 ea = val(), ed = val() & 0xff;
        mem = val();
        if (sigsetjmp(jb, 0)) {
            h = mix(mix(mix(h, 0xdead0000u + (fault_ip - cur_ip)), fault_eax), fault_edx);
            continue;
        }
        __asm__ volatile("call 0f\n0:\tpopl %[ipm]\n\tdivl %[m]" : [ipm] "=m"(cur_ip), "+a"(ea), "+d"(ed) : [m] "m"(mem) : "cc");
        h = mix(mix(h, ea), ed);
    }
    return h;
}

// one-operand multiplies: results and the defined flags (CF, OF)
#define MUL1(NAME, INS, DECL, ASM)                                         \
    static u32 m_##NAME(void) {                                            \
        u32 h = 2166136261u;                                               \
        for (int i = 0; i < 2500; i++) {                                   \
            u32 a = val(), s = val(), d = val(), fl; DECL                  \
            ASM                                                            \
            h = mix(mix(mix(h, a), d), fl & (CF | OF));                    \
        }                                                                  \
        return h;                                                          \
    }
MUL1(mul8, "mulb", , __asm__ volatile("mulb %b2\n\tpushf\n\tpopl %1" : "+a"(a), "=r"(fl) : "q"(s) : "cc"); d = a >> 8;)
MUL1(imul8, "imulb", , __asm__ volatile("imulb %b2\n\tpushf\n\tpopl %1" : "+a"(a), "=r"(fl) : "q"(s) : "cc"); d = a >> 8;)
MUL1(mul16, "mulw", , __asm__ volatile("mulw %w3\n\tpushf\n\tpopl %1" : "+a"(a), "=r"(fl), "+d"(d) : "r"(s) : "cc");)
MUL1(imul16, "imulw", , __asm__ volatile("imulw %w3\n\tpushf\n\tpopl %1" : "+a"(a), "=r"(fl), "+d"(d) : "r"(s) : "cc");)
MUL1(mul32, "mull", , __asm__ volatile("mull %3\n\tpushf\n\tpopl %1" : "+a"(a), "=r"(fl), "+d"(d) : "r"(s) : "cc");)
MUL1(imul32, "imull", , __asm__ volatile("imull %3\n\tpushf\n\tpopl %1" : "+a"(a), "=r"(fl), "+d"(d) : "r"(s) : "cc");)

// two/three operand imul (CF/OF defined; SF ZF AF PF undefined)
static u32 m_imul2_16(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 2500; i++) {
        u16 a = (u16)val(), b = (u16)val(); u32 fl;
        __asm__ volatile("imulw %2, %0\n\tpushf\n\tpopl %1" : "+r"(a), "=r"(fl) : "r"(b) : "cc");
        h = mix(mix(h, a), fl & (CF | OF));
    }
    return h;
}
static u32 m_imul2_32(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 2500; i++) {
        u32 a = val(), b = val(), fl;
        __asm__ volatile("imull %2, %0\n\tpushf\n\tpopl %1" : "+r"(a), "=r"(fl) : "r"(b) : "cc");
        h = mix(mix(h, a), fl & (CF | OF));
    }
    return h;
}
static u32 m_imul3(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 2500; i++) {
        u32 a = val(), r1, r2, r3, fl;
        __asm__ volatile("imull $1664525, %2, %0\n\tpushf\n\tpopl %1" : "=r"(r1), "=r"(fl) : "r"(a) : "cc");
        h = mix(mix(h, r1), fl & (CF | OF));
        __asm__ volatile("imull $-7, %2, %0\n\tpushf\n\tpopl %1" : "=r"(r2), "=r"(fl) : "r"(a) : "cc");
        h = mix(mix(h, r2), fl & (CF | OF));
        u16 w = (u16)a, w2;
        __asm__ volatile("imulw $300, %2, %0\n\tpushf\n\tpopl %1" : "=r"(w2), "=r"(fl) : "r"(w) : "cc");
        h = mix(mix(h, w2), fl & (CF | OF));
        r3 = a;
        __asm__ volatile("imull $0x10001, (%2), %0\n\tpushf\n\tpopl %1" : "=r"(r3), "=r"(fl) : "r"(&a) : "cc", "memory");
        h = mix(mix(h, r3), fl & (CF | OF));
    }
    return h;
}

// cdq/cwd/cbw/cwde
static u32 t_sext(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 2500; i++) {
        u32 a = val(), b = val(), c = val(), d = val();
        u32 x = a, y = b;
        __asm__ volatile("cdq" : "+a"(x), "=d"(y));
        h = mix(mix(h, x), y);
        x = a; y = b;
        __asm__ volatile("cwde" : "+a"(x));
        h = mix(h, x);
        x = a; y = c;
        __asm__ volatile("cbw" : "+a"(x));
        h = mix(h, x);
        x = a; y = d;
        __asm__ volatile("cwd" : "+a"(x), "+d"(y));
        h = mix(mix(h, x), y);
    }
    return h;
}

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define E(n) {#n, n}
int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_fpe;
    sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGFPE, &sa, 0);
    static const struct T tests[] = {
        E(d_div8), E(d_idiv8), E(d_div16), E(d_idiv16), E(d_div32), E(d_idiv32), E(d_div32m),
        E(m_mul8), E(m_imul8), E(m_mul16), E(m_imul16), E(m_mul32), E(m_imul32),
        E(m_imul2_16), E(m_imul2_32), E(m_imul3), E(t_sext),
    };
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0x1234abcdu + i * 7919u;
        nfaults = 0;
        u32 a = tests[i].f();
        u32 nf = nfaults;
        rs = 0x1234abcdu + i * 7919u;
        u32 b = tests[i].f();
        printf("%-10s %08x faults=%u%s\n", tests[i].name, a, nf, a == b ? "" : " MISMATCH");
    }
    return 0;
}
