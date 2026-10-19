#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <stdlib.h>

#define NT 4
#define ITERS 20000
static pthread_mutex_t mu = PTHREAD_MUTEX_INITIALIZER;
static long counter;
static __thread int tls_val = 5;
static pthread_key_t key;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static int once_count;

static void once_fn(void) { once_count++; }

static void *worker(void *arg) {
    long id = (long)arg;
    pthread_once(&once, once_fn);
    tls_val += id;
    for (int i = 0; i < ITERS; i++) {
        pthread_mutex_lock(&mu);
        counter++;
        pthread_mutex_unlock(&mu);
        if (i % 5000 == 0) sched_yield();
    }
    pthread_setspecific(key, (void *)(id * 100));
    long r = tls_val * 1000 + (long)pthread_getspecific(key);
    return (void *)r;
}

/* producer / consumer through a condvar */
static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static int queue[16], qn, done;
static void *producer(void *a) {
    for (int i = 1; i <= 100; i++) {
        pthread_mutex_lock(&mu);
        while (qn == 16) pthread_cond_wait(&cv, &mu);
        queue[qn++] = i;
        pthread_cond_broadcast(&cv);
        pthread_mutex_unlock(&mu);
    }
    pthread_mutex_lock(&mu); done = 1; pthread_cond_broadcast(&cv); pthread_mutex_unlock(&mu);
    return 0;
}
static void *consumer(void *a) {
    long sum = 0;
    for (;;) {
        pthread_mutex_lock(&mu);
        while (qn == 0 && !done) pthread_cond_wait(&cv, &mu);
        if (qn == 0 && done) { pthread_mutex_unlock(&mu); break; }
        sum += queue[--qn];
        pthread_cond_broadcast(&cv);
        pthread_mutex_unlock(&mu);
    }
    return (void *)sum;
}

static void *sleeper(void *a) { usleep(20000); return (void *)77; }

int main(void) {
    pthread_key_create(&key, 0);
    pthread_t t[NT];
    for (long i = 0; i < NT; i++) pthread_create(&t[i], 0, worker, (void *)i);
    for (int i = 0; i < NT; i++) { void *r; pthread_join(t[i], &r); printf("thread %d -> %ld\n", i, (long)r); }
    printf("counter=%ld once=%d main_tls=%d\n", counter, once_count, tls_val);

    pthread_t p, c; void *sum;
    pthread_create(&p, 0, producer, 0); pthread_create(&c, 0, consumer, 0);
    pthread_join(p, 0); pthread_join(c, &sum);
    printf("consumed sum=%ld\n", (long)sum);

    pthread_t s; void *r;
    pthread_create(&s, 0, sleeper, 0); pthread_join(s, &r);
    printf("sleeper=%ld\n", (long)r);

    /* detached thread + cancellation */
    pthread_t d;
    pthread_create(&d, 0, sleeper, 0);
    pthread_detach(d);
    usleep(50000);
    puts("done");
    return 0;
}
