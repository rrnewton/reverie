/* How do hermit's clocks behave as the thread count grows?
 *
 * Every measurement in the clock root-cause work was single-threaded, which is
 * exactly where SUM and MAX cannot differ: a sum over one thread IS that
 * thread. This program varies the thread count so the two can be told apart.
 *
 * Each worker does an identical fixed amount of work, then thread 0 reports:
 *   - CLOCK_MONOTONIC (global virtual time, the SUM)
 *   - its own rdtsc    (its thread-local virtual time)
 *
 * Predictions if global time is a SUM over threads plus scheduler time:
 *   - monotonic should grow roughly with N (every thread's work counts)
 *   - one thread's rdtsc should stay roughly flat in N (its own work is fixed)
 * Under a MAX the monotonic figure would instead stay flat in N.
 *
 * Build: gcc -O2 -pthread -o threadclock threadclock.c
 * Usage: threadclock <threads> <work-iterations>
 */
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <x86intrin.h>

static unsigned long work_iterations = 3000000UL;

static double monotonic_seconds(void)
{
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}

/* volatile keeps the loop; the retired-branch count is what advances virtual
 * time, so the work must not be optimised away. */
static unsigned long burn(unsigned long iterations)
{
    volatile unsigned long sink = 0;
    for (unsigned long i = 0; i < iterations; i++)
        sink += i * 2654435761u;
    return sink;
}

static void *worker(void *arg)
{
    (void)arg;
    burn(work_iterations);
    return NULL;
}

int main(int argc, char **argv)
{
    int threads = argc > 1 ? atoi(argv[1]) : 1;
    if (argc > 2)
        work_iterations = strtoul(argv[2], NULL, 10);

    double mono_before = monotonic_seconds();
    unsigned long long tsc_before = __rdtsc();

    pthread_t *ids = calloc((size_t)threads, sizeof(pthread_t));
    /* Thread 0 is this thread: it does one worker's share itself, so every
     * arm does the same work per thread and the main thread's own rdtsc
     * measures one worker's worth regardless of N. */
    for (int i = 1; i < threads; i++)
        pthread_create(&ids[i], NULL, worker, NULL);
    burn(work_iterations);
    for (int i = 1; i < threads; i++)
        pthread_join(ids[i], NULL);

    unsigned long long tsc_after = __rdtsc();
    double mono_after = monotonic_seconds();

    printf("threads %d monotonic_delta %.9f tsc_delta %llu\n",
           threads, mono_after - mono_before, tsc_after - tsc_before);
    free(ids);
    return 0;
}
