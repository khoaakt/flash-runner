// Call-heavy recursion (call/ret/push/pop/cmp/jcc) used to track JIT performance.
#include <stdio.h>
static int fib(int n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
int main(void) {
    printf("fib(30)=%d\n", fib(30));
    printf("fib(27)=%d\n", fib(27));
    return 0;
}
