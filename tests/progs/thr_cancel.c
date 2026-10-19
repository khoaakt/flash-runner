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
    pthread_t t1, t2;
    pthread_create(&t1, 0, blocked_in_cond, 0); pthread_create(&t2, 0, sleeper, 0);
    usleep(30000);
    pthread_cancel(t1); pthread_cancel(t2);
    void *r1, *r2; pthread_join(t1, &r1); pthread_join(t2, &r2);
    printf("cancelled: %d %d cleanup=%d\n", r1 == PTHREAD_CANCELED, r2 == PTHREAD_CANCELED, cleanup_ran);
    return 0;
}
