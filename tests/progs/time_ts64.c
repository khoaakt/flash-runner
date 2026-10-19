// clock_gettime64 (403) must write FULL 64-bit timespec fields: a 4-byte write leaves the upper
// half of the guest's i64 as stack garbage and absolute deadlines (sem_timedwait, futex time64)
// explode. Regression test for the refactor that split these syscalls into Kernel::time_syscall.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>

struct ts64 { long long sec, nsec; };

static int sane64(struct ts64 *t) { return t->sec > 0 && t->sec < (1LL << 34) && t->nsec >= 0 && t->nsec < 1000000000LL; }
/* CLOCK_MONOTONIC is boot-relative in the guest: right after start sec may legitimately be 0 */
static int sane64_mono(struct ts64 *t) { return t->sec >= 0 && t->sec < (1LL << 34) && t->nsec >= 0 && t->nsec < 1000000000LL; }
static int sane32_mono(long sec, long nsec) { return sec >= 0 && sec < (1LL << 34) && nsec >= 0 && nsec < 1000000000; }

static int verbose;
int main(int argc, char **argv) {
    verbose = argc > 1;
    struct ts64 t;
    memset(&t, 0xAA, sizeof t);
    long r = syscall(403, 0, &t); // clock_gettime64(CLOCK_REALTIME)
    printf("403 real: ret=%ld sane=%d%s", r, sane64(&t), verbose ? "" : "\n"); if (verbose) printf(" sec=%lld nsec=%lld\n", t.sec, t.nsec);
    memset(&t, 0xAA, sizeof t);
    r = syscall(403, 1, &t); // CLOCK_MONOTONIC
    printf("403 mono: ret=%ld sane=%d%s", r, sane64_mono(&t), verbose ? "" : "\n"); if (verbose) printf(" sec=%lld nsec=%lld\n", t.sec, t.nsec);
    struct { int sec, nsec; } t32;
    memset(&t32, 0xAA, sizeof t32);
    r = syscall(265, 1, &t32); // 32-bit clock_gettime(CLOCK_MONOTONIC)
    printf("265: ret=%ld sane=%d\n", r, sane32_mono(t32.sec, t32.nsec));
    memset(&t32, 0xAA, sizeof t32);
    r = syscall(78, &t32, 0); // gettimeofday
    printf("78: ret=%ld sane=%d\n", r, t32.sec > 0 && t32.nsec >= 0 && t32.nsec < 1000000);
    return 0;
}
