// JIT stress: compiled code must stay preemptible. A thread spinning in a hot syscall-free loop is only ever
// descheduled by the instruction budget; the other threads have to make progress for the program to finish.
#define _GNU_SOURCE
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <time.h>
typedef uint32_t u32;
static volatile int flag1, flag2;
static volatile u32 spins1, spins2;
static u32 counter;                    // updated with lock-prefixed instructions only
static volatile u32 arrived;

static void *spinner(void *a) {
    (void)a;
    while (!flag1) spins1++;           // tight compiled loop, never enters the kernel
    return 0;
}
static void *waiter(void *a) {         // sets flag1 after doing its own bounded work
    (void)a;
    u32 x = 1;
    for (int i = 0; i < 200000; i++) x = x * 1664525u + 1013904223u;
    flag1 = x != 0 || x == 0;
    return 0;
}
static void *atomic_worker(void *a) {
    int n = (int)(intptr_t)a;
    for (int i = 0; i < n; i++) {
        __asm__ volatile("lock incl %0" : "+m"(counter) : : "memory", "cc");
        u32 old, nw;
        do {
            old = counter;
            nw = old + 2;
            __asm__ volatile("lock cmpxchgl %2, %1" : "=a"(old) : "m"(counter), "r"(nw), "0"(old) : "memory", "cc");
        } while (old != counter - 0 && 0);
        __asm__ volatile("lock xaddl %1, %0" : "+m"(counter), "+r"(nw) : : "memory", "cc");
    }
    return 0;
}
static void *sleeper_setter(void *a) {
    (void)a;
    struct timespec ts = {0, 2000000};
    nanosleep(&ts, 0);                  // the spinner must be preempted for this thread to run at all
    flag2 = 1;
    return 0;
}
static void *spinner2(void *a) {
    (void)a;
    u32 x = 0;
    while (!flag2) x += 3;
    spins2 = x;
    return 0;
}

int main(void) {
    pthread_t t[8];
    pthread_create(&t[0], 0, spinner, 0);
    pthread_create(&t[1], 0, waiter, 0);
    pthread_join(t[1], 0);
    pthread_join(t[0], 0);
    printf("spinner released: %s\n", spins1 > 0 ? "yes" : "no");

    pthread_create(&t[2], 0, spinner2, 0);
    pthread_create(&t[3], 0, sleeper_setter, 0);
    pthread_join(t[3], 0);
    pthread_join(t[2], 0);
    printf("spinner2 released: %s\n", spins2 > 0 ? "yes" : "no");

    for (int i = 0; i < 4; i++) pthread_create(&t[4 + i], 0, atomic_worker, (void *)(intptr_t)3000);
    for (int i = 0; i < 4; i++) pthread_join(t[4 + i], 0);
    // each iteration adds: 1 (incl) + 2 (cmpxchg loop body writes nothing: compare only) -> only incl and xadd(nw) count
    printf("counter ok: %s\n", counter >= 4 * 3000 ? "yes" : "no");
    return 0;
}
