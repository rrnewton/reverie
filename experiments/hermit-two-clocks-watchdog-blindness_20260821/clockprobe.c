/* Read every clock a program can reach, repeatedly, and report what each one
 * did between consecutive samples.
 *
 * The guest-kernel finding is that two clocks disagreed: the guest's
 * clocksource showed a 30-second step and sched_clock, which the watchdogs
 * compute from, did not. The same question applies to a program under hermit,
 * which is hermit's actual product surface: if the clocks hermit exposes can
 * disagree with each other, then any check built on one of them is blind to a
 * step in another.
 *
 * Clocks read here, and what each is:
 *   CLOCK_MONOTONIC        the usual "how long has this been running"
 *   CLOCK_MONOTONIC_RAW    the same, without NTP adjustment
 *   CLOCK_REALTIME         wall clock
 *   CLOCK_PROCESS_CPUTIME  CPU consumed by this process
 *   CLOCK_THREAD_CPUTIME   CPU consumed by this thread
 *   rdtsc                  the hardware cycle counter, read directly, NOT a
 *                          syscall -- the one a tracer must work to intercept
 *   getrusage utime+stime  CPU accounting by a different kernel path
 *
 * Build: gcc -O2 -o clockprobe clockprobe.c
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <x86intrin.h>

static double seconds(struct timespec t)
{
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

static double clock_seconds(clockid_t which)
{
    struct timespec t;
    if (clock_gettime(which, &t) != 0)
        return -1.0;
    return seconds(t);
}

static double rusage_seconds(void)
{
    struct rusage r;
    if (getrusage(RUSAGE_SELF, &r) != 0)
        return -1.0;
    return (double)r.ru_utime.tv_sec + (double)r.ru_utime.tv_usec / 1e6
         + (double)r.ru_stime.tv_sec + (double)r.ru_stime.tv_usec / 1e6;
}

/* Burn a fixed amount of work so each sample is separated by real progress
 * rather than by a sleep -- under a deterministic scheduler, instructions
 * retired is the thing that advances. */
static unsigned long burn(unsigned long iterations)
{
    volatile unsigned long sink = 0;
    for (unsigned long i = 0; i < iterations; i++)
        sink += i * 2654435761u;
    return sink;
}

int main(int argc, char **argv)
{
    int samples = argc > 1 ? atoi(argv[1]) : 8;
    unsigned long work = argc > 2 ? strtoul(argv[2], NULL, 10) : 20000000UL;

    printf("sample monotonic monotonic_raw realtime process_cpu thread_cpu "
           "tsc rusage_cpu\n");
    for (int i = 0; i < samples; i++) {
        unsigned long long cycles = __rdtsc();
        printf("%d %.9f %.9f %.9f %.9f %.9f %llu %.6f\n",
               i,
               clock_seconds(CLOCK_MONOTONIC),
               clock_seconds(CLOCK_MONOTONIC_RAW),
               clock_seconds(CLOCK_REALTIME),
               clock_seconds(CLOCK_PROCESS_CPUTIME_ID),
               clock_seconds(CLOCK_THREAD_CPUTIME_ID),
               cycles,
               rusage_seconds());
        fflush(stdout);
        burn(work);
    }
    return 0;
}
