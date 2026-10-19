// Cost of time queries: 2 M clock_gettime(CLOCK_MONOTONIC) calls (Flash makes ~150 000 per second)
#include <stdio.h>
#include <time.h>
#include <sys/time.h>
int main(void) {
    struct timespec ts; struct timeval tv;
    unsigned long long acc = 0;
    for (int i = 0; i < 2000000; i++) { clock_gettime(CLOCK_MONOTONIC, &ts); acc += ts.tv_nsec; }
    for (int i = 0; i < 1000000; i++) { gettimeofday(&tv, 0); acc += tv.tv_usec; }
    printf("done %d\n", acc == 0);
    return 0;
}
