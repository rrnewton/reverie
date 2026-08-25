/* Per iteration: 1 conditional branch (loop) + 1 call + 1 ret (unconditional).
   So conditional ~= N, all-retired-branches ~= 3N. */
#include <stdio.h>
#include <stdlib.h>
__attribute__((noinline)) void nop_fn(void) { __asm__ volatile("" ::: "memory"); }
int main(int argc, char **argv) {
    long n = (argc > 1) ? atol(argv[1]) : 100000000L;
    for (long i = 0; i < n; i++) nop_fn();
    printf("iterations=%ld\n", n);
    return 0;
}
