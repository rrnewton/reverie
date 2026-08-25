/* Measure what it costs Hermit to hash guest memory at a syscall boundary.
 *
 * Two unknowns decide whether a syscall-boundary buffer hash is affordable:
 *   c_call  - fixed cost per syscall of doing the hash at all (for --detlog-heap
 *             that includes parsing /proc/<pid>/maps; a targeted check would NOT
 *             pay this, since the address and length come from the syscall args)
 *   c_byte  - marginal cost per byte read out of the guest and hashed
 *
 * Method: hold the syscall count fixed, vary the size of the hashed region, and
 * run under the EXISTING --detlog-heap flag. No Hermit change is needed to get
 * the two coefficients. The model is then validated by predicting the measured
 * cost of --detlog-stack (a different region size on a different workload)
 * before it is used to price anything new.
 *
 * The region must be the [heap] mapping, so glibc must not satisfy the request
 * with mmap: M_MMAP_THRESHOLD is raised and M_MMAP_MAX zeroed so the allocation
 * comes from brk.
 *
 * usage: hashcost2 <heap_kib> <syscalls> <extra_mappings>
 */
#include <malloc.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/mman.h>

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <heap_kib> <syscalls>\n", argv[0]);
        return 2;
    }
    size_t kib = (size_t)strtoul(argv[1], NULL, 10);
    long n = strtol(argv[2], NULL, 10);
    long extra = strtol(argv[3], NULL, 10);

    /* Create `extra` DISTINCT mappings. Alternating protections stop the kernel
     * from coalescing them, so /proc/<pid>/maps really does grow by `extra`
     * lines. This isolates the cost of PARSING the map -- the per-call term a
     * syscall-boundary check would not pay, since it gets (addr,len) from the
     * syscall arguments instead. */
    for (long i = 0; i < extra; i++) {
        int prot = (i % 2) ? PROT_READ : (PROT_READ | PROT_WRITE);
        if (mmap(NULL, 4096, prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) == MAP_FAILED) {
            perror("mmap");
            return 1;
        }
    }

    mallopt(M_MMAP_THRESHOLD, 1 << 30);
    mallopt(M_MMAP_MAX, 0);

    if (kib) {
        size_t bytes = kib * 1024;
        char *p = malloc(bytes);
        if (!p) {
            perror("malloc");
            return 1;
        }
        /* Touch every page so the region is resident and really gets read. */
        memset(p, 0xa5, bytes);
    }

    /* A syscall Detcore intercepts, with no output buffer of its own, so the
     * only per-call work attributable to the flag is the memory hashing. */
    struct timespec ts;
    for (long i = 0; i < n; i++) {
        clock_gettime(CLOCK_MONOTONIC, &ts);
    }
    printf("heap_kib=%zu syscalls=%ld extra=%ld\n", kib, n, extra);
    return 0;
}
