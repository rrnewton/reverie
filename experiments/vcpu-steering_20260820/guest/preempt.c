/* Counts GUEST PREEMPTIONS of a fixed workload, one process per guest CPU.
 *
 * Two processes, pinned one per guest CPU, each spin a fixed number of
 * iterations. Each reads nonvoluntary_ctxt_switches from /proc/self/status
 * before and after; the delta is how many times the guest kernel took the CPU
 * away from a process that still wanted it. Voluntary switches are reported
 * alongside so a run that blocked rather than being preempted is visible
 * instead of being counted as preemption.
 *
 * The work is FIXED, not timed, so every arm does the same amount of guest
 * execution and the counts are comparable across timeslice values.
 *
 * This is NOT the detector named in the 07:35 predeclaration (sha256 e0d7c8fa),
 * which is not in any branch or worktree. Numbers from here are not claimed to
 * be comparable with the recorded 50 ms -> 83 arm.
 */
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

static void read_switches(unsigned long *invol, unsigned long *vol)
{
    FILE *f = fopen("/proc/self/status", "r");
    char line[256];
    *invol = *vol = 0;
    if (!f) return;
    while (fgets(line, sizeof line, f)) {
        if (!strncmp(line, "nonvoluntary_ctxt_switches:", 27))
            *invol = strtoul(line + 27, NULL, 10);
        else if (!strncmp(line, "voluntary_ctxt_switches:", 24))
            *vol = strtoul(line + 24, NULL, 10);
    }
    fclose(f);
}

int main(int argc, char **argv)
{
    unsigned long long k = (argc > 1) ? strtoull(argv[1], NULL, 10) : 100000000ULL;
    /* Placement. With one process per guest CPU there is no contention, so the guest
     * kernel has no reason to preempt either of them and the count is ~0 BY
     * CONSTRUCTION -- measured: 0 involuntary at a 2 ms timeslice. Preemption of a
     * guest process requires two runnable processes on ONE guest CPU, which is also
     * the case the steering work has been unable to reach. `same` is that case;
     * `split` is the control. */
    int same = (argc > 2 && !strcmp(argv[2], "same"));
    int ncpu = (int)sysconf(_SC_NPROCESSORS_ONLN);
    volatile unsigned long long *pad =
        mmap(NULL, 2 * 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (pad == MAP_FAILED) { perror("mmap"); return 1; }
    printf("PREEMPT-START ncpu=%d k=%llu placement=%s\n", ncpu, k, same ? "same" : "split");
    fflush(stdout);
    if (ncpu < 2) { printf("PREEMPT-ABORT need 2 guest cpus\n"); fflush(stdout); return 1; }

    for (int i = 0; i < 2; i++) {
        if (fork() == 0) {
            cpu_set_t set;
            CPU_ZERO(&set);
            CPU_SET(same ? 0 : i, &set);
            if (sched_setaffinity(0, sizeof(set), &set) != 0) {
                printf("PREEMPT-ABORT affinity cpu%d\n", i); fflush(stdout); _exit(1);
            }
            unsigned long iv0, vo0, iv1, vo1;
            read_switches(&iv0, &vo0);
            /* Own page per process: two counters in one page made the two guest
             * CPUs write the same page and cost about 4x, measured earlier in
             * this directory. It cancels out of a ratio but not out of a rate. */
            volatile unsigned long long *c = &pad[i * (4096 / sizeof(unsigned long long))];
            *c = 0;
            while (*c < k) (*c)++;
            read_switches(&iv1, &vo1);
            printf("PREEMPT cpu=%d spins=%llu involuntary=%lu voluntary=%lu oncpu=%d\n",
                   i, (unsigned long long)*c, iv1 - iv0, vo1 - vo0, sched_getcpu());
            fflush(stdout);
            _exit(0);
        }
    }
    int st;
    wait(&st);
    wait(&st);
    printf("PREEMPT-END\n");
    fflush(stdout);
    return 0;
}
