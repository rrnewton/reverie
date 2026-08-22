/* Cost of one intercepted rdtsc vs one intercepted clock_gettime under Hermit.
 * clock_gettime is the existing coordinator-backed path (RPC to tool_global +
 * shared-floor), so it upper-bounds what rdtsc would cost if it read the same
 * base. Usage: rdtsc_cost <rdtsc|clock|nothing> <iterations>
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <x86intrin.h>

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "rdtsc";
    long n = argc > 2 ? atol(argv[2]) : 10000;
    unsigned long long acc = 0;
    struct timespec ts;

    for (long i = 0; i < n; i++) {
        if (!strcmp(mode, "rdtsc"))
            acc += __rdtsc();
        else if (!strcmp(mode, "clock")) {
            clock_gettime(CLOCK_MONOTONIC, &ts);
            acc += (unsigned long long)ts.tv_nsec;
        } else
            acc += (unsigned long long)i;
    }
    printf("mode=%s n=%ld acc=%llu\n", mode, n, acc);
    return 0;
}
