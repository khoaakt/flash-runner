// Integer/branch/memory micro benchmark (deterministic output) used to track interpreter speed.
#include <stdio.h>
#include <stdint.h>
#include <string.h>
static uint32_t tab[4096];
int main(void) {
    uint32_t x = 12345, acc = 0;
    for (int i = 0; i < 4096; i++) { x = x * 1664525u + 1013904223u; tab[i] = x; }
    for (int round = 0; round < 3000; round++) {
        for (int i = 0; i < 4096; i++) {
            uint32_t v = tab[i];
            v ^= v >> 13; v += (v << 5) ^ acc; if (v & 1) acc += v; else acc ^= v * 3;
            tab[i] = v;
        }
    }
    printf("acc=%u tab0=%u\n", acc, tab[0]);
    return 0;
}
