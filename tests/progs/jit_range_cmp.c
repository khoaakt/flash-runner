// Standalone exact-byte comparator correctness/benchmark fixture, NOT a full Flash integration workload.
#define _GNU_SOURCE
#include <stdint.h>
#include <stdio.h>
#include <sys/mman.h>
typedef uint32_t u32;
extern int range_cmp(const u32 *, const u32 *) __attribute__((cdecl));
__asm__(".text\n.p2align 4\n.globl range_cmp\n.type range_cmp,@function\nrange_cmp:\n"
        ".byte 0x55,0x89,0xe5,0x56,0x8b,0x4d,0x0c,0x8b,0x45,0x08,0x8b,0x10,0x8b,0x31,0xb8,0xff,0xff,0xff,0xff,0x39,0xf2,0x72,0x0a,0x03,0x71,0x04,0x31,0xc0,0x39,0xf2,0x0f,0x93,0xc0,0x5e,0x5d,0xc3\n"
        ".size range_cmp,.-range_cmp\n");
static u32 state = 0x715a9e31;
static u32 rnd(void) { state ^= state << 13; state ^= state >> 17; state ^= state << 5; return state; }
int main(void) {
    u32 hash = 2166136261u, bad = 0;
    for (u32 i = 0; i < 1000000; ++i) {
        u32 elem[2] = {rnd(), rnd()}, key;
        switch (i % 6) {
        case 0: key = elem[0]; break;
        case 1: key = elem[0] - 1; break;
        case 2: key = elem[0] + elem[1]; break;
        case 3: key = elem[0] + elem[1] - 1; break;
        default: key = rnd(); break;
        }
        int want = key < elem[0] ? -1 : key >= elem[0] + elem[1];
        int got = range_cmp(&key, elem);
        bad += got != want;
        hash = (hash ^ (u32)got) * 16777619u;
    }
    // The length word is inaccessible, but must never be read on the below-start branch.
    unsigned char *pages = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (pages == MAP_FAILED) return 2;
    u32 *elem = (u32 *)(pages + 4092), key = 9;
    *elem = 10;
    if (mprotect(pages + 4096, 4096, PROT_NONE)) return 2;
    for (int i = 0; i < 1000; ++i) bad += range_cmp(&key, elem) != -1;
    munmap(pages, 8192);
    printf("range_cmp calls=1001000 mismatches=%u hash=%08x\n", bad, hash);
    return bad != 0;
}
