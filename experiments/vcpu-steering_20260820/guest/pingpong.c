/* A workload that is long in guest-scheduler terms and short in guest execution.
 *
 * The spin detector runs guest code and never blocks, so hermit -- which grants a
 * scheduler turn when a host thread blocks or wakes -- had almost nothing to
 * decide: measured 0.81 turns/s on the two vCPU threads at the default timeslice.
 * Shrinking the timeslice raised that to 11.15/s but stopped the workload from
 * finishing, which is the wall this is trying to get around.
 *
 * Here the two guest processes hand a byte back and forth over a pair of pipes.
 * Each handoff puts one guest CPU to sleep and wakes the other, so the guest CPU
 * idles and resumes once per handoff. An idle guest CPU is what makes QEMU's vCPU
 * thread block, and a blocking host thread is what hermit has to schedule -- so
 * the number of decisions is set by the handoff count, not by how much guest code
 * runs.
 *
 * `split` puts one process on each guest CPU, so every handoff crosses guest CPUs.
 * `same` puts both on guest CPU0, where the guest kernel switches between them
 * without either guest CPU going idle -- the control that separates "the guest
 * scheduler ran" from "a guest CPU slept and woke".
 */
#define _GNU_SOURCE
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
    long n = (argc > 1) ? atol(argv[1]) : 2000;
    /* "same" and "split" both start with 's', so the first character is not enough:
     * the first run asked for split and got same. Compare the whole word. */
    int same = (argc > 2 && !strcmp(argv[2], "same"));
    int a2b[2], b2a[2];
    if (pipe(a2b) || pipe(b2a)) { perror("pipe"); return 1; }
    if ((int)sysconf(_SC_NPROCESSORS_ONLN) < 2) {
        printf("PP-ABORT need 2 guest cpus\n"); fflush(stdout); return 1;
    }
    printf("PP-START n=%ld placement=%s\n", n, same ? "same" : "split");
    fflush(stdout);

    for (int i = 0; i < 2; i++) {
        if (fork() == 0) {
            cpu_set_t set;
            CPU_ZERO(&set);
            CPU_SET(same ? 0 : i, &set);
            if (sched_setaffinity(0, sizeof(set), &set) != 0) {
                printf("PP-ABORT affinity cpu%d\n", i); fflush(stdout); _exit(1);
            }
            int rd = i ? a2b[0] : b2a[0];
            int wr = i ? b2a[1] : a2b[1];
            unsigned long iv0, vo0, iv1, vo1;
            char c = 'x';
            long done = 0;
            read_switches(&iv0, &vo0);
            /* Process 0 starts the exchange; after that each side blocks until the
             * other hands over, so exactly one of them is runnable at any moment. */
            if (i == 0 && write(wr, &c, 1) != 1) _exit(1);
            while (done < n) {
                if (read(rd, &c, 1) != 1) break;
                if (write(wr, &c, 1) != 1) break;
                done++;
            }
            read_switches(&iv1, &vo1);
            printf("PP side=%d handoffs=%ld involuntary=%lu voluntary=%lu oncpu=%d\n",
                   i, done, iv1 - iv0, vo1 - vo0, sched_getcpu());
            fflush(stdout);
            _exit(0);
        }
    }
    int st;
    wait(&st);
    /* One side finishes first and stops reading, so the other can block forever on
     * its last read. Close every descriptor here so that read returns 0 and it
     * exits, rather than the run hanging on a workload that has already finished. */
    for (int k = 0; k < 2; k++) { close(a2b[k]); close(b2a[k]); }
    wait(&st);
    printf("PP-END\n");
    fflush(stdout);
    return 0;
}
