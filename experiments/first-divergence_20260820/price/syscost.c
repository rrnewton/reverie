/* Upper-bound the cost of dereferencing ONE small output buffer at a syscall
 * boundary -- the per-call term a targeted check would pay.
 *
 * mode 0: getpid()        no output buffer; the INFO record has nothing to read
 * mode 1: clock_gettime() 16-byte timespec that Detcore's display ALREADY reads
 *                         out of guest memory and prints ("0x... -> { tv_sec.. }")
 *
 * The difference is the cost of reading and rendering a small output buffer,
 * which is an upper bound on reading and hashing one (hashing 16 bytes is
 * cheaper than formatting them).
 */
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
int main(int argc, char **argv) {
    int mode = atoi(argv[1]);
    long n = atol(argv[2]);
    struct timespec ts;
    volatile long sink = 0;
    for (long i = 0; i < n; i++) {
        if (mode) { clock_gettime(CLOCK_MONOTONIC, &ts); sink += ts.tv_nsec; }
        else      { sink += getpid(); }
    }
    printf("mode=%d n=%ld\n", mode, n);
    return 0;
}
