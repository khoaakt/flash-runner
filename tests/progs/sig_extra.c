#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <pthread.h>
#include <math.h>

static volatile int hits;
static void handler_fp(int s) { volatile double x = 3.0; x = x * x + 1.0; hits += (int)x; }   // dirties x87/SSE state
static void on_usr(int s) { hits++; }
static void *kicker(void *a) { usleep(20000); pthread_kill(*(pthread_t *)a, SIGUSR1); return 0; }

int main(void) {
    // 1. a handler that uses floating point must not disturb the interrupted computation
    signal(SIGUSR2, handler_fp);
    volatile double acc = 1.5;
    for (int i = 0; i < 50; i++) { acc = acc * 1.25 + 0.5; if (i == 25) raise(SIGUSR2); }
    printf("fp state preserved: %.6f hits=%d\n", acc, hits);

    // 2. real-time signal number > 32 raised repeatedly with a non-SA_SIGINFO handler
    struct sigaction sa; memset(&sa, 0, sizeof sa); sa.sa_handler = on_usr; sigaction(SIGRTMIN + 2, &sa, 0);
    hits = 0; raise(SIGRTMIN + 2); raise(SIGRTMIN + 2); raise(SIGRTMIN + 2);
    printf("rt signal handled %d times\n", hits);

    // 3. a blocking read interrupted by a signal returns EINTR (handler installed without SA_RESTART)
    sa.sa_handler = on_usr; sa.sa_flags = 0; sigaction(SIGUSR1, &sa, 0);
    int p[2]; pipe(p); pthread_t me = pthread_self(), k; pthread_create(&k, 0, kicker, &me);
    char c; int r = read(p[0], &c, 1);
    printf("read interrupted: r=%d eintr=%d\n", r, errno == EINTR);
    pthread_join(k, 0);

    // 4. rename into a missing directory must fail and keep the source
    FILE *f = fopen("/tmp/ren_src", "w"); fputs("x", f); fclose(f);
    int rr = rename("/tmp/ren_src", "/tmp/no_such_dir/ren_dst");
    printf("rename -> %d exists=%d\n", rr, access("/tmp/ren_src", F_OK) == 0);
    return 0;
}
