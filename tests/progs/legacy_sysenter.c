// `sysenter` as the fast syscall gate: the same Linux/i386 ABI as `int 0x80` (number in eax,
// the kernel may clobber ecx/edx, execution resumes after the instruction). SEP is not advertised
// by our CPUID so glibc sticks to `int 0x80`; this covers hand-written asm that uses it anyway.
// The oracle is unusable (qemu-user raises #GP on sysenter instead of trapping it as a syscall),
// so this program is compared against the hand-written tests/progs/legacy_sysenter.expected.
// The outputs are self-relative (no pids/uids are printed), identical for static and dynamic.
#include <stdio.h>
#include <unistd.h>
#include <sys/types.h>

static long se(long nr) {
    long r;
    __asm__ volatile ("sysenter" : "=a"(r) : "a"(nr) : "ecx", "edx", "memory");
    return r;
}
static long ia32(long nr) {
    long r;
    __asm__ volatile ("int $0x80" : "=a"(r) : "a"(nr) : "memory");
    return r;
}

int main(void) {
    setbuf(stdout, NULL);
    long pid = se(20); // __NR_getpid (i386)
    printf("sysenter getpid: match=%s nonzero=%s\n",
           pid == (long)getpid() ? "yes" : "no", pid > 0 ? "yes" : "no");
    printf("agrees with int80: %s\n", pid == ia32(20) ? "yes" : "no");
    printf("sysenter getuid: %s\n", se(199) == (long)getuid() ? "yes" : "no"); // __NR_getuid32
    printf("sysenter geteuid: %s\n", se(201) == (long)geteuid() ? "yes" : "no"); // __NR_geteuid32
    return 0;
}
