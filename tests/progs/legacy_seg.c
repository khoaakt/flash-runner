// Segment / protected-mode queries (lar/lsl/verr/verw, sgdt/sidt/sldt/smsw/str) and far-pointer
// loads (lds/les/lfs/lgs/lss) against the emulator's flat virtual Linux/i386 GDT: __USER_CS 0x23
// (flat code, R), __USER_DS 0x2b and the TLS slots 0x33/0x3b/0x43 (flat data, RW), everything else
// unusable at CPL 3. The oracle is unusable here (qemu-user keeps a different synthetic GDT and a
// real kernel's values are per-machine), so this program is compared against the hand-written
// tests/progs/legacy_seg.expected, derived from the documented personality in crates/emu/src/int.rs.
#define _GNU_SOURCE
#include <stdio.h>

struct far { unsigned off; unsigned short sel; } __attribute__((packed));
static __thread int tls_probe;

int main(void) {
    setbuf(stdout, NULL);

    // ---- far-pointer loads (FS is null in this process; GS carries the TLS base: reloading
    // selector 0x33 recomputes the same base, so libc TLS keeps working) ----
    unsigned v;
    struct far f = { 0x12345678, 0x2b }, z = { 0, 0 };
    __asm__ ("lfs %1, %0" : "=r"(v) : "m"(f)); printf("lfs %08x\n", v);
    __asm__ ("lfs %1, %0" : "=r"(v) : "m"(z)); // restore FS = null
    f.off = 0x9abcdef0;
    __asm__ ("les %1, %0" : "=r"(v) : "m"(f)); printf("les %08x\n", v);
    f.off = 0xfedcba98;
    __asm__ ("lds %1, %0" : "=r"(v) : "m"(f)); printf("lds %08x\n", v);
    f.off = 0x00001111;
    __asm__ ("lss %1, %0" : "=r"(v) : "m"(f)); printf("lss %08x\n", v);
    f.off = 0xdeadbeef; f.sel = 0x33;
    __asm__ ("lgs %1, %0" : "=r"(v) : "m"(f)); printf("lgs %08x\n", v);
    tls_probe = 42;
    printf("tls ok %d\n", tls_probe == 42);

    // ---- lar / lsl over interesting selectors ----
    static const unsigned short sels[] = { 0x0000, 0x0008, 0x0010, 0x0023, 0x002b, 0x0033, 0x003b, 0x0043, 0x1234, 0x002f };
    for (unsigned i = 0; i < sizeof sels / sizeof sels[0]; i++) {
        unsigned src = sels[i], d = 0xdeadbeef, fl;
        __asm__ ("larl %2, %0\n\tlahf\n\tmovzbl %%ah, %1" : "+r"(d), "=r"(fl) : "r"(src) : "cc");
        printf("lar %04x -> %08x zf=%d\n", src, d, (fl >> 6) & 1);
        d = 0xdeadbeef;
        __asm__ ("lsll %2, %0\n\tlahf\n\tmovzbl %%ah, %1" : "+r"(d), "=r"(fl) : "r"(src) : "cc");
        printf("lsl %04x -> %08x zf=%d\n", src, d, (fl >> 6) & 1);
    }

    // ---- verr/verw: code is readable-only, data is both, kernel is neither ----
    for (int k = 0; k < 3; k++) {
        unsigned src = (unsigned short[]){ 0x0023, 0x002b, 0x0010 }[k], fl;
        __asm__ ("verr %w1\n\tlahf\n\tmovzbl %%ah, %0" : "=r"(fl) : "r"(src) : "cc");
        printf("verr %04x zf=%d\n", src, (fl >> 6) & 1);
        __asm__ ("verw %w1\n\tlahf\n\tmovzbl %%ah, %0" : "=r"(fl) : "r"(src) : "cc");
        printf("verw %04x zf=%d\n", src, (fl >> 6) & 1);
    }

    // ---- descriptor-table stores and system registers ----
    unsigned char desc[8];
    __asm__ volatile ("sgdt %0" : "=m"(desc));
    printf("sgdt limit=%04x base=%08x\n", desc[0] | desc[1] << 8,
           desc[2] | desc[3] << 8 | desc[4] << 16 | (unsigned)desc[5] << 24);
    __asm__ volatile ("sidt %0" : "=m"(desc));
    printf("sidt limit=%04x base=%08x\n", desc[0] | desc[1] << 8,
           desc[2] | desc[3] << 8 | desc[4] << 16 | (unsigned)desc[5] << 24);
    unsigned s;
    __asm__ ("sldt %0" : "=r"(s)); printf("sldt %04x\n", s & 0xffff);
    __asm__ ("str %0" : "=r"(s));  printf("str %04x\n", s & 0xffff);
    __asm__ ("smsw %0" : "=r"(s)); printf("smsw %04x\n", s & 0xffff);
    return 0;
}
