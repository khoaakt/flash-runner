#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <stdint.h>

int main(void) {
    /* malloc stress incl. mmap-threshold sized chunks and realloc growth */
    void *ptrs[256]; uint32_t seed = 12345; uint64_t sum = 0;
    for (int i = 0; i < 256; i++) { seed = seed * 1103515245 + 12345; size_t n = (seed >> 8) % 70000; ptrs[i] = malloc(n + 1); memset(ptrs[i], i, n + 1); sum += ((unsigned char *)ptrs[i])[n]; }
    for (int i = 0; i < 256; i += 2) free(ptrs[i]);
    for (int i = 1; i < 256; i += 2) { ptrs[i] = realloc(ptrs[i], 300000); sum += ((unsigned char *)ptrs[i])[0]; free(ptrs[i]); }
    printf("sum=%llu\n", (unsigned long long)sum);
    char *big = malloc(40 << 20); memset(big, 7, 40 << 20); printf("big ok %d\n", big[(40 << 20) - 1]); free(big);

    char *a = mmap(0, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    a[0] = 1; a[(1 << 20) - 1] = 2;
    char *b = mremap(a, 1 << 20, 4 << 20, MREMAP_MAYMOVE);
    printf("mremap kept data: %d %d\n", b[0], b[(1 << 20) - 1]);
    munmap(b, 4 << 20);
    char *x = mmap(0, 4096 * 4, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(x, 4096 * 4, PROT_READ | PROT_WRITE | PROT_EXEC);
    /* write a tiny function (mov eax, 1234; ret) and call it: exercises RWX + SMC invalidation */
    unsigned char code[] = {0xb8, 0xd2, 0x04, 0x00, 0x00, 0xc3};
    memcpy(x, code, sizeof code);
    int (*fn)(void) = (int (*)(void))x;
    int r1 = fn();
    code[1] = 0x39; code[2] = 0x05; memcpy(x, code, sizeof code);   /* patch immediate in place -> 1337 */
    int r2 = fn();
    printf("jit-style call: %d then patched %d\n", r1, r2);
    return 0;
}
