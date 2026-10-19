// Pointer chasing through a shuffled singly linked list (dependent loads, tiny loop body).
#include <stdio.h>
#include <stdint.h>
#define N 8192
struct node { struct node *next; uint32_t v; };
static struct node nodes[N];
int main(void) {
    uint32_t x = 1;
    int order[N];
    for (int i = 0; i < N; i++) order[i] = i;
    for (int i = N - 1; i > 0; i--) { x = x * 1103515245u + 12345u; int j = (x >> 8) % (i + 1); int t = order[i]; order[i] = order[j]; order[j] = t; }
    for (int i = 0; i < N; i++) { nodes[order[i]].next = &nodes[order[(i + 1) % N]]; nodes[order[i]].v = i * 7u; }
    uint32_t sum = 0;
    struct node *p = &nodes[order[0]];
    for (int r = 0; r < 2000; r++) for (int i = 0; i < N; i++) { sum += p->v; p = p->next; }
    printf("sum=%u\n", sum);
    return 0;
}
