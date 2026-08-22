/* Which guest CPU advanced, measured by the guest against itself.
 *
 * No clock is read, on either side. Two processes, one pinned to each guest
 * CPU, spin on their own counter in shared memory. The first to reach K reads
 * the other's counter and prints both. `other/K` is the advancement ratio.
 *
 * Clock-free on purpose: guest timers under MTTCG without icount are the
 * subject of several retired claims, so a detector that depends on them would
 * inherit the argument it is meant to settle.
 */
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

/* Chosen so a single guest CPU reaches it in a few seconds of emulated
 * execution; overridable from the kernel cmdline via argv. */
#define DEFAULT_K 30000000ULL

static volatile unsigned long long *counter;

int main(int argc, char **argv)
{
    unsigned long long k = (argc > 1) ? strtoull(argv[1], NULL, 10) : DEFAULT_K;
    /* Orientation mode: load exactly ONE guest CPU. Used only to check that the
     * host thread the harness calls vCPU<n> is the one that runs guest CPU<n> --
     * the measurement arms never pass it. */
    int only = (argc > 2) ? atoi(argv[2]) : -1;
    int ncpu = (int)sysconf(_SC_NPROCESSORS_ONLN);

    /* One page per counter. Both counters sharing a page made the two guest CPUs
     * write the same page constantly, and an unbiased run was ~3x slower than a
     * run with one CPU starved (100M iterations in 1.07s against 0.30s). That is
     * a speed effect and cancels out of a RATIO, but it is removed here so the
     * ratio does not have to be defended against it. */
    counter = mmap(NULL, 4 * 4096, PROT_READ | PROT_WRITE,
                   MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (counter == MAP_FAILED) { perror("mmap"); return 1; }
#define SLOT(i) ((i) * (4096 / (int)sizeof(unsigned long long)))
    counter[SLOT(0)] = counter[SLOT(1)] = 0;
    /* Slots 2 and 3 are "this child reached its spin loop". Without them a
     * reported other=0 cannot be told apart from a child that never ran at
     * all, and the two have different meanings for the question being asked. */
    counter[SLOT(2)] = counter[SLOT(3)] = 0;
    /* Printed before the race so a truncated run still says what it attempted. */
    printf("ADVANCE-START ncpu=%d k=%llu\n", ncpu, k);
    fflush(stdout);
    if (ncpu < 2) { printf("ADVANCE-ABORT need 2 guest cpus\n"); fflush(stdout); return 1; }

    for (int i = 0; i < 2; i++) {
        if (only >= 0 && i != only)
            continue;
        pid_t pid = fork();
        if (pid < 0) { perror("fork"); return 1; }
        if (pid == 0) {
            cpu_set_t set;
            CPU_ZERO(&set);
            CPU_SET(i, &set);
            if (sched_setaffinity(0, sizeof(set), &set) != 0) {
                printf("ADVANCE-ABORT affinity cpu%d failed\n", i);
                fflush(stdout);
                _exit(1);
            }
            /* Confirm the pin took, rather than assuming it. */
            CPU_ZERO(&set);
            sched_getaffinity(0, sizeof(set), &set);
            if (!CPU_ISSET(i, &set) || CPU_COUNT(&set) != 1) {
                printf("ADVANCE-ABORT affinity cpu%d not exclusive\n", i);
                fflush(stdout);
                _exit(1);
            }
            counter[SLOT(2 + i)] = 1;
            while (counter[SLOT(i)] < k)
                counter[SLOT(i)]++;
            /* Read the loser's counter as close to the finish as possible. */
            unsigned long long other = counter[SLOT(1 - i)];
            unsigned long long other_started = counter[SLOT(2 + (1 - i))];
            printf("ADVANCE R%d me=%llu other=%llu other_started=%llu oncpu=%d\n",
                   i, counter[SLOT(i)], other, other_started, sched_getcpu());
            fflush(stdout);
            _exit(0);
        }
    }
    /* The parent must not compete for a guest CPU while the two children run,
     * so it blocks in wait() rather than polling the counters. */
    int status;
    wait(&status);
    if (only >= 0) {
        printf("ADVANCE-ONLY-DONE cpu=%d count=%llu\n", only, counter[SLOT(only)]);
        fflush(stdout);
        return 0;
    }
    printf("ADVANCE-FIRST-DONE\n");
    fflush(stdout);
    wait(&status);
    printf("ADVANCE-END c0=%llu c1=%llu\n", counter[SLOT(0)], counter[SLOT(1)]);
    fflush(stdout);
    return 0;
}
