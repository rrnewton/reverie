/* Executes exactly N conditional branches (one `jnz` per iteration). */
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    long n = (argc > 1) ? atol(argv[1]) : 100000000L;
    long c = n;
    __asm__ volatile ("1:\n\t dec %[c]\n\t jnz 1b\n\t" : [c] "+r"(c) :: "cc");
    printf("requested_conditional_branches=%ld\n", n);
    return 0;
}
