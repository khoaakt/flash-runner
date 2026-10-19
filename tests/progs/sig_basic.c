#include <signal.h>
#include <setjmp.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <ucontext.h>

static sigjmp_buf jb;
static volatile int got_usr1, segv_count;
static volatile void *fault_addr;

static void on_segv(int sig, siginfo_t *si, void *uc) {
    segv_count++;
    fault_addr = si->si_addr;
    siglongjmp(jb, 1);
}
static void on_usr1(int sig) { got_usr1 += sig; }

static ucontext_t main_ctx, co_ctx;
static char co_stack[64 * 1024];
static int co_log[8], co_n;
static void coroutine(void) {
    for (int i = 0; i < 3; i++) { co_log[co_n++] = 10 + i; swapcontext(&co_ctx, &main_ctx); }
    co_log[co_n++] = 99;
}

int main(void) {
    struct sigaction sa; memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = on_segv; sa.sa_flags = SA_SIGINFO | SA_NODEFER;
    sigaction(SIGSEGV, &sa, 0);
    if (sigsetjmp(jb, 1) == 0) { volatile int *p = (int *)0x10; *p = 1; puts("not reached"); }
    printf("segv_count=%d addr=%p\n", segv_count, (void *)fault_addr);

    /* guard page via mprotect */
    char *m = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(m + 4096, 4096, PROT_NONE);
    m[10] = 1;
    if (sigsetjmp(jb, 1) == 0) { m[4096 + 5] = 2; puts("not reached 2"); }
    printf("segv_count=%d off=%ld\n", segv_count, (long)((char *)fault_addr - m));

    signal(SIGUSR1, on_usr1);
    raise(SIGUSR1); kill(getpid(), SIGUSR1);
    printf("usr1 handled sum=%d\n", got_usr1);

    sigset_t set, old; sigemptyset(&set); sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, &old);
    raise(SIGUSR1);
    int before = got_usr1;
    sigprocmask(SIG_SETMASK, &old, 0);
    printf("blocked then delivered: %d -> %d\n", before, got_usr1);

    getcontext(&co_ctx);
    co_ctx.uc_stack.ss_sp = co_stack; co_ctx.uc_stack.ss_size = sizeof co_stack; co_ctx.uc_link = &main_ctx;
    makecontext(&co_ctx, coroutine, 0);
    for (int i = 0; i < 4; i++) swapcontext(&main_ctx, &co_ctx);
    for (int i = 0; i < co_n; i++) printf("co %d\n", co_log[i]);
    return 0;
}
