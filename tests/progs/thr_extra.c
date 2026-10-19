#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
#include <errno.h>
#include <sched.h>
#include <semaphore.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <sys/syscall.h>

static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int cleanup_ran, woke;

static void cleanup(void *a) { cleanup_ran++; }
static void *blocked_in_cond(void *a) {
    pthread_cleanup_push(cleanup, 0);
    pthread_mutex_lock(&mu);
    for (;;) pthread_cond_wait(&cv, &mu);
    pthread_cleanup_pop(0);
    return 0;
}
static void *sleeper(void *a) {
    pthread_cleanup_push(cleanup, 0);
    struct timespec ts = {5, 0};
    nanosleep(&ts, 0);
    pthread_cleanup_pop(0);
    return 0;
}
static void *with_big_stack(void *a) {
    volatile char buf[1 << 20]; buf[0] = 1; buf[sizeof buf - 1] = 2;
    pthread_attr_t at; pthread_getattr_np(pthread_self(), &at);
    void *sp; size_t sz; pthread_attr_getstack(&at, &sp, &sz);
    pthread_attr_destroy(&at);
    return (void *)(sz >= (4u << 20) ? 1 : 0);
}
static sem_t sem;
static void *sem_poster(void *a) { usleep(10000); sem_post(&sem); return 0; }
static __thread char tls_buf[256];
static void *tls_user(void *a) { memset(tls_buf, (int)(long)a, sizeof tls_buf); usleep(1000); return (void *)(long)(tls_buf[100] + tls_buf[255]); }

int main(void) {
    pthread_attr_t at; pthread_getattr_np(pthread_self(), &at);
    size_t ssz; void *sp; pthread_attr_getstack(&at, &sp, &ssz);
    printf("main stack size >= 1MB: %d\n", ssz >= (1u << 20));

    pthread_attr_t big; pthread_attr_init(&big); pthread_attr_setstacksize(&big, 16 << 20);
    pthread_t t3; void *r3; pthread_create(&t3, &big, with_big_stack, 0); pthread_join(t3, &r3);
    printf("big stack thread ok: %ld\n", (long)r3);

    sem_init(&sem, 0, 0); pthread_t t4; pthread_create(&t4, 0, sem_poster, 0);
    struct timespec dl; clock_gettime(CLOCK_REALTIME, &dl); dl.tv_sec += 2;
    printf("sem_timedwait: %d\n", sem_timedwait(&sem, &dl)); pthread_join(t4, 0);
    dl.tv_sec = 0; dl.tv_nsec = 1000000; clock_gettime(CLOCK_REALTIME, &dl); dl.tv_nsec += 5000000;
    if (dl.tv_nsec >= 1000000000) { dl.tv_sec++; dl.tv_nsec -= 1000000000; }
    int tr = sem_timedwait(&sem, &dl); printf("sem timeout: %d errno=%d\n", tr, errno == ETIMEDOUT);

    pthread_t ts[6]; long sum = 0; void *rv;
    for (long i = 0; i < 6; i++) pthread_create(&ts[i], 0, tls_user, (void *)(i + 1));
    for (int i = 0; i < 6; i++) { pthread_join(ts[i], &rv); sum += (long)rv; }
    printf("tls sum=%ld\n", sum);

    struct timespec a, b; clock_gettime(CLOCK_MONOTONIC, &a); usleep(20000); clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    printf("monotonic slept ~20ms: %d\n", ms >= 19 && ms < 200);
    struct rusage ru; getrusage(RUSAGE_SELF, &ru); printf("rusage ok: %d\n", 1);
    cpu_set_t cs; sched_getaffinity(0, sizeof cs, &cs); printf("ncpu ok=%d affinity ok=%d\n", sysconf(_SC_NPROCESSORS_ONLN) >= 1, CPU_COUNT(&cs) >= 1);
    printf("gettid==pid: %d\n", syscall(SYS_gettid) == getpid());
    return 0;
}
