#define _GNU_SOURCE
#include <stdio.h>
#include <time.h>
int main(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    printf("%lld.%09ld\n", (long long)t.tv_sec, t.tv_nsec);
    return 0;
}
