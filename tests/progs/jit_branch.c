// JIT stress: control flow. Jump tables, indirect calls, callee-pops returns, recursion, loop/jecxz, and
// self-modifying code: patching an already hot function, a store that rewrites the very next instruction,
// code regenerated at the same address, and computed calls into freshly written code.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32;
static u32 rs = 0xabad1deau;
static u32 rnd(void) { rs ^= rs << 13; rs ^= rs >> 17; rs ^= rs << 5; return rs; }
static u32 mix(u32 h, u32 v) { return (h ^ v) * 16777619u + (h >> 15); }

// ---- jump tables / switch ---------------------------------------------------------------------------------
__attribute__((noinline)) static u32 sw(u32 x, u32 acc) {
    switch (x & 15) {
    case 0: return acc + 1;
    case 1: return acc * 3;
    case 2: return acc ^ 0x55;
    case 3: return acc - 7;
    case 4: return (acc << 3) | 1;
    case 5: return acc + x;
    case 6: return acc ^ (x << 2);
    case 7: return acc >> 1;
    case 8: return acc + 100;
    case 9: return acc * acc;
    case 10: return acc - x;
    case 11: return ~acc;
    case 12: return acc + (acc >> 4);
    case 13: return acc ^ 0xdeadbeef;
    case 14: return acc * 7 + 3;
    default: return acc + 0x1234;
    }
}
static u32 t_switch(void) {
    u32 acc = 1;
    for (int i = 0; i < 20000; i++) acc = sw(rnd(), acc);
    return acc;
}

// ---- function pointer tables ----------------------------------------------------------------------------------
static u32 f0(u32 x) { return x + 1; }
static u32 f1(u32 x) { return x * 5; }
static u32 f2(u32 x) { return x ^ 0xa5a5; }
static u32 f3(u32 x) { return (x << 7) | (x >> 25); }
static u32 f4(u32 x) { return x - 3; }
static u32 (*const ftab[])(u32) = {f0, f1, f2, f3, f4, f3, f2, f1};
static u32 t_fptr(void) {
    u32 x = 1;
    for (int i = 0; i < 20000; i++) x = ftab[rnd() & 7](x);
    return x;
}

// ---- computed jumps (label addresses) ---------------------------------------------------------------------
static u32 t_goto(void) {
    static const void *const tab[] = {&&l0, &&l1, &&l2, &&l3};
    u32 acc = 0;
    for (int i = 0; i < 20000; i++) {
        goto *tab[rnd() & 3];
    l0: acc += 3; continue;
    l1: acc ^= 0x1234; continue;
    l2: acc = acc * 5 + 1; continue;
    l3: acc -= i; continue;
    }
    return acc;
}

// ---- callee-pops (stdcall style) returns and register-indirect calls ------------------------------------------------
__asm__(".text\n.globl stdcall3\nstdcall3:\n\tmovl 4(%esp), %eax\n\taddl 8(%esp), %eax\n\timull 12(%esp), %eax\n\tret $12\n");
extern u32 stdcall3(u32, u32, u32) __attribute__((regparm(0)));
static u32 t_stdcall(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 5000; i++) {
        u32 a = rnd(), b = rnd(), c = rnd() | 1, r;
        void *fp = (void *)stdcall3;
        __asm__ volatile("pushl %4\n\tpushl %3\n\tpushl %2\n\tcall *%1\n\t" : "=a"(r) : "r"(fp), "r"(a), "r"(b), "r"(c) : "memory", "cc", "ecx", "edx");
        h = mix(h, r);                   // esp is balanced by the callee (ret $12); a wrong ret imm would crash
        u32 sp_before, sp_after;
        __asm__ volatile("movl %%esp, %0\n\tpushl $1\n\tpushl $2\n\tpushl $3\n\tcall *%2\n\tmovl %%esp, %1" : "=r"(sp_before), "=r"(sp_after) : "r"(fp) : "eax", "ecx", "edx", "memory", "cc");
        h = mix(h, sp_before - sp_after);
    }
    return h;
}

// ---- recursion ------------------------------------------------------------------------------------------------------------
static u32 fib(u32 n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
static u32 ack(u32 m, u32 n) { return m == 0 ? n + 1 : n == 0 ? ack(m - 1, 1) : ack(m - 1, ack(m, n - 1)); }
static u32 even(u32 n);
static u32 odd(u32 n) { return n == 0 ? 0 : even(n - 1); }
static u32 even(u32 n) { return n == 0 ? 1 : odd(n - 1); }
static u32 t_recur(void) { return mix(mix(mix(fib(22), ack(2, 300)), even(1001)), odd(777)); }

// ---- loop / jecxz / loopne --------------------------------------------------------------------------------------------------
static u32 t_loops(void) {
    u32 h = 2166136261u;
    for (int i = 0; i < 2000; i++) {
        u32 n = (rnd() & 63) + 1, s = 0, t;
        __asm__ volatile("movl %1, %%ecx\n\txorl %0, %0\n\t1:\taddl %%ecx, %0\n\tloop 1b" : "=&r"(s) : "r"(n) : "ecx", "cc");
        h = mix(h, s);
        __asm__ volatile("movl %1, %%ecx\n\tjecxz 2f\n\tmovl $1, %0\n\tjmp 3f\n2:\tmovl $2, %0\n3:" : "=r"(t) : "r"(n & 1) : "ecx", "cc");
        h = mix(h, t);
        u32 k;
        __asm__ volatile("movl %1, %%ecx\n\tmovl $0, %0\n\t4:\tincl %0\n\tcmpl $7, %0\n\tloopne 4b" : "=&r"(k) : "r"(n) : "ecx", "cc");
        h = mix(h, k);
    }
    return h;
}

// ---- self-modifying code --------------------------------------------------------------------------------------------------
static u8 *code;
// int f(int x) { return x * K + C; }   mov eax,[esp+4]; imul eax,eax,K ; add eax,C ; ret
static void emit_affine(u8 *p, u32 k, u32 c) {
    static const u8 t[] = {0x8b, 0x44, 0x24, 0x04, 0x69, 0xc0, 0, 0, 0, 0, 0x05, 0, 0, 0, 0, 0xc3};
    memcpy(p, t, sizeof t);
    memcpy(p + 6, &k, 4);
    memcpy(p + 11, &c, 4);
}
typedef u32 (*fn1)(u32);
static u32 t_patch_hot(void) {
    u32 h = 2166136261u;
    emit_affine(code, 3, 5);
    for (int round = 0; round < 40; round++) {
        u32 acc = 0;
        for (int i = 0; i < 3000; i++) acc += ((fn1)code)(i);          // hot: compiled after a few iterations
        h = mix(h, acc);
        // patch just the immediates of the already compiled function
        u32 k = round * 2 + 1, c = round * 17 + 3;
        memcpy(code + 6, &k, 4);
        memcpy(code + 11, &c, 4);
    }
    return h;
}
// a store that rewrites the immediate of the *next* instruction in the same block:
//   mov byte [eax+IMMOFF], 0x55 ; mov ebx, 0x11111111 ; mov eax, ebx ; ret
static u32 t_self_overwrite(void) {
    u32 h = 2166136261u;
    static const u8 t[] = {0xc6, 0x40, 0x00, 0x55, 0xbb, 0x11, 0x11, 0x11, 0x11, 0x89, 0xd8, 0xc3};
    for (int i = 0; i < 3000; i++) {
        memcpy(code, t, sizeof t);
        code[2] = 5;                          // [eax+5] = low byte of the imm32 of `mov ebx, imm32`
        code[5] = (u8)i;                      // original byte (will be overwritten by 0x55)
        u32 r;
        __asm__ volatile("movl %1, %%eax\n\tcall *%%eax" : "=a"(r) : "r"(code) : "ebx", "ecx", "edx", "memory", "cc");
        h = mix(h, r);                        // 0x11111155 when the new byte is seen
    }
    return h;
}
// the same address holds different functions over time
static u32 t_regen(void) {
    u32 h = 2166136261u;
    for (int round = 0; round < 60; round++) {
        emit_affine(code + 64, round + 1, round * 3);
        u32 acc = 0;
        for (int i = 0; i < 400; i++) acc += ((fn1)(code + 64))(i);
        h = mix(h, acc);
        // replace the whole function with `mov eax, round ; ret` (shorter, different layout)
        u8 t[] = {0xb8, 0, 0, 0, 0, 0xc3};
        u32 v = round * 1000;
        memcpy(t + 1, &v, 4);
        memset(code + 64, 0x90, 32);
        memcpy(code + 64, t, sizeof t);
        for (int i = 0; i < 400; i++) acc += ((fn1)(code + 64))(i);
        h = mix(h, acc);
    }
    return h;
}
// many small generated functions called through a pointer table, then all regenerated
static u32 t_gen_table(void) {
    u32 h = 2166136261u;
    fn1 tab[32];
    for (int gen = 0; gen < 20; gen++) {
        for (int k = 0; k < 32; k++) {
            u8 *p = code + 256 + k * 16;
            emit_affine(p, k + gen, gen * 7 + k);
            tab[k] = (fn1)p;
        }
        u32 acc = 0;
        for (int rep = 0; rep < 150; rep++)
            for (int k = 0; k < 32; k++) acc += tab[k]((u32)(rep + k));
        h = mix(h, acc);
    }
    return h;
}

typedef u32 (*fn)(void);
struct T { const char *name; fn f; };
#define E(n) {#n, n}
int main(void) {
    code = mmap(0, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (code == MAP_FAILED) { puts("mmap rwx failed"); return 1; }
    static const struct T tests[] = {
        E(t_switch), E(t_fptr), E(t_goto), E(t_stdcall), E(t_recur), E(t_loops),
        E(t_patch_hot), E(t_self_overwrite), E(t_regen), E(t_gen_table),
    };
    for (unsigned i = 0; i < sizeof tests / sizeof tests[0]; i++) {
        rs = 0xabad1deau + i * 7919u;
        u32 a = tests[i].f();
        rs = 0xabad1deau + i * 7919u;
        u32 b = tests[i].f();
        printf("%-16s %08x%s\n", tests[i].name, a, a == b ? "" : " MISMATCH");
    }
    return 0;
}
